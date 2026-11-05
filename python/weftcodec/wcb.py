"""Canonical WCB bytes (WEFTCODEC-FORMAT §8).

WCB is a strict, explicit subset of deterministic CBOR. Python values map as:

===============  ==========================================
Python           WCB
===============  ==========================================
``None``         null (tag 0x00)
``False``        false (0x01)
``True``         true (0x02)
``U64(n)``       unsigned int (0x03)
``I64(n)``       signed int (0x04)
``str``          text (0x05), UTF-8
``bytes``        bytes (0x06)
``list``/tuple   array (0x07)
``dict``         map (0x08), text keys only
``Timestamp(n)`` timestamp (0x09), i64 nanoseconds since the epoch
===============  ==========================================

Tag ``0x0A..0xFF`` is ``NonCanonical`` in v1. Map keys are sorted by their
full encoded bytes, duplicates are rejected, and varints are minimal.
"""

from __future__ import annotations

from dataclasses import dataclass

from . import varint
from .errors import LimitExceeded, Malformed, NonCanonical
from .limits import Limits

TAG_NULL = 0x00
TAG_FALSE = 0x01
TAG_TRUE = 0x02
TAG_U64 = 0x03
TAG_I64 = 0x04
TAG_TEXT = 0x05
TAG_BYTES = 0x06
TAG_ARRAY = 0x07
TAG_MAP = 0x08
TAG_TIMESTAMP = 0x09

I64_MIN = -(1 << 63)
I64_MAX = (1 << 63) - 1
U64_MAX = (1 << 64) - 1


@dataclass(frozen=True)
class U64:
    """Unsigned 64-bit integer."""

    value: int


@dataclass(frozen=True)
class I64:
    """Signed 64-bit integer."""

    value: int


@dataclass(frozen=True)
class Timestamp:
    """Signed 64-bit nanoseconds since the Unix epoch, UTC."""

    nanos: int


def encode(value, limits: "Limits | None" = None) -> bytes:
    """Encode ``value`` to canonical WCB bytes."""
    encoder = _Encoder(limits or Limits())
    out = bytearray()
    encoder.value(value, 0, out)
    return bytes(out)


def decode(data: bytes, limits: "Limits | None" = None):
    """Decode canonical WCB bytes, rejecting every non-canonical form."""
    if not data:
        raise Malformed("empty WCB value")
    decoder = _Decoder(data, limits or Limits())
    value = decoder.value(0)
    if decoder.pos != len(data):
        raise NonCanonical(
            "trailing bytes after WCB value: %d of %d unconsumed"
            % (len(data) - decoder.pos, len(data))
        )
    return value


class _Encoder:
    __slots__ = ("limits", "items")

    def __init__(self, limits: Limits) -> None:
        self.limits = limits
        self.items = 0

    def _bump_item(self) -> None:
        self.items += 1
        if self.items > self.limits.max_structured_items:
            raise LimitExceeded(
                "WCB item count %d exceeds limit %d"
                % (self.items, self.limits.max_structured_items)
            )

    def _check_text(self, length: int, what: str) -> None:
        if length > self.limits.max_string_bytes:
            raise LimitExceeded(
                "%s length %d exceeds limit %d"
                % (what, length, self.limits.max_string_bytes)
            )

    def _enter(self, depth: int) -> int:
        nxt = depth + 1
        if nxt > self.limits.max_structured_depth:
            raise LimitExceeded(
                "WCB nesting depth %d exceeds limit %d"
                % (nxt, self.limits.max_structured_depth)
            )
        return nxt

    def value(self, value, depth: int, out: bytearray) -> None:
        self._bump_item()
        if value is None:
            out.append(TAG_NULL)
        elif value is False:
            out.append(TAG_FALSE)
        elif value is True:
            out.append(TAG_TRUE)
        elif isinstance(value, U64):
            if value.value < 0 or value.value > U64_MAX:
                raise Malformed("U64 value is outside u64")
            out.append(TAG_U64)
            varint.encode(value.value, out)
        elif isinstance(value, I64):
            if value.value < I64_MIN or value.value > I64_MAX:
                raise Malformed("I64 value is outside i64")
            out.append(TAG_I64)
            varint.encode(varint.zigzag_encode(value.value), out)
        elif isinstance(value, Timestamp):
            if value.nanos < I64_MIN or value.nanos > I64_MAX:
                raise Malformed("Timestamp value is outside i64")
            out.append(TAG_TIMESTAMP)
            varint.encode(varint.zigzag_encode(value.nanos), out)
        elif isinstance(value, str):
            raw = value.encode("utf-8")
            self._check_text(len(raw), "text")
            out.append(TAG_TEXT)
            varint.encode(len(raw), out)
            out.extend(raw)
        elif isinstance(value, (bytes, bytearray, memoryview)):
            raw = bytes(value)
            self._check_text(len(raw), "bytes")
            out.append(TAG_BYTES)
            varint.encode(len(raw), out)
            out.extend(raw)
        elif isinstance(value, (list, tuple)):
            child = self._enter(depth)
            if len(value) > self.limits.max_structured_items:
                raise LimitExceeded("WCB array too large")
            out.append(TAG_ARRAY)
            varint.encode(len(value), out)
            for item in value:
                self.value(item, child, out)
        elif isinstance(value, dict):
            child = self._enter(depth)
            if len(value) > self.limits.max_structured_items:
                raise LimitExceeded("WCB map too large")
            entries = []
            for key in value:
                if not isinstance(key, str):
                    raise Malformed("WCB map keys must be text")
                raw = key.encode("utf-8")
                self._check_text(len(raw), "map key")
                encoded_key = bytearray()
                encoded_key.append(TAG_TEXT)
                varint.encode(len(raw), encoded_key)
                encoded_key.extend(raw)
                entries.append((bytes(encoded_key), key))
            entries.sort(key=lambda entry: entry[0])
            out.append(TAG_MAP)
            varint.encode(len(entries), out)
            for encoded_key, key in entries:
                out.extend(encoded_key)
                self.value(value[key], child, out)
        else:
            raise Malformed("value of type %s is not representable in WCB" % type(value).__name__)


class _Decoder:
    __slots__ = ("input", "pos", "limits", "items")

    def __init__(self, data: bytes, limits: Limits) -> None:
        self.input = data
        self.pos = 0
        self.limits = limits
        self.items = 0

    def _bump_item(self) -> None:
        self.items += 1
        if self.items > self.limits.max_structured_items:
            raise LimitExceeded(
                "WCB item count %d exceeds limit %d"
                % (self.items, self.limits.max_structured_items)
            )

    def _read_u8(self) -> int:
        if self.pos >= len(self.input):
            raise Malformed("truncated WCB value")
        byte = self.input[self.pos]
        self.pos += 1
        return byte

    def _read_varint(self) -> int:
        value, self.pos = varint.decode(self.input, self.pos)
        return value

    def _remaining(self) -> int:
        return len(self.input) - self.pos

    def _take(self, length: int, what: str) -> bytes:
        if length > self.limits.max_string_bytes:
            raise LimitExceeded(
                "%s length %d exceeds limit %d"
                % (what, length, self.limits.max_string_bytes)
            )
        if length > self._remaining():
            raise Malformed("truncated WCB %s" % what)
        start = self.pos
        self.pos += length
        return self.input[start : self.pos]

    def _enter(self, depth: int) -> int:
        nxt = depth + 1
        if nxt > self.limits.max_structured_depth:
            raise LimitExceeded(
                "WCB nesting depth %d exceeds limit %d"
                % (nxt, self.limits.max_structured_depth)
            )
        return nxt

    def _check_count(self, count: int, min_bytes_each: int, what: str) -> int:
        if count > self.limits.max_structured_items:
            raise LimitExceeded(
                "WCB %s count %d exceeds limit %d"
                % (what, count, self.limits.max_structured_items)
            )
        if count * min_bytes_each > self._remaining():
            raise Malformed(
                "truncated WCB %s: count %d cannot fit in %d remaining bytes"
                % (what, count, self._remaining())
            )
        return count

    def value(self, depth: int):
        self._bump_item()
        tag = self._read_u8()
        if tag == TAG_NULL:
            return None
        if tag == TAG_FALSE:
            return False
        if tag == TAG_TRUE:
            return True
        if tag == TAG_U64:
            return U64(self._read_varint())
        if tag == TAG_I64:
            return I64(varint.zigzag_decode(self._read_varint()))
        if tag == TAG_TIMESTAMP:
            return Timestamp(varint.zigzag_decode(self._read_varint()))
        if tag == TAG_TEXT:
            raw = self._take(self._read_varint(), "text")
            try:
                return raw.decode("utf-8")
            except UnicodeDecodeError:
                raise NonCanonical("WCB text is not valid UTF-8")
        if tag == TAG_BYTES:
            return self._take(self._read_varint(), "bytes")
        if tag == TAG_ARRAY:
            child = self._enter(depth)
            count = self._check_count(self._read_varint(), 1, "array")
            return [self.value(child) for _ in range(count)]
        if tag == TAG_MAP:
            child = self._enter(depth)
            count = self._check_count(self._read_varint(), 2, "map")
            result = {}
            previous_key = None
            for _ in range(count):
                key_start = self.pos
                key_tag = self._read_u8()
                if key_tag != TAG_TEXT:
                    raise NonCanonical("WCB map key is not a text value (tag 0x05)")
                raw = self._take(self._read_varint(), "map key")
                key_end = self.pos
                if previous_key is not None:
                    previous = self.input[previous_key[0] : previous_key[1]]
                    current = self.input[key_start:key_end]
                    if current <= previous:
                        raise NonCanonical(
                            "WCB map keys are duplicated or not sorted by encoded key bytes"
                        )
                previous_key = (key_start, key_end)
                try:
                    key = raw.decode("utf-8")
                except UnicodeDecodeError:
                    raise NonCanonical("WCB map key is not valid UTF-8")
                if key in result:
                    raise NonCanonical("duplicate WCB map key")
                result[key] = self.value(child)
            return result
        raise NonCanonical("unknown WCB tag 0x%02x" % tag)
