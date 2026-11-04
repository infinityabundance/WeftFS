# WeftFS

**Context-addressed storage, with byte-exact reconstruction.**

WeftFS is a research program building four independent, interoperable
implementations of one storage architecture:

| Project | Language | Role |
|---|---|---|
| [`WeftCodec`](WeftCodec/) | Rust | Lossless context-conditioned codec, normative format |
| [`WeftCodec-Go`](WeftCodec-Go/) | Go | Independent codec implementation, cross-decode conformance |
| [`WeftFS`](.) | Rust | Transactional store + Linux FUSE filesystem |
| [`WeftFS-Go`](WeftFS-Go/) | Go | Independent store + Linux FUSE filesystem |

The central question is whether **typed, versioned contextual relationships**
can be a native addressing and reconstruction primitive — distinct from paths,
content hashes, and object IDs — while retaining exact materialization, durable
POSIX access, reproducible history, and competitive performance.

This repository is the program home. It owns the normative specification
(`spec/`) and exports versioned copies to the subprojects.

## Status

Status is deliberately blunt. Nothing is claimed beyond what has been built and
tested in the Docker court.

| Phase | Deliverable | Status |
|---|---|---|
| P0 | Repositories, staging workflow, OOM-protected Docker courts, licenses | **Implemented** |
| P1 | Normative specs: model, selectors, codec format, compatibility | **Implemented** (v1 frozen; clarifications recorded in the specs) |
| P2 | Rust WeftCodec v1 (all modes, CLI, tests) | **Implemented** — Rust unit and integration suites pass; 60 codec, 20 WCB, 22 selector, 16 context, 10 causal, 36 adversarial vectors |
| P3 | Golden vectors + independent Go WeftCodec + bidirectional cross-decode | **Implemented** — Go unit/vector suites pass; Rust→Go and Go→Rust decode gates pass for all modes, including byte-identical TENSOR, TOKENS, and QUANTIZED_TENSOR |
| P4 | Rust WeftFS core (transactions, snapshots, graph, recovery) | **Implemented** (store core, no FUSE) — 39 core tests — see [`docs/store-status.md`](docs/store-status.md) |
| P5 | Rust FUSE frontend, POSIX matrix, context views | **Implemented** — 81 Rust court tests pass, 0 fail, 2 `#[ignore]`d: ownership needs `CAP_SETUID`; same-inode direct-I/O concurrency hits a reproducible kernel/fuser EIO. Store-level atomicity is tested separately — see [`docs/fuse-status.md`](docs/fuse-status.md) |
| P6 | Go WeftFS core + FUSE + storage handoff court | **Implemented with documented gaps** — 31 Go tests + 13 GC tests, byte-identical fixture rebuild, bidirectional Rust↔Go handoff and GC. Daemon-side locking, tmpfile, fallocate modes, copy_file_range, and SEEK_DATA/HOLE are implemented; kernel 7.3 rejects COLLAPSE/INSERT before forwarding and physical-space PUNCH_HOLE is not claimed. See [`WeftFS-Go/NOTES.md`](WeftFS-Go/NOTES.md) |
| P7 | Bench courts, receipt bundle, showcase, release | **In progress** — verification court passes (`bench/receipts/verify-20261103T172416Z.json`); the `bench` court produced codec, store, and `fio` filesystem receipts (`bench/receipts/bench-*`). Losses and gaps are published in [`bench/RESULTS-v1.md`](bench/RESULTS-v1.md); no headline performance claim is made. |
| F2 | Context Roots, capability/budget enforcement, causal identity (Rust + Go) | **Implemented** — 70 Rust tests pass (2 ignored: `CAP_SETUID` ownership and the documented kernel/fuser EIO), Go `store` + `fusefs` pass under `-race`, and Rust↔Go context/causal/capability handoff gates pass. See [`NOTES.md`](NOTES.md) and [`docs/STATUS.md`](docs/STATUS.md). |
| F3 | Context mounts, ambiguity court, time-travel POSIX, replay bundle (A1/A2/A3/A4/D2) | **Implemented** — `mount --context` / `--at` in Rust and Go; ≥3 concurrent read-only mounts; ambiguity fails closed and lists every ObjectId; 100-version time-travel demo; Rust `export-bundle` + independent Go replay. Receipts: `context-mounts-20261103T210625Z.json` (6/6), `ambiguity-20261103T210633Z.json` (4/4), `time-travel-20261103T210633Z.json` (4/4), `replay-bundle-20261103T210824Z.json` (4/4). Time travel is correctness-only: no io_uring/EKBLOOM acceleration is implemented or claimed. |
| F5 | Proof courts: decoder bombs, randomized POSIX torture, crash-consistency torture, receipt bundle (B1–B4) | **Implemented with documented gaps** — 36/36 decoder bombs, 6/6 POSIX runs (13,694 ops), **10,000/10,000 crash iterations at 19.7 it/s** (6,667 old / 3,333 new, 0 partial, 0 failures); one-command bundle verifier. Receipts: `decoder-bombs-20261103T213314Z.json`, `posix-torture-20261103T213317Z.json`, `crash-torture-20261104T013521Z.json`. Crashes are process-fault proxies, not physical power loss; `pjdfstest`/`fsstress` are not available and no substitute run is claimed. |
| F6 | AI codec v1: exact tensor + token modes, cross-language | **Implemented (subset)** — `TENSOR` (6, row-major little-endian, nine dtypes) and `TOKEN_SEQUENCE` (7, u64 zigzag-delta LEB128) with canonical WCB descriptors, limits, 20 new vectors, rejects, CLI, and byte-identical Rust↔Go frames. Blocked/column-major/sharded/quantized layouts, activation/gradient streams, KV-cache pages, masks, weight-delta descriptors, and semantic deltas are `specified, not implemented` with no wire value. See `spec/WEFT-AI-CODEC.md`. |
| F7 | AI corpus + cost graph (C1/C2) | **Implemented (synthetic)** — 60-file deterministic `bench/corpora/ai-v1` (CC0-1.0, 1.94 MB source / 1.52 MB visible), four strategies with whole-object accounting, cold dependency-DAG latency, and depth. Receipt `ai-corpus-20261103T220113Z.json`; results, every loss, and caveats in [`bench/RESULTS-ai-v1.md`](bench/RESULTS-ai-v1.md). No Meta/production data and no superiority claim. |

Labels used throughout: **implemented** (built and tested), **experimental**
(built, not format-frozen), **conjectured** (design claim without measurement).

## Non-negotiable engineering rules

1. **Exactness.** Every decoded byte sequence must match its declared content
   root. Hash mismatch fails closed. No lossy fallbacks.
2. **No invented evidence.** Every performance, reliability, or compatibility
   claim carries a command, environment, fixture, and raw output.
3. **Independent implementations.** Go never calls Rust. The two codecs share a
   format, not code. The two filesystems share an on-disk format, not code.
4. **Bounded resources.** Decoders enforce depth, byte, count, and work limits.
   Containers enforce hard memory/swap/pids/CPU limits.
5. **Identity separation.** A selector is a query, not an object. A hash is not
   an authorization token. Ambiguity is returned, never silently resolved.
6. **One writer.** The store is single-host, single-writer with an explicit
   `flock` ownership protocol.

## Container images

Published on GHCR, anonymously pullable:

```sh
docker pull ghcr.io/infinityabundance/weftcodec:v0.3.0
docker pull ghcr.io/infinityabundance/weftcodec-go:v0.3.0
docker pull ghcr.io/infinityabundance/weftfs:v0.3.0
docker pull ghcr.io/infinityabundance/weftfs-go:v0.3.0
```

`latest` is also published. FUSE mounts require `--device /dev/fuse --cap-add
SYS_ADMIN`; codec and store-only commands do not. See
[`docs/containers.md`](docs/containers.md).

## Testing: Docker only

All builds and tests execute inside minimal official images. The host runs
only `git`, `gh`, and Docker. The runner hard-codes OOM protection and refuses
unbounded values:

```sh
scripts/docker-court.sh rust "cargo test --all-targets -- --test-threads=1"
scripts/docker-court.sh go   "cd WeftCodec-Go && go test ./..."
scripts/docker-court.sh fuse "cargo test --features fuse --all-targets -- --test-threads=1"
```

Container defaults: `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
`--tmpfs /tmp:size=1m`-bounded (1 GiB), `no-new-privileges`, `cap-drop=ALL`,
and for FUSE only: `/dev/fuse` plus `CAP_SYS_ADMIN`.

## Twelve-step showcase

One command reproduces the mandated end-to-end scenarios (mixed contexts,
context-graph cycle plus acyclic codec DAG, ambiguity, pin stability, Rust FUSE
writes, Rust↔Go handoff, process-fault crash proxies, authorization denials,
and negative compression workloads):

```sh
scripts/docker-court.sh fuse "bash scripts/showcase.sh"
```

The run writes a schema-versioned receipt and its raw console log to:

```
bench/receipts/showcase-<UTC>.json
bench/receipts/showcase-<UTC>.log
```

Every step records its commands, fixture hashes, assertions (checked with
`cmp`, hashes, and JSON field equality), observed outputs, and the court
environment. A determinism re-run compares semantic fields while ignoring
wall-clock fields:

```sh
SHOWCASE_RECEIPT_NAME=showcase-rerun \
SHOWCASE_COMPARE_WITH=/work/bench/receipts/showcase-<UTC>.json \
scripts/docker-court.sh fuse "bash scripts/showcase.sh"
```

A blocked capability is recorded as `"status": "blocked"` with a precise
reason; the script never marks an unobserved step passed.

## F3 demos (context mounts, ambiguity, time travel, replay)

Each demo builds its own deterministic fixture, runs in the FUSE court, and
writes a schema-versioned receipt plus a raw log:

```sh
scripts/docker-court.sh fuse "bash scripts/demo-context-mounts.sh"
scripts/docker-court.sh fuse "bash scripts/demo-ambiguity.sh"
scripts/docker-court.sh fuse "bash scripts/demo-time-travel.sh"
scripts/docker-court.sh fuse "bash scripts/demo-replay-bundle.sh"
```

- **Context mounts** create two byte-identical objects with distinct
  ObjectIds, five typed relations (`task`, `model`, `generated_by`,
  `derived_from`, `prompted_by`), two snapshots at distinct fixed times, and
  mount `by:task=agent-42` (Rust), `by:model=llama-4@ver_19` (Go), and
  `--at 2026-01-01T00:00:00Z` (Go) simultaneously. Every listed file is
  checked against the SHA-256 attribute declared in the store and against the
  BLAKE3 ContentId from `stat`/`context resolve`; a writer commits while the
  three read-only mounts are live.
- **Ambiguity** matches two objects with different ContentIds; the full query
  returns the ordered set, `--require-one` and `context materialize
  --require-one` fail with `Ambiguous` and list both ids, the Context mount
  lists both, and no singular output path is created.
- **Time travel** creates 100 versions of one path, snapshots versions 1, 50,
  and 100, mounts all three at once (Rust + Go), verifies exact bytes, then
  renames and retags the live path and re-reads every mounted snapshot
  unchanged.
- **Replay bundle** writes `bench/bundles/weft-receipt.tar.zst` from Rust and
  verifies it with Go only: extract, `verify --full`, resolve and materialize
  every Context Root, then re-hash every store entry and output file against
  `manifest.json`. The receipt records the bundle SHA-256 and the one-command
  reproduction.

A second Context-mount run is compared with the first using
`scripts/showcase/democmp.go`; the result is
`bench/receipts/context-mounts-determinism.txt` (semantic fields identical,
run ids and wall-clock fields excluded).

## F5 proof courts

One command per court; all run only inside `scripts/docker-court.sh` with the
unchanged hard limits.

```sh
# B1: 36 adversarial vectors, both independent CLIs, 10s timeout, class matrix
scripts/docker-court.sh bench "bash bench/scripts/run_decoder_bomb_court.sh"

# B4: fsx-style randomized POSIX ops, 3 seeds, both writer/reader directions
scripts/docker-court.sh fuse "bash bench/scripts/posix_torture.sh"

# B2: process-fault crash proxy through WEFT_CRASH_PHASE (default 1,000)
scripts/docker-court.sh fuse "bash bench/scripts/crash_torture.sh"
# 10,000-iteration run (2026-11-04: 10,000/10,000, 19.7 it/s)
scripts/docker-court.sh fuse "WEFT_CRASH_ITERATIONS=10000 bash bench/scripts/crash_torture.sh"

# B3: package and independently verify a receipt bundle
scripts/package-receipts.sh
scripts/verify-receipts.sh bench/bundles/receipts-<stamp>.tar.zst --repo-root .
```

Honest gaps:

- Decoder bombs are a curated 36-vector corpus, not fuzzing or a proof of
  absence. The declared 10 GiB vector is rejected before allocation; no 10 GiB
  payload is produced.
- The POSIX court is a self-written fsx-style harness, not `pjdfstest` or
  `fsstress` (not installed in the court). The verb matrix closes mmap,
  locking (with the documented Rust binding boundary), mknod, fallocate,
  `copy_file_range`, `SEEK_DATA`, and the timestamp surface on Rust; Go covers
  the same verbs in `verbs_test.go`.
- The crash court aborts the process at a publication boundary; it is **not**
  physical power loss and does not drop caches or simulate torn sectors. The
  10,000-iteration run completed in 507.9 s (19.7/s) after switching to a fresh
  three-generation store per iteration; the old-or-new/partial checks are
  unchanged.
- The bundle records `dmesg: unavailable (no CAP_SYSLOG)` when the packaging
  user lacks the capability. It is not signed; it is self-verifying against its
  own checksums and, with `--repo-root`, against git revisions.

## Benchmarks

The `bench` court runs the three P7 harnesses under the same hard limits as
the other courts. It is FUSE-capable and includes the external compression
CLIs plus `fio`.

```sh
scripts/docker-court.sh bench "bash bench/scripts/run_codec_ext_bench.sh"
scripts/docker-court.sh bench "bash bench/scripts/run_store_bench.sh"
scripts/docker-court.sh bench "bash bench/scripts/run_fs_bench.sh"
scripts/docker-court.sh bench "bash bench/scripts/run_perf_counters.sh"
scripts/docker-court.sh vm "WEFT_FS_COLD_TARGETS=ext4,xfs,zfs WEFT_FS_COLD_REPS=1 bash bench/scripts/vm_fs_court.sh"
```

Each harness writes a receipt with raw samples and a raw log to
`bench/receipts/`. The current results, every loss, and the remaining gaps are
in [`bench/RESULTS-v1.md`](bench/RESULTS-v1.md); tables can be regenerated with
`bench/scripts/summarize_bench.py` and two receipts compared with
`bench/scripts/compare_bench.py`. The tmpfs filesystem reference is
reference-only and explicitly non-equivalent to WeftFS.

## Repository layout

```
spec/          Normative specifications (single source of truth)
docs/adr/      Architecture decision records
scripts/       Docker court and spec export
docker/        Minimal court images
WeftCodec/     Rust codec (independent git repo later; directory now)
WeftCodec-Go/  Go codec
WeftFS-Go/     Go filesystem
src/, crates/  Rust filesystem implementation
tests/         POSIX, crash, snapshot, graph, interop courts
conformance/   Frozen golden and invalid vectors
security/      Threat model and audit notes
bench/         Contracts, corpora manifests, scripts, receipts
research/      Local-only research notes (gitignored)
```

## Licensing

Code: Apache-2.0. Specification and research prose: CC BY 4.0.
See `LICENSE` and `spec/` headers. Third-party notices are recorded as
dependencies are added.
