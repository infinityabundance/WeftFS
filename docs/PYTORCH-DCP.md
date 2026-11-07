# PyTorch Distributed Checkpoint adapter (Q5, updated in S3)

Status: **implemented and measured** on `staging`. Python court evidence:
`bench/receipts/python-bindings-20261106T000719Z.json` (Q5) and the S3 Python
court receipt (below). Bench receipts:
`bench/receipts/pytorch-dcp-20261106T000734Z.json` (Q5, before) and
`bench/receipts/pytorch-dcp-20261107T063540Z.json` (S3, after).

**S3 change:** save and load no longer start the `weftfs` CLI and no longer
stage a host safetensors file. They use one persistent
`weftfs checkpoint-service` process per adapter object
(`weftcodec.service.CheckpointService`), restore straight into caller-owned
memfds, and never import into the store. The protocol is normative in
`docs/CHECKPOINT-SERVICE.md`; the measured comparison is in
`bench/RESULTS-checkpoint-service.md`.

Real PyTorch ran in the court: **torch 2.15.0+cpu** (official
`download.pytorch.org/whl/cpu` wheel, 209.5 MB as downloaded, baked into
`docker/python-court.Dockerfile`). There is no CUDA runtime in the image, no
GPU was present, and no GPU claim is made. The Q5 wheel-built Python court ran
**191 tests** with 0 failures (62 skipped); the S3 court is reported below.

## 1. What it is

`weftcodec.dcp` (Python) implements the documented shape of
`torch.distributed.checkpoint.StorageWriter` / `StorageReader` over a WeftFS
store. The training process never mounts anything: durable transfers go through
one long-lived helper process over an `AF_UNIX` socket, and tensor payloads
cross as `SCM_RIGHTS` memfds.

```
torch.distributed.checkpoint.save/load
        │  StorageWriter / StorageReader
        ▼
weftcodec.dcp  (host memory, one tensor at a time)
        │  weftcodec.service.CheckpointService  (one process, memfd payloads)
        ▼
weftfs checkpoint-service
        │  the same Store::write_checkpoint / restore_tensor paths the CLI uses
        ▼
WeftFS store
  /checkpoints/<name>/manifest          weft.checkpoint.v1 (canonical WCB)
  /checkpoints/<name>/tNNNNN.sNNN       mode-10 shard or exact zstd/raw shard
  /checkpoints/<name>/dcp.metadata      weft.dcp.metadata.v1 (canonical WCB)
```

The Rust CLI remains the oracle in tests (`checkpoint verify`, `verify --full`,
`checkpoint show`) and still initialises a store (`init`). It is no longer in
the save or load path: both report `store_command_calls: 0`.

Two independent descriptions exist on purpose:

* the **checkpoint manifest** is the byte authority: tensor names, dtypes,
  shapes, per-shard object ids, per-tensor and per-shard logical roots, and
  content roots. The Rust CLI writes it and verifies it.
* the **DCP metadata** records the DCP protocol view (flat state-dict keys,
  storage keys, chunk offsets, dtypes, planner data) so `load()` can rebuild the
  `Metadata` object PyTorch expects. It is verified against the manifest before
  any data is read: an entry whose content root, dtype or shape disagrees is
  `ExactnessError`.

A checkpoint written by the Rust CLI alone (plain safetensors tensor names) is
also readable: `read_metadata()` synthesises the DCP metadata from the manifest
and reports `reader.metadata_source == "manifest"`. Adapter-written checkpoints
report `"dcp"`.

## 2. API surface

```python
from weftcodec.dcp import (
    WeftStorageWriter, WeftStorageReader, save_checkpoint_async,
    AsyncSaveHandle, init_store, verify_checkpoint, StorageInfo, RestoredEntry,
    DcpError, StoreUnavailable, StoreCommandError, InvalidCheckpointId,
    UnsupportedDtype, UnsupportedStateObject, StagingLimitExceeded,
    MetadataMissing, InvalidMetadata, ExactnessError, SaveFailed,
    TORCH_AVAILABLE, TORCH_VERSION,
)
```

### `WeftStorageWriter(path, *, store_root, topology=1, axis=None, shard_strategy="zstd", staging_bytes=256 MiB, overwrite=True, init_store=False, keep_staging=False, weftfs_cli=None, timeout=1800)`

| Member | Behaviour |
|---|---|
| `reset(checkpoint_id=None)` | Starts a new save; an explicit id replaces `path`. |
| `set_up_storage_writer(is_coordinator, *args, **kwargs)` | Records the role; PyTorch 2.15 passes `rank`/`use_collectives`. |
| `prepare_local_plan(plan)` / `prepare_global_plan(plans)` | Pass-through (no storage-side repartitioning). |
| `storage_meta()` | Returns `StorageMeta(checkpoint_id=..., save_id=<uuid>)`. |
| `write_metadata(metadata)` | Interface parity; **torch 2.15 does not call it** (metadata is persisted in `finish`). |
| `write_data(plan, planner)` | Resolves each item, writes a staging safetensors container, runs `weftfs checkpoint write`, verifies the manifest, returns `torch.futures.Future[list[WriteResult]]`. |
| `finish(metadata, results)` | Fills `metadata.storage_data`, serialises canonical WCB and publishes `/checkpoints/<name>/dcp.metadata`. |
| `checkpoint_id`, `store_name`, `stats` | Requested id, resolved store name, counters (entries, bytes, staging peak, CLI ms). |
| `validate_checkpoint_id(id)` | True for a non-empty single-segment name. |

`topology` is `n` or `(n, axis)`. It is handed to the Rust CLI as
`--shards n --axis A`, which splits every tensor on that axis. A non-divisible
extent is rejected by the Rust CLI with "no silent padding"; the adapter never
pads, guesses another axis, or silently stores whole tensors. `topology=1`
stores whole tensors. `BYTE_IO` state objects are stored as `u8[len]` tensors,
so a split topology needs their serialized length divisible by the shard
count; use `topology=1` for state dicts with arbitrary-length byte objects.

`shard_strategy` is `zstd` (default, exact, any shard length), `raw` (exact), or
`tensor-sharded` (mode-10 q8_0 byte-word transport, exact; every shard's byte
length must be divisible by 16). `tensor-tiled` is rejected by this adapter
because it needs a tile shape the DCP plan does not carry.

`staging_bytes` is a hard bound on one resolved tensor copy staged for the
container (and on the sum of cached `BYTE_IO` payloads). A larger payload is
`StagingLimitExceeded`; it is never truncated.

### `WeftStorageReader(path, *, store_root, weftfs_cli=None, timeout=1800, allow_manifest_synthesis=True)`

| Member | Behaviour |
|---|---|
| `reset(checkpoint_id=None)` | Starts a new read. |
| `read_metadata(*args, **kwargs)` | Resolves the newest storage generation, reads/validates the manifest, then loads `dcp.metadata` or synthesises metadata from the manifest. |
| `set_up_storage_reader(metadata, is_coordinator, *args, **kwargs)` | Installs the storage-key map. |
| `prepare_local_plan(plan)` / `prepare_global_plan(plans)` | Pass-through. |
| `read_data(plan, planner)` | Restores one container (full plan) or one container per requested entry, verifies every content root, narrows/copies tensors or hands `BYTE_IO` bytes to `planner.load_bytes`. |
| `restore_entry(entry, *, topology=None, axis=0)` | Restores one manifest entry through `weftfs checkpoint restore --tensor`, optionally with `--topology N --axis A`, returns `RestoredEntry` and verifies its bytes. |
| `metadata_source`, `storage_name`, `stats` | `"dcp"` or `"manifest"`, resolved name, restore counters. |

`validate_checkpoint_id` mirrors the writer.

### Save generations

The store has no recursive directory delete, so the adapter models a second
save to the same id as a new generation: `run-1`, `run-1.v2`, `run-1.v3`, ...
The reader selects the highest generation (an unsuffixed checkpoint counts as
generation 1, so a Rust-written checkpoint is found normally).
`overwrite=False` raises `FileExistsError` instead. Nothing is deleted or
rewritten in place.

### Async save

```python
handle = save_checkpoint_async(
    state_dict,
    checkpoint_id="run-2",
    store_root="/data/weft-store",
    topology=8,
    shard_strategy="zstd",
    staging_bytes=256 * 1024 * 1024,
)
metadata = handle.result()   # raises the save error on the caller's thread
```

`save_checkpoint_async` runs `torch.distributed.checkpoint.save` on a
`ThreadPoolExecutor` worker (1-4 workers; default 1). In the bench the call
returned in **0.72 ms** (median) while completion took **1,329.0 ms**, so the
training thread is free for the duration. This is a host-memory staging path:
every tensor is copied to CPU memory one at a time under the `staging_bytes`
bound. It is **not a GPU-direct path**, and it does not overlap the Rust CLI
with tensor copies (the worker is serial by default).

## 3. Metadata record (`weft.dcp.metadata.v1`)

Canonical WCB map, stored as an exact store file object:

```
{
  "schema":        text = "weft.dcp.metadata.v1",
  "checkpoint_id": text,          // requested DCP id
  "storage_name":  text,          // resolved store checkpoint name
  "dcp_version":   text | null,   // "1.0.0"
  "state_dict": [
    {"fqn": text, "kind": "tensor", "dtype": text, "size": [u64],
     "chunks": [{"offsets": [u64], "sizes": [u64]}]}
    | {"fqn": text, "kind": "bytes"}
  ],
  "storage_data": [
    {"fqn": text, "offset": [u64] | null, "index": u64 | null,
     "info": {"entry": text, "dtype": text, "shape": [u64],
              "storage_offsets": [u64], "lengths": [u64],
              "content_root": bytes32}}
  ],
  "planner_data": {"kind": "none"}
                | {"kind": "dict_str_tuple", "entries": [{"key": text, "value": [text]}]}
}
```

`planner_data` is persisted only when it is `None` or the default planner's
`dict[str, tuple[str, ...]]` mappings. Any other planner data is rejected
(`UnsupportedStateObject`) rather than dropped silently. `ShardedTensor`,
`DTensor` and other stateful objects are likewise rejected; this adapter
persists plain tensors and `BYTE_IO` objects.

Only the exact WEFT-TENSOR v1 dtypes are accepted: `f16`, `bf16`, `f32`, `f64`,
`i8`, `u8`, `i16`, `i32`, `i64` (torch `float16`, `bfloat16`, `float32`,
`float64`, `int8`, `uint8`, `int16`, `int32`, `int64`). `bool`, complex and
float8 tensors fail closed (`UnsupportedDtype`); there is no coercion.

## 4. Exactness and failure semantics

| Failure | Result |
|---|---|
| entry bytes differ from the written content root | `ExactnessError` before publication (writer) / before copy (reader) |
| manifest content root, dtype or shape disagrees with DCP metadata | `ExactnessError` in `read_metadata` |
| restored container shape/dtype/root disagrees | `ExactnessError` |
| non-divisible topology | Rust `NonCanonical` → `StoreCommandError`; nothing published |
| unsupported dtype or state object | `UnsupportedDtype` / `UnsupportedStateObject`; nothing published |
| missing CLI / uninitialised store | `StoreUnavailable` |
| missing metadata and synthesis disabled | `MetadataMissing` |
| staged payload above the bound | `StagingLimitExceeded` |
| `checkpoint write` succeeds but `dcp.metadata` publication fails | `SaveFailed` naming the orphaned checkpoint |

Publication of the tensor checkpoint is one Rust `Store::group` commit: a
failed split or write publishes nothing. Metadata publication is a second
commit (it depends on the write results), so a process failure between the two
leaves a readable tensor checkpoint without DCP metadata. For plain tensors the
reader can still synthesise metadata from the manifest; for sharded or byte
entries it fails closed.

Reads are read-only in the store by default: `weftfs checkpoint restore`
without `--import` decodes the checkpoint and writes only the host file. The
adapter's `import_restored=True` (off by default) adds `--import`, which
publishes an exact copy under `/dcp-reads/<uuid>/` in one commit; that copy can
be collected by GC once unreferenced. The import is a property of the CLI verb,
not of the adapter, and is never implied by a restore.

Verification performed by the tests and the bench:

```sh
weftfs --store <store> checkpoint verify --name <name> --json   # 8 tensors
weftfs --store <store> verify --full --json                     # ok, no problems
```

## 5. Topology matrix (test evidence)

`python/tests/test_dcp.py::test_topology_matrix_8_4_1_and_exactness` writes a
model whose tensors have axis-0 extents divisible by 8 (`16x8` weight, `16`
bias, `8x4` nested weight), stores them with `topology=8` (manifest shows 8
shards per tensor), loads into a fresh module, and then calls
`restore_entry(name, topology=8|4|1)` for every tensor. All 3 tensors × 3
topologies returned byte-identical payloads; the reassembled content root was
the same for every topology; the Rust `checkpoint verify` and `verify --full`
both passed. The underlying CLI form is
`weftfs checkpoint restore --tensor NAME --topology 8|4|1 --axis 0`. The module
reports 10 passed / 0 failed / 0 skipped in the Python court. `test_real_dcp_roundtrip_and_rust_verification` covers
`topology=1`; `test_rust_written_checkpoint_is_read_by_the_adapter` covers a
2-way Rust-only write read back through synthesised metadata.

## 6. Bench (8 tensors x 4 MiB, CPU)

### S3 (after): service-backed, same fixture and court

Receipt `bench/receipts/pytorch-dcp-20261107T063540Z.json`. Same fixture
(synthetic f32, 8 x 4 MiB, `topology=8`, `zstd`, `staging_bytes=64 MiB`).

| Metric | S3 | Q5 (before) | same-revision CLI control |
|---|---:|---:|---:|
| Full save (median of 3) | 1,498.649 ms | 1,223.642 ms | 1,521.614 ms |
| Full load (median of 3) | **101.006 ms** | 152.208 ms | 106.475 ms |
| Single-tensor load (median of 5) | **24.135 ms** | 67.171 ms | 22.362 ms |
| Async save submit | 1.55 ms | 0.72 ms | – |
| Async save completion | 1,621.1 ms | 1,329.0 ms | – |
| WeftFS physical bytes after first save | 31,187,381 B | 31,187,325 B | – |
| `torch.save` baseline | 33,557,979 B (ratio **0.9294**) | same | – |
| Store growth from restores | **0 B** | 0 B | – |
| CLI subprocess calls in save/load | **0** | 2+ per operation | 1 per operation |

The service path is 1.5 % faster than the CLI path it replaces for save and
5 % faster for a full load. Against the Q5 receipt, load is 33.6 % faster and
single-tensor load 64.1 % faster; save is 22.5 % slower, and the whole of that
difference is in the Rust checkpoint write (1,344 ms of service `request_ms`
on a 32 MiB container), not in the boundary — the same-revision CLI control is
slower still.

Losses: single-tensor load is 7.8 % slower than the CLI control because the
control excludes `dcp.load` metadata/framework work; async submit is 0.8 ms
slower (one service round trip instead of an immediate thread start); a full
save is still ~160x a single `torch.save` file on this fixture.

### Q5 (before, retained)

Receipt `bench/receipts/pytorch-dcp-20261106T000734Z.json` (revision
`d732a96`). Court: `weft/python-court:1`, kernel `7.3.1-1-cachyos`, 3 GiB
memory (swap 3 GiB), 4 CPUs, 1024 pids, bounded 1 GiB `/tmp`,
`cap-drop=ALL`, `no-new-privileges`. Fixture: synthetic `torch.randn` f32,
8 x 4 MiB, `topology=8`, `zstd` shards, `staging_bytes=64 MiB`.

| Metric | Value |
|---|---|
| Full save (median of 3) | **1,223.6 ms** (1,038.7-1,229.5) |
| Full load (median of 3) | **152.2 ms** (148.3-156.7) |
| Single-tensor load (median of 5) | **67.2 ms** (64.3-70.0) |
| Synchronous save (median of 3) | **1,316.5 ms** |
| Async save submit (median of 3) | **0.72 ms** |
| Async save completion (median of 3) | **1,329.0 ms** |
| Staging peak | **4,194,304 B** (one 4 MiB tensor) |
| WeftFS physical bytes after first save | **31,187,325 B** |
| `torch.save` baseline file | **33,557,979 B** (ratio **0.9294**) |
| Mode-10 `tensor-sharded` save | **913.3 ms**, **32,826,050 B** incremental |
| `torch.save` / `torch.load` baseline | **8.7 ms** / **5.0 ms** |
| Peak RSS (VmHWM) | **381,424 KiB** |
| Rust verification | 8 tensors verified, `verify --full` ok, no problems |

Honest reading: WeftFS is ~140x slower to save and ~30x slower to load than a
single `torch.save` file on this fixture, while writing 7% fewer physical bytes
because of zstd shards and commit/manifest sharing. Single-tensor load is 2.3x
faster than a full load because DCP only requests one entry and the reader
restores one container. The async submit/completion gap is real (the call
returns in under a millisecond), but total completion is the same as the
synchronous path; the current implementation does not overlap CLI work with
the next tensor copy.

## 7. Non-claims

* Not a GPU-direct or zero-copy path; tensors are copied through host memory.
* No multi-rank distributed training or cluster test was run. All tests call
  `no_dist=True` in one process. The storage interfaces are per-rank by shape,
  but cross-rank collective behaviour is unverified.
* No stock PyTorch DCP storage plugin was used: this is an independent
  implementation of the documented `StorageWriter`/`StorageReader` interfaces.
  The real `torch.distributed.checkpoint.save`/`load` functions did run.
* No media power-loss or host-cache-loss durability claim. The store's own
  crash courts cover that separately; this receipt is a single host with a
  local store and synthetic fixtures.
* Reads never import restored containers into the store. `import_restored=True`
  now fails closed: the service restores into caller memfds and cannot
  materialize a container.
* The destination tensor copy is DCP's own contract (the planner allocates the
  target); the adapter does not claim zero-copy into a planner-owned tensor.
  The decoded bytes themselves land in a caller-owned memfd with no staging
  file.
* `checkpoint restore` decodes whole shards (mode-10 frames are not
  independently seekable); region/tile-level savings are not claimed here.
* `BYTE_IO` objects under `topology > 1` require divisible lengths; the adapter
  rejects rather than pads.
* Version 0.8.0 of the Python package; the wheel is a court artifact, not a
  PyPI publication.
