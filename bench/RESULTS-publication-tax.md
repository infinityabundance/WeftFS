# Publication tax — R20 attribution, options, and the chosen default

**Status:** executed. Rust store on `staging`; no wire change (frame, manifest
v1–v4, commit v1/v2, WFX3 index, conformance vectors and the store fixture are
byte-identical). Every number below comes from a committed receipt produced in
`weft/bench-court:1` on the frozen Qwen2.5-0.5B-Instruct subset.

**Headline.** C (the shipped two-stage planner) goes from **2,343.2 ms to
1,236.8 ms** (−47.2 %) and its footprint **shrinks** from 282,283,011 B to
**282,106,049 B** (−176,962 B). All 101 tensors restore byte-exactly in every
repetition, the chosen representations are unchanged, and the crash proxy still
shows old-or-new-whole at every fault point.

Two changes did it, and the measurements separate them:

| Change | Wall | Footprint |
|---|---:|---:|
| One group commit for the 101 namespace ops + one advisory index segment | **−718.1 ms** (2,343.2 → 1,625.1) | **−176,962 B** |
| One durability barrier per group instead of one per blob | **−388.3 ms** (1,625.1 → 1,236.8) | 0 B |
| Packing the blob plane (64 KiB/1 MiB/8 MiB member bound) | **+247 to +455 ms** (loss) | −3,576 to +9,084 B |
| **Both shipped changes (the new default)** | **−1,106.4 ms (−47.2 %)** | **−176,962 B** |

R20's target was C ≤ 1,600 ms at 282,283,011 B. **Met**: 1,236.8 ms
(least-contended observation) at 282,106,049 B. The published board-2 rung is
also met (`final ≤ 284,140,000 B`); board 1 moves from `1.73× G1` to
**3.65× faster than A** with G1 cleared and **G2 (≤ 1,344 ms) met on the
least-contended observation**.

Two independent sessions measured the same minima within 1 %: this receipt
(WeftFS `ad0ca3b`) gives C 1,236.8 ms and C-percommit 2,343.2 ms; the retained
`…T133000Z` receipt gave 1,225.4 and 2,341.6. The differences are host noise.
The second session's *medians* are worse because the host was heavily shared
(see §7); the frozen contract attributes cost from the least-contended
repetition, and both boards are reported here.

## 1. Receipts

| Receipt | What it is |
|---|---|
| [`r20-publication-20261112T150000Z.json`](receipts/r20-publication-20261112T150000Z.json) | **cited.** 7 interleaved repetitions × 14 rows, phase profile on every timed repetition, per-tensor detail on repetition 1, crash proxy on every Weft row, all 11 checks pass. WeftFS `ad0ca3b`, WeftCodec `1412227`. |
| [`augmentation-local-20261112T150000Z.json`](receipts/augmentation-local-20261112T150000Z.json) | **cited.** Unmodified R3 contract court at the same revision and stamp; its C row is the same-revision identity proof for the ablation runner's C (`default_c_matches_official_court: true`). |
| `r20-publication-20261112T133000Z.json.zst` | Retained repeat of the same matrix at an *earlier* revision (`804fd38`), all 11 checks passed, minima 1,225.4 / 2,341.6 ms. **Not cited**: that commit's tree is incomplete — the barrier split was carried by two new `BackendStats` fields whose zero-initialisers in `src/azure.rs`, `src/gcs.rs` and `src/s3.rs` were never committed, so the revision does not build from a clean checkout. The final design reports the split through `StorageBackend::durability_barriers()` instead (no stats fields, no out-of-scope file touched), and the cited receipt reproduces the numbers. It is kept as the evidence that the method and the numbers are stable across
  sessions. |
| `augmentation-local-20261112T133000Z.json` | Retained contract court for that earlier revision. |
| `r20-publication-20261112T120000Z.json.zst` | Retained repeat (same revision family). All 11 checks passed, but the crash proxy reopened the packed rows as a flat local store, so their three crash verdicts were about the harness, not the store (they reported `tensor_missing: 101`). Fixed and retained. |
| `r20-publication-20261112T112000Z.json.zst`, `…T113000Z.json.zst` | Retained single-repetition probes used to diagnose the packed-read failure and to bound the pack sweep. Their `at_least_seven_reps` check fails by construction; the timings in them are *not* cited (one `pack64k` sample is 11.9 s and is a host stall, see §7). |
| `r20-publication-20261112T105952Z.log` | Retained **failed** run, log only (no receipt was produced): the restore phase of a packed row opened the store as a flat local store and failed `NotFound` on the first tensor. Found the harness bug that both packed rows then carried; fixed by opening each row's store with its own blob plane. |

## 2. What the 202 blob publications actually are

The R17 ledger said "202 blob publications (frame + manifest blobs)". Measured
by kind (`PublicationLedger`, counted where each artifact is handed to the
backend), it is exactly:

| Category | Publications | Bytes |
|---|---:|---:|
| `manifest_blobs` (one frame + one tensor descriptor manifest per object) | **186** | 281,977,361 |
| `object_blobs` (the 16 raw-representation tensors' single 256-byte chunks) | **16** | 4,096 |
| **Total blob publications** | **202** | **281,981,457** |

So the split is **186 manifest blobs + 16 chunk objects**, not 101 + 101: 85
of the 101 objects (35 mode-9 float-tensor, 50 ZSTD) are a single frame blob
plus a descriptor manifest, and 16 are one 256-byte raw chunk plus a descriptor
manifest. The descriptor manifests are ~1 KiB each; the frames carry the data.

The control plane before R20 was **103 commit records + 103 superblock slots +
103 advisory index segments**, because every tensor was its own commit:

| Artifact | Before (per tensor) | After (one group) | Δ |
|---|---:|---:|---:|
| Commit records | 103 | 3 | −100 |
| Commit-record bytes | 121,317 | 76,920 | −44,397 |
| Superblock slot publications | 103 | 3 | −100 |
| Advisory index segments | 103 | 3 | −100 |
| Index bytes | 172,030 | 39,465 | −132,565 |
| Data file barriers (`fsync` of file data) | 413 | 213 | −200 |
| Directory barriers (`fsync` of a directory) | 413 | 161 | −252 |
| Backend writes (all objects) | 516 | 216 | −300 |
| Backend bytes written | 282,705,117 | 282,118,555 | −586,562 |
| **Physical footprint (du walk)** | **282,283,011** | **282,106,049** | **−176,962** |

Three commits remain because the store's genesis commit and the `/tensors`
directory creation each publish before the tensor group; they are the same in
both rows.

The footprint delta closes exactly: −44,397 B fewer commit records plus
−132,565 B fewer index segments = **−176,962 B**. The 100 extra index segments
were each ~1,670 B; the single segment for the whole resulting state is
13,155 B.

## 3. Durability ledger, before and after

`C`, least-contended repetition (1,236.8 ms). Categories are measured; the
residual is `wall − accounted` and is never called zero.

| Category | Before (`C-percommit`, 2,343.2 ms) | After (`C`, 1,236.8 ms) | Δ |
|---|---:|---:|---:|
| init (FORMAT, LOCK, superblock, plane probes) | 20.5 | 20.9 | +0.5 |
| source read (fixture → memory) | 32.2 | 31.3 | −0.9 |
| **prepare** | **1,564.9** | **1,064.9** | **−500.0** |
| ├ encode total | 695.9 | 704.6 | +8.7 |
| │ ├ ZSTD level-3 | 278.5 | 286.6 | |
| │ ├ mode-9 `FLOAT_TENSOR` | 195.6 | 192.8 | |
| │ ├ analyse (R18 two-stage, nested in encode) | 181.2 | 184.4 | |
| │ └ other encode/plan (raw descriptor, pricing, logical root) | 40.7 | 40.8 | |
| ├ **blob publish (202 publications)** | **778.5** | **272.2** | **−506.3** |
| ├ digest (blob ids, content roots) | 66.7 | 67.0 | +0.3 |
| └ prepare unattributed | 23.8 | 21.1 | −2.7 |
| **commit** | **708.6** | **105.4** | **−603.2** |
| ├ advisory index segment (per commit → per checkpoint) | 342.5 | 4.2 | −338.3 |
| ├ commit publication (record + superblock) | 360.1 | 100.8 | −259.3 |
| │ └ of which the deferred durability barrier | 0.0 | 97.0 | +97.0 |
| └ commit unattributed | 6.0 | 0.5 | |
| footprint walk | 1.5 | 0.8 | |
| **accounted** | **2,327.7** | **1,223.3** | |
| **unattributed residual** | 15.5 (0.66 %) | **13.4 (1.09 %)** | |
| **measured wall** | **2,343.2** | **1,236.8** | **−1,106.4** |

(Nested rows are shown under their parent and are never added twice: `analyse`
is inside `encode`, and the durability barrier is inside commit publication.)

Reading of the two mechanisms:

* **Blob publication was fsync-bound, not byte-bound.** 282 MB moved in
  778.5 ms (362 MB/s) when each blob issued its own data + directory barrier,
  and in 272.2 ms (1,036 MB/s) when the group writes and renames all 202
  objects and issues one barrier of 213 file syncs + 161 directory syncs at
  the end (**97.0 ms** this session, 75.2 ms in the retained `133000Z` run —
  the barrier's cost depends on how much of the 282 MB the page cache has
  already written back). The barrier is *additional* work that did not exist
  before, so the net saving in prepare is 506.3 − 97.0 = 409.3 ms; the barrier
  is accounted inside commit publication, which is why prepare falls by
  500.0 ms while commit *rises* by 97.0 ms of barrier on an 8.3 ms control-plane
  base.
* **The control plane was 702.6 ms for 101 commits** — 342.5 ms of full-state
  index segments and 360.1 ms of commit records + superblock slots, i.e.
  **7.0 ms per commit** on this host (3.4 ms of index segment plus 3.6 ms of
  record and head). One group commit collapses it to 8.3 ms measured on the
  group-plus-eager-barrier row (4.2 ms index + 3.8 ms publication).

## 4. Options matrix (7 interleaved repetitions, one session)

Milliseconds. `min` is the least-contended observation, `median`/`p95` are the
observed distribution (§7 explains the tails). Footprint and exactness are
identical in all seven repetitions of a row.

| Row | min | median | p95 | CPU med | RSS med KiB | Footprint (B) | Bytes written | Requests | file/dir fsyncs | Cold restore med |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| A (native anchor) | 339.0 | 6,429.8 | 7,188.7 | 110 | 270,980 | 424,439,296 | — | 101 | — | 427.4 |
| B (raw, one atomic publication) | 900.2 | 1,476.0 | 20,709.9 | 240 | 272,676 | 424,608,542 | 424,621,048 | 611 | 608/310 | 674.0 |
| B-percommit (R17 form) | 2,849.9 | 3,075.1 | 38,649.4 | 280 | 272,568 | 424,784,917 | 425,207,023 | 911 | 808/808 | 624.4 |
| C-percommit (R19 form) | 2,343.2 | 2,359.8 | 29,787.0 | 1,260 | 688,268 | 282,283,011 | 282,705,117 | 516 | 413/413 | 1,604.6 |
| C-group (group, eager barriers) | 1,625.1 | 1,667.9 | 16,286.7 | 1,200 | 687,988 | 282,106,049 | 282,118,555 | 216 | 213/213 | 1,527.9 |
| **C (default: group + deferred barrier)** | **1,236.8** | 1,436.3 | 11,584.0 | **1,210** | **688,348** | **282,106,049** | **282,118,555** | **216** | **213/161** | 1,511.5 |
| C-pack64k | 1,502.1 | 1,521.5 | 11,745.7 | 1,200 | 687,812 | 282,100,120 | 282,133,206 | 171 | 171/171 | 1,424.5 |
| C-pack1m | 1,483.7 | 1,498.6 | 8,525.6 | 1,200 | 688,520 | 282,102,473 | 282,135,511 | 162 | 162/162 | 1,419.2 |
| C-pack8m | 1,692.1 | 1,711.4 | 16,929.0 | 1,230 | 688,592 | 282,115,133 | 282,214,236 | 227 | 227/227 | 1,428.9 |
| D (raw + protocol) | 957.5 | 1,537.1 | 20,971.5 | 250 | 272,688 | 424,659,735 | 424,629,900 | 611 | 616/318 | 547.6 |
| E (planned + protocol) | 1,257.1 | 1,439.7 | 12,271.8 | 1,210 | 687,552 | 282,157,242 | 282,127,407 | 216 | 221/169 | 1,434.0 |
| C-nom9 (R17 attribution) | 21,427.2 | 26,883.4 | 32,391.9 | 20,950 | 893,080 | 330,160,010 | 330,172,516 | 216 | 213/158 | 901.3 |
| C-both (R17 attribution) | 1,020.5 | 1,204.5 | 12,226.0 | 670 | 687,412 | 330,355,860 | 330,368,366 | 216 | 213/159 | 880.3 |
| C-legacy (decision-diff control) | 21,526.6 | 21,893.6 | 27,980.4 | 21,530 | 893,068 | 282,106,049 | 282,118,555 | 216 | 213/161 | 1,435.0 |

Notes on the rows:

* `C-percommit` reproduces the R19 row within session noise (R19 measured
  2,328.4 ms and 282,283,011 B; here 2,343.2 ms and the same footprint, +0.6 %),
  which is the same-revision identity proof for the "before" column.
* **Every row now publishes with the new default**, including D and E, so the
  R17/R19 rows are reproducible only in the `-percommit` form (`B-percommit`,
  `C-percommit`). `C-legacy`, the `noz19`/`nom9`/`both` ablation suffixes and
  the default rows differ only in planner or candidate set, never in
  publication.
* `C-legacy` and `C-nom9` are ~21.5 s because the exhaustive planner encodes
  every candidate: it runs the level-19 ZSTD encode for 18 tensors even though
  the planner selects none of them. They are controls, not wall rows. (R20
  deliberately re-ran them so the same-run decision diff exists:
  `two_stage_matches_exhaustive: true`, 0 of 101 decisions changed,
  `two_stage_storage_unchanged: true`.)
* `C-both` (no mode-9 candidate) is the fastest planned row at 1,020.5 ms but
  costs 330,355,860 B — 48.2 MB (17.1 %) more than C. It is the R17 ablation
  row, not a candidate default: it trades 48 MB for 216 ms.
* `B` and `D` are raw-representation rows: they publish more bytes with less
  encode, so their minima beat C while their footprints are 424 MB.
* `D` and `E` publish two more commits (snapshot, context, provenance) after
  the tensor group, which is why their barrier and request counts differ from
  `B`/`C`.

### Storage delta per option — the pair that must never be split

| Row vs C | Wall Δ | Footprint Δ | Bytes-written Δ |
|---|---:|---:|---:|
| C-percommit (before) | **+1,106.4 ms** | **+176,962 B** | +586,562 B |
| C-group | +388.3 ms | 0 B | 0 B |
| C-pack64k | +265.3 ms | −5,929 B (−0.002 %) | +14,651 B |
| C-pack1m | +246.9 ms | −3,576 B (−0.001 %) | +16,956 B |
| C-pack8m | +455.3 ms | +9,084 B | +95,794 B |

## 5. The chosen default and why

`Options::defer_group_blob_sync` defaults to **true**, and the default rows
publish one group commit with one deferred durability barrier.

* The group commit is not a trade: it is **faster and smaller** on both boards
  (−718.1 ms, −176,962 B). It also strengthens the durability statement — 101
  files become visible atomically instead of one at a time.
* The deferred barrier is a pure wall win at **identical bytes** (−388.3 ms,
  0 B) and does not weaken the ordering rule (see §6).
* The pack plane is rejected. It is 20.0–36.8 % slower and, at 64 KiB and
  1 MiB, it writes *more* bytes (pack headers + a catalog of 120–195 segment
  publications for 170–204 members); its footprint win is 3.6–5.9 KB, which is
  0.001 % of the fixture, i.e. noise next to a 247 ms loss. At 8 MiB it is
  slower *and* larger. The pack plane remains available (`--pack`,
  `BackendKind::packed`) and is the right plane for many small objects; it is
  not right for 186 blobs whose median size is ~1 KiB next to 35 blobs of
  8 MiB.

## 6. Durability, crash consistency, and what is *not* weakened

The ordering invariant is unchanged and is now enforced by construction:

```
write + rename every blob  →  barrier: fsync each file, then each touched dir
                            →  commit record (put_atomic, durable)
                            →  superblock slot / fenced head
```

* `LocalBackend::publish_file` records `(file, directory)` instead of syncing
  when a batch is open; `StorageBackend::end_deferred_sync` opens every
  recorded file, `fsync`s it, then `fsync`s every touched directory once.
  **Every published file still gets its own data barrier** — the batch changes
  *when*, never *whether*, a byte is durable. The test
  `the_deferred_batch_skips_no_file_barrier_and_closes_before_the_commit`
  asserts `file_barriers == object_blobs + manifest_blobs + commit_records +
  index_segments` for both the eager and the deferred row, so no barrier can be
  skipped and none can happen after the commit record.
* The barrier runs inside `flush_buffered_blobs`, which the commit path already
  called before the commit record for the pack plane. A barrier error fails the
  commit: nothing is published and the in-memory state is rolled back.
* The commit record itself is written eagerly (the batch is already closed),
  and the advisory index is written *after* the commit, outside the batch.
* `begin_group` opens the batch; `commit_group` and `abort_group` both close it
  explicitly, so a later publication can never inherit an unbounded sync list.
* A crash before the barrier leaves unreferenced, possibly non-durable blobs
  and the previous whole generation; a crash after it leaves old-or-new-whole.

Crash proxy (every Weft row, three phases, `WEFT_CRASH_GROUP=1` for grouped
rows so the fault points inside the group commit are the ones exercised):

| Row | after_objects | after_commit_file | after_superblock_write |
|---|---|---|---|
| B, C, C-group, C-pack\*, C-percommit, C-legacy, C-nom9, C-both, D, E | old, 0 mismatches, 0 missing | old, 0 mismatches, 0 missing | new, 0 mismatches, 0 missing |

`crash_proxy_old_or_new: true` for all 11 checks. The `after_commit_file` row
reports the same pre-existing, benign `verify` note as R17/R19 (a commit file
exists above the published head — an unreferenced tail, which recovery rejects).

Retention/GC/index semantics are unchanged: exactly one attested index segment
per published checkpoint still covers the whole resulting state (a reader loads
or rebuilds exactly as before), the index-GC per-root rule is untouched, and
`tests/index_persistence.rs` (7), `tests/index_gc_bound.rs` (3),
`tests/retention.rs` (8) and `tests/gc_accounting.rs` (3) all pass. No index
attestation and no format was changed.

Explicit product-path evidence, `bench/receipts/r20-checkpoint-verify-20261112T121955Z.log`
(WeftFS `53bad7c`, bench court): `weftfs checkpoint write` of the 12
`model.layers.0.*` tensors (12 shard objects, 29,824,768 element bytes, manifest
root `2edee158…`) followed by **`verify --full`: ok, full, 2 commits checked, 0
problems**; `checkpoint verify` reports 12 tensors verified; `checkpoint
restore --topology 1` writes 29,826,096 bytes with SHA-256 `3a0f1570…`, identical
across two independent runs. This is a functional gate on the checkpoint path,
not a timing claim. (The store code did not change after that run: `53bad7c` →
`ad0ca3b` only moved where the barrier *split* is reported.)

## 7. Host contention, losses, and non-claims

* The host is shared, and it was much busier in the cited session than in the
  retained one: A's own median here is 6,429.8 ms against a 339.0 ms minimum
  (the retained `133000Z` session measured A at 333.1/375.0 ms). Every row's
  distribution is bimodal for the same reason (`C-group` p95 16,286.7 against
  a 1,625.1 ms minimum; `B`'s p95 20,709.9 against 900.2 ms). Per the R17
  contract the ledger and the ladder use the **least-contended** repetition and
  the medians/p95 are published unchanged. The minima agree across the two
  sessions within 1 % (C 1,236.8 vs 1,225.4; C-percommit 2,343.2 vs 2,341.6),
  and the retained run's medians were clean (C 1,246.2 ms). A single sample is
  never cited: the retained one-repetition probe `…T113000Z` saw an 11.9 s
  `pack64k` write that the 7-repetition row measures at 1,502.1 ms.
* **The pack plane loses on both boards here.** 120–195 catalog segment
  publications for 170–204 members cost a few hundred ms inside the commit's
  flush, and its own `blob_publish` drops to ~180–245 ms. Net +247 to +455 ms.
* `PackedBlobPlane` does not implement `begin_deferred_sync`
  (`CompositeBackend` returns `false`), so the pack rows keep eager per-blob
  barriers. A pack+deferred combination is therefore **not measured and not
  claimed**.
* Restore is unchanged: cold restore median 1,511.5 ms for C against 1,604.6 ms
  for the per-commit row — the sign here is host noise, not an improvement. R20
  is a write-path change and makes no read claim.
* `StorageBackend::durability_barriers()` returns `None` for a provider-ack
  backend (S3/GCS/Azure never claim `fsync`), so the split is reported only
  where it exists.
* Single host, one writer, local POSIX backend, warm page cache, one fixture
  subset. No media, power-loss, fleet, cloud or S3 claim.
* `C`'s residual is 13.4 ms (1.09 %) — the un-instrumented part is the harness
  loop, the prepare prologue (canonicalisation, attribute assembly) and
  in-memory state application for 101 staged ops.

## 8. Reproduce

```
WEFT_AUGMENTATION_STAMP=<stamp> scripts/augmentation_court.sh --local
WEFT_R20_STAMP=<stamp> WEFT_R20_REPS=7 \
  scripts/docker-court.sh bench "bash /work/bench/scripts/r20_publication_court.sh"
```

The court runs the ablation inside `weft/bench-court:1`
(`sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559`,
kernel 7.3.3-1-cachyos, 3 GiB memory == swap, 1024 pids, 4 CPUs, bounded
`/tmp`, `no-new-privileges`, `cap-drop=ALL`; limits not raised). Raw samples,
per-tensor rows and the phase profile for every timed repetition are in the
receipt; the full child-process log is in
`bench/receipts/r20-publication-20261112T150000Z.log`.

## 9. What this implies for R21

The next bottleneck by measured share of C's 1,236.8 ms is **encode (704.6 ms,
57.0 %)**, then **blob publication (272.2 ms, 22.0 %)**, then **analyse
(184.4 ms, 14.9 % — nested in encode)**, then **the durability barrier
(97.0 ms, 7.8 %)**, then digest (67.0 ms, 5.4 %), source read + init (52.2 ms,
4.2 %), the control plane (commit record, head and index, *excluding* the
barrier: 8.4 ms, 0.7 %) and the residual (13.4 ms, 1.1 %).

Within encode, ZSTD-3 is 286.6 ms and mode-9 is 192.8 ms. R21 should target the
bounded parallel pipeline: encode is embarrassingly parallel across tensors and
blob publication is now a single sequential writeback, so the Amdahl cap is set
by the 272 MB embedding. No new publication mechanism is needed — the control
plane is now 0.7 % of the write.
