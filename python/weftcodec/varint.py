"""Minimal unsigned LEB128 varints (WEFTCODEC-FORMAT §1).

The decoder mirrors the Rust implementation exactly:

* at most 10 bytes;
* a multi-byte encoding must not end in ``0x00`` (``NonCanonical``);
* the tenth byte must be ``0x00`` or ``0x01`` (``Malformed`` otherwise);
* truncated input is ``Malformed``.
"""

from __future__ import annotations

from .errors import Malformed, NonCanonical

MAX_VARINT_LEN = 10
U64_MAX = (1 << 64) - 1


def encoded_len(value: int) -> int:
    if value < 0 or value > U64_MAX:
        raise Malformed("varint value is outside u64")
    n = 1
    while value >= 0x80:
        value >>= 7
        n += 1
    return n


def encode(value: int, out: bytearray) -> None:
    """Append the minimal LEB128 encoding of ``value`` to ``out``."""
    if value < 0 or value > U64_MAX:
        raise Malformed("varint value is outside u64")
    v = value
    while True:
        byte = v & 0x7F
        v >>= 7
        if v == 0:
            out.append(byte)
            return
        out.append(byte | 0x80)


def encode_bytes(value: int) -> bytes:
    out = bytearray()
    encode(value, out)
    return bytes(out)


def decode(data: bytes, pos: int) -> "tuple[int, int]":
    """Decode one varint at ``pos``; return ``(value, next_pos)``."""
    value = 0
    shift = 0
    offset = pos
    count = 0
    while True:
        if offset >= len(data):
            raise Malformed("truncated varint")
        byte = data[offset]
        offset += 1
        count += 1
        if count > MAX_VARINT_LEN:
            raise Malformed("varint longer than 10 bytes")
        if count == MAX_VARINT_LEN and byte > 0x01:
            raise Malformed("varint overflows u64")
        low = byte & 0x7F
        if shift < 64:
            value |= low << shift
        elif low != 0:
            raise Malformed("varint overflows u64")
        if byte & 0x80 == 0:
            if count > 1 and byte == 0x00:
                raise NonCanonical("non-minimal varint")
            return value, offset
        shift += 7
        if shift > 63:
            raise Malformed("varint exceeds 10 bytes")


def zigzag_encode(value: int) -> int:
    """``(v << 1) ^ (v >> 63)`` on a 64-bit two's-complement pattern."""
    if value < -(1 << 63) or value > (1 << 63) - 1:
        raise Malformed("signed value is outside i64")
    return ((value << 1) ^ (value >> 63)) & U64_MAX


def zigzag_decode(value: int) -> int:
    """Inverse of :func:`zigzag_encode`."""
    return (value >> 1) ^ -(value & 1)
