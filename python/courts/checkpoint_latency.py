#!/usr/bin/env python3
"""Checkpoint completion latency court (subphase R12).

R11 measured a PyTorch checkpoint completing in **444.1 ms p50** on the
training-goodput fixture (a 4.99 MB / 169-entry state written through
``weftcodec.dcp``), against local DCP's 20.1 ms. This court decomposes that
number instead of guessing at it, and re-measures it after every R12 change.

It measures the *same* fixture as ``python/courts/training_goodput.py``: the
identical three-block transformer stack (``build_model``), AdamW state after a
real optimizer step, captured by the identical ``capture_state``. Only the
surrounding loop is gone, so a save here is the checkpoint half of that court.

Per save this records:

* the client-visible wall time of ``torch.distributed.checkpoint.save`` with a
  fresh ``WeftStorageWriter`` (the R11 shape: one writer per save);
* the adapter's own client-side sections (staging container build, resolve,
  hashing, memfd handoff, socket round trip, manifest verification, DCP
  metadata serialise + commit) from the probe sink in ``weftcodec.service``;
* the service's Rust phase profile over the same window (store open, state
  replay, plane probe, digest, encode, blob publish, commit, index build/load/
  attest/persist, metadata commit), read with the protocol-3 ``PROFILE`` op;
* store bytes and file counts before and after each save.

Nested Rust phases are reported with their parent (`state_replay` and
`plane_probe` inside `store_open`; `index_build`/`index_load`/`index_attest`
inside `index_segment`) and are never added to it.

Runs inside the Docker python court:

  scripts/docker-court.sh python \
    "PYTHONPATH=/work/python python3 python/courts/checkpoint_latency.py run \
       --receipt bench/receipts/checkpoint-latency <stamp>.json"
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import resource
import shutil
import statistics
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

for _name in (
    "OMP_NUM_THREADS",
    "MKL_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "NUMEXPR_NUM_THREADS",
):
    os.environ.setdefault(_name, "1")

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))

import training_goodput as tg  # noqa: E402  (shares the R11 fixture exactly)

from weftcodec.dcp import (  # noqa: E402
    WeftStorageWriter,
    init_store,
    set_probe_sink,
    verify_checkpoint,
)

SCHEMA = "weft.checkpoint-latency.v1"
DEFAULT_REPS = 6
DEFAULT_FRESH = 3


def utc_stamp():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp, path)


def tree_stats(root):
    """Bytes and file count per top-level directory, recursively."""
    root = Path(root)
    total = 0
    files = 0
    by_dir = {}
    for dirpath, _dirnames, filenames in os.walk(root):
        rel = os.path.relpath(dirpath, root)
        key = "root" if rel == "." else rel.split(os.sep)[0]
        bucket = by_dir.setdefault(key, {"bytes": 0, "files": 0})
        for name in filenames:
            try:
                size = os.path.getsize(os.path.join(dirpath, name))
            except OSError:
                continue
            total += size
            files += 1
            bucket["bytes"] += size
            bucket["files"] += 1
    return {"bytes": total, "files": files, "by_dir": by_dir}


class Collector:
    """Aggregates client probe events for one save."""

    def __init__(self):
        self.events = {}

    def __call__(self, name, duration_ns):
        bucket = self.events.setdefault(name, [])
        bucket.append(int(duration_ns))

    def summary(self):
        out = {}
        for name, values in sorted(self.events.items()):
            out[name] = {
                "count": len(values),
                "total_ms": sum(values) / 1e6,
                "max_ms": max(values) / 1e6,
            }
        return out

    def reset(self):
        self.events = {}


def fixture(torch):
    """The exact R11 state: a real optimizer step, then capture_state."""
    # `training_goodput.child_loop` seeds before building the model; without the
    # same seed this court would save different weights on every run.
    torch.manual_seed(tg.SEED)
    model = tg.build_model(torch)
    optimizer = tg.make_optimizer(torch, model)
    batches = tg.data_batches(torch)
    tg.train_step(torch, model, optimizer, batches, 1)
    state = tg.capture_state(torch, model, optimizer, 64)
    return state


def one_save(torch, *, store_root, name, state, collector, service_cli):
    """One R11-shaped save: a fresh writer, `dcp.save`, then close."""
    import torch.distributed.checkpoint as dcp

    before = tree_stats(store_root)
    collector.reset()
    set_probe_sink(collector)

    started = time.perf_counter()
    writer = WeftStorageWriter(
        name,
        store_root=str(store_root),
        topology=1,
        shard_strategy="zstd",
        staging_bytes=tg.STAGING_BYTES,
        init_store=False,
        weftfs_cli=service_cli,
    )
    constructed = time.perf_counter()
    service = writer._get_service()
    profile_available = True
    try:
        service.profile(0)
    except Exception:  # noqa: BLE001 - protocol < 3: no profile, still measure
        profile_available = False
    profiled = time.perf_counter()

    result = dcp.save(
        state,
        storage_writer=writer,
        no_dist=True,
    )
    saved = time.perf_counter()

    rust_profile = {}
    if profile_available:
        rust_profile = service.profile(1)
        try:
            service.profile(2)
        except Exception:  # noqa: BLE001
            pass
    profiled_after = time.perf_counter()

    stats = dict(writer.stats)
    writer.close()
    closed = time.perf_counter()
    set_probe_sink(None)

    after = tree_stats(store_root)
    deltas = {}
    for key in sorted(set(before["by_dir"]) | set(after["by_dir"])):
        b = before["by_dir"].get(key, {"bytes": 0, "files": 0})
        a = after["by_dir"].get(key, {"bytes": 0, "files": 0})
        deltas[key] = {"bytes": a["bytes"] - b["bytes"], "files": a["files"] - b["files"]}

    return {
        "name": name,
        "wall_ms": (saved - started) * 1000.0,
        "construct_ms": (constructed - started) * 1000.0,
        "profile_begin_ms": (profiled - constructed) * 1000.0,
        "save_ms": (saved - profiled) * 1000.0,
        "profile_take_ms": (profiled_after - saved) * 1000.0,
        "close_ms": (closed - profiled_after) * 1000.0,
        "checkpoint_write_ms": stats.get("checkpoint_write_ms"),
        "container_bytes": stats.get("container_bytes"),
        "entries": stats.get("entries"),
        "tensor_bytes": stats.get("tensor_bytes"),
        "client": collector.summary(),
        "rust": rust_profile,
        "profile_available": profile_available,
        "store_delta_bytes": after["bytes"] - before["bytes"],
        "store_delta_files": after["files"] - before["files"],
        "store_delta_by_dir": deltas,
        "store_total_bytes": after["bytes"],
        "store_total_files": after["files"],
        "result": {
            key: value
            for key, value in (
                ("storage_name", getattr(writer, "store_name", None)),
                ("written", 1 if result is not None else 0),
            )
        },
    }


def decompose(rows):
    """Median of each phase name across rows, plus the accounted share."""
    def collect(source):
        names = set()
        for row in rows:
            names.update(row[source])
        out = {}
        for name in sorted(names):
            def total_ms(row):
                entry = row[source].get(name)
                if not entry:
                    return 0.0
                if "total_ms" in entry:
                    return entry["total_ms"]
                return entry.get("total_ns", 0) / 1e6

            totals = [total_ms(row) for row in rows]
            counts = [row[source].get(name, {}).get("count", 0) for row in rows]
            out[name] = {
                "median_ms": statistics.median(totals),
                "median_count": statistics.median(counts),
                "max_ms": max(totals),
            }
        return out

    client = collect("client")
    rust = collect("rust")
    wall = statistics.median(row["wall_ms"] for row in rows)
    # Client sections are disjoint except that `adapter.stage_total` encloses
    # `adapter.resolve` and `adapter.hash`, `adapter.service_obtain` encloses
    # `client.service_start`, and `adapter.metadata_commit` encloses
    # `client.metadata_request`.
    nested_client = {
        "adapter.resolve",
        "adapter.hash",
        "client.service_start",
        "client.metadata_request",
    }
    client_top = sum(v["median_ms"] for k, v in client.items() if k not in nested_client)
    # Rust phases: drop the nested ones (they are named by `nested_in` in the
    # Rust profile doc; the set is fixed in this court).
    nested_rust = {
        "state_replay",
        "plane_probe",
        "index_build",
        "index_load",
        "index_attest",
        "encode_zstd",
        "encode_zstd19",
        "encode_float_tensor",
        "encode_semantic",
    }
    rust_top = sum(v["median_ms"] for k, v in rust.items() if k not in nested_rust)
    return {
        "wall_ms": wall,
        "client": client,
        "rust": rust,
        "client_top_level_ms": client_top,
        "client_accounted_share": (client_top / wall) if wall else None,
        "rust_top_level_ms": rust_top,
    }


def session_series(torch, *, store_root, state, reps, collector, service_cli, idle_secs):
    """N saves through one long-lived session (warm service + held handle)."""
    from weftcodec.dcp import CheckpointSession

    rows = []
    session = CheckpointSession(
        store_root,
        weftfs_cli=service_cli,
        staging_bytes=tg.STAGING_BYTES,
        session_idle_secs=idle_secs,
    )
    try:
        service = session.service()
        for index in range(reps):
            before = tree_stats(store_root)
            collector.reset()
            set_probe_sink(collector)
            service.profile(0)
            started = time.perf_counter()
            stats = session.save(state, checkpoint_id="ckpt-%d" % index)
            saved = time.perf_counter()
            rust_profile = service.profile(1)
            service.profile(2)
            set_probe_sink(None)
            after = tree_stats(store_root)
            deltas = {}
            for key in sorted(set(before["by_dir"]) | set(after["by_dir"])):
                b = before["by_dir"].get(key, {"bytes": 0, "files": 0})
                a = after["by_dir"].get(key, {"bytes": 0, "files": 0})
                deltas[key] = {
                    "bytes": a["bytes"] - b["bytes"],
                    "files": a["files"] - b["files"],
                }
            rows.append(
                {
                    "name": "ckpt-%d" % index,
                    "scenario": "session",
                    "index": index,
                    "wall_ms": (saved - started) * 1000.0,
                    "checkpoint_write_ms": stats.get("checkpoint_write_ms"),
                    "container_bytes": stats.get("container_bytes"),
                    "entries": stats.get("entries"),
                    "tensor_bytes": stats.get("tensor_bytes"),
                    "client": collector.summary(),
                    "rust": rust_profile,
                    "profile_available": True,
                    "store_delta_bytes": after["bytes"] - before["bytes"],
                    "store_delta_files": after["files"] - before["files"],
                    "store_delta_by_dir": deltas,
                    "store_total_bytes": after["bytes"],
                    "store_total_files": after["files"],
                }
            )
        session_stats = dict(session.stats)
        service_pid = session.service_pid
    finally:
        session.close()
    for row in rows:
        row["session_stats"] = session_stats
        row["service_pid"] = service_pid
    return rows


def run(args):
    torch = tg.load_torch()
    import torch.distributed.checkpoint  # noqa: F401  (fail early, not mid-run)

    state = fixture(torch)
    entries = len(state)
    total_bytes = sum(
        int(value.numel() * value.element_size()) for value in state.values()
    )
    fixture_sha256, _ = tg.flat_sha256(torch, state)
    collector = Collector()
    work = Path(args.work_dir)
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)

    fresh_rows = []
    for index in range(args.fresh):
        store = work / ("fresh-%d" % index)
        store.mkdir()
        init_store(store, weftfs_cli=args.weftfs_cli)
        row = one_save(
            torch,
            store_root=store,
            name="ckpt",
            state=state,
            collector=collector,
            service_cli=args.weftfs_cli,
        )
        row["scenario"] = "fresh-store"
        fresh_rows.append(row)

    store = work / "sequential"
    store.mkdir()
    init_store(store, weftfs_cli=args.weftfs_cli)
    sequential_rows = []
    for index in range(args.reps):
        row = one_save(
            torch,
            store_root=store,
            name="ckpt-%d" % index,
            state=state,
            collector=collector,
            service_cli=args.weftfs_cli,
        )
        row["scenario"] = "same-store"
        row["index"] = index
        sequential_rows.append(row)

    session_rows = []
    if args.mode in ("both", "session"):
        session_store = work / "session"
        session_store.mkdir()
        init_store(session_store, weftfs_cli=args.weftfs_cli)
        session_rows = session_series(
            torch,
            store_root=session_store,
            state=state,
            reps=args.reps,
            collector=collector,
            service_cli=args.weftfs_cli,
            idle_secs=args.session_idle_secs,
        )

    verify = verify_checkpoint(store, "ckpt-%d" % (args.reps - 1), weftfs_cli=args.weftfs_cli)
    session_verify = None
    if session_rows:
        session_verify = verify_checkpoint(
            work / "session",
            "ckpt-%d" % (len(session_rows) - 1),
            weftfs_cli=args.weftfs_cli,
            full=True,
        )

    all_rows = fresh_rows + sequential_rows
    later_rows = sequential_rows[1:] if len(sequential_rows) > 1 else sequential_rows
    receipt = {
        "schema": SCHEMA,
        "stamp": args.stamp,
        "measured_at": now_iso(),
        "fixture": {
            "source": "python/courts/training_goodput.py build_model/capture_state",
            "entries": entries,
            "tensor_bytes": total_bytes,
            "flat_sha256": fixture_sha256,
            "topology": 1,
            "shard_strategy": "zstd",
            "staging_bytes": tg.STAGING_BYTES,
            "note": "the R11 training-goodput checkpoint state, saved once per writer",
        },
        "environment": {
            "python": platform.python_version(),
            "torch": getattr(torch, "__version__", "unknown"),
            "platform": platform.platform(),
            "kernel": platform.release(),
            "cpu_count": os.cpu_count(),
            "weftfs_cli": args.weftfs_cli,
            "court_image": os.environ.get("WEFT_COURT_IMAGE"),
            "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID"),
            "court_memory": os.environ.get("WEFT_COURT_MEMORY"),
            "court_cpus": os.environ.get("WEFT_COURT_CPUS"),
            "court_pids": os.environ.get("WEFT_COURT_PIDS"),
            "repo_weftfs": os.environ.get("WEFT_REPO_WEFTFS"),
            "repo_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC"),
        },
        "config": {
            "reps": args.reps,
            "fresh": args.fresh,
            "wait_for_previous": False,
            "writer_per_save": True,
        },
        "summary": {
            "fresh_store_wall_ms": [round(row["wall_ms"], 3) for row in fresh_rows],
            "fresh_store_median_ms": statistics.median(
                row["wall_ms"] for row in fresh_rows
            ),
            "sequential_wall_ms": [
                round(row["wall_ms"], 3) for row in sequential_rows
            ],
            "sequential_first_ms": sequential_rows[0]["wall_ms"],
            "sequential_later_median_ms": statistics.median(
                row["wall_ms"] for row in later_rows
            ),
            "sequential_all_median_ms": statistics.median(
                row["wall_ms"] for row in sequential_rows
            ),
            "session_wall_ms": [round(row["wall_ms"], 3) for row in session_rows],
            "session_first_ms": (session_rows[0]["wall_ms"] if session_rows else None),
            "session_median_ms": (
                statistics.median(row["wall_ms"] for row in session_rows)
                if session_rows
                else None
            ),
            "session_services_started": (
                session_rows[0]["session_stats"]["services_started"]
                if session_rows
                else None
            ),
        },
        "decomposition": {
            "fresh_store": decompose(fresh_rows),
            "sequential_first": decompose(sequential_rows[:1]),
            "sequential_later": decompose(later_rows),
            "session": decompose(session_rows) if session_rows else {},
            "session_later": (
                decompose(session_rows[1:]) if len(session_rows) > 1 else {}
            ),
        },
        "runs": all_rows + session_rows,
        "verify": verify,
        "session_verify": session_verify,
        "peak_rss_kib": int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss),
    }
    if args.receipt:
        write_json(args.receipt, receipt)
        print("checkpoint-latency receipt: %s" % args.receipt)
    print(
        "fresh median %.1f ms | writer-per-save first %.1f / later %.1f | "
        "session first %.1f / median %.1f"
        % (
            receipt["summary"]["fresh_store_median_ms"],
            receipt["summary"]["sequential_first_ms"],
            receipt["summary"]["sequential_later_median_ms"],
            receipt["summary"]["session_first_ms"] or 0.0,
            receipt["summary"]["session_median_ms"] or 0.0,
        )
    )
    return 0


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run_p = sub.add_parser("run")
    run_p.add_argument("--receipt")
    run_p.add_argument("--stamp", default=utc_stamp())
    run_p.add_argument("--work-dir", default="/tmp/weft-checkpoint-latency")
    run_p.add_argument("--reps", type=int, default=DEFAULT_REPS)
    run_p.add_argument("--fresh", type=int, default=DEFAULT_FRESH)
    run_p.add_argument(
        "--mode", choices=("both", "writer", "session"), default="both"
    )
    run_p.add_argument("--session-idle-secs", type=int, default=30)
    run_p.add_argument(
        "--weftfs-cli",
        default=os.environ.get("WEFTFS_CLI", "/work/.docker-cache/target-rust/release/weftfs"),
    )
    args = parser.parse_args(argv)
    if args.command == "run":
        return run(args)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
