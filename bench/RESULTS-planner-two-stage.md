# R18 — two-stage planned write: analyse, select, materialise

**Status: implemented and measured.** The planned write no longer encodes every
candidate and discards the losers. On the frozen fixture it is **24,175.8 →
4,117.8 ms** (least-contended of 7 interleaved repetitions) with the footprint
**unchanged at 282,283,011 B** and **zero per-tensor decision changes**. It is
**182 ms slower than the R17 `C-noz19` ablation** (3,936.0 ms), because the
analysis that makes level 19 *conditional* rather than *deleted* costs 348.8 ms
and the mode-9 encodes it avoids were almost free. That miss is reported, not
rounded away (§7).

Receipts (all in [`receipts/`](receipts/)). One provenance detail: the assembled
receipt was produced from the cited matrix and family receipts by
`bench/scripts/assemble_planner_two_stage.py`; the court script performs that
same sequence, and its first assembly attempt failed on the native `A`/`N` rows
(no phase ledger), so the assembled file was **regenerated from the same inputs**
rather than re-measured. The matrix and family receipts are single court
invocations.

| Receipt | What it is |
|---|---|
| [`planner-two-stage-20261112T081151Z.json`](receipts/planner-two-stage-20261112T081151Z.json) | **cited.** Assembled R18 receipt: matrix, decision diff, cross-family, every check. |
| [`planner-two-stage-matrix-20261112T081151Z.json`](receipts/planner-two-stage-matrix-20261112T081151Z.json) | **cited.** 7 interleaved repetitions × 11 configurations, per-tensor decision stream on repetition 1, crash proxy per Weft configuration. |
| [`planner-two-stage-qwen0.5b-base-20261112T081151Z.json`](receipts/planner-two-stage-qwen0.5b-base-20261112T081151Z.json), [`…-pythia70m-step0-…`](receipts/planner-two-stage-pythia70m-step0-20261112T081151Z.json), [`…-pythia70m-step8-…`](receipts/planner-two-stage-pythia70m-step8-20261112T081151Z.json) | **cited.** Cross-family decision diffs (one repetition each). |
| [`augmentation-local-20261112T070000Z.json`](receipts/augmentation-local-20261112T070000Z.json) | **cited.** Unmodified contract court at this revision; its C row is the same-revision identity proof. |
| [`bf16-plane-profile-20261112T054210Z.json`](receipts/bf16-plane-profile-20261112T054210Z.json) | Cited by R17 for the mode-9 stage split; unchanged. |
| `planner-two-stage-matrix-20261112T071300Z.json.zst`, `…T073226Z.json.zst`, `…T075100Z.json.zst`, `…T063700Z.json.zst` | Retained **aborted** court invocations (receipts compressed; their logs are beside them). The runner rejected its own family fixtures: `pythia` has no `model.embed_tokens.weight`, and a cross-family row asserted the Qwen-only R3 strategy counts. Each matrix is complete and its own checks pass; they are kept because the failures are real harness defects that the final run fixed. The partially written family receipts from those runs are not kept: the retained final run regenerates them identically, so they held no unique evidence. |

## 1. What changed

### 1.1 The three stages

**Analyse** ([`src/two_stage.rs`](../src/two_stage.rs)) is bounded, deterministic
and runs no full-frame encoder:

* a whole-codec-chunk sample (4 chunks at level 3, the first 2 of those at
  level 19) is compressed and extrapolated to a frame size with an explicit
  interval;
* the mode-9 frame length is predicted **exactly** from a full exponent
  histogram by the codec itself
  (`weftcodec::modes::float_tensor_encoded_len_from_histogram`, WeftCodec
  `0aa3881`).

`Phase::Analyze` (nested in `Encode`) and `Phase::AnalyzeMode9` (nested in
`Analyze`) make the cost measurable: **348.8 ms** total on the fixture, of which
**224.0 ms** is the exponent histogram and prediction and 124.8 ms is the ZSTD
sampling.

**Select** decides, per tensor, which encoders may run (§2).

**Materialise** encodes only the selected candidates, checks each measured size
against its planned interval, and — on a violation — re-plans that tensor with
the exhaustive planner (`PlannerMode::Exhaustive`, recorded as
`fallback_exhaustive`) before anything is published. **No violation occurred** in
any run here (`interval_violation_tensors: 0`, `prediction_violation_tensors: 0`,
`exhaustive_fallback_tensors: 0`).

### 1.2 Options and scope

`PlannerMode::{TwoStage (default), Exhaustive}` and `--planner two-stage|exhaustive`
on `write` and `plan`. The two-stage rule applies to the **write** path. The
pricing-only entry points (`plan_tensor`, the rebase planner) keep the exhaustive
candidate set so a caller can still see every candidate's measured size, and a
forced strategy hint keeps its historical path. Nothing persisted changes: no
frame, manifest, commit record, index byte or logical identity.

Two config keys were added to the ablation runner: `C-legacy` (exhaustive
planner on the unmodified candidate set) and, orthogonally, `--diff-only`. The
R17 ablations (`-noz19`, `-nom9`, `-both`) pin the exhaustive planner, because
"encode this candidate and throw it away" only has a meaning there.

## 2. The selection rule

**Level 3** is encoded when its trusted lower bound could still beat the best
candidate whose size is already exact (raw, and mode 9 when its gate passes), or
when it is needed to resolve the mode-9 gate. **Mode 9** is decided from its
**exact** prediction against the level-3 interval: pass if
`mode9 * 64 <= lower * 63`, fail if `mode9 * 64 > upper * 63`, otherwise
unresolved (which forces the level-3 encode so the shipped `1/64` rule can be
evaluated on measured bytes). **Level 19** is decided *after* level 3 is
measured, against an incumbent built only from measured or exact sizes (raw,
the measured level-3 frame, and the exact mode-9 size), using its own estimate
recomputed as `sampled 19/3 ratio × measured level-3 frame`; it is skipped iff
`lower_19 > incumbent`.

**A skip is only allowed with its bound.** Every skipped candidate carries text
naming the interval, the sample, the incumbent and the gap, e.g. on the
embedding:

> level-19 not encoded: lower bound 200,599,584 B (200,599,584..221,715,330 B
> interval, 2 of 4155 sampled chunks) cannot beat the measured mode-9
> float-tensor (exact) at 180,547,519 B; the skip is safe by 20,052,065 B

**Ambiguity fallback:** when a candidate's interval could change the winner it is
encoded and the choice is made on measured bytes (level 19 on pythia, §6); when
an *encoded* candidate's actual size lands outside its interval the tensor is
re-planned exhaustively, deterministically, before publication.

The rule is sound **relative to the intervals**: if the actual sizes lie inside
them, `lower_19 > incumbent` proves level 19 cannot be cheaper. That is why
every interval is checked wherever an actual exists, and why the decision diff —
not this document — is the evidence that no skip changed a decision.

## 3. Mode-9 exactness

`modes::float_tensor_encoded_len_from_histogram` rebuilds the canonical WCB
descriptor with the computed `exponent_plane_len` and `mantissa_plane_len`,
computes the exponent plane length with the same length-limited canonical tree
and the same `Σ f_s·L_s` bit count the emitter writes, and adds the fixed frame
header/trailer, the 4-byte body prefix and the `ceil(count·(sign+mantissa)/8)`
raw plane. The only field not recomputed is `payload_hash`, whose WCB encoding is
a 32-byte byte string regardless of content.

Equality with the encoder is asserted in
[`WeftCodec/tests/float_tensor_len.rs`](../WeftCodec/tests/float_tensor_len.rs)
(WeftCodec `0aa3881`, 5/5 tests):

| Case | Instances | Mismatches |
|---|---:|---:|
| bf16, fp16, fp32, fp8-e4m3, fp8-e5m2 × sizes 1…65,537 elements | 45 | 0 |
| single-symbol / two-symbol / full-256-symbol / mantissa-rounding histograms | 12 | 0 |
| NaN, ±inf, subnormal, ±0 and max-finite bit patterns; shapes 1-D…5-D | 9 | 0 |
| `huffman::encoded_len` vs `huffman::encode` incl. the empty plane | 5 | 0 |
| every committed mode-9 golden frame under `conformance/v1` | 3 | 0 |

At the store level, `mode9_predictions_checked_against_actual: 35` with
`mode9_prediction_mismatches: 0` on the frozen fixture and on Qwen2.5-0.5B base
(35/35 each); the prediction is also compared against an independent
`encode_float_tensor` call for every fixture in
[`tests/two_stage_planner.rs`](../tests/two_stage_planner.rs).

The predictor is exact, but the **margin is zero**: a mismatch is a test failure,
not a bound. It is covered over the five dtypes the mode implements, the
degenerate histograms the coder distinguishes, the committed vectors, and every
tensor of two real model families.

## 4. ZSTD estimator calibration

**Level 3.** Sample whole codec chunks (64 KiB) at deterministic indices that
always include the first and last full chunk, at most 4. The point prediction is
the mean per-chunk compressed size × the chunk count plus the exact frame
overhead (96 B header, 4 B trailer, 48 B per chunk). The interval is
`predicted × (1 ± rel)` where `rel = max(500 bps, sampled spread)`, plus the
unmeasured trailing partial chunk's own byte length as absolute slack.

**Level 19.** The same windows are compressed at level 19 (at most 2), giving a
*ratio* to level 3 on identical bytes. Before the level-3 frame exists the ratio
multiplies the level-3 estimate (and its interval propagates); after it exists
the ratio multiplies the **measured** level-3 frame. That is what stops a
heterogeneous tensor from destroying the prediction: the 272,269,312-byte
embedding's four-chunk level-3 interval degenerates to `0..324,219,036 B`
(23.1 % error), while the 19/3 ratio on the same windows is within a fraction of
a percent, so the level-19 bound stays usable.

Observed accuracy in this court (all actuals inside their intervals; zero
violations in every run):

| Family | level-3 max relative error | level-19 max relative error | actuals outside interval |
|---|---:|---:|---:|
| Qwen2.5-0.5B-Instruct (101 tensors, 51 level-3 encodes) | 23.1 % (embedding; mean 0.45 %) | n/a (0 encoded) | 0 |
| Qwen2.5-0.5B base (101, 51) | 23.0 % (mean 0.45 %) | n/a | 0 |
| pythia-70m step0 (76, 76 encoded) | 0.21 % (mean 0.02 %) | 4.6 % (mean 1.4 %) | 0 |
| pythia-70m step8 (76, 76) | 0.25 % (mean 0.02 %) | 4.3 % (mean 1.6 %) | 0 |

The 500 bps floor is therefore *not* generous for a heterogeneous embedding, and
the interval is honest about it (lower bound 0). The consequence is one extra
level-3 encode on the embedding, not a wrong decision.

## 5. Decision diff

`C-legacy` (exhaustive) → `C` (two-stage), per tensor, same seed and clock, on
the frozen fixture:

| Quantity | Exhaustive | Two-stage |
|---|---:|---:|
| chosen representations | 35 float-tensor, 50 zstd, 16 raw | **identical** |
| strategy or chosen-size changes | — | **0 of 101** |
| level-3 encodes | 101 | 51 |
| level-19 encodes | 18 | **0** |
| mode-9 encodes | 101 | 35 |
| chosen physical bytes (sum of frames) | 281,951,111 | **281,951,111** |
| published footprint | 282,283,011 | **282,283,011** |
| level-3 skip bounds recorded | 0 | 50 |
| level-19 skip bounds recorded | 0 | 18 (83 further tensors are below the 1 MiB gate and were never offered) |
| mode-9 predicted vs actual | — | 35 checked, **0 mismatches** |

Why mode 9 is skipped for 66 tensors but nothing is saved by it: those 66
tensors are **93,696 bytes** in total (0.022 % of the fixture). The 35 mode-9
winners hold 424,345,600 of 424,439,296 bytes. The R17 ledger's "mode 9 costs
1.74 s" is real, but it is the cost of encoding the tensors that *win*.

## 6. Cross-family validation

Both planners, same fixture, same seed, one repetition each, decision diff per
tensor ([§5](#5-decision-diff) method):

| Family | Tensors | Strategy/size changes | Chosen bytes Δ | mode-9 exact | Level-19 encoded | Level-19 skipped with a bound |
|---|---:|---:|---:|---:|---:|---:|
| Qwen2.5-0.5B base | 101 | 0 | 0 | 35/35 | 0 | 18 |
| pythia-70m step0 | 76 | 0 | 0 | n/a (fp32 never wins) | **26** | 50 |
| pythia-70m step8 | 76 | 0 | 0 | n/a | **26** | 50 |

On pythia, level 19 is **smaller than level 3 for 26 of 26 eligible tensors**
(163,863,698 vs 173,186,380 B at step0; 163,678,039 vs 173,178,386 at step8): the
gate encodes it and the saving is kept. **Deleting the level-19 candidate — the
literal R17 wording — would have lost 9.3 MB (5.4 %) of that family's
footprint.** This is the storage the conditional gate protects, and it is why
R18 implemented the rule instead of the removal the R17 ledger suggested.

## 7. Measured write wall, resources and exactness

7 interleaved repetitions per configuration, order rotated by one position per
repetition, phase profile enabled on every repetition, per-tensor rows on
repetition 1. All values are the **least-contended** observation; medians and
p95 of the raw samples are in the receipt (§9 explains why the minimum is used).

| Config | wall min (ms) | median | p95 | CPU min (ms) | peak RSS (KiB) | footprint (B) | cold restore min (ms) | exact |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| A native | 327.8 | 336.7 | 6,759.4 | 90 | 270,864 | 424,439,296 | 346.4 | 101/101 |
| B pass-through | 2,848.0 | 2,859.1 | 20,697.8 | 270 | 272,236 | 424,784,917 | 542.6 | 101/101 |
| B-group | 2,144.3 | 2,174.4 | 31,924.1 | 230 | 272,464 | 424,608,542 | 535.9 | 101/101 |
| **C two-stage** | **4,117.8** | 4,198.0 | 31,490.6 | 2,660 | 1,187,868 | **282,283,011** | 1,425.3 | **101/101** |
| C-legacy exhaustive | 24,175.8 | 24,224.9 | 40,327.3 | 22,550 | 1,186,732 | 282,283,011 | 1,414.4 | 101/101 |
| C-noz19 (exhaustive) | 3,936.0 | 3,947.9 | 3,986.9 | 2,480 | 1,185,096 | 282,283,011 | 1,406.9 | 101/101 |
| C-nom9 (exhaustive) | 22,421.6 | 22,567.7 | 47,555.6 | 20,870 | 891,488 | 330,336,572 | 878.4 | 101/101 |
| C-both (exhaustive) | 2,136.6 | 2,159.3 | 26,691.6 | 680 | 686,400 | 330,532,422 | 832.8 | 101/101 |
| D protocol | 2,892.0 | 2,919.3 | 48,194.9 | 270 | 272,280 | 424,856,058 | 549.4 | 101/101 |
| E combined | 4,145.5 | 4,168.4 | 27,288.4 | 2,670 | 1,187,348 | 282,354,152 | 1,428.1 | 101/101 |
| N native-app | 339.0 | 346.5 | 6,801.6 | 90 | 271,120 | 424,477,171 | 347.1 | 101/101 |

Write requests are unchanged at 516 for C/E (the representation set and the
commit protocol are unchanged); write RSS is unchanged within noise.

### The C ledger (least-contended repetition, 4,117.75 ms)

| Category | ms | % of wall |
|---|---:|---:|
| init (store create, FORMAT, LOCK, superblock) | 20.79 | 0.50 |
| source read | 31.59 | 0.77 |
| **prepare** | **3,327.23** | **80.80** |
| ├ encode total | 2,458.53 | 59.71 |
| │ ├ mode-9 `FLOAT_TENSOR` | 1,790.02 | 43.47 |
| │ ├ **analyse** | **348.76** | **8.47** |
| │ │ └ of which the mode-9 histogram + prediction | 223.95 | 5.44 |
| │ ├ ZSTD level-3 | 278.41 | 6.76 |
| │ ├ ZSTD level-19 | 0.00 | 0.00 |
| │ └ other encode/plan | 41.34 | 1.00 |
| ├ blob publish (202 publications) | 776.23 | 18.85 |
| ├ digest | 65.81 | 1.60 |
| └ prepare unattributed | 26.66 | 0.65 |
| **commit** | **721.91** | **17.53** |
| ├ manifest commit (commit record + superblock) | 358.80 | 8.71 |
| ├ index segment (advisory) | 357.53 | 8.68 |
| └ commit unattributed | 5.58 | 0.14 |
| footprint walk | 1.40 | 0.03 |
| **accounted** | **4,102.92** | **99.64** |
| **unattributed residual** | **14.84** | **0.36** |
| **measured wall** | **4,117.75** | **100.00** |

The ledger closes to 0.36 % (14.84 ms) against R17's 0.065 %; the extra come
from the analyser's own bookkeeping and from `fsync` remaining inside
`BlobPublish`/`ManifestCommit`, as R17 said.

### Why this is 182 ms slower than `C-noz19`

`C-noz19` (exhaustive planner, level 19 disabled outright) is 3,936.0 ms. R18's
two-stage C is 4,117.8 ms. The 182 ms difference decomposes as:

* **+348.8 ms** analyse (224.0 ms exact mode-9 histogram, 124.8 ms ZSTD
  sampling);
* **−161.4 ms** fewer level-3 encodes (278.4 vs 439.8 ms);
* **−18.2 ms** fewer mode-9 encodes (1,790.0 vs 1,808.2 ms) — the six skipped
  tensors that are not below the mode-9 gate are 93,696 bytes in total;
* the remainder is run-to-run noise in publication and commit.

The target for R18 was "**C ≤ ~3,850 ms** with the footprint unchanged". The
measured figure is **4,117.8 ms**, i.e. **7.0 % above** that expectation and
1.23× the G1 rung (3,360 ms). The footprint part of the target is met exactly.
The cause is measured, not guessed: the exactness requirement forces a full
exponent histogram (224 ms), and the encodes it allows the planner to skip cost
almost nothing because the losing tensors carry 0.022 % of the bytes.

## 8. Go parity

**Not claimed.** The Go store's planner (`WeftFS-Go/store/tensor.go`,
`prepareTensorCandidates`) offers the same candidate set and applies the same
rules: raw, level-3 ZSTD, level-19 ZSTD gated by `NoZstd19`/size/CPU budget, and
mode 9 gated by the identical `physical*64 <= zstd*63` rule against the measured
level-3 frame. It reaches the same decisions **by exhaustive encoding** — it has
no analyse stage, no interval and no skip. So:

* storage divergence: **none by construction** (same candidates, same prices,
  same actual-byte comparison), and it is not measured on the fixture here;
* work divergence: Go still encodes level 19 for every eligible tensor and mode
  9 for every float tensor, i.e. it pays the `C-legacy` cost (24,175.8 ms class)
  rather than 4,117.8 ms.

The WeftFS-Go submodule pointer is frozen for this subphase, so no Go change was
made or possible. The Go planner tests still pass at the pinned revision
(`go test -count=1 -run 'TestFloatTensorCandidateAndCostModel|TestRebaseHotDeltaMatchesRustSemantics' ./store/` → ok, go court).

## 9. Losses, limits and non-claims

* **The target was missed:** 4,117.8 ms vs ~3,850 ms. Cause in §7. The remaining
  wall is 1,790 ms of mode-9 encode, 776 ms of blob publish and 716 ms of
  commit; R19 (encoder) and R21 (bounded parallelism, including the analyse
  pass) are where it must come from.
* **The analyse stage is serial.** It adds 348.8 ms to a 4.1 s write and is
  per-tensor independent, so it is parallelisable under R21.
* **The level-3 sampler cannot describe a heterogeneous tensor.** The 272 MB
  embedding's interval degenerates to `0..324 MB`; the planner then encodes
  level 3 to be safe (one extra encode, ~150 ms in this run). It does not
  produce a wrong decision, but a better sampling rule (more chunks for large
  tensors) is a real improvement and was not made.
* **The 500 bps floor is empirical.** It is calibrated on these four real
  families and nothing else; a corpus with radically different within-tensor
  variance could fall outside it. A skipped candidate cannot be re-measured by
  construction, so the defence is the decision diff, not the constant.
* **The level-19 ratio sample is 2 chunks.** On pythia the whole-frame error
  reaches 4.6 % against a ±5 % interval; a different data shape could exceed it
  and force an encode (safe) or, worse, produce a lower bound that is too high
  (unsafe). The four families tested show zero violations.
* **Only four candidates are analysed.** Mode 10, modes 15/17/18/20, the causal
  deltas (11/14/16) and the zstd patch are prepared exactly as before. The R17
  ledger prices them at 41 ms of 24,229 ms, so this is a bounded scope, not a
  claim of global optimality.
* **`plan_tensor` and rebase stay exhaustive.** `weft plan` therefore reports
  every candidate's measured size, not the two-stage decision. The decision
  stream for a write is in that write's plan trace and receipt.
* **Single host, warm cache, local backend, one repetition set.** The host was
  shared; the median and p95 include an external CPU/I/O-heavy process
  (`C` p95 31,490.6 ms against a 4,198.0 ms median). The least-contended value is
  used for attribution, exactly as R17 did, and both are published.
* **No power-loss, fleet, cloud or cold-media claim.** The crash proxy is a
  process-fault proxy: three phases per Weft configuration, every one reopened
  old-or-new-whole with a full verify and 101 exact tensors.
* **The pythia fixtures select all 76 tensors**, not an embedding + 100 evenly
  spaced subset, because those containers have no `model.embed_tokens.weight`.
  The Qwen base/instruct pair uses the frozen 101-tensor selection.

## 10. Reproduce

```sh
# 1. the contract court at this revision (A/B/C/D/E/N, one run each)
WEFT_AUGMENTATION_STAMP=<stamp> scripts/augmentation_court.sh --local

# 2. the matrix (7 interleaved repetitions) and the cross-family decision diffs
scripts/docker-court.sh bench "cd /work && \
  WEFT_PLANNER_STAMP=<stamp> \
  WEFT_PLANNER_OFFICIAL_RECEIPT=/work/bench/receipts/augmentation-local-<stamp>.json \
  bash bench/scripts/planner_two_stage_court.sh"

# 3. the exactness of the mode-9 predictor (codec level)
scripts/docker-court.sh rust "cd /work/WeftCodec && cargo test --test float_tensor_len"

# 4. the planner contract tests
scripts/docker-court.sh rust "cd /work && cargo test --test two_stage_planner"
```

Court: `weft/bench-court:1`
`sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559`,
kernel `7.3.3-1-cachyos`, 3 GiB with swap == memory, 4 CPUs, 1024 pids, bounded
`/tmp`, `no-new-privileges`, `cap-drop=ALL`. Limits were not raised.
