#!/usr/bin/env python3
"""Assemble the R18 receipt from the matrix and cross-family runs.

Inputs are the ablate receipts produced by `examples/augmentation_bench.rs`:

  --matrix       the 7-repetition interleaved matrix (mode "matrix")
  --family       NAME:PATH decision-diff runs (mode "decision-diff")

Output is one `weft.planner_two_stage.v1` receipt with the headline numbers, the
per-tensor decision-diff summary, the cross-family results and every check.
Nothing is estimated here: every number is copied from a measured field. A
missing field is an error, not a zero.
"""
import argparse
import json
import sys


def load(path):
    with open(path) as handle:
        return json.load(handle)


def stat(block, key="min"):
    if not isinstance(block, dict):
        raise SystemExit(f"expected a stat block, got {block!r}")
    value = block.get(key)
    if value is None:
        raise SystemExit(f"stat block has no {key}: {block!r}")
    return value


def config_row(receipt, key):
    try:
        return receipt["configs"][key]
    except KeyError:
        raise SystemExit(f"receipt has no configuration {key!r}")


def summarise_config(receipt, key):
    row = config_row(receipt, key)
    summary = {
        "wall_ms_min": stat(row["wall_ms"], "min"),
        "wall_ms_median": stat(row["wall_ms"], "median"),
        "wall_ms_p95": stat(row["wall_ms"], "p95"),
        "wall_samples_ms": row["wall_ms"]["samples"],
        "cpu_ms_min": stat(row["cpu_ms"], "min"),
        "peak_rss_kib_min": stat(row["peak_rss_kib"], "min"),
        "footprint_bytes": stat(row["physical_footprint_bytes"], "median"),
        "footprint_samples": row["physical_footprint_bytes"]["samples"],
        "cold_restore_ms_min": stat(row["cold_restore_wall_ms"], "min"),
        "cold_restore_median": stat(row["cold_restore_wall_ms"], "median"),
        "representations": row["representations"],
        "representations_stable": row["strategies_stable_across_reps"],
        "restores_exact": row["all_restores_exact"],
        "reps": row["reps"],
        "encode_counts": None,
        "prepare_breakdown_ms": None,
        "planner": None,
        "writer_bytes": None,
        "write_requests": None,
        "crash": None,
    }
    # A (native) and N (native application) have no WeftFS store, so they have
    # no phase ledger, no planner trace and no crash proxy. Their rows are
    # reported for the same-session ladder and nothing else is invented.
    if "attribution_rep" in row:
        ledger = row["attribution_rep"]["ledger"]
        summary["encode_counts"] = ledger["encode_counts"]
        summary["prepare_breakdown_ms"] = ledger["prepare_breakdown"]
        summary["planner"] = ledger.get("planner")
        summary["crash"] = row["attribution_rep"]["crash"]
    if row["samples"]:
        summary["writer_bytes"] = row["samples"][0].get("physical_bytes_written")
        summary["write_requests"] = row["samples"][0].get("write_requests")
    return summary


def cross_family(name, receipt):
    diff = receipt.get("decision_diff")
    if not isinstance(diff, dict):
        raise SystemExit(f"{name}: no decision_diff (was it run with --diff-only?)")
    after = diff["planner_after"]
    return {
        "family": name,
        "fixture": receipt["fixture"],
        "subset": {
            "tensor_count": receipt["subset"]["tensor_count"],
            "logical_bytes": receipt["subset"]["logical_bytes"],
        },
        "tensors_compared": diff["tensors_compared"],
        "strategy_or_size_changes": diff["strategy_or_size_changes"],
        "decisions_match": diff["decisions_match"],
        "changed": diff["changed"],
        "chosen_bytes_before": diff["chosen_bytes_before"],
        "chosen_bytes_after": diff["chosen_bytes_after"],
        "chosen_bytes_delta": diff["chosen_bytes_delta"],
        "mode9_predictions_checked_against_actual": diff[
            "mode9_predictions_checked_against_actual"
        ],
        "mode9_prediction_mismatches": diff["mode9_prediction_mismatches"],
        "interval_violation_tensors": diff["interval_violation_tensors"],
        "skipped_zstd3_with_bound": diff["skipped_zstd3_with_bound"],
        "skipped_zstd19_with_bound": diff["skipped_zstd19_with_bound"],
        "planner_after": after,
        "checks": receipt["checks"],
        "result": receipt["result"],
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--stamp", required=True)
    parser.add_argument("--matrix", required=True)
    parser.add_argument("--family", action="append", default=[])
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    matrix = load(args.matrix)
    if matrix.get("mode") != "matrix":
        raise SystemExit(f"{args.matrix}: expected mode 'matrix', got {matrix.get('mode')!r}")

    configs = {}
    for key in ("A", "B", "B-group", "C", "C-legacy", "C-noz19", "C-nom9", "C-both", "D", "E", "N"):
        if key in matrix["configs"]:
            configs[key] = summarise_config(matrix, key)

    two_stage = configs["C"]
    exhaustive = configs["C-legacy"]
    footprint_delta = two_stage["footprint_bytes"] - exhaustive["footprint_bytes"]
    wall_delta = two_stage["wall_ms_min"] - exhaustive["wall_ms_min"]
    reference_a = configs["A"]["wall_ms_min"]

    families = []
    for entry in args.family:
        name, _, path = entry.partition(":")
        if not path:
            raise SystemExit(f"--family expects NAME:PATH, got {entry!r}")
        families.append(cross_family(name, load(path)))

    checks = dict(matrix["checks"])
    checks["cross_family_decisions_match"] = all(
        family["decisions_match"] for family in families
    )
    checks["cross_family_prediction_exact"] = all(
        family["mode9_prediction_mismatches"] == 0
        and family["interval_violation_tensors"] == 0
        for family in families
    )
    checks["footprint_unchanged_bytes"] = footprint_delta == 0
    checks["matrix_wall_below_exhaustive"] = wall_delta < 0
    result = "pass" if all(checks.values()) else "fail"

    receipt = {
        "schema": "weft.planner_two_stage.v1",
        "generated_utc": args.stamp,
        "result": result,
        "subphase": "R18",
        "design": {
            "stages": [
                "analyse: bounded deterministic whole-codec-chunk ZSTD samples at "
                "levels 3 and 19, plus the codec's bit-exact mode-9 frame-length "
                "prediction from a full exponent histogram",
                "select: encode a candidate only when its physical-size interval "
                "could change the winner or the mode-9 1/64 gate; level 19 is "
                "decided after level 3 is measured, against a measured incumbent",
                "materialise: encode the selected candidate, check the actual size "
                "against its interval, and re-plan exhaustively on a violation "
                "before publication",
            ],
            "scope": "raw (exact), ZSTD level 3 (sampled interval), ZSTD level 19 "
            "(sampled ratio on the measured level-3 frame) and mode 9 (bit-exact). "
            "Every other candidate is prepared exactly as before.",
            "estimator": {
                "zstd_sample_chunks": 4,
                "zstd19_sample_chunks": 2,
                "zstd_rel_error_bps": 500,
                "zstd19_ratio_error_bps": 500,
                "mode9": "exact (weftcodec::modes::float_tensor_encoded_len_from_histogram)",
            },
        },
        "fixture": matrix["fixture"],
        "subset": matrix["subset"],
        "environment": matrix["environment"],
        "orders": matrix["orders"],
        "configs": configs,
        "decision_diff": matrix["decision_diff"],
        "headline": {
            "c_two_stage_wall_ms_min": two_stage["wall_ms_min"],
            "c_two_stage_wall_ms_median": two_stage["wall_ms_median"],
            "c_exhaustive_wall_ms_min": exhaustive["wall_ms_min"],
            "wall_delta_ms": wall_delta,
            "wall_ratio_exhaustive_over_two_stage": (
                exhaustive["wall_ms_min"] / two_stage["wall_ms_min"]
                if two_stage["wall_ms_min"]
                else None
            ),
            "c_two_stage_over_a": two_stage["wall_ms_min"] / reference_a
            if reference_a
            else None,
            "footprint_bytes": two_stage["footprint_bytes"],
            "footprint_delta_vs_exhaustive_bytes": footprint_delta,
            "encode_counts_two_stage": two_stage["encode_counts"],
            "encode_counts_exhaustive": exhaustive["encode_counts"],
            "planner_two_stage": two_stage["planner"],
            "planner_exhaustive": exhaustive["planner"],
        },
        "cross_family": families,
        "checks": checks,
        "inputs": {
            "matrix": args.matrix,
            "families": args.family,
        },
    }
    with open(args.out, "w") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print(f"assemble_planner_two_stage: result={result} receipt={args.out}")
    if result != "pass":
        failed = [name for name, value in checks.items() if not value]
        print(f"assemble_planner_two_stage: failed checks {failed}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
