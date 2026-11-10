# R9 — advisory index amplification: incremental WFX3 segments

Status: implemented in Rust and Go, measured. The index is still advisory:
correctness never depends on it, and any chain inconsistency fails closed to a
rebuild from replayed state. Design and format: `docs/INDEX-AMPLIFICATION.md`.

Receipts (all in the Docker bench court, default 3 GiB / 4 vCPU limits):

| receipt | what it is |
|---|---|
| `bench/receipts/index-chain-20261110T071008Z.json` | per-commit court: 1,250 sequential commits, full vs delta, GC, compaction, query p50 |
| `bench/receipts/index-chain-20261110T050010Z.json` | same court at the **row-level** delta iteration (superseded by the element-level encoder) |
| `bench/receipts/index-chain-20261110T045441Z.json` | first (row-level) run, superseded |
| `bench/receipts/trajectory-20261110T070000Z.json` | 50-step synthetic trajectory, delta, default 32-delta chains, query-reopen phase |
| `bench/receipts/trajectory-20261110T080000Z.json` | the same trajectory with **full segments per commit** (the pre-WFX3 control) |
| `bench/receipts/trajectory-20261110T090000Z.json` | the same trajectory, delta, chain bound raised to 256 deltas |
| `bench/receipts/trajectory-20261108T083234Z.json` | the historical receipt that produced the 3,466,466,150 B figure |

## The defect

A WFX2 segment is a full-state snapshot of four candidate maps. On a trajectory
that commits once per object, every commit wrote the whole state again. The
2026-11-08 trajectory court reported **3,466,466,150 B** of GC-eligible advisory
index for a 74 MB data tree (3,271 segments), i.e. roughly 47x the data.

WFX3 makes a segment either **full** or a **delta**: an immediate base segment id
plus only the changed index rows. A read loads the full ancestor and applies the
deltas in order. Deltas are **element-level**: an append to a row that holds a
set of object ids costs the appended id, not the whole set. Without element-level
patches the fix is nearly useless, because shared attributes
(`weft.tensor.dtype`, `weft.tensor.strategy`, …) put every object into a few
rows:

| encoder iteration (1,250 commits) | index bytes | × data |
|---|---:|---:|
| full segment per commit | 97,066,019 | 81.4x |
| delta, whole-row rewrite | 39,642,192 | 33.2x |
| delta, element-level patch | **3,342,744** | **2.80x** |

## Per-commit court (the clean measurement)

1,250 commits; 1,000 file creates plus 250 relation commits; 1,192,538 B of data
(`objects/` + `manifests/`). `index-chain-20261110T071008Z.json`.

| metric | full segment | delta chain | ratio |
|---|---:|---:|---:|
| index bytes | 97,066,019 | 3,342,744 | **29.0x smaller** |
| index bytes / data | 81.4x | 2.80x | 29.0x |
| index bytes written per commit, p50 | 77,892 B | 406 B | 192x |
| index bytes written per commit, p95 | 144,315 B | 408 B | 354x |
| commit wall p50 | 0.627 ms | 1.232 ms | **+0.605 ms (1.96x)** |
| commit wall p95 | 1.123 ms | 2.305 ms | +1.182 ms |
| first relation query after reopen, p50 | 0.258 ms | 1.411 ms | 5.5x slower |
| first `attrs.team == "red"` query, p50 | 0.405 ms | 1.638 ms | 4.0x slower |
| first `has(attrs.team)` query, p50 | 0.674 ms | 1.913 ms | 2.8x slower |
| first `select true`, p50 | 0.474 ms | 1.691 ms | 3.6x slower |
| GC: unreferenced segments removed | 1,245 / 96,596,161 B | 1,141 / 2,846,256 B | — |
| index bytes after GC | 469,858 | 496,488 | 1.06x |
| compaction wall | 1.245 ms | 1.205 ms | equal |
| compaction bytes added | 151,663 | 158,605 | equal |
| first query after compaction, p50 | 0.238 ms | 0.238 ms | equal |

The commit cost is real: the writer resolves the base chain and encodes a delta
on every commit (and still builds the full segment to prove the delta is
smaller). It is ~0.6 ms per commit here and grows with chain depth.

The read cost is real too: a chain load reads the full ancestor plus `D` deltas
and applies them. Compaction (a no-op commit with a full segment) restores the
full-segment read latency and lets GC drop the old chain.

Pinned retention shape (a snapshot at every commit, 2,250 commits total,
2,380,537 B data): index 5,960,610 B (2.50x data), GC reclaimed 0 bytes because
every commit is a retained root. Snapshots keep chains alive by design.

## Trajectory re-run

The 2026-11-08 court's series is 50 synthetic steps of 64 tensors each (3,200
tensor objects, 34,894,788 B of data). The reduced runs below skip the pythia and
Qwen halves (`WEFT_TRAJ_REAL_WEIGHTS=0`), set `WEFT_TRAJ_BASELINES=0`, and record
a first-query p50 after the series. All three are the same harness and fixture.

| phase / metric | 2026-11-08 WFX2 (historical) | full WFX3 control | delta, bound 32 | delta, bound 256 |
|---|---:|---:|---:|---:|
| series with per-step snapshots | 104,309,122 | 104,637,501 | **6,237,283** | **2,119,849** |
| … as % of 34,894,788 B data | 299.9 % | 299.9 % | **17.9 %** | **6.1 %** |
| series without snapshots | 52,154,621 | 52,318,831 | **3,469,887** | **2,111,799** |
| … as % of data | 149.5 % | 149.9 % | **9.9 %** | **6.1 %** |
| first query after reopen, p50 (snapshots) | not measured | 8.71 ms | 9.30 ms | **60.72 ms** |
| first query after reopen, p50 (no snapshots) | not measured | 11.19 ms | 24.81 ms | **38.55 ms** |
| index bytes before pinned GC | 3,531,842,070 | 211,986,259 | **113,586,041** | **111,436,515** |
| pinned GC reclaimed | 3,466,466,150 | 0 | **0** | **0** |
| index bytes after pinned GC | 65,375,920 | 211,986,259 | 113,586,041 | 111,436,515 |
| index bytes before unpinned GC | 3,479,687,569 | 159,667,589 | **111,737,231** | **111,428,465** |
| unpinned GC reclaimed | 3,466,891,299 | 146,473,617 | 98,528,065 | 98,124,368 |
| index bytes after unpinned GC | 12,796,270 | 13,193,972 | 13,209,166 | 13,304,097 |

What this says, including the parts that are worse than the headline:

* The series phase — the part that used to write one 1–2 MB full segment per
  commit — is now 6.2 MB (default bound) or 2.1 MB (bound 256) instead of
  104.6 MB. That is a **16.8x–49.4x** reduction on the phase that motivates the
  defect.
* The pinned GC number fell from **3.53 GB before / 3.47 GB reclaimed** to
  **0.11 GB before / 0 reclaimed**. Peak index bytes on this trajectory are
  **31.1x** lower (3,531,842,070 → 113,586,041). GC no longer has 3.47 GB to
  reclaim because it was never written.
* **GC can reclaim less than before when snapshots pin every step**: WFX2 could
  drop the 3,271 non-snapshot segments (leaving 65.4 MB); WFX3 retained every
  segment in this pass (leaving 113.6 MB). The chain is retained as long as a
  retained root names it, and this pass reported no unreferenced segment. That
  is a deliberate safety property plus a real post-GC regression on this shape;
  compaction is the way to bound it.
* The unpinned GC leaves ~13.2 MB in every mode: the retained head chain. WFX2
  wrote 3.48 GB transiently to end at 12.8 MB; WFX3 writes 0.11 GB to end at
  13.2 MB.
* **The default 32-delta bound is not what dominates the delete phase.** Raising
  it to 256 shrinks the series index 2.9x more but leaves the pre-GC delete-phase
  tree unchanged (111.4 MB vs 113.6 MB) and triples the first-query p50 (9.3 →
  60.7 ms at depth 99). The delete-phase deltas are ~33 KB each because one
  object's attribute/relation rows are removed per commit; a larger chain bound
  does not change that, it only moves the forced full segment.
* **The historical 3.47 GB cannot be reproduced by the current harness in full
  mode**: the current full-segment control peaks at 212 MB, not 3.5 GB, because
  the checkpoint writer now publishes a group commit per checkpoint instead of
  one commit per tensor and the delete phase shrinks the state faster. The fair
  before/after on *today's* harness is full 211,986,259 B vs delta 113,586,041 B
  (1.87x) for the pinned phase, and 104,637,501 B vs 6,237,283 B (16.8x) for the
  series. The per-commit court is the cleanest statement of the mechanism:
  97,066,019 B → 3,342,744 B.

Residual amplification that remains:

1. One delta segment per commit, even when it is small: ~406 B per append in the
   file court, ~33 KB per delete in the tensor trajectory. Total index is
   O(commits), not O(commits × state), but it is still per-commit.
2. A forced full segment every `index_chain_max_deltas + 1` commits (default 33).
3. Snapshots retain the whole chain of the commit they name; a snapshot per step
   makes the index tree un-reclaimable until those snapshots are dropped or the
   head is compacted.
4. Chain reads cost `D+1` file reads and decodes: 9.3 ms at depth ~16-31,
   60.7 ms at depth 99 for a 3,200-object index. Compaction returns this to
   0.24 ms in the file court.
5. WFX2 files are still read (compatibility) and are never rewritten; they are
   dropped by GC like any unreferenced segment.

## Reproduce

```sh
scripts/docker-court.sh bench "cd /work && cargo run --release --locked --example index_chain_bench"

WEFT_STAMP=<stamp> WEFT_TRAJ_REAL_WEIGHTS=0 WEFT_TRAJ_BASELINES=0 \
  WEFT_TRAJ_QUERY_REOPEN=1 WEFT_TRAJ_INDEX_MODE=delta \
  scripts/docker-court.sh bench "cd /work && bash bench/scripts/trajectory_court.sh"
# ... WEFT_TRAJ_INDEX_MODE=full for the control, and
# WEFT_TRAJ_INDEX_MAX_DELTAS=256 for the bound sensitivity run.
```

## Non-claims

Single host, warm page cache, synthetic fixtures, one process per phase; no
fleet, media, power-loss, or multi-writer claim. The Go implementation reads and
writes the same WFX3 bytes (full segment byte-identical; Rust and Go each load
the other's delta chain), but no Go timing is claimed here. The historical
receipt's commit shape differs from today's writer; both are cited rather than
conflated.
