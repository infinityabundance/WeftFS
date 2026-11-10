"""Persistent checkpoint service client tests.

These run against the real ``weftfs checkpoint-service`` binary in the Python
court. They cover the protocol, the memfd data path, thread safety, typed
errors, bounds, and the two structural guarantees: a restore never mutates the
store, and no staging file is written on either side.
"""

from __future__ import annotations

import hashlib
import json
import os
import struct
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from weftcodec.service import (
    PROTOCOL_VERSION,
    CheckpointService,
    ServiceError,
    ServiceUnavailable,
)

CLI = os.environ.get("WEFTFS_CLI", "/work/.docker-cache/target-rust/release/weftfs")


def _available():
    return os.path.isfile(CLI) and os.access(CLI, os.X_OK)


pytestmark = pytest.mark.skipif(
    not _available(), reason="weftfs CLI not available at %s" % CLI
)

TENSOR = "weight"
SHAPE = [16, 24, 8]
SIZE = 16 * 24 * 8 * 4  # f32


def _payload(seed=7):
    state = seed | 1
    out = bytearray()
    while len(out) < SIZE:
        state ^= state >> 12
        state ^= state << 25
        state ^= state >> 27
        value = (state * 0x2545F4914F6CDD1D) & 0xFFFFFFFFFFFFFFFF
        out += value.to_bytes(8, "little")
    return bytes(out[:SIZE])


def _container(payload):
    header = {
        TENSOR: {
            "dtype": "F32",
            "shape": SHAPE,
            "data_offsets": [0, len(payload)],
        }
    }
    raw = json.dumps(header, separators=(",", ":")).encode("utf-8")
    while len(raw) % 8:
        raw += b" "
    return struct.pack("<Q", len(raw)) + raw + payload


def _digest(root):
    total = 0
    files = 0
    names = []
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            path = Path(dirpath) / name
            info = path.stat()
            total += info.st_size
            files += 1
            names.append("%s:%d" % (path.relative_to(root), info.st_size))
    names.sort()
    return (total, files, "|".join(names))


@pytest.fixture()
def store(tmp_path):
    root = tmp_path / "store"
    subprocess.run([CLI, "--store", os.fspath(root), "init"], check=True,
                   capture_output=True)
    payload = _payload()
    source = tmp_path / "source.safetensors"
    source.write_bytes(_container(payload))
    subprocess.run(
        [
            CLI,
            "--store",
            os.fspath(root),
            "checkpoint",
            "write",
            "--file",
            os.fspath(source),
            "--name",
            "svc",
            "--shards",
            "4",
            "--axis",
            "0",
            "--shard-strategy",
            "zstd",
        ],
        check=True,
        capture_output=True,
    )
    return root, payload


def test_info_manifest_and_restore_into_a_caller_memfd(store):
    root, payload = store
    with CheckpointService(root, cli=CLI) as service:
        info = service.info()
        assert info["protocol"] == PROTOCOL_VERSION
        assert info["max_frame_bytes"] == 1 << 20
        assert info["store_root"] == os.fspath(root)
        assert service.list_checkpoints() == ["svc"]
        manifest = service.manifest("svc")
        assert manifest.complete
        assert len(manifest.tensors) == 1
        row = manifest.tensors[0]
        assert row.name == TENSOR
        assert row.dtype == "f32"
        assert tuple(row.shape) == tuple(SHAPE)
        assert row.element_bytes == SIZE
        assert len(row.shards) == 4

        buffer = service.restore_tensor("svc", TENSOR)
        try:
            assert buffer.size == SIZE
            assert tuple(buffer.shape) == tuple(SHAPE)
            assert buffer.dtype == "f32"
            assert buffer.full_root_verified and buffer.shard_roots_verified
            assert buffer.view == memoryview(payload)
            assert hashlib.sha256(buffer.view).hexdigest() == hashlib.sha256(
                payload
            ).hexdigest()
            # The numpy view shares the memfd buffer.
            array = buffer.array
            assert array.shape == tuple(SHAPE)
            assert array.tobytes() == payload
        finally:
            buffer.close()

        assert service.verify("svc") == 1


def test_region_restore_writes_only_the_region(store):
    root, payload = store
    with CheckpointService(root, cli=CLI) as service:
        region = ([0, 0, 0], [4, 24, 8])
        extent = 4 * 24 * 8 * 4
        buffer = service.restore_tensor("svc", TENSOR, region=region)
        try:
            assert buffer.size == extent
            assert buffer.view == memoryview(payload[:extent])
        finally:
            buffer.close()


def test_topology_restore_verifies_the_same_root(store):
    root, payload = store
    with CheckpointService(root, cli=CLI) as service:
        for shards in (4, 2, 1):
            buffer = service.restore_tensor("svc", TENSOR, topology=(shards, 0))
            try:
                assert buffer.topology_root is not None
                assert buffer.view == memoryview(payload)
            finally:
                buffer.close()


def test_restore_does_not_mutate_the_store(store):
    root, payload = store
    before = _digest(root)
    with CheckpointService(root, cli=CLI) as service:
        for _ in range(20):
            buffer = service.restore_tensor("svc", TENSOR)
            buffer.close()
    assert _digest(root) == before


def test_write_checkpoint_from_a_memfd_and_write_file(store):
    root, payload = store
    container = _container(payload)
    with CheckpointService(root, cli=CLI) as service:
        report = service.write_checkpoint(
            "svc2", container, shards=2, axis=0, strategy="zstd"
        )
        assert report.complete
        assert report.tensor_count == 1
        assert report.source_file_bytes == len(container)
        assert report.source_sha256 == hashlib.sha256(container).hexdigest()
        assert report.manifest.manifest_root == report.manifest_root
        assert "svc2" in service.list_checkpoints()

        service.write_file("/checkpoints/svc2/note", b"hello")
        exists, size = service.stat_file("/checkpoints/svc2/note")
        assert exists and size == 5
        with service.read_file("/checkpoints/svc2/note") as view:
            assert view.bytes == b"hello"
        exists, _size = service.stat_file("/checkpoints/svc2/absent")
        assert not exists

        buffer = service.restore_tensor("svc2", TENSOR)
        try:
            assert buffer.view == memoryview(payload)
        finally:
            buffer.close()


def test_thread_pool_restores_are_byte_exact(store):
    root, payload = store
    expected = hashlib.sha256(payload).hexdigest()

    def restore_once(_index):
        buffer = service.restore_tensor("svc", TENSOR)
        try:
            return hashlib.sha256(buffer.view).hexdigest()
        finally:
            buffer.close()

    with CheckpointService(root, cli=CLI) as service:
        with ThreadPoolExecutor(max_workers=8) as pool:
            digests = list(pool.map(restore_once, range(64)))
    assert digests == [expected] * 64
    assert service.stats["requests"] >= 64


def test_typed_errors_and_bounds(store):
    root, payload = store
    with CheckpointService(root, cli=CLI, max_tensor_bytes=1024) as service:
        with pytest.raises(ServiceError) as info:
            service.restore_tensor("svc", TENSOR)
        assert info.value.class_name == "LimitExceeded"
        with pytest.raises(ServiceError) as info:
            service.restore_tensor("svc", "missing")
        assert info.value.class_name == "NotFound"
        service.write_file("/checkpoints/svc/ok", b"x")
        assert service.stat_file("/checkpoints/svc/ok") == (True, 1)


def test_unavailable_cli_is_a_typed_failure(tmp_path):
    with pytest.raises(ServiceUnavailable):
        CheckpointService(tmp_path / "store", cli=os.fspath(tmp_path / "nope"))
