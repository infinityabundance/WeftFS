"""Payload-mode decoders (WEFTCODEC-FORMAT §3–§6, WEFT-AI-CODEC §2–§17).

Implemented modes: RAW (0), ZSTD (1), REFERENCE (2), DELTA (3), DICTIONARY (4),
STRUCTURED (5), TENSOR (6), TOKEN_SEQUENCE (7), QUANTIZED_TENSOR (8),
FLOAT_TENSOR (9), TENSOR_SHARDED (10), WEIGHT_DELTA (11), KV_CACHE (12),
ACTIVATION_STREAM (13), SEMANTIC_DELTA (14), TENSOR_TILED (15), and
SPARSE_DELTA (16).

No allocated mode raises :class:`weftcodec.errors.UnsupportedMode`; reserved
and experimental mode bytes are rejected by :mod:`weftcodec.frame`. There is no
approximate or lossy fallback anywhere in this module.
"""

from __future__ import annotations

import io
import math
import struct

import zstandard

from . import ai_modes, varint, wcb
from .dependency import Resolver, resolve_dependency
from .errors import (
    Corrupt,
    LimitExceeded,
    Malformed,
    UnsupportedMode,
)
from .frame import Frame, MODE_NAMES
from .hashing import (
    DOMAIN_CHUNK,
    DOMAIN_QUANTIZED_TENSOR_PAYLOAD,
    DOMAIN_TENSOR_PAYLOAD,
    DOMAIN_TOKENS_PAYLOAD,
    content_root,
    hash_domain,
)
from .limits import Limits

TOKEN_ENCODING = "u64-zigzag-delta-leb128"
TENSOR_SCHEMA = "weft.tensor.v1"
TOKENS_SCHEMA = "weft.tokens.v1"
QUANTIZED_TENSOR_SCHEMA = "weft.quantized-tensor.v1"
QUANTIZED_TENSOR_QUANTIZATION = "affine-asymmetric-per-tensor"

TENSOR_DTYPE_SIZES = {
    "f16": 2,
    "bf16": 2,
    "f32": 4,
    "f64": 8,
    "i8": 1,
    "u8": 1,
    "i16": 2,
    "i32": 4,
    "i64": 8,
}

U64_MAX = (1 << 64) - 1


# ---------------------------------------------------------------------------
# Frame decoding
# ---------------------------------------------------------------------------


def decode_bytes(
    data: bytes,
    resolver: "Resolver | None" = None,
    limits: "Limits | None" = None,
    base_frame: "bytes | None" = None,
) -> bytes:
    """Parse and decode a complete frame."""
    from .frame import parse

    return decode_frame(parse(data, limits), resolver, limits, base_frame)


def decode_frame(
    frame: Frame,
    resolver: "Resolver | None" = None,
    limits: "Limits | None" = None,
    base_frame: "bytes | None" = None,
) -> bytes:
    """Decode a parsed frame and verify its content hash."""
    limits = limits or Limits()
    header = frame.header
    mode = header.mode

    if mode == 0:  # RAW
        plaintext = _decode_chunks(frame, limits) if header.is_chunked else frame.body
    elif mode == 1:  # ZSTD
        plaintext = (
            _decode_chunks(frame, limits)
            if header.is_chunked
            else _decompress(frame.body, header.decoded_len)
        )
    elif mode == 2:  # REFERENCE
        plaintext = resolve_dependency(frame.dependencies[0], resolver)
    elif mode == 3:  # DELTA
        base = resolve_dependency(frame.dependencies[0], resolver)
        plaintext = _decode_delta(frame.body, base, header.decoded_len, limits)
    elif mode == 4:  # DICTIONARY
        dictionary = resolve_dependency(frame.dependencies[0], resolver)
        plaintext = _decompress(frame.body, header.decoded_len, dictionary)
    elif mode == 5:  # STRUCTURED
        wcb.decode(frame.body, limits)
        plaintext = frame.body
    elif mode == 6:  # TENSOR
        _, plaintext = _parse_tensor_body(frame, limits)
    elif mode == 7:  # TOKEN_SEQUENCE
        _, plaintext = _parse_token_body(frame, limits)
    elif mode == 8:  # QUANTIZED_TENSOR
        _, plaintext = _parse_quantized_body(frame, limits)
    elif mode == 9:  # FLOAT_TENSOR
        plaintext = ai_modes.decode_float_tensor(frame, limits)
    elif mode == 10:  # TENSOR_SHARDED
        plaintext = ai_modes.decode_tensor_sharded(frame, limits).planes
    elif mode == 11:  # WEIGHT_DELTA
        plaintext = ai_modes.decode_weight_delta(
            frame, resolver, limits, base_frame
        )
    elif mode == 12:  # KV_CACHE
        plaintext = ai_modes.decode_kv_cache(frame, limits)
    elif mode == 13:  # ACTIVATION_STREAM
        plaintext = ai_modes.decode_activation_stream(frame, limits)
    elif mode == 14:  # SEMANTIC_DELTA
        plaintext = ai_modes.decode_semantic_delta(
            frame, resolver, limits, base_frame
        )
    elif mode == 15:  # TENSOR_TILED
        plaintext = ai_modes.decode_tensor_tiled(frame, limits)
    elif mode == 16:  # SPARSE_DELTA
        plaintext = ai_modes.decode_sparse_delta(
            frame, resolver, limits, base_frame
        )
    elif mode == 17:  # FLOAT_TILED
        plaintext = ai_modes.decode_float_tiled(frame, limits)
    else:
        raise UnsupportedMode(
            "mode %d (%s) is allocated but not implemented by the Python reader"
            % (mode, MODE_NAMES.get(mode, "unknown"))
        )

    if len(plaintext) != header.decoded_len:
        raise Corrupt(
            "decoded length %d differs from declared %d"
            % (len(plaintext), header.decoded_len)
        )
    if content_root(plaintext) != header.content_hash:
        raise Corrupt("content hash mismatch")
    return bytes(plaintext)


def _decode_chunks(frame: Frame, limits: Limits) -> bytes:
    out = bytearray()
    for index in range(len(frame.chunks)):
        out.extend(_decode_chunk(frame, index))
        if len(out) > limits.max_decoded_bytes:
            raise LimitExceeded(
                "decoded data length %d exceeds limit %d"
                % (len(out), limits.max_decoded_bytes)
            )
    return bytes(out)


def _decode_chunk(frame: Frame, index: int) -> bytes:
    if index < 0 or index >= len(frame.chunks):
        raise Malformed("chunk index %d out of range" % index)
    chunk = frame.chunks[index]
    start = chunk.encoded_off
    end = start + chunk.encoded_len
    if start > len(frame.body) or end > len(frame.body):
        raise Malformed("chunk %d extends past body" % index)
    encoded = frame.body[start:end]

    if frame.header.mode == 0:
        if len(encoded) != chunk.decoded_len:
            raise Malformed(
                "RAW chunk %d encoded_len does not equal decoded_len" % index
            )
        decoded = encoded
    elif frame.header.mode == 1:
        decoded = _decompress(encoded, chunk.decoded_len)
    else:
        raise Malformed("chunk table is only valid for RAW and ZSTD frames")

    if len(decoded) != chunk.decoded_len:
        raise Corrupt(
            "chunk %d decoded length %d differs from declared %d"
            % (index, len(decoded), chunk.decoded_len)
        )
    if content_root(decoded) != chunk.hash:
        raise Corrupt("chunk %d hash mismatch" % index)
    return decoded


# ---------------------------------------------------------------------------
# Zstandard
# ---------------------------------------------------------------------------


def _check_zstd_preamble(body: bytes, declared_len: int) -> None:
    if not body:
        raise Malformed("zstd payload is empty; a zstd frame is never zero bytes")
    try:
        frame_len = zstandard.frame_content_size(body)
    except zstandard.ZstdError:
        return
    unknown = getattr(zstandard, "CONTENTSIZE_UNKNOWN", -1)
    error = getattr(zstandard, "CONTENTSIZE_ERROR", -2)
    if frame_len not in (unknown, error) and frame_len != declared_len:
        raise Corrupt(
            "zstd frame declares content size %d, Weft header declares %d"
            % (frame_len, declared_len)
        )


def _decompress(body: bytes, declared_len: int, dictionary: "bytes | None" = None) -> bytes:
    _check_zstd_preamble(body, declared_len)
    if dictionary is None:
        context = zstandard.ZstdDecompressor()
    else:
        raw_dict = zstandard.ZstdCompressionDict(
            bytes(dictionary), dict_type=zstandard.DICT_TYPE_RAWCONTENT
        )
        context = zstandard.ZstdDecompressor(dict_data=raw_dict)

    out = bytearray()
    limit = declared_len + 1
    try:
        with context.stream_reader(io.BytesIO(body)) as reader:
            while len(out) < limit:
                piece = reader.read(min(limit - len(out), 1 << 20))
                if not piece:
                    break
                out.extend(piece)
    except zstandard.ZstdError as exc:
        raise Corrupt("zstd decompression failed: %s" % exc)
    if len(out) != declared_len:
        raise Corrupt(
            "zstd output length %d differs from declared %d" % (len(out), declared_len)
        )
    return bytes(out)


# ---------------------------------------------------------------------------
# DELTA (mode 3)
# ---------------------------------------------------------------------------


def _decode_delta(body: bytes, base: bytes, expected_len: int, limits: Limits) -> bytes:
    if expected_len > limits.max_decoded_bytes:
        raise LimitExceeded(
            "decoded data length %d exceeds limit %d"
            % (expected_len, limits.max_decoded_bytes)
        )
    out = bytearray()
    pos = 0
    ops = 0

    while pos < len(body):
        ops += 1
        if ops > limits.max_delta_ops:
            raise LimitExceeded(
                "delta op count %d exceeds limit %d" % (ops, limits.max_delta_ops)
            )
        opcode = body[pos]
        pos += 1
        if opcode == 0x00:  # COPY
            offset, pos = varint.decode(body, pos)
            length, pos = varint.decode(body, pos)
            if length == 0:
                raise Malformed("delta COPY has zero length")
            end = offset + length
            if end > len(base):
                raise Malformed(
                    "delta COPY range %d..%d exceeds base length %d"
                    % (offset, end, len(base))
                )
            if len(out) + length > expected_len:
                raise Corrupt("delta emits more bytes than declared decoded_len")
            out.extend(base[offset:end])
        elif opcode == 0x01:  # INSERT
            length, pos = varint.decode(body, pos)
            if length == 0:
                raise Malformed("delta INSERT has zero length")
            end = pos + length
            if end > len(body):
                raise Malformed("delta INSERT extends past body")
            if len(out) + length > expected_len:
                raise Corrupt("delta emits more bytes than declared decoded_len")
            out.extend(body[pos:end])
            pos = end
        else:
            raise Malformed("unknown delta opcode 0x%02x" % opcode)

    if len(out) != expected_len:
        raise Corrupt(
            "delta produced %d bytes but decoded_len is %d" % (len(out), expected_len)
        )
    return bytes(out)


# ---------------------------------------------------------------------------
# Shared AI-mode helpers
# ---------------------------------------------------------------------------


def _split_ai_body(body: bytes, decoded_len: int) -> "tuple[bytes, bytes]":
    if len(body) < 4:
        raise Malformed("AI body is shorter than its 4-byte metadata length")
    meta_len = int.from_bytes(body[0:4], "little")
    meta_end = 4 + meta_len
    if meta_end > len(body):
        raise Malformed("AI metadata extends past the frame body")
    payload = body[meta_end:]
    if len(payload) != decoded_len:
        raise Malformed(
            "AI payload length %d differs from decoded_len %d"
            % (len(payload), decoded_len)
        )
    return body[4:meta_end], payload


def _ai_map(value, what: str) -> dict:
    if not isinstance(value, dict):
        raise Malformed("%s metadata must be a WCB map" % what)
    return value


def _check_exact_keys(map_value: dict, expected: "tuple[str, ...]", what: str) -> None:
    for key in map_value:
        if key not in expected:
            raise Malformed("%s metadata has unknown key `%s`" % (what, key))
    for key in expected:
        if key not in map_value:
            raise Malformed("%s metadata is missing key `%s`" % (what, key))


def _meta_text(map_value: dict, key: str) -> str:
    value = map_value.get(key)
    if not isinstance(value, str):
        raise Malformed("metadata `%s` must be text" % key)
    return value


def _meta_u64(map_value: dict, key: str) -> int:
    value = map_value.get(key)
    if not isinstance(value, wcb.U64):
        raise Malformed("metadata `%s` must be u64" % key)
    return value.value


def _meta_i64(map_value: dict, key: str) -> int:
    value = map_value.get(key)
    if not isinstance(value, wcb.I64):
        raise Malformed("metadata `%s` must be i64" % key)
    return value.value


def _meta_bytes(map_value: dict, key: str) -> bytes:
    value = map_value.get(key)
    if not isinstance(value, (bytes, bytearray)):
        raise Malformed("metadata `%s` must be bytes" % key)
    return bytes(value)


def _meta_shape(map_value: dict) -> "list[int]":
    value = map_value.get("shape")
    if not isinstance(value, list):
        raise Malformed("metadata `shape` must be an array")
    shape = []
    for item in value:
        if not isinstance(item, wcb.U64):
            raise Malformed("metadata `shape` elements must be u64")
        shape.append(item.value)
    return shape


def _tensor_product(shape: "list[int]", limits: Limits) -> int:
    if len(shape) > limits.max_tensor_rank:
        raise LimitExceeded(
            "tensor rank %d exceeds limit %d" % (len(shape), limits.max_tensor_rank)
        )
    product = 1
    for dim in shape:
        product *= dim
        if product > U64_MAX:
            raise LimitExceeded("tensor shape product overflows u64")
    if product > limits.max_tensor_elements:
        raise LimitExceeded(
            "tensor element count %d exceeds limit %d"
            % (product, limits.max_tensor_elements)
        )
    return product


def _descriptor(body: bytes, decoded_len: int, limits: Limits, what: str):
    descriptor_bytes, payload = _split_ai_body(body, decoded_len)
    value = wcb.decode(descriptor_bytes, limits)
    return _ai_map(value, what), payload


# ---------------------------------------------------------------------------
# TENSOR (mode 6)
# ---------------------------------------------------------------------------

_TENSOR_KEYS = (
    "schema",
    "dtype",
    "shape",
    "layout",
    "endianness",
    "element_count",
    "payload_bytes",
    "payload_hash",
)


def _parse_tensor_body(frame: Frame, limits: Limits):
    map_value, payload = _descriptor(
        frame.body, frame.header.decoded_len, limits, "tensor"
    )
    _check_exact_keys(map_value, _TENSOR_KEYS, "tensor")
    schema = _meta_text(map_value, "schema")
    if schema != TENSOR_SCHEMA:
        raise Malformed("unsupported tensor schema `%s`" % schema)
    dtype = _meta_text(map_value, "dtype")
    if dtype not in TENSOR_DTYPE_SIZES:
        raise Malformed(
            "unknown tensor dtype `%s` (v1 allows f16, bf16, f32, f64, i8, u8, "
            "i16, i32, i64)" % dtype
        )
    layout = _meta_text(map_value, "layout")
    if layout != "row_major":
        raise Malformed(
            "tensor layout `%s` is specified but not implemented in v1 (only row_major)"
            % layout
        )
    endianness = _meta_text(map_value, "endianness")
    if endianness != "little":
        raise Malformed(
            "tensor endianness `%s` is specified but not implemented in v1 (only little)"
            % endianness
        )
    shape = _meta_shape(map_value)
    declared_count = _meta_u64(map_value, "element_count")
    declared_bytes = _meta_u64(map_value, "payload_bytes")
    expected_count = _tensor_product(shape, limits)
    if declared_count != expected_count:
        raise Malformed(
            "tensor element_count %d does not match shape product %d"
            % (declared_count, expected_count)
        )
    expected_bytes = declared_count * TENSOR_DTYPE_SIZES[dtype]
    if expected_bytes > U64_MAX:
        raise LimitExceeded("tensor payload byte count overflows u64")
    if declared_bytes != expected_bytes:
        raise Malformed(
            "tensor payload_bytes %d does not match element_count x element_size %d"
            % (declared_bytes, expected_bytes)
        )
    if declared_bytes != len(payload):
        raise Malformed(
            "tensor payload is %d bytes but metadata declares %d"
            % (len(payload), declared_bytes)
        )
    declared_hash = _meta_bytes(map_value, "payload_hash")
    if len(declared_hash) != 32:
        raise Malformed(
            "tensor payload_hash must be 32 bytes, got %d" % len(declared_hash)
        )
    if declared_hash != hash_domain(DOMAIN_TENSOR_PAYLOAD, payload):
        raise Corrupt("tensor payload_hash mismatch")
    meta = {
        "schema": schema,
        "dtype": dtype,
        "shape": shape,
        "layout": layout,
        "endianness": endianness,
        "element_count": declared_count,
        "payload_bytes": declared_bytes,
    }
    return meta, payload


def tensor_metadata(frame: Frame, limits: "Limits | None" = None) -> dict:
    meta, _ = _parse_tensor_body(frame, limits or Limits())
    return meta


# ---------------------------------------------------------------------------
# TOKEN_SEQUENCE (mode 7)
# ---------------------------------------------------------------------------

_TOKEN_KEYS = (
    "schema",
    "vocab_schema",
    "encoding",
    "token_count",
    "payload_bytes",
    "payload_hash",
)


def _parse_token_body(frame: Frame, limits: Limits):
    map_value, payload = _descriptor(
        frame.body, frame.header.decoded_len, limits, "token"
    )
    _check_exact_keys(map_value, _TOKEN_KEYS, "token")
    schema = _meta_text(map_value, "schema")
    if schema != TOKENS_SCHEMA:
        raise Malformed("unsupported token schema `%s`" % schema)
    vocab_schema = _meta_text(map_value, "vocab_schema")
    if not vocab_schema:
        raise Malformed("token vocab_schema must not be empty")
    encoding = _meta_text(map_value, "encoding")
    if encoding != TOKEN_ENCODING:
        raise Malformed(
            "token encoding `%s` is specified but not implemented in v1 (only %s)"
            % (encoding, TOKEN_ENCODING)
        )
    token_count = _meta_u64(map_value, "token_count")
    if token_count > limits.max_tokens:
        raise LimitExceeded(
            "token count %d exceeds limit %d" % (token_count, limits.max_tokens)
        )
    declared_bytes = _meta_u64(map_value, "payload_bytes")
    if declared_bytes != len(payload):
        raise Malformed(
            "token payload is %d bytes but metadata declares %d"
            % (len(payload), declared_bytes)
        )
    declared_hash = _meta_bytes(map_value, "payload_hash")
    if len(declared_hash) != 32:
        raise Malformed(
            "token payload_hash must be 32 bytes, got %d" % len(declared_hash)
        )
    if declared_hash != hash_domain(DOMAIN_TOKENS_PAYLOAD, payload):
        raise Corrupt("token payload_hash mismatch")
    _validate_token_payload(payload, token_count, limits)
    meta = {
        "schema": schema,
        "vocab_schema": vocab_schema,
        "encoding": encoding,
        "token_count": token_count,
        "payload_bytes": declared_bytes,
    }
    return meta, payload


def _validate_token_payload(payload: bytes, token_count: int, limits: Limits) -> None:
    if token_count > limits.max_tokens:
        raise LimitExceeded(
            "token count %d exceeds limit %d" % (token_count, limits.max_tokens)
        )
    if token_count > len(payload):
        raise Malformed(
            "token count %d cannot fit in a %d-byte payload"
            % (token_count, len(payload))
        )
    pos = 0
    previous = 0
    for index in range(token_count):
        raw, pos = varint.decode(payload, pos)
        if index == 0:
            previous = raw
        else:
            previous = (previous + varint.zigzag_decode(raw)) & U64_MAX
    if pos != len(payload):
        raise Malformed(
            "token payload has %d trailing bytes" % (len(payload) - pos)
        )


def token_metadata(frame: Frame, limits: "Limits | None" = None) -> dict:
    meta, _ = _parse_token_body(frame, limits or Limits())
    return meta


def decode_token_payload(payload: bytes, token_count: int, limits: "Limits | None" = None):
    """Return token ids from an encoded token payload (exact inverse)."""
    limits = limits or Limits()
    _validate_token_payload(payload, token_count, limits)
    ids = []
    pos = 0
    previous = 0
    for index in range(token_count):
        raw, pos = varint.decode(payload, pos)
        if index == 0:
            previous = raw
        else:
            previous = (previous + varint.zigzag_decode(raw)) & U64_MAX
        ids.append(previous)
    return ids


# ---------------------------------------------------------------------------
# QUANTIZED_TENSOR (mode 8)
# ---------------------------------------------------------------------------

_QUANT_KEYS = (
    "schema",
    "dtype",
    "shape",
    "layout",
    "quantization",
    "scale_bits_le",
    "zero_point",
    "element_count",
    "payload_bytes",
    "payload_hash",
)


def _validate_quantized_scale(scale: float) -> None:
    if not math.isfinite(scale):
        raise Malformed("quantized tensor scale must be finite (no NaN or infinity)")
    if scale <= 0.0:
        raise Malformed("quantized tensor scale must be greater than zero")


def _validate_zero_point(dtype: str, zero_point: int) -> None:
    if dtype == "i8":
        low, high = -128, 127
    elif dtype == "u8":
        low, high = 0, 255
    else:
        raise Malformed("quantized tensor dtype must be i8 or u8")
    if zero_point < low or zero_point > high:
        raise Malformed(
            "quantized tensor zero_point %d is outside %s range %d..=%d"
            % (zero_point, dtype, low, high)
        )


def _parse_quantized_body(frame: Frame, limits: Limits):
    map_value, payload = _descriptor(
        frame.body, frame.header.decoded_len, limits, "quantized tensor"
    )
    _check_exact_keys(map_value, _QUANT_KEYS, "quantized tensor")
    schema = _meta_text(map_value, "schema")
    if schema != QUANTIZED_TENSOR_SCHEMA:
        raise Malformed("unsupported quantized tensor schema `%s`" % schema)
    dtype = _meta_text(map_value, "dtype")
    if dtype not in ("i8", "u8"):
        raise Malformed(
            "quantized tensor dtype `%s` is specified but not implemented in v1 "
            "(only i8, u8)" % dtype
        )
    layout = _meta_text(map_value, "layout")
    if layout != "row_major":
        raise Malformed(
            "quantized tensor layout `%s` is specified but not implemented in v1 "
            "(only row_major)" % layout
        )
    quantization = _meta_text(map_value, "quantization")
    if quantization != QUANTIZED_TENSOR_QUANTIZATION:
        raise Malformed(
            "quantized tensor quantization `%s` is specified but not implemented in v1"
            % quantization
        )
    scale_bits = _meta_bytes(map_value, "scale_bits_le")
    if len(scale_bits) != 8:
        raise Malformed(
            "quantized tensor scale_bits_le must be 8 bytes, got %d" % len(scale_bits)
        )
    scale = struct.unpack("<d", scale_bits)[0]
    _validate_quantized_scale(scale)
    zero_point = _meta_i64(map_value, "zero_point")
    _validate_zero_point(dtype, zero_point)
    shape = _meta_shape(map_value)
    declared_count = _meta_u64(map_value, "element_count")
    declared_bytes = _meta_u64(map_value, "payload_bytes")
    expected_count = _tensor_product(shape, limits)
    if declared_count != expected_count:
        raise Malformed(
            "quantized tensor element_count %d does not match shape product %d"
            % (declared_count, expected_count)
        )
    if declared_bytes != declared_count:
        raise Malformed(
            "quantized tensor payload_bytes %d does not match element_count %d "
            "for one-byte elements" % (declared_bytes, declared_count)
        )
    if declared_bytes != len(payload):
        raise Malformed(
            "quantized tensor payload is %d bytes but metadata declares %d"
            % (len(payload), declared_bytes)
        )
    declared_hash = _meta_bytes(map_value, "payload_hash")
    if len(declared_hash) != 32:
        raise Malformed(
            "quantized tensor payload_hash must be 32 bytes, got %d"
            % len(declared_hash)
        )
    if declared_hash != hash_domain(DOMAIN_QUANTIZED_TENSOR_PAYLOAD, payload):
        raise Corrupt("quantized tensor payload_hash mismatch")
    meta = {
        "schema": schema,
        "dtype": dtype,
        "shape": shape,
        "layout": layout,
        "quantization": quantization,
        "scale": scale,
        "zero_point": zero_point,
        "element_count": declared_count,
        "payload_bytes": declared_bytes,
    }
    return meta, payload


def quantized_tensor_metadata(frame: Frame, limits: "Limits | None" = None) -> dict:
    meta, _ = _parse_quantized_body(frame, limits or Limits())
    return meta
