# Checkpoint completion latency (subphase R12b)

Receipts (this file, increment 1 = profiler only):

| receipt | what it is |
|---|---|
| `bench/receipts/checkpoint-latency-20261111T021640Z.json` | R11 fixture, profiler complete (18 Rust phases, 12 client sections), fresh-store and same-store saves |
| `bench/receipts/checkpoint-latency-20261111T021406Z.json` | same court before the semantic phase was separated; retained |
| `bench/receipts/checkpoint-latency-20261111T021227Z.json` | same court before the Rust profile was read correctly; retained |

Court: `weft/python-court:1` (`sha256:aec5deb0…76152`), Linux
`7.3.3-1-cachyos`, 3 GiB / 4 CPUs / 1024 pids / 1 GiB `/tmp`, `cap-drop=ALL`.
torch 2.15.0+cpu. Harness `python/courts/checkpoint_latency.py`, wrapper
`bench/scripts/checkpoint_latency_court.sh`. Standalone court; not part of
`verify-all`.

## 1. The fixture is the R11 fixture

`python/courts/checkpoint_latency.py` imports `build_model`, `make_optimizer`,
`data_batches` and `capture_state` from `python/courts/training_goodput.py`,
runs **one real optimizer step**, and captures the same state the R11 loop
saves: **169 entries, 4,973,352 bytes**, written with `topology=1` and the
`zstd` shard strategy. Each save constructs a fresh `WeftStorageWriter` and
runs `torch.distributed.checkpoint.save`, exactly like
`save_checkpoint_async` does per checkpoint. Nothing about the fixture is
re-implemented, so the numbers are comparable to R11's 444.1 ms p50.

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
