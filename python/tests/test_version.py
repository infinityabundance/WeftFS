"""The advertised version must match the package metadata.

`weftcodec` is imported from the source tree in the courts, where it is not
pip-installed, so ``importlib.metadata.version("weftcodec")`` raises
``PackageNotFoundError``. ``python/pyproject.toml`` is therefore the
authoritative source in both cases; when distribution metadata *is* available
(a built wheel or an editable install) it must agree as well.

This test exists because ``weftcodec.__version__`` was left at ``1.1.0``
through the 1.2.0 release.
"""

from __future__ import annotations

import importlib.metadata
from pathlib import Path

import pytest

import weftcodec

PYPROJECT = Path(__file__).resolve().parents[1] / "pyproject.toml"


def _pyproject_version() -> str:
    try:
        import tomllib
    except ModuleNotFoundError:  # Python 3.9/3.10
        pytest.skip("tomllib is unavailable; the version check needs Python 3.11+")
    with PYPROJECT.open("rb") as handle:
        return tomllib.load(handle)["project"]["version"]


def test_dunder_version_matches_pyproject():
    assert weftcodec.__version__ == _pyproject_version()


def test_distribution_metadata_matches_when_installed():
    try:
        installed = importlib.metadata.version("weftcodec")
    except importlib.metadata.PackageNotFoundError:
        pytest.skip("weftcodec is imported from the source tree, not installed")
    assert installed == weftcodec.__version__
