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
  (256 MiB current-size bound, zero-filled gap), `truncate`, `unlink`,
  `rename` (delete+insert, replacement of non-directories), `link` (no
  directories), `mkdir`, `symlink` (target in `attrs["weft.symlink.target"]` as
  bytes), `setattr`, `relation_add` (idempotent by RelationId),
  `relation_delete`, `snapshot_create`, `checkpoint`.
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

## 3. Format interpretations and spec notes

These are deliberate, documented decisions where the prose admits more than one
reading. They are part of the Go handoff contract.

1. **Manifest schema spelling.** `spec/WEFT-MODEL.md` §2.3 names
   `"weft.manifest.v1"`. The P4 task text also mentions `"weftfs.manifest.v1"`.
   The writer follows the normative spec (`weft.manifest.v1`); the reader
   accepts both spellings and the distinction is only a compatibility courtesy.
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
2. **`write_file` materializes the file.** The 256 MiB bound keeps memory
   bounded, but a write reads the whole file, allocates the new size, and
   re-chunks it. Large-file streaming is future work.
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
