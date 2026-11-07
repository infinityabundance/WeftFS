# Mode 18 `FLOAT_TILED_STREAMS` — measured result

Wire: mode 18, schema `weft.ai.tensor_tiled_streams.v1`, tile root domain
`weft/tensor/float_tile_streams/v1`. Additive and post-v1.1.0; modes 0–17 are
unchanged.

Receipt: `bench/receipts/mode18-20261107T165215Z.json` (raw harness output,
environment, calibration, every row). Court: `weft/bench-court:1`
(`sha256:4041d66e…`), 3 GiB memory/swap, 4 CPUs, 1024 pids. No GPU is present
and no GPU path was executed.

## What was measured

Real Qwen2.5-0.5B BF16 embedding, tensor `model.embed_tokens.weight`,
shape `[151936, 896]`, 272,269,312 logical bytes (259.66 MiB), file SHA-256
`88c14255…0ed342`. Tile `[64, 896]`. Modes 9 (`FLOAT_TENSOR`), 17
(`FLOAT_TILED`), and 18 at `n_streams` = 1, 2, 4, 8. Whole decode at 1 and 4
tile workers; region reads of the whole tensor, 100 rows, and 1 row. Times are
medians of three repetitions after one warmup decode; every decode was compared
byte-for-byte with the source tensor.

## Whole decode

| Representation | Bytes | Bytes vs mode 17 | 1 thread | 4 threads |
|---|---:|---:|---:|---:|
| mode 9 `FLOAT_TENSOR` | 180,623,816 | −0.51 % | 840.1 MiB/s | 843.6 MiB/s (unthreaded) |
| mode 17 `FLOAT_TILED` | 181,541,557 | — | 715.9 MiB/s | **1,828.3 MiB/s** |
| mode 18, n=1 | 181,598,555 | +0.03 % | 602.1 MiB/s | 1,800.4 MiB/s |
| mode 18, n=2 | 182,211,848 | +0.37 % | 453.5 MiB/s | 1,501.0 MiB/s |
| mode 18, n=4 | 183,446,037 | +1.05 % | 428.2 MiB/s | 1,390.3 MiB/s |
| mode 18, n=8 | 185,923,029 | +2.41 % | 408.2 MiB/s | 1,342.7 MiB/s |

Encode time (mode 18, n=4): 1,522.7 ms vs 1,167.0 ms mode 17 (1.30x slower).

## Region reads

| Representation | 100 rows (179,200 B) | 1 row (1,792 B) |
|---|---:|---:|
| mode 17 | 0.283 ms | **0.133 ms** |
| mode 18, n=1 | 0.416 ms | 0.256 ms |
| mode 18, n=2 | 0.515 ms | 0.321 ms |
| mode 18, n=4 | 0.434 ms | 0.336 ms |
| mode 18, n=8 | 0.440 ms | 0.395 ms |
| mode 9 | 309.078 ms (whole decode + slice) | 309.078 ms (whole decode + slice) |

Mode 18 touches every stream of each intersecting tile:
`streams_touched = tiles_touched * 2 * n_streams` (8 for n=4 on one tile,
16 for n=8). Mode 17 touches `tiles_touched` tiles.

## Verdict

**Multi-stream lost.** On this host and tensor, mode 18 is slower than mode 17
on every measured axis:

* **4 threads:** mode 17 1,828.3 MiB/s vs mode 18 n=1 1,800.4, n=2 1,501.0,
  n=4 1,390.3, n=8 1,342.7 MiB/s. Tile-parallel mode 17 wins; the four-stream
  variants lose 1.5–26.6 %.
* **1 thread:** mode 17 715.9 MiB/s vs 602.1/453.5/428.2/408.2 MiB/s
  (15.9–43.0 % slower).
* **Bytes:** +0.03 % to +2.41 % over mode 17. Each exponent stream pays its own
  256-byte table; at tile `[64,896]` the extra tables are small but never free.
* **Small regions:** the 100-row read is 1.47–1.82x slower and the 1-row read
  1.92–2.97x slower, because a tile's streams must all be set up and decoded
  before the requested rows can be extracted.

The measured cause is not the wire format: `n_streams = 1` is byte-identical to
mode 17's tile payload (proved by a unit test and by the `001…n1` vector). The
loss is decode work. The new wide-word AVX2 four-stream gather kernel decodes
one symbol per lane per probe, while the shipped per-stream AVX2 kernel decodes
two symbols per probe with ordinary loads; the gather path did not recover the
difference. Deinterleaving the exponent plane (and the raw mantissa streams for
n > 1) is a full extra pass over the tile. The four independent Huffman decodes
also defeat the two-symbol unrolling that made the shipped kernel fast.

The honest calibration row is `fixed_ns = 0`, `ns_per_mib = 3,674,824`
(single-thread, 1 warmup + 7 repetitions at 128 KiB and 16 MiB, synthetic
weight-like BF16), against mode 17's `14,131 / 3,366,772`. The bytes-only
default plan never selects mode 18. A small-region access plan *can* select it:
the fitted fixed term is 0, while mode 17's 14,131 ns/read dominates a 64-byte
region repeated 1,000 times. That choice is a model artifact — the court's
1-row measurement is 0.256 ms for mode 18 vs 0.133 ms for mode 17 — and is not
a latency claim. The standard store fixture and every whole-tensor plan are
unchanged.

## What would be needed for the reviewer's goal

The measured 1.82 GB/s is tile parallelism, not stream parallelism, and mode 17
already gets it. A multi-GB/s claim would need a kernel that decodes several
streams faster than the same bytes in one stream; this court did not produce
one. A device-memory (GPU) path was not executed and is not claimed.

## Residual

One host, one public tensor, warm caches, 3 GiB court, no GPU, no CUDA build, no
fleet measurement. The receipt records per-configuration medians, not a
distribution.
