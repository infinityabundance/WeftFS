# Held-out predictive planner, sampled-vs-exhaustive oracle, comparators, paired statistics

**Status:** executed on `staging`. The contract
([`bench/CONTRACTS-heldout.md`](CONTRACTS-heldout.md)) was committed **before**
any held-out number existed. It fixes the split, the fitted model's functional
form, the SLA threshold grid and its 10 % margin, the eight adversarial strata
and the gates. Nothing in this document widens a gate.

Everything numeric below is emitted from a receipt by
[`scripts/perf_numbers.py`](../scripts/perf_numbers.py); the canonical table is
[`docs/PERF-NUMBERS.md`](../docs/PERF-NUMBERS.md). Prose quotes no live number
outside a generated region.

| Receipt | Court |
|---|---|
| [`heldout-planner-20261114T175023Z.json`](receipts/heldout-planner-20261114T175023Z.json) (+ `.log`) | fit / validate / held-out prediction and SLA admission |
| [`two-stage-oracle-20261114T175829Z.json`](receipts/two-stage-oracle-20261114T175829Z.json) (+ `.log`) | N11 sampled-vs-exhaustive oracle |
| [`comparators-20261114T182322Z.json`](receipts/comparators-20261114T182322Z.json) (+ `.log`) | T041 comparator gap table |
| [`paired-stats-20261114T185845Z.json`](receipts/paired-stats-20261114T185845Z.json) (+ raw, `.log`, `.telemetry.json`) | T083 / N10 paired A–E statistics |
| `heldout-planner-20261114T174546Z.json` | **Retained.** The first pass. Same model and split; the harness omitted the product's mode-9 `1/64` eligibility rule from the predictor and paid for per-object durability barriers on a contended host. Its predictions are byte-identical to the published pass; only its write phase differs. It is kept because deleting an inconvenient run is the failure mode this programme forbids. |
| `heldout-planner-20261114T174422Z.log`, `heldout-fit-20261114T174422Z.json` | **Retained failure.** The first invocation: the fit completed, then the predict phase could not find the model because the harness deleted its own work directory before writing it. Fixed before any prediction existed; no number from it is cited. |
| [`comparator-install-probe-20261114T185845Z.json`](receipts/comparator-install-probe-20261114T185845Z.json) | Installability probes for ZipNN and ByteCheckpoint, with the exact commands and output. |
| `augmentation-local-20261112T230000Z.json` (R22), `sparse-delta-20261106T052333Z.json`, `real-float-tensor-20261105T053958Z.json` | **Quoted**, not re-measured, where the comparator table needs a WeftFS figure. |

## 1. The declared split

The FIT split is two whole families: Qwen2.5-0.5B (base) and pythia-70m step 0.
The VALIDATION split is Qwen2.5-3B (base); the HELD-OUT split is
Qwen2.5-7B-Instruct, measured **once**. The bounded subset rule (192 MiB for
validation, 160 MiB for held-out, smallest stride that fits the declared tensor
and byte caps) was declared before measurement and is applied by rule, not by
inspecting errors.

<!-- BEGIN GENERATED:heldout-fit (scripts/perf_numbers.py) -->
Fit: **366** tensors measured across two whole families. Every coefficient below was fitted on those tensors only, before any validation or held-out measurement existed. The fitted `raw` decode slope is **8,139,406,165** ns/MiB: it is an origin-constrained least squares over sub-kilobyte tensors where the fixed per-read cost dominates, and it is the direct cause of the restore-tail loss.
<!-- END GENERATED:heldout-fit -->

**Gate 1, as frozen, fails.** `CONTRACTS-heldout.md` §8 asked that "no tensor
name ... appears in two splits". Qwen checkpoints of different widths share
tensor names, so the literal gate cannot pass for any legitimate split of this
family. The receipt publishes both readings: the declared name-level gate
(`false`) and the observation-level diagnostic (`containers_disjoint: true`,
with the exact name overlap). The gate was **not** reworded or removed, and
`checks_passed` counts it as a failure.

## 2. Out-of-sample prediction error

The model has one byte ratio per `(strategy, dtype)`, one encode and one decode
rate per strategy, one pooled publish rate and two fitted constants. It is
fitted on the FIT split and applied unchanged. Predictions are formed from the
model and the tensor's dtype/size **before** the exhaustive plan is consulted
and before anything is written, so no measurement can leak into a prediction.

Two byte rows are published for each family: `bytes_conditional` (the model's
size for the strategy that was actually chosen, isolating size error) and
`bytes_end_to_end` (the model's own choice, which also carries strategy error).

**Validation family.**

<!-- BEGIN GENERATED:heldout-validate (scripts/perf_numbers.py) -->
**87** tensors, **1,079,083,008** logical bytes. Chosen-strategy agreement with the exhaustive measurement: **0.598**. Conditional encoded-bytes relative error: median **0.0078**, p90 **0.1982**; end-to-end **0.0110** at the median.
Cold-restore relative error median **0.0770**. Write-wall relative error median **0.9375** (predicted **4,931.4** ms against measured **29,580.9** ms). Family restore predicted **3,236.4** ms against measured **3,149.9** ms. SLAs accepted **3**, missed **0**.
<!-- END GENERATED:heldout-validate -->

**Held-out family.**

<!-- BEGIN GENERATED:heldout-heldout (scripts/perf_numbers.py) -->
**38** tensors, **1,512,118,272** logical bytes, measured once. Chosen-strategy agreement **0.895**. Conditional encoded-bytes relative error: median **0.0095**, p95 **0.2592**, max **0.5912**; end-to-end **0.0095** at the median.
Cold-restore relative error median **0.0478**; write-wall relative error median **0.7718**. Family restore predicted **4,525.7** ms against measured **4,362.1** ms. SLAs accepted **3**, missed **0**.
<!-- END GENERATED:heldout-heldout -->

### 2.1 What the numbers say

- **Bytes generalise.** The conditional relative error is small at the median
  on both families, so the fitted per-strategy ratios transfer across widths.
  The tail is real: the worst tensors are small layernorm/bias tensors whose
  compressibility is a property of their contents, not of their shape, and a
  single global ratio cannot represent that.
- **Strategy choice does not generalise well enough.** Agreement is below the
  declared gate on both families. The cause is measured, not guessed: mode 9
  and level-3 ZSTD cross over as a function of tensor size (mode 9 has a
  descriptor and a canonical table that a few-kilobyte tensor cannot amortise),
  and a size-blind ratio predicts the large-tensor winner for small tensors.
  The product's own `1/64` eligibility rule removes some of these mistakes but
  not all of them, because eligibility compares *predicted* sizes.
- **Restore wall predicts well at the median** and badly in the tail. The tail
  has a specific, named cause: the contract fixes each decode rate as a slope
  **through the origin**, and the `raw` slope is fitted on sub-kilobyte tensors
  where a fixed per-read cost dominates. The fitted slope is therefore far above
  the other strategies and over-predicts small `raw` reads on the validation
  family. A per-strategy intercept is what the data wants; the declared form
  does not have one, so this is published as a modelling loss rather than fixed
  after the fact.
- **Repetition.** The per-tensor errors are one measurement per tensor, not
  repeated measurements of one cell: the contract allows exactly one held-out
  pass. The only cells in this subphase with N >= 7 paired repetitions are the
  A/N/B/C/D/E statistics in §6. Every single-pass table here is labelled as
  such and none is promoted to a headline claim about run-to-run stability.
- **Write wall is the weak target, and the first pass shows why.** In the
  retained pass the same code and the same strategies wrote the same bytes, and
  the family write wall moved by an order of magnitude between two consecutive
  runs (`publish` 0.93 s against 17.6 s; host load average 7.4 with an
  eighteen-hour-old container resident). The harness writes one object per
  durable commit, so the write target measures the host's `fsync` latency as
  much as the model. The prediction error table publishes that as a loss; it
  does not hide it behind a quiet re-run.

## 3. SLA admission

Thresholds are the model's predicted family restore wall multiplied by
{0.75, 1.00, 1.25, 1.50, 2.00}, fixed before measurement; an SLA is accepted iff
the prediction is at most 0.90 × the threshold. A miss is an accepted SLA whose
measured wall exceeds the threshold.

<!-- BEGIN GENERATED:heldout-sla (scripts/perf_numbers.py) -->
Validation: accepted **3** of five thresholds, missed **0**. Held-out: accepted **3**, missed **0**.
<!-- END GENERATED:heldout-sla -->

No accepted SLA missed on either family. That is **not** a strong claim: the
10 % margin is large relative to the median error and only a few thresholds are
accepted. The grid and the rule are frozen, so a future run cannot tighten the
margin to look better.

## 4. N11 — sampled planner against the exhaustive oracle

Both planners run on the same bytes in the same process: the shipped
`TwoStage` planner through `encode_tensor_object` (the only entry point that
records the decision stream), and a forced `Exhaustive` plan. The strata are
the eight declared before measurement, each with eight deterministic seeds.

<!-- BEGIN GENERATED:oracle-summary (scripts/perf_numbers.py) -->
Synthetic: **19** of **64** tensors had an actual size outside a priced interval (rate **0.2969**). **8** tensors record skip-regret with **8,371,341** bytes of regret, and the mode-9 exact prediction was never violated (**0**).
`sampled-unlucky`: **8** of **8** undetected, **8,371,341** bytes, **0** violations. `rare-heavy`: **8** of **8** detected, **8** fallbacks, no regret. Real tensors: **0** of **87** (validation) and **0** of **38** (held-out) disagreed with the oracle.
<!-- END GENERATED:oracle-summary -->

A first version of this court compared `plan_tensor(TwoStage)` with
`plan_tensor(Exhaustive)`. That entry point returns no decision stream, so the
"sampled" arm silently ran the exhaustive planner and every disagreement was
zero. The version was discarded before it was committed; the published receipt
drives the sampled planner through `encode_tensor_object`, the entry point that
records the stream, and its `mode9_exact_checked` and
`zstd3_interval_checked` counters prove the analysis actually ran.

Two failure modes are separated, and they are not the same finding:

1. **Detected.** On `rare-heavy` and `sampled-lucky` the four sampled chunks are
   unrepresentative in the dangerous direction: they under-estimate the real
   frame, the actual encode falls outside the trusted interval, the planner
   records the contradiction and falls back to the exhaustive set. It then
   picks the oracle's winner, so the *regret is zero* even though the estimate
   was wrong. A safe retry exists and works.
2. **Undetected.** On `sampled-unlucky` the sampled chunks are the *incompressible*
   ones and every other chunk is zero. The level-3 candidate is then skipped
   because the sampled estimate says it cannot win, and a skipped candidate is
   by construction never measured — so no contradiction can ever be recorded.
   The planner publishes mode 9 at roughly five times the size the exhaustive
   planner's level-3 frame would have been, in every one of the eight seeds,
   with no violation and no fallback.

That second row is the answer to "does a safe fallback exist": it exists for
the detected class and **does not** exist for the undetected class. Across the
real validation and held-out tensors the oracle disagreement count is zero, so
this is a bounded adversarial finding, not an observed production failure.

## 5. T041 — comparators

Complete stored bytes = the base container + the stored form of the newer
container + every index byte the arm needs. Both arms restore byte-exactly or
they are not in the table. Sparse = pythia-70m step 0 → step 1 (19 changed
16-bit words). Dense = Qwen2.5-0.5B base → Instruct (a fine-tune).

<!-- BEGIN GENERATED:comparators (scripts/perf_numbers.py) -->
Sparse pair, newer-container marginal bytes: XNOR **23,406**, patch-from -12 **30,265**, patch-from -19 **275,660**, independent zstd -19 **142,240,032**. Native both containers **563,430,352**; XNOR's complete bytes are **0.5000** of native.
Dense pair: XNOR **616,702,657**, independent zstd -19 **764,093,099**, native **1,976,195,648**; XNOR's complete bytes are **0.8121** of native. Both `--patch-from` levels were OOM-killed at the court limit and have no number.
<!-- END GENERATED:comparators -->

Losses and blockers, stated plainly:

- **WeftFS loses to a BitX-style XOR delta on the sparse pair.** Our
  implementation of the idea stores the newer checkpoint in fewer complete
  bytes than WeftFS's mode-16 sparse delta quoted from
  `sparse-delta-20261106T052333Z.json`. A whole-file, per-tensor XOR is a
  strong baseline for a 19-word change and the table says so.
- **Level 19 is not always better than level 12 for `--patch-from`.** On the
  sparse pair PF19's delta is about nine times PF12's. Patching a nearly
  identical file rewards long-search, not a deeper parse.
- **`--patch-from` on the dense pair was OOM-killed** (`exit -9`) at the 3 GiB
  court limit for both levels. The blocker is recorded with its return code;
  the arm has no number.
- **ZipNN did not install.** `zipnn==0.5.4` publishes no binary wheel for the
  court's Python 3.12, and the court that has `pip` has no compiler; the court
  that has `gcc` (the bench court) has no `pip`, and a `venv` could not be
  created. Only 0.1.0/0.1.1 publish binary wheels. No ZipNN number is claimed.
- **ByteCheckpoint did not run.** `bytecheckpoint==0.0.2` installed with
  dependencies was killed by the court's memory limit during `pip install`; with
  `--no-deps` it imports `grpc` at module load and fails. No ByteCheckpoint
  number is claimed.
- The WeftFS column in the comparator receipt is **quoted** from other courts,
  with their stamps and revisions, and its accounting is not identical to the
  external arms (no manifests/index/commits in the quoted marginal figure).
  It is therefore not used to declare a win.

## 6. T083 / N10 — paired statistics and the host

Ten paired repetitions, every arm once per repetition, order **shuffled** by a
recorded seed per repetition (not a fixed rotation), `sync` between cells, and
the raw per-repetition values retained in the receipt. Paired percentile
bootstrap, 10 000 resamples, seeded and recorded.

<!-- BEGIN GENERATED:paired-stats (scripts/perf_numbers.py) -->
**10** paired repetitions per arm. Native write wall min **325.5** ms, p50 **339.4** ms, p95 **7,354.3** ms; planned write (C) p50 **4,790.2** ms. Cold restore p50: A **360.1** ms, C **1,504.1** ms, E **1,518.1** ms.
Of the nine claimed orderings, **4** survive the paired bootstrap 95 % confidence interval and **4** paired differences are distinguishable from zero in either direction.
<!-- END GENERATED:paired-stats -->

### 6.1 Did the earlier ordering survive?

Mostly not. Write-wall orderings are dominated by a long right tail: the native
control's least-contended repetition is a fraction of its p95, which is the same
signature as the 11.48 s native control that motivated this court. Under a
shuffled interleaving and a paired bootstrap, only the A < B write ordering
survives; B < D, D < C and C < E do not (their paired differences have
confidence intervals containing zero or pointing the wrong way). On cold
restore, N < B, D < C and C < E survive; A < N and B < D do not.

So the honest statement is: **the codec and protocol restore costs reproduce in
their old order, and the write-wall ladder does not reproduce on this host at
this sample size.** A/N write wall is not distinguishable from zero difference,
and neither is N < B's write direction beyond its interval.

Telemetry: load average, `/proc/meminfo`, `vmstat`, `iostat`, `df`, cgroup
memory/CPU and thermals were captured before and after. Thermals read
`unavailable` on this host (no readable `/sys/class/thermal/*/temp` inside the
container); that is reported, not zeroed.

**Cross-host repeat: not attempted.** `~/.ssh/known_hosts` lists only
`github.com` and there is no SSH configuration, so no second host is reachable
from this workspace. No cross-host claim is made.

## 7. Every loss in this subphase

1. Gate 4 (`strategy_agreement_ge_0.90`) **fails on both families**. The model's
   size-blind ratios cannot place the mode-9/ZSTD crossover.
2. The write-wall target is **not validated**. One pass measured 2.6 s and an
   identical pass 29.6 s for the same bytes and strategies.
3. Gate 1, as frozen, fails for a benign reason that the contract's wording did
   not anticipate (tensor names repeat across model widths). Both readings are
   published.
4. The sampled planner has a **real undetected failure mode** on adversarial
   chunk distributions: skip-regret with no violation and no fallback.
5. WeftFS loses to an XOR-delta baseline on the sparse pair.
6. Two comparators (`ZIPNN`, `BCP`) could not run at all, and dense
   `--patch-from` was OOM-killed.
7. The 7B held-out split excludes the two ~1 GiB tensors by the declared court
   bound, so the held-out result does not cover the largest tensors.
8. The paired write-wall ladder does not survive at N=10 on this host.
9. The model predicts decode **wall** time; its reuse for decode CPU is a proxy
   and over-predicts, because a read's CPU time is a fraction of its wall time.
10. The origin-constrained `raw` decode slope is fitted on sub-kilobyte tensors
    and over-predicts small raw reads by more than an order of magnitude on the
    validation family. The declared functional form has no intercept, and it was
    not changed after the held-out pass.

## 8. N03 — the per-family Pareto, wins and losses in one table

Durable write wall against physical footprint against cold restore, p50 over
the interleaved repetitions, with the least-contended write repetition shown
because p50 and minimum diverge on a shared host. The pythia row is the worst
write cell in the programme and sits in the same table as the Qwen row.

<!-- BEGIN GENERATED:heldout-pareto (scripts/perf_numbers.py) -->
Qwen2.5-0.5B-Instruct (**10** repetitions): write **24.65x** native at p50 and **3.07x** least-contended; footprint **0.6647** of native (**33.53 %** smaller); cold restore **4.17x**.
pythia-70m step 0 (**7** repetitions): write **50.01x** native at p50 and **50.92x** least-contended; footprint **0.5820** of native (**41.80 %** smaller); cold restore **2.42x**.
<!-- END GENERATED:heldout-pareto -->

What the table says:

- **Every family wins on footprint and loses on write and restore.** There is
  no family in which the augmentation is free.
- **The F32 family is the write-cost catastrophe.** pythia's planned write is
  about fifty times its native write at both statistics, so this is not host
  noise: it is the mode-9/ZSTD candidate set on an F32 container whose values are
  fp16-precision, where mode 9 spends a 32-bit layout to represent 16-bit
  information.
- **The Qwen family's p50 is host noise; its minimum is not.** The p50 and
  least-contended write ratios differ by a factor of eight on the same arm,
  while the restore and footprint ratios are stable. That is the same signature
  as §6: durable per-object publication on a contended host.
- The Pareto is descriptive: it was added to the contract after these two
  families were measured, and the contract says so.

## 9. What is not claimed

One host, one container shape (`3g / 4 cpus / 1024 pids`), warm host page cache
except where `fadvise` is used and labelled. No fleet, cloud, GPU, media or
power-loss claim. Bounded per-tensor subsets are not whole-checkpoint numbers.
The synthetic strata bound a sampler failure mode; they do not estimate its
frequency in real training. The quoted WeftFS comparator figures come from
other courts and are labelled as such.

## 10. Reproduce

```
bash scripts/heldout_planner_court.sh
bash scripts/two_stage_oracle_court.sh
bash scripts/comparator_court.sh
bash scripts/paired_stats_court.sh
python3 scripts/perf_numbers.py --check
bash scripts/check-claims.sh
```

## Claim register

Each line is the machine-checkable anchor for one entry in `docs/claims.json`.

- `claim:t042_contract_frozen_before_measurement` — The held-out split, the model's functional form, the SLA grid and the gates were committed before any held-out number existed.
- `claim:t042_model_not_refit` — The model digest on the validation receipt equals the one on the held-out receipt; no refit happened after validation.
- `claim:t042_strategy_agreement_gate_failed` — Chosen-strategy agreement is below the 0.90 gate on both families and the gate is published as false.
- `claim:t042_bytes_gate_passed` — Conditional encoded-bytes relative error is at or below 10 % at the median on both families.
- `claim:t042_write_wall_not_validated` — The write-wall target is not validated: two identical passes differed by an order of magnitude on this host.
- `claim:t042_first_pass_retained` — The superseded first held-out pass is retained and named rather than deleted.
- `claim:t042_sla_miss_rate_reported` — The SLA receipt publishes accepted count, missed count and miss rate for both families.
- `claim:n11_undetected_regret_exists` — On the declared adversarial stratum, every seed shows undetected skip-regret with no violation and no fallback.
- `claim:n11_detected_violations_fall_back` — Detected interval violations fall back to the exhaustive set and end with zero skip-regret.
- `claim:n11_real_tensors_no_disagreement` — Across the real validation and held-out tensors the sampled planner and the oracle chose the same representation.
- `claim:t041_complete_stored_bytes` — Comparator arms are compared on complete stored bytes, base plus newer stored form plus every index byte.
- `claim:t041_weftfs_loses_sparse_xor` — A BitX-style XOR delta stores the sparse pair in fewer complete bytes than the quoted WeftFS mode-16 figure.
- `claim:t041_patch_from_oom` — Both patch-from levels were killed at the court memory limit on the dense pair and have no number.
- `claim:t041_zipnn_blocked` — ZipNN 0.5.4 could not be installed in either court and no ZipNN number is claimed.
- `claim:t041_bytecheckpoint_blocked` — ByteCheckpoint 0.0.2 could not be run in the court and no ByteCheckpoint number is claimed.
- `claim:t083_n10_randomised_interleaving` — Every local arm ran ten paired repetitions in a seeded shuffled order with raw values retained.
- `claim:t083_bootstrap_ci` — Paired differences carry a seeded percentile bootstrap 95 % confidence interval.
- `claim:t083_write_ordering_not_survived` — Most claimed write-wall orderings do not survive the paired bootstrap on this host.
- `claim:n03_per_family_pareto` — every family in the Pareto wins on footprint and loses on write and restore; the worst cell is not separated.
- `claim:n03_pythia_write_loss` — the pythia F32 write ratio is published in the same table as the Qwen row.
