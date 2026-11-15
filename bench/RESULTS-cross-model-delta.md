# T051 — Cross-model lossless structural delta

Status: implemented and measured, with a negative headline. The mode does what
it says (exact residual reuse over byte-identical regions) and the measurement
shows that such reuse barely exists between independently trained models.

## What is implemented

`src/cross_model.rs` is a policy layer over the **frozen** mode-3 `DELTA`
payload: a COPY/INSERT op stream where a COPY is emitted only for bytes that
are byte-identical in the base, and every other byte is carried as an INSERT.
No wire mode, mode number or root changed. It adds:

* a policy (minimum matched fraction, maximum frame ratio) that decides whether
  a delta is worth storing at all;
* typed decline reasons (`nothing-in-common`, `frame-too-large`, `incomparable`,
  `refused`, `tensor-mode-ineligible`), so a pair with nothing in common
  produces a refusal instead of a bloated delta;
* `tensor_mode_eligibility`, which states that the shape- and dtype-exact
  per-tensor modes (mode 11 and mode 16) cannot express a cross-shape or
  cross-dtype pair — a typed refusal, never a silent byte-level fallback.

The frame is always built before the decision, so the decision rests on
measured bytes.

## Pairs

Real: pythia-70m step0 → step1 and step0 → step8 (consecutive and distant
training checkpoints, F32, identical shapes); Qwen2.5-0.5B base → Instruct
(independent fine-tune, BF16, identical shapes) as a 256 MiB slice and as a
whole-model store arm; the Qwen 0.5B BF16 embedding against the pythia-70m F32
embedding (different size, dtype and shape); two Qwen BF16 tensors of different
shapes. Synthetic: one deterministic random pair, labelled synthetic in the
receipt and in the table.

Whole-file Qwen payloads do not fit the 3 GiB court, so the fine-tune byte
delta is measured on a contiguous slice and the store arm covers the whole
model. That is stated in every cell.

## Method

Fuse court with the standard hard limits. Receipt:
`bench/receipts/cross-model-delta-20261115T064249Z.json` (+ `.log`).
Reproduce:

```
scripts/docker-court.sh fuse "cd /work && bash bench/scripts/cross_model_delta_court.sh"
```

Comparators: `zstd --patch-from -19` (falling back to `-12` and recording the
failure if level 19 is killed), independent `zstd -19` of the target, and the
shipped planner writing the target checkpoint with the base as a causal parent
(marginal retained bytes, every manifest and index included). For the distant
pair the planner arm is forced to `WeightDelta` and, separately, `SparseDelta`.

<!-- BEGIN GENERATED:t051-summary (scripts/perf_numbers.py) -->
<!-- END GENERATED:t051-summary -->

<!-- BEGIN GENERATED:t051-table (scripts/perf_numbers.py) -->
<!-- END GENERATED:t051-table -->

<!-- BEGIN GENERATED:t051-reading (scripts/perf_numbers.py) -->
<!-- END GENERATED:t051-reading -->

## Why the byte delta declines on fine-tunes

A fine-tune changes nearly every weight, and BF16 values that are numerically
close still differ in their low bytes. The encoder only copies regions that are
byte-identical, so on a fine-tune there are almost none, and the policy declines
rather than emitting an INSERT stream the size of the model. The matched
fractions above are the measurement; the decline is the correct behaviour.

The same numbers show where the value lives: on consecutive checkpoints the
byte delta is excellent, and on distant checkpoints the per-tensor
`WEIGHT_DELTA` mode (which compares element words, not bytes) is far smaller
than `zstd --patch-from`. These are different mechanisms with different
eligibility, not competing claims.

## Losses and refusals

<!-- BEGIN GENERATED:t051-losses (scripts/perf_numbers.py) -->
<!-- END GENERATED:t051-losses -->

## Non-claims

* The byte delta needs the whole base available at decode time; it is not a
  standalone artifact.
* No claim that this mode helps independent fine-tunes. It explicitly declines
  them.
* One host, warm cache; the store comparator includes metadata, the standalone
  `zstd` comparators do not have any metadata to include.
* No Go implementation of the policy layer (the mode-3 payload itself is
  implemented in both codecs).
