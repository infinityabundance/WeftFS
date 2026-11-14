#!/usr/bin/env python3
"""T059 court: materialization receipts on real DCP and Orbax restores.

Both adapters restore real tensors, ask the store for a
``weft.materialization.receipt.v1`` derived by re-reading the checkpoint, bind
their restored entries into ``weft.adapter.receipt.v1``, and then a **separate
process** (``weftfs receipt verify``) re-derives everything and checks the
binding. The court also proves the verifier refuses a canonical rewrite of a
destination hash and of a source object id.

    scripts/docker-court.sh python \
      "PYTHONPATH=/work/python python3 python/courts/adapters_receipts.py run \
         --receipt bench/receipts/adapters-receipts-<stamp>.json"

CPU only, single host. No GPU and no multi-host claim.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "weft.adapters-receipts.v1"
DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def run_json(cli, store, args):
    proc = subprocess.run(
        [cli, "--store", str(store), "--json", *[str(a) for a in args]],
        capture_output=True,
        text=True,
        check=False,
    )
    payload = None
    if proc.stdout.strip():
        try:
            payload = json.loads(proc.stdout)
        except ValueError:
            payload = None
    return proc.returncode, payload, proc.stderr


def verify_receipts(cli, store, receipt, adapter):
    code, payload, stderr = run_json(
        cli,
        store,
        ["receipt", "verify", "--receipt", str(receipt), "--adapter", str(adapter)],
    )
    return code == 0 and bool(payload and payload.get("ok")), payload, stderr


def dcp_cell(cli, work, stamp):
    from torch.distributed.checkpoint import load as dcp_load
    from torch.distributed.checkpoint import save as dcp_save
    import torch

    from weftcodec import wcb
    from weftcodec.dcp import WeftStorageReader, WeftStorageWriter, init_store
    from weftcodec.materialization import receipt_root

    class Model(torch.nn.Module):
        def __init__(self):
            super().__init__()
            generator = torch.Generator().manual_seed(20261105)
            self.embed = torch.nn.Parameter(
                torch.randn(32, 16, generator=generator, dtype=torch.float32)
            )
            self.bias = torch.nn.Parameter(
                torch.randn(32, generator=generator, dtype=torch.float32)
            )
            self.block = torch.nn.Module()
            self.block.weight = torch.nn.Parameter(
                torch.randn(16, 8, generator=generator, dtype=torch.float32)
            )
            self.block.bias = torch.nn.Parameter(
                torch.randn(16, generator=generator, dtype=torch.float32)
            )

    store = work / "dcp-store"
    init_store(store, weftfs_cli=cli)
    source = Model()
    state = {name: value.detach().clone() for name, value in source.state_dict().items()}
    save_started = time.perf_counter()
    dcp_save(
        source.state_dict(),
        storage_writer=WeftStorageWriter(
            "dcp-receipts", store_root=store, topology=4, shard_strategy="zstd"
        ),
        no_dist=True,
    )
    save_ms = (time.perf_counter() - save_started) * 1000.0

    receipt = work / "dcp-materialization.wcb"
    target = Model()
    reader = WeftStorageReader(
        "dcp-receipts", store_root=store, materialization_receipt=receipt
    )
    load_started = time.perf_counter()
    dcp_load(target.state_dict(), storage_reader=reader, no_dist=True)
    load_ms = (time.perf_counter() - load_started) * 1000.0

    exact_names = [
        name
        for name in state
        if not torch.equal(state[name], target.state_dict()[name])
    ]
    exact = not exact_names
    report = reader.stats["materialization"]
    adapter = Path(report["adapter_receipt"])
    ok, verified, stderr = verify_receipts(cli, store, receipt, adapter)
    blob = receipt.read_bytes()
    value = wcb.decode(blob)

    # Canonical rewrite: one destination SHA-256 changes, the set root (which
    # covers content roots, not SHA-256) stays consistent, so only a verifier
    # that re-derives the bytes can catch it.
    tampered = wcb.decode(adapter.read_bytes())
    tampered["entries"] = [dict(entry) for entry in tampered["entries"]]
    digest = bytearray(tampered["entries"][0]["sha256"])
    digest[0] ^= 0xFF
    tampered["entries"][0]["sha256"] = bytes(digest)
    tampered_path = work / "dcp-tampered.adapter.wcb"
    tampered_path.write_bytes(wcb.encode(tampered))
    tamper_ok, _, _ = verify_receipts(cli, store, receipt, tampered_path)

    # A rewritten source object id is also refused.
    rewritten = wcb.decode(blob)
    rows = rewritten["tensors"]
    rows[0]["source_objects"][0]["object_id"] = b"\x00" * 32
    rewritten_path = work / "dcp-rewritten.wcb"
    rewritten_path.write_bytes(wcb.encode(rewritten))
    object_ok, _, _ = verify_receipts(cli, store, rewritten_path, adapter)

    return {
        "adapter": "dcp",
        "torch": torch.__version__,
        "checkpoint": "dcp-receipts",
        "tensors": len(state),
        "save_ms": round(save_ms, 3),
        "load_ms": round(load_ms, 3),
        "exact": bool(exact),
        "exact_mismatches": exact_names,
        "entries_bound": report["entries"],
        "materialization_root": report["materialization_root"],
        "receipt_root_recomputed": receipt_root(blob).hex(),
        "receipt_root_matches_store": receipt_root(blob).hex()
        == report["materialization_root"],
        "receipt_bytes": len(blob),
        "adapter_receipt_bytes": adapter.stat().st_size,
        "source_objects": sum(len(row["source_objects"]) for row in value["tensors"]),
        "independent_verifier_ok": ok,
        "verifier_report": verified,
        "canonical_tamper_refused": not tamper_ok,
        "source_object_rewrite_refused": not object_ok,
        "stderr": stderr[-400:],
    }


def orbax_cell(cli, work, stamp):
    try:
        import jax  # noqa: F401
        import jax.numpy as jnp
        import numpy as np
        import orbax.checkpoint as ocp  # noqa: F401
    except Exception as exc:  # noqa: BLE001
        return {
            "adapter": "orbax",
            "unavailable": True,
            "reason": "%s: %s" % (type(exc).__name__, exc),
        }

    from weftcodec import wcb
    from weftcodec import orbax as wo
    from weftcodec.dcp import init_store
    from weftcodec.materialization import receipt_root

    store = work / "orbax-store"
    init_store(store, weftfs_cli=cli)
    handler = wo.WeftOrbaxHandler(
        weftfs_cli=cli,
        staging_bytes=8 << 20,
        materialization_receipt=work / "orbax-materialization.wcb",
    )
    try:
        tree = {
            "params": {
                "f32": jnp.arange(32 * 8, dtype=jnp.float32).reshape(32, 8),
                "bf16": jnp.arange(16 * 4, dtype=jnp.bfloat16).reshape(16, 4),
                "i32": jnp.arange(16, dtype=jnp.int32),
            },
            "step": jnp.arange(8, dtype=jnp.int32),
        }
        directory = work / "orbax-ckpt"
        save_started = time.perf_counter()
        handler.save(
            directory,
            args=wo.WeftSaveArgs(
                item=tree,
                store_root=store,
                checkpoint_id="orbax-receipts",
                init_store=True,
            ),
        )
        save_ms = (time.perf_counter() - save_started) * 1000.0

        target = jax.tree_util.tree_map(
            lambda value: jax.ShapeDtypeStruct(value.shape, value.dtype), tree
        )
        load_started = time.perf_counter()
        restored = handler.restore(directory, wo.WeftRestoreArgs(item=target))
        load_ms = (time.perf_counter() - load_started) * 1000.0
        flat_source = wo.flatten_pytree(tree)
        flat_restored = wo.flatten_pytree(restored)
        mismatched_paths = []
        for (path, a), (path2, b) in zip(flat_source, flat_restored):
            if path != path2 or np.asarray(jax.device_get(a)).tobytes() != np.asarray(
                jax.device_get(b)
            ).tobytes():
                mismatched_paths.append(path)
        exact = (
            len(flat_source) == len(flat_restored) and not mismatched_paths
        )

        report = handler.stats["materialization"]
        receipt = Path(report["materialization_receipt"])
        adapter = Path(report["adapter_receipt"])
        ok, verified, stderr = verify_receipts(cli, store, receipt, adapter)
        blob = receipt.read_bytes()
        value = wcb.decode(blob)

        tampered = wcb.decode(adapter.read_bytes())
        tampered["entries"] = [dict(entry) for entry in tampered["entries"]]
        digest = bytearray(tampered["entries"][0]["sha256"])
        digest[0] ^= 0xFF
        tampered["entries"][0]["sha256"] = bytes(digest)
        tampered_path = work / "orbax-tampered.adapter.wcb"
        tampered_path.write_bytes(wcb.encode(tampered))
        tamper_ok, _, _ = verify_receipts(cli, store, receipt, tampered_path)

        return {
            "adapter": "orbax",
            "jax": jax.__version__,
            "checkpoint": "orbax-receipts",
            "leaves": len(flat_source),
            "save_ms": round(save_ms, 3),
            "load_ms": round(load_ms, 3),
            "exact": bool(exact),
            "exact_mismatches": mismatched_paths,
            "entries_bound": report["entries"],
            "materialization_root": report["materialization_root"],
            "receipt_root_recomputed": receipt_root(blob).hex(),
            "receipt_root_matches_store": receipt_root(blob).hex()
            == report["materialization_root"],
            "receipt_bytes": len(blob),
            "adapter_receipt_bytes": adapter.stat().st_size,
            "source_objects": sum(len(row["source_objects"]) for row in value["tensors"]),
            "independent_verifier_ok": ok,
            "verifier_report": verified,
            "canonical_tamper_refused": not tamper_ok,
            "stderr": stderr[-400:],
        }
    finally:
        handler.close()


def run(args):
    cli = args.cli
    work = Path(args.work_dir)
    work.mkdir(parents=True, exist_ok=True)
    checks = {}
    cells = []
    started = time.perf_counter()

    dcp = dcp_cell(cli, work, args.stamp)
    cells.append(dcp)
    checks["dcp_exact"] = bool(dcp["exact"])
    checks["dcp_receipt_written"] = Path(
        work / "dcp-materialization.wcb"
    ).is_file()
    checks["dcp_root_recomputed_matches"] = dcp["receipt_root_matches_store"]
    checks["dcp_independent_verifier_ok"] = dcp["independent_verifier_ok"]
    checks["dcp_canonical_tamper_refused"] = dcp["canonical_tamper_refused"]
    checks["dcp_source_object_rewrite_refused"] = dcp[
        "source_object_rewrite_refused"
    ]

    orbax = orbax_cell(cli, work, args.stamp)
    cells.append(orbax)
    if orbax.get("unavailable"):
        checks["orbax_available"] = False
    else:
        checks["orbax_exact"] = bool(orbax["exact"])
        checks["orbax_receipt_written"] = True
        checks["orbax_root_recomputed_matches"] = orbax[
            "receipt_root_matches_store"
        ]
        checks["orbax_independent_verifier_ok"] = orbax["independent_verifier_ok"]
        checks["orbax_canonical_tamper_refused"] = orbax[
            "canonical_tamper_refused"
        ]

    receipt = {
        "schema": SCHEMA,
        "stamp": args.stamp,
        "generated_at": utc_now(),
        "court": {
            "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
            "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
            "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
            "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
            "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
            "cpu_only": True,
            "single_host": True,
            "gloo_used": False,
            "note": "CPU only, single host; no GPU, multi-host, or fleet claim",
        },
        "revision": {
            "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        },
        "method": {
            "receipt": "weft.materialization.receipt.v1 (store-derived)",
            "adapter_binding": "weft.adapter.receipt.v1",
            "verifier": "weftfs receipt verify in a separate process",
            "tamper": "canonical rewrite of one destination sha256 and of one source object id",
        },
        "cells": cells,
        "checks": checks,
        "wall_ms": round((time.perf_counter() - started) * 1000.0, 3),
        "losses": [
            "The receipt is a second full read of every stored shard; it is not free.",
            "DCP bounding is a SHA-256 of each restored tensor when the receipt is enabled.",
            "A tampered receipt whose set root also changes is refused at decode, before comparison.",
        ],
        "result": "pass" if all(checks.values()) else "fail",
    }
    Path(args.receipt).parent.mkdir(parents=True, exist_ok=True)
    Path(args.receipt).write_text(json.dumps(receipt, indent=2))
    print(json.dumps({"result": receipt["result"], "checks": checks}, indent=2))
    return 0 if receipt["result"] == "pass" else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run_parser = sub.add_parser("run", help="run the court")
    run_parser.add_argument("--receipt", required=True)
    run_parser.add_argument("--stamp", default=datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ"))
    run_parser.add_argument("--work-dir", default="/tmp/weft-adapters-receipts")
    run_parser.add_argument("--cli", default=DEFAULT_CLI)
    args = parser.parse_args(argv)
    if args.command == "run":
        return run(args)
    return 2


if __name__ == "__main__":
    sys.exit(main())
