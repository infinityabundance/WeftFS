"""A faster encoder for canonical WCB bytes (WEFTCODEC-FORMAT §8).

``weftcodec.wcb.encode`` is the **reference oracle**: a straightforward,
allocation-heavy tree walk that defines the format. This module emits the same
bytes with the same limits and the same rejection classes, but with:

* exact-type dispatch instead of a chain of ``isinstance`` tests (subclasses
  fall through to a slow path that mirrors the oracle);
* encoded map keys and small varints cached, so the same key is utf-8 encoded,
  length-prefixed and sorted once rather than once per occurrence;
* output collected as fragments and joined once, instead of one
  ``bytearray.append`` per byte of every varint and tag.

Nothing here changes the wire format. ``tests/test_wcb_fast.py`` differentially
checks this encoder against ``weftcodec.wcb.encode`` for every generated tree
and every generated invalid case; the DCP metadata encoder is checked against
``weftcodec.dcp.serialize_metadata`` separately.

The caches are process-local, bounded, and never carry anything that affects
the emitted bytes: a cache miss recomputes the identical fragment.
"""

from __future__ import annotations

from operator import itemgetter

from . import varint
from .errors import LimitExceeded, Malformed
from .limits import DEFAULT_MAX_STRING_BYTES, Limits
from .wcb import (
    I64_MAX,
    I64_MIN,
    TAG_ARRAY,
    TAG_BYTES,
    TAG_FALSE,
    TAG_I64,
    TAG_MAP,
    TAG_NULL,
    TAG_TEXT,
    TAG_TIMESTAMP,
    TAG_TRUE,
    TAG_U64,
    U64_MAX,
    I64,
    Timestamp,
    U64,
)

#: Encoded text map keys and their raw byte length, keyed by the key string.
#: Bounded by ``_KEY_CACHE_MAX``; clearing is always safe (a miss recomputes).
#: A cached entry is only used without re-measuring when the configured
#: ``max_string_bytes`` is the default; a lowered limit re-checks the length, so
#: a cache hit can never bypass a limit.
_KEY_CACHE: "dict[str, tuple[bytes, int]]" = {}
_KEY_CACHE_MAX = 4096

#: Minimal varint bytes for small integers. The encoder's integers are mostly
#: lengths, offsets and small dimensions, so this covers the common case.
_SMALL_VARINT = {value: varint.encode_bytes(value) for value in range(1024)}

_TAG_NULL_B = bytes((TAG_NULL,))
_TAG_FALSE_B = bytes((TAG_FALSE,))
_TAG_TRUE_B = bytes((TAG_TRUE,))
_TAG_U64_B = bytes((TAG_U64,))
_TAG_I64_B = bytes((TAG_I64,))
_TAG_TEXT_B = bytes((TAG_TEXT,))
_TAG_BYTES_B = bytes((TAG_BYTES,))
_TAG_ARRAY_B = bytes((TAG_ARRAY,))
_TAG_MAP_B = bytes((TAG_MAP,))
_TAG_TIMESTAMP_B = bytes((TAG_TIMESTAMP,))

_NONE = type(None)
_CODES = {
    _NONE: 0,
    bool: 1,
    U64: 2,
    I64: 3,
    Timestamp: 4,
    str: 5,
    bytes: 6,
    list: 7,
    tuple: 8,
    dict: 9,
}

_get_first = itemgetter(0)


def encode(value, limits: "Limits | None" = None) -> bytes:
    """Encode ``value`` to canonical WCB bytes (identical to ``wcb.encode``)."""
    limits = limits or Limits()
    out = []
    append = out.append
    key_cache = _KEY_CACHE
    small = _SMALL_VARINT
    max_items = limits.max_structured_items
    max_string = limits.max_string_bytes
    max_depth = limits.max_structured_depth
    default_string_limit = max_string == DEFAULT_MAX_STRING_BYTES
    counters = [0]

    def bump():
        counters[0] += 1
        if counters[0] > max_items:
            raise LimitExceeded(
                "WCB item count %d exceeds limit %d" % (counters[0], max_items)
            )

    def enter(depth):
        child = depth + 1
        if child > max_depth:
            raise LimitExceeded(
                "WCB nesting depth %d exceeds limit %d" % (child, max_depth)
            )
        return child

    def text(value):
        raw = value.encode("utf-8")
        length = len(raw)
        if length > max_string:
            raise LimitExceeded(
                "text length %d exceeds limit %d" % (length, max_string)
            )
        var = small.get(length)
        append(_TAG_TEXT_B)
        append(var if var is not None else varint.encode_bytes(length))
        append(raw)

    def emit(item, depth):
        bump()
        code = _CODES.get(type(item))
        if code is None:
            slow(item, depth)
        elif code == 5:
            text(item)
        elif code == 0:
            append(_TAG_NULL_B)
        elif code == 6:
            length = len(item)
            if length > max_string:
                raise LimitExceeded(
                    "bytes length %d exceeds limit %d" % (length, max_string)
                )
            var = small.get(length)
            append(_TAG_BYTES_B)
            append(var if var is not None else varint.encode_bytes(length))
            append(item)
        elif code == 9:
            child = enter(depth)
            count = len(item)
            if count > max_items:
                raise LimitExceeded("WCB map too large")
            rows = []
            row_append = rows.append
            for key in item:
                if type(key) is str:
                    cached = key_cache.get(key)
                    if cached is not None and (default_string_limit or cached[1] <= max_string):
                        encoded = cached[0]
                    else:
                        raw = key.encode("utf-8")
                        length = len(raw)
                        if length > max_string:
                            raise LimitExceeded(
                                "map key length %d exceeds limit %d"
                                % (length, max_string)
                            )
                        var = small.get(length)
                        if var is None:
                            encoded = _TAG_TEXT_B + varint.encode_bytes(length) + raw
                        else:
                            encoded = _TAG_TEXT_B + var + raw
                        if len(key_cache) >= _KEY_CACHE_MAX:
                            key_cache.clear()
                        key_cache[key] = (encoded, length)
                elif isinstance(key, str):
                    # A `str` subclass: accepted by the oracle, never cached.
                    raw = key.encode("utf-8")
                    length = len(raw)
                    if length > max_string:
                        raise LimitExceeded(
                            "map key length %d exceeds limit %d"
                            % (length, max_string)
                        )
                    encoded = _TAG_TEXT_B + varint.encode_bytes(length) + raw
                else:
                    raise Malformed("WCB map keys must be text")
                row_append((encoded, key))
            if count > 1:
                rows.sort(key=_get_first)
            # The oracle counts the header by the number of *iterated* keys, so
            # a mapping that yields a key twice would be a two-entry map there.
            count = len(rows)
            append(_TAG_MAP_B)
            var = small.get(count)
            append(var if var is not None else varint.encode_bytes(count))
            for encoded, key in rows:
                append(encoded)
                emit(item[key], child)
        elif code == 7 or code == 8:
            child = enter(depth)
            count = len(item)
            if count > max_items:
                raise LimitExceeded("WCB array too large")
            append(_TAG_ARRAY_B)
            var = small.get(count)
            append(var if var is not None else varint.encode_bytes(count))
            for element in item:
                emit(element, child)
        elif code == 2:
            number = item.value
            if number < 0 or number > U64_MAX:
                raise Malformed("U64 value is outside u64")
            append(_TAG_U64_B)
            var = small.get(number)
            append(var if var is not None else varint.encode_bytes(number))
        elif code == 3:
            number = item.value
            if number < I64_MIN or number > I64_MAX:
                raise Malformed("I64 value is outside i64")
            zigzag = varint.zigzag_encode(number)
            append(_TAG_I64_B)
            var = small.get(zigzag)
            append(var if var is not None else varint.encode_bytes(zigzag))
        elif code == 4:
            nanos = item.nanos
            if nanos < I64_MIN or nanos > I64_MAX:
                raise Malformed("Timestamp value is outside i64")
            zigzag = varint.zigzag_encode(nanos)
            append(_TAG_TIMESTAMP_B)
            var = small.get(zigzag)
            append(var if var is not None else varint.encode_bytes(zigzag))
        elif code == 1:
            append(_TAG_TRUE_B if item else _TAG_FALSE_B)
        else:  # pragma: no cover - every code above is handled
            raise AssertionError("unreachable WCB code")

    def slow(item, depth):
        """The oracle's decisions for every type outside the fast dispatch.

        This is only reached for subclasses, ``bytearray``/``memoryview`` and
        unrecognised objects, so it exists for exactness, not throughput. It
        shares the caller's limits, item counter and depth check.
        """
        if item is None:
            append(_TAG_NULL_B)
        elif item is False:
            append(_TAG_FALSE_B)
        elif item is True:
            append(_TAG_TRUE_B)
        elif isinstance(item, U64):
            number = item.value
            if number < 0 or number > U64_MAX:
                raise Malformed("U64 value is outside u64")
            append(_TAG_U64_B)
            append(varint.encode_bytes(number))
        elif isinstance(item, I64):
            number = item.value
            if number < I64_MIN or number > I64_MAX:
                raise Malformed("I64 value is outside i64")
            append(_TAG_I64_B)
            append(varint.encode_bytes(varint.zigzag_encode(number)))
        elif isinstance(item, Timestamp):
            nanos = item.nanos
            if nanos < I64_MIN or nanos > I64_MAX:
                raise Malformed("Timestamp value is outside i64")
            append(_TAG_TIMESTAMP_B)
            append(varint.encode_bytes(varint.zigzag_encode(nanos)))
        elif isinstance(item, str):
            text(item)
        elif isinstance(item, (bytes, bytearray, memoryview)):
            raw = bytes(item)
            length = len(raw)
            if length > max_string:
                raise LimitExceeded(
                    "bytes length %d exceeds limit %d" % (length, max_string)
                )
            append(_TAG_BYTES_B)
            append(varint.encode_bytes(length))
            append(raw)
        elif isinstance(item, (list, tuple)):
            child = enter(depth)
            count = len(item)
            if count > max_items:
                raise LimitExceeded("WCB array too large")
            append(_TAG_ARRAY_B)
            append(varint.encode_bytes(count))
            for element in item:
                emit(element, child)
        elif isinstance(item, dict):
            child = enter(depth)
            count = len(item)
            if count > max_items:
                raise LimitExceeded("WCB map too large")
            rows = []
            for key in item:
                if not isinstance(key, str):
                    raise Malformed("WCB map keys must be text")
                raw = key.encode("utf-8")
                if len(raw) > max_string:
                    raise LimitExceeded(
                        "map key length %d exceeds limit %d"
                        % (len(raw), max_string)
                    )
                rows.append((varint.encode_bytes(len(raw)), raw, key))
            rows.sort(key=itemgetter(1))
            append(_TAG_MAP_B)
            append(varint.encode_bytes(len(rows)))
            for length_var, raw, key in rows:
                append(_TAG_TEXT_B)
                append(length_var)
                append(raw)
                emit(item[key], child)
        else:
            raise Malformed(
                "value of type %s is not representable in WCB"
                % type(item).__name__
            )

    emit(value, 0)
    return b"".join(out)


def clear_caches():
    """Drop the encoded-key cache (a measurement and test hook)."""
    _KEY_CACHE.clear()
