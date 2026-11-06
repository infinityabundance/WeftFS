# Overlay court: chunk copy-on-write vs immutable write overlays

Subphase R9, stage 6. Two receipts, both at WeftFS `e9b9bae`:

- `bench/receipts/overlay-20261106T103115Z.json` — local (normative POSIX layout
  on the container filesystem), raw log
  `bench/receipts/overlay-20261106T103115Z.log`.
- `bench/receipts/overlay-s3-20261106T103120Z.json` — single-node MinIO
  `RELEASE.2025-04-22T22-12-26Z` in the same bench court, launched by
  `scripts/s3_court.sh --overlay` (court receipt
  `bench/receipts/s3-court-20261106T103120Z.json`), raw log
  `bench/receipts/overlay-s3-20261106T103120Z.log`.

Both runs: **6/6 checks pass**. Commands:

```
bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/overlay_court.sh --local"
bash scripts/s3_court.sh --overlay
```

## Workload

Deterministic and identical in both configurations and both backends:

- one object, 128 MiB (`134,217,728` B), 64 KiB chunks (2,048 chunks);
- 500 overwrites of 4,096 B at 4,096 B-aligned pseudo-random offsets
  (`seed 0x9e3779b97f4a7c15`, a separate xorshift stream for offsets and
  payloads) — 2,048,000 logical bytes written over 8 MiB of distinct offsets;
- both configurations use `content_identity = merkle`, so their content roots
  are comparable and must be equal;
- each configuration runs in its own child process (fresh store, fresh page
  cache attribution, `VmHWM` per child);
- 4 KiB range read: 20 warm repetitions plus one read through a freshly opened
  read-only handle (chain-resolution cost), then 20 more on that handle;
- one full read, `verify --full`, then compaction.

`overlay` means `RepresentationMode::Overlay`: each write publishes a
`weft.manifest.v4` layer (previous version as immutable base + one extent),
folding adjacent/overlapping extents into the current layer up to 64 extents.
`cow` is the existing Merkle chunk-COW writer: one canonical `weft.manifest.v3`
chunk table per version.

## Checks

| check | local | S3 |
|---|---|---|
| content root matches between representations | pass | pass |
| full-read bytes identical | pass | pass |
| overlay compaction preserves the content root | pass | pass |
| snapshot read unchanged by compaction (and after GC) | pass | pass |
| COW compaction is refused (`UnsupportedRepresentation`) | pass | pass |
| `verify --full` ok everywhere (before and after compaction) | pass | pass |

Content root after the 500 writes, identical in all four runs:
`c00367f0fd99b612a7800b863c66c5c959fcdbe42bafaa36b747d1f8ff164c22`.
Full-read digest before compaction, identical in all four runs:
`6db387cc5675adddae8f17ff9a8a1ba00b25499feffbc9b0d88423fd1253a3a1`.

## Local backend

| metric | COW | overlay |
|---|---|---|
| create 128 MiB | 58.2 ms | 58.4 ms |
| 500 writes, wall | 917.3 ms | **415.5 ms** |
| write p50 / p99 / max | 1.819 / 1.912 / 1.956 ms | 0.717 / 1.698 / 1.755 ms |
| backend requests (writes phase) | 3,501 | 3,993 |
| backend bytes written | 87,403,237 | **36,731,767** |
| backend bytes read (writes phase) | 85,063,500 | 43,808,669 |
| final shape | 2,048-chunk table | depth 8, 22 extents, `overlay_bytes` 32,112,640 |
| 4 KiB range read, warm p50 | 0.919 ms | **0.030 ms** |
| 4 KiB range read, cold handle first read | 0.920 ms / 2 req / 170,127 B | 5.594 ms / 10 req / 215,317 B |
| 4 KiB range read, cold handle steady p50 | 0.912 ms | 0.030 ms |
| full read before compaction | 40.6 ms / 2,049 req / 134,322,319 B | 39.9 ms / 2,049 req / 134,220,016 B |
| `verify --full` before | 53.5 ms, ok, 2,048 chunks | 52.0 ms, ok, 2,048 chunks |
| compaction | refused (`UnsupportedRepresentation`) | **34.0 ms** / 2,049 req / 134,220,016 B read, 134,217,728 B verified, 2,048 chunks |
| full read after compaction | 40.6 ms / 134,322,319 B | 40.8 ms / 134,322,319 B |
| `verify --full` after | 53.2 ms, ok | 52.9 ms, ok |
| retained blob bytes (state) before / after compaction / after GC | 134,322,319 / 134,322,319 / 134,322,319 | 166,480,149 / 134,322,319 / 134,322,319 |
| GC after compaction | 775 blobs, 70,338,845 B deleted | 501 blobs, 2,289,555 B deleted |
| on-disk bytes after GC | 149,348,604 | 166,831,597 |
| peak RSS (child, includes the 128 MiB setup buffer) | 402,348 KiB | 411,248 KiB |

## S3 backend (single-node MinIO, tmpfs data directory)

| metric | COW | overlay |
|---|---|---|
| create 128 MiB | 1,125.7 ms | 1,127.0 ms |
| 500 writes, wall | 2,308.3 ms | **1,702.9 ms** |
| write p50 / p99 / max | 4.543 / 5.630 / 9.617 ms | 3.258 / 4.612 / 7.774 ms |
| backend requests (writes phase) | 3,501 | 3,993 |
| backend bytes written | 89,451,237 | **38,779,767** |
| backend bytes read (writes phase) | 85,063,500 | 43,808,669 |
| 4 KiB range read, warm p50 | 1.422 ms | **0.414 ms** |
| 4 KiB range read, cold handle first read | 1.502 ms / 2 req / 170,127 B | 7.557 ms / 10 req / 215,317 B |
| full read before compaction | 439.3 ms / 2,049 req / 134,322,319 B | 437.8 ms / 2,049 req / 134,220,016 B |
| `verify --full` before | 1,773.0 ms, ok | 782.5 ms, ok |
| compaction | refused | **444.1 ms**, root unchanged |
| full read after compaction | 445.7 ms / 134,322,319 B | 451.8 ms / 134,322,319 B |
| `verify --full` after | 1,759.5 ms, ok | 1,879.8 ms, ok |
| retained blob bytes before / after / after GC | 134,322,319 / 134,322,319 / 134,322,319 | 166,480,149 / 134,322,319 / 134,322,319 |
| GC after compaction | 775 blobs, 70,338,845 B | 501 blobs, 2,289,555 B |

S3 has no physical-byte counter, so on-disk bytes are `null`; retained blob
bytes are computed by the store's own reachability walk over the bucket.

## What this shows

- Writes: overlay is **2.2x faster** (local) and **1.36x faster** (S3) here,
  and writes **58% / 57% fewer backend bytes**, because a COW write re-encodes
  the whole 2,048-entry chunk table (≈82 KB) while an overlay write publishes a
  small layer. Request counts are *higher* for overlay (3,993 vs 3,501) because
  the writer also reads the base manifest for its double-derivation check.
- Warm range reads are 30x faster (local) / 3.4x (S3): the resolved overlay
  table is cached per process, so a read fetches the one intersecting chunk and
  a 67,824-byte manifest rather than a 170,127-byte chunk table.
- Compaction is O(file size): it re-reads and re-hashes every chunk
  (134,220,016 B, 2,049 requests) and publishes one canonical table. It changes
  no content byte and no content root.
- After compaction the overlay head is byte-for-byte the same shape COW writes,
  so every later read costs what COW costs.

## Every loss, stated plainly

1. **Cold range reads are slower with overlays.** A new process must resolve the
   chain from blobs: 5.594 ms / 10 requests / 215,317 B (local) and 7.557 ms /
   10 requests (S3) versus 0.920 / 1.502 ms and 2 requests for COW. Depth 8
   means 8 extra manifest reads. Deeper chains are worse; the writer bound
   (`max_overlay_depth`, default 64) exists to bound this, and only compaction
   resets it.
2. **`overlay_bytes` is much larger than the bytes written.** 30.6 MiB
   (32,112,640 B) for 2.0 MiB of logical writes, because adjacent extents are
   merged across the gaps between them. Only touched chunks are rewritten; the
   enlarged extent is accounting, not I/O — but it is what
   `Options::max_overlay_bytes` bounds, so a long random-write workload reaches
   that bound sooner than its logical write volume suggests.
3. **Full reads are not cheaper.** They still read every logical chunk; the
   ~102 KB difference is manifest size, not content.
4. **A snapshot pins the old chain.** The court's snapshot was taken before
   compaction, so after compaction and GC the overlay store keeps 166,831,597 B
   on disk versus 149,348,604 B for COW (+17.5 MB). That is the snapshot-safety
   guarantee doing its job, not a leak: with no snapshot the chain is
   collectable (stage-5 test
   `gc_after_compaction_without_a_snapshot_collects_the_old_layers`). The
   difference between "retained blob bytes" and on-disk bytes is store metadata
   (commits, checkpoints, superblocks, advisory indexes): 15.0 MB for COW,
   32.5 MB here for overlay, almost all of it the pinned chain.
5. **Compaction costs a full read.** 34.0 ms local, 444.1 ms on MinIO, plus one
   manifest write. It is not incremental and it rewrites nothing but the table.
6. **Memory is slightly higher** for the overlay child (411,248 vs 402,348 KiB)
   at these settings; both peaks are dominated by the 128 MiB setup buffer.
7. **S3 `verify --full` numbers are noisy** (782.5 ms and 1,879.8 ms for the
   same overlay store before and after compaction; 1,773.0 and 1,759.5 ms for
   COW). Do not read a representation win out of that column.
8. **Not a durability claim.** The local backend is the container filesystem;
   the S3 backend is a single-node MinIO with a tmpfs data directory. No fsync,
   media, device-cache, power-loss, cloud, multi-node, fleet, or production
   claim is made. Crash behaviour is covered separately by the process-fault
   tests (`tests/overlay_write.rs`, `tests/overlay_compact.rs`).

## Reproduce

```
bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/overlay_court.sh --local"
bash scripts/s3_court.sh --overlay
```

`bench/scripts/overlay_court.sh` writes `bench/receipts/overlay-<UTC ISO>.json`
and the raw log beside it and exits nonzero if any of the six checks fails.
