# WeftFS normative index (RFC-shaped)

**Status:** index of record, v1.4.0 (2026-11-12). This file allocates stable
identifiers and states the change process. It does not add a format; the
normative text remains the cited document.

A reader who wants to implement only the codec should start at
[`../docs/WRITE-YOUR-OWN-DECODER.md`](../docs/WRITE-YOUR-OWN-DECODER.md). A
reader who wants to evaluate the design should start at
[`PAPER.md`](PAPER.md).

---

## 1. Status vocabulary

| Status | Meaning | What may change |
|---|---|---|
| **Frozen** | Accepted bytes, hashes, and identities are fixed for the version line. Existing conformance vectors are append-only and are never edited. | Documentation, error wording, performance. A change to accepted or emitted bytes needs a new major version or an explicitly additive version line with new vectors. |
| **Experimental** | A wire value is allocated and implemented, but the schema is not promised for persistence. Selection requires an explicit opt-in. | The schema may change in a minor release with new vectors and a compatibility note; stored experimental artifacts are not promised to remain readable. |
| **Specified, no wire value** | A design exists but no mode value is allocated. | Nothing interops; the design is not implementable as a format. |
| **Policy** | A rule about versions, change classes, refusals, or deployment. | By a new policy revision with the old revision retained. |
| **Implemented (non-wire)** | Store or adapter behaviour with no independent byte format. | Semantics may change only with tests and a receipt; no cross-language wire promise. |

`Frozen` does not mean “finished product”. It means a reader may rely on the
bytes. The program's own gaps are listed in `docs/STATUS.md` and in each
`bench/RESULTS-*.md`.

---

## 2. Registry

Identifiers are stable. They are allocated in dependency order, not in order of
importance. In the vector column, `conformance/v1/…` is relative to
`WeftCodec/conformance/v1/` (mirrored in `WeftCodec-Go/`); `conformance/`
without the `v1` is the program-home composite-fixture tree.

| ID | Document | Title | Status | Version | Governs | Conformance vectors |
|---|---|---|---|---|---|---|
| **WEFT-001** | [`WEFT-MODEL.md`](WEFT-MODEL.md) | Identity, state, transactions, durability | **Frozen** v1 core; v2/v3/v4 manifests experimental | 1.0.0 | Store layout, commit record, superblock, manifests, GC roots, recovery, backend contract | `conformance/store/v1`; `conformance/checkpoint/v1`; `conformance/overlay/v1`; `conformance/engram-store/v1` |
| **WEFT-002** | [`WEFTCODEC-FORMAT.md`](WEFTCODEC-FORMAT.md) | Frame format, WCB, error taxonomy | **Frozen** | 1.0.0 | Modes 0–5 (`RAW`, `ZSTD`, `REFERENCE`, `DELTA`, `DICTIONARY`, `STRUCTURED`), header/tables, WCB, limits, classes | `WeftCodec/conformance/v1/codec/`; `.../structured/`; `.../adversarial/` |
| **WEFT-003** | [`WEFT-COMPATIBILITY.md`](WEFT-COMPATIBILITY.md) | Version lines, change classes, unknown data | **Policy** | 1.0.0 | Every version line; mode allocation rules; migration; deprecation | checked by `scripts/check-spec-drift.sh` and the manifest/coverage gates |
| **WEFT-004** | [`WEFT-SELECTORS.md`](WEFT-SELECTORS.md) | WeftQL and canonical selector identity | **Frozen** | 1.0.0 | Selector grammar, canonical AST, `SelectorId` | `WeftCodec/conformance/v1/selectors/` (22 valid) |
| **WEFT-005** | [`WEFT-CONTEXT-ROOT.md`](WEFT-CONTEXT-ROOT.md) | The primary immutable public address | **Frozen** | 1.0.0 | `weft.context-root.v1`, `weft://` grammar, resolution, ambiguity | `WeftCodec/conformance/v1/context/`; `conformance/context/v1/` |
| **WEFT-006** | [`WEFT-CAUSAL.md`](WEFT-CAUSAL.md) | Causal closure and causal identity | **Frozen** | 1.0.0 | `weft.causal.v1`, closure hash, `bytes_only` / `causal_exact` | `WeftCodec/conformance/v1/causal/`; `conformance/context/v1/causal_versions.json` |
| **WEFT-007** | [`WEFT-CAPABILITIES.md`](WEFT-CAPABILITIES.md) | Capability contexts, budgets, enforcement | **Frozen** for records and envelope; enforcement normative | 1.0.0 | `weft.capability.v1`, authenticated envelope, budgets, non-disclosure | `WeftCodec/conformance/v1/capability/` |
| **WEFT-008** | [`WEFT-AI-CODEC.md`](WEFT-AI-CODEC.md) | AI-native codec modes | **Frozen** for modes 6–17 and 19–20; **experimental** mode 18; reserved 21–127 | 1.3.0 | Modes 6–20, descriptors, payloads, validation order, limits | `conformance/v1/{tensor,sharded,weight-delta,kv-cache,activation-stream,semantic-delta,tensor-tiled,sparse-delta,float-tiled,float-tiled-streams,kv-prov,engram}/` |
| **WEFT-009** | [`WEFT-TENSOR.md`](WEFT-TENSOR.md) | Logical tensor identity and region addressing | **Frozen** for identity/layout; mode 18 section experimental | 1.0.0 | `LogicalTensorRoot`, layout descriptors, shard containers, region grammar | `WeftCodec/conformance/v1/tensor/` |
| **WEFT-010** | [`WEFT-PROVENANCE.md`](WEFT-PROVENANCE.md) | Provenance receipts and recovery planning | **Frozen** record and rules | 1.0.0 | `weft.provenance.v1`, attachment, verification, planner | `tests/provenance.rs` (19); no standalone vector directory |
| **WEFT-011** | [`WEFT-TRAINING-READY.md`](WEFT-TRAINING-READY.md) | Training-ready materialization is a refusal | **Implemented (non-wire)** | 1.0.0 | Materialization preconditions, output manifest, verification | `tests/training_ready.rs` (10) |
| **WEFT-012** | [`WEFT-QUOTA.md`](WEFT-QUOTA.md) | Write budgets and branching time travel | **Implemented (non-wire)** | 1.0.0 | Quota records, `set_quota_usage`, branch/merge | `tests/quota.rs`, `tests/branching.rs`; Go `store/quota_test.go`, `store/branch_test.go` |
| **WEFT-013** | [`WEFT-BUCK2.md`](WEFT-BUCK2.md) | Action keys and a store-backed build cache | **Implemented (adapter)** | 1.0.0 | Action key, record, relations, `by:action` Context Root, CLI | `adapters/buck2` tests; `bench/RESULTS-buck2-cas-store.md` |
| **WEFT-014** | [`WEFT-TRANSPORT.md`](WEFT-TRANSPORT.md) | Transport-aligned frame container | **Frozen** | 1.0.0 | `weft.transport.v1`, 4096-byte header, section table, alignment | `WeftCodec/conformance/v1/transport/` (5 valid / 17 reject) |
| **WEFT-015** | [`WEFT-KV-PROVENANCE.md`](WEFT-KV-PROVENANCE.md) | Provenance-bound KV cache pages | **Frozen** | 1.0.0 | Mode 19 `KV_CACHE_PROV`, eight provenance dimensions, store scope | `WeftCodec/conformance/v1/kv-prov/` (8 valid / 12 reject) |
| **WEFT-016** | [`WEFT-ENGRAM.md`](WEFT-ENGRAM.md) | Region-addressable row-group tables | **Frozen** | 1.0.0 | Mode 20 `ENGRAM_SHARD`, row groups, group roots, supersession | `WeftCodec/conformance/v1/engram/` (6 valid / 14 reject) |
| **WEFT-017** | [`WEFT-RETENTION.md`](WEFT-RETENTION.md) | Retention bindings for cached artifacts | **Frozen** record and ordering | 1.0.0 | `weft.retention.v1`, classes, epoch fencing, class-aware GC | `tests/retention.rs` (8); `scripts/retention_cross.sh` (12) |
| **WEFT-018** | [`WEFT-MULTIWRITER.md`](WEFT-MULTIWRITER.md) | Writer capability levels and MW2 transactions | **Frozen** contract; MW2 opt-in; MW3 not implemented | 1.0.0 | MW0–MW2, transaction lifecycle, conflicts, GC pins | `tests/multiwriter.rs` (12); `verification/tla/WeftMultiWriter.tla` |

<!-- BEGIN GENERATED:rfc-conformance (scripts/perf_numbers.py) -->
Manifest: **420** vectors, aggregate
`6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`; the per-mode distribution is the `modes` object in
`conformance/manifest.json`. Regenerate with
`scripts/gen-conformance-manifest.sh` and check with `--check`;
`scripts/check-conformance-coverage.sh` fails when an allocated mode lacks a
valid and a reject vector.
<!-- END GENERATED:rfc-conformance -->

---

## 3. The change process

### 3.1 What “frozen” means

A frozen document fixes the bytes that a conformant writer emits and a
conformant reader accepts. It does not freeze:

- error **message text** (only the class is interoperable);
- performance, allocation strategy, or concurrency;
- an implementation's internal API;
- a semantic that the document explicitly marks as a non-claim.

A change that alters accepted bytes, a hash domain, a canonical encoding, a
frame layout, or an identity hashes different bytes, so it is a major change.
An additive optional field with a safe default is a minor change only if an old
reader already behaves correctly for the default.

### 3.2 Adding an additive payload mode

1. Allocate the **next reserved** mode value; never reuse a value with a
   different meaning.
2. Define a schema string, a payload domain (or state that it reuses another),
   and the exact descriptor key set.
3. Specify the frame shape, limits, and the **normative validation order** that
   determines the error class.
4. Implement in both codec repositories (and the independent Python reader when
   claimed) with the same bytes.
5. Add conformance vectors: at least three valid and a reject for each distinct
   failure class; byte-identical in both codec repositories.
6. Move the reserved-mode reject vector to the new reserved value. Retain the
   old vector byte-for-byte with `superseded_by`; both replay suites skip it.
7. Regenerate `conformance/manifest.json`; the coverage gate must show
   `N:valid/reject`.
8. Update the mode index, this registry, `docs/CONFORMANCE.md`, and
   `docs/WRITE-YOUR-OWN-DECODER.md`.
9. Bump the AI codec document version and state the additive nature in the
   release note. Do not edit an existing vector.

### 3.3 Moving a reserved value

Reserved values are a one-way allocation counter. Moving the reject means:

- the reject vector keeps its historical name and bytes;
- it gains `superseded_by` pointing at the new reject;
- the new reject is `017_unknown_mode_reserved<N>` at the next reserved value;
- coverage reports the new value, and the historical vector is never counted as
  a pass.

The current authoritative reserved reject is **mode 21**. Values `21..=127` are
reserved and `128..=255` are experimental; a persistence reader MUST reject
both unless it implements the experimental contract.

### 3.4 What a compliant implementation must reject

At minimum, and in the order that determines the class:

1. unknown **critical** header flags or feature bits;
2. an unknown or reserved **mode** (`UnsupportedMode`), including 21–127 and
   128–255;
3. an unknown **format major/minor** (`UnsupportedVersion`);
4. unknown **WCB tags**, non-minimal varints, duplicate or unsorted map keys,
   trailing bytes (`NonCanonical`);
5. a frame whose declared lengths, table arithmetic, chunk extents, or
   trailer CRC disagree (`Malformed` / `Corrupt` as the format specifies);
6. a missing, denied, mismatched, or cyclic dependency
   (`DependencyMissing` / `DependencyDenied` / `DependencyMismatch` /
   `DependencyCycle`);
7. a decoded length or content hash mismatch (`Corrupt`);
8. an unknown **commit op** (the whole commit is rejected, never partially
   applied);
9. a reserved or mismatched identity field in a selector, Context Root,
   capability envelope, checkpoint, overlay, transport header, retention
   binding, or MW2 transaction.

An implementation MUST NOT treat an unknown mode, unknown descriptor key, or
unknown section role as opaque bytes and pass them through.

---

## 4. Third-party implementation checklist

This extends
[`../docs/WRITE-YOUR-OWN-DECODER.md`](../docs/WRITE-YOUR-OWN-DECODER.md) to the
modes allocated after it was written. It is the exact order; earlier checks win.

### 4.1 Frame envelope (all modes)

1. `len >= 100` → `Malformed`; `len > max_frame_bytes` → `LimitExceeded`.
2. Magic `WFT1` → `Malformed`.
3. Major/minor supported → `UnsupportedVersion`.
4. Header CRC32C → `Corrupt`.
5. Reserved header fields zero → `Malformed`.
6. Domain `<= 1` → `Malformed`.
7. Unknown critical flags/features, `ORIGINAL_HASH`, `SELF_CONTAINED` →
   `UnsupportedFeature`.
8. `decoded_len`, `dep_count`, `chunk_count` against limits before allocation →
   `LimitExceeded`.
9. `table_len == dep_count*48 + chunk_count*48` → `Malformed`.
10. `len == 96 + table_len + body_len + 4` → `Malformed`.
11. Flag/table consistency (`CHUNKED`, `CHUNK_HASHES`, `DEPENDENCIES`) →
    `Malformed`.
12. Trailer CRC32C → `Corrupt`.
13. Dependency entries (flags, `REQUIRED`, reserved zero, domain, length) →
    `Malformed` / `LimitExceeded`.
14. Chunk entries (contiguous, non-zero length, exact body end, decoded sum) →
    `Malformed`.
15. Mode rules (domain, dep/chunk counts, `DICT_RAW`, flag combinations) →
    `Malformed` / `UnsupportedFeature`.

### 4.2 Mode body

| Mode | Mandatory additional checks in this order |
|---:|---|
| 0 RAW | chunked body uses literal chunks; decoded length; content hash. |
| 1 ZSTD | non-empty body; one complete zstd frame; declared content size (if present) equals `decoded_len`; actual length; content hash. |
| 2 REFERENCE | exactly one dependency; body empty; resolved plaintext content hash. |
| 3 DELTA | COPY/INSERT varints; op count bound; offsets in range; output length; content hash. |
| 4 DICTIONARY | exactly one dependency; zstd frame with that raw dictionary; length; content hash. |
| 5 STRUCTURED | domain 1; canonical WCB; no trailing bytes; content hash over body. |
| 6 TENSOR | exact descriptor keys; dtype/shape/element count; payload length; `logical_root`; content hash. |
| 7 TOKEN_SEQUENCE | token count, boundary deltas, empty-stream rules; decoded length; content hash. |
| 8 QUANTIZED_TENSOR | exact descriptor keys; scale bits, zero point, packed word width; element count; content hash. |
| 9 FLOAT_TENSOR | dtype/shape; exponent plane Huffman (`huffman-canonical-v1`, ≥256 B table); mantissa width/padding; element count; content hash. |
| 10 TENSOR_SHARDED | layout/quantized descriptor; rank/index/extent; one shard per frame; `layout_root` across the shard set. |
| 11 WEIGHT_DELTA | base dtype/shape match; op stream; `delta_root`; reconstructed `logical_root`; content hash. |
| 12 KV_CACHE | page geometry (layer/head/seq), dtype, plane widths, page count; decoded length; content hash. |
| 13 ACTIVATION_STREAM | chunk sequence, per-chunk digest root, stream root, assembly order. |
| 14 SEMANTIC_DELTA | mode-11 checks, then recompute the base semantic root from resolved bytes and compare; `delta_root`; logical root. |
| 15 TENSOR_TILED | tile grid divides extents; full tiles only; per-tile `tile_root`; tile offsets; `tiles_bytes == decoded_len`; `logical_root`. |
| 16 SPARSE_DELTA | exactly nine descriptor keys; zero-run order/length; no stored zero word; no trailing bytes; `delta_root`; content hash. |
| 17 FLOAT_TILED | mode-9 rules per tile; non-dividing tile shape rejected; per-tile `tile_root`; `logical_root`. |
| 18 FLOAT_TILED_STREAMS | `n_streams` in 1..=16; `2N` stream offsets, first zero, monotonic, `offsets[N] == exponent_plane_len`, exponent streams ≥256 B; mantissa interleave; per-tile root; logical root. |
| 19 KV_CACHE_PROV | mode-12 page checks; all eight provenance dimensions and derived roots; cross-checks (`layout.dtype`/`page_rows`, `attention.head_count`/`head_dim`, layer bound); `payload_hash`; descriptor root; content hash. |
| 20 ENGRAM_SHARD | exact descriptor keys; `hash_function` registry; `table_shape`/`vocab_size`; precision and lossless check; `row_group_edges`; group entries and inline ranges; `group_root`/`supersedes` lengths; `payload_hash`; per-group decode; reference resolution; acyclic supersession; whole-table content hash. |

Dependencies are resolved by their declared content hash. A dependency cycle is
`DependencyCycle`; an inferred substitute is never accepted. A mode that takes a
base must verify the base's own root before using it.

### 4.3 Container (WEFT-014, if `weft.transport.v1` is in scope)

Check in the order of `WEFT-TRANSPORT.md` §4.1: length/magic/version, flags,
section count and table offset, table fit and `total_len`, reserved bytes, then
per entry alignment/offset/length/order/reserved/role, then each enclosed frame
prefix and `content_hash`/mode match, then zero padding, align-up total, and
finally `payload_hash`. A single-section reader must not claim it verified
`payload_hash`.

### 4.4 Identity records (only if you claim the corresponding surface)

- **WEFT-004 selector:** parse, canonicalize, re-encode; bytes must be
  identical; `SelectorId = H("weft/selector/v1", WCB)`; ambiguity is an error,
  never a silent choice.
- **WEFT-005 Context Root:** validate namespace/resolver/snapshot/budget;
  canonicalize `weft://`; resolution returns either one object or the complete
  ambiguity set.
- **WEFT-006 causal:** closure is transitive, bounded, acyclic; `causal_exact`
  never downgrades to `bytes_only`.
- **WEFT-007 capability:** verify the envelope, grantee, expiry, revocation,
  budget; authorize before accounting; error text must not disclose hidden
  state.
- **WEFT-010 provenance:** every declared output root must match a supplied
  object; verification re-hashes the record blob and checks containment.
- **WEFT-011 training-ready:** refuse before writing unless resolution is exact,
  every input verifies, the causal closure is complete, authorization passes,
  tensors verify under `--require-tensors`, and the cost envelope is met.
- **WEFT-014 transport:** see §4.3.
- **WEFT-015 KV provenance:** the descriptor root is the identity; the page
  payload is mode 12 verbatim; a store must refuse a scope it cannot prove.
- **WEFT-017 retention:** a binding is **not** an input to any content root,
  descriptor, `ObjectId`, or dedup key. It is mutable, append-only by epoch, and
  may only order GC after reachability is computed.
- **WEFT-018 multiwriter:** verify the transaction id, parent head, op digest,
  conflict class, and idempotent replay; a lost publication must restore the
  last published base before retrying. This is an MW2 contract, not a
  consensus protocol.

---

## 5. Where each gate lives

| Gate | Script | What it proves |
|---|---|---|
| Manifest drift | `scripts/gen-conformance-manifest.sh --check` | <!-- BEGIN GENERATED:rfc-manifest-row (scripts/perf_numbers.py) --> **420** vectors, aggregate `6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8` <!-- END GENERATED:rfc-manifest-row --> |
| Coverage | `scripts/check-conformance-coverage.sh` | every mode 0–20 has valid and reject vectors; every entry has an expectation |
| Spec drift | `scripts/check-spec-drift.sh` | 57 mirrored spec files agree across the four repositories |
| Claims | `scripts/check-claims.sh` | externally visible claims match the committed text |
| Conformance bundle | `scripts/package-conformance.sh` | a third party can run the corpus from the bundle alone |
| Receipt integrity | `scripts/verify-receipts.sh` | checksums, revisions, and bundle manifest verify |

The aggregate runner is `scripts/verify-all.sh`; a court that cannot run is
recorded as `skip` with a reason, and a skip is not a pass.

---

## 6. Performance claims and the comparator

<!-- BEGIN GENERATED:rfc-augmentation (scripts/perf_numbers.py) -->
**Augmentation, not a filesystem comparison.** Every write claim in this repository is
one backend **with and without** the WeftFS protocol + codec. The comparator is that
backend's own durable path (arm A) or a native-plus-manifest path (arm N); the Rust/Go
reference filesystem is development provenance and is never the subject of a headline
comparison, and no row pits it against ext4, XFS, ZFS, Btrfs, 3FS, Lustre, CephFS,
JuiceFS, DAOS, Weka or VAST.

The frozen method is [`bench/CONTRACTS-augmentation.md`](../bench/CONTRACTS-augmentation.md):
arms A native, N native + manifest, B protocol pass-through, C codec, D protocol, E
combined, matched durability/consistency/workload, seven interleaved repetitions. Current
shipped default (arm C): **974.1 ms** least-contended /
1,175.3 ms p50 for **282,106,049 B**, **33.53 %** below A's
424,439,296 B, at **2.98×** A's 327.2 ms. Receipt
`bench/receipts/augmentation-local-20261113T004000Z.json` (stamp 20261113T004503Z, revision
0a01606e2654). The frontier row in §3.1 of the paper is not the shipped default.
<!-- END GENERATED:rfc-augmentation -->
