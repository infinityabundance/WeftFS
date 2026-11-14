#!/usr/bin/env python3
"""T057 court: asynchronous checkpoint saves overlapping a sustained loop.

Three arms on the same model, data and step count:

* ``none``  — no checkpoint (the baseline step time);
* ``async`` — one ``save_checkpoint_async`` per checkpoint, unbounded
  outstanding saves (the shipped per-save path);
* ``queue`` — one ``CheckpointSession`` with a bounded queue
  (``max_outstanding`` and ``max_staged_bytes``) and non-blocking submission.

Reported per arm: step-time p50/p95/p99 and inflation, submit pause, maximum
outstanding saves, queue high water, admission refusals/waits, drain time,
staged-byte high water, process peak RSS and CPU time. The model is trained
through the same deterministic fixture the training-goodput court uses, so the
final parameter hash must match the no-checkpoint arm exactly.

CPU only, single host. **GPU idle time is unmeasured**; nothing here is a GPU
overlap claim.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import resource
import statistics
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "weft.async-save-overlap.v1"
DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"
COURT = Path(__file__).resolve().parent / "training_goodput.py"


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_court():
    spec = importlib.util.spec_from_file_location("weft_training_goodput", COURT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def rss_kib() -> int:
    try:
        with open("/proc/self/status", "r", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return 0


def peak_rss_kib() -> int:
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return int(usage.ru_maxrss)


def cpu_ms() -> float:
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return (usage.ru_utime + usage.ru_stime) * 1000.0


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(round(fraction * (len(ordered) - 1)))))
    return ordered[index]


def summarise(values):
    return {
        "p50_ms": round(percentile(values, 0.50), 3),
        "p95_ms": round(percentile(values, 0.95), 3),
        "p99_ms": round(percentile(values, 0.99), 3),
        "max_ms": round(max(values), 3),
        "n": len(values),
    }


from concurrent.futures import ThreadPoolExecutor


def probe_unsynchronized(cli, work):
    """Two concurrent `save_checkpoint_async` calls on one store.

    The store is single-writer, so this is expected to refuse one of them with
    `Busy`. It is recorded as a limit of the per-call API, not as a training
    measurement: a caller that wants overlap must serialise publications (one
    worker or the bounded session).
    """
    from weftcodec.dcp import init_store, save_checkpoint_async
    import torch

    root = work / "unsync-store"
    if root.exists():
        import shutil

        shutil.rmtree(root)
    init_store(root, weftfs_cli=cli)
    torch.manual_seed(11)
    handles = [
        save_checkpoint_async(
            {"t": torch.arange(16, dtype=torch.float32) + offset},
            checkpoint_id="unsync-%d" % offset,
            store_root=root,
            topology=1,
            weftfs_cli=cli,
            staging_bytes=1 << 20,
        )
        for offset in (0, 1)
    ]
    outcomes = []
    for handle in handles:
        try:
            handle.result(timeout=120)
            outcomes.append("published")
        except BaseException as exc:  # noqa: BLE001 - torch wraps in BaseException
            outcomes.append(type(exc).__name__ + ": " + str(exc)[:160])
    return {
        "description": "two overlapping per-call async saves on one single-writer store",
        "outcomes": outcomes,
        "at_least_one_refused": any(item != "published" for item in outcomes),
    }


def run_arm(court, torch, args, arm, store_root):
    from weftcodec.dcp import CheckpointSession, init_store, save_checkpoint_async

    init_store(store_root, weftfs_cli=args.cli)
    torch.manual_seed(court.SEED)
    model = court.build_model(torch)
    optimizer = court.make_optimizer(torch, model)
    batches = court.data_batches(torch)

    session = None
    handles = []
    tickets = []
    names = []
    executor = ThreadPoolExecutor(max_workers=1) if arm == "async" else None
    if arm == "queue":
        session = CheckpointSession(
            store_root,
            weftfs_cli=args.cli,
            staging_bytes=64 << 20,
            session_idle_secs=30,
            max_outstanding=args.queue_outstanding,
            max_staged_bytes=args.queue_staged_bytes,
            admission_policy="block",
            admission_wait_ms=args.admission_wait_ms,
        )

    step_times = []
    pauses = []
    outstanding_samples = []
    rss_samples = []
    submitted = 0
    submit_errors = 0
    started = time.perf_counter()
    try:
        for step in range(1, args.steps + 1):
            step_started = time.perf_counter()
            court.train_step(torch, model, optimizer, batches, step)
            step_ms = (time.perf_counter() - step_started) * 1000.0
            step_times.append(step_ms)
            if arm != "none" and step % args.every == 0:
                snapshot = court.capture_state(torch, model, optimizer, step)
                checkpoint_id = "overlap-%s-%d" % (arm, step)
                names.append(checkpoint_id)
                submit_started = time.perf_counter()
                try:
                    if arm == "async":
                        # One publication worker: the store is single-writer, so
                        # overlap must serialise publishes, not writers. The
                        # shipped per-call API returns a handle immediately; the
                        # worker waits for it so exactly one publish is active.
                        def _save_one(snapshot=snapshot, checkpoint_id=checkpoint_id):
                            return save_checkpoint_async(
                                snapshot,
                                checkpoint_id=checkpoint_id,
                                store_root=store_root,
                                topology=1,
                                weftfs_cli=args.cli,
                                staging_bytes=64 << 20,
                            ).result(timeout=args.drain_timeout_secs)

                        handles.append(executor.submit(_save_one))
                    else:
                        tickets.append(
                            session.submit(snapshot, checkpoint_id=checkpoint_id)
                        )
                    submitted += 1
                except Exception:  # noqa: BLE001 - a refusal is a measurement
                    submit_errors += 1
                pauses.append((time.perf_counter() - submit_started) * 1000.0)
                pending = sum(1 for handle in handles if not handle.done()) if arm == "async" else 0
                if arm == "queue":
                    pending = session.stats["queue_high_water"]
                outstanding_samples.append(pending)
            rss_samples.append(rss_kib())
        loop_wall_ms = (time.perf_counter() - started) * 1000.0
    finally:
        drain_started = time.perf_counter()
        if arm == "queue":
            session.close(wait=True, timeout=args.drain_timeout_secs)
            drain_report = session.drain_report()
        else:
            drain_report = None
        for handle in handles:
            handle.result(timeout=args.drain_timeout_secs)
        if executor is not None:
            executor.shutdown(wait=True)
        drain_ms = (time.perf_counter() - drain_started) * 1000.0

    if arm != "none":
        # The final published checkpoint must still verify exactly.
        import subprocess

        verified = []
        for name in names[-1:]:
            proc = subprocess.run(
                [args.cli, "--store", str(store_root), "--json", "checkpoint", "verify", "--name", name],
                capture_output=True,
                text=True,
                check=False,
            )
            verified.append(proc.returncode == 0)
        verification_ok = all(verified) if verified else True
    else:
        verification_ok = True
    final_state = court.capture_state(torch, model, optimizer, args.steps)
    parameters = court.flat_sha256(torch, final_state)[0]

    queue_stats = dict(session.stats) if session is not None else None
    return {
        "arm": arm,
        "steps": args.steps,
        "checkpoint_every": args.every,
        "submitted": submitted,
        "submit_errors": submit_errors,
        "step": summarise(step_times),
        "submit_pause": summarise(pauses) if pauses else None,
        "max_outstanding_sampled": max(outstanding_samples) if outstanding_samples else 0,
        "async_worker_threads": 1 if arm == "async" else 0,
        "queue_stats": queue_stats,
        "drain_report": drain_report,
        "drain_ms": round(drain_ms, 3),
        "loop_wall_ms": round(loop_wall_ms, 3),
        "peak_rss_kib": peak_rss_kib(),
        "rss_high_water_kib": max(rss_samples) if rss_samples else 0,
        "cpu_ms": round(cpu_ms(), 3),
        "checkpoints": names,
        "checkpoints_verified": verification_ok,
        "final_model_sha256": parameters,
    }


def run(args):
    court = load_court()
    torch = court.load_torch()
    work = Path(args.work_dir)
    work.mkdir(parents=True, exist_ok=True)

    cells = []
    for rep in range(args.reps):
        for arm in ("none", "async", "queue"):
            store = work / ("%s-%d" % (arm, rep))
            if store.exists():
                import shutil

                shutil.rmtree(store)
            cell = run_arm(court, torch, args, arm, store)
            cell["rep"] = rep
            cells.append(cell)

    def pick(arm, field):
        return [
            cell[field]
            for cell in cells
            if cell["arm"] == arm
        ]

    def medians(arm):
        steps = [cell["step"]["p50_ms"] for cell in cells if cell["arm"] == arm]
        p95 = [cell["step"]["p95_ms"] for cell in cells if cell["arm"] == arm]
        return {
            "step_p50_ms": round(statistics.median(steps), 3),
            "step_p95_ms": round(statistics.median(p95), 3),
        }

    baseline = medians("none")
    arms = {}
    for arm in ("none", "async", "queue"):
        summary = medians(arm)
        summary["step_inflation_p50"] = round(
            summary["step_p50_ms"] / baseline["step_p50_ms"], 4
        )
        summary["step_inflation_p95"] = round(
            summary["step_p95_ms"] / baseline["step_p95_ms"], 4
        )
        summary["peak_rss_kib"] = max(
            cell["peak_rss_kib"] for cell in cells if cell["arm"] == arm
        )
        summary["rss_high_water_kib"] = max(
            cell["rss_high_water_kib"] for cell in cells if cell["arm"] == arm
        )
        summary["cpu_ms"] = max(cell["cpu_ms"] for cell in cells if cell["arm"] == arm)
        summary["drain_ms"] = max(
            cell["drain_ms"] for cell in cells if cell["arm"] == arm
        )
        summary["max_outstanding_sampled"] = max(
            cell["max_outstanding_sampled"]
            for cell in cells
            if cell["arm"] == arm
        )
        summary["submitted"] = sum(
            cell["submitted"] for cell in cells if cell["arm"] == arm
        )
        summary["submit_errors"] = sum(
            cell["submit_errors"] for cell in cells if cell["arm"] == arm
        )
        summary["checkpoints_verified"] = all(
            cell["checkpoints_verified"]
            for cell in cells
            if cell["arm"] == arm
        )
        summary["final_model_sha256"] = sorted(
            {cell["final_model_sha256"] for cell in cells if cell["arm"] == arm}
        )
        queue_stats = [
            cell["queue_stats"]
            for cell in cells
            if cell["arm"] == arm and cell["queue_stats"]
        ]
        if queue_stats:
            summary["queue"] = {
                key: max(int(stats[key]) for stats in queue_stats)
                for key in (
                    "queue_high_water",
                    "staged_bytes_high_water",
                    "admission_refusals",
                    "admission_waits",
                    "largest_admission_bytes",
                )
            }
            summary["queue"]["admission_wait_ms_max"] = max(
                float(stats["admission_wait_ms_max"]) for stats in queue_stats
            )
            summary["queue"]["max_outstanding_bound"] = args.queue_outstanding
            summary["queue"]["staged_bytes_bound"] = args.queue_staged_bytes
        arms[arm] = summary

    exact = arms["none"]["final_model_sha256"] == arms["async"]["final_model_sha256"] == arms["queue"]["final_model_sha256"]
    unsynchronized = probe_unsynchronized(args.cli, work)

    checks = {
        "same_trajectory_all_arms": exact,
        "checkpoints_verify": arms["async"]["checkpoints_verified"]
        and arms["queue"]["checkpoints_verified"],
        "queue_bound_respected": arms["queue"].get("queue", {}).get(
            "queue_high_water", 0
        )
        <= args.queue_outstanding,
        "staging_bound_respected": arms["queue"].get("queue", {}).get(
            "staged_bytes_high_water", 0
        )
        <= args.queue_staged_bytes,
        "no_unbounded_rss_growth": all(
            arm["rss_high_water_kib"] < (2 << 20) for arm in arms.values()
        ),
        "single_writer_collision_observed": unsynchronized["at_least_one_refused"],
    }

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
            "gpu_idle_unmeasured": True,
            "note": "CPU-side stall only; GPU idle time is not measured and is not claimed",
        },
        "revision": {
            "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        },
        "args": {
            "steps": args.steps,
            "checkpoint_every": args.every,
            "reps": args.reps,
            "queue_outstanding": args.queue_outstanding,
            "queue_staged_bytes": args.queue_staged_bytes,
            "admission_wait_ms": args.admission_wait_ms,
        },
        "arms": arms,
        "unsynchronized_probe": unsynchronized,
        "raw_cells": cells,
        "checks": checks,
        "losses": [
            "The baseline step is CPU-bound and only ~10 ms; a shorter step would make the same save look larger.",
            "The async arm's outstanding-save count is sampled after each step, not continuously.",
            "RSS is the process high-water mark; the kernel may not return freed pages during a run.",
            "Two per-call async saves on one store collide: the store is single-writer (see unsynchronized_probe).",
            "No GPU exists in this court, so GPU idle overlap is unmeasured.",
        ],
        "result": "pass" if all(checks.values()) else "fail",
    }
    Path(args.receipt).parent.mkdir(parents=True, exist_ok=True)
    Path(args.receipt).write_text(json.dumps(receipt, indent=2))
    print(json.dumps({"result": receipt["result"], "checks": checks, "arms": arms}, indent=2))
    return 0 if receipt["result"] == "pass" else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run_parser = sub.add_parser("run")
    run_parser.add_argument("--receipt", required=True)
    run_parser.add_argument(
        "--stamp", default=datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    )
    run_parser.add_argument("--work-dir", default="/tmp/weft-async-overlap")
    run_parser.add_argument("--cli", default=DEFAULT_CLI)
    run_parser.add_argument("--steps", type=int, default=160)
    run_parser.add_argument("--every", type=int, default=16)
    run_parser.add_argument("--reps", type=int, default=3)
    run_parser.add_argument("--queue-outstanding", type=int, default=2)
    run_parser.add_argument("--queue-staged-bytes", type=int, default=1 << 30)
    run_parser.add_argument("--admission-wait-ms", type=float, default=30000.0)
    run_parser.add_argument("--drain-timeout-secs", type=float, default=300.0)
    args = parser.parse_args(argv)
    if args.command == "run":
        return run(args)
    return 2


if __name__ == "__main__":
    sys.exit(main())
