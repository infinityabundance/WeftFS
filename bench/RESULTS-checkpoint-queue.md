# Checkpoint queue: bounded admission, measured backpressure (R12c item 4)

Receipts (all under `bench/receipts/`, raw log beside each):

| section | phase | receipt | measured at |
|---|---|---|---|
| `sweep` | baseline | `checkpoint-queue-sweep-baseline-20261112T020000Z.json` | WeftFS `de210a6`, library `d7c27ec` |
| `overload` | baseline | `checkpoint-queue-overload-baseline-20261112T020000Z.json` | WeftFS `de210a6`, library `d7c27ec` |
| `sweep` (disk variant) | baseline | `checkpoint-queue-sweep-baseline-20261112T012500Z.json` | WeftFS `de210a6`, library `d7c27ec` |

Every receipt records the four repository revisions, the court image id, kernel,
Python version, the exact `argv`, and the fixture as *built*, not as asserted.

The two tooling commits (`0c4b103`, `04fae6a`, `c8fafab`, `de210a6`) changed only
`python/courts/`, `bench/scripts/` and `scripts/docker-court.sh`;
`git diff d7c27ec..de210a6 -- python/weftcodec` is **empty**, so the baseline is
the shipped adapter with the queue court attached. That is checkable, not
asserted.

Retained failures:

| file | what it is |
|---|---|
| `checkpoint-queue-sweep-baseline-20261112T000000Z.tmpfs-full.log` | first 192-step attempt at interval 1: the court's bounded 1 GiB `/tmp` filled (`No space left on device`); the court now keeps its scratch store on the bind-mounted work tree and records `df` |
| `checkpoint-queue-sweep-baseline-20261112T001500Z.log` | second attempt: the shared sweep's progress line formatted an empty "clean step" sample as a float and raised `TypeError` at interval 1 (every step takes a checkpoint); fixed in `de210a6` |

## 1. Court, fixture, commands

Court: `weft/python-court:1`
(`sha256:aec5deb084693cdbe465b2bfc0b1c897d497db84282a131d31750cde48576152`),
3 GiB memory / 3 GiB swap, 4 CPUs, 1024 pids, 1 GiB `/tmp`, `cap-drop=ALL`,
`no-new-privileges`. Python 3.12.16, `torch 2.15.0+cpu` (no CUDA runtime, no GPU).

Fixture: the R11/R12b training-goodput model **by construction** — the court
imports `build_model`/`make_optimizer`/`capture_state`/`train_step` from
`python/courts/training_goodput.py`, so every checkpoint is the same **169-entry,
~4.99 MB** safetensors container (4,923,713 B of store per checkpoint in the
table below). The receipt records the built entry count, logical bytes and
largest tensor per run.

```sh
scripts/docker-court.sh rust "cargo build --release --features fuse --bin weftfs"

# primary interval sweep (scratch store on the court's tmpfs)
WEFT_QUEUE_SECTION=sweep WEFT_QUEUE_PHASE=baseline \
WEFT_QUEUE_INTERVALS=1,2,4,8,16,32 WEFT_QUEUE_CONFIGS=weft,weft-session \
WEFT_QUEUE_STEPS=96 WEFT_QUEUE_REPS=2 WEFT_QUEUE_WORK=/tmp/queue-work \
  scripts/docker-court.sh python "bench/scripts/checkpoint_queue_court.sh"

# concurrent-submission probe
WEFT_QUEUE_SECTION=overload WEFT_QUEUE_PHASE=baseline WEFT_QUEUE_COUNTS=4,8 \
  scripts/docker-court.sh python "bench/scripts/checkpoint_queue_court.sh"
```

`--interval 1` is deliberately shorter than a save: at a ~10 ms training step a
1-step interval is ~10 ms against a ~39 ms warm save, so the cell *must* either
block, refuse, or drop. Every cell restores its checkpoints in a **separate
process** and compares the flat tensor bytes, the next-step loss bits and the
post-step state.

## 2. What overload did before the change (measured, and from source)

Two different callers, two different behaviours:

**A training loop that waits for the previous save (what the harness does)** —
it **blocks**. Source: `python/courts/training_goodput.py:child_loop` finalises
`inflight` at the next checkpoint step and records `waited_for_previous_ms`; the
loop never has two saves in flight. Measurement (primary sweep): the maximum
wait for the previous save is **1,688.2 ms** at a 1-step interval (`weft`) and
**82.3 ms** (`weft-session`); at interval 2 it is 598.5 / 73.4 ms; interval 4
233.4 / 0.0 ms. Nothing is lost — the loop simply runs slower.

**A caller that submits without waiting** — it **raises**, and *which* save fails
is decided by a race. Source: `CheckpointService._call` serialises requests but
the `prepare`→`finish` window is not atomic, and the helper holds exactly one
deferred checkpoint; `src/checkpoint_service.rs:handle_prepare` refuses a second
one with `InvalidArgument`. Measurement (`checkpoint-queue-overload-baseline-20261112T020000Z.json`,
4 then 8 caller threads against one store, all outcomes counted):

| mode | concurrent submissions | landed | raised | inner class + message | store checkpoints | threads (before → peak) |
|---|---:|---:|---:|---|---:|---|
| shipped async API (`save_checkpoint_async`, one executor per call) | 4 | 2 | 2 | `ServiceError: InvalidArgument: a deferred checkpoint is already open on this connection` | 2 | 1 → 10 |
| session `save` from 4 threads | 4 | 2 | 2 | same | 2 | 2 → 7 |
| shipped async API | 8 | 2 | 6 | same | 2 | 2 → 19 |
| session `save` from 8 threads | 8 | 2 | 6 | same | 2 | 2 → 11 |

So the pre-item-4 answer is: **block if you wait, raise if you don't, never
silently lose**. Two things are still wrong, and they are what item 4 fixes:

1. the refusal is a *race*, not a documented bound — the caller cannot know how
   many saves may be outstanding, and the count that survives depends on timing
   (2 of 4, 2 of 8 here);
2. nothing bounds the number of submitters or the work they queue:
   `save_checkpoint_async` creates a thread pool and a writer **per call**, so 8
   concurrent submissions took the process from 2 to 19 threads with no limit,
   and each of them would have published an unbounded amount of work had the
   helper accepted them.

`landed == store checkpoints` in every run (`landed_equals_store: true`) and
`verify --full` is clean: a refused save published **nothing**, and no run left a
partial checkpoint.

## 3. Baseline interval sweep (primary: tmpfs scratch store, 96 steps, 2 reps)

`weft` = one service per save (shipped), `S` = reusable `CheckpointSession`
(R12b, item 1–3). ms; `wall` = loop wall; `step p50/p95` are **all** steps
(at interval 1 every step takes a checkpoint, so the harness's clean-step view is
empty and is recorded separately in the receipt).

| steps/ckpt | config | cold p50 | warm p50 | warm p95 | warm p99 | wait max | wall | step p50 | step p95 | pause p50 | peak RSS KiB | loop CPU ms |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | `weft` | 107.0 | 675.7 | 1,582.2 | 1,694.0 | **1,688.2** | **74,370.2** | 681.2 | 1,616.1 | 4.00 | 342,768 | 7,215 |
| 1 | `S` | 101.4 | **42.5** | 47.7 | 97.5 | **82.3** | **7,415.3** | 75.0 | 103.6 | **0.85** | 332,984 | 6,930 |
| 2 | `weft` | 103.3 | 303.6 | 608.2 | 641.2 | 598.5 | 16,289.6 | 20.2 | 569.0 | 3.80 | 344,232 | 3,980 |
| 2 | `S` | 101.5 | **39.8** | 43.5 | 92.9 | 73.4 | **3,037.7** | 19.7 | 56.0 | **0.66** | 333,392 | 3,900 |
| 4 | `weft` | 143.9 | 169.8 | 281.3 | 294.4 | 233.4 | 4,560.6 | 14.5 | 206.6 | 3.85 | 340,720 | 3,025 |
| 4 | `S` | 101.2 | **39.2** | 42.2 | 43.1 | **0.0** | **1,723.9** | 14.2 | 22.8 | **0.60** | 333,912 | 2,860 |
| 8 | `weft` | 103.9 | 99.3 | 145.3 | 145.7 | 49.1 | 1,595.2 | 10.2 | 35.6 | 3.25 | 336,812 | 2,410 |
| 8 | `S` | 97.1 | **38.8** | 40.8 | 41.7 | **0.0** | **1,338.6** | 10.1 | 22.2 | **0.59** | 334,356 | 2,410 |
| 16 | `weft` | 102.9 | 76.0 | 91.9 | 91.9 | 0.0 | 1,249.2 | 10.0 | 22.3 | 3.46 | 335,596 | 2,245 |
| 16 | `S` | 99.0 | **38.0** | 39.1 | 39.1 | 0.0 | **1,196.3** | 10.0 | 22.1 | **0.63** | 334,580 | 2,245 |
| 32 | `weft` | 104.9 | 61.7 | 65.8 | 65.8 | 0.0 | 1,146.2 | 9.93 | 15.3 | 1.67 | 333,124 | 2,155 |
| 32 | `S` | 99.1 | **39.0** | 40.6 | 40.6 | 0.0 | **1,114.2** | 9.88 | 15.6 | **0.65** | 333,236 | 2,175 |

Exactness: every cell restored and compared exactly — 97/97, 49/49, 25/25,
13/13, 7/7 and 4/4 checkpoints per cell (rep 0 checks every checkpoint, rep 1 its
last), with `first_step_loss_match`, `probe_loss_match` and
`post_step_state_match` all true. Store bytes per checkpoint 4,925,338–4,925,849
(`bytes/ckpt` in the receipt).

**The "4-step cliff" in the baseline is a serialisation cliff, not a queueing
one.** At 4 steps the save (39.2 ms) is just under the interval (~40 ms), so the
wait for the previous save drops to 0 and the wall falls 6.3x versus interval 1.
Above 4 steps the loop is never blocked; below it, the loop pays the difference
between the save and the interval on every checkpoint. A queue cannot make a
save finish sooner — it can only let the loop run *ahead* by a bounded number of
saves before it must wait.

## 4. Disk-backed variant (retained, not the primary table)

Same fixture and intervals with the scratch store on the bind-mounted work tree
and 192 steps (`checkpoint-queue-sweep-baseline-20261112T012500Z.json`). An fsync
on that filesystem is far slower than on the court's tmpfs, so the same save takes
**675–680 ms**:

| steps/ckpt | config | warm p50 | warm p95 | wait max | wall |
|---:|---|---:|---:|---:|---:|
| 1 | `weft` | 2,660.0 | 8,375.0 | 18,399.1 | 706,507.3 |
| 1 | `S` | 681.2 | 10,437.8 | 14,488.0 | 296,514.6 |
| 2 | `S` | 679.3 | 7,719.4 | 13,940.3 | 132,576.1 |
| 4 | `S` | 674.3 | 11,340.2 | 12,786.0 | 77,451.8 |
| 8 | `S` | 673.3 | 12,070.3 | 13,271.6 | 47,910.1 |
| 16 | `S` | 676.1 | 739.8 | 767.9 | 8,852.6 |
| 32 | `S` | 686.4 | 739.2 | 593.1 | 4,843.5 |

Every checkpoint in this variant was restored exactly too (193/193 … 7/7). It is
retained because it is the regime in which a 4-step interval is *far* shorter
than a save (160 ms against 675 ms), and because the interval-8 `weft-session`
cell (47.9 s wall, 12.1 s p95) shows how noisy a nearly-full host filesystem
makes these numbers. It is not used for the before/after delta: the delta uses
one medium on both sides.

## 5. What the baseline says about item 4

* There is no queue today. "Async" means *one* save at a time behind a
  wait-for-previous barrier in the harness, or an unbounded number of racing
  threads outside it.
* The answer to the reviewer's question — does a short-interval run block, raise
  or lose? — is **block if the caller waits, raise if it does not, never silently
  lose**, and in both cases the number that survives is decided by timing rather
  than by a stated bound.
* Measured floor to beat: at a 1-step interval the loop cannot be faster than
  (checkpoints × save time) unless saves are dropped. The baseline `S` wall of
  7,415.3 ms for 96 checkpoints is 77.2 ms per checkpoint against a 42.5 ms warm
  save, i.e. there is 34.7 ms/checkpoint of avoidable stall if the loop could run
  ahead — bounded by the queue, never by dropping saves.
