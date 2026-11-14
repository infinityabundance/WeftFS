# Multi-writer GC and concurrent retention (T024)

Status: **implemented and measured** for the Rust store (`local`, `object`,
`3fs`, `s3`, `azure`); **not implemented** in `WeftFS-Go`, whose GC remains
quiescent. Normative text: `spec/WEFT-MULTIWRITER.md` §11 and §15.9. Court:
`scripts/mw_gc_court.sh`, receipt `bench/receipts/mw-gc-20261114T101410Z.json`
(revision `fd56f32`).

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

## What the court does

| scenario | what it proves |
|---|---|
| A reservation window | a blob published under a live reservation is retained with the age gate disabled, and the later publication reads exactly |
| B kill matrix | a process kill at each publication stage, with a concurrent GC loop, loses no reachable object and the retry resolves through the shipped outcome path |
| C reclaim control | genuinely unreachable blobs *are* reclaimed in the same pass that retains roots (the assertion is not vacuous) |
| D MW3 | a coordinated pass marks every domain head; a domain-bound handle refuses |
| E readers | readers racing the sweep see exact bytes or absence, never wrong bytes |

The receipt records both sides: the reclaimed bytes in scenario C and zero
reachable-object losses across the kill matrix. The exact figures are generated
into `bench/RESULTS-mw-t024-t029.md`.

## Residual risk

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
