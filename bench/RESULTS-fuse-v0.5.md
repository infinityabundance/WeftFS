# Mounted filesystem end-to-end results — v0.5 (2026-11-05)

Receipt: `bench/receipts/fuse-bench-20261105T022024Z.json`
sha256 `4445b1554e4779cd23e70ed98c8f841eae31efeb5d766eaed01029807ae2f835`
Run log: `bench/receipts/fuse-bench-20261105T022024Z.log`

Command (the only supported way to run it):

```sh
scripts/docker-court.sh bench "bash bench/scripts/run_fuse_bench.sh"
```

This is a **single-host, synthetic, buffered-I/O** measurement. Page caches were
not dropped (the court cannot write `/proc/sys/vm/drop_caches`). The tmpfs target
is a reference only: no durability and no FUSE layer. The ext4 target is the host
ext4 filesystem behind the `/work` bind mount (`/dev/nvme1n1p1[/WeftFS]`), not a
dedicated block device handed to the container. No `winner` field is emitted and
no hardware-independent claim follows.

> Post-run note: the first assembly of this receipt had an empty cell 2 because
> the assembler split fio filenames on the wrong hyphen. The parser was fixed and
> the receipt was re-assembled from the run's unmodified raw directory. No
> measurement was rerun or edited. The log records both hashes.

## Rig

| Item | Value |
|---|---|
| Court image | `weft/bench-court:1` |
| Image id | `sha256:e80c438516a509c251d6f3ca4848363a86d12a92287a1d9284117acd9cc0d85a` |
| Kernel | `7.3.1-1-cachyos` |
| Limits | 3 GiB memory == swap, 4 CPUs, 1,024 pids, `/tmp` 1 GiB, no-new-privileges |
| WeftFS (Rust) | `8c8e5a3b8e5d872103f172e192621b8eb945c415` |
| WeftFS-Go | `1c990705209cde372cd072440a3b171e56e4cdcf` |
| WeftCodec | `90d9c13e8a4702c131aaeac4e6d18fc439392ae3` |
| WeftCodec-Go | `cf62bb151275401f0a6556e33c9f9fc993cc3c31` |
| Wall time | ~31 minutes (10,000-object store build alone: 109.6 s) |

Three repetitions per cell, with the target order rotated between repetitions.
Medians are across the three repetitions. fio percentiles are within a
repetition and are reported as the median of the three within-run percentiles.

## Cell 1 — metadata (2,000 files, 200 dirs)

`create` is open-with-`O_CREAT`+close; `stat` is `lstat`; `readdir` is one
`Readdirnames(-1)` per directory (10 entries each); `unlink` is `unlink(2)`.
Ops/s is the median of three repetitions. Latencies are per operation.

| Target | create ops/s | stat ops/s | readdir ops/s | unlink ops/s | create p50/p95/p99 | stat p95 | readdir p95 | unlink p95 |
|---|---:|---:|---:|---:|---|---:|---:|---:|
| WeftFS Rust | 157.4 | 22,165.6 | 11,738.0 | 159.0 | 6.04 / 8.29 / 9.43 ms | 0.051 ms | 0.096 ms | 8.08 ms |
| WeftFS Go | 300.6 | 5,722.2 | 2,607.2 | 314.2 | 3.16 / 4.19 / 7.73 ms | 0.185 ms | 0.404 ms | 4.08 ms |
| ext4 (court path) | 291,722.7 | 2,456,214.9 | 345,967.5 | 606,341.9 | 0.003 / 0.005 / 0.008 ms | 0.001 ms | 0.003 ms | 0.002 ms |
| tmpfs (reference) | 504,451.2 | 2,387,552.3 | 466,234.2 | 1,415,485.3 | 0.002 / 0.004 / 0.006 ms | 0.001 ms | 0.002 ms | 0.001 ms |

Per-repetition create/unlink ops/s:

| Target | rep 1 | rep 2 | rep 3 |
|---|---|---|---|
| Rust create | 157.4 | **84.7** | 158.3 |
| Rust unlink | 158.9 | 159.6 | 159.0 |
| Go create | 295.9 | 303.4 | 300.6 |
| Go unlink | 312.0 | 314.2 | 315.6 |
| ext4 create | 291,722.7 | 317,344.9 | 284,725.1 |
| tmpfs create | 498,311.8 | 504,451.2 | 524,720.5 |

The only large outlier is Rust's second create repetition (84.7 ops/s versus
157–158); the median is reported and the outlier is retained.

## Cell 2 — fio, numjobs=1, iodepth=1, psync, buffered (`direct=0`)

The file layout is 256 MiB (`--size=256M`). Sampling is capped for every target
so the court fits its time budget:

* `4k randwrite`: 512 KiB of I/O = 128 I/Os inside the 256 MiB file.
* `1M seqwrite`: 64 MiB of I/O = 64 I/Os inside the 256 MiB file.

The cap is necessary, not cosmetic: the Go write path costs O(file size) per
write (see losses), so a full 256 MiB sequential write takes ~14 minutes per Go
repetition. The same cap is applied to all targets for comparability. IOPS and
percentiles measure the real FUSE write path; total bytes are bounded and stated.

### 4 KiB random write

| Target | IOPS (reps) | MB/s | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| WeftFS Rust | 9.4 (9.2, 9.4, 9.4) | 0.04 | 106.43 ms | 109.58 ms | 110.63 ms |
| WeftFS Go | 2.5 (2.5, 2.5, 2.5) | 0.01 | 392.17 ms | 404.75 ms | 425.72 ms |
| ext4 (court path) | 128,000.0 (128,000 ×3) | 500.00 | 0.001 ms | 0.002 ms | 0.006 ms |
| tmpfs (reference) | 128,000.0 (128,000 ×3) | 500.00 | 0.001 ms | 0.001 ms | 0.001 ms |

### 1 MiB sequential write

| Target | IOPS (reps) | MB/s | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| WeftFS Rust | 6.9 (6.8, 6.9, 6.9) | 6.92 | 143.66 ms | 147.85 ms | 149.95 ms |
| WeftFS Go | 0.3 (0.3, 0.3, 0.3) | 0.31 | 3,170.89 ms | 3,238.00 ms | 3,305.11 ms |
| ext4 (court path) | 9,142.9 (9,142.9, 9,142.9, 10,666.7) | 9,142.86 | 0.094 ms | 0.116 ms | 0.144 ms |
| tmpfs (reference) | 9,142.9 (9,142.9, 9,142.9, 10,666.7) | 9,142.86 | 0.091 ms | 0.114 ms | 0.144 ms |

\* Two repetitions are 9,142.9 and the third is 10,666.7; the median of the
three is 9,142.9.

The ext4 and tmpfs numbers are page-cache submission throughput with an
end-of-job flush. They are **not** device write rates and the IOPS figures are
not comparable to a durable write rate. The WeftFS figures are its real FUSE
write path.

## Cell 3 — 500 random 4 KiB overwrites into a 128 MiB file (WeftFS only)

The file is created with `ftruncate` (sparse) before timing; each operation is
one `pwrite(4 KiB)` at a deterministic random offset followed by `fdatasync`.
`total` is the whole operation; `sync` is only the `fdatasync` portion.

| Implementation | ops/s | total p50 | total p95 | total p99 | fsync p50 | fsync p95 | fsync p99 |
|---|---:|---:|---:|---:|---:|---:|---:|
| WeftFS Rust | 9.22 | 108.22 ms | 111.57 ms | 113.30 ms | 0.013 ms | 0.019 ms | 0.026 ms |
| WeftFS Go | 2.54 | 392.30 ms | 405.77 ms | 427.40 ms | 0.022 ms | 0.030 ms | 0.036 ms |

Per-repetition ops/s: Rust 8.45, 9.22, 9.22; Go 2.49, 2.54, 2.56.

The durability flush is cheap (13–36 µs). Essentially all of the 108 ms / 392 ms
is the write path itself: the store rewrites the touched extent and recomputes
the v1 whole-file `ContentId`, whose cost grows with the logical file size.

## Cell 4 — cold mount of a 10,000-object store + Context Root `ls`

Store built with one `Store::group` publish (109,618 ms), then one snapshot and
one saved Context Root (`select attrs.bench == "fuse-v05"`). Each repetition
unmounts completely, then measures from daemon spawn to “mount visible **and** a
root `getattr` succeeds”, followed immediately by the first `readdir` of the
Context Root (the `ls` core). The listing has 10,001 entries: the 10,000 selected
objects plus the virtual `.weft` entry.

| Rep | mount-to-ready | first readdir | entries |
|---|---:|---:|---:|
| 1 | 472.377 ms | 446.3 ms | 10,001 |
| 2 | 472.424 ms | 455.0 ms | 10,001 |
| 3 | 478.604 ms | 461.6 ms | 10,001 |
| **median** | **472.424 ms** | **455.0 ms** | 10,001 |
| **max** | 478.604 ms | 461.6 ms | 10,001 |

Mount is stable within 6.2 ms across repetitions. The 455 ms readdir is ~45.5 µs
per entry. The FUSE caches are empty after each unmount, but the backing store
files remain warm in the host page cache; this is not a cold-disc measurement and
there is no ext4/tmpfs equivalent for a Context Root view.

## Largest losses

Ratios use the medians above. ext4/tmpfs are page-cache-dominated for writes.

1. **Metadata creates are 970–3,205× slower.** Rust 157.4 ops/s, Go 300.6 vs
   ext4 291,722.7 and tmpfs 504,451.2. Each create is a durable store commit.
2. **Unlinks are 1,930–8,902× slower.** Rust 159.0, Go 314.2 vs ext4 606,341.9
   and tmpfs 1,415,485.3.
3. **`stat` is 111–429× slower.** Rust 22,165.6, Go 5,722.2 vs ext4 2,456,214.9
   and tmpfs 2,387,552.3.
4. **`readdir` is 29–179× slower.** Rust 11,738.0, Go 2,607.2 vs ext4 345,967.5
   and tmpfs 466,234.2.
5. **4 KiB random writes are 13,617× (Rust) and 51,200× (Go) lower than ext4’s
   buffered submission rate**; p99 is 18,438× / 70,954× worse (110.6 ms / 425.7 ms
   vs 0.006 ms). The ext4 side is page cache, not a device write, but the WeftFS
   side is still 106 ms / 392 ms per 4 KiB write.
6. **1 MiB sequential writes are 1,325× (Rust) and 30,476× (Go) lower than
   ext4**; p99 is 1,041× / 22,952× worse. Go’s 3.17 s per 1 MiB write is the
   single worst observed number in this pass.
7. **The Go write path is O(file size) per write.** On the 256 MiB layout, a
   1 MiB write costs 3.17 s and a 4 KiB write costs 392 ms. The cap in cell 2
   exists because a full 256 MiB sequential write would take ~14 minutes per
   repetition. Rust is 3.6–23× faster on the same cells but has the same
   qualitative shape (a whole-file digest per write).
8. **Small overwrites are 9.22 ops/s (Rust) and 2.54 ops/s (Go)**, i.e. 108 ms
   and 392 ms per 4 KiB overwrite, with only ~5% p99/p50 spread. The variance is
   low; the cost is uniformly high. `fdatasync` contributes 13–36 µs, so this is
   not a durability-flush cost.
9. **First `ls` of a 10,000-object Context Root costs 455 ms** (~45.5 µs per
   entry). Mount-to-ready is 472 ms. There is no baseline for this view.

## Where WeftFS wins or ties

* Go creates and unlinks faster than Rust (1.91× and 1.98×). This is an
  implementation difference, not a language claim.
* Rust is faster than Go for `stat` (3.87×), `readdir` (4.50×), 4 KiB random
  writes (3.76×), 1 MiB sequential writes (23×), and small overwrites (3.63×).
* Mount-to-ready is sub-second (472 ms) and stable (±6.2 ms) for a 10,000-object
  Context Root on an already-built store.
* fdatasync latency is tens of microseconds; the write path, not the flush,
  dominates the overwrite cost.

## What this does not show

* No direct I/O; the court’s kernel/fuser combination can return EIO on
  same-inode direct I/O (`docs/fuse-status.md`), so `direct=0` is used.
* No cache-drop; `/proc/sys/vm/drop_caches` is not writable in the court.
* No physical power-loss durability; that is a separate court.
* No ext4/XFS/Btrfs/ZFS block-device baseline; ext4 here is the host bind-mount
  path and tmpfs is a non-equivalent reference.
* The fio samples are capped (128 random I/Os; 64 sequential I/Os) while keeping
  a 256 MiB file layout. Percentiles over 64–128 I/Os are coarse.
* Three repetitions are enough for scoped medians, not for confidence intervals.
* Cell 3 uses a sparse file, so it measures writes into a logically sized file
  with holes, not overwrites of pre-existing data.
* Cell 4’s backing store files are warm in the host page cache; “cold” refers to
  the mount/FUSE state, not the storage media.

## Reproduce

```sh
scripts/docker-court.sh bench "bash bench/scripts/run_fuse_bench.sh"
```

The script is self-contained: it builds the Rust and Go binaries, generates its
metadata/overwrite driver and receipt assembler into `.docker-cache`, mounts both
implementations, and writes `bench/receipts/fuse-bench-<UTC>.json` plus a log.
Product code is not modified.
