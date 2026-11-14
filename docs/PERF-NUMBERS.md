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

## Current six-arm contract court (R25)

The frozen contract (§[`bench/CONTRACTS-augmentation.md`](../bench/CONTRACTS-augmentation.md)),
seven interleaved repetitions, local POSIX, 101 tensors / 424,439,296 logical
bytes. Only `A` and `C` are shipped defaults; every row's `reproduction` object
in the receipt says how it is reached. Receipt
`bench/receipts/augmentation-local-20261113T004000Z.json`.

<!-- BEGIN GENERATED:perf-catalog-contract (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `contract.A.ms_min` | 327.2 | ms | A native durable write: durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.ms_p50` | 335.3 | ms | A native durable write: durable write wall, p50 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.ms_p95` | 7,012.2 | ms | A native durable write: durable write wall, p95 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.foot_b` | 424,439,296 | B | A native durable write: physical footprint, least-contended rep | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.cold_ms` | 349.6 | ms | A native durable write: cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.cold_p50` | 401.6 | ms | A native durable write: cold restore wall, p50 | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.cpu_ms` | 80 | ms | A native durable write: write CPU user+sys, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.rss_kib` | 271,160 | KiB | A native durable write: write peak RSS, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.written_b` | 424,439,296 | B | A native durable write: bytes handed to the backend | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.A.wreq` | 101 | requests | A native durable write: backend write requests | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.ms_min` | 333.4 | ms | N native + application manifest (supplementary): durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.ms_p50` | 345.9 | ms | N native + application manifest (supplementary): durable write wall, p50 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.ms_p95` | 7,474.9 | ms | N native + application manifest (supplementary): durable write wall, p95 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.foot_b` | 424,477,171 | B | N native + application manifest (supplementary): physical footprint, least-contended rep | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.cold_ms` | 348.8 | ms | N native + application manifest (supplementary): cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.cold_p50` | 582.7 | ms | N native + application manifest (supplementary): cold restore wall, p50 | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.cpu_ms` | 90 | ms | N native + application manifest (supplementary): write CPU user+sys, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.rss_kib` | 271,492 | KiB | N native + application manifest (supplementary): write peak RSS, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.written_b` | 424,477,171 | B | N native + application manifest (supplementary): bytes handed to the backend | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.wreq` | 104 | requests | N native + application manifest (supplementary): backend write requests | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.ms_min` | 931.2 | ms | B protocol pass-through: durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.ms_p50` | 1,543.7 | ms | B protocol pass-through: durable write wall, p50 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.ms_p95` | 19,021.6 | ms | B protocol pass-through: durable write wall, p95 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.foot_b` | 424,608,542 | B | B protocol pass-through: physical footprint, least-contended rep | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.cold_ms` | 533.6 | ms | B protocol pass-through: cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.cold_p50` | 585.3 | ms | B protocol pass-through: cold restore wall, p50 | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.cpu_ms` | 250 | ms | B protocol pass-through: write CPU user+sys, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.rss_kib` | 538,720 | KiB | B protocol pass-through: write peak RSS, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.written_b` | 424,621,048 | B | B protocol pass-through: bytes handed to the backend | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.wreq` | 611 | requests | B protocol pass-through: backend write requests | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.ms_min` | 974.1 | ms | C codec (shipped planned write): durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.ms_p50` | 1,175.3 | ms | C codec (shipped planned write): durable write wall, p50 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.ms_p95` | 7,605.3 | ms | C codec (shipped planned write): durable write wall, p95 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.foot_b` | 282,106,049 | B | C codec (shipped planned write): physical footprint, least-contended rep | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.cold_ms` | 1,408.3 | ms | C codec (shipped planned write): cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.cold_p50` | 1,431.8 | ms | C codec (shipped planned write): cold restore wall, p50 | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.cpu_ms` | 1,090 | ms | C codec (shipped planned write): write CPU user+sys, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.rss_kib` | 690,664 | KiB | C codec (shipped planned write): write peak RSS, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.written_b` | 282,118,555 | B | C codec (shipped planned write): bytes handed to the backend | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.wreq` | 216 | requests | C codec (shipped planned write): backend write requests | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.ms_min` | 1,522.5 | ms | D protocol: durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.ms_p50` | 9,338.5 | ms | D protocol: durable write wall, p50 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.ms_p95` | 20,637.1 | ms | D protocol: durable write wall, p95 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.foot_b` | 424,659,735 | B | D protocol: physical footprint, least-contended rep | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.cold_ms` | 537.3 | ms | D protocol: cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.cold_p50` | 620.0 | ms | D protocol: cold restore wall, p50 | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.cpu_ms` | 270 | ms | D protocol: write CPU user+sys, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.rss_kib` | 538,844 | KiB | D protocol: write peak RSS, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.written_b` | 424,629,900 | B | D protocol: bytes handed to the backend | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.wreq` | 611 | requests | D protocol: backend write requests | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.ms_min` | 1,019.3 | ms | E combined: durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.ms_p50` | 11,271.6 | ms | E combined: durable write wall, p50 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.ms_p95` | 11,728.3 | ms | E combined: durable write wall, p95 (7 reps) | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.foot_b` | 282,157,242 | B | E combined: physical footprint, least-contended rep | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.cold_ms` | 1,426.3 | ms | E combined: cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.cold_p50` | 1,545.8 | ms | E combined: cold restore wall, p50 | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.cpu_ms` | 1,090 | ms | E combined: write CPU user+sys, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.rss_kib` | 690,980 | KiB | E combined: write peak RSS, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.written_b` | 282,127,407 | B | E combined: bytes handed to the backend | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.wreq` | 216 | requests | E combined: backend write requests | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.ratio` | 1.02 | x | N write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.N.storage_pct` | -0.01 | % | N physical-footprint reduction vs A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.ratio` | 2.85 | x | B write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.B.storage_pct` | -0.04 | % | B physical-footprint reduction vs A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.ratio` | 2.98 | x | C write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.C.storage_pct` | 33.53 | % | C physical-footprint reduction vs A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.ratio` | 4.65 | x | D write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.D.storage_pct` | -0.05 | % | D physical-footprint reduction vs A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.ratio` | 3.12 | x | E write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.E.storage_pct` | 33.52 | % | E physical-footprint reduction vs A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `contract.minio.A.ms_min` | 823.6 | ms | MinIO A: durable write wall, least-contended | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
| `contract.minio.A.ms_p50` | 829.4 | ms | MinIO A: durable write wall, p50 | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
| `contract.minio.A.foot_b` | 424,439,296 | B | MinIO A: physical footprint | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
| `contract.minio.A.cold_ms` | 300.5 | ms | MinIO A: cold restore wall, least-contended | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
| `contract.minio.C.ms_min` | 1,238.9 | ms | MinIO C: durable write wall, least-contended | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
| `contract.minio.C.ms_p50` | 1,252.3 | ms | MinIO C: durable write wall, p50 | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
| `contract.minio.C.foot_b` | 282,106,311 | B | MinIO C: physical footprint | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
| `contract.minio.C.cold_ms` | 1,519.3 | ms | MinIO C: cold restore wall, least-contended | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
| `contract.minio.C.ratio` | 1.50 | x | MinIO C write wall as a multiple of A | `bench/receipts/augmentation-minio-20261113T010500Z.json` | 20261113T004643Z | 0a01606e2654 |
<!-- END GENERATED:perf-catalog-contract -->

## R22 store-API frontier (not the shipped default)

`C-pipe4` drives the bounded prepare/publish pipeline
(`Store::prepare_tensor_group_pipelined`); no shipped CLI verb reaches it. It is
reported separately so the frontier is never read as the product default.

<!-- BEGIN GENERATED:perf-catalog-frontier (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `frontier.c.ms_min` | 592.6 | ms | R22 C-pipe4 write wall, least-contended | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `frontier.c.ms_med` | 600.9 | ms | R22 C-pipe4 write wall, median of 7 | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `frontier.c.foot_b` | 282,106,049 | B | R22 C-pipe4 physical footprint | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `frontier.c.rss_kib` | 939,256 | KiB | R22 C-pipe4 peak RSS | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `frontier.c.cpu_ms` | 1,150 | ms | R22 C-pipe4 write CPU | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `frontier.a.ms_min` | 332.0 | ms | R22 native A write wall, least-contended | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `frontier.c.ratio` | 1.78 | x | R22 C-pipe4 write wall as a multiple of A | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
<!-- END GENERATED:perf-catalog-frontier -->

## Cross-family contract rows (R25)

Same contract and repetitions; only the fixture and its bounded subset differ.
The pythia rows are a measured loss.

<!-- BEGIN GENERATED:perf-catalog-family (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `family.qwen05.logical_b` | 424,439,296 | B | Qwen2.5-0.5B-Instruct (101-tensor subset): logical bytes | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `family.qwen05.a_ms` | 327.2 | ms | Qwen2.5-0.5B-Instruct (101-tensor subset): native A durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `family.qwen05.c_ms` | 974.1 | ms | Qwen2.5-0.5B-Instruct (101-tensor subset): shipped C durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `family.qwen05.a_foot_b` | 424,439,296 | B | Qwen2.5-0.5B-Instruct (101-tensor subset): native A physical footprint | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `family.qwen05.c_foot_b` | 282,106,049 | B | Qwen2.5-0.5B-Instruct (101-tensor subset): shipped C physical footprint | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `family.qwen05.c_cold_ms` | 1,408.3 | ms | Qwen2.5-0.5B-Instruct (101-tensor subset): shipped C cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `family.qwen05.ratio` | 2.98 | x | Qwen2.5-0.5B-Instruct (101-tensor subset): C write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `family.qwen05.storage_pct` | 33.53 | % | Qwen2.5-0.5B-Instruct (101-tensor subset): C footprint reduction vs A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `family.qwen3b.logical_b` | 626,540,544 | B | Qwen2.5-3B-Instruct (9-tensor subset): logical bytes | `bench/receipts/augmentation-local-20261113T013000Z.json` | 20261113T010916Z | 9d8fa3845486 |
| `family.qwen3b.a_ms` | 411.6 | ms | Qwen2.5-3B-Instruct (9-tensor subset): native A durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T013000Z.json` | 20261113T010916Z | 9d8fa3845486 |
| `family.qwen3b.c_ms` | 1,329.1 | ms | Qwen2.5-3B-Instruct (9-tensor subset): shipped C durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T013000Z.json` | 20261113T010916Z | 9d8fa3845486 |
| `family.qwen3b.a_foot_b` | 626,540,544 | B | Qwen2.5-3B-Instruct (9-tensor subset): native A physical footprint | `bench/receipts/augmentation-local-20261113T013000Z.json` | 20261113T010916Z | 9d8fa3845486 |
| `family.qwen3b.c_foot_b` | 415,882,220 | B | Qwen2.5-3B-Instruct (9-tensor subset): shipped C physical footprint | `bench/receipts/augmentation-local-20261113T013000Z.json` | 20261113T010916Z | 9d8fa3845486 |
| `family.qwen3b.c_cold_ms` | 2,129.2 | ms | Qwen2.5-3B-Instruct (9-tensor subset): shipped C cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T013000Z.json` | 20261113T010916Z | 9d8fa3845486 |
| `family.qwen3b.ratio` | 3.23 | x | Qwen2.5-3B-Instruct (9-tensor subset): C write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T013000Z.json` | 20261113T010916Z | 9d8fa3845486 |
| `family.qwen3b.storage_pct` | 33.62 | % | Qwen2.5-3B-Instruct (9-tensor subset): C footprint reduction vs A | `bench/receipts/augmentation-local-20261113T013000Z.json` | 20261113T010916Z | 9d8fa3845486 |
| `family.pythia0.logical_b` | 281,706,496 | B | pythia-70m step 0 (76-tensor subset): logical bytes | `bench/receipts/augmentation-local-20261113T011500Z.json` | 20261113T005131Z | 9d8fa3845486 |
| `family.pythia0.a_ms` | 233.3 | ms | pythia-70m step 0 (76-tensor subset): native A durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T011500Z.json` | 20261113T005131Z | 9d8fa3845486 |
| `family.pythia0.c_ms` | 12,008.6 | ms | pythia-70m step 0 (76-tensor subset): shipped C durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T011500Z.json` | 20261113T005131Z | 9d8fa3845486 |
| `family.pythia0.a_foot_b` | 281,706,496 | B | pythia-70m step 0 (76-tensor subset): native A physical footprint | `bench/receipts/augmentation-local-20261113T011500Z.json` | 20261113T005131Z | 9d8fa3845486 |
| `family.pythia0.c_foot_b` | 163,964,176 | B | pythia-70m step 0 (76-tensor subset): shipped C physical footprint | `bench/receipts/augmentation-local-20261113T011500Z.json` | 20261113T005131Z | 9d8fa3845486 |
| `family.pythia0.c_cold_ms` | 682.6 | ms | pythia-70m step 0 (76-tensor subset): shipped C cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T011500Z.json` | 20261113T005131Z | 9d8fa3845486 |
| `family.pythia0.ratio` | 51.48 | x | pythia-70m step 0 (76-tensor subset): C write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T011500Z.json` | 20261113T005131Z | 9d8fa3845486 |
| `family.pythia0.storage_pct` | 41.80 | % | pythia-70m step 0 (76-tensor subset): C footprint reduction vs A | `bench/receipts/augmentation-local-20261113T011500Z.json` | 20261113T005131Z | 9d8fa3845486 |
| `family.pythia8.logical_b` | 281,706,496 | B | pythia-70m step 8 (76-tensor subset): logical bytes | `bench/receipts/augmentation-local-20261113T012000Z.json` | 20261113T005724Z | 9d8fa3845486 |
| `family.pythia8.a_ms` | 226.8 | ms | pythia-70m step 8 (76-tensor subset): native A durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T012000Z.json` | 20261113T005724Z | 9d8fa3845486 |
| `family.pythia8.c_ms` | 12,070.4 | ms | pythia-70m step 8 (76-tensor subset): shipped C durable write wall, least-contended | `bench/receipts/augmentation-local-20261113T012000Z.json` | 20261113T005724Z | 9d8fa3845486 |
| `family.pythia8.a_foot_b` | 281,706,496 | B | pythia-70m step 8 (76-tensor subset): native A physical footprint | `bench/receipts/augmentation-local-20261113T012000Z.json` | 20261113T005724Z | 9d8fa3845486 |
| `family.pythia8.c_foot_b` | 163,830,373 | B | pythia-70m step 8 (76-tensor subset): shipped C physical footprint | `bench/receipts/augmentation-local-20261113T012000Z.json` | 20261113T005724Z | 9d8fa3845486 |
| `family.pythia8.c_cold_ms` | 689.5 | ms | pythia-70m step 8 (76-tensor subset): shipped C cold restore wall, least-contended | `bench/receipts/augmentation-local-20261113T012000Z.json` | 20261113T005724Z | 9d8fa3845486 |
| `family.pythia8.ratio` | 53.22 | x | pythia-70m step 8 (76-tensor subset): C write wall as a multiple of A | `bench/receipts/augmentation-local-20261113T012000Z.json` | 20261113T005724Z | 9d8fa3845486 |
| `family.pythia8.storage_pct` | 41.84 | % | pythia-70m step 8 (76-tensor subset): C footprint reduction vs A | `bench/receipts/augmentation-local-20261113T012000Z.json` | 20261113T005724Z | 9d8fa3845486 |
<!-- END GENERATED:perf-catalog-family -->

## Compressed write (R17–R22)

This section is the historical R17–R22 frontier. `A` is the same backend's
native per-tensor write; `C-pipe4` is a **store-API row, not the shipped
default** (the shipped default is the R25 contract row above). Receipt
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
| `ladder.r3_footprint_b` | 284,139,539 | B | R3 planned footprint | `bench/receipts/augmentation-local-20261109T134132Z.json` | 20261109T134244Z | 2fb4e4b7fa5e |
| `ladder.r17_a_ms` | 326.4 | ms | Rung R17 native A write wall, least-contended | `bench/receipts/write-cost-ablation-20261112T052203Z.json` | 20261112T054124Z | d1893e1abd05 |
| `ladder.r18_a_ms` | 327.8 | ms | Rung R18 native A write wall, least-contended | `bench/receipts/planner-two-stage-20261112T081151Z.json` | 20261112T081151Z | 4c6a9abc640c |
| `ladder.r19_a_ms` | 329.8 | ms | Rung R19 native A write wall, least-contended | `bench/receipts/r19-wall-after-20261112T140000Z.json` | 20261112T100622Z | c3f5e1a5d988 |
| `ladder.r19_footprint_b` | 282,283,011 | B | Rung R19 planned footprint | `bench/receipts/r19-wall-after-20261112T140000Z.json` | 20261112T100622Z | c3f5e1a5d988 |
| `ladder.r20_a_ms` | 339.0 | ms | Rung R20 native A write wall, least-contended | `bench/receipts/r20-publication-20261112T150000Z.json` | 20261112T125513Z | ad0ca3be596c |
| `ladder.r20_footprint_b` | 282,106,049 | B | Rung R20 planned footprint | `bench/receipts/r20-publication-20261112T150000Z.json` | 20261112T125513Z | ad0ca3be596c |
| `ladder.r21_a_ms` | 333.5 | ms | Rung R21 native A write wall, least-contended | `bench/receipts/r21-pipeline-20261112T201500Z.json` | 20261112T155655Z | 209858e04b60 |
| `ladder.r21_footprint_b` | 282,106,049 | B | Rung R21 planned footprint | `bench/receipts/r21-pipeline-20261112T201500Z.json` | 20261112T155655Z | 209858e04b60 |
| `ladder.r22_a_ms` | 332.0 | ms | Rung R22 native A write wall, least-contended | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `ladder.r22_footprint_b` | 282,106,049 | B | Rung R22 planned footprint | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `ladder.r22_ms` | 592.6 | ms | R22 frontier planned write wall, least-contended | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `ladder.r25_ms` | 974.1 | ms | R25 shipped planned write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `ladder.r25_a_ms` | 327.2 | ms | R25 native A write wall, least-contended | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `ladder.r25_footprint_b` | 282,106,049 | B | R25 shipped planned footprint | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
| `ladder.r3_ratio` | 73.96 | x | Rung R3 planned write as a multiple of A | `bench/receipts/augmentation-local-20261109T134132Z.json` | 20261109T134244Z | 2fb4e4b7fa5e |
| `ladder.r17_ratio` | 74.22 | x | Rung R17 planned write as a multiple of A | `bench/receipts/write-cost-ablation-20261112T052203Z.json` | 20261112T054124Z | d1893e1abd05 |
| `ladder.r18_ratio` | 12.56 | x | Rung R18 planned write as a multiple of A | `bench/receipts/planner-two-stage-20261112T081151Z.json` | 20261112T081151Z | 4c6a9abc640c |
| `ladder.r19_ratio` | 7.06 | x | Rung R19 planned write as a multiple of A | `bench/receipts/r19-wall-after-20261112T140000Z.json` | 20261112T100622Z | c3f5e1a5d988 |
| `ladder.r20_ratio` | 3.65 | x | Rung R20 planned write as a multiple of A | `bench/receipts/r20-publication-20261112T150000Z.json` | 20261112T125513Z | ad0ca3be596c |
| `ladder.r21_ratio` | 1.87 | x | Rung R21 planned write as a multiple of A | `bench/receipts/r21-pipeline-20261112T201500Z.json` | 20261112T155655Z | 209858e04b60 |
| `ladder.r22_ratio` | 1.78 | x | Rung R22 planned write as a multiple of A | `bench/receipts/r22-pareto-20261112T230000Z.json` | 20261112T202816Z | ccc6d695b114 |
| `ladder.r25_ratio` | 2.98 | x | Rung R25 planned write as a multiple of A | `bench/receipts/augmentation-local-20261113T004000Z.json` | 20261113T004503Z | 0a01606e2654 |
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
| `tiles.write_mode9_ms` | 2,286.7 | ms | Embedding write, one mode-9 frame | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.write_tiled_ms` | 7,434.7 | ms | Embedding write, tiled | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
| `tiles.write_ratio` | 3.25 | x | Tiled write cost as a multiple of the one-frame write | `bench/receipts/float-tiled-20261107T042955Z.json` | 20261107T042955Z | — |
<!-- END GENERATED:perf-catalog-storage -->

## Multi-writer (MW2) throughput

Measured at WeftFS `b0bcbc5e` / WeftFS-Go `f46b351f`, **before** the R17–R22
write work; the numbers are not re-measured and are published as a scaling
loss (efficiency falls with N). Receipt
`bench/receipts/bench-multiwriter-20261109T150423Z.json`.

<!-- BEGIN GENERATED:perf-catalog-multiwriter (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `mw.n1_tx_s` | 4,000 | tx/s | MW2 local throughput at N=1, disjoint | `bench/receipts/bench-multiwriter-20261109T150423Z.json` | 20261109T150423Z | b0bcbc5e4ded |
| `mw.n32_tx_s` | 638 | tx/s | MW2 local throughput at N=32, disjoint | `bench/receipts/bench-multiwriter-20261109T150423Z.json` | 20261109T150423Z | b0bcbc5e4ded |
| `mw.n32_efficiency` | 0.0050 | ratio | MW2 scaling efficiency at N=32 | `bench/receipts/bench-multiwriter-20261109T150423Z.json` | 20261109T150423Z | b0bcbc5e4ded |
| `mw.minio_n4_tx_s` | 212.8 | tx/s | MW2 MinIO throughput at N=4 | `bench/receipts/bench-multiwriter-20261109T150423Z.json` | 20261109T150423Z | b0bcbc5e4ded |
<!-- END GENERATED:perf-catalog-multiwriter -->

## N04: the object-backend writer pipeline (real MinIO)

One loopback MinIO server, six contract arms and four pipelined rows, seven
interleaved repetitions. Source: `bench/receipts/augmentation-minio-20261113T190000Z.json`;
analysis: [`bench/RESULTS-object-pipeline.md`](bench/RESULTS-object-pipeline.md).

<!-- BEGIN GENERATED:perf-catalog-n04 (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `n04.reps` | 7 | count | repetitions in the N04 court | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.tensors` | 101 | count | tensors restored by every arm | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.logical_b` | 424,439,296 | B | logical bytes in the subset | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.mismatches` | 0 | count | SHA-256 mismatches across every arm | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.a_write_p50` | 846.6 | ms | A durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.b_write_p50` | 1,480.7 | ms | B durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.b_pipe4_write_p50` | 1,391.8 | ms | B-pipe4 durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.c_write_p50` | 1,275.6 | ms | C durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.c_pipe4_write_p50` | 858.7 | ms | C-pipe4 durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.d_write_p50` | 1,513.6 | ms | D durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.d_pipe4_write_p50` | 1,412.7 | ms | D-pipe4 durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.e_write_p50` | 1,295.5 | ms | E durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.e_pipe4_write_p50` | 865.4 | ms | E-pipe4 durable write wall, p50 of repetitions | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.cpipe_write_min` | 826.9 | ms | C-pipe4 durable write wall, least-contended repetition | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.a_cold_p50` | 307.7 | ms | A cold restore wall, p50 | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.cpipe_cold_p50` | 1,520.4 | ms | C-pipe4 cold restore wall, p50 | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.cpipe_storage_pct` | 33.5 | % | C-pipe4 physical-footprint reduction against A | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.epipe_storage_pct` | 33.5 | % | E-pipe4 physical-footprint reduction against A | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.c_rss` | 691,960 | KiB | C write peak RSS, p50 | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.cpipe_rss` | 938,376 | KiB | C-pipe4 write peak RSS, p50 | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.c_cpu` | 1,220 | ms | C write CPU user+sys, p50 | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.cpipe_cpu` | 1,350 | ms | C-pipe4 write CPU user+sys, p50 | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.puts` | 43 | count | PUT requests in the adverse baseline write | `bench/receipts/n04-object-pipeline-20261113T190000Z.json` | 20261113T190000Z | 37a03c0dbb62 |
| `n04.fault_target` | 30 | count | the PUT index the fault proxy targets | `bench/receipts/n04-object-pipeline-20261113T190000Z.json` | 20261113T190000Z | 37a03c0dbb62 |
| `n04.pipe_b_pct` | 6.0 | % | B write-wall change from the pipeline | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.pipe_c_pct` | 32.7 | % | C write-wall change from the pipeline | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.pipe_d_pct` | 6.7 | % | D write-wall change from the pipeline | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.pipe_e_pct` | 33.2 | % | E write-wall change from the pipeline | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
| `n04.cpipe_vs_a_ratio` | 1.01 | ratio | C-pipe4 write wall divided by A's | `bench/receipts/augmentation-minio-20261113T190000Z.json` | 20261114T000237Z | 37a03c0dbb62 |
<!-- END GENERATED:perf-catalog-n04 -->

## Single-node CephFS (T030/T038/T081)

A real deployed Ceph 19.2.0 with an MDS, mounted with `ceph-fuse`, six arms,
seven interleaved repetitions. Source:
`bench/receipts/augmentation-cephfs-20261114T040000Z.json`; analysis:
[`bench/RESULTS-cephfs.md`](bench/RESULTS-cephfs.md).

<!-- BEGIN GENERATED:perf-catalog-cephfs (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `cephfs.reps` | 7 | count | repetitions | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.tensors` | 101 | count | tensors restored by every arm | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.logical_b` | 424,439,296 | B | logical bytes in the subset | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.mismatches` | 0 | count | SHA-256 mismatches across every arm | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.a_write_p50` | 1,092.3 | ms | A durable write wall, p50 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.a_write_p95` | 9,349.9 | ms | A durable write wall, p95 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.b_write_p50` | 2,556.9 | ms | B durable write wall, p50 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.b_write_p95` | 21,257.5 | ms | B durable write wall, p95 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.c_write_p50` | 1,906.0 | ms | C durable write wall, p50 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.c_write_p95` | 20,622.6 | ms | C durable write wall, p95 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.d_write_p50` | 2,793.7 | ms | D durable write wall, p50 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.d_write_p95` | 33,543.0 | ms | D durable write wall, p95 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.e_write_p50` | 2,111.9 | ms | E durable write wall, p50 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.e_write_p95` | 17,555.8 | ms | E durable write wall, p95 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.n_write_p50` | 1,112.6 | ms | N durable write wall, p50 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.n_write_p95` | 15,206.2 | ms | N durable write wall, p95 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.c_write_min` | 1,848.5 | ms | C durable write wall, least-contended | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.a_cold_p50` | 867.4 | ms | A cold restore wall, p50 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.c_cold_p50` | 1,800.8 | ms | C cold restore wall, p50 | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.c_storage_pct` | 33.5 | % | C physical-footprint reduction against A | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.e_storage_pct` | 33.5 | % | E physical-footprint reduction against A | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.c_latency_pct` | -98.4 | % | C cold-restore latency change against A | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.c_vs_a_write_ratio` | 1.74 | ratio | C write wall divided by A's | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
| `cephfs.c_vs_a_cold_ratio` | 2.08 | ratio | C cold restore divided by A's | `bench/receipts/augmentation-cephfs-20261114T080000Z.json` | 20261114T023000Z | 6a584fd7a3f7 |
<!-- END GENERATED:perf-catalog-cephfs -->

## Verification, conformance and claims counts

<!-- BEGIN GENERATED:perf-catalog-verification (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `verify.courts` | 47 | courts | Courts in the cited aggregate | `bench/receipts/verify-20261113T033408Z.json` | 20261113T033408Z | 52da9888f372 |
| `verify.passed` | 47 | courts | Courts passed | `bench/receipts/verify-20261113T033408Z.json` | 20261113T033408Z | 52da9888f372 |
| `verify.failed` | 0 | courts | Courts failed | `bench/receipts/verify-20261113T033408Z.json` | 20261113T033408Z | 52da9888f372 |
| `verify.skipped` | 0 | courts | Courts skipped | `bench/receipts/verify-20261113T033408Z.json` | 20261113T033408Z | 52da9888f372 |
| `verify.stamp` | 20261113T033408Z | text | Aggregate stamp | `bench/receipts/verify-20261113T033408Z.json` | 20261113T033408Z | 52da9888f372 |
| `verify.revision` | 52da9888f3726e6af5c364af7707fd98574fc954 | text | Aggregate WeftFS revision | `bench/receipts/verify-20261113T033408Z.json` | 20261113T033408Z | 52da9888f372 |
| `conformance.vectors` | 420 | vectors | Conformance vectors in the manifest | `conformance/manifest.json` | — | — |
| `conformance.aggregate` | 6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8 | text | Conformance manifest aggregate SHA-256 | `conformance/manifest.json` | — | — |
| `claims.count` | 1,261 | claims | Entries in the claims manifest | `docs/claims.json` | — | — |
| `crash.requested` | 100,000 | iterations | Crash-torture iterations requested | `bench/receipts/crash-torture-20261104T104233Z.json` | 2026-11-04T10:42:33Z | 380ea4754483 |
| `crash.bad` | 0 | states | Crash-torture bad states | `bench/receipts/crash-torture-20261104T104233Z.json` | 2026-11-04T10:42:33Z | 380ea4754483 |
| `tests.rust_store_passed` | 807 | tests | Rust all-targets tests passed | `bench/receipts/verify-20261113T033408Z.tests.json` | 94321513d150e4b52d8ce4e3f534189a93041e5d5d9f5d3651d1ac830459e12a | bench/receip |
| `tests.rust_store_binaries` | 170 | tests | Rust all-targets test-result lines (one per binary) | `bench/receipts/verify-20261113T033408Z.tests.json` | 94321513d150e4b52d8ce4e3f534189a93041e5d5d9f5d3651d1ac830459e12a | bench/receip |
| `tests.rust_fuse_passed` | 850 | tests | FUSE all-targets tests passed | `bench/receipts/verify-20261113T033408Z.tests.json` | 94321513d150e4b52d8ce4e3f534189a93041e5d5d9f5d3651d1ac830459e12a | bench/receip |
| `tests.rust_fuse_failed` | 0 | tests | FUSE all-targets tests failed | `bench/receipts/verify-20261113T033408Z.tests.json` | 94321513d150e4b52d8ce4e3f534189a93041e5d5d9f5d3651d1ac830459e12a | bench/receip |
| `tests.rust_fuse_ignored` | 2 | tests | FUSE all-targets tests ignored | `bench/receipts/verify-20261113T033408Z.tests.json` | 94321513d150e4b52d8ce4e3f534189a93041e5d5d9f5d3651d1ac830459e12a | bench/receip |
| `tests.rust_codec_passed` | 290 | tests | WeftCodec all-targets tests passed | `bench/receipts/verify-20261113T033408Z.tests.json` | 94321513d150e4b52d8ce4e3f534189a93041e5d5d9f5d3651d1ac830459e12a | bench/receip |
| `artifacts.wheel_bytes` | 115,851 | B | wheel size | `bench/receipts/release-artifacts-20261112T233126Z.json` | 20261112T233126Z | — |
| `artifacts.wheel_sha` | 9b6ff9bbfc92a195408bf411661208a92c6a739d6f7f1b0b8207bb5304464928 | text | wheel sha256 | `bench/receipts/release-artifacts-20261112T233126Z.json` | 20261112T233126Z | — |
| `artifacts.sdist_bytes` | 156,977 | B | sdist size | `bench/receipts/release-artifacts-20261112T233126Z.json` | 20261112T233126Z | — |
| `artifacts.sdist_sha` | cc86abd96365390259b63d585b982698eee4ddafa24ec4ea69ea170a5c388295 | text | sdist sha256 | `bench/receipts/release-artifacts-20261112T233126Z.json` | 20261112T233126Z | — |
| `build.wheel_sha` | 92efd39d2e4732081dccfccbb19c2f2849ff427e9f38d6aed9226624d5ceed06 | — | 1.4.1 wheel sha256 (GitHub release asset build) | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `build.wheel_bytes` | 115,889 | B | 1.4.1 wheel size (GitHub release asset build) | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `build.sdist_sha` | 773c317d216b252be36dc6b07d5a1add8ba80c14faf46c7e510011180ec196e2 | — | 1.4.1 sdist sha256 (GitHub release asset build) | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `build.sdist_bytes` | 156,971 | B | 1.4.1 sdist size (GitHub release asset build) | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `live.pypi.wheel_sha` | 41ad31b43326feb86ef9cc2cac6b5d4f369e19fe6d9ce0da80203c940ff9d3c6 | — | wheel sha256 | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `live.pypi.wheel_bytes` | 115,851 | B | wheel size | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `live.pypi.sdist_sha` | 2fd174fc74ba4bba3c2d1dff241c9eeb9f4f002dd25af994b48f04ae201e69c0 | — | sdist sha256 | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `live.pypi.sdist_bytes` | 156,858 | B | sdist size | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `live.crates.weftcodec` | 7f24d38382f7894903cc04763493530b48a5cc13d8e4b66fa08e0f82ea756882 | — | weftcodec 1.4.0 crates.io checksum | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `live.crates.weftfs` | d94ff6e7803a7ae7af807c5b4fa162b60f1002fa1b3388e086a63562a4d06be0 | — | weftfs 1.4.0 crates.io checksum | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `verify.v140.courts` | 46 | courts | v1.4.0 courts | `bench/receipts/verify-20261112T223540Z.json` | 20261112T223540Z | 0461132d4fc6 |
| `verify.v140.passed` | 46 | courts | v1.4.0 courts passed | `bench/receipts/verify-20261112T223540Z.json` | 20261112T223540Z | 0461132d4fc6 |
| `verify.v140.failed` | 0 | courts | v1.4.0 courts failed | `bench/receipts/verify-20261112T223540Z.json` | 20261112T223540Z | 0461132d4fc6 |
| `verify.v140.skipped` | 0 | courts | v1.4.0 courts skipped | `bench/receipts/verify-20261112T223540Z.json` | 20261112T223540Z | 0461132d4fc6 |
| `verify.v140.stamp` | 20261112T223540Z | — | v1.4.0 aggregate stamp | `bench/receipts/verify-20261112T223540Z.json` | 20261112T223540Z | 0461132d4fc6 |
| `verify.v140.revision` | 0461132d4fc63ce458de6b388b38a8f27222d387 | — | v1.4.0 aggregate revision | `bench/receipts/verify-20261112T223540Z.json` | 20261112T223540Z | 0461132d4fc6 |
| `tests.v140.rust_store_passed` | 807 | tests | v1.4.0 Rust all-targets passed | `bench/receipts/verify-20261112T223540Z.tests.json` | 68f10f4da63532d2526d82feb5fc7727fc17573278a0d05dd317559c29cf84fa | bench/receip |
| `tests.v140.rust_fuse_passed` | 850 | tests | v1.4.0 FUSE all-targets passed | `bench/receipts/verify-20261112T223540Z.tests.json` | 68f10f4da63532d2526d82feb5fc7727fc17573278a0d05dd317559c29cf84fa | bench/receip |
| `tests.v140.rust_fuse_failed` | 0 | tests | v1.4.0 FUSE all-targets failed | `bench/receipts/verify-20261112T223540Z.tests.json` | 68f10f4da63532d2526d82feb5fc7727fc17573278a0d05dd317559c29cf84fa | bench/receip |
| `tests.v140.rust_fuse_ignored` | 2 | tests | v1.4.0 FUSE all-targets ignored | `bench/receipts/verify-20261112T223540Z.tests.json` | 68f10f4da63532d2526d82feb5fc7727fc17573278a0d05dd317559c29cf84fa | bench/receip |
| `tests.v140.rust_codec_passed` | 290 | tests | v1.4.0 Rust codec all-targets passed | `bench/receipts/verify-20261112T223540Z.tests.json` | 68f10f4da63532d2526d82feb5fc7727fc17573278a0d05dd317559c29cf84fa | bench/receip |
| `verify.v141.courts` | 46 | courts | v1.4.1 Courts in the cited aggregate | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.v141.passed` | 46 | courts | v1.4.1 Courts passed | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.v141.failed` | 0 | courts | v1.4.1 Courts failed | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.v141.skipped` | 0 | courts | v1.4.1 Courts skipped | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.v141.stamp` | 20261113T022751Z | text | v1.4.1 Aggregate stamp | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.v141.revision` | e67e63186456d8d0915204baa19d65013a6bb281 | text | v1.4.1 Aggregate WeftFS revision | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `tests.v141.rust_store_passed` | 807 | tests | v1.4.1 Rust all-targets tests passed | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.v141.rust_store_binaries` | 170 | tests | v1.4.1 Rust all-targets test-result lines (one per binary) | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.v141.rust_fuse_passed` | 850 | tests | v1.4.1 FUSE all-targets tests passed | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.v141.rust_fuse_failed` | 0 | tests | v1.4.1 FUSE all-targets tests failed | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.v141.rust_fuse_ignored` | 2 | tests | v1.4.1 FUSE all-targets tests ignored | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.v141.rust_codec_passed` | 290 | tests | v1.4.1 WeftCodec all-targets tests passed | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `artifacts142.wheel_sha` | 787cc6ca203ea6a6c6285551281833de44b826946968ba3faf3b3d2d9ef2b022 | text | v1.4.2 wheel sha256 | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `artifacts142.wheel_bytes` | 115,685 | B | v1.4.2 wheel size | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `artifacts142.sdist_sha` | ab214bffced0312f0f50fb3a1929a68c2163e32493f9a159821a9ea78b39ae78 | text | v1.4.2 sdist sha256 | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `artifacts142.sdist_bytes` | 156,563 | B | v1.4.2 sdist size | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.first.wheel_sha` | 74779b5c26feaaf0dbb70c909d48221a699cc4e91abd5fbd6c1eed7782bcb176 | text | 1.4.1 first-build wheel sha256 | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.first.wheel_bytes` | 115,540 | B | 1.4.1 first-build wheel size | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.first.sdist_sha` | 9fa6cfb62062754e13db98708dd32727b9d046dd6ab5896569216958097b79a0 | text | 1.4.1 first-build sdist sha256 | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.first.sdist_bytes` | 156,293 | B | 1.4.1 first-build sdist size | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.second.wheel_sha` | 92efd39d2e4732081dccfccbb19c2f2849ff427e9f38d6aed9226624d5ceed06 | text | 1.4.1 second-build wheel sha256 | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.second.wheel_bytes` | 115,889 | B | 1.4.1 second-build wheel size | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.second.sdist_sha` | 773c317d216b252be36dc6b07d5a1add8ba80c14faf46c7e510011180ec196e2 | text | 1.4.1 second-build sdist sha256 | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.second.sdist_bytes` | 156,971 | B | 1.4.1 second-build sdist size | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.pypi.wheel_sha` | 73cd3422ec7267b9876ad48458682e6d7dede24a53b4c33c76d50dc8613b4f32 | text | 1.4.1 pypi-build wheel sha256 | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.pypi.wheel_bytes` | 115,889 | B | 1.4.1 pypi-build wheel size | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.pypi.sdist_sha` | 93a0b8c8c320f8d3316152c2bdc1b797f4869fcb2116dbbb825953b8942286a1 | text | 1.4.1 pypi-build sdist sha256 | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `build141.pypi.sdist_bytes` | 157,012 | B | 1.4.1 pypi-build sdist size | `bench/receipts/release-artifacts-20261113T045609Z.json` | 20261113T045609Z | 52da9888f372 |
| `writer_lock.clean.iterations` | 60 | runs | Writer-lock suite repetitions (root, no load) | `bench/receipts/writer-lock-20261113T120752Z.json` | 20261113T120752Z | fcb4cb38c4cc |
| `writer_lock.clean.failed` | 0 | failures | Writer-lock suite failures (root, no load) | `bench/receipts/writer-lock-20261113T120752Z.json` | 20261113T120752Z | fcb4cb38c4cc |
| `writer_lock.loaded.iterations` | 25 | runs | Writer-lock suite repetitions beside CPU load | `bench/receipts/writer-lock-load-20261113T120805Z.json` | 20261113T120805Z | fcb4cb38c4cc |
| `writer_lock.loaded.load` | 8 | processes | Busy CPU processes beside the loaded repetitions | `bench/receipts/writer-lock-load-20261113T120805Z.json` | 20261113T120805Z | fcb4cb38c4cc |
| `writer_lock.loaded.failed` | 0 | failures | Writer-lock suite failures beside CPU load | `bench/receipts/writer-lock-load-20261113T120805Z.json` | 20261113T120805Z | fcb4cb38c4cc |
| `writer_lock.nonroot.iterations` | 10 | runs | Writer-lock suite repetitions as a non-root uid | `bench/receipts/writer-lock-nonroot-20261113T120819Z.json` | 20261113T120819Z | fcb4cb38c4cc |
| `writer_lock.nonroot.uid` | 1001 | — | uid the non-root repetitions ran as | `bench/receipts/writer-lock-nonroot-20261113T120819Z.json` | 20261113T120819Z | fcb4cb38c4cc |
| `writer_lock.nonroot.failed` | 0 | failures | Writer-lock suite failures as a non-root uid | `bench/receipts/writer-lock-nonroot-20261113T120819Z.json` | 20261113T120819Z | fcb4cb38c4cc |
| `stage_budget.fix.test_count` | 13 | tests | Tests in the stage_budget module | `bench/receipts/stage-budget-fix-20261113T224055Z.json` | 20261113T224055Z | a6ab12eb667b |
| `stage_budget.fix.reps` | 30 | runs | Repetitions per thread count | `bench/receipts/stage-budget-fix-20261113T224055Z.json` | 20261113T224055Z | a6ab12eb667b |
| `stage_budget.fix.total_runs` | 120 | runs | Total stage_budget repetitions | `bench/receipts/stage-budget-fix-20261113T224055Z.json` | 20261113T224055Z | a6ab12eb667b |
| `stage_budget.fix.failed` | 0 | failures | Stage_budget repetition failures | `bench/receipts/stage-budget-fix-20261113T224055Z.json` | 20261113T224055Z | a6ab12eb667b |
| `stage_budget.fix.timeouts` | 0 | timeouts | Stage_budget repetition timeouts (a hang is exit 124) | `bench/receipts/stage-budget-fix-20261113T224055Z.json` | 20261113T224055Z | a6ab12eb667b |
| `stage_budget.fix.control_delta_pct` | -20.47 | % | X06 unbudgeted control p50 change (arm never calls admit) | `bench/receipts/stage-budget-delta-20261113T225001Z.json` | 20261113T225001Z | a6ab12eb667b |
| `stage_budget.fix.restore_test_count` | 8 | tests | Tests in the restore_budget suite | `bench/receipts/stage-budget-fix-20261113T224055Z.json` | 20261113T224055Z | a6ab12eb667b |
| `stage_budget.fix.stage_budget_runs` | 90 | runs | stage_budget module repetitions | `bench/receipts/stage-budget-fix-20261113T224055Z.json` | 20261113T224055Z | a6ab12eb667b |
| `stage_budget.fix.restore_budget_runs` | 30 | runs | restore_budget suite repetitions | `bench/receipts/stage-budget-fix-20261113T224055Z.json` | 20261113T224055Z | a6ab12eb667b |
| `stage_budget.suites.rust_suites` | 182 | tests | Full Rust suites (all-targets) | `bench/receipts/stage-budget-suites-20261113T224133Z.json` | 20261113T224133Z | a6ab12eb667b |
| `stage_budget.suites.rust_passed` | 862 | tests | Full Rust passed tests | `bench/receipts/stage-budget-suites-20261113T224133Z.json` | 20261113T224133Z | a6ab12eb667b |
| `stage_budget.suites.rust_failed` | 0 | tests | Full Rust failed tests | `bench/receipts/stage-budget-suites-20261113T224133Z.json` | 20261113T224133Z | a6ab12eb667b |
| `stage_budget.suites.rust_ignored` | 0 | tests | Full Rust ignored tests | `bench/receipts/stage-budget-suites-20261113T224133Z.json` | 20261113T224133Z | a6ab12eb667b |
| `stage_budget.suites.fuse_suites` | 182 | tests | Full FUSE suites (all-targets) | `bench/receipts/stage-budget-suites-20261113T224133Z.json` | 20261113T224133Z | a6ab12eb667b |
| `stage_budget.suites.fuse_passed` | 905 | tests | Full FUSE passed tests | `bench/receipts/stage-budget-suites-20261113T224133Z.json` | 20261113T224133Z | a6ab12eb667b |
| `stage_budget.suites.fuse_failed` | 0 | tests | Full FUSE failed tests | `bench/receipts/stage-budget-suites-20261113T224133Z.json` | 20261113T224133Z | a6ab12eb667b |
| `stage_budget.suites.fuse_ignored` | 2 | tests | Full FUSE ignored tests | `bench/receipts/stage-budget-suites-20261113T224133Z.json` | 20261113T224133Z | a6ab12eb667b |
| `conformance.rust_total` | 420 | count | Rust adapter vectors run | `bench/receipts/conformance-rust-20261114T062338Z.json` | — | — |
| `conformance.rust_pass` | 288 | count | Rust adapter passes | `bench/receipts/conformance-rust-20261114T062338Z.json` | — | — |
| `conformance.rust_fail` | 0 | count | Rust adapter failures | `bench/receipts/conformance-rust-20261114T062338Z.json` | — | — |
| `conformance.rust_skip` | 132 | count | Rust adapter skips | `bench/receipts/conformance-rust-20261114T062338Z.json` | — | — |
| `conformance.go_total` | 420 | count | Go adapter vectors run | `bench/receipts/conformance-go-20261114T062338Z.json` | — | — |
| `conformance.go_pass` | 288 | count | Go adapter passes | `bench/receipts/conformance-go-20261114T062338Z.json` | — | — |
| `conformance.go_fail` | 0 | count | Go adapter failures | `bench/receipts/conformance-go-20261114T062338Z.json` | — | — |
| `conformance.go_skip` | 132 | count | Go adapter skips | `bench/receipts/conformance-go-20261114T062338Z.json` | — | — |
| `conformance.python_total` | 420 | count | Python reader vectors run | `bench/receipts/conformance-python-20261114T062338Z.json` | — | — |
| `conformance.python_pass` | 229 | count | Python reader passes | `bench/receipts/conformance-python-20261114T062338Z.json` | — | — |
| `conformance.python_fail` | 0 | count | Python reader failures | `bench/receipts/conformance-python-20261114T062338Z.json` | — | — |
| `conformance.python_skip` | 191 | count | Python reader skips | `bench/receipts/conformance-python-20261114T062338Z.json` | — | — |
<!-- END GENERATED:perf-catalog-verification -->

## Context Advantage Court (R26)

One backend, three arms — the backend alone (N), a strong application-level
metadata baseline (M), and the WeftFS context plane (W) — seven interleaved
repetitions with the arm order rotated per repetition. Source receipt:
`bench/receipts/context-advantage-20261113T054528Z.json`; contract
`bench/CONTRACTS-context-advantage.md`; analysis and every loss
`bench/RESULTS-context-advantage.md`. The pre-registered fast-path tolerance is
carried in the same receipt and **failed**; `claim_holds` is false.

<!-- BEGIN GENERATED:perf-catalog-context-advantage (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `ca.page_bytes` | 65,536 | B | KV page logical size | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.pages` | 8 | pages | pages written per arm | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.probes` | 56 | probes | declared cross-scope probes | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.tol.p50` | 2.0 | × | declared fast-path p50 tolerance (W / N) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.tol.p95` | 2.5 | × | declared fast-path p95 tolerance (W / N) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.tol.bytes` | 1.10 | × | declared fast-path backend-byte tolerance (W / N) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.p50` | 29.1 | µs | N fast-path p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.p95` | 34.8 | µs | N fast-path p95 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.bytes_per_req` | 65,699 | B | N backend bytes per fast-path request | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.exact` | 64 | reads | N byte-exact compatible reads | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.accepted` | 48 | probes | N cross-scope probes accepted | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.wrong` | 48 | probes | N cross-scope acceptances with the wrong payload | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.denied` | 8 | probes | N cross-scope probes refused | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.probe_bytes` | 3,153,552 | B | N backend bytes read while refusing | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.probe_requests` | 96 | requests | N requests issued while refusing | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.probe_returned` | 3,145,728 | B | N logical bytes returned on a refusal | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.loss_reused` | 16 | reads | N compatible reuse after metadata loss | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.build_ms` | 116.0 | ms | N page build wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.footprint` | 544,093 | B | N physical footprint | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rss` | 19,968 | KiB | N peak RSS | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rec_outputs` | 99 | outputs | N recovery outputs materialized | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rec_logical` | 3,219,456 | B | N recovery logical bytes materialized | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rec_backend` | 3,235,591 | B | N recovery backend bytes read | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rec_wall` | 2.650 | ms | N recovery wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.p50` | 103.4 | µs | M fast-path p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.p95` | 117.7 | µs | M fast-path p95 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.bytes_per_req` | 31,918 | B | M backend bytes per fast-path request | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.exact` | 64 | reads | M byte-exact compatible reads | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.accepted` | 0 | probes | M cross-scope probes accepted | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.wrong` | 0 | probes | M cross-scope acceptances with the wrong payload | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.denied` | 56 | probes | M cross-scope probes refused | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.probe_bytes` | 766,026 | B | M backend bytes read while refusing | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.probe_requests` | 48 | requests | M requests issued while refusing | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.probe_returned` | 0 | B | M logical bytes returned on a refusal | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.loss_reused` | 0 | reads | M compatible reuse after metadata loss | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.build_ms` | 142.9 | ms | M page build wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.footprint` | 287,151 | B | M physical footprint | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rss` | 17,776 | KiB | M peak RSS | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_outputs` | 9 | outputs | M recovery outputs materialized | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_logical` | 270,336 | B | M recovery logical bytes materialized | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_backend` | 286,540 | B | M recovery backend bytes read | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_wall` | 0.400 | ms | M recovery wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.p50` | 169.6 | µs | W fast-path p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.p95` | 230.2 | µs | W fast-path p95 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.bytes_per_req` | 28,313 | B | W backend bytes per fast-path request | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.exact` | 64 | reads | W byte-exact compatible reads | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.accepted` | 0 | probes | W cross-scope probes accepted | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.wrong` | 0 | probes | W cross-scope acceptances with the wrong payload | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.denied` | 56 | probes | W cross-scope probes refused | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.probe_bytes` | 0 | B | W backend bytes read while refusing | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.probe_requests` | 0 | requests | W requests issued while refusing | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.probe_returned` | 0 | B | W logical bytes returned on a refusal | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.loss_reused` | 16 | reads | W compatible reuse after metadata loss | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.build_ms` | 119.3 | ms | W page build wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.footprint` | 268,004 | B | W physical footprint | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rss` | 22,860 | KiB | W peak RSS | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_outputs` | 9 | outputs | W recovery outputs materialized | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_logical` | 270,336 | B | W recovery logical bytes materialized | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_backend` | 321,343 | B | W recovery backend bytes read | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_wall` | 1.276 | ms | W recovery wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.compat_total` | 64 | reads | byte-exact compatible reads in the declared workload | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.loss_total` | 16 | reads | compatible reads repeated with the metadata database removed | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.recompute` | 2,122.7 | µs | measured recompute of one page (scalar kernel) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.ratio.w_n_p50` | 5.84 | × | W / N fast-path p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.ratio.w_n_p95` | 6.62 | × | W / N fast-path p95 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.ratio.m_n_p50` | 3.56 | × | M / N fast-path p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.ratio.m_n_p95` | 3.39 | × | M / N fast-path p95 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.ratio.w_n_bytes` | 0.431 | × | W / N backend bytes per request | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.mode12_decode` | 116.7 | µs | mode-12 decode p50 of the same page | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.mode19_decode` | 130.8 | µs | mode-19 decode p50 of the same page | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.prov_ratio` | 1.121 | × | mode-19 / mode-12 decode | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.frame_bytes` | 225,201 | B | mode-19 frame bytes for the eight pages | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.mode12_bytes` | 221,001 | B | the same eight pages as mode-12 frames | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.frame_delta_pct` | 1.90 | % | provenance descriptor overhead on frame size | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.ratio.w_m_rec_backend` | 1.12 | × | W / M recovery backend bytes | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.ratio.w_m_rec_wall` | 3.19 | × | W / M recovery wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.build_commits` | 8 | commits | N commits published for the same workload | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.build_commits` | 10 | commits | M commits published for the same workload | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.build_commits` | 8 | commits | W commits published for the same workload | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.tensors` | 96 | tensors | tensors in the recovery fixture | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.tensor_bytes` | 32,768 | B | bytes per recovery tensor | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.resolve` | 1.02 | µs | N path resolution | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.obj_io` | 2.76 | µs | N object blob I/O | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.blob_hash` | 10.51 | µs | N object blob hash | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.manifest_io` | 1.56 | µs | N manifest I/O | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.manifest_hash` | 0.25 | µs | N manifest hash | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.manifest_decode` | 0.80 | µs | N manifest decode | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.assemble` | 0.54 | µs | N chunk assembly | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.content_verify` | 10.46 | µs | N whole-content verification | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.compare` | 0.86 | µs | N exactness compare | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.instrumented` | 29.60 | µs | N instrumented wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.residual` | 0.93 | µs | N unnamed residual | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.resolve` | 1.18 | µs | W path resolution | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.auth_state` | 0.24 | µs | W object-record lookup | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.auth_descriptor` | 0.17 | µs | W descriptor decode + identity | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.auth_capability` | 0.17 | µs | W capability envelope + grants | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.auth_posix` | 0.07 | µs | W POSIX / ACL | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.obj_io` | 2.11 | µs | W object blob I/O | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.blob_hash` | 9.64 | µs | W object blob hash | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.manifest_io` | 1.86 | µs | W manifest I/O | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.manifest_hash` | 0.25 | µs | W manifest hash | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.manifest_decode` | 0.96 | µs | W manifest decode | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.assemble` | 0.22 | µs | W chunk assembly | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.content_verify` | 9.63 | µs | W whole-content verification | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.frame_parse` | 0.87 | µs | W frame parse + checksum | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.decode` | 152.82 | µs | W payload decode | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.compare` | 0.91 | µs | W exactness compare | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.instrumented` | 186.11 | µs | W instrumented wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.residual` | 4.55 | µs | W unnamed residual | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.auth_total` | 0.64 | µs | W total authorization preamble | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.overhead_vs_n` | 156.50 | µs | W instrumented wall minus N instrumented wall | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.decode_planes` | 103.69 | µs | mode-19 byte planes (unpack + join), p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.decode_hash` | 10.45 | µs | mode-19 reconstructed-page hash, p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.decode_desc` | 10.32 | µs | mode-19 descriptor parse, p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.decode_root` | 4.75 | µs | mode-19 descriptor identity (re-encode + hash), p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.decode_all` | 130.81 | µs | mode-19 decode, p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.decode_alloc` | 1.60 | µs | mode-19 decode not covered by the named components, p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.decode_share` | 0.90 | x | payload decode as a share of W fast-path p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rec_manifest_reads` | 99 | count | N manifest reads per recovery | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_manifest_reads` | 10 | count | M manifest reads per recovery | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_manifest_reads` | 28 | count | W manifest reads per recovery | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rec_object_reads` | 99 | count | N object reads per recovery | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_object_reads` | 10 | count | M object reads per recovery | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_object_reads` | 4 | count | W object reads per recovery | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rec_ledger_wall` | 2.50 | ms | N recovery, instrumented pass | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_ledger_wall` | 0.38 | ms | M recovery, instrumented pass | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_ledger_wall` | 1.19 | ms | W recovery, instrumented pass | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.rec_gap_extra_bytes` | 34,803 | B | W minus M backend bytes, recovery | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.rec_gap_per_tensor` | 5,800 | B | the same gap divided by the six tensor restores | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.r26.w.p50` | 187.8 | µs | R26 W fast-path p50 | `bench/receipts/context-advantage-20261113T054528Z.json` | 20261113T054528Z | 21a16be12b66 |
| `ca.r26.w.p95` | 250.8 | µs | R26 W fast-path p95 | `bench/receipts/context-advantage-20261113T054528Z.json` | 20261113T054528Z | 21a16be12b66 |
| `ca.r26.n.p50` | 28.9 | µs | R26 N fast-path p50 | `bench/receipts/context-advantage-20261113T054528Z.json` | 20261113T054528Z | 21a16be12b66 |
| `ca.r26.n.p95` | 31.7 | µs | R26 N fast-path p95 | `bench/receipts/context-advantage-20261113T054528Z.json` | 20261113T054528Z | 21a16be12b66 |
| `ca.r26.ratio.w_n_p50` | 6.51 | x | R26 W-over-N p50 | `bench/receipts/context-advantage-20261113T054528Z.json` | 20261113T054528Z | 21a16be12b66 |
| `ca.r26.ratio.w_n_p95` | 7.91 | x | R26 W-over-N p95 | `bench/receipts/context-advantage-20261113T054528Z.json` | 20261113T054528Z | 21a16be12b66 |
| `ca.r26.w.denied` | 56 | count | R26 W refused probes | `bench/receipts/context-advantage-20261113T054528Z.json` | 20261113T054528Z | 21a16be12b66 |
| `ca.p50_reduction_pct` | 9.7 | % | W fast-path p50 reduction since R26 | `bench/receipts/context-advantage-20261113T054528Z.json, bench/receipts/context-advantage-20261113T072921Z.json` | mixed | mixed |
| `ca.p95_reduction_pct` | 8.2 | % | W fast-path p95 reduction since R26 | `bench/receipts/context-advantage-20261113T054528Z.json, bench/receipts/context-advantage-20261113T072921Z.json` | mixed | mixed |
| `ca.ratio.w_m_p50` | 1.64 | x | W over M on the fast path (diagnostic, not the gate) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.r27.w.rec_backend` | 593,503 | B | R27 W recovery backend bytes | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.m.rec_backend` | 286,540 | B | R27 M recovery backend bytes | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.w.rec_wall` | 3.340 | ms | R27 W recovery wall | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.m.rec_wall` | 0.402 | ms | R27 M recovery wall | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.w.p50` | 169.7 | µs | R27 W fast-path p50 | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.w.p95` | 232.3 | µs | R27 W fast-path p95 | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.ratio.w_n_p50` | 5.84 | × | R27 W/N p50 ratio | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.ratio.w_n_p95` | 7.03 | × | R27 W/N p95 ratio | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.rec_gap_extra_bytes` | 306,963 | B | R27 W minus M recovery backend bytes | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.r27.p50_reduction_pct` | 9.7 | % | R26 to R27 fast-path p50 reduction | `bench/receipts/context-advantage-20261113T054528Z.json, bench/receipts/context-advantage-20261113T061907Z.json` | mixed | mixed |
| `ca.r27.p95_reduction_pct` | 7.3 | % | R26 to R27 fast-path p95 reduction | `bench/receipts/context-advantage-20261113T054528Z.json, bench/receipts/context-advantage-20261113T061907Z.json` | mixed | mixed |
| `ca.n.rec_manifest_loads` | 0 | loads | N checkpoint manifest loads per recovery pass | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.rec_output_p50` | 21.43 | µs | N recovery per-output p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_manifest_loads` | 0 | loads | M checkpoint manifest loads per recovery pass | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.rec_output_p50` | 24.81 | µs | M recovery per-output p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_manifest_loads` | 1 | loads | W checkpoint manifest loads per recovery pass | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_output_p50` | 97.24 | µs | W recovery per-output p50 | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.rec_manifest_object` | 45,197 | B | W checkpoint manifest object size | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.rec_gap_closed_bytes` | 272,160 | B | recovery byte gap removed in R28 | `bench/receipts/context-advantage-20261113T061907Z.json, bench/receipts/context-advantage-20261113T072921Z.json` | mixed | mixed |
| `ca.rec_manifest_load_cost` | 45,360 | B | backend bytes one checkpoint manifest load costs | `bench/receipts/context-advantage-20261113T061907Z.json, bench/receipts/context-advantage-20261113T072921Z.json` | mixed | mixed |
| `ca.rec_manifest_frame_bytes` | 163 | B | the manifest blob that describes the manifest object's single chunk | `bench/receipts/context-advantage-20261113T061907Z.json, bench/receipts/context-advantage-20261113T072921Z.json` | mixed | mixed |
| `ca.repr.page_bytes` | 65,536 | B | page logical size for both payload steps | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.repr.huffman_us` | 103.69 | µs | shipped huffman-canonical-v1 payload step | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.repr.huffman_mbs` | 632.0 | MB/s | shipped huffman-canonical-v1 payload step throughput | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.repr.raw_us` | 0.70 | µs | raw-payload floor: one allocation and one copy | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.repr.raw_mbs` | 93,489.3 | MB/s | raw-payload floor throughput | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.repr.ratio` | 147.9 | × | shipped payload step over the raw-copy floor | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.r27.w.rec_manifest_reads` | 34 | reads | R27 W recovery manifest-blob reads | `bench/receipts/context-advantage-20261113T061907Z.json` | 20261113T061907Z | 20e66147a7f8 |
| `ca.rec_manifest_loads_removed` | 6 | loads | checkpoint manifest loads removed per recovery pass | `bench/receipts/context-advantage-20261113T061907Z.json, bench/receipts/context-advantage-20261113T072921Z.json` | mixed | mixed |
| `ca.floor.nopayload_us` | 65.9 | µs | W fast-path p50 with the whole payload decode removed (derived) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.floor.nopayload_ratio` | 2.27 | × | that floor as a multiple of N's p50 (derived) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.repr.raw_payload_ratio` | 2.33 | × | raw page payload as a multiple of the shipped frames on the same pages (derived) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.repr.raw_payload_delta_pct` | 132.8 | % | storage delta of a raw payload on the same pages (derived) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.n.req_per_read` | 2.0 | requests | N backend requests per fast-path read (local POSIX) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.m.req_per_read` | 2.0 | requests | M backend requests per fast-path read (local POSIX) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
| `ca.w.req_per_read` | 2.0 | requests | W backend requests per fast-path read (local POSIX) | `bench/receipts/context-advantage-20261113T072921Z.json` | 20261113T072921Z | ad263816cbbc |
<!-- END GENERATED:perf-catalog-context-advantage -->

## EKBLOOM trial (retired)

One host, one local POSIX backend, one process, warm page cache. `EKB-E1`
(boundary) passed; `EKB-E2` (space) and `EKB-E3` (bounded random access) were
lost; the line retired under `docs/EKBLOOM-PLAN.md` §8.2. `EKB-E4`, `EKB-E5`
and `EKB-E6` were not run. Receipts `ekbloom-20261113T081205Z`,
`ekbloom-space-20261113T081552Z` and `ekbloom-read-20261113T083231Z`; analysis
[`bench/RESULTS-ekbloom.md`](../bench/RESULTS-ekbloom.md).

<!-- BEGIN GENERATED:perf-catalog-ekbloom (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `ekb.e1.objects` | 580 | objects | E1 objects round-tripped | `bench/receipts/ekbloom-20261113T081205Z.json` | 20261113T081205Z | bc75d63b68e8 |
| `ekb.e1.exact` | 580 | objects | E1 objects restored byte-exactly | `bench/receipts/ekbloom-20261113T081205Z.json` | 20261113T081205Z | bc75d63b68e8 |
| `ekb.e1.refused` | 0 | objects | E1 objects refused for memory_bound | `bench/receipts/ekbloom-20261113T081205Z.json` | 20261113T081205Z | bc75d63b68e8 |
| `ekb.e1.logical_bytes` | 1,976,131,072 | B | E1 declared element bytes | `bench/receipts/ekbloom-20261113T081205Z.json` | 20261113T081205Z | bc75d63b68e8 |
| `ekb.e1.peak_kib` | 776,292 | KiB | E1 process peak RSS | `bench/receipts/ekbloom-20261113T081205Z.json` | 20261113T081205Z | bc75d63b68e8 |
| `ekb.e2.objects` | 98 | objects | E2 objects (both containers) | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.logical_bytes` | 852,852,736 | B | E2 raw element bytes | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.body` | 662,410,464 | B | E2 aggregate EKBLOOM container bytes | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.planned` | 566,677,130 | B | E2 aggregate shipped-planner bytes | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.losses` | 98 | objects | E2 per-object losses | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.wins` | 0 | objects | E2 per-object wins | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.exact` | 98 | objects | E2 objects exact on both arms | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.largest_loss` | 30,522,289 | B | E2 largest single per-object loss | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.ratio` | 1.168938 | × | E2 EKBLOOM bytes over shipped-planner bytes | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e2.over_pct` | 16.89 | % | E2 EKBLOOM bytes above the shipped planner | `bench/receipts/ekbloom-space-20261113T081552Z.json` | 20261113T081552Z | f64287c95475 |
| `ekb.e3.objects` | 14 | objects | E3 selected objects | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.logical_bytes` | 583,077,888 | B | E3 declared logical bytes | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.w_foot` | 400,293,613 | B | E3 W (mode-15 tiles) retained footprint | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.a_foot` | 583,083,071 | B | E3 A (raw + manifest + index) retained footprint | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.e_foot` | 458,840,944 | B | E3 E (EKBLOOM containers) retained footprint | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.p95_w` | 6,495,913 | ns | E3 W pooled p95 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.p95_a` | 2,634 | ns | E3 A pooled p95 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.p95_e` | 222,636 | ns | E3 E pooled p95 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.p95_best` | 2,634 | ns | E3 best baseline pooled p95 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.samples_1` | 784 | reads | E3 reads in the 1 B class | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.samples_4096` | 560 | reads | E3 reads in the 4,096 B class | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.samples_65536` | 560 | reads | E3 reads in the 65,536 B class | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.samples_pooled` | 1,904 | reads | E3 pooled reads per arm | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.exact_w` | 1,904 | reads | E3 exact reads, W | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.exact_a` | 1,904 | reads | E3 exact reads, A | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.exact_e` | 1,904 | reads | E3 exact reads, E | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.backend_w_bytes` | 544,033.8 | B | E3 backend bytes per read, W | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.backend_a_bytes` | 20,480.4 | B | E3 backend bytes per read, A | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.backend_e_bytes` | 37,934.3 | B | E3 backend bytes per read, E | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.backend_w_reqs` | 18.84 | requests | E3 backend requests per read, W | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.backend_a_reqs` | 1.00 | requests | E3 backend requests per read, A | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.backend_e_reqs` | 2.00 | requests | E3 backend requests per read, E | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.e_over_w` | 1.14626 | × | E3 E footprint over W footprint | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.e_over_a` | 0.78692 | × | E3 E footprint over A footprint | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.p95_e_over_best` | 84.52 | × | E3 E p95 over the best baseline p95 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.p95_w_over_e` | 29.18 | × | E3 W p95 over E p95 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.exact_total` | 5,712 | reads | E3 exact reads across all three arms | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_00` | 24 | requests | E3 engine-probe requests, selected tensor 00 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_01` | 8 | requests | E3 engine-probe requests, selected tensor 01 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_02` | 24 | requests | E3 engine-probe requests, selected tensor 02 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_03` | 24 | requests | E3 engine-probe requests, selected tensor 03 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_04` | 8 | requests | E3 engine-probe requests, selected tensor 04 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_05` | 24 | requests | E3 engine-probe requests, selected tensor 05 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_06` | 24 | requests | E3 engine-probe requests, selected tensor 06 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_07` | 24 | requests | E3 engine-probe requests, selected tensor 07 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_08` | 8 | requests | E3 engine-probe requests, selected tensor 08 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_09` | 24 | requests | E3 engine-probe requests, selected tensor 09 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_10` | 24 | requests | E3 engine-probe requests, selected tensor 10 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_11` | 8 | requests | E3 engine-probe requests, selected tensor 11 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_12` | 24 | requests | E3 engine-probe requests, selected tensor 12 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_req_13` | 24 | requests | E3 engine-probe requests, selected tensor 13 | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
| `ekb.e3.probe_requests` | 272 | requests | E3 in-memory Engine probe requests | `bench/receipts/ekbloom-read-20261113T083231Z.json` | 20261113T083231Z | ef2aa0a3a21a |
<!-- END GENERATED:perf-catalog-ekbloom -->

## Azure MW2 mixed-language court (X13/N14)

One pinned Azurite emulator on loopback, Rust and Go writer processes through the
opt-in MW2 transaction API. Azurite is not Azure. The positive receipt is
`azure-mw2-20261113T095300Z`; the retained short-grace negative run is
`azure-mw2-20261113T094900Z`; analysis in
[`bench/RESULTS-azure-mw2.md`](../bench/RESULTS-azure-mw2.md).

<!-- BEGIN GENERATED:perf-catalog-azure-mw2 (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `azm2.scenarios` | 8 | scenarios | Azure MW2 scenarios run | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.scenarios_passed` | 8 | scenarios | Azure MW2 scenarios passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.checks` | 167 | checks | Azure MW2 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.checks_passed` | 167 | checks | Azure MW2 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.failures` | 0 | failures | Azure MW2 failures | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.duration_s` | 34.3 | s | Azure MW2 court wall | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.memory_peak_b` | 52,301,824 | B | Azure MW2 court cgroup memory peak | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s1.checks` | 30 | checks | Azure MW2 S1 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s1.passed` | 30 | checks | Azure MW2 S1 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s2.checks` | 28 | checks | Azure MW2 S2 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s2.passed` | 28 | checks | Azure MW2 S2 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s3.checks` | 22 | checks | Azure MW2 S3 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s3.passed` | 22 | checks | Azure MW2 S3 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s4.checks` | 18 | checks | Azure MW2 S4 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s4.passed` | 18 | checks | Azure MW2 S4 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s5.checks` | 18 | checks | Azure MW2 S5 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s5.passed` | 18 | checks | Azure MW2 S5 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s6.checks` | 24 | checks | Azure MW2 S6 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s6.passed` | 24 | checks | Azure MW2 S6 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s7.checks` | 18 | checks | Azure MW2 S7 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s7.passed` | 18 | checks | Azure MW2 S7 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s8.checks` | 9 | checks | Azure MW2 S8 checks | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
| `azm2.s8.passed` | 9 | checks | Azure MW2 S8 checks passed | `bench/receipts/azure-mw2-20261113T103500Z.json` | 20261113T103500Z | 103dd99c7fd9 |
<!-- END GENERATED:perf-catalog-azure-mw2 -->

<!-- BEGIN GENERATED:perf-catalog-azure-mw2-negative (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `azm2n.checks` | 167 | checks | Retained negative run checks | `bench/receipts/azure-mw2-20261113T103700Z.json` | 20261113T103700Z | 103dd99c7fd9 |
| `azm2n.checks_passed` | 163 | checks | Retained negative run checks passed | `bench/receipts/azure-mw2-20261113T103700Z.json` | 20261113T103700Z | 103dd99c7fd9 |
| `azm2n.failures` | 4 | failures | Retained negative run failures | `bench/receipts/azure-mw2-20261113T103700Z.json` | 20261113T103700Z | 103dd99c7fd9 |
| `azm2n.scenarios_passed` | 7 | scenarios | Retained negative run scenarios passed | `bench/receipts/azure-mw2-20261113T103700Z.json` | 20261113T103700Z | 103dd99c7fd9 |
<!-- END GENERATED:perf-catalog-azure-mw2-negative -->

## X04 7B ingestion and streaming mode 9

Every number below comes from `bench/RESULTS-scale-7b.md`'s receipts. The
`scale7b.before.*` rows are the **pre-X04** starting point and carry that
receipt's older revision; every other row is measured on the code revision named
in its own row.

<!-- BEGIN GENERATED:perf-catalog-scale7b (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `scale7b.mode9.small_logical` | 272,269,312 | B | 0.5B embedding logical bytes | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.small_frame` | 180,547,519 | B | 0.5B embedding mode-9 frame bytes | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.small_oneshot_ms` | 478.083 | ms | 0.5B embedding one-shot encode wall | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.small_oneshot_rss` | 444,652 | KiB | 0.5B embedding one-shot peak RSS | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.small_stream_ms` | 386.554 | ms | 0.5B embedding streamed encode wall | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.small_stream_rss` | 179,896 | KiB | 0.5B embedding streamed peak RSS | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.small_rss_ratio` | 2.472 | x | 0.5B embedding peak-RSS ratio one-shot/streamed | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.small_wall_ratio` | 0.809 | x | 0.5B embedding wall ratio streamed/one-shot | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_logical` | 1,089,994,752 | B | 7B lm_head logical bytes | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_frame` | 726,141,127 | B | 7B lm_head mode-9 frame bytes | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_oneshot_ms` | 1,950.528 | ms | 7B lm_head one-shot encode wall | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_oneshot_rss` | 1,776,132 | KiB | 7B lm_head one-shot peak RSS | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_stream_ms` | 1,526.492 | ms | 7B lm_head streamed encode wall | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_stream_rss` | 712,720 | KiB | 7B lm_head streamed peak RSS | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_rss_ratio` | 2.492 | x | 7B lm_head peak-RSS ratio one-shot/streamed | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_wall_ratio` | 0.783 | x | 7B lm_head wall ratio streamed/one-shot | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_analysis_ms` | 269.019 | ms | 7B lm_head streamed counting-pass wall | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_analysis_mbs` | 4,051.7 | MB/s | 7B lm_head streamed counting-pass throughput | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.large_analysis_rss` | 3,856 | KiB | 7B lm_head streamed counting-pass peak RSS | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.mode9.small_analysis_ms` | 67.878 | ms | 0.5B embedding streamed counting-pass wall | `bench/receipts/mode9-stream-20261113T113138Z.json` | 20261113T113138Z | 62845b63ecb5 |
| `scale7b.planned.wall_ms` | 119,174.9 | ms | planned 7B checkpoint write wall | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.mb_s` | 127.8 | MB/s | planned 7B checkpoint write throughput | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.rss_kib` | 442,672 | KiB | planned 7B checkpoint process peak RSS | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.cgroup_peak` | 3,221,225,472 | B | planned 7B checkpoint cgroup memory peak | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.store_bytes` | 10,160,456,499 | B | planned 7B checkpoint store bytes | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.verify_ms` | 51,767.2 | ms | planned 7B in-process verify --full wall | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.publish_ms` | 29,984.4 | ms | planned 7B blob publication wall | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.encode_ms` | 16,797.0 | ms | planned 7B encode wall | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.mode9_ms` | 6,949.9 | ms | planned 7B mode-9 encode wall | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.sync_ms` | 12,523.8 | ms | planned 7B durability barrier | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.shards` | 2,712 | objects | planned 7B shard objects | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.planned.reprs` | 2,032 | representations | planned 7B codec representations | `bench/receipts/scale-7b-20261113T104950Z.json` | 20261113T104950Z | cbfe86f6c90a |
| `scale7b.ingest.wall_ms` | 220,047.8 | ms | streamed 7B ingest wall | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.mb_s` | 69.2 | MB/s | streamed 7B ingest throughput | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.rss_kib` | 2,873,124 | KiB | streamed 7B ingest process peak RSS | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.store_bytes` | 10,162,716,887 | B | streamed 7B ingest store bytes | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.mode9_count` | 240 | tensors | streamed 7B ingest mode-9 tensors | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.zstd` | 59 | tensors | streamed 7B ingest ZSTD tensors | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.raw` | 40 | tensors | streamed 7B ingest raw tensors | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.verify_ms` | 49,296.1 | ms | streamed 7B in-process verify --full wall | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.publish_ms` | 139,191.0 | ms | streamed 7B blob publication wall | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.ingest.mode9_ms` | 14,398.4 | ms | streamed 7B mode-9 encode wall | `bench/receipts/scale-7b-20261113T105331Z.json` | 20261113T105331Z | cbfe86f6c90a |
| `scale7b.before.ingest_exit` | 137 | status | pre-X04 per-tensor planned ingest exit status | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.before.flat_ms` | 1,406,533.6 | ms | pre-X04 fast-raw flat write wall | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.before.flat_rss` | 173,032 | KiB | pre-X04 fast-raw flat write peak RSS | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.before.flat_store` | 15,247,327,175 | B | pre-X04 fast-raw flat store bytes | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.before.packed_ms` | 1,088,654.1 | ms | pre-X04 fast-raw packed write wall | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.before.packed_rss` | 287,420 | KiB | pre-X04 fast-raw packed write peak RSS | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.before.packed_store` | 23,023,484,778 | B | pre-X04 fast-raw packed store bytes | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.before.segments` | 148,694 | publishes | pre-X04 catalog segment publications | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.before.root_publishes` | 911 | publishes | pre-X04 catalog root publications | `bench/receipts/scale-7b-20261110T213731Z.json` | 20261110T213731Z | 118015f364d0 |
| `scale7b.packed.wall_ms` | 258,071.4 | ms | packed 7B write wall | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.mb_s` | 59.0 | MB/s | packed 7B write throughput | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.rss_kib` | 404,812 | KiB | packed 7B write process peak RSS | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.store_bytes` | 15,274,042,488 | B | packed 7B store bytes | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.packed_bytes` | 15,242,664,309 | B | packed 7B pack object bytes | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.members` | 236,237 | members | packed 7B catalogued members | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.packs` | 911 | packs | packed 7B sealed packs | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.flat_members` | 17 | members | packed 7B flat members | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.flat_bytes` | 2,721,040 | B | packed 7B flat member bytes | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.segments` | 256 | publishes | packed 7B catalog segment publications | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.packed.root_publishes` | 1 | publishes | packed 7B catalog root publications | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.catalog.bytes` | 26,707,069 | B | packed 7B catalog bytes on disk | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.catalog.files` | 256 | files | packed 7B catalog segment files | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.catalog.store_bytes` | 15,274,042,488 | B | packed 7B store bytes (catalog phase) | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore1.ms` | 43,452.9 | ms | 7B restore wall, 1 worker | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore1.mbs` | 350.5 | MB/s | 7B restore throughput, 1 worker | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore1.rss` | 2,254,132 | KiB | 7B restore peak RSS, 1 worker | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore4.ms` | 39,276.1 | ms | 7B restore wall, 4 workers | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore4.mbs` | 387.8 | MB/s | 7B restore throughput, 4 workers | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore4.rss` | 2,433,784 | KiB | 7B restore peak RSS, 4 workers | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore8.ms` | 40,303.3 | ms | 7B restore wall, 8 workers | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore8.mbs` | 377.9 | MB/s | 7B restore throughput, 8 workers | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.restore8.rss` | 2,511,704 | KiB | 7B restore peak RSS, 8 workers | `bench/receipts/scale-7b-20261113T105851Z.json` | 20261113T105851Z | cbfe86f6c90a |
| `scale7b.flat.wall_ms` | 783,916.2 | ms | fast-raw flat 7B write wall | `bench/receipts/scale-7b-20261113T110624Z.json` | 20261113T110624Z | cbfe86f6c90a |
| `scale7b.flat.mb_s` | 19.4 | MB/s | fast-raw flat 7B write throughput | `bench/receipts/scale-7b-20261113T110624Z.json` | 20261113T110624Z | cbfe86f6c90a |
| `scale7b.flat.writes` | 236,263 | writes | fast-raw flat 7B backend writes | `bench/receipts/scale-7b-20261113T110624Z.json` | 20261113T110624Z | cbfe86f6c90a |
| `scale7b.flat.sync_ms` | 464,189.0 | ms | fast-raw flat 7B durability barrier | `bench/receipts/scale-7b-20261113T110624Z.json` | 20261113T110624Z | cbfe86f6c90a |
| `scale7b.flat.rss_kib` | 349,608 | KiB | fast-raw flat 7B write peak RSS | `bench/receipts/scale-7b-20261113T110624Z.json` | 20261113T110624Z | cbfe86f6c90a |
| `scale7b.flat.store_bytes` | 15,247,327,175 | B | fast-raw flat 7B store bytes | `bench/receipts/scale-7b-20261113T110624Z.json` | 20261113T110624Z | cbfe86f6c90a |
<!-- END GENERATED:perf-catalog-scale7b -->

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
