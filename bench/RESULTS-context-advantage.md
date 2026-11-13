# Context Advantage Court — results (R26, extended by R27)

One mature backend, with and without the WeftFS context plane, holding the
hardware, durability, workload, caching policy, concurrency and the available
optimizations constant. Frozen contract:
[`bench/CONTRACTS-context-advantage.md`](CONTRACTS-context-advantage.md),
committed before the court produced any measurement.

Receipt: `bench/receipts/context-advantage-20261113T061907Z.json` (R27, stamp
`20261113T061907Z`, WeftFS `20e66147a7f8`), superseding the R26 receipt
`context-advantage-20261113T054528Z.json`; both are retained, and §12 lists the
runs that are not cited. The reduced three-repetition configuration the
aggregate court runs is `context-advantage-20261113T061953Z.json`. Every number
below is generated from the current receipt by `scripts/perf_numbers.py`.

---

## 1. The result

<!-- BEGIN GENERATED:context-advantage-verdict (scripts/perf_numbers.py) -->
**Safety:** W refused **56 of 56** declared cross-scope probes and returned **0** bytes; N accepted **48** and returned the wrong payload **48** times; M accepted **0** but read **766,026** B over **48** requests to refuse the ones its five-dimension key could not distinguish.
**Availability:** after the metadata database was removed, M reused **0 of 16** pages and W reused **16 of 16** byte-exactly.
**Fast path, declared tolerance measured and failed:** W **169.7** µs p50 against N's **29.1** µs = **5.84×** (declared ≤ 2.0×); W **232.3** µs p95 against N's **33.1** µs = **7.03×** (declared ≤ 2.5×); W reads **0.431×** N's backend bytes per request (declared ≤ 1.10×).
**Recovery:** W and M both materialize **270,336** B for the identical demanded output set, and W reads **2.07×** M's backend bytes (**593,503** B against **286,540** B).
**Only one capability claim survives:** provenance-safe reuse that outlives the application's metadata database, and a refusal that reads nothing. Every performance claim in this court failed or tied.
<!-- END GENERATED:context-advantage-verdict -->

**Read that in the order it is written.** The safety difference and the
availability difference are real and measured. The performance claim was
**declared in advance, measured, and failed**; it is not restated as a win.

## 2. Where the baseline wins, ties and loses

This is the most important part of the session, so it comes first.

**M wins on the fast path.** A plain zstd-3 blob with an in-memory index and a
SHA-256 check is materially faster than a mode-19 scoped read, and it reads
about half the bytes the raw floor does — but it is not free: it too is slower
than the raw floor, for the same reason (see §6). R27 narrowed W's gap to N by
removing duplicated decode work in W's own read path (§6.2 compares the two
receipts); what is left is the payload codec, and M pays that too.

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

**W loses on recovery backend bytes and on recovery wall time**, and it loses
on decode cost versus both other arms. Both gaps are attributed in §5.1 and §6.1:
the recovery loss is one re-read manifest per tensor restore, and the fast-path
loss is the payload codec. See §8.

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
| fast-path p50 (µs) | 29.1 | 104.0 | 169.7 |
| fast-path p95 (µs) | 33.1 | 115.2 | 232.3 |
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
| page build wall (ms) | 113.5 | 143.1 | 118.0 |
| physical footprint (B) | 544,093 | 287,151 | 268,004 |
| peak RSS (KiB) | 19,932 | 16,292 | 23,152 |

Measured recompute of one page (the same scalar kernel in every arm): **2,122.6 µs**. The workload was 8 pages of 65,536 logical bytes and 56 cross-scope probes.
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

<!-- BEGIN GENERATED:context-advantage-b (scripts/perf_numbers.py) -->
| measure | N | M | W |
|---|---:|---:|---:|
| outputs materialized | 99 | 9 | 9 |
| logical bytes materialized | 3,219,456 | 270,336 | 270,336 |
| backend bytes read | 3,235,591 | 286,540 | 593,503 |
| wall time (ms) | 2.711 | 0.402 | 3.340 |

For the same output set W reads **2.07×** M's backend bytes and takes **8.32×** M's wall time. N's row is the full restore the contract lets it do. The fixture is 96 tensors of 32,768 B, two activation blobs and one derived artifact.
<!-- END GENERATED:context-advantage-b -->

All three arms returned a byte-identical digest of the demanded output set in
every repetition. W's closure resolved both declared parents to live objects
with no missing member, M's manifest declared the same dependency, and after one
activation was removed from the store every arm failed closed — W as a typed
`DependencyMismatch` before reading anything, M as `NotFound` after it had
already read one of the two activations.

W is **slower and reads more** here. The cause is not the context plane's
identity model: `restore_named_tensor_at` reloads the checkpoint manifest on
every call, so six tensor restores read the manifest six times. That is a
shipped-API cost, it is measured, and it is in the receipt's `losses` array.

### 5.1 The recovery gap, attributed (R27)

The instrumented pass below runs the same recovery on the same store a second
time, so its wall is warm and is never the headline number. What it does give
is the *shape* of the gap: how many objects and manifests each arm reads, and
where the wall goes.

<!-- BEGIN GENERATED:context-advantage-recovery-ledger (scripts/perf_numbers.py) -->
| one recovery pass | N | M | W |
|---|---:|---:|---:|
| object reads | 99 | 10 | 10 |
| manifest reads | 99 | 10 | 34 |
| backend bytes | 3,235,591 | 286,540 | 593,503 |
| wall (headline pass, ms) | 2.711 | 0.402 | 3.340 |
| wall (instrumented second pass, ms) | 2.62 | 0.38 | 3.24 |

W reads the same number of objects as M and a different number of manifests. The whole byte gap is **306,963 B**, which is **51,160 B** per demanded tensor: `restore_named_tensor_at` re-reads the checkpoint manifest on every call. M loads its manifest once.
<!-- END GENERATED:context-advantage-recovery-ledger -->

The ledger names the whole byte gap and nothing else: W reads the same objects
as M and pays only for the manifests it re-reads. The repair is a
`restore_named_tensor_with_manifest` entry point that takes the checkpoint
manifest the caller has already loaded, which is what a serving loop would do.
It is **not** in this receipt: adding it would change what the W arm does, so
the before/after numbers would no longer be comparable. It is named here as the
next increment, not claimed as done.

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

<!-- BEGIN GENERATED:context-advantage-c (scripts/perf_numbers.py) -->
| measure | N | M | W | declared limit |
|---|---:|---:|---:|---:|
| p50 (µs) | 29.1 | 104.0 | 169.7 | W ≤ 2.0× N |
| p95 (µs) | 33.1 | 115.2 | 232.3 | W ≤ 2.5× N |
| backend bytes per request | 65,699 | 31,918 | 28,313 | W ≤ 1.10× N |

| ratio to N | M | W |
|---|---:|---:|
| p50 | 3.58× | 5.84× |
| p95 | 3.49× | 7.03× |
| backend bytes per request | | 0.431× |
<!-- END GENERATED:context-advantage-c -->

**C1 and C2 fail. The tolerance is not widened.** The measured penalty is a
decode cost, not an I/O cost: W reads *fewer* bytes per request than N and still
takes longer, because the frame must be decoded and its descriptor verified.

### 6.1 The attribution ledger (R27)

R26 could only say "the frame must be decoded". R27 measures the whole path with
the store's benchmark-only phase profiler (`src/phase_profile.rs`), instrumented
at each decisive boundary, and reports a ledger that sums to *its own* wall with
the remainder named. The phases are disjoint leaves: no phase is nested inside
another, so the rows add up.

<!-- BEGIN GENERATED:context-advantage-ledger (scripts/perf_numbers.py) -->
| phase, µs per fast-path read | N | W |
|---|---:|---:|
| path resolution | 0.99 | 1.14 |
| object-record lookup | — | 0.23 |
| descriptor decode + identity | — | 0.16 |
| capability envelope + grants | — | 0.14 |
| POSIX / ACL | — | 0.05 |
| manifest read (I/O) | 1.52 | 1.79 |
| manifest hash | 0.25 | 0.25 |
| manifest decode | 0.78 | 0.97 |
| object read (I/O) | 2.72 | 2.16 |
| object blob hash | 10.49 | 9.67 |
| chunk assembly | 0.53 | 0.21 |
| whole-content verification | 10.43 | 9.61 |
| frame parse + checksum | — | 0.87 |
| payload decode | — | 151.68 |
| exactness compare | 0.85 | 0.90 |
| **named total** | **29.44** | **183.64** |
| unnamed residual | 0.96 | 4.37 |

W's whole authorization preamble — object record, descriptor, capability, POSIX — is **0.59 µs** per read. The instrumented ledger runs **154.20 µs** per read above N's, and **0.89×** the uninstrumented W p50 is a single phase: the payload decode.
<!-- END GENERATED:context-advantage-ledger -->

The ledger is instrumented, and the profiler is not free: it takes a global lock
and pushes a sample per section. That is why the instrumented wall is above the
headline p50, and why a row of a few tenths of a microsecond is an upper bound
that is mostly the profiler itself. The two numbers that matter are far larger
than that noise: the payload decode, and the two hash passes every arm pays.

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
W's fast-path p50 moved from **187.8 µs** (R26, receipt `context-advantage-20261113T054528Z`) to **169.7 µs** — **9.7 %** lower — and p95 from **250.8 µs** to **232.3 µs** (**7.3 %**). Against N that is **6.51× → 5.84×** at p50 and **7.91× → 7.03×** at p95. The gate needs **2.0×** / **2.5×**, so it is still missed, by a narrower margin. W is now **1.63×** M, the strong application baseline that also has to decode a compressed payload.
<!-- END GENERATED:context-advantage-r27-delta -->

After that, the remaining cost is the payload decode. Its own split, measured in
memory on the same page:

<!-- BEGIN GENERATED:context-advantage-decode (scripts/perf_numbers.py) -->
| mode-19 decode, in memory | p50 µs |
|---|---:|
| byte planes: `huffman-canonical-v1` unpack + word join | 104.35 |
| reconstructed-page hash | 10.48 |
| descriptor parse | 10.48 |
| descriptor identity (re-encode + hash) | 4.85 |
| not covered by those four (decode buffers, first touch) | 2.05 |
| **whole mode-19 decode** | **132.22** |

The same page as a mode-12 frame decodes in **117.7 µs**, so the provenance descriptor itself is **1.124×** the decode it wraps, and the frame is **1.90 %** larger. What fills the rest is the byte-plane codec the wire format fixes for modes 9–19, not the context plane's logic.
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

## 7. The isolated provenance overhead

W's fast path mixes three costs: backend I/O, payload decode, and the
provenance descriptor. The receipt separates them by decoding the *same page*
in memory as a mode-12 frame (payload, no provenance) and as a mode-19 frame
(payload plus provenance), repeatedly, in the same process.

<!-- BEGIN GENERATED:context-advantage-provenance (scripts/perf_numbers.py) -->
Mode-12 decode p50 **117.7 µs**, mode-19 **132.2 µs**, ratio **1.124×**. At the eight-page scale the mode-19 frames are **225,201 B** against **221,001 B** for the same pages as mode-12 frames: **+1.90 %**, which is the descriptor.
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
  failed);
* the strong application baseline is faster than the context plane on that same
  path, though the gap is now the payload codec rather than duplicated work;
* both compressed arms are slower than the raw floor, so the penalty is not
  unique to WeftFS;
* W reads more backend bytes and takes more wall time than M on subset recovery,
  because the shipped per-tensor restore re-reads the checkpoint manifest;
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

## 10. Non-claims

* Not a fleet, cloud, multi-writer, media or power-loss result: one host, one
  process at a time, the local POSIX data plane.
* Not a GPU or production-prefill result. `recompute` is a scalar reference
  kernel and is labelled as such everywhere it appears.
* Not a codec comparison: M's zstd-3 and W's huffman planes differ by
  construction, which is why §7 separates them.
* Not an adversarial-security result. The salt is an identifier dimension, not a
  secret; no timing or access-pattern privacy is claimed.
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
| `20261113T061907Z` | full | **the cited receipt.** |
| `20261113T061953Z` | reduced | **the cited reduced receipt** for the aggregate court. |

Every structural result is identical across these runs: 56/56 refusals with 0
backend bytes and 0 bytes returned, 64/64 exact fast-path reads in every arm,
16/16 reuse after metadata loss for W and 0/16 for M, and identical materialized
recovery bytes. Only the current receipt is cited by the generated numbers.
