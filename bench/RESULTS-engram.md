# Mode 20 `ENGRAM_SHARD` — measured results

Receipt: `bench/receipts/engram-20261109T232921Z.json` (`weft.engram.v1`),
raw log beside it. Revisions in the receipt: WeftFS `5d63217`, WeftFS-Go
`281e268`, WeftCodec `37bb199`, WeftCodec-Go `9f49b0b`. Court:
`scripts/docker-court.sh bench`, image `weft/bench-court:1`, the usual hard
limits (`3g` memory, `3g` swap, 1024 pids, 4 CPUs, bounded `/tmp`), one host.

## Fixture

- 131,072 rows × 1,024 dim, bf16, 268,435,456 logical bytes (256 MiB).
- 64 equal row groups of 2,048 rows (4 MiB logical each).
- Generator (synthetic, deterministic): a row is active when `row % 64 == 0`
  (2,048 rows, 1.5625 %); active rows are filled with xorshift64* bytes and
  every inactive row repeats one fixed template row. This models a sparse
  n-gram / embedding table, not a serving trace.
- Table SHA-256 `aebff6f09c7f6ce9d69ab2cf4c9283af6e08857d51ae9931d79a16406f2d9369`.

All 17 checks in the receipt pass: every path is byte-exact, every group root
recomputes, and the untouched-group counter equals the touched set on every
region read.

## Measured table

Times are one run in the court (not a distribution). "stored read" is the sum
of the touched groups' stored payload bytes; it is not a disk-counter claim.

| Path | Bytes | Time | Groups decoded |
|---|---:|---:|---:|
| Mode-20 encode | 108,743,984 frame / 108,733,075 payload | 499.8 ms | — |
| Mode-20 full decode | 268,435,456 | 616.2 ms | 64 |
| Mode-20 single group (group 16) | 4,194,304 out / 1,698,934 stored read | 9.15 ms | 1 |
| Mode-20 four non-contiguous groups `[1,21,42,63]` | 16,777,216 out / 6,795,996 stored read | 37.83 ms | 4 |
| Mode-20 four contiguous groups `[1..5]` | 16,777,216 out / 6,795,886 stored read | 37.73 ms | 4 |
| Mode-20 sequential eight `[0..8]` | 33,554,432 | 75.58 ms | 8 |
| Mode-20 random eight `[14,17,19,20,21,36,40,52]` | 33,554,432 | 74.68 ms | 8 |
| Raw file + `pread`, one group | 4,194,304 | 0.414 ms | — |
| Raw file + `pread`, four groups | 16,777,216 | 2.68 ms | — |
| One ZSTD-3 frame, encode | 4,236,607 | 51.7 ms | — |
| One ZSTD-3 frame, full decode | 268,435,456 | 57.0 ms | — |
| Mode-15 tiles (one tile = one group), encode | 108,741,170 | 545.5 ms | — |
| Mode-15 full decode | 268,435,456 | 603.2 ms | — |
| Mode-15 single region | 4,194,304 | 9.40 ms | 1 tile |
| Mode-15 contiguous four regions | 16,777,216 | 37.20 ms | 4 tiles |
| Mode-15 62-group span | 260,046,848 | 572.2 ms | 62 tiles |

## Untouched-group counter

`reset_engram_group_decode_count()` is called before each region read and
`engram_group_decode_count()` after it. The receipt records both the touched
set the codec returns and the counter:

| Read | Touched set | Groups decoded |
|---|---|---:|
| single group 16 | `[16]` | 1 |
| four non-contiguous | `[1,21,42,63]` | 4 |
| four contiguous | `[1,2,3,4]` | 4 |
| sequential eight | `[0..7]` | 8 |
| random eight | `[14,17,19,20,21,36,40,52]` | 8 |

The counter equals the touched-set length in every case, so untouched groups
are not decoded. This is a codec-level counter; it does not claim how many
bytes the backend read (the Rust store's own range reads are exercised by
`tests/engram_store.rs`).

## Incremental update

Changing 4 of 64 groups (`[1,21,42,63]`) and referencing the other 60:

| | Incremental | Full rewrite |
|---|---:|---:|
| Encode | 62.7 ms | 499.7 ms |
| Frame bytes | 16,790,298 | 118,727,256 |
| Payload bytes | 16,779,264 | 108,733,075 |
| Full decode | 2,302.8 ms | 616.2 ms |

The old version is byte-identical after the update (the receipt checks the
old descriptor root and the snapshot-visible table). The incremental frame
stores only the four changed groups and emits the other 60 as references.

## Every loss

1. **Whole-table size and decode.** Mode 20 is 25.7× larger than one ZSTD-3
   frame on this fixture (108.74 MB vs 4.24 MB) and 10.8× slower to decode
   (616 ms vs 57 ms). The cause is structural: each group is Huffman-coded on
   its own byte planes, so the repeated template row cannot be matched across
   groups, and ZSTD's longer-range matching wins on this synthetic table. The
   sparsity model deliberately favors ZSTD; a less compressible table would
   narrow the gap, but this receipt does not claim that it would.
2. **Small single-group reads lose to raw `pread`.** One group costs 9.15 ms
   in mode 20 vs 0.414 ms for a raw `pread` of 4 MiB (22.1× slower). The
   stored bytes read are 2.47× smaller (1.70 MB vs 4.19 MB), but the Huffman
   decode dominates.
3. **Four-group reads lose to raw `pread`** (37.8-37.8 ms vs 2.68 ms, 14.1×)
   and the contiguous four-group read is 1.4 % slower than mode 15 tiles
   (37.73 ms vs 37.20 ms).
4. **Single-group read is effectively tied with mode 15** (9.15 ms vs
   9.40 ms); the difference is within run-to-run noise, so mode 20 does not
   claim a win there.
5. **Incremental decode is 3.74× slower than a full rewrite decode** (2.30 s
   vs 616 ms). A full decode of the new version resolves every referenced
   group through the earlier frame and still decodes all 64 groups. Only the
   *write* is cheaper.
6. **Per-group Huffman table overhead.** Each group pays one 256-byte table
   per byte plane; this is visible in the small-group payload (1.70 MB for a
   4 MiB group) and in the 4 MiB groups that compress to about 4.19 MB
   (the incremental payload for four changed groups is 16.78 MB).
7. **Region-read verification caveat.** A partial read verifies the touched
   groups' roots but **not** the frame trailer, the payload hash or the
   whole-table content hash. Full decode verifies all three. This is the same
   honesty as the mode-15 prefix reader.
8. **Sequential vs random access is not differentiated here.** 75.6 ms vs
   74.7 ms for eight groups. The frame is in memory in this court; the run is
   not a disk-random-access claim.
9. **Go store region read reads the whole frame blob** and decodes only the
   touched groups; it does not yet issue per-group byte-range reads. The Rust
   store issued range reads for exactly the touched groups in the store test.
10. **The generator is synthetic.** It is not a public n-gram table or a
    serving trace; no cache, fleet, or power-loss claim is made.

## What the mode is for

Mode 20's measured win is the incremental update: 7.97× less encode wall time
and 7.07× fewer frame bytes when 4 of 64 groups change. It also exposes exact
per-group roots and a region read that provably does not decode untouched
groups (counter evidence in the receipt). The whole-table and raw-pread losses
above are real and are why the planner never selects mode 20 implicitly: the
caller must supply engram metadata and opt in.
