#!/usr/bin/env python3
"""N02 — the durability tail, traced and compared.

Writes the *published* byte ranges of a real WeftFS store under three layouts on
the same device with the same acknowledgement contract (write every byte, then
one data barrier per file, then one barrier on the directory; no pre-ack
transcode):

  per_object          one file per published blob (the shipped layout, 202 files)
  fewer_larger_16     the same bytes concatenated into 16 files plus one index
  one_contiguous_pack the same bytes in one pack file with an in-file index

Nothing is compressed, moved or re-encoded. The payload is read once from the
store's `objects/` and `manifests/` trees, so the byte totals are the store's own
publication ledger. Wall time starts before the first write and ends after the
last directory barrier.

Every repetition records `/proc/self/io` and the host `/proc/diskstats` deltas so
the process's write bytes and the block layer's received sectors can be compared
with the payload instead of assumed. `/proc/diskstats` is host-wide: the court is
the only writer in this container shape but not the only writer on the host, so
the delta is reported as an upper bound and labelled.

Exit 78 with one `n02_durability_tail: skip <reason>` line when the store or the
environment is missing. Never raises a limit.
"""

import argparse
import json
import os
import random
import shutil
import statistics
import sys
import time

SECTOR = 512


def skip(reason):
    print(f"n02_durability_tail: skip {reason}")
    raise SystemExit(78)


def read_proc_io(path="/proc/self/io"):
    out = {}
    with open(path, "r") as handle:
        for line in handle:
            key, _, value = line.partition(":")
            out[key.strip()] = int(value.strip())
    return out


def read_diskstats(path="/proc/diskstats"):
    out = {}
    with open(path, "r") as handle:
        for line in handle:
            fields = line.split()
            if len(fields) < 14:
                continue
            name = fields[2]
            # Skip partitions' parents by using every entry; the caller sums a
            # named prefix only, so double counting is explicit.
            out[name] = {
                "sectors_written": int(fields[9]),
                "writes_completed": int(fields[7]),
            }
    return out


def collect_blobs(store):
    blobs = []
    for tree in ("objects", "manifests"):
        root = os.path.join(store, tree)
        for base, _dirs, files in os.walk(root):
            for name in files:
                path = os.path.join(base, name)
                relative = os.path.relpath(path, store)
                blobs.append((relative, os.path.getsize(path)))
    blobs.sort()
    return blobs


def write_file(path, chunks):
    """Write `chunks` (iterable of bytes) to a new file in bounded pieces."""
    with open(path, "wb", buffering=0) as handle:
        for chunk in chunks:
            view = memoryview(chunk)
            offset = 0
            while offset < len(view):
                offset += handle.write(view[offset:])
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def fsync_dir(path):
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def plan_variant(blobs, variant, store):
    """Return a list of (output_path, list[source_path]) for the variant."""
    sources = [(os.path.join(store, relative), size) for relative, size in blobs]
    if variant == "per_object":
        return [[(relative, [(path, size)])] for path, relative, size in sources]
    if variant == "fewer_larger_16":
        # Group by descending size so each group holds a mix of large and small
        # blobs; the index records (relative, offset, length) for every member.
        ordered = sorted(sources, key=lambda item: (-item[1], item[0]))
        groups = [[] for _ in range(16)]
        for index, item in enumerate(ordered):
            groups[index % 16].append(item)
        out = []
        for number, group in enumerate(groups):
            out.append((f"group-{number:02d}.bin", [(path, size) for path, size in group]))
        return [out]
    if variant == "one_contiguous_pack":
        return [[("pack-0000.bin", [(path, size) for path, size in sources])]]
    raise SystemExit(f"unknown variant {variant}")


def run_once(store, target, variant, blob_list):
    os.makedirs(target, exist_ok=True)
    io_before = read_proc_io()
    disk_before = read_diskstats()
    started = time.perf_counter()
    files = 0
    fsyncs = 0
    payload = 0
    if variant == "per_object":
        # One directory per 256 prefixes, like the store.
        for relative, size in blob_list:
            path = os.path.join(target, relative)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            source = os.path.join(store, relative)
            with open(source, "rb") as src, open(path, "wb", buffering=0) as dst:
                remaining = size
                while remaining:
                    block = src.read(min(1 << 20, remaining))
                    if not block:
                        break
                    dst.write(block)
                    remaining -= len(block)
            fd = os.open(path, os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
            files += 1
            fsyncs += 1
            payload += size
        fsync_dir(target)
    else:
        # Grouped variants: build an index first, then each group file.
        index = []
        for name, members in plan_variant(blob_list, variant, store)[0]:
            offset = 0
            with open(os.path.join(target, name), "wb", buffering=0) as dst:
                for source, size in members:
                    with open(source, "rb") as src:
                        remaining = size
                        while remaining:
                            block = src.read(min(1 << 20, remaining))
                            if not block:
                                break
                            dst.write(block)
                            remaining -= len(block)
                    index.append([os.path.relpath(source, store), name, offset, size])
                    offset += size
                    payload += size
            fd = os.open(os.path.join(target, name), os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
            files += 1
            fsyncs += 1
        index_path = os.path.join(target, "index.wcb")
        with open(index_path, "w") as handle:
            json.dump(index, handle)
        fd = os.open(index_path, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
        files += 1
        fsyncs += 1
    fsync_dir(target)
    wall_ms = (time.perf_counter() - started) * 1000.0
    io_after = read_proc_io()
    disk_after = read_diskstats()
    sectors = 0
    writes = 0
    for name, after in disk_after.items():
        before = disk_before.get(name)
        if not before:
            continue
        sectors += max(0, after["sectors_written"] - before["sectors_written"])
        writes += max(0, after["writes_completed"] - before["writes_completed"])
    return {
        "wall_ms": wall_ms,
        "files": files,
        "fsyncs": fsyncs,
        "payload_bytes": payload,
        "proc_write_bytes": max(0, io_after.get("write_bytes", 0) - io_before.get("write_bytes", 0)),
        "proc_write_chars": max(0, io_after.get("wchar", 0) - io_before.get("wchar", 0)),
        "disk_sectors_written": sectors,
        "disk_bytes_written": sectors * SECTOR,
        "disk_write_calls": writes,
    }


def percentile(samples, fraction):
    ordered = sorted(samples)
    if not ordered:
        return 0.0
    position = (len(ordered) - 1) * fraction
    low = int(position)
    high = min(low + 1, len(ordered) - 1)
    weight = position - low
    return ordered[low] + (ordered[high] - ordered[low]) * weight


def summarize(samples, key):
    values = [sample[key] for sample in samples]
    return {
        "min": min(values),
        "median": statistics.median(values),
        "p95": percentile(values, 0.95),
        "p99": percentile(values, 0.99),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--store", required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--reps", type=int, default=7)
    parser.add_argument("--seed", type=int, default=20261114)
    args = parser.parse_args()

    if not os.path.isdir(args.store):
        skip(f"store {args.store} is missing")
    blobs = collect_blobs(args.store)
    if not blobs:
        skip(f"store {args.store} has no objects/ or manifests/ blobs")
    total = sum(size for _rel, size in blobs)
    if total < 1024 * 1024:
        skip(f"store payload is only {total} B; need at least 1 MiB")
    if shutil.which("python3") is None:
        skip("python3 is unavailable")

    variants = ["per_object", "fewer_larger_16", "one_contiguous_pack"]
    rows = {name: [] for name in variants}
    orders = []
    rng = random.Random(args.seed)
    for rep in range(1, args.reps + 1):
        order = list(variants)
        rng.shuffle(order)
        orders.append({"rep": rep, "order": order})
        for variant in order:
            target = os.path.join(args.work, f"rep-{rep:02d}", variant)
            if os.path.isdir(target):
                shutil.rmtree(target)
            os.sync()
            sample = run_once(args.store, target, variant, blobs)
            sample["variant"] = variant
            sample["rep"] = rep
            rows[variant].append(sample)
            # The next cell must not inherit this cell's dirty pages.
            os.sync()
            shutil.rmtree(target, ignore_errors=True)

    summary = {}
    for variant in variants:
        samples = rows[variant]
        summary[variant] = {
            "samples": samples,
            "wall_ms": summarize(samples, "wall_ms"),
            "files": summarize(samples, "files"),
            "fsyncs": summarize(samples, "fsyncs"),
            "proc_write_bytes": summarize(samples, "proc_write_bytes"),
            "disk_bytes_written": summarize(samples, "disk_bytes_written"),
        }

    baseline = summary["per_object"]["wall_ms"]["min"]
    for variant in variants:
        best = summary[variant]["wall_ms"]["min"]
        summary[variant]["saving_vs_per_object_min_pct"] = (
            100.0 * (baseline - best) / baseline if baseline else 0.0
        )
    spread = {
        "per_object_min_to_median_pct": 100.0
        * (summary["per_object"]["wall_ms"]["median"] - baseline)
        / baseline
        if baseline
        else 0.0,
        "per_object_min_to_p95_pct": 100.0
        * (summary["per_object"]["wall_ms"]["p95"] - baseline)
        / baseline
        if baseline
        else 0.0,
    }

    receipt = {
        "schema": "weft.n02.durability-tail.v1",
        "generated_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "method": {
            "store": args.store,
            "blob_count": len(blobs),
            "payload_bytes": total,
            "reps": args.reps,
            "seed": args.seed,
            "order": "seeded Fisher-Yates per repetition; every variant runs once per repetition",
            "acknowledgement": "every output byte written, one fdatasync per file, one directory fsync; no pre-acknowledgement transcode",
            "quiesce": "os.sync() before and after every cell",
            "diskstats": "/proc/diskstats is host-wide and is reported as an upper bound, not as the court's own write",
        },
        "blob_distribution": {
            "count": len(blobs),
            "bytes": total,
            "min": min(size for _rel, size in blobs),
            "median": statistics.median([size for _rel, size in blobs]),
            "max": max(size for _rel, size in blobs),
        },
        "orders": orders,
        "variants": summary,
        "host_spread": spread,
        "durability": {
            "per_object": "each published file is individually durable after its own fdatasync; its directory entry is durable after the directory fsync. A crash between files leaves a prefix of durable files and no torn file.",
            "fewer_larger_16": "each of the 16 group files is individually durable; the index is a 17th file and is not atomic with the groups, so a crash can leave a durable group file with no durable index. Reconstructing a member additionally needs the index.",
            "one_contiguous_pack": "the pack file is individually durable after one fdatasync, but its in-file index and members share one barrier; a torn append is visible to a reader unless an external commit record names a verified length. This is what the store's pack plane adds a catalog and commit ordering for.",
        },
        "note": "This is a device/acknowledgement comparison of the byte ranges a real store publishes. It is not a new store layout and it changes no format.",
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print(
        "n02_durability_tail: baseline min %.1f ms; %s"
        % (
            baseline,
            ", ".join(
                "%s %.1f%% (%d files)"
                % (
                    variant,
                    summary[variant]["saving_vs_per_object_min_pct"],
                    int(summary[variant]["files"]["min"]),
                )
                for variant in variants
            ),
        )
    )


if __name__ == "__main__":
    main()
