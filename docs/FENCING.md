# Fenced writer protocol (R17/R18, single-publication-point protocol R27)

The store admits **one writer at a time**. On a POSIX namespace the proof is a
`flock` on `LOCK`. On an object store there is no kernel lock, so the proof is a
**fenced lease**: a `LOCK` object that carries an owner, a monotonic fencing
token, an absolute deadline, and — since R27 — the head being published.

This document is the protocol specification for the object-store form. It is
implemented three times, independently: Rust `src/s3.rs`, `src/gcs.rs`,
`src/azure.rs` (one shared record parser in `src/writer_fence.rs`), and Go
`WeftFS-Go/store/s3commit.go` + `store/s3client.go`.

## 1. Why the publication point matters

The store publishes a commit as:

1. write `commits/<generation>.commit` create-once;
2. publish the head (the two-slot superblock slot);
3. optionally refresh caches.

R17/R18 wrote the fence as one conditional `LOCK` write and the head as a
**second** conditional write on a **different** object (`superblock`). That
leaves this interleaving uncovered:

```
A: fence (LOCK if-match)            ok
A: (suspended / GC pause)
                                        B: lease expired -> takeover (LOCK if-match, fence+1)
                                        B: no head published yet
A: head CAS on superblock           ok  <-- accepted under a stale token
```

Nothing in the old protocol stopped A's superblock write: the head object still
held the value A had read.

## 2. The v3 publication point

A v3 `LOCK` record is the store's **publication point**:

```
weftfs-s3-lock v3                 (or weftfs-gcs-lock v3 / weftfs-azure-lock v3)
owner=<principal>
fence=<u64, monotonic>
sequence=<u64, conditional writes by this handle>
lease_deadline_ns=<i64, absolute>
renewed_ns=<i64>
pending_generation=<u64 head generation, 0 when idle>
pending_commit=<64 hex, or "-">
pending_commit_len=<u64 WCB length, 0 when idle>
```

* The fence and the new head are written by **one** conditional write:
  `If-Match: <ETag>` (S3, Azure) or `ifGenerationMatch: <generation>` (GCS) on
  the bytes this handle last published.
* A successor's takeover rewrites the same object, so the suspended writer's
  write fails `412 Precondition Failed` and surfaces as `Busy`. There is no
  second object write left for it to win.
* Every other record write (the pre-commit fence, a renewal, the release
  tombstone) carries the current head forward, so the record never loses the
  attestation of an already-published head.
* The two-slot `superblock` remains the **publication point on local/POSIX
  backends** (a `flock` cannot be stolen by a suspended process, so the two
  writes are already serialised by the kernel). On an object-store commit plane
  it is an **attested cache**: written after the record, validated against the
  record at every open, and repaired on a writable open.

### Release

`weftfs-<backend>-lock-released v2` carries the same `pending_*` fields and is
written with a conditional write over the bytes this handle last published. It
is never an unconditional `DELETE`: deleting the object would reset the fencing
token, and an unconditional delete after a read can delete a successor's lock.
A tombstone keeps the token monotonic.

### Version history and migration

| record | readable | meaning |
|---|---|---|
| `weftfs-<b>-lock v1` | yes | `owner` + `expires_ns`; read as **fence 0** |
| `weftfs-<b>-lock v2` | yes | fenced, names the head, **no length** (reported 0 = unknown) |
| `weftfs-<b>-lock v3` | yes | the publication point (this document) |
| `weftfs-<b>-lock-released v1` | yes | tombstone, no head |
| `weftfs-<b>-lock-released v2` | yes | tombstone carrying the head |

A writable open upgrades an **expired or released** v2/v1 record to v3 *in the
same conditional write that takes the lease over*: one write, no window in which
the record is half-upgraded. A **live** v2 record refuses the open as `Busy`
exactly like a live v3 record, so two writers never run at once.

Mixing versions across time is refused rather than guessed at: if the
superblock cache names a generation **ahead of** the head the record attests, a
pre-v3 writer must have published after the upgrade. Recovery returns
`UnsupportedVersion` naming both generations. (A record ahead of the cache is
the ordinary crash window and is recovered as the new head.)

## 3. State machine

```
            no LOCK
               |  create-once (fence 1)
               v
  +------> live lease (owner, fence, deadline)  --publication--> same state,
  |            |        |                                        fence constant,
  |            |        |                                        sequence+1,
  |            |        |                                        head updated
  |            |        |
  |            |        +-- own deadline passes --> refuse (no successor needed)
  |            |        +-- taken over ----------> refuse Busy (412)
  |            |
  |   release (conditional tombstone)
  |            v
  |      released tombstone (fence kept)
  |            |
  +------------+  takeover: fence+1, new deadline, head carried forward
                 (conditioned on the exact bytes read)
```

Properties:

* **Monotonic fence.** Created at 1; every takeover is `previous + 1`; the token
  is never reused and never reset (hence the tombstone).
* **Renewal.** When one publication budget (half the lease) remains, the same
  conditional write extends the deadline. The fence does not change.
* **Own-deadline refusal.** A writer refuses to publish once its own recorded
  deadline has passed, even with no successor. A local re-check immediately
  before the commit record means a writer suspended past its deadline leaves no
  unpublished commit tail.
* **Lost CAS is `Busy`.** No blind retry: a lost conditional write is retried
  only from a fresh read and only in the takeover path.

## 4. Interleavings covered

| # | interleaving | outcome |
|---|---|---|
| 1 | A fences, is suspended before the publication write; B takes over without publishing | A's publication is one conditional write on the rewritten record -> `412`/`Busy`; head unchanged |
| 2 | A fences, publishes, then is suspended; B takes over | A's publication is already durable and is the head B inherits; nothing A does afterwards can move it (only a cache write, which never moves the cache backwards) |
| 3 | A is suspended before the fence; B takes over | A's pre-commit fence fails `412`/`Busy` (R17 test) |
| 4 | A's own deadline passes with no successor | local refusal; nothing written |
| 5 | B takes over in the rename/ETag gap of A's release | the conditional tombstone fails and B's record is byte-identical (R17 test) |
| 6 | A's publication write returns, then A crashes before the cache | the record attests the head; recovery uses it; the cache is repaired |
| 7 | A crashes after the commit file, before the record | the record still names the old head; the commit tail is invisible (old whole state) |
| 8 | Two writers race the create-once LOCK | one wins; the loser retries from a fresh read |
| 9 | A reader opens while a writer is publishing | it sees the old or the new record; both name a complete head |

## 5. Interleavings outside the guarantee

* **Clock skew.** No clock-skew defence exists: deadlines are the writer's own
  wall clock. A successor may not take over early if clocks agree; a badly
  skewed clock can extend or shorten a lease.
* **A lost network reply** to a conditional write is the usual at-least-once
  ambiguity: the write may have landed. The next conditional write fails `Busy`
  and the handle must reopen.
* **Two different objects are still touched per publication** as far as the
  *cache* is concerned: the record (publication point) and the superblock
  (cache). Only the cache write follows the publication; it is conditional,
  monotonic, and never trusted.
* **Not a multi-writer protocol.** Writers are exclusive by design. There is no
  quorum, no leader election, and no lease renewal by a third party.
* **Not a media/power-loss claim.** An endpoint that acknowledges a write and
  then loses it is outside the protocol; the courts use process-fault proxies
  and one device-model court, labelled as such.
* **No atomic fence+head operation exists on plain S3/GCS/Azure.** The single
  write *is* the strongest expression available: the conditional write of one
  object that both proves the token and names the head.

## 6. What is preserved

* Create-once blobs and the normative local layout (unchanged bytes).
* `Busy` on any lost race, with the fixed non-disclosing open refusal.
* Renewal, own-deadline refusal, conditional tombstone release.
* No unconditional delete, no silent degradation, no fallback endpoint.
* Read-only opens never write and never probe.
* v1 and v2 records remain readable.

## 7. Evidence

| court | suite | result |
|---|---|---|
| MinIO (`scripts/s3_court.sh`) | `tests/s3_backend.rs` + `tests/s3_fencing.rs` | 6/6 + 11/11 |
| Ceph RGW 19.2.0 (`scripts/ceph_court.sh`) | same | 6/6 + 11/11 |
| MinIO, Go (`WeftFS-Go/scripts/go-s3-court.sh`) | focused S3 package tests | 20 pass, 0 fail, 1 skip (crash helper) |
| `fake-gcs-server` (`bench/scripts/gcs_court.sh`) | `tests/gcs_fencing.rs` | 8/8 |
| Azurite 3.35.0 (`scripts/azure_court.sh`) | `tests/azure_fencing.rs` | 8/8 |

The new R27 acceptance test is
`paused_after_the_fence_cannot_publish_after_a_takeover` in
`tests/support/fencing.rs`, run by all three Rust fencing suites, and
`TestS3FencedWriterPausedAfterFenceCannotPublish` in `WeftFS-Go`. The hook is
`WEFT_FENCE_PAUSE_AFTER_FENCE=1`; it writes `WEFT_FENCE_AFTER_FENCE_READY` and
then suspends the writer at the exact boundary between the fence and the
publication. Rust uses `raise(SIGSTOP)` + `SIGCONT`; the Go court uses a named
FIFO (`WEFT_FENCE_AFTER_FENCE_GATE`) because Go's runtime does not deliver a
self-sent `SIGSTOP` as a group stop in that court. The interleaving is
identical; only the suspension primitive differs. A second S3 test
(`a_replaced_lock_record_makes_the_stale_publication_fail_412`) forces the same
`412` deterministically while the stale writer's own lease is still valid, so
the conditional-write refusal is isolated from the own-deadline guard.

## 8. Reproduction

```sh
bash scripts/docker-court.sh rust "cd /work && cargo test --test s3_fencing -- --test-threads=1 --nocapture"
bash scripts/s3_court.sh            # MinIO
bash scripts/ceph_court.sh          # Ceph RGW 19.2.0
bash WeftFS-Go/scripts/go-s3-court.sh --focused-only
bash bench/scripts/gcs_court.sh
bash scripts/azure_court.sh
```
