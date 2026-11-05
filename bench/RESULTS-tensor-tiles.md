# Tensor tiles (mode 15) — region-read results

Receipt: `bench/receipts/tensor-tiles-20261105T222531Z.json`
Log: `bench/receipts/tensor-tiles-20261105T222531Z.log`
Court: `scripts/docker-court.sh fuse "bash bench/scripts/run_tensor_tiles_court.sh"`
Label: **single-host Docker fuse court, one real public Qwen2.5-0.5B BF16
tensor, in-process store API plus a FUSE mount read by the unmodified
`safetensors` library; not a fleet, multi-host, or remote-store claim.**

## Question

A mode-10 checkpoint region read already reads only the shard objects the
region intersects, but it decodes each intersecting shard in full. Q2 asks
whether independently decodable tiles remove that whole-shard decode.

## Fixture

| Field | Value |
|---|---|
| Container | `Qwen/Qwen2.5-0.5B` `model.safetensors` (cached, LFS-verified in P2/P3) |
| Tensor | `model.layers.0.mlp.down_proj.weight` |
| dtype / shape | `bf16` / `[896, 4864]` |
| Logical bytes | 8,716,288 |
| Tile shape | `[32, 256]` (tile counts `[28, 19]`, 532 tiles) |
| Checkpoint | two single-shard checkpoints: default mode 10 and mode 15 |
| Regions | 100 rows `[0,100) × [0,4864)` and 1 row `[0,1) × [0,4864)` |

Both checkpoints store the same logical tensor and report the same
`LogicalTensorRoot`; the 100-row restore bytes from the two strategies are
byte-identical (asserted in the harness).

## Method

`examples/bench_tensor_tiles.rs` writes both checkpoints through the shipped
`Store::write_checkpoint` API. For each region and strategy it performs one
warm-up `Store::restore_checkpoint_bytes` call, then 10 timed calls. `p50` is
the lower median (sorted index `len/2`). `store_bytes_read` is the backend
counter for the last timed call; `units_touched` is the restore report's
physical-unit count (shard for mode 10, decoded tile for mode 15). The mount
phase restores the tiled tensor into the store and reads
`bench-mount/down.safetensors` through a FUSE mount with the unmodified
`safetensors` 0.8.0 / NumPy 2.4.6 / ml_dtypes 0.6.0 stack.

## Results

| Region | Mode | p50 | Backend bytes read | Units touched |
|---|---|---:|---:|---:|
| 100 rows | mode 10 | 130.131 ms | 7,213,077 | 1 (shard) |
| 100 rows | mode 15 | 12.052 ms | 934,413 | 76 (tiles) |
| 1 row | mode 10 | 129.979 ms | 7,213,077 | 1 (shard) |
| 1 row | mode 15 | 3.218 ms | 281,816 | 19 (tiles) |

| Region | Speedup | Backend byte reduction |
|---|---:|---:|
| 100 rows | 10.8× | 87.0% |
| 1 row | 40.4× | 96.1% |

The 1-row region is 100× narrower than the 100-row region but mode 15 still
touches 19 tiles: `tile_shape[1] = 256` means one row spans 19 column tiles,
and `tile_shape[0] = 32` means a one-row slice still decodes the full 32-row
tile band. Tile shape therefore sets the read floor; it is a tunable, and a
smaller first extent trades descriptor size and per-tile Huffman tables for a
smaller one-row floor.

## Mount loader

`bench/scripts/mount_loader_tiled.py` uses the unmodified `safetensors` library
(`safe_open`) to enumerate and materialise the restored container through the
mount. The checkpoint is filtered to one tensor, so whole-file SHA-256 differs
by construction; the loader compares each mounted tensor with the same source
tensor instead.

| Field | Value |
|---|---|
| `safetensors` / NumPy / ml_dtypes | 0.8.0 / 2.4.6 / 0.6.0 |
| Library keys | 1 |
| Materialised with NumPy | yes |
| Tensor SHA-256 (mounted = source) | `db708f9a857fe3b09bdd61078acbcb6d5d4b58987364cdcfe8339d7eefd572c6` |
| Source container SHA-256 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |

## What this supports, and what it does not

Supported:

* Mode 15 decodes only intersecting tiles; mode 10 decodes the whole shard.
* The region output is exact and equal between the two strategies.
* Per-tile roots and the descriptor's logical root are verified on every read.
* The tiled-restored container is readable by the unmodified `safetensors`
  library through the mount.

Not claimed:

* No fleet, concurrent-mount, remote-object-store, or network result.
* No claim that tile decoding is faster than zstd per byte; the win here is
  reading and decoding fewer bytes, not a faster inner loop.
* No partial boundary tiles; a tile shape must divide every extent.
* The dominant absolute times are host- and cache-dependent. Only the paired
  ratios and the byte counters are the measured evidence.
* `store_bytes_read` includes the manifest/descriptor reads; it is a backend
  counter, not a hardware I/O claim.
