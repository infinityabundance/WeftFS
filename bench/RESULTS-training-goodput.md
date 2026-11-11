# Training goodput and time-to-safe-resume (R11)

Receipt: `bench/receipts/training-goodput-20261110T200930Z.json`
(raw court log beside it), measured at WeftFS `a40c17d`, WeftCodec `4cd729f`,
WeftFS-Go `ad90835` (codec and Go trees unchanged; the harness and docs are
the only new code).

Court: `weft/python-court:1` (`sha256:aec5deb0…76152`), Linux
`7.3.3-1-cachyos`, 3 GiB memory / 3 GiB swap, 4 CPUs, 1024 pids, bounded
1 GiB `/tmp`, `cap-drop=ALL`, `no-new-privileges`. Python 3.12,
**torch 2.15.0+cpu** (the official CPU wheel; no CUDA runtime, no GPU present,
no GPU claim). The court image includes numpy for tensor views; numpy is
present in all three configurations and its RSS is included equally.

Harness: `python/courts/training_goodput.py`, wrapper
`bench/scripts/training_goodput_court.sh`, helper tests
`python/tests/test_goodput_harness.py` (4 passed). The full loop and the
SIGKILL court are a standalone court; they are not part of `verify-all`.

This is the number training engineers use: how many optimizer steps a run
completes per second, and how long it takes from a process death to the first
completed optimizer step afterwards. Compression ratio and single-call save
latency are secondary and are reported only as components.

## 1. The loop

A genuine (small) CPU training loop, not a fixture save:

* three transformer blocks: LayerNorm, explicit multi-head causal attention
  (4 heads, `d_model=96`), residual projections, GELU MLP; token and position
  embeddings, final LayerNorm, output head. **414,432 parameters**,
  4,973,352 bytes of parameters plus AdamW state.
* AdamW (`lr=3e-3`, `betas=(0.9,0.95)`, `weight_decay=0.01`, `eps=1e-8`);
  `loss = cross_entropy(next-token logits, targets)`.
* 256 steps, checkpoint every 64 steps (4 checkpoints per run), batch 8,
  sequence 48, vocabulary 384, 64 seeded synthetic batches:
  `seq[b,t] = (offset[b] + t) mod vocab`, targets are the next token.
* Exactly fixed: `torch.manual_seed(20261110)` before model construction, one
  seeded generator for the data, `torch.use_deterministic_algorithms(True)`,
  `torch.set_num_threads(1)` and `set_num_interop_threads(1)`,
  `OMP/MKL/OPENBLAS/NUMEXPR_NUM_THREADS=1` before torch import, no dropout, no
  augmentation, fixed batch order. All 15 runs of the three configurations
  produced the **same loss-bit trajectory and the same final state hash**.

Checkpoints are asynchronous in every configuration. On a checkpoint step the
training thread clones parameters + AdamW state + step counter (the
synchronisation point), writes the verification spec, and submits the save to
a background worker; it never runs the encoder. The clone is 4.97 MB and is
the same work in A, B and C, so the pause is comparable. The harness never
runs two saves at once; no checkpoint had to wait for the previous save
(`waited_for_previous_ms_max = 0` in every run).

## 2. A / B / C on identical steps and data

A = `torch.save` to a local file, B = PyTorch DCP `FileSystemWriter` to a local
directory, C = PyTorch DCP through `weftcodec.dcp` / `save_checkpoint_async`
(`topology=1`, `zstd` shards, `staging_bytes=64 MiB`). All are async and all
run on the identical 256 steps, seeds and batches.

Step p50/p95 are **pooled over 1,245 clean steps per configuration** (5 runs ×
249; the 4 checkpoint steps and the 3 post-checkpoint harness-verification
steps per run are excluded and reported separately). Pause p50/p95/max are
pooled over 20 checkpoint events per configuration. Wall is the median of 5
runs.

| metric | A `torch.save` | B local DCP | C WeftFS |
|---|---:|---:|---:|
| loop wall, median (ms) | **2,667.0** | 2,747.9 | 3,190.7 |
| process wall, median (ms) | 2,852.3 | 2,943.8 | 3,372.1 |
| training steps/s (256 / step sum) | 96.22 | 93.75 | **94.17** |
| end-to-end steps/s (256 / loop wall) | **95.99** | 93.16 | 80.23 |
| step p50, pooled (ms) | 10.115 | 10.068 | **10.018** |
| step p95, pooled (ms) | 10.630 | 10.848 | **10.581** |
| step max, pooled (ms) | 15.249 | 15.517 | **68.117** |
| step sum, median (ms) | 2,660.6 | 2,730.8 | 2,718.6 |
| harness verification in step sum (ms) | 11.5 | 11.6 | 11.6 |
| final checkpoint completion, median (ms) | **3.605** | 18.348 | 459.023 |
| post-loop tail (wall − step sum), median (ms) | 6.5 | 17.7 | 472.5 |
| save pause p50, pooled (ms) | 0.676 | 0.740 | 3.411 |
| save pause p95, pooled (ms) | 0.821 | 0.923 | 3.621 |
| save pause max, pooled (ms) | 0.863 | 0.962 | **3.695** |
| snapshot clone p50 (ms) | 0.626 | 0.641 | 0.594 |
| async submit p50 (ms) | 0.038 | 0.044 | **2.685** |
| save completion p50 (ms) | **3.862** | 20.112 | 444.130 |
| completion first / later p50 (ms) | 4.476 / 3.742 | 20.500 / 19.467 | 213.495 / 448.964 |
| step the save is observed at (p50, +steps) | 1 | 1 | 29 |
| physical bytes per checkpoint (median) | 5,025,099 | 5,277,059 | **4,924,427** |
| physical bytes, all 4 checkpoints (median) | 20,100,396 | 21,108,236 | **19,700,440** |
| peak RSS, max (KiB) | **370,156** | 371,152 | 379,272 |
| loop CPU, median (ms) | **3,740** | 3,840 | 3,800 |
| resume load p50 (ms) | **7.1** | 21.9 | 231.4 |

Reading it plainly:

* **The training steps themselves are not slower.** Training rates are
  **96.22 / 93.75 / 94.17 steps/s** in A/B/C. WeftFS's pooled step p50 is
  10.018 ms versus 10.115/10.068 ms; the p95 is 10.58 vs 10.63/10.85 ms. The
  checkpoint *submit* path costs 3.41 ms p50 and 3.70 ms max, and the clone is
  0.59 ms of that.
* **The wall difference is the tail, not the loop.** The last checkpoint has no
  later training step to overlap with, so the run wall includes its completion.
  C's post-loop tail is 472.5 ms (final completion 459.0 ms) versus 6.5/17.7 ms
  for A/B; that is 466.1/454.8 ms of the 523.7/442.8 ms total wall difference.
  The WeftFS step sum (2,718.6 ms) is 2.2 % above A and 0.4 % below B, so there
  is no per-step throughput deficit on this run. A longer run amortizes the
  final tail.
* **WeftFS writes less.** 2.00 % fewer physical bytes per checkpoint than
  `torch.save` and 6.68 % fewer than local DCP. Per checkpoint the container is
  4,989,309 bytes (zstd shards) and each checkpoint adds 4,924,427 bytes of
  store data (manifest + objects + commit + advisory index).

### Where a WeftFS checkpoint's 444 ms goes

The adapter reports its own spans; the receipt keeps the raw numbers.

| span | value |
|---|---:|
| completion, p50 | 444.1 ms |
| adapter `checkpoint_write_ms`, p50 | 380.2 ms |
| service socket round trip (`request_ms`), p50 | 284.5 ms |
| completion − `checkpoint_write_ms` | 63.9 ms |

The 380.2 ms is the adapter's recorded checkpoint-write span (memfd handoff +
service round trip + manifest verification). The bare service round trip,
which contains the Rust candidate encode and the group commit, is 284.5 ms.
The remaining ~64 ms of completion is Python container staging and hashing,
the per-save checkpoint-service process start, and the second commit that
publishes `dcp.metadata`. There is **no CLI subprocess** in the save
(`store_command_calls = 0`).

Two further measured facts, from the receipt's `store_growth_probe`:

* A checkpoint in **a fresh store** completes in 210.5 ms
  (`checkpoint_write_ms` 186.8 ms). Three fresh stores saved the same container
  in 209.3/209.8/212.6 ms. The first checkpoint of the training run is
  213.5 ms.
* The **second and later checkpoints in the same store** take 383.8, 433.3 and
  455.8 ms for the same 4.99 MB container. The per-checkpoint deltas are
  constant (structured manifest ~4.59 MB, `objects/` ~0.16 MB, commit
  ~0.12 MB, advisory index segment 62 KB), so the growth is store-state
  bookkeeping, not more data. This court measures it and does not localise it
  further; it is the same class of behaviour as the per-commit full-state index
  documented in `bench/RESULTS-gc-accounting.md`.

So the honest comparison is: **the first save pays ~190 ms of encode/commit;
later saves pay ~360-420 ms; ~60 ms is Python-side staging, service start and
metadata; local DCP writes the same 4.99 MB in ~21 ms with no encode at all.**
The training thread only sees the 3.4 ms pause and background CPU contention.

## 3. Exactness: every checkpoint, in a fresh process

Every one of the 4 checkpoints of every one of the 15 normal runs (3
configurations × 5 repetitions) was restored by a **separate Python process**,
then compared against the in-process copy that was written:

| check | result |
|---|---|
| parameters, optimizer state, step counter (SHA-256 of every tensor) | **60/60** |
| next-step loss bits, computed from the restored state | **60/60** |
| state reached **after** that next optimizer step (SHA-256) | **45/45** |
| cross-configuration loss trajectory and final state | identical |

The 45 (not 60) post-step checks are the checkpoints that have a next step in
the same run; the last checkpoint of each run has no step 257 to compare. The
spec records both a no-grad probe loss at snapshot time and the actual
next-step loss, and both matched; the post-step state proves the optimizer step
itself is bit-identical, not just the load.

## 4. SIGKILL and resumption

The kill court runs the same WeftFS configuration in its own store with
checkpoints at 64 and 128. Each scenario is repeated 3 times, kills the
process with `SIGKILL`, discovers the newest complete checkpoint (store listing
is part of the resume path), and starts a fresh process that loads it and runs
one optimizer step.

| scenario | kill point | TTSR p50 / p95 (ms) | steps lost | exposed checkpoint |
|---|---|---:|---:|---|
| after ack | step 140, checkpoint 128 acknowledged | 1,385.1 / 1,386.0 | 12 | 128 (new), 3/3 |
| before ack, immediate | submit at step 128, kill immediately | 1,381.5 / 1,386.5 | 64 | 64 (old), 3/3; 128 absent 3/3 |
| before ack, half | submit at 128, kill after 225 ms | 1,366.5 / 1,371.6 | 64 | 64 (old), 3/3; 128 absent 3/3 |
| before ack, mid-flight | submit at 128, kill after 405 ms | 1,360.5 / 1,376.5 | 0 | 128 (new), 3/3 |
| before ack, after completion | submit at 128, kill after 518 ms | 1,379.4 / 1,398.6 | 0 | 128 (new), 3/3 |

`TTSR` = kill epoch → first completed optimizer step in the fresh process,
including store discovery. All 15/15 kill runs resumed from a **complete**
checkpoint, **never a partial one**; `checkpoint verify` and `verify --full`
were clean on all of them; every resumed state was exact and every next-step
loss matched.

The same table also says what "before the ack" means here: at 225 ms the
in-flight checkpoint had not committed, so only 64 existed; at 405 ms the
commit had landed but the training thread was killed before observing the
completion, so the store exposed 128 and the resume could use it. That is
old-or-new, not partial.

### Where 1.3-1.4 s of time-to-safe-resume goes

| component | value |
|---|---:|
| store discovery (list + newest complete checkpoint) | 26.9-61.9 ms |
| checkpoint load (manifest + exact restore) | 216.7-229.1 ms per scenario, 231.4 ms overall p50 |
| fresh process: `import weftcodec.dcp` (includes torch 2.15) | 889 ms |
| rest of the fresh process: model build, install, first step | 435 ms total process-start→first-step |

**The store's share of time-to-safe-resume is ~0.26-0.29 s; ~1.1 s is CPython
and torch import in a fresh process.** WeftFS's load is 10.6× local DCP's 21.9 ms
(exact manifest verification plus planning and decode); that difference is
~209 ms, i.e. ~15 % of the resume number. A long-lived worker that keeps torch
imported would remove the startup share; this court does not implement one and
does not claim it.

## 5. Staging bound

`staging_bytes` is a hard bound, not a hint. On the full state, a
1,024-byte bound is refused with `StagingLimitExceeded` (the real message
names the tensor: `model.blocks.0.fc1.bias needs 1536 staged bytes, above the
1024 staging_bytes bound`; DCP wraps it in its own `CheckpointException`). The
largest tensor is 147,456 bytes. The same state saves with the production
64 MiB bound, staging peak 147,456 bytes, 169 entries, nothing truncated.
This is in the receipt (`staging_bound`) and
`python/tests/test_goodput_harness.py::test_staging_bound_is_fail_closed`.

## 6. Losses

* Total loop wall is **1.20× `torch.save`** and **1.16× local DCP**. Almost all
  of that is the final in-flight checkpoint with no later step to overlap
  (466.1/454.8 ms of the 523.7/442.8 ms difference); the step sum is 2.2 % above
  A and 0.4 % below B, and pooled training-step p50 is not slower.
* The synchronisation pause is **3.41 ms**, against 0.68/0.74 ms for the
  baselines (4.6-4.9×). The absolute cost is small, but it is a real loss and
  it is almost entirely the `save_checkpoint_async` call (writer construction,
  store check, thread start); the clone is 0.59 ms.
* A checkpoint **completion** is 444.1 ms: **22.1× local DCP** and **115× the
  `torch.save` worker**. The save does not stall the loop, but the checkpoint
  interval must exceed the completion to keep it off the next checkpoint's
  path. With 64-step intervals this run fits; a shorter interval would wait.
* The **first** checkpoint in a store is ~210-214 ms; later checkpoints in the
  same store are ~384-456 ms for the same container (store-state bookkeeping).
  This is a real per-checkpoint growth, not noise (three fresh-store controls
  are flat).
* Checkpoint **load on resume** is 231 ms, **10.6× local DCP**.
* The worst single training step under WeftFS is **68.1 ms** versus 15.2/15.5 ms
  for the baselines. The submit path is not that stall (pause max 3.70 ms); it
  is host scheduling / background encode contention and is reported, not
  explained away.
* Any kill loses the steps after the last acked checkpoint: 12 steps after an
  acknowledged checkpoint, 64 when the kill lands before the next commit.
* Peak RSS is 2.5 % higher than `torch.save` and 2.2 % higher than DCP
  (379,272 KiB); loop CPU is 1.6 % higher than `torch.save` and 1.0 % lower
  than DCP.
* The harness's own exactness verification costs ~12 ms per run
  (three post-checkpoint state hashes) and is excluded from the step-time
  distributions; it is identical in all three configurations.

## 7. Non-claims

* Single CPU host inside the Docker python court; no GPU in the image and no
  GPU-direct, NCCL, or multi-rank/distributed training claim.
* Deterministic synthetic next-token data; a 0.4M-parameter transformer stack,
  not a production model or a public corpus.
* Kill tests are `SIGKILL` process faults against a local store, not physical
  media power loss or host-cache loss.
* The store is single-writer (MW1); no multi-writer training claim.
* `torch.save` and local DCP are single-host local files; no cloud or
  distributed checkpoint backend was exercised.
* A checkpoint interval shorter than the async completion would require the
  next checkpoint to wait for the previous save; the harness measures the wait
  but does not claim a production queueing/skip policy.
* Wall-clock medians are one host, warm page cache after the first repetition;
  no fleet, energy, or cost claim.
* The Python court image includes numpy for tensor views; it is present in all
  three configurations and its RSS/startup share is included equally. The
  harness's tensor hashing uses numpy when available and a pure-Python fallback
  otherwise; the receipt was measured with numpy present.
* Time-to-safe-resume includes fresh-process startup; the store share is the
  load and discovery, and the startup share is reported separately.

## 8. Reproduce

```sh
# Full published court (5 repetitions, 3 kill repetitions):
scripts/docker-court.sh python "bench/scripts/training_goodput_court.sh"

# Explicit configuration, one repetition, for a smoke run:
scripts/docker-court.sh python \
  "PYTHONPATH=/work/python WEFT_GOODPUT_REPS=1 WEFT_GOODPUT_KILL_REPS=1 \
   WEFT_GOODPUT_STEPS=64 WEFT_GOODPUT_CKPT_EVERY=32 \
   WEFT_GOODPUT_KILL_STEPS=64 WEFT_GOODPUT_KILL_AFTER_ACK_STEP=48 \
   WEFT_GOODPUT_KILL_BEFORE_ACK_STEP=32 \
   python3 python/courts/training_goodput.py run --receipt /tmp/tg.json"

# Harness helpers only (4 tests):
scripts/docker-court.sh python \
  "PYTHONPATH=/work/python python3 -m pytest -q python/tests/test_goodput_harness.py"
```

Raw receipt fields used above: `configs.*.summary`, `resume_exactness`,
`cross_config`, `kill_resume`, `staging_bound`, `store_growth_probe`,
`resume_startup_probe`, `losses`, `wins`, `method`. The log beside the receipt
contains every run and kill line, including the per-run loop wall, pause,
completion and byte counts printed by the harness.

## 9. Follow-up: the 444 ms decomposed (R12b)

`bench/RESULTS-checkpoint-latency.md` re-measures this fixture's checkpoint
half with the shipped phase profiler. The dominant cost is not the store, the
commit or the index: it is the planner's implicit mode-14 semantic search
(`tensor_semantic_candidates`), which scans every file object in the store and
decodes every same-dtype/same-shape tensor to price one candidate. It is
130.4 ms of a 211.1 ms fresh-store save and 344.5 ms of a 472.6 ms later save.
The rest of the growth is four store opens per save (a fresh writable handle
and a fresh read-only handle for both the checkpoint and the DCP metadata).

## 10. R12b increment 8 — the goodput court re-run with a fair baseline

Receipts `bench/receipts/training-goodput-20261111T044800Z.json` (5
configurations, 2 reps, 128 steps, checkpoint every 32, 1 kill rep per
scenario) and `bench/receipts/training-goodput-20261111T045900Z`/
`…T050200Z.json` (1 rep, 64 steps, **checkpoint every 4** — an interval shorter
than a save completion). Both are **reduced runs** (the published R11 court was
5 reps / 256 steps); the reduction is in the receipt's `reps`/`steps` fields.

Two configurations were added:

* **D, `torchsave-durable`** — the equivalent-durability baseline:
  `torch.save` to a sibling temp file, `fsync`, `os.replace`, `fsync` of the
  directory. The guarantees matched to a WeftFS commit are exactly: (i) the
  bytes are on stable storage before the save returns, and (ii) the destination
  is either the previous complete artifact or the new one, never a partial
  file. It does **not** claim WeftFS's content addressing, dedup, manifests,
  indexes, snapshots or provenance — it is the cheapest local baseline that
  makes those two durability promises.
* **E, `weft-fast`** — the WeftFS performance floor with the planner's
  optional work removed: `write_policy=fast`, `fast_codec=raw`, no semantic
  selection, no mode-9/10/15 candidates, no deltas.

### Standard interval (every 32 steps), 2 reps

| metric | A torch.save | D durable | B local DCP | C WeftFS | E WeftFS fast |
|---|---:|---:|---:|---:|---:|
| loop wall, median (ms) | **1,411.5** | 1,414.8 | 1,528.1 | 1,587.8 | 1,604.2 |
| process wall (ms) | 1,607.4 | 1,608.9 | 1,719.0 | 1,775.2 | 1,795.2 |
| step p50, pooled (ms) | **10.38** | 10.46 | 10.40 | **10.27** | 10.40 |
| pause p50, pooled (ms) | 0.79 | 0.73 | 0.88 | 3.50 | 3.28 |
| completion p50 (ms) | **3.84** | **4.20** | 21.09 | 75.57 | 72.98 |
| completion first / later (ms) | 4.49 / 3.74 | 4.85 / 3.98 | 20.80 / 21.37 | 122.57 / 71.84 | 78.81 / 73.24 |
| physical bytes per checkpoint | 5,025,099 | 5,024,237 | 5,277,072 | **4,925,012** | 5,349,420 |
| peak RSS (KiB) | **325,476** | 325,680 | 326,052 | 331,044 | 330,428 |
| resume load p50 (ms) | **7.23** | 7.23 | 22.17 | 256.71 | 256.69 |

* **The durability promise costs almost nothing locally.** D is 0.36 ms slower
  than plain `torch.save` (4.20 vs 3.84 ms) and writes the same bytes. That is
  the honest comparison for a checkpoint that must survive a power cut, and it
  makes WeftFS's **71.8-75.6 ms completion 17-18x D**, not 20x the
  non-durable A.
* **The performance floor is not faster.** E (`fast` + raw) completes in 73.0
  ms against C's 75.6 ms and writes 8.6 % *more* bytes. On this async court the
  optional planner work was already a few ms; the remaining cost is the service
  round trip, the chunking/manifest publication and the commit, not the
  optional semantic services.
* Training steps are not slower (C's pooled p50 is the lowest of the five).
  The wall difference is still the final in-flight checkpoint.
* Exactness: **40/40** checkpoints restored by a fresh process with matching
  tensor SHA-256, **40/40** next-step loss bits, **30/30** post-step states,
  and all 2-rep trajectories/states identical across the five configurations.

### Interval shorter than completion (every 4 steps), 1 rep

| metric | A | D | B | C | E |
|---|---:|---:|---:|---:|---:|
| loop wall (ms) | **951** | 939 | 1,188 | **2,490** | 2,552 |
| completion p50 (ms) | 3.62 | 4.30 | 20.57 | 130.23 | 132.40 |
| **waited for previous save, max (ms)** | 0.0 | 0.0 | 0.0 | **149.9** | **161.8** |
| step the save lands at (p50, +steps) | 1 | 1 | 1 | **4** | **4** |
| exactness | 16/16 | 16/16 | 16/16 | 16/16 | 16/16 |

This is the regression the R11 text predicted and did not measure: at a 40 ms
checkpoint interval the next save has to wait **149.9 ms** for the previous
one, the loop wall is **2.6x** `torch.save` and **2.1x** the durable baseline,
and the save lands four steps late. `weft-fast` does not help (161.8 ms): its
frames are larger, and the wait is dominated by the round trip and commit, not
by encoding. No variant is acknowledged before its representation is durable,
and all 80 checkpoints of the short-interval run restored exactly.
