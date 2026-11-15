#!/usr/bin/env python3
"""Independent NumPy reference for one adapter merge (T048).

The Rust court writes a handful of merged tensors out as raw element bytes and
records their identity in the receipt. This script re-reads the *source*
containers (base model and `adapter_model.safetensors`), recomputes the merge
with NumPy, and compares the result element by element with the Rust bytes.

It exists to measure one claim: applying a LoRA mathematically is **not**
byte-equivalent to a different implementation unless the arithmetic order and
rounding are pinned. NumPy's `matmul` uses BLAS and its own accumulation order,
so the final bf16 values may differ in the last place even though both compute
the same real-valued expression. Those differences are reported, never hidden.

Standard library plus numpy. BF16 is decoded/rounded with integer bit
operations so the comparison does not depend on a bf16-capable NumPy build.
"""

from __future__ import annotations

import argparse
import json
import os
import struct

import numpy as np


def load_safetensors(path):
    """Minimal safetensors reader: name -> (dtype, shape, raw bytes)."""
    with open(path, "rb") as handle:
        blob = handle.read()
    if len(blob) < 8:
        raise SystemExit("%s: too short" % path)
    header_len = struct.unpack("<Q", blob[:8])[0]
    header = json.loads(blob[8 : 8 + header_len])
    data = memoryview(blob)[8 + header_len :]
    tensors = {}
    for name, info in header.items():
        if name == "__metadata__":
            continue
        begin, end = info["data_offsets"]
        tensors[name] = (info["dtype"], list(info["shape"]), bytes(data[begin:end]))
    return tensors


def bf16_to_f32(raw):
    bits = np.frombuffer(raw, dtype="<u2").astype(np.uint32)
    return (bits << 16).view(np.float32)


def f32_to_bf16_raw(values):
    flat = np.ascontiguousarray(values, dtype=np.float32).reshape(-1)
    bits = flat.view(np.uint32).copy()
    nan = np.isnan(flat)
    rounded = bits + (0x7FFF + ((bits >> 16) & 1))
    out = (rounded >> 16).astype(np.uint16)
    out[nan] = ((bits[nan] >> 16) | 0x40).astype(np.uint16)
    return out


def as_f32(dtype, raw):
    if dtype == "F32":
        return np.frombuffer(raw, dtype="<f4").astype(np.float32)
    if dtype == "BF16":
        return bf16_to_f32(raw)
    if dtype == "F16":
        return np.frombuffer(raw, dtype="<f2").astype(np.float32)
    raise SystemExit("unsupported reference dtype %s" % dtype)


def ulp_difference(a, b):
    """Distance between two f32 arrays in units of the local f32 spacing."""
    a = np.asarray(a, dtype=np.float32)
    b = np.asarray(b, dtype=np.float32)
    spacing = np.spacing(np.maximum(np.abs(a), np.abs(b)).astype(np.float32))
    spacing = np.where(spacing == 0, np.float32(2.0**-149), spacing)
    return np.abs(a - b) / spacing


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--receipt", required=True)
    parser.add_argument("--corpus", required=True)
    parser.add_argument("--base", required=True)
    parser.add_argument("--repo-root", default="/work")
    parser.add_argument("--dump", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    receipt = json.load(open(args.receipt))
    corpus = json.load(open(args.corpus))
    base = load_safetensors(args.base)
    views = receipt.get("merged_views") or []
    if not views:
        raise SystemExit("receipt has no merged views")
    view = views[0]
    repo = view.get("repo")
    if not repo:
        raise SystemExit("merged view does not name its source repo")
    entry = next(item for item in corpus["adapters"] if item["repo"] == repo)
    adapter_path = os.path.join(args.repo_root, entry["local_path"])
    adapter = load_safetensors(adapter_path)

    rank = float(entry["rank"])
    alpha = float(entry["lora_alpha"])
    scale = alpha / rank
    dora = bool(entry.get("use_dora"))
    rows = []
    for item in view.get("dumped_tensors") or []:
        tensor = item["tensor"]
        module = tensor[: -len(".weight")] if tensor.endswith(".weight") else tensor
        prefix = "base_model.model." + module
        a_name = prefix + ".lora_A.weight"
        b_name = prefix + ".lora_B.weight"
        if a_name not in adapter or b_name not in adapter:
            raise SystemExit("%s: adapter has no matrices for %s" % (repo, tensor))
        _, base_shape, base_raw = base[tensor]
        a_dtype, a_shape, a_raw = adapter[a_name]
        b_dtype, b_shape, b_raw = adapter[b_name]
        out_dim, in_dim = base_shape
        a = as_f32(a_dtype, a_raw).reshape(a_shape).astype(np.float32)
        b = as_f32(b_dtype, b_raw).reshape(b_shape).astype(np.float32)
        w = as_f32("BF16", base_raw).reshape(base_shape)

        rust = np.frombuffer(open("%s/%s/%s" % (args.dump, view["adapter"], item["file"]), "rb").read(), dtype="<u2")
        rust_f32 = bf16_to_f32(rust.tobytes()).reshape(base_shape)

        # Variant 1: float32 matmul, float32 accumulation (NumPy/BLAS order).
        delta32 = (b @ a).astype(np.float32)
        merged32 = (w + np.float32(scale) * delta32).astype(np.float32)
        if dora:
            magnitude_name = prefix + ".lora_magnitude_vector"
            _, _, m_raw = adapter[magnitude_name]
            magnitude = as_f32("F32", m_raw).astype(np.float32)
            norm = np.linalg.norm(merged32, axis=1, keepdims=True).astype(np.float32)
            merged32 = (merged32 / norm * magnitude.reshape(-1, 1)).astype(np.float32)
        bf16_32 = f32_to_bf16_raw(merged32).reshape(base_shape)

        # Variant 2: float64 accumulation, then a single narrowing to bf16.
        delta64 = b.astype(np.float64) @ a.astype(np.float64)
        merged64 = w.astype(np.float64) + np.float64(scale) * delta64
        if dora:
            magnitude_name = prefix + ".lora_magnitude_vector"
            _, _, m_raw = adapter[magnitude_name]
            magnitude = as_f32("F32", m_raw).astype(np.float64)
            norm = np.linalg.norm(merged64, axis=1, keepdims=True)
            merged64 = merged64 / norm * magnitude.reshape(-1, 1)
        bf16_64 = f32_to_bf16_raw(merged64.astype(np.float32)).reshape(base_shape)

        rust_bits = np.frombuffer(open("%s/%s/%s" % (args.dump, view["adapter"], item["file"]), "rb").read(), dtype="<u2").reshape(base_shape)
        mismatch32 = int(np.count_nonzero(bf16_32 != rust_bits))
        mismatch64 = int(np.count_nonzero(bf16_64 != rust_bits))
        base_bits = np.frombuffer(base_raw, dtype="<u2").reshape(base_shape)
        base_mismatch = int(np.count_nonzero(base_bits != rust_bits))
        ulps = ulp_difference(merged32.reshape(-1), rust_f32.reshape(-1))
        max_abs = float(np.max(np.abs(merged32.reshape(-1) - rust_f32.reshape(-1))))
        rows.append(
            {
                "tensor": tensor,
                "shape": base_shape,
                "rank": int(rank),
                "scale": scale,
                "dora": dora,
                "elements": int(rust_bits.size),
                "num_dtype": a_dtype.lower(),
                "rust_matches_f32_matmul": mismatch32 == 0,
                "f32_matmul_mismatches": mismatch32,
                "rust_matches_f64_accumulation": mismatch64 == 0,
                "f64_accumulation_mismatches": mismatch64,
                "max_f32_ulp_vs_f32_matmul": float(np.max(ulps)),
                "max_abs_difference_vs_f32_matmul": max_abs,
                "elements_differing_from_base": base_mismatch,
                "rust_equals_base": base_mismatch == 0,
            }
        )

    report = {
        "schema": "weft.adapter-reference.v1",
        "reference": "NumPy float32 matmul (BLAS order) and float64 accumulation, both narrowed to bf16 with round-half-to-even",
        "adapter_repo": repo,
        "adapter_revision": entry["revision"],
        "adapter_file_sha256": entry["files"][0]["sha256"],
        "tensors": rows,
        "all_match_f32_matmul": all(row["rust_matches_f32_matmul"] for row in rows),
        "all_match_f64_accumulation": all(row["rust_matches_f64_accumulation"] for row in rows),
        "none_equal_base": all(not row["rust_equals_base"] for row in rows),
    }
    with open(args.out, "w") as handle:
        json.dump(report, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print("adapter_reference_merge: wrote", args.out)


if __name__ == "__main__":
    main()
