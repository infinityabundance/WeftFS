"""Deterministic frame generator for the randomized differential court.

This is **test support**, not part of the reader. It constructs WFT1 frames
from the normative encoding rules so the Python decoder and the Rust CLI can
be compared on the same bytes. Every generated descriptor, plane, and hash is
checked by the Rust CLI first; a generator bug therefore fails the court rather
than silently weakening it.

The PRNG is a deterministic xorshift64*; a seed fully determines the corpus.
"""

from __future__ import annotations

import struct

from weftcodec import wcb
from weftcodec.crc32c import crc32c
from weftcodec.hashing import (
    DOMAIN_TENSOR_SHARDED_PAYLOAD,
    DOMAIN_TENSOR_VALUES,
    hash_domain,
)

U64_MAX = (1 << 64) - 1
U32_MAX = (1 << 32) - 1

FLOAT_FIELDS = {
    "bf16": (1, 8, 7),
    "fp16": (1, 5, 10),
    "fp32": (1, 8, 23),
    "fp8-e4m3": (1, 4, 3),
    "fp8-e5m2": (1, 5, 2),
}
WEIGHT_STORAGE = {"bf16": 2, "fp16": 2, "fp32": 4, "q8_0": 1, "q4_0": 1}
WEIGHT_BITS = {"bf16": 16, "fp16": 16, "fp32": 32, "q8_0": 8, "q4_0": 4}
WEIGHT_PLANES = {"bf16": 2, "fp16": 2, "fp32": 4, "q8_0": 1, "q4_0": 1}
KV_BYTES = {"bf16": 2, "fp16": 2, "fp32": 4}
TENSOR_SIZES = {
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
BLOCK_SIZES = (16, 32, 64, 128, 256)


class Rng:
    """Deterministic xorshift64* (seed 0 is remapped to 1)."""

    MASK = (1 << 64) - 1

    def __init__(self, seed: int) -> None:
        self.state = (seed & self.MASK) or 0x9E3779B97F4A7C15

    def next_u64(self) -> int:
        x = self.state
        x ^= (x << 13) & self.MASK
        x ^= x >> 7
        x ^= (x << 17) & self.MASK
        self.state = x & self.MASK
        return (self.state * 0x2545F4914F6CDD1D) & self.MASK

    def randint(self, low: int, high: int) -> int:
        """Inclusive integer range."""
        return low + self.next_u64() % (high - low + 1)

    def choice(self, items):
        return items[self.next_u64() % len(items)]

    def bytes(self, length: int) -> bytes:
        out = bytearray()
        while len(out) < length:
            out.extend(self.next_u64().to_bytes(8, "little"))
        return bytes(out[:length])


# ---------------------------------------------------------------------------
# Canonical Huffman encoder (test-only; mirrors WEFT-AI-CODEC §5.3.1)
# ---------------------------------------------------------------------------


def huffman_encode(plane: bytes) -> bytes:
    if not plane:
        return bytes(256)
    frequencies = [0] * 256
    for byte in plane:
        frequencies[byte] += 1
    used = [index for index, value in enumerate(frequencies) if value]
    if len(used) == 1:
        lengths = [0] * 256
        lengths[used[0]] = 1
        return bytes(lengths)

    weights = list(frequencies)
    while True:
        lengths = _huffman_lengths(weights)
        if max(lengths) <= 24:
            break
        weights = [max(1, value >> 1) if value else 0 for value in weights]
    codes = _canonical_codes(lengths)
    out = bytearray()
    accumulator = 0
    bits = 0
    for byte in plane:
        code, length = codes[byte]
        accumulator = (accumulator << length) | code
        bits += length
        while bits >= 8:
            bits -= 8
            out.append((accumulator >> bits) & 0xFF)
    if bits:
        out.append((accumulator << (8 - bits)) & 0xFF)
    return bytes(lengths) + bytes(out)


def _huffman_lengths(weights):
    nodes = []
    for symbol in range(256):
        if weights[symbol]:
            nodes.append(
                {
                    "weight": weights[symbol],
                    "minimum": symbol,
                    "index": len(nodes),
                    "symbol": symbol,
                    "left": None,
                    "right": None,
                }
            )
    if not nodes:
        return [0] * 256
    if len(nodes) == 1:
        lengths = [0] * 256
        lengths[nodes[0]["symbol"]] = 1
        return lengths
    active = list(nodes)
    next_index = len(nodes)
    while len(active) > 1:
        active.sort(key=lambda node: (node["weight"], node["minimum"], node["index"]))
        left = active.pop(0)
        right = active.pop(0)
        parent = {
            "weight": left["weight"] + right["weight"],
            "minimum": min(left["minimum"], right["minimum"]),
            "index": next_index,
            "symbol": None,
            "left": left,
            "right": right,
        }
        next_index += 1
        active.append(parent)
    lengths = [0] * 256

    def walk(node, depth):
        if node["symbol"] is not None:
            lengths[node["symbol"]] = depth
            return
        walk(node["left"], depth + 1)
        walk(node["right"], depth + 1)

    walk(active[0], 0)
    return lengths


def _canonical_codes(lengths):
    order = sorted(
        (length, symbol) for symbol, length in enumerate(lengths) if length
    )
    codes = {}
    code = 0
    previous = 0
    for length, symbol in order:
        code <<= length - previous
        codes[symbol] = (code, length)
        code += 1
        previous = length
    return codes


# ---------------------------------------------------------------------------
# WFT1 frame builder
# ---------------------------------------------------------------------------


def build_frame(mode, decoded_len, content_hash, body, dependencies=()):
    dep_count = len(dependencies)
    table = bytearray()
    for entry in dependencies:
        table.extend(entry["hash"])
        table.extend(struct.pack("<Q", entry["decoded_len"]))
        table.append(0)  # dep_domain = payload
        table.append(1)  # REQUIRED
        table.extend(b"\x00\x00")
        table.extend(struct.pack("<I", entry.get("codec_hint", 0)))
    header = bytearray(96)
    header[0:4] = b"WFT1"
    header[4] = 1
    header[5] = 0
    header[6] = 0
    header[7] = mode
    header[8:10] = struct.pack("<H", 0)
    header[10:12] = struct.pack("<H", 0)
    header[12:20] = struct.pack("<Q", decoded_len)
    header[20:28] = struct.pack("<Q", len(body))
    header[28:32] = struct.pack("<I", dep_count)
    header[32:36] = struct.pack("<I", 0)
    header[36:38] = struct.pack("<H", 1 if dep_count else 0)
    header[38:40] = struct.pack("<H", 0)
    header[40:72] = content_hash
    header[76:84] = struct.pack("<Q", len(table))
    header[84:88] = struct.pack("<I", 0)
    header[88:96] = struct.pack("<Q", 0)
    header[72:76] = struct.pack("<I", crc32c(bytes(header[0:72]) + bytes(header[76:96])))
    frame = bytes(header) + bytes(table) + body
    return frame + struct.pack("<I", crc32c(frame))


def ai_body(descriptor, payload):
    encoded = wcb.encode(descriptor)
    return struct.pack("<I", len(encoded)) + encoded + payload


def shape_bytes(shape, count, rng, quantized=None):
    if quantized == "q4_0":
        return bytes(rng.randint(0, 15) for _ in range(count))
    if quantized == "q8_0":
        return bytes(rng.randint(0, 255) for _ in range(count))
    return rng.bytes(count)


def _random_shape(rng, max_rank=3, max_extent=4, minimum=1):
    rank = rng.randint(1, max_rank)
    shape = [rng.randint(minimum, max_extent) for _ in range(rank)]
    return shape


def _product(shape):
    value = 1
    for dim in shape:
        value *= dim
    return value


# ---------------------------------------------------------------------------
# Mode 6 base frames (used by the delta modes)
# ---------------------------------------------------------------------------

TENSOR_BASE_DTYPE = {
    "bf16": "bf16",
    "fp16": "f16",
    "fp32": "f32",
    "q8_0": "i8",
    "q4_0": "u8",
}


def tensor_base_frame(dtype, shape, data):
    descriptor = {
        "schema": "weft.tensor.v1",
        "dtype": TENSOR_BASE_DTYPE[dtype],
        "shape": [wcb.U64(dim) for dim in shape],
        "layout": "row_major",
        "endianness": "little",
        "element_count": wcb.U64(_product(shape)),
        "payload_bytes": wcb.U64(len(data)),
        "payload_hash": hash_domain("weft/tensor/payload/v1", data),
    }
    body = ai_body(descriptor, data)
    return build_frame(6, len(data), hash_domain("weft/chunk/v1", data), body)


# ---------------------------------------------------------------------------
# Mode-specific case builders
# ---------------------------------------------------------------------------


def float_case(rng):
    dtype = rng.choice(sorted(FLOAT_FIELDS))
    sign_bits, exponent_bits, mantissa_bits = FLOAT_FIELDS[dtype]
    element_size = (sign_bits + exponent_bits + mantissa_bits) // 8
    shape = _random_shape(rng, max_rank=3, max_extent=4)
    count = _product(shape)
    payload = rng.bytes(count * element_size)
    exponents = bytearray()
    bits = []
    for index in range(count):
        word = int.from_bytes(
            payload[index * element_size : (index + 1) * element_size], "little"
        )
        exponents.append((word >> mantissa_bits) & ((1 << exponent_bits) - 1))
        sign = (word >> (exponent_bits + mantissa_bits)) & 1
        mantissa = word & ((1 << mantissa_bits) - 1)
        for bit in range(sign_bits - 1, -1, -1):
            bits.append((sign >> bit) & 1)
        for bit in range(mantissa_bits - 1, -1, -1):
            bits.append((mantissa >> bit) & 1)
    mantissa_plane = _pack_bits(bits)
    exponent_plane = huffman_encode(bytes(exponents))
    stored = exponent_plane + mantissa_plane
    descriptor = {
        "schema": "weft.ai.float_tensor.v1",
        "dtype": dtype,
        "shape": [wcb.U64(dim) for dim in shape],
        "element_count": wcb.U64(count),
        "sign_bits": wcb.U64(sign_bits),
        "exponent_bits": wcb.U64(exponent_bits),
        "mantissa_bits": wcb.U64(mantissa_bits),
        "exponent_coder": "huffman-canonical-v1",
        "exponent_plane_len": wcb.U64(len(exponent_plane)),
        "mantissa_plane_len": wcb.U64(len(mantissa_plane)),
        "payload_hash": hash_domain("weft/float-tensor/payload/v1", stored),
    }
    frame = build_frame(
        9,
        len(payload),
        hash_domain("weft/chunk/v1", payload),
        ai_body(descriptor, stored),
    )
    return {"mode": 9, "kind": "single", "frames": [frame], "base": None}


def _pack_bits(bits):
    out = bytearray()
    accumulator = 0
    count = 0
    for bit in bits:
        accumulator = (accumulator << 1) | bit
        count += 1
        if count == 8:
            out.append(accumulator)
            accumulator = 0
            count = 0
    if count:
        out.append(accumulator << (8 - count))
    return bytes(out)


def sharded_case(rng):
    dtype = rng.choice(["q8_0", "q4_0"])
    presets = [([16], 16), ([4, 4], 16), ([4, 8], 32), ([8, 4], 32),
               ([32], 32), ([4, 4, 4], 64), ([8, 8], 64), ([4, 16], 64)]
    shape, count = rng.choice(presets)
    axis = rng.randint(0, len(shape) - 1)
    divisors = [value for value in (1, 2, 4, 8) if shape[axis] % value == 0]
    shard_count = rng.choice(divisors)
    block_size = rng.choice([value for value in BLOCK_SIZES if count % value == 0])
    block_count = count // block_size
    valid_scales = [0x3F800000, 0x40000000, 0x3F000000, 0x40490FDB, 0x40800000]
    scales = [rng.choice(valid_scales) for _ in range(block_count)]
    zero_points = [rng.randint(-1000, 1000) for _ in range(block_count)]
    if dtype == "q8_0":
        words = [rng.randint(-128, 127) for _ in range(count)]
    else:
        words = [rng.randint(-8, 7) for _ in range(count)]
    from weftcodec import ai_modes

    root = ai_modes.sharded_logical_root(
        dtype, shape, words, scales, zero_points, _default_limits()
    )
    shard_words = split_words(shape, axis, shard_count, words)
    scale_plane = b"".join(struct.pack("<I", value) for value in scales)
    frames = []
    for index, slab in enumerate(shard_words):
        if dtype == "q8_0":
            quant = bytes(value & 0xFF for value in slab)
        else:
            quant = bytearray()
            for pair in range(0, len(slab), 2):
                low = slab[pair] & 0x0F
                high = slab[pair + 1] & 0x0F
                quant.append(low | (high << 4))
            quant = bytes(quant)
        planes = quant + scale_plane
        stored = huffman_encode(planes)
        descriptor = {
            "schema": "weft.ai.tensor_sharded.v1",
            "dtype": dtype,
            "shape": [wcb.U64(dim) for dim in shape],
            "element_count": wcb.U64(count),
            "shard_axis": wcb.U64(axis),
            "shard_count": wcb.U64(shard_count),
            "shard_index": wcb.U64(index),
            "block_size": wcb.U64(block_size),
            "scales": [wcb.U64(value) for value in scales],
            "zero_points": [wcb.I64(value) for value in zero_points],
            "layout_root": root,
            "payload_len": wcb.U64(len(stored)),
            "payload_hash": hash_domain(DOMAIN_TENSOR_SHARDED_PAYLOAD, stored),
        }
        frames.append(
            build_frame(
                10,
                len(planes),
                hash_domain("weft/chunk/v1", planes),
                ai_body(descriptor, stored),
            )
        )
    return {
        "mode": 10,
        "kind": "sharded",
        "frames": frames,
        "base": None,
        "layout_root": root,
    }


def split_words(shape, axis, shard_count, words):
    count = len(shape)
    shard_shape = list(shape)
    shard_shape[axis] = shard_shape[axis] // shard_count
    plane = shape[axis] // shard_count
    total = len(words)
    strides = [0] * count
    stride = 1
    for index in range(count - 1, -1, -1):
        strides[index] = stride
        stride *= shape[index]
    shard_strides = [0] * count
    stride = 1
    for index in range(count - 1, -1, -1):
        shard_strides[index] = stride
        stride *= shard_shape[index]
    shards = [[0] * _product(shard_shape) for _ in range(shard_count)]
    for flat in range(total):
        coords = [0] * count
        remainder = flat
        for index in range(count - 1, -1, -1):
            coords[index] = remainder % shape[index]
            remainder //= shape[index]
        shard = coords[axis] // plane
        local = [coords[i] if i != axis else coords[i] % plane for i in range(count)]
        local_flat = 0
        for index in range(count):
            local_flat = local_flat * shard_shape[index] + local[index]
        shards[shard][local_flat] = words[flat]
    return shards


def _default_limits():
    from weftcodec.limits import Limits

    return Limits()


def delta_case(rng, mode):
    dtype = rng.choice(["bf16", "fp16", "fp32", "q8_0", "q4_0"])
    shape = _random_shape(rng, max_rank=3, max_extent=4)
    count = _product(shape)
    storage = WEIGHT_STORAGE[dtype]
    if dtype == "q4_0":
        base = bytes(rng.randint(0, 15) for _ in range(count))
        target = bytes(rng.randint(0, 15) for _ in range(count))
    else:
        base = rng.bytes(count * storage)
        target = rng.bytes(count * storage)
    base_frame = tensor_base_frame(dtype, shape, base)
    base_root = hash_domain("weft/chunk/v1", base)

    def word_at(data, index):
        if storage == 1:
            return data[index]
        return int.from_bytes(data[index * storage : (index + 1) * storage], "little")

    mask = (1 << WEIGHT_BITS[dtype]) - 1
    deltas = [
        (word_at(target, index) - word_at(base, index)) & mask
        for index in range(count)
    ]
    delta_bytes = bytearray()
    for delta in deltas:
        delta_bytes.extend(delta.to_bytes(storage, "little"))
    delta_bytes = bytes(delta_bytes)
    delta_root = hash_domain(DOMAIN_TENSOR_VALUES, delta_bytes)

    if mode in (11, 14):
        planes = []
        for plane in range(WEIGHT_PLANES[dtype]):
            planes.append(
                bytes((delta >> (8 * plane)) & 0xFF for delta in deltas)
            )
        payload = b"".join(huffman_encode(plane) for plane in planes)
    else:
        payload = zero_run_payload(deltas, storage)

    descriptor = {
        "schema": {
            11: "weft.ai.weight_delta.v1",
            14: "weft.ai.semantic_delta.v1",
            16: "weft.ai.sparse_delta.v1",
        }[mode],
        "dtype": dtype,
        "shape": [wcb.U64(dim) for dim in shape],
        "element_count": wcb.U64(count),
    }
    if mode == 16:
        descriptor["storage_bytes"] = wcb.U64(storage)
        descriptor["op"] = "mod-sub"
        descriptor["sparse_codec"] = "zero-run-v1"
        descriptor["base_content_root"] = base_root
        descriptor["delta_root"] = delta_root
    else:
        descriptor["word_bits"] = wcb.U64(WEIGHT_BITS[dtype])
        descriptor["base"] = base_root
        descriptor["base_dtype"] = dtype
        descriptor["base_shape"] = [wcb.U64(dim) for dim in shape]
        if mode == 14:
            policy, policy_value = _policy_for(rng, dtype, shape, base, count)
            descriptor["base_semantic_root"] = policy
            descriptor["policy"] = policy_value
        descriptor["delta_root"] = delta_root
        if mode == 11:
            descriptor["op"] = "mod-sub"
            descriptor["planes"] = wcb.U64(WEIGHT_PLANES[dtype])
        descriptor["plane_codec"] = "huffman-canonical-v1"
        descriptor["payload_len"] = wcb.U64(len(payload))

    dependency = {
        "hash": base_root,
        "decoded_len": len(target),
        "codec_hint": 6,
    }
    frame = build_frame(
        mode,
        len(target),
        hash_domain("weft/chunk/v1", target),
        ai_body(descriptor, payload),
        dependencies=[dependency],
    )
    return {
        "mode": mode,
        "kind": "single",
        "frames": [frame],
        "base": base_frame,
        "target": target,
    }


def zero_run_payload(deltas, storage):
    out = bytearray()
    run_count = sum(1 for delta in deltas if delta != 0)
    out.extend(struct.pack("<I", run_count))
    zero_run = 0
    for delta in deltas:
        if delta == 0:
            zero_run += 1
            continue
        out.extend(_varint(zero_run))
        out.extend(delta.to_bytes(storage, "little"))
        zero_run = 0
    return bytes(out)


def _varint(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def _policy_for(rng, dtype, shape, base, count):
    from weftcodec import ai_modes

    choices = ["none"]
    if dtype in ("bf16", "fp16", "fp32"):
        choices.append("mantissa")
    choices.append("block")
    kind = rng.choice(choices)
    if kind == "none":
        policy = ai_modes.SemanticPolicy("none")
        value = {"scheme": "none"}
    elif kind == "mantissa":
        maximum = {"bf16": 7, "fp16": 10, "fp32": 23}[dtype]
        bits = rng.randint(1, maximum)
        policy = ai_modes.SemanticPolicy("mantissa_truncate", bits=bits)
        value = {"bits": wcb.U64(bits), "scheme": "mantissa_truncate"}
    else:
        divisors = [value for value in (1, 2, 4, 8, 16) if count % value == 0]
        block = rng.choice(divisors)
        levels = rng.choice([2, 4, 16, 256])
        policy = ai_modes.SemanticPolicy(
            "block_quantize", block_size=block, levels=levels
        )
        value = {
            "block_size": wcb.U64(block),
            "levels": wcb.U64(levels),
            "scheme": "block_quantize",
        }
    root = ai_modes.semantic_root(dtype, shape, base, policy, _default_limits())
    return root, value


def kv_case(rng):
    dtype = rng.choice(["bf16", "fp16", "fp32"])
    word_bytes = KV_BYTES[dtype]
    rows = rng.randint(1, 4)
    head_count = rng.randint(1, 4)
    head_dim = rng.randint(1, 4)
    element_count = rows * head_count * head_dim
    page_bytes = element_count * word_bytes
    page_count = rng.randint(1, 4)
    page_index = rng.randint(0, page_count - 1)
    seq_start = rng.randint(0, 4096)
    seq_end = seq_start + rows
    payload = rng.bytes(page_bytes)
    frames = []
    for index in range(page_count):
        one = payload if index == page_index else rng.bytes(page_bytes)
        planes = [
            bytes(one[element * word_bytes + plane] for element in range(element_count))
            for plane in range(word_bytes)
        ]
        stored = b"".join(huffman_encode(plane) for plane in planes)
        descriptor = {
            "schema": "weft.ai.kv_cache.v1",
            "model": "diff-model",
            "layer": wcb.U64(rng.randint(0, 31)),
            "session": "diff-session",
            "page_index": wcb.U64(index),
            "page_count": wcb.U64(page_count),
            "seq_start": wcb.U64(seq_start),
            "seq_end": wcb.U64(seq_end),
            "dtype": dtype,
            "head_count": wcb.U64(head_count),
            "head_dim": wcb.U64(head_dim),
            "shape": [wcb.U64(rows), wcb.U64(head_count), wcb.U64(head_dim)],
            "page_bytes": wcb.U64(page_bytes),
            "plane_codec": "huffman-canonical-v1",
            "payload_len": wcb.U64(len(stored)),
        }
        frames.append(
            build_frame(
                12,
                page_bytes,
                hash_domain("weft/chunk/v1", one),
                ai_body(descriptor, stored),
            )
        )
    return {"mode": 12, "kind": "pages", "frames": frames, "base": None}


def activation_case(rng):
    dtype = rng.choice(["bf16", "fp16", "fp32"])
    word_bytes = KV_BYTES[dtype]
    stream = rng.choice(["activation", "gradient"])
    world = rng.randint(1, 4)
    rank = rng.randint(0, world - 1)
    chunk_count = rng.randint(1, 4)
    shape = _random_shape(rng, max_rank=3, max_extent=4)
    element_count = _product(shape)
    byte_len = element_count * word_bytes
    frames = []
    for index in range(chunk_count):
        payload = rng.bytes(byte_len)
        planes = [
            bytes(
                payload[element * word_bytes + plane]
                for element in range(element_count)
            )
            for plane in range(word_bytes)
        ]
        stored = b"".join(huffman_encode(plane) for plane in planes)
        descriptor = {
            "schema": "weft.ai.activation_stream.v1",
            "stream": stream,
            "step": wcb.U64(rng.randint(0, 1_000_000)),
            "rank": wcb.U64(rank),
            "world": wcb.U64(world),
            "chunk_index": wcb.U64(index),
            "chunk_count": wcb.U64(chunk_count),
            "dtype": dtype,
            "shape": [wcb.U64(dim) for dim in shape],
            "element_count": wcb.U64(element_count),
            "digest_root": hash_domain(DOMAIN_TENSOR_VALUES, payload),
            "plane_codec": "huffman-canonical-v1",
            "payload_len": wcb.U64(len(stored)),
        }
        frames.append(
            build_frame(
                13,
                byte_len,
                hash_domain("weft/chunk/v1", payload),
                ai_body(descriptor, stored),
            )
        )
    return {"mode": 13, "kind": "chunks", "frames": frames, "base": None}


def tiled_case(rng):
    dtype = rng.choice(sorted(TENSOR_SIZES))
    element_size = TENSOR_SIZES[dtype]
    shape = _random_shape(rng, max_rank=2, max_extent=4)
    tile_shape = [rng.choice([value for value in (1, 2, 4) if dim % value == 0]) for dim in shape]
    counts = [shape[i] // tile_shape[i] for i in range(len(shape))]
    element_count = _product(shape)
    payload = rng.bytes(element_count * element_size)
    total = _product(counts)
    stored = bytearray()
    entries = []
    for index in range(total):
        coords = _tile_coords(counts, index)
        start = [coords[axis] * tile_shape[axis] for axis in range(len(shape))]
        tile = _box_bytes(payload, shape, start, tile_shape, element_size)
        tile_bytes = bytearray()
        for plane in range(element_size):
            plane_bytes = bytes(
                tile[element * element_size + plane]
                for element in range(_product(tile_shape))
            )
            tile_bytes.extend(huffman_encode(plane_bytes))
        root = _tile_root(dtype, tile_shape, bytes(tile))
        entries.append(
            {
                "index": wcb.U64(index),
                "dims": [wcb.U64(dim) for dim in tile_shape],
                "offset": wcb.U64(len(stored)),
                "compressed_len": wcb.U64(len(tile_bytes)),
                "element_len": wcb.U64(len(tile)),
                "tile_root": root,
            }
        )
        stored.extend(tile_bytes)
    from weftcodec import ai_modes

    logical = ai_modes.tensor_logical_root(
        dtype, shape, element_count, payload, _default_limits()
    )
    descriptor = {
        "schema": "weft.ai.tensor_tiled.v1",
        "dtype": dtype,
        "shape": [wcb.U64(dim) for dim in shape],
        "element_count": wcb.U64(element_count),
        "layout": "row_major",
        "tile_shape": [wcb.U64(dim) for dim in tile_shape],
        "tile_counts": [wcb.U64(dim) for dim in counts],
        "tile_codec": "huffman-canonical-v1",
        "tiles": entries,
        "payload_len": wcb.U64(len(stored)),
        "logical_root": logical,
    }
    frame = build_frame(
        15,
        len(payload),
        hash_domain("weft/chunk/v1", payload),
        ai_body(descriptor, bytes(stored)),
    )
    return {"mode": 15, "kind": "single", "frames": [frame], "base": None}


def _tile_root(dtype, dims, tile):
    from weftcodec import ai_modes

    return ai_modes.tile_root(dtype, dims, tile, _default_limits())


def _tile_coords(counts, index):
    coords = [0] * len(counts)
    remainder = index
    for axis in range(len(counts) - 1, -1, -1):
        coords[axis] = remainder % counts[axis]
        remainder //= counts[axis]
    return coords


def _box_bytes(payload, shape, start, extent, element_size):
    strides = [0] * len(shape)
    stride = element_size
    for index in range(len(shape) - 1, -1, -1):
        strides[index] = stride
        stride *= shape[index]
    out = bytearray()

    def walk(dimension, coords):
        if dimension == len(extent):
            offset = sum(
                (start[axis] + coords[axis]) * strides[axis]
                for axis in range(len(shape))
            )
            out.extend(payload[offset : offset + element_size])
            return
        for position in range(extent[dimension]):
            walk(dimension + 1, coords + [position])

    walk(0, [])
    return bytes(out)


CASE_BUILDERS = {
    9: float_case,
    10: sharded_case,
    11: lambda rng: delta_case(rng, 11),
    12: kv_case,
    13: activation_case,
    14: lambda rng: delta_case(rng, 14),
    15: tiled_case,
    16: lambda rng: delta_case(rng, 16),
}


def generate(mode, seed, count):
    """Yield ``count`` independent deterministic cases for ``mode``."""
    rng = Rng(seed)
    builder = CASE_BUILDERS[mode]
    for _ in range(count):
        yield builder(rng)


def frame_count(case):
    return len(case["frames"])
