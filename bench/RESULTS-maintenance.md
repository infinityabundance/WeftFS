# T080 — bounded background maintenance

Receipt: `bench/receipts/maintenance-20261115T124918Z.json` (stamp
`20261115T124918Z`, WeftFS revision `63ad66c`). Raw log beside it. Normative
record definition: [`spec/WEFT-MAINTENANCE.md`](../spec/WEFT-MAINTENANCE.md).

## 1. What was built

A driver around a bounded sweep: it plans once, persists the plan and a cursor,
sweeps in byte-bounded batches under a declared rate and duty cycle, and writes
the state after every batch so a restart resumes. Safety is not in the plan:
`Store::maintenance_reclaim` acquires a GC epoch, removes orphan object records
as `gc --apply` does, re-derives reachability, and refuses to delete a candidate
that is reachable now. `weftfs ops` is the read-only ops surface (counters,
reachability, dry-run plan) and `weftfs ops --apply` runs the driver.

## 2. Method

Harness `examples/maintenance_court.rs`, court `scripts/maintenance_court.sh`,
Rust court. Fixture: 120 live 4 KiB artifacts and 600 unlinked 8 KiB files with
distinct content, 4 KiB chunks, no automatic checkpoint (a retained checkpoint
state is a GC root and would legitimately keep garbage).

1. **Interference.** 12,000 whole-object reads against a control store; then the
   same 12,000 reads on a second store while a maintenance pass runs at a
   declared 4 MiB/s and a 10% duty cycle on 4 CPUs.
2. **Kill and resume.** A child process runs batches; the parent polls the
   persisted state and `SIGKILL`s it after the first batch is durable; the store
   is reopened and resumed.
3. **Two compactors.** Two child processes in the optimistic writer mode sweep
   the same plan.

## 3. Bounded interference

<!-- BEGIN GENERATED:maintenance-interference (scripts/perf_numbers.py) -->
Two arms of **12,000** warm whole-object reads on 4 CPUs. Control p50/p95/p99: **12,003 / 12,964 / 14,457 ns**. With maintenance active at a declared **4,194,304 B/s** ceiling and **10%** duty cycle: **12,062 / 12,403 / 13,725 ns**, ratios **1.00 / 0.96 / 0.95**. The pass took **7,179 ms** and achieved **150,539 B/s** — below the declared ceiling because it is scan-bound and duty-limited.
<!-- END GENERATED:maintenance-interference -->

The measured cost is small. In this run two of the three ratios are at or
below 1.00 and the p50 ratio is above it, which is what noise looks like on one
host; the honest statement is that this court did not resolve a tail penalty
larger than run-to-run variation. It does not show that no such penalty exists:
a read can land while a batch is unlinking blobs, and a run on a busier host
would be needed to size that.

The declared rate is a **ceiling used to pace batches, not a throughput
promise**. The pass is scan-bound and duty-limited, so it achieved less than the
declared rate; the state records what it achieved instead of the target.

## 4. Restart and concurrent failure

<!-- BEGIN GENERATED:maintenance-failure (scripts/perf_numbers.py) -->
The killed process had persisted **1** pass(es) and cursor **64**; the resumed run recovered **818,662 B** from where it stopped. Two compactors reclaimed **1,080,806 B** against a plan of **1,080,806 B** — equal, so no candidate was counted twice.
<!-- END GENERATED:maintenance-failure -->

* The killed pass lost no reachable artifact: the live set is byte-identical
  before and after, and `verify --full` is clean.
* The restart **resumed**: the persisted cursor advanced from where the killed
  process stopped instead of starting over.
* Two compactors together reclaimed exactly the plan and no more; the epoch CAS
  serialised them and `delete_if_present` made each lost race a no-op.

## 5. A defect the court found

The `SIGKILL` arm found a real bug in the driver, not in the store: a state
record written with a *shorter* payload through a range write left the previous
record's tail in the file, producing a record that did not decode
(`NonCanonical: trailing bytes`). The writer now truncates to the record length.
The failing run is retained in the log history of this subphase; the fix is in
`src/maintenance.rs`.

## 6. Losses

1. **Self-churn.** The plan and state are namespace files, so each new version
   leaves the previous version's manifest unreachable. A completed run leaves a
   bounded amount of its own garbage for the next run (the suite asserts it is
   small, and the second-run reclaim is reported by the harness).
2. **The plan can be stale.** Safety comes from re-verification, at the cost of
   one reachability scan per batch. A candidate that is reachable now is skipped
   and reported rather than deleted. On a large store that scan is the dominant
   cost of a batch, not the deletion.
3. **A candidate larger than the declared bound is refused.** `BudgetExceeded`
   names the blob, and a blob cannot be split.
4. **The declared rate is not achieved.** The pass is scan-bound and
   duty-limited; the declared rate paces batches but does not set throughput.
4. **The duty cycle is enforced by sleeping**, not by cgroup CPU shares, and
   does not compensate for other load on the machine. `last_duty_percent` is the
   duty the driver enforced, not an independent CPU sample.
5. **Data-only.** Advisory index segments and transaction records are left to
   `gc`; maintenance does not reclaim them.
6. **One writer mode at a time.** In the default single-writer mode a second
   compactor cannot even open the store (writer lock, `Busy`); concurrency
   requires the optimistic writer mode, which must be migrated explicitly.
7. **`gc_grace_secs` is an option, not persisted state.** A reopened handle with
   default options retains every young blob; the court reopens with the same
   options and says so.
8. **No Go parity.** WeftFS-Go has no maintenance driver or ops surface.
9. One host, one run per arm, page cache warm for the control arm.

## 7. Non-claims

* No claim that interference is zero; it is measured and published above.
* No throughput, fleet, cloud, media or power-loss claim.
* No claim that a resumed pass finishes in bounded time: each batch is bounded,
  the number of batches is not.
