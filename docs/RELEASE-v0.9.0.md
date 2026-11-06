# WeftFS v0.9.0 — decoder throughput, sparse and tiled tensors, shared mounts, caches, overlays, split planes, packs, host calibration, and a distributed correctness court

Research preview. v0.9.0 is the R2–R13 subphase release. It is the first release
cut after v0.8.0: a faster decoder and re-baked decode-cost table, an additive
sparse-delta mode, seekable parallel tile decode, a shared multi-mount daemon,
single-flight and a dependency-aware cache, an immutable write-overlay format
with compaction, a probed blob/commit plane split, physical object packs,
host-local planner calibration with Go rebase parity, and a real distributed
correctness court.

This is an **additive** release. The frozen v1 frame format, the frozen
`weft.manifest.v1`/`v2`/`v3` representations, the default whole-file v1 content
identity outside `mount`, the selector grammar, and codec payload modes 0–15 are
unchanged. `SPARSE_DELTA` (mode 16) is new and opt-in. The overlay store
representation (`weft.manifest.v4`), split planes, physical packs, shared mount
views, the decoded-object cache, host calibration, and rebase are opt-in.
Defaults are unchanged.

Everything below is measured in the Docker courts with the hard limits in
`AGENTS.md`. No claim is a fleet, production, physical-media power-loss, RAPL,
GPU, multi-node, or cloud claim. Every loss we know about is stated.

## Version and artifacts

```toml
weftcodec = "0.9.0"
weftfs = "0.9.0"
```

- Rust: `weftfs` 0.9.0 and `weftcodec` 0.9.0 appear in both `Cargo.toml` and
  `Cargo.lock`; the parent depends on `weftcodec = "0.9.0"`. The WeftCodec bump
  was committed and pushed first (`WeftCodec` `a888d79`), then re-pinned here.
- Python: `python/dist/weftcodec-0.9.0-py3-none-any.whl`, **66,164 bytes**,
  sha256 `b671a8636cbca0c06097ec39c225f48558f323975a4ff8114ee6ed7f026652b4`.
  Built in the Python court and tested from the installed wheel: **331 tests,
  0 failures, 0 errors, 9 skipped**
  (`bench/receipts/python-bindings-20261106T142616Z.json`). The wheel metadata
  reports version 0.9.0. **Not published to PyPI** (no token); the wheel is
  attached to the GitHub release.
- Images are published by each repository's container workflow when the release
  tag is pushed. The parent `containers.yml` publishes the `weftfs` image on a
  `v*` tag; the subprojects publish their own images. CI is triggered by
  `v0.9.0`; no image is claimed here unless its own workflow run is verified.
  FUSE mounts need `--device /dev/fuse --cap-add SYS_ADMIN`; codec and
  store-only commands do not. See `docs/containers.md`.
- crates.io publication is performed after the tag with
  `scripts/publish-crates.sh`: `weftcodec 0.9.0` first, then `weftfs 0.9.0`
  after the index is visible. The live registry checksum is reported with the
  release, not guessed here.

## Verification at the release tree

`bench/receipts/verify-20261106T141623Z.json` — **27/27 courts `pass`**, run
from WeftFS `cef37bc` (the version-bump and submodule-pin commit):

- `rust_codec`, `rust_store`, `go_codec`, `rust_fuse`, `go_store_and_fuse`;
- codec cross-decode in both directions plus byte comparison;
- `spec_drift`, `generated_types`, `claims`, `conformance_manifest`,
  `conformance_coverage`;
- Buck2 CAS unit court and store-backed demo;
- agent memory, context cross-language gates, capability cross-language gates;
- `fuzz_diff` (1,000,000 differential inputs).

Revisions pinned by that receipt:

| Repo | Verified revision | Role |
|---|---|---|
| `WeftFS` | `cef37bc` | program home, Rust store + FUSE, specs, courts |
| `WeftCodec` | `a888d79` | Rust codec, AI modes, checkpoint/capability support |
| `WeftCodec-Go` | `8957998` | independent Go codec and capability parity |
| `WeftFS-Go` | `007bf11` | independent Go store + FUSE, spec mirror |

The tag is cut after the release-note and status commits, so the tagged commits
are later than the verified code revisions; the only changes between them are
version literals, the Python receipt-generator fix, and documentation. The
host-only `claims` court is re-run on the final docs tree before the tag.

The Rust FUSE suite inside that receipt reports **434 passed, 0 failed, 2
ignored**, and the Go store/FUSE court reports `ok` for all four packages
(`cmd/weftfs`, `fusefs`, `model`, `store`). The two `#[ignore]`d tests remain:
ownership needs `CAP_SETUID`; same-inode direct-I/O on this kernel reproduces an
EIO. The Python court is separate and reports **331 tests, 0 failures, 0
errors, 9 skipped** at the same tree.

## R2 — decoder throughput and the re-baked decode table

Receipts `bench/receipts/planner-float-20261106T035316Z.json` and
`bench/receipts/parallel-restore-20261106T043524Z.json`; analysis in
[`bench/RESULTS-real-model.md`](../bench/RESULTS-real-model.md) (R2 section).

- `huffman-canonical-v1` decode now uses a 12-bit primary table over an
  MSB-aligned accumulator; the original canonical loop is kept for codes longer
  than 12 bits and for tails. Wire format, encoder bytes, validation order, and
  error classes are unchanged. Isolated microbench: **4.7–5.0x Rust, 4.1–5.3x
  Go**.
- `FLOAT_TENSOR` plane join extracts sign+mantissa once per element and writes
  in place. Microbench: **3.1–5.7x Rust, 4.8–6.5x Go**. A test-only verbatim
  reference decoder plus 10,000-case randomized differentials per language
  compare output bytes **and** error class.
- The decode table was re-measured (median of three 7-repetition calibrations)
  and re-baked: mode 9 `15,403,165 -> 4,683,726 ns/MiB` (3.29x), mode 15
  `10,056,794 -> 3,366,772` (2.99x), mode 10 `13,788,933 -> 9,123,307` (1.51x),
  weight-delta `5,297,277 -> 5,293,987`, semantic-delta `7,228,350 ->
  6,458,523`. `src/cost_model.rs` and `WeftFS-Go/store/cost_model.go` carry
  identical integers under `weftfs-r2/decode-calibration/v1`.
- Restore on the Qwen2.5-0.5B family: base read **13,775.8 -> 4,627.5 ms
  (2.98x)**, base materialize 16,736.9 -> 6,494.3 ms; instruct read
  **26,935.7 -> 11,371.2 ms (2.37x)**, instruct materialize 29,950.5 ->
  12,092.2 ms; single tensor base **111.4 -> 39.8 ms**, instruct **221.9 ->
  92.7 ms**. Storage is byte-identical at 1,276,128,956 B (3.07 % under ZipNN)
  because the planner is bytes-only at `lambda = 0`.
- Bounded parallel reconstruction (`Store::read_tensors_bounded`) reads at 4
  threads **2,206.4 ms** base and 5,275.7 ms instruct (**2.09x** / 2.08x versus
  one thread, 95–99 % of the Amdahl cap set by the 272 MB embedding).
- **Losses.** Mode 9 is still 6.0x zstd per MiB (4.68 vs 0.78); base read is
  4.8x the F10 zstd-strategy read and ~19x ZipNN's 240 ms. Materialize does
  **not** improve reliably (4-thread base 10,987.9 ms, 0.43x) because host
  writeback dominates a sequential 988 MB write. Parallel reconstruction is
  per-object only; there are no block-independent streams inside one frame.
  Single host, warm cache; the table is court-bound.

## R3 — `SPARSE_DELTA` (mode 16) and the 19-byte pair

Receipt `bench/receipts/sparse-delta-20261106T052333Z.json`; analysis
[`bench/RESULTS-sparse-delta.md`](../bench/RESULTS-sparse-delta.md).

New additive mode 16 `weft.ai.sparse_delta.v1`: a zero-run varint stream plus raw
non-zero words over a base tensor, with the same `delta_root` and exactness rules
as mode 11. `max_sparse_runs` is 16,777,216. Frozen mode 11 is byte-identical.
The reserved-mode reject moved to 17; 18 conformance files are byte-identical in
both codec repos and the cross gate reports `YES byte-identical
sparse_delta_bf16/q8/fp32`.

- pythia step0→step1 (**19 changed words**): mode 11 7,434,959 B -> **mode 16
  29,887 B (0.0040x)**, also below the recorded ~30 KB
  `zstd --patch-from -3`.
- step1→step2 28.0M -> 18.1M (0.647x).
- **Losses.** step2→step4 33.4M -> 74.2M (2.22x), step4→step8 49.2M -> 161.7M
  (3.29x), Qwen fine-tune 541.2M -> 1,050.0M (1.94x). `max_sparse_runs` refused
  `embed_out` and the embedding on some pairs (recorded as refused, excluded
  from sums). The planner selects per tensor; mode 16 is only for genuinely
  sparse deltas. The zstd/patch baselines were copied from the P2 receipt, not
  re-run.

## R4 — parallel, seekable tiled decode and auto-tiling

Receipt `bench/receipts/tiled-parallel-20261106T062146Z.json` (12/12 checks);
analysis [`bench/RESULTS-tiled-parallel.md`](../bench/RESULTS-tiled-parallel.md).
No wire change.

Mode-15 tiles decode on a bounded pool (`TileDecodeOptions`, shared with
`read_tensors_bounded`, per-tile roots verified, default `threads = 1`);
`weft cat --threads N`. `TensorPlanOptions.auto_tile_bytes` and
`weft plan|write --auto-tile-bytes N` derive a tile shape that divides both
extents (typed error otherwise).

- Bytes-only pricing still selects mode 9 for all 290 tensors; auto-tiling
  changed zero representations and zero bytes because mode 9 is 2.174 % smaller.
  The forced-tiled control (169 tensors) is **+14,310,892 B (+2.174 %)** and is
  labelled a control, not planner output.
- Forced control full-restore read: **3,038.3 ms at 1 thread -> 1,062.1 ms at 4
  threads (2.861x)**; materialize 4,215.6 -> 2,871.4 ms.
- Embedding region read: whole **738.7 -> 203.0 ms at 4 threads (3.64x)**; a
  1-row read touches 4 tiles and reads **730,133 B instead of 180,625,100 B**
  (247x fewer backend bytes) in 3.54–3.67 ms.
- **Losses.** Small regions do not parallelise (a 100-row or 1-row region
  touches 4 tiles, so 4 workers give 4.108 vs 3.96 ms). Materialize is still
  writeback bound. No Go parity for tile-decode options or auto-tiling.

## R5 — entropy-coder court: promote nothing

Receipt `bench/receipts/entropy-coder-20261106T065154Z.json`; analysis
[`bench/RESULTS-entropy-coders.md`](../bench/RESULTS-entropy-coders.md).

Experimental internal coders only: shipped Huffman, Tunstall k=8/10/12 with
canonical deterministic dictionaries at per-plane, per-tensor, and
family-shared scopes (full dictionary cost charged), static order-0 rANS
(12-bit normalised, 4096), zstd-3/-19, and raw. 252 rows on real planes from
Qwen BF16 and pythia F32 through the shipped encoders; 5,000-case differentials
per coder. The recommendation is **promote nothing**, and no mode was added or
changed.

- rANS is 0.5–3.6 % smaller than shipped Huffman on BF16/dense/tile/KV whole
  planes but decodes **2.0x slower (254 vs 510 MiB/s)** and 23.7x slower than
  zstd-19; sparse whole planes are 74.4x slower.
- Tunstall loses at every k and scope: k=12 whole-plane ratio 0.99–1.03 and
  1-row regions 4.5–83.8x the best coder including dictionary cost; k=8 cannot
  split.
- zstd-19 is the size reference on FP32 mode 9 (−12.1 % vs rANS) and 1-row
  regions but encodes at 28.5 MiB/s; zstd-3 is 3.9–6.5 % larger than Huffman.
- Huffman is within 0.5–3.6 % of the smallest coder where it matters at
  **1,229 MiB/s decode**.
- Reverse findings kept: rANS really is smaller; Huffman is not size-optimal;
  the 256-byte table is 12.5 % of a 2 KiB plane. One host, warm cache, one
  tensor per class; codec-level planes only.

## R6 — Python reader modes 9–16

Receipts `bench/receipts/conformance-python-20261106T072745Z.json` and
`bench/receipts/python-bindings-20261106T072822Z.json`.

The independent stdlib Python reader now decodes modes 9–16 with every
normative root check: payload hash, `delta_root`, `logical_root`, `tile_root`,
`content_hash`, and canonical descriptors.

- Conformance adapter is executed rather than skipped: **326 total, 218 pass,
  0 fail, 108 skip** (was 56/0/270); no mode 0–16 vector is skipped.
- Randomized differential versus the Rust CLI: **16,003 generated frames**
  across the 8 modes, all bytes and all accept/reject decisions identical;
  1,600 mutants with 1,581 expected reject classes all matched; 627/627
  shard-set roots identical.
- Remaining skips are non-codec surfaces only: WEFT-TENSOR layout containers,
  selectors, context, causal identity, capability envelopes, checkpoints,
  archives, adversarial vectors, store fixtures, and superseded vectors.
- **Loss.** Python does not implement selectors, Context Roots, capability
  envelopes, checkpoint manifests, archives, or WEFT-TENSOR layout containers;
  `TENSOR_TILED` is full-decode only (no region reader).

## R7 — shared multi-mount daemon

Receipt `bench/receipts/shared-mounts-20261106T082042Z.json` (19/19); analysis
[`bench/RESULTS-shared-mounts.md`](../bench/RESULTS-shared-mounts.md) and
[`docs/shared-mounts.md`](shared-mounts.md).

`StoreHandle::{Exclusive,Shared}`; shared views borrow `&Store` and cannot
obtain `&mut`, so writes return `EROFS`. `MountOptions.snapshot_state` is an
`Arc<State>`, so N views share one replay. `SharedMount`, `run_shared`,
`MAX_SHARED_VIEWS = 256`, and `weftfs mount-many --spec FILE` are new; the
single-mount CLI is unchanged.

At N=100 on the oracle corpus, with the same 425 file SHA-256 checks and the
same digest over all 100 per-view listing digests:

- processes **100 -> 1**; summed RSS **1,963,056 -> 27,692 KiB** (70.9x less);
  cgroup peak **1.259 GiB -> 66.2 MiB** (19.5x less); threads **692 -> 102**.
- **Losses.** Time to first view 523 -> 3,964 ms; time to all views 4,428 ->
  4,064 ms. At a 512 MiB limit shared completed 100/100 (peak 70.2 MiB, 0 OOM)
  while per-daemon mounted 51/100 with 49 OOM kills. One backend mutex
  serialises physical I/O across views, so the shared server can be slower under
  concurrent large reads. Views are read-only; a shared server pins at open;
  one process is one failure domain. No Go `mount-many` parity; one host.

## R8 — single-flight, dependency-aware cache, and the counter-flake fix

Receipt `bench/receipts/cache-20261106T091322Z.json` (26/26 plus 5/5
adversarial); analysis [`bench/RESULTS-cache.md`](../bench/RESULTS-cache.md).

- **Counter flake fixed at the root cause.** `codec_chunk_decode_count` was one
  process-global atomic; it is now thread-local (the work is on the calling
  thread), with the aggregate kept as `codec_chunk_decode_total()`. The
  assertion is unchanged. 20/20 parallel runs at `--test-threads=8`; a
  regression test was added.
- Single-flight is keyed by object id + version + region, with bounded entries
  and bytes, waiter-only cancellation, and error fan-out; off by default. At 4
  threads it cut decode calls **4x (640 -> 160)** and backend bytes **4x
  (3.02 GB -> 754.6 MB)**; wall time on 4 otherwise idle vCPUs was within noise
  (−3.9 %).
- The dependency-aware decoded-object cache is bounded by `--cache-bytes` and
  admits by expected reconstruction cost avoided per byte (measured decode cost
  x dependent count x read frequency), with deterministic eviction, pinning,
  version separation, and authorization before lookup. On the real Qwen family
  (226.6 MB working set, 4 rounds): no cache at 4 threads **6,645.9 ms /
  3.02 GB read / 640 decodes** -> cache+single-flight at 4 threads **1,140.6 ms
  (5.83x) / 107.5 MB / 26 decodes**, 100 % hash match, `verify --full` ok. The
  1-thread pair is 6,440.7 -> 1,124.7 ms (5.73x).
- **Losses.** The cache retains decoded bytes: `both@4` peak RSS 391,116 KiB
  versus `none@1` 95,120 KiB (about +289 MiB in this fixture); a bound
  below the working set re-decodes; round 1 still races with cache only; p99
  stays bimodal; the cache is per-process and not persisted. Single-flight at 1
  thread coalesces nothing and is published as no benefit. Mode-15 whole-object
  reads bypass both layers; FUSE range reads get single-flight only. No Go
  parity.

## R9 — immutable write overlays and compaction

Receipts `bench/receipts/overlay-20261106T103115Z.json` (local) and
`bench/receipts/overlay-s3-20261106T103120Z.json` (MinIO), 6/6 checks each;
analysis [`bench/RESULTS-overlay.md`](../bench/RESULTS-overlay.md).

`weft.manifest.v4` `kind:"overlay"` carries an immutable base plus ordered
extents, has no top-level chunk table, and has a Merkle content root equal to a
canonical v3 single-table manifest. Bounds: depth 1..=256 (writer default 64),
≤64 extents per layer, chunk-aligned extents, sorted and non-overlapping within
a layer, 1 GiB default overlay bytes with a typed limit naming `compact`.
v1/v2/v3 are byte-identical. `Store::compact` re-reads and re-hashes every chunk,
requires the pre-compaction root, and publishes a canonical v3 table through the
normal commit protocol.

128 MiB file, 64 KiB chunks, 500 x 4 KiB writes:

- local write wall **917.3 ms (COW) -> 415.5 ms (overlay)**, **87.4 -> 36.7 MB**
  written, p50 **1.819 -> 0.717 ms**; MinIO **2,308.3 -> 1,702.9 ms**, 89.5 ->
  38.8 MB. The content root is identical in all four runs (`c00367f0...`).
- warm 4 KiB range read **0.919 -> 0.030 ms** local and **1.422 -> 0.414 ms**
  MinIO; cold first read **0.920 -> 5.594 ms** (2 vs 10 requests, depth-8 chain
  resolution).
- compaction **34.0 ms** local / **444.1 ms** MinIO, root unchanged, snapshot
  byte-identical after compaction and GC. Depth 8, 22 extents,
  `overlay_bytes` 32,112,640 for 2,048,000 logical bytes written (coalescing
  widens extents). Post-compaction+GC disk 149,348,604 (COW) vs 166,831,597
  (overlay; ~32.5 MB held by the court snapshot). GC deleted 775 blobs /
  70,338,845 B (COW) vs 501 / 2,289,555 B.
- **Losses and limits.** A whole-file rewrite of an overlay object materialises
  v3, and range/append then refuse; shrinking `truncate`, `collapse_range`, and
  `insert_range` refuse with `UnsupportedRepresentation`; streaming append keeps
  refusing overlay objects. Depth is paid for in cold-read requests. Go has
  read/verify/range-read/GC/quota only: **no v4 writer and no compaction**, and a
  range write to an overlay returns `InvalidArgument`. No Go write-parity claim.

## R10 — blob and commit planes with probed capabilities

Receipts `bench/receipts/planes-20261106T110154Z.json` and
`bench/receipts/planes-20261106T110216Z.json` (14/14 tests each; MinIO
court receipts beside them); analysis
[`bench/RESULTS-planes.md`](../bench/RESULTS-planes.md), contract in
[`docs/PLANES.md`](PLANES.md).

`BlobPlane` (get, get_range, put_if_absent, head_part, delete, capabilities) and
`CommitPlane` (read_control, compare_exchange, compare_exchange_head,
load_head, write_control, create_control, put_control_if_absent,
delete_control, lock, sync_dir, probe) are now the contracts; the existing
`StorageBackend` is retained and delegates, so the plane contract cannot drift
from the publishing primitives.

Capabilities are **probed** on writable open, never merely declared: the blob
probe publishes a private 192-byte object, HEADs its length, reads a 17-byte
mid-range, and deletes it; the commit probe does create-once CAS, matching CAS,
and a stale CAS that must return `Busy`. An endpoint accepting a stale token is
refused `UnsupportedVersion`. A probe may only refute a declared capability,
never upgrade one. Read-only opens never probe. `--blob-backend`,
`--commit-backend`, and `--blob-root` split the planes; the default is the
previous single backend.

6,291,456 bytes, 48 x 128 KiB, 96 x 4 KiB range writes; all four layouts
(local/local, s3/s3, s3-blobs/local-commits, local-blobs/s3-commits) produce
root `7ebb1bdb...`, generation 145, digest `74d54de5...`, `reopen.match=true`,
and 0 verify problems. s3 blobs / local commits: create 98.4 ms, write p50
2.254 ms, full read 31.3 ms, verify 143.1 ms, reopen 44.3 ms (versus s3/s3
129.9 / 2.877 / 30.0 / 208.5 / 45.5). local blobs / s3 commits: 55.7 / 1.149 /
3.37 / 91.5 / 2.98 ms. Crash proxy at after_objects, after_commit_file, and
after_superblock_write: old-whole or new-whole only, never partial; a second
writer is refused `Busy`.

**Limits.** Rust-only split; a single-writer commit plane; the crash proxy is a
process-fault proxy, not physical power loss; a 6 MiB synthetic fixture; store
and CLI level only.

## R11 — physical object packs

Receipt `bench/receipts/packs-20261106T113605Z.json` (13/13 tests, with all
four configurations exact); analysis [`bench/RESULTS-packs.md`](../bench/RESULTS-packs.md).

`PackedBlobPlane` wraps a `BlobPlane` with a `CommitPlane` catalog. Members
≤64 KiB are sealed into packs published with `put_if_absent` under
`H("weft/pack/v1", bytes)`; the catalog is content-addressed segments plus one
bounded `pack-catalog.root` control record replaced by a single CAS. Order is
pack -> segments -> root CAS, so a crash leaves unreferenced packs, never a
dangling entry. Reads do one `get_range` for exactly the member. Bounds: 16 MiB
pack, 64 KiB member threshold, 65,536 members/pack, 8 MiB catalog, 256 segments;
over a bound falls back to flat, not failure. Refuses if the inner plane lacks
`true_range_reads`.

10,000 x 1 KiB in one group commit:

- MinIO object requests **20,003 -> 261**, wall **7,321.5 -> 2,791.7 ms**;
  local **20,003 -> 262**, **298.3 -> 185.4 ms**.
- 1,000 reads: warm requests **1,000 -> 1,000** (1.02 MB); cold **1,000 ->
  1,252** requests / 3.26 MB (3.19x read amplification).
- A 4 KiB range inside a 16,777,216-byte pack is **1 ranged request, 4,096
  bytes**.
- **Losses.** Written bytes +13.8 %, footprint +19.2 % (catalog 2,272,401 B for
  20,001 members), MinIO warm p50 0.3437 vs 0.1685–0.1729 ms, `verify --full`
  +42.6 %, RSS +6,036/+2,228 KiB. No Go parity; packs are never rewritten; the
  catalog-bound fallback is plane-tested only; a partial range read cannot
  verify a member digest.

## R12 — host calibration, Go rebase CLI, and lineage rebase at scale

Receipt `bench/receipts/rebase-scale-20261106T130054Z.json`; analysis
[`bench/RESULTS-rebase-scale.md`](../bench/RESULTS-rebase-scale.md).

- `src/calibration.rs` owns the measurement. `weftfs calibrate --out|--from|
  --install`, and `--planner-cost-model measured --calibration FILE` replaces
  the baked table. The loader is strict (schema, exact classes, ≥5 samples,
  bounds, monotonicity, digest) with no fallback; the record schema is
  `weft.calibration.v1`. Tests 8/8, 18 rejection cases. Divergence from the
  baked table on this host: raw +5.4 %, zstd +4.4 %, float-tensor +2.0 %,
  tensor-sharded −2.5 %, tensor-tiled +5.0 %, weight-delta +5.7 %,
  semantic-delta +8.7 %.
- Go now has `weftfs rebase` with `--root/--threshold/--dry-run/--strategy/
  --lambda-ns/--max-objects`; a Go-applied rebase is verified by Rust
  `verify --full` and `provenance show`, and a Rust-applied rebase is read by
  Go. The work also found and fixed a real bug in **both** languages: `flush`
  early-returned on an empty pending map, so a pure consumed-counter reset never
  persisted (regression tests in `src/access.rs` and
  `WeftFS-Go/store/access_consume_test.go`).
- Scale court (Qwen family, 16 hot tensors x 32 reads, 41 rebaseable deltas
  unread, 291 scanned): **16 selected / 16 applied / 0 prediction mismatches**;
  a second apply wrote 0. Selected per-tensor decode **4,501.5 -> 1,900.8 ms
  (−57.8 %)**; full instruct restore **10,938.8 -> 8,430.9 ms (−22.9 %)**; base
  unchanged. Physical representation **+43,575,360 B (+19.45 % on the selected
  set)**; store tree +277,231,717 (old frames retained by snapshot/history);
  indexes +9,555,328. Break-even is ~87.2 ms/read for an 8.7 MB MLP tensor, so
  the fixed 200 ms threshold is more conservative than storage economics for
  the 2-read set. 13/13 exactness checks including the source SHA-256.
- **Limits.** Go has no `--calibration`; the scale court is Rust-only; no
  physical power loss.

## R13 — distributed correctness court

Passing receipt `bench/receipts/distributed-20261106T134954Z.json`: **9/9
scenarios, 229 checks, 0 failed, 120 processes, 42 buckets**. The failing first
receipt `bench/receipts/distributed-20261106T133534Z.json` (206/229) is retained
because it found real defects. Analysis
[`bench/RESULTS-distributed.md`](../bench/RESULTS-distributed.md).

`bench/scripts/distributed_court.sh` and `examples/distributed_court.rs` drive
separate processes against an in-container MinIO; `tests/
distributed_semantics.rs` 12/12. Scenarios: single-writer interlock (20/20
`Busy`, reopen exact); crash at after_objects / after_commit_file /
after_superblock_write (15 runs, 10 old / 5 new, 0 mixed); idempotent retry
(same head `32a0ba3d...9431`); 8 concurrent readers versus 10 commits (64/64
SHA-256 matches); stale reader never sees newer objects; unreachable store (open
`Io`, read `Io` with 0 bytes, restart verifies); GC race under
snapshot/checkpoint/provenance (48 blobs / 16,435 B reclaimed, all live state
exact); authorization by content hash (`NotFound: no such object`, byte-identical
to absent; other token -> `Unauthorized`); and the same fault set over
`--blob-backend s3 --commit-backend local` (60/60).

Real fixes: `FORMAT` read errors and transport failures on manifest/chunk reads
were classified `Corrupt`; error class `Io` is now preserved while genuine
missing/hash-mismatch stays `Corrupt`.

**Limits.** Single-node MinIO on loopback; no TCP partition beyond a process
stop or no-listener endpoint; no physical media loss; writers are exclusive by
design; Rust-only; the split case uses a local commit plane.

## Retained receipts and known flakes

- `bench/receipts/verify-20261106T141623Z.json` — clean **27/27** release
  verification.
- `bench/receipts/distributed-20261106T133534Z.json` — failing first distributed
  run (206/229); retained because it found real defects.
- `bench/receipts/python-bindings-20261106T142538Z.json` — the Python court was
  clean, but the receipt generator hard-coded `"version": "0.8.0"`. The
  generator now derives the version from the built wheel; the clean rerun is
  `python-bindings-20261106T142616Z.json`. Both receipts are retained.
- `bench/receipts/entropy-coder-20261106T064820Z.json` — pre-final entropy
  court; retained; the final analysed receipt is `...065154Z.json`.
- `bench/receipts/verify-20261106T011444Z.json` — the v0.8.0 release receipt;
  retained and cited in `docs/RELEASE-v0.8.0.md`.
- Pre-existing `#[ignore]`d FUSE tests remain: ownership needs `CAP_SETUID`,
  and kernel 7.3 same-inode direct-I/O reproduces an EIO. The earlier
  `tests/representation.rs` shared atomic chunk-counter race is **fixed** in
  R8 (thread-local counter, regression test); the `tests/quota.rs` writer-lock
  flake was not observed in this release run and is not claimed fixed.

## Still not claimed

Physical media power loss, host page-cache loss, RAPL energy, GPU/SIMD paths,
runs against 3FS/GCS/Azure/Ceph, PyPI publication, a multi-writer store, a
fleet, multi-node, or cloud claim, a multi-host rebase policy, Go parity for the
v4 overlay writer/compaction, Go pack planes, Go `--calibration`, and Go
split-plane parity. The Python reader still lacks selectors, Context Roots,
capability envelopes, checkpoint manifests, archives, and WEFT-TENSOR layout
containers. Two FUSE tests remain ignored for the environment reasons above.

Prior release: [`docs/RELEASE-v0.8.0.md`](RELEASE-v0.8.0.md) (fast mount writes,
tiled tensors, commit-time quotas, S3, PyTorch DCP, signed bundles).
