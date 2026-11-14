# N05 — Rust/Go optimization parity

**Status:** executed on `staging`. Rust at `6d79c4c`, Go at `WeftFS-Go`
`a8e8e7a`. One fixture, one subset rule, one seed, both arms in their own
OOM-protected courts.

**Headline.** The Go selector is now the R18 policy: sampled level-3, sampled
19/3 ratio, exact mode 9 against the `1/64` gate, and level 19 only when its
trusted lower bound can beat a measured incumbent. On the fixture it reproduces
Go's own exhaustive decisions **exactly** — every strategy, physical size, frame
id, manifest id and logical root — and it is **materially faster than Go's own
exhaustive path**:
<!-- BEGIN GENERATED:n05-speedup (scripts/perf_numbers.py) -->
**6.61×** faster (36,705.3 ms → 5,552.7 ms) at the identical footprint of 282,079,225 B, with 0 errors.
<!-- END GENERATED:n05-speedup -->

Full cross-language agreement on content ids is **not** reached, and cannot be
with the current encoders: Rust links libzstd and Go uses `klauspost/compress`,
so a ZSTD frame is implementation-defined. The two implementations decode every
byte the other writes (both stores pass the other's `verify --full`), and the
mode-9 and raw frames are byte-identical, but any tensor where a ZSTD candidate
is near the winner can choose differently. That is the gate's loss, published
with every divergent tensor in the receipt.

Receipts:

| Receipt | What it is |
|---|---|
| [`n05-go-parity-20261114T203000Z.json`](receipts/n05-go-parity-20261114T203000Z.json) | **cited.** The comparison: per-tensor divergences, agreement counts, both walls, cross-verify results. |
| [`n05-rust-per-tensor-20261114T203000Z.json`](receipts/n05-rust-per-tensor-20261114T203000Z.json) | **cited.** Rust `C-pipe4` write with `--per-tensor-detail`. |
| [`n05-go-exhaustive-20261114T203000Z.json`](receipts/n05-go-exhaustive-20261114T203000Z.json) | **cited.** Go exhaustive (`cmd/tensorparity`, no `-two-stage`). |
| [`n05-go-twostage-20261114T203000Z.json`](receipts/n05-go-twostage-20261114T203000Z.json) | **cited.** Go two-stage. |
| [`n05-cross-verify-rust-on-go-20261114T203000Z.log`](receipts/n05-cross-verify-rust-on-go-20261114T203000Z.log) | **cited.** Rust `verify --full` over the Go-written store. |
| [`n05-cross-verify-go-on-rust-20261114T203000Z.log`](receipts/n05-cross-verify-go-on-rust-20261114T203000Z.log) | **cited.** Go `verify --full` over the Rust-written store. |

---

## 1. What was ported

| Rust mechanism | Go state after N05 | Evidence |
|---|---|---|
| Two-stage analyse → select → materialise | **ported** (`store/two_stage.go`); exact agreement with Go exhaustive | 0 differing strategy / physical size / frame id / manifest id / logical root |
| Exact mode-9 prediction from the exponent histogram | **not ported**; Go encodes mode 9 and measures it. The frame is identical, so the decision is identical. | mode-9 bytes equal on every tensor both chose as mode 9 |
| Chunk-parallel ZSTD | **not ported** | Go still encodes chunks sequentially |
| Grouped publication | **used** (one `BeginGroup`/`CommitGroup` for the 101 tensors) | one group commit; the store still fsyncs each blob immediately |
| Bounded writer pipeline with CPU-token scheduling | **not ported** | Go encodes and publishes one tensor at a time |
| Deferred group durability barrier | **not ported** | Go's local plane fsyncs every published blob at publication |
| Histogram parallelisation | **not applicable** (no histogram predictor in Go) | — |

## 2. The measured improvement and the agreement table

<!-- BEGIN GENERATED:n05-result (scripts/perf_numbers.py) -->
| arm | wall ms | footprint B | peak RSS KiB |
|---|---:|---:|---:|
| Rust `C-pipe4` | 604.0 | 282,106,049 | — |
| Go exhaustive | 36,705.3 | 282,079,225 | 2,323,364 |
| Go two-stage | 5,552.7 | 282,079,225 | 1,733,116 |
<!-- END GENERATED:n05-result -->

<!-- BEGIN GENERATED:n05-agreement (scripts/perf_numbers.py) -->
| comparison | tensors |
|---|---:|
| same chosen strategy | 61 / 101 |
| same strategy **and** physical bytes | 51 |
| divergent | 50 |
| both mode 9, bytes equal | 35 / 35 |
| both raw, bytes equal | 16 / 16 |
| both zstd, bytes equal | 0 / 10 |
<!-- END GENERATED:n05-agreement -->

The residual divergences are all small tensors where the two ZSTD encoders
produce different frame lengths, plus the choice flips that follow from them.
The full list is in the receipt (`divergences[]`), not summarised away. Example:
on a BF16 layernorm Rust's libzstd frame is smaller than Go's mode-9 frame, while
Go's own zstd frame is larger than its mode-9 frame; the two implementations
therefore choose different representations. Both restore the exact tensor.

## 3. Cross-verification, both directions

<!-- BEGIN GENERATED:n05-cross (scripts/perf_numbers.py) -->
* Rust `verify --full` over the Go store: `ok=true`.
* Go `verify --full` over the Rust store: `ok=true`.
<!-- END GENERATED:n05-cross -->

The two verifies check different raw-chunk and representation counts because the
implementations chose different representations; the counts are in the logs. The
difference is the same choice difference, not a format gap.

Recovery and error classes that were exercised:

* the Go fallback path (q8_0, causal deltas, tile shapes, non-zero lambda,
  level-3 interval violation) calls the exhaustive planner; nothing is
  approximated;
* the Go store's own tests cover corrupt/reopen paths unchanged;
* both stores pass the other implementation's `verify --full` after a clean
  reopen.

## 4. Divergences, stated plainly

1. **ZSTD frame bytes and content ids differ** between the implementations.
   This is inherent to two independent ZSTD encoders; it is not smoothed over.
2. **Mode-9 decision path differs in cost**: Go encodes mode 9 to measure it,
   Rust predicts its length. Decisions agree; CPU does not.
3. **Go publishes per-blob durably**, so its wall includes 202 file barriers
   that Rust batches. The generated speedup is against Go's *own* exhaustive
   path, not a claim that Go matches Rust's wall.
4. **No bounded pipeline, no chunk-parallel ZSTD, no CPU-token scheduler** in
   Go. These are the remaining parity gaps.
5. The 0.5B fixture is the only family re-run for this gate. No 3B or pythia
   parity claim is made.

## 5. Losses and non-claims

<!-- BEGIN GENERATED:n05-losses (scripts/perf_numbers.py) -->
* 50 tensors diverge in representation choice or physical size; every one is listed in the receipt.
* Go two-stage peak RSS is 1,733,116 KiB against 2,323,364 KiB exhaustive: the selector removes encode work but Go still holds the tensor and its candidates.
<!-- END GENERATED:n05-losses -->

* Single host, warm page cache, one process per arm; no fleet, cloud, media or
  power-loss claim.
* The Rust and Go walls are not an apples-to-apples throughput comparison
  because the durability mechanisms differ.
* The Go two-stage path is bytes-only; a non-zero lambda falls back to
  exhaustive rather than approximating the pricing.

## 6. Reproduce

```
# Rust per-tensor write
scripts/docker-court.sh bench "cd /work && cargo run --release --quiet \
  --example augmentation_bench -- prepare --fixture \
  .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors --work \
  .docker-cache/n05-rust --max-others 100 --include-embedding 1 && \
  cargo run --release --quiet --example augmentation_bench -- write --work \
  .docker-cache/n05-rust --subset .docker-cache/n05-rust/subset.json \
  --config C-pipe4 --backend local --per-tensor-detail \
  --phase-out .docker-cache/n05-rust/write.json --chunk-size 1048576 \
  --seed weft-augmentation-r3"

# Go exhaustive and two-stage
scripts/docker-court.sh go "cd /work/WeftFS-Go && go build -o \
  /work/.docker-cache/tensorparity ./cmd/tensorparity && \
  /work/.docker-cache/tensorparity -fixture \
  /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  -store /work/.docker-cache/n05-go-exhaustive \
  -out /work/.docker-cache/n05-go-exhaustive.json -max-others 100 && \
  /work/.docker-cache/tensorparity -fixture \
  /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  -store /work/.docker-cache/n05-go-twostage \
  -out /work/.docker-cache/n05-go-twostage.json -max-others 100 -two-stage"

python3 bench/scripts/n05_assemble_receipt.py <rust> <go-exhaustive> \
  <go-two-stage> bench/receipts/n05-go-parity-<stamp>.json <stamp>
```

## Claim register

- `claim:n05_go_two_stage_exact` — the Go two-stage selector reproduces Go exhaustive on every strategy, physical size, frame id, manifest id and logical root.
- `claim:n05_go_material_improvement` — the Go write is materially faster than Go exhaustive at an identical footprint.
- `claim:n05_cross_language_choices_differ` — 50 of 101 tensors choose a different representation or physical size, and the receipt lists every one.
- `claim:n05_mode9_bytes_identical` — mode-9 and raw frames are byte-identical across the implementations.
- `claim:n05_cross_verify_both_ways` — each store passes the other implementation's `verify --full`.
- `claim:n05_go_pipeline_not_ported` — the bounded pipeline, chunk-parallel ZSTD, CPU-token scheduler and deferred barrier are not ported.
