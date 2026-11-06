# R11 — physical object packs (measured)

Court: `bench/receipts/packs-20261106T113605Z.json` (four configurations),
`bench/receipts/s3-court-20261106T113605Z.json` (MinIO provenance), raw log
`bench/receipts/packs-20261106T113605Z.log`, test summary
`bench/receipts/packs-20261106T113605Z.tests.txt`.

Revision: `8b42103` on `staging`. Bench court image `weft/bench-court:1`
(`sha256:4041d66e…`), 3 GiB / 4 vCPU / 1024 pids, bounded `/tmp`,
`cap-drop=ALL`. MinIO `RELEASE.2025-04-22T22-12-26Z` (`sha256:53e2a2cb…`),
single node, tmpfs data directory.

This is a single-host benchmark with a deterministic synthetic fixture. It is
not a cloud, fleet, device-cache, media, power-loss, or production claim.

## What was built

`PackedBlobPlane` (`src/packs.rs`) wraps a `BlobPlane` and a `CommitPlane`:

* blobs at or below `member_threshold_bytes` are appended to an in-memory open
  pack; a pack is published with `put_if_absent` under
  `H("weft/pack/v1", pack bytes)` when the next member would exceed
  `pack_target_bytes` or `max_members_per_pack`;
* membership (`key → pack, offset, length, digest`) lives in
  `catalog_segments` immutable content-addressed catalog blobs, named by one
  mutable `pack-catalog.root` control record replaced with a compare-and-swap;
* a member read is one catalog lookup (cached for the process) and one
  `get_range` of exactly the member's bytes, with the member digest verified
  (`Corrupt` on mismatch, short read, or truncated pack);
* `flush` (called by the store before it publishes a commit record) seals the
  open pack, so a commit never names a blob that is not durable;
* when the catalog bound is reached, packing stops and new blobs are published
  flat;
* a plane without `true_range_reads` is refused at construction.

No manifest, commit, or superblock byte changes. Logical keys are unchanged;
packs live in `packs/<ab>/<hex>`, catalog segments in
`pack-catalog/<ab>/<hex>`, the catalog root in `pack-catalog.root`.

Pack policy for this court: 16 MiB target, 64 KiB member threshold, 65,536
members/pack, 8 MiB catalog bound, 256 catalog segments.

## Workload

* 10,000 files of 1,024 bytes, written through the store in **one group
  commit** (`chunk_size = 1024`), i.e. 10,000 chunks + 10,000 manifests =
  20,001 catalogued blobs (one extra state manifest).
* Read 1,000 chunk blobs sequentially and the same 1,000 in a deterministic
  random order, each pass cold (fresh plane) and warm (same plane again).
* An isolated pack of exactly 16 MiB (2,048 × 8 KiB members) and one 4 KiB
  range read at offset 2,048 inside member 1,000.
* `verify --full` on the writer and again after a read-only reopen.

All four configurations report the same generation `2`, the same snapshot root
`57c24012a9483db06f5c6fb3ca9b8860430492276e48e64cdbb21df034c37975`, the same
read digest `a8812b26c57dc49c2371b6d52fb2b6063e21e84023eef360fb9b772695871b99`,
and zero verify problems (`exactness.match = true`). Packing changes physical
layout only.

## Publish 10,000 small blobs (one group commit)

| configuration | object requests | writes | wall ms | bytes written | physical footprint |
|---|---:|---:|---:|---:|---:|
| local flat | 20,003 | 20,003 | 298.3 | 16,524,362 | 11,840,000 |
| local packed | **262** | 260 | **185.4** | 18,809,103 | 14,112,521 |
| s3 flat | 20,003 | 20,003 | 7,321.5 | 16,528,458 | 11,840,000 |
| s3 packed | **261** | 260 | **2,791.7** | 18,809,103 | 14,112,521 |

Packed publishes **76.4x / 76.6x** fewer object requests and is 1.61x (local)
and 2.62x (S3) faster in wall time. It writes **13.8% more bytes** and its
on-disk footprint is **19.2% larger**: the catalog costs 2,272,401 bytes for
20,001 members (113 bytes/entry), and both the pack and its catalog segments
are rewritten when the pack is sealed.

The 260 packed writes are one 11.8 MiB pack, 256 catalog segments, one catalog
root compare-and-swap, plus commit/superblock/format publications. A catalog
segment is republished when any member of it is added, so every seal rewrites
the whole touched catalog: the written-byte cost is O(catalog size) per seal,
not O(members in that seal).

Counter caveat: both planes perform one *existence* read per blob before
publishing it (the store verifies an existing blob before treating publication
as idempotent). The local and S3 backends do not count a 404/failed read in
`reads`, so those 10,000 probes are absent from both columns equally. On S3
they are real HTTP requests that this receipt does not count.

## Read 1,000 chunk blobs (1 KiB members)

| configuration | pass | requests | bytes read | p50 ms | p99 ms | wall ms |
|---|---|---:|---:|---:|---:|---:|
| local flat | sequential cold | 1,000 | 1,024,000 | 0.0016 | 0.0020 | 5.4 |
| local packed | sequential cold | 1,252 | 3,263,612 | 0.0026 | 0.0129 | 8.6 |
| local flat | sequential warm | 1,000 | 1,024,000 | 0.0012 | 0.0014 | 5.0 |
| local packed | sequential warm | 1,000 | 1,024,000 | 0.0024 | 0.0027 | 6.1 |
| local flat | random cold | 1,000 | 1,024,000 | 0.0012 | 0.0045 | 5.0 |
| local packed | random cold | 1,252 | 3,263,612 | 0.0025 | 0.0128 | 8.4 |
| local flat | random warm | 1,000 | 1,024,000 | 0.0012 | 0.0013 | 4.9 |
| local packed | random warm | 1,000 | 1,024,000 | 0.0024 | 0.0026 | 6.1 |
| s3 flat | sequential cold | 1,000 | 1,024,000 | 0.1723 | 0.2588 | 181.7 |
| s3 packed | sequential cold | 1,252 | 3,263,612 | 0.3589 | 1.8524 | 444.3 |
| s3 flat | sequential warm | 1,000 | 1,024,000 | 0.1729 | 0.3195 | 186.1 |
| s3 packed | sequential warm | 1,000 | 1,024,000 | 0.3437 | 1.5820 | 385.1 |
| s3 flat | random cold | 1,000 | 1,024,000 | 0.1693 | 0.2448 | 178.7 |
| s3 packed | random cold | 1,252 | 3,263,612 | 0.3508 | 1.8499 | 433.1 |
| s3 flat | random warm | 1,000 | 1,024,000 | 0.1685 | 0.2384 | 177.0 |
| s3 packed | random warm | 1,000 | 1,024,000 | 0.3439 | 1.6002 | 384.7 |

**Packed reads are a loss at this member size.** A cold read pays one root
read, ~252 segment reads (for 1,000 members spread over 256 segments) and
1,000 ranged pack reads: 1.25x requests and **3.19x bytes read**. A warm
catalog removes the segment reads (1,000 requests, 1,024,000 bytes) but each
read is still a ranged request, and on S3 a range GET is about twice the
latency of a 1 KiB whole-object GET (p50 0.3437 vs 0.1685-0.1729 ms; p99
1.58-1.85 vs 0.24-0.32 ms). The catalog is 2.27 MiB for 20,001 members; at
1 KiB members that is inherent read amplification.

## 4 KiB range read from a 16 MiB pack

| configuration | packs | pack bytes | requests | range reads | bytes read | ms |
|---|---:|---:|---:|---:|---:|---:|
| local packed | 1 | 16,777,216 | 1 | 1 | 4,096 | 0.0019 |
| s3 packed | 1 | 16,777,216 | 1 | 1 | 4,096 | 0.3807 |

The backend byte counter moved by exactly 4,096 bytes for one ranged request on
a 16,777,216-byte pack. The whole pack is never read or decoded for a member.

## `verify --full` and footprint

| configuration | verify ms | problems | checked objects | checked manifests | checked chunks | peak RSS KiB |
|---|---:|---:|---:|---:|---:|---:|
| local flat | 479.7 | 0 | 10,000 | 10,000 | 10,000 | 74,008 |
| local packed | 465.8 | 0 | 10,000 | 10,000 | 10,000 | 80,044 |
| s3 flat | 16,202.3 | 0 | 10,000 | 10,000 | 10,000 | 78,504 |
| s3 packed | 23,103.2 | 0 | 10,000 | 10,000 | 10,000 | 80,732 |

The read-only reopen also verified clean and recovered the same root and
generation in all four configurations.

**A full scan is slower on a packed store**: on S3, 23,103.2 ms vs
16,202.3 ms (+42.6%), because every member read is a catalog lookup plus a
ranged GET and the catalog segments are read too. Local is slightly faster
packed here (465.8 vs 479.7 ms) and is within run-to-run noise.

## Every loss

1. **Written bytes.** Packed writes 18,809,103 bytes vs 16,524,362 (local) /
   16,528,458 (S3): **+13.8%**, because the catalog is republished per seal
   (2.27 MiB for 20,001 members) and both the pack and its segments are
   rewritten.
2. **Physical footprint.** 14,112,521 vs 11,840,000 bytes: **+19.2%** at 1 KiB
   members; the catalog is pure overhead. The ratio improves with member size.
3. **Cold read amplification.** 1.25x requests and 3.19x bytes read at 1 KiB
   members; p99 grows from 0.2384-0.3195 to 1.5820-1.8524 ms on S3.
4. **Warm read latency.** One ranged request costs about twice one whole-object
   request for a 1 KiB object on S3 (p50 0.3437 vs 0.1685-0.1729 ms).
5. **Full scan.** `verify --full` on S3 is 42.6% slower packed.
6. **Peak RSS.** +6,036 KiB (local) / +2,228 KiB (S3): the open pack plus the
   cached catalog (bounded by `max_catalog_bytes`, 8 MiB) and pack buffers.
7. **No pack rewriting.** A dead member's bytes stay in the pack until every
   member is deleted; the pack is then reclaimed by `gc --apply`.
8. **Write-back buffering.** Blobs are not durable until the pre-commit flush.
   A crash loses only blobs no commit names, but there is no incremental
   durability for a long stream of small writes before the flush.
9. **Duplicate existence probe.** Both planes ask the store's "does this blob
   already exist" question; the packed plane caches up to 4,096 observed-absent
   keys in-process to avoid asking twice, but that cache is an optimisation for
   a single-writer store, not a durable fact.

## What remains

* **Go parity.** WeftFS-Go has no pack plane; the Go store cannot read a store
  whose blobs are packed.
* **Pack rewriting / compaction.** Packs are immutable and never repacked, so
  dead members occupy space until the whole pack is unreferenced. There is no
  `compact` for packs.
* **Catalog layout.** One root per store and 256 segments is a fixed choice;
  there is no tiered or sharded catalog for very large member counts, and no
  on-disk index of pack → members (reclamation scans the catalog).
* **Physical power loss and media errors.** The crash proxy in
  `tests/packs.rs` kills the process at two pack phases; it does not cut power
  or corrupt media.
* **Catalog-bound fallback** is tested at the plane level, not measured in this
  court.
* **Range reads of a member do not verify the digest** (a partial read cannot);
  whole-object reads do. This matches the flat plane, where only whole-object
  reads are content-checked.

## Reproduction

```
bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/packs_court.sh"
bash scripts/s3_court.sh --packs        # MinIO, four configurations
bash scripts/docker-court.sh rust "cd /work && cargo test --test packs -- --test-threads=1"
```

The court also runs `tests/packs.rs` (13 tests) inside the same container and
embeds the summary in the receipt.
