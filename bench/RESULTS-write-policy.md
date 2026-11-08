# Write policy and background transcode — measured

Receipt: `bench/receipts/write-policy-20261107T235728Z.json`
(raw court log beside it: `bench/receipts/write-policy-20261107T235728Z.log`)

Court: `bench/scripts/write_policy_court.sh` in `weft/bench-court:1`
(`sha256:4041d66e…`), Linux 7.3.1, 3 GiB / 3 GiB swap, 4 CPUs, pids 1024,
bounded `/tmp`. Single host, warm page cache after the first configuration, no
fleet claim.

Corpus: `Qwen/Qwen2.5-0.5B` (`88c14255…0ed342`) and
`Qwen/Qwen2.5-0.5B-Instruct` (`fdf756fa…88fb7fe`), BF16, 290 tensors,
988,097,824 B each. Both SHA-256 verified in the court.

Configuration: checkpoint `--shards 8`, axis 0, one shard object per tensor
slice (2,320 shard files). `planned` = `--shard-strategy tensor-sharded`
(2,320 mode-10 q8_0 byte transports). `fast` = raw or one ZSTD frame per shard.
`auto` resolves to fast when `--critical-path` or `--deadline-ms` is given; it
is covered by tests, not re-measured here.

Pause = wall time from the `write_checkpoint` call to its return (the group
commit is durable). Wall = whole process. CPU = user+sys from `/proc/self/stat`.
All restores are exact (`sha256` of the produced container equals the source).

## Base

| policy | pause ms | wall ms | CPU ms | store bytes | shard strategies |
|---|---:|---:|---:|---:|---|
| planned | 98,882.7 | 98,902.7 | 18,770 | 824,049,698 | tensor-sharded 2,320 |
| fast raw | **55,689.4** | 56,103.3 | **2,470** | 992,491,894 | raw 2,320 |
| fast zstd-1 | 75,815.3 | 75,845.1 | 2,690 | 775,692,890 | zstd 2,320 |
| fast zstd-3 | 137,135.5 | 137,168.2 | 2,910 | 775,589,685 | zstd 2,320 |

## Instruct

| policy | pause ms | wall ms | CPU ms | store bytes |
|---|---:|---:|---:|---:|
| planned | 108,088.5 | 108,623.5 | 18,780 | 824,511,180 |
| fast raw | 110,191.2 | 110,239.0 | 2,780 | 992,491,894 |
| fast zstd-1 | **76,572.7** | 76,602.0 | 2,720 | 776,155,588 |
| fast zstd-3 | 95,997.7 | 96,790.3 | 2,800 | 776,062,140 |

**The robust result is CPU, not wall.** Fast removes 6.5–7.6× of encode CPU
(planned 18.8 s → 2.5–2.9 s) on both containers. Wall/pause is dominated by
publishing 2,320 objects and by host page-cache state; on these single runs raw
won the base wall (1.77× over planned) while zstd-1 won the instruct wall
(1.41×), and zstd-3 lost on both. A repeat would be needed to call the wall
ordering stable, so it is not claimed. Raw writes 28.0 % more bytes than
zstd-1 (992.5 vs 775.7 MB); zstd-1 is the default because it is within 9 % of
raw's CPU and 21.8 % smaller on disk. This is a policy choice, not a measured
wall win.

## Background transcode

`weftfs transcode --root /checkpoints/model` (or
`Store::transcode`): exclusive single-writer pass, bounded by objects, logical
bytes, wall milliseconds, and a cancellation flag. Each rewrite is a new
version of the same object id (old version retained), with a
`weft.provenance.v1` record `operation: "transcode"` and a read-back byte
comparison before the pass reports success.

| source | wall ms | CPU ms | logical bytes read | old physical | new physical | applied |
|---|---:|---:|---:|---:|---:|---:|
| base raw | 123,617.4 | 34,120 | 988,065,536 | 987,922,432 | **658,437,460** | 1,352 |
| base zstd-1 | 227,642.8 | 60,600 | 988,065,536 | 772,037,803 | **658,580,564** | 2,320 |
| base zstd-3 | 236,625.5 | 60,660 | 988,065,536 | 771,934,598 | **658,580,564** | 2,320 |
| instruct raw | 128,450.2 | 37,330 | 988,065,536 | 987,922,432 | **658,385,249** | 1,352 |
| instruct zstd-1 | 242,828.1 | 61,440 | 988,065,536 | 772,500,501 | **658,528,353** | 2,320 |
| instruct zstd-3 | 284,063.1 | 61,220 | 988,065,536 | 772,407,053 | **658,528,353** | 2,320 |

After transcode the mix is 1,352 float-tensor + 968 raw shards (the 48
embedding/norm tensors stay raw in the independent plan), 658.4–658.6 MB per
container. The raw-source pass leaves 968 raw objects untouched because the
planned independent representation for them is also raw (they are already the
smallest form); those are not counted as applied.

### Restore before vs after transcode (full container)

| container | fast-raw before | after | fast-zstd1 before | after |
|---|---:|---:|---:|---:|
| base | 1,675.8 ms | 4,431.5 ms | 2,194.5 ms | 4,755.9 ms |
| instruct | 1,717.8 ms | 4,698.6 ms | 2,208.7 ms | 6,005.6 ms |

**Transcode is a storage win and a restore loss**: the planned representation
is mode-9 float-tensor, which decodes ~2–2.7× slower than the fast ZSTD frame.
This is the same trade recorded in R1. A 1-row read after transcode is
18.7–38.1 ms and a single-tensor read 466–625 ms (before: 18.7–38.1 ms and
466–616 ms); no region claim beyond the touched-unit count.

## Crash during transcode

The crash proxy aborts a child process with `SIGABRT` (exit 134) through a
test-only hook.

| phase | exit | strategies after reopen | checkpoint verify |
|---|---:|---|---|
| abort after 1 durable publication | 134 | float-tensor 1, raw 2,319 | exact, 290 tensors, 0 problems |
| abort just before the next publication | 134 | float-tensor 2, raw 2,318 | exact, 290 tensors, 0 problems |
| clean pass after the crash | 0 | float-tensor 1,352, raw 968 | exact, 0 problems |

Every object is old-or-new; no partial state was observed. This is a
process-fault proxy, not physical media power loss.

## DCP and Orbax adapter save pause

Court: `bench/scripts/adapter_pause_court.py` in `weft/python-court:1`.
Fixture: synthetic CPU-only 8 × 4 MiB f32 (32 MiB) for each adapter. A 0.5B
pair does not fit in the 3 GiB court with torch/JAX, the adapter staging
buffer, and the CLI resident; the full-pair pause is the CLI court above.
Every save was verified with the Rust CLI (`checkpoint verify --full`) and
restored for exactness.

| adapter | planned pause | fast pause | speedup | bytes |
|---|---:|---:|---:|---|
| PyTorch DCP (`dcp.save`) | 1,384.5 ms | **114.8 ms** | **12.06×** | 31,186,687 vs 31,186,675 |
| Orbax (`WeftOrbaxHandler` + `Checkpointer`) | 1,394.6 ms | **131.3 ms** | **10.62×** | 31,187,054 vs 31,187,036 |
| Orbax's own OCDBT (same tree) | — | 31.9 ms | — | 31,020,645 |

Receipt: `bench/receipts/adapter-pause-20261108T004305Z.json`.

`WEFT_WRITE_POLICY` is the deployment knob the Python service reads on every
`checkpoint write`; `WEFT_FAST_CODEC` / `WEFT_FAST_ZSTD_LEVEL` select the codec.
The policy is carried on the checkpoint-service protocol (v2, backward
compatible: a v1 body has no trailing policy bytes and keeps planned).

**What remains of the 23.85× deficit.** On the same fixture the deficit was
43.7× with the planned policy and is **4.12×** with fast (WeftFS 131.3 ms vs
Orbax OCDBT 31.9 ms). The remaining gap is not encode: it is the safetensors
staging copy, the memfd hand-off, one service round trip, and one durable
commit. On the prior 58.7 MB Orbax receipt the save was 22.2× slower; this
court does not re-run that fixture, so the cross-receipt comparison is
indicative only.

## Losses and non-claims

* Wall/pause ordering across fast codecs is not stable in this court; CPU is.
* Transcode is slower than the save it replaces (2–4 minutes for 0.99 GB) and
  makes restore slower; it is storage, not latency, work.
* Transcode is a separate exclusive pass, not a concurrent background thread
  inside the training writer. The store has one single-writer lock by design.
* `fast` does not produce weight-delta: a fast save offers its shard object as
  a causal parent for the *next* checkpoint, but the fast write itself never
  runs a delta encoder.
* One host, one public model pair, synthetic adapter fixtures, no GPU, no
  multi-writer, no physical power-loss claim.
