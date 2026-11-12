# Plan R17–R22 — compressed-write parity

**Status:** R17, R18, R19 and R20 executed; R21–R22 planned. This plan is driven
by the measured ledger in
[`bench/RESULTS-write-cost-ledger.md`](../bench/RESULTS-write-cost-ledger.md),
its R18 correction and the R20 publication ledger
([`bench/RESULTS-publication-tax.md`](../bench/RESULTS-publication-tax.md)), not
by intuition. Nothing in R21–R22 may weaken the exactness or durability
contracts to hit a number.

## 1. The objective

Native-write parity **while retaining as much of the measured physical
reduction as possible** — not parity achieved by disabling compression.
"Native" means A: the same backend's own durable per-tensor write path for the
same 101 tensors, with no WeftFS layer. The comparator is never the reference
FUSE filesystem.

Frozen fixture for every rung: Qwen2.5-0.5B-Instruct, 101 tensors,
424,439,296 logical bytes, 4 CPUs, 3 GiB court, the methodology in
[`bench/CONTRACTS-augmentation.md`](../bench/CONTRACTS-augmentation.md). A and C
are measured in the same session; a claim that crosses sessions states both.

## 2. The two scoreboards

A change is only a candidate if it reports **both** boards together.

### Board 1 — durable write wall (ms)

| Rung | Target | Meaning |
|---|---:|---|
| start | 24,229.2 (C, least-contended) | R17 measured |
| R18 | 4,117.8 | two-stage planner |
| R19 | 2,328.4 | mode-9 encoder |
| **R20** | **1,236.8** | one atomic publication per checkpoint |
| G1 | ≤ 3,360 | 10× A's 336 ms (cleared at R18) |
| G2 | ≤ 1,344 | 4× A (**met at R20**) |
| G3 | ≤ 672 | 2× A |
| final | ≤ 336 | A parity |

A is 326–365 ms on this host; the ladder uses 336 ms as the parity anchor so a
rung cannot be passed by choosing a favourable A sample. Every rung reports the
same-session A.

### Board 2 — physical footprint (B)

| Rung | Target | Meaning |
|---|---:|---|
| start | 282,283,011 (C) | R17 measured (33.5 % below A) |
| first success | ≤ 340,000,000 | R3 contract's first success |
| final | ≤ 284,140,000 | the R3-era 284.14 MB, i.e. no meaningful loss |
| **R20** | **282,106,049** | one commit record and one index segment per checkpoint |

**No rung may be reported as a wall win without its footprint delta, and no
footprint win without its wall delta.** A row that is faster and larger says
so; a row that is smaller and slower says so.

## 3. The rule that closes the obvious loophole

**Deferred or background transcode does not count as compressed-write parity.**
Publishing raw bytes fast and compressing them later is a legitimate system
feature, but it does not make the *write* compressed. A rung may use it only if
it reports the write wall and the compressed footprint separately, labels the
compressed form as post-write, and still meets the wall target **with the final
representation already durable** for the objects it claims. The same applies to
FUSE writeback, page-cache tricks, and any "measure after the caller returns"
scheme: the clock stops when the durable compressed state exists, not when the
API returns.

## 4. The six phases

### R17 — attribution and ablation (done)

Established: C = 24,229 ms, of which ZSTD-19 is **20,353 ms (84.0 %)** and is
selected for **zero** tensors; mode 9 is **1,743 ms (7.2 %)** and buys 48.1 MB
(14.5 %); publication/commit is **1,489 ms (6.1 %)**; everything else is
≤ 1.8 % each; the residual is **15.9 ms (0.065 %)**. The one-group-commit B
shows the control-plane tax is 689 ms for 101 commits and collapses to 8.8 ms.
C without the two expensive candidates (`C-both`) is **2,133 ms / 330.5 MB** —
already at G2's wall with a 22.2 % footprint win over B.

### R18 — candidate elimination (done)

Implemented as **analyse → select → materialise**, not as a deletion: level 19 is
conditional, because on the pythia-70m trajectory it is smaller than level 3 for
26 of 26 eligible tensors. Evidence
[`bench/RESULTS-planner-two-stage.md`](../bench/RESULTS-planner-two-stage.md),
receipt `planner-two-stage-20261112T081151Z`.

1. **Done.** `src/two_stage.rs` samples 4 codec chunks at level 3 and 2 at level
   19, predicts mode 9 **bit-exactly** from a full exponent histogram, and
   encodes a candidate only when its interval could change the winner or the
   mode-9 `1/64` gate. Level 19 is decided after level 3 is measured, against a
   measured incumbent; every skip records its bound.
2. **Done.** Level 19 is not removed: it is gated by its own predicted lower
   bound against a measured incumbent, and it still runs where it wins (pythia).
3. **Done.** Object identity, chosen bytes and restores are unchanged: 0 of 101
   per-tensor decision changes, footprint 282,283,011 B identical, 101/101 exact,
   crash old-or-new-whole.
4. **Gate: met on storage, missed on wall.** C two-stage is **4,117.8 ms /
   282,283,011 B** (7 reps, least contended) against the 3,850 ms expectation: a
   **7.0 % miss**, 1.23× G1. Measured cause: analysis costs 348.8 ms and the 66
   skipped mode-9 encodes were 93,696 bytes in total. Two-stage C is 182 ms
   slower than the C-noz19 ablation (3,936.0 ms).
5. **Consequence for the ladder.** G1 (3,360 ms) now needs ~0.76 s, which must
   come from mode-9 encoder work (R19), batched publication (R20) and the
   bounded parallel pipeline (R21, which also parallelises the analyse pass).

### R19 — BF16 mode-9 encoder (done, 2026-11-12)

Targets from the stage profile: emission is 79 % of Huffman, histogram 17 %,
tree/code construction is free; "frame assembly" 447 ms for the embedding.
1. A faster bit-emission kernel (wide accumulator, table-driven symbol lookup)
   with byte-identical output and the same differential court. **Done**:
   `EncoderTable` + a four-byte-flush `BitStream`; the byte-at-a-time emitter is
   kept as the test oracle.
2. Remove avoidable frame-assembly copies; keep `payload_hash` and the exact
   stored plane layout. **Done**: one allocation, planes written in place, one
   CRC pass. The faithful replica showed the real cost was the generic
   splitter (705.8 ms) and the emitter (301.1 ms), not the copies (74 ms).
3. **Gate:** byte-identical mode-9 frames, the existing conformance vectors
   unchanged, encoder ≥ 2× faster with no footprint change. **Met**: 2.85x
   sequential, 5.81x at four emission threads, 0 byte mismatches, footprint
   unchanged at 282,283,011 B.
4. Expected "mode 9 from 1.74 s to ≤ 0.9 s": **met and exceeded** — C's write
   wall went 4,101.6 → 2,328.4 ms (−43.2 %) and E 4,156.4 → 2,363.2 ms.
   Receipts in `bench/RESULTS-mode9-encoder.md`.

### R20 — publication tax (done, 2026-11-12)

1. Applied batched/group publication to the C path: the 101 tensor files are
   published in **one group commit**, and the group is also **one durability
   barrier**. **Done**: `Options::defer_group_blob_sync` (default on) makes
   `begin_group` open a deferred batch; blob publications write and rename
   eagerly and the barrier the commit path already performed before the commit
   record (`flush_buffered_blobs`) syncs every recorded file and every touched
   directory. Every file still gets its own data barrier — the batch changes
   when, never whether, a byte is durable. The barrier is reported by
   `StorageBackend::durability_barriers()` and timed as `Phase::BackendSync`.
2. Evaluated the pack plane on the C object set at 64 KiB / 1 MiB / 8 MiB
   member bounds, with the catalog bytes counted. **Done and rejected**: it is
   20.7–36.9 % slower and writes more bytes, for a 3.6–5.9 KB footprint win.
3. **Gate:** no change to commit-record, manifest or index bytes a reader
   depends on (only *fewer* commit records and *one* index segment per
   checkpoint; no field, schema or attestation changed); crash old-or-new on
   all three phases for every row; footprint delta reported. **Met.**
4. Result: C **2,343.2 → 1,236.8 ms** (−47.2 %) at **282,106,049 B** (−176,962
   B). Evidence [`bench/RESULTS-publication-tax.md`](../bench/RESULTS-publication-tax.md),
   receipt `r20-publication-20261112T133000Z`.

### R21 — bounded parallel pipeline

1. Bounded parallel tensor encode (mode 9 and ZSTD-3) with the existing
   `parallel::run_bounded` discipline; fixed worker and in-flight byte bounds.
2. Bounded parallel blob publication through the existing read/write handle
   pool and single-flight rules.
3. **Gate:** byte-identical output at every worker count; RSS inside 3 GiB;
   exact restore; report the Amdahl cap and the achieved fraction.
4. Expected: C from 1,237 ms toward the G3 rung. The measured shares to beat
   are encode 704.6 ms (57.0 %), blob publication 272.2 ms (22.0 %) and the
   durability barrier 97.0 ms (7.8 %).

### R22 — Pareto sweep

1. Sweep the surviving candidates (mode 9 on/off, ZSTD-3 level, optional level
   19) against the two boards; publish the Pareto front, not a single point.
2. Re-run A/B/C/D/E at the chosen points; the ladder rung and footprint rung
   are claimed only from same-session runs.
3. **Gate:** full Rust/FUSE suites, claims, conformance, crash proxy; every
   loss in the results document.

## 5. Current position

| Configuration | Wall (min) | Footprint (B) | Ladder | Footprint |
|---|---:|---:|---|---|
| A (same session) | 339.0 | 424,439,296 | anchor | — |
| **C (R20 default)** | **1,236.8** | **282,106,049** | **G2 met (3.65× A)** | final band |
| C-percommit (R19 two-stage) | 2,343.2 | 282,283,011 | 1.74× G1 | final band |
| C-group (group, eager barriers) | 1,625.1 | 282,106,049 | 1.21× G1 | final band |
| C-pack1m | 1,483.7 | 282,102,473 | loss | loss |
| C-both | 1,020.5 | 330,355,860 | 1.54× G1 | first-success band |
| C-nom9 | 21,427.2 | 330,160,010 | above G1 | first-success band |
| C-legacy (exhaustive) | 21,526.6 | 282,106,049 | above G1 | final band |
| E (R20) | 1,257.1 | 282,157,242 | 1.91× G1 | final band |
| D (R20) | 957.5 | 424,659,735 | 1.42× G1 | no reduction |
| B (R20 default) | 900.2 | 424,608,542 | 1.39× G1 | no reduction |

(R20 runs, least-contended of 7 interleaved repetitions at WeftFS `ad0ca3b`;
median and p95 are in the receipt and in the results document. `B` and `D` are
raw-representation rows: their minima beat C's because they encode nothing.)

The target is the **knee**: C's footprint at the lowest sustainable wall, then
down the ladder without leaving the 340 MB (first) and 284.14 MB (final) bands.
R18 moved the default from 24.2 s to 4.12 s, R19 to 2.33 s, and R20 to
**1.24 s at a footprint 177 KB smaller than R19's**. The remaining 0.56 s to the
G3 rung (672 ms) is encoder and pipeline work (R21): the commit section is now
**105.4 ms of a 1,237 ms write (8.5 %), of which 97.0 ms is the durability
barrier itself**, and the control plane (commit record, head and index) is
8.4 ms (0.7 %).

## 6. Non-negotiables

- No frozen format change (frame, manifest v1–v4, commit v1/v2, WFX2 index).
- No lossy decode, no approximate candidate, no silent fallback.
- Every run: 101/101 exact restores and old-or-new-whole on the crash proxy.
- Every number carries command, environment, fixture, raw output and revision.
- Failures and losses are retained and published.
- Single host, warm cache, tmpfs-free local filesystem, no fleet or power-loss
  claim unless a court actually establishes it.
