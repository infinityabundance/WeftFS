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
| `index-gc-bound-20261111T194000Z-prefix-tail.json` | **retained failure**: GC refuses at 14 retained roots (item 2.1) |
| `index-gc-bound-20261111T190100Z-prefix-8roots.json` | the same refusal at 8 snapshots (union 268 segments) |
| `index-gc-bound-20261111T194500Z-fixed-tail.json` | the identical fixture after the fix: reclaims, no problems |
| `incremental-index-20261111T210000Z-post-gc-live64.json` | 64 live, 1k/10k/100k, GC at each checkpoint |
| `incremental-index-20261111T211000Z-post-gc-live512-sweep.json` | 512 live, 100k, GC every 20k |
| `incremental-index-20261111T213000Z-post-gc-live512-gap.json` | 512 live, GC only at 1k/10k/100k: **retained failure**, ENOSPC at 85,758 |
| `incremental-index-20261111T214000Z-post-gc-1m-live64.json` | the 1M attempt: **retained failure**, ENOSPC at 169,701 |
| `incremental-index-20261111T215000Z-post-gc-live64-200k.json` | 64 live, 200k generations, GC every 20k |

Each receipt has a `.log` beside it with the command, the per-checkpoint
summary and the full JSON.

Item 2.1's earlier runs are retained and **superseded**, not cited as current:
`index-gc-bound-20261111T190000Z-prefix.json` (12 roots before the unretained
tail), `…T193000Z-fixed.json` / `…T193100Z-fixed-8roots.json` (the fix before the
tail and per-root depth fields), and `incremental-index-20261111T200000Z-post-gc-
live64.json` / `…T201000Z-post-gc-live512.json` (before the allocation
accounting; the same measurements, without the tmpfs page breakdown).

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

## GC bound fix (item 2.1)

Status: implemented in Rust (`src/store.rs`) and Go
(`WeftFS-Go/store/index.go`), measured in the bench court at `686bce5` /
`4c4f459` with the default hard limits (3 GiB, 4 CPUs, pids 1024, bounded
`/tmp`). No limit was raised. The index stays advisory: correctness never
depends on a retained segment.

### The defect

`Store::expand_retained_index_chains` walked every retained root — the head,
every snapshot, and every commit carrying a state checkpoint — through **one**
`visited: BTreeSet<Hash32>` and compared that **union** against
`HARD_MAX_CHAIN_DELTAS` (256):

```rust
let mut queue: VecDeque<Hash32> = retained.iter().copied().collect();
let mut visited: BTreeSet<Hash32> = BTreeSet::new();
while let Some(commit) = queue.pop_front() {
    if !visited.insert(commit) { continue; }
    /* read the segment header; on a delta: */
    if visited.len() > HARD_MAX_CHAIN_DELTAS as usize {
        return Err(Error::limit_exceeded(
            "index segment chain exceeds the hard depth bound"));
    }
    /* retain and enqueue header.base_commit */
}
```

The bound was meant for one chain walk. Applying it to the union makes it a
budget shared by every root, so once a store retains more than about eight
roots (8 × ~33 segments) index GC refuses everything:

```
index GC skipped: cannot resolve a retained segment chain:
LimitExceeded: index segment chain exceeds the hard depth bound
```

Consequence: `report.problems` is non-empty, **zero** index segments are
deleted, and the index tree grows linearly with the commit count (the item-2
baseline filled the 1 GiB bounded `/tmp` at generation 80,785 with 512 live
objects and at 9,750 with 10,000 live).

### Reproduction (retained failing receipt)

`examples/index_gc_bound.rs` builds the reproducing shape: 12 snapshots, each
pinned when the writer's head chain is at its maximum 32-delta depth (so no two
chains overlap), one forced checkpoint commit, then 400 unretained tail commits
whose segments GC should be free to reclaim. Fixed clock, fixed content, one
process, tmpfs.

`bench/receipts/index-gc-bound-20261111T194000Z-prefix-tail.json` (revision
`e1ab002`; the receipt names the store revision it measured, and
`examples/index_gc_bound.rs` is committed in the follow-up commit that adds it):

```
retained: commits=801 index_before=369010 index_after=369010 data=20591 \
  segments_before=801 segments_after=801 deleted=0 deleted_bytes=0 refused=true
retained: problem: index GC skipped: cannot resolve a retained segment chain: \
  LimitExceeded: index segment chain exceeds the hard depth bound
retained: roots=14 union_segments=404 hard_max=256
```

14 retained roots, chain depths 1–32, and a union of **404 segments**. The
head-only cell of the same run reclaims 199 segments / **87,891 B** — the
single-root path was never affected.

Eight snapshots are enough:
`index-gc-bound-20261111T190100Z-prefix-8roots.json` (10 roots, union **268
segments**, 269 files, refused, 0 B reclaimed), which is exactly the threshold
the shared set implies.

### The fix

Each retained root is walked on its own path. The distance from a commit to its
full ancestor is memoised, so a commit shared by several roots is read once,
and a second root that reaches it is still charged its own cumulative depth:

```rust
let mut distance: BTreeMap<Hash32, u32> = BTreeMap::new(); // deltas to full
for root in roots {
    let mut path: Vec<Hash32> = Vec::new();
    let mut on_path: BTreeSet<Hash32> = BTreeSet::new();
    let mut current = root;
    loop {
        if let Some(&rest) = distance.get(&current) {
            if path.len() as u64 + u64::from(rest) > HARD_MAX_CHAIN_DELTAS.into() {
                return Err(/* LimitExceeded, same text */);
            }
            break;
        }
        if !on_path.insert(current) { return Err(/* cycle */); }
        /* read segment; on full/absent/non-WFX3: distance = 0, stop */
        if path.len() as u64 >= HARD_MAX_CHAIN_DELTAS.into() {
            return Err(/* LimitExceeded, same text */);
        }
        path.push(current);
        retained.insert(header.base_commit);
        current = header.base_commit;
    }
    /* write distance for every commit on the path, backwards */
}
```

**Invariant (as stated in the code comment).** For every retained root `r`, the
walk from `r` reaches a `Full` segment — or a commit with no WFX3 segment, which
has nothing to retain — after at most `HARD_MAX_CHAIN_DELTAS` (256) delta hops
measured **from that root**. The bound is never the cardinality of the union of
the retained chains. A commit shared by several roots is read once and its
memoised distance-to-full is added to each walker's own depth, so sharing cannot
hide an over-long chain. If any root violates the bound, or revisits a commit on
its own walk (a cycle), the whole pass refuses with the same typed class
(`LimitExceeded`), and no segment is deleted.

Files changed: `src/store.rs` (Rust), `WeftFS-Go/store/index.go` (Go, same
defect and same shape), `tests/index_gc_bound.rs`,
`WeftFS-Go/store/index_chain_test.go`, `examples/index_gc_bound.rs`, and the
allocation accounting in `examples/index_incremental_bench.rs`.

### Before / after on the identical fixture

`index-gc-bound-20261111T194500Z-fixed-tail.json` (`686bce5`) is the same
workload as the failing receipt above:

| | pre-fix (`e1ab002`) | post-fix (`686bce5`) |
|---|---:|---:|
| commits | 801 | 801 |
| retained roots | 14 | 14 |
| union of retained chains | 404 segments | 404 segments |
| index bytes before GC | 369,010 | 369,010 |
| index segments before GC | 801 | 801 |
| GC problems | 1 (refusal) | **0** |
| segments deleted | 0 | **397** |
| bytes reclaimed | 0 | **182,321 B** (397 segments / 182,321 B) |
| index bytes after GC | 369,010 | 186,689 |
| segments after GC | 801 | 404 |
| `verify --full` | clean | clean (801 commits, 33 manifests, 32 objects) |
| each snapshot's chain on a fresh handle | loads | loads, `builds = 0`, `rejects = 0`, answers = scan oracle |

Head-only control, same fixture, both revisions: 202 commits, 199 segments /
**87,891 B** reclaimed, 3 segments left, no problems. The single-root path is
byte-for-byte unchanged, which is what the shared set meant when it could only
ever see one root.

### Index footprint and reclaimed bytes at scale

All post-fix cells are GC at each named checkpoint (`WEFT_INDEX_GC=1`,
`gc_grace_secs = 0`). "index after" is the published steady-state tree; the
harness samples before its own GC, so both numbers are in the receipts.

64 live objects, one `set_xattr` commit per step
(`…T210000Z-post-gc-live64.json`):

| generation | commit p50 | index before GC | index after GC | data bytes | index / data (after) | segments reclaimed | GC wall |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1,000 | 0.110 ms | 828,646 | 87,033 | 110,342 | 0.789 | 943 | 28.3 ms |
| 10,000 | 0.111 ms | 7,454,358 | 897,882 | 1,307,846 | 0.687 | 8,373 | 279.2 ms |
| 100,000 | 0.110 ms | 74,518,557 | 8,754,798 | 12,983,837 | 0.674 | 84,057 | 2,877.8 ms |

Without GC the same 100,000-generation cell published 81,816,646 B (6.30x
data); with the bound fixed it publishes 8,754,798 B (0.674x) and each pass
reclaims the gap since the previous checkpoint.

512 live objects, GC every 20,000 generations
(`…T211000Z-post-gc-live512-sweep.json`):

| generation | index before GC | index after GC | data bytes | index / data (after) | segments reclaimed |
|---:|---:|---:|---:|---:|---:|
| 1,000 | 4,310,411 | 547,137 | 876,405 | 0.624 | 943 |
| 10,000 | 40,202,718 | 5,615,778 | 10,374,681 | 0.541 | 8,373 |
| 20,000 | 49,632,269 | 11,092,097 | 20,664,495 | 0.537 | 9,372 |
| 40,000 | 99,125,079 | 22,061,955 | 41,244,171 | 0.535 | 18,684 |
| 60,000 | 110,094,937 | 33,042,145 | 61,823,847 | 0.534 | 18,648 |
| 80,000 | 121,075,127 | 44,013,725 | 82,403,523 | 0.534 | 18,678 |
| 100,000 | 132,046,707 | 54,986,166 | 102,983,199 | 0.534 | 18,675 |

This is the cell the item-2 baseline could not reach: **512 live at 100,000
generations completes with GC**, and its steady-state index is 0.534x the data.
A 64-live sweep to **200,000 generations**
(`…T215000Z-post-gc-live64-200k.json`) completes too, with the post-GC
index/data ratio flat at 0.673–0.676 from 20,000 to 200,000 generations
(17,517,919 B index for 25,990,452 B data at 200,000; commit p50 0.110–0.115 ms)
while the uncollected tree at each sample stays bounded by the 20,000-generation
interval (28.6 MB before GC at 160,000).

### 1,000,000 generations: measured limiter, not extrapolated

`…T214000Z-post-gc-1m-live64.json` (`WEFT_INDEX_SIZES=1000,10000,100000,1000000`,
64 live, GC at those four points) exhausts the 1 GiB bounded `/tmp` at
generation **169,701**. The failure record includes the allocated-byte
breakdown, which sums to exactly 1,073,741,824 B:

| tree | files | apparent bytes | allocated bytes |
|---|---:|---:|---:|
| `commits/` | 169,702 | 136,154,644 | 695,152,640 |
| `indexes/` | 76,328 | 65,771,211 | 353,648,640 |
| `manifests/` | 726 | 22,031,479 | 24,666,112 |
| `objects/` | 64 | 438 | 262,144 |
| whole store | 246,825 | 223,957,772 | **1,073,741,824** |

The limiter is not the index bound: `commits/` alone is 695,152,640 B for
169,702 files, exactly **4,096 B of commit pages per generation** (tmpfs
allocates a whole page for a ~140-byte commit file). One commit file per
generation is the store's history, so 1,000,000 generations need 4,096,000,000 B
of commit pages before any index exists — 3.81 times the court's 1 GiB `/tmp`
with a correct GC or with none. The index contributes a further ~4.6 KB per
uncollected commit between checkpoints. This is reported as the measured
limiter; nothing is extrapolated and no court limit was raised.

The 512-live cell shows the same effect without the 1M ambition: with GC only at
1k/10k/100k (`…T213000Z-post-gc-live512-gap.json`) the store dies at generation
85,758 with `indexes/` at 628,711,424 B allocated and `commits/` at
351,719,424 B. Checking the GC bound does not make long GC intervals free; GC
frequency is part of the cell.

### Tests

* `tests/index_gc_bound.rs` 3/3.
  `many_retained_roots_each_with_a_long_chain_are_reclaimed`: 10 snapshots plus
a checkpoint, union > 256; after GC there are no problems, segments are
reclaimed, every root's whole chain is present, every snapshot opens a fresh
handle with `disk_loads >= 1` / `builds = 0` / `rejects = 0` and answers
exactly like `resolve_scan`, and `verify(true)` is clean.
  `a_single_over_long_chain_still_refuses_and_deletes_nothing`: a forged
300-delta chain (the writer never builds one) is refused with
`LimitExceeded: index segment chain exceeds the hard depth bound` and the
segment count is unchanged. `a_chain_without_retention_reclaims_exactly_the_
unnamed_segments`: the deleted count and bytes equal the exact set outside the
head chain, and the tree shrinks by exactly that.
  The first test was run against `e1ab002`'s `src/store.rs` and failed at the
"GC must resolve every per-root chain" assertion; it passes on the fix.
* Go twin: `WeftFS-Go/store/index_chain_test.go`
  `TestGcChainBoundIsPerRetainedRoot` (10 snapshots + a checkpoint, union > 256,
reclaim, every chain loads with `Builds == 0`) and
`TestOverLongRetainedChainStillRefusesIndexGc` (forged 300-delta chain,
nothing deleted, same typed text). The first fails on the previous Go code with
the same message and passes on the fix.
* Item-2 oracle and adversarial set re-run unchanged: `tests/index_incremental`
3/3 (byte-exact WFX2 sections against `StateIndex::build` over 10,000 ops per
seed 1/7/2026, group commits, damaged/absent/orphan segments),
`tests/index_chain` 8/8 (partial, forged root, swapped ancestor, compaction,
snapshot pinning, bounds), `tests/index_persistence` 7/7,
`tests/index_equivalence` 1/1. Commit p50 stays 0.110 ms at 1k/10k/100k, so the
incremental path is still flat.
* Suites at `4c4f459`: Rust `cargo test --all-targets` 163 binaries / 787 passed
/ 0 failed / 0 ignored; FUSE `--features fuse --all-targets` 163 binaries /
830 passed / 0 failed / 2 ignored (the two pre-existing ignores). Go `gofmt`
clean, `go vet ./...` clean, `CGO_ENABLED=0 go build ./...` clean,
`go test -race ./store` passes except the pre-existing
`TestAzureOptimisticTwoDisjointCommitters`, which fails identically on the
unmodified tree (the WeftFS-Go HEAD carries unverified R14d Azure-MW2 work).
The two `fusefs` writeback tests need `/bin/fusermount`, which the go court does
not provide; they are not related to this change.

### Not fixed, not claimed

* Go has the GC retention fix only. It still has **no incremental index
maintenance** and no Go timing is claimed here.
* The retained set is still one root per state checkpoint (every 256 commits by
default) and one per snapshot, so the post-GC index still grows with history: at
64 live objects 8.75 MB at 100k and 17.52 MB at 200k (0.674x data). Making that
flat needs a snapshot/checkpoint retention policy, which is a product decision,
not a bound bug.
* GC cost is O(commits) per pass because `recovery::all_commits_via` reads every
commit file (2.9 s at 100k generation, 5.0 s at 200k in the 64-live cells). That
is why the long-interval cells exhaust `/tmp` before the index does.
* One host, tmpfs, warm page cache, single writer, one process. No physical
power-loss, fleet, cloud, or multi-writer claim. No wire-format change: WFX2,
WFX3, `weftfs.commit.v2` and every attested root are untouched.

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
* **Suites**: Rust `cargo test --all-targets` 784 passed / 0 failed
  (161 binaries, measured at `8d324dc`); FUSE
  `cargo test --features fuse --all-targets` 827 passed /
  0 failed / 2 ignored (the two pre-existing ignores). Go store suite:
  `go test -race ./store -run 'Index|Handoff'` PASS (78.7 s).
  (The `7d7e2af` commit message quotes 782: that run predates the last two unit
  tests; no code changed after the 784-test run except documentation.)

## Losses, residual risks, not claimed

* **The live-state dependency is reduced, not removed** (4.05x at 10,000 live
  objects, but still linear in the live index because of the attested candidates
  root). `commit p50` is used as a proxy for cost; CPU was not sampled.
* **Pre-existing GC defect found by this court, fixed in item 2.1** (see the
  "GC bound fix" section): `Store::expand_retained_index_chains` applied
  `HARD_MAX_CHAIN_DELTAS` to a `visited` set shared by *every* retained root, so
  a store with more than a few retained checkpoints refused index GC entirely:
  `index GC skipped: cannot resolve a retained segment chain: LimitExceeded:
  index segment chain exceeds the hard depth bound`
  (receipt `...T214500Z`, generation 10,000). The index tree then grew with
  commit count, and the 1 GiB bounded `/tmp` was exhausted at generation 80,785
  (live 512) and 9,750 (live 10,000) — both runs retained above. The bound is
  now per retained root, in Rust and Go; the 512-live 100,000-generation sweep
  fits, and the 1,000,000-generation attempt is bounded by tmpfs page overhead
  rather than by this defect (measured in the same section).
* **Index footprint is unchanged by item 2** (the same sealed segments; 7.5x data
  at 64 live without GC, 0.674x with GC, 6.3x with 100k generations uncollected,
  3.9x at 512 live/10k with GC). This item does not make the index tree smaller,
  only cheaper to produce; item 2.1 lets GC actually reclaim it.
* **1,000,000 generations was attempted and does not fit**: the measured floor is
  4,096 B of commit pages per generation (169,702 commit files = 695,152,640 B
  allocated), so 1M needs ~4 GiB of `commits/` alone. Not extrapolated into a
  claim and the court limit was not raised.
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


## Aged-store court (T012 / N13), measured 2026-11-13

Court: `scripts/index_aged_court.sh` around `examples/index_aged_bench.rs`; one
store per cell, one process, default bench court limits. `gens` grows history
with 64 live objects; `live` grows the state and runs 200-300 measured commits.
Receipts: one `bench/receipts/index-aged-<stamp>.json` per cell; retained
failures `index-aged-20261113T193825Z.failure.log` (1M live objects, OOM-killed)
and `index-aged-20261113T213345Z.failure.log` (50k live objects, first refused
commit at generation 229).

<!-- BEGIN GENERATED:aged-index-table (scripts/perf_numbers.py) -->
<!-- END GENERATED:aged-index-table -->

Correctness at every passing cell: the persisted index's ordered answers equal
the scan oracle for relation, tag, attr-eq and has-attr selectors; the writer's
maintained WFX2 sections equal a full `StateIndex::build`; a reopen with
`indexes/` renamed away rebuilds and still matches; GC with a snapshot pin
retains every segment a retained root names (missing = 0) and reclaims the
rest; a forged head segment (one tag row removed, otherwise valid) is refused
by the commit's candidate root and rebuilt to exact answers. Cross-language:
`WeftFS-Go/store/index_aged_handoff_test.go` opens the Rust-written 10k-live,
10k-generation and 100k-generation stores, loads the Rust segment
(`DiskLoads>0`, `Builds==0`) and reproduces the Rust CLI's **ordered** answers
and Go's scan oracle (PASS, 3.19 s).

Three defects were found and fixed at the root, not re-framed:

1. `State::apply_ops` cloned the whole state per commit, so replay and group
   commits were O(state). A journal of the touched slots replaced it (10k live
   objects: cold open 642 -> 210 ms on the same fixture).
2. The WFX3 reader required `edgeCount * 32` remaining bytes for a relation
   row; a value target is one kind byte plus its payload, so any segment whose
   last relation row held such an edge was refused and rebuilt on every reopen
   (`compact_index` produced one). The bound is now the true one-byte minimum
   with a capped initial capacity, in Rust and Go.
3. GC and the retained-root diagnostic materialised every commit to find
   checkpoint roots; at 1,000,000 generations that vector OOM-killed the court.
   `recovery::for_each_commit_via` streams the scan and retains only the
   checkpoint rows.

The 1,000,000 **live-object** cell is not reachable in the 3 GiB court, and the
reason is measured: a checkpoint state manifest is a canonical WCB value
encoded with the codec's default 1,000,000-item bound, and
`examples/state_manifest_bound.rs` reports 50,000 objects encoding (2.4 MB per
10k) while 100,000 objects alone already exceed the bound. With entries and
relations the first checkpoint is refused at 50,000 live objects. A
100,000-object store therefore cannot checkpoint at all
(`checkpoints = 0` above); the target 1M attempt additionally exceeded 3 GiB
during seeding. This is a limit, not an index defect: correctness is unchanged
(the index rebuilds), but bounded open and checkpoint roots are unavailable. A
sharded or separately-bounded state manifest is the fix and is not implemented
here.
