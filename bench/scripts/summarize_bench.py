#!/usr/bin/env python3
"""Summarize WeftFS benchmark receipts (raw samples -> medians + markdown).

Run inside the bench court so the environment matches the receipts:
  scripts/docker-court.sh bench \
    "python3 bench/scripts/summarize_bench.py --codec bench/receipts/bench-codec-*.json \
       --store bench/receipts/bench-store-*.json --fs bench/receipts/bench-fs-*.json"

This script only reads receipts. It never edits them and never emits a
`winner` field.
"""

import argparse
import glob
import json
import statistics
from pathlib import Path


def median(values):
    clean = [float(v) for v in values if v is not None]
    return statistics.median(clean) if clean else None


def fmt_ms(ns):
    if ns is None:
        return "n/a"
    return f"{ns / 1e6:.3f}"


def fmt_bytes(value):
    if value is None:
        return "n/a"
    return f"{int(value):,}"


def fmt_num(value, digits=1):
    if value is None:
        return "n/a"
    return f"{value:,.{digits}f}"


def load(path):
    return json.loads(Path(path).read_text())


def codec_section(path):
    receipt = load(path)
    print(f"## Codec external baselines\n")
    print(f"Receipt: `{path.name}` (schema `{receipt.get('schema')}`, "
          f"iterations {receipt.get('iterations')}, image "
          f"`{receipt.get('environment', {}).get('court_image_id')}`)\n")
    print("### External CLIs (process wall time, median of raw samples)\n")
    print("| Corpus | Tool | Level | Compressed bytes | Encode ms | Decode ms |")
    print("|---|---|---:|---:|---:|---:|")
    for entry in receipt.get("external", []):
        if not entry.get("available"):
            print(f"| {entry['corpus']} | {entry['tool']} | {entry['level']} | "
                  f"unavailable | n/a | n/a |")
            continue
        print(
            f"| {entry['corpus']} | {entry['tool']} | {entry['level']} | "
            f"{fmt_bytes(median(entry.get('compressed_bytes', [])))} | "
            f"{fmt_ms(median(entry.get('encode_ns', [])))} | "
            f"{fmt_ms(median(entry.get('decode_ns', [])))} |"
        )
    print("\n### WeftCodec (marginal frame and whole-object bytes)\n")
    print("| Corpus | Mode | Chosen | Frame bytes | Dependency bytes | Whole bytes | Encode ms | Decode ms |")
    print("|---|---|---|---:|---:|---:|---:|---:|")
    for entry in receipt.get("weftcodec", []):
        if not entry.get("applicable", True):
            print(f"| {entry['corpus']} | {entry['mode']} | n/a | n/a | n/a | n/a | n/a | n/a |")
            continue
        print(
            f"| {entry['corpus']} | {entry['mode']} | {entry.get('chosen_mode', '')} | "
            f"{fmt_bytes(entry.get('frame_bytes'))} | "
            f"{fmt_bytes(entry.get('dependency_bytes'))} | "
            f"{fmt_bytes(entry.get('whole_object_bytes'))} | "
            f"{fmt_ms(median(entry.get('encode_ns', [])))} | "
            f"{fmt_ms(median(entry.get('decode_ns', [])))} |"
        )
    print()


def store_section(path):
    receipt = load(path)
    print(f"## Store and context benchmark\n")
    print(f"Receipt: `{path.name}` (schema `{receipt.get('schema')}`, "
          f"wall {receipt.get('wall_time_s'):.1f}s, peak RSS "
          f"{receipt.get('vm_hwm_kib')} KiB)\n")
    print("| Objects | Relations | Ingest s | Objects/s | Open ms | Query 0 ms | Query 1 ms | Query n ms | One-hop ms | Verify(full) ms | GC dry ms | GC apply ms |")
    print("|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for result in receipt.get("results", []):
        timings = result["timings"]
        throughput = result["throughput"]
        print(
            f"| {result['objects']:,} | {result['relations']:,} | "
            f"{throughput['ingest_wall_ns'] / 1e9:.1f} | "
            f"{throughput['objects_per_sec']:.1f} | "
            f"{fmt_ms(median(timings.get('open_readonly_ns', [])))} | "
            f"{fmt_ms(median(timings.get('query_0_ns', [])))} | "
            f"{fmt_ms(median(timings.get('query_1_ns', [])))} | "
            f"{fmt_ms(median(timings.get('query_n_ns', [])))} | "
            f"{fmt_ms(median(timings.get('query_hop_ns', [])))} | "
            f"{fmt_ms(median(timings.get('verify_full_ns', [])))} | "
            f"{fmt_ms(median(timings.get('gc_dry_run_ns', [])))} | "
            f"{fmt_ms(median(timings.get('gc_apply_ns', [])))} |"
        )
    print("\n| Objects | Snapshot ms | Churn add ms | Churn delete ms | Disk before GC bytes | Disk after GC bytes | GC deleted | GC deleted bytes | Verify ok |")
    print("|---:|---:|---:|---:|---:|---:|---:|---:|---|")
    for result in receipt.get("results", []):
        timings = result["timings"]
        disk = result["disk"]
        gc = result["gc"]["apply"]
        verify_ok = all(sample["ok"] for sample in result["verify"]["samples"])
        print(
            f"| {result['objects']:,} | "
            f"{fmt_ms(median(timings.get('snapshot_ns', [])))} | "
            f"{fmt_ms(median(timings.get('churn_add_ns', [])))} | "
            f"{fmt_ms(median(timings.get('churn_delete_ns', [])))} | "
            f"{fmt_bytes(disk['before_gc']['total_bytes'])} | "
            f"{fmt_bytes(disk['after_gc']['total_bytes'])} | "
            f"{gc['deleted_count']} | {fmt_bytes(gc['deleted_bytes'])} | {verify_ok} |"
        )
    print("\nQuery cardinalities were asserted before sampling: "
          + ", ".join(
              f"{r['objects']:,} objects -> {r['query_matched']}"
              for r in receipt.get("results", [])
          )
          + ".\n")


def fs_section(path):
    receipt = load(path)
    print(f"## Filesystem benchmark (fio)\n")
    print(f"Receipt: `{path.name}` (schema `{receipt.get('schema')}`, "
          f"fio `{receipt.get('fio_version')}`, go_status "
          f"`{receipt.get('go_status')}`)\n")
    if receipt.get("go_error"):
        print("Go blocked error:\n")
        print("```")
        print(receipt["go_error"].strip())
        print("```\n")
    print("| Target | Workload | Throughput KiB/s | IOPS | p50 ms | p95 ms | p99 ms |")
    print("|---|---|---:|---:|---:|---:|---:|")
    for result in receipt.get("results", []):
        median_values = result["median"]
        print(
            f"| {result['target']} | {result['workload']} | "
            f"{fmt_num(median_values.get('bw_kib_s'))} | "
            f"{fmt_num(median_values.get('iops'), 2)} | "
            f"{fmt_ms(median_values.get('p50_ns'))} | "
            f"{fmt_ms(median_values.get('p95_ns'))} | "
            f"{fmt_ms(median_values.get('p99_ns'))} |"
        )
    print()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codec", nargs="*", default=[])
    parser.add_argument("--store", nargs="*", default=[])
    parser.add_argument("--fs", nargs="*", default=[])
    args = parser.parse_args()

    codec_paths = [Path(p) for pattern in args.codec for p in glob.glob(pattern)]
    store_paths = [Path(p) for pattern in args.store for p in glob.glob(pattern)]
    fs_paths = [Path(p) for pattern in args.fs for p in glob.glob(pattern)]

    # Newest receipt of each kind.
    for path in sorted(codec_paths)[-1:]:
        codec_section(path)
    for path in sorted(store_paths)[-1:]:
        store_section(path)
    for path in sorted(fs_paths)[-1:]:
        fs_section(path)


if __name__ == "__main__":
    main()
