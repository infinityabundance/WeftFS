"""CRC-32C (Castagnoli), WEFTCODEC-FORMAT §1.

Polynomial 0x1EDC6F41, reflected input/output, init 0xFFFFFFFF, final XOR
0xFFFFFFFF. Implemented locally so the reader has no extra dependency.
"""

from __future__ import annotations

_POLY = 0x82F63B78


def _make_table() -> "list[int]":
    table = []
    for index in range(256):
        crc = index
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ _POLY
            else:
                crc >>= 1
        table.append(crc)
    return table


_TABLE = _make_table()


def crc32c(data: bytes) -> int:
    crc = 0xFFFFFFFF
    table = _TABLE
    for byte in data:
        crc = table[(crc ^ byte) & 0xFF] ^ (crc >> 8)
    return crc ^ 0xFFFFFFFF
