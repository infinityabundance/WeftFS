# Real public model checkpoint family — storage and restore

**Status: implemented, measured twice, single host.** This is the subphase F
headline experiment: two real public checkpoints are ingested into WeftFS as
tensors, every restored byte is checked against the Hugging Face LFS SHA-256, and
the complete storage and restore cost is compared with independent `zstd -19`,
`zstd --patch-from`, and ZipNN.

* **F10 (2026-11-05):** bytes-only planner, no float-tensor candidate.
  Receipt [`bench/receipts/real-model-20261105T012000Z.json`](receipts/real-model-20261105T012000Z.json).
* **R1 (2026-11-06):** measured decode-cost table, mode-9 `FLOAT_TENSOR`
  candidate, and lineage-aware rebasing. Receipt
  [`bench/receipts/planner-float-20261106T025351Z.json`](receipts/planner-float-20261106T025351Z.json).
  The storage headline **flips** (WeftFS now beats ZipNN); the restore table
  gets worse. Both measurements are kept below.
* **R2 (2026-11-06):** table-driven `huffman-canonical-v1` decode and a
  word-at-a-time `FLOAT_TENSOR` plane join. **Storage is byte-identical**; the
  base read is 2.98x faster and the instruct read 2.37x. Receipt
  [`bench/receipts/planner-float-20261106T035316Z.json`](receipts/planner-float-20261106T035316Z.json).
  R2 still loses to zstd on single-threaded decode; that is stated below.

Label: single-host bench court, synthetic-free public corpus, no fleet or
production claim. **The comparison is at matched exactness: every method is
lossless, and every restored checkpoint matched the upstream LFS SHA-256.**

## R2 update: decoder throughput (no wire change)

Two internal changes, no new mode and no encoder-output change:

1. **`huffman-canonical-v1` decode** (modes 9-15): a 12-bit primary lookup
   table over an MSB-aligned 64-bit accumulator. The original canonical
   bit-at-a-time loop is retained for codes longer than 12 bits and for stream
   tails, so validation order, error classes, padding and consumed-byte
   accounting are unchanged. Rust and Go both changed.
2. **`FLOAT_TENSOR` plane join:** one `sign+mantissa` extraction per element
   from an MSB accumulator, written in place into an exactly sized buffer. The
   former loop read two bit fields one bit at a time (~17 ns/element).

Evidence that the bytes did not move: a test-only verbatim reference decoder
in each language, 10,000-case randomized differentials comparing output bytes
**and** error class (Rust and Go), the cross-language gate (`compare`), and
both conformance replays - all unchanged at 206 pass / 0 fail / 101 skip.

### Planner decode table, re-measured

Median of three independent 7-repetition calibrations (128 KiB + 16 MiB, warm
cache, synthetic weight-like BF16), same court:

| class | R1 ns per MiB | **R2 ns per MiB** | speedup |
|---|---:|---:|---:|
| float-tensor (9) | 15,403,165 | **4,683,726** | **3.29x** |
| tensor-tiled (15) | 10,056,794 | **3,366,772** | **2.99x** |
| tensor-sharded (10) | 13,788,933 | **9,123,307** | 1.51x |
| weight-delta (11, incremental) | 5,297,277 | **5,293,987** | 1.00x |
| semantic-delta (14, incremental) | 7,228,350 | **6,458,523** | 1.12x |
| zstd | 814,267 | 779,888 | unchanged path |
| raw | 318,321 | 307,019 | unchanged path |

Isolated single-thread coder microbenchmarks: Huffman decode 4.7-5.0x (Rust)
and 4.1-5.3x (Go); plane join 3.1-5.7x (Rust) and 4.8-6.5x (Go).

`weight-delta` barely moved because its cost is dominated by reconstructing
full byte planes, not by the join, and `tensor-sharded` is now the slowest
class per MiB - the next targeted optimization, not a solved one.

### Restore, same corpus and court

| operation | R1 | **R2** | speedup |
|---|---:|---:|---:|
| base read-only | 13,775.8 ms | **4,627.5 ms** | **2.98x** |
| base materialize | 16,736.9 ms | **6,494.3 ms** | 2.58x |
| instruct read-only | 26,935.7 ms | **11,371.2 ms** | **2.37x** |
| instruct materialize | 29,950.5 ms | **12,092.2 ms** | 2.48x |
| single tensor base | 111.4 ms | **39.8 ms** | 2.80x |
| single tensor instruct | 221.9 ms | **92.7 ms** | 2.39x |

Storage is **byte-identical** to R1: 1,276,128,956 B total, 1,199,958,505 B
data-only, 3.07 % under ZipNN. The planner's default objective is bytes, so a
decoder change cannot move selection at `lambda = 0`. Ingest also got faster
because the planner and verifier decode each candidate: base 147.2 s -> 65.2 s,
instruct phase 241.4 s -> 111.6 s.

The hot-instruct rebase selection falls from 73 of 241 to **1 of 241** at the
same 1e9 ns threshold, because the chain cost per read is now ~3x lower; the
baked table was re-measured rather than reusing the R1 constants.

### What still loses (R2 is not parity)

* Base read-only is still **4.8x** the F10 zstd-strategy read (968.8 ms) and
  ~19x ZipNN's 240 ms in-memory decompressor. Mode 9 is 6.0x zstd per MiB
  (4.68 vs 0.78 ms/MiB), down from 18.9x.
* The bottleneck is now the dependent L1 table lookup per symbol plus the
  per-byte verification and hashing on the read path. The decode is still
  **sequential across the 290 tensors**, and frames are not yet split into
  block-independent streams with an offset table, so neither intra-frame nor
  inter-tensor parallelism exists. That is the next lever; another small loop
  rewrite is not.
* `tensor-sharded` (mode 10) is the slowest class at 9.12 ms/MiB and is used by
  checkpoint shards, not by this store planner.
* Single host, warm cache; the table is court-bound. No fleet claim.

Revisions: WeftFS `ba5a0e6`, WeftCodec `1ee5d90`, WeftCodec-Go `d1aab12`,
WeftFS-Go `bb15514`. Tests at that tree: WeftCodec 204 passed; WeftCodec-Go
`go test ./...` all packages; parent 244 passed; FUSE `--all-targets` 278
passed / 2 ignored; WeftFS-Go `-race` all packages. One transient
`extent_cow::chunk_size_comes_from_the_manifest` failure occurred in the first
fully loaded parallel FUSE run; 10/10 standalone repeats and the rerun pass,
and it is recorded here rather than hidden.

## R1 update: mode-9 planner, measured decode table, rebasing

Measured on the same corpus (SHA-256 re-verified in court), same court limits.
The only storage change is the planner: the candidate set now includes mode-9
`FLOAT_TENSOR` for BF16/FP16, and decode costs come from a court-calibrated
table instead of hand-written linear constants.

### Decode cost table used by the planner

| class | fixed ns | ns per MiB | this run / baked |
|---|---:|---:|---:|
| raw | 10,000 | 318,321 | 0.993 |
| zstd | 17,000 | 814,267 | 1.011 |
| float-tensor (9) | 0 | 15,403,165 | **0.996** |
| tensor-sharded (10) | 32,000 | 13,788,933 | 1.005 |
| tensor-tiled (15) | 14,000 | 10,056,794 | 0.998 |
| weight-delta (11, incremental) | 31,000 | 5,297,277 | 1.024 |
| semantic-delta (14, incremental) | 18,000 | 7,228,350 | 0.994 |

Baked values are the median of three independent 7-repetition calibration runs
(128 KiB + 16 MiB, warm cache, synthetic weight-like BF16). The last column is
this run's calibration divided by the baked value: mode 9 is reproducible to
0.4 %, zstd to 1.1 %, raw to 0.7 % (a previous run varied 20 % on raw, which
is cheap and dominated by host noise).
Modes 12/13 are never produced by the store and keep the declared rows. `--planner-cost-model=declared`
reproduces the old linear model; the default is `measured`.

### Complete storage (family)

| measure | F10 (bytes-only) | **R1 (measured + mode 9)** | R1 vs ZipNN |
|---|---:|---:|---:|
| ZipNN 0.5.4 (recorded) | 1,316,518,229 | 1,316,518,229 | — |
| WeftFS store total | 1,389,785,926 | **1,276,128,956** | **−3.07 %** |
| WeftFS data only | 1,313,652,215 | **1,199,958,505** | −8.87 % |
| indexes | 75,549,540 | 75,584,928 | 5.92 % of total |
| base checkpoint total | 788,405,375 | **674,729,302** | — |
| instruct marginal | 601,380,551 | 601,399,654 | — |

**The storage headline flips.** F10 lost complete storage to ZipNN by 5.27 %;
R1 wins by **3.07 %** (40,389,273 bytes smaller) and is 8.18 % smaller than the
previous WeftFS run (113,656,970 bytes). The reviewer's arithmetic predicted
about 1.20 GB before index overhead and roughly 3 % under ZipNN with indexes;
the measurement is 1,199,958,505 data-only (within 0.003 % of 1.20 GB) and
1,276,128,956 total (−3.07 %), so **the prediction is confirmed**, with the
index share being 5.92 % of this total rather than 3 %.

Strategy distribution:

| checkpoint | raw | zstd | float-tensor (9) | weight-delta (11) |
|---|---:|---:|---:|---:|
| base (290) | 48 (12,288 B) | 73 (130,816 B) | **169 (987,922,432 B)** | 0 |
| instruct (290) | 48 (12,288 B) | 1 (1,792 B) | 0 | **241 (988,051,456 B)** |

The base instruction stream moved from 242 zstd tensors to 169 mode-9 + 73 zstd
+ 48 raw: mode 9 is priced only when it saves at least 1/64 of the zstd frame,
so small/incompressible tensors stay on zstd/raw. The instruct checkpoint was
already represented by weight deltas; mode 9 does not displace them.

### Restore (this is where R1 loses)

| operation | F10 WeftFS | **R1 WeftFS** | ZipNN (recorded) |
|---|---:|---:|---:|
| base read-only | 968.8 ms | **13,776 ms** | 240 ms |
| base materialize | 6,464 ms | **16,737 ms** | 508 ms |
| instruct read-only | 14,367 ms | **26,936 ms** | 196 ms |
| instruct materialize | 23,752 ms | **29,951 ms** | 455 ms |
| single tensor base | 7.3 ms | **111.4 ms** | 508 ms (whole file) |
| single tensor instruct | 118.6 ms | **221.9 ms** | 455 ms (whole file) |

Mode 9 decodes are ~19x slower than zstd in this court (15.4 vs 0.81 ms/MiB).
The byte win is bought with decode time: the base restore is now 14.2x slower
than F10 and 57x slower than ZipNN's in-memory decompressor, and the instruct
restore is 1.9x slower than F10 (its delta base is now mode 9). Single-tensor
reads still move 6.8 MB / 11.6 MB instead of the whole 658–765 MB file, but the
instruct single-tensor time (223 ms) is now slightly slower than ZipNN's
whole-file in-memory decompression (196 ms). This is an honest trade, not a
claim that R1 is uniformly better.

### Hot instruct checkpoint and rebase dry run

After the restore pass, the harness read every instruct tensor 10 times
(9,880,655,360 logical bytes read). `weft rebase --root /instruct --threshold 1000000000 --dry-run`
selected **73 of the 241 weight-delta objects**, all to `float-tensor`, with no
writes:

| object | reads | current decode | independent decode | new bytes |
|---|---:|---:|---:|---:|
| `/model.embed_tokens.weight` | 10 | 5,375,030,143 ns | 3,999,528,062 ns | 180,547,519 |
| `/model.layers.7.mlp.gate_proj.weight` | 10 | 172,103,424 ns | 128,038,809 ns | 5,799,745 |
| `/model.layers.21.mlp.gate_proj.weight` | 10 | 172,103,424 ns | 128,038,809 ns | 5,790,520 |

The rebase decision is `reads x decode cost > threshold` (here 10 x the chain
cost, including the mode-9 base) and requires a standalone representation that
decodes strictly faster and is not larger than the logical bytes. Full rows are
in the receipt under `rebase.objects`.

### Ingest cost and peak RSS

The new planner encodes both zstd and mode-9 candidates for every tensor.
Base ingest 147.6 s, instruct phase 248.4 s (F10: 30.7 s / 105.3 s). Peak RSS
2,720,500 KiB of the 3 GiB court limit during family ingest (F10: 2,540,908 KiB).
This is an encode-time cost of adding a candidate, not a storage cost.

## Corpus

| role | repository | revision | file | bytes | SHA-256 (verified) |
|---|---|---|---|---|---|
| base | `Qwen/Qwen2.5-0.5B` | `060db6499f32faf8b98477b0a26969ef7d8b9987` | `model.safetensors` | 988,097,824 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |
| instruct | `Qwen/Qwen2.5-0.5B-Instruct` | `7ae557604adf67be50417f59c2c2f167def9a775` | `model.safetensors` | 988,097,824 | `fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe` |

Both files are **BF16**, have the same **290 tensor names and shapes**, and the
same 988,065,536-byte tensor data section; the largest tensor is the
151,936 × 896 embedding (272,269,312 bytes). The instruct checkpoint is a real
fine-tune of this base architecture. Sizes and SHA-256 were checked against the
hub's LFS pointers after download (`bench/scripts/real_model_fetch.py`).

### Why not the first-choice SmolLM pair?

The operator's first preference was `HuggingFaceTB/SmolLM-135M` +
`SmolLM-135M-Instruct`. The hub publishes the base as **F32** (538,090,408
bytes) and the instruct model as **BF16** (269,060,552 bytes), even though both
`config.json` files declare `torch_dtype: bfloat16`. Tensor names and shapes are
identical, but dtypes differ, so an exact weight delta is impossible. That pair
was still ingested as a secondary (see below): the planner correctly refuses the
causal parent (`causal_parent_offered=false`) and still restores both files
byte-exactly. Qwen is the headline because both sides are BF16, which is the
case the shared/independent chooser is meant to handle.

The SmolLM secondary is recorded in the same receipt
(`corpus.secondary`) and in
[`bench/corpora/real-models/smollm-135m-pair.json`](corpora/real-models/smollm-135m-pair.json).

## Method

- **WeftFS harness:** `examples/real_model_bench.rs`. It parses the safetensors
  header directly (8-byte LE length + JSON, `dtype`/`shape`/`data_offsets`); no
  transformers dependency. Each tensor's exact bytes are passed to
  `write_tensor_file_with_spec` with `strategy_hint = None` (the shipped
  bytes-only planner, `lambda_ns = 0`), in `data_offsets` order. The instruct
  tensor is offered the **corresponding base tensor version as its causal
  parent**. The safetensors header is stored as a raw object so the complete
  file can be reconstructed byte-for-byte. `chunk_size = 65536`,
  `checkpoint_interval = 256`, default `Raw` v1 manifests.
- **Ingest and restore are separate processes** so each peak RSS is honest.
  Restore reopens read-only, reads and hashes every tensor, recomputes every
  logical root, reassembles the original file, and compares SHA-256 with the
  LFS value. The single-tensor restore runs five times and reports the median.
- **Baseline 1:** `zstd -19 -T1` on each raw safetensors file.
- **Baseline 2:** `zstd --patch-from <base> <instruct>`. `-19` is not runnable
  under the court's 3 GiB limit (recorded below); the reported configuration is
  `-12` for **both** the base and the patch, so it is single-level. zstd 1.5.4
  enables long mode automatically for `--patch-from`.
- **Baseline 3:** ZipNN 0.5.4, the official byte path with its shipped script's
  default `method="HUFFMAN"`, `input_format="byte"`,
  `bytearray_dtype="bfloat16"`, streaming 4 MiB chunks, 4 threads. Round-trip
  SHA-256 was checked inside the same process.
- **Baseline 4:** DFloat11 0.5.0 could not run (see losses).
- **Environment:** `weft/bench-court:1`, image id
  `sha256:e80c438516a509c251d6f3ca4848363a86d12a92287a1d9284117acd9cc0d85a`,
  kernel `7.3.1-1-cachyos`, 3 GiB memory / 3 GiB swap, 4 vCPU, 1024 pids,
  1 GiB `/tmp` tmpfs, `cap-drop=ALL`, `no-new-privileges`. zstd 1.5.4,
  ZipNN 0.5.4, torch 2.15.0+cpu, Python 3.11.2.

ZipNN was run in the bench court because it ships only a source distribution
for Linux and needs a compiler; the Python court image has no compiler and
`apt-get` cannot run under `cap-drop=ALL` (its setuid helper is denied). The
install steps are in the reproduce section and in the receipt.

## Complete storage

All sizes are bytes on disk for **both checkpoints together**.

| method | total bytes | vs ZipNN | encode wall | encode peak RSS | restored exact |
|---|---:|---:|---:|---:|---|
| **ZipNN 0.5.4** (independent) | **1,316,518,229** | — | **0.50 s** | 2,890,276 KiB | yes |
| **WeftFS** (store total) | 1,389,785,926 | **+5.57 %** | 136.0 s | 2,540,908 KiB | yes |
| WeftFS (data only, no indexes/commits) | 1,313,652,215 | −0.22 % | — | — | yes |
| zstd `--patch-from` (both `-12`) | 1,503,140,795 | +14.18 % | 57.1 s | 3,116,888 KiB (patch) | yes |
| zstd `-19` (independent) | 1,528,909,007 | +16.13 % | 538.9 s | 272,160 KiB | yes |

**ZipNN wins complete storage by 5.27 % over WeftFS** (73,267,697 bytes) and by
14–16 % over both zstd configurations. WeftFS data-only is 0.22 % smaller than
ZipNN, i.e. a tie once its 75,549,540 bytes of rebuildable per-commit index
segments and 575,964 bytes of commit records are excluded.

### Per-checkpoint attribution

| checkpoint | WeftFS store | WeftFS data only | zstd `-19` | ZipNN 0.5.4 |
|---|---:|---:|---:|---:|
| base | 788,405,375 | 771,938,949 | 764,815,908 | 658,281,885 |
| instruct (marginal) | 601,380,551 | 541,713,266 | 764,093,099 | 658,236,344 |

The WeftFS base-only store was built separately with the same harness
(`--skip-instruct`, 788,405,375 bytes total), so the instruct marginal figure is
`full store − base-only store` and includes that checkpoint's own commits and
index segments.

- WeftFS **wins the instruct checkpoint by 8.64 % over ZipNN** (601.4 MB vs
  658.2 MB) and by 21.3 % over zstd `-19`. Its delta representation stores the
  fine-tune in 541.7 MB of data, 29.8 % less than the base checkpoint costs as
  independent zstd-3 chunks.
- WeftFS **loses the base checkpoint by 19.8 % to ZipNN** (788.4 MB vs 658.3 MB)
  and by 3.1 % to zstd `-19`. That single loss more than consumes the delta win.

### Strategy distribution (automatic planner)

| checkpoint | raw | zstd | weight-delta | semantic-delta |
|---|---:|---:|---:|---:|
| base (290 tensors) | 48 (12,288 B) | 242 (988,053,248 B) | 0 | 0 |
| instruct (290 tensors) | 48 (12,288 B) | 1 (1,792 B) | **241 (988,051,456 B)** | 0 |

The 48 raw tensors are the 128/896-element RMSNorm and bias vectors, where the
raw bytes are already smaller than a zstd frame. The planner needed no hint: it
priced raw, zstd, and weight-delta for every instruct tensor and chose 241
weight deltas. No semantic delta was applicable: the fine-tune does not preserve
any tensor's mantissa-truncated semantic root.

### Why WeftFS loses on base bytes

The base losses are structural, not a bug:

1. **The independent candidates are zstd level 3 per 64 KiB chunk**, not a
   dtype-aware entropy coder. BF16 weights are near-incompressible for a generic
   LZ matcher; ZipNN separates the byte planes and Huffman-codes the exponent
   stream, which is exactly why it reaches 66.6 % of raw on both files. WeftFS
   has no equivalent candidate for an independent tensor.
2. **Per-tensor framing and manifests** add 43 KB of overhead for raw tensors
   and descriptor/manifest atoms for every object; this is visible but small
   compared with the coding gap.
3. **Advisory index segments** (75.5 MB, 5.4 % of the store) are written after
   every commit and are counted in the headline total. They make restart fast
   but are rebuildable from commits.

The delta path does what it was designed to do: it turns one checkpoint's worth
of weights into ~0.54 GB of dependent bytes and restores them exactly. The net
loss to ZipNN is the base's coding deficit minus the delta win.

## Restore

"Read-only" means the bytes are decoded and hashed but not written to a file.
"Materialize" means the exact safetensors file is written. All times are
wall-clock, medians where repeats were run; the cache is warm and no cache
dropping is available in the court, for any method.

| operation | WeftFS | zstd `-19` | ZipNN 0.5.4 |
|---|---:|---:|---:|
| base, read-only | 968.8 ms | 1,413 ms (`-dc > /dev/null`) | 240 ms (in-memory) |
| instruct, read-only | **14,367 ms** | 1,488 ms | 196 ms |
| base, materialize | 6,464 ms | 976 ms | 508 ms |
| instruct, materialize | **23,752 ms** | 727 ms | 455 ms |

Bytes read from storage (WeftFS backend counter / compressed input size):

| operation | WeftFS | zstd `-19` | ZipNN 0.5.4 |
|---|---:|---:|---:|
| base | 771,794,555 | 764,815,908 | 658,281,885 |
| instruct | 1,313,095,322 (delta + base dependency) | 764,093,099 | 658,236,344 |
| instruct via `--patch-from` | — | 1,737,632,739 (patch + base) | — |

WeftFS restores the base faster than zstd (`-19`) but **loses the full instruct
restore by 9.7× to zstd and 73× to ZipNN** on the read-only path, and by ~33× /
52× when materializing. The cost is the weight-delta Huffman decode plus the
dependency read of the base tensors; the raw weights themselves are not stored.
This is the honest trade for the 8.6 % byte win on that checkpoint.

### Single-tensor restore

`model.layers.10.mlp.down_proj.weight`, median of five runs, read-only:

| checkpoint | WeftFS time | WeftFS bytes read | whole-file alternative |
|---|---:|---:|---|
| base (zstd candidate) | **7.3 ms** | **6,842,059** | zstd/ZipNN must decode the whole file: 0.98 s / 0.51 s, 764.8/658.3 MB |
| instruct (weight-delta) | **118.6 ms** | **11,643,580** (delta + base tensor) | zstd/ZipNN whole file: 0.73 s / 0.46 s, 764.1/658.2 MB |

WeftFS reads 6.8 MB (base) and 11.6 MB (instruct) to return one tensor; the
file-at-a-time formats must read/decompress 658–765 MB. That is a **56–119×**
reduction in bytes and a **6–134×** reduction in wall time for this topology.
`--patch-from` is worse than independent zstd for a one-tensor read: it still
needs the 749.5 MB patch *and* the 988.1 MB base file.

## Ingest

| method | wall | effective input rate | peak RSS |
|---|---:|---:|---:|
| ZipNN 0.5.4 | 0.50 s | ~4.0 GB/s | 2,890,276 KiB (92 % of 3 GiB) |
| zstd `--patch-from` `-12` (base+patch) | 57.1 s | 34.6 MB/s | 3,116,888 KiB (99 % of 3 GiB) |
| WeftFS | 136.0 s | 14.5 MB/s | 2,540,908 KiB |
| zstd `-19` | 538.9 s | 3.7 MB/s | 272,160 KiB |

WeftFS ingest breaks down as 30.7 s for the base and 105.3 s for the instruct
checkpoint. The instruct phase pays for pricing three candidates per tensor
(raw, zstd, weight-delta), including reading the base tensors repeatedly: the
store backend read 19.07 GB during that phase (6.93 GB for the base). That is
an ingest-time cost, not a storage cost, and it is not hidden.

## Losses and availability

1. **ZipNN wins complete storage by 5.27 %** (1,316,518,229 vs 1,389,785,926
   bytes). WeftFS only ties it on data-only bytes.
2. **ZipNN wins full restore** by 60×–73× read-only and 47×–52× materialized on
   the instruct checkpoint, and is also faster on the base.
3. **zstd `--patch-from -19` is unavailable under the court limit.** The attempt
   ran 510.3 s and was killed by the 3 GiB cgroup limit (`returncode 137`,
   `maxrss 3,131,264 KiB`, 622,206,976 partial bytes). The reported patch
   baseline is therefore `-12` for both files, which is a weaker compressor
   than `-19`.
4. **ZipNN peak RSS is 2.89 GB**, 92 % of the 3 GiB limit, because the streaming
   byte path holds the compressed buffer and the decompressed output. It
   completed, but it has very little headroom.
5. **DFloat11 0.5.0 is unavailable in the court.** The wheel installs, but the
   module constructs a CUDA `RawModule` at import (`dfloat11/dfloat11.py:38`)
   and the court exposes no `/dev/nvidia*`; `cupy 14.2.0` reports
   `CUDARuntimeError: cudaErrorInsufficientDriver: CUDA driver version is
   insufficient for CUDA runtime version`. Its README requires a CUDA GPU.
   No DFloat11 bytes or time are claimed.
6. **The first-choice SmolLM pair cannot exercise sharing** (F32 vs BF16). See
   the secondary result below.
7. **Restore timings are warm-cache and single-run for the full-file rows**
   (medians only for the zstd decode repeats). Writeback variance on the host
   filesystem is large: zstd decode-to-file repeats spanned 0.67–6.40 s. The
   zstd and ZipNN materialize rows should be read as "best observed median",
   not a distribution.
8. **No Go parity, no index-free mode, no delta-aware ZipNN.** ZipNN has an XOR
   delta mode; it was not run because the requested baseline is its independent
   lossless compression. A per-tensor or delta-aware ZipNN is the most obvious
   remaining competitor.

## Secondary: SmolLM-135M (cross-dtype)

Ingested with the same harness, no baselines:

| item | result |
|---|---|
| base | `HuggingFaceTB/SmolLM-135M` `model.safetensors`, 538,090,408 B, F32, SHA-256 `c7a387d6…dc093` |
| instruct | `HuggingFaceTB/SmolLM-135M-Instruct` `model.safetensors`, 269,060,552 B, BF16, SHA-256 `c1464c9e…32f2d` |
| causal parent offered | **no** (dtype mismatch for all 272 tensors) |
| strategies | base: 272 zstd; instruct: 272 zstd; no delta |
| store total | 519,706,606 B (data 462,689,964; indexes 56,523,767) |
| restore | both files byte-exact, every logical root exact |
| ingest wall | 18.6 s base + 12.6 s instruct; peak RSS 330,592 KiB |

This confirms the fail-closed behavior: a dtype mismatch produces independent
representations, never a fabricated delta, and exact restoration still holds.

## Reproduce

All commands run from the repository root. The corpus and stores live under
`.docker-cache/real-corpus/` (gitignored); no weights are committed.

```sh
# 1. Fetch and verify against the HF LFS pointers (network required).
scripts/docker-court.sh bench 'python3 bench/scripts/real_model_fetch.py \
  --repo Qwen/Qwen2.5-0.5B --file model.safetensors \
  --dest .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors'
scripts/docker-court.sh bench 'python3 bench/scripts/real_model_fetch.py \
  --repo Qwen/Qwen2.5-0.5B-Instruct --file model.safetensors \
  --dest .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors'

# 2. Build the store-API harness in the rust court.
scripts/docker-court.sh rust 'cargo build --release --example real_model_bench'

# 3. WeftFS ingest, full-store restore, and the base-only attribution store.
scripts/docker-court.sh bench '.docker-cache/target-rust/release/examples/real_model_bench ingest \
  --base-file .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors \
  --instruct-file .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  --store .docker-cache/real-corpus/store-qwen \
  --out .docker-cache/real-corpus/results/qwen-ingest.json'
scripts/docker-court.sh bench '.docker-cache/target-rust/release/examples/real_model_bench restore \
  --store .docker-cache/real-corpus/store-qwen \
  --ingest-json .docker-cache/real-corpus/results/qwen-ingest.json \
  --out .docker-cache/real-corpus/results/qwen-restore.json \
  --reassemble-dir .docker-cache/real-corpus/restored-qwen \
  --single-tensor model.layers.10.mlp.down_proj.weight'
scripts/docker-court.sh bench '.docker-cache/target-rust/release/examples/real_model_bench ingest \
  --base-file .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors \
  --instruct-file .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  --store .docker-cache/real-corpus/store-qwen-base \
  --out .docker-cache/real-corpus/results/qwen-ingest-base-only.json --skip-instruct'

# 4. ZipNN 0.5.4 (source build; CPU torch). The bench court has gcc but no
#    pip or Python headers, and apt cannot run under cap-drop=ALL, so pip is
#    bootstrapped and the Debian libpython3.11-dev headers are unpacked into
#    the cache. Nothing is installed into an image.
scripts/docker-court.sh bench 'export TMPDIR=/work/.docker-cache/pip-tmp; \
  python3 /work/.docker-cache/get-pip.py --break-system-packages --no-warn-script-location; \
  python3 -m pip install --no-cache-dir --target /work/.docker-cache/pylibs \
    --index-url https://download.pytorch.org/whl/cpu torch; \
  export CPATH=/work/.docker-cache/pydev/usr/include/python3.11:/work/.docker-cache/pydev/usr/include; \
  python3 -m pip install --no-cache-dir --target /work/.docker-cache/pylibs \
    --no-deps numpy safetensors zstandard zipnn'
scripts/docker-court.sh bench 'PYTHONPATH=/work/.docker-cache/pylibs python3 bench/scripts/zipnn_bench.py \
  --file .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors \
  --compressed .docker-cache/real-corpus/baselines/Qwen2.5-0.5B.safetensors.znn \
  --out .docker-cache/real-corpus/baselines/zipnn-Qwen2.5-0.5B.json'
scripts/docker-court.sh bench 'PYTHONPATH=/work/.docker-cache/pylibs python3 bench/scripts/zipnn_bench.py \
  --file .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  --compressed .docker-cache/real-corpus/baselines/Qwen2.5-0.5B-Instruct.safetensors.znn \
  --out .docker-cache/real-corpus/baselines/zipnn-Qwen2.5-0.5B-Instruct.json'

# 5. DFloat11 availability probe (records the exact CUDA failure).
scripts/docker-court.sh bench 'PYTHONPATH=/work/.docker-cache/pylibs python3 \
  bench/scripts/dfloat11_probe.py \
  --out .docker-cache/real-corpus/baselines/dfloat11-probe.json'

# 6. zstd baselines, reassembly SHA-256 checks, and the merged receipt.
scripts/docker-court.sh bench 'python3 bench/scripts/real_model_court.py \
  --receipt bench/receipts/real-model-<UTC>.json'

# The SmolLM secondary uses the same harness with the secondary manifest paths.
```

The fetch helper is `bench/scripts/real_model_fetch.py`; the corpus manifests
are `bench/corpora/real-models/*.json`. The receipt records every command,
revision, image id, limit, and observed raw value used above.
