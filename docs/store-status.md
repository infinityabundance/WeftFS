# WeftFS P4 store status — Rust transactional core (no FUSE)

**Date:** 2026-11-03
**Scope:** parent repository `/mnt/1tb_kingston/WeftFS` only. The `WeftCodec`,
`WeftCodec-Go`, and `WeftFS-Go` submodules were not modified.
**Claim level:** implemented and tested on the stated host/kernel through the
Docker court. No property is claimed model-checked. No performance claim is
made. Power-loss claims are limited to the underlying filesystem's documented
`fsync` contract; the crash tests are **process-fault proxies**, not physical
power loss.

> **P5 addendum (2026-11-03).** The store gained the extensions the FUSE layer
> needs: `rmdir`, POSIX rename replacement, `RENAME_NOREPLACE`/`RENAME_EXCHANGE`,
> xattr APIs, saved contexts, open-handle object/content pins, `nlink`, and
> `statfs`. Those are covered by `tests/store_extensions.rs` and described in
> [`docs/fuse-status.md`](fuse-status.md) §10. Items in §5 below that name
> `rmdir`, directory replacement, xattr verbs, or open-handle pins are
> superseded by that addendum; the P4 observations and risks remain otherwise
> unchanged.

> **Storage-backend addendum (subphase C, 2026-11-04).** All durable I/O now
> goes through `StorageBackend` (`src/backend.rs`). `LocalBackend` is the
> normative POSIX layout and remains the default, byte-for-byte. `ObjectBackend`
> runs the same commit protocol over an immutable object namespace published
> with `fsync` + `link(2)` (no rename replacement). `BackendKind::Posix {
> capabilities }` lets a caller declare weaker guarantees; writable mode refuses
> when `durable_fsync` is absent and every publication refuses when
> `atomic_publish` is absent. `tests/backend.rs` builds the same seeded fixture
> over both backends and requires identical logical state, commit bytes, head
> hash, and full verification; it also replays the extension operations over the
> object backend, proves capability refusals, and runs the process-fault crash
> proxy over the object backend. Bench receipt:
> `bench/receipts/backend-20261104T232908Z.json` (1,000 creates + 100 MiB, 3,612
> publications each, same head, physical bytes identical, 3,612 `link(2)`
> publications for the object backend and 0 rename fallbacks). The release
> binary
> selects it with `--backend local|object`. A 3FS namespace is selectable as
> `--backend 3fs` (POSIX) or `--backend 3fs --threefs-mode usrbio`; see
> `docs/THREEFS.md`. **No 3FS deployment was run**, so no 3FS throughput,
> latency, RDMA, durability, or multi-node claim is made; the POSIX transport is
> byte-identical to local and the USRBIO transport is exercised against a
> conformance library. Spec: `spec/WEFT-MODEL.md` §4.1.

> **S3 backend addendum (subphase Q4, 2026-11-05).** `S3Backend` implements the
> same `StorageBackend` contract over a real S3-compatible endpoint using SigV4
> and conditional writes. An S3 store and a local store built from the same
> seeded operations publish **byte-identical** commit, manifest, chunk, index,
> and snapshot-ref keys; `tests/s3_backend.rs` proves this against a live MinIO
> server. Writable open requires the endpoint to honour `If-None-Match: *` and
> `If-Match`; a server that ignores either form is refused with
> `UnsupportedVersion` rather than silently overwriting a head. No AWS, GCS,
> 3FS, cloud, multi-node, media, or power-loss claim is made.

### 0.1 Backend matrix

| Backend | Selection | Publication primitive | Range reads | Writer lock | Court |
|---|---|---|---|---|---|
| `LocalBackend` | `--backend local` (default) | `tmp` + `fsync` + `rename(2)` + dir `fsync` | yes | `flock(2)` on `LOCK` | all existing courts |
| `ObjectBackend` | `--backend object` | `tmp` + `fsync` + `link(2)` + unlink + dir `fsync` (rename fallback stated) | yes | `flock(2)` on `LOCK` | `tests/backend.rs` |
| `S3Backend` | `--backend s3` + `--s3-*` | `PUT` `If-None-Match: *`; superblock `GET` + `PUT` `If-Match` | yes | `LOCK` object, owner + lease, `If-Match` reclaim | `scripts/s3_court.sh` |
| `Posix` | caller-declared capabilities | whatever the declaration says (enforced) | declared | `flock(2)` | capability-refusal tests |
| `ThreeFs` (posix) | `--backend 3fs` | `tmp` + `fsync` + `rename(2)` + dir `fsync` through the 3FS mount | yes | `flock(2)` on `LOCK` | `tests/threefs.rs` |
| `ThreeFs` (usrbio) | `--backend 3fs --threefs-mode usrbio` | same publication; staging filled through hf3fs rings | yes (rings) | `flock(2)` on `LOCK` | `scripts/threefs_court.sh` (conformance library, not 3FS) |

S3 key mapping is the local logical layout, not a private encoding:
`FORMAT`, `superblock` (the same two 4096-byte slots), `LOCK`,
`commits/<gen>.commit`, `objects/<ab>/<hash>`, `manifests/<ab>/<hash>`,
`indexes/<ab>/<commit>.idx`, `refs/snapshots/<name>`, `refs/tags/<name>`,
`quota`, and `quota.migrated`.

### 0.2 S3 head publication

1. Content, manifests, commit records, indexes, refs, and quota use
   `PUT` with `If-None-Match: *`. A `412` means the key exists: identical bytes
   are idempotent, different bytes are `Exists` (content addresses make the
   latter a real inconsistency).
2. The `superblock` object is 8192 bytes and holds the same two slots as the
   local layout. `write_at` reads the whole object (including its ETag), patches
   the inactive slot, and publishes the whole object with `If-Match: <ETag>`.
   A lost race returns `412`; the writer re-reads and retries a bounded number
   of times, then fails `Busy`. The previous head is never overwritten blindly.
3. Every S3 `write_at` is a whole-object rewrite because object stores have no
   in-place range write. This preserves the two-slot protocol, not a physical
   in-place update.
4. `LOCK` is a fenced control object: `weftfs-s3-lock v2` on S3 and
   `weftfs-gcs-lock v2` on GCS, with identical fields (`owner`, monotonic
   `fence`, `sequence`, absolute `lease_deadline_ns`, `renewed_ns`, the pending
   head). A live second writer fails `Busy`. Before every publication the
   holder rewrites `LOCK` conditionally on the token it last published
   (`If-Match` on S3, `ifGenerationMatch` on GCS), which both proves ownership
   and renews the lease; a successor's takeover makes that write fail `412` and
   the stale writer returns `Busy` before the head is touched. A writer whose
   own deadline passed refuses locally. Release writes a conditional tombstone
   (`weftfs-*-lock-released v1`); it never deletes `LOCK` after a read. The lock
   is advisory and provider-side, not a POSIX file lock.
5. Writable open probes both conditional forms on a temporary key. Ignoring
   either one fails closed with `UnsupportedVersion`; there is no degraded mode.
   GCS uses the same gate with `ifGenerationMatch=0` (create-once) and a stale
   generation that must be refused.
6. GCS replaces `LOCK` and the `superblock` with generation-conditioned
   uploads; it has no `If-Match`. A successful upload's reply carries the
   generation the fence must quote next. If an endpoint omits it, the backend
   confirms the generation with a read-after-write whose bytes must match the
   publication exactly, and refuses `UnsupportedVersion` otherwise.

### 0.3 MinIO court and measured result

The official `minio/minio` Docker Hub repository was not pullable on
2026-11-05 (registry returned 404/unauthorized). `scripts/s3_court.sh` therefore
runs the official linux-amd64 MinIO release binary
`RELEASE.2025-04-22T22-12-26Z` (sha256
`53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`) inside a
minimal `debian@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc`
container. It uses the same hard limits as every other court (`3g` memory,
swap `3g`, `1024` pids, `4` CPUs, bounded tmpfs, no-new-privileges,
`cap-drop=ALL`) and a tmpfs data directory. The script creates a bucket, runs
the Rust test binary against it, and tears the server and network down.

Observed at WeftFS `8683ecc`:

| Court | Observed |
|---|---|
| `tests/s3_backend.rs` with MinIO | 6 passed, 0 failed, 0 ignored |
| `tests/s3_backend.rs` without S3 env | 6 passed (each prints `SKIP s3_backend: WEFT_S3_TEST_ENDPOINT is not set`), 0 failed |
| Rust `cargo test --all-targets` | 221 passed, 0 failed, 0 ignored |
| FUSE `cargo test --features fuse --all-targets` | 255 passed, 0 failed, 2 ignored (pre-existing) |

The tests cover init/open/create/read/range read/write/truncate/rename/
snapshot/`verify --full`, live second-writer refusal, a lost `If-Match` leaving
the previous head readable, a process-fault crash proxy at all three
publication phases, GC deleting only unreachable content, and byte-identical
artifacts versus a local store built from the same operations.

Benchmark receipt `bench/receipts/s3-backend-20261105T233154Z.json`
(single-node MinIO on localhost; 1,000 creates of a 25-byte payload, 100 x
1 MiB writes, 1,000 reads; both backends produced the same head
`8e47145e1a09c3bf…` and generation 1101):

| Phase | Local | S3 | Ratio |
|---|---|---|---|
| 1,000 creates wall | 105.9 ms / 9,442 ops/s | 1,809.7 ms / 552.6 ops/s | 17.1x |
| create p50 / p95 | 100 / 154 us | 1,781 / 1,967 us | — |
| 100 MiB writes wall | 192.0 ms / 520.9 MiB/s | 1,168.3 ms / 85.6 MiB/s | 6.1x |
| write p50 / p95 | 664 / 868 us | 10,300 / 10,775 us | — |
| 1,000 reads wall | 4.4 ms / 224,799 ops/s | 345.8 ms / 2,892 ops/s | 78.6x |
| read p50 / p95 | 4 / 4 us | 334 / 407 us | — |
| backend bytes written | 111,581,685 | 116,091,480 | +4.0% |
| verify --full | ok | ok | — |

The S3 byte total is higher because each commit rewrites the full 8192-byte
superblock object instead of one 4096-byte slot, and lock/probe traffic is
counted. S3 provides no physical-byte counter, so the receipt reports the
requested payload bytes per phase and sets `physical_bytes` to 0 there.

**Residual risks.** The S3 path is Rust-only. MinIO is single-node on
localhost with a tmpfs data directory, so this court proves protocol behaviour
and conditional-write safety, not media durability or power-loss recovery. The
writer lock is a lease, so a paused writer longer than the lease can lose the
lock; the default lease is 900 s and the court uses 2 s. No remote
multi-writer, fleet, AWS, GCS, or 3FS run is claimed. The 3FS transports are
Rust-only and were never run against a 3FS deployment: the POSIX transport is
byte-identical to `LocalBackend`, and the USRBIO transport is tested against
`bench/threefs/hf3fs_shim.c`, which reports `hf3fs_is_hf3fs = 0`.

---

## 1. Verification commands and observed results

All builds and tests ran in the OOM-protected court; nothing was compiled on
the host.

```
cd /mnt/1tb_kingston/WeftFS
scripts/docker-court.sh rust "cargo build --all-targets"
scripts/docker-court.sh rust "cargo test --all-targets"
```

Observed in `weft/rust-court:1` (built from `docker/rust.Dockerfile`,
`rust:1-slim-bookworm`; `cargo 1.99.1`, `rustc 1.99.1`):

```
tests/cli.rs          4 passed
tests/crash_proxy.rs  3 passed
tests/recovery.rs    11 passed
tests/store_core.rs  11 passed
--------------------------------
total                29 passed, 0 failed
```

The court enforces `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
a 1 GiB `/tmp` tmpfs, `no-new-privileges`, and `cap-drop=ALL`.

Tests map to the P4 requirements as follows:

| Requirement | Test(s) |
|---|---|
| init/create/reopen exact bytes, metadata, listings | `store_core::init_create_reopen_roundtrip` |
| write range / truncate / snapshot old bytes / stable ObjectId | `store_core::overwrite_range_truncate_and_snapshot_reads` |
| rename keeps ObjectId, hard links, unlink, snapshot + GC retention | `store_core::rename_hardlink_unlink_snapshot_and_gc_retention` |
| snapshots stable across reopen; live selector changes | `store_core::snapshots_are_stable_and_selectors_follow_the_pin` |
| relations: cycles, tag, one-hop, ambiguity / `require_one` | `store_core::relations_cycles_tags_one_hop_and_cardinality` |
| authorization (mode bits + `weft.acl.read`) filters results/counts/evidence | `store_core::authorization_filters_results_counts_and_evidence` |
| commit CRC, truncation, slot corruption, missing chunk, corrupt manifest | `recovery::*` (11 tests) |
| process-fault crash proxy at three publication phases | `crash_proxy::*` (3 tests) |
| single writer lock + concurrent read-only open | `store_core::single_writer_lock_and_read_only_open` |
| checkpoint reopen and corrupt-checkpoint fallback | `recovery::checkpoint_survives_reopen_and_corrupt_checkpoint_is_ignored` |
| GC mark/sweep with snapshots retained | `store_core::gc_mark_sweep_removes_only_unreachable_blobs` |
| deterministic fixture, twice, under 100 KiB | `store_core::conformance_fixture_is_deterministic_and_small`, `cli::cli_conformance_store_is_deterministic` |
| CLI JSON keeps identities separate; errors print classes | `cli::cli_verbs_and_query_json_contract`, `cli::cli_errors_print_normative_class` |
| `conformance-store` works without `--store`; `--json` after a verb | `cli::cli_conformance_store_needs_no_store_flag` |
| empty files, chunk-boundary ranges | `store_core::empty_files_and_range_reads` |

---

## 2. Implemented

- Portable on-disk layout: `FORMAT`, `LOCK`, 8192-byte superblock (two 4096-byte
  CRC32C slots), `commits/<gen>.commit`, `objects/<ab>/<hex>`,
  `manifests/<ab>/<hex>`, `refs/snapshots/<name>`, `refs/tags/`, `tmp/`.
- **Advisory selector-index segments** (Rust-only, not part of the portable
  format): `indexes/<ab>/<commit>.idx`, written after a commit publishes the
  new head. Bytes are WFX2: magic, u32le version, `commit[32]`, u64le
  `generation`/`object_count`/`relation_count`/`entry_count`, then fixed-order
  sections with u32 counts and fixed-width elements — `with_attr` (name +
  sorted object ids), `attr_eq` (name + canonical value key + sorted ids),
  `relations_from` (subject + predicate + target kind/payload, flattened and
  sorted), `tags` (name + sorted subjects) — and a trailing
  `BLAKE3("weft/index/v2", preceding bytes)`. Arrays are sorted once at write
  time; load is a sequential read plus pre-sized map insertion. A reader uses a
  segment only when magic/version, exact section bounds, the integrity digest,
  the commit key, the generation, and all three counts agree with the replayed
  state. Missing, stale, truncated, tampered, or superseded WFX1 segments are
  ignored and reported in `Store::index_stats()`, then the index is rebuilt
  from state (WFX1 is not interpreted; new writes are WFX2 only). Complete
  indexes only are persisted, so a loaded index is exact for its commit.
  `FORMAT`, the superblock, commit records, manifests, and checkpoint states
  are unchanged; correctness never depends on an index file. GC deletes `.idx`
  files whose commit is not the current head, a snapshot commit, or a commit
  carrying a checkpoint; `verify` ignores them.
- Superblock exactly per `WEFT-MODEL.md` §2.1 (magic `WFSB`, 1.0, generation,
  commit hash, commit WCB length, timestamp, CRC over `[0,64)`, zero padding).
  Store open validates magic/version/CRC/generation/padding.
- Commit record exactly per §2.2 (`weftfs.commit.v1`), all eight ops, canonical
  WCB. Commit file `WFC1 || u64le length || WCB || u32le CRC32C(WCB)`; commit
  hash `H("weft/store/commit/v1", WCB)`; superblock record length is
  cross-checked against the decoded commit length during recovery and verify.
- Publication order: content/manifests fsynced → commit file fsynced →
  `commits/` fsynced → superblock slot (`generation % 2`) fsynced → store root
  fsynced. Newly created two-hex shard directories fsync their parent.
- Recovery: reads both slots, selects the highest published generation,
  validates commit CRC/hash/generation/parent chain, replays from the nearest
  valid checkpoint (skipping that commit's ops), validates the final catalog
  (root, entries, tips, manifest identity, chunk existence and length), falls
  back generation by generation, records rejected generations, and never
  deletes rejected data. A torn publication above the highest valid slot is not
  visible.
- Writer generations skip any rejected commit file (`next_generation`), so a
  writer cannot overwrite a torn `commits/<n>.commit`.
- Single-writer `flock` (`fs2::try_lock_exclusive`), typed `Busy`. Read-only
  open takes no lock and pins the generation it read.
- Object operations: lookup, `read_object`, `read_range`, `read_content`,
  `read_object_at`, `object_meta`, `list_dir`, path/component resolution.
  Materialization verifies chunk hashes and, for a full object read, the
  whole-content hash. A hash/length mismatch fails closed.
- Mutations, each exactly one atomic commit: `create_file`, `write_file`
  (current-size bound `MAX_FILE_BYTES` = 1 TiB, zero-filled gap), `truncate`,
  `unlink`, `rename` (delete+insert, replacement of non-directories), `link` (no
  directories), `mkdir`, `symlink` (target in `attrs["weft.symlink.target"]` as
  bytes), `setattr`, `relation_add` (idempotent by RelationId),
  `relation_delete`, `snapshot_create`, `checkpoint`.
- **Streaming create/append.** `create_file_streaming` (plus `_seeded` and
  `_seeded_causal`) reads a generic `Read` source in bounded units
  (`max(1 MiB, chunk_size)`, rounded up to a chunk multiple), stores each chunk
  as an object blob, and feeds the same bytes to the frozen v1 streaming hasher.
  `append_file_streaming` reuses the existing full chunks verbatim, combines the
  old tail when the file is not chunk-aligned, and stores the new bytes one
  chunk at a time. Both publish exactly one ordinary commit and use the same
  object/entry/tip records, version derivation, attrs, and causal rules as the
  buffered paths. Read errors and the size bound fail before publication; the
  generation and all state maps are unchanged, unreferenced blobs are left for
  `gc`, and `verify` passes. Peak memory is O(I/O buffer + chunk count + chunk
  size), not O(file size). A manifest is capped at `MAX_CHUNKS_PER_FILE`
  (16 Mi references, exactly 1 TiB at the default 64 KiB chunk size), so a
  pathologically small configured chunk size fails with `LimitExceeded`
  instead of growing an O(file size) chunk list. `Raw` writes the frozen v1
  manifest; `CodecAuto` buffers only a stream that ends inside a 16 MiB-capped
  preamble, otherwise it writes a v2 `kind:"raw"` chunk manifest. Streaming
  ZSTD/DELTA is not
  implemented: the codec encoders take a whole in-memory buffer, so using them
  would reintroduce O(file size) memory. `write_file` itself is unchanged and
  still materialises the file; `read_range` reads only intersecting chunks,
  while `read_object` materialises the whole file and is therefore limited by
  available memory.
- **Group commit** (`Store::group`): a closure runs the ordinary mutation
  methods and the store publishes **one** commit containing every buffered op,
  in application order, when the closure returns `Ok`; the generation advances
  by exactly one. Ops are applied to the live in-memory state as they are made,
  so reads inside the closure see earlier writes in the same group; per-op
  validation, bounds, timestamps and version derivation are the
  single-mutation ones. On closure error nothing is published: the commit
  chain, head, and in-memory records (objects, entries, relations, snapshots,
  tips) are exactly as before. Content chunks, manifests and causal blobs
  written before the failure remain unreferenced; `verify` still passes and
  `gc` reclaims them. Nested groups, and `gc` inside a group, are
  `InvalidArgument`; a read-only store is `ReadOnly`. A group with no buffered
  ops publishes nothing. Snapshots created inside a group pin the generation
  that was head when the group opened.
- **Group durability.** A group is durable only when `Store::group` returns
  `Ok`, at the one publication the group performs: commit file written and
  fsynced, `commits/` fsynced, superblock slot written and fsynced, root
  fsynced — the same sequence a single mutation uses. A crash at any point
  inside the closure leaves the previous generation exactly as it was before
  the group; a crash during publication is the ordinary commit crash set (old
  generation, or the whole new generation, never a partial state).
  `tests/group_commit.rs` injects a child-process fault at `after_objects`,
  `after_commit_file`, and `after_superblock_write` for one group of five
  creates and asserts exactly those outcomes.
- Explicit ObjectId seeds: `create_file_seeded`, `mkdir_seeded`,
  `symlink_seeded`; random seeds come from `/dev/urandom` (fail closed).
- Selector evaluation with `weftcodec::selector`: namespace check, authorized
  universe first, absent/mistyped comparisons false, `attr_ne` only when both
  sides exist, `not` complement inside the authorized universe, one-hop
  `relation`, `tagged_as` sugar, order list + unconditional `object_id`
  ascending tie-break, offset then limit, counts/evidence authorized only.
  `selector_id` is always the query hash; `snapshot_id` is the pinned commit.
- `require_one`: `NotFound` for 0, `Ambiguous` for >1, using the full
  authorized count even when pagination hides matches.
- Authorization: POSIX owner/group/other bits, supplementary groups, plus
  `weft.acl.read` (array of u64 uid) as a conjunctive filter. uid 0 has no
  special status. `can_read` / `can_traverse` / `can_write` exposed.
- `verify(full)` and mark-and-sweep `gc(apply)` with roots = current state,
  every snapshot state, and every checkpoint state manifest (including
  checkpoints of unpublished/rejected commits). GC honors `gc_grace_secs`
  (`0` disables the age gate) and only deletes with `apply`.
- Deterministic conformance fixture (`weftfs conformance-store`,
  `conformance::build_fixture_store`): fixed timestamp
  `1700000000000000000` ns and explicit seeds. Committed under
  `conformance/store/v1/` with `EXPECTED.json` and an append-only README.
- CLI: `init`, `info`/`open`, `create`, `write`, `cat`, `ls`, `stat`,
  `unlink`, `rename`, `link`, `mkdir`, `symlink`, `setattr`,
  `relation add|list|delete`, `query`, `snapshot create|list`, `pin`,
  `verify`, `gc`, `checkpoint`, `conformance-store`. `--store` and `--json`
  are global flags (they may appear before or after the verb);
  `conformance-store` does not require `--store`. Errors print
  `Class: message` and exit nonzero. Query JSON exposes `selector_id`,
  `snapshot_id`, `matched_count`, and per-result `object_id`, `version_id`,
  `content_id`, `size`, `kind`, `path`.

---

## 2a. Experimental codec-backed representations (`CodecAuto`)

**Status: experimental, opt-in, implemented in Rust and Go (mutual read/verify; frame bytes are not claimed identical).** `Options.representation` defaults to
`RepresentationMode::Raw`. With `Raw` — the mode used by every existing fixture,
test, FUSE mount, and handoff — the writer emits the frozen v1 manifest and the
on-disk bytes are unchanged. `CodecAuto` must be selected explicitly.

The CLI exposes the policy as global flags (accepted before or after the
subcommand, so they also work as mount flags):

```sh
weftfs --store DIR --representation codec-auto create /big.bin --file big.bin
weftfs --store DIR --representation codec-auto --codec-min-bytes 65536 verify --full
weftfs --store DIR mount --mountpoint MNT --representation codec-auto \
  --content-identity whole-file
```

`mount` defaults `--content-identity` to `merkle` (Q1); a v3 Merkle manifest
carries a raw representation only, so `--representation codec-auto` on a mount
requires the explicit `--content-identity whole-file` shown above and is
otherwise `InvalidArgument`. Non-mount commands keep the frozen `whole-file`
default.

`--representation` accepts `raw` (the default) or `codec-auto`; any other value
is `InvalidArgument`. `--codec-min-bytes` defaults to
`DEFAULT_CODEC_MIN_BYTES` (4096) and rejects non-integers with
`InvalidArgument`. The flags are resolved once into `Options` where
`open_with_options` is called, so every write verb (including writes through a
FUSE mount) uses the same policy. Read-only verbs accept and ignore the flags
(the policy affects only new writes). The policy is per invocation and is not
stored in the superblock; `info --json` reports the active invocation policy as
`representation` and `codec_min_bytes`. A plain invocation and a plain mount
remain `raw`, byte-identical to the frozen fixture.

A v2 manifest (`weft.manifest.v2`, one spelling, no alias) has the v1 fields
plus a canonical WCB `representation` map:

| Key | Type | Meaning |
|---|---|---|
| `kind` | text | `raw` or `codec` |
| `frame` | bytes32 or null | manifest-domain blob id of the WFT1 frame (null for raw) |
| `dependencies` | array of bytes32 | content roots (manifest ids) of bases needed to decode |
| `strategy` | text | `raw`, `zstd`, `delta`, or `reference`; cross-checked against the frame header |
| `decoded_size` | u64 | must equal the manifest `size` |

A v1 manifest must not contain `representation`; a v2 manifest must contain it.
Unknown schemas are `UnsupportedVersion`. A v2 `kind:"raw"` manifest uses the
same chunk layout as v1 and decodes identically. A v2 `kind:"codec"` manifest
has an empty `chunks` array: the logical bytes live in the frame blob.

Writer policy (`store_file_content`, the create path, and `write_file_causal`):

- Files below `codec_min_bytes` (default 4096) stay on the raw path. In
  `CodecAuto` the fallback manifest is v2 `kind:"raw"`; in `Raw` it is v1.
- Otherwise the writer tries ZSTD (`weftcodec::modes::encode_zstd`) with the
  store chunk size clamped to 4 KiB–64 KiB, and DELTA
  (`weftcodec::modes::encode_delta`) when a causal parent
  (`weft.causal.parents`) resolves to a live file version. DELTA is selected
  only when it is at least `codec_delta_margin_percent` (default 10) smaller
  than the ZSTD frame. The base's **content root** is recorded in
  `dependencies`.
- If no frame is strictly smaller than the raw chunk bytes, the writer falls
  back to the raw layout. Frames are stored as `BlobKind::Manifest` blobs under
  `manifests/<ab>/<hex>` (domain `weft/manifest/v1`); the v2 manifest is stored
  the same way. The choice is explicit and consistent.

Reader policy: v1 and v2 `kind:"raw"` use the existing chunk path;
`kind:"codec"` fetches and parses the frame with `Limits::default()`, requires
the frame's declared payload mode to match `strategy`, materialises each
dependency from its content root and verifies its length and
`H("weft/chunk/v1", bytes)`, then decodes. Chunked ZSTD/RAW ranges decode only
the intersecting chunks (the writer chooses a chunk size, so a 4 KiB range does
not decode the whole object). Dependency depth is bounded at 8 and a visited
set rejects cycles. Missing, unreadable, or mismatched dependencies fail closed
with `Corrupt`; no partial or approximate bytes are returned.

GC marks a codec manifest's frame and its dependency content roots
(transitively, bounded), so deleting an object does not silently break a live
dependent that names it. `gc --apply` reclaims the dependency once no root
references it. `verify --full` decodes and re-hashes every codec representation
and names the object, manifest, and frame on failure.

The Go store implements the same v2 parser and writer for `weft.manifest.v2`
(fail closed with its unknown-schema/unsupported-version class). The Rust→Go
handoff court therefore still uses the default `Raw` path (`--representation raw`)
and continues to compare byte-identical v1 fixtures; the Go store does not
understand stores written by `--representation codec-auto`. The v2 path is not
part of the frozen v1 format and is not claimed compatible or interoperable.

---

## 2b. Tensor representations and the per-object planner (B1)

**Status: implemented in Rust only; the Go store rejects `kind:"tensor"` with
its usual unknown-schema/unsupported-version class.** A tensor object uses the
v2 `kind:"tensor"` representation described in `spec/WEFT-MODEL.md` §2.3.1:
`frame`, `tensor_mode` (10/11/12/13/14), `dtype`, `shape`, `logical_root`,
canonical `layout`, `dependencies` (base content roots for 11/14), `strategy`,
and `decoded_size`. The store owns the tensor vocabulary:

- **One object per tensor, one frame.** A multi-shard declaration
  (`sharded` with `shards > 1`) is rejected with `InvalidArgument`; the store
  does not silently write a partial shard set.
- **Layout independence.** `write_tensor_file*` accepts canonical row-major,
  column-major, blocked, or single-shard input for `bytes` and canonicalises it
  to row-major before encoding. The recorded `layout` describes the supplied
  arrangement. The same logical tensor written under different layouts has the
  same `LogicalTensorRoot` (tested).
- **Writer/planner coverage:** `raw` chunks, v2 `zstd`, mode 10 (`q8_0` only),
  mode 11 weight-delta against the causal parent, and mode 14 semantic-delta
  against a bounded same-semantic-root candidate set. Modes 12/13 are decoded
  and verified on read but not produced by the B1 planner (they need workload
  metadata outside the tensor contract). `q4_0` store writes are rejected.
- **Delta bases are synthesised, not stored.** A mode-11/14 frame's
  `dependencies` is the base *content root*; the reader materialises the base
  representation, verifies its content hash against the frame descriptor, and
  rebuilds a self-contained mode-6 `TENSOR` frame. The frozen dependency/GC
  rules therefore apply unchanged and no extra base blob is written.
- **Depth bound.** The planner refuses a delta whose base chain would exceed
  the frozen dependency depth bound (8), falling back to an independent
  representation. This keeps a 10-step checkpoint chain readable.
- **Identity for raw/zstd tensors.** When the planner chooses `raw` or `zstd`,
  the manifest stays the unchanged v2 kind; tensor identity is carried by the
  reserved attributes `weft.tensor.dtype`, `weft.tensor.shape`,
  `weft.tensor.logical_root`, `weft.tensor.layout`, and `weft.tensor.strategy`.
- **Reads:** full reads decode the frame, verify dtype/shape, recompute
  `logical_root`, and verify the frozen whole-content digest. `read_tensor_region`
  returns an exact row-major hyper-rectangle; delta/Huffman frames are not
  independently seekable, so the whole object is decoded first (documented cost,
  not a correctness claim).
- **GC/verify:** `mark_manifest_tree` retains a tensor frame and recursively
  marks dependency content roots; `verify --full` decodes every tensor
  representation and reports the object, manifest, frame, and failing
  dependency. A missing base fails reads with `Corrupt` and is named by verify.

The planner is `physical_bytes + lambda_ns * estimated_decode_ns`, with
`lambda_ns = 0` by default (bytes-only). Candidates are encoded to measure
`physical_bytes`; the integer decode model is fixed in code
(raw `size/200 + 20 µs`, zstd `size + 50 µs`, mode 10 `4*size + 100 µs`,
mode 11 `3*size + 100 µs`, mode 14 `3*size + 150 µs`) and is explicitly an
estimate, not a measurement. Ties keep the fixed candidate order. Only the
chosen representation is written; the runner-up and prices are exposed by
`Store::plan_tensor` and `weft plan --json` because the manifest schema admits no
planner fields.

CLI:

```sh
weftfs --store DIR write /w --tensor --dtype bf16 --shape 256,512 \
    --file weights.bin --layout blocked --block-shape 64,64 --lambda-ns 0
weftfs --store DIR --json plan --file weights.bin --dtype bf16 --shape 256,512 \
    --lambda-ns 100000 --causal <version-hex>
weftfs --store DIR cat /w --region 0,0:2,512
weftfs --store DIR verify --full
```

Measured bench (`bench/receipts/tensor-planner-20261104T224929Z.json`, synthetic
10 checkpoints x 8 tensors x 1 MiB bf16, 1% change per step, single host court,
no fleet claim): independent ZSTD 15,917,707 physical bytes / 8.22 s ingest /
93 ms restore (1 worker); bytes-only planner 11,187,086 bytes / 24.37 s /
1.79 s; latency-weighted planner 83,985,344 bytes / 7.77 s / 37 ms. Restore
with 2/4 workers was 48/26 ms (zstd), 906/467 ms (bytes), and 22/15 ms
(latency); peak RSS 51,680/52,164/52,148 KiB. The bytes-only planner saves
29.7% physical bytes but its delta chains and semantic candidate search cost
~3x ingest and ~19x restore; the latency-weighted planner trades 5.3x physical
space for the fastest restore. All three planner runs verify clean.

---

## 2c. Merkle content identity (`weft.manifest.v3`)

**Status: implemented in Rust and Go; opt-in; the default remains v1.** A raw
chunk table may be written under schema `weft.manifest.v3`, which replaces
`content_hash` with `content_root` and carries `content_identity:"merkle-v1"`.
The root is the Merkle fold of
`H("weft/merkle/leaf/v1", u64le(len) || chunk_hash)` with internal nodes
`H("weft/merkle/node/v1", left||right)`; an odd trailing node is promoted
unchanged; an empty file is `H("weft/merkle/empty/v1", empty)`. See
`spec/WEFT-MODEL.md` §2.3.2 for the normative construction.

Selecting it:

```
weftfs --store DIR --content-identity merkle create /f --file big.bin
weftfs --store DIR --content-identity merkle write /f --offset 4096 --file patch
weftfs --store DIR --content-identity merkle info --json
```

Go usage is identical (`weftfs --content-identity merkle ...`). The policy is
per invocation and not persisted; reading dispatches on the manifest schema.

Write/read behaviour:

- A range write re-hashes and re-stores only chunks whose bytes changed; every
  untouched chunk reference is reused verbatim and its blob is not read. The
  root is folded from the manifest's chunk hashes, so hashing a small write is
  O(changed chunks + #chunks small hashes), never an O(file size) read.
- A full read verifies every chunk blob and the recomputed root. A range read
  verifies the touched blobs. `Store::verify_region` /
  `Store::VerifyContentRegion` verifies the touched blobs and recomputes the
  root from the manifest chunk table; a failure names the chunk index.
- A v3 file is never implicitly rewritten as v1/v2. Reopening with the default
  identity preserves it, and `verify --full` handles v1, v2, and v3.
- The root is chunk-size dependent by design; `chunk_size` is part of the
  manifest. Same bytes + same chunk size produce the same root in both
  implementations (cross-language tests compare them in both directions).

Measured (`bench/receipts/merkle-identity-20261105T161026Z.json`): 128 MiB,
64 KiB chunks, 500 random 4 KiB overwrites. Rust whole-file p50 50.43 ms /
1,024,537 chunk reads / 67.2 GB `rchar`; Rust Merkle p50 1.81 ms / 537 chunk
reads / 87.9 MB. Go whole-file p50 202.7 ms / 134.3 GB `rchar` / 437 MiB peak
RSS; Go Merkle p50 4.38 ms / 87.9 MB / 43 MiB. Every run verified full, kept
its identity, and changed the root.

## 2d. Immutable write overlays (`weft.manifest.v4`)

**Status: Rust write path + compaction implemented; Go read/verify/GC only.**
`spec/WEFT-MODEL.md` §2.3.3 defines `kind:"overlay"`: an immutable base manifest
plus ordered extents, no top-level chunk table, and a `content_root` equal to
the Merkle root of the derived canonical table. v1/v2/v3 bytes are unchanged,
and a v4 manifest is written only when overlay representation is selected.

Selecting the writer:

```
weftfs --store DIR --representation overlay --content-identity merkle create /f --file big.bin
weftfs --store DIR --representation overlay write /f --offset 4096 --file patch
weftfs --store DIR compact /f
```

`--representation overlay` selects the Merkle identity when no identity is
named (an overlay layer is only composable over a Merkle chunk table); an
explicit `--content-identity whole-file` is refused, not upgraded.

Write behaviour:

- A range write publishes **one new manifest version**: the previous version as
  an immutable base plus one extent. Only chunks the write touches are read and,
  when their bytes change, stored; untouched chunks inside the extent keep their
  references and their blobs are not read. The chunk table and Merkle root are
  derived without reading any unchanged chunk.
- A misaligned write is widened to the enclosing chunk grid and the untouched
  bytes of the widened chunks come from the previous version, so the logical
  content is exact.
- Adjacent or overlapping extents are folded into the current layer (the layer
  may list up to 64 extents); only a full layer starts a new one. Chain depth
  therefore grows per 64 disjoint extents, not per write.
- Before publishing, the writer derives the canonical table twice: from the
  previous table plus the edit, and from the base manifest plus the new extent
  list. They must be identical or the write fails with `Corrupt` and nothing is
  published. The result is exactly the root a canonical v3 table produces.
- Bounds: `Options::max_overlay_depth` (default 64, hard reader bound 256) and
  `Options::max_overlay_bytes` (default 1 GiB) refuse with `LimitExceeded` and a
  message naming `weftfs compact`. Publication uses the unchanged commit
  protocol; old versions and their chunks stay valid for snapshots.

Whole-file helpers on an overlay object (no silent materialisation):

| helper | behaviour |
|---|---|
| `write_file` / `write_file_ranges` | publish an overlay layer |
| `zero_range` (extend or not) | publish an overlay layer of zeros; a full zero chunk is stored once |
| `truncate` to a larger size | publish an overlay layer of zeros |
| `truncate` to the same size | no-op, publishes nothing |
| `truncate` to a smaller size | `UnsupportedRepresentation` |
| `insert_range` | `UnsupportedRepresentation` (shifts every following byte) |
| `collapse_range` | `UnsupportedRepresentation` (shortens and shifts) |
| `append_file_streaming` | `InvalidArgument` (unchanged; use a range write at EOF) |

`Store::compact(id)` re-reads and re-hashes every logical chunk in order
(bounded to one chunk of memory), recomputes the root, and refuses to publish
unless it equals the pre-compaction root. It then writes one canonical
`weft.manifest.v3` table through the normal commit protocol, so the object is
readable by both implementations again and snapshots pinned to the old chain
remain exact. A non-overlay object is refused with
`UnsupportedRepresentation`. CLI: `weftfs compact <path>`.

Go: `weft-model` parses v4, and the Go store reads, range-reads, verifies, GCs,
and charges quota for an overlay object. Go has no v4 writer and no
compaction; a range write to an overlay object returns `InvalidArgument`.
There is no Go write parity to claim.

Measured (`bench/RESULTS-overlay.md`, receipts
`bench/receipts/overlay-20261106T103115Z.json` local and
`bench/receipts/overlay-s3-20261106T103120Z.json` MinIO): 128 MiB object,
500 4 KiB overwrites, 64 KiB chunks. Local 500-write wall 917.3 ms (COW) vs
415.5 ms (overlay), 87.4 MB vs 36.7 MB written; S3 2,308.3 ms vs 1,702.9 ms.
Warm 4 KiB range read 0.919 ms vs 0.030 ms; **cold first read 0.920 ms vs
5.594 ms (2 vs 10 requests)** because a new process resolves the depth-8 chain.
Compaction 34.0 ms local / 444.1 ms S3, root unchanged, snapshot unchanged
after compaction and GC. Full-read cost is the same.

---

## 3. Format interpretations and spec notes

These are deliberate, documented decisions where the prose admits more than one
reading. They are part of the Go handoff contract.

1. **Manifest schema spelling.** `spec/WEFT-MODEL.md` §2.3 names
   `"weft.manifest.v1"`. The P4 task text also mentions `"weftfs.manifest.v1"`.
   The writer follows the normative spec (`weft.manifest.v1`); the reader
   accepts both spellings and the distinction is only a compatibility courtesy.
   The experimental representation schema is written and read as
   `"weft.manifest.v2"` with no alias (see §2a). It is not part of the frozen
   v1 contract.
2. **`ObjectRecord.content`.** For regular files this field is the **manifest**
   hash (`ManifestId`), so a reader can locate `manifests/<ab>/<hex>`. The
   whole-file `H("weft/chunk/v1", plaintext)` digest is stored inside the
   manifest as `content_hash` and verified after assembly. Directories use the
   empty-content digest (no manifest exists); symlinks use the target-bytes
   digest (no manifest exists). `read_object` returns the target bytes for a
   symlink.
3. **SnapshotId.** `snapshot_create` commits a `set_snapshot` op whose
   `commit` field is the current head, then returns that pinned commit hash.
   `SnapshotId` is therefore the immutable commit root of the pinned state, not
   the hash of the publishing commit. The state map (`snapshots`) and the
   `refs/snapshots/<name>` cache both name the pinned commit. This makes the id
   stable across replay, checkpoints, and reopen.
4. **Generation gaps.** After recovery rejects a torn commit file, the next
   writer commit uses `max(recovered generation, highest commit filename) + 1`,
   leaving a gap. Parent links, not arithmetic, define the chain; replay
   requires strictly increasing generations and a generation-1 root.
5. **Commit filenames** are plain decimal (`commits/12.commit`), not
   zero-padded. Readers must parse the numeric stem.
6. **Tips map keys.** WCB map keys are text, so the checkpoint `tips` map uses
   lowercase hex object ids as keys.
7. **Version records.** `VersionId` is derived at write time from the
   `weftfs.version.v1` record and recorded in object/commit state. The record
   itself is not persisted, so readers cannot re-derive the `parent` field from
   an object record; the recorded VersionId is treated as an opaque 32-byte
   identity. This is a v1 format gap for Go (see §5).
8. **Unlink and object records.** When the last directory entry for a
   non-directory object is removed, the object record is deleted in the same
   commit. P4 has no open-handle table; P5 must add pins so an unlinked open
   handle keeps its object reachable.
9. **Path resolution** at the store layer does not follow symlinks and rejects
   `..`. Names are bytes, never implicitly UTF-8. `.` and `..` cannot be
   created.
10. **Selector traversal.** The authorized universe for selectors is “objects
    readable by the principal”; directory traversal is not required. The
    namespace root must be readable, otherwise resolution returns
    `Unauthorized`. Relation edges are evaluated only when the target object is
    visible.
11. **Checkpoints** describe the state *after* the naming commit's ops and
    declare that commit's generation. Replay loads and skips that commit's ops.
    A checkpoint that is missing, corrupt, wrong-schema, or wrong-generation is
    reported and ignored; replay falls back to earlier state without inventing
    data.
12. **`fixed_timestamp_ns`** is a test/fixture hook on `Options`. It is not a
    format feature.
13. **`state_at` visibility.** Only commits reachable by parent links from the
    recovered head are addressable. A commit file written above the published
    superblock (a crash tail) is not readable through `state_at`/`resolve`,
    even if its hash is known.

---

## 4. Conformance fixture

- Path: `conformance/store/v1/`
- Format: `weftfs-store 1`
- Generation: `11`
- Snapshot root (head): `8959c65b7b5d821063e8785ed950a67214b555210bec45f65fd576053b0e1d81`
- Snapshot `v1`: `38d1062a65723036d0a9e146ee38ccdf61820eae69aaa7c9f983aef59735619a`
- Contents: three files (`/hello.txt`, `/docs/readme.md`, `/data/binary.bin`),
  directories `/docs` and `/data`, symlink `/link`, hard link
  `/hello.txt-hard`, two relations (`tagged_as` and `derived_from`), and three
  selector expectations.
- `EXPECTED.json` records format, generation, snapshot root, object ids,
  version ids, content roots, whole-content hashes, paths, entries (including
  both hard-link names), relation ids, snapshot names/hashes, and selector
  results.
- Reproducible with:

  ```
  scripts/docker-court.sh rust \
    "cargo run --quiet --bin weftfs -- \
       --store .docker-cache/fixture-out/unused \
       conformance-store --out .docker-cache/fixture-out/v1 \
       --generator-revision <revision>"
  ```

  The store bytes depend only on the fixed timestamp, seeds, and op order; the
  `--generator-revision` value appears only in `EXPECTED.json` and `README.md`.
  The committed fixture was generated from the implementation revision named in
  those files.
- Empty layout directories cannot be represented in git; `tmp/.gitkeep` and
  `refs/tags/.gitkeep` are placeholders. A writer recreates missing layout
  directories on open. The store ignores files whose names are not two-level
  64-digit lowercase hex.

---

## 5. Known gaps (not implemented)

- **FUSE frontend (P5), POSIX matrix, crash court** — out of scope for P4.
- **`rmdir`**, directory replacement in `rename`, recursive directory removal.
- **Open-handle pins** beyond the explicit `pin` API; unlinked objects are
  reclaimed when the last name disappears.
- **`xattr`/`xattr remove` as separate verbs**; attributes are replaced through
  `setattr`.
- **Copy / reflink / import / export / migration.**
- **`verify --repair` / quarantine tooling.** Recovery never repairs or deletes;
  rejected generations are reported and left in place.
- **Performance measurements.** Replay is O(history) with checkpoints every 256
  commits by default; no benchmark is claimed.
- **Multi-writer, replication, encryption at rest, authentication of
  authorship.** Explicitly out of scope (see `security/THREAT_MODEL.md`).
- **Model checking.** Invariants I1–I8 are exercised by tests only.
- **WeftFS-Go** consumption of the fixture (P6).
- **Go `weft.manifest.v4` writer and compaction.** Go reads, range-reads,
  verifies, GCs, and charges quota for an overlay object, and refuses a range
  write to one with `InvalidArgument`. There is no Go write or compaction
  parity.
- **Overlay whole-file rewrites that shorten or shift the file.** A shrinking
  `truncate`, `collapse_range`, and `insert_range` return
  `UnsupportedRepresentation` on an overlay object; compact first or use the
  raw representation.
- **CLI flags for the overlay writer bounds.** `max_overlay_depth` and
  `max_overlay_bytes` are library options with documented defaults (64 and
  1 GiB).
- **Persisting `Options`.** `owner_uid`, `chunk_size`, `checkpoint_interval`,
  and `gc_grace_secs` are per-process options, not stored in the superblock.
  New files record their chunk size in the manifest, so readers do not depend
  on the option value; CLI invocations use documented defaults.
- **Version record persistence** (§3.7).

---

## 6. Remaining risks

1. **Open-time catalog checks are existence/length checks.** Recovery verifies
   manifest identity and chunk file existence/length, but does not hash every
   chunk for every object at open. `read_object`/`read_range` verify the bytes
   they return; `verify --full` hashes all chunks and whole-file content.
   A latent silent bit flip in an unread chunk is therefore not detected at
   open.
2. **`write_file` materializes the file.** The raised 1 TiB bound no longer
   keeps memory bounded for a buffered write: in the frozen raw representation
   `write_file`, `truncate`, `zero_range`, `collapse_range`, and `insert_range`
   still read the whole file and allocate the new size. Use
   `create_file_streaming` / `append_file_streaming` for large content, or the
   overlay representation, where a range write, `zero_range`, and a growing
   `truncate` are chunk-bounded (a shrinking truncate, collapse, and insert
   refuse instead). The whole-file `ContentId` still costs one O(size) digest
   pass even on the streaming append path; range reads are chunk-bounded, but
   `read_object` returns the whole file and is memory-limited by design. The Go
   store still enforces its 256 MiB `MaxFileBytes`; Go matches the same bound
   and chunk limit.
3. **Checkpoint cost.** A checkpoint manifest contains the whole state; a
   checkpoint commit is O(state). The default interval of 256 is a
   replay-cost/memory tradeoff not yet measured.
4. **`WEFT_CRASH_PHASE` is honored whenever the environment variable is set.**
   It is intended only for the crash-proxy test, but it is not compiled out.
   A production environment must not carry that variable.
5. **Snapshot pointer publication is post-commit.** If `snapshot_create`
   crashes after the commit but before the pointer write, the snapshot is
   authoritative in commit state but the `refs/snapshots/<name>` cache is
   absent. Readers must use replay, as the spec requires.
6. **GC roots depend on readable snapshot states.** If a snapshot's chunks or
   manifests are already missing, GC records the problem but cannot mark that
   snapshot's content; the snapshot was already unreadable.
7. **Single-host lock only.** `flock` is advisory and local; NFS/multi-host
   writers are not supported.
8. **Timestamp clock.** Real writes use wall-clock nanoseconds; a backwards
   clock can make version mtimes non-monotonic. Version identity includes
   content/mtime/parent and remains well-defined.
9. **Digest identity vs. authorization.** BLAKE3 digests are integrity
   identifiers, never capabilities. v1 deduplication is store-local and reveals
   equality to a local observer; no cross-tenant privacy is claimed.

## R8 addendum — read-side single-flight and decoded-object cache (2026-11-06)

The read path gained two optional layers. Neither is part of any on-disk
format: the frozen manifests, commits, superblock, checkpoints, indexes, and
access counters are unchanged, and `verify`/GC/commits never consult them.

* **`Class::Cancelled`** is a new store-level error class, emitted when a
  caller cancels while waiting for an identical in-flight reconstruction. It is
  not a codec class and no wire encoding uses it.
* **`Options`** gains `single_flight` (default `false`),
  `single_flight_max_entries` (64), `single_flight_max_bytes` (256 MiB),
  `cache_bytes` (0 = disabled) and `cache_entries` (65,536).
* **Authorization first.** The layers are reachable only through an
  `AuthorizedRead` token from `Store::authorize_read` / `authorize_read_at` /
  `authorize_read_in`, which evaluate POSIX mode, `weft.acl.read`, and any
  capability envelope. `read_object_authorized`, `read_range_authorized` and
  `read_content_authorized` take the token; the cache is never consulted before
  authorization and a denial never reveals an entry.
* **Cache identity and bounds.** Key `(object id, content version)`; value is
  the verified decoded bytes. Eviction is lowest
  `decode_cost * dependent_count * observed_read_frequency` per byte, ties by
  lower score, larger bytes, then lower ids; pinned entries are never evicted.
* **Uncached internal reads.** `read_object`, `read_object_at`, `read_range`,
  `read_content`, `read_object_uncounted`, and every verification/planning path
  keep the shipped behaviour and never populate the cache.
* **FUSE.** Range reads use single-flight only when a layer is enabled; a
  whole-object `read_content` can be cached. `mode 15` tiled whole-object reads
  bypass both layers.

Measured behaviour and losses: `bench/RESULTS-cache.md` and receipt
`bench/receipts/cache-20261106T091322Z.json`.
