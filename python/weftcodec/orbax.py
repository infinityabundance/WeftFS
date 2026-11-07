"""JAX / Orbax checkpoint adapter over WeftFS.

``weftcodec.orbax`` provides an Orbax ``CheckpointHandler`` that stores a JAX
PyTree as one canonical ``weft.checkpoint.v1`` checkpoint through the shipped
Rust CLI. It reuses the storage plumbing of :mod:`weftcodec.dcp` and does not
introduce a wire format: leaves are written into a safetensors staging
container, the Rust CLI ingests that container, and a canonical WCB record
(``weft.orbax.v1``) binds each flattened leaf path to its dtype, shape,
content root, and logical root.

The "no silent fallback" rules of the DCP adapter apply unchanged:

* a missing CLI, store, tensor, or metadata record is a typed error;
* every restored leaf is checked against the recorded BLAKE3 content root,
  dtype, shape, and the checkpoint manifest;
* a target PyTree with a missing, extra, or differently typed leaf fails
  closed instead of guessing;
* staging is bounded (``staging_bytes``) and never silently exceeded.

Orbax and JAX are optional. Importing :mod:`weftcodec.orbax` without them works
and exposes the same classes/methods (contract mode); operations that need
JAX raise :class:`JaxRequired`.

Not claimed: GPU/TPU I/O, multi-process or multi-host JAX, a full ``orbax``
replacement, TensorStore/OCDBT compatibility, or a zero-copy path. The adapter
copies each leaf through host memory, one leaf at a time, bounded by
``staging_bytes``.
"""

from __future__ import annotations

import dataclasses
import importlib
import os
import shutil
import struct
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from . import wcb
from .dcp import (
    DEFAULT_CLI,
    DEFAULT_STAGING_BYTES,
    DEFAULT_TIMEOUT_SECONDS,
    DcpError,
    ExactnessError,
    InvalidCheckpointId,
    InvalidMetadata,
    MetadataMissing,
    SaveFailed,
    StagingLimitExceeded,
    StoreCommandError,
    StoreUnavailable,
    AsyncSaveHandle,
    _Storage,
    _generation,
    _parse_safetensors,
    _parse_topology,
    _validate_checkpoint_id,
    init_store,
    verify_checkpoint,
)
from .hashing import content_root
from .service import CheckpointService, ServiceError
from .dcp import _default_cli

__all__ = [
    "DEFAULT_STAGING_BYTES",
    "JAX_AVAILABLE",
    "JAX_VERSION",
    "ORBAX_AVAILABLE",
    "ORBAX_METADATA_NAME",
    "ORBAX_METADATA_SCHEMA",
    "ORBAX_POINTER_NAME",
    "ORBAX_VERSION",
    "OrbaxAdapterError",
    "OrbaxLeaf",
    "OrbaxMetadata",
    "OrbaxSaveResult",
    "DuplicateLeafPath",
    "JaxRequired",
    "MismatchedLeaf",
    "MissingLeaf",
    "UnexpectedLeaf",
    "UnsupportedLeaf",
    "UnsupportedDtype",
    "WeftOrbaxHandler",
    "WeftSaveArgs",
    "WeftRestoreArgs",
    "flatten_pytree",
    "leaf_paths",
    "metadata_of",
    "restore_pytree",
    "save_async_pytree",
    "save_pytree",
    "verify_checkpoint",
]

ORBAX_METADATA_SCHEMA = "weft.orbax.v1"
ORBAX_POINTER_SCHEMA = "weft.orbax.pointer.v1"
ORBAX_METADATA_NAME = "orbax.metadata"
ORBAX_POINTER_NAME = "weft.orbax.wcb"


# ---------------------------------------------------------------------------
# Optional JAX / Orbax. Importing weftcodec never imports these modules.


def _load_jax():
    try:
        return importlib.import_module("jax")
    except Exception:  # ImportError or a broken optional install
        return None


_JAX = _load_jax()
JAX_AVAILABLE = _JAX is not None
JAX_VERSION = getattr(_JAX, "__version__", None) if _JAX is not None else None

_NP = None
_ML_DTYPES = None
if JAX_AVAILABLE:  # pragma: no branch - both paths are exercised by the court
    try:
        import numpy as _NP  # noqa: F401
        import ml_dtypes as _ML_DTYPES  # noqa: F401
        _OPTIONAL_IMPORT_ERROR = None
    except Exception as exc:  # pragma: no cover - defensive
        _JAX = None
        JAX_AVAILABLE = False
        JAX_VERSION = None
        _OPTIONAL_IMPORT_ERROR = str(exc)
else:
    _OPTIONAL_IMPORT_ERROR = "jax is not installed"


def _load_orbax():
    try:
        return importlib.import_module("orbax.checkpoint")
    except Exception:
        return None


_ORBAX = _load_orbax()
ORBAX_AVAILABLE = _ORBAX is not None
ORBAX_VERSION = getattr(_ORBAX, "__version__", None) if _ORBAX is not None else None

if ORBAX_AVAILABLE:
    try:
        from orbax.checkpoint import AsyncCheckpointHandler as _AsyncCheckpointHandler
        from orbax.checkpoint import CheckpointHandler as _CheckpointHandler
        from orbax.checkpoint import args as _orbax_args

        _ORBAX_IMPORT_ERROR = None
    except Exception as exc:  # pragma: no cover - defensive
        ORBAX_AVAILABLE = False
        ORBAX_VERSION = None
        _ORBAX = None
        _ORBAX_IMPORT_ERROR = str(exc)
else:  # pragma: no cover - the court always has orbax
    _ORBAX_IMPORT_ERROR = "orbax.checkpoint is not installed"

if not ORBAX_AVAILABLE:

    class _CheckpointHandler:  # type: ignore[no-redef]
        """Contract-mode stand-in when Orbax is absent."""

    class _AsyncCheckpointHandler(_CheckpointHandler):  # type: ignore[no-redef]
        """Contract-mode stand-in when Orbax is absent."""

    class _CheckpointArgs:  # type: ignore[no-redef]
        """Contract-mode stand-in for ``orbax.checkpoint.args.CheckpointArgs``."""

else:
    _CheckpointArgs = _orbax_args.CheckpointArgs


def _require_jax(operation):
    if not JAX_AVAILABLE:
        raise JaxRequired(
            "%s requires JAX (install with `pip install jax[cpu] orbax-checkpoint`); "
            "weftcodec.orbax imported in contract mode: %s"
            % (operation, _OPTIONAL_IMPORT_ERROR)
        )


def _orbax_version():
    return ORBAX_VERSION or "unavailable"


# ---------------------------------------------------------------------------
# Typed errors


class OrbaxAdapterError(DcpError):
    """Base class for Orbax-adapter-specific failures."""


class JaxRequired(OrbaxAdapterError):
    """This operation needs JAX but it is not installed."""


class UnsupportedLeaf(OrbaxAdapterError):
    """A PyTree leaf is not an array with an exact WEFT-TENSOR v1 dtype."""


class UnsupportedDtype(OrbaxAdapterError):
    """A leaf dtype has no exact safetensors/WEFT-TENSOR v1 mapping."""


class DuplicateLeafPath(OrbaxAdapterError):
    """Two flattened leaves produced the same canonical path."""


class MissingLeaf(OrbaxAdapterError):
    """A stored leaf is absent from the restore target PyTree."""


class UnexpectedLeaf(OrbaxAdapterError):
    """The restore target has a leaf that is not in the checkpoint."""


class MismatchedLeaf(OrbaxAdapterError):
    """A target leaf dtype or shape disagrees with the stored leaf."""


# ---------------------------------------------------------------------------
# Canonical leaf identity and WCB records


def _u(value):
    return wcb.U64(int(value))


def _i(value):
    if isinstance(value, wcb.U64):
        return int(value.value)
    if isinstance(value, wcb.I64):
        return int(value.value)
    return int(value)


def _u_list(values):
    return [_u(value) for value in values]


def _i_list(values):
    return tuple(_i(value) for value in values)


#: canonical WEFT-TENSOR dtype -> (safetensors spelling, element bytes, numpy
#: dtype factory name). Only these dtypes have an exact Rust mapping.
_DTYPE_TABLE = {
    "f16": ("F16", 2, "float16"),
    "bf16": ("BF16", 2, "bfloat16"),
    "f32": ("F32", 4, "float32"),
    "f64": ("F64", 8, "float64"),
    "i8": ("I8", 1, "int8"),
    "u8": ("U8", 1, "uint8"),
    "i16": ("I16", 2, "int16"),
    "i32": ("I32", 4, "int32"),
    "i64": ("I64", 8, "int64"),
}

#: JAX/numpy dtype spelling -> canonical WEFT-TENSOR dtype.
_CANONICAL_BY_NAME = {
    "float16": "f16",
    "bfloat16": "bf16",
    "float32": "f32",
    "float64": "f64",
    "int8": "i8",
    "uint8": "u8",
    "int16": "i16",
    "int32": "i32",
    "int64": "i64",
}


def _dtype_name(dtype) -> str:
    """Normalise a JAX/numpy dtype to its short spelling."""
    return str(dtype)


def _canonical_dtype(dtype) -> str:
    name = _dtype_name(dtype)
    try:
        return _CANONICAL_BY_NAME[name]
    except KeyError:
        raise UnsupportedDtype(
            "dtype %s has no exact WEFT-TENSOR/safetensors v1 mapping; "
            "supported: %s"
            % (name, ", ".join(sorted(_CANONICAL_BY_NAME)))
        )


def _safetensors_dtype(canonical: str) -> str:
    try:
        return _DTYPE_TABLE[canonical][0]
    except KeyError:
        raise UnsupportedDtype("dtype %s has no safetensors mapping" % canonical)


def _element_bytes(canonical: str) -> int:
    try:
        return _DTYPE_TABLE[canonical][1]
    except KeyError:
        raise UnsupportedDtype("dtype %s has no element size" % canonical)


def _numpy_dtype(canonical: str):
    _require_jax("materialising a restored leaf")
    try:
        name = _DTYPE_TABLE[canonical][2]
    except KeyError:
        raise UnsupportedDtype("dtype %s has no numpy mapping" % canonical)
    if name == "bfloat16":
        if _ML_DTYPES is None:  # pragma: no cover - jax always brings ml_dtypes
            raise UnsupportedDtype("bfloat16 needs ml_dtypes")
        return _NP.dtype(_ML_DTYPES.bfloat16)
    return _NP.dtype(name)


def _canonical_path(path_tuple) -> str:
    try:
        text = _JAX.tree_util.keystr(path_tuple)
    except Exception:  # pragma: no cover - defensive for exotic key types
        text = "".join(repr(key) for key in path_tuple)
    return text or "<root>"


def _path_types(path_tuple):
    return [type(key).__name__ for key in path_tuple]


def _shape_product(shape) -> int:
    product = 1
    for extent in shape:
        extent = int(extent)
        if extent < 0:
            raise OrbaxAdapterError("negative leaf extent %d" % extent)
        product *= extent
    return product


@dataclasses.dataclass(frozen=True)
class OrbaxLeaf:
    """One flattened leaf as recorded in ``weft.orbax.v1``.

    ``element_bytes`` is the total tensor byte length, matching the manifest's
    ``element_bytes`` field (not the per-element width).
    """

    entry: str
    path: str
    path_types: tuple
    dtype: str
    shape: tuple
    element_bytes: int
    content_root: bytes
    logical_root: bytes

    def to_value(self):
        return {
            "entry": self.entry,
            "path": self.path,
            "path_types": list(self.path_types),
            "dtype": self.dtype,
            "shape": _u_list(self.shape),
            "element_bytes": _u(self.element_bytes),
            "content_root": self.content_root,
            "logical_root": self.logical_root,
        }

    @classmethod
    def from_value(cls, value):
        try:
            shape = _i_list(value["shape"])
            element_bytes = _i(value["element_bytes"])
            content = bytes(value["content_root"])
            logical = bytes(value["logical_root"])
            dtype = value["dtype"]
            entry = value["entry"]
            path = value["path"]
            path_types = tuple(str(item) for item in value["path_types"])
        except (KeyError, TypeError, ValueError) as exc:
            raise InvalidMetadata("bad orbax leaf record: %s" % exc) from exc
        if dtype not in _DTYPE_TABLE:
            raise InvalidMetadata("orbax leaf %r has unknown dtype %r" % (path, dtype))
        expected = _shape_product(shape) * _element_bytes(dtype)
        if element_bytes != expected:
            raise InvalidMetadata(
                "orbax leaf %r element_bytes %d != %d (shape * width) for dtype %s"
                % (path, element_bytes, expected, dtype)
            )
        if len(content) != 32 or len(logical) != 32:
            raise InvalidMetadata("orbax leaf %r roots must be 32 bytes" % path)
        if not entry or not path:
            raise InvalidMetadata("orbax leaf entry/path must be non-empty")
        return cls(
            entry=entry,
            path=path,
            path_types=path_types,
            dtype=dtype,
            shape=shape,
            element_bytes=element_bytes,
            content_root=content,
            logical_root=logical,
        )


@dataclasses.dataclass(frozen=True)
class OrbaxMetadata:
    """Decoded ``weft.orbax.v1`` metadata record."""

    checkpoint_id: str
    storage_name: str
    leaves: tuple
    write_topology: tuple
    shard_strategy: str
    staging_bytes: int
    source_file_bytes: int
    source_sha256: str
    jax_version: str
    orbax_version: str

    @property
    def leaf_count(self):
        return len(self.leaves)

    @property
    def total_element_bytes(self):
        return sum(leaf.element_bytes for leaf in self.leaves)

    def by_path(self):
        return {leaf.path: leaf for leaf in self.leaves}

    def by_entry(self):
        return {leaf.entry: leaf for leaf in self.leaves}

    def to_value(self):
        return {
            "schema": ORBAX_METADATA_SCHEMA,
            "checkpoint_id": self.checkpoint_id,
            "storage_name": self.storage_name,
            "leaf_count": _u(self.leaf_count),
            "leaves": [leaf.to_value() for leaf in self.leaves],
            "write_topology": _u_list(self.write_topology),
            "shard_strategy": self.shard_strategy,
            "staging_bytes": _u(self.staging_bytes),
            "source_file_bytes": _u(self.source_file_bytes),
            "source_sha256": self.source_sha256,
            "jax_version": self.jax_version,
            "orbax_version": self.orbax_version,
        }


def serialize_metadata(metadata: OrbaxMetadata) -> bytes:
    return wcb.encode(metadata.to_value())


_METADATA_KEYS = {
    "schema",
    "checkpoint_id",
    "storage_name",
    "leaf_count",
    "leaves",
    "write_topology",
    "shard_strategy",
    "staging_bytes",
    "source_file_bytes",
    "source_sha256",
    "jax_version",
    "orbax_version",
}


def metadata_from_value(value) -> OrbaxMetadata:
    if not isinstance(value, dict) or value.get("schema") != ORBAX_METADATA_SCHEMA:
        raise InvalidMetadata(
            "orbax metadata schema is %r, expected %r"
            % (
                value.get("schema") if isinstance(value, dict) else None,
                ORBAX_METADATA_SCHEMA,
            )
        )
    if set(value) != _METADATA_KEYS:
        raise InvalidMetadata(
            "orbax metadata keys %r != the exact schema key set"
            % sorted(value)
        )
    leaves = tuple(OrbaxLeaf.from_value(row) for row in value["leaves"])
    if _i(value["leaf_count"]) != len(leaves):
        raise InvalidMetadata(
            "orbax metadata leaf_count %d != %d leaf records"
            % (_i(value["leaf_count"]), len(leaves))
        )
    if len({leaf.entry for leaf in leaves}) != len(leaves):
        raise InvalidMetadata("orbax metadata has duplicate entry names")
    if len({leaf.path for leaf in leaves}) != len(leaves):
        raise InvalidMetadata("orbax metadata has duplicate leaf paths")
    topology = _i_list(value["write_topology"])
    if len(topology) != 2:
        raise InvalidMetadata("write_topology must be [shards, axis]")
    shards, axis = topology
    if shards < 1 or axis < 0:
        raise InvalidMetadata("invalid write_topology %r" % (topology,))
    source_sha = value["source_sha256"]
    if not isinstance(source_sha, str) or len(source_sha) != 64:
        raise InvalidMetadata("source_sha256 must be 64 hex characters")
    try:
        bytes.fromhex(source_sha)
    except ValueError as exc:
        raise InvalidMetadata("source_sha256 is not hex: %s" % exc) from exc
    for key in ("checkpoint_id", "storage_name", "shard_strategy"):
        if not isinstance(value[key], str) or not value[key]:
            raise InvalidMetadata("%s must be a non-empty string" % key)
    return OrbaxMetadata(
        checkpoint_id=value["checkpoint_id"],
        storage_name=value["storage_name"],
        leaves=leaves,
        write_topology=(int(shards), int(axis)),
        shard_strategy=value["shard_strategy"],
        staging_bytes=_i(value["staging_bytes"]),
        source_file_bytes=_i(value["source_file_bytes"]),
        source_sha256=source_sha,
        jax_version=str(value["jax_version"]),
        orbax_version=str(value["orbax_version"]),
    )


def serialize_pointer(*, store_root, checkpoint_id, storage_name, metadata_bytes):
    value = {
        "schema": ORBAX_POINTER_SCHEMA,
        "store_root": os.path.abspath(os.fspath(store_root)),
        "checkpoint_id": checkpoint_id,
        "storage_name": storage_name,
        "metadata_root": content_root(metadata_bytes),
    }
    return wcb.encode(value)


_POINTER_KEYS = {
    "schema",
    "store_root",
    "checkpoint_id",
    "storage_name",
    "metadata_root",
}


def pointer_from_value(value) -> dict:
    if not isinstance(value, dict) or value.get("schema") != ORBAX_POINTER_SCHEMA:
        raise InvalidMetadata(
            "orbax pointer schema is %r, expected %r"
            % (
                value.get("schema") if isinstance(value, dict) else None,
                ORBAX_POINTER_SCHEMA,
            )
        )
    if set(value) != _POINTER_KEYS:
        raise InvalidMetadata(
            "orbax pointer keys %r != the exact schema key set" % sorted(value)
        )
    root = bytes(value["metadata_root"])
    if len(root) != 32:
        raise InvalidMetadata("orbax pointer metadata_root must be 32 bytes")
    for key in ("store_root", "checkpoint_id", "storage_name"):
        if not isinstance(value[key], str) or not value[key]:
            raise InvalidMetadata("orbax pointer %s must be a non-empty string" % key)
    return {
        "store_root": value["store_root"],
        "checkpoint_id": value["checkpoint_id"],
        "storage_name": value["storage_name"],
        "metadata_root": root,
    }


# ---------------------------------------------------------------------------
# Orbax argument dataclasses (registered with the real handler when available)


@dataclasses.dataclass
class WeftSaveArgs(_CheckpointArgs):
    """Arguments for one WeftFS/Orbax save.

    ``checkpoint_id`` is the checkpoint name inside the WeftFS store (a single
    store name, not a filesystem path). The Orbax directory passed to the
    handler is a locator: it records the store path and the exact canonical
    metadata record.
    """

    item: object = None
    store_root: object = None
    checkpoint_id: object = None
    write_topology: object = (1, 0)
    shard_strategy: str = "zstd"
    staging_bytes: object = None
    overwrite: bool = True
    init_store: bool = False


@dataclasses.dataclass
class WeftRestoreArgs(_CheckpointArgs):
    """Arguments for one WeftFS/Orbax restore.

    ``item`` is the target PyTree (concrete arrays or ``jax.ShapeDtypeStruct``).
    ``restore_topology`` is an optional Rust-side proof layout
    (``n`` or ``(n, axis)``); it is independent of the JAX target sharding.
    """

    item: object = None
    store_root: object = None
    checkpoint_id: object = None
    storage_name: object = None
    restore_topology: object = None
    partial_restore: bool = False
    staging_bytes: object = None


# Registration happens after the handler class is defined (below).


# ---------------------------------------------------------------------------
# Result objects


@dataclasses.dataclass(frozen=True)
class OrbaxSaveResult:
    """What one save published."""

    checkpoint_id: str
    storage_name: str
    store_root: str
    directory: str
    leaves: tuple
    write_topology: tuple
    shard_strategy: str
    staging_bytes: int
    staging_peak_bytes: int
    checkpoint_write_ms: float
    store_command_ms: float
    source_file_bytes: int
    source_sha256: str
    manifest: dict
    metadata: OrbaxMetadata

    @property
    def leaf_count(self):
        return len(self.leaves)

    @property
    def leaf_bytes(self):
        return sum(leaf.element_bytes for leaf in self.leaves)

    def stats(self):
        return {
            "checkpoint_id": self.checkpoint_id,
            "storage_name": self.storage_name,
            "store_root": self.store_root,
            "directory": self.directory,
            "leaf_count": self.leaf_count,
            "leaf_bytes": self.leaf_bytes,
            "staging_bytes": self.staging_bytes,
            "staging_peak_bytes": self.staging_peak_bytes,
            "checkpoint_write_ms": self.checkpoint_write_ms,
            "store_command_ms": self.store_command_ms,
            "source_file_bytes": self.source_file_bytes,
            "source_sha256": self.source_sha256,
            "write_topology": self.write_topology,
            "shard_strategy": self.shard_strategy,
        }


# ---------------------------------------------------------------------------
# Leaf materialisation / target validation


def _array_bytes(leaf):
    _require_jax("serialising a PyTree leaf")
    array = _NP.asarray(leaf)
    if not array.flags.c_contiguous:
        array = _NP.ascontiguousarray(array)
    if array.dtype.byteorder == ">":
        array = array.byteswap().view(array.dtype.newbyteorder("<"))
    return array.tobytes(order="C")


def _leaf_dtype_shape(leaf):
    _require_jax("inspecting a PyTree leaf")
    if isinstance(leaf, _JAX.ShapeDtypeStruct):
        return _dtype_name(leaf.dtype), tuple(int(x) for x in leaf.shape), leaf
    if isinstance(leaf, _JAX.Array):
        return _dtype_name(leaf.dtype), tuple(int(x) for x in leaf.shape), leaf
    if _NP is not None and isinstance(leaf, _NP.ndarray):
        return _dtype_name(leaf.dtype), tuple(int(x) for x in leaf.shape), leaf
    raise UnsupportedLeaf(
        "PyTree leaf %r is %s; only jax.Array, numpy.ndarray, and "
        "jax.ShapeDtypeStruct are supported"
        % (leaf, type(leaf).__name__)
    )


def _target_sharding(leaf):
    return getattr(leaf, "sharding", None)


def _to_target(buffer, *, canonical, shape, target):
    """Materialise one restored leaf from its caller-owned memfd buffer.

    ``buffer`` is the service's :class:`weftcodec.service.TensorBuffer`; the
    dtype/shape were already verified against the metadata, so the mapping is
    used directly. For a JAX destination the host array is handed to
    ``device_put`` with no extra host copy; for a numpy destination the caller
    owns the result, so exactly one bounded copy is made.
    """
    _require_jax("materialising a restored leaf")
    np_dtype = _numpy_dtype(canonical)
    array = _NP.frombuffer(buffer.view, dtype=np_dtype).reshape(tuple(shape))
    if _NP is not None and isinstance(target, _NP.ndarray):
        return _NP.array(array, copy=True, order="C")
    jnp = _JAX.numpy
    value = jnp.asarray(array)
    sharding = _target_sharding(target)
    if sharding is not None:
        value = _JAX.device_put(value, sharding)
    return value


# ---------------------------------------------------------------------------
# Async plumbing


class _LazyFuture:
    """A bounded-worker future that starts work on first ``result()``.

    Orbax's ``AsyncCheckpointer`` creates its temporary directory as one commit
    operation and then waits on the handler's futures. Writing into that
    directory before the creation operation completes is a real race (the
    creation helper removes and recreates it). Starting the save only when
    ``result()`` is called means the directory is ready, while the caller's
    ``save()`` still returns immediately.
    """

    def __init__(self, executor, function):
        self._executor = executor
        self._function = function
        self._lock = threading.Lock()
        self._future = None
        self._cancelled = False

    def result(self, timeout=None):
        with self._lock:
            if self._cancelled:
                raise RuntimeError("orbax save future was cancelled")
            if self._future is None:
                self._future = self._executor.submit(self._function)
            future = self._future
        return future.result(timeout=timeout)

    def cancel(self):
        with self._lock:
            if self._future is not None:
                return self._future.cancel()
            self._cancelled = True
            return True

    def done(self):
        with self._lock:
            future = self._future
        return future.done() if future is not None else self._cancelled


# ---------------------------------------------------------------------------
# Handler


class WeftOrbaxHandler(_AsyncCheckpointHandler):
    """Orbax ``AsyncCheckpointHandler`` that publishes canonical WeftFS checkpoints.

    ``save``/``restore`` are also usable directly. The real Orbax
    ``Checkpointer``, ``AsyncCheckpointer``, and (when the installed Orbax
    supports it) ``CheckpointManager`` can drive this handler through
    :class:`WeftSaveArgs` / :class:`WeftRestoreArgs`.

    ``staging_bytes`` bounds the total leaf bytes of one staging container and
    the total restored bytes materialised for one restore. The executor is
    bounded to ``max_workers`` (1..4); each worker uses the same hard bound.
    """

    def __init__(
        self,
        *,
        staging_bytes=DEFAULT_STAGING_BYTES,
        max_workers=1,
        weftfs_cli=None,
        timeout=DEFAULT_TIMEOUT_SECONDS,
    ):
        staging_bytes = int(staging_bytes)
        if staging_bytes <= 0:
            raise ValueError("staging_bytes must be a positive byte count")
        workers = int(max_workers)
        if workers < 1 or workers > 4:
            raise ValueError("max_workers must be between 1 and 4")
        self.staging_bytes = staging_bytes
        self.weftfs_cli = weftfs_cli
        self.timeout = float(timeout)
        self._executor = ThreadPoolExecutor(
            max_workers=workers, thread_name_prefix="weft-orbax-save"
        )
        self._service_cli = _default_cli(weftfs_cli)
        self._service = None
        self._service_root = None
        self._lock = threading.RLock()
        self._last_result = None
        self._stats = {
            "saves": 0,
            "restores": 0,
            "leaves_restored": 0,
            "restored_bytes": 0,
        }

    # -- properties ---------------------------------------------------------

    def _get_service(self, store_root):
        """One helper process for every operation of this handler."""
        root = os.path.abspath(os.fspath(store_root))
        if self._service is None or self._service_root != root:
            if self._service is not None:
                self._service.close()
            self._service = CheckpointService(
                root, cli=self._service_cli, timeout=self.timeout
            )
            self._service_root = root
        return self._service

    @property
    def stats(self):
        with self._lock:
            payload = dict(self._stats)
        result = self.last_result
        if result is not None:
            payload["last_save"] = result.stats()
        payload["staging_bytes"] = self.staging_bytes
        return payload

    @property
    def last_result(self):
        with self._lock:
            return self._last_result

    @classmethod
    def typestr(cls):
        return "%s.%s" % (cls.__module__, cls.__qualname__)

    # -- Orbax interface ----------------------------------------------------

    def save(self, directory, *args, **kwargs):
        save_args = _extract_args(args, kwargs, WeftSaveArgs, "save")
        return self._save_sync(Path(os.fspath(directory)), save_args)

    async def async_save(self, directory, *args, **kwargs):
        save_args = _extract_args(args, kwargs, WeftSaveArgs, "async_save")
        if save_args.item is None:
            raise SaveFailed("WeftSaveArgs.item is required")
        return [
            _LazyFuture(
                self._executor,
                lambda: self._save_sync(Path(os.fspath(directory)), save_args),
            )
        ]

    def restore(self, directory, *args, **kwargs):
        restore_args = _extract_args(args, kwargs, WeftRestoreArgs, "restore")
        return self._restore_sync(Path(os.fspath(directory)), restore_args)

    def metadata(self, directory):
        return read_metadata(Path(os.fspath(directory)))[2]

    def finalize(self, directory):
        # The store publish and the locator write are already durable; Orbax
        # owns the atomic directory rename/commit of the locator.
        return None

    def close(self):
        if self._service is not None:
            self._service.close()
            self._service = None
            self._service_root = None
        self._executor.shutdown(wait=True, cancel_futures=False)

    @staticmethod
    def _translate(exc):
        return _translate_service_error(exc)

    # -- save ---------------------------------------------------------------

    def _save_sync(self, directory, save_args):
        _require_jax("writing a PyTree checkpoint")
        if save_args.item is None:
            raise SaveFailed("WeftSaveArgs.item is required")
        store_root = _resolve_save_store_root(save_args.store_root)
        checkpoint_id = _validate_checkpoint_id(save_args.checkpoint_id)
        shards, axis = _parse_topology(save_args.write_topology)
        if save_args.shard_strategy not in ("zstd", "raw", "tensor-sharded"):
            raise ValueError(
                "shard_strategy must be zstd, raw, or tensor-sharded "
                "(tensor-tiled needs a tile shape this adapter does not expose)"
            )
        staging_bound = (
            int(save_args.staging_bytes)
            if save_args.staging_bytes is not None
            else self.staging_bytes
        )
        if staging_bound <= 0:
            raise ValueError("staging_bytes must be a positive byte count")

        flat = _flatten(save_args.item)
        if not flat:
            raise SaveFailed("refusing to save an empty PyTree")
        entries = _entry_names(len(flat))
        leaves = []
        total = 0
        for index, (path_tuple, leaf) in enumerate(flat):
            path = _canonical_path(path_tuple)
            dtype_name, shape, _source = _leaf_dtype_shape(leaf)
            canonical = _canonical_dtype(dtype_name)
            nbytes = _shape_product(shape) * _element_bytes(canonical)
            total += nbytes
            if total > staging_bound:
                raise StagingLimitExceeded(
                    "PyTree leaves need %d staged bytes, above the %d "
                    "staging_bytes bound" % (total, staging_bound)
                )
            leaves.append(
                {
                    "entry": entries[index],
                    "path": path,
                    "path_types": tuple(_path_types(path_tuple)),
                    "dtype": canonical,
                    "shape": tuple(int(x) for x in shape),
                    "element_bytes": nbytes,
                    "nbytes": nbytes,
                }
            )
        if len({leaf["path"] for leaf in leaves}) != len(leaves):
            raise DuplicateLeafPath(
                "flattened PyTree produced duplicate canonical leaf paths"
            )

        directory = Path(directory)
        directory.mkdir(parents=True, exist_ok=True)
        pointer_path = directory / ORBAX_POINTER_NAME
        peak = 0
        storage_name = None
        store = _Storage(store_root, cli=self.weftfs_cli, timeout=self.timeout)
        if save_args.init_store:
            store.init()
        store.requires_store()
        service = self._get_service(store_root)
        try:
            header = {}
            cursor = 0
            for leaf in leaves:
                header[leaf["entry"]] = {
                    "dtype": _safetensors_dtype(leaf["dtype"]),
                    "shape": list(leaf["shape"]),
                    "data_offsets": [cursor, cursor + leaf["nbytes"]],
                }
                leaf["offset"] = cursor
                cursor += leaf["nbytes"]
            header_raw = _json_bytes(header)
            if len(header_raw) > 64 * 1024 * 1024:
                raise SaveFailed("staging safetensors header exceeds 64 MiB")
            # The container must exist as safetensors bytes (the checkpoint
            # manifest records its header and SHA-256), but it is staged in
            # memory only: no host file, no CLI argument, one memfd hand-off.
            container = bytearray(struct.pack("<Q", len(header_raw)))
            container += header_raw
            for leaf, (path_tuple, source_leaf) in zip(leaves, flat):
                payload = _array_bytes(source_leaf)
                if len(payload) != leaf["nbytes"]:
                    raise SaveFailed(
                        "leaf %r produced %d bytes, header declared %d"
                        % (leaf["path"], len(payload), leaf["nbytes"])
                    )
                peak = max(peak, len(payload))
                container += payload
                leaf["content_root"] = content_root(payload)

            storage_name = _allocate_storage_name(
                store, checkpoint_id, bool(save_args.overwrite)
            )
            started = time.monotonic()
            write_report = service.write_checkpoint(
                storage_name,
                container,
                shards=shards,
                axis=axis,
                strategy=save_args.shard_strategy,
            )
            checkpoint_write_ms = (time.monotonic() - started) * 1000.0
            manifest = write_report.manifest.to_value()
            show = {
                "manifest_root": write_report.manifest_root,
                "manifest": manifest,
            }
            _verify_manifest(show, leaves, storage_name)

            manifest_rows = {row["name"]: row for row in manifest.get("tensors", [])}
            recorded = []
            for index, leaf in enumerate(leaves):
                row = manifest_rows[leaf["entry"]]
                recorded.append(
                    OrbaxLeaf(
                        entry=leaf["entry"],
                        path=leaf["path"],
                        path_types=leaf["path_types"],
                        dtype=leaf["dtype"],
                        shape=leaf["shape"],
                        element_bytes=leaf["element_bytes"],
                        content_root=leaf["content_root"],
                        logical_root=bytes.fromhex(row["logical_root"]),
                    )
                )
            recorded.sort(key=lambda item: item.path)
            metadata = OrbaxMetadata(
                checkpoint_id=checkpoint_id,
                storage_name=storage_name,
                leaves=tuple(recorded),
                write_topology=(int(shards), int(axis)),
                shard_strategy=save_args.shard_strategy,
                staging_bytes=staging_bound,
                source_file_bytes=int(manifest.get("source_file_bytes", 0)),
                source_sha256=str(manifest.get("source_sha256", "")),
                jax_version=JAX_VERSION or "unavailable",
                orbax_version=_orbax_version(),
            )
            metadata_bytes = serialize_metadata(metadata)
            service.write_file(
                "/checkpoints/%s/%s" % (storage_name, ORBAX_METADATA_NAME),
                metadata_bytes,
            )
            pointer_bytes = serialize_pointer(
                store_root=store_root,
                checkpoint_id=checkpoint_id,
                storage_name=storage_name,
                metadata_bytes=metadata_bytes,
            )
            with open(pointer_path, "wb") as handle:
                handle.write(pointer_bytes)
                handle.flush()
                os.fsync(handle.fileno())
            _fsync_directory(directory)

            result = OrbaxSaveResult(
                checkpoint_id=checkpoint_id,
                storage_name=storage_name,
                store_root=os.fspath(store_root),
                directory=os.fspath(directory),
                leaves=tuple(recorded),
                write_topology=(int(shards), int(axis)),
                shard_strategy=save_args.shard_strategy,
                staging_bytes=staging_bound,
                staging_peak_bytes=peak,
                checkpoint_write_ms=checkpoint_write_ms,
                store_command_ms=store.command_ms,
                source_file_bytes=int(manifest.get("source_file_bytes", 0)),
                source_sha256=str(manifest.get("source_sha256", "")),
                manifest=manifest,
                metadata=metadata,
            )
            with self._lock:
                self._stats["saves"] += 1
                self._last_result = result
            return result
        finally:
            pass

    # -- restore ------------------------------------------------------------

    def _restore_sync(self, directory, restore_args):
        _require_jax("reading a PyTree checkpoint")
        target = restore_args.item
        if target is None:
            raise InvalidMetadata("WeftRestoreArgs.item is required")
        store, storage_name, metadata, show, _pointer = _resolve_metadata(
            directory,
            store_root=restore_args.store_root,
            checkpoint_id=restore_args.checkpoint_id,
            storage_name=restore_args.storage_name,
            weftfs_cli=self.weftfs_cli,
            timeout=self.timeout,
            service_factory=self._get_service,
        )
        _verify_metadata_manifest(metadata, show)
        staging_bound = (
            int(restore_args.staging_bytes)
            if restore_args.staging_bytes is not None
            else self.staging_bytes
        )
        restore_topology = restore_args.restore_topology
        shards, axis = (
            (None, 0)
            if restore_topology is None
            else _parse_topology(restore_topology)
        )

        flat = _flatten(target)
        target_paths = [_canonical_path(path_tuple) for path_tuple, _ in flat]
        if len(set(target_paths)) != len(target_paths):
            raise DuplicateLeafPath("restore target has duplicate canonical paths")
        stored = metadata.by_path()
        missing = [path for path in stored if path not in set(target_paths)]
        extra = [path for path in target_paths if path not in stored]
        if extra:
            raise UnexpectedLeaf(
                "restore target has leaves absent from checkpoint %r: %r"
                % (storage_name, sorted(extra))
            )
        if missing and not restore_args.partial_restore:
            raise MissingLeaf(
                "checkpoint %r has leaves absent from the restore target: %r"
                % (storage_name, sorted(missing))
            )
        if not target_paths:
            raise InvalidMetadata("restore target has no leaves")

        # Fail closed on a target dtype/shape mismatch before reading any data.
        for (path_tuple, leaf), path in zip(flat, target_paths):
            dtype_name, shape, _source = _leaf_dtype_shape(leaf)
            canonical = _canonical_dtype(dtype_name)
            leaf_meta = stored[path]
            if canonical != leaf_meta.dtype:
                raise MismatchedLeaf(
                    "target leaf %r dtype %s != stored dtype %s"
                    % (path, canonical, leaf_meta.dtype)
                )
            if tuple(int(x) for x in shape) != leaf_meta.shape:
                raise MismatchedLeaf(
                    "target leaf %r shape %r != stored shape %r"
                    % (path, tuple(shape), leaf_meta.shape)
                )

        wanted = [stored[path] for path in target_paths]
        total_wanted = sum(leaf.element_bytes for leaf in wanted)
        if total_wanted > staging_bound:
            raise StagingLimitExceeded(
                "restore needs %d bytes, above the %d staging_bytes bound"
                % (total_wanted, staging_bound)
            )

        all_entries = metadata.by_entry()
        full = len(wanted) == len(all_entries)
        buffers = {}
        try:
            for leaf in wanted:
                try:
                    buffer = store.restore_tensor(
                        storage_name,
                        leaf.entry,
                        topology=None if shards is None else (shards, axis),
                    )
                except ServiceError as exc:
                    raise self._translate(exc) from exc
                if tuple(buffer.shape) != leaf.shape:
                    buffer.close()
                    raise ExactnessError(
                        "leaf %r restored shape %r, expected %r"
                        % (leaf.path, tuple(buffer.shape), leaf.shape)
                    )
                actual = content_root(buffer.view)
                if actual != leaf.content_root:
                    buffer.close()
                    raise ExactnessError(
                        "leaf %r restored content root %s != recorded %s"
                        % (leaf.path, actual.hex(), leaf.content_root.hex())
                    )
                buffers[leaf.entry] = buffer
            _record_restore_stats(
                self, len(wanted), sum(buffer.size for buffer in buffers.values())
            )
            restored_leaves = []
            for (path_tuple, target_leaf), path in zip(flat, target_paths):
                leaf_meta = stored[path]
                restored_leaves.append(
                    _to_target(
                        buffers[leaf_meta.entry],
                        canonical=leaf_meta.dtype,
                        shape=leaf_meta.shape,
                        target=target_leaf,
                    )
                )
            return _JAX.tree_util.tree_unflatten(_treedef(target), restored_leaves)
        finally:
            for buffer in buffers.values():
                buffer.close()

    # -- extension: single-leaf restore -------------------------------------

    def restore_leaf(self, directory, path, *, target=None, **kwargs):
        """Restore exactly one recorded leaf by canonical path.

        This is an adapter extension, not part of the Orbax interface. It calls
        ``weftfs checkpoint restore --tensor ENTRY`` for the one leaf and
        verifies the recorded content root, dtype, and shape before returning.
        """
        _require_jax("restoring one leaf")
        restore_topology = kwargs.pop("restore_topology", None)
        axis_arg = kwargs.pop("axis", 0)
        store, storage_name, metadata, show, _pointer = _resolve_metadata(
            directory,
            weftfs_cli=self.weftfs_cli,
            timeout=self.timeout,
            service_factory=self._get_service,
            **kwargs,
        )
        _verify_metadata_manifest(metadata, show)
        stored = metadata.by_path()
        if path not in stored:
            raise MissingLeaf(
                "checkpoint %r has no leaf %r; paths: %r"
                % (storage_name, path, sorted(stored))
            )
        leaf = stored[path]
        if target is not None:
            dtype_name, shape, _source = _leaf_dtype_shape(target)
            if _canonical_dtype(dtype_name) != leaf.dtype or tuple(shape) != leaf.shape:
                raise MismatchedLeaf(
                    "target leaf %r type/shape disagrees with the stored leaf" % path
                )
        shards, axis = (
            (None, 0)
            if restore_topology is None
            else _parse_topology(restore_topology, axis_arg)
        )
        buffer = None
        try:
            try:
                buffer = store.restore_tensor(
                    storage_name,
                    leaf.entry,
                    topology=None if shards is None else (shards, axis),
                )
            except ServiceError as exc:
                raise self._translate(exc) from exc
            if tuple(buffer.shape) != leaf.shape:
                raise ExactnessError(
                    "leaf %r restored shape %r, expected %r"
                    % (path, tuple(buffer.shape), leaf.shape)
                )
            actual = content_root(buffer.view)
            if actual != leaf.content_root:
                raise ExactnessError(
                    "leaf %r restored content root %s != recorded %s"
                    % (path, actual.hex(), leaf.content_root.hex())
                )
            _record_restore_stats(self, 1, buffer.size)
            if target is None:
                # The returned numpy array owns its bytes; this is the one
                # explicit copy on the "no destination given" path.
                return (
                    _NP.frombuffer(buffer.view, dtype=_numpy_dtype(leaf.dtype))
                    .reshape(leaf.shape)
                    .copy()
                )
            return _to_target(
                buffer, canonical=leaf.dtype, shape=leaf.shape, target=target
            )
        finally:
            if buffer is not None:
                buffer.close()

    def leaf_paths(self, directory):
        metadata = metadata_of(Path(directory))
        return [leaf.path for leaf in metadata.leaves]


if ORBAX_AVAILABLE:  # pragma: no branch - registered only with a real Orbax
    _orbax_args.register_with_handler(WeftOrbaxHandler, for_save=True)(WeftSaveArgs)
    _orbax_args.register_with_handler(WeftOrbaxHandler, for_restore=True)(
        WeftRestoreArgs
    )


# ---------------------------------------------------------------------------
# Argument / environment helpers


def _extract_args(args, kwargs, expected, operation):
    value = kwargs.get("args")
    if value is None:
        for candidate in args:
            if isinstance(candidate, expected):
                value = candidate
                break
    if value is None:
        raise SaveFailed(
            "%s needs a %s instance (pass args=%s(...))"
            % (operation, expected.__name__, expected.__name__)
        )
    if not isinstance(value, expected):
        raise SaveFailed(
            "%s got args of type %s, expected %s"
            % (operation, type(value).__name__, expected.__name__)
        )
    return value


def _resolve_save_store_root(store_root):
    if store_root is None:
        store_root = os.environ.get("WEFTFS_STORE")
    if store_root is None:
        raise StoreUnavailable(
            "WeftSaveArgs.store_root is required (or set WEFTFS_STORE)"
        )
    return Path(os.fspath(store_root))


def _flatten(tree):
    _require_jax("flattening a PyTree")
    return _JAX.tree_util.tree_flatten_with_path(tree)[0]


def _treedef(tree):
    return _JAX.tree_util.tree_structure(tree)


def _entry_names(count):
    width = max(4, len(str(max(count - 1, 0))))
    return ["leaf_%0*d" % (width, index) for index in range(count)]


def _json_bytes(value):
    import json

    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def _fsync_directory(path):
    try:
        fd = os.open(os.fspath(path), os.O_RDONLY)
    except OSError:
        return
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _allocate_storage_name(store, checkpoint_id, overwrite):
    existing = set(store.list_checkpoints())
    if checkpoint_id not in existing:
        return checkpoint_id
    if not overwrite:
        raise FileExistsError(
            "checkpoint %r already exists and overwrite=False" % checkpoint_id
        )
    highest = 1
    for name in existing:
        generation = _generation(name, checkpoint_id)
        if generation is not None and generation > highest:
            highest = generation
    return "%s.v%d" % (checkpoint_id, highest + 1)


# ---------------------------------------------------------------------------
# Manifest / metadata validation


def _verify_manifest(show, leaves, storage_name):
    manifest = show.get("manifest")
    if not isinstance(manifest, dict):
        raise SaveFailed("checkpoint %r has no manifest object" % storage_name)
    if manifest.get("complete") is not True:
        raise SaveFailed("checkpoint %r is not complete" % storage_name)
    rows = manifest.get("tensors")
    if not isinstance(rows, list) or not rows:
        raise SaveFailed("checkpoint %r has no tensor list" % storage_name)
    by_name = {row["name"]: row for row in rows}
    expected = {leaf["entry"] for leaf in leaves}
    if set(by_name) != expected:
        raise SaveFailed(
            "checkpoint %r stored tensors %r, expected %r"
            % (storage_name, sorted(by_name), sorted(expected))
        )
    for leaf in leaves:
        row = by_name[leaf["entry"]]
        if row.get("dtype") != leaf["dtype"]:
            raise ExactnessError(
                "leaf %r dtype %r != canonical %r"
                % (leaf["path"], row.get("dtype"), leaf["dtype"])
            )
        if tuple(int(x) for x in row.get("shape", [])) != leaf["shape"]:
            raise ExactnessError(
                "leaf %r shape %r != %r"
                % (leaf["path"], row.get("shape"), leaf["shape"])
            )
        if int(row.get("element_bytes", -1)) != leaf["element_bytes"]:
            raise ExactnessError(
                "leaf %r element_bytes %r != %r"
                % (leaf["path"], row.get("element_bytes"), leaf["element_bytes"])
            )
        if row.get("content_root") != leaf["content_root"].hex():
            raise ExactnessError(
                "leaf %r content root %s != the bytes written (%s)"
                % (
                    leaf["path"],
                    row.get("content_root"),
                    leaf["content_root"].hex(),
                )
            )


def _verify_metadata_manifest(metadata, show):
    manifest = show.get("manifest")
    if not isinstance(manifest, dict) or manifest.get("complete") is not True:
        raise InvalidMetadata(
            "checkpoint %r manifest is missing or incomplete" % metadata.storage_name
        )
    rows = {row["name"]: row for row in manifest.get("tensors", [])}
    if set(rows) != {leaf.entry for leaf in metadata.leaves}:
        raise InvalidMetadata(
            "checkpoint %r manifest tensors %r != metadata entries %r"
            % (
                metadata.storage_name,
                sorted(rows),
                sorted(leaf.entry for leaf in metadata.leaves),
            )
        )
    for leaf in metadata.leaves:
        row = rows[leaf.entry]
        if row.get("dtype") != leaf.dtype:
            raise ExactnessError(
                "leaf %r manifest dtype %r != metadata %r"
                % (leaf.path, row.get("dtype"), leaf.dtype)
            )
        if tuple(int(x) for x in row.get("shape", [])) != leaf.shape:
            raise ExactnessError(
                "leaf %r manifest shape %r != metadata %r"
                % (leaf.path, row.get("shape"), leaf.shape)
            )
        if int(row.get("element_bytes", -1)) != leaf.element_bytes:
            raise ExactnessError(
                "leaf %r manifest element_bytes %r != metadata %r"
                % (leaf.path, row.get("element_bytes"), leaf.element_bytes)
            )
        if row.get("content_root") != leaf.content_root.hex():
            raise ExactnessError(
                "leaf %r manifest content root %s != metadata %s"
                % (leaf.path, row.get("content_root"), leaf.content_root.hex())
            )
        if row.get("logical_root") != leaf.logical_root.hex():
            raise ExactnessError(
                "leaf %r manifest logical root %s != metadata %s"
                % (leaf.path, row.get("logical_root"), leaf.logical_root.hex())
            )


def _check_restored_payloads(parsed, leaves, storage_name):
    for leaf in leaves:
        if leaf.entry not in parsed:
            raise InvalidMetadata(
                "restored container for %r has no entry %r (has %r)"
                % (storage_name, leaf.entry, sorted(parsed))
            )
        sf_dtype, shape, payload = parsed[leaf.entry]
        if sf_dtype != _safetensors_dtype(leaf.dtype):
            raise ExactnessError(
                "leaf %r restored dtype %s, expected %s"
                % (leaf.path, sf_dtype, _safetensors_dtype(leaf.dtype))
            )
        if tuple(int(x) for x in shape) != leaf.shape:
            raise ExactnessError(
                "leaf %r restored shape %r, expected %r"
                % (leaf.path, tuple(shape), leaf.shape)
            )
        actual = content_root(payload)
        if actual != leaf.content_root:
            raise ExactnessError(
                "leaf %r restored content root %s != recorded %s"
                % (leaf.path, actual.hex(), leaf.content_root.hex())
            )


def _record_restore_stats(handler, leaves, nbytes):
    with handler._lock:
        handler._stats["restores"] += 1
        handler._stats["leaves_restored"] += leaves
        handler._stats["restored_bytes"] += nbytes


# ---------------------------------------------------------------------------
# Locator / metadata resolution


def read_pointer(directory):
    path = Path(directory) / ORBAX_POINTER_NAME
    try:
        raw = path.read_bytes()
    except FileNotFoundError as exc:
        raise MetadataMissing(
            "directory %s has no %s locator" % (directory, ORBAX_POINTER_NAME)
        ) from exc
    try:
        value = wcb.decode(raw)
    except Exception as exc:
        raise InvalidMetadata(
            "orbax locator at %s is not canonical WCB: %s" % (path, exc)
        ) from exc
    return pointer_from_value(value)


def _resolve_metadata(
    directory,
    *,
    store_root=None,
    checkpoint_id=None,
    storage_name=None,
    weftfs_cli=None,
    timeout=DEFAULT_TIMEOUT_SECONDS,
    service=None,
    service_factory=None,
):
    directory = Path(os.fspath(directory))
    pointer = None
    pointer_path = directory / ORBAX_POINTER_NAME
    if pointer_path.is_file():
        pointer = read_pointer(directory)
    if store_root is None:
        if pointer is None:
            raise StoreUnavailable(
                "no %s in %s and no store_root given"
                % (ORBAX_POINTER_NAME, directory)
            )
        store_root = pointer["store_root"]
    store_root = Path(os.fspath(store_root))
    if pointer is not None:
        if os.path.abspath(os.fspath(store_root)) != os.path.abspath(
            pointer["store_root"]
        ):
            raise InvalidMetadata(
                "explicit store_root %s != locator store_root %s"
                % (store_root, pointer["store_root"])
            )
        if checkpoint_id is not None and checkpoint_id != pointer["checkpoint_id"]:
            raise InvalidMetadata(
                "explicit checkpoint_id %r != locator checkpoint_id %r"
                % (checkpoint_id, pointer["checkpoint_id"])
            )
        if storage_name is not None and storage_name != pointer["storage_name"]:
            raise InvalidMetadata(
                "explicit storage_name %r != locator storage_name %r"
                % (storage_name, pointer["storage_name"])
            )
        checkpoint_id = pointer["checkpoint_id"]
        storage_name = pointer["storage_name"]
    if service is None:
        if service_factory is not None:
            service = service_factory(store_root)
        else:
            service = CheckpointService(store_root, cli=weftfs_cli, timeout=timeout)
    store = service
    # Refresh before reading so a commit made by another process (the CLI oracle
    # in the adversarial tests, for example) is visible on this long-lived
    # service. The refresh reopens the read handle; it is not a write.
    store.refresh()
    if storage_name is None:
        if checkpoint_id is None:
            raise InvalidMetadata(
                "restore needs checkpoint_id or storage_name when the directory "
                "has no locator"
            )
        storage_name = _latest_storage_name(store, checkpoint_id)
    elif checkpoint_id is not None:
        if _generation(storage_name, checkpoint_id) is None:
            raise InvalidMetadata(
                "storage_name %r is not a generation of checkpoint_id %r"
                % (storage_name, checkpoint_id)
            )
    metadata_path = "/checkpoints/%s/%s" % (storage_name, ORBAX_METADATA_NAME)
    exists, _size = store.stat_file(metadata_path)
    if not exists:
        raise MetadataMissing(
            "checkpoint %r has no %s; was it written by the Orbax adapter?"
            % (metadata_path, ORBAX_METADATA_NAME)
        )
    with store.read_file(metadata_path) as view:
        blob = view.bytes
    if pointer is not None and content_root(blob) != pointer["metadata_root"]:
        raise InvalidMetadata(
            "store metadata root %s != locator metadata_root %s"
            % (content_root(blob).hex(), pointer["metadata_root"].hex())
        )
    try:
        value = wcb.decode(blob)
    except Exception as exc:
        raise InvalidMetadata(
            "store metadata at %s is not canonical WCB: %s" % (metadata_path, exc)
        ) from exc
    metadata = metadata_from_value(value)
    if metadata.storage_name != storage_name:
        raise InvalidMetadata(
            "store metadata names storage %r but resolved %r"
            % (metadata.storage_name, storage_name)
        )
    if checkpoint_id is not None and metadata.checkpoint_id != checkpoint_id:
        raise InvalidMetadata(
            "store metadata names checkpoint %r but resolved %r"
            % (metadata.checkpoint_id, checkpoint_id)
        )
    show = _show_from_manifest(store.manifest(storage_name))
    return store, storage_name, metadata, show, pointer


def _show_from_manifest(descriptor):
    return {
        "manifest_root": descriptor.manifest_root,
        "manifest": descriptor.to_value(),
    }


def _translate_service_error(exc):
    """Map a typed service error onto this adapter's error hierarchy."""
    message = str(exc)
    class_name = getattr(exc, "class_name", "")
    if class_name == "NotFound":
        return MetadataMissing(message)
    if class_name in ("Corrupt", "Malformed", "NonCanonical", "DependencyMismatch"):
        return ExactnessError(message)
    if class_name in ("LimitExceeded", "BudgetExceeded"):
        return StagingLimitExceeded(message)
    return SaveFailed(message)


def _latest_storage_name(store, checkpoint_id):
    names = set(store.list_checkpoints())
    highest = 1
    chosen = None
    for name in names:
        generation = _generation(name, checkpoint_id)
        if generation is not None and generation >= highest:
            highest = generation
            chosen = name
    if chosen is None:
        raise MetadataMissing("checkpoint %r does not exist in the store" % checkpoint_id)
    return chosen


def read_metadata(directory, **kwargs):
    """Read and validate the canonical metadata for an Orbax directory."""
    return _resolve_metadata(Path(directory), **kwargs)


# ---------------------------------------------------------------------------
# Convenience wrappers


def save_pytree(item, *, directory, store_root, checkpoint_id, **kwargs):
    """Synchronous save without constructing a handler by hand."""
    handler = WeftOrbaxHandler(
        staging_bytes=kwargs.pop("handler_staging_bytes", DEFAULT_STAGING_BYTES),
        max_workers=kwargs.pop("max_workers", 1),
        weftfs_cli=kwargs.pop("weftfs_cli", None),
        timeout=kwargs.pop("timeout", DEFAULT_TIMEOUT_SECONDS),
    )
    try:
        args = WeftSaveArgs(
            item=item,
            store_root=store_root,
            checkpoint_id=checkpoint_id,
            **kwargs,
        )
        return handler.save(directory, args)
    finally:
        handler.close()


def save_async_pytree(item, *, directory, store_root, checkpoint_id, **kwargs):
    """Run a save on one bounded worker; returns a DCP-style async handle."""
    handler = WeftOrbaxHandler(
        staging_bytes=kwargs.pop("handler_staging_bytes", DEFAULT_STAGING_BYTES),
        max_workers=kwargs.pop("max_workers", 1),
        weftfs_cli=kwargs.pop("weftfs_cli", None),
        timeout=kwargs.pop("timeout", DEFAULT_TIMEOUT_SECONDS),
    )
    executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="weft-orbax-async")
    args = WeftSaveArgs(
        item=item,
        store_root=store_root,
        checkpoint_id=checkpoint_id,
        **kwargs,
    )
    future = executor.submit(handler.save, Path(os.fspath(directory)), args)
    future.add_done_callback(lambda _future: handler.close())
    return AsyncSaveHandle(future, handler, executor)


def restore_pytree(directory, item, **kwargs):
    """Synchronous restore without constructing a handler by hand."""
    handler = WeftOrbaxHandler(
        staging_bytes=kwargs.pop("handler_staging_bytes", DEFAULT_STAGING_BYTES),
        weftfs_cli=kwargs.pop("weftfs_cli", None),
        timeout=kwargs.pop("timeout", DEFAULT_TIMEOUT_SECONDS),
    )
    try:
        return handler.restore(directory, WeftRestoreArgs(item=item, **kwargs))
    finally:
        handler.close()


def leaf_paths(item):
    """Canonical paths of each leaf of a PyTree, in flatten order."""
    return [_canonical_path(path_tuple) for path_tuple, _ in _flatten(item)]


def flatten_pytree(item):
    """``[(canonical_path, leaf)]`` in JAX flatten order."""
    return [(_canonical_path(path_tuple), leaf) for path_tuple, leaf in _flatten(item)]


def metadata_of(directory, **kwargs):
    """Return the decoded :class:`OrbaxMetadata` for a directory."""
    resolved = read_metadata(Path(directory), **kwargs)
    service = resolved[0]
    try:
        return resolved[2]
    finally:
        close = getattr(service, "close", None)
        if callable(close):
            close()
