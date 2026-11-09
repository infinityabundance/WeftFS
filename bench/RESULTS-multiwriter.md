# Multi-writer results (R4)

Receipts (all measured at WeftFS `b0bcbc5`, WeftFS-Go `f46b351`):

* benchmark `bench/receipts/bench-multiwriter-20261109T150423Z.json`
  (14 rows: local disjoint N=1..32, local contention N=1..32, mixed N=8,
  MinIO N=4) and its `.log`;
* mixed-language correctness court
  `bench/receipts/multiwriter-court-20261109T175012Z.json`;
* failure court `bench/receipts/multiwriter-failure-20261109T175014Z.json`;
* TLA+ court `bench/receipts/tla-20261109T174959Z.json` (the same five models
  and results as `tla-20261109T140116Z.json`, rerun by the final aggregate).

The earlier all-pass court receipts `multiwriter-court-20261109T150402Z.json`,
`multiwriter-failure-20261109T150414Z.json`, and `tla-20261109T140116Z.json`
are retained; the stamps above are the final-aggregate reruns at WeftFS
`a3256af`.

The earlier benchmark `bench-multiwriter-20261109T144401Z.json` (WeftFS
`f2cda44`) is **retained but superseded**: it measured the same protocol with
the pre-final driver. Its N=32 row was 542.4 tx/s and p99 354.4 ms against
638.2 tx/s and 287.3 ms here (17.7 % and 19 % spread). That spread is the
honest reproducibility of a single repetition; no confidence interval is
claimed.

The benchmark method is frozen in
[`CONTRACTS-multiwriter.md`](CONTRACTS-multiwriter.md). Every number below is
one run on 4 vCPUs in the bench court.

## 1. Local disjoint: scaling

N independent processes, one transaction each per operation, disjoint names.
`eff = throughput(N) / (N × throughput(1))`. `prep p50` is the content-manifest
publication (`PrepareContent`) alone; the latency columns are the whole
optimistic execution (catch-up, validation, quota, encode, commit record, head
CAS, transaction record).

| N | committed | tx/s | eff | p50 ms | p95 ms | p99 ms | max ms | prep p50 | retry amp | RSS sum MiB | CPU user ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 60/60 | 4,000.0 | 1.000 | 0.180 | 0.216 | 0.228 | 0.3 | 0.034 | 1.000 | 10.9 | 10 |
| 2 | 120/120 | 3,529.4 | 0.441 | 0.199 | 0.232 | 0.263 | 17.7 | 0.033 | 1.050 | 21.0 | 20 |
| 4 | 240/240 | 2,105.3 | 0.132 | 0.240 | 0.314 | 17.390 | 91.7 | 0.033 | 1.096 | 43.9 | 60 |
| 8 | 480/480 | 1,672.5 | 0.052 | 0.323 | 0.459 | 60.377 | 253.3 | 0.033 | 1.146 | 90.4 | 290 |
| 16 | 240/240 | 652.2 | 0.0102 | 0.256 | 19.325 | 303.030 | 360.3 | 0.034 | 1.700 | 178.2 | 370 |
| 32 | 224/224 | 638.2 | 0.0050 | 0.278 | 175.982 | 287.300 | 338.2 | 0.039 | 2.446 | 354.2 | 470 |

Every configuration committed every requested transaction and passed
`verify --full`. Gaps in generation numbers (for example N=16 head 244 for 241
expected commits) come from generations abandoned on a lost race; the store
never reuses a generation number. The head chain, not the arithmetic, is
verified.

Logical and physical write throughput (payload bytes only for logical;
backend `bytes_written` for physical):

| N | logical B/s | physical MB/s | backend reads | writes | reads/commit | writes/commit |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 19,333 | 22.5 | 192 | 424 | 3.2 | 7.1 |
| 2 | 17,059 | 19.9 | 457 | 847 | 3.8 | 7.1 |
| 4 | 10,175 | 11.8 | 1,179 | 1,694 | 4.9 | 7.1 |
| 8 | 8,084 | 9.4 | 3,374 | 3,385 | 7.0 | 7.1 |
| 16 | 3,071 | 3.7 | 9,471 | 1,733 | 39.5 | 7.2 |
| 32 | 2,991 | 3.6 | 11,356 | 1,665 | 50.7 | 7.4 |

The headline is a **loss**: scaling efficiency is 0.44 at N=2 and 0.0050 at
N=32. The publication point is one conditional head write; on the local
backend it is serialised by a short `LOCK.publish` OFD lock. At N≥4 the tail is
waiting for that critical section plus retrying, not CPU (user CPU is under
half a second in every run). Metadata cost grows with N: backend reads per
commit rise from 3.2 to 50.7, because every lost race re-reads the winner's
commit. Write counts are flat (about 7 per commit). Logical payload throughput
is tiny by construction — a 4–8 byte payload plus canonical records — so it
tracks transaction rate and is not a data-plane throughput claim.

## 2. Local high contention

Every writer targets `hot.bin`; create semantics mean exactly one can win.

| N | requested | committed | conflicts | conflict rate | wall ms | p50 ms | p99 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 10 | 1 | 9 | 0.90 | 2 | 0.040 | 0.226 |
| 2 | 20 | 1 | 19 | 0.95 | 2 | 0.044 | 0.506 |
| 4 | 40 | 1 | 39 | 0.975 | 2 | 0.042 | 0.506 |
| 8 | 80 | 1 | 79 | 0.9875 | 3 | 0.049 | 0.225 |
| 16 | 160 | 1 | 159 | 0.994 | 5 | 0.078 | 0.342 |
| 32 | 320 | 1 | 319 | 0.997 | 10 | 0.089 | 0.975 |

**This is the correct result for a contended name**, not a throughput result:
one winner, every other request a typed `NameExists` conflict. No
last-writer-wins behaviour was observed, and the conflict path is fast
(sub-millisecond p50). A workload with this shape should shard its names.

## 3. Mixed Rust/Go

N=8 (4 Rust + 4 Go), 30 transactions each, disjoint:

| committed | tx/s | p50 ms | p95 ms | p99 ms | retry amp | verify |
|---:|---:|---:|---:|---:|---:|---|
| 240/240 | 1,558.4 | 0.225 | 0.320 | 32.993 | 1.221 | all 16 cross-language verifiers pass; `verify --full` ok |

Deterministic parity (the same one-transaction store in both languages):
head, manifest, content root, object id, entry id, and version are identical;
`commits/2.commit` and the full 8 KiB `superblock` image are **byte-identical**;
each language reads the other's store with the same content hash. Receipt
`multiwriter-court-20261109T175012Z.json` (160/160 disjoint transactions,
1 winner out of 80 contended requests, 16 verifiers, history check
`generation >= genesis + committed`).

## 4. MinIO (S3 commit plane)

N=4 Rust processes, 10 disjoint transactions each.

| committed | tx/s | p50 ms | p95 ms | p99 ms | retry amp | backend reads | writes | verify |
|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 40/40 | 212.8 | 5.294 | 74.875 | 86.147 | 1.650 | 481 | 302 | `verify --full` ok |

The S3 path is **not** byte-identical to local and is much slower: every
publication is a commit-file `PUT If-None-Match`, a superblock `GET`, and one
`PUT If-Match`; every retry re-reads the winner's commit and the `writers`
policy. At N=4 it is 9.9× slower than local N=4 (212.8 vs 2,105.3 tx/s), or
18.8× slower than local N=1. The p99 is dominated by a lost conditional write,
not by encode. The row has no generation and no scaling efficiency because no
same-backend N=1 baseline was measured; neither is inferred.

## 5. Failure and adversarial histories

Receipt `multiwriter-failure-20261109T175014Z.json` (process-fault proxy, not
physical power loss). Each language was killed at four points; a fresh process
reopened the store, ran `verify --full`, looked the transaction up, retried it
when the answer was `unknown`, and read the name.

| stage | Rust | Go | initial outcome | final outcome |
|---|---|---|---|---|
| after prepared record | exit 134 | exit 137 | unknown | committed |
| before head write | 134 | 137 | unknown | committed |
| after head write | 134 | 137 | unknown | committed |
| after acknowledgement record | 134 | 137 | committed | committed |

In every case the reopened store was whole (`verify --full`), the name was
either absent or exactly right (never wrong content), and no partial state was
observed. A prepared record is deliberately `unknown` until a retry with the
same op set proves publication; the commit format does not carry the
transaction digest.

* **Idempotent replay**: a second `write` with the same transaction id
  committed nothing new; generation stayed 2 and the second attempt count was
  0 (`attempts` 1 then 0).
* **Delayed writer**: A paused 400 ms before execute while B committed the same
  name; A resumed with 0 commits and 1 typed conflict, B had 1 commit.
* **Quota race**: two transactions, budget 1 object; the winner commits, the
  loser is `BudgetExceeded` and publishes nothing (in-process tests, both
  languages). A new regression test proves that a lost publication race does
  **not** leak the losing attempt's ops into the retry state, which would
  under-count the winner's quota charge.
* **Authorization race**: a mode change that revokes read access between build
  and publish denies the other transaction with `Unauthorized`.
* **GC / prepared transaction**: `prepare → gc(true) → execute` keeps the
  prepared transaction's manifest (GC pins from `txs/<id>`); `verify --full`
  passes. True concurrent GC/publication is **not** claimed.

The finite history check is exactly that: every requested operation in the
benchmark is committed or a typed conflict, no process reported an unexpected
error, and the head is at or above genesis plus committed. It is **not** a
linearizability proof.

## 6. Model checking

`verification/tla/WeftMultiWriter.tla` (2 committers, 2 keys, MaxGen=2,
MaxAttempts=1): 2,690,909 states generated, 268,641 distinct, depth 20, all
invariants pass. Two deliberate flaws produce counterexamples:

* no observed-version check → `NoBlindOverwrite` violation (last-writer-wins);
* GC marks from the head only → `GcNeverReclaimsReachable` violation.

This is a bounded model of the abstract protocol, not a refinement proof and
not a linearizability proof.

## 7. Every loss and gap

1. **Scaling is poor.** Efficiency 0.44 (N=2) to 0.0050 (N=32). One head, one
   publication lock, retries. It is not lock-free and does not claim to be.
2. **Tail latency grows sharply.** p99 0.23 ms (N=1) → 287 ms (N=32); max
   360 ms. The critical section is short, but losers wait and re-read.
3. **Retry amplification.** 1.0 → 2.45. Every retry re-reads the winner's
   commit; at N=32 that is 50.7 backend reads per commit.
4. **RSS scales with processes.** About 11.1 MiB per idle writer handle at N=32
   (362,712 KiB total), before any data is cached. There is no shared daemon.
5. **Contention is unproductive by design.** 1 winner and N×10−1 conflicts;
   the conflict path is fast but a hot name cannot be made concurrent by this
   protocol.
6. **MinIO is ~10× slower than local at N=4** (212.8 vs 2,105.3 tx/s) and has
   an 86 ms p99. There is no batching, no lease, and no session cache. There is
   no S3 scaling claim.
7. **No validate/publish split.** The driver measures prepare and execute only.
   A phase-level breakdown would need store instrumentation that does not exist.
8. **Logical bytes exclude canonical records.** `logical_bytes` is the payload
   only; the object/entry/tip records and the manifest are counted as backend
   bytes, not logical bytes.
9. **CPU is approximate.** `/proc/self/stat` ticks at USER_HZ=100, summed per
   process; this is not energy and not RAPL.
10. **Go does not measure backend counters.** The Go summary omits
    reads/writes/bytes; mixed rows report only Rust-side totals where present.
11. **Single repetition, one host, 4 vCPUs.** No confidence intervals, no
    multi-node, no cold media, no fleet claim. The retained earlier run differs
    by up to 19 % at N=32.
12. **Only local, object, `3fs`, and S3 MW2 are implemented in Rust; Go is
    local/object only.** GCS/Azure are refused for optimistic mode.
13. **FUSE and generic checkpoint publication are still MW1.** No MW2 claim
    for file mutations or a checkpoint set.
14. **MW3 is not implemented.** No conflict domains, no multi-writer GC
    protocol, no distributed scalability.
15. **No physical power loss.** The failure court is a process-fault proxy; the
    earlier device-model courts remain the only lying-disk evidence and are not
    repeated here.

## 8. What survives

* Independent Rust and Go committers publish to one store with no lost
  acknowledged transaction in the executed courts.
* Deterministic parity is byte-identical at the commit file and superblock
  image.
* Conflicts are typed; there is no last-writer-wins path.
* A prepared transaction's blobs survive a GC pass.
* The single-writer path is unchanged and remains the default.

## 9. Retained failures

These receipts are kept because they found real defects. They were produced
before the final driver and are **not** used for any published number.

* `bench/receipts/bench-multiwriter-20261109T144320Z.json`: the S3 row failed
  with `Exists: key commits/12.commit already exists with different bytes`
  (twice) and `verify --full` failed. It found that a create-once commit-file
  collision on S3 must be classified `Busy` and retried against the new head,
  not surfaced as a fatal `Exists`.
* `bench/receipts/bench-multiwriter-20261109T144333Z.json`: S3 transactions
  committed but the harness invoked `verify --full` with the wrong backend
  flag. Fixed in `scripts/bench_multiwriter.sh`.
* `bench/receipts/bench-multiwriter-20261109T144140Z.json` and
  `…144235Z.json`: local runs whose `verify_full` field was absent, which led
  to the explicit per-row verify and the `verify_full` assertion.
* `bench/receipts/bench-multiwriter-20261109T144351Z.log` (no receipt): the
  harness aborted while parsing a writer summary; the receipt was never
  written, so none is cited.
