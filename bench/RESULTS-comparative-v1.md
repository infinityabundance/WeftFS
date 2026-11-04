# Comparative performance results — v1 (2026-11-03)

**Status:** measured, single-host, court-bounded. This document reports scoped
facts from four new courts: an external SQLite context/query baseline, a
matched Rust/Go WeftCodec court, a guest cold-cache concurrent `fio` court with
ext4/XFS baselines, and an energy/counter probe. It does **not** declare a
winner, a record, a general speedup, or production readiness. Every number
comes from a receipt in `bench/receipts/`; raw samples are in those receipts and
raw logs sit beside them.

The benchmark contract (`bench/CONTRACTS.md`) was fixed before these runs. No
baseline was weakened after the fact, no failed case was deleted, and no unrun
test is called passed.

## 1. Evidence index

| Run | Receipt | SHA-256 | Role |
|---|---|---|---|
| External SQLite, final | `bench/receipts/bench-external-20261104T000122Z.json` | `6264a2c69e47119a804eb3b300970288e00f075d2114690b6087461e1acacb19` | Relational + adjacency baseline, 100/1,000/10,000 |
| External corpus manifest | `bench/receipts/external-corpus-20261104T000122Z.json` | `491f121ccf73abbbe6025a528f400edcf7eb663f2c135974b7c6342db7c21914` | Exact payloads, ObjectIds, relations, snapshots, generations |
| External raw log | `bench/receipts/bench-external-20261104T000122Z.log` | `dc9d59357aac78b43a8d3b9b4c88fc28ad3b4db733a2213c56def7af2fe79e5d` | Commands and console output |
| External SQLite, stability | `bench/receipts/bench-external-20261104T000127Z.json` | `1d400627e0fa2964c9a3801706412120b7bd09084c7a1e7339ca9de4a0c02dfb` | Same-methodology stability comparison |
| External stability log | `bench/receipts/bench-external-20261104T000127Z.log` | `4da675745f43b264b4fb256e820fa052d888842c63db3f012910d03b9f8fbca3` | Raw log |
| Matched codec, final | `bench/receipts/bench-codec-matched-20261103T232158Z.json` | `7008187de4182b1c14b1e08089f34bb8db7eec1fc90460336c0e2e8e3c6e70db` | Rust vs Go CLI, 7 modes, encode/decode, CPU/RSS |
| Matched codec, first | `bench/receipts/bench-codec-matched-20261103T225536Z.json` | `42a84fb74b1cb4804cfc37e7de604bc391125be11e4a05601496fbc9e323ab07` | Stability comparison |
| Matched codec log | `bench/receipts/bench-codec-matched-20261103T232158Z.log` | `ee5bc747e9e3652990eea6fa3f82636273ed13a57eb3496a20c81185475070bb` | Raw court log |
| Cold fio, final | `bench/receipts/bench-fs-cold-20261103T232216Z.json` | `b3b76b8b44d4e3d690c0da0077422fa247f2aa811a5edccca1edaa8962503fa5` | ext4/XFS/Rust/Go, cold/warm, 1/4/16 jobs |
| Cold fio, stability | `bench/receipts/bench-fs-cold-20261103T233740Z.json` | `8e0dea0d2f84f9c619b991fec47a28510a9f0a5f2de5c18ff860387619262de1` | Same-methodology stability comparison |
| Cold fio stability log | `bench/receipts/bench-fs-cold-20261103T233740Z.log` | `59456710e41fb22245265710786dd697f50b2de0608057c83ded689cea8bc296` | Raw log |
| Cold fio log | `bench/receipts/bench-fs-cold-20261103T232216Z.log` | `66ff3df37e1b9bd39bc63938fd3498b3ccd71f41fbbddadee8fd769e55b69b04` | Raw court log |
| Energy probe | `bench/receipts/energy-probe-20261103T225547Z.json` | `d92d74ee5c4f9a105e6faf564d37374dea39dccf6fdf7987e6538ba18c38c150` | `perf`/RAPL availability |
| Rust store, current HEAD | `bench/receipts/bench-store-20261103T235127Z.json` | `f37b9c449c05e8f39f91fa51c9e8f40cc9f9e1ef0989372745a6aa16b69858c8` | Same-revision store side of §3.4 |

The stability receipts (`bench-external-20261104T000127Z.json`,
`bench-codec-matched-20261103T225536Z.json`,
`bench-fs-cold-20261103T233740Z.json`) use the same code and method as their
final counterparts; they are not headline results.

## 2. Environment, revisions, and exact commands

All execution ran inside `scripts/docker-court.sh`; no build, test, or
benchmark ran on the host.

- Host kernel `7.3.1-1-cachyos`; AMD Ryzen 7 9800X3D; 16 host CPUs, 4-CPU
  quota; 131,565,228 KiB host `MemTotal`; Docker server `29.9.2`.
- Court limits in every run: `--memory=3g --memory-swap=3g --pids-limit=1024
  --cpus=4`, `--tmpfs /tmp:size=1g`, `no-new-privileges`, `cap-drop=ALL`.
  `bench` adds `/dev/fuse` + `CAP_SYS_ADMIN` and labelled tmpfs mounts; `vm`
  adds `/dev/kvm` when present.
- Revisions recorded by the receipts: WeftFS
  `2388352b36920feb7378d953b12dbcd0c30ed2b2`, WeftCodec
  `ab50e8b182956444c6d3145299b24d46dad2aeea`, WeftCodec-Go
  `ce2cff9283c2ccb841ee27ea5415ef3cb4691c6a`, WeftFS-Go
  `209e4c30e3e4155ed631ef94a65059ca892fbaad`.
- Bench court image `weft/bench-court:1`, id
  `sha256:e80c438516a509c251d6f3ca4848363a86d12a92287a1d9284117acd9cc0d85a`.
- VM court image `weft/vm-court:1`, id
  `sha256:526b2b9b284dd7d2c3e6ca87289dc42079b3efdf570bd3a4e6975bf2280c60d9`.
- Guest: Debian kernel `6.1.0-54-amd64`, QEMU `7.2.22`, `fio-3.33`,
  `mkfs.xfs 6.1.0`, KVM acceleration. Guest gets 4 vCPUs and 1,536 MiB; the
  host court is still limited to 4 CPUs and 3 GiB.
- External baseline: Python 3.11.2 with SQLite 3.40.1 linked into the
  interpreter.

Exact commands:

```sh
scripts/docker-court.sh bench "bash bench/scripts/run_external_bench.sh"
scripts/docker-court.sh bench "bash bench/scripts/run_matched_codec_bench.sh"
scripts/docker-court.sh bench "python3 bench/scripts/probe_energy.py --out bench/receipts/energy-probe-<UTC>.json"
scripts/docker-court.sh vm \
  "WEFT_FS_COLD_TARGETS=ext4,xfs,rust,go WEFT_FS_COLD_REPS=2 \
   WEFT_FS_COLD_TIMEOUT=1800 bash bench/scripts/vm_fs_court.sh"
```

The external receipt records the exact Python command line, every schema and
index statement, and every query with its parameters and `EXPLAIN QUERY PLAN`
rows. The cold-fio receipt records every `fio` command line in
`targets.*.commands` and every guest serial log path.

## 3. External context/query baseline (SQLite)

### 3.1 Corpus and schema

`examples/bench_external_manifest.rs` emits the same definitions as
`examples/bench_store.rs`: deterministic 32–256-byte payloads, ObjectIds from
`weftfs::identity::object_id_from_seed` (`weft/object/v1` BLAKE3), the same
`refers_to` ring, `tagged_as` even/odd tags (every fifth object untagged),
`owner=team-3` edges, `objects-only`/`half`/`final` snapshots, and the same commit
generations. Payloads are carried as hex in the manifest so the external
baseline cannot silently drift. The manifest SHA-256 is in §1.

Two SQLite variants were built at 100, 1,000, and 10,000 objects:

- **relational**: `objects`, `relations`, `snapshots`, `payloads`; surrogate
  relation ids.
- **adjacency**: integer-keyed `nodes`, an adjacency `edges(src,label,dst,dst_value)`
  table, `snapshots`, `payloads`. This is the **“SQLite relational/adjacency
  baseline”**; it is **not** a dedicated graph database. No Neo4j, SPARQL, or
  other graph/triple store was run.

Indexes (created after bulk load and timed separately):

```sql
-- relational
CREATE INDEX objects_kind ON objects(kind);
CREATE INDEX objects_generation ON objects(generation);
CREATE INDEX relations_predicate_target ON relations(predicate, target_kind, target_value);
CREATE INDEX relations_subject_predicate ON relations(subject, predicate);
-- adjacency
CREATE INDEX nodes_kind ON nodes(kind);
CREATE INDEX nodes_generation ON nodes(generation);
CREATE INDEX edges_label_dst ON edges(label, dst);
CREATE INDEX edges_label_value ON edges(label, dst_value);
CREATE INDEX edges_src_label ON edges(src, label);
```

`PRAGMA journal_mode=WAL; synchronous=FULL; cache_size=-8000; mmap_size=0;
temp_store=MEMORY`. Warm runs use 50 iterations (100/1,000) or 10 (10,000)
after five warmups. Cold runs close the connection, apply
`POSIX_FADV_DONTNEED` to the DB/WAL/SHM files, reopen, and time one query.
This is **file-scoped page-cache eviction**, not a device-cache drop; the VM
fio court performs a real guest `drop_caches`. Every result count was asserted
(0, 1, n, even-tag, and one-hop) before a sample was accepted.

### 3.2 Warm query medians (µs)

| Objects | Variant | q0 tag absent | q1 relation incoming | qn kind=file | qeven tag | qhop one-hop | qoutgoing | snapshot resolve | snapshot visible |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 100 | relational | 1.29 | 1.63 | 15.35 | 9.83 | 1.66 | 1.51 | 1.36 | 2.75 |
| 100 | adjacency | 1.19 | 1.45 | 15.56 | 8.71 | 1.46 | 1.41 | 1.39 | 2.75 |
| 1,000 | relational | 1.30 | 1.69 | 137.34 | 86.71 | 1.72 | 1.52 | 1.37 | 16.60 |
| 1,000 | adjacency | 1.22 | 1.51 | 145.57 | 79.31 | 1.52 | 1.44 | 1.41 | 16.57 |
| 10,000 | relational | 1.43 | 1.81 | 1,504.77 | 969.23 | 1.79 | 1.60 | 1.35 | 154.40 |
| 10,000 | adjacency | 1.23 | 1.54 | 1,509.91 | 845.04 | 1.51 | 1.45 | 1.35 | 150.62 |

Cold medians at 10,000, same column order (µs): relational `207.25, 527.78,
5,155.73, 6,603.05, 522.34, 520.26, 137.79, 379.88`; adjacency `156.09, 448.27,
6,034.81, 2,514.72, 377.10, 413.42, 160.24, 422.74`. The cold penalty is real
for range scans and multi-result tags; point lookups remain sub-millisecond.

### 3.3 Updates, index build, and bytes on disk

| Objects | Variant | Bulk load ms | Index build ms | Index bytes | Churn add ms | Churn delete ms | Durable insert ms | Total bytes |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 100 | relational | 1.45 | 5.83 | 8,192 | 1.14 | 1.14 | 1.15 | 208,896 |
| 100 | adjacency | 1.41 | 6.12 | 0 | 1.16 | 1.15 | 1.14 | 159,744 |
| 1,000 | relational | 4.51 | 7.78 | 290,816 | 1.14 | 1.14 | 1.15 | 1,249,280 |
| 1,000 | adjacency | 3.64 | 8.92 | 126,976 | 1.16 | 1.15 | 1.14 | 733,184 |
| 10,000 | relational | 35.96 | 24.96 | 3,137,536 | 1.15 | 1.14 | 0.77 | 11,870,208 |
| 10,000 | adjacency | 29.20 | 22.11 | 1,396,736 | 1.17 | 1.15 | 0.77 | 6,447,104 |

“Durable insert” is a per-object insert with its own committed transaction and
`fsync`, on a scratch table, sampled over the first 100 objects. “Bulk load” is
one transaction and is not a durability equivalent of WeftFS per-object commits.
Adjacency index bytes at 100 objects are zero because the new indexes fit in
pages already allocated; this is recorded as measured, not rounded.

### 3.4 Comparison with the Rust store

The Rust store receipt is `bench/receipts/bench-store-20261103T235127Z.json`
(revision `2388352`, same corpus definitions used by the manifest; wall time
487.9 s, peak RSS 169,356 KiB). Medians:

| Objects | Store q0 | Store q1 | Store qn | Store qeven | Store qhop | Store churn add/del | Store relation add | Store snapshot create | Store open/replay | Store bytes |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 100 | 26.9 µs | 25.3 µs | 21.0 µs | 31.6 µs | 25.8 µs | 2.29/2.28 ms | 2.28 ms | 4.51 ms | 6.51 ms | 183,562 |
| 1,000 | 2.43 ms | 2.32 ms | 180.4 µs | 2.42 ms | 2.29 ms | 2.64/2.64 ms | 2.54 ms | 4.83 ms | 319.1 ms | 1,630,681 |
| 10,000 | 494.4 ms | 465.7 ms | 1.90 ms | 487.4 ms | 457.6 ms | 5.94/6.02 ms | 4.68 ms | 7.87 ms | 32.83 s | 52,582,315 |

Scoped facts, not a verdict:

- SQLite point/relation/tag lookups are ~1.2–1.8 µs warm and ~0.14–0.60 ms
  cold at 10,000; the Rust store’s equivalent relation/tag selectors are
  ~457.6–494.4 ms because evaluation scans relations per object. This is the
  largest published loss in this pass.
- The n-result `kind == "file"` query is close: 1.90 ms in the store versus
  1.50 ms relational / 1.51 ms adjacency at 10,000.
- Relation insert/delete commits cost 5.94/6.02 ms in the store versus
  1.14–1.17 ms in SQLite at 10,000. The store is committing a durable
  transaction; SQLite is also `synchronous=FULL`, but its WAL commit is cheaper.
- The store’s replay/open at 10,000 is 32.83 s versus a 24.96 ms SQLite index
  build (relational) / 22.11 ms (adjacency). These are not the same operation
  (the store validates every manifest and commit), so the comparison is only
  an order-of-magnitude statement.
- Whole-store bytes before verification are 52.58 MB at 10,000 versus 11.87 MB
  (relational) and 6.45 MB (adjacency). The store also carries manifests,
  commits, snapshots, and version records; SQLite carries only the listed
  tables. This is a real space loss, not an equivalent-format comparison.
- Snapshot resolve is only measured on the SQLite side (1.35 µs warm). The
  store’s measured snapshot **create** is 4.53–6.95 ms; create and resolve are
  not the same operation and are not presented as equivalent.
- No authorization, Context Root, ambiguity, or evidence semantics are modelled
  by either SQLite variant. This is a physical-access baseline only.

## 4. Matched Rust/Go WeftCodec court

Both implementations run as CLI processes under the same court limits, same
fixtures, and default normative limits. Encode and decode are separate
processes; wall time includes spawn, dynamic loading, argument parsing, file
I/O, and codec work. Per-process CPU user/sys and max RSS come from `wait4`
rusage. Every timed decode was compared byte-for-byte with the expected bytes;
every frame was also cross-decoded by the other implementation. The final run
used 7 iterations for 1–4 MiB fixtures and 15 for tensor/token fixtures.

| Mode | Impl | Encode median ms | Encode p95 ms | Decode median ms | Decode p95 ms | Enc CPU ms | Dec CPU ms | Enc RSS KiB | Dec RSS KiB | Frame bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| raw | Rust | 2.768 | 3.069 | 2.255 | 2.382 | 2.705 | 2.184 | 35,448 | 35,448 | 4,194,404 |
| raw | Go | 2.476 | 2.565 | 2.048 | 2.301 | 3.687 | 3.351 | 42,812 | 42,812 | 4,194,404 |
| zstd | Rust | 7.463 | 7.633 | 3.898 | 4.123 | 7.367 | 3.790 | 42,812 | 42,812 | 746,462 |
| zstd | Go | 10.663 | 10.798 | 4.683 | 4.849 | 11.675 | 6.125 | 42,812 | 42,812 | 734,433 |
| reference | Rust | 0.927 | 0.975 | 0.982 | 1.143 | 0.880 | 0.944 | 42,812 | 42,812 | 148 |
| reference | Go | 0.951 | 1.053 | 0.873 | 1.110 | 1.289 | 1.420 | 42,812 | 42,812 | 148 |
| delta | Rust | 37.699 | 38.550 | 1.609 | 1.661 | 37.543 | 1.511 | 82,088 | 42,812 | 80,070 |
| delta | Go | 70.901 | 72.886 | 3.612 | 3.866 | 63.535 | 2.168 | 120,368 | 42,812 | 80,070 |
| dictionary | Rust | 10.533 | 11.658 | 4.096 | 4.124 | 2.573 | 1.306 | 42,812 | 42,812 | 164,193 |
| dictionary | Go | 3.930 | 12.642 | 1.941 | 3.115 | 4.320 | 2.425 | 42,812 | 42,812 | 167,755 |
| tensor | Rust | 0.379 | 0.414 | 0.377 | 0.420 | 0.342 | 0.340 | 42,812 | 42,812 | 4,373 |
| tensor | Go | 0.460 | 0.525 | 0.468 | 0.524 | 0.474 | 0.480 | 42,812 | 42,812 | 4,373 |
| tokens | Rust | 0.423 | 0.452 | 0.391 | 0.413 | 0.392 | 0.355 | 42,812 | 42,812 | 10,728 |
| tokens | Go | 0.788 | 0.851 | 0.499 | 0.551 | 0.828 | 0.545 | 42,812 | 42,812 | 10,728 |

Cross-decode returned `true` in all 14 frame/direction cells (7 modes × 2
directions). Process spawn overhead measured with `--help`: Rust 0.326 ms
median wall, Go 0.387 ms; not subtracted. Go runtime threads can make process
CPU exceed wall time; CPU time is not energy.

Losses and scoped facts:

- Rust encodes `zstd` 4 MiB in 7.46 ms versus Go 10.66 ms; Rust decodes in
  3.90 ms versus 4.68 ms. Rust `delta` encode is 37.70 ms versus Go 70.90 ms,
  and Rust `delta` decode 1.61 ms versus 3.61 ms. These are CLI-process
  measurements, not library benchmarks.
- Go produces a smaller `zstd` frame (734,433 vs 746,462 bytes) and a larger
  `dictionary` frame (167,755 vs 164,193 bytes). Frame bytes are not a
  speed claim.
- Go is faster on this run for `raw` encode/decode and `dictionary` encode
  median, but Rust is faster on `dictionary` decode and `tensor`/`tokens`.
  `dictionary` Rust encode has a large wall/CPU gap (10.53 ms wall, 2.57 ms
  CPU) and a 22% run-to-run change; the variance is published in §7, not
  explained away.
- Rust `delta` encode peaks at 82,088 KiB RSS; Go at 120,368 KiB.
- No library-level Go/Rust codec comparison was run in this pass. The existing
  in-process Rust codec receipt uses a different corpus and is not directly
  comparable.

## 5. Cold-cache and concurrent fio

### 5.1 Rig and method

A QEMU guest boots the Debian kernel/initramfs from the vm court image with a
read-only ext4 root image built in the court. The root contains busybox, fio,
both WeftFS CLIs, and only the kernel modules needed (`fuse.ko`, `xfs.ko`,
`btrfs.ko`, `libcrc32c.ko`, `crc32c_generic.ko`, `crc32c-intel.ko`, plus the
OpenZFS DKMS module stack and `zpool`/`zfs`). The data disk is raw virtio-blk
with QEMU `cache=none`; the results disk is a small ext4 image extracted with
`debugfs` (no loop mount, no extra capability).

- Cold: guest runs `sync; echo 3 > /proc/sys/vm/drop_caches` immediately before
  the run. Warm: the next run on the same files with no drop.
- `fio` psync, `direct=0` (buffered), `group_reporting`,
  `percentile_list=50:95:99`, `randrepeat=1`. Sizes per job: 8 MiB sequential,
  512 KiB random, 64 KiB with `--fsync=1`. Write workloads also use
  `--end_fsync=1`; `fsync-write-4k` uses `--fsync=1`.
- `numjobs` 1/4/16; two repetitions per cell. Medians are across the two
  per-run values; each per-run value is retained in the receipt.
- ext4 and XFS are guest-kernel 6.1.0-54-amd64. Rust and Go WeftFS are mounted
  over FUSE inside the guest; the store and mount live on an ext4 data disk.
  Mount options recorded in the receipt: Rust
  `rw,nosuid,nodev,relatime,user_id=0,group_id=0,default_permissions,allow_other`;
  Go `rw,nosuid,nodev,relatime,user_id=0,group_id=0,default_permissions,max_read=131072`.
  The `allow_other`/`max_read` difference is an implementation default and is a
  limitation of this comparison.
- `fio` write buffers default to zeros; WeftFS chunk dedup can therefore reduce
  stored bytes. This receipt measures latency/IOPS, **not** storage
  amplification. Buffered-write IOPS are submission IOPS; `end_fsync` flushes
  at job end, not per write.

### 5.2 Results (medians of two repetitions)

| Target | Workload | nj | Cold IOPS | Warm IOPS | Cold clat p50/p95/p99 µs | Warm clat p50/p95/p99 µs | Cold sync p99 µs |
|---|---|---:|---:|---:|---|---|---:|
| ext4 | fsync-write-4k | 1 | 586.1 | 701.0 | 6.1/18.3/18.3 | 4.6/9.0/9.0 | 2,736.1 |
| ext4 | fsync-write-4k | 4 | 900.4 | 1,014.3 | 67.7/84.5/89.1 | 62.8/86.0/96.8 | 6,225.9 |
| ext4 | fsync-write-4k | 16 | 3,204.5 | 3,202.0 | 80.9/135.9/235.0 | 71.0/160.8/243.7 | 8,011.8 |
| ext4 | rand-read-4k | 1 | 9,494.5 | 9,494.5 | 96.8/121.9/154.6 | 96.8/117.8/173.1 | n/a |
| ext4 | rand-read-4k | 4 | 39,384.6 | 39,384.6 | 34.6/99.3/119.8 | 33.8/99.3/110.6 | n/a |
| ext4 | rand-read-4k | 16 | 146,285.7 | 146,285.7 | 54.5/154.6/350.2 | 57.3/151.6/350.2 | n/a |
| ext4 | rand-write-4k | 1 | 37,333.3 | 42,666.7 | 0.6/1.1/1.5 | 0.7/1.1/1.4 | n/a |
| ext4 | rand-write-4k | 4 | 100,571.4 | 115,200.0 | 0.7/1.1/4.6 | 0.6/1.1/2.6 | n/a |
| ext4 | rand-write-4k | 16 | 274,285.7 | 227,555.6 | 0.7/1.3/6.8 | 0.6/1.1/6.4 | n/a |
| ext4 | seq-read-1m | 1 | 1,400.0 | 1,400.0 | 656.4/1,642.5/1,642.5 | 674.8/1,669.1/1,669.1 | n/a |
| ext4 | seq-read-1m | 4 | 3,607.8 | 3,508.8 | 1,043.5/3,026.9/3,625.0 | 878.6/2,977.8/4,616.2 | n/a |
| ext4 | seq-read-1m | 16 | 5,760.0 | 6,568.4 | 1,671.2/6,750.2/7,979.0 | 1,255.4/6,225.9/7,602.2 | n/a |
| ext4 | seq-write-1m | 1 | 1,142.9 | 1,071.4 | 132.1/167.9/167.9 | 143.4/176.1/176.1 | n/a |
| ext4 | seq-write-1m | 4 | 2,084.0 | 1,830.1 | 194.6/282.1/294.9 | 150.5/237.6/244.2 | n/a |
| ext4 | seq-write-1m | 16 | 2,783.9 | 2,876.8 | 177.2/299.0/479.2 | 180.2/290.8/358.4 | n/a |
| xfs | fsync-write-4k | 1 | 692.8 | 888.9 | 5.6/12.4/12.4 | 4.2/11.9/11.9 | 2,707.5 |
| xfs | fsync-write-4k | 4 | 1,357.2 | 1,777.8 | 7.0/17.2/25.2 | 6.2/13.3/26.1 | 5,505.0 |
| xfs | fsync-write-4k | 16 | 1,846.4 | 2,491.3 | 7.0/19.2/27.9 | 6.9/18.7/26.2 | 25,395.2 |
| xfs | rand-read-4k | 1 | 9,494.5 | 9,142.9 | 97.3/116.7/151.6 | 97.8/120.8/140.0 | n/a |
| xfs | rand-read-4k | 4 | 39,384.6 | 39,384.6 | 35.3/100.4/117.2 | 38.7/103.9/125.7 | n/a |
| xfs | rand-read-4k | 16 | 146,285.7 | 151,912.1 | 59.6/162.8/474.1 | 56.3/135.9/263.7 | n/a |
| xfs | rand-write-4k | 1 | 48,000.0 | 42,666.7 | 0.6/1.1/4.4 | 0.6/1.0/1.2 | n/a |
| xfs | rand-write-4k | 4 | 136,533.3 | 170,666.7 | 0.7/1.1/1.4 | 0.6/1.0/1.4 | n/a |
| xfs | rand-write-4k | 16 | 316,952.4 | 341,333.3 | 0.6/1.1/5.7 | 0.6/1.1/2.7 | n/a |
| xfs | seq-read-1m | 1 | 2,000.0 | 1,800.0 | 360.4/852.0/852.0 | 380.9/905.2/905.2 | n/a |
| xfs | seq-read-1m | 4 | 4,571.4 | 4,571.4 | 464.9/1,564.7/1,646.6 | 490.5/1,409.0/1,564.7 | n/a |
| xfs | seq-read-1m | 16 | 6,568.4 | 6,400.0 | 1,515.5/6,651.9/7,864.3 | 1,695.7/5,799.9/10,305.5 | n/a |
| xfs | seq-write-1m | 1 | 1,333.3 | 1,333.3 | 117.5/142.6/142.6 | 116.7/134.4/134.4 | n/a |
| xfs | seq-write-1m | 4 | 1,888.9 | 2,297.4 | 192.5/292.9/299.0 | 122.6/194.6/199.7 | n/a |
| xfs | seq-write-1m | 16 | 2,702.2 | 2,915.1 | 170.0/284.7/531.5 | 162.8/265.7/311.3 | n/a |
| rust | fsync-write-4k | 1 | 82.1 | 83.6 | 11,599.9/15,401.0/15,401.0 | 11,534.3/15,269.9/15,269.9 | 36.7 |
| rust | fsync-write-4k | 4 | 81.9 | 55.1 | 35,127.3/50,855.9/60,686.3 | 34,603.0/247,595.0/522,059.8 | 34,275.3 |
| rust | fsync-write-4k | 16 | 54.9 | 74.8 | 124,256.3/920,125.4/1,131,937.8 | 144,703.5/176,160.8/188,743.7 | 519,831.6 |
| rust | rand-read-4k | 1 | 14,476.2 | 18,285.7 | 46.1/247.4/296.1 | 46.3/55.3/86.0 | n/a |
| rust | rand-read-4k | 4 | 22,857.1 | 28,444.4 | 136.2/360.4/623.6 | 136.2/149.5/157.7 | n/a |
| rust | rand-read-4k | 16 | 26,426.9 | 28,249.6 | 553.0/766.0/1,091.6 | 553.0/581.6/602.1 | n/a |
| rust | rand-write-4k | 1 | 80.2 | 81.5 | 11,993.1/15,007.7/16,318.5 | 11,862.0/14,680.1/16,613.4 | n/a |
| rust | rand-write-4k | 4 | 77.5 | 73.2 | 51,118.1/62,652.4/72,876.0 | 54,526.0/66,453.5/72,351.7 | n/a |
| rust | rand-write-4k | 16 | 73.9 | 73.8 | 218,103.8/231,735.3/240,123.9 | 218,103.8/231,735.3/243,269.6 | n/a |
| rust | seq-read-1m | 1 | 895.2 | 1,600.0 | 2,649.1/3,881.0/3,881.0 | 466.9/679.9/679.9 | n/a |
| rust | seq-read-1m | 4 | 873.4 | 2,285.7 | 5,169.2/7,217.2/7,258.1 | 1,597.4/1,777.7/1,818.6 | n/a |
| rust | seq-read-1m | 16 | 1,320.1 | 2,509.8 | 12,124.2/13,303.8/13,697.0 | 6,193.2/6,291.5/6,324.2 | n/a |
| rust | seq-write-1m | 1 | 9.9 | 10.5 | 104,857.6/123,469.8/123,469.8 | 97,517.6/109,838.3/109,838.3 | n/a |
| rust | seq-write-1m | 4 | 11.1 | 12.3 | 360,710.1/531,628.0/565,182.5 | 284,688.4/523,239.4/583,008.3 | n/a |
| rust | seq-write-1m | 16 | 12.1 | 11.8 | 931,135.5/1,400,897.5/1,484,783.6 | 1,010,827.3/1,358,954.5/1,442,840.6 | n/a |
| go | fsync-write-4k | 1 | 80.2 | 79.8 | 11,927.6/15,007.7/15,007.7 | 12,058.6/15,466.5/15,466.5 | 35.6 |
| go | fsync-write-4k | 4 | 80.8 | 82.3 | 47,448.1/79,691.8/105,381.9 | 42,729.5/67,764.2/100,663.3 | 78.5 |
| go | fsync-write-4k | 16 | 82.5 | 73.3 | 180,355.1/199,229.4/213,909.5 | 200,278.0/230,686.7/249,561.1 | 149.2 |
| go | rand-read-4k | 1 | 6,909.1 | 8,266.7 | 100.9/357.9/588.8 | 101.9/191.5/289.3 | n/a |
| go | rand-read-4k | 4 | 16,000.0 | 17,970.4 | 175.1/514.0/1,001.5 | 174.1/491.5/663.6 | n/a |
| go | rand-read-4k | 16 | 20,086.2 | 19,520.7 | 442.4/2,474.0/3,899.4 | 469.0/2,408.4/4,137.0 | n/a |
| go | rand-write-4k | 1 | 73.8 | 74.0 | 13,172.7/16,613.4/18,087.9 | 13,172.7/16,252.9/17,432.6 | n/a |
| go | rand-write-4k | 4 | 50.5 | 67.1 | 60,293.1/523,501.6/657,719.3 | 61,866.0/105,381.9/121,110.5 | n/a |
| go | rand-write-4k | 16 | 68.6 | 68.3 | 233,832.4/253,755.4/276,824.1 | 235,929.6/251,658.2/272,105.5 | n/a |
| go | seq-read-1m | 1 | 231.9 | 307.7 | 5,595.1/6,414.3/6,414.3 | 3,113.0/3,670.0/3,670.0 | n/a |
| go | seq-read-1m | 4 | 404.4 | 633.7 | 5,832.7/14,254.1/14,581.8 | 5,931.0/7,372.8/7,864.3 | n/a |
| go | seq-read-1m | 16 | 783.1 | 917.6 | 15,859.7/26,607.6/33,751.0 | 15,663.1/22,675.5/27,394.0 | n/a |
| go | seq-write-1m | 1 | 3.0 | 3.2 | 320,864.3/377,487.4/377,487.4 | 310,378.5/325,058.6/325,058.6 | n/a |
| go | seq-write-1m | 4 | 3.2 | 3.1 | 1,216,348.2/1,308,622.8/1,325,400.1 | 1,300,234.2/1,358,954.5/1,375,731.7 | n/a |
| go | seq-write-1m | 16 | 2.4 | 3.5 | 4,529,848.3/10,921,967.6/10,955,522.0 | 4,529,848.3/4,697,620.5/4,697,620.5 | n/a |

### 5.3 Losses and limitations

- WeftFS sequential 1 MiB writes are 9.9–12.1 IOPS (Rust) and 2.4–3.2 IOPS
  (Go) versus 1,143–2,784 (ext4) and 1,333–2,702 (XFS). The store rewrites the
  whole file and publishes a commit per write; the gap is two to three orders
  of magnitude.
- WeftFS 4 KiB random writes are 73.9–80.2 IOPS (Rust) and 50.5–68.6 IOPS (Go)
  versus 37,333–316,952 IOPS for ext4/XFS. The ext4/XFS figure is buffered
  submission throughput with an end-of-job flush; it is not a device write
  rate. The WeftFS figure is its real FUSE write path.
- `fsync`-per-write is 80–83 IOPS for both WeftFS implementations at `numjobs`
  1, but tail latency explodes with concurrency: in the headline run Rust p99
  reaches 1.13 s at `numjobs=16`, Go 214 ms; the stability run shows the same
  tail instability. ext4/XFS sustain 586–3,205 / 693–2,846 IOPS with p99
  between 12 µs and 25 ms. This is the most severe tail loss.
- 4 KiB random reads: Rust is faster than ext4/XFS at `numjobs=1`
  (14,476 vs 9,495 IOPS) but loses at `numjobs=16` (26,427 vs 146,286). Go is
  slower than both baselines at every concurrency level.
- Sequential reads: Rust cold reaches 873–1,320 IOPS and Go 232–783; warm
  improves Rust to 1,600–2,510 and Go 308–918. ext4/XFS are 1,400–6,568.
  Cold-cache penalty is visible and published.
- ext4 versus XFS: XFS wins random writes and `fsync` at `numjobs=16` in this
  guest; ext4 wins some cold read tails. No general ranking is claimed.
- ZFS (2026-11-04, historical): `bench-fs-cold-20261104T014727Z.json` ran
  ext4/XFS/ZFS with a **file-backed vdev** on an ext4 data disk
  (`zpool create ... /data/zfs-vdev.img`). That result remains separately
  labelled and is superseded for the whole-disk question by §5.4. The file
  target was needed because the minimal guest ran no udev; libzfs' partition
  wait could not see its own GPT partition. ZFS reads in that run are ARC-served
  because `drop_caches` does not drop the ARC. No ZFS ranking is claimed.
- **Whole-disk ZFS (2026-11-04):** §5.4. The vdev is the raw `/dev/vdb` block
  device passed to `zpool create`; no partition, loop device, or file-backed
  vdev is used. ZFS creates and owns its GPT partition `/dev/vdb1`. The guest
  runs the stock Debian `systemd-udevd 252.39` so libzfs' libudev wait can
  complete. Without udevd the exact failure is `cannot label 'vdb': failed to
  detect device partitions on '/dev/vdb1': 19` and the pool is not created; no
  substitute is reported as a whole-disk result.
- **Btrfs (2026-11-04):** §5.4. `mkfs.btrfs` (btrfs-progs 6.2) on the host,
  guest `btrfs.ko`, mount options `rw,relatime,space_cache=v2,subvolid=5`.
  Cold reads use the guest `drop_caches`; Btrfs page cache is evictable in a way
  the ZFS ARC is not. No ranking is claimed.
- The Rust and Go mounts do not have identical options (`allow_other` vs
  `max_read=131072`); both are buffered FUSE mounts, but this is not a
  perfectly controlled mount-option comparison.
- FUSE direct I/O and same-inode concurrent direct I/O were not exercised.
  The known kernel 7.3 direct-I/O EIO path is not part of this court; the guest
  kernel is 6.1.0-54-amd64.
- `fio` write buffers default to zeros, and WeftFS deduplicates chunks. The
  write results measure the write/commit path, not storage growth.
- Two repetitions per cell are enough for medians, not for trustworthy p99
  tails; §7 quantifies the tail variance.

### 5.4 Whole-disk ZFS and Btrfs (2026-11-04)

Receipt: `bench/receipts/bench-fs-cold-20261104T071922Z.json`. One repetition
per cell, `numjobs` 1/4/16, the same `fio` matrix, data disk raw virtio-blk with
`cache=none`. Targets: ext4, XFS, Btrfs, `zfs-whole`. All 30 fio runs per target
completed. Guest kernel 6.1.0-54-amd64; OpenZFS 2.1.11-1+deb12u1 (DKMS);
btrfs-progs 6.2; QEMU 7.2.22.

ZFS construction in the guest (`zfs-whole`):

```
insmod /lib/modules/6.1.0-54-amd64/updates/dkms/zfs.ko
mknod /dev/zfs c 10 <zfs-misc-minor>
/usr/lib/systemd/systemd-udevd --daemon
udevadm trigger --action=add --subsystem-match=block
zpool create -f -m /data/tank tank /dev/vdb
zfs create tank/fio
# ... fio on /data/tank/fio ...
zpool export tank
```

`zpool status -P` shows `NAME /dev/vdb1`; `/dev/vdb` is what was passed to
`zpool`, and `/dev/vdb1` is the GPT partition ZFS created inside that whole
disk. `zpool_create_vdev=/dev/vdb` and `zpool_status_vdev=/dev/vdb1` are both
recorded in the guest status. No file-backed vdev is used for this target.

| Target | Workload | nj | Cold IOPS | Warm IOPS |
|---|---|---:|---:|---:|
| ext4 | seq-write-1m | 1 | 1,142.9 | 1,142.9 |
| XFS | seq-write-1m | 1 | 1,142.9 | 1,333.3 |
| Btrfs | seq-write-1m | 1 | 1,142.9 | 1,000.0 |
| ZFS whole-disk | seq-write-1m | 1 | 1,333.3 | 1,333.3 |
| ext4 | fsync-write-4k | 1 | 410.3 | 640.0 |
| XFS | fsync-write-4k | 1 | 390.2 | 941.2 |
| Btrfs | fsync-write-4k | 1 | 457.1 | 484.8 |
| ZFS whole-disk | fsync-write-4k | 1 | 800.0 | 842.1 |
| ext4 | rand-read-4k | 1 | 9,846.2 | 9,846.2 |
| XFS | rand-read-4k | 1 | 9,846.2 | 9,846.2 |
| Btrfs | rand-read-4k | 1 | 8,533.3 | 8,533.3 |
| ZFS whole-disk | rand-read-4k | 1 | 128,000.0 | 128,000.0 |
| ext4 | rand-write-4k | 1 | 42,666.7 | 32,000.0 |
| XFS | rand-write-4k | 1 | 42,666.7 | 64,000.0 |
| Btrfs | rand-write-4k | 1 | 42,666.7 | 32,000.0 |
| ZFS whole-disk | rand-write-4k | 1 | 42,666.7 | 42,666.7 |
| ext4 | fsync-write-4k | 16 | 3,160.5 | 3,368.4 |
| XFS | fsync-write-4k | 16 | 1,855.1 | 2,169.5 |
| Btrfs | fsync-write-4k | 16 | 911.0 | 977.1 |
| ZFS whole-disk | fsync-write-4k | 16 | 6,736.8 | 6,243.9 |
| ext4 | rand-read-4k | 16 | 157,538.5 | 157,538.5 |
| XFS | rand-read-4k | 16 | 146,285.7 | 146,285.7 |
| Btrfs | rand-read-4k | 16 | 107,789.5 | 120,470.6 |
| ZFS whole-disk | rand-read-4k | 16 | 682,666.7 | 2,048,000.0 |

**Losses and caveats, stated plainly:**

- ZFS random reads are **ARC-served** and are not a cold-disk comparison. At
  `numjobs=1` cold and warm are both 128,000 because `drop_caches` does not
  evict the ARC; at `numjobs=16` cold is 682,666.7 and warm is 2,048,000. No
  ZFS read or overall ranking is claimed.
- ZFS was created without an explicit `-O compression=off`; OpenZFS's
  zero-block elimination can therefore affect all-zero fio writes. This is a
  property of the target configuration, not a comparative claim.
- Btrfs fsync-write at `numjobs=16` is 911.0 IOPS versus ext4 3,160.5 and XFS
  1,855.1. Btrfs is not the fastest or slowest on every workload; no ranking is
  claimed.
- ZFS's 6,736.8 fsync-write IOPS at `numjobs=16` is the observed pool/ZIL
  behaviour in this guest; it is not evidence of physical-media durability and
  is not compared to a power-loss test.
- One repetition per cell is enough for scoped medians, not for trustworthy
  tails. Tail percentiles are retained in the receipt and are not summarized
  here.
- The guest runs `systemd-udevd` only for the `zfs-whole` target so libzfs can
  observe its own GPT partition. This is guest-side udev, not a host partition
  or a file-backed vdev. The no-udev failure is recorded above.

### 5.5 Block-layer lying-disk model (dm-flakey and blkdebug, 2026-11-04)

Receipt: `bench/receipts/blkdebug-20261104T072008Z.json`. `scripts/blkdebug_court.sh`
runs in the vm court, prepares one clean store base image, copies it per
iteration, boots the guest with a block-layer fault, waits for a returned
commit, sleeps a seeded delay, `SIGKILL`s QEMU, reboots, and verifies with Rust
and Go against the exact expected whole generation. 30 iterations: 5 per mode.

| Mode | Iterations | old | new | lost | partial | fault observed |
|---|---:|---:|---:|---:|---:|---:|
| control (no fault) | 5 | 5 | 0 | 0 | 0 | 0 |
| dm-flakey silent write drop | 5 | 0 | 0 | 5 | 0 | 5 (by loss) |
| dm-flakey write EIO | 5 | 5 | 0 | 0 | 0 | 5 (COMMIT_FAIL) |
| QEMU snapshot total write drop | 5 | 0 | 0 | 5 | 0 | 5 (by loss) |
| blkdebug write-error | 5 | — | — | — | 0 | blocked at mount |
| blkdebug flush-error | 5 | — | — | — | 0 | blocked at mount |

- **dm-flakey silent write drop** is the lying-disk result. The guest creates
  `flakey /dev/vdb 0 2 3600 1 drop_writes`; the 2-second up interval covers
  ext4 mount, then writes are acknowledged and silently discarded. Every
  iteration lost acknowledged commits: recovered generation 87/89/89/88/89 vs
  last returned 319/218/334/292/246, i.e. 232/129/245/204/157 commits lost.
  Both implementations recovered the same generation with `verify --full`
  clean and read the exact expected bytes. **0 partial states.** This is the
  expected consequence of a disk that lies about writes, not a WeftFS
  consistency failure.
- **dm-flakey write EIO** failed the first write after the 2-second up
  interval every time. The store returned a failed commit (one `COMMIT_FAIL`
  per iteration), recovered generation equalled the last returned generation
  in all five (88/79/8/7/6), `verify --full` was clean, and no acknowledged
  commit was lost. This is fail-closed behaviour at the filesystem/block
  boundary.
- **QEMU snapshot total write drop** (`snapshot=on`) acknowledged tens to
  hundreds of commits into a volatile overlay that `SIGKILL` discarded. Every
  iteration recovered generation 2, the prepared base, and lost all
  acknowledged commits (last returned 52/77/205/182/128). 0 partial. This is
  a total write-drop model, not selective media loss.
- **blkdebug write-error and flush-error are blocked, not passed.** QEMU 7.2
  blkdebug arms at device open, so the first injected write is ext4's mount
  superblock at block 0. The guest log is exactly `I/O error, dev vdb, sector 0
  op 0x1:(WRITE)` then `JBD2: recovery failed` / `EXT4-fs (vdb): error loading
  journal`; the guest never reaches `GUEST_WRITING`. All 10 attempts are
  recorded as blocked. No result is invented for these modes.
- **What blkdebug can and cannot express here:** accepted events are `read_aio`,
  `write_aio`, and `flush_to_os`; `flush_do_sync` is rejected, and a config with
  `errno=0` still writes the bytes (tested with `qemu-io`). There is no silent
  selective write-drop primitive. The arm-after-mount equivalent is dm-flakey,
  which is what the data matrix uses.
- **Claim boundary:** this is a **block-layer lying-disk model combined with
  SIGKILL and reboot**, not physical media power loss. Host page cache is not
  dropped. The next section records the exact host-cache blocker.

### 5.6 Host page-cache loss attempt

Receipt: `bench/receipts/hostcache-probe-20261104T072314Z.json`, run in the vm
court with the usual `--cap-drop=ALL` and no added capabilities.

- `/proc/sys/vm/drop_caches` exists (mode `200`) but the container cannot write
  it: `writable_by_container=false`, `CapEff=0000000000000000`, and the exact
  kernel refusal is `Read-only file system`. **No host cache was dropped.**
- A per-file `posix_fadvise(POSIX_FADV_DONTNEED)` on one 4 MiB scratch file under
  the court bind mount succeeded. That is safe per-file clean-page eviction, not
  host-wide cache loss, and is recorded as such.
- The storage courts use QEMU `cache=none` for guest data, which opens the image
  with `O_DIRECT`; guest data therefore does not occupy the host page cache on
  that path. `cache=writeback` appears in labelled control/lying-fsync runs, but
  its host page cache was not dropped.
- **Not claimed:** host page-cache loss, global cache drop, device write-cache
  loss, physical media power loss.

## 6. Energy and CPU

`bench/receipts/energy-probe-20261103T225547Z.json` recorded the first attempt
inside the bench court:

- `perf` is **not installed** in the bench image; `perf stat -e
  cycles,instructions,task-clock` could not run. `kernel.perf_event_paranoid=2`
  is recorded.
- `/sys/class/powercap/intel-rapl`, `intel-rapl:0`, and `intel-rapl:0:0`
  exist, but no `energy_uj` file is readable; `readable_energy_files=0`.
- cgroup `cpu.stat`, `cpu.max`, and `memory.current` are readable.

`bench/receipts/perf-counters-20261104T015026Z.json` closes the `perf` part:
Debian `linux-perf 6.1.190-1` is installed inside the ephemeral bench court
(no host package is changed), and `perf stat` records cycles, instructions,
branches, branch-misses, cache references/misses, context switches, and page
faults for RAW and ZSTD encode/decode, three repetitions each; all 96 counter
rows completed. These are hardware counters, **not energy**.

RAPL remains blocked. Inside the bench court `/sys/class/powercap` exists but
no `energy_uj` file is visible: `/sys/devices/virtual/powercap` is masked or
empty in Docker. On the host the RAPL files are mode `0400` root-only and the
unprivileged host session cannot read `sudo` without a password. In the QEMU
guest `/sys/class/powercap` is present but `energy_uj` files = 0 and no `perf`
binary exists. The exact commands are in the receipt and its log. Therefore no
joule, watt, package-power, or energy-efficiency number is reported.

Therefore the supported metric remains CPU user and system time per process
from `wait4` rusage, plus wall time and max RSS. **CPU time and perf counters
are not energy.** The fio receipt records fio's guest `usr_cpu`/`sys_cpu`
percentages as reported by `fio`, which are CPU utilization, not energy.

Scoped CPU observations from the matched codec court: Rust encode CPU total is
2.71 ms (`raw`), 7.37 ms (`zstd`), 37.54 ms (`delta`), 2.57 ms
(`dictionary`); Go is 3.69, 11.68, 63.54, 4.32 ms respectively. Rust decode
CPU total is 2.18, 3.79, 1.51, 1.31 ms; Go is 3.35, 6.13, 2.17, 2.43 ms.
These are CPU time, not energy, and include the CLI process overhead.

## 7. Stability

Two independent runs of each new court were compared by per-cell relative
change (|a−b| / min(a,b)); no sample was removed.

| Court | Metric | Cells | Median | p95 | Max |
|---|---|---:|---:|---:|---:|
| External SQLite | query median (warm + cold) | 96 | 1.6% | 13.4% | 35.0% |
| Matched codec | encode/decode wall median | 28 | 2.9% | 33.5% | 40.0% |
| Cold fio | IOPS median | 120 | 2.5% | 49.5% | 147.5% |
| Cold fio | clat p50 median | 120 | 2.7% | 19.7% | 1,218.1% |
| Cold fio | clat p99 median | 120 | 10.0% | 538.0% | 916.1% |

The worst matched-codec change is Rust `raw` encode (3.876 → 2.768 ms, 40.0%)
and Rust `raw` decode (3.011 → 2.255 ms, 33.5%); the first run followed a fresh
codec build, the second reused the binary. The worst external change is
relational/10,000 `qsnapshot_resolve` cold (137.8 → 186.1 µs, 35.0%). The worst
cold-fio change is ext4 `fsync-write-4k` `numjobs=4` warm p50 (62.85 ms →
4.77 ms, 1,218%); p99 changed by up to 916% and is not stable in this rig.
Tail metrics at high concurrency are reported as unstable, not as a result.

## 8. Explicit gaps and what is not claimed

- No physical power-loss, host page-cache loss, torn-sector, or device
  write-cache-loss test. The VM power-cut court remains a QEMU proxy; its
  `cache=unsafe` mode is a lying-fsync device model, not physical media. The
  block-layer matrix in §5.5 deliberately loses acknowledged writes with
  dm-flakey and a snapshot overlay, but it is a software block-layer model, not
  physical media.
- Host `drop_caches` is blocked in the unprivileged court (`Read-only file
  system`, mode `200`, `CapEff=0`); only a per-file `fadvise(DONTNEED)` was
  attempted and succeeded. No host/device cache was lost.
- ZFS now has a **whole-disk** result (§5.4): raw `/dev/vdb` passed to `zpool`,
  ZFS-created GPT inside it, guest udevd present. The earlier file-backed ZFS
  result remains separately labelled. ZFS reads are ARC-served and are not
  cold-disk numbers. Btrfs ran on the same matrix.
- blkdebug write-error/flush-error injection cannot reach the write loop because
  it arms at device open and hits ext4's mount superblock; the exact guest log
  is recorded in §5.5. No result is substituted for those modes.
- No RAPL energy or external power meter. Perf hardware counters are reported
  separately and are not energy.
- No direct-I/O filesystem run and no same-inode concurrent direct I/O.
- No metadata-storm, open-unlinked, hardlink, symlink, or xattr throughput run
  in this comparative pass.
- No AI-corpus external SQLite/graph baseline; the external baseline uses the
  store corpus, not the F7 AI corpus.
- No 10^5–10^6-object scaling; the external baseline stops at 10,000.
- No dedicated graph database, Neo4j, or SPARQL comparison. The adjacency
  variant is SQLite with an adjacency table.
- No library-vs-library Go/Rust codec comparison; the matched court is
  CLI-process to CLI-process, with process overhead measured and not
  subtracted.
- No multi-host, NUMA, non-x86, or different-kernel measurement. All numbers
  are single-host, single-guest, quota-limited observations.
- No claim that any implementation is “faster”, “smallest”, “production
  ready”, or superior overall. The tables contain wins and losses for every
  contestant; the losses are part of the result.

## 9. Reproduction notes

- The external corpus is regenerated by
  `cargo build --release --example bench_external_manifest` plus the manifest
  invocation in `bench/scripts/run_external_bench.sh`; the manifest hash must
  match §1 for a byte-identical corpus.
- Override external sizes with `WEFT_EXTERNAL_SIZES`, warm iterations with
  `WEFT_EXTERNAL_WARM_ITERS`, cold iterations with `WEFT_EXTERNAL_COLD_ITERS`.
- Matched codec iterations: `WEFT_MATCHED_ITERATIONS` (large fixtures) and
  `WEFT_MATCHED_SMALL_ITERATIONS` (tensor/tokens). The harness is
  `bench/matched/main.go`; it builds both CLIs from the checked-out revisions.
- Cold fio targets: `WEFT_FS_COLD_TARGETS`; repetitions:
  `WEFT_FS_COLD_REPS`; per-target timeout: `WEFT_FS_COLD_TIMEOUT`.
  Raw fio JSON stays under `.docker-cache/vm-fs/<UTC>/extracted/`; the receipt
  records each file name and every command line. The whole-disk ZFS/Btrfs run is
  `scripts/docker-court.sh vm "WEFT_FS_COLD_TARGETS=ext4,xfs,btrfs,zfs-whole WEFT_FS_COLD_REPS=1 bash bench/scripts/vm_fs_court.sh"`;
  the vm image is built from `docker/vm.Dockerfile`, which installs OpenZFS via
  DKMS for the Debian guest kernel and btrfs-progs. The earlier file-backed ZFS
  run is `WEFT_FS_COLD_TARGETS=ext4,xfs,zfs`.
- Block-layer lying-disk matrix:
  `scripts/docker-court.sh vm "bash scripts/blkdebug_court.sh"`; override modes
  with `BLKDEBUG_MODES` and iterations with `BLKDEBUG_ITERATIONS`. The receipt
  records every mode's blkdebug table/config, dm-flakey table, kill delay, and
  the exact blocked-mount log. Host-cache probe:
  `scripts/docker-court.sh vm "bash scripts/hostcache_probe.sh"`.
- Perf/energy probe: `scripts/docker-court.sh bench "bash bench/scripts/run_perf_counters.sh"`.
  It installs `linux-perf`/`jq` inside the ephemeral bench court when absent and
  records the exact package versions; no host package is changed.
- The vm court image must be rebuilt if `docker/vm.Dockerfile` changes; the
  image id in the receipt identifies the exact image used.
