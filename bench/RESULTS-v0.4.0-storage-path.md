# Storage-path results — v0.4.0 (2026-11-04)

This note covers the five storage-path receipts and the representation bench
added for v0.4.0. Every number is a single-host, page-cache or tmpfs
observation from the Rust court, on synthetic fixtures. None is a device,
FUSE, fleet, or production claim. The release note
[`../docs/RELEASE-v0.4.0.md`](../docs/RELEASE-v0.4.0.md) summarises the same
work; the receipts are the authority.

## 1. Method

All harnesses are Rust `examples/` binaries run through
`scripts/docker-court.sh rust`, which enforces `--memory=3g --memory-swap=3g
--pids-limit=1024 --cpus=4`, bounded `/tmp`, `no-new-privileges`, and
`cap-drop=ALL`. The court image was `weft/rust-court:1`, image id
`sha256:c96472367ec767612a78d1809c2a8c8cf9244c4ed9757dbe1bd821eb3a723b9d`;
host kernel `7.3.1-1-cachyos`; CPU AMD Ryzen 7 9800X3D; release profile.

| Receipt | What was measured | Harness | Storage / statistic | Revision |
|---|---|---|---|---|
| `index-bench-20261104T085157Z.json` | Resolver-only selector latency, indexed vs full scan, 10,000 objects + 10,000 relations | `examples/index_bench.rs` | In-memory state; 100 queries per kind; median wall ms; cold = first indexed call | `6e1380a` |
| `extent-cow-20261104T090112Z.json` | 20 × 4 KiB aligned overwrites of one 64 MiB object, chunk-COW vs whole-file rewrite | `examples/extent_cow_bench.rs` | Separate child process per mode; median wall ms, `/proc/self/io` `rchar`/`wchar`, `VmHWM` | `c130759` |
| `fast-open-20261104T090945Z.json` | Open/replay of a 10,000-generation store, bounded replay vs forced full scan | `examples/open_bench.rs` | tmpfs under court `/tmp`; 10 samples; commit files read | `7becc3e` (WeftCodec `3cccd8b`) |
| `group-commit-20261104T092015Z.json` | 1,000 creates and 1,000 writes, individual durable commits vs one group commit | `examples/group_commit_bench.rs` | tmpfs; 5 fresh stores; median wall ms; generation count | `78e4d14` |
| `streaming-20261104T094742Z.json` | 256 MiB create, buffered vs streaming | `examples/streaming_bench.rs` | tmpfs; one child process per mode; wall ms, `VmHWM` | `9202347` (working tree; see receipt note) |
| `representation-20261104T093437Z.json` | 8 MiB text and near-duplicate pair, raw v1 vs `weft.manifest.v2` ZSTD/DELTA | `examples/representation_bench.rs` | Host page cache; 5 repetitions; physical bytes, encode, full decode, 4 KiB range decode | `3c65acc` (dirty tree recorded) |

The index and group-commit receipts record `package_version = 0.3.0` because
they were produced before the version bump; the revisions above are the code
revisions they pin.

## 2. Numbers

### 2.1 Selector index (resolver-only, warm unless marked)

| Selector | Before (full scan) | After (index) | Cold after | Matched total |
|---|---:|---:|---:|---:|
| Relation | 132.795 ms | 0.689 ms (1.089 ms max) | 13.028 ms | 83 |
| Tag | 148.015 ms | 2.051 ms (2.191 ms max) | 14.044 ms | 8,570 |
| Attribute equality | 1.640 ms | 1.239 ms (2.365 ms max) | 13.323 ms | 147,135 |

`eval_expr` confirms every candidate, so results, ordering, `matched_count`,
and ambiguity equal the scan. `tests/index_equivalence.rs` compares 3 states
(seeds 11/29/47), 315 selector/views, and 1,566 assertions against a retained
scan oracle.

### 2.2 Extent COW (64 MiB object, 20 overwrites)

| Metric | Whole-file rewrite | Chunk COW |
|---|---:|---:|
| Median wall time | 69.64 ms | 33.01 ms |
| Median `rchar` | 134,204,646 B | 67,226,853 B |
| Median `wchar` | 122,431 B | 122,431 B |
| Peak RSS | 134,836 KiB | 3,932 KiB |

### 2.3 Bounded open (10,000 generations, checkpoint interval 256)

| Metric | Forced full scan | Bounded replay |
|---|---:|---:|
| Median | 3,789.1 ms | 71.6 ms |
| p95 | 3,892.0 ms | 73.2 ms |
| Commit files read | 10,001 | 18 |

### 2.4 Group commit (1,000 objects, 32-byte payload, 5 runs)

| Metric | Individual commits | One group |
|---|---:|---:|
| 1,000 creates, median | 89.87 ms | 22.01 ms |
| Durable commits | 1,000 | 1 |
| 1,000 grouped writes, median | — | 21.86 ms |

Correctness assertions: one generation increase per group, 1,000 objects read
back, `verify --full` clean on every run.

### 2.5 Streaming 256 MiB create

| Metric | Buffered | Streaming |
|---|---:|---:|
| Wall time | 138.361 ms | 190.704 ms |
| Peak RSS | 269,072 KiB | 6,764 KiB |
| Physical write bytes | 268,649,181 | 268,649,181 |

### 2.6 Representation (8 MiB synthetic, Rust store)

| Case | Physical bytes | Encode ms | Full decode ms | 4 KiB range ms |
|---|---:|---:|---:|---:|
| Text, raw v1 | 8,395,250 | 227.56 | 2.394 | 0.046 |
| Text, v2 ZSTD | 16,853 | 12.84 | 2.925 | 0.032 |
| Near-duplicate, raw | 8,395,250 | 388.60 | 2.497 | 0.049 |
| Near-duplicate, v2 ZSTD (falls back to raw) | 8,395,250 | 396.08 | 2.452 | 0.054 |
| Near-duplicate, v2 DELTA against causal base | 1,153,697 | 86.01 | 6.942 | 6.171 |

`store_total_physical_bytes` accounts for deduplication and differs from the
per-representation `physical_bytes` in the raw cases (for example the text raw
case stores 4,069,874 unique blob bytes). The receipt records both.

## 3. Losses and limits

- **The index is not persisted.** It is per-state and rebuilt on first use;
  cold resolution pays 13.0–14.0 ms at 10k objects/relations. Attribute
  equality improves only ~1.3× (1.640→1.239 ms warm), because it remains a
  scan of present attributes. The measurement is resolver-only, in-memory,
  and not disk/FUSE/end-to-end.
- **Extent COW still hashes the whole file.** The v1 whole-file `ContentId`
  keeps one O(size) digest pass; only writes, read-verification memory, and
  page-cache traffic improve. `read_object` still materializes the file.
- **Bounded open can skip older history below a checkpoint.** It emits a
  recovery note naming the checkpoint and falls back on any inconsistency;
  `verify --full` still enumerates every commit. Checkpoint creation is
  O(state) and its interval is not tuned or measured here.
- **Group commit is a store-API timing on tmpfs.** Absolute times are
  dominated by tmpfs; it is not a device, FUSE, or fleet claim, and it does
  not change the per-mutation durability contract outside a group.
- **Streaming is slower in wall time** (190.7 ms vs 138.4 ms) and trades that
  for memory (6.8 MiB vs 269 MiB). Codec-backed streaming is not
  implemented (large `CodecAuto` streams use a v2 raw chunk manifest), and
  `read_object` still returns the whole file.
- **The effective large-file cap is the WCB array decode limit**, roughly
  12.8 GiB at 64 KiB chunks with the default 1,000,000-item limit, not the
  nominal 1 TiB. Both implementations fail closed with `LimitExceeded`.
  The Go store still enforces its own 256 MiB `MaxFileBytes`
  (`docs/store-status.md` §6).
- **Representation benchmarks are synthetic.** 8 MiB fixtures, Rust only,
  one schema spelling, no production or model-family data. The Go store
  rejects `weft.manifest.v2`; the Rust↔Go handoff court stays on raw. DELTA
  range reads are slower (6.17 ms vs 0.05 ms raw) and full DELTA decode is
  slower (6.94 ms vs 2.50 ms raw); incompressible input falls back to raw.
  Planning is per-object, not global, and no compaction/migration exists.
- **Single host, 4 vCPUs, 3 GiB court limit, page cache.** No fsync,
  device-level, power-loss, concurrency-scaling, or 10^5–10^6-object claim is
  made from these receipts.

## 4. Reproduce

Every command runs in the Rust court with the unchanged hard limits:

```sh
scripts/docker-court.sh rust "cargo run --release --example index_bench"
scripts/docker-court.sh rust "cargo run --release --example extent_cow_bench"
scripts/docker-court.sh rust "cargo run --release --example open_bench"
scripts/docker-court.sh rust "cargo run --release --example group_commit_bench"
scripts/docker-court.sh rust "cargo run --release --example streaming_bench"
scripts/docker-court.sh rust "cargo run --release --example representation_bench"
```

`open_bench` also accepts `--generations N`, `--samples N`, `--work-dir DIR`,
and `--out PATH`; it writes `bench/receipts/fast-open-<UTC>.json` by default.
The other harnesses write their own timestamped receipt under
`bench/receipts/`. Exact revisions, court image id, and raw samples are in each
receipt; the representation receipt records `working_tree_dirty`, and the
streaming receipt records that it was measured on a working tree whose base
revision is the recorded value.
