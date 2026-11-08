# Stage 3 — stronger compression candidates in the planner

Revision: parent `staging` (see the commit that adds this file). Courts: `weft/bench-court:1`
(`sha256:4c6c41fb…`), 4 vCPU / 3 GiB for the Pythia runs, **8 vCPU / 20 GiB for the Qwen
runs** (the 988 MB base plus `zstd --patch-from` does not fit in 3 GiB; the override is
recorded in each receipt under `environment.court_memory` / `court_cpus`). One host, warm
page cache, synthetic/real public weights.

Harness: `examples/compression_candidates.rs`. It is **plan-only**: it sums each tensor's
`chosen_physical_bytes` from `Store::plan_tensor` and then writes no checkpoint, so store
manifests, commit records and index segments are **excluded**. The zstd CLI baselines are
whole-file, independent processes. The two totals are therefore not byte-for-byte
comparable; the caveat is repeated in every receipt (`non_claims`).

## What was added

* **ZSTD level 19 candidate** — a second ZSTD candidate at level 19 (the wire mode is the
  frozen ZSTD mode; the level is not part of the frame identity). Offered only by the
  `Planned` write policy, only at or above 1 MiB, and only when its estimated encode time
  fits a 20 s CPU budget. Encode is priced at the measured 28.5 MiB/s (R5 entropy court)
  in `weftfs-write-cost/v1`. `--no-zstd19`, `--zstd19-cpu-budget-ns`.
* **Whole-base ZSTD patch candidate** — `TensorStrategy::ZstdPatch`, persisted as codec
  strategy `zstd-patch`: a mode-4 `DICTIONARY` frame whose raw-content dictionary is the
  whole causal base and whose zstd window is set to cover that base, with long-distance
  matching enabled. This is the `zstd --patch-from` analogue. It carries exactly one
  dependency (the base content root), respects the frozen dependency-depth bound, and is
  gated by the same planned-only / size / CPU-budget rules. `--no-zstd-patch`,
  `--zstd-patch-level` (default 12).
* The decoder raises `ZSTD_d_windowLogMax` to the size of the **already-loaded**
  dependency, never to a frame-declared size, so a patch frame cannot force an allocation
  larger than its base.

Exactness is covered by tests, not by the plan-only court: `tests/planner_write_cost.rs`
writes a patch frame, reads it back byte-exactly, and runs `verify(true)`; the frame also
carries `decoded_len`, `content_hash` and the base dependency, all checked on read.

## Results

### Pythia-70m F32, step0 → step1 (a 19-byte change)

76 tensors. `zstd -19` whole-file = 142,240,232 B; `zstd -12 --patch-from step0` = 30,265 B.

| configuration | bytes | vs current | chosen strategies |
|---|---:|---:|---|
| standalone (no causal base) | 144,212,394 | +144,193,631 | 59 zstd, 17 semantic-delta |
| **current** (causal, pre-Stage-3) | **18,763** | — | 50 zstd, 26 sparse-delta |
| + zstd-19 | 18,763 | **0** | unchanged |
| **+ zstd-19 + patch** | **17,923** | **−840** | 50 zstd, 20 sparse-delta, **6 zstd-patch** |

The family total (17,923 B) is **40.8 % under** the independent whole-file
`--patch-from -12` baseline (30,265 B) and 99.99 % under whole-file `zstd -19`. The new
patch candidate wins on 6 of 76 tensors; the existing sparse delta already handles the
rest.

### Pythia-70m F32, step0 base checkpoint (standalone)

| configuration | bytes | vs current |
|---|---:|---:|
| **current** | **173,194,756** | — |
| + zstd-19 | 163,872,074 | **−9,322,682 (−5.4 %)** |
| independent whole-file `zstd -19` | 142,240,232 | **WeftFS still loses by 21,631,842 B (+15.2 %)** |

This is the review's "F32 base loses to `zstd -19`" case. The level-19 candidate closes
5.4 % of the gap; the planner still loses 15.2 % to a whole-file `zstd -19` on the base
checkpoint. The residual cause is structural: the plan is per tensor (76 independent
frames) while the baseline is one 281 MB stream, and the planner's F32 candidates are
either `zstd` (this candidate) or mode 9, which stores a 24-bit sign+mantissa plane and
loses on F32.

### Qwen2.5-0.5B BF16, base → Instruct (a fine-tune)

290 tensors. Whole-file `zstd -19` = 764,815,908 B; `zstd -12 --patch-from` = 749,534,915 B.

| configuration | bytes | vs current | chosen strategies |
|---|---:|---:|---|
| standalone (no causal base) | 657,845,918 | +116,694,244 | 169 float-tensor, 73 zstd, 48 raw |
| **current** (causal, pre-Stage-3) | **541,151,674** | — | 241 weight-delta, 1 zstd, 48 raw |
| + zstd-19 | 541,151,674 | **0** | unchanged |
| **+ zstd-19 + patch** | **541,151,674** | **0** | unchanged |

WeftFS beats both whole-file baselines on this checkpoint (29.2 % under `zstd -19`,
27.8 % under `--patch-from`), but **neither new candidate changes a single byte**: every
tensor where a delta is possible already prefers mode 11. The patch candidate loses to
the Huffman-plane weight delta on this dense fine-tune. This is a measured loss for the
new candidate and is retained.

### Qwen2.5-0.5B BF16 base checkpoint (standalone)

| configuration | bytes | vs current |
|---|---:|---:|
| **current** | **657,929,667** | — |
| + zstd-19 | 657,929,667 | **0** |
| independent whole-file `zstd -19` | 764,815,908 | WeftFS wins by 106,886,241 (−14.0 %) |

Level 19 never wins over mode 9 / level 3 on BF16 weights, so the candidate is inert
here. It is kept because it is gated and because the F32 case shows a real 5.4 % gain.

## Every remaining loss

1. **F32 base checkpoint**: plan total 163,872,074 B vs whole-file `zstd -19`
   142,240,232 B — **+15.2 %**. Per-tensor framing plus mode 9's F32 layout.
2. **Qwen fine-tune pair**: the patch candidate changes **0 B**; mode 11 already wins on
   every tensor with a base.
3. **Pythia pair**: the patch candidate wins only 6/76 tensors (840 B of 17,923 B);
   the bulk of the delta win is the pre-existing sparse delta, not the new candidate.
4. **Level 19** changes nothing on Qwen (BF16) and nothing on the Pythia pair; its only
   measured gain is the F32 base (−5.4 %).
5. The plan-only totals exclude store manifests, commit records, index segments and
   checkpoint metadata, while the zstd CLI baselines are whole-file. Percentages above
   compare like with the stated caveat, not as a store-footprint claim.
6. Patch encode cost is charged at the measured level-19 rate (conservative); the actual
   level-12 patch encode was not separately timed. No latency claim is made.

## Receipts

* `bench/receipts/compression-candidates-pythia-20261108.json`
* `bench/receipts/compression-candidates-pythia-base-20261108.json`
* `bench/receipts/compression-candidates-qwen-20261108.json`
* `bench/receipts/compression-candidates-qwen-base-20261108.json`
