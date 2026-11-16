# X14 — indexed semantic candidate discovery

Receipt: `bench/receipts/semantic-index-20261115T235917Z.json` (stamp
`20261115T235917Z`, WeftFS revision `4ab9ed2`). Raw log beside it. Module
documentation: [`src/semantic_index.rs`](../src/semantic_index.rs).

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
rust court. One synthetic store per run: 12 family-A variants (perturbed every
8th, 16th, … 96th element), 8 same-shape distractors, 200 different-shape
fillers, 220 objects, 5 queries of 1 MiB each. Family-A perturbations change only
low mantissa bits, and the fixture pins a finite BF16 exponent: the default
policy preserves NaN/inf words bit-exactly, so a random exponent would give
tensors that differ only in low mantissa bits different semantic roots and the
fixture would measure nothing.

Five arms, all on the same store and the same planner options:

| arm | discovery |
|---|---|
| `none` | no candidate list, no index, implicit scan off (the current default) |
| `scan_default_bound` | implicit scan with `max_candidates = 8` (the historical default) |
| `scan_all` | implicit scan with `max_candidates = 20` (every compatible object) |
| `index` | the persisted fingerprint index at `max_candidates = 8` |
| `stale_index` | the index after every family-A tensor was unlinked |

`lookup_ms` includes reading the returned candidate tensors (8 MiB per query);
the fingerprint comparison on its own is reported by the stale arm, where no
candidate bytes are read because every entry is stale.

## 3. Results

<!-- BEGIN GENERATED:semantic-index-table (scripts/perf_numbers.py) -->
| arm | note | semantic B | physical B | plan p50 ms |
|---|---|---:|---:|---:|
| none | no discovery (current default) | 0 | 2,624,550 | 45.4 |
| scan_default_bound | historical default bound (`max_candidates = 8`) | 715,251 | 715,251 | 94.1 |
| scan_all | every compatible object (`max_candidates = 20`) | 529,563 | 529,563 | 157.1 |
| index | the X14 index at the same bound as the historical scan | 529,563 | 529,563 | 99.3 |
| stale_index | after the family was unlinked: every entry stale | 0 | 2,624,550 | 45.6 |
<!-- END GENERATED:semantic-index-table -->

<!-- BEGIN GENERATED:semantic-index-prose (scripts/perf_numbers.py) -->
The court ran 6 of 6 checks and all passed. Building the index over 220 live tensors read 21,022,720 B and took 56.6 ms, producing a 51,786 B record (257.3 µs per entry). A lookup that returns candidates costs 18.22 ms p50 because it reads the candidate tensors; the fingerprint ranking alone costs 0.547 ms. The index and the exhaustive scan chose the same representations: 529,563 B of semantic-delta payload, 529,563 B of physical bytes, with planning 99.3 ms p50 versus 157.1 ms for the exhaustive scan (57.7 ms more) and 5.3 ms more than the historical default bound.
The exhaustive scan chose the same 529,563 B. Without discovery the same five queries cost 2,624,550 B in 45.4 ms — 2,094,987 B more. The historical default bound cost 715,251 B in 94.1 ms, missing 185,688 B (35.1 % of the achievable saving), while the index at the same bound recovers it exactly. With a stale index the lookup skipped 40 entries, served no bytes, and the plan fell back to 2,624,550 B in 45.6 ms (miss rate 1.00).
<!-- END GENERATED:semantic-index-prose -->

## 4. Losses

1. **The index is `O(objects)` to build and reads every tensor once.** The build
   cost and record size are in the generated paragraph above; it is a derived
   cache written on demand, not a per-commit structure, and nothing rebuilds it
   automatically.
2. **A lookup reads whole candidate tensors.** The quoted lookup time includes
   the candidate bytes; the fingerprint comparison on its own, measured by the
   stale arm where no bytes are read, is three orders of magnitude smaller. On a
   large tensor a lookup is therefore not free even when it returns nothing
   useful.
3. **The indexed plan is slower at p50 than the historical bounded scan** on this
   fixture, because both price the same number of candidates but the index ranks
   them by fingerprint while the scan stops at the first eight in object-id
   order. The index buys correctness of choice at a small, measured planning
   cost — it is not a free win on plan time.
4. **A stale index misses the entire saving.** After the family was unlinked the
   lookup skipped every entry, served no bytes, and the planner fell back to the
   next-best representation: the full semantic saving lost, a 100 % miss rate.
   The fallback is safe, not free.
5. **The historical default bound misses savings the index recovers.** The
   implicit scan at its default candidate bound chose a larger representation
   than the exhaustive scan and the index, because it takes candidates in
   object-id order and never reaches the best base. The missed bytes and the
   share of the achievable saving are in the generated paragraph. This is why
   the index exists and why the bounded scan is not an equivalent substitute.
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
* No change to any frozen byte, mode number or root; mode 14 itself is
  unchanged.
