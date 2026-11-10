"""JAX / Orbax adapter tests.

The real-Orbax tests drive ``orbax.checkpoint.Checkpointer``,
``AsyncCheckpointer``, and ``CheckpointManager`` against
``weftcodec.orbax.WeftOrbaxHandler`` and verify the published
``weft.checkpoint.v1`` checkpoint with the Rust CLI. They are skipped with an
explicit reason when JAX, Orbax, or the Rust ``weftfs`` CLI is absent.

``test_contract_mode_without_jax`` is contract-only: it blocks the ``jax`` and
``orbax`` imports in a subprocess and checks the documented fail-closed shape.
It is not a real Orbax run and is labelled as such.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

# Force a multi-device host platform before JAX is imported. This is a real
# 8-device CPU mesh, not a simulated sharding label.
os.environ.setdefault("XLA_FLAGS", "--xla_force_host_platform_device_count=8")

import support  # noqa: E402

from weftcodec import wcb  # noqa: E402
from weftcodec.dcp import DEFAULT_CLI, _Storage, init_store  # noqa: E402

def _has_module(name: str) -> bool:
    try:
        return importlib.util.find_spec(name) is not None
    except (ImportError, ModuleNotFoundError):
        # find_spec raises when a parent package (e.g. ``orbax``) is absent.
        return False


HAS_JAX = _has_module("jax")
HAS_ORBAX = _has_module("orbax.checkpoint")
WEFTFS_CLI = Path(os.environ.get("WEFTFS_CLI", DEFAULT_CLI))


def _has_weftfs() -> bool:
    return WEFTFS_CLI.is_file() and os.access(WEFTFS_CLI, os.X_OK)


pytestmark = pytest.mark.skipif(
    not (_has_weftfs() and HAS_JAX and HAS_ORBAX),
    reason=(
        "requires jax, orbax-checkpoint, and the weftfs CLI at %s; run this "
        "test in the Docker python court with the Orbax target on PYTHONPATH"
        % WEFTFS_CLI
    ),
)

if HAS_JAX and HAS_ORBAX:
    import numpy as np
    import jax
    import jax.numpy as jnp
    import orbax.checkpoint as ocp
    from jax.sharding import Mesh, NamedSharding, PartitionSpec as P

    from weftcodec import orbax as wo
else:  # pragma: no cover - collection-only path
    np = jax = jnp = ocp = None
    Mesh = NamedSharding = P = None
    wo = None


requires = pytest.mark.skipif(
    not (_has_weftfs() and HAS_JAX and HAS_ORBAX),
    reason="requires jax, orbax-checkpoint, and the weftfs CLI",
)


# ---------------------------------------------------------------------------
# Fixtures and helpers


@pytest.fixture(scope="module")
def mesh():
    devices = jax.devices()
    if len(devices) < 2:
        pytest.skip("this test needs at least two JAX devices")
    return Mesh(np.array(devices), ("x",))


@pytest.fixture(scope="module")
def sharding(mesh):
    return NamedSharding(mesh, P("x"))


@pytest.fixture()
def store(tmp_path):
    root = tmp_path / "store"
    init_store(root, weftfs_cli=WEFTFS_CLI)
    return root


@pytest.fixture()
def handler():
    handler = wo.WeftOrbaxHandler(weftfs_cli=WEFTFS_CLI, staging_bytes=8 << 20)
    try:
        yield handler
    finally:
        handler.close()


def _tree():
    return {
        "params": {
            "f32": jnp.arange(16 * 4, dtype=jnp.float32).reshape(16, 4),
            "bf16": jnp.arange(16 * 4, dtype=jnp.bfloat16).reshape(16, 4),
            "f16": jnp.arange(16, dtype=jnp.float16),
            "i32": jnp.arange(16, dtype=jnp.int32),
            "i8": jnp.arange(16, dtype=jnp.int8),
            "u8": jnp.arange(16, dtype=jnp.uint8),
        },
        "step": jnp.arange(8, dtype=jnp.int32),
    }


def _shard(tree, sharding):
    return jax.tree_util.tree_map(lambda value: jax.device_put(value, sharding), tree)


def _target(tree, sharding=None):
    def make(value):
        if sharding is None:
            return jax.ShapeDtypeStruct(value.shape, value.dtype)
        return jax.ShapeDtypeStruct(value.shape, value.dtype, sharding=sharding)

    return jax.tree_util.tree_map(make, tree)


def _bytes(value):
    return np.asarray(jax.device_get(value)).tobytes()


def _assert_exact(left, right):
    left_flat = wo.flatten_pytree(left)
    right_flat = wo.flatten_pytree(right)
    assert [path for path, _ in left_flat] == [path for path, _ in right_flat]
    for (path, a), (path2, b) in zip(left_flat, right_flat):
        assert path == path2
        assert str(a.dtype) == str(b.dtype), path
        assert tuple(a.shape) == tuple(b.shape), path
        assert _bytes(a) == _bytes(b), path


def _save(handler, store, directory, checkpoint_id, tree, **kwargs):
    args = wo.WeftSaveArgs(
        item=tree,
        store_root=store,
        checkpoint_id=checkpoint_id,
        init_store=True,
        **kwargs,
    )
    return handler.save(directory, args)


# ---------------------------------------------------------------------------
# Roundtrip and real Orbax orchestration


@requires
def test_roundtrip_exact_and_rust_verification(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "ckpt"
    result = _save(handler, store, directory, "roundtrip", source)

    assert result.leaf_count == 7
    assert result.storage_name == "roundtrip"
    assert (directory / wo.ORBAX_POINTER_NAME).is_file()
    assert result.leaf_bytes == sum(
        int(np.prod(leaf.shape)) * wo._element_bytes(leaf.dtype)
        for leaf in result.leaves
    )

    target = _target(source, sharding)
    restored = handler.restore(directory, wo.WeftRestoreArgs(item=target))
    _assert_exact(source, restored)

    metadata = wo.metadata_of(directory)
    assert metadata.leaf_count == result.leaf_count
    assert metadata.storage_name == result.storage_name
    assert [leaf.path for leaf in metadata.leaves] == sorted(
        leaf.path for leaf in result.leaves
    )
    assert metadata.source_sha256 == result.source_sha256

    verification = wo.verify_checkpoint(store, result.storage_name, full=True)
    assert verification["tensors_verified"] == result.leaf_count
    assert verification["full"]["ok"] is True
    assert verification["full"]["problems"] == []


@requires
def test_orbax_checkpointer_integration(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "checkpointer"
    saver = ocp.Checkpointer(handler)
    saver.save(
        directory,
        args=wo.WeftSaveArgs(
            item=source, store_root=store, checkpoint_id="checkpointer", init_store=True
        ),
    )
    restored = saver.restore(
        directory, args=wo.WeftRestoreArgs(item=_target(source, sharding))
    )
    _assert_exact(source, restored)
    saver.close()


@requires
def test_orbax_async_checkpointer_integration(store, sharding, tmp_path):
    handler = wo.WeftOrbaxHandler(weftfs_cli=WEFTFS_CLI, staging_bytes=8 << 20)
    source = _shard(_tree(), sharding)
    directory = tmp_path / "async"
    saver = ocp.AsyncCheckpointer(handler)
    try:
        import time

        started = time.monotonic()
        saver.save(
            directory,
            args=wo.WeftSaveArgs(
                item=source,
                store_root=store,
                checkpoint_id="async",
                init_store=True,
            ),
        )
        pause = time.monotonic() - started
        saver.wait_until_finished()
        total = time.monotonic() - started
        assert total >= pause
        restored = ocp.Checkpointer(handler).restore(
            directory, args=wo.WeftRestoreArgs(item=_target(source, sharding))
        )
        _assert_exact(source, restored)
        assert handler.stats["saves"] == 1
    finally:
        saver.close()
        handler.close()


@requires
def test_checkpoint_manager_integration(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    manager = ocp.CheckpointManager(
        tmp_path / "manager",
        item_handlers={"state": handler},
        item_names=("state",),
        options=ocp.CheckpointManagerOptions(max_to_keep=1),
    )
    try:
        manager.save(
            3,
            args=ocp.args.Composite(
                state=wo.WeftSaveArgs(
                    item=source,
                    store_root=store,
                    checkpoint_id="manager-step3",
                    init_store=True,
                )
            ),
        )
        manager.wait_until_finished()
        restored = manager.restore(
            3,
            args=ocp.args.Composite(
                state=wo.WeftRestoreArgs(item=_target(source, sharding))
            ),
        )
        _assert_exact(source, restored["state"])
    finally:
        manager.close()


# ---------------------------------------------------------------------------
# Partial and single-leaf restore


@requires
def test_partial_restore_and_single_leaf(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "partial"
    result = _save(handler, store, directory, "partial", source)

    subset = {"params": {"f32": jax.ShapeDtypeStruct((16, 4), jnp.float32, sharding=sharding)}}
    restored = handler.restore(
        directory, wo.WeftRestoreArgs(item=subset, partial_restore=True)
    )
    _assert_exact({"params": {"f32": source["params"]["f32"]}}, restored)

    single = handler.restore_leaf(directory, "['params']['bf16']")
    expected = source["params"]["bf16"]
    assert str(single.dtype) == str(expected.dtype)
    assert tuple(single.shape) == tuple(expected.shape)
    assert _bytes(single) == _bytes(expected)

    target = jax.ShapeDtypeStruct((16, 4), jnp.bfloat16, sharding=sharding)
    single_sharded = handler.restore_leaf(
        directory, "['params']['bf16']", target=target
    )
    assert single_sharded.sharding == sharding
    assert _bytes(single_sharded) == _bytes(expected)
    assert result.leaf_count == 7


# ---------------------------------------------------------------------------
# Topology independence


@requires
def test_different_sharding_topology(store, handler, mesh, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "reshard"
    _save(handler, store, directory, "reshard", source)

    smaller = sharding if len(mesh.devices) <= 2 else NamedSharding(
        Mesh(np.array(jax.devices()[:2]), ("x",)), P("x")
    )
    target = _target(source, smaller)
    restored = handler.restore(directory, wo.WeftRestoreArgs(item=target))
    _assert_exact(source, restored)
    assert restored["params"]["f32"].sharding == smaller


@requires
def test_rust_restore_topology_proof(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "topology"
    result = _save(handler, store, directory, "topology", source, write_topology=(8, 0))
    assert result.write_topology == (8, 0)

    # The same logical leaves through 8 and then 2 Rust-side shards.
    for topology in (8, 2):
        restored = handler.restore(
            directory,
            wo.WeftRestoreArgs(
                item=_target(source, sharding), restore_topology=topology
            ),
        )
        _assert_exact(source, restored)


# ---------------------------------------------------------------------------
# Fail-closed behaviour


@requires
def test_missing_leaf_fails_closed(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "missing"
    _save(handler, store, directory, "missing", source)
    with pytest.raises(wo.MissingLeaf):
        handler.restore(
            directory,
            wo.WeftRestoreArgs(
                item={"params": {"f32": jax.ShapeDtypeStruct((16, 4), jnp.float32)}}
            ),
        )


@requires
def test_extra_leaf_fails_closed(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "extra"
    _save(handler, store, directory, "extra", source)
    target = _target(source)
    target["extra"] = jax.ShapeDtypeStruct((8,), jnp.float32)
    with pytest.raises(wo.UnexpectedLeaf):
        handler.restore(directory, wo.WeftRestoreArgs(item=target))


@requires
def test_dtype_mismatch_fails_closed(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "dtype"
    _save(handler, store, directory, "dtype", source)
    target = _target(source)
    target["params"]["f32"] = jax.ShapeDtypeStruct((16, 4), jnp.float64)
    with pytest.raises(wo.MismatchedLeaf):
        handler.restore(directory, wo.WeftRestoreArgs(item=target))


@requires
def test_shape_mismatch_fails_closed(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "shape"
    _save(handler, store, directory, "shape", source)
    target = _target(source)
    target["params"]["f32"] = jax.ShapeDtypeStruct((32, 2), jnp.float32)
    with pytest.raises(wo.MismatchedLeaf):
        handler.restore(directory, wo.WeftRestoreArgs(item=target))


@requires
def test_staging_bound_fails_closed(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "staging"
    before = store_list(store)
    with pytest.raises(wo.StagingLimitExceeded):
        _save(
            handler,
            store,
            directory,
            "staging",
            source,
            staging_bytes=16,
        )
    assert store_list(store) == before


@requires
def test_corrupt_locator_fails_closed(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "locator"
    _save(handler, store, directory, "locator", source)
    (directory / wo.ORBAX_POINTER_NAME).write_bytes(b"not canonical wcb")
    with pytest.raises(wo.InvalidMetadata):
        handler.restore(directory, wo.WeftRestoreArgs(item=_target(source)))


@requires
def test_corrupt_store_metadata_fails_closed(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "metadata"
    result = _save(handler, store, directory, "metadata", source)
    storage = _Storage(store, cli=WEFTFS_CLI)
    with tempfile.NamedTemporaryFile(prefix="weft-orbax-corrupt-", delete=False) as handle:
        handle.write(b"\x00" * 32)
        corrupt_path = handle.name
    try:
        storage._run(
            [
                "write",
                "/checkpoints/%s/%s" % (result.storage_name, wo.ORBAX_METADATA_NAME),
                "--file",
                corrupt_path,
            ]
        )
    finally:
        os.unlink(corrupt_path)
    with pytest.raises(wo.InvalidMetadata):
        handler.restore(directory, wo.WeftRestoreArgs(item=_target(source)))


@requires
def test_pointer_store_mismatch_fails_closed(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "mismatch"
    _save(handler, store, directory, "mismatch", source)
    other = tmp_path / "other"
    init_store(other, weftfs_cli=WEFTFS_CLI)
    with pytest.raises(wo.InvalidMetadata):
        handler.restore(
            directory,
            wo.WeftRestoreArgs(item=_target(source), store_root=other),
        )


# ---------------------------------------------------------------------------
# Generation and canonical record


@requires
def test_second_save_is_a_generation(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    first_dir = tmp_path / "first"
    second_dir = tmp_path / "second"
    first = _save(handler, store, first_dir, "generation", source)
    second = _save(handler, store, second_dir, "generation", source)
    assert first.storage_name == "generation"
    assert second.storage_name == "generation.v2"
    for directory, result in ((first_dir, first), (second_dir, second)):
        restored = handler.restore(
            directory, wo.WeftRestoreArgs(item=_target(source, sharding))
        )
        _assert_exact(source, restored)
        assert wo.metadata_of(directory).storage_name == result.storage_name


@requires
def test_metadata_record_is_canonical_wcb(store, handler, sharding, tmp_path):
    source = _shard(_tree(), sharding)
    directory = tmp_path / "record"
    result = _save(handler, store, directory, "record", source)

    pointer_raw = (directory / wo.ORBAX_POINTER_NAME).read_bytes()
    pointer_value = wcb.decode(pointer_raw)
    assert wcb.encode(pointer_value) == pointer_raw
    assert pointer_value["schema"] == wo.ORBAX_POINTER_SCHEMA
    assert pointer_value["storage_name"] == result.storage_name

    storage = _Storage(store, cli=WEFTFS_CLI)
    metadata_raw = storage.get_file(
        "/checkpoints/%s/%s" % (result.storage_name, wo.ORBAX_METADATA_NAME)
    )
    metadata_value = wcb.decode(metadata_raw)
    assert wcb.encode(metadata_value) == metadata_raw
    assert metadata_value["schema"] == wo.ORBAX_METADATA_SCHEMA
    assert int(metadata_value["leaf_count"].value) == result.leaf_count
    for row in metadata_value["leaves"]:
        assert row["dtype"] in ("f16", "bf16", "f32", "f64", "i8", "u8", "i16", "i32", "i64")
        assert len(row["content_root"]) == 32
        assert len(row["logical_root"]) == 32


def store_list(store):
    return _Storage(store, cli=WEFTFS_CLI).list_checkpoints()


# ---------------------------------------------------------------------------
# Contract mode (no JAX / no Orbax)


def test_contract_mode_without_jax():
    script = r"""
import sys
sys.modules['jax'] = None
sys.modules['orbax'] = None
sys.modules['orbax.checkpoint'] = None
from weftcodec import orbax
assert orbax.JAX_AVAILABLE is False
assert orbax.ORBAX_AVAILABLE is False
handler = orbax.WeftOrbaxHandler()
try:
    handler.save('/tmp/weft-contract', orbax.WeftSaveArgs(
        item=1, store_root='/tmp/weft-contract-store', checkpoint_id='x'))
except orbax.JaxRequired:
    print('OK')
else:
    raise SystemExit('expected JaxRequired')
finally:
    handler.close()
"""
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join(
        [
            str(Path(__file__).resolve().parents[1]),
            env.get("PYTHONPATH", ""),
        ]
    )
    proc = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip() == "OK"
