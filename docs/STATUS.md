# Implementation status — 2026-11-03

This is the honest, evidence-backed state of the program. It distinguishes
implemented, tested, ignored, and not done. The current verification receipt is
`bench/receipts/verify-20261103T220311Z.json` (all fourteen courts `pass`,
including F6 tensor/tokens cross-decode and spec drift). Earlier
`verify-20261103T172416Z` and `verify-20261103T165944Z` receipts are retained as
history. The
twelve-step showcase receipt is
`bench/receipts/showcase-20261103T171440Z.json` (twelve of twelve `pass`).
The P7 benchmark receipts are `bench/receipts/bench-codec-20261103T175847Z.json`,
`bench/receipts/bench-store-20261103T175920Z.json`, and
`bench/receipts/bench-fs-20261103T180751Z.json`; the analysis, losses, and
gaps are in `bench/RESULTS-v1.md`. The F2 receipt is
`bench/receipts/f2-20261103T205045Z.json` with its raw log beside it. The F3
receipts are `bench/receipts/context-mounts-20261103T210625Z.json` (6/6),
`bench/receipts/ambiguity-20261103T210633Z.json` (4/4),
`bench/receipts/time-travel-20261103T210633Z.json` (4/4), and
`bench/receipts/replay-bundle-20261103T210824Z.json` (4/4), each with its raw
`.log` and `.results.tsv`; the determinism comparison is
`bench/receipts/context-mounts-determinism.txt` (`semantic_match=true`).
The F6 cross-language gate is `WeftCodec-Go/hack/cross.sh` (tensor/tokens
byte-identical in both directions). The F7 final receipt is
`bench/receipts/ai-corpus-20261103T220113Z.json` with cost graph JSON/CSV beside
it; analysis, losses, and caveats are in `bench/RESULTS-ai-v1.md`.
The F9 TLA+ receipt is `bench/receipts/tla-20261103T223957Z.json` (corrected
model pass; flawed model counterexample). The F9 power-loss receipts are
`bench/receipts/powerloss-20261103T224003Z.json` (30/30, `cache=none`) and
`bench/receipts/powerloss-20261103T224205Z.json` (10/10, `cache=writeback`).
The abstraction mapping, assumptions, and limitations are in
`verification/README.md`.

## 1. Repositories and revisions

| Repo | F9 revision | Role |
|---|---|---|
| `WeftFS` | `12acedd` | program home, Rust store + FUSE, specs, ADRs, courts |
| `WeftCodec` | `ab50e8b` | Rust codec v1 |
| `WeftCodec-Go` | `ce2cff9` | independent Go codec |
| `WeftFS-Go` | `209e4c3` | independent Go store + FUSE |

All work is on `staging`. `main` holds the P0 foundation only. The research
directory and `AGENTS.md` are gitignored, per operator directive.

## 2. What is implemented and tested

### WeftCodec (Rust) — implemented

- Exact WFT1 framing: 96-byte header, 48-byte dependency/chunk entries, header
  and trailing CRC32C, checked lengths, reserved-bit rejection.
- Domain-separated BLAKE3, minimal LEB128, canonical WCB with encoded-key map
  ordering and non-canonical rejection.
- RAW, ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED, TENSOR, TOKEN_SEQUENCE;
  chunked RAW/ZSTD; `read_range` decodes only intersecting chunks.
- Resolver + frame-DAG decoding with cycle detection and depth/fetch/decoded
  bounds; WFA1 self-contained archives.
- WeftQL parser, normalization, canonical selector bytes, SelectorId.
- CLI (`encode`, `decode`, `inspect --json`, `verify`, `archive`, `selector`,
  `conformance`), including `--mode tensor` and `--mode tokens`.
- **110 tests pass** in the Rust court. Frozen vectors: 49 codec (including 20
  AI-mode valid/reject vectors), 20 WCB, 22 selector, 16 context, 10 causal,
  and 36 adversarial.

### WeftCodec-Go — implemented, independent

- Native Go implementation of the same v1 bytes (no cgo, no Rust calls).
- **57 top-level tests pass**, including Rust-vector replay and malformed-input
  classes; `go vet` and a CGO-disabled build pass.
- Cross-language gates pass in both directions for RAW, ZSTD, REFERENCE, DELTA,
  DICTIONARY, chunked, STRUCTURED, TENSOR, and TOKENS. RAW/REFERENCE/DELTA/
  STRUCTURED/TENSOR/TOKENS frames are byte-identical; ZSTD/DICTIONARY are
  decode-equivalent by spec.

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
- **Not implemented and not allocated a wire value:** blocked/column-major/
  sharded/quantized layouts, activation/gradient streams, embedding/KV-cache
  pages, attention masks, vocabulary-artifact binding, weight-delta descriptors,
  and semantic/structured deltas. They are `specified, not implemented` in
  `spec/WEFT-AI-CODEC.md` §4.
- Cross-language: Rust→Go and Go→Rust decode gates pass; TENSOR and TOKENS
  frames are byte-identical. Historical `017_unknown_mode` (mode 6 reserved) is
  retained byte-for-byte and marked superseded by mode 8's reject vector.

### F7 — AI corpus and cost graph (implemented, synthetic)

- Committed deterministic corpus `bench/corpora/ai-v1/`: 60 files, 1,939,911
  source bytes, 1,516,737 visible bytes; prompts, JSON/text tool outputs,
  near-duplicate agent intermediates, 12 typed tensors, 8 token sequences, and
  near-duplicate checkpoint-like blobs. License CC0-1.0, synthetic provenance,
  per-file SHA-256 manifest.
- Receipt `bench/receipts/ai-corpus-20261103T220113Z.json` + cost graph JSON/CSV
  beside it. Analysis, every loss, and caveats in `bench/RESULTS-ai-v1.md`.
- Measured physical bytes: `zstd_forced` 575,808; `weft_auto` 575,713;
  `cdc_zstd` 603,094 (+4.74%); `weft_context` 176,564 (−69.34% vs forced zstd).
  COLD chain p95 269,625 ns vs 30,126 ns for `weft_auto`. Typed TENSOR/TOKENS
  modes were evaluated for all 20 typed objects and chosen 0 times; descriptor
  overhead 272–288 bytes. No superiority claim.

### WeftFS (Rust) — implemented core + FUSE (with documented limitations)

- Portable store: `FORMAT`, `LOCK`, dual CRC32C superblock slots, `WFC1`
  commits, canonical commit/checkpoint schemas, immutable chunks/manifests,
  snapshots, relations, tips, xattrs, saved contexts.
- Atomic publication with fsync ordering; single-writer `flock`; generation
  fallback; recovery; full verify; mark-and-sweep GC; checkpoints; selector
  evaluation with POSIX + `weft.acl.read` authorization; ambiguity errors.
- FUSE: lookup/getattr/setattr/open/create/read/write/flush/fsync/release,
  readdir(+plus), mkdir/rmdir/unlink/rename(+NOREPLACE/EXCHANGE)/link/symlink/
  readlink, xattrs, statfs, access, fallocate(mode 0), O_APPEND/O_TRUNC,
  sparse writes, open-unlinked, snapshot mounts, `/.weft/id/<hex>`, saved
  context views.
- Deterministic store fixture `conformance/store/v1/` (head
  `8959c65b...`) with `EXPECTED.json`.
- **42 core tests pass** (CLI 4, crash proxy 3, recovery 11, store core 11,
  extensions 10, bounded recovery 3). `tests/bounded_recovery.rs` enumerates
  every one of generations 3–5 corrupted in three ways (empty, half, bit flip)
  and every superblock-slot corruption, asserting the previous whole state and
  replay idempotence. This is bounded enumeration, **not** TLA+/Alloy model
  checking.
- **54 FUSE tests pass, 0 fail, 2 ignored**:
  - ownership test needs `CAP_SETUID` (court has only `CAP_SYS_ADMIN`);
  - same-inode concurrent direct read/write can return `EIO` on kernel 7.3
    with fuser 0.15.1. The daemon logs successful replies and the failing
    request never arrives; rate depends on timing. This is documented, with
    the exact reproducer, in `docs/fuse-status.md`. Store-level atomicity is
    tested separately.

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
  `max_work` is deterministic objects-inspected plus relation-edges.
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

**B2 — crash-consistency torture (1/1 pass).**
`bench/receipts/crash-torture-20261103T213446Z.json`. 1,000 iterations through
`WEFT_CRASH_PHASE`, alternating Rust/Go writers and readers. Every crash was
followed by the other implementation's `verify --full` (accepting only the
documented unpublished-tail note), an exact `/f` byte check, and an exact `/`
listing. Observed old 667, new 333, partial/missing 0; verify clean 667,
unpublished-tail 333, bad 0; phases 334/333/333; 350,541 ms at 2.9 it/s. This
is a **process-fault proxy**, not physical power loss. The documented 10,000-
iteration command is supported; linear projection is 3,505 s (~58.4 min), and
the actual cost is higher because `verify --full` rescans a growing commit
chain each iteration.

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
`bench/receipts/tla-20261103T223957Z.json`.

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

Published primary run (`cache=none`, KVM): 30/30 pass, 26 old states, 4 new
states, 0 partial, 0 durability failures, 0 Rust/Go disagreements, 1 accepted
documented unpublished tail, unsynced-control loss 30/30. Receipt
`bench/receipts/powerloss-20261103T224003Z.json`. Secondary writeback run:
10/10 pass, 2 accepted unpublished tails, control loss 10/10; receipt
`bench/receipts/powerloss-20261103T224205Z.json`. The acceptance predicate
allows only the documented `above the highest published superblock generation`
note or an unparseable commit file strictly above the recovered generation;
all other verifier problems fail the iteration.

**Claim boundary.** This is a QEMU VM power-cut proxy, not physical media power
loss, host page-cache loss, or an fsync that lies. The TLA+ model is checked;
the implementation is not verified, and there is no refinement proof. See
`verification/README.md` for the exact mapping, assumptions, and remaining
gaps (checkpoints, GC, capabilities, multi-writer, physical rig).

## 3. Evidence discipline

- Every build/test ran through `scripts/docker-court.sh` in a minimal official
  image with `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
  bounded `/tmp`, `no-new-privileges`, and `cap-drop=ALL` (FUSE adds only
  `/dev/fuse` and `CAP_SYS_ADMIN`).
- `scripts/verify-all.sh` re-ran all nine courts and produced
  `bench/receipts/verify-20261103T165944Z.{json,log}` with revisions, host,
  command log, and per-court results.
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
  host/device caches. It runs under the same hard limits as every other court.

## 4. What is **not** done (ranked)

1. **Remaining benchmark gaps** (P7): no external SQLite/graph-store baseline,
   no ext4/XFS/Btrfs/ZFS baseline, no physical power-loss or energy
   measurement, no cold-cache/direct-I/O filesystem run, no metadata-storm or
   concurrency sweep, and no matched Go/Rust codec performance court. The
   codec/store/fio benchmarks themselves ran to completion in this pass; see
   `bench/RESULTS-v1.md` §8 for the exact list.
2. **Formal model coverage beyond the commit/recovery abstraction.** The F9 TLA+
   model checks bounded commit/recovery safety, durability, and replay
   idempotence, but not checkpoints, GC, capability filtering, selector
   semantics, multi-writer concurrency, or a refinement proof to Rust/Go.
3. **Showcase re-run at a later revision.** The committed receipt is evidence
   for the exact revision it records; re-running after code changes requires a
   new receipt, not an inference from the old one.
4. **WeftFS-Go verb edge cases** remain: physical-space `PUNCH_HOLE`,
   kernel-forwarded COLLAPSE/INSERT (store code exists; 7.3.1 rejects before
   forwarding), `ioctl`, and cross-daemon/host lock coordination. Online GC is
   implemented and opt-in.
5. **Same-inode concurrent direct FUSE I/O** on kernel 7.3: blocked on the
   kernel/fuser EIO interaction; no workaround is claimed.
6. **Physical power-loss court** (real media/device write-cache loss) and
   host page-cache loss; the F9 QEMU VM proxy is not physical. Remote/object-
   storage backend also remains unimplemented and unclaimed.
7. **Encryption, multi-writer, distributed consistency**; explicitly out of
   scope for v1 claims.
8. **Streaming codec API for files larger than memory**; chunked/range APIs are
   real, but the CLI materializes whole inputs within limits.
9. **atime/ctime persistence**, `mmap` writeback, and device/FIFO/socket
   nodes; not implemented. `mknod` regular files and `tmpfile` are implemented.
10. **Spec-to-Go export automation in CI**; `scripts/export-spec.sh` exists but
    no CI workflow pins the export.
11. **F3 time travel is correctness-only.** There is no io_uring, EKBLOOM, or
    other acceleration; none is implemented or claimed. The 100-version demo
    measures exactness and pinning, not latency.
12. **Long-horizon agent memory (D2) is partial.** Typed edges and task/model/
    time mounts pass, but “months-later” retention and a multi-month corpus are
    not measured. The demo covers 100 versions and a rename/retag within one
    store.
13. **Replay bundle scope.** The bundle carries one store and its Context
    Roots; it does not yet carry a filtered/partitioned subset, remote fetch,
    or a signature. `tar` and `zstd` are required tools and a missing tool is a
    hard error (no uncompressed fallback).
14. **Context mount root adds a constant `.weft` entry** for exact-id access;
    it is not part of the Context Root result set. Listing digests in the demo
    exclude that constant name explicitly.

## 5. Next work queue (dependency order)

1. Close the P7 benchmark gaps in `bench/RESULTS-v1.md` §8: external
   SQLite/graph-store baselines, an ext4/XFS/ZFS court, cold-cache and
   direct-I/O runs, metadata storms, concurrency, and a matched Go/Rust codec
   court. Then add the eight-component economic profile.
2. Extend the TLA+ model to checkpoints, GC reachability, capability
   filtering, and multi-writer refusal; add a refinement mapping to the Rust
   and Go state machines. The current F9 model covers commit/recovery only.
3. Physical power-loss on hardware with controllable power (and host/device
   cache loss); today's F9 evidence is a QEMU VM power-cut proxy. The VM rig is
   `scripts/powerloss_court.sh`; it should be re-run at each release revision.
4. Remaining Go FUSE edge work: physical-space PUNCH_HOLE or sparse extents,
   kernel-forwarded COLLAPSE/INSERT, `ioctl`, and cross-daemon/host locks.
   Online GC is implemented and opt-in.
5. F3 follow-ups: partitioned replay bundles, a long-horizon agent-memory
   retention measurement, and any future acceleration (none is claimed now).
6. Only then consider a `staging` → `main` merge and a versioned release.
