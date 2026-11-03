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

F7 AI-corpus pass (2026-11-03):

- Synthetic corpus `bench/corpora/ai-v1/` (60 files, CC0-1.0, deterministic
  generator, per-file SHA-256 manifest, no downloaded or production data).
- `bench/receipts/ai-corpus-20261103T220113Z.json` — forced zstd, content-defined
  dedup + zstd, WeftCodec `auto`, and relation-conditioned
  DELTA/DICTIONARY/auto, with whole-object accounting, cold dependency-DAG
  latency, dependency depth, and every loss.
- Cost graph JSON/CSV beside the receipt; analysis and caveats in
  `bench/RESULTS-ai-v1.md`. The typed TENSOR/TOKENS modes were evaluated for all
  20 typed objects and chosen 0 times; that loss is published, not hidden.
- Not measured: external SQLite/graph/ext4/XFS AI-corpus baselines, trained
  zstd dictionaries, or 10^4–10^6-object scaling.

Still not measured: external SQLite/graph-store baselines, ext4/XFS/Btrfs/ZFS
baselines, physical power loss, energy, cold-cache and direct-I/O filesystem
runs, metadata storms, concurrency sweeps, and a matched Go/Rust codec court.
No headline performance claim is made from this first benchmark pass.

## 9. F5 proof courts (B1–B4)

These are correctness/adversarial courts, not performance benchmarks. Each
runs only inside `scripts/docker-court.sh` with the unchanged hard limits
(`--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`, bounded `/tmp`,
`no-new-privileges`, `cap-drop=ALL`; FUSE courts add `/dev/fuse` +
`CAP_SYS_ADMIN`).

### B1 — decoder bombs

```sh
scripts/docker-court.sh bench "bash bench/scripts/run_decoder_bomb_court.sh"
```

- 36 committed vectors under `WeftCodec/conformance/v1/adversarial/`, copied
  byte-identically to `WeftCodec-Go/conformance/v1/adversarial/`; the court
  asserts every mirrored file hash matches.
- Every vector is run twice per implementation under
  `timeout 10s /usr/bin/time -f 'maxrss_kb=%M wall_s=%e'`. The court asserts
  nonzero exit, the expected normative class in both, identical classes, no
  output file, no timeout, no OOM kill (137), stable classes across repeats,
  and max RSS below 512 MiB.
- Coverage: declared 10 GiB output, zstd expansion under a low bound, zstd
  output-length mismatch, a tiny body claiming 4 GiB (the allocation-bomb
  regression), delta offset/length arithmetic and op counts, dependency
  depth/cycle/count/total-fetch limits, chunk extent/overlap/gap/count faults,
  header length overflow, unknown mode/flag/feature, non-minimal/overflow
  WCB varints, WCB depth/items, selector depth/input, archive entry-count,
  non-topological archive order, dictionary mismatch and size, frame/archive
  truncation boundaries.
- Receipt: `bench/receipts/decoder-bombs-20261103T213314Z.json` (36/36 pass)
  and its raw `.log`. Class matrix: LimitExceeded 15, Malformed 12, Corrupt 2,
  DependencyCycle 2, UnsupportedFeature 2, NonCanonical 1, UnsupportedMode 1,
  DependencyMismatch 1. Peak measured RSS was below 5 MiB for every vector.
- Not claimed: a proof of absence; coverage is curated, not fuzzing. No 10 GiB
  payload is actually produced or allocated.

### B4 — randomized POSIX torture

```sh
scripts/docker-court.sh fuse "bash bench/scripts/posix_torture.sh"
```

- `fsx`-style model of namespace and content; operations: create, write, read,
  truncate, append, rename, rename-over, link, unlink, mkdir, rmdir, symlink,
  readlink, user xattr, fsync, open-unlinked. Every operation is checked for
  bytes, size, mode, nlink, symlink target, and xattrs; a full-tree check runs
  every 100 operations.
- Seeds 1–3, both directions (Rust writer → Go reader, Go writer → Rust
  reader): six runs, 2,251–2,342 completed operations each (13,694 total).
  After unmount the other implementation runs `verify --full`, remounts, and
  compares an exact manifest (names, types, modes, uid/gid, nlink, mtime,
  content sha256, symlink targets, xattrs).
- Receipt: `bench/receipts/posix-torture-20261103T213317Z.json` (6/6 pass).
- Not claimed: `pjdfstest`/`fsstress` (not installed in the court; no substitute
  run is claimed), mmap, byte-range locking, mknod, fallocate,
  copy_file_range, SEEK_DATA, or atime/ctime equality.

### B2 — crash-consistency torture

```sh
scripts/docker-court.sh fuse "bash bench/scripts/crash_torture.sh"
# documented 10,000-iteration command (not run; see projection):
scripts/docker-court.sh fuse "WEFT_CRASH_ITERATIONS=10000 bash bench/scripts/crash_torture.sh"
```

- 1,000 iterations alternating Rust/Go writers and readers through the
  existing `WEFT_CRASH_PHASE` hook, cycling `after_objects`,
  `after_commit_file`, and `after_superblock_write`.
- After every crash the other implementation runs `verify --full`; only the
  documented “commit file above the highest published superblock generation”
  note is accepted as a non-clean result. `/f` must be exactly the old whole
  file or the new whole file, and `/` must contain exactly `f`.
- Observed: old 667, new 333, partial/missing 0; verify clean 667,
  unpublished-tail 333, bad 0; phases 334/333/333. Duration 350,541 ms
  (2.9 iterations/s).
- Receipt: `bench/receipts/crash-torture-20261103T213446Z.json` (1/1 pass).
- Not claimed: physical power loss, cache drops, torn sectors, or a 10,000-
  iteration run. Linear projection from the measured rate is 3,505 s
  (≈58.4 min); the actual cost grows because `verify --full` rescans a growing
  commit chain every iteration.

### B3 — one-command receipt bundle

```sh
scripts/package-receipts.sh
scripts/verify-receipts.sh bench/bundles/receipts-<stamp>.tar.zst --repo-root .
```

The bundle contains `manifest.json` (kernel, Docker client/server, court image
IDs and Dockerfile sha256s, repo revisions, corpus and receipt hashes, raw log
list, dmesg status), `CHECKSUMS.sha256`, `REVISIONS.txt`, every receipt and raw
log, the conformance corpus, and the F5 harness/Dockerfiles. The verifier
extracts to a private temporary directory, rejects absolute/parent-traversing
tar entries, verifies every sha256, rejects unlisted files, checks every
revision against `git` when `--repo-root` is supplied, and confirms no
`research/` tree is present. Text files are sanitized for producer host paths;
`manifest.json` records the original and bundled hashes. `dmesg` is recorded
as `unavailable (no CAP_SYSLOG)` when the packaging user lacks the capability.
No secrets, `research/`, or out-of-bundle host paths are included.

## 10. F9 model-checking contract

A property is called **model-checked** only when all of the following hold:

- the TLA+ module, configuration, constants, and tool version are committed;
- `scripts/run_tla.sh` ran in the hard-limited `tla` Docker court;
- TLC terminated with `No error has been found` for the corrected model and
  the declared invariants, over the declared bounded constants;
- a deliberately flawed model was also checked and TLC produced the expected
  counterexample (a harness that cannot find the injected defect is a
  harness failure);
- the receipt records states generated, distinct states, search depth, result,
  tool version, jar sha256, court image, and repository revisions;
- the abstraction mapping and assumptions are committed in
  `verification/README.md`.

Commands:

```sh
scripts/run_tla.sh
```

Current evidence: `bench/receipts/tla-20261103T224446Z.json` — corrected model
`MaxGen=8`, 615 states generated, 591 distinct, depth 68, pass; flawed model
`MaxGen=3`, `NoPartial` counterexample. The pinned tool is TLC 1.7.4
`tla2tools.jar` sha256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`.

Explicitly **not** claimed: that the Rust or Go source refines the model; that
checkpoints, GC, capability filtering, selector semantics, or multi-writer
behaviour are model-checked; that a bounded TLC run proves absence of all
defects. A model check is not a substitute for the crash, POSIX, and
cross-implementation courts.

## 11. F9 power-loss court contract

This court answers one question: after a virtual-machine power cut at a random
point in the write loop, do the two independent implementations recover the
same exact whole state, and does every commit that returned before the cut
survive?

Rig contract:

- QEMU runs inside the hard-limited `vm` Docker court; no build/test executes
  on the host. The guest kernel/initramfs and read-only root are built in the
  image; the data disk is a fresh qcow2 overlay per iteration.
- The guest mounts ext4 with `data=ordered,barrier=1` and **never runs fsck**.
  Kernel journal replay is recorded from the guest `dmesg`.
- The write loop uses the Rust CLI, one commit per write, and never calls
  `sync(1)`. A `COMMIT` marker is printed only after the CLI returns.
- The host kills `qemu-system-x86_64` with `SIGKILL`; the guest is not shut
  down and no QEMU cache flush is requested. The kill delay is seeded and
  recorded per iteration.
- After reboot, Rust and Go each open the crashed image, run `verify --full`,
  and read the target file. The host asserts: identical generation; identical
  snapshot root; identical bytes; exact expected content for that generation;
  every returned commit is present; and only documented crash artifacts are
  accepted from `verify`.
- Accepted non-clean verify results are limited to (a) the documented
  `above the highest published superblock generation` note and (b) an
  unparseable commit file at a generation strictly above the recovered one.
  Any other problem fails the iteration.
- A never-fsynced control file on the data filesystem must be smaller than the
  number of returned commits; this proves the rig discarded guest page cache
  and is not a process-abort substitute. The published runs observed control
  loss in every iteration.

Commands:

```sh
scripts/powerloss_court.sh
scripts/docker-court.sh vm "POWERLOSS_ITERATIONS=30 POWERLOSS_CACHE_MODE=none bash scripts/powerloss_court.sh"
```

Published evidence: `bench/receipts/powerloss-20261103T224453Z.json` — 30/30
pass, 28 old + 2 new, 0 partial, 0 durability failures, 0 Rust/Go
disagreements, 1 accepted unpublished tail, control loss 30/30, KVM,
`cache=none`. Secondary: `bench/receipts/powerloss-20261103T224652Z.json` —
10/10 pass, `cache=writeback`, 0 unpublished tails, control loss 10/10.

Explicitly **not** claimed: physical media power loss, real device write-cache
loss, host page-cache loss, an fsync that lies to the guest, or a 10,000-
iteration campaign. A process abort alone is not accepted as physical
power-loss evidence and must never be relabelled as such.
