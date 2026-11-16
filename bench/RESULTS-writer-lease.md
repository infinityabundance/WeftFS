# X09 — writer session lease: expiry, fencing and fairness

Receipt: `bench/receipts/writer-lease-20261115T235811Z.json` (stamp
`20261115T235811Z`, WeftFS revision `4ab9ed2`). Raw log beside it. Normative
record and refusal rules: [`spec/WEFT-WRITER-LEASE.md`](../spec/WEFT-WRITER-LEASE.md).

## 1. What was built

A writer session is a **durable, renewing grant with an absolute deadline**
(`weft.writer.lease.v1` at `writer/lease`), not a request-triggered check:

* **Expiry is a wall-clock instant in the record.** A contender may take over
  iff the record is `released` or `now >= deadline_ns`. An idle live session
  therefore expires on its own; nothing has to ask it to.
* **Fencing is monotone.** Every takeover writes `previous + 1`. A resumed,
  stale writer's next publication is refused `LeaseExpired` *inside* the
  publication critical section, before the commit record and before the head
  compare-and-swap, so it writes nothing.
* **Renewal is explicit and conditional.** The keeper thread renews an idle
  session at `ttl / 3`; the ordinary write path renews when the remaining
  lifetime is at most half the TTL. Both are compare-and-swaps on the bytes this
  handle last wrote.
* **Release is a tombstone**, never an unconditional delete: a successor's
  record is never removed by a straggler.
* **Fairness is a FIFO ticket queue** (`weft.writer.ticket.v1`, allocated
  `highest + 1` and created once). A newcomer's number is strictly greater than
  every ticket present when it was allocated, so a stream of short-lived writers
  cannot overtake a waiter.

It is opt-in: `--writer-lease-secs` together with the optimistic writer mode
(MW2). MW1 is refused `InvalidArgument` at open, before any byte is written, and
a store with no lease policy behaves exactly as before.

## 2. Method

Harness `examples/writer_lease_court.rs`, court `scripts/writer_lease_court.sh`,
rust court (3 GiB / 3 GiB swap / 1,024 pids / 4 CPUs). Every case uses its own
store on the local backend in optimistic writer mode.

| case | what it does |
|---|---|
| `acquire_cost` | 120 open+acquire cycles, fairness on and off |
| `publication_cost` | 60 commits per arm, with and without a lease |
| `idle_expiry` | a holder stops; a second process waits for the deadline |
| `live_renewing` | a holder renews; a contender tries to take over |
| `kill_child` | a real `SIGKILL` of the holder process |
| `stop_child` | `SIGSTOP`, successor takes over, `SIGCONT`, holder tries to publish |
| `fairness_on` | one queued waiter plus 150 short-lived newcomers |
| `fairness_off` | the same workload with `--no-lease-fairness` (control) |

Fault cases re-execute the same binary as a **real second process**; the TTL in
those cases is 2,000 ms so a deadline can be observed inside a court run. Wall
clock throughout — a lease deadline is a real duration, so the fixed-timestamp
fixture override is never used for it.

## 3. Results

<!-- BEGIN GENERATED:writer-lease-table (scripts/perf_numbers.py) -->
| measurement | result |
|---|---|
| open + acquire, fairness on (p50 / p95) | 11,626 µs / 16,018 µs |
| open + acquire, fairness off (p50) | 9,270 µs |
| publication, no lease (p50 / p99) | 11.49 ms / 15.32 ms |
| publication, leased (p50 / p99) | 11.50 ms / 14.92 ms |
| idle holder: successor wait | 2,019.9 ms |
| idle holder: fence | 1 → 2 |
| live renewing holder: renewals / refusals / takeovers | 19 / 17 / 0 |
| SIGKILL: successor wait (TTL 2,000 ms) | 2,019.4 ms |
| SIGSTOP → SIGCONT: holder's next publish | LeaseExpired, exit 4 |
| fairness on: newcomers that overtook / attempted | 0 / 150 |
| fairness on: waiter's fence / wait | 2 / 2,369.6 ms |
| fairness off (control): newcomers that overtook / attempted | 100 / 150 |
| fairness off (control): waiter's fence / wait | 102 / 3,595.8 ms |
<!-- END GENERATED:writer-lease-table -->

<!-- BEGIN GENERATED:writer-lease-prose (scripts/perf_numbers.py) -->
The court ran 13 of 13 checks and all passed. The lease adds 0.009 ms at p50 to a publication (leased minus unleased, i.e. within noise on this host). A killed holder is replaced after one full lifetime: the successor waited 2,019.4 ms of a 2,000 ms TTL, taking fence 1 → 2. With fairness on, 0 of 150 newcomers overtook the queued waiter, which then took fence 2 after 2,369.6 ms. With fairness off, 100 of 150 overtook it: the waiter's fence was still 102 after 3,595.8 ms.
<!-- END GENERATED:writer-lease-prose -->

The lease is essentially free on the publication path: the difference between
the two commit arms is 0.009 ms at p50, and the leased arm's p99 is *lower* than
the unleased arm's on this run (both are single-run medians of 60 writes on a
warm page cache, so this is noise, not a win). The real cost is acquisition,
where fairness adds the queue list/create/delete.

## 4. Losses

1. **Fairness costs acquisition latency.** With fairness on, open+acquire p50 is
   11,626 µs versus 9,270 µs with it off — the same store, the same work, plus
   one queue listing, one create-once ticket and one delete. That is the price
   of the guarantee, and it is charged once per open, not per write.
2. **An idle or killed holder keeps the store until its deadline.** A `SIGKILL`
   leaves a `held` record, so the successor waits the full TTL (2,019.4 ms of a
   2,000 ms lifetime here). Release is prompt only when the holder is alive
   enough to write a tombstone.
3. **A suspended holder is refused when it resumes.** That is the contract, but
   it means a process paused longer than its TTL must re-open; its in-flight
   work is lost. The stop/resume case shows the refusal (`LeaseExpired`, exit 4)
   and an unchanged head.
4. **The wait budget bounds, it does not eliminate, waiting.** A live holder that
   keeps renewing is never stolen — correct, but a second writer that wants the
   store cannot get it until the holder releases or dies.
5. **A ticket expires.** A waiter that waits past its ticket lifetime (10 s
   default, twice the default 5 s wait budget) stops blocking others and
   forfeits its position. The queue bound is 4,096 live tickets.
6. **Lease mode is MW2-only.** MW1 keeps its kernel lock and gets no lease; the
   fence is not available to MW1 writers.
7. **One host, warm page cache, one run per cell.** The fairness control arm is
   reported, not asserted: 100 of 150 newcomers overtook the waiter without it,
   which is the defect the queue removes, not a stable number.
8. **No clock-skew defence.** Deadlines are wall-clock instants compared by every
   participant; a badly skewed clock can expire a live session early. The refusal
   is still typed and still prevents a second writer, but liveness is not
   guaranteed across a clock jump.

## 5. Non-claims

* Not a multi-writer protocol: at most one session holds the lease, and MW2's
  transaction loop is unchanged.
* The advisory `head_generation`/`head_commit` fields are not an attestation of
  durability and are never a root.
* No fleet, cloud, media or power-loss claim; the fault cases are real process
  faults on one host, not a physical power cut.
* No claim that 150 newcomers is a worst case: it is a workload chosen to fit
  the court's four CPUs and five-second wait budget.
