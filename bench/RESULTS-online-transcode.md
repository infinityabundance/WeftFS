# P06 — online background transcode with a real budget

Receipt: `bench/receipts/online-transcode-20261115T235938Z.json` (stamp
`20261115T235938Z`, WeftFS revision `4ab9ed2`). Raw log beside it. Module:
[`src/online_transcode.rs`](../src/online_transcode.rs); tests
[`tests/online_transcode.rs`](../tests/online_transcode.rs).

## 1. What was built

`transcode` already re-encoded objects into a better representation, but only as
an exclusive pass: nothing else could write while it ran. P06 makes it a
**background worker** that shares one store with foreground writers:

* `OnlineTranscoder` walks the object set in a deterministic order and calls
  `step_once` per object. Between steps it **yields** the store so foreground
  writes proceed.
* Every step is admitted through the existing `StageBudget` (a bound on active
  workers and in-flight bytes). An object larger than the budget is skipped and
  counted, never admitted anyway.
* The object's logical identity is unchanged: `transcode` publishes a **new
  version** of the object with the same content root and the same ObjectId. The
  previous representation stays reachable — by history and by any snapshot — until
  the new one is published, and readers see one representation or the other.
* A killed worker leaves the previous representation intact and reachable: the
  only mutation is the publish, which is atomic.

## 2. Method

Harness `examples/online_transcode_court.rs`, court
`scripts/online_transcode_court.sh`, rust court. Fixture: 24 objects of 1 MiB
BF16 (512 × 1,024), written with a fast ZSTD strategy so there is genuine work
for the transcoder. Foreground workload: 60 range writes of one row each — a range
write re-encodes the whole tensor on this store, so it is a realistic, expensive
foreground operation and a hard test of interference.

| arm | what it does |
|---|---|
| `quiet` | the 60 foreground writes with no worker |
| `busy` | the same 60 writes with the worker active |
| `crash` | a real `SIGKILL` of the worker process, then reopen and verify |

The budget is one active worker and in-flight bytes equal to one object plus one
byte. The worker's share of wall time is the time it held the store divided by
the foreground wall **plus the two-second measurement window** (the window in
which the worker runs alone is not counted against it), so interference is
reported both as a latency ratio and as a cause.

## 3. Results

<!-- BEGIN GENERATED:online-transcode-table (scripts/perf_numbers.py) -->
| metric | quiet | busy |
|---|---:|---:|
| foreground writes | 60 | 60 |
| p50 ms | 571.1 | 578.9 |
| p95 ms | 613.5 | 618.8 |
| p99 ms | 617.1 | 623.3 |
| busy / quiet p50 | — | 1.014× |
| busy / quiet p99 | — | 1.010× |
| worker held the store | — | 2,682.2 ms of 34.63 s (7.32 %, share 0.0732) |
<!-- END GENERATED:online-transcode-table -->

<!-- BEGIN GENERATED:online-transcode-prose (scripts/perf_numbers.py) -->
The court ran 5 of 5 checks and all passed. The worker applied 4 objects in 5 steps (1 empty), read 6,291,456 B and saved 236,901 B; inside the measured window it applied 3 objects and saved 177,677 B, i.e. 1.50 objects/s and 0.085 MiB/s against a declared budget of one active worker and 1,048,577 in-flight bytes (0 oversized admissions, 0 budget refusals). It yielded the store 32,942 times.
Crash arm: 24 of 24 objects verified exact, 2 had been rewritten before the kill, 24 of 24 pre-run snapshot entries still name the old representation, and `verify --full` reports 0 problems.
<!-- END GENERATED:online-transcode-prose -->

## 4. Losses

1. **Foreground writes are slower while the worker runs.** The p50 and p99 ratios
   are above 1.00; they are small, but the interference is real and is published
   as a cost, not rounded to zero. The worker's hold share is the mechanism: a
   foreground write that arrives while the worker holds the store waits.
2. **The worker is slow in absolute terms.** It applied a handful of objects in
   the run and saved a small fraction of the bytes it read. On a fixture whose
   objects are already compressed there is little left to gain; the transcoder's
   rate is bounded by the encoder, not by the budget.
3. **The budget bounds admission, not rate.** `max_active_workers = 1` and an
   in-flight byte bound say nothing about how many objects per second the worker
   achieves; that is measured separately and reported.
4. **An oversized object is skipped, not split.** An object larger than the
   in-flight budget is counted as an oversized skip and never transcoded by this
   path; a streaming transcode is not implemented.
5. **Whole-object rewrite.** Transcode publishes a new whole-object version; it
   does not rewrite a range, so the write amplification of a large object is the
   full frame.
6. **One worker per store, one process.** Two concurrent transcode processes are
   not part of the protocol; the store's single-writer rule still applies to
   concurrent *writers*.
7. **Crash safety is a process kill.** The crash arm is a real `SIGKILL`, not
   physical power loss; the store's ordinary crash guarantees apply, but no
   power-loss claim is made here.
8. **Rust only.** No Go online transcode, and no fleet or cloud claim.

## 5. Non-claims

* No claim of zero interference: the measured ratio is above 1.00.
* No claim that the worker converges on a whole store; the court runs a bounded
  number of steps.
* No claim about foreground *fairness*: the worker yields between steps, but a
  step that is already admitted is not preempted.
* No change to any frozen byte, mode number, root or planner decision.
