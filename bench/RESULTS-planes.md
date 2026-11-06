# R10 — blob and commit planes: court results

Two receipts, one per independent run, from the same tree:

| run | receipt | tests |
|---|---|---|
| 1 | `bench/receipts/planes-20261106T110154Z.json` | `planes` 14/14, MinIO court |
| 2 | `bench/receipts/planes-20261106T110216Z.json` | `planes` 14/14, MinIO court |

Both were produced by `bash scripts/s3_court.sh --planes`, which starts the
official MinIO release binary (`RELEASE.2025-04-22T22-12-26Z`, sha256
`53e2a2cb…bb817c`) in a minimal `debian:bookworm-slim` container under the same
hard limits as every other court (3 GiB memory == swap, 4 CPUs, 1024 pids,
bounded tmpfs, `no-new-privileges`, `cap-drop=ALL`), then runs
`bench/scripts/planes_court.sh` in the `weft/bench-court:1` image
(`sha256:4041d66e…f2041`). The local-only variant
`bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/planes_court.sh"`
was also run (`bench/receipts/planes-20261106T110054Z.json`); without an S3
endpoint only `local-local` runs and the three S3 layouts are recorded as
skipped with the reason, so the receipt is honest rather than empty.

## Workload

Deterministic, seeded, fixed clock (`fixed_timestamp_ns = 1700000000000000000`):

* 48 files × 128 KiB (6,291,456 visible bytes), chunk size 64 KiB,
  checkpoint interval 8;
* 96 range writes of 4 KiB at pseudo-random aligned offsets;
* full read of every file with a BLAKE3 digest;
* `verify --full`;
* close, reopen read-only, `verify --full` again.

Every layout must produce the same content root, the same commit generation,
the same verified byte count, and the same digest. That is the exactness test,
not a success flag.

## Comparison (run 1, medians are p50/p99 of 96 range writes)

| layout | create | 4 KiB write p50 / p99 (total) | full read | `verify --full` | reopen + recover | peak RSS |
|---|---|---|---|---|---|---|
| local / local (one backend) | 5.90 ms | 0.134 / 0.233 ms (14.06 ms) | 3.24 ms | 11.1 ms | 0.53 ms | 12,036 KiB |
| s3 / s3 (one backend) | 129.90 ms | 2.877 / 3.619 ms (286.44 ms) | 30.03 ms | 208.5 ms | 45.50 ms | 13,124 KiB |
| s3 blobs / local commits | 98.38 ms | 2.254 / 2.914 ms (220.68 ms) | 31.31 ms | 143.1 ms | 44.26 ms | 13,136 KiB |
| local blobs / s3 commits | 55.68 ms | 1.149 / 1.998 ms (117.49 ms) | 3.37 ms | 91.5 ms | 2.98 ms | 13,092 KiB |

Per-plane request counts and bytes (run 1):

| layout | plane | reads | bytes read | writes | bytes written | publications |
|---|---|---|---|---|---|---|
| local / local | merged | 1,347 | 31.0 MiB | 794 | 13.0 MiB | 649 `rename(2)` |
| s3 / s3 | merged | 1,345 | 31.0 MiB | 797 | 13.6 MiB | 797 S3 PUT |
| s3 blobs / local commits | blobs (S3) | 903 | 30.7 MiB | 502 | 12.4 MiB | 502 S3 PUT |
| | commits (local) | 732 | 2.5 MiB | 294 | 1.2 MiB | 294 `rename(2)` |
| local blobs / s3 commits | blobs (local) | 903 | 30.7 MiB | 500 | 12.4 MiB | 500 `rename(2)` |
| | commits (S3) | 585 | 1.4 MiB | 297 | 1.2 MiB | 297 S3 PUT |

Reads are dominated by `verify --full` re-reading every manifest and chunk;
writes are dominated by the 48 creates. Splitting moves exactly the traffic the
routing table says it should: ~30.7 MiB of blob bytes and ~1.2–2.5 MiB of
control traffic.

## Exactness

Run 1, all four layouts:

```
generation            145
snapshot_root         7ebb1bdb425b86b04dfe7aa7018136b52648da0f00a148f9356eabdddb80a470
verified_bytes        6,291,456
content_digest        74d54de57fd438a492b7966810ca4a631cb7eb9cc23b7e20da8f4f54d1bf5b59
reopen match          true (all four)
verify --full         problems = 0 (all four)
```

The same root, generation, and digest across a single local backend, a single
S3 backend, and both splits is the byte-exactness proof: the split changes
where the bytes live, not which bytes exist. `tests/planes.rs` additionally
compares the whole replayed state record-by-record (`objects`, `entries`,
`relations`, `snapshots`) and every file's bytes between the single-backend and
split stores, in both directions.

## Crash proxy

`tests/planes.rs` kills the writer at the three publication phases
(`after_objects`, `after_commit_file`, `after_superblock_write`) for a local
split and for an S3-blobs/local-commits split, via the documented
`WEFT_CRASH_PHASE` hook. In every case the reopened store holds the old whole
state (phase 1 and 2) or the new whole state (phase 3) — never a partial one —
and an unpublished newer commit file is the expected early-crash state, exactly
as in the pre-existing single-backend proxies. Both tests pass in every court
run (`split_crash_proxy_yields_old_or_new`,
`s3_split_crash_proxy_yields_old_or_new`).

The head publication is a real compare-and-swap on the split
(`compare_exchange_head`), and `split_publication_uses_one_head_compare_and_swap`
proves a second writer is refused with `Busy`. A stale token is rejected by the
commit probe on every backend, including S3.

## Stability (run 1 vs run 2)

| layout | create | 96 writes | full read | peak RSS |
|---|---|---|---|---|
| local / local | 1.0% | 1.0% | 1.7% | 1.4% |
| s3 / s3 | 1.2% | 0.3% | 0.1% | 2.5% |
| s3 blobs / local commits | 0.6% | 0.3% | 18.0% | 0.2% |
| local blobs / s3 commits | 2.9% | 1.6% | 2.1% | 2.9% |

Absolute deltas are in tenths of a millisecond except the 18.0%: the split's
read phase moved from 31.31 to 36.96 ms because every chunk read is a network
round trip on a shared CPU. Content roots, generations, and digests are
identical between the two runs.

## Losses and limits (all of them)

* **Everything remote is slower.** `s3/s3` create is 22× the local number and a
  4 KiB write is 21.5×; the split does not change that for the bytes that are
  remote. `local blobs / s3 commits` creates 2.3× slower than pure local
  because every commit record and head publication is a network round trip.
* **The split adds read round trips.** `verify --full` on `local blobs / s3
  commits` is 8.2× local (91.5 vs 11.1 ms) even though the 30.7 MiB of chunk
  and manifest reads are local, because control-state reads are remote. The
  request counts show 585 commit-plane reads against 903 blob-plane reads for
  that layout.
* **Peak RSS is ~1 MiB higher for every remote layout**, consistent with the S3
  client's buffers, not with plane routing.
* **The commit plane is still single-writer.** Two writers are refused with
  `Busy`; there is no distributed coordination and no fleet claim.
* **A split store is not interchangeable with a single-backend store.** The
  same logical bytes are written, but opening requires the same split
  configuration. The `info --json` output and the diagnostics name the layout
  so a mismatched open is a visible mistake rather than a silent one.
* **Local/local in the court uses two directories** (`--blob-root`) to make the
  separation real; a local/local split sharing one directory is accepted but is
  pointless.
* **Process-fault crash proxy only.** No power loss, no host cache loss, no
  media error injection, no RAPL energy.
* **One host, warm caches, synthetic fixture.** MinIO is a single node on
  localhost with a tmpfs data directory; the local planes are tmpfs or the
  container overlay. Absolute times are court-bound and are not a general
  performance claim.

## Residual work

* Go (WeftFS-Go) does not implement the plane split; it keeps the single
  `store` backend. No cross-language split handoff is claimed.
* `--blob-backend`/`--commit-backend` are CLI flags; the library API is
  `BackendKind::split`.
* An external metadata engine (pack index, distributed KV, consensus log) is
  specified in [`docs/PLANES.md`](../docs/PLANES.md) §5 but not implemented.
* FUSE mounts use whichever `BackendKind` the CLI selects; the split court is
  library/CLI-level, not mounted.
