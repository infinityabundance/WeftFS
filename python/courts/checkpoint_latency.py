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
import hashlib
import statistics
import subprocess
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
    CheckpointSession,
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


SIZE_BUCKETS = (
    (0, 512),
    (513, 4096),
    (4097, 16384),
    (16385, 65536),
    (65537, 262144),
    (262145, 1 << 20),
    ((1 << 20) + 1, 1 << 62),
)


def _bucket_name(low, high):
    if high >= (1 << 62):
        return ">1MiB"
    if low == 0:
        return "<=512B"
    return "%dB..%dB" % (low, high)


def tree_stats(root):
    """Bytes, file count per top-level directory, and every file's size."""
    root = Path(root)
    total = 0
    files = 0
    by_dir = {}
    sizes = {}
    for dirpath, _dirnames, filenames in os.walk(root):
        rel = os.path.relpath(dirpath, root)
        key = "root" if rel == "." else rel.split(os.sep)[0]
        bucket = by_dir.setdefault(key, {"bytes": 0, "files": 0})
        for name in filenames:
            path = os.path.join(dirpath, name)
            try:
                size = os.path.getsize(path)
            except OSError:
                continue
            total += size
            files += 1
            bucket["bytes"] += size
            bucket["files"] += 1
            sizes[os.path.relpath(path, root)] = size
    return {"bytes": total, "files": files, "by_dir": by_dir, "sizes": sizes}


def frame_histogram(before, after):
    """Size buckets of the files this save *created*."""
    created = [after["sizes"][name] for name in after["sizes"] if name not in before["sizes"]]
    out = {_bucket_name(low, high): 0 for low, high in SIZE_BUCKETS}
    for size in created:
        for low, high in SIZE_BUCKETS:
            if low <= size <= high:
                out[_bucket_name(low, high)] += 1
                break
    return {"files": len(created), "buckets": out, "total_bytes": sum(created)}


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


def one_save(torch, *, store_root, name, state, collector, service_cli, atomic=True):
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
        atomic_publish=atomic,
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
    histogram = frame_histogram(before, after)

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
        "frame_histogram": histogram,
        "finish": stats.get("finish"),
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

    if not rows:
        return {
            "wall_ms": None,
            "client": {},
            "rust": {},
            "client_top_level_ms": None,
            "client_accounted_share": None,
            "rust_top_level_ms": None,
        }
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


def session_series(
    torch,
    *,
    store_root,
    state,
    reps,
    collector,
    service_cli,
    idle_secs,
    atomic=True,
    extra_args=(),
    label="session",
    policy=(),
):
    """N saves through one long-lived session (warm service + held handle)."""
    from weftcodec.dcp import CheckpointSession

    rows = []
    session = CheckpointSession(
        store_root,
        weftfs_cli=service_cli,
        staging_bytes=tg.STAGING_BYTES,
        session_idle_secs=idle_secs,
        extra_args=extra_args,
    )
    try:
        service = session.service()
        for index in range(reps):
            before = tree_stats(store_root)
            collector.reset()
            set_probe_sink(collector)
            service.profile(0)
            started = time.perf_counter()
            stats = session.save(
                state,
                checkpoint_id="ckpt-%d" % index,
                atomic_publish=atomic,
                **dict(policy),
            )
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
            histogram = frame_histogram(before, after)
            rows.append(
                {
                    "name": "ckpt-%d" % index,
                    "scenario": label,
                    "index": index,
                    "wall_ms": (saved - started) * 1000.0,
                    "checkpoint_write_ms": stats.get("checkpoint_write_ms"),
                    "container_bytes": stats.get("container_bytes"),
                    "container_copies": stats.get("container_copies"),
                    "staging_buffer_bytes": stats.get("staging_buffer_bytes"),
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
                    "frame_histogram": histogram,
                    "finish": stats.get("finish"),
                    "session_stats": dict(session.stats),
                }
            )
        session_stats = dict(session.stats)
        service_pid = session.service_pid
        restore = restore_latency(session, state)
    finally:
        session.close()
    for row in rows:
        row["session_stats"] = session_stats
        row["service_pid"] = service_pid
        row["restore"] = restore
    return rows


def restore_latency(session, state):
    """Restore every tensor and one tensor through the same session."""
    service = session.service()
    names = service.list_checkpoints()
    if not names:
        return None
    name = names[-1]
    manifest = service.manifest(name)
    started = time.perf_counter()
    total = 0
    digest = hashlib.sha256()
    for row in manifest.tensors:
        buffer = service.restore_tensor(
            name, row.name, expected_size=row.element_bytes
        )
        total += buffer.size
        digest.update(row.name.encode("utf-8") + b"\x00")
        digest.update(hashlib.sha256(buffer.view).digest())
        buffer.close()
    whole_ms = (time.perf_counter() - started) * 1000.0
    row = max(manifest.tensors, key=lambda item: item.element_bytes)
    started = time.perf_counter()
    buffer = service.restore_tensor(name, row.name, expected_size=row.element_bytes)
    buffer.close()
    largest_ms = (time.perf_counter() - started) * 1000.0
    return {
        "checkpoint": name,
        "tensors": len(manifest.tensors),
        "restore_digest": digest.hexdigest(),
        "bytes": total,
        "whole_ms": whole_ms,
        "largest_tensor": row.name,
        "largest_bytes": row.element_bytes,
        "largest_ms": largest_ms,
    }


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

    two_commit_rows = []
    if args.mode in ("both", "session"):
        two_store = work / "two-commit"
        two_store.mkdir()
        init_store(two_store, weftfs_cli=args.weftfs_cli)
        two_commit_rows = session_series(
            torch,
            store_root=two_store,
            state=state,
            reps=args.reps,
            collector=collector,
            service_cli=args.weftfs_cli,
            idle_secs=args.session_idle_secs,
            atomic=False,
        )
        for row in two_commit_rows:
            row["scenario"] = "session-two-commit"

    session_rows = []
    packed_rows = []
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

    for target, member in args.pack:
        label = "packed-%d-%d" % (target, member)
        fresh_label = label + "-fresh"
        fresh_pack_store = work / fresh_label
        fresh_pack_store.mkdir()
        init_store(fresh_pack_store, weftfs_cli=args.weftfs_cli)
        fresh_packed = session_series(
            torch,
            store_root=fresh_pack_store,
            state=state,
            reps=1,
            collector=collector,
            service_cli=args.weftfs_cli,
            idle_secs=args.session_idle_secs,
            extra_args=(
                "--pack",
                "--pack-target-bytes",
                str(target),
                "--pack-member-threshold",
                str(member),
            ),
            label=fresh_label,
        )
        packed_rows.extend(fresh_packed)
        pack_store = work / label
        pack_store.mkdir()
        init_store(pack_store, weftfs_cli=args.weftfs_cli)
        rows = session_series(
            torch,
            store_root=pack_store,
            state=state,
            reps=args.reps,
            collector=collector,
            service_cli=args.weftfs_cli,
            idle_secs=args.session_idle_secs,
            extra_args=(
                "--pack",
                "--pack-target-bytes",
                str(target),
                "--pack-member-threshold",
                str(member),
            ),
            label=label,
        )
        # A packed store is only readable through the packed plane, so the
        # plain `weftfs checkpoint verify` CLI cannot see it. Exactness is
        # proven by restoring every tensor through the session instead (the
        # receipt's `restore_digest` must equal the unpacked one).
        for row in rows:
            row["pack_verify"] = None
        packed_rows.extend(rows)

    policy_rows = {}
    for label, policy in args.policy:
        policy_store = work / label
        policy_store.mkdir()
        init_store(policy_store, weftfs_cli=args.weftfs_cli)
        rows = session_series(
            torch,
            store_root=policy_store,
            state=state,
            reps=max(3, args.reps // 2),
            collector=collector,
            service_cli=args.weftfs_cli,
            idle_secs=args.session_idle_secs,
            label=label,
            policy=policy,
        )
        entry = {
            "walls": [round(r["wall_ms"], 3) for r in rows],
            "median_ms": statistics.median(r["wall_ms"] for r in rows),
            "last_delta_bytes": rows[-1]["store_delta_bytes"],
            "container_copies": rows[-1].get("container_copies"),
            "staging_buffer_bytes": rows[-1].get("staging_buffer_bytes"),
            "stage_total_ms": (
                rows[-1]["client"].get("adapter.stage_total", {}).get("total_ms")
            ),
            "store_bytes": rows[-1]["store_total_bytes"],
            "restore": rows[-1].get("restore"),
            "policy": dict(policy),
        }
        if label.startswith("fast-deferred"):
            # One acknowledgement is already durable; this is the later
            # optimisation pass, with its cost and the split it leaves behind.
            name = "ckpt-%d" % (len(rows) - 1)
            started = time.perf_counter()
            proc = subprocess.run(
                [
                    args.weftfs_cli,
                    "--store",
                    str(policy_store),
                    "transcode",
                    "--root",
                    "/checkpoints/%s" % name,
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            entry["transcode_ms"] = (time.perf_counter() - started) * 1000.0
            entry["transcode_ok"] = proc.returncode == 0
            if proc.returncode != 0:
                entry["transcode_error"] = proc.stderr.strip()[-400:]
            entry["post_transcode_store_bytes"] = tree_stats(policy_store)["bytes"]
            with CheckpointSession(
                policy_store,
                weftfs_cli=args.weftfs_cli,
                staging_bytes=tg.STAGING_BYTES,
                session_idle_secs=args.session_idle_secs,
            ) as after:
                entry["post_transcode_restore"] = restore_latency(after, state)
        policy_rows[label] = entry

    verify = (
        verify_checkpoint(store, "ckpt-%d" % (args.reps - 1), weftfs_cli=args.weftfs_cli)
        if sequential_rows
        else None
    )
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
            "fresh_store_median_ms": (
                statistics.median(row["wall_ms"] for row in fresh_rows)
                if fresh_rows
                else None
            ),
            "sequential_wall_ms": [
                round(row["wall_ms"], 3) for row in sequential_rows
            ],
            "sequential_first_ms": (
                sequential_rows[0]["wall_ms"] if sequential_rows else None
            ),
            "sequential_later_median_ms": (
                statistics.median(row["wall_ms"] for row in later_rows)
                if later_rows
                else None
            ),
            "sequential_all_median_ms": (
                statistics.median(row["wall_ms"] for row in sequential_rows)
                if sequential_rows
                else None
            ),
            "session_wall_ms": [round(row["wall_ms"], 3) for row in session_rows],
            "session_first_ms": (session_rows[0]["wall_ms"] if session_rows else None),
            "session_median_ms": (
                statistics.median(row["wall_ms"] for row in session_rows)
                if session_rows
                else None
            ),
            "two_commit_wall_ms": [
                round(row["wall_ms"], 3) for row in two_commit_rows
            ],
            "two_commit_median_ms": (
                statistics.median(row["wall_ms"] for row in two_commit_rows)
                if two_commit_rows
                else None
            ),
            "session_services_started": (
                session_rows[0]["session_stats"]["services_started"]
                if session_rows
                else None
            ),
            "packed": {
                label: {
                    "wall_ms": [
                        round(r["wall_ms"], 3)
                        for r in packed_rows
                        if r["scenario"] == label
                    ],
                    "median_ms": statistics.median(
                        r["wall_ms"] for r in packed_rows if r["scenario"] == label
                    ),
                    "files": [
                        r["store_delta_files"]
                        for r in packed_rows
                        if r["scenario"] == label
                    ][-1],
                    "bytes": [
                        r["store_delta_bytes"]
                        for r in packed_rows
                        if r["scenario"] == label
                    ][-1],
                    "fsyncs": [
                        (r.get("finish") or {}).get("backend_fsyncs")
                        for r in packed_rows
                        if r["scenario"] == label
                    ][-1],
                    "histogram": [
                        r.get("frame_histogram")
                        for r in packed_rows
                        if r["scenario"] == label
                    ][-1],
                    "restore": [
                        r.get("restore")
                        for r in packed_rows
                        if r["scenario"] == label
                    ][-1],
                }
                for label in sorted({r["scenario"] for r in packed_rows})
            },
            "fresh_files": fresh_rows[0]["store_delta_files"] if fresh_rows else None,
            "fresh_bytes": fresh_rows[0]["store_delta_bytes"] if fresh_rows else None,
            "fresh_fsyncs": (
                (fresh_rows[0].get("finish") or {}).get("backend_fsyncs")
                if fresh_rows
                else None
            ),
            "fresh_histogram": (
                fresh_rows[0].get("frame_histogram") if fresh_rows else None
            ),
            "unpacked_files": [
                r["store_delta_files"] for r in session_rows
            ][-1] if session_rows else None,
            "unpacked_bytes": [
                r["store_delta_bytes"] for r in session_rows
            ][-1] if session_rows else None,
            "unpacked_fsyncs": (
                (session_rows[-1].get("finish") or {}).get("backend_fsyncs")
                if session_rows
                else None
            ),
            "unpacked_histogram": (
                session_rows[-1].get("frame_histogram") if session_rows else None
            ),
            "unpacked_restore": (
                session_rows[-1].get("restore") if session_rows else None
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
            "session_two_commit": (
                decompose(two_commit_rows) if two_commit_rows else {}
            ),
            **{
                label: decompose([r for r in packed_rows if r["scenario"] == label])
                for label in sorted({r["scenario"] for r in packed_rows})
            },
        },
        "policy": policy_rows,
        "runs": all_rows + two_commit_rows + session_rows + packed_rows,
        "verify": verify,
        "session_verify": session_verify,
        "peak_rss_kib": int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss),
    }
    if args.receipt:
        write_json(args.receipt, receipt)
        print("checkpoint-latency receipt: %s" % args.receipt)
    print(
        "fresh median %s ms | writer-per-save first %s / later %s | "
        "session first %s / median %s | policies %s"
        % (
            receipt["summary"]["fresh_store_median_ms"],
            receipt["summary"]["sequential_first_ms"],
            receipt["summary"]["sequential_later_median_ms"],
            receipt["summary"]["session_first_ms"],
            receipt["summary"]["session_median_ms"],
            {
                label: round(entry["median_ms"], 1)
                for label, entry in receipt["policy"].items()
            },
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
        "--policy",
        action="append",
        default=None,
        metavar="LABEL",
        help="add a write-policy variant: planned|fast-raw|fast-zstd1|fast-deferred-zstd1",
    )
    run_p.add_argument(
        "--pack",
        action="append",
        default=None,
        metavar="TARGET:MEMBER",
        help="add a packed variant (target bytes:member-threshold bytes)",
    )
    run_p.add_argument(
        "--weftfs-cli",
        default=os.environ.get("WEFTFS_CLI", "/work/.docker-cache/target-rust/release/weftfs"),
    )
    args = parser.parse_args(argv)
    policies = {
        "planned": {},
        "fast-raw": {"write_policy": "fast", "fast_codec": "raw"},
        "fast-zstd1": {
            "write_policy": "fast",
            "fast_codec": "zstd",
            "fast_zstd_level": 1,
        },
        "fast-deferred-zstd1": {
            "write_policy": "fast",
            "fast_codec": "zstd",
            "fast_zstd_level": 1,
        },
        "staged-container": {"direct_container": False},
        "direct-container": {"direct_container": True},
    }
    args.policy = [(label, policies[label]) for label in (args.policy or [])]
    if getattr(args, "pack", None) is None:
        args.pack = []
    else:
        args.pack = [tuple(int(part) for part in spec.split(":")) for spec in args.pack]
    if args.command == "run":
        return run(args)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
