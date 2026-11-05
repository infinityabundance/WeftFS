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
| P5 | Rust FUSE frontend, POSIX matrix, context views | **Implemented** — 88 Rust court tests pass, 0 fail, 2 `#[ignore]`d: ownership needs `CAP_SETUID`; same-inode direct-I/O concurrency hits a reproducible kernel/fuser EIO. Store-level atomicity is tested separately — see [`docs/fuse-status.md`](docs/fuse-status.md) |
| P6 | Go WeftFS core + FUSE + storage handoff court | **Implemented with documented gaps** — 31 Go tests + 13 GC tests, byte-identical fixture rebuild, bidirectional Rust↔Go handoff and GC. Daemon-side locking, tmpfile, fallocate modes, copy_file_range, and SEEK_DATA/HOLE are implemented; kernel 7.3 rejects COLLAPSE/INSERT before forwarding and physical-space PUNCH_HOLE is not claimed. See [`WeftFS-Go/NOTES.md`](WeftFS-Go/NOTES.md) |
| P7 | Bench courts, receipt bundle, showcase, release | **v0.6.0 released; ongoing** — per-repo GHCR images and crates.io crates, per-repo GitHub releases, verification court, and the codec/store/`fio`/FUSE comparative suites exist. Latest full verification: `bench/receipts/verify-20261105T035513Z.json` (**25/25**, including `claims`, the five authenticated-capability cross-language courts, and `fuzz_diff`). Losses and gaps are published in [`bench/RESULTS-v1.md`](bench/RESULTS-v1.md), [`bench/RESULTS-comparative-v1.md`](bench/RESULTS-comparative-v1.md), [`bench/RESULTS-fuse-v0.5.md`](bench/RESULTS-fuse-v0.5.md), [`bench/RESULTS-capability-envelope.md`](bench/RESULTS-capability-envelope.md), and [`bench/RESULTS-real-model.md`](bench/RESULTS-real-model.md); no headline performance claim is made. See [`docs/RELEASE-v0.6.0.md`](docs/RELEASE-v0.6.0.md). |
| F2 | Context Roots, capability/budget enforcement, causal identity (Rust + Go) | **Implemented** — 70 Rust tests pass (2 ignored: `CAP_SETUID` ownership and the documented kernel/fuser EIO), Go `store` + `fusefs` pass under `-race`, and Rust↔Go context/causal/capability handoff gates pass. See [`NOTES.md`](NOTES.md) and [`docs/STATUS.md`](docs/STATUS.md). |
| F3 | Context mounts, ambiguity court, time-travel POSIX, replay bundle (A1/A2/A3/A4/D2) | **Implemented** — `mount --context` / `--at` in Rust and Go; default 3-mount demo plus **100 concurrent read-only context mounts** (50 `by:task`, 25 `by:model`, 25 `derived_from` roots; 425 file checks, 100/100 distinct listing digests); ambiguity fails closed and lists every ObjectId; default 100-version plus **10,000-version time travel** (101 snapshots, 10 mounted at once, later live write/rename leaves every snapshot byte-identical); Rust `export-bundle` + independent Go replay. Receipts: `context-mounts-20261103T210625Z.json` (6/6, default), `context-mounts-20261104T125344Z.json` (5/5, 100 mounts, `semantic_match=true`), `ambiguity-20261103T210633Z.json` (4/4), `time-travel-20261103T210633Z.json` (4/4, default), `time-travel-20261104T125947Z.json` (6/6, 10,000 versions, `semantic_match=true`), `replay-bundle-20261103T210824Z.json` (4/4). Single-host court, synthetic fixtures; not a fleet claim. Time travel is correctness-only: no io_uring/EKBLOOM acceleration is implemented or claimed. |
| F5 | Proof courts: decoder bombs, randomized POSIX torture, crash-consistency torture, receipt bundle (B1–B4) | **Implemented with documented gaps** — 36/36 decoder bombs, 6/6 POSIX runs (13,694 ops), **100,000/100,000 crash iterations at 19.2 it/s** (66,667 old / 33,333 new, 0 partial, 0 failures); one-command bundle verifier. Receipts: `decoder-bombs-20261103T213314Z.json`, `posix-torture-20261103T213317Z.json`, `crash-torture-20261104T104233Z.json`. Crashes are process-fault proxies, not physical power loss; `pjdfstest`/`fsstress` are not available and no substitute run is claimed. |
| F6 | AI codec v1: exact tensor + token modes, cross-language | **Implemented (modes 6–14)** — `TENSOR` (6), `TOKEN_SEQUENCE` (7), `QUANTIZED_TENSOR` (8), `FLOAT_TENSOR` (9, `huffman-canonical-v1`), `TENSOR_SHARDED` (10, `q4_0`/`q8_0`), `WEIGHT_DELTA` (11), `KV_CACHE` (12), `ACTIVATION_STREAM` (13), and exact `SEMANTIC_DELTA` (14) with canonical WCB descriptors, limits, vectors/rejects, CLI, and byte-identical Rust↔Go frames. Remaining brainstormed modes have no wire value. See `spec/WEFT-AI-CODEC.md` and [`docs/RELEASE-v0.5.0.md`](docs/RELEASE-v0.5.0.md). |
| F7 | AI corpus + cost graph (C1/C2) | **Implemented (synthetic)** — 60-file deterministic `bench/corpora/ai-v1` (CC0-1.0, 1.94 MB source / 1.52 MB visible), four strategies with whole-object accounting, cold dependency-DAG latency, and depth. Receipt `ai-corpus-20261104T021627Z.json` (whole-pair-aware planner); results, every loss, and caveats in [`bench/RESULTS-ai-v1.md`](bench/RESULTS-ai-v1.md). No Meta/production data and no superiority claim. |
| F8 | Product/identity hooks: Buck2 CAS, spec-as-code types, agent memory | **Implemented (prototype)** — Buck2 CAS prototype (15/15), generated Rust/Go identity types with a drift court, agent-memory replay demo (7/7), and `QUANTIZED_TENSOR` mode 8 byte-identical Rust↔Go. Not a Buck2 plugin and not REAPI; see [`docs/STATUS.md`](docs/STATUS.md). |
| F9 | Verification: TLA+ model, power-loss/block-layer courts, comparative baselines | **Implemented with documented limits** — TLA+ commit/recovery model with a buggy-model counterexample, QEMU lying-fsync and dm-flakey/blkdebug block-layer courts, whole-disk ZFS/Btrfs plus ext4/XFS `fio` baselines, and SQLite relational/adjacency baselines. Physical media power loss, host page-cache loss, and RAPL energy are **not** claimed. See [`verification/README.md`](verification/README.md). |
| F10 | Real public model checkpoint family: exact ingest, automatic shared/independent representation, storage and restore vs baselines | **Implemented, measured once** — Qwen2.5-0.5B + Instruct (BF16, 290 tensors, 988 MB each) ingested through the shipped tensor planner; every restored file matches the Hugging Face LFS SHA-256. ZipNN 0.5.4 wins complete storage by 5.27 % (1.317 GB vs 1.390 GB); WeftFS wins the instruct checkpoint by 8.6 % (delta sharing) but loses the base by 19.8 %, and loses full instruct restore by 9.7× to zstd / 73× to ZipNN. Single-tensor restore is 7.3–118.6 ms and 6.8–11.6 MB. DFloat11 and `zstd --patch-from -19` are unavailable (CUDA; 3 GiB OOM). Receipt `bench/receipts/real-model-20261105T012000Z.json`; every loss in [`bench/RESULTS-real-model.md`](bench/RESULTS-real-model.md). |

Labels used throughout: **implemented** (built and tested), **experimental**
(built, not format-frozen), **conjectured** (design claim without measurement).

## What's new in v0.6.0

Full note with every limit: [`docs/RELEASE-v0.6.0.md`](docs/RELEASE-v0.6.0.md).

- **Authenticated capability envelopes:** canonical `weft.capability.envelope.v1`, Ed25519, offline verification against an explicit caller-supplied revocation set, and byte-identical signed blobs in Rust and Go (five cross-language courts).
- **Layered enforcement:** every grant present must independently allow; budgets take the minimum and expiry the earliest bound; expired, revoked, wrong-grantee, and non-overlapping grants fail closed with non-disclosing errors.
- **Cross-tenant isolation court:** resolution, stat, enumeration, work units, and budgets are independent of the other tenant's population; deduplication is allowed only when each tenant independently holds an authorized object with that content root.
- **Verification:** `bench/receipts/verify-20261105T035513Z.json` is **25/25 courts pass**, including the 42-entry `claims` gate.
- **Unchanged:** frozen v1 formats and frame bytes; the Python wheel remains 0.5.0 and is not on PyPI.

Prior release: [`docs/RELEASE-v0.5.0.md`](docs/RELEASE-v0.5.0.md) (tensor-native storage, AI codec modes 8–14, backends, provenance).

Non-claims: the release note's “Still not claimed” and [`docs/STATUS.md`](docs/STATUS.md).

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
docker pull ghcr.io/infinityabundance/weftcodec:v0.6.0
docker pull ghcr.io/infinityabundance/weftcodec-go:v0.6.0
docker pull ghcr.io/infinityabundance/weftfs:v0.6.0
docker pull ghcr.io/infinityabundance/weftfs-go:v0.6.0
```

`latest` is also published. FUSE mounts require `--device /dev/fuse --cap-add
SYS_ADMIN`; codec and store-only commands do not. See
[`docs/containers.md`](docs/containers.md).

## Rust crates

Published on crates.io:

```toml
[dependencies]
weftcodec = "0.6.0"
weftfs = "0.6.0"
```

```sh
cargo add weftcodec@0.6.0
cargo add weftfs@0.6.0
```

`weftcodec` is the lossless codec/framing library and CLI. `weftfs` is the
transactional store plus FUSE frontend (the `fuse` feature enables mounting).

### Experimental codec representation (opt-in)

The default `raw` policy writes the frozen v1 manifests. The experimental,
`codec-auto` is selected with `--representation codec-auto` in both implementations
and `--codec-min-bytes N` (default 4096); both are global flags and are also
accepted after `mount`, e.g. `weftfs --store DIR mount --mountpoint MNT
--representation codec-auto`. Read-only verbs accept and ignore them, and
`info --json` reports the active policy. `codec-auto` writes `weft.manifest.v2`,
which both the Rust and Go stores now parse, write, verify, and GC. The two
implementations read each other's v2 objects byte-exactly, but their zstd frame
bytes are not claimed identical. Defaults and fixtures are unchanged.

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
scripts/docker-court.sh fuse "WEFT_CONTEXT_MOUNTS=100 bash scripts/demo-context-mounts.sh"
scripts/docker-court.sh fuse "bash scripts/demo-ambiguity.sh"
scripts/docker-court.sh fuse "bash scripts/demo-time-travel.sh"
scripts/docker-court.sh fuse "WEFT_TIME_TRAVEL_VERSIONS=10000 bash scripts/demo-time-travel.sh"
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
  unchanged. The scaled mode (`WEFT_TIME_TRAVEL_VERSIONS=10000`) writes 10,000
  versions with a snapshot every 100, mounts ten evenly spaced snapshots
  (`s1`, `s1100` … `s10000`), and repeats the immutability check after a later
  write to the renamed live path.
- **Scaled context mounts** (`WEFT_CONTEXT_MOUNTS=100`) build 200 corpus
  objects, 25 models, 10 grounding docs, 599 typed relations and exactly 100
  distinct Context Roots, spawn all 100 FUSE daemons before awaiting any of
  them, then check every mounted listing digest, every one of 425 mounted
  files against its declared SHA-256, and a representative `weftfs cat` byte
  comparison per mount. The receipt records per-mount latency p50/p95/max,
  wall time to mounted-and-verified, daemon RSS, cgroup peak, and the number
  of surviving daemons after unmount.
- **Replay bundle** writes `bench/bundles/weft-receipt.tar.zst` from Rust and
  verifies it with Go only: extract, `verify --full`, resolve and materialize
  every Context Root, then re-hash every store entry and output file against
  `manifest.json`. The receipt records the bundle SHA-256 and the one-command
  reproduction.

A second run of either scale is compared with the first using
`scripts/showcase/democmp.go` by setting
`WEFT_DEMO_COMPARE_WITH=/work/bench/receipts/<first>.json`; the demo writes
`<second>.determinism.txt` (semantic fields identical, run ids and wall-clock
fields excluded). The scaled receipts also carry a top-level `label` stating
single-host court, synthetic fixtures, local store, and that this is not a
fleet claim.

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
# 100,000-iteration run (2026-11-04: 100,000/100,000, 19.2 it/s, 86 min)
scripts/docker-court.sh fuse "WEFT_CRASH_ITERATIONS=100000 bash bench/scripts/crash_torture.sh"

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
  100,000-iteration run completed in 5,196.2 s (19.2/s) after switching to a
  fresh three-generation store per iteration; the old-or-new/partial checks are
  unchanged. The 10,000-iteration receipt (507.9 s, 19.7/s) is retained.
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
