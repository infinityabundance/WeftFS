"""PyTorch-facing dataset tests. Skipped when torch is not installed."""

from __future__ import annotations

import subprocess
import sys

import pytest

torch = pytest.importorskip("torch")

from weftcodec.torch import WeftMapDataset


def _make_tree(root):
    root.mkdir(parents=True, exist_ok=True)
    (root / "b.bin").write_bytes(b"BBBB")
    (root / "a.bin").write_bytes(b"A")
    nested = root / "sub"
    nested.mkdir()
    (nested / "c.bin").write_bytes(b"CCC")
    return root


def test_dataset_ordering_and_bytes(tmp_path):
    root = _make_tree(tmp_path / "mount")
    dataset = WeftMapDataset(root)
    assert len(dataset) == 3
    assert dataset.paths() == ["a.bin", "b.bin", "sub/c.bin"]

    path, payload = dataset[0]
    assert path == "a.bin"
    assert payload == b"A"
    assert isinstance(payload, bytes)

    path, payload = dataset[-1]
    assert path == "sub/c.bin"
    assert payload == b"CCC"

    with pytest.raises(IndexError):
        dataset[3]


def test_dataset_transform(tmp_path):
    root = _make_tree(tmp_path / "mount")
    dataset = WeftMapDataset(root, transform=lambda data: data + b"!")
    path, payload = dataset[1]
    assert path == "b.bin"
    assert payload == b"BBBB!"


def test_dataset_tensor_shape(tmp_path):
    root = _make_tree(tmp_path / "mount")
    dataset = WeftMapDataset(root, dtype=torch.uint8)
    path, tensor = dataset[1]
    assert path == "b.bin"
    assert isinstance(tensor, torch.Tensor)
    assert tensor.dtype == torch.uint8
    assert tuple(tensor.shape) == (4,)
    assert tensor.tolist() == [ord("B")] * 4

    shaped = WeftMapDataset(root, dtype=torch.uint8, shape=(2, 2))
    _, matrix = shaped[1]
    assert tuple(matrix.shape) == (2, 2)


def test_torch_is_not_imported_by_the_package_itself(tmp_path):
    # Use an explicit small root: walking the repository root can cross large
    # build/cache trees and is not what this test is about.
    root = _make_tree(tmp_path / "mount")
    script = (
        "import sys; import weftcodec.torch; "
        "print('torch' in sys.modules); "
        "d = weftcodec.torch.WeftMapDataset(sys.argv[1]); print(len(d))"
    )
    proc = subprocess.run(
        [sys.executable, "-c", script, str(root)],
        capture_output=True,
        text=True,
        check=True,
    )
    first = proc.stdout.strip().splitlines()[0]
    # `import weftcodec.torch` must not import the optional dependency.
    assert first == "False", proc.stdout
    assert proc.stdout.strip().splitlines()[1] == "3", proc.stdout
