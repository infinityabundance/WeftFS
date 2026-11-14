#!/usr/bin/env python3
"""T083 / N10 paired statistics from an augmentation court receipt.

Reads `repetition_metrics` per arm (raw samples kept by the court), forms the
per-repetition paired difference `arm - A`, and publishes:

* the full distribution per arm (min, p50, p95, max, every raw sample);
* a paired percentile bootstrap 95 % CI on the difference (10,000 resamples,
  seeded and recorded);
* whether each pairwise ordering claim survives (CI excludes zero in the
  claimed direction);
* telemetry captured at run time.

Stdlib only.
"""

import json
import random
import sys


def load(path):
    with open(path) as handle:
        return json.load(handle)


def percentile(sorted_values, p):
    if not sorted_values:
        return float("nan")
    rank = (len(sorted_values) - 1) * max(0.0, min(1.0, p))
    lo = int(rank)
    hi = min(lo + 1, len(sorted_values) - 1)
    weight = rank - lo
    return sorted_values[lo] * (1 - weight) + sorted_values[hi] * weight


def quantiles(values):
    ordered = sorted(values)
    return {
        "n": len(ordered),
        "min": ordered[0] if ordered else None,
        "p50": percentile(ordered, 0.50),
        "p95": percentile(ordered, 0.95),
        "max": ordered[-1] if ordered else None,
        "values": values,
    }


def bootstrap_ci(pairs, seed, draws=10000):
    """Percentile bootstrap CI for the mean paired difference."""
    rng = random.Random(seed)
    n = len(pairs)
    means = []
    for _ in range(draws):
        total = 0.0
        for _ in range(n):
            total += pairs[rng.randrange(n)]
        means.append(total / n)
    means.sort()
    return {
        "resamples": draws,
        "seed": seed,
        "mean": sum(pairs) / n,
        "ci95_low": percentile(means, 0.025),
        "ci95_high": percentile(means, 0.975),
        "contains_zero": percentile(means, 0.025) <= 0.0 <= percentile(means, 0.975),
    }


def main():
    if len(sys.argv) not in (5, 7):
        print(
            "usage: paired_stats.py RAW_RECEIPT TELEMETRY OUT BOOTSTRAP_SEED [STAMP REVISION]",
            file=sys.stderr,
        )
        return 64
    raw = load(sys.argv[1])
    stamp = sys.argv[5] if len(sys.argv) == 7 else raw.get("generated_utc", "unknown")
    revision = sys.argv[6] if len(sys.argv) == 7 else raw.get("environment", {}).get("revisions", {}).get("WeftFS", "unknown")
    telemetry = load(sys.argv[2])
    out_path = sys.argv[3]
    seed = int(sys.argv[4])

    configurations = raw["configurations"]
    base = "A"
    if base not in configurations:
        print("the A row is required for a paired difference", file=sys.stderr)
        return 65

    def series(arm, field):
        return [row.get(field) for row in configurations[arm]["repetition_metrics"]]

    reps = len(configurations[base]["repetition_metrics"])
    arms = {}
    for arm in configurations:
        entry = configurations[arm]
        arms[arm] = {
            "reps": len(entry["repetition_metrics"]),
            "write_wall_ms": quantiles([v for v in series(arm, "write_wall_ms") if v is not None]),
            "cold_restore_wall_ms": quantiles(
                [v for v in series(arm, "cold_restore_wall_ms") if v is not None]
            ),
            "physical_footprint_bytes": quantiles(
                [v for v in series(arm, "physical_footprint_bytes") if v is not None]
            ),
            "cold_bytes_read": quantiles([v for v in series(arm, "cold_bytes_read") if v is not None]),
            "all_restores_exact": entry.get("all_restores_exact"),
            "strategies_stable_across_reps": entry.get("strategies_stable_across_reps"),
        }

    differences = {}
    for arm in configurations:
        if arm == base:
            continue
        entry = {}
        for field, label in (
            ("write_wall_ms", "write_wall_ms"),
            ("cold_restore_wall_ms", "cold_restore_wall_ms"),
            ("physical_footprint_bytes", "physical_footprint_bytes"),
        ):
            a = series(base, field)
            b = series(arm, field)
            pairs = [bb - aa for aa, bb in zip(a, b) if aa is not None and bb is not None]
            if not pairs:
                continue
            entry[label] = {
                "paired_difference_arm_minus_A": bootstrap_ci(
                    pairs,
                    seed + sum((i + 1) * ord(c) for i, c in enumerate(arm + label)) % 1000,
                ),
                "distribution": quantiles(pairs),
            }
        differences[arm] = entry

    claimed_write_order = ["A", "B", "D", "C", "E"]
    claimed_restore_order = ["A", "N", "B", "D", "C", "E"]

    def ordering(sequence, field):
        rows = []
        for left, right in zip(sequence, sequence[1:]):
            if left not in configurations or right not in configurations:
                rows.append({"left": left, "right": right, "status": "arm absent"})
                continue
            a = series(left, field)
            b = series(right, field)
            pairs = [bb - aa for aa, bb in zip(a, b) if aa is not None and bb is not None]
            if not pairs:
                rows.append({"left": left, "right": right, "status": "no pairs"})
                continue
            ci = bootstrap_ci(pairs, seed + 7 * len(rows) + 1)
            rows.append(
                {
                    "left": left,
                    "right": right,
                    "expected": f"{left} < {right}",
                    "mean_difference_right_minus_left": ci["mean"],
                    "ci95_low": ci["ci95_low"],
                    "ci95_high": ci["ci95_high"],
                    # `expected` is left < right, so right-minus-left must be
                    # positive with its whole CI above zero.
                    "survives": ci["ci95_low"] > 0.0,
                    "distinguishable_from_zero": not ci["contains_zero"],
                }
            )
        return rows

    receipt = {
        "schema": "weft.paired-stats.v1",
        "stamp": stamp,
        "revision": revision,
        "reps": reps,
        "bootstrap_seed": seed,
        "base_arm": base,
        "interleaving": raw.get("interleaving")
        or raw.get("configuration", {}).get("interleaving"),
        "repetition_orders": raw.get("orders"),
        "arms": arms,
        "paired_differences": differences,
        "ordering": {
            "claimed_write_order": claimed_write_order,
            "claimed_restore_order": claimed_restore_order,
            "write_wall": ordering(claimed_write_order, "write_wall_ms"),
            "cold_restore_wall": ordering(claimed_restore_order, "cold_restore_wall_ms"),
        },
        "telemetry": telemetry,
        "raw_receipt": sys.argv[1],
        "not_claimed": [
            "one host; no cross-host repeat was possible",
            "the fixture is a 101-tensor subset, not a whole checkpoint",
            "N=10 gives a coarse CI; it cannot resolve differences smaller than the host's own spread",
        ],
    }
    rows = receipt["ordering"]["write_wall"] + receipt["ordering"]["cold_restore_wall"]
    receipt["ordering_survives"] = sum(1 for row in rows if row.get("survives"))
    receipt["ordering_total"] = len(rows)
    receipt["ordering_distinguishable"] = sum(
        1 for row in rows if row.get("distinguishable_from_zero")
    )
    with open(out_path, "w") as handle:
        json.dump(receipt, handle, indent=1, default=str)
    survived = [row for row in receipt["ordering"]["write_wall"] + receipt["ordering"]["cold_restore_wall"] if row.get("survives")]
    total = len(receipt["ordering"]["write_wall"]) + len(receipt["ordering"]["cold_restore_wall"])
    print(
        "paired_stats: reps={} arms={} ordering_claims_survive={}/{} out={}".format(
            reps, len(arms), len(survived), total, out_path
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
