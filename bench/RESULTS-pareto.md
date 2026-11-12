# R22 — the write frontier: chunk-parallel ZSTD, exact analyse, and the measured defaults

**Status:** executed on `staging`. No frame, manifest, commit, index or identity
byte changed: every planned row in the frontier court publishes
**282,106,049 B**, which is R18's, R19's, R20's and R21's footprint exactly, and
`default_c_matches_official_court: true` re-proves the shipped C row against the
R3 contract court in the same session.

**Headline.** Chunk-parallel ZSTD is implemented, proved byte-identical, and
**does not move the frozen fixture's write wall** — it cuts the 272 MB
embedding's level-3 frame from 280.3 ms to 135.7 ms and the embedding's whole
encode from 381.3 ms to ~330 ms, but publication and the durability barrier are
the binder at this operating point. The shipped write is **592.6 ms
least-contended / 600.9 ms median** (G3, 1.78× A) at 282,106,049 B. The final
rung (≤ 336 ms) is **not reached**; §7 states the measured limiter and the single
change most likely to close it.

| Receipt | What it is |
|---|---|
| [`r22-pareto-20261112T230000Z.json`](receipts/r22-pareto-20261112T230000Z.json) | **cited frontier.** 25 rows × 7 interleaved repetitions, one rotated order per rep, **11/11 checks pass**, device control 3,200 MB/s (`clean`), A = 332.0 ms. WeftFS `85e25f2` (the code tree is that commit; the receipt was written from it), WeftCodec `41b2a10`. Log and device control beside it. |
| [`augmentation-local-20261112T230000Z.json`](receipts/augmentation-local-20261112T230000Z.json) | **cited.** The unmodified R3 contract court in the same session and stamp; its C row is the identity oracle (`default_c_matches_official_court: true`). |
| [`zstd-chunk-parallel-20261112T231500Z.json`](receipts/zstd-chunk-parallel-20261112T231500Z.json) | **cited.** 22.1: per-frame timings of the level-3 and level-19 frames at 1–4 threads, every timed frame compared byte-for-byte with the sequential encoder (40 checks), plus the analyse histogram cost. |
| [`r22-family-20261112T235800Z.json`](receipts/r22-family-20261112T235800Z.json) | **cited.** 22.5: Qwen2.5-0.5B-Instruct and the pythia-70m step 0/step 8 pair under the shipped defaults (A, C, pipeline, chunk-parallel, exhaustive), with exact-restore rows. |
| [`r22-family-20261112T211500Z.json`](receipts/r22-family-20261112T211500Z.json) | **cited for 3B only.** Qwen2.5-3B-Instruct A/pipeline/chunk-parallel. Measured while the default ZSTD thread count was still 1; the A control is 471.2 ms. |
| [`augmentation-minio-20261112T240000Z.json`](receipts/augmentation-minio-20261112T240000Z.json) | **cited.** 22.5 MinIO arm, the unmodified R3 court over a real S3 backend. |
| `r22-pareto-20261112T220000Z.json`, `…T193000Z.json`, `…T190000Z.json`, `…T165731Z.json` | **Retained failures/superseded.** 220000Z is the same 25-row sweep one default earlier (ZSTD threads fixed at 2); 193000Z had no matching contract-court receipt; 190000Z failed `ablation_encode_counts_exact` before the two-stage candidate ablations were taught to the check; 165731Z was the first attempt, missing the decision-diff row. Every timing in these is real and none is cited. |
| `bench/receipts/r22-probe-20261112T163717Z*` (receipt) and the host probe attempts | **Retained failures: the environment, not the code.** A 200 MiB `conv=fsync` write fell from 1.28 GB/s to 125–180 MB/s and the native control A wrote the 424 MB fixture in **11,482.6 ms** (34× the clean 332 ms). No row from that window is cited. |

---

## 1. 22.1 — chunk-parallel ZSTD

### 1.1 What it is

A CHUNKED ZSTD frame (`WEFTCODEC-FORMAT` §3) is a **concatenation of
independent per-chunk frames**, so the encoder can build them in any order and
concatenate them in index order. `weftcodec::modes::encode_zstd_with` does
exactly that on a bounded pool:

* work is handed out in **ordered batches** of `batch_decoded_bytes` decoded
  bytes, so the pool's transient memory is a function of the *batch*, not of the
  thread count;
* each worker reuses one `zstd::bulk::Compressor` for its chunks
  (`ZSTD_compress2` resets the session on every call), so the 4,155-chunk
  embedding does not allocate 4,155 contexts;
* each chunk's frame is produced by the **same `compress(chunk, level)` call**
  the sequential encoder makes, and its `weft/chunk/v1` hash is computed in the
  same worker;
* the body and the chunk table are assembled in index order, so a batch
  boundary cannot reorder anything.

### 1.2 The identity proof

| Where | What it proves | Result |
|---|---|---|
| `WeftCodec/tests/zstd_chunk_parallel.rs` | every chunk-count boundary: for 4 KiB and 64 KiB chunks, `{k·chunk−1, k·chunk, k·chunk+1}` for `k = 0..=6` plus 7 awkward lengths, at levels **3 and 19**, with **1/2/3/4/8 threads** and **five batch bounds** (one chunk, two, four, sixteen, the default) — 2,800 whole-frame comparisons (2 chunk sizes × 2 levels × 5 thread counts × 5 batch bounds × 28 lengths), each also re-parsed and decoded and compared chunk entry by chunk entry | **5/5 tests pass; 0 mismatches** |
| same file | 2,000 randomised frames (incompressible, 2/16/64/255-symbol, random length/chunk/level/threads/batch) | 0 mismatches |
| same file | all-zero, all-`0xFF`, all-`A` payloads past a chunk boundary; 3-byte partial element; empty input; the sequential fallbacks (below threshold, one thread, no chunking) | 0 mismatches; a zero batch bound is refused `Malformed` |
| `bench/receipts/zstd-chunk-parallel-20261112T231500Z.json` | the **real** 272,269,312 B Qwen2.5-0.5B-Instruct embedding at 64 KiB chunks (4,155 chunks): every timed frame (40 of them) compared with the sequential frame | `all_frames_byte_identical: true` |
| the frontier court | seven interleaved repetitions of every row: footprint **282,106,049 B in every planned row**, `default_c_matches_official_court: true`, `strategies_stable_across_reps: true`, `verify --full` over a chunk-parallel-written store: ok | pass |
| Rust↔Go cross gate (`WeftCodec-Go/hack/cross.sh`, fuse court) | 41 frames decoded each way, all `YES byte-identical` mode fixtures, `cross-language gate: all checks passed` | pass |

### 1.3 The measured saving on that frame

272,269,312 B BF16 embedding, 64 KiB frame chunks, 4,155 chunks, 5 reps each,
warm page cache, bench court:

| threads | level-3 frame | speed-up | level-19 frame | speed-up |
|---:|---:|---:|---:|---:|
| 1 (sequential) | 280.3 ms | 1.00× | 13,597.4 ms | 1.00× |
| 2 | 189.0 ms | 1.48× | 7,182.5 ms | 1.89× |
| 3 | 154.0 ms | 1.82× | 4,875.6 ms | 2.79× |
| 4 | **135.7 ms** | **2.07×** | **3,667.1 ms** | **3.71×** |

Frame bytes are identical at every thread count (210,905,154 B level 3,
210,709,304 B level 19). Peak RSS of the harness (it holds the tensor, the
sequential frame and one parallel frame at once): 908,592 KiB.

### 1.4 The result the projection did not anticipate

The plan projected **~430 ms** from this change. That projection assumed the
embedding's encode was the only critical path. It was not, once the encode got
short. In the cited 7-rep court the shipped pipeline's stages are:

| stage | default (592.6 ms) | note |
|---|---:|---|
| encode wall (all workers) | 410.6 ms | the embedding's own encode is 381.3 ms |
| **publication wall** | **452.5 ms** | publisher busy 391.2 ms |
| **durability writeback** | **461.6 ms** | 202 files `fsync`ed by the early-barrier thread |
| pipeline wall | 551.8 ms | + init/commit/tail = 592.6 ms |

So **the encode is no longer the binder**: publication plus durability is
452–462 ms of a 552 ms pipeline, and the row that shortens the encode furthest
(`C-pipe4-zpar4`, encode wall 392.2 ms against the default's 410.6 ms) lands at
584.8 ms against 592.6 ms — 18 ms of encode wall buys 8 ms of wall, i.e. inside
the spread. The projection is **refuted at this operating point**, and the
negative result is the finding: there is no wall win to claim on the frozen
fixture, only an encode-stage win that the durability path hides.

**Where the win does become a wall win** is where the planned encode is
ZSTD-only, i.e. where there is no mode-9 candidate competing for the CPUs (§6):
pythia-70m goes from 12,204.4 ms to 8,143.8 ms (1.50×) and Qwen 3B improves
18 % over the sequential encoder.

---

## 2. 22.2 — the remaining duplicate passes

The analyse stage's passes, measured:

| term | measured | can it be removed? |
|---|---:|---|
| mode-9 exponent histogram (one full-tensor pass) | **37.1 ms** for the embedding at 7.35 GB/s | **No.** It is what decides whether mode 9 runs at all (the R18 gate) and it is the mode-9 encoder's own input. Moving it into an encode is circular. |
| level-3 sample (4 whole chunks) | ≈ 0.3 ms | Yes, and worth nothing: 4 of 4,155 chunks are 0.1 % of the frame. |
| level-19 sample (2 whole chunks) | ≈ 125 ms across the fixture | **No**, for the same reason: the 19/3 ratio is the level-19 gate's only evidence. Measured by the `t19off` row, whose whole analyse stage is 31.6 ms against the default's 156.9 ms — i.e. the level-19 sample is the dominant analyse cost, not the histogram. |

**What was done instead of a fusion.** The histogram is exactly parallelisable:
it is a vector of integer counts, so whole-element windows accumulated on
separate threads and summed are bit-identical. `float_tensor_exponent_histogram_with`
does that, the store uses it above 8 MiB, and
`WeftCodec/tests/histogram_parallel.rs` proves (a) the counts equal the
single-pass counts for every dtype, thread count and window bound, (b) the
**R18 bit-exact predicted length is unchanged**, and (c) the prediction still
equals the real frame length the encoder emits. The R18 decision court
(`tests/two_stage_planner.rs`, 7 tests) is part of the 807-test rust run and still
passes; `two_stage_matches_exhaustive: true` and
`two_stage_storage_unchanged: true` re-prove it at court level.

**What it bought: nothing measurable at the wall, and that is expected.** The
`C-pipe4-an1` control (sequential histogram) is 586.6 / 614.7 ms against the
default's 592.6 / 600.9 ms — inside the spread of the host. It is a **CPU-work**
reduction: 37 ms of the embedding's serial analyse becomes ~10 ms, which is
worth 6 ms of wall at four CPUs and is invisible under a 452 ms publication
wall. It stays because it is free, exact, and helps the sequential writer.

**What fusion could not do** is stated as a rule: no pass can be fused into an
encode that its own output decides, and the two passes that *could* be merged
(the level-3 candidate and the whole-tensor digests, both of which read the same
bytes) would need the digest to ride inside the codec's chunk loop — a codec
change with a new byte-identity obligation that buys ≈ 75 ms of CPU (19 ms of
wall). It is left open rather than approximated.

---

## 3. 22.3 — the shipped defaults

Both defaults are now **measured**, and both are in the library so the court and
any embedder get the same numbers.

### 3.1 CPU tokens: `n − 1`, not `n`

`TensorPipelineOptions::default_cpu_tokens(encode_threads) = encode_threads − 1`,
floored at 1.

| `cpu_tokens` | wall min | wall median | footprint | what happens |
|---:|---:|---:|---:|---|
| 1 | 912.8 ms | 924.6 ms | 282,106,049 | one busy encoder; the pipeline is serial |
| 2 | 959.8 ms | 982.3 ms | 282,106,049 | **publisher-bound**: one publisher cannot publish 282 MB faster than ~430 ms |
| **3 (shipped)** | **592.6 ms** | **600.9 ms** | 282,106,049 | one CPU left for the reader, the publisher and the writeback thread |
| 4 (previous default) | 612.1 ms | 644.0 ms | 282,106,049 | every CPU busy encoding; the publisher and the barrier wait |

The two numbers that justify it: **592.6 ms against 612.1 ms**
(least-contended) and **600.9 ms against 644.0 ms** (median) of seven
interleaved repetitions — the same 4–5 % gap R21 measured (593.9 vs 623.3) in an
independent session.

*Worse under the new default:* any workload whose wall is set by encoding
throughput rather than by latency, i.e. a saturated host. On pythia-70m the
pipeline is 8,143.8 ms at the default and 7,190.0 ms with three encode threads
and three CPU tokens forced — tokens trade single-stream latency against
aggregate encode throughput.

### 3.2 Chunk-parallel ZSTD: all CPUs, but only when the encode has them

`default_zstd_encode_threads() = min(available_parallelism, 4)`, plus a rule in
the planned writer: **the pool is used only when the tensor's encode has the CPUs
to itself** — if the R21 candidate concurrency is about to run the level-3 and
mode-9 encoders on two threads, the chunk pool is forced to one thread.

The rule is the measurement. On the frozen fixture the embedding runs both
candidates, so the pool is inert there (default 592.6 ms against the
sequential-ZSTD control's 592.2 ms); the ZSTD-only tensors still get it, which is
why `C-pipe4-zpar4` (all CPUs everywhere) is 584.8 ms and the *median* rows in
between are indistinguishable. Where no mode-9 candidate competes the pool is a
large win: pythia-70m 12,204.4 → 8,143.8 ms, Qwen 3B 1,554.3 → 1,319.7 ms
(measured at 3 threads while the default was 1), 35 % fewer encode-wall
milliseconds.

*Worse under the new default:* +5–7 % RSS (939,256 KiB against 693,320 KiB for
the sequential writer) and +5 % CPU; `C-pipe4-tokens4`-style oversubscription is
still available through the budget and is 3 % slower.

---

## 4. 22.4 — the frontier

Fixture: Qwen2.5-0.5B-Instruct, 101 tensors, 424,439,296 logical bytes, 4 CPUs,
3 GiB, local POSIX backend, 7 interleaved repetitions with a rotated order.
`min` is the least-contended repetition (the statistic R21 cites), `med` is the
median (= p50), `p95` is the 95th percentile of the seven. `written` is the
physical bytes the backend was handed, `req` the backend write requests, and
`durability` the early-barrier thread's work — the count of files it `fsync`ed
and its busy milliseconds; the commit's own barrier then closes the batch and
`fsync`s every remaining file and directory before the commit record (one
`backend_sync` per write, measured 1.28 ms in every pipeline row because the
early barrier has already drained it). Device control: 3,200 MB/s (`clean`);
native control A = 332.0 ms, whose own 101 `sync_all` calls are inside its wall
and are not reported by the backend counters (`—`).

Every planned row writes **282,118,555 B** and issues **216** write requests:
the 202 blob publications plus the commit, index and superblock control writes.
The pack rows are the exception (163 and 227 requests) because one request
carries many members.

| config | min ms | med ms | p95 ms | footprint B | written B | req | durability | CPU ms | RSS KiB | restore ms | verdict |
|---|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|---|
| `A` | 332.0 | 339.7 | 6882.5 | 424,439,296 | 424,439,296 | — | — | 100 | 270,532 | 347.7 | native anchor |
| `C-pipe4` | 592.6 | 600.9 | 13578.4 | 282,106,049 | 282,118,555 | 216 | 202 files / 462 ms | 1150 | 939,256 | 1401.7 | **on the frontier (shipped default)** |
| `C-pipe4-zpar4` | 584.8 | 618.5 | 12195.4 | 282,106,049 | 282,118,555 | 216 | 202 files / 456 ms | 1150 | 926,240 | 1412.7 | frontier (wall), not adopted |
| `C-pipe4-t19off` | 575.9 | 593.7 | 11458.2 | 282,106,049 | 282,118,555 | 216 | 202 files / 464 ms | 1060 | 1,019,624 | 1415.0 | frontier here only - not portable (6.3) |
| `C-pipe4-gate4` | 586.3 | 594.9 | 11146.5 | 282,106,049 | 282,118,555 | 216 | 202 files / 461 ms | 1150 | 938,848 | 1409.2 | equivalent to the default |
| `C-pipe4-an1` | 586.6 | 614.7 | 12217.1 | 282,106,049 | 282,118,555 | 216 | 202 files / 468 ms | 1120 | 924,604 | 1423.4 | equivalent to the default |
| `C-pipe4-sync16` | 582.0 | 617.1 | 12378.5 | 282,106,049 | 282,118,555 | 216 | 202 files / 451 ms | 1150 | 910,908 | 1408.4 | equivalent to the default |
| `C-pipe4-zpar3` | 593.1 | 608.4 | 12892.6 | 282,106,049 | 282,118,555 | 216 | 202 files / 462 ms | 1150 | 938,812 | 1415.3 | equivalent |
| `C-pipe4-zpar1` | 592.2 | 611.8 | 13510.2 | 282,106,049 | 282,118,555 | 216 | 202 files / 461 ms | 1160 | 938,088 | 1415.4 | equivalent (pool inert here) |
| `C-pipe4-zpar2` | 598.0 | 620.1 | 11654.9 | 282,106,049 | 282,118,555 | 216 | 202 files / 482 ms | 1150 | 949,388 | 1404.8 | equivalent |
| `C-pipe4-tokens4` | 612.1 | 644.0 | 11605.6 | 282,106,049 | 282,118,555 | 216 | 202 files / 503 ms | 1210 | 975,160 | 1401.2 | dominated by the default |
| `C-pipe4-tm9off` | 593.6 | 649.2 | 12638.8 | 330,355,860 | 330,368,366 | 216 | 196 files / 461 ms | 940 | 908,960 | 705.0 | frontier (wall) but **+48.2 MB (+17.1 %)** |
| `C-pipe4-sync32` | 627.9 | 665.6 | 11631.5 | 282,106,049 | 282,118,555 | 216 | 202 files / 426 ms | 1150 | 932,496 | 1412.8 | dominated |
| `C-pipe8` | 648.7 | 686.3 | 13609.5 | 282,106,049 | 282,118,555 | 216 | 202 files / 458 ms | 1320 | 949,596 | 1411.0 | dominated |
| `C-pipe4-zchunk16384` | 671.7 | 703.2 | 11575.8 | 282,106,049 | 282,118,555 | 216 | 202 files / 467 ms | 1180 | 949,772 | 1417.2 | dominated (13 % slower, bytes identical) |
| `E-pipe4` | 603.4 | 621.8 | 12049.1 | 282,157,248 | 282,127,407 | 216 | 202 files / 456 ms | 1150 | 935,960 | 1408.2 | dominated (protocol cost) |
| `C-pipe4-tokens1` | 912.8 | 924.6 | 12800.9 | 282,106,049 | 282,118,555 | 216 | 186 files / 439 ms | 1140 | 841,252 | 1412.8 | loss (serial encode) |
| `C-pipe4-tokens2` | 959.8 | 982.3 | 13991.8 | 282,106,049 | 282,118,555 | 216 | 196 files / 440 ms | 1110 | 1,014,468 | 1413.0 | loss (publisher-bound) |
| `C-pipe2` | 926.5 | 954.3 | 13287.3 | 282,106,049 | 282,118,555 | 216 | 186 files / 438 ms | 1130 | 842,056 | 1418.9 | loss (publisher-bound) |
| `C-pipe4-pack1m` | 962.7 | 1010.0 | 10870.1 | 282,102,473 | 282,135,559 | 163 | 0 files / 0 ms | 1120 | 899,184 | 1400.8 | **loss, third measured rejection** |
| `C-pipe4-pack8m` | 964.4 | 978.7 | 17038.4 | 282,115,133 | 282,214,123 | 227 | 0 files / 0 ms | 1180 | 1,025,000 | 1411.9 | loss |
| `C-pipe4-leeway4` | 2552.1 | 2617.6 | 12873.1 | 282,106,049 | 282,118,555 | 216 | 200 files / 665 ms | 9930 | 1,030,280 | 1418.8 | **loss** (2.0 s of forced level 19, zero bytes) |
| `C-pipe4-znochunk` | 3518.1 | 3587.0 | 11865.1 | 282,103,649 | 282,116,155 | 216 | 200 files / 416 ms | 8810 | 810,084 | 1409.8 | **loss** (breaks the two-stage predictor) |
| `C` | 1004.4 | 1179.4 | 10587.4 | 282,106,049 | 282,118,555 | 216 | — | 1080 | 690,656 | 1422.2 | dominated (the sequential writer) |
| `C-legacy` | 6583.8 | 6670.3 | 17705.6 | 282,106,049 | 282,118,555 | 216 | — | 21790 | 907,556 | 1413.9 | dominated (exhaustive planner) |

**Pareto-optimal set** (wall against footprint, over every row above):

* **A** — 332.0 ms at 424,439,296 B: fastest, largest. The native anchor.
* **C-pipe4 (shipped)** — 592.6 ms at 282,106,049 B: 1.78× A for 33.5 % fewer bytes.
* **C-pipe4-zpar4** — 584.8 ms at the same bytes: 1.3 % faster than the default and inside the host's spread; not adopted because it gives every tensor the pool even when a mode-9 candidate is competing (§3.2).
* **C-pipe4-t19off** — 575.9 ms at the same bytes: Pareto-dominant *on this fixture*, where the R18 gate already skips every level-19 encode; **not shippable**, because level 19 is where pythia's 76 tensors are won (§6.2).
* **C-pipe4-tm9off** — 593.6 ms at 330,355,860 B: a genuine frontier point only if 17 % more bytes are acceptable. They are not for a compressed-write parity claim.

Everything else is dominated on both axes. The default is the **knee**: the
fastest row that keeps the full 33.5 % reduction and is portable across the four
families measured.

**Losses in this sweep, plainly.**

* `znochunk` (no chunking) is **5.9× slower** and saves 2.4 KB. The two-stage
  sampler's unit *is* the chunk: without chunking 50 of 101 tensors record a
  prediction violation, fall back to exhaustive encoding, and level 19 actually
  runs (6,170 ms of it). Chunking is not an optimisation of the frame; it is what
  makes the frame parallelisable and predictable.
* The pack plane **loses for the third time** (962.7 ms against 592.6 ms). It
  does halve publisher busy time (391.2 → 219.5 ms) and removes all 202 writeback
  files, but sealing packs and publishing catalog segments costs more than the
  writeback it replaces at this operating point.
* `leeway4` (a level-19 gate that tolerates a 4 MiB predicted loss) is 4.4×
  slower for **zero** bytes saved: the R18 gate is doing real work.
* 16 KiB codec chunks are 13 % worse than 64 KiB chunks (671.7 vs 592.6 ms) and
  identical in bytes; the per-chunk frame overhead is real.
* `sync16`/`sync32` are equal to the 8 MiB default within the spread; R21's
  clean-host ordering (8 < 16 < 32) is not reproduced here, so the default stays
  at 8 MiB on R21's evidence.
* **No statistic in this table separates the 576–612 ms cluster.** The same row's
  median moved 13 % between sessions (the default: 600.9 ms here, 682.7 ms in the
  220000Z run, 659.4 ms in 213000Z) while its least-contended value moved 2.5 %.
  The frontier is therefore stated as a **band**, and only differences larger
  than ~10 % are treated as real.

---

## 5. Scoreboard A — the 424 MB court

| requirement | target | measured |
|---|---|---|
| durable write wall | G1 ≤ 3,360 / G2 ≤ 1,344 / G3 ≤ 672 / final ≤ 336 ms | **592.6 min / 600.9 median** → **G3 met**, final **not met** (1.78× A) |
| physical footprint | ≤ 340,000,000 first / ≤ 284,140,000 final | **282,106,049 B** (final band, 33.5 % below A) |
| exact restores | 101/101 | **101/101**, `all_restores_exact: true` in every row, `mismatches: 0` |
| crash consistency | old-or-new whole | `crash_proxy_old_or_new: true` on **every** Weft row (three phases each), 0 partial |
| 4-CPU / 3 GiB court | unchanged | unchanged; peak RSS 939,256 KiB (34 % of the court), 0 OOM, 0 pids refusals |
| Rust/Go format compatibility | no regression | Rust 807/0/0, FUSE 850/0/2 ignored, Rust↔Go cross gate `all checks passed`, `verify --full` ok on a chunk-parallel-written store |

**Write completion and compressed physical footprint describe the same committed
state.** The 592.6 ms wall ends when the commit record and superblock are
published and every blob is durable; the 282,106,049 B is the reachable physical
state that commit names. No deferred or background transcode is counted:
`transcode` exists (`Store::transcode`, T1) and is **not** used by any row here,
no row publishes raw bytes and compresses later, and no FUSE writeback or
page-cache trick is involved. Where a row *does* move work off the write
(`tm9off`, which simply stores more bytes) the footprint says so.

---

## 6. 22.5 — cross-family and cross-backend

### 6.1 The families

`r22-family-20261112T235800Z` (shipped defaults), one repetition per (family,
row), exactness from the restore row. `A` is the same backend's own durable
per-tensor write with no WeftFS layer.

| family | row | wall ms | CPU ms | footprint B | strategies | restore ms | exact |
|---|---|---:|---:|---:|---|---:|---|
| Qwen2.5-0.5B-Instruct (A = 346.6) | C (sequential) | 1,217.6 | 1,140 | 282,106,049 | 35 m9 / 50 zstd / 16 raw | 1,435.2 | ✅ |
| | **C-pipe4 (shipped)** | **628.4** | 1,170 | 282,106,049 | same | 1,430.1 | ✅ |
| | C-pipe4-zpar3 | 626.0 | 1,310 | 282,106,049 | same | 1,456.9 | ✅ |
| | C-legacy (exhaustive) | 6,703.0 | 21,850 | 282,106,049 | same | 1,422.2 | ✅ |
| pythia-70m step 0 (A = 240.4) | C (sequential) | 12,204.4 | 25,560 | 163,964,176 | **76 zstd** | 709.1 | ✅ |
| | **C-pipe4 (shipped)** | **8,143.8** | 29,760 | 163,964,176 | same | 706.9 | ✅ |
| | C-pipe4-zpar3 | 7,190.0 | 25,760 | 163,964,176 | same | 695.0 | ✅ |
| | C-legacy | 12,228.8 | 25,580 | 163,964,176 | same | 696.0 | ✅ |
| pythia-70m step 8 (A = 238.8) | C (sequential) | 12,176.8 | 25,490 | 163,830,373 | 76 zstd | 700.5 | ✅ |
| | **C-pipe4 (shipped)** | **7,512.6** | 27,330 | 163,830,373 | same | 695.7 | ✅ |
| | C-pipe4-zpar3 | 7,143.0 | 25,790 | 163,830,373 | same | 701.8 | ✅ |
| | C-legacy | 12,247.4 | 25,510 | 163,830,373 | same | 711.5 | ✅ |
| Qwen2.5-3B-Instruct (A = 471.2, receipt `211500Z`) | C-pipe4 | 1,554.3 | 2,090 | 421,550,249 | 13 m9 / 12 zstd | 2,153.6 | ✅ |
| | C-pipe4-zpar3 | 1,319.7 | 2,210 | 421,550,249 | same | 2,167.5 | ✅ |

Readings that matter:

* **Level 19 still wins where it used to.** On pythia-70m every one of the 76
  tensors is ZSTD at 163.96 MB (58.2 % of the native 281.7 MB), and the
  exhaustive planner (`C-legacy`) produces the **same footprint** — so the R18
  two-stage gate selected exactly what exhaustive encoding would have, on a
  family where level 19 is the winner. That is why `t19off` is not shippable even
  though it is Pareto-dominant on the Qwen fixture.
* **Chunk-parallel ZSTD pays where ZSTD is the encode.** 1.50×/1.62× on pythia
  (12.2 s → 8.1 s / 7.5 s) and 1.18× on Qwen 3B, against ~1.00× on Qwen 0.5B
  where mode 9 shares the CPUs.
* **Footprints are family-portable**: 33.5 % reduction on Qwen 0.5B, 41.8 % on
  pythia-70m, 33.8 % on Qwen 3B, all with exact restores.
* **Qwen 3B's peak RSS is 1,980,292 KiB** (63 % of the 3 GiB court) for a 635 MB
  subset. That is the tightest row measured and the reason 7B remains out of
  reach for the per-tensor planned writer (measured in R12: OOM-killed).
* **Qwen2.5-3B base and Qwen2.5-7B were not run in this subphase.** Left open
  explicitly, not extrapolated.

### 6.2 MinIO (the R3 court's S3 arm)

`augmentation-minio-20261112T240000Z`, real MinIO `RELEASE.2025-04-22T22-12-26Z`
in-court, one loopback process, tmpfs data directory, `result: pass`, 8/8 checks.

| config | durable wall ms | bytes written | write requests | reads | storage reduction vs A | cold restore ms |
|---|---:|---:|---:|---:|---:|---:|
| A (native S3 PUT per tensor) | 842.9 | 424,439,296 | 101 | 0 | — | 309.9 |
| **C (planned writer)** | **1,275.4** | **282,132,555** | **225** | 6 | **33.53 %** | 1,545.3 |
| E (C + protocol) | 1,325.2 | 282,141,407 | 225 | 6 | 33.5 % | 1,554.3 |

C is 1.51× A over S3 (against 3.0–3.6× for the *sequential* writer locally),
because request latency dominates A as well, and the 33.5 % reduction is
identical to the local one. **This arm measures the planned writer, not the R22
pipeline**: the family and frontier courts are local-only, so *the R22 pipeline
over S3 is not measured in this subphase* and is left open. MinIO here is one
loopback process with a tmpfs data directory — **not** a cloud, media or
power-loss claim.

---

## 7. The final rung: not reached, and exactly why

The target is ≤ 336 ms (native parity). The measurement is 592.6 ms. Two terms
account for it:

1. **A CPU floor of ≈ 288 ms.** The write costs 1,150 ms of CPU across four
   CPUs. The encode is 614.8 ms of it summed over threads (level 3 319.9, mode 9
   229.4), digest 74.7, analyse 156.9, publication and hashing the rest.
2. **A durability tail that cannot overlap, ≈ 250–300 ms.** The early-barrier
   thread `fsync`s **202 files / 282 MB in 461.6 ms** (612 MB/s) while native
   writes 424 MB in 332 ms (1,278 MB/s, 101 files). The publisher's own wall is
   452.5 ms. The last tensors publish after the last encode, so the tail is
   charged in full.

**The one change most likely to close it**, from the measurements: make the
durable state **fewer and larger objects, without a sealing cost on the critical
path**. The pack plane is the right shape and is already measured to cut
publisher busy time from 391.2 ms to 219.5 ms and to eliminate all 202 writeback
files — it loses only because sealing and the catalog publish are charged to the
publisher (total 962.7 ms). Moving pack sealing and the catalog publication onto
the early-barrier thread (the mechanism R21 already built for blob writeback)
is the concrete next step; the second candidate is the digest pass riding inside
the chunk loop (§2), worth ~19 ms of wall.

---

## 8. Gates

| Gate | Result |
|---|---|
| `scripts/docker-court.sh bench "bash bench/scripts/r22_pareto_court.sh"` | 25 rows × 7 reps, **11/11 checks pass**, `result: pass`, `verify --full` ok, device control clean |
| `scripts/augmentation_court.sh --local` (same stamp) | `result=pass` |
| `scripts/augmentation_court.sh --minio` | `result=pass`, 8/8 checks |
| `scripts/docker-court.sh bench "bash bench/scripts/r22_family_court.sh"` | 15/15 measured rows, every restore exact |
| `cargo test --all-targets` (rust court) | **807 passed / 0 failed / 0 ignored** (170 test binaries) — log `bench/receipts/r22-rust-tests.log` |
| `cargo test --features fuse --all-targets` (fuse court) | **850 passed / 0 failed / 2 ignored** (the two pre-existing kernel/CAP_SETUID tests) — log `bench/receipts/r22-fuse-tests.log` |
| `WeftCodec` `cargo test --test zstd_chunk_parallel` | 5/5 |
| `WeftCodec` `cargo test --test histogram_parallel` | 3/3 |
| Rust↔Go cross gate (`WeftCodec-Go/hack/cross.sh`, fuse court) | 41 frames each way, all mode fixtures `YES byte-identical`, `all checks passed` |
| determinism across worker counts / chunk threads | `tests/tensor_pipeline.rs` 6/6; every court row publishes 282,106,049 B |
| crash proxy | `crash_proxy_old_or_new: true` on every Weft row |
| `scripts/check-claims.sh` / spec drift / conformance manifest | see §9 |

**Not run:** the full `scripts/verify-all.sh` aggregate (the programme's
instruction for this subphase) — so no aggregate receipt is claimed and
`verify_receipt_current` still cites `verify-20261111T182601Z` (45/45).

---

## 9. Reproduce

```
WEFT_AUGMENTATION_STAMP=<stamp> scripts/augmentation_court.sh --local
WEFT_R22_STAMP=<stamp> WEFT_R22_REPS=7 \
  scripts/docker-court.sh bench "bash bench/scripts/r22_pareto_court.sh"
WEFT_R22_STAMP=<stamp2> \
  scripts/docker-court.sh bench "bash bench/scripts/r22_family_court.sh"
WEFT_AUGMENTATION_STAMP=<stamp3> scripts/augmentation_court.sh --minio
scripts/docker-court.sh bench "cargo run --release --quiet --example zstd_chunk_bench -- \
  --fixture /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  --out /work/bench/receipts/zstd-chunk-parallel-<stamp4>.json \
  --levels 3,19 --threads 1,2,3,4 --batch-kib 32768 --reps 5"
```

Court image `weft/bench-court:1`
(`sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559`,
kernel 7.3.3-1-cachyos). Fixtures in `.docker-cache/real-corpus/`, each
SHA-256-pinned by the harness (the Qwen 0.5B pair
`88c14255…0ed342`/`fdf756fa…88fb7fe`, pythia step 0/8
`17a077ab…1aaf8e`/`17254dbf…870e48`, Qwen 3B-Instruct `54917aba…0d91d2`).
