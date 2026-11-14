# Multi-writer benchmark contract (R4) — FROZEN

Version: `weft.bench.multiwriter.v1`. Frozen 2026-11-09.

This file fixes the **method** so a later run is comparable. It does not fix
the results; those are in [`RESULTS-multiwriter.md`](RESULTS-multiwriter.md).
Changing a fixture size, count, distribution, tool, or metric below requires a
new contract version and a new receipt schema.

## 1. What is measured

| Quantity | Definition |
|---|---|
| successful durable transactions/s | committed transactions divided by the wall from the first writer spawn to the last writer exit. A transaction is committed only when `ExecuteTransaction` returned `Ok` after the head publication. |
| logical write throughput | committed transactions/s × logical bytes per transaction. `logical_bytes` is the sum of **payload** bytes of committed transactions; it excludes the canonical object/entry/tip records and the manifest, which are counted only as physical backend bytes. This is a recorded gap, not a claim of total logical accounting. |
| physical write throughput | backend `writes` and `bytes_written` deltas for the writers' handles; local POSIX counters. |
| per-writer p50/p95/p99 | one latency sample per requested transaction, measured around the whole optimistic execution (catch-up, conflict validation, quota, encode, commit record, head CAS, transaction record). The receipt stores every writer's own percentiles (`per_writer`) and the row stores the min/median/max across writers (`writer_p50_ms_min` etc.). Merged percentiles are also reported but are not a substitute for per-writer behaviour. |
| conflict / abort / retry rates | `conflicts` (typed `Conflict`), `aborted` (the outer retry budget was exhausted with `Busy`, so the outcome is unknown and must be resolved by transaction id), and retry amplification = attempts / committed. |
| commit latency split | `prepare` (content manifest publication only) and `execute` (everything after it) are measured separately. Validation and publication are **not** separately instrumented; this is recorded as a gap. |
| metadata ops per commit | backend reads + writes divided by committed transactions. Local counters only; the Go driver and the S3 client do not expose equivalent per-process counters. |
| CPU / RSS | sum of per-process `VmHWM` and approximate `/proc/self/stat` user+sys ticks (USER_HZ=100). Approximate, not a RAPL/energy measurement. |
| scaling efficiency | `throughput(N) / (N * throughput(1))` for N = 1, 2, 4, 8, 16, 32, local backend only. The S3 row has no same-backend N=1 baseline, so its `scaling_efficiency` is `null` and no S3 scaling claim is made. Higher N is **not** extrapolated. |

## 2. Fixtures

Local backend:

* one store per configuration, initialized by the Rust driver in `single` mode
  and then opened by each writer with `--writer-mode optimistic`;
* N independent **processes**, each with its own store handle;
* each transaction creates one regular file: `PrepareContent` (raw v1 manifest)
  + `PutObjectCreate` + `PutEntryCreate` + `SetTip` in one commit;
* payload = the decimal string `"<writer>:<index>"` (4–8 bytes);
* fixed clock `1_700_000_000_000_000_000 ns` for every mtime and commit;
* deterministic object seed `"<writer>:<index>:seed"`;
* transaction id = first 32 hex characters of `BLAKE3("weft/tx/id/v1", "<writer>:<index>")`;
* disjoint distribution: name `"<writer>-<index:06d>"`;
* high-contention distribution: every transaction in every writer targets the
  name `"hot.bin"` (create semantics, so exactly one can win);
* transaction options: `max_attempts=64`, outcome window 256, outer Busy retries
  up to 3 with a 20 ms pause (the caller's `outcome_unknown` resolution);
* counts: N=1..8 → 60 transactions per writer; N=16 → 15; N=32 → 7 (240
  requested in total per configuration); contention → 10 per writer at every N.

MinIO:

* the pinned official MinIO release `RELEASE.2025-04-22T22-12-26Z`, sha256
  `53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`, in
  `debian:bookworm-slim` under the court hard limits;
* tmpfs data directory (1 GiB), single node, path-style addressing;
* N=4 Rust processes, disjoint, 10 transactions per writer (40 total);
* verified with `weftfs --backend s3 verify --full` after the run;
* the S3 receipt row has no `generation` (the check requires a local store
  directory) and no scaling efficiency; both are recorded as `null`.

## 3. Commands

```
# local: N = 1,2,4,8,16,32 disjoint + high contention, mixed Rust/Go N=8
bash scripts/docker-court.sh bench "cd /work && bash scripts/bench_multiwriter.sh"

# MinIO: the same local rows plus N=4 disjoint on S3
bash scripts/multiwriter_s3_court.sh
```

The driver is `examples/mw2_writer.rs` (Rust) and
`WeftFS-Go/cmd/mw2writer/main.go` (Go). It is a court driver, not a product CLI.

## 4. Environment

Every run records the court image id, kernel, memory/CPU/pids limits, and the
Rust/Go repository revisions. The committed receipt was produced on 4 vCPUs
with `--memory=3g --memory-swap=3g --pids-limit=1024`. A different environment
is a different measurement and must not be spliced into these tables.

## 5. Exclusions

* No physical power loss, host page-cache loss, device-cache loss, RAPL energy,
  GPU, fleet, multi-node, or cloud measurement.
* No FUSE-frontend multi-writer run: FUSE file mutations remain single-writer.
* No generic checkpoint publication over MW2: only the explicit transaction API
  is measured.
* No Go S3/GCS MW2: Go implements MW2 for the local and object commit planes.
* One repetition per configuration. No confidence interval is claimed; the
  receipt is a single run and is labelled as such.
* The wall includes process startup, store open/recovery, and driver JSON
  emission; it is not a pure transaction loop.

## 6. Receipt

`bench/receipts/bench-multiwriter-<UTC>.json`, schema
`weftfs.multiwriter.bench/1`, with one row per configuration and the exact
fields above. The raw log is beside it and its sha256 is in the receipt.

## 7. MW3, T021 and T027 courts (added 2026-11-14)

These are separate courts with their own receipts and their own driver; they
do not modify §§1–6, and no row from them is spliced into the MW2 tables.

* **MW3 (`scripts/mw3_court.sh`, `bench/receipts/mw3-<stamp>.json`).** One
  conflict domain per writer; each writer is a process with its own sharded
  handle. Distributions: `disjoint` (distinct names, N = 1, 2, 4, 8, 16, 32)
  and `same-domain` (every writer targets `hot.bin` in one domain with create
  semantics, N = 2, 8, 32, ten transactions per writer). Counts: N ≤ 8 → 500
  transactions per writer; N = 16 → 200; N = 32 → 100. Each domain is verified
  with `--domain ... verify --full`; the global verifier must refuse. The
  receipt records throughput, efficiency, p50/p95/p99, retry amplification,
  reads/commit, gap commits read, conflicts and RSS. No cross-domain atomicity
  is requested; that refusal is a unit test, not a benchmark.
* **T021 (`bench/receipts/mw2-retry-<stamp>.json`).** The frozen §2 local
  disjoint fixture at N=32 only (32 processes, seven transactions each,
  `checkpoint_interval = 0`), with two arms: `before` is a release build of the
  tree at the subphase's start revision (a `git archive`, same `WeftCodec`
  sources), `after` is the measured tree. Three repetitions per arm,
  **interleaved**, and the comparison uses each arm's median. Reported:
  reads/commit, p50/p95/p99, throughput, retry amplification, gap commits read,
  intervening-commit cache hits, and max consecutive retries. The baseline
  binary path is passed in `WEFT_MW3_BEFORE` and named in the receipt; without
  it the receipt contains the after arm only and says so.
* **T027 (`bench/receipts/writer-scale-<stamp>.json`).** The compact client
  `examples/mw3_compact.rs`: N writer **threads in one process**, each with its
  own handle and domain, N = 8, 16, 32, 64, 128, 256, 512, 1024, five to fifty
  transactions per writer. At N ≥ 512 every worker is **held after spawning**,
  so the peak thread count is real and the court's pid bound is measured (a
  refused spawn is recorded). One 32-writer cell uses one process per writer
  and exists only to prove that path still works. Reported: throughput,
  p50/p95/p99, max consecutive retries, peak RSS and RSS per writer, max
  head-state rows per writer, threads spawned, spawn failures, and the timeout
  return code.

The environment is the same hard-limited bench court as §4. Every court is one
host, one local namespace, warm cache, and makes no fleet, cloud, media,
power-loss, Go-parity or cross-domain-atomicity claim. `scripts/verify-all.sh`
runs the reduced configuration as the `mw3` court; a court that cannot run
prints one `mw3: skip <reason>` line and exits 78.
