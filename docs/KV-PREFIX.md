# KV prefix cache (T2): storage-side primitive and reuse decision

**Status: implemented (Rust store, no engine integration).** This document is
normative for the `weft.kv.prefix.v1` record and for the reuse-vs-recompute
decision. It is deliberately narrow: WeftFS stores, verifies, and prices a
reusable KV prefix. It does not run an inference engine, does not integrate with
vLLM or SGLang, and makes **no time-to-first-token claim**.

## 1. What is stored

A prefix is one canonical WCB record plus one mode-12 `KV_CACHE` page object per
(layer, page) slot. The page objects already exist: they are the frozen mode-12
frames guarded by `weft.kv.capability.v1` descriptors from the KV-governance
subphase (`src/kv_capability.rs`). The prefix record is new.

```
/kv/prefixes/<name>            canonical weft.kv.prefix.v1 record
/kv/pages/p-<descriptor>-<sha> mode-12 KV_CACHE page objects
```

The page file name binds **both** the page descriptor id and the SHA-256 of the
exact payload, so two prefixes whose pages share metadata but not bytes can
never alias one object.

### `weft.kv.prefix.v1`

Canonical WCB map; unknown keys, missing keys, non-canonical bytes, and
inconsistent arithmetic are all `Corrupt`. The record id is
`H("weft/kv/prefix/v1", canonical_bytes)`.

| field | type | meaning |
|---|---|---|
| `schema` | text | `weft.kv.prefix.v1` |
| `tenant` | text | namespace the prefix belongs to |
| `model` | text | model identity (exact match) |
| `tokenizer_hash` | hex | declared tokenizer identity |
| `attention_hash` | hex | declared attention configuration identity |
| `dtype` | text | `bf16` / `fp16` / `fp32` |
| `head_count` | u64 | KV heads |
| `head_dim` | u64 | head width |
| `layer_count` | u64 | number of layers |
| `page_rows` | u64 | tokens per page |
| `page_count` | u64 | pages per layer (`ceil(tokens/page_rows)`) |
| `position_start` | u64 | always `0` |
| `position_end` | u64 | token length |
| `token_chain_root` | hex | `H("weft/kv/token-chain/v1", ...)` over the exact tokens |
| `parent` | hex or null | prefix id this record extends |
| `pages` | array of array of hex | `pages[layer][page]` object ids, ordered |
| `pinned` | bool | a pinned prefix cannot be replaced or deleted |
| `page_bytes` | u64 | logical bytes of every page in the prefix |
| `stored_page_bytes` | u64 | logical bytes of the pages this prefix adds |
| `reused_pages` | u64 | page slots reused from the parent |

The token chain is `root_0 = H(domain, "")` and
`root_{i+1} = H(domain, root_i || u64le(token_i))`. It binds the prefix to the
exact token sequence, not just its length.

`page_count` in an individual page descriptor is the count in the prefix that
**wrote** that page. A shared page keeps its original descriptor; the prefix
record is authoritative for composition. This is the only place page metadata is
allowed to differ between a page's producer and its consumer, and it is why the
gate checks layer, page index, token range, dtype, model, heads, and dim — but
not `page_count` — against the record.

## 2. Compatibility is exact; a mismatch fails closed

`KvPrefixRecord::compatibility(request)` compares, in order:

`tenant`, `model`, `tokenizer_hash`, `attention_hash`, `dtype`, `head_count`,
`head_dim`, `layer_count`, `page_rows`, then `position_start == 0`,
`position_end <= request.tokens.len()`, and the request's token-chain root at
`position_end` equal to the stored root.

Every field must match. There is no similarity metric, no nearest neighbour, no
cross-model or cross-tokenizer reuse. A failed lookup returns
`KvLookup::Recompute { reasons }` where each reason names the first mismatch for
that candidate; the caller recomputes. `kv_prefix_lookup` chooses the **longest**
compatible prefix and never partial-matches.

## 3. Dedup

* **Parent chain.** `kv put --parent NAME` reuses every page object of the
  parent verbatim and writes only the new pages. A parent must end on a page
  boundary; a partial last page is refused (`InvalidArgument`), never silently
  widened or padded. The child's token chain must extend the parent's, proven by
  the chain root at the parent length.
* **Content-addressed pages.** The page name binds descriptor + payload digest,
  so two prefixes that produce the same page share one object even without a
  parent relationship.
* **One commit.** Pages and the record are published together with
  `Store::group`; a failure publishes nothing.

## 4. Capability enforcement at `kv get`

`Store::kv_prefix_get` requires an authenticated `weft.capability.envelope.v1`:

1. no envelope, an invalid envelope, or an expired one → the single
   non-disclosing `Unauthorized` text `kv page is not available to this
   principal`;
2. the envelope's principal must equal the record's tenant and must grant the
   `kv.<tenant>` namespace;
3. every page is read through `Store::read_kv_page`, so the per-page descriptor
   must be in the envelope's `roots`;
4. after authorization, the decoded page metadata is compared with the record.

`kv list` is metadata (names, model, sizes, pin flags) and is not a data path.
`kv_prefix` (record load) is an internal/administrative path that never returns
page bytes.

## 5. Logical GC and pins

`weftfs kv gc` is a **logical** sweep of the KV namespace:

* roots: every page referenced by a live prefix record, plus any object held by
  an open-handle pin;
* candidates: KV page objects under `/kv/pages` that no live prefix names;
* `--apply` removes their directory entries (one commit per removal). Physical
  blob reclamation is still the store's mark-and-sweep `weftfs gc`, because
  retained commits and snapshots may still reference the blobs.

A `pinned` prefix cannot be replaced or deleted (`Busy`), so its pages are
always referenced by a live prefix. Pinning a *page* through an open handle is
also honoured (`kept_pinned` in the report).

## 6. The reuse-vs-recompute decision

```
T_reuse     = lookup + transfer + decode + placement
T_recompute = prefill_fixed_ns + tokens * prefill_ns_per_token
reuse       = (a compatible prefix exists) and (T_reuse < T_recompute)
```

* `transfer = page_bytes * transfer_ns_per_mib / 1 MiB`
* `decode   = CostTable(KV_CACHE).estimate_ns(page_bytes)` — the existing
  measured decode row (or the declared row, selectable in the document)
* `lookup   = max(declared lookup_fixed_ns, observed scan wall time)`
* `placement = placement_fixed_ns`

The observed scan time is folded in as a floor so the estimate cannot pretend a
10 ms pool scan is free. `observed_lookup_ns` is recorded separately.

### The calibration document

`bench/calibration/kv-prefill-v1.json` (`weft.kv.prefill.v1`) holds the declared
coefficients. `KvPrefillModel::declared()` is the same document in code, and a
test fails if they differ. The shipped values:

```
defaults.prefill_fixed_ns         2_000_000
defaults.prefill_ns_per_token       120_000   (≈8,333 tokens/s)
qwen2.5-0.5b@060db649              1_500_000 / 90_000
transfer_ns_per_mib                 250_000   (≈4 GiB/s)
placement_fixed_ns                    50_000
lookup_fixed_ns                       20_000
decode_cost_model                   measured
```

**This is a declared model. It was not measured against a real inference
engine.** 120 µs/token is a conservative small-model prefill estimate chosen to
make the decision legible; a real engine on real hardware will differ by a large
factor, and the decision must be re-tuned from measurements before anyone uses
it for a serving policy. The loader is strict: unknown keys, a missing
`defaults`, non-integer or out-of-bound coefficients, and an unknown cost-model
name are rejected, with no fallback.

## 7. Measured (court, synthetic)

Receipt `bench/receipts/kv-prefix-20261108T042229Z.json`; analysis in
`bench/RESULTS-kv-prefix.md`. Declared shapes: BF16, 14 layers, 2 KV heads, head
dim 64, 64 tokens/page → 3,584 B/token, 229,376 B/page across layers.

| row | result |
|---|---|
| one 512-token prefix | 1,835,008 logical B, 112 page objects, 1,931,305 physical B, 56.3 ms put |
| 1,024-token child | 3,670,016 logical B, **112 reused / 112 new** page objects, 1,835,008 new B |
| 100 prefixes over one 512-token system prompt | 286,720,000 logical B → **103,219,200 stored B, dedup ratio 0.64**; 6,412 objects / 110,569,036 B on disk; 4.24 s |
| `get` (authorized, 126–210 pages) | p50 **23.7 ms**, p95 36.6 ms, max 36.9 ms |
| compatibility scan over 101 prefixes | p50 **10.6 ms**, p95 10.7 ms |
| decisions (13 declared requests) | 6 reuse / 7 recompute; categories: model-mismatch 3, tokenizer 1, attention 1, token-chain 1, longer-than-request 1 |

The scan cost is the dominant reuse term and a real limitation: the lookup
re-reads and parses every prefix record. See §9.

## 8. Non-claims

* **No vLLM or SGLang integration.** No inference engine is in the court.
* **No time-to-first-token or throughput claim.** The court measures storage,
  capability verification, and decode; it does not measure serving.
* No GPU, TPU, multi-host, or fleet claim; one host, one court, warm cache.
* No claim that the declared prefill coefficients describe any real engine.
* The pages are synthetic (deterministic bit patterns), not captured model
  state.
* `kv get` returns decoded pages to the caller; it does not place them in an
  engine's cache.
* No multi-writer or online-GC claim beyond what `docs/FENCING.md` and
  `docs/store-status.md` already state.

## 9. Residual and the exact validating experiment

Residual:

* `kv_prefix_lookup` parses every prefix record (10.6 ms at 101 prefixes). A
  persisted KV index is not implemented.
* Page-level dedup requires the child's new pages to be supplied by the caller;
  the store does not derive them from a token stream.
* No Go parity for the prefix record, the chain, or the decision.
* `kv get` verifies the envelope once per page (the existing per-page KV rule),
  which dominates its latency.
* The decision is single-shot per request; there is no queueing, batching, or
  eviction policy.
* Partial last pages cannot be shared (by design); a parent must be
  page-aligned.

**The experiment that would validate this against a real engine:** load the
same token sequence through vLLM or SGLang with an identical tokenizer and
attention configuration, export or point at its KV cache pages for the prefix,
store them as `weft.kv.prefix.v1` pages, then for a request that extends the
prefix (a) import the stored pages into the engine's cache and (b) recompute the
prefix, and compare time-to-first-token and output identity. Until that runs,
this subphase is a storage-side primitive with a declared decision model.

## 10. Reproduction

```sh
bash scripts/docker-court.sh rust "cd /work && cargo test --test kv_prefix -- --test-threads=1"
bash scripts/docker-court.sh bench "cd /work && cargo build --release --example kv_prefix_bench \
  && .docker-cache/target-rust/release/examples/kv_prefix_bench --out /work/bench/receipts/kv-prefix-<stamp>.json"
```

CLI:

```sh
weftfs --store S kv put --name sys --model m --tokens 2048 --page-rows 64 --layers 14 --heads 2 --head-dim 64
weftfs --store S kv put --name sys+user --parent sys --tokens 4096 ...
weftfs --store S kv list --json
weftfs --store S kv plan --model m --tokens 4096 --page-rows 64 --layers 14 --heads 2 --head-dim 64 --json
weftfs --store S kv get --name sys --envelope grant.bin
weftfs --store S kv gc --apply --json
```
