# P01 — one composable Training Artifact, Rust and Go

**Date:** 2026-11-15 (refreshed 2026-11-15T153500Z). **Court:**
`scripts/training_artifact_court.sh` (one hard-limited fuse container, Rust and
Go). **Receipt:** `bench/receipts/training-artifact-20261115T153500Z.json` and
`.log`, at WeftFS `10fde89`, WeftFS-Go `21c41b1`, WeftCodec `98019d8`,
WeftCodec-Go `d45fcb4`. **Normative:** `spec/WEFT-TRAINING-ARTIFACT.md`.
**Retained failures:** `training-artifact-20261115T132004Z.json` (empty
`record_bytes`), `…144500Z.json` (Go refused the certifier's canonical-roots
rule), `…152000Z.json` (Go refusal bundle carried no records).

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
| Court checks | 24 / 24 pass |
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

### 2.1 Two defects the independent checker found

Both were found by running `verifier/closure_check.py`, which knows nothing
about either implementation, against bundles produced by the independent Go
consumer. Both are now fixed and covered by the court.

1. **The bundle echoed an unnormalised request set.** Both stores sort and
   dedupe the root list internally, but the Go bundle printed the caller's list,
   so its `roots` were not ascending and the verifier refused the entire bundle
   (`NonCanonical: roots is not strictly ascending`) before recomputing
   anything. The request set is now derived by one store function per language,
   and `complete`/`minimal` also differ correctly there (`complete` includes the
   artifact's declared transitive `closure` as roots, `minimal` does not).
   Spec §5 now states the rule.
2. **A refusal bundle could not be recomputed.** The verifier walks from the
   bundle's `roots`; neither producer emitted a record for a root whose object
   declares no causal attributes, so the verifier failed on the root rather than
   on the identity the producer named. Both producers now emit a record for
   every root that resolves (the canonical empty `weft.causal.v1` record when
   the node declares no ancestry) and leave an unresolvable root absent so the
   walk hits it. The Go bundle previously carried no records at all on a
   refusal.

`tests/training_artifact.rs::refusal_bundle_carries_a_record_for_every_request_root`
and `WeftFS-Go/store/training_artifact_test.go::TestClosureRootsAreCanonicalAndModeSeparated`
hold the two rules directly.

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
* **Evidence schema is court-local.** `weft.closure.bundle.v1` is not a store
  format; it is what this court and the clean-room verifier agree on. A second
  independent producer must implement the same bundle shape to be checkable, and
  two of the rules it needs were only learned by failing against it (§2.1).
* **The Go refusal bundle is checked for consistency, not compared field by
  field with Rust's.** The court asserts the verifier reaches the same class and
  identity; it does not diff record bytes.

## 5. Non-claims

No fleet, cloud, media, power-loss or GPU claim. The fixture is a synthetic
store with one checkpoint tensor and two causal notes; the court is one host with a
warm page cache. The record is not a training guarantee. Equal artifact roots
across two stores mean equal immutable member identities and declarations, not
equal commit histories.
