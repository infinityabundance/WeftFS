# X06 / X07 / X08 / N08 / N09 — one budget, one handle cap, packed races, restore cost

Receipts (revision `a6ab12eb667b9d77c6b61a5958c10ac367282469`, stamp `20261113T225001Z`):

| gate | receipt | result |
|---|---|---|
| X06 restore budget | `bench/receipts/restore-budget-20261113T225001Z.json` | 12/12 checks |
| X07 active-handle cap | `bench/receipts/handle-cap-20261113T225001Z.json` | 30/30 checks |
| X08 packed races | `bench/receipts/packed-races-20261113T225001Z.json` | 4/4 checks (5 Rust scenarios, 2 cross-language) |
| N08 restore cost | `bench/receipts/restore-cost-20261113T225001Z.json` | 13/13 checks |
| N09 stage budget | `bench/receipts/stage-budget-20261113T225001Z.json` | 12/12 checks |

The earlier run at revision `5b49da3f`, stamp `20261113T125506Z`, is retained
beside these; it predates the X31 admission fix. The X31 comparison and its
receipts are in `bench/RESULTS-stage-budget-fix.md`.

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
that used to oversubscribe is now clamped. X06 measures that clamp; the
generated block below carries the figures. The cost is structural: the cap, not
the machine, decides the width, while the unbudgeted 4×4 control is allowed
sixteen threads on four CPUs. That is the price of never exceeding the cap, not
a like-for-like comparison at the same width.

<!-- BEGIN GENERATED:resource-courts (scripts/perf_numbers.py) -->
* **X06**: at cap 4 the best row is **2.59 ms** p50 (2 tensor workers, 2 shard workers) and the measured default is **2.92 ms**; the unbudgeted 4×4 control is **1.91 ms**, so the cap costs **+35.7 %** on this fixture. At cap 1 the best is **8.50 ms**; at cap 8 the default reaches **1.86 ms**.
* **N08 dominant tensor**: one 64 MiB, 8-shard tensor restores in **669.41 ms** with one shard worker and **388.11 ms** with two — shard parallelism is the only restore-side lever for a single dominant tensor.
* **N08 100-tensor restore** (100 × 1 MiB, 4 tensor workers): **278.72 ms** p50 with the worker-side SHA-256 and no copy; **272.21 ms** with the pre-N08 copy restored and **273.95 ms** with the writer-side SHA-256 control. Neither change is a win at this scale. A 2-row region read is **5.43 ms**; the resumed 10-tensor usable set is **118.65 ms**.
* **X07**: 16 capped readers reach **255,808 reads/s**; at 128 readers the uncapped control reaches **257,859 reads/s** and the cap of 64 yields **66,223 reads/s** — the cap costs a factor of about four exactly where it binds.
* **N09**: running the encode and restore stages concurrently takes **0.936×** the sequential pair at cap 4 and **0.858×** at cap 8. The policy bounds both stages; it does not promise an overlap win.
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
for the 100 x 1 MiB fixture the decode is the overwhelming majority of the CPU
time, and the four threads decode at roughly four times the single-thread rate
(`decode_sum_ms / decode_wall_ms`). Manifest I/O, manifest hashing, whole-content
verification and the per-tensor SHA-256 are each one to two orders of magnitude
smaller.

Two principled changes were implemented and **measured; neither is a win at this
scale, and neither is claimed as one**:

* a whole-tensor read no longer copies the decoded buffer before verifying it
  (`WEFT_TENSOR_READ_COPY=1` restores the copy as the control arm); the copy is
  invisible in the restore wall (see the generated N08 row);
* the report's per-tensor SHA-256 moved from the ordered writer to the decode
  worker (`WEFT_RESTORE_WRITER_SHA=1` restores the writer arm); with four
  workers the writer was already waiting, so moving the hash changes nothing
  measurable (see the generated N08 row).

What *is* a win is the shape the budget recommends. A tensor cannot spread
across tensor workers when it is the only tensor, so the dominant term is a
single tensor's decode and the only lever is its shard workers. For one 64 MiB,
8-shard tensor the generated block gives both shard-worker figures; the ratio
between them is the shard-parallel gain, and a budget of four reproduces the
same wall. That is why `RestorePipelineOptions::budgeted()` now defaults to two
shard workers and `max(1, cap / 2)` tensor workers.

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

Where it is worse: overlapping the two stages has no stable win. The
generated block below carries the measured ratio; it is noise-dominated at this
scale and two runs of the same arm on this host disagreed on its sign at cap 4.
The two stages contend for the same four CPUs, so the policy bounds the damage
and does not promise an overlap win.

## Reproduce

```
scripts/resource_courts.sh            # full run, 5 reps, sizes 1,10,100
scripts/resource_courts.sh --reduced  # the verify-all configuration
```

## Claim values (checked verbatim by `scripts/check-claims.sh`)

- `x06_one_budget`: observed 1/2/4/8 active workers at caps 1/2/4/8, every configuration byte-identical
- `x06_oversize_completes`: 8 oversized admissions, 2,097,752 bytes restored, verify clean
- `x06_recommended_default`: RestorePipelineOptions::budgeted
- `x07_active_cap`: cap 64 default, asserted by observed_max_active_handles
- `x07_cap_cost`: see generated block in RESULTS-resource-courts.md
- `x07_go_parity`: WeftFS-Go store/read_pool_active_test.go
- `x08_invariant`: stated in tests/packed_races.rs and the receipt
- `x08_scenarios`: 5 Rust tests, 2 Go cross-language tests
- `n08_decode_dominant`: phases in the restore-cost receipt
- `n08_no_copy_not_a_win`: see the generated N08 row in RESULTS-resource-courts.md
- `n08_worker_sha_not_a_win`: see the generated N08 row in RESULTS-resource-courts.md
- `n08_dominant_tensor_shard_parallel`: see the generated N08 dominant-tensor row
- `n09_cross_stage`: all four caps, maintenance admitted every round
- `n09_overlap_not_a_win`: published as a mixed, noise-dominated result
- `x31_contract`: the normative contract in docs/STAGE-BUDGET.md
- `x31_repetitions`: the repetition counts in docs/STATUS.md and RESULTS-stage-budget-fix.md
