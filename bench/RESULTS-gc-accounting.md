# GC byte accounting — cause, fix, and corrected trajectory numbers

Receipt: `bench/receipts/trajectory-20261108T083234Z.json` (reduced trajectory: pythia
step0/1, 50 synthetic steps, Qwen half skipped, same 3 GiB / 4 vCPU bench court).
Superseded wrong receipt: `bench/receipts/trajectory-20261107T220652Z.json`.

## Cause

`Store::gc` accumulated **two different kinds of bytes into one field**:

```rust
// data blobs (objects/ + manifests/)            -> the physical data tree
report.deleted_bytes += len;
...
// advisory index segments (indexes/)            -> rebuildable accelerators
report.deleted_bytes += len;
```

The trajectory court measures the data tree with a walk of `objects/` + `manifests/`
only. In the pinned-snapshot phase no data blob is eligible (the snapshot is a root), so
`deleted_blobs = 0` and the data delta is exactly 0 — yet the report said
**3,466,466,150 B** because it also counted 3,271 stale advisory index segments. The
segments are real on disk, but they are not data: deleting one rebuilds on the next
query. The counter therefore could not mean "physical data reclaimed", and it was
already feeding dashboards as if it did.

## Fix

The report now carries three counters:

* `deleted_bytes` — physical bytes of **data blobs** (objects + manifests) unlinked by
  this pass. For a flat plane this is exactly the shrink of the data tree.
* `deleted_index_bytes` — physical bytes of **advisory index segments** unlinked.
  Reported separately; never added to the data counter.
* `deleted_total_bytes` — `deleted_bytes + deleted_index_bytes + reclaimed_pack_bytes`,
  so every unlinked byte is accounted exactly once and the components reconcile.

The same split is implemented in WeftFS-Go (`DeletedBytes`, `DeletedIndexBytes`,
`DeletedTotalBytes`). `weft gc --json` and `weftfs gc --json` expose all three. The
trajectory harness now emits `tree_delta_bytes`, `index_bytes_before_gc/after_gc`,
`deleted_data_bytes`, `deleted_index_bytes`, `deleted_total_bytes`, `data_reconciled`
and `total_reconciled_within_written_indexes`.

Regression test: `tests/gc_accounting.rs` (3/3). It builds a store, writes a checkpoint,
adds filler commits, creates a pinned snapshot, deletes the checkpoint path, and then:

* pinned case: `deleted_bytes == 0`, the data tree does not shrink, and every named
  index deletion is real and exactly sized;
* unpinned case: `deleted_bytes == data_bytes_before − data_bytes_after` exactly, and
  `deleted_total_bytes == deleted_bytes + deleted_index_bytes + reclaimed_pack_bytes`;
* dry run: every counter is zero and the tree is unchanged.

## Corrected numbers beside the old ones

Synthetic 50-step series, same fixture and same measured walk.

| phase | field | old (wrong) | corrected |
|---|---|---:|---:|
| pinned snapshot | `deleted_bytes` | 3,466,466,150 | **0** |
| pinned snapshot | data-tree delta | 0 | **0** |
| pinned snapshot | `deleted_index_bytes` | *(not reported)* | **3,466,466,150** |
| pinned snapshot | `deleted_total_bytes` | *(not reported)* | **3,466,466,150** |
| pinned snapshot | blobs / indexes deleted | 0 / 3,271 | 0 / 3,271 |
| unpinned | `deleted_bytes` | 3,467,041,895 | **150,596** |
| unpinned | data-tree delta | 150,596 | **150,596** |
| unpinned | `deleted_index_bytes` | *(not reported)* | **3,466,891,299** |
| unpinned | `deleted_total_bytes` | *(not reported)* | **3,467,041,895** |
| unpinned | blobs / indexes deleted | 142 / 3,272 | 142 / 3,272 |
| both | `data_reconciled` | *(not reported)* | **true** |
| both | restore exact | true | true |

## What the corrected numbers expose

The advisory index directory is **3.47 GB for a 74 MB data tree** on this fixture:
3,271 per-commit full-state WFX2 segments, ~1.06 MB each, because the synthetic court
writes one commit per tensor and every commit carries a full-state index. GC removes the
segments not named by a retained root, leaving 12,796,270 B after the pass. This is a
real storage-amplification finding, not a counter artifact; it is reported here and is
**not** fixed in this stage. The corrected split makes it visible and prevents it from
ever being mistaken for data.

Non-claims: the same reduced trajectory as the receipt (pythia 0/1, 50 synthetic steps,
no Qwen); process-fault crash proxy only; one host, warm caches; index amplification is
measured, not yet mitigated.
