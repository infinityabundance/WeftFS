# R10 — throughput: phase profile, batched publication, pipelined restore

Subphase R10 answers one measured question: **where does the time actually go**
in ingest, save, and restore, and what happens when the two dominant costs —
publication and (for the planner) encoding — are attacked directly?

Every number below comes from a receipt written by a phase that ran inside the
OOM-protected bench court. Nothing is estimated, and every loss is listed.

## 0. Courts, corpora, and what was run

```sh
# r10 phase profile, pack-plane save, pipelined restore — Qwen2.5-0.5B
WEFT_THROUGHPUT_FAMILIES=qwen0.5b WEFT_THROUGHPUT_REPS=2 \
WEFT_THROUGHPUT_RANKS="0 1 8" WEFT_THROUGHPUT_SAVE_CONFIGS="raw zstd planned" \
WEFT_THROUGHPUT_RESTORE_THREADS="1 2 4 8" WEFT_THROUGHPUT_TILE_THREADS="1 4" \
  scripts/docker-court.sh bench "cd /work && bash bench/scripts/throughput_court.sh"

# the same court, Qwen2.5-3B (reduced: one save configuration, two rank counts)
WEFT_THROUGHPUT_FAMILIES=qwen3b WEFT_THROUGHPUT_REPS=1 \
WEFT_THROUGHPUT_RANKS="0 8" WEFT_THROUGHPUT_SAVE_CONFIGS="raw" \
WEFT_THROUGHPUT_RESTORE_THREADS="1 2 4 8" WEFT_THROUGHPUT_TILE_THREADS="1" \
  scripts/docker-court.sh bench "cd /work && bash bench/scripts/throughput_court.sh"
```

| | |
|---|---|
| Courts | `weft/bench-court:1` `sha256:4c6c41fb…f66501559`, kernel 7.3.3-1-cachyos |
| Limits | `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4 --tmpfs /tmp:size=1g --cap-drop=ALL --security-opt no-new-privileges` (unchanged defaults; no run raised a limit) |
| 0.5B receipt | `bench/receipts/throughput-20261110T134917Z.json` (+ `.log`) |
| 3B receipt | `bench/receipts/throughput-20261110T145341Z.json` (+ `.log`) |
| Corpora | `Qwen2.5-0.5B.safetensors` 988,097,824 B `88c14255…0ed342`; `Qwen2.5-3B.safetensors` 6,171,927,112 B `52da3d99…d1917ab` — both re-hashed by the court before any measurement |
| Labels | single host, warm page cache, one run per cell unless the receipt says otherwise; **not** a fleet, media, power-loss, or cloud claim |

Phase naming in every table: `source_read`, `digest`, `encode`, `blob_publish`,
`manifest_commit`, `index_segment`, `verify`. They are wall-clock sums across
every thread, so a phase total can exceed wall time when the prepare path runs
it concurrently, and they are **not** a partition: chunk-table construction,
planner cost arithmetic, in-memory state application and harness bookkeeping
are outside every section. The residual is reported explicitly where it matters.

## 1. Increment 1 — the phase breakdown (before any optimisation)

### 1a. Family ingest, planned writer — **encode dominates**

`family_lifecycle ingest --reps 1|2 --no-snapshot`, `WEFT_PHASE_PROFILE=1`.
Logical bytes: 0.5B = 988,065,536 per checkpoint × 2 (base + next); 3B =
6,171,877,376 × 2.

| phase | 0.5B total ms | 0.5B share | 3B total ms | 3B share |
|---|---:|---:|---:|---:|
| **encode** | **434,347.1** | **81.4 %** | **865,052.9** | **89.8 %** |
| blob_publish | 25,455.2 | 4.8 % | 19,480.9 | 2.0 % |
| manifest_commit | 15,876.4 | 3.0 % | 6,854.2 | 0.7 % |
| index_segment | 12,713.9 | 2.4 % | 7,184.4 | 0.7 % |
| source_read | 300.9 | 0.06 % | 11,038.7 | 1.1 % |
| digest | 511.7 | 0.10 % | 1,617.7 | 0.2 % |
| verify (outside the ingest wall) | 7,865.0 | — | 53,805.5 | — |
| **ingest wall** | 266,775.8 (median of 2 reps) | | 920,052.4 | |

Sections: 0.5B 1,160 `encode` sections (868 per checkpoint pair × 2 reps) with
p50 20.3 ms / p95 1,330.8 ms / max 18,948.1 ms; 3B 868 sections with p50
142.7 ms / p95 3,933.1 ms / max 4,532.0 ms. The 3B effective rate is
**≈ 13.4 MB/s** (12.34 GB of tensor bytes in 920 s), matching the operator's
measured 12.6 MB/s.

Phase totals sum to 963.8 s for 3B against 920.1 s of ingest wall; verify
(53.8 s) runs after the measured wall, so the accounted ingest time is 910.0 s
of 920.1 s = **98.9 %**. For 0.5B the 2-rep run sums to 496.1 s against
533.6 s of wall for two families (93.0 %).

**Conclusion 1 (measured, not guessed): for the planned family ingest the cost
is encoding, not publication and not I/O.** `blob_publish` is 2.0 % of the 3B
run at 2,177 sections (p50 2.3 ms); `digest` 0.2 %; `source_read` 1.1 %.

### 1b. Checkpoint save, fast raw, 8-way — **publication dominates**

`family_lifecycle checkpoint --shards 8 --axis 0 --strategy tensor-sharded
--write-policy fast --fast-codec raw`, `WEFT_PHASE_PROFILE=1`, 3,472 shard
objects, 6,171,877,376 element bytes.

| phase | 3B total ms | share | sections | p50 ms | max ms |
|---|---:|---:|---:|---:|---:|
| **blob_publish** | **771,452.7** | **96.6 %** | 99,103 | 2.322 | 977.0 |
| source_read | 6,328.2 | 0.8 % | 3,906 | 0.010 | 560.2 |
| digest | 2,887.6 | 0.4 % | 103,009 | 0.013 | 104.1 |
| index_segment | 29.1 | <0.01 % | 2 | 24.9 | 24.9 |
| manifest_commit | 15.8 | <0.01 % | 1 | 15.8 | 15.8 |
| encode | 7.5 | <0.01 % | 3,472 | 0.002 | 0.0 |
| **wall** | **798,331.8** | | | | |

**Conclusion 2: a fast-raw checkpoint save is 99,103 small blob publications
(96.6 % of wall) and 7.5 ms of encoding.** The measured 2.5 s of CPU against
55 s of wall for a 988 MB save is this: each 64 KiB chunk is one create-once
publication, and the local plane fsyncs each one. The same contour at 0.5B:
19,170 publications, 137.6 s of 139.0 s wall (99.0 %).

## 2. Increment 2 — batched durable publication through the pack plane

Small blobs (chunks and manifests at or under the 64 KiB member threshold) are
grouped into content-addressed packs and named by a compare-and-swap catalog.
Only physical placement changes.

### 2a. The parallel path needed a real design change

The first implementation let each blob worker seal packs and publish catalog
roots. `tests/checkpoint_packs.rs` caught it failing on a real model: a local
catalog compare-and-swap is atomic only under the writer lock, which blob
workers deliberately do not hold, so a racing seal silently lost entries.

Workers now run in **deferred catalog** mode: a full pack is published
immediately as an immutable, content-addressed object (safe to publish
concurrently, exactly as before) and its members are only *recorded*. The single
committer merges every worker's members into **one** catalog publication, so no
worker writes a control record and the single-writer proof is untouched.

A second real defect fell out of the same test: the catalog merge detected a
member packed by two workers (identical bytes, common with duplicate chunks) and
then **dropped** it instead of keeping the existing entry. Fixed; a key with
*identical* bytes is idempotent and a key with different bytes still fails
closed.

### 2b. Identity is unchanged

`tests/checkpoint_packs.rs` (5/5) proves, for a fixed clock and deterministic
seed: identical head, identical checkpoint manifest **bytes**, and identical
manifest root for flat vs packed, sequential vs prepare/commit at ranks 1/2/4,
and for 1/2/4/8 ranks with heavy content dedup. Every tensor restores
byte-identically and `verify --full` is clean.

### 2c. What it measures (fast raw, the publication pathology)

0.5B, best of 2, `--pack-compare` in one receipt:

| ranks | flat wall ms | packed wall ms | speedup | flat publishes | packed publishes* | store bytes | peak RSS KiB |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 (seq) | 108,889.2 | 29,008.0 | **3.75×** | 38,346 | 21,988 | 992.5 MB → 1,039.2 MB | 38,744 → 71,720 |
| 1 | 109,617.6 | 3,918.6 | 27.97× | 26* | 544 | 992.5 → 994.8 MB | 324,640 → 357,240 |
| 8 | 22,608.8 | 7,353.9 | **3.07×** | 26* | 4,046 | 992.5 → 1,003.7 MB | 370,508 → 528,708 |

3B, one rep:

| ranks | flat wall ms | packed wall ms | speedup | store bytes | peak RSS KiB |
|---:|---:|---:|---:|---:|---:|
| 0 (seq) | 800,031.6 | 507,126.8 | **1.58×** | 6,182.1 MB → 6,941.5 MB (+12.3 %) | 82,880 → 114,620 |
| 8 | 412,384.2 | 145,692.7 | **2.83×** | 6,182.1 → 6,234.7 MB (+0.85 %) | 794,416 → 1,042,832 |

\* **Request counts for the prepare/commit split are committer-only.** Each rank
owns its own backend, and the child reports the committer handle's counters, so
the split rows under-report publications. The sequential rows are complete.
The honest request evidence for the split is the *wall time* plus the
publication phase, not this counter.

Where packing *loses* (same receipts, same corpora):

| config | ranks | flat | packed | result |
|---|---:|---:|---:|---|
| fast zstd-1, 0.5B | 0 | 13,758.7 ms | 5,844.0 ms | 2.35× win |
| fast zstd-1, 0.5B | 1 | 16,330.5 ms | 24,401.8 ms | **0.67× loss** |
| fast zstd-1, 0.5B | 8 | 6,409.8 ms | 8,283.3 ms | **0.77× loss** |
| planned, 0.5B | 8 | 18,674.0 ms | 22,011.7 ms | **0.85× loss** |

With zstd-1 frames most bytes are already `> 64 KiB` and stay flat; what remains
is manifests and a little catalog, so packing adds catalog work and sealing for
little benefit. **Packing is a win exactly where publications are small and
numerous (fast raw), and a loss when frames are large (zstd-1) or when the
planner already produces few objects.** The court has to select it, and the
default stays off.

Costs published in full:

* **Footprint.** 0.5B fast-raw packed store is +4.7 % (catalog 19,171 entries at
  113 B plus superseded segments); 3B packed sequential is **+12.3 %** because
  the sequential path republished **46,069 catalog segments** for 278 seals
  (≈ 740 MB of superseded, unreferenced segment objects, reclaimed only by GC).
  The 8-rank deferred path published 2,049 segments for 8 catalog commits and
  cost +0.85 %.
* **Catalog bound.** The default 8 MiB catalog holds 74,235 members. The 3B
  fast-raw save publishes 99,106 small blobs, so 24,870 members (1.54 GB) stayed
  **flat** and the run recorded `packing_disabled: true`. Fallback, never
  failure — but a 3B save needs `--pack-catalog-bytes` above 8 MiB to pack
  everything. The bound is also enforced per handle during prepare, so the final
  adopted catalog can exceed it.
* **Cold read amplification (measured in
  `tests/checkpoint_packs.rs::cold_read_of_a_packed_member_costs_more_requests`).**
  One packed member costs the catalog root + one segment + one ranged pack read
  instead of one `GET`.
* **Peak RSS** grows with packs: 0.5B ranks-8 370 → 529 MB (+43 %), 3B ranks-8
  794 MB → 1,043 MB (+31 %).

## 3. Increment 3 — parallel restore end to end

`Store::restore_checkpoint_pipelined` drives the shipped ordered pipeline:
bounded workers across tensors, a bounded in-flight byte budget, exact container
order, and per-tensor tile threads. `tests/checkpoint_restore_pipeline.rs` (4/4)
proves byte-identical output at 1/2/4/8 workers × 1/2 tile threads, single-tensor
and region parity with the sequential path, and tiled (mode 15) shards.

Measurement: full-checkpoint restore into a **SHA-256 hashing sink** — decode +
backend read + hash, no host writeback. The container SHA-256 must equal the
checkpoint's recorded source SHA-256 (it did, every cell).

0.5B, 988,065,536 logical bytes, 290 tensors, largest tensor 272,269,312 B
(`model.embed_tokens.weight`), Amdahl cap = total/largest = **3.63×**:

| threads | tiles | p50 ms | p95 ms | p99 ms | MB/s | backend bytes | requests |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1 | 1,978.3 | 1,978.3 | 1,978.3 | 503.0 | 992,589,671 | 23,810 |
| 2 | 1 | **1,434.7** | 1,434.7 | 1,434.7 | **695.5** | 992,589,671 | 23,810 |
| 4 | 1 | 1,479.6 | 1,479.6 | 1,479.6 | 670.4 | 992,589,671 | 23,810 |
| 8 | 1 | 1,459.7 | 1,459.7 | 1,459.7 | 677.1 | 992,589,671 | 23,810 |
| 1 | 4 | 1,963.6 | 1,963.6 | 1,963.6 | 503.5 | 992,589,671 | 23,810 |
| 8 | 4 | 1,474.9 | 1,474.9 | 1,474.9 | 673.1 | 992,589,671 | 23,810 |

3B, 6,171,877,376 logical bytes, 434 tensors, largest 622,329,856 B, Amdahl cap
**9.92×**:

| threads | p50 ms | p95 ms | p99 ms | MB/s | backend bytes | requests |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 31,670.1 | 31,670.1 | 31,670.1 | 194.9 | 6,189,430,488 | 106,047 |
| 2 | 26,737.9 | 26,737.9 | 26,737.9 | 230.8 | 6,189,430,488 | 106,047 |
| 4 | 26,432.6 | 26,432.6 | 26,432.6 | 233.5 | 6,189,430,488 | 106,047 |
| 8 | **26,238.8** | 26,238.8 | 26,238.8 | **235.2** | 6,189,430,488 | 106,047 |

Restore is exact every time: `sha256_match: true`, `exact_source_file: true`,
`decode_bytes == logical_bytes` (no amplification), and peak RSS 1,534,292 KiB for
the 3B restore.

**The honest result: 1.21× at 8 workers on 3B and 1.36× at 2 workers on 0.5B,
against a 9.92× / 3.63× Amdahl cap.** The cap is not what binds. What binds is
that every blob read goes through the store's single backend mutex and each
tensor's shards are decoded on one thread. Decode work is outside the lock, so
some parallelism is realised — but the read path is serialized. This is the next
measured bottleneck, stated, not fixed.

## 4. Increment 4 — streaming encode: **not implemented, with the evidence**

Increment 1 shows encode dominating (89.8 % of the 3B planned ingest), so the
conditional in the task is met. What the profile also shows is *where* inside
`encode` the time goes: the store-side buffering is already streamed — the
checkpoint writer reads and encodes **one shard at a time**
(`read_tensor_shard`, proven byte-identical by
`tests/checkpoint_streaming.rs`), and the restore decodes tiles on a bounded
pool. The 865 s of 3B encode is codec work inside the candidate encoders
(mode 9 `FLOAT_TENSOR` on the large matrices: p95 3.9 s, max 4.5 s per tensor;
mode 10, mode 15 and zstd-19 candidates likewise), not bytes held or copied by
the store. A store-level streaming path cannot remove that work; a real fix is a
streaming mode-9/mode-15 encoder inside WeftCodec, which is a codec change with
its own conformance obligations and is **out of this subphase**.

I did not implement a streaming path that would have measured as a win only by
changing what the planner selects. That would violate the exactness rule.

## 5. Increment 5 — what is still serial, and every loss

Still serial:

* **One committer.** All state publication is one commit; the prepare/commit
  split parallelizes only immutable blob publication.
* **One backend mutex.** Every read and write of a store handle serializes on
  one `Mutex`; parallel restore and multi-worker prepare both contend on it.
* **One decode thread per tensor.** A tensor's shards are decoded in sequence
  unless its representation is tiled; the 3B restore's per-tensor work is
  serial within a task.
* **The largest tensor.** 593.5 MiB of the 3B family (10.1 %); the Amdahl cap is
  9.92×, and a single-tensor restore of it measures 3.70 s at p50.
* **Sequential packed savers.** The sequential path seals per 16 MiB pack and
  republishes the touched catalog segments each time (46,069 segment publishes
  on the 3B save); only the prepare/commit path defers the catalog. Making the
  sequential group commit defer as well is the identified next step.

Losses, all measured above: packed 0.67×/0.77×/0.85× on zstd-1 and planned
saves; +12.3 % footprint on the sequential 3B packed save; catalog-bound
fallback leaves 1.54 GB flat on the 3B save; +31-43 % peak RSS with packs; restore
speedup 1.21× against a 9.92× cap; tile threads are within noise for
non-tiled checkpoints; the split-path request counter is committer-only.

Not claimed: fleet, multi-node, physical power loss/host cache, RAPL energy,
GPU, cold media, Go parity for any of these three increments (the Go store has
no checkpoint writer, pack deferral, or pipelined checkpoint restore), or a
cloud run.

## 6. Verification

| gate | result |
|---|---|
| Rust `cargo test --all-targets` | **736 passed / 0 failed / 0 ignored**, 150 suites |
| FUSE `cargo test --features fuse --all-targets` | **779 passed / 0 failed / 2 pre-existing ignored**, 150 suites |
| Go `go test -race ./...` | `store` ok (136.5 s), `cmd/weftfs` ok, `fusefs` ok, `index` ok, `model` ok |
| `scripts/check-claims.sh` | **808 / 808** |
| `scripts/check-spec-drift.sh` | no spec changed in R10 (unchanged) |
| conformance manifest / coverage | unchanged (no wire-format change; 420 entries) |
| new tests | `tests/checkpoint_packs.rs` 5/5, `tests/checkpoint_restore_pipeline.rs` 4/4, `src/phase_profile.rs` 4/4, `tests/packs.rs` 13/13 |
| aggregate | `bench/receipts/verify-20261110T181259Z.json` — **42/42** courts pass, 0 failed, 0 skipped |

`bench/scripts/throughput_court.sh` is registered in `scripts/verify-all.sh` in a
reduced configuration (`qwen0.5b`, one save configuration, one rank count, a
40-tensor ingest prefix, two restore thread counts); the full-size receipts
above are the standalone runs. Two aggregate runs are retained because they
found real defects in this change: `verify-20261110T160409Z.json` (40/42 — the
new court was invoked on the host instead of in the bench court, plus a
`rust_fuse` failure that did not reproduce standalone) and
`verify-20261110T170837Z.json` (41/42 — a `local` declaration outside a function
in the new court script).
