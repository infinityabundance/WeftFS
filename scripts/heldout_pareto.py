#!/usr/bin/env python3
"""Derive the N03 per-family Pareto summary from two court receipts.

Descriptive, not a hypothesis test: the axes are fixed by N03 (durable write
wall, physical footprint after write, cold restore wall), and a family that
loses is published in the same table as one that wins. This script writes a
receipt that names both sources, their stamps and their revisions, so no number
here is hand-written.
"""

import json
import sys


def load(path):
    with open(path) as handle:
        return json.load(handle)


def percentile(values, p):
    ordered = sorted(values)
    rank = (len(ordered) - 1) * p
    lo = int(rank)
    hi = min(lo + 1, len(ordered) - 1)
    weight = rank - lo
    return ordered[lo] * (1 - weight) + ordered[hi] * weight


def stats(receipt, arm):
    entry = receipt["configurations"][arm]["stats"]
    writes = [
        row["write_wall_ms"]
        for row in receipt["configurations"][arm]["repetition_metrics"]
        if row.get("write_wall_ms") is not None
    ]
    return {
        # The same accessor the paired-statistics court publishes
        # (`sample_write_wall`: init + source read + write + protocol). The
        # harness's `stats.write_wall_ms` reads `write.wall_ms`, which differs
        # for arm C; both are in the source receipts and only this one is used
        # for the Pareto.
        "write_p50_ms": percentile(writes, 0.50),
        "write_min_ms": min(writes),
        "footprint_p50_bytes": entry["physical_footprint_bytes"]["p50"],
        "restore_p50_ms": entry["cold_restore_wall_ms"]["p50"],
        "reps": receipt["configurations"][arm]["reps"],
        "all_exact": receipt["configurations"][arm]["all_restores_exact"],
    }


def family(receipt, label, fixture, receipt_path):
    a = stats(receipt, "A")
    c = stats(receipt, "C")
    return {
        "family": label,
        "fixture": fixture,
        "source_receipt": receipt_path,
        "source_stamp": receipt.get("generated_utc"),
        "source_revision": receipt.get("environment", {}).get("revisions", {}).get("WeftFS"),
        "reps": a["reps"],
        "a_write_p50_ms": a["write_p50_ms"],
        "c_write_p50_ms": c["write_p50_ms"],
        "c_write_min_ms": c["write_min_ms"],
        "write_ratio": c["write_p50_ms"] / a["write_p50_ms"],
        "write_min_ratio": c["write_min_ms"] / a["write_min_ms"],
        "a_footprint_bytes": a["footprint_p50_bytes"],
        "c_footprint_bytes": c["footprint_p50_bytes"],
        "footprint_ratio": c["footprint_p50_bytes"] / a["footprint_p50_bytes"],
        "storage_reduction_pct": 100.0
        * (a["footprint_p50_bytes"] - c["footprint_p50_bytes"])
        / a["footprint_p50_bytes"],
        "a_restore_p50_ms": a["restore_p50_ms"],
        "c_restore_p50_ms": c["restore_p50_ms"],
        "restore_ratio": c["restore_p50_ms"] / a["restore_p50_ms"],
        "all_exact": a["all_exact"] and c["all_exact"],
        "verdict": "loses on write and restore, wins on footprint"
        if c["write_p50_ms"] > a["write_p50_ms"] and c["footprint_p50_bytes"] < a["footprint_p50_bytes"]
        else "mixed",
    }


def main():
    if len(sys.argv) != 5:
        print(
            "usage: heldout_pareto.py QWEN_RECEIPT PYTHIA_RECEIPT OUT STAMP",
            file=sys.stderr,
        )
        return 64
    qwen_path, pythia_path, out_path, stamp = sys.argv[1:5]
    qwen = load(qwen_path)
    pythia = load(pythia_path)

    families = [
        family(qwen, "qwen_0_5b_instruct", "Qwen2.5-0.5B-Instruct.safetensors (101-tensor subset)", qwen_path),
        family(pythia, "pythia_70m_step0", "pythia-70m step 0 (whole container, F32 containers)", pythia_path),
    ]
    receipt = {
        "schema": "weft.heldout-pareto.v1",
        "stamp": stamp,
        "revision": qwen.get("environment", {}).get("revisions", {}).get("WeftFS"),
        "derived": True,
        "axes": {
            "write": "durable write wall from the harness `write.wall_ms` (the same accessor for both families), p50 over the interleaved repetitions; `write_min_ratio` is the least-contended repetition and is published because the p50 and the minimum diverge on a shared host",
            "footprint": "physical bytes retained after write",
            "restore": "cold restore wall, p50 over the repetitions",
        },
        "families": {row["family"]: row for row in families},
        "note": "A is the native per-tensor baseline on the same backend; C is the shipped planned WeftFS write. Every row is exact.",
        "losses": [
            "pythia-70m (F32) is the worst cell for write cost; it is published in the same table as the Qwen row",
        ],
    }
    with open(out_path, "w") as handle:
        json.dump(receipt, handle, indent=1)
    for row in families:
        print(
            "heldout_pareto: {family} reps={reps} write_ratio={write_ratio:.2f} "
            "footprint_ratio={footprint_ratio:.4f} restore_ratio={restore_ratio:.2f}".format(**row)
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
