# F7 AI-corpus cost graph — results (ai-v1, 2026-11-03)

**Status:** measured, single-host, court-bounded, synthetic corpus. This
document reports every number, loss, and caveat. It does **not** claim
superiority, a speedup, a production result, or a general AI-workload
conclusion. The corpus is synthetic and contains no Meta or production data.

## 1. Evidence index

| Artifact | SHA-256 | Role |
|---|---|---|
| `bench/receipts/ai-corpus-20261104T021627Z.json` | `7fb7d9b432d6f4e633053ad69523fc25a64278dc2f2e94d18c54842ed2b8b44f` | **Current** receipt: whole-pair-aware planner, per-object base amortization and planner rejections |
| `bench/receipts/ai-cost-graph-20261104T021627Z.json` | `9ca3dd313e5d415da5121bedaa8be7dede6255fe7949e9632fc766f430415bcd` | Cost-graph data for the current run |
| `bench/receipts/ai-cost-graph-20261104T021627Z.csv` | `5f785f1b475015d52140b41812cce1e2a5584f4d3eba76b90b358489c3f5e670` | Long-form CSV of the current rows |
| `bench/receipts/ai-corpus-20261104T021627Z.log` | `817ca4ebbbac74efb9543108a03d4ce642c5d03cc02e4f6408cb32c4fb6f377e` | Raw court log for the current run |
| `bench/receipts/ai-corpus-20261103T220113Z.json` | `f779b851c4802995a630ab1dda0bafee0ae70564c92e2793654659b9d5657de0` | Historical receipt: frame-smallest planner (superseded) |
| `bench/receipts/ai-cost-graph-20261103T220113Z.json` | `02253434c755d8c805ede59f2a21e40929706cf29ef22d8fe08ff5f735512b90` | Historical cost-graph data |
| `bench/receipts/ai-cost-graph-20261103T220113Z.csv` | `4ddf788040e247ff0d3ae6a5235281054b0fec181c5cc0fbc0d3f285cf6fc44f` | Historical CSV |
| `bench/receipts/ai-corpus-20261103T220113Z.log` | `6f7fb342240f3931abb64861d4de4fdd89bf18f8d9280ecd30c75c1dfdea427d` | Historical raw court log |
| `bench/corpora/ai-v1/MANIFEST.json` | `f88e5dfcc7ffad772f905018ed22b2cc0904894725e9b3760ee6c826b5c5af83` | Per-file sha256, sizes, typed metadata, relations, license |
| `bench/receipts/ai-corpus-cdc-sweep-20261104T192956Z.json` | `b477cb87fb05979dfe859a2cb85f49caca696e5cb556222ed59ead2b1a7ad187` | **Current** CDC average-chunk-size sweep (8/4/2/1/0.5 KiB) with pair boundary and content-reuse fractions (§9) |
| `bench/receipts/ai-corpus-cdc-sweep-20261104T192956Z.log` | `9b01878af068f17db9a545c1f98ea11a03bbb4d40195da4077c6b3bdc8a03510` | Raw court log for the sweep |
| `bench/receipts/ai-corpus-cdc-sweep-20261104T192902Z.json` | `35485a89311c286417dea85a7c42f76143011fcd0e48893210be5ed8f00dbaf5` | Pre-final sweep run; identical physical-byte totals, lacks the raw/marginal accounting fields (superseded) |
| `bench/receipts/ai-corpus-cdc-sweep-20261104T192902Z.log` | `792bc4f47a9d3e1ce631f4c3e1f38cb3920192fec15c0b323a3baca003a3ce6f` | Raw court log for the pre-final sweep |

A second run on the same corpus and code (`ai-corpus-20261103T215948Z.json`)
produced **identical physical-byte totals** and latency medians within 2.1%
(worst p95 delta 2.07%, `cdc_zstd`). It is retained as a stability comparison,
not as a headline. Earlier `215848`/`215911` receipts are superseded
development runs (one used an 8-revision chain trimmed to the default dependency
depth; one lacked the text tool-output files).

## 1.1 Current revision: whole-pair-aware planner (2026-11-04)

The 2026-11-04 run changes only the context planner. It replaces "pick the
smallest frame" with a whole-pair objective:

```
planner_cost = marginal_frame_bytes
             + (base_plaintext_bytes unless the base is amortized)
amortized    = base is a retained chain root (no declared relation)
             OR base has >= 2 direct dependents
select contextual only when planner_cost < best context-free frame bytes
```

The receipt records `planner_policy`, every `planner_rejections` entry, and
per-object `base_amortized`/`planner_cost` values. Raw `pair_bytes` and raw
`pair_losses` are still computed and published for every selected object;
nothing is hidden.

**Current aggregate (same corpus, same host, 7 iterations per object):**

| Strategy | Frame bytes | Physical bytes | Physical / visible | vs `zstd_forced` | Pair bytes | Frame losses | Pair losses | Contextual objects |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `zstd_forced` | 571,897 | 575,808 | 0.3796 | — | — | 10 | 0 | — |
| `weft_auto` | 571,802 | 575,713 | 0.3796 | −0.02% | 571,802 | 10 | 0 | 0 |
| `cdc_zstd` | 585,400 | 603,094 | 0.3976 | +4.74% | — | 8 | 0 | — |
| `weft_context` | 508,522 | 514,506 | 0.3392 | **−10.65%** | **697,014** | 10 | **8** | **8 / 60** |

Latency: median 5,150 ns, p95 36,007 ns (the previous planner's p95 was
269,625 ns because it selected deep delta chains with tiny frames).

What changed and what remains:

- Pair bytes fall from **1,367,212 (90.14% of visible) to 697,014 (45.96%)**.
- Pair losses fall from **20 to 8**. The eight remaining pair losses are all
  selected because the base is a retained chain root; their raw pair cost is
  still above the object's visible bytes, and each one is listed in the
  receipt with `base_amortized: true`.
- 24 contextual candidates were rejected because their planner cost was not
  below the context-free best. Twelve chain revisions (`a002`–`a008`,
  `c002`–`c006`) now use context-free zstd frames instead of tiny deltas against
  an unamortized intermediate revision. This is why physical bytes rise from
  the historical 176,564: the old number was bought with per-object pair costs
  that the planner no longer accepts.
- `planner_violations` is empty: no selected contextual object violates the
  planner objective.

**This is a tradeoff, not a free win.** The historical 176,564-byte store
remains in the repository as a superseded result. The new planner chooses fewer
contextual objects and a larger store because it refuses to move cost into
bases that an individual historical read must still materialize.

## 2. Corpus

Generated by `bench/corpora/ai-v1/generate.go` from a fixed seed
`0x9e3779b97f4a7c15`; `go run generate.go -check` re-verifies every SHA-256
before the benchmark. License: **CC0-1.0**, synthetic, authored for this
repository. No downloaded, scraped, production, or Meta data is present.

| Kind | Files | Source bytes | Visible bytes | Notes |
|---|---:|---:|---:|---|
| prompts | 12 | 6,632 | 6,632 | text, some `revised_from` relations |
| tool_outputs | 12 | 23,111 | 23,111 | JSON and plain text; every third is a near-duplicate |
| agent_intermediates | 9 | 443,700 | 443,700 | 48 KiB base + 8 revisions in a `derived_from` chain |
| tensors | 12 | 69,636 | 69,636 | f16/bf16/f32/f64/i8/u8/i16/i32/i64, scalar and zero-element |
| tokens | 8 | 470,139 | 46,965 | JSON CLI input; visible = canonical delta-LEB128 payload |
| checkpoints | 7 | 928,256 | 928,256 | 128 KiB base + 6 near-duplicate revisions |
| **total** | **60** | **1,939,911** | **1,516,737** | |

The source/visible difference for tokens is intentional: the corpus stores the
documented JSON input form, while the measured artifact is the canonical binary
token payload. Tensors store raw little-endian element bytes directly.

## 3. Environment and exact commands

All execution ran inside `scripts/docker-court.sh bench`; nothing was built,
tested, or benchmarked on the host.

- Court image: `weft/bench-court:1`, id
  `sha256:e80c438516a509c251d6f3ca4848363a86d12a92287a1d9284117acd9cc0d85a`.
- Limits (unchanged): `--memory=3g --memory-swap=3g --pids-limit=1024
  --cpus=4`, `--tmpfs /tmp:size=1g`, `no-new-privileges`, `cap-drop=ALL`.
  The bench court additionally has `/dev/fuse`, `CAP_SYS_ADMIN`, and two
  labelled tmpfs mounts; this workload did not use FUSE.
- Host: Linux `7.3.1-1-cachyos`, AMD Ryzen 7 9800X3D (16 host CPUs, 4-CPU
  quota), 131,565,228 KiB host `MemTotal`.
- Revisions recorded by the receipt: WeftFS
  `91409dd8e2528d06a52f3474c281b81b1a377a48`, WeftCodec
  `c632aa103509e601d916ef57230e6dc7d74fdb5f`, WeftCodec-Go
  `6778f65476babcc71d38473ea6214cd7fc8f8e74`, WeftFS-Go
  `73a671930b504fd2913887f703d5efa02d591bd8`.
- Tools: Go toolchain from the bench image for the generator check; Rust
  release build of `examples/bench_ai_corpus`; zstd via the `zstd` crate
  already pinned by `weftcodec`.

Exact commands:

```sh
# Generate (or regenerate) the corpus.
scripts/docker-court.sh bench \
  "cd bench/corpora/ai-v1 && GO111MODULE=off go run generate.go"

# Verify the committed corpus against its manifest.
scripts/docker-court.sh bench \
  "cd bench/corpora/ai-v1 && GO111MODULE=off go run generate.go -check"

# Measure. This writes the receipt, graph JSON, graph CSV, and raw log.
scripts/docker-court.sh bench "bash bench/scripts/run_ai_corpus_bench.sh"
```

The run script uses `WEFT_AI_ITERATIONS=7` by default. Each latency sample is
accepted only after a byte-for-byte decode check against the expected plaintext.

## 4. Strategies and accounting

| Strategy | What it does | Relation use |
|---|---|---|
| `zstd_forced` | WeftCodec mode ZSTD for every object, level 3 | none |
| `cdc_zstd` | gear-hash content-defined chunking (default min 2 KiB, avg 8 KiB, max 32 KiB; `WEFT_CDC_AVG_KIB` overrides the average, §9.1), global dedup by `H("weft/chunk/v1")`, zstd level 3 per unique chunk | none (content-only) |
| `weft_auto` | WeftCodec `auto` over RAW, ZSTD, and typed TENSOR/TOKENS candidates | none |
| `weft_context` | `weft_auto` plus DELTA, DICTIONARY, and REFERENCE against bases named by declared typed relations | `derived_from`, `revised_from` |

Accounting:

- `visible_bytes`: manifest-declared visible representation (element payload for
  tensors, canonical token payload for tokens).
- `frame_bytes`: per-object WFT1 frame bytes (headers, dependency tables,
  descriptors, payload/body).
- `physical_bytes`: whole-corpus stored bytes counted once: every chosen frame,
  the object index, and (for `weft_context`) the relation manifest. For CDC it
  is unique compressed chunks + 48-byte chunk index entries + per-file chunk
  manifests + the file table.
- `base_plaintext_referenced_bytes`: sum of base payload sizes for dependent
  objects, with repeats (not deduplicated).
- `pair_bytes`: per dependent object, `frame_bytes + base_bytes`. The sum can
  exceed the store-wide physical total because shared bases are counted per
  dependent object.
- `read_latency_ns`: median over verified decodes. `historical_read_ns` is the
  cold dependency-DAG path (`decode_frame_dag`, no plaintext cache) when the
  chosen frame has dependencies; otherwise the warm path. `warm_decode_ns` uses
  a pre-materialized dependency map.
- `dependency_depth` is the chosen frame's own DAG depth (`0` for
  RAW/ZSTD/TENSOR/TOKENS, `1 + base depth` for DELTA/DICTIONARY/REFERENCE).
  `relation_chain_depth` is the declared historical chain depth.

## 5. Aggregate results

All sizes in bytes; ratios use `visible_bytes = 1,516,737`.

| Strategy | Frame bytes | Physical bytes | Physical / visible | vs `zstd_forced` | Median latency | p95 latency | Max depth | Mean depth | Frame losses | Pair losses |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `zstd_forced` | 571,897 | 575,808 | 0.3796 | — | 4,258 ns | 30,687 ns | 0 | 0.00 | 10 | 0 |
| `weft_auto` | 571,802 | 575,713 | 0.3796 | −0.02% | 4,208 ns | 30,126 ns | 0 | 0.00 | 10 | 0 |
| `cdc_zstd` | 585,400 | 603,094 | 0.3976 | +4.74% | 1,683 ns | 15,198 ns | 0 | 0.00 | 8 | 0 |
| `weft_context` | 170,580 | 176,564 | 0.1164 | −69.34% | 5,199 ns | 269,625 ns | 8 | 1.05 | 10 | 20 |

What these numbers mean, and do not mean:

- `weft_context` stores **30.66%** of the `zstd_forced` physical bytes on this
  corpus because 20 near-duplicate objects (agent revisions, checkpoint
  revisions, prompt revisions, tool-output revisions) become small
  DELTA/DICTIONARY frames against declared bases. Its `pair_bytes_sum` is
  1,367,212 bytes (90.14% of visible), still below the naive representation, but
  that sum intentionally double-counts shared bases.
- `weft_auto` is effectively tied with `zstd_forced` (−0.02%); the typed modes
  did not win a single object on this corpus (§6). The small difference is the
  per-object `auto` tie-break choosing RAW where the zstd frame is one or two
  bytes larger.
- `cdc_zstd` is **worse** than forced zstd by 4.74% physical bytes here. Its
  chunk index (7,008 bytes), per-file manifests (6,895 bytes), and file table
  (3,791 bytes) outweigh the dedup it finds: 146 unique chunks for 1.52 MB
  visible. Content-defined dedup is not free; §9 measures why it finds so little
  (edit density per chunk, not boundary failure) and how a 512 B–2 KiB sweep
  changes neither that conclusion nor the sign of the result.
- CDC has the lowest median read latency (1,683 ns) because chunks are small
  and independent; `weft_context` has the highest p95 (269,625 ns, 8.95× the
  `weft_auto` p95) because a deep delta chain re-decodes every ancestor.

## 6. Typed TENSOR / TOKEN_SEQUENCE candidates (honest loss)

The typed modes are implemented and conformance-tested, but **none of the 20
typed corpus artifacts chose them**: 0/20. The descriptor+frame overhead is
272–288 bytes (median 279) on top of the exact payload. Examples:

| Artifact | Payload bytes | TENSOR/TOKENS frame | ZSTD frame | Chosen |
|---|---:|---:|---:|---|
| `tensors/f32_64x64.bin` | 16,384 | 16,663 | 5,637 | zstd |
| `tensors/f16_64x64.bin` | 8,192 | 8,470 | 6,667 | zstd |
| `tokens/walk_4096.json` | 4,096 | 4,382 | 2,280 | zstd |
| `tokens/large_20000.json` | 20,009 | 20,297 | 10,602 | zstd |

The typed modes provide exact self-description (dtype/shape/layout or
vocab_schema/encoding/count) and a payload-domain hash binding. They are not a
compression win for these small, mostly-random artifacts. This is the expected
tradeoff and is why the corpus benchmark reports it instead of hiding it.

## 7. Per-file losses

Every loss is listed in the receipt. Grouped by strategy and kind:

| Strategy | Tensor losses | Token losses | Failing examples | Cause |
|---|---:|---:|---|---|
| `zstd_forced` | 7 | 3 | `tensors/f32_scalar.bin` (113 B vs 4 B), `tokens/single.json` (110 B vs 1 B) | fixed WFT1 frame overhead (100+ bytes) exceeds tiny payloads |
| `weft_auto` | 7 | 3 | same tiny artifacts; `tensors/f32_zero.bin` (100 B vs 0 B) | RAW frame overhead |
| `cdc_zstd` | 6 | 2 | `tensors/f32_scalar.bin` (13 B vs 4 B), `tokens/single.json` (10 B vs 1 B) | zstd chunk headers on tiny inputs |
| `weft_context` | 7 | 3 | same as `weft_auto` | frame overhead |
| `weft_context` pair losses (current planner) | — | — | 8 retained-base objects (`a001`, `c001`, `p004`, `p008`, `t002`, `t005`, `t008`, `t011`) | raw pair exceeds visible, but the base is a retained chain root and is charged zero by the planner; the receipt marks `base_amortized: true` for each |
| `weft_context` pair losses (historical planner) | — | — | all 20 dependent objects | superseded frame-smallest planner |

No loss was deleted. The pair-loss result is important: context conditioning
moves cost into shared bases. It reduces the **store**, but a single historical
read still requires its base bytes, so per-object pair cost can exceed the
object alone.

## 8. Dependency depth and historical read latency

Cold dependency-DAG reads grow roughly linearly with depth on this corpus:

| Object | Mode | Frame bytes | Warm decode | Cold chain | Historical read | Depth |
|---|---:|---:|---:|---:|---:|---:|
| `agent_intermediates/a000_base.bin` | zstd | 6,533 | 19,266 ns | 19,567 ns | 19,266 ns | 0 |
| `agent_intermediates/a008.bin` | dictionary | 260 | 31,348 ns | 266,428 ns | 266,428 ns | 8 |
| `checkpoints/c000_base.bin` | zstd | 55,267 | 23,664 ns | 24,456 ns | 23,664 ns | 0 |
| `checkpoints/c006.bin` | dictionary | 1,235 | 64,360 ns | 390,992 ns | 390,992 ns | 6 |

The chain tail is the p95 driver. A consumer that needs the tail object must
pay the ancestor decodes unless it caches them; the receipt records both warm
and cold numbers so the tradeoff is visible.

## 9. CDC details, chunk-size sweep, and why it lost

> **Superseded sentence (retained for the record):** "Dedup found almost
> nothing because the synthetic near-duplicates are **localized edits**, not
> identical chunks; the revisions shift byte offsets, so the content-defined
> boundaries rarely reproduce identical chunk spans." Both mechanisms in that
> sentence are contradicted by measurement: boundaries mostly *do* reproduce,
> and the revisions never shift offsets before EOF (every edit is a same-length
> in-place overwrite; each revision appends only at the end). The measured
> explanation replaces it below (2026-11-04).

Default chunking: gear hash, min 2 KiB, average 8 KiB (`mask = 2^13 − 1`), max
32 KiB. Default run: 146 unique chunks, 585,400 compressed unique bytes,
17,694 bytes of index/manifest overhead, 603,094 physical bytes, **+4.74%**
over forced zstd. That negative result stands; the *reason* in the original
sentence did not.

### 9.1 Sweep: average chunk size 8 KiB → 512 B

Exact command (bench court, unchanged hard limits):

```sh
scripts/docker-court.sh bench "bash bench/scripts/run_ai_corpus_cdc_sweep.sh"
# one configuration only (cheap, no WeftCodec planners):
scripts/docker-court.sh rust "WEFT_CDC_AVG_KIB=2 cargo run --release \
  --example bench_ai_corpus -- --corpus bench/corpora/ai-v1 --only cdc \
  --out /tmp/cdc-2k.json"
```

`WEFT_CDC_AVG_KIB` (default 8) sets the average; min = `max(avg/4, 64 B)` and
max = `avg*4` are derived from it and recorded in the receipt's
`chunking` metadata. Corpus, zstd level, decode verification, and every other
strategy are untouched; the forced-zstd comparator (575,808 physical bytes) is
measured in the same sweep run. 512 B is included and is not noisy: physical
byte accounting has no timing component, and two independent sweep runs
produced identical totals (`ai-corpus-cdc-sweep-20261104T192902Z` and
`20261104T192956Z`).

"Effective mean" is actual chunked bytes ÷ actual chunks; gear cutoffs on this
non-uniform data yield chunks larger than the nominal average.

| Avg requested | Effective mean | Total chunks | Unique chunks | Unique raw B | Marginal compressed B | Unique compressed B | Index B | **Physical B** | vs `zstd_forced` | Boundary match | Content reuse (bytes) | Content reuse (chunks) |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 8 KiB (baseline) | 9,661 | 157 | 146 | 1,471,681 | 594,500 | 585,400 | 17,694 | **603,094** | +4.74% | 86.7% | 3.75% | 11.2% |
| 4 KiB | 7,121 | 213 | 200 | 1,446,232 | 603,801 | 590,766 | 22,078 | **612,844** | +6.43% | 85.0% | 5.88% | 9.4% |
| 2 KiB | 3,960 | 383 | 343 | 1,368,549 | 630,325 | 598,143 | 34,382 | **632,525** | +9.85% | 86.5% | 12.35% | 15.2% |
| 1 KiB | 1,927 | 787 | 645 | 1,250,035 | 678,798 | 601,343 | 61,806 | **663,149** | +15.17% | 88.6% | 22.22% | 24.5% |
| 512 B (exploratory) | 689 | 2,203 | 1,142 | 1,101,850 | 854,792 | 614,651 | 130,974 | **745,625** | +29.49% | 92.8% | 33.89% | 60.2% |

Metric definitions (per pair; 20 pairs = the manifest's `derived_from`/
`revised_from` relations; nothing selected or discarded after measurement):

- **Boundary match**: shared interior chunk starts at *identical absolute
  offsets*, Jaccard over the pair's boundaries (aggregated over pair sums).
- **Content reuse**: dependent chunk bytes whose chunk hash also occurs in the
  base. It is hash-based and therefore offset-insensitive: a chunk that
  survives a shift still counts. `content_matched_shifted_chunks` is 0 at every
  size, consistent with the corpus having no mid-file insertions/deletions.

Content reuse by bytes, per kind (same five sizes):

| Kind | 8 KiB | 4 KiB | 2 KiB | 1 KiB | 512 B |
|---|---:|---:|---:|---:|---:|
| `agent_intermediate` (7×32 B edits per revision) | 11.4% | 17.9% | 36.8% | 62.8% | **90.7%** |
| `checkpoint` (~1 edit per 333 B) | 0.0% | 0.0% | 0.4% | 2.4% | **6.0%** |
| `tool_output` (one 15 B edit) | 0.0% | 0.0% | 0.0% | 0.0% | **16.6%** |
| `prompt` | 0.0% | 0.0% | 0.0% | 0.0% | 0.0% |

The two `prompt` pairs declare `revised_from` but the generator writes an
independent prompt rather than editing the target, so their 0% is a fixture
property, not evidence about CDC.

### 9.2 Measured explanation

The **edit-density hypothesis is the supported one**; the offset-shift
explanation is refuted.

1. **Boundaries reproduce.** 86.7% of interior chunk starts already coincide at
the 8 KiB baseline (agent pairs: 32/32, checkpoints: 40/46), and the fraction
stays high at every smaller size. A gear boundary is decided by roughly the last
64 bytes of hash state, so an edit only moves a boundary if it lands in that
window; at these edit densities most boundaries survive.
2. **Offsets do not shift.** Every edit is a same-length in-place overwrite and
each revision appends only at EOF, so no matched chunk sits at a different
absolute offset: 0 of 11 matched chunks at 8 KiB, 0 of 1,057 at 512 B. "The
revisions shift byte offsets" is simply not true of this corpus.
3. **Edit density decides reuse.** Whether a chunk is reusable is whether it
contains an edit. Checkpoints carry ~1 edit per 333 B, so even a 512 B chunk
averages ~1.5 edits; agent revisions carry 7 discrete 32 B edits per 49 KiB, so
a 512 B chunk averages ~0.07. The measured 512 B reuse follows that ratio
almost exactly: agent 93.1% of chunks / 90.7% of bytes reused, checkpoints
18.1% / 6.0%.

Smaller chunks therefore do what the reviewer predicted: unmatched
pair bytes fall from 1,154,963 to 793,324 and dedup savings rise from 9,100 to
240,141 compressed bytes. **But CDC's total physical bytes still rise
monotonically** (603,094 → 745,625, +29.49% over forced zstd) because (a) zstd
compresses 512–700 B chunks far worse per byte (marginal compressed bytes
594,500 → 854,792, +43.8% while the unique set only grows +5.0%) and (b) the
48-byte unique-chunk index plus 32-byte-per-reference manifests grow with chunk
count, 17,694 → 130,974 bytes.

**Corrected conclusion.** CDC loses on this corpus not because content-defined
boundaries fail to reproduce across edits — they mostly do — but because the
edits are dense per chunk (most notably the checkpoints' ~1 edit per 333 B), so
most chunk *contents* are genuinely unique; shrinking chunks raises content
reuse from 3.75% to 33.89% of pair bytes yet costs more in per-chunk compression
and index bytes than dedup saves, so the storage-optimal tested size remains the
largest, and even that loses to forced zstd by 4.74%.

This remains a real negative result for content-only dedup on this corpus. A
delta-aware layer still might change it; that is not measured here and is not
claimed. The sweep measures the harness's CDC construction, not the WeftCodec
product path.

## 10. Caveats and non-claims

- **Synthetic corpus.** No Meta, production, scraped, or downloaded data. The
  prompts, tool outputs, tensors, token sequences, and checkpoints are generated
  patterns. They exercise code paths, not real model behavior.
- **Single host, small corpus.** 60 files, 1.52 MB visible. Ratios at this scale
  are dominated by fixed frame/index overhead; they do not generalize to
  terabyte stores.
- **No external baseline set.** The comparison is WeftCodec modes and a
  content-defined-dedup construction implemented in the harness. SQLite, a
  graph store, ext4/XFS, and an external zstd-dictionary trainer are not in this
  receipt. (The P7 `bench/RESULTS-v1.md` has separate small-object baselines;
  they are not AI-corpus numbers.)
- **zstd encoder identity.** Rust and Go zstd encoders are not byte-identical by
  spec §12; this harness uses the Rust crate for all frame bytes and measures
  decode equivalence, not cross-encoder byte identity.
- **Latency is not a tail-latency claim.** Seven iterations per object, warm
  page cache, one container, no confidence interval. The p95 values are sample
  maxima over 60 medians, not a production SLO.
- **Typed modes are not a compression claim.** They lost on every typed object
  in this corpus; the result is reported, not hidden.
- **No superiority claim.** `weft_context` is smaller on this corpus because
  the corpus declares useful near-duplicate relations. A corpus without those
  relations would not show the same storage result, and the cold-read latency
  cost remains.

## 11. What remains

- An external, licensed AI-artifact corpus with a data card.
- Trained zstd dictionaries and an honest context-conditioned dictionary
  baseline.
- Larger stores (10^4–10^6 objects) with index-cost scaling.
- A plotting script is optional; the CSV/JSON graph is the data of record.
- Online GC, dependency-aware compaction, and cache policy for chain tails are
  filesystem work, not codec work, and are not measured here.
