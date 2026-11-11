# Checkpoint completion latency (subphase R12b)

Receipts:

| receipt | what it is |
|---|---|
| `bench/receipts/checkpoint-latency-20261111T024824Z.json` | **current**: deterministic fixture, atomic publication, 8 saves/series |
| `bench/receipts/checkpoint-latency-20261111T022638Z.json` | deterministic fixture before the atomic publication |
| `bench/receipts/checkpoint-latency-20261111T022557Z.json` | first run after the strategy-hint short-circuit (fixture not yet seeded) |
| `bench/receipts/checkpoint-latency-20261111T022409Z.json` | first run after bounding the implicit semantic search |
| `bench/receipts/checkpoint-latency-20261111T021640Z.json` | the profiler baseline (before any optimisation) |
| `bench/receipts/checkpoint-latency-20261111T021406Z.json`, `…021227Z` | earlier profiler iterations, retained |

Court: `weft/python-court:1` (`sha256:aec5deb0…76152`), Linux
`7.3.3-1-cachyos`, 3 GiB / 4 CPUs / 1024 pids / 1 GiB `/tmp`, `cap-drop=ALL`.
torch 2.15.0+cpu. Harness `python/courts/checkpoint_latency.py`, wrapper
`bench/scripts/checkpoint_latency_court.sh`. Standalone court; not part of
`verify-all`.

## 1. The fixture is the R11 fixture

`python/courts/checkpoint_latency.py` imports `build_model`, `make_optimizer`,
`data_batches` and `capture_state` from `python/courts/training_goodput.py`,
runs **one real optimizer step**, and captures the same state the R11 loop
saves: **169 entries, 4,973,360 bytes**, written with `topology=1` and the
`zstd` shard strategy. The model seed is `SEED = 20261110` (the R11 loop's
seed), so every run saves byte-identical weights: the receipt records the flat
state SHA-256 (`b2a00193…a621cee4`) and the per-save store deltas agree to the
byte (277,402 bytes for every later checkpoint). Each writer-per-save run
constructs a fresh `WeftStorageWriter` and runs
`torch.distributed.checkpoint.save`, exactly like `save_checkpoint_async` does
per checkpoint.

## 2. What was instrumented (and what was not)

The profiler is the shipped one (`src/phase_profile.rs`), extended, not a
second one:

* **Rust, in the service process.** Eleven new phases alongside the seven R10
  phases: `store_open`, `state_replay`, `plane_probe`, `index_build`,
  `index_load`, `index_attest`, `metadata_commit`, `encode_zstd`,
  `encode_zstd19`, `encode_float_tensor`, `encode_semantic`. Nested phases
  carry `nested_in` in the JSON so a reader never adds them to their parent.
  The court reads them over a new **protocol-3 `PROFILE` op** (modes 0 begin,
  1 take, 2 end), which returns only `(name, count, total_ns, max_ns)`.
* **Python, in the client.** `weftcodec.service.set_probe_sink` installs a
  callback; the adapter and the service client report
  `adapter.service_obtain`, `client.service_start`, `adapter.allocate_name`,
  `adapter.stage_total` (enclosing `adapter.resolve` and `adapter.hash`),
  `client.container_memfd`, `client.write_request`,
  `adapter.manifest_verify`, `adapter.metadata_serialize`,
  `adapter.metadata_commit` (enclosing `client.metadata_request`).
* Not instrumented because it does not exist in this path: `torch.save`/DCP
  local baselines (increment 8), fsync counts (increment 5), and any GPU work.

Profiling is off unless the court asks for it: the service pays one relaxed
atomic load per site, and the client pays one `is not None` test. A profiling
run publishes exactly the bytes a non-profiling run publishes.

## 3. The decomposition (median of 3 fresh stores and 5 later saves)

All values in ms. `same-store, later` is the median of saves 2-6 in **one**
store; `fresh store` is the median of three saves into three new stores.

| phase | fresh store | same store, first | same store, later |
|---|---:|---:|---:|
| **completion (client wall)** | **211.1** | **211.1** | **472.6** |
| `encode` (all planner candidates) | 160.8 | 162.1 | 376.0 |
| &nbsp;&nbsp;`encode_semantic` (mode 14 search) | **130.4** | **131.8** | **344.5** |
| &nbsp;&nbsp;`encode_float_tensor` (mode 9) | 22.8 | 23.2 | 23.0 |
| &nbsp;&nbsp;`encode_zstd` (level 3) | 5.9 | 5.9 | 6.8 |
| `store_open` (4 opens per save) | 12.4 | 12.8 | 44.9 |
| &nbsp;&nbsp;`state_replay` (nested) | 9.8 | 10.2 | 42.2 |
| &nbsp;&nbsp;`plane_probe` (nested) | 0.1 | 0.1 | 0.1 |
| `blob_publish` (344 publishes) | 3.5 | 3.6 | 1.6 |
| `digest` (682 sections) | 3.0 | 3.0 | 3.0 |
| `source_read` (338 sections) | 0.5 | 0.5 | 0.6 |
| `index_segment` (2 segments) | 2.3 | 2.4 | 5.3 |
| &nbsp;&nbsp;`index_build` / `index_load` / `index_attest` | 0.4 / 1.3 / 0.2 | 0.4 / 1.3 / 0.2 | 1.5 / 2.7 / 0.5 |
| `manifest_commit` | 0.4 | 0.4 | 0.4 |
| `metadata_commit` (Rust side of `WRITE_FILE`) | 1.8 | 1.9 | 3.7 |
| client `write_request` (whole `WRITE_CHECKPOINT`) | 185.7 | 186.8 | 422.2 |
| client `metadata_commit` (whole `WRITE_FILE`) | 9.3 | 9.7 | 28.0 |
| client `metadata_serialize` (WCB) | 4.3 | 4.2 | 4.3 |
| client `stage_total` (container assembly) | 2.7 | 2.5 | 2.4 |
| client `container_memfd` (alloc + copy) | 0.8 | 0.5 | 0.5 |
| client `service_obtain` (spawn + `INFO`) | 1.5 | 1.4 | 8.8 |
| client `manifest_verify` | 0.1 | 0.1 | 0.1 |
| **accounted share of wall** | **96.8 %** | **97.1 %** | **98.7 %** |

The client sections are disjoint except for three labelled enclosures
(`stage_total` ⊃ `resolve` + `hash`; `service_obtain` ⊃ `service_start`;
`metadata_commit` ⊃ `metadata_request`). The remaining 1.3-3.2 % is the
`shutil`/`tree_stats` walk the court does outside the timed save, `writer.close`
(`SHUTDOWN` + `wait`), and `dcp.save`'s own plan construction.

## 4. What the profile says

1. **The 444 ms was not the store, the commit or the index: it was the
   planner's implicit mode-14 semantic search.** `tensor_semantic_candidates`
   scans **every file object in the store state** and, for each one whose
   dtype and shape match, reads and decodes the whole tensor to price a
   semantic delta. With no explicit candidate list this is `O(objects)` per
   tensor, i.e. `O(objects × tensors)` per checkpoint. It is
   **130.4 ms of the 211.1 ms first save (61.8 %)** and
   **344.5 ms of the 472.6 ms later saves (72.9 %)**, and it is the entire
   reason saves grow within a store: each checkpoint adds 169 objects, so the
   scan gets longer while the tensor count stays the same. This is the
   "store-state bookkeeping" R11 measured and did not localise.
2. **Reopening the store costs 12.4 → 44.9 ms per save.** Four opens happen
   (`WRITE_CHECKPOINT` writable + read-only reopen, `WRITE_FILE` writable +
   read-only reopen) and each replays commits: 9.8 → 42.2 ms of `state_replay`
   for the same 4.99 MB of data. Two of those opens exist only because the
   writer is thrown away per save.
3. **The `WRITE_FILE` metadata commit is 9.3 → 28.0 ms client-visible** for a
   ~30 KB blob, because it too opens a writable store, probes both planes and
   replays state.
4. The genuine encode work is small: 22.8 ms mode-9 over 168 tensors (0.14 ms
   each) and 5.9 ms zstd. Level 19 is never offered (tensors are below the
   1 MiB threshold). Raw chunking, digesting and blob publishing together are
   ~7 ms.
5. Python staging, the memfd handoff, container hashing and the DCP metadata
   serialise are all under 5 ms combined. Increment 7's copy removal is worth
   well under 1 ms on this fixture; the profile says so before the work.

## 5. Non-claims

One host, warm page cache, CPU only, single writer, local store on tmpfs.
`encode_float_tensor`/`encode_semantic`/`encode_zstd` are nested in `encode`
and must not be added to it. The `later` column is the median of five saves in
one store, so its growth is a trend, not a per-checkpoint forecast. No
`torch.save`/DCP comparison is made here; that is increment 8.

## 6. Increment 2 — a reusable session, and the two planner changes the profile justified

### 6.1 Bounding the implicit mode-14 search (planner change)

`TensorPlanOptions::semantic_scan` is new and **off by default**. It gates only
the *implicit* candidate source: when a caller supplies no `candidates` list and
does not set `semantic_scan`, the mode-14 candidate is reported as
`skipped("no candidate list and the implicit store scan is off")` instead of
walking the whole store. An explicit `candidates` list works exactly as before
(the R7/A3 semantic-index shape), and `semantic_scan = true` restores the
historical search, now bounded by `semantic_scan_max_objects` (default 256,
ascending object id, so a truncation is a deterministic subset rather than a
race). `weftfs checkpoint write --semantic-scan` opts in; `--no-semantic`
disables the candidate entirely, as before.

This changes the *planner's candidate set* for callers that never set the
option. It is disclosed rather than hidden: the old default encoded every
same-dtype/same-shape tensor in the store for every tensor being written, which
is `O(objects)` per tensor and has no index behind it. Nothing in the fixture's
chosen representation changed: every shard is still one ZSTD frame, and the
per-checkpoint store deltas are byte-identical to the pre-change run apart from
the fixture seed (Section 1).

Effect (median of the same court, before → after):
**fresh store 211.1 → 67.9 ms; later saves in one store 472.6 → 81.5 ms.**

### 6.2 A caller-pinned strategy does not pay for the candidates it cannot choose

`checkpoint_shard_spec` pins `TensorStrategy::Zstd` (or `Raw`) for the
`zstd`/`raw` shard strategies. `choose_candidate` then returns *only* that
strategy, so encoding mode 9, mode 10, the deltas and the patch was dead work.
`prepare_candidates` now returns after building the pinned candidate when the
hint is `Raw` or `Zstd` and the tensor is not q8_0; every other hint keeps the
full candidate set, and a hint that is not applicable still fails with the same
typed error.

Effect: **fresh 67.9 → 42.6 ms; later 81.5 → 60.6 ms**; the Rust `encode`
phase fell from 29.7 ms to 5.8 ms per save (mode 9 was 22.8 ms of it).

### 6.3 `CheckpointSession` — one service, one store handle

`weftcodec.dcp.CheckpointSession` owns one `CheckpointService` across saves and
`WeftStorageWriter(session=...)` binds a writer to it without closing it. The
service now keeps **one** store handle for the connection: it starts read-only,
is upgraded to a writable handle on the first publication, and every later read
goes through it (`group` updates the handle's state and head in place), so the
four opens per save become one.

Ownership rules, implemented:

* The writable handle is the only holder of the store writer lock. It is
  released after `--session-idle-secs` without a publication (default 30 s);
  `0` releases it immediately after every publication, which is the
  pre-session behaviour. The release happens on a client request, never on a
  timer thread.
* A writable open re-proves both plane capabilities, so a session handle is as
  validated as a per-request one. A stale fence is never replaced: the service
  returns the typed `Busy`/`Unauthorized` and the caller must reopen.
* A session refuses `service()` in a process that did not create it
  (`fork()` safety), and `CheckpointService.close()`/`_call()` refuse in a
  child so a child can never `SHUTDOWN` the parent's helper.
* `session.save(...)` retries a **transport** failure at most once and only
  when the store proves nothing was published under the name the writer
  allocated. When a lost response *did* land, it raises an explicit
  "published but not finished" error instead of publishing a second
  generation, so one `(checkpoint_id, container)` never yields two checkpoints.

Effect at 8 saves in one store: **first 38.5 ms, median 42.2 ms, last 46.1 ms**
(one 93.0 ms host-scheduling outlier is in the raw row list and is not
hidden). The writer-per-save series grows monotonically,
40.0 → 44.8 → 50.4 → 56.4 → 64.0 → 71.2 → 79.5 → 85.7 ms, because each save
replays the store from scratch twice.

### 6.4 The session's decomposition (median of 8 saves)

| phase | ms | note |
|---|---:|---|
| **completion** | **42.2** | first 38.5, last 46.1 |
| client `write_request` (whole `WRITE_CHECKPOINT`) | 23.4 | |
| → `index_segment` (2 segments) | 5.2 | `index_build` 1.6, `index_load` 2.6, `index_attest` 0.5 |
| → `encode_zstd` (169 frames) | 5.8 | the only real encode left |
| → `metadata_commit` (the Rust `WRITE_FILE` commit) | 3.8 | a second commit, one atomic publication would remove it |
| → `digest` (682 sections) | 3.0 | |
| → `blob_publish` (344 publishes) | 1.6 | |
| → `manifest_commit` | 0.4 | one commit per save |
| → descriptor/chunking/state apply | ~3.6 | residual inside the round trip |
| → `store_open` | **0.0** | the session handle is reused (0 opens) |
| client `metadata_serialize` (WCB) | 4.2 | pure Python |
| client staging (`resolve` + `hash` + append) | 2.2 | |
| client `container_memfd` (alloc + copy) | 0.4 | |
| client `allocate_name`, `manifest_verify` | 0.1 | |
| residual (DCP plan, writer construction, drain) | ~6 | |

Commit cost does not grow with history: `manifest_commit` is 0.43-0.45 ms at
every index, and the whole later-save delta is 277,402 bytes at every index.
The only history-dependent term left inside a session is the index chain
(`index_load` 2.4 ms), which is increment 4's target.

## 7. Milestones

| milestone | p50 | receipt |
|---|---:|---|
| R11 baseline | 444.1 ms | `training-goodput-20261110T200930Z` |
| profiled, no optimisation | 472.6 ms later / 211.1 fresh | `…T021640Z` |
| 100 ms | 81.5 ms later / 67.9 fresh | `…T022409Z` |
| 50 ms | 42.2 ms (session) | `…T022638Z` |
| 50 ms confirmed | 37.7 ms (session, one atomic commit) | `…T024824Z` |
| 20 ms | not reached yet | — |

## 8. Non-claims (increments 1-2)

One host, warm page cache, CPU only, single writer, local store on tmpfs; the
court's work directory is inside the container. The 30 s session idle window
means another process cannot open the store for writing between checkspoints
that are closer than 30 s apart — that is the documented trade-off, not an
indefinite lock. `semantic_scan` default-off is a planner candidate-set change,
disclosed above; the chosen representation on this fixture is unchanged. No
`torch.save`/DCP comparison, no packing, no index change and no Python copy
removal is claimed yet.

## 9. Increment 3 — prepare / finish with one atomic publication

`StorageWriter` already had the right seam: `write_data()` then `finish()`.
The adapter committed the tensors in `write_data` and the DCP metadata in
`finish`, so a failure between them left a checkpoint directory whose tensors
existed and whose `dcp.metadata` did not.

The store now supports a deferred group:

* `CheckpointWriteOptions::defer_commit` runs the whole checkpoint body inside
  an explicitly opened group (`Store::begin_group`) and returns **without**
  publishing. Every blob is immutable and create-once; the state ops are
  applied in memory only.
* `Store::commit_deferred_checkpoint_metadata(path, bytes)` adds one metadata
  file to that group and publishes exactly **one** commit;
  `Store::abort_deferred_checkpoint()` discards it.
* Protocol 3 gains `PREPARE_CHECKPOINT` (0x0c), `FINISH_CHECKPOINT` (0x0d) and
  `ABORT_CHECKPOINT` (0x0e). One deferred checkpoint per connection; a second
  `PREPARE` or a plain `WRITE_CHECKPOINT` while one is open is
  `InvalidArgument`; the metadata path must be under `/checkpoints/<name>/`;
  `SHUTDOWN` aborts an open group; the idle-release path never drops a handle
  with an open group.
* `WeftStorageWriter(atomic_publish=True)` (the new default) prepares in
  `write_data` and finishes in `finish`. `atomic_publish=False` reproduces the
  historical two-commit shape for comparison. `close()`/`reset()` abort an
  unfinished prepare, so a failed save publishes nothing.

Readers see either the previous complete artifact or the new one. `weftfs
checkpoint verify` and `verify --full` are clean on both sides.

**Crash test** (`tests/checkpoint_atomic.rs`, 6/6) uses the process-fault proxy
(`WEFT_CRASH_PHASE`) in a child process:

| scenario | result |
|---|---|
| prepare, never finish | generation unchanged, no checkpoint, no metadata, verify clean — **the old window no longer exists** |
| prepare + finish | exactly **+1** generation, checkpoint and `dcp.metadata` both visible there, verify --full clean |
| crash at `after_objects` (during the final publication) | old *or* new whole state; no name and no metadata, or both; only an unpublished-tail note from `verify` |
| crash at `after_commit_file` (after the commit fsync, before the superblock slot) | same old-or-new result; the fsynced-but-unpublished commit file is reported as a tail, never as state damage |
| historical two-commit shape, same bytes | **+2** generations (tensors, then metadata) — the window this change removes |

The metadata blob is byte-identical between the two shapes (content-addressed);
only the commit history differs.

### Measured effect

Same session, same fixture, same 8 saves:

| | p50 | first | last |
|---|---:|---:|---:|
| two commits (historical) | 40.7 ms | 40.1 | 45.1 (one 92.7 outlier) |
| **one atomic commit** | **37.7 ms** | 37.2 | 39.6 |

The saving is exactly the second commit and its index work: Rust
`metadata_commit` 4.3 ms → 0 (folded into the single commit),
`index_segment` 5.6 → 3.7 ms (one segment per save instead of two), and
`index_build`/`index_load` 1.7/2.8 → 0.9/1.9 ms. That is ~3 ms of completion and
one less generation per checkpoint.

Honest cost: `finish()` is now the durability point, so a caller that never
calls `finish` publishes nothing (previously the tensors were already durable).
That is the intended semantics and it is the repair, but it is a behaviour
change for any caller that used `write_data` without `finish`.

## 10. Increment 5 — physical aggregation for small frames (measured, mostly a loss)

Receipt `bench/receipts/checkpoint-latency-20261111T034200Z.json` (6 saves per
series, 2 fresh stores, three pack configurations). The court gained a
frame-size histogram of the files a save *creates*, per-save backend writes /
bytes / fsync counts (read from the new `BackendStats::fsyncs`, which counts
`sync_all` + directory `fsync` calls), and a restore pass that decodes every
tensor through the same session.

### The frame-size distribution comes first

First save of the 4.97 MB / 169-entry checkpoint, unpacked:

| size bucket | files |
|---|---:|
| ≤ 512 B | 147 |
| 513 B – 4 KiB | 67 |
| 4 – 16 KiB | 1 |
| 16 – 64 KiB | 16 |
| 64 – 256 KiB | 34 |
| 256 KiB – 1 MiB | 0 |
| > 1 MiB | 0 |
| **total** | **265 files, 4,890,427 B, 528 fsyncs** |

A *later* save of the same state writes **7 files, 276,794 B, 14 fsyncs**: the
tensor frames are content-addressed and already present, so only the new
checkpoint directory, manifest file, commit, index segment and DCP metadata are
new. That is the workload R10's packing policy was not designed for — the small
frames are commit/index/access records, not tensor data, and the tensor data is
deduplicated across saves.

### Deferred packing, three thresholds

| configuration | first-save files | first-save fsyncs | first-save bytes | later-save files | later-save fsyncs | completion p50 (later) |
|---|---:|---:|---:|---:|---:|---:|
| unpacked | 265 | 528 | 4,890,427 | 7 | 14 | **39.3 ms** |
| pack 1 MiB / 64 KiB | 189 | 380 | 4,869,849 | 8 | 20 | 39.9 ms |
| pack 4 MiB / 256 KiB | 184 | 372 | 4,878,151 | 8 | 20 | 42.5 ms |
| pack 16 MiB / 1 MiB | 165 | 332 | 4,874,010 | 7 | 18 | 41.6 ms |

Packing **does** what it says on the first save: 76–100 fewer files and 148–196
fewer durability barriers, with slightly fewer bytes. It does **not** reduce
completion: the first save went 47.0 → 40.0/42.3/43.5 ms on one sample per
configuration, which is inside this court's run-to-run spread (the unpacked
fresh rows were 47.0 and 100.0 ms). And it **loses** on the steady state that
matters for training: +2 to +6 fsyncs per save for the pack catalog root and
segment, +0.5–1.6 KB, and no file reduction (7 → 7/8).

Restore is unchanged and exact: all 169 tensors restore through the packed
store with the same digest (`b2a00193…a621cee4`), 147.3 ms unpacked versus
147.0–154.2 ms packed, largest tensor 1.09 vs 1.09–1.36 ms.

**Conclusion: the R10 packing policy does not transfer to checkpoint
sessions.** It should stay off for this workload (`CheckpointSession` does not
enable it by default), and the R11 recommendation to pack a large
single-checkpoint ingest still stands on its own evidence. Losses recorded:
no completion win, extra fsyncs and bytes in the steady state, a packed store
is only readable through the packed plane (so a plain `weftfs checkpoint
verify` on the CLI cannot see it — exactness here was proven by restoring every
tensor through the session, which is why `verify --full` is not cited for the
packed rows), and the 304 MB peak RSS is the Python process (torch); the
service's own RSS is not measured by this court.

## 11. Increment 6 — write-policy comparison on the same fixture

Receipt `bench/receipts/checkpoint-latency-20261111T035900Z.json` (3 saves per
policy in one store, same session, same fixture, no packing). The policy is
`CheckpointWriteOptions::write_policy`, now plumbed through
`WeftStorageWriter(write_policy=..., fast_codec=..., fast_zstd_level=...)`.

| policy | completion p50 | store bytes | restore (all 169) | digest |
|---|---:|---:|---:|---|
| `planned` (level-3 ZSTD per shard) | 41.2 ms | 5,453,026 | 144.1 ms | `b2a00193…` |
| `fast` + raw | **36.0 ms** | 5,912,028 (+8.4 %) | 148.7 ms | `b2a00193…` |
| `fast` + zstd level 1 | 42.3 ms | 5,453,389 | 147.1 ms | `b2a00193…` |
| `fast` + zstd-1, then transcode | 39.2 ms (+584 ms later) | 5,453,389 → 12,577,339 | 148.5 → 154.2 ms | `b2a00193…` |

* **Raw is the only fast representation that actually wins here**: 36.0 ms,
  5.2 ms (12.6 %) faster than planned, for 8.4 % more physical bytes. The
  tensors are ~29 KB each, so raw needs no encoder at all.
* **zstd-1 is not a win over level 3 on this data** (42.3 vs 41.2 ms; the walls
  were 42.3/40.1/45.5 vs 40.0/41.2/123.4, so this is a tie inside the noise).
  Level 3 on 29 KB inputs is already cheap; the fast path additionally builds a
  raw candidate that the planned path does not.
* **Deferred transcode costs 584 ms and grows the store** from 5.45 MB to
  12.58 MB, because the pass publishes a new version and the old one is
  retained. Restore is unchanged (same digest). It is a storage optimisation
  run later, not a latency win, and it must be budgeted accordingly.
* **Acknowledgement always follows a durable representation.** In `fast` the
  raw or zstd-1 frame is published and committed before the save returns, so
  exactly one valid representation exists at ack time; the transcode is a
  separate, explicit later pass with `--min-saved-percent`. No variant is
  acknowledged before its frame is durable, and every variant restores the same
  digest.

Transient storage growth: raw adds 458,962 bytes over four saves (8.4 %);
transcode adds 7,123,950 bytes (the retained old versions), which GC reclaims
once the new versions are no longer pinned by a snapshot.

## 12. Increment 7 — the container is built in the memfd, not in a bytearray

Receipt `bench/receipts/checkpoint-latency-20261111T041200Z.json`
(`--policy staged-container --policy direct-container`, 8 saves each).

Before: `_execute` assembled a `bytearray` (header + every payload) and then
`write_checkpoint` copied that bytearray into a memfd — **two full-buffer
copies** and a ~5 MB staging buffer in the Python process. Now the adapter
allocates a memfd of the exact final size, maps it writable, writes the header
and copies each payload **directly into the mapping** (`ctypes.memmove`, no
intermediate `bytes`), hashes each payload from the mapping, and hands the
descriptor to the service (`container_fd=`). One copy per payload; no
container-sized Python buffer.

| | container copies | staging buffer | `stage_total` | completion p50 | restored digest |
|---|---:|---:|---:|---:|---|
| staged bytearray (old) | 2 | 4,989,309 B | 2.48 ms | 40.2 ms | `b2a00193…` |
| direct mapping (new) | **1** | **0 B** | 3.06 ms | 40.1 ms | `b2a00193…` |

**The copy count fell from 2 to 1 and the staging buffer disappeared; the
completion time did not move.** `stage_total` is 0.6 ms *worse*: a per-tensor
`ctypes.memmove` into a mapped page costs slightly more than appending to a
bytearray and then writing the whole thing once. That is the honest result —
this change buys bounded memory (one fewer container-sized copy, which matters
for a large checkpoint) rather than latency on a 5 MB fixture.

Exactness: every tensor's restored bytes hash to the same digest from both
paths, `container_bytes` is identical, and
`python/tests/test_dcp.py::test_direct_container_matches_the_staged_path`
asserts `container_copies == 1`, a zero staging buffer, identical container
size and identical per-tensor restored hashes.

Duplicate hashing: the adapter still computes its own content root per tensor,
but it now reads the bytes **from the mapping** instead of a second payload
object, so one buffer serves the copy and the hash. The Python root is
deliberately kept: it is the independent check against the Rust manifest
(`_verify_report`), and removing it would remove a proof rather than a copy.

## 13. Where R12b landed

p50 completion of the R11 fixture, per increment (same fixture, same court
unless noted):

| increment | p50 | receipt |
|---|---:|---|
| R11 baseline | 444.1 ms | `training-goodput-20261110T200930Z` |
| 1 — profiler, no optimisation | 472.6 (later) / 211.1 (fresh) | `checkpoint-latency-20261111T021640Z` |
| 2a — implicit semantic search opt-in | 81.5 / 67.9 | `…T022409Z` |
| 2b — pinned-strategy shortcut | 60.6 / 42.6 | `…T022557Z` |
| 2c — `CheckpointSession` | **42.2** | `…T022638Z` |
| 3 — one atomic commit | **37.7** | `…T024824Z` |
| 4 — index chain kept in memory | 40.6 (fixture too small; the 1,250-commit court is the evidence) | `…T030522Z` |
| 5 — packing measured, not adopted | 39.3 (unpacked) | `…T034200Z` |
| 6 — fast policy | **36.0** (raw) / 41.2 (planned) | `…T035900Z` |
| 7 — container built in the memfd | 40.1 | `…T041200Z` |

The honest headline: **444.1 → 37.7 ms p50 (11.8x) with the planned policy,
36.0 ms with raw frames**, and the training-goodput court's own completion
(75.6 ms async on a contended 4-vCPU box) is 17-18x the equivalent-durability
`torch.save` baseline.

### Why 20 ms was not reached, with the measured shares

The remaining 36-40 ms is:
`WRITE_CHECKPOINT` round trip 21-23 ms (of which ZSTD encode 5.7, digest 3.0,
blob publish 1.6, index segment 3.7, commit 0.45, and ~6 ms of chunking,
manifest construction and state application), Python DCP-metadata WCB
serialisation 4.2 ms, the `FINISH_CHECKPOINT` commit 4.0 ms, Python staging
2.2 + hashing 0.85 + memfd 0.35 ms, and ~6 ms of DCP plan/writer overhead
outside every instrumented section.

Each candidate for the last 16-20 ms is either unimplemented or measured to be
insufficient on its own:

1. **Row-level incremental index** (`StateIndex::apply_ops`): would remove
   `index_build` (0.9-1.6 ms) and most of `index_segment` (3.3-5.2 ms) at this
   state size. Not implemented; the chain cache removes only the re-read.
2. **Python metadata serialisation** (4.2 ms for 169 entries): a pure-Python
   WCB encoder. Not optimised in R12b.
3. **One round trip instead of two**: DCP only produces its metadata after
   `write_data`, so `finish` needs a second call. A caller that can supply the
   metadata up front could use one; the adapter cannot.
4. **Fewer durability barriers / smaller frames**: packing was measured and did
   not help the steady state (Section 10).
5. **Parallel per-tensor hashing/encoding**: the service is single-request and
   single-threaded per connection; a bounded worker pool inside
   `WRITE_CHECKPOINT` is not implemented.

### What was verified, and what was not

Verified: every increment's numbers are from a court run at a named revision;
the atomic-publication crash test (6/6); the index-chain court and its oracle
test; exactness at every step (40/40 and 80/80 restored states, identical
digests across policies); the kill court 5/5 old-or-new; Rust 758/0 and FUSE
801/0/2; Python 368 passed / 70 skipped; claims 882; spec drift 60/60;
conformance manifest 420.

Not verified / not claimed: 20 ms; a row-level incremental index; a
multi-writer or fleet training claim; GPU; physical power loss; a production
queueing/skip policy for intervals shorter than completion (the wait is
measured, not solved); Go parity for any R12b change (all of it is Rust and
Python).
