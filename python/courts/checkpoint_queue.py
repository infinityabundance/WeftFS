"""Checkpoint queue court (R12c item 4).

Measures what happens when a training loop requests durable checkpoints faster
than they complete, and what the bounded queue changes.

Three sections, each with its own command so the *baseline* can be measured at
a revision that has no queue at all:

``sweep``
    The interval sweep: for each checkpoint interval (1, 2, 4, 8, 16, 32 steps)
    and each configuration, run the identical training loop and report step,
    pause, completion, admission wait, loop wall, RSS and CPU, plus the exact
    restore check of every checkpoint. The loop, fixture, checkpoint names and
    verification are the R11/R12b court's own code (`training_goodput.py`), so
    the fixture is the same 169-entry ~5 MB container by construction.

``overload``
    What a caller that does **not** wait for the previous save gets today: N
    concurrent submissions through the shipped async API and through the
    session's synchronous `save`, with the outcome of each counted by class.
    This is the "does it block, raise, or lose a save?" measurement, and it is
    deliberately run before the queue exists.

``bound``
    The queue-bound sweep: the same interval sweep at several
    ``max_outstanding`` values, so the default bound is chosen from
    measurement rather than taste.

Every section writes one receipt; the caller passes ``--receipt``. Nothing here
is a claim: a number exists only where the receipt records the command, court,
fixture and revision.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

import training_goodput as tg  # noqa: E402

SCHEMA_RECEIPT = "weft.checkpoint-queue.v1"


def stamp():
    return time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())


def write_json(path, payload):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return path


def court_block(args):
    return {
        "image": os.environ.get("WEFT_COURT_IMAGE", ""),
        "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", ""),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", ""),
        "memory": os.environ.get("WEFT_COURT_MEMORY", ""),
        "cpus": os.environ.get("WEFT_COURT_CPUS", ""),
        "pids": os.environ.get("WEFT_COURT_PIDS", ""),
        "cap_drop": "ALL",
        "no_new_privileges": True,
        "tmpfs": "/tmp:rw,size=1g,nosuid,nodev",
        "docker_server": os.environ.get("WEFT_DOCKER_SERVER", ""),
        "python": sys.version.split()[0],
        "weftfs_cli": str(args.weftfs_cli),
        "revision_weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "revision_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        "revision_weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
        "revision_weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
    }


def fixture_block(torch):
    """Describe the fixture by building it, not by asserting a size."""
    model = tg.build_model(torch)
    optimizer = tg.make_optimizer(torch, model)
    batches = tg.data_batches(torch)
    tg.train_step(torch, model, optimizer, batches, 1)
    snapshot = tg.capture_state(torch, model, optimizer, 1)
    entries = tg.state_spec(torch, snapshot)
    logical = sum(
        int(value.numel() * value.element_size()) for value in snapshot.values()
    )
    return {
        "seed": tg.SEED,
        "entries": len(entries),
        "logical_bytes": logical,
        "largest_tensor_bytes": max(
            int(value.numel() * value.element_size()) for value in snapshot.values()
        ),
        "staging_bytes": tg.STAGING_BYTES,
        "container_note": (
            "the same 169-entry safetensors container the R11/R12b courts used; "
            "the receipt records the observed per-checkpoint bytes in every cell"
        ),
    }


def pooled_steps(results):
    """Clean training steps: not a checkpoint step, not a post-checkpoint probe."""
    values = []
    for result in results:
        checkpoints = {
            int(event["checkpoint_step"]) for event in tg.checkpoint_events(result)
        }
        augment_steps = {
            int(event["step"])
            for event in result["events"]
            if event.get("event") == "step" and event.get("augment_ms") is not None
        }
        values.extend(
            value
            for index, value in enumerate(result["step_ms"], start=1)
            if index not in checkpoints and index not in augment_steps
        )
    return values


def admission_waits(result):
    return [
        float(event["admission_wait_ms"])
        for event in tg.checkpoint_events(result)
        if event.get("admission_wait_ms") is not None
    ]


def _step_stats(values):
    if not values:
        return {"samples": 0, "p50_ms": None, "p95_ms": None, "p99_ms": None}
    return {
        "samples": len(values),
        "p50_ms": tg.round3(tg.median(values)),
        "p95_ms": tg.round3(tg.percentile(values, 0.95)),
        "p99_ms": tg.round3(tg.percentile(values, 0.99)),
        "max_ms": tg.round3(max(values)),
    }


def annotate_cell(cell):
    """Add the admission-wait distribution for a queue cell.

    Step statistics come from the harness itself (`step_ms_all` and
    `step_ms_clean`), because at a 1-step interval every step takes a checkpoint
    and the "clean step" view is empty; both views are labelled in the cell.
    """
    events = cell.get("checkpoint_events_rep0") or []
    waits = [
        float(event["admission_wait_ms"])
        for event in events
        if event.get("admission_wait_ms") is not None
    ]
    if waits:
        cell["admission_wait_ms"] = _step_stats(waits)
    depths = [
        int(event["queue_depth"])
        for event in events
        if event.get("queue_depth") is not None
    ]
    if depths:
        cell["queue_depth"] = {
            "samples": len(depths),
            "p50": int(tg.median(depths)),
            "max": max(depths),
        }
    cell["admission_wait_scope"] = "checkpoint submission events of rep 0"
    return cell


# ---------------------------------------------------------------------------
# sweep
# ---------------------------------------------------------------------------


def run_sweep(args):
    work = Path(args.work_dir)
    intervals = [int(item) for item in str(args.intervals).split(",") if item.strip()]
    configs = [item.strip() for item in str(args.configs).split(",") if item.strip()]
    unknown = [config for config in configs if config not in tg.ALL_CONFIGS]
    if unknown:
        raise SystemExit(
            "unknown config(s) %s at this revision (known: %s)"
            % (", ".join(unknown), ", ".join(tg.ALL_CONFIGS))
        )
    import torch

    fixture = fixture_block(torch)
    sweep = {
        "steps": args.steps,
        "reps": args.reps,
        "intervals": intervals,
        "configs": configs,
        "resume": args.resume,
        "queue": {
            "max_outstanding": args.queue_max_outstanding,
            "max_staged_bytes": args.queue_max_staged_bytes,
            "admission_policy": args.queue_admission_policy,
            "admission_wait_ms": args.queue_admission_wait_ms,
        },
        "cells": {},
    }
    for interval in intervals:
        sweep["cells"][str(interval)] = {}
        for config in configs:
            print(
                "queue court sweep interval=%d config=%s" % (interval, config),
                flush=True,
            )
            cell = tg._sweep_cell(args, interval, config, work)
            annotate_cell(cell)
            sweep["cells"][str(interval)][config] = cell
            summary = {
                key: (cell.get("queue") or {}).get(key)
                for key in (
                    "requested",
                    "admitted",
                    "completed",
                    "failed",
                    "refused",
                    "queue_high_water",
                    "admission_wait_ms_max",
                )
            }
            print(
                "  wall=%s completion_p50=%s step_p50_all=%s queue=%s"
                % (
                    cell.get("loop_wall_ms_median"),
                    (cell.get("completion_warm") or {}).get("p50_ms"),
                    (cell.get("step_ms_all") or {}).get("p50_ms"),
                    json.dumps(summary),
                ),
                flush=True,
            )

    comparison = {}
    for interval in intervals:
        cells = sweep["cells"][str(interval)]
        blocking = cells.get("weft-session")
        queued = cells.get("weft-queue")
        if blocking and queued:
            comparison[str(interval)] = {
                "warm_p50_ratio_queue_over_blocking": tg._ratio(
                    (queued.get("completion_warm") or {}).get("p50_ms"),
                    (blocking.get("completion_warm") or {}).get("p50_ms"),
                ),
                "warm_p95_ratio_queue_over_blocking": tg._ratio(
                    (queued.get("completion_warm") or {}).get("p95_ms"),
                    (blocking.get("completion_warm") or {}).get("p95_ms"),
                ),
                "loop_wall_ratio_queue_over_blocking": tg._ratio(
                    queued.get("loop_wall_ms_median"),
                    blocking.get("loop_wall_ms_median"),
                ),
                "queue_no_silent_skips": any(
                    (row or {}).get("no_silent_skips")
                    for row in (queued.get("queue_all_runs") or [])
                    if row
                ),
            }
    sweep["comparison"] = comparison

    receipt = {
        "schema": SCHEMA_RECEIPT,
        "section": "sweep",
        "phase": args.phase,
        "stamp": args.stamp or stamp(),
        "generated_at_epoch": time.time(),
        "court": court_block(args),
        "fixture": fixture,
        "sweep": sweep,
        "command": " ".join(sys.argv),
    }
    write_json(args.receipt, receipt)
    print(json.dumps({"receipt": str(args.receipt), "section": "sweep"}), flush=True)
    shutil.rmtree(work, ignore_errors=True)
    return 0


# ---------------------------------------------------------------------------
# overload
# ---------------------------------------------------------------------------


def _store_checkpoints(cli, store):
    payload = tg.run_cli_json(cli, store, "checkpoint", "list")
    rows = payload.get("checkpoints", [])
    return [row["name"] for row in rows]


def _thread_count():
    try:
        return len(os.listdir("/proc/self/task"))
    except OSError:
        return None


def _race_submissions(args, *, mode, count, max_outstanding=None, policy="fail"):
    """Submit `count` saves concurrently and count exactly what each one did."""
    from weftcodec.dcp import CheckpointSession, DcpError, init_store

    torch = tg.load_torch()
    torch.manual_seed(tg.SEED)
    model = tg.build_model(torch)
    optimizer = tg.make_optimizer(torch, model)
    batches = tg.data_batches(torch)
    tg.train_step(torch, model, optimizer, batches, 1)
    snapshot = tg.capture_state(torch, model, optimizer, 1)

    work = Path(args.work_dir) / ("overload-%s" % mode)
    if work.exists():
        shutil.rmtree(work)
    store = work / "store"
    init_store(store, weftfs_cli=args.weftfs_cli)

    session_kwargs = {}
    if max_outstanding is not None:
        session_kwargs = {
            "max_outstanding": int(max_outstanding),
            "admission_policy": policy,
            "admission_wait_ms": float(args.queue_admission_wait_ms),
        }
    session = CheckpointSession(
        str(store),
        weftfs_cli=args.weftfs_cli,
        staging_bytes=tg.STAGING_BYTES,
        session_idle_secs=int(args.session_idle_secs),
        **session_kwargs,
    )

    baseline_threads = _thread_count()
    outcomes = []
    started = threading.Barrier(count)

    def one(index):
        name = "ckpt-%02d" % index
        row = {"index": index, "checkpoint_id": name, "outcome": None, "error_class": None}
        started.wait()
        try:
            if mode == "queue":
                ticket = session.submit(snapshot, checkpoint_id=name)
                row["admission_wait_ms"] = ticket.admission_wait_ms
                row["status"] = ticket.status
                ticket.result(timeout=600)
                row["outcome"] = "landed"
            elif mode == "session-save":
                session.save(snapshot, checkpoint_id=name)
                row["outcome"] = "landed"
            else:  # shipped async API: one executor and one writer per call
                from weftcodec.dcp import save_checkpoint_async

                handle = save_checkpoint_async(
                    snapshot,
                    checkpoint_id=name,
                    store_root=str(store),
                    staging_bytes=tg.STAGING_BYTES,
                    weftfs_cli=args.weftfs_cli,
                    session=session,
                )
                handle.result(timeout=600)
                row["outcome"] = "landed"
        except BaseException as exc:  # noqa: BLE001 - the class is the datum
            if isinstance(exc, (SystemExit, KeyboardInterrupt)):
                raise
            row["outcome"] = "raised"
            row["error_class"] = type(exc).__name__
            row["error"] = str(exc)[:600]
            row["dcp_error"] = isinstance(exc, DcpError)
        finally:
            outcomes.append(row)

    peak_threads = [baseline_threads or 0]

    def watch():
        for _ in range(400):
            current = _thread_count()
            if current is not None:
                peak_threads[0] = max(peak_threads[0], current)
            time.sleep(0.005)

    watcher = threading.Thread(target=watch, daemon=True)
    wall_started = time.perf_counter()
    watcher.start()
    threads = [threading.Thread(target=one, args=(index,)) for index in range(count)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=900)
    wall_ms = (time.perf_counter() - wall_started) * 1000.0
    watcher.join(timeout=1.0)

    drain = session.close()
    landed_reported = [row for row in outcomes if row["outcome"] == "landed"]
    raised = [row for row in outcomes if row["outcome"] == "raised"]
    names = _store_checkpoints(args.weftfs_cli, store)
    verify = None
    try:
        verify = tg.run_cli_json(args.weftfs_cli, store, "verify", "--full")
    except Exception as exc:  # noqa: BLE001 - a failing verify is the result
        verify = {"error": str(exc)[:400]}
    shutil.rmtree(work, ignore_errors=True)
    return {
        "mode": mode,
        "concurrent_submissions": count,
        "max_outstanding": max_outstanding,
        "admission_policy": policy if max_outstanding is not None else None,
        "wall_ms": round(wall_ms, 3),
        "outcomes": sorted(outcomes, key=lambda row: row["index"]),
        "landed": len(landed_reported),
        "raised": len(raised),
        "raised_classes": _count_classes(raised),
        "store_checkpoints": names,
        "store_checkpoint_count": len(names),
        "landed_equals_store": len(landed_reported) == len(names),
        "threads_before": baseline_threads,
        "threads_peak": peak_threads[0],
        "stats": dict(session.stats),
        "drain": {
            key: value for key, value in drain.items() if key != "tickets"
        },
        "verify_full": verify,
    }


def _count_classes(rows):
    counts = {}
    for row in rows:
        counts[row["error_class"]] = counts.get(row["error_class"], 0) + 1
    return counts


def run_overload(args):
    import torch

    fixture = fixture_block(torch)
    results = {"fixture": fixture, "runs": []}
    for count in args.counts:
        results["runs"].append(
            _race_submissions(args, mode="async-api", count=count)
        )
        results["runs"].append(
            _race_submissions(args, mode="session-save", count=count)
        )
        from weftcodec.dcp import CheckpointSession  # noqa: F401

        if hasattr(CheckpointSession, "submit"):
            results["runs"].append(
                _race_submissions(
                    args,
                    mode="queue",
                    count=count,
                    max_outstanding=args.queue_max_outstanding,
                    policy="fail",
                )
            )
            results["runs"].append(
                _race_submissions(
                    args,
                    mode="queue",
                    count=count,
                    max_outstanding=args.queue_max_outstanding,
                    policy="block",
                )
            )
    results["queue_api_present"] = hasattr(
        __import__("weftcodec.dcp", fromlist=["CheckpointSession"]).CheckpointSession,
        "submit",
    )
    receipt = {
        "schema": SCHEMA_RECEIPT,
        "section": "overload",
        "phase": args.phase,
        "stamp": args.stamp or stamp(),
        "generated_at_epoch": time.time(),
        "court": court_block(args),
        "fixture": fixture,
        "overload": results,
        "command": " ".join(sys.argv),
    }
    write_json(args.receipt, receipt)
    print(
        json.dumps(
            {
                "receipt": str(args.receipt),
                "section": "overload",
                "runs": [
                    {
                        "mode": row["mode"],
                        "count": row["concurrent_submissions"],
                        "landed": row["landed"],
                        "raised": row["raised"],
                        "classes": row["raised_classes"],
                    }
                    for row in results["runs"]
                ],
            }
        ),
        flush=True,
    )
    return 0


# ---------------------------------------------------------------------------
# bound
# ---------------------------------------------------------------------------


def run_bound(args):
    """Sweep `max_outstanding` at the intervals where the queue matters."""
    work = Path(args.work_dir)
    import torch

    fixture = fixture_block(torch)
    bounds = [int(item) for item in str(args.bounds).split(",") if item.strip()]
    intervals = [int(item) for item in str(args.intervals).split(",") if item.strip()]
    rows = []
    for bound in bounds:
        for interval in intervals:
            local = argparse.Namespace(**vars(args))
            local.queue_max_outstanding = bound
            local.checkpoint_every = interval
            print(
                "queue bound sweep bound=%d interval=%d" % (bound, interval),
                flush=True,
            )
            cell = tg._sweep_cell(local, interval, "weft-queue", work)
            rows.append(
                {
                    "max_outstanding": bound,
                    "interval_steps": interval,
                    "loop_wall_ms_median": cell.get("loop_wall_ms_median"),
                    "completion_warm": cell.get("completion_warm"),
                    "step_ms_p50_pooled": cell.get("step_ms_p50_pooled"),
                    "step_ms_p95_pooled": cell.get("step_ms_p95_pooled"),
                    "pause_ms_p50_pooled": cell.get("pause_ms_p50_pooled"),
                    "peak_rss_kib_max": cell.get("peak_rss_kib_max"),
                    "loop_cpu_ms_median": cell.get("loop_cpu_ms_median"),
                    "queue": cell.get("queue"),
                    "resume_checks": cell.get("resume_checks"),
                    "resume_exact": cell.get("resume_exact"),
                }
            )
    receipt = {
        "schema": SCHEMA_RECEIPT,
        "section": "bound",
        "phase": args.phase,
        "stamp": args.stamp or stamp(),
        "generated_at_epoch": time.time(),
        "court": court_block(args),
        "fixture": fixture,
        "bound_sweep": {
            "steps": args.steps,
            "reps": args.reps,
            "bounds": bounds,
            "intervals": intervals,
            "rows": rows,
        },
        "command": " ".join(sys.argv),
    }
    write_json(args.receipt, receipt)
    print(json.dumps({"receipt": str(args.receipt), "section": "bound"}), flush=True)
    shutil.rmtree(work, ignore_errors=True)
    return 0


# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    def common(parser, default_work):
        parser.add_argument("--receipt", required=True)
        parser.add_argument("--stamp", default=None)
        parser.add_argument("--phase", choices=("baseline", "after"), required=True)
        parser.add_argument("--work-dir", default=default_work)
        parser.add_argument("--steps", type=int, default=192)
        parser.add_argument("--reps", type=int, default=2)
        parser.add_argument("--resume", choices=("all", "final"), default="final")
        parser.add_argument("--kill-reps", type=int, default=1)
        parser.add_argument("--kill-steps", type=int, default=64)
        parser.add_argument("--kill-after-ack-step", type=int, default=48)
        parser.add_argument("--kill-before-ack-step", type=int, default=32)
        parser.add_argument(
            "--session-idle-secs", type=int, default=tg.DEFAULT_SESSION_IDLE_SECS
        )
        parser.add_argument(
            "--weftfs-cli", default=os.environ.get("WEFTFS_CLI", tg.DEFAULT_CLI)
        )
        parser.add_argument(
            "--queue-max-outstanding",
            type=int,
            default=getattr(tg, "DEFAULT_QUEUE_MAX_OUTSTANDING", 4),
        )
        parser.add_argument("--queue-max-staged-bytes", type=int, default=None)
        parser.add_argument(
            "--queue-admission-policy", choices=("block", "fail"), default="block"
        )
        parser.add_argument(
            "--queue-admission-wait-ms",
            type=float,
            default=getattr(tg, "DEFAULT_QUEUE_ADMISSION_WAIT_MS", 30000.0),
        )

    sweep = sub.add_parser("sweep", help="interval sweep over configurations")
    common(sweep, "/tmp/weft-checkpoint-queue-sweep")
    sweep.add_argument("--intervals", default="1,2,4,8,16,32")
    sweep.add_argument("--configs", default="weft-session,weft-queue")
    sweep.set_defaults(func=run_sweep)

    overload = sub.add_parser("overload", help="concurrent submission, no waiting")
    common(overload, "/tmp/weft-checkpoint-queue-overload")
    overload.add_argument("--counts", default="4,8")
    overload.set_defaults(func=run_overload)

    bound = sub.add_parser("bound", help="max_outstanding sweep")
    common(bound, "/tmp/weft-checkpoint-queue-bound")
    bound.add_argument("--bounds", default="1,2,4,8")
    bound.add_argument("--intervals", default="1,4")
    bound.set_defaults(func=run_bound)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
