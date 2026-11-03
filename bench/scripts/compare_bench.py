#!/usr/bin/env python3
"""Compare two WeftFS benchmark receipts of the same kind for stability.

Usage:
  python3 bench/scripts/compare_bench.py --kind codec OLD.json NEW.json
  python3 bench/scripts/compare_bench.py --kind store OLD.json NEW.json
  python3 bench/scripts/compare_bench.py --kind fs OLD.json NEW.json

Prints median-of-samples pairs and relative differences. It does not alter the
receipts and does not emit a `winner` field.
"""

import argparse
import json
import statistics
from pathlib import Path


def median(values):
    clean = [float(v) for v in values if v is not None]
    return statistics.median(clean) if clean else None


def rel(old, new):
    if old is None or new is None or old == 0:
        return None
    return (new - old) / old * 100.0


def compare_codec(old, new):
    old_external = {(e["corpus"], e.get("tool_id")): e for e in old.get("external", [])}
    new_external = {(e["corpus"], e.get("tool_id")): e for e in new.get("external", [])}
    rows = []
    for key in sorted(old_external.keys() & new_external.keys()):
        a, b = old_external[key], new_external[key]
        for field in ("encode_ns", "decode_ns"):
            va, vb = median(a.get(field, [])), median(b.get(field, []))
            rows.append(("external " + "/".join(key), field, va, vb))
    old_weft = {
        (e["corpus"], e["mode"], e.get("level")): e
        for e in old.get("weftcodec", [])
        if e.get("applicable", True)
    }
    new_weft = {
        (e["corpus"], e["mode"], e.get("level")): e
        for e in new.get("weftcodec", [])
        if e.get("applicable", True)
    }
    for key in sorted(old_weft.keys() & new_weft.keys()):
        a, b = old_weft[key], new_weft[key]
        for field in ("encode_ns", "decode_ns"):
            rows.append(("weft " + "/".join(str(part) for part in key), field,
                         median(a.get(field, [])), median(b.get(field, []))))
    return rows


def compare_store(old, new):
    old_results = {r["objects"]: r for r in old.get("results", [])}
    new_results = {r["objects"]: r for r in new.get("results", [])}
    rows = []
    for size in sorted(old_results.keys() & new_results.keys()):
        a, b = old_results[size], new_results[size]
        rows.append((f"store {size}", "ingest_wall_ns",
                     a["throughput"]["ingest_wall_ns"], b["throughput"]["ingest_wall_ns"]))
        for field in (
            "open_readonly_ns",
            "query_0_ns",
            "query_1_ns",
            "query_n_ns",
            "query_hop_ns",
            "verify_full_ns",
            "gc_dry_run_ns",
            "gc_apply_ns",
            "snapshot_ns",
        ):
            rows.append((f"store {size}", field,
                         median(a["timings"].get(field, [])),
                         median(b["timings"].get(field, []))))
        rows.append((f"store {size}", "disk_after_gc",
                     a["disk"]["after_gc"]["total_bytes"],
                     b["disk"]["after_gc"]["total_bytes"]))
    return rows


def compare_fs(old, new):
    old_results = {(r["target"], r["workload"]): r for r in old.get("results", [])}
    new_results = {(r["target"], r["workload"]): r for r in new.get("results", [])}
    rows = []
    for key in sorted(old_results.keys() & new_results.keys()):
        a, b = old_results[key], new_results[key]
        for field in ("bw_kib_s", "iops", "p50_ns", "p95_ns", "p99_ns"):
            rows.append(("fs " + "/".join(key), field,
                         a["median"].get(field), b["median"].get(field)))
    return rows


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kind", choices=["codec", "store", "fs"], required=True)
    parser.add_argument("old")
    parser.add_argument("new")
    args = parser.parse_args()
    old = json.loads(Path(args.old).read_text())
    new = json.loads(Path(args.new).read_text())
    rows = {
        "codec": compare_codec,
        "store": compare_store,
        "fs": compare_fs,
    }[args.kind](old, new)
    diffs = []
    print("| Group | Field | Old median | New median | Delta % |")
    print("|---|---|---:|---:|---:|")
    for group, field, a, b in rows:
        difference = rel(a, b)
        if difference is not None:
            diffs.append(abs(difference))
        print(
            f"| {group} | {field} | "
            f"{'n/a' if a is None else f'{a:,.0f}'} | "
            f"{'n/a' if b is None else f'{b:,.0f}'} | "
            f"{'n/a' if difference is None else f'{difference:+.1f}'} |"
        )
    if diffs:
        print(
            f"\nCompared {len(diffs)} medians; median |delta| "
            f"{statistics.median(diffs):.1f}%, max |delta| {max(diffs):.1f}%."
        )


if __name__ == "__main__":
    main()
