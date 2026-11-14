# Context-plane conformance requirements and release gates

Status: **reviewable candidate** for draft RFC 0001. This document expands
RFC §15 into a test matrix. It is not a conformance claim for any
implementation, and it does not invent vectors, hashes or counts.

Normative text: [`spec/rfc/RFC-0001-context-plane.md`](../spec/rfc/RFC-0001-context-plane.md)
§15. The frozen record formats remain those of `spec/`. The authoritative
vector inventory is [`conformance/manifest.json`](manifest.json); every path
below is an entry in that manifest unless explicitly marked **missing**.

## 1. How to read this matrix

- **Fixture class**: `valid` (a canonical record that must parse, re-encode and
  produce the declared identity), `adversarial` (malformed, noncanonical,
  truncated, oversized or forged input that must be refused before allocation),
  `refusal` (a well-formed record or request that must be denied or reported
  unproven for a semantic, authorization or profile reason).
- **Gate**: `first-release` means RFC §15 requires it before any profile is
  advertised stable; `later` means the RFC names it but the first read-only
  interoperability claim does not depend on it.
- **Existing corpus**: the vector family or file that satisfies the
  requirement **today**. `partial` means some cases exist but the assertion in
  the first column is not fully exercised. `none` means the corpus does not
  exercise it.
- A skip is never a pass. A vector that one reader skips is listed here as a
  gap in that reader's coverage, not as coverage.

Current corpus inventory (read from `conformance/manifest.json`, not
hand-counted here): the manifest is the source of truth for the aggregate
digest and per-kind counts; `docs/CONFORMANCE.md` describes the runner
contract. The RFC's `conformance/REQUIREMENTS.md` is the human matrix, not a
second manifest.

## 2. Matrix

### Gate G1 — canonical bytes and identity construction (RFC §15.1)

| ID | Requirement (what is asserted) | Fixture class | Existing corpus today | Missing | Gate |
|---|---|---|---|---|---|
| R01 | Every WCB tag decodes and re-encodes byte-identically (`null`, `false`, `true`, `u64`, `i64`, `timestamp`, `text`, `bytes`, `array`, `map`) | valid | `WeftCodec/conformance/v1/structured/001_null.json` … `012_map.json` (12 vectors, all with `canonical_hex`) | — | first-release |
| R02 | Non-minimal varints are refused as `NonCanonical` | adversarial | `structured/013_non_minimal_varint.json` | — | first-release |
| R03 | Unsorted map keys are refused as `NonCanonical` (sort is by full encoded key bytes) | adversarial | `structured/014_unsorted_map_keys.json` | — | first-release |
| R04 | Duplicate encoded map keys are refused as `NonCanonical` | adversarial | `structured/015_duplicate_map_keys.json` | — | first-release |
| R05 | Non-text map keys are refused as `NonCanonical` | adversarial | `structured/016_non_text_map_key.json` | — | first-release |
| R06 | Trailing bytes after one value are refused as `NonCanonical` | adversarial | `structured/017_trailing_bytes.json` | — | first-release |
| R07 | Unknown tags (`0x0A..0xFF`) are refused as `NonCanonical` | adversarial | `structured/018_unknown_tag.json` | — | first-release |
| R08 | Truncated records are refused as `Malformed` and over-deep/nested records as `LimitExceeded` before unbounded allocation | adversarial | `structured/019_truncated_text.json`, `structured/020_depth_limit.json`, `adversarial/022_wcb_depth_limit.json`, `adversarial/023_wcb_item_limit.json` | Oversized/streamed string-bound vector (`max_string_bytes`) is not in the corpus | first-release |
| R09 | The domain-separated hash is exactly `BLAKE3-256(u32le(len(domain)) ‖ domain ‖ data)` and no other construction is accepted | valid + adversarial | Identity vectors below exercise the construction indirectly; there is **no standalone hash-domain KAT vector** | A KAT vector per domain (input bytes → digest) is missing; today every implementation derives the construction from the spec text | first-release |

### Gate G2 — canonical IDs for the frozen reader families (RFC §15.2)

| ID | Requirement (what is asserted) | Fixture class | Existing corpus today | Missing | Gate |
|---|---|---|---|---|---|
| R10 | Selector `SelectorId = H("weft/selector/v1", WCB(AST))` matches the declared id | valid | `WeftCodec/conformance/v1/selectors/001_select_true.json` … `022_select_attrs_b_true_and_attrs_n_false.json` (22 valid, each with `canonical_hex` and `selector_id`) | Selector **refusal** vectors (bad namespace, unsorted order fields, duplicate order fields, grammar errors) are absent from the corpus; `selectors/` has no `1xx` files | first-release |
| R11 | Context Root `ContextRootId = H("weft/context-root/v1", WCB(record))` matches the declared id and the embedded selector bytes hash to `selector_id` | valid | `context/001_root_true.json` … `006_root_empty_causal.json` (6 valid) | — | first-release |
| R12 | Context Root invariants: nonzero snapshot, sorted unique nonzero causal list, namespace grammar, resolver grammar, ordering policy, capability hash, budget shape | adversarial | `context/101_unsorted_causal.json` … `110_unknown_key.json` (10 refusals) | Snapshot is checked for zero, not for **absence in a store** (see R16) | first-release |
| R13 | Causal closure `CausalClosureId = H("weft/causal/v1", WCB(record))`; arrays strictly ascending, unique, nonzero, closure ⊇ parents | valid + adversarial | `causal/001_single_parent.json` … `004_empty_closure.json`; `causal/101_unsorted.json` … `106_unknown_key.json` | — | first-release |
| R14 | `LogicalTensorRoot = H("weft/tensor/logical/v1", WCB(logical record))`; shape/rank/dtype/element-count/`values_hash` rules | valid + adversarial | `tensor/001_logical_bf16_2x3x4.json` … `003_logical_i8_2x2.json`; `101_logical_rank_zero.json` … `110_logical_count_exceeds_limit.json` | — | first-release |
| R15 | Same logical values under row-major, column-major, blocked and sharded layouts yield the **same** `LogicalTensorRoot` | valid | `tensor/201_layout_row_major.json` … `204_layout_sharded.json` carry `canonical_hex` and `decoded_sha256` but **`content_root` is null** | A single cross-layout vector set that asserts one equal root is missing; the equality is currently a Rust/Go test, not a corpus vector | first-release |
| R16 | Pinned snapshot selection; missing snapshot and ambiguous result are refused, never substituted from the head | refusal | `context/103_zero_snapshot.json` covers a zero snapshot only; `conformance/store/v1` is a store fixture, not a selection vector | **Missing entirely**: a portable vector family for `resolve` with pinned snapshot, missing snapshot (`SnapshotMissing`) and ambiguous singular access (`Ambiguous`) | first-release |
| R17 | Semantic-root policies produce the declared root; a policy mismatch is refused | valid + adversarial | `semantic-root/001_root_none_bf16.json` … `003_root_block_quantize_fp32_b128_l256.json`; `101_unknown_scheme.json` … `106_non_finite_policy.json` | — | first-release |
| R18 | Capability envelope identity (`EnvelopeId`, `BlobId`) and canonical form | valid | `capability/001_valid_envelope.json`, `002_valid_minimal_envelope.json` | — | first-release |
| R19 | Capability envelope semantic refusals: bad signature, wrong grantee, expired, revoked | refusal | `capability/101_bad_signature.json` … `104_revoked.json`; noncanonical in `105_non_canonical.json` | These are **verifier-semantic** refusals, not identity refusals; a reader without Ed25519 reports them as explicit skips | first-release |
| R20 | KV provenance descriptor `descriptor_root = H("weft/kv/prov/v1", WCB(descriptor))` and its dimension cross-checks | valid + adversarial | `kv-prov/001_bf16_single_layer.json` … `008_two_pages.json` (8 valid); `101_missing_salt.json` … `111_descriptor_non_canonical.json` (11 refusals) plus `112_reserved_mode_21.json` | — | first-release |
| R21 | Reserved mode values are refused as `UnsupportedMode`, and a historical vector keeps its original bytes with `superseded_by` | adversarial | `codec/017_unknown_mode_reserved21.json`, `kv-prov/112_reserved_mode_20.json` (`superseded_by` 21), `engram/112_reserved_mode_21.json` | — | first-release |
| R22 | Provenance record `ProvenanceId = H("weft/provenance/v1", WCB(record))`, ordered inputs/outputs, duplicates and bounds | valid + adversarial | **none** — there is no `conformance/v1/provenance/` family in the manifest | A frozen provenance vector family is missing. The verifier's provenance checks are self-tests (see R36) and are labelled as such | first-release |

### Gate G3 — authorization and evidence (RFC §15.3–§15.4)

| ID | Requirement (what is asserted) | Fixture class | Existing corpus today | Missing | Gate |
|---|---|---|---|---|---|
| R23 | Authorization precedes evaluation, counting and evidence; a denied ancestor is indistinguishable from an absent one | refusal | No portable vector family. `tests/capability_nondisclosure.rs` and the R5 KV adversarial court exercise it at the store level | A canonical request/response vector family with two authorized views over different hidden populations | first-release |
| R24 | `matched_count` is the authorized pre-limit count; `limit`/`offset` never assert completeness | refusal | none | Missing request/response vectors | first-release |
| R25 | A capability hash is not a bearer credential; a bare `capability_hash` does not authorize | refusal | `capability/` covers envelope verification; no vector shows a bare hash being refused | Missing | first-release |

### Gate G4 — compatibility adjudication (RFC §15.5–§15.6)

| ID | Requirement (what is asserted) | Fixture class | Existing corpus today | Missing | Gate |
|---|---|---|---|---|---|
| R26 | A compatibility request names an exact guarantee and a versioned rule; unknown rule or missing evidence yields `unproven`, never `exact` | refusal | none — the request/decision schemas are **proposed** and deliberately have no wire value | New proposed schemas need independently generated vectors before freeze | later |
| R27 | Mismatched model, adapter, tokenizer, attention mask, positional policy, numerical profile, tenant or causal prefix never yields `exact` | refusal | `kv-prov/` proves descriptor identity differences, **not** a compatibility verdict | Missing the decision-record vectors and a reference evaluator | first-release |
| R28 | A mode-19 descriptor match is not proof of complete tokenizer identity (`tokenizer_root` hashes `{scheme, vocab_size}`) | adversarial + refusal | `kv-prov/106_tokenizer_mismatch.json` proves the root matches the descriptor; no vector proves two tokenizers with the same scheme/size are distinguished | Missing same-scheme/same-size/different-content tokenizer pair | first-release |
| R29 | Non-prefix KV reuse without repair evidence never yields `exact` (`repair_required`/`unproven`) | refusal | none | Missing | later |

### Gate G5 — tensor and training continuation (RFC §15.7–§15.8)

| ID | Requirement (what is asserted) | Fixture class | Existing corpus today | Missing | Gate |
|---|---|---|---|---|---|
| R30 | A lossy conversion (dtype change, quantization, truncation) never claims logical equality unless a policy explicitly defines its root | valid + adversarial | `semantic-root/` proves a policy root differs from the exact root; `semantic-delta/101_base_semantic_mismatch.json` refuses a mismatched base | No vector asserts "lossy output is not the exact logical root" as a boolean | first-release |
| R31 | A weights-only checkpoint is accepted for initialization and refused for exact training resumption when optimizer/random/data state is missing | refusal | `checkpoint/v1/001_manifest_bf16_1shard.json` plus four refuses cover the manifest; **no execution-cut record exists** | Missing `weft.context.execution-cut.v1` vectors and a status matrix | later |

### Gate G6 — cross-backend and lifecycle (RFC §15.9)

| ID | Requirement (what is asserted) | Fixture class | Existing corpus today | Missing | Gate |
|---|---|---|---|---|---|
| R32 | Cross-backend migration preserves logical guarantees without equating unrelated commit snapshots | valid + refusal | `conformance/store/v1` fixture and bench courts (`planes`, `s3`, `ceph`) exercise bytes and heads; no portable migration vector | Missing a portable two-store vector pair with distinct heads and equal logical roots | later |
| R33 | GC never reclaims a reachable blob; a finite grace period alone is not sufficient for unbounded prepare-to-publish | refusal | `tests/retention.rs`, the retention court and `tests/gc_accounting.rs` exercise it; no portable vector | Missing portable lifecycle vectors | later |

### Gate G7 — ABI and independent implementation (RFC §15.10–§15.11)

| ID | Requirement (what is asserted) | Fixture class | Existing corpus today | Missing | Gate |
|---|---|---|---|---|---|
| R34 | The candidate C header is valid C11 and C++17 under `-Wall -Wextra -Werror`, and negotiation/ownership/thread rules are documented | valid (compile-only) | `include/weft_context_plane.h`, `tests/c/test_context_plane_header.c`, `tests/c/test_context_plane_header.cpp`, `scripts/check-context-plane-header.sh` | No binary implementation exists; no major/minor negotiation, release-order or exception-boundary test can run | first-release (compile-only) |
| R35 | A malformed WCB request is refused with `NonCanonical`/`Malformed` at the ABI boundary and delivers no trusted response | adversarial | none (no implementation) | Missing once an implementation exists | later |
| R36 | An implementation from outside the reference tree parses and re-encodes the frozen corpus and reproduces the canonical ids | valid + adversarial | `verifier/` (clean-room Python) validates WCB, selector, Context Root, causal closure, logical tensor, capability identity and KV provenance descriptor; see `bench/receipts/context-plane-verifier-*.json` | Provenance has no corpus (R22); capability signature semantics are out of scope; no external third-party submission | first-release |
| R37 | Every new semantic profile has a reference workflow **and** a deliberately faulty implementation that the tests distinguish | refusal | none | Both are missing for every proposed profile | later |
| R38 | A conformance claim carries a machine-readable manifest with revision, profile list, schema versions, corpus digest, commands and outcomes | valid | `conformance/manifest.json`, `docs/CONFORMANCE.md`, the conformance runner and receipts | The new proposed profiles have no manifest entries (by design: no wire value) | first-release |

## 3. Gate summary

| Gate | Requirements | Covered by an existing corpus family today | New work |
|---|---|---|---|
| G1 canonical bytes | R01–R09 | R01–R07 fully; R08 partial | A hash-domain KAT vector (R09) and a string-bound vector (R08) |
| G2 canonical IDs | R10–R22 | R10 partial, R11–R14, R17–R21 fully | Selector refusals (R10), cross-layout equal-root vector (R15), snapshot/ambiguity vectors (R16), provenance family (R22) |
| G3 authorization | R23–R25 | none | Request/response vectors for auth-before-count, pagination completeness and bare-hash refusal |
| G4 compatibility | R26–R29 | R28 partial (descriptor only) | The five proposed schemas and a reference evaluator; a same-scheme/same-size tokenizer pair |
| G5 tensor/training | R30–R31 | R30 partial | A lossy-vs-exact assertion; execution-cut vectors |
| G6 cross-backend | R32–R33 | none in the portable corpus | Two-store migration and lifecycle vectors |
| G7 ABI/independent | R34–R38 | R34 compile-only; R36 verifier; R38 manifest | A binary implementation; ABI negotiation tests; a deliberately faulty implementation |

**Honest count.** Of the 38 requirements above, **11** are exercised in full by
the current frozen corpus (R01–R07, R11–R14, R17–R21), **7** are exercised only
in part (R08, R10, R15, R19, R28, R30, R34/R38 compile-and-run evidence), and
**20** require new corpus or new implementation work. A requirement listed as
"partial" is not a pass: it names exactly what the corpus does not yet say.

## 4. What the current corpus already covers (by kind)

The kinds below are the `kind` field of `conformance/manifest.json`. Counts are
deliberately not duplicated here; read the manifest for the current aggregate.

| Kind | What it already proves | What it does not prove |
|---|---|---|
| `wcb` | canonical WCB values and the eight canonical refusals | hash-domain KATs, string-size bounds |
| `selector` | canonical selector bytes and `SelectorId` for 22 valid expressions | any selector refusal class |
| `context` | Context Root identity and invariants; causal closure identity and invariants | snapshot presence/absence in a store; ambiguity; provenance |
| `tensor` | logical tensor identity, layout descriptors, semantic-root policies | a cross-layout equal-root vector |
| `capability` | envelope identity and canonical form; four semantic refusals | bare-hash-is-not-authority |
| `checkpoint` | `weft.checkpoint.v1` manifest identity and four refusals | execution cut / training resumption |
| `transport` | `weft.transport.v1` containers (5 valid + 17 refusals) | a secure remote channel |
| `frame` | modes 0–20 valid frames and per-mode refusals | context-plane adjudication |
| `store`, `fuzz` | a store fixture and a differential seed | portable cross-backend migration |

## 5. New work this matrix names

1. **Hash-domain KATs (R09).** One vector per frozen domain, carrying exact
   input bytes and the expected digest. No such vector exists today; adding one
   does not change any frozen record.
2. **Selector refusal vectors (R10).** The corpus has 22 valid selectors and no
   negative selector. A refusal family (bad grammar, wrong namespace, unsorted
   or duplicate order fields) is missing.
3. **Cross-layout equal-root vector (R15).** One logical tensor under four
   layouts must declare one root. Today the layout vectors carry
   `content_root: null`.
4. **Snapshot and ambiguity vectors (R16).** These require a store view, so
   they belong in a portable `conformance/context/` request/response family,
   not in a codec frame.
5. **Provenance family (R22).** `weft.provenance.v1` has normative text,
   Rust tests and no frozen vector. This is the largest single gap in the
   reader matrix.
6. **Authorization and evidence vectors (R23–R25).** Needed to make
   "auth-before-count" a portable assertion rather than a per-implementation
   test.
7. **Proposed-schema vectors (R26–R29, R31, R37).** Deliberately blocked until
   the RFC's request, decision, execution, materialization-intent and
   execution-cut schemas are reviewed and given actual bytes. No wire value is
   assigned by this document.

## 6. Running the matrix

Existing, shipping evidence:

```sh
# Rust and Go codec adapters (built inside the rust/go courts).
scripts/run-conformance.sh scripts/conformance-adapter-rust.sh --json OUT.json
scripts/run-conformance.sh scripts/conformance-adapter-go.sh   --json OUT.json

# Independent Python reader (python court; it imports blake3).
WEFT_PYTHONPATH=python PYTHONPATH=python \
  scripts/run-conformance.sh scripts/conformance-adapter-python.sh --json OUT.json
```

Independent verifier (Phase 1 of the adoption plan):

```sh
python3 verifier/verify_corpus.py --json bench/receipts/context-plane-verifier-<stamp>.json
```

The C ABI declaration gate:

```sh
scripts/check-context-plane-header.sh
```

None of these commands changes a frozen byte. A new vector is added by
appending a file and regenerating the manifest (`scripts/gen-conformance-manifest.sh`),
never by editing an existing vector.
