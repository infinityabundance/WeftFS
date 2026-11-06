"""`huffman-canonical-v1` decoder (WEFT-AI-CODEC §5.3.1).

The serialized form is::

    u8 code_lengths[256]   # 0 = symbol absent
    || bitstream           # MSB-first, zero-padded to a byte boundary

The decoder rebuilds the canonical table from the 256 lengths and rejects, in
this order:

1. a length greater than 24 -> ``NonCanonical``;
2. an over-subscribed or incomplete prefix table (Kraft sum, 24-bit
   resolution, not exactly ``2**24``) -> ``NonCanonical``, except the
   single-symbol form (one used symbol with length 1);
3. a truncated bitstream -> ``Corrupt``;
4. extra trailing bytes, or padding bits that are not zero -> ``Corrupt``;
5. an output length other than the declared ``element_count`` -> ``Corrupt``.

``decode`` consumes the whole slice; ``decode_prefix`` consumes exactly the
256-byte table plus this stream's own padded bitstream and returns that length
so callers can concatenate independently coded streams (modes 11/12/13/14/15/
16).

This is a deliberately straightforward canonical decoder: performance is not
the goal, exactness and identical error classes are.
"""

from __future__ import annotations

from .errors import Corrupt, NonCanonical

TABLE_BYTES = 256
MAX_CODE_BITS = 24


class _Canonical:
    """Canonical tables for one 256-byte code-length header."""

    __slots__ = ("counts", "first_code", "first_index", "symbols", "max_len")

    def __init__(self) -> None:
        self.counts = [0] * (MAX_CODE_BITS + 1)
        self.first_code = [0] * (MAX_CODE_BITS + 1)
        self.first_index = [0] * (MAX_CODE_BITS + 1)
        self.symbols = []
        self.max_len = 0


def _build_table(lengths: bytes, expected_len: int) -> "tuple[_Canonical | None, bytes | None, int]":
    """Return ``(tables, single_symbol_output, used)``.

    ``tables`` is ``None`` for the empty and single-symbol forms. The second
    element is the pre-filled output for a single-symbol table.
    """
    if len(lengths) < TABLE_BYTES:
        raise Corrupt(
            "huffman-canonical-v1 stream is %d bytes, shorter than its %d-byte "
            "code-length table" % (len(lengths), TABLE_BYTES)
        )
    lengths = lengths[:TABLE_BYTES]
    counts = [0] * (MAX_CODE_BITS + 1)
    used = 0
    max_len = 0
    for symbol, length in enumerate(lengths):
        if length > MAX_CODE_BITS:
            raise NonCanonical(
                "huffman-canonical-v1 symbol %d has code length %d > %d"
                % (symbol, length, MAX_CODE_BITS)
            )
        if length:
            used += 1
            counts[length] += 1
            if length > max_len:
                max_len = length

    if used == 0:
        if expected_len != 0:
            raise Corrupt(
                "huffman-canonical-v1 table has no symbols but the descriptor "
                "declares a non-empty plane"
            )
        return None, b"", 0

    if used == 1:
        if max_len != 1:
            raise NonCanonical(
                "huffman-canonical-v1 single-symbol table must use code length 1"
            )
        if expected_len == 0:
            raise NonCanonical(
                "huffman-canonical-v1 non-empty table for an empty plane"
            )
        symbol = next(index for index, length in enumerate(lengths) if length)
        return None, bytes([symbol]) * expected_len, 1

    if expected_len == 0:
        raise NonCanonical(
            "huffman-canonical-v1 non-empty table for an empty plane"
        )

    # Kraft equality proves a complete prefix code without building a table.
    space = 1 << MAX_CODE_BITS
    used_space = 0
    for length in range(1, MAX_CODE_BITS + 1):
        used_space += counts[length] << (MAX_CODE_BITS - length)
        if used_space > space:
            raise NonCanonical(
                "huffman-canonical-v1 code lengths are over-subscribed"
            )
    if used_space != space:
        raise NonCanonical("huffman-canonical-v1 code lengths are incomplete")

    table = _Canonical()
    table.counts = counts
    table.max_len = max_len
    # Canonical assignment: sort used symbols by (code length, symbol).
    order = sorted(
        (length, symbol) for symbol, length in enumerate(lengths) if length
    )
    code = 0
    previous = 0
    assigned = [0] * (MAX_CODE_BITS + 1)
    symbols = []
    for length, symbol in order:
        code <<= length - previous
        if assigned[length] == 0:
            table.first_code[length] = code
            table.first_index[length] = len(symbols)
        assigned[length] += 1
        symbols.append(symbol)
        code += 1
        previous = length
    table.symbols = symbols
    return table, None, used


def _decode_prefix_tables(
    table: _Canonical, stream: bytes, expected_len: int
) -> "tuple[bytes, int]":
    out = bytearray()
    bit = 0
    total_bits = len(stream) * 8
    current_code = 0
    current_len = 0
    counts = table.counts
    first_code = table.first_code
    first_index = table.first_index
    symbols = table.symbols
    max_len = table.max_len

    while len(out) < expected_len:
        if bit >= total_bits:
            raise Corrupt(
                "huffman-canonical-v1 bitstream ended after %d of %d bytes"
                % (len(out), expected_len)
            )
        byte = stream[bit >> 3]
        current_code = (current_code << 1) | ((byte >> (7 - (bit & 7))) & 1)
        current_len += 1
        bit += 1

        slot = current_len
        if slot <= max_len:
            first = first_code[slot]
            if current_code >= first:
                offset = current_code - first
                if offset < counts[slot]:
                    out.append(symbols[first_index[slot] + offset])
                    current_code = 0
                    current_len = 0
                    continue
        if current_len >= max_len:
            raise Corrupt(
                "huffman-canonical-v1 bitstream contains an invalid code"
            )

    consumed_stream = (bit + 7) // 8
    used_bits_in_last = bit % 8
    if used_bits_in_last:
        last = stream[consumed_stream - 1]
        mask = (1 << (8 - used_bits_in_last)) - 1
        if last & mask:
            raise Corrupt("huffman-canonical-v1 padding bits must be zero")
    return bytes(out), TABLE_BYTES + consumed_stream


def decode_prefix(encoded: bytes, expected_len: int):
    """Decode one stream and return ``(bytes, consumed)``.

    ``consumed`` is the 256-byte table plus this stream's own padded bitstream.
    Bytes after that prefix belong to the caller.
    """
    if len(encoded) < TABLE_BYTES:
        raise Corrupt(
            "huffman-canonical-v1 stream is %d bytes, shorter than its %d-byte "
            "code-length table" % (len(encoded), TABLE_BYTES)
        )
    table, single, _used = _build_table(encoded[:TABLE_BYTES], expected_len)
    if table is None:
        return single, TABLE_BYTES
    return _decode_prefix_tables(table, encoded[TABLE_BYTES:], expected_len)


def decode(encoded: bytes, expected_len: int) -> bytes:
    """Decode exactly one complete stream; any trailing byte is ``Corrupt``."""
    out, consumed = decode_prefix(encoded, expected_len)
    if consumed != len(encoded):
        raise Corrupt(
            "huffman-canonical-v1 bitstream has trailing bytes after the "
            "declared output"
        )
    return out
