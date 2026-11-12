# Performance numbers — generated from receipts

Every performance, storage, latency, throughput, resource and
verification-count number that this repository publishes as a **current claim**
is emitted by `scripts/perf_numbers.py` from
[`docs/perf-numbers.json`](perf-numbers.json). The generator rewrites only the
text between `BEGIN GENERATED:<block-id>` and `END GENERATED:<block-id>`
comments; everything else in a document is untouched.

**Never edit a number inside a generated region.** Change the receipt (or add
one) and re-run the generator. A hand-edited region is a defect. A number
written by hand outside a region is also a defect unless it is a historical
quotation that names its receipt stamp and revision — the ungenerated parts of
`docs/STATUS.md` and the older release notes are exactly that.

**What `--check` enforces** (it is the `perf_numbers` court, and it exits
non-zero on any of these):

1. every generated region matches its rendered value exactly;
2. every referenced receipt exists;
3. every declared `stamp` equals the value the receipt actually carries, and
   every declared `revision` equals the revision the receipt actually records —
   so a re-run that produces a new stamp fails the check until the manifest is
   re-pinned deliberately;
4. every manifest number is printed by at least one block (no orphans), and
   every `BEGIN GENERATED` marker in the tree has a manifest block (no unknown
   regions).

`scripts/perf_numbers.py --list` prints the full mapping: key, label, format,
unit, receipt, JSON path and stamp/revision, for audit.

**Derived numbers.** A row whose source is an `expr` is arithmetic over other
rows in this table (`write.ratio`, `write.reduction_pct`, `family.zipnn_delta_pct`,
and the like). It inherits their provenance; the expression is in the manifest,
so nothing is hidden.

**Historical quotations.** Numbers that a document deliberately retains from an
earlier release are listed under "Historical quotations" below *with their
receipt, stamp and revision*. They are generated too, so they cannot silently
drift, but they are labelled as superseded.

## Compressed write (R17–R22)

The fixture is the frozen 101-tensor Qwen2.5-0.5B-Instruct subset
on the local POSIX backend, 4 CPUs,
3 GiB court. `A` is the same backend's native per-tensor write; `C-pipe4` is the
shipped planned write. Receipt
`bench/receipts/r22-pareto-20261112T230000Z.json`.

<!-- BEGIN GENERATED:perf-catalog-write (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `write.r22.min_ms` | 592.6 | ms | R22 durable write wall, least-contended | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.med_ms` | 600.9 | ms | R22 durable write wall, median of 7 | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.footprint_b` | 282,106,049 | B | R22 reachable physical footprint | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.written_b` | 282,118,555 | B | Bytes handed to the backend | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.requests` | 216 | requests | Backend write requests | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.cpu_ms` | 1,150 | ms | R22 CPU (user+sys) | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.rss_kib` | 939,256 | KiB | R22 peak RSS of the least-contended rep | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.restore_ms` | 1,401.7 | ms | R22 cold restore of the fixture | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.encode_wall_ms` | 410.6 | ms | Pipeline encode wall | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.publish_wall_ms` | 452.5 | ms | Pipeline publication wall | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.publish_busy_ms` | 391.2 | ms | Pipeline publisher busy | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.barrier_ms` | 461.6 | ms | Early-barrier thread busy | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.barrier_files` | 202 | files | Files fsynced by the early-barrier thread | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.encode_ms` | 614.8 | ms | Encode time summed over threads | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.encode_zstd_ms` | 319.9 | ms | Level-3 ZSTD encode | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.encode_mode9_ms` | 229.4 | ms | Mode-9 encode | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.digest_ms` | 74.7 | ms | Digest time | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.analyze_ms` | 156.9 | ms | Analyse time | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.a.min_ms` | 332.0 | ms | Native per-tensor control A, least-contended | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.a.med_ms` | 339.7 | ms | Native per-tensor control A, median | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.a.footprint_b` | 424,439,296 | B | Native control A footprint | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.a.cpu_ms` | 100 | ms | Native control A CPU | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.a.rss_kib` | 270,532 | KiB | Native control A peak RSS | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.a.restore_ms` | 347.7 | ms | Native control A cold restore | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.t19off_min_ms` | 575.9 | ms | C-pipe4 with the level-19 gate disabled | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.zpar4_min_ms` | 584.8 | ms | C-pipe4 with chunk-parallel ZSTD on every tensor | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.tm9off_min_ms` | 593.6 | ms | C-pipe4 with mode 9 disabled | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.tm9off_footprint_b` | 330,355,860 | B | C-pipe4 with mode 9 disabled, footprint | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.pack_min_ms` | 962.7 | ms | C-pipe4 with 1 MiB packs | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.pack_footprint_b` | 282,102,473 | B | C-pipe4 with 1 MiB packs, footprint | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.pack_publish_busy_ms` | 219.5 | ms | C-pipe4 with 1 MiB packs, publisher busy | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.znochunk_min_ms` | 3,518.1 | ms | C-pipe4 with no codec chunking | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.leeway4_min_ms` | 2,552.1 | ms | C-pipe4 with a 4 MiB level-19 gate leeway | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.chunk16k_min_ms` | 671.7 | ms | C-pipe4 with 16 KiB codec chunks | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.pipe2_min_ms` | 926.5 | ms | Pipeline at 2 workers | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.pipe8_min_ms` | 648.7 | ms | Pipeline at 8 workers | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.tokens1_min_ms` | 912.8 | ms | Pipeline with 1 CPU token | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.tokens2_min_ms` | 959.8 | ms | Pipeline with 2 CPU tokens | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.tokens4_min_ms` | 612.1 | ms | Pipeline with 4 CPU tokens | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.sync16_min_ms` | 582.0 | ms | Pipeline with a 16 MiB sync granularity | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.sync32_min_ms` | 627.9 | ms | Pipeline with a 32 MiB sync granularity | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.seq_min_ms` | 1,004.4 | ms | Sequential writer C | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.legacy_min_ms` | 6,583.8 | ms | Exhaustive planner C-legacy | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.ratio` | 1.78 | x | R22 wall as a multiple of native A | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.reduction_pct` | 33.5 | % | Footprint reduction against native A | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.speedup_r17` | 40.9 | x | Speed-up from R17 to R22 | `bench/receipts/write-cost-ablation-20261112T052203Z.json, bench/receipts/r22-pareto-20261112T230000Z.json` | mixed | mixed |
| `write.r22.znochunk_footprint_b` | 282,103,649 | B | C-pipe4 with no codec chunking, footprint | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r17_zstd19_share_pct` | 84.0 | % | R17 level-19 encode share of the wall | `bench/receipts/write-cost-ablation-20261112T052203Z.json` | 20261112T054124Z | d1893e1abd05 |
| `write.r22.znochunk_saved_b` | 2,400 | B | Bytes saved by dropping codec chunking | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.leeway_ratio` | 4.3 | x | Leeway-gate wall as a multiple of the default | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.chunk16k_overhead_pct` | 13.4 | % | 16 KiB chunks versus 64 KiB, wall | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.cpu_ratio` | 11.5 | x | CPU as a multiple of native | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.rss_ratio` | 3.5 | x | Peak RSS as a multiple of native | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.pack_ratio` | 1.6 | x | Pack plane wall as a multiple of the default | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.rss_court_pct` | 29.9 | % | Peak RSS as a share of the 3 GiB court | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.barrier_mbps` | 611 | MB/s | Early-barrier writeback throughput | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.a.mbps` | 1,278 | MB/s | Native control write throughput | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `pareto.configs` | 25 | configs | Configurations in the R22 sweep | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `pareto.checks` | 11 | checks | Checks passed by the R22 sweep | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `pareto.reps` | 7 | reps | Interleaved repetitions per configuration | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.fixture.logical_b` | 424,439,296 | B | Logical bytes of the 101-tensor fixture | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.fixture.tensors` | 101 | tensors | Tensors in the fixture | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `analyze.histogram_ms` | 37.1 | ms | Mode-9 exponent histogram, embedding | `bench/receipts/zstd-chunk-parallel-20261112T231500Z.json` | 20261112T204529Z | ccc6d695b114 |
| `analyze.histogram_gbps` | 7.35 | GB/s | Mode-9 exponent histogram throughput | `bench/receipts/zstd-chunk-parallel-20261112T231500Z.json` | 20261112T204529Z | ccc6d695b114 |
| `write.r22.seq_rss_kib` | 690,656 | KiB | Sequential writer C peak RSS | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `write.r22.zpar1_min_ms` | 592.2 | ms | C-pipe4 with chunk-parallel ZSTD at one thread | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
<!-- END GENERATED:perf-catalog-write -->

## The write ladder, per rung

<!-- BEGIN GENERATED:perf-catalog-ladder (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `ladder.r17_ms` | 24,229.2 | ms | R17 write wall (attribution baseline) | `bench/receipts/write-cost-ablation-20261112T052203Z.json` | 20261112T054124Z | d1893e1abd05 |
| `ladder.r17_footprint_b` | 282,283,011 | B | R17 footprint | `bench/receipts/write-cost-ablation-20261112T052203Z.json` | 20261112T054124Z | d1893e1abd05 |
| `ladder.r17_zstd19_ms` | 20,353.0 | ms | R17 level-19 ZSTD encode, selected for zero tensors | `bench/receipts/write-cost-ablation-20261112T052203Z.json` | 20261112T054124Z | d1893e1abd05 |
| `ladder.r18_ms` | 4,117.8 | ms | R18 write wall, least-contended | `bench/receipts/planner-two-stage-20261112T081151Z.json` | 20261112T081151Z | 4c6a9abc640c |
| `ladder.r18_med_ms` | 4,198.0 | ms | R18 write wall, median | `bench/receipts/planner-two-stage-20261112T081151Z.json` | 20261112T081151Z | 4c6a9abc640c |
| `ladder.r18_footprint_b` | 282,283,011 | B | R18 footprint | `bench/receipts/planner-two-stage-20261112T081151Z.json` | 20261112T081151Z | 4c6a9abc640c |
| `ladder.r19_ms` | 2,328.4 | ms | R19 write wall | `bench/receipts/r19-wall-after-20261112T140000Z.json` | 20261112T100622Z | c3f5e1a5d988 |
| `ladder.r20_ms` | 1,236.8 | ms | R20 write wall | `bench/receipts/r20-publication-20261112T150000Z.json` | 20261112T125513Z | ad0ca3be596c |
| `ladder.r21_ms` | 623.3 | ms | R21 write wall | `bench/receipts/r21-pipeline-20261112T201500Z.json` | 20261112T155655Z | 209858e04b60 |
| `ladder.r19_encode_before_ms` | 1,304.0 | ms | Mode-9 encode before R19 | `bench/receipts/mode9-encoder-20261112T160000Z.json` | 20261112T102632Z | c3f5e1a5d988 |
| `ladder.r19_encode_after_ms` | 457.0 | ms | Mode-9 encode after R19, sequential | `bench/receipts/mode9-encoder-20261112T160000Z.json` | 20261112T102632Z | c3f5e1a5d988 |
| `ladder.r19_encode_threaded_ms` | 224.3 | ms | Mode-9 encode after R19, four emission threads | `bench/receipts/mode9-encoder-20261112T160000Z.json` | 20261112T102632Z | c3f5e1a5d988 |
| `ladder.r19_speedup` | 2.85 | x | R19 mode-9 encoder speed-up, sequential | `bench/receipts/mode9-encoder-20261112T160000Z.json` | 20261112T102632Z | c3f5e1a5d988 |
| `ladder.r19_speedup_threaded` | 5.81 | x | R19 mode-9 encoder speed-up, four threads | `bench/receipts/mode9-encoder-20261112T160000Z.json` | 20261112T102632Z | c3f5e1a5d988 |
<!-- END GENERATED:perf-catalog-ladder -->

## R12c: checkpoint sessions, queue, incremental index, DCP metadata

<!-- BEGIN GENERATED:perf-catalog-r12c (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `session.persave_warm_p50_ms` | 339.0 | ms | Warm completion p50, one service per save | `bench/receipts/training-goodput-session-20261111T220500Z.json` | 20261111T220500Z | 76f098134085 |
| `session.session_warm_p50_ms` | 62.9 | ms | Warm completion p50, reused CheckpointSession | `bench/receipts/training-goodput-session-20261111T220500Z.json` | 20261111T220500Z | 76f098134085 |
| `session.persave_loop_wall_ms` | 17,615.8 | ms | Loop wall, one service per save | `bench/receipts/training-goodput-session-20261111T220500Z.json` | 20261111T220500Z | 76f098134085 |
| `session.session_loop_wall_ms` | 4,476.3 | ms | Loop wall, reused CheckpointSession | `bench/receipts/training-goodput-session-20261111T220500Z.json` | 20261111T220500Z | 76f098134085 |
| `session.persave_pause_p50_ms` | 3.99 | ms | Training-thread pause p50, one service per save | `bench/receipts/training-goodput-session-20261111T220500Z.json` | 20261111T220500Z | 76f098134085 |
| `session.session_pause_p50_ms` | 0.80 | ms | Training-thread pause p50, reused session | `bench/receipts/training-goodput-session-20261111T220500Z.json` | 20261111T220500Z | 76f098134085 |
| `queue.baseline_wall_ms` | 7,415.3 | ms | 1-step loop wall before the admission bound | `bench/receipts/checkpoint-queue-sweep-baseline-20261112T020000Z.json` | 20261112T020000Z | de210a62c6fc |
| `queue.baseline_warm_p50_ms` | 42.5 | ms | Warm completion p50 before the admission bound | `bench/receipts/checkpoint-queue-sweep-baseline-20261112T020000Z.json` | 20261112T020000Z | de210a62c6fc |
| `queue.bounded_wall_ms` | 5,761.7 | ms | 1-step loop wall at max_outstanding=2 | `bench/receipts/checkpoint-queue-bound-after-20261112T043000Z.json` | 20261112T043000Z | 1d716632d024 |
| `queue.bounded_warm_p50_ms` | 48.9 | ms | Warm completion p50 at max_outstanding=2 | `bench/receipts/checkpoint-queue-bound-after-20261112T043000Z.json` | 20261112T043000Z | 1d716632d024 |
| `queue.bounded_warm_p95_ms` | 85.9 | ms | Warm completion p95 at max_outstanding=2 | `bench/receipts/checkpoint-queue-bound-after-20261112T043000Z.json` | 20261112T043000Z | 1d716632d024 |
| `index.baseline_commit_p50_ms` | 0.198 | ms | Commit p50 at 1k generations, before | `bench/receipts/incremental-index-20261111T220000Z-baseline-delta-live64.json` | 20261111T220000Z-baseline-delta-live64 | b69fa0ef6a41 |
| `index.post_commit_p50_ms` | 0.110 | ms | Commit p50 at 1k generations, after | `bench/receipts/incremental-index-20261111T230000Z-post-delta-live64.json` | 20261111T230000Z-post-delta-live64 | 036add1a91db |
| `index.post_commit_p50_100k_ms` | 0.112 | ms | Commit p50 at 100k generations, after | `bench/receipts/incremental-index-20261111T230000Z-post-delta-live64.json` | 20261111T230000Z-post-delta-live64 | 036add1a91db |
| `index.live10k_baseline_ms` | 22.6 | ms | Commit p50 at 10k live objects, before | `bench/receipts/incremental-index-20261111T222000Z-baseline-delta-live10000.json` | 20261111T222000Z-baseline-delta-live10000 | b69fa0ef6a41 |
| `index.live10k_post_ms` | 5.6 | ms | Commit p50 at 10k live objects, after | `bench/receipts/incremental-index-20261111T235000Z-post-delta-live10000.json` | 20261111T235000Z-post-delta-live10000 | 036add1a91db |
| `dcp.reference_p50_ms` | 3.957 | ms | DCP metadata, reference Python encoder | `bench/receipts/dcp-metadata-20261111T230500Z.json` | 20261111T230500Z | 11230b33fe37 |
| `dcp.path_p50_ms` | 0.555 | ms | DCP metadata, plan path after the first call | `bench/receipts/dcp-metadata-20261111T235500Z.json` | 20261111T235500Z | 4947ef232ae4 |
| `dcp.path_first_ms` | 71.8 | ms | DCP metadata, first call (plan build) | `bench/receipts/dcp-metadata-20261111T235500Z.json` | 20261111T235500Z | 4947ef232ae4 |
| `dcp.plan_assemble_ms` | 0.405 | ms | DCP metadata, plan assembly | `bench/receipts/dcp-metadata-20261111T235500Z.json` | 20261111T235500Z | 4947ef232ae4 |
| `dcp.plan_match_ms` | 0.119 | ms | DCP metadata, plan/oracle equality signature | `bench/receipts/dcp-metadata-20261111T235500Z.json` | 20261111T235500Z | 4947ef232ae4 |
<!-- END GENERATED:perf-catalog-r12c -->

## Storage, region reads and multi-writer baselines

<!-- BEGIN GENERATED:perf-catalog-storage (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `family.total_b` | 1,276,128,956 | B | Qwen2.5-0.5B base+Instruct store total | `bench/receipts/planner-float-20261106T035316Z.json` | 2026-11-06T03:53:16Z | ba5a0e66ddba |
| `family.data_b` | 1,199,958,505 | B | Qwen2.5-0.5B base+Instruct tensor data only | `bench/receipts/planner-float-20261106T035316Z.json` | 2026-11-06T03:53:16Z | ba5a0e66ddba |
| `family.indexes_b` | 75,584,928 | B | Qwen2.5-0.5B base+Instruct index bytes | `bench/receipts/planner-float-20261106T035316Z.json` | 2026-11-06T03:53:16Z | ba5a0e66ddba |
| `family.zipnn_b` | 1,316,518,229 | B | ZipNN 0.5.4 whole-file total | `bench/receipts/planner-float-20261106T035316Z.json` | 2026-11-06T03:53:16Z | ba5a0e66ddba |
| `family.independent_zstd_b` | 1,528,909,007 | B | Independent per-tensor zstd -19 total | `bench/receipts/real-model-20261105T012000Z.json` | 2026-11-05T01:23:56Z | 1362a46ee372 |
| `family.base_read_ms` | 4,627.5 | ms | Qwen base full restore, read | `bench/receipts/planner-float-20261106T035316Z.json` | 2026-11-06T03:53:16Z | ba5a0e66ddba |
| `family.instruct_read_ms` | 11,371.2 | ms | Qwen Instruct full restore, read | `bench/receipts/planner-float-20261106T035316Z.json` | 2026-11-06T03:53:16Z | ba5a0e66ddba |
| `family.base_materialize_ms` | 6,494.3 | ms | Qwen base full restore, materialize | `bench/receipts/planner-float-20261106T035316Z.json` | 2026-11-06T03:53:16Z | ba5a0e66ddba |
| `family.zipnn_delta_pct` | 3.07 | % | Store total under ZipNN | `bench/receipts/planner-float-20261106T035316Z.json` | 2026-11-06T03:53:16Z | ba5a0e66ddba |
| `family.zstd_delta_pct` | 16.5 | % | Store total under independent per-tensor zstd -19 | `bench/receipts/real-model-20261105T012000Z.json, bench/receipts/planner-float-20261106T035316Z.json` | mixed | mixed |
| `tiles.row1_mode9_ms` | 749.26 | ms | 1-row embedding read, one mode-9 frame | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.row1_tiled_ms` | 2.12 | ms | 1-row embedding read, tiled | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.row100_mode9_ms` | 777.08 | ms | 100-row embedding read, one mode-9 frame | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.row100_tiled_ms` | 2.30 | ms | 100-row embedding read, tiled | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.write_mode9_b` | 180,623,816 | B | Embedding physical bytes, one mode-9 frame | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.write_tiled_b` | 181,541,557 | B | Embedding physical bytes, tiled | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.byte_overhead_pct` | 0.51 | % | Tiled byte overhead on the embedding | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.speedup_row1` | 353 | x | 1-row read speed-up from tiling | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.speedup_row100` | 339 | x | 100-row read speed-up from tiling | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `mw.n32_retry_amp` | 2.45 | ratio | MW2 retry amplification at N=32 | `bench/receipts/bench-multiwriter-20261109T150423Z.json` | 20261109T150423Z | b0bcbc5e4ded |
| `minio.c_storage_reduction_pct` | 33.53 | % | MinIO planned-writer storage reduction vs A | `bench/receipts/augmentation-minio-20261112T240000Z.json` | 20261112T204955Z | ccc6d695b114 |
<!-- END GENERATED:perf-catalog-storage -->

## Verification, conformance and claims counts

<!-- BEGIN GENERATED:perf-catalog-verification (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `verify.courts` | 45 | courts | Courts in the cited aggregate | `bench/receipts/verify-20261111T182601Z.json` | 20261111T182601Z | 3ab54c75b720 |
| `verify.passed` | 45 | courts | Courts passed | `bench/receipts/verify-20261111T182601Z.json` | 20261111T182601Z | 3ab54c75b720 |
| `verify.failed` | 0 | courts | Courts failed | `bench/receipts/verify-20261111T182601Z.json` | 20261111T182601Z | 3ab54c75b720 |
| `verify.skipped` | 0 | courts | Courts skipped | `bench/receipts/verify-20261111T182601Z.json` | 20261111T182601Z | 3ab54c75b720 |
| `verify.stamp` | 20261111T182601Z | text | Aggregate stamp | `bench/receipts/verify-20261111T182601Z.json` | 20261111T182601Z | 3ab54c75b720 |
| `verify.revision` | 3ab54c75b7205e9897e185aa1fac670b6dff16ff | text | Aggregate WeftFS revision | `bench/receipts/verify-20261111T182601Z.json` | 20261111T182601Z | 3ab54c75b720 |
| `conformance.vectors` | 420 | vectors | Conformance vectors in the manifest | `conformance/manifest.json` | — | — |
| `conformance.aggregate` | 6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8 | text | Conformance manifest aggregate SHA-256 | `conformance/manifest.json` | — | — |
| `claims.count` | 1,046 | claims | Entries in the claims manifest | `docs/claims.json` | — | — |
| `crash.requested` | 100,000 | iterations | Crash-torture iterations requested | `bench/receipts/crash-torture-20261104T104233Z.json` | 2026-11-04T10:42:33Z | 380ea4754483 |
| `crash.bad` | 0 | states | Crash-torture bad states | `bench/receipts/crash-torture-20261104T104233Z.json` | 2026-11-04T10:42:33Z | 380ea4754483 |
| `tests.rust_store_passed` | 772 | tests | Rust all-targets tests passed | `bench/receipts/verify-20261111T182601Z.tests.json` | — | — |
| `tests.rust_store_binaries` | 158 | tests | Rust all-targets test-result lines (one per binary) | `bench/receipts/verify-20261111T182601Z.tests.json` | — | — |
| `tests.rust_fuse_passed` | 815 | tests | FUSE all-targets tests passed | `bench/receipts/verify-20261111T182601Z.tests.json` | — | — |
| `tests.rust_fuse_failed` | 0 | tests | FUSE all-targets tests failed | `bench/receipts/verify-20261111T182601Z.tests.json` | — | — |
| `tests.rust_fuse_ignored` | 2 | tests | FUSE all-targets tests ignored | `bench/receipts/verify-20261111T182601Z.tests.json` | — | — |
| `tests.rust_codec_passed` | 267 | tests | WeftCodec all-targets tests passed | `bench/receipts/verify-20261111T182601Z.tests.json` | — | — |
<!-- END GENERATED:perf-catalog-verification -->

## Historical quotations

These are retained because documents quote them as the measurement of their
time. They are not current claims.

<!-- BEGIN GENERATED:perf-catalog-history (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `hist.r3.c_wall_ms` | 24,840.1 | ms | R3 planned write wall (historical) | `bench/receipts/augmentation-local-20261109T134132Z.json` | 20261109T134244Z | 2fb4e4b7fa5e |
| `hist.r3.a_wall_ms` | 335.9 | ms | R3 native control wall (historical) | `bench/receipts/augmentation-local-20261109T134132Z.json` | 20261109T134244Z | 2fb4e4b7fa5e |
| `hist.r3.ratio` | 73.9 | x | R3 write ratio versus native (historical) | `bench/receipts/augmentation-local-20261109T134132Z.json` | 20261109T134244Z | 2fb4e4b7fa5e |
| `hist.r3.reduction_pct` | 33.1 | % | R3 storage reduction (historical) | `bench/receipts/augmentation-local-20261109T134132Z.json` | 20261109T134244Z | 2fb4e4b7fa5e |
<!-- END GENERATED:perf-catalog-history -->

## Reproduce

```sh
# rewrite every generated region from the receipts
scripts/perf_numbers.py

# fail closed on any stale region, missing receipt, stamp/revision mismatch,
# unconsumed number or unknown block id
scripts/perf_numbers.py --check

# audit the mapping
scripts/perf_numbers.py --list
```

The script is standard-library Python and runs on the host or inside any court;
it reads files only.
