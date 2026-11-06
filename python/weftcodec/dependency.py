"""Dependency resolution for REFERENCE/DELTA/DICTIONARY frames.

Resolution contract (WEFTCODEC-FORMAT §5)::

    resolve(hash, decoded_len, dep_domain) -> bytes | typed_error

The decoder verifies the resolved bytes' length and content hash itself; a
resolver may never substitute another representation.
"""

from __future__ import annotations

import os
from pathlib import Path

from .errors import (
    DependencyCycle,
    DependencyMismatch,
    DependencyMissing,
    Io,
    LimitExceeded,
)
from .frame import Dependency
from .hashing import content_root


class Resolver:
    """Resolver protocol. Implementations return the exact dependency bytes.

    ``resolve_frame`` is optional: a resolver that can also supply complete
    frames (mode 11/14/16 use it to verify a base frame's own dtype/shape)
    overrides it. The default reports frames as unavailable.
    """

    def resolve(self, hash_bytes: bytes, decoded_len: int, dep_domain: int) -> bytes:
        raise NotImplementedError

    def resolve_frame(self, content_hash: bytes) -> "bytes | None":
        return None


class DictResolver(Resolver):
    """In-memory resolver keyed by a 32-byte digest (hex keys accepted)."""

    def __init__(self, entries: "dict[bytes | str, bytes] | None" = None) -> None:
        self._entries = {}
        self._frames = {}
        if entries:
            for key, value in entries.items():
                self.add(key, value)

    def add(self, key: "bytes | str", value: bytes) -> None:
        raw = bytes.fromhex(key) if isinstance(key, str) else bytes(key)
        if len(raw) != 32:
            raise ValueError("dependency key must be a 32-byte digest")
        self._entries[raw] = bytes(value)

    def add_frame(self, key: "bytes | str", value: bytes) -> None:
        raw = bytes.fromhex(key) if isinstance(key, str) else bytes(key)
        if len(raw) != 32:
            raise ValueError("frame key must be a 32-byte digest")
        self._frames[raw] = bytes(value)

    def resolve(self, hash_bytes: bytes, decoded_len: int, dep_domain: int) -> bytes:
        try:
            return self._entries[hash_bytes]
        except KeyError:
            raise DependencyMissing(
                "dependency %s is not present in the resolver" % hash_bytes.hex()
            )

    def resolve_frame(self, content_hash: bytes) -> "bytes | None":
        return self._frames.get(content_hash)


class DirectoryResolver(Resolver):
    """Resolve dependencies from a directory of files named by lowercase hex.

    This matches the Rust CLI's ``verify --deps DIR`` layout.
    """

    def __init__(self, directory: "str | os.PathLike[str]") -> None:
        self.directory = Path(directory)

    def resolve(self, hash_bytes: bytes, decoded_len: int, dep_domain: int) -> bytes:
        path = self.directory / hash_bytes.hex()
        try:
            with open(path, "rb") as handle:
                return handle.read()
        except FileNotFoundError:
            raise DependencyMissing(
                "dependency %s is not present in %s" % (hash_bytes.hex(), self.directory)
            )
        except OSError as exc:
            raise Io("cannot read dependency %s: %s" % (path, exc))


class FrameSourceResolver(Resolver):
    """Resolve dependency plaintext by decoding frames from a frame source.

    Mirrors the Rust ``DagResolver``: fetches by content hash, parses each
    frame, decodes recursively with a visiting set (cycle detection), and
    verifies the decoded length and plaintext hash at every boundary. Depth,
    total fetched bytes, and cumulative decoded bytes are enforced.
    """

    def __init__(self, frames, limits, visiting=None, depth=0, stats=None) -> None:
        self.frames = frames
        self.limits = limits
        self.visiting = visiting if visiting is not None else set()
        self.depth = depth
        self.stats = stats if stats is not None else {"fetched": 0, "decoded": 0}

    def _frame_bytes(self, content_hash: bytes) -> "bytes | None":
        return self.frames.get(bytes(content_hash))

    def resolve_frame(self, content_hash: bytes) -> "bytes | None":
        return self._frame_bytes(content_hash)

    def _child(self, depth: int) -> "FrameSourceResolver":
        return FrameSourceResolver(
            self.frames, self.limits, self.visiting, depth, self.stats
        )

    def resolve(self, hash_bytes: bytes, decoded_len: int, dep_domain: int) -> bytes:
        from .frame import parse
        from .modes import decode_bytes

        depth = self.depth + 1
        if depth > self.limits.max_dependency_depth:
            raise LimitExceeded(
                "dependency depth %d exceeds limit %d"
                % (depth, self.limits.max_dependency_depth)
            )
        if hash_bytes in self.visiting:
            raise DependencyCycle(
                "dependency cycle re-entered frame %s" % hash_bytes.hex()
            )
        frame_bytes = self._frame_bytes(hash_bytes)
        if frame_bytes is None:
            raise DependencyMissing(
                "frame source has no frame for %s" % hash_bytes.hex()
            )
        self.stats["fetched"] += len(frame_bytes)
        if self.stats["fetched"] > self.limits.max_total_fetched_bytes:
            raise LimitExceeded(
                "total fetched bytes %d exceed limit %d"
                % (self.stats["fetched"], self.limits.max_total_fetched_bytes)
            )
        parsed = parse(frame_bytes, self.limits)
        if parsed.header.content_hash != hash_bytes:
            raise DependencyMismatch(
                "frame at %s declares content hash %s"
                % (hash_bytes.hex(), parsed.header.content_hash.hex())
            )

        self.visiting.add(hash_bytes)
        child = self._child(depth)
        try:
            decoded = decode_bytes(frame_bytes, child, self.limits)
        finally:
            self.visiting.discard(hash_bytes)
        if len(decoded) != decoded_len:
            raise DependencyMismatch(
                "dependency frame %s decodes to %d bytes, declared %d"
                % (hash_bytes.hex(), len(decoded), decoded_len)
            )
        if content_root(decoded) != hash_bytes:
            raise DependencyMismatch(
                "dependency frame plaintext hash mismatch"
            )
        self.stats["decoded"] += len(decoded)
        if self.stats["decoded"] > self.limits.max_decoded_bytes:
            raise LimitExceeded(
                "cumulative decoded dependency bytes %d exceed limit %d"
                % (self.stats["decoded"], self.limits.max_decoded_bytes)
            )
        return decoded


class DirectoryFrameSourceResolver(FrameSourceResolver):
    """Frame source backed by a directory of frames named by content hash."""

    def __init__(self, directory, limits, visiting=None, depth=0, stats=None) -> None:
        self.directory = Path(directory)
        super().__init__({}, limits, visiting, depth, stats)

    def _frame_bytes(self, content_hash: bytes) -> "bytes | None":
        path = self.directory / bytes(content_hash).hex()
        try:
            with open(path, "rb") as handle:
                return handle.read()
        except FileNotFoundError:
            return None
        except OSError as exc:
            raise Io("cannot read frame %s: %s" % (path, exc))

    def _child(self, depth: int) -> "FrameSourceResolver":
        return DirectoryFrameSourceResolver(
            self.directory, self.limits, self.visiting, depth, self.stats
        )


def resolve_dependency(
    dependency: Dependency, resolver: "Resolver | None"
) -> bytes:
    """Resolve and verify one dependency entry."""
    if resolver is None:
        raise DependencyMissing(
            "frame needs dependency %s but no resolver was supplied"
            % dependency.hash.hex()
        )
    data = resolver.resolve(dependency.hash, dependency.decoded_len, dependency.dep_domain)
    if not isinstance(data, (bytes, bytearray, memoryview)):
        raise DependencyMismatch("resolver returned a non-bytes value")
    data = bytes(data)
    if len(data) != dependency.decoded_len:
        raise DependencyMismatch(
            "dependency length %d differs from declared %d"
            % (len(data), dependency.decoded_len)
        )
    if content_root(data) != dependency.hash:
        raise DependencyMismatch("dependency content hash mismatch")
    return data
