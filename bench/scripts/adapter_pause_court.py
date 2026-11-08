#!/usr/bin/env python3
"""Adapter save-pause court: planned vs fast write policy through the real
PyTorch DCP and Orbax/TensorStore adapters.

Runs inside the Docker python court:

  scripts/docker-court.sh python \
    "PYTHONPATH=/work/.docker-cache/py-orbax:/work/python \
     python3 bench/scripts/adapter_pause_court.py"

The fixture is a synthetic CPU-only model-like state dict (8 x 4 MiB f32 for
DCP, 8 leaves x 4 MiB f32 for Orbax). A 0.5B pair does not fit in the court
with torch/jax, the adapter staging buffer, and the CLI all resident, so the
adapter measurement is deliberately smaller than the CLI court's; the CLI court
reports the full Qwen pair. Every save is verified with the Rust CLI and
restored for exactness.

`WEFT_WRITE_POLICY` is the deployment knob the adapter service reads on every
`checkpoint write`; this court sets it to `planned` and then `fast`.
"""

from __future__ import annotations

import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import torch
import torch.distributed.checkpoint as dcp

from weftcodec.dcp import (
    DEFAULT_CLI,
    WeftStorageWriter,
    init_store,
    verify_checkpoint,
)

TENSOR_COUNT = 8
TENSOR_ELEMENTS = 1024 * 1024  # 4 MiB at f32
SHAPE = (8, TENSOR_ELEMENTS // 8)
REPS = int(os.environ.get("WEFT_ADAPTER_REPS", "3"))


def stamp():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def rss_kib():
    try:
        with open("/proc/self/status", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return None


def store_bytes(root):
    total = 0
    files = 0
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            try:
                total += os.path.getsize(os.path.join(dirpath, name))
                files += 1
            except OSError:
                pass
    return {"bytes": total, "files": files}


def cpu_ms():
    with open("/proc/self/stat", encoding="utf-8") as handle:
        raw = handle.read()
    fields = raw.rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) * 10.0


def state_dict():
    generator = torch.Generator().manual_seed(20261107)
    return {
        "layer%02d.weight" % index: torch.randn(SHAPE, generator=generator)
        for index in range(TENSOR_COUNT)
    }


def one_dcp(policy, rep, root, cli):
    from weftcodec.dcp import WeftStorageReader

    os.environ["WEFT_WRITE_POLICY"] = policy
    store = root / ("store-%s-%d" % (policy, rep))
    init_store(store, weftfs_cli=cli)
    state = state_dict()
    checkpoint_id = "pause-%s-%d" % (policy, rep)
    writer = WeftStorageWriter(
        checkpoint_id,
        store_root=store,
        topology=(8, 0),
        shard_strategy="zstd",
        staging_bytes=256 * 1024 * 1024,
        weftfs_cli=cli,
    )
    cpu0 = cpu_ms()
    started = time.perf_counter()
    dcp.save(state, storage_writer=writer, no_dist=True)
    pause_ms = (time.perf_counter() - started) * 1000.0
    cpu = cpu_ms() - cpu0
    stats = dict(writer.stats)
    verified = verify_checkpoint(store, checkpoint_id, full=True, weftfs_cli=cli)
    reader = WeftStorageReader(checkpoint_id, store_root=store, weftfs_cli=cli)
    target = {
        name: torch.empty_like(tensor) for name, tensor in state.items()
    }
    dcp.load(target, storage_reader=reader, no_dist=True)
    exact = all(torch.equal(target[name], state[name]) for name in state)
    return {
        "policy": policy,
        "rep": rep,
        "pause_ms": round(pause_ms, 3),
        "cpu_ms": round(cpu, 1),
        "checkpoint_write_ms": round(float(stats.get("checkpoint_write_ms", 0.0)), 3),
        "container_bytes": int(stats.get("container_bytes", 0)),
        "store_physical": store_bytes(store),
        "verified": bool(verified),
        "exact": exact,
        "peak_rss_kib": rss_kib(),
    }


def one_orbax(policy, rep, root, cli):
    import jax
    import jax.numpy as jnp
    import orbax.checkpoint as ocp
    from weftcodec import orbax as wo

    os.environ["WEFT_WRITE_POLICY"] = policy
    store = root / ("orbax-store-%s-%d" % (policy, rep))
    init_store(store, weftfs_cli=cli)
    key = jax.random.PRNGKey(20261107)
    keys = jax.random.split(key, TENSOR_COUNT)
    source = {
        "params": {
            "block%02d" % index: jax.device_put(
                jax.random.normal(keys[index], (1024, 1024), dtype=jnp.float32)
            )
            for index in range(TENSOR_COUNT)
        }
    }
    handler = wo.WeftOrbaxHandler(
        weftfs_cli=cli, staging_bytes=256 * 1024 * 1024, max_workers=1
    )
    directory = root / ("orbax-dir-%s-%d" % (policy, rep))
    checkpointer = ocp.Checkpointer(handler)
    cpu0 = cpu_ms()
    started = time.perf_counter()
    checkpointer.save(
        directory,
        args=wo.WeftSaveArgs(
            item=source,
            store_root=store,
            checkpoint_id="orbax-pause-%s-%d" % (policy, rep),
            write_topology=(8, 0),
            shard_strategy="zstd",
            init_store=True,
        ),
    )
    pause_ms = (time.perf_counter() - started) * 1000.0
    cpu = cpu_ms() - cpu0
    result = handler.last_result
    verified = wo.verify_checkpoint(store, result.storage_name, full=True)
    target = jax.tree_util.tree_map(
        lambda value: jax.ShapeDtypeStruct(value.shape, value.dtype), source
    )
    restored = handler.restore(
        directory, wo.WeftRestoreArgs(item=target)
    )
    exact = True
    for path, leaf in jax.tree_util.tree_flatten_with_path(source)[0]:
        got = restored
        for part in path:
            name = part.key if hasattr(part, "key") else part
            got = got[name]
        if not np.array_equal(np.asarray(leaf), np.asarray(got)):
            exact = False
    return {
        "policy": policy,
        "rep": rep,
        "pause_ms": round(pause_ms, 3),
        "cpu_ms": round(cpu, 1),
        "store_physical": store_bytes(store),
        "verified": bool(verified),
        "exact": exact,
        "peak_rss_kib": rss_kib(),
    }


def standard_orbax(root):
    """Standard Orbax OCDBT save of the same 8-leaf tree, for the deficit."""
    import jax
    import jax.numpy as jnp
    import orbax.checkpoint as ocp

    key = jax.random.PRNGKey(20261107)
    keys = jax.random.split(key, TENSOR_COUNT)
    source = {
        "params": {
            "block%02d" % index: jax.device_put(
                jax.random.normal(keys[index], (1024, 1024), dtype=jnp.float32)
            )
            for index in range(TENSOR_COUNT)
        }
    }
    samples = []
    physical = {}
    for rep in range(REPS):
        directory = root / ("standard-orbax-%d" % rep)
        checkpointer = ocp.PyTreeCheckpointer(
            use_ocdbt=True, use_compression=True
        )
        started = time.perf_counter()
        checkpointer.save(
            directory, args=ocp.args.PyTreeSave(item=source), force=True
        )
        samples.append((time.perf_counter() - started) * 1000.0)
        physical = store_bytes(directory)
        checkpointer.close()
    return {
        "reps": REPS,
        "pause_median_ms": round(statistics.median(samples), 3),
        "pause_samples_ms": [round(value, 3) for value in samples],
        "store_physical": physical,
    }


def summarise(rows):
    by_policy = {}
    for row in rows:
        by_policy.setdefault(row["policy"], []).append(row)
    out = {}
    for policy, group in by_policy.items():
        out[policy] = {
            "reps": len(group),
            "pause_median_ms": round(statistics.median(r["pause_ms"] for r in group), 3),
            "pause_samples_ms": [r["pause_ms"] for r in group],
            "cpu_median_ms": round(statistics.median(r["cpu_ms"] for r in group), 1),
            "checkpoint_write_median_ms": (
                round(
                    statistics.median(
                        r.get("checkpoint_write_ms", 0.0) for r in group
                    ),
                    3,
                )
                if "checkpoint_write_ms" in group[0]
                else None
            ),
            "all_verified": all(r["verified"] for r in group),
            "all_exact": all(r["exact"] for r in group),
            "peak_rss_kib_max": max((r["peak_rss_kib"] or 0) for r in group),
        }
    if "planned" in out and "fast" in out:
        planned = out["planned"]["pause_median_ms"]
        fast = out["fast"]["pause_median_ms"]
        out["fast_speedup"] = round(planned / fast, 2) if fast else None
    return out


def main():
    cli = os.environ.get("WEFTFS_CLI", DEFAULT_CLI)
    if not os.access(cli, os.X_OK):
        raise SystemExit("weftfs CLI not executable at %s" % cli)
    work = Path(tempfile.mkdtemp(prefix="weft-adapter-pause-"))
    receipt = Path(
        os.environ.get(
            "WEFT_ADAPTER_RECEIPT",
            "bench/receipts/adapter-pause-%s.json" % stamp(),
        )
    )
    receipt.parent.mkdir(parents=True, exist_ok=True)
    result = {
        "schema": "weft.adapter-pause.court.v1",
        "stamp": stamp(),
        "court": {
            "image": os.environ.get("WEFT_COURT_IMAGE", ""),
            "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", ""),
            "kernel": os.environ.get("WEFT_COURT_KERNEL", ""),
            "memory": os.environ.get("WEFT_COURT_MEMORY", ""),
            "cpus": os.environ.get("WEFT_COURT_CPUS", ""),
        },
        "fixture": {
            "dcp": {"tensors": TENSOR_COUNT, "shape": list(SHAPE), "dtype": "float32"},
            "orbax": {"leaves": TENSOR_COUNT, "shape": [1024, 1024], "dtype": "float32"},
            "note": "synthetic CPU fixture; the 0.5B pair is measured in the CLI court",
        },
        "reps": REPS,
    }
    dcp_rows = []
    for policy in ("planned", "fast"):
        for rep in range(REPS):
            row = one_dcp(policy, rep, work, cli)
            print("dcp", row, flush=True)
            dcp_rows.append(row)
    result["dcp"] = {"rows": dcp_rows, "summary": summarise(dcp_rows)}

    try:
        orbax_rows = []
        for policy in ("planned", "fast"):
            for rep in range(REPS):
                row = one_orbax(policy, rep, work, cli)
                print("orbax", row, flush=True)
                orbax_rows.append(row)
        standard = standard_orbax(work)
        print("orbax-standard", standard, flush=True)
        result["orbax"] = {
            "rows": orbax_rows,
            "summary": summarise(orbax_rows),
            "standard_ocdbt": standard,
        }
        fast_pause = result["orbax"]["summary"].get("fast", {}).get("pause_median_ms")
        if fast_pause and standard.get("pause_median_ms"):
            result["orbax"]["fast_vs_standard_ratio"] = round(
                fast_pause / standard["pause_median_ms"], 2
            )
        planned_pause = result["orbax"]["summary"].get("planned", {}).get("pause_median_ms")
        if planned_pause and standard.get("pause_median_ms"):
            result["orbax"]["planned_vs_standard_ratio"] = round(
                planned_pause / standard["pause_median_ms"], 2
            )
    except Exception as exc:  # honest skip: record the exact reason
        result["orbax"] = {
            "skipped": True,
            "error_class": type(exc).__name__,
            "error": str(exc)[:2000],
        }

    receipt.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print("receipt", receipt)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
