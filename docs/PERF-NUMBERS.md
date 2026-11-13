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

## Verification, conformance and claims counts

<!-- BEGIN GENERATED:perf-catalog-verification (scripts/perf_numbers.py) -->
| key | value | unit | what it measures | receipt | stamp | revision |
|---|---:|---|---|---|---|---|
| `verify.courts` | 46 | courts | Courts in the cited aggregate | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.passed` | 46 | courts | Courts passed | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.failed` | 0 | courts | Courts failed | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.skipped` | 0 | courts | Courts skipped | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.stamp` | 20261113T022751Z | text | Aggregate stamp | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `verify.revision` | e67e63186456d8d0915204baa19d65013a6bb281 | text | Aggregate WeftFS revision | `bench/receipts/verify-20261113T022751Z.json` | 20261113T022751Z | e67e63186456 |
| `conformance.vectors` | 420 | vectors | Conformance vectors in the manifest | `conformance/manifest.json` | — | — |
| `conformance.aggregate` | 6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8 | text | Conformance manifest aggregate SHA-256 | `conformance/manifest.json` | — | — |
| `claims.count` | 1,046 | claims | Entries in the claims manifest | `docs/claims.json` | — | — |
| `crash.requested` | 100,000 | iterations | Crash-torture iterations requested | `bench/receipts/crash-torture-20261104T104233Z.json` | 2026-11-04T10:42:33Z | 380ea4754483 |
| `crash.bad` | 0 | states | Crash-torture bad states | `bench/receipts/crash-torture-20261104T104233Z.json` | 2026-11-04T10:42:33Z | 380ea4754483 |
| `tests.rust_store_passed` | 807 | tests | Rust all-targets tests passed | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.rust_store_binaries` | 170 | tests | Rust all-targets test-result lines (one per binary) | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.rust_fuse_passed` | 850 | tests | FUSE all-targets tests passed | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.rust_fuse_failed` | 0 | tests | FUSE all-targets tests failed | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.rust_fuse_ignored` | 2 | tests | FUSE all-targets tests ignored | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `tests.rust_codec_passed` | 290 | tests | WeftCodec all-targets tests passed | `bench/receipts/verify-20261113T022751Z.tests.json` | da84a5061ff4bc25c205c2d5817f9f9f63a6978824e374a7cb4d0a60727d2c0e | bench/receip |
| `artifacts.wheel_bytes` | 115,851 | B | wheel size | `bench/receipts/release-artifacts-20261112T233126Z.json` | 20261112T233126Z | — |
| `artifacts.wheel_sha` | 9b6ff9bbfc92a195408bf411661208a92c6a739d6f7f1b0b8207bb5304464928 | text | wheel sha256 | `bench/receipts/release-artifacts-20261112T233126Z.json` | 20261112T233126Z | — |
| `artifacts.sdist_bytes` | 156,977 | B | sdist size | `bench/receipts/release-artifacts-20261112T233126Z.json` | 20261112T233126Z | — |
| `artifacts.sdist_sha` | cc86abd96365390259b63d585b982698eee4ddafa24ec4ea69ea170a5c388295 | text | sdist sha256 | `bench/receipts/release-artifacts-20261112T233126Z.json` | 20261112T233126Z | — |
| `build.wheel_sha` | 92efd39d2e4732081dccfccbb19c2f2849ff427e9f38d6aed9226624d5ceed06 | — | wheel sha256 | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `build.wheel_bytes` | 115,889 | B | wheel size | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `build.sdist_sha` | 773c317d216b252be36dc6b07d5a1add8ba80c14faf46c7e510011180ec196e2 | — | sdist sha256 | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
| `build.sdist_bytes` | 156,971 | B | sdist size | `bench/receipts/release-artifacts-20261113T011500Z.json` | 20261113T011500Z | 610ce131598b |
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
