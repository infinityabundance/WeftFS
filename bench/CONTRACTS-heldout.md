# Held-out predictive-planner contract (T042 / N03 / N11 / T041 / T083 / N10)

**Status: frozen before measurement.** This file is committed before any
held-out number exists. Every rule below is fixed in advance. Nothing in it may
be widened, and no model coefficient may be re-fitted after a validation or
held-out result is seen.

Receipts this contract authorises (all under `bench/receipts/`):

| Receipt | Court |
|---|---|
| `heldout-planner-<stamp>.json` (+ `.log`) | fit / validate / held-out prediction, SLA admission |
| `two-stage-oracle-<stamp>.json` (+ `.log`) | N11 sampled-vs-exhaustive oracle |
| `comparators-<stamp>.json` (+ `.log`) | T041 comparator gap table |
| `paired-stats-<stamp>.json` (+ `.log`) | T083 / N10 paired A–E statistics |

A retained failure keeps its stamp and is never deleted; a re-run gets a new
stamp. Numbers in tracked documents are generated from these receipts by
`scripts/perf_numbers.py` (directive 12).

---

## 1. The split (declared now, not chosen later)

Families are whole checkpoint containers. **A tensor is never moved between
splits**, and no family appears in two splits.

| Role | Family | Container | Why |
|---|---|---|---|
| **FIT** | Qwen2.5-0.5B (base) | `Qwen2.5-0.5B.safetensors` | small, dense BF16, 290 tensors |
| **FIT** | pythia-70m step 0 | `pythia-70m-step0.safetensors` | different dtype regime (F32 stored), 76 tensors |
| **VALIDATE** | Qwen2.5-3B (base) | `Qwen2.5-3B.safetensors` | larger, same family, different width/depth |
| **HELD OUT** | Qwen2.5-7B-Instruct | `Qwen2.5-7B-Instruct.safetensors` | different family scale and a fine-tuned checkpoint; measured **once** |

The 3B validation split is a **bounded deterministic subset** because the court
is 3 GiB / 4 CPUs and per-tensor exhaustive planning holds every candidate
artifact of one tensor at once (raw + level-3 + level-19 + mode-9). The bound is
**every tensor with `byte_len <= 192 MiB`**, then every `k`-th remaining tensor
in ascending name order, `k` chosen as the smallest integer that keeps the
subset at or below 96 tensors and 1.5 GiB. The exact tensor list is written into
the receipt. The 192 MiB bound is a declared court-memory bound, not a
result-dependent filter: at 192 MiB the candidate set is at most ~0.8 GiB and
the court has never OOM-killed that shape.

The 7B held-out split is also bounded, and the bound is **declared now**: every
tensor with `byte_len <= 160 MiB`, in ascending name order, at most 40 tensors
(the same 192 MiB court-memory rule, applied at 160 MiB to leave headroom for a
container-level peak already measured at ~2.9 GiB on 7B).
The two 1 GiB tensors (`model.embed_tokens.weight`, `lm_head.weight`) are
**excluded before measurement** because the measured per-tensor planner peak for
a 7B tensor was an OOM kill at the 3 GiB court limit (`docs/STATUS.md`, R12).
This is a limitation of the court, stated as such; it is not a selection made
after seeing errors.

Both bounded subsets are chosen by a rule, not by inspecting results.

## 2. The fitted model (fixed functional form, fixed fitting procedure)

The model is fitted **only** on the FIT families and then applied unchanged.

For each `(strategy, dtype)` pair the fit produces:

1. **Byte ratio** `r`: the median of `actual_frame_bytes / logical_bytes` over
   every tensor in the FIT split for which that strategy was actual, together
   with the 5th and 95th percentiles of the same ratio (published, not used to
   change the choice).
2. **Encode rate** `e` (ns/MiB): least-squares slope through the origin of
   measured whole-object write wall against logical MiB, using only tensors
   whose **chosen** strategy is that strategy.
3. **Decode rate** `d` (ns/MiB): least-squares slope through the origin of
   measured cold read wall against logical MiB, same population.
4. **Publication rate** `p` (ns/MiB of stored frame): least-squares slope of
   measured publish wall against stored bytes, pooled across strategies (it is
   a backend cost, not a codec cost).
5. **Restore overhead** `o` (ns/tensor): least-squares intercept of per-tensor
   read wall against logical bytes, pooled.
6. **Write overhead** `w` (ns/object): the median residual of measured
   per-object write wall minus `(encode + publish)` for that object. It covers
   the metadata commit, superblock publication and object bookkeeping that one
   object-at-a-time publication pays and that no per-byte rate predicts. It is
   a fitted constant, not a per-family fudge: one value, from the FIT split.

A strategy with no FIT observation is **not predicted**. It is reported as
`unpredicted` and excluded from that family's choice comparison; it is never
given a guessed coefficient.

The decision objective is the one the shipped planner documents:
`price = bytes + lambda_decode * decode_ns + lambda_write * encode_ns`, with
`lambda_decode = 1 ns/byte` and `lambda_write = lambda_decode`. Two λ settings
are reported: `bytes-only` (`lambda_decode = 0`, the shipped default) and
`latency` (`lambda_decode = 1`). The **primary** primary is `bytes-only`,
because that is what the shipped store chooses by default; `latency` is a
secondary row.

## 3. Prediction targets and error definition

Per held-out tensor and per family:

| Target | Predicted | Measured |
|---|---|---|
| chosen strategy | argmin predicted price over predicted strategies | argmin measured price over **exhaustively encoded** candidates |
| encoded bytes | `r * logical_bytes` for the predicted strategy | actual frame bytes of the measured winner |
| write wall | `e * MiB + p * predicted_stored_MiB + o_w` | actual per-object write wall |
| cold restore wall | `d * MiB + o` | actual per-object cold read wall |
| decode CPU | same model as cold restore wall (CPU-time analogue) | actual process CPU delta of the read |

Family-level predictions are the sums of the per-tensor predictions plus the
fitted constant, compared against the whole measured family run.

Error definitions (both published for every target):

```
abs_error  = predicted - measured
rel_error  = (predicted - measured) / measured        (measured != 0)
```

Distributions are reported as **median, p90, p95, max** of `abs_error` and
`|rel_error|`, plus the signed median relative error so systematic bias is
visible. `p90/p95` use linear interpolation on the sorted sample. A cell with
fewer than 7 samples is labelled `small_n` and is never a headline.

Exactly one **held-out pass** on 7B is allowed. If that pass is corrupted by an
environmental failure, the failed run is retained, its cause is recorded, and a
single labelled re-run is permitted; the re-run does not change the model or
the contract.

## 4. SLA admission (declared thresholds)

SLAs are declared as a **grid of absolute restore-wall thresholds for each
validation family**, computed from the FIT model **before** the validation run:

```
predicted_family_restore = sum over tensors of (d * MiB + o)
thresholds = {0.75, 1.00, 1.25, 1.50, 2.00} * predicted_family_restore
```

The admission rule is fixed: **accept** an SLA iff
`predicted_family_restore <= 0.90 * threshold` (a 10 % safety margin), otherwise
**reject**. The same rule is applied unchanged to the held-out family using the
same model and the same multiplier grid.

A **miss** is an accepted SLA whose measured family restore wall exceeds the
threshold. The miss rate is `missed_accepted / accepted`, reported with the
exact accepted/rejected counts. A miss is published in the headline table; it is
never re-framed as "close", never excluded as an outlier, and the threshold is
never moved.

Write-wall and stored-bytes SLA rows use the same 10 % margin and the same
multiplier grid, and their misses are reported separately.

## 5. N11 — sampled-vs-exhaustive oracle

Two planners are run **on the same bytes, in the same process**:

- **S** = `PlannerMode::TwoStage` (the shipped sampled planner);
- **X** = `PlannerMode::Exhaustive` (the forced oracle: every candidate is
  encoded and priced).

Catalogues:

**Real tensors.** Every tensor in the 3B validation subset and the 7B held-out
subset, plus the FIT families. Each tensor contributes one paired observation.

**Stratified synthetic adversarial tensors.** Eight strata, declared now, each
`2 MiB` of logical bytes at a `64 KiB` codec chunk (32 chunks), generated by a
deterministic xorshift64\* stream from the stratum seed:

| # | Stratum | Construction |
|---|---|---|
| 1 | `uniform-random` | every byte independent |
| 2 | `all-zero` | every byte zero |
| 3 | `alternating` | even chunks random, odd chunks zero |
| 4 | `rare-heavy` | chunks 0..3 compressible, one random chunk in 8..31 is incompressible |
| 5 | `sampled-lucky` | the chunks a 4-chunk sample would choose are compressible; every other chunk is random |
| 6 | `sampled-unlucky` | the sampled chunks are random; every other chunk is compressible |
| 7 | `two-level` | per-chunk regime alternates between two compressibility levels |
| 8 | `per-chunk-random` | each chunk is independently all-zero or random |

Stratum 5 and 6 are the adversarial pair the sampler's safety argument depends
on: a 4-chunk sample must not be treated as proof about 32 chunks. The sample
indices are read from the shipped `two_stage::sample_indices`, not assumed.

Metrics per stratum and overall:

- **estimate-violation rate**: fraction of tensors where an actual encoded size
  falls outside the predicted interval of a candidate the planner priced
  (`analysis.zstd3` / `analysis.zstd19`, and the mode-9 exact prediction vs the
  actual mode-9 frame);
- **skip-regret**: for every tensor where X's winner differs from S's winner,
  the extra published bytes and the extra measured encode nanoseconds S would
  pay relative to X (`regret = cost_S - cost_X` under the same bytes-only
  price). Reported as median/p90/max over the affected tensors, plus the total;
- **fallback behaviour**: whether S recorded `fallback_exhaustive` and whether
  that fallback recovered X's winner (the "safe retry" question). If the
  fallback fires only after a violation, the *undetected* case is the one that
  matters, and the skip-regret count of tensors **without** a recorded violation
  is published separately.

A non-zero violation rate is the expected honest outcome on strata 5–6; it is
published as a finding and no default is changed as part of this court.

## 6. T041 — comparators

One contract, one fixture pair per density class, complete stored bytes
including every manifest, index, commit and retained base:

- **Sparse pair**: pythia-70m step 0 → step 1 (19 changed 16-bit words).
- **Dense pair**: Qwen2.5-0.5B base → Qwen2.5-0.5B-Instruct (a fine-tune).

Arms:

| Arm | What |
|---|---|
| **N** | native files: both checkpoints stored separately, `du` bytes |
| **PF12** | `zstd --patch-from <base> -12` on the newer checkpoint |
| **PF19** | `zstd --patch-from <base> -19` (only if it fits the 3 GiB court; an OOM is recorded, not hidden) |
| **Z19** | independent `zstd -19` of the newer checkpoint |
| **XNOR** | **our** implementation of a ZipLLM/BitX-style XOR-then-compress delta: byte-wise XOR of the two tensors, per-tensor `zstd -19`, complete stored bytes. Labelled "our implementation of the idea", never as the authors' code |
| **ZIPNN** | pinned ZipNN, if it installs in a court; whole-file arm |
| **BCP** | ByteCheckpoint, if obtainable and runnable |

The comparators are compared on **complete stored bytes** and on
decode/restore/write wall. A comparator that cannot be obtained or run is
listed with its exact blocker string and no number. A comparator that runs is
never silently dropped from the table even when WeftFS loses.

Every arm restores the same logical tensors and is checked by SHA-256. No arm
gets a cache, a warm-up, or a concurrency advantage another lacks.

## 7. T083 / N10 — statistics and host control

- **N = 10 paired repetitions** for every local A/N/B/C/D/E cell, run in
  **randomised order within each repetition** (seed recorded in the receipt),
  not a fixed rotation. Every rep runs every arm exactly once.
- Raw per-rep samples are retained in the receipt. The report publishes the
  full distribution: min, p50, p95, max, and every raw sample.
- **Paired bootstrap** confidence intervals: 10,000 resamples of the per-rep
  paired difference `(arm - A)`, percentile 95 % CI. The resampling seed is
  recorded. A difference whose CI contains zero is reported as
  **not distinguishable from zero**, even if the point estimate is favourable.
- The earlier host-noise window (native control 11,482.6 ms against a clean
  332 ms, p95 13.6 s) is the reason this court reruns. The report states
  whether the earlier ordering survived under the quietest conditions
  available: the ordering is `A < B < D < C < E` on write wall and
  `A < N < B < D < C < E` on cold restore from the R3 publication. Survival
  means every pairwise ordering in that list has a CI that excludes zero in the
  same direction.
- Device/telemetry captured per rep where readable: load average, `vmstat 1 5`,
  `/proc/meminfo` `Cached`, `df` on the work filesystem, container cgroup
  memory/CPU counters, and temperature from `/sys/class/thermal/*/temp` if
  present. A missing telemetry source is reported as `unavailable`, never as
  zero.
- **Cross-host repeat**: only if a second host is reachable. If not, the report
  says `cross_host: not_attempted <reason>`; no cross-host claim is made.

## 8. Gates (declared before measurement)

The court passes only if all of the following hold; each check is a boolean in
the receipt:

1. `fit_and_eval_families_disjoint` — no tensor name, and no container, appears
   in two splits.
2. `model_not_refit_after_validation` — the model file hash used on validation
   equals the one used on held-out.
3. `bytes_prediction_median_abs_rel_le_0.10` — the shipped bytes-only planner
   predicting held-out encoded bytes within 10 % at the median. A failure is
   published as a failure.
4. `strategy_agreement_ge_0.90` — held-out chosen-strategy agreement with the
   exhaustive measurement at or above 90 %.
5. `restore_prediction_median_abs_rel_le_0.20` — family cold-restore wall.
6. `sla_misses_reported` — the miss count is present, even when zero, and every
   miss is listed.
7. `all_arms_exact` — every restored tensor matches its source SHA-256 in every
   configuration.
8. `oracle_violation_rate_reported` — the N11 violation rate is present for
   every stratum, including non-zero values.

A gate that fails is reported as `false` in the receipt and in the results
document. **The gate is never widened after seeing the number.** A failed
physically meaningful gate is the honest outcome and the paper says so.

## 9. What this contract does not establish

- One host, one container shape (`3g / 4 cpus / 1024 pids`), warm host page
  cache except where `fadvise` is used and labelled. No fleet, cloud, GPU, or
  media claim.
- No physical power-loss claim: durability is the ordinary barrier.
- The 7B held-out split excludes the two ~1 GiB tensors by a rule declared
  above; the held-out result therefore does **not** cover the largest LMs'
  tensors.
- The synthetic strata are adversarial constructions, not a model family. They
  bound a failure mode; they do not estimate its frequency in real training.
- A comparator that could not be installed is `NOT MEASURED`; its absence is
  never evidence for WeftFS.
