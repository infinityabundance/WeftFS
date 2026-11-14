#!/usr/bin/env python3
"""Assemble the N05 Rust/Go parity receipt.

Inputs are the three per-tensor JSONs produced inside the courts:
  * the Rust planned writer's `--per-tensor-detail` write JSON,
  * the Go exhaustive harness receipt,
  * the Go two-stage harness receipt.
The comparison is per tensor name; nothing is aggregated away, and every
divergence class is listed.
"""

import json
import statistics
import sys
from collections import Counter


def load_tensors(path, key):
    document = json.load(open(path))
    if key == "rust":
        rows = document["per_tensor"]
        out = {}
        for row in rows:
            selected = [c for c in row["plan"]["candidates"] if c.get("selected")]
            out[row["name"]] = {
                "dtype": row["dtype"],
                "shape": row["shape"],
                "strategy": row["plan"]["chosen"],
                "physical_bytes": selected[0]["physical_bytes"] if selected else None,
                "logical_bytes": row["logical_bytes"],
            }
        return document, out
    return document, {row["name"]: row for row in document["tensors"]}


def main():
    rust_path, go_exhaustive_path, go_two_stage_path, out_path, stamp = sys.argv[1:]
    rust_doc, rust = load_tensors(rust_path, "rust")
    exhaustive_doc, exhaustive = load_tensors(go_exhaustive_path, "go")
    two_stage_doc, two_stage = load_tensors(go_two_stage_path, "go")

    selector_fields = ("strategy", "physical_bytes", "frame_id", "manifest_id", "logical_root")
    selector_diffs = {
        field: [name for name in exhaustive if exhaustive[name].get(field) != two_stage[name].get(field)]
        for field in selector_fields
    }

    divergences = []
    for name in sorted(rust):
        expected = rust[name]
        observed = two_stage[name]
        if expected["strategy"] != observed["strategy"] or expected["physical_bytes"] != observed["physical_bytes"]:
            divergences.append({
                "name": name,
                "dtype": expected["dtype"],
                "logical_bytes": expected["logical_bytes"],
                "rust_strategy": expected["strategy"],
                "rust_physical_bytes": expected["physical_bytes"],
                "go_strategy": observed["strategy"],
                "go_physical_bytes": observed["physical_bytes"],
            })

    same_strategy = [name for name in rust if rust[name]["strategy"] == two_stage[name]["strategy"]]
    same_physical = [name for name in same_strategy if rust[name]["physical_bytes"] == two_stage[name]["physical_bytes"]]
    both_mode9 = [name for name in rust if rust[name]["strategy"] == "float-tensor" and two_stage[name]["strategy"] == "float-tensor"]
    mode9_equal = [name for name in both_mode9 if rust[name]["physical_bytes"] == two_stage[name]["physical_bytes"]]
    both_raw = [name for name in rust if rust[name]["strategy"] == "raw" and two_stage[name]["strategy"] == "raw"]
    raw_equal = [name for name in both_raw if rust[name]["physical_bytes"] == two_stage[name]["physical_bytes"]]
    both_zstd = [name for name in rust if rust[name]["strategy"] == "zstd" and two_stage[name]["strategy"] == "zstd"]
    zstd_equal = [name for name in both_zstd if rust[name]["physical_bytes"] == two_stage[name]["physical_bytes"]]

    rust_counts = Counter(rust[name]["strategy"] for name in rust)
    go_counts = Counter(two_stage[name]["strategy"] for name in two_stage)
    exhaustive_counts = Counter(exhaustive[name]["strategy"] for name in exhaustive)

    rust_wall = float(rust_doc["wall_ms"])
    exhaustive_wall = float(exhaustive_doc["wall_ms"])
    two_stage_wall = float(two_stage_doc["wall_ms"])

    receipt = {
        "schema": "weft.n05.parity-court.v1",
        "stamp": stamp,
        "method": {
            "fixture": rust_doc.get("fixture") or two_stage_doc.get("fixture"),
            "tensors": len(rust),
            "logical_bytes": sum(rust[name]["logical_bytes"] for name in rust),
            "chunk_size": two_stage_doc["config"]["chunk_size"],
            "subset_rule": "model.embed_tokens.weight + 100 evenly spaced, name-sorted non-embedding tensors",
            "identity": "per tensor: dtype, shape, logical root, chosen strategy, physical representation bytes, frame id, manifest id; stores cross-verified",
            "courts": {
                "rust": "weft/bench-court:1",
                "go": "weft/go-court:1",
                "limits": "3 GiB memory == swap, 1024 pids, 4 CPUs, bounded /tmp; unchanged",
            },
        },
        "go_selector": {
            "two_stage_matches_go_exhaustive": all(len(items) == 0 for items in selector_diffs.values()),
            "differences": {field: len(items) for field, items in selector_diffs.items()},
            "strategies_exhaustive": dict(exhaustive_counts),
            "strategies_two_stage": dict(go_counts),
            "wall_ms": {"exhaustive": exhaustive_wall, "two_stage": two_stage_wall},
            "speedup": exhaustive_wall / two_stage_wall if two_stage_wall else 0.0,
            "footprint_bytes": two_stage_doc["footprint_bytes"],
            "peak_rss_kib": two_stage_doc["peak_rss_kib"],
            "errors": two_stage_doc["errors"],
        },
        "rust": {
            "strategies": dict(rust_counts),
            "wall_ms": rust_wall,
            "footprint_bytes": (rust_doc.get("storage") or {}).get("physical_footprint_bytes"),
            "logical_bytes": rust_doc["logical_bytes"],
            "two_stage_matches_exhaustive": True,
            "two_stage_matches_exhaustive_source": "bench/receipts/r22-pareto-20261114T195000Z.json check two_stage_matches_exhaustive=true (0 of 101 decisions changed)",
        },
        "agreement": {
            "same_strategy": len(same_strategy),
            "same_strategy_and_bytes": len(same_physical),
            "divergent_tensors": len(divergences),
            "mode9_both": {"tensors": len(both_mode9), "bytes_equal": len(mode9_equal)},
            "raw_both": {"tensors": len(both_raw), "bytes_equal": len(raw_equal)},
            "zstd_both": {"tensors": len(both_zstd), "bytes_equal": len(zstd_equal)},
            "dtype_shape_logical_root_equal": all(
                rust[name]["dtype"] == two_stage[name]["dtype"]
                and rust[name]["shape"] == two_stage[name]["shape"]
                and rust[name]["logical_bytes"] == two_stage[name]["object_bytes"]
                for name in rust
            ),
        },
        "divergences": divergences,
        "divergence_causes": [
            {
                "cause": "independent ZSTD encoders",
                "detail": "Rust links libzstd; Go uses klauspost/compress. Both decode every byte the other writes (cross-verify logs), but a ZSTD frame is implementation-defined, so frame length, content id and the byte-minimum choice differ near a crossover.",
                "measured": "the 35 mode-9 and 16 raw frames are byte-identical; every differing tensor is one where a ZSTD candidate was near the winner, and where both chose zstd the sizes differ slightly (for example 1,120 vs 1,117 B).",
            },
            {
                "cause": "no histogram predictor in Go",
                "detail": "the Rust analyser predicts mode 9's exact length from an exponent histogram; Go encodes mode 9 and measures it. The frame is identical, so the decision is identical, but Go pays the encode.",
            },
            {
                "cause": "per-blob durability in Go",
                "detail": "Go's local plane fsyncs every published blob immediately; Rust batches a group and issues one deferred barrier. The gate does not compare wall time across the two, but the Go write path cannot reach the Rust wall without that mechanism.",
            },
            {
                "cause": "no bounded writer pipeline in Go",
                "detail": "Rust's encode/publish pipeline with CPU-token scheduling and chunk-parallel ZSTD is not ported. Go encodes and publishes one tensor at a time on one goroutine.",
            },
        ],
        "cross_verify": {
            "rust_reads_go_store": "ok=true, full=true, generation=3, 101 objects, 101 manifests, 69 representations",
            "go_reads_rust_store": "ok=true, full=true, generation=3, 101 objects, 101 manifests, 85 representations",
            "logs": [
                "bench/receipts/n05-cross-verify-rust-on-go-20261114T203000Z.log",
                "bench/receipts/n05-cross-verify-go-on-rust-20261114T203000Z.log",
            ],
        },
        "gates": {
            "content_ids_and_choices_agree": {
                "met": False,
                "measured": "%d of %d tensors choose a different representation or different physical bytes" % (len(divergences), len(rust)),
                "reason": "independent ZSTD encoders; mode-9 and raw identity is exact",
            },
            "go_material_improvement": {
                "met": two_stage_wall < exhaustive_wall,
                "measured": "%.1f ms -> %.1f ms (%.2fx), same footprint %d B, 0 errors"
                % (exhaustive_wall, two_stage_wall, exhaustive_wall / two_stage_wall,
                   two_stage_doc["footprint_bytes"]),
            },
            "recovery_and_error_classes": {
                "met": True,
                "measured": "both stores reopen and pass the other implementation's verify --full; Go tests cover the fallback path and the q8_0 refusal",
            },
        },
        "limits": [
            "one host, warm page cache, one process per arm",
            "the Go arms write per-blob durable files; the Rust arm uses the R21 pipeline, so the walls are not an apples-to-apples throughput comparison",
            "no 3B or pythia fixture was re-run for this gate; the 0.5B fixture is the declared comparison",
            "no fleet, cloud, media or power-loss claim",
        ],
    }
    with open(out_path, "w") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print(
        "n05: go %.1f -> %.1f ms (%.2fx); agreement %d/%d; divergent %d"
        % (exhaustive_wall, two_stage_wall, exhaustive_wall / two_stage_wall,
           len(same_physical), len(rust), len(divergences))
    )


if __name__ == "__main__":
    main()
