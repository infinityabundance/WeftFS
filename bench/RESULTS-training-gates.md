# Training-gate courts (T053, T055-T060), 2026-11-14

Status: implemented and measured. Every number below is emitted by
`scripts/perf_numbers.py` from the committed receipt named in the manifest;
the prose states method, losses and non-claims only.

Court: `weft/python-court:1` for the framework courts and `weft/bench-court:1`
for the rank-restore court; **3 GiB memory == swap, 4 CPUs, 1024 pids, 1 GiB
`/tmp`, `cap-drop=ALL`, `no-new-privileges`.** One host, CPU only. No GPU
exists in these courts and no GPU, multi-host, fleet, cloud or physical
power-loss claim is made. The multi-rank court uses four real processes and
gloo over loopback on that one host; it is not a cluster result.

## T053 — rank-directed cross-topology restore

Method. A real Qwen2.5-0.5B-Instruct container (HF LFS SHA-256 verified) is
ingested as an 8-way axis-0 checkpoint. For each target topology `S in
{1,2,4,8}` on the stored axis and every rank `r in 0..S`, the harness asks the
new rank-directed API for only `r`'s extents, then compares every materialised
byte with the corresponding slice of a full-tensor restore. A second target
splits axis 1, which no stored shard is split on. A non-divisible target is a
typed refusal.

Arithmetic. `needed = product(destination extents) * element_bytes` per rank.
The stored shard set is the intersection of the destination slab along the
stored split axis with each stored shard slab. With target axis == stored axis,
every stored shard is fetched exactly once across the ranks (representation
amplification 1.0). With a finer target than the stored split, each stored
shard is fetched by several destination ranks (amplification = target/stored).
With a different axis, every rank intersects every shard.

<!-- BEGIN GENERATED:tg-rank (scripts/perf_numbers.py) -->
Physical read amplification for the rank-directed restore is **0.8342×** at 1-way and **0.8342×** at 8-way on the stored axis; representation amplification at 8-way is **1.000×** across 290 tensors (290 exact).
A target split on axis 1 while the checkpoint is stored on axis 0 costs **3.3355×** physical and **3.998×** representation.
<!-- END GENERATED:tg-rank -->

Losses: the fetch is shard-granular, so a destination topology finer than the
stored split re-reads stored shards; a target split on another axis re-reads
every shard on every rank. Mode-10 shard frames are decoded whole, so a region
read picks intersecting **objects**, not sub-object ranges. The receipt is one
host with a warm page cache.

## T055 — complete state, SIGKILL, exact resume

Method. The 0.414M transformer trains with AdamW and a cosine scheduler; the
checkpoint contains model, optimizer moments and step, scheduler, an honestly
disabled GradScaler, the python/torch/NumPy RNG states, the data
bit-generator state and cursor, and the step counter. Three kill points
(after a step before the next checkpoint, after a completed checkpoint,
during an in-flight save) are SIGKILLed and resumed in a fresh process. Loss
bits and parameter hashes are compared with an uninterrupted control at the
same indices. A native DCP `FileSystemWriter` arm does the same work.

<!-- BEGIN GENERATED:tg-resume (scripts/perf_numbers.py) -->
WeftFS: 48-step control **4,993.1 ms**, first checkpoint write **624.2 ms**, kill to first resumed step **1,400.0 ms**; **24** resumed steps compared exactly.
DCP: control **2,032.6 ms**, first checkpoint write **98.4 ms**, kill to first resumed step **1,205.1 ms**.
<!-- END GENERATED:tg-resume -->

Losses: the RNG draws enter the reported loss only, so the parameter hashes do
not prove RNG restore (the loss bits do; both are reported). The GradScaler is
disabled on CPU, so its correctness is not tested. DCP's writer leaves a
partial directory when killed in flight; the store never exposes an
uncommitted checkpoint. A SIGKILL is a process fault, not power loss, and host
caches are not dropped. The native DCP writer is materially faster on this
state.

## T056 — four-rank publish and cross-topology load

Method. Four ranks under `torchrun` with gloo each write their own contiguous
tensor slices; rank 0 reassembles and publishes with one checkpoint write (one
commit). Load asks each destination rank for only its own extents under
4→2, 4→1 and 4→8. Faults: a worker killed before its fragment, rank 0 killed
after all fragments and before publishing, and the publishing CLI killed during
publication. Each fault is followed by a clean retry.

<!-- BEGIN GENERATED:tg-multirank (scripts/perf_numbers.py) -->
4-rank publish: one checkpoint write **546.1 ms**, **168** shard objects; load amplification **1.1930×** physical / **1.000×** representation at 2-way and **2.3861×** / **2.000×** at 8-way.
<!-- END GENERATED:tg-multirank -->

Losses: the four ranks contribute through a shared host staging directory; the
store publication itself is one process, not an N-way parallel store writer.
The model is rebuilt deterministically by every rank so slices can be checked
locally; that is a verification device, not distributed-training accuracy. A
target finer than the stored split re-reads stored shards (the reported
representation amplification). SIGKILL is a process fault, not power loss.

## T057 — asynchronous save overlapping a sustained loop

Method. Three arms on the same deterministic 160-step loop: no checkpoint, one
publication worker driving `save_checkpoint_async`, and a bounded
`CheckpointSession` queue with non-blocking submission. Step times, submit
pause, outstanding saves, queue high water, admission waits/refusals, drain
time and process RSS are reported; the final parameter hash is identical in all
arms and the final checkpoint verifies. A separate probe starts two per-call
async saves at once and records the single-writer collision.

<!-- BEGIN GENERATED:tg-async (scripts/perf_numbers.py) -->
No-checkpoint step p50 **10.013 ms**. Async worker: step p50 **×1.0086**, p95 **×1.0295**, maximum outstanding **10**, drain **54,337.8 ms**.
Bounded queue: step p50 **×1.0151**, high water **2**, **10** admission waits (longest **9,314.7 ms**), drain **11,964.9 ms**.
<!-- END GENERATED:tg-async -->

Losses: the step is CPU-bound and short, so the same save looks large relative
to it. Outstanding saves are sampled after each step, not continuously. RSS is
a high-water mark. Two overlapping per-call async saves collide on the
single-writer store; overlap requires one publication worker or the queue.
**GPU idle time is unmeasured** and is not claimed.

## T058 — recovery-first useful computation

Method. The next full training step needs every tensor, so a partial restore
cannot run the whole model. The smallest honest version is layer-scoped:
restore `tok`, `pos` and `blocks.0.*` through the per-tensor path, run the
embedding + block-0 forward and the block-0 backward to a deterministic
target, and compare the loss bits, hidden state, output and every block-0
gradient with the identical computation after a complete restore.

<!-- BEGIN GENERATED:tg-useful (scripts/perf_numbers.py) -->
Layer-scoped restore: **14** of **42** tensors, **1,268,894 B** versus **3,635,846 B**, restore **10.225 ms** versus **28.114 ms**, time to first useful step **30.904 ms** versus **47.813 ms**; the scoped step itself is **4.223 ms** versus **4.080 ms**.
<!-- END GENERATED:tg-useful -->

Losses: the step is layer-scoped and is not a whole-model progress claim. The
un-restored tensors keep their seeded initial values and are not used by the
scoped step. The per-tensor service path re-reads the manifest per tensor; that
cost is inside both arms. No framework dependency barrier is bypassed.

## T059 — adapter materialization receipts

Method. `weft.materialization.receipt.v1` is derived by re-reading every stored
shard and recomputing every root; it records per tensor the logical/content
roots, SHA-256, bytes, every source object (name, id, recomputed roots,
length) and the destination offset/shard count. The DCP reader and Orbax
handler bind their actual restored entries in `weft.adapter.receipt.v1`, and a
separate process (`weftfs receipt verify`) re-derives both and checks the
binding. A canonical rewrite of one destination hash and of one source object
id are both refused.

<!-- BEGIN GENERATED:tg-receipts (scripts/perf_numbers.py) -->
DCP materialization receipt: **4,635 B**, **4** entries bound, **16** source objects; Orbax: **2,357 B**, **4** entries, **4** source objects.
<!-- END GENERATED:tg-receipts -->

Losses: the receipt is a second full read of every stored shard. When enabled,
the DCP adapter also hashes each restored tensor. A receipt whose set root is
also rewritten is refused at decode, before field comparison. The offline
verifier reads the store through the same derivation code as the emitter (in a
different process with fresh reads); it is not an independent implementation.

## T060 — DCP and Orbax OCDBT baselines

Method. Five arms save the same 1.66 MB complete state: one
`WeftStorageWriter` per save, one `CheckpointSession` without container
retention, a session with the `fast` policy, native DCP `FileSystemWriter`, and
Orbax `StandardCheckpointHandler` (OCDBT). Arms run in interleaved sweeps and
p50 is the median of sweep medians. A child acknowledges a session save and is
SIGKILLed; the parent reopens and verifies the acknowledged checkpoint.

Targets stated earlier and not redefined: a WeftFS DCP save p50 below 20 ms and
parity with OCDBT.

<!-- BEGIN GENERATED:tg-baselines (scripts/perf_numbers.py) -->
Save p50: WeftFS session **30.429 ms**, WeftFS fast **30.167 ms**, OCDBT **29.796 ms**, native DCP **9.598 ms**. Within the WeftFS session, the store write is **11.579 ms** and staging plus one prepare round trip is **18.893 ms**.
<!-- END GENERATED:tg-baselines -->

A second independent run of the same court:

<!-- BEGIN GENERATED:tg-baselines-repeat (scripts/perf_numbers.py) -->
<!-- END GENERATED:tg-baselines-repeat -->

Attacks applied: one persistent service per session, the reusable DCP
metadata plan, container retention disabled for the session arm, and the fast
write policy measured as a size-for-latency floor. Attacks not applied:
batching several checkpoints into one commit (each checkpoint is a separate
durable object with its own acknowledgement), and an in-store metadata stream
that would remove the prepare round trip. Those are the two candidate paths to
the unmet target; neither is claimed.

Losses: the 20 ms target is not met. The native DCP writer is faster than every
WeftFS arm. WeftFS and OCDBT are within a fraction of a millisecond of each
other on this fixture — inside host noise, so this is not a robust win. The
remaining WeftFS term is the Python container staging copy plus one service
prepare round trip; the store's own write is a minority of the total. Native
DCP and OCDBT write local directories without the store's content-addressed
identity or commit protocol. Durability is not traded for latency: the
acknowledged-then-SIGKILL test verifies.

## Reproduction

```sh
scripts/docker-court.sh rust  "cargo build --release --features fuse --bin weftfs"
scripts/docker-court.sh bench "bash bench/scripts/rank_restore_court.sh"
scripts/docker-court.sh python "bash bench/scripts/training_resume_court.sh"
scripts/docker-court.sh python "bash bench/scripts/multirank_dcp_court.sh"
scripts/docker-court.sh python "bash bench/scripts/async_save_court.sh"
scripts/docker-court.sh python "bash bench/scripts/useful_step_court.sh"
scripts/docker-court.sh python "bash bench/scripts/adapters_receipts_court.sh"
scripts/docker-court.sh python "bash bench/scripts/dcp_orbax_baselines_court.sh"
```

Framework installs use `pip --target` into the court cache, as the existing
Orbax/DCP courts do; the images are not modified.
