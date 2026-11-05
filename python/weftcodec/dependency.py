"""Dependency resolution for REFERENCE/DELTA/DICTIONARY frames.

Resolution contract (WEFTCODEC-FORMAT §5)::

    resolve(hash, decoded_len, dep_domain) -> bytes | typed_error

The decoder verifies the resolved bytes' length and content hash itself; a
resolver may never substitute another representation.
"""

from __future__ import annotations

import os
from pathlib import Path

from .errors import DependencyMismatch, DependencyMissing, Io
from .frame import Dependency
from .hashing import content_root


class Resolver:
    """Resolver protocol. Implementations return the exact dependency bytes."""

    def resolve(self, hash_bytes: bytes, decoded_len: int, dep_domain: int) -> bytes:
        raise NotImplementedError


class DictResolver(Resolver):
    """In-memory resolver keyed by a 32-byte digest (hex keys accepted)."""

    def __init__(self, entries: "dict[bytes | str, bytes] | None" = None) -> None:
        self._entries = {}
        if entries:
            for key, value in entries.items():
                self.add(key, value)

    def add(self, key: "bytes | str", value: bytes) -> None:
        raw = bytes.fromhex(key) if isinstance(key, str) else bytes(key)
        if len(raw) != 32:
            raise ValueError("dependency key must be a 32-byte digest")
        self._entries[raw] = bytes(value)

    def resolve(self, hash_bytes: bytes, decoded_len: int, dep_domain: int) -> bytes:
        try:
            return self._entries[hash_bytes]
        except KeyError:
            raise DependencyMissing(
                "dependency %s is not present in the resolver" % hash_bytes.hex()
            )


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
