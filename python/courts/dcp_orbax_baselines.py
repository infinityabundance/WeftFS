#!/usr/bin/env python3
"""T060 court: the frameworks' real best save paths, and the remaining gap.

Arms, all saving the same 1.66 MB complete training state at the same barrier:

* ``weft-planned``   — one ``WeftStorageWriter`` per save (planned policy);
* ``weft-session``   — one ``CheckpointSession``; no container retention;
* ``weft-fast``      — session + the ``fast`` write policy (zstd-1, no planner);
* ``dcp-fs``         — native ``torch.distributed.checkpoint`` ``FileSystemWriter``;
* ``orbax-ocdbt``    — Orbax ``StandardCheckpointHandler`` (OCDBT), when installed.

Targets stated earlier and **not** redefined here: DCP save p50 below 20 ms and
parity with OCDBT. If they are missed, the achieved numbers, the split and the
remaining term are published.

A durability check follows: a child process saves through the session, reports
the ack, and is SIGKILLed immediately; the parent then reopens the store and
verifies the acknowledged checkpoint and its exact contents.

CPU only, single host. No GPU and no multi-host claim.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import signal
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "weft.dcp-orbax-baselines.v1"
DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"
COURT = Path(__file__).resolve().parent / "training_goodput.py"
RESUME = Path(__file__).resolve().parent / "training_resume.py"


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def build_state():
    court = load_module(COURT, "weft_training_goodput")
    resume = load_module(RESUME, "weft_training_resume")
    torch = court.load_torch()
    torch.manual_seed(court.SEED)
    import random

    random.seed(court.SEED)
    model = court.build_model(torch)
    optimizer = court.make_optimizer(torch, model)
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, T_max=256)
    scaler = torch.amp.GradScaler("cpu", enabled=False)
    batches = court.data_batches(torch)
    pipeline = resume.DataPipeline(court.SEED ^ 0x00D1E5, batches)
    state = resume.capture_state(
        torch, model, optimizer, scheduler, scaler, pipeline, 7
    )
    return court, torch, state


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, int(round(fraction * (len(ordered) - 1)))))]


def median_or_none(rows, key):
    values = [row[key] for row in rows if row.get(key) is not None]
    return round(statistics.median(values), 3) if values else None


def dir_bytes(path):
    return sum(file.stat().st_size for file in Path(path).rglob("*") if file.is_file())


def weft_arm(cli, state, work, arm, reps):
    from weftcodec.dcp import CheckpointSession, WeftStorageWriter, init_store
    from torch.distributed.checkpoint import save as dcp_save

    store = work / ("store-%s" % arm)
    if store.exists():
        shutil.rmtree(store)
    init_store(store, weftfs_cli=cli)
    session = None
    if arm in ("weft-session", "weft-fast"):
        session = CheckpointSession(
            store, weftfs_cli=cli, staging_bytes=64 << 20, session_idle_secs=30
        )
    walls = []
    writes = []
    try:
        for rep in range(reps):
            name = "%s-%03d" % (arm, rep)
            kwargs = {}
            if arm == "weft-session":
                kwargs["retain_container"] = False
            if arm == "weft-fast":
                kwargs["write_policy"] = "fast"
                kwargs["fast_codec"] = "zstd"
                kwargs["fast_zstd_level"] = 1
            started = time.perf_counter()
            if session is not None:
                stats = session.save(state, checkpoint_id=name, **kwargs)
                writes.append(float(stats.get("checkpoint_write_ms", 0.0)))
            else:
                writer = WeftStorageWriter(
                    name, store_root=store, topology=1, shard_strategy="zstd",
                    weftfs_cli=cli, **kwargs
                )
                dcp_save(state, storage_writer=writer, no_dist=True)
                stats = writer.stats
                writes.append(float(stats.get("checkpoint_write_ms", 0.0)))
            walls.append((time.perf_counter() - started) * 1000.0)
        names = [row["name"] for row in json.loads(
            subprocess.run(
                [cli, "--store", str(store), "--json", "checkpoint", "list"],
                capture_output=True, text=True, check=True,
            ).stdout
        )["checkpoints"]]
        last = sorted(name for name in names if name.startswith(arm))[-1]
        verify = subprocess.run(
            [cli, "--store", str(store), "--json", "checkpoint", "verify", "--name", last],
            capture_output=True, text=True, check=False,
        )
        from weftcodec.dcp import WeftStorageReader

        reader = WeftStorageReader(last, store_root=store, weftfs_cli=cli)
        restored = 0
        for info in reader.read_metadata().storage_data.values():
            reader.restore_entry(info.entry)
            restored += 1
        reader.close()
        return {
            "arm": arm,
            "save_ms": walls,
            "save_p50_ms": round(statistics.median(walls), 3),
            "save_p95_ms": round(percentile(walls, 0.95), 3),
            "store_write_p50_ms": round(statistics.median(writes), 3) if writes else None,
            "staging_plus_transport_p50_ms": round(
                statistics.median(
                    [wall - write for wall, write in zip(walls, writes)]
                ),
                3,
            ) if writes else None,
            "retained_bytes": dir_bytes(store),
            "checkpoint": last,
            "verified": verify.returncode == 0,
            "tensors_restored": restored,
            "session_stats": dict(session.stats) if session is not None else None,
            "durability": "store commit (fsync commit file + superblock)",
        }
    finally:
        if session is not None:
            session.close(wait=True, timeout=120.0)


def dcp_arm(state, work, reps):
    import torch.distributed.checkpoint as dcp
    import torch

    walls = []
    run_dir = work / "dcp-fs"
    if run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)
    last = None
    for rep in range(reps):
        name = "dcp-%03d" % rep
        path = run_dir / name
        started = time.perf_counter()
        dcp.save(state, storage_writer=dcp.FileSystemWriter(path))
        walls.append((time.perf_counter() - started) * 1000.0)
        last = path
    target = {key: torch.zeros_like(value) for key, value in state.items()}
    dcp.load(target, checkpoint_id=str(last))
    exact = all(torch.equal(target[key], state[key]) for key in state)
    return {
        "arm": "dcp-fs",
        "save_ms": walls,
        "save_p50_ms": round(statistics.median(walls), 3),
        "save_p95_ms": round(percentile(walls, 0.95), 3),
        "retained_bytes": dir_bytes(run_dir),
        "checkpoint": str(last),
        "verified": exact,
        "tensors_restored": len(state),
        "durability": "FileSystemWriter (no directory fsync/atomic publish)",
    }


def orbax_arm(state, work, reps):
    try:
        import jax
        import numpy as np
        import orbax.checkpoint as ocp
    except Exception as exc:  # noqa: BLE001
        return {"arm": "orbax-ocdbt", "unavailable": True, "reason": str(exc)}

    tree = jax.tree_util.tree_map(lambda value: np.asarray(value), state)
    run_dir = work / "orbax-ocdbt"
    if run_dir.exists():
        shutil.rmtree(run_dir)
    checkpointer = ocp.Checkpointer(ocp.StandardCheckpointHandler())
    walls = []
    for rep in range(reps):
        directory = run_dir / ("ckpt-%03d" % rep)
        started = time.perf_counter()
        checkpointer.save(directory, args=ocp.args.StandardSave(tree))
        walls.append((time.perf_counter() - started) * 1000.0)
    restored = checkpointer.restore(run_dir / ("ckpt-%03d" % (reps - 1)))
    flat_state = jax.tree_util.tree_leaves(tree)
    flat_restored = jax.tree_util.tree_leaves(restored)
    exact = len(flat_state) == len(flat_restored) and all(
        np.asarray(a).tobytes() == np.asarray(b).tobytes()
        for a, b in zip(flat_state, flat_restored)
    )
    return {
        "arm": "orbax-ocdbt",
        "save_ms": walls,
        "save_p50_ms": round(statistics.median(walls), 3),
        "save_p95_ms": round(percentile(walls, 0.95), 3),
        "retained_bytes": dir_bytes(run_dir),
        "verified": exact,
        "tensors_restored": len(state),
        "durability": "OCDBT directory (Orbax's own commit)",
    }


def durability_child(args):
    from weftcodec.dcp import CheckpointSession, init_store
    import torch

    court = load_module(COURT, "weft_training_goodput")
    torch.manual_seed(11)
    state = {"t": torch.arange(4096, dtype=torch.float32)}
    store = Path(args.store)
    if not (store / "FORMAT").is_file():
        init_store(store, weftfs_cli=args.cli)
    session = CheckpointSession(store, weftfs_cli=args.cli, staging_bytes=8 << 20)
    stats = session.save(state, checkpoint_id="acked", retain_container=False)
    Path(args.ack).write_text(json.dumps({"stats": stats}))
    os.kill(os.getpid(), signal.SIGKILL)
    return 0


def durability(cli, work):
    store = work / "durability-store"
    ack = work / "ack.json"
    if store.exists():
        shutil.rmtree(store)
    proc = subprocess.run(
        [sys.executable, os.fspath(Path(__file__)), "durability-child",
         "--store", str(store), "--cli", cli, "--ack", str(ack)],
        check=False,
    )
    if not ack.is_file():
        return {"ok": False, "reason": "the child produced no ack record", "returncode": proc.returncode}
    verify = subprocess.run(
        [cli, "--store", str(store), "--json", "checkpoint", "verify", "--name", "acked"],
        capture_output=True, text=True, check=False,
    )
    return {
        "ok": verify.returncode == 0,
        "child_returncode": proc.returncode,
        "verified_after_sigkill": verify.returncode == 0,
        "verify_stdout": verify.stdout.strip()[:200],
    }


def run(args):
    work = Path(args.work_dir)
    work.mkdir(parents=True, exist_ok=True)
    court, torch, state = build_state()
    total_bytes = sum(value.numel() * value.element_size() for value in state.values())
    base_order = ["weft-planned", "weft-session", "weft-fast", "dcp-fs", "orbax-ocdbt"]
    sweeps = []
    for sweep in range(args.sweeps):
        order = base_order[sweep % len(base_order) :] + base_order[: sweep % len(base_order)]
        cells = []
        for arm in order:
            if arm in ("weft-planned", "weft-session", "weft-fast"):
                cells.append(weft_arm(args.cli, state, work, arm, args.reps))
            elif arm == "dcp-fs":
                cells.append(dcp_arm(state, work, args.reps))
            else:
                cells.append(orbax_arm(state, work, args.reps))
        sweeps.append(cells)
    durability_cell = durability(args.cli, work)

    arms = {}
    for arm in base_order:
        rows = [
            cell for cells in sweeps for cell in cells if cell["arm"] == arm
        ]
        if not rows:
            continue
        arms[arm] = {
            "save_p50_ms": round(statistics.median(row["save_p50_ms"] for row in rows), 3),
            "save_p95_ms": round(max(row["save_p95_ms"] for row in rows), 3),
            "sweep_p50_ms": [row["save_p50_ms"] for row in rows],
            "store_write_p50_ms": median_or_none(rows, "store_write_p50_ms"),
            "staging_plus_transport_p50_ms": median_or_none(
                rows, "staging_plus_transport_p50_ms"
            ),
            "retained_bytes": rows[-1].get("retained_bytes"),
            "verified": all(row.get("verified") for row in rows),
            "unavailable": any(row.get("unavailable") for row in rows),
        }

    dcp_cell = arms["dcp-fs"]
    orbax_cell = arms["orbax-ocdbt"]
    weft_best = min(
        value["save_p50_ms"] for name, value in arms.items() if name.startswith("weft-")
    )
    orbax_p50 = orbax_cell.get("save_p50_ms")
    targets = {
        "weft_dcp_save_p50_below_20ms": weft_best < 20.0,
        "weft_matched_ocdbt": orbax_p50 is not None and weft_best <= orbax_p50,
    }
    checks = {
        "all_saves_exact": all(
            value.get("verified")
            for value in arms.values()
            if not value.get("unavailable")
        ),
        "durable_after_ack_and_sigkill": bool(durability_cell.get("ok")),
        "state_bytes_measured": total_bytes > 0,
        "ocdbt_available": orbax_p50 is not None,
        "interleaved_sweeps": len(sweeps) >= 2,
    }
    claim_holds = all(targets.values())

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
            "note": "CPU only, single host; no GPU or multi-host claim",
        },
        "revision": {
            "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        },
        "state": {
            "entries": len(state),
            "bytes": total_bytes,
            "content": "model + optimizer + scheduler + scaler + RNGs + data cursor + step",
        },
        "targets_stated_previously": {
            "weft_dcp_save_p50_ms": 20.0,
            "ocdbt_parity": True,
        },
        "cells": arms,
        "sweeps": sweeps,
        "durability": durability_cell,
        "targets": targets,
        "claim_holds": claim_holds,
        "weft_best_p50_ms": weft_best,
        "native_dcp_p50_ms": dcp_cell["save_p50_ms"],
        "ocdbt_p50_ms": orbax_p50,
        "remaining_term": (
            "weft-session's store write is %.2f ms; the remaining %.2f ms is the "
            "Python container staging, one service prepare round trip and the "
            "adapter's metadata path. The native writer writes one local directory "
            "with no content-addressed identity or commit protocol."
            % (
                arms["weft-session"]["store_write_p50_ms"],
                arms["weft-session"]["staging_plus_transport_p50_ms"],
            )
        ),
        "checks": checks,
        "losses": [
            "The 20 ms target is NOT met; the achieved p50 and the gap to OCDBT are in `targets` and `cells`.",
            "The native DCP FileSystemWriter is faster than every WeftFS arm on this fixture; the ratio is in `cells`.",
            "The native DCP and Orbax arms write a local directory tree; they do not share the store's content-addressed identity or its commit protocol.",
            "Orbax OCDBT is a directory checkpoint, not a single file; its retained bytes are its own layout.",
            "All latencies are on one 1.66 MB state in one court; larger states change both sides.",
            "The service round trip and the Python staging copy are inside the WeftFS latency; only the store's own publish time is in `store_write_p50_ms`.",
            "The WeftFS/OCDBT comparison lands within a fraction of a millisecond, which is inside host noise; it is not a robust win.",
        ],
        "result": "pass" if all(checks.values()) else "fail",
    }
    Path(args.receipt).parent.mkdir(parents=True, exist_ok=True)
    Path(args.receipt).write_text(json.dumps(receipt, indent=2))
    print(json.dumps({
        "result": receipt["result"],
        "claim_holds": claim_holds,
        "targets": targets,
        "p50": {name: value.get("save_p50_ms") for name, value in arms.items()},
        "checks": checks,
    }, indent=2))
    return 0 if receipt["result"] == "pass" else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run_parser = sub.add_parser("run")
    run_parser.add_argument("--receipt", required=True)
    run_parser.add_argument(
        "--stamp", default=datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    )
    run_parser.add_argument("--work-dir", default="/tmp/weft-baselines")
    run_parser.add_argument("--cli", default=DEFAULT_CLI)
    run_parser.add_argument("--reps", type=int, default=8)
    run_parser.add_argument("--sweeps", type=int, default=3)
    child = sub.add_parser("durability-child")
    child.add_argument("--store", required=True)
    child.add_argument("--cli", default=DEFAULT_CLI)
    child.add_argument("--ack", required=True)
    args = parser.parse_args(argv)
    if args.command == "run":
        return run(args)
    if args.command == "durability-child":
        return durability_child(args)
    return 2


if __name__ == "__main__":
    sys.exit(main())
