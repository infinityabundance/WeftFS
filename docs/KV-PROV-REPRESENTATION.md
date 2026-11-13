# Provenance-bound page representations — finding and design proposal (R28)

Status: **proposal, not implemented.** No wire byte, frame, descriptor, vector
or store format changed in R28. This document exists because the task that
produced it asked for exactly that: establish whether an existing representation
already gives a cheap-decode provenance-bound page, and if not, report a design
rather than change a frozen format blind.

Measured evidence for everything below (the ledger, the representation floor and
the storage numbers) is in
[`bench/RESULTS-context-advantage.md`](../bench/RESULTS-context-advantage.md)
§6, generated from the receipt named there. This document deliberately carries
no performance number of its own.

---

## 1. The question

A hot, immediately-consumed KV page wants a cheap decode; a cold page wants a
compact payload. Both must carry the same mode-19 provenance identity (model
weights root, adapter set, tokenizer, attention configuration, positional
treatment, cache layout, sharing salt, extension dimensions, payload hash) and
both must be refusable *before a byte is read* when the scope does not match.
Does the tree already offer a representation that satisfies both?

## 2. The finding — no, and the reason is structural

1. **Mode 19's payload is mode 12's payload, verbatim.**
   `spec/WEFT-KV-PROVENANCE.md` §3: "`payload` is byte-for-byte the mode-12 page
   payload: the canonical row-major little-endian element bytes split into byte
   planes, each coded with its own `huffman-canonical-v1` stream". The mode-19
   descriptor embeds the frozen mode-12 page descriptor **verbatim**, including
   its `plane_codec` key.
2. **Mode 12 fixes `plane_codec` to one value.**
   `spec/WEFT-AI-CODEC.md` §13.2: `plane_codec` is "exactly
   `huffman-canonical-v1`"; §13.3 defines the payload as per-plane Huffman
   streams; §13.4 rejects any other value as `Malformed`. In code that is
   `KV_CACHE_PLANE_CODEC` and the guard in `WeftCodec/src/kv_cache.rs`
   (`plane_codec ... is not implemented`).
3. **No other mode carries the eight dimensions.**
   `adapter_roots`, `tokenizer`, `attention`, `positional` and `salt` exist only
   in `WeftCodec/src/kv_prov.rs` (mode 19) and its conformance/CLI plumbing.
   Modes 6/9–18 carry tensor, tile, weight-delta, KV-page or transport identity;
   none of them binds a page to a model, an adapter set and a sharing domain.
4. **Tensor and raw store strategies are not a substitute.** `write_tensor_file`
   with `strategy: raw` carries a *tensor* identity (`logical_root`) in reserved
   `weft.tensor.*` attributes. That root has none of the eight provenance
   dimensions, so it cannot answer "is this page valid for this scope".

Consequence: a page with a raw payload cannot carry mode-19 identity today,
because the identity include the payload codec. Storing the same page both ways
would produce two descriptors and two roots, and a scope decision taken on the
frame root would treat them as different pages.

## 3. What a design has to solve

Not just "make the payload a copy". Three things must hold together:

* **R1 — cheap decode.** A hot page's payload step should be a bounds-checked
  copy, not an entropy decode.
* **R2 — identical provenance identity.** The *scope* identity of a page must be
  the same whatever representation carries it. `payload_hash` already is:
  `H("weft/chunk/v1", page element bytes)` hashes the elements, not the frame.
  The frame root is not, because it covers `plane_codec` and `payload_len`.
* **R3 — no frozen vector changes, and old readers fail closed.** A reader that
  does not know the new representation must reject the frame, never
  mis-decode it.

## 4. Design A (recommended): an additive `plane_codec` value plus a derived `scope_root`

**Wire.** No new field. The existing `page.plane_codec` text field gains one
additive value, `raw-le-v1`. For that value:

* `payload` is the page's canonical row-major little-endian element bytes,
  exactly `page_bytes` long; `payload_len == page_bytes` and
  `decoded_len == page_bytes`;
* decoding is a length check plus a copy — no tables, no per-plane streams;
* every existing normative check stays: descriptor cross-checks (§2.1 of
  WEFT-KV-PROVENANCE), `payload_len`, `decoded_len`, the reconstructed
  `payload_hash`, the frame `content_hash`, and the descriptor root. In this
  representation `payload_hash == content_hash` by construction, and the
  encoder must still verify it.

**Identity.** Add a *derivation*, not a field:

```text
scope_root = H("weft/kv/scope/v1", WCB(model_root, adapter_set_root,
              tokenizer, attention, positional, layout, salt,
              extensions, payload_hash))
```

It is computed from a mode-19 descriptor that already exists, so it needs no
wire change, and it excludes `plane_codec` and `payload_len` by construction, so
it is representation-independent: the same page under the same scope has the
same `scope_root` as a Huffman frame and as a raw frame. Scope decisions and
capability grants name `scope_root`; the frame root still names one exact stored
representation. This is the property that makes "identity identical either way"
*checked* rather than asserted.

**Compatibility.** `raw-le-v1` is an additive value in a field whose only
current value is `huffman-canonical-v1`; nothing about existing frames changes.
A reader that does not implement it returns `Malformed` (unknown plane codec) —
fail closed, which is the rule for an unknown value in a validated text field.
`raw-le-v1` must be added to mode 12 and mode 19 together, because mode 19's
payload is mode 12's payload; the reserved-value rejection stays.

**Costs the design must publish, not assume.**

* *Storage.* A raw payload stores the page at its full logical size while the
  Huffman planes compress it. The delta is a size, per page, and it is the
  price of the cheap decode; it must be reported beside every latency win.
* *Selection.* The choice belongs to the artifact's context (ephemeral/hot page
  → raw; cold page → Huffman), never a default, and the receipt must record the
  mix it chose.
* *Scope.* Whether it closes the declared fast-path tolerance is **not
  established** by the R28 measurements: the store path's Huffman cost is only
  bounded between the in-memory plane measurement and the ledger row that also
  contains the page hash, the descriptor parse and the descriptor identity. The
  design would have to be implemented and measured as its own court before any
  claim, and the per-read provenance checks it does **not** remove are named in
  RESULTS §6.3.

## 5. Design B (named, not recommended): raw object plus a store-level descriptor

Store the page as an ordinary chunked object whose bytes are the raw elements,
and carry the mode-19 descriptor in the reserved `weft.kv.descriptor` attribute
that the store already writes, with a v2 capability descriptor naming
`scope_root` and the representation. No codec, vector or spec-mirror change.

It is cheaper to build, and it keeps the descriptor in the store's own object
record, so it survives application-metadata loss exactly as today. It is not
recommended because it gives up the property mode 19 exists for: a descriptor
*outside* the frame is invisible to a bundle of frames, a foreign index, or any
consumer that resolves a page by content hash alone — the silent-reuse defect
`spec/WEFT-KV-PROVENANCE.md` §1 closes. It also splits the identity model in
two, which is the kind of ambiguity this programme refuses.

## 6. What would be built, in order, if accepted

1. `WEFT-AI-CODEC.md` §13.2/§13.3 and `WEFT-KV-PROVENANCE.md` §3/§4 amended
   additively (new value, unchanged existing text), exported to all three spec
   mirrors.
2. Rust and Go `kv_cache`/`kv_prov`: the new value in the encoder and decoder,
   `scope_root` derivation, and the reserved-value reject table extended.
3. Conformance vectors (valid plus reject in both codec repos, byte-identical),
   manifest and coverage regenerated; no existing vector edited.
4. Store: representation choice from the declared context, `scope_root` in the
   capability descriptor derivation, and a `weft plan`-style report of the
   storage delta.
5. A court: W_raw and W_huffman on the same pages, reporting p50/p95 against N
   and M, the storage delta, the unchanged refusal (zero bytes read, zero bytes
   returned), the unchanged post-metadata-loss reuse, and `scope_root` equality
   across representations.

Until step 5 exists, no fast-path claim is made and the court continues to
measure the shipped Huffman representation.
