# Implementation status — 2026-11-05

This is the honest, evidence-backed state of the program. It distinguishes
implemented, tested, ignored, and not done. The current verification receipt is
`bench/receipts/verify-20261105T035513Z.json` (25/25 courts `pass`, including the
host-only `claims` documentation gate, spec drift, generated-type drift,
quantized cross-decode, context and authenticated-capability cross-language
gates, Buck2 CAS, agent memory, and `fuzz_diff`), at WeftFS `091d0bb`,
WeftCodec `272c2c0`, WeftCodec-Go `b7400ec`, WeftFS-Go `963dc62`. The earlier
`verify-20261105T034941Z.json` is retained because one run of the pre-existing
`tests/representation.rs` shared atomic decode counter raced under parallel
test execution (`expected exactly one decoded chunk, got 2`); it passed
standalone and in the 035513Z rerun, which is the current receipt. Preceding
receipts `verify-20261105T030019Z.json` and `verify-20261104T100531Z.json` are
retained as history. Earlier
`verify-20261104T022254Z`,
`verify-20261103T220311Z`,
`verify-20261103T172416Z`, and `verify-20261103T165944Z` receipts are retained
as history. The
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
`bench/receipts/buck2-cas-20261104T022511Z.json` (15/15 prototype assertions)
and `bench/receipts/agent-memory-20261104T022512Z.json` (7/7 simulated-timestamp
months-later replay). D2 authenticated capability grants are implemented and
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
| `WeftFS` | `091d0bb` | program home, Rust store + FUSE, specs, ADRs, courts |
| `WeftCodec` | `272c2c0` | Rust codec v1 + authenticated capability envelopes |
| `WeftCodec-Go` | `b7400ec` | independent Go codec, envelope parity |
| `WeftFS-Go` | `963dc62` | independent Go store + FUSE, spec mirror |

These are the revisions pinned by the latest full verification receipt
(`bench/receipts/verify-20261105T035513Z.json`, 25/25 courts pass).
The current release is **v0.6.0** (`docs/RELEASE-v0.6.0.md`); the tag, the
per-repo GitHub releases, the four GHCR images, and the two crates are cut
from the `staging` and `main` commits after the version bump and release
documentation, with this receipt's code revisions unchanged apart from version
literals and docs (both branches are kept identical and pushed in all four
repositories).

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

### F8 — Hooks: Buck2 CAS prototype, agent memory, generated types (implemented)

- **D1 Buck2 adapter (prototype, not a plugin):** `adapters/buck2/` implements a
  local CAS plus Buck2-compatible `sha256:<hex>` blob layout. The action key is
  a real `weft.context-root.v1` record whose pinned snapshot is the BLAKE3
  digest of the typed action context (action type + toolchain + sorted inputs).
  `store`, `fetch`, `materialize`, `query`, and `verify` exist; the same key
  returns identical bytes, different toolchain/input misses, query by action
  type/toolchain/input works, and same-key/different-bytes fails closed. Go
  tests + `go vet` and a 15/15 assertion demo run in the Go court; receipt
  `bench/receipts/buck2-cas-20261104T022511Z.json`. No REAPI gRPC, no Buck2
  plugin ABI, no performance claim.
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
- Atomic publication with fsync ordering; single-writer `flock`; generation
  fallback; recovery; full verify; mark-and-sweep GC; checkpoints; selector
  evaluation with POSIX + `weft.acl.read` authorization; ambiguity errors.
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
13. **The Buck2 adapter is a prototype.** It is a local CAS service plus a
    Buck2-compatible key/digest mapping, not a Buck2 plugin, worker, or REAPI
    gRPC backend. It does not execute actions, fetch remotely, or integrate with
    Buck2's scheduler.
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
10. **Buck2 plugin/REAPI integration.** Prerequisite: the local CAS prototype
    (`bench/receipts/buck2-cas-20261104T022511Z.json`, 15/15), which is not a
    plugin, worker, or REAPI gRPC backend.
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
