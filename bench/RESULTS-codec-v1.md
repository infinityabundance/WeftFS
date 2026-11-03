# Codec microbenchmark — first measured result (2026-11-03)

**Receipt:** `bench/receipts/codec-20261103T172307Z.json`
**Raw log:** `bench/receipts/codec-20261103T172307Z.log`
**Receipt SHA-256:** `bb78c1815e10ce6c4e8a86d653744d3ae0b7e691bd53ce43257bfe98bfd3b6cc`
**Court:** `weft/rust-court:1` (image id `sha256:c96472367ec767612a78d1809c2a8c8cf9244c4ed9757dbe1bd821eb3a723b9d`),
`--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`, kernel
`7.3.1-1-cachyos`.
**Codec revision:** `a60d9f55bc38e209c6e0cbef2e04ac7b9eb7d1fe`.
**Iterations:** 7 encodes and 7 decodes per case; every decode was checked
byte-for-byte against the input before timing was recorded.
**Corpora:** deterministic (xorshift seed recorded in the example); sizes and
input hashes are in the receipt.

This is a **single-host microbenchmark**. It is not a comparison against an
external zstd binary, not a filesystem benchmark, and not a claim that WeftCodec
is faster or smaller than any competitor. Times are medians of raw nanosecond
samples; the full samples are in the receipt.

## Selected results

Frame bytes include the 100-byte WFT1 framing overhead. "Encode/decode" are
medians in milliseconds.

| Corpus | Mode (level) | Input bytes | Frame bytes | Encode ms | Decode ms |
|---|---|---:|---:|---:|---:|
| random 4 MiB | RAW | 4,194,304 | 4,194,404 | 0.651 | 0.500 |
| random 4 MiB | ZSTD (3) | 4,194,304 | 4,194,510 | 0.947 | 0.498 |
| random 4 MiB | ZSTD (19) | 4,194,304 | 4,194,509 | 107.424 | 0.499 |
| repetitive 4 MiB | RAW | 4,194,304 | 4,194,404 | 0.650 | 0.496 |
| repetitive 4 MiB | ZSTD (3) | 4,194,304 | **545** | 0.685 | 0.608 |
| repetitive 4 MiB | ZSTD (19) | 4,194,304 | 508 | 4.665 | 0.606 |
| near-dup 1 MiB | RAW | 1,048,576 | 1,048,676 | 0.172 | 0.131 |
| near-dup 1 MiB | ZSTD (3) | 1,048,576 | 1,048,709 | 0.256 | 0.133 |
| near-dup 1 MiB | DELTA vs base | 1,048,576 | 79,984 (marginal) | 36.883 | 0.439 |
| near-dup 1 MiB | DICTIONARY (3) | 1,048,576 | 24,733 (marginal) | 1.778 | 0.451 |

## Honest interpretation

1. **Incompressible data:** ZSTD does not beat RAW; the frame is 106 bytes
   larger than RAW's 100-byte overhead. `auto` must select RAW here, and the
   step-11 showcase verifies that it does.
2. **Highly repetitive data:** ZSTD is dramatically smaller at level 3 (545
   bytes vs 4,194,404) with a small encode-time cost.
3. **Near-duplicate data, marginal bytes:** DELTA (79,984) and DICTIONARY
   (24,733) are far smaller than RAW/ZSTD frames, but they **require the base
   or dictionary**:
   - DELTA whole-pair bytes = 1,048,576 (base) + 79,984 = 1,128,560.
   - DICTIONARY whole-pair bytes = 1,048,576 (base/dict) + 24,733 = 1,073,309.
   - ZSTD (3) alone = 1,048,709.
   Therefore, on this corpus and with the base counted, context conditioning
   **does not reduce whole-pair bytes** versus plain ZSTD. It only helps when
   the base/dictionary is already retained for another reason (shared
   dependency) and its cost is amortized. The showcase's step 7 reports the
   same conditional accounting rather than a miraculous ratio.
4. **Encoder cost:** the current DELTA matcher is a bounded heuristic and is
   much slower to encode (36.9 ms) than zstd level 3 (0.26 ms) on this input.
   That is a real tradeoff, not a win to hide.
5. **Level 19 is not free:** 107 ms to encode 4 MiB of incompressible data,
   with no size benefit. Tuned level selection matters.

## What is not measured

- External zstd/LZ4/Brotli/gzip/XZ binaries; randomized/interleaved runs;
  CPU cycles/energy; tail latency for ranged reads; filesystem I/O; context
  query latency; whole-repository shared-dependency accounting at scale.
- The Go codec benchmark court (a separate matched court is required before
  cross-language performance statements).
- Any workload with a genuinely shared, already-retained dictionary.

These remain explicit P7 gaps in `docs/STATUS.md`. No headline performance
claim is made from this first court.
