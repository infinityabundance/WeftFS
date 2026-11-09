# Parent-graph planning (S13)

A trajectory is an ordered sequence of versions. Each version is stored either
independently or as a delta against an **earlier** version. That choice is the
whole trade: a delta is smaller on disk, but a cold restore has to decode every
frame on the version's chain.

Before S13 the parent was always the previous version, so a 9-checkpoint
pythia-70m trajectory reached chain depth 8 and cold restore grew from 683 ms
to 9 521 ms. The trade was never chosen; it happened.

S13 adds a write-time planner. It does **not** add a wire field: the decision
surface is the existing `TensorWriteSpec.causal` / `CheckpointWriteOptions::
causal_checkpoint` path, plus the existing independent representation for a
restart point. A plan names, for every version, either an earlier version or
`none`.

## Objective

```
F(plan) = sum_i edge_bytes(i, parent(i))
        + lambda_read_ns * reads_per_version * sum_i restore_ns(i)
restore_ns(i) = decode_chain_ns(i) + restore_overhead_ns(i)
decode_chain_ns(i) = own_decode_ns(i, parent(i))
                   + (parent(i) = none ? 0 : decode_chain_ns(parent(i)))
restore_overhead_ns(i) = per_version_ns
                       + per_tensor_ns * tensors(i)
                       + per_mib_ns * logical_mib(i)
```

`edge_bytes` and `own_decode_ns` are measured, not invented: the store calls
the shipped `Store::plan_tensor` with the same candidate set the writer uses
(with and without a causal parent version), so the byte price is the frame the
writer would publish and the decode price is the existing
`src/cost_model.rs` row. Delta rows in the cost table are incremental, which is
what makes the recursion add up to a chain cost.

`restore_overhead_ns` is the **S13b correction**. The affine decode rows are
fitted on single-object `Store::read_tensor` calls; a checkpoint restore also
loads the checkpoint manifest and re-verifies every shard content root, shard
logical root, reassembled content root and reassembled logical root, hashes each
tensor for the report, and hashes the assembled container. That work is
proportional to the version's logical bytes and carries a small per-tensor
cost. The baked term is measured by `examples/calibrate_restore.rs` on real
`Store::restore_checkpoint` calls (`RESTORE_OVERHEAD_CALIBRATION =
"weftfs-s13b/restore-overhead/v1"`, receipt
`bench/receipts/s13b-restore-floor-20261108T230417Z.json`):

| term | measured value | evidence |
|---|---:|---|
| `per_version_ns` | 13,285 | manifest read of a one-tensor checkpoint |
| `per_tensor_ns` | 29,205 | count sweep, 4 KiB–1 MiB tensors |
| `per_mib_ns` | 2,114,993 | size sweep, 1–64 MiB tensors |

It is charged **once per version restore**, never once per dependency hop. The
first S13b draft added it to the recursive chain and would have over-predicted
a depth-8 chain by an extra 8 × 0.6 s; the `restore_overhead_is_charged_once_
per_version` test pins the distinction.

The S13b measurements refuted the per-object hypothesis in the range that
matters for model checkpoints: at a fixed 64 MiB total, one 64 MiB tensor and
sixty-four 1 MiB tensors restore within 7 % of each other (136.2 ms vs
127.0 ms of unexplained residue), so there is no significant per-object term
above ~256 KiB. The measured per-tensor term is the small-object fixed work
(roughly 30–40 µs per tensor) that appears below that size.

`lambda_read_ns` is the existing planner exchange rate (bytes charged per
nanosecond of decode). `reads_per_version` is the expected number of reads of
each version over its lifetime; each read pays the whole chain. Both are
explicit arguments; the court publishes the break-even lambda instead of
tuning one.

## Solvers and state spaces

| solver | parent space | state space | exactness |
|---|---|---|---|
| `dp` | `none` or the immediately previous version | `(version, depth)` | exact over that space, `O(n * max_depth)` |
| `brute-force` | `none` or **any** earlier compatible version | the whole assignment | exact, `O(n!)`, refused above 12 versions |
| `greedy` | `none` or any earlier compatible version | one pass, bounded | not claimed optimal |

The chain/restart restriction is not cosmetic. In a sequence where every
version is the same architecture, the adjacent delta is the smallest, so a
non-adjacent parent changes only the byte term and is dominated. The
brute-force oracle checks this on the measured graph; when it finds a better
non-adjacent edge the court reports the DP's loss rather than hiding it.

The exhaustive search is the only exact general solver. `n!` grows fast, so the
default limit is 12 versions and the limit is a typed `LimitExceeded`, never a
silent fallback to a heuristic.

## Constraints

* `max_depth` (default 8). The frozen reader bound is
  `MAX_CODEC_DEPENDENCY_DEPTH = 8`; a plan can never exceed it.
* `sla_ns` bounds the worst-case cold restore of **any** version
  (`max_i restore_ns(i)`). The solvers prune on `restore_ns(i)`, not on the raw
  decode chain, and `check_constraints` refuses a plan whose corrected restore
  exceeds the SLA with a typed `LimitExceeded`. If no plan in the solver's space
  satisfies it, the error names the best achievable value. The independent
  representation is the minimum-decode representation for every version, so if
  the all-independent plan fails the SLA nothing can pass.
* An SLA claim with **no** restore-overhead model (`PlanOptions::
  restore_overhead = None`) is refused with a typed `InvalidArgument`: the
  prediction would omit known work, and treating a missing term as zero is the
  S13 defect. With no SLA, `None` is allowed and the plan reports that its
  prediction has no overhead model.
* `storage_budget` bounds `sum_i edge_bytes(i)`. The DP optimises the objective
  and then falls back to the exhaustive budget-pruned search when the optimum
  is over budget; it never returns an over-budget plan.

## Determinism and tie-break

Every solver is deterministic: candidates are visited in version order, there
is no randomness, and complete plans are ordered by

```
(objective, fewer dependencies, lower parent index)
```

where the parent index is compared lexicographically in version order and
`none` sorts before every index. The same graph and options therefore produce
the same plan byte for byte.

## Measurement boundary

`Store::price_parent_graph` requires the versions to be present as **complete,
single-shard, `planned`** checkpoints. It calls `restore_named_tensor_at` for
the child bytes and `plan_tensor` with/without a causal version. This is the
same decision the writer makes, but it is a *model*: it charges a version's
edge bytes, its own decode rows, and the measured restore overhead above. The
court measures the realised store bytes and restore medians next to the model
and publishes both, including the prediction error.

The model has two measured boundaries that the S13b court exposes rather than
hides:

1. **Warm page cache.** The overhead term is measured with the working set in
   the OS page cache. When a nine-version, 1.5 GB all-independent store is
   restored inside the 3 GiB court, versions 0–3 stay warm (796–807 ms against
   a 781.6 ms prediction) and versions 4–8 fall out of the cgroup's page cache
   (878–886 ms), so the measured worst is 11.8 % above the prediction. Every
   later cold restore costs roughly 100 ms per 164 MB read. In a fresh
   calibration store the same nine checkpoints restore at 778–793 ms
   (−1.5 %..+0.4 % error). This is a cache-capacity boundary, not a fitted
   constant, and it is not compensated by raising the constant.
2. **Per-candidate pricing is unchanged in S13b.** The graph prices a
   level-19 zstd frame with the same `zstd` row as a level-3 frame, and
   sparse-delta / zstd-patch with the weight-delta row. The measured per-hop
   costs are lower than those rows for the pythia deltas (sparse-delta by
   ~60 %, weight-delta by 12–21 %), so chain and restart predictions are
   **conservative** (chain +31.7 %, restart-3/4 +12.2 %, restart-2 +7.6 %).
   Correcting the candidate rows could flip stored strategies across corpora
   and is a separate decision; it is explicitly out of scope here.

The CLI `weftfs plan-parents` writes the independent probes into a private
`--scratch` store, prices the graph, prints the plan, and with `--apply`
writes the chosen graph into a new target store in version order. The probe
store is removed unless `--keep-scratch` is set. `--restore-overhead none` is a
diagnostic: an SLA with it is refused.

## What this is not

* Not a multi-writer protocol and not a change to the on-disk format.
* Not a Go implementation (see `docs/STATUS.md` S13 and S13b). WeftFS-Go has no
  parent-graph planner, so the restore-overhead constant is documented in the
  Rust cost model only; no cross-language agreement is claimed.
* Not a change to per-candidate strategy pricing (S13b). The decode rows and
  the strategy-to-class mapping are untouched; the measured restore overhead
  is an additive planning term.
* Not a promise that a warm-cache prediction holds for cold media. The court
  quantifies the cache-capacity boundary in "Measurement boundary" above.
* Not a claim that restarts pay for every workload. The court publishes a
  break-even read count per plan; if reads are rare, the chain wins.
