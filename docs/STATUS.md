# Implementation status — 2026-11-03

This is the honest, evidence-backed state of the program. It distinguishes
implemented, tested, ignored, and not done. The machine-readable receipt is
`bench/receipts/verify-20261103T165944Z.json` (all nine courts `pass`).

## 1. Repositories and revisions

| Repo | Staging HEAD | Role |
|---|---|---|
| `WeftFS` | `66fbf3e` (+ this commit) | program home, Rust store + FUSE, specs, ADRs, courts |
| `WeftCodec` | `a60d9f5` | Rust codec v1 |
| `WeftCodec-Go` | `b2beeab` | independent Go codec |
| `WeftFS-Go` | `9d9e386` | independent Go store + FUSE |

All work is on `staging`. `main` holds the P0 foundation only. The research
directory and `AGENTS.md` are gitignored, per operator directive.

## 2. What is implemented and tested

### WeftCodec (Rust) — implemented

- Exact WFT1 framing: 96-byte header, 48-byte dependency/chunk entries, header
  and trailing CRC32C, checked lengths, reserved-bit rejection.
- Domain-separated BLAKE3, minimal LEB128, canonical WCB with encoded-key map
  ordering and non-canonical rejection.
- RAW, ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED; chunked RAW/ZSTD;
  `read_range` decodes only intersecting chunks.
- Resolver + frame-DAG decoding with cycle detection and depth/fetch/decoded
  bounds; WFA1 self-contained archives.
- WeftQL parser, normalization, canonical selector bytes, SelectorId.
- CLI (`encode`, `decode`, `inspect --json`, `verify`, `archive`, `selector`,
  `conformance`).
- **87 tests pass** in the Rust court. Frozen vectors: 29 codec, 20 WCB,
  22 selector.

### WeftCodec-Go — implemented, independent

- Native Go implementation of the same v1 bytes (no cgo, no Rust calls).
- **44 tests pass**, including Rust-vector replay and malformed-input classes.
- Cross-language gates pass in both directions for RAW, ZSTD, REFERENCE, DELTA,
  DICTIONARY, chunked, and STRUCTURED. RAW/REFERENCE/DELTA/STRUCTURED frames are
  byte-identical; ZSTD/DICTIONARY are decode-equivalent by spec.

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
- **39 core tests pass** (CLI 4, crash proxy 3, recovery 11, store core 11,
  extensions 10).
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
  `/.weft/id/<hex>`, and saved-context views.
- **31 top-level tests + 5 subtests pass** under `-race`, `go vet`, and
  `CGO_ENABLED=0 go build`.
- Rebuilding the Rust fixture through the Go writer reproduces the same head
  and byte-identical `FORMAT`, `superblock`, commits, manifests, chunks, and
  snapshot pointer.
- Bidirectional handoff gates pass: Rust fixture → Go mutations → Rust verify;
  Go init/mutate → Rust verify/mount; alternating generations; snapshots and
  checkpoints both ways; crash proxy through both.
- Missing: GC, byte-range locking (ENOSYS), mmap writeback, mknod/tmpfile/
  fallocate/copy_file_range/SEEK_DATA, persisted atime/ctime.

### Twelve-step showcase — implemented, all twelve pass

- `scripts/showcase.sh` executes the mandated twelve scenarios inside the FUSE
  court and writes a schema-versioned receipt plus a raw console log:
  `bench/receipts/showcase-<UTC>.json` and `.log`.
- Every step records its exact commands, fixture hashes, machine-checked
  assertions (`cmp`, SHA-256, JSON field equality), observed outputs, court
  limits, image/kernel/tool versions, and repository revisions.
- Step 9 is explicitly labelled a **process-fault proxy** (`WEFT_CRASH_PHASE`),
  not physical power loss. Step 10 records that the codec CLI has no remote
  fetch policy; missing/mismatched dependencies fail closed, while
  `DependencyDenied` remains an API-level class.
- The codec-cycle fixture generator is `examples/codec_cycle_probe.rs`; the
  real `weftcodec archive create` CLI is what rejects the forged cycle.
- Deterministic fixtures use fixed ObjectId seeds and a fixed clock; a re-run
  with `SHOWCASE_COMPARE_WITH` must match on every field except wall-clock
  timestamps.

## 3. Evidence discipline

- Every build/test ran through `scripts/docker-court.sh` in a minimal official
  image with `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
  bounded `/tmp`, `no-new-privileges`, and `cap-drop=ALL` (FUSE adds only
  `/dev/fuse` and `CAP_SYS_ADMIN`).
- `scripts/verify-all.sh` re-ran all nine courts and produced
  `bench/receipts/verify-20261103T165944Z.{json,log}` with revisions, host,
  command log, and per-court results.
- No performance comparison has been run. `bench/CONTRACTS.md` fixes the
  methodology before results exist. No throughput/space/energy claim is made.
- Crash tests are process-fault proxies, not physical power-loss tests.
- No property is claimed model-checked. Invariants are tested.

## 4. What is **not** done (ranked)

1. **Benchmark courts and receipts** (P7): codec/context/filesystem baselines
   and the eight-component economic profile are specified but unrun.
2. **Formal model checking** (TLA+/Alloy/Apalache or equivalent) for snapshot
   atomicity, replay idempotence, and no-dangling-roots. Current evidence is
   tests and crash proxies.
3. **Showcase re-run at a later revision.** The committed receipt is evidence
   for the exact revision it records; re-running after code changes requires a
   new receipt, not an inference from the old one.
4. **Go WeftFS GC** and the remaining unsupported FUSE verbs; Go crash court
   exists but GC/retention parity does not.
5. **Same-inode concurrent direct FUSE I/O** on kernel 7.3: blocked on the
   kernel/fuser EIO interaction; no workaround is claimed.
6. **Physical power-loss court** and remote/object-storage backend; not
   implemented, not claimed.
7. **Encryption, multi-writer, distributed consistency**; explicitly out of
   scope for v1 claims.
8. **Streaming codec API for files larger than memory**; chunked/range APIs are
   real, but the CLI materializes whole inputs within limits.
9. **atime/ctime persistence** and `mknod`/FIFO/device nodes; not implemented.
10. **Spec-to-Go export automation in CI**; `scripts/export-spec.sh` exists but
    no CI workflow pins the export.

## 5. Next work queue (dependency order)

1. Write the showcase script and run it in the FUSE court; emit a receipt for
   all twelve scenarios.
2. Implement Go GC parity and run the alternating-generation GC handoff.
3. Add the benchmark harness per `bench/CONTRACTS.md` and publish negative
   results before any headline comparison.
4. Add bounded model checking for the commit/replay state machine and link the
   exact model revision in this document.
5. Only then consider a `staging` → `main` merge and a versioned release.
