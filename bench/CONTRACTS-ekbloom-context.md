# EKBLOOM Context Court — frozen contract (version 1)

This contract is written and committed **before** the court is run. It fixes the
arms, the byte budget, the prototype rule, the key-sufficiency argument, the
workload, the sample count, the gates and the retirement rule. Nothing here is
edited after a measurement; a changed rule is a new contract version and a new
receipt.

**Pre-measurement corrections (version 1, before any run).** Four editorial or
implementation errors were found while building the harness, before the court
was executed. None changes an arm, a gate, a margin, the workload's request
shares or the retirement rule. (1) §5.1 said "48 consumer contracts" and then
listed 4 compatible + 9 variants + 4 absent per branch, which is 17 per branch
and **51** in total; the list is honoured and the total is corrected to **51**.
(2) §2 said the manifest key area is 144 bytes per entry; a manifest hit must
also complete the post-lookup presence check without re-reading the subject
scope record, so the key area is **208 bytes** (the 144-byte key material plus
the 32-byte page id and the 32-byte page content root). A manifest entry is
therefore **64 + 208 = 272 bytes**, larger than a cache entry. This correction
makes E's byte position *worse*, not better. (3) The first smoke fixture (512 KV
pages, 96 hot pairs, 512 warm pairs) produced only about 3,300 distinct
decisions, below the §5.2 declared pressure condition of more than 4,096. The
fixture is enlarged to 2,048 KV pages and 128 tensor regions per branch and the
hot/warm sets to 2,048/4,096 pairs. The request count, bucket shares, events,
samples, arms, gates and margin are unchanged. (4) E's cache arena reserves 10 %
of `B - manifest_bytes` for hash-map capacity rounding, so E's total retained
bytes stay at or below M's; the cache itself is sized by an exact fit. This
tightens E, never widens it. All four corrections were made before any receipt
existed.

**Read against.** WeftFS `20640d3aca9de377293640352ba2ec70ad7e13a2`,
WeftCodec `d808f254ca9ead037bacd3fb937e2b5d3d3f5abb`,
WeftCodec-Go `f7b9de1bf1685d10fe186499d71d4b1b692baa1e`,
WeftFS-Go `eda27bf89cf55a5fee4489fbed5cca12d6c8aaa1`. EKBLOOM repository
`github.com/infinityabundance/EKBLOOM`, revision
`8275a41b72c17abdc55bf74b82f319876e587ba0` ("Release 0.7.3"), crate
`ekbloom 0.7.3`, crates.io checksum
`1c56ae95137ee95cdd8a3c26006aec3281752b23c150a5243152e2370e965da4`.
The harness pins `ekbloom = "=0.7.3"` and builds with `--locked`; substitution —
a vendored tree, a patched source, a different revision — is forbidden.

**Relationship to the retired tensor-codec trial.** This is a **fresh court with
its own contract**. It does not revise, reopen, re-scope or rehabilitate the
retired EKBLOOM tensor-codec trial (`bench/CONTRACTS-ekbloom.md`, E1–E3,
`docs/EKBLOOM-PLAN.md`, `bench/RESULTS-ekbloom.md`). That trial measured
*representation* of tensor bytes and lost space and bounded random access. This
court asks a different question: whether a manifested, certified **decision**
(not payload) can make reasoning over AI state cheaper than a strong
conventional cache of the same decisions at the same retained-byte budget. A
result here changes nothing in that trial's verdict.

**Why the court exists.** Programme rule 14 makes the context plane the product
and the substrate secondary. The one thing a context plane can certify that a
plain object store cannot is a *compatibility decision*: whether a stored
artifact is valid for a consumer under a model, tokenizer, attention,
positional, geometry, token-history and tenant contract. EKBLOOM's defining
idea is *spend manifested space to maximise speed*. The untested question is
whether that trade can accelerate **decision reuse**. The opponent is not a
strawman: it is the shipped resolver plus a real bounded cache (LRU **and**
W-TinyLFU), because EKBLOOM's own `docs/DB-INTEGRATION.md` reports that a plain
encoding lost to raw/LZ4/dictionary-zstd for point gets, and its
`docs/CONTROLLER.md` reports that it does not beat an equal-budget LZ4 cache on
warm reads.

Harness: `bench/ekbloom-context-harness/` (an independent crate, excluded from
the workspace). Runner: `scripts/ekbloom_context_court.sh`. Receipt schema:
`weft.ekbloom_context.v1`.

---

## 1. The arms

One backend — the local POSIX data plane of the WeftFS store
(`BackendKind::Local`), with the ordinary commit protocol and `fsync` of the
file and its directory — used three ways. Each arm gets its own byte-identical
copy of the same base store and replays the same state-change events at the same
request indices. One process, one thread, one request at a time.

### W — shipped resolver, no cross-request reuse (today's baseline)

For every request, W:

1. resolves the subject object in the current state (`Store::object_meta`);
2. reads the **subject scope record** `weft.kv.exact-prefix.subject.v1` from the
   store (a content-addressed canonical WCB object);
3. resolves the store's provenance-bound descriptor with
   `Store::kv_prov_descriptor` and checks that its `provenance_root` equals the
   one named by the subject scope record (a shipped descriptor decode, including
   the descriptor-id check);
4. checks the current authority record for the subject's tenant and branch;
5. compares every required dimension and returns `exact`, `denied` or
   `unproven`.

W never reuses a decision across requests. It is the oracle for G1.

### M — W plus a strong conventional bounded cache of certified decisions

M caches the decision for the **exact request key** (§4) and is run in two
variants, both mandatory:

* **M-LRU** — a byte-budgeted LRU with a fixed-size entry arena.
* **M-W-TinyLFU** — a byte-budgeted W-TinyLFU (1 % window LRU, 20/80 probation
  and protected segments, 4-row 4-bit Count-Min sketch) with the same arena.

Both are generation/revocation-epoch scoped: the key contains the snapshot, the
authority record root and the authority epoch, and every hit is re-checked
against the **current** snapshot, authority record and subject presence before
its verdict is returned. A hit is never a licence to skip those checks.

### E — M plus EKBLOOM-manifested certified results

E is M (same cache implementation, same admission/eviction rules, same
re-checks) plus a manifested **certified decision table** built with the pinned
`ekbloom` crate:

* the table is a fixed-width slot array followed by an exact full-key area,
  encoded as one `.ekb` object with `create_auto_within_budget` and
  `ManifestationProfile::Sparse { interval: 1024 }` (the profile the retired
  trial used, chosen before measurement for determinism, not tuned);
* E reads through the crate's **bounded engine** (`ekbloom::engine::ReadyObject`
  + `Engine::read_into`) on the in-memory `EkbloomBody` produced by
  `create_auto_within_budget`. The `.ekb` file is still written and its length
  is reported; the retained accounting charges the manifestation once, at
  `max(file_bytes, body.physical_bytes())`. The file-backed
  `OpenObject::read_into` path is measured as a diagnostic and reported beside
  E, because its dispatch tries checkpoint replay before the
  directly-addressable core; the contract asks for the caller-buffer path when
  the crate exposes one. The engine path is the gating E.
* lookup order is **cache → manifestation → evaluate**; the manifestation is an
  overflow for entries the cache has evicted, not a replacement for the cache;
* the conventional cache budget is `B - manifest_bytes` (recomputed after every
  rebuild), so cache + manifestation never exceeds `B`;
* rebuilds run **between** requests, never inside a timed request, when the
  number of newly certified decisions since the last build reaches 128 or the
  pending set would exceed the manifest cap. A rebuild writes a new `.ekb`, opens
  and verifies it, swaps it in atomically, and then deletes the previous file.
  Build time is reported separately (`manifestation_build_ms`) and is **excluded**
  from per-request latency; total wall time including builds is also reported.

The manifesto body is immutable per generation. E's table is *collectable*: the
whole generation may be dropped at any time and rebuilt from certified decisions
without changing any answer. This is EKBLOOM's non-withdrawal property applied
to one generation; it is not a claim that a manifested object can be partially
withdrawn.

### The mandated sensitivity control (not a contender)

**C-naive** is a deliberately insufficient cache keyed only by
`(subject_root, contract_root)`, with no snapshot and no authority epoch, and no
post-lookup re-checks beyond presence. It exists only to prove that the workload
and the oracle *can* detect a G1 violation. It is not an arm, its numbers are
never compared to E, and it must produce at least one false acceptance on the
declared stream. If it produces zero, the court is **inconclusive** and cannot
report a pass.

---

## 2. The declared retained-byte budget

**B = 1,048,576 bytes (1 MiB) per arm**, counted as retained bytes:

* M: `entries × 256` (fixed arena slot) + `index_capacity × 32` + sketch bytes
  (W-TinyLFU: 4 rows × 16,384 counters × 4 bits = 32,768 B). The arena is
  preallocated, so the accounting is exact and does not depend on the allocator.
* E: the same cache accounting for `B - manifest_bytes`, plus the current
  `.ekb` file length (the whole file, including header, core, manifested
  structures, key area and checksums). The pending buffer is capped at 128
  entries × 256 B = 32,768 B and is included in E's retained bytes.

A cache entry is 256 B and contains the 32-byte decision key, the verdict, the
decision root, the evidence roots and the checked snapshot/epoch. The manifest
slot is 64 B (16-byte key prefix, 8-byte key-area offset, verdict, guarantee and
reason codes, decision-root prefix); the key area is 208 B per entry
(rule/version, subject root, contract root, snapshot, authority root, epoch,
page id, page content root). A manifest entry is therefore **272 B**, more than
a cache entry. The manifestation therefore retains **more** bytes per entry than
the cache; E cannot win by compactness. That asymmetry is declared and is not a
mistake.

**Why 1 MiB.** The declared workload produces more distinct decisions than
`B / 256 ≈ 4,096` entries by construction (§5.2), so M must evict and a p99 can
be attributed to misses. A budget that held the whole working set would make the
p99 comparison vacuous. 1 MiB is also large enough that neither variant is a
token cache: 4,096 certified decisions is a real service's hot set.

---

## 3. The gates, declared now

### G1 — hard, absolute: zero incorrect acceptances

For every request in every repetition, each of M-LRU, M-W-TinyLFU and E must
return the **same verdict as W** (`exact`, `denied` or `unproven`). A
`false_accept` is a request where the arm returns `exact` and W returns anything
else. A `semantic_mismatch` is any verdict disagreement, including
`exact` → `denied`/`unproven`, `denied` → `unproven`, and the reverse.

G1 holds iff, over every arm and every repetition:
`false_accepts = 0` **and** `semantic_mismatches = 0`.

Any nonzero value fails the court outright. The performance gates are not
evaluated for a decision if G1 fails; the receipt still reports every measured
number.

### G2 — p99 total request time, E versus M

Over all measured requests (§5.4), let `p99_M = min(p99_M-LRU, p99_M-TinyLFU)`
(the stronger conventional baseline) and `p99_E` be E's p99.

**G2 holds iff `p99_E ≤ 0.90 × p99_M`, i.e. E reduces p99 by at least 10 %.**

If G2 does not hold, E is **not worse** (`non-inferior`) iff
`p99_E ≤ 1.10 × p99_M`. Non-inferiority is reported but is **not** a win:
the retirement rule in §8 fires on G2.

**Justification of the 10 % margin, before measurement.** It is not chosen from a
desired result:

1. The largest published court reproducibility spread in this programme is
   1.6 % (codec median absolute relative difference; store 0.8 %, fio 1.1 % —
   `AGENTS.md`, P7 stability record). A 10 % p99 reduction is more than six
   times that spread, so it cannot be explained by run-to-run noise on one host.
2. It is a small fraction of the reuse win the shipped decoded-object cache
   already measured: R8 reported 6,440.7 ms → 1,140.6 ms, a 5.65× p50
   improvement at 3.02 GB → 107.5 MB of backend reads
   (`bench/receipts/cache-20261106T091322Z.json`). E is asked to capture roughly
   one tenth of a p99 effect of that size, not a new order of magnitude.
3. It is a *p99* margin, not a p50 margin. A cache that only improves the
   median does not change the tail; the gate deliberately tests the tail, where
   eviction and re-evaluation are visible.

No gate is widened, retuned or re-scoped after a receipt exists.

### G3 — retained bytes

G3 holds iff, in **every** repetition:
`E_retained ≤ B` **and** `E_retained ≤ M_retained` for both M variants.
Retained bytes are the accounting in §2, measured at the end of the request
stream before any post-run cleanup. A court that only meets the budget on the
median fails G3.

### G4 — correctness under change

Four state-change events occur at fixed request indices (§5.3): a model update,
a capability revocation, a missing dependency, and a new snapshot. For each
event and each of M and E:

1. at least one request after the event must be a **re-evaluation** (not a
   cache or manifest hit whose key predates the event);
2. `stale_accepts = 0`, where a stale acceptance is a hit that returns `exact`
   while the decision's `evaluated_snapshot` or `authority_epoch` differs from
   the current one;
3. the arm's verdict on the first affected request must equal W's.

G4 holds iff all three conditions hold for all four events and both arms. A
stale acceptance also fails G1.

---

## 4. The prototype rule and the key-sufficiency argument

### 4.1 `weft.kv.exact-prefix.v1` (prototype, not normative)

This rule is built for the court. It is **not** added to the wire format, the
specification, the planner or the shipped public API. It consumes the shipped
mode-19 provenance dimensions.

A **subject scope record** is a canonical WCB object
`weft.kv.exact-prefix.subject.v1` naming the subject KV page's store descriptor
id and the eight provenance dimensions: model root, adapter-set root, tokenizer
root, attention `(head_count, head_dim, sliding_window, rope_theta_bits,
rope_scale_bits)`, positional policy, geometry `(dtype, page_rows,
layer_count)`, token-history root over the page's prefix, tenant, and sharing
salt. It also names the mode-19 codec `provenance_root`.

A **consumer contract** is a canonical WCB object
`weft.kv.exact-prefix.contract.v1` naming the same dimensions, each of which may
be **absent**. An absent required dimension is not a wildcard.

The verdict is one of:

* `exact` — every required dimension is present in both the contract and the
  subject, all are equal, the subject's page object exists at the evaluated
  snapshot with the recorded content root, and the authority record currently
  grants the tenant/root;
* `denied` — every required dimension is present and at least one differs, **or**
  the authority record refuses the tenant/root;
* `unproven` — a required dimension is absent from the contract or the subject,
  the subject's evidence (scope record, page object, or codec descriptor) is
  missing or unverifiable, or the authority record is missing. The rule never
  guesses and never falls back to a partial match.

A decision is a canonical WCB record `weft.kv.exact-prefix.decision.v1`:
`{schema, rule, rule_version, request_key, verdict, established_guarantee,
evidence:[{kind, root}], evaluated_snapshot, authority_epoch, reasons}`.
`established_guarantee` is a stable code (`all_dimensions_equal_and_evidence_verified`,
`dimension_mismatch:<name>`, `missing_dimension:<name>`, `evidence_unavailable`,
`authority_refused`, `authority_unavailable`). The decision root is
`H("weft/kv/exact-prefix/decision/v1", WCB(decision))`.

### 4.2 The compact key

```
key_material = WCB({
  rule: "weft.kv.exact-prefix.v1",
  rule_version: 1,
  subject_root: <subject scope record root>,
  contract_root: <canonical consumer contract root>,
  snapshot: <the commit id at which the decision is evaluated>,
  authority_root: <canonical authority record root for the subject's branch>,
  authority_epoch: <u64 carried inside that authority record>,
})
request_key = H("weft/kv/exact-prefix/key/v1", key_material)
```

The manifest's 16-byte **prefix** is `BLAKE3(request_key)[0..16]`. It is a
lookup accelerator only. A prefix hit reads the exact 144-byte key material from
the key area and compares it byte-for-byte before returning a verdict. A Bloom
filter or any approximate structure may return candidates; it may **never**
produce an accept. This is asserted in code and in a test.

### 4.3 Why the key is sufficient — the argument enforced in code

The verdict is a pure function of `(rule_version, subject scope record content,
contract content, snapshot state, authority record content)`. Each of those is
content-addressed:

* the subject scope record root and the contract root are hashes of canonical
  WCB bytes, so same root ⇒ same bytes;
* the snapshot is the commit id: it fixes object presence, object content roots,
  descriptor ids and tips;
* the authority record root is the hash of the canonical grant record and the
  epoch it carries.

Therefore two requests with the same `request_key` have the same inputs and the
same verdict. No dimension is omitted: model, adapter set, tokenizer, attention,
positional, geometry, token history, tenant and salt all live inside the subject
and contract roots.

The key contains **only** content-addressed, immutable-at-that-snapshot
material. Anything mutable is checked **after** the lookup:

* the current head must still equal `snapshot` (otherwise the key itself is
  different, but the check is enforced anyway);
* the current authority record root and epoch must equal the key's, and the
  current authority must still grant the tenant/root;
* the subject object must still exist in the current state, and (for W) its
  descriptor must still resolve to the recorded provenance root.

If any post-lookup check fails, the entry is **rejected** and the request is
re-evaluated. A cache or manifest entry can therefore be stale, and the arm is
still correct; it can never turn a stale entry into an accept.

### 4.4 What is deliberately not in the key

Per-request caller data that cannot change the verdict (request id, wall-clock
arrival time, retry count, trace id) is not in the key. It is not a dimension of
the decision and cannot change it. This exclusion is stated so no reader thinks
a missing field was overlooked.

---

## 5. The workload

Synthetic but structurally realistic, deterministic seeds, declared here before
measurement. No public model weights are needed: the rule consumes identity
dimensions, not tensor values.

### 5.1 Fixture

* **3 model branches** `b0`, `b1`, `b2`, each with its own model root, tokenizer
  root, attention configuration, positional policy, and tenant
  (`tenant-b0`, `tenant-b1`, `tenant-b2`).
* **192 Context Roots** (64 per branch), each naming a subject set.
* **6,144 KV-page subjects** (2,048 per branch) and **384 tensor-region keys**
  (128 per branch), written as mode-19 provenance-bound pages through the shipped
  `Store::write_kv_prov_page`, 1,024 logical bytes per page, one page per
  object. Each has a subject scope record. Twenty-four additional subjects (8
  per branch) have a scope record and **no page object** from `S0`.
* **51 consumer contracts** per the rule: per branch, 4 compatible contracts
  (differing only in the token-history root), 9 single-dimension variants
  (`model`, `adapter`, `tokenizer`, `attention`, `positional`, `dtype/geometry`,
  `tenant`, `token_history`, `salt`), and 4 contracts with one required
  dimension absent. A variant is a valid contract that simply does not match.
* One **authority record** per branch: `{scope_root, epoch, tenant, allowed
  subject roots, revoked}`.
* One base-store commit id `S0`; every arm gets a byte-identical copy.

### 5.2 Request mix (8,000 requests per repetition)

| Bucket | Share | Requests | Correct verdict |
|---|---:|---:|---|
| hot exact | 55 % | 4,400 | `exact` |
| warm exact | 20 % | 1,600 | `exact` |
| cold exact | 10 % | 800 | `exact` |
| incompatible variant | 9 % | 720 | `denied` |
| missing required dimension | 3 % | 240 | `unproven` |
| missing evidence | 3 % | 240 | `unproven` |

Hot requests are drawn from a fixed 2,048-pair set under a deterministic
Zipf-like weight (80 % of hot draws hit 512 pairs). Warm requests use a fixed
4,096-pair set uniformly. Cold requests are uniform over all `(subject,
compatible contract)` pairs and include pairs never seen before. The two
`unproven` buckets use contracts with an absent dimension and the 24 subjects
whose page object is absent from `S0` (missing dependency by construction),
respectively.

Distinct decisions exceed `B / 256 ≈ 4,096` by construction; the cold bucket and
the variant buckets fill the tail. (The first smoke fixture, with 512 KV pages
and the original 96/512 hot/warm sets, produced only about 3,300 distinct
decisions — below this declared pressure condition. That was a pre-measurement
fixture defect, corrected here by enlarging the subject pool and the hot/warm
sets; no arm, gate, margin, request count or bucket share changed.)

### 5.3 State-change events (fixed request indices)

| Index | Event | State change | Expected verdict change |
|---:|---|---|---|
| 2,000 | model update on `b1` | new model root for `b1`; the `b1` compatible contracts are republished under the new root; a note object is committed (head advances) | old `b1` subjects become `denied`; W is the oracle |
| 3,600 | capability revocation on `b2` | authority record for `b2` is republished with `epoch + 1` and `revoked = true`; committed (head advances) | every `b2` verdict becomes `denied` |
| 5,200 | missing dependency | the page object of one hot `b0` subject is deleted; committed (head advances) | that subject becomes `unproven` |
| 6,800 | new snapshot | a named snapshot of the current state is published (head advances) | no logical verdict changes; every key changes because the snapshot is in the key |

Each event is applied to the arm's own store copy at the same stream index, by
the same deterministic writer with the same fixed timestamp and seed. The
request stream is generated **before** any arm runs, from seed
`0xEKB10C0DE5EED0001 + repetition`, and every arm receives the identical stream.

### 5.4 Repetitions, samples and interleaving

* **9 repetitions**. Repetition 1 is cold-first (fresh process, no warm-up);
  repetitions 2–9 are preceded by one untimed warm-up pass over 512 requests.
* **72,000 measured requests per arm** (9 × 8,000). p99 is nearest-rank over the
  pooled samples, so the p99 cell is supported by 720 samples. This is the
  declared sample count; no arm gets a different count.
* Arm order rotates by one position per repetition over
  `[W, M-LRU, M-TinyLFU, E]`, and C-naive runs after the four contenders in each
  repetition. Every order is recorded in the receipt.
* Each repetition uses a fresh base store; each arm uses a fresh copy and a fresh
  process.
* Percentiles are nearest-rank. Latency is `Instant` around the whole request
  (key construction, lookup, post-lookup checks, evaluation on a miss, verdict
  construction). `n`, `min`, `p50`, `p95`, `p99`, `max` and the sum are reported
  per arm per repetition.

### 5.5 Receipt format

`bench/receipts/ekbloom-context-<stamp>.json` schema
`weft.ekbloom_context.v1`, with: `contract` path and `contract_sha256`;
`generated_utc`; `environment` (court image and id, kernel, memory/cpus/pids,
uname, the four repository revisions); `ekbloom` (crate version, checksum,
resolved `Cargo.lock` checksum, `manifestation_profile`);
`fixture` (branch/tenant/root/subject/contract counts, base-store head, subject
digest, contract roots, page digests); `methodology` (arms, budget accounting,
interleaving, repetitions, samples, events); `arms` with per-repetition and
pooled metrics: `n`, `p50/p95/p99/max_us`, `total_us`, `nodes_traversed`,
`backend_bytes`, `backend_requests`, `allocations`, `alloc_bytes`,
`cache_hits`, `cache_misses`, `manifest_hits`, `manifest_misses`,
`evaluations`, `false_accepts`, `semantic_mismatches`, `stale_accepts`,
`retained_bytes`, `manifest_bytes`, `manifest_build_ms`, `manifest_read_bytes`;
`gates` G1–G4 each with `holds` and a decisive detail; `checks`; `losses`;
`result` (measurement validity) and `result_note` (separating validity from the
claim). Raw per-request rows are written to
`bench/receipts/ekbloom-context-<stamp>.rows.jsonl.zst`, with its sha256 and row
count in the receipt. A `.log` beside the receipt carries every `CHECK` and
`GATE` line.

A failing measurement is retained with its raw log; a receipt is never
rewritten. `WEFT_EKBLOOM_CONTEXT_STAMP` overrides the stamp for a deliberate
re-run.

---

## 6. Measurement rules

1. Nothing is measured on the host. Every run is `scripts/docker-court.sh bench`
   (3 GiB / 3 GiB swap / 1,024 pids / 4 cpus / bounded `/tmp`), never raised.
2. One process per (arm, repetition); one thread; one request at a time; no
   decoded-object cache and no single-flight in the store
   (`Options::default()`), so the only reuse under test is the arm's own.
3. Backend bytes and requests are `Store::backend_stats()` deltas around the
   request. Manifest reads are counted by the harness and are **not** backend
   bytes.
4. Allocations are counted by a counting global allocator (`alloc` calls and
   bytes) around each request; the allocator itself does not allocate.
5. `nodes_traversed` is counted by the resolver: one for each subject scope
   record, codec descriptor, authority record, or manifest slot/key it inspects.
6. CPU time is the process `utime + stime`; peak RSS is `VmHWM`; cgroup peak and
   OOM/pids events are read from the court.
7. No number in prose is hand-written: `docs/perf-numbers.json` and
   `scripts/perf_numbers.py` emit every generated region from the receipt.
8. The scale phase (§7) runs in separate processes with its own bounds; an
   over-large size that cannot fit the court is reported as a measured ceiling,
   not silently skipped.

---

## 7. The court phases

`scripts/ekbloom_context_court.sh` implements:

* `decisions` (required) — the arms, workload, events and gates above;
* `scale` — key-sufficiency certificate construction and lookup at 1k / 100k /
  1M keys, the `first_conflict` quadratic construction against a bounded
  streaming construction, EKBLOOM table construction and lookup, and the
  conventional index for comparison;
* `lifecycle` — post-run: delete every manifestation, replay the measured
  request stream through E, and require identical verdicts; plus the
  insufficient-key sensitivity control and the prefix/Bloom candidate-only test.

`closure` and `impact` are **not implemented in this subphase**: the script
prints `ekbloom_context_court: not implemented in this subphase` and exits
non-zero, rather than silently passing.

---

## 8. The retirement rule

If **G1 holds** and **E does not beat M on G2** (p99 reduction below 10 %) at
equal retained bytes, the **manifested-decision direction retires**. It is
published as a loss in `bench/RESULTS-ekbloom-context.md`: no decision wire
format, no shipped decision cache, no `ekbloom` dependency, no planner slot, no
claim. A non-inferiority result is not a win and does not rescue the direction.

If G1 fails, the direction is not retired on performance: it is **refuted** as
unsafe, which is the stronger negative result. The court reports it as a failure,
not as a loss.

If the measurement is invalid (missing rows, a vacuous sensitivity control, a
limit breach, a fixture mismatch), no verdict is taken and no claim is made.

---

## 9. What this court does not establish

* Not a fleet, cloud, multi-writer, media or power-loss result: one host, one
  process at a time, one local POSIX backend.
* Not an external implementation: the rule, the subject record, the contract and
  the decision schema are a **prototype** built for this court and are not
  normative, not exported to the subprojects, and not a wire value.
* Not a claim about tensor bytes, codecs or the retired tensor-codec trial.
* Not a claim that any capability is impossible elsewhere. The opponent is the
  shipped resolver plus a strong conventional cache; if the cache matches or
  beats E, that is the published result.
* Not a security result: the salt is an identifier dimension, not a secret; no
  timing, size or access-pattern privacy is claimed, and no signature scheme is
  introduced. E's `.ekb` container is read through the pinned crate's normal
  integrity checks; it is not an authorization boundary.
* Not a production service: the workload is synthetic, the rule is one narrow
  prototype, and the dataset is generated deterministically from identity
  dimensions rather than from real model weights.
* The 1 MiB budget is a declared operating point, not a universal one. A
  different budget or request mix is a different contract and a new receipt.
