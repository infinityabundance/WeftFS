# Context Advantage Court — results (R26)

One mature backend, with and without the WeftFS context plane, holding the
hardware, durability, workload, caching policy, concurrency and the available
optimizations constant. Frozen contract:
[`bench/CONTRACTS-context-advantage.md`](CONTRACTS-context-advantage.md),
committed before the court produced any measurement.

Receipt: `bench/receipts/context-advantage-20261113T054528Z.json` (analysis of the
superseded runs is in §12). Every number below is generated from the current
receipt by `scripts/perf_numbers.py`.

---

## 1. The result

<!-- BEGIN GENERATED:context-advantage-verdict (scripts/perf_numbers.py) -->
**Safety:** W refused **56 of 56** declared cross-scope probes and returned **0** bytes; N accepted **48** and returned the wrong payload **48** times; M accepted **0** but read **766,026** B over **48** requests to refuse the ones its five-dimension key could not distinguish.
**Availability:** after the metadata database was removed, M reused **0 of 16** pages and W reused **16 of 16** byte-exactly.
**Fast path, declared tolerance measured and failed:** W **187.8** µs p50 against N's **28.9** µs = **6.51×** (declared ≤ 2.0×); W **250.8** µs p95 against N's **31.7** µs = **7.91×** (declared ≤ 2.5×); W reads **0.431×** N's backend bytes per request (declared ≤ 1.10×).
**Recovery:** W and M both materialize **270,336** B for the identical demanded output set, and W reads **2.07×** M's backend bytes (**593,503** B against **286,540** B).
**Only one capability claim survives:** provenance-safe reuse that outlives the application's metadata database, and a refusal that reads nothing. Every performance claim in this court failed or tied.
<!-- END GENERATED:context-advantage-verdict -->

**Read that in the order it is written.** The safety difference and the
availability difference are real and measured. The performance claim was
**declared in advance, measured, and failed**; it is not restated as a win.

## 2. Where the baseline wins, ties and loses

This is the most important part of the session, so it comes first.

**M wins on the fast path.** A plain zstd-3 blob with an in-memory index and a
SHA-256 check is materially faster than a mode-19 scoped read, and it is faster
than the raw backend read would allow if it compressed nothing — but it is not
free. W is the slowest of the three arms on a request that needs no contextual
transformation.

**M ties on the happy path.** All three arms returned every compatible page
byte-exactly, every repetition. There is no correctness advantage to the
context plane when the caller already knows the scope.

**M ties on materialized bytes.** For the identical demanded output set
(six tensors, two activations, one derived artifact) M and W both materialize
exactly the contract's bytes. W does not win by materializing less.

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
on decode cost versus both other arms. See §6 and §8.

## 3. Arms and what was held constant

| | N | M | W |
|---|---|---|---|
| payload | raw element bytes | zstd-3 blob | mode-19 frame (`huffman-canonical-v1` planes) |
| metadata | none | canonical manifest + `app.kv.index.v1` index | the object's own provenance descriptor |
| reuse check | filename | index hit → blob read → SHA-256 → **all eight** dimensions | descriptor id in the capability envelope, checked **before** the read |
| subset selection | none (restores everything) | its own manifest | `weft.checkpoint.v1` + causal closure |
| commits for the same workload | 8 | 10 (8 blobs + index + manifest) | 8 |

Held constant: `BackendKind::Local`, the store's commit protocol and durability
barrier, `chunk_size` 65,536 B, no decoded-object cache, no single-flight, one
process, one thread, one request at a time, identical logical objects and
names, identical demanded output set, a fixed store clock, and the unchanged
Docker court limits (3 GiB / 3 GiB swap / 1024 pids / 4 cpus / bounded `/tmp`).
Each repetition is a fresh process against a fresh store root, and the arm order
rotates by one position per repetition.

The one deliberate asymmetry is the payload codec, which the arms would choose
differently in practice. §7 isolates it.

## 4. Experiment A — provenance-safe reuse

Eight pages (4 prefixes × 2 layers, 65,536 logical bytes each) are written under
the base scope. Seven single-dimension variants are then presented as requests,
each changing exactly one dimension of the provenance tuple. Only the base
scope's pages exist, so a correct arm refuses and emits nothing. The payload is
seeded by the scope, so an arm that returns another scope's page returns
*different* bytes.

Numbers are medians of seven interleaved repetitions; the receipt's `raw` array
holds every sample.

<!-- BEGIN GENERATED:context-advantage-a (scripts/perf_numbers.py) -->
| measure | N | M | W |
|---|---:|---:|---:|
| fast-path p50 (µs) | 28.9 | 104.1 | 187.8 |
| fast-path p95 (µs) | 31.7 | 128.3 | 250.8 |
| backend bytes per fast-path request | 65,699 | 31,918 | 28,313 |
| compatible reads byte-exact | 64 / 64 | 64 / 64 | 64 / 64 |
| cross-scope probes accepted | 48 | 0 | 0 |
| … of which returned the wrong payload | 48 | 0 | 0 |
| cross-scope probes refused | 8 | 56 | 56 |
| backend bytes read while refusing | 3,153,552 | 766,026 | 0 |
| requests issued while refusing | 96 | 48 | 0 |
| logical bytes returned on a refusal | 3,145,728 | 0 | 0 |
| compatible reuse after metadata loss | 16 / 16 | 0 / 16 | 16 / 16 |
| page build wall (ms) | 113.1 | 143.1 | 118.5 |
| physical footprint (B) | 544,093 | 287,151 | 268,004 |
| peak RSS (KiB) | 16,432 | 17,680 | 22,812 |

Measured recompute of one page (the same scalar kernel in every arm): **2,120.0 µs**. The workload was 8 pages of 65,536 logical bytes and 56 cross-scope probes.
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

A checkpoint of 96 tensors (32,768 B each), two activation blobs and one derived
artifact that declares the activations as its causal parents. The downstream
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
| wall time (ms) | 2.641 | 0.403 | 3.319 |

For the same output set W reads **2.07×** M's backend bytes and takes **8.23×** M's wall time. N's row is the full restore the contract lets it do.
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

## 6. Experiment C — fast-path neutrality

The critical path is a request that needs no contextual transformation: serve a
page for a scope the caller already knows is compatible. The tolerance was
declared in the contract before measurement:

* **C1** W p50 ≤ 2.0 × N p50
* **C2** W p95 ≤ 2.5 × N p95
* **C3** W backend bytes per request ≤ 1.10 × N
* **C4** every fast-path read is byte-exact

<!-- BEGIN GENERATED:context-advantage-c (scripts/perf_numbers.py) -->
| measure | N | M | W | declared limit |
|---|---:|---:|---:|---:|
| p50 (µs) | 28.9 | 104.1 | 187.8 | W ≤ 2.0× N |
| p95 (µs) | 31.7 | 128.3 | 250.8 | W ≤ 2.5× N |
| backend bytes per request | 65,699 | 31,918 | 28,313 | W ≤ 1.10× N |

| ratio to N | M | W |
|---|---:|---:|
| p50 | 3.61× | 6.51× |
| p95 | 4.04× | 7.91× |
| backend bytes per request | | 0.431× |
<!-- END GENERATED:context-advantage-c -->

**C1 and C2 fail. The tolerance is not widened.** The measured penalty is a
decode cost, not an I/O cost: W reads *fewer* bytes per request than N and still
takes longer, because the frame must be decoded and its descriptor verified.

The M row shows the penalty is not specific to the context plane: a zstd blob
with an application-level index also misses the raw floor, by a smaller factor.
Neither compressed arm preserves a raw-read fast path, and W adds its own
descriptor cost on top.

## 7. The isolated provenance overhead

W's fast path mixes three costs: backend I/O, payload decode, and the
provenance descriptor. The receipt separates them by decoding the *same page*
in memory as a mode-12 frame (payload, no provenance) and as a mode-19 frame
(payload plus provenance), eight times each, in the same process.

<!-- BEGIN GENERATED:context-advantage-provenance (scripts/perf_numbers.py) -->
Mode-12 decode p50 **116.4 µs**, mode-19 **132.4 µs**, ratio **1.137×**. At the eight-page scale the mode-19 frames are **225,201 B** against **221,001 B** for the same pages as mode-12 frames: **+1.90 %**, which is the descriptor.
<!-- END GENERATED:context-advantage-provenance -->

So the descriptor, the identity derivation and the field-by-field cross-check
cost about an eighth of the payload decode and under two percent of the frame
size. The rest of W's fast-path gap against N is the payload codec both the
contract and the wire format impose.

## 8. Every loss

The receipt's `losses` array names each measured loss with its numbers; the
tables above carry the same values. In summary, and without restating digits:

* the context plane does not preserve a raw-read critical path (declared gate
  failed);
* the strong application baseline is faster than the context plane on that same
  path;
* both compressed arms are slower than the raw floor, so the penalty is not
  unique to WeftFS;
* W reads more backend bytes and takes more wall time than M on subset recovery,
  because the shipped per-tensor restore re-reads the checkpoint manifest;
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
