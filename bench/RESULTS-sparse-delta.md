# Mode 16 `SPARSE_DELTA` vs mode 11 `WEIGHT_DELTA` on real public weights

Receipt: `bench/receipts/sparse-delta-20261106T052333Z.json`
Court: `weft/bench-court:1` (`sha256:4041d66e…`), kernel `7.3.1-1-cachyos`,
`--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`,
`--no-new-privileges`, `--cap-drop=ALL`.
Revisions recorded in the receipt: `weftfs eac3687`, `weftcodec 45a319b`,
`weftcodec_go e47fa03`, `weftfs_go 47fd4d2`.
Harness: `WeftCodec/examples/real_sparse_delta_bench.rs`; court wrapper
`bench/scripts/sparse_delta_court.py`.

**Scope.** Single host, warm page cache, **codec-level per-tensor frames**
(not the store planner), one public checkpoint family plus one prompt
fine-tune. Consecutive training checkpoints and the fine-tune are reported
separately and are never aggregated. Nothing here is a fleet or a
compression-superiority claim.

Headline: on the 19-word pair (`step0 -> step1`) mode 16 stores
**29,887 B vs 7,434,959 B** for mode 11, and on the dense fine-tune it stores
1.94× **more** than mode 11. Both directions are published below.

## Method

For every tensor in a pair:

1. Read the base tensor's exact bytes; encode them as one mode-6 `TENSOR`
   frame. Both modes use this same base.
2. Encode mode 11 and mode 16 against that base. Encode wall time is one
   measurement per mode per tensor.
3. Decode both frames five times; the reported decode time is the median of
   the five. Every decode is compared against the original tensor bytes **and**
   against the other mode's decode.
4. `zstd -19` bytes and decode medians are **read from the target
   checkpoint's existing P2 tensors phase**
   (`bench/receipts/real-float-tensor-20261105T053958Z.json`); they were not
   re-measured here.
5. Whole-file `zstd --patch-from` sizes are copied from that same P2 receipt
   and labelled; they were not re-run.

Every corpus file was SHA-256-verified against
`bench/corpora/real-models/float-tensor-real.json` inside the court before
measurement.

Mode 16 has a documented run bound (`max_sparse_runs`, default 16,777,216). A
dense delta that exceeds it is refused with `LimitExceeded`; those tensors are
reported as **refused** and excluded from the mode-16 byte sums (there is no
silent truncation and no fabricated size).

## Consecutive training checkpoints (EleutherAI/pythia-70m, F32)

| Pair | changed words | mode 11 | mode 16 | mode 16 / mode 11 | mode 16 / zstd-19 | tensors 16 / 11 / refused |
|---|---:|---:|---:|---:|---:|---:|
| step0 → step1 | 19 / 70,426,624 | 7,434,959 B | **29,887 B** | **0.0040** | 0.0002 | 76 / 0 / 0 |
| step1 → step2 | 3,618,263 (5.14 %) | 28,030,116 B | **18,135,056 B** | **0.6470** | 0.1257 | 27 / 49 / 0 |
| step2 → step4 | 14,830,076 (21.06 %) | 33,412,606 B | 74,180,869 B | 2.2201 (loss) | 0.5141 | 13 / 63 / 0 |
| step4 → step8 | 54,645,896 (77.59 %) | 49,198,709 B | 161,687,834 B | 3.2864 (loss) | 1.1192 | 13 / 62 / 1 |

* The **19-byte pair** is the case mode 16 exists for: 29,887 B instead of
  7.4 MB (a 249× reduction), and each of the 76 tensors is smaller. Versus the
  whole-file `zstd --patch-from -3` size recorded by P2 (30,265 B) mode 16 is
  **0.988×** — the same order of magnitude, per tensor, with no window and no
  patch-file machinery. Versus `--patch-from -19` (275,660 B) it is 0.108×.
* step1 → step2 is a mixed pair: mode 16 wins overall (0.647×) but only 27 of
  76 tensors are smaller; the losses are with the widest MLP matrices, where
  many words change and the Huffman planes win (worst five each +~100 KB on a
  ~440 KB mode-11 frame).
* From step2 → step4 onward the delta is dense enough that mode 16 loses:
  2.22× and 3.29× of mode 11. `gpt_neox.embed_in.weight` alone costs 55.6 MB
  more than mode 11 at step4 → step8.
* At step4 → step8 `embed_out.weight` (25,755,648 elements, 22,314,297 non-zero
  deltas) exceeds `max_sparse_runs` and is refused; the mode-16 sum therefore
  covers 75 of 76 tensors (178.7 MB of 281.7 MB decoded bytes).

### Encode/decode throughput (sums of per-tensor measurements, single thread)

| Pair | mode 11 encode | mode 16 encode | mode 11 decode | mode 16 decode | zstd-19 decode |
|---|---:|---:|---:|---:|---:|
| step0 → step1 | 541 MB/s | 646 MB/s | 409 MB/s | 715 MB/s | 1,269 MB/s |
| step1 → step2 | 325 MB/s | 595 MB/s | 303 MB/s | 653 MB/s | 1,277 MB/s |
| step2 → step4 | 287 MB/s | 490 MB/s | 290 MB/s | 641 MB/s | 1,289 MB/s |
| step4 → step8 | 302 MB/s | 316 MB/s | 290 MB/s | 517 MB/s | 1,275 MB/s |

Mode 16 encodes and decodes faster than mode 11 in every pair, including the
pairs where it is **larger**: it writes zero runs instead of Huffman-decoding
two to four byte planes for every element. That is a speed observation on this
host, not a general claim, and it does not rescue mode 16 on dense deltas —
there the larger frame also means more bytes read and stored. Both modes remain
1.8–9.6× slower than `zstd -19` to decode (mode 11 3.1–9.6×, mode 16 1.8–5.9×).

## Independent fine-tune (Qwen2.5-0.5B → Qwen2.5-0.5B-Instruct, BF16)

| Pair | changed words | mode 11 | mode 16 | mode 16 / mode 11 | mode 16 / zstd-19 | tensors 16 / 11 / refused |
|---|---:|---:|---:|---:|---:|---:|
| base → instruct | 483,818,305 (97.93 %) | 541,188,577 B | 1,049,967,405 B | 1.9401 (loss) | 1.3679 | 48 / 241 / 1 |

* A different instruction-tuning run changes almost every word, and mode 16
  loses the same way it loses on the dense consecutive pairs: 1.94× mode 11
  and 1.37× per-tensor zstd-19. Only 48 of 290 tensors are smaller.
* The largest tensor, `model.embed_tokens.weight` (136,134,656 elements,
  133,866,988 non-zero deltas), is refused by `max_sparse_runs`; the mode-16
  sum covers 289 of 290 tensors (715.8 MB of 988.1 MB decoded bytes).
* Worst tensors are the mlp matrices: +8.2 MB per tensor on ~4.5 MB mode-11
  frames.
* Encode 239 MB/s (mode 11) vs 256 MB/s (mode 16); decode 195 MB/s vs 319 MB/s;
  `zstd -19` decodes at 1,875 MB/s. The whole-file `--patch-from -3` size from
  P2 (769,386,807 B) is 1.365× the mode-16 per-tensor total and 0.703× the
  mode-11 total; `--patch-from -19` OOM-killed at 3 GiB in P2 and is not
  comparable.

## Where mode 16 loses (complete list of pair-level losses)

| Pair | loss factor vs mode 11 | tensors where mode 16 is larger | worst single tensor |
|---|---:|---:|---|
| step2 → step4 | 2.2201× | 63 of 76 | `embed_out.weight` +14,293,276 B |
| step4 → step8 | 3.2864× | 62 of 76 | `gpt_neox.embed_in.weight` +55,647,787 B |
| qwen base → instruct | 1.9401× | 241 of 290 | `model.layers.0.mlp.down_proj.weight` +8,196,857 B |

Also losses, by construction:

* mode 16 is **never** chosen for a delta with more than 16,777,216 non-zero
  words; those two tensors are refused, so a store planner falls back to mode
  11 (or an independent representation).
* mode 16 does not code the delta stream with an entropy coder, so on
  medium-density deltas it loses to mode 11 even when it is under the run
  bound. The crossover on this corpus is between step1 → step2 (0.647×, wins)
  and step2 → step4 (2.22×, loses).

## What this changes in the planner

`src/tensor_store.rs` offers both candidates whenever a causal parent with the
same dtype/shape exists and lets the existing byte price decide, so a
19-byte pair now stores 29,887 B instead of 7.4 MB while a dense fine-tune
keeps mode 11. `WeftFS-Go` has the same candidate and read path.

## Residual limits

* one host, one court image, warm page cache; no cold-cache, power-loss, or
  fleet claim;
* codec-level frames, not the store planner's own bytes (planner behaviour is
  covered by `tests/planner_sparse.rs`, not by this receipt);
* zstd baselines are copied from the P2 receipt (per-tensor zstd-19 and
  whole-file patch-from) and were not re-run;
* decode totals are sums of per-tensor medians, not one-pass wall time;
* only `pythia-70m` and `Qwen2.5-0.5B`; no other model families;
* `max_sparse_runs` refused two tensors; the bound is a policy choice, not a
  format limit.
