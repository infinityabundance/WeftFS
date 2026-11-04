# Design note: Context Root as the primary identity

**Status:** Design rationale for `spec/WEFT-CONTEXT-ROOT.md`. Normative rules
live in the spec; this note explains *why* the layering is what it is, what is
proven today, and what remains conjecture.

---

## 1. The layering

```
                 weft://<namespace>/c/<ContextRootId>     public address
                              |
                      Context Root record                 selection + policy
                selector bytes, SelectorId,
                snapshot commit, resolver version,
                causal closure, capability hash, budget
                     /          |          \
              SelectorId    SnapshotId   CausalClosureId / CapabilityHash
                     |            |              |
                 expression   commit root    ancestry / grant
                              /       \
                        ObjectId ... ContentId          content identity
                              |
                          exact bytes
```

Each layer answers a different question:

| Layer | Question | Changes when |
|---|---|---|
| `ContextRootId` | *Which selection, at which state, under which rules?* | selection, snapshot, policy, closure, grant, or budget changes |
| `SelectorId` | *Which expression?* | the expression changes (never the snapshot) |
| `SnapshotId` | *Which committed state?* | a new commit exists |
| `ObjectId` | *Which object, independent of path?* | never (rename/retag/update) |
| `ContentId` | *Which exact bytes?* | the bytes change |
| Path | *Where is it in this mutable projection?* | rename, link, mount |

The Context Root is layered **above** `ContentId`/`ObjectId`, not instead of
them. Content identity remains the integrity primitive. The Context Root adds
the two things content hashes cannot express: *a selection* and *the rules of
its evaluation*.

## 2. Why not make the selector the address?

`SelectorId` is already a stable identity for an expression. It is insufficient
as a public address because it intentionally omits:

- the snapshot (a selector is live; results change after commit);
- the resolver/policy version (evaluation semantics can change);
- the causal closure (ancestry is part of meaning for derived artifacts);
- the capability scope and budget (the authorized view and resource ceiling).

Including those in the Context Root makes the address denote exactly one
resolvable view. It also avoids a class of bugs where a cached or logged
`SelectorId` is mistaken for "the result" or "the latest result".

## 3. Why not make the content hash the address?

A content hash tells you what bytes exist, not which selection produced them or
whether the selection is complete. Two roots may legitimately select identical
bytes; merging them by content would erase provenance and authorization
boundaries. Conversely, the same content can be reached through many roots with
different causal closures; in `causal_exact` mode those are distinct versions.

The Context Root is deliberately **not** deduplicated against other roots. Store
object dedup still happens at the chunk/manifest layer, where it is safe.

## 4. Determinism

Determinism is preserved by construction:

1. **Canonical bytes.** The record is WCB with a fixed field set and WCB
   encoded-key ordering. There is no map iteration order, no locale, no clock.
2. **Canonicalization at the boundary.** Descriptor URIs are normalized by
   parsing the selector, rebuilding canonical selector bytes, sorting/deduplicating
   the closure, and validating the budget. The authoritative URI is a pure
   function of the canonical record.
3. **Canonical ordering of results.** `selector_then_object_id` is part of the
   record, so even a resolver rewrite cannot silently change tie-breaks without
   changing the address.
4. **Pinned snapshot.** No "latest" exists in the identity. A live query is a
   different construct with no `ContextRootId` until a snapshot is chosen.
5. **Authorization before evaluation.** Counts and evidence are computed on the
   authorized universe, so a principal's result is a deterministic function of
   `(root, principal context, pinned snapshot)`.
6. **Cross-language equality.** The canonical record and the `weft://`
   authoritative URI are byte-identical in Rust and Go; golden vectors freeze
   both.

What determinism does **not** mean: equal semantics. Two selectors that are
logically equivalent but written differently have different `SelectorId`s (and
usually different `ContextRootId`s). That is intentional: identity tracks the
expression, not a theorem prover's opinion.

## 5. What is proven, what is tested, what is conjecture

| Claim | Status |
|---|---|
| Canonical WCB record and hash are deterministic and cross-language identical | **Tested** (F1 conformance vectors + Rust↔Go gate) |
| Descriptor URI canonicalizes to exactly one authoritative URI | **Tested** (round-trip and rejection vectors) |
| Unsorted/duplicate closure, zero snapshot, bad namespace, bad budget are rejected | **Tested** (rejection vectors) |
| The record can bind a canonical selector and rejects a mismatched `SelectorId` | **Tested** |
| Resolution returns 0/1/n, never "first match" | **Specified**; enforced in F2/F3 (existing selector APIs already fail closed on ambiguity) |
| Authorization filtering before counts/evidence | **Specified** for capability mode; store-level filtering exists in P4/P5 |
| Materialization exactness for a Context Root | **Specified**; materialization exists, root binding arrives in F3 |
| Causal cycles terminate with a visited set | **Specified**; bounded enumeration precedent exists in P4 recovery tests; no causal-graph court yet |
| Cryptographic capability unforgeability | **Not claimed** |
| Distributed capability transfer/revocation | **Conjectured / out of scope** |

## 6. Explicit non-goals

- The Context Root does not encode a *result set*, a *file list*, or a
  *materialization receipt*. Those are derived evidence, not identity.
- It does not encode a mutable tag or name. Names may point at roots; the name
  is not the root.
- It is not an access-control token. The capability hash binds a scope; it does
  not prove possession or grant authority by itself.
- It does not solve distributed consistency, cross-store namespace resolution,
  or federated revocation.
- It does not promise selector equivalence or query optimization.
- It does not claim that two different Context Roots with the same results are
  the same root.

## 7. Consequences for the rest of the program

- **Mounts** (F3) become `weft://...` addresses plus a resolver; a mount's
  `/proc`-style identity can name the root.
- **Replay bundles** (F4) can name roots instead of paths, and Go can replay
  without Rust because the canonical bytes and URI are frozen.
- **Buck2/agent hooks** (F8) get a stable action/context key that includes the
  toolchain/resolver version and causal ancestry.
- **Time travel** (F3) is simply resolution at an older pinned snapshot; there
  is no "overwrite" of a root.
- **Ambiguity** (A3) is structurally impossible to hide: the root carries a
  selector whose cardinality is always exposed, and singular access fails closed.

The design deliberately pays a small cost in URI length and one extra hash to
remove an entire class of "which result did you mean?" ambiguity.
