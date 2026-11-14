# Multi-writer architecture (R4)

Normative contract: [`spec/WEFT-MULTIWRITER.md`](../spec/WEFT-MULTIWRITER.md).
Benchmark method: [`bench/CONTRACTS-multiwriter.md`](../bench/CONTRACTS-multiwriter.md).
Results and losses: [`bench/RESULTS-multiwriter.md`](../bench/RESULTS-multiwriter.md).

**Status.** MW1 (many producers, one committer) remains the default. MW2
(independent committers) is opt-in (`--writer-mode optimistic`) and implemented
for the explicit transaction API on the `local`, `object`, `3fs`, and `s3`
commit planes in Rust, and on `local`/`object` in Go. FUSE file mutations and
generic checkpoint publication are still MW1. MW3 (causal conflict domains with
independent per-domain heads) is an **opt-in, Rust-only, local-plane
prototype** — `--writer-mode sharded`, §12 below; Go has no MW3 surface.

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

## 11. Residual work after MW3

* a shared multi-mount / multi-writer daemon instead of one process per writer,
  and one shared decoded cache so the per-handle memory bound composes;
* a coordinated multi-writer GC protocol (GC is refused on a sharded store);
* cross-domain atomic publication, which needs a two-phase record and a
  recovery court that do not exist here;
* a multi-node court with a real quorum or consensus substrate and a fleet
  throughput claim;
* a publication path that does not need to re-read another writer's chain —
  MW3 gives each writer its own head, which removes the case rather than the
  cost;
* Go parity for S3/GCS MW2, Azure MW2, MW3, and a generic checkpoint
  transaction.

## 12. MW3 design as implemented (T020)

MW3 is **opt-in** (`--writer-mode sharded`), **additive**, and **Rust-only on
the local commit plane**. It changes no MW1 or MW2 byte. The normative contract
is `spec/WEFT-MULTIWRITER.md` §15; this section is the implementation map.

### 12.1 The unit and its identity

A domain is a canonical `weft.domain.v1` record `{name, kind, scope}` and
`DomainId = H("weft/domain/v1", WCB(record))`. The identity is derived and
stable; a rename is a new domain. A domain is a *publication* boundary, not a
naming convention, so a transaction cannot drift into another domain by
changing a path prefix.

### 12.2 Keys and layout

| logical | physical |
|---|---|
| domain head | `domains/<DomainId hex>/superblock` (two 4096-byte MW1 slots) |
| domain commit | `domains/<DomainId hex>/commits/<g>.commit` |
| domain publication lock | `domains/<DomainId hex>/LOCK.publish` |
| discovery record | `domains.root` |
| blobs | `objects/`, `manifests/`, packs, indexes — **global**, content-addressed |

`Key::{DomainSuperblock, DomainCommit, DomainIndex, DomainLink}` carry these
through `StorageBackend`, so a backend that cannot address them answers
`supports_domain_heads() == false` and a sharded open is refused rather than
downgraded. The head image is the frozen MW1 slot encoding byte for byte; the
commit is the frozen commit record under a per-domain generation.

### 12.3 Publication

The existing MW1/MW2 publication path is **reused, not reimplemented**:
`Store::try_publish_staged` → `publish_commit_optimistic` with the domain's
commit key and `publish_domain_head_conditional`. The create-once commit
record, the conditional head compare-and-swap, the conflict table, quota,
authorization and the transaction record all run unchanged. The only additions
are (a) the domain keys and (b) `begin_domain_publication`, which takes an OFD
lock on the **domain's** publication file. A single global publication lock
would reproduce the exact serialisation MW3 exists to remove.

### 12.4 Discovery: per-domain registration, not a global CAS

`domains.root` is a canonical `weft.domains.v1` record with a monotonic epoch.
It changes **only when a domain is first registered**, in one compare-and-swap
that requires the observed epoch. Publication never touches it. The rejected
alternative was one discovery CAS per commit: that is a single publication
point again, and the R4 measurement (efficiency 0.0045 at N=32) is what it
looks like. Each entry carries the derived `head_key`
(`domains/<DomainId hex>/superblock`) and the record is checked against it on
decode, so the record **maps a domain to its head object**; the head value is
read fresh (`discover_heads`, `weftfs mw3 list`) and never cached in the record.
The price of the choice is that discovery is eventually complete: a reader may
need one retry or a directory listing to see a domain registered concurrently.

### 12.5 Read view

`Store::capture_read_view` reads each declared domain head **once**, in the
declared order, and returns `{pairs, view_root}` with
`capture_kind = "sequential"`. This is a capture. It is not a global
linearizable point, not a consistent cut and not a cross-domain snapshot; the
spec's isolation sentence says so normatively. `write_view_pin` stores a
`weft.view.pin.v1` record as a content-addressed blob so the capture is
auditable; it is not a retention guarantee in this prototype.

### 12.6 Refusals

| request | class | where |
|---|---|---|
| grouped transaction over ≥2 domains | `CrossDomain` | `Store::execute_transaction_across` |
| handle publishing into another domain | `CrossDomain` | `Store::execute_transaction_in` |
| sharded open on a non-local plane | `UnsupportedVersion` | `open_impl_mode` |
| single/optimistic open on a sharded store | `UnsupportedVersion` | `ensure_writer_policy` / read-only policy probe |
| GC on a sharded store | `UnsupportedVersion` | `Store::gc` |
| missing domain in a view | `NotFound` naming the DomainId | `capture_read_view` |

### 12.7 Deliberate prototype limits

Domain commits are `weftfs.commit.v1` (no index attestation; the handle records
the reason). GC is refused rather than run per domain. `weft.domain.link.v1`
records lineage only: a successor has one create-once record naming its
predecessor(s), and the protocol never copies or chooses data. Transaction ids
are store-global. Go has no MW3 surface. A sharded store has no crash court of
its own. Every limit is in `bench/RESULTS-mw3.md` §5.

### 12.8 Code map (MW3 and T021)

| path | role |
|---|---|
| `src/mw3.rs` | domain record/ID, discovery record with the derived head key, read view, view pin, domain link |
| `src/store.rs` | `open_domain_with_options_and_backend`, `register_domain`, `discover_domains`, `discover_heads`, `capture_read_view`, `write_view_pin`, `write_domain_link`, `domain_link`, `verify_domain`, per-domain keys, domain GC refusal |
| `src/backend.rs` | `Key::Domain*`, `load_domain_head`, `publish_domain_head_conditional`, `ensure_domain_layout`, `begin_domain_publication`, `list_domain_commits` |
| `src/mw2.rs` | `WriterMode::Sharded`, `touched_keys`/`changed_keys`, jittered backoff |
| `src/main.rs` | `--domain`, `mw3 register`, `mw3 list` (with heads), `mw3 capture`, `mw3 pin`, `mw3 link` |
| `tests/mw3.rs` | 13 semantics/refusal/lineage tests |
| `examples/mw2_writer.rs`, `examples/mw3_compact.rs` | process-per-writer and compact-client drivers |
| `scripts/mw3_court.sh`, `scripts/mw3_merge_runs.py`, `scripts/mw3_verify_court.sh` | the courts, the median merge, and the aggregate wrapper |
