---
status: draft candidate proposal — NOT an IETF RFC, NOT ratified, NOT an implementation claim
date: 2026-11-13
placed at revision: f8f352ba9db37dfa08d9305fe3ba7f9288d6be46 (WeftFS staging head this file was added on top of)
---

# WeftFS — Open Context Plane Protocol for Interoperable AI Storage

**Draft RFC 0001 · Candidate specification · 13 November 2026**
**Status:** Proposal for public technical review. **Not an IETF RFC, ratified industry standard, or implemented WeftFS release.**
**Reference baseline:** WeftFS `staging` commit `0efdcb328b967e546e7f6984a5677bc4727101b2` (13 November 2026).
**Scope:** Semantic interoperability of AI storage state, not a replacement for a filesystem, transport, tensor runtime, checkpoint engine, or KV block manager.

## Abstract

This document proposes a vendor-neutral context plane through which independent AI runtimes and storage implementations can exchange and verify **what a stored computational artifact means**, its logical identity, its ancestry, its admissible uses, its authority constraints, and the state needed to reconstruct or continue a computation. A backend remains responsible for its data plane. A runtime remains the authority for the computational semantics of the operators it executes. The context plane binds their declarations to canonical, versioned, independently verifiable records; refuses unsupported claims; and returns reasoned compatibility verdicts.

The design preserves the existing WeftFS `weft.context-root.v1`, selector, causal and logical-tensor identities byte-for-byte. The new compatibility, execution and materialization records described here are **proposed**, with no assigned production wire values. This RFC defines a minimal interoperable reader, a set of separable optional profiles, a language-neutral operation surface, a candidate C ABI, a security model, and the conformance process required before any profile is declared stable.

## 1. Motivation and boundary

A byte-perfect artifact can be computationally inappropriate. A KV page produced with one model, attention mask or token history cannot automatically be reused for another. An exact checkpoint of model weights may omit the optimizer, random or data-consumption state needed for training continuation. A tensor can have the same canonical values under different physical layouts, while a lossy conversion does not preserve identical values. The protocol distinguishes:

1. **Byte identity:** canonical content is identical.
2. **Logical identity:** state is identical under a *specific defined logical value schema*.
3. **Compatibility:** a defined consumer may use that state for a *particular computation*.
4. **Authorization:** the requesting principal may access or act on it.
5. **Availability and reconstruction:** required state is present or reconstructible under a declared contract.

None of the five implies all the others. Implementations MUST NOT infer computational equivalence from a content hash or from successful decoding alone.

### 1.1 What the protocol does not own

It does not own RDMA, GPU memory, block placement, network transfers, replication, erasure coding, storage durability, inference scheduling, distributed checkpoint orchestration, framework operator semantics, or the correctness of arbitrary user-supplied provenance assertions. Those remain with existing providers. WeftFS and WeftCodec are possible implementations, not mandatory dependencies.

### 1.2 Existing work and novelty boundary

vLLM already includes parent-prefix hashes, token blocks, LoRA and multimodal identities and salts. SGLang HiCache, LMCache and NVIDIA Dynamo already coordinate distributed and tiered KV state. PyTorch Distributed Checkpoint already reshares checkpoints across topologies. CacheBlend, Cache-Craft and SAFE-KV investigate non-prefix cache reuse and repair, demonstrating that matching text or blocks alone is insufficient. This proposal does **not** claim those mechanisms as novel. Its intended contribution is **a public, executable semantic contract for independently verifiable admissibility across implementations**, with typed failure and complete evidence requirements.

## 2. Normative language and stages

The capitalized words MUST, MUST NOT, SHOULD, SHOULD NOT and MAY follow RFC 2119 and RFC 8174. Requirements here are candidate normative text; they are not representations that released software already conforms.

A feature's status is one of: **Existing WeftFS specification**, **Implemented in the cited WeftFS reference code**, **Proposed in this RFC**, **Experimental evidence**, or **Unsupported**. Documentation MUST state the status for each profile and each independent implementation. A documented wire shape is not evidence of a working runtime adapter.

## 3. Core model

### 3.1 Identity namespaces and canonical serialization

The first interoperable profile uses WeftFS canonical WCB and its frozen domain-separated BLAKE3 construction:

`H(domain, data) = BLAKE3-256(u32le(byte_length(domain)) || ASCII(domain) || data)`.

Existing identities (for example `ContextRootId = H("weft/context-root/v1", WCB(context-root record))`) MUST remain exactly unchanged. Existing record schemas and their limits are incorporated by reference from the normative WeftFS specifications, not restated with subtly different rules. A conforming decoder MUST reject noncanonical data, unknown critical fields, incorrect hash bindings and unsupported schema versions. A conforming implementation MUST NOT silently reinterpret a record using a different serializer or hash domain.

A future deterministic CBOR profile MAY be separately specified, but its identity mapping and domain separation MUST be negotiated. CBOR is **not** assumed byte-equivalent to WCB. JSON may be used for diagnostics but MUST NOT serve as an unversioned alternative identity serialization.

### 3.2 Distinguished identities

| Concept | Meaning | Existing basis |
|---|---|---|
| `ContentId` | Canonical bytes under a declared content schema | `WEFT-MODEL.md` / codec format |
| `LogicalTensorRoot` | Dtype, shape and canonical logical element values; independent of layout | `WEFT-TENSOR.md` |
| `SelectorId` | Identity of a canonical query expression; NOT its results | `WEFT-SELECTORS.md` |
| `ContextRootId` | Pinned selector + snapshot + resolver + causal scope + optional capability/budget | `WEFT-CONTEXT-ROOT.md` |
| `CausalClosureId` | Hash of an asserted canonical closure, whose truth must be checked | `WEFT-CAUSAL.md` |
| `ProvenanceId` | Identity of an immutable provenance assertion, NOT proof of execution | `WEFT-PROVENANCE.md` |
| `CapabilityHash` | Identity of a capability grant, NOT a signature or bearer credential by itself | `WEFT-CAPABILITIES.md` |

Implementations MUST distinguish **record identity** from **trust in a record's claims**. A hash proves integrity only relative to the hash algorithm and trusted reference; a signature can authenticate an assertion but cannot prove a computation ran correctly.

### 3.3 Context Roots and independent stores

The existing Context Root includes a **pinned commit-root snapshot**, not merely a logical artifact set. A re-import into a distinct store with a different commit history may yield a different ContextRootId even when all logical tensor values are identical. Implementations MUST NOT claim cross-store ContextRootId equality merely from equal tensors. Cross-store comparison MAY return an independently verified logical-equivalence witness referencing *both* original roots and the applicable selection/interpretation contract; it MUST preserve their distinct snapshot histories.

### 3.4 Causal versus reconstruction graphs

Causal graphs MAY have cycles and are statements about relationships; reconstruction dependency graphs MUST be acyclic for an exact decode path. The resolver MUST use bounded visited-set traversal and MUST NOT assume a causal edge implies a runnable transformation or authority to read the other endpoint.

## 4. Roles and discoverable profiles

An implementation may support more than one role:

- **Reader:** validates canonical records, their IDs, and locally supported schemas. This role is required for a conformance claim under this RFC.
- **Resolver:** evaluates a Context Root at its pinned snapshot using the named resolver and authorized universe.
- **Compatibility evaluator:** evaluates a *specific* requested use of computational state and produces a reasoned verdict.
- **Materialization planner:** produces a checked, bounded set of required objects/regions/dependencies without claiming to schedule storage traffic.
- **Publisher:** attaches or publishes records with durability and consistency semantics separately defined by its backend; optional and not part of the first read-only interoperability claim.

Independent profiles may be advertised as strings of the form `weft.<profile-name>.v1`, with a registry recording the schema, compatible versions, test fixtures, and security assumptions. The following **existing** schema families may be referenced: context-root v1, selector v1, causal v1, logical-tensor v1, KV provenance v1, training-ready v1. The following **new** schemas are proposed below and MUST NOT be advertised as implemented until conformance exists: `weft.context.compatibility-request.v1`, `weft.context.compatibility-decision.v1`, `weft.context.execution.v1`, `weft.context.materialization-intent.v1`, `weft.context.execution-cut.v1`.

Profile negotiation MUST fail closed on a missing required profile. Unsupported optional profiles MUST be explicitly reported; they cannot be treated as successfully evaluated. No implementation is required to implement WeftCodec's physical coding modes in order to read and validate context records.

## 5. Language-neutral API operations

The normative abstract operations, independent of the local ABI or network transport, are:

| Operation | Required role | Input | Result or refusal |
|---|---|---|---|
| `describe` | Reader | requested protocol version and required profiles | negotiated capabilities, limits, identities and profiles |
| `validate_record` | Reader | canonical WCB record, optional claimed digest | canonicality, schema validity, computed identity, typed failure |
| `resolve` | Resolver | canonical Context Root, authenticated principal context, budget | pinned and authorized ordered results, true pre-limit count, evidence |
| `evaluate_compatibility` | Compatibility evaluator | typed request (Section 6) | decision (Section 7) and evidence |
| `dependency_closure` | Resolver | roots, relation scope, missing-only verified-local set, budget | authorized closure and completeness state |
| `plan_materialization` | Materialization planner | intent (Section 9) | complete ordered plan, blockers, dependencies, required transformations |
| `verify_execution_cut` | Optional execution-cut profile | checkpoint and target execution contract | valid continuation, transformation needed, partial only, or refusal |

A successful transport response is NOT necessarily a successful semantic decision. In particular, `unproven` and `denied` are valid decision *outcomes*, not transport errors. Singular selection MUST fail on ambiguity; an `offset` or `limit` MUST NOT be represented as an assertion of result-set completeness. All operations MUST be explicitly scoped to a pinned snapshot or report that they are provisional/nonidentity live queries. No mutable `latest` alias is allowed inside an immutable Context Root.

### 5.1 Common request envelope (proposed)

Canonical WCB map with **exactly**:

- `schema`: `weft.context.request.v1` (text)
- `request_id`: bytes(16) (required, opaque correlation value; no authorization significance)
- `operation`: one of the operation strings in Section 5 (text)
- `params`: canonical WCB map (required, operation-specific)
- `required_profiles`: ordered, unique list of registered profile strings (may be empty)
- `max_response_bytes`: u64 from 1 to 16,777,216 (required)

Unknown keys MUST be rejected; the envelope is not itself a persistent contextual identity. The outer envelope does not contain a claimed principal: a caller-supplied principal string MUST NOT establish authority. The execution environment supplies an authenticated principal; capability context can only narrow it.

### 5.2 Common response envelope (proposed)

Exactly: `schema="weft.context.response.v1"`, `request_id:bytes(16)`, `outcome:"ok"|"error"`, `result:map|null`, `error:map|null`. Exactly one of `result` and `error` is non-null. `error`, when present, contains `class:text` and `code:text` drawn from the versioned failure registry; externally visible detail is intentionally absent at nondisclosing boundaries. For non-privacy-critical failures, an authenticated diagnostic endpoint MAY provide details separately. An implementation MUST NOT return partial success after a budget failure; if pagination is supported, each complete page has a snapshot-bound cursor and an explicit `complete:false` until the final page.

For `describe`, the result MUST report `protocol_major`, `protocol_minor`, `wire_format`, supported `roles`, supported `profiles` (with per-profile version and implementation status), `max_request_bytes`, `max_response_bytes`, `hash_domains`, and the implementation's conformance manifest root. These are factual implementation declarations, not evidence of passing independent tests; clients SHOULD verify conformance claims when trust is required.

## 6. Compatibility request (proposed)

`weft.context.compatibility-request.v1` is a canonical WCB record with these keys; unknown keys are rejected:

| Key | Type | Semantics |
|---|---|---|
| `schema` | text | exactly `weft.context.compatibility-request.v1` |
| `subject_kind` | text | registered state kind, e.g. `kv_page`, `logical_tensor`, `training_cut` |
| `subject_root` | bytes(32) | nonzero domain-typed subject root |
| `consumer_kind` | text | registered consumer operation, e.g. `attention_read` or `training_resume` |
| `consumer_contract_root` | bytes(32) | nonzero identity of versioned execution/consumer contract |
| `snapshot` | bytes(32) | nonzero pinned snapshot of requested evaluation |
| `requested_guarantee` | text | one of `byte_exact`, `logical_exact`, `execution_equivalent`, `behavior_validated` |
| `compatibility_rule` | text | fully qualified registered rule and version; no unversioned defaults |
| `capability_hash` | bytes(32) or null | hash binding if required by request; NOT credential by itself |
| `constraints` | canonical map | rule-defined exact constraints, mandatory even when empty |

The request's identity, if persisted, is `H("weft/context/compatibility-request/v1", WCB(record))`; this is a **new proposed domain**, not an allocated existing WeftFS identity. A compliant reader MUST NOT infer the requested guarantee from what it can cheaply verify. An unsupported rule or missing evidence yields `unproven` or a typed unsupported-rule refusal, never `exact`.

### 6.1 Execution contract (proposed)

A runtime-supplied `weft.context.execution.v1` record specifies, where relevant: immutable model and adapter roots; tokenizer and preprocessing roots; operator graph and attention semantics; positional policy and full relevant input history; cache tensor roles and geometry; numeric precision and quantization; deterministic kernel/phase/batch conditions; and sharing domain. Its canonical field registry and equivalence rules require **separate, frozen, independently tested profile specifications**. Merely naming a runtime version MUST NOT establish execution equivalence. Unspecified dimensions cannot be interpreted as matching defaults unless the profile formally defines them. The producer runtime MUST attest the computational dependency facts that the verifier cannot independently derive. **Important existing limitation:** the frozen mode-19 `tokenizer_root` currently hashes `tokenizer={scheme, vocab_size}`; two tokenizers with different vocabulary/merge/normalizer/special-token mappings can share that same record. The mode-19 tokenizer root alone therefore MUST NOT be treated as proof of complete tokenizer identity. A new execution-contract profile MUST bind a digest of the actual tokenizer implementation/configuration and any required preprocessing/chat-template semantics before claiming exact cross-runtime reuse. Do not mutate frozen mode-19 bytes to accomplish this.

## 7. Compatibility decision and guarantee semantics (proposed)

The result `weft.context.compatibility-decision.v1` contains: `schema`, `request_root:bytes(32)`, `verdict:text`, `established_guarantee:text|null`, `rule:text`, `evidence:[map]`, `missing:[text]`, `reason_code:text`, `evaluated_snapshot:bytes(32)`. Evidence entries MUST be canonical, typed, ordered by `(kind, root)` with no duplicates and MUST NOT expose unauthorized object existence. Each evidence entry identifies a public rule-defined proof/check, witness root, or independently checkable fact; mere producer assertions are labelled `declared`.

| Verdict | What it permits | Required evidence |
|---|---|---|
| `exact` | Use *under the specifically named exact guarantee and operation* | Complete identity and execution-rule proof for that scope |
| `convertible` | Run a named authorized transformation, then re-evaluate | Verified transformation contract and preconditions; not permission to use original state unchanged |
| `repair_required` | Recompute specified dependent state; do not reuse unchanged | A valid repair plan; not a claim that repair already happened |
| `approximate` | Policy-authorized experimental reuse only | Explicit opt-in, declared comparison criterion, validation evidence and known limits |
| `denied` | No reuse | Authorization failure or demonstrated incompatibility, with nondisclosing external code where required |
| `unproven` | No reuse | Missing or insufficient evidence; absence is not equivalence |

`exact` MUST NOT mean "hashes matched" unless the requested guarantee was *only* byte/logical equality. `behavior_validated` MUST NOT be silently upgraded to execution equality. `approximate` MUST NEVER satisfy an exact request. `convertible` MUST NOT claim exactness until the transformed state passes the stated postcondition. A consumer SHOULD check a witness itself or through an independent trusted verifier, rather than blindly trusting the producer's verdict.

## 8. Specialized compatibility profiles

### 8.1 KV state profile (first high-value extension)

Building on existing WeftFS `weft.ai.kv_prov.v1` mode 19 and `weft.kv.prefix.v1`, a frozen KV compatibility rule MUST bind at least: actual model-parameter identity, adapters, tokenizer/preprocessing identity, attention configuration and mask semantics, positional treatment, exact causal token/multimodal history, layer and head geometry, storage dtype and quantization conventions, tenant/sharing domain, and the producer and consumer numerical execution contracts *to the extent required by the requested guarantee*.

A token match without an equivalent causal history MUST NOT authorize reuse. A matching geometry alone MUST NOT authorize reuse. Unknown attention variations (e.g. latent attention, sliding windows, cross-attention, hybrid/recurrent state) MUST be refused until a registered profile defines their dependencies and conversion behavior. A change of memory layout may be a *convertible representation* but not automatically a byte-identical execution state.

Non-prefix cached chunks MUST default to `repair_required` or `unproven`; any approximate blending must be explicit. Likewise, a mode-19 descriptor match that omits full tokenizer identity, attention mask semantics, relevant per-layer operator identity, or numerical execution constraints is insufficient to prove `execution_equivalent`. Research results about generation quality MUST NOT be described as mathematical exactness. Prefix reuse is only `exact` where the entire versioned execution contract establishes that claim.

### 8.2 Tensor profile

`LogicalTensorRoot` is independent of row/column-major, blocked and sharded physical layouts when canonical logical element values and dtype/shape agree, as defined in `WEFT-TENSOR.md`. Different dtypes, quantization scales, numerical values, or lossful conversions are NOT the same logical tensor by default. View, transpose, reshuffle, reshard, adapter merge and quantize are separate typed transformations with individually defined exactness rules. The protocol MUST NOT conflate shared storage, aliasing, logical value identity and computational substitutability. A logical tensor region MUST be identified with checked bounds, global shape, canonical coordinates, shard mapping, version and expected logical root. Partial reads cannot prove a whole tensor's root without a valid authenticated commitment/proof structure; the result MUST state its verification scope.

### 8.3 Training continuation profile

A checkpoint may be complete for **model initialization** while incomplete for **training resumption**. A continuation contract MAY require model parameters, optimizer state, scheduler and scaler, random streams, data-pipeline cursor and packing state, gradient accumulation, distributed collective boundary, rank topology and in-flight work. The runtime MUST declare a coherent capture boundary; storage cannot infer it from files. Continuation MAY be exact only under a defined execution/determinism class and a verified state cut; a topology change may require explicit transformations. A missing mandatory component yields `partial_only` or refusal, never a silent `exact`.

### 8.4 Model changes and causal invalidation

A runtime may publish a typed operator/tensor dependency graph. Change impact MUST be computed over declared relevant operators and input identities, not tensor filenames. If dependencies are incomplete, the system returns `unproven`, not "unaffected." Unchanged upstream operator subgraphs may permit reuse under a proven contract; downstream dependencies must be invalidated or re-evaluated. A lineage relation is not a proof of computational dependency completeness.

### 8.5 Agents, multimodal and specialized state

Agent continuation profiles MAY bind immutable conversation branches, sampled-token history, random state, tool completion receipts and permission scope. External side effects require independent idempotency/transactional mechanisms; WeftFS does not guarantee exactly-once tool calls. Multimodal profiles bind transformations from source media to tokens/embeddings (crop, frame/timestamp, encoder, preprocessing, insertion positions). Mixture-of-experts, recurrent, diffusion and latent-attention states use separate architecture profiles rather than pretending all computational state is ordinary KV.

## 9. Materialization intent (proposed)

`weft.context.materialization-intent.v1` contains exactly: `schema`, `context_root:bytes32`, `consumer_contract_root:bytes32`, `requested_artifacts:[map]`, `required_guarantee:text`, `capability_hash:bytes32|null`, `budget:map`, and `allow_partial:bool`. Each artifact request MUST name a typed logical identity, canonical region or full object, and expected verification scope. The planner returns a deterministic, ordered set of logical requirements, matched representations and unmet dependencies, *without prescribing transport execution*. If `allow_partial=false`, a missing required dependency yields a typed failure and no claim of a complete set. A plan is not proof that a subsequent execution completed; actual materialization MUST revalidate snapshot bindings, authority and content.

## 10. Consistent checkpoint / execution cut (proposed)

`weft.context.execution-cut.v1` binds `execution_contract_root`, `checkpoint_context_root`, typed mandatory state roles, a declared capture barrier, producer attestations, verified tensor/optimizer/data state roots, permitted transforms, and completion status. The capture barrier is a runtime assertion requiring independent evidence according to the training profile. The protocol distinguishes `complete_for_initialization`, `complete_for_resume`, `partial_only`, `inconsistent` and `unproven`; none may be silently promoted. A successful test of a resumed step validates only the tested execution contract; arbitrary hardware/kernel changes do not inherit bitwise equivalence.

## 11. Transport and wire envelopes

The semantic API is transport-neutral. A minimal optional stream transport can frame one canonical WCB envelope as `u32le(payload_length) || payload`, with `payload_length` between 1 and 16,777,216 bytes and no trailing bytes. Overlong, duplicate, truncated or noncanonical messages MUST be rejected before attempting schema resolution. Protocol errors carry the response envelope from Section 5; malformed frames may terminate a session with a transport failure. This is only a *framing rule*, not a complete secure network protocol.

A remotely exposed implementation MUST use a transport providing server authentication, confidentiality, integrity and replay protection appropriate to its deployment, and authenticate the principal outside caller-controlled request fields. A separate transport-binding document MUST define the selected secure channel, connection lifecycle, request concurrency, cancellation and principal binding before claiming remote-network interoperability. In particular, this RFC **does not claim** that the framing rule plus a bare socket constitutes a secure service.

## 12. Candidate C ABI and ownership

The companion `include/weft_context_plane.h` is a proposed in-process C ABI. It is **not** part of the current WeftFS published library API. It defines one versioned entry point returning a fixed-size function table of opaque handles and canonical WCB byte slices. It does not expose Rust layout, Go pointers, C++ objects, tensor allocations, GPU pointers or exceptions across the boundary.

In ABI major 1:

- the library exports exactly `weft_context_get_api_v1` as its required discovery symbol;
- a caller provides the size of its function-table struct and requested minimum ABI minor;
- version negotiation fails on incompatible major/minor or undersized table;
- `open` binds to a trusted local configuration; `invoke` accepts one canonical request and returns an opaque owned buffer containing a canonical response;
- input bytes are borrowed only for the duration of the call;
- output buffer lifetime is controlled only by `buffer_release` from the same producer library;
- no function may unwind a Rust panic or propagate a C++ exception through the boundary;
- each session is single-threaded unless a separately negotiated extension states otherwise;
- callers MUST keep the producer library loaded until every session and buffer is released;
- `invoke` returns `WEFT_CONTEXT_OK` when an envelope was delivered, even if that envelope contains `outcome:"error"`; nonzero ABI status means no trustworthy response was delivered.

Future ABI minor revisions may add only trailing function-table fields and optional features, preserving old offsets. Major revisions require a different symbol and structure; no in-place struct reinterpretation is permitted. A stable ABI is not a wire-format version.

The ABI deliberately does not transfer tensor memory. An optional data-interchange adapter MAY use existing DLPack or Arrow interfaces rather than inventing a second memory ownership protocol. The core handles context bytes and metadata only.

## 13. Error semantics and partial results

Existing WeftFS typed classes retain their meanings, including `Corrupt`, `Malformed`, `NonCanonical`, `UnsupportedVersion`, `LimitExceeded`, `Ambiguous`, `Unauthorized`, `DependencyMismatch`, `BudgetExceeded`, and `NotFound`. Candidate additions include `UnsupportedProfile`, `Unproven`, `Incompatible`, `SnapshotMissing`, `CapabilityRevoked`, `TransformationRequired`, and `MaterializationIncomplete`; these strings are **proposed** and must be reconciled against the existing class registry before freeze.

A required dependency unavailable, a missing capability proof, a mismatched numerical contract, and an unsupported execution rule MUST yield separate machine-identifiable internal causes. When nondisclosure is required, the externally visible response MUST collapse denial and absence into the same documented code and MUST NOT leak existence through counts or evidence. Timing side-channel resistance is not guaranteed by this document. A budget interruption cannot be reported as a complete result. Result cursors MUST bind to the exact snapshot, selector, principal authorization context and rule version; replay under a different context fails closed.

## 14. Security and trust model

Threats include malformed or adversarial WCB input, hash substitution, untrusted producer assertions, forged provenance, capability confusion, cross-tenant KV leakage, stale policy epochs, incomplete materialization, replay, malicious compatibility rules, unbounded recursion, unauthorized evidence disclosure, and mutable-backend data races.

A ContentId or CapabilityHash is not an access token. The enforcement principal MUST be authenticated independently. Evidence origin must be explicit: `verified_bytes`, `independently_replayed`, `signed_attestation`, `trusted_runtime_attestation`, or `declared_only`; each has a registered scope and limitations. A signed claim is authenticated *as a claim*, not proven computationally true. Independent verifiers MUST reject unsupported evidence classes for an exact guarantee. Consumers MUST NOT load or execute arbitrary transformations referenced by untrusted metadata without an allowlisted runtime adapter. Resource limits apply before allocations. Secrets must not be copied into reusable context metadata or error messages.

Snapshot verification and authorization MUST precede contextual selection and materialization. Revocation checks must bind to the stated enforcement boundary; the current WeftFS capability specification does not claim global distributed revocation. A cross-backend implementation MUST NOT silently upgrade a local revocation claim into a global one. Immutable object publication and garbage collection need an explicit in-flight protection/retention contract: a finite grace period alone is not sufficient for unbounded prepare-to-publish intervals. In particular, implementation correctness claims MUST exclude unsafe GC behavior until adversarial writers and deletion are tested.

## 15. Conformance requirements

No implementation may advertise `conformant` without a machine-readable conformance manifest listing exact source revision, profile list, schema versions, corpus digest, executable commands and test outcomes. Conformance requires **independently authored** implementations and both valid and adversarial fixtures. A two-language implementation derived from the same algorithm is useful; an independent verifier from outside the reference tree is stronger evidence.

Minimum first-release gates:

1. Parse-and-re-encode byte identity for frozen WCB records; reject noncanonical encodings, extra keys and mismatched hashes.
2. Match canonical IDs for Context Roots, selectors, causal closure and logical tensors under a shared frozen corpus.
3. Pinned snapshot selection and ambiguous-result refusal, including missing snapshots.
4. Auth-before-count/evidence with nondisclosing denial-vs-absence behavior.
5. Compatibility adjudication refuses mismatched model, adapter, tokenizer, mask, position, numerical profile, tenant and causal prefix.
6. Non-prefix KV retrieval without adequate repair evidence never yields exact reuse.
7. Same logical tensor under different physical layouts yields equal logical root; lossy conversion never claims equality.
8. Training resumption refuses incomplete optimizer/random/data state while allowing separately declared initialization.
9. Cross-backend migration preserves logical guarantees without fabricating equality of different commit snapshot IDs.
10. ABI major/minor negotiation, ownership, malformed WCB input, callback/thread safety, release order, and error envelopes pass under C, Rust and Go interoperability tests.
11. For every new semantic profile, establish a reference workflow, a deliberately faulty implementation, and tests that distinguish them.

The accompanying `conformance/REQUIREMENTS.md` expands these into a reviewable test matrix and release gates. **No golden byte vectors are invented in this draft.** All proposed schemas must receive actual independently generated and reviewed vectors before their wire formats are frozen.

## 16. Versioning and extension rules

The identity hash domain and canonical serializer are immutable within a frozen schema version. Changing an identity-affecting field, normalization rule, map-key ordering, reference scope or evaluation semantics requires a new schema/rule version and new frozen vectors. Unknown critical fields and execution rules fail closed. New optional profiles may be negotiated without redefining the base ContextRootId. A feature is not supported merely because its record can be parsed.

A public registry SHOULD record schema strings, hash domains, compatibility rule names, canonical encoding, semantics, test vectors, threat-model notes, ownership/licensing, and change process. Allocation follows technical review with competing implementations and published test failures, not unilateral version-number growth. A profile's *normative bytes*, its *reference implementation*, and its *adoption maturity* are separate statuses.

## 17. Standards governance and adoption

Publish the proposal as an openly reviewable WeftFS RFC in the existing repository identity. Keep the descriptive title; do not invent a competing umbrella name. Invite reviewers from inference runtimes, checkpoint frameworks, tensor interchange libraries and mature storage backends. Require a public issue/decision log, transparent specification changes, an explicit patent/IP policy suitable for vendor adoption, and independent conformance submissions. An RFC-style document in a GitHub repository is **not** an official IETF RFC; registration or standardization by an external body would require that body's process.

Recommended initial compatibility targets: WeftFS Rust/Go readers; an independent third-party WCB/Context Root verifier; one vLLM or SGLang runtime adapter for exact KV scope; one PyTorch Distributed Checkpoint adapter for execution-cut evidence; and a mature backend connector that uses WeftFS only as a context plane. The point is **semantic interoperability**, not a synthetic speed contest.

## 18. Identified blockers before version 1 freeze

- **Normative registry reconciliation:** The checked WeftFS `WEFTCODEC-FORMAT.md` still contains a historical mode-range table showing values `19..127` as reserved, while `WEFT-AI-CODEC.md` explicitly defines modes 19 (`KV_CACHE_PROV`) and 20 (`ENGRAM_SHARD`) as implemented. The authoritative registry must be reconciled and tests frozen before vendors implement these modes. This is a concrete editorial/interoperability defect, not a proposal to change the frame bytes.
- **Existing tokenizer identity gap:** `WEFT-KV-PROVENANCE.md` binds `tokenizer_root` to the canonical `{scheme,vocab_size}` map; a rigorous exact execution contract additionally requires a root for complete tokenizer/preprocessor identity. Existing mode-19 bytes must remain frozen.
- **Execution semantics:** The runtime/operator compatibility registry is not frozen; model, attention and numerical contracts cannot yet claim independent exact reuse beyond what existing specific profiles prove.
- **Principal binding:** A secure remote transport and authentication binding are not yet specified.
- **Independent implementation:** Existing Rust/Go and vector parity are strong foundations, not proof of vendor-neutral adoption.
- **Cross-backend snapshots:** Need an explicit comparison/export proof without equating unrelated commit histories.
- **Attestation assurance:** Need identity and trust roots for runtime claims, including revocation and independently replayable evidence where possible.
- **Garbage-collection safety:** Need cross-writer in-flight retention mechanisms before advertising unrestricted distributed lifecycle safety.
- **ABI acceptance:** The header compiles, but it is *not* a working library, shipped symbol, or binary-compatibility certification.

## 19. Prior art and references

References reflect the state of public documentation available on 13 November 2026. The companion research document discusses why each matters.

- WeftFS existing specification: https://github.com/infinityabundance/WeftFS/tree/staging/spec
- vLLM automatic prefix caching: https://docs.vllm.ai/en/stable/design/prefix_caching/
- vLLM canonical KV offloading layouts: https://github.com/vllm-project/vllm/blob/main/docs/features/kv_offloading_usage.md
- SGLang HiCache: https://www.sglang.io/blog/hicache
- NVIDIA Dynamo KV block management: https://docs.dynamo.nvidia.com/dynamo/components/kvbm
- LMCache: https://docs.lmcache.ai/
- CacheBlend: https://www.microsoft.com/en-us/research/publication/you-only-prefill-once-combining-cached-knowledge-for-large-language-model-serving-with-cacheblend/
- Cache-Craft: https://arxiv.org/abs/2502.15734
- SAFE-KV: https://aclanthology.org/2026.findings-aacl.155/
- PyTorch Distributed Checkpoint: https://docs.pytorch.org/docs/main/distributed/checkpoint.html
- DeepSpeed Universal Checkpointing: https://github.com/deepspeedai/DeepSpeed/blob/master/docs/_tutorials/universal-checkpointing.md
- vLLM train–inference numerical consistency (2026-10-14): https://vllm.ai/blog/2026-10-14-train-inference-consistency
- vLLM asynchronous training/inference: https://docs.vllm.ai/en/latest/training/async_rl/
- Arrow C Data Interface: https://arrow.apache.org/docs/format/CDataInterface.html
- DLPack C API: https://dmlc.github.io/dlpack/latest/c_api.html
- RFC 2119 / 8174 requirements language: https://www.rfc-editor.org/info/rfc2119/ and https://www.rfc-editor.org/info/rfc8174/
- RFC 8949 deterministic encoding: https://www.rfc-editor.org/rfc/rfc8949.html
- RFC 3552 security considerations: https://www.rfc-editor.org/info/rfc3552/
- in-toto attestations: https://github.com/in-toto/attestation/blob/main/spec/README.md

## Appendix A. Non-normative illustrative compatibility exchange

The following is **human-readable pseudo-JSON** showing the conceptual WCB values. It is not a canonical wire fixture and MUST NOT be hashed as though it were one.

```json
{
  "schema": "weft.context.compatibility-request.v1",
  "subject_kind": "kv_page",
  "subject_root": "<bytes32:provenance-bound-KV-root>",
  "consumer_kind": "attention_read",
  "consumer_contract_root": "<bytes32:exact-execution-contract>",
  "snapshot": "<bytes32:pinned-commit>",
  "requested_guarantee": "execution_equivalent",
  "compatibility_rule": "weft.kv.exact-prefix.v1",
  "capability_hash": null,
  "constraints": { "token_history_root": "<bytes32:prefix-history>" }
}
```

If the stored page came from the right model but a different attention mask, the evaluator must return `denied` or `unproven` with no reusable bytes, not a best-effort hit. A producer-generated matching hash without evidence that the execution contract captures the mask cannot prove execution equivalence.

## Appendix B. Editorial and technical open questions

1. Which existing WeftFS normative documents need to be formally incorporated verbatim in the first interoperability profile?
2. Should all new compatibility objects be persistent hashed records, or should short-lived judgments be treated as signed, expiring audit entries outside persistent identity?
3. What evidence suffices for `execution_equivalent` on a model's first supported operator subset?
4. Which failure codes can be made externally indistinguishable without undermining authorized diagnostics?
5. How are state retention and garbage collection coordinated with distributed writes and exported contextual references?
6. How can cross-backend evidence use original ContextRootIds without implying equality of unrelated snapshots?
7. What exact principal and capability binding does the first remote secure-transport profile use?
8. Which independently built runtime integration will serve as the first external standards reviewer and implementation?

These questions are intentionally visible. Concealing them behind aspirational completeness would make the standard less credible.
