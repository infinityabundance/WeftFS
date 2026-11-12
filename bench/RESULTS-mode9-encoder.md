# R19 — mode-9 encoder: the same bytes, 2.9x faster

**Status:** executed. Rust codec and store on `staging`; Go untouched (no wire
change, no encoder change needed). Every number below is from the bench court
(3 GiB memory == swap, 1024 pids, 4 CPUs, bounded `/tmp`) on the frozen
Qwen2.5-0.5B-Instruct container
(`fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe`).

**Headline.** C (shipped two-stage planner) goes from **4,101.6 ms to
2,328.4 ms** (−43.2 %) with the footprint **byte-for-byte unchanged at
282,283,011 B**, all 101 tensors restored exactly, and the same chosen
representations. E goes from 4,156.4 to 2,363.2 ms (−43.1 %) at
282,354,152 B. The mode-9 encoder itself goes from 262.3 MB/s to
748.4 MB/s sequential (2.85x) and 1,524.8 MB/s at four emission threads
(5.81x), measured on the same nine tensors in the same court in the same
session.

| Receipt | What it is |
|---|---|
| `mode9-encoder-20261112T160000Z.json` | per-tensor before/after, 7 reps, 9 tensors (342 MB), byte-identity checks, page-fault counts, allocation probe |
| `r19-wall-before-20261112T150000Z.json` | 7 interleaved repetitions per config at WeftFS `24f94e4` / WeftCodec `0aa3881` |
| `r19-wall-after-20261112T140000Z.json` | the same command at WeftFS `c3f5e1a` / WeftCodec `1412227` |
| `augmentation-local-20261112T140000Z.json` / `…T150000Z.json` | the R3 contract court, after / before |

---

## 1. What was actually slow

R17 attributed 446.90 ms of the 1,163.80 ms embedding encode to "frame
assembly / other". That label was wrong, and the corrected attribution is the
reason this subphase has any effect at all. The fix required a **faithful
line-by-line replica** of the pre-R19 encoder rather than isolated stage
probes: a stage measured on its own re-uses already-mapped heap pages and
already-faulted memory, so it looks cheaper than the same stage inside the
encoder.

The faithful replica reproduces the whole wall (embedding: replica sum
1,185.63 ms, replica wall 1,187.99 ms, gap 0.0 ms), and inside it:

| Stage | ms | Note |
|---|---:|---|
| generic plane split (`chunks_exact` + per-element inner loop) | **705.8** | derived: `split_payload` 1,064.6 − huffman 358.8 |
| exponent histogram | 57.71 | |
| Huffman bit emission (`append_bits`, a `Vec::push` per byte) | **301.12** | |
| Huffman lengths + canonical codes | 0.02 | |
| concatenate both planes into `stored` | 13.96 | |
| `payload_hash` over `stored` | 17.59 | |
| copy `stored` into a `body` | 13.82 | |
| `encode_frame`: copy the body again, CRC the frame, then `Frame::parse` re-CRCs it | 46.62 | |
| `content_hash` over the payload | 26.96 | |

So the 446.90 ms was **not** frame assembly (that is 74 ms of copies plus two
redundant CRC passes). It was:

* the **generic splitter running at 386 MB/s** where a BF16 specialisation
  runs at ~816 MB/s, i.e. ~343 ms;
* the **byte-at-a-time emitter** (301 ms) plus its 57.7 ms histogram;
* ~1 GB of fresh anonymous mappings per tensor: **9,814 minor faults** around
  one embedding encode, versus 644 after the rewrite. The allocation probe in
  the receipt shows a fresh 272 MB mapping costs 14.0 ms to touch page by page
  against 22.3 ms for a recycled memcpy of the same size — the fault cost is
  real but it is the *number* of mappings (five per encode: both planes,
  `stored`, `body`, the frame) that mattered, not one page-fault constant.

## 2. The implementation

All of it is inside WeftCodec; nothing about the frame, the descriptor, the
canonical table or the padding changed.

1. **BF16 plane split** (`modes::split_bf16_planes`). `exponent = (w >> 7) as u8`,
   `sign|mantissa = ((w >> 8) as u8 & 0x80) | (w as u8 & 0x7f)`, over
   four-element/8-byte windows, writing into **pre-sized** planes and
   returning the exponent histogram from the same pass (four counter banks, so
   the skewed exponent distribution does not serialise on one counter). The
   generic word loop is kept verbatim as
   `modes::split_float_tensor_planes_reference` and still serves FP16, FP32,
   FP8.
2. **Fused histogram.** `split_float_tensor_payload` no longer builds a plane
   and then re-counts it: one pass produces both planes *and* the counts.
   `float_tensor_exponent_histogram` (the R18 analyse stage) got the same
   four-bank kernel, so the analyser's exact histogram is now ~2x cheaper too.
   Equivalence is total, not sampled: `tests/mode9_encoder_identity.rs`
   compares it against a plain count over all 65,536 BF16 words and over
   windowed accumulation.
3. **Huffman emission.** `huffman::EncoderTable` packs each symbol's canonical
   `code | length << 32` into one `u64`; `huffman::BitStream` accumulates in a
   64-bit register and flushes **four bytes at a time** into a pre-sized slice,
   so the hot loop is one table load, one shift-or and one predictable branch
   per symbol. The old byte-at-a-time `append_bits` emitter is preserved
   verbatim in the test module as the differential oracle.
4. **One allocation, no copies.** `encode_float_tensor` now plans the exact
   frame length first (the descriptor length is digest-independent, so a
   placeholder gives the exact offsets), allocates the frame once, and writes
   the descriptor and both planes **straight into it**
   (`frame::encode_frame_with_body`), hashes the stored region in place, and
   appends the trailer CRC. The body copy, the `stored` vector and the
   second CRC pass in `Frame::parse` are gone; `parse_without_trailer_check`
   still validates the header CRC, the tables, the length agreement and every
   mode/flag rule.
5. **Histogram hand-off.** `two_stage::Mode9Prediction` carries the analyser's
   exact histogram (`Arc<[u64; 256]>`), and the store calls
   `encode_float_tensor_with_histogram`, so the write path never counts a
   tensor twice.
6. **Bounded parallel emission** (`FloatTensorEncodeOptions`). See §4.

### AVX2: considered, measured, not shipped

The BF16 split is a read-2-bytes/write-2-bytes shuffle with a 256-entry
scatter-add histogram fused into it. The histogram cannot be vectorised
(scatter), and the scalar kernel already runs at ~816 MB/s for the split;
`sweftcodec::simd::active_path()` reports `avx2` for the *existing* decode and
plane-join kernels, which are where wide words pay. No AVX2 split kernel was
added, because there is no measured headroom to justify a second code path
that would then need its own differential court. This is a decision, not an
omission: the split is no longer the bottleneck (§5).

## 3. Byte identity

The invariant is that **not one emitted byte may change**. Evidence:

* `WeftCodec/tests/mode9_encoder_identity.rs`, 6 tests, all passing:
  * every committed `float_tensor` vector under `conformance/v1/codec/` is
    decoded and re-encoded; the frame must equal the committed `frame_hex`
    exactly, through the shipped encoder, through the pre-R19 reference
    encoder, through the histogram hand-off path, and at 2/3/4/8 emission
    threads;
  * the specialised splitter equals the generic oracle over **every 16-bit
    word** (65,536 patterns: all NaN payloads, both infinities, subnormals,
    ±0, max-finite) and over randomised planes at 1..1,000,000 bytes;
  * the whole-frame differential over 5 dtypes × 30 sizes (1..4,097 elements,
    chosen around the 32-bit flush word and the 8-byte split window) ×
    threads 1/2/3/4/8, including a **forced-parallel** configuration with
    `min_partition_elements = 1` so the boundary stitching is exercised at
    every alignment;
  * a 300,000-element skewed plane at every thread count 1..8;
  * `huffman::huffman` unit tests: fast emitter vs the byte-at-a-time
    reference over 20 shapes, and a two-partition stitch test;
  * `huffman::histogram` vs a plain count, and `EncoderTable` bit lengths vs
    the emitted length.
* The court compares the shipped frame with the reference frame on every
  tensor and every repetition (9 tensors × 7 reps), **0 mismatches**.
* The committed conformance vectors are unchanged and still decode, and the
  store's per-tensor `prediction_violations` count is 0 in the wall receipt
  (`ablation_encode_counts_exact`, `two_stage_matches_exhaustive`,
  `two_stage_storage_unchanged` are all true).

## 4. Parallel emission and its memory bound

The exponent bitstream is a pure concatenation of per-symbol canonical codes,
so it can be split at any element boundary:

1. partition the element range into `k` contiguous ranges
   (`k = min(threads, count / 65,536)`, capped at 64);
2. compute each partition's **exact** bit length by summing
   `EncoderTable::bits_of_byte` over its range, then prefix-sum them;
3. emit partition `j` into `stream[first_j .. first_j + complete_j]` starting at
   bit offset `B_j % 8`, with the one byte it shares with partition `j+1`
   spilled into a **one-byte scratch slot**;
4. a serial pass of `k` ORs merges each saved byte into the byte the next
   worker already wrote.

Each shared byte is written by one worker with only its own bits set, so the
OR is exact. The byte ranges are disjoint, so there is no aliasing and no
extra buffer: the only memory beyond the frame is one `u64` per partition for
the bit lengths and one `u8` per partition for the scratch. For the 272 MB
embedding that is 64 bytes of extra heap.

Bounds: `parallel_min_bitstream_bytes = 1 MiB` (below it, thread start-up
dominates), `parallel_max_bitstream_bytes = 512 MiB` (above it the encode
stays sequential, so the encoder never introduces a large transient
allocation), `min_partition_elements = 65,536`. Threads default to
`min(available_parallelism, 4)` in the store and `1` in the codec API.

Measured effect (7 reps, same court):

| Tensor | sequential | 4 threads | speed-up |
|---|---:|---:|---:|
| embedding, 272,269,312 B | 363.87 ms | **177.24 ms** | 2.05x |
| 8,716,288 B MLP (8 of them) | 11.26–11.87 ms | **5.80–6.02 ms** | ~1.95x |

Peak RSS for the whole C write falls from 1,188,052 KiB to 687,864 KiB (−42 %)
because the encoder no longer holds five copies of the planes; the parallel
path adds no measurable RSS. The 1 MiB threshold is what brings the eight
8.7 MB MLP tensors (≈1.4 MiB of bitstream each) into the parallel path; at the
4 MiB threshold I first tried, they stayed sequential and the total was
262.8 ms instead of 224.3 ms.

## 5. Per-stage before/after

Embedding, 272,269,312 B, median of 7 (receipt `mode9-encoder-20261112T160000Z`):

| Stage | before (ms) | after (ms) |
|---|---:|---:|
| generic plane split | 705.8 | — (specialised, fused) |
| BF16 split, same maths, alone | 333.54 | fused into the second pass |
| exponent histogram | 57.71 | fused into the first pass |
| Huffman lengths + codes | 0.02 | < 0.01 |
| Huffman emission | 301.12 | see "Huffman total" |
| Huffman total (shipped call) | 358.8 | **247.1** |
| concatenate planes | 13.96 | 0 (written in place) |
| copy into `body` | 13.82 | 0 |
| `encode_frame` copy + CRC + re-parse CRC | 46.62 | 0 (one assembly, one CRC) |
| `payload_hash` | 17.59 | inside the second half |
| `content_hash` | 26.96 | fused with the histogram |
| fused histogram + content hash | — | **79.2** |
| **encode total** | **1,044.2** (reference, this column)<br>1,188.0 (faithful replica wall) | **363.87** |
| encode total, 4 threads | — | **177.24** |
| MB/s | 260.7 | 748.3 (1,536.2 at 4 threads) |
| minor page faults | 9,814 | 644 |

The second half of the new encoder (split + emit + mantissa write + descriptor
+ payload hash + trailer CRC + the single allocation) is **at most 284.7 ms**
(363.87 − 79.2). The standalone `split_float_tensor_payload` measures 304.15 ms
for the same tensor and is therefore an *upper* bound on it: that call still
materialises both planes into fresh vectors.

Eight 8,716,288 B MLP tensors (median of the eight):

| Stage | before | after | 4 threads |
|---|---:|---:|---:|
| encode total | 32.11–32.95 ms | 11.26–11.87 ms | 5.80–6.02 ms |
| `split_payload` (whole) | 32.64 ms | 9.54 ms (upper bound) | — |
| fused histogram + hash | — | 2.54 ms | — |

## 6. The write wall

`bench/scripts/r19_wall_court.sh`, 7 interleaved repetitions, order rotated by
one position per repetition, least-contended observation. The same command was
run at the pre-R19 revision (`WeftFS 24f94e4` + `WeftCodec 0aa3881`) and at the
post-R19 revision (`WeftFS c3f5e1a` + `WeftCodec 1412227`) in this session.
B and B-group are omitted: their 405 per-blob `fsync` publications dominate
their own wall and they encode no mode 9.

| Config | before (ms) | after (ms) | Δ | footprint (B, both sides) |
|---|---:|---:|---:|---:|
| A native | 329.4 | 329.8 | +0.4 (noise) | 424,439,296 |
| **C two-stage** | **4,101.6** | **2,328.4** | **−1,773.2 (−43.2 %)** | **282,283,011** |
| C-legacy exhaustive | 24,079.9 | 22,791.2 | −1,288.7 (−5.4 %) | 282,283,011 |
| C-noz19 | 3,896.4 | 2,376.8 | −1,519.6 (−39.0 %) | 282,283,011 |
| C-nom9 (no mode 9) | 22,469.3 | 22,505.5 | +36.2 (noise) | 330,336,572 |
| C-both | 2,135.9 | 2,134.3 | −1.6 (noise) | 330,532,422 |
| D fast raw | 2,875.7 | 2,890.0 | +14.3 (noise) | 424,856,058 |
| **E combined** | **4,156.4** | **2,363.2** | **−1,793.2 (−43.1 %)** | **282,354,152** |

CPU (median): C 2,690 → 1,240 ms (−54 %), E 2,700 → 1,240 ms.
Peak RSS (median): C 1,188,052 → 687,864 KiB, E 1,188,452 → 687,880 KiB.
Cold restore (median): C 1,432.0 → 1,432.7 ms, E 1,440.1 → 1,434.0 ms —
unchanged, as expected: this subphase does not touch the decoder.
C / A = 12.44x → 7.06x.

The rows that contain **no mode-9 encode** (A, C-nom9, C-both, D) are
unchanged within noise, which is the control that says the delta is the
encoder and not a planner or publication change. `two_stage_matches_exhaustive`
and `two_stage_storage_unchanged` are true in the after receipt: identical
strategies, identical chosen sizes, identical head.

R18's own published C figure was 4,117.8 ms at the same codec revision; the
before run here reproduces it (4,101.6), so the pair is comparable across
sessions and the after figure is not an artefact of a lucky host.

**R22 note (added later; the numbers above are R19's and stand).** The mode-9
encoder measured here is unchanged by R22 — its per-tensor cost, byte identity
and frame bytes are the same. Two things around it moved: the pipeline wall
(R21, then R22's defaults) and the analyse stage. The 2,328.4 ms C row above is
the *sequential* planned writer at R19; the shipped write is now 592.6 ms
(least-contended) at the same 282,106,049 B, and the mode-9 candidate is 229.4 ms
of that write's summed CPU. See `bench/RESULTS-pareto.md`.

## 6a. The measured ledger at the R19 revision

The wall court records the shipped phase profile for one attribution repetition
(`configs.C.attribution_rep.ledger` in both receipts), so the delta is not a
projection:

| Phase | before (ms) | after (ms) | share of the after wall |
|---|---:|---:|---:|
| mode-9 `FLOAT_TENSOR` encode (35 tensors) | 1,791.4 | **194.4** | 8.4 % |
| analyse (exact histogram + ZSTD sampling) | 346.7 | **181.7** | 7.8 % |
| ZSTD level 3 (51 tensors) | 278.8 | 283.9 | 12.2 % |
| other encode / planning / raw | 41.9 | 40.5 | 1.7 % |
| **prepare** | **3,323.4** | **1,559.4** | 67.0 % |
| ├ encode total | 2,458.8 | 700.5 | 30.1 % |
| ├ blob publish (202 publications) | 774.2 | 769.2 | 33.0 % |
| ├ digest | 66.2 | 66.4 | 2.9 % |
| └ prepare unattributed | 24.2 | 23.3 | 1.0 % |
| **commit** | **708.1** | **700.4** | 30.1 % |
| ├ index segment (advisory, per commit) | 347.6 | 337.8 | 14.5 % |
| └ manifest commit (records + superblock) | 354.6 | 356.9 | 15.3 % |
| init + source read + footprint walk | 54.8 | 53.3 | 2.3 % |
| unattributed residual | 15.4 | 15.4 | 0.7 % |
| **wall** | **4,101.6** | **2,328.4** | 100 % |

The store-level mode-9 encode falls **9.2x** (1,791.4 → 194.4 ms), more than
the codec-level 2.85x/5.81x, for two reasons that are both attributable: the
analyser's histogram is now handed to the encoder instead of being recomputed
(one fewer pass over every gated tensor), and the eight-tensor MLP tail and the
embedding now run their emission on four threads.

## 6b. Next bottleneck

Publication is now 63.1 % of the wall: **blob publish 769.2 ms + commit 700.4 ms
= 1,469.6 ms**, of which the advisory full-state index segment is 337.8 ms and
the 101 commit-record/superblock publications are 356.9 ms. Encoding is 30.1 %.
That is exactly R20's target ("publication tax"): the shipped `Store::group`
path already saves ~689 ms of control-plane work on the B ablation (R17 §8), and
the pack plane trades requests and metadata for bytes on the 330 MB C object
set. R19 does not touch either.

## 7. Losses, limits and non-claims

* **One host, warm page cache.** The absolute walls move with host write
  latency. During the first attempt at the "official" run, `fsync` latency on
  this host was ~4x worse (A 6,275.9 ms, C 17,950.7 ms in
  `augmentation-local-20261112T131500Z.json`, retained); that receipt is not
  used for any claim. The before/after pair was measured back to back in a
  quiet window, and every row above is the least-contended of 7.
* **Level-19 ZSTD is still the tail.** `C-noz19` (2,376.8 after) is still
  slower than `C-both` (2,134.3): the analyse stage that R18 added costs more
  than the encodes it avoids on this fixture. That is R18's trade, unchanged.
* **The mode-9 encoder is no longer the bottleneck.** Of the remaining
  2,328.4 ms: blob publication and commit/index dominate (see the ledger in
  `bench/RESULTS-planner-two-stage.md`), and `C-legacy` (22,791.2) is 9.8x
  the shipped planner because it still encodes level 19 and mode 9 for every
  tensor that might win.
* **The histogram is still computed twice per gated tensor.** The store hands
  its analyser histogram to the encoder, but the analyser itself runs for
  every float tensor (101 here) while only 35 pass the 1/64 gate. Making the
  analyse stage cheap was part of this subphase (2x on that kernel), not
  eliminating it.
* **`split_float_tensor_payload` still materialises planes.** It is the public
  API used by tests, the entropy courts and the tiled writers; the encoder's
  zero-copy path is internal to `encode_float_tensor*`. So the second pass of
  the *API* is 304 ms while the encoder's own second half is ≤285 ms.
* **FP16/FP32/FP8 keep the generic splitter.** They get the new emitter, the
  fused histogram and the single assembly, but not a specialised split. No
  FP32/FP16 corpus was measured here; the fixture is BF16.
* **The parallel path is intra-tensor only.** There is no inter-tensor
  parallelism (that is R21), no shared thread pool, and each encode spawns its
  own scoped threads. At 4 CPUs the curve flattens at 4; 8 threads measured
  worse than 4 on this host in earlier probes.
* **Go unchanged.** `WeftCodec-Go` has its own encoder and its own
  byte-identical output; the frame format did not move, so nothing there
  needed to change. No Go performance claim is made.
* **Not a format change.** Nothing in `spec/` moved; the conformance manifest,
  coverage and cross gate are unchanged.
