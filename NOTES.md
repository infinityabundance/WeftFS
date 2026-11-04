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

---

# F3 implementation notes — context mounts, ambiguity, time travel, replay

**Scope:** F3 of `docs/PROGRAM.md`: A1 (context mounts), A3 (ambiguity court),
A4 (time-travel POSIX), A2 (replay bundle), and the D2 task/model/time mount
surface. This section records the exact commands and the remaining gaps.

## 5. What F3 adds

### Mount roots (`--context`, `--at`)
- `weftfs mount --context NAME|weft://...` resolves a stored Context Root at
  mount time with the mount capability (if any) and the root principal,
  evaluates the pinned selector, and makes that ordered result set the mount
  root. Names are the existing deterministic `basename--<8 hex ObjectId>`
  scheme, extended on collision. One helper builds the entry list for both
  `readdir` and `lookup`, so they always agree. A constant `.weft` entry keeps
  the exact-id surface reachable; it is not a selector result.
- The root inode is a virtual directory (`0o555`), every result is a virtual
  read-only node, and all mutation verbs return `EROFS`. A capability mount is
  forced read-only even without `--read-only`, and resolution filters before
  names/counts are produced.
- `--at RFC3339|unix-ns` parses UTC nanoseconds or RFC 3339 (fractional
  seconds, `Z` or numeric offset) and walks the published parent chain of the
  base (head, or `--snapshot` when supplied). It selects the newest commit with
  `timestamp_ns <= t`, breaking ties by higher generation. No match is
  `NotFound: no commit at or before …`; no fallback state is used.
- `--context` pins its own snapshot, so `--context` + `--at` is rejected.
- Read-only opens take no `flock` in both implementations, so any number of
  mounts can coexist with a live writer. The test suite exercises three
  simultaneous roots and a successful writer commit.

### Ambiguity
- `require_one` re-evaluates without `offset`/`limit` and includes every
  authorized ObjectId in the `Ambiguous` message. `context materialize
  --require-one` resolves first and returns `Ambiguous` before creating any
  output path.

### Replay bundle
- `weftfs export-bundle --out FILE [--context NAME]...` copies `FORMAT`, `LOCK`,
  `superblock`, `commits/`, `objects/`, `manifests/`, and `refs/` into
  `store/`; materializes each Context Root once in a temporary work directory
  to hash its exact bytes; writes `manifest.json` (store head/generation,
  ContextRootIds, snapshot/selector ids, per-file BLAKE3 ContentId and SHA-256,
  full relation graph, source revisions, court command), `contexts.tsv`,
  `checks.sha256`, `materialized.sha256`, `replay.sh`, and `README.md`; then
  creates the tarball with `tar` and compresses with `zstd`.
- `scripts/replay-bundle.sh` extracts with `tar --zstd`, checks
  `checks.sha256`, builds/uses the independent Go frontend, runs
  `verify --full`, resolves and materializes every Context Root, and runs
  `scripts/replay/verify_manifest.go` to re-hash every store entry and
  materialized file and compare the resolved identities. No Rust runs in this
  path.

## 6. Exact commands and results

```text
scripts/docker-court.sh fuse "cargo test --features fuse --all-targets"
  -> 81 passed, 0 failed, 2 ignored (CAP_SETUID ownership; kernel 7.3
     same-inode direct-I/O EIO). New: fuse_context_mount 6, CLI ambiguity 1.

scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race ./..."
  -> store and fusefs pass; new context-mount, capability, --at, three-mount,
     and ambiguity tests pass.

scripts/docker-court.sh go "cd /work/WeftFS-Go && go vet ./... && CGO_ENABLED=0 go build ./..."
  -> clean.

scripts/docker-court.sh fuse "bash scripts/demo-context-mounts.sh"
  -> 6/6 pass. Receipt: bench/receipts/context-mounts-20261103T210625Z.json.

scripts/docker-court.sh fuse "bash scripts/demo-ambiguity.sh"
  -> 4/4 pass. Receipt: bench/receipts/ambiguity-20261103T210633Z.json.

scripts/docker-court.sh fuse "bash scripts/demo-time-travel.sh"
  -> 4/4 pass. Receipt: bench/receipts/time-travel-20261103T210633Z.json.

scripts/docker-court.sh fuse "bash scripts/demo-replay-bundle.sh"
  -> 4/4 pass. Bundle: bench/bundles/weft-receipt.tar.zst
     sha256 940579db909ba8a3360f73377448ce90df455278a37c5a0b02f6bb7a584bdbd2.
```

The context-mount, ambiguity, and time-travel receipts record WeftFS
`f3d1bc1` and WeftFS-Go `acf6df6`; the replay-bundle receipt records WeftFS
`bcedd26` and WeftFS-Go `7693ad4` (the replay-verifier path fix). The
Context-mount demo was run twice and compared with
`scripts/showcase/democmp.go`: `bench/receipts/context-mounts-determinism.txt`
says `semantic_match=true`.

## 7. Remaining F3 gaps

1. **No acceleration.** Time travel and context mounts are correctness-only.
   io_uring, EKBLOOM, and similar are not implemented and not claimed.
2. **SHA-256 vs ContentId.** The demos compare each mounted file's SHA-256
   with a SHA-256 attribute declared in the store, and separately require the
   store's BLAKE3 ContentId to match `stat`/`context resolve`. The normative
   content root remains the BLAKE3 ContentId; SHA-256 is not substituted for it.
3. **Deterministic demos use fixed timestamps and explicit ObjectId seeds.**
   `mkdir` without `--seed` uses a random seed, so a deterministic fixture must
   pass seeds for directories as well. This was found by the determinism run.
4. **Replay bundles are whole-store.** No partitioned subset, remote fetch, or
   signature exists. `tar`/`zstd` are hard requirements.
5. **D2 is partial.** Task/model/time mounts and typed edges pass; a
   months-long retention measurement is not made.
6. **Context mount root includes `.weft`.** Listing digests exclude it
   explicitly; it is a constant control surface, not a selector result.

---

# F8 — WeftFS-Go FUSE verb closure and online GC

Scope: close the remaining Go FUSE gaps and add an opt-in background collector.
Detailed semantics, commands, and the precise blocked items are in
`WeftFS-Go/NOTES.md` §F8; this section is the program-level pointer.

## 8. What changed

- Verbs: real in-memory `getlk`/`setlk`/`setlkw` for POSIX byte ranges and
  flock (`EnableLocks`; blocking wait plus `EINTR` cancellation); `mknod`
  regular files only, FIFO/socket/device `EPERM`; `tmpfile` (O_TMPFILE,
  linkable via `/proc/self/fd`); `fallocate` modes 0/`KEEP_SIZE`/`PUNCH_HOLE`/
  `ZERO_RANGE` plus store-level COLLAPSE/INSERT; daemon-side
  `copy_file_range`; dense `lseek` `SEEK_DATA`/`SEEK_HOLE`. `ioctl` remains
  absent (`ENOTTY`).
- GC: `store.GCBounded` (bounded deletions per pass), `store.GCWatch` (standalone
  CLI loop), and `fusefs.Server.StartBackgroundGC` (`mount --gc-watch`). Passes
  require the writer lock, take the FUSE server mutex, re-derive roots
  (current/snapshots/checkpoints/pins) every time, are cancellable, and are
  disabled by default. `gc --apply --watch` requires the explicit `--apply`.

## 9. Exact results

```text
scripts/docker-court.sh go "cd /work/WeftFS-Go && gofmt -l . && go vet ./... && CGO_ENABLED=0 go build ./..."
  -> clean.

scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race -count=1 -v ./..."
  -> 74 passed, 2 skipped (existing child-process guards), 0 failed.
     fusefs 6.503 s, store 1.893 s.
```

CLI watch smoke: two bounded passes deleted the two unreachable blobs
(`deleted_files=1` each), later passes reported zero, SIGTERM exited 0, and
`verify --full` was `ok=true`. Cross-language:
`TestGCHandoffGoBackgroundWatchRustVerifies` runs the Go watcher over
Rust-created garbage and then requires Rust `verify --full`, Rust live read,
and a Rust snapshot mount read to pass. The existing GC handoff gates A–D
still pass.

## 10. Exact limitations

1. **COLLAPSE/INSERT at the kernel boundary.** The store implements and directly
   tests both. Kernel 7.3.1 refuses these modes with `EOPNOTSUPP` before
   forwarding; go-fuse debug showed no `FUSE_FALLOCATE` frame. The mount test
   records that refusal and does not claim the verb end-to-end on this kernel.
2. **Physical space is not reclaimed.** v1 files are dense, so `PUNCH_HOLE` and
   `ZERO_RANGE` zero content without freeing chunks; no sparse-extent format
   exists.
3. **Locks are per-daemon advisory.** They do not coordinate independent mounts
   or hosts. FUSE does not reveal a POSIX owner at open, so closing a handle
   that never acquired a lock cannot implement POSIX close-any-fd release;
   owners seen on a handle are released at handle release, and explicit
   `F_UNLCK` always works.
4. **Background GC is opt-in.** Default collection remains an explicit pass.
   Long-lived readers pinned to non-snapshot historical commits are outside the
   root set; use snapshots or the grace window.
5. **Still absent:** `ioctl`, mmap writeback, persisted atime/ctime, physical
   power-loss court.
