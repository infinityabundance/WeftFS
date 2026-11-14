#!/usr/bin/env python3
"""Assemble the N01 write-rung receipt from the court receipt and its evidence."""

import json
import sys


def stats(samples):
    ordered = sorted(samples)

    def pct(fraction):
        position = (len(ordered) - 1) * fraction
        low = int(position)
        high = min(low + 1, len(ordered) - 1)
        weight = position - low
        return ordered[low] + (ordered[high] - ordered[low]) * weight

    return {
        "min": ordered[0],
        "median": ordered[len(ordered) // 2] if len(ordered) % 2 else
        (ordered[len(ordered) // 2 - 1] + ordered[len(ordered) // 2]) / 2,
        "p95": pct(0.95),
        "p99": pct(0.99),
        "samples": ordered,
    }


def row(config):
    samples = [sample["wall_ms"] for sample in config["samples"]]
    best = min(config["samples"], key=lambda sample: sample["wall_ms"])
    pipeline = best.get("pipeline") or {}
    stages = pipeline.get("stages_ms") or {}
    barrier = pipeline.get("early_barrier") or {}
    return {
        "wall_ms": stats(samples),
        "footprint_bytes": int(config["physical_footprint_bytes"]["median"]),
        "cpu_ms": config["cpu_ms"]["median"],
        "peak_rss_kib": config["peak_rss_kib"]["median"],
        "restore_ms": config["cold_restore_wall_ms"]["median"],
        "best_rep_stages_ms": stages,
        "best_rep_early_barrier": barrier,
        "best_rep_commit_ms": best.get("commit_ms"),
        "best_rep_reader_ms": stages.get("reader"),
        "best_rep_tail_ms": (
            stages.get("pipeline_wall", 0.0) - stages.get("reader", 0.0) - stages.get("encode_wall", 0.0)
            if stages else None
        ),
        "single_run": len(samples) < 2,
        "representations": config.get("representations"),
    }


def main():
    court_path, phase_path, identity_path, seven_path, out_path, stamp = sys.argv[1:]
    court = json.load(open(court_path))
    phase = json.load(open(phase_path))
    identity = json.load(open(identity_path))
    seven = json.load(open(seven_path))

    rows = {name: row(config) for name, config in court["configs"].items()}
    baseline = rows["C-pipe4-wb1"]
    shipped = rows["C-pipe4"]
    best_lane = rows["C-pipe4-wb8"]
    floor_rep = shipped
    pipeline_wall = floor_rep["best_rep_stages_ms"].get("pipeline_wall", 0.0)
    reader = floor_rep["best_rep_stages_ms"].get("reader", 0.0)
    encode = floor_rep["best_rep_stages_ms"].get("encode_wall", 0.0)
    publish = floor_rep["best_rep_stages_ms"].get("publish_wall", 0.0)
    commit = floor_rep["best_rep_commit_ms"] or 0.0
    floor_ms = reader + encode + max(0.0, publish - encode) + commit

    prepare = phase.get("prepare_phases") or {}
    commit_phases = phase.get("commit_phases") or {}
    split = {
        "source": "bench/receipts/n01-phase-split-20261114T195000Z.json (same session; wb4 default; one write, phase profile on)",
        "wall_ms": phase["wall_ms"],
        "prepare_ms": phase["prepare_ms"],
        "commit_ms": phase["commit_ms"],
        "phases_ms": {name: block["total_ms"] for name, block in prepare.items()},
        "commit_phases_ms": {name: block["total_ms"] for name, block in commit_phases.items()},
        "pipeline_stages_ms": (phase.get("pipeline") or {}).get("stages_ms"),
        "early_barrier": (phase.get("pipeline") or {}).get("early_barrier"),
        "publication_ledger": phase.get("publication_ledger"),
    }

    receipt = {
        "schema": "weft.n01.write-rung.v1",
        "stamp": stamp,
        "fixture": court["fixture"],
        "method": {
            "court": "weft/bench-court:1 sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559",
            "limits": "3 GiB memory == swap, 1024 pids, 4 CPUs, bounded /tmp; unchanged",
            "repetitions": 7,
            "interleaving": "seeded Fisher-Yates per repetition (seed 20261114) after the one-position rotation; every row runs once per repetition",
            "statistic": "least-contended (min) is the contract statistic; median, p95 and p99 are published from the same seven samples",
            "footprint_source": "recursive file walk of the store directory",
        },
        "court_receipt": court_path,
        "court_checks": court["checks"],
        "court_result": court["result"],
        "rows": rows,
        "pre_change": {
            "config": "C-pipe4-wb1",
            "wall_ms": baseline["wall_ms"],
            "stages_ms": baseline["best_rep_stages_ms"],
            "early_barrier": baseline["best_rep_early_barrier"],
            "split_note": "one writeback lane (the R21/R22 default before this change): encode wall 403.5 ms, publication wall 445.4 ms (388.5 ms busy), 202-file early barrier 459.2 ms busy on one thread, commit 9.9 ms at a 582.6 ms wall",
        },
        "change": {
            "what": "the early barrier's `fdatasync` work runs on `writeback_threads` lanes (default min(available_parallelism, 4); `wb1..wb8` selects)",
            "where": "src/tensor_pipeline.rs (`sync_drained_files`), TensorPipelineOptions::writeback_threads",
            "invariants": "every published file is still `fdatasync`ed exactly once; the commit's own barrier still closes the batch and fsyncs every remaining file and directory before the commit record; no frame, manifest, commit, index or identity byte changes",
            "bound": "1..=8 lanes, validated; a failure on any lane fails the pipeline before anything is committed",
        },
        "post_change": {
            "shipped_default_config": "C-pipe4 (4 lanes)",
            "shipped_wall_ms": shipped["wall_ms"],
            "best_lane_config": "C-pipe4-wb8",
            "best_lane_wall_ms": best_lane["wall_ms"],
            "delta_vs_pre_change_min_pct": round(100.0 * (baseline["wall_ms"]["min"] - shipped["wall_ms"]["min"]) / baseline["wall_ms"]["min"], 3),
            "delta_vs_pre_change_median_pct": round(100.0 * (baseline["wall_ms"]["median"] - shipped["wall_ms"]["median"]) / baseline["wall_ms"]["median"], 3),
        },
        "stage_split": split,
        "identity": identity,
        "floor": {
            "rule": "reader + encode_wall + (publish_wall - encode_wall) + commit on the least-contended shipped row; the early barrier is fully overlapped, so it is not on this path",
            "reader_ms": reader,
            "encode_wall_ms": encode,
            "publisher_finish_tail_ms": max(0.0, publish - encode),
            "commit_ms": commit,
            "floor_ms": floor_ms,
            "note": "the 336 ms rung would need the embedding's read+encode chain to fall by about %.0f ms; the writeback tail that dominated R22 is now hidden (%.1f ms busy under a %.1f ms pipeline)" % (
                floor_ms - 336.0, floor_rep["best_rep_early_barrier"].get("busy_ms", 0.0), pipeline_wall),
        },
        "seven_b_cells": {
            "receipt": seven_path,
            "planned": seven["phases"]["planned"]["report"]["wall_ms"] if "phases" in seven else None,
            "flat": seven["phases"]["flat"]["report"]["wall_ms"] if "phases" in seven else None,
            "note": "reduced cells with the new default; the changed module is not on the checkpoint path (src/checkpoint.rs never calls the tensor pipeline), so these are a no-regression check, not an equal-treatment comparison",
        },
        "verdict": {
            "rung_ms": 336.0,
            "met": shipped["wall_ms"]["min"] <= 336.0,
            "achieved_min_ms": shipped["wall_ms"]["min"],
            "achieved_median_ms": shipped["wall_ms"]["median"],
            "best_lane_min_ms": best_lane["wall_ms"]["min"],
            "ratio_to_native": shipped["wall_ms"]["min"] / rows["A"]["wall_ms"]["min"],
            "reason": "the durability tail is hidden by parallel file barriers; the remaining critical path is the embedding's read + encode chain (%.1f ms) plus the final publish and commit" % encode if not shipped["wall_ms"]["min"] <= 336.0 else "met",
        },
        "losses": [
            "the rung is not met: %0.1f ms least-contended against 336 ms" % shipped["wall_ms"]["min"],
            "RSS and CPU are unchanged by the lanes (the work is the same), so the change buys wall only",
            "8 lanes is not separable from 4 on the median in this session (one 678.0 ms median sample against 579.3 ms); the least-contended values are 542.3 vs 544.4 ms",
            "the host stalled in several repetitions (p95 is seconds); the paired/least-contended statistic is what the contract uses, and the raw samples are published",
        ],
        "non_claims": [
            "single host, one writer, local POSIX backend, warm page cache",
            "no media, power-loss, fleet, cloud or Go claim",
            "the pipeline is a store-API path; no shipped CLI verb reaches it",
        ],
    }
    with open(out_path, "w") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print("n01: pre %.1f ms -> shipped %.1f ms min / %.1f ms median; rung met=%s"
          % (baseline["wall_ms"]["min"], shipped["wall_ms"]["min"], shipped["wall_ms"]["median"],
             receipt["verdict"]["met"]))


if __name__ == "__main__":
    main()
