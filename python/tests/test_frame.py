"""Low-level frame, varint, and CRC32C tests."""

from __future__ import annotations

import pytest

from support import VECTORS, load_json

from weftcodec import content_root, parse
from weftcodec.crc32c import crc32c
from weftcodec.errors import WeftError
from weftcodec.frame import HEADER_LEN, IMPLEMENTED_MODES, UNIMPLEMENTED_MODES
from weftcodec.modes import decode_bytes
from weftcodec import varint


def test_crc32c_known_value():
    assert crc32c(b"123456789") == 0xE3069283
    assert crc32c(b"") == 0x00000000


def test_varint_roundtrip_boundaries():
    for value in [0, 1, 127, 128, 129, 255, 256, 16384, (1 << 32) - 1, (1 << 64) - 1]:
        encoded = varint.encode_bytes(value)
        assert varint.encoded_len(value) == len(encoded)
        decoded, pos = varint.decode(encoded, 0)
        assert decoded == value
        assert pos == len(encoded)


def test_varint_rejections():
    with pytest.raises(WeftError) as info:
        varint.decode(b"\x80\x00", 0)
    assert info.value.error_class == "NonCanonical"

    with pytest.raises(WeftError) as info:
        varint.decode(b"\x80", 0)
    assert info.value.error_class == "Malformed"

    with pytest.raises(WeftError) as info:
        varint.decode(b"\xff" * 10 + b"\x02", 0)
    assert info.value.error_class == "Malformed"


def test_varint_u64_max_bytes():
    assert varint.decode(b"\xff" * 9 + b"\x01", 0) == ((1 << 64) - 1, 10)


def test_mode_sets_are_disjoint_and_complete():
    assert IMPLEMENTED_MODES.isdisjoint(UNIMPLEMENTED_MODES)
    # Modes 0-18 are allocated and implemented; 19 (KV_CACHE_PROV) and 20
    # (ENGRAM_SHARD) are allocated without a Python decoder; 21+ are reserved.
    assert IMPLEMENTED_MODES | UNIMPLEMENTED_MODES == set(range(21))


def test_raw_frame_header_and_plaintext():
    vector = load_json(VECTORS / "codec" / "003_raw_4k.json")
    frame_bytes = bytes.fromhex(vector["frame_hex"])
    frame = parse(frame_bytes)
    header = frame.header
    assert header.mode == 0
    assert header.mode_name == "raw"
    assert header.decoded_len == 4096
    assert header.body_len == 4096
    assert header.dep_count == 0
    assert header.chunk_count == 0
    assert header.content_hash.hex() == vector["content_hash"]
    plaintext = decode_bytes(frame_bytes)
    assert plaintext == bytes.fromhex(vector["plaintext_hex"])
    assert content_root(plaintext).hex() == vector["plaintext_hash"]
    assert frame.body == plaintext


def test_structured_body_is_validated_as_wcb():
    vector = load_json(VECTORS / "codec" / "028_structured_non_canonical.json")
    with pytest.raises(WeftError) as info:
        decode_bytes(bytes.fromhex(vector["frame_hex"]))
    assert info.value.error_class == "NonCanonical"


def test_chunk_table_access():
    vector = load_json(VECTORS / "codec" / "006_raw_chunked_1024.json")
    frame = parse(bytes.fromhex(vector["frame_hex"]))
    assert frame.header.is_chunked
    assert len(frame.chunks) == 5
    assert sum(chunk.decoded_len for chunk in frame.chunks) == frame.header.decoded_len
    assert frame.chunks[0].encoded_off == 0
    assert frame.chunks[-1].encoded_off + frame.chunks[-1].encoded_len == len(frame.body)


def test_truncated_frame_is_malformed():
    with pytest.raises(WeftError) as info:
        parse(b"WFT1" + b"\x00" * 50)
    assert info.value.error_class == "Malformed"
    assert HEADER_LEN == 96
