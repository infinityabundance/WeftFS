# EKBLOOM Context Court — results

The frozen contract is [`bench/CONTRACTS-ekbloom-context.md`](CONTRACTS-ekbloom-context.md),
committed at `440950c` before any measurement and amended only by the
pre-measurement corrections recorded in its header at `88e1fa6`. This is a
**fresh court**. It does not revise, reopen or rehabilitate the retired
tensor-codec trial (`bench/CONTRACTS-ekbloom.md`, `docs/EKBLOOM-PLAN.md`,
`bench/RESULTS-ekbloom.md`). That trial measured the representation of tensor
bytes and lost space and bounded random access. This court asks whether a
manifested **decision** can accelerate reasoning over AI state.

Receipts (each with its log beside it):

| Court | Receipt | WeftFS revision | Stamp |
|---|---|---|---|
| decisions | `bench/receipts/ekbloom-context-20261114T100000Z.json` | `29ae91d0ec45` | `20261114T100000Z` |
| scale | `bench/receipts/ekbloom-context-scale-20261114T100000Z.json` | `97a79d8254c4` | `20261114T100000Z` |
| lifecycle | `bench/receipts/ekbloom-context-lifecycle-20261114T100000Z.json` | `29ae91d0ec45` | `20261114T100000Z` |

Every number below is emitted from those receipts by `scripts/perf_numbers.py`
between `BEGIN GENERATED` sentinels; the regions are not hand-edited.

---

## 1. The verdict

<!-- BEGIN GENERATED:ekbloom-context-verdict (scripts/perf_numbers.py) -->
**Verdict: retired.** G1 (zero incorrect acceptances) holds on every request in every repetition; G2 (p99 margin) fails — E is **138.19 µs** p99 against the stronger conventional baseline's **33.87 µs** (**4.08×**, declared ≤ 0.90×); G3 (equal budget) holds — E retains **1,001,392 B**, M **1,048,576 B**, budget **1,048,576 B**; G4 (correctness under change) holds. The sensitivity control produced **1,806** false acceptances on the same stream, so G1 is not vacuous. Measurement validity: **pass** (stamp 20261114T100000Z, revision 29ae91d0ec45).
<!-- END GENERATED:ekbloom-context-verdict -->

**The manifested-decision direction is retired.** Under the contract's
retirement rule, G1 (zero incorrect acceptances) and G4 (correctness under
change) hold, G3 (equal retained-byte budget) holds, and G2 fails: E's p99 is
several times M's, not the declared 10 % below it. A non-inferiority result
would also not have been a win; this is not even that. The manifest is a
correct, collectable accelerator that is **slower and larger per useful entry**
than an exact-keyed in-memory cache at the same retained-byte budget.

The mechanism is real. The manifested table was built, opened, verified, read
through the pinned crate's caller-buffer path, and every one of its hits was
compared against the full 144-byte sufficient key; it returned no wrong
verdict. What it did not do is beat the conventional cache on the declared
axis.

## 2. What was tested

### 2.1 The rule and the decision

The prototype rule `weft.kv.exact-prefix.v1` decides `exact`, `denied` or
`unproven` for a subject KV page (or tensor-region key) against a consumer
contract built from model root, adapter-set root, tokenizer root, attention
config, positional policy, geometry, token-history root, tenant and salt.
Absence is not a wildcard: a required dimension that is absent on either side
yields `unproven`. A decision is a canonical WCB record
`weft.kv.exact-prefix.decision.v1` with the request key, verdict, established
guarantee, evidence roots, snapshot and authority epoch.

### 2.2 Key sufficiency

The lookup key is
`H("weft/kv/exact-prefix/key/v1", WCB({rule, rule_version, subject_root,
contract_root, snapshot, authority_root, authority_epoch}))`. Every input is
content-addressed; the snapshot fixes object presence and content; the
authority record commits to the grant and epoch. Same key ⇒ same inputs ⇒ same
verdict. The key contains nothing mutable. After every cache or manifest
lookup the arm re-checks the current head, the current authority root and
epoch, the subject's presence, and the page's content root; a failure is a
stale rejection followed by re-evaluation, never an accept.

The manifest's 16-byte prefix is a lookup accelerator only. A prefix hit reads
the 208-byte key area and compares the 144-byte core byte-for-byte. The
lifecycle receipt's `candidate_only_rule_holds` is true: a prefix candidate
with a different core returns no verdict. A Bloom or prefix hit can never
produce an accept.

### 2.3 The arms and the budget

One backend (the local POSIX WeftFS data plane), one byte-identical store copy
per arm, one process, one thread, one request at a time. W evaluates every
request through the shipped resolver. M-LRU and M-W-TinyLFU cache the decision
by the exact key, with the same 1 MiB retained-byte budget; M's p99 is the
**minimum** of the two (the stronger baseline). E is M-LRU plus an
EKBLOOM-manifested certified decision table (`create_auto_within_budget`,
`ManifestationProfile::Sparse { interval: 1024 }`), read through the crate's
bounded engine on an in-memory body, with the manifest charged in full and the
cache budget reduced by the manifest's retained bytes (plus a declared 10 %
arena reserve). C-naive is a deliberately insufficient key (subject +
contract, no snapshot or epoch, no post-lookup re-check) and is a sensitivity
control, not a contender.

### 2.4 The workload

6,144 KV-page subjects and 384 tensor-region keys across three model branches,
192 Context Roots, 51 consumer contracts, and 24 subjects whose page object is
absent from the base store. 8,000 requests per repetition over six buckets
(hot 55 %, warm 20 %, cold 10 %, incompatible 9 %, missing dimension 3 %,
missing evidence 3 %), with four state-change events at fixed indices: a model
update on branch 1 at 2,000, a capability revocation on branch 2 at 3,600, a
missing-dependency deletion at 5,200, and a new snapshot at 6,800. Nine
repetitions, arm order rotated by one per repetition, 72,000 measured requests
per arm. The p99 cell is supported by 720 samples.

## 3. Results

<!-- BEGIN GENERATED:ekbloom-context-results (scripts/perf_numbers.py) -->
| Arm | p50 µs | p95 µs | p99 µs | max µs | retained B | evaluations | cache hits | manifest hits | manifest misses |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| W — shipped resolver | 21.35 | 27.83 | 38.70 | 9,964.09 | 0 | 72,000 | 0 | 0 | 0 |
| M-LRU | 11.40 | 26.43 | 34.41 | 6,624.52 | 1,048,576 | 38,249 | 33,751 | 0 | 0 |
| M-W-TinyLFU | 11.39 | 26.36 | 33.87 | 20,609.83 | 1,048,576 | 38,249 | 33,751 | 0 | 0 |
| E — M plus manifestation | 18.19 | 49.86 | 138.19 | 5,863.47 | 1,001,392 | 39,509 | 25,630 | 6,861 | 65,139 |
| C-naive — sensitivity control | 2.48 | 26.15 | 33.40 | 29,350.19 | — | — | — | — | — |

All numbers are over **72,000** measured requests per arm in the full nine-repetition run; the retained column is the maximum over repetitions. The stronger conventional baseline is M-W-TinyLFU because its p99 is the lower of the two M variants (the contract declares the minimum).

| Arm | dependency nodes traversed | backend bytes read |
|---|---:|---:|
| W | 271,787 | 57,783,360 |
| M-LRU | 171,681 | 30,712,276 |
| E | 188,854 | 31,723,156 |

Fixture: **6,552** subjects, **72,000** measured requests per arm, **80,000** retained raw request rows. E's final manifestation retains **417,176 B** and was rebuilt **306** times for **3,433.2 ms** of construction, all outside the measured request latency.
<!-- END GENERATED:ekbloom-context-results -->

The p50 win belongs to the **cache**, not the manifestation: M halves W's p50
and trims its p99. E's p50 is worse than M's because its manifest consumes
budget that the cache would otherwise hold, and its p99 is several times M's
because every miss also reads a manifested slot through EKBLOOM.

## 4. The gates

### G1 — zero incorrect acceptances: **holds**

<!-- BEGIN GENERATED:ekbloom-context-g1 (scripts/perf_numbers.py) -->
| Arm | false acceptances | semantic mismatches |
|---|---:|---:|
| M-LRU | 0 | 0 |
| M-W-TinyLFU | 0 | 0 |
| E | 0 | 0 |
| C-naive (control, not an arm) | 1,806 | — |
<!-- END GENERATED:ekbloom-context-g1 -->

Every M and E verdict agreed with W on every request in every repetition:
verdict **and** reason. The sensitivity control produced
**<!-- generated inline see G1 block -->** false acceptances on the same
stream, so the stream and the oracle can detect an insufficient key. That
control is the evidence that G1 is not vacuous.

### G2 — p99 margin: **fails**

<!-- BEGIN GENERATED:ekbloom-context-g2 (scripts/perf_numbers.py) -->
E p99 **138.19 µs**; stronger M p99 **33.87 µs**; ratio **4.08×**. Declared pass: `p99_E ≤ 0.90 × p99_M`; the declared non-inferiority band (`≤ 1.10 ×`) also fails. E's p50 is **1.60×** M's p50, so it does not even match the cache at the median.
<!-- END GENERATED:ekbloom-context-g2 -->

The declared margin was `p99_E ≤ 0.90 × p99_M`, justified in the contract from
the published court reproducibility spread (1.6 %) and the R8 cache's 5.65× p50
reuse win. E misses it by a wide margin. The number was not re-tuned, no
tolerance was widened, and the non-inferiority band (`≤ 1.10 ×`) also fails.

### G3 — retained bytes: **holds**

<!-- BEGIN GENERATED:ekbloom-context-g3 (scripts/perf_numbers.py) -->
| Measure | Bytes |
|---|---:|
| E maximum retained | 1,001,392 |
| M-LRU maximum retained | 1,048,576 |
| M-W-TinyLFU maximum retained | 1,048,576 |
| Declared budget | 1,048,576 |

G3 requires E ≤ budget and E ≤ both M variants in every repetition; it holds. The manifestation is charged at its full retained size, not its apparent slot size.
<!-- END GENERATED:ekbloom-context-g3 -->

E stays under the 1 MiB budget and under M in every repetition. The manifested
table is charged at its full retained size (`max(file_bytes,
body.physical_bytes())`), not the apparent size of its slots. The loss is
therefore **not** an accounting artifact: E is at or below M's bytes and still
loses G2.

### G4 — correctness under change: **holds**

<!-- BEGIN GENERATED:ekbloom-context-g4 (scripts/perf_numbers.py) -->
Each of the four events (model update at 2,000, revocation at 3,600, missing dependency at 5,200, new snapshot at 6,800) forced a real re-evaluation on the first affected request in M and E, agreed with W, and produced no stale acceptance. The snapshot is in the key, so a manifest entry cannot survive a head change; the authority root and epoch are in the key, so a revocation cannot be served from a cache.
<!-- END GENERATED:ekbloom-context-g4 -->

After each event the first affected request was a real re-evaluation in M and
E (the key contains the snapshot and, for the revocation, the authority root
and epoch), agreed with W, and no stale acceptance was recorded. E's manifest
cannot serve a post-event key: the snapshot is part of the key.

## 5. Construction, call boundary and scale

### 5.1 `first_conflict` is quadratic; the bounded construction is linear

The pinned crate's `first_conflict` groups key/outcome pairs with a linear scan
over a vector of keys. It is the primitive behind EKBLOOM's key-sufficiency
law. Measured (single host, release build, in the bench court):

<!-- BEGIN GENERATED:ekbloom-context-quadratic (scripts/perf_numbers.py) -->
| Keys | `first_conflict` ms |
|---:|---:|
| 1,000 | 0.13 |
| 4,000 | 1.71 |
| 16,000 | 25.62 |
| 64,000 | 398.56 |
<!-- END GENERATED:ekbloom-context-quadratic -->

The bounded construction (one hash map, first-seen order) at the declared
sizes:

<!-- BEGIN GENERATED:ekbloom-context-bounded (scripts/perf_numbers.py) -->
| Keys | Bounded certificate ms |
|---:|---:|
| 1,000 | 0.021 |
| 100,000 | 2.625 |
| 1,000,000 | 29.249 |
<!-- END GENERATED:ekbloom-context-bounded -->

At 64k keys the quadratic baseline is already hundreds of milliseconds and its
measured growth is quadratic; the 100k and 1M rows were not timed and are
reported as a projection, not a measurement. The bounded construction is used
by the harness for every certificate; no quadratic construction runs inside the
court.

### 5.2 The manifestation at 1k / 100k / 1M keys

<!-- BEGIN GENERATED:ekbloom-context-scale (scripts/perf_numbers.py) -->
| Keys | EKBLOOM build ms | source B | manifested B | EKBLOOM p50 ns | index p50 ns | gap × |
|---:|---:|---:|---:|---:|---:|---:|
| 1,000 | 1.6 | 167,168 | 60,896 | 1,643 | 20 | 82.2 |
| 100,000 | 181.5 | 19,981,312 | 6,209,024 | 1,633 | 30 | 54.4 |
| 1,000,000 | 1,558.1 | 166,221,824 | 60,085,000 | 1,784 | 30 | 59.5 |
<!-- END GENERATED:ekbloom-context-scale -->

EKBLOOM construction grows linearly with the source and the lookup is flat at
1.6–1.8 µs p50 across three orders of magnitude. The conventional index is
55–90× faster per lookup at every size. The 1M table fit the court: 166 MB
source, 60 MB manifested file, 1.6 s to build. **Largest size run: 1,000,000
keys.** No size was skipped; the quadratic baseline's ceiling is the measured
64k row.

### 5.3 The call boundary

`OpenObject::read_into` exists, but its file-backed dispatch tries checkpoint
replay before the directly-addressable core; on this body the plan exposed no
resident surface (`manifest_resident_surface = false`), so the engine's
`read_into` fell through to `read_counted`. The allocation counter shows the
cost:

<!-- BEGIN GENERATED:ekbloom-context-alloc (scripts/perf_numbers.py) -->
| Arm | allocations/request | bytes/request | allocation calls | allocated bytes | requests |
|---|---:|---:|---:|---:|---:|
| W | 271.7 | 25,485 | 19,564,233 | 1,834,933,145 | 72,000 |
| M-LRU | 149.2 | 13,902 | 10,739,794 | 1,000,948,736 | 72,000 |
| E | 164.1 | 84,342 | 11,811,686 | 6,072,652,253 | 72,000 |
<!-- END GENERATED:ekbloom-context-alloc -->

E allocates more per request than M and, because the manifest path allocates
internally, more bytes per request than W. The 208-byte key area and the
64-byte slot are read into caller buffers, but the pinned crate's internal
dispatch still allocates. A future implementation that exposes a resident
slice for this profile would remove that allocation; this court did not find
one.

## 6. Lifecycle: the manifestation is collectable and never load-bearing

<!-- BEGIN GENERATED:ekbloom-context-lifecycle (scripts/perf_numbers.py) -->
Replay **2,000** requests after deleting the manifestation: **0** verdict mismatches; final manifestation **969,576 B**; the candidate-only rule holds.
<!-- END GENERATED:ekbloom-context-lifecycle -->

After the measured run, the lifecycle phase deleted the final manifestation,
dropped the cache and the open table, and replayed 2,000 requests. Every
verdict and reason matched both the pre-deletion answer and a fresh W
evaluation. The manifest is a cache of deterministically recomputable
decisions; correctness never depends on it surviving. This is the lifecycle
contract the contract promised: a generation is immutable, and the whole
generation is collectable at any time.

## 7. Every loss

1. **G2 is lost, decisively.** E's p99 is **4.08×** M's, not 0.90×. The
   manifest is a slower path than an exact-keyed in-memory cache.
2. **E's p50 is worse than M's.** The manifest consumes cache budget; the
   remaining cache is smaller and the manifest lookup costs more than a cache
   probe.
3. **E's retained bytes are not free.** The manifestation is **417,176 B** at
   the end of the run, and E stays under the budget only by giving the cache
   less than M has. The manifest entry (272 B) is larger than a cache entry
   (256 B), so the manifestation cannot win on compactness either.
4. **The manifestation costs construction.** **3,433 ms** over **306** rebuilds
   in the measured run, all outside request latency and reported separately. A
   service that rebuilt this table at the same cadence would pay that CPU.
5. **The manifest's hit path allocates.** E allocates more bytes per request
   than W because the pinned crate's engine fell back to `read_counted`; the
   caller-buffer API does not by itself remove the allocation.
6. **`first_conflict` does not scale.** It is quadratic in distinct keys and
   was stopped at 64k; the bounded construction is required for the declared
   1M certificate.
7. **The conventional index wins the lookup axis at every size.** 20–30 ns
   versus 1.6–1.8 µs is a 55–90× gap that the manifestation's persistence does
   not close in this workload.

## 8. Non-claims

* Not a fleet, cloud, multi-host, multi-writer, media or power-loss result:
  one host, one process at a time, one local POSIX backend, OOM-protected
  Docker court.
* Not an external implementation: the rule, the subject record, the contract
  and the decision schema are a **prototype** built for this court, not
  normative, not exported to the subprojects, and not a wire value.
* Not a codec or tensor-bytes comparison, and no statement here revises the
  retired E3 verdict.
* Not a claim that a capability is impossible elsewhere. The opponent is the
  shipped resolver plus a strong conventional cache; if that cache matches or
  beats E, that is the published result.
* Not a security result: the salt is an identifier, not a secret; no timing,
  size or access-pattern privacy is claimed. The `.ekb` file's integrity is the
  pinned crate's; it is not an authorization boundary.
* No Go or independent implementation of this rule; no `closure` or `impact`
  phase (both refuse with `not implemented in this subphase`).
* The 1 MiB budget, the 8,000-request mix and the 9-repetition sample are
  declared operating points, not universal ones.

## 9. Reproduce

```sh
scripts/docker-court.sh bench "cd /work && bash scripts/ekbloom_context_court.sh --phase=decisions"
scripts/docker-court.sh bench "cd /work && bash scripts/ekbloom_context_court.sh --phase=scale"
scripts/docker-court.sh bench "cd /work && bash scripts/ekbloom_context_court.sh --phase=lifecycle"
```

The harness pins `ekbloom = "=0.7.3"` and builds with `--locked`; the exact
crate checksum is in the receipt. The shipped dependency graph contains no
`ekbloom`: the court logs `shipped graph ekbloom mentions=0` from
`cargo tree -e normal`. The unit tests for the caches and the manifest
candidate-only rule run in the court before the measurement (3 passed).

## 10. Pre-measurement corrections

The contract retains its corrections verbatim: 51 contracts (not 48), the
208-byte manifest key area (not 144), the enlarged fixture that satisfies the
declared >4,096-distinct-decisions pressure condition, and the 10 % arena
reserve that keeps E at or below M's retained bytes. They were made before any
receipt existed. The contract's gates, margin, request count, bucket shares and
retirement rule were never changed.
