# R12 increment 2 — fleet cold-start

**One host with N processes, not a fleet claim.** The point is what the R8 read
layers (single-flight and the decoded-object cache) do when many *processes*
load the same model through one shared `StoreHandle`.

## Shape of the experiment

* **One real MinIO server** (`RELEASE.2025-04-22T22-12-26Z`, binary SHA-256
  `53e2a2cb…bb817c`) started inside the bench court on loopback, with its data
  directory on the court's `/work` bind mount.
* **One real S3-backed store** built over MinIO from
  `SmolLM-135M-Instruct.safetensors` (269,060,552 B, SHA-256
  `c1464c9ebccc6fb497aa4ccc8d74d29b33f9a9eddc4638a4575719357f532f2d`,
  272 BF16 tensors, largest 56,623,104 B). `scale_7b ingest --backend s3`.
* **One shared read-only server process** (`fleet_coldstart server`) opening
  that store once and serving **100 FUSE views** through `SharedMount` (R7). The
  store's `Arc<Store>` — and therefore its single-flight registry, decoded-object
  cache and one backend handle — is shared by every view.
* **100 replica processes** (`fleet_coldstart replica`), one per view, each
  reading the whole model through its own mount and hashing every byte.
  All 100 start from one published `go` file.

Two configurations, identical in every other respect:

| | single-flight | decoded-object cache |
|---|---|---|
| `with` | on (64 flights / 256 MiB reserved) | 128 MiB, 4,096 entries |
| `without` | off | 0 (disabled) |

Court: `weft/bench-court:1`, **8 GiB** memory (recorded, chosen because 100
replica processes plus 100 FUSE session threads plus MinIO share one cgroup),
`--memory-swap=8g --pids-limit=1024 --cpus=4 --tmpfs /tmp:size=1g
--cap-drop=ALL --no-new-privileges`, kernel 7.3.3-1-cachyos. No other limit was
changed.

Receipt: `bench/receipts/fleet-coldstart-20261110T225018Z.json` (+ `.log`).

## Result

| metric | `with` | `without` | ratio |
|---|---:|---:|---:|
| replicas completed | **100/100** | **100/100** | — |
| backend object requests | **61,908** | 112,368 | 1.81× |
| backend bytes read | **28,248,268,423** | 442,325,447,419 | **15.66×** |
| decode calls (gated counter) | 5,440 | 0 (see loss 4) | — |
| decode bytes (gated counter) | 3,120,960,064 | 0 | — |
| single-flight leaders / waiters | 5,440 / **37,327** | 0 / 0 | — |
| cache hits / misses | 13,133 / 4,967 | — | — |
| cache resident / served | 53,154,432 B / 3,361,908,096 B | — | — |
| time to first tensor p50 | **118.3 ms** | 134.7 ms | 1.14× |
| time to first tensor p95 | **141.9 ms** | 205.5 ms | 1.45× |
| time to first tensor max | **157.6 ms** | 208.3 ms | 1.32× |
| per-replica total p50 | **49,952.9 ms** | 651,409.2 ms | **13.04×** |
| wall until all 100 ready | **51,827.3 ms** | 672,765.2 ms | **12.98×** |
| server peak RSS | 1,032,148 KiB | 1,285,848 KiB | — |
| replica RSS, sum / max | 404,236 / 4,180 KiB | 382,052 / 4,052 KiB | — |
| cgroup memory peak | 1,757,917,184 B | 1,964,732,416 B | — |
| cgroup pids peak | 315 | 322 | — |
| OOM kills / pids refusals | **0 / 0** | **0 / 0** | — |

Exactness in both configurations: every replica read **272 tensors /
269,030,016 B** and all 100 per-replica byte digests are **identical**
(`replica_bytes_digest_agrees: true`).

Single-flight coalesced **87 %** of reconstruction requests (37,327 waiters out
of 42,767 total callers) with zero degraded admissions and zero panics. The
cache served 3.36 GB from 181 resident objects (53.2 MB of its 128 MiB budget)
with zero evictions.

## What this shows

The shared `StoreHandle` is what makes a 100-process cold start tractable on one
host. Without the read layers, the same 269 MB model costs **442 GB** of backend
traffic and **11.2 minutes**; with them it costs **28 GB** and **52 seconds**.
The mechanism is visible in the counters: 37,327 callers joined an existing
flight instead of decoding, and 13,133 lookups were served from the decoded
cache.

## Every loss

1. **Startup is serialised in one process.** The server opens the store
   (145 ms) and creates all 100 mounts (76 ms) before any replica can read, and
   every replica waits on one `go` signal. Time-to-first is 118-135 ms; the
   per-replica total is 50-651 s, so the first read is cheap and the *last*
   tensor is what dominates.
2. **One backend mutex.** All physical reads for all 100 views go through the
   store's single backend handle. This is why 100 independent readers cost 13×
   rather than the ~100× of fully independent daemons — but it is also a
   serialisation point that cannot be scaled by adding replicas.
3. **Cache memory.** The `with` server holds 1,008 MiB RSS versus 1,255 MiB for
   the control (the cache *raises* steady RSS by ~53 MB of cached objects while
   *lowering* total cgroup footprint from 1.83 to 1.64 GiB by avoiding
   re-materialised reads). A 128 MiB budget cannot hold the 269 MB model, so
   misses remain (4,967) and the cache is not a substitute for more memory.
4. **Decode count is only instrumented on the gated path.** `decode_calls` is
   incremented inside the single-flight closure (`src/store.rs:3001`); the
   control uses the ungated read path, so it reports **0**. The court therefore
   claims a decode count only for `with` (5,440) and uses backend bytes and wall
   time for the control. No decode number is invented for `without`.
5. **Tail latency is worse without the layers**: p95 time-to-first 205 ms vs
   142 ms, and the per-replica total spread (p50 651.4 s, max 671.7 s) shows
   every replica pays the full cost.
6. **Coalescing depends on timing.** The `with` cell was measured once here.
   An earlier aborted attempt at the same revision produced a materially
   different `with` wall for the same configuration (the replicas overlapped
   differently); its artifacts were not retained, so its numbers are not
   quoted. The *direction* and the ~13× gap are robust; the absolute `with`
   wall is not a tight number.
7. **One host, loopback MinIO, warm host caches.** No multi-host, network
   partition, cloud, or production-workload claim. The `without` run also
   demonstrates that 100 replicas fit in 8 GiB only because each replica
   streams 1 MiB at a time; a whole-tensor read per replica would not fit.

## Reproduce

```
bash scripts/fleet_coldstart_court.sh 100
# WEFT_FLEET_N / WEFT_FLEET_MEMORY override N and the recorded court memory.
```
