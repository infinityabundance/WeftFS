# X31 — the stage-budget defect and its fix

The shipped `StageBudget` (`src/stage_budget.rs`, added by `4d55d74`, not on
`main`) had three defects. The two an independent session reported were a
failing test and a deadlocking test; the third was found while reproducing them:
a waiter cancelled while it was **not** at the head of the FIFO left its ticket
behind, and every later request then waited forever. Fixing the first of those
exposed a fourth, a real deadlock in the ordered restore pipeline, which is also
recorded here.

The contract and the liveness argument are normative in
[`docs/STAGE-BUDGET.md`](../docs/STAGE-BUDGET.md). This file records the
reproductions, the fixes, the evidence and the numeric deltas.

## Reproduction at `0a7beb7` (the pre-fix tree)

The code was unchanged between `4d55d74` and `0a7beb7`. Both reported tests were
run individually in the rust court with an inner `timeout`.

1. **Oversize did not run alone.** The waiter was granted while the oversize
   admission was still live. Verbatim from
   `bench/receipts/stage-budget-prefix-oversize-20261113T224055Z.log`:

   ```
   thread 'stage_budget::tests::a_byte_request_larger_than_the_budget_runs_alone_and_completes' (76) panicked at src/stage_budget.rs:696:9:
   41.358µs
   ```

   The assertion required the waiter to have waited at least 25 ms. Mechanism:
   an oversize grant stored `bytes: 0`, so the byte test
   (`inflight_bytes + bytes <= max`) passed for every later request and they
   were admitted beside it. `exclusive` did not exist.

2. **The reservation test required its own deadlock.** Verbatim from
   `bench/receipts/stage-budget-prefix-hang-20261113T224055Z.log`:

   ```
   running 1 test
   test stage_budget::tests::releasing_workers_keeps_the_byte_reservation ...
   ```

   and the command exited **124** after `timeout 25` (recorded on the host; the
   log ends at the instant the process was killed). The test joined a worker
   whose one-byte request could not be admitted while the main thread still held
   the 64-byte reservation, and the main thread only dropped that reservation
   *after* the join. The reservation lifetime itself was correct; the test
   asserted the wrong thing.

3. **A cancelled non-head waiter left a FIFO hole.** A throwaway public-API test
   against `0a7beb7` queued three waiters behind one holder, cancelled the
   middle one and then the head, dropped the holder, and required the tail to be
   served. Verbatim from
   `bench/receipts/stage-budget-prefix-fifo-hole-20261113T224055Z.log`:

   ```
   thread 'cancelling_a_non_head_waiter_does_not_stall_the_queue' (2457) panicked at tests/stage_budget_hole.rs:21:9:
   worker did not finish in 5s: FIFO hole
   ```

   Mechanism: `abandon` advanced `serving` only when the cancelled ticket was
   the head. A cancelled middle ticket was forgotten, the cursor later pointed
   at it, and no waiter owned that ticket any more.

## Reproduction of the fourth defect (`a6ac2b2`, oversize alone in place)

`tests/restore_budget.rs::a_tensor_larger_than_the_whole_budget_runs_alone_and_completes`
hung intermittently. The committed log
`bench/receipts/stage-budget-prefix-pipeline-hang-20261113T221305Z.log` ends
with a `ok` line for every test except the oversize one, which never reported.

Mechanism: the ordered restore pipeline granted its own staging reservations in
input order but called `StageBudget::admit` in whatever order the workers
reached it. With exclusive oversize accounting:

```
index 3 (oversize) admits first and holds the budget exclusively
index 0             blocks in admit
index 3             finishes decoding, waits for the ordered writer (cursor 0)
index 0             cannot admit, so cursor 0 never advances
index 3             is never consumed, so it never releases  -> cycle
```

The pipeline now takes a second input-order turn (`Queue::next_to_admit`) for
the budget call. Three unit tests pin it: order, no queue lock held while
waiting, and forward progress.

## The contract now implemented

* An oversize request is granted only when `active_workers == 0` **and**
  `inflight_bytes == 0`; it reserves no bytes (so the published byte high-water
  mark never exceeds the bound) and holds an `exclusive` flag until its
  `Admission` is dropped, including during unwinding. While the flag is set no
  other admission is granted, including a second oversize request.
* Strict FIFO means a stream of later small requests cannot overtake a queued
  oversize request, so it cannot be starved. Holding the head while waiting for
  the budget to go idle is deliberate.
* The byte reservation belongs to the request. `release_workers()` reduces only
  `active_workers`; the bytes are released only by `Admission::drop`. An
  oversize admission stays exclusive after `release_workers()`.
* A cancelled waiter either advances the cursor (head) or records its ticket as
  abandoned, and the cursor skips abandoned tickets when it arrives. No
  cancellation can leave a hole.
* The ordered restore pipeline acquires the shared budget in input order, so a
  holder is always a prefix of the writer's cursor.

## Evidence

`scripts/stage_budget_court.sh` runs the whole module at `--test-threads=8`, 2
and 1 and the ordered-pipeline suite at `--test-threads=4`, each repetition
through `timeout 120`, recording one TSV row per run (exit code 124 = recorded
hang). Receipt
`bench/receipts/stage-budget-fix-20261113T224055Z.json` at revision `a6ab12e`:

<!-- BEGIN GENERATED:stage-budget-fix-proof (scripts/perf_numbers.py) -->
<!-- END GENERATED:stage-budget-fix-proof -->

The module suite contains the two rewritten tests (oversize alone, reservation
lifetime) plus adversarial cases: oversize arriving while smaller holders hold
bytes, a stream of small requests queued behind it, two oversize requests, a
pool of waiters where half are cancelled (head and holes), a waiter that gives
up without being admitted, and unwinding out of an oversize holder. The
pipeline suite includes the mid-stream oversize case that reproduced the fourth
defect.

Full suites at the same revision, each in its own court (the FUSE tests need
`/dev/fuse`):

<!-- BEGIN GENERATED:stage-budget-fix-suites (scripts/perf_numbers.py) -->
<!-- END GENERATED:stage-budget-fix-suites -->

Neither `#[ignore]` is new and neither is skipped by a filter: ownership needs
`CAP_SETUID`, which the court deliberately drops, and same-inode direct-I/O
concurrency hits a documented kernel+fuser EIO (`docs/fuse-status.md`).

A first full-suite run at `a6ac2b2` failed and is retained, with its log, as
`bench/receipts/stage-budget-suites-20261113T221717Z.json`: that is how the
pre-existing cancellation race in `tests/restore_budget.rs` was found. The test
now waits until the second worker has entered `admit` before cancelling. The
same test failed intermittently before this work started; the deterministic
version passes the repetition court above.

## Did any published number move?

`scripts/resource_courts.sh` was re-run at the fix revision; all five receipts
pass. The fixes change oversize admissions, cancelled waiters,
`release_workers()` and the order in which the restore pipeline calls `admit`.
The non-oversize rows report `oversized_admissions = 0`; the X06 **control**
arm does not call `StageBudget::admit` at all, and its restore does not use the
ordered pipeline. The comparison is generated by
`scripts/stage_budget_delta.py` into
`bench/receipts/stage-budget-delta-20261113T225001Z.json`:

<!-- BEGIN GENERATED:stage-budget-fix-results (scripts/perf_numbers.py) -->
<!-- END GENERATED:stage-budget-fix-results -->

Reading:

* The control arm moved by far more than any budgeted row, and it cannot
  execute the changed code (no `admit`, no pipeline budget). That bounds the
  run-to-run noise of this single-host court and refutes a causal reading of
  any individual delta.
* The budgeted rows (`cap 4` best, `cap 4` default, `cap 8` default) are within
  that noise band. The pipeline's new turn adds one mutex round-trip per
  tensor before admission; it is not resolved by these medians at this fixture
  size.
* The N09 overlap ratio is a wall-clock comparison of two stages on four shared
  CPUs. It is noise-dominated and disagreeing across runs on its sign at cap 4;
  it was published as a mixed result before and is published as a mixed result
  now, not as a claim in either direction.
* No representation, byte count, admission count, high-water mark or exactness
  result changed. `oversize.bytes` and `oversized_admissions` are identical
  before and after; the oversize arm now genuinely runs alone.

Therefore the generated numbers in `bench/RESULTS-resource-courts.md` were
regenerated from the new receipts (rule 12), and the hand-written percentage in
that document's generated region was replaced by a computed ratio.

## Residual and not claimed

* One host, warm page cache, five reps per cell; the deltas above are not a
  latency claim in either direction.
* No Go equivalent: `WeftFS-Go` has no stage-admission primitive (its quota
  "admission" is a per-commit byte budget, unrelated), so there was nothing to
  port.
* The primitive intentionally has no admission timeout; a caller that needs one
  passes a `StageCancelToken`.
* FUSE ownership and same-inode direct-I/O tests remain environment-blocked as
  before.
