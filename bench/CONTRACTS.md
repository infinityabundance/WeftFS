# Benchmark contracts (P7)

**Status:** Methodology fixed before any headline result. No comparative
performance claim has been produced yet. This file exists so that future
measurements cannot be retrofitted to a preferred conclusion.

## 1. Claims policy

- A result is reported only with: exact command, image digest or image name and
  pulled version, host kernel/CPU/memory/storage, mount options and cache mode,
  corpus license and manifest hash, source revisions, raw samples, and the
  receipt path.
- No "fastest", "smallest", "world record", "novel", "production-ready", or
  "drop-in" language without a scoped, reproducible comparison.
- Losses and Pareto tradeoffs are published alongside wins.
- Failed hypotheses become documented negative results. Baselines are never
  weakened after the fact.

## 2. Environment capture

Every run records:

```
uname -a
docker version
docker image inspect <image> --format '{{.Id}} {{.RepoDigests}}'
nproc; lscpu; free -h; df -h <store path>; mount | grep <store path>
cat /sys/block/<dev>/queue/scheduler
cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor
```

Court limits must be identical between contestants: `--memory=3g
--memory-swap=3g --pids-limit=1024 --cpus=4`, bounded `/tmp`. A competitor that
needs more memory is run in a separate, equally documented court, not compared
silently against a constrained run.

## 3. Codec family

Workloads: empty; 1 byte; 4 KiB random; 4 MiB random; 4 MiB repetitive; 64 KiB
near-duplicate revisions (1%, 5%, 20% edits); source tree; JSON/CSV; executable;
8 MiB audio/video-like opaque data; 1000 tiny files; invalid UTF-8 payloads.

Measure separately:

- encoded bytes including dependencies, dictionaries, manifests, archive
  overhead, and index bytes;
- encode/decode wall time, CPU time, peak RSS, allocations;
- full decode and ranged read latency (chunk-local vs full-file decode);
- incremental update cost;
- dependency fanout, depth, and repair cost;
- streaming time-to-first-byte where a streaming API exists.

Baselines: zstd at tuned levels (3, 9, 19), zstd trained dictionaries, LZ4,
Brotli, gzip, XZ, xdelta/rsync-style deltas, content-defined dedup + zstd, and
WeftCodec with contextual candidates disabled. Raw samples and medians; no
single-run ratios.

## 4. Context addressing

Workloads: selector cardinality 0/1/n; hop depth 1 (v1); relation insert/delete
churn; snapshot resolve; live views; 10^3–10^6 objects and edges; cold/warm
index; authorization filtering.

Baselines: schema-indexed SQLite (with correct indexes, FTS5 where relevant), a
graph/triple store (version recorded), and a POSIX tree plus an external tuned
index. Multi-result selectors are sets; returning many rows is not a bug.

Report: query latency, index build bytes/time, update amplification, memory per
relation, selector canonicalization cost, explain overhead.

## 5. Filesystem

Workloads: sequential/random read/write, 4 KiB and 1 MiB I/O, `fio` tail
latency, create/unlink/rename/mkdir/rmdir storms, directory sync, open-unlinked,
hardlink/symlink, xattrs, 8 MiB+ files, concurrent readers (same and separate
files), snapshot create/reopen, crash/recovery, GC, cold/warm cache, cold start.

Baselines: ext4/XFS/Btrfs/ZFS where the court provides them, a minimal
passthrough FUSE filesystem, and a SQLite-index + directory-tree equivalent for
query workloads. FUSE cache mode and mount options are recorded; comparing
different cache modes is prohibited unless explicitly labelled.

Known court limitation: same-inode concurrent direct read/write on kernel 7.3
can return `EIO` (documented in `docs/fuse-status.md`). Do not benchmark that
path until the kernel/fuser interaction is resolved.

## 6. Statistics

- Interleave or randomize paired runs; warm up; report at least 10 samples for
  microbenchmarks and 30 for tail-latency claims.
- Record median, mean, p95, p99, standard deviation, and raw samples.
- Use bootstrap confidence intervals for ratios; do not claim precision the
  sample cannot support.
- Report every loss as well as every win. A single tail-latency regression is a
  result, not an outlier to delete.

## 7. Receipts

`bench/receipts/` holds JSON/CSV with the command, environment, corpus manifest,
source revisions, raw samples, and analysis script. `bench/scripts/` holds the
runnable harness. A receipt without a corpus manifest and revision is invalid.

## 8. Current evidence

Correctness evidence: `scripts/verify-all.sh` produces a machine-readable
verification receipt, and the twelve-step showcase produces its own receipt.

First benchmark pass (2026-11-03):

- `bench/receipts/bench-codec-20261103T175847Z.json` — external zstd/gzip/xz/
  brotli baselines plus WeftCodec modes with marginal and whole-object
  accounting; every decode verified before timing.
- `bench/receipts/bench-store-20261103T175920Z.json` — 100/1,000/10,000-object
  stores, selectors, relations, snapshots, verify, GC, and on-disk bytes.
- `bench/receipts/bench-fs-20261103T180751Z.json` — `fio` against Rust and Go
  WeftFS mounts and a labelled tmpfs reference in one container.
- `bench/RESULTS-v1.md` — tables, scoped analysis, every published loss, and
  the explicit gaps.

Still not measured: external SQLite/graph-store baselines, ext4/XFS/Btrfs/ZFS
baselines, physical power loss, energy, cold-cache and direct-I/O filesystem
runs, metadata storms, concurrency sweeps, and a matched Go/Rust codec court.
No headline performance claim is made from this first benchmark pass.
