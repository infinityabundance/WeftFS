# R8 — reconstruction single-flight and dependency-aware decoded-object cache

**Date:** 2026-11-06
**Revision:** WeftFS `bc93654` (flake fix) + the R8 feature commit above it.
**Court:** `weft/bench-court:1` (`sha256:4041d66e…`), kernel `7.3.1-1-cachyos`,
3 GiB / 4 vCPU / 1024 pids, unchanged hard limits.
**Receipt:** `bench/receipts/cache-20261106T091322Z.json` (26/26 checks),
raw log `cache-20261106T091322Z.log`, recorded at WeftFS `dbeaf0a`.

## What was built

* **Single-flight** (`src/singleflight.rs`). A bounded `Mutex` + `Condvar`
  registry keyed by a validated reconstruction identity
  `(object id, content/representation version, region, verified)`. One leader
  runs the read+decode; identical concurrent callers wait and receive a clone of
  the leader's result or its error. A waiter may cancel (`Class::Cancelled`)
  without cancelling the leader. Bounds (`max_entries`, `max_bytes`) degrade a
  request to an independent read instead of queueing. The registry is reached
  only through an authorized read token.
* **Decoded-object cache** (`src/object_cache.rs`). Bounded bytes + entries,
  keyed by `(object id, content version)`, storing the **verified decoded
  bytes**. Admission/eviction score is
  `decode_cost_ns(class, bytes) * dependent_count * observed_read_frequency`
  compared per cached byte with exact integer arithmetic; ties break by lower
  score, then larger bytes, then lower content/object id. An object can be
  pinned so it is never evicted. Authorization is checked before every lookup:
  the token comes from `Store::authorize_read`, so a denial is `Unauthorized`
  and reveals nothing about cache contents.
* **Wiring.** `Store::read_object_authorized`, `read_range_authorized`,
  `read_content_authorized` (plus `*_cancellable`) enter the layers; dependency
  reconstructions inside a delta propagate the same token, so a shared base is
  coalesced and cached too. `Options` gains `single_flight`,
  `single_flight_max_entries` (64), `single_flight_max_bytes` (256 MiB),
  `cache_bytes` (0 = disabled), `cache_entries` (65 536). CLI: global
  `--single-flight`, `--single-flight-max-entries`, `--single-flight-max-bytes`,
  `--cache-bytes`, `--cache-entries` on `cat`, `mount`, and the harness.
  Internal reads (verify, planning, GC, recovery) never touch the cache and
  keep the shipped path.

## Workload

The court re-hashes both real Qwen2.5-0.5B / Instruct files against
`bench/corpora/real-models/qwen2.5-0.5b-pair.json`, then writes 26 objects with
the shipped planner:

| group | objects | representation |
|---|---|---|
| `shared_base` | 1 | `float-tensor` (mode 9), 8,716,288 B |
| `shared_dependent` | 6 | `sparse-delta` (mode 16) on that one base |
| `base` | 7 | `float-tensor` |
| `delta` | 8 | real Instruct `weight-delta` (mode 11) on their base |
| `independent` | 4 | `float-tensor`, no shared dependency |

226,623,488 logical bytes per round, 4 rounds, 104 reads per thread-set at one
thread and 416 at four. Cache bound 256 MiB. Every returned byte is hashed and
compared with the plan.

## Comparison (4 configurations × 1 and 4 threads)

| config | thr | reads | decode calls | backend bytes read | p50 | p99 | wall | peak RSS | cache hits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| none | 1 | 104 | 160 | 754,432,204 | 61.17 ms | 93.85 ms | 6,440.7 ms | 92.9 MiB | 0 |
| none | 4 | 416 | 640 | 3,017,728,816 | 63.25 ms | 98.20 ms | 6,645.9 ms | 321.5 MiB | 0 |
| single-flight | 1 | 104 | 160 | 754,432,204 | 61.64 ms | 94.33 ms | 6,413.3 ms | 91.6 MiB | 0 |
| single-flight | 4 | 416 | 160 | 754,570,204 | 60.68 ms | 93.94 ms | 6,384.4 ms | 108.9 MiB | 0 |
| cache | 1 | 104 | 26 | 107,411,220 | 0.224 ms | 53.87 ms | 1,121.9 ms | 326.3 MiB | 92 |
| cache | 4 | 416 | 104 | 429,644,880 | 0.236 ms | 57.30 ms | 1,193.2 ms | 470.7 MiB | 368 |
| both | 1 | 104 | 26 | 107,411,220 | 0.240 ms | 53.66 ms | 1,124.7 ms | 329.9 MiB | 92 |
| both | 4 | 416 | 26 | 107,549,220 | 0.240 ms | 54.62 ms | 1,140.6 ms | 381.9 MiB | 326 |

All eight runs: 100 % hash matches, `verify --full` ok, zero mismatches.

Derived effects:

* Single-flight at 4 threads: **640 → 160** reconstructions (−75 %) and
  **3,017.7 → 754.6 MB** read (−75 %). Wall time moved from 6,645.9 to
  6,384.4 ms (−3.9 %, within run-to-run noise): the duplicate work was being
  done in parallel on otherwise-idle vCPUs. The durable effect is work and
  backend I/O, not latency on a 4-vCPU court.
* Cache at 1 thread: **160 → 26** reconstructions (6.2×), **754.4 → 107.4 MB**
  read (7.0×), wall **6.44 → 1.12 s** (5.7×), p50 **61.17 ms → 0.224 ms**
  (273×).
* Both at 4 threads: **640 → 26** reconstructions, **3.02 GB → 0.11 GB** read,
  wall **6.65 → 1.14 s** (5.8×), and 26 versus 104 decode calls for cache-only
  (the first-round races collapse to one decode).
* p99 never falls below ~54 ms: the first read of each large tensor is still a
  full decode, so the latency distribution stays bimodal.

## Adversarial (5/5, in the same receipt)

| case | result |
|---|---|
| waiter cancellation | waiter `Cancelled`, leader returned the exact bytes, `decode_calls == 1` |
| decode error fan-out | 4/4 waiters `Corrupt`, `decode_calls == 1`, 0 cache entries |
| cache disabled | `enabled=false`, 0 entries, 0 hits, 2 decodes for 2 reads |
| pinned base retained | pinned entry hit while two higher-value entries were admitted |
| unpinned base evicted | the lowest-score resident was re-decoded after a higher-value admission |

## Losses and honest limits

1. **Cache-only still races.** At 4 threads `cache` performs 104 decodes versus
   26 at 1 thread: every worker misses the same object before the first
   admission. Only `both` removes that (26).
2. **Single-flight is not a wall-time win on 4 vCPUs.** Duplicate work was
   already spread across idle cores. It reduces work and backend bytes, which
   matters for contention, slow backends, and large dependency chains.
3. **Single-flight at 1 thread has no benefit and a mutex/condvar cost**; the
   decode counts are identical and the run is within noise of `none`
   (6.41 vs 6.44 s).
4. **The cache costs memory.** +233 MiB RSS at 1 thread (326.3 vs 92.9 MiB) and
   +149 MiB at 4 threads (470.7 vs 321.5 MiB) for a 226 MB working set.
5. **The bound must exceed the working set.** With a bound below
   `working_set`, eviction re-decodes later rounds; the receipt's
   `cache_evictions` field exposes it. This run used 256 MiB for a 226 MB
   working set and saw 0 evictions.
6. **Bimodal latency.** p99 is unchanged by the cache (cold first decode).
7. **Per-process and per store handle.** A new process or mount starts cold;
   nothing is persisted, and this is not a cross-host or cross-mount cache.
8. **Not covered by this court:** `mode 15` `TENSOR_TILED` whole-object reads
   bypass the layers (they return per-request tile statistics); FUSE range reads
   get single-flight only (the cache is whole-object); no Go parity.
9. **Scope of the numbers.** One host, warm in-process store, tmpfs-backed
   `.docker-cache`, synthetic dependents intermixed with real weights. Not a
   fleet, cold-cache, or power-loss claim.
