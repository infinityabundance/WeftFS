# X14 — indexed semantic candidate discovery

Receipt: `bench/receipts/semantic-index-20261116T001216Z.json` (stamp
`20261116T001216Z`, WeftFS revision `4ab9ed2`, three fresh stores). Raw log
beside it. Module documentation:
[`src/semantic_index.rs`](../src/semantic_index.rs).

## 1. The defect and the replacement

Mode 14 (`SEMANTIC_DELTA`) needs a base tensor with the same dtype, the same
shape and the same semantic root under the chosen policy. The only automatic way
to find one was `semantic_scan`, a whole-store walk that decodes every compatible
tensor. It was switched off by default because it cost `O(objects)` per tensor
written — and that silently removed the cross-version savings it had found.

X14 replaces it with an **explicit, bounded, persisted index**:

* `Store::build_semantic_index` reads each live tensor once and stores a
  fingerprint: dtype, shape, element count, two quantised byte-plane histograms,
  and a digest of a bounded head/tail sample. The record is canonical WCB
  (`weft.semantic.index.v1`) at `semantic-index.root`, content-addressed and
  digest-checked; a corrupt record is a hard error, never silently trusted.
* `Store::query_semantic_index` ranks compatible entries by fingerprint distance
  (distance, then base content id) and returns at most `limit` candidates. It
  reads no tensor bytes other than the query's own at rank time.
* The planner then prices the **real mode-14 candidate** against every returned
  base, so the index narrows the search and can only miss a saving, never invent
  one.
* A stale entry (object deleted, or its content version moved) is skipped and
  counted. The bytes are never taken from a stale fingerprint.

## 2. Method

Harness `examples/semantic_index_court.rs`, court `scripts/semantic_index_court.sh`,
rust court. Each **repetition builds a fresh store** of the same fixture: 12
family-A variants (perturbed every 8th, 16th, … 96th element), 8 same-shape
distractors, 200 different-shape fillers, 220 objects, 5 queries of 1 MiB each.
Family-A perturbations change only low mantissa bits, and the fixture pins a
finite BF16 exponent: the default policy preserves NaN/inf words bit-exactly, so
a random exponent would give tensors that differ only in low mantissa bits
different semantic roots and the fixture would measure nothing.

Repetitions matter here. ObjectIds are drawn from a random seed, so the
**implicit scan's candidate order (sorted by ObjectId) changes every run** while
the fingerprint index does not. A single run would measure one lucky order.

Six arms, all on the same store and the same planner options:

| arm | discovery | candidate bound |
|---|---|---|
| `none` | no candidate list, no index, implicit scan off (the current default) | 16 |
| `scan_bound8` | implicit scan, half the shipped default | 8 |
| `scan_default` | implicit scan at the shipped default | 16 |
| `scan_all` | implicit scan over every compatible object | 20 |
| `index` | the persisted fingerprint index at the shipped default | 16 |
| `stale_index` | the index after every family-A tensor was unlinked | 16 |

`lookup_ms` includes reading the returned candidate tensors (8 MiB per query);
the fingerprint comparison on its own is reported by the stale arm, where no
candidate bytes are read because every entry is stale.

## 3. Results

<!-- BEGIN GENERATED:semantic-index-table (scripts/perf_numbers.py) -->
| arm | note | semantic B | physical B | plan p50 ms |
|---|---|---:|---:|---:|
| none | no discovery (current default) | 0 | 2,624,550 | 45.9 |
| scan_bound8 | implicit scan, half the shipped default | 529,563 | 529,563 | 95.5 |
| scan_default | implicit scan at the shipped default bound | 529,563 | 529,563 | 138.3 |
| scan_all | implicit scan over every compatible object | 529,563 | 529,563 | 159.0 |
| index | the X14 index at the shipped default bound | 529,563 | 529,563 | 141.1 |
| stale_index | after the family was unlinked: every entry stale | 0 | 2,624,550 | 65.2 |
<!-- END GENERATED:semantic-index-table -->

<!-- BEGIN GENERATED:semantic-index-prose (scripts/perf_numbers.py) -->
The court ran 8 of 8 checks and all passed. Building the index over 220 live tensors read 21,022,720 B and took 57.6 ms, producing a 51,786 B record (262.0 µs per entry). A lookup that returns candidates costs 18.60 ms p50 because it reads the candidate tensors; the fingerprint ranking alone costs 0.548 ms.
On 3 fresh stores the index chose 529,563 B every time (529,563, 529,563), exactly the exhaustive scan's 529,563 / 529,563 / 529,563 B. At the shipped default bound (16) the implicit scan also produced 529,563, 529,563 and 529,563 B. At half that bound (8) it produced 529,563, 715,251 and 529,563 B — missing 185,688 B (35.1 % of the achievable saving) on one store, because its candidate list is ObjectId order, which is random. Without discovery the same five queries cost 2,624,550 B in 45.9 ms — 2,094,987 B more than the index.
Planning: the index takes 141.1 ms p50, the exhaustive scan 159.0 ms (17.9 ms more), the default-bound scan 138.3 ms (the index is 2.8 ms slower) and the 8-bound scan 95.5 ms. With a stale index the lookup skipped 40 entries and served no bytes, losing the whole 529,563 B saving and planning in 65.2 ms (miss rate 1.00).
<!-- END GENERATED:semantic-index-prose -->

## 4. Losses

1. **The index is `O(objects)` to build and reads every tensor once.** The build
   cost and record size are in the generated paragraph above; it is a derived
   cache written on demand, not a per-commit structure, and nothing rebuilds it
   automatically.
2. **A lookup reads whole candidate tensors.** The quoted lookup time includes
   the candidate bytes; the fingerprint comparison on its own, measured by the
   stale arm where no bytes are read, is orders of magnitude smaller. On a large
   tensor a lookup is therefore not free even when it returns nothing useful.
3. **The indexed plan is not the cheapest plan.** The index prices its
   best-ranked candidates and prepares a real mode-14 frame for each; the
   default-bound scan prices an arbitrary id-ordered subset. On this fixture the
   index is a couple of milliseconds slower at p50 than the default-bound scan
   and faster than the exhaustive scan; the exact deltas are in the generated
   paragraph. The index buys a correct, stable choice at a measured planning
   cost — it is not a free win on plan time.
4. **A stale index misses the entire saving.** After the family was unlinked the
   lookup skipped every entry, served no bytes, and the planner fell back to the
   next-best representation. The fallback is safe, not free.
5. **The implicit scan's saving depends on ObjectId order, and the bound only
   narrows the window.** ObjectIds are random, so the scan's candidate list is a
   random subset taken in id order. At half the shipped default bound one of
   three stores missed 35.1 % of the achievable saving. At the shipped default
   all three stores happened to include the best base — which is not a
   guarantee, only what these three stores did; a larger bound reduces the
   window without removing the dependence on the order. The index produced the
   identical total on all three stores.
6. **The fingerprint is a prefilter, not a semantic hash.** Two tensors with the
   same histogram rank close even when their roots differ, and two tensors with
   different histograms can still share a root. The index can only order and
   truncate; correctness comes from the codec's root comparison.
7. **Rust only.** There is no Go index writer or query path, and the receipt is a
   single-host, warm-cache, synthetic measurement — no real model weights.

## 5. Non-claims

* No claim that the index finds *every* base: it is bounded by `max_candidates`
  and by the fingerprint map, so a base that is not indexed or ranks outside the
  bound is a missed saving, not an error.
* The index is not an authorization boundary: it returns candidates the caller
  may read through the ordinary object API, and skips anything unreadable.
* No claim about the absolute size win on real checkpoints; the fixture is
  synthetic and chosen to make the cross-version relationship measurable.
* Three repetitions bound the observed scan variation; they do not estimate a
  distribution. The exhaustive arm is stable because it reads every compatible
  object, which is exactly why it is too expensive to ship as the default.
* No change to any frozen byte, mode number or root; mode 14 itself is
  unchanged.
