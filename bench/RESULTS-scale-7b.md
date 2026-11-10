# R12 increment 1 — 7B scale

**Corpus: the real Qwen/Qwen2.5-7B-Instruct checkpoint.** Not synthetic.

The hub publishes four `model-0000N-of-00004.safetensors` shards. They were
fetched through `bench/scripts/real_model_fetch.py` (which verifies each file
against the LFS pointer's size and SHA-256) and merged into one container by
`bench/scripts/safetensors_merge.py`, which verifies every tensor's SHA-256
against the shard it came from before renaming the output.

| | |
|---|---|
| file | `Qwen2.5-7B-Instruct.safetensors` |
| size | 15,231,272,088 B (15.23 GB) |
| SHA-256 | `5527ae977f6b2a2fc0add1d76834f1300a607f79aa4bdce1457bd738523c1ef1` |
| tensors | 339 (BF16) |
| largest tensor | `model.embed_tokens.weight`, 1,089,994,752 B (1.015 GiB) |
| element bytes | 15,231,233,024 |

## Court

`scripts/docker-court.sh bench`, unchanged hard limits: `--memory=3g
--memory-swap=3g --pids-limit=1024 --cpus=4 --tmpfs /tmp:size=1g
--cap-drop=ALL --security-opt no-new-privileges`, image `weft/bench-court:1`
`sha256:4c6c41fb…f66501559`, kernel 7.3.3-1-cachyos. **No limit was raised.**

Every phase runs in its own container, so peak RSS is per process. The court
deletes the ingest store before the checkpoint stores and the flat store before
the packed one; the widest overlap on disk is the 15.23 GB source plus one
store.

Receipt: `bench/receipts/scale-7b-20261110T213731Z.json` (+ `.log`).

## Result 1 — the per-tensor ingest path does not fit; exact failure recorded

`scale_7b ingest` reads one whole tensor and writes it through the shipped
per-tensor planner (`write_tensor_file_with_spec`). It is the path S12's
streaming work does **not** cover, because the planner needs the whole tensor.

```
PROGRESS ingest-begin 0 model.embed_tokens.weight bytes=1089994752 rss_hwm_kib=6284
bash: line 1:     8 Killed  …/scale_7b ingest …
```

* exit **137** (cgroup OOM kill),
* cgroup `memory.peak` **3,221,225,472 B** — the 3 GiB limit exactly,
* the store was left with 10,096 B (nothing published); `verify --full` on the
  empty store is ok.

The first tensor is the 1.015 GiB embedding. Holding it plus the planner's
encoded candidate needs more than 3 GiB. The limit was **not** raised to hide
it, and there is no number to report for this phase beyond the failure.

## Result 2 — the bounded/streaming checkpoint writer fits, flat and packed

`scale_7b checkpoint --shards 8 --axis 0 --write-policy fast --fast-codec raw
--max-ingest-bytes 1610612736`. The writer streams the source in fixed windows,
reads one shard at a time, and bounds a tensor (or one shard) at the stated
bound; 1.5 GiB is above the 1.015 GiB embedding.

| phase | wall | MB/s | peak RSS | store bytes | vs element bytes |
|---|---:|---:|---:|---:|---:|
| flat | **1,406,533.6 ms** (23.4 min) | **10.83** | 173,032 KiB (169 MiB) | 15,247,327,175 | **+0.98 %** |
| packed | **1,088,654.1 ms** (18.1 min) | **13.99** | 287,420 KiB (281 MiB) | 23,023,484,778 | **+51.16 %** |

Both wrote 339 tensors × 8 shards = **2,712 shard objects** and 233,539 chunks,
and both `verify --full` clean (2,713 objects, 2,713 manifests, 233,539 chunks,
0 problems).

* Flat took **236,263 backend writes** (one durable publication per 64 KiB
  chunk); packed took **150,542** — 36 % fewer — because 236,237 members were
  sealed into **911 packs**.
* The packed store's footprint is a real loss: the sequential writer republished
  **148,694 catalog segments** for those 911 seals. R10 measured the same
  pathology at 3B (+12.3 %); at 7B it is +51.2 %.
* `verify --full` cost 48,790.7 ms on the flat store and **21,543.7 ms** on the
  packed one (fewer physical objects to open).

## Result 3 — full restore at 1/4/8 workers

Restores read the **packed** store and write into a SHA-256 sink (no host
writeback). The container digest must equal the source file's.

| workers | wall | MB/s | backend bytes read | peak RSS |
|---:|---:|---:|---:|---:|
| 1 | 45,101.8 ms | 337.7 | 15,268,572,464 | 2,230,164 KiB |
| 4 | **41,447.6 ms** | **367.5** | 15,268,572,464 | 2,406,596 KiB |
| 8 | 41,415.8 ms | 367.8 | 15,268,572,464 | 2,360,764 KiB |

`sha256_match: true`, `exact_source_file: true` in every cell; 2,712 shards
read; 483,280 store reads.

**The parallel win is 1.089× (1 → 4) and 1.089× (1 → 8) against an Amdahl cap
of 13.97×** (total bytes / largest tensor). The binding limits are the same two
R10 identified: one backend mutex in the read path and one decode thread per
tensor. At 7B the largest tensor is 1.015 GiB, so it also sets a floor: restore
peak RSS is 2.2-2.4 GiB inside a 3 GiB court.

## Extrapolation to 70B — explicitly an extrapolation

70B parameters at BF16 is ≈ 140 GB of element bytes. At the measured
throughput (not measured at 70B):

| phase | measured rate | 70B projection |
|---|---:|---:|
| flat checkpoint write | 10.83 MB/s | ≈ 12,900 s ≈ **3.6 h** |
| packed checkpoint write | 13.99 MB/s | ≈ 10,000 s ≈ **2.8 h** |
| restore, 8 workers | 367.8 MB/s | ≈ 381 s ≈ **6.4 min** |

These are linear projections from one measured rate. They ignore the packed
catalog bound, the 2.5 GB embedding of a 72B model (which would exceed the
restore memory profile seen here), and any storage-media effect. The per-tensor
ingest cannot be projected at all: it already fails at 7B.

## Every loss

1. **Planned per-tensor ingest OOMs at 7B** (3 GiB court). Only the streaming
   checkpoint writer is usable at this scale.
2. **Packed sequential write costs +51.2 % footprint and 281 MiB RSS** versus
   169 MiB flat: 148,694 catalog-segment publishes for 911 seals.
3. **Restore parallelism is 1.089×**, not the 13.97× cap.
4. **The cgroup memory peak is pinned at the 3 GiB limit in every write and
   restore phase** (page cache counts toward the cgroup). Process `VmHWM` is
   169-281 MiB for the writes and 2.2-2.4 GiB for restores. Both numbers are
   reported; the cgroup one is not a claim about process memory.
5. Rust only: no Go parity for `scale_7b`, the streaming writer bound, or the
   pipelined restore. No physical power-loss, media, fleet or cloud claim.
6. Single host, warm page cache, one run per cell.

## Retained failing evidence

`bench/receipts/scale-7b-20261110T204331Z.json` is retained. Its flat write and
verify passed, but `verify_packed` and the restores failed because the court
re-opened the packed store **without** `--pack`, so the verifier looked for flat
manifest keys a packed store does not have (`missing blob
manifests/5e/5edb…`). That is a court configuration bug, not a store defect;
the run above is the corrected court at the same revision. The packed store from
that run was deleted before it could be re-verified.

## Reproduce

```
bash bench/scripts/scale_7b_court.sh
# environment: WEFT_SCALE_PHASES="flat packed restore_1 restore_4 restore_8"
# selects phases; WEFT_SCALE_MEMORY overrides the court memory (recorded).
```
