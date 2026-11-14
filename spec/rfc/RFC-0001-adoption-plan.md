---
status: draft candidate proposal — NOT an IETF RFC, NOT ratified, NOT an implementation claim
date: 2026-11-13
placed at revision: f8f352ba9db37dfa08d9305fe3ba7f9288d6be46 (WeftFS staging head this file was added on top of)
---

# WeftFS open context plane — adoption and implementation plan

**Companion to draft RFC 0001 · 13 November 2026**

The objective is not to convince a frontier organization to replace its filesystem. It is to make a narrowly defined compatibility question answerable across a storage backend and an AI runtime, with the exact same result produced by an independent implementation.

## Phase 0 — reconcile and freeze existing primitives

Treat the current WeftFS format specifications as the *existing core*, not material to rewrite for presentation. Audit the exact source revision for:

- frozen `ContextRootId`, selector, causal and logical-tensor hash domains and WCB bytes;
- authoritative mode registry including values 19 and 20 (currently contradictory between documents);
- mode-19 KV provenance limitations, especially actual tokenizer content identity versus scheme/vocabulary size;
- capability-hash versus authentication distinctions and nondisclosure rules;
- exact existing cross-language golden vectors and their checksums;
- evidence of what Rust, Go and independent readers implement versus what remains designed only.

**Ship gate:** a public matrix of canonical records/versions/hash domains with no contradictory normative documents and a full two-language vector run. Do not renumber, relabel or rehash frozen identities.

## Phase 1 — independent semantic verifier

Create a small library or standalone binary that reads the published protocol and validates identities, schemas, canonicality, pinned snapshots (when supplied a read-only store view), and errors without calling the WeftFS Rust/Go code through FFI.

**Ship gate:** third-party or clean-room reader passes all reader conformance cases, including malformed and adversarial inputs; it correctly rejects forged hash fields, noncanonical order and ambiguous/missing snapshots. Publish code, commands and frozen outputs. This is not dependent on a deployed data plane.

## Phase 2 — narrow, complete KV exactness rule

Work with one concrete inference runtime to freeze the actual execution dependencies for an exact prefix case. Export: model-weight root and adapter identity, *complete tokenizer/preprocessing identity*, attention masks/positions, exact token history, KV geometry and numerical execution constraints. Define the scope in which such a witness actually establishes reuse validity; any missing dimension yields `unproven`.

**Ship gate:** the exact-prefix court admits valid cases and refuses every incompatible fixture, including same-size/different-tokenizer cases. Independently verify the decision from canonical records. Include a deliberately flawed evaluator that passes ordinary hash tests but fails causal/attention/tokenizer tests.

## Phase 3 — two runtimes and one real backend boundary

Connect two independently maintained inference runtimes or a runtime and an independent verifier to the same context-plane records. Use one existing mature storage backend or object backend for byte delivery; the context plane must not replace its transfer path. Demonstrate one valid reuse and several correctly refused reuse cases, with identical semantic results and a documented physical layout compatibility boundary.

**Ship gate:** no environment-specific private interpretation is necessary to decide the published supported rule. A missing runtime rule is refused instead of silently mapped to the nearest known one. The official claim names the exact runtime versions, supported layers and evidence scope.

## Phase 4 — tensor and checkpoint compatibility

Expose the existing `LogicalTensorRoot` through the read-only API and verify topology-independent values, then publish a separate execution-cut contract through PyTorch Distributed Checkpoint or DeepSpeed. Capture the full training state that a particular runtime requires at a coherent barrier, including optimizer and data-pipeline state.

**Ship gate:** one uninterrupted-vs-resumed deterministic next-step comparison and adversarial omission cases. A model-weights-only checkpoint is accepted only for initialization, never promoted to exact training resumption.

## Phase 5 — evidence, governance and ecosystem

Publish a review process for schema and rule changes, release-state registry, independent verifier reports, security disclosures and breaking-change protocol. Invite interested maintainers of vLLM, SGLang, PyTorch, LMCache, NVIDIA Dynamo, DLPack/Arrow, and storage backends to critique the semantics. An official standards-body process is optional and requires a separate institutional commitment; the RFC itself is neither official nor ratified.

**Ship gate:** more than one independent implementer, unchanged frozen vectors for published identities, coherent backwards compatibility policy, public security assumptions and an explicit contributor/patent policy approved by the project owner.

## Three flagship demonstrations

**1. Two indistinguishable byte streams, only one permissible reuse.** Same KV frame payload and geometry, but a different model adapter/tokenizer/attention context. Ordinary content lookup returns both. A correct context-plane evaluator refuses the incompatible candidate *before* materializing it and supplies machine-verifiable reasons. A positive valid-prefix candidate must succeed under the same engine.

**2. Resharded tensor, unchanged logical identity.** Store a real checkpoint in two physical layouts and backend configurations. Prove equal canonical logical roots without suggesting identical commit histories. Flip one scale/value or omit a shard; refuse. Independently reconstruct selected tensor regions and verify their declared scope.

**3. Valid model bytes, invalid continuation.** A weight-only checkpoint passes byte verification but fails exact training continuation because the required optimizer/data/RNG state is missing. Supply the complete state and a coherent capture barrier, then demonstrate the declared next-step equivalence. The distinction must be visible to a consumer through the public API.

## Editorial standard

Write specifications as if a skeptical engineer will implement them from the text alone. Every nontrivial guarantee needs an explicit request, exact allowed input, normative behavior, a dangerous near-miss, a typed refusal, an implementer-visible fixture, and a repeatable negative test. Avoid invented umbrella names, untested speed claims and statements that a compatible data format automatically guarantees compatible computation.
