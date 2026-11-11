"""Differential tests for the fast WCB encoder (R12c item 3).

``weftcodec.wcb.encode`` is the reference oracle. ``weftcodec.wcb_fast.encode``
must emit byte-identical output for every representable tree and must raise the
same error class for every invalid one.

Coverage:

* 5 deterministic seeds x 200 randomly generated trees (nested maps, byte
  strings, unicode strings, ints at every varint boundary, empty containers,
  sort-order-sensitive maps, deep nesting);
* values at the u64/i64/varint limits, and one-byte-below/above each boundary;
* subclasses, ``bytearray`` and ``memoryview``, which the oracle accepts through
  ``isinstance``;
* invalid values (floats, sets, complex, out-of-range integers, non-text map
  keys, unrecognised objects) with class equality;
* lowered limits (items, string bytes, depth) with class equality;
* a mapping that yields one key twice: both encoders must emit the same
  (non-canonical) bytes, and the canonical decoder must reject them.

The tests do not touch the store or the Rust CLI; they run in any court with
`blake3`/`zstandard` installed, and in the Docker python court as part of the
full suite.
"""

from __future__ import annotations

import random

import pytest

from weftcodec import wcb, wcb_fast
from weftcodec.errors import LimitExceeded, Malformed, NonCanonical
from weftcodec.limits import Limits

SEEDS = (1, 2, 3, 4, 5)
TREES_PER_SEED = 200

VARINT_BOUNDARIES = (
    0,
    1,
    127,
    128,
    129,
    255,
    256,
    16383,
    16384,
    16385,
    (1 << 21) - 1,
    1 << 21,
    (1 << 28) - 1,
    1 << 28,
    (1 << 35) - 1,
    1 << 35,
    (1 << 49) - 1,
    1 << 49,
    (1 << 63) - 1,
    1 << 63,
    (1 << 64) - 1,
    1 << 64,
)

U64_BOUNDARIES = tuple(value for value in VARINT_BOUNDARIES if value <= (1 << 64) - 1)

UNICODE_STRINGS = (
    "",
    "a",
    "ascii-key",
    "café",
    "日本語",
    "emoji-🧵-key",
    "\u0000",
    "mixed-é-日-🧵",
)


def _random_scalar(rng):
    kind = rng.randrange(9)
    if kind == 0:
        return None
    if kind == 1:
        return bool(rng.getrandbits(1))
    if kind == 2:
        return wcb.U64(rng.choice(U64_BOUNDARIES))
    if kind == 3:
        return wcb.I64(
            rng.choice(
                (
                    0,
                    1,
                    -1,
                    63,
                    64,
                    -64,
                    -65,
                    (1 << 62),
                    -(1 << 62),
                    (1 << 63) - 1,
                    -(1 << 63),
                )
            )
        )
    if kind == 4:
        return wcb.Timestamp(rng.choice((-1, 0, 1, 1_700_000_000_000_000_000)))
    if kind == 5:
        return rng.choice(UNICODE_STRINGS)
    if kind == 6:
        return bytes(rng.getrandbits(8) for _ in range(rng.randrange(0, 9)))
    if kind == 7:
        return "x" * rng.choice((1, 2, 127, 128, 300))
    return bytes(rng.randrange(256) for _ in range(rng.choice((0, 1, 32, 200))))


def _random_key(rng):
    return rng.choice(
        (
            "a",
            "b",
            "aa",
            "ab",
            "b" * 128,
            "café",
            "日本語",
            "key%d" % 7,
            "z",
        )
    )


def _random_value(rng, depth=0):
    if depth >= 5:
        return _random_scalar(rng)
    kind = rng.randrange(10)
    if kind < 6:
        return _random_scalar(rng)
    if kind < 8:
        return [_random_value(rng, depth + 1) for _ in range(rng.randrange(0, 5))]
    count = rng.randrange(0, 6)
    return {_random_key(rng): _random_value(rng, depth + 1) for _ in range(count)}


def test_random_trees_are_byte_identical():
    """5 seeds x 200 trees: the fast encoder matches the oracle exactly."""
    compared = 0
    for seed in SEEDS:
        rng = random.Random(seed)
        for _ in range(TREES_PER_SEED):
            value = _random_value(rng)
            expected = wcb.encode(value)
            actual = wcb_fast.encode(value)
            assert actual == expected, "seed %d tree %r differs" % (seed, value)
            # A byte match is necessary but not sufficient: the result must also
            # be a canonical value the decoder accepts and round-trips.
            decoded = wcb.decode(actual)
            assert wcb.encode(decoded) == expected
            compared += 1
    assert compared == len(SEEDS) * TREES_PER_SEED


def test_deeply_nested_and_wide_containers():
    wide = {"k%04d" % index: wcb.U64(index) for index in range(400)}
    nested = wide
    for _ in range(10):
        nested = {"level": nested, "list": [nested]}
    assert wcb_fast.encode(wide) == wcb.encode(wide)
    assert wcb_fast.encode(nested) == wcb.encode(nested)
    empty = {"empty-map": {}, "empty-array": [], "null": None}
    assert wcb_fast.encode(empty) == wcb.encode(empty)


def test_varint_boundaries_are_identical():
    for boundary in VARINT_BOUNDARIES:
        if boundary <= (1 << 64) - 1:
            assert wcb_fast.encode(wcb.U64(boundary)) == wcb.encode(wcb.U64(boundary))
        signed = boundary if boundary < (1 << 63) else (1 << 63) - 1
        for value in (signed, -signed):
            expected = wcb.encode(wcb.I64(value))
            assert wcb_fast.encode(wcb.I64(value)) == expected
        assert wcb_fast.encode(wcb.Timestamp(signed)) == wcb.encode(
            wcb.Timestamp(signed)
        )


def test_subclasses_bytearray_and_memoryview_match_the_oracle():
    class StrSub(str):
        pass

    class BytesSub(bytes):
        pass

    class ListSub(list):
        pass

    class DictSub(dict):
        pass

    values = [
        StrSub("subclass-key"),
        BytesSub(b"subclass-bytes"),
        ListSub([wcb.U64(1), StrSub("x")]),
        DictSub({"z": wcb.U64(1), "a": wcb.U64(2)}),
        bytearray(b"mutable"),
        memoryview(b"viewed"),
        {"k": bytearray(b"nested")},
    ]
    for value in values:
        assert wcb_fast.encode(value) == wcb.encode(value), value


INVALID_VALUES = [
    1.5,
    float("nan"),
    {1, 2},
    frozenset({1}),
    complex(1, 2),
    object(),
    wcb.U64(-1),
    wcb.U64(1 << 64),
    wcb.I64(1 << 63),
    wcb.I64(-(1 << 63) - 1),
    wcb.Timestamp(1 << 63),
    {"k": 2.5},
    [object()],
]


@pytest.mark.parametrize("value", INVALID_VALUES)
def test_invalid_values_raise_the_same_class(value):
    with pytest.raises(Exception) as oracle:
        wcb.encode(value)
    with pytest.raises(Exception) as fast:
        wcb_fast.encode(value)
    assert type(fast.value) is type(oracle.value), (type(fast.value), type(oracle.value))
    assert isinstance(fast.value, Malformed)


def test_non_text_map_keys_raise_the_same_class():
    for mapping in ({1: "x"}, {b"x": 1}, {None: 1}, {(1, 2): "y"}):
        with pytest.raises(Malformed):
            wcb.encode(mapping)
        with pytest.raises(Malformed):
            wcb_fast.encode(mapping)


def test_duplicate_iteration_is_byte_identical_and_rejected_on_decode():
    """A mapping that yields one key twice: same bytes, non-canonical result."""

    class Duplicating(dict):
        def __iter__(self):
            yield "dup"
            yield "dup"
        def __len__(self):
            return 2

    value = Duplicating({"dup": wcb.U64(7)})
    expected = wcb.encode(value)
    actual = wcb_fast.encode(value)
    assert actual == expected
    with pytest.raises(NonCanonical):
        wcb.decode(actual)


@pytest.mark.parametrize(
    "limits",
    [
        Limits(max_structured_items=1),
        Limits(max_structured_items=5),
        Limits(max_structured_items=50),
        Limits(max_string_bytes=4),
        Limits(max_string_bytes=0),
        Limits(max_structured_depth=1),
        Limits(max_structured_depth=2),
        Limits(max_structured_depth=3),
    ],
)
def test_lowered_limits_raise_the_same_class(limits):
    trees = [
        {"a": wcb.U64(1)},
        {"a": {"b": [wcb.U64(1), wcb.U64(2)]}},
        {"key": "value"},
        "long-string-value",
        [wcb.U64(1)] * 8,
    ]
    for tree in trees:
        oracle_error = None
        try:
            wcb.encode(tree, limits)
        except Exception as exc:  # noqa: BLE001 - the class is the assertion
            oracle_error = exc
        fast_error = None
        try:
            wcb_fast.encode(tree, limits)
        except Exception as exc:  # noqa: BLE001
            fast_error = exc
        if oracle_error is None:
            assert fast_error is None, (tree, limits, fast_error)
        else:
            assert fast_error is not None, (tree, limits)
            assert type(fast_error) is type(oracle_error), (
                tree,
                limits,
                type(fast_error),
                type(oracle_error),
            )
            assert isinstance(fast_error, LimitExceeded)


def test_limits_are_not_leaked_by_the_key_cache():
    """The key cache must not let a lowered limit accept an oversized key."""
    wcb_fast.encode({"a-long-key": wcb.U64(1)})
    with pytest.raises(LimitExceeded):
        wcb_fast.encode({"a-long-key": wcb.U64(1)}, Limits(max_string_bytes=2))
    with pytest.raises(LimitExceeded):
        wcb.encode({"a-long-key": wcb.U64(1)}, Limits(max_string_bytes=2))


def test_module_contract():
    assert callable(wcb_fast.encode)
    assert wcb_fast.clear_caches() is None
    # Clearing the cache never changes the bytes.
    value = {"k": [wcb.U64(1), {"n": "café"}]}
    before = wcb_fast.encode(value)
    wcb_fast.clear_caches()
    assert wcb_fast.encode(value) == before == wcb.encode(value)
