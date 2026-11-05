"""Decoder/encoder resource limits (WEFTCODEC-FORMAT §10).

Defaults are the normative v1 values. Every field can be lowered; nothing in
this package raises a limit above the frozen default.
"""

from __future__ import annotations

from dataclasses import dataclass, fields, replace

DEFAULT_MAX_FRAME_BYTES = 1 << 30
DEFAULT_MAX_DECODED_BYTES = 4 << 30
DEFAULT_MAX_DEPENDENCY_DEPTH = 8
DEFAULT_MAX_DEPENDENCY_COUNT = 64
DEFAULT_MAX_TOTAL_FETCHED_BYTES = 8 << 30
DEFAULT_MAX_CHUNKS = 1_048_576
DEFAULT_MAX_DICTIONARY_BYTES = 64 << 20
DEFAULT_MAX_STRUCTURED_DEPTH = 64
DEFAULT_MAX_STRUCTURED_ITEMS = 1_000_000
DEFAULT_MAX_STRING_BYTES = 256 << 20
DEFAULT_MAX_DELTA_OPS = 16_777_216
DEFAULT_MAX_TENSOR_ELEMENTS = 1 << 32
DEFAULT_MAX_TENSOR_RANK = 8
DEFAULT_MAX_TOKENS = 1 << 24


@dataclass(frozen=True)
class Limits:
    max_frame_bytes: int = DEFAULT_MAX_FRAME_BYTES
    max_decoded_bytes: int = DEFAULT_MAX_DECODED_BYTES
    max_dependency_depth: int = DEFAULT_MAX_DEPENDENCY_DEPTH
    max_dependency_count: int = DEFAULT_MAX_DEPENDENCY_COUNT
    max_total_fetched_bytes: int = DEFAULT_MAX_TOTAL_FETCHED_BYTES
    max_chunks: int = DEFAULT_MAX_CHUNKS
    max_dictionary_bytes: int = DEFAULT_MAX_DICTIONARY_BYTES
    max_structured_depth: int = DEFAULT_MAX_STRUCTURED_DEPTH
    max_structured_items: int = DEFAULT_MAX_STRUCTURED_ITEMS
    max_string_bytes: int = DEFAULT_MAX_STRING_BYTES
    max_delta_ops: int = DEFAULT_MAX_DELTA_OPS
    max_tensor_elements: int = DEFAULT_MAX_TENSOR_ELEMENTS
    max_tensor_rank: int = DEFAULT_MAX_TENSOR_RANK
    max_tokens: int = DEFAULT_MAX_TOKENS

    @classmethod
    def from_overrides(cls, overrides: "dict[str, int] | None") -> "Limits":
        """Build limits from a vector's ``limits`` object.

        Unknown keys are rejected so a typo can never silently widen a bound.
        """
        if not overrides:
            return cls()
        known = {f.name for f in fields(cls)}
        unknown = set(overrides) - known
        if unknown:
            raise ValueError("unknown limit(s): %s" % ", ".join(sorted(unknown)))
        return replace(cls(), **{k: int(v) for k, v in overrides.items()})
