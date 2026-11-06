# Bounded parallel tile decode and auto-tiling (Qwen2.5-0.5B, single host)

Receipt: `bench/receipts/tiled-parallel-20261106T061820Z.json`
Harness: `bench/scripts/tiled_parallel_court.sh`,
`bench/scripts/assemble_tiled_parallel.py`,
`examples/real_model_bench.rs` (`ingest`, `tiled`)
Court: `weft/bench-court:1`, image id
`sha256:4041d66ed25626e875de58f79e6527997c85ca81541566cbabe09451b40f2041`,
kernel `7.3.1-1-cachyos`, 3 GiB, 4 vCPU, 1024 pids, no swap.
Revisions: WeftFS `44295e5`, WeftCodec `4b4c131`, WeftCodec-Go `e47fa03`,
WeftFS-Go `47fd4d2`.

Label: single host, warm page cache, one run per configuration and thread
count. No fleet, multi-node, or cold-cache claim. The mode-15 wire layout is
unchanged; this receipt measures a read scheduler and an additive planner
option.

## What was added

- `TileDecodeOptions { threads, max_inflight_bytes }`, default `threads = 1`.
  `Store::read_tensor_with_options`, `read_tensor_region_with_options`, and the
  `_and_stats` forms decode mode-15 tiles on the same bounded
  `std::thread::scope` runner as `read_tensors_bounded`: worker bound, staging
  bound, exact input order, lowest-index error, panics caught, and one worker
  runs the shipped sequential path.
- `tensor_store::derive_auto_tile_shape(shape, dtype, budget)`: a rank-2 tile
  shape whose extents divide both logical extents exactly, chosen by largest
  element count at or below `budget / element_bytes`, then the more balanced
  pair, then lexicographically. `TensorPlanOptions::auto_tile_bytes` offers
  mode 15 to the unchanged price function. The derived shape is recorded in the
  candidate `detail` and in the `weft.tensor.tile_shape` object attribute.
- `weft cat --threads N`, `weft plan --auto-tile-bytes N`,
  `weft write --auto-tile-bytes N`.
- `tests/tiled_parallel.rs`: 8 tests covering byte identity at 1/2/4 threads,
  identical tile counts, touched-tile counts for three region shapes, the
  staging bound, 20-repeat determinism at 4 threads, corrupted-tile failure,
  and auto-tile derivation/planner determinism.

## Corpus

Qwen/Qwen2.5-0.5B `model.safetensors`, BF16, 290 tensors, 988,065,536 tensor
bytes, 988,097,824 file bytes. SHA-256
`88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`, matching
`bench/corpora/real-models/qwen2.5-0.5b-pair.json` and re-hashed in the court.
Only the base checkpoint was ingested.

## Storage

| configuration | tiled tensors | tensor data B | store total B | delta vs default |
| --- | ---: | ---: | ---: | ---: |
| default planner | 0 / 290 | 658,243,887 | 674,729,302 | — |
| planner + `--auto-tile-bytes 65536` | **0 / 290** | 658,243,887 | 674,729,302 | **0 B (0.0 %)** |
| forced-tiled control | 169 / 290 | 672,554,779 | 689,845,406 | **+14,310,892 B (+2.174 %)** |

Strategy distribution (base, all three stores): float-tensor 169, raw 48,
zstd 73. The forced control replaces every float-tensor object with
tensor-tiled; raw and zstd are unchanged. Derived tile shapes in the control:
`[128,224]`, `[128,256]`, `[256,128]`.

**The auto-tile price never selected mode 15 on this corpus.** The planner is
bytes-only at `lambda = 0`, and mode 9 FLOAT_TENSOR is 2.17 % smaller than the
best dividing mode-15 encoding here. `auto_tile_bytes` therefore changes no
representation for these BF16 weights. It is offered, priced, and loses; the
default candidate set and price function were not changed.

The forced control is labelled as a control, not as planner output. It exists
because otherwise there is no tiled real-weight object to exercise the bounded
tile decoder.

## Full-checkpoint restore

Each pass reads all 290 tensors with `Store::read_tensor_with_options` in
ingest order (the 272 MB embedding is decoded alone; smaller tensors still get
the same worker count). Materialize repeats the pass while writing the exact
`model.safetensors`. The logical-root pass is `Store::tensor_logical_root`,
which has no tile options and is therefore sequential by construction.

| configuration | threads | effective | read ms | materialize ms | root-check ms | tiles | peak RSS KiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| default | 1 | 1 | 4,730.3 | 7,047.9 | 4,735.6 | 0 | 1,258,108 |
| default | 4 | 4 | 4,762.3 | 7,171.2 | 4,708.8 | 0 | 1,258,492 |
| auto-tile | 1 | 1 | 4,873.0 | 6,304.3 | 4,706.3 | 0 | 1,257,272 |
| auto-tile | 4 | 4 | 4,880.5 | 8,386.8 | 4,678.4 | 0 | 1,255,832 |
| forced-tiled | 1 | 1 | 3,038.3 | 4,215.6 | 2,915.5 | 15,860 | 1,073,448 |
| forced-tiled | 4 | 4 | **1,062.1** | **2,871.4** | 2,921.2 | 15,860 | 1,089,020 |

Ratios (4 threads vs 1):

| configuration | read | materialize | root recheck |
| --- | ---: | ---: | ---: |
| default | 0.993x | 0.983x | 1.006x |
| auto-tile | 0.999x | 0.752x | 1.006x |
| forced-tiled | **2.861x** | **1.468x** | 0.998x |

Backend bytes read in the full read pass: 658,230,303 (default and auto-tile),
672,393,555 (forced). Restored bytes are 988,065,536 in every run.

Default and auto-tile store zero mode-15 objects, so `--threads` has no tile
decode work to parallelise there. Their ratio swings (including 0.983x for
default materialize and 0.752x for auto-tile materialize) are host scheduling
and writeback noise, not speedups.

## Embedding region reads

`model.embed_tokens.weight`, BF16 `[151936, 896]`, full tensor 272,269,312 B.
In the forced control the derived tile shape is `[128, 224]`, a 1187 x 4 tile
grid. A region read fetches the frame prefix and only the stored ranges of
intersecting tiles; every touched tile root is verified. The whole-frame
trailer and content hash are deliberately not re-verified on this path.

| configuration | region | threads | ms | tiles touched | backend bytes | range reads | exact |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| default | whole | 1 | 1,340.1 | 1 | 180,625,100 | 0 | yes |
| default | whole | 4 | 1,333.9 | 1 | 180,625,100 | 0 | yes |
| default | 100 rows | 4 | 1,290.1 | 1 | 180,625,100 | 0 | yes |
| default | 1 row | 4 | 1,285.9 | 1 | 180,625,100 | 0 | yes |
| auto-tile | 100 rows | 4 | 1,296.2 | 1 | 180,625,100 | 0 | yes |
| auto-tile | 1 row | 4 | 1,298.9 | 1 | 180,625,100 | 0 | yes |
| forced-tiled | whole | 1 | 738.7 | 4,748 | 184,634,026 | 4,750 | yes |
| forced-tiled | whole | 4 | **203.0** | 4,748 | 184,634,026 | 4,750 | yes |
| forced-tiled | 100 rows | 1 | 4.11 | 4 | 730,133 | 6 | yes |
| forced-tiled | 100 rows | 4 | 3.96 | 4 | 730,133 | 6 | yes |
| forced-tiled | 1 row | 1 | 3.54 | 4 | 730,133 | 6 | yes |
| forced-tiled | 1 row | 4 | 3.67 | 4 | 730,133 | 6 | yes |

The one-row read fetches **730,133 B instead of 180,625,100 B (247.4x fewer
backend bytes)**. The whole-embedding region read improves **3.64x** at four
workers. The 100-row and 1-row regions intersect only four tiles, so four
workers do not help there; the win is seekability, not parallelism.

## Exactness

Every run:

- per-tensor BLAKE3 equals the ingest record for all 290 tensors;
- every `LogicalTensorRoot` recomputes to the recorded value;
- the reassembled file is 988,097,824 B with SHA-256
  `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`, equal to
  the source file and the corpus manifest;
- region bytes equal the same slice of the source tensor;
- effective workers are 1 and 4 for the requested counts;
- restored tensor bytes are identical across all six runs.

The receipt's `checks` object is 12/12 true.

## Losses and limits

- **Auto-tiling did not fire.** With the bytes-only price, mode 15 lost to mode
  9 on all 290 BF16 tensors. Auto-tiling adds a planner path, not a storage win,
  on this corpus.
- **Tiling costs 2.174 % tensor bytes** when forced. That is the price of
  independent tile units on these weights.
- **Materialize is still writeback bound.** The forced control improves
  1.47x at four workers, but the earlier R2 result (materialize unreliable)
  still applies; it is not a decode-only benchmark.
- **Logical-root recheck does not parallelise.** `tensor_logical_root` has no
  tile options, so the 4-thread ratio is 0.99-1.01x on every configuration.
- **Small regions do not parallelise.** Four touched tiles cannot use four
  workers; the measured differences are noise.
- **The forced control is not a planner outcome.** The required auto-tile
  configuration is the `0 / 290` row; the control is clearly labelled.
- **Rank-2 only.** `derive_auto_tile_shape` returns `InvalidArgument` for any
  other rank, so rank-1 layernorms and any rank-3 tensor stay on the planner's
  other candidates.
- Single host, warm cache, one run per cell. Absolute times are court-bound.
- No Go parity for tile decode or auto-tiling; no intra-frame block-independent
  streams beyond mode 15's tile table; no partial boundary tiles (v1 is full
  tiles only).

## Reproduce

```sh
scripts/docker-court.sh bench 'cd /work && bash bench/scripts/tiled_parallel_court.sh'
```

`bench/receipts/tiled-parallel-20261106T061820Z.json` is the committed receipt.

The script re-hashes both cached Qwen files against
`bench/corpora/real-models/qwen2.5-0.5b-pair.json`, builds
`real_model_bench`, ingests the base checkpoint into three fresh stores
(default, auto-tile, forced control), runs the `tiled` phase at 1 and 4
threads for each, and assembles the receipt. Stores and run JSONs stay under
`.docker-cache/tiled-parallel/` (gitignored).
