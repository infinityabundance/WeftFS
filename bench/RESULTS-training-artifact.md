# P01 — one composable Training Artifact, Rust and Go

**Date:** 2026-11-15. **Court:** `scripts/training_artifact_court.sh` (one
hard-limited fuse container, Rust and Go). **Receipt:**
`bench/receipts/training-artifact-20261115T141554Z.json` and `.log`.
**Normative:** `spec/WEFT-TRAINING-ARTIFACT.md`.

## 1. What was proved

One canonical record composes identities that already existed: exact
`LogicalTensorRoot`s and whole-tensor content roots, a `weft.checkpoint.v1`
manifest, a `ContextRootId`, a `weft.causal.v1` closure and
`weft.provenance.v1` receipts. Rust writes it; an independent Go consumer
(`WeftFS-Go/store/training_artifact.go`, no Rust calls, no shared code) reads
the same store, decodes every member tensor, parses the checkpoint manifest,
recomputes every root, checks the Context Root, walks the causal closure and
checks the declared policy. The reverse direction is also exercised: Go writes
a tensor-set artifact and the Rust CLI validates it.

The three identity classes remain separate. `TrainingArtifactRoot` is derived
from the immutable record only. A grant edit moves the grant epoch and leaves
the artifact root unchanged (checked in both languages); the artifact record
contains no snapshot, path, principal, grant, retention or quarantine field.

## 2. Cross-resolution

<!-- BEGIN GENERATED:ta-proof (scripts/perf_numbers.py) -->
| Rust wrote, Go validated | 1 tensor, 1 shard, 2 causal nodes, 1 provenance receipt |
| Exact tensors recomputed by Go | 1 |
| Artifact record | 1,071 B |
| Go wrote (tensor set), Rust validated | 2 tensors, 48 logical bytes |
| Court checks | 23 / 23 pass |
<!-- END GENERATED:ta-proof -->

The negative cases are produced by the Rust fixture harness (a real store
mutation or a re-encoded artifact record), then validated by both consumers:

| Case | Rust class | Go class |
|---|---|---|
| mutated tensor value (replacement version at the head) | `Corrupt` | `Corrupt` |
| lineage declaration removed while the policy still claims one | `Malformed` | `Malformed` |
| policy `allowed_dtypes` contradicted | `Corrupt` | `Corrupt` |
| declared ancestor absent | `DependencyMismatch` | `DependencyMismatch` |
| grant edited | root unchanged | root unchanged |
| record bytes tampered at the same path | `Corrupt`/`NonCanonical` | — |

The clean-room verifier also re-checks a *refusal* bundle: from canonical
records alone it recomputes the walk, hits the same unresolvable identity and
matches the class and the named identity. That is a P02 finding (below).

## 3. What Go cannot check, and why

These are reported rather than skipped:

* **Provenance record semantics.** Go verifies that each declared receipt blob
  hashes to its id under `weft/provenance/v1`, and that each member shard object
  carries a `provenance` relation naming a declared receipt. It does not decode
  every provenance field (operation, inputs, environment, determinism) and
  re-run the provenance module's own rules. Rust does; the Go receipt is a
  partial check, not a full provenance validation.
* **Grant signatures.** Neither implementation verifies an Ed25519 grant
  signature in this court; the grant is a mutable authority record whose
  structure, actions and expiry are checked. A grant without a configured trust
  set is not an authorization claim.
* **Historical versions.** Both validate at one pinned snapshot. Neither proves
  that a retired version was never served; that is a store-history question.
* **`source_header` / `source_file_bytes`.** The artifact declares the
  checkpoint's tensor set and source SHA-256; it does not copy the container
  header, so neither consumer re-checks header bytes against the source file.
* **Context Root selector re-evaluation.** Both check that the recorded
  `ContextRootId` is present at the snapshot and that its selector id is part of
  the record. Re-running the selector is a separate resolution, not part of
  artifact validation.

## 4. Losses

* **Record overhead.** The record size in the table above duplicates the
  checkpoint manifest's tensor and shard fields. The duplication is what
  makes the artifact self-contained and independently checkable; it grows
  linearly with tensor and shard count.
* **Go has no checkpoint writer.** The Go side builds a tensor-set artifact
  from objects it wrote. A checkpoint-backed artifact is Rust-written; the Go
  consumer validates it but cannot author one.
* **Two manifest parsers.** Rust and Go each parse `weft.checkpoint.v1`
  independently. That is the point of the proof and also a maintenance cost:
  two strict parsers must track the same frozen key set.
* **Whole-tensor decode.** Both consumers decode every member tensor to
  recompute its content root. There is no incremental/partial artifact
  validation in this court.
* **No signature.** `TrainingArtifactRoot` is an unsigned content address.
  Authenticity of the writer is a separate surface.

## 5. Non-claims

No fleet, cloud, media, power-loss or GPU claim. The fixture is a synthetic
store with one checkpoint tensor and two causal notes; the court is one host with a
warm page cache. The record is not a training guarantee. Equal artifact roots
across two stores mean equal immutable member identities and declarations, not
equal commit histories.
