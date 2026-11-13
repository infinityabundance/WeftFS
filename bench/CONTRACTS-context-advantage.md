# Context Advantage Court — frozen contract (R26, version 2)

This contract is written and committed **before** the court is run. It fixes the
arms, the invariants, the metric definitions, the declared tolerance for
fast-path neutrality, and the conditions that falsify the claim. Nothing here is
edited after a measurement; a changed rule is a new contract version and a new
receipt.

**v2 change log (R28, 2026-11-13).** One number was wrong in v1 §3 C3: it said
`1.25 ×` while the tolerance the harness declares and enforces in its receipt
(`fast_path.declared.bytes_ratio_max`) is **`1.10 ×`**, which is the value every
run since R26 has been gated on. The v2 change is a correction of that
typographical inconsistency in favour of the stricter, already-enforced number;
it tightens nothing at measurement time and loosens nothing. No arm, invariant,
metric, phase, gate or tolerance was changed, and the R26 and R27 receipts remain
valid under this text. See `bench/RESULTS-context-advantage.md` §6 for the
recorded reconciliation.

The court exists because of programme rule 14: the product is the context plane,
and a capability claim is only made when it beats **a strong application-level
baseline** built on the *same* backend, under the same contract. Native
checkpoint-write parity and every comparison against the Rust reference
filesystem are outside this document (rules 13 and 14).

Court harness: `examples/context_advantage_court.rs`.
Runner: `scripts/context_advantage_court.sh`.
Receipt schema: `weft.context_advantage.v1`.

---

## 1. The three arms

One backend — the local POSIX data plane of the WeftFS store
(`BackendKind::Local`), the same commit protocol, the same chunk size, the same
durability barrier — used three ways. Each arm gets its own store root so the
arms cannot share caches or commit chains.

### N — backend alone

Plain durable objects. A page is written as the exact page element bytes under
the name `<model>_p<prefix>_l<layer>.kv`. There is no descriptor, no attribute,
no index, no manifest, no sidecar and no scope record. A read is
`resolve name → read object`. The name carries the model and the position and
nothing else, which is the strongest key a system with no metadata can use.

### M — backend + a strong application-level metadata baseline

This is the arm that decides whether the contribution is real, so it is built
the way a competent team builds it today, with no handicap:

* **Payload**: one `zstd` level-3 blob per page (the ordinary optimization a
  team applies), plus one canonical JSON **manifest** object for the cache.
* **Index**: a real key→value index, `app.kv.index.v1`, keyed by
  `(model_root, adapter_set_root, tokenizer_root, attention_config, prefix,
  layer)` — exactly the five dimensions the operator brief names, plus the
  position. The value records the **complete** scope tuple (all eight
  provenance dimensions), the blob name, the blob content root, the byte length,
  `created_ns` and `last_used_ns`. The index is loaded into memory once on open
  and republished atomically after a batch of writes.
* **Validation before reuse**: a key hit is not a reuse. M re-reads the blob,
  recomputes its SHA-256, compares it with the recorded content root, and then
  compares **every one of the eight scope dimensions** in the recorded value
  against the requesting scope. Any disagreement is a miss (the caller
  recomputes); M never returns a page it has not validated.
* **TTL / eviction**: entries older than the TTL are not reused, and the index
  is LRU-capped. The court runs with the TTL and capacity large enough that the
  measured working set is not evicted; the eviction path is exercised by its own
  check and is reported.
* **Subset selection**: M's manifest declares, per artifact, its dependencies,
  and M selects a subset from its own manifest — it does not ask the store to
  plan.

M is allowed every optimization that does not change the contract: compression,
an in-memory index, batched index publication, content-digest validation. If M
matches WeftFS, that is the result and it is published as the headline.

### W — backend + the WeftFS context plane

The shipped context plane, with no special-casing:

* **Provenance-bound KV pages**: mode 19 (`weft.ai.kv_prov.v1`), written with
  `Store::write_kv_prov_page`, read with `Store::read_kv_prov_page` under a
  capability envelope whose `roots` name the expected provenance-bound
  descriptor id.
* **Scoped reads**: authorization precedes the read; a scope that does not name
  the object's descriptor id is `Unauthorized` and emits zero bytes.
* **Causal closure**: `weft.checkpoint.v1` manifests for tensor recovery, and
  causal-closure resolution over declared parent identities for the derived
  artifact.
* **Typed selectors** are available but are not required by any measurement
  below; nothing in this court depends on them.

---

## 2. Invariants held constant

| Invariant | Value | How it is held |
|---|---|---|
| Backend | `BackendKind::Local`, one root per arm | same `Options`, same `chunk_size` (65,536 B) |
| Durability | the store's ordinary commit protocol and fsync barrier | every arm publishes through `create_file` / `write_checkpoint`; no arm bypasses the commit |
| Logical objects | identical element bytes, identical names, identical count | the same `build` generator feeds all three arms |
| Cache capacity | no decoded-object cache (`cache_bytes = 0`), no single-flight | `Options` default for all arms |
| Concurrency | one process, one thread, one request at a time | the harness is single-threaded |
| Workload | 8 pages (4 prefixes × 2 layers), page = 64 rows × 512 elements × 2 B = 65,536 B | identical for all arms |
| Demand | the same 56 cross-scope probes, the same 8 compatible reads, the same 7 recovery outputs | identical counters per arm |
| Clock | fixed store timestamp (`fixed_timestamp_ns`) | identical |
| Court | `scripts/docker-court.sh bench`, 3 GiB / 3 GiB swap / 1024 pids / 4 cpus / bounded `/tmp` | never raised |

Deliberate, declared asymmetries (they are the thing under test, not confounds):

1. **Payload codec.** N stores raw bytes (it is the backend alone). M stores
   zstd-3. W stores the mode-19 frame, whose payload the wire format fixes as
   `huffman-canonical-v1` byte planes. Footprint is therefore a mixed number;
   the receipt also records, for every arm, the *same page* encoded as a mode-12
   frame, so the isolated provenance-descriptor overhead is separable from the
   codec difference and is reported separately.
2. **Metadata location.** M's metadata is a separate object graph (index +
   manifest) in the same store. W's metadata is inside the object and is part of
   its identity. This is the property under test.

---

## 3. The workload

### A — provenance-safe reuse

Eight pages are written under the **base scope** S0. S0 is a full provenance
tuple: model root, adapter set root, tokenizer `(scheme, vocab_size)`,
attention `(head_count, head_dim, sliding_window, rope theta/scale bits)`,
positional treatment, layout `(dtype, page_rows, layer_count)`, and a 32-byte
sharing-domain salt.

Seven **single-dimension variants** are then presented as requests. Each variant
changes exactly one dimension and leaves everything else — including the page
geometry and the payload bytes — identical:

| id | change | why it is hard to catch |
|---|---|---|
| `model` | model root B | the strongest key a metadata system has |
| `adapter` | one adapter root added | |
| `tokenizer` | scheme `spm` with the **same** `vocab_size` | a key on vocab size alone collides |
| `attention` | 16 heads × 32 dim instead of 8 × 64 | the tensor shape `[64, 16, 32]` is element-for-element the same |
| `positional` | `alibi` instead of `rope` | not one of the five dimensions named in the brief |
| `dtype` | `fp16` instead of `bf16` | same element width, same byte count |
| `salt` | a different sharing domain | not one of the five dimensions named in the brief |

For every variant × every one of the 8 positions (4 prefixes × 2 layers) = **56
probes**, the caller asks for a page it does not own, in a store that holds only
S0's page at that position. The correct outcome is a refusal that emits nothing.

Measured per arm:

* `incompatible_accepted` — probes that returned bytes (the safety result),
* `incompatible_wrong_bytes` — accepted probes whose bytes were not the
  requested scope's bytes,
* `incompatible_denied` — probes that emitted nothing,
* `denial_backend_bytes` and `denial_requests` — what it cost to refuse,
* `compatible_reused` / `compatible_reused_exact` — S0 pages served, and served
  byte-exactly,
* `hit_ms` (p50/p95 over 64 reads), `hit_backend_bytes`, `hit_requests`,
* `recompute_ms` — the measured cost of regenerating one page with the
  harness's scalar reference kernel (it *computes the page*, it does not sleep),
* `metadata_loss` — a second open of the same store with the arm's own metadata
  database absent: `reused_after_loss`, `exact_after_loss`. A capability that
  survives losing the metadata database is the only safe-reuse property N and M
  cannot express without re-implementing the same discipline.

### B — dependency-aware recovery

A checkpoint of **96 tensors** (64 × 256 BF16 = 32,768 B each), plus two
activation blobs (32,768 B each) and one derived artifact (8,192 B) that
declares the two activations as its causal parents. A downstream operation needs
**6 named tensors + the 2 activations + the derived artifact**.

* N has no metadata: it materializes all 96 tensors, the activations and the
  derived artifact.
* M selects its subset from its own manifest.
* W selects its subset from the `weft.checkpoint.v1` manifest and resolves the
  derived artifact's causal closure.

W may not win by materializing less than the contract permits: the demanded
output set is identical (6 + 2 + 1) and both M and W must return the same
logical bytes. A second, separate store copy is used for a **missing
dependency** probe in which one activation blob is removed; both M and W must
fail closed.

### C — fast-path neutrality

The critical path is "serve a page for a scope the caller already knows is
compatible": no contextual transformation, no search, no selection. 64 reads of
the 8 S0 pages, N/M/W.

**Declared tolerance, before measurement:**

* **C1** W fast-path p50 ≤ **2.0 ×** N fast-path p50.
* **C2** W fast-path p95 ≤ **2.5 ×** N fast-path p95.
* **C3** W backend bytes read per fast-path request ≤ **1.10 ×** N.
* **C4** W returns byte-exact pages on every fast-path read.

If any of C1–C3 is violated, the number is published as measured and the
tolerance is **not** widened.

---

## 4. Measurement rules

1. Seven repetitions of every phase. The arms are **interleaved** inside each
   repetition and the order rotates by one position per repetition, so no arm
   always occupies the same slot. The order of every repetition is recorded.
2. Every repetition is a fresh store root and a fresh process. Raw per-operation
   samples are retained in the receipt.
3. Wall time is `Instant` around the operation, single-threaded. CPU time is the
   process `utime + stime`. Peak RSS is `VmHWM`.
4. Backend bytes and requests are `Store::backend_stats()` deltas around the
   measured region; logical bytes are counted by the harness.
5. Nothing is measured on the host. Every run is `scripts/docker-court.sh bench`
   under the unchanged limits.
6. No number in prose is hand-written: `docs/PERF-NUMBERS.md` is generated from
   the receipt by `scripts/perf_numbers.py`.
7. A failing measurement is retained with its raw log; a receipt is never
   rewritten.

---

## 5. What would falsify the claim

The capability claim is **refuted** if any of the following holds on the
recorded receipt:

* **F1** W accepts one or more incompatible probes, or emits any byte on a
  denied probe.
* **F2** N does not accept incompatibly — i.e. the probe is vacuous and proves
  nothing.
* **F3** M (the governing arm) accepts no incompatible probe **and** retains
  exact reuse after loss of its metadata database. Then M has the same safety
  and the same availability, and W has no capability advantage.
* **F4** W materializes more bytes than M for the identical demanded output set,
  with no compensating verified-closure property that the court measured.
* **F5** C1–C3 are violated: the plane does not preserve the fast path, so the
  "no measurable regression" ordering in rule 14 fails.

A claim may be made only for what survives at least one of F1–F5 and only with
its receipt. The weakest admissible form of the claim is stated in §6.

## 6. The claim this court can support, in advance

Before any number, the only claim this design can support is:

> On one backend, with a strong application-level metadata baseline that is
> given compression, an in-memory index on the brief's five dimensions, full
> value validation, TTL/eviction and its own manifest-based subset selection,
> the WeftFS context plane rejects every cross-scope request with zero bytes
> read and zero bytes returned, and retains exact reuse after the metadata
> database is lost. The baseline matches the happy path and may match the
> refusal count; where it does, that is published as a tie.

That is a *materially more expressive and more reliable mechanism* claim, not an
"impossible without WeftFS" claim. It says nothing about fleet behaviour,
physical media, other backends, GPU prefill, or any reference filesystem.

## 7. What this court does not establish

* Not a fleet, cloud, multi-writer or power-loss result: one host, one process
  at a time, local POSIX data plane.
* Not a GPU or production-prefill result: `recompute_ms` is a scalar reference
  kernel that regenerates the page, and it is labelled as such everywhere it
  appears. It is not an optimized prefill and no ratio against it is a
  production claim.
* Not a codec comparison: M's zstd-3 and W's huffman-canonical payload are
  different by construction, so footprint is reported as a mixed number with the
  mode-12/mode-19 delta separated out.
* Not an adversarial-security result: the salt is an identifier dimension, not
  a secret; the court does not claim timing or access-pattern privacy.
* Not a test of an application baseline that embeds a canonical scope record in
  every blob and rebuilds its index by scanning. That design is available and is
  named in the results as untested.
