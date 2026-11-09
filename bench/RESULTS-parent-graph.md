# Parent-graph planning on a real pythia-70m trajectory (S13, corrected by S13b)

Receipts (all at WeftFS `4fc1ece` / WeftCodec `cffcc1a`, default bench court:
3 GiB / 4 cpus / 1024 pids, `weft/bench-court:1` id `sha256:4c6c41fb…`):

| evidence | receipt |
|---|---|
| S13b restore-overhead calibration | `bench/receipts/s13b-restore-floor-20261108T230417Z.json` |
| S13b parent-graph court | `bench/receipts/parent-graph-20261108T232016Z.json` |
| S13b first court run (retained; larger cold-cache penalty) | `bench/receipts/parent-graph-20261108T215207Z.json` |
| S13 original court (superseded predictions) | `bench/receipts/parent-graph-20261108T192017Z.json` |

Reproduce:

```sh
scripts/docker-court.sh bench \
  "cd /work && bash bench/scripts/s13b_restore_floor_court.sh"
scripts/docker-court.sh bench \
  "cd /work && bash bench/scripts/parent_graph_court.sh"
```

Corpus: the nine cached pythia-70m checkpoints, 281,715,176 B and 76 tensors
each, re-hashed in both courts (step 0
`17a077abfddb6c589091e292adfcc2ee82d39bfd839f5dd80aaaea42861aaf8e`, step 1000
`09883f904c4a50de85c682cdfa0434aaf34e62392fe883e58410d12d39b1ee88`).

## What S13b changed

S13's model summed the measured affine decode rows and nothing else. Its own
court measured the resulting error: the chain worst restore was over-predicted
by 24.2 % and the all-independent floor under-predicted by 73.7 % (0.211 s
predicted, 0.802 s measured). An SLA chosen between those numbers would have
been accepted and missed.

S13b adds a **measured whole-checkpoint restore overhead**, charged once per
version restore:

```
restore_ns(i) = decode_chain_ns(i) + restore_overhead_ns(i)
restore_overhead_ns(i) = per_version_ns + per_tensor_ns * tensors(i)
                       + per_mib_ns * logical_mib(i)
```

The baked term (`RESTORE_OVERHEAD_CALIBRATION =
weftfs-s13b/restore-overhead/v1`) is `13,285 / 29,205 / 2,114,993` ns. It is
fitted on real `Store::restore_checkpoint` calls, not on in-memory frame
decode, and validated on the real checkpoints. The solvers now prune on the
corrected restore (not the raw decode chain), and an SLA claim with no
overhead model is refused with a typed `InvalidArgument`.

## Calibration: which hypothesis held

The harness (`examples/calibrate_restore.rs`) writes complete checkpoints with
the writer's `Planned` representation and times `restore_checkpoint` from a
fresh read-only handle, 7 repetitions per point (154 fitted samples), then fits
`residue = measured_restore − Σ decode rows`.

| hypothesis | verdict | evidence |
|---|---|---|
| 1. per-object fixed cost | **refuted above ~256 KiB** | at a fixed 64 MiB total: one 64 MiB tensor = 136.2 ms residue, sixty-four 1 MiB tensors = 127.0 ms. No positive count slope until tensors fall below ~256 KiB. |
| 2. per-version cost | real but tiny | manifest read 14.0 µs for 1 tensor, 0.31 ms for 76, 77 ms for 16,384. The per-tensor term absorbs it; the measured intercept is ~0. |
| 3. chain over-prediction is a misplaced constant | **no** | the controlled chain shows the over-prediction is per hop and class-dependent (below), not a per-version term counted per hop. |

The measured per-object term is the small-object fixed work: a 4 KiB tensor
carries ~30–40 µs of restore work that is not decode. Model checkpoints have
few such tensors (44 tensors of 2–8 KiB in pythia = ~1.8 ms), so it is a
correctness term, not a pythia-sized one.

Fit residuals: worst point residual 31.6 % (a 128 × 512 KiB point), which is
the cost of forcing one linear count slope over a range where the per-object
cost only turns on below 256 KiB. Real-container validation (step 0):
predicted 781.6 ms, measured 780.1 ms (+0.19 %). Whole-trajectory validation
(all nine checkpoints, each written independently into one store): predicted
781.6 ms for every version, measured 778.2–793.3 ms, error −1.5 %..+0.4 %.

Chain-hop check (8 MiB tensor, 8 hops, `Planned`):

| hops | chosen representation | measured increment | model row | model error |
|---|---|---:|---:|---:|
| 1–2 | sparse-delta | ~14.9–15.9 ms | 42.4 ms | +166 %..+184 % (model too high) |
| 3–8 | weight-delta | 35.6–38.7 ms | 42.4 ms | +10 %..+19 % (model too high) |

This is the S13b chain residual: `SPARSE_DELTA` and `ZSTD_PATCH` share the
`WEIGHT_DELTA` decode row, and level-19 zstd shares the level-3 `ZSTD` row.
Correcting those candidate prices would change stored strategies across
corpora and is explicitly out of scope; the graph's chain/restart predictions
are therefore conservative.

## Corrected plan comparison (model)

`lambda_ns = 1` (the measured break-even values are 0.008408 / 0.005266 /
0.004714 bytes/ns for restart intervals 2/3/4), SLA 4 s.

| plan | parents | depth | edge bytes | decode worst | **corrected worst restore** |
|---|---|---:|---:|---:|---:|
| current chain | `−1,0,1,2,3,4,5,6,7` | 8 | 647,175,737 | 11,599.9 ms | 12,170.3 ms |
| restart every 2 | `−1,0,−1,2,−1,4,−1,6,−1` | 1 | 1,030,180,485 | 1,634.8 ms | 2,205.3 ms |
| restart every 3 | `−1,0,1,−1,3,4,−1,6,7` | 2 | 849,572,567 | 3,058.6 ms | 3,629.1 ms |
| restart every 4 | `−1,0,1,2,−1,4,5,6,−1` | 3 | 808,225,589 | 4,482.4 ms | 5,052.8 ms |
| **DP @ λ=1, SLA 4 s** | all independent | 0 | 1,474,646,073 | 211.1 ms | **781.6 ms** |
| DP @ λ=0, SLA 4 s | = restart every 3 | 2 | 849,572,567 | 3,058.6 ms | 3,629.1 ms |
| brute force @ λ=1 | all independent | 0 | 1,474,646,073 | 211.1 ms | 781.6 ms |

The DP and the exhaustive oracle still agree exactly. The per-version overhead
is a constant across plans (every plan restores every version), so it changes
no objective ordering; it only changes SLA feasibility and the reported times.

## Realised stores (7 restore medians per version)

| plan | data bytes | bytes error | **predicted worst** | **measured worst** | error | Σ measured | exact | verify |
|---|---:|---:|---:|---:|---:|---:|---|---|
| chain | 647,411,616 | +235,879 (+0.036 %) | 12,170.3 ms | 9,238.2 ms | +31.7 % | 40,988.0 ms | 9/9 | ok |
| restart-2 | 1,030,285,678 | +105,193 (+0.010 %) | 2,205.3 ms | 2,049.9 ms | +7.6 % | 11,454.4 ms | 9/9 | ok |
| restart-3 | 849,712,592 | +140,025 (+0.016 %) | 3,629.1 ms | 3,234.3 ms | +12.2 % | 16,146.5 ms | 9/9 | ok |
| restart-4 | 808,373,400 | +147,811 (+0.018 %) | 5,052.8 ms | 4,503.9 ms | +12.2 % | 18,662.8 ms | 9/9 | ok |
| optimizer (λ=1) | 1,447,239,539 | −27,406,534 (−1.86 %) | 781.6 ms | 886.5 ms | **−11.8 %** | 7,618.0 ms | 9/9 | ok |

Every version in every plan matched the source container byte for byte and
every store passed `verify --full` with 0 problems.

Chain per-hop increments inside the realised chain plan (measured boundary
between consecutive versions vs the model's incremental decode row):

| hop | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| measured ms | 604.2 | 515.1 | 1,180.0 | 1,190.4 | 1,209.9 | 1,275.6 | 1,256.3 | 1,215.3 |
| model ms | 1,423.4 | 1,423.4 | 1,423.4 | 1,423.4 | 1,423.7 | 1,423.9 | 1,423.7 | 1,423.8 |

Hops 1–2 are sparse-delta/patch edges (model priced as weight-delta); hops 3–8
are weight-delta. This is why the chain's corrected prediction is +31.7 %
high: the per-hop rows over-price the real deltas, and the overhead correction
is added once, not per hop.

## The floor residual: a page-cache-capacity boundary

The all-independent plan is the floor. Its corrected prediction is 781.6 ms;
the court measured 886.5 ms (−11.8 %). The per-version detail shows this is
not a model error but a measurement-regime change:

| versions | predicted | measured | error |
|---|---:|---:|---:|
| 0–3 (step 0/1/2/4) | 781.6 ms | 796.0–807.2 ms | +1.8 %..+3.3 % |
| 4–8 (step 8/16/64/512/1000) | 781.6 ms | 878.5–886.5 ms | −11.3 %..−11.8 % |

All nine versions read the same 163.9 MB from the store; the step is not
explained by bytes. The court restores nine 1.5 GB versions inside a 3 GiB
cgroup; versions 0–3 stay in the page cache and versions 4–8 do not, adding
~100 ms per 164 MB read (~0.9 GB/s from the device). The same nine checkpoints
restored in the calibration court's fresh store (smaller footprint) measure
778–793 ms, all inside 1.5 %. The first S13b court run showed the same step
with a larger penalty (782–805 ms warm, 964–987 ms cold). The overhead term is
measured warm; the S13 court already labelled its restore as "warm OS page
cache", and this is the quantified boundary of that label. The constant was
**not** raised to cover the cold case.

## SLA sweep: accepted plans and whether they hold

`500, 850, 1000, 2000, 2500, 3000, 4000, 6000, 14000` ms at λ = 0 and λ = 1:

| SLA | λ=0 choice | λ=1 choice | predicted | measured | holds |
|---:|---|---|---:|---:|---|
| 500 ms | refused (`LimitExceeded`) | refused | — | — | — |
| 850 ms | all independent | all independent | 781.6 ms | 886.5 ms | **no** (cold versions) |
| 1000 ms | all independent | all independent | 781.6 ms | 886.5 ms | yes |
| 2000 ms | all independent | all independent | 781.6 ms | 886.5 ms | yes |
| 2500 ms | restart-2 | all independent | 2,205.3 / 781.6 ms | 2,049.9 / 886.5 ms | yes |
| 3000 ms | restart-2 | all independent | 2,205.3 / 781.6 ms | 2,049.9 / 886.5 ms | yes |
| 4000 ms | restart-3 | all independent | 3,629.1 / 781.6 ms | 3,234.3 / 886.5 ms | yes |
| 6000 ms | restart-4 | all independent | 5,052.8 / 781.6 ms | 4,503.9 / 886.5 ms | yes |
| 14000 ms | chain | all independent | 12,170.3 / 781.6 ms | 9,238.2 / 886.5 ms | yes |

The 850 ms row is the only accepted plan that misses: its prediction (781.6 ms)
is inside 850 ms and its *warm* measurement (778–807 ms) is too, but the cold
versions measure 886.5 ms. The court's `no_accepted_plan_misses_its_sla` check
is therefore **false**; it is reported as observed. At every other SLA in the
sweep, every accepted plan holds.

Changes from S13: the plan choices at λ=0/λ=1 and SLA 4 s are unchanged
(independent at λ=1, restart-3 at λ=0). The corrections show up as (a) a
typed refusal at 500 ms, which S13 would have accepted with a 211 ms
prediction, and (b) an 850 ms acceptance that is now marginal and, cold,
missed. All other sweep choices are unchanged.

## Honest verdict

* The floor is now predicted from a measured term, not from the decode rows
  alone: 781.6 ms against a warm 778–807 ms (within 4 %) and a cold
  878–886 ms (−11.8 %). The S13 73.7 % error is gone; the residual is a
  quantified cache-capacity boundary.
* Chain and restart predictions are conservative, not optimistic: +31.7 %
  (chain), +12.2 % (restart-3/4), +7.6 % (restart-2). They over-state restore
  time because sparse-delta, zstd-patch and zstd-19 are priced with the
  weight-delta and level-3 zstd rows. That is a per-candidate pricing decision
  left untouched.
* The chain is still the smallest plan (647.4 MB) and the slowest (9.24 s
  worst, 41.0 s for all nine); the λ=1 optimum is all-independent
  (1,447.2 MB, 0.89 s worst, 7.62 s for all nine). The trade is unchanged; the
  prediction of its cost is corrected.
* The model is a single host, warm-cache, single-threaded restore model. It
  does not model cold media, disk throughput, CPU contention, or per-candidate
  decode-row bias. The court publishes the gap in every case it observed.

## What is not claimed

Not a Go implementation (WeftFS-Go has no parent-graph planner; the overhead
constant is documented in the Rust cost model only and no cross-language
agreement is claimed). Not a multi-writer protocol. No physical power-loss
test. No per-candidate repricing. No cold-media or fleet SLA. One host, one
public corpus, warm OS page cache for the calibration (the court's cold step is
reported, not corrected).

## Historical record: the original S13 court (superseded predictions)

This section is retained verbatim in substance so the S13 evidence stays
readable. Its **model** columns are the uncorrected ones; S13b above replaces
them. Receipt: `bench/receipts/parent-graph-20261108T192017Z.json` (3 restore
reps per version).

| plan | parents | depth | edge bytes | Σ chain decode | worst restore (model) |
|---|---|---:|---:|---:|---:|
| current chain | `−1,0,1,2,3,4,5,6,7` | 8 | 647,175,737 | 53,146.7 ms | 11,599.9 ms |
| restart every 2 | `−1,0,−1,2,−1,4,−1,6,−1` | 1 | 1,030,180,485 | 7,594.5 ms | 1,634.8 ms |
| restart every 3 | `−1,0,1,−1,3,4,−1,6,7` | 2 | 849,572,567 | 14,712.2 ms | 3,058.6 ms |
| restart every 4 | `−1,0,1,2,−1,4,5,6,−1` | 3 | 808,225,589 | 18,983.3 ms | 4,482.4 ms |
| DP @ λ=1, SLA 4 s | all independent | 0 | 1,474,646,073 | 1,900.3 ms | 211.1 ms |
| DP @ λ=0, SLA 4 s | = restart every 3 | 2 | 849,572,567 | 14,712.2 ms | 3,058.6 ms |
| greedy @ λ=1 | all independent | 0 | 1,474,646,073 | 1,900.3 ms | 211.1 ms |
| brute force @ λ=1 | all independent | 0 | 1,474,646,073 | 1,900.3 ms | 211.1 ms |

The DP and the exhaustive oracle agreed exactly (`dp_matches_brute_force:
true`). The realised stores (3 restore medians per version):

| plan | data bytes | predicted | error | worst restore | Σ restore |
|---|---:|---:|---:|---:|---:|
| chain | 647,411,616 | 647,175,737 | +235,879 (+0.036 %) | 9,336.3 ms | 41,218.8 ms |
| restart-2 | 1,030,285,678 | 1,030,180,485 | +105,193 (+0.010 %) | 2,448.9 ms | 12,583.3 ms |
| restart-3 | 849,712,592 | 849,572,567 | +140,025 (+0.016 %) | 3,248.2 ms | 16,289.9 ms |
| restart-4 | 808,373,400 | 808,225,589 | +147,811 (+0.018 %) | 4,650.7 ms | 18,856.6 ms |
| optimizer (λ=1) | 1,447,239,539 | 1,474,646,073 | −27,406,534 (−1.86 %) | 801.5 ms | 7,128.7 ms |

S13's verdict: the chain was over-predicted by 24.2 % (11.60 s model vs
9.34 s measured) and the all-independent floor under-predicted by 73.7 %
(0.211 s vs 0.802 s) because the fixed restore work was not in the affine
table. That is the defect S13b fixes; the corrected numbers are above.
