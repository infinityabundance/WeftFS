# Augmenting AI storage systems

**Status (R2, 2026-11-09):** architectural intent and an honest boundary
statement. This document does not claim a deployment on any storage system it
names. The only integrations that have executed are the ones in the README
integration matrix and in the receipts cited below. The Rust and Go filesystems
in this repository are **reference filesystems / an executable
specification**, not competitors to 3FS, DAOS, Lustre, BeeGFS, CephFS, or
JuiceFS data planes.

## 1. What WeftFS is

WeftFS is a portable **context-addressing, identity, provenance and
reconstruction protocol**, plus **WeftCodec**, a lossless tensor-aware codec.
It is intended to sit beside a mature storage system and add the things that
system is not designed to express:

* which data is selected, and why (`SelectorId`, Context Roots, ambiguity
  returned rather than resolved);
* how versions are related (causal parents, snapshots, checkpoint topology);
* how logical state is represented (raw, zstd, tensor frames, independent
  tiles, deltas, KV pages) without changing its identity;
* who may read or write an artefact (capabilities, budgets, quota);
* how an artefact was produced and how it can be reconstructed (provenance
  receipts, recovery planning);
* how bytes are verified before they are returned (content roots checked on
  every read).

The storage system remains responsible for **delivering bytes efficiently,
consistently and durably according to its own capabilities**.

## 2. Separation of concerns

| Concern | Owner | What that means |
|---|---|---|
| Identity | WeftFS | `ContentId`, `LogicalTensorRoot`, `TileRoot`, `SelectorId`, `ProvenanceId` are computed from canonical bytes. A data plane may not rewrite them. |
| Selection | WeftFS | WeftQL compiles to a canonical AST; a selector identifies a query, never a result set. Ambiguity fails closed. |
| Lineage and context | WeftFS | Causal parents, snapshots, Context Roots, checkpoint topology. |
| Policy | WeftFS | Capabilities, budgets, quota, authorization before accounting. |
| Representation | WeftFS / WeftCodec | A logical object is encoded for storage and decoded exactly on read. Changing representation never changes identity. |
| Provenance and recovery | WeftFS | `weft.provenance.v1` records and a pure recovery planner. |
| Verification | WeftFS | Every returned byte is checked against its declared root. |
| Retention | WeftFS | Mark/sweep GC over commits, snapshots, checkpoints, provenance, and pins. |
| Bytes, replicas, erasure coding | Storage backend | Replication, EC, placement, repair, and physical durability. |
| Transport acceleration | Storage backend | RDMA, kernel bypass, client caching, and network scheduling. |
| Scheduling and GPU-direct I/O | Storage backend and its client | WeftFS does not schedule storage work or own GPU memory movement. |
| Media durability | Storage backend | WeftFS asks for `fsync` or a provider acknowledgement; it does not claim media survival. |

The boundary is deliberately small: **`BlobPlane` plus `CommitPlane`**. A
backend that implements those two contracts can be used without WeftFS owning
any part of its data path.

## 3. The backend contract

### 3.1 `BlobPlane`

Immutable, content-addressed bytes. The operations WeftFS actually needs are:

| Operation | Requirement |
|---|---|
| `get(key)` / `get_range(key, offset, len)` | Whole or true ranged reads; a short read at EOF is normal. |
| `put_if_absent(key, bytes)` | Create-once. Identical bytes are an idempotent no-op; different bytes under the same key fail closed. |
| `delete(key)` | Delete by key, used by GC and compaction. |
| `len` / `exists` / `list(prefix)` | Diagnostics and reachability checks. |
| `head_part` | A probe that proves create-once visibility, exact length, and ranged reads. |

### 3.2 `CommitPlane`

Versioned control state. The operations WeftFS actually needs are:

| Operation | Requirement |
|---|---|
| `read_control(key)` | Read a control record and return its content token. |
| `compare_exchange(key, expected, bytes)` | Replace only if the record still holds the token the caller read; a lost race is `Busy`, never a silent overwrite. |
| Head publication | Publish the next head only if the plane still reports the expected head. |
| Writer lock | An exclusive writer, either a real lock or a **fenced conditional write**. |
| Durability barrier | `fsync` or the strongest provider acknowledgement the backend offers. |

Capabilities are **probed on writable open**, not merely declared. The probe
publishes a private blob, checks its length and a mid-range read, and performs
a create-once, a matching compare-and-swap, and a **stale** compare-and-swap
that must be refused. An endpoint that accepts a stale write is refused with
`UnsupportedVersion` before any commit. A read-only open never writes and never
probes. There is no fallback to a weaker mode.

A backend that has immutable blobs but no conditional control plane can still
be used as a **blob plane only**; commits must then live on a plane that does
have a conditional write. That is exactly what the split-plane configuration
(`--blob-backend`, `--commit-backend`) exists for.

## 4. Writer capability levels

These are capability levels, not product tiers. A system is at the highest
level for which it has an executed, tested protocol.

### MW0 — concurrent blob writes

**Status: implemented.** Immutable blobs may be published concurrently because
their key *is* their digest. `Store::open_blob_worker` creates a handle that is
read-only for control state but may publish immutable, content-addressed blobs
through `put_if_absent`. Workers can read causal bases and write fragments in
parallel; they cannot create files, publish a commit, touch the superblock,
the writer lock, the lease, or the fence.

This is a sub-capability, not a standalone safe mode for logical state. It is
the prepare half of MW1.

Evidence: `docs/PARALLEL-CHECKPOINT.md`; `tests/parallel_checkpoint.rs` (7/7);
receipt `bench/receipts/parallel-checkpoint-20261108T033000Z.json`.

### MW1 — many producers, one committer

**Status: today's implementation is MW1 — many producers, one committer. This
is the highest level WeftFS reaches.**

N prepare workers publish immutable fragments. Exactly one committer owns the
writer lock and the existing lease/fence, re-verifies every fragment against
its own backend (re-reads each manifest, decodes bytes, recomputes content
roots, per-shard logical roots, and the whole-tensor root), and publishes **one
group commit**. The single-writer proof is untouched: one committer, one head
expectation, one superblock publication.

The real-model court (Qwen2.5-0.5B, 8 shards, 4 vCPU) measured one commit per
run at every rank count, with the same head and manifest root at 1/2/4/8
ranks. The local receipt is
`bench/receipts/parallel-checkpoint-20261108T033000Z.json`; the MinIO subset
receipt is `bench/receipts/parallel-checkpoint-s3-20261108T033921Z.json`.
Committer time was 73–74 ms for the single commit, and fragment verification
was about 1.25 s in that court.

Scope limits that matter to a storage engineer:

* it is one **process** with N worker threads/handles, not N independent
  clients;
* the committer re-verifies every fragment by default; `--no-fragment-verify`
  is a performance choice, not a trust claim;
* the Go reference store implements the same single-committer model and a
  group commit, but not the Rust parallel checkpoint split;
* the writer is still exclusive. A second writer of control state is refused
  `Busy`.

### MW2 — independent committers

**Status: not implemented. Not claimed.**

There is no protocol by which two processes may independently publish commits
to the same store and resolve conflicts. The fenced lease decides **which
single writer is allowed**, not how two writers agree. A second writer is
refused `Busy`; a suspended writer is fenced after takeover. There is no
multi-committer merge, no conflict resolution, and no independent-committer
test.

### MW3 — distributed scalable publication

**Status: not implemented. Not claimed.**

There is no consensus protocol, no multi-node commit protocol, no
linearizable cross-node namespace, no coordinated multi-writer GC, and no
fleet evidence. Every distributed court in this repository runs on one host,
usually against one MinIO or Ceph RGW instance. A multi-node or fleet claim
would require a different publication protocol and a different court.

## 5. Supported guarantees per backend

The guarantee is the one the backend **proves in a court**, not the one its
vendor documents. `provider ack` means the backend acknowledged the write; it
is not a media-durability claim.

| Backend | Create-once blobs | Conditional head | Writer | Durability wording | Range reads | Executed evidence |
|---|---|---|---|---|---|---|
| Local POSIX (`local`) | `link(2)` of a written+fsynced temp file (rename fallback recorded) | writer lock + content-token check | `flock(2)` on `LOCK` | `fsync`; the underlying filesystem's contract | yes | byte-identical fixtures, crash proxy, recovery, full verify |
| Object namespace (`object`) | `link(2)` in a content-addressed namespace | writer lock + content-token check | `flock(2)` on `LOCK` | `fsync` + link + directory `fsync` | yes | `tests/backend.rs`, crash proxy |
| S3 (MinIO, Ceph RGW) | `PUT` with `If-None-Match: *` | `PUT` with `If-Match`; the fenced `LOCK` v3 record is the publication point | fenced lease, one conditional write | provider acknowledgement only | yes | `s3-court`, `ceph_rgw`, fenced-writer suites |
| GCS | upload with `ifGenerationMatch=0` | upload with `ifGenerationMatch=<observed>`; fenced `LOCK` v3 | fenced lease | provider acknowledgement only | yes | `fake-gcs-server` court; no live GCS |
| Azure Blob | `Put Blob` with `If-None-Match: *` | `Put Blob` with `If-Match`; fenced `LOCK` v3 | fenced lease | provider acknowledgement only | yes | Azurite court; SAS is effectively read-only; no live Azure |
| 3FS (POSIX transport) | the store's `tmp` + `fsync` + `rename(2)` through the 3FS mount | writer lock + content-token check | `flock(2)` on `LOCK` | 3FS's `fsync` contract (untested on a cluster) | yes | ABI conformance shim only; **no deployed cluster** |
| 3FS (USRBIO transport) | data plane uses hf3fs rings; control state stays POSIX | unchanged (POSIX) | unchanged (POSIX) | unchanged (POSIX) | rings | ABI conformance shim only; **no deployed cluster** |
| DAOS, Lustre, BeeGFS, CephFS, JuiceFS, IBM Storage Scale, WEKA, VAST, AIStore | not integrated | not integrated | not integrated | the system's own | not integrated | none; design only |

Two consequences deserve to be stated plainly:

1. **A conditional write is not a distributed transaction.** The fenced `LOCK`
   v3 record makes head publication one conditional write on the control
   plane. It does not make two writers safe, and it does not make the data
   plane transactional. A lost network reply is the usual at-least-once
   ambiguity.
2. **The data plane keeps its own strengths.** If a backend provides
   replication, erasure coding, RDMA, client caching, or GPU-direct I/O,
   WeftFS uses it through the backend's client and does not reimplement,
   proxy, or claim it.

## 6. Augmenting a native backend without a FUSE-on-FUSE layer

FUSE is a convenience frontend for the reference filesystem, not a requirement
of the protocol. There are three integration shapes, in order of preference:

### 6.1 Native `BlobPlane`/`CommitPlane` adapter (preferred)

Implement the two contracts in the storage system's own client library or
service. The protocol and codec run in the caller's process (or in a
checkpoint service); blobs go through the native client; commits go through
the native control plane, or through a small control plane that offers a real
conditional write. There is no FUSE mount, no second namespace, and no
interposition on the data path.

This is the shape the S3, GCS, Azure, local, object, and 3FS adapters already
take. A DAOS, Lustre, CephFS, or JuiceFS adapter would follow the same shape:
the storage client supplies bytes; WeftFS supplies identity, representation,
selection, provenance, verification, and recovery.

### 6.2 Checkpoint/loader adapter

A training or inference framework can call WeftFS through an adapter that
writes codec frames into the native store and records provenance, without a
filesystem at all. The PyTorch DCP and Orbax adapters in this repository are
examples of the shape, though they currently use the reference store as their
backend.

### 6.3 POSIX namespace compatibility path

The reference store can point at a directory inside an existing native mount
(`--backend 3fs` is the 3FS POSIX transport). This is a compatibility path for
systems that expose a POSIX namespace. It still does not add a second FUSE
layer: the store's writes go through the native mount's own client.

**Explicitly not the design:** mounting the WeftFS FUSE frontend on top of
another FUSE mount. That would stack two userspace filesystems, duplicate
caching and locking, and put a second daemon on the critical path. The
reference frontend exists to exercise POSIX semantics and to make the
executable specification usable; it is not the integration mechanism.

### 6.4 What stays with the native system

| Native system keeps | WeftFS adds |
|---|---|
| Replication, erasure coding, placement, repair | Content identity and exact verification |
| RDMA, kernel bypass, client caching | Context selection and ambiguity handling |
| Scheduling, QoS, capacity management | Causal lineage and Context Roots |
| Media durability and its own failure domains | Capability/budget/quota policy |
| Namespace and POSIX semantics | Provenance, recovery planning, GC roots |
| GPU-direct I/O and its memory registration | Representation choice and exact reconstruction |

## 7. What is explicitly not claimed

* **No data-plane competition.** WeftFS is not a replacement for 3FS, DAOS,
  Lustre, BeeGFS, CephFS, JuiceFS, IBM Storage Scale, WEKA, VAST, or AIStore,
  and makes no throughput, latency, or scalability comparison against them.
* **No ownership of replication, erasure coding, RDMA, scheduling, caching, or
  GPU-direct I/O.** Those remain the backend's.
* **No deployed 3FS, DAOS, Lustre, BeeGFS, CephFS, or JuiceFS run.** The 3FS
  court is an ABI-compatible shim loaded with `dlopen`; it is not a cluster.
* **No live AWS, Google Cloud, or Azure run.** No credential or bucket was
  available. MinIO and Ceph RGW are real S3-compatible servers; `fake-gcs-server`
  and Azurite are emulators.
* **No MW2 or MW3 claim.** Today's implementation is MW1: many producers, one
  committer. There is no independent-committer protocol and no distributed
  scalable publication.
* **No multi-writer, multi-node, or fleet operation.** Writers are exclusive by
  design; distributed courts are one host and one server process.
* **No media power-loss or host-cache-loss claim.** Crash courts are
  process-fault proxies. Durability is the backend's `fsync` or provider
  acknowledgement.
* **No production readiness, security audit, support commitment, or SLA.**
* **No claim that interface compatibility is a native deployment benchmark.**
  A row in the integration matrix says what executed, not what would happen at
  scale.
* **No performance parity or superiority claim.** The published results include
  losses; the release note and `bench/RESULTS-*` documents list them.
* **No Go Azure backend.** Go has S3 and GCS; `--backend azure` in Go is
  `InvalidArgument`.
* **No writable FUSE over an object store.** Only read-only mounts over S3 and
  GCS are evidenced.

## 8. Reader check

Read the first screen as a skeptical storage engineer responsible for a
production 3FS or Lustre deployment. The three questions that decide whether
the positioning is honest:

**1. Can I see where the protocol and codec run?**

Yes. The first-screen diagram places the context protocol and WeftCodec
between AI consumers and the backend integration, and §2 and §6 state that they
run in the client, a checkpoint adapter, or a backend-specific integration
layer. §6 explicitly rejects FUSE-on-FUSE. Nothing in the first screen implies
that WeftFS owns the data path.

**2. Can I see which integrations actually executed?**

Yes. The integration matrix names each integration and its strongest executed
evidence. Local POSIX, MinIO S3, and Ceph RGW are marked *implemented and
tested on a real backend*; `fake-gcs-server` and Azurite are *emulator-tested*;
3FS is *ABI/conformance-tested only* and explicitly **not a deployed cluster**;
the other systems are *designed but not executed*; live AWS/GCS/Azure are
*inaccessible*. The categories are defined as the strongest executed evidence,
not the design intent.

**3. Is the native data path preserved?**

Yes. The first screen says the storage system remains responsible for
delivering bytes, and lists replication, erasure coding, placement, RDMA,
scheduling, caching, and GPU-direct I/O as backend concerns. §3 reduces the
WeftFS interface to `BlobPlane` and `CommitPlane`; §6.4 separates what the
native system keeps from what WeftFS adds. No row claims a backend feature as
a WeftFS feature.

If any of those answers is not visible in the first screen, the text is wrong,
not the claim. This check is recorded in R2, guarded by entries in
`docs/claims.json`, and cited from `docs/STATUS.md`.

## 9. Where the evidence lives

| Topic | Document | Receipts |
|---|---|---|
| Plane contract and capability probes | [`PLANES.md`](PLANES.md) | `planes-20261106T110154Z.json` |
| Fenced writer protocol | [`FENCING.md`](FENCING.md) | `s3-court-20261109T112958Z.json`, `ceph-rgw-20261109T112958Z.json` |
| One committer, N blob writers | [`PARALLEL-CHECKPOINT.md`](PARALLEL-CHECKPOINT.md) | `parallel-checkpoint-20261108T033000Z.json`, `parallel-checkpoint-s3-20261108T033921Z.json` |
| 3FS POSIX and USRBIO transports | [`THREEFS.md`](THREEFS.md) | `threefs-20261107T020623Z.json` |
| 3FS context-plane design (unrun) | [`THREEFS-CONTEXT-PLANE.md`](THREEFS-CONTEXT-PLANE.md) | design only |
| Local backend and crash proxy | [`store-status.md`](store-status.md) | `backend-20261104T232908Z.json`, `crash-torture-20261104T104233Z.json` |
| GCS emulator | [`../bench/RESULTS-gcs.md`](../bench/RESULTS-gcs.md) | `gcs-court-20261107T034113Z.json` |
| Azure emulator and SAS | [`../bench/RESULTS-azure-sas.md`](../bench/RESULTS-azure-sas.md) | `azure-20261107T034219Z.json` |
| Buck2 CAS prototype | [`../bench/RESULTS-buck2-cas-store.md`](../bench/RESULTS-buck2-cas-store.md) | `buck2-cas-store-correctness-20261109T113544Z.json` |
| Release-wide non-claims | [`RELEASE-v1.2.0.md`](RELEASE-v1.2.0.md) | `release-artifacts-20261109T124325Z.json` |
| Same-backend augmentation A/B/C/D/E (local and MinIO) | [`../bench/RESULTS-augmentation.md`](../bench/RESULTS-augmentation.md), [`../bench/CONTRACTS-augmentation.md`](../bench/CONTRACTS-augmentation.md) | `augmentation-local-20261109T134132Z.json`, `augmentation-minio-20261109T134257Z.json`, `augmentation-local-20261109T134434Z.json` |
