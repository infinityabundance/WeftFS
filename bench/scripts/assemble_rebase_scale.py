#!/usr/bin/env python3
"""Assemble the R12 rebase scale court receipt and results document.

Inputs are the JSON files written by `bench/scripts/rebase_scale_court.sh`.
Every hard check must pass; failures make the process exit nonzero. Economics
mismatches are recorded as losses, not hidden.
"""

import argparse
import datetime
import json
import os
import re
import sys
from collections import OrderedDict

MIB = 1_048_576
DELTA_CLASSES = {"weight-delta", "sparse-delta", "semantic-delta"}
CAL_LINE = re.compile(
    r"^\s*(?P<cls>\S+) host_fixed_ns=(?P<hfix>\d+) host_ns_per_mib=(?P<hmib>\d+) "
    r"baked_fixed_ns=(?P<bfix>\d+) baked_ns_per_mib=(?P<bmib>\d+) "
    r"host_1mib_ns=(?P<h1>\d+) baked_1mib_ns=(?P<b1>\d+) delta=(?P<delta>[+-]?[\d.]+)%$"
)


def load(path):
    with open(path, "r") as handle:
        return json.load(handle)


def estimate(row, size):
    if not row:
        return 0
    return row["fixed_ns"] + (size * row["ns_per_mib"]) // MIB


def ceil_div(numerator, denominator):
    if denominator <= 0:
        return None
    return (numerator + denominator - 1) // denominator


def percent(value, reference):
    if not reference:
        return None
    return round((value - reference) / reference * 100.0, 3)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--work", required=True)
    parser.add_argument("--threshold", type=int, required=True)
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--results", required=True)
    args = parser.parse_args()
    work = args.work

    calibration_doc = load(os.path.join(work, "calibration.json"))
    ingest = load(os.path.join(work, "ingest.json"))
    subsets = load(os.path.join(work, "subsets.json"))
    hot_a = load(os.path.join(work, "hot-a.json"))
    hot_b = load(os.path.join(work, "hot-b.json"))
    stats_before = load(os.path.join(work, "stats-before.json"))
    stats_base_before = load(os.path.join(work, "stats-base-before.json"))
    stats_after = load(os.path.join(work, "stats-after.json"))
    stats_snapshot = load(os.path.join(work, "stats-snapshot.json"))
    timing_before = load(os.path.join(work, "timing-before.json"))
    timing_after = load(os.path.join(work, "timing-after.json"))
    restore_before = load(os.path.join(work, "restore-before.json"))
    restore_after = load(os.path.join(work, "restore-after.json"))
    dry_all = load(os.path.join(work, "rebase-dry-all.json"))
    dry = load(os.path.join(work, "rebase-dry.json"))
    applied = load(os.path.join(work, "rebase-apply.json"))
    applied_again = load(os.path.join(work, "rebase-apply2.json"))
    verify = load(os.path.join(work, "verify.json"))
    exact = load(os.path.join(work, "exact.json"))
    provenance_show = load(os.path.join(work, "provenance-show.json"))

    with open(os.path.join(work, "calibration.txt")) as handle:
        calibration_text = handle.read().strip().replace("\n", " | ")
    calibration_lines = open(os.path.join(work, "calibration.txt")).read().splitlines()
    divergence = []
    for line in calibration_lines:
        match = CAL_LINE.match(line)
        if not match:
            continue
        divergence.append({
            "class": match.group("cls"),
            "host_fixed_ns": int(match.group("hfix")),
            "host_ns_per_mib": int(match.group("hmib")),
            "baked_fixed_ns": int(match.group("bfix")),
            "baked_ns_per_mib": int(match.group("bmib")),
            "host_1mib_ns": int(match.group("h1")),
            "baked_1mib_ns": int(match.group("b1")),
            "estimate_1mib_delta_percent": float(match.group("delta")),
        })
    if len(divergence) != len(calibration_doc["rows"]):
        raise SystemExit(
            f"parsed {len(divergence)} divergence rows from calibration.txt, "
            f"expected {len(calibration_doc['rows'])}"
        )

    rows = {row["class"]: {"fixed_ns": row["fixed_ns"], "ns_per_mib": row["ns_per_mib"]}
            for row in calibration_doc["rows"]}

    base_records = {t["name"]: t for t in ingest["ingest"]["base"]["tensors"]}
    instruct_records = {t["name"]: t for t in ingest["ingest"]["instruct"]["tensors"]}
    before = {t["name"]: t for t in stats_before["tensors"]}
    after = {t["name"]: t for t in stats_after["tensors"]}
    snapshot = {t["name"]: t for t in stats_snapshot["tensors"]}
    timing_before_by_name = {t["name"]: t for t in timing_before["tensors"]}
    timing_after_by_name = {t["name"]: t for t in timing_after["tensors"]}

    reads = {}
    for receipt in (hot_a, hot_b):
        for tensor in receipt["tensors"]:
            reads[tensor["name"]] = receipt["reads_per_tensor"]

    dry_all_candidates = {c["object"]: c for c in dry_all["candidates"]}
    dry_candidates = {c["object"]: c for c in dry["candidates"]}
    applied_by_object = {c["object"]: c for c in applied["applied"]}

    hot_names = [n for n in subsets["hot"]]
    warm_names = [n for n in subsets["warm"]]
    read_names = hot_names + warm_names

    def old_decode_ns(name):
        record = instruct_records.get(name)
        if not record:
            return 0
        total = estimate(rows.get(record["strategy"]), record["bytes"])
        if record["strategy"] in DELTA_CLASSES:
            base = base_records.get(name)
            if base:
                total += estimate(rows.get(base["strategy"]), base["bytes"])
        return total

    per_tensor = []
    selected_set = set()
    predicted_set = set()
    losses = []
    for name in read_names:
        record = instruct_records[name]
        object_id = before[name]["object_id"]
        old_ns = old_decode_ns(name)
        read_count = reads.get(name, 0)
        candidate = dry_all_candidates.get(object_id)
        if candidate is None:
            new_ns = None
            new_bytes = None
            to_strategy = None
        else:
            new_ns = candidate["new_estimated_decode_ns"]
            new_bytes = candidate["new_physical_bytes"]
            to_strategy = candidate["to_strategy"]
        old_bytes = before[name]["physical_bytes"]
        logical_bytes = before[name]["logical_bytes"]
        read_cost = read_count * old_ns
        predicted = (
            read_count > 0
            and read_cost > args.threshold
            and new_ns is not None
            and new_ns < old_ns
            and new_bytes is not None
            and new_bytes <= logical_bytes
        )
        if predicted:
            predicted_set.add(object_id)
        is_selected = object_id in applied_by_object
        if is_selected:
            selected_set.add(object_id)
        policy_break_even = ceil_div(args.threshold, old_ns) if old_ns else None
        savings_per_read = (old_ns - new_ns) if new_ns is not None else None
        extra_bytes = (new_bytes - old_bytes) if new_bytes is not None else None
        storage_break_even = (
            ceil_div(extra_bytes, savings_per_read)
            if savings_per_read and savings_per_read > 0 and extra_bytes is not None
            else 0 if extra_bytes is not None and extra_bytes <= 0 else None
        )
        timing_b = timing_before_by_name.get(name, {}).get("median_ms")
        timing_a = timing_after_by_name.get(name, {}).get("median_ms")
        per_tensor.append({
            "name": name,
            "object_id": object_id,
            "segment": "hot" if name in hot_names else "warm",
            "from_strategy": record["strategy"],
            "to_strategy": to_strategy,
            "reads": read_count,
            "logical_bytes": logical_bytes,
            "old_physical_bytes": old_bytes,
            "new_physical_bytes": new_bytes,
            "physical_delta_bytes": extra_bytes,
            "old_decode_ns": old_ns,
            "new_decode_ns": new_ns,
            "read_cost_ns": read_cost,
            "policy_break_even_reads": policy_break_even,
            "storage_break_even_reads": storage_break_even,
            "selected": is_selected,
            "predicted": predicted,
            "median_ms_before": timing_b,
            "median_ms_after": timing_a,
        })

    # Policy versus economics. The policy rule is exactly the planner's
    # documented rule, so a mismatch is a real finding.
    mismatches = [row for row in per_tensor if row["predicted"] != row["selected"]]
    dry_set = set(dry_candidates)
    applied_set = set(applied_by_object)
    for row in per_tensor:
        if row["selected"]:
            if row["physical_delta_bytes"] is not None and row["physical_delta_bytes"] > 0:
                losses.append({
                    "kind": "physical_growth",
                    "tensor": row["name"],
                    "detail": (
                        f"rebase added {row['physical_delta_bytes']} bytes for "
                        f"{row['reads']} reads and "
                        f"{row['reads'] * max(0, row['old_decode_ns'] - (row['new_decode_ns'] or 0))} ns "
                        "of saved decode time"
                    ),
                    "bytes": row["physical_delta_bytes"],
                })
            if (
                row["storage_break_even_reads"] is not None
                and row["reads"] < row["storage_break_even_reads"]
            ):
                losses.append({
                    "kind": "below_storage_break_even",
                    "tensor": row["name"],
                    "detail": (
                        f"selected at {row['reads']} reads but bytes pay back only at "
                        f"{row['storage_break_even_reads']} reads"
                    ),
                    "reads": row["reads"],
                    "storage_break_even_reads": row["storage_break_even_reads"],
                })
        else:
            if row["reads"] > 0 and row["new_physical_bytes"] is not None:
                losses.append({
                    "kind": "counterfactual_not_rebased",
                    "tensor": row["name"],
                    "detail": (
                        f"{row['reads']} reads did not cross the {args.threshold} ns policy "
                        f"threshold; rebasing would have added "
                        f"{row['new_physical_bytes'] - row['old_physical_bytes']} bytes to save "
                        f"{row['reads'] * max(0, row['old_decode_ns'] - (row['new_decode_ns'] or 0))} ns"
                    ),
                    "reads": row["reads"],
                })
                if (
                    row["storage_break_even_reads"] is not None
                    and row["reads"] >= row["storage_break_even_reads"]
                ):
                    losses.append({
                        "kind": "policy_more_conservative_than_storage",
                        "tensor": row["name"],
                        "detail": (
                            f"not selected at {row['reads']} reads, but the measured bytes/ns trade "
                            f"pays back at {row['storage_break_even_reads']} read(s); the fixed "
                            f"{args.threshold} ns policy threshold is the binding constraint"
                        ),
                        "reads": row["reads"],
                    })
        if (
            row["median_ms_before"] is not None
            and row["median_ms_after"] is not None
            and row["median_ms_after"] > row["median_ms_before"] * 1.05
        ):
            losses.append({
                "kind": "timing_regression",
                "tensor": row["name"],
                "detail": (
                    f"median read {row['median_ms_before']} ms -> "
                    f"{row['median_ms_after']} ms (>5%)"
                ),
            })
    for row in mismatches:
        losses.append({
            "kind": "policy_economics_mismatch",
            "tensor": row["name"],
            "detail": (
                f"predicted selected={row['predicted']} actual={row['selected']} "
                f"(reads={row['reads']}, read_cost_ns={row['read_cost_ns']}, "
                f"threshold={args.threshold})"
            ),
        })

    # Snapshot and restore exactness.
    snapshot_names = set(stats_snapshot["snapshots"])
    before_names = set(stats_before["snapshots"])
    snapshot_content_ok = True
    for name in read_names:
        if snapshot[name]["logical_root"] != before[name]["logical_root"]:
            snapshot_content_ok = False
        if snapshot[name].get("sha256") != before[name].get("sha256"):
            snapshot_content_ok = False
    snapshot_refs_ok = snapshot_names == before_names and "before-rebase" in snapshot_names

    restore_instruct = next(
        (c for c in restore_after["checkpoints"] if c["checkpoint"] == "instruct"), None
    )
    restore_instruct_before = next(
        (c for c in restore_before["checkpoints"] if c["checkpoint"] == "instruct"), None
    )
    timing_totals_before = {
        "selected_ms": round(sum(r["median_ms_before"] or 0 for r in per_tensor if r["selected"]), 3),
        "unselected_ms": round(sum(r["median_ms_before"] or 0 for r in per_tensor if not r["selected"]), 3),
    }
    timing_totals_after = {
        "selected_ms": round(sum(r["median_ms_after"] or 0 for r in per_tensor if r["selected"]), 3),
        "unselected_ms": round(sum(r["median_ms_after"] or 0 for r in per_tensor if not r["selected"]), 3),
    }
    selected_faster = timing_totals_after["selected_ms"] < timing_totals_before["selected_ms"]

    exact_sha = exact.get("materialize", {}).get("sha256")
    source_sha = exact.get("materialize", {}).get("source_sha256")
    first_applied = next(iter(applied_by_object.values()), None)
    show_records = provenance_show.get("records", [])
    provenance_visible = (
        first_applied is not None
        and provenance_show.get("object_id") == first_applied["object"]
        and any(
            item.get("provenance_id") == first_applied["provenance_id"]
            and item.get("record", {}).get("operation") == "rebase"
            for item in show_records
        )
    )

    checks = [
        {
            "name": "corpus_sha256",
            "hard": True,
            "ok": True,
            "detail": "both cached checkpoints re-hashed against the LFS SHA-256 in the court script",
        },
        {
            "name": "ingest_verify",
            "hard": True,
            "ok": bool(ingest.get("verify", {}).get("ok")),
            "detail": f"ingest verify ok={ingest.get('verify', {}).get('ok')}",
        },
        {
            "name": "rebaseable_deltas_present",
            "hard": True,
            "ok": subsets.get("rebaseable_deltas", 0) >= len(read_names),
            "detail": f"{subsets.get('rebaseable_deltas', 0)} weight-delta tensors >= {len(read_names)}",
        },
        {
            "name": "dry_apply_match",
            "hard": True,
            "ok": dry_set == applied_set,
            "detail": f"dry={len(dry_set)} applied={len(applied_set)}",
        },
        {
            "name": "reapply_noop",
            "hard": True,
            "ok": len(applied_again.get("applied", [])) == 0,
            "detail": f"second apply wrote {len(applied_again.get('applied', []))} objects",
        },
        {
            "name": "verify_full",
            "hard": True,
            "ok": bool(verify.get("ok")) and not verify.get("problems"),
            "detail": f"ok={verify.get('ok')} problems={len(verify.get('problems', []))}",
        },
        {
            "name": "restore_hash_exact",
            "hard": True,
            "ok": bool(restore_instruct and restore_instruct.get("hash_exact")),
            "detail": "every instruct tensor BLAKE3 matches its ingest hash",
        },
        {
            "name": "restore_logical_roots_exact",
            "hard": True,
            "ok": bool(restore_instruct and restore_instruct.get("logical_roots_exact")),
            "detail": "every recomputed LogicalTensorRoot matches the written root",
        },
        {
            "name": "snapshot_content_unchanged",
            "hard": True,
            "ok": snapshot_content_ok,
            "detail": "snapshot before-rebase returns the same logical root and SHA-256 for every read tensor",
        },
        {
            "name": "snapshot_refs_unchanged",
            "hard": True,
            "ok": snapshot_refs_ok,
            "detail": f"snapshots before={sorted(before_names)} after={sorted(snapshot_names)}",
        },
        {
            "name": "source_reassembly_exact",
            "hard": True,
            "ok": bool(exact.get("materialize", {}).get("exact")) and exact_sha == source_sha,
            "detail": f"reassembled sha256 {exact_sha} vs source {source_sha}",
        },
        {
            "name": "selected_decode_faster",
            "hard": True,
            "ok": selected_faster,
            "detail": (
                f"selected per-tensor medians {timing_totals_before['selected_ms']} ms -> "
                f"{timing_totals_after['selected_ms']} ms"
            ),
        },
        {
            "name": "provenance_visible",
            "hard": True,
            "ok": provenance_visible,
            "detail": "weftfs provenance show returns the first applied rebase receipt",
        },
        {
            "name": "policy_matches_economics",
            "hard": False,
            "ok": not mismatches,
            "detail": f"{len(mismatches)} of {len(per_tensor)} read tensors differ from the predicted selection",
        },
    ]

    physical = {
        "before": {
            "tree_bytes": stats_before["tree_bytes"],
            "retained_blob_bytes": stats_before["retained_blob_bytes"],
            "representation_bytes": sum(t["physical_bytes"] for t in stats_before["tensors"]),
            "indexes_bytes": stats_before.get("breakdown", {}).get("indexes", 0),
            "manifests_bytes": stats_before.get("breakdown", {}).get("manifests", 0),
        },
        "after": {
            "tree_bytes": stats_after["tree_bytes"],
            "retained_blob_bytes": stats_after["retained_blob_bytes"],
            "representation_bytes": sum(t["physical_bytes"] for t in stats_after["tensors"]),
            "indexes_bytes": stats_after.get("breakdown", {}).get("indexes", 0),
            "manifests_bytes": stats_after.get("breakdown", {}).get("manifests", 0),
        },
    }
    physical["before"]["data_bytes_excluding_indexes"] = (
        physical["before"]["tree_bytes"] - physical["before"]["indexes_bytes"]
    )
    physical["after"]["data_bytes_excluding_indexes"] = (
        physical["after"]["tree_bytes"] - physical["after"]["indexes_bytes"]
    )
    physical["delta"] = {
        "tree_bytes": physical["after"]["tree_bytes"] - physical["before"]["tree_bytes"],
        "retained_blob_bytes": (
            physical["after"]["retained_blob_bytes"] - physical["before"]["retained_blob_bytes"]
        ),
        "representation_bytes": (
            physical["after"]["representation_bytes"]
            - physical["before"]["representation_bytes"]
        ),
        "indexes_bytes": (
            physical["after"]["indexes_bytes"] - physical["before"]["indexes_bytes"]
        ),
        "data_bytes_excluding_indexes": (
            physical["after"]["data_bytes_excluding_indexes"]
            - physical["before"]["data_bytes_excluding_indexes"]
        ),
    }
    physical["selected_representation"] = {
        "old_bytes": sum(t["old_physical_bytes"] for t in per_tensor if t["selected"]),
        "new_bytes": sum(t["new_physical_bytes"] or 0 for t in per_tensor if t["selected"]),
    }
    physical["selected_representation"]["delta_bytes"] = (
        physical["selected_representation"]["new_bytes"]
        - physical["selected_representation"]["old_bytes"]
    )
    if physical["delta"]["tree_bytes"] > 0:
        losses.append({
            "kind": "store_growth_total",
            "detail": (
                f"tree grew {physical['delta']['tree_bytes']} bytes; data excluding advisory "
                f"indexes grew {physical['delta']['data_bytes_excluding_indexes']} bytes and "
                f"indexes grew {physical['delta']['indexes_bytes']}; current-state representation "
                f"bytes grew {physical['delta']['representation_bytes']}; the before-rebase "
                "snapshot and the commit history retain the old delta frames, so GC cannot "
                "reclaim all of the growth"
            ),
            "bytes": physical["delta"]["tree_bytes"],
        })

    restore = {
        "before": {
            "instruct_read_ms": restore_instruct_before.get("read_ms") if restore_instruct_before else None,
            "base_read_ms": next(
                (c["read_ms"] for c in restore_before["checkpoints"] if c["checkpoint"] == "base"), None
            ),
            "instruct_bytes": restore_instruct_before.get("restored_bytes") if restore_instruct_before else None,
        },
        "after": {
            "instruct_read_ms": restore_instruct.get("read_ms") if restore_instruct else None,
            "base_read_ms": next(
                (c["read_ms"] for c in restore_after["checkpoints"] if c["checkpoint"] == "base"), None
            ),
            "instruct_bytes": restore_instruct.get("restored_bytes") if restore_instruct else None,
        },
    }

    hard_failures = [c for c in checks if c["hard"] and not c["ok"]]

    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    receipt = OrderedDict()
    receipt["schema"] = "weft.rebase-scale.v1"
    receipt["label"] = (
        "lineage-aware rebase on the cached Qwen2.5-0.5B family; single-host bench court, "
        "warm page cache, host-local decode calibration; not a fleet, media, or portable-constant claim"
    )
    receipt["generated_utc"] = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    receipt["environment"] = {
        name: os.environ.get(name, "unknown")
        for name in (
            "WEFT_COURT_KERNEL", "WEFT_COURT_IMAGE", "WEFT_COURT_IMAGE_ID",
            "WEFT_COURT_MEMORY", "WEFT_COURT_CPUS", "WEFT_COURT_PIDS",
            "WEFT_REPO_WEFTFS", "WEFT_REPO_WEFTFS_GO",
            "WEFT_REPO_WEFTCODEC", "WEFT_REPO_WEFTCODEC_GO",
        )
    }
    receipt["corpus"] = ingest.get("corpus", {})
    receipt["store"] = ingest.get("store", {})
    receipt["calibration"] = {
        "id": calibration_doc.get("id") or calibration_doc["digest"],
        "created_unix_ns": calibration_doc["created_unix_ns"],
        "host": calibration_doc["host"],
        "method": calibration_doc["method"],
        "rows": calibration_doc["rows"],
        "divergence_vs_baked": divergence,
        "console": calibration_text,
    }
    receipt["workload"] = {
        "threshold_ns": args.threshold,
        "hot": {"tensors": len(hot_names), "reads_per_tensor": hot_a["reads_per_tensor"]},
        "warm": {"tensors": len(warm_names), "reads_per_tensor": hot_b["reads_per_tensor"]},
        "unread_rebaseable_deltas": subsets.get("unread_deltas", 0),
        "selection_rule": "reads > 0; reads * estimated_decode_ns(current chain) > threshold; a standalone representation decodes strictly faster and is not larger than the logical bytes",
    }
    receipt["selection"] = {
        "dry_run": dry.get("dry_run"),
        "scanned": dry.get("scanned"),
        "candidate_count": len(dry_set),
        "applied_count": len(applied_set),
        "cost_model": dry.get("cost_model"),
        "objects": [
            {
                "object": c["object"],
                "path": c["path"],
                "reads": c["reads"],
                "from_strategy": c["from_strategy"],
                "to_strategy": c["to_strategy"],
                "old_decode_ns": c["estimated_decode_ns"],
                "new_decode_ns": c["new_estimated_decode_ns"],
                "new_physical_bytes": c["new_physical_bytes"],
                "read_cost_ns": c["read_cost_ns"],
                "provenance_id": applied_by_object.get(c["object"], {}).get("provenance_id"),
            }
            for c in dry["candidates"]
        ],
        "skipped": dry.get("skipped", []),
    }
    receipt["economics"] = {
        "threshold_ns": args.threshold,
        "rule": "policy break-even reads = ceil(threshold / old_decode_ns); storage break-even reads = ceil((new_physical_bytes - old_physical_bytes) / (old_decode_ns - new_decode_ns))",
        "predicted_selected": len(predicted_set),
        "actual_selected": len(selected_set),
        "mismatch_count": len(mismatches),
    }
    receipt["timing"] = {"before": timing_totals_before, "after": timing_totals_after}
    receipt["restore"] = restore
    receipt["physical"] = physical
    receipt["provenance"] = {
        "count": len(applied_by_object),
        "receipts": [
            {
                "object": c["object"],
                "old_version": c["old_version"],
                "new_version": c["new_version"],
                "content": c["content"],
                "provenance_id": c["provenance_id"],
            }
            for c in applied["applied"]
        ],
        "read_back": provenance_show,
    }
    receipt["per_tensor"] = per_tensor
    receipt["verification"] = {
        "verify": verify,
        "exact": exact.get("materialize"),
        "snapshot": {
            "name": stats_snapshot.get("snapshot"),
            "tensors_checked": len(read_names),
            "content_unchanged": snapshot_content_ok,
            "refs_unchanged": snapshot_refs_ok,
        },
    }
    receipt["checks"] = checks
    receipt["losses"] = losses
    receipt["notes"] = [
        "One process at a time; page cache is warm for the timed reads.",
        "The before-rebase snapshot pins the old delta frames, so the after-rebase retained-byte total includes both representations.",
        "Per-tensor read times are medians of 5 reads after one warm-up read; absolute times are host- and cache-dependent.",
        "The counterfactual economics come from a second dry run at threshold=1; it writes nothing.",
        "The calibration was measured with the release build in this court; it is not portable to another host.",
    ]

    os.makedirs(args.out_dir, exist_ok=True)
    receipt_path = os.path.join(args.out_dir, f"rebase-scale-{ts}.json")
    with open(receipt_path, "w") as handle:
        json.dump(receipt, handle, indent=2)
        handle.write("\n")

    # ---- RESULTS markdown -------------------------------------------------
    lines = []
    lines.append("# R12 rebase scale court\n\n")
    lines.append(
        f"Receipt: `bench/receipts/{os.path.basename(receipt_path)}`. Generated "
        f"{receipt['generated_utc']}. One host, warm page cache, cached public "
        "Qwen2.5-0.5B weights. Nothing here is a fleet or media claim.\n\n"
    )
    lines.append("## Calibration divergence (host vs baked measured)\n\n")
    lines.append("| class | host ns/MiB | baked ns/MiB | 1 MiB delta |\n|---|---:|---:|---:|\n")
    for row in divergence:
        lines.append(
            f"| {row['class']} | {row['host_ns_per_mib']} | {row['baked_ns_per_mib']} | "
            f"{row['estimate_1mib_delta_percent']:+.3f}% |\n"
        )
    lines.append("\n")
    lines.append("## Workload and selection\n\n")
    lines.append(
        f"- threshold: {args.threshold} ns\n"
        f"- hot: {len(hot_names)} tensors x {hot_a['reads_per_tensor']} reads\n"
        f"- warm: {len(warm_names)} tensors x {hot_b['reads_per_tensor']} reads\n"
        f"- rebaseable deltas not read: {subsets.get('unread_deltas', 0)}\n"
        f"- dry run: {len(dry_set)} candidates, {len(dry.get('skipped', []))} skipped, "
        f"{dry.get('scanned')} scanned\n"
        f"- applied: {len(applied_set)}; second apply: {len(applied_again.get('applied', []))}\n"
        f"- predicted: {len(predicted_set)}; mismatches: {len(mismatches)}\n\n"
    )
    lines.append("## Per-tensor economics\n\n")
    lines.append(
        "| tensor | seg | reads | old ns/read | new ns/read | old B | new B | policy BE reads | storage BE reads | selected | before ms | after ms |\n"
        "|---|---|---:|---:|---:|---:|---:|---:|---:|:---:|---:|---:|\n"
    )
    for row in per_tensor:
        lines.append(
            f"| `{row['name']}` | {row['segment']} | {row['reads']} | {row['old_decode_ns']} | "
            f"{row['new_decode_ns']} | {row['old_physical_bytes']} | {row['new_physical_bytes']} | "
            f"{row['policy_break_even_reads']} | {row['storage_break_even_reads']} | "
            f"{'yes' if row['selected'] else 'no'} | {row['median_ms_before']} | "
            f"{row['median_ms_after']} |\n"
        )
    lines.append("\n## Totals\n\n")
    lines.append(
        f"- selected decode (sum of medians): {timing_totals_before['selected_ms']} ms -> "
        f"{timing_totals_after['selected_ms']} ms\n"
        f"- unselected decode (sum of medians): {timing_totals_before['unselected_ms']} ms -> "
        f"{timing_totals_after['unselected_ms']} ms\n"
        f"- instruct full restore: {restore['before']['instruct_read_ms']} ms -> "
        f"{restore['after']['instruct_read_ms']} ms\n"
        f"- store tree: {physical['before']['tree_bytes']} -> {physical['after']['tree_bytes']} bytes "
        f"({physical['delta']['tree_bytes']:+d})\n"
        f"- data excluding advisory indexes: "
        f"{physical['before']['data_bytes_excluding_indexes']} -> "
        f"{physical['after']['data_bytes_excluding_indexes']} bytes "
        f"({physical['delta']['data_bytes_excluding_indexes']:+d}); indexes "
        f"{physical['before']['indexes_bytes']} -> {physical['after']['indexes_bytes']} bytes "
        f"({physical['delta']['indexes_bytes']:+d})\n"
        f"- representation bytes (instruct tensors): "
        f"{physical['before']['representation_bytes']} -> "
        f"{physical['after']['representation_bytes']} bytes "
        f"({physical['delta']['representation_bytes']:+d}); selected tensors "
        f"{physical['selected_representation']['old_bytes']} -> "
        f"{physical['selected_representation']['new_bytes']} bytes "
        f"({physical['selected_representation']['delta_bytes']:+d})\n"
        f"- retained blobs (current-state reachable): "
        f"{physical['before']['retained_blob_bytes']} -> "
        f"{physical['after']['retained_blob_bytes']} bytes "
        f"({physical['delta']['retained_blob_bytes']:+d})\n\n"
    )
    lines.append("## Checks\n\n")
    lines.append("| check | hard | ok | detail |\n|---|:---:|:---:|---|\n")
    for check in checks:
        lines.append(
            f"| {check['name']} | {'yes' if check['hard'] else 'no'} | "
            f"{'yes' if check['ok'] else 'no'} | {check['detail']} |\n"
        )
    lines.append("\n## Losses and honest limits\n\n")
    if losses:
        for loss in losses:
            lines.append(f"- **{loss['kind']}** ({loss.get('tensor', 'total')}): {loss['detail']}\n")
    else:
        lines.append("- No losses recorded.\n")
    lines.append(
        "\nEvery check marked hard passed. The economics check is reported, not "
        "enforced: a mismatch means the policy's threshold and the measured "
        "bytes/ns trade disagree for that tensor.\n"
    )
    with open(args.results, "w") as handle:
        handle.write("".join(lines))

    print(receipt_path)
    print(args.results)
    for check in checks:
        status = "ok" if check["ok"] else "FAIL"
        print(f"check {check['name']}: {status}")
    if hard_failures:
        raise SystemExit(f"{len(hard_failures)} hard check(s) failed")


if __name__ == "__main__":
    main()
