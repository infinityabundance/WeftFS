# Go read-handle pool (subphase R14)

**Status:** implemented; correctness proven in the fuse court, and a host-local
descriptor-cost observation recorded. Not a workload or restore-throughput
claim.

## What it does

The Go store's local and object blob planes opened, read, and closed one file
descriptor per chunk read. `store/read_pool.go` keeps at most
`Options.ReadPoolIdle` idle handles (default 64; a negative value disables
pooling), closes the oldest first, and never holds the pool mutex across I/O.
Reads use `ReadAt`, so concurrent readers of the same blob cannot disturb a
shared offset.

The hash is computed on the returned bytes after the handle is released, so
verify-then-trust is unchanged. Remote planes (S3, GCS, Azure) have no
descriptors and are never pooled.

## The fail-closed property that had to be added

The first version reused an idle descriptor without re-checking the path, and
the full suite found two real regressions:

* `TestOverlayRangeReadsOnlyIntersectingChunks` — "range [24,32) succeeded with
  its chunk missing"
* `TestTensorMissingDependencyFailsClosedAndGC` — "full read with missing
  dependency = <nil>, want Corrupt"

Both delete a blob behind the store's back; an open descriptor still returned
the old bytes, so a read that must fail closed succeeded. An idle handle is now
validated with one `stat` before reuse: a vanished path closes the handle and
returns `NotFound`, and a changed size or modification time reopens the file
instead of reusing it. Both tests pass again, and
`TestReadPoolVanishedBlobFailsClosed` asserts the behaviour directly and counts
the refresh.

## Tests

`store/read_pool_test.go`, all passing under `-race`:

| Test | Property |
|---|---|
| `TestReadPoolReusesHandles` | fifty reads of one blob open the file once |
| `TestReadPoolBoundIsHard` | the idle set never exceeds the bound; evictions counted |
| `TestReadPoolMutationInvalidates` | delete + republish is never served from a pre-mutation handle |
| `TestReadPoolHandlesDoNotMix` | interleaved reads of two blobs return their own bytes |
| `TestReadPoolConcurrentReaders` | eight readers × 50 reads, no mixing, no race |
| `TestReadPoolRangeReads` | clamped ranges, `InvalidArgument` past EOF, `NotFound` for a missing blob |
| `TestReadPoolVanishedBlobFailsClosed` | a blob deleted behind the plane is not served |
| `TestReadPoolDisabledByOption` | a negative bound is the plain path and reports no pool |
| `TestReadPoolReportsThroughTheStore` | a default store pools; reuse observed through `ReadPoolStats` |

Full suite: `go test -race ./...` in the fuse court at WeftFS-Go `de9a4b9`,
5 packages `ok`, 0 failed, `store` 210.618 s.

## Measured descriptor cost

Command (go court, tmpfs, warm page cache):

```
go test -run '^$' -bench 'ReadBlob' -benchtime 300x -count 3 ./store
```

Fixture: one 73,728-byte file in a freshly created store root, read 300 times
per run.

| Run | Unpooled (ns/op) | Pooled (ns/op) |
|---|---|---|
| 1 | 6,939 | 6,321 |
| 2 | 7,240 | 5,156 |
| 3 | 8,420 | 5,334 |

Medians: 7,240 vs 5,334 ns/op. The spread across three runs is wide, so no
tighter figure is asserted than "the pooled path is consistently lower on this
fixture". The MB/s column the benchmark prints is an artifact of the page cache
and is not quoted.

**This is not a restore-throughput claim.** A real restore also hashes, decodes,
and copies, and the pool only removes descriptor work. No end-to-end figure, no
comparison with the Rust client, and no fleet claim is made.

## Not claimed

* No measurement against a real checkpoint restore, and no p50/p99 figures.
* No comparison with the Rust read pool on the same workload.
* Pooling is not applied to the packed-plane bypass rules beyond the
  content-addressed paths: a packed member read still goes through the pack
  range read.
* Blocked handles stay open for the life of the store; the bound is a count,
  not a byte budget.
