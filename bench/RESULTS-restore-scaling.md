# R13 — restore scaling: the backend mutex, and what removing it bought

Subphase R13 asked one question: R10 measured an ordered checkpoint restore at
**1.21× on 8 requested workers against a 9.92× Amdahl cap**, and named a single
backend mutex and per-tensor sequential shards as the cause. This note proves
that diagnosis first, then changes the synchronisation boundary, then measures
the result on Qwen2.5-0.5B, Qwen2.5-3B, and a MinIO S3 store.

Everything here was measured inside `scripts/docker-court.sh bench` under the
standard hard limits (`--memory=3g --memory-swap=3g --pids-limit=1024
--cpus=4`, bounded `/tmp`, `no-new-privileges`) on one host with the page cache
warm. No limit was raised. "MB/s" is **logical restored bytes / wall time**;
the receipts record the exact definition and the sink mode.

Revisions and receipts:

| what | revision | receipt |
| --- | --- | --- |
| probe + baseline | `525f884` (code), `18fc254` (evidence) | `restore-scaling-20261111T063612Z.json` (0.5B), `restore-scaling-3b-baseline-20261111T071500Z.json` (3B) |
| immutable reads off the state mutex | `80c7c97` | `restore-scaling-20261111T075701Z.json` |
| within-tensor shard concurrency | `32b3462` | `restore-scaling-20261111T081932Z.json` |
| final (single-tensor floor uses the shard setting) | `9e821dd` | `restore-scaling-20261111T090904Z.json` (0.5B), `restore-scaling-3b-final-20261111T092000Z.json` (3B) |
| independent readers (5 repetitions) | `9e821dd` | `restore-scaling-readers-20261111T091500Z.json` |
| MinIO S3 | `65be878` | `restore-scaling-minio-20261111T085500Z.json` |

The first MinIO attempt (`restore-scaling-minio-20261111T083500Z.log`, an
uppercase, non-DNS bucket name) is retained by name and not cited for a
number. An earlier 3B baseline run that mixed cold-cache state with the worker
count was discarded rather than cited.

Fixtures: `Qwen2.5-0.5B` / `-Instruct` (BF16, 290 tensors, 988,097,824 B,
SHA-256 `88c14255…0ed342` / `fdf756fa…88fb7fe`) and `Qwen2.5-3B` /
`-Instruct` (BF16, 6,171,927,112 B, `52da3d99…17ab` / `54917aba…d91d2`), each
written as a fast-raw, 8-way sharded `weft.checkpoint.v1` immediately before
the restore matrix. Every container was re-hashed in the court.

## 1. The bottleneck, measured

`Store` keeps one `Mutex<Box<dyn StorageBackend + Send>>`. Every durable read
entered it, and the domain-separated BLAKE3 check ran *inside* the closure, so
each chunk's read **and** hash were serialised. An opt-in probe (off by
default) now records per-entry mutex wait and hold, demand concurrency, and
per-tensor attribution; `tests/lock_probe.rs` proves one thread cannot hold
the lock longer than its own wall time.

At `525f884`, 0.5B, whole-file SHA (counting sink, no per-tensor SHA):

| requested workers (effective) | MB/s | mutex hold / wall | wait / wall | peak demand |
| --- | --- | --- | --- | --- |
| 1 (1) | 806 | 0.21 | 0.00 | 1 |
| 2 (2) | 1464 | 0.41 | 0.14 | 2 |
| 4 (4) | 1411 | 0.41 | 0.50 | 4 |
| 8 (4) | 1403 | 0.41 | 0.50 | 4 |
| 16 (4) | 1408 | 0.41 | 0.51 | 4 |

3B, same method: 818 / 1709 / 1976 / 1975 MB/s at 1/2/4/8, hold 0.23 / 0.53 /
0.65 / 0.65, wait 0.00 / 0.18 / 0.75 / 0.74.

The hold is one thread at a time; four threads wait 0.75 of the wall in
aggregate. Per tensor at four workers, the backend lock took **42 %** of the
0.5B embedding's wall time and **53–60 %** of a 64 MB MLP tensor's; for 3B it
was **37 %** and **53–55 %**. Hold p50/p95 was 15/20 µs (0.5B) and 19/28 µs
(3B); wait p95 48/60 µs. That is the binding serial resource: the same bytes
must be read and hashed exactly once, and before R13 they could only be read
and hashed by one thread at a time.

An `io` section was added in increment 2 so the receipt separates state-mutex
wait, state-mutex hold, blob transport, and decode CPU per tensor.

## 2. The change

**Increment 2 (`80c7c97`).** `src/read_pool.rs` adds a bounded
`ReadHandlePool` (at most 64 idle handles) for `objects/` and `manifests/`
reads. A read borrows a handle, calls the backend, releases the handle, and
only then verifies the BLAKE3 hash. The state mutex now protects only the
superblock, commit records, indexes, quota/access counters, and the optional
caches.

Safety rules, all tested:

* Every backend mutation bumps the pool epoch and drops idle handles, so a
  handle whose packed-plane catalog predates a publish is never reused. In-flight
  readers keep reading immutable bytes.
* A released handle's backend counters are diffed into a harvested
  `BackendStats`; `Store::backend_stats` and `Store::plane_stats` merge it, so
  receipts still see every byte and request.
* 3FS is deliberately **not** pooled (each handle would open its own USRBIO
  ring set); that transport keeps the old path. Nothing silently substitutes.
* `tests/read_pool.rs`: immutable reads take **zero** state-mutex holds while
  four readers overlap; pooled bytes are counted; a mutation invalidates
  handles; four readers racing ten rewrites and ten publishes always see exact
  bytes; a flipped manifest or chunk fails closed; pooled handles never mix
  bytes.

**Increment 3 (`32b3462`).** A tensor's shards are read and verified with the
existing `parallel::run_bounded` runner: `shard_threads` (default 1 = the old
sequential walk) and `shard_max_inflight_bytes` (default 64 MiB). Per-shard
length, content-root, and logical-root checks run in the worker; assembly and
the whole-tensor root check stay in input order. The first failure is still the
lowest failing input index. `tests/checkpoint_restore_pipeline.rs` compares
bytes and the container SHA-256 against the sequential restore at
1/2/4/8/16 workers × 1/2 tile threads × 1/4 shard threads (20 configurations).

The Amdahl floor moves from **the largest tensor** to **the largest shard**:
both Qwen embeddings exceed the 256 MiB in-flight bound and therefore ran alone
before R13 (272 MB of 988 MB for 0.5B, 622 MB of 6.17 GB for 3B). Their 8
shards are 34 MB and 74 MB, so shard concurrency can fill the gap. When the
whole-file SHA is enabled it remains a serial 0.39 s (0.5B) / 2.4 s (3B) floor;
that is the store's own exactness contract, not the harness.

## 3. Scaling

### 3.1 Same method before/after (counting sink, whole-file SHA)

This is the only row that can be compared directly across `525f884`, because
the baseline has no unhashed restore. It includes the serial whole-file SHA.

0.5B:

| workers | before MB/s | after MB/s | change |
| --- | ---: | ---: | ---: |
| 1 | 796 | 807 | +1.4 % |
| 2 | 1481 | 1560 | +5.3 % |
| 4 | 1393 | 1549 | +11.2 % |
| 8 | 1414 | 1541 | +9.0 % |
| 16 | 1426 | 1550 | +8.7 % |

3B:

| workers | before MB/s | after MB/s | change |
| --- | ---: | ---: | ---: |
| 1 | 804 | 792 | **−1.5 %** |
| 2 | 1725 | 1896 | +9.9 % |
| 4 | 2008 | 2039 | +1.5 % |
| 8 | 1993 | 2050 | +2.9 % |

The 3B four-worker row barely moves because the whole-file SHA (2.4 s) becomes
the floor at 3.0 s; the store itself did improve (§3.2). The 3B one-worker row
is a small **loss**, within run-to-run noise on this host but reported as a
loss rather than hidden.

### 3.2 Store-only restore (no output hashing; exactness proven by one untimed SHA pass)

| fixture | workers | shard_threads=1 | p50 ms | shard_threads=4 | p50 ms |
| --- | --- | ---: | ---: | ---: | ---: |
| 0.5B | 1 | 1188 MB/s | 833 | **2354 MB/s** | 420 |
| 0.5B | 2 | 2233 | 443 | **3801** | 292 |
| 0.5B | 4 | 2776 | 356 | 2903 | 343 |
| 0.5B | 8 (4 eff.) | 2780 | 358 | 3394 | 383 |
| 0.5B | 16 (4 eff.) | 2792 | 355 | 2953 | 392 |
| 3B | 1 | 1177 | 5167 | **2390** | 2650 |
| 3B | 2 | 2090 | 2976 | **3480** | 1846 |
| 3B | 4 | 2998 | 2026 | 2439 | 2678 |
| 3B | 8 (4 eff.) | 3073 | 2028 | 2438 | 2641 |

* Single large tensor (the embedding, one tensor, one worker): 0.5B **552 →
  454 ms** (−17.8 %) with shard_threads=4; 3B **1,303 → 1,072 ms** (−17.8 %).
* State-mutex hold share after the change: **0.000** at every worker count on
  every run (only 12 prologue/`backend_stats` entries, 1.7 µs total, 0.5B).
* Read pool at four workers: 4 handles created, 607,838 reuses, 607,842 chunk
  reads, `max_in_flight` 4 (0.5B); 4 / 2,149,556 / 2,149,560 / 4 (3B).
  Shard-threads=4 reaches `max_in_flight` 13–15.
* Peak RSS (VmHWM, KiB): 0.5B 892,544 (shard 1) → 1,073,264 (shard 4, **+20.2
  %**); 3B 1,619,236 → 1,852,948 (**+14.4 %**).
* Whole-file SHA at 3B: 792 / 1896 / 2039 / 2050 (shard 1). Store-only reaches
  3.0–3.1 GB/s at four workers; with the SHA it is 2.0 GB/s.

**Oversubscription is real.** `shard_threads=4` with four tensor workers runs
up to 16 shard workers on 4 vCPUs: 0.5B 4×4 is 2,903 MB/s (worse than 2×4 at
3,801) and 3B 4×4 is 2,439 MB/s (worse than 2×4 at 3,480), while backend I/O
time rises from 4.5 s to 13.4 s (3B). There is **no automatic worker budget**;
the best measured configuration on this court is **2 tensor workers × 4 shard
threads**, and that is a court result, not a default.

### 3.3 Concurrent independent readers (distinct checkpoints)

Four readers on two checkpoints, first restore per reader hashed and checked,
the rest counted. 0.5B, 5 repetitions, `restore-scaling-readers-20261111T091500Z`:
**1,860 / 2,509 / 2,145 / 1,992 / 2,658 MB/s** aggregate (mean 2,233 MB/s)
versus **1,188 MB/s** for one store-only restore. State-mutex hold 0; the pool
serves 4 concurrent reads (`max_in_flight` 4, 1,428,620 reads). The 3-run
in-court readers phase in the full receipt is noisier (1,480–2,443 MB/s) and is
retained as-is; the 5-repetition receipt is the cited number.

### 3.4 MinIO S3 (loopback, official binary `RELEASE.2025-04-22T22-12-26Z`)

0.5B store-only: **179 / 345 / 491 / 495 MB/s** at 1/2/4/8 (2.74× at four).
Whole-file SHA: 143 / 326 / 347 / 390. Four readers: 219 and 379 MB/s
aggregate over two repetitions. State-mutex hold 0; the request latency, not
the mutex, is the serial resource (I/O 9.4–11.1 s against 17–18 s of summed
wall at one worker). MinIO is a single loopback process on a tmpfs-backed data
directory; this is not a cloud or fleet claim.

## 4. Losses and limits (all of them)

* **The whole-file SHA remains a serial floor.** With it enabled, the 3B
  one-worker row is −1.5 % and the four-worker row only +1.5 %; the 0.5B gains
  are +9–11 %. The unhashed row is a measurement mode, not the production
  restore.
* **Oversubscription.** Outer workers × shard threads can multiply past the
  CPUs: 4×4 is slower than 2×4 on both fixtures, and 3B backend I/O time
  triples. No auto-budget was added.
* **RSS growth.** Shard concurrency reserves more decoded bytes at once:
  +20.2 % (0.5B) and +14.4 % (3B) peak RSS. A 3 GiB court is close to the 3B
  shard-4 peak (1.85 GiB) before any page cache.
* **A 3FS transport is not pooled**, so it keeps the old state-mutex read path
  and gets no benefit from increment 2.
* **Handles are invalidated by every mutation.** A mixed read/write workload
  recreates handles after each publish; the pool is a read-workload
  optimisation and this cost was not measured.
* **Small tensors.** Per-tensor shard dispatch has a fixed cost; the shard
  matrix helps large tensors and the full restore, not a 1-row region read.
* **Partial (region) restores still walk their shards sequentially.** Only the
  full-tensor path was parallelised; tile threads remain the region mechanism.
* **MinIO absolute numbers are provider- and host-bound** (5.6× slower than
  the local store at one worker), and the S3 pool handle construct is not
  free.
* **One host, warm cache, one run** per cell except the 5-repetition readers
  and 3-repetition matrices. No media, power-loss, fleet, or cloud claim.
* The 3B `ckpt-probe` store was written once before the commit series and
  reused; it is a fast-raw 8-way checkpoint of the verified corpus, not a
  per-run re-ingest.

## 5. What is now true

* The single backend mutex is **proven** to be the binding serial resource for
  the pre-R13 read path: 0.41–0.65 hold share and 0.50–0.75 aggregate wait
  share at four workers, with 37–60 % of each tensor's wall time inside it.
* Immutable reads no longer take the state mutex: the hold share is 0.000 in
  every measured configuration, and four readers overlap on pooled handles.
* The full-tensor restore is no longer bounded by the largest tensor: a
  tensor's shards are read and verified concurrently with the shipped bounded
  runner, byte-identically at every worker/tile/shard combination tested.
* The remaining serial resources are explicit: the optional whole-file SHA,
  the single ordered writer, and (for oversubscribed configurations) the CPU
  count. The store-only 0.5B restore reaches **2.79 GB/s** at four workers and
  the 3B **3.07 GB/s**; the shard-concurrent single-tensor floor is **454 ms**
  (0.5B) and **1,072 ms** (3B).

## 6. Reproduce

```sh
scripts/docker-court.sh bench "cd /work && \
  WEFT_RESTORE_FAMILIES='qwen0.5b' WEFT_RESTORE_THREADS='1 2 4 8 16' \
  WEFT_RESTORE_REPS=3 WEFT_RESTORE_MINIO=0 \
  bash bench/scripts/restore_scaling_court.sh"
```

The 3B rows use the same `family_lifecycle restore` commands against the
fast-raw 8-way 3B checkpoint store, with `--shard-threads 1` and `--shard-threads
4`; the exact invocations are in the `.log` beside each 3B receipt. The MinIO
row is `bench/scripts/restore_scaling_minio_inner.sh` (it starts the pinned
MinIO binary itself). Gates for the code: `cargo test --all-targets` (770
tests, 0 failed) and `cargo test --features fuse --all-targets` (813, 0
failed), both in the courts; `scripts/check-claims.sh`; `scripts/spec-drift.sh`;
and the conformance manifest/coverage checks.
