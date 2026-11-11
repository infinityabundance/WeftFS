# R12c plan — credential follow-up to R12b

**Starting revision:** `d3db8aa3529a42ff135aedf426907b7ade335278` (`staging`,
subject `WIP: R14d Azure MW2, unverified (restored)`).
**Date:** 2026-11-11. **Branch:** `staging`.
**Status of this document:** plan, not evidence. A claim only exists once a
receipt under `bench/receipts/` names the command, court, fixture and revision.

Labelling: **in this session** = implemented and measured here; **planned, not
started** = nothing below has been built or run. No item below is a promise of
a number; every acceptance criterion is "measured and published, including the
loss".

## Ordered items

The order is the review's order and is dependency-driven: (1) is the only item
that reuses machinery already shipped (R12b `CheckpointSession`), so it is the
cheapest real measurement. (2) and (3) attack the two measured client/service
costs that R12b left in the 37.7 ms planned completion. (4) is the behaviour
question that (1) exposes. (5) and (6) are the two scale/lifecycle holes the
small-fixture courts cannot see. (7) is the aggregate gate.

### 1. Persistent sessions in the training-goodput court — **in this session**

* **Mechanism.** Opt-in `weft-session` configuration in
  `python/courts/training_goodput.py`: one `CheckpointSession` per
  (store, run) reused for every checkpoint in the loop, instead of one
  `save_checkpoint_async` writer/service per checkpoint. The existing `weft`
  and `weft-fast` paths stay byte-for-byte as they are; the session path is
  selected explicitly.
* **Acceptance test.** Cold (first checkpoint of a run, service started) and
  warm (later checkpoints) completion p50/p95/p99, training-step p50, loop
  wall, RSS, CPU and backend durable barriers reported separately for both
  configurations at 4/8/16/32 steps between checkpoints; every checkpoint
  restored by a fresh process with identical byte digests; `verify --full`
  clean.
* **Evidence required.** One receipt under `bench/receipts/` at the measured
  revision, raw log beside it, `bench/RESULTS-training-goodput-session.md`, and
  a test that a reused session does not weaken locking, fencing or publication
  semantics (unfinished prepare publishes nothing; a failure around final
  publication exposes old-or-new complete state).
* **Not claimed.** No queueing policy, no multi-writer training, no GPU, no
  fleet. A session holding the writer lock between checkpoints is a trade-off,
  not a free win; if it does not help under contention that is published as a
  loss.

### 2. Operation-derived incremental indexes — **planned, not started**

* **Mechanism.** Maintain the per-state `StateIndex` from the operation stream
  (apply each op to the previous index) instead of calling the whole-state
  `StateIndex::build` on every publish. The full rebuild stays the oracle the
  incremental path is differentially checked against.
* **Acceptance test.** Differential court: for randomly generated op sequences,
  incremental index results equal `StateIndex::build` results for every
  selector view; a corrupted/absent index falls back to a rebuild and answers
  identically; commit-attested index validation still rejects a forged segment.
* **Evidence required.** Selector-court receipt with rebuild-vs-incremental
  equality assertions and the measured publish-path share removed, at a named
  revision.
* **Not claimed.** Nothing about persistence format, on-disk compactness, or
  concurrent writers. Indexes stay advisory: a reject must rebuild, never
  answer from an unverified index.

### 3. DCP metadata serialisation cost — **planned, not started**

* **Mechanism.** The R12b profiler attribute ~4.2 ms of a 169-entry save to the
  pure-Python WCB metadata encoder. Candidate direction: reduce the encoding
  work (fewer temporary objects/bytes concatenations) while keeping the
  canonical bytes identical, or amortise it across the save.
* **Acceptance test.** Byte-for-byte identical metadata blob and metadata
  content root before/after; the existing DCP/Orbax metadata contract tests
  still pass; the profiled `metadata_commit`/client section shrinks by a
  measured amount.
* **Evidence required.** Before/after receipt on the unchanged R12b fixture,
  with the metadata sha256 recorded on both sides.
* **Not claimed.** No change to `weft.dcp.metadata.v1`, no new wire value, no
  claim that 20 ms total completion is reachable.

### 4. Checkpoint queueing — **planned, not started**

* **Mechanism.** Bounded persistent checkpoint workers with admission control:
  a requested durable checkpoint is never silently skipped; when the queue is
  full the caller blocks (or is refused with a typed error), and backpressure is
  measured rather than absorbed.
* **Acceptance test.** A short-interval run where saves complete slower than
  the interval: every requested checkpoint either lands or the run fails with a
  typed, non-silent outcome; measured wait/backpressure; no dropped request;
  no two workers publishing at once on one store handle.
* **Evidence required.** Receipt with the requested/landed/refused counts and
  the wait distribution; a test that a full queue refuses instead of skipping.
* **Not claimed.** No throughput claim beyond what one store's single writer
  lock permits; no multi-writer training.

### 5. 7B scale — **planned, not started**

* **Mechanism.** Extend the session/queue work to a real 7B-sized checkpoint so
  the publication bottleneck and packed-catalog amplification are measured
  beyond the ~5 MB fixture.
* **Acceptance test.** Same exactness as every smaller court (byte digests,
  source SHA-256, `verify --full`), with write phase attribution at 7B and the
  catalog/footprint cost stated per configuration.
* **Evidence required.** Receipt at a named revision inside the default court
  limits (no limit raised); if a configuration does not fit, the OOM-kill is
  the published result, not a reason to raise the bound.
* **Not claimed.** No fleet, no cloud backend, no GPU; one host, warm cache.

### 6. Persistent-session lock lifecycle hardening — **planned, not started**

* **Mechanism.** A session's writable handle must not be released only when the
  next client request arrives. Make the idle-release decision independent of
  client traffic (so an abandoned session cannot hold the writer lock for its
  lifetime), and make the release observable.
* **Acceptance test.** A session that goes idle past its window releases the
  writer lock without any further request; a second process can then open for
  writing; a session that is killed releases the lock through the existing
  process-exit path; no path releases a lock it no longer holds.
* **Evidence required.** Court/receipt proving a second writer succeeds after
  the idle window with no intervening request, plus the crash/SIGKILL case.
* **Not claimed.** No lease semantics change on S3/GCS/Azure, no multi-writer
  protocol; MW1 remains the default.

### 7. Full aggregate verification at the new revision — **planned, not started**

* **Mechanism.** Run `scripts/verify-all.sh` at the final R12c revision and
  cite it.
* **Acceptance test.** Every court passes, or an explicit, reasoned skip is
  recorded; a skip is not a pass.
* **Evidence required.** `bench/receipts/verify-<stamp>.json` naming the four
  revisions, with the per-court results file retained.
* **Not claimed.** An aggregate proves the courts ran and passed at one
  revision; it is not new evidence for any individual performance claim.

## Cross-cutting rules for every item

* Courts only, through `scripts/docker-court.sh`; hard limits unchanged.
* Same fixture, same durability conditions, same command between the compared
  configurations; a changed fixture invalidates the comparison.
* Receipts and raw logs committed separately from code.
* Every loss is published. A negative result is a deliverable.
