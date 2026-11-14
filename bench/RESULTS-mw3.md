# MW3 conflict domains, the T021 rebase and the T027 writer envelope

Normative contract: `spec/WEFT-MULTIWRITER.md` §15. Method:
`bench/CONTRACTS-multiwriter.md` §8. Everything here is **one host, one local
POSIX namespace, warm page cache, four vCPUs**, inside the bench court with
`--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4` (never raised). No
fleet, cloud, media, power-loss, Go-parity or cross-domain-atomicity claim is
made. MW3 is **opt-in** (`--writer-mode sharded`), additive, Rust-only and
local-plane only; MW1 remains the default in every shipped path.

## 0. Evidence

Seven complete court runs at one revision, plus one merged receipt per table:

* `bench/receipts/mw3-20261114T0654*Z.json` — the seven raw runs
  (`20261114T065404Z`, `…065415Z`, `…065426Z`, `…065450Z`, `…065502Z`,
  `…065513Z`, `…065524Z`), each with its `…log`;
* `bench/receipts/mw3-merged-20261114T070000Z.json` — per-cell **median across
  the seven runs**, with `spread` recording the lowest and highest run per
  numeric field and `merged_from` naming every source receipt, its stamp and its
  sha256;
* `bench/receipts/writer-scale-*.json` and
  `bench/receipts/writer-scale-merged-20261114T070000Z.json` — the same for the
  T027 cells;
* `bench/receipts/mw2-retry-20261114T0654*Z.json` — each run's own T021
  before/after pair (three interleaved repetitions per arm);
* `bench/receipts/mw2-retry-runs-20261114T070000Z.json` — every run's pair kept
  unchanged, plus the median across runs.

The merge is `scripts/mw3_merge_runs.py`: it refuses receipts from different
revisions, different schemas or different row sets, and recomputes every ratio
from the merged values. It is a presentation step, not a measurement.

The T021 **before** arm is a release build of `8c84344` — the revision this
subphase started from — built in the same court from a `git worktree` of that
commit with its own `WeftCodec` pin. The receipt records the baseline revision
and the sha256 of the baseline binary (`a40cd3f9…`). Both arms run the same
fixture, in the same session, interleaved.

## 1. T020 — disjoint writers, one conflict domain per writer

Every cell here is **process-per-writer**: each writer registers its own domain
and opens a sharded handle bound to it. N=64 uses 64 processes, which fits the
court's pid bound. Each writer commits the same number of transactions, so the
cells differ in writer count, not in per-writer work.

`efficiency = tx/s(N) / (N × tx/s(1))`. `reads/commit` counts backend blob and
commit reads per committed transaction, including the open-time replay of that
writer's own domain chain.

<!-- BEGIN GENERATED:mw3-disjoint-table (scripts/perf_numbers.py) -->
| N | committed | tx/s (median) | lowest–highest run | efficiency vs N=1 | p50 ms | p95 ms | p99 ms | retry amp | reads/commit | gap commits read | backend requests | RSS KiB |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 400 | 11,111 | 10,526–11,429 | 1.000 | 0.062 | 0.076 | 0.111 | 1.00 | 3.04 | 0 | 3,621 | 6,428 |
| 2 | 800 | 21,622 | 19,512–21,622 | 0.973 | 0.064 | 0.082 | 0.112 | 1.00 | 3.04 | 0 | 7,242 | 13,020 |
| 4 | 1,600 | 40,000 | 38,095–41,026 | 0.900 | 0.066 | 0.089 | 0.145 | 1.00 | 3.04 | 0 | 14,484 | 25,940 |
| 8 | 3,200 | 56,140 | 43,836–57,143 | 0.632 | 0.087 | 0.129 | 0.214 | 1.00 | 3.04 | 0 | 28,968 | 51,788 |
| 16 | 6,400 | 26,122 | 20,847–27,004 | 0.147 | 0.143 | 0.420 | 1.092 | 1.00 | 3.04 | 0 | 57,936 | 103,332 |
| 32 | 12,800 | 21,227 | 20,350–21,918 | 0.060 | 0.161 | 1.460 | 48.768 | 1.00 | 3.04 | 0 | 115,872 | 206,380 |
| 64 | 25,600 | 19,617 | 18,977–20,142 | 0.028 | 0.176 | 3.609 | 67.785 | 1.00 | 3.04 | 0 | 231,744 | 414,812 |
<!-- END GENERATED:mw3-disjoint-table -->

Reading it honestly:

* **Aggregate throughput genuinely rises with N, and then stops.** The peak is
  at N=8, which is twice the vCPU count; all seven runs peak in that same cell.
  Beyond it the curve falls and then **plateaus** at roughly the N=1 rate:
  adding writers past that point buys queueing, not throughput.
  `p50` grows gently and `p99` grows by orders of magnitude, which is what a
  saturated four-vCPU run queue looks like.
* **The plateau is the machine, not the protocol.** There is no shared head, no
  shared queue, no global discovery write and no leader in this path: every
  writer's publication is a conditional write to *its own* domain head. The
  per-cell `retry amp` stays at one and `intervening commits read` stays at
  zero in every disjoint cell of every run — no writer ever lost a race, at any
  N, which is exactly the property the domain split is for.
* **Read amplification is flat and small** at every N, because a writer only
  ever replays its own domain.
* **Process cost is inside the number.** `wall` spans process start to the last
  writer's exit. The court also records each writer's own `elapsed_ms`; the
  steady-state window (widest writer span) differs from end-to-end by a few
  milliseconds, so process start is not what produces the plateau.
* Every cell verified its domain with `verify --full` in every run, and the
  **global** verifier refused every store with `UnsupportedVersion`
  (`global_reader_refused: true`): an MW1/MW2 reader does not report an empty namespace for a sharded store.
* The negative control: the two statements above are not free. A single-domain
  workload cannot be scaled this way — §2 is the same machine doing the same
  work through one head.

### 1.1 Client model per N

| N | client | why |
|---:|---|---|
| 1, 2, 4, 8, 16, 32, 64 | one process per writer | the shipped driver (`examples/mw2_writer.rs`); 64 processes fit the 1024-pid court |
| 8 … 1024 | threads in one process | the T027 compact driver (`examples/mw3_compact.rs`), §4 |

The compact driver exists because the pid bound, not the protocol, stops a
process-per-writer run above a few hundred. §4 shows the same shape from the
thread client, so the plateau is not a process-model artefact.

## 2. T020 — same-domain contention

Every writer targets one name in **one** domain with create semantics. The
result must be exactly MW2's: one winner, every other request a typed
`Conflict(NameExists)`, nothing partially visible.

<!-- BEGIN GENERATED:mw3-conflict-table (scripts/perf_numbers.py) -->
| N | requested | committed | typed conflicts | conflict rate | p95 ms | p99 ms |
|---:|---:|---:|---:|---:|---:|---:|
| 2 | 20 | 1 | 19 | 0.9500 | 0.057 | 0.081 |
| 8 | 80 | 1 | 79 | 0.9875 | 0.063 | 0.099 |
| 32 | 320 | 1 | 319 | 0.9969 | 0.222 | 0.465 |
<!-- END GENERATED:mw3-conflict-table -->

The conflict path is sub-millisecond at every N. `reads/commit` is deliberately
not tabulated here: it divides one winner's cost by one committed transaction
and says nothing about the losers.

Two writers in **different** domains never conflict at all: §1 issued the same
file names in different domains and every transaction committed with zero conflicts.

**This is the honest shape of the result.** MW3 gives a workload a first-class
unit to shard on; it does not make a shared name concurrent. A hot name must be
sharded by its owner.

## 3. T021 — the MW2 rebase at N=32

Both arms use the identical fixture: 32 process writers, seven disjoint
transactions each, one local store, `checkpoint_interval = 0`. The arms
alternate inside each court run, and each arm contributes three repetitions.

T021 is **not a protocol change**. The pre-change tree already rebased
incrementally (a retry reads only the commits published since the handle last
refreshed). T021 added (a) bounded exponential backoff with **deterministic
jitter** seeded from the transaction digest, (b) a bounded cache of parsed
intervening commits, (c) an explicit touched-key set and (d) `RebaseStats`
counters. No commit byte, conflict class, precondition, conditional head write
or frozen format changed.

<!-- BEGIN GENERATED:mw3-retry-summary (scripts/perf_numbers.py) -->
| arm | reads/commit | p99 ms | tx/s | retry amp | cache hits | gap commits read |
|---|---:|---:|---:|---:|---:|---:|
| before (median of 7 runs) | 68.90 | 206.3 | 862 | 2.21 | n/a | n/a |
| after (median of 7 runs) | 32.22 | 104.7 | 1,697 | 2.03 | 0 | 3,344 |
| published R4 N=32 (different revision) | 31.39 | 302.2 | 580 | n/a | n/a | n/a |
<!-- END GENERATED:mw3-retry-summary -->

<!-- BEGIN GENERATED:mw3-retry-method (scripts/perf_numbers.py) -->
The baseline is a release build of revision `8c843448009fbc1b4744715ad2b0cbaafe3724c5`; each arm contributes 3 repetitions inside each of the 7 runs, and every run's own pair is in the table above.
<!-- END GENERATED:mw3-retry-method -->

<!-- BEGIN GENERATED:mw3-r4-reference (scripts/perf_numbers.py) -->
The published R4 N=32 row is 224 committed transactions from 7,032 backend reads at revision `288e2a8d9c49e78074793d69ada1c80b2aafc38d`, measured by the R4 harness generation (stamp 20261109T175227Z). It is cited as labelled history: a different revision, harness and session, not this subphase's baseline.
<!-- END GENERATED:mw3-r4-reference -->

<!-- BEGIN GENERATED:mw3-retry-runs (scripts/perf_numbers.py) -->
| run | before reads/commit | after reads/commit | change | before p99 ms | after p99 ms | before tx/s | after tx/s |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 32.97 | 55.00 | 66.8 % | 231.0 | 105.5 | 780 | 1,697 |
| 2 | 82.40 | 32.22 | -60.9 % | 187.4 | 104.7 | 1,028 | 1,965 |
| 3 | 33.38 | 31.27 | -6.3 % | 206.2 | 104.6 | 865 | 1,684 |
| 4 | 68.90 | 31.05 | -54.9 % | 212.2 | 120.8 | 855 | 1,427 |
| 5 | 79.62 | 55.34 | -30.5 % | 189.5 | 105.6 | 911 | 1,697 |
| 6 | 32.23 | 31.84 | -1.2 % | 206.3 | 99.8 | 862 | 1,697 |
| 7 | 84.92 | 56.25 | -33.8 % | 236.9 | 99.3 | 765 | 2,018 |
<!-- END GENERATED:mw3-retry-runs -->

What the numbers actually say:

* **p99 is the stable result.** It fell in every run of every repetition set:
  the after arm is both lower and tighter. The mechanism is jittered fair
  backoff — writers that collided once do not retry in lockstep, so a retry wave
  no longer queues behind one publication point.
* **Reads per commit is bimodal, and one run regressed.** The retry schedule is
  bursty: a run either produces a storm of catch-up reads or it does not. The
  median across runs falls clearly, but the per-run table shows one run where
  the after arm read *more*, and two runs where the change is small. **The read
  count is therefore not a stable claim**; the p99 and throughput results are.
* **The cache is not the mechanism.** `gap commits cached` is zero in every run
  of every arm. Catch-up was already incremental, so a retry never re-reads a
  commit it already validated; the cache is retained for a schedule this court
  does not produce and is credited with nothing.
* **Against the published R4 figure the read count is parity, not a fall.** The
  R4 court measured the same logical fixture at revision `288e2a8` with a
  different harness generation; its N=32 row is in the summary table, labelled.
  The current tree's *before* arm is not the R4 code either. The gate is
  therefore met on p99 and **not met** on read amplification versus the
  published number; it is met against the same-session baseline. No gate was
  widened to hide this.
* **The residual read cost is structural.** At this fixture every writer opens
  with `checkpoint_interval = 0`, so open replays the chain, and every attempt
  must read each newly published commit to rebase on it. A single head cannot
  avoid that. MW3 removes it by giving each writer its own head — §1 reports
  zero intervening commits read at every N. T021 makes the retry cheap; MW3
  stops it happening.
* The bounded TLA model was **not** re-run: no abstract transition changed, and
  a jittered retry delay is outside its abstraction. The receipt does not claim
  a re-run.

## 4. T027 — bounded writer count, fairness, and per-client memory

The compact client runs N writer **threads in one process**, each with its own
store handle and conflict domain. At 512 and 1024 the workers are held after
spawning, so the peak thread count is real and the court's pid bound is measured
rather than recycled. `commits/writer` differs by cell because the court keeps
total work bounded; it is reported so a reader never compares unlike cells.

<!-- BEGIN GENERATED:mw3-scale-table (scripts/perf_numbers.py) -->
| writers | spawned | refused | committed | commits/writer | tx/s | p50 ms | p95 ms | p99 ms | peak RSS KiB | B/writer | max state rows | max consecutive retries | writers that retried |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 8 | 8 | 0 | 400 | 50 | 56,132 | 0.080 | 0.11 | 0.33 | 6,496 | 831,488 | 101 | 0 | 0 |
| 16 | 16 | 0 | 800 | 50 | 64,730 | 0.123 | 0.28 | 0.68 | 8,004 | 512,256 | 101 | 0 | 0 |
| 32 | 32 | 0 | 1,600 | 50 | 60,225 | 0.135 | 1.11 | 1.75 | 11,012 | 352,384 | 101 | 0 | 0 |
| 64 | 64 | 0 | 3,200 | 50 | 31,596 | 0.143 | 2.63 | 5.11 | 17,084 | 273,344 | 101 | 0 | 0 |
| 128 | 128 | 0 | 2,560 | 20 | 42,151 | 0.140 | 5.40 | 14.62 | 22,912 | 183,296 | 41 | 0 | 0 |
| 256 | 256 | 0 | 5,120 | 20 | 22,890 | 0.149 | 15.61 | 69.36 | 38,480 | 153,920 | 41 | 0 | 0 |
| 512 | 512 | 0 | 5,120 | 10 | 21,289 | 0.167 | 75.33 | 85.95 | 59,136 | 118,272 | 21 | 0 | 0 |
| 1024 | 1,018 | 6 | 10,180 | 10 | 17,833 | 0.202 | 107.15 | 165.28 | 110,784 | 111,437 | 21 | 0 | 0 |
| 32 (process per writer) | 32 | 0 | 160 | 5 | 32,000 | n/a | n/a | 0.70 | 187,768 | n/a | n/a | 0 | 0 |
<!-- END GENERATED:mw3-scale-table -->

<!-- BEGIN GENERATED:mw3-envelope (scripts/perf_numbers.py) -->
At 1,018 spawned writers in one process the court's pid bound refuses 6 further threads, and the observed peak RSS is **110,784 KiB**, i.e. **111,437 B/writer**.
No writer was starved: the largest number of consecutive retries any writer took is 0 at 1024 writers and 0 at 32, and 0 writers ever retried.
The declared envelope is **one thread per writer with a 512 KiB thread stack, set explicitly by the court client, plus one open store handle**. The stack bound is virtual, so it does not bound resident memory; what is observed is **111,437 B/writer** at 1024 writers and 118,272 B/writer at 512, both at 10 commits per writer, and 831,488 B/writer at 8 writers at 50 commits per writer. Resident cost grows with the per-writer committed state, which is why the small-N cells are larger per writer.
<!-- END GENERATED:mw3-envelope -->

Where the bound binds, in order:

1. **The pid bound at 1024 names.** With every worker held alive the court
   refuses further threads; the refused count is in the table and the cell's
   `ok` flag is false in the receipt because its request could not be met. This
   is the cgroup limit, not a protocol limit: the protocol has no per-writer
   registration, no leader and no shared queue to saturate.
2. **CPU at four vCPUs.** Aggregate throughput peaks around the 16–32 writer
   cells and falls from there. Per-writer throughput collapses by two orders of
   magnitude between the smallest and largest cells — that is the run queue, and
   it is why p99 grows the same way.
3. **Not starvation.** The longest run of consecutive retries any single writer
   took is zero in every cell, and no writer retried at all: a disjoint domain
   never loses a race. The tail is scheduling and page-cache contention on one
   namespace, not a writer being starved by protocol fairness.
4. **The head-state cache is per handle and tiny here.** `max state rows` is the
   largest object+entry+relation row count in any one writer's replayed state.
   Since a domain holds only its writer's objects, it stays small; it varies
   between cells only because `commits/writer` varies.

**The declared per-writer envelope.** One thread per writer (the client sets a
512 KiB stack explicitly) plus one open store handle. The handle's decoded-object
and intervening-commit caches are *bounded per handle* (a ceiling, never a
preallocation), and in a disjoint run they hold nothing: the court reports zero
intervening commits read and zero cache hits. The observed resident high-water
mark per writer is in the envelope table and stays close to it as N grows.

**The envelope does not compose at its ceiling, and that is a real limit.** The
per-handle caches are bounded per handle, so N handles have a theoretical
ceiling of N × the per-handle bound — far more than the court's memory. What is
actually observed is bounded because a disjoint writer stores nothing in those
caches. A shared multi-writer daemon with one cache would make the bound compose;
this prototype does not have one.

## 5. Every loss and limitation

1. **MW3 is Rust-only and local-plane-only.** Go implements no MW3 surface;
   `object`, `s3`, `gcs`, `azure`, `3fs` and split planes refuse
   `--writer-mode sharded` with `UnsupportedVersion`. No parity claim.
2. **No cross-domain atomicity.** `execute_transaction_across` refuses two or
   more domains with `Class::CrossDomain`. There is no two-phase commit, no
   cross-domain snapshot isolation and no global linearizable view; a read view
   is a sequential capture (`capture_kind = "sequential"`).
3. **No coordinated GC.** A sharded store refuses GC. Unreachable blobs cannot
   be swept per domain without a cross-domain pass, and that pass does not exist.
4. **Domain commits carry no index attestation.** A domain handle forces
   `attest_indexes = false`, so its commits are `weftfs.commit.v1` and weaker
   than MW2's attested index. The handle records the reason in its diagnostics.
5. **Lineage, not merge.** `weft.domain.link.v1` is implemented as create-once
   lineage; the protocol never copies or selects data. A merge is an explicit
   republish into a successor plus the link record.
6. **Transaction ids are store-global.** `weft.tx.v1` has no domain field, so the
   same id in two domains is one logical id and a differing op set is
   `Conflict(TransactionIdReused)`. Callers must namespace ids per domain.
7. **One head per domain, and it is still one head.** Two writers in one domain
   scale exactly as MW2 does — badly. MW3 moves the contention boundary.
8. **Scaling stops at the CPU count.** The T020 peak is at N=8, twice the vCPU
   count, and §4 never beats its own N=16 cell. No claim is made above four
   vCPUs, and no claim is made that MW3 is faster for a single writer.
9. **The disjoint curve is measured with 400 commits per writer.** It is a
   throughput curve, not an endurance test; the store is `tmpfs`, where `fsync`
   is free, so nothing here measures durable-media cost.
10. **Reads/commit at N=32 is bimodal and one run regressed.** See §3. The
    headline is p99, not the read count, and both are in the tables.
11. **The T021 cache measured zero hits.** It is retained for a schedule this
    court does not produce and is not credited with the win.
12. **The bounded TLA model was not re-run for T021 or MW3.** The abstract MW2
    transitions are unchanged; MW3's per-domain publication reuses them
    verbatim. No model of the domain split itself exists.
13. **The compact client at 1024 writers could not be measured as requested:**
    the court refused the last threads and the cell is `ok = false`. That is the
    pid bound, reported rather than hidden.
14. **No physical power loss, host or device cache loss, fleet, cloud, GPU or
    RAPL measurement.** A sharded store has never been crash-tested; the MW2
    crash courts cover the shared publication path only.
15. **No performance claim against another filesystem.** This court measures the
    context-plane augmentation, per the programme rule.

## 6. Gate verdicts

| gate | verdict |
|---|---|
| T019 normative spec: domain identity and canonical encoding | **met** — `weft.domain.v1` + derived `DomainId`, tested |
| T019 per-domain heads and generations | **met** — two-slot MW1 head under a per-domain key |
| T019 discovery record mapping domain → head | **met** — derived `head_key` in the record, resolved live by `discover_heads`, `mw3 list` |
| T019 read view explicitly not globally linearizable | **met** — `capture_kind = "sequential"`, normative isolation sentence |
| T019 cross-domain snapshot semantics | **met** — view pin is an auditable record, not a guarantee |
| T019 atomicity within a domain, refusal across | **met** — one commit per domain; `CrossDomain` otherwise |
| T019 merge semantics for a split/superseded domain | **met** — no data merge; create-once lineage names one or several predecessors |
| T019 typed refusals | **met** — `CrossDomain`, `UnsupportedVersion`, `NotFound`, `Conflict` |
| T020 disjoint writers N=1…64 with positive scaling | **met up to the CPU count**, plateau published, not re-framed |
| T020 same-domain behaves exactly as MW2 | **met** — one winner, typed conflicts, winner's bytes only |
| T020 different domains never conflict | **met** — zero conflicts and zero retries in every disjoint cell |
| T020 MW1 default, MW3 opt-in, MW1 reader refuses | **met** — default `single`; every cell's global verify refused |
| T021 p99 falls materially | **met** — fell in every run |
| T021 read amplification falls materially vs the published R4 figure | **not met** — parity against R4; a fall only against the same-session baseline, and one run regressed |
| T027 fairness at 32…1024 writers | **met** — zero consecutive retries, zero stalled writers |
| T027 envelope and high-water marks inside it | **met** for the observed footprint; the per-handle cache ceiling does not compose (stated) |
| T027 court limits named as binding | **met** — pid bound at 1024, CPU above ~32 |

## 7. Reproduce

```bash
# build the T021 baseline once, into a gitignored worktree
git worktree add --detach .docker-cache/mw3-before-tree 8c843448009fbc1b4744715ad2b0cbaafe3724c5
git -C WeftCodec archive 7c8dac37867626a4c45273933c8c99927a58728e --prefix=WeftCodec/ \
  -o /tmp/weftcodec-7c8dac3.tar
tar -xf /tmp/weftcodec-7c8dac3.tar -C .docker-cache/mw3-before-tree
bash scripts/docker-court.sh bench "cd /work/.docker-cache/mw3-before-tree && \
  CARGO_TARGET_DIR=/work/.docker-cache/target-mw3-before \
  cargo build --offline --release --example mw2_writer && \
  mkdir -p /work/.docker-cache/mw3-before && \
  cp /work/.docker-cache/target-mw3-before/release/examples/mw2_writer \
     /work/.docker-cache/mw3-before/example-mw2-writer"

# seven runs, then the medians
for i in 1 2 3 4 5 6 7; do
  bash scripts/docker-court.sh bench "cd /work && \
    WEFT_MW3_BEFORE=/work/.docker-cache/mw3-before \
    WEFT_MW3_BEFORE_REV=8c843448009fbc1b4744715ad2b0cbaafe3724c5 \
    WEFT_MW3_PROFILE=full bash scripts/mw3_court.sh"
done
python3 scripts/mw3_merge_runs.py --schema weftfs.mw3.bench/1 --stamp <stamp> \
  -o bench/receipts/mw3-merged-<stamp>.json bench/receipts/mw3-*.json
python3 scripts/mw3_merge_runs.py --schema retry-runs --stamp <stamp> \
  -o bench/receipts/mw2-retry-runs-<stamp>.json bench/receipts/mw2-retry-*.json
```

The reduced aggregate court is `scripts/mw3_verify_court.sh` (N=1/4/32 disjoint,
N=8/32 same-domain, compact 8/32/256/1024, no T021 baseline). It prints one
`mw3: skip <reason>` line and exits 78 when the environment cannot run it.
