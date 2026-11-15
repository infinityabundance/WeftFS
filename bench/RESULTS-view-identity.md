# T052 — Exact versus approximate view identity

Status: implemented with one working view (`q8_0-block`), a canonical
descriptor, an explicit error contract, and measured storage/time trade-offs.

The canonical key set, identity domains and refusal rules are in
[`docs/CONTEXT-ARTIFACTS.md`](../docs/CONTEXT-ARTIFACTS.md).

## What is implemented

* `weft.view.v1` — a view descriptor with its own identity
  (`ViewId = H("weft/view/v1", WCB(descriptor))`), the exact source's object
  id, content root and logical root, the reconstructed-values root, the stored
  `codes` and `scales` object ids and roots, the element count, the decoded
  dtype and the error-bound statement **verbatim**.
* The method implemented is `q8_0-block`: `f32` per block of `block_size`
  elements (`scale = amax/127`), `i8` codes, round-half-to-even, and a final
  narrowing of each reconstructed value back to the source dtype. No clamp is
  reachable (`|x| <= 127*scale`), and the encoder counts a clip if one ever
  occurs.
* The error contract is an absolute per-element bound:

  `|x - x_hat| <= scale_block/2 + half_ulp(dtype, q*scale_block)`

  where the second term is the exact half-spacing of the source dtype at the
  reconstructed value (`bf16` `2^(E-8)`, `f16` `2^(E-11)`, fixed subnormal
  spacings). The measured bound is reported and asserted.
* The exact artifact is untouched: the view is written from a read of it, its
  content root and logical root are re-checked afterwards, and it still
  verifies. `Store::resolve_exact_object` refuses a `weft.view.v1` descriptor
  with a typed `UnsupportedRepresentation` naming the view and the exact source
  a caller must ask for instead.
* `resolve_view` is the strict door: it re-reads the exact source and
  re-measures the bound on every call, refusing if the contract is violated.
  `resolve_view_bytes` verifies the member roots and the reconstruction root
  but does not re-read the exact source; both are documented and tested to
  return identical bytes and to refuse a corrupted code byte.

## Method

Fuse court with the standard hard limits, real public tensors: the Qwen2.5-0.5B
`down_proj` matrix and the tied embedding, both BF16, and a pythia-70m F32
attention matrix, at block sizes 32/64/128. Receipt:
`bench/receipts/view-identity-20261115T070334Z.json` (+ `.log`). Reproduce:

```
scripts/docker-court.sh fuse "cd /work && bash bench/scripts/view_identity_court.sh"
```

`exact stored` is the planner's exact object (mode 9 or similar), not raw
bytes; `view stored` is the incremental reachable tree including the codes,
scales and descriptor. Read times are one point measurement per cell on a warm
page cache, not percentiles.

## Results

<!-- BEGIN GENERATED:t052-table (scripts/perf_numbers.py) -->
| Tensor | Exact stored | View block 32 | View block 128 | Exact read / identity-only / strict (ms) |
|---|---:|---:|---:|---:|
| Qwen `down_proj` (bf16) | 5,798,764 | 4,463,546 (0.770×) | 4,152,976 (0.716×) | 21.41 / 44.19 / 149.11 |
| Qwen embedding (bf16) | 180,547,947 | — | 127,794,475 (0.708×) | 700.60 / 1,374.21 / — |
| pythia QKV (f32) | 1,829,113 | — | 762,020 (0.417×) | 4.78 / 8.11 / — |
<!-- END GENERATED:t052-table -->

<!-- BEGIN GENERATED:t052-error (scripts/perf_numbers.py) -->
| Tensor | Measured max abs error | Declared bound |
|---|---:|---:|
| Qwen `down_proj` | 0.001953125 | 0.002176120 |
| Qwen embedding | 0.000751495 | 0.001280296 |
| pythia QKV | 0.000571424 | 0.000571904 |
<!-- END GENERATED:t052-error -->

<!-- BEGIN GENERATED:t052-verdict (scripts/perf_numbers.py) -->
* Bytes: the view wins — **28.38** % fewer stored bytes on the
  Qwen MLP matrix, **29.22** % on the embedding and
  **58.34** % on the f32 pythia matrix (block 128).
* Read time: the view loses — **2.06×** the exact read with
  identity checking only, and **6.96×** when the strict door
  re-reads the source and re-measures the bound. The embedding shows the same
  shape (**1.96×**) and so does the f32 tensor
  (**1.70×**).
* Verdict: a q8_0 view is a **bandwidth and capacity** representation, not a
  latency representation. Larger blocks trade a little more error for fewer
  bytes; the error stays inside the declared bound in every cell.
<!-- END GENERATED:t052-verdict -->

## Refusal proof

The view court asks the exact door for the descriptor object itself and records
the typed refusal (`exact_door_refused`, class `UnsupportedRepresentation`);
the unit tests prove the same thing plus the reverse: asking the exact door for
the exact object still returns the exact bytes, and the exact object's content
root and logical root are unchanged after the view is written. A descriptor
that claims `exact: true` is rejected as malformed.

## Losses

* Read latency: even identity-only resolution is slower than the exact read
  (see the verdict block). The strict door is much slower because it re-reads
  the exact source and rescans every element to re-assert the bound.
* The view is genuinely lossy; `resolved_bytes_equal_exact` is false in every
  cell and is asserted to be false, so the court cannot pass by accident on an
  exact fixture.
* Blocks beyond the tested sizes are untested; the error grows with block size
  while the byte saving grows slowly, so larger blocks are not obviously good.

## Non-claims

* One method only (`q8_0-block`). No claim about activation or KV views here.
* No GPU, fleet or serving claim; no claim that a view is a substitute for the
  exact artifact. The exact root remains authoritative.
* The manifest/descriptor schemas are Rust-only; Go reads the underlying exact
  tensor objects but has no view resolver.
