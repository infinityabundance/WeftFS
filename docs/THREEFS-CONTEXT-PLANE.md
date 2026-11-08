# 3FS context plane

**Status:** design note.
No experiment in this document was run against a 3FS deployment.
The only 3FS code that has executed is the USRBIO ABI conformance
court described in `docs/THREEFS.md` (a shim over POSIX `pread`/`pwrite`). Every
number below is either a measured *WeftFS* number with the local backend (cited
to its receipt) or an explicitly modelled budget; where a number is a model it
says so.

## 1. One-sentence layering statement

> WeftFS sits **above** 3FS: 3FS is an unordered, high-throughput byte store for
> immutable blobs and a POSIX namespace for control state, while WeftFS owns
> identity, ordering, authorization, and the context plane — so a 3FS cluster
> never needs to understand a provenance receipt, a capability envelope, a
> tensor tile, or a commit record to store and return them exactly.

## 2. Layering

```
┌──────────────────────────────────────────────────────────────────────┐
│ consumers: training restarts, agent sessions, evaluation jobs,       │
│            inference servers, CI caches                              │
└───────────────┬──────────────────────────────────────────────────────┘
                │ WeftFS API (Rust store / CLI / FUSE / Python adapters)
┌───────────────▼──────────────────────────────────────────────────────┐
│ WeftFS context plane                                                 │
│                                                                      │
│  identity       ContentId H("weft/chunk/v1", bytes)                  │
│                 LogicalTensorRoot, TileRoot, DescriptorId            │
│  ordering       generation, commit chain, snapshots, checkpoints     │
│  authorization  POSIX + ACL + authenticated capability envelopes      │
│  provenance     weft.provenance.v1 receipts, recovery planner        │
│  context        Context Roots (selector + snapshot + causal closure)  │
│  representation codec modes 0–17, tensor tiles, KV pages, deltas     │
│  retention      mark/sweep GC, pins, compaction                      │
└───────────────┬──────────────────────────────────────────────────────┘
                │ BlobPlane (get/get_range/put_if_absent/delete)
                │ CommitPlane (read_control/compare_exchange/lock/sync_dir)
┌───────────────▼──────────────────────────────────────────────────────┐
│ WeftFS backend adapters (one contract, interchangeable planes)        │
│  local POSIX · object (link-publish) · S3 · GCS · Azure · 3FS        │
└───────────────┬──────────────────────────────────────────────────────┘
                │ 3FS POSIX mount            or    hf3fs USRBIO rings
┌───────────────▼──────────────────────────────────────────────────────┐
│ 3FS data plane: CLUSTER / METADATA SERVICE / STORAGE SERVICE         │
│  (bytes, replicas, erasure coding, RDMA)                             │
└──────────────────────────────────────────────────────────────────────┘
```

Two 3FS transports, one on-disk layout (`docs/THREEFS.md`):

| Transport | Data plane | Control plane |
|---|---|---|
| `--backend 3fs` (default `--threefs-mode posix`) | positional `pread`/`pwrite` through the 3FS FUSE mount | `fsync` + `rename` + directory `fsync` |
| `--backend 3fs --threefs-mode usrbio` | hf3fs rings: shared region, read ring + write ring, batched submission | unchanged (POSIX) |

Control state **never** enters a ring. Commit records, the two-slot superblock,
the writer lock, refs, and indexes are published with the ordinary POSIX
sequence, so 3FS sees exactly a small filesystem workload on the control plane
and large sequential/positional I/O on the blob plane.

## 3. Which operation goes where

| Operation | Context plane (WeftFS) | Data plane (3FS) |
|---|---|---|
| Write a tensor/checkpoint | codec encode, identity, plan, publish commit | one PUT per immutable blob (pack-sized), POSIX write for the commit record |
| Read a tensor | authorize, resolve dependencies, verify roots | range reads for frame + dependency blobs |
| Read a 1-row region of a tiled tensor | decode only intersecting tiles, verify tile roots | one range read per intersecting tile |
| KV page read | capability check (tenant/model/session/token range), decode mode 12 | one blob read |
| Provenance receipt | canonical WCB, `ProvenanceId`, relation, GC root | one blob (usually ≤ 2 KiB) |
| Snapshot / checkpoint | commit record + `refs/snapshots` pointer | two small POSIX writes |
| GC | mark from state/snapshots/checkpoints, delete unreachable keys | delete by key (`unlink`/`DELETE`) |
| Fenced writer | monotonic fence token, lease deadline | conditional publication primitive on the control plane only |

The split matters because 3FS is an outstanding *byte* store and has no notion
of a commit: if 3FS were asked to arbitrate publication, a crash could expose a
half-published family. Keeping publication on a conditional control record is
what makes "old whole or new whole" true regardless of the data plane.

## 4. What the USRBIO court did and did not prove

**Proved** (receipt `threefs-20261106T232643Z.json`, rust court, 5 repetitions):

* the C-ABI transcription (`hf3fs_iov`, `hf3fs_ior`, `hf3fs_cqe` sizes, offsets,
  alignments) matches the published header, enforced by `const` assertions in
  Rust *and* `_Static_assert` in C;
* registration lifetime is correct on every path, including an injected
  `prep_io` failure (zero live registrations afterwards);
* buffers handed to `hf3fs_prep_io` always lie inside the registered region;
* batching structure: 1,500 reads became **50 `hf3fs_submit_ios` calls** (≤ 31
  per ring-full), with **byte-identical** output to the positional path;
* a store round trip over both transports reaches the same generation and the
  same snapshot root; `verify --full` passes over `usrbio`.

**Did not prove:**

* that USRBIO is faster. On the shim it is **1.127× slower** (9.899 ms vs
  11.159 ms for 300 × 256 KiB) because the shim is `pread` inside
  `wait_for_ios`, and the adapter registers a descriptor per read (1,500
  registrations in the receipt);
* any RDMA, kernel-bypass, multi-node, failure, or durability property of 3FS;
* that the fenced-writer protocol works on 3FS (the fence lives on the control
  plane, and the only 3FS tests were single-node);
* anything about a real 3FS lease or lock interacting with the store's `LOCK`.

## 5. Latency budget of a context plane on the hot path

Measured WeftFS numbers (local backend, warm cache, `weft/bench-court:1`):

| Item | Measurement | Receipt |
|---|---|---|
| capability check, verified grant in hand | **15 µs** fixed (33.0 − 18.0 µs at 1 KiB; 203.9 − 188.7 µs at 64 KiB) | `kv-capability-20261107T152958Z` |
| capability check, envelope re-verified per read | **63 µs** fixed (≈ 48 µs is Ed25519) | same |
| denied read (fails before decode) | 6.4 µs | same |
| 1-row region read of a tiled tensor | **5.2 ms**, 2.9 MB fetched (qwen); 1.46 ms, 0.36 MB (pythia) | `family-lifecycle-20261107T132359Z` |
| same read with no tile representation | 2,367 ms, 334 MB | same |
| full family restore, 1 / 4 threads | 8.75 / 3.97 s (qwen); 1.03 / 0.79 s (pythia) | same |
| provenance receipt fetch (`recover --plan`) | 68.6 ms wall for 72 fetches + 18 decodes | `provenance-20261104T235013Z` |

Budget for a KV/tensor read on a deployed 3FS cluster:

```
read latency  = 3FS data fetch + WeftFS decode + context checks
              ≈ 3FS fetch (unmeasured) + 5.2 / 1.5 ms (measured decode)
                + 15 µs (cached capability)  [or + 63 µs if re-verified]
```

The context plane is therefore **≈ 0.3 % of a tiled region read** and **≤ 8 % of
a 64 KiB KV page decode** with a cached grant, and **≈ 4.6×** the local
decode-only cost of a 1 KiB page. The single most important budget decision is
consequently *verify a grant once per session, not per page*. Whether that is
enough to keep the context plane off the critical path of a **deployed** 3FS
cluster is not established here: the 3FS data-fetch term is unmeasured, and the
comparison above is local. With per-read verification the plane can dominate a
very small page; whether it does on a cluster is the unrun experiment in §6.

For a write, the context plane adds codec encode + identity + one commit
publication. The family lifecycle court measures **11.2–159.1 s per full-family
ingest** (76 tensors for pythia-70m, 290 for Qwen2.5-0.5B; median of 3 reps per
configuration, `bench/receipts/family-lifecycle-20261108T053655Z.json`). That is
**148–549 ms per tensor** (11,216.6 ms / 76 = 147.6 ms; 159,131.2 ms / 290 =
548.7 ms), not the tens-to-hundreds of microseconds an earlier revision of this
note claimed: the court records **seconds**, and the earlier text was wrong by a
factor of 1 000. There is also one `fsync`-equivalent per commit on the control
plane.

Because that measurement is a local POSIX/SSD store, it says nothing directly
about a deployed 3FS cluster. A 3FS deployment must not put the control plane on
a latency-critical path — the commit is a small POSIX write + `fsync` + `rename`
+ directory `fsync` against the 3FS metadata service — but whether the context
plane *is* responsible for a latency miss on such a cluster is **untested**. The
experiment in §6 (control-plane `fio` baseline, metadata-service counters,
per-request latency) is the one that decides it, and until it runs the honest
statement is conditional: at the measured local rate the context plane is
unlikely to dominate a multi-second ingest, but no deployed-3FS latency claim is
made here.

## 6. The exact experiment that would validate this on a deployed 3FS cluster

Not run. To be run by an operator with a real cluster:

1. **Topology.** ≥ 3 storage nodes, ≥ 1 metadata service, ≥ 2 clients on
   different hosts, one FUSE mount per client, `hf3fs` userspace library present
   (`WEFT_3FS_LIB`), and one shared store root on the 3FS mount.
2. **Control-plane baseline.** `fio` or `mdtest` on the same mount for the exact
   control workload: 4 KiB write + `fsync` + `rename` + directory `fsync`, at
   the store's commit rate, 3 repetitions. Record p50/p95/p99 and the 3FS
   metadata service's own counters. This — not throughput — is the number that
   decides whether the design is serviceable.
3. **Data-plane baseline.** `fio` sequential write and random 4 KiB/64 KiB/1 MiB
   read at 1/4/16 jobs against the same mount.
4. **WeftFS over 3FS, positional.** Re-run `bench/scripts/checkpoint_family_court.sh`
   with `--backend 3fs` (POSIX transport) on the Qwen family, and
   `scripts/threefs_court.sh` against the *real* library rather than the shim.
   Required invariants: identical content roots and head as the local run,
   `verify --full` clean, `restart` after killing a client mid-ingest leaves
   old-or-new whole.
5. **WeftFS over 3FS, USRBIO.** Same, `--threefs-mode usrbio
   --threefs-require-mount`. Report `direct_reads` vs `staged_reads`,
   registrations, submissions, and per-request latency; compare against step 4
   with a *steady-state* registration policy (register once per handle). The
   shim's 1.127× slowdown is not a prediction for this.
6. **Fault matrix, on the cluster.** Kill a client during commit; stop one
   storage node during a read; stop the metadata service during publication;
   restart each and verify old-or-new whole, `fsync` durability after a client
   host reboot (not a media power-loss claim), and that the writer lock is
   reclaimable only after its lease.
7. **Multi-writer interlock.** Two clients, one store: the second must be refused
   `Busy`, and a `SIGSTOP`ped holder must be fenced — the same S10 scenario the
   MinIO/Ceph court already runs, repeated on 3FS.
8. **Context plane cost on the cluster.** The KV capability benchmark and a
   1-row tile read, so the 15 µs / 63 µs figures are replaced by cluster numbers
   rather than extrapolated.

Only after steps 1–8 may any 3FS throughput, latency, durability, or fleet claim
be made.

## 7. Why the context plane belongs above 3FS

* **3FS is content-agnostic by design.** It replicates and erasure-codes bytes;
  it does not know that two blobs form a checkpoint, that a third is a
  provenance receipt naming the first two, or that a KV page belongs to tenant
  A's session 42. Encoding that in the storage layer would force every 3FS
  client to agree on a schema.
* **Identity must be computed from the bytes, not stored beside them.** A
  `ContentId` that the data plane could rewrite would break every downstream
  guarantee (dedup, delta bases, provenance, GC roots). WeftFS computes it
  before publication and verifies it on read.
* **Authorization is a per-artifact decision.** The KV court shows a
  capability pinned to one page's descriptor id. A shared POSIX mount can only
  express ownership and mode; the tenant/session/token-range binding has to live
  above it.
* **Ordering is a control-plane problem.** 3FS has no compare-and-swap on a
  byte range; WeftFS's commit record and superblock give a linearisation point
  that is independent of the data plane's consistency model.

The design therefore keeps 3FS exactly where it is strongest — a fast, replicated
byte store and a POSIX namespace — and pays a small, measured, fixed cost per
artifact for the context the cluster cannot express.
