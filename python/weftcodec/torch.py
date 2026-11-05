"""PyTorch-facing dataset over a mounted WeftFS directory.

``torch`` is an optional dependency and is never imported at module import time.
Plain ``bytes`` access works without it; requesting a tensor raises a clear
``ImportError`` when PyTorch is absent.
"""

from __future__ import annotations

import importlib
import os
from pathlib import Path


def _torch_module():
    try:
        return importlib.import_module("torch")
    except ImportError as exc:  # pragma: no cover - exercised without torch
        raise ImportError(
            "weftcodec.torch requires PyTorch; install it with "
            "`pip install weftcodec[torch]` (or `pip install torch`)"
        ) from exc


class WeftMapDataset:
    """Deterministic map-style dataset over files in a WeftFS mount.

    Parameters
    ----------
    root:
        Directory served by a WeftFS mount (or any directory).
    transform:
        Optional callable applied to the raw ``bytes`` before the item is
        returned or converted to a tensor.
    dtype:
        When set (for example ``torch.uint8`` or ``torch.float32``), items are
        returned as ``torch.frombuffer`` tensors instead of ``bytes``.
    shape:
        Optional shape used to reshape a returned tensor. ``-1`` can be used
        for one inferred dimension.
    """

    def __init__(
        self,
        root: "str | os.PathLike[str]",
        transform=None,
        *,
        dtype=None,
        shape: "tuple[int, ...] | None" = None,
    ) -> None:
        self.root = Path(root)
        if not self.root.is_dir():
            raise NotADirectoryError("dataset root is not a directory: %s" % self.root)
        self.transform = transform
        self.dtype = dtype
        self.shape = tuple(shape) if shape is not None else None
        self._entries = self._walk()

    def _walk(self):
        entries = []
        for dirpath, dirnames, filenames in os.walk(self.root):
            dirnames.sort()
            for name in sorted(filenames):
                full = Path(dirpath) / name
                if full.is_file():
                    relative = full.relative_to(self.root).as_posix()
                    entries.append((relative, full))
        entries.sort(key=lambda entry: entry[0])
        return entries

    def __len__(self) -> int:
        return len(self._entries)

    def paths(self) -> "list[str]":
        return [relative for relative, _ in self._entries]

    def __getitem__(self, index: int):
        if index < 0:
            index += len(self._entries)
        if index < 0 or index >= len(self._entries):
            raise IndexError("dataset index %d out of range" % index)
        relative, full = self._entries[index]
        with open(full, "rb") as handle:
            data = handle.read()
        if self.transform is not None:
            data = self.transform(data)
        if self.dtype is None:
            return relative, data
        torch = _torch_module()
        # `bytes` is read-only; torch.frombuffer needs a writable buffer.
        tensor = torch.frombuffer(bytearray(data), dtype=self.dtype)
        if self.shape is not None:
            tensor = tensor.reshape(self.shape)
        return relative, tensor
