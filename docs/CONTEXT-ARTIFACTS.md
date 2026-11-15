# Context artifacts: adapters, merged views, training state and views

This document specifies the canonical WCB schemas added for T048, T049 and
T052. None of them changes a frozen wire byte, mode number or root: they are
new versioned descriptors whose member tensors are ordinary store objects
written through the existing planner, and whose manifests are ordinary files in
the store's namespace.

These schemas are **not yet exported into `spec/` and the subprojects**. They
have fixtures and replay tests in this repository (Rust only); the export is a
follow-up, and until it happens no other implementation is claimed to read
them.

## Identity rules

| Artifact | Domain | Identity |
|---|---|---|
| Adapter | `weft/adapter/v1` | `H(domain, WCB(adapter manifest))` |
| Merged view | `weft/merged-view/v1` | `H(domain, WCB(merged-view manifest))` |
| Optimizer state | `weft/optimizer-state/v1` | `H(domain, WCB(manifest))` |
| Gradients | `weft/gradient/v1` | `H(domain, WCB(manifest))` |
| Attention mask | `weft/attention-mask/v1` | `H(domain, WCB(manifest))` |
| View | `weft/view/v1` | `H(domain, WCB(descriptor))` |
| Reconstructed view values | `weft/view/values/v1` | `H(domain, reconstructed bytes)` |

## `weft.adapter.v1`

Exact key set (checked; unknown or missing keys are `Malformed`):

`schema, name, kind, rank, alpha_bits, scaling_bits, use_rslora, bias,
merge_equivalence, target_modules, source, source_container, base, targets`

* `kind` is `lora` or `dora`. `alpha_bits`/`scaling_bits` are f64 bit patterns.
  `scaling = alpha/r`, or `alpha/sqrt(r)` when `use_rslora`.
* `merge_equivalence` must be `not_claimed`.
* `target_modules` is sorted and unique; every declared module must have at
  least one target tensor.
* `source` is `{repo, revision, file, url, file_sha256, config_sha256}`.
* `source_container` is `{header, file_bytes}` or null. `header` is the exact
  `u64le(header_len) || JSON header` bytes of the source container; the
  recorded length must match.
* `base` is `{checkpoint, manifest_content, manifest_root, container_sha256,
  tensor_count, element_bytes}`.
* `targets` is sorted by `base_tensor` and unique. Each target is
  `{base_tensor, base_content, a, b, magnitude, source_offsets}` where a matrix
  is `{object, content, dtype, shape}` and `source_offsets` is
  `{a: {begin,end}, b: {begin,end}, magnitude: {begin,end}|null}` (or null).
  Offsets must span exactly the tensor's logical bytes.
* A `lora` target must not carry a magnitude; a `dora` target must carry one.
* `restore_adapter` reproduces the source container only when the recorded
  ranges tile the data section exactly and the result hashes to
  `source.file_sha256`; otherwise it fails closed with `Corrupt`.

## `weft.merge-contract.v1`

`schema, dtype_accum, dtype_output, rounding, order, scaling_bits, dora_norm,
clamp`. The implementation performs exactly one arithmetic: `f32` accumulate,
`round-half-to-even`, `delta_then_add` (LoRA) or
`delta_then_add_then_row_normalize` with `l2_over_input_dim` (DoRA), no clamp.
Any other declared contract is `UnsupportedRepresentation`.

## `weft.merged-view.v1`

`schema, name, adapter, adapter_manifest_content, base, contract,
merge_equivalence, tensors`.

* `merge_equivalence` must be `not_claimed`.
* `tensors` is sorted by name and unique, and its name set must equal the base
  checkpoint's tensor set.
* Each entry is `{name, merged, content, object, dtype, shape}`; `object` is
  present exactly when `merged` is true, and `dtype` equals the contract's
  output dtype.
* Verification re-resolves the adapter id, the base manifest root and content,
  every base-referenced tensor's content root, and every merged object's
  content root. Ties are not invented: any mismatch is
  `DependencyMismatch` or `Corrupt`.

## `weft.optimizer-state.v1`

`schema, name, model, optimizer, hyper, params`.

* `optimizer` is `adamw` (anything else is `UnsupportedRepresentation`).
* `model` is the same `base` structure as an adapter's.
* `hyper` is `{lr_bits, beta1_bits, beta2_bits, eps_bits, weight_decay_bits}`,
  all finite f64 bit patterns.
* `params` is sorted and unique; each is `{name, base_content, exp_avg,
  exp_avg_sq, step}` with `step >= 1` and both moments sharing dtype and shape.
  The moment dtype is recorded in each `TensorRef`, so f32 moments for a bf16
  model are exact and explicit rather than inferred.

## `weft.gradient.v1`

`schema, name, model, step, loss_kind, loss_bits, tokens, seq_len,
global_norm_bits, params`. `params` is sorted and unique; each is
`{name, base_content, grad}`. `loss_bits` and `global_norm_bits` are exact f64
bit patterns; `loss` must be finite.

## `weft.attention-mask.v1`

`schema, name, kind, dtype, shape, seq_len, pad_positions, mask,
interpretation`.

* `dtype` is `u8` (only implemented encoding).
* `kind` is `causal|padding|hybrid|custom`.
* `shape` must equal the stored tensor's shape; element count must be non-zero;
  `pad_positions <= seq_len`.
* `interpretation` is the verbatim statement of what a `1` and a `0` mean.

## `weft.view.v1`

`schema, view_kind, exact, name, method, block_size, source_object,
source_content, source_logical_root, source_dtype, source_shape,
approximate_root, codes, codes_content, codes_shape, scales, scales_content,
scales_shape, element_count, decoded_dtype, error_bound`.

* `view_kind` is `quantized_view`; `exact` must be `false` (a descriptor that
  claims exactness is `Malformed`).
* `method` is `q8_0-block`; `block_size` must divide the element count.
* `approximate_root` must differ from both exact source roots.
* `error_bound` states: `|x - x_hat| <= scale_block/2 +
  half_ulp(dtype, q*scale_block)`, with `half_ulp` the exact half-spacing
  (bf16 `2^(E-8)`, f16 `2^(E-11)`, fixed subnormal spacings).
* `resolve_view` re-reads the exact source and re-measures the bound;
  `resolve_view_bytes` checks the member roots and `approximate_root` only.
  `resolve_exact_object` refuses a view descriptor with
  `UnsupportedRepresentation`.
