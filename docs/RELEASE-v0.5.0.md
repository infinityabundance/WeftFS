# WeftFS v0.5.0 — tensor-native storage, AI codecs, backends, provenance

Research preview. v0.5.0 is the first release in which typed tensors are a
first-class physical representation, AI codec modes 9–14 are implemented in both
Rust and Go, storage is abstracted behind a capability-checked backend, and
provenance records drive a deterministic recovery planner. The frozen v1
on-disk formats and the default `raw` representation remain byte-identical.

Everything below is measured in the Docker courts with the hard limits in
`AGENTS.md`. No claim is a fleet, production, or hardware-independent claim.
Every loss we know about is listed with the feature that produced it.

## Version and artifacts

```toml
weftcodec = "0.5.0"
weftfs = "0.5.0"
```

```sh
docker pull ghcr.io/infinityabundance/weftcodec:v0.5.0
docker pull ghcr.io/infinityabundance/weftcodec-go:v0.5.0
docker pull ghcr.io/infinityabundance/weftfs:v0.5.0
docker pull ghcr.io/infinityabundance/weftfs-go:v0.5.0
```

- Rust: `weftfs` 0.5.0 and `weftcodec` 0.5.0 (both `Cargo.toml` and
  `Cargo.lock`; the parent depends on `weftcodec = "0.5.0"`).
- Python: `python/dist/weftcodec-0.5.0-py3-none-any.whl`, 27,070 bytes,
  sha256 `cc0afe3b9b68490c5ad29a9fe54b830bf6266fd704e9ad962a674cdb4a2781f1`.
  **Not published to PyPI** (no API token); the wheel is a release artifact.
- Images are published by each repository's `container.yml` on the release tag.
  FUSE mounts need `--device /dev/fuse --cap-add SYS_ADMIN`; codec and store-only
  commands do not. See `docs/containers.md`.

## WeftTensor v1 — identity independent of layout

`spec/WEFT-TENSOR.md` defines

```text
LogicalTensorRoot = H("weft/tensor/logical/v1",
                      WCB({schema, dtype, shape, element_count, values_hash}))
values_hash       = H("weft/tensor/values/v1", row-major element bytes)
```

The root therefore does not depend on how the bytes are physically arranged.
Canonical `weft.tensor.layout.v1` descriptors exist for `row-major`,
`column-major`, `blocked`, and `sharded`; rank, zero dimensions, overflow,
divisibility, and field-presence rules fail closed. `weft.tensor.shard.v1`
containers accept shuffled input and order by `index`; duplicate, missing,
out-of-range, and inconsistent shards are rejected. Rust and Go agree.

Evidence (`bench/receipts/wefttensor-20261104T205020Z.json`, synthetic, in-memory,
single host, 64 MiB bf16, shape 128×512×512, 512 blocks of 16×64×64):

| Arrangement | Root median | Physical work |
|---|---:|---|
| row-major | 6.18 ms | 67,108,864 B hashed |
| blocked | 10.95 ms | 512 blocks touched |
| 8 shards | 16.63 ms | 8 shards touched |

All four arrangements produce the same root
`410800d9c574b05eb73daec70f18ea82af3f221ecca5047a18be6a00366308e3`; the
column-major case is covered by conformance vector `202_layout_column_major`.
A 1 MiB region read is 0.026 ms row-major and 0.031 ms sharded, but the sharded
read touches one 8 MiB shard to return 1 MiB.

Losses and limits:

- The root is a hash over the logical values, so computing it costs O(tensor
  bytes). Blocked and sharded arrangements cost more than row-major.
- A sharded region read over-fetches to the containing shard (8 MiB for 1 MiB
  in this fixture). There is no sub-shard paging.
- The layout layer has no FUSE, store, or object-store binding; this is not a
  topology-independent restore receipt.
- Partially resident tensors, irregular block lists, and padded blocks are not
  implemented.

Vectors: 3 valid logical, 10 logical rejects, 4 valid layouts, 10 layout
rejects, and 2 shard sets, byte-identical in both codec repositories. Cross
gate: `WeftCodec-Go/hack/tensor-cross.sh` (roots and containers identical; each
language reassembles the other's shards).

## AI codec modes 9–14

Mode 9 `FLOAT_TENSOR` now encodes its exponent plane with
`huffman-canonical-v1` instead of zstd, so its frames are **byte-identical**
Rust↔Go. The sign+mantissa plane stays raw. NaN payloads, infinities, signed
zeros, and denormals round-trip bit-exactly. 3 valid + 8 reject vectors;
`cargo test --all-targets` 133 tests and `go test ./...` pass;
`WeftCodec-Go/hack/cross.sh compare` reports `PASS identical float_tensor`,
`float_tensor_single`, and `float_tensor_uniform` (also part of
`verify-20261105T030019Z.json`).

| Mode | Name | Exact mechanism | Evidence |
|---:|---|---|---|
| 10 | `TENSOR_SHARDED` | `q8_0`/`q4_0`, block sizes 16–256, shard axis/count/index, global scale plane, exact zero points, `huffman-canonical-v1` payload | 3 valid + 8 reject vectors; byte-identical `sharded_q8` (2 shards), `sharded_q4` (4), single-shard |
| 11 | `WEIGHT_DELTA` | Bit-exact modular subtraction on unsigned words (`bf16`, `fp16`, `fp32`, `q8_0`, `q4_0`), one `huffman-canonical-v1` stream per byte plane, base frame is a hard dependency | 4 MiB bf16 pair: 533,110 B vs 4,194,410 B zstd-3 (7.87×), 3,649,949 B zstd-19; encode 12.86 ms, decode 16.99 ms |
| 12 | `KV_CACHE` | One frame per page; exact descriptor; two/four Huffman byte planes; pages have no dependencies or cross-page state | 16 MiB KV, 16 pages: encode 3.18 ms/page, whole-set decode 129.48 ms, single page 8.88 ms |
| 13 | `ACTIVATION_STREAM` | One frame per chunk; `digest_root` per chunk; assembly orders by index, verifies every digest, returns a stream root | 64 MiB, 64 chunks: encode 2.99 ms/chunk, whole-set decode 580.90 ms, assembly 612.63 ms, single chunk 9.01 ms |
| 14 | `SEMANTIC_DELTA` | Exact delta against a base whose semantic root is recomputed and checked; policies `none`, `mantissa_truncate:N`, `block_quantize:B:L`; **no lossy decode path exists** | 8 MiB bf16 pair: 1,050,660 B vs 6,127,332 B zstd-3 (5.83×), 4,976,379 B zstd-19; encode 31.93 ms, decode 44.22 ms |

Mode 11 detail (`bench/receipts/weight-delta-20261104T214337Z.json`): the two
planes are 265,022 + 267,643 bytes from 2,097,152 raw bytes each; the decoded
words and logical root `279bc0801a8f3a012afd97f2e002fb48d0f998636211246937b83032b6ea1207`
match the original. The verified ratio on this pair is 7.87× versus independent
zstd-3. A 5.81× figure was requested for this mode; no receipt in the repository
contains it, so it is not claimed.

Mode 12/13 detail (`bench/receipts/kv-activation-20261104T220008Z.json`): the
KV frame is 16,791,311 bytes and the activation frame 67,164,992 bytes for
their ~16 MiB / ~64 MiB payloads. Assembly is intentionally more expensive
than decoding because it re-verifies every chunk digest and concatenates the
stream.

Mode 14 detail (`bench/receipts/semantic-delta-20261104T222029Z.json`): 4,195
of 4,194,304 bf16 elements changed (0.10002%). Semantic-root medians are
0.68 ms (`none`), 8.95 ms (`mantissa_truncate:3`), and 15.91 ms
(`block_quantize:128:256`). Selection over 32 same-root candidates takes
983.21 ms and chooses index 0; all candidates share the root, and
`exact`/`logical_root_match` are true.

Losses and limits:

- Mode 10 duplicates the global scale plane in every shard (`4 * block_count`
  bytes per shard). A shard-local block partition is future work. `block_count`
  is also bounded by `max_structured_items` (1,000,000), which is stricter than
  `max_tensor_elements` for very large tensors. Dequantization is out of band
  and not part of identity.
- Mode 11 cannot decode independently: it needs the base frame through the
  resolver, and a missing/mismatched base fails closed. Its compression is
  measured on a synthetic 4 MiB pair, not production data.
- Modes 12/13 deliberately have no cross-page or cross-chunk state. Huffman
  planes are not seekable; a range read decodes one whole page/chunk. The store
  currently reads modes 12/13 but does not write them as object
  representations.
- Mode 14 selection decodes every candidate (983 ms for 32), has only three
  numeric policies, and has no remote semantic index. A semantic root is an
  index hint, not an authorization or identity substitute. There is no lossy
  mode: `no_lossy_mode` is a receipt field and no approximate decoder exists.

## Tensor representations and the cost-aware planner (Rust + Go)

Additive v2 manifests accept `kind:"tensor"` with `frame`, `tensor_mode`
(10/11/12/13/14), `dtype`, `shape`, `logical_root`, canonical `layout`,
`dependencies` (11/14), `decoded_size`, and `strategy`. Raw and codec v2 bytes
are unchanged. The store writes one object per tensor and one frame; multi-shard
declarations are rejected rather than truncated. Reads verify frame/mode/
strategy, dtype/shape, dependency closure, the recomputed logical root, and the
frozen content hash. GC retains delta bases; missing dependencies fail closed.

The planner minimizes `physical_bytes + lambda_ns * estimated_decode_ns`
(default `lambda_ns = 0`), with deterministic candidate order and tie-break.
Candidates are raw, zstd, mode 10 (`q8_0`), mode 11 (causal parent), and mode
14 (bounded same-semantic-root set).

Receipt `bench/receipts/tensor-planner-20261104T224929Z.json`: 10 checkpoints ×
8 tensors × 1 MiB bf16, 1% change per step, bench court.

| Policy | Physical data | Ingest | Restore 1/2/4 workers |
|---|---:|---:|---:|
| zstd | 15,917,707 B | 8.22 s | 93 / 48 / 26 ms |
| bytes (`λ=0`) | 11,187,086 B (−29.7%) | 24.37 s | 1,789 / 906 / 467 ms |
| latency (`λ=100,000`) | 83,985,344 B | 7.77 s | 37 / 22 / 15 ms |

Peak RSS was 51,680 / 52,164 / 52,148 KiB; all three runs verify clean.
Rust tests: `tests/tensor_store.rs` 9/9; `cargo test --all-targets` 112 passed;
FUSE `--features fuse --all-targets` 140 passed / 2 ignored.

Go parity uses receipt `bench/receipts/go-tensor-20261104T231018Z.json`: the
planner chooses `weight-delta` at 1,054,778 B versus 5,414,446 B independent
(19.48% of independent), encode median 139.8 ms, decode 73.3 ms, region read
74.0 ms (the whole frame is decoded), six representations verified. Handoff
tests run Rust→Go and Go→Rust, retain a Rust-written delta base through Go GC,
fail closed on a missing dependency, and enforce the depth bound
(`WeftFS-Go/store/tensor_handoff_test.go`).

Losses and limits:

- The bytes planner saves 29.7% physical bytes at ~3× ingest and ~19× restore
  cost; delta chains are decoded per read. The latency planner is fastest to
  restore at 5.3× the physical bytes.
- Planner decode estimates are a documented integer model, not calibrated to a
  device or to measured decode times.
- Region reads of delta/Huffman frames decode the whole frame.
- The Rust store has no `q4_0` representation yet; the Go store restricts mode
  10 to a single whole-tensor `q8_0` shard. Modes 12/13 are read-only in the
  store. Multi-shard objects are not written.
- The planner decision is not persisted as non-canonical manifest fields; only
  `plan --json` exposes candidate prices.

## Storage backends: Local and Object

All durable store I/O now goes through `StorageBackend`. The normative Local
layout and commit protocol are unchanged. The Object backend maps keys to
`objects/<2hex>/<name>` and publishes with tmp file + `fsync` + `link(2)` +
unlink(tmp) + directory `fsync`, so it does not depend on POSIX rename
replacement. A caller may declare `BackendKind::Posix { capabilities }`; the
store fails closed if `durable_fsync` is absent on a writable open or if
`atomic_publish` is absent on any publication. No degraded mode exists.

Receipt `bench/receipts/backend-20261104T232908Z.json` (1,000 creates + 100 MiB,
bench court):

| Backend | Create | 100 MiB write | Publications | Physical bytes |
|---|---:|---:|---:|---:|
| Local | 92.8 ms | 176.97 ms | 3,612 `rename(2)` | 106,350,774 |
| Object | 95.11 ms | 176.59 ms | 3,612 `link(2)`, 0 rename fallbacks | 106,350,774 |

Both produce the same head
`61f1a9f3b3b28ce7912443c7d17eb1f614320998b2508d93e78e03c5df011086`, the same
generation, and verify clean. `tests/backend.rs` is 4/4, including capability
refusals.

Losses and limits:

- The Object backend is still a single-host POSIX namespace; it is not an S3 or
  distributed object store. There is no conditional put, multipart upload, or
  network protocol.
- A POSIX-namespace target such as 3FS is a declared capability configuration.
  **No 3FS instance was run or exercised**, and no 3FS or distributed claim is
  made.
- The receipt has no syscall counter (none is available in the bench court); it
  reports wall time and backend byte counters. No fsync, device-cache, or
  power-loss claim follows.

## Go parity

- **v2 representation manifests.** The Go store parses, writes, verifies, and
  GCs `weft.manifest.v2`, with the same depth-8/cycle/chunk bounds and
  fail-closed dependency handling. Rust writes; Go verifies and reads
  byte-exactly, and the reverse. Zstd frame bytes are **not** claimed identical;
  the claim is mutual read/verify.
- **`kind:"tensor"`.** Go implements modes 10/11/12/13/14 manifests, the
  planner, GC retention, and missing-dependency failure. See the Go tensor
  receipt above and `WeftFS-Go/store/tensor_handoff_test.go`.
- **Bounded open.** WeftFS-Go `store/recovery.go` uses generation-addressed
  replay from the selected head to the newest verified checkpoint, with a
  mandatory note and authoritative full-scan fallback. At 100,000 generations
  (interval 256, tmpfs, synthetic,
  `bench/receipts/go-fast-open-20261104T190732Z.json`), bounded open is
  **2.80 ms median / 162 commit reads** versus **2,155.65 ms / 100,001 reads**
  on the forced full scan, with identical recovered state.

Losses and limits:

- Go still has no persisted selector-index segments or bounded commit index.
- Go mode 10 is restricted to one `q8_0` shard; `q4_0` storage is absent.
- Go `PUNCH_HOLE`/`ZERO_RANGE` are content-only on dense v1; physical space is
  not reclaimed. Locks are per-daemon advisory.

## Provenance receipts and the recovery planner

`weft.provenance.v1` records `operation`, bounded `params`,
`inputs[{role,content,object}]`, sorted unique `outputs`,
`environment{tool,version,env_hash,...}`, `determinism`
(`exact|declared|unknown`), `principal`, and `timestamp_ns`.
`ProvenanceId = H("weft/provenance/v1", WCB(record))`. The record is stored as a
content-addressed manifest-namespace blob, and `record_provenance` rejects a
declared output root that differs from the supplied object's actual content
root before publishing `predicate="provenance"` relations in one commit. GC
marks a receipt, its inputs, and its outputs while an output is retained.
`verify --full` checks the blob hash/canonical encoding, subject existence, and
that the receipt still names the subject's current content root.

The planner is pure and deterministic. Eligibility is `available` for
fetch/decode. **Recompute additionally requires `determinism == "exact"` and
every input verified.** Cost is the saturating sum of declared components; ties
break on `(cost, fetch < decode < recompute, name)`. With no eligible candidate
it returns typed `NoEligibleStrategy` and never falls back.

Receipt `bench/receipts/provenance-20261104T235013Z.json` (10 operations × 10
artefacts × 1 MiB, synthetic, warm cache):

| Plan | Mix | Wall | Physical bytes read |
|---|---|---:|---:|
| Injected costs | 72 fetch / 18 decode / 10 recompute | 68.648 ms | 159,629,377 |
| Measured costs | 72 fetch / 18 decode / 10 recompute | 68.823 ms | 159,629,377 |
| Forced recompute | 100 recompute | 311.583 ms | 756,138,570 |

`verify --full` is clean with 90 checked provenance relations. Tests:
`tests/provenance.rs` 19/19; Rust `cargo test --all-targets` 135 passed; FUSE
163 passed / 2 ignored.

Losses and limits:

- No Go `weft.provenance.v1` parity.
- Store-side fetch/decode availability is determined by representation, so the
  measured costs do not change the chosen mix.
- A rewrite after recording makes the receipt stale; `verify --full` reports it.
- Closure follows an explicit input object or a unique content-root match;
  otherwise the input is an external leaf.
- Synthetic, warm-cache, single-host measurement.

## Python reader 0.5.0 (independent, not on PyPI)

`python/` is a third independent implementation with no Rust or Go extension
modules. Receipt `bench/receipts/python-bindings-20261105T000147Z.json`:

- 111 frame vectors discovered; **61 asserted, 61 passed, 0 failed**. The 50
  skips are explicit: 7 historical `superseded_by` vectors and 43 frames in
  unimplemented modes 9–14 (13 valid, 30 reject). Modes 9–14 raise typed
  `UnsupportedMode`; there is no approximate decode and no opaque-body fallback.
- **11/11 Rust-CLI cross-checks** pass: Rust encodes RAW, ZSTD, chunked
  RAW/ZSTD, REFERENCE, DELTA, DICTIONARY, TENSOR, TOKENS, QUANTIZED_TENSOR, and
  STRUCTURED; Python verifies and decodes each byte-exactly.
- Full suite: 166 tests, 115 passed, 51 skipped, 0 failed.
- Wheel: `python/dist/weftcodec-0.5.0-py3-none-any.whl`, 27,070 bytes, sha256
  `cc0afe3b9b68490c5ad29a9fe54b830bf6266fd704e9ad962a674cdb4a2781f1`,
  built in `weft/python-court:1`. The wheel is reproducible with
  `SOURCE_DATE_EPOCH=1793836906`. **Not published to PyPI** (no token).

Losses and limits: modes 9–14 are not implemented in Python; the wheel exists
only as a release artifact; PyTorch is an optional extra and is never imported
at module load.

## Real public model family (measured once, single host)

Receipt `bench/receipts/real-model-20261105T012000Z.json`; analysis and every
loss in `bench/RESULTS-real-model.md`.

Corpus: `Qwen/Qwen2.5-0.5B` and `Qwen/Qwen2.5-0.5B-Instruct`, one 988,097,824-byte
BF16 `model.safetensors` each, 290 identical tensor names/shapes, SHA-256
verified against the Hugging Face LFS pointers. Every restored file matches the
upstream SHA-256. All compared methods are lossless.

| Method | Both checkpoints | vs ZipNN |
|---|---:|---:|
| ZipNN 0.5.4 | 1,316,518,229 B | — |
| **WeftFS store** | **1,389,785,926 B** | **+73,267,697 B (5.57% relative; 5.27% of WeftFS)** |
| WeftFS data only | 1,313,652,215 B | −0.22% (tie) |
| zstd `--patch-from -12` | 1,503,140,795 B | +14.18% |
| zstd `-19` | 1,528,909,007 B | +16.13% |

- WeftFS **wins the instruct checkpoint by 8.64%** over ZipNN (601,380,551 vs
  658,236,344 B) and by 21.3% over zstd `-19`; the planner chose 241 weight
  deltas, 1 zstd frame, and 48 raw tensors.
- WeftFS **loses the base checkpoint by 19.8%** to ZipNN (788,405,375 vs
  658,281,885 B) and by 3.1% to zstd `-19` (764,815,908 B). That single loss
  consumes the delta win, so complete storage loses overall.
- WeftFS beats independent zstd `-19` by 9.1% and zstd `--patch-from` by 7.5%,
  but loses to ZipNN overall.
- Ingest is 136.0 s (30.7 s base + 105.3 s instruct) versus 0.50 s for ZipNN and
  538.9 s for zstd `-19`; peak RSS 2,540,908 KiB. The instruct phase reads
  19.07 GB from the backend while pricing three candidates per tensor.
- Full instruct restore loses by 9.7× to zstd and 73× to ZipNN (Huffman decode
  plus base dependency); a single tensor restores in 7.3 ms (base) / 118.6 ms
  (instruct, delta + base).
- DFloat11 0.5.0 is unavailable: it constructs a CUDA `RawModule` at import and
  the court has no CUDA driver. `zstd --patch-from -19` is unavailable under
  the 3 GiB limit (killed after 510.3 s, `maxrss 3,131,264 KiB`); the reported
  patch baseline is `-12`. No DFloat11 or `-19` patch bytes are claimed.
- The first-choice SmolLM-135M pair could not exercise sharing (F32 base vs
  BF16 instruct); it was ingested as a cross-dtype secondary and restores
  byte-exactly with no causal parent.
- Single host, warm caches, no cache dropping, measured once; no Go parity for
  this court.

## End-to-end mounted filesystem (FUSE)

Receipt `bench/receipts/fuse-bench-20261105T022024Z.json` (+ `.log`); analysis
in `bench/RESULTS-fuse-v0.5.md`. Single host, synthetic fixtures, buffered I/O,
page caches **not** dropped. ext4 is the host bind-mount path and tmpfs is a
non-equivalent reference.

| Metric | WeftFS Rust | WeftFS Go | ext4 | tmpfs |
|---|---:|---:|---:|---:|
| create ops/s | 157.4 | 300.6 | 291,722.7 | 504,451.2 |
| stat ops/s | 22,165.6 | 5,722.2 | 2,456,214.9 | 2,387,552.3 |
| readdir ops/s | 11,738.0 | 2,607.2 | 345,967.5 | 466,234.2 |
| 4 KiB random write | 9.4 IOPS | 2.5 IOPS | 128,000 IOPS | 128,000 IOPS |
| 1 MiB sequential write | 6.9 IOPS | 0.3 IOPS | 9,142.9 IOPS | 9,142.9 IOPS |
| 4 KiB overwrite (sync) | 108.22 ms | 392.30 ms | — | — |
| mount-to-ready | 472.424 ms | — | — | — |
| first Context Root `ls` | 455.0 ms (10,001 entries) | — | — | — |

The dominant cause is explicit: every write recomputes the frozen v1
whole-file `ContentId`, an O(file size) digest pass. `fdatasync` contributes
only 13–36 µs of the 108/392 ms overwrite. Metadata creates are 970–3,205×
slower than ext4, `stat` 111–429×, `readdir` 29–179×, 4 KiB random writes
13,617× (Rust) and 51,200× (Go), and 1 MiB sequential writes 1,325× / 30,476×.
This is the **top open bottleneck for v0.6**. The numbers do not establish a
device write rate: the ext4/tmpfs write rows are page-cache submission rates.

## Verification

Latest full verification before this release preparation:
`bench/receipts/verify-20261105T030019Z.json` — **20/20 courts pass**, including
the host-only `claims` gate, `spec_drift`, `generated_types`, `buck2_cas`,
`agent_memory`, the Rust↔Go codec and context gates, and `fuzz_diff` (1,000,000
inputs, 0 mismatches, 0 panics). Revisions in that receipt: WeftFS `c9b8bdb`,
WeftCodec `90d9c13`, WeftCodec-Go `cf62bb1`, WeftFS-Go `1c99070`. The release
preparation after that receipt changes only version literals (`Cargo.toml`,
`Cargo.lock`) and documentation.

Supporting long runs:

- **Crash consistency: 100,000/100,000 iterations pass**, 0 partial, 0 bad,
  66,667 old / 33,333 new, 19.2 it/s
  (`bench/receipts/crash-torture-20261104T104233Z.json`). This is a
  process-fault proxy, not physical power loss.
- **POSIX torture: 1,202,560 completed operations** across six seed × direction
  configurations, all 6/6 pass with writer/reader manifest SHA-256 equality
  (`posix-torture-20261104T162109Z.json`, `…T172156Z`, `…T182537Z`).

## Still not claimed

- No physical media power loss and no host page-cache loss. The power-loss
  courts use a QEMU VM proxy, a lying-fsync device model, and a block-layer
  lying-disk model; the host cache probe cannot drop caches in the court.
- No RAPL/joule energy measurement (RAPL is unreadable under the court).
- **No 3FS run.** The POSIX backend is a capability configuration only.
- **No PyPI publication.** The wheel is a release artifact with no token.
- No multi-writer or distributed store. The store is single-host,
  single-writer with `flock`.
- Python does not implement modes 9–14.
- Region reads of delta/Huffman tensor frames decode the whole frame.
- The planner's decode costs are uncalibrated estimates.
- Go lacks `q4_0` tensor storage, a bounded commit index, persisted selector
  indexes, and provenance parity; its `PUNCH_HOLE`/`ZERO_RANGE` are
  content-only on dense v1.
- No fleet, production-data, or hardware-independent claim anywhere in this
  note.
