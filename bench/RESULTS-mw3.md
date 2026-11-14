# MW3 conflict domains, the T021 rebase and the T027 writer envelope

Receipts (all generated in the bench court at WeftFS
`d2a8b7e1993c62ea10167c8a9fb6fe7a599cb541`, stamp `20261114T025943Z`, with
`--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`):

* `bench/receipts/mw3-20261114T025943Z.json` — MW3 disjoint and same-domain
  writers, process per writer;
* `bench/receipts/mw2-retry-20261114T025943Z.json` — T021 before/after on MW2
  at N=32, three interleaved repetitions per arm;
* `bench/receipts/writer-scale-20261114T025943Z.json` — T027 compact client at
  32…1024 writers plus one process-per-writer subset;
* the raw log `bench/receipts/mw3-20261114T025943Z.log` (sha256 in each
  receipt).

The normative contract is `spec/WEFT-MULTIWRITER.md` §15. The method is
`bench/CONTRACTS-multiwriter.md` §8. Everything below is one host, one local
POSIX namespace, warm page cache, one repetition unless the row says
otherwise. No fleet, cloud, media, power-loss, Go-parity or cross-domain
atomicity claim is made.

## 1. MW3 disjoint writers, one domain per writer

Each writer registers its own conflict domain, opens a sharded handle bound to
it, and commits disjoint file transactions. `efficiency = tx/s(N) / (N ×
tx/s(1))`. `gap commits read` counts commits the handle read after losing a
race; in a disjoint world it is zero because no other writer advances the
domain's head.

<!-- BEGIN GENERATED:mw3-disjoint-table (scripts/perf_numbers.py) -->
| N | committed | tx/s | efficiency | p99 ms | reads/commit | gap commits read |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 500 | 10,417 | 1.000 | 0.127 | 3.04 | 0 |
| 2 | 1,000 | 20,408 | 0.980 | 0.137 | 3.04 | 0 |
| 4 | 2,000 | 35,714 | 0.857 | 0.154 | 3.04 | 0 |
| 8 | 4,000 | 33,058 | 0.397 | 0.282 | 3.04 | 0 |
| 16 | 3,200 | 57,143 | 0.343 | 0.847 | 3.09 | 0 |
| 32 | 3,200 | 26,230 | 0.079 | 2.765 | 3.18 | 0 |
<!-- END GENERATED:mw3-disjoint-table -->

Aggregate throughput rises from N=1 through N=16 and then falls at N=32. The
fall is not a protocol effect: 32 processes share four vCPUs and each pays its
own open and process start inside the measured wall. The plateau begins where
the machine is CPU-bound, not where the protocol is. Read amplification stays
flat and small at every N because no rebase ever happens.

The same run verified every domain separately with `verify --full` (all rows
`verify_full: true`), and the global verifier **refused** every store with
`UnsupportedVersion` (`global_reader_refused: true` in every row): an MW1/MW2
reader does not report an empty namespace for a sharded store.

## 2. MW3 same-domain contention

Every writer targets one name in **one** domain with create semantics. The
result must be exactly MW2's: one winner, every other request a typed
`Conflict(NameExists)`, nothing partially visible.

<!-- BEGIN GENERATED:mw3-conflict-table (scripts/perf_numbers.py) -->
| N | requested | committed | typed conflicts | conflict rate | p99 ms |
|---:|---:|---:|---:|---:|---:|
| 2 | 20 | 1 | 19 | 0.9500 | 0.084 |
| 8 | 80 | 1 | 79 | 0.9875 | 0.183 |
| 32 | 320 | 1 | 319 | 0.9969 | 0.119 |
<!-- END GENERATED:mw3-conflict-table -->

The conflict path is sub-millisecond at every N: rejecting a lost create is
cheap, and a hot name cannot be made concurrent by either MW2 or MW3. A
workload with this shape must shard its names; MW3 gives it a first-class unit
to shard on (`DomainId`), but it does not make the shared name concurrent.

Two writers in **different** domains never conflict at all: §1's disjoint rows
issued the same file names in different domains and every transaction
committed with zero conflicts.

## 3. T021: the MW2 rebase at N=32

Both arms use the identical fixture (32 processes, seven disjoint transactions
each, one local store, `checkpoint_interval = 0` as the frozen R4 fixture
specifies). The arms alternate and the comparison is the median of three
repetitions; the per-repetition rows are in the receipt.

* **before** is a release build of the tree at `8c84344`, the revision this
  subphase started from, built from a `git archive` of that commit with the
  same `WeftCodec` sources;
* **after** is `d2a8b7e1`. The only behavioural change in the rebase path is
  bounded exponential backoff with deterministic jitter (75–125 %, seeded from
  the transaction digest), plus a bounded cache of parsed intervening commits
  and the `RebaseStats` counters. Precondition validation already consulted
  only the transaction's touched keys; that is now explicit in
  `mw2::touched_keys` and measured by `touched_intersections`.

<!-- BEGIN GENERATED:mw3-retry-table (scripts/perf_numbers.py) -->
| arm | reads/commit | p99 ms | tx/s | retry amp | cache hits | gap commits read |
|---|---:|---:|---:|---:|---:|---:|
| before | 61.15 | 187.1 | 872 | 2.17 | n/a | n/a |
| after | 30.71 | 102.6 | 1,427 | 2.01 | 0 | 3,428 |
| change | **-49.8 %** | **-45.2 %** | | | | |
<!-- END GENERATED:mw3-retry-table -->

Reading the result honestly:

* Read amplification and p99 both fell materially, and throughput rose.
* **The cache is not the mechanism.** `gap_commits_cached` is zero in every
  repetition: catch-up was already incremental — a retry reads only the
  commits published since the handle last refreshed, never the same commit
  twice. The cache is retained because a schedule can retry against an
  unchanged head (for example an ambiguous failure that did not move the head),
  but this court never exercised that case and the receipt says so.
* The win is therefore attributable to **jittered, fair backoff**: writers that
  collided once no longer retry in lockstep, so a retry wave no longer reads a
  whole wave of winners' commits and no longer queues behind the same
  publication lock. Retry amplification fell as well.
* The residual read cost is structural, not a retry artefact: at this fixture
  every writer opens with `checkpoint_interval = 0`, so open replays the chain,
  and every attempt must read each newly published commit to rebase on it. A
  single head cannot avoid that; MW3 removes it by giving each writer its own
  head. §1 is the evidence: the same shape of workload reports zero gap commits
  and a flat, small reads-per-commit.
* This is **not** a change to the protocol. No commit byte, conflict class,
  precondition, conditional head write or frozen format changed. The abstract
  protocol is therefore unchanged and the bounded TLA model's invariants
  (`NoLostAck`, `NoBlindOverwrite`, `NoPartial`, `GcNeverReclaimsReachable`,
  …) still apply; retrying later with jitter is a liveness/scheduling change,
  not a safety change. The model was **not** re-run for T021 because the
  abstract transitions are identical, and the receipt does not claim a re-run.

## 4. T027: bounded control-plane fairness and per-client memory

The compact client runs N writer **threads in one process**, each with its own
store handle and conflict domain. At 512 and 1024 the workers are **held after
spawning**, so the peak thread count is real and the court's pid bound is
measured rather than recycled. A separate 32-writer cell uses one process per
writer to prove the process path is not required.

<!-- BEGIN GENERATED:mw3-scale-table (scripts/perf_numbers.py) -->
| writers | actor | spawned | spawn failures | tx/s | p99 ms | peak RSS KiB | RSS/writer B | max state rows |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 8 | threads | 8 | 0 | 59,728 | 0.11 | 6,476 | 828,928 | 101 |
| 16 | threads | 16 | 0 | 66,656 | 0.57 | 7,940 | 508,160 | 101 |
| 32 | threads | 32 | 0 | 62,211 | 1.58 | 10,984 | 351,488 | 101 |
| 64 | threads | 64 | 0 | 26,869 | 58.98 | 17,240 | 275,840 | 101 |
| 128 | threads | 128 | 0 | 28,206 | 13.87 | 23,148 | 185,184 | 41 |
| 256 | threads | 256 | 0 | 27,463 | 63.25 | 38,968 | 155,872 | 41 |
| 512 | threads + hold | 512 | 0 | 21,195 | 80.53 | 58,776 | 117,552 | 21 |
| 1024 | threads + hold | 1,017 | 7 | 18,275 | 122.74 | 111,428 | 112,195 | 21 |
| 32 | one process per writer | 32 | 0 | 32,000 | 1.34 | n/a | n/a | n/a |
<!-- END GENERATED:mw3-scale-table -->

<!-- BEGIN GENERATED:mw3-envelope (scripts/perf_numbers.py) -->
At 1,017 spawned writers in one process the court's pid bound refuses 7 further threads, and the observed peak RSS is **111,428 KiB**, i.e. **112,195 B/writer**.
The declared envelope is **512 KiB of thread stack (virtual) and one thread per writer**; the observed resident high-water mark per writer is 112,195 B at 1024 writers and 351,488 B at 32, both inside it.
<!-- END GENERATED:mw3-envelope -->

What binds, in order:

1. **CPU at four vCPUs.** Aggregate throughput is highest in the 16–32 writer
   range and declines from 64 upward. Adding writers past that point buys
   latency, not throughput: p99 grows with the run queue.
2. **The court's pid limit at 1024.** With every worker held alive, the court
   refuses further threads at 1024 names; the receipt records the exact
   refused count. This is the pid bound, not a protocol limit: the protocol has
   no per-writer registration, no leader and no shared queue to saturate.
3. **Scheduling latency, not starvation.** `max_consecutive_retries` is zero in
   every cell — a disjoint domain never loses a race, so no writer starves. The
   tail is thread scheduling and `fsync` contention on one namespace.

The head-state cache is the in-memory `State` per handle. It is bounded by the
store's own replay and, in this fixture, by the domain's committed rows; the
`max state rows` column is the largest per-writer row count observed, and it
stays small because a domain contains only its writer's objects.

## 5. Every loss and limitation

1. **MW3 is Rust-only, local-plane-only.** Go implements no MW3 surface;
   `object`, `s3`, `gcs`, `azure`, `3fs` and split planes refuse
   `--writer-mode sharded` with `UnsupportedVersion`. No parity claim.
2. **No cross-domain atomicity.** `execute_transaction_across` refuses two or
   more domains with `Class::CrossDomain`. There is no two-phase commit, no
   cross-domain snapshot isolation and no global linearizable view; the read
   view is a sequential capture (`capture_kind = "sequential"`).
3. **No coordinated GC.** A sharded store refuses GC. A domain's unreachable
   blobs cannot be swept without a cross-domain pass, and that pass does not
   exist. This is a real operational limitation, not a design claim.
4. **Domain commits carry no index attestation.** The prototype forces
   `attest_indexes = false` for a domain handle, so its commits are
   `weftfs.commit.v1`. The handle records the reason in its diagnostics. This
   is weaker than MW2's attested index and is stated wherever MW3 is cited.
5. **`weft.domain.link.v1` is specified, not implemented.** A split or
   superseded domain is never merged automatically; a successor is a new
   domain, and the link record has no writer yet.
6. **Transaction ids are store-global.** `weft.tx.v1` has no domain field, so
   the same id used in two domains is one logical id and a differing op set is
   `Conflict(TransactionIdReused)`. Callers must namespace ids per domain.
7. **One head per domain, and it is still one head.** Two writers in one domain
   scale exactly as MW2 does — badly. MW3 moves the contention boundary; it
   does not remove it.
8. **Process-per-writer at N=32 pays process cost.** The compact client is
   faster for the same logical work; the process cell exists only to prove the
   process path works, not to win a throughput comparison.
9. **Reads per commit in §1 include open-time replay.** Each writer's handle
   replays its domain's chain (the genesis plus its own commits) on open. That
   cost is inside the number and is not separated.
10. **Single host, single repetition, warm cache.** The §1 curve is
    non-monotonic at N=8 and N=32 (process start under four vCPUs); no
    confidence interval is claimed. The §3 comparison is the median of three
    interleaved repetitions, and its per-repetition spread is in the receipt.
11. **The T021 cache measured zero hits.** It is retained for a schedule this
    court does not produce, and it is not credited with the win.
12. **The bounded TLA model was not re-run for T021.** No abstract transition
    changed. The existing model still covers the protocol; a jittered retry
    delay is outside its abstraction.
13. **No physical power loss, host or device cache loss, fleet, cloud, GPU or
    RAPL measurement.**

## 6. What survives

* Per-domain heads do not contend: disjoint writers commit with zero conflicts,
  zero retries, and flat small read amplification.
* Same-domain contention behaves exactly as MW2: one winner, typed conflicts,
  no partial state.
* A sharded store is refused by an MW1/MW2 reader rather than misread as empty.
* Cross-domain atomic requests and GC are refused with typed errors, not
  approximated.
* MW1 remains the default; MW2 and its frozen fixtures are unchanged.
* T021 reduces MW2's rebase read amplification and p99 at N=32 without changing
  any protocol rule.
