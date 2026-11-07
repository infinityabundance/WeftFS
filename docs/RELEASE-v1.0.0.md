# WeftFS v1.0.0 — remote object planes, fenced writers, independent S3 implementations, tensor restore pipelines, 3FS transports, Orbax, and Go parity

Research preview. v1.0.0 is the R2–R26 subphase release: the first release cut
after v0.9.0. It does **not** freeze a new wire format. Every frozen v1 artifact
from v0.9.0 is unchanged: the WFT1 frame, `weft.manifest.v1`/`v2`/`v3`/`v4`,
the selector grammar, codec payload modes 0–16, canonical WCB, store fixture
v1, and every conformance vector. **v1.0.0 covers codec modes 0–16.** Mode 17
(`FLOAT_TILED`) and mode 18 (`FLOAT_TILED_STREAMS`) are additive allocations
made *after* v1.0.0; v1.1.0 covers modes 0–17, and mode 18 is post-v1.1.0. The version bump is metadata plus one
corrected tool-version string (see “Version bump and byte identity”).

`1.0.0` here means “the first release whose program surface we are willing to
name 1.0”, not a claim of production readiness. Every measured loss and every
non-claim below is part of the release.

Everything was measured in the Docker courts with the hard limits in
`AGENTS.md` (memory == swap, pids, CPUs, bounded `/tmp`, `no-new-privileges`,
`cap-drop=ALL`). Nothing was built or tested on the host.

## Verification at the release tree

**Aggregate.** `bench/receipts/verify-20261107T021227Z.json` —
**30/30 courts `pass`, 0 failed, 0 skipped**, at:

| Repo | Verified revision |
|---|---|
| `WeftFS` | `7c10ad4feca5bed8c5a3f7207d188d27328ade2f` |
| `WeftCodec` | `d61ed769cab2978416e78441db7e8165163bf890` |
| `WeftCodec-Go` | `86ded1edf5ca8c62ffd61687a572e924044f0c70` |
| `WeftFS-Go` | `9fa39d3b9ddb860ff0d53c132b87b522427262b8` |

Courts: `rust_codec`, `rust_store`, `go_codec`, `rust_fuse`,
`go_store_and_fuse`; codec cross-encode/decode in both directions and the
byte comparison; `spec_drift`, `claims`, `conformance_manifest`,
`conformance_coverage`, `generated_types`; `buck2_cas`, `buck2_cas_store`;
`threefs`; `agent_memory`; `ceph_rgw` (Ceph RGW 19.2.0, S3 backend suite and
the R17 fenced-writer suite); `s3_court` (MinIO, same two suites); the four
context cross-language courts; the five authenticated-capability courts; and
`fuzz_diff` (1,000,000 differential inputs, identical classes, no panics).

The `v1.0.0` tags are cut on the receipt commit. `WeftFS` tag `v1.0.0` peels to
`4bfabd1`; its `WeftFS-Go` pin is `6f1749f`, which is one receipt-only commit
after the verified `9fa39d3` (no code differs). `WeftCodec` `d61ed76` and
`WeftCodec-Go` `86ded1e` are both the verified revisions and the tagged ones.
This release-note commit is later than the tag; only documentation
and the claims manifest changed after the tag.

The same tree was also verified **before** the version bump:
`bench/receipts/verify-20261107T014045Z.json`, 30/30, at WeftFS `0cfbea1`,
WeftCodec `57288e1`, WeftCodec-Go `86ded1e`, WeftFS-Go `39b58f2`.

**Aborted run, retained.** `bench/receipts/verify-20261107T020631Z.log` (no
receipt was written). The shared host volume `/mnt/1tb_kingston` reached 100 %
(0 B available) during the run; `tee` failed with `No space left on device`
while writing the `go_codec` log and `rust_store` was marked `FAIL`. After
deleting only gitignored, regenerable court scratch under `.docker-cache/`
(~42 GiB), the rerun above passed 30/30. No test, assertion, or court default
was changed to obtain the pass.

**Standalone courts not in the aggregate** (see the dedicated section below):
Azure/Azurite, Go S3/MinIO, 3FS `--bench`, and the Python court.

## Version bump and byte identity

Changed, and nothing else:

- `Cargo.toml`: `weftfs` `0.9.0 -> 1.0.0`; `weftcodec = "1.0.0"`.
- `WeftCodec/Cargo.toml`: `weftcodec` `0.9.0 -> 1.0.0`.
- `Cargo.lock` and `WeftCodec/Cargo.lock`: the `weftfs`/`weftcodec` package
  entries only (the third-party `untrusted 0.9.0` entry is untouched).
- `python/pyproject.toml`: `0.9.0 -> 1.0.0`.
- `python/weftcodec/__init__.py`: `__version__` `0.8.0 -> 1.0.0` (it had not
  been bumped with `pyproject.toml` in 0.9.0; now they agree).
- `WeftFS-Go/store/rebase.go`: `storeVersion` `0.8.0 -> 1.0.0`. This is the
  tool version recorded inside generated rebase provenance records; Rust uses
  `env!("CARGO_PKG_VERSION")`, so the two agree again. It changes the *value*
  of a version field in newly written provenance, not any format or parser.

No Go codec `--version` string exists; WeftCodec-Go needed no source change.

**Proof that no frozen wire or on-disk byte changed.** All three conformance
adapters were re-run at the bumped tree and reproduce the pre-bump result
exactly, against the same committed 326-vector corpus:

| Adapter | pass | fail | skip | manifest aggregate |
|---|---:|---:|---:|---|
| Rust (`conformance-rust-20261107T015750Z.json`) | 224 | 0 | 102 | `1fc710a88e610290e65810d6b0c53a687d02b7316cd7cc5626dd195c07a1dcc0` |
| Go (`conformance-go-20261107T015800Z.json`) | 224 | 0 | 102 | same |
| Python (`conformance-python-20261107T015820Z.json`) | 218 | 0 | 108 | same |

These counts are identical to R2/R3/R6 (224/0/102 and 218/0/108). The
aggregate’s four codec cross-language courts and `conformance_manifest` /
`conformance_coverage` also pass at the bumped tree, and no fixture, vector,
manifest, commit-record, superblock, or chunk byte was edited. Store fixture
heads cited in earlier releases are unchanged.

Honest caveat: a newly generated Go rebase provenance record now records tool
version `1.0.0` where it previously recorded the stale `0.8.0`. Provenance is
a versioned application record whose version field is intentionally the tool
version; the schema and every frozen storage format are unchanged.

## What changed, R2–R26

Each entry states what was measured and every loss or non-claim recorded in the
subphase. Full detail and raw receipts are in `docs/STATUS.md` and
`bench/RESULTS-*.md`.

### R2 — decoder throughput (no wire change)

- 12-bit primary Huffman table: 4.7–5.0x faster decode in Rust, 4.1–5.3x in
  Go, validated by a 10,000-case differential per language; Modes 9–15.
- FLOAT_TENSOR one-pass plane join: 3.1–5.7x (Rust), 4.8–6.5x (Go).
- Real Qwen2.5-0.5B: base read 13,775.8 -> 4,627.5 ms (2.98x); instruct
  26,935.7 -> 11,371.2 ms (2.37x); single tensor 111.4 -> 39.8 ms and
  221.9 -> 92.7 ms. Storage byte-identical at 1,276,128,956 B.
- Bounded parallel tensor reads: base 4,606 -> 2,206 ms and instruct
  10,967 -> 5,276 ms at 4 threads (2.09x/2.08x), exact bytes.
- Losses: mode 9 is still 6.0x zstd per MiB (4,683,726 vs ~780,000 ns/MiB);
  base read is 4.8x the zstd-strategy path; materialize is *not* a parallel
  win (0.43x, host writeback bound); the 272 MB embedding caps Amdahl.

### R3 — `SPARSE_DELTA` (mode 16, additive)

- A 19-word pythia checkpoint change: 7,434,959 -> **29,887 B**; step1→2
  28.0M -> 18.1M; the planner selects per tensor.
- Losses: step2→4 is 2.22x larger, step4→8 3.29x, the Qwen fine-tune 1.94x;
  `max_sparse_runs` refuses the densest tensors (`LimitExceeded`, recorded as
  refused and excluded from sums). Sparse coding is for genuinely sparse
  deltas only.

### R4 — seekable, parallel tile decode (no wire change)

- Forced tiled control: full restore 3,038.3 -> 1,062.1 ms at 4 threads
  (2.861x); whole embedding region 738.7 -> 203.0 ms (3.64x); a 1-row slice
  reads **730,133 B instead of 180,625,100 B** (247x fewer backend bytes).
- Auto-tiling changed zero representations and zero bytes: bytes-only pricing
  still selects mode 9 (2.174 % smaller). The forced-tiled control is
  +14,310,892 B and is labelled a control, not planner output.
- Losses: small regions do not parallelise; no latency-aware default; no Go
  parity for tile-decode options/auto-tile.

### R5 — entropy-coder court (experimental, promote nothing)

- rANS is 0.5–3.6 % smaller than shipped Huffman on BF16/dense/tile/KV planes
  but decodes 2.0x slower (254 vs 510 MiB/s) and 23.7x slower than zstd-19;
  sparse whole planes 74.4x slower. Tunstall loses at every k and scope
  (k=12 whole-plane ratio 0.99–1.03; k=8 cannot split).
- Recommendation and outcome: **promote nothing**. The codecs remain
  internal/experimental.

### R6 — independent Python reader (modes 9–16)

- 326 conformance vectors: 218 pass, 0 fail, 108 skip; no mode 0–16 vector
  skipped (the Python reader at that revision covered modes 0–16 only). 16,003 randomized frames identical to the Rust CLI; 1,600 mutants,
  1,581 expected reject classes all matched; 627/627 shard roots identical.
- Residual: Python still does not implement selectors, Context Roots,
  capability envelopes, checkpoint manifests, archives, or WEFT-TENSOR layout
  containers; TENSOR_TILED is full-decode only (no region reader).

### R7 — shared multi-mount daemon

- 100 views in **1 process**: summed RSS 1,963,056 -> 27,692 KiB (70.9x);
  cgroup peak 1.259 GiB -> 66.2 MiB; threads 692 -> 102; same 425 SHA-256
  checks and the same digest over all listing digests.
- 512 MiB cgroup: shared mounts 100/100 (peak 70.2 MiB, 0 OOM) where
  per-daemon mounts reached 51/100 with 49 OOM kills.
- Loss: time to first view 523 -> 3,964 ms. Read-only views only; one backend
  mutex; one failure domain; no pins; no 1,000-view run; no Go `mount-many`.

### R8 — single-flight and dependency-aware decode cache

- Qwen family, 226.6 MB working set, 4 rounds: 6,440.7 -> **1,140.6 ms
  (5.65x)**; backend reads 3.02 GB -> 107.5 MB; decodes 640 -> 26; 100 % hash
  match and `verify --full` ok.
- Losses: +233 MiB RSS for a 226 MB working set; a bound below the working set
  re-decodes; round 1 still races; p99 bimodal; per-process, not persisted;
  mode-15 whole-object reads bypass both layers; FUSE range reads get
  single-flight only.

### R9 — immutable write overlays (`weft.manifest.v4`) and compaction

- 500 x 4 KiB writes: local 917.3 -> 415.5 ms, 87.4 -> 36.7 MB written;
  MinIO 2,308.3 -> 1,702.9 ms, 89.5 -> 38.8 MB. The same content root
  (`c00367f0…`) in all four runs.
- Warm 4 KiB read 0.919 -> 0.030 ms local, 1.422 -> 0.414 ms S3. Compaction
  preserves the root; a snapshot stays byte-identical after compaction + GC.
- Losses: cold first read 0.920 -> 5.594 ms (depth-8 chain); depth grows per
  64 disjoint extents; `overlay_bytes` 32,112,640 for 2,048,000 logical bytes
  written because misaligned edits widen to the chunk grid; Go v4 writes are
  Rust-only in this subphase (Go read/verify/GC only; R20 closes the writer).

### R10 — blob/commit plane split

- Capabilities are **probed** on writable open, never merely declared: blob
  create-once/HEAD/range/delete and commit create-once/matching-CAS/stale-CAS.
  An endpoint accepting a stale token is refused `UnsupportedVersion`.
- All four layouts (local/local, s3/s3, s3-blobs/local-commits,
  local-blobs/s3-commits) produce root `7ebb1bdb…`, generation 145; crash
  proxy is old-whole or new-whole, never partial.
- Losses: Rust-only split at this point (R20 closes Go); single-writer commit
  plane; process-fault proxy only.

### R11 — physical object packs

- 10,000 x 1 KiB in one group commit: MinIO requests 20,003 -> **261**; wall
  7,321.5 -> **2,791.7 ms**; local 298.3 -> 185.4 ms.
- Losses: written bytes +13.8 %, footprint +19.2 % (catalog 2,272,401 B);
  cold reads amplify 3.19x (1,000 -> 1,252 requests); S3 warm p50 0.3437 vs
  0.1685–0.1729 ms; `verify --full` +42.6 %; packs are never rewritten;
  partial range reads cannot verify a member digest; no Go parity until R20.

### R12 — host-local calibration and rebase parity

- `weft.calibration.v1` with a strict loader (no fallback); 8/8 tests, 18
  rejection cases. Host calibration differs from the baked table by +2.0 % to
  +8.7 % by class (raw +5.4 %, zstd +4.4 %, weight-delta +5.7 %).
- 16 hot Qwen tensors: per-tensor decode 4,501.5 -> 1,900.8 ms (−57.8 %); full
  instruct restore 10,938.8 -> 8,430.9 ms (−22.9 %); base unchanged; 16/16
  applied, 0 prediction mismatches; second apply wrote 0.
- Losses: physical representation +43,575,360 B (+19.45 % on the selected
  set), store tree +277 M (old frames retained by snapshot/history), indexes
  +9.55 MB; break-even ~87.2 ms/read, so the fixed 200 ms threshold is more
  conservative than storage economics for a 2-read set. Go had no
  `--calibration` until R20.

### R13 — distributed correctness court

- **9/9 scenarios, 229 checks, 0 failed, 120 processes, 42 buckets** against
  real MinIO: single-writer interlock, three crash phases, idempotent retry,
  readers vs commits, stale reader, unreachable store, GC race, authorization
  by content hash, and the same faults over split planes.
- Found a real defect: transport failures and FORMAT read errors were
  classified `Corrupt`; error class `Io` (and, later, `Unauthorized`) is now
  preserved.
- Non-claims: single-node MinIO on loopback, no TCP partition beyond process
  stop, no physical media loss, writers exclusive by design.

### R14 — GCS backend

- JSON API with OAuth2 bearer tokens, generation preconditions
  (`ifGenerationMatch`), the same plane contract, split layouts, and a real
  pinned `fake-gcs-server v1.56.1` court: same content root, generation 21,
  commit sequence 1…21 as local, clean `verify --full`, stale-generation probe
  refused, crash proxy 3/3 old-or-new, 14/14 unit tests.
- Found and fixed two real defects (bucket `insert` body/URL, object listing
  URL); failing warrants retained.
- Losses/non-claims: not Google Cloud Storage (memory emulator on loopback);
  bearer token only, no refresh flow / JWT service account at this point
  (R21 adds RS256); no multi-writer; four-attempt bounded generation read;
  no retry on 429/5xx; Rust only.

### R15 — Orbax/TensorStore adapter

- Real `jax 0.12.0.1`, `jaxlib`, `orbax-checkpoint 0.12.9`,
  `tensorstore 0.1.86` installed in the Python court; 18 adapter tests;
  13/13 harness checks; 11/11 leaves exact.
- Measured losses (single process, CPU): total save **22.2x slower**
  (1,770.7 vs 79.6 ms), full restore **126.7x** (2,690.3 vs 21.2 ms), single
  leaf **72.5x** (380.4 vs 5.2 ms), physical bytes +0.44 % (189 vs 13 files);
  an extra full-size staging container. The smaller save *pause* is deferred
  work, not a throughput win.
- Non-claims: CPU only, no TPU/GPU; single host/process; not file-level
  TensorStore/OCDBT interoperability; no crash/power-loss court; no Go
  adapter; synthetic weights.

### R16 — independent S3 implementation (Ceph RGW 19.2.0)

- Ran Ceph RGW (mon+mgr+OSD+rgw) in one sidecar under the hard limits.
  Compatibility matrix **8/8** against both MinIO and RGW; `s3_backend` 6/6
  against RGW.
- Found a real interop defect: RGW compares `If-Match` literally against its
  unquoted ETag, so the RFC-correct quoted form is 412. The client now
  **measures** the accepted form at open, keeps it only when matching works,
  and still refuses a stale token.
- Environment facts recorded: the default 512-thread RGW pool plus
  `radosgw-admin` exceeded the 1024-pid bound, fixed with
  `--rgw-thread-pool-size 32` (the limit was **not** raised); the demo sets
  `rgw dns name`, so the court uses an IP-literal endpoint.
- Non-claims: single node/zone, no multisite, no TLS/presign/multipart/
  versioning/ACLs, no media or power-loss run, no performance claim.

### R17/R18 — fenced writer protocol (S3, then GCS)

- `LOCK` is a fenced record: owner, monotonic fence, sequence, absolute
  deadline, renewal stamp, pending head. The fence is a conditional write and
  the **last gate before the head CAS**; takeover is fence+1; release is a
  content-addressed tombstone, never an unconditional delete; a writer stops
  at its own deadline.
- Adversarial evidence: `s3_fencing` 8/8 on MinIO (15.23 s) and 8/8 on Ceph
  RGW (80.85 s), including a real `SIGSTOP` child (paused fence 2 ->
  successor 3, `Busy` after `SIGCONT`, head unchanged); local
  `writer_fencing` 4/4; distributed S10 26 checks.
- GCS uses `ifGenerationMatch` with shared encode/parse; the same invariant
  bodies run on MinIO, RGW, and `fake-gcs-server v1.56.1`.
- Non-claims: single writer; the fence and head are two conditional writes on
  two objects (one atomic fence+head is not expressible on plain S3/GCS); no
  clock-skew defence; a lost network reply keeps the usual at-least-once
  ambiguity; GCS backend is Rust-only.

### R19 — pipelined restore

- Ordered producer/consumer with one in-order writer and a 64 MiB in-flight
  bound. On tmpfs: 4,913.0 -> 2,157.3 ms at 4 threads (2.207x pipeline
  scaling); the write overlaps decode.
- Losses: on the host bind mount the same work ranged 2,526–15,325 ms (batch)
  and 2,976–17,519 ms (pipeline); the pipeline wins 1.177x in one base case
  and loses 1.35x in another within one court. **No disk performance claim.**
- Also removed implicit read-side store mutation: `checkpoint restore` imports
  a container only with `--import`; Python readers default
  `import_restored=False`.

### R20 — Go parity for R9–R12 storage surfaces

- Go v4 overlay writer + compaction (identical manifest `f04879eb…` and head
  `64103f3b…` for a fixed clock/seed); probed plane split (four layouts share
  head `6ce4fb88…`); pack plane (2 packs / 8 members / 366,270 B for a
  300,000 B file, Rust reads Go packs and vice versa); strict calibration
  loading (identical digest); 51 new tests.
- Residual: Go still lacks S3/GCS at that point (R25 adds S3), `calibrate`
  measurement (R24), pack rewriting, streaming overlay, and an on-disk
  selector index (R26).

### R21 — Azure Blob backend and GCS service-account auth

- Azure REST (SharedKey HMAC-SHA256, SAS, anonymous) behind the same planes;
  fenced writer ported; If-Match form measured. Azurite 3.35.0 court:
  local and Azure root `0a8198f5…`, generations 1…21, crash 3/3 old-or-new.
- GCS auth: RS256 JWT-bearer with a from-scratch strict DER/PKCS#1/Montgomery
  implementation; the RFC 7515 A.2 vector signature matches exactly.
- Non-claims: no live Azure or Google account; Azurite ≠ Azure; RS256 is not
  constant-time; SAS/anonymous request construction was tested before R23 but
  acceptance was unproven; no Go Azure/GCS backend.

### R22 — 3FS transports (POSIX and USRBIO)

- One normative layout, two transports: positional I/O over a 3FS mount, or
  the hf3fs user-space rings (`dlopen`, no link-time dependency; a missing
  symbol is refused by name). POSIX transport is **byte-identical** to local.
  1,500 requests become **50 submissions** with exact bytes.
- Measured loss: ring 11.159 ms vs positional 9.899 ms (1.127x slower); store
  round trip +8.5 %; same generation 101 and root. Named causes: the shim
  `pread`s per request, one descriptor registration per read, and owned
  vectors on the ring side.
- **No 3FS deployment was run**: this proves the adapter against a written
  ABI, not 3FS throughput, RDMA, kernel bypass, durability, or multi-node.

### R23 — Azure service-SAS acceptance

- A real container-scoped service SAS (`sv=2018-11-09`, HMAC-SHA256): the
  `sp=rcwdl` SAS is **accepted** for read-only store open, object reads,
  `verify(true)`, and create-once blob PUT/GET. 19 backend + 7 fencing tests,
  0 failures.
- Limitation: Azurite refuses `HEAD ?restype=container` under every SAS form,
  so a **writable** open over SAS fails `Unauthorized`; SharedKey is the
  writable path. SAS is effectively read-only. Account SAS untested.
- Fixed a real defect: an auth failure during `open_impl` was wrapped
  `Corrupt`; `Unauthorized` (and `Io`) are now preserved.
- Non-claims: Azurite is not Azure; anonymous acceptance unproven; no live
  account; tmpfs, loopback, warm caches.

### R24 — Go calibrate measurement and packed/split mount coverage

- Go measures all planner classes with shipped encoders; the Rust CLI accepts
  the document (schema `weft.calibration.v1`, 10 rows / 7 measured, identical
  calibration id `b07fbff6…`). Go `ns/MiB` (codec-level method): raw 819,494;
  zstd 1,115,176; float-tensor 2,837,430; tensor-sharded 3,475,654;
  tensor-tiled 4,828,817; weight-delta 6,003,877; semantic-delta 7,206,540.
  These are not comparable to Rust’s store-read method.
- Fixed a real defect: Go preferred the smallest dividing `q8_0` block (16)
  while Rust prefers 256, failing 8 MiB writes and diverging mode-10 bytes.
- FUSE coverage for packed, split, and packed-split stores: 9 files and 3
  directories per layout, byte-for-byte/SHA-256 reads, listing digests
  identical, `verify --full` three times. `go test -race` 0 failures.
- Non-claims: no Go `calibrate --install`; read-only mount coverage; Go has no
  store-side mode-15/14 writer.

### R25 — Go S3 backend

- SigV4 over `net/http` (stdlib), create-once `PUT`, whole/ranged `GET`,
  `If-Match` CAS, `ListObjectsV2`, the ported fenced writer, `--backend s3`
  and all four split layouts.
- MinIO court: focused **17 pass / 0 fail / 1 skip**; full
  `go test -race -count=1 ./...` all packages ok, 0 failures; cross-language
  **17/17** checks (Go store -> Rust `verify --full`, same head, byte-exact;
  Rust -> Go, same head, byte-exact; Go GC on a Rust store agreed by Rust).
- Measured form: MinIO accepts the quoted ETag as returned; the dequoted form
  is implemented but unexercised in the Go court.
- Non-claims: no Go GCS/Azure backend; no FUSE-over-S3 mount court; no
  AWS/multi-node/replication/media claim; the fence is still two conditional
  writes; no performance claim.

### R26 — Go selector-index persistence (WFX2)

- Go writes and loads the same `WFX2` segment Rust writes; a 10,000-object
  segment is 2,423,650 B. With the segment present, the first relation query
  after reopen is 7.749 / 8.042 ms versus 16.838 / 16.852 ms rebuilt
  (0.460 / 0.477); first tag query 16.583 / 16.884 vs 24.841 / 23.851 ms.
  Nine corruption modes each reject, rebuild, and return the scan-oracle
  answers; `go test -race` 0 failures.
- Losses: the segment is rewritten per commit, so cost is O(state) (~17 ms on
  top of the build at 10k objects here); non-narrowing selectors pay the load
  and are not measured; `RequireOne`/saved-context still scan; no
  cross-language proof for I64/Timestamp attribute keys (the Rust CLI cannot
  author them); default GC grace 3600 s; no CLI switch to disable writes.

## Standalone courts not in the aggregate

These are cited separately. None is part of `verify-20261107T021227Z.json`.

| Court | Receipt | Result |
|---|---|---|
| Azure / Azurite 3.35.0 service SAS | `bench/receipts/azure-20261107T015947Z.json` and `bench/receipts/azure-court-20261107T015947Z.json` | `azure_backend` **19 passed / 0 failed** (12.71 s), `azure_fencing` **7 passed / 0 failed** (15.01 s); end-to-end court `status: ok`, local/Azure digest `2e3b9902…`, generation 21, root `0a8198f5…` |
| Go S3 / MinIO `RELEASE.2025-04-22T22-12-26Z` | `WeftFS-Go/bench/receipts/go-s3-20261107T020137Z.json` (+ focused/full/xlang logs) | focused **17 pass / 0 fail / 1 skip**; full `go test -race` 0 failures (store 188.8 s); cross-language **17/17**; If-Match form `quoted-as-returned` |
| 3FS `--bench` (conformance shim clang-built, not a 3FS run) | `bench/receipts/threefs-20261107T020623Z.json` | 300 files x 256 KiB x 5 reps: scalar 8.849701 ms, ring 10.963013 ms (1.2388x slower), bytes identical; 1,500 requests / 50 submissions; store round trip same generation 101 and root `9dcdc31f…`; peak RSS 398,364 KiB |
| Python court (built wheel) | `bench/receipts/python-bindings-20261107T015918Z.json` (+ `.log`) | **349 tests, 340 passed, 0 failures, 0 errors, 9 skipped** (pytest status 0); Python 3.12.16 in `weft/python-court:1` |

## Artifacts

### Rust crates (crates.io)

Published with `scripts/publish-crates.sh` (rust court, weftcodec first):

| Crate | Version | Live checksum | Yanked |
|---|---|---|---|
| `weftcodec` | 1.0.0 | `ea436adb2dd807c49a419e541359c1f55b0c86c12097d6b7e9d5bd2b8117f464` | false |
| `weftfs` | 1.0.0 | `3552b5242c1bc1aa459e2ed4eabd3d999d39d5a7ff5c76379895609d6297bf88` | false |

Checksums read back from the crates.io API after publication.

### Python wheel

`python/dist/weftcodec-1.0.0-py3-none-any.whl`, **80,261 bytes**, sha256
`1e8088672616eae2f9cf6fd972c64c921aaa6fcfb024329e36bfe36442260aa2`. Built
and tested from the installed wheel in the Python court.

**Not published to PyPI: no token is available.** The wheel is a release
artifact only.

### Container images (GHCR)

The tag push builds one image per owning repository; the parent workflow
publishes **only** `weftfs` (the old four-image parent matrix is deliberately
not reintroduced). Digest and source-label verification are recorded here:

All four tag runs completed `success` (WeftFS run 42329273174). Each image
was pulled and its config label read:

| Image | Index digest (`@sha256`) | `org.opencontainers.image.source` | `org.opencontainers.image.revision` |
|---|---|---|---|
| `ghcr.io/infinityabundance/weftfs:v1.0.0` | `8089148c82fbace0777b0e559bc80d48bbad27e783a0ddd78030a56cd310d717` | `https://github.com/infinityabundance/WeftFS` | `4bfabd1e8472e646a5232bab6857a1ba80aa9d96` |
| `ghcr.io/infinityabundance/weftcodec:v1.0.0` | `19f507e993294f66be64955231f438aadf00138f74483144fdb75edb92cc6001` | `https://github.com/infinityabundance/WeftCodec` | `d61ed769cab2978416e78441db7e8165163bf890` |
| `ghcr.io/infinityabundance/weftcodec-go:v1.0.0` | `4e67b1b9e70f9e0e4087e566b317f3974f9f233e990d81caeba388d7d497206d` | `https://github.com/infinityabundance/WeftCodec-Go` | `86ded1edf5ca8c62ffd61687a572e924044f0c70` |
| `ghcr.io/infinityabundance/weftfs-go:v1.0.0` | `d576eef8febc4853a21ec7cd735778e255cee66c5ef7e3b4c1a12c912340067d` | `https://github.com/infinityabundance/WeftFS-Go` | `6f1749f34508b8cb0ee03cf426dc349fffe62916` |

Each label names the repository that owns the package, and each revision is the
tagged commit. Container builds are not bit-reproducible, so the digests above
are the observed release digests, not a reproducibility claim. FUSE mounts need
`--device /dev/fuse --cap-add SYS_ADMIN`; codec and store-only commands do not.

## Not claimed

This release does **not** claim any of the following:

- **Physical media power loss** or a physical power-loss court. Crash courts
  are process-fault proxies; the QEMU/block-layer courts use a labelled device
  model and `cache=unsafe` lying-fsync, not a pulled power cable.
- **Host page-cache loss.** The host’s `drop_caches` was not writable in the
  court; only per-file `fadvise(DONTNEED)` was possible.
- **RAPL / joule energy.** `energy_uj` was unreadable on host, guest, and court
  (Docker masks `/sys/devices/virtual/powercap`). CPU user/sys time is not
  energy and is not presented as such.
- **GPU, TPU, or SIMD decode paths.** All decode and restore measurements are
  scalar CPU work in a 4-vCPU court. Orbax/JAX ran with forced CPU devices.
- **Any fleet, multi-host, multi-node, or multi-writer claim.** Writers are
  exclusive by design; distributed courts are one host, one MinIO/Ceph/Azurite
  instance; sharing is in-process read-only views.
- **Constant-time RSA.** The GCS RS256 implementation is strict and
  load-time-proven but not constant-time.
- **Live Azure, GCS, or 3FS deployments.** Azure was Azurite; GCS was
  `fake-gcs-server`; 3FS was a written-ABI conformance shim with
  `hf3fs_is_hf3fs = 0`. No real service, RDMA, or kernel bypass was run.
- **Go GCS or Azure backends**, or Go `calibrate --install`.
- **Orbax competitiveness.** The adapter is **22–127x slower** than Orbax’s
  own OCDBT path in this court and +0.44 % larger; it is an interoperability
  and exactness result, not a performance result.
- **General-purpose-filesystem performance.** Published comparative results
  include large losses (filesystem metadata/sequence throughput, relation and
  tag selectors versus SQLite, multi-job fsync tails). No “faster than
  ext4/XFS/ZFS” claim is made.
- **PyPI publication.** No token; the wheel is not on PyPI.
- Production readiness, security audit, or a support commitment of any kind.

## Reproduce

```sh
bash scripts/docker-court.sh rust "cargo test --all-targets"
bash scripts/docker-court.sh fuse "cargo test --features fuse --all-targets -- --test-threads=1"
bash scripts/docker-court.sh go "cd WeftFS-Go && go test -race -count=1 ./..."
bash scripts/verify-all.sh                       # 30 courts, writes bench/receipts/verify-*.json
bash scripts/run-conformance.sh scripts/conformance-adapter-rust.sh
bash bench/scripts/azure_court.sh                # Azurite sidecar
cd WeftFS-Go && bash scripts/go-s3-court.sh      # MinIO sidecar
bash scripts/docker-court.sh rust "bash scripts/threefs_court.sh --bench"
bash scripts/docker-court.sh python "python/scripts/court-receipt.sh"
bash scripts/check-claims.sh
bash scripts/check-spec-drift.sh
```
