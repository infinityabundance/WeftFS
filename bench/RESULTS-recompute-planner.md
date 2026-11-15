# T096 — store / fetch / reconstruct / recompute decision planner

**Status:** research gate, executed on `staging`. The planner is **not** on any
shipped default path: the contract gate it uses is the shipped
`weftfs::provenance::choose_recovery`, and the utility policies are research.
Receipt: `bench/receipts/recompute-planner-<stamp>.json` (court
`bench/scripts/recompute_planner_court.sh`, 6/6 checks).

Numbers below are emitted from the receipt by
[`scripts/perf_numbers.py`](../scripts/perf_numbers.py).

## 1. Question

Four strategies can serve one logical tensor from a real store: read the raw
object (`fetch`), read a mode-9 frame (`decode`), read a mode-11 delta and
resolve its dependency chain (`reconstruct`), or run a declared operation over
verified inputs (`recompute`). Which decision rule is worth using, and can
recompute be trusted to be exact?

## 2. Contract separation

* The **exactness gate is the shipped planner**. `choose_recovery` requires
  `Determinism::Exact` and every declared input verified before `recompute` is
  eligible; nothing here re-implements or weakens it.
* Every fixture also carries an **approximate** provenance record: the same
  inputs and output object, `determinism = declared`, and an operation that does
  *not* reproduce the target. The court fails if that candidate is ever eligible
  or selected.
* Exact and approximate paths are therefore physically separate in the candidate
  list, and the approximate one is labelled at the point of declaration.

## 3. Method

The fixture is real: twelve Qwen2.5-0.5B-Instruct tensors (64 KiB–1 MiB) written
to one store as a raw file, a mode-9 tensor object, a mode-11 delta against the
0.5B base, and two halves whose concatenation is the tensor. Provenance records
declare `concat-halves` (exact) and `concat-halves-truncated` (declared).

Sixty requests = twelve tensors × five availability masks (which strategies the
trace makes available). Every available strategy is executed and timed (median of
three runs); all outputs are compared with the source tensor. Policies are
fitted on six *other* availability masks and evaluated on the five held-out
masks.

## 4. Result

<!-- BEGIN GENERATED:t096-policies (scripts/perf_numbers.py) -->
**60** held-out requests, **312** measured strategy executions.
Oracle **52,229,224 ns**. Best fixed priority **175,908,611 ns** (regret
**2.3680×**); mean-utility **52,230,436 ns** (regret
**0.000023×**); declared costs **52,230,436 ns**; shipped planner
**95,351,512 ns** (regret **0.8256×**).
<!-- END GENERATED:t096-policies -->

<!-- BEGIN GENERATED:t096-contract (scripts/perf_numbers.py) -->
**12** exact requests; recompute was chosen on
**48** requests and produced the exact bytes on
**48** of them (**1.0000**).
The approximate candidate was refused **60** times and selected
**0** times.
<!-- END GENERATED:t096-contract -->

## 5. What the numbers say

* **A fixed priority order is the wrong rule.** The best fixed order found on the
  training traces is far worse than the oracle on held-out masks, because which
  strategy is cheapest flips with availability.
* **A single mean-cost model is enough on these traces.** Predicting each
  strategy's mean training cost and choosing the cheapest available is within a
  few thousandths of a percent of the oracle. That is a real result and also a
  limitation: these traces are separable by one mean per strategy, so the court
  provides no evidence that a context-dependent policy is needed.
* **Static declared costs are not a utility model.** The shipped declared-cost
  ordering (fetch < decode < reconstruct < recompute) is well above the oracle
  here, because the declared numbers are not measured costs of a delta-chain
  reconstruction.
* **Recompute is exact when chosen.** Every one of the recompute decisions
  produced bytes identical to the source, and the approximate candidate was
  never eligible. The gate, not the policy, is what makes that true.
* **Removing `fetch` is what makes recompute matter.** Recompute is chosen only
  on the held-out mask that withholds fetch; the other four masks never pick it,
  because reading the stored bytes is cheaper. That is the honest shape of the
  trade: recompute is an availability fallback, not a speed win, on this data.

## 6. Every loss and limitation

1. The learned policy's advantage over the trivial declared-cost rule is zero on
   this trace set (both are at the oracle), so the research adds no policy value
   here.
2. The trace set is small: twelve tensors, five held-out masks.
3. The recompute operation is a concatenation of two stored halves, not a real
   training step. It validates the contract, not the economics of recomputation.
4. Wall time includes the store's own hash verification on every read; a
   cached-read variant would change the ranking between `decode` and
   `reconstruct`.
5. The declared costs (100 / 1 000 / 3 000 / 5 000) are arbitrary and are shown
   to be a poor model; they are kept because they are the shipped planner's
   input shape.
6. One host, warm page cache, single process; no concurrent load.
7. The approximate record is *declared* approximate and its execution is never
   invoked to prove it differs — the court proves the planner refuses it, not
   that the operation is wrong.
8. No cold media, power loss, fleet, cloud or multi-node claim.

## 7. Reproduce

```
bash bench/scripts/recompute_planner_court.sh
```

A rust court measures; a fuse court assembles the receipt. Reduced knobs:
`WEFT_RP_TENSORS`, `WEFT_RP_REPS`, `WEFT_RP_MAX_BYTES`. The script prints
`recompute_planner_court: skip <reason>` and exits 78 when Docker, the images or
the corpus are unavailable.
