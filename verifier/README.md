# Independent context-plane verifier (Phase 1)

A clean-room semantic verifier for the frozen WeftFS context-plane records and
for draft RFC 0001's reader role. It is written in **Python 3, standard
library only**, and it does **not** import, link, call through FFI, or shell out
to `weftcodec`, `weftfs`, `WeftCodec-Go`, `WeftFS-Go` or `python/weftcodec`.

## What it implements, from the published text

| File | Content |
|---|---|
| `blake3.py` | Unkeyed BLAKE3-256, pure Python, full tree construction |
| `hashing.py` | `H(domain, data) = BLAKE3-256(u32le(len(domain)) ‖ domain ‖ data)` |
| `wcb.py` | Canonical WCB decode/encode: tags `0x00..0x09`, minimal varints, encoded-key map order, duplicate rejection, depth/item/string bounds, UTF-8, trailing-byte rejection |
| `ed25519.py` | RFC 8032 verification, used only by the capability-envelope check |
| `records.py` | Record validators and the `UNSPECIFIED` findings list |
| `verify_corpus.py` | Manifest driver, negative suite, provenance self-test, receipt writer |

Record families validated:

- **selector** — `weft.selector.v1`, `SelectorId`;
- **context root** — `weft.context-root.v1`, embedded selector binding,
  `ContextRootId`;
- **causal closure** — `weft.causal.v1`, sorted/unique/nonzero/superset rules,
  `CausalClosureId`;
- **logical tensor** — `weft.tensor.logical.v1`, dtype/shape/element-count,
  `LogicalTensorRoot`, and the `values_hash` preimage when the vector supplies
  it;
- **tensor layout** — `weft.tensor.layout.v1` structural rules (no identity);
- **semantic root** — `weft.tensor.semantic.v1` and the three policies
  (`none`, `mantissa_truncate`, `block_quantize`), including the actual
  transformed-values hash;
- **capability** — `weft.capability.v1` identity, `weft.capability.envelope.v1`
  identity, Ed25519 signature, grantee, expiry and revocation;
- **KV provenance descriptor** — mode-19 frame structure (header/trailer
  CRC32C), the 13-key descriptor, the 15-key mode-12 page map, all dimension
  cross-checks, `payload_hash` vs the frame `content_hash`, and
  `descriptor_root`;
- **provenance** — `weft.provenance.v1` schema and `ProvenanceId`, as a
  **self-test**: no frozen `conformance/v1/provenance/` family exists in the
  manifest, and this README does not pretend otherwise.

It does not implement: checkpoint manifests, transport containers, engram
shards, retention, multi-writer records, selectors' evaluation semantics,
Context Root resolution against a store, or any network/ABI surface. Those are
listed as gaps in `conformance/REQUIREMENTS.md` and
`spec/CONTEXT-PLANE-FREEZE.md`.

## Independence, stated honestly

This verifier was **authored by the same organisation** from the published
specification text. It is independent of the **implementation code path**
(no shared code, no FFI, no subprocess call into a WeftFS binary), but it is
**not** an external third-party implementation. RFC 0001 §15 asks for
independently authored implementations; this satisfies the "independent
implementation from outside the reference code path" half and does **not**
satisfy the "external third party" half. No claim to the contrary is made.

The BLAKE3 implementation is independent of the `blake3` PyPI package; its
known-answer vectors (including a multi-chunk vector) were produced by the
official package in the python court and recorded as constants, not recomputed
by this module.

## Run

```sh
python3 verifier/verify_corpus.py \
  --json bench/receipts/context-plane-verifier-<stamp>.json
```

Exit status is `0` only when every case it ran passed and the negative suite
passed. Skips are counted and never treated as passes.

The script writes the same text to `<stamp>.log` and a machine-readable receipt
with one entry per vector, the negative suite, the provenance self-test, the
manifest aggregate, the verifier source digest and the revisions.

## What the current corpus covers

The manifest holds the authoritative count. At the revision this directory was
added, the verifier ran **136 cases: 131 pass, 0 fail, 5 skip**, plus a **7/7**
negative suite and a **7/7** provenance self-test. The skips are:

- two shard-set containers (`tensor/401`, `tensor/402`) — outside the reader
  families;
- `conformance/context/v1/causal_versions.json` — a composite store-level
  vector, not one canonical record;
- `codec/017_unknown_mode_reserved19.json` and
  `kv-prov/112_reserved_mode_20.json` — historical vectors marked
  `superseded_by`, which the reference runner also skips.

## Rules found under-specified in the spec text

These are findings, not workarounds. Each one required a decision that the
published text does not make; the receipt records them in `unspecified`.

1. **Selector value typing.** `WEFT-SELECTORS.md` §2 permits a WCB scalar and
   says a timestamp uses tag `0x09` "when the runtime value is temporal", but
   the text grammar has no timestamp literal and the document does not say
   whether a decoded `attr_eq`/comparison timestamp is accepted. This verifier
   accepts timestamps.
2. **Selector namespace grammar.** It is defined in
   `WEFT-CONTEXT-ROOT.md` §3.1, not in the selector document. A reader that
   implements only the selector spec cannot validate the namespace.
3. **Context Root causal ordering.** The record table says "strictly ascending,
   unique, nonzero"; the canonical array rules and the closure-superset rule
   live in `WEFT-CAUSAL.md` §1.1. The Context Root document is not
   self-contained.
4. **KV provenance `page` map.** `WEFT-KV-PROVENANCE.md` §3.2 says the `page`
   map is the mode-12 descriptor "verbatim" but does not restate its 15 keys,
   their types, or the cross-checks. This verifier had to read
   `WEFT-AI-CODEC.md` §13.2/§13.4.
5. **Provenance unknown top-level keys.** `WEFT-PROVENANCE.md` §2 lists fields
   but never says whether an unknown key is rejected. Every other frozen
   record schema says so explicitly. This verifier rejects it
   (`NonCanonical`).
6. **Provenance optional input hint.** The spec shows `inputs[].object` as an
   optional hint but does not say whether an absent hint is encoded as `null`
   or by omitting the key. This verifier accepts both and does not normalise.
7. **Identity domains absent from the frozen table.**
   `WEFTCODEC-FORMAT.md` §1.1 omits `weft/checkpoint/manifest/v1`,
   `weft/index/state/v1`, `weft/index/candidates/v1`, `weft/pack/*`,
   `weft/load/*` and `weft/mw2/key/v1`, all of which code and vectors use.
8. **Quantized logical `values_hash`.** `WEFT-TENSOR.md` §3 defines the logical
   record, but the `q8_0`/`q4_0` `values_hash` preimage (values byte followed by
   per-block scale/zero-point bytes) is in §2.1. A logical-record-only reader
   cannot verify the hash without the tensor layer.

Two smaller observations, recorded for completeness:

- The mode-19 descriptor's `tokenizer_root` hashes only
  `{scheme, vocab_size}`. This verifier confirms the gap rather than hiding it;
  see `spec/CONTEXT-PLANE-FREEZE.md` §4.
- Capability verification needs Ed25519, which the specification names but
  whose strict-key rules are described only as "strict verification path
  (reject small-order/non-canonical keys)". This verifier rejects non-canonical
  `y`, the identity key and the order-2 point; it does not claim a complete
  small-order classification.

## Files

```
verifier/
  README.md
  blake3.py
  hashing.py
  wcb.py
  ed25519.py
  records.py
  verify_corpus.py
```

No file in this directory is imported by the Rust, Go or Python WeftFS
implementations, and no implementation imports it.
