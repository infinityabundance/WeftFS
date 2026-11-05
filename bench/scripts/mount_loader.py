#!/usr/bin/env python3
"""Read a restored safetensors checkpoint through a mounted WeftFS tree.

The container is opened with the unmodified `safetensors` library (which
validates the header and enumerates every key) and every BF16 tensor is
materialised with NumPy + `ml_dtypes`. Each tensor's bytes are compared with the
source container's, and the whole-file SHA-256 is compared as well.

This script is the mount-path proof: it never touches the store directly, only
the FUSE mountpoint it is given.
"""

import argparse
import hashlib
import json
import os
import resource
import struct
import sys
import time

import ml_dtypes  # noqa: F401  registers the bfloat16 dtype with NumPy
import numpy as np
from safetensors import safe_open

NPDTYPES = {
    "F16": np.float16,
    "BF16": ml_dtypes.bfloat16,
    "F32": np.float32,
    "F64": np.float64,
    "I8": np.int8,
    "U8": np.uint8,
    "I16": np.int16,
    "I32": np.int32,
    "I64": np.int64,
}


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def parse_header(path):
    with open(path, "rb") as handle:
        prefix = handle.read(8)
        if len(prefix) != 8:
            raise SystemExit(f"{path}: shorter than the 8-byte header length")
        header_len = struct.unpack("<Q", prefix)[0]
        header = handle.read(header_len)
    return json.loads(header), 8 + header_len


def read_range(path, offset, length):
    with open(path, "rb") as handle:
        handle.seek(offset)
        return handle.read(length)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--mounted", required=True)
    parser.add_argument("--source", required=True)
    parser.add_argument("--json", required=True)
    args = parser.parse_args()

    started = time.time()
    mounted_sha = sha256_file(args.mounted)
    source_sha = sha256_file(args.source)

    # The unmodified library must accept the container and see every key.
    with safe_open(args.mounted, framework="numpy") as handle:
        library_keys = sorted(handle.keys())

    mounted_header, mounted_start = parse_header(args.mounted)
    source_header, source_start = parse_header(args.source)

    mounted_tensors = sorted(k for k in mounted_header if k != "__metadata__")
    source_tensors = sorted(k for k in source_header if k != "__metadata__")
    mismatches = []
    records = []
    total_bytes = 0
    for name in mounted_tensors:
        entry = mounted_header[name]
        expected = source_header.get(name)
        if expected is None:
            mismatches.append({"tensor": name, "reason": "missing from the source"})
            continue
        begin, end = entry["data_offsets"]
        expected_begin, expected_end = expected["data_offsets"]
        raw = read_range(args.mounted, mounted_start + begin, end - begin)
        reference = read_range(
            args.source,
            source_start + expected_begin,
            expected_end - expected_begin,
        )
        dtype = NPDTYPES.get(entry["dtype"])
        materialised = False
        if dtype is not None:
            array = np.frombuffer(raw, dtype=dtype)
            materialised = array.size == int(np.prod(entry["shape"], dtype=np.int64))
        tensor_sha = hashlib.sha256(raw).hexdigest()
        reference_sha = hashlib.sha256(reference).hexdigest()
        matches = (
            raw == reference
            and tensor_sha == reference_sha
            and entry["dtype"] == expected["dtype"]
            and entry["shape"] == expected["shape"]
            and materialised
        )
        if not matches:
            mismatches.append(
                {
                    "tensor": name,
                    "reason": "byte/metadata mismatch",
                    "mounted_sha256": tensor_sha,
                    "source_sha256": reference_sha,
                }
            )
        total_bytes += len(raw)
        records.append(
            {
                "tensor": name,
                "dtype": entry["dtype"],
                "shape": entry["shape"],
                "bytes": len(raw),
                "sha256": tensor_sha,
                "numpy_materialised": materialised,
                "matches_source": matches,
            }
        )

    if len(library_keys) != len(mounted_tensors):
        mismatches.append(
            {
                "tensor": "<keys>",
                "reason": "safetensors key count differs from the header tensor count",
            }
        )

    result = {
        "mounted_path": args.mounted,
        "source_path": args.source,
        "mounted_sha256": mounted_sha,
        "source_sha256": source_sha,
        "sha256_match": mounted_sha == source_sha,
        "safetensors_version": __import__("safetensors").__version__,
        "numpy_version": np.__version__,
        "ml_dtypes_version": ml_dtypes.__version__,
        "library_keys": len(library_keys),
        "tensor_count": len(mounted_tensors),
        "tensor_bytes": total_bytes,
        "all_tensors_materialised": all(r["numpy_materialised"] for r in records),
        "all_tensors_match_source": all(r["matches_source"] for r in records),
        "mismatches": mismatches,
        "ok": not mismatches and mounted_sha == source_sha,
        "elapsed_ms": (time.time() - started) * 1000.0,
        "peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        "tensors": records,
    }
    with open(args.json, "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(
        "mount-loader: ok={ok} tensors={tensor_count} bytes={tensor_bytes} "
        "sha256_match={sha} materialised={mat}".format(
            ok=result["ok"],
            tensor_count=result["tensor_count"],
            tensor_bytes=result["tensor_bytes"],
            sha=result["sha256_match"],
            mat=result["all_tensors_materialised"],
        )
    )
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
