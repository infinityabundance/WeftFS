"""PyTorch Distributed Checkpoint adapter tests.

The real-DCP tests call ``torch.distributed.checkpoint.save`` / ``load`` with
``weftcodec.dcp`` storage classes, then verify the resulting canonical WeftFS
checkpoint with the Rust CLI. They are skipped with an explicit reason when
PyTorch or the Rust ``weftfs`` CLI is absent.

``test_contract_mode_without_torch`` is contract-only: it blocks the ``torch``
import in a subprocess and checks the documented API shape. It is not a real DCP
run and is labelled as such.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import shutil
import struct
import subprocess
import sys
from pathlib import Path

import pytest

import support

from weftcodec import wcb
from weftcodec.dcp import (
    DEFAULT_CLI,
    CheckpointSession,
    DcpError,
    ExactnessError,
    StagingLimitExceeded,
    WeftStorageReader,
    WeftStorageWriter,
    init_store,
    save_checkpoint_async,
    verify_checkpoint,
)

HAS_TORCH = importlib.util.find_spec("torch") is not None
WEFTFS_CLI = Path(os.environ.get("WEFTFS_CLI", DEFAULT_CLI))


def _has_weftfs() -> bool:
    return WEFTFS_CLI.is_file() and os.access(WEFTFS_CLI, os.X_OK)


pytestmark = pytest.mark.skipif(
    not _has_weftfs(),
    reason="weftfs CLI not found at %s; run this test in the Docker python court"
    % WEFTFS_CLI,
)

torch = pytest.importorskip("torch") if HAS_TORCH else None
dcp = pytest.importorskip("torch.distributed.checkpoint") if HAS_TORCH else None
if HAS_TORCH:
    from torch.distributed.checkpoint.api import CheckpointException
else:  # pragma: no cover - contract-only runs
    CheckpointException = DcpError

requires_torch = pytest.mark.skipif(
    not HAS_TORCH, reason="PyTorch is not installed in this court"
)


def _run(*args, expect=0):
    proc = subprocess.run(
        [str(WEFTFS_CLI), "--store", str(args[0]), *[str(a) for a in args[1:]]],
        capture_output=True,
        check=False,
    )
    if expect is not None:
        assert proc.returncode == expect, (
            "weftfs %s failed (%d): %s"
            % (" ".join(str(a) for a in args[1:]), proc.returncode, proc.stderr.decode())
        )
    return proc


def _write_safetensors(path, tensors):
    """Write ``{name: tensor}`` exactly, without depending on safetensors."""
    from weftcodec.dcp import _SAFETENSORS_BY_TORCH, _tensor_bytes

    header = {}
    payloads = []
    cursor = 0
    for name, tensor in sorted(tensors.items()):
        payload = _tensor_bytes(tensor)
        header[name] = {
            "dtype": _SAFETENSORS_BY_TORCH[str(tensor.dtype)][1],
            "shape": list(tensor.shape),
            "data_offsets": [cursor, cursor + len(payload)],
        }
        cursor += len(payload)
        payloads.append(payload)
    raw_header = json.dumps(header, sort_keys=True, separators=(",", ":")).encode()
    with open(path, "wb") as handle:
        handle.write(struct.pack("<Q", len(raw_header)))
        handle.write(raw_header)
        for payload in payloads:
            handle.write(payload)


class SmallModel(torch.nn.Module if HAS_TORCH else object):
    """Shapes are divisible by 8 on axis 0 so the topology matrix is real."""

    def __init__(self):
        super().__init__()
        generator = torch.Generator().manual_seed(20261105)
        self.embed = torch.nn.Parameter(
            torch.randn(16, 8, generator=generator, dtype=torch.float32)
        )
        self.bias = torch.nn.Parameter(
            torch.randn(16, generator=generator, dtype=torch.float32)
        )
        self.block = torch.nn.Module()
        self.block.weight = torch.nn.Parameter(
            torch.randn(8, 4, generator=generator, dtype=torch.float32)
        )


class BadShapeModel(torch.nn.Module if HAS_TORCH else object):
    def __init__(self):
        super().__init__()
        self.weight = torch.nn.Parameter(torch.arange(10, dtype=torch.float32))


@pytest.fixture()
def store(tmp_path):
    root = tmp_path / "store"
    init_store(root, weftfs_cli=WEFTFS_CLI)
    return root


def _assert_state_equal(left, right):
    assert set(left) == set(right)
    for name in left:
        assert torch.equal(left[name], right[name]), "tensor %s differs" % name


@requires_torch
def test_real_dcp_roundtrip_and_rust_verification(store):
    model = SmallModel()
    source = {name: value.detach().clone() for name, value in model.state_dict().items()}

    writer = WeftStorageWriter("roundtrip", store_root=store, topology=1)
    metadata = dcp.save(model.state_dict(), storage_writer=writer, no_dist=True)

    assert writer.store_name == "roundtrip"
    assert sorted(metadata.state_dict_metadata) == sorted(source)
    assert writer.stats["entries"] == len(source)

    target = SmallModel()
    reader = WeftStorageReader("roundtrip", store_root=store)
    dcp.load(target.state_dict(), storage_reader=reader, no_dist=True)
    _assert_state_equal(source, target.state_dict())
    assert reader.metadata_source == "dcp"
    assert reader.storage_name == "roundtrip"

    result = verify_checkpoint(store, "roundtrip", full=True, weftfs_cli=WEFTFS_CLI)
    assert result["tensors_verified"] == len(source)
    assert result["full"]["ok"] is True
    assert result["full"]["problems"] == []


@requires_torch
def test_topology_matrix_8_4_1_and_exactness(store):
    from weftcodec.dcp import _tensor_bytes

    model = SmallModel()
    source = {name: value.detach().clone() for name, value in model.state_dict().items()}

    writer = WeftStorageWriter("matrix", store_root=store, topology=8)
    dcp.save(model.state_dict(), storage_writer=writer, no_dist=True)
    assert writer.stats["topology"] == (8, 0)

    target = SmallModel()
    reader = WeftStorageReader("matrix", store_root=store)
    dcp.load(target.state_dict(), storage_reader=reader, no_dist=True)
    _assert_state_equal(source, target.state_dict())

    manifest = reader._store.checkpoint_show("matrix")["manifest"]
    manifest_names = {row["name"] for row in manifest["tensors"]}
    assert manifest_names == set(source)
    assert all(len(row["shards"]) == 8 for row in manifest["tensors"])

    roots = set()
    for name in sorted(source):
        expected = _tensor_bytes(source[name])
        for shards in (8, 4, 1):
            restored = reader.restore_entry(name, topology=shards, axis=0)
            assert restored.data == expected, "%s topology %d bytes differ" % (
                name,
                shards,
            )
            assert restored.content_root_hex == [
                row for row in manifest["tensors"] if row["name"] == name
            ][0]["content_root"]
            roots.add(restored.content_root_hex)
    # The reassembled logical tensor is identical under every topology.
    assert len(roots) == len(source)

    full = verify_checkpoint(store, "matrix", full=True, weftfs_cli=WEFTFS_CLI)
    assert full["tensors_verified"] == len(source)
    assert full["full"]["ok"] is True


@requires_torch
def test_rust_written_checkpoint_is_read_by_the_adapter(store, tmp_path):
    model = SmallModel()
    source = {name: value.detach().clone() for name, value in model.state_dict().items()}
    container = tmp_path / "rust.safetensors"
    _write_safetensors(container, source)

    # The Rust CLI alone writes the canonical checkpoint: split every tensor
    # 2 ways on axis 0.
    _run(store, "checkpoint", "write", "--file", container,
         "--name", "rustckpt", "--shards", 2, "--axis", 0, "--json")

    target = SmallModel()
    reader = WeftStorageReader("rustckpt", store_root=store)
    metadata = reader.read_metadata()
    assert reader.metadata_source == "manifest"
    assert set(metadata.state_dict_metadata) == set(source)
    dcp.load(target.state_dict(), storage_reader=reader, no_dist=True)
    _assert_state_equal(source, target.state_dict())

    result = verify_checkpoint(store, "rustckpt", full=True, weftfs_cli=WEFTFS_CLI)
    assert result["tensors_verified"] == len(source)
    assert result["full"]["ok"] is True


@requires_torch
def test_adapter_metadata_content_roots_match_the_rust_manifest(store):
    model = SmallModel()
    writer = WeftStorageWriter("roots", store_root=store, topology=4)
    dcp.save(model.state_dict(), storage_writer=writer, no_dist=True)

    reader = WeftStorageReader("roots", store_root=store)
    metadata = reader.read_metadata()
    manifest = reader._store.checkpoint_show("roots")["manifest"]
    rows = {row["name"]: row for row in manifest["tensors"]}
    for info in metadata.storage_data.values():
        assert rows[info.entry]["content_root"] == info.content_root.hex()
        assert rows[info.entry]["dtype"] == {
            "torch.float32": "f32",
            "torch.float16": "f16",
            "torch.bfloat16": "bf16",
            "torch.float64": "f64",
            "torch.int8": "i8",
            "torch.uint8": "u8",
            "torch.int16": "i16",
            "torch.int32": "i32",
            "torch.int64": "i64",
        }[info.dtype]


@requires_torch
def test_byte_state_objects_roundtrip(store):
    model = SmallModel()
    source = {name: value.detach().clone() for name, value in model.state_dict().items()}
    source["training"] = {"step": 7, "label": "run-a", "seed": 20261105}

    dcp.save(
        source,
        storage_writer=WeftStorageWriter("bytes", store_root=store, topology=1),
        no_dist=True,
    )

    target = {name: torch.empty_like(value) for name, value in model.state_dict().items()}
    target["training"] = {"step": 0, "label": "", "seed": 0}
    reader = WeftStorageReader("bytes", store_root=store)
    metadata = reader.read_metadata()
    assert metadata.state_dict_metadata["training.step"].__class__.__name__ == (
        "BytesStorageMetadata"
    )
    dcp.load(target, storage_reader=reader, no_dist=True)
    _assert_state_equal(
        {name: value for name, value in source.items() if name != "training"},
        {name: value for name, value in target.items() if name != "training"},
    )
    assert target["training"] == source["training"]


@requires_torch
def test_save_generations_and_overwrite_semantics(store):
    model = SmallModel()
    dcp.save(
        model.state_dict(),
        storage_writer=WeftStorageWriter("gen", store_root=store),
        no_dist=True,
    )
    first_entry = WeftStorageReader("gen", store_root=store).restore_entry("bias")

    # Change every value, save again: the adapter publishes <id>.v2 (there is
    # no store-side recursive delete), and the reader resolves the newest.
    for parameter in model.parameters():
        with torch.no_grad():
            parameter.add_(1.0)
    dcp.save(
        model.state_dict(),
        storage_writer=WeftStorageWriter("gen", store_root=store),
        no_dist=True,
    )
    reader = WeftStorageReader("gen", store_root=store)
    reader.read_metadata()
    assert reader.storage_name == "gen.v2"
    newer = reader.restore_entry("bias")
    assert newer.data != first_entry.data

    target = SmallModel()
    target.load_state_dict(model.state_dict())
    dcp.load(target.state_dict(), storage_reader=WeftStorageReader("gen", store_root=store),
             no_dist=True)
    _assert_state_equal(model.state_dict(), target.state_dict())

    with pytest.raises(CheckpointException) as caught:
        dcp.save(
            model.state_dict(),
            storage_writer=WeftStorageWriter("gen", store_root=store, overwrite=False),
            no_dist=True,
        )
    assert "FileExistsError" in _exception_chain(caught.value)


@requires_torch
def test_async_save_completion_and_error_propagation(store):
    model = SmallModel()
    source = {name: value.detach().clone() for name, value in model.state_dict().items()}

    handle = save_checkpoint_async(
        model.state_dict(),
        checkpoint_id="async-ok",
        store_root=store,
        topology=4,
        staging_bytes=4 * 1024 * 1024,
        weftfs_cli=WEFTFS_CLI,
    )
    metadata = handle.result(timeout=300)
    assert handle.done() is True
    assert metadata is not None
    assert handle.writer.store_name == "async-ok"
    assert handle.stats["staging_peak_bytes"] <= 4 * 1024 * 1024

    target = SmallModel()
    dcp.load(target.state_dict(),
             storage_reader=WeftStorageReader("async-ok", store_root=store),
             no_dist=True)
    _assert_state_equal(source, target.state_dict())

    # A non-divisible topology is a real Rust-CLI rejection, not a fallback.
    bad = save_checkpoint_async(
        BadShapeModel().state_dict(),
        checkpoint_id="async-bad-topology",
        store_root=store,
        topology=8,
        weftfs_cli=WEFTFS_CLI,
    )
    with pytest.raises(CheckpointException) as caught:
        bad.result(timeout=300)
    assert "InvalidArgument" in _exception_chain(caught.value) or "no silent padding" in _exception_chain(caught.value)
    assert "async-bad-topology" not in _checkpoint_names(store)

    staged = save_checkpoint_async(
        model.state_dict(),
        checkpoint_id="async-staging",
        store_root=store,
        topology=1,
        staging_bytes=16,
        weftfs_cli=WEFTFS_CLI,
    )
    with pytest.raises(CheckpointException) as caught:
        staged.result(timeout=300)
    assert "StagingLimitExceeded" in _exception_chain(caught.value)
    assert "async-staging" not in _checkpoint_names(store)


def _exception_chain(exc):
    parts = []
    seen = set()
    current = exc
    while current is not None and id(current) not in seen:
        seen.add(id(current))
        parts.append("%s: %s" % (type(current).__name__, current))
        for nested in getattr(current, "exceptions", {}).values():
            parts.append("%s: %s" % (type(nested).__name__, nested))
        current = current.__cause__ or current.__context__
    return " | ".join(parts)


def _checkpoint_names(store):
    payload = json.loads(
        _run(store, "checkpoint", "list", "--json").stdout.decode("utf-8")
    )
    return {row["name"] for row in payload.get("checkpoints", [])}


@requires_torch
def test_unsupported_dtype_fails_closed(store):
    class BoolState(torch.nn.Module):
        def __init__(self):
            super().__init__()
            self.mask = torch.nn.Parameter(
                torch.tensor([True, False, True, False]), requires_grad=False
            )

    writer = WeftStorageWriter("bad-dtype", store_root=store)
    with pytest.raises(CheckpointException) as caught:
        dcp.save(BoolState().state_dict(), storage_writer=writer, no_dist=True)
    assert "UnsupportedDtype" in _exception_chain(caught.value)
    assert "bad-dtype" not in _checkpoint_names(store)


def test_contract_mode_without_torch(tmp_path):
    """Contract-only: the documented API shape with torch import blocked.

    This is not a real DCP run; the real round trips are the tests above.
    """
    root = tmp_path / "store"
    script = r"""
import importlib.abc
import sys

class Blocker(importlib.abc.MetaPathFinder):
    def find_spec(self, name, path=None, target=None):
        if name == "torch" or name.startswith("torch."):
            raise ImportError("torch blocked by contract test")
        return None

sys.meta_path.insert(0, Blocker())
import weftcodec.dcp as dcp

assert dcp.TORCH_AVAILABLE is False, "torch was importable"
required_writer = {"reset", "write_metadata", "write_data", "finish",
                   "storage_meta", "set_up_storage_writer", "prepare_local_plan",
                   "prepare_global_plan", "validate_checkpoint_id"}
required_reader = {"reset", "read_metadata", "read_data",
                   "set_up_storage_reader", "prepare_local_plan",
                   "prepare_global_plan", "validate_checkpoint_id"}
assert required_writer <= set(dir(dcp.WeftStorageWriter))
assert required_reader <= set(dir(dcp.WeftStorageReader))
assert dcp.WeftStorageWriter.validate_checkpoint_id("ok") is True
assert dcp.WeftStorageWriter.validate_checkpoint_id("not/ok") is False
w = dcp.WeftStorageWriter("contract", store_root=sys.argv[1])
assert w.checkpoint_id == "contract"
try:
    w.write_data(None, None).value()
except dcp.TorchRequired:
    pass
else:
    raise AssertionError("write_data did not require torch")
print("contract-ok")
"""
    dcp_dummy = init_store(root, weftfs_cli=WEFTFS_CLI)
    assert dcp_dummy == root
    proc = subprocess.run(
        [sys.executable, "-c", script, str(root)],
        capture_output=True,
        text=True,
        env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parents[1])},
    )
    assert proc.returncode == 0, proc.stderr
    assert "contract-ok" in proc.stdout


def test_metadata_wcb_roundtrip_is_canonical():
    """The DCP metadata blob is canonical WCB and decodes back to itself."""
    from weftcodec.dcp import DCP_METADATA_SCHEMA

    value = {
        "schema": DCP_METADATA_SCHEMA,
        "checkpoint_id": "x",
        "storage_name": "x",
        "dcp_version": "1.0.0",
        "state_dict": [],
        "storage_data": [],
        "planner_data": {"kind": "none"},
    }
    encoded = wcb.encode(value)
    assert wcb.encode(wcb.decode(encoded)) == encoded
    assert wcb.decode(encoded)["schema"] == DCP_METADATA_SCHEMA


@requires_torch
def test_session_reuses_one_service_and_restores_exactly(store):
    """A `CheckpointSession` saves repeatedly through one helper process."""
    model = SmallModel()
    source = {name: value.detach().clone() for name, value in model.state_dict().items()}

    with CheckpointSession(store, weftfs_cli=WEFTFS_CLI, session_idle_secs=30) as session:
        first = session.save(model.state_dict(), checkpoint_id="sess-1")
        second = session.save(model.state_dict(), checkpoint_id="sess-2")
        assert session.stats["services_started"] == 1
        assert session.stats["saves"] == 2
        # The same helper served both saves.
        pid = session.service_pid
        assert session.service().pid == pid

    assert first["entries"] == second["entries"] == len(source)
    for name in ("sess-1", "sess-2"):
        reader = WeftStorageReader(name, store_root=store, weftfs_cli=WEFTFS_CLI)
        target = SmallModel()
        dcp.load(target.state_dict(), storage_reader=reader, no_dist=True)
        _assert_state_equal(source, target.state_dict())
        reader.close()
    result = verify_checkpoint(store, "sess-2", full=True, weftfs_cli=WEFTFS_CLI)
    assert result["full"]["ok"] is True


@requires_torch
def test_session_is_not_usable_in_a_forked_child(store):
    """A child inherits the attributes but must not touch the parent's helper."""
    if not hasattr(os, "fork"):
        pytest.skip("no fork on this platform")
    with CheckpointSession(store, weftfs_cli=WEFTFS_CLI) as session:
        session.service()
        pid = os.fork()
        if pid == 0:  # child
            status = 1
            try:
                from weftcodec.service import ServiceUnavailable

                try:
                    session.service()
                except ServiceUnavailable:
                    status = 0
            finally:
                os._exit(status)
        _, status = os.waitpid(pid, 0)
        assert os.waitstatus_to_exitcode(status) == 0
        # The parent's helper is still alive and usable.
        assert session.service().info()["protocol"] == 3


@requires_torch
def test_session_keeps_the_writer_lock_until_it_is_closed(store, tmp_path):
    """A reused session does not weaken the single-writer guarantee.

    While the session is warm its helper holds the writable handle, so an
    independent ``weftfs`` process must be refused the writer lock. Closing the
    session releases it and the same command then succeeds. In between, three
    saves through the one helper must all be complete and exact -- reuse must
    not change publication semantics.
    """
    model = SmallModel()
    head = tmp_path / "payload.txt"
    head.write_text("independent-writer\n", encoding="utf-8")

    with CheckpointSession(store, weftfs_cli=WEFTFS_CLI, session_idle_secs=30) as session:
        for index in range(1, 4):
            session.save(model.state_dict(), checkpoint_id="lock-%d" % index)
        assert session.stats["services_started"] == 1
        assert session.stats["saves"] == 3

        refused = _run(store, "create", "/independent.txt", "--file", head, expect=None)
        assert refused.returncode != 0
        text = (refused.stderr + refused.stdout).decode()
        assert "Busy" in text or "locked" in text.lower(), text
        listed = _run(store, "checkpoint", "list", "--json")
        names = {row["name"] for row in json.loads(listed.stdout.decode())["checkpoints"]}
        assert names == {"lock-1", "lock-2", "lock-3"}
        for name in sorted(names):
            result = verify_checkpoint(store, name, full=True, weftfs_cli=WEFTFS_CLI)
            assert result["full"]["ok"] is True

    # The session is closed: the exclusive lock is gone and the same write works.
    accepted = _run(store, "create", "/independent.txt", "--file", head)
    assert accepted.returncode == 0


@requires_torch
def test_session_killed_during_a_save_leaves_no_partial_checkpoint(store):
    """SIGKILL a process mid-save: the store exposes nothing or a whole one.

    The child owns one session and one save it never finishes; the parent kills
    it and then asserts the old-or-new contract from outside: no partially
    published checkpoint, no verify problem, and any checkpoint that does exist
    restores the exact source bytes.
    """
    import time

    def expected_state():
        generator = torch.Generator().manual_seed(20261105)
        return {
            "embed": torch.randn(16, 8, generator=generator),
            "bias": torch.randn(16, generator=generator),
            "block.weight": torch.randn(8, 4, generator=generator),
        }

    source = expected_state()
    child = (
        "import sys,time\n"
        "sys.path.insert(0, %r)\n"
        "import torch\n"
        "from weftcodec.dcp import CheckpointSession\n"
        "gen = torch.Generator().manual_seed(20261105)\n"
        "state = {'embed': torch.randn(16, 8, generator=gen),\n"
        "         'bias': torch.randn(16, generator=gen),\n"
        "         'block.weight': torch.randn(8, 4, generator=gen)}\n"
        "session = CheckpointSession(%r, weftfs_cli=%r, session_idle_secs=0)\n"
        "sys.stdout.write('ready\\n')\n"
        "sys.stdout.flush()\n"
        "session.save(state, checkpoint_id='killed')\n"
        "time.sleep(30)\n"
    ) % (
        str(Path(__file__).resolve().parents[1]),
        str(store),
        str(WEFTFS_CLI),
    )
    # Kill points relative to the first byte of the save (the child signals
    # "ready" after its torch import and helper setup, so the delay is the save
    # window itself): before the first publication, inside it, and after the
    # commit. A reused session must not turn any of them into a partial state.
    for delay in (0.001, 0.01, 0.04):
        proc = subprocess.Popen(
            [sys.executable, "-c", child],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env={
                **os.environ,
                "PYTHONPATH": str(Path(__file__).resolve().parents[1]),
            },
        )
        assert proc.stdout.readline().strip() == b"ready"
        time.sleep(delay)
        proc.kill()
        rest = proc.communicate(timeout=120)[0]
        assert proc.returncode != 0, rest[-2000:]

        listed = _run(store, "checkpoint", "list", "--json")
        rows = json.loads(listed.stdout.decode())["checkpoints"]
        for row in rows:
            assert row["complete"] is True, row
            result = verify_checkpoint(
                store, row["name"], full=True, weftfs_cli=WEFTFS_CLI
            )
            assert result["full"]["ok"] is True, result
            reader = WeftStorageReader(
                row["name"], store_root=store, weftfs_cli=WEFTFS_CLI
            )
            try:
                target = expected_state()
                dcp.load(target, storage_reader=reader, no_dist=True)
            finally:
                reader.close()
            _assert_state_equal(source, target)


@requires_torch
def test_direct_container_matches_the_staged_path(store, tmp_path):
    """One copy instead of two, and byte-identical restored content."""
    from weftcodec.service import CheckpointService

    second = tmp_path / "store-direct"
    init_store(second, weftfs_cli=WEFTFS_CLI)
    model = SmallModel()

    staged = WeftStorageWriter(
        "staged", store_root=store, topology=1, direct_container=False
    )
    dcp.save(model.state_dict(), storage_writer=staged, no_dist=True)
    staged_stats = staged.stats

    direct = WeftStorageWriter(
        "direct", store_root=second, topology=1, direct_container=True
    )
    dcp.save(model.state_dict(), storage_writer=direct, no_dist=True)
    direct_stats = direct.stats

    assert staged_stats["container_copies"] == 2
    assert direct_stats["container_copies"] == 1
    assert direct_stats["staging_buffer_bytes"] == 0
    assert staged_stats["container_bytes"] == direct_stats["container_bytes"]

    roots = {}
    for label, root in (("staged", store), ("direct", second)):
        with CheckpointService(root, cli=WEFTFS_CLI) as service:
            manifest = service.manifest(label)
            rows = {}
            for row in manifest.tensors:
                buffer = service.restore_tensor(
                    label, row.name, expected_size=row.element_bytes
                )
                rows[row.name] = hashlib.sha256(buffer.view).hexdigest()
                buffer.close()
            roots[label] = rows
    assert roots["staged"] == roots["direct"]
    verify_checkpoint(second, "direct", full=True, weftfs_cli=WEFTFS_CLI)
