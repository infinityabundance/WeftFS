"""Independent Python implementation of the WeftCodec v1 reader.

This package reads WFT1 frames and canonical WCB bytes, verifies content roots
with domain-separated BLAKE3, and exposes a PyTorch-facing dataset over a
mounted directory. It contains no Rust or Go extension modules.

Implemented payload modes: RAW, ZSTD, REFERENCE, DELTA, DICTIONARY,
STRUCTURED, TENSOR, TOKEN_SEQUENCE, QUANTIZED_TENSOR, FLOAT_TENSOR,
TENSOR_SHARDED, WEIGHT_DELTA, KV_CACHE, ACTIVATION_STREAM, SEMANTIC_DELTA,
TENSOR_TILED, and SPARSE_DELTA (every allocated mode 0-16). Reserved and
experimental mode bytes raise :class:`UnsupportedMode` in
:mod:`weftcodec.frame`.
"""

from __future__ import annotations

from .errors import (
    Corrupt,
    DependencyCycle,
    DependencyDenied,
    DependencyMismatch,
    DependencyMissing,
    LimitExceeded,
    Malformed,
    NonCanonical,
    UnsupportedFeature,
    UnsupportedMode,
    UnsupportedVersion,
    WeftError,
)
from .frame import Frame, FrameHeader, IMPLEMENTED_MODES, MODE_NAMES, parse
from .hashing import content_root, content_root_hex, hash_domain
from .identity import VerifyResult, verify_bytes, verify_frame

__version__ = "1.0.0"

__all__ = [
    "Corrupt",
    "DependencyCycle",
    "DependencyDenied",
    "DependencyMismatch",
    "DependencyMissing",
    "Frame",
    "FrameHeader",
    "IMPLEMENTED_MODES",
    "LimitExceeded",
    "MODE_NAMES",
    "Malformed",
    "NonCanonical",
    "UnsupportedFeature",
    "UnsupportedMode",
    "UnsupportedVersion",
    "VerifyResult",
    "WeftError",
    "__version__",
    "content_root",
    "content_root_hex",
    "hash_domain",
    "parse",
    "verify_bytes",
    "verify_frame",
]
