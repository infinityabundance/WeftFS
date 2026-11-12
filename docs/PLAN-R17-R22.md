# Plan R17–R22 — compressed-write parity

**Status:** R17-R22 **complete**. This plan is driven
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

### R21 — bounded parallel pipeline (done, 2026-11-12)

1. **Done.** `Store::encode_tensor_object` + `Store::publish_encoded_tensor`
   split prepare into a pure CPU stage and a pure I/O stage;
   `Store::prepare_tensor_group_pipelined` (`src/tensor_pipeline.rs`) runs
   reader → N encoder workers → one publisher behind bounded **byte** queues
   (staging 256 MiB, worker-resident 1 GiB, encode 256 MiB, publish 128 MiB)
   plus a CPU-token budget, FIFO admission and oversize admission. Not
   `run_bounded`: a group needs a separate reader, a separate publisher and a
   writeback thread, which `run_bounded`'s single consumer cannot express.
2. **Done, differently from the plan.** Publication was *not* parallelised:
   the backend mutex serialises it and one publisher keeps up (409.6 ms of
   busy time against a 448.8 ms encode wall). What publication needed was the
   **early barrier**: the publisher drains the open deferred batch's file list
   every 8 MiB and a writeback thread `fsync`s those files outside the backend
   lock, which moves the group's 282 MB of writeback under the encoders and
   takes the commit barrier from 329.6 ms to 1.3 ms.
3. **Gate:** byte-identical output at every worker count (same head, manifest
   ids, content roots and manifest bytes at 1/2/4/8, `verify --full` clean);
   RSS inside 3 GiB (measured peak 921 MiB, stated bound 1,664 MiB); 101/101
   exact restores; the Amdahl cap reported — **met**.
4. **Result:** C **1,206.2 → 623.3 ms** (−48.3 %) at a **byte-identical**
   282,106,049 B. **G3 (≤ 672 ms) met** on the least-contended repetition and
   on the median (653.9 ms); best measured configuration 593.9 ms
   (`--pipeline-cpu-tokens 3`); E 1,259.5 → 663.0 ms. The Amdahl cap is the
   largest tensor: at 2, 4 and 8 workers the encode wall equals the 272 MB
   embedding's own 448.8 ms, so 1→4 is 1.71× of an ideal 4×. Evidence
   [`bench/RESULTS-parallel-pipeline.md`](../bench/RESULTS-parallel-pipeline.md),
   receipt `r21-pipeline-20261112T201500Z`.

### R22 — write frontier (done, 2026-11-12)

1. **Done, and it refuted its own projection.** Chunk-parallel ZSTD is
   implemented (`weftcodec::modes::encode_zstd_with`), proved byte-identical
   (codec court 5/5; every timed frame in the receipt) and it cuts the 272 MB
   embedding's level-3 frame from 280.3 ms to 135.7 ms at four threads — but the
   frozen fixture's wall does not move, because **publication (452.5 ms) and the
   durability writeback (461.6 ms for 202 files) are the binder**. The projected
   ~430 ms write is recorded as refuted, with the stage measurements that refute
   it.
2. **Done: `analyse` parallelised exactly rather than fused.** The histogram is
   integer counts, so whole-element windows summed across threads are
   bit-identical; the R18 exact mode-9 prediction is unchanged and re-proved
   (`histogram_parallel` 3/3 + the planner court `two_stage_planner` 7/7 +
   `two_stage_matches_exhaustive`/`two_stage_storage_unchanged`). What could not
   be fused is named: the histogram decides whether the encode it would be fused
   into runs at all, and the dominant analyse cost is the level-19 *sample*
   (156.9 ms), not the histogram (37.1 ms).
3. **Done: measured defaults.** `cpu_tokens = encode_threads - 1` (592.6/600.9
   against 612.1/644.0) and a chunk pool bounded to `min(available, 4)` threads
   that runs only when the tensor's encode has the CPUs to itself.
4. **Done: the Pareto sweep** — 25 rows x 7 interleaved repetitions, device
   control, native-control band, Pareto set stated explicitly
   (`r22-pareto-20261112T230000Z`), plus cross-family (0.5B base/instruct,
   pythia step 0/8, Qwen 3B-Instruct) and MinIO arms.
5. **Gate:** Rust 807/0/0, FUSE 850/0/2 ignored, claims, spec drift,
   conformance manifest, cross gate, crash proxy — all met. Evidence
   [`bench/RESULTS-pareto.md`](../bench/RESULTS-pareto.md).

### What remains after R17-R22

* **The final rung (<= 336 ms) is not reached** (592.6 ms, 1.78x A). The measured
  limiter is the non-overlappable durable tail (282 MB written and `fsync`ed as
  202 files in 461.6 ms against native's 424 MB in 332 ms) over a 288 ms CPU
  floor. The single change most likely to close it: seal the pack plane and
  publish its catalog **on the early-barrier thread**, so fewer, larger durable
  objects do not pay a critical-path sealing cost (packs already halve publisher
  busy time, 391 -> 220 ms, and remove all 202 writeback files).
* A fusion of the two whole-tensor digest passes into the level-3 chunk loop
  (~75 ms of CPU, ~19 ms of wall) with a fresh byte-identity obligation.
* Go parity for chunk-parallel ZSTD, the parallel histogram and the R22
  defaults; the R22 pipeline over S3; Qwen2.5-3B base and Qwen2.5-7B beyond the
  635 MB subset measured here.
* A quiet-host re-run of the 576-612 ms cluster: the host's demonstrated
  variance (the same row's median moving 13 % between sessions) is currently
  larger than every difference inside that band.

## 5. Current position

R21 row values are the least-contended of 7 interleaved repetitions in the
same session (`r21-pipeline-20261112T201500Z`); the R18–R20 rows are their own
sessions' cited numbers and are kept for the ladder history.

| Configuration | Wall (min) | Footprint (B) | Ladder | Footprint |
|---|---:|---:|---|---|
| A (same session as R21) | 333.5 | 424,439,296 | anchor | — |
| **C (R21 default pipeline, 4 workers)** | **623.3** | **282,106,049** | **G3 met (1.87× A)** | final band |
| C-pipe4-tokens3 | 593.9 | 282,106,049 | G3 met (1.78× A) | final band |
| C-pipe8 | 612.1 | 282,106,049 | G3 met | final band |
| E-pipe4 | 663.0 | 282,157,248 | G3 met | final band |
| C (R20 sequential default) | 1,206.2 | 282,106,049 | G2 met (3.62× A) | final band |
| C-pipe2 | 986.8 | 282,106,049 | 3.0× A | final band |
| C-pipe1 | 1,114.1 | 282,106,049 | 3.3× A | final band |
| C-pipe4-nosync | 892.2 | 282,106,049 | 2.7× A | final band |
| **C (R20 cited)** | **1,236.8** | **282,106,049** | **G2 met (3.65× A)** | final band |
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
R18 moved the default from 24.2 s to 4.12 s, R19 to 2.33 s, R20 to **1.24 s at
a footprint 177 KB smaller than R19's**, and R21 to **623.3 ms at the same
282,106,049 B** — the G3 rung, cleared without buying it with bytes. What is
left to the final rung (≤ 336 ms) is one tensor: 448.8 ms of the 623.3 ms write
is the 272 MB embedding's own encode, of which ~295 ms is a level-3 ZSTD frame
built from 4,155 independent chunks. Parallelising those chunk encodes is
byte-identical by construction and is R22's first item.

## 6. Non-negotiables

- No frozen format change (frame, manifest v1–v4, commit v1/v2, WFX2 index).
- No lossy decode, no approximate candidate, no silent fallback.
- Every run: 101/101 exact restores and old-or-new-whole on the crash proxy.
- Every number carries command, environment, fixture, raw output and revision.
- Failures and losses are retained and published.
- Single host, warm cache, tmpfs-free local filesystem, no fleet or power-loss
  claim unless a court actually establishes it.
