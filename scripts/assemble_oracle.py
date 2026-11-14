#!/usr/bin/env python3
"""Join the three oracle phase receipts into one N11 receipt (stdlib only)."""

import hashlib
import json
import subprocess
import sys


def load(path):
    with open(path) as handle:
        return json.load(handle)


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    if len(sys.argv) != 7:
        print(
            "usage: assemble_oracle.py OUTPUT STAMP SYNTHETIC VALIDATE HELDOUT LOG",
            file=sys.stderr,
        )
        return 64
    output, stamp, synthetic_path, validate_path, heldout_path, log_path = sys.argv[1:7]
    synthetic = load(synthetic_path)
    validate = load(validate_path)
    heldout = load(heldout_path)

    try:
        revision = subprocess.check_output(
            ["git", "--no-pager", "rev-parse", "HEAD"], text=True
        ).strip()
    except Exception:
        revision = "unknown"

    strata = {row["stratum"]: row["aggregate"] for row in synthetic["synthetic"]}
    overall = synthetic["overall_aggregate"]
    real_overall = {
        "validate": validate["real_aggregate"],
        "held_out": heldout["real_aggregate"],
    }

    checks = {
        # N11 gate: the violation rate is present for every stratum, including
        # non-zero values, and the skip-regret accounting is present.
        "oracle_violation_rate_reported": all(
            "violation_rate" in row and "zstd3_interval_violations" in row
            for row in strata.values()
        ),
        "skip_regret_reported": all("skip_regret_bytes_total" in row for row in strata.values()),
        "mode9_exact_prediction_never_violated": all(
            row["mode9_exact_violations"] == 0 for row in strata.values()
        ),
    }

    receipt = {
        "schema": "weft.two-stage-oracle.receipt.v1",
        "stamp": stamp,
        "revision": revision,
        "contract": {"path": "bench/CONTRACTS-heldout.md", "sha256": sha256_file("bench/CONTRACTS-heldout.md")},
        "scope": {
            "synthetic": "8 strata x 8 deterministic seeds x 32 chunks x the codec chunk size, bf16, 2 MiB each",
            "real": "the same bounded subsets as the held-out planner court",
            "objective": "bytes-only (lambda 0), the shipped default",
        },
        "strata": strata,
        "synthetic": synthetic,
        "validate": validate,
        "held_out": heldout,
        "real_aggregate": real_overall,
        "overall": overall,
        "checks": checks,
        "checks_passed": sum(1 for value in checks.values() if value),
        "checks_total": len(checks),
        "log": log_path,
        "log_sha256": sha256_file(log_path),
        "not_claimed": [
            "the synthetic strata bound a sampler failure mode; they do not estimate its frequency in real training",
            "one host, warm page cache, bench court only",
            "skip-regret in nanoseconds is planner wall time, not end-to-end write time",
        ],
    }

    with open(output, "w") as handle:
        json.dump(receipt, handle, indent=1)

    print(
        "two_stage_oracle: receipt={} checks={}/{} synthetic_violation_rate={:.4f} "
        "real_violation_rate={:.4f} real_skip_regret_bytes={}".format(
            output,
            receipt["checks_passed"],
            receipt["checks_total"],
            overall["violation_rate"],
            real_overall["validate"]["violation_rate"],
            real_overall["validate"]["skip_regret_bytes_total"]
            + real_overall["held_out"]["skip_regret_bytes_total"],
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
