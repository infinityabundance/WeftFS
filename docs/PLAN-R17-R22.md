# Plan R17–R22 — compressed-write parity

**Status:** R17 executed; R18–R22 planned. This plan is driven by the measured
ledger in [`bench/RESULTS-write-cost-ledger.md`](../bench/RESULTS-write-cost-ledger.md),
not by intuition. Nothing in R18–R22 may weaken the exactness or durability
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
| G1 | ≤ 3,360 | 10× A's 336 ms |
| G2 | ≤ 1,344 | 4× A |
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

### R18 — candidate elimination (next)

1. Remove the level-19 candidate from the default planned path, or make it
   opt-in outside the critical path. Evidence: zero selected objects and zero
   bytes saved on the frozen fixture for 20.35 s of encode.
2. Re-price the remaining candidates with the write lambda so a candidate must
   earn its encode time, not only its bytes.
3. Keep the exactness contract: the same object identity, the same chosen
   bytes, the same restore.
4. **Gate:** C-noz19-like wall and footprint, 101/101 exact, crash old-or-new.
5. Expected: C at ~3.85 s / 282.3 MB → **G1 met** (3.85 s is within 3.36 s only
   after R21 or a further ~0.5 s reduction; report honestly either way).

### R19 — BF16 mode-9 encoder

Targets from the stage profile: emission is 79 % of Huffman, histogram 17 %,
tree/code construction is free; frame assembly is 447 ms for the embedding.
1. A faster bit-emission kernel (wide accumulator, table-driven symbol lookup)
   with byte-identical output and the same differential court.
2. Remove avoidable frame-assembly copies; keep `payload_hash` and the exact
   stored plane layout.
3. **Gate:** byte-identical mode-9 frames, the existing conformance vectors
   unchanged, encoder ≥ 2× faster with no footprint change.
4. Expected: mode 9 from 1.74 s to ≤ 0.9 s, footprint unchanged at 282.3 MB.

### R20 — publication tax

1. Apply batched/group publication to the C path (the shipped `Store::group`
   already saves 689 ms of control-plane work for 101 tensors).
2. Evaluate the pack plane for the 330 MB C object set; it trades requests and
   metadata for bytes, so it must be judged on both boards.
3. **Gate:** no change to commit-record, manifest or index bytes that a reader
   depends on; crash old-or-new; footprint delta reported.

### R21 — bounded parallel pipeline

1. Bounded parallel tensor encode (mode 9 and ZSTD-3) with the existing
   `parallel::run_bounded` discipline; fixed worker and in-flight byte bounds.
2. Bounded parallel blob publication through the existing read/write handle
   pool and single-flight rules.
3. **Gate:** byte-identical output at every worker count; RSS inside 3 GiB;
   exact restore; report the Amdahl cap and the achieved fraction.
4. Expected: C-noz19 from 3.85 s toward the G1 rung.

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
| A | 326.4 | 424,439,296 | anchor | — |
| C (R17) | 24,229.2 | 282,283,011 | above G1 | final band |
| C-noz19 | 3,849.9 | 282,283,011 | 1.15× G1 | final band |
| C-both | 2,132.6 | 330,532,422 | between G2 and G1 | first-success band |
| B-group | 2,139.3 | 424,608,542 | between G2 and G1 | no reduction |

The target is the **knee**: C-noz19's footprint at C-both's wall, then move down
the ladder without leaving the 340 MB (first) and 284.14 MB (final) bands.

## 6. Non-negotiables

- No frozen format change (frame, manifest v1–v4, commit v1/v2, WFX2 index).
- No lossy decode, no approximate candidate, no silent fallback.
- Every run: 101/101 exact restores and old-or-new-whole on the crash proxy.
- Every number carries command, environment, fixture, raw output and revision.
- Failures and losses are retained and published.
- Single host, warm cache, tmpfs-free local filesystem, no fleet or power-loss
  claim unless a court actually establishes it.
