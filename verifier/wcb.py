"""Canonical WCB decoding and encoding, written from `WEFTCODEC-FORMAT.md` §8.

This module is deliberately independent of every WeftFS package. It models the
wire value with explicit wrappers so a `u64`, an `i64`, a timestamp, text and
bytes can never be confused during re-encoding.

Rules enforced (all from the spec text):
  * tags 0x00..0x09 only; 0x0A..0xFF is `NonCanonical`;
  * minimal unsigned LEB128 varints, at most 10 bytes, tenth byte 0 or 1;
  * map keys are text and are strictly ascending by the **full encoded key
    bytes**; duplicates and out-of-order keys are `NonCanonical`;
  * no trailing bytes after one value;
  * depth, item count and string length are bounded before allocation;
  * text must be valid UTF-8;
  * truncation is `Malformed`.
"""

from __future__ import annotations

from dataclasses import dataclass

MAX_DEPTH = 64
MAX_ITEMS = 1_000_000
MAX_STRING_BYTES = 256 * 1024 * 1024


class WcbError(Exception):
    """A typed WCB refusal. `klass` is the interoperable error class."""

    def __init__(self, klass: str, message: str):
        super().__init__(message)
        self.klass = klass
        self.message = message

    def __str__(self):
        return "%s: %s" % (self.klass, self.message)


class U64:
    __slots__ = ("value",)

    def __init__(self, value: int):
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise ValueError("U64 needs a non-negative int")
        if value > 0xFFFFFFFFFFFFFFFF:
            raise ValueError("U64 out of range")
        self.value = value

    def __eq__(self, other):
        return isinstance(other, U64) and other.value == self.value

    def __hash__(self):
        return hash(("u64", self.value))

    def __repr__(self):
        return "U64(%d)" % self.value


class I64:
    __slots__ = ("value",)

    def __init__(self, value: int):
        if not isinstance(value, int) or isinstance(value, bool):
            raise ValueError("I64 needs an int")
        if not -(1 << 63) <= value <= (1 << 63) - 1:
            raise ValueError("I64 out of range")
        self.value = value

    def __eq__(self, other):
        return isinstance(other, I64) and other.value == self.value

    def __hash__(self):
        return hash(("i64", self.value))

    def __repr__(self):
        return "I64(%d)" % self.value


class Timestamp:
    __slots__ = ("value",)

    def __init__(self, value: int):
        if not isinstance(value, int) or isinstance(value, bool):
            raise ValueError("Timestamp needs an int")
        if not -(1 << 63) <= value <= (1 << 63) - 1:
            raise ValueError("Timestamp out of range")
        self.value = value

    def __eq__(self, other):
        return isinstance(other, Timestamp) and other.value == self.value

    def __hash__(self):
        return hash(("ts", self.value))

    def __repr__(self):
        return "Timestamp(%d)" % self.value


class Bytes:
    __slots__ = ("value",)

    def __init__(self, value: bytes):
        if not isinstance(value, (bytes, bytearray)):
            raise ValueError("Bytes needs bytes")
        self.value = bytes(value)

    def __eq__(self, other):
        return isinstance(other, Bytes) and other.value == self.value

    def __hash__(self):
        return hash(("bytes", self.value))

    def __repr__(self):
        return "Bytes(%d)" % len(self.value)


def _zigzag_encode(value: int) -> int:
    return ((value << 1) ^ (value >> 63)) & 0xFFFFFFFFFFFFFFFF


def _zigzag_decode(value: int) -> int:
    return (value >> 1) ^ -(value & 1)


def _varint_encode(value: int) -> bytes:
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def _key_bytes(key: str) -> bytes:
    raw = key.encode("utf-8")
    return b"\x05" + _varint_encode(len(raw)) + raw


def encode(value) -> bytes:
    out = bytearray()
    _encode_into(value, out)
    return bytes(out)


def _encode_into(value, out: bytearray) -> None:
    if value is None:
        out.append(0x00)
    elif value is False:
        out.append(0x01)
    elif value is True:
        out.append(0x02)
    elif isinstance(value, U64):
        out.append(0x03)
        out.extend(_varint_encode(value.value))
    elif isinstance(value, I64):
        out.append(0x04)
        out.extend(_varint_encode(_zigzag_encode(value.value)))
    elif isinstance(value, str):
        raw = value.encode("utf-8")
        out.append(0x05)
        out.extend(_varint_encode(len(raw)))
        out.extend(raw)
    elif isinstance(value, Bytes):
        out.append(0x06)
        out.extend(_varint_encode(len(value.value)))
        out.extend(value.value)
    elif isinstance(value, Timestamp):
        out.append(0x09)
        out.extend(_varint_encode(_zigzag_encode(value.value)))
    elif isinstance(value, list):
        out.append(0x07)
        out.extend(_varint_encode(len(value)))
        for item in value:
            _encode_into(item, out)
    elif isinstance(value, dict):
        encoded = [(_key_bytes(k), v) for k, v in value.items()]
        encoded.sort(key=lambda pair: pair[0])
        out.append(0x08)
        out.extend(_varint_encode(len(encoded)))
        for key, item in encoded:
            out.extend(key)
            _encode_into(item, out)
    else:
        raise TypeError("cannot encode %r" % (value,))


class Limits:
    __slots__ = ("max_depth", "max_items", "max_string_bytes")

    def __init__(self, max_depth=MAX_DEPTH, max_items=MAX_ITEMS,
                 max_string_bytes=MAX_STRING_BYTES):
        self.max_depth = max_depth
        self.max_items = max_items
        self.max_string_bytes = max_string_bytes


class _Reader:
    __slots__ = ("data", "pos", "limits")

    def __init__(self, data: bytes, limits: Limits):
        self.data = data
        self.pos = 0
        self.limits = limits

    def _byte(self) -> int:
        if self.pos >= len(self.data):
            raise WcbError("Malformed", "truncated at byte %d" % self.pos)
        byte = self.data[self.pos]
        self.pos += 1
        return byte

    def _take(self, count: int) -> bytes:
        if count < 0:
            raise WcbError("Malformed", "negative length")
        if count > len(self.data) - self.pos:
            raise WcbError("Malformed", "truncated: need %d at %d" % (count, self.pos))
        chunk = self.data[self.pos:self.pos + count]
        self.pos += count
        return chunk

    def _varint(self) -> int:
        value = 0
        shift = 0
        length = 0
        while True:
            byte = self._byte()
            length += 1
            if length > 10:
                raise WcbError("NonCanonical", "varint longer than 10 bytes")
            value |= (byte & 0x7F) << shift
            if not (byte & 0x80):
                break
            shift += 7
        if length > 1 and (self.data[self.pos - 1] & 0x7F) == 0:
            raise WcbError("NonCanonical", "non-minimal varint")
        if length == 10 and (self.data[self.pos - 1] & 0x7F) > 1:
            raise WcbError("NonCanonical", "tenth varint byte above 1")
        return value

    def value(self, depth: int):
        if depth > self.limits.max_depth:
            raise WcbError("LimitExceeded", "WCB depth above %d" % self.limits.max_depth)
        tag = self._byte()
        if tag == 0x00:
            return None
        if tag == 0x01:
            return False
        if tag == 0x02:
            return True
        if tag == 0x03:
            return U64(self._varint())
        if tag == 0x04:
            return I64(_zigzag_decode(self._varint()))
        if tag == 0x05:
            length = self._varint()
            if length > self.limits.max_string_bytes:
                raise WcbError("LimitExceeded", "text longer than the string bound")
            raw = self._take(length)
            try:
                return raw.decode("utf-8")
            except UnicodeDecodeError:
                raise WcbError("NonCanonical", "invalid UTF-8 text")
        if tag == 0x06:
            length = self._varint()
            if length > self.limits.max_string_bytes:
                raise WcbError("LimitExceeded", "bytes longer than the string bound")
            return Bytes(self._take(length))
        if tag == 0x07:
            count = self._varint()
            if count > self.limits.max_items:
                raise WcbError("LimitExceeded", "array above the item bound")
            return [self.value(depth + 1) for _ in range(count)]
        if tag == 0x08:
            count = self._varint()
            if count > self.limits.max_items:
                raise WcbError("LimitExceeded", "map above the item bound")
            result = {}
            previous = None
            for _ in range(count):
                if self._byte() != 0x05:
                    raise WcbError("NonCanonical", "map key is not text")
                length = self._varint()
                if length > self.limits.max_string_bytes:
                    raise WcbError("LimitExceeded", "map key longer than the bound")
                raw = self._take(length)
                try:
                    key = raw.decode("utf-8")
                except UnicodeDecodeError:
                    raise WcbError("NonCanonical", "invalid UTF-8 map key")
                encoded_key = b"\x05" + _varint_encode(length) + raw
                if previous is not None:
                    if encoded_key == previous:
                        raise WcbError("NonCanonical", "duplicate map key")
                    if encoded_key < previous:
                        raise WcbError("NonCanonical", "unsorted map keys")
                previous = encoded_key
                result[key] = self.value(depth + 1)
            return result
        if tag == 0x09:
            return Timestamp(_zigzag_decode(self._varint()))
        raise WcbError("NonCanonical", "unknown WCB tag 0x%02x" % tag)


def decode(data: bytes, limits: Limits | None = None):
    """Decode exactly one canonical WCB value. Raises `WcbError`."""
    if not isinstance(data, (bytes, bytearray)):
        raise TypeError("decode expects bytes")
    reader = _Reader(bytes(data), limits or Limits())
    value = reader.value(0)
    if reader.pos != len(reader.data):
        raise WcbError("NonCanonical", "trailing bytes after the value")
    return value


def decode_and_reencode(data: bytes, limits: Limits | None = None):
    """Decode, re-encode, and require byte identity. Returns the value."""
    value = decode(data, limits)
    if encode(value) != bytes(data):
        raise WcbError("NonCanonical", "record does not re-encode to the same bytes")
    return value


def expect_map(value, keys, where: str) -> dict:
    """Require a map with exactly `keys` (set equality), else Malformed."""
    if not isinstance(value, dict):
        raise WcbError("Malformed", "%s is not a WCB map" % where)
    if set(value) != set(keys):
        missing = sorted(set(keys) - set(value))
        extra = sorted(set(value) - set(keys))
        raise WcbError(
            "Malformed",
            "%s key set differs (missing %s, extra %s)" % (where, missing, extra),
        )
    return value


def expect_text(value, where: str) -> str:
    if not isinstance(value, str):
        raise WcbError("Malformed", "%s is not text" % where)
    return value


def expect_bytes(value, length: int | None, where: str) -> bytes:
    if not isinstance(value, Bytes):
        raise WcbError("Malformed", "%s is not bytes" % where)
    if length is not None and len(value.value) != length:
        raise WcbError("Malformed", "%s is not %d bytes" % (where, length))
    return value.value


def expect_u64(value, where: str) -> int:
    if not isinstance(value, U64):
        raise WcbError("Malformed", "%s is not a u64" % where)
    return value.value


def expect_array(value, where: str) -> list:
    if not isinstance(value, list):
        raise WcbError("Malformed", "%s is not an array" % where)
    return value


def expect_map_value(value, where: str) -> dict:
    if not isinstance(value, dict):
        raise WcbError("Malformed", "%s is not a map" % where)
    return value
