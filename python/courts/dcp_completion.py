#!/usr/bin/env python3
"""DCP checkpoint completion: interleaved encoder arms and the full split.

The R12c court measured 34.6 ms p50 session completion after the metadata plan
shipped. This court answers the next question directly: with the shipped
adapter, is a 20 ms completion reachable, and if not, which term owns the rest?

It runs the **same** R11 fixture as ``checkpoint_latency.py`` through one
``CheckpointSession`` (one service, one store open, one kept writable handle),
then
alternates the two metadata-encoder arms inside one process, save by save:

  * ``reference`` — the oracle encoder, forced;
  * ``auto`` — the shipped plan (the arm under test).

Interleaving matters because the two arms are not the same work; alternating
them per repetition keeps drift (page cache, host load) from being attributed
to an arm.

The durability control is separate from the latency numbers: after a save
returns and the client acknowledges it, the helper process is killed with
SIGKILL and the checkpoint is re-opened and verified by a **new** ``weftfs``
process. A latency win that breaks that property is not a win, so it is checked
on every run.

Run in the python court:

  scripts/docker-court.sh python \
    "cd /work && PYTHONPATH=/work/python python3 python/courts/dcp_completion.py \
       --receipt bench/receipts/dcp-latency-<stamp>.json --reps 7"
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import statistics
import sys
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

import training_goodput as tg  # noqa: E402

from checkpoint_latency import (  # noqa: E402
    Collector,
    decompose,
    fixture,
)
from weftcodec.dcp import DEFAULT_CLI, init_store, verify_checkpoint  # noqa: E402


def utc_stamp():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def percentile(values, fraction):
    ordered = sorted(values)
    if not ordered:
        return None
    index = round((len(ordered) - 1) * fraction)
    return ordered[min(index, len(ordered) - 1)]


def arm_rows(rows, arm):
    return [row for row in rows if row["arm"] == arm]


def arm_summary(rows):
    save_ms = [row["save_ms"] for row in rows]
    return {
        "reps": len(rows),
        "completion_p50_ms": statistics.median(save_ms),
        "completion_p95_ms": percentile(save_ms, 0.95),
        "completion_samples_ms": save_ms,
    }


def run(args):
    torch = tg.load_torch()
    import torch.distributed.checkpoint  # noqa: F401 - fail early

    state = fixture(torch)
    collector = Collector()
    work = Path(args.work_dir)
    if work.exists():
        import shutil

        shutil.rmtree(work)
    work.mkdir(parents=True)

    store = work / "store"
    store.mkdir()
    init_store(store, weftfs_cli=args.weftfs_cli)

    from weftcodec.dcp import CheckpointSession, set_probe_sink

    session = CheckpointSession(
        store,
        weftfs_cli=args.weftfs_cli,
        staging_bytes=tg.STAGING_BYTES,
        session_idle_secs=30,
    )

    def save_once(name):
        """One synchronous save through the shared session, fully instrumented."""
        import torch.distributed.checkpoint as dcp

        collector.reset()
        set_probe_sink(collector)
        service = session.service()
        profile_available = True
        try:
            service.profile(0)
        except Exception:  # noqa: BLE001 - protocol < 3 has no profile
            profile_available = False
        started = time.perf_counter()
        writer = session.writer(name)
        dcp.save(state, storage_writer=writer, no_dist=True)
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        rust = {}
        if profile_available:
            rust = service.profile(1)
            try:
                service.profile(2)
            except Exception:  # noqa: BLE001
                pass
        client = collector.summary()
        stats = dict(writer.stats)
        set_probe_sink(None)
        return {
            "save_ms": elapsed_ms,
            "client": client,
            "rust": rust,
            "metadata_encode": stats.get("metadata_encode"),
            "store_delta_bytes": None,
        }

    rows = []
    # Warm both arms once so the plan build and first-touch costs are not
    # attributed to a repetition.
    for arm, encoding in (("auto", "auto"), ("reference", "reference")):
        os.environ["WEFT_DCP_METADATA_ENCODING"] = encoding
        save_once("warm-%s" % arm)

    for rep in range(args.reps):
        for arm, encoding in (("reference", "reference"), ("auto", "auto")):
            os.environ["WEFT_DCP_METADATA_ENCODING"] = encoding
            row = save_once("rep%02d-%s" % (rep, arm))
            row["arm"] = arm
            row["rep"] = rep
            rows.append(row)

    # Durability control: ack, SIGKILL the session helper, verify from a new
    # process. The session's writable handle is what a crash would lose.
    os.environ["WEFT_DCP_METADATA_ENCODING"] = "auto"
    control_name = "durability-control"
    save_once(control_name)
    service = session.service()
    process = getattr(service, "_process", None)
    pid = getattr(process, "pid", None)
    killed = False
    if pid:
        try:
            os.kill(pid, signal.SIGKILL)
            killed = True
        except ProcessLookupError:
            killed = False
    close_error = None
    try:
        session.close(wait=True)
    except Exception as exc:  # noqa: BLE001 - the helper is dead on purpose
        close_error = str(exc)
    verified = verify_checkpoint(
        store, control_name, full=False, weftfs_cli=args.weftfs_cli
    )

    summaries = {arm: arm_summary(arm_rows(rows, arm)) for arm in ("auto", "reference")}
    splits = {}
    for arm in ("auto", "reference"):
        arm_only = []
        for row in arm_rows(rows, arm):
            row = dict(row)
            row["wall_ms"] = row["save_ms"]
            arm_only.append(row)
        splits[arm] = decompose(arm_only)

    return {
        "schema": "weft.dcp-completion.v1",
        "stamp": args.stamp or utc_stamp(),
        "revision": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "reference_arm": "reference (oracle encoder, forced)",
        "measured_arm": "auto (shipped metadata plan)",
        "interleaved": True,
        "reps": args.reps,
        "fixture": {
            "entries": len(state),
            "logical_bytes": sum(
                int(value.numel() * value.element_size()) for value in state.values()
            ),
        },
        "summaries": summaries,
        "splits": splits,
        "delta_completion_p50_ms": (
            summaries["reference"]["completion_p50_ms"]
            - summaries["auto"]["completion_p50_ms"]
        ),
        "target_ms": 20.0,
        "target_reached": summaries["auto"]["completion_p50_ms"] < 20.0,
        "durability_control": {
            "acknowledged": True,
            "helper_killed": killed,
            "helper_pid": pid,
            "verified_from_new_process": verified,
            "close_error": close_error,
        },
        "rows": [
            {
                "arm": row["arm"],
                "rep": row["rep"],
                "save_ms": row["save_ms"],
                "client": row["client"],
                "rust": row["rust"],
                "metadata_encode": row.get("metadata_encode"),
                "store_delta_bytes": row["store_delta_bytes"],
            }
            for row in rows
        ],
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--receipt", required=True)
    parser.add_argument("--stamp", default=None)
    parser.add_argument("--reps", type=int, default=7)
    parser.add_argument("--work-dir", default="/tmp/weft-dcp-completion")
    parser.add_argument(
        "--weftfs-cli", default=os.environ.get("WEFTFS_CLI", DEFAULT_CLI)
    )
    args = parser.parse_args(argv)

    receipt = run(args)
    path = Path(args.receipt)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(receipt, indent=1, sort_keys=True) + "\n")
    log = path.with_suffix(".log")
    auto = receipt["summaries"]["auto"]
    ref = receipt["summaries"]["reference"]
    lines = [
        "stamp=%s reps=%d fixture_entries=%d"
        % (receipt["stamp"], receipt["reps"], receipt["fixture"]["entries"]),
        "auto      completion p50=%.3f p95=%.3f ms"
        % (auto["completion_p50_ms"], auto["completion_p95_ms"]),
        "reference completion p50=%.3f p95=%.3f ms"
        % (ref["completion_p50_ms"], ref["completion_p95_ms"]),
        "delta p50=%.3f ms target=%.1f reached=%s"
        % (
            receipt["delta_completion_p50_ms"],
            receipt["target_ms"],
            receipt["target_reached"],
        ),
        "durability control=%s" % json.dumps(receipt["durability_control"], sort_keys=True),
    ]
    log.write_text("\n".join(lines) + "\n" + json.dumps(receipt, indent=1, sort_keys=True) + "\n")
    print("\n".join(lines))
    print("receipt: %s" % path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
