# Real weights: FLOAT_TENSOR (mode 9) and WEIGHT_DELTA (mode 11)

**Status: implemented, measured once, single host.** This court answers two
questions on real public weights, and reports them separately:

- **A.** Does `FLOAT_TENSOR` (mode 9) compress real bf16 weights competitively
  with `zstd` and ZipNN-class coding?
- **B.** Does the WeftFS weight-delta representation help more on consecutive
  training checkpoints than on an independent fine-tune?

Receipt: [`bench/receipts/real-float-tensor-20261105T053958Z.json`](receipts/real-float-tensor-20261105T053958Z.json)
(1.28 MB; includes every per-tensor row and a byte-plane probe).

Label: **single host, public weights, warm cache, no fleet claim.** Every
method is lossless and every decode was byte-compared with the original tensor
bytes. No weights are committed; they live under `.docker-cache/real-corpus/`
(gitignored).

## Corpus

| id | repository | revision | file | bytes | safetensors dtype | SHA-256 (verified) |
|---|---|---|---|---|---:|---|
| qwen-base | `Qwen/Qwen2.5-0.5B` | `060db6499f32faf8b98477b0a26969ef7d8b9987` | `model.safetensors` | 988,097,824 | BF16 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |
| qwen-instruct | `Qwen/Qwen2.5-0.5B-Instruct` | `7ae557604adf67be50417f59c2c2f167def9a775` | `model.safetensors` | 988,097,824 | BF16 | `fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe` |
| pythia-step0 | `EleutherAI/pythia-70m` | `step0` (`87c8ac0b…`) | `model.safetensors` | 281,715,176 | F32 | `17a077abfddb6c589091e292adfcc2ee82d39bfd839f5dd80aaaea42861aaf8e` |
| pythia-step1 | `EleutherAI/pythia-70m` | `step1` (`85473bbd…`) | `model.safetensors` | 281,715,176 | F32 | `652e38fd15e118cc7c43d0aeb5e30675c7180cf1d8782cb401ba5d0f933fa109` |
| pythia-step2 | `EleutherAI/pythia-70m` | `step2` (`cf8b7696…`) | `model.safetensors` | 281,715,176 | F32 | `491b4bafde38f7b58871b997597d5ca8447e683718879f846501b20a719127b2` |
| pythia-step4 | `EleutherAI/pythia-70m` | `step4` (`667d937e…`) | `model.safetensors` | 281,715,176 | F32 | `cf8a0b16691ab56ed52e5c6ace2c14f99637e7de755eba9ee3b113417d996223` |
| pythia-step8 | `EleutherAI/pythia-70m` | `step8` (`e17bfc00…`) | `model.safetensors` | 281,715,176 | F32 | `17254dbf6e86d12b32a5da7b7fa5d51df3e5d3a12907b3b1ea1442ef51870e48` |

Files were fetched by `bench/scripts/real_model_fetch.py --revision <branch>`,
which reads the LFS pointer for that exact revision from the Hub tree API and
verifies size and SHA-256 after download; the court re-hashes every file before
measurement (`bench/corpora/real-models/float-tensor-real.json`).

**dtype honesty.** The Qwen files are BF16 (290 tensors, 988,065,536 tensor
bytes each). The pythia `model.safetensors` files at these step revisions are
**F32** (76 tensors, 281,706,496 tensor bytes each). They are not bf16 and are
not reported as bf16. The F32 containers hold fp16-precision values widened to F32.
The receipt's byte-plane probe on the largest tensor (`embed_out.weight`,
103,022,592 bytes) shows byte plane 0 with entropy 0.0 (one symbol), plane 1
with 8 symbols and 3.0 bits, plane 2 with 7.97 bits, and plane 3 with 2.72 bits
— i.e. the low 13 mantissa bits are zero. The comparison below is therefore
"F32 containers of fp16-precision values"; no claim is made for
full-precision random F32 weights.

**Consecutive-checkpoint caveat.** The published `step0` and `step1` files
differ in **exactly 19 bytes** (direct file comparison; the mode-11 changed-word
count is also 19). That pair is reported separately and is not used as the
headline consecutive result. `step1`→`step2`, `step2`→`step4`, and
`step4`→`step8` change 4.42 MB, 18.87 MB, and 69.00 MB of file bytes.

## Method

- **Codec harness:** `WeftCodec/examples/real_float_tensor_bench.rs`. It parses
  the safetensors header directly (8-byte LE length + JSON; no transformers)
  and calls the shipped codec entry points: `modes::encode_float_tensor` /
  `decode_float_tensor` for mode 9, and `weight_delta::encode_weight_delta` /
  `decode_with_base_frame` for mode 11. The mode-11 base is the corresponding
  base tensor's mode-9 frame; it is recomputed in process and never written.
- **zstd baselines:** the real `zstd 1.5.4` CLI, one tensor at a time, with
  `-19 -T1` and the default `-3 -T1` on the exact tensor bytes. Whole-file
  `zstd -19 -T1` of each safetensors file is reported separately (it includes
  the header and can exploit cross-tensor redundancy).
- **ZipNN:** 0.5.4, official byte path, `method="HUFFMAN"`,
  `input_format="byte"`, streaming 4 MiB chunks, 4 threads. It is run
  whole-file and is **not delta-aware**; it is not used as a delta baseline.
- **Restore:** single-threaded, in-process decode; median of 5 reps per tensor.
  The checkpoint totals in the restore table are the **sum of per-tensor
  medians** and exclude storage I/O; zstd frames are the CLI's `-19` output
  decoded with libzstd. Delta bytes read include the base mode-9 frame.
- **Environment:** `weft/bench-court:1`, image id
  `sha256:e80c438516a509c251d6f3ca4848363a86d12a92287a1d9284117acd9cc0d85a`,
  kernel `7.3.1-1-cachyos`, 3 GiB memory / 3 GiB swap, 4 vCPU, 1024 pids,
  1 GiB `/tmp` tmpfs, `cap-drop=ALL`, `no-new-privileges`. zstd 1.5.4,
  ZipNN 0.5.4, Python 3.11.2. All commands ran through
  `scripts/docker-court.sh bench`.

## A. FLOAT_TENSOR (mode 9) vs zstd, per tensor

| case | dtype | tensor bytes | **FLOAT_TENSOR** | zstd `-19` (per tensor) | **mode 9 / zstd-19** | zstd `-3` (per tensor) | zstd `-19` (whole file) | ZipNN 0.5.4 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| qwen-base | bf16 | 988,065,536 | **657,975,743** | 768,347,895 | **0.8564** | 769,017,099 | 764,815,908 | 658,281,885 |
| qwen-instruct | bf16 | 988,065,536 | **657,891,990** | 767,597,259 | **0.8571** | 769,519,717 | 764,093,099 | 658,236,344 |
| pythia-step0 | fp32 | 281,706,496 | 233,958,945 | **144,397,747** | **1.6202** | 166,338,280 | 142,240,232 | **119,680,613** |
| pythia-step1 | fp32 | 281,706,496 | 233,958,945 | **144,403,851** | **1.6202** | 166,330,470 | 142,240,032 | **119,680,613** |
| pythia-step2 | fp32 | 281,706,496 | 233,963,826 | **144,272,053** | **1.6217** | 166,357,108 | 142,257,923 | **119,682,001** |
| pythia-step4 | fp32 | 281,706,496 | 233,965,750 | **144,303,717** | **1.6213** | 166,369,163 | 142,271,117 | **119,685,713** |
| pythia-step8 | fp32 | 281,706,496 | 233,963,977 | **144,464,688** | **1.6195** | 166,383,214 | 142,277,741 | **119,681,706** |

Bold marks the smallest of the per-tensor mode-9 and zstd-19 columns.

**BF16 result (survived).** On both Qwen checkpoints `FLOAT_TENSOR` is
**14.4 % smaller than per-tensor zstd -19** (base: 657,975,743 vs 768,347,895)
and **14.0 % smaller than whole-file zstd -19**. It is essentially tied with
ZipNN 0.5.4: 306,142 bytes smaller on the base (0.047 %) and 344,354 bytes
smaller on the instruct checkpoint (0.052 %). This is a tie, not a meaningful
win over ZipNN.

**F32 result (did not survive).** On the pythia checkpoints `FLOAT_TENSOR` is
**62.0 % larger than per-tensor zstd -19** (233,958,945 vs 144,397,747) and
**95.5 % larger than ZipNN 0.5.4** (233,958,945 vs 119,680,613). The cause is
structural: for F32, mode 9 Huffman-codes the 8-bit exponent plane and stores
the 24-bit sign+mantissa plane raw, so its floor is ~3.3 bytes/element (0.8305
of raw, measured). The fp16-widened pythia mantissas have three low-entropy
bytes that a byte-level LZ removes and this fixed bit split cannot.

### Per-tensor breakdown

| case | tensors | mode 9 ≤ zstd-19 | median ratio (≥1 MiB) | median ratio (all) | largest absolute loss |
|---|---:|---:|---:|---:|---|
| qwen-base | 290 | 169 (121/121 tensors ≥1 MiB) | 0.8524 | 0.8579 | +663 B (`model.layers.0.post_attention_layernorm.weight`) |
| qwen-instruct | 290 | 169 (121/121 ≥1 MiB) | 0.8519 | 0.8567 | +666 B (`model.layers.0.post_attention_layernorm.weight`) |
| pythia-step0 | 76 | 0 | 1.5604 | 86.12 | +33,371,992 B (`embed_out.weight`) |
| pythia-step1 | 76 | 0 | 1.5604 | 86.12 | +33,365,260 B (`embed_out.weight`) |
| pythia-step2 | 76 | 0 | 1.5594 | 9.5236 | +33,590,760 B (`embed_out.weight`) |
| pythia-step4 | 76 | 0 | 1.5584 | 5.5917 | +33,602,926 B (`embed_out.weight`) |
| pythia-step8 | 76 | 0 | 1.5593 | 3.6211 | +33,374,630 B (`embed_out.weight`) |

The "all" median for pythia is dominated by ~1–2 KB tensors whose zstd frame is
tens of bytes; the byte-weighted total (1.62) is the meaningful figure. The
five largest mode-9 losses on pythia-step0 are:

| tensor | bytes | mode 9 | zstd -19 | added |
|---|---:|---:|---:|---:|
| `embed_out.weight` | 103,022,592 | 85,547,238 | 52,175,246 | +33,371,992 |
| `gpt_neox.embed_in.weight` | 103,022,592 | 85,548,671 | 52,188,045 | +33,360,626 |
| `gpt_neox.layers.0.mlp.dense_h_to_4h.weight` | 4,194,304 | 3,483,827 | 2,190,516 | +1,293,311 |
| `gpt_neox.layers.4.mlp.dense_h_to_4h.weight` | 4,194,304 | 3,483,640 | 2,196,805 | +1,286,835 |
| `gpt_neox.layers.4.mlp.dense_4h_to_h.weight` | 4,194,304 | 3,483,611 | 2,201,234 | +1,282,377 |

On Qwen the five largest savings and the five largest losses are:

| tensor (qwen-base) | bytes | mode 9 | zstd -19 | delta |
|---|---:|---:|---:|---:|
| `model.embed_tokens.weight` | 272,269,312 | 180,623,816 | 208,141,749 | **−27,517,933** |
| `model.layers.2.mlp.gate_proj.weight` | 8,716,288 | 5,785,588 | 6,824,317 | **−1,038,729** |
| `model.layers.1.mlp.gate_proj.weight` | 8,716,288 | 5,791,432 | 6,823,808 | **−1,032,376** |
| `model.layers.3.mlp.gate_proj.weight` | 8,716,288 | 5,791,738 | 6,822,925 | **−1,031,187** |
| `model.layers.5.mlp.gate_proj.weight` | 8,716,288 | 5,807,297 | 6,832,446 | **−1,025,149** |
| `model.layers.0.post_attention_layernorm.weight` | 1,792 | 1,630 | 967 | +663 |
| `model.layers.23.post_attention_layernorm.weight` | 1,792 | 1,631 | 987 | +644 |
| `model.norm.weight` | 1,792 | 1,633 | 1,004 | +629 |
| `model.layers.22.post_attention_layernorm.weight` | 1,792 | 1,662 | 1,079 | +583 |
| `model.layers.21.post_attention_layernorm.weight` | 1,792 | 1,677 | 1,099 | +578 |

The BF16 win is carried by the 272 MB embedding (27.5 MB saved) and the MLP
matrices; the 1,792-byte RMSNorm vectors are the only losers, by under 700
bytes each.

## B. WEIGHT_DELTA, consecutive training checkpoints (reported pair by pair)

The independent baseline is the newer checkpoint's own per-tensor `zstd -19`
total (`zstd19/pt`); the whole-file `zstd -19` and the best runnable
`zstd --patch-from` are shown beside it. Delta bytes never include the base,
which is retained anyway.

| pair | changed words / elements | changed file bytes | **delta (mode 11)** | zstd-19/pt | **delta / zstd-19** | whole-file zstd-19 | best patch-from | **delta / patch** | delta / target raw |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| step0→step1 | 19 / 70,426,624 (0.00003 %) | 19 | 7,434,959 | 144,403,851 | 0.0515 | 142,240,032 | **30,265** (`-3`) | **245.66× worse** | 0.0264 |
| step1→step2 | 3,618,263 / 70,426,624 (5.14 %) | 4,420,148 | **28,030,116** | 144,272,053 | 0.1943 | 142,257,923 | 67,686,695 (`-19`) | **0.4141** | 0.0995 |
| step2→step4 | 14,830,076 / 70,426,624 (21.06 %) | 18,869,341 | **33,412,606** | 144,303,717 | 0.2315 | 142,271,117 | 111,529,080 (`-19`) | **0.2996** | 0.1186 |
| step4→step8 | 54,645,896 / 70,426,624 (77.59 %) | 69,001,326 | **49,198,709** | 144,464,688 | 0.3406 | 142,277,741 | 141,228,981 (`-19`) | **0.3484** | 0.1746 |

- **Survived:** on the three pairs where real change occurred, mode 11 is
  **19.4–34.1 % of independent per-tensor zstd -19** and **2.4–3.3× smaller
  than the best `zstd --patch-from`**. In summary, delta is 19.4-34.1 % of
  independent per-tensor zstd -19 on the meaningful consecutive pairs. The
  advantage is largest when the step distance is smallest.
- **Lost, explicitly:** on step0→step1 only 19 bytes differ. `zstd --patch-from`
  represents that in **30,265 bytes**, while mode 11 needs **7,434,959 bytes**
  — 245.66× larger. Mode 11 must Huffman-code a full 70,426,624-element plane
  for any plane that is not constant, at a floor of one bit per element; a
  sparse change cannot amortize that floor. This is a real, reproducible loss,
  not a measurement artifact.
- `zstd --patch-from -19` fit the 3 GiB court for all four pythia pairs
  (peak RSS 1.03–1.10 GiB). For the Qwen pair it was killed at 3,128,344 KiB
  after 509.6 s; `-12` succeeded (peak 3,113,340 KiB) and is the reported
  patch baseline for that pair.

## C. WEIGHT_DELTA, independent fine-tune (reported separately)

| pair | changed words / elements | **delta (mode 11)** | zstd-19/pt | **delta / zstd-19** | whole-file zstd-19 | best patch-from | **delta / patch** | delta / target raw |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| qwen base→instruct | 483,818,305 / 494,032,768 (97.93 %) | **541,188,577** | 767,597,259 | **0.7050** | 764,093,099 | 749,534,915 (`-12`) | **0.7220** | 0.5477 |

The fine-tune is a different scale of change: nearly every BF16 word moves.
Mode 11 still wins bytes (29.5 % fewer than independent zstd -19 and 27.8 %
fewer than the best patch), but the ratio is **70.5 % of zstd -19**, versus
**19.4–34.1 %** for the meaningful consecutive pairs. Delta therefore helps the
fine-tune, but **much less** than consecutive training checkpoints.

### Separation (not aggregated)

The two case sets are never aggregated.

| case set | delta / independent zstd -19 | delta / best patch-from | delta / target raw |
|---|---:|---:|---:|
| consecutive, meaningful (3 pairs) | 0.194–0.341 | 0.300–0.414 | 0.100–0.175 |
| independent fine-tune (1 pair) | 0.705 | 0.722 | 0.548 |

## Restore cost

Single-threaded in-process decode, median of 5 reps per tensor; totals are the
sum of per-tensor medians and exclude storage I/O. "Bytes read" is the frame
size sum; delta includes the base mode-9 frame.

| case | FLOAT_TENSOR ms | FLOAT_TENSOR bytes | zstd -19 ms | zstd -19 bytes | mode 9 / zstd decode |
|---|---:|---:|---:|---:|---:|
| qwen-base | 5,963.4 | 657,975,743 | 496.9 | 768,347,895 | **12.0× slower** |
| qwen-instruct | 5,938.9 | 657,891,990 | 527.0 | 767,597,259 | **11.3× slower** |
| pythia-step0 | 1,485.4 | 233,958,945 | 222.5 | 144,397,747 | **6.7× slower** |
| pythia-step1 | 1,487.0 | 233,958,945 | 222.0 | 144,403,851 | **6.7× slower** |
| pythia-step2 | 1,486.3 | 233,963,826 | 220.6 | 144,272,053 | **6.7× slower** |
| pythia-step4 | 1,484.3 | 233,965,750 | 218.6 | 144,303,717 | **6.8× slower** |
| pythia-step8 | 1,483.8 | 233,963,977 | 220.9 | 144,464,688 | **6.7× slower** |

| pair | delta ms | delta bytes read | zstd -19 ms | zstd -19 bytes | delta / zstd decode |
|---|---:|---:|---:|---:|---:|
| step0→step1 | 3,575.8 | 241,393,904 | 222.0 | 144,403,851 | **16.1× slower** |
| step1→step2 | 3,822.7 | 261,989,061 | 220.6 | 144,272,053 | **17.3× slower** |
| step2→step4 | 4,028.6 | 267,376,432 | 218.6 | 144,303,717 | **18.4× slower** |
| step4→step8 | 4,561.8 | 283,164,459 | 220.9 | 144,464,688 | **20.6× slower** |
| qwen base→instruct | 23,169.7 | 1,199,164,320 | 527.0 | 767,597,259 | **44.0× slower** |

Delta buys bytes at a large decode-time cost: the Huffman plane decode is
serial, and a delta restore also has to read the base frame. This is the same
trade the v0.5.0 store-level experiment observed.

## Exactness

Every mode-9 frame, every mode-11 frame (`decode_with_base_frame`), and every
zstd `-19` frame decoded back to bytes equal to the original tensor bytes; the
harness asserts this per tensor and exits non-zero on any mismatch. ZipNN 0.5.4
reported `restored_matches_source: true` for all seven files in-process.

## Every loss, in one place

1. **FLOAT_TENSOR loses on F32**: 62.0 % larger than per-tensor zstd -19 and
   95.5 % larger than ZipNN 0.5.4 on every pythia checkpoint, and 0/76 tensors
   win. The result is not transferred to BF16 or to full-precision F32.
2. **FLOAT_TENSOR only ties ZipNN on BF16**: 306,142 / 344,354 bytes smaller
   (0.05 %) is inside the noise of a single run; no superiority over ZipNN is
   claimed.
3. **FLOAT_TENSOR decode is 6.7–12× slower** than zstd `-19`, for 14 % fewer
   bytes on BF16 and 62 % more bytes on F32.
4. **Mode 11 loses 245.66× to `zstd --patch-from` on step0→step1** (7,434,959
   vs 30,265 bytes) because of the one-bit-per-element non-constant-plane
   floor.
5. **Mode 11 is only 70.5 % of zstd -19 on the fine-tune**, not the 19–34 % it
   reaches on meaningful consecutive checkpoints.
6. **Delta restore is 16–21× slower** than zstd on pythia and **44× slower** on
   the Qwen fine-tune, and reads more bytes (delta + base).
7. **`zstd --patch-from -19` is not runnable for the 988 MB Qwen pair** under
   the 3 GiB court limit; the `-12` result is reported and labelled.
8. **ZipNN is whole-file and not delta-aware**, so there is no ZipNN-class
   *delta* baseline; the fine-tune section compares delta against zstd only.
9. **Timings are warm-cache, single-host, sum-of-medians**, not one wall-clock
   pass, and no cache-dropping capability exists in the court.

## What survived, what did not

| question | outcome |
|---|---|
| FLOAT_TENSOR competes with zstd on real **bf16** weights | **yes**: 14.4 % smaller than per-tensor zstd -19 |
| FLOAT_TENSOR competes with **ZipNN-class** coding | **tie**: within 0.05 % on bf16; loses 95.5 % on fp32 |
| FLOAT_TENSOR is competitive on real **F32** weights | **no**: 62.0 % larger than zstd -19 |
| Delta helps **more** on consecutive checkpoints than a fine-tune | **yes**: 19.4–34.1 % vs 70.5 % of zstd -19 |
| Delta beats a general-purpose patch coder on consecutive checkpoints | **yes** for 3/3 meaningful pairs (2.4–3.3×), **no** for the 19-byte step0→step1 pair (245.66× worse) |
| Delta saves decode time | **no**: 16–44× slower than zstd |

## Reproduce

```sh
scripts/docker-court.sh bench 'python3 bench/scripts/real_model_fetch.py \
  --repo EleutherAI/pythia-70m --revision step0 --file model.safetensors \
  --dest .docker-cache/real-corpus/pythia-70m-step0.safetensors'
scripts/docker-court.sh rust \
  'cd /work/WeftCodec && cargo build --release --example real_float_tensor_bench'
scripts/docker-court.sh bench \
  'python3 bench/scripts/real_float_tensor_court.py --phases verify,tensors,deltas'
scripts/docker-court.sh bench \
  'python3 bench/scripts/real_float_tensor_court.py --phases zstd'
scripts/docker-court.sh bench \
  'python3 bench/scripts/real_float_tensor_court.py --phases patchfrom'
scripts/docker-court.sh bench \
  'python3 bench/scripts/real_float_tensor_court.py --phases zipnn'
scripts/docker-court.sh bench \
  'python3 bench/scripts/real_float_tensor_court.py --phases receipt'
```

Residual risks: all numbers are one host and one warm-cache run; restore totals
are sums of per-tensor medians and exclude I/O; the pythia F32 containers hold
fp16-precision values, so the fp32 result is not a full-precision F32 result;
ZipNN is whole-file and not delta-aware.
