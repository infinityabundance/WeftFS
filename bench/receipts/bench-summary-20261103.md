## Codec external baselines

Receipt: `bench-codec-20261103T175847Z.json` (schema `weft-codec-bench-ext-v1`, iterations 7, image `sha256:165bafbe79ff54a41fae8db8b524a3183956e3e43e3211d01823680bba1403cd`)

### External CLIs (process wall time, median of raw samples)

| Corpus | Tool | Level | Compressed bytes | Encode ms | Decode ms |
|---|---|---:|---:|---:|---:|
| empty | zstd | 3 | 13 | 0.301 | 0.283 |
| empty | zstd | 9 | 13 | 0.272 | 0.264 |
| empty | zstd | 19 | 13 | 0.277 | 0.283 |
| empty | gzip | 9 | 32 | 0.137 | 0.135 |
| empty | xz | 9 | 32 | 0.187 | 0.172 |
| empty | brotli | 9 | 2 | 0.179 | 0.160 |
| one_byte | zstd | 3 | 14 | 0.279 | 0.267 |
| one_byte | zstd | 9 | 14 | 0.290 | 0.281 |
| one_byte | zstd | 19 | 14 | 0.281 | 0.273 |
| one_byte | gzip | 9 | 36 | 0.146 | 0.154 |
| one_byte | xz | 9 | 60 | 0.196 | 0.182 |
| one_byte | brotli | 9 | 6 | 0.169 | 0.159 |
| random_4m | zstd | 3 | 4,194,414 | 2.028 | 1.093 |
| random_4m | zstd | 9 | 4,194,413 | 9.550 | 1.204 |
| random_4m | zstd | 19 | 4,194,413 | 181.706 | 1.725 |
| random_4m | gzip | 9 | 4,194,978 | 66.329 | 9.608 |
| random_4m | xz | 9 | 4,194,576 | 481.748 | 2.285 |
| random_4m | brotli | 9 | 4,194,313 | 16.034 | 1.647 |
| repetitive_4m | zstd | 3 | 449 | 1.380 | 1.506 |
| repetitive_4m | zstd | 9 | 448 | 2.374 | 1.606 |
| repetitive_4m | zstd | 19 | 412 | 3.872 | 1.539 |
| repetitive_4m | gzip | 9 | 12,296 | 13.000 | 8.196 |
| repetitive_4m | xz | 9 | 792 | 26.140 | 3.608 |
| repetitive_4m | brotli | 9 | 104 | 3.570 | 2.792 |
| near_dup_base_1m | zstd | 3 | 1,048,613 | 1.244 | 0.663 |
| near_dup_base_1m | zstd | 9 | 1,048,613 | 4.237 | 0.683 |
| near_dup_base_1m | zstd | 19 | 1,048,613 | 29.306 | 0.705 |
| near_dup_base_1m | gzip | 9 | 1,048,777 | 16.649 | 2.531 |
| near_dup_base_1m | xz | 9 | 1,048,688 | 106.518 | 0.858 |
| near_dup_base_1m | brotli | 9 | 1,048,581 | 4.604 | 0.713 |
| near_dup_1pct | zstd | 3 | 1,048,613 | 1.143 | 0.716 |
| near_dup_1pct | zstd | 9 | 1,048,613 | 3.986 | 0.673 |
| near_dup_1pct | zstd | 19 | 1,048,613 | 28.849 | 0.733 |
| near_dup_1pct | gzip | 9 | 1,048,774 | 16.710 | 2.605 |
| near_dup_1pct | xz | 9 | 1,048,688 | 104.567 | 0.906 |
| near_dup_1pct | brotli | 9 | 1,048,581 | 4.179 | 0.659 |
| source_text_4m | zstd | 3 | 763,396 | 8.362 | 2.647 |
| source_text_4m | zstd | 9 | 665,991 | 30.609 | 2.601 |
| source_text_4m | zstd | 19 | 573,917 | 762.045 | 2.788 |
| source_text_4m | gzip | 9 | 662,053 | 152.996 | 11.466 |
| source_text_4m | xz | 9 | 572,700 | 508.618 | 22.802 |
| source_text_4m | brotli | 9 | 649,512 | 85.796 | 5.628 |

### WeftCodec (marginal frame and whole-object bytes)

| Corpus | Mode | Level | Chosen | Frame bytes | Dependency bytes | Whole bytes | Encode ms | Decode ms |
|---|---|---:|---|---:|---:|---:|---:|---:|
| empty | delta | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| empty | dictionary | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| empty | raw | 0 | raw | 100 | 0 | 100 | 0.000 | 0.000 |
| empty | zstd | 3 | zstd | 109 | 0 | 109 | 0.000 | 0.000 |
| empty | zstd | 9 | zstd | 109 | 0 | 109 | 0.000 | 0.000 |
| empty | zstd | 19 | zstd | 109 | 0 | 109 | 0.000 | 0.000 |
| empty | auto | 3 | raw | 100 | 0 | 100 | 0.001 | 0.000 |
| one_byte | delta | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| one_byte | dictionary | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| one_byte | raw | 0 | raw | 101 | 0 | 101 | 0.000 | 0.000 |
| one_byte | zstd | 3 | zstd | 110 | 0 | 110 | 0.000 | 0.000 |
| one_byte | zstd | 9 | zstd | 110 | 0 | 110 | 0.000 | 0.000 |
| one_byte | zstd | 19 | zstd | 110 | 0 | 110 | 0.000 | 0.000 |
| one_byte | auto | 3 | raw | 101 | 0 | 101 | 0.001 | 0.000 |
| random_4m | delta | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| random_4m | dictionary | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| random_4m | raw | 0 | raw | 4,194,404 | 0 | 4,194,404 | 0.653 | 0.498 |
| random_4m | zstd | 3 | zstd | 4,194,510 | 0 | 4,194,510 | 0.938 | 0.498 |
| random_4m | zstd | 9 | zstd | 4,194,509 | 0 | 4,194,509 | 2.128 | 0.499 |
| random_4m | zstd | 19 | zstd | 4,194,509 | 0 | 4,194,509 | 95.505 | 0.499 |
| random_4m | auto | 3 | raw | 4,194,404 | 0 | 4,194,404 | 1.593 | 0.498 |
| repetitive_4m | delta | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| repetitive_4m | dictionary | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| repetitive_4m | raw | 0 | raw | 4,194,404 | 0 | 4,194,404 | 0.681 | 0.507 |
| repetitive_4m | zstd | 3 | zstd | 545 | 0 | 545 | 0.680 | 0.607 |
| repetitive_4m | zstd | 9 | zstd | 5,475 | 0 | 5,475 | 2.319 | 0.617 |
| repetitive_4m | zstd | 19 | zstd | 508 | 0 | 508 | 4.051 | 0.607 |
| repetitive_4m | auto | 3 | zstd | 545 | 0 | 545 | 1.361 | 0.609 |
| near_dup_base_1m | raw | 0 | raw | 1,048,676 | 0 | 1,048,676 | 0.168 | 0.128 |
| near_dup_base_1m | zstd | 3 | zstd | 1,048,709 | 0 | 1,048,709 | 0.240 | 0.130 |
| near_dup_base_1m | zstd | 9 | zstd | 1,048,709 | 0 | 1,048,709 | 0.678 | 0.130 |
| near_dup_base_1m | zstd | 19 | zstd | 1,048,709 | 0 | 1,048,709 | 23.844 | 0.130 |
| near_dup_base_1m | delta | 0 | delta | 153 | 1,048,576 | 1,048,729 | 34.639 | 0.296 |
| near_dup_base_1m | dictionary | 3 | dictionary | 1,194 | 1,048,576 | 1,049,770 | 1.478 | 0.312 |
| near_dup_base_1m | auto | 3 | reference | 148 | 1,048,576 | 1,048,724 | 37.869 | 0.282 |
| near_dup_1pct | raw | 0 | raw | 1,048,676 | 0 | 1,048,676 | 0.172 | 0.130 |
| near_dup_1pct | zstd | 3 | zstd | 1,048,709 | 0 | 1,048,709 | 0.255 | 0.129 |
| near_dup_1pct | zstd | 9 | zstd | 1,048,709 | 0 | 1,048,709 | 0.696 | 0.129 |
| near_dup_1pct | zstd | 19 | zstd | 1,048,709 | 0 | 1,048,709 | 23.932 | 0.132 |
| near_dup_1pct | delta | 0 | delta | 79,984 | 1,048,576 | 1,128,560 | 35.862 | 0.415 |
| near_dup_1pct | dictionary | 3 | dictionary | 24,733 | 1,048,576 | 1,073,309 | 1.768 | 0.426 |
| near_dup_1pct | auto | 3 | dictionary | 24,733 | 1,048,576 | 1,073,309 | 39.283 | 0.422 |
| source_text_4m | delta | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| source_text_4m | dictionary | n/a | n/a | n/a | n/a | n/a | n/a | n/a |
| source_text_4m | raw | 0 | raw | 4,194,404 | 0 | 4,194,404 | 0.671 | 0.509 |
| source_text_4m | zstd | 3 | zstd | 762,372 | 0 | 762,372 | 6.343 | 1.715 |
| source_text_4m | zstd | 9 | zstd | 667,412 | 0 | 667,412 | 26.472 | 1.679 |
| source_text_4m | zstd | 19 | zstd | 577,018 | 0 | 577,018 | 661.098 | 1.423 |
| source_text_4m | auto | 3 | zstd | 762,372 | 0 | 762,372 | 6.964 | 1.692 |

## Store and context benchmark

Receipt: `bench-store-20261103T175920Z.json` (schema `weft-store-bench-v1`, wall 508.3s, peak RSS 169072 KiB)

| Objects | Relations | Ingest s | Objects/s | Open ms | Query 0 ms | Query 1 ms | Query n ms | One-hop ms | Verify(full) ms | GC dry ms | GC apply ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 100 | 207 | 1.4 | 71.3 | 6.516 | 0.023 | 0.028 | 0.021 | 0.026 | 17.866 | 25.513 | 25.701 |
| 1,000 | 2,067 | 24.3 | 41.2 | 317.148 | 2.399 | 2.344 | 0.183 | 2.309 | 682.942 | 600.871 | 608.855 |
| 10,000 | 20,667 | 173.4 | 57.7 | 32587.135 | 424.071 | 414.336 | 1.931 | 411.802 | 70304.925 | 51461.419 | 51168.123 |

| Objects | Snapshot ms | Churn add ms | Churn delete ms | Disk before GC bytes | Disk after GC bytes | GC deleted | GC deleted bytes | Verify ok |
|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 100 | 4.526 | 2.299 | 2.282 | 286,785 | 280,988 | 40 | 5,797 | True |
| 1,000 | 4.738 | 2.626 | 2.613 | 2,445,735 | 2,439,938 | 40 | 5,797 | True |
| 10,000 | 6.950 | 5.733 | 5.732 | 60,515,532 | 60,509,735 | 40 | 5,797 | True |

Query cardinalities were asserted before sampling: 100 objects -> {'even': 40, 'n': 100, 'one': 1, 'one_hop': 1, 'zero': 0}, 1,000 objects -> {'even': 400, 'n': 1000, 'one': 1, 'one_hop': 1, 'zero': 0}, 10,000 objects -> {'even': 4000, 'n': 10000, 'one': 1, 'one_hop': 1, 'zero': 0}.

## Filesystem benchmark (fio)

Receipt: `bench-fs-20261103T180751Z.json` (schema `weft-fs-bench-v1`, fio `fio-3.33`, go_status `ok`)

| Target | Workload | Throughput KiB/s | IOPS | p50 ms | p95 ms | p99 ms |
|---|---|---:|---:|---:|---:|---:|
| go | fsync-write-4k | 503.0 | 125.86 | 7.766 | 9.896 | 11.207 |
| go | rand-read-4k | 42,010.0 | 10,502.56 | 0.073 | 0.191 | 0.241 |
| go | rand-write-4k | 221.0 | 55.45 | 17.695 | 20.316 | 21.627 |
| go | seq-read-1m | 630,153.0 | 615.38 | 1.466 | 1.794 | 1.794 |
| go | seq-write-1m | 8,258.0 | 8.06 | 119.013 | 166.724 | 166.724 |
| rust | fsync-write-4k | 508.0 | 127.11 | 7.635 | 9.896 | 11.076 |
| rust | rand-read-4k | 74,472.0 | 18,618.18 | 0.052 | 0.058 | 0.066 |
| rust | rand-write-4k | 292.0 | 73.07 | 13.435 | 15.794 | 16.908 |
| rust | seq-read-1m | 2,048,000.0 | 2,000.00 | 0.346 | 0.610 | 0.610 |
| rust | seq-write-1m | 20,791.0 | 20.30 | 47.972 | 52.167 | 52.167 |
| tmpfs | fsync-write-4k | 1,024,000.0 | 256,000.00 | 0.000 | 0.000 | 0.001 |
| tmpfs | rand-read-4k | 4,096,000.0 | 1,024,000.00 | 0.000 | 0.001 | 0.001 |
| tmpfs | rand-write-4k | 4,096,000.0 | 1,024,000.00 | 0.000 | 0.000 | 0.001 |
| tmpfs | seq-read-1m | 8,192,000.0 | 8,000.00 | 0.078 | 0.230 | 0.230 |
| tmpfs | seq-write-1m | 8,192,000.0 | 8,000.00 | 0.071 | 0.100 | 0.100 |

