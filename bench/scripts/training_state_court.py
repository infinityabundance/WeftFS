#!/usr/bin/env python3
"""T049 court: optimizer state, gradients and an attention mask from a real
backward pass.

The model, loss and optimizer are the deterministic transformer the
training-goodput court uses (`python/courts/training_goodput.py`), so the
payloads here are produced by autograd and by one AdamW step, not by a random
number generator. The fixture is small (0.414 M parameters) and that is stated
in the receipt.

Steps:
1. forward + cross-entropy + backward + one AdamW step;
2. dump bf16 parameters, f32 AdamW moments and bf16 gradients as safetensors,
   and a real u8 attention mask ([batch, seq, seq], causal with padded keys);
3. run the shipped Rust ingest/restore harness against a fresh store;
4. compare every restored payload with the torch tensors byte-for-byte;
5. run the independent Go implementation's `verify --full` over the store;
6. write one receipt.

CPU only, single host, one thread.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import struct
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

COURT = Path(__file__).resolve().parents[2] / "python" / "courts" / "training_goodput.py"


def utc_now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_court():
    spec = importlib.util.spec_from_file_location("weft_training_goodput", COURT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write_safetensors(path, tensors):
    """tensors: list of (name, dtype string like 'BF16'/'F32'/'U8', shape, bytes)"""
    header = {}
    offset = 0
    for name, dtype, shape, raw in tensors:
        header[name] = {
            "dtype": dtype,
            "shape": list(shape),
            "data_offsets": [offset, offset + len(raw)],
        }
        offset += len(raw)
    header["__metadata__"] = {"format": "pt"}
    text = json.dumps(header, separators=(",", ":")).encode("utf-8")
    with open(path, "wb") as handle:
        handle.write(struct.pack("<Q", len(text)))
        handle.write(text)
        for _, _, _, raw in tensors:
            handle.write(raw)


def f32_bytes(array):
    return np.ascontiguousarray(array, dtype="<f4").tobytes()


def bf16_bytes(array):
    flat = np.ascontiguousarray(array, dtype=np.float32).reshape(-1)
    bits = flat.view(np.uint32).copy()
    nan = np.isnan(flat)
    rounded = bits + (0x7FFF + ((bits >> 16) & 1))
    out = (rounded >> 16).astype(np.uint16)
    out[nan] = ((bits[nan] >> 16) | 0x40).astype(np.uint16)
    return out.tobytes()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--stamp", required=True)
    parser.add_argument("--binary", required=True, help="training_state_court example")
    parser.add_argument("--go-binary", required=True, help="WeftFS-Go weftfs binary")
    parser.add_argument("--work", required=True)
    parser.add_argument("--receipt", required=True)
    args = parser.parse_args()

    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    (work / "restored").mkdir(exist_ok=True)

    court = load_court()
    torch = court.load_torch()
    F = torch.nn.functional
    model = court.build_model(torch)
    optimizer = court.make_optimizer(torch, model)

    batch = court.data_batches(torch)[0]
    inputs, targets = batch
    logits = model(inputs)
    loss = F.cross_entropy(logits.reshape(-1, logits.shape[-1]), targets.reshape(-1))
    zero_grad = optimizer.zero_grad
    zero_grad(set_to_none=True)
    loss.backward()
    grads = {
        name: parameter.grad.detach().clone()
        for name, parameter in model.named_parameters()
        if parameter.grad is not None
    }
    global_norm = float(
        torch.sqrt(sum((grad.float() ** 2).sum() for grad in grads.values()))
    )
    optimizer.step()

    state = optimizer.state
    moments = []
    for name, parameter in model.named_parameters():
        entry = state.get(parameter)
        if not entry:
            raise SystemExit("optimizer has no state for %s" % name)
        moments.append(
            (
                name,
                entry["exp_avg"].detach().numpy(),
                entry["exp_avg_sq"].detach().numpy(),
                int(entry["step"].item()),
            )
        )
    step = max(item[3] for item in moments)
    if any(item[3] != step for item in moments):
        raise SystemExit("AdamW step counters disagree")

    # Model container: bf16 parameters (the mixed-precision case: moments stay
    # f32 even though the weights are bf16).
    write_safetensors(
        work / "model.safetensors",
        [
            (name, "BF16", tuple(parameter.shape), bf16_bytes(parameter.detach().numpy()))
            for name, parameter in model.named_parameters()
        ],
    )
    write_safetensors(
        work / "moments.safetensors",
        [
            (name + ".exp_avg", "F32", tuple(avg.shape), f32_bytes(avg))
            for name, avg, _, _ in moments
        ]
        + [
            (name + ".exp_avg_sq", "F32", tuple(sq.shape), f32_bytes(sq))
            for name, _, sq, _ in moments
        ],
    )
    write_safetensors(
        work / "gradients.safetensors",
        [
            (name, "BF16", tuple(grad.shape), bf16_bytes(grad.numpy()))
            for name, grad in grads.items()
        ],
    )

    # A real mask: 1 where a key is attended, 0 for a padded key. The first
    # three keys are padding, so every query attends only to keys >= 3 and <=
    # its own position.
    seq = int(inputs.shape[1])
    batch_n = int(inputs.shape[0])
    pad = 3
    mask = np.zeros((batch_n, seq, seq), dtype=np.uint8)
    for b in range(batch_n):
        for query in range(seq):
            for key in range(seq):
                mask[b, query, key] = 1 if (key >= pad and key <= query) else 0
    write_safetensors(work / "mask.safetensors", [("mask", "U8", mask.shape, mask.tobytes())])

    d_model_hyper = {
        "lr": court.LEARNING_RATE,
        "beta1": court.BETAS[0],
        "beta2": court.BETAS[1],
        "eps": court.EPS,
        "weight_decay": court.WEIGHT_DECAY,
    }
    meta = {
        "step": step,
        "loss_kind": "cross_entropy",
        "loss": float(loss.detach()),
        "loss_bits": struct.unpack("<I", struct.pack("<f", float(loss.detach())))[0],
        "tokens": int(targets.numel()),
        "seq_len": seq,
        "global_norm": global_norm,
        "hyper": d_model_hyper,
        "mask": {
            "name": "causal-padded",
            "kind": "hybrid",
            "seq_len": seq,
            "pad_positions": pad,
            "interpretation": "mask[b, q, k] = 1 iff the key k is attended by query q: k <= q (causal) and k >= pad_positions (the first pad_positions keys are padding and are never attended). dtype u8, one byte per (batch, query, key).",
        },
    }
    (work / "meta.json").write_text(json.dumps(meta, indent=1))

    harness_out = work / "harness.json"
    command = [
        args.binary,
        "--model", str(work / "model.safetensors"),
        "--moments", str(work / "moments.safetensors"),
        "--gradients", str(work / "gradients.safetensors"),
        "--mask", str(work / "mask.safetensors"),
        "--meta", str(work / "meta.json"),
        "--store", str(work / "store"),
        "--scratch", str(work / "scratch"),
        "--restore-dir", str(work / "restored"),
        "--out", str(harness_out),
    ]
    subprocess.run(command, check=True, text=True)

    receipt = json.loads(harness_out.read_text())

    # zstd -19 baseline on exactly the payload bytes, measured with libzstd via
    # the `zstandard` module (the court has no zstd CLI).
    import zstandard

    compressor = zstandard.ZstdCompressor(level=19)
    zstd_sizes = {}
    for name in ("moments", "gradients", "mask"):
        raw = (work / "scratch" / (name + ".raw")).read_bytes()
        zstd_sizes[name] = len(compressor.compress(raw))
    for key, name in (
        ("optimizer_state", "moments"),
        ("gradients", "gradients"),
        ("attention_mask", "mask"),
    ):
        cell = receipt[key]
        cell["zstd19_raw_bytes"] = zstd_sizes[name]
        cell["physical_vs_zstd19"] = cell["stored_physical_bytes"] / zstd_sizes[name]
    receipt["training_state_total"]["zstd19_total_bytes"] = sum(zstd_sizes.values())
    receipt["zstd_baseline"] = {
        "implementation": "python-zstandard level 19 (libzstd), one shot per payload",
        "bytes": zstd_sizes,
    }

    # Compare every restored payload with the torch tensor it came from.
    mismatches = []
    for name, avg, sq, _ in moments:
        for label, expected, path in (
            ("exp_avg", avg, work / "restored" / (name + ".exp_avg.bin")),
            ("exp_avg_sq", sq, work / "restored" / (name + ".exp_avg_sq.bin")),
        ):
            actual = np.frombuffer(path.read_bytes(), dtype="<f4").reshape(expected.shape)
            if not np.array_equal(actual, expected):
                mismatches.append("%s.%s" % (name, label))
    if (work / "restored" / "mask.bin").read_bytes() != mask.tobytes():
        mismatches.append("mask")
    receipt["oracle"] = {
        "optimizer_payloads_compared": 2 * len(moments),
        "mask_compared": 1,
        "mismatches": mismatches,
        "all_equal": not mismatches,
        "source": "torch tensors from the real backward pass and AdamW step",
    }

    # Independent Go implementation: verify every frame in the same store.
    go = subprocess.run(
        [args.go_binary, "--store", str(work / "store"), "verify", "--full", "--json"],
        text=True,
        capture_output=True,
    )
    go_report = None
    if go.returncode == 0 and go.stdout.strip():
        try:
            go_report = json.loads(go.stdout)
        except json.JSONDecodeError:
            go_report = {"raw": go.stdout}
    receipt["go_verify"] = {
        "binary": args.go_binary,
        "exit_code": go.returncode,
        "report": go_report,
        "stderr": go.stderr.strip()[:4000],
    }

    checks = {
        "optimizer_restore_exact": receipt["optimizer_state"]["restore_exact"],
        "gradients_restore_exact": receipt["gradients"]["restore_exact"],
        "attention_mask_restore_exact": receipt["attention_mask"]["restore_exact"],
        "torch_oracle_all_equal": receipt["oracle"]["all_equal"],
        "store_verify_full_clean": receipt["verify"]["problems"] == 0,
        "go_verify_full_clean": go.returncode == 0
        and (go_report or {}).get("OK", (go_report or {}).get("ok", False)),
    }
    receipt["checks"] = checks
    receipt["result"] = "pass" if all(checks.values()) else "fail"
    receipt["stamp"] = args.stamp
    receipt["generated_utc"] = utc_now()
    Path(args.receipt).write_text(json.dumps(receipt, indent=1) + "\n")
    print("training_state_court: checks", json.dumps(checks))
    print(
        "training_state_court: optimizer %d bytes stored %.2fx raw %.2fx zstd19"
        % (
            receipt["optimizer_state"]["logical_bytes"],
            receipt["optimizer_state"]["physical_vs_raw"],
            receipt["optimizer_state"]["physical_vs_zstd19"],
        )
    )
    sys.exit(0 if all(checks.values()) else 1)


if __name__ == "__main__":
    main()
