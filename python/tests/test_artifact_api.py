"""Regression tests for the shared artifact API's buffer lifetime.

T072 replaced the adapters' private `service.TensorBuffer` with
`artifact_api.ArtifactTensorBuffer`. The rewrite dropped the guard the older
buffer had: `mmap.close()` refuses with `BufferError` while any exported
pointer is alive, and a consumer is *supposed* to keep one -- `_to_target`
hands a numpy/JAX array built with `frombuffer`/`asarray` straight back to the
caller and documents that no extra host copy is made.

The aggregate's `adapters_receipts` court failed on exactly that:
`BufferError: cannot close exported pointers exist` raised out of
`OrbaxCheckpointHandler.restore`. These tests pin the semantics that close()
must not raise while a consumer still points into the mapping, and must still
unmap when nothing does.
"""

from __future__ import annotations

import gc
import os
from dataclasses import dataclass

import numpy as np
import pytest

from weftcodec.artifact_api import ArtifactTensorBuffer

PAYLOAD = bytes(range(64))


@dataclass
class _Entry:
    dtype: str = "u8"
    shape: tuple = (8, 8)
    content_root: str = "11" * 32
    logical_root: str = "22" * 32


def _buffer(payload: bytes = PAYLOAD) -> ArtifactTensorBuffer:
    """A restored tensor in a caller-owned memfd, as the CLI path builds it."""
    fd = os.memfd_create("weft-artifact-api-test")
    os.ftruncate(fd, len(payload))
    os.pwrite(fd, payload, 0)
    return ArtifactTensorBuffer(
        fd,
        len(payload),
        entry=_Entry(),
        plan_root=None,
        snapshot=None,
        provisional=False,
    )


def test_close_with_an_exported_view_keeps_the_mapping_alive():
    buffer = _buffer()
    exported = np.frombuffer(buffer.view, dtype=np.uint8)

    buffer.close()  # must not raise BufferError while `exported` is alive

    # The caller's array is still valid: the mapping outlives the buffer and is
    # released when the last exporter is collected.
    assert exported.tobytes() == PAYLOAD
    assert buffer._fd is None, "the descriptor is closed even when the mapping stays"

    del exported
    gc.collect()
    buffer.close()
    assert buffer._mm is None, "with no exporter the mapping is released"


def test_close_unmaps_when_nothing_exports_the_mapping():
    buffer = _buffer()
    buffer.close()
    assert buffer._mm is None
    with pytest.raises(TypeError):
        buffer.view  # a released mapping cannot export


def test_close_is_idempotent():
    buffer = _buffer()
    buffer.close()
    buffer.close()
    assert buffer._fd is None
    assert buffer._mm is None
