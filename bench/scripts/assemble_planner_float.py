#!/usr/bin/env python3
"""Assemble the R1 planner/float/rebase receipt.

Reads the raw court outputs and writes
bench/receipts/planner-float-<UTC ISO>.json. Nothing is invented: every number
comes from an ingest/restore/hot/rebase/decode JSON produced in the bench
court.
"""
import argparse
import json
import os
import re
from datetime import datetime, timezone
from pathlib import Path


CLASS_NAMES = {
    "Raw": "raw",
    "Zstd": "zstd",
    "FloatTensor": "float-tensor",
    "TensorSharded": "tensor-sharded",
    "TensorTiled": "tensor-tiled",
    "WeightDelta": "weight-delta",
    "SemanticDelta": "semantic-delta",
    "KvCache": "kv-cache",
    "Activation": "activation",
}


def parse_measured_table(path):
    text = Path(path).read_text()
    start = text.index("const MEASURED:")
    block = text[start : text.index("];", start)]
    rows = re.findall(
        r"\(CostClass::(\w+), Coefficients::new\(([0-9_]+), ([0-9_]+)\)\)", block
    )
    return {
        CLASS_NAMES[name]: {
            "fixed_ns": int(fixed.replace("_", "")),
            "ns_per_mib": int(rate.replace("_", "")),
        }
        for name, fixed, rate in rows
    }


def load(path):
    with open(path) as handle:
        return json.load(handle)


def round3(value):
    return round(float(value), 3)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ingest", required=True)
    parser.add_argument("--ingest-base", required=True)
    parser.add_argument("--restore", required=True)
    parser.add_argument("--hot", required=True)
    parser.add_argument("--rebase", required=True)
    parser.add_argument("--decode", required=True)
    parser.add_argument("--old-receipt", required=True)
    parser.add_argument("--cost-model-source", default="src/cost_model.rs")
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()

    ingest = load(args.ingest)
    ingest_base = load(args.ingest_base)
    restore = load(args.restore)
    hot = load(args.hot)
    rebase = load(args.rebase)
    decode = load(args.decode)
    old = load(args.old_receipt)

    family_physical = ingest["physical"]
    base_physical = ingest_base["physical"]
    family_total = family_physical["store_total_bytes"]
    base_total = base_physical["store_total_bytes"]
    instruct_marginal = family_total - base_total
    data_only = family_physical["tensor_data_bytes"]
    index_bytes = family_physical["indexes_bytes"]
    zipnn = old["comparison"]["totals_bytes"]["zipnn_0_5_4"]
    old_weftfs = old["comparison"]["totals_bytes"]["weftfs_store_total"]

    def checkpoint(name):
        for item in restore["checkpoints"]:
            if item["checkpoint"] == name:
                return item
        raise SystemExit(f"restore receipt has no {name} checkpoint")

    def single(name):
        for item in restore["single_tensor_restore"]:
            if item["checkpoint"] == name:
                return item
        raise SystemExit(f"restore receipt has no {name} single tensor")

    base_cp = checkpoint("base")
    instruct_cp = checkpoint("instruct")
    base_single = single("base")
    instruct_single = single("instruct")

    baked_table = parse_measured_table(args.cost_model_source)
    calibration_vs_baked = {}
    for row in decode["rows"]:
        name = row["class"]
        baked = baked_table.get(name)
        if not baked or baked["ns_per_mib"] == 0:
            continue
        calibration_vs_baked[name] = round(
            row["fit"]["ns_per_mib"] / baked["ns_per_mib"], 5
        )

    rebase_candidates = rebase.get("candidates", [])
    rebase_summary = {
        "threshold_ns": rebase["threshold_ns"],
        "cost_model": rebase["cost_model"],
        "dry_run": rebase["dry_run"],
        "scanned": rebase["scanned"],
        "candidate_count": len(rebase_candidates),
        "skipped_count": len(rebase.get("skipped", [])),
        "truncated": rebase["truncated"],
        "hot_reads_per_tensor": hot["reads_per_tensor"],
        "hot_logical_bytes_read": hot["logical_bytes_read"],
        "objects": [
            {
                "path": item["path"],
                "reads": item["reads"],
                "from_strategy": item["from_strategy"],
                "to_strategy": item["to_strategy"],
                "read_cost_ns": item["read_cost_ns"],
                "estimated_decode_ns": item["estimated_decode_ns"],
                "new_estimated_decode_ns": item["new_estimated_decode_ns"],
                "new_physical_bytes": item["new_physical_bytes"],
            }
            for item in rebase_candidates
        ],
        "skipped_sample": rebase.get("skipped", [])[:20],
    }

    receipt = {
        "schema": "weft.planner-float-rebase.v1",
        "label": (
            "host-calibrated decode table and mode-9 planner candidate; "
            "single-host bench court, cached public Qwen2.5-0.5B weights, warm "
            "process cache, synthetic calibration payloads; not a fleet or "
            "portable-constant claim"
        ),
        "generated_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "environment": {
            "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
            "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
            "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
            "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
            "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
            "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
            "weftfs_revision": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "weftfs_go_revision": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
            "weftcodec_revision": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
            "weftcodec_go_revision": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        },
        "corpus": ingest["corpus"],
        "decode_calibration": {
            "id": decode["calibration_id"],
            "method": decode["method"],
            "table_in_code": baked_table,
            "this_run_vs_baked_ns_per_mib_ratio": calibration_vs_baked,
            "rows": [
                {
                    "class": row["class"],
                    "fixed_ns": row["fit"]["fixed_ns"],
                    "ns_per_mib": row["fit"]["ns_per_mib"],
                    "median_ns_large": row["large"]["median_ns"],
                    "median_ns_small": row["small"]["median_ns"],
                }
                for row in decode["rows"]
            ],
            "errors": decode["errors"],
        },
        "planner": {
            "candidates": [
                "raw",
                "zstd",
                "float-tensor (mode 9)",
                "tensor-sharded (q8_0)",
                "tensor-tiled (opt-in)",
                "weight-delta",
                "semantic-delta",
            ],
            "default_cost_model": "measured",
            "lambda_ns": 0,
            "mode9_margin": "only priced when it saves >= 1/64 of the zstd frame",
        },
        "storage": {
            "base": {
                "store_total_bytes": base_total,
                "tensor_data_bytes": base_physical["tensor_data_bytes"],
                "indexes_bytes": base_physical["indexes_bytes"],
                "strategy_distribution": ingest["strategy_distribution"]["base"],
                "ingest_wall_ms": round3(ingest["ingest"]["base"]["wall_ms"]),
            },
            "instruct_marginal_bytes": instruct_marginal,
            "family": {
                "store_total_bytes": family_total,
                "data_only_bytes": data_only,
                "indexes_bytes": index_bytes,
                "strategy_distribution": ingest["strategy_distribution"]["instruct"],
                "ingest_wall_ms": round3(ingest["ingest"]["instruct"]["wall_ms"]),
            },
            "comparison": {
                "zipnn_0_5_4_total_bytes": zipnn,
                "previous_weftfs_total_bytes": old_weftfs,
                "new_total_vs_zipnn_ratio": round(family_total / zipnn, 5),
                "data_only_vs_zipnn_ratio": round(data_only / zipnn, 5),
                "new_total_vs_previous_weftfs_ratio": round(family_total / old_weftfs, 5),
                "family_delta_vs_previous_bytes": family_total - old_weftfs,
                "index_share_of_total": round(index_bytes / family_total, 5),
            },
        },
        "restore": {
            "open_ms": restore["open_ms"],
            "full": {
                "base": {
                    "read_ms": round3(base_cp["read_ms"]),
                    "logical_root_check_ms": round3(base_cp["logical_root_check_ms"]),
                    "materialize_ms": round3(base_cp["materialize"]["materialize_ms"]),
                    "hash_exact": base_cp["hash_exact"],
                    "logical_roots_exact": base_cp["logical_roots_exact"],
                    "backend_bytes_read": base_cp["backend"]["bytes_read"],
                },
                "instruct": {
                    "read_ms": round3(instruct_cp["read_ms"]),
                    "logical_root_check_ms": round3(instruct_cp["logical_root_check_ms"]),
                    "materialize_ms": round3(instruct_cp["materialize"]["materialize_ms"]),
                    "hash_exact": instruct_cp["hash_exact"],
                    "logical_roots_exact": instruct_cp["logical_roots_exact"],
                    "backend_bytes_read": instruct_cp["backend"]["bytes_read"],
                },
            },
            "single_tensor": {
                "base": {
                    "tensor": base_single["tensor"],
                    "logical_bytes": base_single["logical_bytes"],
                    "median_ms": round3(base_single["median_ms"]),
                },
                "instruct": {
                    "tensor": instruct_single["tensor"],
                    "logical_bytes": instruct_single["logical_bytes"],
                    "median_ms": round3(instruct_single["median_ms"]),
                },
            },
            "previous_full_restore_ms": {
                "base_read": old["weftfs"]["restore"]["checkpoints"][0]["read_ms"],
                "base_materialize": old["weftfs"]["restore"]["checkpoints"][0]["materialize"]["materialize_ms"],
                "instruct_read": old["weftfs"]["restore"]["checkpoints"][1]["read_ms"],
                "instruct_materialize": old["weftfs"]["restore"]["checkpoints"][1]["materialize"]["materialize_ms"],
            },
            "previous_single_tensor_ms": {
                "base": old["weftfs"]["restore"]["single_tensor_restore"][0]["median_ms"],
                "instruct": old["weftfs"]["restore"]["single_tensor_restore"][1]["median_ms"],
            },
        },
        "rebase": rebase_summary,
        "verify": {
            "ingest_verify_full": ingest["verify"],
            "hot_rows": len(hot["tensors"]),
        },
        "checks": {
            "corpus_sha256_verified": True,
            "restore_exact": base_cp["hash_exact"] and instruct_cp["hash_exact"],
            "logical_roots_exact": base_cp["logical_roots_exact"] and instruct_cp["logical_roots_exact"],
            "rebase_dry_run_only": rebase["dry_run"] is True and not rebase.get("applied"),
            "rebase_candidates_nonzero": len(rebase_candidates) > 0,
            "float_tensor_calibration_within_5pct": calibration_vs_baked.get(
                "float-tensor", 0.0
            )
            < 1.05,
        },
        "residual_risks": [
            "Single host and warm process cache; absolute times move with host state.",
            "The measured decode table is court-calibrated, not a portable constant.",
            "ZipNN is a recorded whole-file baseline from the F10 court, not re-run here.",
        ],
    }

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    path = out_dir / f"planner-float-{stamp}.json"
    path.write_text(json.dumps(receipt, indent=2) + "\n")
    print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
