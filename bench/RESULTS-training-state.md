# T049 — Optimizer state, gradients and attention masks

Status: implemented with fixtures, replay tests and a real backward pass. RL
trajectories remain **specified only** (see below).

## What is implemented

* `weft.optimizer-state.v1` — AdamW `exp_avg`, `exp_avg_sq` and `step` per
  parameter, with every hyperparameter stored as exact f64 bits, bound to the
  model checkpoint's manifest root, manifest content and container SHA-256.
  The moments carry their **own dtype** (`moment_dtype`), because real AdamW
  state for a bf16 model is f32; the earlier fixture-only path could store
  moments only in the parameter's dtype.
* `weft.gradient.v1` — one gradient tensor per parameter for a declared step,
  loss (exact f64 bits), token count, sequence length, loss kind and global
  norm, bound to the same model reference. `grad_dtype` is explicit.
* `weft.attention-mask.v1` — one `u8` mask tensor with its kind
  (`causal|padding|hybrid|custom`), shape, sequence length, padded-position
  count and the exact interpretation string reproduced verbatim.
* Restore is exact byte-for-byte for all three, through the verified tensor
  path. The artifacts are bound to the model by content root; a changed model
  is a typed `DependencyMismatch`, not a silent success.

## Method

The fixture is not a random byte string: `bench/scripts/training_state_court.py`
builds the deterministic transformer the training-goodput court uses, runs a
real forward pass, cross-entropy, `backward()` and one `AdamW.step()`, then
dumps bf16 parameters, f32 moments, bf16 gradients and a real `u8`
causal-with-padding mask. The Rust harness
(`examples/training_state_court.rs`) ingests them through the shipped API,
restores them, and compares every restored payload with the torch tensor.
`zstd -19` baselines come from libzstd through the `zstandard` module (the
Python court has no `zstd` CLI). WeftFS-Go `verify --full` reads the same store.

Receipt: `bench/receipts/training-state-20261115T054734Z.json` (+ `.log`).
Reproduce:

```
bash bench/scripts/training_state_court.sh
```

<!-- BEGIN GENERATED:t049-shape (scripts/perf_numbers.py) -->
<!-- END GENERATED:t049-shape -->

## Results

<!-- BEGIN GENERATED:t049-results (scripts/perf_numbers.py) -->
<!-- END GENERATED:t049-results -->

<!-- BEGIN GENERATED:t049-go (scripts/perf_numbers.py) -->
<!-- END GENERATED:t049-go -->

## Losses

<!-- BEGIN GENERATED:t049-losses (scripts/perf_numbers.py) -->
<!-- END GENERATED:t049-losses -->

The optimizer-state result is deliberately unglamorous: AdamW moments are
noise-like and do not compress dramatically. The honest summary is "mildly
compressible, slightly better than `zstd -19` on the same bytes, and exact".

## Exactness and parity

* Every restored moment, gradient and mask payload was compared with the torch
  tensor it came from; the receipt records zero mismatches.
* `verify --full` in Rust and in Go both pass over the same store, so the codec
  frames these artifacts use are decoded identically by both implementations.
  The artifact **manifests** are Rust-only schemas; Go verifies the store, not
  the T049 descriptors.
* The receipt records the step counter, loss bits and gradient global norm, so
  the state can be checked against the training trace, not just against itself.

## RL trajectories: specified only

No `weft.rl-trajectory.v1` descriptor was invented. There is no consumer in
this repository yet, and a format without a replay test and a real producer
would be noise. That remains true; `spec/WEFT-TRAINING.md` describes the
intended shape but nothing is advertised as implemented.

## Non-claims

* CPU only, single host, one thread; the fixture model is small (stated in the
  receipt) and is not a production-scale training job.
* No multi-rank or distributed checkpoint claim.
* No claim that the store reduces optimizer-state bytes dramatically.
