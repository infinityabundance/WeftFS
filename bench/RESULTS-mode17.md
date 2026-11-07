# Mode 17 `FLOAT_TILED`: independently decodable float tiles

Receipts:

* `bench/receipts/float-tiled-20261107T042955Z.json` — real Qwen BF16
  embedding, mode 9 vs mode 17.
* `bench/receipts/access-pattern-20261107T044101Z.json` — planner selections.
* `bench/receipts/restore-pipeline-20261107T045603Z.json` — 988 MB restore at
  1/2/4 workers, batched vs pipelined (mode-9/delta store).
* `bench/receipts/float-tiled-20261107T042222Z.json` — earlier run of the same
  court, superseded by `042955Z` (same conclusions, slightly different times).

All runs are in the bench court (3 GiB, 4 CPUs, pids 1024). The corpus
(`Qwen/Qwen2.5-0.5B`, BF16) is re-hashed in the court against
`88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`. Single
host, warm caches, one public tensor; not a model-wide or fleet claim.

## 1. Wire facts

```
schema          weft.ai.float_tiled.v1
mode            17   (reserved reject moved to 18)
tile_root       H("weft/tensor/float_tile/v1",
                  WCB({dtype, dims, element_len,
                       exponent_plane_len, mantissa_plane_len, values_hash}))
values_hash     H("weft/tensor/values/v1", decoded tile bytes)
logical_root    WEFT-TENSOR LogicalTensorRoot of the whole tensor
payload         per tile: huffman(exponent_plane) || raw(sign+mantissa)
```

Descriptor keys: `schema, dtype, shape, element_count, layout, tile_shape,
tile_counts, exponent_coder, tiles, payload_len, logical_root`.
Per-tile keys: `index, dims, offset, compressed_len, exponent_plane_len,
mantissa_plane_len, element_len, tile_root`.
Only full tiles; `tile_shape` must divide every extent; offsets must be
contiguous and cover `payload_len`; each tile's exponent stream must be at
least the 256-byte code-length table. Supported dtypes are bf16/fp16/fp32.

Conformance: `conformance/v1/float-tiled/` has **3 valid + 8 reject** vectors,
byte-identical in `WeftCodec` and `WeftCodec-Go`; the aggregate manifest has
338 entries and the coverage gate reports `17:3/8`. The cross-language gate
prints `YES byte-identical float_tiled_bf16` and `float_tiled_fp32`, and both
directions decode exactly.

## 2. Real Qwen BF16 embedding (`[151936, 896]`, 272,269,312 B)

Tile shape `[64, 896]` → 2,374 tiles.

| metric | mode 9 (`FLOAT_TENSOR`) | mode 17 (`FLOAT_TILED`) |
|---|---:|---:|
| physical frame bytes | 180,623,816 | 181,541,557 (**+0.51%**) |
| write (in-memory encode) | 2,287 ms | 7,435 ms (**3.25x slower**) |
| whole read, 1 thread | 745.7 ms (p99 815.5) | **486.6 ms** (p99 500.7) |
| whole read, 4 threads | 740.2 ms (p99 748.1) | **235.9 ms** (p99 248.4) |
| 100-row read, 1 thread | 777.1 ms (p99 790.4) | **2.30 ms** (p99 2.56) |
| 100-row read, 4 threads | 758.7 ms (p99 783.7) | 2.55 ms (p99 2.82) |
| 1-row read, 1 thread | 749.3 ms (p99 751.8) | **2.12 ms** (p99 2.12) |
| 1-row read, 4 threads | 741.9 ms (p99 747.8) | 2.43 ms (p99 2.44) |
| region `units_touched` | 1 (whole decode each time) | 2 (100 rows) / 1 (1 row) |

Every read was checked byte-identical to the source tensor.

Reading the table:

* **Region reads are the win**: 337x (100 rows) and 353x (1 row) at one thread.
  This is the reviewer's "272 MB embedding remains a large serial component":
  mode 9 cannot seek, so every region read decodes 272 MB.
* **Whole decode is also faster** (1.53x at one thread, 3.14x at four). Part of
  that is structural: mode 17 verifies per-tile roots and the logical root, while
  mode 9 additionally hashes the ~180 MB stored plane pair. Both verify the
  whole-content hash.
* **Mode 17 is not free**: +0.51% bytes, 3.25x write time, and 2,374 per-tile
  Huffman tables in the descriptor. The descriptor is part of
  `physical_bytes`; on tensors with tiny tiles it dominates.
* 4 threads do not help a 100-row/1-row read because only 1–2 tiles are decoded;
  the pool overhead cancels the gain (2.30 → 2.55 ms).

## 3. Access-pattern planner (real embedding)

`weft plan --tile-shape 64,896 --access-pattern ...`, measured cost table,
`lambda_ns` = bytes charged per estimated nanosecond.

| pattern | lambda | chosen | bytes | estimated decode ns |
|---|---:|---|---:|---:|
| `whole` | 0 | `float-tensor` (mode 9) | 180,623,816 | 1,216,176,993 |
| `whole` | 1 | `raw` | 272,269,312 | 79,731,159 |
| `region:1792:1000` (1 row × 1000) | 1 | **`float-tiled`** | 181,541,557 | 19,884,000 |
| `region:179200:1000` (100 rows × 1000) | 1 | **`float-tiled`** | 181,541,557 | 198,840,000 |
| `region:1792:1` | 1 | `float-tiled` | 181,541,557 | 19,884 |
| `region:272269312:100` (whole × 100) | 1 | `raw` | 272,269,312 | 7,973,115,900 |
| `mixed:0:1792:1000` | 1 | **`float-tiled`** | 181,541,557 | 19,884,000 |

Why: at a region pattern a tile-independent representation is charged only for
the fetched bytes (`region_bytes`, clamped to the tensor) per read; mode 9,
zstd and raw are charged a whole-tensor decode **per read**. At `whole` the
decode term is the full-tensor estimate, and with `lambda_ns = 1` the raw
representation wins because it has almost no decode work — the model prices
`raw` at 318,321 ns/MiB versus 4,683,726 for mode 9. That is an honest loss of
the model, not a planner bug: it says a decode-latency-only objective should
choose the format with no decode.

**Where the pattern-aware plan is worse.** The objective has no write term.
For a *single* region read (`region:1792:1`) the estimate still picks mode 17
(cost 181,561,441), but measured end-to-end it is worse than mode 9:

* mode 9: 2,287 ms write + 749 ms read = **3,036 ms**
* mode 17: 7,435 ms write + 2.12 ms read = **7,437 ms** (+4.4 s)

Mode 17 pays off after roughly seven 1-row reads. Until then the planner's
answer is worse than bytes-only. A write-cost or break-even term is future
work.

**Defaults are unchanged.** `AccessPattern::Whole` is the default and the
decode term is exactly the previous full-tensor estimate; a test asserts the
default plan and the explicit `Whole` plan are equal in chosen strategy, bytes,
estimated ns, cost, and every candidate (Rust) and the Go parity test selects
the same strategies on the same fixture: `Whole` → `float-tensor`,
`Region` → `float-tiled`.

## 4. Parallel intra-tensor decode and the restore pipeline

Mode-17 tiles are independent units, so the store decodes them on the existing
bounded pool (`TileDecodeOptions`) and region reads fetch only intersecting
tile byte ranges. Measured: whole embedding 486.6 → 235.9 ms at 4 workers
(2.06x); 1-row/100-row regions do not parallelise (1–2 tiles).

**Mode 9 has no sound intra-frame split.** A mode-9 frame is one sequential
`huffman-canonical-v1` stream; symbol `n+1` starts at the bit where symbol `n`
ended. Chunking is disallowed for AI modes and there is no independent
sub-stream. We did not fake one. The available parallelism for mode 9 is across
tensors, and it already exists.

988,097,824-byte restore (cached planner-float store, 290 tensors), tmpfs
destination (writeback-free), one repetition:

| workers | batched read (ms) | pipelined total (ms) | pipeline decode wall (ms) | write (ms) |
|---:|---:|---:|---:|---:|
| 1 | 2,822.4 | 2,764.5 | 2,643.5 | 120.4 |
| 2 | 1,837.0 | 1,678.8 | 1,677.1 | 180.9 |
| 4 | 1,420.8 | 1,317.9 | 1,316.2 | 225.4 |

**`total ≈ decode` moved**: at 1 worker decode is 95.6% of the pipeline total;
at 4 workers it is 99.9% (writes fully overlapped). Decode itself fell 2.0x
from 1 to 4 workers. The disk destination is host-writeback dependent and is
reported in the receipt but not used for this conclusion (2- and 4-worker disk
runs are 10–14 s because of writeback contention; R2 recorded the same).

A whole-model mode-17 checkpoint was **not** produced: one tile shape must
divide every tensor's extents and ranks, and the model mixes rank-1 layernorms
with rank-2 matrices. The mode-17 restore evidence is therefore the real
embedding plus the checkpoint test in `tests/float_tiled_store.rs`, not a
290-tensor mode-17 rewrite. This is a real gap.

## 5. Losses and non-claims

* Mode 17 costs +0.51% bytes on the real embedding and 3.25x the write time.
* Per-tile Huffman tables grow the descriptor with the tile count; tiny tiles
  can make mode 17 much larger than mode 9 (not measured on a full model).
* The planner has no write-cost term, so it recommends mode 17 for a single
  region read where end-to-end it loses (numbers above).
* Region estimates use `region_bytes / decoded_bytes`, ignoring per-tile
  rounding, so they under-price a region that touches many small tiles.
* No whole-model mode-17 checkpoint; no Go region fetch that reads only tile
  ranges (Go decodes the whole object then slices); no cold cache, GPU, or
  fleet claim.
* Go's planner selects the same strategies on the same fixture, but the Go
  store has no checkpoint writer for mode 17.
