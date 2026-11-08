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
        + lambda_read_ns * reads_per_version * sum_i chain_decode_ns(i)
chain_decode_ns(i) = own_decode_ns(i, parent(i))
                   + (parent(i) = none ? 0 : chain_decode_ns(parent(i)))
```

`edge_bytes` and `own_decode_ns` are measured, not invented: the store calls
the shipped `Store::plan_tensor` with the same candidate set the writer uses
(with and without a causal parent version), so the byte price is the frame the
writer would publish and the decode price is the existing
`src/cost_model.rs` row. Delta rows in the cost table are incremental, which is
what makes the recursion add up to a chain cost.

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
  (`max_i chain_decode_ns(i)`). If no plan in the solver's space satisfies it,
  the solver returns `LimitExceeded` naming the best achievable value. The
  independent representation is the minimum-decode representation for every
  version, so if the all-independent plan fails the SLA nothing can pass.
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
edge bytes and its own decode cost, and the chain is summed from the measured
rows. The court measures the realised store bytes and restore medians next to
the model and publishes both, including the prediction error.

The CLI `weftfs plan-parents` writes the independent probes into a private
`--scratch` store, prices the graph, prints the plan, and with `--apply`
writes the chosen graph into a new target store in version order. The probe
store is removed unless `--keep-scratch` is set.

## What this is not

* Not a multi-writer protocol and not a change to the on-disk format.
* Not a Go implementation (see `docs/STATUS.md` S13).
* Not a claim that restarts pay for every workload. The court publishes a
  break-even read count per plan; if reads are rare, the chain wins.
