"""Identity helpers: domain-separated BLAKE3 and frame verification."""

from __future__ import annotations

import os
from dataclasses import asdict, dataclass

from .dependency import Resolver
from .frame import Frame, parse
from .hashing import (
    DOMAIN_CHUNK,
    HASH_LEN,
    content_root,
    content_root_hex,
    hash_domain,
    hash_domain_hex,
)
from .limits import Limits
from .modes import decode_frame

__all__ = [
    "DOMAIN_CHUNK",
    "HASH_LEN",
    "VerifyResult",
    "content_root",
    "content_root_hex",
    "hash_domain",
    "hash_domain_hex",
    "verify_bytes",
    "verify_frame",
]


@dataclass
class VerifyResult:
    """Structured result of parsing and decoding one frame."""

    ok: bool
    mode: "str | None" = None
    mode_value: "int | None" = None
    domain: "int | None" = None
    flags: "list[str] | None" = None
    features: "list[str] | None" = None
    decoded_len: "int | None" = None
    body_len: "int | None" = None
    chunk_count: "int | None" = None
    dependency_count: "int | None" = None
    level: "int | None" = None
    content_hash: "str | None" = None
    computed_content_hash: "str | None" = None
    error_class: "str | None" = None
    error: "str | None" = None

    def to_dict(self) -> dict:
        return asdict(self)


def _fill_from_header(result: VerifyResult, frame: Frame) -> None:
    header = frame.header
    result.mode = header.mode_name
    result.mode_value = header.mode
    result.domain = header.domain
    result.flags = header.flag_names()
    result.features = header.feature_names()
    result.decoded_len = header.decoded_len
    result.body_len = header.body_len
    result.chunk_count = header.chunk_count
    result.dependency_count = header.dep_count
    result.level = header.level
    result.content_hash = header.content_hash.hex()


def verify_bytes(
    data: bytes,
    resolver: "Resolver | None" = None,
    limits: "Limits | None" = None,
    base_frame: "bytes | None" = None,
) -> VerifyResult:
    """Parse, decode, and verify every hash in ``data``."""
    limits = limits or Limits()
    result = VerifyResult(ok=False)
    try:
        frame = parse(data, limits)
    except Exception as exc:  # typed Weft errors plus no unexpected swallowing
        return _failure(result, exc)
    _fill_from_header(result, frame)
    try:
        plaintext = decode_frame(frame, resolver, limits, base_frame)
    except Exception as exc:
        return _failure(result, exc, keep_header=True)
    result.computed_content_hash = content_root_hex(plaintext)
    result.ok = True
    return result


def verify_frame(
    path: "str | os.PathLike[str]",
    resolver: "Resolver | None" = None,
    limits: "Limits | None" = None,
    base_frame: "bytes | None" = None,
) -> VerifyResult:
    """Read ``path``, parse, decode, and verify it. Never raises for codec errors."""
    limits = limits or Limits()
    result = VerifyResult(ok=False)
    try:
        size = os.path.getsize(path)
    except OSError as exc:
        from .errors import Io

        return _failure(result, Io("cannot stat %s: %s" % (path, exc)))
    if size > limits.max_frame_bytes:
        from .errors import LimitExceeded

        return _failure(
            result,
            LimitExceeded(
                "frame length %d exceeds limit %d" % (size, limits.max_frame_bytes)
            ),
        )
    try:
        with open(path, "rb") as handle:
            data = handle.read()
    except OSError as exc:
        from .errors import Io

        return _failure(result, Io("cannot read %s: %s" % (path, exc)))
    return verify_bytes(data, resolver, limits, base_frame)


def _failure(
    result: VerifyResult, exc: Exception, keep_header: bool = False
) -> VerifyResult:
    error_class = getattr(exc, "error_class", None)
    if error_class is None:
        raise exc
    result.ok = False
    result.error_class = error_class
    result.error = str(exc)
    if not keep_header:
        result.mode = None
    return result
