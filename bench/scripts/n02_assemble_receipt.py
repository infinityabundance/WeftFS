#!/usr/bin/env python3
"""Assemble the N02 receipt from a store write and a durability-tail probe.

The gate statistic is **paired**: every repetition runs every variant in a
seeded random order, so the per-repetition difference against `per_object`
cancels the host state that made one cell 5-8 s long. The median paired saving
and a seeded percentile bootstrap 95 % interval are reported; the raw
min/median/p95 of every variant are published unchanged next to them.
"""

import json
import random
import statistics
import sys


def total(block, name):
    return round(float((block.get(name) or {}).get("total_ms", 0.0)), 3)


def percentile(values, fraction):
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    low = int(position)
    high = min(low + 1, len(ordered) - 1)
    weight = position - low
    return ordered[low] + (ordered[high] - ordered[low]) * weight


def bootstrap_median(values, seed=20261114, resamples=10000):
    generator = random.Random(seed)
    medians = []
    for _ in range(resamples):
        sample = [values[generator.randrange(len(values))] for _ in values]
        medians.append(statistics.median(sample))
    medians.sort()
    return percentile(medians, 0.025), percentile(medians, 0.975)


def main():
    write_path, probe_path, out_path, stamp = sys.argv[1:]
    write = json.load(open(write_path))
    probe = json.load(open(probe_path))
    pipeline = write.get("pipeline") or {}
    stages = pipeline.get("stages_ms") or {}
    barrier = pipeline.get("early_barrier") or {}
    phases = write.get("prepare_phases") or {}
    commit = write.get("commit_phases") or {}

    init_ms = round(float(write.get("init_ms") or 0.0), 3)
    reader_ms = round(float(write.get("source_read_ms") or 0.0), 3)
    commit_ms = round(float(write.get("commit_ms") or 0.0), 3)
    footprint_ms = round(float(write.get("footprint_ms") or 0.0), 3)
    wall_ms = round(float(write.get("wall_ms") or 0.0), 3)
    encode_wall_ms = round(float(stages.get("encode_wall", 0.0)), 3)
    pipeline_wall_ms = round(float(stages.get("pipeline_wall", 0.0)), 3)
    tail_ms = round(pipeline_wall_ms - reader_ms - encode_wall_ms, 3)
    accounted = round(
        init_ms + reader_ms + encode_wall_ms + tail_ms + commit_ms + footprint_ms, 3
    )
    residual = round(wall_ms - accounted, 3)

    ledger = write.get("publication_ledger") or {}
    backend = write.get("backend_stats") or {}
    blob_payload = probe["blob_distribution"]["bytes"]

    rows = {
        variant: {sample["rep"]: sample["wall_ms"] for sample in data["samples"]}
        for variant, data in probe["variants"].items()
    }
    baseline = rows["per_object"]
    paired = {}
    for variant in rows:
        if variant == "per_object":
            continue
        savings = [
            100.0 * (baseline[rep] - rows[variant][rep]) / baseline[rep]
            for rep in sorted(baseline)
        ]
        low, high = bootstrap_median(savings)
        paired[variant] = {
            "per_rep_saving_pct": [round(value, 3) for value in savings],
            "median_saving_pct": round(statistics.median(savings), 3),
            "bootstrap_95_low_pct": round(low, 3),
            "bootstrap_95_high_pct": round(high, 3),
            "reps": len(savings),
        }

    winning = max(paired, key=lambda name: paired[name]["bootstrap_95_low_pct"])
    verdict = {
        "gate": "whole-wall saving > 10 % beyond the measured host spread for the winning variant",
        "statistic": "paired per-repetition saving against per_object (seeded Fisher-Yates order), median and seeded percentile bootstrap 95 % interval",
        "winning_variant": winning,
        "winning_median_saving_pct": paired[winning]["median_saving_pct"],
        "winning_ci_low_pct": paired[winning]["bootstrap_95_low_pct"],
        "winning_ci_high_pct": paired[winning]["bootstrap_95_high_pct"],
        "threshold_pct": 10.0,
        "clears": bool(paired[winning]["bootstrap_95_low_pct"] > 10.0),
        "unpaired_spread_pct": probe["host_spread"],
        "why_paired": "the host stalled for 1-9 s in individual cells; a paired difference within one repetition cancels that state, while the unpaired min/median/p95 are still published unchanged",
        "product_note": "the winning variant is a device/acknowledgement layout, not a store format. The store-level pack plane measured separately (R22 narrowed matrix) is slower because sealing and catalog publication are charged to the publisher, so no product default changes on this evidence.",
    }

    receipt = {
        "schema": "weft.n02.durability-tail-court.v1",
        "stamp": stamp,
        "store_write": {
            "config": write.get("config"),
            "tensors": write.get("tensor_count"),
            "logical_bytes": write.get("logical_bytes"),
            "physical_bytes_written": write.get("physical_bytes_written"),
            "physical_footprint_bytes": write.get("physical_footprint_bytes"),
            "wall_ms": wall_ms,
            "prepare_ms": round(float(write.get("prepare_ms") or 0.0), 3),
            "commit_ms": commit_ms,
            "stages_ms": stages,
            "early_barrier": barrier,
            "prepare_phases": {name: total(phases, name) for name in phases},
            "commit_phases": {name: total(commit, name) for name in commit},
            "publication_ledger": ledger,
        },
        "critical_path_account": {
            "init": init_ms,
            "reader": reader_ms,
            "encode_wall": encode_wall_ms,
            "publisher_finish_tail": tail_ms,
            "commit": commit_ms,
            "footprint": footprint_ms,
            "accounted": accounted,
            "measured_wall": wall_ms,
            "residual": residual,
            "residual_pct": round(100.0 * residual / wall_ms, 3) if wall_ms else 0.0,
            "closure_rule": "init + reader + encode_wall + (pipeline_wall - reader - encode_wall) + commit + footprint; the residual is the harness loop and is never called zero",
        },
        "io_cross_check": {
            "blob_payload_bytes": blob_payload,
            "backend_bytes_written": ledger.get("backend_bytes_written")
            or backend.get("bytes_written"),
            "backend_writes": ledger.get("backend_writes") or backend.get("writes"),
            "fsync_file": (ledger.get("fsyncs") or {}).get("file"),
            "fsync_dir": (ledger.get("fsyncs") or {}).get("dir"),
            "manifest_blobs": (ledger.get("categories") or {}).get("manifest_blobs"),
            "object_blobs": (ledger.get("categories") or {}).get("object_blobs"),
            "payload_minus_backend": (ledger.get("backend_bytes_written") or 0)
            - blob_payload,
        },
        "variants": probe["variants"],
        "paired_savings": paired,
        "host_spread": probe["host_spread"],
        "verdict": verdict,
        "durability_statements": probe["durability"],
        "method": probe["method"],
        "limits": [
            "one host, one container shape, warm page cache except the probe's own os.sync()",
            "/proc/diskstats is host-wide and is reported as an upper bound",
            "the probe copies the store's published bytes; it is a device and acknowledgement comparison, not a new store format",
            "the store-level pack plane is slower; no product default changes on this evidence",
            "no media, power-loss, fleet or cloud claim",
        ],
    }
    with open(out_path, "w") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print(
        "n02_durability_tail: winner %s median %.1f%% CI [%.1f, %.1f] clears=%s"
        % (
            winning,
            paired[winning]["median_saving_pct"],
            paired[winning]["bootstrap_95_low_pct"],
            paired[winning]["bootstrap_95_high_pct"],
            verdict["clears"],
        )
    )


if __name__ == "__main__":
    main()
