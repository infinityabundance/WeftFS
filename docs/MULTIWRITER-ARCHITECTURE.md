# Multi-writer architecture (R4)

Normative contract: [`spec/WEFT-MULTIWRITER.md`](../spec/WEFT-MULTIWRITER.md).
Benchmark method: [`bench/CONTRACTS-multiwriter.md`](../bench/CONTRACTS-multiwriter.md).
Results and losses: [`bench/RESULTS-multiwriter.md`](../bench/RESULTS-multiwriter.md).

**Status.** MW1 (many producers, one committer) remains the default. MW2
(independent committers) is opt-in (`--writer-mode optimistic`) and implemented
for the explicit transaction API on the `local`, `object`, `3fs`, and `s3`
commit planes in Rust, and on `local`/`object` in Go. FUSE file mutations and
generic checkpoint publication are still MW1. MW3 is not implemented.

## 1. The shape of the protocol

```
 writer A                           shared store                         writer B
 ────────                           ────────────                         ────────
 prepare content blobs  ──────►  objects/ manifests/  ◄────── prepare content blobs
 (content-addressed,            (create-once, immutable)
  create-once)

 read head h  ◄──────────────  superblock / head  ──────────────►  read head h
 write txs/<id> prepared
 build commit(parent=h)
 ┌────────────── publication critical section ──────────────┐
 │ check head still h                                        │
 │ create-once commits/g.commit                              │
 │ compare_exchange_head(h -> g)                             │
 └───────────────────────────────────────────────────────────┘
 write txs/<id> committed
```

The transaction is a **single publication point** (the commit record) plus a
**single conditional head write**. The commit's ops become visible together
because a reader replays the commit, not the individual object writes. The
protocol never needs multi-object atomicity from the backend.

## 2. Transaction lifecycle in code

| state | artifact | code |
|---|---|---|
| build | `Transaction { id, base_head, ops, preconditions, auth, pins }` | `Store::begin_transaction`, `Transaction::put_*` |
| prepared | `txs/<id>` = `weft.tx.v1`, state `prepared` | `Store::prepare_transaction`, `write_tx_record` |
| publish | `commits/<g>.commit` + head CAS | `Store::publish_commit_optimistic` |
| committed | `txs/<id>` state `committed`, commit hash | `write_tx_record` |
| unknown | prepared record, no proof | `Store::transaction_outcome` → `Unknown` |
| resolved | a retry scans the bounded window by op digest | `find_tx_commit` |

`execute_transaction` first calls `transaction_outcome`. If the record is
`committed`, it verifies the commit is an ancestor and returns it. If the
record is `prepared` (ambiguous timeout or crash after the head write), the
answer is `Unknown`; a retry with the same op set scans commits from the head
back to the recorded parent generation and, on a match, repairs the record and
returns the same commit. The digest covers only the transaction id and op set,
not the attempt's parent, so a retry from a newer head resolves to the same
outcome.

## 3. Retry and rebase

Each attempt:

1. catch up to the published head (`refresh_to_head`);
2. validate every precondition against the rebased state;
3. re-evaluate authorization and quota;
4. apply the ops to a candidate state and encode the commit;
5. publish with `parent = observed head`;
6. on `Busy`, restore the last published state (the attempt's own ops are
   discarded, never carried into the retry), back off (200 µs doubling to
   25 ms, deterministic) and retry, bounded by `max_attempts` (default 8; the
   benchmark driver uses 64 and up to 3 outer retries for `outcome_unknown`).

Step 6 matters for correctness, not only for timing. Quota charges,
preconditions, and authorization are evaluated against the handle's state; a
retry that kept its own unpublished candidate would double-count a charge or
observe an entry the store never published. The regression test
`a_lost_publication_race_does_not_pollute_the_retry_state` fails if the restore
is removed.

`refresh_to_head` applies only the missing commits. It falls back to the
authoritative full replay if a parent does not match, a commit is missing or
malformed, or an op rejects. The fast path therefore cannot invent state.

## 4. Conflict table (implementation)

| class | where | outcome |
|---|---|---|
| `NameExists` | `put_entry_create` observed absent, name now taken by another target | `Conflict`, no publication |
| `ObjectExists` | `put_object_create` observed absent, id now taken | `Conflict` |
| `ObjectChanged` | replace with an observed version that is no longer current | `Conflict` |
| `EntryChanged` | delete/rename of an entry whose record changed | `Conflict` |
| `RelationConflict` | same deterministic relation id, different assertion | `Conflict` (also rejected by replay) |
| `SnapshotConflict` | named pointer moved; no automatic branch | `Conflict` |
| `TipChanged` | observed tip changed | `Conflict` |
| `QuotaExceeded` | rebased state would exceed a budget | `BudgetExceeded` |
| `CapabilityDenied` | a read-authorization check fails on the rebased state | `Unauthorized` |
| `TransactionIdReused` | same id, different op digest | `Conflict` |
| `GenerationCollision` | commit file exists at the chosen generation | `Busy` → retry |
| `ParentAdvanced` | observed head is no longer the head | retry, then validate |

There is no last-writer-wins path. A caller that intends to replace a name must
observe the existing entry (`put_entry`); a create uses `put_entry_create` and
gets `NameExists`.

## 5. Backend contracts and what they actually guarantee

| backend | head publication | commit record | publication critical section | proved by |
|---|---|---|---|---|
| `local`, `object`, `3fs` | read-check-write of the 4 KiB slot under `LOCK.publish` (OFD lock + compatibility `flock`) | `link(2)`/rename create-once under the same lock | one OFD mutex, short | `tests/multiwriter.rs`, mixed court |
| `s3` (MinIO) | one `PUT` of the superblock image with `If-Match: <ETag read with the head check>` | `PUT If-None-Match: *` | native conditional write; no mutex | MinIO benchmark, `verify --full` |
| `gcs`, `azure` | refused for optimistic mode in R4 | — | — | — |

The local `LOCK.publish` is **not** the single-writer `LOCK`. It is created
lazily by an optimistic store and is never read for correctness. A single-mode
`LOCK` is never created implicitly.

The object-store S3 path does not use the fenced lease. `docs/FENCING.md`
describes the MW1 lease; MW2 uses the superblock's conditional write instead.
`recovery` treats the superblock as authoritative when the `writers` policy
record says `optimistic`, so a stale lease record from an earlier single-writer
life cannot override the head.

## 6. Transaction and policy records

`writers` (canonical WCB `weft.writers.v1`):

```
schema = "weft.writers.v1"
mode = "single" | "optimistic"
min_reader = text
created_ns = i64
```

Absent means `single`; a single-mode open never writes it (frozen fixtures stay
byte-identical). The first optimistic open creates it create-once. A mode
mismatch is refused `UnsupportedVersion`; modes are not mixed.

`txs/<id>` (canonical WCB `weft.tx.v1`):

```
schema, id, state ("prepared"|"committed"), digest,
parent_generation, commit (hash or null), updated_ns,
pins [manifest ids, sorted]
```

`pins` is what GC marks while the prepared record is live. The record is a
discovery aid, never a source of truth for the head.

## 7. GC and prepared transactions

GC marks the head, snapshots, checkpoints, capability/context/provenance roots,
and the `pins` of every live prepared `txs/<id>` record within
`gc_grace_secs`. A record older than the grace is abandoned; its unreferenced
blobs may be reclaimed and the record deleted. A corrupt record fails the whole
pass closed rather than hiding a pin.

R4 does **not** run GC concurrently with an optimistic publisher: the pass is
an administration step and must be run when the committers are quiesced. The
prepared→GC→publish interleaving is tested; true concurrency is not claimed.
Orphan object-record removal inside GC is skipped in optimistic mode and
reported, because it would be an unvalidated logical publish.

## 8. Bounds and failure handling

* `max_attempts` (8 default), deterministic backoff, no unbounded loop.
* `outcome_window` (256 generations) bounds the proof scan.
* `max_ops` (4096) bounds one transaction; the transaction record and pins are
  bounded by the same op bound.
* No global mutex around encoding: the publication lock covers only the
  commit-record check and the head CAS.
* A lost conditional write is `Busy`; a true transport failure is `Io`; a
  missing commit/head is `Corrupt`. The transaction loop retries only `Busy`.
* A commit record created by a losing attempt is deleted only when the head
  write is **definitively** refused (`Busy`): the head still names another
  commit, so the record is unreferenced. On an ambiguous failure (`Io`,
  timeout) the record is left in place, because the conditional write may have
  landed and deleting it could destroy a published commit. An unreferenced
  record is reported by `verify`; a landed one is resolved by transaction-id
  lookup.
* The S3 commit-file `Exists` on a lost generation race is mapped to `Busy`;
  a generation is never reused with different bytes.

## 9. Deliberate non-goals and tradeoffs

* **Not lock-free and not consensus.** The local head publication is serialised
  by one OFD lock. S3 uses one conditional write with no quorum.
* **Not serializable and not snapshot-isolated across a workload.** The
  guarantee is linearizable head publication plus atomic visibility of one
  commit.
* **Not a fast path for a single writer.** Optimistic mode costs a `txs` record,
  a head read, and a CAS; the R4 benchmark shows this is not a single-writer
  throughput win.
* **No distributed conflict domains.** Two stores are two stores.
* **No automatic branching/merge.** A named pointer conflict is returned.
* **No physical durability claim.** The local path uses the existing fsync
  protocol; S3 means provider acknowledgement.

## 10. Code map

| path | role |
|---|---|
| `spec/WEFT-MULTIWRITER.md` | normative contract |
| `src/mw2.rs` | writer mode, transaction, records, conflict validation |
| `src/store.rs` | `begin_transaction`, `execute_transaction`, `transaction_outcome`, optimistic publisher, `refresh_to_head`, GC pins |
| `src/backend.rs` | `begin_publication`, `publish_head_conditional`, `Key::Tx`/`Key::Writers`, `LOCK.publish` |
| `src/s3.rs` | single-conditional-write S3 head publication |
| `examples/mw2_writer.rs` | court/bench driver |
| `WeftFS-Go/store/mw2.go`, `gc.go`, `store.go` | independent Go implementation |
| `WeftFS-Go/cmd/mw2writer` | Go driver |
| `scripts/multiwriter_court.sh`, `scripts/multiwriter_failure_court.sh`, `scripts/bench_multiwriter.sh`, `scripts/multiwriter_s3_court.sh` | courts |
| `verification/tla/WeftMultiWriter*.tla` | bounded model |

## 11. Residual work toward MW3

* conflict domains / sharded heads so independent transactions stop contending
  on one publication point;
* a shared multi-mount / multi-writer daemon instead of one process per writer;
* a coordinated multi-writer GC protocol;
* a multi-node court with a real quorum or consensus substrate and a fleet
  throughput claim;
* a low-latency conditional publication path that does not re-read the chain on
  every retry;
* Go parity for S3/GCS MW2, Azure MW2, and a generic checkpoint transaction.
