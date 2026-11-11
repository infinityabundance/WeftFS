# R12c item 2 — operation-derived incremental index maintenance

Status: implemented and measured in Rust. The index stays **advisory**:
correctness never depends on it, and every inconsistency rebuilds from replayed
state. `StateIndex::build` stays the oracle. Go has **no** incremental
maintenance in this session; the format-compatibility gate passes (below).

* Baseline revision: `b69fa0e` (staging, 2026-11-11).
* Measured tree: commit `7d7e2af` (source files byte-identical to the tree that
  was measured; the receipts' `revision` field names `036add1` because the
  harness reads `WEFT_REPO_WEFTFS` from `HEAD` at run time).
* Court: `scripts/docker-court.sh bench`, default hard limits
  (`--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
  bounded `/tmp` 1 GiB). No limit was raised.
* Harness: `examples/index_incremental_bench.rs` (release, tmpfs, single host,
  warm page cache). One store per mode; a fixed live working set is seeded in
  group commits, then one `set_xattr(rev)` commit per step. Statistics are the
  last 200 commits ending at the checkpoint.

## Receipts

| receipt | what it is |
|---|---|
| `incremental-index-20261111T220000Z-baseline-delta-live64.json` | baseline history sweep, 64 live, 1k/10k/100k generations |
| `incremental-index-20261111T221000Z-baseline-delta-live1000.json` | baseline, 1,000 live |
| `incremental-index-20261111T223000Z-baseline-full-live64.json` | baseline full-segment control |
| `incremental-index-20261111T214500Z-baseline-delta.json` | baseline, 512 live; **retained failure**: ENOSPC at generation 80,785 |
| `incremental-index-20261111T222000Z-baseline-delta-live10000.json` | baseline, 10,000 live; **retained failure**: ENOSPC at generation 9,750 |
| `incremental-index-20261111T230000Z-post-delta-live64.json` | post-change history sweep |
| `incremental-index-20261111T231000Z-post-delta-live1000.json` | post-change, 1,000 live |
| `incremental-index-20261111T235000Z-post-delta-live10000.json` | post-change, 10,000 live |
| `incremental-index-20261111T234000Z-post-delta-live512.json` | post-change, 512 live (no GC) |
| `incremental-index-20261111T240000Z-post-delta-live512-gc.json` | post-change, 512 live, same `--gc=1` shape as the baseline cell |
| `incremental-index-20261111T232000Z-post-full-live64.json` | post-change full-segment control |
| `incremental-index-20261111T233000Z-control-no-maint-live1000.json` | **same-revision** control: `incremental_index_maintenance = false` (the pre-change writer) |

Each receipt has a `.log` beside it with the command, the per-checkpoint
summary and the full JSON.

Baseline command (the post runs are the same with `WEFT_INDEX_MODE=delta-no-maint`
for the control cell):

```sh
scripts/docker-court.sh bench "cd /work && \
  WEFT_INDEX_MODE=delta WEFT_INDEX_SIZES=1000,10000,100000 WEFT_INDEX_LIVE=64 \
  cargo run --release --locked --example index_incremental_bench"
```

The baseline was measured with the harness at `036add1`, which has no
maintenance switch. The post-change harness adds one `Options` field and the
`delta-no-maint` mode; the measured workload is identical.

## C1 baseline (`b69fa0e`)

History sweep, 64 live objects, one small attribute update per commit:

| generation | commit p50 | p95 | p99 | index bytes | data bytes | index / data |
|---:|---:|---:|---:|---:|---:|---:|
| 1,000 | 0.198 ms | 0.246 | 0.306 | 828,646 | 110,342 | 7.51x |
| 10,000 | 0.201 ms | 0.327 | 0.344 | 8,195,971 | 1,307,846 | 6.27x |
| 100,000 | 0.199 ms | 0.221 | 0.244 | 81,816,646 | 12,983,837 | 6.30x |

Index work per commit at 100,000 (phase totals, cumulative):
`index_segment` 15,284.4 ms, of which `index_build`
(`StateIndex::build`) 4,549.1 ms, `index_attest` 4,680.3 ms,
`index_load` 2,801.7 ms; `manifest_commit` 1,635.0 ms. Peak RSS 16,232 KiB.

Live-state dimension at 1,000 generations (the history is held constant; the
live state grows):

| live objects | commit p50 | p95 | index_build per commit | index_segment per commit |
|---:|---:|---:|---:|---:|
| 64 | 0.198 ms | 0.246 | 0.046 ms | 0.153 ms |
| 512 | 1.086 ms | 1.136 | 0.423 ms | 0.792 ms |
| 1,000 | 2.077 ms | 2.235 | 0.848 ms | 1.414 ms |
| 10,000 | 22.577 ms | 24.605 | 9.630 ms | 14.476 ms |

Cold first-query p50 after a fresh read-only open (1,000 live, 1,000
generations): 2.05–3.08 ms; at 10,000 generations 1.01–1.24 ms; at 10,000 live
objects 25.3–30.6 ms (chain depth 29).

Full-segment control (64 live, `incremental_index = false`): p50 0.155/0.154 ms
at 1k/10k, 18,580 B per commit, 17.7/178.0 MB of index.

## C4 post-change (same sizes, same fixture)

History sweep, 64 live objects:

| generation | commit p50 | p95 | p99 | index bytes | data bytes | index / data |
|---:|---:|---:|---:|---:|---:|---:|
| 1,000 | **0.110 ms** | 0.119 | 0.131 | 828,646 | 110,342 | 7.51x |
| 10,000 | **0.112 ms** | 0.152 | 0.199 | 8,195,971 | 1,307,846 | 6.27x |
| 100,000 | **0.112 ms** | 0.133 | 0.207 | 81,816,646 | 12,983,837 | 6.30x |

**Commit p50 is flat within 1.8 % from 1,000 to 100,000 generations**
(0.110 → 0.112 ms) while the history grows 100x and the index tree grows 99x.
The sealed on-disk bytes are identical in every cell: the segment encoder's
output did not change, only how the rows were produced.

Index work at 100,000 (cumulative): `index_segment` **6,771.5 ms** (was
15,284.4), `index_attest` **2,464.0** (was 4,680.3), `index_load` **0**
(the base is the in-memory attested cache; was 2,801.7), `index_build` **0**
after genesis (was 4,549.1). Peak RSS 12,224 KiB (was 16,232).

Live state at 1,000 generations, baseline → post:

| live objects | baseline p50 | post p50 | speed-up | baseline p95 | post p95 |
|---:|---:|---:|---:|---:|---:|
| 64 | 0.198 | **0.110** | 1.80x | 0.246 | 0.119 |
| 512 | 1.086 | **0.353** | 3.08x | 1.136 | 0.535 |
| 1,000 | 2.077 | **0.641** | 3.24x | 2.235 | 0.676 |
| 10,000 | 22.577 | **5.569** | 4.05x | 24.605 | 7.230 |

At 1,000 live / 10,000 generations: 2.145 → **0.665 ms** (3.23x).
Index work at 1,000 live / 10,000 generations: `index_segment`
16,644.2 → **1,983.2 ms** (8.39x), `index_attest` 3,591.8 → **103.4 ms**,
`index_build` 8,541.1 → **0**.

Same-revision control (`incremental_index_maintenance = false`, 1,000 live):
p50 2.072/2.136 ms at 1k/10k, i.e. within 0.5 % of the baseline's 2.077/2.145.
The control proves the comparison is the maintenance change, not the revision.

Full-segment control (64 live): 0.155/0.154 → **0.114/0.115 ms** (1.36x), same
18,580 B per commit and the same 17.7/178.0 MB index tree.

Cold first-query p50 after reopen (unchanged, within noise):
1,000 live / 1,000 gens 3.08 → 2.20 ms; 1,000 live / 10,000 gens 1.05 → 0.97 ms;
512 live / 1k 1.25 → 1.09 ms; 10,000 live / 1k 26.9 → 23.6 ms.
`builds == 1` per store (the genesis commit), `maintenance_updates` = every
attested non-genesis commit, `maintenance_fallbacks = 0`, `rejects = 0`,
`write_errors = 0` in every post run.

Peak RSS fell in the comparable cells (64 live 16.2 → 12.2 MiB; 10,000 live
167.2 → 86.0 MiB); the `--gc=1` cell is 36.7 MiB because GC itself builds the
mark set.

## What remains O(index), and why

The writer still pays two whole-index passes per commit, and they are the
remaining dependency on live-state size (~0.5 µs/object at 1,000 generations):

1. `index.candidates_root()` — the commit attests `H("weft/index/candidates/v1",
   WFX2 sections)`, so the canonical section bytes must be produced (sort + hash)
   for every publish. The contract in `docs/INDEX-TRUST.md` requires that root
   per commit and requires it to equal a full rebuild; it cannot be maintained
   incrementally without changing the attested root definition.
2. Every `index_chain_max_deltas + 1` commits a **full** WFX3 segment is
   written (the reader-merge bound), which re-encodes the whole index; and every
   commit compares the delta's length against the full segment's length once the
   delta is larger than 1 KiB or the index is small.

Removing (1) needs a Merkle root over per-row digests (a new normative
attestation), which is out of scope for item 2; removing (2) would raise the
reader-merge bound. Neither is claimed.

## Design choice and the reader-merge bound

Chosen: **(a) an operation-derived delta segment** on top of the existing WFX3
chain, not a new bounded-merge compaction format.

* One WFX3 segment per commit, as before: full segments are unchanged, deltas
  now carry exactly the rows the ops touched, with element-level patches.
* A reader merges at most `index_chain_max_deltas` deltas plus one full ancestor
  (default **32 + 1 = 33** segments; hard reader cap `256 + 1`). The writer
  still forces a full segment at that bound, so the chain cannot grow past it.
  No new format, no new trust semantics, no change to `weftfs.commit.v2` or its
  `index_attestation`.
* Why not compaction: the chain already bounds the merge count and GC already
  retains exactly the segments a retained commit names. A delta writer is the
  smaller change and keeps every frozen byte (WFX2/WFX3, `candidates_root`,
  `segment_root`) identical. Compaction exists (`weftfs compact`) for the
  read-latency case; it is unchanged and cheap with maintenance on.

Maintenance rules (`src/index_incremental.rs`):

* `put_object` / `delete_object`: the object's attribute rows are removed for
  the pre-op record and added for the post-op record. `objects_with_attr` skips
  `null`; `objects_by_attr_eq` uses the same `eq_key` normalisation as the
  oracle; arrays/maps are absent; an unencodable scalar marks the index
  incomplete exactly as the oracle does.
* `put_relation` / `del_relation`: one `(subject, predicate)` edge row. A
  `tagged_as` value relation also updates the tag set; the subject leaves the
  set only when no remaining relation on that row still names the same target,
  which is what the oracle's set semantics imply (a test with two tag edges on
  one subject covers it).
* Entries, snapshots, tips and quota assertions touch no rows; only the three
  counts and the generation are refreshed.
* Pre-images are captured before the state mutates (non-group) or by the group's
  undo journal (group). A missing pre-image, a row the ops imply but the base
  lacks, a generation gap, or a failed `Arc` unwrap all return "rebuild":
  `maintenance_fallbacks` counts it, `StateIndex::build` runs, and a **full**
  segment is published. There is no partial index.

## Correctness gates

* **Oracle equality** (`tests/index_incremental.rs`): deterministic xorshift64\*
  sequences, 10,000 operations per seed, seeds 1, 7, 2026, mixing create,
  attribute update, unrelated metadata edit, delete, rename, object relation,
  tag add and tag delete. Every 500 operations and at the end the serialized
  WFX2 candidate sections of the maintained index are compared **byte for byte**
  with `StateIndex::build` over the live state, and seven selector views are
  compared with the scan oracle. 3/3 pass; `maintenance_updates` = all attested
  commits minus genesis, `maintenance_fallbacks = 0`, `builds` = the test's own
  oracle calls plus genesis only.
* **Group commits**: one group with repeated writes to two objects, a tag and an
  object relation; same byte-exact check (pass).
* **Delta bytes**: unit test proves `encode_delta_changed` output equals
  `encode_delta` output for the same index (the optimisation may not change the
  wire bytes). 5/5 unit tests pass, including a duplicate tag edge and a plain
  delete.
* **Adversarial**: (`tests/index_incremental.rs`) head segment deleted,
  truncated, or one body byte flipped → `disk_loads = 0`, `builds >= 1`,
  `rejects >= 1` for truncate/flip, exact scan answers; an unreferenced
  `ff…idx` file is ignored and the head still loads. (`tests/index_chain.rs`,
  extended) the head file replaced by one of its own ancestors, all bytes and
  digests valid → rejected, rebuilt, exact answers. Existing
  `tests/index_persistence.rs` forged-subset test still passes.
* **Crash safety**: publication order is **commit first, segment second**, so a
  kill cannot expose a segment for an unpublished commit; the reachable states
  are "attested commit without a segment" (deleted/truncated test → rebuild) and
  "orphan segment under a commit id the chain never names" (ignored test). A
  segment is written atomically (`put_atomic`) and validated by its digest,
  counts, chain root and the commit's `candidates_root`; no partial segment can
  be trusted. No new physical power-loss court was run; that remains the
  programme-wide gap.
* **Cross-language (format compatibility, not parity)**: in the FUSE court,
  `TestRustLoadsGoWrittenIndexSegment`, `TestGoLoadsRustWrittenIndexSegment` and
  `TestRustLoadsGoDeltaChain` all PASS (1.1 s): Go resolves the Rust-written
  delta chain, re-encodes it byte-identically, and loads Rust segments with
  `DiskLoads=1, Builds=0`. A Rust store with a delta head written at this
  revision verifies from Go:
  `weftfs verify --full` → `ok=true full=true generation=6`;
  `WeftFS-Go/cmd/weftfs verify --full` on the same directory →
  `ok=true full=true generation=6 commit=22218594…` with 6 commits,
  3 objects, 3 manifests, 3 chunks checked. Go has **no incremental
  maintenance** in this session and no Go timing is claimed.
* **Suites**: Rust `cargo test --all-targets` 782 passed / 0 failed
  (161 binaries); FUSE `cargo test --features fuse --all-targets` 827 passed /
  0 failed / 2 ignored (the two pre-existing ignores). Go store suite:
  `go test -race ./store -run 'Index|Handoff'` PASS (78.7 s).

## Losses, residual risks, not claimed

* **The live-state dependency is reduced, not removed** (4.05x at 10,000 live
  objects, but still linear in the live index because of the attested candidates
  root). `commit p50` is used as a proxy for cost; CPU was not sampled.
* **Index footprint is unchanged** (the same sealed segments; 7.5x data at 64
  live, 6.3x with 100k generations, 3.9x at 512 live/10k with GC). This item
  does not make the index tree smaller, only cheaper to produce.
* **Pre-existing GC defect found by this court, not fixed here**:
  `Store::expand_retained_index_chains` applies `HARD_MAX_CHAIN_DELTAS` to a
  `visited` set shared by *every* retained root, so a store with more than a few
  retained checkpoints refuses index GC entirely:
  `index GC skipped: cannot resolve a retained segment chain: LimitExceeded:
  index segment chain exceeds the hard depth bound`
  (receipt `...T214500Z`, generation 10,000). The bound should be per root. The
  index tree then grows with commit count, and the 1 GiB bounded `/tmp` is
  exhausted at generation 80,785 (live 512) and 9,750 (live 10,000) — both runs
  are retained above. With the fix, a 100,000-generation sweep at 512 live
  objects would fit; without it, this document reports 64 live for the 100k
  point.
* **1,000,000 generations was not attempted**: the baseline index tree is linear
  in commits (~0.4 KB/commit at 64 live) and would need ~0.4 GB at 1M plus the
  commit files, above the 1 GiB bounded `/tmp` with the GC defect in place. Not
  extrapolated into a claim.
* **Duplicate relation edges** (same subject/predicate/kind/target, different
  provenance or attrs) collapse in a WFX3 edge patch, as they already did in
  `encode_delta`; the oracle in memory keeps them, so such a store's segment can
  be rejected on reopen and rebuilt. Safe, but a read-cost regression on that
  pathological shape. Not fixed.
* **In-memory cache validation** compares the candidates root recorded when the
  index was written instead of recomputing it on every publish; the disk path
  still recomputes it, and every reader re-checks the root of the bytes it
  loaded. This is what removes the second whole-index encode per commit.
* One host, one process, tmpfs, warm page cache, single writer. No power-loss,
  fleet, cloud, multi-writer, or Go-parity claim. No Go CLI verb changed.
* `self.index_cache` is not populated by the writer (the head's index lives in
  the attested writer cache); a query on a writing handle after a commit loads
  the persisted chain once per head, as the cold-read numbers show.
