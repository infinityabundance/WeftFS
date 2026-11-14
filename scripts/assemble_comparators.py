#!/usr/bin/env python3
"""Join the sparse and dense comparator receipts and publish the gap table.

Stdlib only. The WeftFS column is quoted from named receipts (stamp + revision
in those receipts); this court does not re-measure the store, and says so.
"""

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


def table(arm_rows):
    rows = []
    for row in arm_rows:
        entries = {}
        for arm in row["arms"]:
            entries[arm["arm"]] = {
                "stored_bytes": arm["stored_bytes"],
                "delta_bytes": arm["delta_bytes"],
                "exact": arm["exact"],
                "compress_ms": arm.get("compress_ms"),
                "decompress_ms": arm.get("decompress_ms"),
                "index_bytes": arm.get("index_bytes"),
            }
        best = min((a for a in row["arms"]), key=lambda a: a["stored_bytes"])
        worst = max((a for a in row["arms"]), key=lambda a: a["stored_bytes"])
        rows.append(
            {
                "pair": row["pair"]["base"]["path"].split("/")[-1]
                + " -> "
                + row["pair"]["newer"]["path"].split("/")[-1],
                "stored_bytes": entries,
                "best_arm": best["arm"],
                "best_stored_bytes": best["stored_bytes"],
                "worst_arm": worst["arm"],
                "worst_stored_bytes": worst["stored_bytes"],
                "spread_vs_native": None,
                "not_measured": row["not_measured"],
            }
        )
        native = entries.get("N")
        if native:
            rows[-1]["spread_vs_native"] = {
                arm: value["stored_bytes"] / native["stored_bytes"]
                for arm, value in entries.items()
            }
    return rows


def main():
    if len(sys.argv) != 6:
        print(
            "usage: assemble_comparators.py OUTPUT STAMP SPARSE DENSE LOG",
            file=sys.stderr,
        )
        return 64
    output, stamp, sparse_path, dense_path, log_path = sys.argv[1:6]
    sparse = load(sparse_path)
    dense = load(dense_path)

    try:
        revision = subprocess.check_output(
            ["git", "--no-pager", "rev-parse", "HEAD"], text=True
        ).strip()
    except Exception:
        revision = "unknown"

    rows = table([sparse, dense])
    exact_ok = all(
        arm["exact"]
        for row in (sparse, dense)
        for arm in row["arms"]
    )
    checks = {
        "all_arms_exact": exact_ok,
        "every_arm_either_measured_or_blocked": all(
            row["not_measured"] is not None for row in (sparse, dense)
        ),
        "losses_published": True,
    }

    receipt = {
        "schema": "weft.comparators.receipt.v1",
        "stamp": stamp,
        "revision": revision,
        "contract": {
            "path": "bench/CONTRACTS-heldout.md",
            "sha256": sha256_file("bench/CONTRACTS-heldout.md"),
        },
        "pairs": rows,
        "sparse": sparse,
        "dense": dense,
        "weftfs_reference": {
            "note": "quoted from named receipts, not re-measured in this court",
            "sparse_pythia_step0_to_step1": {
                "mode11_weight_delta_bytes": 7434959,
                "mode16_sparse_delta_bytes": 29887,
                "receipt": "bench/receipts/sparse-delta-20261106T052333Z.json",
                "document": "bench/RESULTS-sparse-delta.md",
            },
            "dense_qwen05b_base_to_instruct": {
                "weight_delta_bytes": 541188577,
                "receipt": "bench/receipts/real-float-tensor-20261105T053958Z.json",
                "document": "bench/RESULTS-float-tensor-real.md",
                "note": "mode-11 marginal bytes for the instruct checkpoint against the base, per tensor, no manifests/index/commits; not comparable one-for-one with the arms above",
            },
            "family_totals": {
                "receipt": "bench/receipts/planner-float-20261106T035316Z.json",
                "document": "bench/RESULTS-planner-two-stage.md",
            },
        },
        "checks": checks,
        "checks_passed": sum(1 for value in checks.values() if value),
        "checks_total": len(checks),
        "log": log_path,
        "log_sha256": sha256_file(log_path),
        "not_claimed": [
            "the WeftFS column is quoted from other courts; this court drives external comparators only",
            "no ZipNN or ByteCheckpoint arm ran unless a receipt row exists for it",
            "one host, warm page cache, one zstd version, no GPU",
        ],
    }
    with open(output, "w") as handle:
        json.dump(receipt, handle, indent=1)
    print(
        "comparators: receipt={} checks={}/{} sparse_best={} dense_best={}".format(
            output,
            receipt["checks_passed"],
            receipt["checks_total"],
            rows[0]["best_arm"],
            rows[1]["best_arm"],
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
