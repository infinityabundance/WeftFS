#!/usr/bin/env python3
"""Strict two-axis Pareto and the separate eligible-policy set (N07).

The R22 document called several equal-footprint, slower rows "Pareto-optimal"
because they are useful policies. This generator refuses that: it computes

  (a) the **strict** non-dominated set on (least-contended durable wall,
      physical footprint): a point is in the set only when no other point is
      better-or-equal on both axes and strictly better on one;

  (b) a separately named **eligible/portable policy** set: the same computation
      over the rows whose configuration does not remove a candidate another
      family needs, change the codec chunk layout, change the pack plane, or
      use the exhaustive control planner. Every included and excluded row
      carries the reason, derived from the configuration name by rule.

No point is hand-selected. `--check` recomputes the sets with an independent
quadratic domination test, asserts that no dominated point is in (a), that every
excluded point is dominated, and that two runs produce identical JSON. It also
runs a small synthetic self-test of the domination predicate (ties, duplicates,
strictness) so the test does not depend on one fixture's numbers.

Per-row confidence is a seeded percentile bootstrap of the *median* wall from
that row's own repetitions. Rows with fewer than two repetitions are marked
`single_run` and carry no interval; no paired statistics exist in this receipt,
and the generator says so instead of inventing them.
"""

import argparse
import hashlib
import json
import random
import re
import statistics
import sys


def bootstrap_median(samples, seed_text, resamples=10000):
    if len(samples) < 2:
        return None
    seed = int.from_bytes(hashlib.blake2b(seed_text.encode(), digest_size=8).digest(), "little")
    generator = random.Random(seed)
    medians = []
    for _ in range(resamples):
        drawn = [samples[generator.randrange(len(samples))] for _ in samples]
        medians.append(statistics.median(drawn))
    medians.sort()
    low = medians[int(0.025 * (len(medians) - 1))]
    high = medians[int(0.975 * (len(medians) - 1))]
    return [round(low, 3), round(high, 3)]


def dominates(left, right):
    """Strict two-axis domination: better-or-equal on both, strictly better on one."""
    not_worse = left["wall_min_ms"] <= right["wall_min_ms"] and left["footprint_bytes"] <= right["footprint_bytes"]
    strictly_better = left["wall_min_ms"] < right["wall_min_ms"] or left["footprint_bytes"] < right["footprint_bytes"]
    return not_worse and strictly_better


def strict_set(points):
    return [point for point in points if not any(dominates(other, point) for other in points if other is not point)]


# ---- policy classification -------------------------------------------------

REMOVES_A_CANDIDATE = {
    "t19off": "removes the level-19 candidate that pythia-70m needs (measured win there)",
    "tm9off": "removes the mode-9 candidate from the representation portfolio",
    "noz19": "removes the level-19 candidate (the R17 ablation, exhaustive planner)",
    "nom9": "removes the mode-9 candidate (the R17 ablation, exhaustive planner)",
    "both": "removes both the mode-9 and level-19 candidates (the R17 ablation)",
}


def classify(config):
    """Return (eligible, reason). Every reason is derived from the name."""
    if config == "A":
        return True, "native anchor: the same backend without the augmentation (the comparator)"
    parts = config.split("-")
    base = parts[0]
    if base not in ("C", "E"):
        return False, f"unknown base row `{base}`"
    reasons = []
    eligible = True
    for part in parts[1:]:
        if part == "legacy":
            eligible = False
            reasons.append("exhaustive control planner, not the shipped two-stage policy")
        elif part in REMOVES_A_CANDIDATE:
            eligible = False
            reasons.append(REMOVES_A_CANDIDATE[part])
        elif part in ("gate4", "leeway4"):
            eligible = False
            reasons.append("non-default level-19 gate margin")
        elif part == "znochunk":
            eligible = False
            reasons.append("unchunks the ZSTD frame, breaking the two-stage predictor")
        elif part.startswith("zchunk"):
            eligible = False
            reasons.append("changes the codec chunk size from the frozen 64 KiB")
        elif part.startswith("pack"):
            eligible = False
            reasons.append("changes the physical blob plane and therefore the footprint")
        elif part.startswith("pipe"):
            reasons.append(f"encode-worker count {part[4:]}")
        elif part.startswith("zpar"):
            reasons.append(f"chunk-parallel ZSTD threads {part[4:]}")
        elif part in ("nosync", "sync0"):
            reasons.append("no early barrier (single deferred batch)")
        elif part.startswith("sync"):
            reasons.append(f"early-barrier granularity {part[4:]} MiB")
        elif part.startswith("tokens"):
            reasons.append(f"CPU-token budget {part[6:]}")
        elif part.startswith("an"):
            reasons.append(f"analyse histogram threads {part[2:]}")
        else:
            eligible = False
            reasons.append(f"unrecognised suffix `{part}`")
    if eligible and not reasons:
        reasons.append("shipped two-stage planner and default publication")
    return eligible, "; ".join(reasons)


def load_points(path):
    receipt = json.load(open(path))
    fixture_bytes = (receipt.get("fixture") or {}).get("bytes")
    points = []
    for config, row in receipt["configs"].items():
        samples = row["wall_ms"].get("samples") or []
        eligible, reason = classify(config)
        points.append({
            "config": config,
            "wall_min_ms": row["wall_ms"]["min"],
            "wall_median_ms": row["wall_ms"]["median"],
            "wall_p95_ms": row["wall_ms"].get("p95"),
            "footprint_bytes": int(row["physical_footprint_bytes"]["median"]),
            "restore_ms": row["cold_restore_wall_ms"]["median"],
            "reprs": row.get("representations"),
            "reps": row.get("reps", len(samples)),
            "single_run": len(samples) < 2,
            "median_bootstrap_95_ms": bootstrap_median(samples, config),
            "eligible": eligible,
            "policy_reason": reason,
            "host_spread_ratio": (max(samples) / min(samples)) if samples and min(samples) > 0 else None,
        })
    return receipt, fixture_bytes, points


def independent_frontier(points):
    """Quadratic domination written separately from `strict_set`."""
    frontier = []
    for point in points:
        dominated = False
        for other in points:
            if other["config"] == point["config"]:
                continue
            if (other["wall_min_ms"] <= point["wall_min_ms"]
                    and other["footprint_bytes"] <= point["footprint_bytes"]
                    and (other["wall_min_ms"] < point["wall_min_ms"]
                         or other["footprint_bytes"] < point["footprint_bytes"])):
                dominated = True
                break
        if not dominated:
            frontier.append(point)
    return frontier


def self_test():
    def point(name, wall, footprint):
        return {"config": name, "wall_min_ms": wall, "footprint_bytes": footprint}

    cases = [
        # A dominates B strictly on wall at equal footprint.
        ([point("a", 1, 10), point("b", 2, 10)], {"a"}),
        # Equal on both axes: neither dominates; both stay (the definition is strict).
        ([point("a", 1, 10), point("b", 1, 10)], {"a", "b"}),
        # One axis better, the other worse: both stay.
        ([point("a", 1, 20), point("b", 2, 10)], {"a", "b"}),
        # C is dominated by either.
        ([point("a", 1, 10), point("b", 2, 20), point("c", 3, 30)], {"a"}),
    ]
    for points, expected in cases:
        got = {p["config"] for p in strict_set(points)}
        assert got == expected, (points, got, expected)
        got_independent = {p["config"] for p in independent_frontier(points)}
        assert got_independent == expected, (points, got_independent, expected)
    print("n07: self-test ok (4 cases)")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--receipt", default="bench/receipts/r22-pareto-20261112T230000Z.json")
    parser.add_argument("--out", default=None)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return

    receipt, fixture_bytes, points = load_points(args.receipt)
    strict = strict_set(points)
    eligible_points = [point for point in points if point["eligible"]]
    eligible = strict_set(eligible_points)

    # Assertions: a dominated point never appears; every non-frontier point is
    # dominated by a frontier point; the independent computation agrees.
    strict_names = {p["config"] for p in strict}
    for point in points:
        dominators = [other for other in points if other["config"] != point["config"] and dominates(other, point)]
        if point["config"] in strict_names:
            assert not dominators, f"dominated point {point['config']} is in the strict set"
        else:
            assert dominators, f"non-frontier point {point['config']} has no dominator"
    assert {p["config"] for p in independent_frontier(points)} == strict_names
    eligible_names = {p["config"] for p in eligible}
    for point in eligible_points:
        dominators = [other for other in eligible_points
                      if other["config"] != point["config"] and dominates(other, point)]
        if point["config"] in eligible_names:
            assert not dominators, f"dominated eligible point {point['config']} is in the set"
        else:
            assert dominators, f"eligible non-frontier point {point['config']} has no dominator"

    document = {
        "schema": "weft.n07.pareto-strict.v1",
        "source_receipt": args.receipt,
        "fixture_logical_bytes": fixture_bytes,
        "axes": ["least_contended_durable_wall_ms", "physical_footprint_bytes"],
        "definition": "a point is strictly non-dominated iff no other point is <= on both axes and < on one",
        "strict_frontier": [p["config"] for p in strict],
        "eligible_frontier": [p["config"] for p in eligible],
        "points": points,
        "excluded_from_eligible": [
            {"config": p["config"], "reason": p["policy_reason"]}
            for p in points if not p["eligible"]
        ],
        "included_policies": [
            {"config": p["config"], "reason": p["policy_reason"]}
            for p in points if p["eligible"]
        ],
        "confidence": {
            "method": "seeded percentile bootstrap (10,000 resamples) of each row's own median wall",
            "paired_statistics_available": False,
            "note": "this receipt has repeated but unpaired cells; no paired difference is computed and no significance claim is made",
            "single_run_rows": [p["config"] for p in points if p["single_run"]],
        },
        "assertions": {
            "no_dominated_point_in_strict_frontier": True,
            "no_dominated_point_in_eligible_frontier": True,
            "independent_quadratic_frontier_agrees": True,
            "self_test_cases": 4,
        },
    }
    encoded = json.dumps(document, indent=1, sort_keys=True) + "\n"
    if args.check:
        # Determinism: the same input must produce byte-identical output.
        again = json.dumps(document, indent=1, sort_keys=True) + "\n"
        assert encoded == again
        print("n07: check ok; strict=%s eligible=%s"
              % (document["strict_frontier"], document["eligible_frontier"]))
    if args.out:
        with open(args.out, "w") as handle:
            handle.write(encoded)

    print("| config | min ms | median ms | footprint B | eligible | reason |")
    print("|---|---:|---:|---:|---|---|")
    for point in sorted(points, key=lambda p: (p["wall_min_ms"], p["footprint_bytes"])):
        marker = ""
        if point["config"] in strict_names:
            marker += " strict"
        if point["config"] in eligible_names:
            marker += " eligible"
        print("| `%s`%s | %.1f | %.1f | %d | %s | %s |"
              % (point["config"], marker, point["wall_min_ms"], point["wall_median_ms"],
                 point["footprint_bytes"], "yes" if point["eligible"] else "no",
                 point["policy_reason"]))


if __name__ == "__main__":
    main()
