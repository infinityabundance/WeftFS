# WeftFS F2 implementation notes — Context Roots, capabilities, causal identity

**Scope:** F2 of `docs/PROGRAM.md`: executable Context Roots in the Rust and Go
stores, capability and budget enforcement, causal-closure identity, and
cross-language resolution. This file records what is implemented, the exact
court commands, and the remaining gaps. It is not a substitute for
`docs/STATUS.md`.

## 1. Implemented (Rust and Go)

### Context Root persistence
- A Context Root is a normal root-subject relation: predicate `context_root`,
  target = text name, attrs `context.record` (lowercase hex of the canonical
  `weft.context-root.v1` WCB) and `uri` (the authoritative
  `weft://<namespace>/c/<id>`).
- Creation is one commit. Duplicate names are `Exists`; `--replace` performs
  delete+add in one commit. Capability records referenced by a new root are
  persisted in the same commit under predicate `capability`.
- Resolution recomputes `ContextRootId` from canonical bytes, verifies the
  stored URI, requires the pinned snapshot to replay, accepts only
  `weft-resolver-v1`, and evaluates the selector against that pinned state.
  It returns ordered results plus `context_root_id`, `selector_id`,
  `snapshot_id`, `matched_count`, `returned`, `closure`, and effective budget.
  0/1/n cardinality is always explicit; no API returns "the first match".

### Capability enforcement
- `weft.capability.v1` records are parsed from a JSON file or canonical WCB
  hex. `--capability FILE` supplies a scope; a bound root also accepts the
  capability record stored in the same store.
- Enforcement points: context-root namespace/root grant, selector universe,
  relation/tag predicate traversal, exact-id access (`Store::exact_id_object*`,
  CLI/`/.weft/id`), saved-context views, and materialization. Denials are
  reported as `Unauthorized`/`NotFound` and do not expose counts, ids,
  evidence, or existence. The capability is an additional narrowing over the
  uid/gid/groups principal; deny wins.
- v1 explicitly makes **no cryptographic unforgeability claim**. A capability
  hash is a content identifier, not a signature or a MAC, and is not a defense
  against a privileged local attacker.

### Budgets
- Effective bound = component-wise minimum of the Context Root budget and the
  capability budget.
- **No truncation policy exists in v1.** A stated bound is a ceiling; exceeding
  it returns `LimitExceeded` (the store error class; `WEFT-CAPABILITIES.md`
  calls the condition `BudgetExceeded`). A successful result is never a
  silently truncated complete set, so `truncated` is always `false`.
- `max_bytes` bounds materialized bytes; `max_results` bounds returned results;
  `max_depth` is one-hop in v1; `max_work` uses "authorized objects inspected +
  relation edges traversed" as the deterministic work unit.

### Materialization
- `context materialize` verifies every chunk/manifest/whole-content hash in the
  pinned snapshot **before** publishing output. Files are written into a
  sibling temporary directory, hashed again, then the directory is renamed into
  place. Any mismatch aborts and leaves no partial output at `--out`.
- The manifest (`.weft-context.json`) records context root id, URI, snapshot id,
  selector id, root closure, capability hash, effective budget, matched/returned
  counts, and per-file path/object/version/content ids, content hash, size,
  kind, and causal closure.

### Causal identity
- `create --causal <ids>` / `write --causal <ids>` resolve explicit entries to
  artifact identities, compute the transitive closure available in the store
  with a visited set (cycles terminate), and store
  `weft.causal.parents`, `weft.causal.closure`, `weft.causal.hash`, and
  `weft.identity.mode`.
- `bytes_only` (default, no `--causal`) is byte-identical to the frozen v1
  version record. `causal_exact` adds `identity_mode` and `causal_hash` to the
  version record before hashing, so only that mode changes `VersionId`.
- Closure records are published content-addressed under the manifest domain
  (`H("weft/causal/v1", bytes)`), before the referencing object. GC marks them
  as manifests while the object that names them is retained.
- `causal show` prints the object's recorded parents/closure/hash/mode.
- Causal graphs may cycle; closure computation is bounded and terminates.
  Codec reconstruction dependency cycles remain errors and are a different
  graph.

### Verification
- `verify --full` validates context-root records and URIs, re-resolves each
  root and requires the recorded ids to be reproduced, validates causal attrs
  and the content-addressed closure blob, and decodes/re-hashes stored
  capability records.

### GC rule
- Context-root and capability relations are ordinary store metadata. They are
  retained exactly while their containing state (current, snapshot, or
  checkpoint) is retained. They never keep an object or content alive by
  themselves: materialized content remains reachable through the snapshot the
  root pins. A context root relation that exists only in current state and no
  retained snapshot disappears with that state's supersession.

## 2. Exact court commands and results

All commands run through `scripts/docker-court.sh` with unchanged hard limits.

```text
scripts/docker-court.sh fuse "cargo test --features fuse --all-targets"
  -> 70 passed, 0 failed, 2 ignored (CAP_SETUID ownership test; kernel 7.3
     same-inode direct-I/O EIO). New: context 12, CLI 5.

scripts/docker-court.sh fuse "cargo build --release --features fuse --locked --bin weftfs"
  -> release oracle built for Go handoff tests.

scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race ./..."
  -> store and fusefs packages pass, including:
     TestContextCreateResolveMaterializeCardinality
     TestContextSnapshotPinningAndReplacement
     TestContextCapabilityAndBudgetEnforcement
     TestCapabilityHashParticipatesInIdentity
     TestCausalBytesOnlyAndCausalExact
     TestCausalTransitiveAndCycle
     TestMaterializationMismatchLeavesNoOutput
     TestGoldenCausalVectorsMatchBothLanguages
     TestContextCausalCapabilityHandoffRustThenGo
     TestContextCausalCapabilityHandoffGoThenRust

scripts/docker-court.sh go "cd /work/WeftFS-Go && go vet ./... && CGO_ENABLED=0 go build ./..."
  -> clean.
```

The Go handoff tests use the Rust binary only as a test oracle. No Go production
package invokes Rust or a subprocess.

## 3. Cross-language matrix

| Direction | Context Root | Causal closure/VersionId | Capability | Materialization |
|---|---|---|---|---|
| Rust creates → Go reads/resolves | pass | pass | pass (stored record + hash) | pass |
| Go creates → Rust reads/resolves | pass | pass | pass (stored record + hash) | pass (Rust resolves Go roots) |

Golden causal vectors live in `conformance/context/v1/causal_versions.json` and
are asserted by both `tests/context.rs` (Rust) and `store/context_test.go` (Go).

## 4. Remaining gaps (honest)

1. **No formal model checking** for capability enforcement or resolution; tests
   are targeted, not a proof.
2. **No non-disclosure timing claim.** v1 does not claim constant-time
   authorization; denial classes are collapsed in output, not timing.
3. **No distributed/federated capabilities or revocation.** v1 capability
   records are store-local; revocation is not implemented in this phase.
4. **No cryptographic capability tokens.** Hashes identify grants; they do not
   authenticate them.
5. **`max_work` is deterministic but coarse** (objects inspected + relation
   edges traversed), not a precise cost model.
6. **Causal closure traverses the closure graph available in current state.**
   Historical closures are reachable through snapshots of the objects, but a
   resolver does not reconstruct a full global graph across all historical
   versions.
7. **FUSE capability wiring is exact-id/saved-context scoped.** A capability
   mount is forced read-only; full capability-scoped POSIX context mounts are
   F3 work.
