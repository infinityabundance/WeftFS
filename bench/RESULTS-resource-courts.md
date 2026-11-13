# X06 / X07 / X08 / N08 / N09 — one budget, one handle cap, packed races, restore cost

Receipts (revision `5b49da3fe44d20ec83ea244213c1bda91576ccc2`, stamp `20261113T125506Z`):

| gate | receipt | result |
|---|---|---|
| X06 restore budget | `bench/receipts/restore-budget-20261113T125506Z.json` | 12/12 checks |
| X07 active-handle cap | `bench/receipts/handle-cap-20261113T125506Z.json` | 30/30 checks |
| X08 packed races | `bench/receipts/packed-races-20261113T125506Z.json` | 4/4 checks (5 Rust scenarios, 2 cross-language) |
| N08 restore cost | `bench/receipts/restore-cost-20261113T125506Z.json` | 13/13 checks |
| N09 stage budget | `bench/receipts/stage-budget-20261113T125506Z.json` | 12/12 checks |

All five run in `scripts/resource_courts.sh` through `scripts/docker-court.sh` at the unchanged hard limits (3 GiB / 3 GiB swap / 1024 pids / 4 cpus).

## The one admission object (X06, N09)

`src/stage_budget.rs` is the single scheduler. Every concurrent stage asks it
for two resources: a number of active worker threads and a number of in-flight
bytes. The policy defaults are derived from the machine
(`max_active_workers = available_parallelism`, `max_inflight_bytes = 512 MiB`),
admission is strictly FIFO over tickets, a request larger than the byte bound
runs alone, a worker request larger than the cap is clamped (and reported), and
every grant is an RAII `Admission` that releases on drop, including during
unwinding. Cancellation is a per-request `StageCancelToken` plus a budget-wide
`cancel_all`; a cancelled waiter releases its ticket and returns `Cancelled`.

The restore path admits **one group per tensor**: the tensor worker, its shard
workers and its tile workers are one request whose width is
`shard_threads * tile_threads`, reduced deterministically (shards first) so the
product never exceeds the cap. The write pipeline's encode workers charge their
busy encoder threads and worker-reserved bytes to the same object
(`TensorPipelineOptions::budget`). Nothing is shared implicitly: the historical
defaults of both option structs remain their private bounds; a caller opts in by
constructing one budget and passing it to both.

Where it is worse than before: with a budget, a single-worker restore still
holds one worker slot, and a `threads × shard_threads × tile_threads` request
that used to oversubscribe is now clamped. X06 measures that clamp: at cap 4 the
best budgeted row is 2.61 ms p50 and the recommended default is 2.86 ms, while
the unbudgeted 4×4 control on the same fixture is 2.16 ms — the cap costs
**+21 %** on this 1.5 MB fixture because the control is allowed 16 threads. That
is the price of never exceeding four.

<!-- BEGIN GENERATED:resource-courts (scripts/perf_numbers.py) -->
<!-- END GENERATED:resource-courts -->

## X07 — a hard cap on active handles

`ReadHandlePool` bounded **idle** handles (≤64) and nothing else; a reader that
found the idle set empty opened a new handle. `MAX_ACTIVE_HANDLES = 64` is now
acquired before a handle is checked out and released by an RAII lease. Epoch
invalidation, the buffered-pack bypass, the checked roots and every error class
are unchanged; `0` disables the cap and *is* the control arm.

Measured on 64 objects of 64 KiB: at 1/4/16/64 concurrent readers the cap is not
reached (the observed high-water mark is 8–34 because reads are short) and
throughput is within noise of the control. At 128 concurrent readers the cap
does bind: the control reaches the figure in the generated block and the capped
arm falls to roughly a quarter of it, because 128 threads on four CPUs plus a
condvar handoff is a much worse shape than 128 short uncapped reads. The Go
pool (`WeftFS-Go/store/read_pool.go`) had the same defect and the same fix;
`store/read_pool_active_test.go` proves the unbounded control and the cap.

## X08 — packed-plane races

The discovery invariant is stated in `tests/packed_races.rs` (module docs) and in
the receipt. Five Rust scenarios and two cross-language ones run under one
store: racing seal/catalog/commit with three reader threads and a same-handle
read-back of every just-created member; an aborted group; a real `abort()` mid
seal; GC concurrent with readers and a writer; a cancelled reader; a Rust writer
against Go readers; and a Go writer against Rust readers. Every scenario ends
with `verify --full` clean and every live root byte-exact.

Two defects found while building the court were **in the court, not the
product**, and are recorded here so they are not mistaken for product fixes: a
read-only handle must be opened on the *packed* backend (a flat handle cannot
see members inside packs, so every read is `NotFound`), and a reader must
snapshot the published id list *before* it opens, otherwise it reads ids
committed after its own snapshot (a stale reader is allowed; that is not a torn
member). No product defect was found in X08.

## N08 — where a compressed restore spends its time

The profile (new `tensor_decode` and `logical_root` phases, plus the existing
`content_verify`, `digest`, `blob_*`) is dominated by the mode-9 payload decode:
for the 100 × 1 MiB fixture the decode is the overwhelming majority of the CPU
time, and the four threads decode at ~4× the single-thread rate
(`decode_sum_ms / decode_wall_ms`). Manifest I/O, manifest hashing, whole-content
verification and the per-tensor SHA-256 are each one to two orders of magnitude
smaller.

Two principled changes were implemented and **measured; neither is a win at this
scale, and neither is claimed as one**:

* a whole-tensor read no longer copies the decoded buffer before verifying it
  (`WEFT_TENSOR_READ_COPY=1` restores the copy as the control arm). The copy is
  invisible in the restore wall: 268.4 ms without it, 268.0 ms with it at 100
  tensors (also within noise at 1 and 10);
* the report's per-tensor SHA-256 moved from the ordered writer to the decode
  worker (`WEFT_RESTORE_WRITER_SHA=1` restores the writer arm). With four
  workers the writer was already waiting, so moving the hash changes nothing
  measurable (268.4 vs 268.1 ms).

What *is* a win is the shape the budget recommends. A tensor cannot spread
across tensor workers when it is the only tensor, so the dominant term is a
single tensor's decode and the only lever is its shard workers. One 64 MiB,
8-shard tensor restores at the two figures in the generated block: shard-parallel
restore is **~1.72×** faster, and a budget of four reproduces the same wall. That
is why `RestorePipelineOptions::budgeted()` now defaults to two shard workers and
`max(1, cap / 2)` tensor workers.

`resume` in the receipt is the T058 question measured as far as this subphase
reaches: the time to restore the first *k* tensors one at a time after a
simulated failure. It is **not** a framework forward/backward step; no framework
step was run.

## N09 — the cross-stage budget

One budget object is shared by a budgeted restore and the shipped
`TensorPipelineOptions` encoder. The stress arms run 1/2/4/8-worker policies with
the restore and the encoder concurrently, plus a tensor larger than the whole
byte budget, twelve cancellation rounds, and a maintenance thread that must not
be starved by a busy restore stage. Observed high-water marks never exceed the
cap, the oversize restore completes, every cancellation either finishes or
returns `Cancelled` with the full cap available afterwards, and maintenance is
granted every round.

Where it is worse: overlapping the two stages is not always faster. At cap 4 the
concurrent run takes 1.02× the sequential pair and at cap 8 it takes 0.88×; the
sign flips with the cap because the two stages contend for the same four CPUs.
The policy bounds the damage; it does not promise an overlap win.

## Reproduce

```
scripts/resource_courts.sh            # full run, 5 reps, sizes 1,10,100
scripts/resource_courts.sh --reduced  # the verify-all configuration
```
