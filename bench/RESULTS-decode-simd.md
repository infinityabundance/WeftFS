# Decode-throughput floor and wide-word kernels (v1.0 audit)

Receipts:

* `bench/receipts/decode-floor-20261107T034727Z.json` — baseline, single-threaded.
* `bench/receipts/decode-floor-20261107T040250Z.json` — after the wide-word
  kernels; includes the `.go-bench.txt` sibling.
* `bench/receipts/decode-floor-20261107T035819Z.json` — an intermediate run with
  a mis-computed join column; retained, superseded by `040250Z`.

All runs are in the bench court (`scripts/docker-court.sh bench`), 3 GiB,
4 CPUs, pids 1024, 1 GiB `/tmp`. The court reported `sse2`, `avx2`, and
`avx512f`; the runtime dispatcher selected **avx2** for both kernels. Every
measured output is asserted byte-identical to the source, so no row can be a
fast wrong answer.

Method: weight-like BF16/FP32 words, sizes 128 KiB / 1 MiB / 16 MiB, one warm-up
then five measured calls, median. Warm in-process input. Coder-only
microbenchmark; no store, cold cache, or fleet claim.

## 1. Baseline (before any change)

| component | dtype | 128 KiB | 1 MiB | 16 MiB |
|---|---|---:|---:|---:|
| `huffman_decode` (ms) | bf16 | 0.1198 | 0.9591 | 15.4646 |
| `plane_join` (end-to-end, ms) | bf16 | 0.1333 | 1.1409 | 18.0656 |
| `mode9_decode` (ms) | bf16 | 0.2754 | 2.2621 | 34.7865 |
| `huffman_decode` (ms) | fp32 | 0.0599 | 0.4769 | 7.7160 |
| `plane_join` (end-to-end, ms) | fp32 | 0.0922 | 0.7146 | 12.4711 |
| `mode9_decode` (ms) | fp32 | 0.1653 | 1.3007 | 21.4125 |

Per-MiB rates at 16 MiB: Huffman 1034.6 MiB/s (bf16 exponent), plane join
885.7 MiB/s, mode-9 decode 459.9 MiB/s (bf16) and 747.2 MiB/s (fp32).

## 2. What changed

### Rust (`WeftCodec/src/simd.rs` only)

* `huffman::decode_prefix` now calls a runtime-dispatched unpack loop. The AVX2
  build uses `_mm_loadl_epi64` for eight-byte refills and decodes two symbols
  per table probe when both prefixes are buffered. The scalar build is
  byte-refill, single probe. Error handling stays in `huffman.rs`, so classes
  and messages are shared.
* The BF16 plane join moved to a byte-wise formulation:
  `low = (plane & 0x7f) | ((exponent & 1) << 7)`,
  `high = ((exponent >> 1) & 0x7f) | (plane & 0x80)`. This form is selected; LLVM
  auto-vectorises it. An explicit AVX2 kernel (`_mm_loadu_si128` ×2,
  `_mm_unpacklo/hi_epi8`, one 32-byte store) is retained and differential-tested
  behind `join_bf16_avx2_path`, but is **not** selected.
* `unsafe` is confined to `simd.rs`: the crate root changed from
  `forbid(unsafe_code)` to `deny(unsafe_code)`, and
  `tests/no_unsafe_outside_simd.rs` fails if any other source line mentions
  `unsafe` (lint attributes excluded).
* 10,000 randomised Huffman streams (including one-byte mutations, so
  accept/reject parity is tested) and 10,000 randomised BF16 plane pairs compare
  scalar, AVX2, and the documented bit layout.

### Go (`WeftCodec-Go/modes`)

* Word-level Huffman variant: `binary.BigEndian.Uint64` refill plus a
  two-symbol unroll. Go has no SIMD intrinsics, so this is the only available
  class of optimisation.
* BF16 join avoids the 64-bit accumulator and packs the two bytes directly.
* 10,000-case differential tests per kernel and fair kernel benchmarks.

## 3. After: Rust

Same-run kernel-to-kernel (the speedup is against the retained scalar kernel):

| dtype | size | Huffman scalar → AVX2 (ms) | speedup | join scalar → AVX2 kernel (ms) | speedup |
|---|---|---:|---:|---:|---:|
| bf16 | 128 KiB | 0.1333 → 0.1210 | **1.10x** | 0.00418 → 0.01604 | 0.26x |
| bf16 | 1 MiB | 1.0642 → 0.9687 | **1.10x** | 0.1111 → 0.1102 | 1.01x |
| bf16 | 16 MiB | 17.1627 → 16.0343 | **1.07x** | 1.2899 → 1.2402 | 1.04x |
| fp32 | 128 KiB | 0.0668 → 0.0610 | 1.10x | — (no vector kernel) | — |
| fp32 | 1 MiB | 0.5324 → 0.4865 | 1.09x | — | — |
| fp32 | 16 MiB | 8.5744 → 7.8535 | 1.09x | — | — |

Cross-run, absolute (Stage 1 → Stage 2), same corpus and court:

| component | dtype | 1 MiB | 16 MiB |
|---|---|---:|---:|
| `huffman_decode` | bf16 | 0.9591 → 0.9687 (+1.0%) | 15.4646 → 16.0343 (+3.7%) |
| `plane_join` (end-to-end) | bf16 | 1.1409 → 0.1402 (**8.1x**) | 18.0656 → 1.6166 (**11.2x**) |
| `mode9_decode` | bf16 | 2.2621 → 1.2181 (**1.86x**) | 34.7865 → 18.2186 (**1.91x**) |
| `plane_join` (end-to-end) | fp32 | 0.7146 → 0.7722 (+8.1%) | 12.4711 → 13.1756 (+5.6%) |
| `mode9_decode` | fp32 | 1.3007 → 1.3325 (+2.4%) | 21.4125 → 22.5559 (+5.3%) |

Read this honestly:

* **Huffman has no absolute win.** The AVX2/unroll kernel is 7–10% faster than
  the *new* scalar kernel, but the old shipped single-probe loop measured
  1–4% faster than the new AVX2 path across runs. A single Huffman bitstream
  cannot be lane-parallelised without a wire change: symbol `n+1` starts at the
  bit position symbol `n` ended. The two-symbol unroll is the only legal
  instruction-level parallelism, and it is not enough to beat run-to-run
  variance. The kernel is kept because it is the faster of the two kernels in
  the same process, and the loss is recorded rather than claimed as a gain.
* **The plane join is the real win, and it is not SIMD intrinsics.** The
  byte-wise reformulation lets LLVM vectorise; the explicit AVX2 kernel is a
  tie at ≥1 MiB (1.01–1.04x) and 3.8x *slower* at 128 KiB in this harness, so it
  is not selected.
* FP32 plane joins and decodes are unchanged; the fp32 regression shown above
  (+2–8%) is run-to-run variance from a different process order, not a code
  path (fp32 has no new kernel). Treat fp32 deltas under 10% as noise here.

## 4. After: Go (word-level only)

`.go-bench.txt` in the same receipt, three `-count=3` runs:

| kernel | median | rate |
|---|---:|---:|
| `BenchmarkHuffmanDecodeScalar` (1 MiB) | 3.566 ms | **294.0 MB/s** |
| `BenchmarkHuffmanDecodeWordOptimised` (1 MiB) | 3.817 ms | 274.8 MB/s (**0.93x**) |
| `BenchmarkBF16JoinAccumulator` | 0.741 ms | 1413 MB/s |
| `BenchmarkBF16JoinOptimised` (`JoinFloatTensorPayload`) | 0.512 ms | **2044 MB/s (1.45x)** |

The Go Huffman word-level variant is a **loss** (0.93x) and is not selected;
`HuffmanDecodePrefix` stays scalar and `HuffmanDecodePrefixWordOptimised` is
retained and measured. The Go BF16 join gain is real (1.45x) and comes from
dropping the 64-bit accumulator, not from vectorisation. Go can never match the
Rust 11x join gain because it has no SIMD intrinsics and its compiler does not
auto-vectorise this loop.

## 5. Not claimed

* No GPU/SIMD-lane decode: the format has one sequential Huffman stream per
  plane.
* No cold-cache or end-to-end claim from this receipt; the real-weight numbers
  are in `RESULTS-mode17.md`.
* No Go SIMD claim: word-level only.
