# Context Advantage Court — results (R26, extended by R27 and R28)

One mature backend, with and without the WeftFS context plane, holding the
hardware, durability, workload, caching policy, concurrency and the available
optimizations constant. Frozen contract:
[`bench/CONTRACTS-context-advantage.md`](CONTRACTS-context-advantage.md)
(version 2; see §6 for the one correction), committed before the court produced
any measurement.

Receipt: `bench/receipts/context-advantage-20261113T070714Z.json` (R28, stamp
`20261113T070714Z`, WeftFS `923c7bc4d187`), superseding the R28 runs
`...T064743Z`, `...T064629Z` and `...T064310Z`, the R27 receipt
`context-advantage-20261113T061907Z.json` and the R26 receipt
`context-advantage-20261113T054528Z.json`; all are retained, and §12 lists the
runs that are not cited. The reduced three-repetition configuration the
aggregate court runs is `context-advantage-20261113T070956Z.json`. Every number
below is generated from the current receipt by `scripts/perf_numbers.py`.

---

## 1. The result

<!-- BEGIN GENERATED:context-advantage-verdict (scripts/perf_numbers.py) -->
**Safety:** W refused **56 of 56** declared cross-scope probes and returned **0** bytes; N accepted **48** and returned the wrong payload **48** times; M accepted **0** but read **766,026** B over **48** requests to refuse the ones its five-dimension key could not distinguish.
**Availability:** after the metadata database was removed, M reused **0 of 16** pages and W reused **16 of 16** byte-exactly.
**Fast path, declared tolerance measured and failed:** W **169.2** µs p50 against N's **29.3** µs = **5.78×** (declared ≤ 2.0×); W **239.1** µs p95 against N's **32.8** µs = **7.29×** (declared ≤ 2.5×); W reads **0.431×** N's backend bytes per request (declared ≤ 1.10×).
**Recovery:** W and M both materialize **270,336** B for the identical demanded output set; W reads **1.12×** M's backend bytes (**321,343** B against **286,540** B) and takes **3.21×** M's wall time, after resolving the checkpoint manifest once per pass instead of once per demanded tensor.
**Only one capability claim survives:** provenance-safe reuse that outlives the application's metadata database, and a refusal that reads nothing. Every performance claim in this court failed or tied.
<!-- END GENERATED:context-advantage-verdict -->

**Read that in the order it is written.** The safety difference and the
availability difference are real and measured. The performance claim was
**declared in advance, measured, and failed**; it is not restated as a win.

The surviving claim, in the exact words this court supports:

> On one backend, with a strong application-level metadata baseline given
> compression, an in-memory index on the brief's five dimensions, full value
> validation, TTL/eviction and its own manifest-based subset selection, the
> WeftFS context plane rejects every cross-scope request with zero bytes read
> and zero bytes returned, and retains exact re-use after the metadata database
> is lost. The baseline matches the happy path and ties the refusal count; on the
> fast path it is faster, and that is the published result.

**No fast-path or neutrality claim is made.** The gate is not met. R27 removed
the duplicated decode work the R26 ledger found; R28 left the fast path
alone and closed the recovery gap (§5.1, §6.2, §6.3).

## 2. Where the baseline wins, ties and loses

This is the most important part of the session, so it comes first.

**M wins on the fast path.** A plain zstd-3 blob with an in-memory index and a
SHA-256 check is materially faster than a mode-19 scoped read, and it reads
about half the bytes the raw floor does — but it is not free: it too is slower
than the raw floor, for the same reason (see §6). R27 removed duplicated work
in W's own read path and R28 removed the last read-path duplication it had
found (a per-tensor checkpoint manifest load, §5.1); what is left on the fast
path is the payload codec, and M pays that too.

**M ties on the happy path.** All three arms returned every compatible page
byte-exactly, every repetition. There is no correctness advantage to the
context plane when the caller already knows the scope.

**M ties on materialized bytes.** For the identical demanded output set M and
W both materialize exactly the contract's bytes. W does not win by materializing
less.

**M loses on cross-scope exclusivity and on metadata loss.** M's index key
covers the five dimensions the brief names; a request that changes
`positional`, `dtype` or `salt` hits that key, so M must read the blob to
discover the mismatch. It still refuses correctly — but it pays reads to do it.
And once M's index and manifest are gone, M cannot reuse anything, because the
scope lives in the metadata database. W's scope lives in the object and is part
of its identity, so the same loss costs W nothing.

**N loses, as constructed.** The floor has no metadata at all: it keys by
`(model, prefix, layer)`, accepts every cross-scope request whose model matches,
and returns the wrong payload every time. That is the defect the other two arms
exist to prevent.

**W no longer loses on recovery metadata.** R27 attributed W's recovery loss to
one re-read checkpoint manifest per tensor restore. R28 removed it: the manifest
is resolved once per pass and reused, so W's recovery byte gap against M dropped
from six manifest loads to one (§5.1 compares the two receipts and names the
remainder). What is left of the gap is decode and root recomputation inside each
tensor restore. W still reads more than M and still takes longer — see §5.1
and §8.

**W loses on fast-path decode cost versus both other arms.** That gap is
attributed in §6.1 and the representation question behind it is answered in
§6.3: no shipped representation carries mode-19 identity with a cheap payload,
and the design that would is reported, not implemented.

## 3. Arms and what was held constant

| | N | M | W |
|---|---|---|---|
| payload | raw element bytes | zstd-3 blob | mode-19 frame (`huffman-canonical-v1` planes) |
| metadata | none | canonical manifest + `app.kv.index.v1` index | the object's own provenance descriptor |
| reuse check | filename | index hit → blob read → SHA-256 → **all eight** dimensions | descriptor id in the capability envelope, checked **before** the read |
| subset selection | none (restores everything) | its own manifest | `weft.checkpoint.v1` + causal closure |

Held constant: `BackendKind::Local`, the store's commit protocol and durability
barrier, the same chunk size, no decoded-object cache, no single-flight, one
process, one thread, one request at a time, identical logical objects and
names, identical demanded output set, a fixed store clock, and the unchanged
Docker court limits (3 GiB / 3 GiB swap / 1024 pids / 4 cpus / bounded `/tmp`).
Each repetition is a fresh process against a fresh store root, and the arm order
rotates by one position per repetition.

The one deliberate asymmetry is the payload codec, which the arms would choose
differently in practice. §7 isolates it.

## 4. Experiment A — provenance-safe reuse

The fixture writes its page grid under the base scope. Seven single-dimension
variants are then presented as requests, each changing exactly one dimension of
the provenance tuple. Only the base
scope's pages exist, so a correct arm refuses and emits nothing. The payload is
seeded by the scope, so an arm that returns another scope's page returns
*different* bytes.

Numbers are medians of seven interleaved repetitions; the receipt's `raw` array
holds every sample.

<!-- BEGIN GENERATED:context-advantage-a (scripts/perf_numbers.py) -->
| measure | N | M | W |
|---|---:|---:|---:|
| fast-path p50 (µs) | 29.3 | 104.4 | 169.2 |
| fast-path p95 (µs) | 32.8 | 123.7 | 239.1 |
| backend bytes per fast-path request | 65,699 | 31,918 | 28,313 |
| compatible reads byte-exact | 64 / 64 | 64 / 64 | 64 / 64 |
| cross-scope probes accepted | 48 | 0 | 0 |
| … of which returned the wrong payload | 48 | 0 | 0 |
| cross-scope probes refused | 8 | 56 | 56 |
| backend bytes read while refusing | 3,153,552 | 766,026 | 0 |
| requests issued while refusing | 96 | 48 | 0 |
| logical bytes returned on a refusal | 3,145,728 | 0 | 0 |
| compatible reuse after metadata loss | 16 / 16 | 0 / 16 | 16 / 16 |
| build commits for the same workload | 8 | 10 | 8 |
| page build wall (ms) | 116.5 | 144.0 | 123.9 |
| physical footprint (B) | 544,093 | 287,151 | 268,004 |
| peak RSS (KiB) | 20,120 | 17,976 | 23,020 |

Measured recompute of one page (the same scalar kernel in every arm): **2,123.6 µs**. The workload was 8 pages of 65,536 logical bytes and 56 cross-scope probes.
<!-- END GENERATED:context-advantage-a -->

Per-variant detail, by arm (accepted / denied / backend bytes read):

<!-- BEGIN GENERATED:context-advantage-n (scripts/perf_numbers.py) -->
| variant | accepted | wrong payload | denied | backend B read |
|---|---:|---:|---:|---:|
| `model` | 0 | 0 | 8 | 0 |
| `adapter` | 8 | 8 | 0 | 525,592 |
| `tokenizer` | 8 | 8 | 0 | 525,592 |
| `attention` | 8 | 8 | 0 | 525,592 |
| `positional` | 8 | 8 | 0 | 525,592 |
| `dtype` | 8 | 8 | 0 | 525,592 |
| `salt` | 8 | 8 | 0 | 525,592 |
<!-- END GENERATED:context-advantage-n -->

<!-- BEGIN GENERATED:context-advantage-m (scripts/perf_numbers.py) -->
| variant | accepted | wrong payload | denied | backend B read |
|---|---:|---:|---:|---:|
| `model` | 0 | 0 | 8 | 0 |
| `adapter` | 0 | 0 | 8 | 0 |
| `tokenizer` | 0 | 0 | 8 | 0 |
| `attention` | 0 | 0 | 8 | 0 |
| `positional` | 0 | 0 | 8 | 255,342 |
| `dtype` | 0 | 0 | 8 | 255,342 |
| `salt` | 0 | 0 | 8 | 255,342 |
<!-- END GENERATED:context-advantage-m -->

<!-- BEGIN GENERATED:context-advantage-w (scripts/perf_numbers.py) -->
| variant | accepted | wrong payload | denied | backend B read |
|---|---:|---:|---:|---:|
| `model` | 0 | 0 | 8 | 0 |
| `adapter` | 0 | 0 | 8 | 0 |
| `tokenizer` | 0 | 0 | 8 | 0 |
| `attention` | 0 | 0 | 8 | 0 |
| `positional` | 0 | 0 | 8 | 0 |
| `dtype` | 0 | 0 | 8 | 0 |
| `salt` | 0 | 0 | 8 | 0 |
<!-- END GENERATED:context-advantage-w -->

### The two labelled M diagnostics

The receipt records two other M decisions over the same index. They are
diagnostics, not alternative arms, and the claim uses the governing arm above.

* **`index_only`** — the same index with no value validation. It would accept
  the `positional`, `dtype` and `salt` variants (the three dimensions the
  five-dimension key does not carry) and refuse the rest. This is what an
  implementation that stops at the brief's five named dimensions does.
* **`full_key`** — the same index keyed on all eight dimensions. It refuses
  everything with zero bytes read, because no key matches at all. It is the
  fastest possible refusal and the least useful index: every near match becomes
  a miss and a recompute rather than a validated reuse.

Neither is a strawman; both are things a competent team might ship. The
governing arm is the middle one: the five-dimension key with full value
validation, which is the strongest of the three.

### The metadata-loss probe

After the main measurement, M's index and manifest objects are removed from the
same store and the compatible reads are repeated. N and W have no such database
to lose, so their rows are controls. This is the property that separates the
arms structurally rather than by implementation quality: an index that lives
beside the data can be lost, stale or split-brained; identity derived from the
object cannot.

### Recomputation

`recompute` is measured, not declared: the harness re-runs the scalar kernel
that *produced* the page and checks it regenerates the exact bytes. It is a
scalar kernel, not an optimized prefill — a production GPU prefill would be much
faster, so the hit-versus-recompute ratio overstates the benefit of reuse. The
receipt labels it that way and no claim is made from it.

## 5. Experiment B — dependency-aware recovery

A checkpoint of the recovery fixture's tensors, two activation blobs and one
derived artifact that declares the activations as its causal parents. The downstream
operation needs six named tensors, both activations and the derived artifact.

N has no metadata and restores everything. M selects from its own manifest. W
selects from the `weft.checkpoint.v1` manifest and resolves the derived
artifact's causal closure.

W's arm resolves the checkpoint manifest **once** and reuses it for every
named-tensor restore (`Store::load_checkpoint_manifest` +
`Store::restore_named_tensor_with_manifest`). That changes nothing about what is
verified: every shard is still read through the content-addressed object path,
every shard's content root and own logical root is still recomputed and
compared with the manifest, the assembled tensor root is still compared, and a
manifest naming another checkpoint is refused. `tests/checkpoint_manifest_reuse.rs`
proves byte-identical bytes, regions and topologies against the per-call path,
proves that only the manifest read is saved (measured in backend bytes and
reads), and proves a forged shard root or byte length still fails `Corrupt`.

<!-- BEGIN GENERATED:context-advantage-b (scripts/perf_numbers.py) -->
| measure | N | M | W |
|---|---:|---:|---:|
| outputs materialized | 99 | 9 | 9 |
| checkpoint manifest loads | 0 | 0 | 1 |
| logical bytes materialized | 3,219,456 | 270,336 | 270,336 |
| backend bytes read | 3,235,591 | 286,540 | 321,343 |
| wall time (ms) | 2.576 | 0.407 | 1.305 |

For the same output set W reads **1.12×** M's backend bytes and takes **3.21×** M's wall time. N's row is the full restore the contract lets it do. The fixture is 96 tensors of 32,768 B, two activation blobs and one derived artifact.
<!-- END GENERATED:context-advantage-b -->

All three arms returned a byte-identical digest of the demanded output set in
every repetition. W's closure resolved both declared parents to live objects
with no missing member, M's manifest declared the same dependency, and after one
activation was removed from the store every arm failed closed — W as a typed
`DependencyMismatch` before reading anything, M as `NotFound` after it had
already read one of the two activations.

W still reads more and is slower (the two generated ratios above), but the
cause is no longer metadata. R27 measured the whole gap as one re-read manifest
per tensor restore; R28 removed that (the numbers are compared below), and what
is left of it is the per-shard tensor metadata the checkpoint format carries —
which is exactly the identity M does not have — plus the cost of decoding and
re-verifying each restored tensor.

### 5.1 The recovery gap, attributed (R27, closed in R28)

The instrumented pass below runs the same recovery on the same store a second
time, so its wall is warm and is never the headline number. What it does give
is the *shape* of the gap: how many objects and manifests each arm reads, how
many checkpoint manifests the arm loads, and where the wall goes.

<!-- BEGIN GENERATED:context-advantage-recovery-ledger (scripts/perf_numbers.py) -->
| one recovery pass | N | M | W |
|---|---:|---:|---:|
| object reads | 99 | 10 | 4 |
| manifest reads | 99 | 10 | 28 |
| checkpoint manifest loads | 0 | 0 | 1 |
| backend bytes | 3,235,591 | 286,540 | 321,343 |
| wall (headline pass, ms) | 2.576 | 0.407 | 1.305 |
| wall (instrumented second pass, ms) | 2.57 | 0.38 | 1.21 |

W reads the same number of objects as M and now loads the checkpoint manifest **once** per pass instead of once per demanded tensor. The whole byte gap against M is **34,803 B** (**5,800 B** per demanded tensor), which is the per-shard tensor metadata the checkpoint path reads to verify each shard's content root and its own logical root; M's flat JSON manifest has no per-shard identity to verify.
<!-- END GENERATED:context-advantage-recovery-ledger -->

The ledger now names a residual rather than the whole gap. Below is the fix,
quantified: what the duplicated manifest read was worth, and what is left.

<!-- BEGIN GENERATED:context-advantage-recovery-fix (scripts/perf_numbers.py) -->
| the recovery byte gap | bytes |
|---|---:|
| W minus M, R27 (one manifest load per tensor) | 306,963 |
| W minus M, R28 (one manifest load per pass) | 34,803 |
| removed by the change | 272,160 |
| backend bytes one manifest load costs | 45,360 |
| of which the checkpoint manifest object | 45,197 |
| of which the manifest blob describing its single chunk | 163 |

The **6** eliminated loads of **45,360 B** each are the whole **272,160 B** reduction, and the ledger's section counts agree (`blob_io_manifest` 28 against R27's 34). What is left is decode and root recomputation, not metadata: W's per-output restore p50 is **97.87 µs** against M's **24.70 µs** over **9** outputs.
<!-- END GENERATED:context-advantage-recovery-fix -->

The remaining per-output cost is decode, not metadata, and it is the same
representation cost §6.3 is about: a checkpoint shard is a mode-10 frame whose
payload is `huffman-canonical-v1` byte planes, and the restore recomputes the
shard root and the tensor root. M zstd-decompresses a blob and SHA-256s it. The
contract did not ask W to be as fast as M at recovery and this court does not
claim it is; it asks that W not materialize more, and W does not.

## 6. Experiment C — fast-path neutrality

The critical path is a request that needs no contextual transformation: serve a
page for a scope the caller already knows is compatible. The tolerance was
declared in the contract before measurement:

<!-- BEGIN GENERATED:context-advantage-tolerances (scripts/perf_numbers.py) -->
* **C1** W p50 ≤ 2.0 × N p50
* **C2** W p95 ≤ 2.5 × N p95
* **C3** W backend bytes per request ≤ 1.10 × N
* **C4** every fast-path read is byte-exact
<!-- END GENERATED:context-advantage-tolerances -->

**The contract was corrected to match the gate it always enforced.** Version 1
of `bench/CONTRACTS-context-advantage.md` §3 C3 printed a bytes tolerance that
did not match the value the harness declares in every receipt and gates on; the
contract is now version 2, its change log states the correction, and the number
above is the enforced one. Nothing was measured under the looser printed value:
every run since R26 declared and compared against the enforced one. The result
would have been the same either way, because W reads fewer bytes per request
than N rather than more, so C3 passes under both.

<!-- BEGIN GENERATED:context-advantage-c (scripts/perf_numbers.py) -->
| measure | N | M | W | declared limit |
|---|---:|---:|---:|---:|
| p50 (µs) | 29.3 | 104.4 | 169.2 | W ≤ 2.0× N |
| p95 (µs) | 32.8 | 123.7 | 239.1 | W ≤ 2.5× N |
| backend bytes per request | 65,699 | 31,918 | 28,313 | W ≤ 1.10× N |

| ratio to N | M | W |
|---|---:|---:|
| p50 | 3.57× | 5.78× |
| p95 | 3.77× | 7.29× |
| backend bytes per request | | 0.431× |
<!-- END GENERATED:context-advantage-c -->

**C1 and C2 fail. The tolerance is not widened.** The measured penalty is a
decode cost, not an I/O cost: W reads *fewer* bytes per request than N and still
takes longer, because the frame must be decoded and its descriptor verified.

### 6.1 The attribution ledger (R27, re-measured in R28)

R26 could only say "the frame must be decoded". R27 measures the whole path with
the store's benchmark-only phase profiler (`src/phase_profile.rs`), instrumented
at each decisive boundary, and reports a ledger that sums to *its own* wall with
the remainder named. The phases are disjoint leaves: no phase is nested inside
another, so the rows add up.

<!-- BEGIN GENERATED:context-advantage-ledger (scripts/perf_numbers.py) -->
| phase, µs per fast-path read | N | W |
|---|---:|---:|
| path resolution | 1.01 | 1.21 |
| object-record lookup | — | 0.24 |
| descriptor decode + identity | — | 0.16 |
| capability envelope + grants | — | 0.17 |
| POSIX / ACL | — | 0.06 |
| manifest read (I/O) | 1.50 | 1.90 |
| manifest hash | 0.25 | 0.25 |
| manifest decode | 0.77 | 1.00 |
| object read (I/O) | 2.34 | 2.17 |
| object blob hash | 10.48 | 9.65 |
| chunk assembly | 0.54 | 0.23 |
| whole-content verification | 10.48 | 9.61 |
| frame parse + checksum | — | 0.90 |
| payload decode | — | 154.60 |
| exactness compare | 0.85 | 0.94 |
| **instrumented wall** | **29.15** | **187.59** |
| unnamed residual (wall minus the rows, per repetition) | 0.92 | 4.68 |

W's whole authorization preamble — object record, descriptor, capability, POSIX — is **0.63 µs** per read. The instrumented ledger runs **158.44 µs** per read above N's, and **0.91×** the uninstrumented W p50 is a single phase: the payload decode.
<!-- END GENERATED:context-advantage-ledger -->

The ledger is instrumented, and the profiler is not free: it takes a global lock
and pushes a sample per section. That is why the instrumented wall is above the
headline p50, and why a row of a few tenths of a microsecond is an upper bound
that is mostly the profiler itself. Each row is an independent median, and the
residual is the median of (wall minus the rows) inside each repetition, so the
table is internally consistent without pretending the medians add exactly. The
two numbers that matter are far larger than that noise: the payload decode, and
the two hash passes every arm pays.

**The finding is not what R26 assumed.** The *authorization* preamble — object
record, descriptor decode and identity, capability envelope, grants, POSIX — is
not where W's excess came from: it was duplicated work, not the plane's logic,
and after the fix it is under one microsecond per read (§6.2). What remains is
the payload decode, which the mode-19 wire format fixes as
`huffman-canonical-v1` byte planes, plus the store's own chunk hash,
whole-content verification and blob reads — all of which N pays too.

### 6.2 What the fixes bought, and the floor that remains

Two things were re-derived on every read that are pure functions of
content-addressed inputs, and both are now computed once per (record, head
generation) and once per envelope:

* the stored capability descriptor was hex-decoded, WCB-decoded, re-encoded and
  hashed on every read; the validated descriptor and its id are now cached,
  keyed by the object record's `(id, content, version)` and dropped whenever the
  head generation changes;
* the same immutable capability envelope was decoded twice per read — once for
  the tenant/namespace/root grants, again inside `authorize_read`;
  `authorize_read_with_capability` now hands the already-decoded scope to the
  unchanged authorization body.

Nothing about authorization moved: every check runs on every read, and a refusal
still reads no bytes. **No authorization decision is cached** — the cache holds
two decode results and nothing else. `tests/kv_fast_path_cache.rs` proves the
cache cannot become a bypass (an expired grant, a second grant, a forged
envelope id, a rewrite, and a commit fence).

What that was worth, receipt against receipt:

<!-- BEGIN GENERATED:context-advantage-r27-delta (scripts/perf_numbers.py) -->
W's fast-path p50 moved from **187.8 µs** (R26) to **169.7 µs** — **9.7 %** lower — and p95 from **250.8 µs** to **232.3 µs** (**7.3 %**). Against N that is **6.51× → 5.84×** at p50 and **7.91× → 7.03×** at p95. The gate needs **2.0×** / **2.5×**, so it was still missed. Those duplications are the R27 change and remain in force.
<!-- END GENERATED:context-advantage-r27-delta -->

The payload decode is still the cost. Its own split, measured in memory on the
same page:

<!-- BEGIN GENERATED:context-advantage-decode (scripts/perf_numbers.py) -->
| mode-19 decode, in memory | p50 µs |
|---|---:|
| byte planes: `huffman-canonical-v1` unpack + word join | 103.61 |
| reconstructed-page hash | 10.49 |
| descriptor parse | 10.40 |
| descriptor identity (re-encode + hash) | 4.85 |
| not covered by those four (decode buffers, first touch) | 1.29 |
| **whole mode-19 decode** | **130.64** |

The same page as a mode-12 frame decodes in **116.7 µs**, so the provenance descriptor itself is **1.119×** the decode it wraps, and the frame is **1.90 %** larger. What fills the rest is the byte-plane codec the wire format fixes for modes 9–19, not the context plane's logic.
<!-- END GENERATED:context-advantage-decode -->

So the floor is made of: `huffman-canonical-v1` symbol decode, one hash of the
reconstructed page (a normative check that the frame's declared payload hash
matches the bytes it produced), the descriptor's identity, and the first touch
of the decode buffers. The only way to remove the largest of those is a
different payload representation for mode 19, which is a wire-format change and
not a read-path fix. **The declared tolerance was not widened and the gate still
fails.**

The in-memory split above sums to less than the ledger's payload-decode row
because the in-memory loop decodes the same frame repeatedly with its buffers
already warm, while a store read allocates its plane and output buffers per
call. That difference — not the context plane — is the last unaccounted gap
between the ledger and the decode measurement.

The M row shows the penalty is not specific to the context plane in kind: a zstd
blob with an application-level index also misses the raw floor, by a smaller
factor, for the same reason — it decodes a compressed payload on every read.
Neither compressed arm preserves a raw-read fast path.

What R28 changed, and what it did not:

<!-- BEGIN GENERATED:context-advantage-r28-delta (scripts/perf_numbers.py) -->
R28 did not change the fast path. W's p50 is **169.2 µs** against R27's **169.7 µs** and p95 **239.1 µs** against **232.3 µs** (R27 receipt `context-advantage-20261113T061907Z`), so the ratio to N is **5.78×** at p50 and **7.29×** at p95 and the declared **2.0×** / **2.5×** is still missed. What R28 changed is recovery: W's backend bytes fell from **593,503 B** to **321,343 B** and its wall from **3.340 ms** to **1.305 ms**.
<!-- END GENERATED:context-advantage-r28-delta -->

### 6.3 The representation question — asked, answered, and not yet built

The context plane's job is to pick the representation that serves the
artifact's context: a hot, immediately-consumed page wants a cheap decode, a
cold page wants compactness, and the provenance identity must be the same
either way. This section reports where that stands.

**An existing representation does not suffice.** Mode 19's payload is the
mode-12 payload verbatim, and the mode-12 page descriptor — which mode 19
embeds verbatim — fixes `plane_codec` to exactly `huffman-canonical-v1`
(`spec/WEFT-AI-CODEC.md` §13.2/§13.3, enforced in
`WeftCodec/src/kv_cache.rs`). No other mode carries the eight provenance
dimensions; they exist only in mode 19. So a page with a raw payload cannot
carry mode-19 identity today, and the only way to get one is an additive,
versioned representation.

The payload step of each is measured on the same page, the same bytes, in the
same process:

<!-- BEGIN GENERATED:context-advantage-representation (scripts/perf_numbers.py) -->
| payload step, same page, in memory | p50 µs | MB/s |
|---|---:|---:|
| shipped: `huffman-canonical-v1` byte planes (modes 9-19) | 103.61 | 632.5 |
| floor of a raw payload: one allocation and one copy of the same bytes | 0.70 | 93,489.3 |

The shipped payload step is **147.8×** the copy on the same 65,536 B. The copy is a floor, not a shipped codec: the wire format fixes `huffman-canonical-v1` for modes 9-19, so no provenance-bound page with a raw payload exists to measure end to end. **The storage delta of this receipt is zero**: no representation changed, the eight pages re-encode to the same 225,201 B of mode-19 frames, and the store footprint is unchanged from R27. Whether replacing the payload step alone would meet C1 is not established here — the store path's Huffman cost is bounded only between the in-memory row above and the ledger's payload-decode row, which also contains the page hash, the descriptor parse and the descriptor identity a raw representation would still pay. The design is `docs/KV-PROV-REPRESENTATION.md`; it is not implemented and nothing here claims it would win.
<!-- END GENERATED:context-advantage-representation -->

**The decision.** Report the design, do not implement it blind. The wire format
that would carry it is frozen, both languages and both spec mirrors would
change, and — the part that decides it — replacing the payload step would not by
itself be shown to meet C1, because the descriptor parse, the descriptor
identity and the reconstructed-page hash are normative per-read checks that a
raw payload still pays. So the design is written down in
[`docs/KV-PROV-REPRESENTATION.md`](../docs/KV-PROV-REPRESENTATION.md): an
additive `plane_codec` value (`raw-le-v1`) in the field that already exists, a
representation-independent `scope_root` derived from the existing descriptor so
"same provenance identity, either representation" is checked rather than
asserted, old readers failing closed on the unknown value, no frozen vector
touched, and a court that must publish the storage delta beside any latency win.
It is **not** in this receipt, and the receipt says so in `not_measured`.

The cost of that decision is exact and is not hidden: the fast path in this
receipt is still the shipped Huffman representation, the gate is still missed,
and the storage delta of the decision is zero because nothing was re-encoded.
The next increment is the design's step 5, its own court.

## 7. The isolated provenance overhead

W's fast path mixes three costs: backend I/O, payload decode, and the
provenance descriptor. The receipt separates them by decoding the *same page*
in memory as a mode-12 frame (payload, no provenance) and as a mode-19 frame
(payload plus provenance), repeatedly, in the same process.

<!-- BEGIN GENERATED:context-advantage-provenance (scripts/perf_numbers.py) -->
Mode-12 decode p50 **116.7 µs**, mode-19 **130.6 µs**, ratio **1.119×**. At the eight-page scale the mode-19 frames are **225,201 B** against **221,001 B** for the same pages as mode-12 frames: **+1.90 %**, which is the descriptor.
<!-- END GENERATED:context-advantage-provenance -->

So the descriptor, the identity derivation and the field-by-field cross-check
cost about an eighth of the payload decode and under two percent of the frame
size. The rest of W's fast-path gap against N is the payload codec both the
contract and the wire format impose; §6.1 attributes the whole path and §6.2
splits the decode.

## 8. Every loss

The receipt's `losses` array names each measured loss with its numbers; the
tables above carry the same values. In summary, and without restating digits:

* the context plane does not preserve a raw-read critical path (declared gate
  failed), and R28 did not change that: it changed recovery, not the fast path;
* the strong application baseline is faster than the context plane on that same
  path, though the gap is now the payload codec rather than duplicated work;
* both compressed arms are slower than the raw floor, so the penalty is not
  unique to WeftFS;
* W still reads more backend bytes and takes more wall time than M on subset
  recovery; R28 removed the re-read checkpoint manifest, and what is left is the
  per-shard tensor metadata the checkpoint format carries plus per-tensor decode
  and root recomputation;
* the representation that would make a hot page cheap to decode does not exist in
  the shipped wire format, and the design that would add it is reported, not
  implemented, so its storage delta and its latency are unknown rather than
  claimed;
* W's fast path still pays a normative hash of every reconstructed page and the
  first touch of its decode buffers, neither of which the read-path fixes can
  remove;
* the floor accepts most cross-scope requests and returns the wrong payload;
* M loses all safe reuse when its metadata database is removed;
* M reads payload bytes to refuse requests its five-dimension key cannot
  distinguish.

## 9. What an application-level metadata system could still do that this court did not test

These are all reachable designs. None were measured, and none may be claimed
against WeftFS without a court:

1. **Embed a canonical scope record in every blob** and then rebuild the index
   by scanning the store. This is the WeftFS design re-implemented by hand: it
   would survive metadata loss and could reject cross-scope requests without a
   read. It needs its own canonical serialisation, its own hash domain, its own
   validation order and its own collision rules — the work is in getting it
   right, not in the idea. This is the most important untested baseline.
2. **Key the index on all eight dimensions** (`M.full_key`, measured here as a
   diagnostic). Refusals become free, at the cost of turning every near match
   into a recompute.
3. **Cache decoded pages in the application**, which would beat both arms on the
   fast path for a working set that fits. The contract fixes the cache capacity
   at zero for all arms; a real deployment would not.
4. **Keep the index in memory across the whole process lifetime** rather than
   reloading per open. M already loads in well under a millisecond here; at
   scale it would matter.
5. **Use a real database** (SQLite, RocksDB) for the index, with transactions and
   its own crash recovery. The court used a sorted JSON index published
   atomically; a real engine trades faster point queries for its own write
   amplification and failure modes.
6. **Content-addressed dedup across scopes**: because the scope is part of W's
   identity, two scopes with identical payloads produce two frames; an
   application that keys on the payload hash alone would dedup them and be
   wrong. A baseline could dedup on `(scope, payload-hash)` safely, and the court
   did not test whether that saves bytes.
7. **Server-side push-down**: an S3 or 3FS backend could hold the scope as
   object metadata and filter without reading payload bytes. Not tested here;
   the local data plane has no such facility.
8. **Choose the payload representation per page** — a raw payload for a hot page,
   a compressed one for a cold page — with the same provenance identity either
   way. The shipped wire format does not allow it (§6.3), so the design in
   `docs/KV-PROV-REPRESENTATION.md` is a proposal. Once implemented it becomes a
   capability an application cannot match without re-implementing the identity
   and the selection, but it is not one today and nothing here is claimed from
   it.

## 10. Non-claims

* Not a fleet, cloud, multi-writer, media or power-loss result: one host, one
  process at a time, the local POSIX data plane.
* Not a GPU or production-prefill result. `recompute` is a scalar reference
  kernel and is labelled as such everywhere it appears.
* Not a codec comparison: M's zstd-3 and W's huffman planes differ by
  construction, which is why §7 separates them.
* Not an adversarial-security result. The salt is an identifier dimension, not a
  secret; no timing or access-pattern privacy is claimed.
* Not a representation-change result. No wire byte changed in R28; the fast path
  measured here is the shipped `huffman-canonical-v1` payload, the raw-copy row
  is a floor rather than a shipped codec, and the design that would add one is a
  proposal.
* Not a statement that any of this is impossible without WeftFS. The claim is
  narrower and is stated in §1 and in the receipt's `claim_gates`.

## 11. Reproduce

```text
scripts/context_advantage_court.sh                 # 7 interleaved repetitions
scripts/context_advantage_court.sh --reduced       # 3 repetitions, aggregate court
scripts/docker-court.sh bench "cd /work && cargo run --release --quiet \
  --example context_advantage_court -- assemble --work <dir> --out <receipt> --reps 7"
```

## 12. Superseded receipts

Three full seven-repetition runs are retained but not cited:

* `bench/receipts/context-advantage-20261113T053807Z.json` recorded a null
  `generated_utc`, because the runner did not forward the stamp into the
  container;
* `bench/receipts/context-advantage-20261113T053930Z.json` fixed the stamp but
  used a fixture whose payload was identical across provenance scopes, which
  made "accepted but wrong bytes" structurally unobservable;
* `bench/receipts/context-advantage-20261113T054318Z.json` used the corrected
  scope-dependent fixture and was superseded within one commit by the addition
  of the per-arm frame-size metrics.

The structural results (refusal counts, backend bytes on refusal, metadata
loss, exactness) are the same in all four runs. Only the current receipt is
cited by the generated numbers.

The reduced three-repetition configuration that `scripts/verify-all.sh`
registers was run once as
`bench/receipts/context-advantage-20261113T055900Z.json` (`label: reduced`). It
reproduces every structural result and the same failed fast-path gate.

### R27 runs

| stamp | label | status |
|---|---|---|
| `20261113T061016Z` | full | **not cited, deleted.** Produced from a working tree before the fix was committed, so `environment.revisions.WeftFS` named a tree that did not contain the code it measured. A receipt whose revision field cannot reproduce it is not evidence; it is not kept. |
| `20261113T061410Z` | full | superseded: same code as the cited run, before the aggregated receipt carried the mode-19 decode split. Cited by §6.2's regression only through the R26 receipt, never for a ledger number. |
| `20261113T061527Z` | reduced | superseded by `...T061953Z`; same structural results and the same failed gate. |
| `20261113T061907Z` | full | the R27 receipt; now superseded by `...T064743Z`, and still the "before" side of every R28 before/after comparison. |
| `20261113T061953Z` | reduced | the R27 reduced receipt; superseded by `...T064832Z`. |

Every structural result is identical across these runs: 56/56 refusals with 0
backend bytes and 0 bytes returned, 64/64 exact fast-path reads in every arm,
16/16 reuse after metadata loss for W and 0/16 for M, and identical materialized
recovery bytes. Only the current receipt is cited by the generated numbers.

### R28 runs

| stamp | label | status |
|---|---|---|
| `20261113T064310Z` | full | superseded: the manifest-reuse W arm, before the receipt named the manifest object's size and the per-output restore latency. Its structural results match the cited run. |
| `20261113T064629Z` | full | superseded by `...T064743Z` only by a field rename (`manifest_blob_bytes` → `manifest_object_bytes`) on the same code path; every measured value is identical. |
| `20261113T064743Z` | full | superseded: same code path, before the receipt's `losses` array stopped describing the manifest re-read that R28 removed; every structural result matches the cited run. |
| `20261113T064832Z` | reduced | superseded by `...T070956Z`; same structural results and the same failed gate. |
| `20261113T070714Z` | full | **the cited receipt.** |
| `20261113T070956Z` | reduced | **the cited reduced receipt** for the aggregate court; reproduces every structural result and the same failed fast-path gate. |

The R28 runs are the first ones in which the W arm resolves the checkpoint
manifest once; their recovery byte and wall values are not comparable with the
R27 rows and are never averaged with them.
