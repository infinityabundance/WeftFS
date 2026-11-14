"""Domain-separated identity, exactly as `WEFTCODEC-FORMAT.md` §1 defines it.

    H(domain, data) = BLAKE3-256( u32le(len(domain)) || ASCII(domain) || data )

The domain is ASCII without a NUL. No other hash form is accepted by the
verifier.
"""

from __future__ import annotations

from blake3 import blake3_256


def hash_domain(domain: str, data: bytes) -> bytes:
    if not isinstance(domain, str) or not domain:
        raise ValueError("hash domain must be a non-empty string")
    raw = domain.encode("ascii")
    if len(raw) > 0xFFFFFFFF:
        raise ValueError("domain too long")
    return blake3_256(len(raw).to_bytes(4, "little") + raw + bytes(data))
