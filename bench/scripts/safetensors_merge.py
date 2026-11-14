#!/usr/bin/env python3
"""Merge a sharded safetensors checkpoint into one container, exactly.

Some public checkpoints are published as several `model-0000N-of-0000M`
safetensors files. The WeftFS family harness takes one container per
checkpoint, so this tool performs a *container* transformation only: it never
touches tensor bytes, dtypes or shapes, and it verifies every tensor against
the shard it came from before the output is renamed into place.

Usage:
  python3 bench/scripts/safetensors_merge.py \
      --inputs shard-a.safetensors shard-b.safetensors \
      --out merged.safetensors --manifest merged.json

Verification performed (all failures are fatal):
  * each shard has a valid 8-byte little-endian header length and JSON header;
  * tensor names are unique across shards;
  * dtype and shape are carried verbatim;
  * the source data range is inside the shard's data section;
  * every tensor's SHA-256 is identical in the source shard and the output;
  * the output's element byte total equals the sum of the source totals;
  * `__metadata__` (when present) is preserved exactly.

The manifest records each shard's path/size/sha256, the merged file's
path/size/sha256, and a per-tensor sha256 so a later court can re-verify the
merge without trusting this script's exit code.
"""

import argparse
import hashlib
import json
import os
import struct
import sys

COPY_BLOCK = 8 << 20


def read_header(path):
    size = os.path.getsize(path)
    if size < 10:
        raise SystemExit(f"{path}: too small to be safetensors")
    with open(path, "rb") as handle:
        raw = handle.read(8)
        header_len = struct.unpack("<Q", raw)[0]
        if header_len == 0 or 8 + header_len > size:
            raise SystemExit(f"{path}: invalid header length {header_len}")
        header_bytes = handle.read(header_len)
    try:
        header = json.loads(header_bytes.decode("utf-8"))
    except Exception as err:  # noqa: BLE001 - report the exact parse failure
        raise SystemExit(f"{path}: header is not valid JSON: {err}")
    if not isinstance(header, dict):
        raise SystemExit(f"{path}: header is not a JSON object")
    return header, 8 + header_len


def sha256_range(path, start, length):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        handle.seek(start)
        remaining = length
        while remaining:
            block = handle.read(min(COPY_BLOCK, remaining))
            if not block:
                raise SystemExit(f"{path}: short read at {start}+{length}")
            digest.update(block)
            remaining -= len(block)
    return digest.hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while True:
            block = handle.read(COPY_BLOCK)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--inputs", nargs="+", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--manifest", required=True)
    args = parser.parse_args()

    shards = []
    header = {}
    metadata = None
    seen = set()
    order = []
    total_bytes = 0
    for path in args.inputs:
        shard_header, data_start = read_header(path)
        shard_size = os.path.getsize(path)
        data_len = shard_size - data_start
        if "__metadata__" in shard_header:
            if metadata is not None and shard_header["__metadata__"] != metadata:
                raise SystemExit(f"{path}: conflicting __metadata__")
            metadata = shard_header["__metadata__"]
        tensor_count = 0
        for name, entry in shard_header.items():
            if name == "__metadata__":
                continue
            if not isinstance(entry, dict):
                raise SystemExit(f"{path}: tensor {name} is not an object")
            for key in ("dtype", "shape", "data_offsets"):
                if key not in entry:
                    raise SystemExit(f"{path}: tensor {name} lacks {key}")
            if name in seen:
                raise SystemExit(f"duplicate tensor name {name!r} across shards")
            seen.add(name)
            offsets = entry["data_offsets"]
            if (
                not isinstance(offsets, list)
                or len(offsets) != 2
                or not all(isinstance(v, int) for v in offsets)
                or offsets[0] < 0
                or offsets[1] < offsets[0]
                or offsets[1] > data_len
            ):
                raise SystemExit(f"{path}: tensor {name} has invalid data_offsets {offsets}")
            length = offsets[1] - offsets[0]
            header[name] = {
                "dtype": entry["dtype"],
                "shape": list(entry["shape"]),
                "data_offsets": [total_bytes, total_bytes + length],
            }
            total_bytes += length
            tensor_count += 1
            order.append((name, path, data_start + offsets[0], length))
        shards.append(
            {
                "path": path,
                "size": shard_size,
                "sha256": sha256_file(path),
                "data_start": data_start,
                "data_bytes": data_len,
                "tensors": tensor_count,
            }
        )

    out_header = {}
    if metadata is not None:
        out_header["__metadata__"] = metadata
    for name, _path, _start, _length in order:
        out_header[name] = header[name]
    header_bytes = json.dumps(out_header, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    pad = (-len(header_bytes)) % 8
    header_bytes += b" " * pad
    if len(header_bytes) >= 1 << 32:
        raise SystemExit("merged header exceeds the 32-bit header length")

    part = args.out + ".part"
    with open(part, "wb") as out:
        out.write(struct.pack("<Q", len(header_bytes)))
        out.write(header_bytes)
        for name, path, start, length in order:
            with open(path, "rb") as src:
                src.seek(start)
                remaining = length
                while remaining:
                    block = src.read(min(COPY_BLOCK, remaining))
                    if not block:
                        raise SystemExit(f"{path}: short read copying {name}")
                    out.write(block)
                    remaining -= len(block)

    expected_size = 8 + len(header_bytes) + total_bytes
    actual_size = os.path.getsize(part)
    if actual_size != expected_size:
        raise SystemExit(f"{part}: expected {expected_size} bytes, got {actual_size}")

    # Re-read the merged file and verify every tensor against its shard.
    merged_header, merged_start = read_header(part)
    rows = []
    for name, path, start, length in order:
        entry = merged_header.get(name)
        if entry is None:
            raise SystemExit(f"{part}: merged header lost tensor {name}")
        source_offset = entry["data_offsets"][0]
        merged_digest = sha256_range(part, merged_start + source_offset, length)
        source_digest = sha256_range(path, start, length)
        if merged_digest != source_digest:
            raise SystemExit(f"{name}: merged sha256 {merged_digest} != source {source_digest}")
        if entry["dtype"] != header[name]["dtype"] or entry["shape"] != header[name]["shape"]:
            raise SystemExit(f"{name}: dtype/shape changed during merge")
        rows.append(
            {
                "name": name,
                "dtype": entry["dtype"],
                "shape": entry["shape"],
                "bytes": length,
                "sha256": merged_digest,
                "source": path,
            }
        )

    if merged_header.get("__metadata__") != metadata:
        raise SystemExit(f"{part}: __metadata__ was not preserved")
    merged_total = sum(row["bytes"] for row in rows)
    if merged_total != total_bytes:
        raise SystemExit(f"{part}: merged data {merged_total} != source {total_bytes}")

    os.replace(part, args.out)
    manifest = {
        "schema": "weft.safetensors-merge.v1",
        "label": "sharded public checkpoint merged into one container; tensor bytes unchanged",
        "inputs": shards,
        "merged": {
            "path": args.out,
            "size": os.path.getsize(args.out),
            "sha256": sha256_file(args.out),
            "header_bytes": len(header_bytes),
            "data_start": merged_start,
            "tensor_count": len(rows),
            "tensor_data_bytes": merged_total,
        },
        "tensors": rows,
    }
    with open(args.manifest, "w", encoding="utf-8") as handle:
        json.dump(manifest, handle, indent=1)
        handle.write("\n")
    print(
        json.dumps(
            {
                "out": args.out,
                "size": manifest["merged"]["size"],
                "sha256": manifest["merged"]["sha256"],
                "tensors": len(rows),
                "tensor_data_bytes": merged_total,
                "shards": len(shards),
            }
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
