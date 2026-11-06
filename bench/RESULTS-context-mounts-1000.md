# 1,000 concurrent context mounts — measured, with the break point

Measured 2026-11-05 in the OOM-protected FUSE court at WeftFS `6e4aea1`.
Superseded measurements are not cited here; every number below comes from a
receipt in `bench/receipts/`.

## What was measured

`scripts/demo-context-mounts.sh` scaled to `WEFT_CONTEXT_MOUNTS=1000` builds one
store with:

| item | value |
|---|---|
| corpus files | 2,000 (`/corpus/f-000.bin` … `f-1999.bin`) |
| model objects | 250 |
| grounding documents | 10 |
| typed relations | 5,999 (`generated_by` 2,000, `grounded_in` 2,000, `derived_from` 1,999) |
| Context Roots | 1,000 (500 `by:task=agent-N`, 250 `by:model=llama-4@ver_N`, 250 `by:derived=f-NNN`) |
| store objects / relations as counted by `weftfs info` | 2,264 / 6,999 |
| one snapshot (`all`), one commit chain | yes |

All 1,000 daemons are spawned before any is awaited, alternating Rust and Go.
For every mount the court verifies:

* the listing digest against the **construction oracle** (the corpus manifest
  emitted by the build: 2,000 listed names, each `basename--oid8`), and all
  1,000 digests must be distinct;
* **every** listed file's SHA-256 against the declared content hash;
* one `weftfs cat` byte comparison per mount;
* `context resolve` on every 50th mount (20 of 1,000), compared id-for-id with
  the construction oracle;
* read-only denial on the first Rust and Go mounts;
* clean unmount of every mount and zero surviving daemons.

The 100-mount mode keeps its original one-commit-per-operation CLI build and its
per-mount `context resolve`. Above 100 the corpus is built by
`examples/context_mount_corpus.rs` through the shipped store API with group
commits (256 operations per publish, generation 43 for the 1,000-mount store),
because one commit per operation makes every daemon replay ~9,260 commits for
its pinned snapshot and commit-chain replay would dominate the measurement.
The harness corpus is byte-identical to the CLI fixture: same seeds, same object
ids (`01e4b377…` for `/corpus/f-000.bin`), same content hashes
(`d0ce9f7b…`), same counts and the same selector results.

## Environment

* Court image `weft/fuse-court:1`, image id
  `sha256:e74926322e19ab08833ddce3db2086d8b6be3434880ba969dd879266d7f7cba8`
* Kernel `Linux 7.3.1-1-cachyos x86_64`; host `nproc` = 16
* Hard limits are always applied by `scripts/docker-court.sh`; the scale court
  raises them explicitly and the receipt records the exact strings:
  * `WEFT_MEMORY=52g WEFT_CPUS=8 WEFT_PIDS=16384 WEFT_NPROC=16384` for the
    1,000-mount runs,
  * `WEFT_MEMORY=24g WEFT_CPUS=8 WEFT_PIDS=4096 WEFT_NPROC=4096` for the
    ceiling search.
* No limit is disabled: memory == swap, pids, RLIMIT_NPROC, CPUs, bounded
  `/tmp`, `no-new-privileges`, `cap-drop=ALL` remain in force.

## Results

| receipt | limits (mem/cpu/pids) | N requested | spawned | mounted | verified | files SHA-256 | distinct digests | clean unmounts | survivors | peak cgroup | result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `context-mounts-20261105T122643Z.json` | 52g/8/16384 | 1000 | 1000 | 1000 | 1000 | 4250 | 1000 | 1000 | 0 | 40.92 GiB | **5/5 pass** |
| `context-mounts-20261105T131116Z.json` | 52g/8/16384 | 1000 | 1000 | 1000 | 1000 | 4250 | 1000 | 1000 | 0 | 40.22 GiB | **5/5 pass**, `semantic_match=true` |
| `context-mounts-20261105T135805Z.json` | 24g/8/4096 | 400 | 400 | 400 | 400 | 1700 | 400 | 400 | 0 | 11.72 GiB | 5/5 pass |
| `context-mounts-20261105T145819Z.json` | 24g/8/4096 | 500 | 500 | 500 | 500 | 2125 | 496 | 496 | 0 | 16.32 GiB | 3/5 (4 Go daemons died mid-run) |
| `context-mounts-20261105T140342Z.json` | 24g/8/4096 | 1000 | 747 | 692 | 563 | 2896 | 523 | 631 | 0 | 24.00 GiB (limit) | 2/5, **INCOMPLETE 692/1000** |
| `context-mounts-20261105T150653Z.json` | 3g/4/1024 (default court) | 100 | 100 | 100 | 100 | 425 | 100 | 100 | 0 | 1.74 GiB | 5/5 pass (100 mode unchanged) |

Latency and memory (per run):

| receipt | p50 mount | p95 mount | max mount | all mounted+verified | run wall | daemon RSS sum | max RSS | threads sum/max |
|---|---|---|---|---|---|---|---|---|
| `…122643Z` | 197.0 s | 330.2 s | 331.7 s | 1,785.8 s | 2,636 s | 46.44 GiB | 80.5 MiB | 6751 / 15 |
| `…131116Z` | 195.2 s | 328.3 s | 329.7 s | 1,916.1 s | 2,769 s | 45.89 GiB | 81.3 MiB | 6771 / 14 |
| `…135805Z` | 22.6 s | 38.4 s | 38.5 s | 172.5 s | 306 s | 12.15 GiB | 41.1 MiB | 2562 / 14 |
| `…145819Z` | 37.1 s | 62.4 s | 62.7 s | 291.7 s | 496 s | 17.38 GiB | 48.5 MiB | 3231 / 14 |
| `…140342Z` | 190.7 s | 266.4 s | 267.1 s | 2,386.6 s | 3,238 s | 23.83 GiB | 67.1 MiB | 3763 / 15 |
| `…150653Z` | 23.0 s | 24.1 s | 24.3 s | 93.9 s | 176 s | 1.77 GiB | 26.5 MiB | 651 / 14 |

"All mounted+verified" is measured from the start of daemon spawning to the end
of the per-mount verification; the run wall additionally contains `verify --full`
(see below) and the read-only/unmount steps. Mount latency is measured from
before a daemon's spawn until its mountpoint is observed (200 ms poll), so at
1,000 mounts it includes 1,000-way startup contention on 8 vCPUs; it is not a
steady-state per-mount cost.

## Where it breaks

The default court (`3 GiB / 4 CPUs / 1,024 pids`) cannot host 1,000 concurrent
mounts, and neither can the directed scale budget of 24 GiB / 8 CPUs / 4,096
pids. The exact failures, all from receipts:

* **Pids, not memory, binds first at 4,096.** Each daemon needs several tasks:
  Rust mounts settle at 3 threads, Go mounts at up to 14–15. The 1,000-mount run
  measured 6,751 tasks for 1,000 daemons (6.75/daemon). At
  `pids.max=4096` the court stopped spawning at mount 747
  (`pids.current=3841`), and the run recorded `pids.events max = 98` (98 fork
  attempts refused by the cgroup) and `memory.events oom_kill = 107`. Go
  daemons died with
  `runtime: failed to create new OS thread (have 5 already; errno=11) …
  fatal error: newosproc`; Rust daemons with
  `fuse: fork() failed: Resource temporarily unavailable`.
* **24 GiB is exhausted around 700 mounts** at this corpus size: the directed
  1,000 mount run pinned the cgroup at 24.00 GiB (its limit) and lost 156 of the
  747 spawned daemons by the time RSS was sampled.
* **500 mounts mount and verify but are not stable** at the directed limits:
  all 500 mounted, all 2,125 file hashes and all 500 `cat` comparisons passed,
  but 4 Go daemons died while verification ran, so 4 mounts became unreadable
  and only 496 unmounted cleanly. The cgroup was at 16.32 GiB of 24 GiB with
  3,231 tasks, so these deaths are transient Go thread spikes rather than a
  sustained limit breach: the pids bound must cover the *peak* thread count, not
  the settled one.
* **400 completes cleanly** at 24 GiB / 4,096: 5/5 steps, 400/400 mounts,
  1,700 file hashes, 400 distinct digests, 400 clean unmounts, 0 survivors.
* **At 52 GiB / 16,384 pids, 1,000 completes and verifies twice** with
  `semantic_match=true`. Peak cgroup was 40.9 GiB (78.7 % of the limit) and
  6,751 tasks (41 % of the pids limit), so the successful configuration has
  headroom on both bounds.

Per-daemon memory scales with the pinned snapshot state, so the
"≈16 MiB per mount" number extrapolated from the 100-mount court understates
1,000 mounts:

| corpus objects | receipts | daemon RSS mean | max |
|---|---|---|---|
| 235 | 100-mount court | ≈18.1 MiB | 26.5 MiB |
| 914 | 400-mount | ≈31.1 MiB | 41.1 MiB |
| 1,139 | 500-mount | ≈35.6 MiB | 48.5 MiB |
| 2,264 | 1,000-mount | ≈47.6 MiB | 80.5 MiB |

That is the honest reason a shared multi-mount daemon (one process serving many
contexts) is required to fit many mounts inside a small court; 1,000
single-context daemons need ~46 GiB of RSS sum and ~41 GiB of cgroup peak at this
corpus size.

## Verification integrity

* 1,000/1,000 listing digests matched the construction oracle and were pairwise
  distinct in both 1,000-mount runs.
* 4,250/4,250 mounted files matched their declared SHA-256 (2,000 from `by:task`,
  2,000 from `by:model`, 250 from `by:derived`).
* 1,000/1,000 `weftfs cat` outputs matched the declared hash.
* 20/20 sampled `context resolve` calls matched the oracle id-for-id (the sample
  is every 50th mount; the ≤100 mode still resolves every mount).
* 1,000/1,000 clean unmounts, 0 surviving daemons, in both runs.
* Two runs at the same limits compared semantically identical
  (`context-mounts-20261105T131116Z.determinism.txt` = `semantic_match=true`;
  run ids, timings, and metric values are excluded by the comparator).
* The corpus is re-verified by `weftfs verify --full` inside the store step
  before any mount exists.

`verify --full` is itself a large part of the run wall, and is superlinear in
this metadata-heavy corpus (measured separately in the rust court, same store
layout):

| store | objects | `verify --full` |
|---|---|---|
| 100-mount | 235 | 42.6 s |
| 400-mount | 914 | 124.0 s |
| 500-mount | 1,139 | 194.2 s |
| 1,000-mount | 2,264 | 822.1 s |

Corpus construction through the group-commit harness is 13.3 s for the
1,000-mount store (43 generations, 54 MiB on disk). The same logical store built
one operation per commit (the 100-mount mode) is 82 MiB for 235 objects because
of per-commit advisory index segments.

## Interpretation and limits

* **Implemented and measured:** 1,000 concurrent, distinct Context Roots mounted
  from one store in one host, every mount checked for listing digest, per-file
  SHA-256, and a CLI byte comparison, with clean unmount and no survivors, twice,
  semantically identical.
* **Not claimed:** a fleet claim, a per-mount steady-state cost, or that 1,000
  fits the default 3 GiB / 4 CPU / 1,024-pid court. Single host, synthetic
  deterministic fixtures, local court bind-mount store, page cache warm from
  repeated runs, 8 vCPUs granted to the container although the host has 16.
  The mount latency at 1,000 is startup-throughput under contention
  (p50 ≈ 3.3 min), not the cost of an already-mounted context.
* **Open:** the per-daemon memory cost argues for a shared multi-mount daemon;
  `verify --full` cost at ~2k objects is a separate follow-up; Go's dynamic
  thread count means the pids bound must be sized on peak threads.

## Reproduction

```sh
# 1,000 mounts (the successful configuration), twice, with determinism:
WEFT_MEMORY=52g WEFT_CPUS=8 WEFT_PIDS=16384 WEFT_NPROC=16384 \
  scripts/docker-court.sh fuse 'WEFT_CONTEXT_MOUNTS=1000 bash scripts/demo-context-mounts.sh'
WEFT_MEMORY=52g WEFT_CPUS=8 WEFT_PIDS=16384 WEFT_NPROC=16384 \
  scripts/docker-court.sh fuse 'WEFT_CONTEXT_MOUNTS=1000 \
  WEFT_DEMO_COMPARE_WITH=/work/bench/receipts/context-mounts-<first>.json \
  bash scripts/demo-context-mounts.sh'

# the ceiling under the directed budget:
WEFT_MEMORY=24g WEFT_CPUS=8 WEFT_PIDS=4096 WEFT_NPROC=4096 \
  scripts/docker-court.sh fuse 'WEFT_CONTEXT_MOUNTS=400 bash scripts/demo-context-mounts.sh'
WEFT_MEMORY=24g WEFT_CPUS=8 WEFT_PIDS=4096 WEFT_NPROC=4096 \
  scripts/docker-court.sh fuse 'WEFT_CONTEXT_MOUNTS=1000 bash scripts/demo-context-mounts.sh'

# the unchanged 100-mount mode in the default court:
scripts/docker-court.sh fuse 'WEFT_CONTEXT_MOUNTS=100 bash scripts/demo-context-mounts.sh'
```

Raw logs and results TSVs for the scale receipts are stored beside each JSON as
`.log.zst` / `.results.tsv.zst`.

## Follow-up: shared multi-mount daemon (R7)

The per-daemon memory and pids costs recorded above motivated Subphase R7. One
process now serves N read-only views over one shared store. The measured court
is `bench/RESULTS-shared-mounts.md` (receipt
`bench/receipts/shared-mounts-20261106T082042Z.json`); it compares both modes at
N=100 rather than 1,000, and at 512 MiB per-daemon mounted only 51/100 while
shared mounted 100/100. This file's historical measurements are unchanged and
were not re-run.
