# T048 — LoRA / DoRA adapters as typed causal dependencies

Status: implemented and measured on real public adapters. Not an adoption
claim: no training framework, PEFT, or upstream merge tool was modified.

## What is implemented

* `weft.adapter.v1` — an adapter is its **own** artifact. It names its base by
  the base checkpoint's manifest content root and manifest root, the whole
  container's SHA-256 and its tensor count, and records every `lora_A`,
  `lora_B` and DoRA `lora_magnitude_vector` by store object id, content root,
  dtype and shape. It also records the source container's header verbatim and
  each tensor's `data_offsets`, so `restore_adapter` reproduces the original
  `adapter_model.safetensors` **byte for byte** and proves it against the
  recorded SHA-256. A header or offset set that cannot tile the data section
  exactly is refused, never guessed.
* `weft.merged-view.v1` — an optional precomputed merge with its own identity
  (`H("weft/merged-view/v1", WCB(manifest))`). It references the adapter and
  the base, covers the base tensor set exactly, stores merged tensors as new
  objects, and keeps non-target tensors referenced from the base.
  `weft.merge-contract.v1` pins accumulation dtype (`f32`), output dtype,
  round-half-to-even, operation order, the exact scaling as f64 bits, the DoRA
  norm and the clamp rule. A manifest declaring any other contract is refused.
* Adapter and merged-view manifests both carry
  `merge_equivalence: "not_claimed"`. The NumPy reference below measures the
  gap rather than asserting equality.

## Corpus

Public adapters for the pinned `Qwen/Qwen2.5-0.5B-Instruct` family (revision
`7ae557604adf67be50417f59c2c2f167def9a775`, base SHA-256
`fdf756fa…88fb7fe`), fetched file by file and verified against the LFS pointer.
Every file's URL, revision and SHA-256 is in
`bench/corpora/real-models/adapters-qwen2.5-0.5b.json`; no weights are
committed. Most are LoRA; one is DoRA. A few are ingested but **not** merged
because their own PEFT config does not name the pinned base (one declares a
4-bit quantized unsloth base, one a local path), so merging them would not be
evidence that the bf16 merge is warranted.

<!-- BEGIN GENERATED:t048-corpus-counts (scripts/perf_numbers.py) -->
<!-- END GENERATED:t048-corpus-counts -->

## Method

Bench court, `weft/fuse-court:1`, with the standard hard limits, one host,
warm page cache. Receipt:
`bench/receipts/adapters-20261115T052218Z.json` (+ `.log`, and the NumPy
reference `bench/receipts/adapters-reference-20261115T052218Z.json`).
Reproduce:

```
scripts/docker-court.sh fuse "cd /work && WEFT_ADAPTER_MERGED_FULL=1 WEFT_ADAPTER_MERGED_ZSTD=2 bash bench/scripts/adapter_court.sh"
```

Arms: adapter-only storage and restore; base plus every adapter; a precomputed
merged view per eligible adapter; a full merged checkpoint in the same store
and in a fresh store; and applying the adapter to a restored base in memory
(the arithmetic a loader pays when no view was precomputed). Comparators are
the same files as plain bytes and `zstd -19` on the same files.
`stored_physical` is the incremental reachable tree, so manifests, frames,
index segments and commit records are included.

## Results

<!-- BEGIN GENERATED:t048-results (scripts/perf_numbers.py) -->
<!-- END GENERATED:t048-results -->

Read the two merged-model rows together: the *incremental* cost of also
writing a full merged checkpoint beside a merged view is only metadata, but the
standalone cost — a second model copy in a store that holds nothing else — is
the full model size. The shared-store row is deduplication, not a storage win.

<!-- BEGIN GENERATED:t048-time (scripts/perf_numbers.py) -->
<!-- END GENERATED:t048-time -->

These are single runs on a contended court; the ordering held across runs but
the absolute values are point measurements.

## The arithmetic is not byte-equivalent, and it is measured

The NumPy reference recomputes the merge from the **original** files (BLAS
`float32` matmul, then round-half-to-even to bf16) and compares it with the
Rust bytes element by element:

<!-- BEGIN GENERATED:t048-reference (scripts/perf_numbers.py) -->
<!-- END GENERATED:t048-reference -->

Every dumped tensor behaved the same way: a handful of mismatching elements out
of millions. That is why the manifests say `merge_equivalence: not_claimed`,
and why a merged view has its own identity instead of pretending to be the base
or an upstream merge.

## Losses

<!-- BEGIN GENERATED:t048-losses (scripts/perf_numbers.py) -->
<!-- END GENERATED:t048-losses -->

## Non-claims

* No PEFT/Transformers/merge-tool integration; no upstream merge is claimed
  equal to ours.
* DoRA's clamp rule is declared as `none`. PEFT versions differ on clamping;
  this implementation pins what it does and refuses any other declared
  contract rather than guessing.
* No GPU, fleet, cloud or multi-writer claim. One host, warm cache.
* The Go store does not implement the adapter or merged-view schemas.
* The adapter files are real, but the family is small and the court is not a
  serving workload.
