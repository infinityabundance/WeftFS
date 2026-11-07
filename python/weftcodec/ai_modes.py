"""AI-mode payload decoders (WEFT-AI-CODEC §5 and §11-§17).

Modes implemented here:

* 9  ``FLOAT_TENSOR``
* 10 ``TENSOR_SHARDED``
* 11 ``WEIGHT_DELTA``
* 12 ``KV_CACHE``
* 13 ``ACTIVATION_STREAM``
* 14 ``SEMANTIC_DELTA``
* 15 ``TENSOR_TILED``
* 16 ``SPARSE_DELTA``
* 17 ``FLOAT_TILED``

Every function is an independent decoder written from the normative spec and
the canonical WCB reader in :mod:`weftcodec.wcb`. No Rust or Go code is loaded
or shelled out to. Error classes follow the normative validation order; there
is no approximate or lossy fallback.
"""

from __future__ import annotations

import math
import struct
from dataclasses import dataclass

from . import huffman, varint, wcb
from .errors import (
    Corrupt,
    DependencyMismatch,
    DependencyMissing,
    LimitExceeded,
    Malformed,
    NonCanonical,
)
from .hashing import (
    DOMAIN_AI_STREAM,
    DOMAIN_TENSOR_LOGICAL,
    DOMAIN_TENSOR_SEMANTIC,
    DOMAIN_TENSOR_SEMANTIC_VALUES,
    DOMAIN_TENSOR_TILE,
    DOMAIN_TENSOR_FLOAT_TILE,
    DOMAIN_TENSOR_FLOAT_TILE_STREAMS,
    DOMAIN_TENSOR_VALUES,
    DOMAIN_TENSOR_SHARDED_PAYLOAD,
    hash_domain,
)
from .limits import Limits

U64_MAX = (1 << 64) - 1
U32_MAX = (1 << 32) - 1
HASH_LEN = 32

FLOAT_TENSOR_SCHEMA = "weft.ai.float_tensor.v1"
FLOAT_TENSOR_CODER = "huffman-canonical-v1"
TENSOR_SHARDED_SCHEMA = "weft.ai.tensor_sharded.v1"
WEIGHT_DELTA_SCHEMA = "weft.ai.weight_delta.v1"
SEMANTIC_DELTA_SCHEMA = "weft.ai.semantic_delta.v1"
KV_CACHE_SCHEMA = "weft.ai.kv_cache.v1"
ACTIVATION_STREAM_SCHEMA = "weft.ai.activation_stream.v1"
TENSOR_TILED_SCHEMA = "weft.ai.tensor_tiled.v1"
SPARSE_DELTA_SCHEMA = "weft.ai.sparse_delta.v1"
SEMANTIC_SCHEMA = "weft.tensor.semantic.v1"
LOGICAL_SCHEMA = "weft.tensor.logical.v1"
PLANE_CODEC = "huffman-canonical-v1"
DELTA_OP = "mod-sub"
SPARSE_CODEC = "zero-run-v1"
TILE_LAYOUT = "row_major"
BLOCK_SIZES = (16, 32, 64, 128, 256)
MAX_BLOCK_LEVELS = 1 << 24

FLOAT_FIELD_BITS = {
    "bf16": (1, 8, 7),
    "fp16": (1, 5, 10),
    "fp32": (1, 8, 23),
    "fp8-e4m3": (1, 4, 3),
    "fp8-e5m2": (1, 5, 2),
}

WEIGHT_WORD_BITS = {"bf16": 16, "fp16": 16, "fp32": 32, "q8_0": 8, "q4_0": 4}
WEIGHT_STORAGE_BYTES = {"bf16": 2, "fp16": 2, "fp32": 4, "q8_0": 1, "q4_0": 1}
WEIGHT_PLANES = {"bf16": 2, "fp16": 2, "fp32": 4, "q8_0": 1, "q4_0": 1}
WEIGHT_MANTISSA_BITS = {"bf16": 7, "fp16": 10, "fp32": 23}

SHARD_STORAGE_BYTES = {"q8_0": 1, "q4_0": 1}

KV_WORD_BYTES = {"bf16": 2, "fp16": 2, "fp32": 4}
KV_PLANES = {"bf16": 2, "fp16": 2, "fp32": 4}

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


# ---------------------------------------------------------------------------
# Small checked-arithmetic and metadata helpers
# ---------------------------------------------------------------------------


def _checked_add(left: int, right: int, what: str) -> int:
    value = left + right
    if value > U64_MAX:
        raise LimitExceeded("%s overflows u64" % what)
    return value


def _checked_mul(left: int, right: int, what: str) -> int:
    value = left * right
    if value > U64_MAX:
        raise LimitExceeded("%s overflows u64" % what)
    return value


def _check_len(length: int, maximum: int, what: str) -> None:
    if length > maximum:
        raise LimitExceeded("%s length %d exceeds limit %d" % (what, length, maximum))


def _map(value, what: str) -> dict:
    if not isinstance(value, dict):
        raise Malformed("%s metadata must be a WCB map" % what)
    return value


def _exact_keys(map_value: dict, expected, what: str) -> None:
    for key in map_value:
        if key not in expected:
            raise Malformed("%s metadata has unknown key `%s`" % (what, key))
    for key in expected:
        if key not in map_value:
            raise Malformed("%s metadata is missing key `%s`" % (what, key))


def _text(map_value: dict, key: str) -> str:
    value = map_value.get(key)
    if not isinstance(value, str):
        raise Malformed("metadata `%s` must be text" % key)
    return value


def _u64(map_value: dict, key: str) -> int:
    value = map_value.get(key)
    if not isinstance(value, wcb.U64):
        raise Malformed("metadata `%s` must be u64" % key)
    return value.value


def _u32(map_value: dict, key: str) -> int:
    value = _u64(map_value, key)
    if value > U32_MAX:
        raise Malformed("metadata `%s` = %d does not fit u32" % (key, value))
    return value


def _i64(map_value: dict, key: str) -> int:
    value = map_value.get(key)
    if not isinstance(value, wcb.I64):
        raise Malformed("metadata `%s` must be i64" % key)
    return value.value


def _bytes_of(map_value: dict, key: str) -> bytes:
    value = map_value.get(key)
    if not isinstance(value, (bytes, bytearray)):
        raise Malformed("metadata `%s` must be bytes" % key)
    return bytes(value)


def _hash_field(map_value: dict, key: str) -> bytes:
    value = _bytes_of(map_value, key)
    if len(value) != HASH_LEN:
        raise Malformed(
            "metadata `%s` must be %d bytes, got %d" % (key, HASH_LEN, len(value))
        )
    return value


def _array_u64(map_value: dict, key: str) -> "list[int]":
    value = map_value.get(key)
    if not isinstance(value, list):
        raise Malformed("metadata `%s` must be an array" % key)
    out = []
    for item in value:
        if not isinstance(item, wcb.U64):
            raise Malformed("metadata `%s` elements must be u64" % key)
        out.append(item.value)
    return out


def _shape(map_value: dict) -> "list[int]":
    return _array_u64(map_value, "shape")


def _split_prefix(body: bytes) -> "tuple[bytes, bytes]":
    """Split ``u32le(meta_len) || descriptor || payload`` without a length bias."""
    if len(body) < 4:
        raise Malformed("AI body is shorter than its 4-byte metadata length")
    meta_len = int.from_bytes(body[0:4], "little")
    meta_end = 4 + meta_len
    if meta_end > len(body):
        raise Malformed("AI metadata extends past the frame body")
    return body[4:meta_end], body[meta_end:]


def _split_exact(body: bytes, decoded_len: int) -> "tuple[bytes, bytes]":
    descriptor, payload = _split_prefix(body)
    if len(payload) != decoded_len:
        raise Malformed(
            "AI payload length %d differs from decoded_len %d"
            % (len(payload), decoded_len)
        )
    return descriptor, payload


def _decode_descriptor(descriptor_bytes: bytes, limits: Limits):
    return wcb.decode(descriptor_bytes, limits)


def _u64_vec(values) -> "list[wcb.U64]":
    return [wcb.U64(value) for value in values]


def _validate_shape(shape: "list[int]", limits: Limits, what: str) -> int:
    if not shape:
        raise Malformed("%s shape rank must be at least 1" % what)
    if len(shape) > limits.max_tensor_rank:
        raise LimitExceeded(
            "%s rank %d exceeds limit %d"
            % (what, len(shape), limits.max_tensor_rank)
        )
    product = 1
    for dim in shape:
        if dim == 0:
            raise Malformed("%s shape dimensions must be greater than zero" % what)
        product = _checked_mul(product, dim, "%s shape product" % what)
    if product > limits.max_tensor_elements:
        raise LimitExceeded(
            "%s element count %d exceeds limit %d"
            % (what, product, limits.max_tensor_elements)
        )
    return product


# ---------------------------------------------------------------------------
# Logical roots and semantic roots
# ---------------------------------------------------------------------------


def values_hash(data: bytes) -> bytes:
    return hash_domain(DOMAIN_TENSOR_VALUES, data)


def tensor_logical_root(
    dtype: str, shape: "list[int]", element_count: int, data: bytes, limits: Limits
) -> bytes:
    record = {
        "schema": LOGICAL_SCHEMA,
        "dtype": dtype,
        "shape": _u64_vec(shape),
        "element_count": wcb.U64(element_count),
        "values_hash": values_hash(data),
    }
    return hash_domain(DOMAIN_TENSOR_LOGICAL, wcb.encode(record, limits))


def sharded_logical_root(
    dtype: str,
    shape: "list[int]",
    words: "list[int]",
    scales: "list[int]",
    zero_points: "list[int]",
    limits: Limits,
) -> bytes:
    element_count = _validate_shape(shape, limits, "TENSOR_SHARDED")
    if len(words) != element_count:
        raise Malformed(
            "TENSOR_SHARDED tensor has %d words but shape requires %d"
            % (len(words), element_count)
        )
    if len(scales) != len(zero_points):
        raise Malformed("TENSOR_SHARDED scales and zero_points differ in length")
    material = bytearray()
    for word in words:
        if dtype == "q4_0" and not (-8 <= word <= 7):
            raise Malformed("q4_0 word out of signed 4-bit range")
        material.append(word & 0xFF)
    for scale, zero in zip(scales, zero_points):
        material.extend(struct.pack("<I", scale))
        material.extend(struct.pack("<q", zero))
    record = {
        "schema": LOGICAL_SCHEMA,
        "dtype": dtype,
        "shape": _u64_vec(shape),
        "element_count": wcb.U64(element_count),
        "values_hash": values_hash(bytes(material)),
    }
    return hash_domain(DOMAIN_TENSOR_LOGICAL, wcb.encode(record, limits))


def tile_root(dtype: str, dims: "list[int]", tile_bytes: bytes, limits: Limits) -> bytes:
    record = {
        "dtype": dtype,
        "dims": _u64_vec(dims),
        "element_len": wcb.U64(len(tile_bytes)),
        "values_hash": values_hash(tile_bytes),
    }
    return hash_domain(DOMAIN_TENSOR_TILE, wcb.encode(record, limits))


@dataclass(frozen=True)
class SemanticPolicy:
    scheme: str
    bits: int = 0
    block_size: int = 0
    levels: int = 0

    def to_wcb(self):
        if self.scheme == "none":
            return {"scheme": "none"}
        if self.scheme == "mantissa_truncate":
            return {"bits": wcb.U64(self.bits), "scheme": "mantissa_truncate"}
        return {
            "block_size": wcb.U64(self.block_size),
            "levels": wcb.U64(self.levels),
            "scheme": "block_quantize",
        }

    def validate(self, dtype: str, element_count: int) -> None:
        if self.scheme == "none":
            return
        if self.scheme == "mantissa_truncate":
            maximum = WEIGHT_MANTISSA_BITS.get(dtype)
            if maximum is None:
                raise Malformed(
                    "mantissa_truncate policy is not defined for dtype %s" % dtype
                )
            if self.bits == 0 or self.bits > maximum:
                raise Malformed(
                    "mantissa_truncate bits %d is outside 1..=%d for %s"
                    % (self.bits, maximum, dtype)
                )
            return
        if self.block_size == 0:
            raise Malformed("block_quantize block_size must be greater than zero")
        if self.levels == 0 or self.levels > MAX_BLOCK_LEVELS:
            raise Malformed(
                "block_quantize levels %d is outside 1..=%d"
                % (self.levels, MAX_BLOCK_LEVELS)
            )
        if element_count % self.block_size != 0:
            raise Malformed(
                "block_quantize block_size %d does not divide element_count %d"
                % (self.block_size, element_count)
            )


def parse_semantic_policy(value, dtype: str, element_count: int) -> SemanticPolicy:
    map_value = value if isinstance(value, dict) else None
    if map_value is None:
        raise Malformed("semantic policy must be a WCB map")
    scheme = map_value.get("scheme")
    if not isinstance(scheme, str):
        raise Malformed("semantic policy `scheme` must be text")
    if scheme == "none":
        _exact_keys(map_value, ("scheme",), "semantic policy")
        return SemanticPolicy("none")
    if scheme == "mantissa_truncate":
        _exact_keys(map_value, ("bits", "scheme"), "semantic policy")
        policy = SemanticPolicy("mantissa_truncate", bits=_u64(map_value, "bits"))
        policy.validate(dtype, element_count)
        return policy
    if scheme == "block_quantize":
        _exact_keys(map_value, ("block_size", "levels", "scheme"), "semantic policy")
        policy = SemanticPolicy(
            "block_quantize",
            block_size=_u64(map_value, "block_size"),
            levels=_u64(map_value, "levels"),
        )
        policy.validate(dtype, element_count)
        return policy
    raise Malformed(
        "unknown semantic policy scheme `%s` (v1 allows none, mantissa_truncate, "
        "block_quantize)" % scheme
    )


def _signed_word(dtype: str, word: int) -> int:
    bits = WEIGHT_WORD_BITS[dtype]
    sign = 1 << (bits - 1)
    modulus = 1 << bits
    if word >= sign:
        return word - modulus
    return word


def _raw_word(dtype: str, signed: int) -> int:
    return signed & ((1 << WEIGHT_WORD_BITS[dtype]) - 1)


def _exponent_all_ones(dtype: str, word: int) -> bool:
    if dtype in ("bf16", "fp16"):
        return (word & 0x7F80) == 0x7F80
    if dtype == "fp32":
        return (word & 0x7F800000) == 0x7F800000
    return False


def _word_at(dtype: str, data: bytes, index: int) -> int:
    if dtype in ("bf16", "fp16"):
        offset = index * 2
        return int.from_bytes(data[offset : offset + 2], "little")
    if dtype == "fp32":
        offset = index * 4
        return int.from_bytes(data[offset : offset + 4], "little")
    return data[index]


def _push_word(dtype: str, word: int, out: bytearray) -> None:
    if dtype in ("bf16", "fp16"):
        out.extend(int(word & 0xFFFF).to_bytes(2, "little"))
    elif dtype == "fp32":
        out.extend(int(word & 0xFFFFFFFF).to_bytes(4, "little"))
    else:
        out.append(word & 0xFF)


def _transform_bytes(
    dtype: str, data: bytes, element_count: int, policy: SemanticPolicy
) -> bytes:
    if policy.scheme == "none":
        return bytes(data)
    if policy.scheme == "mantissa_truncate":
        retain = ~((1 << policy.bits) - 1) & ((1 << WEIGHT_WORD_BITS[dtype]) - 1)
        out = bytearray()
        for index in range(element_count):
            word = _word_at(dtype, data, index)
            transformed = (
                word if _exponent_all_ones(dtype, word) else word & retain
            )
            _push_word(dtype, transformed, out)
        return bytes(out)
    block = policy.block_size
    levels = policy.levels
    out = bytearray()
    start = 0
    while start < element_count:
        end = min(start + block, element_count)
        amax = 0
        for index in range(start, end):
            magnitude = abs(_signed_word(dtype, _word_at(dtype, data, index)))
            if magnitude > amax:
                amax = magnitude
        if amax == 0 or levels == 1:
            for _ in range(start, end):
                _push_word(dtype, 0, out)
        else:
            span = 2 * amax
            for index in range(start, end):
                value = _signed_word(dtype, _word_at(dtype, data, index))
                numerator = (value + amax) * (levels - 1) + amax
                k = numerator // span
                if k >= levels:
                    k = levels - 1
                level = (span * k + (levels - 1) // 2) // (levels - 1)
                _push_word(dtype, _raw_word(dtype, level - amax), out)
        start = end
    return bytes(out)


def semantic_root(
    dtype: str, shape: "list[int]", data: bytes, policy: SemanticPolicy, limits: Limits
) -> bytes:
    count = _validate_shape(shape, limits, "semantic tensor")
    expected = _checked_mul(
        count, WEIGHT_STORAGE_BYTES[dtype], "semantic tensor byte length"
    )
    _check_len(expected, limits.max_decoded_bytes, "semantic tensor bytes")
    if expected != len(data):
        raise Malformed(
            "semantic tensor is %d bytes but shape %r x %s requires %d"
            % (len(data), shape, dtype, expected)
        )
    policy.validate(dtype, count)
    if dtype == "q4_0":
        for byte in data:
            if byte > 15:
                raise Corrupt("q4_0 semantic tensor word is outside 0..=15")
    if policy.scheme == "none":
        quantized_values_hash = values_hash(bytes(data))
    else:
        quantized_values_hash = hash_domain(
            DOMAIN_TENSOR_SEMANTIC_VALUES,
            _transform_bytes(dtype, data, count, policy),
        )
    record = {
        "schema": SEMANTIC_SCHEMA,
        "dtype": dtype,
        "shape": _u64_vec(shape),
        "element_count": wcb.U64(count),
        "policy": policy.to_wcb(),
        "quantized_values_hash": quantized_values_hash,
    }
    return hash_domain(DOMAIN_TENSOR_SEMANTIC, wcb.encode(record, limits))


def semantic_record_from_canonical(data: bytes, limits: Limits) -> dict:
    """Parse and validate a canonical ``weft.tensor.semantic.v1`` record."""
    value = wcb.decode(data, limits)
    map_value = _map(value, "semantic root record")
    _exact_keys(
        map_value,
        ("dtype", "element_count", "policy", "quantized_values_hash", "schema", "shape"),
        "semantic root record",
    )
    schema = _text(map_value, "schema")
    if schema != SEMANTIC_SCHEMA:
        raise Malformed("unsupported semantic root schema `%s`" % schema)
    dtype = _weight_dtype(_text(map_value, "dtype"))
    shape = _shape(map_value)
    element_count = _u64(map_value, "element_count")
    policy = parse_semantic_policy(map_value.get("policy"), dtype, element_count)
    quantized_values_hash = _hash_field(map_value, "quantized_values_hash")
    count = _validate_shape(shape, limits, "semantic")
    if element_count != count:
        raise Malformed(
            "semantic element_count %d does not match shape product %d"
            % (element_count, count)
        )
    _checked_mul(count, WEIGHT_STORAGE_BYTES[dtype], "semantic tensor byte length")
    policy.validate(dtype, count)
    record = {
        "schema": schema,
        "dtype": dtype,
        "shape": _u64_vec(shape),
        "element_count": wcb.U64(element_count),
        "policy": policy.to_wcb(),
        "quantized_values_hash": quantized_values_hash,
    }
    if wcb.encode(record, limits) != data:
        raise NonCanonical("semantic root bytes are not in canonical form")
    return record


# ---------------------------------------------------------------------------
# Mode 9 -- FLOAT_TENSOR
# ---------------------------------------------------------------------------

_FLOAT_KEYS = (
    "schema",
    "dtype",
    "shape",
    "element_count",
    "sign_bits",
    "exponent_bits",
    "mantissa_bits",
    "exponent_coder",
    "exponent_plane_len",
    "mantissa_plane_len",
    "payload_hash",
)


@dataclass(frozen=True)
class FloatTensorMeta:
    dtype: str
    shape: "list[int]"
    element_count: int
    sign_bits: int
    exponent_bits: int
    mantissa_bits: int
    exponent_plane_len: int
    mantissa_plane_len: int


def _float_dtype(name: str) -> str:
    if name not in FLOAT_FIELD_BITS:
        raise Malformed(
            "unknown float tensor dtype `%s` (v1 allows bf16, fp16, fp32, "
            "fp8-e4m3, fp8-e5m2)" % name
        )
    return name


def _byte_field(map_value: dict, key: str) -> int:
    value = _u64(map_value, key)
    if value > 0xFF:
        raise Malformed(
            "float tensor `%s` must fit an unsigned byte, got %d" % (key, value)
        )
    return value


def parse_float_descriptor(
    frame, limits: Limits
) -> "tuple[FloatTensorMeta, bytes, bytes, bytes, bytes]":
    descriptor_bytes, stored = _split_prefix(frame.body)
    map_value = _map(_decode_descriptor(descriptor_bytes, limits), "float tensor")
    _exact_keys(map_value, _FLOAT_KEYS, "float tensor")
    schema = _text(map_value, "schema")
    if schema != FLOAT_TENSOR_SCHEMA:
        raise Malformed("unsupported float tensor schema `%s`" % schema)
    dtype = _float_dtype(_text(map_value, "dtype"))
    coder = _text(map_value, "exponent_coder")
    if coder != PLANE_CODEC:
        raise Malformed(
            "unsupported float tensor exponent coder `%s` (v1 allows `%s`)"
            % (coder, PLANE_CODEC)
        )
    sign_bits = _byte_field(map_value, "sign_bits")
    exponent_bits = _byte_field(map_value, "exponent_bits")
    mantissa_bits = _byte_field(map_value, "mantissa_bits")
    if (sign_bits, exponent_bits, mantissa_bits) != FLOAT_FIELD_BITS[dtype]:
        raise Malformed(
            "float tensor sub-field widths %d, %d, %d disagree with dtype `%s`"
            % (sign_bits, exponent_bits, mantissa_bits, dtype)
        )
    element_bits = sign_bits + exponent_bits + mantissa_bits
    if element_bits % 8 != 0:
        raise Malformed("float tensor element width is not byte-aligned")
    element_size = element_bits // 8
    shape = _shape(map_value)
    declared_count = _u64(map_value, "element_count")
    expected_count = _validate_shape(shape, limits, "float tensor")
    if declared_count != expected_count:
        raise Malformed(
            "float tensor element_count %d does not match shape product %d"
            % (declared_count, expected_count)
        )
    decoded_bytes = _checked_mul(
        declared_count, element_size, "float tensor decoded byte count"
    )
    _check_len(decoded_bytes, limits.max_decoded_bytes, "float tensor decoded bytes")
    if decoded_bytes != frame.header.decoded_len:
        raise Corrupt(
            "float tensor decoded_len is %d but the descriptor requires %d"
            % (frame.header.decoded_len, decoded_bytes)
        )
    mantissa_bits_total = _checked_mul(
        declared_count,
        sign_bits + mantissa_bits,
        "float tensor mantissa bit count",
    )
    expected_mantissa = _checked_add(
        mantissa_bits_total, 7, "float tensor mantissa length"
    ) // 8
    declared_mantissa = _u64(map_value, "mantissa_plane_len")
    if declared_mantissa != expected_mantissa:
        raise Malformed(
            "float tensor mantissa_plane_len %d does not match element_count x "
            "(sign_bits + mantissa_bits) / 8 %d"
            % (declared_mantissa, expected_mantissa)
        )
    declared_exponent = _u64(map_value, "exponent_plane_len")
    stored_len = _checked_add(
        declared_exponent, declared_mantissa, "float tensor plane lengths"
    )
    if stored_len != len(stored):
        raise Malformed(
            "float tensor stored payload is %d bytes but the descriptor declares %d"
            % (len(stored), stored_len)
        )
    exponent_compressed = stored[:declared_exponent]
    mantissa_plane = stored[declared_exponent:]
    meta = FloatTensorMeta(
        dtype=dtype,
        shape=shape,
        element_count=declared_count,
        sign_bits=sign_bits,
        exponent_bits=exponent_bits,
        mantissa_bits=mantissa_bits,
        exponent_plane_len=declared_exponent,
        mantissa_plane_len=declared_mantissa,
    )
    return meta, stored, exponent_compressed, mantissa_plane, _bytes_of(
        map_value, "payload_hash"
    )


def _join_float_planes(
    exponent_plane: bytes,
    mantissa_plane: bytes,
    dtype: str,
    element_count: int,
    limits: Limits,
) -> bytes:
    if len(exponent_plane) != element_count:
        raise Corrupt(
            "float tensor exponent plane has %d bytes but element_count is %d"
            % (len(exponent_plane), element_count)
        )
    sign_bits, exponent_bits, mantissa_bits = FLOAT_FIELD_BITS[dtype]
    element_size = (sign_bits + exponent_bits + mantissa_bits) // 8
    decoded_bytes = _checked_mul(
        element_count, element_size, "float tensor decoded byte count"
    )
    _check_len(decoded_bytes, limits.max_decoded_bytes, "float tensor decoded bytes")
    expected_mantissa_bits = _checked_mul(
        element_count,
        sign_bits + mantissa_bits,
        "float tensor mantissa bit count",
    )
    expected_mantissa_bytes = _checked_add(
        expected_mantissa_bits, 7, "float tensor mantissa length"
    ) // 8
    if len(mantissa_plane) != expected_mantissa_bytes:
        raise Corrupt(
            "float tensor mantissa plane has %d bytes but descriptor arithmetic "
            "requires %d" % (len(mantissa_plane), expected_mantissa_bytes)
        )

    width = sign_bits + mantissa_bits
    bit = 0
    total_bits = len(mantissa_plane) * 8
    out = bytearray(decoded_bytes)
    mantissa_mask = (1 << mantissa_bits) - 1
    for index, exponent_byte in enumerate(exponent_plane):
        if exponent_byte >> exponent_bits:
            raise Corrupt(
                "float tensor exponent byte exceeds the dtype exponent width"
            )
        value = 0
        for _ in range(width):
            if bit >= total_bits:
                # The plane is exactly long enough by construction; this is an
                # internal invariant, reported as corruption rather than panic.
                raise Corrupt("float tensor mantissa plane is truncated")
            byte = mantissa_plane[bit >> 3]
            value = (value << 1) | ((byte >> (7 - (bit & 7))) & 1)
            bit += 1
        sign = value >> mantissa_bits
        mantissa = value & mantissa_mask
        word = (
            (sign << (exponent_bits + mantissa_bits))
            | (exponent_byte << mantissa_bits)
            | mantissa
        )
        offset = index * element_size
        out[offset : offset + element_size] = word.to_bytes(element_size, "little")

    padding = len(mantissa_plane) * 8 - expected_mantissa_bits
    if padding > 0:
        last = mantissa_plane[-1]
        if last & ((1 << padding) - 1):
            raise Corrupt("float tensor mantissa padding bits must be zero")
    return bytes(out)


def decode_float_tensor(frame, limits: Limits) -> bytes:
    meta, stored, exponent_compressed, mantissa_plane, declared_hash = (
        parse_float_descriptor(frame, limits)
    )
    exponent_plane = huffman.decode(exponent_compressed, meta.element_count)
    out = _join_float_planes(
        exponent_plane,
        mantissa_plane,
        meta.dtype,
        meta.element_count,
        limits,
    )
    if len(declared_hash) != HASH_LEN:
        raise Malformed(
            "float tensor payload_hash must be %d bytes, got %d"
            % (HASH_LEN, len(declared_hash))
        )
    if declared_hash != hash_domain("weft/float-tensor/payload/v1", stored):
        raise Corrupt("float tensor payload_hash mismatch")
    return out


# ---------------------------------------------------------------------------
# Mode 10 -- TENSOR_SHARDED
# ---------------------------------------------------------------------------

_SHARDED_KEYS = (
    "schema",
    "dtype",
    "shape",
    "element_count",
    "shard_axis",
    "shard_count",
    "shard_index",
    "block_size",
    "scales",
    "zero_points",
    "layout_root",
    "payload_len",
    "payload_hash",
)


@dataclass
class ShardedMeta:
    dtype: str
    shape: "list[int]"
    element_count: int
    shard_axis: int
    shard_count: int
    shard_index: int
    block_size: int
    scales: "list[int]"
    zero_points: "list[int]"
    layout_root: bytes
    payload_len: int


@dataclass
class DecodedShard:
    meta: ShardedMeta
    words: "list[int]"
    planes: bytes


def _sharded_dtype(name: str) -> str:
    if name not in ("q8_0", "q4_0"):
        raise Malformed(
            "unknown TENSOR_SHARDED dtype `%s` (v1 allows q8_0, q4_0)" % name
        )
    return name


def _u32_array(map_value: dict, key: str) -> "list[int]":
    values = _array_u64(map_value, key)
    for value in values:
        if value > U32_MAX:
            raise Malformed(
                "TENSOR_SHARDED scale bits exceed the 32-bit f32 pattern"
            )
    return values


def _i64_array(map_value: dict, key: str) -> "list[int]":
    value = map_value.get(key)
    if not isinstance(value, list):
        raise Malformed("metadata `%s` must be an array" % key)
    out = []
    for item in value:
        if not isinstance(item, wcb.I64):
            raise Malformed("metadata `%s` elements must be i64" % key)
        out.append(item.value)
    return out


def parse_sharded_descriptor(descriptor_bytes: bytes, limits: Limits):
    map_value = _map(_decode_descriptor(descriptor_bytes, limits), "TENSOR_SHARDED")
    _exact_keys(map_value, _SHARDED_KEYS, "TENSOR_SHARDED")
    schema = _text(map_value, "schema")
    if schema != TENSOR_SHARDED_SCHEMA:
        raise Malformed("unsupported TENSOR_SHARDED schema `%s`" % schema)
    dtype = _sharded_dtype(_text(map_value, "dtype"))
    shape = _shape(map_value)
    element_count = _u64(map_value, "element_count")
    shard_axis = _u32(map_value, "shard_axis")
    shard_count = _u32(map_value, "shard_count")
    shard_index = _u32(map_value, "shard_index")
    block_size = _u64(map_value, "block_size")
    scales = _u32_array(map_value, "scales")
    zero_points = _i64_array(map_value, "zero_points")
    layout_root = _hash_field(map_value, "layout_root")
    payload_len = _u64(map_value, "payload_len")
    payload_hash = _hash_field(map_value, "payload_hash")
    meta = ShardedMeta(
        dtype=dtype,
        shape=shape,
        element_count=element_count,
        shard_axis=shard_axis,
        shard_count=shard_count,
        shard_index=shard_index,
        block_size=block_size,
        scales=scales,
        zero_points=zero_points,
        layout_root=layout_root,
        payload_len=payload_len,
    )
    return meta, payload_hash


def _validate_sharded_meta(
    meta: ShardedMeta, limits: Limits
) -> "tuple[int, int, int]":
    element_count = _validate_shape(meta.shape, limits, "TENSOR_SHARDED")
    if meta.element_count != element_count:
        raise Malformed(
            "TENSOR_SHARDED element_count %d does not match shape product %d"
            % (meta.element_count, element_count)
        )
    if meta.shard_count == 0:
        raise Malformed("TENSOR_SHARDED shard_count must be at least 1")
    if meta.shard_axis >= len(meta.shape):
        raise Malformed(
            "TENSOR_SHARDED shard_axis %d is not below rank %d"
            % (meta.shard_axis, len(meta.shape))
        )
    if meta.shard_index >= meta.shard_count:
        raise Malformed(
            "TENSOR_SHARDED shard_index %d is not below shard_count %d"
            % (meta.shard_index, meta.shard_count)
        )
    axis_dim = meta.shape[meta.shard_axis]
    if axis_dim % meta.shard_count != 0:
        raise Malformed(
            "TENSOR_SHARDED shape[%d] = %d is not divisible by shard_count %d"
            % (meta.shard_axis, axis_dim, meta.shard_count)
        )
    if element_count % meta.shard_count != 0:
        raise Malformed(
            "TENSOR_SHARDED element_count %d is not divisible by shard_count %d"
            % (element_count, meta.shard_count)
        )
    shard_elements = element_count // meta.shard_count
    if meta.block_size not in BLOCK_SIZES:
        raise Malformed(
            "TENSOR_SHARDED block_size %d is not one of 16, 32, 64, 128, 256"
            % meta.block_size
        )
    if element_count % meta.block_size != 0:
        raise Malformed(
            "TENSOR_SHARDED element_count %d is not divisible by block_size %d"
            % (element_count, meta.block_size)
        )
    block_count = element_count // meta.block_size
    if block_count > limits.max_structured_items:
        raise LimitExceeded(
            "TENSOR_SHARDED block count %d exceeds WCB item limit %d"
            % (block_count, limits.max_structured_items)
        )
    if len(meta.scales) != block_count or len(meta.zero_points) != block_count:
        raise Malformed(
            "TENSOR_SHARDED descriptor has %d scales and %d zero points but "
            "requires %d"
            % (len(meta.scales), len(meta.zero_points), block_count)
        )
    for bits in meta.scales:
        scale = struct.unpack("<f", struct.pack("<I", bits))[0]
        if not math.isfinite(scale):
            raise Malformed("TENSOR_SHARDED scale bits must be a finite f32")
        if scale <= 0.0:
            raise Malformed("TENSOR_SHARDED scale must be greater than zero")
    if meta.dtype == "q4_0" and shard_elements % 2 != 0:
        raise Malformed(
            "q4_0 shard element count must be even to pack nibble pairs"
        )
    quant_len = shard_elements if meta.dtype == "q8_0" else shard_elements // 2
    planes_len = _checked_add(
        quant_len, _checked_mul(block_count, 4, "TENSOR_SHARDED scale plane"),
        "TENSOR_SHARDED plane length",
    )
    _check_len(
        planes_len, limits.max_decoded_bytes, "TENSOR_SHARDED reconstructed planes"
    )
    return element_count, shard_elements, block_count


def decode_tensor_sharded(frame, limits: Limits) -> DecodedShard:
    descriptor_bytes, stored = _split_prefix(frame.body)
    meta, declared_payload_hash = parse_sharded_descriptor(descriptor_bytes, limits)
    _element_count, shard_elements, block_count = _validate_sharded_meta(meta, limits)
    if meta.payload_len != len(stored):
        raise Malformed(
            "TENSOR_SHARDED payload is %d bytes but payload_len declares %d"
            % (len(stored), meta.payload_len)
        )
    quant_len = shard_elements if meta.dtype == "q8_0" else shard_elements // 2
    planes_len = _checked_add(
        quant_len, _checked_mul(block_count, 4, "TENSOR_SHARDED scale plane"),
        "TENSOR_SHARDED plane length",
    )
    if frame.header.decoded_len != planes_len:
        raise Corrupt(
            "TENSOR_SHARDED decoded_len is %d but the descriptor requires %d"
            % (frame.header.decoded_len, planes_len)
        )
    if declared_payload_hash != hash_domain(DOMAIN_TENSOR_SHARDED_PAYLOAD, stored):
        raise Corrupt("TENSOR_SHARDED payload_hash mismatch")
    _check_len(
        planes_len, limits.max_decoded_bytes, "TENSOR_SHARDED reconstructed planes"
    )
    planes = huffman.decode(stored, planes_len)
    if quant_len > len(planes):
        raise Corrupt(
            "TENSOR_SHARDED reconstructed plane length is inconsistent"
        )
    quant_plane = planes[:quant_len]
    scale_bytes = planes[quant_len:]
    if len(scale_bytes) != block_count * 4:
        raise Corrupt("TENSOR_SHARDED scale plane length is inconsistent")
    for index in range(block_count):
        chunk = scale_bytes[index * 4 : index * 4 + 4]
        actual = int.from_bytes(chunk, "little")
        if actual != meta.scales[index]:
            raise Corrupt(
                "TENSOR_SHARDED scale plane block %d does not match the descriptor"
                % index
            )
    if frame.header.content_hash != hash_domain("weft/chunk/v1", planes):
        raise Corrupt(
            "TENSOR_SHARDED content_hash mismatch against the reconstructed planes"
        )
    if meta.dtype == "q8_0":
        words = [byte - 256 if byte >= 128 else byte for byte in quant_plane]
    else:
        words = []
        for byte in quant_plane:
            low = byte & 0x0F
            high = byte >> 4
            words.append(low - 16 if low >= 8 else low)
            words.append(high - 16 if high >= 8 else high)
        words = words[:shard_elements]
    if len(words) != shard_elements:
        raise Corrupt(
            "TENSOR_SHARDED reconstructed word count is inconsistent"
        )
    return DecodedShard(meta=meta, words=words, planes=planes)


def _same_sharded_tensor(left: ShardedMeta, right: ShardedMeta) -> bool:
    return (
        left.dtype == right.dtype
        and left.shape == right.shape
        and left.element_count == right.element_count
        and left.shard_axis == right.shard_axis
        and left.shard_count == right.shard_count
        and left.block_size == right.block_size
        and left.scales == right.scales
        and left.zero_points == right.zero_points
        and left.layout_root == right.layout_root
    )


def _assemble_shard_words(
    shape: "list[int]", axis: int, shards: "list[list[int]]"
) -> "list[int]":
    count = len(shards)
    shard_shape = list(shape)
    shard_shape[axis] = shard_shape[axis] // count
    total = 1
    for dim in shape:
        total *= dim
    out = [0] * total
    plane = shape[axis] // count
    strides = [0] * len(shape)
    stride = 1
    for index in range(len(shape) - 1, -1, -1):
        strides[index] = stride
        stride *= shape[index]
    for shard_index, payload in enumerate(shards):
        # Iterate the shard-local row-major coordinates.
        def walk(dimension: int, local_coord: "list[int]", local_flat: int) -> int:
            if dimension == len(shard_shape):
                full = 0
                for axis_index, coord in enumerate(local_coord):
                    full_coord = coord
                    if axis_index == axis:
                        full_coord = shard_index * plane + coord
                    full += full_coord * strides[axis_index]
                out[full] = payload[local_flat]
                return local_flat + 1
            extent = shard_shape[dimension]
            for position in range(extent):
                local_coord[dimension] = position
                local_flat = walk(dimension + 1, local_coord, local_flat)
            return local_flat

        walk(0, [0] * len(shard_shape), 0)
    return out


def shard_set_root(
    shards: "list[DecodedShard]", limits: Limits
) -> "tuple[bytes, list[int]]":
    if not shards:
        raise Malformed("TENSOR_SHARDED shard set is empty")
    first = shards[0].meta
    _validate_sharded_meta(first, limits)
    count = first.shard_count
    if len(shards) != count:
        raise Malformed(
            "TENSOR_SHARDED shard set has %d entries but the descriptor declares %d"
            % (len(shards), count)
        )
    ordered: "list[DecodedShard | None]" = [None] * count
    for shard in shards:
        if not _same_sharded_tensor(first, shard.meta):
            raise NonCanonical(
                "TENSOR_SHARDED shard descriptors disagree on tensor identity"
            )
        index = shard.meta.shard_index
        if index >= count:
            raise NonCanonical(
                "TENSOR_SHARDED shard index %d is not below shard count %d"
                % (index, count)
            )
        if ordered[index] is not None:
            raise NonCanonical(
                "duplicate TENSOR_SHARDED shard index %d" % index
            )
        ordered[index] = shard
    for index, entry in enumerate(ordered):
        if entry is None:
            raise NonCanonical("missing TENSOR_SHARDED shard index %d" % index)
    words = _assemble_shard_words(
        first.shape, first.shard_axis, [entry.words for entry in ordered]
    )
    root = sharded_logical_root(
        first.dtype,
        first.shape,
        words,
        first.scales,
        first.zero_points,
        limits,
    )
    if root != first.layout_root:
        raise Corrupt(
            "TENSOR_SHARDED shard set recomputes a different logical root"
        )
    return root, words


# ---------------------------------------------------------------------------
# Modes 11/14/16 -- delta modes
# ---------------------------------------------------------------------------


def _weight_dtype(name: str) -> str:
    if name not in WEIGHT_WORD_BITS:
        raise Malformed(
            "unknown WEIGHT_DELTA dtype `%s` (v1 allows bf16, fp16, fp32, "
            "q8_0, q4_0)" % name
        )
    return name


@dataclass
class WeightDeltaMeta:
    dtype: str
    shape: "list[int]"
    element_count: int
    word_bits: int
    base: bytes
    base_dtype: str
    base_shape: "list[int]"
    op: str
    planes: int
    plane_codec: str
    delta_root: bytes
    payload_len: int


_WEIGHT_KEYS = (
    "schema",
    "dtype",
    "shape",
    "element_count",
    "word_bits",
    "base",
    "base_dtype",
    "base_shape",
    "op",
    "planes",
    "plane_codec",
    "delta_root",
    "payload_len",
)


def parse_weight_descriptor(descriptor_bytes: bytes, limits: Limits) -> WeightDeltaMeta:
    map_value = _map(_decode_descriptor(descriptor_bytes, limits), "WEIGHT_DELTA")
    _exact_keys(map_value, _WEIGHT_KEYS, "WEIGHT_DELTA")
    schema = _text(map_value, "schema")
    if schema != WEIGHT_DELTA_SCHEMA:
        raise Malformed("unsupported WEIGHT_DELTA schema `%s`" % schema)
    dtype = _weight_dtype(_text(map_value, "dtype"))
    shape = _shape(map_value)
    element_count = _u64(map_value, "element_count")
    word_bits = _u64(map_value, "word_bits")
    base = _hash_field(map_value, "base")
    base_dtype = _weight_dtype(_text(map_value, "base_dtype"))
    base_shape = _array_u64(map_value, "base_shape")
    op = _text(map_value, "op")
    planes = _u64(map_value, "planes")
    plane_codec = _text(map_value, "plane_codec")
    delta_root = _hash_field(map_value, "delta_root")
    payload_len = _u64(map_value, "payload_len")
    meta = WeightDeltaMeta(
        dtype=dtype,
        shape=shape,
        element_count=element_count,
        word_bits=word_bits,
        base=base,
        base_dtype=base_dtype,
        base_shape=base_shape,
        op=op,
        planes=planes,
        plane_codec=plane_codec,
        delta_root=delta_root,
        payload_len=payload_len,
    )
    validate_weight_meta(meta, limits)
    return meta


def validate_weight_meta(meta: WeightDeltaMeta, limits: Limits) -> int:
    if meta.op != DELTA_OP:
        raise Malformed(
            "WEIGHT_DELTA op `%s` is not implemented (only %s)"
            % (meta.op, DELTA_OP)
        )
    if meta.plane_codec != PLANE_CODEC:
        raise Malformed(
            "WEIGHT_DELTA plane_codec `%s` is not implemented (only %s)"
            % (meta.plane_codec, PLANE_CODEC)
        )
    if WEIGHT_WORD_BITS[meta.dtype] != meta.word_bits:
        raise Malformed(
            "WEIGHT_DELTA word_bits %d disagrees with dtype %s (%d bits)"
            % (meta.word_bits, meta.dtype, WEIGHT_WORD_BITS[meta.dtype])
        )
    if WEIGHT_PLANES[meta.dtype] != meta.planes:
        raise Malformed(
            "WEIGHT_DELTA planes %d disagrees with dtype %s (%d planes)"
            % (meta.planes, meta.dtype, WEIGHT_PLANES[meta.dtype])
        )
    if meta.base_dtype != meta.dtype:
        raise Malformed(
            "WEIGHT_DELTA base_dtype `%s` must equal dtype `%s`"
            % (meta.base_dtype, meta.dtype)
        )
    if meta.base_shape != meta.shape:
        raise Malformed("WEIGHT_DELTA base_shape must equal shape")
    count = _validate_shape(meta.shape, limits, "WEIGHT_DELTA")
    if meta.element_count != count:
        raise Malformed(
            "WEIGHT_DELTA element_count %d does not match shape product %d"
            % (meta.element_count, count)
        )
    _checked_mul(
        count, WEIGHT_STORAGE_BYTES[meta.dtype],
        "WEIGHT_DELTA element byte length",
    )
    return count


def weight_metadata(frame, limits: Limits) -> WeightDeltaMeta:
    descriptor_bytes, _payload = _split_prefix(frame.body)
    return parse_weight_descriptor(descriptor_bytes, limits)


def _decode_planes(dtype: str, payload: bytes, element_count: int, what: str):
    plane_count = WEIGHT_PLANES[dtype]
    planes = []
    offset = 0
    for _ in range(plane_count):
        if offset >= len(payload):
            raise Corrupt("%s payload ends before all declared planes" % what)
        plane, consumed = huffman.decode_prefix(payload[offset:], element_count)
        offset += consumed
        planes.append(plane)
    if offset != len(payload):
        raise Corrupt(
            "%s payload has trailing bytes after the declared planes" % what
        )
    return planes


def _delta_word_from_planes(dtype: str, planes, index: int) -> int:
    if dtype in ("bf16", "fp16"):
        return planes[0][index] | (planes[1][index] << 8)
    if dtype == "fp32":
        return (
            planes[0][index]
            | (planes[1][index] << 8)
            | (planes[2][index] << 16)
            | (planes[3][index] << 24)
        )
    if dtype == "q8_0":
        return planes[0][index]
    nibble = planes[0][index]
    if nibble > 15:
        raise Corrupt(
            "WEIGHT_DELTA q4_0 delta word %d is %d, outside 0..=15"
            % (index, nibble)
        )
    return nibble


def _build_delta_bytes(dtype: str, planes, count: int) -> bytes:
    out = bytearray()
    for index in range(count):
        _push_word(dtype, _delta_word_from_planes(dtype, planes, index), out)
    return bytes(out)


def _mod_mask(dtype: str) -> int:
    return (1 << WEIGHT_WORD_BITS[dtype]) - 1


def _base_frame_descriptor(frame, limits: Limits) -> "tuple[str, list[int]]":
    from . import modes as _modes

    mode = frame.header.mode
    if mode == 6:
        meta, _payload = _modes._parse_tensor_body(frame, limits)
        mapped = {
            "bf16": "bf16",
            "f16": "fp16",
            "f32": "fp32",
            "i8": "q8_0",
            "u8": "q4_0",
        }.get(meta["dtype"])
        if mapped is None:
            raise Malformed(
                "base frame dtype `%s` cannot back a WEIGHT_DELTA word plane"
                % meta["dtype"]
            )
        return mapped, meta["shape"]
    if mode == 9:
        meta = parse_float_descriptor(frame, limits)[0]
        mapped = {
            "bf16": "bf16",
            "fp16": "fp16",
            "fp32": "fp32",
        }.get(meta.dtype)
        if mapped is None:
            raise Malformed(
                "base frame dtype `%s` cannot back a WEIGHT_DELTA word plane"
                % meta.dtype
            )
        return mapped, meta.shape
    if mode == 11:
        meta = weight_metadata(frame, limits)
        return meta.dtype, meta.shape
    if mode == 14:
        meta = semantic_delta_metadata(frame, limits)
        return meta.dtype, meta.shape
    if mode == 16:
        meta = sparse_delta_metadata(frame, limits)
        return meta.dtype, meta.shape
    raise Malformed(
        "base frame mode `%s` does not carry tensor dtype/shape metadata"
        % frame.header.mode_name
    )


def _base_bytes(
    frame,
    meta_base: bytes,
    resolver,
    limits: Limits,
    base_frame,
    what: str,
) -> "tuple[bytes, bytes | None]":
    from .modes import decode_bytes as _decode_bytes

    if base_frame is not None:
        return _decode_bytes(base_frame, resolver, limits), base_frame
    if resolver is None:
        raise DependencyMissing(
            "frame needs dependency %s but no resolver was supplied"
            % meta_base.hex()
        )
    fetched = resolver.resolve_frame(meta_base)
    data = resolver.resolve(meta_base, frame.header.decoded_len, 0)
    if not isinstance(data, (bytes, bytearray, memoryview)):
        raise DependencyMismatch("resolver returned a non-bytes value")
    data = bytes(data)
    if len(data) != frame.header.decoded_len:
        raise DependencyMismatch(
            "dependency length %d differs from declared %d"
            % (len(data), frame.header.decoded_len)
        )
    from .hashing import content_root

    if content_root(data) != meta_base:
        raise DependencyMismatch("dependency content hash mismatch")
    return data, fetched


def _verify_base_frame(
    supplied: bytes, meta, limits: Limits, what: str
) -> None:
    from .frame import parse as _parse

    parsed = _parse(supplied, limits)
    if parsed.header.content_hash != meta.base:
        raise DependencyMismatch(
            "%s supplied base frame content hash does not equal the descriptor base"
            % what
        )
    try:
        base_dtype, base_shape = _base_frame_descriptor(parsed, limits)
    except (Malformed, LimitExceeded) as exc:
        raise DependencyMismatch(exc.message)
    if base_dtype != meta.dtype:
        raise DependencyMismatch(
            "%s base frame dtype `%s` does not match %s dtype `%s`"
            % (what, base_dtype, what, meta.dtype)
        )
    if base_shape != meta.shape:
        raise DependencyMismatch(
            "%s base frame shape does not match the %s shape" % (what, what)
        )


def _check_quantized(dtype: str, data: bytes, what: str) -> None:
    if dtype == "q4_0":
        for index, byte in enumerate(data):
            if byte > 15:
                raise Corrupt(
                    "%s q4_0 word %d is %d, outside 0..=15"
                    % (what, index, byte)
                )


def decode_weight_delta(frame, resolver, limits: Limits, base_frame=None) -> bytes:
    descriptor_bytes, payload = _split_prefix(frame.body)
    meta = parse_weight_descriptor(descriptor_bytes, limits)
    count = validate_weight_meta(meta, limits)
    expected_len = _checked_mul(
        count, WEIGHT_STORAGE_BYTES[meta.dtype],
        "WEIGHT_DELTA element byte length",
    )
    if frame.header.decoded_len != expected_len:
        raise Corrupt(
            "WEIGHT_DELTA decoded_len is %d but the descriptor requires %d"
            % (frame.header.decoded_len, expected_len)
        )
    _check_len(expected_len, limits.max_decoded_bytes, "WEIGHT_DELTA element bytes")
    if meta.payload_len != len(payload):
        raise Malformed(
            "WEIGHT_DELTA payload is %d bytes but payload_len declares %d"
            % (len(payload), meta.payload_len)
        )
    if frame.header.dep_count != 1:
        raise Malformed("WEIGHT_DELTA requires exactly one dependency")
    dep = frame.dependencies[0]
    if dep.hash != meta.base:
        raise Malformed(
            "WEIGHT_DELTA dependency hash must equal the descriptor base"
        )
    if dep.decoded_len != frame.header.decoded_len:
        raise Malformed(
            "WEIGHT_DELTA dependency decoded_len must equal the frame decoded_len"
        )
    planes = _decode_planes(meta.dtype, payload, count, "WEIGHT_DELTA")
    delta_bytes = _build_delta_bytes(meta.dtype, planes, count)
    if values_hash(delta_bytes) != meta.delta_root:
        raise Corrupt("WEIGHT_DELTA delta_root mismatch")

    base_bytes, supplied = _base_bytes(
        frame, meta.base, resolver, limits, base_frame, "WEIGHT_DELTA"
    )
    if supplied is not None:
        _verify_base_frame(supplied, meta, limits, "WEIGHT_DELTA")
    if len(base_bytes) != expected_len:
        raise DependencyMismatch(
            "base frame decodes to %d bytes but the descriptor requires %d"
            % (len(base_bytes), expected_len)
        )
    _check_quantized(meta.dtype, base_bytes, "WEIGHT_DELTA")

    mask = _mod_mask(meta.dtype)
    target = bytearray()
    for index in range(count):
        delta = _delta_word_from_planes(meta.dtype, planes, index)
        base_word = _word_at(meta.dtype, base_bytes, index)
        _push_word(meta.dtype, (delta + base_word) & mask, target)
    if len(target) != expected_len:
        raise Corrupt(
            "WEIGHT_DELTA reconstructed element length is inconsistent"
        )
    if hash_domain("weft/chunk/v1", bytes(target)) != frame.header.content_hash:
        raise Corrupt(
            "WEIGHT_DELTA content_hash mismatch against the reconstructed tensor"
        )
    return bytes(target)


# --- mode 14 ---------------------------------------------------------------


@dataclass
class SemanticDeltaMeta:
    dtype: str
    shape: "list[int]"
    element_count: int
    word_bits: int
    base: bytes
    base_dtype: str
    base_shape: "list[int]"
    base_semantic_root: bytes
    policy: SemanticPolicy
    delta_root: bytes
    plane_codec: str
    payload_len: int


_SEMANTIC_DELTA_KEYS = (
    "schema",
    "dtype",
    "shape",
    "element_count",
    "word_bits",
    "base",
    "base_dtype",
    "base_shape",
    "base_semantic_root",
    "policy",
    "delta_root",
    "plane_codec",
    "payload_len",
)


def parse_semantic_delta_descriptor(descriptor_bytes: bytes, limits: Limits):
    map_value = _map(_decode_descriptor(descriptor_bytes, limits), "SEMANTIC_DELTA")
    _exact_keys(map_value, _SEMANTIC_DELTA_KEYS, "SEMANTIC_DELTA")
    schema = _text(map_value, "schema")
    if schema != SEMANTIC_DELTA_SCHEMA:
        raise Malformed("unsupported SEMANTIC_DELTA schema `%s`" % schema)
    dtype = _weight_dtype(_text(map_value, "dtype"))
    shape = _shape(map_value)
    element_count = _u64(map_value, "element_count")
    word_bits = _u64(map_value, "word_bits")
    base = _hash_field(map_value, "base")
    base_dtype = _weight_dtype(_text(map_value, "base_dtype"))
    base_shape = _array_u64(map_value, "base_shape")
    base_semantic_root = _hash_field(map_value, "base_semantic_root")
    policy = parse_semantic_policy(map_value.get("policy"), dtype, element_count)
    delta_root = _hash_field(map_value, "delta_root")
    plane_codec = _text(map_value, "plane_codec")
    payload_len = _u64(map_value, "payload_len")
    meta = SemanticDeltaMeta(
        dtype=dtype,
        shape=shape,
        element_count=element_count,
        word_bits=word_bits,
        base=base,
        base_dtype=base_dtype,
        base_shape=base_shape,
        base_semantic_root=base_semantic_root,
        policy=policy,
        delta_root=delta_root,
        plane_codec=plane_codec,
        payload_len=payload_len,
    )
    validate_semantic_delta_meta(meta, limits)
    return meta


def validate_semantic_delta_meta(meta: SemanticDeltaMeta, limits: Limits) -> int:
    if meta.plane_codec != PLANE_CODEC:
        raise Malformed(
            "SEMANTIC_DELTA plane_codec `%s` is not implemented (only %s)"
            % (meta.plane_codec, PLANE_CODEC)
        )
    if WEIGHT_WORD_BITS[meta.dtype] != meta.word_bits:
        raise Malformed(
            "SEMANTIC_DELTA word_bits %d disagrees with dtype %s (%d bits)"
            % (meta.word_bits, meta.dtype, WEIGHT_WORD_BITS[meta.dtype])
        )
    if meta.base_dtype != meta.dtype:
        raise Malformed(
            "SEMANTIC_DELTA base_dtype `%s` must equal dtype `%s`"
            % (meta.base_dtype, meta.dtype)
        )
    if meta.base_shape != meta.shape:
        raise Malformed("SEMANTIC_DELTA base_shape must equal shape")
    count = _validate_shape(meta.shape, limits, "SEMANTIC_DELTA")
    if meta.element_count != count:
        raise Malformed(
            "SEMANTIC_DELTA element_count %d does not match shape product %d"
            % (meta.element_count, count)
        )
    _checked_mul(
        count, WEIGHT_STORAGE_BYTES[meta.dtype],
        "SEMANTIC_DELTA element byte length",
    )
    meta.policy.validate(meta.dtype, count)
    return count


def semantic_delta_metadata(frame, limits: Limits) -> SemanticDeltaMeta:
    descriptor_bytes, _payload = _split_prefix(frame.body)
    return parse_semantic_delta_descriptor(descriptor_bytes, limits)


def decode_semantic_delta(frame, resolver, limits: Limits, base_frame=None) -> bytes:
    descriptor_bytes, payload = _split_prefix(frame.body)
    meta = parse_semantic_delta_descriptor(descriptor_bytes, limits)
    count = validate_semantic_delta_meta(meta, limits)
    expected_len = _checked_mul(
        count, WEIGHT_STORAGE_BYTES[meta.dtype],
        "SEMANTIC_DELTA element byte length",
    )
    if frame.header.decoded_len != expected_len:
        raise Corrupt(
            "SEMANTIC_DELTA decoded_len is %d but the descriptor requires %d"
            % (frame.header.decoded_len, expected_len)
        )
    _check_len(expected_len, limits.max_decoded_bytes, "SEMANTIC_DELTA element bytes")
    if meta.payload_len != len(payload):
        raise Malformed(
            "SEMANTIC_DELTA payload is %d bytes but payload_len declares %d"
            % (len(payload), meta.payload_len)
        )
    if frame.header.dep_count != 1:
        raise Malformed("SEMANTIC_DELTA requires exactly one dependency")
    dep = frame.dependencies[0]
    if dep.hash != meta.base:
        raise Malformed(
            "SEMANTIC_DELTA dependency hash must equal the descriptor base"
        )
    if dep.decoded_len != frame.header.decoded_len:
        raise Malformed(
            "SEMANTIC_DELTA dependency decoded_len must equal the frame decoded_len"
        )
    planes = _decode_planes(meta.dtype, payload, count, "SEMANTIC_DELTA")
    delta_bytes = _build_delta_bytes(meta.dtype, planes, count)
    if values_hash(delta_bytes) != meta.delta_root:
        raise Corrupt("SEMANTIC_DELTA delta_root mismatch")

    base_bytes, supplied = _base_bytes(
        frame, meta.base, resolver, limits, base_frame, "SEMANTIC_DELTA"
    )
    if supplied is not None:
        _verify_base_frame(supplied, meta, limits, "SEMANTIC_DELTA")
    if len(base_bytes) != expected_len:
        raise DependencyMismatch(
            "base frame decodes to %d bytes but the descriptor requires %d"
            % (len(base_bytes), expected_len)
        )
    _check_quantized(meta.dtype, base_bytes, "SEMANTIC_DELTA")

    try:
        recomputed = semantic_root(
            meta.dtype, meta.shape, base_bytes, meta.policy, limits
        )
    except (Malformed, NonCanonical, LimitExceeded, Corrupt) as exc:
        raise DependencyMismatch(exc.message)
    if recomputed != meta.base_semantic_root:
        raise DependencyMismatch(
            "base semantic root does not equal the descriptor (no approximate "
            "reconstruction exists)"
        )

    mask = _mod_mask(meta.dtype)
    target = bytearray()
    for index in range(count):
        delta = _delta_word_from_planes(meta.dtype, planes, index)
        base_word = _word_at(meta.dtype, base_bytes, index)
        _push_word(meta.dtype, (delta + base_word) & mask, target)
    if len(target) != expected_len:
        raise Corrupt(
            "SEMANTIC_DELTA reconstructed element length is inconsistent"
        )
    if hash_domain("weft/chunk/v1", bytes(target)) != frame.header.content_hash:
        raise Corrupt(
            "SEMANTIC_DELTA content_hash mismatch against the reconstructed tensor"
        )
    return bytes(target)


# --- mode 16 ---------------------------------------------------------------

_SPARSE_KEYS = (
    "schema",
    "dtype",
    "shape",
    "element_count",
    "storage_bytes",
    "op",
    "sparse_codec",
    "base_content_root",
    "delta_root",
)


@dataclass
class SparseDeltaMeta:
    dtype: str
    shape: "list[int]"
    element_count: int
    storage_bytes: int
    op: str
    sparse_codec: str
    base: bytes
    delta_root: bytes


def parse_sparse_descriptor(descriptor_bytes: bytes, limits: Limits) -> SparseDeltaMeta:
    map_value = _map(_decode_descriptor(descriptor_bytes, limits), "SPARSE_DELTA")
    _exact_keys(map_value, _SPARSE_KEYS, "SPARSE_DELTA")
    schema = _text(map_value, "schema")
    if schema != SPARSE_DELTA_SCHEMA:
        raise Malformed("unsupported SPARSE_DELTA schema `%s`" % schema)
    dtype = _weight_dtype(_text(map_value, "dtype"))
    shape = _shape(map_value)
    element_count = _u64(map_value, "element_count")
    storage_bytes = _u64(map_value, "storage_bytes")
    op = _text(map_value, "op")
    sparse_codec = _text(map_value, "sparse_codec")
    base = _hash_field(map_value, "base_content_root")
    delta_root = _hash_field(map_value, "delta_root")
    meta = SparseDeltaMeta(
        dtype=dtype,
        shape=shape,
        element_count=element_count,
        storage_bytes=storage_bytes,
        op=op,
        sparse_codec=sparse_codec,
        base=base,
        delta_root=delta_root,
    )
    validate_sparse_meta(meta, limits)
    return meta


def validate_sparse_meta(meta: SparseDeltaMeta, limits: Limits) -> int:
    if meta.op != DELTA_OP:
        raise Malformed(
            "SPARSE_DELTA op `%s` is not implemented (only %s)"
            % (meta.op, DELTA_OP)
        )
    if meta.sparse_codec != SPARSE_CODEC:
        raise Malformed(
            "SPARSE_DELTA sparse_codec `%s` is not implemented (only %s)"
            % (meta.sparse_codec, SPARSE_CODEC)
        )
    if meta.storage_bytes != WEIGHT_STORAGE_BYTES[meta.dtype]:
        raise Malformed(
            "SPARSE_DELTA storage_bytes %d disagrees with dtype %s (%d bytes)"
            % (
                meta.storage_bytes,
                meta.dtype,
                WEIGHT_STORAGE_BYTES[meta.dtype],
            )
        )
    count = _validate_shape(meta.shape, limits, "SPARSE_DELTA")
    if meta.element_count != count:
        raise Malformed(
            "SPARSE_DELTA element_count %d does not match shape product %d"
            % (meta.element_count, count)
        )
    _checked_mul(
        count, WEIGHT_STORAGE_BYTES[meta.dtype],
        "SPARSE_DELTA element byte length",
    )
    return count


def sparse_delta_metadata(frame, limits: Limits) -> SparseDeltaMeta:
    descriptor_bytes, _payload = _split_prefix(frame.body)
    return parse_sparse_descriptor(descriptor_bytes, limits)


def _decode_runs(
    dtype: str, payload: bytes, element_count: int, limits: Limits
) -> bytes:
    if len(payload) < 4:
        raise Malformed("SPARSE_DELTA body is shorter than its 4-byte run_count")
    run_count = int.from_bytes(payload[0:4], "little")
    if run_count > limits.max_sparse_runs:
        raise LimitExceeded(
            "SPARSE_DELTA run_count %d exceeds limit %d"
            % (run_count, limits.max_sparse_runs)
        )
    if run_count > element_count:
        raise Malformed(
            "SPARSE_DELTA run_count %d exceeds element_count %d"
            % (run_count, element_count)
        )
    storage = WEIGHT_STORAGE_BYTES[dtype]
    delta_len = _checked_mul(element_count, storage, "SPARSE_DELTA delta bytes")
    delta = bytearray(delta_len)
    position = 4
    emitted = 0
    for record in range(run_count):
        zero_run, position = varint.decode(payload, position)
        emitted = _checked_add(
            emitted, zero_run, "SPARSE_DELTA zero-run length"
        )
        if emitted >= element_count:
            raise Malformed(
                "SPARSE_DELTA run %d skips %d words past element_count %d"
                % (record, zero_run, element_count)
            )
        end = position + storage
        if end > len(payload):
            raise Malformed("SPARSE_DELTA run %d record is truncated" % record)
        word = payload[position:end]
        if not any(word):
            raise Corrupt(
                "SPARSE_DELTA run %d stores a zero word (non-canonical: zeros "
                "are implicit)" % record
            )
        if dtype == "q4_0" and word[0] > 15:
            raise Corrupt(
                "SPARSE_DELTA q4_0 run %d word is %d, outside 0..=15"
                % (record, word[0])
            )
        offset = emitted * storage
        delta[offset : offset + storage] = word
        position = end
        emitted += 1
    if position != len(payload):
        raise Corrupt(
            "SPARSE_DELTA payload has %d trailing bytes after the %d declared runs"
            % (len(payload) - position, run_count)
        )
    return bytes(delta)


def decode_sparse_delta(frame, resolver, limits: Limits, base_frame=None) -> bytes:
    descriptor_bytes, payload = _split_prefix(frame.body)
    meta = parse_sparse_descriptor(descriptor_bytes, limits)
    count = validate_sparse_meta(meta, limits)
    expected_len = _checked_mul(
        count, WEIGHT_STORAGE_BYTES[meta.dtype],
        "SPARSE_DELTA element byte length",
    )
    if frame.header.decoded_len != expected_len:
        raise Corrupt(
            "SPARSE_DELTA decoded_len is %d but the descriptor requires %d"
            % (frame.header.decoded_len, expected_len)
        )
    _check_len(expected_len, limits.max_decoded_bytes, "SPARSE_DELTA element bytes")
    if frame.header.dep_count != 1:
        raise Malformed("SPARSE_DELTA requires exactly one dependency")
    dep = frame.dependencies[0]
    if dep.hash != meta.base:
        raise Malformed(
            "SPARSE_DELTA dependency hash must equal the descriptor "
            "base_content_root"
        )
    if dep.decoded_len != frame.header.decoded_len:
        raise Malformed(
            "SPARSE_DELTA dependency decoded_len must equal the frame decoded_len"
        )
    delta_bytes = _decode_runs(meta.dtype, payload, count, limits)
    if values_hash(delta_bytes) != meta.delta_root:
        raise Corrupt("SPARSE_DELTA delta_root mismatch")

    base_bytes, supplied = _base_bytes(
        frame, meta.base, resolver, limits, base_frame, "SPARSE_DELTA"
    )
    if supplied is not None:
        _verify_base_frame(supplied, meta, limits, "SPARSE_DELTA")
    if len(base_bytes) != expected_len:
        raise DependencyMismatch(
            "base frame decodes to %d bytes but the descriptor requires %d"
            % (len(base_bytes), expected_len)
        )
    _check_quantized(meta.dtype, base_bytes, "SPARSE_DELTA")

    mask = _mod_mask(meta.dtype)
    target = bytearray()
    for index in range(count):
        delta = _word_at(meta.dtype, delta_bytes, index)
        base_word = _word_at(meta.dtype, base_bytes, index)
        _push_word(meta.dtype, (delta + base_word) & mask, target)
    if len(target) != expected_len:
        raise Corrupt(
            "SPARSE_DELTA reconstructed element length is inconsistent"
        )
    if hash_domain("weft/chunk/v1", bytes(target)) != frame.header.content_hash:
        raise Corrupt(
            "SPARSE_DELTA content_hash mismatch against the reconstructed tensor"
        )
    return bytes(target)


def activation_stream_root(
    stream: str,
    step: int,
    rank: int,
    world: int,
    chunk_count: int,
    digest_roots: "list[bytes]",
    limits: Limits,
) -> bytes:
    if len(digest_roots) != chunk_count:
        raise Malformed(
            "ACTIVATION_STREAM stream root needs %d digests, got %d"
            % (chunk_count, len(digest_roots))
        )
    record = {
        "stream": stream,
        "step": wcb.U64(step),
        "rank": wcb.U64(rank),
        "world": wcb.U64(world),
        "chunk_count": wcb.U64(chunk_count),
        "digest_roots": [bytes(digest) for digest in digest_roots],
    }
    return hash_domain(DOMAIN_AI_STREAM, wcb.encode(record, limits))


# ---------------------------------------------------------------------------
# Mode 12 -- KV_CACHE
# ---------------------------------------------------------------------------

_KV_KEYS = (
    "schema",
    "model",
    "layer",
    "session",
    "page_index",
    "page_count",
    "seq_start",
    "seq_end",
    "dtype",
    "head_count",
    "head_dim",
    "shape",
    "page_bytes",
    "plane_codec",
    "payload_len",
)


@dataclass
class KvMeta:
    model: str
    layer: int
    session: str
    page_index: int
    page_count: int
    seq_start: int
    seq_end: int
    dtype: str
    head_count: int
    head_dim: int
    shape: "list[int]"
    page_bytes: int
    plane_codec: str
    payload_len: int


def _kv_dtype(name: str) -> str:
    if name not in KV_WORD_BYTES:
        raise Malformed(
            "unknown KV_CACHE dtype `%s` (v1 allows bf16, fp16, fp32)" % name
        )
    return name


def parse_kv_descriptor(descriptor_bytes: bytes, limits: Limits) -> KvMeta:
    map_value = _map(_decode_descriptor(descriptor_bytes, limits), "KV_CACHE")
    _exact_keys(map_value, _KV_KEYS, "KV_CACHE")
    schema = _text(map_value, "schema")
    if schema != KV_CACHE_SCHEMA:
        raise Malformed("unsupported KV_CACHE schema `%s`" % schema)
    meta = KvMeta(
        model=_text(map_value, "model"),
        layer=_u32(map_value, "layer"),
        session=_text(map_value, "session"),
        page_index=_u64(map_value, "page_index"),
        page_count=_u64(map_value, "page_count"),
        seq_start=_u64(map_value, "seq_start"),
        seq_end=_u64(map_value, "seq_end"),
        dtype=_kv_dtype(_text(map_value, "dtype")),
        head_count=_u32(map_value, "head_count"),
        head_dim=_u32(map_value, "head_dim"),
        shape=_shape(map_value),
        page_bytes=_u64(map_value, "page_bytes"),
        plane_codec=_text(map_value, "plane_codec"),
        payload_len=_u64(map_value, "payload_len"),
    )
    validate_kv_meta(meta, limits)
    return meta


def validate_kv_meta(meta: KvMeta, limits: Limits) -> int:
    if meta.plane_codec != PLANE_CODEC:
        raise Malformed(
            "KV_CACHE plane_codec `%s` is not implemented (only %s)"
            % (meta.plane_codec, PLANE_CODEC)
        )
    if meta.page_count == 0:
        raise Malformed("KV_CACHE page_count must be at least 1")
    if meta.page_index >= meta.page_count:
        raise Malformed(
            "KV_CACHE page_index %d is not below page_count %d"
            % (meta.page_index, meta.page_count)
        )
    if meta.seq_start >= meta.seq_end:
        raise Malformed(
            "KV_CACHE seq_start %d must be less than seq_end %d"
            % (meta.seq_start, meta.seq_end)
        )
    rows = meta.seq_end - meta.seq_start
    if meta.head_count == 0:
        raise Malformed("KV_CACHE head_count must be greater than zero")
    if meta.head_dim == 0:
        raise Malformed("KV_CACHE head_dim must be greater than zero")
    expected_shape = [rows, meta.head_count, meta.head_dim]
    if meta.shape != expected_shape:
        raise Malformed(
            "KV_CACHE shape %r must equal [page_rows, head_count, head_dim] = %r"
            % (meta.shape, expected_shape)
        )
    if len(meta.shape) > limits.max_tensor_rank:
        raise LimitExceeded(
            "KV_CACHE rank %d exceeds limit %d"
            % (len(meta.shape), limits.max_tensor_rank)
        )
    element_count = _checked_mul(rows, meta.head_count, "KV_CACHE element count")
    element_count = _checked_mul(
        element_count, meta.head_dim, "KV_CACHE element count"
    )
    if element_count > limits.max_tensor_elements:
        raise LimitExceeded(
            "KV_CACHE element count %d exceeds limit %d"
            % (element_count, limits.max_tensor_elements)
        )
    computed = _checked_mul(
        element_count, KV_WORD_BYTES[meta.dtype], "KV_CACHE page byte count"
    )
    if meta.page_bytes != computed:
        raise Malformed(
            "KV_CACHE page_bytes %d does not equal page_rows * head_count * "
            "head_dim * word_bytes = %d" % (meta.page_bytes, computed)
        )
    _check_len(meta.page_bytes, limits.max_decoded_bytes, "KV_CACHE page")
    return computed


def _decode_byte_planes(
    payload: bytes, word_bytes: int, byte_len: int, what: str
) -> bytes:
    if byte_len % word_bytes != 0:
        raise Malformed("%s byte length is not a whole number of elements" % what)
    element_count = byte_len // word_bytes
    plane_count = word_bytes
    planes = []
    offset = 0
    for _ in range(plane_count):
        if offset >= len(payload):
            raise Corrupt("%s payload ends before all declared planes" % what)
        plane, consumed = huffman.decode_prefix(payload[offset:], element_count)
        offset += consumed
        planes.append(plane)
    if offset != len(payload):
        raise Corrupt(
            "%s payload has trailing bytes after the declared planes" % what
        )
    out = bytearray()
    for index in range(element_count):
        word = 0
        for plane_index, plane in enumerate(planes):
            word |= plane[index] << (8 * plane_index)
        out.extend(word.to_bytes(word_bytes, "little"))
    return bytes(out)


def decode_kv_cache(frame, limits: Limits) -> bytes:
    descriptor_bytes, payload = _split_prefix(frame.body)
    meta = parse_kv_descriptor(descriptor_bytes, limits)
    page_bytes = validate_kv_meta(meta, limits)
    if frame.header.decoded_len != page_bytes:
        raise Corrupt(
            "KV_CACHE decoded_len is %d but the descriptor requires %d"
            % (frame.header.decoded_len, page_bytes)
        )
    if meta.payload_len != len(payload):
        raise Malformed(
            "KV_CACHE payload is %d bytes but payload_len declares %d"
            % (len(payload), meta.payload_len)
        )
    out = _decode_byte_planes(
        payload, KV_WORD_BYTES[meta.dtype], page_bytes, "KV_CACHE"
    )
    if len(out) != page_bytes:
        raise Corrupt("KV_CACHE reconstructed page length is inconsistent")
    if hash_domain("weft/chunk/v1", out) != frame.header.content_hash:
        raise Corrupt(
            "KV_CACHE content_hash mismatch against the reconstructed page"
        )
    return out


# ---------------------------------------------------------------------------
# Mode 13 -- ACTIVATION_STREAM
# ---------------------------------------------------------------------------

_ACTIVATION_KEYS = (
    "schema",
    "stream",
    "step",
    "rank",
    "world",
    "chunk_index",
    "chunk_count",
    "dtype",
    "shape",
    "element_count",
    "digest_root",
    "plane_codec",
    "payload_len",
)


@dataclass
class ActivationMeta:
    stream: str
    step: int
    rank: int
    world: int
    chunk_index: int
    chunk_count: int
    dtype: str
    shape: "list[int]"
    element_count: int
    digest_root: bytes
    plane_codec: str
    payload_len: int


def _stream_dtype(name: str) -> str:
    if name not in KV_WORD_BYTES:
        raise Malformed(
            "unknown ACTIVATION_STREAM dtype `%s` (v1 allows bf16, fp16, fp32)"
            % name
        )
    return name


def _shape_product(shape: "list[int]", limits: Limits) -> int:
    if not shape:
        raise Malformed("ACTIVATION_STREAM shape rank must be at least 1")
    return _validate_shape(shape, limits, "ACTIVATION_STREAM")


def parse_activation_descriptor(
    descriptor_bytes: bytes, limits: Limits
) -> ActivationMeta:
    map_value = _map(
        _decode_descriptor(descriptor_bytes, limits), "ACTIVATION_STREAM"
    )
    _exact_keys(map_value, _ACTIVATION_KEYS, "ACTIVATION_STREAM")
    schema = _text(map_value, "schema")
    if schema != ACTIVATION_STREAM_SCHEMA:
        raise Malformed("unsupported ACTIVATION_STREAM schema `%s`" % schema)
    stream = _text(map_value, "stream")
    if stream not in ("activation", "gradient"):
        raise Malformed(
            "unknown ACTIVATION_STREAM stream `%s` (v1 allows activation, "
            "gradient)" % stream
        )
    meta = ActivationMeta(
        stream=stream,
        step=_u64(map_value, "step"),
        rank=_u32(map_value, "rank"),
        world=_u32(map_value, "world"),
        chunk_index=_u64(map_value, "chunk_index"),
        chunk_count=_u64(map_value, "chunk_count"),
        dtype=_stream_dtype(_text(map_value, "dtype")),
        shape=_shape(map_value),
        element_count=_u64(map_value, "element_count"),
        digest_root=_hash_field(map_value, "digest_root"),
        plane_codec=_text(map_value, "plane_codec"),
        payload_len=_u64(map_value, "payload_len"),
    )
    validate_activation_meta(meta, limits)
    return meta


def validate_activation_meta(meta: ActivationMeta, limits: Limits) -> int:
    if meta.plane_codec != PLANE_CODEC:
        raise Malformed(
            "ACTIVATION_STREAM plane_codec `%s` is not implemented (only %s)"
            % (meta.plane_codec, PLANE_CODEC)
        )
    if meta.world == 0:
        raise Malformed("ACTIVATION_STREAM world must be at least 1")
    if meta.rank >= meta.world:
        raise Malformed(
            "ACTIVATION_STREAM rank %d must be below world %d"
            % (meta.rank, meta.world)
        )
    if meta.chunk_count == 0:
        raise Malformed("ACTIVATION_STREAM chunk_count must be at least 1")
    product = _shape_product(meta.shape, limits)
    byte_len = _checked_mul(
        product, KV_WORD_BYTES[meta.dtype], "ACTIVATION_STREAM chunk byte count"
    )
    _check_len(byte_len, limits.max_decoded_bytes, "ACTIVATION_STREAM chunk")
    if meta.chunk_index >= meta.chunk_count:
        raise Malformed(
            "ACTIVATION_STREAM chunk_index %d is not below chunk_count %d"
            % (meta.chunk_index, meta.chunk_count)
        )
    if meta.element_count != product:
        raise Malformed(
            "ACTIVATION_STREAM element_count %d does not match shape product %d"
            % (meta.element_count, product)
        )
    return byte_len


def activation_metadata(frame, limits: Limits) -> ActivationMeta:
    descriptor_bytes, _payload = _split_prefix(frame.body)
    return parse_activation_descriptor(descriptor_bytes, limits)


def decode_activation_stream(frame, limits: Limits) -> bytes:
    descriptor_bytes, payload = _split_prefix(frame.body)
    meta = parse_activation_descriptor(descriptor_bytes, limits)
    byte_len = validate_activation_meta(meta, limits)
    if frame.header.decoded_len != byte_len:
        raise Corrupt(
            "ACTIVATION_STREAM decoded_len is %d but the descriptor requires %d"
            % (frame.header.decoded_len, byte_len)
        )
    if meta.payload_len != len(payload):
        raise Malformed(
            "ACTIVATION_STREAM payload is %d bytes but payload_len declares %d"
            % (len(payload), meta.payload_len)
        )
    out = _decode_byte_planes(
        payload, KV_WORD_BYTES[meta.dtype], byte_len, "ACTIVATION_STREAM"
    )
    if len(out) != byte_len:
        raise Corrupt(
            "ACTIVATION_STREAM reconstructed chunk length is inconsistent"
        )
    if values_hash(out) != meta.digest_root:
        raise Corrupt("ACTIVATION_STREAM digest_root mismatch")
    if hash_domain("weft/chunk/v1", out) != frame.header.content_hash:
        raise Corrupt(
            "ACTIVATION_STREAM content_hash mismatch against the reconstructed chunk"
        )
    return out


# ---------------------------------------------------------------------------
# Mode 15 -- TENSOR_TILED
# ---------------------------------------------------------------------------

_TILED_KEYS = (
    "schema",
    "dtype",
    "shape",
    "element_count",
    "layout",
    "tile_shape",
    "tile_counts",
    "tile_codec",
    "tiles",
    "payload_len",
    "logical_root",
)

_TILE_KEYS = (
    "index",
    "dims",
    "offset",
    "compressed_len",
    "element_len",
    "tile_root",
)


@dataclass
class TileEntry:
    index: int
    dims: "list[int]"
    offset: int
    compressed_len: int
    element_len: int
    tile_root: bytes


@dataclass
class TiledMeta:
    dtype: str
    shape: "list[int]"
    element_count: int
    tile_shape: "list[int]"
    tile_counts: "list[int]"
    tiles: "list[TileEntry]"
    payload_len: int
    logical_root: bytes


def _tensor_dtype(name: str) -> str:
    if name not in TENSOR_DTYPE_SIZES:
        raise Malformed(
            "unknown tensor dtype `%s` (v1 allows f16, bf16, f32, f64, i8, u8, "
            "i16, i32, i64)" % name
        )
    return name


def _tile_counts(shape: "list[int]", tile_shape: "list[int]") -> "list[int]":
    if len(shape) != len(tile_shape):
        raise Malformed(
            "tensor tile rank %d does not equal shape rank %d"
            % (len(tile_shape), len(shape))
        )
    counts = []
    for axis, (extent, tile) in enumerate(zip(shape, tile_shape)):
        if tile == 0:
            raise Malformed(
                "tensor tile shape axis %d must be greater than zero" % axis
            )
        if extent % tile != 0:
            raise Malformed(
                "tensor tile shape axis %d extent %d does not divide shape "
                "extent %d" % (axis, tile, extent)
            )
        counts.append(extent // tile)
    return counts


def parse_tiled_descriptor(
    descriptor_bytes: bytes, decoded_len: int, payload_len: int, limits: Limits
) -> TiledMeta:
    map_value = _map(_decode_descriptor(descriptor_bytes, limits), "tiled tensor")
    _exact_keys(map_value, _TILED_KEYS, "tiled tensor")
    schema = _text(map_value, "schema")
    if schema != TENSOR_TILED_SCHEMA:
        raise Malformed("unsupported tiled tensor schema `%s`" % schema)
    dtype = _tensor_dtype(_text(map_value, "dtype"))
    layout = _text(map_value, "layout")
    if layout != TILE_LAYOUT:
        raise Malformed(
            "tiled tensor layout `%s` is specified but not implemented in v1 "
            "(only %s)" % (layout, TILE_LAYOUT)
        )
    tile_codec = _text(map_value, "tile_codec")
    if tile_codec != PLANE_CODEC:
        raise Malformed(
            "unsupported tensor tile codec `%s` (v1 allows `%s`)"
            % (tile_codec, PLANE_CODEC)
        )
    shape = _shape(map_value)
    element_count = _validate_shape(shape, limits, "tiled tensor")
    declared_count = _u64(map_value, "element_count")
    if declared_count != element_count:
        raise Malformed(
            "tiled tensor element_count %d does not match shape product %d"
            % (declared_count, element_count)
        )
    element_size = TENSOR_DTYPE_SIZES[dtype]
    decoded_bytes = _checked_mul(
        element_count, element_size, "tiled tensor decoded bytes"
    )
    _check_len(decoded_bytes, limits.max_decoded_bytes, "tiled tensor decoded bytes")
    if decoded_len != decoded_bytes:
        raise Corrupt(
            "tiled tensor decoded_len is %d but the descriptor requires %d"
            % (decoded_len, decoded_bytes)
        )
    tile_shape = _array_u64(map_value, "tile_shape")
    counts = _tile_counts(shape, tile_shape)
    declared_counts = _array_u64(map_value, "tile_counts")
    if declared_counts != counts:
        raise Malformed(
            "tiled tensor tile_counts %r do not match shape/tile_shape %r"
            % (declared_counts, counts)
        )
    tile_elements = _validate_shape(tile_shape, limits, "tensor tile")
    tile_element_len = _checked_mul(
        tile_elements, element_size, "tensor tile byte length"
    )
    tile_count = 1
    for count in counts:
        tile_count = _checked_mul(tile_count, count, "tensor tile count")
    if tile_count > limits.max_structured_items:
        raise LimitExceeded(
            "tensor tile count %d exceeds limit %d"
            % (tile_count, limits.max_structured_items)
        )
    declared_payload_len = _u64(map_value, "payload_len")
    if declared_payload_len != payload_len:
        raise Malformed(
            "tiled tensor payload is %d bytes but metadata declares %d"
            % (payload_len, declared_payload_len)
        )
    entries = map_value.get("tiles")
    if not isinstance(entries, list):
        raise Malformed("tiled tensor `tiles` must be an array")
    if len(entries) != tile_count:
        raise Malformed(
            "tiled tensor has %d tile entries but the grid requires %d"
            % (len(entries), tile_count)
        )
    tiles = []
    expected_offset = 0
    for position, item in enumerate(entries):
        entry = _map(item, "tensor tile")
        _exact_keys(entry, _TILE_KEYS, "tensor tile")
        index = _u64(entry, "index")
        if index != position:
            raise Malformed(
                "tiled tensor entry %d declares index %d; entries must be in "
                "canonical order" % (position, index)
            )
        dims = _array_u64(entry, "dims")
        if dims != tile_shape:
            raise Malformed(
                "tensor tile %d dims %r do not equal tile_shape %r; partial "
                "tiles are not defined in v1" % (position, dims, tile_shape)
            )
        declared_element_len = _u64(entry, "element_len")
        if declared_element_len != tile_element_len:
            raise Malformed(
                "tensor tile %d element_len %d does not match %d"
                % (position, declared_element_len, tile_element_len)
            )
        offset = _u64(entry, "offset")
        if offset != expected_offset:
            raise Malformed(
                "tensor tile %d offset %d is not the contiguous offset %d"
                % (position, offset, expected_offset)
            )
        compressed_len = _u64(entry, "compressed_len")
        if compressed_len == 0:
            raise Malformed(
                "tensor tile %d has an empty stored range" % position
            )
        expected_offset = _checked_add(
            expected_offset, compressed_len, "tensor tile offset"
        )
        root = _bytes_of(entry, "tile_root")
        if len(root) != HASH_LEN:
            raise Malformed("tensor tile_root must be 32 bytes")
        tiles.append(
            TileEntry(
                index=index,
                dims=dims,
                offset=offset,
                compressed_len=compressed_len,
                element_len=declared_element_len,
                tile_root=root,
            )
        )
    if expected_offset != declared_payload_len:
        raise Malformed(
            "tensor tile ranges cover %d bytes but payload_len is %d"
            % (expected_offset, declared_payload_len)
        )
    logical_root = _bytes_of(map_value, "logical_root")
    if len(logical_root) != HASH_LEN:
        raise Malformed("tiled tensor logical_root must be 32 bytes")
    return TiledMeta(
        dtype=dtype,
        shape=shape,
        element_count=element_count,
        tile_shape=tile_shape,
        tile_counts=counts,
        tiles=tiles,
        payload_len=declared_payload_len,
        logical_root=logical_root,
    )


def _tile_start(counts: "list[int]", index: int) -> "list[int]":
    coords = [0] * len(counts)
    remainder = index
    for axis in range(len(counts) - 1, -1, -1):
        coords[axis] = remainder % counts[axis]
        remainder //= counts[axis]
    return coords


def _decode_tile_planes(
    stored: bytes, element_size: int, element_len: int
) -> bytes:
    if element_size == 0 or element_len % element_size != 0:
        raise Malformed(
            "tensor tile element length is not a whole number of elements"
        )
    elements = element_len // element_size
    planes = []
    cursor = 0
    for _ in range(element_size):
        if cursor > len(stored):
            raise Corrupt("tensor tile plane is truncated")
        plane, consumed = huffman.decode_prefix(stored[cursor:], elements)
        cursor += consumed
        planes.append(plane)
    if cursor != len(stored):
        raise Corrupt(
            "tensor tile planes leave trailing bytes inside the tile range"
        )
    out = bytearray(element_len)
    for plane_index, plane in enumerate(planes):
        for element, byte in enumerate(plane):
            out[element * element_size + plane_index] = byte
    return bytes(out)


def _iter_coords(extent: "list[int]"):
    if not extent:
        yield []
        return
    def walk(dimension: int, prefix: "list[int]"):
        if dimension == len(extent):
            yield list(prefix)
            return
        for position in range(extent[dimension]):
            prefix.append(position)
            yield from walk(dimension + 1, prefix)
            prefix.pop()

    yield from walk(0, [])


def _flat_index(coords: "list[int]", shape: "list[int]") -> int:
    index = 0
    for coord, dim in zip(coords, shape):
        index = index * dim + coord
    return index


def decode_tensor_tiled(frame, limits: Limits) -> bytes:
    descriptor_bytes, payload = _split_prefix(frame.body)
    meta = parse_tiled_descriptor(
        descriptor_bytes, frame.header.decoded_len, len(payload), limits
    )
    element_size = TENSOR_DTYPE_SIZES[meta.dtype]
    total = meta.element_count * element_size
    out = bytearray(total)
    tiles_bytes = 0
    for entry in meta.tiles:
        start = entry.offset
        end = start + entry.compressed_len
        if end > len(payload):
            raise Corrupt("tensor tile range is outside the payload")
        stored = payload[start:end]
        if len(stored) != entry.compressed_len:
            raise Corrupt("tensor tile stored range length is inconsistent")
        tile = _decode_tile_planes(stored, element_size, entry.element_len)
        root = tile_root(meta.dtype, entry.dims, tile, limits)
        if root != entry.tile_root:
            raise Corrupt(
                "tensor tile %d root does not match the descriptor" % entry.index
            )
        tiles_bytes += len(tile)
        tile_coords = _tile_start(meta.tile_counts, entry.index)
        dst_start = [
            coord * meta.tile_shape[axis]
            for axis, coord in enumerate(tile_coords)
        ]
        for local in _iter_coords(entry.dims):
            src = _flat_index(local, entry.dims) * element_size
            full = [
                dst_start[axis] + local[axis]
                for axis in range(len(meta.shape))
            ]
            dst = _flat_index(full, meta.shape) * element_size
            out[dst : dst + element_size] = tile[src : src + element_size]
    if tiles_bytes != total:
        raise Corrupt(
            "tiled tensor decoded to %d bytes but the descriptor requires %d"
            % (tiles_bytes, total)
        )
    logical = tensor_logical_root(
        meta.dtype, meta.shape, meta.element_count, bytes(out), limits
    )
    if logical != meta.logical_root:
        raise Corrupt(
            "tiled tensor logical root does not match the descriptor"
        )
    return bytes(out)


# ---------------------------------------------------------------------------
# Mode 17 -- FLOAT_TILED
# ---------------------------------------------------------------------------

FLOAT_TILED_SCHEMA = "weft.ai.float_tiled.v1"
FLOAT_TILED_DTYPES = ("bf16", "fp16", "fp32")
# The descriptor uses the AI-codec spelling; the WEFT-TENSOR identity uses `f16`.
FLOAT_TILED_LOGICAL_DTYPE = {"bf16": "bf16", "fp16": "f16", "fp32": "f32"}

_FLOAT_TILED_KEYS = (
    "schema",
    "dtype",
    "shape",
    "element_count",
    "layout",
    "tile_shape",
    "tile_counts",
    "exponent_coder",
    "tiles",
    "payload_len",
    "logical_root",
)

_FLOAT_TILE_KEYS = (
    "index",
    "dims",
    "offset",
    "compressed_len",
    "exponent_plane_len",
    "mantissa_plane_len",
    "element_len",
    "tile_root",
)


@dataclass
class FloatTileEntry:
    index: int
    dims: "list[int]"
    offset: int
    compressed_len: int
    exponent_plane_len: int
    mantissa_plane_len: int
    element_len: int
    tile_root: bytes


@dataclass
class FloatTiledMeta:
    dtype: str
    shape: "list[int]"
    element_count: int
    tile_shape: "list[int]"
    tile_counts: "list[int]"
    tiles: "list[FloatTileEntry]"
    payload_len: int
    logical_root: bytes


def float_tile_root(
    dtype: str,
    dims: "list[int]",
    element_len: int,
    exponent_plane_len: int,
    mantissa_plane_len: int,
    tile_bytes: bytes,
    limits: Limits,
) -> bytes:
    record = {
        "dtype": dtype,
        "dims": _u64_vec(dims),
        "element_len": wcb.U64(element_len),
        "exponent_plane_len": wcb.U64(exponent_plane_len),
        "mantissa_plane_len": wcb.U64(mantissa_plane_len),
        "values_hash": values_hash(tile_bytes),
    }
    return hash_domain(DOMAIN_TENSOR_FLOAT_TILE, wcb.encode(record, limits))


def parse_float_tiled_descriptor(
    descriptor_bytes: bytes, decoded_len: int, payload_len: int, limits: Limits
) -> FloatTiledMeta:
    map_value = _map(_decode_descriptor(descriptor_bytes, limits), "float tiled tensor")
    _exact_keys(map_value, _FLOAT_TILED_KEYS, "float tiled tensor")
    schema = _text(map_value, "schema")
    if schema != FLOAT_TILED_SCHEMA:
        raise Malformed("unsupported float tiled tensor schema `%s`" % schema)
    dtype = _text(map_value, "dtype")
    if dtype not in FLOAT_FIELD_BITS:
        raise Malformed(
            "unknown float tiled tensor dtype `%s` (v1 allows bf16, fp16, "
            "fp32, fp8-e4m3, fp8-e5m2)" % dtype
        )
    if dtype not in FLOAT_TILED_DTYPES:
        raise UnsupportedFeature(
            "float tiled tensor dtype `%s` is not implemented in v1" % dtype
        )
    layout = _text(map_value, "layout")
    if layout != TILE_LAYOUT:
        raise Malformed(
            "float tiled tensor layout `%s` is specified but not implemented "
            "in v1 (only %s)" % (layout, TILE_LAYOUT)
        )
    coder = _text(map_value, "exponent_coder")
    if coder != PLANE_CODEC:
        raise Malformed(
            "unsupported float tiled tensor exponent coder `%s` (v1 allows `%s`)"
            % (coder, PLANE_CODEC)
        )
    shape = _shape(map_value)
    element_count = _validate_shape(shape, limits, "float tiled tensor")
    declared_count = _u64(map_value, "element_count")
    if declared_count != element_count:
        raise Malformed(
            "float tiled tensor element_count %d does not match shape product %d"
            % (declared_count, element_count)
        )
    sign_bits, exponent_bits, mantissa_bits = FLOAT_FIELD_BITS[dtype]
    element_bits = sign_bits + exponent_bits + mantissa_bits
    if element_bits % 8 != 0:
        raise Malformed("float tiled tensor element width is not byte-aligned")
    element_size = element_bits // 8
    decoded_bytes = _checked_mul(
        element_count, element_size, "float tiled tensor decoded bytes"
    )
    _check_len(decoded_bytes, limits.max_decoded_bytes, "float tiled tensor decoded bytes")
    if decoded_len != decoded_bytes:
        raise Corrupt(
            "float tiled tensor decoded_len is %d but the descriptor requires %d"
            % (decoded_len, decoded_bytes)
        )
    tile_shape = _array_u64(map_value, "tile_shape")
    counts = _tile_counts(shape, tile_shape)
    declared_counts = _array_u64(map_value, "tile_counts")
    if declared_counts != counts:
        raise Malformed(
            "float tiled tensor tile_counts %r do not match shape/tile_shape %r"
            % (declared_counts, counts)
        )
    tile_elements = _validate_shape(tile_shape, limits, "float tensor tile")
    tile_element_len = _checked_mul(
        tile_elements, element_size, "float tensor tile byte length"
    )
    tile_count = 1
    for count in counts:
        tile_count = _checked_mul(tile_count, count, "float tensor tile count")
    if tile_count > limits.max_structured_items:
        raise LimitExceeded(
            "float tensor tile count %d exceeds limit %d"
            % (tile_count, limits.max_structured_items)
        )
    declared_payload_len = _u64(map_value, "payload_len")
    if declared_payload_len != payload_len:
        # The frame body is the authority; a declared length that disagrees is
        # a truncated or over-long payload, which is corruption.
        raise Corrupt(
            "float tiled tensor payload is %d bytes but metadata declares %d"
            % (payload_len, declared_payload_len)
        )
    mantissa_total_bits = _checked_mul(
        tile_elements, sign_bits + mantissa_bits, "float tensor tile mantissa bits"
    )
    expected_tile_mantissa = _checked_add(
        mantissa_total_bits, 7, "float tensor tile mantissa length"
    ) // 8
    entries = map_value.get("tiles")
    if not isinstance(entries, list):
        raise Malformed("float tiled tensor `tiles` must be an array")
    if len(entries) != tile_count:
        raise Malformed(
            "float tiled tensor has %d tile entries but the grid requires %d"
            % (len(entries), tile_count)
        )
    tiles = []
    expected_offset = 0
    for position, item in enumerate(entries):
        entry = _map(item, "float tensor tile")
        _exact_keys(entry, _FLOAT_TILE_KEYS, "float tensor tile")
        index = _u64(entry, "index")
        if index != position:
            raise Malformed(
                "float tiled tensor entry %d declares index %d; entries must be "
                "in canonical order" % (position, index)
            )
        if index >= tile_count:
            raise Malformed(
                "float tiled tensor tile index %d is outside the %d-tile grid"
                % (index, tile_count)
            )
        dims = _array_u64(entry, "dims")
        if dims != tile_shape:
            raise Malformed(
                "float tensor tile %d dims %r do not equal tile_shape %r; "
                "partial tiles are not defined in v1" % (position, dims, tile_shape)
            )
        declared_element_len = _u64(entry, "element_len")
        if declared_element_len != tile_element_len:
            raise Malformed(
                "float tensor tile %d element_len %d does not match %d"
                % (position, declared_element_len, tile_element_len)
            )
        offset = _u64(entry, "offset")
        if offset != expected_offset:
            raise Malformed(
                "float tensor tile %d offset %d is not the contiguous offset %d"
                % (position, offset, expected_offset)
            )
        compressed_len = _u64(entry, "compressed_len")
        if compressed_len == 0:
            raise Malformed("float tensor tile %d has an empty stored range" % position)
        exponent_plane_len = _u64(entry, "exponent_plane_len")
        if exponent_plane_len < 256:
            raise Malformed(
                "float tensor tile %d exponent_plane_len %d is below the "
                "Huffman table minimum" % (position, exponent_plane_len)
            )
        mantissa_plane_len = _u64(entry, "mantissa_plane_len")
        if mantissa_plane_len != expected_tile_mantissa:
            raise Malformed(
                "float tensor tile %d mantissa_plane_len %d does not match the "
                "tile arithmetic %d"
                % (position, mantissa_plane_len, expected_tile_mantissa)
            )
        if exponent_plane_len + mantissa_plane_len != compressed_len:
            raise Malformed(
                "float tensor tile %d plane lengths %d + %d do not sum to "
                "compressed_len %d"
                % (
                    position,
                    exponent_plane_len,
                    mantissa_plane_len,
                    compressed_len,
                )
            )
        expected_offset = _checked_add(
            expected_offset, compressed_len, "float tensor tile offset"
        )
        root = _bytes_of(entry, "tile_root")
        if len(root) != HASH_LEN:
            raise Malformed("float tensor tile_root must be 32 bytes")
        tiles.append(
            FloatTileEntry(
                index=index,
                dims=dims,
                offset=offset,
                compressed_len=compressed_len,
                exponent_plane_len=exponent_plane_len,
                mantissa_plane_len=mantissa_plane_len,
                element_len=declared_element_len,
                tile_root=root,
            )
        )
    if expected_offset != declared_payload_len:
        raise Corrupt(
            "float tensor tile ranges cover %d bytes but payload_len is %d"
            % (expected_offset, declared_payload_len)
        )
    logical_root = _bytes_of(map_value, "logical_root")
    if len(logical_root) != HASH_LEN:
        raise Malformed("float tiled tensor logical_root must be 32 bytes")
    return FloatTiledMeta(
        dtype=dtype,
        shape=shape,
        element_count=element_count,
        tile_shape=tile_shape,
        tile_counts=counts,
        tiles=tiles,
        payload_len=declared_payload_len,
        logical_root=logical_root,
    )


def decode_float_tiled(frame, limits: Limits) -> bytes:
    descriptor_bytes, payload = _split_prefix(frame.body)
    meta = parse_float_tiled_descriptor(
        descriptor_bytes, frame.header.decoded_len, len(payload), limits
    )
    sign_bits, exponent_bits, mantissa_bits = FLOAT_FIELD_BITS[meta.dtype]
    element_size = (sign_bits + exponent_bits + mantissa_bits) // 8
    total = meta.element_count * element_size
    out = bytearray(total)
    tiles_bytes = 0
    for entry in meta.tiles:
        start = entry.offset
        end = start + entry.compressed_len
        if end > len(payload):
            raise Corrupt("float tensor tile range is outside the payload")
        stored = payload[start:end]
        exponent_compressed = stored[: entry.exponent_plane_len]
        mantissa_plane = stored[entry.exponent_plane_len :]
        elements = entry.element_len // element_size
        exponent_plane = huffman.decode(exponent_compressed, elements)
        tile = _join_float_planes(
            exponent_plane,
            mantissa_plane,
            meta.dtype,
            elements,
            limits,
        )
        root = float_tile_root(
            meta.dtype,
            entry.dims,
            entry.element_len,
            entry.exponent_plane_len,
            entry.mantissa_plane_len,
            tile,
            limits,
        )
        if root != entry.tile_root:
            raise Corrupt(
                "float tensor tile %d root does not match the descriptor"
                % entry.index
            )
        tiles_bytes += len(tile)
        tile_coords = _tile_start(meta.tile_counts, entry.index)
        dst_start = [
            coord * meta.tile_shape[axis] for axis, coord in enumerate(tile_coords)
        ]
        for local in _iter_coords(entry.dims):
            src = _flat_index(local, entry.dims) * element_size
            full = [dst_start[axis] + local[axis] for axis in range(len(meta.shape))]
            dst = _flat_index(full, meta.shape) * element_size
            out[dst : dst + element_size] = tile[src : src + element_size]
    if tiles_bytes != total:
        raise Corrupt(
            "float tiled tensor decoded to %d bytes but the descriptor "
            "requires %d" % (tiles_bytes, total)
        )
    logical = tensor_logical_root(
        FLOAT_TILED_LOGICAL_DTYPE[meta.dtype],
        meta.shape,
        meta.element_count,
        bytes(out),
        limits,
    )
    if logical != meta.logical_root:
        raise Corrupt(
            "float tiled tensor logical root does not match the descriptor"
        )
    return bytes(out)


# ---------------------------------------------------------------------------
# Mode 18 -- FLOAT_TILED_STREAMS
# ---------------------------------------------------------------------------

FLOAT_TILED_STREAMS_SCHEMA = "weft.ai.tensor_tiled_streams.v1"
FLOAT_TILED_STREAMS_MIN = 1
FLOAT_TILED_STREAMS_MAX = 16

_FLOAT_TILED_STREAMS_KEYS = (
    "schema",
    "dtype",
    "shape",
    "element_count",
    "layout",
    "tile_shape",
    "tile_counts",
    "exponent_coder",
    "n_streams",
    "tiles",
    "payload_len",
    "logical_root",
)

_FLOAT_TILE_STREAMS_KEYS = (
    "index",
    "dims",
    "offset",
    "compressed_len",
    "exponent_plane_len",
    "mantissa_plane_len",
    "element_len",
    "stream_offsets",
    "tile_root",
)


@dataclass
class FloatTileStreamsEntry:
    index: int
    dims: "list[int]"
    offset: int
    compressed_len: int
    exponent_plane_len: int
    mantissa_plane_len: int
    element_len: int
    stream_offsets: "list[int]"
    tile_root: bytes


@dataclass
class FloatTiledStreamsMeta:
    dtype: str
    shape: "list[int]"
    element_count: int
    tile_shape: "list[int]"
    tile_counts: "list[int]"
    n_streams: int
    tiles: "list[FloatTileStreamsEntry]"
    payload_len: int
    logical_root: bytes


def interleaved_stream_len(total: int, stream: int, n_streams: int) -> int:
    if n_streams <= 0 or total <= stream:
        return 0
    return (total - stream + n_streams - 1) // n_streams


def float_tile_streams_root(
    dtype: str,
    dims: "list[int]",
    element_len: int,
    exponent_plane_len: int,
    mantissa_plane_len: int,
    n_streams: int,
    stream_offsets: "list[int]",
    tile_bytes: bytes,
    limits: Limits,
) -> bytes:
    record = {
        "dtype": dtype,
        "dims": _u64_vec(dims),
        "element_len": wcb.U64(element_len),
        "exponent_plane_len": wcb.U64(exponent_plane_len),
        "mantissa_plane_len": wcb.U64(mantissa_plane_len),
        "n_streams": wcb.U64(n_streams),
        "stream_offsets": _u64_vec(stream_offsets),
        "values_hash": values_hash(tile_bytes),
    }
    return hash_domain(DOMAIN_TENSOR_FLOAT_TILE_STREAMS, wcb.encode(record, limits))


def parse_float_tiled_streams_descriptor(
    descriptor_bytes: bytes, decoded_len: int, payload_len: int, limits: Limits
) -> FloatTiledStreamsMeta:
    map_value = _map(
        _decode_descriptor(descriptor_bytes, limits), "float tiled streams tensor"
    )
    _exact_keys(map_value, _FLOAT_TILED_STREAMS_KEYS, "float tiled streams tensor")
    schema = _text(map_value, "schema")
    if schema != FLOAT_TILED_STREAMS_SCHEMA:
        raise Malformed("unsupported float tiled streams schema `%s`" % schema)
    dtype = _text(map_value, "dtype")
    if dtype not in FLOAT_FIELD_BITS:
        raise Malformed("unknown float tiled streams dtype `%s`" % dtype)
    if dtype not in FLOAT_TILED_DTYPES:
        raise UnsupportedFeature(
            "float tiled streams dtype `%s` is not implemented in v1" % dtype
        )
    layout = _text(map_value, "layout")
    if layout != TILE_LAYOUT:
        raise Malformed(
            "float tiled streams layout `%s` is specified but not implemented "
            "in v1 (only %s)" % (layout, TILE_LAYOUT)
        )
    coder = _text(map_value, "exponent_coder")
    if coder != PLANE_CODEC:
        raise Malformed(
            "unsupported float tiled streams exponent coder `%s` (v1 allows `%s`)"
            % (coder, PLANE_CODEC)
        )
    n_streams = _u64(map_value, "n_streams")
    if not FLOAT_TILED_STREAMS_MIN <= n_streams <= FLOAT_TILED_STREAMS_MAX:
        raise Malformed(
            "n_streams %d is outside %d..=%d"
            % (n_streams, FLOAT_TILED_STREAMS_MIN, FLOAT_TILED_STREAMS_MAX)
        )
    shape = _shape(map_value)
    element_count = _validate_shape(shape, limits, "float tiled streams tensor")
    declared_count = _u64(map_value, "element_count")
    if declared_count != element_count:
        raise Malformed(
            "float tiled streams element_count %d does not match shape product %d"
            % (declared_count, element_count)
        )
    sign_bits, exponent_bits, mantissa_bits = FLOAT_FIELD_BITS[dtype]
    element_size = (sign_bits + exponent_bits + mantissa_bits) // 8
    decoded_bytes = _checked_mul(
        element_count, element_size, "float tiled streams decoded bytes"
    )
    _check_len(
        decoded_bytes, limits.max_decoded_bytes, "float tiled streams decoded bytes"
    )
    if decoded_len != decoded_bytes:
        raise Corrupt(
            "float tiled streams decoded_len is %d but the descriptor requires %d"
            % (decoded_len, decoded_bytes)
        )
    tile_shape = _array_u64(map_value, "tile_shape")
    counts = _tile_counts(shape, tile_shape)
    if _array_u64(map_value, "tile_counts") != counts:
        raise Malformed(
            "float tiled streams tile_counts %r do not match shape/tile_shape %r"
            % (_array_u64(map_value, "tile_counts"), counts)
        )
    tile_elements = _validate_shape(tile_shape, limits, "float tensor tile")
    tile_element_len = _checked_mul(
        tile_elements, element_size, "float tensor tile byte length"
    )
    tile_count = 1
    for count in counts:
        tile_count = _checked_mul(tile_count, count, "float tensor tile count")
    if tile_count > limits.max_structured_items:
        raise LimitExceeded(
            "float tensor tile count %d exceeds limit %d"
            % (tile_count, limits.max_structured_items)
        )
    mantissa_total_bits = _checked_mul(
        tile_elements, sign_bits + mantissa_bits, "float tensor tile mantissa bits"
    )
    expected_tile_mantissa = _checked_add(
        mantissa_total_bits, 7, "float tensor tile mantissa length"
    ) // 8
    declared_payload_len = _u64(map_value, "payload_len")
    if declared_payload_len != payload_len:
        raise Corrupt(
            "float tiled streams payload is %d bytes but metadata declares %d"
            % (payload_len, declared_payload_len)
        )
    entries = map_value.get("tiles")
    if not isinstance(entries, list):
        raise Malformed("float tiled streams `tiles` must be an array")
    if len(entries) != tile_count:
        raise Malformed(
            "float tiled streams has %d tile entries but the grid requires %d"
            % (len(entries), tile_count)
        )
    tiles = []
    expected_offset = 0
    for position, item in enumerate(entries):
        entry = _map(item, "float tensor streams tile")
        _exact_keys(entry, _FLOAT_TILE_STREAMS_KEYS, "float tensor streams tile")
        index = _u64(entry, "index")
        if index != position:
            raise Malformed(
                "float tiled streams entry %d declares index %d; entries must be "
                "in canonical order" % (position, index)
            )
        dims = _array_u64(entry, "dims")
        if dims != tile_shape:
            raise Malformed(
                "float tensor tile %d dims %r do not equal tile_shape %r; "
                "partial tiles are not defined in v1" % (position, dims, tile_shape)
            )
        declared_element_len = _u64(entry, "element_len")
        if declared_element_len != tile_element_len:
            raise Malformed(
                "float tensor tile %d element_len %d does not match %d"
                % (position, declared_element_len, tile_element_len)
            )
        offset = _u64(entry, "offset")
        if offset != expected_offset:
            raise Malformed(
                "float tensor tile %d offset %d is not the contiguous offset %d"
                % (position, offset, expected_offset)
            )
        compressed_len = _u64(entry, "compressed_len")
        exponent_plane_len = _u64(entry, "exponent_plane_len")
        mantissa_plane_len = _u64(entry, "mantissa_plane_len")
        if mantissa_plane_len != expected_tile_mantissa:
            raise Malformed(
                "float tensor tile %d mantissa_plane_len %d does not match the "
                "tile arithmetic %d"
                % (position, mantissa_plane_len, expected_tile_mantissa)
            )
        if exponent_plane_len + mantissa_plane_len != compressed_len:
            raise Malformed(
                "float tensor tile %d plane lengths do not sum to compressed_len"
                % position
            )
        expected_offset = _checked_add(
            expected_offset, compressed_len, "float tensor tile offset"
        )
        stream_offsets = _array_u64(entry, "stream_offsets")
        if len(stream_offsets) != 2 * n_streams:
            raise Malformed(
                "float tile %d has %d stream offsets but n_streams %d requires %d"
                % (position, len(stream_offsets), n_streams, 2 * n_streams)
            )
        if stream_offsets[0] != 0:
            raise Malformed(
                "float tile %d stream_offsets[0] is %d not 0"
                % (position, stream_offsets[0])
            )
        for earlier, later in zip(stream_offsets, stream_offsets[1:]):
            if later < earlier:
                raise Malformed(
                    "float tile %d stream_offsets are not monotonic: %d then %d"
                    % (position, earlier, later)
                )
        if stream_offsets[-1] > compressed_len:
            raise Malformed(
                "float tile %d stream offset %d is past the %d-byte tile payload"
                % (position, stream_offsets[-1], compressed_len)
            )
        if stream_offsets[n_streams] != exponent_plane_len:
            raise Malformed(
                "float tile %d stream boundary %d does not equal exponent_plane_len %d"
                % (position, stream_offsets[n_streams], exponent_plane_len)
            )
        for stream in range(n_streams):
            end = (
                stream_offsets[stream + 1]
                if stream + 1 < n_streams
                else exponent_plane_len
            )
            if end - stream_offsets[stream] < 256:
                raise Malformed(
                    "float tile %d exponent stream %d is shorter than its "
                    "256-byte code-length table" % (position, stream)
                )
        for stream in range(n_streams):
            start = stream_offsets[n_streams + stream] - exponent_plane_len
            end = (
                stream_offsets[n_streams + stream + 1] - exponent_plane_len
                if stream + 1 < n_streams
                else mantissa_plane_len
            )
            expected = interleaved_stream_len(
                mantissa_plane_len, stream, n_streams
            )
            if end - start != expected:
                raise Malformed(
                    "float tile %d mantissa stream %d is %d bytes but the "
                    "interleave requires %d"
                    % (position, stream, end - start, expected)
                )
        root = _bytes_of(entry, "tile_root")
        if len(root) != HASH_LEN:
            raise Malformed("float tensor tile_root must be 32 bytes")
        tiles.append(
            FloatTileStreamsEntry(
                index=index,
                dims=dims,
                offset=offset,
                compressed_len=compressed_len,
                exponent_plane_len=exponent_plane_len,
                mantissa_plane_len=mantissa_plane_len,
                element_len=declared_element_len,
                stream_offsets=stream_offsets,
                tile_root=root,
            )
        )
    if expected_offset != declared_payload_len:
        raise Corrupt(
            "float tensor tile ranges cover %d bytes but payload_len is %d"
            % (expected_offset, declared_payload_len)
        )
    logical_root = _bytes_of(map_value, "logical_root")
    if len(logical_root) != HASH_LEN:
        raise Malformed("float tiled streams logical_root must be 32 bytes")
    return FloatTiledStreamsMeta(
        dtype=dtype,
        shape=shape,
        element_count=element_count,
        tile_shape=tile_shape,
        tile_counts=counts,
        n_streams=n_streams,
        tiles=tiles,
        payload_len=declared_payload_len,
        logical_root=logical_root,
    )


def _deinterleave(streams: "list[bytes]", total: int, what: str) -> bytes:
    n_streams = len(streams)
    if n_streams == 0:
        raise Malformed("%s declares zero streams" % what)
    out = bytearray(total)
    for index in range(total):
        stream = streams[index % n_streams]
        position = index // n_streams
        if position >= len(stream):
            raise Corrupt("%s stream ended before its interleaved plane" % what)
        out[index] = stream[position]
    return bytes(out)


def decode_float_tiled_streams(frame, limits: Limits) -> bytes:
    descriptor_bytes, payload = _split_prefix(frame.body)
    meta = parse_float_tiled_streams_descriptor(
        descriptor_bytes, frame.header.decoded_len, len(payload), limits
    )
    sign_bits, exponent_bits, mantissa_bits = FLOAT_FIELD_BITS[meta.dtype]
    element_size = (sign_bits + exponent_bits + mantissa_bits) // 8
    total = meta.element_count * element_size
    out = bytearray(total)
    tiles_bytes = 0
    n_streams = meta.n_streams
    for entry in meta.tiles:
        start = entry.offset
        end = start + entry.compressed_len
        if end > len(payload):
            raise Corrupt("float tensor tile range is outside the payload")
        stored = payload[start:end]
        if entry.exponent_plane_len > len(stored):
            raise Corrupt("float tile exponent plane is truncated")
        exponent_stored = stored[: entry.exponent_plane_len]
        mantissa_stored = stored[entry.exponent_plane_len :]
        elements = entry.element_len // element_size
        exponent_parts = []
        for stream in range(n_streams):
            s_start = entry.stream_offsets[stream]
            s_end = (
                entry.stream_offsets[stream + 1]
                if stream + 1 < n_streams
                else entry.exponent_plane_len
            )
            if s_end > len(exponent_stored) or s_start > s_end:
                raise Corrupt("float tile exponent stream is outside the tile")
            expected = interleaved_stream_len(elements, stream, n_streams)
            exponent_parts.append(
                huffman.decode(exponent_stored[s_start:s_end], expected)
            )
        exponent_plane = _deinterleave(exponent_parts, elements, "exponent plane")
        mantissa_parts = []
        for stream in range(n_streams):
            s_start = entry.stream_offsets[n_streams + stream] - entry.exponent_plane_len
            s_end = (
                entry.stream_offsets[n_streams + stream + 1] - entry.exponent_plane_len
                if stream + 1 < n_streams
                else entry.mantissa_plane_len
            )
            if s_end > len(mantissa_stored) or s_start > s_end:
                raise Corrupt("float tile mantissa stream is outside the tile")
            expected = interleaved_stream_len(
                entry.mantissa_plane_len, stream, n_streams
            )
            if s_end - s_start != expected:
                raise Corrupt(
                    "float tile %d mantissa stream %d length differs from the "
                    "interleave" % (entry.index, stream)
                )
            mantissa_parts.append(mantissa_stored[s_start:s_end])
        mantissa_plane = _deinterleave(
            mantissa_parts, entry.mantissa_plane_len, "mantissa plane"
        )
        tile = _join_float_planes(
            exponent_plane, mantissa_plane, meta.dtype, elements, limits
        )
        root = float_tile_streams_root(
            meta.dtype,
            entry.dims,
            entry.element_len,
            entry.exponent_plane_len,
            entry.mantissa_plane_len,
            n_streams,
            entry.stream_offsets,
            tile,
            limits,
        )
        if root != entry.tile_root:
            raise Corrupt(
                "float tensor tile %d stream root does not match the descriptor"
                % entry.index
            )
        tiles_bytes += len(tile)
        tile_coords = _tile_start(meta.tile_counts, entry.index)
        dst_start = [
            coord * meta.tile_shape[axis] for axis, coord in enumerate(tile_coords)
        ]
        for local in _iter_coords(entry.dims):
            src = _flat_index(local, entry.dims) * element_size
            full = [dst_start[axis] + local[axis] for axis in range(len(meta.shape))]
            dst = _flat_index(full, meta.shape) * element_size
            out[dst : dst + element_size] = tile[src : src + element_size]
    if tiles_bytes != total:
        raise Corrupt(
            "float tiled streams decoded to %d bytes but the descriptor requires %d"
            % (tiles_bytes, total)
        )
    logical = tensor_logical_root(
        FLOAT_TILED_LOGICAL_DTYPE[meta.dtype],
        meta.shape,
        meta.element_count,
        bytes(out),
        limits,
    )
    if logical != meta.logical_root:
        raise Corrupt(
            "float tiled streams logical root does not match the descriptor"
        )
    return bytes(out)
