# Driver boundary: what a backend or transport adapter must implement

Status: **normative for the interface** (`src/planes.rs`, mirrored by
`WeftFS-Go/store/planes.go`); **advisory** for the phrasing. An adapter that
satisfies this document can carry a WeftFS store. No adapter may widen what the
store is allowed to assume.

The store talks to exactly two interfaces. Everything else — commits,
manifests, chunks, GC reachability, authorization, selectors, the context
plane — is above them and must not be pushed into a driver.

```
store (commits, manifests, chunks, GC, authorization, context plane)
  │
  ├── BlobPlane      immutable, content-addressed bytes
  └── CommitPlane    control records, the head, the writer lock
```

A single physical system (POSIX, an object store, a distributed KV engine) can
implement both (`BackendKind::Local`, `::S3`, `::Gcs`, `::Azure`, `::ThreeFs`);
`BackendKind::Split` gives the two planes different physical homes. Wrappers
(packing, encryption) implement `BlobPlane` themselves and delegate.

## 1. `BlobPlane` — the immutable byte plane

| method | must | must never |
|---|---|---|
| `get(key)` | return the exact bytes stored under `key`, or `NotFound` | return bytes from another key, a truncated read, or an empty object for a missing key |
| `get_range(key, offset, len)` | return exactly `[offset, min(offset+len, eof))`; short only at end of object | return a short read in the middle; return `Ok` for `offset > eof` (that is an error) |
| `read_into` / `read_ranges` | keep input order; each entry keeps `get_range`'s contract | reorder, merge, or coalesce in a way that changes which bytes a request sees |
| `put_if_absent(key, bytes)` | publish all-or-nothing; identical bytes are an idempotent no-op; different bytes for an existing key are `Exists` | overwrite, partially overwrite, or silently accept different bytes |
| `delete(key)` | remove the object; missing is `NotFound` | delete anything else, or report success without deleting |
| `len(key)` | exact object length | approximate, cached-then-stale, or a range length |
| `exists(key)` | true iff the object is readable | implement it as "list contains the prefix" |
| `list(prefix)` | every key under the selector | filter, paginate silently, or invent keys |
| `flush()` | make every prior `put_if_absent` durable and visible before returning | return before the publications are durable |
| `init_layout` / `ensure_layout` | create/recreate the physical skeleton only | write payload bytes or control records |
| `clean_stale(grace)` | remove this plane's own staging files older than `grace` | remove published objects |
| `head_part(bytes)` | the probe (§3) | assert a capability it did not observe |
| `stats` / `diagnostics` / `root` | honest counters and a physics path | report bytes it did not transfer |

The packing wrapper additionally uses `pack_counters`, `verify_packs`,
`reclaim_unreferenced_packs`, `buffered_blob_kinds`, `refresh_metadata`,
`defer_pack_catalog`, `take_pending_pack_members`,
`adopt_pending_pack_members`. A driver that is not itself a packing plane
returns the defaults; if it *is* a batching plane it must implement the
buffered-kind mask honestly, because the store uses it to route a read of a
not-yet-sealed member back through the writing handle.

## 2. `CommitPlane` — the control plane

Control records are `(bytes, token)`. The token is whatever the plane can
compare cheaply: an ETag, a generation, a version, a digest. **The store never
interprets the token**; it only hands back the token it received.

| method | must | must never |
|---|---|---|
| `read_control(key)` | `Ok(None)` iff the record does not exist | return `Ok(Some(empty))` for a missing record |
| `compare_exchange(key, expected, bytes)` | replace iff the current token equals `expected` (`None` = must be absent); a lost race is `Busy` | overwrite when `expected` is stale, or report `Busy` as success |
| `write_control` / `put_control_if_absent` / `create_control` | exactly what the names say | create-once that silently replaces |
| `compare_exchange_head(expected, next)` | two-slot superblock compare-and-swap as specified in `spec/WEFT-MODEL.md` | pick a slot other than `next.generation % 2`; rewrite the other slot |
| `load_head` | the head from the superblock control record | infer a head from listing commit names |
| `acquire_lock` | exclusive single-writer ownership for the handle's lifetime | implement it as a read/write flag, or release it implicitly |
| `verify_write_lease` / `fence_head_publication` / `publish_fenced_head` | prove this writer may still publish; renew a lease when one publication remains; name the head about to be published | renew without a conditional write, or publish after the deadline |
| `attested_head` | the head the lock record attests, when the plane publishes through it | guess, cache forever, or return a head the lock record does not name |
| `sync_dir(key)` | durability of the directory holding `key` | report a barrier it did not issue |
| `clean_stale(grace)` | the plane's own staging records | delete published commits, refs or the superblock |

## 3. Capabilities: declared vs probed

A driver **declares** (`Capabilities`, `BlobCapabilities`, `CommitCapabilities`)
and the store **probes** on a writable open:

1. blob probe — publish a private ~192-byte object create-once, `HEAD` it for
   its length, read a 17-byte mid-range slice, delete it, and observe the
   deletion;
2. commit probe — create-once a control record, a matching CAS, and a **stale**
   CAS that must return `Busy`.

Rules:

* **A probe may refute a declared capability. It may never upgrade one.** A
  declared-absent `atomic_publish` skips the blob probe (the store refuses that
  configuration where it matters) rather than discovering it.
* A plane that accepts a stale token is refused with `UnsupportedVersion`: it
  cannot carry the commit protocol.
* A read-only open never probes and never writes.
* `durable_fsync` is an **attestation**, not a measurement. The probe proves
  visibility, exact length and a true ranged read; it cannot prove that bytes
  reached stable media. A driver must not declare `durable_fsync` on the
  strength of a successful write, a `flush()` return, or a provider's 200
  response unless that response really is the durability point. The
  `publication` and `consistency` strings must say which it is.
* `true_range_reads` means a ranged request returns bytes without transferring
  the whole object. It is probe-verified; declaring it on a plane that
  downloads and slices is a defect (the pack plane refuses such an inner
  plane).
* On writable open the store refuses a plane without `durable_fsync` or
  `conditional_head`, and a split store per plane with a message naming the
  plane (`BackendKind::require_plane_contract`).

## 4. What a driver must never infer

1. **Never decide authorization.** The driver sees bytes because the store has
   already authorized the request. A driver must not cache an allow/deny
   decision, and must not return `NotFound` where the store expects a real
   `Unauthorized` (or the reverse) — nondisclosure is decided above it.
2. **Never parse payload semantics.** Blobs are opaque byte strings. A driver
   must not look inside a frame, a manifest, an index, a checkpoint or an
   encryption envelope, must not decompress or decrypt, and must not "repair"
   a hash mismatch. (The encryption and packing wrappers are `BlobPlane`
   implementations above the driver, not drivers.)
3. **Never repair or retry a create-once with different bytes.** Retrying the
   identical bytes is fine; substituting bytes is a correctness bug no upper
   layer can see.
4. **Never translate a transport failure into `NotFound`.** The store relies on
   `Io` versus `NotFound` to tell "unreachable" from "gone". A real defect of
   exactly this kind was found by the distributed court (R13) and is now a
   regression test.
5. **Never infer the head, the generation, or the list of commits** from a
   listing, a modification time, or a name sort. The commit record and the
   two-slot superblock are the only authorities.
6. **Never merge or split a logical key.** `Key` is normative: `Format`,
   `Lock`, `Superblock`, `Commit{generation}`, `Object{hash}`,
   `Manifest{hash}`, `Index{commit}`, `SnapshotRef{name}`, `Tag{name}`,
   `Tmp{name}`, `Quota`, `Pack{hash}`, `PackCatalog{hash}`, and so on. A
   driver maps a key to a physical name; it must not reuse one physical object
   for two keys, and must not hash a key into a different address space in a
   way that collides silently.
7. **Never invent a cache that changes visibility.** An immutable object may be
   cached only by key and only while the key is live; a deleted object must
   never be served from a driver cache. The store's own decode cache is above
   the driver and is keyed by object id plus version.
8. **Never make a partial publication visible.** A staging name may exist; it
   must never be readable at the published key before the publication
   completes.
9. **Never add control records the store does not name.** Extra objects in a
   driver's own namespace are tolerated only if they are invisible to `list`
   and cannot collide with a `Key`; a store-level cache that shadows `Key`
   values is a correctness bug.
10. **Never claim multi-writer support the plane does not have.** MW2 requires
    `compare_exchange_head` with a real conditional update plus the writer
    fence (§2). A plane that cannot do it is refused
    (`UnsupportedVersion`), never downgraded to blind overwrite.

## 5. Error classes a driver may return

| situation | class |
|---|---|
| endpoint unreachable, read/write/transport failure | `Io` |
| object or record genuinely absent | `NotFound` |
| credential rejected | `Unauthorized` |
| create-once found different bytes | `Exists` |
| conditional update lost the race / lock held | `Busy` |
| declared bound exceeded (object too large, too many records) | `LimitExceeded` |
| capability the plane does not have, or a stale CAS accepted | `UnsupportedVersion` |
| corrupted bytes read back from the medium | `Corrupt` (only when the bytes really are damaged) |

Register these in a `match` that is exhaustive over the driver's own error
type; do not pass through a string.

## 6. Proving an adapter

```bash
# unit-level: plane contract, capability refusals, crash proxy
cargo test --test backend          # local vs object
cargo test --test packs            # if the adapter is a batching plane
cargo test --test s3_backend       # live MinIO/Ceph RGW, skipped without an endpoint
cargo test --test azure_backend    # Azurite emulator
cargo test --test distributed_semantics

# capabilities and the probe itself
cargo test --test planes_court
```

A new adapter should pass: identical logical state against `BackendKind::Local`
for the same seeded operation script; byte-identical commits, manifests and
chunks where the mode is deterministic; `verify --full` clean; a three-phase
crash proxy that leaves only old-or-new whole state; and the capability
refusals (a plane without `conditional_head` must be refused at writable open,
not silently used).

## 7. What this boundary is not

Not a scheduler, not a placement policy, not a cache, not a conflict resolver,
not a consistency protocol, and not a metadata engine. A driver that wants to
make placement decisions may do so **inside** its own layer as long as the
`BlobPlane`/`CommitPlane` contract is unchanged; the store exposes no hint
channel and will not read one. A driver must not implement MW2, GC, retention
or erasure-coding above the boundary and hide it — those are store semantics
with receipts, and moving them into a driver removes the evidence.
