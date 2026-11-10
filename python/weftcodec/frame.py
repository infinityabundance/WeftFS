"""WFT1 frame header/table parsing and strict structural validation.

Layout (WEFTCODEC-FORMAT §2)::

    header (96 bytes) || dependency table || chunk table || body || trailer (4)

This module parses and validates structure only; payload modes live in
:mod:`weftcodec.modes`.
"""

from __future__ import annotations

from dataclasses import dataclass

from .crc32c import crc32c
from .errors import (
    Corrupt,
    Malformed,
    UnsupportedFeature,
    UnsupportedMode,
    UnsupportedVersion,
    LimitExceeded,
)
from .limits import Limits

MAGIC = b"WFT1"
HEADER_LEN = 96
TRAILER_LEN = 4
DEP_ENTRY_LEN = 48
CHUNK_ENTRY_LEN = 48
HASH_LEN = 32

FORMAT_MAJOR = 1
FORMAT_MINOR = 0

DOMAIN_PAYLOAD = 0
DOMAIN_STRUCTURED = 1

DEP_DOMAIN_PAYLOAD = 0
DEP_DOMAIN_STRUCTURED = 1
DEP_DOMAIN_DICTIONARY = 2

DEP_FLAG_REQUIRED = 0x01

FLAG_CHUNKED = 0x0001
FLAG_SELF_CONTAINED = 0x0002
FLAG_DICT_RAW = 0x0004
FLAG_ORIGINAL_HASH = 0x0008

FEATURE_DEPENDENCIES = 0x0001
FEATURE_CHUNK_HASHES = 0x0002

MODE_NAMES = {
    0: "raw",
    1: "zstd",
    2: "reference",
    3: "delta",
    4: "dictionary",
    5: "structured",
    6: "tensor",
    7: "tokens",
    8: "quantized_tensor",
    9: "float_tensor",
    10: "tensor_sharded",
    11: "weight_delta",
    12: "kv_cache",
    13: "activation_stream",
    14: "semantic_delta",
    15: "tensor_tiled",
    16: "sparse_delta",
    17: "float_tiled",
    18: "float_tiled_streams",
}

#: Modes this Python reader can actually materialize plaintext for.
IMPLEMENTED_MODES = frozenset(MODE_NAMES)

#: Modes whose wire value is allocated but not implemented here. Modes 19
#: (KV_CACHE_PROV) and 20 (ENGRAM_SHARD) have normative vectors in the shared
#: corpus but no Python decoder; the conformance adapter skips them explicitly.
UNIMPLEMENTED_MODES = frozenset({19, 20})


@dataclass(frozen=True)
class Dependency:
    hash: bytes
    decoded_len: int
    dep_domain: int
    dep_flags: int
    codec_hint: int


@dataclass(frozen=True)
class ChunkEntry:
    encoded_off: int
    encoded_len: int
    decoded_len: int
    hash: bytes


@dataclass(frozen=True)
class FrameHeader:
    format_major: int
    format_minor: int
    domain: int
    mode: int
    flags: int
    level: int
    decoded_len: int
    body_len: int
    dep_count: int
    chunk_count: int
    features: int
    content_hash: bytes
    table_len: int

    @property
    def mode_name(self) -> str:
        return MODE_NAMES.get(self.mode, "unknown")

    @property
    def is_chunked(self) -> bool:
        return bool(self.flags & FLAG_CHUNKED)

    def flag_names(self) -> "list[str]":
        names = []
        if self.flags & FLAG_CHUNKED:
            names.append("CHUNKED")
        if self.flags & FLAG_SELF_CONTAINED:
            names.append("SELF_CONTAINED")
        if self.flags & FLAG_DICT_RAW:
            names.append("DICT_RAW")
        if self.flags & FLAG_ORIGINAL_HASH:
            names.append("ORIGINAL_HASH")
        return names

    def feature_names(self) -> "list[str]":
        names = []
        if self.features & FEATURE_DEPENDENCIES:
            names.append("DEPENDENCIES")
        if self.features & FEATURE_CHUNK_HASHES:
            names.append("CHUNK_HASHES")
        return names


@dataclass(frozen=True)
class Frame:
    header: FrameHeader
    dependencies: "tuple[Dependency, ...]"
    chunks: "tuple[ChunkEntry, ...]"
    body: bytes
    raw: bytes


def _le_u16(data: bytes, offset: int) -> int:
    return int.from_bytes(data[offset : offset + 2], "little")


def _le_u32(data: bytes, offset: int) -> int:
    return int.from_bytes(data[offset : offset + 4], "little")


def _le_u64(data: bytes, offset: int) -> int:
    return int.from_bytes(data[offset : offset + 8], "little")


def _check_len(length: int, maximum: int, what: str) -> None:
    if length > maximum:
        raise LimitExceeded("%s length %d exceeds limit %d" % (what, length, maximum))


def mode_from_u8(value: int) -> int:
    if value in MODE_NAMES:
        return value
    if 17 <= value <= 127:
        raise UnsupportedMode("reserved mode %d is not defined in v1" % value)
    raise UnsupportedMode("experimental mode %d must not be persisted" % value)


def parse(data: bytes, limits: "Limits | None" = None) -> Frame:
    """Parse and fully validate a frame's structure."""
    limits = limits or Limits()
    if len(data) < HEADER_LEN + TRAILER_LEN:
        raise Malformed(
            "frame length %d is below the minimum %d"
            % (len(data), HEADER_LEN + TRAILER_LEN)
        )
    _check_len(len(data), limits.max_frame_bytes, "frame")

    if data[0:4] != MAGIC:
        raise Malformed("bad frame magic (expected WFT1)")
    format_major = data[4]
    format_minor = data[5]
    if format_major != FORMAT_MAJOR:
        raise UnsupportedVersion("unsupported format major %d" % format_major)
    if format_minor != FORMAT_MINOR:
        raise UnsupportedVersion("unsupported format minor %d" % format_minor)

    # The header checksum is verified before any length field is trusted.
    stored_header_crc = _le_u32(data, 72)
    if crc32c(data[0:72] + data[76:96]) != stored_header_crc:
        raise Corrupt("header CRC32C mismatch")

    domain = data[6]
    mode = mode_from_u8(data[7])
    flags = _le_u16(data, 8)
    level = _le_u16(data, 10)
    decoded_len = _le_u64(data, 12)
    body_len = _le_u64(data, 20)
    dep_count = _le_u32(data, 28)
    chunk_count = _le_u32(data, 32)
    feature_bits = _le_u16(data, 36)
    reserved0 = _le_u16(data, 38)
    content_hash = bytes(data[40:72])
    table_len = _le_u64(data, 76)
    reserved1 = _le_u32(data, 84)
    reserved2 = _le_u64(data, 88)

    if reserved0 != 0 or reserved1 != 0 or reserved2 != 0:
        raise Malformed("reserved header fields must be zero")
    if domain > DOMAIN_STRUCTURED:
        raise Malformed("invalid domain value %d" % domain)
    if flags & ~(FLAG_CHUNKED | FLAG_SELF_CONTAINED | FLAG_DICT_RAW | FLAG_ORIGINAL_HASH):
        raise UnsupportedFeature("unknown critical flag bits 0x%04x" % (flags & ~0x000F))
    if flags & FLAG_ORIGINAL_HASH:
        raise UnsupportedFeature("ORIGINAL_HASH flag is reserved in v1")
    if flags & FLAG_SELF_CONTAINED:
        raise UnsupportedFeature("SELF_CONTAINED is reserved and not produced in v1")
    if feature_bits & ~(FEATURE_DEPENDENCIES | FEATURE_CHUNK_HASHES):
        raise UnsupportedFeature(
            "unknown critical feature bits 0x%04x" % (feature_bits & ~0x0003)
        )

    _check_len(decoded_len, limits.max_decoded_bytes, "decoded data")
    _check_len(dep_count, limits.max_dependency_count, "dependency count")
    _check_len(chunk_count, limits.max_chunks, "chunk count")

    expected_table_len = dep_count * DEP_ENTRY_LEN + chunk_count * CHUNK_ENTRY_LEN
    if table_len != expected_table_len:
        raise Malformed(
            "table_len %d does not match %d dependencies and %d chunks"
            % (table_len, dep_count, chunk_count)
        )

    expected_total = HEADER_LEN + table_len + body_len + TRAILER_LEN
    if expected_total != len(data):
        raise Malformed(
            "frame length %d does not match header-derived length %d"
            % (len(data), expected_total)
        )

    chunked = bool(flags & FLAG_CHUNKED)
    if chunked:
        if chunk_count == 0:
            raise Malformed("CHUNKED set with an empty chunk table")
        if not feature_bits & FEATURE_CHUNK_HASHES:
            raise Malformed("CHUNKED set without the CHUNK_HASHES feature")
    else:
        if chunk_count != 0:
            raise Malformed("chunk table present without the CHUNKED flag")
        if feature_bits & FEATURE_CHUNK_HASHES:
            raise Malformed("CHUNK_HASHES set without the CHUNKED flag")
    if dep_count > 0 and not feature_bits & FEATURE_DEPENDENCIES:
        raise Malformed("dependency table present without the DEPENDENCIES feature")

    body_start = HEADER_LEN + table_len
    body_end = body_start + body_len
    body = bytes(data[body_start:body_end])

    trailer_at = len(data) - TRAILER_LEN
    stored_trailer_crc = _le_u32(data, trailer_at)
    if crc32c(data[:trailer_at]) != stored_trailer_crc:
        raise Corrupt("frame trailer CRC32C mismatch")

    dependencies = []
    for index in range(dep_count):
        offset = HEADER_LEN + index * DEP_ENTRY_LEN
        dep_hash = bytes(data[offset : offset + HASH_LEN])
        dep_len = _le_u64(data, offset + 32)
        dep_domain = data[offset + 40]
        dep_flags = data[offset + 41]
        reserved = _le_u16(data, offset + 42)
        codec_hint = _le_u32(data, offset + 44)
        if dep_flags & ~DEP_FLAG_REQUIRED:
            raise Malformed("dependency %d has unknown flag bits" % index)
        if not dep_flags & DEP_FLAG_REQUIRED:
            raise Malformed("dependency %d does not set REQUIRED" % index)
        if reserved != 0:
            raise Malformed("dependency %d reserved field must be zero" % index)
        if dep_domain > DEP_DOMAIN_DICTIONARY:
            raise Malformed("dependency %d has invalid domain %d" % (index, dep_domain))
        _check_len(dep_len, limits.max_decoded_bytes, "dependency decoded data")
        dependencies.append(
            Dependency(dep_hash, dep_len, dep_domain, dep_flags, codec_hint)
        )

    chunk_table_start = HEADER_LEN + dep_count * DEP_ENTRY_LEN
    chunks = []
    for index in range(chunk_count):
        offset = chunk_table_start + index * CHUNK_ENTRY_LEN
        chunks.append(
            ChunkEntry(
                encoded_off=_le_u64(data, offset),
                encoded_len=_le_u32(data, offset + 8),
                decoded_len=_le_u32(data, offset + 12),
                hash=bytes(data[offset + 16 : offset + 48]),
            )
        )

    if chunked:
        next_offset = 0
        decoded_sum = 0
        for index, chunk in enumerate(chunks):
            if chunk.encoded_off != next_offset:
                raise Malformed(
                    "chunk %d starts at %d but expected %d (gap or overlap)"
                    % (index, chunk.encoded_off, next_offset)
                )
            if chunk.encoded_len == 0:
                raise Malformed("chunk %d has zero encoded length" % index)
            next_offset += chunk.encoded_len
            decoded_sum += chunk.decoded_len
        if next_offset != body_len:
            raise Malformed(
                "chunk extents end at %d but body_len is %d" % (next_offset, body_len)
            )
        if decoded_sum != decoded_len:
            raise Malformed(
                "chunk decoded lengths sum to %d but decoded_len is %d"
                % (decoded_sum, decoded_len)
            )

    header = FrameHeader(
        format_major=format_major,
        format_minor=format_minor,
        domain=domain,
        mode=mode,
        flags=flags,
        level=level,
        decoded_len=decoded_len,
        body_len=body_len,
        dep_count=dep_count,
        chunk_count=chunk_count,
        features=feature_bits,
        content_hash=content_hash,
        table_len=table_len,
    )
    _validate_mode_rules(header, dependencies, limits)
    return Frame(header, tuple(dependencies), tuple(chunks), body, bytes(data))


def _validate_mode_rules(
    header: FrameHeader, dependencies: "list[Dependency]", limits: Limits
) -> None:
    mode = header.mode
    dict_flag = bool(header.flags & FLAG_DICT_RAW)

    def payload_domain(what: str) -> None:
        if header.domain != DOMAIN_PAYLOAD:
            raise Malformed("%s frame must use the payload domain" % what)

    def no_dict(what: str) -> None:
        if dict_flag:
            raise Malformed("DICT_RAW is only valid for DICTIONARY (%s)" % what)

    if mode == 0:  # RAW
        payload_domain("RAW")
        no_dict("RAW")
        if header.dep_count != 0:
            raise Malformed("RAW frame must not have dependencies")
        if header.body_len != header.decoded_len:
            raise Malformed("RAW body_len must equal decoded_len")
    elif mode == 1:  # ZSTD
        payload_domain("ZSTD")
        no_dict("ZSTD")
        if header.dep_count != 0:
            raise Malformed("ZSTD frame must not have dependencies in v1")
    elif mode == 2:  # REFERENCE
        payload_domain("REFERENCE")
        no_dict("REFERENCE")
        if header.body_len != 0:
            raise Malformed("REFERENCE body must be empty")
        if header.dep_count != 1:
            raise Malformed("REFERENCE requires exactly one dependency")
        if header.chunk_count != 0:
            raise Malformed("REFERENCE must not be chunked")
        if header.decoded_len != dependencies[0].decoded_len:
            raise Malformed(
                "REFERENCE decoded_len must equal dependency decoded_len"
            )
    elif mode == 3:  # DELTA
        payload_domain("DELTA")
        no_dict("DELTA")
        if header.dep_count != 1:
            raise Malformed("DELTA requires exactly one dependency")
        if header.chunk_count != 0:
            raise Malformed("DELTA must not be chunked")
    elif mode == 4:  # DICTIONARY
        payload_domain("DICTIONARY")
        if not dict_flag:
            raise Malformed("DICTIONARY requires the DICT_RAW flag")
        if header.dep_count != 1:
            raise Malformed("DICTIONARY requires exactly one dependency")
        if dependencies[0].dep_domain != DEP_DOMAIN_DICTIONARY:
            raise Malformed(
                "DICTIONARY dependency must use dep_domain 2 (dictionary bytes)"
            )
        if header.chunk_count != 0:
            raise Malformed("DICTIONARY must not be chunked")
        _check_len(
            dependencies[0].decoded_len,
            limits.max_dictionary_bytes,
            "dictionary",
        )
    elif mode == 5:  # STRUCTURED
        if header.domain != DOMAIN_STRUCTURED:
            raise Malformed("STRUCTURED frame must use the structured domain")
        no_dict("STRUCTURED")
        if header.dep_count != 0:
            raise Malformed("STRUCTURED must not have dependencies in v1")
        if header.chunk_count != 0:
            raise Malformed("STRUCTURED must not be chunked")
        if header.body_len != header.decoded_len:
            raise Malformed("STRUCTURED body_len must equal decoded_len")
    elif mode in (6, 7, 8, 9, 10, 12, 13, 15):  # AI modes without dependencies
        payload_domain("AI-mode")
        no_dict("AI-mode")
        if header.dep_count != 0:
            raise Malformed("AI-mode frames must not have dependencies in v1")
        if header.chunk_count != 0:
            raise Malformed("AI-mode frames must not be chunked")
        if header.body_len < 4:
            raise Malformed("AI-mode body is shorter than its 4-byte metadata length")
    elif mode in (11, 14, 16):  # WEIGHT_DELTA / SEMANTIC_DELTA / SPARSE_DELTA
        name = {
            11: "WEIGHT_DELTA",
            14: "SEMANTIC_DELTA",
            16: "SPARSE_DELTA",
        }[mode]
        payload_domain(name)
        no_dict(name)
        if header.dep_count != 1:
            raise Malformed("%s requires exactly one dependency" % name)
        if header.chunk_count != 0:
            raise Malformed("%s must not be chunked" % name)
        if header.body_len < 4:
            raise Malformed(
                "%s body is shorter than its 4-byte metadata length" % name
            )
        if dependencies[0].dep_domain != DEP_DOMAIN_PAYLOAD:
            raise Malformed(
                "%s dependency must use dep_domain 0 (payload)" % name
            )
        if header.decoded_len != dependencies[0].decoded_len:
            raise Malformed(
                "%s decoded_len must equal the base decoded_len" % name
            )
