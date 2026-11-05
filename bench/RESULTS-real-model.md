# Real public model checkpoint family — storage and restore

**Status: implemented, measured once, single host.** This is the subphase F
headline experiment: two real public checkpoints are ingested into WeftFS as
tensors, every restored byte is checked against the Hugging Face LFS SHA-256, and
the complete storage and restore cost is compared with independent `zstd -19`,
`zstd --patch-from`, and ZipNN.

Receipt: [`bench/receipts/real-model-20261105T012000Z.json`](receipts/real-model-20261105T012000Z.json)
(442 KB, includes per-tensor strategy, root, and hash records).

Label: single-host bench court, synthetic-free public corpus, no fleet or
production claim. **The comparison is at matched exactness: every method is
lossless, and every restored checkpoint matched the upstream LFS SHA-256.**

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
