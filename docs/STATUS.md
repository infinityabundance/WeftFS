# Implementation status — 2026-11-07

This is the honest, evidence-backed state of the program. It distinguishes
implemented, tested, ignored, and not done. The current verification receipt is
`bench/receipts/verify-20261107T052349Z.json` (30/30 courts `pass`, 0 skipped,
including the host-only `claims` documentation gate, spec drift, generated-type
drift, `conformance_manifest`, `conformance_coverage`, quantized cross-decode,
context and authenticated-capability cross-language gates, Buck2 CAS, agent
memory, the R22 `threefs` court (3FS POSIX byte-identity plus the USRBIO ABI
conformance court), `ceph_rgw` — Ceph RGW 19.2.0, an independent S3
implementation, now running both the S3 backend suite and the R17 fenced-writer
suite — the MinIO `s3_court` (native S3 backend and fenced-writer suites), and
`fuzz_diff`), at
WeftFS `7c10ad4`, WeftCodec `d61ed76`, WeftCodec-Go `86ded1e`, WeftFS-Go
`9fa39d3`. This is the v1.0.0 release receipt, run after the 1.0.0 version bump;
`verify-20261107T014045Z.json` (30/30 at the pre-bump tree, WeftFS `0cfbea1`,
WeftCodec `57288e1`, WeftCodec-Go `86ded1e`, WeftFS-Go `39b58f2`) is retained.
One intermediate run was aborted because the shared host volume reached 100 %
(`No space left on device`); its log `verify-20261107T020631Z.log` is retained
and **no receipt exists** for it, because the script never reached receipt
writing; after deleting only gitignored `.docker-cache` scratch, the rerun
passed 30/30. Standalone courts not in the aggregate are cited in
`docs/RELEASE-v1.0.0.md` and in the per-subphase notes: Azure/Azurite
`azure-20261107T015947Z.json` (19 backend + 7 fencing, 0 failures), Go S3
`WeftFS-Go/bench/receipts/go-s3-20261107T020137Z.json` (17 pass / 1 skip
focused, full `-race` 0 failures, 17/17 cross-language), 3FS `--bench`
`threefs-20261107T020623Z.json`, and the Python court
`python-bindings-20261107T015918Z.json` (349 tests, 0 failures, 9 skipped). The
previous 30/30 receipt (`verify-20261106T232858Z.json` at `4704ded`), the
29/29 receipt (`verify-20261106T225457Z.json` at
`132e713`), the 29/29 receipt before it
(`verify-20261106T214749Z.json` at `9a6156c`), its 28/28 predecessor
(`verify-20261106T174703Z.json` at `37a3a39`), and the 28/28 receipt before that
(`verify-20261106T162208Z.json` at `0e3670a`) are retained. The failing first
R20 aggregate (`verify-20261106T212741Z.json`, 28/29) is also retained; that
failure was the pre-existing `tests/quota.rs` writer-lock flake,
`usage_survives_reopen_and_still_denies`, which passed standalone immediately
after and in the rerun. A court that cannot run in the aggregate's environment
(non-x86_64, no Docker daemon, a pinned image or binary unobtainable, or a
court's private network already taken) is recorded as `skip` with its exact
reason under `skips` in the receipt, and a skip is not a pass; this run had
none. The v0.9.0 release receipt
`bench/receipts/verify-20261106T141623Z.json` (27/27 at WeftFS `cef37bc`,
before `ceph_rgw` joined the aggregate) remains the release evidence, with
`docs/RELEASE-v0.9.0.md` and the Python court at that tree: 331 tests,
0 failures, 0 errors, 9 skips
(`bench/receipts/python-bindings-20261106T142616Z.json`). The earlier
`verify-20261106T083428Z.json` (27/27 at the R7 shared-mount tree) is retained.
The previous release receipt,
`verify-20261106T011444Z.json` (also 27/27), is retained for the v0.8.0 release
and is cited in `docs/RELEASE-v0.8.0.md`; its own first run at that tree,
`verify-20261106T010756Z.json`, is retained because `rust_store` hit the
pre-existing `tests/quota.rs` writer-lock flake (`usage_survives_reopen_and_still_denies`,
`Busy` on reopen); that test passed standalone five times and in three full
`cargo test --all-targets` runs. The earlier
`verify-20261105T175154Z.json`, `verify-20261105T174254Z.json`, and
`verify-20261105T034941Z.json` are retained
as history; the last of those had one pre-existing `tests/representation.rs`
shared atomic decode counter race (`expected exactly one decoded chunk, got 2`)
that passed standalone and in the 035513Z rerun. Earlier
`verify-20261104T022254Z`,
`verify-20261103T220311Z`,
`verify-20261103T172416Z`, and `verify-20261103T165944Z` receipts are retained
as history. The P6 store-backed Buck2 CAS court is separate from that full
`verify-all` receipt: it passes 41/41 assertions at WeftFS `d175fe7` and is
recorded in `bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json`,
with the benchmark in
`bench/receipts/buck2-cas-store-20261105T163930Z.json` and analysis in
`bench/RESULTS-buck2-cas-store.md`.
The Q5 PyTorch Distributed Checkpoint adapter receipt is
`bench/receipts/pytorch-dcp-20261106T000734Z.json` (real torch 2.15.0+cpu,
topology matrix, 8 x 4 MiB bench); analysis in `docs/PYTORCH-DCP.md`.
The
twelve-step showcase receipt is
`bench/receipts/showcase-20261103T171440Z.json` (twelve of twelve `pass`).
The P7 benchmark receipts are `bench/receipts/bench-codec-20261103T175847Z.json`,
`bench/receipts/bench-store-20261103T175920Z.json`, and
`bench/receipts/bench-fs-20261103T180751Z.json`; the analysis, losses, and
gaps are in `bench/RESULTS-v1.md`. The comparative pass receipts are
`bench/receipts/bench-external-20261104T000122Z.json`,
`bench/receipts/bench-codec-matched-20261103T232158Z.json`,
`bench/receipts/bench-fs-cold-20261103T232216Z.json`,
`bench/receipts/bench-store-20261103T235127Z.json` (same-revision store side),
and `bench/receipts/energy-probe-20261103T225547Z.json`; analysis is
`bench/RESULTS-comparative-v1.md`. The F2 receipt is
`bench/receipts/f2-20261103T205045Z.json` with its raw log beside it. The F3
receipts are `bench/receipts/context-mounts-20261103T210625Z.json` (6/6),
`bench/receipts/ambiguity-20261103T210633Z.json` (4/4),
`bench/receipts/time-travel-20261103T210633Z.json` (4/4), and
`bench/receipts/replay-bundle-20261103T210824Z.json` (4/4), each with its raw
`.log` and `.results.tsv`; the determinism comparison is
`bench/receipts/context-mounts-determinism.txt` (`semantic_match=true`). The
1,000-mount scale receipts are
`bench/receipts/context-mounts-20261105T122643Z.json` and
`bench/receipts/context-mounts-20261105T131116Z.json` (5/5 each, 1000/1000
mounts, `semantic_match=true`), with the directed-limit ceiling in
`bench/receipts/context-mounts-20261105T140342Z.json` (INCOMPLETE 692/1000) and
`bench/receipts/context-mounts-20261105T135805Z.json` (400/400 at 24 GiB /
4,096 pids); analysis `bench/RESULTS-context-mounts-1000.md`. Raw logs and
results TSVs for those runs are stored as `.log.zst` / `.results.tsv.zst`.
The F6 cross-language gate is `WeftCodec-Go/hack/cross.sh` (tensor/tokens/
quantized byte-identical in both directions). The current F7 receipt is
`bench/receipts/ai-corpus-20261104T021627Z.json` with cost graph JSON/CSV beside
it (whole-pair-aware planner); analysis, losses, and caveats are in
`bench/RESULTS-ai-v1.md`. The historical frame-smallest receipt
`ai-corpus-20261103T220113Z.json` is retained. F8 receipts are
`bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json` (41/41,
store-backed CAS with mount and cross-language) and
`bench/receipts/agent-memory-20261104T022512Z.json` (7/7 simulated-timestamp
months-later replay); the earlier prototype receipt
`bench/receipts/buck2-cas-20261104T022511Z.json` (15/15) is retained. D2 authenticated capability grants are implemented and
released in v0.6.0: Ed25519 `weft.capability.envelope.v1`, offline verification
against an explicit revocation set, enforcement layered on POSIX and explicit
capabilities, a two-tenant isolation court, and the envelope benchmark receipt
`bench/receipts/capability-envelope-20261105T034858Z.json` (analysis in
`bench/RESULTS-capability-envelope.md`). The cross-language gate is
`WeftCodec-Go/hack/capability-cross.sh`: byte-identical signed blobs and
identical `BadSignature`/`WrongGrantee`/`Expired`/`Revoked`/`NonCanonical`
rejections. Generated-type drift is `scripts/gen-types.sh --check`,
wired into `verify-all`. FLOAT_TENSOR (mode 9) now encodes its exponent plane with `huffman-canonical-v1`: `cargo test --all-targets` (133 tests) and `go test ./...` pass in the Docker courts, and `WeftCodec-Go/hack/cross.sh compare` reports `PASS identical float_tensor`, `float_tensor_single`, and `float_tensor_uniform`; this change is covered by the `codec_cross_compare` court in `verify-20261105T035513Z.json`.
The F9 TLA+ receipt is `bench/receipts/tla-20261103T224446Z.json` (corrected
model pass; flawed model counterexample). The F9 power-loss receipts are
`bench/receipts/powerloss-20261103T224453Z.json` (30/30, `cache=none`),
`bench/receipts/powerloss-20261103T224652Z.json` (10/10, `cache=writeback`), and
`bench/receipts/powerloss-20261104T014805Z.json` (30/30, `cache=unsafe`,
lying-fsync device model). The block-layer lying-disk matrix is
`bench/receipts/blkdebug-20261104T072008Z.json` (30 iterations; dm-flakey
silent drop 5/5 lost, dm-flakey EIO 5/5 fail-closed, snapshot total drop 5/5;
blkdebug write/flush error 10/10 blocked at ext4 mount). The host-cache probe is
`bench/receipts/hostcache-probe-20261104T072314Z.json` (global drop refused:
`Read-only file system`). The B2 100,000-iteration receipt is
`bench/receipts/crash-torture-20261104T104233Z.json` (raw log and TSV stored
losslessly as `.zst` beside it; the 10,000-iteration receipt
`crash-torture-20261104T013521Z.json` is retained). The counter/energy probe is
`bench/receipts/perf-counters-20261104T015026Z.json` (perf counters available;
RAPL energy still unreadable). The whole-disk ZFS/Btrfs fio run is
`bench/receipts/bench-fs-cold-20261104T071922Z.json` (ext4/XFS/Btrfs/ZFS,
`zfs-whole` uses raw `/dev/vdb`; one repetition per cell). The earlier
file-backed ZFS run `bench-fs-cold-20261104T014727Z.json` is retained and
separately labelled. The abstraction mapping, assumptions, and limitations are
in `verification/README.md`.

**CHECKPOINT TOPOLOGY (P3), 2026-11-05.** A checkpoint is now a store directory
of named tensors with a canonical `weft.checkpoint.v1` manifest
(`docs/checkpoint-topology.md`). The manifest records, per tensor, the logical
dtype/shape/`LogicalTensorRoot`, the content root, the element bytes, and the
storage topology of every shard (`{axis, shards, index, object_name, object_id,
content_root, logical_root, byte_len}`). Each shard is a mode-10
`TENSOR_SHARDED` tensor object whose `q8_0` words are the shard's exact bytes
(a byte transport, not a lossy quantization), written with the shard shape's
last dimension scaled by the element size so the codec region API addresses
logical slices. `checkpoint write` splits a safetensors container along an axis,
rejects non-divisible splits with "no silent padding", and publishes all shard
objects plus the manifest in one group commit. `checkpoint restore` reassembles
from the shard objects, verifies every shard root and the full tensor root, and
can re-express the bytes under `sharded 8/4/2/1`, row-major, column-major, or
blocked topologies before writing the container; complete restores reproduce
the source header and require the source SHA-256. A region restore
(`--tensor NAME --range ...`) reads only intersecting shard objects through the
codec region API and reports shard/unit/byte counters. Real-weight court on the
cached Qwen2.5-0.5B container (BF16, 290 tensors, 988,097,824 bytes, SHA-256
`88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`): 8-way
ingest 41.53 s / 582,028 KiB peak RSS / 825,180,192 store bytes / 2,320 shard
objects (ingest wall varied 37.04–83.77 s across three runs of the same code);
complete restores under topology 8 and 4 read the same 821,882,936 bytes and
reproduce the source SHA-256 exactly (warm pair 20.44 s and 19.89 s; the first
restore after ingest varied 56.30–111.50 s with host writeback state);
all 290 logical roots are identical across topologies; a 100-row slice reads
one shard and 28,053,320 bytes instead of 224,668,493; and a plain mount plus
the unmodified `safetensors` 0.8.0 loader with NumPy 2.4.6 + `ml_dtypes` 0.6.0
reads all 290 tensors from the mount and matches the source SHA-256. Receipt:
`bench/receipts/checkpoint-topology-20261105T062409Z.json`; analysis:
`bench/RESULTS-checkpoint-topology.md`. Tests `tests/checkpoint_topology.rs`
9/9 (topology matrix, partial slice counters, missing/corrupt shard, manifest
root mismatch, non-divisible split, CLI end to end). `cargo test --all-targets`
154 passed / 0 failed; `cargo test --features fuse --all-targets` 182 passed /
0 failed / 2 ignored (the pre-existing CAP_SETUID and shared-counter tests).
The Go store does not implement `weft.checkpoint.v1`; this phase is Rust-only.

**TENSOR_SHARDED (mode 10), 2026-11-04.** Both codecs now implement an
additive sharded quantized tensor mode (`spec/WEFT-AI-CODEC.md` §11,
`spec/WEFT-TENSOR.md` §2.1). A shard frame carries a canonical
`weft.ai.tensor_sharded.v1` descriptor (`q8_0`/`q4_0`, whole shape, global
per-block f32 scale bits and i64 zero points, shard axis/count/index, and the
WeftTensor `layout_root`) plus one `huffman-canonical-v1` stream over the
shard's quantized words and the global scale plane. Decoding reconstructs the
exact quantized values and scale bits; dequantization is out of band. The
logical root is computed over the whole canonical row-major quantized words
and block records, so it does not change with shard axis or count. Evidence:
`cargo test --all-targets` 150 passed / 0 failed / 0 ignored; `go test ./...`
all packages pass; `WeftCodec-Go/hack/cross.sh compare` reports byte-identical
mode-10 frames, decode equivalence, and identical roots for all three sets.
Vectors are `conformance/v1/sharded/001`–`003` (valid) and `101`–`108`
(rejects), byte-identical in both repositories. The historical
`017_unknown_mode_reserved10` vector is superseded by
`017_unknown_mode_reserved11`; that vector is now itself superseded by
`017_unknown_mode_reserved12` because mode 11 is allocated (below). Receipt:
`bench/receipts/tensor-sharded-20261104T211555Z.json`.

**WEIGHT_DELTA (mode 11), 2026-11-04.** Both codecs now implement an exact,
dependency-linked tensor delta (`spec/WEFT-AI-CODEC.md` §12). A frame carries a
canonical `weft.ai.weight_delta.v1` descriptor (`bf16`, `fp16`, `fp32`, `q8_0`,
`q4_0`; shape, element count, word bits, base frame hash, `op: "mod-sub"`, plane
count/coder, `delta_root`, and stored payload length) and one
`huffman-canonical-v1` stream per byte plane. The base is a single dependency
frame; decoding resolves it through the caller's resolver (DAG depth/cycle/
missing/hash rules apply) and, when the base frame bytes are available, checks
its own dtype and shape. All arithmetic is on unsigned words, so every bit
pattern (NaNs, denormals, signed zeros, infinities) reconstructs exactly. A
4 MiB bf16 base/target pair differing in 1% of elements produces a 533,110-byte
delta frame versus 4,194,410 independent zstd-3 bytes (3,649,949 at zstd-19);
the two Huffman planes are 265,022 + 267,643 bytes of 2,097,152 raw bytes each;
encode median 12.86 ms, decode median 16.99 ms; reconstruction and the logical
tensor root match the original. Evidence: `cargo test --all-targets` 161 passed
/ 0 failed / 0 ignored; `go test ./...` all packages pass (103 top-level tests);
`WeftCodec-Go/hack/cross.sh compare` reports `PASS identical
weight_delta_bf16`, `weight_delta_fp32`, `weight_delta_q8` and decode
equivalence in both directions. Vectors are
`conformance/v1/weight-delta/001`–`003` (including an all-zero delta) and
`101`–`108` (base dtype mismatch, base shape mismatch, unknown op, word-bits
inconsistency, delta-root mismatch, payload-length mismatch, missing base,
dependency cycle), byte-identical in both repositories. Receipt:
`bench/receipts/weight-delta-20261104T214337Z.json` (synthetic, single host,
Docker court).

## 1. Repositories and revisions

| Repo | Verified revision | Role |
|---|---|---|
| `WeftFS` | `d6ba189` | program home, Rust store + FUSE, specs, ADRs, courts |
| `WeftCodec` | `3f3eae0` | Rust codec v1 + AI modes, checkpoint and capability support |
| `WeftCodec-Go` | `65bc14d` | independent Go codec, capability parity |
| `WeftFS-Go` | `f920ae0` | independent Go store + FUSE, spec mirror |

These are the revisions pinned by the latest full verification receipt
(`bench/receipts/verify-20261106T011444Z.json`, 27/27 courts pass).
The current release is **v0.8.0** (`docs/RELEASE-v0.8.0.md`); the tag, the
per-repo GitHub releases, the four GHCR images, the two crates, and the Python
wheel are cut from the `staging` and `main` commits after the version bump and
release documentation, with this receipt's code revisions unchanged apart from
version literals and docs (both branches are kept identical and pushed in all
four repositories).

## 2. What is implemented and tested

### WeftCodec (Rust) — implemented

- Exact WFT1 framing: 96-byte header, 48-byte dependency/chunk entries, header
  and trailing CRC32C, checked lengths, reserved-bit rejection.
- Domain-separated BLAKE3, minimal LEB128, canonical WCB with encoded-key map
  ordering and non-canonical rejection.
- RAW, ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED, TENSOR, TOKEN_SEQUENCE,
  QUANTIZED_TENSOR; chunked RAW/ZSTD; `read_range` decodes only intersecting
  chunks.
- Resolver + frame-DAG decoding with cycle detection and depth/fetch/decoded
  bounds; WFA1 self-contained archives.
- WeftQL parser, normalization, canonical selector bytes, SelectorId.
- CLI (`encode`, `decode`, `inspect --json`, `verify`, `archive`, `selector`,
  `conformance`), including `--mode tensor` and `--mode tokens`.
- **110 tests pass** in the Rust court. Frozen vectors: 60 codec (including 28
  AI-mode valid/reject vectors), 20 WCB, 22 selector, 16 context, 10 causal,
  and 36 adversarial.

### WeftCodec-Go — implemented, independent

- Native Go implementation of the same v1 bytes (no cgo, no Rust calls).
- **57 top-level tests pass**, including Rust-vector replay and malformed-input
  classes; `go vet` and a CGO-disabled build pass.
- Cross-language gates pass in both directions for RAW, ZSTD, REFERENCE, DELTA,
  DICTIONARY, chunked, STRUCTURED, TENSOR, TOKENS, and QUANTIZED_TENSOR.
  RAW/REFERENCE/DELTA/STRUCTURED/TENSOR/TOKENS/QUANTIZED_TENSOR frames are
  byte-identical; ZSTD/DICTIONARY are decode-equivalent by spec.

### F6 — AI codec modes (implemented)

- `TENSOR` (mode 6): exact little-endian row-major payload plus a canonical WCB
  descriptor (`weft.tensor.v1`) carrying dtype, shape, layout, endianness,
  element count, payload bytes, and a `weft/tensor/payload/v1` hash binding.
  Supported dtypes: `f16`, `bf16`, `f32`, `f64`, `i8`, `u8`, `i16`, `i32`,
  `i64`. Checked shape arithmetic; scalar and zero-element tensors; rank ≤ 8.
- `TOKEN_SEQUENCE` (mode 7): canonical WCB descriptor (`weft.tokens.v1`) and a
  deterministic `u64-zigzag-delta-leb128` payload: first id absolute, later ids
  wrapping 64-bit signed zigzag deltas, minimal LEB128, exact count and
  trailing-byte checks.
- Both modes: `decoded_len` is the payload length and `content_hash` is
  `H(weft/chunk/v1, payload)`; the descriptor binds the payload under its own
  domain. Frame `domain=0`, no dependencies, no chunks.
- `QUANTIZED_TENSOR` (mode 8, added 2026-11-04): packed `i8`/`u8` row-major
  payload plus a `weft.quantized-tensor.v1` descriptor carrying dtype, shape,
  layout, `affine-asymmetric-per-tensor`, exact f64 `scale_bits_le`, i64
  `zero_point`, element count, payload bytes, and a
  `weft/quantized-tensor/payload/v1` hash binding. Finite positive scale and
  dtype-range zero point are enforced; three valid and eight reject vectors;
  Rust↔Go frames are byte-identical.
- `FLOAT_TENSOR` (mode 9, added 2026-11-04): `huffman-canonical-v1` exponent plane plus raw sign+mantissa plane, bit-exact including NaN payloads, infinities, signed zeros and denormals; 3 valid + 8 reject vectors; Rust↔Go frames are byte-identical.
- **Not implemented and not allocated a wire value in mode 6:** blocked/
  column-major/sharded layout *fields*, i4 nibble packing and per-axis
  quantized scales, activation/gradient streams, embedding/KV-cache pages,
  attention masks, vocabulary-artifact binding, weight-delta descriptors, and
  semantic/structured deltas. They are `specified, not implemented` in
  `spec/WEFT-AI-CODEC.md` §6. The layouts now have an **additive** identity and
  addressing layer outside mode 6; see WEFT-TENSOR below.
- Cross-language: Rust→Go and Go→Rust decode gates pass; TENSOR, TOKENS, and
  QUANTIZED_TENSOR frames are byte-identical. Historical
  `017_unknown_mode_reserved8` (mode 8 reserved) is retained byte-for-byte and
  marked superseded by the new mode-9 reject vector.

### WEFT-TENSOR — logical identity and region addressing (implemented, both codecs)

- `spec/WEFT-TENSOR.md` defines a layout-independent identity:
  `LogicalTensorRoot = H("weft/tensor/logical/v1", WCB({schema, dtype, shape,
  element_count, values_hash}))` with
  `values_hash = H("weft/tensor/values/v1", row-major element bytes)`.
- Canonical `weft.tensor.layout.v1` descriptors for `row-major`,
  `column-major`, `blocked`, and `sharded`; rank/zero/overflow/divisibility and
  field-presence rules fail closed. Additive only: mode 6 stays `row_major`.
- Physical arrangements and `read_region` report the number of physical units
  touched, proving that a region inside one shard touches exactly one shard.
- `weft.tensor.shard.v1` containers (`u32le || WCB || payload`, payload-hash
  bound) and `shard_root`, which accepts shuffled input and reorders by `index`
  deterministically; duplicate, missing, out-of-range, and inconsistent shards
  are rejected. Rust and Go agree.
- Conformance vectors `conformance/v1/tensor/` (3 valid logical, 10 logical
  rejects, 4 valid layouts, 10 layout rejects, 2 shard sets with byte-identical
  containers) are byte-identical in both repos. Cross gate:
  `WeftCodec-Go/hack/tensor-cross.sh` (roots and containers byte-identical; each
  language reassembles the other's shards). Bench receipt:
  `bench/receipts/wefttensor-20261104T205020Z.json` (64 MiB bf16; in-memory,
  synthetic, single host).
- Not done: no FUSE/store/object-store binding of the layout layer; no
  partially resident tensors, arbitrary irregular block lists, or padded
  blocks.

### F7 — AI corpus and cost graph (implemented, synthetic)

- Committed deterministic corpus `bench/corpora/ai-v1/`: 60 files, 1,939,911
  source bytes, 1,516,737 visible bytes; prompts, JSON/text tool outputs,
  near-duplicate agent intermediates, 12 typed tensors, 8 token sequences, and
  near-duplicate checkpoint-like blobs. License CC0-1.0, synthetic provenance,
  per-file SHA-256 manifest.
- Receipt `bench/receipts/ai-corpus-20261104T021627Z.json` + cost graph JSON/CSV
  beside it. Historical `ai-corpus-20261103T220113Z.json` retained. Analysis,
  every loss, and caveats in `bench/RESULTS-ai-v1.md`.
- Current measured physical bytes: `zstd_forced` 575,808; `weft_auto` 575,713;
  `cdc_zstd` 603,094 (+4.74%); `weft_context` 514,506 (−10.65% vs forced zstd)
  with raw pair bytes 697,014 (45.96% of visible), 8 pair losses, and 8
  contextual objects. The current whole-pair-aware planner rejects contextual
  candidates whose base is not amortized and whose pair cost does not beat the
  context-free frame; 24 such rejections are recorded. The historical
  frame-smallest planner produced 176,564 physical bytes but 1,367,212 pair
  bytes and 20 pair losses; it is retained as a superseded result.
- Typed TENSOR/TOKENS modes were evaluated for all 20 typed objects and chosen
  0 times; descriptor overhead 272–288 bytes. QUANTIZED_TENSOR is implemented
  and cross-language tested but is not part of the ai-v1 corpus. No superiority
  claim.

### F8 — Hooks: store-backed Buck2 CAS, agent memory, generated types (implemented)

- **D1 Buck2 adapter (store-backed, not a plugin):** `adapters/buck2/` is now a
  CAS on the WeftFS store, not a private directory. Every blob is an ordinary
  WeftFS file whose `ContentId` is `H("weft/chunk/v1", bytes)`; the action
  record is a canonical WCB file (`weft.buck2.action-record.v1`); `cas_action`
  (record→output) and `cas_output_of` (output→record) are typed edges, so
  `by:action=<key>` is a one-hop WeftQL selector. `ActionKey =
  H("weft/buck2/action/v1", WCB({action_type, toolchain, sorted input
  ContentIds, canonical params}))` — input paths are metadata, not key material.
  Verbs: `store --key-file --blob`, `fetch --key --out`, `query --by
  action=<key>`, `verify --key --blob`, `context`, `resolve`, `delete`, `gc`.
  Same key + identical bytes is an idempotent hit; same key + different bytes
  fails closed with `Conflict` before any store mutation. The correctness court
  runs in the FUSE court and passes **41/41** assertions, including:
  `mount --context by:action=<key>` reads the artifact byte-exactly; a flipped
  manifest byte is detected by `verify`/`fetch`; GC collects an unreferenced
  action while a named snapshot + Context Root keep another output exact; the
  Rust CLI verifies/cats/stats/resolves a Go-written store; the Go adapter
  fetch/verify/query reads a store written entirely by the Rust CLI. Go tests
  are 7/7 in the FUSE court and 5/7 pass with 2 honest skips in the Go-only
  court (no Rust oracle). Receipts:
  `bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json` (41/41)
  and `bench/receipts/buck2-cas-store-20261105T163930Z.json` (bench); analysis
  `bench/RESULTS-buck2-cas-store.md`. The historical prototype receipt
  `bench/receipts/buck2-cas-20261104T022511Z.json` (15/15) is retained but
  superseded. `buck2` itself was **absent** in the court; no Buck2 run, no
  plugin/worker ABI, no REAPI gRPC.
- **D2 agent long-term memory:** `scripts/demo-agent-memory.sh` creates
  prompt/model/tool/dataset/output objects and five typed edges, three Context
  Roots (task/model/time), a January snapshot, an exported bundle, then
  simulates September writes and replays the January bundle later. Verified:
  run-001 sha256 exact in all January views; run-002 absent from January;
  September time root keeps old bytes exact; replayed bundle reproduces all
  three ContextRootIds and five lineage edges with Go only. Receipt
  `bench/receipts/agent-memory-20261104T022512Z.json` (7/7). Timestamps are
  simulated; this is not a real-time aging or retention measurement.
- **D3 spec-as-code:** `spec/schema/identity-v1.schema` is the canonical source;
  `scripts/gen-types.sh` generates `WeftCodec/src/generated.rs` and
  `WeftCodec-Go/generated/generated.go` (schema/domain constants, limits,
  record types, field lists, source SHA-256). The runtime identity layer
  re-exports generated constants; Rust and Go tests compare generated values to
  runtime values; `scripts/gen-types.sh --check` fails on drift and is a court
  in `verify-all`. The schema is exported to subproject `spec/schema/` and
  checked by `scripts/check-spec-drift.sh`.

### WeftFS (Rust) — implemented core + FUSE (with documented limitations)

- Portable store: `FORMAT`, `LOCK`, dual CRC32C superblock slots, `WFC1`
  commits, canonical commit/checkpoint schemas, immutable chunks/manifests,
  snapshots, relations, tips, xattrs, saved contexts.
- Atomic publication with fsync ordering; one writer (`flock` on a local or
  object namespace, a fenced lease with a conditional compare-and-swap on S3,
  GCS, and Azure); generation fallback; recovery; full verify; mark-and-sweep
  GC; checkpoints; selector evaluation with POSIX + `weft.acl.read`
  authorization; ambiguity errors.
- FUSE: lookup/getattr/setattr/open/create/read/write/flush/fsync/release,
  readdir(+plus), mkdir/rmdir/unlink/rename(+NOREPLACE/EXCHANGE)/link/symlink/
  readlink, xattrs, statfs, access, mknod(regular; FIFO/device `EPERM`),
  fallocate(0/KEEP_SIZE/PUNCH_HOLE/ZERO_RANGE/COLLAPSE/INSERT),
  copy_file_range(server-side), lseek(SEEK_DATA/SEEK_HOLE), O_APPEND/O_TRUNC/
  O_EXCL, sparse writes, open-unlinked, snapshot mounts, `/.weft/id/<hex>`,
  saved context views. Explicit binding boundaries: Rust `O_TMPFILE` and
  daemon-side flock/POSIX locks are unavailable in fuser 0.15.1.
- Deterministic store fixture `conformance/store/v1/` (head
  `8959c65b...`) with `EXPECTED.json`.
- **64 core tests pass** (lib 7, CLI 6, crash proxy 3, recovery 11, store core
  11, extensions 11, bounded recovery 3, context 12). `tests/bounded_recovery.rs`
  enumerates every one of generations 3–5 corrupted in three ways (empty, half,
  bit flip) and every superblock-slot corruption, asserting the previous whole
  state and replay idempotence. This is bounded enumeration, **not** TLA+/Alloy
  model checking.
- **86 FUSE-feature tests pass, 0 fail, 2 ignored** (fuse 22, core 64):
  - ownership test needs `CAP_SETUID` (court has only `CAP_SYS_ADMIN`);
  - same-inode concurrent direct read/write can return `EIO` on kernel 7.3
    with fuser 0.15.1. The daemon logs successful replies and the failing
    request never arrives; rate depends on timing. This is documented, with
    the exact reproducer, in `docs/fuse-status.md`. Store-level atomicity is
    tested separately. A same-description write after shared `mmap` also
    returned `EIO` once; shared mmap/write/msync/read passes with a fresh
    description.

### WeftFS-Go — implemented with documented gaps

- Independent Go reader/writer for the identical on-disk format, all eight
  commit ops, checkpoints, superblock recovery, manifests/chunks, selector
  evaluation, authorization, `verify --full`.
- Native `go-fuse/v2` frontend with the core POSIX surface, snapshot mounts,
  `/.weft/id/<hex>`, and saved-context views. Added in F8: `getlk`/`setlk`/
  `setlkw` (POSIX byte ranges + flock through an in-memory manager and
  `EnableLocks`), `mknod` (regular files only; FIFO/socket/device `EPERM`),
  `tmpfile` (O_TMPFILE plus link through `/proc/self/fd`), `fallocate` modes
  0/`KEEP_SIZE`/`PUNCH_HOLE`/`ZERO_RANGE` (with store-level COLLAPSE/INSERT),
  `copy_file_range`, and `lseek` `SEEK_DATA`/`SEEK_HOLE`. `ioctl` is
  intentionally unimplemented (`ENOTTY`).
- **Online/background GC:** `store.GCBounded`/`GCWatch`, `gc --apply --watch`,
  and `mount --gc-watch`. Passes require the writer lock, run under the FUSE
  server mutex, delete at most a configured number of candidates, are
  cancellable, preserve snapshots/checkpoints/pins, and are disabled by
  default.
- **74 top-level tests pass, 2 skipped (pre-existing child-process guards),
  0 failed** under `go test -race -count=1 ./...`; `go vet ./...` and
  `CGO_ENABLED=0 go build ./...` pass. New verb/GC tests are listed in
  `WeftFS-Go/NOTES.md` §F8.
- Rebuilding the Rust fixture through the Go writer reproduces the same head
  and byte-identical `FORMAT`, `superblock`, commits, manifests, chunks, and
  snapshot pointer.
- Bidirectional handoff gates pass: Rust fixture → Go mutations → Rust verify;
  Go init/mutate → Rust verify/mount; alternating generations; snapshots and
  checkpoints both ways; crash proxy through both; and Go background GC → Rust
  `verify --full` + snapshot mount.
- Remaining limits: `PUNCH_HOLE`/`ZERO_RANGE` define content but do not reclaim
  physical chunks (dense v1); 7.3.1 FUSE rejects COLLAPSE/INSERT before
  forwarding; locks are per-daemon advisory and do not implement the POSIX
  close-any-fd release for owners never seen on the closed handle; `ioctl`,
  mmap writeback, and persisted atime/ctime are not implemented.

### F8 — WeftFS-Go FUSE verb closure and online GC (implemented 2026-11-03)

- Commands: `scripts/docker-court.sh go "cd /work/WeftFS-Go && gofmt -l . &&
  go vet ./... && CGO_ENABLED=0 go build ./..."` (clean) and
  `scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race -count=1
  -v ./..."` (**74 passed, 2 skipped, 0 failed**; skips are the existing
  child-process guards).
- CLI watch smoke: `gc --apply --watch --interval 100ms --max-deletes 1
  --grace-secs 0` deleted two unreachable blobs in two bounded passes, then
  reported zero; SIGTERM exited 0; `verify --full` reported `ok=true`.
- Cross-language: the Rust oracle created a snapshot plus garbage; the Go
  watcher swept it; Rust `verify --full`, Rust live read, and a Rust snapshot
  mount all passed. Existing GC/handoff gates A–D also pass.
- Blocked verb evidence: on kernel 7.3.1, COLLAPSE/INSERT return
  `EOPNOTSUPP` with go-fuse debug showing no `FUSE_FALLOCATE` frame; the store
  implementations are directly tested. `PUNCH_HOLE` without `KEEP_SIZE` is
  also rejected at that boundary. Details and safe-use notes are in
  `WeftFS-Go/NOTES.md` §F8.

### F2 — Context Roots, capabilities, and causal identity (implemented in Rust and Go)

- **Context Root persistence:** `context_root` relations from the root object
  with the canonical record hex and authoritative `weft://` URI. Duplicate names
  are `Exists`; `--replace` is delete+add in one commit. Capability records are
  stored in the same commit when first referenced.
- **Resolution:** recomputes `ContextRootId`, verifies the stored URI, requires
  the pinned snapshot to replay, accepts only `weft-resolver-v1`, authorizes
  before counting, and returns ordered results plus `context_root_id`,
  `selector_id`, `snapshot_id`, `matched_count`, `returned`, and closure
  entries. 0/1/n is always explicit.
- **CLI:** `context create|list|show|resolve|materialize|uri|delete`,
  `causal show`, `uri canonicalize|show`, and `--causal` on `create`/`write` in
  both languages.
- **Capabilities:** `weft.capability.v1` JSON/hex files; enforcement for
  namespace/root grants, selector universe, relation/tag predicates, exact-id
  access, saved-context views, and materialization. Denials are
  `Unauthorized`/`NotFound` and do not expose counts, ids, evidence, or
  existence. v1 makes no cryptographic unforgeability or timing claim.
- **Budgets:** effective bound is the component-wise minimum of root and
  capability budgets. No truncation policy exists in v1: exceeding a bound is
  `LimitExceeded`, never a silent truncation. `max_bytes` bounds materialized
  bytes, `max_results` bounds returned results, `max_depth` is one-hop, and
  `max_work` is deterministic **authorized** objects inspected plus **visible**
  relation edges. Authorization and namespace-root readability are decided
  before work accounting, and `max_work`/`max_depth` failures emit fixed,
  non-disclosing messages (`resolver work budget exceeded`,
  `traversal depth exceeds max_depth`). This closes a real defect: the earlier
  implementation counted the whole state (including unauthorized objects and
  relations) and echoed the count. Proven by
  `tests/capability_nondisclosure.rs`: two stores with identical authorized
  views and different hidden populations return identical results, work units,
  and budget outcomes.
- **Causal identity:** `--causal` computes explicit parents plus the available
  transitive closure with a visited set (cycles terminate), stores the four
  reserved attrs, publishes the closure record content-addressed, and includes
  `identity_mode`/`causal_hash` in `VersionId` only for `causal_exact`.
  `bytes_only` remains byte-identical to the frozen v1 record.
- **Materialization:** all content hashes are verified before output is
  published; writes go to a sibling temporary directory and are renamed into
  place. A mismatch leaves no partial output. The manifest records root id,
  snapshot id, selector id, closure, capability hash, budget, counts, and
  per-file path/hash/size/ids/causal closure.
- **Verification and GC:** `verify --full` validates context records/URIs,
  re-resolves roots and reproduces recorded ids, checks causal attrs and
  closure blobs, and re-hashes stored capability records. GC retains causal
  closure blobs while their referencing object is retained; context/capability
  relations are ordinary metadata and never keep content alive by themselves.
- **Evidence:** `scripts/docker-court.sh fuse "cargo test --features fuse --all-targets"`
  reports **70 passed, 0 failed, 2 ignored** (the existing CAP_SETUID and
  kernel-7.3 direct-I/O limitations). `scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race ./..."`
  passes for `store` and `fusefs`, including Rust→Go and Go→Rust context,
  causal, capability, and materialization handoff gates.
  `go vet ./...` and `CGO_ENABLED=0 go build ./...` pass in the Go court.
  Golden causal vectors are `conformance/context/v1/causal_versions.json`.

### Codec-backed representation manifests (implemented 2026-11-04, opt-in, Rust + Go)

- **What it is:** a `weft.manifest.v2` representation manifest can store an
  object's bytes as a WFT1 frame (`kind:"codec"`) instead of raw chunks, with
  `strategy` (`zstd`/`delta`/`reference`), a `frame` blob, and a
  `dependencies` list of base content roots. v2 also permits `kind:"raw"`.
  The frozen `weft.manifest.v1` is unchanged and remains the default.
- **Selection:** `Options.representation = RepresentationMode::CodecAuto` plus
  `codec_min_bytes` (default 4096), reachable as `--representation
  raw|codec-auto` and `--codec-min-bytes` on both the CLI and `mount`. The
  default stays `raw`. The writer picks DELTA against the causal
  parent's content root only when it beats independent ZSTD by the configured
  margin; otherwise it falls back to a v2 raw manifest. There is exactly one
  v2 schema spelling (`weft.manifest.v2`); unknown schemas fail closed.
- **Read path:** range reads decode only intersecting codec chunks
  (`weftcodec::stream::read_range`), verify the frame's payload mode against
  `strategy`, bound dependency depth (8) and reject cycles, and verify the
  frozen whole-content hash on full reads. Missing/unreadable/mismatched
  dependencies fail closed; no partial or approximate reconstruction.
- **Retention:** GC treats representation dependencies as roots, so a retained
  dependent keeps its base; `verify --full` decodes representations and
  re-hashes them.
- **Measured** (8 MiB synthetic, release, rust court, host page cache; receipt
  `bench/receipts/representation-20261104T093437Z.json`): repetitive text
  8,395,250 B raw → 16,853 B ZSTD, encode 227.56 ms → 12.84 ms; near-duplicate
  8 MiB pair 8,395,250 B independent → 1,153,697 B DELTA, at the cost of
  full-object DELTA range reads (6.17 ms vs 0.05 ms raw). Incompressible
  near-duplicates correctly fell back to raw.
- **Byte-identity evidence:** with `Raw` (the default) the committed
  `conformance/store/v1` fixture still rebuilds to generation 11 and head
  `8959c65b…e1d81`, and `tests/representation.rs` asserts the v1 manifest
  layout, keys, and byte-identical manifests across two Raw stores.
- **Go parity (2026-11-04, `WeftFS-Go` `d7d8d06`):** the Go store now parses,
  writes, verifies, and GCs v2 manifests with the same selection rule, limits
  (depth 8, cycles, chunk count), fail-closed dependency handling, and CLI /
  mount flags. Cross-language tests run both directions for real: Rust writes
  zstd + delta + raw-fallback, Go verifies and reads byte-exactly; Go writes
  the same shape, Rust verifies and reads byte-exactly; Go GC retains a
  Rust-written delta base.
- **Not claimed:** experimental. V2 **frame bytes are not byte-identical**
  between the Rust and Go zstd encoders; the claim is mutual read/verify, not
  identical frames. DELTA/REFERENCE and unchunked ZSTD ranged reads materialise
  the whole plaintext in both implementations. A global representation
  planner and compaction/migration of existing objects remain future work.
  Bench numbers are synthetic, not production data.

### Storage-path performance: extent COW and bounded open (implemented 2026-11-04)

- **Extent-level COW range writes.** `Store::write_file`/`write_file_causal` no
  longer buffer or rewrite the whole object. A range write reads the manifest
  and only the chunks it touches; untouched `ChunkRef`s are reused verbatim and
  their blobs are neither read nor rewritten. New chunks are content-addressed
  and deduplicated; a byte-identical rewrite adds zero blobs and zero
  manifests. Measured on a 64 MiB object, 64 KiB chunks, 20 × 4 KiB aligned
  overwrites (release, rust court, medians): 33.01 ms vs 69.64 ms, `rchar`
  64.1 MiB vs 128.0 MiB, peak RSS 3.8 MiB vs 131.7 MiB; `wchar` identical
  (122,431 B) because both paths already dedupe unchanged chunks. Receipt
  `bench/receipts/extent-cow-20261104T090112Z.json`. **Limit:** v1's
  whole-file `ContentId` is a hash of every byte, so the digest pass remains
  O(file size); the gain is in writes, read-verification, and memory. A
  Merkle-root content identity that would make hashing O(changed chunks) is a
  v2 format change, not claimed.
- **Bounded replay / fast open.** `recovery::load_state_bounded` starts at the
  selected head generation and walks parents by generation number, stopping at
  the newest commit whose checkpoint verifies; it does not enumerate older
  commit files. Any inconsistency falls back to the authoritative full-scan
  recovery. Measured on a 10,000-generation store with `checkpoint_interval`
  256 (release, rust court, tmpfs): open median 71.6 ms / p95 73.2 ms reading
  **18** commit files, versus 3,789.1 ms / 3,892.0 ms reading 10,001 for the
  forced full path. Receipt
  `bench/receipts/fast-open-20261104T090945Z.json`.
- **Go parity for bounded open (2026-11-04).** WeftFS-Go `store/recovery.go` now
  implements the same generation-addressed walk with the full scan as the
  authoritative fallback; a read-only open never enumerates the commit
  directory and a writable open enumerates once for `next_generation`.
  Measured on a 100,000-generation store, interval 256 (go court, tmpfs,
  synthetic): bounded open median 2.80 ms / 162 commit reads, forced full scan
  2155.65 ms / 100,001 reads, identical recovered states. Receipt
  `bench/receipts/go-fast-open-20261104T190732Z.json`; courts
  `store/fast_open_test.go` (4 tests).
- **Semantics made explicit.** A bounded open records the note `bounded replay
  resumed from the checkpoint at generation N; commit files below it were not
  enumerated`, and `spec/WEFT-MODEL.md` now requires that note plus the
  fallback rules. Older-history gaps below a checkpoint are therefore not
  detected by open; `verify --full` still enumerates every commit and reports
  them. Tests: `tests/extent_cow.rs` (6) and `tests/fast_open.rs` (3),
  including corrupted-chain fallbacks and checkpoint corruption.
- **Not claimed:** these are single-host, page-cache measurements on synthetic
  fixtures, not device-level or fleet claims.
- **Group commit.** `Store::group` applies mutations to a journalled in-memory
  state and publishes exactly one durable commit; rollback and crash-proxy
  tests cover the failure path (`tests/group_commit.rs`). Measured on tmpfs
  over 5 fresh stores, release, rust court: 1,000 creates 89.87 ms → 22.01 ms
  and 1,000 commits → 1; 1,000 grouped writes 21.86 ms. Receipt
  `bench/receipts/group-commit-20261104T092015Z.json`. This is a store-API
  timing on tmpfs, not a device, FUSE, or fleet claim.

- **Streaming I/O and the file bound.** `create_file_streaming*` and
  `append_file_streaming` read the source in 1 MiB steps, compute the frozen
  content hash incrementally, and commit once. A 256 MiB streaming create uses
  ~7 MiB peak RSS versus ~263 MiB for the buffered path (190.7 ms vs 138.4 ms;
  receipt `bench/receipts/streaming-20261104T094742Z.json`). `MAX_FILE_BYTES`
  is now a 1 TiB defensive bound plus `MAX_CHUNKS_PER_FILE = 16,777,216`; every
  offset/size conversion is checked and returns `LimitExceeded` rather than
  truncating. A 512 MiB file is created and fully verified in the court at
  ~15 MiB peak RSS (`tests/streaming.rs`). Go matches both constants with no
  format change (`WeftFS-Go` `b6408fe`).
- **Effective large-file limit (honest):** the manifest is a WCB array, so the
  codec's `max_array_items` decode limit caps readable chunk counts far below
  1 TiB (at 64 KiB chunks, ~200k chunks ≈ 12.8 GiB for the default 1,000,000
  item limit). Neither implementation truncates; both fail closed with
  `LimitExceeded`. Raising that ceiling requires a chunk-table format change
  (the codec already has a chunk table for frames) and is future work.
- **Not claimed:** codec-backed streaming (`CodecAuto` above
  `codec_min_bytes` writes a v2 raw manifest) and streaming arbitrary-offset
  writes (append is the streaming write form; range writes remain
  chunk-materialising).

### Contextual indexes for selector resolution (implemented 2026-11-04, in-memory)

- **What it is:** `src/index.rs` `StateIndex` indexes attribute presence and
  scalar equality (`objects_with_attr`, `objects_by_attr_eq`), outgoing
  relations per `(subject, predicate)`, and `tagged_as` value edges. Candidate
  sets are computed per AST node (`And` intersects, `Or` unions, `Not` uses the
  authorized-set difference only when the inner set is exact) and intersect the
  authorized universe computed before evaluation.
- **Semantics:** candidate narrowing only — `eval_expr` remains authoritative and
  confirms every candidate, so results, evidence, ordering, `matched_count`, and
  ambiguity are identical to the index-free scan. This is enforced by the
  differential court `tests/index_equivalence.rs` (3 randomized states, seeds
  11/29/47, 315 selector/view comparisons, 1,566 assertions, compared against a
  retained scan oracle).
- **Cache:** bounded 8-entry LRU keyed by the snapshot commit hash that produced
  the state; the guard fails closed with `Corrupt` if the index does not
  describe the state (never a silent under-selection).
- **Persistence:** `indexes/<ab>/<commit>.idx` holds a compact binary WFX2
  segment (magic/version, commit, generation, three counts, sorted fixed-width
  sections, trailing `BLAKE3("weft/index/v2", preceding bytes)`) written
  atomically after a commit publishes. Arrays are sorted once at write time;
  load does one sequential read, bounds-checked slicing, and pre-sized map
  insertion. The segment is advisory: the read path accepts it only when
  magic/version, exact lengths, digest, commit key, generation, and
  object/relation/entry counts agree with the replayed state. Any failure
  records a reject in `Store::index_stats()` and rebuilds from state, so query
  results are unchanged. WFX1 segments are not interpreted; they are discarded
  and rebuilt. Only complete indexes are persisted. Read-only opens never write
  segments; GC deletes segments not named by the head, a snapshot, or a
  checkpoint commit; `verify` ignores them. See `tests/index_persistence.rs`.
- **Measured:** synthetic in-memory state, 10,000 objects + 10,000 relations,
  100 queries per kind, release, medians. Relation 132.795 ms → 0.689 ms warm
  (13.028 ms cold); tag 148.015 ms → 2.051 ms warm (14.044 ms cold); attr-eq
  1.640 ms → 1.239 ms warm. Receipt
  `bench/receipts/index-bench-20261104T085157Z.json`; labelled as a resolver-only
  synthetic measurement, not disk/FUSE/end-to-end.
- **Persistence measurement** (`bench/receipts/index-persist-20261104T141248Z.json`):
  10,000 objects / 10,000 relations, release, synthetic/tmpfs. Cold build
  9.83 ms; write including build 12.00 ms; WFX2 segment 2,423,650 B; peak RSS
  171,756 KiB. First relation query after a fresh reopen is 4.70 ms from the
  segment vs 12.44 ms rebuilding (tag 6.23 vs 14.21 ms). The superseded WFX1
  measurement (`index-persist-20261104T140435Z.json`, 3,065,209 B) was slower
  than rebuilding (19.05 vs 14.17 ms relation; 21.11 vs 12.96 ms tag); its
  canonical-WCB decode was the cost. WFX2 is retained for restart survival and
  is now measured as a first-query win.
- **Not claimed:** segments are per-commit full snapshots, not
  snapshot-incremental; the write path builds the whole index after each commit.
  GC retention is exact-commit membership, not graph reachability. Go has no
  segment parity yet.

### F3 — Context mounts, ambiguity court, time travel, and replay bundle (Rust + Go)

- **`mount --context NAME|weft://...`** (both frontends) makes the resolved
  Context Root's ordered, capability-authorized result set the read-only mount
  root. Entries use the established deterministic names (percent-escaped
  basename + `--` + 8 hex ObjectId, extended in 8-character steps on
  collision). `readdir` and `lookup` are fed by one helper, so they cannot
  disagree; every authorized result appears. The constant `.weft` exact-id
  surface remains reachable. Capability-scoped mounts are forced read-only and
  resolve (and therefore filter) before names or counts are produced.
- **`mount --at RFC3339|unix-ns`** (both frontends) walks the published parent
  chain of the base snapshot (or head), selects the newest commit with
  `timestamp_ns <= t` (ties: higher generation), and mounts that state
  read-only. A time before the first commit fails with
  `NotFound: no commit at or before … ns in the mounted lineage`; no state is
  substituted. `--context` pins its own snapshot, so `--context` + `--at` is
  rejected explicitly.
- **Concurrent mounts:** read-only opens take no writer lock in either
  implementation. A FUSE integration test mounts three different roots at once
  and then commits through a normal writer; the writer succeeds and the pinned
  mount does not observe the later commit. The scaled court spawns **100 FUSE
  daemons concurrently** (50 Rust, 50 Go) over 100 genuinely distinct Context
  Roots — 50 `by:task`, 25 `by:model`, and 25 `derived_from` views over 200
  corpus objects, 25 model objects, 10 grounding docs, and 599 typed relations.
  All 100 mounted; 425 file reads matched their declared SHA-256, one
  representative `weftfs cat` per mount matched the store bytes, all 100
  listing digests were distinct, and 0 daemons survived unmount. Mount latency
  under that contention was p50 31.6 s / p95 32.5 s / max 32.6 s; wall to
  100-mounted-and-verified was 100.0 s; daemon RSS sum 1.62 GiB and cgroup peak
  1.56 GiB, inside the unchanged 3 GiB court limit.
- **1,000 concurrent mounts (scale court, explicit overrides):**
  `WEFT_CONTEXT_MOUNTS=1000` builds 2,000 corpus files, 250 models, 10 grounding
  docs, 5,999 typed relations, and 1,000 distinct Context Roots (500 `by:task`,
  250 `by:model`, 250 `by:derived`), spawns 500 Rust + 500 Go daemons before
  awaiting any, and verifies every mount: listing digest against the corpus
  construction oracle (all 1,000 distinct), every one of 4,250 mounted files
  against its declared SHA-256, one `weftfs cat` byte comparison per mount,
  `context resolve` id-for-id on every 50th mount, read-only denial, clean
  unmount, no survivors. With `24g / 8 CPUs / 4,096 pids / 4,096 nproc` the
  court **cannot** reach 1,000: 747 daemons spawned, 692 mounted, 55 mount
  failures, 107 cgroup OOM kills and 98 pids-limit fork refusals, 156 daemons
  lost — 400 is the largest N tested that passes every step at that budget.
  With `52g / 8 CPUs / 16,384 pids / 16,384 nproc` **1,000/1,000 mounts pass
  twice** (1,000 distinct digests, 4,250 file hashes, 1,000 cat comparisons,
  1,000 clean unmounts, 0 survivors; `semantic_match=true`), at mount latency
  p50 197.0 s / p95 330.2 s / max 331.7 s, 1,785.8 s to all-mounted-and-verified,
  46.44 GiB daemon RSS sum (max 80.5 MiB per daemon), 6,751 threads, cgroup peak
  40.92 GiB. Above 100 mounts the corpus is built by
  `examples/context_mount_corpus.rs` through the shipped store API with group
  commits (the corpus is byte-identical to the CLI fixture; one commit per
  operation would make commit-chain replay, not mounting, the measured cost).
  Every limit override is recorded in the receipt; none is disabled. Details,
  the per-daemon memory curve, and the exact failure modes are in
  `bench/RESULTS-context-mounts-1000.md`.
- **Ambiguity:** `require_one` now re-evaluates without pagination and lists
  every competing ObjectId in the `Ambiguous` message. `context materialize
  --require-one` checks cardinality before any filesystem work and leaves no
  output path on failure.
- **Replay bundle:** Rust `export-bundle` writes `weft-receipt.tar.zst` with the
  portable store, the complete relation graph including Context Root records,
  `manifest.json` (ContextRootIds, snapshot/selector ids, content hashes,
  source revisions, court command), `contexts.tsv`, `checks.sha256`,
  `materialized.sha256`, `replay.sh`, and a README. Go extracts it with
  `tar`/`zstd`, opens it, runs `verify --full`, resolves and materializes every
  Context Root, and re-hashes every store entry and output file against
  `manifest.json` using a stdlib-only verifier. No Rust runs at Go replay time.
- **Evidence:** `scripts/docker-court.sh fuse "cargo test --features fuse
  --all-targets"` reports **81 passed, 0 failed, 2 ignored** (unchanged
  CAP_SETUID and kernel-7.3 limitations). `scripts/docker-court.sh fuse "cd
  /work/WeftFS-Go && go test -race ./..."` passes for `store` and `fusefs`,
  including four new context-mount/time-travel/concurrency tests and an
  ambiguity message test; `go vet ./...` and `CGO_ENABLED=0 go build ./...`
  pass.
- **Demo receipts (all `pass`):**
  - `bench/receipts/context-mounts-20261103T210625Z.json` (6/6) — three
    simultaneous mounts, per-file SHA-256 and ContentId agreement, one
    content-addressed manifest shared by two contexts, read-only denials, and a
    live writer while mounted.
  - `bench/receipts/ambiguity-20261103T210633Z.json` (4/4) — full ordered set,
    `Ambiguous` with both ids, two-result mount listing, no singular output.
  - `bench/receipts/time-travel-20261103T210633Z.json` (4/4) — 100 versions,
    snapshots 1/50/100 mounted together, exact historical bytes after a live
    rename/retag.
  - `bench/receipts/context-mounts-20261104T125344Z.json` (5/5) — scaled
    `WEFT_CONTEXT_MOUNTS=100`: 100 concurrent mounts, 425/425 file checks,
    100/100 distinct listing digests, 100/100 clean unmounts, 0 survivors;
    determinism `context-mounts-20261104T125344Z.determinism.txt` =
    `semantic_match=true`.
  - `bench/receipts/context-mounts-20261105T122643Z.json` (5/5) and
    `bench/receipts/context-mounts-20261105T131116Z.json` (5/5) —
    `WEFT_CONTEXT_MOUNTS=1000` at `52g / 8 CPUs / 16384 pids / 16384 nproc`:
    1000/1000 mounts, 4250 file SHA-256 checks, 1000 distinct listing digests,
    1000 cat comparisons, 20 sampled resolves equal to the construction oracle,
    1000 clean unmounts, 0 survivors; determinism
    `context-mounts-20261105T131116Z.determinism.txt` = `semantic_match=true`.
    Analysis in `bench/RESULTS-context-mounts-1000.md`.
  - `bench/receipts/context-mounts-20261105T135805Z.json` (5/5) — the same
    court at `24g / 8 CPUs / 4096 pids / 4096 nproc` with 400 mounts: the
    largest N tested that passes every step at the directed budget.
  - `bench/receipts/context-mounts-20261105T145819Z.json` (3/5) — 500 mounts at
    the directed budget: all 500 mounted and all file/cat checks passed, but 4
    Go daemons died mid-run (transient thread spikes), so 496 unmounted cleanly.
  - `bench/receipts/context-mounts-20261105T140342Z.json` (2/5,
    `INCOMPLETE 692/1000`) — the directed budget at 1,000 requested: 747
    spawned, 692 mounted, 55 mount failures, 107 OOM kills, 98 pids-limit
    refusals, 156 daemons lost. Records the exact break point.
  - `bench/receipts/context-mounts-20261105T150653Z.json` (5/5) —
    `WEFT_CONTEXT_MOUNTS=100` in the unchanged default court
    (`3g / 4 CPUs / 1024 pids`).
  - `bench/receipts/time-travel-20261104T125947Z.json` (6/6) — scaled
    `WEFT_TIME_TRAVEL_VERSIONS=10000`: 10,000 sequential versions, 101
    snapshots, 10 mounted at once (`s1`, `s1100` … `s10000`), 164.1 s ingest
    (60.9 versions/s), reopen p50 6 ms, mount p50 2.28 s, verification 77 ms,
    all snapshots byte-identical after a later write and live rename;
    determinism `time-travel-20261104T125947Z.determinism.txt` =
    `semantic_match=true`.
  - `bench/receipts/replay-bundle-20261103T210824Z.json` (4/4) — bundle
    sha256 `940579db909ba8a3360f73377448ce90df455278a37c5a0b02f6bb7a584bdbd2`,
    17 store entries, 2 contexts, 3 context files, 2 resolved identities, zero
    mismatches.
  - `bench/receipts/context-mounts-determinism.txt` — `semantic_match=true`
    between two Context-mount runs (run ids and wall-clock fields excluded).
- **Bundle:** `bench/bundles/weft-receipt.tar.zst` (6,662 bytes), sha256
  `940579db…`. Reproduction:
  `scripts/docker-court.sh fuse "bash scripts/replay-bundle.sh --bundle
  bench/bundles/weft-receipt.tar.zst --out /tmp/replay-out"`.
- **Not claimed:** io_uring, EKBLOOM, or any acceleration. Time travel is
  correctness-only. The SHA-256 value in the demos is a declared attribute and
  a byte-identity check; the normative content root is the store's BLAKE3
  ContentId, which `verify --full` re-checks (the scaled modes run it before
  mounting). The scaled receipts are labelled single-host Docker FUSE court,
  synthetic deterministic fixtures, local court bind-mount store, explicit
  court limit overrides recorded in the receipt, and **not a fleet claim**. The
  100-mount run uses the unchanged default court (`3 GiB / 4 CPUs / 1,024
  pids`); the 1,000-mount run raises the hard limits to `52g / 8 CPUs / 16,384
  pids / 16,384 nproc` and the ceiling run uses `24g / 8 / 4,096`. No limit is
  ever disabled, and 1,000 is **not** claimed to fit the default court: at the
  directed 24 GiB / 4,096 budget the court reached 692 mounted and a complete
  pass at 400. Mount latency includes CPU contention among all daemons starting
  at once; it is not a per-mount steady-state cost. Each receipt records its
  exact revisions: the context mount, ambiguity, and time-travel default runs
  used WeftFS `f3d1bc1` / WeftFS-Go `acf6df6`; the replay run used WeftFS
  `bcedd26` / WeftFS-Go `7693ad4`; the 2026-11-04 scaled runs used WeftFS
  `6316fd4` / WeftFS-Go `fce700a`; the 2026-11-05 1,000-mount and ceiling runs
  used WeftFS `6e4aea1` / WeftFS-Go `8372e6f`.

### Twelve-step showcase — implemented, all twelve pass

- `scripts/showcase.sh` executes the mandated twelve scenarios inside the FUSE
  court and writes a schema-versioned receipt plus a raw console log:
  `bench/receipts/showcase-20261103T171440Z.json` and `.log` (all twelve
  `pass`; schema `weftfs.showcase.receipt/1`).
- A second run under a different receipt name compared every field except
  wall-clock timestamps: `semantic_match=true`
  (`showcase-determinism-20261103T171440Z.determinism.txt`).
- Every step records its exact commands, fixture hashes, machine-checked
  assertions (`cmp`, SHA-256, JSON field equality), observed outputs, court
  limits, image/kernel/tool versions, and repository revisions (WeftFS
  `4cb40af`, WeftFS-Go `74c8c90`, WeftCodec `a60d9f5`, WeftCodec-Go `b2beeab`).
- Step 9 is explicitly labelled a **process-fault proxy** (`WEFT_CRASH_PHASE`),
  not physical power loss. Step 10 records that the codec CLI has no remote
  fetch policy; missing/mismatched dependencies fail closed, while
  `DependencyDenied` remains an API-level class.
- The codec-cycle fixture generator is `examples/codec_cycle_probe.rs`; the
  real `weftcodec archive create` CLI is what rejects the forged cycle.
- Deterministic fixtures use fixed ObjectId seeds and a fixed clock; a re-run
  with `SHOWCASE_COMPARE_WITH` must match on every field except wall-clock
  timestamps.

### P7 benchmark court — implemented, first measured results

- `docker/bench.Dockerfile` and `scripts/docker-court.sh bench` add a
  FUSE-capable, OOM-limited benchmark court with `zstd`, `xz`, `gzip`,
  `brotli`, `sqlite3`, `fio`, Rust, and Go. Unknown court kinds are still
  rejected and the memory/swap/pids/CPU limits are unchanged.
- The existing `fuse` court was re-run after the runner change at `244dc26`:
  `fuse_posix` reported 9 passed and 1 ignored (the ownership test needs
  `CAP_SETUID`). The added `/mnt/weft-bench` tmpfs does not change the POSIX
  result.
- Codec baselines (`bench-codec-20261103T175847Z.json`): external zstd 3/9/19,
  gzip 9, xz 9, and brotli 9 on seven deterministic corpora, plus WeftCodec
  RAW/ZSTD/DELTA/DICTIONARY/auto with marginal and whole-object accounting.
  Every decode is verified before its sample is accepted.
- Store benchmark (`bench-store-20261103T175920Z.json`): 100/1,000/10,000
  objects, 1–3 relations each, snapshots, selectors, churn, full verify, GC,
  and on-disk bytes against the real Rust store API. Peak RSS 169 MiB.
- Filesystem benchmark (`bench-fs-20261103T180751Z.json`): `fio` against a
  Rust WeftFS mount, a Go WeftFS mount, and a tmpfs reference in one container.
  The Go mount completed reliably (`go_status: ok`). The tmpfs column is
  reference-only and explicitly non-equivalent.
- Headline scoped observations (not claims of superiority):
  - WeftCodec ZSTD-3 is 545 bytes on 4 MiB repetitive data and RAW is
    4,194,404 bytes on 4 MiB random data; `auto` selects accordingly.
  - DELTA/DICTIONARY do not reduce whole-pair bytes when the required base is
    counted (near-dup 1 MiB: 1,128,560 / 1,073,309 vs 1,048,709 for ZSTD-3).
  - Store relation/tag queries cost ~414–424 ms at 10,000 objects because
    evaluation scans all relations per object; the n-result kind query costs
    1.93 ms. Replay is 32.6 s and full verify 70.3 s at 10,000 objects.
  - Filesystem writes rewrite the whole file per write: ~20 IOPS for 1 MiB
    sequential writes and ~73 IOPS for 4 KiB random writes (Rust), with
    ~7.6 ms p50 for a 4 KiB write with `fsync`.
  - A 1,000-object ingest measurement varied by 86% between runs; the range is
    reported rather than a single point.
- `bench/scripts/summarize_bench.py` regenerates the tables and
  `bench/scripts/compare_bench.py` compares two receipts for stability.
  `bench/RESULTS-v1.md` publishes every loss and the explicit gaps.

### P7 comparative pass — external SQLite, matched codec, cold-cache fio (implemented)

- External context/query baseline: `bench-external-20261104T000122Z.json`
  (plus corpus manifest `external-corpus-20261104T000122Z.json`). Schema-indexed
  SQLite relational and adjacency-table variants at 100/1,000/10,000 objects
  using the exact `bench_store` payloads, ObjectIds, relations, snapshots, and
  generations. Warm and cold (per-file page-cache eviction) samples, query
  plans, churn, index build time/bytes, and bytes on disk are recorded. The
  adjacency variant is labelled “SQLite relational/adjacency baseline”; no
  Neo4j/SPARQL/dedicated graph database was run.
- Same-revision store baseline for the comparison: `bench-store-20261103T235127Z.json`
  at HEAD `2388352` (487.9 s wall, peak RSS 169,356 KiB), 100/1,000/10,000
  objects, with the query/churn/snapshot/open/bytes medians used in
  `bench/RESULTS-comparative-v1.md` §3.4.
- Matched Rust/Go WeftCodec court: `bench-codec-matched-20261103T232158Z.json`
  (first run `T225536Z` retained for stability). RAW/ZSTD/REFERENCE/DELTA/
  DICTIONARY/TENSOR/TOKENS as CLI processes, medians and p95, CPU user/sys,
  max RSS, frame bytes, per-iteration decode verification, and cross-decode in
  both directions. Process overhead measured separately (Rust 0.326 ms, Go
  0.387 ms) and not subtracted; no library-vs-library comparison is claimed.
- Cold-cache concurrent fio: `bench-fs-cold-20261103T232216Z.json` (stability
  run `T233740Z`). QEMU guest with a genuine guest
  `sync; echo 3 > /proc/sys/vm/drop_caches` cold run, `numjobs` 1/4/16,
  ext4/XFS plus Rust and Go WeftFS over FUSE, two repetitions per cell. The
  Rust and Go mounts differ in `allow_other`/`max_read` defaults; that is
  published as a limitation.
- Whole-disk ZFS and Btrfs (2026-11-04):
  `bench-fs-cold-20261104T071922Z.json` runs ext4/XFS/Btrfs/`zfs-whole` in one
  guest court, one repetition per cell, 30 fio runs per target. For `zfs-whole`
  the guest passed the raw `/dev/vdb` block device to `zpool create -f -m
  /data/tank tank /dev/vdb`; `zpool status -P` shows the GPT partition
  `/dev/vdb1` that ZFS created and owns. No partition, loop device, or
  file-backed vdev is used. The guest runs stock `systemd-udevd 252.39` so
  libzfs' libudev partition wait can complete; without udevd the exact error is
  `cannot label 'vdb': failed to detect device partitions on '/dev/vdb1': 19`.
  OpenZFS 2.1.11-1+deb12u1 (DKMS, kernel 6.1.0-54-amd64); Btrfs uses
  btrfs-progs 6.2 on the host and the guest `btrfs.ko`. Scoped `numjobs=1` cold
  medians: seq-write-1m ext4 1,142.9, XFS 1,142.9, Btrfs 1,142.9, ZFS 1,333.3;
  fsync-write-4k ext4 410.3, XFS 390.2, Btrfs 457.1, ZFS 800.0; rand-read-4k
  ext4 9,846.2, XFS 9,846.2, Btrfs 8,533.3, ZFS 128,000 (ARC-served). At
  `numjobs=16` fsync-write-4k: ext4 3,160.5, XFS 1,855.1, Btrfs 911.0, ZFS
  6,736.8. ZFS reads are **not cold**: `drop_caches` does not evict the ARC;
  ranking or superiority claim is made. The earlier file-backed ZFS run
  `bench-fs-cold-20261104T014727Z.json` is retained as a separate, labelled
  result, not as the whole-disk claim.
- Block-layer lying-disk matrix (2026-11-04): `bench/receipts/blkdebug-20261104T072008Z.json`,
  30 iterations, 5 per mode, in the vm court. `dm-flakey drop_writes` (2 s up,
  then silent drop) lost acknowledged commits in 5/5 iterations: recovered
  generations 87/89/89/88/89 versus last returned 319/218/334/292/246
  (232/129/245/204/157 commits lost), 0 partial, both implementations agreed,
  `verify --full` clean. `dm-flakey error_writes` failed the first post-window
  write in 5/5, recovered exactly the last returned generation (88/79/8/7/6),
  0 loss, 0 partial. QEMU `snapshot=on` total write drop recovered generation 2
  in 5/5 and lost all acknowledged commits (last returned 52/77/205/182/128).
  QEMU blkdebug write-error and flush-error were attempted and are **blocked at
  ext4 mount** in 10/10: the first injected write is the mount superblock at
  block 0, then `JBD2: recovery failed` / `EXT4-fs (vdb): error loading
  journal`, and the guest never reaches `GUEST_WRITING`. No result is invented
  for those two modes. This is a block-layer lying-disk model, not physical
  media power loss.
- Host page-cache attempt (2026-11-04):
  `bench/receipts/hostcache-probe-20261104T072314Z.json`. The unprivileged court
  (`CapEff=0`) cannot write `/proc/sys/vm/drop_caches`: mode `200`, exact
  refusal `Read-only file system`; no host cache was dropped. A per-file
  `posix_fadvise(POSIX_FADV_DONTNEED)` on one 4 MiB scratch file succeeded and
  is recorded as per-file eviction only. QEMU `cache=none` uses `O_DIRECT`, so
  guest data does not occupy the host page cache on that path. Host page-cache
  loss remains **not tested**.
- Energy/counter probe: `energy-probe-20261103T225547Z.json` recorded that
  `perf` was not installed and no readable RAPL `energy_uj` existed. The
  2026-11-04 probe `perf-counters-20261104T015026Z.json` installs Debian
  `linux-perf 6.1.190-1` inside the ephemeral bench court and records
  cycles/instructions/branches/branch-misses/cache/context-switch/page-fault
  counters for RAW/ZSTD encode and decode (3 repetitions, all runs complete).
  RAPL is still unavailable: `/sys/class/powercap` exists but no `energy_uj`
  file is visible inside Docker (`/sys/devices/virtual/powercap` is masked).
  The VM guest reports `powercap present`, `energy_uj files 0`, `perf absent`.
  CPU time and perf counters are **not** called energy; no joule/watt number is
  reported.
- Analysis: `bench/RESULTS-comparative-v1.md` contains the exact commands,
  environments, revisions, tables, every loss, stability deltas, and explicit
  limits. Scoped examples: SQLite point lookups are ~1.2–1.8 µs warm at 10,000
  versus ~457.6–494.4 ms for the store’s relation/tag selectors; WeftFS
  sequential 1 MiB writes are 9.9–12.1 IOPS (Rust) and 2.4–3.2 IOPS (Go)
  versus 1,143–2,784 (ext4) and 1,333–2,702 (XFS); Rust `fsync`-write p99
  reaches 1.13 s at `numjobs=16`. No winner or world-record claim is made.

### F5 — proof courts (B1–B4)

The F5 code revision is parent `9d767d5`, `WeftCodec` `127bc85`,
`WeftCodec-Go` `71066cf`; `WeftFS-Go` remains `7693ad4` (no F5 code change).
All three F5 receipts below were generated at those revisions after the code
commit, in the unchanged hard-limited courts.

**B1 — decoder bombs (36/36 pass).** `bench/receipts/decoder-bombs-20261103T213314Z.json`
and its raw `.log`. 36 committed adversarial vectors in both codec repos
(byte-identical mirror checked by the court). Both CLIs were run twice per
vector under `timeout 10s /usr/bin/time`; every run exited nonzero with the
expected class in both implementations, no output file, no timeout, no
OOM-kill (137), stable classes across repeats, and peak RSS below 5 MiB.
Class matrix: LimitExceeded 15, Malformed 12, Corrupt 2, DependencyCycle 2,
UnsupportedFeature 2, NonCanonical 1, UnsupportedMode 1, DependencyMismatch 1.
The tiny-body/4 GiB-declared vector is the regression for the fixed zstd
preallocation bug; Rust and Go both report Corrupt with ~3.5/4.7 MiB RSS.
Limits are configurable through the ten requested `decode` flags (plus
structured-item, selector-depth/operand, and archive-entry bounds); defaults
remain the normative spec values.

**B5 — deterministic differential fuzz (1,000,000 inputs; 0 mismatches).**
`bench/receipts/fuzz-diff-20261104T132310Z.json` (direct run) and
`bench/receipts/fuzz-diff-20261104T132413Z.json` (inside `verify-all`). A shared
seed corpus of 44 valid WFT1 frames (RAW, ZSTD, REFERENCE, DELTA, DICTIONARY,
STRUCTURED, TENSOR, TOKEN_SEQUENCE, QUANTIZED_TENSOR, chunked) lives in
`conformance/fuzz/v1/seeds.txt`. Identical xorshift64* generators in Rust
(`WeftCodec/examples/fuzz_differential.rs`) and Go
(`WeftCodec-Go/cmd/fuzzdiff`) produce 1,000,000 inputs with a fixed split:
1/8 raw random bytes (0–2048), 1/8 un-repaired seed mutations (1–8 legacy
operations), 4/8 repaired body/table mutations (1–4 operations with `body_len`,
header CRC32C, and trailer CRC32C recomputed; content hash deliberately left
stale), 1/8 semantic faults (missing/lying/denying dependency, self-cycle
REFERENCE through the DAG entry point, mode bytes 9/127/128/255,
`decoded_len = u64::MAX`, bad chunk extent, wrong content hash, unknown
critical flag, non-canonical STRUCTURED body), and 1/8 clean seeds. Both
decoders ran with `Limits::default()` inside the unchanged hard-limited courts;
corpus hash `4ed6da22ac497a2494a56d53abda1689eef8a5bcbbed381b64f2c3fecf6e12a5`
(BLAKE3-256 over `LE64(len) || input` per input) matched before comparison.

Normative zstd preamble rule (WEFTCODEC-FORMAT §3, applied identically in Rust
and Go to ZSTD, DICTIONARY, and every chunked ZSTD chunk): the payload MUST be
a complete well-formed zstd stream; an empty payload is `Malformed` even when
`decoded_len == 0`; if the zstd frame header declares a content size it MUST
equal `decoded_len` (`Corrupt` otherwise, even when the actual byte count
matches); and the actual decompressed length MUST equal `decoded_len`
(`Corrupt` otherwise). Both languages enforce this with existing dependencies:
Rust `zstd::zstd_safe::get_frame_content_size` and Go `zstd.Header.Decode`
(`HasFCS`/`FrameContentSize`) from klauspost/compress v1.17.11. The Go encoder
now sets `zstd.WithZeroFrames(true)` so empty plaintext emits a valid minimal
frame instead of an empty body. New reject vectors
`038_zstd_empty_body` (`Malformed`) and
`039_zstd_content_size_mismatch` (`Corrupt`) are byte-identical in both repos.

Result lines: 1,000,000 each, 0 panics, **0 mismatches**, identical class
distributions: ok 172,294, Corrupt 174,899, Malformed 466,548, NonCanonical
67,975, DependencyMissing 35,987, LimitExceeded 16,900, DependencyMismatch
14,081, UnsupportedMode 12,608, UnsupportedFeature 12,347, DependencyCycle
12,349, DependencyDenied 12,313, UnsupportedVersion 1,699. Peak RSS 6,304 KiB
(Rust) and 205,888 KiB (Go), inside the 256 MiB bar; elapsed 1,205 ms and
1,964 ms, total 7,000 ms. The historical failing receipt
`bench/receipts/fuzz-diff-20261104T102754Z.json` (544 mismatches: 451 Rust ok /
Go Corrupt on a zstd frame declaring content size 2 while yielding 0 bytes, 93
Rust Corrupt / Go ok on an empty zstd body with `decoded_len = 0`) is retained
as the evidence that motivated the rule. The court is not coverage-guided
fuzzing and not a substitute for the curated B1 decoder-bomb vectors. Wired
into `scripts/verify-all.sh`; `verify-20261105T035513Z.json` passes all 25
courts including `claims`, the authenticated-capability gates, and `fuzz_diff`.

**B4 — randomized POSIX torture (6/6 pass; 1,202,560 completed operations).**
Three FUSE-court invocations at WeftFS `42c82f7` and WeftFS-Go `d7d8d06`, same
`weft/fuse-court:1` image, all six writer/reader configurations (seeds 1–3 ×
both directions): **2,640,000 requested / 1,202,560 completed / 1,437,440
skipped by design**, 9,315,108 ms of torture wall (2 h 35 min 15 s), **129.10
completed ops/s** (283.4 requested ops/s).

| Seed | Direction | Requested | Completed | Skipped | Entries | Manifest SHA-256 (writer = reader) |
|---|---|---:|---:|---:|---:|---|
| 1 | rust→go | 240,000 | 112,098 | 127,902 | 96 | `5a5c73ea32f98d7a3995d76b535758217a9e9889fde29739aebd51128ff244bf` |
| 1 | go→rust | 640,000 | 298,004 | 341,996 | 97 | `05b5b5b82f5b1882584e71b2f5c2606cf922dd6de099d1b6ab3a61468b2332af` |
| 2 | rust→go | 240,000 | 112,386 | 127,614 | 97 | `dc82c98d33d46158d08abd8c510eb3a9d8a354d73b642b3001f25e44e9b798d0` |
| 2 | go→rust | 640,000 | 270,201 | 369,799 | 96 | `1aa7fa3c7d6a843075b9f01e1e4d04383d69c0f3eab65301680beefb2c52f679` |
| 3 | rust→go | 240,000 | 112,025 | 127,975 | 97 | `3f16185c83686d80edb64d365195f179b9247ac5b114186bcbb6a4f6e616d1da` |
| 3 | go→rust | 640,000 | 297,846 | 342,154 | 97 | `81422cb2944b6ca2963319365ba06885b657565da371b62ae0bd8a0a315fa299` |

The three receipts are `bench/receipts/posix-torture-20261104T162109Z.json`
(rust→go, seeds 1–3: 336,509 completed of 720,000 requested, 3,638,847 ms),
`bench/receipts/posix-torture-20261104T172156Z.json` (go→rust, seeds 1–2:
568,205 completed of 1,280,000 requested, 3,807,418 ms), and
`bench/receipts/posix-torture-20261104T182537Z.json` (go→rust, seed 3:
297,846 completed of 640,000 requested, 1,868,843 ms), each with its raw `.log`
and `.results.tsv` beside it. Totals were recomputed independently from the six
raw per-run result JSON files and the six manifest files, not from the harness
summary: in every run `completed + skipped = requested`, `op_counts` sums to the
requested count, every manifest SHA-256 was recomputed from its entries and
equals both recorded digests, and all six reader mounts reproduced the writer's
exact manifest. Every configuration passed all assertions (per-operation bytes,
size, mode, nlink, symlink target, xattrs; a full-tree check every 100
operations; `verify --full` after unmount; writer/reader manifest equality).
Operations cover create/write/read/truncate/append/rename/rename-over/link/
unlink/mkdir/rmdir/symlink/readlink/xattr/fsync/open-unlinked.

The size asymmetry is deliberate and is a real product finding from the run
itself. At that revision the Go store's `Open` (`store.CommitIndex` →
`LoadStateAt`) parsed every commit file and had no bounded open: three
consecutive probes measured 18.65–19.07 s to mount a store with 287,759 commits,
while Rust mounted the same store in 305 ms. The harness's mount wait is a fixed
15 s (300 × 50 ms), so Go-reader runs are capped at 240,000 requested (~133k
commits, ~9 s mount) while Rust-reader runs use 640,000. The retained failing
receipt `bench/receipts/posix-torture-20261104T153125Z.json` is the
520,000-request rust→go attempt that completed 242,422 operations and then
exceeded the 15 s wait at 287,759 commits; its step store is not kept. No product
code, assertion, or harness default was changed. **Update 2026-11-04:** Go now
implements the same bounded replay (`WeftFS-Go` `1461c04`,
`bench/receipts/go-fast-open-20261104T190732Z.json`: 100,000 generations open in
2.80 ms / 162 commit reads versus 2155.65 ms / 100,001 reads full-scan). The
numbers above were measured before that change and the court sizes were left
unchanged, so they remain a record of the old path, not of current behaviour.

Caveat (unchanged): this is a self-written fsx-style harness in a single-host
Docker court, not `pjdfstest`/`fsstress` (not installed in the court; no
substitute run is claimed), and it is not a fleet claim. The earlier
1,000,002-request run (`bench/receipts/posix-torture-20261104T142424Z.json`,
453,432 completed) and the 13,694-operation run
(`bench/receipts/posix-torture-20261103T213317Z.json`) are retained as history;
the harness still accepts `WEFT_POSIX_OPS`, `WEFT_POSIX_OPS_MINIMUM`,
`WEFT_POSIX_SEEDS`, and `WEFT_POSIX_ONLY` with unchanged defaults (3,200 /
2,000 / all seeds / all six configurations).

**B2 — crash-consistency torture (100,000/100,000 pass).**
`bench/receipts/crash-torture-20261104T104233Z.json` (18 MB; log and TSV stored
losslessly as `.zst`). The harness uses a fresh three-generation store per
iteration, a pre-generated payload pool, and a clean fast path for
`verify --full`; the old-or-new/partial and phase-specific assertions are
unchanged (`WEFT_CRASH_FRESH_STORE=0` retains the historical growing-store
mode). Result: 100,000 iterations, 100,000 completed, 0 failures, 0 bad verify
states; observed old 66,667 / new 33,333 / partial or missing 0; verify clean
66,667 / accepted unpublished-tail 33,333; phases 33,334/33,333/33,333;
5,196,158 ms (86 min 36 s), **19.2 iterations/s**. Every iteration was
recomputed from the 14,822,217-byte NDJSON independently of the harness
counters. The 10,000-iteration receipt `crash-torture-20261104T013521Z.json`
is retained. Alternating Rust/Go writers and readers; every
crash followed by the other implementation's `verify --full`, exact `/f` byte
check, and exact `/` listing. This remains a **process-fault proxy**, not
physical power loss. The earlier 1,000-iteration receipt
`crash-torture-20261103T213446Z.json` (2.9 it/s) is retained as history.

**B3 — receipt bundle.** `scripts/package-receipts.sh` writes
`bench/bundles/receipts-<stamp>.tar.zst` with manifest, checksums, revisions,
receipts/raw logs, conformance corpus, harness/Dockerfiles, and dmesg status.
`scripts/verify-receipts.sh <bundle> --repo-root .` extracts to a private
temporary directory and independently verifies every sha256, rejects unlisted
files, checks all four revisions against git, and rejects unsafe tar entries.
The bundle records `dmesg: unavailable (no CAP_SYSLOG)` when applicable.

### F9 — machine-checked commit/recovery model and VM power-cut court (implemented 2026-11-03)

**Model checking.** `verification/tla/WeftCommit.tla` is a bounded TLA+
abstraction of the commit/recovery state machine: immutable commit records,
two alternating superblock slots, objects that must be durable before a commit,
arbitrary crash boundaries, recovery scanning for the highest valid generation,
and replay. TLC 1.7.4 (jar pinned by sha256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`) checks
`TypeOK`, `NoPartial`, `HighestValid`, `NoLostDependency`, `Durability`,
`ReturnedObjectsDurable`, and `ReplayIdempotent` at `MaxGen=8`: 615 states
generated, 591 distinct, search depth 68, no error. Receipt
`bench/receipts/tla-20261103T224446Z.json`.

`verification/tla/WeftCommitBuggy.tla` is deliberately flawed: recovery trusts
the highest commit file, ignoring superblock visibility and CRC validation.
TLC finds the expected `NoPartial` violation at `MaxGen=3` (32 generated, 29
distinct, depth 9). The counterexample is
`verification/tla/WeftCommitBuggy.counterexample.txt`; it shows a torn generation-1
commit replayed as an operation prefix.

**Power-loss rig.** `scripts/powerloss_court.sh` (new `vm` Docker court,
`docker/vm.Dockerfile`) builds both CLIs, a read-only raw ext4 root disk with a
Debian 6.1.0-54 kernel/initramfs, and a fresh qcow2 data overlay per iteration.
The guest initialises the store and commits 64 KiB writes in a loop without
`sync(1)`; the host waits for returned commits, sleeps a seeded pseudo-random
delay, and `SIGKILL`s QEMU. Reboot uses the same image with ext4 journal replay
and **no fsck**; Rust and Go independently recover, `verify --full`, and read
the file. The host asserts identical generation and snapshot root, exact
expected content, and that every returned commit survived. A never-fsynced
control file on the data disk distinguishes this from a process abort.

Published primary run (`cache=none`, KVM): 30/30 pass, 28 old states, 2 new
states, 0 partial, 0 durability failures, 0 Rust/Go disagreements, 1 accepted
documented unpublished tail, unsynced-control loss 30/30. Receipt
`bench/receipts/powerloss-20261103T224453Z.json`. Secondary writeback run:
10/10 pass, 0 unpublished tails, control loss 10/10; receipt
`bench/receipts/powerloss-20261103T224652Z.json`. The acceptance predicate
allows only the documented `above the highest published superblock generation`
note or an unparseable commit file strictly above the recovered generation;
all other verifier problems fail the iteration.

**Lying-fsync device model (2026-11-04).** `POWERLOSS_CACHE_MODE=unsafe` uses
QEMU's `cache=unsafe`, which accepts guest flush requests and does not honour
them. The same SIGKILL/reboot/verify matrix ran 30 iterations
(`bench/receipts/powerloss-20261104T014805Z.json`): 30/30 pass, 27 old, 3 new,
0 partial, 0 durability failures, 0 Rust/Go disagreements, unsynced-control
loss 30/30. **No data loss was observed.** That is the honest result and it has
an exact explanation: SIGKILL of QEMU discards the guest page cache and QEMU
state, but the host page cache holding QEMU's already-written image data is not
removed, so a lying guest-visible flush does not create loss under a process
kill. The mode is labelled a **lying-fsync device model**, not physical media
power loss; real disk-firmware lying and host power loss remain untested.

**Block-layer lying-disk court (2026-11-04).** `scripts/blkdebug_court.sh`
prepares one clean ext4 store base, copies it per iteration, and combines a
block-layer fault with the same SIGKILL/reboot/Rust+Go verification. Modes are
`control`, `dm-flakey-drop`, `dm-flakey-error`, `write-drop-all` (QEMU
`snapshot=on`), and two blkdebug attempts. `dm-flakey` is created inside the
guest over `/dev/vdb` with a 2 s up interval so mount completes, then a 1 h down
interval; `drop_writes` silently discards writes while returning success, and
`error_writes` returns EIO. In `bench/receipts/blkdebug-20261104T072008Z.json`:
control 5 old; dm-flakey-drop 5/5 lost (232/129/245/204/157 acknowledged
commits lost, recovered 87/89/89/88/89); dm-flakey-error 5/5 old, 5/5
COMMIT_FAIL, 0 loss; write-drop-all 5/5 recovered generation 2 and lost all
acknowledged commits; 0 partial in all completed modes. blkdebug write-error
and flush-error are 10/10 **blocked**, not passed: blkdebug arms at open, so the
first injected write is ext4's mount superblock (`I/O error, dev vdb, sector 0`,
`JBD2: recovery failed`, `EXT4-fs (vdb): error loading journal`) and the guest
never reaches `GUEST_WRITING`. QEMU 7.2 blkdebug accepts `read_aio`,
`write_aio`, and `flush_to_os`; it rejects `flush_do_sync`, and `errno=0` still
writes, so there is no silent selective write-drop primitive. `dm-flakey` is
the arm-after-mount equivalent used for the data results.

**Host page-cache probe (2026-11-04).**
`bench/receipts/hostcache-probe-20261104T072314Z.json` records that the
unprivileged court (`CapEff=0`) cannot write `/proc/sys/vm/drop_caches` (mode
`200`, exact refusal `Read-only file system`), so no host cache was dropped. A
per-file `posix_fadvise(POSIX_FADV_DONTNEED)` on one 4 MiB scratch file
succeeded and is labelled per-file eviction only. QEMU `cache=none` uses
`O_DIRECT` for guest data. Host page-cache loss remains untested.

**Claim boundary.** This is a QEMU VM power-cut proxy plus a lying-fsync device
model and a block-layer lying-disk model, not physical media power loss or host
page-cache loss. The TLA+ model is checked; the implementation is not verified,
and there is no refinement proof. See `verification/README.md` for the exact
mapping, assumptions, and remaining gaps (checkpoints, GC, capabilities,
multi-writer, physical rig).

### E — Independent Python reader and PyTorch dataset (implemented 2026-11-04)

`python/` ships `weftcodec` 0.5.0 (Apache-2.0) as a **third independent
implementation** with no Rust or Go extension modules. It provides canonical
WCB encode/decode, WFT1 header/table parsing with CRC32C and strict structural
validation, domain-separated BLAKE3 content roots, and exact decoders for RAW,
ZSTD, REFERENCE, DELTA, DICTIONARY, STRUCTURED, TENSOR, TOKEN_SEQUENCE, and
QUANTIZED_TENSOR. Modes 9–14 (`FLOAT_TENSOR`, `TENSOR_SHARDED`, `WEIGHT_DELTA`,
`KV_CACHE`, `ACTIVATION_STREAM`, `SEMANTIC_DELTA`) raise the typed
`UnsupportedMode`; there is no approximate decode and no opaque-body fallback.
`weftcodec.torch.WeftMapDataset` walks a mounted tree in sorted POSIX-relative
order and returns `(path, bytes)` or, when `dtype` is supplied,
`torch.frombuffer` tensors; `torch` is an optional extra and is never imported
at module import time.

**Court and evidence.** New minimal image `weft/python-court:1`
(`docker/python-court.Dockerfile`, from `python:3.12-slim`; image id
`sha256:67d4cf7ba67c9c2e3c6229e271b1b012a61e909852f422da0852bfaa755797e0`),
wired as the `python` mode of `scripts/docker-court.sh` with the same hard
limits as every other court (3 GiB memory/swap, 1024 pids, 4 CPUs, 1 GiB
`/tmp`, `no-new-privileges`, `cap-drop=ALL`). Receipt
`bench/receipts/python-bindings-20261105T000147Z.json` with its raw `.log`
(WeftFS `1c4b249`, WeftCodec `f1d32e3`, kernel `7.3.1-1-cachyos`).

**Counts (from the receipt, not estimated).** 111 frame vectors discovered; 61
asserted, 61 passed, 0 failed. The 50 skips are explicit and labelled: 7
historical `superseded_by` vectors and 43 frames in unimplemented modes 9–14
(13 valid, 30 reject; the Python reader returns `UnsupportedMode` where Rust
returns the mode-specific class). All asserted valid vectors decode
byte-exactly and match the recorded plaintext hash, content hash, mode, flags,
features, and chunk count; all asserted reject vectors raise the recorded
class. Cross-language: 11/11 cases pass — Rust CLI encodes RAW, ZSTD, chunked
RAW/ZSTD, REFERENCE, DELTA, DICTIONARY, TENSOR, TOKENS, QUANTIZED_TENSOR, and
STRUCTURED, and the Python reader verifies and decodes each byte-exactly, with
`inspect --json` fields matching Rust's. Full suite 166 tests: 115 passed, 51
skipped (50 vector skips plus the PyTorch module, absent by design), 0 failed.

**Artifact.** `python/dist/weftcodec-0.5.0-py3-none-any.whl`, 27,070 bytes,
sha256 `cc0afe3b9b68490c5ad29a9fe54b830bf6266fd704e9ad962a674cdb4a2781f1`.
It is **not published to PyPI** (no API token); it is a release artifact.

## 3. Evidence discipline

- Every build/test ran through `scripts/docker-court.sh` in a minimal official
  image with `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
  bounded `/tmp`, `no-new-privileges`, and `cap-drop=ALL` (FUSE adds only
  `/dev/fuse` and `CAP_SYS_ADMIN`).
- `scripts/verify-all.sh` re-ran all twenty-five courts and produced
  `bench/receipts/verify-20261105T035513Z.json` with revisions, host, command
  log, and per-court results.
- The P7 benchmark court ran codec, store, and filesystem benchmarks under the
  same hard limits. Every receipt contains raw samples, environment, and
  revisions. The codec receipt also records corpus hashes and per-tool command
  templates; the filesystem receipt embeds the full `fio` command log. The
  exact court commands are recorded in `bench/RESULTS-v1.md` §2. The results
  are single-host observations; `bench/RESULTS-v1.md` states the losses and
  gaps and makes no headline performance claim.
- Crash tests are process-fault proxies, not physical power-loss tests.
- The TLA+ **abstraction** is model-checked by bounded TLC runs. The Rust and
  Go implementations are **not** formally verified; there is no refinement
  proof. Implementation invariants remain test- and receipt-backed, and
  checkpoint/GC/capability/selector/multi-writer behaviour is outside the
  checked model.
- The F9 power-loss court is a QEMU virtual-machine power-cut proxy: `SIGKILL`
  loses the guest page cache and QEMU user-space state, not physical media or
  host/device caches. The block-layer court adds dm-flakey silent write drop,
  dm-flakey EIO, and a snapshot total-write-drop model; blkdebug write/flush
  errors are recorded as blocked at mount. It runs under the same hard limits as
  every other court.

## 4. What is **not** done (ranked)

1. **Remaining benchmark gaps** (P7): the comparative pass closed the external
   SQLite, ext4/XFS cold-cache, concurrent-fio, matched Go/Rust codec, and now
   whole-disk ZFS/Btrfs gaps (`bench/RESULTS-comparative-v1.md`). The 2026-11-04
   pass adds `bench-fs-cold-20261104T071922Z.json` (whole-disk `zfs-whole` on raw
   `/dev/vdb` plus Btrfs), `blkdebug-20261104T072008Z.json` (dm-flakey lying-disk
   matrix; blkdebug mount blocker), and `hostcache-probe-20261104T072314Z.json`
   (global cache drop refused). Still open: physical media power loss, host/
   device cache loss, RAPL/joule energy, direct-I/O, metadata storms, AI-corpus
   external baselines, 10^5–10^6-object scaling, and a library-vs-library codec
   court. See `bench/RESULTS-v1.md` §8 and `bench/RESULTS-comparative-v1.md` §8.
2. **Formal model coverage beyond the commit/recovery abstraction.** The F9 TLA+
   model checks bounded commit/recovery safety, durability, and replay
   idempotence, but not checkpoints, GC, capability filtering, selector
   semantics, multi-writer concurrency, or a refinement proof to Rust/Go.
3. **Showcase re-run at a later revision.** The committed receipt is evidence
   for the exact revision it records; re-running after code changes requires a
   new receipt, not an inference from the old one.
4. **FUSE binding limits in Rust.** `fuser 0.15.1` has no `FUSE_TMPFILE`
   dispatch (7.37) and drops `lk_flags`, so Rust cannot express `O_TMPFILE` or
   distinguish flock from POSIX byte-range locks; both are explicit boundaries
   tested in `tests/fuse_verb_matrix.rs`, with kernel-local locking as the
   fallback. Go implements both. COLLAPSE/INSERT store code exists in both, but
   kernel 7.3 rejects those flags before forwarding. `ioctl` and cross-daemon
   lock coordination remain absent.
5. **Same-inode concurrent direct FUSE I/O** on kernel 7.3: blocked on the
   kernel/fuser EIO interaction; no workaround is claimed. A same-description
   write after a shared `mmap` also returned EIO once; the matrix test uses a
   fresh description. Shared mmap itself (map, write, `msync`, exact read) passes.
6. **Physical power-loss court** (real media/device write-cache loss), host
   page-cache loss, and real disk firmware that lies about flushes; the F9 QEMU
   VM proxy and its `cache=unsafe` lying-fsync device model are not physical.
   Remote/object-storage backend also remains unimplemented and unclaimed.
7. **Encryption, multi-writer, distributed consistency**; explicitly out of
   scope for v1 claims.
8. **Streaming codec API for files larger than memory**; chunked/range APIs are
   real, but the CLI materializes whole inputs within limits.
9. **atime/ctime persistence**, device/FIFO/socket nodes, and Rust `O_TMPFILE`;
   not implemented. `mknod` regular files and FIFO/device rejection are
   implemented and tested in both frontends; Go implements `tmpfile`.
10. **Spec-to-Go export automation in CI**; `scripts/export-spec.sh` and
    `scripts/gen-types.sh --check` exist, but no CI workflow pins them. The
    drift court runs only when `verify-all` is invoked.
11. **F3 time travel is correctness-only.** There is no io_uring, EKBLOOM, or
    other acceleration; none is implemented or claimed. The 100-version demo
    measures exactness and pinning, not latency.
12. **Long-horizon agent memory (D2) is simulated, not measured.** Typed edges,
    task/model/time mounts, a January snapshot, September writes, and a
    months-later bundle replay pass with exact bytes and lineage. The timestamps
    are fixture nanoseconds and the replay is a point-in-time export; no real
    months of wall-clock retention, no background aging, and no physical media
    aging were tested.
13. **The Buck2 adapter is a local adapter, not an integration.** It is a
    store-backed CAS plus canonical action keys. It is not a Buck2 plugin,
    worker, or REAPI gRPC backend. It does not execute actions, fetch remotely,
    or integrate with Buck2's scheduler. The generated Context Root pins a
    named snapshot; a Context Root whose commit is not a named snapshot is not
    itself a v1 GC root.
13. **Replay bundle scope.** The bundle carries one store and its Context
    Roots; it does not yet carry a filtered/partitioned subset, remote fetch,
    or a signature. `tar` and `zstd` are required tools and a missing tool is a
    hard error (no uncompressed fallback).
14. **Context mount root adds a constant `.weft` entry** for exact-id access;
    it is not part of the Context Root result set. Listing digests in the demo
    exclude that constant name explicitly.

## 5. Next work queue (dependency order)

1. **Physical media power loss, host page-cache loss, and RAPL/joule energy.**
   Prerequisites already recorded: QEMU VM power-cut proxy and lying-fsync
   device model (`bench/receipts/powerloss-20261104T014805Z.json`), block-layer
   lying-disk matrix (`bench/receipts/blkdebug-20261104T072008Z.json`),
   unprivileged host-cache probe (`bench/receipts/hostcache-probe-20261104T072314Z.json`),
   and perf counters without readable RAPL
   (`bench/receipts/perf-counters-20261104T015026Z.json`). The exact remaining
   limits are in `verification/README.md` §2.5.
2. **TLA+ implementation refinement.** Prerequisite: the bounded
   commit/recovery model and its deliberate counterexample
   (`bench/receipts/tla-20261103T224446Z.json`, `verification/tla/`). The model
   is abstract; there is no refinement proof to the Rust or Go state machines.
3. **Go parity for `weft.manifest.v2`.** Prerequisite: the Rust-only v2
   representation result (`bench/receipts/representation-20261104T093437Z.json`),
   the unchanged default raw fixture (`8959c65b…e1d81`), and the Rust↔Go
   handoff court, which stays on raw. The Go store rejects v2 today.
4. **Go parity for persisted selector-index segments.** Rust writes advisory
   `indexes/<ab>/<commit>.idx` segments (see §F2 above); the portable format does
   not require them and Go still rebuilds per state. Segments are not
   snapshot-incremental on either side.
5. **Go bounded open — closed 2026-11-04.** Go `Store.Open` now uses the
   Rust-reference generation-addressed bounded replay in `store/recovery.go`,
   with the authoritative full scan as the fallback; on a 100,000-generation
   store the read-only open is 2.80 ms median / 162 commit reads versus
   2155.65 ms / 100,001 reads on the forced full scan
   (`bench/receipts/go-fast-open-20261104T190732Z.json`, synthetic tmpfs,
   single host, states identical). The earlier measurement (18.65–19.07 s to
   open 287,759 commits versus 305 ms Rust) is the old full-scan path; the
   POSIX court above therefore still sizes rust→go runs at 240,000 requested
   and go→rust runs at 640,000 (fixed 15 s harness mount wait). Bounded commit
   index is not implemented and is not claimed. **1,000,000 completed
   operations is met** (1,202,560 across
   `bench/receipts/posix-torture-20261104T162109Z.json`,
   `…T172156Z`, `…T182537Z`); the earlier 1,000,002-request run
   (`…T142424Z`, 453,432 completed) and the 13,694-operation run
   (`posix-torture-20261103T213317Z.json`) remain history.
   `pjdfstest`/`fsstress` are not installed in the court and no substitute run
   is claimed.
6. **Remaining AI codec modes:** float-aware, KV-cache, activation/gradient,
   and semantic-delta. Mode 10 `TENSOR_SHARDED` covers sharded/per-block
   quantized storage (WEFT-AI-CODEC §11) and mode 11 `WEIGHT_DELTA` covers
   dtype/shape-checked exact weight deltas (§12).
   Prerequisite: modes 6–11 byte-identical across Rust and Go
   (`WeftCodec-Go/hack/cross.sh`) and the `specified, not implemented` list in
   `spec/WEFT-AI-CODEC.md` §6. No wire value exists for the remaining modes.
7. **Layout-independent tensor identity.** Implemented for the identity,
   layout, region, and shard layers in both codecs (WEFT-TENSOR v1); no
   FUSE/store binding yet, so this is not a topology-independent restore
   receipt.
8. **A real public model-family corpus.** Prerequisite: the synthetic `ai-v1`
   corpus and cost graph (`bench/receipts/ai-corpus-20261104T021627Z.json`,
   `bench/RESULTS-ai-v1.md`). `ai-v1` is synthetic; no public corpus or
   superiority claim is made.
9. **Native async API and object-store backend.** Prerequisite: none; not
   started, and no claim is made.
10. **Buck2 plugin/REAPI integration.** Prerequisite: the store-backed local
    CAS (`bench/receipts/buck2-cas-store-correctness-20261105T163919Z.json`,
    41/41), which is not a plugin, worker, or REAPI gRPC backend.
11. **Real-time aging.** Prerequisite: the simulated-timestamp agent-memory demo
    (`bench/receipts/agent-memory-20261104T022512Z.json`, 7/7). Months of
    wall-clock retention were not measured.

    ## 2026-11-04 — A2: KV_CACHE (12) and ACTIVATION_STREAM (13)

    Two page-addressed AI modes are implemented in Rust and Go with byte-identical
    frames and bounded lazy page/chunk reads.

    - **Mode 12 `KV_CACHE`** (`weft.ai.kv_cache.v1`): one frame per page; exact
      descriptor (`model`, `layer`, `session`, `page_index`, `page_count`,
      `seq_start`, `seq_end`, dtype, head geometry, shape, `page_bytes`,
      `plane_codec`, `payload_len`). Payload is canonical row-major element bytes
      split into 2 (bf16/fp16) or 4 (fp32) Huffman-coded byte planes. Pages have no
      dependencies and no cross-page state; `KvPageSet` decodes exactly one page at
      a time and its counter is the independence evidence.
    - **Mode 13 `ACTIVATION_STREAM`** (`weft.ai.activation_stream.v1`): one frame
      per activation/gradient chunk with `digest_root = H("weft/tensor/values/v1",
      elements)`. `assemble_stream` rejects duplicate/missing indices, orders by
      `chunk_index`, verifies every digest, and returns
      `H("weft/ai/stream/v1", WCB({stream,step,rank,world,chunk_count,digest_roots}))`.
      `append_chunk` never decodes a previous chunk; the writer's decoded counter
      proves it.
    - **Vectors:** `conformance/v1/kv-cache/` and `conformance/v1/activation-stream/`
      are byte-identical in both codec repos: 3 valid + 8 rejects per mode
      (22 files). Reserved-mode vector moved to mode 14; the mode-12 vector is
      retained and marked `superseded_by`.
    - **Cross gate:** `WeftCodec-Go/hack/cross.sh compare` reports
      `YES byte-identical` for `kv_cache_p0`, `kv_cache_p1`, `activation_c0`, and
      `activation_c1`, and all existing modes still pass both directions.
    - **Bench:** `bench/receipts/kv-activation-20261104T220008Z.json`. Synthetic
      single-host rust court: 16 MiB KV (16 pages of 1 MiB bf16) encode median
      3.18 ms/page, whole-set decode 129.5 ms, single-page decode 8.88 ms; 64 MiB
      activation (64 chunks of 1 MiB bf16) encode median 2.99 ms/chunk, whole-set
      decode 580.9 ms, assembly 612.6 ms, single-chunk decode 9.01 ms.
    - **Tests:** Rust `cargo test --all-targets` and Go `go test ./...` in the
      Docker courts; new Rust integration tests cover bit-exact NaN/denormal/
      signed-zero round-trips, page-7-of-8 independence, chunk independence,
      append counters, shuffled/duplicate/missing assembly, and range reads.
    - **Residual:** Huffman planes are not seekable, so a range read decodes one
      page/chunk and slices it; mode allocation is now 13, reserving 14+.

    ## 2026-11-04 — A3: semantic roots and SEMANTIC_DELTA (14)

    Near-duplicate tensors are now findable by an explicit semantic root and
    exactly delta-encodable by mode 14. No lossy decode path exists.

    - **Semantic root:** `H("weft/tensor/semantic/v1",
      WCB(weft.tensor.semantic.v1))`. Policies are `none`,
      `mantissa_truncate:N` (bf16/fp16/fp32; 1 <= N <= mantissa bits; sign,
      exponent, NaN/inf preserved), and `block_quantize:B:L` (B divides the
      element count; signed two's-complement words; symmetric `[-amax, amax]`
      grid with the exact integer formulas in §15.1). `none` uses the exact
      `weft/tensor/values/v1` hash; the other schemes use
      `weft/tensor/semantic-values/v1`. `semantic_root` and `semantic_matches`
      exist in both languages and are byte-identical.
    - **Mode 14 `SEMANTIC_DELTA`** (`weft.ai.semantic_delta.v1`): the mode-11
      payload verbatim (modular words, byte planes, one Huffman stream per
      plane) plus a descriptor binding `base_semantic_root` and `policy`.
      Decode verifies base content hash, dtype, shape, the semantic root
      recomputed from the actual base bytes under the declared policy, the delta
      root, and the reconstructed content hash. Every mismatch fails closed.
    - **Candidate selection:** `select_semantic_base` requires both tensors
      present, rejects different dtype/shape/root, and picks the smallest exact
      per-plane Huffman size of the modular difference, tie-breaking on the
      candidate content hash. Deterministic; different roots are never offered.
    - **No-lossy statement:** approximate decoding is not implemented. The
      semantic policy only groups and verifies a base; reconstruction is
      bit-exact and an approximate base is `DependencyMismatch`, never a
      fallback.
    - **Vectors:** `conformance/v1/semantic-root/` (3 valid + 6 rejects) and
      `conformance/v1/semantic-delta/` (3 valid + 6 rejects), byte-identical in
      both codec repos. The previous reserved-mode vector moved from 14 to 15;
      `017_unknown_mode_reserved14` is retained with `superseded_by`.
    - **Cross gate:** `WeftCodec-Go/hack/cross.sh compare` reports
      `YES byte-identical semantic_delta_bf16`, `..._fp32`, and `..._q8`, and
      decode-equivalent both directions.
    - **Tests:** Rust `cargo test --all-targets` 182 passed / 0 failed
      (`tests/semantic_delta.rs` 6/6); Go `go test ./...` all packages pass
      (`conformance` semantic vectors and `modes` near-duplicate selection).
    - **Bench:** `bench/receipts/semantic-delta-20261104T222029Z.json`. Synthetic
      single-host rust court, 8 MiB bf16 pair, 4,195/4,194,304 changed (0.10002%):
      semantic root median `none` 0.68 ms, `mantissa_truncate:3` 8.95 ms,
      `block_quantize:128:256` 15.91 ms; selection over 32 same-root candidates
      983.2 ms, selected index 0; delta frame 1,050,660 B vs independent zstd-3
      6,127,332 B (5.83x) and zstd-19 4,976,379 B; encode median 31.93 ms,
      decode median 44.22 ms; `exact` and `logical_root_match` true.
    - **Residual:** the semantic root is an index hint, not a remote search
      service; candidate selection decodes every candidate; only three numeric
      policies exist in v1.

    ## 2026-11-04 — B1: tensor representations and the per-object planner

    WeftTensor frames are a first-class physical representation in the Rust
    store, selected by a deterministic cost-aware planner.

    - **Manifest extension:** additive v2 `kind:"tensor"` with `frame`,
      `tensor_mode` (10/11/12/13/14), `dtype`, `shape`, `logical_root`,
      canonical `layout`, `dependencies` (base content roots for 11/14),
      `decoded_size`, and `strategy`. Raw/codec v2 bytes are unchanged.
      `spec/WEFT-MODEL.md` §2.3.1 is normative.
    - **Store API:** `write_tensor_file`, `write_tensor_file_with_spec`,
      `plan_tensor`, `read_tensor`, `read_tensor_region`,
      `tensor_logical_root`, `tensor_metadata`, `write_tensor_range`. The store
      writes one object per tensor with one frame; multi-shard declarations are
      rejected rather than truncated. Row-major/column-major/blocked/single-
      shard inputs canonicalise to the same logical bytes and root.
    - **Planner:** `physical_bytes + lambda_ns * estimated_decode_ns`, default
      `lambda_ns = 0`; deterministic candidate order and tie-break; estimates
      are a documented integer model, not measurements. Candidates are raw,
      zstd, mode 10 (q8_0), mode 11 (causal parent), mode 14 (bounded
      same-semantic-root candidate set). Delta chains respect the frozen
      dependency depth bound. Planner prices are surfaced by `plan --json`
      only; the manifest carries no non-canonical fields.
    - **Reads/GC/verify:** full reads verify frame/mode/strategy, dtype/shape,
      dependency closure, recomputed `LogicalTensorRoot`, and the frozen
      content hash. GC recursively retains delta bases; `verify --full` names
      the object/manifest/frame/dependency on failure. Missing dependencies
      fail closed with `Corrupt`.
    - **Identity for raw/zstd tensors:** reserved attributes
      `weft.tensor.dtype|shape|logical_root|layout|strategy`.
    - **CLI:** `write --tensor --dtype --shape --layout`, `cat --region`,
      `plan --json`.
    - **Tests:** `tests/tensor_store.rs` 9/9 (mode 10/11 round trips and layout
      roots, raw selection for incompressible data, weight-delta under 50% of
      independent zstd, latency-weight switch, semantic-root gating,
      missing-dependency/GC/verify, canonical layout encoding, byte-rewrite
      drops stale tensor identity). Full Rust
      `cargo test --all-targets`: 112 passed / 0 failed. FUSE `--features fuse
      --all-targets`: 140 passed / 2 ignored.
    - **Bench:** `bench/receipts/tensor-planner-20261104T224929Z.json`,
      synthetic 10 checkpoints x 8 tensors x 1 MiB bf16, 1% change per step,
      bench court (`weft/bench-court:1`, 3 GiB, 4 vCPU, kernel
      7.3.1-1-cachyos), labelled single-host/no-fleet. Physical data bytes /
      ingest / restore (1 worker): zstd 15,917,707 / 8.22 s / 93 ms; bytes-only
      planner 11,187,086 / 24.37 s / 1.79 s; latency-weighted planner
      83,985,344 / 7.77 s / 37 ms. Restore with 2/4 workers: zstd 48/26 ms,
      bytes 906/467 ms, latency 22/15 ms; peak RSS 51,680/52,164/52,148 KiB;
      all three verify clean. The bytes planner saves 29.7% physical bytes at
      ~3x ingest and ~19x restore cost (delta chains are decoded per read); the
      latency planner is fastest to restore at 5.3x the physical bytes.
    - **Residual:** Go store parity for `kind:"tensor"`; multi-shard objects;
      q4_0 store representation; modes 12/13 are read-only in the store;
      region reads decode the whole delta/Huffman frame; the planner's decode
      estimate is not calibrated to a device.

    ## 2026-11-04 — D1: provenance receipts and provenance-guided recovery

    Provenance is now a first-class store object, and recovery is planned from
    verified bytes, representation chains, and recorded computation.

    - **Record:** `weft.provenance.v1` canonical WCB with `operation`, bounded
      `params`, `inputs[{role,content,object}]`, sorted unique `outputs`,
      `environment{tool,version,env_hash,...}`, `determinism`
      (`exact|declared|unknown`), `principal{uid,gid}`, and `timestamp_ns`.
      Inputs are strictly ordered by `(role, content)`; duplicate inputs or
      outputs are `NonCanonical`. Bounds: params 64 KiB, environment 16 KiB,
      1,024 inputs, 1,024 outputs, 1,024-byte text fields, 1 MiB record.
      `ProvenanceId = H("weft/provenance/v1", WCB(record))`.
    - **Storage/attachment:** the record is a content-addressed blob in the
      manifest namespace under the provenance domain (never parsed as a
      manifest). `record_provenance(outputs, record)` rejects any declared
      output root that does not equal a supplied object's actual content root,
      publishes the blob, then attaches `predicate="provenance"` value-target
      relations (lowercase hex ProvenanceId) to every output in one commit.
      API: `record_provenance`, `provenance_of`, `provenance_records_of`,
      `provenance_ids_of`, `provenance_closure` (bounded, cycle-rejecting).
    - **Retention:** GC marks a receipt, its declared inputs, and its outputs
      while any output object that names it is retained; external inputs are
      skipped. Object deletion now emits explicit `del_relation` ops for
      relations whose subject is the removed object, so the last output
      deletion releases the receipt and otherwise-unreferenced inputs. Replay
      and cross-language state see the same explicit ops; `apply_op` was not
      changed.
    - **Verify:** `verify --full` validates the relation value target, subject
      existence, blob hash/canonical re-encoding, and that the receipt still
      declares the subject's current content root. Report field
      `checked_provenance`. A rewrite without a new receipt is reported as
      stale provenance.
    - **Planner:** pure and deterministic. Eligibility is `available` for
      fetch/decode; recompute additionally requires `determinism == "exact"`
      and every input verified. Cost is the saturating sum of the declared
      fetch/decode/recompute components; the winner minimises
      `(cost, fetch < decode < recompute, name)`. With no eligible candidate it
      returns `NoEligibleStrategy` naming every candidate and reason; there is
      no fallback. `Store::recovery_candidates` offers fetch for raw chunked
      manifests, decode for frame-backed codec/tensor manifests, and one
      recompute candidate per attached receipt with verified-input checks.
    - **CLI/spec:** `provenance record|show|closure`, `recover --plan` (prints
      strategy and three cost components), and `spec/WEFT-PROVENANCE.md`.
    - **Tests:** `tests/provenance.rs` 19/19 (round trip, order-independent
      identity, unsorted/duplicate rejection, output mismatch without writing,
      bounded params, atomic multi-output attachment, cost choice/component
      sums, exact-determinism and missing-input exclusion, tie-break, typed
      no-eligible error, closure/depth/cycle, GC retention and reclamation,
      tampered record, stale output, malformed target, reopen). Rust
      `cargo test --all-targets`: 135 passed / 0 failed / 0 ignored. FUSE
      `cargo test --features fuse --all-targets`: 163 passed / 0 failed /
      2 ignored (existing CAP_SETUID and kernel direct-I/O guards).
    - **Bench:** `bench/receipts/provenance-20261104T235013Z.json`, bench court
      `weft/bench-court:1` (3 GiB, 4 vCPU, 1024 pids, kernel
      7.3.1-1-cachyos), synthetic 10 operations x 10 artefacts x 1 MiB
      (104,857,600 logical bytes; 80 fetch-capable, 20 decode-capable), one
      full operation (10 artefacts) unlinked and collected (141 blobs /
      8,579,129 bytes), receipt retained by the harness. Injected plan
      (fetch 10 ms/MiB, decode 30 ms, recompute 100 ms): 72 fetch / 18 decode /
      10 recompute, 68.648 ms wall, 159,629,377 physical bytes, 2,260 ms
      declared. Measured plan (warm-cache medians: fetch 318,013 ns across 72
      artefacts, decode 374,498 ns across 18): same mix, 68.823 ms wall, 1,029
      ms declared. Forced recompute: 100 recompute, 311.583 ms wall,
      756,138,570 physical bytes, 10,000 ms declared. `verify --full` clean
      with 90 checked provenance relations. Label: synthetic, warm-cache,
      single host, no fleet claim.
    - **Residual:** no Go `weft.provenance.v1` parity; the planner's store-side
      fetch/decode availability is determined by representation (raw vs
      frame-backed), so the bench's measured costs do not change the mix;
      closure follows an explicit input object or a unique content-root match,
      otherwise the input is an external leaf; a rewrite after recording makes
      the receipt stale and `verify --full` reports it.

## Subphase F — real public checkpoint family (storage and restore)

**Implemented, measured once, single host.** The headline experiment ingests a
real public base/fine-tune pair into WeftFS as tensors, chooses representations
with the shipped bytes-only planner, and compares complete storage and restore
against strong conventional baselines at matched exactness. Receipt:
`bench/receipts/real-model-20261105T012000Z.json`; analysis, every loss, and the
reproduce commands: `bench/RESULTS-real-model.md`.

- **Corpus:** `Qwen/Qwen2.5-0.5B` (`060db649…`) and
  `Qwen/Qwen2.5-0.5B-Instruct` (`7ae55760…`), one 988,097,824-byte
  `model.safetensors` each, both BF16, 290 identical tensor names/shapes,
  SHA-256 verified against the Hugging Face LFS pointers. The operator's first
  choice (`HuggingFaceTB/SmolLM-135M` + Instruct) publishes the base as F32 and
  the instruct model as BF16 despite both configs saying bfloat16, so no exact
  causal delta is possible; it was ingested as a cross-dtype secondary instead.
  No model weights are committed; they live under `.docker-cache/real-corpus/`.
- **Harness:** `examples/real_model_bench.rs` parses safetensors headers
  directly and calls `write_tensor_file_with_spec` per tensor with no strategy
  hint, offering the corresponding base tensor version as the causal parent for
  the instruct checkpoint. The header is stored as a raw object so the complete
  file can be reassembled. Ingest and restore are separate processes for honest
  peak RSS.
- **Exactness:** both reassembled files match the LFS SHA-256
  (`88c14255…0ed342`, `fdf756fa…88fb7fe`); every tensor hash and every logical
  root matches; `verify --full` is clean. All four judged methods are lossless.
- **Storage, both checkpoints (bytes):** ZipNN 0.5.4 **1,316,518,229** (winner);
  WeftFS store **1,389,785,926** (+5.57 %); WeftFS data-only 1,313,652,215
  (−0.22 %, i.e. a tie once 75,549,540 bytes of rebuildable index segments and
  575,964 bytes of commits are excluded); zstd `--patch-from` `-12` both files
  1,503,140,795 (+14.18 %); zstd `-19` independent 1,528,909,007 (+16.13 %).
- **Per checkpoint:** WeftFS base 788,405,375 (loses 19.8 % to ZipNN and 3.1 %
  to zstd `-19`); WeftFS instruct marginal 601,380,551 (**wins 8.64 % vs
  ZipNN** and 21.3 % vs zstd `-19`). The planner chose 241 weight deltas, 1
  zstd frame, and 48 raw tensors for the instruct checkpoint; the base chose
  242 zstd + 48 raw. No semantic delta was applicable.
- **Restore:** WeftFS base read 968.8 ms / materialize 6,464 ms; instruct read
  14,367 ms / materialize 23,752 ms. zstd `-19` read (to null) 1,413 / 1,488 ms
  and materialize 976 / 727 ms; ZipNN 240 / 196 ms in-memory and 508 / 455 ms
  to file. **WeftFS loses full instruct restore by 9.7× to zstd and 73× to
  ZipNN**, the cost of Huffman-decoding weight deltas plus the base dependency.
  A single tensor (`model.layers.10.mlp.down_proj.weight`) restores in 7.3 ms
  reading 6,842,059 bytes (base) and 118.6 ms reading 11,643,580 bytes
  (instruct, delta + base), versus a whole-file decode of 658–765 MB for the
  file-at-a-time formats.
- **Ingest:** 136.0 s total (30.7 s base + 105.3 s instruct), peak RSS
  2,540,908 KiB; the instruct phase reads 19.07 GB from the store backend while
  pricing raw/zstd/weight-delta candidates. zstd `-19` took 538.9 s; ZipNN
  0.50 s.
- **Unavailable baselines:** `zstd --patch-from -19` was killed by the 3 GiB
  cgroup limit after 510.3 s (`returncode 137`, `maxrss 3,131,264 KiB`,
  622,206,976 partial bytes); the reported patch baseline is `-12` for both
  files. DFloat11 0.5.0 installs but cannot import: it builds a CUDA `RawModule`
  at module load and the court exposes no `/dev/nvidia*`; cupy 14.2.0 reports
  `cudaErrorInsufficientDriver`. No DFloat11 bytes or time are claimed.
- **Secondary:** SmolLM-135M (F32) + Instruct (BF16) ingested with no causal
  parent offered, all tensors stored independently (512.7 MB data), both files
  restored byte-exactly; confirms the dtype mismatch fails closed.
- **Residual:** single host, warm caches, no cache dropping; ZipNN was run
  whole-file and independent, not per-tensor or delta-aware; WeftFS restore
  timings are single observations for the full-file rows; no Go parity for this
  court.

## Subphase R1 — measured decode cost, mode-9 planner candidate, rebasing

**Implemented, measured once, single host; Go parity in the store API.**
Receipt `bench/receipts/planner-float-20261106T025351Z.json`; analysis and
old-vs-new tables in `bench/RESULTS-real-model.md` (R1 section).

- **Candidate set:** mode 9 `FLOAT_TENSOR` is now priced by the planner for
  BF16/FP16/FP32 tensors written through `write_tensor_file*`, alongside raw,
  zstd, mode 10, mode 15 (opt-in), weight-delta, and semantic-delta. Mode 9 is
  only offered when it saves at least 1/64 (1.56 %) of the zstd frame, because
  it decodes ~19x slower; this prevents a noise-sized win on incompressible
  data from selecting the slow decoder. FP32 remains offered but loses on the
  measured corpora (P2: +62 % physical), so the planner does not select it.
- **Cost model:** one affine table per language (`src/cost_model.rs`,
  `WeftFS-Go/store/cost_model.go`) with `fixed_ns + decoded_bytes * ns_per_mib
  / 1048576`, integer-only. Default `measured` (median of three 7-repetition
  court runs at 128 KiB and 16 MiB: raw 10,000 + 318,321/MiB; zstd 17,000 +
  814,267; mode 9 0 + 15,403,165; mode 10 32,000 + 13,788,933; mode 15 14,000
  + 10,056,794; weight-delta 31,000 + 5,297,277 incremental; semantic-delta
  18,000 + 7,228,350 incremental). `--planner-cost-model=declared` reproduces
  the old hand-written linear model. Delta rows are incremental and the rebase
  planner sums the dependency chain. Modes 12/13 are never produced and keep
  declared rows. Calibration is host/court-bound, not a portable constant; this
  run reproduced mode 9 to 0.003 %, zstd 5 %, raw 0.7 % in this run (a previous run varied 20 %, host noise).
- **Access counters:** advisory, per-object, bounded (1,000,000 counters;
  8,192 journal records), persisted as `access/counts.v1` (epoch + digest) and
  `access/journal.v1`, rebuildable from the journal, and never consulted by
  commits, recovery, verification, GC, or any frozen format. Read-only opens
  never write; the read path flushes every 256 observations and on close.
  Go writes the same files and reads the same epoch/digest rules.
- **Rebase:** `weft rebase --root <path|id> --threshold <ns> [--dry-run]
  [--strategy ...]`. A candidate needs `reads > 0`,
  `reads x current_decode_cost > threshold`, and a standalone representation
  that decodes strictly faster and is no larger than the logical bytes. Apply
  publishes a **new version** (causal parent = previous version), records a
  canonical `weft.provenance.v1` receipt (`operation: "rebase"`, input = old
  content root, output = new content root, params include reads/threshold/
  strategies), consumes the counter, and leaves every previous version and
  snapshot intact. A second run with no intervening reads is a no-op. Go
  implements the same decision and writes a receipt the Rust verifier reads
  (`store/rebase.go`; cross-language test).
- **Qwen2.5-0.5B family with the new candidate set:** family store total
  **1,276,128,956 B** (F10 1,389,785,926), data-only **1,199,958,505 B**
  (1.20 GB, matching the reviewer's arithmetic), indexes 75,584,928 B (5.92 %).
  **WeftFS now beats the recorded ZipNN total 1,316,518,229 B by 3.07 %**
  (F10 lost by 5.27 %); per checkpoint base 674,729,302 B and instruct marginal
  601,399,654 B. Base strategy mix: 169 mode 9 + 73 zstd + 48 raw; instruct:
  241 weight-delta + 1 zstd + 48 raw. Exactness unchanged (both files match the
  LFS SHA-256, every logical root matches, `verify --full` clean).
- **Restore cost (the honest loss):** base read 13,776 ms / materialize
  16,737 ms (F10 968.8 / 6,464); instruct read 26,936 ms / materialize
  29,951 ms (F10 14,367 / 23,752). Single tensor base 111.4 ms, instruct
  221.9 ms (F10 7.3 / 118.6). Mode 9 is 14.2x slower than the F10 base path and
  ~57x slower than ZipNN's in-memory decompressor; the storage win is bought
  with decode time. Ingest also costs more (147.6 s base + 248.4 s instruct;
  peak RSS 2,720,500 KiB of 3 GiB) because both candidates are encoded.
- **Rebase dry run (hot instruct):** after 10 reads per tensor (9.88 GB
  logical), threshold 1e9 ns selected 73 of 241 weight-delta objects, all
  `float-tensor`, no writes; e.g. the 272 MB embedding current decode
  5.375 s/read -> 4.000 s/read, 180,547,519 B independent.
- **Tests:** Rust `cargo test --all-targets` 244 passed / 0 failed / 0 ignored;
  FUSE `cargo test --features fuse --all-targets` 278 passed / 0 failed /
  2 ignored (one earlier FUSE invocation reported a transient failure; two
  subsequent clean runs passed all 278); Go `go test -race -count=1 ./...` all
  packages pass, including the cross-language rebase/provenance test. New tests:
  `tests/planner_float.rs` 4/4 and `WeftFS-Go/store/planner_float_test.go` 3/3.
- **Residual:** one host, warm cache; the measured table is court-bound; mode 9
  restore is slower than zstd/ZipNN; the rebase dry run only plans (no apply in
  the real-model court); the Go store has no CLI `rebase` verb yet (API + tests
  only); index segments are 5.92 % of the total.

## Subphase R2 — decoder throughput (no wire change)

**Implemented, measured once, single host, one subphase.** Receipt
`bench/receipts/planner-float-20261106T035316Z.json`; analysis and both
old-vs-new tables in `bench/RESULTS-real-model.md` (R2 section). Revisions:
WeftFS `ba5a0e6`, WeftCodec `1ee5d90`, WeftCodec-Go `d1aab12`, WeftFS-Go
`bb15514`. This is the governing objective from the R1 review: reconstruction
cost, not more format coverage.

- **Huffman decode:** `huffman-canonical-v1` now uses a 12-bit primary table
  over an MSB-aligned 64-bit accumulator; the original canonical bit-at-a-time
  loop is kept for codes longer than 12 bits and for stream tails. Wire format,
  encoder bytes, validation order, error classes, padding and consumed-byte
  accounting are unchanged. Isolated microbench: 4.7-5.0x (Rust), 4.1-5.3x
  (Go).
- **FLOAT_TENSOR plane join:** `join_float_tensor_payload` /
  `JoinFloatTensorPayload` extract `sign+mantissa` once per element and write in
  place, replacing a loop that read two bit fields one bit at a time. Microbench:
  3.1-5.7x (Rust), 4.8-6.5x (Go).
- **Proof the bytes did not move:** a test-only verbatim reference decoder in
  each language plus 10,000-case randomized differentials comparing output
  bytes **and** error class, in Rust and Go; the cross-language `compare` gate
  and both conformance replays unchanged at 206 pass / 0 fail / 101 skip.
- **Planner table re-measured and re-baked:** median of three independent
  7-repetition calibrations. mode 9 15,403,165 -> **4,683,726 ns/MiB (3.29x)**;
  mode 15 10,056,794 -> **3,366,772 (2.99x)**; mode 10 13,788,933 ->
  **9,123,307 (1.51x)**; weight-delta 5,297,277 -> 5,293,987 (1.00x);
  semantic-delta 7,228,350 -> 6,458,523 (1.12x). `src/cost_model.rs` and
  `WeftFS-Go/store/cost_model.go` carry identical integers and
  `MEASURED_CALIBRATION` moves to `weftfs-r2/decode-calibration/v1`.
- **Restore:** base read 13,775.8 -> **4,627.5 ms (2.98x)**, base materialize
  16,736.9 -> 6,494.3 ms; instruct read 26,935.7 -> **11,371.2 ms (2.37x)**,
  instruct materialize 29,950.5 -> 12,092.2 ms; single tensor base 111.4 ->
  **39.8 ms**, instruct 221.9 -> **92.7 ms**. Storage unchanged at
  1,276,128,956 B (3.07 % under ZipNN) because the planner is bytes-only at
  `lambda = 0`. Ingest also fell (base 147.2 -> 65.2 s, family 241.4 -> 111.6 s)
  because planning and verification decode candidates.
- **Rebase:** with the cheaper measured table, the 1e9 ns threshold on the hot
  instruct checkpoint selects **1 of 241** objects (R1: 73 of 241); the check
  remains non-zero and the dry run still writes nothing.
- **Still a loss:** base read is 4.8x the F10 zstd-strategy read (968.8 ms) and
  ~19x ZipNN's 240 ms; mode 9 is 6.0x zstd per MiB (4.68 vs 0.78). At this
  measurement decode is sequential across tensors and frames are not yet
  block-independent streams with an offset table; the parallel additions below
  are later in this section. `tensor-sharded` is now the slowest class
  (9.12 ms/MiB).
- **Tests at that tree:** WeftCodec `cargo test --all-targets` 204 passed / 0
  failed; WeftCodec-Go `go test ./...` + `go vet` all packages; parent 244
  passed; FUSE `--all-targets` 278 passed / 0 failed / 2 ignored; WeftFS-Go
  `go test -race -count=1 ./...` all packages. One transient
  `extent_cow::chunk_size_comes_from_the_manifest` failure appeared in the
  first fully loaded parallel FUSE run; 10/10 standalone repeats and the rerun
  passed. It is recorded, not deleted.
- **Parallel reconstruction (R2 addition):** `Store::read_tensors_bounded`
  (`src/parallel.rs`) decodes independent tensors on a bounded
  `std::thread::scope` pool and returns exact input order for every thread
  count; the harness gained `--threads` and `--max-inflight-mib`. Read at
  4 threads is **2,206.4 ms** base and 5,275.7 ms instruct (**2.09x** / 2.08x
  against one thread, 95-99 % of the Amdahl cap set by the 272 MB embedding,
  which alone exceeds the staging bound and runs alone). Backend bytes read and
  every restored byte are identical for all thread counts. Materialize does
  **not** improve reliably (4-thread base median 10,987.9 ms, 0.43x, in the
  committed pass; host writeback dominates a sequential 988 MB write). Peak RSS
  1.94-2.18 GiB (2,036,324-2,290,452 KiB) of the 3 GiB court. Receipt
  `bench/receipts/parallel-restore-20261106T043524Z.json`, two further passes
  in the same court, analysis in `bench/RESULTS-real-model.md`. Tests:
  `tests/parallel_read.rs` 9/9, parent 253 passed / 0 failed, FUSE 287 passed /
  0 failed / 2 ignored (pre-existing). Single-host, 4-vCPU, warm-cache; no
  fleet claim.
- **Residual:** single host, warm cache; the table is court-bound; parallel
  reconstruction is per-object only (no block-independent streams inside one
  frame) and does not make materialize reliable; no Go writer for the
  checkpoint/mode-15 paths; no fleet claim.
- **Planner tiled candidate (additive):** `TensorPlanOptions.tile_shape` and
  `weft plan --tile-shape A,B` price mode 15 without changing defaults. On the
  real BF16 base embedding (272,269,312 B), tiled `[128,224]` is **184,633,502 B
  vs 180,623,816 B for mode 9 (+2.22 %)** and estimates **874,217,523 ns vs
  1,216,176,993 ns (-28 %)**; mode 9 still wins at the bytes-only default. Test
  `planner_offers_mode15_only_when_a_tile_shape_is_supplied`. Intra-tensor
  parallel tile decode is covered by the next bullet.
- **Bounded parallel tile decode + auto-tiling (additive, wire unchanged):**
  `TileDecodeOptions { threads, max_inflight_bytes }` (default `threads = 1`)
  and `Store::read_tensor_with_options` /
  `read_tensor_region_with_options` decode mode-15 tiles on the same bounded
  runner as `read_tensors_bounded`: deterministic order, lowest-index error,
  panics caught, exact staging bound; every touched tile root is verified, the
  full-object path also verifies the logical root and content hash, and the
  region path is documented as not re-reading the whole-frame trailer.
  `TensorPlanOptions.auto_tile_bytes` derives a dividing rank-2 tile shape
  (largest element count in budget, then balance, then lexicographic), offers
  mode 15 to the **unchanged** price, and records the shape in the candidate
  detail and in `weft.tensor.tile_shape`. `weft cat --threads N`,
  `weft plan --auto-tile-bytes N`, `weft write --auto-tile-bytes N` expose it.
  On Qwen2.5-0.5B base the bytes-only price selected mode 15 for **0/290**
  tensors (float-tensor is 2.17 % smaller), so auto-tiling at 65,536 B changed
  no representation. A labelled forced-tiled control (169 rank-2 tensors, shapes
  `[128,224]`/`[128,256]`/`[256,128]`) costs **+14,310,892 B (+2.174 %)** of
  tensor data and measures full-restore read **3,038.3 -> 1,062.1 ms (2.86x)**
  at 4 workers, materialize 4,215.6 -> 2,871.4 ms (1.47x, still writeback
  bound), embedding whole-region **738.7 -> 203.0 ms (3.64x)**, and 1-row region
  3.5 ms with 4 tiles and **730,133 B fetched vs 180,625,100 B** (247x).
  Logical-root recheck stays sequential (0.99x); 100-row/1-row regions touch 4
  tiles and do not parallelise. Receipt
  `bench/receipts/tiled-parallel-20261106T062146Z.json`, analysis
  `bench/RESULTS-tiled-parallel.md`, `tests/tiled_parallel.rs` 8/8, parent 265
  passed, FUSE 299 passed / 2 ignored / 0 failed (one transient
  `representation` failure in a loaded first run passed standalone and on
  rerun). Single host, 4 vCPU, warm cache; no fleet claim; no Go parity; no
  partial boundary tiles.

## Subphase P2 — real-weight FLOAT_TENSOR and WEIGHT_DELTA court

**Implemented, measured once, single host.** This court settles two empirical
questions on real public weights, reported separately: (A) whether
`FLOAT_TENSOR` (mode 9) is competitive with zstd and ZipNN-class coding, and
(B) whether `WEIGHT_DELTA` (mode 11) helps more on consecutive training
checkpoints than on an independent fine-tune. Receipt:
`bench/receipts/real-float-tensor-20261105T053958Z.json`; tables, method, every
loss, and reproduce commands: `bench/RESULTS-float-tensor-real.md`. Label:
single host, public weights, warm cache, no fleet claim; every method lossless.

- **Corpus:** seven files, all SHA-256-verified against the Hugging Face LFS
  pointers for the exact revision. `Qwen/Qwen2.5-0.5B` and `-Instruct` are
  BF16 (290 tensors, 988,097,824 bytes each). `EleutherAI/pythia-70m` branches
  `step0`, `step1`, `step2`, `step4`, `step8` are **F32** (76 tensors,
  281,715,176 bytes each) and are not reported as bf16.
  The F32 containers hold fp16-precision values widened to F32 (the receipt's
  byte-plane probe finds entropy 0.0 in byte plane 0 and 8 symbols in plane 1
  of the largest tensor);
  no claim is made for full-precision random F32. The published `step0` and
  `step1` files differ in **exactly 19 bytes**, so that pair is reported
  separately and is not the headline consecutive result.
- **Harness:** `WeftCodec/examples/real_float_tensor_bench.rs` parses
  safetensors headers directly and calls the shipped `encode_float_tensor` /
  `decode_float_tensor` and `encode_weight_delta` / `decode_with_base_frame`
  entry points. zstd baselines are the real 1.5.4 CLI, per tensor at `-19 -T1`
  and default `-3 -T1`; whole-file `-19` is reported separately. ZipNN 0.5.4
  is the official whole-file byte path; it is not delta-aware.
- **(A) BF16:** `FLOAT_TENSOR` is **14.4 % smaller than per-tensor zstd -19**
  on the base (657,975,743 vs 768,347,895) and 14.3 % smaller on instruct
  (657,891,990 vs 767,597,259); it is 14.0 % smaller than whole-file zstd -19.
  ZipNN 0.5.4 gives 658,281,885 / 658,236,344, so mode 9 is ahead by 306,142 /
  344,354 bytes (0.05 %) — a tie, not a claimed win. Mode-9/raw is 0.6659.
  All 121 tensors of at least 1 MiB win; 169/290 win overall; the worst loss is
  +663 bytes on a 1,792-byte layernorm.
- **(A) F32:** `FLOAT_TENSOR` is **62.0 % larger than per-tensor zstd -19**
  (233,958,945 vs 144,397,747 on step0) and **95.5 % larger than ZipNN 0.5.4**
  (119,680,613); 0/76 tensors win. Mode-9/raw is 0.8305. The structural reason:
  mode 9 stores the 24-bit sign+mantissa plane raw for F32 and the pythia
  mantissas have three low-entropy bytes that zstd removes.
- **(B) Consecutive pairs** (delta bytes vs the newer checkpoint's per-tensor
  zstd -19; patch-from is the whole-file baseline): step0→step1 7,434,959 /
  144,403,851 (0.0515; patch 30,265, so delta is 245.66× worse on this
  19-byte-change artifact); step1→step2 28,030,116 / 144,272,053 (0.1943;
  patch 67,686,695); step2→step4 33,412,606 / 144,303,717 (0.2315; patch
  111,529,080); step4→step8 49,198,709 / 144,464,688 (0.3406; patch
  141,228,981). Delta is 19.4-34.1 % of independent per-tensor zstd -19 on the
  meaningful consecutive pairs and 2.4–3.3× smaller than the best
  `zstd --patch-from` there.
- **(B) Independent fine-tune:** base→instruct changes 97.93 % of words; mode
  11 is 541,188,577 bytes, which is **70.5 % of zstd -19** (767,597,259) and
  72.2 % of the best patch (749,534,915 at `-12`; `-19` was killed by the
  3 GiB limit at 3,128,344 KiB). So delta helps, but 2–3.6× less than on
  meaningful consecutive checkpoints. The two case sets are never aggregated.
- **Restore:** single-threaded, 5 reps per tensor, sum of per-tensor medians,
  excluding storage I/O. Mode 9 is 6.7× (pythia F32) to 12.0× (Qwen BF16)
  slower than zstd -19 decode; delta is 16.1–20.6× slower on the four pythia
  pairs and 44.0× slower on the Qwen fine-tune. Delta restore also reads the
  base frame (the Qwen pair reads 1,199,164,320 bytes vs zstd's 767,597,259).
- **Losses kept explicit:** mode 9 loses on F32 by 62.0 %/95.5 %; only ties
  ZipNN on BF16; decodes 6.7–12× slower. Mode 11 loses 245.66× to patch-from
  on the 19-byte step0→step1 pair because any non-constant Huffman plane costs
  at least one bit per element; it reaches only 70.5 % of zstd -19 on the
  fine-tune; and its restore is 16–44× slower.
- **Residual:** one host, warm caches, no cache dropping; restore totals are
  sums of medians rather than one pass; ZipNN is whole-file and not
  delta-aware; mode 9/11 are measured as codec frames, not through the store
  planner; no Go parity for this court.

## Subphase P5a — write budgets and branching time travel

Implemented in both stores (`spec/WEFT-QUOTA.md`, `docs/quota-branching.md`).

- **Commit-time budgets.** Admission is checked inside the writer lock before
  any commit bytes exist. A commit that would exceed a configured
  `{posix(uid,gid) | tenant(name), max_new_bytes, max_objects}` budget fails
  with `BudgetExceeded: write budget exceeded`; generation, head, and state are
  unchanged. Charges are the canonical object-record bytes plus newly reachable
  manifest/frame/chunk blob bytes (content already reachable costs zero,
  deduped per commit). POSIX attribution is the record's own owner; tenant
  attribution is the reserved `weft.tenant` attribute (`create --tenant`).
- **Usage storage.** The store-root `quota` file (`WFQ1 || u64le len || WCB ||
  BLAKE3("weft/quota/v1", WCB)`) carries the policy and cumulative counters
  keyed by the committed generation and head. It is written atomically after
  each commit and is never authoritative over content. On open, a record that
  does not name the committed head fails writes closed (`Corrupt`) and is
  reported by `verify`; `quota rebuild` recomputes usage from the published
  commit chain, `quota reset` zeroes counters, and both are explicit admin
  operations. `quota show` returns only the caller's own counters and limits.
  **(Superseded by Q3 below: usage is now a `set_quota_usage` op inside the
  commit; `WFQ1` is read only for migration.)**
- **Branching.** `snapshot branch` adds a ref to an immutable commit.
  `context branch` persists a new Context Root pinned to the source root's fork
  commit (optionally conjoined with `attrs.task`/`attrs.model`) and records a
  `derived_from` lineage relation. `merge` is an explicit **union** merge: a
  path present in both inputs or already at the live head must have identical
  content, else `Conflict` naming the path; it never overwrites the head and is
  not a three-way content merge.
- **Tests.** Rust `tests/quota.rs` 10/10 and `tests/branching.rs` 9/9;
  `cargo test --all-targets` and `cargo test --features fuse --all-targets`
  pass (the same two ignored ownership/concurrency cases as before). Go
  `store/quota_test.go` 7/7 and `store/branch_test.go` 7/7 (including both
  cross-language directions), full Go suite with `-race` passes in the fuse
  court, `go vet` and `CGO_ENABLED=0 go build` clean.
- **Cross-language.** Rust writes a store with quotas and a snapshot branch;
  Go loads the persisted policy, verifies, reads the branch, enforces a denial,
  and advances usage. The reverse direction does the same with the Rust CLI
  reading and enforcing the Go-written record.
- **Bench** (`bench/RESULTS-quota-branch.md`, receipt
  `bench/receipts/quota-branch-20261105T071244Z.json`): 1,000 commits p50
  11.35 ms without budgets and 13.75 ms with budgets (+2.41 ms, +21.2 %; the
  delta is the durable usage record); branch creation p50 9.11 ms over 50;
  merge of 1,000 disjoint paths 255.6 ms, 1,000 objects/entries, full verify
  ok.
- **Demo** (receipt `bench/receipts/branching-20261105T071457Z.json`, 7/7):
  10,000 versions, 21 snapshots, branch from `s5000`
  (`72ace03186863e95c9fe2c7f3ae166c8d5cfe2eed9a118cea6a34f5e0443cc7d`) into
  `by:task=agent-43`, five later writes, then `s5000`, `s10000`
  (`da172164095e35da79952b4a22b52130cc47a50a65b10367ec004088036a2762`), the
  snapshot ref, and the branch Context Root all read their exact historical
  bytes; live reads the 10,005th version; four mounts, zero surviving daemons.
- **Residual:** the quota record is the policy authority, so deleting it is a
  store-local admin act that disables enforcement (a present-but-unusable
  record always fails closed). Budgets charge admission, not footprint, so
  deletions do not return budget. The merge is a union, not a three-way merge.
  Tenant attribution is not yet set by the FUSE write path.

## Subphase P4 — Merkle content identity (`weft.manifest.v3`)

Implemented in both stores and specified in `spec/WEFT-MODEL.md` §2.3.2.
The measured bottleneck was the frozen v1 `ContentId =
H("weft/chunk/v1", whole_plaintext)`: every range write recomputed it by reading
the whole file (108 ms Rust / 392 ms Go in the FUSE court). v3 replaces it with
a Merkle root over the chunk table.

- **Format.** `weft.manifest.v3` carries the v2 fields with `content_root` in
  place of `content_hash`, plus `content_identity:"merkle-v1"`. Leaves are
  `H("weft/merkle/leaf/v1", u64le(len) || chunk_hash)`, nodes are
  `H("weft/merkle/node/v1", l||r)`, an odd trailing node is promoted unchanged,
  and an empty file is `H("weft/merkle/empty/v1", empty)`. The chunk domain for
  `chunk_hash` is the existing `weft/chunk/v1`. The root is chunk-size dependent
  by design; `chunk_size` is part of the manifest.
- **Selection.** `Options.content_identity = WholeFileV1 (default) | MerkleV3`
  and the global `--content-identity whole-file|merkle` flag in both CLIs.
  `merkle` + `codec-auto` is rejected. The policy is per invocation and not
  persisted. The default and every existing fixture stay byte-identical.
- **Write path.** A range write re-hashes and re-stores only changed chunks and
  folds the root from the manifest's stored chunk hashes plus changed leaves. No
  whole-file read for hashing. A `weft.manifest.v3` file is never implicitly
  rewritten as v1/v2; `truncate`, `zero/collapse/insert`, and reopen preserve
  it. Unknown schemas fail closed.
- **Read/verify.** Full reads verify every chunk blob and recompute the root;
  range reads verify touched blobs. `Store::verify_region` /
  `Store::VerifyContentRegion` verify the touched blobs and recompute the root
  from all chunk hashes (O(#chunks) small hashes; no stored sibling table) and
  name the failing chunk index. `verify --full` handles v1, v2, and v3.
- **Tests.** Rust `tests/merkle_identity.rs` 7/7 (determinism, chunk-size
  dependence, empty/single/multiple sizes, 128 MiB 4 KiB overwrite with at most
  two chunk reads, tampered blob naming the index, tampered chunk hash →
  `Corrupt`, unknown schema, reopen + GC). Go
  `store/merkle_identity_test.go` 6 tests. Cross-language
  `store/merkle_handoff_test.go` in the FUSE court: Rust→Go and Go→Rust read
  exact bytes, preserve v3, and report the identical root.
- **Bench** (`bench/RESULTS-merkle-identity.md`, receipt
  `bench/receipts/merkle-identity-20261105T161026Z.json`): 128 MiB, 64 KiB
  chunks, 500 random 4 KiB overwrites. Rust whole-file p50 50.43 ms / 67.2 GB
  `rchar` / 1,024,537 chunk reads; Rust Merkle p50 1.81 ms / 87.9 MB / 537.
  Go whole-file p50 202.73 ms / 134.3 GB / 437 MiB peak RSS; Go Merkle p50
  4.38 ms / 87.9 MB / 43 MiB. Every run verified full.
- **Residual.** The leaf fold is O(#chunks) per write (small, but not O(log n));
  no per-level sibling table; the root changes with `chunk_size`; there is no
  explicit v3→v1/v2 conversion command (implicit downgrade is refused); Go's
  v1 whole-file path is unchanged; one host and warm caches.

## Subphase P7 — training-ready is a storage refusal

Implemented in `src/training_ready.rs`, the `DependencyMismatch` error class,
snapshot-aware checkpoint/tensor verification helpers, the CLI verbs
`weft materialize` and `weft training-ready`, the normative
`spec/WEFT-TRAINING-READY.md`, and `tests/training_ready.rs` (10/10).

- **Contract.** A Context Root may be materialized as training-ready only when
  all six preconditions hold at the pinned snapshot: exact untruncated
  resolution; every decoded object verifies (v1 whole-file, v3 Merkle root, v2
  representation decode, symlink digest); every declared and transitively
  reachable causal parent is present (cycles terminate; a missing identity is
  `DependencyMismatch` naming it); every object and selector predicate is
  authorized under the supplied capability/envelope; every tensor logical root
  is recomputed (and every checkpoint manifest is fully decoded and reassembled
  when `--require-tensors` is given); and the estimate and the measured cost
  stay inside `--max-bytes`/`--max-objects`/`--max-decode-ms`. Any failure is a
  typed error, creates no output directory, and leaves no partial file.
- **Commands.** `weft materialize --root N --training-ready --out DIR
  [--max-*] [--capability FILE] [--require-tensors]`;
  `weft training-ready --root N --check`; and
  `weft training-ready --verify-out DIR`. An existing output path is `Exists`.
- **Output.** Verified decoded bytes at their store paths, plus canonical WCB
  `manifest.wcb` (`weft.training_ready.manifest.v1`) and
  `training-ready.wcb` (`weft.training_ready.v1`). No JSON is written into the
  output directory. Publication is a sibling temporary directory renamed into
  place after every check and write succeeds.
- **Reused verification.** `checkpoint::verify_checkpoint_at` and
  `tensor_store::tensor_logical_root_at` resolve and read against the pinned
  state; no second hashing implementation was added. The proof is the store's
  ordinary verified read path, not a trusted field.
- **Tests.** `tests/training_ready.rs` 10/10: happy path with relations and a
  real `checkpoint write` tensor; tampered chunk blob → `Corrupt`, no out dir;
  removed causal parent → `DependencyMismatch` naming the root; budget below
  actual → `BudgetExceeded`; corrupted `weft.tensor.logical_root` → `Corrupt`;
  capability that grants another namespace → `Unauthorized`; second run into an
  existing directory → `Exists`; a causal cycle terminates; check mode writes
  nothing; the CLI check/materialize/verify-out surface. `cargo test
  --all-targets` and `cargo test --features fuse --all-targets` both pass in
  the Docker courts (one pre-existing parallel `tests/quota.rs` lock flake
  passed standalone and on rerun).
- **Real-data demo.** Receipt
  `bench/receipts/training-ready-20261105T171104Z.json`, raw log beside it;
  `scripts/demo-training-ready.sh` in the FUSE court. Corpus: cached
  Qwen/Qwen2.5-0.5B BF16 container (SHA-256
  `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`),
  290 tensors, 988,065,536 element bytes, 2 raw shards per tensor (580 shard
  objects); the store is a copy of the P3 cached base store. Checkpoint write
  66.35 s, max RSS 1,174,908 KiB. `training-ready --check` 77.25 s, 552,156 KiB,
  all six preconditions true, 581 objects, 988,283,592 bytes, estimate 30,034 ms
  vs actual 77,099 ms. Materialize 98.14 s, 552,236 KiB, 581 files, 580 tensor
  objects verified, 290 checkpoint tensors verified. Offline `--verify-out`
  re-reads all 581 files, recomputes 580 tensor roots, reassembles all 290
  checkpoint tensors, and matches the source digest. The materialized
  checkpoint's `source_sha256` equals the source container's SHA-256.
- **Residual.** Only the Rust store implements training-ready (Go parity is
  not claimed); the output contains decoded shard bytes and a canonical
  manifest, not a single restored safetensors container; output verification
  trusts the canonical manifest for paths and only checks that every recorded
  byte and tensor root matches; the demo is one host, warm caches, and a copy
  of a cached public-weight store. The cost estimate is a declared
  throughput model, not a calibration.

## Subphase P8 — conformance manifest, third-party runner, decoder guide

- **Manifest.** `conformance/manifest.json` (295 entries) is generated by
  `scripts/gen-conformance-manifest.sh` (stdlib Python, no Docker) and committed.
  Each entry carries `path`, `kind`, `mode`/`mode_name` where a frame mode
  applies, `expect` (`ok` with `content_root` + `decoded_sha256`, or `error`
  with the normative class), `superseded_by`/`supersedes`, the vector file
  SHA-256, and byte-identical mirror paths. The top-level aggregate is
  `3c981f03fca856c0632ec48d5be166a1ae6513f87b393f934f3555482c2c0bda`;
  `--check` fails on drift and on any cross-repository mirror difference.
- **Third-party contract.** `scripts/run-conformance.sh <adapter>` invokes
  `<adapter> VECTOR.json`, requires exactly one JSON object
  (`ok`/`error`/`skip`), prints
  `VECTOR MISMATCH <path>: expected ... got ...` on any mismatch, and exits
  non-zero. `docs/CONFORMANCE.md` documents the contract, kinds, null
  semantics, skip rules, and exact court commands.
- **Reference adapters.** Rust, Go, and independent Python adapters shell out to
  the shipped CLIs; they never import library code. Committed receipts:
  `bench/receipts/conformance-rust-20261105T180000Z.json` (206 pass / 0 fail /
  89 skip), `conformance-go-20261105T180100Z.json` (206/0/89), and
  `conformance-python-20261105T175000Z.json` (56/0/239). Every skip carries a
  reason; the Python reader implements modes 0–8, and the record-level rejects
  and raw WCB/checkpoint kinds have no public CLI verb in the codec CLIs.
- **Decoder guide.** `docs/WRITE-YOUR-OWN-DECODER.md` gives the normative
  pointers for framing, WCB, hashing, modes 0–14, limits, failure classes, the
  exact check order (header CRC before lengths, trailer CRC after structural
  consistency, decoded length before content hash), frozen/experimental status,
  and a 50-line skeleton.
- **Coverage.** `scripts/check-conformance-coverage.sh` fails on a stale
  manifest, a mode 0–14 without a valid or reject vector, an identity kind
  without a valid vector, an incomplete expectation, or a mirror mismatch. It
  and the manifest check are `conformance_coverage` and
  `conformance_manifest` courts in `scripts/verify-all.sh`, both host-only.
- **Gaps filled.** The review found two identity kinds with no vectors. Added
  `WeftCodec/conformance/v1/capability/` (2 valid envelopes + 5 rejects;
  byte-identical in `WeftCodec-Go`) and `conformance/checkpoint/v1/`
  (`weft.checkpoint.v1` canonical manifest + 4 rejects), with Rust replay tests
  (`WeftCodec/tests/conformance.rs`, `tests/conformance_checkpoint.rs`) and a
  Go capability replay test. All modes 0–14 already had valid and reject
  vectors; no codec-mode vector was added.
- **CLI parity fixes found by the runner.** Rust `capability id --json` now
  reports `canonical_hex`; Go `capability id --json` does the same; Go
  `causal id` accepts an explicitly empty `--parents` (empty closure) while
  still requiring the flag; Go `decode` exposes the tensor/token limit flags.
- **Verification.** `bench/receipts/verify-20261105T174254Z.json` is 27/27
  courts pass at WeftFS `df9dcb0`, WeftCodec `b15353a`, WeftCodec-Go `9d275c0`,
  WeftFS-Go `4278445`. Residual: the reference adapters skip the kinds named
  above; the Python reader is not a full codec; the manifest is a snapshot and
  must be regenerated when vectors are added.

## Subphase Q1 — mount write path (merkle default, write-back, group commit)

Implemented and measured at WeftFS `staging` (this commit) and `WeftFS-Go`
staging (this commit), both buildable and testable only through
`scripts/docker-court.sh`.

- **Merkle identity is the mount default.** `mount` now defaults
  `--content-identity` to `merkle` (explicit `whole-file` still available); every
  non-mount command keeps the frozen `whole-file` default, so existing fixtures
  are unchanged. Reads accept v1/v2/v3. `mount --representation codec-auto`
  requires an explicit `--content-identity whole-file` because a v3 manifest
  carries a raw representation only.
- **`--write-back` (off by default).** Per-handle buffered ranges, published as
  one commit at `flush`/`fsync`/`release`; the writing handle reads its own
  buffered bytes, other readers see the committed version until the flush;
  `fsync` is durable before it replies; a crash loses unflushed ranges. Buffers
  are bounded (32 MiB per handle, early flush on overflow).
- **`--group-commit` (off by default).** Independent mutations share one durable
  commit within a 10 ms window (max 256 ops), also published on `fsync` and
  unmount. A crash leaves the previous whole generation. Publish failures after
  a reply are surfaced as `EIO` on later mutations and logged.
- **Redundant work removed.** `Store::write_file_ranges` applies many ranges
  with one manifest read, one manifest encode and one commit (used by a
  write-back flush); `Store::write_file_cached` reuses a handle's manifest only
  when it still names the object's current content id; `Store::manifest_of`
  seeds that cache. The store exposes `begin_group`/`commit_group`/
  `abort_group`; Go gained the same group API with a pre-image journal and
  `WriteFileRanges`/`WriteFileCached`.
- **Go FUSE parity.** Same `--write-back`/`--group-commit` semantics, same mount
  default, a per-handle buffer and overlay read, and a lifecycle-safe
  background group publisher (`OnForget`, unmount, and tests can stop it
  concurrently).
- **Tests.** Rust `cargo test --all-targets`: 200 passed, 0 failed. FUSE
  all-targets: 234 passed, 0 failed, 2 ignored (both pre-existing: kernel 7.3
  same-inode EIO and `CAP_SETUID`). New `tests/fuse_writeback.rs` (6),
  `tests/range_write.rs` (6), and three group-API tests in
  `tests/group_commit.rs` (11 total). Go `go vet`, `CGO_ENABLED=0 go build`, and
  `go test -race ./...`: all packages pass; new `store/group_test.go` (5) and
  `fusefs/writeback_test.go` (4).
- **Measurement.** `bench/receipts/fuse-writeback-20261105T202300Z.json`
  (single host, synthetic fixtures, buffered I/O, no device claim; 500 × 4 KiB
  overwrites of 128 MiB, 20×10 tree, crash proxy 8/8 pass). Key results:
  write-back takes merkle write p50 from 27.3 ms to 8.8 µs (Rust) and 33.6 ms to
  10.7 µs (Go); group-commit takes create from 162 to 26,548 ops/s (Rust) and
  310 to 6,264 (Go), unlink from 162 to 39,126 (Rust) and 311 to 9,610 (Go);
  whole-file writes are 4.4× (Rust) to 12.6× (Go) slower than merkle by
  default. Group-commit makes the write tail worse (p99 611–804 ms Rust,
  320–432 ms Go) because a request can wait behind a group publish. Crash proxy: unflushed buffered writes lost, flushed/durable kept,
  grouped metadata all-or-nothing, `verify --full` clean in 8/8 cases.
- **Recommendation.** `--content-identity merkle --write-back --group-commit`
  for AI-artifact mounts. Residual: write-back defers visibility (and a crash
  loses unflushed ranges); group commit defers durability; both are opt-in and
  the default remains commit-per-mutation; blob publication inside a group is
  still one durable write per touched chunk, which is why group commit helps
  metadata far more than content writes. This is a process-fault crash proxy,
  not a physical power-loss claim.

## Subphase Q2 — independently decodable tensor tiles (mode 15)

Implemented at WeftFS `staging` (this commit), WeftCodec `staging` and
WeftCodec-Go `staging` (this commit), all buildable and testable only through
`scripts/docker-court.sh`.

- **New payload mode 15 `TENSOR_TILED`** (`weft.ai.tensor_tiled.v1`,
  `WEFT-AI-CODEC` §16). Mode 10 is unchanged. A tensor is split into a
  rank-equal, exactly dividing tile grid; every tile stores its canonical
  row-major bytes as byte planes, each plane coded independently with
  `huffman-canonical-v1`. The descriptor records `tile_shape`, `tile_counts`,
  and per-tile `{index, dims, offset, compressed_len, element_len, tile_root}`.
  `tile_root = H("weft/tensor/tile/v1", WCB({dtype, dims, element_len,
  values_hash}))`; the whole-tensor `LogicalTensorRoot` is unchanged, so tiling
  is a physical representation, not a new identity.
- **Region API in both codecs.** `read_region(descriptor, region)` returns the
  region's canonical row-major bytes and `tiles_touched`, decoding and
  verifying only intersecting tiles. `read_region_with` additionally lets a
  range reader supply the stored tile bytes, so the WeftFS store reads only the
  descriptor prefix and the intersecting tiles' byte ranges from the backend.
  Full decode still verifies every tile root, the logical root, the frame
  trailer, and the content hash.
- **Checkpoint integration.** `checkpoint write --tile-shape A,B,...` writes
  mode-15 shard objects (mode 10 stays the default). A non-dividing tile shape
  is rejected with "no silent padding"; a tiled strategy without a tile shape
  is rejected. Restore detects the shard's representation and uses element
  coordinates for tiled shards; region restores report `units_touched` as the
  number of decoded tiles.
- **Tests.** Rust codec `cargo test --all-targets`: 194 passed, 0 failed,
  including `tests/tensor_tiled.rs` (7: roundtrips, region counts, encoder
  rejects, committed-vector replay 3 valid + 8 rejects). Go codec `go test
  ./...`: all packages pass, including `modes/tensor_tiled_test.go` and
  `conformance/tensor_tiled_test.go` (11 vectors replayed byte-exactly).
  Rust WeftFS `cargo test --all-targets`: 203 passed, 0 failed;
  `tests/checkpoint_tiles.rs` (3) proves exact whole-checkpoint restore, tiled
  region accuracy, two touched tiles for the sampled window, and lower backend
  bytes than mode 10. FUSE all-targets: 237 passed, 0 failed, 2 ignored
  (pre-existing). Go store/fuse `go test -race -count=1 ./store ./fusefs`
  passes.
- **Conformance.** `conformance/v1/tensor-tiled/` has 3 valid + 8 reject
  vectors, byte-identical in both codec repositories. The cross gate prints
  `YES byte-identical tensor_tiled_bf16` and `YES byte-identical
  tensor_tiled_fp32`, and both directions decode to the original bytes.
- **Measurement.** `bench/receipts/tensor-tiles-20261105T222531Z.json`
  (single-host fuse court, one real Qwen2.5-0.5B BF16 tensor
  `model.layers.0.mlp.down_proj.weight`, shape `[896, 4864]`, tile `[32, 256]`,
  10 reps per row, in-process store API). 100-row slice: mode-10 130.1 ms →
  mode-15 12.1 ms (10.8×), backend bytes 7,213,077 → 934,413 (−87.0%), touched
  units 1 shard → 76 tiles. 1-row slice: 130.0 ms → 3.2 ms (40.4×), bytes
  7,213,077 → 281,816 (−96.1%), units 1 → 19. The restored 100-row slices are
  byte-identical between the two strategies. After a tiled restore, the mount
  was read by the unmodified `safetensors` 0.8.0 + NumPy 2.4.6 + ml_dtypes
  0.6.0 loader: 1/1 tensor materialised, tensor SHA-256 equal to the source
  (`db708f9a…d572c6`), 5/5 court checks pass.
- **Residual.** Tiles are full-size only (no partial boundary tiles); the
  descriptor grows with tile count; a prefix range read deliberately skips the
  whole-frame trailer/content-hash check and relies on per-tile roots; the
  benchmark is one host, one public tensor, and a warm in-process store, not a
  fleet or remote-object-store claim; WeftFS-Go does not implement
  `weft.checkpoint.v1` and therefore does not yet write or read tiled
  checkpoints.

## Subphase Q3 — quota accounting inside the commit transaction

Implemented in Rust and Go; normative text in `spec/WEFT-MODEL.md` §2.2 and
`spec/WEFT-QUOTA.md` §1.3–1.5; operational notes in
`docs/quota-branching.md`.

- **Where usage lives.** A quota-enabled commit appends exactly one
  `set_quota_usage` op as its last op. The op carries the cumulative admitted
  usage (sorted principal array, `reset:false`) after all other ops in the same
  commit, or is a standalone `reset:true` op with an empty array. It changes no
  namespace state, so checkpoints are unchanged. The commit schema stays
  `weftfs.commit.v1`; old commits with no usage op replay unchanged.
- **No second durable write.** The store-root `quota` file is now policy-only
  (`WFQ2 || u64le len || WCB || BLAKE3("weft/quota/v2", WCB)`); it is written
  only when a policy is installed/replaced or a legacy record is migrated.
  Ordinary commits perform one commit publication and no quota-file write.
- **Admission and replay.** Admission is still computed before publication;
  denial is `BudgetExceeded: write budget exceeded` with nothing published.
  Replay recomputes the admission totals and rejects a `reset:false`
  assertion that disagrees as `Corrupt`; a reset must be alone and empty.
  `verify --full` validates every assertion and compares the recomputed totals
  with the in-memory counters. `quota rebuild` validates, then publishes a
  corrective op; `quota reset` publishes one reset op. Both are explicit admin
  operations.
- **Migration.** A legacy `WFQ1` record is imported once on writable open:
  usage is recomputed from the chain, the exact legacy bytes are preserved as
  `quota.migrated`, `quota` becomes `WFQ2`, and a corrective usage op is
  published when needed. If `quota` and `quota.migrated` exist with different
  bytes, or a preserved record naming the current head disagrees with committed
  usage, writes fail closed with `Corrupt`. A stale legacy record has its usage
  recomputed from the authoritative chain and the repair is noted.
- **Cross-language.** Rust-written usage ops are read, verified, and enforced
  by Go; Go-written usage ops are read, verified, and enforced by Rust.
  `TestQuotaUsageOpCrossLanguageEquality` compares numeric counters in both
  directions and proves both implementations deny an oversized write.
- **Tests.** Rust `tests/quota_usage_op.rs` 9/9; `tests/quota.rs` 10/10.
  `cargo test --all-targets`: 212 passed, 0 failed. `cargo test --features fuse
  --all-targets` in the fuse court: 246 passed, 0 failed, 2 ignored
  (pre-existing). Go `go test -race -count=1 ./store/...` passes, including 6
  new Q3 tests; `go vet` and `CGO_ENABLED=0 go build` clean.
- **Bench** (`bench/RESULTS-quota-branch.md`, receipt
  `bench/receipts/quota-branch-20261105T224337Z.json`, bench court): 1,000
  single-object commits, p50 11.278 ms without a budget and 11.344 ms with a
  budget (+0.066 ms, +0.59 %); p95 14.928 → 14.848 ms; wall 11.661 → 11.736 s.
  Historical P5a before was 13.754 ms p50 (+21.2 %); Q3 is −17.5 % against it.
  Store bytes 1,926,625 → 2,028,648 (+102,023 B, the 1,000 usage ops). Usage
  after the run is 1,000 objects / 1,383,000 bytes; verify ok.
- **Residual.** Trusting the head assertion on open is O(1); full re-derivation
  and rejection of an inconsistent assertion happen in `verify --full`,
  `quota rebuild`, and any open that must replay the chain. The policy file is
  still the enforcement authority: deleting it disables enforcement. Usage is
  an admission counter, not a reclaimable footprint; deletes do not return
  budget. One host, synthetic fixtures, one repetition.

## Subphase Q4 — real S3-compatible backend and MinIO court

Implemented in Rust on `staging` at `8683ecc` (code commit); not pushed.

- **Backend.** `S3Backend` implements `StorageBackend` over SigV4 with a minimal
  blocking client (`ureq`, rustls-capable). It uses the local logical key
  layout verbatim (`objects/<ab>/<hash>`, `manifests/<ab>/<hash>`,
  `commits/<gen>.commit`, `indexes/...`, `refs/...`, `FORMAT`, `superblock`,
  `LOCK`, `quota`). Blobs use `PUT` + `If-None-Match: *` (identical bytes
  idempotent, different bytes `Exists`), range reads use HTTP `Range`, and the
  two-slot superblock is updated by whole-object `GET` + `PUT` with
  `If-Match: <ETag>`. A lost conditional update retries from a fresh read and
  then fails `Busy`; it never overwrites the head blindly. Writable open probes
  both conditional forms and refuses (`UnsupportedVersion`) if the endpoint
  ignores either. CLI: `--backend s3` plus `--s3-endpoint`, `--s3-bucket`,
  `--s3-region`, `--s3-access-key`, `--s3-secret-key` (or the AWS_* variables).
  `--store` is optional for S3.
- **Lock.** The writer lock is a `LOCK` object carrying an owner, a monotonic
  fencing token, an absolute lease deadline, and the head being published,
  created with `If-None-Match: *`. A live second writer fails `Busy`; a crashed
  writer's lease is reclaimed with `If-Match` at the next fencing token. Since
  R27 the record itself publishes the head (one conditional write), so a stale
  writer cannot publish after a takeover; the superblock is an attested cache.
  This is advisory and is not a POSIX `flock` or a multi-host protocol. See
  [`FENCING.md`](FENCING.md); the Q4 paragraph below records the pre-R27 shape.
- **Capabilities.** `atomic_publish`, `durable_fsync` (provider
  acknowledgement only; no local fsync), `true_range_reads`, max object bound
  1 GiB, consistency `single-node S3-compatible endpoint; read-after-write and
  list-after-write; conditional writes required`. Publication text states the
  `If-None-Match`/`If-Match` protocol.
- **MinIO court.** The official `minio/minio` Docker Hub repository was not
  pullable on 2026-11-05 (registry 404/unauthorized).
  `scripts/s3_court.sh` runs the official linux-amd64 MinIO release binary
  `RELEASE.2025-04-22T22-12-26Z` (sha256
  `53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`) in a
  minimal `debian@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc`
  container with the same hard limits (`3g`, swap `3g`, `1024` pids, `4`
  CPUs, bounded tmpfs, no-new-privileges, `cap-drop=ALL`) and a tmpfs data
  directory. It creates a bucket, runs `tests/s3_backend.rs`, and tears down the
  server and network. `scripts/docker-court.sh` gained a validated optional
  network and forwards only named `WEFT_S3_*`/`AWS_*` variables.
- **Tests.** With MinIO: `tests/s3_backend.rs` 6 passed / 0 failed / 0 ignored
  (init/open/create/read/range/write/truncate/rename/snapshot/`verify --full`;
  live second-writer `Busy`; lost `If-Match` leaves the previous head readable;
  process-fault crash proxy at `after_objects`, `after_commit_file`,
  `after_superblock_write`; GC removes only unreachable content;
  byte-identical local-vs-S3 artifacts). Without the S3 environment each test
  prints `SKIP s3_backend: WEFT_S3_TEST_ENDPOINT is not set` and passes.
  Rust `cargo test --all-targets`: 221 passed / 0 failed / 0 ignored. FUSE
  `--features fuse --all-targets`: 255 passed / 0 failed / 2 ignored
  (pre-existing).
- **Bench** (`bench/receipts/s3-backend-20261105T233154Z.json`, court receipt
  `bench/receipts/s3-court-20261105T233154Z.json`): single-node MinIO on
  localhost, same seeded operations, same head `8e47145e1a09c3bf…`, generation
  1101. 1,000 creates: local 105.9 ms / 9,442 ops/s (p50 100 us, p95 154 us)
  vs S3 1,809.7 ms / 552.6 ops/s (p50 1,781 us, p95 1,967 us). 100 MiB in 100
  files: 192.0 ms / 520.9 MiB/s vs 1,168.3 ms / 85.6 MiB/s; p50 664 vs 10,300
  us. 1,000 reads: 4.4 ms / 224,799 ops/s vs 345.8 ms / 2,892 ops/s; p50 4 vs
  334 us. Backend bytes written 111,581,685 vs 116,091,480 (+4.0%, from
  whole-superblock rewrites and lock/probe traffic). `verify --full` ok on both.
- **Residual.** Rust only. MinIO is one node on localhost with a tmpfs data
  directory: protocol and conditional-write safety, not media durability or
  power loss. The writer lock is a lease (default 900 s; court 2 s); a paused
  writer longer than the lease can lose it. No AWS, GCS, 3FS, cloud,
  multi-node, or fleet claim.

## Q5 — PyTorch Distributed Checkpoint adapter (2026-11-06)

Implemented on `staging` at `d732a9631d0fae395ddacf36d99e816a214dc289`
(code `464b38f`; one test-only follow-up). The Python court now carries
**torch 2.15.0+cpu** (official CPU wheel, 209.5 MB as downloaded). Real
`torch.distributed.checkpoint.save`/`load` ran; this is not a contract-only
fixture and no stock DCP storage plugin was used.

- **Adapter.** `python/weftcodec/dcp.py` implements
  `WeftStorageWriter`/`WeftStorageReader` with `reset`,
  `write_metadata`/`write_data`/`finish`, `storage_meta`, `read_metadata`,
  `read_data`, `set_up_storage_*`, the plan hooks, and
  `validate_checkpoint_id`. Tensor bytes become one canonical
  `weft.checkpoint.v1` checkpoint through `weftfs checkpoint write`; DCP
  protocol metadata is canonical WCB (`weft.dcp.metadata.v1`) at
  `/checkpoints/<name>/dcp.metadata`. Every content root, dtype and shape is
  cross-checked against the Rust manifest on write and after every restore.
  A Rust-written plain-safetensors checkpoint is read back through synthesised
  metadata (`metadata_source == "manifest"`).
- **Topology.** `topology=n` stores n-way axis splits; a non-divisible extent is
  rejected by the Rust CLI ("no silent padding"). The test writes `16x8`,
  `16` and `8x4` tensors 8-way, loads into a fresh module, and restores each
  entry through `--topology 8|4|1`; 3 tensors x 3 topologies were byte-identical
  with the same content root (`10 passed`, `python/tests/test_dcp.py`).
- **Async.** `save_checkpoint_async` runs `dcp.save` on a ThreadPoolExecutor
  worker with an explicit `staging_bytes` bound. Bench: submit 0.72 ms
  (median), completion 1,329.0 ms vs synchronous 1,316.5 ms. Tensors are copied
  through host memory; it is **not a GPU-direct path**.
- **Court.** Built wheel
  `bench/receipts/python-bindings-20261106T000719Z.json`: 191 tests, 0 failures,
  62 skipped. A superseded failing receipt
  `python-bindings-20261106T000301Z.json` is retained: the newly-enabled
  dataset import test walked the repository root across build caches and was
  OOM-killed; the test now uses a bounded fixture root.
- **Bench.** `bench/receipts/pytorch-dcp-20261106T000734Z.json` (revision
  `d732a96`, `weft/python-court:1`, kernel `7.3.1-1-cachyos`, 3 GiB / 4 CPUs /
  1024 pids, single host, CPU, synthetic 8 x 4 MiB f32, no GPU claim). Full save
  1,223.6 ms; full load 152.2 ms; single-tensor load 67.2 ms; staging peak
  4,194,304 B; peak RSS 381,424 KiB; WeftFS physical 31,187,325 B vs
  `torch.save` 33,557,979 B (0.9294); mode-10 tensor-sharded save 913.3 ms /
  32,826,050 B. Rust `checkpoint verify` 8 tensors and `verify --full` ok, no
  problems. The superseded `pytorch-dcp-20261106T000232Z.json` is retained.
- **Docs.** `docs/PYTORCH-DCP.md` (API, metadata schema, exactness table,
  topology evidence, bench, non-claims) and the DCP section in
  `python/README.md`.
- **Not done / residual.** No GPU-direct path, no multi-rank cluster run, no
  media power-loss claim. `BYTE_IO` objects under a split topology need
  divisible lengths (reject, never pad). Reads import restored containers into
  the store. Save/load are ~141x/~30x slower than one `torch.save`/`torch.load`
  file on this fixture, while writing 7.1 % fewer physical bytes.

## Q6 — signed, verifiable receipt bundles with store provenance linkage (2026-11-06)

Implemented on `staging`; packaging and verification were run end to end on a
real ~1,061-file bundle.

- **Canonical manifest.** `src/receipt_bundle.rs` defines
  `weft.receipt-bundle.manifest.v1`: `generated_at`, `stamp`, `files`
  (`path`, `size`, `sha256`), and `revisions` (`name`, `revision`, `branch`,
  `worktree`, `changed_files`). Entries are strictly sorted by canonical encoded
  bytes; duplicate, unsorted, absolute, traversing, backslash, or control-byte
  paths are rejected. Decode re-encodes and requires byte equality, so a
  non-canonical manifest is refused before any signature check.
- **Signing.** `scripts/package-receipts.sh --sign-key FILE` (32 raw bytes or 64
  hex) signs `H("weft/receipt-bundle/v1", manifest_canonical_bytes)` with
  Ed25519 and writes `signature` and `public_key` next to
  `receipt-manifest.wcb`. It uses `weftcodec::capability::sign_bytes` and
  `verify_bytes`, the same code path as capability envelopes. Output is
  `receipts-<stamp>.signed.tar.zst`. Without a key the bundle is
  `receipts-<stamp>.tar.zst`, carries `UNSIGNED.txt`, and says it is unsigned.
  The seed is streamed to the court on stdin (`--sign-key -`) and is never
  copied into the repository or `.docker-cache`; it never enters the archive.
  The manifest, signature, public
  key, and `CHECKSUMS.sha256` are metadata and are deliberately not listed in
  the manifest.
- **Verification.** `scripts/verify-receipts.sh BUNDLE --public-key <64-hex>`
  (or `--signature-from-bundle`) re-hashes every listed file, rejects unlisted
  payload files, decodes the canonical manifest, then verifies the signature.
  It prints `SIGNATURE OK ed25519:<fingerprint>` or fails closed with a precise
  reason. An unsigned bundle fails under either key mode with
  `missing signature file; bundle is unsigned`; a wrong key fails with
  `public_key file does not match the requested key`; tampering fails with
  `sha256`, `unlisted payload file`, or `signature does not verify under ...`.
  Legacy bundles without `receipt-manifest.wcb` still verify structurally and
  report `signature: not applicable`.
- **Tests.** `tests/signed_bundle.rs` 11/11: sign/verify round trip, wrong key,
  tampered same-size file, missing signature, unsigned-with-key, unsigned
  default, wrong-key library path, deterministic manifest/digest/signature,
  non-canonical and trailing-byte rejection, traversal rejection, unlisted
  payload, and `weft provenance show` linkage. Rust `cargo test --all-targets`
  234 passed / 0 failed across 66 test binaries; FUSE `--features fuse
  --all-targets` 268 passed / 0 failed / 2 ignored; Go `go test ./...` in
  `WeftCodec-Go` passed every package. Two earlier full runs hit pre-existing
  flakes (`tests/quota.rs` writer lock, `tests/representation.rs` chunk
  counter); each passed standalone and the published full run was clean.
- **Store linkage.** `scripts/demo-signed-bundle.sh BUNDLE` stores the canonical
  manifest as a real file, creates snapshot `q6-signed-bundle`, records a
  `weft.provenance.v1` receipt whose output is the stored manifest's content
  root, and publishes `derived_from` to the snapshot. `weft provenance show
  /bundle.manifest` resolves the provenance id, the `derived_from` input, and
  the snapshot id; `weft relation list --predicate derived_from` resolves the
  edge. Tampering changes both the signed digest and the recomputed output
  content root, and the old signature no longer verifies.
- **Go cross-check.** `WeftCodec-Go/capability` gained `SignBytes`/`VerifyBytes`
  (same `crypto/ed25519` path) and `cmd/verify-ed25519` verified the Rust
  signature over the real manifest digest: `VERIFY OK`; a changed digest
  produced `VERIFY FAIL`. This was run in the Go court.
- **Bench** (`bench/receipts/signed-bundles-20261106T010118Z.json`, raw log
  beside it; single host, clean worktree at revision `259c29f`, ephemeral
  benchmark-only keys, kernel `7.3.1-1-cachyos`, 3 GiB / 4 CPUs / 1024 pids):
  1,064 payload files, 1,139 tar entries. Signed package 19,648 ms; unsigned
  12,091 ms. Pinned-key verify 9,029 ms; unsigned verify 8,981 ms. Signature
  64 raw bytes (129-byte hex file), public key 65 bytes, manifest 128,731 bytes;
  raw metadata added (manifest + signature + public key − `UNSIGNED.txt`)
  128,633 bytes. The observed compressed bundle delta was −54 bytes because the
  two runs have different timestamps and one has `UNSIGNED.txt`; it is not
  attributable to signing. All 15 rejection/acceptance cases passed, including
  the Go checks, the seed-not-cached and seed-absent-from-bundle checks, and the
  provenance demo.
- **Non-claims.** No certificate authority, no transparency log, no key
  distribution or rotation, and no claim that the signer is trustworthy.
  Signing proves bundle integrity relative to a key, not the truth of the
  measurements. Verification is offline; it uses the local repository and its
  Docker court. A leaked seed permits forged bundles under that key. Bundle
  timings are one host and warm caches, not a fleet or storage-media claim.
  The canonical manifest is an advisory artifact for this bundle format, not a
  new frozen WeftCodec wire mode; the frozen v1/v2 store and codec formats are
  unchanged.

## Q7 — v0.8.0 release (2026-11-06)

v0.8.0 is cut from `staging` after the Q1–Q6 subphases. Versions:
`weftfs` 0.8.0 and `weftcodec` 0.8.0 in `Cargo.toml` and `Cargo.lock`; the
parent pins `weftcodec = "0.8.0"`. Python wheel
`python/dist/weftcodec-0.8.0-py3-none-any.whl`, 45,400 bytes, sha256
`5a41c419a43e9d7ba16dd14c2d170ecd34d102f16c69d51652af5245e0fe91ab`, built and
tested from the installed wheel in the Python court (191 tests, 0 failures,
0 errors, 62 skipped; receipt
`bench/receipts/python-bindings-20261106T012031Z.json`). **Not published to
PyPI** (no token); the wheel is a release asset.

Release verification: `bench/receipts/verify-20261106T011444Z.json`, **27/27
courts pass**, at WeftFS `d6ba189`, WeftCodec `3f3eae0`, WeftCodec-Go
`65bc14d`, WeftFS-Go `f920ae0` (before the version-literal bump). The retained
first run `bench/receipts/verify-20261106T010756Z.json` failed on the
pre-existing quota writer-lock flake; the retained Python receipt
`bench/receipts/python-bindings-20261106T011946Z.json` failed because the shared
release binary was fuse-linked and the Python court image has no
`libfuse3.so.3`. Both clean reruns are the published receipts. Full note with
every loss: `docs/RELEASE-v0.8.0.md`.

Subphase evidence: Q1 `bench/receipts/fuse-writeback-20261105T202300Z.json`,
Q2 `bench/receipts/tensor-tiles-20261105T222531Z.json`, Q3
`bench/receipts/quota-branch-20261105T224337Z.json`, Q4
`bench/receipts/s3-backend-20261105T233154Z.json` and
`bench/receipts/s3-court-20261105T233154Z.json`, Q5
`bench/receipts/pytorch-dcp-20261106T000734Z.json`, Q6
`bench/receipts/signed-bundles-20261106T010118Z.json`.

## R3 — mode 16 `SPARSE_DELTA` (2026-11-06)

Additive payload mode 16 (`weft.ai.sparse_delta.v1`) in both codecs and both
stores. Mode 11 `WEIGHT_DELTA` is untouched and byte-identical; mode 16 is the
sparse counterpart, not a replacement.

- **Format.** Exactly nine descriptor keys (`schema`, `dtype`, `shape`,
  `element_count`, `storage_bytes`, `op` = `mod-sub`, `sparse_codec` =
  `zero-run-v1`, `base_content_root`, `delta_root`); one dependency naming the
  base tensor; body is `u32le(meta_len) || WCB(meta) || u32le(run_count) ||
  run_count records`, each record `varint(zero_run_len) ||
  storage_bytes(dtype) bytes`. Trailing zeros are implicit; an all-zero delta is
  `run_count = 0`; a stored zero word and trailing bytes are rejected. Spec:
  `spec/WEFT-AI-CODEC.md` §17; mode tables and the reserved range (now
  `17..=127`) updated in `WEFTCODEC-FORMAT` and `WEFT-COMPATIBILITY`.
- **Limit.** New `max_sparse_runs` (default 16,777,216, CLI
  `--max-sparse-runs` in both codecs). A denser delta is `LimitExceeded` at
  encode and decode; the store planner then keeps mode 11.
- **Vectors.** `conformance/v1/sparse-delta/` 18 files (4 valid: all-zero, one
  word, several runs ending exactly at `element_count`, fully dense;
  14 rejects), byte-identical in both codec repositories.
  `017_unknown_mode_reserved16` is retained byte-for-byte and marked
  `superseded_by` `017_unknown_mode_reserved17`. Manifest 326 entries
  (aggregate `1fc710a8…`), coverage mode 16 = 4 valid / 14 reject. Rust and Go
  reference adapters replay 326 total, **224 pass, 0 fail, 102 skip**, no
  sparse-delta skips.
- **Cross-language.** `WeftCodec-Go/hack/cross.sh` gains three mode-16 fixtures;
  `YES byte-identical sparse_delta_bf16 / sparse_delta_q8 /
  sparse_delta_fp32` and decode equivalence in both directions; the whole gate
  reports "all checks passed".
- **Planner.** `TensorStrategy::SparseDelta` / `CostClass::SparseDelta` in Rust
  and Go, offered next to mode 11 whenever a causal parent has the same
  dtype/shape and priced by the same integer cost table (the SparseDelta row
  **reuses** the measured weight-delta coefficients; it was not separately
  calibrated and the table says so). `tests/planner_sparse.rs` (3 tests): a
  19-word change selects mode 16 and reads back exactly with an independently
  recomputed `delta_root`; a fully dense delta selects mode 11; a forced dense
  mode-16 object is still readable after reopen. Go reads and verifies
  Rust-written mode-16 objects and vice versa (`TestTensorHandoff*` pass in
  both directions with both modes).
- **Measurement** (`bench/receipts/sparse-delta-20261106T052333Z.json`,
  bench court `sha256:4041d66e…`, revisions `weftfs eac3687`, `weftcodec
  45a319b`, `weftcodec_go e47fa03`, `weftfs_go 47fd4d2`; analysis
  `bench/RESULTS-sparse-delta.md`). Consecutive pythia-70m pairs:
  step0→step1 mode 16 **29,887 B vs 7,434,959 B** mode 11 (0.0040×; 0.0002×
  per-tensor zstd-19; 0.988× the whole-file `zstd --patch-from -3` recorded by
  P2), 76/76 tensors smaller. step1→step2 0.647× (27/76 tensors smaller);
  step2→step4 **2.22× (loss)**; step4→step8 **3.29× (loss)** with
  `embed_out.weight` refused by `max_sparse_runs`. Fine-tune Qwen
  base→instruct: **1.94× (loss)**, 48/290 tensors smaller,
  `model.embed_tokens.weight` refused. Mode 16 encoded and decoded faster than
  mode 11 in every pair (decode 653–815 MB/s vs 290–409 MB/s consecutive;
  319 vs 195 MB/s fine-tune) but both remain 1.8–9.6× slower than `zstd -19`.
  Consequences: the planner now stores a 19-byte checkpoint change in ~30 KB
  instead of ~7.4 MB, and keeps mode 11 for dense pairs instead of paying the
  run stream.
- **Tests.** Rust codec `cargo test --all-targets` all pass (new
  `tests/sparse_delta.rs` 12/12); parent `cargo test --all-targets` 257 passed /
  0 failed and `--features fuse --all-targets` 291 passed / 0 failed / 2 ignored
  (the two pre-existing ones); Go codec `go test ./...` all packages pass; Go
  store `go test -race -count=1 ./...` store package passes and `./fusefs`
  passes in the fuse court.
- **Changed expectations (honest, not hidden).** Two Rust planner tests and one
  Go planner test used a sparse fixture and expected mode 11; the smaller
  representation is now mode 16, so they expect mode 16 and assert mode 11 is
  still priced and larger. The mode-reserved structural tests moved 16 → 17.
  Cross-language handoff fixtures now write both an auto (mode 16) and a forced
  mode-11 object.
- **Residual.** Single host and warm cache; the receipt is codec-level
  per-tensor frames, not store-planner bytes; zstd baselines are copied from a
  prior receipt; mode 16 is not entropy-coded, so medium-density deltas still
  lose to mode 11; `max_sparse_runs` refused one tensor in each of two pairs.

## Subphase R5 — entropy-coder court (2026-11-06)

Experimental, no wire change. `WeftCodec/src/experimental/entropy/` adds a
Tunstall coder (k=8/10/12), a static order-0 32-bit rANS coder, zstd levels
3/19, raw, and the shipped `huffman-canonical-v1` as the baseline. No
persistent encode/decode path calls the module; the court script fails if a
production source file references `experimental::`. No wire mode, table, or
frozen format changed.

- **Inputs.** Real cached weights (all 7 files re-hashed in court; manifest
  sha256 `89fce691…`): Qwen2.5-0.5B/Instruct BF16 and pythia-70m step0/1.
  Plane inputs are extracted through the shipped encoders for modes 9 (exponent
  and sign+mantissa), 11 (dense fine-tune and sparse step0→step1 residuals,
  anchored at changed row 2304), 12 (1 MiB KV page), and 15 (tile [64,128],
  532 tiles). Workload shapes: whole, 100 rows, 1 row (KV: page, 100 rows,
  1 row). 252 rows, 14 coder configurations.
- **Correctness.** Every row round-trips byte-exactly. `tests/
  entropy_experimental.rs` 10/10 covers empty/one-symbol/all-256/skewed/
  incompressible/non-granular inputs, the documented Tunstall rule, shared
  dictionaries over unseen symbols, malformed headers, and 5,000 randomised
  cases plus 5,000 mutated streams each for Tunstall and rANS against
  independent reference decoders. WeftCodec and parent `cargo test
  --all-targets` pass. Cross gate: all checks passed, modes 14/15/16
  byte-identical. Conformance replays: Rust and Go 224 pass, 0 fail, 102
  skip against manifest
  `1fc710a88e610290e65810d6b0c53a687d02b7316cd7cc5626dd195c07a1dcc0`.
- **Bytes.** rANS is the smallest coder on mode-9 BF16 whole planes
  (**5,753,076 B**, 0.6600; 0.86% under zstd-19 and 0.64% under Huffman),
  mode-11 dense whole planes (**4,357,033 B**, 0.4999; 2.40% / 3.58% under
  them), mode-15 tiles (5,804,101 B), KV pages (698,896 B), and the sparse
  whole planes (**1,188 B** vs 3,288 B zstd-19). zstd-19 is smallest on FP32
  mode-9 (**2,014,728 B**, 12.1% under rANS) and on 1-row regions.
- **Speed.** Shipped Huffman 1,161/1,229 MiB/s encode/decode; rANS
  312/305 MiB/s; zstd-19 28.5/9,652 MiB/s; zstd-3 1,807/6,808 MiB/s.
  rANS is 2.0× slower than Huffman on BF16 decode and 23.7× slower than
  zstd-19; on sparse whole planes the gap is 74.4×.
- **Tunstall.** k=8 cannot split (`256+255 > 256`), so it is raw plus
  dictionary (ratio 1.0001–1.0002). k=10/12 compress some planes but are
  47–59% above the best coder on non-sparse whole planes and ~3,000× above
  zstd-19 on the sparse whole planes; on 1-row regions their total is
  6.6–30.5× the best non-sparse coder. Tunstall wins nothing.
- **Dictionary scopes.** Per-plane is the best byte scope at k=12 for whole
  and 100-row multi-plane sets in five of six classes (34,465 B mode 9/tiles,
  38,290 B dense, 34,210 B KV, 171,440 B sparse). The sparse whole planes
  are the exception: 171 KB per-plane loses to 42,860 B per-tensor on total
  bytes. Per-tensor is not a default. Family-shared is an amortisation tool,
  not a compression win: 18,125–42,860 B charged once and amortised to
  62–564 B per tensor (N=290/76/169) or 2,234 B for 8 KV pages; it does not
  improve stream bytes. At 1-row granularity every static dictionary loses.
- **Memory.** The dense full-checkpoint family histogram peaked at
  **2,248,448 KiB (2.14 GiB)** through the shipped delta encoder on the 272 MB
  embedding; the per-row matrix peak was 520,708 KiB. Inside the 3 GiB court,
  about 0.86 GiB headroom.
- **Recommendation: Promote nothing.** Keep `huffman-canonical-v1`. rANS's
  0.5–3.6% size win is not worth a wire change plus a 2× decode loss; zstd-19
  is a size reference but its 28.5 MiB/s encode rules it out as a write path;
  Tunstall is rejected at every k and scope. Conditional follow-up only:
  rANS with a decoder at Huffman throughput; a frame-based coder if 1-row
  reads become first-class.
- **Evidence.** Receipt `bench/receipts/entropy-coder-20261106T065154Z.json`
  (252 rows; supersedes `entropy-coder-20261106T064820Z.json`, whose build-peak
  field was captured after a reset and is not meaningful). Revisions `weftfs
  4e5cb95`, `weftcodec 72cd119`, `weftcodec_go e47fa03`, `weftfs_go 47fd4d2`.
  Analysis `bench/RESULTS-entropy-coders.md`. Court runner
  `bench/scripts/entropy_coder_court.sh`, renderer
  `bench/scripts/render_entropy_results.py`.
- **Residual.** One host/warm cache; one tensor per class; codec-level plane
  streams, not a shipped frame or store planner; no zstd dictionary/long-window;
  rANS not micro-optimised; no substring-frequency Tunstall dictionary.

## Subphase R6 — Python reader: all AI modes 0–16 (2026-11-06)

The independent Python reader (`python/weftcodec/`) now implements every
allocated WFT1 payload mode: 0–8 as before, plus 9 `FLOAT_TENSOR`, 10
`TENSOR_SHARDED`, 11 `WEIGHT_DELTA`, 12 `KV_CACHE`, 13 `ACTIVATION_STREAM`, 14
`SEMANTIC_DELTA`, 15 `TENSOR_TILED`, and 16 `SPARSE_DELTA`. No wire format,
vector, descriptor, or manifest changed.

- **New code.** `python/weftcodec/huffman.py` (212 lines) is a standalone
  `huffman-canonical-v1` decoder with the §5.3.1 validation order and classes
  (length > 24 and Kraft over/incomplete → `NonCanonical`; truncated stream,
  trailing bytes, non-zero padding → `Corrupt`). `python/weftcodec/ai_modes.py`
  (2,570 lines) implements descriptor parsing through the existing canonical
  WCB reader, FLOAT_TENSOR plane join for bf16/fp16/fp32/fp8-e4m3/fp8-e5m2
  (bit-exact, including NaN payloads, denormals, signed zeros), q4_0/q8_0
  shard dequantised-byte reconstruction and shard-set roots, WEIGHT_DELTA and
  SEMANTIC_DELTA modular word reconstruction with `delta_root` and semantic-root
  verification, KV_CACHE and ACTIVATION_STREAM byte planes (plus `stream_root`),
  TENSOR_TILED per-tile roots and the whole-tensor logical root, and
  SPARSE_DELTA `zero-run-v1`. `modes.py` dispatches all sixteen modes;
  `dependency.py` gains frame-source resolution (`resolve_frame`,
  `FrameSourceResolver`, cycle/depth/fetch/decoded bounds); `cli.py` gains
  `--base`, `--resolve`, and `--frame-source`. Total reader core (excluding the
  optional `dcp.py`/`torch.py`) is 5,288 lines; the new AI code is
  2,782 lines (`huffman.py` + `ai_modes.py`).
- **No new dependency.** The reader keeps its existing `blake3` and
  `zstandard` dependencies and adds none (Huffman and all AI payload work are
  pure standard-library Python).
- **Conformance adapter.** `scripts/conformance/adapters/python.py` now imports
  the package directly and executes codec frames, adversarial decode vectors,
  shard sets, delta bases (including frame-source cycle vectors), KV pages,
  activation chunks, tiled frames, semantic roots, and canonical WCB values.
  Skips are explicit and only for surfaces the reader does not claim.
- **Court command and result.**
  `bash scripts/docker-court.sh python "cd /work && PYTHONPATH=/work/python WEFT_PYTHONPATH=/work/python scripts/run-conformance.sh scripts/conformance-adapter-python.sh --quiet --json bench/receipts/conformance-python-20261106T072745Z.json"`
  → **326 total, 218 pass, 0 fail, 108 skip** (manifest
  `1fc710a88e610290e65810d6b0c53a687d02b7316cd7cc5626dd195c07a1dcc0`). The
  previous Python result was 56/0/270. No mode 0–16 vector is skipped.
  Remaining skips: 29 WEFT-TENSOR layout/shard containers, 22 selector, 17
  Context Root (including `causal_versions`), 10 causal closure, 7 capability,
  5 checkpoint, 7 WFA1 archive verbs, 2 `selector-id` adversarial verbs,
  2 composite store/fuzz fixtures, and 9 superseded vectors.
- **AI vector suite.** `python/tests/test_ai_modes.py` asserts **128 AI vectors
  (40 valid, 88 rejects)**, 0 skipped. The suite reports 128 AI vectors (40 valid, 88 rejects). Valid vectors decode to their recorded
  bytes and recompute every recorded root (`content_hash`, `layout_root`,
  `delta_root`, `logical_root`, per-tile roots, `stream_root`, semantic root);
  every reject raises the declared class. The same test decodes each valid
  frame with the Rust CLI and requires identical bytes (and, for mode 10, the
  same `tensor root --frames` value).
- **Randomized differential.** `python/tests/test_random_differential.py`
  generates **16,003 frames** (at least 2,000 per mode 9–16) from a
  deterministic xorshift64* PRNG via `python/tests/ai_frames.py`, decodes each
  with the Python reader and
  `/work/.docker-cache/target-rust/release/weftcodec decode`, and asserts
  identical accept/reject and identical bytes. The court reports 1,600 body mutants. Counts: mode 9 2,000; mode 10
  2,002 (627 shard-set roots, 627/627 matching); mode 11 2,000; mode 12 2,000;
  mode 13 2,001; mode 14 2,000; mode 15 2,000; mode 16 2,000. All 16,003
  frames were accepted by both and byte-identical. A further **1,600 body
  mutants** (CRC repaired so they reach semantic checks) agree on
  accept/reject in 1,600/1,600 cases and on error class in 1,581/1,581
  meaningful reject pairs; 19 mutants remain valid and decode identically.
- **Python test receipt.** `bench/receipts/python-bindings-20261106T072822Z.json`
  records **331 tests, 0 failures, 0 errors, 9 skips** (the skips are the
  superseded historical `017_unknown_mode_reserved*` vectors). The receipt records 331 tests, 0 failures, 0 errors, 9 skips. Wheel
  `weftcodec-0.8.0-py3-none-any.whl`, 66,163 bytes, sha256
  `39898a4a0d206d12528f32f6d0282f7e261f4bc8a5105eb127d701854f12c6ba`.
  Court image `weft/python-court:1` id
  `sha256:af751fc866908fb6e6abb190c6e139739b62246ea019447a6a754e5c3b2a849e`
  (3 GiB / 4 CPUs / 1,024 pids).
- **Court image detail.** `docker/python-court.Dockerfile` is now pinned to
  `python:3.12-slim-bookworm` (the floating `python:3.12-slim` moved to
  trixie, where `libfuse3-3` no longer exists) and installs `libfuse3-3` so the
  fuse-linked `weftfs` CLI used by the existing DCP tests runs in this court.
  No `/dev/fuse` is passed to the Python court.
- **Honest residual.** The Python reader still does not claim WeftQL selectors,
  Context Roots, capability envelopes, `weft.checkpoint.v1` manifests, WFA1
  archives, or `weft.tensor.layout.v1` / `weft.tensor.shard.v1` records;
  TENSOR_TILED supports full decode but not the Python region reader. The
  randomized differential is one host with synthetic frames (Rust CLI is the
  oracle); overflow and absurd-limit vectors are covered by the curated
  conformance corpus, not by the generator. No Go code, no wire change, and no
  manifest regeneration was performed.

## Subphase R7 — shared multi-mount daemon (2026-11-06)

One process can serve N read-only context mounts over one store, sharing the
store state, selector index cache, and backend handle instead of paying a
process, a thread pool, and a replayed snapshot per mount.

- **Architecture.** `src/fuse.rs` now reaches the store through `StoreHandle`:
  `Exclusive(Arc<Mutex<Store>>)` for a single mount (unchanged behaviour) or
  `Shared(Arc<Store>)` for shared views. A shared view borrows `&Store`
  immutably and can never obtain `&mut Store`, so create/write/truncate/
  setattr/xattr/rename/link/unlink/mkdir/mknod/symlink/fallocate/
  copy_file_range are refused with `ReadOnly` → `EROFS`, the same class a
  single `--read-only` mount uses. Pins are skipped for shared views (they need
  `&mut Store`, and this process runs no writer or GC).
- **Memory.** `MountOptions.snapshot_state` is `Option<Arc<State>>`, and the
  CLI replays each snapshot at most once per server start (`cached_state`), so
  N views pinned to one commit hold one `State`, not N clones.
- **API.** `SharedViewSpec`, `SharedMount` (`start`, `mountpoints`,
  `store`, `is_running`, `any_running`, `unmount`) and `run_shared` in
  `weftfs::fuse`. One `fuser` session thread per view plus the supervisor;
  `MAX_SHARED_VIEWS = 256`, refused with `LimitExceeded` before mounting.
  Starting is all-or-nothing and SIGINT/SIGTERM unmounts every view.
- **CLI.** `weftfs --store DIR mount-many --spec FILE [--allow-other]`. The
  spec is strict JSON (`version: 1`, `views[]` with `mountpoint` and optional
  `snapshot` | `context` | `at`, plus `capability`); unknown fields, duplicate
  mountpoints, and unknown versions are rejected. The single-mount CLI is
  unchanged. Documented in `docs/shared-mounts.md`.
- **Demo.** `scripts/demo-context-mounts.sh` accepts
  `WEFT_CONTEXT_MOUNTS_MODE=shared` and starts one process for all N views;
  the per-daemon mode and its receipts are unchanged. New knobs:
  `WEFT_CONTEXT_CORPUS_BUILD=cli|harness`, `WEFT_DEMO_SKIP_BUILD`,
  `WEFT_DEMO_BUILD_ONLY`, `WEFT_WAIT_MOUNT_TRIES`; new metrics
  `mount_mode`, `mount_processes`, `all_mounted_ms`,
  `mount_latency_min_ms`, `verified_files_sha256`, and
  `listing_digests_sha256` (a digest over every per-view listing digest, so
  two runs can be compared from receipts alone).
- **Court.** `bash bench/scripts/shared_mounts_court.sh 100` runs the same
  oracle-built corpus four times: per-daemon and shared at 3 GiB, then both at
  a 512 MiB limit. It requires identical listing digests, identical file
  SHA-256 counts, identical verified mount counts, a clean shared run at
  512 MiB with zero OOM kills, and a per-daemon run at the same limit that
  does not complete. Receipt
  `bench/receipts/shared-mounts-20261106T082042Z.json`, 19/19 checks pass;
  analysis `bench/RESULTS-shared-mounts.md`.
- **Measured at N=100** (same corpus, same verification, binaries built once):
  per-daemon 100 processes / 692 threads / summed RSS 1,963,056 KiB /
  cgroup peak 1,351,827,456 B / 100 mounts verified / 425 file hashes;
  shared 1 process / 102 threads / RSS 27,692 KiB / cgroup peak
  69,402,624 B / 100 mounts verified / 425 file hashes. The digest over all
  100 per-view listing digests is identical
  (`1d30af9ccebcab723acfdc0cb9ceee56a6fe93a73c8be0220590593eb714566e`) in
  per-daemon at 3 GiB, shared at 3 GiB, and shared at 512 MiB. cgroup peak is
  19.5x smaller, summed RSS 70.9x smaller, threads 6.8x fewer.
- **Small limit.** The same N completed in shared mode at 512 MiB with peak
  73,646,080 B (70.2 MiB), i.e. 441.8 MiB of headroom. Per-daemon at the same
  512 MiB mounted 51/100 and lost 49 daemons to cgroup OOM kills
  (49 `oom_kill` events, peak pinned at the limit).
- **Tests.** `tests/shared_mounts.rs` 8/8: eight shared views match eight
  equivalent single mounts (listing digest and every file byte), write-like
  operations fail with `EROFS` and change nothing, a root-scoped capability
  view can read its granted exact id but gets `ENOENT` for an unauthorized one
  while an unscoped sibling can read it, unmounting one view leaves the others
  serving and the supervisor exits after the last, one session thread per view
  with an oversized spec refused `LimitExceeded`, no panic escapes a view
  under invalid operations, and the spec schema rejects unknown fields,
  duplicates, and a writable field. Full
  `cargo test --features fuse --all-targets`: **307 passed / 0 failed /
  2 ignored** (the two pre-existing ignored tests).
- **Losses.** Time to the *first* view is 3,964 ms vs 523 ms per-daemon (one
  process resolves all contexts and creates all mounts sequentially); time to
  all 100 is 4,064 ms vs 4,428 ms, so completion is not worse. One backend
  mutex serialises physical I/O across views (not measured under concurrent
  large reads), and one process is one failure domain. Shared views are
  read-only by construction; writes stay on the single-mount path.
- **Not done.** No Go frontend parity, no per-view streaming start (resolve
  and mount one at a time), no shared-mount run at 1,000, no concurrent
  large-read comparison against independent daemons, no multi-host claim.
- **Verification.** Full `scripts/verify-all.sh` at the R7 docs tree
  (`a4e2c3d`): **27/27 pass**, receipt
  `bench/receipts/verify-20261106T083428Z.json` (WeftFS `a4e2c3d`, WeftCodec
  `72cd119`, WeftCodec-Go `e47fa03`, WeftFS-Go `47fd4d2`). The first run at
  the same tree, `verify-20261106T082853Z.json`, failed only `rust_store`:
  `tests/representation.rs::codec_auto_round_trips_compressible_data_and_decodes_only_intersecting_chunks`
  observed 5 decoded chunks instead of 1 because that test reads a
  process-global codec decode counter while other test binaries' tests run in
  parallel; it passes with `--test-threads=1` and in the full rerun. The
  failing receipt is retained. `check-claims.sh` 258/258 OK;
  `check-spec-drift.sh` 42/42 OK (no spec change).

## Subphase R8 — reconstruction single-flight and dependency-aware cache (2026-11-06)

Code in `src/singleflight.rs`, `src/object_cache.rs`, the read path in
`src/store.rs`, and the CLI/FUSE wiring; court harness
`examples/cache_bench.rs` + `bench/scripts/cache_court.sh`; analysis
`bench/RESULTS-cache.md`.

### Flake fix (committed first, `tests: make the codec chunk-decode counter
per-thread`)

`codec_chunk_decode_count` was one process-global atomic. libtest runs the
tests of one binary on a thread pool, so an unrelated test that decoded chunks
on another thread inflated
`representation::codec_auto_round_trips_compressible_data_and_decodes_only_intersecting_chunks`
(the retained `verify-20261106T082853Z.json` observed 5 chunks instead of 1).
The observation is now **thread-local** — exactly the work the calling test
performed, because representation reads decode on the calling thread — and the
process-wide aggregate remains available as `codec_chunk_decode_total`. The
assertion is unchanged. New regression test
`chunk_decode_counter_observes_only_the_calling_thread` decodes four chunks on
another thread, waits for it to finish, and proves this thread still observes
0 and then exactly 1. Verified with **20 consecutive parallel runs** of
`cargo test --release --test representation -- --test-threads=8`: 9/9 pass each.

### Single-flight (implemented)

`SingleFlight<K, V>` is a bounded `Mutex` + `Condvar` registry (no `unsafe`).
Identity is `(object id, content/representation version, region, verified)`;
a whole read and a range read covering the whole object coalesce. One leader
runs the read+decode; identical concurrent callers wait and receive a clone of
the leader's result or its exact error. A panicking operation is converted to
`Corrupt` so waiters can never hang. Cancellation is a waiter's decision only
(`CancelToken`, polled every 20 ms, new `Class::Cancelled`): the leader always
finishes for the others, and a token already cancelled before leadership
declines to start. `max_entries` (default 64) and `max_bytes` (default 256 MiB)
are hard admission bounds; a request that does not fit runs independently and
increments `degraded` — it never queues and never deadlocks. Off by default.

### Decoded-object cache (implemented)

`ObjectCache` holds the **verified decoded bytes**, keyed by
`(object id, content version)`: a new version has a new content root and can
never hit an old entry (an admission also removes the old version's entry).
Admission/eviction score is
`decode_cost_ns(class, decoded_bytes) * dependent_count * observed_read_frequency`
from the existing measured cost table, the `derived_from` relation graph, and
the existing advisory access counters; ordering is exact integer
cross-multiplication of cost per byte, then lower score, then larger bytes, then
lower content/object id. Shared bases with many dependents outrank one-off
large tensors. `pin_cache_object` makes an entry unevictable; if pins fill the
bound, unpinned admission is refused (the bound is never exceeded). Sized by
`--cache-bytes` (default 0 = disabled) plus a `--cache-entries` bookkeeping
bound. Counters: hits, misses, admissions, evictions, stale removals,
rejections, bytes served, pinned skips. In process memory only; absent from
every frozen format and from commits/verify/GC.

### Authorization

The cache and single-flight are only reachable through an `AuthorizedRead`
token produced by `Store::authorize_read` / `authorize_read_at` /
`authorize_read_in` (POSIX mode + `weft.acl.read` + capability-envelope
validity). `read_object_authorized`, `read_range_authorized`,
`read_content_authorized` (plus `*_cancellable`) take the token, and dependency
reconstructions inherit it. A denial is `Unauthorized` and never consults the
cache; `read_content_authorized` refuses a token for a different content root.
Internal reads (`verify`, planning, rebase, GC, recovery) keep the shipped
uncached path. FUSE gates the layers with the request's uid/gid, and only when
a layer is enabled (default off = unchanged behaviour).

### Evidence

Receipt `bench/receipts/cache-20261106T091322Z.json` (**26/26 checks**, at
WeftFS `dbeaf0a`) in the bench court on the real Qwen2.5-0.5B/Instruct pair
(both files re-hashed), 26 objects / 226,623,488 logical bytes per round, 4
rounds, 256 MiB cache: `none` 1 thread 160 decodes / 754,432,204 B read /
p50 61.17 ms / 6,440.7 ms; `singleflight` 4 threads 160 decodes /
754,570,204 B / 6,384.4 ms (vs `none` 4 threads 640 decodes / 3,017,728,816 B
/ 6,645.9 ms); `cache` 1 thread 26 decodes / 107,411,220 B / p50 0.224 ms /
1,121.9 ms; `both` 4 threads 26 decodes / 107,549,220 B / 1,140.6 ms. Every run: all bytes hash-equal and
`verify --full` ok. Adversarial 5/5: waiter-only cancellation, error fan-out,
disabled cache, pinned base retained, unpinned base evicted. Losses recorded in
`bench/RESULTS-cache.md`: single-flight is not a wall-time win on 4 otherwise
idle vCPUs (work −75 %, wall −3 %), single-flight at 1 thread is neutral,
cache-only still races on the first round (104 vs 26 decodes), the cache costs
+233 MiB RSS at 1 thread for a 226 MB working set, a bound below the working set
re-decodes, p99 stays bimodal, and the cache is per process.

Tests: `tests/singleflight.rs` 9/9, `tests/object_cache.rs` 10/10, unit tests
in both modules, `tests/representation.rs` 9/9. `cargo test --release
--all-targets`: **299 passed / 0 failed** (75 binaries). `cargo test --release
--features fuse --all-targets`: **341 passed / 0 failed / 2 ignored** (the two
pre-existing ignored tests). `check-claims.sh` and `check-spec-drift.sh` pass;
no normative spec changed (R8 is read-path only, no wire/format change).
Not done: no Go parity, no mode-15 whole-object caching, no FUSE range caching,
no persistence across processes, no multi-host claim.

## Subphase R9 — immutable write overlays and compaction (experimental, Rust write path)

`weft.manifest.v4` (`spec/WEFT-MODEL.md` §2.3.3) adds `kind:"overlay"`: an
immutable base manifest plus an ordered list of immutable extents. A v4
manifest has no top-level chunk table; the canonical table is derived from the
base chain, and `content_root` is the Merkle root a canonical single-table v3
manifest would produce over the same bytes at the same chunk size. The reader
accepts chains up to 256 manifests, at most 64 extents per manifest, requires
extents to be chunk-aligned, sorted, non-overlapping within a layer, and exact
in count/length/sum, and fails closed (`Malformed`, `Corrupt`, `LimitExceeded`,
`UnsupportedVersion`) on every violated rule. v1/v2/v3 encodings are
byte-identical (the frozen fixture tests are unchanged).

Read path: the resolver reads manifest blobs only, validates every base
cross-check (`base_content_root`, `base_size`, `chunk_size`, Merkle identity,
depth accounting, `overlay_bytes`), merges layers newest-wins, and checks the
resolved table's root before any content chunk is fetched. Range reads fetch
only intersecting chunks; tests prove this with the per-thread overlay chunk
counter and the backend byte counter. GC marks overlay extents and the base
chain, so bases and chunks reachable only through an overlay are retained and
unreferenced overlay chunks are collectable. Shallow `verify` resolves the
chain; full `verify` decodes every logical chunk and reports the root.

Write path (stage 4): `RepresentationMode::Overlay` and
`--representation overlay` publish one new manifest version per range write —
the previous version as an immutable base plus one extent. Only chunks the edit
touches are read and, when their bytes change, stored. A misaligned edit is
widened to the enclosing chunk grid and the untouched bytes of the widened
chunks come from the previous version, so the logical content is exact.
Adjacent or overlapping extents fold into the current layer (up to 64 extents
per manifest), so chain depth grows per 64 disjoint extents rather than per
write. Before publishing, the writer derives the canonical table twice (from
the previous table plus the edit, and from the base manifest plus the new
extent list) and refuses with `Corrupt` unless they are identical, so the
manifest a reader resolves is exactly the table the writer priced. Bounds:
`Options::max_overlay_depth` (default 64; the hard reader bound is 256) and
`Options::max_overlay_bytes` (default 1 GiB) refuse with `LimitExceeded` naming
`weftfs compact`. Publication uses the unchanged commit protocol, so old
versions and their chunks stay valid for snapshots. `--representation overlay`
selects the Merkle identity when no identity is named; an explicit
`--content-identity whole-file` is refused rather than upgraded.

Whole-file helpers never silently materialise an overlay object: `zero_range`
(both modes) and a growing `truncate` publish an overlay layer (a full zero
chunk is content-addressed once, so memory stays at one chunk); `truncate` to
the same size is a no-op; a shrinking `truncate`, `collapse_range`, and
`insert_range` return the new `UnsupportedRepresentation` class because an
immutable extent can replace or append chunks but cannot shorten or shift the
tail. `append_file_streaming` keeps refusing overlay objects; a range write at
EOF is the supported append.

Compaction (stage 5): `Store::compact(id)` re-reads and re-hashes every logical
chunk in order (one chunk of memory), recomputes the Merkle root, and refuses
to publish unless it equals the pre-compaction root. It then encodes one
canonical `weft.manifest.v3` table, decodes it back, checks chunks/size/root
again, and publishes through the normal commit protocol. No content blob is
rewritten: every chunk is content-addressed and was just verified. A
non-overlay object is refused with `UnsupportedRepresentation`. CLI:
`weftfs compact <path>` (`--json` reports `content_root`, `chunks`,
`overlay_depth`, `overlay_bytes`, `verified_bytes`).

Court (stage 6): `bench/scripts/overlay_court.sh` runs the same seeded workload
through both writers in separate child processes — one 128 MiB object with
64 KiB chunks and 500 4 KiB overwrites at 4 KiB-aligned random offsets — plus a
4 KiB range read (warm and through a freshly opened handle), a full read,
`verify --full`, compaction, GC, and six checks. It runs over the local backend
and, through `scripts/s3_court.sh --overlay`, over single-node MinIO.

| metric (local / MinIO) | chunk COW | overlay |
|---|---|---|
| 500 writes, wall | 917.3 / 2,308.3 ms | **415.5 / 1,702.9 ms** |
| write p50 / p99 (local) | 1.819 / 1.912 ms | 0.717 / 1.698 ms |
| backend bytes written | 87,403,237 / 89,451,237 | **36,731,767 / 38,779,767** |
| backend requests (writes) | 3,501 | 3,993 |
| 4 KiB range read, warm p50 | 0.919 / 1.422 ms | **0.030 / 0.414 ms** |
| 4 KiB range read, cold first | 0.920 / 1.502 ms, 2 req | **5.594 / 7.557 ms, 10 req** |
| full read before compaction | 40.6 / 439.3 ms | 39.9 / 437.8 ms |
| `verify --full` before | 53.5 / 1,773.0 ms, ok | 52.0 / 782.5 ms, ok |
| compaction | refused (`UnsupportedRepresentation`) | 34.0 / 444.1 ms, root unchanged |
| `verify --full` after | 53.2 / 1,759.5 ms, ok | 52.9 / 1,879.8 ms, ok |
| retained blob bytes before / after compact / after GC | 134,322,319 each | 166,480,149 / 134,322,319 / 134,322,319 |
| GC after compaction | 775 blobs, 70,338,845 B | 501 blobs, 2,289,555 B |
| on-disk bytes after GC | 149,348,604 | 166,831,597 (about 32.5 MB is the snapshot-pinned old chain) |
| peak RSS per child | 402,348 / 403,964 KiB | 411,248 / 412,348 KiB |

The content root after the writes is identical in all four runs
(`c00367f0fd99b612a7800b863c66c5c959fcdbe42bafaa36b747d1f8ff164c22`), and so is
the full-read digest. Compaction changes no byte and no root. A snapshot taken
before compaction reads byte-identically after compaction and after GC, so the
old chain is retained exactly as long as the snapshot exists.

Losses, recorded in `bench/RESULTS-overlay.md`: a cold range read is 6.1x
(local) / 5.0x (MinIO) slower because a new process resolves the depth-8 chain
(10 requests / 215,317 B instead of 2 / 170,127 B); extent coalescing widens
extents so `overlay_bytes` reached 30.6 MiB for 2.0 MiB of logical writes (the
accounting metric the byte bound limits, not extra I/O); full reads are not
cheaper; a snapshot pins the old chain, so the overlay store keeps 17.5 MB more
on disk after compaction + GC in this court; compaction is O(file size); the
overlay child peaks 8.9 MB higher; S3 `verify --full` timings are noisy; and
this is not a durability, media, power-loss, cloud, or fleet claim.

Go reads the same v4 objects: `model.ManifestFromWCB` validates the exact key
set and grid, and the store resolves the chain, verifies every chunk/root, adds
overlay retention to GC and quota accounting, and refuses range writes to an
overlay object with `InvalidArgument`. **Go has no v4 writer and no
compaction**; there is no Go write-parity claim. The committed fixture
`conformance/overlay/v1` (also mirrored at `WeftFS-Go/conformance/overlay/v1`)
is generated by `examples/gen_overlay_fixture.rs` and read by both
`tests/overlay_fixture.rs` and `WeftFS-Go/store/overlay_handoff_test.go`.

Evidence (all in the Docker courts):

- `bash scripts/docker-court.sh rust "cd /work && cargo test --release --all-targets --no-fail-fast"`
  -> **336 passed / 0 failed**, including `overlay_format` 6/6,
  `overlay_read` 9/9, `overlay_fixture` 1/1, `overlay_write` 13/13, and
  `overlay_compact` 8/8.
- `bash scripts/docker-court.sh fuse "cd /work && cargo test --release --features fuse --all-targets"`
  -> **378 passed / 0 failed / 2 ignored** (the two pre-existing ignored tests).
- `bash scripts/docker-court.sh go "cd /work/WeftFS-Go && go test -race -count=1 ./store/..."`
  -> **ok**.
- `bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/overlay_court.sh --local"`
  -> **6/6 checks**, receipt `bench/receipts/overlay-20261106T103115Z.json`.
- `bash scripts/s3_court.sh --overlay` -> **6/6 checks**, receipt
  `bench/receipts/overlay-s3-20261106T103120Z.json`, court receipt
  `bench/receipts/s3-court-20261106T103120Z.json`.
- `scripts/check-spec-drift.sh` 42/42; `scripts/check-claims.sh` all entries.

Stage commits: `fa37197` (format + spec mirrors), `eaad6eb` (read path),
`4fc75fa` (Go read + fixture), `ca34643` (Rust write path), `9776c5e`
(compaction + `weftfs compact`), `e9b9bae` (court harness + S3 flag), and the
docs/receipts commit. The writer-bound spec paragraph was exported to WeftCodec
`54cb4f6`, WeftCodec-Go `8957998`, WeftFS-Go `17cef27`.

Residual: write and compaction parity is Rust-only; the truncate-shrink,
insert, and collapse paths refuse rather than rewrite; the writer bounds are
library options, not CLI flags; coalescing can inflate `overlay_bytes`; a
fresh-process read pays chain resolution; the court is one host with a warm
page cache and one synthetic workload; no physical power loss or fleet claim.

## Subphase R10 — blob and commit planes (2026-11-06)

`StorageBackend` used to mix immutable content bytes with POSIX-shaped control
state (`write_at` into the superblock, `sync_dir`, an exclusive lock, in-place
zero-fill). It worked for one local filesystem while blocking the next two
programme items: physical object packs need a non-directory metadata plane, and
distributed backends need a conditional-head publication contract rather than
an in-place 4 KiB write.

R10 splits the contract into `BlobPlane` (immutable bytes: `get`, `get_range`,
`put_if_absent`, `delete`, `head_part` probe, `list`) and `CommitPlane`
(versioned control state: `read_control`, `compare_exchange`,
`compare_exchange_head`, `load_head`, `probe`), keeps `StorageBackend` as the
store's one I/O adapter, and routes each `Key` to a plane through
`CompositeBackend`. `BlobPlane` and `CommitPlane` are implemented for
`LocalBackend`, `ObjectBackend`, and `S3Backend` by delegating to the existing
primitives, so the plane contract and the legacy contract cannot drift.

**No on-disk format change.** The key layout, commit-file bytes, two-slot 8 KiB
superblock, publication order, and every content identity are unchanged, and
all pre-existing tests pass unmodified.

Capabilities are **probed**, not declared. A writable open runs a blob probe
(create-once publication, exact length, a 17-byte ranged read, delete) and a
commit probe (create-once CAS, matching CAS, and a **stale** CAS that must be
rejected with `Busy`). A probe may only refute a declared capability, never
upgrade one; a declared-absent capability is skipped and refused where the
store needs it. Durability stays an attested property of the publication
primitive — no probe can prove media durability without power loss, and none is
claimed. Read-only opens never probe and never write.

The probe result is kept on the handle (`Store::plane_probes`) and is recorded
in the court receipt. A writable open now also refuses a backend without
conditional head publication (`conditional_head`), which is the contract a
distributed metadata engine must satisfy.

### Split configuration (opt-in; the default is unchanged)

```
weftfs --blob-backend s3    --commit-backend local --store DIR ...
weftfs --blob-backend local --commit-backend s3    --blob-root DIR --store DIR ...
weftfs --blob-backend local --commit-backend local --blob-root DIR --store DIR ...
```

Omitted flags mean "same as `--backend`", so the historical layout is the
default and is never silently changed. `BackendKind::split` refuses nesting.
`weftfs info --json` reports `"backend"` and a `"planes"` object with both
planes' capabilities. A split is refused before touching disk when the blob
plane lacks `atomic_publish`/`durable_fsync` (a half-written manifest or chunk
would break exactness) or the commit plane lacks
`atomic_publish`/`conditional_head`/`durable_fsync` (a torn or overwritten head
would make a commit ambiguous). The S3 commit plane is expressible because S3
already has `If-None-Match`/`If-Match`; the court runs it.

### Evidence

* `cargo test --release --all-targets` -> **350 passed / 0 failed** (83 test
  binaries `ok`), including `tests/planes.rs` 14/14.
* `cargo test --release --features fuse --all-targets` -> **392 passed / 0 failed / 2 ignored** (the two pre-existing `#[ignore]`s), 83 binaries `ok`.
* `bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/planes_court.sh"`
  -> 14/14 plane tests, receipt `bench/receipts/planes-20261106T110054Z.json`
  (local layout only; the S3 layouts are recorded as skipped with the reason).
* `bash scripts/s3_court.sh --planes` -> 14/14 plane tests and **4/4 layouts**
  with `exactness.match = true`, receipts
  `bench/receipts/planes-20261106T110154Z.json` and
  `bench/receipts/planes-20261106T110216Z.json` (independent repeat), court
  receipts `bench/receipts/s3-court-20261106T110154Z.json` /
  `...T110216Z.json`.
* `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass (see the
  claim entries; no spec text changed, because no wire format changed).

All four layouts produced the identical content root
`7ebb1bdb425b86b04dfe7aa7018136b52648da0f00a148f9356eabdddb80a470`, generation
`145`, 6,291,456 verified bytes, and digest
`74d54de57fd438a492b7966810ca4a631cb7eb9cc23b7e20da8f4f54d1bf5b59`:
single local, single S3, S3-blobs/local-commits, and local-blobs/S3-commits.

Create / 96-write / full-read medians, run 1 (run 2 in parentheses):
local/local 5.90 (5.84) / 14.06 (14.21) / 3.24 (3.30) ms;
s3/s3 129.90 (131.52) / 286.44 (287.32) / 30.03 (29.99) ms;
s3-blobs/local-commits 98.38 (97.75) / 220.68 (221.37) / 31.31 (36.96) ms;
local-blobs/s3-commits 55.68 (57.30) / 117.49 (119.32) / 3.37 (3.44) ms.
Per-plane request counts and bytes, recoveries, peak RSS, and every loss are in
[`bench/RESULTS-planes.md`](../bench/RESULTS-planes.md); the contract and the
precise list of what an external metadata engine must implement are in
[`docs/PLANES.md`](PLANES.md).

### Crash proxy

`tests/planes.rs` kills the writer at `after_objects`, `after_commit_file`, and
`after_superblock_write` for a local split and for an S3-blobs/local-commits
split. Every iteration reopened to the old whole state (phases 1-2) or the new
whole state (phase 3), never partial; an unpublished newer commit file in the
early phases is the documented state, exactly as in the pre-existing
single-backend proxies. The split publishes the head with
`compare_exchange_head`, and a second live writer is refused with `Busy`
(`split_publication_uses_one_head_compare_and_swap`).

### Residual (unchanged by this subphase)

* Split parity is Rust-only; WeftFS-Go keeps the single backend and there is no
  cross-language split handoff.
* The commit plane is single-writer; there is no distributed coordination or
  fleet claim. The single writer is enforced by `flock` locally and by a fenced
  lease with a conditional compare-and-swap on object stores. An external
  metadata engine is specified but not implemented.
* Process-fault crash proxy only; no physical power loss, host cache loss, or
  media error injection.
* One host, warm caches, synthetic 6 MiB fixture; MinIO is a single node with a
  tmpfs data directory. Absolute times are court-bound.
* `weftfs mount` accepts the split flags but the court exercises the store and
  CLI paths, not a mounted split filesystem.

## Subphase R11 — physical object packs (2026-11-06)

A content-addressed namespace costs one physical request per small blob:
10,000 one-kilobyte chunks were 10,000 `PUT`s and 10,000 `GET`s. The R10 commit
plane makes a real pack catalog possible.

`src/packs.rs` adds `PackedBlobPlane`, a `BlobPlane` over an inner blob plane and
a `CommitPlane`-backed catalog:

* members at or below `member_threshold_bytes` are buffered and sealed into a
  content-addressed pack (`packs/<ab>/<hex>`) when `pack_target_bytes` or
  `max_members_per_pack` is reached; larger blobs stay flat;
* membership is `key -> (pack, offset, length, digest)` in
  `catalog_segments` immutable catalog blobs (`pack-catalog/<ab>/<hex>`), named
  by one mutable `pack-catalog.root` control record replaced with a
  compare-and-swap, so a reader sees the old or the new whole catalog;
* the pack is published **before** the catalog names it: a crash between them
  leaves an unreferenced pack (reclaimed by GC), never a dangling entry;
* a read is one catalog lookup then one `get_range` for exactly the member's
  bytes; whole-member reads verify the catalog digest and fail closed
  (`Corrupt`) on a mismatch, a short read, or a truncated pack;
* `Store::publish_commit` calls the new `flush_buffered_blobs` before writing the
  commit record, so a commit never names a non-durable blob;
* `gc --apply` deletes candidates in one batch (`delete_many`) and then reclaims
  packs no catalog entry names; a pack with one live member survives;
* when the catalog bound (`max_catalog_bytes`) is reached, packing stops and new
  blobs are published flat;
* a plane without `true_range_reads` is refused at construction
  (`UnsupportedVersion`).

New key namespaces are `packs/`, `pack-catalog/`, and `pack-catalog.root`; the
existing logical keys, manifest bytes, commit bytes, and superblock are
unchanged. `--pack`, `--pack-target-bytes`, `--pack-member-threshold`,
`--pack-catalog-bytes`, and `--pack-segments` select the policy.

### Bounds chosen

Pack target 16 MiB, member threshold 64 KiB, 65,536 members per pack, 8 MiB
catalog bound, 256 catalog segments (each catalog entry is 113 bytes; the bound
admits 74,288 entries). `PackOptions::validate` refuses a threshold above the
pack target, a non-power-of-two segment count, and a catalog smaller than one
entry per segment.

### Evidence

* `cargo test --all-targets` -> **369 passed / 0 failed**; the new
  `tests/packs.rs` is 13/13: byte-identical whole and ranged reads, a narrow
  range asserted with the backend byte counter, digest mismatch, truncated
  pack, missing catalog and missing segment, the two-phase pack crash proxy,
  GC reclamation (one live member keeps the pack; all-unreferenced collects it),
  the catalog-bound flat fallback, deterministic layout, and a store-level
  round trip/verify/GC on a packed plane.
* `cargo test --features fuse --all-targets` -> **411 passed / 0 failed / 2
  ignored** (the two pre-existing ignores).
* `bash scripts/s3_court.sh --packs` -> four configurations, exactness
  `match = true`: same generation `2`, snapshot root
  `57c24012a9483db06f5c6fb3ca9b8860430492276e48e64cdbb21df034c37975`, read
  digest `a8812b26c57dc49c2371b6d52fb2b6063e21e84023eef360fb9b772695871b99`,
  zero verify problems. Receipt `bench/receipts/packs-20261106T113605Z.json`,
  MinIO provenance `bench/receipts/s3-court-20261106T113605Z.json`, log and test
  summary beside them. Revision `8b42103`.
* `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass (no spec text
  changed; no wire format changed).

### Measured result (see `bench/RESULTS-packs.md`)

10,000 x 1 KiB files in one group commit: object requests **20,003 -> 262**
(local) and **20,003 -> 261** (S3), wall **298.3 -> 185.4 ms** (local) and
**7,321.5 -> 2,791.7 ms** (S3). Losses: written bytes **+13.8%**
(18,809,103 vs 16,524,362/16,528,458), physical footprint **+19.2%**
(14,112,521 vs 11,840,000), cold 1,000-read pass **1.25x requests / 3.19x
bytes**, S3 warm read p50 **0.3437 vs 0.1685-0.1729 ms**, S3 `verify --full`
**23,103.2 vs 16,202.3 ms (+42.6%)**, peak RSS **+6,036 KiB (local) / +2,228
KiB (S3)**. A 4 KiB range read inside a 16,777,216-byte pack costs exactly **1
request and 4,096 bytes**.

### Residual

* Rust only: WeftFS-Go has no pack plane and cannot read a packed store.
* Packs are immutable and never rewritten; dead members occupy pack space until
  the whole pack is unreferenced (no pack compaction).
* One catalogue root per store, 256 segments; reclamation scans the catalog.
* Process-fault crash proxy only; no physical power loss or media errors.
* Single host, synthetic fixture, MinIO single node with a tmpfs data
  directory; absolute times are court-bound.
* A range read cannot verify the member digest (it is a partial read); only
  whole-object reads are digest-checked, matching the flat plane.

## Subphase R12 — rebase parity and host calibration (2026-11-06)

`weftfs rebase` existed in Rust with provenance receipts and in WeftFS-Go only as
a store API. At the same time the planner's `measured` decode table was a
constant baked into the crate: it was calibrated in this bench court, so a host
with different decode throughput had no supported way to use its own numbers.
R12 closes both gaps. No wire format changed; manifests, commits, vectors, and
the superblock are unchanged.

### Host-local calibration

The measurement code that lived in `examples/calibrate_decode.rs` now lives in
`src/calibration.rs`; the example is a thin wrapper over the library, so the CLI
verb and the bench harness cannot drift. A calibration document uses the
schema `weft.calibration.v1` and carries the created time, the host/kernel/image
identity, the measurement method, exactly ten class rows (`fixed_ns`,
`ns_per_mib`, `samples`, `origin`), and a digest
`H("weft/decode-calibration/v1", compact JSON without the digest field)`.

The loader is strict and fails before the planner sees a row:

* wrong schema, unknown or missing keys, a non-object, or a file above 1 MiB;
* missing, duplicated, or unknown cost classes (completeness is ten rows);
* an unknown `origin`, or a `measured` row with fewer than 5 samples;
* `fixed_ns > 60 s`, `ns_per_mib == 0`, or `ns_per_mib > 100 s/MiB`;
* any row that is not strictly monotonic between 1 MiB and 64 MiB;
* a digest mismatch.

There is no partial parse, no silent fallback to the baked table, and no merge of host
and baked rows. `weftfs calibrate --out FILE` measures, writes the record, and
prints the per-class divergence against the baked `measured` table;
`weftfs calibrate --from FILE --json` reports divergence for an existing file;
`weftfs calibrate --from FILE --install --store DIR` copies it to the advisory
`<store>/calibrations/<id>.json` (ignored by replay, verify, and GC, like
`indexes/`). `--planner-cost-model measured --calibration FILE` uses the file's
rows for the tensor planner and `rebase`; with `declared` it is refused with the
exact error `--calibration requires --planner-cost-model measured`. Without the
flag, behaviour is byte-identical to R11.

In the final scale court the host table differed from the baked table by
+1.962% (float-tensor) to +8.720% (semantic-delta), and -2.541% for
tensor-sharded, measured at 1 MiB. The table is host-bound by construction; the
record names the exact image, kernel, CPU count, and WeftFS revision.

`tests/calibration.rs` is 8/8: round trip through a file, digest over the
canonical document (whitespace-insensitive), 18 malformed/incomplete/out-of-
range rejection cases, oversized-file refusal, planner equality with the file's
rows, default behaviour unchanged, rebase chain pricing with the file, and an
advisory install that replay/verify ignore. Unit tests in `src/cost_model.rs`
pin the baked tables.

### Go rebase CLI parity

`weftfs rebase` now exists in WeftFS-Go with the same flags and decision rule as
Rust: `--root`, `--threshold`, `--dry-run`, `--strategy`, `--lambda-ns`, and
`--max-objects`. It opens read-only for a dry run, writes a
`weft.provenance.v1` receipt on apply, and emits the same JSON field names. A
dependency strategy (`weight-delta`, `sparse-delta`, `semantic-delta`) is
refused with an explicit message in both languages; `--strategy` is for the
standalone target.

Cross-language tests run in both directions:

* Go applies a rebase; Rust `verify --full` reports ok, `provenance show`
  returns the Go receipt, and the Go read equals the original bytes with the
  same logical root.
* Rust applies a rebase; Go `Verify(true)` reports ok, `ReadTensor` equals the
  original bytes, the logical root is unchanged, the `before-rebase` snapshot is
  byte-identical, and the provenance relation is present in Go state.

`cmd/weftfs/rebase_test.go` also proves a dry run writes nothing (generation,
head, version, and counters unchanged), a high threshold selects nothing, the
second apply after a reopen is a no-op, and an unsupported strategy is refused.
`TestRustRebaseReadableByGo` is the Rust-to-Go direction.

The new reopen test exposed a real bug in both implementations: `flush`
early-returned when the pending map was empty, so a **pure consumption** (the
rebase counter reset, which writes nothing else) was never persisted. A reopened
store then resurrected the count and a second rebase re-selected an object that
was already rebased. Both languages now keep a zero-delta pending entry so the
consumption publishes a compacted snapshot; `src/access.rs` and
`WeftFS-Go/store/access_consume_test.go` pin the behaviour across a reopen.

### Scale court (`bench/scripts/rebase_scale_court.sh`)

The court runs the cached Qwen2.5-0.5B / Instruct pair in the bench court: both
files are re-hashed against the LFS SHA-256, a release calibration is measured
and used for every planner call, the family is ingested (241 instruct tensors
are weight deltas), the pre-rebase state is snapshotted, 16 tensors are read 32
times and a different 16 are read twice, and per-tensor reads plus a full
checkpoint restore are timed before and after. A second dry run at threshold=1
provides the counterfactual economics for every read tensor without writing.

With a 200 ms threshold, the policy selected 16 of 32 read tensors, exactly the
predicted set (0 mismatches, 291 objects scanned). Selected per-tensor decode
went from `4501.537 ms -> 1900.812 ms` (-57.8%) and the full instruct restore
from 10,938.771 ms to 8,430.865 ms (-22.9%). The warm 16 (two reads, ~174 ms of
decode each) stayed deltas. Representation bytes for the 16 selected tensors
grew by 43575360 bytes (~19.4%) and the current-state representation total by
43575360 bytes; the store tree grew 277,231,717 bytes because the snapshot and
commit history retain the old delta frames and the advisory indexes grew
9,555,328 bytes. All 13 hard checks pass: `verify --full`, every BLAKE3 hash,
every recomputed logical root, the unchanged `before-rebase` snapshot, the
second apply writing nothing, the provenance read-back, and exact source
reassembly (`source_reassembly_exact`, SHA-256
`fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe`).

The economics are reported, not flattened: the measured chain cost is ~87.2
ms/read for an 8.7 MB BF16 MLP tensor, so the policy break-even is 3 reads; the
embedding costs ~2.724 s/read, so its break-even is 1. The bytes/ns trade would
have paid back after one read for the two-read tensors, but the fixed 200 ms
policy threshold is the binding constraint. That is recorded as
`policy_more_conservative_than_storage`, alongside the physical growth per
selected tensor. Receipt
`bench/receipts/rebase-scale-20261106T130054Z.json`; every table, method, and
loss is in `bench/RESULTS-rebase-scale.md`.

### Evidence (this subphase; same court limits)

* `cargo test --all-targets` (rust court): **380 passed / 0 failed / 0 ignored**.
* `cargo test --features fuse --all-targets` (fuse court): **422 passed / 0
  failed / 2 ignored** (the two pre-existing ignores).
* `go test -race -count=1 ./store/... ./cmd/weftfs/...`: **125 passed / 0 failed / 7 skipped**;
  the skips are the two pre-existing child-process guards and the five FUSE-mount
  handoff tests that need a `--features fuse` Rust oracle and `/dev/fuse`, which
  this court does not have. `go vet ./...` is clean and `CGO_ENABLED=0 go build
  ./...` succeeds.
* `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass.

### Residual and non-claims

* The calibration is a host artifact. It is not portable, not distributed by the
  store, and not signed; an operator copies it. Go's planner and rebase CLI still
  use the baked table (there is no Go `--calibration` in R12).
* The scale court is one host, one process at a time, warm page cache, cached
  public weights, and a synthetic calibration workload. Absolute times are
  court-bound. No physical power loss, host cache loss, fleet, or concurrent
  reader claim.
* A rebase is a new version. The before-rebase snapshot and the commit history
  retain the old frames, so the on-disk tree grows more than the current-state
  representation and no GC was run in the court.
* Only 32 of 290 instruct tensors were read in the scale court; the 41 unread
  rebaseable deltas are untouched. The court does not claim the full family was
  rebased.
* Go rebase parity is tested at the store and CLI levels; the scale court itself
  is Rust-only.

## Subphase R13 — distributed correctness court (2026-11-06)

The S3 backend, the blob/commit plane split, conditional head publication with
a probed stale-CAS refusal, GC, capabilities, and snapshots each had a court,
but none of them drove **separate writer/reader/GC processes** through the
distributed contract under failure. R13 adds that court before any further
performance claim. No wire format, manifest, commit, superblock, or vector
changed.

### The court

`bench/scripts/distributed_court.sh` runs in the bench court. It starts the
verified MinIO `RELEASE.2025-04-22T22-12-26Z` binary
(sha256 `53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`)
itself on loopback so it can stop and restart the server process. The harness
(`examples/distributed_court.rs`) drives real processes and records, for every
scenario, the exact command line, PID, exit code or signal, observed
generations, verification result, and raw log path:

* **S1** single-writer interlock: 20 alternating pairs; every second open is
  refused `Busy`; a third process opens after the first exits and the store
  verifies (64/64 checks).
* **S2** atomic publication under a process fault at `after_objects`,
  `after_commit_file`, and `after_superblock_write` (15 runs): 10 old, 5 new,
  **0 mixed/partial**; the 5 pre-head tails are reported by `verify`, never
  hidden (34/34).
* **S3** at-least-once retry: after `after_objects` the retried commit is
  **byte-identical** to the uninterrupted head (`32a0ba3d…9431`); after
  `after_commit_file` the visible state is identical and one entry per path
  exists, with lineage `[3, 1]` and the abandoned generation never reused;
  after publication the retry is refused `Exists` and writes nothing (21/21).
* **S4** 8 reader processes keep the exact pinned bytes across 10 commits
  (64/64 reads at pinned generation 3, published 4–13) (19/19).
* **S5** an open reader keeps its generation and bytes after the writer
  publishes; only a reopen observes generation 3 (4 stale + 4 fresh reads)
  (4/4).
* **S6** an endpoint with no listener fails at open with `Io`; stopping the
  live MinIO process makes a read fail with `Io` and emit **0** bytes;
  restarting on the same data verifies and serves the same pinned bytes
  (2 starts, 1 stop) (9/9).
* **S7** `gc --apply` racing a reader pinned to a snapshot-only object:
  48 blobs / 16,435 bytes reclaimed; the snapshot-only object, the live file,
  the snapshot state, the checkpoint, and the provenance receipt all still
  read exactly; `verify --full` is clean (8/8).
* **S8** knowing a `ContentId` is not authority under a signed capability
  envelope: the denial is `NotFound: no such object`, byte-identical to an
  absent hash; a token for another object cannot cover the content root; the
  CLI denies `/secret` with the same text and no bytes (10/10).
* **S9** S2/S4/S5 repeated for `--blob-backend s3 --commit-backend local`
  (60/60).

The passing receipt is
`bench/receipts/distributed-20261106T134954Z.json`: **9/9 scenarios, 229 checks
passed, 0 failed, 120 processes recorded**, 42 isolated buckets, 5,968 ms of
harness wall time, 7,460 KiB peak RSS. The first run,
`bench/receipts/distributed-20261106T133534Z.json`, is retained as the failing
receipt (206 passed / 23 failed) that found the defects below. Analysis and
every limit: `bench/RESULTS-distributed.md`.

`tests/distributed_semantics.rs` is the always-on half: **12 passed / 0
failed**. It pins the interlock, stale reader (local and local split),
8-thread concurrent readers, the crash matrix (local and split), the three
retry cases, the unreachable endpoint, the GC race, and content-hash
non-disclosure without needing MinIO.

### Defects found and fixed

1. Opening a store whose `FORMAT` read failed reported every error as
   `Corrupt`, including a connection refusal. `Store::open_impl` now preserves
   `Class::Io` with the same context text.
2. A transport failure during a read was reported as
   `Corrupt: missing blob …: Io: …`. The recovery blob/superblock/commit
   wrappers and the three chunk-read wrappers now preserve `Class::Io`; a
   genuinely missing or mismatched record is still `Corrupt`. This is what
   makes S6's "fails closed with a transport error" literally true.

### Verification and gates

* `cargo test --all-targets` (rust court): **392 passed / 0 failed**.
* `cargo test --features fuse --all-targets` (fuse court): **434 passed / 0
  failed / 2 ignored** (the two pre-existing ignores).
* `scripts/s3_court.sh` (single-node MinIO unit court): **6/6**.
* `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass.

### Residual and non-claims

* Not a cloud, multi-node, fleet, device-cache, media, or production claim:
  one host, one MinIO process on loopback, a tmpfs data directory.
* No TCP-level partition: the only network faults are an endpoint with no
  listener and stopping/restarting the server process. There is no packet
  filter, delay, duplication, or split-brain case.
* No physical media or power-loss claim; S3 durability means the server
  acknowledged the write.
* Writers are exclusive by design (one at a time); no multi-writer protocol is
  claimed or tested.
* The `after_commit_file` retry produces a new generation (3 instead of the
  reference 2) because an unconfirmed commit file is never reused. Its visible
  state is identical and there is no second visible state.
* A crashed S3 writer's 30 s lease is cleared explicitly by the court before
  each retry; production waits it out and the lease is never disabled. The
  receipt records this.
* The court is Rust-only; the split layout's commit plane is local in S9.

## Subphase R14 — GCS backend (2026-11-06)

`src/planes.rs` splits durable storage into an immutable **blob plane** and a
versioned **commit plane**; `src/s3.rs` implements both for S3-compatible
services with SigV4, `If-None-Match: *` / `If-Match`, and a probed stale-CAS
refusal. GCS is the next backend target, but its concurrency model is
**generation preconditions** (`ifGenerationMatch`), not ETags. R14 adds a GCS
JSON-API backend against the same two planes. No manifest, commit record,
superblock slot, logical key name, or conformance vector changed.

### What is implemented

* `src/gcs.rs` — `GcsConfig` and `GcsBackend`.
  * `BlobPlane`: `get` (`alt=media`), `get_range` (`Range` header, `206`),
    `put_if_absent` (`ifGenerationMatch=0`), `len`/`exists` (object metadata),
    `delete`, `list` (object collection with `prefix`/`pageToken`), `head_part`/
    `probe` (the shared plane probes), `clean_stale`, `init_layout`.
  * `CommitPlane`: `read_control`, `compare_exchange` (read the bytes and the
    observed generation, refuse unless the caller's content token matches, then
    upload with `ifGenerationMatch=<generation>`), `write_control` (generation-
    conditioned retry), `create_control`, `put_control_if_absent`,
    `delete_control`, `control_len`, `control_exists`, `list_control`,
    `sync_dir` (provider acknowledgement), `acquire_lock` (a `LOCK` object with
    an owner and lease; an expired lease is reclaimed with
    `ifGenerationMatch=<observed>`), and `probe`.
  * HTTP `412 Precondition Failed` maps to the same `Busy` class the S3 backend
    reports for a lost conditional write. `401`/`403` are `Unauthorized`; every
    transport failure is `Io` and is never reported as `Corruption`; `413` is
    `LimitExceeded`.
  * Uploads: payloads up to 5 MiB use the JSON API simple media upload; larger
    payloads use the documented two-step resumable upload (session initiation
    carries the precondition, the bytes go to the returned session URI in one
    bounded request, and `308` is a hard error — a partial upload is never
    published).
  * A generation is read *and re-checked after the bytes*, so a concurrent
    replacement can never be mistaken for the observed state. That read is
    bounded to four attempts and then fails `Busy`.
* `StorageBackend for GcsBackend` delegates to those same plane operations, so
  one implementation of each physical operation carries the CLI, the store,
  FUSE, GC, verify, and recovery. `BackendKind::Gcs { config }` participates in
  split layouts (`--blob-backend gcs --commit-backend local` and the reverse).
* CLI: `--backend gcs` (and `--blob-backend`/`--commit-backend gcs`) with
  `--gcs-endpoint`, `--gcs-bucket`, `--gcs-token`, `--gcs-project`,
  `--gcs-anonymous`; environment `WEFT_GCS_ENDPOINT`, `WEFT_GCS_BUCKET`,
  `WEFT_GCS_ACCESS_TOKEN` / `GOOGLE_OAUTH_ACCESS_TOKEN`, `WEFT_GCS_PROJECT` /
  `GOOGLE_CLOUD_PROJECT`, `WEFT_GCS_ANONYMOUS`, `WEFT_GCS_LOCK_LEASE_SECS`.
* Authentication is an OAuth2 **bearer token** supplied by flags or environment,
  or anonymous mode for an emulator. Service-account JSON key signing needs
  RS256 and is **not** implemented: setting `GOOGLE_APPLICATION_CREDENTIALS`
  alone is refused with a message saying so. No RSA dependency was added.
  `GcsConfig`'s `Debug` redacts the token, and no token reaches diagnostics,
  error text, receipts, or the court's environment list.
* The logical listing helpers (`objects/`, `manifests/`, `indexes/`, `refs/`,
  `tmp/`, `packs/`) moved from `src/s3.rs` to `src/backend.rs` verbatim and are
  now shared, so the S3 and GCS listing paths cannot drift from the normative
  layout.

### Generation preconditions and the stale-must-fail probe

The probe runs on every writable open (`init_layout`/`ensure_layout`, before any
commit is published) and again as `CommitPlane::probe`, both against a private
`tmp/` key:

1. publish with `ifGenerationMatch=0` — must succeed (create-once);
2. publish a second time with `ifGenerationMatch=0` — must be refused `412`,
   otherwise the store is refused `UnsupportedVersion` because create-once
   cannot be guaranteed;
3. read the bytes and the observed generation;
4. publish with `ifGenerationMatch=<observed>` — must succeed;
5. publish **again** with the same, now stale, generation — must be refused
   `412`. An endpoint that accepts it is refused `UnsupportedVersion`, because
   head publication could then lose a race silently.

A probe may only refute a declared capability, never upgrade one. Durability is
not probed: `durable_fsync` means the provider acknowledged the write, exactly
as for the S3 backend, and no media or power-loss claim is made.

### Court

`bench/scripts/gcs_court.sh` downloads the pinned
`fake-gcs-server` `v1.56.1` `linux-amd64` release tarball (sha256
`0f2f9f6884417acd5cc27126a9aa237ccc627372a4aa5008b399641de6db9469`), verifies
it, and runs the binary **inside** the rust court on loopback with the memory
backend. If the binary cannot be obtained the script writes an
`emulator_unavailable` receipt naming the exact command and error; it never
substitutes another emulator.

Passing receipt `bench/receipts/gcs-20261106T150444Z.json` (harness, at
WeftFS `ca7004c`) and `bench/receipts/gcs-court-20261106T150444Z.json` (court):
12 files x 32 KiB, 8 KiB chunks, eight 4 KiB range writes, deterministic ids
and fixed clock.

* local and GCS produce the **same content root**
  (`0a8198f56ed59a11…`), the same **generation 21**, the same **commit sequence
  1…21**, the same 393,216 verified bytes, and the same content digest
  (`2e3b9902…`).
* GCS `verify --full` is clean; a read-only reopen recovers generation 21 and
  the same root with a clean full verify.
* The `superblock` control object's GCS generation was sampled three times and
  is strictly increasing (`1793977493537315`, `…551975`, `…563518`): every head
  publication replaced it under `ifGenerationMatch`.
* The plane probe recorded `stale_compare_exchange_rejected: true` against the
  real emulator, not only the in-process fake.
* Crash proxy (CLI aborted by signal at each phase): `after_objects` → old
  state, clean verify; `after_commit_file` → old state plus the documented
  unpublished commit tail (the only problem accepted, and labelled); 
  `after_superblock_write` → new state, clean verify. 3/3 old-or-new, **0
  partial**.
* 14/14 in-process fake-GCS unit tests in the same run.

### Defects found and fixed by running the emulator

1. `buckets.insert` was sent to `/storage/v1/b/<bucket>` with an empty body.
   GCS takes the bucket resource in the body of `POST /storage/v1/b?project=…`;
   the emulator returned `400 EOF`. The backend now sends `{"name": …}` to the
   collection URL. Retained failing receipt `gcs-court-20261106T145714Z.json`.
2. Object listing used `/storage/v1/b/<bucket>?prefix=…` (bucket metadata)
   instead of `/storage/v1/b/<bucket>/o?prefix=…`. The emulator's listing was
   therefore empty and `verify --full` reported a broken parent chain. Fixed.
   Retained failing receipt `gcs-court-20261106T145838Z.json` (a first run that
   also over-asserted a clean verify for the documented unpublished tail).

### Tests and gates

* `cargo test --test gcs_backend -- --test-threads=1`: **14 passed / 0 failed**.
* `cargo test --all-targets` (rust court, WeftFS `ca7004c`): **416 passed / 0
  failed / 0 ignored**.
* `cargo test --features fuse --all-targets` (fuse court): **458 passed / 0
  failed / 2 ignored** (the two pre-existing ignores).
* `bench/scripts/gcs_court.sh`: unit tests 14/14 and the court above.
* `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh` pass.

### Residual and non-claims

* The emulator is `fake-gcs-server` with a **memory** backend on loopback. It is
  not Google Cloud Storage: no IAM, no bucket policies, no requester-pays, no
  object composition, no signed URLs, no multi-region behaviour, no media
  durability, and no physical power loss. Nothing here is a cloud, fleet,
  multi-node, or production claim.
* Only bearer-token authentication is implemented. No OAuth2 refresh flow, no
  service-account JWT (RS256), no workload identity, no `GOOGLE_APPLICATION_CREDENTIALS`.
  Anonymous mode is for emulators and is refused together with a token.
* Writers are exclusive by design (one `LOCK` object with a lease); no
  multi-writer protocol is claimed.
* The generation-precondition read is bounded to four attempts; a store under
  continuous concurrent replacement fails `Busy` instead of guessing.
* `modified()` returns `None` (the JSON API reports a timestamp string and this
  crate has no date parser); the store does not depend on it.
* No batch/JSON-multipart upload path, no retry on `429`/`5xx`, no client-side
  rate limiting. A `429` is an `Io` error.
* Rust only. WeftFS-Go has no GCS backend, so there is no cross-language GCS
  parity claim; the on-disk format is unchanged by this subphase.
* The in-process fake service returns one listing page and does not paginate;
  the real emulator court exercises pagination-free listings of 21 commits plus
  objects. Pagination is implemented in the backend but only exercised for the
  single-page case.
* The crash proxy is a process-fault proxy (the CLI is aborted by signal), not
  a physical power-loss test; each phase waits out the 2 s lock lease before
  reopening, which the receipt records.

## Subphase R15 — Orbax/TensorStore adapter (2026-11-06)

`python/weftcodec/orbax.py` adds an Orbax-compatible `AsyncCheckpointHandler`
over the same canonical `weft.checkpoint.v1` checkpoint and Rust CLI path as the
DCP adapter. The importable module is `weftcodec.orbax`. It flattens a JAX
PyTree with
`jax.tree_util.tree_flatten_with_path`, writes the leaves into one safetensors
staging container, ingests it with `weftfs checkpoint write`, and records a
canonical WCB `weft.orbax.v1` record binding each leaf's path, path-key types,
dtype, shape, total element bytes, content root, and logical root. The store
record lives at `/checkpoints/<name>/orbax.metadata`; the Orbax directory
carries a small canonical `weft.orbax.wcb` locator naming the store, checkpoint,
storage generation, and the metadata record's BLAKE3 root. No new wire format
was introduced: both records are ordinary canonical WCB application records and
the checkpoint is unchanged.

Restore flattens the target PyTree, requires exact path/dtype/shape agreement,
re-reads the Rust manifest, restores the requested entries through
`weftfs checkpoint restore`, re-verifies every leaf's content root, and rejects
missing, extra, or mismatched leaves (`MissingLeaf`, `UnexpectedLeaf`,
`MismatchedLeaf`). `partial_restore=True`, a direct `restore_leaf(path)`, and a
requested JAX target sharding are supported; a Rust-side
`restore_topology` proof (`--topology N`) is independent of the JAX placement.
The asynchronous path returns immediately and starts the save on Orbax's
background commit thread only after Orbax has created its temporary directory;
`staging_bytes` bounds the staging container and the restored bytes. Real
`orbax.checkpoint.Checkpointer`, `AsyncCheckpointer`, and `CheckpointManager`
all drive the handler in tests.

### Installation evidence

JAX and Orbax were installed **inside the python court** with `pip --target`
into `/work/.docker-cache/py-orbax`; the image was not modified. The install
succeeded from PyPI on 2026-11-06: `jax 0.12.0.1`, `jaxlib 0.12.0.1`,
`orbax-checkpoint 0.12.9`, `tensorstore 0.1.86`, `numpy 2.5.4`. The exact install
command, status, and tail are in
`bench/receipts/orbax-20261106T152612Z.install.log` and the receipt's `install`
object. The adapter tests in that court were **18 passed**
(`orbax-20261106T152612Z.tests.log`). The measured harness checked **13/13**
conditions and wrote `bench/receipts/orbax-20261106T152612Z.json` with
`status: ok`. The Python court receipt
`bench/receipts/python-bindings-20261106T153054Z.json` records **349 tests / 9
skipped** (340 passed, 0 failures, 0 errors) and the four optional dependency
versions.

### Workload and measured comparison

One deterministic 8-way sharded PyTree on a forced 8-device CPU mesh
(`XLA_FLAGS=--xla_force_host_platform_device_count=8`): 11 leaves,
58,765,352 logical bytes, dtypes bf16/f16/f32/i8/u8/i32, random values from a
fixed PRNG seed. Orbax's own side is
`PyTreeCheckpointHandler(use_ocdbt=True, use_compression=True)` under real
`AsyncCheckpointer`; the WeftFS side is `WeftOrbaxHandler` under the same
orchestrator.

| metric | Orbax OCDBT | WeftFS Orbax adapter | delta |
|---|---:|---:|---:|
| save pause (ms) | 10.273 | 1.017 | -9.256 (deferred work) |
| total save (ms) | 79.631 | 1,770.713 | +1,691.082 (**22.2x**) |
| full restore (ms) | 21.243 | 2,690.338 | +2,669.095 (**126.7x**) |
| single-leaf restore (ms) | 5.248 | 380.432 | +375.184 (**72.5x**) |
| checkpoint physical bytes | 50,644,895 | 50,866,816 | +221,921 (**+0.44 %**) |
| physical / logical | 0.861816 | 0.865592 | +0.003776 |
| physical files | 13 | 189 | +176 |
| exact leaves | 11/11 | 11/11 | 0 |
| peak process RSS (KiB) | — | 1,693,668 | includes JAX + both stores |

The physical comparison is the store immediately after publish, excluding the
restored-container imports. The CLI's `checkpoint restore` imports each
restored container under `/dcp-reads/<uuid>`; repeated restores grew the store
by **67,615,378 B** in this run (50,866,816 -> 118,482,194 B). That overhead is
reported separately and is not part of the checkpoint-byte comparison.

### Exactness, topology, and Rust verification

All **11/11 leaves exact** on both sides: dtype, shape, and SHA-256 of the exact
bytes. The Rust manifest, the canonical metadata record, and every leaf content
root and logical root agree. `checkpoint verify` verified 11 tensors;
`verify --full` was **ok** immediately after save (generation 3) and **ok**
after all restores (generation 14, 95 objects, 3,719 chunks, 95 manifests).

A requested 2-device `NamedSharding` restore target is honoured by the WeftFS
adapter (`requested_sharding_honored: true`) with 11/11 exact leaves. Orbax
0.12.9 restored with the saved 8-device sharding and emitted its own warning
(`requested_sharding_honored: false`) but was still 11/11 exact; that is an
Orbax limitation on this path, not a WeftFS loss. The Rust-side topology proofs
`--topology 8` and `--topology 2` were both exact (242.785 ms and 253.270 ms).

### Every measured loss

* Total save **22.2x slower** (1,770.7 vs 79.6 ms); the Rust
  `checkpoint write` command alone was 1,701.4 ms because the default `zstd`
  shard strategy is slow on incompressible random weights.
* Full restore **126.7x slower** (2,690.3 vs 21.2 ms); the CLI process reopens
  the store, decodes shards, and imports a restored container.
* Single-leaf restore **72.5x slower** (380.4 vs 5.2 ms) for the same reasons.
* Physical bytes **+0.44 %** (+221,921 B) and 189 files vs 13; the store also
  carries manifests, commits, indexes, and the metadata object.
* Restored-container imports add **67,615,378 B** (`/dcp-reads/...`) across the
  restores in this run.
* The adapter writes an extra full-size safetensors staging container before
  the Rust CLI ingests it, then removes it.
* The smaller **save pause** is a scheduling effect (work deferred to Orbax's
  background commit thread), not a throughput win; total save is 22.2x slower.
* Peak process RSS 1,693,668 KiB (~1.62 GiB) includes JAX, Orbax, TensorStore,
  the source tree, both restored trees, and the store; it is not an isolated
  adapter number.

### Non-claims and gates

**CPU only** (eight forced CPU devices in one process); **no TPU and no GPU**.
**Single process**, single host; **no multi-host** and no true distributed JAX.
The Orbax checkpoint is OCDBT/TensorStore and the WeftFS checkpoint is a
separate on-disk format; this is not file-level interoperability. Unexercised
Orbax features include Zarr3, per-leaf `save_args`/`restore_args`, transforms,
custom metadata, step-metadata contents, retention/deletion internals,
emergency checkpointing, and multi-axis meshes. Supported leaf dtypes are only
the exact WEFT-TENSOR v1 set; bool, float8, complex, and unsigned 16/32/64-bit
arrays are rejected, never coerced. There is no process-crash, power-loss, or
media-failure court for this adapter, no Go-side Orbax adapter, and no fleet or
production claim. The workload is synthetic random weights, one run, warm
caches; the results in `bench/RESULTS-orbax.md` are the measured record and
include every loss above.

Gates: `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh`
pass, and the Python court receipt is
`bench/receipts/python-bindings-20261106T153054Z.json`.

## Subphase R16 — independent S3 implementation (Ceph RGW) (2026-11-06)

MinIO (Q4) and `fake-gcs-server` (R14) are single implementations of their
protocols, so neither can answer whether "S3-compatible" means the wire
behaviour the store depends on. This subphase adds a **Ceph RADOS Gateway**
court: a genuinely independent S3 implementation (Ceph shares no code with
MinIO), plus a reusable compatibility matrix that can be pointed at any
S3-compatible endpoint.

### What is implemented

* `scripts/ceph_court.sh` — starts `quay.io/ceph/demo` (Ceph 19.2.0 squid:
  mon + mgr + one bluestore OSD + rgw/beast, pinned by image digest) on a
  dedicated bridge, waits for a real HTTP answer from the frontend, runs the
  existing S3 court logic and the matrix against it, optionally runs the same
  matrix against MinIO, and writes a court receipt plus a side-by-side
  comparison.
* `examples/s3_compat_matrix.rs` — the eight behaviours that commonly differ
  between "S3-compatible" servers, measured rather than assumed. Harness check
  names: `if-none-match-create-once`, `if-match-conditional-overwrite`,
  `range-open-ended`, `range-out-of-range`, `head-missing-key`,
  `delete-idempotent`, `list-pagination`, `stale-cas-probe`
  (`If-None-Match: *` create-once, `If-Match` conditional overwrite,
  open-ended range read, out-of-range `416`, `HEAD` on a missing key, delete
  idempotence, `ListObjectsV2` pagination with `max-keys` +
  `continuation-token`, and a stale compare-and-swap probe on an
  8192-byte superblock-shaped object). `--compare a.json b.json` prints and
  writes the two-implementation matrix.
* `scripts/s3_court.sh --matrix` — the same matrix against MinIO, sharing the
  run stamp so the two receipts compare cleanly.
* `S3Backend` now **measures** the `If-Match` comparison form at writable open
  (`IfMatchForm::{AsReturned, Dequoted}`) instead of assuming the AWS/MinIO
  rule. The chosen form is reported in diagnostics as
  `S3 If-Match form measured at open: ...`. It is accepted only if a matching
  value succeeds, a stale value in the *same* form is refused `412`, and the
  winner's bytes survive; otherwise the endpoint is refused
  (`UnsupportedVersion`) exactly as before. No capability is inferred and no
  check is skipped. The writer lock negotiates the form itself, because the
  store locks before it probes the planes.

### Evidence

Ceph RGW 19.2.0, `ceph version 19.2.0 (16063ff2022298c9300e49a547a16ffda59baf13)
squid (stable)`, image
`quay.io/ceph/demo@sha256:522483cf07cfce6386b8e18a3edfa88a1b32c688dee231d0a6962b1513557723`:

* court: `bench/receipts/ceph-rgw-20261106T160906Z.json`
  (+ `.cluster.txt` for the raw `ceph -s`)
* matrix: `bench/receipts/s3-compat-ceph-20261106T160906Z.json`
* MinIO matrix: `bench/receipts/s3-compat-minio-20261106T160906Z.json`
* comparison: `bench/receipts/s3-compat-matrix-20261106T160906Z.json`
* MinIO regression receipt for the modified client:
  `bench/receipts/s3-court-20261106T160906Z.json`

The existing S3 court logic passes against RGW: `cargo test --test s3_backend`
reports **6 passed; 0 failed** (44.77 s) — store round trip (content root,
generation sequence, read-only reopen match, clean `verify --full`), writer
lock with a lost conditional head update, the three-phase crash proxy, GC, and
local-vs-S3 byte identity.

Both implementations are **8/8** on the matrix. The one deviation is the
`If-Match` entity-tag quoting rule: MinIO accepts the quoted ETag it returned;
Ceph RGW compares the header bytes literally, so its own quoted ETag is
**accepted only after de-quoting** — `412 PreconditionFailed` for
`If-Match: "9abcde3c..."` and `200` for `If-Match: 9abcde3c...`, with the stale
value still refused `412`. Without the measured form the store refused RGW
writable open (`S3 conditional probe: a matching If-Match was unexpectedly
rejected`) and five store-level tests could not run.

Two RGW-specific configuration facts are recorded in the receipt:
`--rgw-thread-pool-size 32` (the default 512-thread RGW pool plus
`radosgw-admin`'s own 512-thread pool exceeds the 1024-pid court bound; the
kernel refuses thread creation and `ceph-mon`/`radosgw` abort silently — the
pids limit was not raised), and IP-literal addressing (RGW's demo config sets
`rgw dns name` to the container hostname and treats a matching `Host` as a
virtual-hosted bucket, so path-style requests need an endpoint whose Host is not
that name).

### Repository suites at this change

* `cargo test --all-targets`: 92 suites, **418 passed, 0 failed**.
* `cargo test --features fuse --all-targets`: 92 suites,
  **460 passed, 0 failed, 2 ignored** (the two pre-existing ignores for kernel
  7.3 direct-I/O EIO and the missing CAP_SETUID ownership test).
* Sidecar limits: 3g memory/swap, 1024 pids (observed 316 used, 319 threads,
  0 OOM kills), 4 cpus, bounded tmpfs, `cap-drop=ALL` plus CHOWN,
  DAC_OVERRIDE, FOWNER, SETGID, SETUID for the Ceph daemons' uid 167; peak
  container memory 1,120,411,648 B.

### What is not covered

No physical media or power-loss claim (the RGW OSD is a tmpfs file in one
container, and there is no RGW process-crash scenario); no multi-node,
multisite, or zone-replication claim; only the eight matrix behaviours (no TLS,
presigned URLs, multipart upload, policies/ACLs, versioning, object lock,
lifecycle, SSE); listing pagination exercised at `max-keys=2`, not at the
1000-key boundary; no Go-side S3 backend, so no cross-language claim against
RGW; no AWS or third-vendor measurement; no concurrency court beyond two
writers; no performance claim. The analysis, exact commands, and the raw
`If-Match` evidence are in `bench/RESULTS-ceph.md`.

### Aggregate coverage

`scripts/verify-all.sh` runs this court as its 28th entry (`ceph_rgw`), so the
aggregate now proves the S3 backend against an independent implementation on
every verification run, not only on the dedicated R16 receipts. The court
exit status is mapped explicitly: `0` is `pass`, a real failure is `fail` and
fails the aggregate, and exit `78` is `skip` — recorded as `skip` together with
the court's own reason string under a `skips` object in the verify receipt.
Skip is reserved for environment refusals detected *before* the server starts
(non-x86_64, no Docker daemon, the pinned Ceph image cannot be obtained, or the
court's private subnet is already in use); the rule is that **a skip is not a
pass**, and the receipt's `summary` block carries `courts`/`passed`/`failed`/
`skipped` so a skip cannot hide inside a percentage.

The first aggregate run containing this court is
`bench/receipts/verify-20261106T162208Z.json`: **28/28 courts pass, 0 skipped**
at WeftFS `0e3670a`, with the Ceph court's own receipt at
`bench/receipts/ceph-rgw-20261106T162208Z.json` (named in the verify receipt's
`receipts` block).

The skip path itself is measured: a second aggregate run at the same code with
`WEFT_CEPH_IMAGE` pointed at a nonexistent digest produced
`bench/receipts/verify-20261106T162933Z.json` (27 passed, 0 failed, 1 skipped;
`ceph_rgw: skip` with the reason `cannot obtain pinned image ... manifest
unknown`). That run is a deliberate negative test of the skip mechanism and is
not the current pass receipt; the all-pass run above is.

Gates: `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh`
pass (the frozen formats and spec files are untouched by this subphase).

---

## Subphase R17 — fenced writer protocol (2026-11-06)

Closes the reviewer-reported defect in `src/s3.rs`: `release_lock()` read
`LOCK`, checked the owner, then issued an **unconditional `DELETE`**, so a
writer whose lease a successor had just reclaimed could delete that successor's
lock; and nothing consulted the lease at publication time, so an expired writer
could keep publishing. A conditional head compare-and-swap alone is not
ownership proof, because it says nothing about who holds the lease.

### What is implemented

* `LOCK` is now `weftfs-s3-lock v2`: `owner`, a **monotonic fencing token**
  (`fence`), `sequence`, an absolute `lease_deadline_ns`, `renewed_ns`, and the
  head being published (`pending_generation`, `pending_commit`). The first
  lease takes fence 1; every takeover takes `previous + 1` (checked); a legacy
  v1 record is read as fence 0 so the chain stays monotonic across the change.
* **The fence is one conditional write.** Before every publication the store
  writes `LOCK` with `If-Match: <ETag of the bytes this handle last
  published>`. That succeeds only while the object still carries this writer's
  token, so a successor's takeover (which rewrote the object) makes a stale
  writer fail `412` → `Busy`, *before* the head is written. The store calls the
  proof before the commit record and again — this time naming the head — as the
  last write before the superblock compare-and-swap.
* **Lease renewal** happens inside that same conditional write, once per
  publication, when at most one publication budget remains (`deadline - now <=
  lease/2`), extending the deadline to `now + lease`. The interval is bounded
  by the lease.
* **Own-deadline stop.** A writer refuses to publish when its own deadline has
  passed, without consulting anyone and even with no successor. The refusal
  names the expired token and, read-only, who holds the lock now.
* **Release is a conditional tombstone**, never an unconditional delete:
  `weftfs-s3-lock-released v1` with `If-Match` on the observed ETag, carrying
  the released token and `released_from` (sha256 of the exact bytes replaced).
  A record that names another writer is never touched. A tombstone is preferred
  over a conditional `DELETE` because deleting the object would reset the
  fencing token. The writable-open probe already refuses an endpoint whose
  conditional writes cannot be proven, which is the primitive this needs.
* **The same question for POSIX backends.** `LocalBackend` and `ObjectBackend`
  refuse to publish when the `LOCK` name no longer resolves to the exact
  `(dev, ino)` this handle flocked, i.e. when the namespace was handed to
  someone else.
* `CompositeBackend` delegates the proof to its commit plane (the plane that
  owns `LOCK` and the head), so a split store with S3 blobs and local commits
  is fenced by its local lock and an S3 commit plane is fenced by its lease.

### Exact atomicity statement

S3 allows one conditional write per object and no multi-object transaction, so
the fence and the head write are two operations on two objects and cannot be
fused. Safety does not depend on fusing them: a publication may only start
while `now + budget <= deadline` (`budget = lease/2`), and a takeover is only
allowed at `now > deadline`, so the publication window closes before any
successor may touch the lock; the gate is the last write before the head CAS.
A lost network reply remains the standard at-least-once ambiguity of any
conditional `PUT` and is recorded as a residual, not as a pass.

### Evidence

* `tests/s3_fencing.rs` (MinIO court): **8/8** in 15.23 s. Real `SIGSTOP` child
  (the test binary re-executes itself), reclaimed lease, `SIGCONT`; paused
  writer fence 2 → successor fence 3, `Busy`, paused generation and head
  unchanged, published head the successor's, `/fenced` absent, `verify --full`
  ok. Stale release leaves the successor's `LOCK` bytes and ETag identical.
  20 release/acquire races each advance the token by exactly one and never
  delete the object. Two renewal windows extend the deadline with a constant
  token. Five leases produce fences 1–5.
* `examples/distributed_court.rs` scenario **S10**: 26 checks, **10/10
  scenarios, 255 checks, 0 failed, 126 processes**
  (`bench/receipts/distributed-20261106T174634Z.json`). Token map
  `{paused 2, successor 3, release_holder 2, release_racer 3, renew 2, expired
  2}`, generation sequence `[2, 2, 2]`.
* `tests/writer_fencing.rs` (local + object): 4/4.
* Rust `cargo test --all-targets`: 94 targets, **431 passed, 0 failed**.
  FUSE `--features fuse --all-targets -- --test-threads=1`: 94 targets,
  **473 passed, 0 failed, 2 ignored** (both pre-existing).
* S3 court receipt `bench/receipts/s3-court-20261106T174609Z.json` (6 backend +
  8 fencing tests, MinIO): s3_backend 6/6 in 0.53 s, s3_fencing 8/8 in 15.23 s.
* Independent implementation: `scripts/ceph_court.sh` now runs the fencing
  suite too. Against Ceph RGW 19.2.0 (literal `If-Match`, dequoted ETag, and
  roughly an order of magnitude slower per round trip) `s3_backend` is 6/6 in
  41.96 s and `s3_fencing` 8/8 in 80.85 s. Receipts
  `bench/receipts/ceph-rgw-20261106T174703Z.json` and the aggregate
  `bench/receipts/verify-20261106T174703Z.json` (**28/28 courts pass** at
  `37a3a39`).
* Analysis and exact commands: `bench/RESULTS-fencing.md`.

### Lease configuration (and one behaviour change)

The fencing suite needs a lease a court can expire (`WEFT_S3_FENCE_LEASE_SECS`,
3 s); the general suites use 60 s (`WEFT_S3_LOCK_LEASE_SECS`). The split exists
because a writer that stays **idle past its deadline must reopen**: the deadline
check fires before the lock is read, so a long-idle handle gets `Busy` instead
of silently re-acquiring. That is deliberate (the reviewer's requirement is
that a writer stops once its deadline has passed), but it is a real behaviour
change for a rarely-writing long-lived handle. The first aggregate run used a
2 s lease for every S3 court and `store_round_trip_over_s3` failed on RGW with
`S3 writer lease deadline passed 216 ms ago`; that receipt
(`verify-20261106T171925Z.json`) is retained as the evidence.

Two more defects the RGW run found were in the harness, and are fixed: the
crash-proxy child's 1 s lease expired *mid-publication* on the slower endpoint
(the child now gets a realistic lease and the reset is the fenced tombstone),
and `release_stale_lease` did not negotiate the endpoint's `If-Match` form on a
fresh handle, so every court reset was a `412`. The release/acquire race test
also genuinely races now: the acquirer retries until it has observed the live
lock, records at least one `Busy`, and then wins with the next token.

### Not claimed

Not a multi-writer protocol: exactly one writer is admitted, and there is no
merge, queue, or consensus point for the head. No single atomic fence+head
operation (not expressible on plain S3). No defence against a badly skewed
writer clock beyond the budget assumption. The **GCS backend kept its older
lease code and no fence** in R17 (`verify_write_lease` defaulted to a no-op
there); that gap is closed in R18 below. No
Go parity (WeftFS-Go has no S3 backend). No physical-media or power-loss claim.
`scripts/verify-all.sh` passes 28/28 after this subphase
(`bench/receipts/verify-20261106T174703Z.json`); the aggregate includes the
MinIO and Ceph-RGW courts with the fencing suite, but not the separate
multi-process `s3_court.sh --distributed` run.

Gates: `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh`
pass; no frozen format changed, so no spec edition was needed.

## Subphase R18 — fenced writers on GCS (2026-11-06)

Closes the reviewer-reported gap in `src/gcs.rs`. Before this subphase the GCS
backend still had the pre-R17 shape:

* `release_lock()` read `LOCK`, checked that the body named this owner, then
  issued an **unconditional delete**; a successor that had reclaimed the lease
  in between lost its lock.
* `acquire_lock` wrote `owner` + `expires_ns` (`weftfs-gcs-lock v1`) and
  nothing consulted the lease at publication time.
* `verify_write_lease` was not implemented, so the trait default (`Ok(())`)
  let a stale writer publish, and `fence_head_publication` fell back to the
  same no-op. The head compare-and-swap only proves the head did not move; it
  says nothing about who holds the lease.

### What is implemented

* `LOCK` is now `weftfs-gcs-lock v2` with the **same field set as S3's record**:
  `owner`, monotonic `fence`, `sequence`, absolute `lease_deadline_ns`,
  `renewed_ns`, `pending_generation`, `pending_commit`. A legacy
  `weftfs-gcs-lock v1` record is read as fence 0, so the chain stays monotonic
  across the upgrade. Encode/parse/describe now live in `src/writer_fence.rs`
  and are shared by both backends; only the schema header differs.
* **Takeover** of an expired lease or a released tombstone is one upload with
  `ifGenerationMatch=<the generation read>`, taking `previous.fence + 1`
  (checked). A lost race retries from a fresh read.
* **The fence is one generation-conditioned upload.** Before every publication
  the store uploads `LOCK` with `ifGenerationMatch=<the generation this handle
  last published>`. That succeeds only while the object still carries this
  writer's token, so a successor's reclaim (which changes the generation) makes
  a stale writer fail `412` → `Busy` before the head is written. The store calls
  the proof before the commit record and again, naming the head, immediately
  before the superblock CAS.
* **The upload path reports the generation** (`upload_tracked`). The JSON API
  returns the object resource in the reply; if an endpoint omits it, the
  generation is confirmed by a read-after-write that must return exactly the
  bytes just published. An endpoint that can do neither is refused
  `UnsupportedVersion` rather than guessed at.
* **Renewal** happens inside the same conditional upload, once per publication,
  when at most one publication budget remains (`deadline - now <= lease/2`),
  extending the deadline to `now + lease`.
* **Own-deadline stop**: a writer whose `now >= lease_deadline_ns` refuses
  before any head object is touched, even with no successor.
* **Release is a conditional tombstone** (`weftfs-gcs-lock-released v1`) written
  with `ifGenerationMatch` on the observed generation, carrying the released
  token and `released_from` (sha256 of the exact bytes replaced). A lock that
  now names another writer is never touched. `LOCK` is never deleted after a
  read.
* `verify_write_lease` and `fence_head_publication` are implemented on both the
  `StorageBackend` and `CommitPlane` impls; `CompositeBackend` already delegates
  to its commit plane, so a split store with GCS commits is fenced too.
* Diagnostics gained the same counters as S3 (`fence_checks`,
  `lease_renewals`, `fence_refusals`, `lease_expired_publications`). The
  end-to-end court observed **42 fence CAS operations for 21 commits**
  (proof before the commit record and fenced proof before the head).

### Protocol mapping

| concern | S3 (R17) | GCS (R18) |
|---|---|---|
| conditional primitive | `PUT` with `If-Match` | upload with `ifGenerationMatch` |
| create once | `If-None-Match: *` | `ifGenerationMatch=0` |
| observed token | ETag string | object generation (`u64`) |
| token source | response `ETag` header | upload reply object resource (verified re-read fallback) |
| stale write | `412` → `Busy` | `412` → `Busy` |
| takeover | `If-Match` on observed ETag | `ifGenerationMatch` on observed generation |
| fence | conditional rewrite of `LOCK` | conditional upload of `LOCK` |
| renewal | same conditional write | same conditional upload |
| release | tombstone with `If-Match` | tombstone with `ifGenerationMatch` |
| record | `weftfs-s3-lock v2` | `weftfs-gcs-lock v2` (identical fields) |
| tombstone | `weftfs-s3-lock-released v1` | `weftfs-gcs-lock-released v1` |
| writable-open proof | `If-Match` accepted, stale refused | matching generation accepted, stale refused |

### Shared invariant tests

`tests/support/fencing.rs` holds the test bodies; `tests/s3_fencing.rs` and
`tests/gcs_fencing.rs` import them and supply only the court configuration and
observer hooks. The same five invariants therefore run on MinIO, Ceph RGW, and
`fake-gcs-server`:

1. an expired lease with **no successor** refuses publication; head,
   generation, commit count, bytes, and the still-live token are unchanged;
2. a **paused** writer (real `SIGSTOP` child, reclaimed lease, `SIGCONT`) fails
   `Busy`; its own generation and head do not move; the successor's head is
   published and its file is the only visible one;
3. **release of a replaced lock** leaves the successor's `LOCK` bytes and
   conditional token byte-for-byte and value-for-value identical;
4. **renewal** extends the deadline under the same conditional write, keeps
   `fence` constant, and advances the sequence across two windows;
5. **takeover monotonicity**: five leases produce fences 1–5 and every
   publication names its pending head generation.

The S3 court additionally keeps its release/acquire race (20 iterations, the
acquirer records at least one `Busy` and wins with `fence + 1`, the object is
never deleted).

### Exact atomicity statement

Unchanged from R17 in form, but GCS makes the two operations even more explicit:
the fence is an upload of `LOCK` conditioned on the old `LOCK` generation, and
the head is a compare-and-swap on `superblock` conditioned on the old superblock
generation. These are two conditional writes on two objects; GCS offers no
multi-object transaction. Safety does not depend on fusing them: a publication
may only start while `now + budget <= deadline` (`budget = lease/2`), and a
takeover is only allowed at `now > deadline`, so the publication window closes
before any successor may touch the lock; the fence is the last write before the
head CAS. A lost upload reply remains the standard at-least-once ambiguity of
any conditional write and is a recorded residual.

### Evidence

* **GCS (`fake-gcs-server` v1.56.1, tarball sha256
  `0f2f9f6884417acd5cc27126a9aa237ccc627372a4aa5008b399641de6db9469`)** at
  `5667c7c`: `tests/gcs_backend` **15/15** (1.42 s, including the
  omitted-generation fallback) and `tests/gcs_fencing` **7/7** (14.60 s, 3 s
  lease), then the end-to-end court with `content_root_match=true`,
  `generation_match=true`, `generation_sequence_match=true`. Court receipt
  `bench/receipts/gcs-court-20261106T182003Z.json`; harness receipt
  `bench/receipts/gcs-20261106T182003Z.json`; raw logs
  `gcs-20261106T182003Z.fencing.log` and `.tests.log`.
* **MinIO `RELEASE.2025-04-22T22-12-26Z`**: `s3_backend` **6/6** and the
  refactored shared suite `s3_fencing` **8/8** (15.19 s). Receipt
  `bench/receipts/s3-court-20261106T182043Z.json`.
* **Ceph RGW 19.2.0** (`quay.io/ceph/demo@sha256:522483cf...`): `s3_backend`
  **6/6** (41.94 s), `s3_fencing` **8/8** (80.87 s), compatibility matrix
  **8/8**. Receipts `bench/receipts/ceph-rgw-20261106T183427Z.json` and
  `bench/receipts/s3-compat-*-20261106T183427Z.json`.
* Rust `cargo test --all-targets`: **95 test binaries, 445 passed, 0 failed**.
  FUSE `cargo test --features fuse --all-targets -- --test-threads=1`:
  **95 test binaries, 487 passed, 0 failed, 2 ignored** (both pre-existing).
* Single-run cost observation (not a benchmark; two runs, not interleaved):
  the same 21-commit GCS workload was 14.64 → 15.96 ms create, 11.52 → 12.62 ms
  range write, 4.49 → 4.89 ms read, 9.86 → 9.22 ms reopen, 27.84 → 27.56 ms
  verify against `bench/receipts/gcs-20261106T150444Z.json` (pre-fence). The
  fence is two extra small conditional uploads per commit.
* Gates: `bash scripts/check-claims.sh` and `bash scripts/check-spec-drift.sh`
  pass. No frozen format changed, so no spec edition was needed.

### Flake disclosure

The first full Rust run after stage 1 failed
`distributed_semantics::single_writer_interlock_is_exclusive_and_reopens_cleanly`
at its `Store::open(...).unwrap()` after the first handle was dropped. It passed
standalone immediately, that whole binary passed **5/5** consecutive runs, and
the full suite rerun was **445 passed / 0 failed**. R18 touches no local-backend
code. Recorded here rather than silently omitted; the same intermittent
fresh-reopen shape was already noted for `tests/quota.rs` in R9.

One harness defect was found by the first court run and fixed:
`scripts/docker-court.sh` forwards an explicit environment allowlist, so
`WEFT_GCS_FENCING_LOG` did not reach the container. The failed court receipt
`bench/receipts/gcs-court-20261106T180729Z.json` is retained as evidence.

### Not claimed

No multi-writer protocol: exactly one writer is admitted, and there is no merge,
queue, or consensus point for the head. No single atomic fence+head operation
(not expressible on GCS's per-object preconditions). No defence against a badly
skewed writer clock beyond the budget assumption. **No Go parity: WeftFS-Go has
no GCS backend.** No claim about a real Google Cloud Storage service: the
evidence is one single-node emulator with a memory backend, so no multi-node,
device-cache, media, power-loss, or production claim is made. No performance
claim from the single-run phase comparison above.

`scripts/verify-all.sh` was **not** rerun for R18 (the GCS court is not part of
the 28-court aggregate); the suites, both gates, and the three fencing courts
were run directly and are cited above.

## Subphase R19 — pipelined restore (2026-11-06)

Closes the reviewer's top remaining item: parallel tensor decode existed, but
`materialize` still decoded, returned one `Vec<Vec<u8>>`, and only then wrote the
988 MB container, so the write was serial after the decode. The R4 court
measured that as a 0.43x 4-thread regression on the host bind mount.

### What is implemented

* `src/restore_pipeline.rs`:
  * `run_ordered_pipeline(inputs, sizes, options, work, measure, consume)` — an
    ordered producer/consumer runner. Workers decode with a bounded count and a
    bounded declared-byte reservation; **one writer (the calling thread) calls
    `consume` for indices 0, 1, 2, ... in exact source order**, outside the
    queue lock. There is no whole-file buffer.
  * `RestorePipelineOptions { threads, max_inflight_bytes }` and
    `RestorePipelineStats`: tasks, tensors/bytes written and dropped, observed
    in-flight bytes/tasks, oversized tasks, `decode_wall_ms`, `decode_sum_ms`,
    `transfer_ms`, `write_ms`, `total_ms`.
  * `Store::restore_tensors_pipelined` and
    `Store::restore_tensors_pipelined_authorized`, bound to the shipped
    `read_tensor` / `read_tensor_authorized` path (R8 cache/single-flight still
    apply where they applied before).
* Bounds and policy:
  * workers: `0 -> 1`, capped at `available_parallelism()`;
  * **reservations are granted in input order**, which is what makes the
    in-order writer deadlock-free against the staging bound;
  * non-oversized reservations (decoding + published-but-unwritten + the tensor
    being written) are bounded by `max_inflight_bytes` (default 64 MiB); an
    oversized tensor is tracked separately, at most one outstanding, so the peak
    is at most `max_inflight_bytes + largest tensor`;
  * the writer consumes strictly in order, so the first failure it sees **is**
    the lowest failing input index; it then stops claiming work, drains every
    in-flight decode, frees every reservation, joins every worker, and returns
    that error. The sink holds exactly the prefix before the failure.
  * the store is only read; the sink is the caller's `Write`, never a store
    namespace.
* Harness: `real_model_bench restore --materialize-pipeline --threads N` writes
  the exact safetensors header and then streams tensors through the pipeline.
  `--checkpoint base|instruct` scopes a bench run (library/CLI unchanged).
* **Read-induced store mutation removed**: `weftfs checkpoint restore` is now
  read-only in the store by default. It no longer reopens read-write and imports
  the container under `--out` unless `--import` is passed (`--out`/`--out-name`
  are rejected without it). The Python DCP/Orbax readers gained
  `import_restored` (default `False`) and pass `--import` only when asked, so
  the `/dcp-reads/<uuid>` copies seen in the Q5/R15 receipts are no longer an
  implicit cost. Both docs were updated.

### Evidence

* Court `bench/scripts/restore_pipeline_court.sh` +
  `bench/scripts/assemble_restore_pipeline.py`, receipt
  `bench/receipts/restore-pipeline-20261106T204445Z.json` (20/20 checks),
  analysis `bench/RESULTS-restore-pipeline.md`, at WeftFS `eb72c72`. Cached
  Qwen2.5-0.5B pair, source SHA-256 re-hashed in court, one cached store, three
  interleaved repetitions, output removed and `sync` between runs.
* **tmpfs (controlled, base checkpoint; the write is effectively free):**

  | threads | batch ms | pipeline total ms | decode wall ms | transfer ms | write ms | pipeline vs batch |
  | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
  | 1 | 4,913.0 | 4,760.1 | 4,661.0 | 0.0 | 96.7 | 1.032x |
  | 2 | 3,140.9 | 2,844.0 | 2,842.6 | 2,713.1 | 134.3 | 1.104x |
  | 4 | 2,354.2 | 2,157.3 | 2,155.9 | 2,005.9 | 151.0 | 1.091x |

  Pipeline scaling is 1.674x at 2 workers and 2.207x at 4; at 2/4 workers
  `total ≈ decode`, i.e. the write is overlapped. Peak RSS 727,488/857,100/
  853,040 KiB (batch) and 726,936/864,040/897,776 KiB (pipeline).
* **Host bind mount (real path, both checkpoints):** the same work ranged
  **2,526.4–15,325.7 ms** for batch materialize and **2,976.0–17,519.3 ms** for
  the pipeline total across three repetitions; the pipeline's own write ranged
  237.2–11,967.0 ms. That host writeback spread is larger than every mode
  difference, in both directions. **No performance claim is made from the disk
  rows**; they are kept in the receipt precisely to show the variance. On the
  disk the pipeline wins 1.177x at 4 threads (instruct) and loses 1.35x at 4
  threads (base) within the same court.
* Exactness in every mode/thread/destination: 290/290 per-tensor BLAKE3, every
  logical root, restored bytes identical, and the final SHA-256 equal to
  `88c14255...0ed342` (base) / `fdf756fa...88fb7fe` (instruct). Pipeline wrote
  290 tensors and 988,065,536 B per checkpoint, and observed in-flight bytes
  never exceeded 64 MiB + 272,269,312 B (the one oversized embedding).
* Tests `tests/restore_pipeline.rs` **14/14**: byte identity at 1/2/4 threads,
  ordering under skewed decode times, the observed staging bound, one oversized
  task at a time plus an oversized-between-normal case, lowest-index error with
  a deterministic written prefix (synthetic and corrupted-manifest forms), sink
  failure, declared-size errors not stalling later reservations, empty input,
  **no store mutation on a read-only restore** (head, generation, full tree
  digest, `verify --full`), and a real `SIGABRT` child killed mid-write that
  leaves exactly a truncated source prefix and an unchanged store.
* Suites: Rust `cargo test --all-targets` **96 test binaries, 460 passed, 0
  failed, 0 ignored**; FUSE `cargo test --features fuse --all-targets --
  --test-threads=1` **96 test binaries, 502 passed, 0 failed, 2 ignored** (the
  same two pre-existing ignores). That is 15 new tests (`tests/
  restore_pipeline.rs` 14, plus
  `cli_checkpoint_restore_is_read_only_unless_imported`). Python in the python
  court with the rebuilt CLI: `tests/test_dcp.py` **10 passed**,
  `tests/test_orbax.py` **18 passed**. `scripts/check-claims.sh` **393/393**,
  `scripts/check-spec-drift.sh` **42/42**. `verify-all` was not rerun (no frozen
  format, court list, or aggregate input changed).
* Flake disclosure: the first full Rust run failed
  `backend::capabilities_fail_closed` at its read-write
  `Store::open_with_backend(...).unwrap()` on a fixture it had just built. It
  passed **5/5** standalone immediately after, and the full suite rerun was
  **460 passed / 0 failed**. R19 touches no store-open code; this is the same
  intermittent fresh-reopen shape already disclosed in R9 and R18.

### Losses and not claimed

* The pipeline does not make the write faster and cannot control disk
  bandwidth. On this host the disk end-to-end claim is not measurable.
* One worker is only 3.0 % faster on tmpfs; overlap needs at least two workers.
* The 272 MB embedding remains the serial oversized task; the pipeline does not
  split one tensor.
* Full-pair restore still peaks at ~2.0–2.2 GB RSS on the host mount because the
  harness restores both checkpoints in one process.
* No Go parity for the ordered pipeline; no change to the on-disk format, codec,
  CLI restore semantics beyond the opt-in import, FUSE path, or selectors.
* Single host, warm cache, one store, three repetitions; no cold-cache, fleet,
  GPU, or multi-node claim.

## Subphase R20 — Go parity for the R9–R12 storage surfaces (2026-11-06)

The two-implementation guarantee covered the codec, store, contexts, GC, quota,
and FUSE, but the Rust storage work from R9–R12 had no Go counterpart. R20
closes four of those gaps in dependency order on `staging`.

### What is implemented

* **`weft.manifest.v4` overlay writer and compaction (Go).** The previous
  version is the immutable base plus one merged extent; a misaligned edit is
  widened to the enclosing chunk grid with untouched bytes read from the
  previous version; adjacent or overlapping extents fold into the current layer
  while it stays at or below `model.MaxOverlayExtentsPerManifest` (64) and a new
  layer is published only when folding would exceed that bound or the writer's
  configured `MaxOverlayDepth` / `MaxOverlayBytes`. The writer derives the
  canonical table twice — previous table + edit, and the manifest the reader
  will resolve — and refuses `Corrupt` unless both are identical. `Store.Compact`
  re-reads and re-hashes every chunk (one chunk of memory), requires the
  pre-compaction Merkle root, then publishes a canonical v3 table through the
  normal commit protocol. `Truncate` (grow), `ZeroRange`, and range writes publish
  layers; shrinking truncate, `collapse_range`, and `insert_range` are refused
  with the same wording as Rust. CLI: `compact`, `--representation overlay`
  (which requires `--content-identity merkle`).
* **Blob/commit plane split (Go).** `BlobAccess` routes `objects/`,
  `manifests/`, `indexes/`, `packs/`, and `pack-catalog/`; the commit plane owns
  `FORMAT`, `LOCK`, the superblock, `commits/`, `refs/`, quota, and access
  counters. The default local/local layout writes the historical bytes. `local`
  publishes with `rename(2)` over a fsynced temporary file; `object` uses
  `link(2)`. Capabilities are **measured** on every writable open: the blob probe
  publishes a private 192-byte object, HEADs its length, reads a 17-byte
  mid-range slice, and deletes it; the commit probe performs a create-once CAS, a
  matching CAS, and a stale CAS that must be refused with `Busy`. A
  declared-absent `posix` plane is refused for writing (and `Init` leaves no
  `FORMAT` marker), while read-only opens never probe or write. CLI:
  `--blob-backend`, `--commit-backend`, `--blob-root`; `info` reports both
  planes, their roots, and the probe evidence.
* **Physical pack plane (Go).** Identical wire format to Rust: `WPS1` catalog
  segments (magic, version, index, count, 113-byte entries, domain digest),
  `WPR1` root (magic, version, segment count, total, ids, digest), the same four
  domains, segment index masking, and member digests. Packs are published
  before any catalog record names them; the open pack is flushed before every
  commit record; the catalog is bounded and falls back to flat publication
  without failing. CLI: `--pack`, `--pack-target-bytes`,
  `--pack-member-threshold`, `--pack-catalog-bytes`, `--pack-segments`; `info`
  reports catalog truth. GC reclaims unreferenced packs and orphaned segments;
  `verify --full` reports catalog problems and reads through the packed plane.
* **Host calibration (Go).** `weft.calibration.v1` documents are loaded with
  exact schema, key-set, row-set, sample, bound, monotonicity, and digest
  validation and no fallback. The digest is computed over the compact key-sorted
  document with the digest key removed, matching `serde_json`'s BTreeMap form.
  `--planner-cost-model declared|measured` and `--calibration FILE` are accepted
  (`--calibration` requires `measured`); the validated table overrides the baked
  coefficients in tensor planning and rebase pricing.

### Evidence: cross-language byte equality

All values below are asserted by the named tests; they are not estimates.

* **Overlay writer, fixed clock `1700000000000000000` and seed
  `overlay-same-seed`, six 64 KiB chunks + 77 bytes, writes at 100, 150000, and
  400000:** Go and Rust produce the **same overlay manifest id**
  `f04879ebaf27059d16f8f1ef09543c89bf43e5cd30d4479252e0ecf20beca5f9`, the
  **same 605 manifest bytes**, and the **same published head**
  `64103f3b7d1961f415b694c954271e9f363c3e60b7582cb9d1a3051c1b2c3f48`
  (`TestOverlayWriterMatchesRustByteForByte`). Rust `verify --full` accepts the
  Go store.
* **Continuation and compaction both ways:** Rust extends and compacts a
  Go-written overlay; Go extends and compacts a Rust-written overlay; every
  result is read byte-exactly by the other language with `verify --full` clean
  (`TestOverlayGoExtendsRustStore`, `TestOverlayGoCompactsRustStore`,
  `TestOverlayRustCompactsGoStore`).
* **Four split layouts** — local/local, object/object, object-blobs/local-commits,
  local-blobs/object-commits — publish the **same head**
  `6ce4fb88479aa685bcb749f7fba061bf1bb86b0a31de008b1b30d093b9754a9f`,
  **generation 3**, and **manifest**
  `fe6b7cff0e29a36c6a11b7a3797969c84b622da054f6780d041cb22753105e73`
  (`TestSplitPlanesMatchTheSingleBackendStore`). Rust reads and verifies a
  Go-split store; Go reads and verifies a Rust-split store, with matching head
  and manifest id (`TestGoSplitStoreRustReadsAndVerifies`,
  `TestRustSplitStoreGoReadsAndVerifies`).
* **Packs:** Go's 300,000-byte file seals **2 packs / 8 members / 366,270 packed
  bytes with 0 flat members** under the default policy (head
  `6ba667e1…e76a`, manifest `8635b945…af33`). Rust `verify --full` and `cat`
  agree; Go reads and verifies Rust-written packs; Go GC reclamation is then
  re-verified by Rust (`TestGoPacksRustReadsAndVerifies`,
  `TestRustPacksGoReadsAndVerifies`, `TestGoGCReclaimsPacksThenRustVerifies`).
* **Calibration:** a document produced by the Rust `calibrate` command loads in
  Go with the **identical 64-hex digest** and seven measured rows
  (e.g. `12bc635c1620b5d4ed4c47f39c6817fe386dd5263bfe8f7d1676aef367a38cd5`);
  loading re-computes the digest, so acceptance is evidence that Go's JSON
  canonicalisation matches Rust's (`TestRustCalibrationDocumentIsAccepted`).
* **Commits:** WeftFS-Go `fa186a3` (stage 1), `9d78940` (stage 2), `cc988fb`
  (stage 3), `f577cb7` (stage 4), `8670d77` (evidence logging, the measured
  tree of the aggregate above), and `447b702` (NOTES.md only). Parent pins:
  `ae11484`, `d0e0da9`/`9a6156c`, then the final pin. The final pin adds only
  NOTES.md to the measured submodule tree.

### Repository suites at this change

* Go: **168** top-level tests in `store`, **15** in `cmd/weftfs`, and **24** in
  `fusefs`, run as `go test -race ./... -count=1` in the FUSE court: **0
  failures**. **51 new top-level tests** were added (46 store + 5 CLI), plus the
  32-case calibration rejection matrix as subtests. Cross-language tests invoke
  the Rust binary only as an oracle.
* Honest note on skips: the Go court image has no `libfuse3`, so when the shared
  Rust release binary was built with `--features fuse`, oracle tests skip there
  with the loader error. The aggregate runs the same tests in the FUSE court,
  where they execute and pass. A skip is never counted as a pass.
* Rust: `cargo test --all-targets` **460 passed / 0 failed** (96 binaries);
  `cargo test --features fuse --all-targets -- --test-threads=1` **502 passed /
  0 failed / 2 ignored**; WeftCodec **226 passed / 0 failed**.
* Gates: `scripts/check-spec-drift.sh` **42/42**; `scripts/check-claims.sh`
  **399/399**; conformance manifest and coverage unchanged.

### Aggregate coverage

`scripts/verify-all.sh` now runs the MinIO court as its own **29th** court
(`s3_court`) with the same environment-only skip semantics as `ceph_rgw`:
exactly one `s3_court: skip <reason>` line and exit 78 for a non-x86_64 host, no
Docker CLI or daemon, no `curl`, an unobtainable pinned MinIO release or
`debian:bookworm-slim` image, or a Docker network that cannot be created.
Everything after preflight is a hard failure. One helper now handles both
orchestrated courts (pass / explicit skip / fail) and the receipt gains an
`s3_court` receipt pointer. The R20 aggregate
`bench/receipts/verify-20261106T214749Z.json` is **29/29 pass, 0 failed, 0
skipped** at WeftFS `9a6156c` / WeftFS-Go `8670d77`. The failing first R20 run
`verify-20261106T212741Z.json` (28/29) is retained: `rust_store` hit the
pre-existing `tests/quota.rs` writer-lock flake disclosed in R9, R18, and R19;
the test passed standalone immediately after and in the rerun.

### Losses, non-claims, and what Go still lacks

Revised after R24–R27. The original R20 text below was written when the Go
store had no S3 backend, no calibration measurement, no pack/plane courts, and
no index persistence; those are now implemented. The reconciled list is:

* Go **has** a v4 overlay writer and compaction, the physical pack plane, a
  probed blob/commit plane split (`--blob-backend`/`--commit-backend`, all four
  local/object/S3 layouts), `weft.calibration.v1` **loading**, `calibrate`
  **measurement** (a Rust-accepted document with the identical id), a real
  **S3 backend** (SigV4, `If-None-Match`/`If-Match`, the fenced writer), and
  **WFX2** selector-index persistence readable both ways.
* Go still has **no GCS or Azure backend**; its plane split over a remote
  endpoint is S3-only, and there is no cross-vendor Go claim.
* Go still has **no `calibrate --install`** command. It measures, validates,
  loads, and applies a document.
* Go has **no FUSE-over-S3 coverage**: the Go S3 court opens the store through
  the API and the CLI, not through a mounted filesystem.
* Packs are **never rewritten**: a dead member's bytes stay until the whole pack
  is unreferenced. A sub-member read cannot verify the member digest by design;
  only whole-member reads do.
* Overlay: Go refuses shrinking truncate, `collapse_range`, and `insert_range`
  (matching Rust); there is no streaming overlay writer.
* No performance claim, no physical power loss, no multi-writer, no fleet or
  cloud claim. Crash evidence is the process-fault proxy used elsewhere.

## Subphase R21 — Azure Blob and GCS service-account auth (2026-11-06)

Two backend gaps are closed: there is a real Azure Blob REST backend, and the
GCS backend can authenticate with a service-account key instead of a bearer
token supplied out of band.

### What is implemented

**Azure Blob** (`src/azure.rs`, 2,679 lines with tests)

* Blob REST over the existing blocking `ureq` client: `Put Blob` with
  `x-ms-blob-type: BlockBlob` (`If-None-Match: *` for create-once, `409
  BlobAlreadyExists`; `If-Match: <ETag>` for compare-and-swap, `412`),
  `Get Blob` (whole and `Range`), `Get Blob Properties`, `Delete Blob`,
  `List Blobs` (`restype=container&comp=list`), and container create/properties.
* `BlobPlane`, `CommitPlane`, and `StorageBackend` implementations. The public
  `ControlRecord.token` remains the **content token** of the record's bytes; the
  service ETag is the private transport token, so a caller can never confuse the
  two. `--backend azure` also works for `--blob-backend`/`--commit-backend`.
* Credentials: **SharedKey** (HMAC-SHA256 over the documented canonicalised
  headers and resource, implemented with `sha2` only), a **SAS** query string,
  or **anonymous** for an emulator. The three forms are mutually exclusive and
  refused when mixed.
* The R17/R18 fenced writer is ported verbatim: monotonic fencing token in the
  `LOCK` blob, one conditional write before each publication naming the pending
  head, renewal when one publication budget remains, refusal after the writer's
  own deadline, and a content-addressed conditional tombstone for release.
* Endpoint behaviour is **measured**, not assumed: the writable-open probe
  requires create-once, a matching `If-Match`, and a **refused stale**
  `If-Match` with the winner's bytes surviving. It also measures whether the
  endpoint compares the quoted ETag or a dequoted value (the Ceph RGW lesson
  from R16). An endpoint that accepts a stale value is refused
  `UnsupportedVersion`.
* Resource canonicalisation is `/{account}{request path}`. On a path-style
  endpoint (Azurite) the account appears twice; that is what both Azurite and
  Azure recompute. The first court run used `/{account}/{container}/{blob}` and
  every request failed authentication (`azure-court-20261106T221522Z.json` is
  retained for that defect).

**GCS service-account auth** (`src/service_account.rs`)

* RS256 JWT-bearer flow with no RSA dependency: strict DER for PKCS#8
  (`BEGIN PRIVATE KEY`) and PKCS#1 (`BEGIN RSA PRIVATE KEY`), fixed-width
  big-integer Montgomery arithmetic, EMSA-PKCS1-v1_5 with the SHA-256
  `DigestInfo` prefix, and CRT signing. `#![forbid(unsafe_code)]` still holds.
* The key is proved at load time: `p·q == n`, the CRT result equals
  `m^d mod n`, and the public exponent inverts the private operation. A key that
  fails any check is refused `InvalidArgument` before it can sign.
* Assertion: `{"alg":"RS256","typ":"JWT"}` plus `kid` when `private_key_id` is
  present, and `iss`/`scope`/`aud`/`exp`/`iat` with a one-hour lifetime.
* Token exchange with a single-slot cache and a refresh window of
  `min(300 s, lifetime/2)`. One exchange under concurrency. Provider refusals
  are typed (`Unauthorized` for 400/401/403, `Corrupt` for an unreadable or
  lifetime-less reply, `UnsupportedVersion` for a non-Bearer token type, `Io`
  for an unreachable endpoint).
* Explicit refusals, with what would be required: a non-`service_account`
  credential (`authorized_user` needs a refresh-token flow), an encrypted
  private key (PBES2/PBKDF2), a non-RSA key, a modulus outside 2048..=8192 bits,
  and an inconsistent key.
* Key material, the assertion, and the access token are redacted from `Debug`
  and never appear in an error message. Diagnostics name only the key's
  SHA-256 fingerprint and modulus width.

### Evidence

* **RFC 7515 Appendix A.2.** `tests/service_account.rs` signs the published
  RS256 signing input with the published 2048-bit key and asserts the exact
  published signature octets, in both PKCS#1 and PKCS#8 encodings. This is the
  external cross-check of the RSA and JWS code; it needs no network.
* **Azure emulator.** `bench/scripts/azure_court.sh` runs pinned Azurite
  `3.35.0`, image
  `mcr.microsoft.com/azure-storage/azurite@sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37`,
  as a sibling container with the same hard limits (3g/3g, 1024 pids, 4 cpus,
  bounded tmpfs, `cap-drop=ALL`, `no-new-privileges`). Court receipts
  `azure-court-20261106T231026Z.json` (current) with
  `azure-court-20261106T230714Z.json` and
  `azure-court-20261106T221838Z.json` as earlier runs of the same workload
  (same root and counts):
  * local and Azure runs have the **same content root**
    `0a8198f56ed59a11d3a704b05814232359e4fb6544d4dc241d41d2fd59756c5f`, the
    same generation sequence 1..21, the same verified bytes (393,216) and the
    same content digest.
  * the `superblock` ETag differs at every sampled publication (three samples in
    the current run: `"0x20B1B3025C57900"`, `"0x211D4915F03EF60"`,
    `"0x276CB5EE4E027E0"`), which is direct evidence that each head went
    through `If-Match`.
  * process-fault crash proxy: 3/3 phases old-or-new, 0 partial states.
  * backend suite **14 passed, 0 failed**; shared fenced-writer invariants
    **7 passed, 0 failed** (`tests/support/fencing.rs`: expired-lease stop,
    SIGSTOP reclaim then `Busy`, replaced-lock release, renewal, monotonic
    token).
  * four of the backend tests run against the live emulator; ten run against an
    in-process Azure service that independently recomputes the SharedKey
    signature, so a wrong canonicalisation or an unsigned header fails.
* **GCS auth court.** `bench/scripts/gcs_court.sh` now runs
  `cargo test --test service_account` every time and records an `auth` object in
  `gcs-court-20261106T230852Z.json` (current; `gcs-court-20261106T225115Z.json`
  is the earlier run with the same result): **14 passed, 0 failed**, status
  `no_credentials`, `live_probe: null`. The emulator court itself is unchanged
  (same content root, commit sequence, fencing 7/7). A retained failing receipt
  `gcs-court-20261106T225005Z.json` records one auth test asserting a project
  from the key file while the court environment set `WEFT_GCS_PROJECT`; explicit
  project precedence is what is asserted now.
* **Aggregate.** `bench/receipts/verify-20261106T225457Z.json` is **29/29
  courts pass**, 0 failed, 0 skipped, at WeftFS `132e713`. The standalone court
  receipts above were re-generated from committed trees after the aggregate:
  the GCS one records WeftFS `c283131` and the Azure one `d67855c`; the backend
  and auth code is unchanged from `132e713`, and the Azure workload root is
  identical across all three runs. The new Azure and GCS
  auth courts are standalone like `gcs_court`, so they are not counted in that
  29; their own receipts above are the evidence.
* Repository suites, counted from the aggregate's logs: `rust_store`
  (`cargo test --all-targets`) **101 suites / 513 passed / 0 failed**;
  `rust_fuse` (`cargo test --features fuse --all-targets`) **101 suites / 555
  passed / 0 failed / 2 pre-existing ignored**. New tests: 8 Azure unit, 14
  Azure backend (10 in-process fake + 4 live Azurite), 7 Azure fencing, 6
  service-account arithmetic/DER, 14 service-account integration.
* Gates: `scripts/check-claims.sh` and `scripts/check-spec-drift.sh` pass in the
  same aggregate. No spec changed: both additions are transport/auth surfaces,
  not wire formats, so the subproject spec mirrors are untouched.

### Losses and non-claims

* **Azure SAS acceptance is measured; anonymous acceptance is not.** R23
  (`bench/receipts/azure-20261106T235445Z.json`) proves a container-scoped
  service SAS is accepted by Azurite 3.35.0 for read-only opens, object reads,
  `verify(true)`, and blob writes. The same SAS is refused at the
  `HEAD ?restype=container` check a writable open performs, so writable opens
  need SharedKey (proven) or an account SAS (untested). Anonymous is refused on
  a private container; acceptance needs a real account with container ACL
  `blob` or `container` and remains unproven. Neither form is claimed as
  accepted on a real Azure deployment.
* **No live cloud run.** Azure has never been contacted; GCS has never received
  a token exchange. The precise gap is a credential and an account:
  * GCS needs a service-account JSON key with Storage permissions. Provide it
    with `WEFT_GCS_SA_KEY=/path/key.json bench/scripts/gcs_court.sh`; the script
    copies it to the gitignored court cache and runs
    `examples/gcs_auth_probe.rs`, which performs one real exchange and prints
    only the fingerprint, modulus width, and remaining lifetime. Until that is
    run, `no_credentials` is the whole claim.
  * Azure needs a storage account and a SharedKey or SAS credential. The same
    `--azure-endpoint`/`--azure-account`/`--azure-key` path applies; Azurite is
    not Azure, and untested differences include service-version acceptance,
    large-block-blob limits, `x-ms-*` header edge cases, and listing paging.
* **RS256 is not constant-time.** The modular exponentiation is a simple
  square-and-multiply with conditional multiplies. Signing runs locally with the
  operator's own key, but a co-located attacker could in principle learn
  information from timing. A production hardening step is a constant-time
  bignum implementation (or a vetted dependency); neither is in this tree.
* **Azurite data is a tmpfs.** The court proves protocol behaviour, signature
  acceptance, conditional-write safety, and fencing — not media durability,
  replication, zone failure, or physical power loss. `durable_fsync` for both
  backends means "the provider acknowledged the write".
* **Fencing is still two conditional writes on two objects.** The fence and the
  head compare-and-swap are not one atomic operation (not expressible on plain
  S3/Azure/GCS semantics); a lost network reply retains the usual at-least-once
  ambiguity. No multi-writer, clock-skew, or fleet claim.
* **Service-account assertions are re-signed per token**, not cached; a token is
  cached for its lifetime and refreshed in the bounded window. A clock skewed by
  more than the one-hour assertion lifetime will be refused by the provider.
* **No Go parity** for either addition: WeftFS-Go has no Azure or S3/GCS
  backend and no service-account flow. `src/azure.rs` and
  `src/service_account.rs` are Rust-only.
* The Azure backend has **no performance claim** and was not added to
  `scripts/verify-all.sh`. The GCS auth suite likewise runs inside the existing
  standalone `gcs_court`, not the aggregate.

## Subphase R22 — 3FS transports: POSIX and USRBIO (2026-11-06)

The store can now run over DeepSeek 3FS through two transports that share the
normative layout: `posix` (positional I/O through the 3FS FUSE mount) and
`usrbio` (the hf3fs user-space ring API for the immutable data plane).
**No 3FS deployment is run by this repository**, so nothing here is a 3FS
throughput, latency, RDMA, durability, or multi-node claim.

### What is implemented

**Buffer-oriented plane boundary** (`src/planes.rs`, `src/backend.rs`)

* `BlobPlane::read_into` / `StorageBackend::read_into` fill a caller-owned
  buffer. The local and 3FS backends implement it as one positional `pread`
  into that buffer — no intermediate `Vec`.
* `read_ranges(&[RangeRead]) -> Vec<Vec<u8>>` expresses a batch; output order is
  input order and each entry keeps the exact `read` contract (short tail at EOF,
  `InvalidArgument` past EOF).
* `publish_file` fills its staging file through the write ring when the
  transport is `usrbio`; `fsync` + `rename` + directory `fsync` still order the
  publication, so the commit protocol is unchanged.

**`src/threefs/`** — the only module allowed to use `unsafe`
(`src/lib.rs` is `#![deny(unsafe_code)]`, and `tests/threefs.rs` fails if
`unsafe` appears anywhere else under `src/`).

* `ffi.rs` transcribes `hf3fs_usrbio.h`: `hf3fs_iov` (312 B), `hf3fs_ior`
  (600 B), `hf3fs_cqe` (24 B), and the eleven entry points. Sizes and field
  offsets are pinned by `const` assertions here and by `_Static_assert` in the C
  conformance library, so a transcription error fails a build.
* `dlopen(RTLD_NOW|RTLD_LOCAL)`; a missing library or missing symbol is
  `UnsupportedVersion` naming every missing symbol. No fallback to POSIX.
* Registration lifetime is owned by a `Drop` guard that deregisters and closes
  on every path; one mutex serialises prep/submit against wait, which is what
  makes `Send`/`Sync` sound; destinations reach the ABI only through
  `region_contains`, so `[ptr, ptr+len)` is inside the region by construction.
* Bounds: entries ≤ 4096, one request ≤ 32 MiB, region ≤ 1 GiB.
* `Usrbio::counters()` reports submissions, requests, completions, bytes,
  direct versus staged reads, and registrations; `ThreeFsStatus` carries the
  library path and the measured `hf3fs_is_hf3fs` answer.
* `BackendKind::ThreeFs { config }`, `--backend 3fs`, `--threefs-mode`,
  `--threefs-mount`, `--threefs-lib`, `--threefs-entries`, `--threefs-io-bytes`,
  `--threefs-require-mount`. An unrelated `.so` is refused by name.
* Declared capabilities carry the literal text "Declared, not measured", and a
  test asserts that wording is present.

### Court

`scripts/threefs_court.sh` builds `bench/threefs/hf3fs_shim.c` — a conformance
implementation of the published ABI over real POSIX `pread`/`pwrite` with
layout assertions — then runs `tests/threefs.rs` **13/13** single-threaded:

* POSIX transport byte-identical to local: commits, chunks, manifests, indexes,
  generation, snapshot root, full verify, reopen;
* the shim's `hf3fs_is_hf3fs` is `false` and the adapter records it;
* 300 requests over 128-entry rings produce exactly 3 submissions, exact bytes;
* partial completion (two CQEs per wait) still yields exact bytes;
* short read at EOF, `InvalidArgument` past EOF, and an empty request;
* direct path inside the region (`direct_reads=1`, `staged_reads=0`) versus a
  staged read;
* injected `prep_io` failure propagates as `Io` with zero live registrations
  afterwards (the `Drop` guard ran);
* a full store round trip over the ring is byte-identical to local, including
  `verify --full` and a read-only reopen;
* `unsafe` is confined to `src/threefs/`;
* unbounded configuration is refused, and `require_3fs_mount` refuses the shim.

Receipt `bench/receipts/threefs-20261106T232643Z.json`, analysis
`bench/RESULTS-threefs.md`, design `docs/THREEFS.md`.

### Measured (single host, warm cache, shim)

300 × 256 KiB range reads, 5 reps: positional **9.899 ms**, ring **11.159 ms
(1.127× slower)**; 1,500 requests in **50 submissions**; bytes identical. Store
round trip 100 × 16 KiB: POSIX 11.31 ms, USRBIO 12.28 ms (+8.5 %); same
generation 101 and same snapshot root; reopen + verify 1.69 ms. Peak RSS
397,196 KiB.

The loss has three named causes, all recorded in the receipt: the shim performs
`pread` per request inside `wait_for_ios` and has no RDMA or kernel bypass; the
adapter opens and registers one descriptor per read (`registrations = 1500`); and
the positional side reuses destinations while the ring side returns owned
vectors.

### Environment fact

The court's `/tmp` is a Docker tmpfs mounted `noexec`, so `dlopen` of a shared
object there fails with *"failed to map segment from shared object"*. The court
builds the shim under `./.docker-cache/`, an executable bind mount.
`scripts/docker-court.sh` now forwards `WEFT_3FS_LIB`, `WEFT_3FS_SHIM`,
`WEFT_3FS_SHIM_DIR`, `WEFT_THREEFS_REPS`, `WEFT_THREEFS_RECEIPT`,
`WEFT_THREEFS_LOG`, and `WEFT_STAMP`.

### Not done

* No real 3FS deployment, RDMA, kernel bypass, multi-node consistency, or
  power-loss scenario.
* No registration cache: each read opens and registers a descriptor.
* `read_batch` still returns owned vectors at the store boundary; the zero-copy
  property covers storage → shared region only.
* No Go 3FS transport.
* The writer lock remains the store's single-writer `LOCK` file on the POSIX
  plane; USRBIO does not add or replace coordination.

## Subphase R23 — Azure SAS acceptance (2026-11-06)

R21 shipped the Azure SAS and anonymous credential forms with
request-construction tests only. R23 mints a real container-scoped **service
SAS** (15-field string-to-sign for `sv=2018-11-09`, HMAC-SHA256 over the account
key, base64 with `+ / =` escaped) and requires Azurite to accept or refuse it.
Code revision `dcbe066`.

### Environment and command

Azurite Blob `3.35.0`,
`mcr.microsoft.com/azure-storage/azurite@sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37`
(image id `sha256:03f7cb26ef…`), tmpfs `/data` 1 GiB, sibling container on a
private network. Test court `weft/rust-court:1`: memory/swap 3g/3g, pids 1024,
cpus 4, nproc 2048, `--cap-drop=ALL`, `no-new-privileges`, tmpfs `/tmp` 1 GiB.

```sh
bash bench/scripts/azure_court.sh
```

The inner court ran `cargo test --test azure_backend -- --test-threads=1
--nocapture`, `cargo test --test azure_fencing -- --test-threads=1
--nocapture`, then `cargo run --release --quiet --example azure_court`. The image
was obtained; the court did not skip.

### Result

* `azure_backend`: **19 passed / 0 failed** (12.93 s). New live tests:
  `live_sas_token_authenticates_and_preserves_the_store`,
  `live_read_only_sas_refuses_a_writable_open`,
  `live_a_tampered_sas_is_rejected`,
  `live_anonymous_is_refused_on_a_private_container`, and
  `live_sas_authorizes_blob_data_but_not_container_properties`.
* `azure_fencing`: **7 passed / 0 failed** (15.02 s), unchanged.
* End-to-end court: status `ok`, local and Azure content digest `2e3b9902…`,
  generation 21, `content_root_match` / `generation_match` /
  `generation_sequence_match` all `true`, peak RSS 7,488 KiB.
* A service SAS with `sp=rcwdl` **was accepted** for a read-only store open
  (FORMAT, commits, superblock, manifests, objects), an object read, a full
  `verify(true)`, and a create-once blob PUT + GET.
* The same SAS **was refused** on a writable open at
  `HEAD ?restype=container` (Get Container Properties): class `Unauthorized`,
  `Azure denied reading container \`…\``. Azurite returns 403 for that call
  under `sp=r`, `sp=rcwdl`, and `sp=racwdl` alike, while blob data operations
  are accepted when `w` is present. A writable open therefore needs SharedKey
  (proven) or an account SAS (not tested). This is recorded as a limitation;
  the container-properties check was not skipped to make SAS writable.
* `sp=r` is refused earlier on a writable open, at the conditional-write probe:
  `Unauthorized: Azure denied PUT \`tmp/conditional-probe-…\``.
* A signature tampered from `sp=r` to `sp=rcwdl` is refused at the FORMAT read
  (Azurite `AuthorizationFailure`) as `Unauthorized`.
* Anonymous is **refused** at the FORMAT read of a private container
  (`AuthorizationFailure`, `Unauthorized`). Acceptance remains unproven.

### Defect found and fixed

`Store::open_impl` wrapped every non-`Io` FORMAT-read failure as `Corrupt`, so a
rejected credential was reported as damaged data (`Corrupt: cannot read FORMAT:
Unauthorized: …`). The tampered-SAS test exposed it. `src/store.rs` now preserves
`Class::Unauthorized` as well as `Class::Io`; a genuinely missing or mismatched
FORMAT remains `Corrupt`. The failing run that found it
(`azure-20261106T234510Z`, `15 passed; 2 failed`) is retained.

### Receipts

`bench/receipts/azure-20261106T235445Z.json` (sha256
`f4e29469…d9f551`) and `bench/receipts/azure-court-20261106T235445Z.json` (sha256
`13809ef7…686a8d`), with `.tests.log`, `.tests.txt`, `.fencing.log`, and
`.fencing.txt`. Analysis: `bench/RESULTS-azure-sas.md`. The retained failing
run is `azure-20261106T234510Z.*`.

### Losses and non-claims

* No live Azure account: Azurite is not Azure. Service-version acceptance,
  block-blob limits, `x-ms-*` edge cases, and listing paging are untested.
* No account-SAS acceptance test.
* Anonymous acceptance is unproven; only refusal on a private container is
  proved. Proving it needs a real account with container ACL `blob` or
  `container`.
* Writable SAS opens are refused by design of the endpoint's container check,
  so SAS is effectively read-only on this backend.
* One host, loopback, tmpfs, warm caches; no performance, power-loss,
  multi-node, or fleet claim.

## Subphase R24 — Go calibrate measurement and packed/split mount coverage (2026-11-06)

Two named Go gaps are closed. Work is in WeftFS-Go `staging` `af8ad88`; the
parent pins that revision.

**Go `weftfs calibrate` (measurement).** `store/calibrate.go` measures the
planner classes with the shipped Go encoders/decoders (raw, zstd, modes 9, 10,
15, 11, 14) at two sizes with medians, a warm-up, and incremental delta rows
(base decode subtracted), then fits the same affine
`fixed_ns + decoded_bytes*ns_per_mib/2^20` model and rounding as Rust.
`sparse-delta` reuses the weight-delta row; `kv-cache` and `activation` stay
declared. The command is `weftfs calibrate [--out|--from|--reps|--large-mib|
--small-kib]`, with `--json` output shaped like Rust.

Rust accepts the Go document: `weftfs --json calibrate --from
go-calibration.json` exits 0 with schema `weft.calibration.v1`, 10 rows
(7 measured) and calibration id
`b07fbff6f59d6bbd284f15520673ac5d51635a7a2d86d1bd230c9f0b56a9bec7`, identical
in both languages. Automated as `TestRustAcceptsGoCalibrationDocument`, which
ran rather than skipped.

**Real defect found and fixed.** Go's `q8BlockSize` preferred the *smallest*
dividing mode-10 block (16) while Rust prefers the largest (256). At 8 MiB any
`q8_0` write failed `LimitExceeded: WCB item count 1000001 exceeds limit
1000000`, and mode-10 bytes diverged from Rust. The order is now
`256,128,64,32,16`, matching Rust's `q8_0_block_size`.

Measured, host-local and not portable (default 7 reps, 128 KiB / 16 MiB, go
court image, kernel 7.3.1, 4 CPUs, wall 4.422 s), `ns_per_mib` under the Go
codec-level method: raw 819,494; zstd 1,115,176; float-tensor 2,837,430;
tensor-sharded 3,475,654; tensor-tiled 4,828,817; weight-delta 6,003,877;
semantic-delta 7,206,540. Go measures codec frames while Rust measures store
reads, so the divergence from the baked Rust table is method and machine, not a
speed claim. Two minimum-size runs drifted ≤11.1 % on slope and ≤57,648 ns on
intercept.

**Packed and split mount coverage.** `fusefs/packed_mount_test.go` builds three
layouts with the Go CLI — `--pack`, `--blob-backend object --commit-backend
local`, and `--pack` over that split — creates 9 files (12 B…300,000 B) and 3
directories in each, asserts `info --json` reports the expected pack and plane
state, mounts a real read-only daemon, compares every file byte-for-byte and by
SHA-256, performs an 8 KiB `ReadAt` at offset 100,000, checks listing digests
(identical across layouts), unmounts, and runs `verify --full` three times.

Court: `scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race
-count=1 ./..."` → **0 failures**; top-level tests store 173, `cmd/weftfs` 17,
`fusefs` 25 (was 168/15/24). `gofmt`, `go vet`, and a CGO-disabled build are
clean.

**Not done:** no Go `calibrate --install`; the calibration exercises the codec,
not a store-side mode-15/14 read path (Go has no writer for those); the FUSE
coverage mounts read-only; no power-loss, multi-host, or fleet claim; a
calibration is host-local by construction.

## Subphase R25 — Go S3 backend (2026-11-06)

The largest remaining dual-implementation gap is closed: WeftFS-Go has a real
S3 backend with SigV4, conditional publication, and the fenced writer. Work is
in WeftFS-Go `staging` `c421590` (tested code `3e14f26`); the parent pins it.

**What is implemented.**

* `store/s3client.go` — SigV4 over `net/http`, standard library only. Canonical
  path/query, every body hashed, no `UNSIGNED-PAYLOAD` path. Pinned by the AWS
  published "GET Object" vector plus a payload-digest test. Config precedence is
  `--s3-*` flags, then `WEFT_S3_*`, then `AWS_*`.
* `store/s3blob.go` — create-once `PUT` with `If-None-Match: *` (identical bytes
  idempotent, different bytes `Exists`), whole and ranged `GET` (an ignored
  `Range` is sliced locally; `416` is EOF), `HEAD` length, `DELETE`, and
  `ListObjectsV2` with continuation tokens, under the same 1 GiB object bound.
* `store/s3commit.go` — control records create-once or `If-Match` CAS, and the
  8192-byte two-slot superblock via read-modify-write + `If-Match` with bounded
  retry. The writable-open probe requires a matching conditional write to
  succeed **and** a stale one to be refused with the winner's bytes intact; an
  endpoint that ignores either form is `UnsupportedVersion`.
* `store/s3fence` (fenced writer ported verbatim): `weftfs-s3-lock v2` with
  owner, monotonic fence, sequence, absolute deadline, renewal stamp, and the
  pending head. Fence+1 takeover, a conditional `LOCK` rewrite as the last gate
  before the head CAS, renewal at one publication budget, refusal after the
  writer's own deadline, and a content-addressed tombstone release under
  `If-Match` — never an unconditional delete.
* `store/control.go` — `--backend s3`, `--blob-backend s3`, `--commit-backend
  s3`, and all four split layouts. Snapshots, GC, verify, quota, access
  counters, checkpoints, and packs are unchanged.

**Measured If-Match form.** MinIO accepts the quoted ETag as returned
(`S3 If-Match form measured at open: quoted-as-returned`, also in the receipt as
`measured_if_match_form`). The dequoted form (Ceph RGW's literal comparison,
found in R16) is implemented but **unexercised** in the Go court.

**A real defect the full suite caught.** Control-record replacement staged under
`tmp/tmp` (and would have created `access/tmp`); fixed in `6fc87cc`, failing log
retained.

**Courts.** `scripts/go-s3-court.sh` (and `--focused-only`) start the official
MinIO release `RELEASE.2025-04-22T22-12-26Z` (binary sha256 `53e2a2cb…bb817c`)
as a sibling container under the usual hard limits, with a tmpfs data directory
and a private bridge, path-style over `http://<container>:9000`.

* Focused (go court): `gofmt -l`, `go vet`, `CGO_ENABLED=0 go build` clean;
  **17 pass / 0 fail / 1 skip** (the skip is a crash-child helper).
* Full `go test -race -count=1 ./...` (fuse court): all four packages ok,
  **0 failures** — `cmd/weftfs` 24.8 s, `fusefs` 7.5 s, `model` 1.0 s, `store`
  116.9 s.
* Cross-language `scripts/go-s3-xlang.sh`: **17/17** checks. Go init + files +
  snapshot → Rust `verify --full`, same head, byte-exact reads including a
  70,000-byte file; Rust init + files → Go `verify --full`, same head,
  byte-exact; then Go unlinks and runs `gc --apply` on the Rust-written store
  and both implementations agree the file is gone.

Receipts live in `WeftFS-Go/bench/receipts/go-s3-20261107T005722Z.json` and the
focused/full/xlang logs beside it; `WeftFS-Go/NOTES.md` §R25 records commands,
counts, host-local request counters (writer 110 requests / 30 writes / 101,104 B;
reader 68 requests), and every limitation.

**Not done.** No Go GCS or Azure backend; no FUSE-over-S3 mount court (the path
exists through `store`, but it is not evidenced); no AWS, multi-node,
replication, media, or power-loss claim; MinIO is loopback on tmpfs; the fence is
still two conditional writes on two objects, not one atomic operation; the Ceph
RGW dequoted form and the pack-plane `ReclaimUnreferencedPacks`/`VerifyPacks`
are unexercised on S3; no performance claim.

## Subphase R26 — Go selector-index persistence (WFX2) (2026-11-06)

Go now persists its selector index in the same `WFX2` segment format Rust
writes, so a reopen no longer replays and re-indexes the whole state. Work is in
WeftFS-Go `staging` `39b58f2`; the parent pins it.

**Implemented.** `index/` ports Rust's `StateIndex` and the WFX2 codec
(presence/equality attribute maps, `(subject,predicate)` relation edges,
`tagged_as` tags; equality keys are the canonical WCB `["weft.eq.<tag>", v]`, and
numeric families share `["weft.eq.num","<decimal>"]`). `resolver/indexed.go`
ports `candidates_for` and keeps `evalExpr` authoritative for every candidate.
`store/index.go` writes a segment after **all four** publish sites (`init`,
`commitOps`, `publishGroupOps`, `publishQuotaUsageOp`); a write failure is a
note and never fails the commit. Load validates the commit key, generation, all
three counts, state shape, digest and canonical ordering, and rejects with a
note + rebuild otherwise. Only complete indexes are written. Read-only opens
never write. `IndexStats{ DiskLoads, Builds, Rejects, Writes, WriteErrors,
GCDeleted, Notes }` is the observable surface. GC deletes segments not named by
the head, a snapshot, or a checkpoint (grace honoured) and reports them in
`GCReport.DeletedIndexes`; `verify` never reads segments.

**Byte layout, measured on a real 10,000-object segment.** 2,423,650 bytes —
the same length as Rust's own 10k WFX2 file. Header is 72 B
(`WFX2 | u32 version | commit[32] | u64 generation | u64 objects | u64
relations | u64 entries`), then sorted fixed-width sections, then
`BLAKE3("weft/index/v2", preceding bytes)`. Consistency of length is not used
as an equality proof; the cross-language tests below are.

**Cross-language, real binaries in the fuse court.**

* Rust loads a Go-written segment: Go writes; Rust `query` agrees. Then the Go
  segment's `tags` section (and separately `attr_eq`) is emptied and re-sealed
  with a valid digest: Rust answers **0 matches** where a rebuild would have
  returned matches — proof that Rust decoded and trusted the Go bytes. Bytes
  restored, Rust answers again.
* Go loads a Rust-written segment: Rust writes init/creates with string and u64
  attributes, `tagged_as`, `links`, and a snapshot; Go opens read-only and
  answers five query shapes with `DiskLoads=1, Builds=0, Rejects=0`, equal to the
  scan oracle for two principals. `DecodeFile`→`EncodeFile` of the Rust file is
  byte-identical.

**Tests.** `go test -race -count=1 ./...` in the fuse court: all packages ok
(`store` 112.4 s). Top-level test functions: store 197, fusefs 25, index 4,
model 6, `cmd/weftfs` 18. Nine corruption modes each assert `Rejects≥1`,
`DiskLoads=0`, `Builds≥1`, a note naming the index, unchanged answers versus the
scan oracle, and a clean verify: bad magic, wrong version, wrong commit, wrong
generation, wrong object count, bad digest, truncated, trailing bytes, and WFX1
magic.

**Measured, host-local, synthetic, warm cache, `/tmp` tmpfs, 5 samples, two
independent runs.** With the segment present, the first relation query after
reopen is **7.749 / 8.042 ms** versus **16.838 / 16.852 ms** rebuilt
(ratio 0.460 / 0.477); the first tag query is **16.583 / 16.884 ms** versus
**24.841 / 23.851 ms** (ratio 0.668 / 0.708). Cold build 8.5–10.4 ms; write
including build 25.4–25.7 ms for 2,423,650 B; peak RSS 419–423 MiB.

**Published losses.** The index is rewritten per commit, so its cost is O(state)
in time and bytes (~17 ms on top of the build at 10k objects here); absolute
times are not comparable to Rust's different fixture; queries that cannot narrow
(`select true`, `score > 5`) still pay the load for nothing and are **not
measured**; packing makes pack layout depend on commit timestamps, so
`TestPackedLayoutIsDeterministic` now pins its clock; `RequireOne` and
saved-context resolution still scan; there is no cross-language proof for
I64/Timestamp attribute keys (the Rust CLI cannot author them); the default GC
grace is 3600 s; and there is no CLI switch to disable segment writes.

**Not done.** No index in `RequireOne`/saved-context paths; the in-memory index
cache is not persisted; single host, synthetic fixture, warm cache — no
device, power-loss, or fleet claim.

## Subphase R27 — one conditional write for the fenced head (2026-11-07)

**Status: implemented and tested on MinIO, Ceph RGW, fake-GCS, Azurite, and Go
MinIO.** No wire format, codec mode, manifest, commit record, or superblock
layout changed.

### The defect, as found in source

R17/R18 wrote the fence as one conditional `LOCK` rewrite and the head as a
*second* conditional write on the `superblock`. An external review identified the
interleaving no existing test covered: A's fence succeeds, A is suspended before
the head CAS, A's lease expires, B takes over the fence without publishing a new
head, A resumes, and A's head CAS still succeeds because the head is the value A
read. The `SIGSTOP` tests only stopped a writer *before* the fence.

### What changed

* The lock record is now `weftfs-s3-lock v3` / `weftfs-gcs-lock v3` /
  `weftfs-azure-lock v3`: `owner`, `fence`, `sequence`, `lease_deadline_ns`,
  `renewed_ns`, `pending_generation`, `pending_commit`, **`pending_commit_len`**.
* **The head is published by writing that record**, with `If-Match` (S3/Azure) or
  `ifGenerationMatch` (GCS) on the bytes this handle last published. One
  conditional write carries both the fence and the new head. A takeover rewrites
  the same object, so the stale write is `412 -> Busy`.
* The two-slot superblock is an **attested cache** on an object-store commit
  plane: written after the record, never moved backwards by a cache refresh,
  merged as the authoritative head by recovery, and repaired on a writable open.
  A cache *ahead* of the record is refused `UnsupportedVersion` (mixed-version
  store). Local/POSIX backends keep the superblock as the publication point:
  `flock` cannot be taken by a suspended process.
* Every non-publication record write (pre-commit fence, renewal, release
  tombstone) carries the current head forward. Tombstones are
  `weftfs-<backend>-lock-released v2` with the head; v1 tombstones stay readable.
* v2 records stay readable (`pending_commit_len` reported as 0 = unknown); v1
  reads as fence 0. A writable open upgrades an expired/released v2/v1 record to
  v3 in the same takeover conditional write; a live v2 record still refuses the
  open `Busy`.
* A local own-deadline re-check runs between the fence and the commit record, so
  a writer suspended past its lease leaves no unpublished commit tail.
* Implementation: Rust `src/writer_fence.rs`, `src/s3.rs`, `src/gcs.rs`,
  `src/azure.rs`, `src/backend.rs`, `src/planes.rs`, `src/recovery.rs`,
  `src/store.rs`, `src/fence_pause.rs`; Go `store/s3client.go`,
  `store/s3commit.go`, `store/control.go`, `store/recovery.go`, `store/store.go`,
  `store/fence_pause.go`.

### The acceptance test

`tests/support/fencing.rs::paused_after_the_fence_cannot_publish_after_a_takeover`
(shared by the S3, GCS, and Azure suites) and
`WeftFS-Go/store/s3_integration_test.go::TestS3FencedWriterPausedAfterFenceCannotPublish`.

1. A child opens and fences, then suspends itself at the hook
   `WEFT_FENCE_PAUSE_AFTER_FENCE=1` (it first writes
   `WEFT_FENCE_AFTER_FENCE_READY` with pid, generation, and head). Rust uses
   `raise(SIGSTOP)`; Go blocks on a named FIFO because its runtime does not
   deliver a self-sent `SIGSTOP` as a group stop in this court. The hook is inert
   unless armed and fires at most once per process.
2. The parent lets the lease expire and writer B takes over the fence **without
   publishing a head**.
3. The parent resumes A (`SIGCONT` in Rust, one byte on the FIFO in Go). A's
   publication is `Busy`; the error text names the takeover
   (`the LOCK now names owner … fence …`) or the taken-over CAS; generation and
   head are unchanged.
4. B then publishes and succeeds; `verify --full` is clean and A's file is not
   visible.
5. The inverse sanity test (suspended *before* the fence) is retained and still
   passes.

`tests/s3_fencing.rs::a_replaced_lock_record_makes_the_stale_publication_fail_412`
forces the same `412` deterministically while the stale writer's own lease is
still valid, isolating the conditional-write refusal from the own-deadline guard.

### Evidence (this host, Docker courts, all limits unchanged)

* MinIO (`scripts/s3_court.sh`): `s3_backend` 6/6, `s3_fencing` **11/11**
  including both new tests; receipt `bench/receipts/s3-court-20261107T025420Z.json`.
* Ceph RGW 19.2.0 (`scripts/ceph_court.sh`): `s3_backend` 6/6, `s3_fencing`
  **11/11**; receipt `bench/receipts/ceph-rgw-20261107T025545Z.json`.
* `fake-gcs-server` v1.56.1 (`bench/scripts/gcs_court.sh`): `gcs_fencing`
  **9/9**, including the new test; receipt
  `bench/receipts/gcs-20261107T034113Z.json`.
* Azurite 3.35.0 (`bench/scripts/azure_court.sh`): `azure_fencing` **9/9**,
  including the new test; receipt
  `bench/receipts/azure-20261107T034219Z.json`.
* Go MinIO (`WeftFS-Go/scripts/go-s3-court.sh --focused-only`): **20 pass /
  0 fail / 1 skip** (the crash child helper), including
  `TestS3FencedWriterPausedAfterFenceCannotPublish`; receipt
  `WeftFS-Go/bench/receipts/go-s3-20261107T032512Z.json`.
* Rust `cargo test --all-targets`: **540 passed / 0 failed / 0 ignored**, 103
  suites. FUSE `cargo test --features fuse --all-targets`: **582 passed /
  0 failed / 2 ignored** (the pre-existing `CAP_SETUID` and kernel-7.3 direct-I/O
  tests).
* Go `go test -race -count=1 ./...` in the FUSE court: all packages ok;
  `go vet ./...` clean; `CGO_ENABLED=0 go build ./...` clean.
* The crate-level unsafe guard was extended with one explicit, narrow exception:
  `src/fence_pause.rs` may contain exactly one `unsafe` line
  (`libc::raise(SIGSTOP)`), and the guard fails if a second appears.

### Migration

A store last written by R17/R18 has a v2 `LOCK` record. The next writable open
takes the lease over (the record must not be live) and writes a v3 record in that
one conditional write, filling `pending_commit_len` from the record (0 = unknown
until the next publication). If a v2 writer publishes after that upgrade, the
superblock cache can be ahead of the record; the store then refuses to open with
`UnsupportedVersion` naming both generations rather than silently choosing one.
Upgrade every writer before opening the store again.

### Residual

* Still one writer; not a multi-writer or fleet protocol.
* No clock-skew defence; deadlines are the writer's wall clock.
* A lost network reply to the publication write is the usual at-least-once
  ambiguity (the next write fails `Busy` and the handle reopens).
* The cache write is a second object write, but it follows the publication and
  can never move the cache backwards or be trusted over the record.
* `WeftFS/AGENTS.md` is gitignored by operator directive and is not committed;
  this section records the protocol change that the program memory also notes.
* No physical media power loss; the courts remain process-fault and single-node
  device-model proxies.

## Subphase S1 — decode floor, wide-word kernels, mode 17, access-pattern planner (2026-11-07)

This subphase answers the reviewer's decode-throughput requirement with measured
numbers, including every loss. Receipts:
`decode-floor-20261107T034727Z.json` (baseline),
`decode-floor-20261107T040250Z.json` (kernels),
`float-tiled-20261107T042955Z.json` (real Qwen),
`access-pattern-20261107T044101Z.json` (planner),
`restore-pipeline-20261107T045603Z.json` (988 MB). Analysis:
`bench/RESULTS-decode-simd.md`, `bench/RESULTS-mode17.md`.

### Measured decode floor and kernels

* Baseline, single-threaded, 16 MiB BF16: Huffman 15.4646 ms (1034.6 MiB/s of
  exponent plane), plane join 18.0656 ms, whole mode-9 decode 34.7865 ms
  (459.9 MiB/s). FP32 mode 9: 21.4125 ms (747.2 MiB/s).
* Rust Huffman: an AVX2 eight-byte-refill, two-symbol-unroll kernel is
  **1.07–1.10x** the retained scalar kernel in the same process. Cross-run
  absolute change is +1.0% (1 MiB) to +3.7% (16 MiB), i.e. **no absolute win**;
  a single Huffman bitstream cannot be lane-parallelised without a wire change.
* Rust BF16 plane join: the byte-wise formulation is **11.2x** faster
  end-to-end at 16 MiB (18.0656 → 1.6166 ms) because LLVM auto-vectorises it.
  The explicit AVX2 kernel is a tie at ≥1 MiB (1.01–1.04x) and 3.8x slower at
  128 KiB, so it is not selected.
* Whole BF16 mode-9 decode: 34.7865 → 18.2186 ms (**1.91x**); 1 MiB 2.2621 →
  1.2181 ms (1.86x). FP32 is unchanged; its +2–8% cross-run deltas are noise.
* `unsafe` is confined to `WeftCodec/src/simd.rs`; the crate root is
  `deny(unsafe_code)` and `tests/no_unsafe_outside_simd.rs` fails otherwise.
  10,000-case differentials per kernel (Huffman streams including mutations,
  BF16 plane pairs) pass.
* Go: the word-level Huffman variant is **0.93x** (274.8 vs 294.0 MB/s) and is
  not selected; the BF16 direct pack is **1.45x** (2044 vs 1413 MB/s). Go can
  never match the Rust join because it has no SIMD intrinsics.

### Mode 17 `FLOAT_TILED`

* Wire: mode 17, schema `weft.ai.float_tiled.v1`; per-tile mode-9 plane split;
  `tile_root = H("weft/tensor/float_tile/v1", WCB({dtype, dims, element_len,
  exponent_plane_len, mantissa_plane_len, values_hash}))`; full decode verifies
  every tile root and the logical root; region reads return `tiles_touched`.
  Modes 0–16 are byte-identical and the reserved reject is now 18.
* Conformance: **3 valid + 8 reject** vectors in both codec repos; manifest 338
  entries, coverage gate `17:3/8`. Cross-language gate:
  **YES byte-identical float_tiled_bf16** and **float_tiled_fp32**, both
  directions decode exactly.
* Store: `TensorStrategy::FloatTiled`, planner opt-in tile shape, write/read/
  region paths, `checkpoint write --shard-strategy float-tiled`, full verify.
  Go store: same planner option, writer, and reader (whole-object region
  decode only); the same fixture selects the same strategies.
* Real Qwen BF16 embedding `[151936,896]`, tile `[64,896]`: mode 9
  **180,623,816 B** vs mode 17 **181,541,557 B (+0.51%)**. Write 2,287 vs
  7,435 ms (3.25x slower). Whole read 745.7 → 486.6 ms at one thread and
  740.2 → 235.9 ms at four. 100-row read 777.1 → 2.30 ms (337x); 1-row read
  749.3 → 2.12 ms (353x). Every read byte-exact.
* No whole-model mode-17 checkpoint: one tile shape cannot divide every tensor
  rank/extent in the model. Mode-17 restore evidence is the embedding plus the
  checkpoint test, not a 290-tensor rewrite.

### Access-pattern planner

* `TensorPlanOptions.access_pattern` (`AccessPattern::Whole` default, `Region {
  region_bytes, reads_per_write }`, `Mixed { ... }`), `weft plan
  --access-pattern`, and the same integer objective
  `physical_bytes + lambda_ns * decode_at_pattern`. Tile-independent modes are
  charged only for fetched region bytes per read; every other mode is charged a
  whole-tensor decode per read. Defaults are unchanged and a test proves the
  default and explicit `Whole` plans are identical (chosen, bytes, ns, cost,
  every candidate).
* Real embedding: `whole` at lambda 0 → `float-tensor` (180,623,816 B);
  `region:1792:1000` and `region:179200:1000` → `float-tiled`;
  `mixed:0:1792:1000` → `float-tiled`; `whole` at lambda 1 → `raw` (the model
  prices no-decode highest under latency-only weighting); a whole-size region
  read ×100 also selects `raw`.
* Loss: the objective has no write term. `region:1792:1` selects `float-tiled`
  by estimate, but measured end-to-end it is 7,437 ms (7,435 write + 2.12 read)
  versus mode 9 3,036 ms (2,287 + 749). Break-even is about seven 1-row reads.
* Go parity: the same 512x512 weight-like fixture selects `float-tensor` for
  Whole and `float-tiled` for Region in both languages.

### Restore pipeline

* 988,097,824 B / 290 tensors, tmpfs, one repetition: batched read 2,822.4 /
  1,837.0 / 1,420.8 ms and pipelined total 2,764.5 / 1,678.8 / 1,317.9 ms at
  1/2/4 workers. Pipeline decode wall is 95.6% of total at one worker and 99.9%
  at four: `total ≈ decode` still holds and decode fell 2.0x. Disk-destination
  runs are writeback-bound (10–14 s at 2/4 workers) and are not the basis of the
  claim.
* Mode 9 has no sound intra-frame parallelism (one sequential Huffman stream),
  so none was added. Mode-17 tiles use the bounded pool and do not parallelise a
  1–2-tile region.

### Not done here

* Whole-model mode-17 checkpoint/restore; Go range fetch that reads only tile
  bytes; write-cost term in the planner; per-tile rounding in the region
  estimate; cold cache, GPU, or fleet claims.

## Subphase S3 — persistent checkpoint service, memfd restore, DCP/Orbax re-measure (2026-11-07)

Closes the external review item: "the Orbax adapter stages a safetensors file,
launches the Rust CLI, and restores through a path that imports reconstructed
files into the store." Save and load now use one long-lived
`weftfs checkpoint-service` process, payloads cross as `SCM_RIGHTS` memfds, and
a restore cannot write to the store at all.

Receipts: `bench/receipts/checkpoint-service-20261107T063519Z.json` (11/11),
`bench/receipts/orbax-20261107T063412Z.json` (13/13),
`bench/receipts/pytorch-dcp-20261107T063540Z.json`. Analysis:
`bench/RESULTS-checkpoint-service.md`. Protocol:
`docs/CHECKPOINT-SERVICE.md`.

### Protocol and bounds

Frame `u32 length | u8 opcode | u32 request_id | u8 status | body` over an
`AF_UNIX` stream; responses echo the id; errors carry the normative `Class`
string. Opcodes: `INFO`, `LIST_CHECKPOINTS`, `CHECKPOINT_MANIFEST`,
`RESTORE_TENSOR`, `WRITE_CHECKPOINT`, `VERIFY_CHECKPOINT`, `REFRESH`,
`WRITE_FILE`, `STAT_FILE`, `READ_FILE`, `SHUTDOWN`. One request in flight.
Bounds: 1 MiB per control frame, 4 GiB per tensor or staged container, 16 MiB
per metadata file, one descriptor per frame, 8 cached manifests. A framing
violation or unknown opcode answers `Malformed` and exits non-zero rather than
resynchronising.

The store is opened read-only once. A write opens a read-write handle for that
operation, publishes, and reopens the read handle. `REFRESH` is the only way a
pinned read handle sees another process's commits; the adapters call it before
a read.

### Service court (32 x 1 MiB f32, 4-way zstd)

| operation | service | CLI per call | ratio |
| --- | ---: | ---: | ---: |
| restore one tensor (median of 20) | **2.550 ms** | 10.386 ms | 4.07x |
| restore all 32 (median of 3) | **79.465 ms** | 96.363 ms | 1.21x |
| write checkpoint | **1,206.718 ms** | 1,286.272 ms | 1.07x |
| manifest | 0.467 ms | – | – |

230 requests / 216 restores / 226,492,416 B in one process. After 100 restores
the store was identical: **34,027,955 B, 270 files, generation 2**.

### Orbax (same fixture as R15)

11 leaves, 8-way sharded, 58,765,352 logical bytes, `zstd`.

| metric | Orbax OCDBT | WeftFS S3 | CLI control (same revision) | R15 before |
| --- | ---: | ---: | ---: | ---: |
| save total | **77.958 ms** | 1,859.152 ms | 2,603.080 ms | 1,770.713 ms |
| full restore | **22.927 ms** | 185.788 ms | 205.633 ms | 2,690.338 ms |
| single leaf | **5.327 ms** | 21.948 ms | 27.049 ms | 380.432 ms |
| physical bytes | 50,643,641 | 50,866,872 (+0.44 %) | – | 50,866,816 |
| store growth from restores | – | **0 B** | – | +67,615,378 B |

Save is 1.40x faster than the CLI path it replaces, full restore 1.11x, single
leaf 1.23x. Requested 2-device restore sharding is honoured (Orbax 0.12.9
returns the saved 8-device sharding). 11/11 leaves are byte-identical, and
`checkpoint verify` + `verify --full` pass after save and after all restores.

Remaining deficits: save is still **23.85x** Orbax and restore **8.10x**; the
Rust checkpoint write (1,808 ms of the 1,859 ms save) and 11 per-leaf decodes
dominate, not the boundary.

### DCP (8 x 4 MiB f32, topology 8)

| metric | Q5 before | S3 | CLI control (same revision) |
| --- | ---: | ---: | ---: |
| full save (median of 3) | 1,223.642 ms | 1,498.649 ms | 1,521.614 ms |
| full load (median of 3) | 152.208 ms | **101.006 ms** | 106.475 ms |
| single-tensor load (median of 5) | 67.171 ms | **24.135 ms** | 22.362 ms |
| async submit | 0.720 ms | 1.549 ms | – |
| physical bytes / `torch.save` | 0.9294 | 0.9294 | – |
| store growth from restores | 0 B | 0 B | – |

Save is 1.5 % faster than the CLI control and 22.5 % slower than the Q5 record
(the difference is the Rust write, not the boundary). Single-tensor load is
7.8 % slower than the control because the control omits `dcp.load` metadata and
framework work; per leaf the service is 4x faster. Both adapters report
**0 CLI subprocess calls** in save and load (`store_command_calls: 0`).

### No PyO3

`pyo3` is absent from `Cargo.lock` (0 matches) and from the court's cargo
registry cache and extracted sources (0 matches each), so an in-process
extension module would need a network dependency. No crate was added. The
persistent service is the delivered in-process boundary; that limitation is
recorded here and in `docs/CHECKPOINT-SERVICE.md` §1.

### Fixes found on the way

* A pre-existing region-restore bug: the region path byte-scaled the last axis
  for every shard strategy, but only the mode-10 `q8_0` transport is
  byte-addressed. A region read of a `zstd`-sharded checkpoint failed
  `NonCanonical` ("region axis 2 end 32 exceeds shape extent 8"). Fixed in
  `src/checkpoint.rs`; `test_service::test_region_restore_writes_only_the_region`
  covers it.
* `WeftStorageReader(import_restored=True)` fails closed: the service has no
  container to import.

### Tests

Rust `cargo test --all-targets`: **106 test binaries, 0 failed** (includes the
new `tests/checkpoint_service.rs` 7/7 and the second `unsafe` island guard in
`tests/threefs.rs`). FUSE all-targets is reported with the release verification.

Python court receipt `bench/receipts/python-bindings-20261107T064323Z.json`
(built wheel, not the source tree): **369 tests, 0 failures, 10 skipped**
(`tests/test_service.py` 8, `tests/test_dcp.py` 10, `tests/test_orbax.py` 18).
The first S3 Python court run failed 12 checks: the independent Python reader
did not implement mode 17, so every `float-tiled` vector failed
(`bench/receipts/python-bindings-20261107T063910Z.json`, retained). Mode 17 is
now implemented in `python/weftcodec/ai_modes.py` (per-tile Huffman exponent
plane + mantissa plane, tile roots, logical root, all reject classes) and those
vectors pass.

Intermediate runs of the bench harnesses before the final control columns were
added are not retained; the final receipts above supersede them and no claim
depends on the removed files.

### Not done

No Go client, no multi-client server, no parallel decode inside the service, no
watch (only `REFRESH`), no TLS/TCP, and no claim that a caller buffer is grown
for it. No GPU-direct path: DCP still copies into the planner's tensor because
that is DCP's own contract; Orbax feeds `device_put` from the mapping without an
extra host copy. Single host, warm cache, synthetic fixtures.
