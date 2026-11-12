# Publication tax — R20 attribution, options, and the chosen default

**Status:** executed. Rust store on `staging`; no wire change (frame, manifest
v1–v4, commit v1/v2, WFX3 index, conformance vectors and the store fixture are
byte-identical). Every number below comes from a committed receipt produced in
`weft/bench-court:1` on the frozen Qwen2.5-0.5B-Instruct subset.

**Headline.** C (the shipped two-stage planner) goes from **2,341.6 ms to
1,225.4 ms** (−47.7 %) and its footprint **shrinks** from 282,283,011 B to
**282,106,049 B** (−176,962 B). All 101 tensors restore byte-exactly in every
repetition, the chosen representations are unchanged, and the crash proxy still
shows old-or-new-whole at every fault point.

Two changes did it, and the measurements separate them:

| Change | Wall | Footprint |
|---|---:|---:|
| One group commit for the 101 namespace ops + one advisory index segment | **−709.1 ms** (2,341.6 → 1,632.5) | **−176,962 B** |
| One durability barrier per group instead of one per blob | **−407.1 ms** (1,632.5 → 1,225.4) | 0 B |
| Packing the blob plane (64 KiB/1 MiB/8 MiB member bound) | **+253 to +453 ms** (loss) | −3,576 to +9,084 B |
| **Both shipped changes (the new default)** | **−1,116.2 ms (−47.7 %)** | **−176,962 B** |

R20's target was C ≤ 1,600 ms at 282,283,011 B. **Met**: 1,225.4 ms (least
contended) and 1,246.2 ms (median) at 282,106,049 B. The published board-2
rung is also met (`final ≤ 284,140,000 B`); board 1 moves from `1.73× G1` to
**`3.68×` faster than A** with G1 cleared and **G2 (≤ 1,344 ms) met**.

## 1. Receipts

| Receipt | What it is |
|---|---|
| [`r20-publication-20261112T133000Z.json`](receipts/r20-publication-20261112T133000Z.json) | **cited.** 7 interleaved repetitions × 14 rows, phase profile on every timed repetition, per-tensor detail on repetition 1, crash proxy on every Weft row, all 11 checks pass. WeftFS `804fd38`, WeftCodec `1412227`. |
| [`augmentation-local-20261112T133000Z.json`](receipts/augmentation-local-20261112T133000Z.json) | **cited.** Unmodified R3 contract court at the same revision and stamp; its C row is the same-revision identity proof for the ablation runner's C (`default_c_matches_official_court: true`). |
| `r20-publication-20261112T120000Z.json` | Retained repeat of the same matrix. All 11 checks passed, but the crash proxy reopened the packed rows as a flat local store, so their three crash verdicts were about the harness, not the store (they were `tensor_missing: 101`). Fixed and retained. |
| `augmentation-local-20261112T120000Z.json` | Retained contract court for the repeat above. |
| `r20-publication-20261112T112000Z.json`, `…T113000Z.json` | Retained single-repetition probes used to diagnose the packed-read failure and to bound the pack sweep. Their `at_least_seven_reps` check fails by construction; the timings in them are *not* cited (one `pack64k` sample is 11.9 s and is a host stall, see §7). |
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
plus a descriptor manifest, and 16 are one chunk plus a descriptor manifest.
The descriptor manifests are ~1 KiB each; the frames carry the data.

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

`C`, least-contended repetition (1,225.4 ms). Categories are measured; the
residual is `wall − accounted` and is never called zero.

| Category | Before (`C-percommit`, 2,341.6 ms) | After (`C`, 1,225.4 ms) | Δ |
|---|---:|---:|---:|
| init (FORMAT, LOCK, superblock, plane probes) | 20.2 | 20.2 | 0.0 |
| source read (fixture → memory) | 31.3 | 31.6 | +0.2 |
| **prepare** | **1,571.3** | **1,076.3** | **−495.0** |
| ├ encode total | 695.6 | 703.2 | +7.6 |
| │ ├ ZSTD level-3 | 282.9 | 280.0 | |
| │ ├ mode-9 `FLOAT_TENSOR` | 191.8 | 191.1 | |
| │ ├ analyse (R18 two-stage, nested in encode) | 180.4 | 191.6 | |
| │ └ other encode/plan (raw descriptor, pricing, logical root) | 40.5 | 40.5 | |
| ├ **blob publish (202 publications)** | **786.0** | **286.0** | **−500.0** |
| ├ digest (blob ids, content roots) | 66.3 | 66.1 | −0.2 |
| └ prepare unattributed | 23.3 | 20.9 | −2.4 |
| **commit** | **702.4** | **83.6** | **−618.8** |
| ├ advisory index segment (per commit → per checkpoint) | 341.8 | 4.2 | −337.6 |
| ├ commit publication (record + superblock) | 355.0 | 79.0 | −276.0 |
| │ └ of which the deferred durability barrier | 0.0 | 75.2 | +75.2 |
| └ commit unattributed | 5.6 | 0.4 | |
| footprint walk | 1.2 | 0.7 | |
| **accounted** | **2,326.5** | **1,212.3** | |
| **unattributed residual** | 15.2 (0.65 %) | **13.1 (1.07 %)** | |
| **measured wall** | **2,341.6** | **1,225.4** | **−1,116.2** |

(Nested rows are shown under their parent and are never added twice: `analyse`
is inside `encode`, and the durability barrier is inside commit publication.)

Reading of the two mechanisms:

* **Blob publication was fsync-bound, not byte-bound.** 282 MB moved in
  786.0 ms (359 MB/s) when each blob issued its own data + directory barrier,
  and in 286.0 ms (986 MB/s) when the group writes and renames all 202 objects
  and issues one barrier of 213 file syncs + 161 directory syncs at the end
  (**75.2 ms**, 3.75 GB/s of writeback). The barrier is *additional* work that
  did not exist before, so the net saving in prepare is 500.0 − 75.2 = 424.8 ms;
  the barrier is accounted inside commit publication, which is why prepare falls
  by 495.0 ms while commit *rises* by 75.2 ms of barrier on an 11.5 ms
  control-plane base.
* **The control plane was 696.8 ms for 101 commits** — 341.8 ms of full-state
  index segments and 355.0 ms of commit records + superblock slots, i.e.
  **6.9 ms per commit** on this host (3.4 ms of index segment plus 3.5 ms of
  record and head). One group commit collapses it to 12.0 ms measured on the
  group-plus-eager-barrier row (5.9 ms index + 5.6 ms publication).

## 4. Options matrix (7 interleaved repetitions, one session)

Milliseconds. `min` is the least-contended observation, `median`/`p95` are the
observed distribution (§7 explains the tails). Footprint and exactness are
identical in all seven repetitions of a row.

| Row | min | median | p95 | CPU med | RSS med KiB | Footprint (B) | Bytes written | Requests | file/dir fsyncs | Cold restore med |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| A (native anchor) | 333.1 | 375.0 | 6,673.5 | 110 | 271,560 | 424,439,296 | — | 101 | — | 403.2 |
| B (raw, one atomic publication) | 935.9 | 1,506.1 | 3,857.4 | 240 | 272,628 | 424,608,542 | 424,621,048 | 611 | 608/310 | 538.8 |
| B-percommit (R17 form) | 2,864.8 | 2,890.3 | 46,558.8 | 270 | 272,700 | 424,784,917 | 425,207,023 | 911 | 808/808 | 547.1 |
| C-percommit (R19 form) | 2,341.6 | 2,353.3 | 2,381.8 | 1,230 | 688,684 | 282,283,011 | 282,705,117 | 516 | 413/413 | 1,434.2 |
| C-group (group, eager barriers) | 1,632.5 | 1,652.7 | 15,332.9 | 1,210 | 687,952 | 282,106,049 | 282,118,555 | 216 | 213/213 | 1,416.1 |
| **C (default: group + deferred barrier)** | **1,225.4** | **1,246.2** | **1,413.3** | **1,180** | **688,128** | **282,106,049** | **282,118,555** | **216** | **213/161** | **1,420.1** |
| C-pack64k | 1,498.0 | 1,525.1 | 13,825.6 | 1,200 | 688,724 | 282,100,120 | 282,133,206 | 171 | 171/171 | 1,466.0 |
| C-pack1m | 1,478.8 | 1,494.3 | 13,704.3 | 1,210 | 688,244 | 282,102,473 | 282,135,511 | 162 | 162/162 | 1,417.5 |
| C-pack8m | 1,678.1 | 1,698.8 | 16,441.9 | 1,230 | 688,312 | 282,115,133 | 282,214,349 | 227 | 227/227 | 1,425.2 |
| D (raw + protocol) | 951.6 | 1,833.0 | 21,322.7 | 250 | 272,744 | 424,659,735 | 424,629,900 | 611 | 608/310 | 637.5 |
| E (planned + protocol) | 1,286.3 | 1,447.6 | 12,122.7 | 1,200 | 688,104 | 282,157,242 | 282,127,407 | 216 | 213/161 | 1,437.8 |
| C-nom9 (R17 attribution) | 21,230.6 | 21,556.1 | 32,633.0 | 20,940 | 893,380 | 330,160,010 | 330,172,516 | 216 | 213/158 | 960.2 |
| C-both (R17 attribution) | 1,037.0 | 1,194.8 | 11,723.0 | 670 | 686,792 | 330,355,860 | 330,368,366 | 216 | 213/159 | 874.6 |
| C-legacy (decision-diff control) | 21,440.2 | 21,847.2 | 31,564.3 | 21,490 | 892,168 | 282,106,049 | 282,118,555 | 216 | 213/161 | 1,415.0 |

Notes on the rows:

* `C-percommit` reproduces the R19 row within session noise (R19 measured
  2,328.4 ms and 282,283,011 B; here 2,341.6 ms and the same footprint, +0.6 %),
  which is the same-revision identity proof for the "before" column.
* **Every row now publishes with the new default**, including D and E, so the
  R17/R19 rows are reproducible only in the `-percommit` form (`B-percommit`,
  `C-percommit`). `C-legacy`, `C-noz19`-style ablation suffixes and the default
  rows differ only in planner or candidate set, never in publication.
* `C-legacy` and `C-nom9` are ~21.5 s because the exhaustive planner encodes
  every candidate: it runs the level-19 ZSTD encode for 18 tensors even though
  the planner selects none of them. They are controls, not wall rows. (R20
  deliberately re-ran them so the same-run decision diff exists:
  `two_stage_matches_exhaustive: true`, 0 of 101 decisions changed,
  `two_stage_storage_unchanged: true`.)
* `C-both` (no mode-9 candidate) is the fastest planned row at 1,037.0 ms but
  costs 330,355,860 B — 48.2 MB (17.1 %) more than C. It is the R17 ablation
  row, not a candidate default: it trades the 48 MB for 188 ms.
* `B` and `D` are raw-representation rows: they publish more bytes with less
  encode, so their minima beat C while their footprints are 424 MB.

### Storage delta per option — the pair that must never be split

| Row vs C | Wall Δ | Footprint Δ | Bytes-written Δ |
|---|---:|---:|---:|
| C-percommit (before) | **+1,116.2 ms** | **+176,962 B** | +586,562 B |
| C-group | +407.1 ms | 0 B | 0 B |
| C-pack64k | +272.6 ms | −5,929 B (−0.002 %) | +14,651 B |
| C-pack1m | +253.4 ms | −3,576 B (−0.001 %) | +16,956 B |
| C-pack8m | +452.7 ms | +9,084 B | +95,794 B |

## 5. The chosen default and why

`Options.defer_group_blob_sync` defaults to **true**, and the default rows
publish one group commit with one deferred durability barrier.

* The group commit is not a trade: it is **faster and smaller** on both boards
  (−709.1 ms, −176,962 B). It also strengthens the durability statement — 101
  files become visible atomically instead of one at a time.
* The deferred barrier is a pure wall win at **identical bytes** (−407.1 ms,
  0 B) and does not weaken the ordering rule (see §6).
* The pack plane is rejected. It is 20.7–36.9 % slower and, at 64 KiB and
  1 MiB, it writes *more* bytes (pack headers + a catalog of 120–195 segment
  publications for 170–204 members); its footprint win is 3.6–5.9 KB, which is
  0.001 % of the fixture, i.e. noise next to a 253 ms loss. At 8 MiB it is
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
  asserts `fsync_file == object_blobs + manifest_blobs + commit_records +
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
not a timing claim.

## 7. Host contention, losses, and non-claims

* The host is shared. The distribution is bimodal: `C-group` has a 15,332.9 ms
  p95 against a 1,632.5 ms minimum; `B`'s p95 is 3,857.4 ms against 935.9 ms.
  `C`'s own distribution was clean this session (min 1,225.4, median 1,246.2,
  p95 1,413.3). Per the R17 contract the ledger describes the least-contended
  repetition and the medians/p95 are published unchanged. The retained
  single-repetition probe `…T113000Z` saw a 11.9 s `pack64k` *write* that the
  7-repetition row measures at 1,498.0 ms — it was a host stall, and it is why
  one sample is never cited.
* **The pack plane loses on both boards here.** 120–195 catalog segment
  publications for 170–204 members cost 397–436 ms of `manifest_commit` (the
  catalog is sealed and published inside the commit's flush), and its own
  `blob_publish` drops to 182–246 ms. Net +253 to +453 ms.
* `PackedBlobPlane` does not implement `begin_deferred_sync`
  (`CompositeBackend` returns `false`), so the pack rows keep eager per-blob
  barriers. A pack+deferred combination is therefore **not measured and not
  claimed**.
* Restore is unchanged: cold restore is 1,420.1 ms for C (was 1,434.2). R20 is
  a write-path change and makes no read claim.
* Single host, one writer, local POSIX backend, warm page cache, one fixture
  subset. No media, power-loss, fleet, cloud or S3 claim: the deferred barrier
  is a POSIX `fsync` primitive and a provider-ack backend reports its own
  durability, unchanged by this work.
* `C`'s residual is 13.1 ms (1.07 %) — the un-instrumented part is the harness
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
`bench/receipts/r20-publication-20261112T133000Z.log`.

## 9. What this implies for R21

The next bottleneck by measured share of C's 1,225.4 ms is **encode (703.2 ms,
57.4 %)**, then **blob publication (286.0 ms, 23.3 %)**, then **analyse
(191.6 ms, 15.6 % — nested in encode)**, then **the durability barrier
(75.2 ms, 6.1 %)**, then digest (66.1 ms, 5.4 %), source read + init (51.7 ms,
4.2 %), commit + index (8.4 ms, 0.7 %) and the residual (13.1 ms, 1.1 %).

Within encode, ZSTD-3 is 280.0 ms and mode-9 is 191.1 ms. R21 should target the
bounded parallel pipeline: encode is embarrassingly parallel across tensors and
blob publication is now a single sequential writeback, so the Amdahl cap is set
by the 272 MB embedding (286 ms of publication, 191 ms of mode-9 encode). No
new publication mechanism is needed — the control plane is now 0.7 % of the
write.
