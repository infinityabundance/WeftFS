# Findings: rules the causal-closure verifier had to learn

**Date:** 2026-11-15. **Context:** extending `verifier/` to check a closure from
canonical `weft.causal.v1` records alone (P02). Each item below is a rule that
is **not stated** in `spec/WEFT-CAUSAL.md` or `spec/WEFT-TRAINING-ARTIFACT.md`
and had to be read out of `src/causal_closure.rs`, `src/training_artifact.rs` or
the Go implementation. They are recorded as findings, not silently assumed.

1. **`minimal` has no normative root set.** The spec defines a "minimal
   sufficient closure" informally (omit ancestors that are not required) but
   does not say which identities are roots. The implementation uses the
   artifact's own `causal.parents` plus the member object versions; it excludes
   provenance receipt ids, because a receipt is not a graph node, and excludes
   member objects' unrelated causal ancestry. A second implementation could
   reasonably choose differently and produce a different "minimal" set.

2. **Depth is edge hops, and a root is depth 0.** Neither the depth unit nor the
   root convention is stated. `max_depth = 64` therefore means at most 64 edges
   from a requested root, not 64 nodes and not 64 levels of nesting.

3. **Node order is unspecified.** The implementation returns nodes strictly
   ascending by identity bytes and reports depth as metadata. The verifier now
   requires that order; a producer that returned BFS order would be rejected.

4. **`closure` entries are not necessarily graph nodes.** `WEFT-CAUSAL.md` §1.2
   says a closure is a claim whose truth is checked at resolution, and §3.2
   allows a `CausalClosureId` to be stored and referenced. The graph is indexed
   by `VersionId`; a closure entry that names an `ObjectId`, `ContentId` or a
   `CausalClosureId` has no outgoing edges and is refused as unresolvable. That
   is stricter than the identity family as written.

5. **A rewrite retires the old `VersionId`.** A Context Root that pins a
   `VersionId` in its causal array becomes unresolvable after the object is
   rewritten, even if the new bytes are equivalent. This was found by the P03
   invalidation test; retargeting the root to the new snapshot requires the new
   version id. The spec's "every declared ancestor must resolve" rule does not
   warn about this ordinary operation.

6. **Unsorted parents are a construction error, not a decode convenience.** The
   spec says arrays are canonical, but a producer that naturally collects
   parents in graph order (for example a join over branch heads) must sort them
   before encoding. The store refused an unsorted join in the benchmark
   fixture. The verifier enforces the same rule.

7. **Non-disclosure has no class name.** The spec says a denied ancestor must be
   indistinguishable from an absent one, but not which error class to use. The
   implementation uses `DependencyMismatch` with identical text for both; the
   verifier checks that a refusal names the missing identity and does not
   distinguish absence from denial.

8. **A node with no ancestry needs an empty record.** The verifier's bundle must
   carry a canonical `weft.causal.v1` record for every reachable node, including
   objects that declare no causal attributes: the record is
   `{schema, parents: [], closure: []}`, not an absent record. This was learned
   from the first bundle, where empty `record_bytes` caused a WCB
   "truncated at byte 0" refusal.

9. **Provenance ids are not closure nodes.** The artifact's `provenance` array
   is a set of receipt identities. Including it in the closure root set made the
   first implementation refuse its own artifact (the receipt is not a store
   object). The rule now is explicit in both languages.

10. **A refusal must itself be checkable.** The verifier is only useful if a
    refusal bundle carries enough canonical records to recompute the refusal.
    The producer emits every causal record in the snapshot on a refusal; the
    verifier then hits the same missing identity. This is an evidence rule that
    no specification states.

11. **The request set in a bundle must be the canonical set actually queried.**
    Found by running the clean-room verifier against the independent Go
    consumer: both stores sort and dedupe the root list internally, but the Go
    bundle echoed the caller's unnormalised list, so its `roots` were not
    ascending and the verifier refused the whole bundle (`NonCanonical: roots is
    not strictly ascending`) before recomputing anything. `complete` and
    `minimal` also differed in the emitted start set: `complete` must include the
    artifact's declared transitive `closure` entries as roots, `minimal` must
    not. Both rules are now stated in `WEFT-TRAINING-ARTIFACT.md` §5 and the
    root derivation is one store function in each language.
