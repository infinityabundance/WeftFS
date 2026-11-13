# The one stage budget

One process-wide admission object, `weftfs::stage_budget::StageBudget`, owns the
concurrency policy for every stage that runs work concurrently: the checkpoint
write pipeline's encode workers, codec-internal threads, backend I/O, restore
workers (tensor, shard and tile) and background maintenance.

## Declared default

| field | default | derivation |
|---|---|---|
| `max_active_workers` | `available_parallelism()` (≥ 1) | the machine's CPUs; one process does not assume it owns more |
| `max_inflight_bytes` | 512 MiB | a single staging bound for all stages together |

A policy is validated at construction: `0` is rejected, so "unlimited" is not
expressible. `WEFT_STAGE_MAX_ACTIVE` / `WEFT_STAGE_MAX_INFLIGHT_BYTES` override it
for a court; an unparsable value is an error, never a fallback.

## Semantics

* **One request, one group.** A caller declares the worker width and the bytes it
  will hold. A restore admits `shard_threads × tile_threads` (clamped, shards
  first) because those nested runners are the threads that tensor will actually
  run; the tensor worker itself is part of the same grant. The write pipeline
  admits `cpu_tokens` and its worker reservation.
* **FIFO.** Tickets are served in arrival order across stages, so encode cannot
  lock out restore and restore cannot lock out maintenance.
* **Clamped, not exceeded.** A worker request larger than the cap is granted the
  cap and counted in `clamped_worker_requests`. The observed high-water mark
  therefore never exceeds the policy.
* **RAII.** `Admission` releases on drop, including during unwinding.
  `release_workers()` drops the thread slots while keeping the bytes reserved, so
  a finished decode stops counting against the worker cap while its bytes still
  count against the staging bound until the ordered writer consumes them.
* **Cancellation.** A `StageCancelToken` (per request) or `cancel_all()` wakes
  every waiter; the waiter releases its ticket and returns `Cancelled`. Grants
  already held are released by their guard, not by the canceller.

## Normative contract: oversize requests

A request is **oversize** when its declared bytes exceed
`max_inflight_bytes`. It is not admitted under the ordinary byte rule, because
no reservation of that size can ever fit. The contract is:

1. **Admitted only into an idle budget.** The request is granted only when
   `active_workers == 0` *and* `inflight_bytes == 0`. Existing holders are
   earlier tickets; they finish and release. An oversize request that arrives
   mid-stream therefore waits for them instead of deadlocking against them.
2. **Runs alone for its whole lifetime.** The grant sets an `exclusive` flag
   that blocks every later admission — including another oversize request —
   until the owning `Admission` is dropped. "Alone" is a property of the
   request's lifetime, not of the instant it was granted. This is why the
   primitive does not need to charge the oversize request's bytes to
   `inflight_bytes`: the published high-water mark stays within the bound, and a
   second worker cannot slip in beside the oversize one as it could if the flag
   were omitted.
3. **Not starved by a stream of small requests.** Admission is strict FIFO. A
   later small request receives a higher ticket and cannot overtake a queued
   oversize request; the oversize request is served as soon as the tickets
   ahead of it drain. Holding the FIFO head while it waits for the budget to
   go idle is intentional: letting later requests through would allow a
   continuous stream of small arrivals to postpone it forever.
4. **Released exactly once, by its owner.** `Admission::drop` clears the
   exclusivity flag along with the worker slots, including during unwinding. A
   panic in an oversize stage therefore cannot leave the budget permanently
   exclusive.

## Normative contract: reservation lifetime

The byte reservation belongs to the **request**, not to the worker count and not
 to whichever thread happens to be running:

* a grant charges `bytes` to the request; `release_workers()` reduces
  `active_workers` only;
* the bytes remain charged until the request's `Admission` is dropped;
* no other release path exists, and `release_workers()` is idempotent;
* an oversize request keeps its exclusivity after `release_workers()`, because
  the bytes it decoded are still live and are not represented in
  `inflight_bytes`.

## Liveness (why this cannot deadlock)

Two invariants hold under the state mutex:

* **I1 — every blocker is strictly ahead in the queue.** A grant happens only
  when `serving == ticket`. After a grant or an abandonment, `serving` advances
  past that ticket, so every live holder has a ticket smaller than `serving`,
  and `serving` is the smallest ticket that is neither granted nor abandoned.
  A waiter can therefore only wait for requests that were admitted earlier and
  are not waiting in `admit` themselves.
* **I2 — nobody waits while holding capacity it could release.** Waiting happens
  only before a grant. A holder may run work, perform I/O or wait for its
  ordered writer, but it never needs another admission from this budget (the
  nested-runner group is what `in_admitted_group()` exists to enforce).

Given I1 and I2, the wait-for graph is acyclic: a waiter waits on earlier
granted requests, and a granted request has no outgoing admission edge. Each
granted request eventually drops its `Admission` (normal completion or
unwinding), which wakes the queue and lets the next ticket, in order, be
granted. A cancelled waiter leaves the queue without consuming capacity, and
either advances the cursor (when it is the head) or records its ticket as
abandoned so the cursor skips the hole when it arrives — so cancellation also
strictly decreases the number of tickets ahead of any survivor. Because the
ticket space is finite for the waiters that exist, the head advances until every
request has been granted or has left, or `cancel_all()` releases it.

This is a bounded-progress argument, not a timeout: the primitive deliberately
has no admission timeout, because a timeout would turn a slow-but-correct
holder into a spurious failure. Callers that need one pass a
`StageCancelToken`.

## What is opt-in, and why

The historical defaults of `TensorPipelineOptions` and `RestorePipelineOptions`
are unchanged (`budget: None`). A caller opts in by constructing one budget and
passing it to both; `RestorePipelineOptions::budgeted(budget)` applies the
configuration the X06 court measured as best at each cap. Flipping the default of
every entry point would silently change every published write and restore
number, so it is left to the caller and documented here.

## Evidence

`scripts/resource_courts.sh` produces the X06 and N09 receipts; the policy table
and every measured number are in `bench/RESULTS-resource-courts.md`.
`scripts/stage_budget_court.sh` repeats the unit suite at `--test-threads=8`, 2
and 1 and the ordered-pipeline suite at `--test-threads=4`, every repetition
through a per-run `timeout`, and writes a receipt; it is standalone and is not
part of `verify-all`. The X31 defect (an oversize admission that did not block
others, a test that required a reservation to be released mid-join, a cancelled
non-head ticket that stalled the queue, and an ordered-pipeline deadlock caused
by admitting the budget out of input order) and its fix are recorded in
`docs/STATUS.md` and `bench/RESULTS-stage-budget-fix.md`.
