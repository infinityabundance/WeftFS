# Multi-writer GC and concurrent retention (T024)

Status: **implemented and measured** for the Rust store (`local`, `object`,
`3fs`, `s3`, `azure`); **not implemented** in `WeftFS-Go`, whose GC remains
quiescent. Normative text: `spec/WEFT-MULTIWRITER.md` §11 and §15.9. Court:
`scripts/mw_gc_court.sh`, receipt `bench/receipts/mw-gc-20261114T214308Z.json`
(revision `2b93e5f`); repetition proof `scripts/multi_writer_gc_stress.sh`,
receipt `bench/receipts/multi-writer-gc-stress-20261114T214326Z.json`.

## The retention contract

A pass may reclaim a blob only after it has observed, in this order:

1. the **epoch** it acquired by compare-and-swap on `gc/epoch`;
2. every **publication reservation** taken at or before that epoch;
3. the **published head** — plus every MW3 domain head.

Only then may it mark and sweep. The roots are: head, named snapshots,
checkpoint generations, every domain head and the snapshots/checkpoints inside
each domain, live prepared transactions (`txs/<id>`, including their `pins`),
capability/context/provenance/retention roots, provenance inputs and receipt
blobs while an output is retained, pack catalogs and their pending members, and
the index segments named by a retained commit.

A blob is a candidate only when it is unmarked **and** older than both the
pass's cut and its floor. In `optimistic`/`sharded` modes the cut is the
acquisition instant minus one second of clock slack; in `single` mode it is the
exact instant, because the pass holds the exclusive writer lock. The floor is
the earliest live reservation's start, minus the same slack.

## The mechanism, and why

**Epoch plus per-writer reservation records**, not a shared in-memory retention
set.

* A reservation is a small durable record (`weft.tx.reservation.v1`) written
  before the writer's first blob and deleted only after its commit is durable.
  It survives a process crash, which an in-process set cannot.
* The epoch CAS gives the pass a single linearization point. A reservation
  registered after the CAS is newer than the cut; one registered before it is
  in the floor. There is no window in which a blob is neither "behind a
  reservation" nor "newer than the cut" nor "named by the head".
* A shared set would need a lock or consensus to be read atomically with the
  head; the epoch record is a compare-and-swap the backend already supports
  (`planes::CommitPlane`).

The cross-writer case falls out of the same rule: writer A's prepared
transaction registered before B's pass acquired its epoch, so B sees A's
reservation and its floor protects A's blobs. If A registered after the CAS, its
blobs are newer than the cut. If A already committed, B's head read names them.
A GC started by writer B cannot reclaim writer A's prepared blobs.

The publisher side is closed for the helpers: `txn_create_file`,
`txn_overwrite_file` and the FUSE mutation path re-read their reservation
immediately before the commit and refuse with `Busy` if it is gone or expired.

### Contention: the acquisition waits, the close is conditional

Two passes may overlap, so both `gc/epoch` writes are compare-and-swaps:

* **Acquisition** retries a lost swap with the deterministic exponential
  backoff the writer contract already states for a rebase
  (`spec/WEFT-MULTIWRITER.md` §7): 200 µs doubling to a 25 ms cap, no jitter.
  The wait is bounded by `EPOCH_RETRY_BUDGET` (3 s) of wall clock, **not** by a
  retry count, because what a pass waits out is the winner's read-verify-write
  of the record *plus the `fsync` calls that publish it* — a critical section
  that grows with storage latency. After the budget the caller gets `Busy`
  `another GC pass holds the epoch; retry after it closes`, and no epoch was
  consumed: a refused pass has mutated nothing.
* **Closing** the epoch is conditional on the record still being the bytes this
  pass published at acquisition. A pass that acquired epoch *N* and finishes
  after a later pass closed *N+1* leaves the newer record alone and reports
  `gc_epoch_closed = false`. An unconditional replace would write *N* over
  *N+1*: the epoch would go backwards, the record would name the older pass and
  the older cut, and a later reader could not tell which acquisition was newest.
* The **sweep** itself tolerates its twin: two passes can name the same
  unreachable blob, and the loser's unlink returns `NotFound`. "The blob is
  gone" is the outcome both wanted, so a lost race is not an error, and only
  blobs a pass actually unlinked are counted in `deleted_bytes`.

## What the court does

| scenario | what it proves |
|---|---|
| A reservation window | a blob published under a live reservation is retained with the age gate disabled, and the later publication reads exactly |
| B kill matrix | a process kill at each publication stage, with a concurrent GC loop, loses no reachable object and the retry resolves through the shipped outcome path |
| C reclaim control | genuinely unreachable blobs *are* reclaimed in the same pass that retains roots (the assertion is not vacuous) |
| D MW3 | a coordinated pass marks every domain head; a domain-bound handle refuses |
| E readers | readers racing the sweep see exact bytes or absence, never wrong bytes |
| F concurrent passes | two independent processes run 12 passes each over one store: every pass acquires its own epoch (1..24, contiguous, disjoint across the loops), none is refused, and `verify --full` is clean |

The receipt records both sides: the reclaimed bytes in scenario C and zero
reachable objects lost across the kill matrix. The exact figures are generated
into `bench/RESULTS-mw-t024-t029.md`.

## Test determinism

`tests/multi_writer_gc.rs` releases the two racing passes on a
`std::sync::Barrier` immediately before `gc`, so the overlap is guaranteed by
construction and no assertion depends on which thread wins. It asserts the
contract — distinct consecutive epochs, no refusal, the record naming the
newest pass, a clean `verify` — and never an ordering.

The *ordering-specific* half of the contract (an older pass finishing last) is
not reachable through the public API: `acquire_gc_epoch` and `close_gc_epoch`
are one call inside `gc`, so a test cannot hold an epoch open across another
pass. That interleaving is therefore proven deterministically in
`src/store.rs::gc_epoch_record_tests`, which drives both writes directly:
acquire on A, acquire on B, close B, then close A, and require the record to
stay on B's epoch. With the old unconditional close that test fails with
`a superseded pass must not rewrite the record`.

The acquisition half *is* reachable from a test, and the barrier makes it
sharp: with the old tight retry loop, 50 of 50 runs of that test fail with the
CI message once the store is on a real filesystem; with the backoff, 0 of 50
do. The sleeps that remain in this file wait on the one-second clock slack (a
wall-clock threshold that can only grow under load), never for another thread.

## Residual risk

* A pass that loses the epoch compare-and-swap for longer than
  `EPOCH_RETRY_BUDGET` (3 s of wall clock) is refused `Busy`. That is a bounded
  refusal, not a correctness failure — the pass consumed no epoch and mutated
  nothing — but it is a real outcome under a saturated filesystem, and the
  caller must retry. The wait does not serialise the sweeps: only the epoch
  number is agreed by the CAS, and two sweeps may still overlap by design.
* The *order* in which two overlapping passes finish is scheduler-dependent and
  is deliberately not asserted by any test; see “Test determinism” above for
  where the ordering-specific invariant is proven instead.

* A publisher that stalls past its reservation deadline and then publishes
  **without** the helper check is outside the protocol; the pass may have
  reclaimed its uncommitted blobs. This is not claimed safe.
* The pass is a single-store administration operation. Physical power loss,
  host cache loss, media failure and a fleet-wide GC are not claimed.
* `local`/`object`/`3fs` GC also republishes logical state for orphan removal.
  The reservation/epoch machinery covers blob reclamation; it does not turn the
  orphan-removal commit into a transaction.
* Go parity is absent. `WeftFS-Go` still refuses GC while a multi-writer handle
  is live; no cross-language GC claim is made.
