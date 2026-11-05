"""Canonical WCB tests against the Rust-generated structured vectors."""

from __future__ import annotations

import pytest

from support import VECTORS, load_json

from weftcodec import wcb
from weftcodec.errors import WeftError
from weftcodec.limits import Limits

STRUCTURED_DIR = VECTORS / "structured"


def _files():
    return sorted(STRUCTURED_DIR.glob("*.json"))


def _ids():
    return [path.stem for path in _files()]


def _limits(vector: dict) -> Limits:
    return Limits.from_overrides(vector.get("limits"))


@pytest.mark.parametrize("path", _files(), ids=_ids())
def test_structured_vector(path):
    vector = load_json(path)
    name = vector.get("name", path.stem)
    limits = _limits(vector)

    if "expect_error" in vector:
        raw = bytes.fromhex(vector["hex"])
        with pytest.raises(WeftError) as info:
            wcb.decode(raw, limits)
        assert info.value.error_class == vector["expect_error"], name
        return

    raw = bytes.fromhex(vector["canonical_hex"])
    value = wcb.decode(raw, limits)
    reencoded = wcb.encode(value, limits)
    assert reencoded == raw, "%s canonical bytes" % name


def test_explicit_scalars_and_map_order():
    # Map entries are sorted by full encoded key bytes, so insertion order is
    # irrelevant and the golden bytes are stable.
    value = {
        "c": None,
        "a": wcb.U64(1),
        "b": "two",
    }
    assert wcb.encode(value).hex() == "08030501610301050162050374776f05016300"
    assert wcb.decode(bytes.fromhex("08030501610301050162050374776f05016300")) == {
        "a": wcb.U64(1),
        "b": "two",
        "c": None,
    }


def test_signed_and_timestamp_roundtrip():
    for original in (-1, 0, 1, -(1 << 63), (1 << 63) - 1):
        encoded = wcb.encode(wcb.I64(original))
        assert wcb.decode(encoded) == wcb.I64(original)
    stamp = wcb.Timestamp(1700000000123456789)
    assert wcb.encode(stamp).hex() == "09aab4aed8c7bfce972f"
    assert wcb.decode(bytes.fromhex("09aab4aed8c7bfce972f")) == stamp


def test_duplicate_keys_are_rejected():
    # Two identical text keys "a" with values U64(1) and U64(2).
    with pytest.raises(WeftError) as info:
        wcb.decode(bytes.fromhex("080205016103010501610302"))
    assert info.value.error_class == "NonCanonical"


def test_non_text_map_key_rejected():
    with pytest.raises(WeftError) as info:
        wcb.decode(bytes.fromhex("080103010302"))
    assert info.value.error_class == "NonCanonical"


def test_trailing_bytes_rejected():
    with pytest.raises(WeftError) as info:
        wcb.decode(bytes.fromhex("0000"))
    assert info.value.error_class == "NonCanonical"


def test_non_minimal_varint_rejected():
    with pytest.raises(WeftError) as info:
        wcb.decode(bytes.fromhex("038000"))
    assert info.value.error_class == "NonCanonical"
