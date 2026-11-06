# Entropy coders for AI byte planes — experimental court (R5)

**Status: experimental, no wire change.** `huffman-canonical-v1` remains
the only coder used by modes 9–15. This court adds
`WeftCodec/src/experimental/entropy/`, which no persistent encode or
decode path calls, and defines no mode. Nothing was promoted.

Receipt: `bench/receipts/entropy-coder-20261106T065154Z.json`  
Analysis generated from that receipt; every number below is from the run.

| revision | value |
|---|---|
| weftfs | `4e5cb95a79ceb1e52393e001051c4d2fa7b81ad8` |
| weftcodec | `72cd11944ab3517d5f639191eb0ad8da9b89e784` |
| weftcodec_go | `e47fa03ee8d576106e3bace065d59f930cc741bd` |
| weftfs_go | `47fd4d2229fc24e5750808a302432c32081064a5` |

Court: `weft/bench-court:1` (`sha256:4041d66ed25626e8…`), kernel `7.3.1-1-cachyos`, limits memory `3g`, cpus `4`, pids `1024`, nproc `2048`, `--cap-drop=ALL`, `no-new-privileges`, 1 GiB `/tmp`.

The harness sha256 is `149379c993fa1e230d166552b1ffb6657dbde77b198418df1229f228ab2ef246`; the 252-row NDJSON sha256 is `517a8dea19d3aa88468de477a9a030074320ae480f0c552f9b159e2c33d9e3df`.

## Corpus

All seven files in `bench/corpora/real-models/float-tensor-real.json`
were re-hashed **inside the court** before any measurement (manifest
sha256 `89fce69126dedec742c273a663884701b6e8b95083b3fe5de54b0163b39c9345`). The court measures four of them; the rest are verified but
not used.

| id | file | bytes | dtype | tensors | sha256 |
|---|---|---:|---|---:|---|
| qwen-base | `Qwen2.5-0.5B.safetensors` | 988097824 | bf16 | 290 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |
| qwen-instruct | `Qwen2.5-0.5B-Instruct.safetensors` | 988097824 | bf16 | 290 | `fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe` |
| pythia-step0 | `pythia-70m-step0.safetensors` | 281715176 | fp32 | 76 | `17a077abfddb6c589091e292adfcc2ee82d39bfd839f5dd80aaaea42861aaf8e` |
| pythia-step1 | `pythia-70m-step1.safetensors` | 281715176 | fp32 | 76 | `652e38fd15e118cc7c43d0aeb5e30675c7180cf1d8782cb401ba5d0f933fa109` |
| pythia-step2 | `pythia-70m-step2.safetensors` | 281715176 | fp32 | 76 | `491b4bafde38f7b58871b997597d5ca8447e683718879f846501b20a719127b2` |
| pythia-step4 | `pythia-70m-step4.safetensors` | 281715176 | fp32 | 76 | `cf8a0b16691ab56ed52e5c6ace2c14f99637e7de755eba9ee3b113417d996223` |
| pythia-step8 | `pythia-70m-step8.safetensors` | 281715176 | fp32 | 76 | `17254dbf6e86d12b32a5da7b7fa5d51df3e5d3a12907b3b1ea1442ef51870e48` |

No weights are committed. The corpus lives under
`.docker-cache/real-corpus/`.

## Input classes

Every plane is extracted **through the shipped mode encoders**, never by
a second implementation: the mode-9 exponent plane is decoded from the
shipped Huffman stream, and every other plane is read out of the shipped
payload layout. The coders under test never see a `WFT1` frame.

| class | tensor | dtype | workload shapes |
|---|---|---|---|
| `mode9_bf16` | `model.layers.0.mlp.down_proj.weight` | BF16 | `whole`, `rows_100`, `row_1` |
| `mode9_fp32` | `gpt_neox.layers.0.mlp.dense_h_to_4h.weight` | F32 | `whole`, `rows_100`, `row_1` |
| `mode11_dense` | `model.layers.0.mlp.down_proj.weight` | BF16 | `whole`, `rows_100`, `row_1` |
| `mode11_sparse` | `embed_out.weight` | F32 | `whole`, `rows_100`, `row_1` |
| `mode15_tiles` | `model.layers.0.mlp.down_proj.weight` | BF16 | `whole`, `rows_100`, `row_1` |
| `kv_page` | `model.layers.0.mlp.down_proj.weight` | bf16 | `kv_page`, `rows_100`, `row_1` |

Details:

* `mode9_bf16` — mode-9 exponent and sign+mantissa planes of
  `model.layers.0.mlp.down_proj.weight` (BF16, 4,358,144 elements,
  8,716,288 tensor bytes; 4,358,144 bytes per plane because BF16's
  sign+mantissa is exactly eight bits).
* `mode9_fp32` — the same two planes for `gpt_neox.layers.0.mlp.dense_h_to_4h.weight` (F32, 1,048,576 elements, 4,194,304 tensor bytes;
  the sign+mantissa plane is three bytes per element).
* `mode11_dense` — mode-11 residual byte planes for the independent
  fine-tune pair Qwen2.5-0.5B → Instruct on the same BF16 tensor (two
  planes, 8,716,288 bytes).
* `mode11_sparse` — mode-11 residual byte planes for pythia-70m
  step0 → step1 on `embed_out.weight` (F32, [50304, 512], 25,755,648
  elements; four planes, 103,022,592 bytes). The first changed row is
  row 2304; the 100-row and 1-row regions are anchored there so they are
  not all-zero.
* `mode15_tiles` — mode-15 tile planes for the BF16 tensor at tile
  `[64, 128]` (532 tiles; per-plane concatenation in tile order). Region
  tile shapes are derived so they divide the region: `[50, 128]` for
  100 rows and `[1, 128]` for one row.
* `kv_page` — mode-12 page planes with geometry from
  `WeftCodec/examples/bench_kv_activation.rs` (512 rows × 16 heads ×
  64 dims, BF16, 1,048,576 bytes). The page bytes are the first MiB of
  the real Qwen tensor. `rows_100` and `row_1` are prefixes of that page.

## Method and accounting

* Each coder encodes each plane independently. `encoded_bytes` sums the
  streams **and charges the serialised Tunstall dictionary in full**.
* Dictionary scopes:
  * **per-plane** — one dictionary per coded plane.
  * **per-tensor** — one dictionary shared by all planes of the tensor;
  * **family-shared** — one dictionary built from the aggregate
    histogram of a declared family: all 290 Qwen tensors (mode 9), all
    76 pythia tensors (mode 9 fp32 and mode 11 sparse), all 290 Qwen
    pairs (mode 11 dense), the 169 tile-compatible Qwen tensors (121
    excluded: 1-D or not divisible by `[64,128]`), and 8 KV pages.
    `encoded_bytes_amortized` divides that dictionary by the number of
    family members; the full dictionary is reported separately so its
    cost never disappears.
* Encode MiB/s is the median of the encode repetitions. Decode MiB/s is
  the p50 of the decode repetitions. p99 is the observed percentile of
  that sample set. Repetitions are 31 (≤64 KiB), 21 (≤1 MiB), 9 (≤8 MiB)
  for encode and at least 11 for decode; 3 for larger whole-plane inputs.
* The process peak RSS is the kernel high-water mark reset immediately
  before each row. All six input classes stay resident, so small-region
  rows report a floor set by resident inputs, not by the row itself.
* The pooled summary is a byte-weighted sum of per-row medians, not one
  pooled timing run, and it is dominated by the 103 MB sparse whole-plane
  input. Use the per-class tables, not the pooled row, for decisions.

## Headline results

| coder configuration | encoded / decoded | enc MiB/s | dec MiB/s | worst p99 ms |
|---|---:|---:|---:|---:|
| `huffman-canonical-v1` | 0.1773 | 1161.2 | 1228.8 | 39.641 |
| `rans` | 0.1519 | 312.2 | 305.4 | 298.327 |
| `raw` | 1.0000 | 47621.6 | 46953.4 | 2.624 |
| `tunstall-10/family-shared` | 0.4685 | 74.2 | 819.4 | 68.986 |
| `tunstall-10/per-plane` | 0.4653 | 74.1 | 820.6 | 69.178 |
| `tunstall-10/per-tensor` | 0.4685 | 74.2 | 820.4 | 68.825 |
| `tunstall-12/family-shared` | 0.3182 | 60.2 | 1419.6 | 19.752 |
| `tunstall-12/per-plane` | 0.3020 | 62.0 | 1263.9 | 24.521 |
| `tunstall-12/per-tensor` | 0.3156 | 64.5 | 1341.1 | 24.724 |
| `tunstall-8/family-shared` | 1.0001 | 115.2 | 370.1 | 267.260 |
| `tunstall-8/per-plane` | 1.0002 | 115.2 | 369.1 | 268.768 |
| `tunstall-8/per-tensor` | 1.0001 | 115.1 | 369.3 | 268.013 |
| `zstd-19` | 0.1515 | 28.5 | 9651.6 | 4.159 |
| `zstd-3` | 0.1615 | 1806.5 | 6807.5 | 5.616 |

Peak memory: the family/build phase reached **2248448 KiB (2.14 GiB)**; the
per-row matrix peak was **520708 KiB**. The build peak is the dense
full-checkpoint family histogram on the 272 MB embedding through the
shipped delta encoder. It is inside the 3 GiB court but leaves only about
0.86 GiB of headroom; a production family dictionary should not hold the
largest tensors and their delta frames at once.

### Where each coder wins on bytes

* **rANS is the smallest** on mode-9 BF16 whole planes (5753076 B, 0.6600),
  mode-11 dense whole planes (4357033 B, 0.4999), mode-15 tiles whole planes
  (5804101 B), KV whole pages (698896 B), and the sparse whole planes (1188 B).
  It is 0.64% under the shipped Huffman and 0.86% under zstd-19 on
  mode-9 BF16, and 3.58% / 2.40% under them on dense residuals.
  Over the BF16/dense/tile/KV whole-plane classes the win is 0.5–3.6% over Huffman and 0.7–2.4% over zstd-19.
* **zstd-19 is smallest on FP32 mode-9** (2014728 B, 0.4803): it is 12.1%
  under rANS and 13.0% under the shipped Huffman.
* **zstd-19 is smallest on 1-row regions** in every class except the
  sparse residual, where rANS's tiny single-symbol state wins. On the KV
  row it is 1446 B vs 1878 B for Huffman and 2168 B for rANS.
* **Tunstall wins nothing.** At k=8 a split cannot fit (`256 + 255 >
  256`), so its ratio is 1.0001–1.0002: raw plus dictionary. At k=10 and
  k=12 it compresses some planes (0.31–1.03× raw on the non-sparse whole
  planes, 0.095× on the sparse whole planes) but is 47–59% above the best
  of zstd-19/rANS/Huffman on the non-sparse whole planes, and about
  3,000× above zstd-19 on the sparse whole planes where zstd exploits the
  zero runs. On 1-row regions its total is 6.6–30.5× the best non-sparse
  coder and its dictionary alone is 3.5–83.8× the payload. It never beats
  zstd-19, rANS, or the shipped Huffman.

### Speed

* zstd-19 decodes at 9652 MiB/s pooled but encodes at 28.5 MiB/s: it is a
  size reference, not a write path.
* The shipped Huffman encodes at 1161 MiB/s and decodes at 1229 MiB/s,
  0.5–3.6% larger than rANS on BF16/dense/tile/KV whole planes.
* rANS is 2.0× slower than Huffman on BF16 whole planes (254.5 vs 510.3
  MiB/s decode) and 23.7× slower than zstd-19 on the same planes. On the
  sparse whole planes zstd-19 decodes at 24528 MiB/s and rANS at 329.5 MiB/s,
  a 74.4× gap.
* Tunstall k=12 decodes fast (up to 5706 MiB/s) but encodes at 55–87 MiB/s
  and never beats zstd-19, rANS, or the shipped Huffman on bytes.

## Full matrix

Each row is one coder and one workload shape for one input class. `ratio`
is encoded bytes / decoded plane bytes. `peak KiB` is the per-row
high-water mark (see the resident-input caveat above).

### `mode9_bf16`

Tensor `model.layers.0.mlp.down_proj.weight`, BF16, shape [896, 4864]; shapes `whole`, `rows_100`, `row_1`.

| shape | coder | encoded B | ratio | enc MiB/s | dec MiB/s | p50 ms | p99 ms | peak KiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| whole | raw | 8716288 | 1.0000 | 64810.3 | 64972.4 | 0.1279 | 0.1305 | 198400 |
| whole | zstd-3 | 6155815 | 0.7062 | 567.2 | 2851.3 | 2.9154 | 3.0099 | 198912 |
| whole | zstd-19 | 5802784 | 0.6657 | 7.7 | 6031.2 | 1.3782 | 1.5740 | 281292 |
| whole | huffman-canonical-v1 | 5790267 | 0.6643 | 542.0 | 510.3 | 16.2892 | 16.6615 | 198912 |
| whole | rans | 5753076 | 0.6600 | 264.0 | 254.5 | 32.6681 | 32.7155 | 198912 |
| whole | tunstall-8 per-plane | 8717844 | 1.0002 | 114.2 | 368.4 | 22.5641 | 22.5802 | 234464 |
| whole | tunstall-8 per-tensor | 8717074 | 1.0001 | 114.1 | 368.2 | 22.5781 | 22.8801 | 234464 |
| whole | tunstall-8 family-shared | 8717074 | 1.0001 | 113.8 | 369.0 | 22.5247 | 22.7971 | 234464 |
| whole | tunstall-10 per-plane | 8504360 | 0.9757 | 87.4 | 335.1 | 24.8050 | 24.9199 | 232028 |
| whole | tunstall-10 per-tensor | 8552871 | 0.9813 | 88.0 | 338.4 | 24.5607 | 24.7497 | 232284 |
| whole | tunstall-10 family-shared | 8552871 | 0.9813 | 87.9 | 339.7 | 24.4736 | 24.6015 | 232284 |
| whole | tunstall-12 per-plane | 8667332 | 0.9944 | 77.0 | 410.3 | 20.2607 | 20.4947 | 230364 |
| whole | tunstall-12 per-tensor | 9056525 | 1.0390 | 80.5 | 460.2 | 18.0618 | 18.2208 | 231900 |
| whole | tunstall-12 family-shared | 9056525 | 1.0390 | 80.3 | 457.4 | 18.1714 | 18.4948 | 231900 |
| rows_100 | raw | 972800 | 1.0000 | 83496.9 | 82311.6 | 0.0113 | 0.0136 | 198912 |
| rows_100 | zstd-3 | 684756 | 0.7039 | 571.6 | 3098.9 | 0.2994 | 0.3150 | 198912 |
| rows_100 | zstd-19 | 647660 | 0.6658 | 10.9 | 7066.6 | 0.1313 | 0.1618 | 198912 |
| rows_100 | huffman-canonical-v1 | 646628 | 0.6647 | 498.0 | 516.9 | 1.7947 | 1.8864 | 198912 |
| rows_100 | rans | 642676 | 0.6606 | 265.0 | 255.6 | 3.6298 | 3.6737 | 198912 |
| rows_100 | tunstall-8 per-plane | 974356 | 1.0016 | 117.1 | 370.7 | 2.5027 | 2.5114 | 198912 |
| rows_100 | tunstall-8 per-tensor | 973586 | 1.0008 | 113.7 | 368.9 | 2.5152 | 2.5343 | 198912 |
| rows_100 | tunstall-8 family-shared | 973586 | 1.0008 | 119.6 | 370.1 | 2.5069 | 2.5465 | 198912 |
| rows_100 | tunstall-10 per-plane | 955847 | 0.9826 | 89.6 | 336.5 | 2.7572 | 2.8208 | 198912 |
| rows_100 | tunstall-10 per-tensor | 957907 | 0.9847 | 90.2 | 340.6 | 2.7241 | 2.8004 | 198912 |
| rows_100 | tunstall-10 family-shared | 957907 | 0.9847 | 90.1 | 341.2 | 2.7192 | 2.7698 | 198912 |
| rows_100 | tunstall-12 per-plane | 997718 | 1.0256 | 78.0 | 411.2 | 2.2561 | 2.3857 | 198912 |
| rows_100 | tunstall-12 per-tensor | 1026586 | 1.0553 | 81.5 | 460.7 | 2.0139 | 2.1261 | 198912 |
| rows_100 | tunstall-12 family-shared | 1026586 | 1.0553 | 81.5 | 460.1 | 2.0162 | 2.1021 | 198912 |
| row_1 | raw | 9728 | 1.0000 | 103081.6 | 77311.2 | 0.0001 | 0.0002 | 198912 |
| row_1 | zstd-3 | 6975 | 0.7170 | 490.0 | 1740.6 | 0.0053 | 0.0064 | 198912 |
| row_1 | zstd-19 | 6599 | 0.6784 | 23.3 | 4007.5 | 0.0023 | 0.0033 | 198912 |
| row_1 | huffman-canonical-v1 | 7016 | 0.7212 | 53.1 | 459.8 | 0.0202 | 0.0230 | 198912 |
| row_1 | rans | 7308 | 0.7512 | 273.5 | 282.1 | 0.0329 | 0.0382 | 198912 |
| row_1 | tunstall-8 per-plane | 11284 | 1.1600 | 116.7 | 370.1 | 0.0251 | 0.0263 | 198912 |
| row_1 | tunstall-8 per-tensor | 10514 | 1.0808 | 119.8 | 370.4 | 0.0250 | 0.0270 | 198912 |
| row_1 | tunstall-8 family-shared | 10514 | 1.0808 | 115.0 | 364.9 | 0.0254 | 0.0273 | 198912 |
| row_1 | tunstall-10 per-plane | 17280 | 1.7763 | 87.7 | 429.5 | 0.0216 | 0.0258 | 198912 |
| row_1 | tunstall-10 per-tensor | 13521 | 1.3899 | 88.4 | 431.1 | 0.0215 | 0.0248 | 198912 |
| row_1 | tunstall-10 family-shared | 13521 | 1.3899 | 88.0 | 436.0 | 0.0213 | 0.0218 | 198912 |
| row_1 | tunstall-12 per-plane | 44159 | 4.5394 | 78.6 | 432.5 | 0.0215 | 0.0234 | 198912 |
| row_1 | tunstall-12 per-tensor | 28348 | 2.9141 | 84.3 | 451.7 | 0.0205 | 0.0223 | 198912 |
| row_1 | tunstall-12 family-shared | 28348 | 2.9141 | 83.5 | 455.3 | 0.0204 | 0.0218 | 198912 |

### `mode9_fp32`

Tensor `gpt_neox.layers.0.mlp.dense_h_to_4h.weight`, F32, shape [2048, 512]; shapes `whole`, `rows_100`, `row_1`.

| shape | coder | encoded B | ratio | enc MiB/s | dec MiB/s | p50 ms | p99 ms | peak KiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| whole | raw | 4194304 | 1.0000 | 64292.1 | 66123.4 | 0.0605 | 0.0608 | 198912 |
| whole | zstd-3 | 2288395 | 0.5456 | 265.6 | 1565.7 | 2.5548 | 2.6315 | 198912 |
| whole | zstd-19 | 2014728 | 0.4803 | 6.7 | 1729.1 | 2.3134 | 2.4788 | 248780 |
| whole | huffman-canonical-v1 | 2316258 | 0.5522 | 437.2 | 454.8 | 8.7942 | 9.1935 | 198912 |
| whole | rans | 2290872 | 0.5462 | 247.2 | 253.3 | 15.7903 | 15.8755 | 198912 |
| whole | tunstall-8 per-plane | 4195860 | 1.0004 | 115.7 | 369.2 | 10.8339 | 10.9659 | 224316 |
| whole | tunstall-8 per-tensor | 4195090 | 1.0002 | 115.1 | 369.0 | 10.8389 | 10.8567 | 224316 |
| whole | tunstall-8 family-shared | 4195090 | 1.0002 | 115.7 | 370.1 | 10.8092 | 10.8784 | 224316 |
| whole | tunstall-10 per-plane | 3205105 | 0.7642 | 86.4 | 428.2 | 9.3415 | 9.4100 | 198912 |
| whole | tunstall-10 per-tensor | 3453006 | 0.8233 | 88.3 | 372.4 | 10.7424 | 10.8241 | 198912 |
| whole | tunstall-10 family-shared | 3453006 | 0.8233 | 88.2 | 367.9 | 10.8720 | 10.9916 | 198912 |
| whole | tunstall-12 per-plane | 3192574 | 0.7612 | 74.2 | 395.9 | 10.1046 | 10.1419 | 198912 |
| whole | tunstall-12 per-tensor | 3753475 | 0.8949 | 86.6 | 414.1 | 9.6598 | 9.7533 | 198912 |
| whole | tunstall-12 family-shared | 3753475 | 0.8949 | 86.7 | 419.7 | 9.5316 | 9.6388 | 198912 |
| rows_100 | raw | 204800 | 1.0000 | 110784.2 | 108930.6 | 0.0018 | 0.0018 | 198912 |
| rows_100 | zstd-3 | 112488 | 0.5493 | 272.0 | 1364.9 | 0.1431 | 0.1571 | 198912 |
| rows_100 | zstd-19 | 109380 | 0.5341 | 7.9 | 1308.8 | 0.1492 | 0.1686 | 198912 |
| rows_100 | huffman-canonical-v1 | 113582 | 0.5546 | 329.1 | 485.4 | 0.4023 | 0.4484 | 198912 |
| rows_100 | rans | 112640 | 0.5500 | 249.3 | 258.8 | 0.7547 | 0.7782 | 198912 |
| rows_100 | tunstall-8 per-plane | 206356 | 1.0076 | 119.8 | 365.4 | 0.5345 | 0.5377 | 198912 |
| rows_100 | tunstall-8 per-tensor | 205586 | 1.0038 | 120.4 | 371.8 | 0.5253 | 0.5276 | 198912 |
| rows_100 | tunstall-8 family-shared | 205586 | 1.0038 | 119.8 | 370.5 | 0.5271 | 0.5769 | 198912 |
| rows_100 | tunstall-10 per-plane | 164545 | 0.8034 | 86.4 | 440.3 | 0.4436 | 0.4798 | 198912 |
| rows_100 | tunstall-10 per-tensor | 172630 | 0.8429 | 88.2 | 384.0 | 0.5086 | 0.5389 | 198912 |
| rows_100 | tunstall-10 family-shared | 172630 | 0.8429 | 88.3 | 384.6 | 0.5078 | 0.5317 | 198912 |
| rows_100 | tunstall-12 per-plane | 191192 | 0.9336 | 74.1 | 408.1 | 0.4786 | 0.5126 | 198912 |
| rows_100 | tunstall-12 per-tensor | 199534 | 0.9743 | 86.8 | 426.3 | 0.4582 | 0.4850 | 198912 |
| rows_100 | tunstall-12 family-shared | 199534 | 0.9743 | 86.9 | 426.3 | 0.4582 | 0.4769 | 198912 |
| row_1 | raw | 2048 | 1.0000 | 39062.5 | 27901.8 | 0.0001 | 0.0001 | 198912 |
| row_1 | zstd-3 | 1315 | 0.6421 | 197.7 | 589.0 | 0.0033 | 0.0049 | 198912 |
| row_1 | zstd-19 | 1264 | 0.6172 | 29.4 | 711.5 | 0.0027 | 0.0032 | 198912 |
| row_1 | huffman-canonical-v1 | 1636 | 0.7988 | 15.2 | 329.3 | 0.0059 | 0.0081 | 198912 |
| row_1 | rans | 1834 | 0.8955 | 212.6 | 269.2 | 0.0073 | 0.0093 | 198912 |
| row_1 | tunstall-8 per-plane | 3604 | 1.7598 | 115.3 | 359.7 | 0.0054 | 0.0056 | 198912 |
| row_1 | tunstall-8 per-tensor | 2834 | 1.3838 | 113.0 | 365.1 | 0.0053 | 0.0055 | 198912 |
| row_1 | tunstall-8 family-shared | 2834 | 1.3838 | 117.4 | 363.0 | 0.0054 | 0.0055 | 198912 |
| row_1 | tunstall-10 per-plane | 9525 | 4.6509 | 83.2 | 531.3 | 0.0037 | 0.0052 | 198912 |
| row_1 | tunstall-10 per-tensor | 5812 | 2.8379 | 88.2 | 486.1 | 0.0040 | 0.0049 | 198912 |
| row_1 | tunstall-10 family-shared | 5812 | 2.8379 | 88.7 | 480.1 | 0.0041 | 0.0043 | 198912 |
| row_1 | tunstall-12 per-plane | 38561 | 18.8286 | 73.8 | 620.8 | 0.0031 | 0.0039 | 198912 |
| row_1 | tunstall-12 per-tensor | 18724 | 9.1426 | 85.7 | 507.7 | 0.0038 | 0.0046 | 198912 |
| row_1 | tunstall-12 family-shared | 18979 | 9.2671 | 86.3 | 503.8 | 0.0039 | 0.0041 | 198912 |

### `mode11_dense`

Tensor `model.layers.0.mlp.down_proj.weight`, BF16, shape [896, 4864]; shapes `whole`, `rows_100`, `row_1`.

| shape | coder | encoded B | ratio | enc MiB/s | dec MiB/s | p50 ms | p99 ms | peak KiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| whole | raw | 8716288 | 1.0000 | 66712.4 | 65289.3 | 0.1273 | 0.1644 | 198912 |
| whole | zstd-3 | 4695289 | 0.5387 | 890.5 | 2004.1 | 4.1478 | 4.2398 | 198912 |
| whole | zstd-19 | 4464320 | 0.5122 | 5.7 | 2293.2 | 3.6249 | 3.7108 | 281516 |
| whole | huffman-canonical-v1 | 4518647 | 0.5184 | 373.0 | 464.7 | 17.8893 | 18.7529 | 198912 |
| whole | rans | 4357033 | 0.4999 | 240.0 | 243.7 | 34.1097 | 35.1564 | 198912 |
| whole | tunstall-8 per-plane | 8717844 | 1.0002 | 114.7 | 369.7 | 22.4847 | 22.5812 | 234544 |
| whole | tunstall-8 per-tensor | 8717074 | 1.0001 | 114.4 | 369.8 | 22.4784 | 22.5987 | 234544 |
| whole | tunstall-8 family-shared | 8717074 | 1.0001 | 114.0 | 369.8 | 22.4755 | 22.7862 | 234544 |
| whole | tunstall-10 per-plane | 7369950 | 0.8455 | 85.5 | 423.0 | 19.6490 | 19.8511 | 230444 |
| whole | tunstall-10 per-tensor | 7529641 | 0.8639 | 86.8 | 443.6 | 18.7373 | 18.9482 | 230828 |
| whole | tunstall-10 family-shared | 7529641 | 0.8639 | 87.0 | 442.9 | 18.7685 | 18.8876 | 230828 |
| whole | tunstall-12 per-plane | 6395633 | 0.7338 | 69.4 | 341.4 | 24.3469 | 24.5211 | 224300 |
| whole | tunstall-12 per-tensor | 7303676 | 0.8379 | 72.5 | 338.7 | 24.5427 | 24.7235 | 228400 |
| whole | tunstall-12 family-shared | 7616994 | 0.8739 | 79.4 | 423.7 | 19.6182 | 19.7519 | 230448 |
| rows_100 | raw | 972800 | 1.0000 | 58982.4 | 81875.8 | 0.0113 | 0.0222 | 198912 |
| rows_100 | zstd-3 | 523984 | 0.5386 | 897.7 | 2118.6 | 0.4379 | 0.4662 | 198912 |
| rows_100 | zstd-19 | 504444 | 0.5185 | 6.3 | 2868.1 | 0.3235 | 0.3989 | 198912 |
| rows_100 | huffman-canonical-v1 | 504694 | 0.5188 | 351.3 | 471.8 | 1.9662 | 2.1015 | 198912 |
| rows_100 | rans | 486703 | 0.5003 | 239.0 | 245.2 | 3.7832 | 3.8135 | 198912 |
| rows_100 | tunstall-8 per-plane | 974356 | 1.0016 | 118.1 | 370.7 | 2.5026 | 2.5302 | 198912 |
| rows_100 | tunstall-8 per-tensor | 973586 | 1.0008 | 119.1 | 371.1 | 2.5002 | 2.5180 | 198912 |
| rows_100 | tunstall-8 family-shared | 973586 | 1.0008 | 118.7 | 371.1 | 2.5002 | 2.5077 | 198912 |
| rows_100 | tunstall-10 per-plane | 829520 | 0.8527 | 86.5 | 424.9 | 2.1832 | 2.2243 | 198912 |
| rows_100 | tunstall-10 per-tensor | 843807 | 0.8674 | 87.6 | 443.2 | 2.0931 | 2.1378 | 198912 |
| rows_100 | tunstall-10 family-shared | 843807 | 0.8674 | 87.6 | 442.9 | 2.0949 | 2.1418 | 198912 |
| rows_100 | tunstall-12 per-plane | 747544 | 0.7684 | 70.2 | 336.1 | 2.7606 | 2.9525 | 198912 |
| rows_100 | tunstall-12 per-tensor | 831691 | 0.8549 | 73.7 | 337.2 | 2.7512 | 2.8824 | 198912 |
| rows_100 | tunstall-12 family-shared | 868399 | 0.8927 | 79.6 | 424.9 | 2.1836 | 2.3011 | 198912 |
| row_1 | raw | 9728 | 1.0000 | 103081.6 | 84339.5 | 0.0001 | 0.0002 | 198912 |
| row_1 | zstd-3 | 5352 | 0.5502 | 535.9 | 1241.3 | 0.0075 | 0.0090 | 198912 |
| row_1 | zstd-19 | 5118 | 0.5261 | 7.1 | 1550.9 | 0.0060 | 0.0079 | 198912 |
| row_1 | huffman-canonical-v1 | 5449 | 0.5601 | 53.7 | 486.1 | 0.0191 | 0.0244 | 198912 |
| row_1 | rans | 5579 | 0.5735 | 264.6 | 297.5 | 0.0312 | 0.0408 | 198912 |
| row_1 | tunstall-8 per-plane | 11284 | 1.1600 | 112.9 | 367.5 | 0.0252 | 0.0264 | 198912 |
| row_1 | tunstall-8 per-tensor | 10514 | 1.0808 | 115.4 | 367.0 | 0.0253 | 0.0254 | 198912 |
| row_1 | tunstall-8 family-shared | 10514 | 1.0808 | 115.3 | 371.0 | 0.0250 | 0.0267 | 198912 |
| row_1 | tunstall-10 per-plane | 15943 | 1.6389 | 82.8 | 457.7 | 0.0203 | 0.0224 | 198912 |
| row_1 | tunstall-10 per-tensor | 12428 | 1.2775 | 85.0 | 455.0 | 0.0204 | 0.0213 | 198912 |
| row_1 | tunstall-10 family-shared | 12428 | 1.2775 | 85.0 | 453.3 | 0.0205 | 0.0217 | 198912 |
| row_1 | tunstall-12 per-plane | 46237 | 4.7530 | 63.8 | 684.9 | 0.0135 | 0.0212 | 198912 |
| row_1 | tunstall-12 per-tensor | 27072 | 2.7829 | 73.9 | 594.7 | 0.0156 | 0.0228 | 198912 |
| row_1 | tunstall-12 family-shared | 29170 | 2.9986 | 80.1 | 499.5 | 0.0186 | 0.0230 | 198912 |

### `mode11_sparse`

Tensor `embed_out.weight`, F32, shape [50304, 512]; shapes `whole`, `rows_100`, `row_1`.

| shape | coder | encoded B | ratio | enc MiB/s | dec MiB/s | p50 ms | p99 ms | peak KiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| whole | raw | 103022592 | 1.0000 | 43230.4 | 42585.9 | 2.3071 | 2.6237 | 318660 |
| whole | zstd-3 | 3289 | 0.0000 | 8987.2 | 19116.4 | 5.1396 | 5.6157 | 218152 |
| whole | zstd-19 | 3288 | 0.0000 | 1622.8 | 24527.9 | 4.0056 | 4.1585 | 327552 |
| whole | huffman-canonical-v1 | 3220480 | 0.0313 | 2450.9 | 2500.9 | 39.2858 | 39.6409 | 250704 |
| whole | rans | 1188 | 0.0000 | 337.9 | 329.5 | 298.1797 | 298.3266 | 218132 |
| whole | tunstall-8 per-plane | 103025704 | 1.0000 | 115.4 | 369.0 | 266.2456 | 268.7680 | 520708 |
| whole | tunstall-8 per-tensor | 103023394 | 1.0000 | 115.5 | 369.6 | 265.8188 | 268.0131 | 520708 |
| whole | tunstall-8 family-shared | 103023394 | 1.0000 | 115.6 | 370.5 | 265.2109 | 267.2602 | 520708 |
| whole | tunstall-10 per-plane | 32212989 | 0.3127 | 70.7 | 1432.9 | 68.5688 | 69.1777 | 277952 |
| whole | tunstall-10 per-tensor | 32199195 | 0.3125 | 70.6 | 1431.6 | 68.6314 | 68.8248 | 277952 |
| whole | tunstall-10 family-shared | 32199195 | 0.3125 | 70.6 | 1431.7 | 68.6263 | 68.9858 | 277952 |
| whole | tunstall-12 per-plane | 9829857 | 0.0954 | 58.6 | 5693.9 | 17.2553 | 17.4941 | 225912 |
| whole | tunstall-12 per-tensor | 9701277 | 0.0942 | 60.7 | 5705.6 | 17.2199 | 17.2568 | 225912 |
| whole | tunstall-12 family-shared | 9701277 | 0.0942 | 55.3 | 5675.8 | 17.3103 | 17.4726 | 225912 |
| rows_100 | raw | 204800 | 1.0000 | 107728.9 | 102634.0 | 0.0019 | 0.0060 | 225912 |
| rows_100 | zstd-3 | 85 | 0.0004 | 10137.7 | 25484.4 | 0.0077 | 0.0097 | 225912 |
| rows_100 | zstd-19 | 83 | 0.0004 | 1976.1 | 25250.5 | 0.0077 | 0.0085 | 225912 |
| rows_100 | huffman-canonical-v1 | 7424 | 0.0362 | 2427.1 | 2667.9 | 0.0732 | 0.0750 | 225912 |
| rows_100 | rans | 50 | 0.0002 | 337.6 | 330.9 | 0.5902 | 0.6039 | 225912 |
| rows_100 | tunstall-8 per-plane | 207912 | 1.0152 | 117.7 | 370.3 | 0.5274 | 0.5298 | 225912 |
| rows_100 | tunstall-8 per-tensor | 205602 | 1.0039 | 116.6 | 366.8 | 0.5325 | 0.5434 | 225912 |
| rows_100 | tunstall-8 family-shared | 205602 | 1.0039 | 116.4 | 367.9 | 0.5309 | 0.5329 | 225912 |
| rows_100 | tunstall-10 per-plane | 82425 | 0.4025 | 70.4 | 1405.0 | 0.1390 | 0.1407 | 225912 |
| rows_100 | tunstall-10 per-tensor | 68631 | 0.3351 | 70.9 | 1442.5 | 0.1354 | 0.1371 | 225912 |
| rows_100 | tunstall-10 family-shared | 68631 | 0.3351 | 70.8 | 1441.8 | 0.1355 | 0.1370 | 225912 |
| rows_100 | tunstall-12 per-plane | 190677 | 0.9310 | 58.5 | 5713.6 | 0.0342 | 0.0354 | 225912 |
| rows_100 | tunstall-12 per-tensor | 62097 | 0.3032 | 55.5 | 5723.6 | 0.0341 | 0.0343 | 225912 |
| rows_100 | tunstall-12 family-shared | 62097 | 0.3032 | 60.8 | 5693.6 | 0.0343 | 0.0352 | 225912 |
| row_1 | raw | 2048 | 1.0000 | 27901.8 | 19531.2 | 0.0001 | 0.0003 | 225912 |
| row_1 | zstd-3 | 81 | 0.0396 | 601.7 | 1892.6 | 0.0010 | 0.0011 | 225912 |
| row_1 | zstd-19 | 76 | 0.0371 | 233.2 | 1892.6 | 0.0010 | 0.0011 | 225912 |
| row_1 | huffman-canonical-v1 | 1088 | 0.5312 | 1119.9 | 1422.5 | 0.0014 | 0.0015 | 225912 |
| row_1 | rans | 48 | 0.0234 | 308.0 | 306.5 | 0.0064 | 0.0065 | 225912 |
| row_1 | tunstall-8 per-plane | 5160 | 2.5195 | 115.6 | 359.0 | 0.0054 | 0.0055 | 225912 |
| row_1 | tunstall-8 per-tensor | 2850 | 1.3916 | 115.8 | 358.4 | 0.0054 | 0.0071 | 225912 |
| row_1 | tunstall-8 family-shared | 2850 | 1.3916 | 116.1 | 354.5 | 0.0055 | 0.0055 | 225912 |
| row_1 | tunstall-10 per-plane | 19065 | 9.3091 | 69.4 | 1326.9 | 0.0015 | 0.0019 | 225912 |
| row_1 | tunstall-10 per-tensor | 5271 | 2.5737 | 69.3 | 1326.0 | 0.0015 | 0.0030 | 225912 |
| row_1 | tunstall-10 family-shared | 5271 | 2.5737 | 68.0 | 1326.0 | 0.0015 | 0.0015 | 225912 |
| row_1 | tunstall-12 per-plane | 171669 | 83.8228 | 59.2 | 4428.9 | 0.0004 | 0.0005 | 225912 |
| row_1 | tunstall-12 per-tensor | 43089 | 21.0396 | 59.8 | 4428.9 | 0.0004 | 0.0005 | 225912 |
| row_1 | tunstall-12 family-shared | 43089 | 21.0396 | 54.6 | 4428.9 | 0.0004 | 0.0005 | 225912 |

### `mode15_tiles`

Tensor `model.layers.0.mlp.down_proj.weight`, BF16, shape [896, 4864]; shapes `whole`, `rows_100`, `row_1`.

| shape | coder | encoded B | ratio | enc MiB/s | dec MiB/s | p50 ms | p99 ms | peak KiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| whole | raw | 8716288 | 1.0000 | 69969.4 | 65202.2 | 0.1275 | 0.1334 | 225912 |
| whole | zstd-3 | 6212225 | 0.7127 | 538.6 | 2803.2 | 2.9654 | 3.0507 | 225912 |
| whole | zstd-19 | 5848520 | 0.6710 | 8.6 | 6010.4 | 1.3830 | 1.5885 | 308396 |
| whole | huffman-canonical-v1 | 5834667 | 0.6694 | 522.3 | 508.4 | 16.3492 | 16.6409 | 225912 |
| whole | rans | 5804101 | 0.6659 | 263.5 | 252.9 | 32.8632 | 32.9909 | 225912 |
| whole | tunstall-8 per-plane | 8717844 | 1.0002 | 112.3 | 370.2 | 22.4547 | 22.7850 | 261504 |
| whole | tunstall-8 per-tensor | 8717074 | 1.0001 | 111.8 | 367.1 | 22.6445 | 22.7559 | 261504 |
| whole | tunstall-8 family-shared | 8717074 | 1.0001 | 111.9 | 367.5 | 22.6215 | 22.7918 | 261504 |
| whole | tunstall-10 per-plane | 8578358 | 0.9842 | 86.0 | 322.3 | 25.7920 | 25.9732 | 259456 |
| whole | tunstall-10 per-tensor | 8606350 | 0.9874 | 86.1 | 324.6 | 25.6086 | 25.7770 | 259456 |
| whole | tunstall-10 family-shared | 8606350 | 0.9874 | 85.5 | 323.3 | 25.7077 | 25.9157 | 259456 |
| whole | tunstall-12 per-plane | 8753472 | 1.0043 | 76.9 | 392.6 | 21.1714 | 21.3756 | 225912 |
| whole | tunstall-12 per-tensor | 9080329 | 1.0418 | 81.2 | 447.4 | 18.5807 | 18.8098 | 259456 |
| whole | tunstall-12 family-shared | 9080329 | 1.0418 | 81.7 | 447.9 | 18.5598 | 18.7996 | 259456 |
| rows_100 | raw | 972800 | 1.0000 | 80039.2 | 82826.0 | 0.0112 | 0.0152 | 225912 |
| rows_100 | zstd-3 | 690632 | 0.7099 | 539.9 | 3056.2 | 0.3036 | 0.3270 | 225912 |
| rows_100 | zstd-19 | 652863 | 0.6711 | 11.2 | 7063.3 | 0.1313 | 0.1667 | 225912 |
| rows_100 | huffman-canonical-v1 | 651552 | 0.6698 | 480.1 | 511.3 | 1.8144 | 1.8909 | 225912 |
| rows_100 | rans | 648385 | 0.6665 | 263.2 | 253.7 | 3.6572 | 3.6850 | 225912 |
| rows_100 | tunstall-8 per-plane | 974356 | 1.0016 | 119.7 | 370.1 | 2.5067 | 2.5714 | 225912 |
| rows_100 | tunstall-8 per-tensor | 973586 | 1.0008 | 119.6 | 367.9 | 2.5217 | 2.5713 | 225912 |
| rows_100 | tunstall-8 family-shared | 973586 | 1.0008 | 119.0 | 368.8 | 2.5153 | 2.5462 | 225912 |
| rows_100 | tunstall-10 per-plane | 964078 | 0.9910 | 87.0 | 319.9 | 2.9000 | 3.0076 | 225912 |
| rows_100 | tunstall-10 per-tensor | 963648 | 0.9906 | 87.8 | 322.9 | 2.8735 | 2.9291 | 225912 |
| rows_100 | tunstall-10 family-shared | 963648 | 0.9906 | 87.3 | 319.7 | 2.9016 | 3.0353 | 225912 |
| rows_100 | tunstall-12 per-plane | 1007352 | 1.0355 | 76.6 | 393.8 | 2.3561 | 2.6350 | 225912 |
| rows_100 | tunstall-12 per-tensor | 1029292 | 1.0581 | 83.1 | 447.8 | 2.0719 | 2.2782 | 225912 |
| rows_100 | tunstall-12 family-shared | 1029292 | 1.0581 | 83.1 | 447.2 | 2.0744 | 2.2005 | 225912 |
| row_1 | raw | 9728 | 1.0000 | 92773.4 | 84339.5 | 0.0001 | 0.0002 | 225912 |
| row_1 | zstd-3 | 7035 | 0.7232 | 482.3 | 1699.1 | 0.0055 | 0.0068 | 225912 |
| row_1 | zstd-19 | 6648 | 0.6834 | 24.3 | 3825.7 | 0.0024 | 0.0070 | 225912 |
| row_1 | huffman-canonical-v1 | 7071 | 0.7269 | 51.0 | 458.4 | 0.0202 | 0.0233 | 225912 |
| row_1 | rans | 7366 | 0.7572 | 272.3 | 283.8 | 0.0327 | 0.0394 | 225912 |
| row_1 | tunstall-8 per-plane | 11284 | 1.1600 | 119.2 | 369.7 | 0.0251 | 0.0265 | 225912 |
| row_1 | tunstall-8 per-tensor | 10514 | 1.0808 | 119.5 | 369.5 | 0.0251 | 0.0268 | 225912 |
| row_1 | tunstall-8 family-shared | 10514 | 1.0808 | 115.7 | 369.7 | 0.0251 | 0.1098 | 225912 |
| row_1 | tunstall-10 per-plane | 17336 | 1.7821 | 86.5 | 424.0 | 0.0219 | 0.0265 | 225912 |
| row_1 | tunstall-10 per-tensor | 13581 | 1.3961 | 87.3 | 430.9 | 0.0215 | 0.0232 | 225912 |
| row_1 | tunstall-10 family-shared | 13590 | 1.3970 | 86.9 | 428.3 | 0.0217 | 0.0254 | 225912 |
| row_1 | tunstall-12 per-plane | 43703 | 4.4925 | 83.1 | 431.9 | 0.0215 | 0.0235 | 225912 |
| row_1 | tunstall-12 per-tensor | 28033 | 2.8817 | 86.6 | 449.7 | 0.0206 | 0.0218 | 225912 |
| row_1 | tunstall-12 family-shared | 28045 | 2.8829 | 86.9 | 444.1 | 0.0209 | 0.0219 | 225912 |

### `kv_page`

Tensor `model.layers.0.mlp.down_proj.weight`, bf16, shape [512, 1024]; shapes `kv_page`, `rows_100`, `row_1`.

| shape | coder | encoded B | ratio | enc MiB/s | dec MiB/s | p50 ms | p99 ms | peak KiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| kv_page | raw | 1048576 | 1.0000 | 80366.5 | 80755.9 | 0.0124 | 0.0126 | 225912 |
| kv_page | zstd-3 | 744599 | 0.7101 | 534.1 | 3051.1 | 0.3278 | 0.3452 | 225912 |
| kv_page | zstd-19 | 703563 | 0.6710 | 11.2 | 7066.9 | 0.1415 | 0.1950 | 225912 |
| kv_page | huffman-canonical-v1 | 702301 | 0.6698 | 478.5 | 508.1 | 1.9683 | 1.9826 | 225912 |
| kv_page | rans | 698896 | 0.6665 | 262.9 | 253.9 | 3.9390 | 3.9666 | 225912 |
| kv_page | tunstall-8 per-plane | 1050132 | 1.0015 | 119.0 | 366.6 | 2.7280 | 2.7519 | 225912 |
| kv_page | tunstall-8 per-tensor | 1049362 | 1.0007 | 120.4 | 367.3 | 2.7226 | 2.7605 | 225912 |
| kv_page | tunstall-8 family-shared | 1049362 | 1.0007 | 120.4 | 368.2 | 2.7159 | 2.7499 | 225912 |
| kv_page | tunstall-10 per-plane | 1038659 | 0.9905 | 87.6 | 320.2 | 3.1227 | 3.1866 | 225912 |
| kv_page | tunstall-10 per-tensor | 1038544 | 0.9904 | 87.9 | 321.5 | 3.1102 | 3.1725 | 225912 |
| kv_page | tunstall-10 family-shared | 1038544 | 0.9904 | 87.7 | 320.8 | 3.1174 | 3.1603 | 225912 |
| kv_page | tunstall-12 per-plane | 1083057 | 1.0329 | 77.3 | 393.8 | 2.5396 | 2.7536 | 225912 |
| kv_page | tunstall-12 per-tensor | 1108093 | 1.0568 | 83.6 | 448.1 | 2.2317 | 2.3450 | 225912 |
| kv_page | tunstall-12 family-shared | 1108093 | 1.0568 | 83.6 | 446.9 | 2.2379 | 2.3821 | 225912 |
| rows_100 | raw | 204800 | 1.0000 | 110159.3 | 108869.8 | 0.0018 | 0.0025 | 225912 |
| rows_100 | zstd-3 | 144937 | 0.7077 | 536.1 | 3094.4 | 0.0631 | 0.0692 | 225912 |
| rows_100 | zstd-19 | 137343 | 0.6706 | 9.3 | 6908.1 | 0.0283 | 0.0432 | 225912 |
| rows_100 | huffman-canonical-v1 | 137428 | 0.6710 | 367.5 | 508.0 | 0.3845 | 0.4423 | 225912 |
| rows_100 | rans | 137020 | 0.6690 | 267.0 | 257.9 | 0.7573 | 0.7767 | 225912 |
| rows_100 | tunstall-8 per-plane | 206356 | 1.0076 | 118.1 | 369.5 | 0.5286 | 0.5638 | 225912 |
| rows_100 | tunstall-8 per-tensor | 205586 | 1.0038 | 117.7 | 370.8 | 0.5267 | 0.5612 | 225912 |
| rows_100 | tunstall-8 family-shared | 205586 | 1.0038 | 116.6 | 369.0 | 0.5293 | 0.5648 | 225912 |
| rows_100 | tunstall-10 per-plane | 208845 | 1.0198 | 88.1 | 329.9 | 0.5921 | 0.6341 | 225912 |
| rows_100 | tunstall-10 per-tensor | 205822 | 1.0050 | 88.4 | 331.4 | 0.5894 | 0.6226 | 225912 |
| rows_100 | tunstall-10 family-shared | 205822 | 1.0050 | 88.7 | 332.7 | 0.5870 | 0.6095 | 225912 |
| rows_100 | tunstall-12 per-plane | 238864 | 1.1663 | 77.3 | 398.3 | 0.4903 | 0.6039 | 225912 |
| rows_100 | tunstall-12 per-tensor | 230702 | 1.1265 | 83.8 | 451.5 | 0.4326 | 0.4651 | 225912 |
| rows_100 | tunstall-12 family-shared | 230702 | 1.1265 | 83.8 | 450.9 | 0.4331 | 0.4667 | 225912 |
| row_1 | raw | 2048 | 1.0000 | 48828.1 | 39062.5 | 0.0000 | 0.0001 | 225912 |
| row_1 | zstd-3 | 1509 | 0.7368 | 315.5 | 923.9 | 0.0021 | 0.0033 | 225912 |
| row_1 | zstd-19 | 1446 | 0.7061 | 29.6 | 1210.9 | 0.0016 | 0.0020 | 225912 |
| row_1 | huffman-canonical-v1 | 1878 | 0.9170 | 11.8 | 310.4 | 0.0063 | 0.0079 | 225912 |
| row_1 | rans | 2168 | 1.0586 | 248.3 | 250.2 | 0.0078 | 0.0097 | 225912 |
| row_1 | tunstall-8 per-plane | 3604 | 1.7598 | 119.2 | 367.1 | 0.0053 | 0.0054 | 225912 |
| row_1 | tunstall-8 per-tensor | 2834 | 1.3838 | 118.6 | 367.8 | 0.0053 | 0.0054 | 225912 |
| row_1 | tunstall-8 family-shared | 2834 | 1.3838 | 113.9 | 366.4 | 0.0053 | 0.0067 | 225912 |
| row_1 | tunstall-10 per-plane | 9714 | 4.7432 | 87.4 | 415.6 | 0.0047 | 0.0060 | 225912 |
| row_1 | tunstall-10 per-tensor | 5904 | 2.8828 | 88.8 | 420.2 | 0.0046 | 0.0048 | 225912 |
| row_1 | tunstall-10 family-shared | 5916 | 2.8887 | 88.2 | 426.6 | 0.0046 | 0.0054 | 225912 |
| row_1 | tunstall-12 per-plane | 36247 | 17.6987 | 80.9 | 460.9 | 0.0042 | 0.0051 | 225912 |
| row_1 | tunstall-12 per-tensor | 20028 | 9.7793 | 86.1 | 448.2 | 0.0044 | 0.0046 | 225912 |
| row_1 | tunstall-12 family-shared | 20037 | 9.7837 | 86.6 | 445.1 | 0.0044 | 0.0046 | 225912 |

## Tunstall dictionary scopes

`dict` is the canonical serialised dictionary bytes. `total` is the
stream bytes plus the charged dictionary. `amort` is the family
dictionary divided by the declared family member count. The full
dictionary is always also reported, so a shared dictionary's cost cannot
disappear.

| class | shape | k | per-plane dict / total | per-tensor dict / total | family dict / total / amort (N) |
|---|---|---:|---:|---:|---:|
| mode9_bf16 | whole | 8 | 1540 / 8717844 | 770 / 8717074 | 770 / 8717074 / 8716307 (290) |
| mode9_bf16 | whole | 10 | 7666 / 8504360 | 3833 / 8552871 | 3833 / 8552871 / 8549051 (290) |
| mode9_bf16 | whole | 12 | 34465 / 8667332 | 18125 / 9056525 | 18125 / 9056525 / 9038462 (290) |
| mode9_bf16 | rows_100 | 8 | 1540 / 974356 | 770 / 973586 | 770 / 973586 / 972819 (290) |
| mode9_bf16 | rows_100 | 10 | 7666 / 955847 | 3833 / 957907 | 3833 / 957907 / 954087 (290) |
| mode9_bf16 | rows_100 | 12 | 34465 / 997718 | 18125 / 1026586 | 18125 / 1026586 / 1008524 (290) |
| mode9_bf16 | row_1 | 8 | 1540 / 11284 | 770 / 10514 | 770 / 10514 / 9747 (290) |
| mode9_bf16 | row_1 | 10 | 7666 / 17280 | 3833 / 13521 | 3833 / 13521 / 9701 (290) |
| mode9_bf16 | row_1 | 12 | 34465 / 44159 | 18125 / 28348 | 18125 / 28348 / 10286 (290) |
| mode9_fp32 | whole | 8 | 1540 / 4195860 | 770 / 4195090 | 770 / 4195090 / 4194330 (76) |
| mode9_fp32 | whole | 10 | 8431 / 3205105 | 4088 / 3453006 | 4088 / 3453006 / 3448972 (76) |
| mode9_fp32 | whole | 12 | 37270 / 3192574 | 17105 / 3753475 | 17105 / 3753475 / 3736595 (76) |
| mode9_fp32 | rows_100 | 8 | 1540 / 206356 | 770 / 205586 | 770 / 205586 / 204826 (76) |
| mode9_fp32 | rows_100 | 10 | 8431 / 164545 | 4088 / 172630 | 4088 / 172630 / 168596 (76) |
| mode9_fp32 | rows_100 | 12 | 37270 / 191192 | 17105 / 199534 | 17105 / 199534 / 182654 (76) |
| mode9_fp32 | row_1 | 8 | 1540 / 3604 | 770 / 2834 | 770 / 2834 / 2074 (76) |
| mode9_fp32 | row_1 | 10 | 7921 / 9525 | 4088 / 5812 | 4088 / 5812 / 1778 (76) |
| mode9_fp32 | row_1 | 12 | 37015 / 38561 | 16850 / 18724 | 17105 / 18979 / 2099 (76) |
| mode11_dense | whole | 8 | 1540 / 8717844 | 770 / 8717074 | 770 / 8717074 / 8716307 (290) |
| mode11_dense | whole | 10 | 7921 / 7369950 | 4088 / 7529641 | 4088 / 7529641 / 7525567 (290) |
| mode11_dense | whole | 12 | 38290 / 6395633 | 19145 / 7303676 | 20675 / 7616994 / 7596390 (290) |
| mode11_dense | rows_100 | 8 | 1540 / 974356 | 770 / 973586 | 770 / 973586 / 972819 (290) |
| mode11_dense | rows_100 | 10 | 7921 / 829520 | 4088 / 843807 | 4088 / 843807 / 839733 (290) |
| mode11_dense | rows_100 | 12 | 38290 / 747544 | 19145 / 831691 | 20675 / 868399 / 847795 (290) |
| mode11_dense | row_1 | 8 | 1540 / 11284 | 770 / 10514 | 770 / 10514 / 9747 (290) |
| mode11_dense | row_1 | 10 | 7921 / 15943 | 4088 / 12428 | 4088 / 12428 / 8354 (290) |
| mode11_dense | row_1 | 12 | 39310 / 46237 | 19145 / 27072 | 20675 / 29170 / 8566 (290) |
| mode11_sparse | whole | 8 | 3080 / 103025704 | 770 / 103023394 | 770 / 103023394 / 103022634 (76) |
| mode11_sparse | whole | 10 | 18392 / 32212989 | 4598 / 32199195 | 4598 / 32199195 / 32194658 (76) |
| mode11_sparse | whole | 12 | 171440 / 9829857 | 42860 / 9701277 | 42860 / 9701277 / 9658981 (76) |
| mode11_sparse | rows_100 | 8 | 3080 / 207912 | 770 / 205602 | 770 / 205602 / 204842 (76) |
| mode11_sparse | rows_100 | 10 | 18392 / 82425 | 4598 / 68631 | 4598 / 68631 / 64094 (76) |
| mode11_sparse | rows_100 | 12 | 171440 / 190677 | 42860 / 62097 | 42860 / 62097 / 19801 (76) |
| mode11_sparse | row_1 | 8 | 3080 / 5160 | 770 / 2850 | 770 / 2850 / 2090 (76) |
| mode11_sparse | row_1 | 10 | 18392 / 19065 | 4598 / 5271 | 4598 / 5271 / 734 (76) |
| mode11_sparse | row_1 | 12 | 171440 / 171669 | 42860 / 43089 | 42860 / 43089 / 793 (76) |
| mode15_tiles | whole | 8 | 1540 / 8717844 | 770 / 8717074 | 770 / 8717074 / 8716309 (169) |
| mode15_tiles | whole | 10 | 7666 / 8578358 | 3833 / 8606350 | 3833 / 8606350 / 8602540 (169) |
| mode15_tiles | whole | 12 | 34465 / 8753472 | 17870 / 9080329 | 17870 / 9080329 / 9062565 (169) |
| mode15_tiles | rows_100 | 8 | 1540 / 974356 | 770 / 973586 | 770 / 973586 / 972821 (169) |
| mode15_tiles | rows_100 | 10 | 7666 / 964078 | 3833 / 963648 | 3833 / 963648 / 959838 (169) |
| mode15_tiles | rows_100 | 12 | 34465 / 1007352 | 17870 / 1029292 | 17870 / 1029292 / 1011528 (169) |
| mode15_tiles | row_1 | 8 | 1540 / 11284 | 770 / 10514 | 770 / 10514 / 9749 (169) |
| mode15_tiles | row_1 | 10 | 7666 / 17336 | 3833 / 13581 | 3833 / 13590 / 9780 (169) |
| mode15_tiles | row_1 | 12 | 33955 / 43703 | 17870 / 28033 | 17870 / 28045 / 10281 (169) |
| kv_page | kv_page | 8 | 1540 / 1050132 | 770 / 1049362 | 770 / 1049362 / 1048688 (8) |
| kv_page | kv_page | 10 | 7666 / 1038659 | 3833 / 1038544 | 3833 / 1038544 / 1035190 (8) |
| kv_page | kv_page | 12 | 34465 / 1083057 | 17870 / 1108093 | 17870 / 1108093 / 1092457 (8) |
| kv_page | rows_100 | 8 | 1540 / 206356 | 770 / 205586 | 770 / 205586 / 204912 (8) |
| kv_page | rows_100 | 10 | 7666 / 208845 | 3833 / 205822 | 3833 / 205822 / 202468 (8) |
| kv_page | rows_100 | 12 | 34465 / 238864 | 17870 / 230702 | 17870 / 230702 / 215066 (8) |
| kv_page | row_1 | 8 | 1540 / 3604 | 770 / 2834 | 770 / 2834 / 2160 (8) |
| kv_page | row_1 | 10 | 7666 / 9714 | 3833 / 5904 | 3833 / 5916 / 2562 (8) |
| kv_page | row_1 | 12 | 34210 / 36247 | 17870 / 20028 | 17870 / 20037 / 4401 (8) |

What the scopes say:

* **Per-plane is the best byte scope at k=12 for whole and 100-row
  multi-plane sets** in five of six classes: it lets each plane use its
  own distribution. At k=12 it costs 34,465 B (mode 9 BF16 and tiles),
  38,290 B (dense), 34,210 B (KV), and 171,440 B (sparse). The sparse
  case is the exception: its 171 KB per-plane dictionary is so large that
  per-tensor (42,860 B, total 9701277 vs 9829857) wins on total bytes.
* **Per-tensor is not a default.** It wins only where the per-plane
  dictionary is large relative to the streams (the sparse whole planes at
  k=10/12, and KV at k=10 by 115 B). It saves dictionary bytes but loses
  on total bytes in the other measured whole/100-row sets.
* **Family-shared is an amortisation tool, not a compression win.** Its
  stream bytes equal or exceed per-tensor, and its dictionary is only
  cheap per tensor because it is divided by the family size. At k=12 the
  18,125–42,860 B family dictionary amortises to 62–564 B per tensor for
  the 290/76/169-member families, and to 2,234 B for the 8-page KV
  family. That makes it the least-bad Tunstall scope for a 1-row region
  (KV row 1: 4,401 B amortised vs 20,028 B per-tensor and 36,247 B
  per-plane), but 4,401 B is still 3.0× the 1,446 B zstd-19 row.
* **At 1-row granularity every static dictionary loses.** The 2 KiB
  payloads are smaller than a k=10/k=12 dictionary (7.7–171 KB). Even
  k=8 (770–3,080 B) roughly doubles or triples the size. A frame-based
  coder (zstd, ~10 B of framing) is the only coder that behaves at this
  granularity.

## Recommendation

### Promote nothing

No wire mode, table, or default changes. `huffman-canonical-v1` stays.
The court does not support replacing it with any measured candidate:

* **rANS** is the only byte winner over the shipped coder (0.5–3.6% on
  BF16/dense/tile/KV whole planes and 100-row regions) and is 0.7–2.4%
  under zstd-19 on the same classes, but it decodes 2× slower than
  Huffman and 24× slower than zstd-19. A 1–3% size win is not worth a
  wire change plus a 2× decode cost. It becomes interesting only with a
  decoder at or above Huffman's throughput; this experiment does not have
  one.
* **zstd-19** is the size reference on FP32 mode-9 (12.1% under rANS)
  and on 1-row regions, but its 28.5 MiB/s encode rules it out as a
  write path. zstd-3 is fast (1807 MiB/s) but 3.9–6.5% larger than the
  shipped Huffman on the BF16/dense/tile/KV classes.
* **Tunstall** is rejected at every k and every dictionary scope. It
  never wins on bytes; k=8 is raw plus dictionary; k=12's only fast
  property is decode (up to 5706 MiB/s), which does not rescue a
  47–59% byte loss on non-sparse whole planes (about 3,000× on the sparse
  whole planes) or a 6.6–30.5× total on 1-row regions.

### Conditional follow-up, not a promotion

* If a future workload is dominated by **dense BF16 residual or mode-9
  BF16 planes** and a decode floor of ~250 MiB/s is acceptable, rANS
  deserves a decoder-engineering pass (table-driven or SIMD) before any
  format decision. Today it is a 1–3% size win for a 2× decode loss
  against the already-shipped coder.
* If **1-row reads** ever become a first-class path, use a frame-based
  coder (zstd) or persist/amortise the dictionary outside the stream.
  Every static dictionary measured here is dominated by its own table at
  that granularity. The shipped Huffman's 256-byte table is 12.5% of a
  2 KiB plane, but zstd-19 still wins the KV row by 23% (1,446 vs 1,878 B).
* If a **family dictionary** is ever built for a store, the dense
  full-checkpoint histogram peaked at 2.14 GiB in this court. That is
  inside the 3 GiB bound but close; stream it or exclude the largest
  tensors instead of holding the base frame and both tensor copies at
  once.

### Evidence against the recommendation

Honest losses for the "promote nothing" choice:

* rANS really is smaller on the classes above; the shipped coder is not
  size-optimal. The decision is a speed/complexity trade, not a claim
  that Huffman is the smallest.
* zstd-19 is clearly smaller on FP32 mode-9 and on 1-row regions; the
  shipped coder loses those byte comparisons.
* The shipped Huffman's 256-byte table is 12.5% of a 2 KiB plane and
  25% of a 1 KiB plane; at very small granularity it is already costly.
* This is one host, warm cache, one tensor per class, and codec-level
  plane streams. It is not a store-planner or fleet measurement.

## Correctness and persistent-format evidence

* Every coder round-trips every one of the 252 rows byte-exactly.
* `WeftCodec/tests/entropy_experimental.rs` (10 tests): edge cases
  (empty, one symbol, all 256 symbols, highly skewed, incompressible,
  non-granular lengths), the documented dictionary rule, shared
  dictionaries over unseen symbols, malformed headers, and **5,000
  randomised cases each for Tunstall and rANS** against independent
  reference decoders plus 5,000 mutated streams each, comparing bytes and
  error classes.
* `cargo test --all-targets` in WeftCodec and in the parent both pass at
  the recorded revisions.
* Cross-language gate: `cross-language gate: all checks passed`, 45 Rust
  frames and 45 Go frames, 36 cross-decodes each way, modes 14/15/16
  byte-identical.
* Conformance replays: Rust and Go each **326 total, 224 pass, 0 fail,
  102 skip** against manifest aggregate
  `1fc710a88e610290e65810d6b0c53a687d02b7316cd7cc5626dd195c07a1dcc0`.
  Receipts `bench/receipts/conformance-rust-20261106T065613Z.json` and
  `bench/receipts/conformance-go-20261106T065613Z.json`.
* No wire mode, table, or frozen format changed. The experimental module
  is reachable only from `lib.rs`'s module declaration, its own tree, the
  example, and the test; the court script greps production sources for
  `experimental::` and fails if it finds one.

## Not run / limitations

* One host, warm page cache; no fleet, cold-cache, or NUMA claim.
* One tensor per class (the same tensor the sparse-delta court uses for
  the sparse pair); no cross-model sweep.
* Codec-level plane streams only; no shipped-frame or store-planner
  comparison.
* No zstd dictionary or long-window mode; levels 3 and 19 only.
* Tunstall dictionaries are built from symbol histograms only (the rule
  mandated by this subphase); no substring-frequency dictionary.
* rANS uses integer division per symbol and is not micro-optimised. Its
  throughput is a property of this implementation, not a bound on rANS.
* The 5,000-case differential suite is deterministic and reproducible,
  not a fuzzer campaign.
* The older receipt `entropy-coder-20261106T064820Z.json` is superseded:
  it was produced before the build-peak capture was reordered and its
  `build_peak_rss_kib` is not meaningful. Its rows are otherwise the same
  shape and are retained only as run history.

