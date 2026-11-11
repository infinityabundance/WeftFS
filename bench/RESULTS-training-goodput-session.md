# Training goodput with a persistent checkpoint session (R12c subphase B)

Receipt: `bench/receipts/training-goodput-session-20261111T220500Z.json`
(raw court log beside it). Measured at WeftFS
`76f098134085d7fee331a61eba569923256b12eb`, WeftCodec
`d9234e5322686afbbbe6fcca103da853928811aa`, WeftCodec-Go
`fe05240558a94abe530e3020c25b64ab2ad23a32`, WeftFS-Go
`146b5178d156e0629d4b031f5beead326e43fe25`. Those revisions are recorded by
the court in the receipt, not retyped.

Two earlier runs of the same fixture are retained and are **not** the current
number:

| receipt | revision | difference |
|---|---|---|
| `training-goodput-session-20261111T210500Z.json` | `7d5f5eb` | kill scenarios were `after_ack` + `before_ack_midflight`; no per-save write-time attribution |
| `training-goodput-session-20261111T213500Z.json` | `7d0d19c` | kill scenarios `after_ack`/`before_ack_half`/`after_completion`; no step/pause/cold loss rules in the receipt |
| `training-goodput-session-20261111T220500Z.json` | `76f0981` | **current**: adds the step, pause, cold, write-attribution and fsync-equality claims |

The three runs agree on the ordering and the shape of every cell. They are one
host, so they are not a stability claim either.

## 1. Court, fixture and commands

Court: `weft/python-court:1`
(`sha256:aec5deb084693cdbe465b2bfc0b1c897d497db84282a131d31750cde48576152`),
Linux `7.3.3-1-cachyos`, Docker server `29.9.3`, **3 GiB memory / 3 GiB swap, 4
CPUs, 1024 pids, 1 GiB `/tmp`, `cap-drop=ALL`, `no-new-privileges`.** Python
3.12.16, `torch 2.15.0+cpu` (official CPU wheel; no CUDA runtime, no GPU in the
image).

Fixture: the R11/R12b training-goodput fixture **by construction** — the court
imports `build_model`/`make_optimizer`/`capture_state` from
`python/courts/training_goodput.py`, so the checkpoint is the same **169
entries / 4,989,309-byte savetensors container** the earlier courts used. The
model is a 414,432-parameter transformer stack trained with AdamW on seeded
synthetic next-token batches. The receipt records the observed container size
and entry count per cell.

Commands (each in its own container; the release CLI is built first so the
receipt names one tree):

```sh
scripts/docker-court.sh rust "cargo build --release --features fuse --bin weftfs"

WEFT_GOODPUT_STAMP=20261111T220500Z \
WEFT_GOODPUT_SWEEP_STEPS=192 WEFT_GOODPUT_SWEEP_REPS=3 \
WEFT_GOODPUT_INTERVALS=4,8,16,32 WEFT_GOODPUT_RESUME=final \
WEFT_GOODPUT_KILL_REPS=3 \
  scripts/docker-court.sh python "bench/scripts/training_goodput_session_court.sh"
```

What each configuration is:

* **`weft`** — the shipped path the R11 court measured: one
  `save_checkpoint_async(state, ...)` per checkpoint, so every save constructs
  a `WeftStorageWriter`, starts a checkpoint service, opens the store, and
  closes both (`python/courts/training_goodput.py:550` before this change).
* **`weft-session`** — **new and opt-in**: one `CheckpointSession` per
  (store, run) created before step 1, and every checkpoint submitted through
  `session.save(...)` on the single save worker. Nothing else differs:
  `topology=1`, `shard_strategy="zstd"` (planned policy), `staging_bytes=64
  MiB`, one worker, the same snapshot clone, the same 192 steps and batches.

The default `run` court is unchanged: `CONFIGS` still holds the five R12b
configurations and a plain `bench/scripts/training_goodput_court.sh` performs
the same work byte-for-byte. A reduced default run (5 configurations, 5 kill
scenarios) was executed to confirm that.

## 2. Method, and what "cold" and "warm" mean

* **cold** = the first completed checkpoint of each run. For `weft-session`
  this includes the helper start, the store open and the first writable handle
  upgrade; for `weft` it is just the first save. `n = 3` (one per repetition),
  so its p50 is the median of three values and its p95/p99 are not meaningful —
  they are printed anyway rather than hidden.
* **warm** = every later completed checkpoint in the same run. `n = 141 / 69 /
  33 / 15` at 4/8/16/32 steps between checkpoints (3 repetitions). The p99 at
  32 steps is therefore over 15 samples: treat it as "worst warm sample".
* `completion_ms` = submit to the save worker's completion callback.
  `pause_ms` = the state clone plus submit on the training thread.
  `step_ms` = the training step, pooled over the clean steps (checkpoint steps
  and post-checkpoint harness-verification steps excluded).
* Warm samples in **both** arms include store growth: a later commit in the
  same store climbs the index chain. That confound is identical in both arms,
  so the comparison is fair, but neither arm is a fresh-store number.
* **Resume exactness scope:** every checkpoint of the first repetition of every
  cell was restored by a separate process, and only the final checkpoint of the
  second and third repetitions (`--resume final`, recorded in the receipt).
  196/196 state comparisons, 196/196 next-step loss bits, 172/172 post-step
  states (the post-step check needs a following step, so the last checkpoint of
  a run has none).

## 3. Measured table

`weft` = per-save service, `S` = reusable session. Times in ms.

| steps/ckpt | config | cold p50 | warm p50 | warm p95 | warm p99 | loop wall | step p50 | step p95 | pause p50 | peak RSS KiB | loop CPU ms | bytes/ckpt |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 4 | `weft` | 50.7 | **339.0** | 643.5 | 684.6 | **17,615.8** | **10.61** | **12.18** | 3.99 | 342,680 | 5,420 | 4,923,753 |
| 4 | `S` | 101.6 | **62.9** | 88.9 | 143.3 | **4,476.3** | 11.99 | 17.72 | **0.80** | 333,188 | 5,380 | 4,923,713 |
| 8 | `weft` | 106.8 | 173.6 | 315.0 | 317.2 | 4,983.3 | 10.42 | 15.61 | 3.73 | 341,524 | 4,110 | 4,923,328 |
| 8 | `S` | 99.4 | **50.6** | 61.6 | 117.3 | **2,960.7** | 10.48 | 16.80 | **0.69** | 333,464 | 4,110 | 4,923,288 |
| 16 | `weft` | 48.3 | 119.9 | 162.0 | 171.0 | 2,580.3 | 10.42 | 15.80 | 3.46 | 337,600 | 3,560 | 4,923,328 |
| 16 | `S` | 101.5 | **47.7** | 55.1 | 111.8 | **2,501.2** | 10.41 | 16.23 | **0.71** | 332,968 | 3,680 | 4,923,288 |
| 32 | `weft` | 49.2 | 94.7 | 130.0 | 142.7 | **2,329.3** | 10.40 | 12.11 | 3.59 | 334,560 | 3,360 | 4,924,051 |
| 32 | `S` | 137.1 | **46.3** | 48.8 | 50.9 | 2,429.9 | 10.59 | 12.78 | **0.76** | 332,828 | 3,580 | 4,924,011 |

Ratios (session ÷ per-save): warm p50 **0.185 / 0.291 / 0.398 / 0.488**;
warm p95 0.138 / 0.196 / 0.340 / 0.376; loop wall 0.254 / 0.594 / 0.969 /
1.043; step p50 1.130 / 1.005 / 0.999 / 1.018; cold p50 2.003 / 0.930 / 2.102 /
2.787.

Where the difference comes from — the store's own server-side publish time
(`checkpoint_write_ms`, p50 per save) and its durability counters:

| steps/ckpt | `weft` write p50 | `S` write p50 | fsyncs p50 | writes p50 | waited for previous (max) |
|---:|---:|---:|---:|---:|---|
| 4 | 115.5 | 28.0 | 528 / 528 | 265 / 265 | 615.8 / 30.8 |
| 8 | 65.0 | 26.2 | 528 / 528 | 265 / 265 | 226.1 / 0.0 |
| 16 | 45.6 | 26.0 | 528 / 528 | 265 / 265 | 0.0 / 0.0 |
| 32 | 36.5 | 26.6 | 528 / 528 | 265 / 265 | 0.0 / 0.0 |

`weft` also cannot keep up with the 4-step interval: one save needs 339 ms
against a ~40 ms interval, so the loop waits **615.8 ms** at the next
checkpoint. The session's save at the same interval is 62.9 ms and waits
**30.8 ms**.

Session counters in the receipt are one helper for the whole run
(`services_started = 1`, `saves = 48 / 24 / 12 / 6`). The session's
`writable_handle_opens` field is **not** incremented by the shipped adapter and
is not reported; making the handle lifecycle observable is
[`docs/PLAN-R12C.md`](PLAN-R12C.md) item 6.

## 4. What the numbers mean (and what they do not)

**The win is the reusable handle, not a faster encoder.** The server-side
publish p50 falls from 115.5 → 28.0 ms at the 4-step interval and from 36.5 →
26.6 ms at 32 steps. The per-save arm opens the store and replays the commit
chain on every save, and that cost grows with the commits already in the store
(the same growth `store_growth_probe` reports in the R11 court). The session
pays it once. The bytes written per checkpoint are unchanged (4.92 MB), and
every save in both arms reports the **same 528 backend fsyncs and 265 writes**,
so nothing about durability was relaxed.

**The pause also falls**, 3.99 → 0.80 ms p50 at the short interval: with a
session the submit hands the snapshot to a worker and the worker builds the
writer; the per-save path builds the writer (and touches the store) on the
training thread first.

**At a 32-step interval the session no longer wins the loop.** The save is
46.3 vs 94.7 ms, but the interval is 320 ms, so neither arm stalls the loop and
the measured wall is 2,429.9 vs 2,329.3 ms (+4.3 %, inside the noise of a single
host but published as a loss because the sign is not ours to choose). Use the
session for the short-interval regime; it is not a free win at long intervals.

**At a 4-step interval the clean step is 13 % slower** (11.99 vs 10.61 ms p50,
and step p95 17.72 vs 12.18). The work is the same; the session stops stalling
the loop, so more of the encode overlaps training steps on one host. That is
the trade: a 3.9x shorter loop wall and a 20x smaller tail wait, paid for with
a ~1.4 ms higher clean-step median. It is a real loss and is in the receipt.

**Cold is worse, as expected**: 101.6/99.4/101.5/137.1 ms vs
48.3–106.8 ms. The first save starts the helper and upgrades to a writable
handle. With `n = 3` per cell these cold p50s are noisy (the 137.1 ms at 32
steps is one slow start), which is why they are reported separately and not
averaged into the warm number.

**Not measured here:** queueing. The loop waits for the previous save; it never
overlaps two saves and never drops one. A bounded persistent worker queue is
[`docs/PLAN-R12C.md`](PLAN-R12C.md) item 4.

## 5. Correctness evidence

All 196 resume comparisons were exact and the loss trajectory and final state
were identical between the two arms in every interval
(`identical_loss_trajectory` and `identical_final_state` true at 4/8/16/32).

SIGKILL through a session (`session_kill_resume` in the receipt, 3 repetitions
per scenario, `checkpoint_every=16`):

| scenario (kill point) | resumed from | old / new | steps lost (median) | exact | `verify --full` |
|---|---|---:|---:|---|---|
| `after_ack` (after completion) | `ckpt-0048` | 0 / 3 | 0 | 3/3 | clean 3/3 |
| `before_ack_half` (0.5 × warm p50) | `ckpt-0016` | **3 / 0** | 16 | 3/3 | clean 3/3 |
| `before_ack_after_completion` (1.15 ×) | `ckpt-0032` | 0 / 3 | 0 | 3/3 | clean 3/3 |

So the court exercises both branches of the recovery set: a kill before the
commit resumes from the previous checkpoint and loses exactly one interval; a
kill at or after it exposes the new checkpoint whole.

Deterministic pre-publication coverage is in `python/tests/test_dcp.py` (5
session tests, all passing in the python court):

* `test_session_reuses_one_service_and_restores_exactly` (existing) — one
  helper, two saves, both restored exactly.
* `test_session_is_not_usable_in_a_forked_child` (existing).
* `test_session_keeps_the_writer_lock_until_it_is_closed` — with the warm
  session holding the handle, an independent `weftfs create` is refused with
  `Busy`; the three checkpoints published through the one helper are complete
  and verify clean; after `close()` the same command succeeds.
* `test_session_killed_during_a_save_leaves_no_partial_checkpoint` — SIGKILL a
  child after it signals ready, at 1/10/40 ms into a save: every exposed
  checkpoint is complete, verifies, and restores the exact source bytes.
* `test_session_save_aborted_at_each_publication_phase_leaves_old_or_new` —
  the store's own `WEFT_CRASH_PHASE` injection through a session:
  `after_objects` and `after_commit_file` expose the previous generation and no
  new checkpoint; `after_superblock_write` exposes the new one whole.

That last test recorded a real observation worth stating: a crash **between the
commit-file fsync and the superblock write** leaves the old, complete state
visible, but `verify --full` reports `Corrupt: commit file for generation N
exists above the highest published superblock generation N-1`. Readers are
correct and the state is old-and-complete; the orphan commit file is reported
rather than hidden, and the test asserts that this is the *only* problem in that
phase. Nothing was changed to make that report disappear.

## 6. Losses (all of them)

1. Asynchronous save still costs ~63 ms at the short interval and ~46 ms at the
   long one, versus ~4 ms for `torch.save` in the R11 court. This work does not
   close that gap; it removes the per-save store open and replay only.
2. At a 4-step interval the clean training-step p50 is 13 % higher and step p95
   45 % higher (17.72 vs 12.18 ms), because more save work overlaps training on
   one host.
3. The first checkpoint of a session run is ~2x the per-save first checkpoint
   (helper start + first writable handle open).
4. At a 32-step interval the total loop wall is 4.3 % higher with the session.
5. The session holds a writable store handle between checkpoints, so for up to
   `session_idle_secs` after a save no other process can open the store for
   writing. This is the reason it stays opt-in. The idle release is still
   driven by the next client request, not by a timer (plan item 6).
6. Warm numbers include store growth in both arms; neither arm is a
   fresh-store measurement.
7. `--resume final` verifies every checkpoint of the first repetition only; the
   second and third repetitions verify their final checkpoint.
8. The kill court always loses the steps after the last acked checkpoint; the
   measured loss is 0 or 16 steps depending on the kill point, never zero for
   the pre-commit branch.
9. The session's helper counters are cumulative; per-save request counts are
   only available for the per-save arm, and the session's
   `writable_handle_opens` field is not maintained by the adapter.

## 7. Not claimed

* Single CPU host inside the Docker python court; no GPU, GPU-direct, NCCL,
  multi-rank, or fleet claim.
* Synthetic seeded data and a 0.4M-parameter model, not a public corpus or a
  production model.
* No queueing policy, no admission control, no automatic interval selection.
* Kill tests are SIGKILL process faults and the store's `WEFT_CRASH_PHASE`
  injection against a local store — not physical media power loss, not host
  page-cache loss.
* The store is single-writer (MW1). A warm session keeps that writer lock; no
  multi-writer training claim.
* One host and warm page cache after the first repetition; no stability,
  energy, or scaling claim. The three retained runs are agreement, not a
  distribution.
