# JAX / Orbax adapter (`weftcodec.orbax`)

`weftcodec.orbax` is an Orbax `AsyncCheckpointHandler` that stores a JAX PyTree
as one canonical `weft.checkpoint.v1` checkpoint through the shipped Rust
`weftfs` CLI. It reuses the DCP adapter's storage plumbing and writes no new
wire format. The measured comparison against Orbax's own OCDBT checkpoint is in
`bench/RESULTS-orbax.md`; the receipt is
`bench/receipts/orbax-20261106T152612Z.json`.

## Install

JAX and Orbax are optional dependencies:

```sh
pip install "weftcodec-0.9.0-py3-none-any.whl[orbax]"
# or, in the court:
pip install --target /work/.docker-cache/py-orbax 'jax[cpu]' orbax-checkpoint
```

Importing `weftcodec` never imports JAX or Orbax. Importing
`weftcodec.orbax` without them works in contract mode; operations that need JAX
raise `JaxRequired`.

## API

| name | purpose |
|---|---|
| `WeftOrbaxHandler` | Orbax `AsyncCheckpointHandler`; drive it with `Checkpointer`, `AsyncCheckpointer`, or `CheckpointManager` |
| `WeftSaveArgs` | `item`, `store_root`, `checkpoint_id`, `write_topology`, `shard_strategy`, `staging_bytes`, `overwrite`, `init_store` |
| `WeftRestoreArgs` | `item`, `store_root`, `checkpoint_id`, `storage_name`, `restore_topology`, `partial_restore`, `staging_bytes` |
| `save_pytree` / `restore_pytree` | synchronous convenience wrappers |
| `save_async_pytree` | one bounded worker; returns a DCP-style `AsyncSaveHandle` |
| `restore_leaf` | adapter extension: restore exactly one leaf by canonical path |
| `metadata_of` / `leaf_paths` / `flatten_pytree` | inspect the canonical record and leaf paths |

`checkpoint_id` is a single WeftFS checkpoint name, not a filesystem path. The
Orbax directory is a locator, not the tensor store.

```python
import jax
import orbax.checkpoint as ocp
from weftcodec.orbax import (
    WeftOrbaxHandler, WeftSaveArgs, WeftRestoreArgs,
)

handler = WeftOrbaxHandler(staging_bytes=256 * 1024 * 1024)
saver = ocp.AsyncCheckpointer(handler)
saver.save(
    "/checkpoints/step-10",
    args=WeftSaveArgs(
        item=params,
        store_root="/data/weft-store",
        checkpoint_id="step-10",
        write_topology=(8, 0),       # store each leaf split 8 ways on axis 0
        shard_strategy="zstd",       # zstd | raw | tensor-sharded
    ),
)
saver.wait_until_finished()

target = jax.tree_util.tree_map(
    lambda a: jax.ShapeDtypeStruct(a.shape, a.dtype, sharding=a.sharding),
    params,
)
restored = ocp.Checkpointer(handler).restore(
    "/checkpoints/step-10", args=WeftRestoreArgs(item=target)
)
assert jax.tree_util.tree_structure(restored) == jax.tree_util.tree_structure(target)
```

`CheckpointManager` works through `item_handlers` and
`ocp.args.Composite(..., state=WeftSaveArgs(...))`; see the tests.

## What is written

The Rust CLI ingests a safetensors staging container and publishes a canonical
`weft.checkpoint.v1` checkpoint (one mode-10 or exact codec shard per requested
split). The adapter then writes:

* `/checkpoints/<storage>/orbax.metadata` — a canonical WCB
  `weft.orbax.v1` record;
* `<orbax directory>/weft.orbax.wcb` — a canonical WCB
  `weft.orbax.pointer.v1` locator containing the store path, checkpoint id,
  storage generation, and the BLAKE3 root of the metadata record.

The `weft.orbax.v1` record has this exact key set:

```
schema, checkpoint_id, storage_name, leaf_count, leaves,
write_topology, shard_strategy, staging_bytes, source_file_bytes,
source_sha256, jax_version, orbax_version
```

Each leaf row has `entry`, `path`, `path_types`, `dtype`, `shape`,
`element_bytes` (total tensor bytes, matching the manifest), `content_root`
(32-byte BLAKE3 of the exact leaf bytes), and `logical_root` (32-byte
topology-independent tensor root). `leaf_count` must equal the number of rows;
entries and paths must be unique; roots must be 32 bytes.

## Exactness and fail-closed behaviour

Restore flattens the target with `jax.tree_util.tree_flatten_with_path`, matches
leaves by canonical path, and then:

1. rejects a target leaf that is absent from the checkpoint (`UnexpectedLeaf`);
2. rejects a stored leaf absent from the target unless
   `partial_restore=True` (`MissingLeaf`);
3. checks dtype and shape before reading data (`MismatchedLeaf`);
4. restores through `weftfs checkpoint restore`, checking the manifest dtype,
   shape, element bytes, content root, and logical root;
5. recomputes each restored payload's BLAKE3 content root and fails
   (`ExactnessError`) on any mismatch;
6. re-materialises the target PyTree in target flatten order, honouring a
   requested JAX `sharding` when the target carries one.

A different JAX sharding topology is allowed because the stored logical tensor
is topology independent. `restore_topology` is a separate Rust-side proof
(`checkpoint restore --topology N`), independent of JAX placement.

Supported leaf dtypes are exactly the WEFT-TENSOR v1 set: `f16`, `bf16`, `f32`,
`f64`, `i8`, `u8`, `i16`, `i32`, `i64`. Bool, float8, complex, unsigned
16/32/64-bit, and non-array leaves are rejected, never coerced.

## Async and bounds

`AsyncCheckpointer` calls `async_save`, which returns a future. The save starts
on Orbax's background commit thread only after Orbax has created its temporary
directory; this avoids a real create/remove race. The caller's `save()` returns
immediately. `staging_bytes` bounds the total leaf bytes of the staging
container and the total bytes materialised for one restore; exceeding it raises
`StagingLimitExceeded` before publishing. The handler's worker pool is bounded
to 1–4 workers. Every leaf is copied through host memory; this is not a
zero-copy or GPU-direct path.

## Verification

```sh
weftfs --store /data/weft-store checkpoint verify --name step-10 --json
weftfs --store /data/weft-store verify --full --json
```

The measured court in `bench/RESULTS-orbax.md` records the exact commands,
versions, leaf hashes, Rust verification, and every measured loss.

## Not covered

CPU only; no TPU and no GPU. Single process, single host; no multi-host and no
true distributed JAX. The Orbax checkpoint is OCDBT/TensorStore and the WeftFS
checkpoint is a separate on-disk format, so this is not file-level
interoperability. No Zarr3, per-leaf `save_args`/`restore_args`, transforms,
custom metadata, step-metadata contents, retention/deletion internals,
emergency checkpointing, or multi-axis meshes. No process-crash, power-loss, or
media-failure court. No Go-side Orbax adapter. The measured workload is
synthetic random weights, one run, warm caches; no fleet or production claim.
