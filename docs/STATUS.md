# Implementation status — 2026-11-04

This is the honest, evidence-backed state of the program. It distinguishes
implemented, tested, ignored, and not done. The current verification receipt is
`bench/receipts/verify-20261104T100531Z.json` (all eighteen courts `pass`,
including quantized cross-decode, generated-type drift, Buck2 CAS, and agent
memory), at WeftFS `c72255d`, WeftCodec `474fc33`, WeftCodec-Go `c3f30e8`,
WeftFS-Go `33743f0`. The preceding receipt `verify-20261104T022254Z.json` is
retained as history. Earlier `verify-20261103T220311Z`,
`verify-20261103T172416Z`, and `verify-20261103T165944Z` receipts are retained
as history. The
twelve-step showcase receipt is
`bench/receipts/showcase-20261103T171440Z.json` (twelve of twelve `pass`).
The P7 benchmark receipts are `bench/receipts/bench-codec-20261103T175847Z.json`,
`bench/receipts/bench-store-20261103T175920Z.json`, and
`bench/receipts/bench-fs-20261103T180751Z.json`; the analysis, losses, and
gaps are in `bench/RESULTS-v1.md`. The comparative pass receipts are
`bench/receipts/bench-external-20261104T000122Z.json`,
`bench/receipts/bench-codec-matched-20261103T232158Z.json`,
`bench/receipts/bench-fs-cold-20261103T232216Z.json`,
`bench/receipts/bench-store-20261103T235127Z.json` (same-revision store side),
and `bench/receipts/energy-probe-20261103T225547Z.json`; analysis is
`bench/RESULTS-comparative-v1.md`. The F2 receipt is
`bench/receipts/f2-20261103T205045Z.json` with its raw log beside it. The F3
receipts are `bench/receipts/context-mounts-20261103T210625Z.json` (6/6),
`bench/receipts/ambiguity-20261103T210633Z.json` (4/4),
`bench/receipts/time-travel-20261103T210633Z.json` (4/4), and
`bench/receipts/replay-bundle-20261103T210824Z.json` (4/4), each with its raw
`.log` and `.results.tsv`; the determinism comparison is
`bench/receipts/context-mounts-determinism.txt` (`semantic_match=true`).
The F6 cross-language gate is `WeftCodec-Go/hack/cross.sh` (tensor/tokens/
quantized byte-identical in both directions). The current F7 receipt is
`bench/receipts/ai-corpus-20261104T021627Z.json` with cost graph JSON/CSV beside
it (whole-pair-aware planner); analysis, losses, and caveats are in
`bench/RESULTS-ai-v1.md`. The historical frame-smallest receipt
`ai-corpus-20261103T220113Z.json` is retained. F8 receipts are
`bench/receipts/buck2-cas-20261104T022511Z.json` (15/15 prototype assertions)
and `bench/receipts/agent-memory-20261104T022512Z.json` (7/7 simulated-timestamp
months-later replay). Generated-type drift is `scripts/gen-types.sh --check`,
wired into `verify-all`.
The F9 TLA+ receipt is `bench/receipts/tla-20261103T224446Z.json` (corrected
model pass; flawed model counterexample). The F9 power-loss receipts are
`bench/receipts/powerloss-20261103T224453Z.json` (30/30, `cache=none`),
`bench/receipts/powerloss-20261103T224652Z.json` (10/10, `cache=writeback`), and
`bench/receipts/powerloss-20261104T014805Z.json` (30/30, `cache=unsafe`,
lying-fsync device model). The block-layer lying-disk matrix is
`bench/receipts/blkdebug-20261104T072008Z.json` (30 iterations; dm-flakey
silent drop 5/5 lost, dm-flakey EIO 5/5 fail-closed, snapshot total drop 5/5;
blkdebug write/flush error 10/10 blocked at ext4 mount). The host-cache probe is
`bench/receipts/hostcache-probe-20261104T072314Z.json` (global drop refused:
`Read-only file system`). The B2 10,000-iteration receipt is
`bench/receipts/crash-torture-20261104T013521Z.json`. The counter/energy probe is
`bench/receipts/perf-counters-20261104T015026Z.json` (perf counters available;
RAPL energy still unreadable). The whole-disk ZFS/Btrfs fio run is
`bench/receipts/bench-fs-cold-20261104T071922Z.json` (ext4/XFS/Btrfs/ZFS,
`zfs-whole` uses raw `/dev/vdb`; one repetition per cell). The earlier
file-backed ZFS run `bench-fs-cold-20261104T014727Z.json` is retained and
separately labelled. The abstraction mapping, assumptions, and limitations are
in `verification/README.md`.

## 1. Repositories and revisions

| Repo | Verified revision | Role |
|---|---|---|
| `WeftFS` | `3ea3e09` | program home, Rust store + FUSE, specs, ADRs, courts |
| `WeftCodec` | `cc31f77` | Rust codec v1 |
| `WeftCodec-Go` | `1a48394` | independent Go codec |
| `WeftFS-Go` | `e4c5f04` | independent Go store + FUSE |

These are the revisions pinned by the latest full verification receipt; later
commits (documentation, tests, packaging) may be ahead of them without a new
full receipt. `staging` and `main` are synchronized and pushed for all four
repositories; `v0.3.0` tags exist in each. The research directory and
`AGENTS.md` are gitignored, per operator directive.

## 2. What is implemented and tested

### WeftCodec (Rust) — implemented

- Exact WFT1 framing: 96-byte header, 48-byte dependency/chunk entries, header
  and trailing CRC32C, checked lengths, reserved-bit rejection.
- Domain-separated BLAKE3, minimal LEB128, canonical WCB with encoded-key map
  ordering and non-canonical rejection.
- RAW, ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED, TENSOR, TOKEN_SEQUENCE,
  QUANTIZED_TENSOR; chunked RAW/ZSTD; `read_range` decodes only intersecting
  chunks.
- Resolver + frame-DAG decoding with cycle detection and depth/fetch/decoded
  bounds; WFA1 self-contained archives.
- WeftQL parser, normalization, canonical selector bytes, SelectorId.
- CLI (`encode`, `decode`, `inspect --json`, `verify`, `archive`, `selector`,
  `conformance`), including `--mode tensor` and `--mode tokens`.
- **110 tests pass** in the Rust court. Frozen vectors: 60 codec (including 28
  AI-mode valid/reject vectors), 20 WCB, 22 selector, 16 context, 10 causal,
  and 36 adversarial.

### WeftCodec-Go — implemented, independent

- Native Go implementation of the same v1 bytes (no cgo, no Rust calls).
- **57 top-level tests pass**, including Rust-vector replay and malformed-input
  classes; `go vet` and a CGO-disabled build pass.
- Cross-language gates pass in both directions for RAW, ZSTD, REFERENCE, DELTA,
  DICTIONARY, chunked, STRUCTURED, TENSOR, TOKENS, and QUANTIZED_TENSOR.
  RAW/REFERENCE/DELTA/STRUCTURED/TENSOR/TOKENS/QUANTIZED_TENSOR frames are
  byte-identical; ZSTD/DICTIONARY are decode-equivalent by spec.

### F6 — AI codec modes (implemented)

- `TENSOR` (mode 6): exact little-endian row-major payload plus a canonical WCB
  descriptor (`weft.tensor.v1`) carrying dtype, shape, layout, endianness,
  element count, payload bytes, and a `weft/tensor/payload/v1` hash binding.
  Supported dtypes: `f16`, `bf16`, `f32`, `f64`, `i8`, `u8`, `i16`, `i32`,
  `i64`. Checked shape arithmetic; scalar and zero-element tensors; rank ≤ 8.
- `TOKEN_SEQUENCE` (mode 7): canonical WCB descriptor (`weft.tokens.v1`) and a
  deterministic `u64-zigzag-delta-leb128` payload: first id absolute, later ids
  wrapping 64-bit signed zigzag deltas, minimal LEB128, exact count and
  trailing-byte checks.
- Both modes: `decoded_len` is the payload length and `content_hash` is
  `H(weft/chunk/v1, payload)`; the descriptor binds the payload under its own
  domain. Frame `domain=0`, no dependencies, no chunks.
- `QUANTIZED_TENSOR` (mode 8, added 2026-11-04): packed `i8`/`u8` row-major
  payload plus a `weft.quantized-tensor.v1` descriptor carrying dtype, shape,
  layout, `affine-asymmetric-per-tensor`, exact f64 `scale_bits_le`, i64
  `zero_point`, element count, payload bytes, and a
  `weft/quantized-tensor/payload/v1` hash binding. Finite positive scale and
  dtype-range zero point are enforced; three valid and eight reject vectors;
  Rust↔Go frames are byte-identical.
- **Not implemented and not allocated a wire value:** blocked/column-major/
  sharded layouts, i4 nibble packing and per-axis quantized scales,
  activation/gradient streams, embedding/KV-cache pages, attention masks,
  vocabulary-artifact binding, weight-delta descriptors, and semantic/structured
  deltas. They are `specified, not implemented` in `spec/WEFT-AI-CODEC.md` §5.
- Cross-language: Rust→Go and Go→Rust decode gates pass; TENSOR, TOKENS, and
  QUANTIZED_TENSOR frames are byte-identical. Historical
  `017_unknown_mode_reserved8` (mode 8 reserved) is retained byte-for-byte and
  marked superseded by the new mode-9 reject vector.

### F7 — AI corpus and cost graph (implemented, synthetic)

- Committed deterministic corpus `bench/corpora/ai-v1/`: 60 files, 1,939,911
  source bytes, 1,516,737 visible bytes; prompts, JSON/text tool outputs,
  near-duplicate agent intermediates, 12 typed tensors, 8 token sequences, and
  near-duplicate checkpoint-like blobs. License CC0-1.0, synthetic provenance,
  per-file SHA-256 manifest.
- Receipt `bench/receipts/ai-corpus-20261104T021627Z.json` + cost graph JSON/CSV
  beside it. Historical `ai-corpus-20261103T220113Z.json` retained. Analysis,
  every loss, and caveats in `bench/RESULTS-ai-v1.md`.
- Current measured physical bytes: `zstd_forced` 575,808; `weft_auto` 575,713;
  `cdc_zstd` 603,094 (+4.74%); `weft_context` 514,506 (−10.65% vs forced zstd)
  with raw pair bytes 697,014 (45.96% of visible), 8 pair losses, and 8
  contextual objects. The current whole-pair-aware planner rejects contextual
  candidates whose base is not amortized and whose pair cost does not beat the
  context-free frame; 24 such rejections are recorded. The historical
  frame-smallest planner produced 176,564 physical bytes but 1,367,212 pair
  bytes and 20 pair losses; it is retained as a superseded result.
- Typed TENSOR/TOKENS modes were evaluated for all 20 typed objects and chosen
  0 times; descriptor overhead 272–288 bytes. QUANTIZED_TENSOR is implemented
  and cross-language tested but is not part of the ai-v1 corpus. No superiority
  claim.

### F8 — Hooks: Buck2 CAS prototype, agent memory, generated types (implemented)

- **D1 Buck2 adapter (prototype, not a plugin):** `adapters/buck2/` implements a
  local CAS plus Buck2-compatible `sha256:<hex>` blob layout. The action key is
  a real `weft.context-root.v1` record whose pinned snapshot is the BLAKE3
  digest of the typed action context (action type + toolchain + sorted inputs).
  `store`, `fetch`, `materialize`, `query`, and `verify` exist; the same key
  returns identical bytes, different toolchain/input misses, query by action
  type/toolchain/input works, and same-key/different-bytes fails closed. Go
  tests + `go vet` and a 15/15 assertion demo run in the Go court; receipt
  `bench/receipts/buck2-cas-20261104T022511Z.json`. No REAPI gRPC, no Buck2
  plugin ABI, no performance claim.
- **D2 agent long-term memory:** `scripts/demo-agent-memory.sh` creates
  prompt/model/tool/dataset/output objects and five typed edges, three Context
  Roots (task/model/time), a January snapshot, an exported bundle, then
  simulates September writes and replays the January bundle later. Verified:
  run-001 sha256 exact in all January views; run-002 absent from January;
  September time root keeps old bytes exact; replayed bundle reproduces all
  three ContextRootIds and five lineage edges with Go only. Receipt
  `bench/receipts/agent-memory-20261104T022512Z.json` (7/7). Timestamps are
  simulated; this is not a real-time aging or retention measurement.
- **D3 spec-as-code:** `spec/schema/identity-v1.schema` is the canonical source;
  `scripts/gen-types.sh` generates `WeftCodec/src/generated.rs` and
  `WeftCodec-Go/generated/generated.go` (schema/domain constants, limits,
  record types, field lists, source SHA-256). The runtime identity layer
  re-exports generated constants; Rust and Go tests compare generated values to
  runtime values; `scripts/gen-types.sh --check` fails on drift and is a court
  in `verify-all`. The schema is exported to subproject `spec/schema/` and
  checked by `scripts/check-spec-drift.sh`.

### WeftFS (Rust) — implemented core + FUSE (with documented limitations)

- Portable store: `FORMAT`, `LOCK`, dual CRC32C superblock slots, `WFC1`
  commits, canonical commit/checkpoint schemas, immutable chunks/manifests,
  snapshots, relations, tips, xattrs, saved contexts.
- Atomic publication with fsync ordering; single-writer `flock`; generation
  fallback; recovery; full verify; mark-and-sweep GC; checkpoints; selector
  evaluation with POSIX + `weft.acl.read` authorization; ambiguity errors.
- FUSE: lookup/getattr/setattr/open/create/read/write/flush/fsync/release,
  readdir(+plus), mkdir/rmdir/unlink/rename(+NOREPLACE/EXCHANGE)/link/symlink/
  readlink, xattrs, statfs, access, mknod(regular; FIFO/device `EPERM`),
  fallocate(0/KEEP_SIZE/PUNCH_HOLE/ZERO_RANGE/COLLAPSE/INSERT),
  copy_file_range(server-side), lseek(SEEK_DATA/SEEK_HOLE), O_APPEND/O_TRUNC/
  O_EXCL, sparse writes, open-unlinked, snapshot mounts, `/.weft/id/<hex>`,
  saved context views. Explicit binding boundaries: Rust `O_TMPFILE` and
  daemon-side flock/POSIX locks are unavailable in fuser 0.15.1.
- Deterministic store fixture `conformance/store/v1/` (head
  `8959c65b...`) with `EXPECTED.json`.
- **64 core tests pass** (lib 7, CLI 6, crash proxy 3, recovery 11, store core
  11, extensions 11, bounded recovery 3, context 12). `tests/bounded_recovery.rs`
  enumerates every one of generations 3–5 corrupted in three ways (empty, half,
  bit flip) and every superblock-slot corruption, asserting the previous whole
  state and replay idempotence. This is bounded enumeration, **not** TLA+/Alloy
  model checking.
- **86 FUSE-feature tests pass, 0 fail, 2 ignored** (fuse 22, core 64):
  - ownership test needs `CAP_SETUID` (court has only `CAP_SYS_ADMIN`);
  - same-inode concurrent direct read/write can return `EIO` on kernel 7.3
    with fuser 0.15.1. The daemon logs successful replies and the failing
    request never arrives; rate depends on timing. This is documented, with
    the exact reproducer, in `docs/fuse-status.md`. Store-level atomicity is
    tested separately. A same-description write after shared `mmap` also
    returned `EIO` once; shared mmap/write/msync/read passes with a fresh
    description.

### WeftFS-Go — implemented with documented gaps

- Independent Go reader/writer for the identical on-disk format, all eight
  commit ops, checkpoints, superblock recovery, manifests/chunks, selector
  evaluation, authorization, `verify --full`.
- Native `go-fuse/v2` frontend with the core POSIX surface, snapshot mounts,
  `/.weft/id/<hex>`, and saved-context views. Added in F8: `getlk`/`setlk`/
  `setlkw` (POSIX byte ranges + flock through an in-memory manager and
  `EnableLocks`), `mknod` (regular files only; FIFO/socket/device `EPERM`),
  `tmpfile` (O_TMPFILE plus link through `/proc/self/fd`), `fallocate` modes
  0/`KEEP_SIZE`/`PUNCH_HOLE`/`ZERO_RANGE` (with store-level COLLAPSE/INSERT),
  `copy_file_range`, and `lseek` `SEEK_DATA`/`SEEK_HOLE`. `ioctl` is
  intentionally unimplemented (`ENOTTY`).
- **Online/background GC:** `store.GCBounded`/`GCWatch`, `gc --apply --watch`,
  and `mount --gc-watch`. Passes require the writer lock, run under the FUSE
  server mutex, delete at most a configured number of candidates, are
  cancellable, preserve snapshots/checkpoints/pins, and are disabled by
  default.
- **74 top-level tests pass, 2 skipped (pre-existing child-process guards),
  0 failed** under `go test -race -count=1 ./...`; `go vet ./...` and
  `CGO_ENABLED=0 go build ./...` pass. New verb/GC tests are listed in
  `WeftFS-Go/NOTES.md` §F8.
- Rebuilding the Rust fixture through the Go writer reproduces the same head
  and byte-identical `FORMAT`, `superblock`, commits, manifests, chunks, and
  snapshot pointer.
- Bidirectional handoff gates pass: Rust fixture → Go mutations → Rust verify;
  Go init/mutate → Rust verify/mount; alternating generations; snapshots and
  checkpoints both ways; crash proxy through both; and Go background GC → Rust
  `verify --full` + snapshot mount.
- Remaining limits: `PUNCH_HOLE`/`ZERO_RANGE` define content but do not reclaim
  physical chunks (dense v1); 7.3.1 FUSE rejects COLLAPSE/INSERT before
  forwarding; locks are per-daemon advisory and do not implement the POSIX
  close-any-fd release for owners never seen on the closed handle; `ioctl`,
  mmap writeback, and persisted atime/ctime are not implemented.

### F8 — WeftFS-Go FUSE verb closure and online GC (implemented 2026-11-03)

- Commands: `scripts/docker-court.sh go "cd /work/WeftFS-Go && gofmt -l . &&
  go vet ./... && CGO_ENABLED=0 go build ./..."` (clean) and
  `scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race -count=1
  -v ./..."` (**74 passed, 2 skipped, 0 failed**; skips are the existing
  child-process guards).
- CLI watch smoke: `gc --apply --watch --interval 100ms --max-deletes 1
  --grace-secs 0` deleted two unreachable blobs in two bounded passes, then
  reported zero; SIGTERM exited 0; `verify --full` reported `ok=true`.
- Cross-language: the Rust oracle created a snapshot plus garbage; the Go
  watcher swept it; Rust `verify --full`, Rust live read, and a Rust snapshot
  mount all passed. Existing GC/handoff gates A–D also pass.
- Blocked verb evidence: on kernel 7.3.1, COLLAPSE/INSERT return
  `EOPNOTSUPP` with go-fuse debug showing no `FUSE_FALLOCATE` frame; the store
  implementations are directly tested. `PUNCH_HOLE` without `KEEP_SIZE` is
  also rejected at that boundary. Details and safe-use notes are in
  `WeftFS-Go/NOTES.md` §F8.

### F2 — Context Roots, capabilities, and causal identity (implemented in Rust and Go)

- **Context Root persistence:** `context_root` relations from the root object
  with the canonical record hex and authoritative `weft://` URI. Duplicate names
  are `Exists`; `--replace` is delete+add in one commit. Capability records are
  stored in the same commit when first referenced.
- **Resolution:** recomputes `ContextRootId`, verifies the stored URI, requires
  the pinned snapshot to replay, accepts only `weft-resolver-v1`, authorizes
  before counting, and returns ordered results plus `context_root_id`,
  `selector_id`, `snapshot_id`, `matched_count`, `returned`, and closure
  entries. 0/1/n is always explicit.
- **CLI:** `context create|list|show|resolve|materialize|uri|delete`,
  `causal show`, `uri canonicalize|show`, and `--causal` on `create`/`write` in
  both languages.
- **Capabilities:** `weft.capability.v1` JSON/hex files; enforcement for
  namespace/root grants, selector universe, relation/tag predicates, exact-id
  access, saved-context views, and materialization. Denials are
  `Unauthorized`/`NotFound` and do not expose counts, ids, evidence, or
  existence. v1 makes no cryptographic unforgeability or timing claim.
- **Budgets:** effective bound is the component-wise minimum of root and
  capability budgets. No truncation policy exists in v1: exceeding a bound is
  `LimitExceeded`, never a silent truncation. `max_bytes` bounds materialized
  bytes, `max_results` bounds returned results, `max_depth` is one-hop, and
  `max_work` is deterministic **authorized** objects inspected plus **visible**
  relation edges. Authorization and namespace-root readability are decided
  before work accounting, and `max_work`/`max_depth` failures emit fixed,
  non-disclosing messages (`resolver work budget exceeded`,
  `traversal depth exceeds max_depth`). This closes a real defect: the earlier
  implementation counted the whole state (including unauthorized objects and
  relations) and echoed the count. Proven by
  `tests/capability_nondisclosure.rs`: two stores with identical authorized
  views and different hidden populations return identical results, work units,
  and budget outcomes.
- **Causal identity:** `--causal` computes explicit parents plus the available
  transitive closure with a visited set (cycles terminate), stores the four
  reserved attrs, publishes the closure record content-addressed, and includes
  `identity_mode`/`causal_hash` in `VersionId` only for `causal_exact`.
  `bytes_only` remains byte-identical to the frozen v1 record.
- **Materialization:** all content hashes are verified before output is
  published; writes go to a sibling temporary directory and are renamed into
  place. A mismatch leaves no partial output. The manifest records root id,
  snapshot id, selector id, closure, capability hash, budget, counts, and
  per-file path/hash/size/ids/causal closure.
- **Verification and GC:** `verify --full` validates context records/URIs,
  re-resolves roots and reproduces recorded ids, checks causal attrs and
  closure blobs, and re-hashes stored capability records. GC retains causal
  closure blobs while their referencing object is retained; context/capability
  relations are ordinary metadata and never keep content alive by themselves.
- **Evidence:** `scripts/docker-court.sh fuse "cargo test --features fuse --all-targets"`
  reports **70 passed, 0 failed, 2 ignored** (the existing CAP_SETUID and
  kernel-7.3 direct-I/O limitations). `scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race ./..."`
  passes for `store` and `fusefs`, including Rust→Go and Go→Rust context,
  causal, capability, and materialization handoff gates.
  `go vet ./...` and `CGO_ENABLED=0 go build ./...` pass in the Go court.
  Golden causal vectors are `conformance/context/v1/causal_versions.json`.

### Codec-backed representation manifests (implemented 2026-11-04, opt-in, Rust-only)

- **What it is:** a `weft.manifest.v2` representation manifest can store an
  object's bytes as a WFT1 frame (`kind:"codec"`) instead of raw chunks, with
  `strategy` (`zstd`/`delta`/`reference`), a `frame` blob, and a
  `dependencies` list of base content roots. v2 also permits `kind:"raw"`.
  The frozen `weft.manifest.v1` is unchanged and remains the default.
- **Selection:** `Options.representation = RepresentationMode::CodecAuto` plus
  `codec_min_bytes` (default 4096), reachable as `--representation
  raw|codec-auto` and `--codec-min-bytes` on both the CLI and `mount`. The
  default stays `raw`. The writer picks DELTA against the causal
  parent's content root only when it beats independent ZSTD by the configured
  margin; otherwise it falls back to a v2 raw manifest. There is exactly one
  v2 schema spelling (`weft.manifest.v2`); unknown schemas fail closed.
- **Read path:** range reads decode only intersecting codec chunks
  (`weftcodec::stream::read_range`), verify the frame's payload mode against
  `strategy`, bound dependency depth (8) and reject cycles, and verify the
  frozen whole-content hash on full reads. Missing/unreadable/mismatched
  dependencies fail closed; no partial or approximate reconstruction.
- **Retention:** GC treats representation dependencies as roots, so a retained
  dependent keeps its base; `verify --full` decodes representations and
  re-hashes them.
- **Measured** (8 MiB synthetic, release, rust court, host page cache; receipt
  `bench/receipts/representation-20261104T093437Z.json`): repetitive text
  8,395,250 B raw → 16,853 B ZSTD, encode 227.56 ms → 12.84 ms; near-duplicate
  8 MiB pair 8,395,250 B independent → 1,153,697 B DELTA, at the cost of
  full-object DELTA range reads (6.17 ms vs 0.05 ms raw). Incompressible
  near-duplicates correctly fell back to raw.
- **Byte-identity evidence:** with `Raw` (the default) the committed
  `conformance/store/v1` fixture still rebuilds to generation 11 and head
  `8959c65b…e1d81`, and `tests/representation.rs` asserts the v1 manifest
  layout, keys, and byte-identical manifests across two Raw stores.
- **Not claimed:** this is experimental and **Rust-only**. The Go store has no
  v2 parser and fails closed on such a manifest; the Rust→Go handoff court
  still runs on the Raw path. Cross-language v2 parity, a planner that
  chooses representations globally, and compaction/migration of existing
  objects are future work. Bench numbers are synthetic, not production data.

### Storage-path performance: extent COW and bounded open (implemented 2026-11-04)

- **Extent-level COW range writes.** `Store::write_file`/`write_file_causal` no
  longer buffer or rewrite the whole object. A range write reads the manifest
  and only the chunks it touches; untouched `ChunkRef`s are reused verbatim and
  their blobs are neither read nor rewritten. New chunks are content-addressed
  and deduplicated; a byte-identical rewrite adds zero blobs and zero
  manifests. Measured on a 64 MiB object, 64 KiB chunks, 20 × 4 KiB aligned
  overwrites (release, rust court, medians): 33.01 ms vs 69.64 ms, `rchar`
  64.1 MiB vs 128.0 MiB, peak RSS 3.8 MiB vs 131.7 MiB; `wchar` identical
  (122,431 B) because both paths already dedupe unchanged chunks. Receipt
  `bench/receipts/extent-cow-20261104T090112Z.json`. **Limit:** v1's
  whole-file `ContentId` is a hash of every byte, so the digest pass remains
  O(file size); the gain is in writes, read-verification, and memory. A
  Merkle-root content identity that would make hashing O(changed chunks) is a
  v2 format change, not claimed.
- **Bounded replay / fast open.** `recovery::load_state_bounded` starts at the
  selected head generation and walks parents by generation number, stopping at
  the newest commit whose checkpoint verifies; it does not enumerate older
  commit files. Any inconsistency falls back to the authoritative full-scan
  recovery. Measured on a 10,000-generation store with `checkpoint_interval`
  256 (release, rust court, tmpfs): open median 71.6 ms / p95 73.2 ms reading
  **18** commit files, versus 3,789.1 ms / 3,892.0 ms reading 10,001 for the
  forced full path. Receipt
  `bench/receipts/fast-open-20261104T090945Z.json`.
- **Semantics made explicit.** A bounded open records the note `bounded replay
  resumed from the checkpoint at generation N; commit files below it were not
  enumerated`, and `spec/WEFT-MODEL.md` now requires that note plus the
  fallback rules. Older-history gaps below a checkpoint are therefore not
  detected by open; `verify --full` still enumerates every commit and reports
  them. Tests: `tests/extent_cow.rs` (6) and `tests/fast_open.rs` (3),
  including corrupted-chain fallbacks and checkpoint corruption.
- **Not claimed:** these are single-host, page-cache measurements on synthetic
  fixtures, not device-level or fleet claims, and no group-commit or
  write-batching change has been made (each mutation still pays its durable
  commit).

- **Streaming I/O and the file bound.** `create_file_streaming*` and
  `append_file_streaming` read the source in 1 MiB steps, compute the frozen
  content hash incrementally, and commit once. A 256 MiB streaming create uses
  ~7 MiB peak RSS versus ~263 MiB for the buffered path (190.7 ms vs 138.4 ms;
  receipt `bench/receipts/streaming-20261104T094742Z.json`). `MAX_FILE_BYTES`
  is now a 1 TiB defensive bound plus `MAX_CHUNKS_PER_FILE = 16,777,216`; every
  offset/size conversion is checked and returns `LimitExceeded` rather than
  truncating. A 512 MiB file is created and fully verified in the court at
  ~15 MiB peak RSS (`tests/streaming.rs`). Go matches both constants with no
  format change (`WeftFS-Go` `b6408fe`).
- **Effective large-file limit (honest):** the manifest is a WCB array, so the
  codec's `max_array_items` decode limit caps readable chunk counts far below
  1 TiB (at 64 KiB chunks, ~200k chunks ≈ 12.8 GiB for the default 1,000,000
  item limit). Neither implementation truncates; both fail closed with
  `LimitExceeded`. Raising that ceiling requires a chunk-table format change
  (the codec already has a chunk table for frames) and is future work.
- **Not claimed:** codec-backed streaming (`CodecAuto` above
  `codec_min_bytes` writes a v2 raw manifest) and streaming arbitrary-offset
  writes (append is the streaming write form; range writes remain
  chunk-materialising).

### Contextual indexes for selector resolution (implemented 2026-11-04, in-memory)

- **What it is:** `src/index.rs` `StateIndex` indexes attribute presence and
  scalar equality (`objects_with_attr`, `objects_by_attr_eq`), outgoing
  relations per `(subject, predicate)`, and `tagged_as` value edges. Candidate
  sets are computed per AST node (`And` intersects, `Or` unions, `Not` uses the
  authorized-set difference only when the inner set is exact) and intersect the
  authorized universe computed before evaluation.
- **Semantics:** candidate narrowing only — `eval_expr` remains authoritative and
  confirms every candidate, so results, evidence, ordering, `matched_count`, and
  ambiguity are identical to the index-free scan. This is enforced by the
  differential court `tests/index_equivalence.rs` (3 randomized states, seeds
  11/29/47, 315 selector/view comparisons, 1,566 assertions, compared against a
  retained scan oracle).
- **Cache:** bounded 8-entry LRU keyed by the snapshot commit hash that produced
  the state; the guard fails closed with `Corrupt` if the index does not
  describe the state (never a silent under-selection).
- **Measured:** synthetic in-memory state, 10,000 objects + 10,000 relations,
  100 queries per kind, release, medians. Relation 132.795 ms → 0.689 ms warm
  (13.028 ms cold); tag 148.015 ms → 2.051 ms warm (14.044 ms cold); attr-eq
  1.640 ms → 1.239 ms warm. Receipt
  `bench/receipts/index-bench-20261104T085157Z.json`; labelled as a resolver-only
  synthetic measurement, not disk/FUSE/end-to-end.
- **Not claimed:** the index is **not persisted on disk** and is not yet
  snapshot-incremental; it is rebuilt from the replayed state on first use per
  commit. Cold resolution therefore still pays one O(objects+relations) build.
  On-disk, generation-keyed index segments and replay-skipping are future work.

### F3 — Context mounts, ambiguity court, time travel, and replay bundle (Rust + Go)

- **`mount --context NAME|weft://...`** (both frontends) makes the resolved
  Context Root's ordered, capability-authorized result set the read-only mount
  root. Entries use the established deterministic names (percent-escaped
  basename + `--` + 8 hex ObjectId, extended in 8-character steps on
  collision). `readdir` and `lookup` are fed by one helper, so they cannot
  disagree; every authorized result appears. The constant `.weft` exact-id
  surface remains reachable. Capability-scoped mounts are forced read-only and
  resolve (and therefore filter) before names or counts are produced.
- **`mount --at RFC3339|unix-ns`** (both frontends) walks the published parent
  chain of the base snapshot (or head), selects the newest commit with
  `timestamp_ns <= t` (ties: higher generation), and mounts that state
  read-only. A time before the first commit fails with
  `NotFound: no commit at or before … ns in the mounted lineage`; no state is
  substituted. `--context` pins its own snapshot, so `--context` + `--at` is
  rejected explicitly.
- **Concurrent mounts:** read-only opens take no writer lock in either
  implementation. A FUSE integration test mounts three different roots at once
  and then commits through a normal writer; the writer succeeds and the pinned
  mount does not observe the later commit.
- **Ambiguity:** `require_one` now re-evaluates without pagination and lists
  every competing ObjectId in the `Ambiguous` message. `context materialize
  --require-one` checks cardinality before any filesystem work and leaves no
  output path on failure.
- **Replay bundle:** Rust `export-bundle` writes `weft-receipt.tar.zst` with the
  portable store, the complete relation graph including Context Root records,
  `manifest.json` (ContextRootIds, snapshot/selector ids, content hashes,
  source revisions, court command), `contexts.tsv`, `checks.sha256`,
  `materialized.sha256`, `replay.sh`, and a README. Go extracts it with
  `tar`/`zstd`, opens it, runs `verify --full`, resolves and materializes every
  Context Root, and re-hashes every store entry and output file against
  `manifest.json` using a stdlib-only verifier. No Rust runs at Go replay time.
- **Evidence:** `scripts/docker-court.sh fuse "cargo test --features fuse
  --all-targets"` reports **81 passed, 0 failed, 2 ignored** (unchanged
  CAP_SETUID and kernel-7.3 limitations). `scripts/docker-court.sh fuse "cd
  /work/WeftFS-Go && go test -race ./..."` passes for `store` and `fusefs`,
  including four new context-mount/time-travel/concurrency tests and an
  ambiguity message test; `go vet ./...` and `CGO_ENABLED=0 go build ./...`
  pass.
- **Demo receipts (all `pass`):**
  - `bench/receipts/context-mounts-20261103T210625Z.json` (6/6) — three
    simultaneous mounts, per-file SHA-256 and ContentId agreement, one
    content-addressed manifest shared by two contexts, read-only denials, and a
    live writer while mounted.
  - `bench/receipts/ambiguity-20261103T210633Z.json` (4/4) — full ordered set,
    `Ambiguous` with both ids, two-result mount listing, no singular output.
  - `bench/receipts/time-travel-20261103T210633Z.json` (4/4) — 100 versions,
    snapshots 1/50/100 mounted together, exact historical bytes after a live
    rename/retag.
  - `bench/receipts/replay-bundle-20261103T210824Z.json` (4/4) — bundle
    sha256 `940579db909ba8a3360f73377448ce90df455278a37c5a0b02f6bb7a584bdbd2`,
    17 store entries, 2 contexts, 3 context files, 2 resolved identities, zero
    mismatches.
  - `bench/receipts/context-mounts-determinism.txt` — `semantic_match=true`
    between two Context-mount runs (run ids and wall-clock fields excluded).
- **Bundle:** `bench/bundles/weft-receipt.tar.zst` (6,662 bytes), sha256
  `940579db…`. Reproduction:
  `scripts/docker-court.sh fuse "bash scripts/replay-bundle.sh --bundle
  bench/bundles/weft-receipt.tar.zst --out /tmp/replay-out"`.
- **Not claimed:** io_uring, EKBLOOM, or any acceleration. Time travel is
  correctness-only. The SHA-256 value in the demos is a declared attribute and
  a byte-identity check; the normative content root is the store's BLAKE3
  ContentId, which `verify --full` re-checks. Each receipt records its exact
  revisions: the context-mount, ambiguity, and time-travel runs used WeftFS
  `f3d1bc1` / WeftFS-Go `acf6df6`; the replay run used WeftFS `bcedd26` /
  WeftFS-Go `7693ad4` (the replay-verifier path fix).

### Twelve-step showcase — implemented, all twelve pass

- `scripts/showcase.sh` executes the mandated twelve scenarios inside the FUSE
  court and writes a schema-versioned receipt plus a raw console log:
  `bench/receipts/showcase-20261103T171440Z.json` and `.log` (all twelve
  `pass`; schema `weftfs.showcase.receipt/1`).
- A second run under a different receipt name compared every field except
  wall-clock timestamps: `semantic_match=true`
  (`showcase-determinism-20261103T171440Z.determinism.txt`).
- Every step records its exact commands, fixture hashes, machine-checked
  assertions (`cmp`, SHA-256, JSON field equality), observed outputs, court
  limits, image/kernel/tool versions, and repository revisions (WeftFS
  `4cb40af`, WeftFS-Go `74c8c90`, WeftCodec `a60d9f5`, WeftCodec-Go `b2beeab`).
- Step 9 is explicitly labelled a **process-fault proxy** (`WEFT_CRASH_PHASE`),
  not physical power loss. Step 10 records that the codec CLI has no remote
  fetch policy; missing/mismatched dependencies fail closed, while
  `DependencyDenied` remains an API-level class.
- The codec-cycle fixture generator is `examples/codec_cycle_probe.rs`; the
  real `weftcodec archive create` CLI is what rejects the forged cycle.
- Deterministic fixtures use fixed ObjectId seeds and a fixed clock; a re-run
  with `SHOWCASE_COMPARE_WITH` must match on every field except wall-clock
  timestamps.

### P7 benchmark court — implemented, first measured results

- `docker/bench.Dockerfile` and `scripts/docker-court.sh bench` add a
  FUSE-capable, OOM-limited benchmark court with `zstd`, `xz`, `gzip`,
  `brotli`, `sqlite3`, `fio`, Rust, and Go. Unknown court kinds are still
  rejected and the memory/swap/pids/CPU limits are unchanged.
- The existing `fuse` court was re-run after the runner change at `244dc26`:
  `fuse_posix` reported 9 passed and 1 ignored (the ownership test needs
  `CAP_SETUID`). The added `/mnt/weft-bench` tmpfs does not change the POSIX
  result.
- Codec baselines (`bench-codec-20261103T175847Z.json`): external zstd 3/9/19,
  gzip 9, xz 9, and brotli 9 on seven deterministic corpora, plus WeftCodec
  RAW/ZSTD/DELTA/DICTIONARY/auto with marginal and whole-object accounting.
  Every decode is verified before its sample is accepted.
- Store benchmark (`bench-store-20261103T175920Z.json`): 100/1,000/10,000
  objects, 1–3 relations each, snapshots, selectors, churn, full verify, GC,
  and on-disk bytes against the real Rust store API. Peak RSS 169 MiB.
- Filesystem benchmark (`bench-fs-20261103T180751Z.json`): `fio` against a
  Rust WeftFS mount, a Go WeftFS mount, and a tmpfs reference in one container.
  The Go mount completed reliably (`go_status: ok`). The tmpfs column is
  reference-only and explicitly non-equivalent.
- Headline scoped observations (not claims of superiority):
  - WeftCodec ZSTD-3 is 545 bytes on 4 MiB repetitive data and RAW is
    4,194,404 bytes on 4 MiB random data; `auto` selects accordingly.
  - DELTA/DICTIONARY do not reduce whole-pair bytes when the required base is
    counted (near-dup 1 MiB: 1,128,560 / 1,073,309 vs 1,048,709 for ZSTD-3).
  - Store relation/tag queries cost ~414–424 ms at 10,000 objects because
    evaluation scans all relations per object; the n-result kind query costs
    1.93 ms. Replay is 32.6 s and full verify 70.3 s at 10,000 objects.
  - Filesystem writes rewrite the whole file per write: ~20 IOPS for 1 MiB
    sequential writes and ~73 IOPS for 4 KiB random writes (Rust), with
    ~7.6 ms p50 for a 4 KiB write with `fsync`.
  - A 1,000-object ingest measurement varied by 86% between runs; the range is
    reported rather than a single point.
- `bench/scripts/summarize_bench.py` regenerates the tables and
  `bench/scripts/compare_bench.py` compares two receipts for stability.
  `bench/RESULTS-v1.md` publishes every loss and the explicit gaps.

### P7 comparative pass — external SQLite, matched codec, cold-cache fio (implemented)

- External context/query baseline: `bench-external-20261104T000122Z.json`
  (plus corpus manifest `external-corpus-20261104T000122Z.json`). Schema-indexed
  SQLite relational and adjacency-table variants at 100/1,000/10,000 objects
  using the exact `bench_store` payloads, ObjectIds, relations, snapshots, and
  generations. Warm and cold (per-file page-cache eviction) samples, query
  plans, churn, index build time/bytes, and bytes on disk are recorded. The
  adjacency variant is labelled “SQLite relational/adjacency baseline”; no
  Neo4j/SPARQL/dedicated graph database was run.
- Same-revision store baseline for the comparison: `bench-store-20261103T235127Z.json`
  at HEAD `2388352` (487.9 s wall, peak RSS 169,356 KiB), 100/1,000/10,000
  objects, with the query/churn/snapshot/open/bytes medians used in
  `bench/RESULTS-comparative-v1.md` §3.4.
- Matched Rust/Go WeftCodec court: `bench-codec-matched-20261103T232158Z.json`
  (first run `T225536Z` retained for stability). RAW/ZSTD/REFERENCE/DELTA/
  DICTIONARY/TENSOR/TOKENS as CLI processes, medians and p95, CPU user/sys,
  max RSS, frame bytes, per-iteration decode verification, and cross-decode in
  both directions. Process overhead measured separately (Rust 0.326 ms, Go
  0.387 ms) and not subtracted; no library-vs-library comparison is claimed.
- Cold-cache concurrent fio: `bench-fs-cold-20261103T232216Z.json` (stability
  run `T233740Z`). QEMU guest with a genuine guest
  `sync; echo 3 > /proc/sys/vm/drop_caches` cold run, `numjobs` 1/4/16,
  ext4/XFS plus Rust and Go WeftFS over FUSE, two repetitions per cell. The
  Rust and Go mounts differ in `allow_other`/`max_read` defaults; that is
  published as a limitation.
- Whole-disk ZFS and Btrfs (2026-11-04):
  `bench-fs-cold-20261104T071922Z.json` runs ext4/XFS/Btrfs/`zfs-whole` in one
  guest court, one repetition per cell, 30 fio runs per target. For `zfs-whole`
  the guest passed the raw `/dev/vdb` block device to `zpool create -f -m
  /data/tank tank /dev/vdb`; `zpool status -P` shows the GPT partition
  `/dev/vdb1` that ZFS created and owns. No partition, loop device, or
  file-backed vdev is used. The guest runs stock `systemd-udevd 252.39` so
  libzfs' libudev partition wait can complete; without udevd the exact error is
  `cannot label 'vdb': failed to detect device partitions on '/dev/vdb1': 19`.
  OpenZFS 2.1.11-1+deb12u1 (DKMS, kernel 6.1.0-54-amd64); Btrfs uses
  btrfs-progs 6.2 on the host and the guest `btrfs.ko`. Scoped `numjobs=1` cold
  medians: seq-write-1m ext4 1,142.9, XFS 1,142.9, Btrfs 1,142.9, ZFS 1,333.3;
  fsync-write-4k ext4 410.3, XFS 390.2, Btrfs 457.1, ZFS 800.0; rand-read-4k
  ext4 9,846.2, XFS 9,846.2, Btrfs 8,533.3, ZFS 128,000 (ARC-served). At
  `numjobs=16` fsync-write-4k: ext4 3,160.5, XFS 1,855.1, Btrfs 911.0, ZFS
  6,736.8. ZFS reads are **not cold**: `drop_caches` does not evict the ARC;
  ranking or superiority claim is made. The earlier file-backed ZFS run
  `bench-fs-cold-20261104T014727Z.json` is retained as a separate, labelled
  result, not as the whole-disk claim.
- Block-layer lying-disk matrix (2026-11-04): `bench/receipts/blkdebug-20261104T072008Z.json`,
  30 iterations, 5 per mode, in the vm court. `dm-flakey drop_writes` (2 s up,
  then silent drop) lost acknowledged commits in 5/5 iterations: recovered
  generations 87/89/89/88/89 versus last returned 319/218/334/292/246
  (232/129/245/204/157 commits lost), 0 partial, both implementations agreed,
  `verify --full` clean. `dm-flakey error_writes` failed the first post-window
  write in 5/5, recovered exactly the last returned generation (88/79/8/7/6),
  0 loss, 0 partial. QEMU `snapshot=on` total write drop recovered generation 2
  in 5/5 and lost all acknowledged commits (last returned 52/77/205/182/128).
  QEMU blkdebug write-error and flush-error were attempted and are **blocked at
  ext4 mount** in 10/10: the first injected write is the mount superblock at
  block 0, then `JBD2: recovery failed` / `EXT4-fs (vdb): error loading
  journal`, and the guest never reaches `GUEST_WRITING`. No result is invented
  for those two modes. This is a block-layer lying-disk model, not physical
  media power loss.
- Host page-cache attempt (2026-11-04):
  `bench/receipts/hostcache-probe-20261104T072314Z.json`. The unprivileged court
  (`CapEff=0`) cannot write `/proc/sys/vm/drop_caches`: mode `200`, exact
  refusal `Read-only file system`; no host cache was dropped. A per-file
  `posix_fadvise(POSIX_FADV_DONTNEED)` on one 4 MiB scratch file succeeded and
  is recorded as per-file eviction only. QEMU `cache=none` uses `O_DIRECT`, so
  guest data does not occupy the host page cache on that path. Host page-cache
  loss remains **not tested**.
- Energy/counter probe: `energy-probe-20261103T225547Z.json` recorded that
  `perf` was not installed and no readable RAPL `energy_uj` existed. The
  2026-11-04 probe `perf-counters-20261104T015026Z.json` installs Debian
  `linux-perf 6.1.190-1` inside the ephemeral bench court and records
  cycles/instructions/branches/branch-misses/cache/context-switch/page-fault
  counters for RAW/ZSTD encode and decode (3 repetitions, all runs complete).
  RAPL is still unavailable: `/sys/class/powercap` exists but no `energy_uj`
  file is visible inside Docker (`/sys/devices/virtual/powercap` is masked).
  The VM guest reports `powercap present`, `energy_uj files 0`, `perf absent`.
  CPU time and perf counters are **not** called energy; no joule/watt number is
  reported.
- Analysis: `bench/RESULTS-comparative-v1.md` contains the exact commands,
  environments, revisions, tables, every loss, stability deltas, and explicit
  limits. Scoped examples: SQLite point lookups are ~1.2–1.8 µs warm at 10,000
  versus ~457.6–494.4 ms for the store’s relation/tag selectors; WeftFS
  sequential 1 MiB writes are 9.9–12.1 IOPS (Rust) and 2.4–3.2 IOPS (Go)
  versus 1,143–2,784 (ext4) and 1,333–2,702 (XFS); Rust `fsync`-write p99
  reaches 1.13 s at `numjobs=16`. No winner or world-record claim is made.

### F5 — proof courts (B1–B4)

The F5 code revision is parent `9d767d5`, `WeftCodec` `127bc85`,
`WeftCodec-Go` `71066cf`; `WeftFS-Go` remains `7693ad4` (no F5 code change).
All three F5 receipts below were generated at those revisions after the code
commit, in the unchanged hard-limited courts.

**B1 — decoder bombs (36/36 pass).** `bench/receipts/decoder-bombs-20261103T213314Z.json`
and its raw `.log`. 36 committed adversarial vectors in both codec repos
(byte-identical mirror checked by the court). Both CLIs were run twice per
vector under `timeout 10s /usr/bin/time`; every run exited nonzero with the
expected class in both implementations, no output file, no timeout, no
OOM-kill (137), stable classes across repeats, and peak RSS below 5 MiB.
Class matrix: LimitExceeded 15, Malformed 12, Corrupt 2, DependencyCycle 2,
UnsupportedFeature 2, NonCanonical 1, UnsupportedMode 1, DependencyMismatch 1.
The tiny-body/4 GiB-declared vector is the regression for the fixed zstd
preallocation bug; Rust and Go both report Corrupt with ~3.5/4.7 MiB RSS.
Limits are configurable through the ten requested `decode` flags (plus
structured-item, selector-depth/operand, and archive-entry bounds); defaults
remain the normative spec values.

**B4 — randomized POSIX torture (6/6 pass).**
`bench/receipts/posix-torture-20261103T213317Z.json`. Seeds 1–3, both
directions (Rust write/Go read and Go write/Rust read), 2,251–2,342 completed
operations per run (13,694 total) over create/write/read/truncate/append/
rename/rename-over/link/unlink/mkdir/rmdir/symlink/readlink/xattr/fsync/
open-unlinked. Every operation was checked for bytes, size, mode, nlink,
symlink target, and xattrs; a full-tree check ran every 100 operations. After
unmount the other implementation ran `verify --full`, remounted, and matched an
exact manifest including names, types, modes, uid/gid, nlink, mtime, content
sha256, symlink targets, and xattrs. This is a self-written fsx-style harness,
not `pjdfstest`/`fsstress` (not installed in the court; no substitute run is
claimed).

**B2 — crash-consistency torture (10,000/10,000 pass).**
`bench/receipts/crash-torture-20261104T013521Z.json`. The harness now uses a
fresh three-generation store per iteration, a pre-generated payload pool, and a
clean fast path for `verify --full`; the old-or-new/partial and phase-specific
assertions are unchanged (`WEFT_CRASH_FRESH_STORE=0` retains the historical
growing-store mode). Result: 10,000 iterations, 10,000 completed, 0 failures,
0 bad verify states; observed old 6,667 / new 3,333 / partial or missing 0;
verify clean 6,667 / accepted unpublished-tail 3,333; phases 3,334/3,333/3,333;
507,885 ms, **19.7 iterations/s**. Alternating Rust/Go writers and readers; every
crash followed by the other implementation's `verify --full`, exact `/f` byte
check, and exact `/` listing. This remains a **process-fault proxy**, not
physical power loss. The earlier 1,000-iteration receipt
`crash-torture-20261103T213446Z.json` (2.9 it/s) is retained as history.

**B3 — receipt bundle.** `scripts/package-receipts.sh` writes
`bench/bundles/receipts-<stamp>.tar.zst` with manifest, checksums, revisions,
receipts/raw logs, conformance corpus, harness/Dockerfiles, and dmesg status.
`scripts/verify-receipts.sh <bundle> --repo-root .` extracts to a private
temporary directory and independently verifies every sha256, rejects unlisted
files, checks all four revisions against git, and rejects unsafe tar entries.
The bundle records `dmesg: unavailable (no CAP_SYSLOG)` when applicable.

### F9 — machine-checked commit/recovery model and VM power-cut court (implemented 2026-11-03)

**Model checking.** `verification/tla/WeftCommit.tla` is a bounded TLA+
abstraction of the commit/recovery state machine: immutable commit records,
two alternating superblock slots, objects that must be durable before a commit,
arbitrary crash boundaries, recovery scanning for the highest valid generation,
and replay. TLC 1.7.4 (jar pinned by sha256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`) checks
`TypeOK`, `NoPartial`, `HighestValid`, `NoLostDependency`, `Durability`,
`ReturnedObjectsDurable`, and `ReplayIdempotent` at `MaxGen=8`: 615 states
generated, 591 distinct, search depth 68, no error. Receipt
`bench/receipts/tla-20261103T224446Z.json`.

`verification/tla/WeftCommitBuggy.tla` is deliberately flawed: recovery trusts
the highest commit file, ignoring superblock visibility and CRC validation.
TLC finds the expected `NoPartial` violation at `MaxGen=3` (32 generated, 29
distinct, depth 9). The counterexample is
`verification/tla/WeftCommitBuggy.counterexample.txt`; it shows a torn generation-1
commit replayed as an operation prefix.

**Power-loss rig.** `scripts/powerloss_court.sh` (new `vm` Docker court,
`docker/vm.Dockerfile`) builds both CLIs, a read-only raw ext4 root disk with a
Debian 6.1.0-54 kernel/initramfs, and a fresh qcow2 data overlay per iteration.
The guest initialises the store and commits 64 KiB writes in a loop without
`sync(1)`; the host waits for returned commits, sleeps a seeded pseudo-random
delay, and `SIGKILL`s QEMU. Reboot uses the same image with ext4 journal replay
and **no fsck**; Rust and Go independently recover, `verify --full`, and read
the file. The host asserts identical generation and snapshot root, exact
expected content, and that every returned commit survived. A never-fsynced
control file on the data disk distinguishes this from a process abort.

Published primary run (`cache=none`, KVM): 30/30 pass, 28 old states, 2 new
states, 0 partial, 0 durability failures, 0 Rust/Go disagreements, 1 accepted
documented unpublished tail, unsynced-control loss 30/30. Receipt
`bench/receipts/powerloss-20261103T224453Z.json`. Secondary writeback run:
10/10 pass, 0 unpublished tails, control loss 10/10; receipt
`bench/receipts/powerloss-20261103T224652Z.json`. The acceptance predicate
allows only the documented `above the highest published superblock generation`
note or an unparseable commit file strictly above the recovered generation;
all other verifier problems fail the iteration.

**Lying-fsync device model (2026-11-04).** `POWERLOSS_CACHE_MODE=unsafe` uses
QEMU's `cache=unsafe`, which accepts guest flush requests and does not honour
them. The same SIGKILL/reboot/verify matrix ran 30 iterations
(`bench/receipts/powerloss-20261104T014805Z.json`): 30/30 pass, 27 old, 3 new,
0 partial, 0 durability failures, 0 Rust/Go disagreements, unsynced-control
loss 30/30. **No data loss was observed.** That is the honest result and it has
an exact explanation: SIGKILL of QEMU discards the guest page cache and QEMU
state, but the host page cache holding QEMU's already-written image data is not
removed, so a lying guest-visible flush does not create loss under a process
kill. The mode is labelled a **lying-fsync device model**, not physical media
power loss; real disk-firmware lying and host power loss remain untested.

**Block-layer lying-disk court (2026-11-04).** `scripts/blkdebug_court.sh`
prepares one clean ext4 store base, copies it per iteration, and combines a
block-layer fault with the same SIGKILL/reboot/Rust+Go verification. Modes are
`control`, `dm-flakey-drop`, `dm-flakey-error`, `write-drop-all` (QEMU
`snapshot=on`), and two blkdebug attempts. `dm-flakey` is created inside the
guest over `/dev/vdb` with a 2 s up interval so mount completes, then a 1 h down
interval; `drop_writes` silently discards writes while returning success, and
`error_writes` returns EIO. In `bench/receipts/blkdebug-20261104T072008Z.json`:
control 5 old; dm-flakey-drop 5/5 lost (232/129/245/204/157 acknowledged
commits lost, recovered 87/89/89/88/89); dm-flakey-error 5/5 old, 5/5
COMMIT_FAIL, 0 loss; write-drop-all 5/5 recovered generation 2 and lost all
acknowledged commits; 0 partial in all completed modes. blkdebug write-error
and flush-error are 10/10 **blocked**, not passed: blkdebug arms at open, so the
first injected write is ext4's mount superblock (`I/O error, dev vdb, sector 0`,
`JBD2: recovery failed`, `EXT4-fs (vdb): error loading journal`) and the guest
never reaches `GUEST_WRITING`. QEMU 7.2 blkdebug accepts `read_aio`,
`write_aio`, and `flush_to_os`; it rejects `flush_do_sync`, and `errno=0` still
writes, so there is no silent selective write-drop primitive. `dm-flakey` is
the arm-after-mount equivalent used for the data results.

**Host page-cache probe (2026-11-04).**
`bench/receipts/hostcache-probe-20261104T072314Z.json` records that the
unprivileged court (`CapEff=0`) cannot write `/proc/sys/vm/drop_caches` (mode
`200`, exact refusal `Read-only file system`), so no host cache was dropped. A
per-file `posix_fadvise(POSIX_FADV_DONTNEED)` on one 4 MiB scratch file
succeeded and is labelled per-file eviction only. QEMU `cache=none` uses
`O_DIRECT` for guest data. Host page-cache loss remains untested.

**Claim boundary.** This is a QEMU VM power-cut proxy plus a lying-fsync device
model and a block-layer lying-disk model, not physical media power loss or host
page-cache loss. The TLA+ model is checked; the implementation is not verified,
and there is no refinement proof. See `verification/README.md` for the exact
mapping, assumptions, and remaining gaps (checkpoints, GC, capabilities,
multi-writer, physical rig).

## 3. Evidence discipline

- Every build/test ran through `scripts/docker-court.sh` in a minimal official
  image with `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
  bounded `/tmp`, `no-new-privileges`, and `cap-drop=ALL` (FUSE adds only
  `/dev/fuse` and `CAP_SYS_ADMIN`).
- `scripts/verify-all.sh` re-ran all eighteen courts and produced
  `bench/receipts/verify-20261104T022254Z.json` with revisions, host, command
  log, and per-court results.
- The P7 benchmark court ran codec, store, and filesystem benchmarks under the
  same hard limits. Every receipt contains raw samples, environment, and
  revisions. The codec receipt also records corpus hashes and per-tool command
  templates; the filesystem receipt embeds the full `fio` command log. The
  exact court commands are recorded in `bench/RESULTS-v1.md` §2. The results
  are single-host observations; `bench/RESULTS-v1.md` states the losses and
  gaps and makes no headline performance claim.
- Crash tests are process-fault proxies, not physical power-loss tests.
- The TLA+ **abstraction** is model-checked by bounded TLC runs. The Rust and
  Go implementations are **not** formally verified; there is no refinement
  proof. Implementation invariants remain test- and receipt-backed, and
  checkpoint/GC/capability/selector/multi-writer behaviour is outside the
  checked model.
- The F9 power-loss court is a QEMU virtual-machine power-cut proxy: `SIGKILL`
  loses the guest page cache and QEMU user-space state, not physical media or
  host/device caches. The block-layer court adds dm-flakey silent write drop,
  dm-flakey EIO, and a snapshot total-write-drop model; blkdebug write/flush
  errors are recorded as blocked at mount. It runs under the same hard limits as
  every other court.

## 4. What is **not** done (ranked)

1. **Remaining benchmark gaps** (P7): the comparative pass closed the external
   SQLite, ext4/XFS cold-cache, concurrent-fio, matched Go/Rust codec, and now
   whole-disk ZFS/Btrfs gaps (`bench/RESULTS-comparative-v1.md`). The 2026-11-04
   pass adds `bench-fs-cold-20261104T071922Z.json` (whole-disk `zfs-whole` on raw
   `/dev/vdb` plus Btrfs), `blkdebug-20261104T072008Z.json` (dm-flakey lying-disk
   matrix; blkdebug mount blocker), and `hostcache-probe-20261104T072314Z.json`
   (global cache drop refused). Still open: physical media power loss, host/
   device cache loss, RAPL/joule energy, direct-I/O, metadata storms, AI-corpus
   external baselines, 10^5–10^6-object scaling, and a library-vs-library codec
   court. See `bench/RESULTS-v1.md` §8 and `bench/RESULTS-comparative-v1.md` §8.
2. **Formal model coverage beyond the commit/recovery abstraction.** The F9 TLA+
   model checks bounded commit/recovery safety, durability, and replay
   idempotence, but not checkpoints, GC, capability filtering, selector
   semantics, multi-writer concurrency, or a refinement proof to Rust/Go.
3. **Showcase re-run at a later revision.** The committed receipt is evidence
   for the exact revision it records; re-running after code changes requires a
   new receipt, not an inference from the old one.
4. **FUSE binding limits in Rust.** `fuser 0.15.1` has no `FUSE_TMPFILE`
   dispatch (7.37) and drops `lk_flags`, so Rust cannot express `O_TMPFILE` or
   distinguish flock from POSIX byte-range locks; both are explicit boundaries
   tested in `tests/fuse_verb_matrix.rs`, with kernel-local locking as the
   fallback. Go implements both. COLLAPSE/INSERT store code exists in both, but
   kernel 7.3 rejects those flags before forwarding. `ioctl` and cross-daemon
   lock coordination remain absent.
5. **Same-inode concurrent direct FUSE I/O** on kernel 7.3: blocked on the
   kernel/fuser EIO interaction; no workaround is claimed. A same-description
   write after a shared `mmap` also returned EIO once; the matrix test uses a
   fresh description. Shared mmap itself (map, write, `msync`, exact read) passes.
6. **Physical power-loss court** (real media/device write-cache loss), host
   page-cache loss, and real disk firmware that lies about flushes; the F9 QEMU
   VM proxy and its `cache=unsafe` lying-fsync device model are not physical.
   Remote/object-storage backend also remains unimplemented and unclaimed.
7. **Encryption, multi-writer, distributed consistency**; explicitly out of
   scope for v1 claims.
8. **Streaming codec API for files larger than memory**; chunked/range APIs are
   real, but the CLI materializes whole inputs within limits.
9. **atime/ctime persistence**, device/FIFO/socket nodes, and Rust `O_TMPFILE`;
   not implemented. `mknod` regular files and FIFO/device rejection are
   implemented and tested in both frontends; Go implements `tmpfile`.
10. **Spec-to-Go export automation in CI**; `scripts/export-spec.sh` and
    `scripts/gen-types.sh --check` exist, but no CI workflow pins them. The
    drift court runs only when `verify-all` is invoked.
11. **F3 time travel is correctness-only.** There is no io_uring, EKBLOOM, or
    other acceleration; none is implemented or claimed. The 100-version demo
    measures exactness and pinning, not latency.
12. **Long-horizon agent memory (D2) is simulated, not measured.** Typed edges,
    task/model/time mounts, a January snapshot, September writes, and a
    months-later bundle replay pass with exact bytes and lineage. The timestamps
    are fixture nanoseconds and the replay is a point-in-time export; no real
    months of wall-clock retention, no background aging, and no physical media
    aging were tested.
13. **The Buck2 adapter is a prototype.** It is a local CAS service plus a
    Buck2-compatible key/digest mapping, not a Buck2 plugin, worker, or REAPI
    gRPC backend. It does not execute actions, fetch remotely, or integrate with
    Buck2's scheduler.
13. **Replay bundle scope.** The bundle carries one store and its Context
    Roots; it does not yet carry a filtered/partitioned subset, remote fetch,
    or a signature. `tar` and `zstd` are required tools and a missing tool is a
    hard error (no uncompressed fallback).
14. **Context mount root adds a constant `.weft` entry** for exact-id access;
    it is not part of the Context Root result set. Listing digests in the demo
    exclude that constant name explicitly.

## 5. Next work queue (dependency order)

1. Close the remaining P7 gaps in `bench/RESULTS-comparative-v1.md` §8:
   physical media power loss, host/device cache loss, RAPL/joule energy,
   direct I/O, metadata storms, AI-corpus external baselines, 10^5–10^6-object
   scaling, and a library-vs-library codec court. The whole-disk ZFS/Btrfs and
   block-layer lying-disk gaps are closed within their stated models; blkdebug
   write/flush injection remains blocked at mount and is recorded. Then add the
   eight-component economic profile.
2. Extend the TLA+ model to checkpoints, GC reachability, capability
   filtering, and multi-writer refusal; add a refinement mapping to the Rust
   and Go state machines. The current F9 model covers commit/recovery only.
3. Physical power-loss on hardware with controllable power (and host/device
   cache loss); today's F9 evidence is a QEMU VM power-cut proxy plus software
   block-layer lying-disk models (`scripts/powerloss_court.sh`,
   `scripts/blkdebug_court.sh`). Re-run both at each release revision.
4. Remaining Go FUSE edge work: physical-space PUNCH_HOLE or sparse extents,
   kernel-forwarded COLLAPSE/INSERT, `ioctl`, and cross-daemon/host locks.
   Online GC is implemented and opt-in.
5. F3 follow-ups: partitioned replay bundles, a long-horizon agent-memory
   retention measurement, and any future acceleration (none is claimed now).
6. Contextual-index follow-ups: persist generation-keyed index segments so a
   cold open does not rebuild from replay, make index maintenance incremental
   per commit, add range/ordered indexes for `lt/le/gt/ge`, and re-measure at
   10^5–10^6 objects. The current index is in-memory and rebuild-on-first-use.
7. `v0.3.0` was released (per-repo GHCR images, crates.io crates, GitHub
   releases). The next versioned release should be cut only after items 1–6
   either close or are explicitly recorded as accepted gaps with receipts.
