# Independent context-plane verifier — results (Phase 1)

This document accompanies `verifier/` and
`bench/receipts/context-plane-verifier-20261114T063213Z.json` (+ `.log`). Every
number below is emitted by `scripts/perf_numbers.py` from that receipt.

<!-- BEGIN GENERATED:context-plane-verifier (scripts/perf_numbers.py) -->
At stamp 20261114T063213Z (revision f8f352ba9db3), the clean-room verifier ran **136** cases against the frozen corpus: **131** passed, **0** failed, **5** explicitly skipped. The negative suite passed **7/7**. The freeze matrix has **24** identity rows; the verifier is **2,675** lines of Python. Source: `bench/receipts/context-plane-verifier-20261114T063213Z.json`.
<!-- END GENERATED:context-plane-verifier -->

## What was run

`verifier/verify_corpus.py` is a Python 3 standard-library implementation
written from the published specification text. It does not import, link, call
through FFI, or shell out to the Rust, Go or Python Weft packages. It validates
the canonical bytes and identities of the frozen reader families:

- `weft.selector.v1` (`SelectorId`);
- `weft.context-root.v1` (`ContextRootId`), including the embedded selector,
  namespace equality and `selector_id` binding;
- `weft.causal.v1` (`CausalClosureId`), including strictly ascending unique
  nonzero arrays and the closure-superset rule;
- `weft.tensor.logical.v1` (`LogicalTensorRoot`) and the `values_hash`
  preimage when a vector supplies it;
- `weft.tensor.layout.v1` (structural, no identity);
- `weft.tensor.semantic.v1` and the three frozen policies, including the
  transformed-values hash;
- `weft.capability.v1`, `weft.capability.envelope.v1` (`EnvelopeId`,
  `sign_input`, Ed25519, grantee, expiry, revocation);
- the mode-19 `weft.ai.kv_prov.v1` frame and descriptor
  (`descriptor_root`), including header/trailer CRC32C, the 13-key descriptor,
  the 15-key mode-12 page map and every dimension cross-check;
- `weft.provenance.v1` (`ProvenanceId`) as a **self-test**, because no frozen
  `conformance/v1/provenance/` family exists. The receipt labels it
  `provenance_self_test_*`, never as corpus coverage.

Skips are explicit: two shard-set containers, one composite store-level vector,
and two historical vectors marked `superseded_by` (which the reference runner
also skips). A skip is not a pass.

## Negative suite

Seven cases prove the verifier refuses: a flipped byte, a noncanonical map
order, a duplicate key, an extra unknown key, a wrong hash domain, a truncated
record, and a record over a configured item bound. A further seven self-tests
exercise the provenance schema and identity, including unsorted inputs,
duplicate outputs, an unknown key, truncation, an oversized params map and a
wrong-domain digest.

## Findings

The spec-text gaps found while implementing are recorded in two places:

- `verifier/README.md` §"Rules found under-specified in the spec text";
- `spec/CONTEXT-PLANE-FREEZE.md` §6 (Phase 0 findings);
- the receipt's machine-readable `unspecified` array.

The most consequential are: the KV provenance document is not self-contained
(it defers the `page` map's types and cross-checks to WEFT-AI-CODEC §13), the
selector namespace grammar lives in the Context Root document, the provenance
record does not say whether unknown keys are rejected, and several identity
domains used by code and vectors are absent from the frozen domain table in
WEFTCODEC-FORMAT §1.1.

## Independence, stated plainly

The verifier was authored by the same organisation from the published text. It
is independent of the implementation code path but **not** an external
third-party implementation. RFC 0001 §15 asks for independently authored
implementations and both valid and adversarial fixtures; this work supplies the
first half and a clean-room reader from outside the reference tree, not an
external submission.

## Reproduce

```sh
python3 verifier/verify_corpus.py \
  --json bench/receipts/context-plane-verifier-<stamp>.json
```

or, as the registered court:

```sh
bash scripts/context-plane-verifier_court.sh
```
