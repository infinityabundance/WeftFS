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
<!-- END GENERATED:heldout-validate -->

**Held-out family.**

<!-- BEGIN GENERATED:heldout-heldout (scripts/perf_numbers.py) -->
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
- **Restore wall predicts well at the median** and badly in the tail: a few
  tensors' per-read cost is dominated by fixed manifest/index work that the
  pooled intercept cannot track.
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

**Cross-host repeat: not attempted.** No second machine is reachable from this
workspace, so no cross-host claim is made.

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

## 8. What is not claimed

One host, one container shape (`3g / 4 cpus / 1024 pids`), warm host page cache
except where `fadvise` is used and labelled. No fleet, cloud, GPU, media or
power-loss claim. Bounded per-tensor subsets are not whole-checkpoint numbers.
The synthetic strata bound a sampler failure mode; they do not estimate its
frequency in real training. The quoted WeftFS comparator figures come from
other courts and are labelled as such.

## 9. Reproduce

```
bash scripts/heldout_planner_court.sh
bash scripts/two_stage_oracle_court.sh
bash scripts/comparator_court.sh
bash scripts/paired_stats_court.sh
python3 scripts/perf_numbers.py --check
bash scripts/check-claims.sh
```
