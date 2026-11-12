# R22 — the write frontier: chunk-parallel ZSTD, exact analyse, and the measured defaults

**Status:** executed on `staging`. No frame, manifest, commit, index or identity
byte changed: every planned row in the frontier court publishes
**282,106,049 B**, which is R18's, R19's, R20's and R21's footprint exactly, and
`default_c_matches_official_court: true` re-proves the shipped C row against the
R3 contract court in the same session.

<!-- BEGIN GENERATED:pareto-headline (scripts/perf_numbers.py) -->
**Headline.** Chunk-parallel ZSTD is implemented, proved byte-identical, and
**does not move the frozen fixture's write wall** — it cuts the 272 MB
embedding's level-3 frame, but publication and the durability barrier are the
binder at this operating point. The shipped write is **592.6 ms**
least-contended / 600.9 ms median (G3, 1.78× A) at
282,106,049 B. The final rung (≤ 336 ms) is **not reached**; §7 states
the measured limiter and the single change most likely to close it.
<!-- END GENERATED:pareto-headline -->

Chunk-parallel ZSTD is implemented and proved byte-identical; it cuts the
272 MB embedding's level-3 frame, but publication and the durability barrier
are the binder at this operating point, so the fixture wall does not move.

| Receipt | What it is |
|---|---|
| [`r22-pareto-20261112T230000Z.json`](receipts/r22-pareto-20261112T230000Z.json) | **cited frontier.** The sweep cited throughout; its own `environment.revisions.WeftFS` records `ccc6d69` with the R22.2–R22.5 changes uncommitted at run time, committed as `85e25f2`. Counts and the native anchor are generated below. |
| [`augmentation-local-20261112T230000Z.json`](receipts/augmentation-local-20261112T230000Z.json) | **cited.** The unmodified R3 contract court in the same session and stamp; its C row is the identity oracle (`default_c_matches_official_court: true`). |
| [`zstd-chunk-parallel-20261112T231500Z.json`](receipts/zstd-chunk-parallel-20261112T231500Z.json) | **cited.** 22.1: per-frame timings of the level-3 and level-19 frames at 1–4 threads, every timed frame compared byte-for-byte with the sequential encoder (40 checks), plus the analyse histogram cost. |
| [`r22-family-20261112T235800Z.json`](receipts/r22-family-20261112T235800Z.json) | **cited.** 22.5: Qwen2.5-0.5B-Instruct and the pythia-70m step 0/step 8 pair under the shipped defaults (A, C, pipeline, chunk-parallel, exhaustive), with exact-restore rows. |
| [`r22-family-20261112T211500Z.json`](receipts/r22-family-20261112T211500Z.json) | **cited for 3B only.** Qwen2.5-3B-Instruct A/pipeline/chunk-parallel. Measured while the default ZSTD thread count was still 1; the A control is 471.2 ms. |
| [`augmentation-minio-20261112T240000Z.json`](receipts/augmentation-minio-20261112T240000Z.json) | **cited.** 22.5 MinIO arm, the unmodified R3 court over a real S3 backend. |
| `r22-pareto-20261112T220000Z.json`, `…T193000Z.json`, `…T190000Z.json`, `…T165731Z.json` | **Retained failures/superseded.** 220000Z is the same 25-row sweep one default earlier (ZSTD threads fixed at 2); 193000Z had no matching contract-court receipt; 190000Z failed `ablation_encode_counts_exact` before the two-stage candidate ablations were taught to the check; 165731Z was the first attempt, missing the decision-diff row. Every timing in these is real and none is cited. |
| `bench/receipts/r22-probe-20261112T163717Z*` (receipt) and the host probe attempts | **Retained failures: the environment, not the code.** A 200 MiB `conv=fsync` write fell from 1.28 GB/s to 125–180 MB/s and the native control A wrote the 424 MB fixture in **11,482.6 ms** (34× the clean 332 ms). No row from that window is cited. |

<!-- BEGIN GENERATED:pareto-counts (scripts/perf_numbers.py) -->
This sweep is **25 configurations × 7 interleaved repetitions**, **11/11 checks pass**, on the 101-tensor fixture (424,439,296 logical bytes) in the 4-CPU / 3 GiB court.
<!-- END GENERATED:pareto-counts -->

---

**How to read this document.** The generated regions (§4, §5, §7, §8 and the
histogram row in §2) are emitted from the receipts by
`scripts/perf_numbers.py` and re-validated by the `perf_numbers` court; the
canonical table of every published number is
[`docs/PERF-NUMBERS.md`](../docs/PERF-NUMBERS.md). The prose in §1, §3 and §6.1
is this court's own analysis of the receipts named at the top of each section.
Where a sentence quotes a number outside a generated region it is a
**historical quotation of that receipt**, not a live value; the generated
table is authoritative if the two ever disagree.

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
| mode-9 exponent histogram (one full-tensor pass) | <!-- BEGIN GENERATED:pareto-hist-row (scripts/perf_numbers.py) --> **37.1 ms** for the embedding at 7.35 GB/s <!-- END GENERATED:pareto-hist-row --> | **No.** It is what decides whether mode 9 runs at all (the R18 gate) and it is the mode-9 encoder's own input. Moving it into an encode is circular. |
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

The four CPU-token rows (`C-pipe4-tokens1`, the shipped default, and
`C-pipe4-tokens4`) are in the §4 table with their measured walls, medians,
footprints, RSS and restores; they are not repeated here. The shipped default
is `cpu_tokens = 3`: one CPU left for the reader, the publisher and the
writeback thread. The measurement behind it is the default row against the
four-token row in §4, and R21 measured the same gap in an independent session
(`r21-pipeline-20261112T201500Z`).

*Worse under the new default:* any workload whose wall is set by encoding
throughput rather than by latency, i.e. a saturated host — tokens trade
single-stream latency against aggregate encode throughput.

### 3.2 Chunk-parallel ZSTD: all CPUs, but only when the encode has them

`default_zstd_encode_threads() = min(available_parallelism, 4)`, plus a rule in
the planned writer: **the pool is used only when the tensor's encode has the CPUs
to itself** — if the R21 candidate concurrency is about to run the level-3 and
mode-9 encoders on two threads, the chunk pool is forced to one thread.

<!-- BEGIN GENERATED:pareto-zstd-note (scripts/perf_numbers.py) -->
The rule is the measurement. On the frozen fixture the embedding runs both
candidates, so the pool is inert there (default 592.6 ms against the
sequential-ZSTD control's 592.2 ms); the ZSTD-only tensors still get it,
which is why `C-pipe4-zpar4` (all CPUs everywhere) is 584.8 ms and the
*median* rows in between are indistinguishable.
<!-- END GENERATED:pareto-zstd-note -->

Where no mode-9 candidate competes the pool is a large win: pythia-70m and
Qwen 3B both write substantially faster under it (the family rows are
`r22-family-20261112T235800Z`), because their encode is ZSTD-dominated.

<!-- BEGIN GENERATED:pareto-rss-note (scripts/perf_numbers.py) -->
*Worse under the new default:* RSS 939,256 KiB against 690,656 KiB for the
sequential writer, and slightly more CPU; `C-pipe4-tokens4`-style
oversubscription is still available through the budget and is slower.
<!-- END GENERATED:pareto-rss-note -->

---

## 4. 22.4 — the frontier

<!-- BEGIN GENERATED:pareto-method (scripts/perf_numbers.py) -->
Fixture: Qwen2.5-0.5B-Instruct, 101 tensors, 424,439,296 logical bytes, 4 CPUs,
3 GiB, local POSIX backend, 7 interleaved repetitions with a rotated order.
`min` is the least-contended repetition (the statistic R21 cites), `med` is the
median (= p50), `p95` is the 95th percentile of the seven. Device control: clean;
native control A = 332.0 ms, whose own `sync_all` calls are inside its wall.
<!-- END GENERATED:pareto-method -->

**Two frontiers, stated separately.** Earlier drafts called several rows
"Pareto-optimal" when they had an identical footprint and a higher measured wall
than another row. That was wrong: such a row is *dominated* on the measured
axes. The sets below are computed from the receipt, not asserted.

**The fixture numerical frontier** is the non-dominated set on (least-contended
wall, physical footprint) over every row of this exact fixture, with no policy
filter:

<!-- BEGIN GENERATED:pareto-fixture-frontier (scripts/perf_numbers.py) -->
**A** (332.0 ms, 424,439,296 B), **C-pipe4-t19off** (575.9 ms, 282,106,049 B), **C-pipe4-pack1m** (962.7 ms, 282,102,473 B).
<!-- END GENERATED:pareto-fixture-frontier -->

Every other row — **including the shipped default** — is dominated on these two
axes by one of them. `C-pipe4` shares its footprint with rows that measured a
lower wall; the level-19-off row is faster at the same bytes; the 1 MiB-pack row
is the smallest-footprint point at a much higher wall.

**The portable/eligible policy frontier** removes configurations that win only
by eliminating a candidate another model family needs. The level-19 gate is what
wins pythia-70m (§6.1), so the level-19-off row is not eligible:

<!-- BEGIN GENERATED:pareto-eligible-frontier (scripts/perf_numbers.py) -->
**A** (332.0 ms, 424,439,296 B), **C-pipe4-sync16** (582.0 ms, 282,106,049 B), **C-pipe4-pack1m** (962.7 ms, 282,102,473 B). `C-pipe4-t19off` is excluded by policy: disabling the level-19 gate wins on this fixture and loses pythia-70m (§6.1).
<!-- END GENERATED:pareto-eligible-frontier -->

Within the eligible set the point estimates in the middle of the table are **not
separable by this measurement**: the same row's median moved 13 % between
sessions while its least-contended value moved 2.5 %, and the sync-granularity
ordering R21 measured on a clean host is not reproduced here. Those rows are
therefore one **measurement band**, not a ranking. The default is chosen from
inside that band for a reason measured in R21 (the 8 MiB barrier granularity),
**not because it is the minimum**, and no row here is described as
Pareto-optimal unless it is non-dominated in one of the two sets above.

<!-- BEGIN GENERATED:pareto-table (scripts/perf_numbers.py) -->
| config | min ms | median ms | p95 ms | footprint B | CPU ms | RSS KiB | restore ms | verdict |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| `A` | 332.0 | 339.7 | 6,882.5 | 424,439,296 | 100 | 270,532 | 347.7 | native anchor: fastest, largest |
| `C-pipe4` | 592.6 | 600.9 | 13,578.4 | 282,106,049 | 1,150 | 939,256 | 1,401.7 | shipped default: inside the measurement band, dominated on the point estimates |
| `C-pipe4-zpar4` | 584.8 | 618.5 | 12,195.4 | 282,106,049 | 1,150 | 926,240 | 1,412.7 | inside the band; not adopted (pool policy) |
| `C-pipe4-t19off` | 575.9 | 593.7 | 11,458.2 | 282,106,049 | 1,060 | 1,019,624 | 1,415.0 | numerical frontier; not policy-eligible (disables the level-19 gate) |
| `C-pipe4-gate4` | 586.3 | 594.9 | 11,146.5 | 282,106,049 | 1,150 | 938,848 | 1,409.2 | inside the band |
| `C-pipe4-an1` | 586.6 | 614.7 | 12,217.1 | 282,106,049 | 1,120 | 924,604 | 1,423.4 | inside the band |
| `C-pipe4-sync16` | 582.0 | 617.1 | 12,378.5 | 282,106,049 | 1,150 | 910,908 | 1,408.4 | inside the band; lowest minimum, not separable |
| `C-pipe4-zpar3` | 593.1 | 608.4 | 12,892.6 | 282,106,049 | 1,150 | 938,812 | 1,415.3 | inside the band |
| `C-pipe4-zpar1` | 592.2 | 611.8 | 13,510.2 | 282,106,049 | 1,160 | 938,088 | 1,415.4 | inside the band |
| `C-pipe4-zpar2` | 598.0 | 620.1 | 11,654.9 | 282,106,049 | 1,150 | 949,388 | 1,404.8 | inside the band |
| `C-pipe4-tokens4` | 612.1 | 644.0 | 11,605.6 | 282,106,049 | 1,210 | 975,160 | 1,401.2 | inside the band (budget oversubscription) |
| `C-pipe4-tm9off` | 593.6 | 649.2 | 12,638.8 | 330,355,860 | 940 | 908,960 | 705.0 | dominated on (wall, footprint); its restore is faster, a third axis this frontier does not price |
| `C-pipe4-sync32` | 627.9 | 665.6 | 11,631.5 | 282,106,049 | 1,150 | 932,496 | 1,412.8 | inside the band |
| `C-pipe8` | 648.7 | 686.3 | 13,609.5 | 282,106,049 | 1,320 | 949,596 | 1,411.0 | loss: no better than 4 workers |
| `C-pipe4-zchunk16384` | 671.7 | 703.2 | 11,575.8 | 282,106,049 | 1,180 | 949,772 | 1,417.2 | loss: 16 KiB chunks |
| `E-pipe4` | 603.4 | 621.8 | 12,049.1 | 282,157,248 | 1,150 | 935,960 | 1,408.2 | loss: protocol-layer cost |
| `C-pipe4-tokens1` | 912.8 | 924.6 | 12,800.9 | 282,106,049 | 1,140 | 841,252 | 1,412.8 | loss: serial encode |
| `C-pipe4-tokens2` | 959.8 | 982.3 | 13,991.8 | 282,106,049 | 1,110 | 1,014,468 | 1,413.0 | loss: publisher-bound |
| `C-pipe2` | 926.5 | 954.3 | 13,287.3 | 282,106,049 | 1,130 | 842,056 | 1,418.9 | loss: publisher-bound |
| `C-pipe4-pack1m` | 962.7 | 1,010.0 | 10,870.1 | 282,102,473 | 1,120 | 899,184 | 1,400.8 | numerical frontier by footprint only; loss on wall |
| `C-pipe4-pack8m` | 964.4 | 978.7 | 17,038.4 | 282,115,133 | 1,180 | 1,025,000 | 1,411.9 | loss: pack plane |
| `C-pipe4-leeway4` | 2,552.1 | 2,617.6 | 12,873.1 | 282,106,049 | 9,930 | 1,030,280 | 1,418.8 | loss: forced level 19 for zero bytes |
| `C-pipe4-znochunk` | 3,518.1 | 3,587.0 | 11,865.1 | 282,103,649 | 8,810 | 810,084 | 1,409.8 | loss: breaks the two-stage predictor |
| `C` | 1,004.4 | 1,179.4 | 10,587.4 | 282,106,049 | 1,080 | 690,656 | 1,422.2 | loss: sequential writer |
| `C-legacy` | 6,583.8 | 6,670.3 | 17,705.6 | 282,106,049 | 21,790 | 907,556 | 1,413.9 | loss: exhaustive planner |
<!-- END GENERATED:pareto-table -->

**Losses in this sweep, plainly.**

<!-- BEGIN GENERATED:pareto-losses (scripts/perf_numbers.py) -->
* `znochunk` (no chunking) is **3,518.1 ms** and saves
  2,400 B. The two-stage sampler's unit *is* the chunk: without
  chunking tensors record a prediction violation, fall back to exhaustive
  encoding, and level 19 actually runs. Chunking is not an optimisation of the
  frame; it is what makes the frame parallelisable and predictable.
* The pack plane **loses for the third time** (962.7 ms against
  592.6 ms). It does halve publisher busy time
  (391.2 → 219.5 ms) and removes every writeback file,
  but sealing packs and publishing catalog segments costs more than the
  writeback it replaces at this operating point.
* `leeway4` (a level-19 gate that tolerates a 4 MiB predicted loss) is
  **4.3× slower** for **zero** bytes saved: the R18 gate is doing real
  work.
* 16 KiB codec chunks are **13.4 % worse** than 64 KiB
  (671.7 vs 592.6 ms) and identical in bytes; the per-chunk
  frame overhead is real.
* `sync16`/`sync32` (582.0 and 627.9 ms) are inside the spread;
  R21's clean-host ordering is not reproduced here, so the default stays at
  8 MiB on R21's evidence.
* **No statistic in this table separates the middle band.** The same row's
  median moved 13 % between sessions while its least-contended value moved
  2.5 %. The frontier is therefore stated as a **band**, and only differences
  larger than ~10 % are treated as real.
<!-- END GENERATED:pareto-losses -->

---

## 5. Scoreboard A — the 424 MB court

<!-- BEGIN GENERATED:pareto-scoreboard (scripts/perf_numbers.py) -->
| requirement | target | measured |
|---|---|---|
| durable write wall | G1 ≤ 3,360 / G2 ≤ 1,344 / G3 ≤ 672 / final ≤ 336 ms | **592.6 min / 600.9 median** → **G3 met**, final **not met** (1.78× A) |
| physical footprint | ≤ 340,000,000 first / ≤ 284,140,000 final | **282,106,049 B** (final band, 33.5 % below A) |
| exact restores | 101/101 | **101/101**, `all_restores_exact: true` in every row, `mismatches: 0` |
| crash consistency | old-or-new whole | `crash_proxy_old_or_new: true` on **every** Weft row (three phases each), 0 partial |
| 4-CPU / 3 GiB court | unchanged | unchanged; peak RSS 939,256 KiB, 0 OOM, 0 pids refusals |
| Rust/Go format compatibility | no regression | Rust 807/0/0, FUSE 850/0/2 ignored, Rust↔Go cross gate `all checks passed`, `verify --full` ok on a chunk-parallel-written store |
<!-- END GENERATED:pareto-scoreboard -->

**Write completion and compressed physical footprint describe the same committed
state.** The shipped wall ends when the commit record and superblock are
published and every blob is durable; the footprint is the reachable physical
state that commit names. No deferred or background transcode is counted:
`transcode` exists (`Store::transcode`, T1) and is **not** used by any row here,
no row publishes raw bytes and compresses later, and no FUSE writeback or
page-cache trick is involved.

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

<!-- BEGIN GENERATED:pareto-minio (scripts/perf_numbers.py) -->
| config | durable wall ms | bytes written | cold restore ms |
|---|---:|---:|---:|
| `A` | 842.9 | 424,439,296 | 309.9 |
| `C` | 1,275.4 | 282,132,555 | 1,545.3 |
| `E` | 1,325.2 | 282,141,407 | 1,554.3 |
<!-- END GENERATED:pareto-minio -->

C is slower than A over S3 (against the much larger multiple for the
*sequential* writer locally), because request latency dominates A as well, and
the <!-- BEGIN GENERATED:pareto-minio-note (scripts/perf_numbers.py) --> **33.53 %** <!-- END GENERATED:pareto-minio-note --> reduction is the same as the local one. **This arm measures the planned writer, not the R22
pipeline**: the family and frontier courts are local-only, so *the R22 pipeline
over S3 is not measured in this subphase* and is left open. MinIO here is one
loopback process with a tmpfs data directory — **not** a cloud, media or
power-loss claim.

---

## 7. The final rung: not reached, and exactly why

<!-- BEGIN GENERATED:pareto-limiter (scripts/perf_numbers.py) -->
The target is ≤ 336 ms (native parity). The measurement is 592.6 ms.
Two terms account for it:

1. **Encode work that cannot be compressed further at this operating point.**
   The encode term is 614.8 ms summed over threads (level 3
   319.9, mode 9 229.4), digest 74.7,
   analyse 156.9.
2. **A durability tail that cannot overlap.** The early-barrier thread `fsync`s
   **202 files / 282 MB in 461.6 ms** while native
   writes A's 424,439,296 B in 332.0 ms. The publisher's own wall
   is 452.5 ms. The last tensors publish after the last encode, so
   the tail is charged in full.

**The one change most likely to close it**, from the measurements: make the
durable state **fewer and larger objects, without a sealing cost on the critical
path**. The pack plane is the right shape and is already measured to cut
publisher busy time from 391.2 ms to 219.5 ms and to
eliminate every writeback file — it loses only because sealing and the catalog
publish are charged to the publisher (total 962.7 ms). Moving pack
sealing and the catalog publication onto the early-barrier thread is the
concrete next step.
<!-- END GENERATED:pareto-limiter -->

The second candidate is the digest pass riding inside the chunk loop (§2).

---

## 8. Gates

| Gate | Result |
|---|---|
| `scripts/docker-court.sh bench "bash bench/scripts/r22_pareto_court.sh"` | 25 rows × 7 reps, **11/11 checks pass**, `result: pass`, `verify --full` ok, device control clean |
| `scripts/augmentation_court.sh --local` (same stamp) | `result=pass` |
| `scripts/augmentation_court.sh --minio` | `result=pass`, 8/8 checks |
| `scripts/docker-court.sh bench "bash bench/scripts/r22_family_court.sh"` | 15/15 measured rows, every restore exact |
<!-- BEGIN GENERATED:pareto-gates (scripts/perf_numbers.py) -->
| `cargo test --all-targets` (rust court) | **807 passed / 0 failed** — log `bench/receipts/r22-rust-tests.log` |
| `cargo test --features fuse --all-targets` (fuse court) | **850 passed / 0 failed / 2 ignored** — log `bench/receipts/r22-fuse-tests.log` |
| `WeftCodec` `cargo test --all-targets` (rust court) | **290 passed / 0 failed** — log `bench/receipts/r22-weftcodec-tests.log` |
<!-- END GENERATED:pareto-gates -->
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
