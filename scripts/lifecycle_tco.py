#!/usr/bin/env python3
"""Stitch one controlled lifecycle from existing receipts, and build a
sensitivity surface from measured resource quantities and cited list prices.

Two things, deliberately separate:

T082 — lifecycle stitch. Every row names the receipt, the stamp and the exact
JSON path it was read from. Nothing is re-measured here and nothing is
estimated: a missing value, a stamp that does not match its filename, or an
untracked receipt fails the whole run. The rows come from different fixtures
and different runs, which is exactly why the receipt has a `cannot_prove`
section.

T089 — sensitivity, not an economic claim. The measured per-checkpoint
resource quantities (physical bytes, CPU seconds) from ONE interleaved
with/without court are multiplied by published list prices that name their
source and retrieval date. Where the quantity has no defensible price (request
counts for a POSIX backend, wall-clock seconds of a single-threaded process) it
is NOT PRICED, with the reason. The result is a break-even surface: at what
$/GiB-month and what $/vCPU-hour the codec's byte saving pays for its measured
CPU cost. No fleet, utilisation, discount or forecast assumption is made.

Usage:
  python3 scripts/lifecycle_tco.py                 # write the receipt
  python3 scripts/lifecycle_tco.py --stamp=...     # fixed stamp
  python3 scripts/lifecycle_tco.py --check         # validate only

Exit 0 on success; 1 on any stale, missing or untracked source.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime, timezone

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def die(message: str) -> "NoReturn":
    print(f"lifecycle_tco: {message}", file=sys.stderr)
    sys.exit(1)


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", "--no-optional-locks", *args],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        die(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def tracked(rel_path: str) -> bool:
    result = subprocess.run(
        ["git", "--no-optional-locks", "ls-files", "--error-unmatch", rel_path],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    return result.returncode == 0


def load(rel_path: str) -> dict:
    full = os.path.join(REPO_ROOT, rel_path)
    if not os.path.isfile(full):
        die(f"cited receipt {rel_path} does not exist")
    if not tracked(rel_path):
        die(f"cited receipt {rel_path} is not tracked by git; commit it before citing it")
    with open(full, "r", encoding="utf-8") as handle:
        return json.load(handle)


def dig(doc: object, path: str) -> object:
    current = doc
    for part in path.split("."):
        if isinstance(current, dict):
            if part not in current:
                raise KeyError(path)
            current = current[part]
        elif isinstance(current, list):
            current = current[int(part)]
        else:
            raise KeyError(path)
    return current


def stamp_of(rel_path: str, doc: dict, stamp_path: str | None) -> str:
    if stamp_path:
        value = dig(doc, stamp_path)
        return str(value)
    base = os.path.basename(rel_path)
    return base.split("-")[-1].removesuffix(".json")


def revision_of(rel_path: str, doc: dict, revision_path: str) -> str | None:
    try:
        return str(dig(doc, revision_path))
    except KeyError:
        return None


# ---------------------------------------------------------------------------
# T082: the rows. Each tuple is
#   (stage, quantity, unit, receipt, json path, stamp path, revision path, note)
# The note is where the fixture and the scope live, because these rows come
# from several courts and must never be read as one run.
# ---------------------------------------------------------------------------
WP = "bench/receipts/write-policy-20261114T104804Z.json"
WP_REV = "revisions.weftfs"
ASYNC = "bench/receipts/async-save-20261114T114340Z.json"
RESUME = "bench/receipts/training-resume-20261114T115016Z.json"
USEFUL = "bench/receipts/useful-step-20261114T115500Z.json"
RETENTION = "bench/receipts/retention-20261109T220237Z.json"

ROWS = [
    (
        "checkpoint write (planned)",
        "durable write pause (writer stall)",
        "ms",
        WP,
        "phases.base-planned-save.pause_ms",
        "stamp",
        WP_REV,
        "Qwen2.5-0.5B base, 290 tensors, 8 shards, 988,065,536 element bytes, shipped planned policy",
    ),
    (
        "checkpoint write (planned)",
        "encode CPU",
        "ms",
        WP,
        "phases.base-planned-save.cpu_user_ms",
        "stamp",
        WP_REV,
        "same cell as the planned pause; the receipt also reports cpu_sys_ms",
    ),
    (
        "checkpoint write (planned)",
        "stored bytes",
        "B",
        WP,
        "phases.base-planned-save.store_bytes",
        "stamp",
        WP_REV,
        "same cell; store_bytes is the reachable tree after the write",
    ),
    (
        "checkpoint write (fast, zstd-1)",
        "durable write pause (writer stall)",
        "ms",
        WP,
        "phases.base-fast-zstd1-save.pause_ms",
        "stamp",
        WP_REV,
        "same container and fixture as the planned row, Fast policy with FastCodec::Zstd level 1",
    ),
    (
        "checkpoint write (fast, zstd-1)",
        "encode CPU",
        "ms",
        WP,
        "phases.base-fast-zstd1-save.cpu_user_ms",
        "stamp",
        WP_REV,
        "same cell",
    ),
    (
        "checkpoint write (fast, zstd-1)",
        "stored bytes",
        "B",
        WP,
        "phases.base-fast-zstd1-save.store_bytes",
        "stamp",
        WP_REV,
        "same cell",
    ),
    (
        "checkpoint write (fast, raw)",
        "durable write pause (writer stall)",
        "ms",
        WP,
        "phases.crash-save.pause_ms",
        "stamp",
        WP_REV,
        "the crash-arm write, which uses Fast policy with FastCodec::Raw on the same fixture",
    ),
    (
        "checkpoint write (fast, raw)",
        "stored bytes",
        "B",
        WP,
        "phases.crash-save.store_bytes",
        "stamp",
        WP_REV,
        "same cell: raw storage of the same tensors",
    ),
    (
        "async save overlap",
        "training step p50 with no save in flight",
        "ms",
        ASYNC,
        "arms.none.step_p50_ms",
        "stamp",
        "revision.weftfs",
        "Python court, CPU only, small transformer, 160 steps, checkpoint every 16, 3 reps",
    ),
    (
        "async save overlap",
        "training step p50 with an async save budget",
        "ms",
        ASYNC,
        "arms.async.step_p50_ms",
        "stamp",
        "revision.weftfs",
        "same court; the measured step inflation is the async arm's own field",
    ),
    (
        "async save overlap",
        "step inflation versus no save",
        "ratio",
        ASYNC,
        "arms.async.step_inflation_p50",
        "stamp",
        "revision.weftfs",
        "same court; queue arm inflates further and is in the receipt",
    ),
    (
        "async save overlap",
        "saves submitted",
        "count",
        ASYNC,
        "arms.async.submitted",
        "stamp",
        "revision.weftfs",
        "same court; every submitted checkpoint verified",
    ),
    (
        "async save overlap",
        "drain time after the last step",
        "ms",
        ASYNC,
        "arms.async.drain_ms",
        "stamp",
        "revision.weftfs",
        "the deferred work is not free: it is paid at the end of the run",
    ),
    (
        "crash and resume",
        "steps lost between the last visible checkpoint and the kill",
        "count",
        RESUME,
        "kills.0.steps_lost",
        "stamp",
        "revision.weftfs",
        "Python court, kill at step 16, last visible checkpoint tip-step-000008",
    ),
    (
        "crash and resume",
        "kill to first resumed training step",
        "ms",
        RESUME,
        "kills.0.kill_to_first_step_ms",
        "stamp",
        "revision.weftfs",
        "same kill; the resumed state was compared bit for bit against the reference run",
    ),
    (
        "crash and resume",
        "resume wall (reopen + restore + 16 steps)",
        "ms",
        RESUME,
        "kills.0.resume_wall_ms",
        "stamp",
        "revision.weftfs",
        "same kill; resumed_checkpoint_valid is true in the receipt",
    ),
    (
        "crash and resume (store-level)",
        "reopen + verify after a process kill during publication",
        "ms",
        WP,
        "phases.crash-recover-verify.wall_ms",
        "stamp",
        WP_REV,
        "same checkpoint fixture; the kill is a process fault, not media loss",
    ),
    (
        "crash and resume (store-level)",
        "tensors verified after recovery",
        "count",
        WP,
        "phases.crash-recover-verify.tensors_verified",
        "stamp",
        WP_REV,
        "verify --full reports zero problems in the same cell",
    ),
    (
        "partial restore",
        "restore wall for the layers the next step needs",
        "ms",
        USEFUL,
        "summary.partial_restore_ms",
        "stamp",
        "revision.weftfs",
        "Python court; embedding + blocks.0 forward/loss/backward only, 14 of 42 tensors",
    ),
    (
        "partial restore",
        "restore wall for the whole checkpoint",
        "ms",
        USEFUL,
        "summary.full_restore_ms",
        "stamp",
        "revision.weftfs",
        "same court, same checkpoint, all 42 tensors",
    ),
    (
        "partial restore",
        "time to first useful step (partial)",
        "ms",
        USEFUL,
        "summary.partial_time_to_first_useful_step_ms",
        "stamp",
        "revision.weftfs",
        "measured, not derived: the court runs the step and records the wall",
    ),
    (
        "partial restore",
        "time to first useful step (full)",
        "ms",
        USEFUL,
        "summary.full_time_to_first_useful_step_ms",
        "stamp",
        "revision.weftfs",
        "same court",
    ),
    (
        "retention / GC",
        "safe hit rate, fixed TTL + LRU",
        "ratio",
        RETENTION,
        "policies.0.safe_hit_rate",
        None,
        "schema",
        "synthetic deterministic reuse trace, 3.0 MB capacity, 3,600 requests, fast rate",
    ),
    (
        "retention / GC",
        "safe hit rate, class-aware ordering",
        "ratio",
        RETENTION,
        "policies.2.safe_hit_rate",
        None,
        "schema",
        "same trace and capacity; unsafe_reclaims is 0 for this policy and 45 for fixed TTL",
    ),
    (
        "retention / GC",
        "physical bytes retained, fixed TTL + LRU",
        "B",
        RETENTION,
        "policies.0.physical_bytes",
        None,
        "schema",
        "same trace",
    ),
    (
        "retention / GC",
        "physical bytes retained, class-aware ordering",
        "B",
        RETENTION,
        "policies.2.physical_bytes",
        None,
        "schema",
        "same trace",
    ),
]


# ---------------------------------------------------------------------------
# T089: prices, with the verbatim string each one was read from.
# ---------------------------------------------------------------------------
PRICES = [
    {
        "id": "storage",
        "label": "object storage, Standard class",
        "unit": "USD per GiB-month",
        "value": 0.0200,
        "source": "Google Cloud Storage pricing page",
        "url": "https://cloud.google.com/storage/pricing",
        "retrieved_utc": "2026-11-15T01:20Z",
        "verbatim": "$0.000027397 / 1 gibibyte hour",
        "unit_conversion": "0.000027397 USD/GiB-hour x 730 h/month = 0.0200 USD/GiB-month",
        "status": "extracted from the page's rendered price table (column header 'Standard storage'); the conversion is shown",
        "band": [0.5, 1.0, 2.0],
    },
    {
        "id": "compute",
        "label": "serverless vCPU time (x86, on-demand)",
        "unit": "USD per vCPU-hour",
        "value": 0.0404784,
        "source": "AWS Fargate pricing page",
        "url": "https://aws.amazon.com/fargate/pricing/",
        "retrieved_utc": "2026-11-15T01:20Z",
        "verbatim": "$0.000011244 per vCPU second",
        "unit_conversion": "0.000011244 USD/vCPU-second x 3600 s/h = 0.0404784 USD/vCPU-hour",
        "status": "extracted verbatim from the page text; the page also lists a lower rate (0.0000089944/vCPU-second) for another configuration, which is not used",
        "band": [0.5, 1.0, 2.0],
    },
]

UNPRICED = [
    {
        "quantity": "request count",
        "why": "the with/without measurement is a POSIX court, so its 'requests' are file operations, not billable HTTP requests; pricing them at a per-1,000-request object-store rate would compare different units",
    },
    {
        "quantity": "wall-clock seconds",
        "why": "the measured wall time is a single process on 4 shared vCPUs; a wall second is not a billable unit and no $/second is claimed. CPU seconds are the priced proxy, and they are measured separately in the same receipt.",
    },
    {
        "quantity": "bytes transferred over a network",
        "why": "the with/without measurement is local; no egress or transfer quantity was measured, so none is priced",
    },
    {
        "quantity": "peak RSS",
        "why": "no memory-reservation price is cited; the difference is reported as a resource quantity only",
    },
]


def sensitivity() -> dict:
    """Build the break-even surface from the newest full local A/C court."""
    receipt = "bench/receipts/augmentation-local-20261114T195000Z.json"
    doc = load(receipt)
    if doc.get("result") != "pass":
        die(f"{receipt} is not a passing receipt")
    if doc["configuration"].get("label") != "full":
        die(f"{receipt} is not the full 101-tensor configuration")
    reps = len(doc["configurations"]["A"]["repetition_metrics"])
    if reps < 7:
        die(f"{receipt} has {reps} repetitions; the contract requires at least 7")

    def cell(name: str) -> dict:
        summary = doc["configurations"][name]["summary"]
        return {
            "footprint_bytes": int(summary["physical_footprint_bytes"]),
            "write_cpu_ms": float(summary["write_cpu_ms"]),
            "restore_cpu_ms": float(summary["restore_cpu_ms"]),
            "write_wall_ms": float(summary["write_wall_ms"]),
            "cold_restore_wall_ms": float(summary["cold_restore_wall_ms"]),
            "write_requests": int(summary["write_requests"]),
            "cold_read_requests": int(summary["cold_read_requests"]),
            "write_peak_rss_kib": int(summary["write_peak_rss_kib"]),
            "restore_peak_rss_kib": int(summary["restore_peak_rss_kib"]),
        }

    arm_a = cell("A")
    arm_c = cell("C")
    logical_bytes = int(doc["subset"]["logical_bytes"])

    gib = 2**30
    hours_per_month = 730.0
    storage = next(p for p in PRICES if p["id"] == "storage")["value"]
    compute = next(p for p in PRICES if p["id"] == "compute")["value"]

    checkpoints_per_day = 1
    retention_days_options = [1, 7, 30]
    restores_options = [0, 1, 10]

    def cost(arm: dict, retention_days: int, restores: int, p_storage: float, p_compute: float, billing: str) -> dict:
        # Steady state: one checkpoint per day, deleted after `retention_days`,
        # so `retention_days` checkpoints are retained on average, and the
        # monthly retained volume is retention_days x one checkpoint. This is an
        # assumption about the workload, published with the result, not a
        # measurement.
        retained_gib_months = arm["footprint_bytes"] / gib * retention_days
        if billing == "cpu_seconds":
            # What the process actually consumed (user+sys), as measured.
            billed_ms = arm["write_cpu_ms"] + restores * arm["restore_cpu_ms"]
        elif billing == "allocated_vcpu_seconds":
            # What a provider that bills a reserved vCPU would charge: the wall
            # time the process occupies its slice, including fsync waits.
            billed_ms = arm["write_wall_ms"] + restores * arm["cold_restore_wall_ms"]
        else:
            raise ValueError(billing)
        cpu_hours = billed_ms / 1000.0 / 3600.0
        storage_usd = p_storage * retained_gib_months
        compute_usd = p_compute * cpu_hours
        return {
            "retained_gib_months": retained_gib_months,
            "cpu_hours": cpu_hours,
            "storage_usd": storage_usd,
            "compute_usd": compute_usd,
            "total_usd": storage_usd + compute_usd,
        }

    surface = []
    for billing in ("cpu_seconds", "allocated_vcpu_seconds"):
        for retention_days in retention_days_options:
            for restores in restores_options:
                a = cost(arm_a, retention_days, restores, storage, compute, billing)
                c = cost(arm_c, retention_days, restores, storage, compute, billing)
                delta_storage = c["storage_usd"] - a["storage_usd"]
                delta_compute = c["compute_usd"] - a["compute_usd"]
                delta_total = c["total_usd"] - a["total_usd"]
                saved_gib_months = a["retained_gib_months"] - c["retained_gib_months"]
                extra_cpu_hours = c["cpu_hours"] - a["cpu_hours"]
                # C costs delta = -p_s*saved_gib_months + p_c*extra_cpu_hours. C is
                # cheaper when p_s > p_c*extra/saved. Both break-evens below are
                # these two ways of writing the same condition, each with the other
                # price held at its cited value.
                break_even_storage = (
                    (compute * extra_cpu_hours) / saved_gib_months if saved_gib_months > 0 else None
                )
                break_even_compute = (
                    (storage * saved_gib_months) / extra_cpu_hours if extra_cpu_hours > 0 else None
                )
                surface.append(
                    {
                        "billing": billing,
                        "retention_days": retention_days,
                        "restores_per_checkpoint": restores,
                        "arm": {
                            "A": a,
                            "C": c,
                        },
                        "delta": {
                            "storage_usd": delta_storage,
                            "compute_usd": delta_compute,
                            "total_usd": delta_total,
                            "cheaper_arm": "C" if delta_total < 0 else "A",
                            "saved_gib_months": saved_gib_months,
                            "extra_cpu_hours": extra_cpu_hours,
                            "extra_write_wall_ms": arm_c["write_wall_ms"] - arm_a["write_wall_ms"],
                            "extra_cold_restore_wall_ms": arm_c["cold_restore_wall_ms"] - arm_a["cold_restore_wall_ms"],
                        },
                        "break_even": {
                            "storage_usd_per_gib_month": break_even_storage,
                            "compute_usd_per_vcpu_hour": break_even_compute,
                            "reading": (
                                "C is cheaper than A when the storage price is above the first value "
                                "(compute held at the cited price) and when the compute price is below "
                                "the second value (storage held at the cited price); the cited prices "
                                "are the reference point, and the whole surface is in price_plane"
                            ),
                        },
                    }
                )

    # Price plane: multipliers around the cited prices, at the mid retention and
    # one restore per checkpoint, under both billing interpretations.
    plane = []
    multipliers = [0.5, 1.0, 2.0, 5.0, 10.0]
    for billing in ("cpu_seconds", "allocated_vcpu_seconds"):
        for ms in multipliers:
            for mc in multipliers:
                p_s = storage * ms
                p_c = compute * mc
                a = cost(arm_a, 7, 1, p_s, p_c, billing)
                c = cost(arm_c, 7, 1, p_s, p_c, billing)
                plane.append(
                    {
                        "billing": billing,
                        "storage_multiplier": ms,
                        "compute_multiplier": mc,
                        "storage_usd_per_gib_month": p_s,
                        "compute_usd_per_vcpu_hour": p_c,
                        "delta_usd": c["total_usd"] - a["total_usd"],
                        "cheaper_arm": "C" if c["total_usd"] < a["total_usd"] else "A",
                    }
                )

    surface_summary = {
        "cells": len(surface),
        "c_cheaper_cells": sum(1 for cell in surface if cell["delta"]["cheaper_arm"] == "C"),
        "a_cheaper_cells": sum(1 for cell in surface if cell["delta"]["cheaper_arm"] == "A"),
    }

    plane_summary = {}
    for billing in ("cpu_seconds", "allocated_vcpu_seconds"):
        cells = [c for c in plane if c["billing"] == billing]
        plane_summary[billing] = {
            "cells": len(cells),
            "c_cheaper_cells": sum(1 for c in cells if c["cheaper_arm"] == "C"),
            "a_cheaper_cells": sum(1 for c in cells if c["cheaper_arm"] == "A"),
            "a_wins_at": [
                [c["storage_multiplier"], c["compute_multiplier"]]
                for c in cells
                if c["cheaper_arm"] == "A"
            ],
        }

    return {
        "base_receipt": receipt,
        "base_stamp": doc.get("generated_utc"),
        "base_revision": dig(doc, "environment.revisions.WeftFS"),
        "repetitions": reps,
        "logical_bytes_per_checkpoint": logical_bytes,
        "arms": {"A": arm_a, "C": arm_c},
        "measured_delta": {
            "footprint_bytes_saved_per_checkpoint": arm_a["footprint_bytes"] - arm_c["footprint_bytes"],
            "footprint_reduction_pct": 100.0
            * (arm_a["footprint_bytes"] - arm_c["footprint_bytes"])
            / arm_a["footprint_bytes"],
            "extra_write_cpu_ms": arm_c["write_cpu_ms"] - arm_a["write_cpu_ms"],
            "extra_restore_cpu_ms": arm_c["restore_cpu_ms"] - arm_a["restore_cpu_ms"],
            "extra_write_wall_ms": arm_c["write_wall_ms"] - arm_a["write_wall_ms"],
            "extra_cold_restore_wall_ms": arm_c["cold_restore_wall_ms"] - arm_a["cold_restore_wall_ms"],
        },
        "model": {
            "checkpoints_per_day": checkpoints_per_day,
            "retention_days": retention_days_options,
            "restores_per_checkpoint": restores_options,
            "steady_state_copies": "retention_days (one per day, deleted after the window)",
            "gib": gib,
            "hours_per_month": hours_per_month,
            "cpu_priced": "measured write CPU once per checkpoint plus measured restore CPU once per restore",
            "billing_models": {
                "cpu_seconds": "only the CPU the process consumed (user+sys) is billed",
                "allocated_vcpu_seconds": "the wall time the process occupies its vCPU is billed, including fsync waits; this is what a provider that reserves a vCPU charges, and it is the less favourable model for the arm that waits",
            },
            "excluded": "fixture hashing, fixture verification and process start-up are outside the priced CPU, exactly as the contract excludes them from the measured phases",
            "what_this_is_not": "not a forecast, not an economic claim, no fleet, no utilisation, no discount, no reserved-instance, no egress and no request price",
        },
        "prices": PRICES,
        "unpriced": UNPRICED,
        "surface": surface,
        "surface_summary": surface_summary,
        "price_plane": plane,
        "plane_summary": plane_summary,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--stamp", default=None)
    parser.add_argument("--out", default=None)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    stamp = args.stamp or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_path = args.out or f"bench/receipts/lifecycle-tco-{stamp}.json"

    rows = []
    for stage, quantity, unit, receipt, path, stamp_path, revision_path, note in ROWS:
        doc = load(receipt)
        try:
            value = dig(doc, path)
        except (KeyError, IndexError, ValueError):
            die(f"{receipt} has no value at {path} (row {stage} / {quantity})")
        declared = stamp_of(receipt, doc, stamp_path)
        filename_stamp = os.path.basename(receipt).split("-")[-1].removesuffix(".json")
        if declared != filename_stamp:
            die(
                f"{receipt}: stamp field {stamp_path or 'filename'} is {declared!r} but the "
                f"filename says {filename_stamp!r}"
            )
        rows.append(
            {
                "stage": stage,
                "quantity": quantity,
                "unit": unit,
                "value": value,
                "evidence": {
                    "receipt": receipt,
                    "path": path,
                    "stamp": declared,
                    "revision": revision_of(receipt, doc, revision_path),
                    "revision_path": revision_path,
                },
                "scope_note": note,
            }
        )

    sensitivity_block = sensitivity()

    # Derived, name-keyed tables so the rendered documents can be generated by
    # scripts/perf_numbers.py from this receipt alone. They add no numbers; each
    # field is copied from a row or a surface cell above.
    lifecycle_table = {}
    for index, row in enumerate(rows, start=1):
        value = row["value"]
        unit = row["unit"]
        # Presentation only: the numeric value stays in lifecycle_rows above.
        if isinstance(value, bool):
            value_text = "yes" if value else "no"
        elif unit == "B":
            value_text = f"{int(value):,}"
        elif unit == "count":
            value_text = f"{int(value):,}"
        elif unit == "ratio":
            value_text = f"{float(value):.4f}"
        else:
            value_text = f"{float(value):,.1f}"
        lifecycle_table[f"row{index:02d}"] = {
            "stage": row["stage"],
            "quantity": row["quantity"],
            "value": value,
            "value_text": value_text,
            "unit": row["unit"],
            "receipt": row["evidence"]["receipt"],
            "stamp": row["evidence"]["stamp"],
        }
    surface_table = {}
    for cell in sensitivity_block["surface"]:
        key = f"{cell['billing']}_{cell['retention_days']}d_{cell['restores_per_checkpoint']}r"
        surface_table[key] = {
            "billing": cell["billing"],
            "retention_days": cell["retention_days"],
            "restores": cell["restores_per_checkpoint"],
            "a_usd": cell["arm"]["A"]["total_usd"],
            "c_usd": cell["arm"]["C"]["total_usd"],
            "delta_usd": cell["delta"]["total_usd"],
            "cheaper": cell["delta"]["cheaper_arm"],
            "be_storage": cell["break_even"]["storage_usd_per_gib_month"],
            "be_compute": cell["break_even"]["compute_usd_per_vcpu_hour"],
        }
    price_table = {}
    for price in PRICES:
        price_table[price["id"]] = {
            "label": price["label"],
            "unit": price["unit"],
            "value": price["value"],
            "source": price["source"],
            "verbatim": price["verbatim"],
            "conversion": price["unit_conversion"],
            "retrieved": price["retrieved_utc"],
        }

    head = git("rev-parse", "HEAD")
    weftcodec = git("-C", "WeftCodec", "rev-parse", "HEAD")

    receipt = {
        "schema": "weft.lifecycle-tco.v1",
        "stamp": stamp,
        "raw_log": out_path.removesuffix(".json") + ".log",
        "revision": {"weftfs": head, "weftcodec": weftcodec},
        "purpose": {
            "T082": "one controlled lifecycle stitched from existing receipts; no value here was re-measured by this generator",
            "T089": "a sensitivity surface over published list prices applied to measured resource quantities, not a forecast",
        },
        "lifecycle_rows": rows,
        "lifecycle_table": lifecycle_table,
        "row_count": len(rows),
        "sensitivity": sensitivity_block,
        "surface_table": surface_table,
        "surface_cells": len(surface_table),
        "plane_cells": len(sensitivity_block["price_plane"]),
        "price_table": price_table,
        "cannot_prove": [
            "the rows come from four different courts and three different fixtures (Qwen2.5-0.5B with 8 shards, a small CPU-only Python transformer, a synthetic reuse trace); they are not one end-to-end run and their sum is not a measured end-to-end cost",
            "the write rows measure a single checkpoint, not a training schedule; the async and resume rows measure a different, much smaller model",
            "the retention rows are a synthetic deterministic reuse trace, not a serving workload; they order the same reachability-safe set and say nothing about a real hit-rate",
            "no row measures a GPU, a fleet, a network, or media failure",
            "the partial-restore rows are layers of one transformer block on CPU; the receipt itself says a whole-model next step needs every tensor",
            "the break-even surface assumes one checkpoint per day, steady-state retention, one restore per checkpoint by default, and prices that do not change with volume or commitment",
        ],
        "result": "pass",
    }

    if args.check:
        print(f"lifecycle_tco: {len(rows)} rows validated, sensitivity computed, nothing written")
        return 0

    log_path = out_path.removesuffix(".json") + ".log"
    full = os.path.join(REPO_ROOT, out_path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=1)
        handle.write("\n")

    # Raw log: every extraction and every computed cell, so a reader can check
    # the receipt without re-deriving it.
    log_full = os.path.join(REPO_ROOT, log_path)
    with open(log_full, "w", encoding="utf-8") as handle:
        handle.write(f"lifecycle_tco stamp={stamp} weftfs={head} weftcodec={weftcodec}\n")
        handle.write("\n[T082] rows\n")
        for index, row in enumerate(rows, start=1):
            handle.write(
                f"row{index:02d}\t{row['stage']}\t{row['quantity']}\t{row['value']}\t{row['unit']}")
            handle.write(
                f"\t{row['evidence']['receipt']}\t{row['evidence']['path']}\t{row['evidence']['stamp']}\n")
        sensitivity_block = receipt["sensitivity"]
        handle.write("\n[T089] base\n")
        handle.write(f"base_receipt\t{sensitivity_block['base_receipt']}\n")
        handle.write(f"repetitions\t{sensitivity_block['repetitions']}\n")
        for arm in ("A", "C"):
            values = sensitivity_block["arms"][arm]
            handle.write(
                f"arm\t{arm}\tfootprint_bytes\t{values['footprint_bytes']}"
                f"\twrite_cpu_ms\t{values['write_cpu_ms']}\trestore_cpu_ms\t{values['restore_cpu_ms']}"
                f"\twrite_wall_ms\t{values['write_wall_ms']}\trestore_wall_ms\t{values['cold_restore_wall_ms']}\n"
            )
        handle.write("\n[T089] prices\n")
        for price in sensitivity_block["prices"]:
            handle.write(
                f"price\t{price['id']}\t{price['value']}\t{price['unit']}"
                f"\t{price['verbatim']}\t{price['url']}\t{price['retrieved_utc']}\n"
            )
        handle.write("\n[T089] surface\n")
        for cell in sensitivity_block["surface"]:
            handle.write(
                f"cell\t{cell['billing']}\t{cell['retention_days']}d\t{cell['restores_per_checkpoint']}r"
                f"\tA_usd\t{cell['arm']['A']['total_usd']:.9f}\tC_usd\t{cell['arm']['C']['total_usd']:.9f}"
                f"\tdelta_usd\t{cell['delta']['total_usd']:.9f}\tcheaper\t{cell['delta']['cheaper_arm']}"
                f"\tbe_store\t{cell['break_even']['storage_usd_per_gib_month']:.12f}"
                f"\tbe_compute\t{cell['break_even']['compute_usd_per_vcpu_hour']:.6f}\n"
            )
        handle.write("\n[T089] plane\n")
        for cell in sensitivity_block["price_plane"]:
            handle.write(
                f"plane\t{cell['billing']}\tstorage_x\t{cell['storage_multiplier']}"
                f"\tcompute_x\t{cell['compute_multiplier']}\tcheaper\t{cell['cheaper_arm']}\n"
            )

    print(
        f"lifecycle_tco: rows={len(rows)} surface={len(sensitivity_block['surface'])} "
        f"receipt={out_path} log={log_path}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
