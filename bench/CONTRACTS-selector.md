# T093 contract — residual-fingerprint selector

This contract is committed **before** any held-out measurement exists. Nothing
below is reworded after the result; a failed gate stays failed.

## Split

| split | families | use |
|---|---|---|
| FIT | Qwen2.5-0.5B (no parent), Qwen2.5-0.5B-Instruct (parent: 0.5B base), pythia-70m step 0 (no parent), pythia-70m step 8 (parent: step 0) | fitting the decision tree and the two trivial thresholds |
| VALIDATION | Qwen2.5-3B base (no parent) | choosing the tree depth **only**, and fitting the trivial thresholds together with FIT |
| HELD-OUT | Qwen2.5-3B-Instruct (parent: 3B base) | one measurement, after the model is frozen |

Subset rules, applied by rule rather than by inspecting errors: tensors are
sorted by **byte length** (alphabetical order is biased — it puts every
`*.bias` first), an evenly spaced stride keeps at most 40 FIT tensors per
family under 8 MiB and at most 24 evaluation tensors per family under 16 MiB.
Tensors above the caps are excluded, not truncated.

## Candidates (all exact)

`raw`, `zstd-3`, `float-tensor` (mode 9). No lossy mode is in the set, so a
selector cannot "win" by choosing an inexact representation.

## Objective

`cost = bytes + (encode_ms + decode_ms) * 1e6 * 0.001`

One millisecond of CPU is worth one kilobyte of storage. The bytes-only total is
reported as a second reading, so a reader can check that the conclusion does not
depend on the exchange rate.

## Features (one pass, no encode)

`log2_bytes`, `residual_entropy`, `residual_zero_words`,
`high_plane_entropy`, `has_parent`. "Residual" is the XOR against the causal
parent when one of the same dtype and shape exists, and the tensor itself
otherwise (`has_parent` records which).

## Policies

1. `always_zstd` — zstd-3 for everything.
2. `always_best_of_two` — measure zstd-3 and mode 9, choose the smaller total.
3. `entropy_threshold` — a fitted residual-entropy threshold choosing between
   raw and zstd-3.
4. `size_threshold` — a fitted `log2_bytes` threshold choosing between two
   measured-cost-minimising strategies on each side (fitted on FIT+VALIDATION).
5. `fingerprint_tree` — cost-sensitive decision tree, depth chosen on
   VALIDATION from {1, 2, 3}, fitted on FIT only, minimising the declared
   objective.
6. `oracle` — measures all three, takes the true minimum.

## Gates (frozen)

* **G1**: the fingerprint tree's held-out total cost is at most the **best
  trivial selector's** total cost (trivial = policies 1–4).
* **G2**: the tree's held-out regret against the oracle is at most **5 %** of
  the oracle total.
* **G3**: per-tensor planning time (fingerprint pass + decision) is at most
  **50 ms**; the fingerprint pass cost is reported separately.
* **G4**: every measured candidate decodes back to the exact input on every
  tensor of every split; a single mismatch fails the court.
* **G5**: no policy's held-out result is reported from a second run. The
  held-out split is measured once; a rerun requires a new receipt and is
  labelled as a rerun.

A null result — the tree failing G1 or G2 — is a successful gate outcome and is
published as such, with the same prominence as a win.
