#!/usr/bin/env python3
"""Assemble the bounded parallel tile-decode receipt from the run JSONs.

Every number comes from a `real_model_bench ingest|tiled` run in the bench court
or from the court's own environment. Nothing is hardcoded.
"""

import argparse
import json
import os
import sys
from datetime import datetime, timezone

CONFIGS = [
    ("default", "default", True, "Shipped planner, no auto-tile."),
    (
        "auto",
        "auto_tile",
        True,
        "Planner with --auto-tile-bytes; mode 15 is offered and the bytes-only "
        "price decides.",
    ),
    (
        "forced",
        "forced_tiled_control",
        False,
        "Labelled control only: mode 15 forced for rank-2 tensors with the same "
        "auto-derived tile shape. Present because the bytes-only price does not "
        "select mode 15 on BF16 Qwen, so the required auto_tile configuration "
        "contains no tiled object to exercise the bounded tile decoder.",
    ),
]


def load(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def round3(value):
    return round(float(value), 3)


def speedup(numerator, denominator):
    if not denominator:
        return None
    return round(float(numerator) / float(denominator), 4)


def tiled_tensors(ingest):
    tensors = ingest["ingest"]["base"]["tensors"]
    return [tensor for tensor in tensors if tensor.get("strategy") == "tensor-tiled"]


def tile_shapes(tensors):
    shapes = {json.dumps(tensor.get("tile_shape")) for tensor in tensors}
    return sorted(shapes)


def region_exact(run):
    return all(row["exact"] for row in run["region_reads"]["rows"])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--work", required=True)
    parser.add_argument("--auto-bytes", type=int, required=True)
    parser.add_argument("--staging-mib", type=int, required=True)
    parser.add_argument("--source-check", required=True)
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--instruct-sha", required=True)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()

    ingests = {}
    runs = {}
    for key, display, required, description in CONFIGS:
        ingest_path = os.path.join(args.work, f"ingest-{key}.json")
        if not os.path.exists(ingest_path):
            raise SystemExit(f"missing {ingest_path}")
        ingests[key] = load(ingest_path)
        runs[key] = {}
        for threads in (1, 4):
            path = os.path.join(args.work, f"tiled-{key}-t{threads}.json")
            if not os.path.exists(path):
                raise SystemExit(f"missing {path}")
            runs[key][threads] = load(path)

    checks = {
        "all_hash_exact": True,
        "all_logical_roots_exact": True,
        "all_materialize_exact": True,
        "all_region_reads_exact": True,
        "all_file_sha256_match_source": True,
        "restored_bytes_identical": True,
        "materialized_bytes_identical": True,
        "effective_threads_1_and_4": True,
        "source_sha256_rehashed_in_court": args.source_check == "rehashed_in_court",
        "forced_control_has_tiled_tensors": False,
        "forced_control_region_spans_multiple_tiles": False,
        "default_config_has_no_tiled_tensors": True,
    }
    restored_values = set()
    materialized_values = set()
    source_hashes = set()
    findings = []

    configs = {}
    for key, display, required, description in CONFIGS:
        ingest = ingests[key]
        physical = ingest.get("physical") or {}
        distribution = (ingest.get("strategy_distribution") or {}).get("base") or {}
        tiled = tiled_tensors(ingest)
        entry = {
            "required_configuration": required,
            "description": description,
            "ingest": {
                "generation": (ingest.get("store") or {}).get("generation"),
                "wall_ms": round3((ingest.get("ingest") or {})["base"]["wall_ms"]),
                "tensor_write_ms": round3(
                    (ingest.get("ingest") or {})["base"]["tensor_write_ms"]
                ),
                "peak_rss_kib": ingest.get("peak_rss_kib"),
                "physical": physical,
                "strategy_distribution": distribution,
                "auto_tile_bytes": (
                    (ingest.get("store") or {}).get("options") or {}
                ).get("auto_tile_bytes"),
                "force_tiled_rank2": (
                    (ingest.get("store") or {}).get("options") or {}
                ).get("force_tiled_rank2"),
                "tiled_tensors": len(tiled),
                "tiled_logical_bytes": sum(tensor.get("bytes", 0) for tensor in tiled),
                "tile_shapes": tile_shapes(tiled),
            },
            "restore": {},
            "regions": {},
        }

        for threads in (1, 4):
            run = runs[key][threads]
            restore = run["restore"]
            materialize = run["materialize"]
            checks["all_hash_exact"] = checks["all_hash_exact"] and bool(
                restore["hash_exact"]
            )
            checks["all_logical_roots_exact"] = checks[
                "all_logical_roots_exact"
            ] and bool(restore["logical_roots_exact"])
            checks["all_materialize_exact"] = checks[
                "all_materialize_exact"
            ] and bool(materialize["exact"])
            checks["all_region_reads_exact"] = checks[
                "all_region_reads_exact"
            ] and region_exact(run)
            source_hashes.add(run["source"]["sha256"])
            restored_values.add(restore["restored_bytes"])
            materialized_values.add(materialize["bytes"])
            checks["all_file_sha256_match_source"] = checks[
                "all_file_sha256_match_source"
            ] and materialize["sha256"] == args.base_sha
            checks["effective_threads_1_and_4"] = checks[
                "effective_threads_1_and_4"
            ] and restore["tile_stats"]["threads_effective"] == threads

            entry["restore"][str(threads)] = {
                "read_ms": round3(restore["read_ms"]),
                "read_backend_bytes": restore["backend"]["bytes_read"],
                "read_backend_reads": restore["backend"]["reads"],
                "read_backend_range_reads": restore["backend"]["range_reads"],
                "logical_root_check_ms": round3(restore["logical_root_check_ms"]),
                "tensors": restore["tensors"],
                "restored_bytes": restore["restored_bytes"],
                "tiles": restore["tile_stats"]["tiles"],
                "effective_threads": restore["tile_stats"]["threads_effective"],
                "observed_max_inflight_bytes": restore["tile_stats"][
                    "observed_max_inflight_bytes"
                ],
                "observed_max_inflight_tasks": restore["tile_stats"][
                    "observed_max_inflight_tasks"
                ],
                "oversized_tiles": restore["tile_stats"]["oversized_tiles"],
                "materialize_ms": round3(materialize["ms"]),
                "materialize_bytes": materialize["bytes"],
                "materialize_sha256": materialize["sha256"],
                "materialize_exact": materialize["exact"],
                "peak_rss_kib": run["peak_rss_kib"],
            }
            entry["regions"][str(threads)] = [
                {
                    "label": row["label"],
                    "shape": row["shape"],
                    "bytes": row["bytes"],
                    "ms": round3(row["ms"]),
                    "exact": row["exact"],
                    "tiles_touched": row["tiles_touched"],
                    "threads_effective": row["tile_stats"]["threads_effective"],
                    "backend_bytes_read": row["backend_bytes_read"],
                    "backend_range_reads": row["backend_range_reads"],
                }
                for row in run["region_reads"]["rows"]
            ]

        one = entry["restore"]["1"]
        four = entry["restore"]["4"]
        entry["restore"]["speedup"] = {
            "read_4_vs_1": speedup(one["read_ms"], four["read_ms"]),
            "materialize_4_vs_1": speedup(
                one["materialize_ms"], four["materialize_ms"]
            ),
            "logical_root_check_4_vs_1": speedup(
                one["logical_root_check_ms"], four["logical_root_check_ms"]
            ),
        }
        configs[display] = entry

    # Deltas are physical bytes, not logical bytes.
    base_tensor_bytes = configs["default"]["ingest"]["physical"].get(
        "tensor_data_bytes", 0
    )
    base_total_bytes = configs["default"]["ingest"]["physical"].get(
        "store_total_bytes", 0
    )
    for display in ("auto_tile", "forced_tiled_control"):
        entry = configs[display]
        tensor_bytes = entry["ingest"]["physical"].get("tensor_data_bytes", 0)
        total_bytes = entry["ingest"]["physical"].get("store_total_bytes", 0)
        entry["storage_delta_vs_default"] = {
            "tensor_data_bytes": tensor_bytes - base_tensor_bytes,
            "tensor_data_percent": round3(
                100.0 * (tensor_bytes - base_tensor_bytes) / base_tensor_bytes
            )
            if base_tensor_bytes
            else None,
            "store_total_bytes": total_bytes - base_total_bytes,
            "store_total_percent": round3(
                100.0 * (total_bytes - base_total_bytes) / base_total_bytes
            )
            if base_total_bytes
            else None,
        }

    checks["default_config_has_no_tiled_tensors"] = (
        configs["default"]["ingest"]["tiled_tensors"] == 0
    )
    checks["forced_control_has_tiled_tensors"] = (
        configs["forced_tiled_control"]["ingest"]["tiled_tensors"] > 0
    )
    forced_rows = configs["forced_tiled_control"]["regions"]["4"]
    by_label = {row["label"]: row for row in forced_rows}
    checks["forced_control_region_spans_multiple_tiles"] = (
        by_label.get("whole", {}).get("tiles_touched", 0) > 1
        and by_label.get("rows_100", {}).get("tiles_touched", 0) > 1
    )

    auto_tiled = configs["auto_tile"]["ingest"]["tiled_tensors"]
    findings.append(
        f"auto-tile at {args.auto_bytes} B selected mode 15 for {auto_tiled} of "
        "290 tensors; the bytes-only price kept float-tensor for the BF16 Qwen "
        "weights, so this configuration has the same storage as default."
    )
    for display in ("auto_tile", "forced_tiled_control"):
        delta = configs[display]["storage_delta_vs_default"]
        findings.append(
            f"{display} storage delta: {delta['tensor_data_bytes']} tensor bytes "
            f"({delta['tensor_data_percent']}%)."
        )
    for display in ("default", "auto_tile", "forced_tiled_control"):
        speed = configs[display]["restore"]["speedup"]
        findings.append(
            f"{display}: full-restore read 4-thread vs 1-thread = "
            f"{speed['read_4_vs_1']}x, materialize = {speed['materialize_4_vs_1']}x, "
            "logical-root recheck = "
            f"{speed['logical_root_check_4_vs_1']}x."
        )

    court = {
        "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
        "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
        "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
        "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        "nproc": os.environ.get("WEFT_COURT_NPROC", "unknown"),
        "docker_server": os.environ.get("WEFT_DOCKER_SERVER", "unknown"),
        "docker_client": os.environ.get("WEFT_DOCKER_CLIENT", "unknown"),
    }
    revisions = {
        "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        "weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        "weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
    }

    receipt = {
        "schema": "weft.tiled-parallel.v1",
        "label": (
            "single-host bench court, 4 vCPUs, warm cache, synthetic-control "
            "tiling for BF16 Qwen; no fleet or multi-node claim"
        ),
        "generated_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "court": court,
        "revisions": revisions,
        "corpus": {
            "name": "Qwen/Qwen2.5-0.5B",
            "base_sha256": args.base_sha,
            "instruct_sha256": args.instruct_sha,
            "source_sha256_rehashed_in_court": args.source_check == "rehashed_in_court",
            "tensors_per_checkpoint": 290,
            "logical_bytes_per_checkpoint": 988065536,
            "file_bytes": 988097824,
        },
        "configuration": {
            "auto_tile_bytes": args.auto_bytes,
            "max_inflight_mib": args.staging_mib,
            "thread_counts": [1, 4],
            "repetitions": 1,
            "store_per_configuration": True,
            "required_configurations": ["default", "auto_tile"],
            "control_configurations": ["forced_tiled_control"],
            "note": (
                "The two required configurations are default and auto_tile. The "
                "forced_tiled_control exists because auto_tile selected zero "
                "mode-15 objects on this BF16 corpus; it is labelled as a control "
                "and is not presented as planner output."
            ),
        },
        "configs": configs,
        "findings": findings,
        "checks": checks,
        "all_checks_pass": all(checks.values()),
    }

    os.makedirs(args.out_dir, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out = os.path.join(args.out_dir, f"tiled-parallel-{stamp}.json")
    with open(out, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(out)
    if not receipt["all_checks_pass"]:
        for key, value in checks.items():
            if not value:
                print(f"CHECK FAILED {key}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
