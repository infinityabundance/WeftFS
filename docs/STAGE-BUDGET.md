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
* **Oversize runs alone.** A byte request larger than the whole bound waits until
  no bytes are reserved; it always completes and cannot deadlock.
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
