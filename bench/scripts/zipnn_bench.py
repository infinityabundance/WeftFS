#!/usr/bin/env python3
"""ZipNN lossless file baseline for the real-model court.

ZipNN 0.5.x ships only a source distribution for CPython 3.11/Linux, so the
bench court (which has a compiler) bootstraps pip, installs a CPU-only torch,
and builds the extension. The baseline itself is byte-level and lossless: each
safetensors file is streamed through `ZipNN(input_format="byte")` as bfloat16
words with 4 MiB streaming chunks.

The 3 GiB court limit matters here, so the source and the compressed file are
`mmap`-ed rather than copied, and verification hashes the returned buffer in
place. All reported times are wall clock; peak RSS is the process high-water
mark.

Usage (inside the court, after installing zipnn):
  python3 bench/scripts/zipnn_bench.py --file model.safetensors \
      --compressed out/model.safetensors.znn --out out/zipnn.json
"""

import argparse
import hashlib
import json
import os
import resource
import sys
import time


def sha256_file(path, chunk=1 << 20):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while True:
            block = handle.read(chunk)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--file", required=True)
    parser.add_argument("--compressed", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--streaming-chunk", type=int, default=4 << 20)
    parser.add_argument("--dtype", default="bfloat16")
    parser.add_argument("--label", default="")
    args = parser.parse_args()

    import importlib.metadata
    import zipnn
    from zipnn import ZipNN

    version = importlib.metadata.version("zipnn")
    source_bytes = os.path.getsize(args.file)
    source_sha = sha256_file(args.file)

    def make():
        # `method="HUFFMAN"` is the default of ZipNN's own shipped
        # `zipnn_compress_file.py` script. ZipNN's byte path mutates the input
        # buffer in place (zero-copy), so every buffer handed to it is a
        # mutable `bytearray` and is never reused after the call.
        return ZipNN(
            method="HUFFMAN",
            input_format="byte",
            bytearray_dtype=args.dtype,
            is_streaming=True,
            streaming_chunk=args.streaming_chunk,
            threads=4,
        )

    with open(args.file, "rb") as handle:
        source = bytearray(handle.read())
    compress_start = time.perf_counter()
    compressed = make().compress(source)
    compress_s = time.perf_counter() - compress_start
    del source
    if not isinstance(compressed, bytearray):
        compressed = bytearray(compressed)
    with open(args.compressed, "wb") as handle:
        handle.write(compressed)

    with open(args.compressed, "rb") as handle:
        compressed_input = bytearray(handle.read())
    decompress_start = time.perf_counter()
    restored = make().decompress(compressed_input)
    decompress_s = time.perf_counter() - decompress_start
    del compressed_input

    restored = memoryview(restored)
    digest = hashlib.sha256(restored)
    restored_sha = digest.hexdigest()
    restored_bytes = len(restored)
    restored_path = args.compressed + ".restored"
    materialize_start = time.perf_counter()
    with open(restored_path, "wb") as handle:
        handle.write(restored)
    materialize_seconds = time.perf_counter() - materialize_start
    restored.release()

    peak_rss_kib = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    receipt = {
        "schema": "weft.real-model.baseline.zipnn.v1",
        "baseline": "ZipNN",
        "label": args.label,
        "version": version,
        "zipnn_module": getattr(zipnn, "__file__", "unknown"),
        "configuration": {
            "method": "HUFFMAN",
            "input_format": "byte",
            "bytearray_dtype": args.dtype,
            "is_streaming": True,
            "streaming_chunk": args.streaming_chunk,
            "threads": 4,
        },
        "source": args.file,
        "source_bytes": source_bytes,
        "source_sha256": source_sha,
        "compressed_bytes": len(compressed),
        "compressed_sha256": sha256_file(args.compressed),
        "compress_seconds": round(compress_s, 3),
        "decompress_seconds": round(decompress_s, 3),
        "materialize_seconds": round(materialize_seconds, 3),
        "restore_total_seconds": round(decompress_s + materialize_seconds, 3),
        "restored_bytes": restored_bytes,
        "restored_sha256": restored_sha,
        "restored_matches_source": restored_sha == source_sha and restored_bytes == source_bytes,
        "ratio": round(len(compressed) / source_bytes, 6),
        "peak_rss_kib": peak_rss_kib,
    }
    os.unlink(restored_path)
    with open(args.out, "w") as handle:
        json.dump(receipt, handle, indent=2)
        handle.write("\n")
    print(json.dumps(receipt))
    return 0 if receipt["restored_matches_source"] else 2


if __name__ == "__main__":
    sys.exit(main())
