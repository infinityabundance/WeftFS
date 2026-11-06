"""Domain-separated BLAKE3-256 hashing (WEFTCODEC-FORMAT §1.1).

Exactly one construction is used in v1::

    H(domain, data) = BLAKE3-256(u32le(|domain|) || domain || data)

Domain strings are ASCII, contain no NUL, and are versioned.
"""

from __future__ import annotations

from blake3 import blake3

HASH_LEN = 32

DOMAIN_CHUNK = "weft/chunk/v1"
DOMAIN_MANIFEST = "weft/manifest/v1"
DOMAIN_STRUCTURED = "weft/structured/v1"
DOMAIN_TENSOR_PAYLOAD = "weft/tensor/payload/v1"
DOMAIN_TOKENS_PAYLOAD = "weft/tokens/payload/v1"
DOMAIN_QUANTIZED_TENSOR_PAYLOAD = "weft/quantized-tensor/payload/v1"
DOMAIN_FLOAT_TENSOR_PAYLOAD = "weft/float-tensor/payload/v1"
DOMAIN_TENSOR_SHARDED_PAYLOAD = "weft/tensor-sharded/payload/v1"
DOMAIN_TENSOR_VALUES = "weft/tensor/values/v1"
DOMAIN_TENSOR_LOGICAL = "weft/tensor/logical/v1"
DOMAIN_TENSOR_TILE = "weft/tensor/tile/v1"
DOMAIN_TENSOR_SEMANTIC = "weft/tensor/semantic/v1"
DOMAIN_TENSOR_SEMANTIC_VALUES = "weft/tensor/semantic-values/v1"
DOMAIN_AI_STREAM = "weft/ai/stream/v1"


def hash_domain(domain: str, data: bytes) -> bytes:
    """Return the 32-byte domain-separated digest."""
    raw = domain.encode("ascii")
    hasher = blake3()
    hasher.update(len(raw).to_bytes(4, "little"))
    hasher.update(raw)
    hasher.update(data)
    return hasher.digest()


def hash_domain_hex(domain: str, data: bytes) -> str:
    return hash_domain(domain, data).hex()


def content_root(data: bytes) -> bytes:
    """``H("weft/chunk/v1", data)`` — the content root of plaintext bytes."""
    return hash_domain(DOMAIN_CHUNK, data)


def content_root_hex(data: bytes) -> str:
    return content_root(data).hex()
