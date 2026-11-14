---
status: draft candidate proposal — NOT an IETF RFC, NOT ratified, NOT an implementation claim
date: 2026-11-13
placed at revision: f8f352ba9db37dfa08d9305fe3ba7f9288d6be46 (WeftFS staging head this file was added on top of)
---

# Language-neutral API and candidate C ABI contract

**WeftFS open context plane · Draft RFC 0001 · 2026-11-13**

This is the proposed implementer-facing interface. The C header in `include/weft_context_plane.h` is a compilable **interface declaration only**; there is no implementation, shared library, exported symbol, existing integration, or network endpoint included in this package.

## 1. General call model

Every API call consists of a canonical WCB `weft.context.request.v1` envelope, an operation name, and a canonical WCB `params` map. Responses are canonical WCB `weft.context.response.v1` envelopes. All `bytes32` values are raw 32-byte arrays on the wire, never lowercase hex strings. Structured examples in Markdown are illustrative and do not claim to be golden wire vectors.

A successful invocation returns an outcome envelope. A compatibility `denied` or `unproven` verdict is a **successful API invocation**, but does **not** authorize use. A transport or C ABI failure yields no valid result. Request ID is an opaque correlation nonce, not an authorization token. The local session must be bound to a trusted context and authenticated principal; callers cannot elevate authority by putting `principal`, `uid` or a `CapabilityHash` in `params`.

Parameter maps described below are exact proposed shapes; unsupported keys or values are `NonCanonical`/`UnsupportedProfile` as appropriate. For all roots and lists: no zeros, duplicates or unsorted entries where a canonical order is required, checked limits before allocation, and schema-specific hash domains. Per-operation security checks are mandatory before returning any existence evidence.

## 2. `describe`

**Role:** Reader. **Parameters:** `protocol_major:u64`, `minimum_minor:u64`.

**Result:** `protocol_major:u64`, `protocol_minor:u64`, `wire_format:text` (frozen value `weft.wcb.v1` proposed for this protocol negotiation), `roles:[text]`, `profiles:[map]`, `max_request_bytes:u64`, `max_response_bytes:u64`, `hash_domains:[text]`, `conformance_manifest_root:bytes32|null`. Each profile map: `name:text`, `version:u64`, `status:text` (`implemented`, `experimental`, `specified_only`), `assurance:text` (`reference_tested`, `independently_tested`, `unverified`). Lists are sorted by bytewise UTF-8, no duplicates.

**Invariant:** Feature advertised as `implemented` is not necessarily `independently_tested`; clients must not interpret those strings as a signature or certification. A required unsupported profile fails before work begins.

## 3. `validate_record`

**Role:** Reader. **Parameters:** `record:bytes`, `expected_id:bytes32|null`. The record's schema must determine its appropriate hash domain from the registered schema table; the client is not allowed to select an arbitrary hash domain.

**Result:** `schema:text`, `computed_id:bytes32`, `canonical:bool` (always true on success), `validation_scope:text` (`record_integrity`, or a versioned stronger check supported by that schema). The validator checks structure, exact byte canonicality, field rules, identity construction and optional expected ID.

**Failure:** `UnknownSchema`, `NonCanonical`, `Corrupt`, `UnsupportedVersion` or `LimitExceeded`. A successful integrity check MUST NOT be described as proof that provenance or computational execution claims are true.

## 4. `resolve`

**Role:** Resolver. **Parameters:** `context_root_record:bytes`, `expected_context_root_id:bytes32`, `capability_record:bytes|null`, `max_results:u64`, `cursor:bytes|null`. `capability_record` provides a record to verify and potentially narrow rights, **not authentication**. The authenticated principal comes from the session. The exact mapping of capability records to the store's authorization and revocation system follows the frozen capability profile.

**Result:** `context_root_id:bytes32`, `snapshot:bytes32`, `selector_id:bytes32`, `matched_count:u64`, `returned_count:u64`, `complete:bool`, `cursor:bytes|null`, `results:[map]`, `evidence:[map]`. Each result map MUST include `object_id`, `version_id`, `content_id` as raw bytes32 and any typed logical roots only if independently validated. The authorized pre-limit match count cannot be replaced by the count returned after `limit` or pagination. If the profile cannot compute the pre-limit count, it must use a separate explicit `count_known:false` result schema version; this v1 proposal does not permit a guessed count.

**Failure:** `SnapshotMissing`, `Ambiguous` for singular access profiles, `Unauthorized`, `BudgetExceeded`, `Corrupt`, `NonCanonical`, `UnsupportedVersion`. A resolver MUST NOT evaluate against the current head if the named snapshot is unavailable.

## 5. `evaluate_compatibility`

**Role:** Compatibility evaluator. **Parameters:** `request_record:bytes` containing exact `weft.context.compatibility-request.v1`; `evidence_roots:[bytes32]` (sorted/unique); `evidence_records:[bytes]` canonical records that can be validated under the registered rule. This is not a mechanism to inject trust: the evaluator classifies the provenance and validity of each evidence item.

**Result:** `decision_record:bytes` containing exactly one `weft.context.compatibility-decision.v1` as specified in the RFC. A decision with `verdict="denied"` or `verdict="unproven"` is delivered as an ordinary `outcome="ok"` response and cannot authorize reuse. The decision references the original request hash, rule version, evaluated snapshot and the complete supporting evidence or missing requirements.

**Rules:** The evaluator MUST NOT invent missing operator semantics, missing tokenizer contents, missing attention masks or an unsupported numerical execution contract. It MUST NOT return `exact` on a merely successful byte decode when `execution_equivalent` was requested. Proposed `convertible` results must name an approved transformation. Proposed `repair_required` results must name the repair preconditions. `approximate` results require explicit consumer opt-in in a future profile and cannot satisfy an exact request.

## 6. `dependency_closure`

**Role:** Resolver. **Parameters:** `snapshot:bytes32`, `roots:[bytes32]` (sorted unique), `relations:[text]` (registered relation predicates), `known_verified_roots:[bytes32]` (sorted unique), `max_nodes:u64`, `max_depth:u64`. Every query is authorized before traversal.

**Result:** `snapshot:bytes32`, `roots:[bytes32]`, `needed:[bytes32]` (sorted unique), `already_verified:[bytes32]`, `complete:bool`, `truncated:bool`. If a required ancestor is unauthorized, no externally visible field may reveal its identity or count; return the nondisclosing error instead of fabricating a complete closure. A cycle in causal relations is legal; a reconstruction dependency cycle is not.

## 7. `plan_materialization`

**Role:** Materialization planner. **Parameters:** `intent_record:bytes` containing `weft.context.materialization-intent.v1`.

**Result:** `intent_root:bytes32`, `snapshot:bytes32`, `complete:bool`, `items:[map]`, `dependencies:[bytes32]`, `missing:[map]`. Each item describes a typed logical root/region, a verified representation reference and a declared transformation if needed. All lists have a normative deterministic order under their profile. A plan by itself is **not** materialization, execution or verified delivery. Actual reads must authenticate and verify the requested state at the pinned snapshot.

## 8. `verify_execution_cut`

**Role:** Optional execution-cut verifier. **Parameters:** `cut_record:bytes`, `target_execution_root:bytes32`, `required_guarantee:text`.

**Result:** `continuation_status:text` (`complete_for_initialization`, `complete_for_resume`, `partial_only`, `inconsistent`, `unproven`), `verified_components:[map]`, `required_transformations:[map]`, `missing_requirements:[text]`, `evidence:[map]`. A `complete_for_resume` decision MUST explicitly identify its exact execution contract and comparison semantics; a weights-only checkpoint cannot satisfy optimizer/data/RNG completeness conditions.

## 9. The C ABI

The header defines one exported discovery function and a function table. There are no static public Rust structures, no private allocator assumptions, and no concurrent session use without negotiation. A caller compiles against the header, obtains the exported `weft_context_get_api_v1` symbol from an implementation library, and sends canonical WCB records via `invoke`.

The `weft_context_api_v1` table includes `open`, `close`, `invoke`, `buffer_view`, and `buffer_release`. The library owns its buffers. Input byte slices are borrowed for a call. Results are immutable borrowed views valid only while the owned output buffer exists. Failures in an operation are carried as canonical error envelopes, while failures in ABI delivery produce a nonzero ABI status and no trusted response. Every callback is required to keep language runtime exceptions and panics inside the library.

The C surface is intentionally small. The ABI is not required to handle tensor memory, GPU device pointers, placement, scheduling or transport. Existing interop interfaces such as DLPack and Arrow remain appropriate for data exchange. A separate network-transport profile is required before claiming secure remote API compatibility.

## 10. Compatibility and stability obligations

A published implementation MUST test that old caller tables cannot be overwritten; unknown minor versions cannot be accepted without negotiation; older client binaries use unchanged offsets; buffers may be released after a session closes; unload order is correct; and Rust/C++/Go language-specific memory managers never free foreign allocations. An implementation that supports a newer ABI major must export a **new major symbol**, not silently reinterpret the v1 table.

In the first candidate version, all calls are synchronous and require small bounded envelopes. Large results use an authenticated, snapshot-bound pagination scheme. The decision to add async callbacks, streaming, shared-memory buffers or remote transport must be a separate versioned profile after clear ownership and cancellation rules are established.

## 11. Unresolved specification choices requiring independent review

A real standard must not leave these ambiguous: `describe` schema/version registry names; exact representation of the optional per-profile assurance metadata; authorized principal and revocation binding over the chosen remote transport; cursor schema and authentication; the full registered KV execution contract; complete canonical WCB schema of every proposed new record; and the set of independently verifiable evidence classes. Until those are frozen and tested, the new API is a *reviewable candidate*, not a wire-compatible ecosystem contract.
