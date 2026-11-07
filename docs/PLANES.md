# Blob and commit planes (R10)

`src/backend.rs` used to expose a single `StorageBackend` that mixed immutable
content bytes with POSIX-shaped control state (`write_at` into the superblock,
`sync_dir`, an exclusive writer lock, in-place zero-fill). That works on a
local filesystem, but it forces every new backend to *simulate* POSIX
superblock and lock semantics — which is exactly what the next two programme
items must not do: physical object packs need a metadata plane that is not a
directory tree, and distributed backends need a conditional-head publication
contract rather than an in-place 4 KiB write.

R10 splits the contract into two planes. Nothing on disk changed: the key
layout, the commit-file bytes, the two-slot superblock, and the publication
protocol are byte-for-byte the same, and every pre-existing test passes
unmodified.

```
                     ┌───────────────────────────────────────────────┐
  Store  ──────────► │ StorageBackend (adapter; one I/O surface)      │
                     └───────────────┬───────────────────────────────┘
                                     │ Key routing
                 ┌───────────────────┴────────────────────┐
                 ▼                                        ▼
     BlobPlane: objects/, manifests/, indexes/   CommitPlane: FORMAT, LOCK,
     get / get_range / put_if_absent / delete     superblock, commits/,
     head_part (probe) / list                     refs/, quota, access/, tmp/
                                                  read_control / compare_exchange
                                                  compare_exchange_head (CAS)
                                                  load_head / probe
```

## 1. The two contracts

`src/planes.rs` defines them. Every method is fallible and returns the store's
typed `Error`; there is no panicking or best-effort path.

### `BlobPlane`

Immutable, content-addressed bytes. **No in-place mutation, no directory
fsync, no lock.**

| method | contract |
|---|---|
| `get(key) -> Vec<u8>` | whole object; `NotFound` if absent |
| `get_range(key, offset, len) -> Vec<u8>` | short read at EOF is normal; offset strictly past EOF is `InvalidArgument` |
| `put_if_absent(key, bytes)` | all-or-nothing create-once; identical bytes are an idempotent no-op; different bytes under the same key is `Exists` |
| `delete(key)` | missing key is `NotFound` |
| `len(key)`, `exists(key)` | length / presence |
| `list(prefix)` | every key under a `Key::Prefix` selector |
| `head_part(bytes) -> BlobProbe` | capability probe (see §3) |
| `clean_stale(grace)`, `init_layout()`, `ensure_layout()` | staging hygiene and physical skeleton |
| `stats()`, `diagnostics()`, `root()`, `blob_label()` | receipts and diagnostics |

### `CommitPlane`

Versioned control state. **Everything the commit protocol needs, and nothing
that assumes a directory tree.**

| method | contract |
|---|---|
| `read_control(key) -> Option<ControlRecord>` | whole record; `token` is the canonical content token of its bytes |
| `compare_exchange(key, expected_token, bytes)` | replace iff the record still holds the token the caller read; `None` means "must be absent"; a lost race is `Busy`, never a silent overwrite |
| `compare_exchange_head(expected, next)` | publish the superblock slot for `next` iff the plane still reports `expected` as the head (compared by generation and commit id); the rest of the 8 KiB image is preserved byte for byte |
| `load_head() -> Option<CommitHead>` | the highest valid slot; `None` only for an all-zero superblock |
| `write_control`, `create_control`, `put_control_if_absent`, `delete_control` | unconditional atomic replace, create-once zeroed, create-once bytes, delete |
| `control_len`, `control_exists`, `list_control`, `modified` | inspection |
| `sync_dir`, `init_layout`, `ensure_layout`, `acquire_lock`, `clean_stale` | durability barrier, skeleton, single-writer lock |
| `probe() -> CommitProbe` | capability probe (see §3) |
| `stats()`, `diagnostics()`, `root()`, `commit_label()` | receipts and diagnostics |

`ControlRecord.token` is always `BLAKE3("weft/control/token/v1", bytes)` in hex,
for **every** backend. An S3 ETag is a transport detail used inside the S3
implementation and a GCS object generation is a transport detail used inside
the GCS implementation; neither ever leaks into the contract, so a caller
cannot mistake a stale ETag or generation for the bytes it actually read.

## 1a. Backend implementations

| backend | file | create-once primitive | compare-and-swap primitive |
|---|---|---|---|
| `local` | `src/backend.rs` | `link(2)` of a written+fsynced temp file (rename fallback recorded) | writer lock + content-token check |
| `object` | `src/backend.rs` | `link(2)` in a content-addressed namespace | writer lock + content-token check |
| `s3` | `src/s3.rs` | `PUT` with `If-None-Match: *` | `GET` + `PUT` with `If-Match: <ETag>` |
| `gcs` | `src/gcs.rs` | JSON API upload with `ifGenerationMatch=0` | metadata+media read, then upload with `ifGenerationMatch=<observed generation>` |

All four feed the same `StorageBackend` adapter, so the store, FUSE, GC,
verify, and recovery have one implementation of each physical operation. The
GCS backend's stale-generation probe and court are described in
[`bench/RESULTS-gcs.md`](../bench/RESULTS-gcs.md).

## 2. Key routing

`CompositeBackend` (in `src/backend.rs`) routes by `Key::kind()`:

| plane | kinds |
|---|---|
| blob | `Object`, `Manifest`, `Index` |
| commit | `Root`, `Format`, `Lock`, `Superblock`, `Commit`, `SnapshotRef`, `Tag`, `Tmp`, `Quota`, `QuotaMigrated`, `Access`, `Other` |

`Key::Prefix(kind)` follows the same table, so GC's object/manifest sweep goes
to the blob plane and recovery's commit-file enumeration goes to the commit
plane. The advisory `access/` counters live on the commit plane because they
are small mutable records; they are never consulted by commits, recovery,
verification, or GC.

## 3. Capabilities are proved, not declared

A writable open probes both planes and fails closed. A read-only open never
writes and therefore never probes.

* **Blob probe (`head_part`).** Publish a private 192-byte object under
  `tmp/put-plane-probe-…`, `HEAD` it for the exact length, read back a
  17-byte range from the middle, and delete it. Establishes create-once
  visibility, exact length, true ranged reads, and deletion.
* **Commit probe (`probe`).** Create a private control record with
  `compare_exchange(expected = None)`, replace it with a matching token, then
  attempt a **stale** token and require `Busy`. An endpoint that accepts a
  stale compare-and-swap is refused with `UnsupportedVersion`: its head
  publication could lose a race silently.
* **Durability is attested, not proved.** No probe can establish media
  durability without physical power loss, and nothing here claims it. The
  receipt records the publication primitive each plane exercises.

A probe may only **refute** a declared capability, never upgrade one. A caller
that declares `atomic_publish: false` (for example `BackendKind::Posix`
standing in for an unmeasured network filesystem) is refused wherever the store
needs that capability and is never silently promoted by a probe. This keeps the
existing tested behaviour: a weak backend still opens read-write and is refused
at the first publication, which is where the refusal is actionable.

Refusal points, unchanged or added:

| condition | where | class |
|---|---|---|
| blob plane lacks `atomic_publish` | `Store::init_with_backend` / writable open (`require_plane_contract`) | `UnsupportedVersion` |
| blob plane lacks `durable_fsync` | same | `UnsupportedVersion` |
| commit plane lacks `atomic_publish` | same | `UnsupportedVersion` |
| commit plane lacks `conditional_head` | same, and `require_conditional_head` | `UnsupportedVersion` |
| commit record publication lacks atomicity | `publish_commit` | `UnsupportedVersion` |
| head publication lacks durability | `publish_commit` | `UnsupportedVersion` |
| head compare-and-swap lost | `compare_exchange_head` | `Busy` |

## 4. Split configuration

Blobs and commit/control state may live in different physical systems:

```
weftfs --blob-backend s3 --commit-backend local --store /var/lib/weft info
weftfs --blob-backend local --commit-backend s3 --blob-root /fast/blobs --store /var/lib/weft info
weftfs --blob-backend local --commit-backend local --blob-root /fast/blobs --store /var/lib/weft info
```

`--blob-backend` / `--commit-backend` default to `--backend`, so the historical
single-backend layout is the default and is never silently changed.
`--blob-root` selects a second local directory when both planes are
POSIX-shaped; a remote blob plane is addressed by its bucket instead.
`BackendKind::split` refuses nested splits.

`weftfs info --json` reports `"backend": "<blobs>-blobs/<commits>-commits"` and
a `"planes"` object with both planes' capabilities.

## 5. What a third-party metadata engine must implement

This is the hook for the pack and distributed-backend work. An external
metadata engine (a distributed KV store, a pack index, a consensus log, a
columnar catalog) does **not** need a filesystem, directories, `fsync`, or a
POSIX lock. It needs exactly this:

**Required.

1. **Addressable control records.** Store and return opaque byte strings under
   the logical names `FORMAT`, `LOCK`, `superblock`, `commits/<generation>.commit`,
   `refs/snapshots/<name>`, `refs/tags/<name>`, `quota`, `access/counts.v1`,
   `access/journal.v1`, `tmp/<name>`, and list them by kind prefix. The bytes
   are the normative ones from `spec/WEFT-MODEL.md`; the engine stores them, it
   does not interpret them.
2. **Atomic create-once publication** of an arbitrary byte range
   (`put_control_if_absent`), and **atomic whole-record replacement**
   (`write_control`). A reader observes the complete old bytes or the complete
   new bytes, never a torn record.
3. **Compare-and-swap on a record** keyed by the content token
   `BLAKE3("weft/control/token/v1", bytes)`. `compare_exchange(key, Some(token),
   bytes)` must fail with `Busy` when the record no longer holds `token`;
   `compare_exchange(key, None, bytes)` must fail with `Busy` when the record
   exists. Retrying is the caller's decision, never the engine's.
4. **A durable acknowledgement** for each publication: the write is not
   reported successful until the engine will not lose it. What "durable" means
   is the engine's documented guarantee; the store records the claim and does
   not manufacture one.
5. **A single-writer interlock** (`acquire_lock`) that fails with `Busy` when
   another writer holds it, plus a documented lease or liveness mechanism so a
   crashed writer does not wedge the store forever.
6. **Bounded reads.** A record read returns at most the record; no unbounded
   scan of the whole namespace. `list_control(Prefix)` is the only enumeration
   and must be prefix-scoped.
7. **The normative head protocol.** `compare_exchange_head(expected, next)`
   must decode the two 4 KiB slots, select the highest valid generation
   (CRC-checked), compare generation and commit id with `expected`, write the
   slot at `next.generation % 2`, and preserve every other byte of the 8 KiB
   image. A higher-generation slot that fails its CRC is *not* a valid head.
8. **Error classification** compatible with the store: `NotFound` for an absent
   record, `Busy` for a lost race or held lock, `Corrupt` for bytes that fail
   their own validation, `UnsupportedVersion` for a capability the engine does
   not have. Classes are load-bearing: `has_hold`/`retry` logic and the crash
   court depend on them.
9. **Pass the commit probe.** A stale compare-and-swap must be rejected. An
   engine that cannot do this cannot be a commit plane.

**Not required.

* No directories, no `rename`, no `fsync` of a parent, no in-place range write,
  no inode, no file mode, no mtime beyond an optional `modified`.
* No content addressing: the blob plane is a separate contract. An engine may
  be a commit plane only.

**Obligations that come with the contract.

* The blob plane must publish each manifest and chunk atomically and durably,
  because the commit record and the head are meaningless if a referenced
  manifest can appear half-written.
* `verify --full` must be able to read every referenced blob through
  `BlobPlane::get`/`get_range` and recompute every root. An engine that
  re-encodes or rewrites bytes on the way through is not conformant.
* Recovery reads `superblock`, then `commits/<generation>.commit` for the
  bounded window, then manifests. It never guesses: a missing or mismatched
  commit is a rejection, not an invitation to invent a head.

A minimal reference shape, for example a KV store with per-key CAS:

```
commit plane                     blob plane
  KV "FORMAT"            -> bytes    content-addressed store keyed by hash
  KV "superblock"        -> 8192 B   put_if_absent: hash -> bytes
  KV "commits/<gen>"     -> bytes    delete: hash
  KV "refs/..."          -> bytes    list: prefix scan over shard prefixes
  CAS via the KV's own
  compare-and-set on the
  content token
```

The R10 court already exercises two shapes that are *not* the plain local
directory: S3 as a blob plane and S3 as a commit plane (both over the same
`If-None-Match` / `If-Match` primitives), with byte-identical results to the
single-backend store.

## 6. What is not claimed

* No distributed, multi-writer, or fleet claim: one store is one writer at a
  time. Locally that writer is enforced by `flock`; on an object-store commit
  plane it is enforced by a **fenced lease with a conditional compare-and-swap**
  (see [`docs/FENCING.md`](FENCING.md)). Neither form is a multi-writer protocol.
* No physical media power loss: the crash court is a process-fault proxy and
  the split court inherits that limit.
* No S3-as-a-database claim: the S3 commit plane is a working shape used to
  prove the contract, not a performance or scale recommendation.
* The advisory index/access records are not a metadata engine; they are
  rebuildable and are never read for correctness.
