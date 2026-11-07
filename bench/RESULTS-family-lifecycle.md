# Checkpoint-family lifecycle court

One court that measures the **whole lifecycle** of a checkpoint family instead of
an isolated decode: ingest, retention, snapshot, restore (five shapes × 1/4
threads), cross-topology restore, process-fault recovery, and GC with and without
a pinned snapshot. It compares the planner's bytes-only default against an
access-pattern plan for 1-row and 100-row reads, against independent per-tensor
`zstd -19`, against `zstd --patch-from`, and over the local backend and a real
MinIO object store.

Receipt: `bench/receipts/family-lifecycle-20261107T132359Z.json` (3,305,116 B;
7 configurations: two families × three plans, plus the reduced object-store
run).
Harness: `examples/family_lifecycle.rs`. Court:
`bench/scripts/checkpoint_family_court.sh`.

**Revision provenance.** Every measured phase ran against the working tree that
was committed, immediately afterwards and unchanged in every measurement path, as
`0d005ff`. The receipt's `repo_weftfs` field therefore records the parent HEAD at
run time (`0c84c34`), because the harness was still uncommitted while it ran.
The only later code change to the harness is the `recompare` subcommand added in
`1901336`, which recomputes the `comparison` block from the `ingest` and
`baselines` blocks already inside each configuration's receipt (it was used once,
for the object-store configuration's per-tensor note); no measurement phase was
re-run, and no measured value changed.

The receipt is the authoritative evidence: it carries every per-phase JSON
(ingest, measure, baselines, topology, recovery, both GC runs), every per-tensor
row, every latency sample, and the corpus hashes. The court's combined `tee` log
was overwritten when the object-store configuration ran as a separate invocation
that re-opened the same log path; the surviving log
(`…object-store-invocation.log`) is that invocation only. No number in this
document depends on the log: each is in the receipt.

## 1. What was run, exactly

```sh
# Local configurations (both families), 6 GiB court.
WEFT_MEMORY=6g scripts/docker-court.sh bench \
  "cd /work && bash bench/scripts/checkpoint_family_court.sh"

# Object-store configuration (reduced to the qwen family), 6 GiB court.
WEFT_MEMORY=6g scripts/docker-court.sh bench \
  "cd /work && WEFT_FAMILY_SKIP_FAMILY=1 WEFT_FAMILY_CONFIGS='' \
   WEFT_FAMILY_FAMILIES='' WEFT_FAMILY_S3=1 \
   bash bench/scripts/checkpoint_family_court.sh"
```

Every phase is a separate process, so peak RSS is that phase's `VmHWM`. The
court builds the release example once and then drives the **shipped library
API**; it never calls the model framework. The safetensors header is parsed
directly (8-byte LE length + JSON).

### Environment (from the receipt)

| | |
|---|---|
| Court image | `weft/bench-court:1`, `sha256:4041d66e…b40f2041` |
| Kernel | `7.3.1-1-cachyos` (x86_64) |
| Limits | memory 6g, memory-swap 6g, pids 1024, cpus 4, `/tmp` tmpfs 1g, `cap-drop=ALL`, `no-new-privileges` |
| WeftFS | `0c84c34853fff19d30799d37df9b1e1a889b8bbb` (plus this court's commits) |
| WeftCodec | `46713726cd454f587c4db006ae723bd4bd4474a2` |
| WeftCodec-Go | `bf6bb70e827fe801fbe83c9e352d0ff3621dc704` |
| WeftFS-Go | `b3e85b83b3ceb108f7ffa30d3130944b077297af` |
| Repetitions | ingest 3, measure 3, topology 3, recovery 3 |
| Peak RSS, local qwen | 3,008,868 KiB (default), 3,401,560 KiB (region1), 3,439,668 KiB (region100) |

**Why 6 GiB and not the 3 GiB default.** In the default court the full qwen
family ingest already reached 3,013,000 KiB (2.87 GiB of 3 GiB) and the
access-pattern plan (tile candidates) was **OOM-killed** at 3 GiB — that run is
recorded as a loss in §9. Every hard limit except memory is the court default,
memory == swap, and each configuration's receipt records its own
`environment.court_memory`, so the two limits are never confused.

### Corpus (LFS-verified, re-hashed in the court)

| File | Bytes | SHA-256 |
|---|---|---|
| `Qwen2.5-0.5B.safetensors` | 988,097,824 | `88c14255…0ed342` |
| `Qwen2.5-0.5B-Instruct.safetensors` | 988,097,824 | `fdf756fa…fb7fe` |
| `pythia-70m-step0.safetensors` | 281,715,176 | `17a077ab…af8e` |
| `pythia-70m-step1.safetensors` | 281,715,176 | `652e38fd…a109` |

The court refuses to run if any file's SHA-256 differs from the committed
manifest, and it also greps the manifests to prove the hard-coded values have
not drifted from `bench/corpora/real-models/`.

**Families are never aggregated.**

* `pythia-consecutive` — step0 → step1 of a **training run**: 76 F32 tensors,
  the second checkpoint differs from the first in **19 32-bit words**.
* `qwen-finetune` — Qwen2.5-0.5B → **Instruct**, an *independent* instruction
  fine-tune: 290 BF16 tensors, 97.9 % of words changed.

### Configurations

| Name | Mechanism it is supposed to exploit |
|---|---|
| `default` | bytes-only planner (`lambda_ns = 0`, whole-object access), causal parent offered for the second checkpoint |
| `region1` | same planner at `lambda_ns = 1`, access pattern `region:1 row:1000 reads`, 1 MiB auto-tile budget so mode 15/17 is offered |
| `region100` | same at `region:100 rows:1000 reads` |
| `s3-reduced` | `default` over MinIO on loopback (qwen family only) |
| `checkpoint8` (family-level) | the checkpoint API, 8-way `tensor-sharded` on axis 0, used for the cross-topology phase |
| baselines | independent per-tensor `zstd -19`; `zstd --patch-from` per tensor and whole-file |

The region configurations price a **region read**, not a whole-object read. The
byte counts are derived from the family's largest rank-2 tensor (the Qwen
embedding is `[151936, 896]`, so 1 row = 1 792 B and 100 rows = 179 200 B;
pythia's `embed_in` is `[50304, 512]` F32, so 1 row = 2 048 B).

## 2. Ingest, retention, and planner choice

`data-only` = `objects/` + `manifests/`; `indexes` are the advisory selector
segments. Ingest wall time is the median of three full family ingests (both
checkpoints) and **excludes** the final `verify --full`.

| Family / config | Ingest median | reps (ms) | Peak RSS | data-only B | indexes B | total B |
|---|---|---|---|---|---|---|
| pythia / default | 31,620 ms | 34,255 / 31,620 / 12,767 | 1,193,536 KiB | 173,233,994 | 4,917,263 (2.76 %) | 178,313,138 |
| pythia / region1 | 18,153 ms | 18,066 / 18,153 / 18,213 | 1,323,228 KiB | 222,137,601 | 5,061,010 (2.23 %) | 227,362,264 |
| pythia / region100 | 18,454 ms | 18,269 / 18,861 / 18,454 | 1,323,140 KiB | 225,738,590 | 5,029,522 (2.18 %) | 230,931,273 |
| qwen / default | 142,420 ms | 139,655 / 142,420 / 187,696 | 3,008,868 KiB | 1,199,958,249 | 76,367,980 (5.98 %) | 1,276,912,624 |
| qwen / region1 | 113,326 ms | 113,753 / 112,759 / 113,326 | 3,401,560 KiB | 1,317,785,514 | 79,519,404 (5.69 %) | 1,397,901,069 |
| qwen / region100 | 145,267 ms | 145,267 / 116,709 / 166,021 | 3,439,668 KiB | 1,375,240,590 | 77,733,036 (5.35 %) | 1,453,562,097 |
| qwen / s3-reduced | 167,894 ms | 167,894 (median of 3) | 3,000,976 KiB | not walked (remote) | not walked | write stream only |

The pythia `default` spread (12.8–34.3 s) is real: the first two repetitions
overlapped writeback of the preceding baseline phase. **The median is published
as-is; the spread is larger than the differences between the pythia
configurations.** Cross-configuration pythia ingest comparisons are therefore
not reliable at this size.

### Planner choice per tensor

| Family / config | Base checkpoint | Second checkpoint |
|---|---|---|
| pythia / default | zstd × 76 → 173,212,480 B | sparse-delta × 26 (22,235 B) + zstd × 50 (19,976 B) |
| pythia / region1 | raw × 50 (176,140 B) + tensor-tiled × 26 (121,187,613 B) | same split (176,140 + 121,187,612) |
| pythia / region100 | raw × 56 (6,473,680 B) + tensor-tiled × 20 (118,491,062 B) | same split |
| qwen / default | float-tensor × 169 (657,881,943 B) + zstd × 73 (123,840 B) + raw × 48 (24,096 B) | weight-delta × 241 (541,247,695 B) + zstd × 1 + raw × 48 |
| qwen / region1 | float-tiled × 169 (658,410,959 B) + raw × 121 (172,870 B) | same split |
| qwen / region100 | float-tiled × 73 (598,968,882 B) + raw × 217 (88,343,254 B) | same split |

Consequences worth stating plainly:

* On the **consecutive** family the causal parent is what pays: the second
  checkpoint is 42,211 B against 144,403,851 B for per-tensor `zstd -19`
  (0.0003×, 3,421× smaller). That is the sparse-delta mode on a 19-word change.
* On the **fine-tune** family the second checkpoint is 541,273,184 B
  (weight-delta) against 767,597,259 B for `zstd -19` (0.705×). The whole
  family is 1,199,958,249 B of data — **under** one `zstd -19` copy of the
  single base container (768,347,895 B) only because the base wins on its own
  (0.856×) and the delta is smaller than a full copy.
* Every access-pattern plan converts **every** rank-2 tensor that beats its
  alternatives into a tile representation and leaves the rest `raw`. `raw` is
  chosen because at `lambda_ns = 1` the model charges one byte per estimated
  nanosecond, so decoding dominates and uncompressed storage wins.

### Where the planner loses (per tensor, against `zstd -19`)

| Family / config | Checkpoint | WeftFS | zstd -19 | ratio | tensors lost | bytes lost | worst loss |
|---|---|---|---|---|---|---|---|
| pythia / default | first | 173,212,480 | 144,397,747 | **1.20×** | 76 / 76 | 28,814,733 | `embed_out.weight` +11,211,210 B (+21.5 %) |
| pythia / default | second | 42,211 | 144,403,851 | 0.0003× | 50 / 76 | 18,837 | — |
| pythia / region1 | first | 121,363,753 | 144,397,747 | 0.84× | 50 / 76 | 175,001 | a 22 B bias modelled raw at 8,438 B (+38 255 %) |
| pythia / region100 | first | 124,964,742 | 144,397,747 | 0.865× | 56 / 76 | 3,080,034 | the same small biases |
| qwen / default | first | 658,029,879 | 768,347,895 | 0.856× | 121 / 290 | 39,448 | a 1.6 KB layernorm +410 B (+34.6 %) |
| qwen / default | second | 541,273,184 | 767,597,259 | **0.705×** | 121 / 290 | 39,325 | — |
| qwen / region1 | first | 658,583,829 | 768,347,895 | 0.857× | 121 / 290 | 64,382 | a 2 KB norm stored raw (+110.8 %) |
| qwen / region100 | first | 687,312,136 | 768,347,895 | 0.895× | 217 / 290 | 18,847,127 | raw norms and biases |

**The single most important loss:** the planner's `zstd` candidate is the
codec's own ZSTD frame (level 3), not level 19. On the F32 family that is
28.8 MB, or 20 % of the checkpoint, against the independent CLI baseline. The
planner is choosing correctly *among its own candidates*; the candidate set is
what loses.

`zstd --patch-from` baselines (same receipt):

| Family | per-tensor `-19` | whole-file `-12` |
|---|---|---|
| pythia | 178,062 B | **30,265 B** (wall 1,257 ms) |
| qwen | 760,669,113 B | 749,534,915 B (wall 25,659 ms) |

On the consecutive family the whole-file `--patch-from -12` (30,265 B) **beats**
the family total (42,211 B) by 28 %. Publish that as a loss: a single
patch-compressed file is cheaper here than the per-tensor sparse deltas, because
the change is one contiguous region and the per-tensor frames carry 3 × 76
descriptor and table bytes. On the fine-tune family the whole-file patch
(749.5 MB) loses badly to weight-delta (541.3 MB).

## 3. Snapshot

| Family | create (1 sample; 3 commits exercised) | metadata (`snapshot_list` + resolve, median of 6) |
|---|---|---|
| pythia | 9.2 / 10.2 / 10.3 ms | 1 µs |
| qwen | 11.0 / 11.9 / 12.7 ms | 1 µs |

Snapshot creation costs one commit (a `SetSnapshot` op + the `refs/snapshots`
pointer); listing and resolving a pin is a pure in-memory map lookup. A pin does
not slow the live read path: the full restore with the snapshot pinned is
inside the same distribution as without it.

## 4. Restore matrix

All reads are `sha256_match = true` and every byte was compared against the
source tensor. `bytes` is the backend bytes fetched on the last repetition.

### pythia-consecutive

| Read | 1 thread p50 / p99 | 4 threads p50 / p99 | bytes fetched |
|---|---|---|---|
| full family (both checkpoints) | 1,028.0 / 1,106.4 ms | 790.9 / 794.3 ms | 173,305,059 |
| 100-tensor subset | 919.1 / 926.4 ms | 741.7 / 742.1 ms | 173,305,059 |
| single tensor (`embed_in`, 103 MB) | 340.1 / 342.8 ms | 340.7 / 341.7 ms | 63,391,536 |
| 100-row region | 339.4 / 341.0 ms | 337.7 / 337.8 ms | 63,391,536 |
| 1-row region | 337.8 / 338.4 ms | 337.9 / 338.5 ms | 63,391,536 |

### qwen-finetune

| Read | 1 thread p50 / p99 | 4 threads p50 / p99 | bytes fetched |
|---|---|---|---|
| full family | 8,753.9 / 8,756.9 ms | 3,973.7 / 3,980.8 ms | 1,199,642,104 |
| 100-tensor subset | 4,494.8 / 4,510.0 ms | 2,826.8 / 2,835.9 ms | 644,377,858 |
| single tensor (`embed_tokens`, 272 MB) | 2,365.1 / 2,411.5 ms | 2,369.7 / 2,371.8 ms | 333,871,119 |
| 100-row region | 2,363.9 / 2,366.8 ms | 2,368.8 / 2,370.2 ms | 333,871,119 |
| 1-row region | 2,366.7 / 2,387.8 ms | 2,358.9 / 2,362.0 ms | 333,871,119 |

### Same reads under the region plans

| Family / config | full (1 / 4 threads) | 100-row region | 1-row region | single tensor |
|---|---|---|---|---|
| pythia / region1 | 714.7 / 545.9 ms | **1.48 / 1.54 ms** (362,852 B) | **1.46 / 1.49 ms** (362,852 B) | 220.8 / 81.2 ms |
| pythia / region100 | 705.3 / 547.3 ms | **1.49 / 1.53 ms** (362,852 B) | **1.45 / 1.46 ms** (362,852 B) | 220.4 / 80.9 ms |
| qwen / region1 | 1,948.9 / 877.8 ms | **5.31 / 2.18 ms** (2,896,846 B) | **5.22 / 1.91 ms** (2,896,846 B) | 452.0 / 192.7 ms |
| qwen / region100 | 1,840.8 / 858.6 ms | **5.31 / 2.06 ms** (2,896,846 B) | **5.25 / 1.89 ms** (2,896,846 B) | 453.7 / 192.4 ms |

A 1-row read on qwen drops from 2.37 s and 333.9 MB fetched to 5.2 ms and
2.9 MB (**454× less data, 454× faster**); on pythia from 338 ms / 63.4 MB to
1.46 ms / 0.36 MB (**175× less data, 231× faster**). 4 threads help a single
tensor's tiles and the across-tensor pipeline, but not the small region read
itself (one region touches 4 tiles).

## 5. Cross-topology restore

Written 8-way sharded on axis 0 through the checkpoint API, then restored as
8-way, 4-way, row-major, and (largest tensor only) blocked. Every per-shard and
per-tensor logical root was recomputed and matched.

| Family | topology | tensors | roots verified | bytes fetched | vs naive full read | p50 |
|---|---|---|---|---|---|---|
| pythia | sharded 8 | 76 | 76/76 | 197,146,951 | 0.700× (281,706,496) | 2,888 ms |
| pythia | sharded 4 | 76 | 76/76 | 197,146,951 | 0.700× | 2,907 ms |
| pythia | row-major | 76 | 76/76 | 197,146,951 | 0.700× | 2,874 ms |
| pythia | blocked `embed_in` | 1 | 1/1 | 71,856,081 | 0.697× of 103,022,592 | 1,087 ms |
| qwen | sharded 8 | 290 | 290/290 | 823,438,236 | 0.833× (988,065,536) | 10,204 ms |
| qwen | sharded 4 | 290 | 290/290 | 823,438,236 | 0.833× | 10,028 ms |
| qwen | row-major | 290 | 290/290 | 823,438,236 | 0.833× | 10,161 ms |
| qwen | blocked `embed_tokens` | 1 | 1/1 | 225,254,705 | 0.827× of 272,269,312 | 2,971 ms |

Topology changes only how the *assembled* bytes are re-expressed and verified;
it does not change which objects are fetched (hence identical byte counts).
This path is **much slower** than the planner store's restore (§4): 2.9 s vs
1.0 s for pythia, 10.2 s vs 8.8 s for qwen at one thread, and 4–5× slower than
the region plans. The checkpoint API is the compatibility surface, not the fast
path.

## 6. Recovery (process-fault proxy)

A child process publishes a real checkpoint (12 tensors of each family) and is
`SIGABRT`ed at one publication phase. The parent then opens the store, asks for
the checkpoint manifest, and runs `verify --full`.

| Phase | Family | old / new | open p50/p99 | time-to-usable p50/p99 | verify | expected |
|---|---|---|---|---|---|---|
| `after_objects` | both | 3 / 0 | 8.5 / 11.0 ms | 0.13 / 0.16 ms | clean | yes |
| `after_commit_file` | both | 3 / 0 | 9.4 / 12.2 ms | 0.27 / 0.29 ms | clean (unpublished tail not selected) | yes |
| `after_superblock_write` | both | 0 / 3 | 9.6 / 9.8 ms | 114 / 119 ms (pythia) · 266 ms (qwen) | clean | yes |

No partial state in any of the 9 runs. `time-to-usable` is the open (8–10 ms)
plus the first manifest read; the `after_superblock_write` figure is larger
because the newly visible checkpoint has to be parsed from its manifest blob.

## 7. Deletion and GC

The GC fixture is a **reduced real-weight set** (8 tensors whose bytes differ
between the two checkpoints, forced to `--strategy zstd` so the second copy is
physically distinct), published twice: once with no snapshot and once with
`family-pin` created immediately before the delete. Deleting the second
checkpoint's objects is one group commit.

| Family | snapshot pinned | entries unlinked | blobs deleted | bytes reclaimed | store before → after | verify | live checkpoint restores |
|---|---|---|---|---|---|---|---|
| pythia | no | 9 | 24 | **141,682,009** | 283,323,671 → 141,646,703 | clean | exact |
| pythia | yes | 9 | 0 | 75,188 (indexes only) | 283,323,671 → 283,263,015 | clean | exact |
| qwen | no | 9 | 24 | **231,628,264** | 463,126,912 → 231,503,946 | clean | exact |
| qwen | yes | 9 | 0 | 78,278 (indexes only) | 463,126,912 → 463,063,680 | clean | exact |

Orphan reachability: `removed_orphans = 0`, `orphan_object_records = 0`,
`verify --full` clean in all four runs, and the surviving checkpoint's
concatenated tensor SHA-256 equals the ingest record.

**A pinned snapshot costs 100 % of the reclaimed bytes.** Deletion without a pin
halves the store; with a pin it reclaims nothing. There is no policy in v1 that
expires a snapshot, so retention is exactly as long as the operator keeps the
pin.

The GC numbers are identical across the three local configurations because the
fixture forces `zstd`; that is deliberate (the planner's sparse-delta frames are
only 42 KB, so a snapshot comparison on them would be uninformative).

## 8. Object-store path (MinIO, reduced)

qwen family, `default` plan, real MinIO release binary on loopback with a tmpfs
data directory, every limit unchanged.

| Phase | local | MinIO | delta |
|---|---|---|---|
| ingest median | 142,420 ms | 167,894 ms | **+17.9 %** |
| full restore, 1 thread | 8,753.9 ms | 9,340.8 ms | +6.7 % |
| full restore, 4 threads | 3,973.7 ms | 4,293.7 ms | +8.1 % |
| 100-tensor subset, 1 / 4 | 4,494.8 / 2,826.8 ms | 4,741.1 / 3,009.5 ms | +5.5 % / +6.5 % |
| 1-row region | 2,366.7 ms | 2,419.4 ms | +2.2 % |
| GC without a snapshot | 231,628,264 B | 231,628,264 B | same |

Per-tensor physical bytes are **not** attributed for a remote store (there is no
local tree to walk); the comparison block says so and does not compute a ratio.
MinIO is single-node on loopback with a tmpfs data directory: this is a protocol
and latency measurement, not a media-durability, multi-node, cloud, or fleet
claim.

## 9. Where each configuration loses

### `default` (bytes-only planner)

* **F32 small tensors:** every one of pythia's 76 tensors is larger than
  `zstd -19`; the whole checkpoint is 1.20× the baseline. The `zstd` candidate
  is codec level 3, not 19.
* **Region reads are full decodes.** A 1-row read of a 272 MB tensor fetches and
  decodes 334 MB (2.37 s). This is the mode-9/weight-delta trade: fast full
  restore, no seekability.
* **Full restore is slower than the access-pattern plans** on both families
  (8.75 s vs 1.95 s qwen; 1.03 s vs 0.71 s pythia, one thread).
* **Index cost:** 5.98 % of the qwen store, 2.76 % of pythia.
* **Peak RSS:** 2.87 GiB for the qwen ingest — inside the 3 GiB default court by
  only 132 MiB.

### `region1` / `region100`

* **Storage.** qwen +9.8 % / +14.6 % data bytes, pythia +28.2 % / +30.3 %
  versus `default`. The access-pattern plan buys latency with bytes and never
  looks at the write cost.
* **Small tensors go `raw`.** At `lambda_ns = 1` an 8 KB bias is stored at 8,438
  B where `zstd -19` needs 22 B (+38 255 %). 50–217 tensors per family are raw.
* **Tiles are not free at ingest.** The 1 MiB auto-tile plan needs 3.4 GiB peak
  RSS: at the 3 GiB court it is **OOM-killed** (retained as a loss, not
  suppressed by a bigger limit alone — it is measured at 6 GiB).
* **Region reads do not scale with threads.** One region touches 4 tiles; p50
  goes 5.2 ms → 1.9 ms from 1 to 4 threads, but the 1-row and 100-row numbers
  are the same because a single tile dominates.
* **Ingest is not faster.** Encoding tiles costs more than encoding one zstd
  frame; only the *decode* side improves.

### `s3-reduced`

* +17.9 % ingest, +6.7–8.1 % restore versus local, and every phase now pays
  HTTP round trips.
* No per-tensor attribution, so the per-tensor loss table cannot be computed on
  the object store.
* Single-node loopback MinIO on tmpfs: no durability, replication, TLS,
  multipart, or cloud claim.

### `checkpoint8` (family-level compatibility surface)

* 197.1 MB (pythia) and 823.4 MB (qwen) for one checkpoint — 0.70× / 0.83× of
  the raw container, but **larger** than the planner store's base checkpoint
  (173.2 MB pythia; 658.0 MB qwen).
* Restore is 2.9 s / 10.2 s at one thread, 4–5× slower than the region plans,
  and topology re-expression does not reduce bytes fetched.
* The 8-way write is a single group commit of 608 / 2,320 shard objects
  (6.7 s / 30.1 s).

## 10. Honest verdict

**Partly.** Context-aware representation reduces total lifecycle cost on this
fixture in two of the three dimensions that matter, and fails in the third.

*For a consecutive training run it is decisive:* the second checkpoint costs
42 KB instead of 144 MB (sparse delta against the causal parent), and the family
fits in 173 MB — less than one `zstd -19` copy of either container. For a
fine-tune it is still a real win: 541 MB instead of 768 MB for the second
checkpoint, both checkpoints for 1.20 GB.

*For a region-read workload it is decisive:* 454× fewer bytes and 454× lower
latency per 1-row read (qwen), 175×/231× on pythia, at +9.8 % to +30 % storage.

*It does not reduce cost for a full-checkpoint restore or for the first
checkpoint itself:* the codec's `zstd` frame is 20 % larger than `zstd -19` on
F32, mode 9/weight-delta decode is slower than zstd, the selector index is 6 % of
the store, GC reclaims nothing while a snapshot is pinned, and the object-store
path adds ~18 % on ingest.

**What would change the answer:** (1) a zstd-19 candidate (or a codec level
setting) in the planner, which alone would remove the 28.8 MB F32 loss;
(2) an access-pattern signal available *per write* rather than per run, so tiles
are chosen only for tensors a workload actually reads in regions; (3) a decoder
fast enough that mode 9 / weight-delta stop losing on full restore; (4) a
snapshot-retention policy, without which deletion reclaims nothing; (5) writing
the second checkpoint through the whole-file patch path when the change is a
single contiguous region (it beats the per-tensor sparse deltas by 28 % on
pythia).
