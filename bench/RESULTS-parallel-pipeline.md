# R21 — bounded parallel encode-and-publish pipeline

**Status:** executed. Rust store and harness on `staging`; the codec is
unchanged, and **no frame, manifest, commit, index, or identity byte changed**
(the frozen fixture footprint is 282,106,049 B in every row below, and the
pipeline's committed state is byte-identical for 1, 2, 4 and 8 workers —
`tests/tensor_pipeline.rs`).

**Headline.** The shipped planned writer goes from **1,206.2 ms to 623.3 ms**
(−582.9 ms, −48.3 %) at a **byte-identical footprint**, with 101/101 tensors
restored exactly, the same chosen representations, and the crash proxy still
old-or-new-whole. **G3 (≤ 672 ms) is met** on the least-contended repetition
and on the median (653.9 ms). The best measured configuration
(`--pipeline-cpu-tokens 3`) is **593.9 ms**. Restore is unchanged (1,406.0 vs
1,413.1 ms, host noise): R21 is a write-path change and makes no read claim.

| Receipt | What it is |
|---|---|
| [`r21-pipeline-20261112T201500Z.json`](receipts/r21-pipeline-20261112T201500Z.json) | **cited.** 7 interleaved repetitions × 13 rows, 11/11 checks pass, attribution rep per row (phase profile, pipeline stages, overlap, reservations, early-barrier counters), per-tensor rows, crash proxy on every Weft row. WeftFS `63705c1`, WeftCodec `1412227`. Its log is beside it. |
| [`augmentation-local-20261112T201500Z.json`](receipts/augmentation-local-20261112T201500Z.json) | **cited.** The unmodified R3 contract court in the same session and stamp; its C row is the same-revision identity proof (`default_c_matches_official_court: true`). |
| `r21-pipeline-smoke4.json.zst` (sha256 `289e8320…bd370d`) | Retained probe: the sync-granularity / CPU-token sweep (1 repetition each, `sync8`, `sync16`, `tokens3`, `sync8+tokens3`) that chose the 8 MiB default. Its `at_least_seven_reps` check fails by construction and its timings are **not** cited. |
| `r21-pipeline-smoke3.json.zst` (sha256 `f867fb90…fbccb`) | Retained probe: `sync64`/`sync0`/`tokens4` on the same fixture, used to show that the whole group's writeback on one barrier costs 330 ms. Not cited as a timing. |
| `r21-pipeline-20261112T193000Z.pre-encode-counts-fix.json.zst` (sha256 `5af4c308…6da56ba`) | Retained **failed** run: every timing and exactness check passed but `ablation_encode_counts_exact` failed, because the pipeline branch did not fill `prepare_phases` and the ledger therefore reported zero encoder calls. Fixed (the pipeline's phase delta is now accumulated). Its `.log` is uncompressed. |
| `r21-pipeline-20261112T190000Z.json.zst` (sha256 `12ca954a…f5669`) | Retained earlier matrix (13 rows, 7 reps) at WeftFS `209858e`, before the early barrier and the `encode_counts` fix. Its C-pipe4 row is 621.2 ms, i.e. the same result from the earlier design; superseded by the cited receipt. |

---

## 1. What was sequential

R20's ledger (least-contended C, 1,236.8 ms) is the starting point:

| Phase | ms | Share |
|---|---:|---:|
| encode (all candidates + analyse) | 704.6 | 57.0 % |
| blob publication | 272.2 | 22.0 % |
| durability barrier | 97.0 | 7.8 % |
| digest | 67.0 | 5.4 % |
| source read + init | 52.2 | 4.2 % |
| control plane | 8.4 | 0.7 % |
| residual | 13.4 | 1.1 % |

All of it ran on one thread, one tensor at a time, and the barrier ran after
the last blob. Two facts decide the design:

1. **The 272 MB embedding is 579 ms of the 1,065 ms prepare** — 57 % of the
   whole prepare in one tensor. Its own chain is
   read 19.6 → analyse 45.0 → level-3 295.0 → mode 9 121.6 → digest 45.8 →
   publish 26.8 ms (measured per tensor in the R20 receipt's per-tensor rows).
   No amount of inter-tensor parallelism can go below that chain, so the
   *single-tensor* path had to shrink too.
2. **282 MB of dirty page cache must be durable before the commit record.**
   In the sequential writer the encoders spend 1.2 s on the CPU and the kernel
   writes that 282 MB back in the background, so the closing barrier only pays
   73–97 ms. Shorten the write to ~500 ms and the same barrier costs 330 ms
   (measured, `nosync` row). The writeback has to be moved *under* the
   encoders, not just out of the publisher.

## 2. The pipeline

```text
  reader ──► staging queue ──► encode workers ──► publish queue ──► publisher
 (1, I/O)   (staging_bytes)   (N, CPU + hash)   (encode_bytes)    (1, I/O)
                                                                      │
                                              early-barrier thread ◄──┘
                                              (N × sync_data, every 8 MiB)
```

Three changes to the store, one to the planner and one to the backend:

1. **Encode/publication split.** `Store::encode_tensor_object` runs the
   planner, the candidate encoders, the logical root and the content hash and
   publishes **nothing**; `Store::publish_encoded_tensor` publishes the chosen
   representation's frame (or the raw chunk table) and the descriptor manifest.
   `prepare_tensor_object_with_spec_seeded_planned` is now exactly those two
   halves in order, so there is no second implementation to drift.
2. **`Store::prepare_tensor_group_pipelined`** (`src/tensor_pipeline.rs`) runs
   the three stages with bounded queues and returns one prepared fragment per
   input, in input order. Nothing is committed: the caller installs the
   fragments inside its own group, so the control plane stays on the single
   writer.
3. **Concurrent candidate encode** (`TensorPlanOptions::parallel_candidates`).
   When the R18 decision stream already guarantees that level 3 and mode 9 both
   run, they are encoded on two threads and then consumed in the historical
   order. This is the change that shortens the embedding: 529.4 ms of encode at
   one worker → 448.8 ms at four (and 412.8 with three CPU tokens), because
   295 ms of level 3 and 122 ms of mode 9 become max(295, 122) on the same
   input bytes.
4. **Early barrier** (`StorageBackend::take_pending_writeback_files` +
   `note_external_file_barriers`). The publisher drains the open deferred
   batch's *file* list every `publish_sync_bytes` and hands it to a writeback
   thread that `fsync`s those files with plain `std::fs` **outside** the
   backend lock. The batch stays open, the directory list stays in it, and the
   commit path still `fsync`s everything that remains before the commit record.
   No durability claim is made early and none is weakened: the pipeline joins
   the writeback thread before it returns, so either every drained file is
   durable or the write fails with nothing committed. Commit barrier:
   **329.6 ms → 1.3 ms**.

### 2.1 Reservation arithmetic

Every budget is bytes or CPU tokens; there is no "threads imply memory" step.

| Budget | Default | What it bounds |
|---|---:|---|
| `staging_bytes` | 256 MiB | decoded tensors queued for an encoder |
| `encode_bytes` | 256 MiB | encoded fragments queued for the publisher |
| `publish_bytes` | 128 MiB | the fragment the publisher is writing |
| `worker_bytes` | 1 GiB | decoded + in-flight frames across **all** encoders |
| `cpu_tokens` | = `encode_threads` | concurrently *busy* encoder threads |
| `publish_sync_bytes` | 8 MiB | published bytes between early-barrier passes |

**A worker's reservation is a function of the tensor, not of the thread
count.** One tensor encode holds at most: the canonical decoded bytes (`n`),
the frame being built (`≤ n` + header), and the single retained best frame.
`retain_candidate` keeps exactly one artifact (it drops every other candidate,
and drops same-strategy duplicates such as level 3 / level 19), so two frames
are live at most. `WORKER_TENSOR_MULTIPLIER = 3` plus a 1 MiB slack is
therefore a sound upper bound:

```text
worker_reservation(n) = 3 * n + 1 MiB
reservation_bound(largest) = staging + encode + publish + max(worker_bytes,
                                                              worker_reservation(largest))
```

For this fixture (largest tensor 272,269,312 B):
`3 × 272,269,312 + 1 MiB = 817,856,512 B`, which is below the 1 GiB worker
budget, so `reservation_bound = 256 + 256 + 128 + 1,024 MiB =`
**1,744,830,464 B (1,664 MiB)**.

Measured at the default configuration (least-contended repetition):

| Reservation | Budget | Observed max |
|---|---:|---:|
| staging | 268,435,456 | 272,269,312 |
| worker | 1,073,741,824 | 872,251,392 |
| encode queue | 268,435,456 | 180,547,519 |
| publisher | 134,217,728 | 180,547,519 |
| **peak RSS** | — | **943,335,424 B (921,388 KiB)** |

Three oversize admissions happened and are counted, not hidden
(`oversize_staging: 1`, `oversize_publish: 1`): the 272 MB embedding is bigger
than the 256 MiB staging and encode budgets and bigger than the 128 MiB publish
budget, so it is admitted **alone** — exactly the rule the shipped
`run_bounded` reader uses. That is why two observed maxima exceed their budgets
and why the bound is stated as `max(worker_bytes, worker_reservation)` rather
than as a sum of thread counts. The measured peak (900 MiB) is **54 % of the
1,664 MiB bound** and 30 % of the court's 3 GiB.

### 2.2 Scheduling policy

* **FIFO.** Tensors are claimed in input order, so a single large tensor cannot
  be starved by a stream of small ones: it is claimed before anything behind it.
* **A large tensor needs two tokens.** At or above
  `candidate_parallel_min_bytes` (16 MiB) a worker asks for two CPU tokens
  *before* it takes the tensor, because it will run level 3 and mode 9
  concurrently. Two tokens are granted only when two are free, so two workers
  can never deadlock each holding one, and the number of busy encoder threads
  can never exceed `cpu_tokens`.
* **Oversize admission, never deadlock.** A tensor larger than the staging or
  worker budget, or a fragment larger than the publish budget, is admitted when
  nothing else is in flight.
* **The publisher never waits on a bound the encoders hold**: its queue is fed
  by `admit_encode_queue`, which also admits an oversize fragment alone.
* Waits are measured (`staging`, `worker_bytes`, `encode_queue`,
  `publish_idle`, `cpu_tokens`) so a bound that actually bound is visible in
  the receipt instead of being inferred.

At the default 4-worker configuration the only bound that bound was the CPU
token budget (181.4 ms of wait across four workers) plus 100.2 ms of publisher
idle; staging, worker-byte and encode-queue waits were **0.0 ms**.

## 3. Overlap measurement

Per-tensor stage intervals are recorded by the thread that owns the step
(nanoseconds since the pipeline started), so "overlap" is the measured
intersection of the encode intervals with the merged publication intervals —
not a subtraction of totals.

| Row | Encode wall | Publish wall | Encode busy | Publish busy | Encode time overlapped by publication | Publication time hidden under encode |
|---|---:|---:|---:|---:|---:|---:|
| C (sequential) | — | — | — | — | — | the two stages never overlap |
| C-pipe1 | 767.8 | 435.6 | 767.7 | 435.3 | 251.1 ms (32.7 %) | 251.1 ms (57.7 %) |
| C-pipe2 | 528.5 | 429.5 | 651.7 | 429.2 | 258.7 ms (39.7 %) | 135.6 ms (31.6 %) |
| **C-pipe4** | **448.8** | **490.0** | **820.3** | **409.6** | **739.3 ms (90.1 %)** | **368.2 ms (89.9 %)** |
| C-pipe8 | 432.5 | 473.4 | 778.3 | 352.6 | 656.5 ms (84.3 %) | 311.6 ms (88.4 %) |
| C-pipe4-tokens3 | 412.8 | 454.3 | 688.9 | 400.3 | 620.2 ms (90.0 %) | 358.6 ms (89.6 %) |
| E-pipe4 | 456.9 | 498.9 | 834.0 | 410.2 | 740.2 ms (88.8 %) | 368.0 ms (89.7 %) |

Read the C-pipe4 row as: **90.1 % of all encoder-thread time happened while the
publisher was writing, and 89.9 % of all publication time was hidden under
encoding.** At one worker the same measurement is 32.7 % / 57.7 % — the
pipeline already overlaps *reader*, *encoder and publisher* with a single
encoder, which is why even `C-pipe1` beats the sequential writer (1,114.1 vs
1,206.2 ms) without any parallelism at all.

The early barrier is what makes the publication span usable:

| Row | Early-barrier passes | Files drained early | Early-barrier busy | Commit barrier |
|---|---:|---:|---:|---:|
| C-pipe4 | 9 | 202 | 502.3 ms | **1.3 ms** |
| C-pipe4-sync16 | 6 | 202 | 495.8 ms | 1.3 ms |
| C-pipe4-sync32 | 3 | 202 | 460.7 ms | 1.3 ms |
| C-pipe4-nosync (`sync0`) | 0 | 0 | 0.0 ms | **329.6 ms** |
| C (sequential, no pipeline) | — | — | — | 74.1 ms |

`nosync` is the ablation that proves the point: the same 282 MB of writeback,
on the commit's critical path, costs **329.6 ms**; moved under the encoders it
costs **1.3 ms** of commit time and 502 ms of a thread that is otherwise idle.
`C-pipe1` shows the same effect at one core (21.6 ms commit barrier), which is
why the early barrier is not a parallelism trick.

## 4. Scaling

Seven interleaved repetitions per row, order rotated by one per repetition, the
whole matrix in one bench court (`3 GiB memory == swap, 1024 pids, 4 CPUs,
bounded /tmp, cap-drop=ALL`, limits not raised).

| Row | min | median | p95 | Δ vs C | Footprint (B) | Peak RSS (KiB) | CPU (ms) | Cold restore (ms) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| A (native, no WeftFS layer) | 333.5 | 359.7 | 7,300.0 | — | 424,439,296 | 270,744 | 100 | 346.0 |
| **C (sequential, R20 default)** | **1,206.2** | **1,411.9** | **10,629.4** | — | **282,106,049** | **687,068** | **1,180** | **1,413.1** |
| C-pipe1 | 1,114.1 | 1,124.1 | 11,828.2 | −7.6 % | 282,106,049 | 836,320 | 1,200 | 1,407.0 |
| C-pipe2 | 986.8 | 1,016.0 | 11,394.3 | −18.2 % | 282,106,049 | 1,014,408 | 1,230 | 1,409.2 |
| **C-pipe4 (shipped pipeline)** | **623.3** | **653.9** | **10,830.2** | **−48.3 %** | **282,106,049** | **921,388** | **1,320** | **1,406.0** |
| C-pipe8 | 612.1 | 664.8 | 11,707.7 | −49.3 % | 282,106,049 | 905,436 | 1,350 | 1,414.9 |
| C-pipe4-tokens3 | 593.9 | 636.6 | 12,069.3 | −50.8 % | 282,106,049 | 906,240 | 1,250 | 1,399.3 |
| C-pipe4-sync16 | 638.4 | 670.2 | 11,227.9 | −47.1 % | 282,106,049 | 943,456 | 1,310 | 1,410.3 |
| C-pipe4-sync32 | 676.9 | 720.4 | 11,729.7 | −43.9 % | 282,106,049 | 901,084 | 1,300 | 1,399.3 |
| C-pipe4-nosync | 892.2 | 938.8 | 8,809.1 | −26.0 % | 282,106,049 | 901,152 | 1,330 | 1,400.6 |
| C-legacy (exhaustive planner) | 21,652.9 | 21,846.1 | 32,319.7 | — | 282,106,049 | 892,196 | 21,350 | 1,416.7 |
| E (sequential + protocol) | 1,259.5 | 1,458.1 | 11,475.4 | — | 282,157,242 | 687,756 | 1,190 | 1,420.9 |
| **E-pipe4** | **663.0** | **674.3** | **11,543.9** | **−47.4 %** | **282,157,248** | **938,912** | **1,340** | **1,416.0** |

`p95` is 10–12 s for every row because the host was shared in this session
(the same bimodality R20 published: A's own p95 is 7.3 s against a 333.5 ms
minimum). Per the frozen contract the board uses the least-contended repetition
and the medians/p95 are published unchanged.

### 4.1 Amdahl — what the parallelism can and cannot buy

| | 1 worker | 2 | 4 | 8 |
|---|---:|---:|---:|---:|
| wall (min) | 1,114.1 | 986.8 | 623.3 | 612.1 |
| encode wall | 767.8 | 528.5 | 448.8 | 432.5 |
| embedding encode | 529.4 | — | 448.8 | 432.5 |
| everything else (100 tensors) | 241.7 | — | 371.5 | 378.3 |

The encode wall **equals the embedding's own encode** at 2, 4 and 8 workers:
inter-tensor parallelism is exhausted, and the largest tensor is the Amdahl
cap exactly as R20 predicted. Speedup from 1 to 4 workers is 1.71× of an ideal
4× (43 % efficiency); the remaining 2.3× is not available to this design
because 448.8 ms of the encode wall is one tensor. The two levers that do
work are the ones measured: **intra-tensor candidate parallelism** (embedding
encode 529.4 → 448.8 ms at 4 workers, → 412.8 ms with three CPU tokens) and
**three CPU tokens instead of four** (wall 623.3 → 593.9 ms, −4.7 %, because
the publisher, the early-barrier thread and the kernel's flushers need a core).

### 4.2 Intra-tensor vs inter-tensor: which binds

Both, and the numbers say which is which.

* **Intra-tensor is the binding constraint.** The embedding is 272,269,312 B
  and its two candidate encodes are 295.0 ms (level 3) and 121.6 ms (mode 9).
  Serial they are 416.6 ms of a 579 ms tensor; concurrent they are 295.0 ms.
  No redistribution of the other 100 tensors (371.5 ms of encode, 368.2 ms of
  publication, both shorter than the embedding's chain) can move the wall.
* **Inter-tensor is what makes the rest free.** Those 100 tensors fit entirely
  under the embedding's chain (371.5 ms of encode spread over the other
  threads, 90 % of publication hidden), which is why the total drops by 583 ms
  rather than by the embedding's own 130 ms.

### 4.3 What `analyse` costs now

R20's 184.4 ms of analyse is still inside `encode` and is *not* fused with the
encode pass. R21 does not touch it: `analyse` is 182.6 ms in C and 202.5–243.5
ms in the pipeline rows, where it is summed across threads that now run
concurrently, so the number goes **up** while the wall goes down. Fusing it
(the histogram is already computed by both the analyser and the encoder for
BF16; R19 fused the count, not the pass) is R22 work, and it must keep the R18
exact mode-9 prediction intact — which is why it was not attempted here.

## 5. Determinism across worker counts

`tests/tensor_pipeline.rs`, 6 tests, in the rust court:

* `every_worker_count_publishes_the_same_bytes` writes the same four tensors
  (1 MiB and 256 KiB bf16 plus two 128 B tensors) through 1, 2, 4 and 8
  workers and requires an identical **store head**, identical **per-object
  manifest ids and content roots**, and identical **manifest blob bytes**; each
  store is then opened and `verify(true)` must report `ok` with every object
  and representation checked.
* `candidate_parallelism_does_not_change_a_single_byte` does the same with
  candidate parallelism off (1 worker) against on for every tensor (2 workers,
  threshold 1 B) and requires the same four identities.
* `the_byte_reservations_are_never_exceeded` and
  `an_oversize_tensor_is_admitted_alone` cover the budgets.
* `a_failing_reader_publishes_nothing_and_names_the_tensor` proves an error
  leaves no entry and names the tensor, and
  `a_strategy_hint_still_forces_that_representation` covers the hinted path.

The court-level identity is the R20 check, unchanged:
`default_c_matches_official_court: true` (the contract court's C row and the
matrix's C row are separate processes with the same seed, fixed timestamp and
identical output contents, footprint and written bytes), and
`strategies_stable_across_reps: true` for all 13 rows. `two_stage_storage_unchanged:
true` — the exhaustive control and the two-stage planner produce the **same
282,106,049 B**.

## 6. Crash, GC, index and retention

* **Crash proxy.** Every Weft row runs three fault points
  (`after_objects`, `after_commit_file`, `after_superblock_write`) with
  `WEFT_CRASH_GROUP=1`; `crash_proxy_old_or_new: true`. The pipeline changes
  *where* work happens, not the publication protocol: blobs are still
  create-once and unreferenced until the commit, the commit record is still
  written after every blobs' barrier, and the head is still compare-and-swapped
  last. A crash during a pipeline leaves unreferenced blobs that GC reclaims.
* **Early barrier and durability.** The drained files are `fsync`ed before the
  pipeline returns; the commit barrier closes the batch and `fsync`s every
  remaining file and every touched directory *before* the commit record. A
  failure in either fails the write with nothing committed.
  `durability_barriers()` stays exact across both passes
  (`note_external_file_barriers`).
* **GC, index and retention are untouched.** One attested index segment per
  published checkpoint still covers the whole resulting state, the
  per-root index-GC rule is unchanged, and `tests/index_persistence.rs` (7),
  `tests/index_gc_bound.rs` (3), `tests/retention.rs` (8) and
  `tests/gc_accounting.rs` (3) all pass. No format, no attestation and no
  retention record changed.
* **Product path.** `bench/scripts/r21_pipeline_court.sh` writes a store with
  `C-pipe4` through the harness and then runs the shipped CLI
  `weftfs --store … verify --full`: `ok`, 13 representations checked, 0
  problems.

## 7. Losses, limits and non-claims

* **RSS rises 687 MiB → 921 MiB (+34 %)** for the default pipeline, and to
  943–1,014 MiB for the two-worker and `sync16` rows. That is the price of the
  byte budgets: more tensors and frames are legitimately in flight. The
  measured peak is still 54 % of the stated bound and 30 % of the court.
* **CPU rises 1,180 ms → 1,320 ms (+11.9 %)**. The work is the same; the
  coordination (queues, token budget, and the extra `open`+`sync_data` on the
  drained files) is not free. This is wall at the cost of CPU time, not a
  throughput improvement for a saturated host.
* **`nosync` is 269 ms worse than the default** (892.2 vs 623.3 ms) and is
  reported as a loss even though it is faster than the sequential writer.
* **Two workers is publisher-bound** (986.8 ms): a single publisher thread
  cannot publish 282 MB faster than ~430 ms, so below three encoders the
  pipeline is limited by publication, not by encode.
* **Eight workers buys nothing** (612.1 min vs 623.3 at four; medians 664.8 vs
  653.9). With `cpu_tokens = encode_threads = 8` the court deliberately
  oversubscribes four CPUs; the result is inside host noise of the 4-worker
  row and is **not** claimed as a win.
* **`prepare_phases` for a pipeline row is the whole-pipeline delta, not a sum
  of per-tensor deltas.** The phase collector is process-wide and thread
  additive, so a phase total (e.g. `encode 755.6` against a 578.1 ms pipeline)
  can exceed the wall when several workers run it concurrently; the receipt
  says so and the per-tensor split is absent for pipeline rows.
* **`C-legacy` remains ~21.7 s** (the exhaustive planner encodes level-19 even
  when it selects nothing). It is carried as the decision-diff control and is
  not a candidate.
* **Restore is unchanged** (1,406.0 vs 1,413.1 ms min). The 6-byte footprint
  difference between `E` and `E-pipe4` is directory accounting for the
  protocol's snapshot/context objects; every tensor object id, manifest id and
  content blob is identical, and the difference is 0.000002 % of the fixture.
* **The early barrier does not reduce writeback, it moves it.** Total
  `fsync` work is unchanged (202 files); 8 MiB is the measured best
  granularity on this host (8 MiB 623.3, 16 MiB 638.4, 32 MiB 676.9, 64 MiB
  712.6/729.0, none 892.2 ms).
* **The default `cpu_tokens` is `encode_threads`, not the measured optimum.**
  `--pipeline-cpu-tokens 3` is 593.9 ms (−4.7 %) on a 4-CPU court; that is
  host-dependent tuning, so it is exposed as a budget and not baked in.
* **Single host, one writer, local POSIX backend, warm page cache, one fixture
  subset.** No media, power-loss, fleet, cloud, multi-writer or Go claim. The
  pipeline is Rust-only: Go's store has no prepare/publish split and no
  early-barrier hook.
* **The court is standalone** and is not registered in `scripts/verify-all.sh`
  (like the R20 and R19 courts); `verify-all` was **not** run for this
  subphase, so no aggregate is claimed.

## 8. Gates

| Gate | Result |
|---|---|
| `scripts/docker-court.sh bench "bash bench/scripts/r21_pipeline_court.sh"` | 13 rows × 7 reps, **11/11 checks pass**, `result: pass`; `verify --full` over a pipeline-written store: ok |
| `scripts/augmentation_court.sh --local` (same stamp) | `result=pass` |
| `cargo test --all-targets` (rust court) | 169 binaries, **807 passed / 0 failed / 0 ignored** |
| `cargo test --features fuse --all-targets` (fuse court) | 169 binaries, **850 passed / 0 failed / 2 ignored** (the two pre-existing kernel/CAP_SETUID tests) |
| determinism across worker counts + crash proxy + `verify --full` | `tests/tensor_pipeline.rs` 6/6; crash proxy `true` on every row; CLI `verify --full` ok |
| GC / index / retention | `index_persistence` 7, `index_gc_bound` 3, `retention` 8, `gc_accounting` 3 — all in the rust run above |
| `scripts/check-claims.sh` / spec drift / conformance | 1,030 claims OK / 60/60 / manifest + coverage 420 OK |
| `scripts/verify-all.sh` | **not run** (see §7) |

Raw gate logs (revision `63705c1`, the code tree is the same at `96ee802`)
are committed beside the receipts: `bench/receipts/r21-rust-tests.log`
(169 binaries, 807 passed), `bench/receipts/r21-fuse-tests.log` (169
binaries, 850 passed, 2 ignored) and
`bench/receipts/r21-tensor-pipeline-tests.log` (the 6-test determinism
court).

## 9. What R22 must do

The wall is now 623.3 ms, of which **448.8 ms is one tensor's encode** (72 %).
Inside it, level 3 is 295–337 ms and mode 9 is 122–227 ms (they already
overlap). The single highest-value change is therefore **intra-tensor chunk
parallelism of the level-3 encode**: `weftcodec::modes::encode_zstd` with a
codec chunk size emits one independent ZSTD frame per chunk under the CHUNKED
flag (4,155 frames for the embedding), so compressing those chunks on a bounded
pool is byte-identical by construction — it changes no frame, no chunk table
and no identity, exactly like R19's parallel mode-9 emission. The measured
headroom: 295 ms of level 3 at ~3 threads ≈ 100–120 ms, which would put the
embedding's chain near 250 ms and the write near **430 ms** (1.29× A) without
touching a single representation decision.

After that, in order: (a) fuse `analyse` with the candidate encodes that already
re-read the same bytes (must preserve the exact mode-9 prediction); (b) ship a
measured CPU-token default instead of `encode_threads`; (c) a second publisher
is *not* the answer — the backend mutex serialises publication and the publisher
is only 409.6 ms of a 578.1 ms pipeline.

## 10. Reproduce

```
WEFT_AUGMENTATION_STAMP=<stamp> scripts/augmentation_court.sh --local
WEFT_R21_STAMP=<stamp> WEFT_R21_REPS=7 \
  scripts/docker-court.sh bench "bash bench/scripts/r21_pipeline_court.sh"
```

Court image `weft/bench-court:1`
(`sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559`,
kernel 7.3.3-1-cachyos). Fixture
`.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors`, LFS SHA-256
verified by the harness before every run. Raw samples, per-tensor rows, the
phase profile and the pipeline counters for every timed repetition are in the
receipt; the child-process log is beside it.
