# R9 — parent-graph planner v2: write term, per-representation rows, uncertainty

Status: implemented in Rust (`src/plan_graph.rs`, `src/cost_model.rs`,
`src/calibration.rs`) and measured by the pythia-70m trajectory court. No wire
format changed. Receipt: `bench/receipts/parent-graph-20261110T015802Z.json`
(9 checkpoints, 14 SLAs × 2 lambdas, 7 restore reps). Earlier runs retained:
`parent-graph-20261110T014508Z` and `parent-graph-20261110T004938Z` (pre-fix
model), and `parent-graph-20261109T101230Z` (S13b).

## The two defects this closes

1. The graph objective had **no write term**: it priced bytes plus decode time,
   so a representation that is cheap to store but expensive to *build* looked
   free. Hops also shared one decode row, over-predicting chains by 31.7 %
   (sparse-delta hops measured 14.9–15.9 ms against a 42.4 ms row).
2. The SLA check compared a point prediction to the SLA with no uncertainty, so
   an **850 ms SLA was accepted and measured 886.5 ms** (S13b).

## The cost decomposition, with coefficients and sources

```
C = storage(bytes)                     1 unit per physical byte
  + lambda_write * encode_ns           measured write rows
  + lambda_retention * bytes           explicit; NO measured table exists
  + lambda_read * reads * chain_ns     measured decode rows + restore overhead
```

* `lambda_write` defaults to `lambda_read` (break-even: one write traded for one
  read). `--lambda-write-ns` overrides it.
* `lambda_retention` defaults to 0 and is only ever explicit: there is **no
  measured retention table**, so the term is reported (`retention_cost_units`)
  and not silently priced with a neighbouring constant.
* `encode_ns` comes from `src/cost_model.rs` `estimate_encode_ns`, calibration
  `weftfs-write-cost/v1` (T1 measured raw / zstd-1 / zstd-3 / planned on the
  real Qwen pair; 6.5–7.6x lower encode CPU for the fast paths).
* `chain_ns` = Σ per-hop increment + `RestoreOverhead { per_version_ns:
  13,285, per_tensor_ns: 29,205, per_mib_ns: 2,114,993 }` from
  `weftfs-s13b/restore-overhead/v1`, charged once per version restore (never
  per hop), plus `SLA margin` `617,758 ns/MiB` from `weftfs-s13c/sla-margin/v1`.

Per-representation hop rows (`weftfs-s13c/chain-hop/v1`, controlled 8-hop
chain, 48 samples per class, median of 3 courts × 7 reps):

| strategy | fixed ns | ns/MiB | source |
|---|---:|---:|---|
| raw | 11,757 | 307,019 | `weftfs-r2/decode-calibration/v1` |
| zstd | 21,285 | 779,888 | `weftfs-r2/decode-calibration/v1` |
| float-tensor (mode 9) | 18,264 | 4,683,726 | `weftfs-r2/decode-calibration/v1` |
| tensor-sharded (mode 10) | 16,172 | 9,123,307 | `weftfs-r2/decode-calibration/v1` |
| tensor-tiled (mode 15) | 14,131 | 3,366,772 | `weftfs-r2/decode-calibration/v1` |
| weight-delta (mode 11) | 0 | 4,690,851 | `weftfs-s13c/chain-hop/v1` |
| sparse-delta (mode 16) | 0 | 2,071,283 | `weftfs-s13c/chain-hop/v1` |
| zstd-patch (mode 4) | 0 | 1,148,427 | `weftfs-r9/zstd-patch-hop/v1` (F32 chain) |
| semantic-delta (mode 14) | 31,891 | 6,458,523 | `weftfs-r2/decode-calibration/v1` |

Mode 18 (`FLOAT_TILED_STREAMS`) uses its own measured row
(`weftfs-mode18/decode-calibration/v1`). There is **no row** for mode 17
(`FLOAT_TILED`), mode 20 (`ENGRAM_SHARD`), or the read-only KV/activation
strategies: pricing one returns the typed `UnmeasuredCost` class naming the
strategy. The planner never substitutes a neighbouring row. (The historical
decode table still carries declared KV/activation integers; they are not
reachable as a hop price.)

A BF16 zstd-patch chain row measured 1,675,326 ns/MiB in the first run; the
frozen F32 chain row of 1,148,427 is used because that is the fixture the court
prices. The BF16 measurement is in the retained `090000Z` receipt.

## Uncertainty and the SLA rule

The bound is the measured **under-prediction** of the fitted samples, carried as
an integer ppm per class (`weftfs-r9/hop-uncertainty/v1` and
`weftfs-r9/decode-uncertainty/v1`):

| class | samples | bound |
|---|---:|---:|
| zstd-patch | 48 | 624,556 ppm (62.5 %) |
| weight-delta | 48 | 178,209 ppm (17.8 %) |
| sparse-delta | 48 | 153,302 ppm (15.3 %) |
| decode rows (all classes) | cross-run | 10,000–10,500 ppm (1.0–1.05 %) |

The SLA rule is `prediction + margin + Σ hop uncertainty <= sla`. A plan that
fails it is refused with `LimitExceeded` naming the best achievable bound; there
is no "close enough" acceptance. The bound is warm-cache, single-host, local
backend; it is not a cold-media or fleet bound.

## Corrected prediction table

`bench/receipts/parent-graph-20261110T015802Z.json`, prediction error =
`(predicted − measured) / measured`; a positive error over-predicts. "bound" is
`worst_sla_bound_ms`, the value the SLA is compared against.

| plan | predicted ms | measured ms | error | bound ms | SLA | exact |
|---|---:|---:|---:|---:|---|---|
| chain (max depth 8) | 9,377.9 | 9,208.0 | +1.85 % | 11,094.4 | holds | yes |
| restart-2 | 2,042.3 | 2,041.5 | +0.04 % | 2,435.0 | holds | yes |
| restart-3 | 3,303.0 | 3,345.2 | −1.26 % | 3,920.2 | holds | yes |
| restart-4 | 4,563.8 | 4,727.4 | −3.46 % | 5,405.5 | holds | yes |
| optimizer (DP, exact) | 2,042.3 | 2,105.9 | −3.02 % | 2,435.0 | holds | yes |
| optimizer, bytes only | 3,303.1 | 3,367.9 | −1.93 % | 3,920.3 | holds | yes |
| **floor** (all independent) | 781.6 | **930.7** | **−16.02 %** | 949.8 | holds | yes |
| sweep plan 2 | 1,333.4 | 1,402.1 | −4.90 % | 1,589.2 | holds | yes |
| sweep plan 3 | 3,303.1 | 3,311.5 | −0.26 % | 3,920.3 | holds | yes |
| sweep plan 4 | 5,051.7 | 5,021.0 | +0.61 % | 5,978.9 | holds | yes |
| sweep plan 5 | 6,312.4 | 6,269.6 | +0.68 % | 7,464.1 | holds | yes |
| sweep plan 6 | 8,282.1 | 8,094.4 | +2.32 % | 9,795.1 | holds | yes |
| sweep plan 7 | 8,833.9 | 8,738.2 | +1.09 % | 10,434.6 | holds | yes |
| (restore floor used by every plan) | 781.6 | 780.1-793.3 | ≤ +0.4 % | — | — | yes |

**No accepted plan missed its SLA: 22/22 measured acceptances, 0 misses.**
Twelve of thirteen rows are within 5 %. The 15 % target is met for every plan
class except the all-independent floor (−16.02 %), which is a page-cache effect,
not a model constant: the same 163.9 MB read restores 796–807 ms for the first
versions (warm) and 878–886 ms for the last (cold) inside the court, while a
fresh-store trajectory measures 778–793 ms. The dependence on cgroup page-cache
capacity is recorded in S13b and is not corrected by tuning a coefficient.

## SLA accept/refuse boundary, before and after

| SLA | before (S13b model) | after (v2 with uncertainty) |
|---|---|---|
| 500 ms | refused (best floor 781.6) | refused, best bound 949.8 |
| **850 ms** | **accepted**, measured 886.5 → **missed** | **refused**, best bound 949.8 |
| 900 ms | accepted, measured 886.5 (held) | **refused**, best bound 949.8 |
| **950 ms** | accepted (floor) | **accepted (floor)**, bound 949.77 ≤ 950, measured 930.7 → holds |
| 1,000 ms | accepted | accepted, measured 930.7 |
| 2,000 ms | accepted (restart-2) | accepted at `λ=0` plan 2 (measured 1,402.1); at `λ=1` the optimizer (measured 2,105.9) |
| 2,500 ms | — | optimizer (bound 2,435.0, measured 2,105.9) |

So the accept/refuse boundary moved from **850 ms (wrong)** to **950 ms
(correct)**. The 850 ms SLA is refused rather than accepted-and-missed, which is
the whole point of the uncertainty term.

## Plan choices versus S13b and the byte consequence

* The objective now charges `encode_ns`, so a byte-cheap build-heavy
  representation loses when writes are weighted. The court's optimizer chooses
  654,498,531 B (predicted) / 654,600,088 B (realised) with eight dependencies
  and max depth 1.
* The chain/restart DP's old optimum was 809,012,976 B ("optimizer-bytes-only"
  row); the exact oracle prefers eight shallow deltas. Realised bytes are
  101,557 B above prediction (0.016 %) — the only plan whose byte error is
  non-trivial is the floor plan, −27,406,534 B (−1.86 %), again a phase-boundary
  effect.
* `lambda_write = lambda_read` reverses the S13b choice at `λ=0`/`λ=1` only where
  the write term is large: at `λ=0` the bytes-only objective still selects
  plan 3–7 (independent at high SLAs); at `λ=1` it selects the optimizer for
  every SLA ≥ 2,500 ms. The court records both lambdas for every SLA.
* The exact oracle is now the **default** solver for ≤ 12 versions
  (`Solver::Auto`), with the DP above that; `dp_matches_brute_force` is reported
  informationally and is false only because the DP's chain/restart space does
  not include the non-adjacent parents the oracle found.

## Costs and losses

* The uncertainty bound makes SLAs conservative: the floor bound is 949.8 ms for
  a 781.6 ms prediction (+21.5 %), and a 900 ms SLA that would probably hold is
  refused.
* The zstd-patch hop row has a 62.5 % measured spread, so a chain containing one
  buys a large bound. That is honest, not tight.
* `lambda_retention` is unpriced: no measured retention table exists, so the
  term is explicit and defaults to zero.
* Modes 17/20 and KV/activation cannot be planned at all (`UnmeasuredCost`).
* The floor prediction remains −16 % because of page-cache state; it is
  published, not tuned away.
* One host, warm page cache, single-threaded restores, 9 checkpoints.

## Reproduce

```sh
scripts/docker-court.sh bench "cd /work && bash bench/scripts/parent_graph_court.sh"
```
