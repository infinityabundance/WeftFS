# WeftFS v0.8.0 — fast mount writes, tiled tensors, commit-time quotas, S3, PyTorch DCP, and signed bundles

Research preview. v0.8.0 is the Q1–Q6 subphase release: a mount write path with
Merkle identity, write-back, and group commit; independently decodable tensor
tiles; quota usage carried inside the commit transaction; a real S3-compatible
backend exercised against MinIO; a PyTorch Distributed Checkpoint adapter run
with real `torch`; and signed receipt bundles with store provenance linkage.

This is an **additive** release. Frozen v1 frame and store formats, the default
whole-file v1 content identity outside `mount`, the selector grammar, and the
default `raw` representation are unchanged. The new codec mode (15), the S3
backend, the Python adapter, and signed bundles are opt-in; `mount` alone now
defaults to Merkle identity.

Everything below is measured in the Docker courts with the hard limits in
`AGENTS.md`. No claim is a fleet, production, physical-media power-loss, RAPL,
GPU, multi-node, cloud, or hardware-independent claim. Every loss we know about
is stated.

## Version and artifacts

```toml
weftcodec = "0.8.0"
weftfs = "0.8.0"
```

```sh
docker pull ghcr.io/infinityabundance/weftcodec:v0.8.0
docker pull ghcr.io/infinityabundance/weftcodec-go:v0.8.0
docker pull ghcr.io/infinityabundance/weftfs:v0.8.0
docker pull ghcr.io/infinityabundance/weftfs-go:v0.8.0
```

- Rust: `weftfs` 0.8.0 and `weftcodec` 0.8.0 in both `Cargo.toml` and
  `Cargo.lock`; the parent depends on `weftcodec = "0.8.0"`. The bump changed
  version literals and both lock files only.
- Python: `python/dist/weftcodec-0.8.0-py3-none-any.whl`, **45,400 bytes**,
  sha256 `5a41c419a43e9d7ba16dd14c2d170ecd34d102f16c69d51652af5245e0fe91ab`.
  Built in the Python court and tested from the installed wheel: **191 tests,
  0 failures, 0 errors, 62 skipped**
  (`bench/receipts/python-bindings-20261106T012031Z.json`). The Python package
  now reports version 0.8.0. **Not published to PyPI** (no token); the wheel is
  attached to the GitHub release.
- Images are published by each repository's `container.yml` on the release tag.
  FUSE mounts need `--device /dev/fuse --cap-add SYS_ADMIN`; codec and
  store-only commands do not. See `docs/containers.md`.

## Verification at the release tree

`bench/receipts/verify-20261106T011444Z.json` — **27/27 courts `pass`**, run
from WeftFS `d6ba189` before the version bump:

- `rust_codec`, `rust_store`, `go_codec`, `rust_fuse`, `go_store_and_fuse`;
- codec cross-decode in both directions plus byte comparison;
- `spec_drift`, `generated_types`, `claims`, `conformance_manifest`,
  `conformance_coverage`;
- Buck2 CAS unit court and store-backed demo;
- agent memory, context cross-language gates, capability cross-language gates;
- `fuzz_diff` (1,000,000 differential inputs).

Revisions pinned by that receipt:

| Repo | Verified revision | Role |
|---|---|---|
| `WeftFS` | `d6ba189` | program home, Rust store + FUSE, specs, courts |
| `WeftCodec` | `3f3eae0` | Rust codec, AI modes, checkpoint/capability support |
| `WeftCodec-Go` | `65bc14d` | independent Go codec and capability parity |
| `WeftFS-Go` | `f920ae0` | independent Go store + FUSE, spec mirror |

The tag is cut after the version and documentation commits, so the tagged
commits are later than the verified code revisions; the only changes between
them are version literals and documentation. The host-only `claims` court is
re-run on the release tree after the v0.8.0 entries are added.

A first full run at the same tree failed `rust_store` in the documented
pre-existing `tests/quota.rs` writer-lock flake
(`usage_survives_reopen_and_still_denies`, `Busy` on reopen). It passed
standalone five times and in three full `cargo test --all-targets` runs; the
clean 27/27 receipt above is the release receipt, and the failing receipt
`bench/receipts/verify-20261106T010756Z.json` is retained. A Python-wheel run
`bench/receipts/python-bindings-20261106T011946Z.json` is also retained: the
shared release binary had been built with `--features fuse` by the FUSE court
and could not load `libfuse3.so.3` in the Python court. Rebuilding the plain
release binary produced the clean wheel receipt.

## Q1 — mount write path: Merkle identity, write-back, group commit

Receipt: `bench/receipts/fuse-writeback-20261105T202300Z.json` (FUSE court,
single host, synthetic deterministic fixtures, 500 × 4 KiB overwrites of a
128 MiB file, one `fsync` at the end, 20 × 10 metadata tree, one repetition).

- **Merkle is the mount default.** `mount` now defaults `--content-identity` to
  `merkle`; every non-mount command keeps the frozen `whole-file` default, and
  reads accept v1/v2/v3. `mount --representation codec-auto` requires an
  explicit `--content-identity whole-file`.
- **`--write-back` (off by default).** Per-handle bounded buffering (32 MiB per
  handle, early flush on overflow) published as one commit at
  `flush`/`fsync`/`release`. The writing handle reads its own buffered bytes;
  other readers see the committed version until the flush; `fsync` is durable
  before it replies.
- **`--group-commit` (off by default).** Independent mutations share one durable
  commit within a 10 ms window (max 256 ops), also published on `fsync` and
  unmount.
- **Go parity.** Same flags, same mount default, per-handle buffer and overlay
  read, and a lifecycle-safe background group publisher.

Measured 4 KiB overwrite p50 and metadata throughput:

| Measurement | Rust before | Rust after | Go before | Go after |
|---|---:|---:|---:|---:|
| 4 KiB overwrite p50, whole-file, no write-back | 120.0 ms | — | 422.9 ms | — |
| 4 KiB overwrite p50, `--write-back` (whole-file) | — | **7.46 µs** | — | **10.61 µs** |
| 4 KiB overwrite p50, merkle, no write-back | 27.3 ms | — | 33.6 ms | — |
| 4 KiB overwrite p50, merkle + `--write-back` | — | **8.83 µs** | — | **10.72 µs** |
| create ops/s, `--group-commit` | 162 | **26,548** | 310 | **6,264** |
| unlink ops/s, `--group-commit` | 162 | **39,126** | 311 | **9,610** |

**Losses and costs.**

- **Group commit worsens the content-write tail.** A request can wait behind a
  group publish: p99 reaches **611–804 ms** (Rust) and **432–478 ms** (Go),
  against 36–128 ms (Rust) and 42–477 ms (Go) without grouping.
- **Write-back loses unflushed ranges on a crash by contract.** Only
  `flush`/`fsync`/`release` publishes; a crash before then discards the buffered
  ranges. `fsync` is durable before it replies.
- **Group commit defers durability.** A group is durable only when published;
  a crash leaves the previous whole generation.
- Blob publication inside a group is still one durable write per touched chunk,
  which is why grouping helps metadata far more than content writes.
- Whole-file writes are **4.4× (Rust) to 12.6× (Go) slower** than Merkle at the
  same no-write-back settings.
- Both flags are opt-in; the default remains commit-per-mutation. The crash
  court is a process-fault proxy (SIGKILL of the daemon), not physical power
  loss and not a device-cache claim.

Crash proxy: **8/8 pass**. Rust and Go each kept the old bytes in the `old` and
`buffered` phases, published the new bytes in the `durable` phase, and produced
an all-or-nothing grouped metadata result (Rust 40/40 files, Go 0/40 — both
allowed states), with `verify --full` clean in every case.

Durability table (verbatim from the receipt):

| Mode | Guarantee |
|---|---|
| default | every mutation is a durably published commit before its reply |
| `--write-back` | buffered bytes are visible to the writing handle immediately; others see them only after flush/fsync/release; `fsync` is durable before it replies; a crash loses unflushed writes |
| `--group-commit` | a group is durable only when published (window, 256 ops, fsync, or unmount); a crash leaves the previous whole generation |

## Q2 — independently decodable tensor tiles (mode 15)

Receipt: `bench/receipts/tensor-tiles-20261105T222531Z.json` (FUSE court, one
real Qwen2.5-0.5B BF16 tensor `model.layers.0.mlp.down_proj.weight`, shape
`[896, 4864]`, tile `[32, 256]`, 10 reps per row, warm in-process store API).

- **Mode 15 `TENSOR_TILED`** (`weft.ai.tensor_tiled.v1`, WEFT-AI-CODEC §16).
  Mode 10 is unchanged and remains the default. A tensor is split into a
  rank-equal, exactly dividing tile grid; each tile stores its canonical
  row-major bytes as byte planes, each plane coded with
  `huffman-canonical-v1`. The whole-tensor `LogicalTensorRoot` is unchanged:
  tiling is a physical representation, not a new identity.
- **Region API in both codecs.** `read_region` decodes and verifies only
  intersecting tiles and reports `tiles_touched`; `read_region_with` lets the
  WeftFS store supply stored tile bytes from backend range reads.
- **Checkpoint integration.** `checkpoint write --tile-shape A,B,...` writes
  mode-15 shard objects. A non-dividing tile shape is rejected with "no silent
  padding". Restore detects the representation and reports decoded tiles as
  `units_touched`.

| Region | Mode 10 p50 | Mode 15 p50 | Speedup | Bytes read (10 → 15) | Units touched |
|---|---:|---:|---:|---:|---:|
| 100 rows | 130.13 ms | **12.05 ms** | **10.8×** | 7,213,077 → **934,413** (−87.0%) | 1 shard → 76 tiles |
| 1 row | 129.98 ms | **3.22 ms** | **40.4×** | 7,213,077 → **281,816** (−96.1%) | 1 shard → 19 tiles |

The restored 100-row slices are byte-identical between strategies. After a
tiled restore was mounted, the unmodified `safetensors` 0.8.0 + NumPy 2.4.6 +
ml_dtypes 0.6.0 loader read 1/1 tensor with SHA-256
`db708f9a857fe3b09bdd61078acbcb6d5d4b58987364cdcfe8339d7eefd572c6`, equal to
the source tensor (5/5 court checks pass).

**Limits.** Tiles are full-size only: no partial boundary tiles and no silent
padding. The descriptor grows with tile count. A prefix range read deliberately
skips the whole-frame trailer/content-hash check and relies on per-tile roots.
WeftFS-Go does not implement `weft.checkpoint.v1`, so there is **no Go mode-15
checkpoint writer**. The measurement is one host, one public tensor, and a warm
in-process store, not a fleet or remote-object-store claim.

## Q3 — quota accounting inside the commit transaction

Receipt: `bench/receipts/quota-branch-20261105T224337Z.json` (bench court,
1,000 single-object commits, 1 KiB distinct payload each).

- A quota-enabled commit appends exactly one `set_quota_usage` op as its last
  op, carrying cumulative admitted usage (sorted principals), or a standalone
  `reset:true` op. It changes no namespace state, so checkpoints are unchanged.
  The commit schema stays `weftfs.commit.v1`; old commits without a usage op
  replay unchanged.
- The store-root `quota` file is now **policy-only** (`WFQ2`); ordinary commits
  perform one commit publication and **no second durable quota write**.
- Replay recomputes admission totals and rejects a disagreeing assertion as
  `Corrupt`; a reset must be alone and empty. `verify --full` validates every
  assertion. `quota rebuild` and `quota reset` are explicit admin operations.
- **Legacy migration.** A legacy `WFQ1` record is imported once on writable
  open: usage is recomputed from the chain, the exact legacy bytes are kept as
  `quota.migrated`, `quota` becomes `WFQ2`, and a corrective usage op is
  published when needed. Conflicting records fail closed as `Corrupt`.

| Metric | Without quota | With quota | Delta |
|---|---:|---:|---:|
| per-commit p50 | 11.278 ms | **11.344 ms** | **+0.066 ms (+0.59%)** |
| per-commit p95 | 14.928 ms | 14.848 ms | −0.080 ms |
| wall, 1,000 commits | 11.661 s | 11.736 s | +0.075 s |
| store bytes | 1,926,625 | 2,028,648 | +102,023 B |

The historical P5a design (separate durable quota record after every commit)
was 13.754 ms p50, **+21.2%** over no quota. Q3 is **−17.5%** against that
historical design. Usage after the run is 1,000 objects / 1,383,000 bytes; full
verify is clean. Cross-language tests read, verify, and enforce Rust-written
usage ops from Go and the reverse.

**Limits.** Trusting the head assertion on open is O(1); full re-derivation
happens in `verify --full`, `quota rebuild`, and any open that must replay the
chain. The policy file is still the authority: deleting it disables
enforcement. Usage is an admission counter, not a reclaimable footprint; deletes
do not return budget. One host, synthetic fixtures, one repetition.

## Q4 — real S3-compatible backend and MinIO court

Receipts: `bench/receipts/s3-backend-20261105T233154Z.json` and
`bench/receipts/s3-court-20261105T233154Z.json`.

- **Backend.** `S3Backend` implements `StorageBackend` over SigV4 with a minimal
  blocking client (`ureq`, rustls). The local logical key layout is used
  verbatim. Blobs use `PUT` + `If-None-Match: *`; range reads use HTTP
  `Range`; the two-slot superblock uses whole-object `GET` + `PUT` with
  `If-Match: <ETag>`. A lost conditional update retries from a fresh read and
  then fails `Busy`; it never overwrites the head blindly. Writable open probes
  both conditional forms and refuses (`UnsupportedVersion`) if the endpoint
  ignores either.
- **Lock.** The writer lock is a `LOCK` object with an owner and expiry lease,
  created with `If-None-Match: *`; a crashed writer's lease is reclaimed with
  `If-Match`. This is advisory: it is not POSIX `flock` and not a multi-host
  protocol.
- **Court.** The official `minio/minio` Docker Hub repository was not pullable
  on 2026-11-05 (registry 404/unauthorized). The court runs the official
  linux-amd64 MinIO release binary `RELEASE.2025-04-22T22-12-26Z`
  (sha256 `53e2a2cb…bb817c`) in a minimal `debian@sha256:734a72b3…b179bc`
  container under the same hard limits (3 GiB, swap 3 GiB, 1,024 pids, 4 CPUs,
  bounded tmpfs, `no-new-privileges`, `cap-drop=ALL`).

Same seeded operations, same head `8e47145e1a09c3bf…`, generation 1101:

| Operation | Local | S3 (MinIO) |
|---|---:|---:|
| 1,000 creates | 105.9 ms / **9,442 ops/s** | 1,809.7 ms / **552.6 ops/s** |
| create p50 / p95 | 100 / 154 µs | 1,781 / 1,967 µs |
| 100 MiB in 100 files | 192.0 ms / **520.9 MiB/s** | 1,168.3 ms / **85.6 MiB/s** |
| write p50 | 664 µs | 10,300 µs |
| 1,000 reads | 4.4 ms / 224,799 ops/s | 345.8 ms / 2,892 ops/s |
| read p50 | 4 µs | 334 µs |
| backend bytes written | 111,581,685 | 116,091,480 (**+4.0%**) |

`verify --full` is clean on both. Artifacts are byte-identical between backends
(6/6 tests with the S3 environment; without it each test prints
`SKIP s3_backend: WEFT_S3_TEST_ENDPOINT is not set` and passes).

**Limits.** Rust only. MinIO is one node on localhost with a tmpfs data
directory: this is protocol and conditional-write safety, not media durability
or power loss. The writer lease is advisory (default 900 s; court 2 s); a paused
writer longer than the lease can lose it. **No AWS, GCS, Azure, Ceph, 3FS,
cloud, multi-node, or fleet claim.**

## Q5 — PyTorch Distributed Checkpoint adapter

Receipts: `bench/receipts/pytorch-dcp-20261106T000734Z.json` and
`bench/receipts/python-bindings-20261106T012031Z.json`. Docs:
`docs/PYTORCH-DCP.md`.

Real PyTorch ran in the court: **torch 2.15.0+cpu** (official CPU wheel,
209.5 MB as downloaded). No CUDA runtime was present and no GPU claim is made.
`torch.distributed.checkpoint.save`/`load` ran through
`WeftStorageWriter`/`WeftStorageReader`; no stock DCP storage plugin was used.

- Every durable transfer is a subprocess call to the shipped Rust `weftfs` CLI.
  The checkpoint manifest (`weft.checkpoint.v1`) is the byte authority; DCP
  protocol metadata (`weft.dcp.metadata.v1`) is cross-checked against it.
- Topology matrix: 3 tensors × 3 restore topologies (8/4/1) = **9/9
  topology-identical restores**, byte-identical payloads with the same content
  root; a non-divisible split is rejected rather than padded.
- Bench (CPU, one process, synthetic 8 × 4 MiB f32, `topology=8`, `zstd`
  shards, 64 MiB staging):

| Phase | Median |
|---|---:|
| Full save | **1,223.6 ms** (1,038.7–1,229.5) |
| Full load | **152.2 ms** (148.3–156.7) |
| Single-tensor load | **67.2 ms** (64.3–70.0) |
| Async save submit | **0.72 ms** |
| Async save completion | 1,329.0 ms |
| Synchronous save | 1,316.5 ms |
| `torch.save` baseline file | 33,557,979 B |
| WeftFS physical after first save | 31,187,325 B (**0.9294×**) |

Peak RSS 381,424 KiB. Rust `checkpoint verify` verified 8 tensors and
`verify --full` was clean.

**Losses.** Save is **141× slower** and load **30× slower** than one
`torch.save`/`torch.load` file on this fixture, even while writing 7.1% fewer
physical bytes. There is **no GPU path** and no multi-rank cluster run; tensors
are copied through host memory. All tests use `no_dist=True` in a single
process. `BYTE_IO` objects under a split topology require divisible lengths
(reject, never pad). Reads import restored containers into the store; they are
not read-only. No media power-loss or host-cache durability claim.

## Q6 — signed, verifiable receipt bundles

Receipt: `bench/receipts/signed-bundles-20261106T010118Z.json`. Implementation
notes: `verification/README.md`.

- **Canonical manifest** `weft.receipt-bundle.manifest.v1`: `generated_at`,
  `stamp`, `files` (`path`, `size`, `sha256`), and `revisions`. Entries are
  strictly sorted by canonical encoded bytes; duplicate, unsorted, absolute,
  traversing, backslash, or control-byte paths are rejected. Decode re-encodes
  and requires byte equality, so a non-canonical manifest is refused before any
  signature check.
- **Signing.** `scripts/package-receipts.sh --sign-key FILE` signs
  `H("weft/receipt-bundle/v1", manifest_canonical_bytes)` with Ed25519 and
  writes `signature` and `public_key` next to `receipt-manifest.wcb`. It reuses
  `weftcodec::capability::sign_bytes` / `verify_bytes`. The seed is streamed to
  the court on stdin and is never copied into the repository, the Docker cache,
  or the archive.
- **Verification.** `scripts/verify-receipts.sh BUNDLE --public-key <64-hex>`
  (or `--signature-from-bundle`) re-hashes every listed file, rejects unlisted
  payload files, decodes the canonical manifest, then verifies the signature.
  It prints `SIGNATURE OK ed25519:<fingerprint>` or fails closed with a precise
  reason.

Court result: **15/15 acceptance/rejection cases pass**, including wrong key,
tampered same-size file, tampered manifest, tampered signature, missing
signature, unlisted file, non-canonical manifest, unsigned-with-key, and two Go
cross-checks (`VERIFY OK`, `VERIFY FAIL`). The Go verifier agreed with the Rust
signature over the real manifest digest.

Bench bundle: 1,064 payload files, 1,139 tar entries. The canonical manifest is
**128,731 bytes**; raw metadata added (manifest + signature + public key −
`UNSIGNED.txt`) is **128,633 bytes**. Signed package 19,648 ms; unsigned
12,091 ms. Pinned-key verify 9,029 ms; unsigned verify 8,981 ms. Signature 64
raw bytes; public key 65 bytes.

**Non-claims.** No certificate authority, no transparency log, no key
distribution or rotation, and no claim that the signer is trustworthy. Signing
proves bundle integrity relative to a key, not the truth of the measurements.
A leaked seed permits forged bundles under that key. Verification is offline.

## Retained receipts and known flakes

- `bench/receipts/verify-20261106T010756Z.json` — first full run at the release
  tree; `rust_store` failed on the pre-existing `tests/quota.rs` writer-lock
  flake. Retained.
- `bench/receipts/verify-20261106T011444Z.json` — clean **27/27** release
  receipt.
- `bench/receipts/python-bindings-20261106T011946Z.json` — retained
  environmental failure (fuse-linked release binary in the Python court).
- `bench/receipts/python-bindings-20261106T012031Z.json` — clean wheel receipt.
- Pre-existing flakes named in `docs/STATUS.md` and still not fixed:
  `tests/quota.rs` writer lock (transient `Busy` on reopen) and
  `tests/representation.rs` shared atomic chunk-counter race. Each passed
  standalone and in reruns. The two `#[ignore]`d FUSE tests remain: ownership
  needs `CAP_SETUID`, and kernel 7.3 same-inode direct-I/O hits a reproducible
  EIO.

## Still not claimed

Physical media power loss, host page-cache loss, RAPL energy, GPU/SIMD paths,
runs against 3FS/GCS/Azure/Ceph, PyPI publication, a multi-writer or
distributed store, Go parity for checkpoints/training-ready/mode-15 writing,
lineage-aware rebasing, calibrated planner costs, and any fleet claim.

Prior release: [`docs/RELEASE-v0.7.0.md`](RELEASE-v0.7.0.md) (real weights,
checkpoint topology, quotas, 1,000 mounts, Merkle identity, CAS, conformance).
