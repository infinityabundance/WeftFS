"""Unit tests for the R11 training-goodput harness helpers.

The harness itself is a court (`python/courts/training_goodput.py`), not part
of the shipped package. These tests cover the pieces a fresh-process resume
relies on: exact state capture/install, the deterministic dataset, loss-bit
encoding, and the fail-closed staging-byte bound. The full loop and the
SIGKILL court are run by `bench/scripts/training_goodput_court.sh`.
"""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path

import pytest

torch = pytest.importorskip("torch")

HARNESS_PATH = Path(__file__).resolve().parents[1] / "courts" / "training_goodput.py"


@pytest.fixture(scope="module", autouse=True)
def deterministic():
    torch.use_deterministic_algorithms(True)
    torch.set_num_threads(1)
    return None


@pytest.fixture(scope="module")
def harness():
    spec = importlib.util.spec_from_file_location("training_goodput_harness", HARNESS_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _trained(harness):
    torch.manual_seed(harness.SEED)
    model = harness.build_model(torch)
    optimizer = harness.make_optimizer(torch, model)
    batches = harness.data_batches(torch)
    for step in range(1, 4):
        harness.train_step(torch, model, optimizer, batches, step)
    return model, optimizer, batches


def test_capture_install_roundtrip_is_bit_exact(harness):
    model, optimizer, batches = _trained(harness)
    flat = harness.capture_state(torch, model, optimizer, 3)

    torch.manual_seed(999)  # a different initialisation must be overwritten
    model2 = harness.build_model(torch)
    optimizer2 = harness.make_optimizer(torch, model2)
    harness.install_state(torch, model2, optimizer2, flat, 3)

    restored = harness.capture_state(torch, model2, optimizer2, 3)
    assert harness.flat_sha256(torch, restored) == harness.flat_sha256(torch, flat)
    assert set(restored) == set(flat)

    loss_a = harness.train_step(torch, model, optimizer, batches, 4)
    loss_b = harness.train_step(torch, model2, optimizer2, batches, 4)
    assert harness.loss_bits(loss_a) == harness.loss_bits(loss_b)
    assert harness.flat_sha256(
        torch, harness.capture_state(torch, model, optimizer, 4)
    ) == harness.flat_sha256(
        torch, harness.capture_state(torch, model2, optimizer2, 4)
    )


def test_dataset_is_deterministic_and_next_token(harness):
    first = harness.data_batches(torch)
    second = harness.data_batches(torch)
    assert len(first) == len(second) == harness.N_BATCHES
    for (x1, y1), (x2, y2) in zip(first, second):
        assert torch.equal(x1, x2)
        assert torch.equal(y1, y2)
        # Targets are the next token of the same sequence.
        assert torch.equal(y1[:, :-1], x1[:, 1:])


def test_tensor_hash_and_loss_bits(harness):
    tensor = torch.tensor([1.0, -2.5], dtype=torch.float32)
    digest = harness.tensor_sha256(tensor)
    assert len(digest) == 64
    assert digest == harness.tensor_sha256(tensor.clone())
    assert harness.loss_bits(torch.tensor(1.25)) == 0x3FA00000


def test_staging_bound_is_fail_closed(harness, tmp_path):
    from weftcodec.dcp import init_store, save_checkpoint_async

    cli = Path(os.environ.get("WEFTFS_CLI", harness.DEFAULT_CLI))
    if not (cli.is_file() and os.access(cli, os.X_OK)):
        pytest.skip("weftfs CLI not present at %s" % cli)

    model, optimizer, _batches = _trained(harness)
    snapshot = harness.capture_state(torch, model, optimizer, 3)
    store = tmp_path / "store"
    init_store(store, weftfs_cli=cli)

    # DCP wraps storage failures in a BaseException subclass, so catch broadly
    # and require the exact bound message rather than a specific Python type.
    with pytest.raises(BaseException) as caught:
        handle = save_checkpoint_async(
            snapshot,
            checkpoint_id="too-small",
            store_root=str(store),
            staging_bytes=1024,
            weftfs_cli=cli,
        )
        handle.result()
    message = str(caught.value)
    assert "StagingLimitExceeded" in message
    assert "staging_bytes bound" in message

    handle = save_checkpoint_async(
        snapshot,
        checkpoint_id="within-bound",
        store_root=str(store),
        staging_bytes=64 << 20,
        weftfs_cli=cli,
    )
    handle.result()
    try:
        assert handle.stats["staging_peak_bytes"] > 1024
        assert handle.stats["entries"] == len(snapshot)
    finally:
        handle.writer.close()
