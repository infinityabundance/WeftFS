#!/usr/bin/env python3
"""Load a tiled-restored container through the unmodified `safetensors` library.

The mounted checkpoint may be a filtered (one-tensor) restore, so unlike
`mount_loader.py` this script compares each mounted tensor with the same tensor
in the source container rather than requiring the whole-file SHA-256 to match.
The library is used exactly as shipped; no monkey-patching or local reader is
involved.
"""

import argparse
import hashlib
import json
import resource
import struct
import sys
import time

import ml_dtypes  # noqa: F401  registers bfloat16 with NumPy
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
        header_len = struct.unpack("<Q", prefix)[0]
        header = json.loads(handle.read(header_len))
    return header, 8 + header_len


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
    with safe_open(args.mounted, framework="numpy") as handle:
        library_keys = sorted(handle.keys())

    mounted_header, mounted_start = parse_header(args.mounted)
    source_header, source_start = parse_header(args.source)

    records = []
    mismatches = []
    for name in library_keys:
        entry = mounted_header[name]
        expected = source_header.get(name)
        if expected is None:
            mismatches.append({"tensor": name, "reason": "missing from the source"})
            continue
        begin, end = entry["data_offsets"]
        expected_begin, expected_end = expected["data_offsets"]
        raw = read_range(args.mounted, mounted_start + begin, end - begin)
        reference = read_range(args.source, source_start + expected_begin, expected_end - expected_begin)
        dtype = NPDTYPES.get(entry["dtype"])
        materialised = False
        if dtype is not None:
            array = np.frombuffer(raw, dtype=dtype)
            materialised = array.size == int(np.prod(entry["shape"], dtype=np.int64))
        matches = (
            raw == reference
            and entry["dtype"] == expected["dtype"]
            and entry["shape"] == expected["shape"]
            and materialised
        )
        if not matches:
            mismatches.append({"tensor": name, "reason": "byte/metadata mismatch"})
        records.append(
            {
                "tensor": name,
                "dtype": entry["dtype"],
                "shape": entry["shape"],
                "bytes": len(raw),
                "tensor_sha256": hashlib.sha256(raw).hexdigest(),
                "source_tensor_sha256": hashlib.sha256(reference).hexdigest(),
                "numpy_materialised": materialised,
                "matches_source": matches,
            }
        )

    result = {
        "mounted_path": args.mounted,
        "source_path": args.source,
        "mounted_sha256": sha256_file(args.mounted),
        "source_sha256": sha256_file(args.source),
        "safetensors_version": __import__("safetensors").__version__,
        "numpy_version": np.__version__,
        "ml_dtypes_version": ml_dtypes.__version__,
        "library_keys": library_keys,
        "tensor_count": len(records),
        "all_tensors_materialised": all(r["numpy_materialised"] for r in records),
        "all_tensors_match_source": all(r["matches_source"] for r in records),
        "mismatches": mismatches,
        "ok": bool(records) and not mismatches,
        "elapsed_ms": (time.time() - started) * 1000.0,
        "peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        "tensors": records,
    }
    with open(args.json, "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(
        "mount-loader-tiled: ok={ok} tensors={count} materialised={mat}".format(
            ok=result["ok"],
            count=result["tensor_count"],
            mat=result["all_tensors_materialised"],
        )
    )
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
