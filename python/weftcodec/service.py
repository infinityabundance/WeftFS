"""Persistent, in-process WeftFS checkpoint service client.

One ``CheckpointService`` owns exactly one ``weftfs checkpoint-service``
process for its whole lifetime. Every call is a bounded, length-prefixed frame
over an ``AF_UNIX`` socket; tensor payloads never travel through the frame —
they cross the process boundary as ``SCM_RIGHTS`` memfd descriptors.

Why a socket rather than stdin/stdout: a caller-owned restore buffer (and the
staged container handed to a write) must be transferred without a copy through
the helper's address space, and ``SCM_RIGHTS`` file descriptors are only
expressible on a socket. The protocol, bounds, and failure semantics are
normative in ``docs/CHECKPOINT-SERVICE.md``.

The service never starts a subprocess per operation, never writes a staging
file for a restore, and a restore never mutates the store (the Rust side opens
one read-only handle and the write path is a separate, explicit operation).

This module has no dependency on torch, JAX, or numpy at import time; numpy is
imported only when a caller asks for an array view.
"""

from __future__ import annotations

import array
import hashlib
import mmap
import os
import socket
import struct
import subprocess
import threading
import time

PROTOCOL_VERSION = 3
MAX_FRAME_BYTES = 1 << 20
DEFAULT_MAX_TENSOR_BYTES = 4 << 30
MAX_FILE_BYTES = 16 << 20
DEFAULT_TIMEOUT_SECONDS = 1800.0
DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"

OP_INFO = 0x01
OP_LIST_CHECKPOINTS = 0x02
OP_CHECKPOINT_MANIFEST = 0x03
OP_RESTORE_TENSOR = 0x04
OP_WRITE_CHECKPOINT = 0x05
OP_VERIFY_CHECKPOINT = 0x06
OP_REFRESH = 0x07
OP_WRITE_FILE = 0x08
OP_STAT_FILE = 0x09
OP_READ_FILE = 0x0A
OP_PROFILE = 0x0B
OP_PREPARE_CHECKPOINT = 0x0C
OP_FINISH_CHECKPOINT = 0x0D
OP_ABORT_CHECKPOINT = 0x0E
OP_SHUTDOWN = 0x7F

STATUS_OK = 0
STATUS_ERROR = 1

_FRAME_HEADER = struct.Struct("<I")  # u32 length


# A measurement hook. ``None`` unless a court calls :func:`set_probe_sink`, so
# the shipped path pays one ``is not None`` test per instrumented section.
_PROBE_SINK = None


def set_probe_sink(sink):
    """Install (or clear, with ``None``) the client-side probe callback.

    The callback receives ``(name, duration_ns)``; it is called once per
    measured client section. It is a measurement aid only: nothing measured
    here is persisted, hashed, or included in any published artifact.
    """
    global _PROBE_SINK
    _PROBE_SINK = sink


def _probe(name, started_ns):
    sink = _PROBE_SINK
    if sink is not None:
        sink(name, time.perf_counter_ns() - started_ns)


class ServiceError(RuntimeError):
    """A typed error class returned by the service."""

    def __init__(self, class_name, message, *, request_id=None):
        super().__init__("%s: %s" % (class_name, message))
        self.class_name = class_name
        self.plain_message = message
        self.request_id = request_id


class ServiceUnavailable(RuntimeError):
    """The helper process could not be started or has exited."""


class ServiceProtocolError(RuntimeError):
    """A framing violation; the connection is no longer usable."""


# ---------------------------------------------------------------------------
# Small encoders (the Rust side is the normative decoder)


def _u32(value):
    return struct.pack("<I", int(value))


def _u64(value):
    return struct.pack("<Q", int(value))


def _string(text):
    data = text.encode("utf-8") if isinstance(text, str) else bytes(text)
    return _u32(len(data)) + data


class _Reader:
    __slots__ = ("data", "at")

    def __init__(self, data):
        self.data = data
        self.at = 0

    def take(self, count):
        if self.at + count > len(self.data):
            raise ServiceProtocolError("truncated response body")
        chunk = self.data[self.at : self.at + count]
        self.at += count
        return chunk

    def u8(self):
        return self.take(1)[0]

    def u32(self):
        return struct.unpack("<I", self.take(4))[0]

    def u64(self):
        return struct.unpack("<Q", self.take(8))[0]

    def string(self):
        return self.take(self.u32()).decode("utf-8")

    def hash_hex(self):
        return self.take(32).hex()

    def rest(self):
        return self.data[self.at :]


# ---------------------------------------------------------------------------
# Result records


class ShardRow:
    __slots__ = (
        "axis",
        "shards",
        "index",
        "object_name",
        "object_id",
        "content_root",
        "logical_root",
        "byte_len",
    )

    def __init__(self, reader):
        self.axis = reader.u32()
        self.shards = reader.u32()
        self.index = reader.u32()
        self.object_name = reader.string()
        self.object_id = reader.hash_hex()
        self.content_root = reader.hash_hex()
        self.logical_root = reader.hash_hex()
        self.byte_len = reader.u64()

    def to_value(self):
        return {
            "axis": self.axis,
            "shards": self.shards,
            "index": self.index,
            "object_name": self.object_name,
            "object_id": self.object_id,
            "content_root": self.content_root,
            "logical_root": self.logical_root,
            "byte_len": self.byte_len,
        }


class TensorRow:
    __slots__ = (
        "name",
        "dtype",
        "shape",
        "logical_root",
        "content_root",
        "element_bytes",
        "source_offset",
        "shards",
    )

    def __init__(self, reader):
        self.name = reader.string()
        self.dtype = reader.string()
        self.shape = tuple(reader.u64() for _ in range(reader.u32()))
        self.logical_root = reader.hash_hex()
        self.content_root = reader.hash_hex()
        self.element_bytes = reader.u64()
        self.source_offset = reader.u64()
        self.shards = tuple(ShardRow(reader) for _ in range(reader.u32()))

    def to_value(self):
        return {
            "name": self.name,
            "dtype": self.dtype,
            "shape": list(self.shape),
            "logical_root": self.logical_root,
            "content_root": self.content_root,
            "element_bytes": self.element_bytes,
            "source_offset": self.source_offset,
            "shards": [shard.to_value() for shard in self.shards],
        }


class Manifest:
    __slots__ = (
        "manifest_root",
        "name",
        "container",
        "complete",
        "element_bytes",
        "source_file_bytes",
        "source_sha256",
        "tensors",
    )

    def __init__(self, data):
        reader = _Reader(data)
        self.manifest_root = reader.hash_hex()
        self.name = reader.string()
        self.container = reader.string()
        self.complete = bool(reader.u8())
        self.element_bytes = reader.u64()
        self.source_file_bytes = reader.u64() if reader.u8() else None
        self.source_sha256 = reader.hash_hex() if reader.u8() else None
        self.tensors = tuple(TensorRow(reader) for _ in range(reader.u32()))
        if reader.at != len(reader.data):
            raise ServiceProtocolError("manifest has trailing bytes")

    def by_name(self):
        return {row.name: row for row in self.tensors}

    def to_value(self):
        return {
            "manifest_root": self.manifest_root,
            "name": self.name,
            "container": self.container,
            "complete": self.complete,
            "element_bytes": self.element_bytes,
            "source_file_bytes": self.source_file_bytes,
            "source_sha256": self.source_sha256,
            "tensors": [row.to_value() for row in self.tensors],
        }


class WriteReport:
    __slots__ = (
        "checkpoint_id",
        "head",
        "manifest_root",
        "source_sha256",
        "source_file_bytes",
        "element_bytes",
        "tensor_count",
        "shard_objects",
        "complete",
        "elapsed_us",
        "manifest",
    )

    def __init__(self, data):
        reader = _Reader(data)
        self.checkpoint_id = reader.hash_hex()
        self.head = reader.hash_hex()
        self.manifest_root = reader.hash_hex()
        self.source_sha256 = reader.hash_hex()
        self.source_file_bytes = reader.u64()
        self.element_bytes = reader.u64()
        self.tensor_count = reader.u32()
        self.shard_objects = reader.u32()
        self.complete = bool(reader.u8())
        self.elapsed_us = reader.u64()
        length = reader.u32()
        self.manifest = Manifest(reader.take(length))
        if reader.at != len(reader.data):
            raise ServiceProtocolError("write response has trailing bytes")

    def to_value(self):
        return {
            "checkpoint_id": self.checkpoint_id,
            "head": self.head,
            "manifest_root": self.manifest_root,
            "source_sha256": self.source_sha256,
            "source_file_bytes": self.source_file_bytes,
            "element_bytes": self.element_bytes,
            "tensor_count": self.tensor_count,
            "shard_objects": self.shard_objects,
            "complete": self.complete,
            "elapsed_us": self.elapsed_us,
            "manifest": self.manifest.to_value(),
        }


_NUMPY_DTYPES = {
    "f16": "float16",
    "bf16": None,  # numpy has no native bfloat16 without ml_dtypes
    "f32": "float32",
    "f64": "float64",
    "i8": "int8",
    "u8": "uint8",
    "i16": "int16",
    "i32": "int32",
    "i64": "int64",
}

_ELEMENT_BYTES = {
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


def canonical_element_bytes(dtype):
    try:
        return _ELEMENT_BYTES[dtype]
    except KeyError:
        raise ServiceError(
            "UnsupportedVersion", "dtype %r has no canonical element size" % (dtype,)
        )


class TensorBuffer:
    """The exact restored bytes of one tensor, in a caller-owned memfd.

    ``array`` is a numpy view over the caller's mapping (created here because
    the caller asked for one), ``view`` is a writable ``memoryview``, and
    ``bytes`` performs an explicit copy. The service wrote directly into this
    buffer: there is no second copy between decode and the array.
    """

    __slots__ = (
        "_mm",
        "_fd",
        "size",
        "dtype",
        "shape",
        "full_shape",
        "content_root",
        "full_logical_root",
        "topology_root",
        "full_root_verified",
        "shard_roots_verified",
        "shards_read",
        "units_touched",
        "store_bytes_read",
        "store_reads",
        "elapsed_us",
    )

    def __init__(self, data):
        reader = _Reader(data)
        self.dtype = reader.string()
        self.shape = tuple(reader.u64() for _ in range(reader.u32()))
        self.size = reader.u64()
        self.content_root = reader.hash_hex()
        self.full_logical_root = reader.hash_hex()
        self.full_root_verified = bool(reader.u8())
        self.shard_roots_verified = bool(reader.u8())
        self.topology_root = reader.hash_hex() if reader.u8() else None
        self.shards_read = reader.u32()
        self.units_touched = reader.u64()
        self.store_bytes_read = reader.u64()
        self.store_reads = reader.u64()
        self.elapsed_us = reader.u64()
        if reader.at != len(reader.data):
            raise ServiceProtocolError("restore response has trailing bytes")
        self._mm = None
        self._fd = None

    def _attach(self, fd):
        self._fd = fd
        if self.size:
            self._mm = mmap.mmap(fd, self.size, access=mmap.ACCESS_WRITE)
        else:
            self._mm = mmap.mmap(-1, 1)

    @property
    def view(self):
        return memoryview(self._mm)[: self.size]

    @property
    def array(self):
        import numpy as np

        name = _NUMPY_DTYPES.get(self.dtype)
        if name is None:
            raise ServiceError(
                "UnsupportedVersion",
                "dtype %r has no native numpy dtype; use .view or .copy_into"
                % (self.dtype,),
            )
        dtype = np.dtype(name)
        count = self.size // dtype.itemsize
        if count == 0:
            return np.empty(self.shape, dtype=dtype)
        return np.frombuffer(self._mm, dtype=dtype, count=count).reshape(self.shape)

    @property
    def bytes(self):
        return bytes(self.view)

    def sha256(self):
        return hashlib.sha256(self.view).digest()

    def copy_into(self, destination):
        """Explicit bounded copy into a caller-supplied writable buffer."""
        target = memoryview(destination)
        if target.nbytes < self.size:
            raise ValueError(
                "destination holds %d bytes, tensor needs %d"
                % (target.nbytes, self.size)
            )
        target[: self.size] = self.view
        return destination

    def close(self):
        if self._mm is not None:
            mapping = self._mm
            try:
                mapping.close()
                self._mm = None
            except (BufferError, ValueError):
                # An exported view (for example a torch tensor) still points
                # into the mapping. Dropping our only reference would be a
                # use-after-free hazard, so keep the mapping alive.
                self._mm = mapping
        if self._fd is not None:
            try:
                os.close(self._fd)
            except OSError:
                pass
            self._fd = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False


# ---------------------------------------------------------------------------
# Client


class CheckpointService:
    """One helper process, reused across every checkpoint operation."""

    def __init__(
        self,
        store_root,
        *,
        cli=None,
        timeout=DEFAULT_TIMEOUT_SECONDS,
        max_frame_bytes=MAX_FRAME_BYTES,
        max_tensor_bytes=DEFAULT_MAX_TENSOR_BYTES,
        extra_args=(),
        session_idle_secs=None,
    ):
        self.store_root = os.fspath(store_root)
        self.cli = os.fspath(cli) if cli is not None else os.fspath(DEFAULT_CLI)
        self.timeout = float(timeout)
        self.max_frame_bytes = int(max_frame_bytes)
        self.max_tensor_bytes = int(max_tensor_bytes)
        self.extra_args = tuple(str(arg) for arg in extra_args)
        if session_idle_secs is not None:
            if int(session_idle_secs) < 0:
                raise ValueError("session_idle_secs must be non-negative")
            self.extra_args += ("--session-idle-secs", str(int(session_idle_secs)))
        self._lock = threading.Lock()
        self._request_id = 0
        self._closed = False
        self._socket = None
        self._process = None
        # A descriptor table is not shared across `fork()`, so a child must
        # never send on the parent's socket (and must never SHUTDOWN it).
        self._owner_pid = os.getpid()
        self.stats = {
            "requests": 0,
            "restores": 0,
            "writes": 0,
            "prepares": 0,
            "restored_bytes": 0,
            "written_bytes": 0,
            "request_ms": 0.0,
        }
        self._start()

    # -- lifecycle ----------------------------------------------------------

    def _start(self):
        probe_started = time.perf_counter_ns()
        parent, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        child.set_inheritable(True)
        command = [
            self.cli,
            "--store",
            self.store_root,
            "checkpoint-service",
            "--fd",
            str(child.fileno()),
            "--max-frame-bytes",
            str(self.max_frame_bytes),
            "--max-tensor-bytes",
            str(self.max_tensor_bytes),
            *self.extra_args,
        ]
        try:
            self._process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                pass_fds=(child.fileno(),),
                close_fds=True,
            )
        except FileNotFoundError as exc:
            parent.close()
            child.close()
            raise ServiceUnavailable(
                "weftfs CLI not executable at %s: %s" % (self.cli, exc)
            ) from exc
        child.close()
        self._socket = parent
        self._socket.settimeout(self.timeout)
        info = self.info()
        if info["protocol"] != PROTOCOL_VERSION:
            self.close()
            raise ServiceProtocolError(
                "service speaks protocol %r, client speaks %d"
                % (info["protocol"], PROTOCOL_VERSION)
            )
        _probe("client.service_start", probe_started)

    def close(self):
        if os.getpid() != self._owner_pid:
            # The child inherited the attributes but not the descriptor table
            # contract; shutting the socket down would kill the parent's
            # service. Drop the reference only.
            self._closed = True
            self._socket = None
            self._process = None
            return
        with self._lock:
            if self._closed:
                return
            self._closed = True
            if self._socket is not None:
                try:
                    frame = self._encode(OP_SHUTDOWN, 0, b"")
                    self._socket.sendall(frame)
                    self._socket.settimeout(2.0)
                    self._socket.recv(1 << 16)
                except OSError:
                    pass
                try:
                    self._socket.close()
                except OSError:
                    pass
                self._socket = None
        if self._process is not None:
            try:
                self._process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._process.kill()
                self._process.wait()
            self._process = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def __del__(self):
        try:
            self.close()
        except Exception:
            pass

    @property
    def pid(self):
        return None if self._process is None else self._process.pid

    # -- framing ------------------------------------------------------------

    def _encode(self, op, request_id, body, status=0):
        length = 6 + len(body)
        if length + 4 > self.max_frame_bytes + 4:
            raise ServiceError(
                "LimitExceeded",
                "request body is %d bytes, above the frame bound" % (len(body),),
            )
        return _FRAME_HEADER.pack(length) + bytes((op,)) + _u32(request_id) + bytes((status,)) + body

    def _call(self, op, body=b"", fd=None):
        if os.getpid() != self._owner_pid:
            raise ServiceUnavailable(
                "checkpoint service handle was inherited across fork(); "
                "open a session in the child instead"
            )
        with self._lock:
            if self._closed or self._socket is None:
                raise ServiceUnavailable("checkpoint service is closed")
            self._request_id += 1
            request_id = self._request_id & 0xFFFFFFFF
            frame = self._encode(op, request_id, body)
            started = time.monotonic()
            ancillary = []
            if fd is not None:
                ancillary = [
                    (socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [int(fd)]))
                ]
            try:
                self._socket.sendmsg([frame], ancillary)
                response = self._recv_frame()
            except OSError as exc:
                self._raise_process_failure(exc)
            elapsed_ms = (time.monotonic() - started) * 1000.0
            self.stats["requests"] += 1
            self.stats["request_ms"] += elapsed_ms
            if response[0] != request_id:
                raise ServiceProtocolError(
                    "response id %r does not match request %r"
                    % (response[0], request_id)
                )
            if response[1] != STATUS_OK:
                reader = _Reader(response[2])
                class_name = reader.string()
                message = reader.string()
                raise ServiceError(class_name, message, request_id=request_id)
            return response[2]

    def _raise_process_failure(self, exc):
        detail = ""
        if self._process is not None:
            try:
                _, stderr = self._process.communicate(timeout=2)
                detail = (stderr or b"").decode("utf-8", "replace").strip()
            except Exception:
                detail = ""
        raise ServiceUnavailable(
            "checkpoint service connection failed (%s); process=%r stderr=%r"
            % (exc, None if self._process is None else self._process.pid, detail)
        ) from exc

    def _recv_frame(self):
        header = self._recv_exact(4, allow_fds=True)
        if header is None:
            raise ServiceProtocolError("service closed the connection")
        (length,) = _FRAME_HEADER.unpack(header)
        if length < 6 or length > self.max_frame_bytes:
            raise ServiceProtocolError("service sent implausible frame length %d" % length)
        payload = self._recv_exact(length)
        if payload is None:
            raise ServiceProtocolError("service closed mid-frame")
        op = payload[0]
        request_id = struct.unpack("<I", payload[1:5])[0]
        status = payload[5]
        if status not in (STATUS_OK, STATUS_ERROR):
            raise ServiceProtocolError("unknown response status %d" % status)
        body = payload[6:]
        if status == STATUS_OK and op == OP_RESTORE_TENSOR:
            # The restore response carries no descriptor: the caller supplied
            # the buffer. The protocol leaves the fd space unused here.
            pass
        return request_id, status, body

    def _recv_exact(self, count, *, allow_fds=False):
        buffer = bytearray()
        first = True
        fds = []
        while len(buffer) < count:
            budget = socket.CMSG_SPACE(4 * 4)
            data, ancillary, flags, _address = self._socket.recvmsg(
                count - len(buffer), budget if (allow_fds and first) else 0
            )
            if not data:
                return None
            buffer.extend(data)
            if first and allow_fds:
                for level, kind, payload in ancillary:
                    if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                        values = array.array("i")
                        values.frombytes(payload[: len(payload) - (len(payload) % 4)])
                        for fd in values:
                            try:
                                os.close(fd)
                            except OSError:
                                pass
                if flags & socket.MSG_CTRUNC:
                    raise ServiceProtocolError("ancillary data was truncated")
            first = False
        return bytes(buffer)

    # -- operations ---------------------------------------------------------

    def info(self):
        body = self._call(OP_INFO)
        reader = _Reader(body)
        return {
            "protocol": reader.u32(),
            "max_frame_bytes": reader.u32(),
            "max_tensor_bytes": reader.u64(),
            "generation": reader.u64(),
            "head": reader.hash_hex(),
            "store_root": reader.string(),
            "elapsed_us": reader.u64(),
        }

    def list_checkpoints(self):
        body = self._call(OP_LIST_CHECKPOINTS)
        reader = _Reader(body)
        return [reader.string() for _ in range(reader.u32())]

    def manifest(self, name):
        return Manifest(self._call(OP_CHECKPOINT_MANIFEST, _string(name)))

    def verify(self, name):
        body = self._call(OP_VERIFY_CHECKPOINT, _string(name))
        return _Reader(body).u32()

    def refresh(self):
        body = self._call(OP_REFRESH)
        reader = _Reader(body)
        return {"generation": reader.u64(), "head": reader.hash_hex()}

    def profile(self, mode):
        """Read the service's phase profile (diagnostic, protocol 3).

        ``mode`` 0 begins a measurement (clearing the collector), 1 returns the
        totals collected so far, 2 stops measuring. The result maps a phase
        name to ``{"count", "total_ns", "max_ns"}``. Nested phases are named by
        the caller: ``state_replay``/``plane_probe`` sit inside ``store_open``
        and ``index_build``/``index_load``/``index_attest`` inside
        ``index_segment``; they must not be added to their parent.
        """
        if mode not in (0, 1, 2):
            raise ValueError("profile mode must be 0, 1, or 2")
        body = self._call(OP_PROFILE, bytes((int(mode),)))
        reader = _Reader(body)
        out = {}
        for _ in range(reader.u32()):
            name = reader.string()
            out[name] = {
                "count": reader.u64(),
                "total_ns": reader.u64(),
                "max_ns": reader.u64(),
            }
        return out

    def _allocate_memfd(self, size, name="weft-tensor"):
        fd = os.memfd_create(name, os.MFD_CLOEXEC)
        if size:
            os.ftruncate(fd, size)
        return fd

    def restore_tensor(self, name, tensor, *, region=None, topology=None, expected_size=None):
        """Restore one tensor straight into a caller-owned memfd.

        ``region`` is ``(start, end)`` in full-tensor coordinates; ``topology``
        is ``(shards, axis)`` and re-lays-out the bytes before verifying the
        same full logical root. ``expected_size`` skips the manifest round trip
        when the caller already knows the payload length (the adapters do).
        The returned :class:`TensorBuffer` owns the mapping; call ``close()``
        (or use it as a context manager).
        """
        row = None
        if region is None and expected_size is not None:
            size = int(expected_size)
        else:
            manifest = self.manifest(name)
            row = manifest.by_name().get(tensor)
            if row is None:
                raise ServiceError(
                    "NotFound", "checkpoint %r has no tensor %r" % (name, tensor)
                )
            if region is not None:
                start, end = region
                if len(start) != len(row.shape) or len(end) != len(row.shape):
                    raise ValueError("region rank does not match the tensor rank")
                extent = tuple(int(b) - int(a) for a, b in zip(start, end))
            else:
                extent = row.shape
            elements = 1
            for dim in extent:
                elements *= int(dim)
            size = elements * canonical_element_bytes(row.dtype)
        if size > self.max_tensor_bytes:
            raise ServiceError(
                "LimitExceeded",
                "restore of %d bytes exceeds max_tensor_bytes (%d)"
                % (size, self.max_tensor_bytes),
            )
        fd = self._allocate_memfd(size, "weft-restore")
        try:
            body = bytearray()
            body += _string(name)
            body += _string(tensor)
            body += bytes((1,)) if region is not None else bytes((0,))
            if region is not None:
                body += bytes((len(region[0]),))
                for value in region[0]:
                    body += _u64(value)
                for value in region[1]:
                    body += _u64(value)
            body += bytes((1,)) if topology is not None else bytes((0,))
            if topology is not None:
                body += _u32(topology[0])
                body += _u32(topology[1])
            response = self._call(OP_RESTORE_TENSOR, bytes(body), fd=fd)
            result = TensorBuffer(response)
            if result.size != size:
                raise ServiceProtocolError(
                    "service restored %d bytes, expected %d" % (result.size, size)
                )
            result._attach(fd)
            fd = None
            self.stats["restores"] += 1
            self.stats["restored_bytes"] += size
            return result
        finally:
            if fd is not None:
                try:
                    os.close(fd)
                except OSError:
                    pass

    def write_checkpoint(
        self,
        name,
        container,
        *,
        shards=1,
        axis=0,
        strategy="zstd",
        tile_shape=None,
        write_policy=None,
        fast_codec=None,
        fast_zstd_level=None,
        critical_path=False,
        deadline_ms=None,
        defer_commit=False,
    ):
        """Publish one checkpoint from a caller-supplied safetensors container.

        ``write_policy`` is ``planned``, ``fast``, or ``auto``. ``None`` reads
        ``$WEFT_WRITE_POLICY`` (default ``planned``), so a training job can set
        the critical-path policy for every save without changing its code.

        ``defer_commit`` publishes every immutable blob and applies the state
        ops in memory but writes **no** commit; the caller then calls
        :meth:`finish_checkpoint` (one atomic publication that can also carry a
        metadata blob) or :meth:`abort_checkpoint`. Only one deferred checkpoint
        may be open on a connection.
        """
        if write_policy is None:
            write_policy = os.environ.get("WEFT_WRITE_POLICY", "planned")
        if fast_codec is None:
            fast_codec = os.environ.get("WEFT_FAST_CODEC", "zstd")
        if fast_zstd_level is None:
            fast_zstd_level = int(os.environ.get("WEFT_FAST_ZSTD_LEVEL", "1"))
        policy_codes = {"planned": 0, "fast": 1, "auto": 2}
        if write_policy not in policy_codes:
            raise ValueError("write_policy must be planned, fast, or auto")
        codec_codes = {"raw": 0, "zstd": 1}
        if fast_codec not in codec_codes:
            raise ValueError("fast_codec must be raw or zstd")
        view = memoryview(container)
        started = time.perf_counter_ns()
        fd = self._allocate_memfd(len(view), "weft-container")
        try:
            offset = 0
            while offset < len(view):
                offset += os.write(fd, view[offset:])
            _probe("client.container_memfd", started)
            body = bytearray()
            body += _string(name)
            body += _u32(shards)
            body += _u32(axis)
            body += _string(strategy)
            shape = tuple(tile_shape or ())
            body += _u32(len(shape))
            for dim in shape:
                body += _u64(dim)
            body += bytes((policy_codes[write_policy],))
            body += bytes((codec_codes[fast_codec],))
            body += _u32(int(fast_zstd_level))
            flags = 1 if critical_path else 0
            body += bytes((flags,))
            if deadline_ms is None:
                body += bytes((0,))
            else:
                body += bytes((1,))
                body += _u64(int(deadline_ms))
            request_started = time.perf_counter_ns()
            opcode = OP_PREPARE_CHECKPOINT if defer_commit else OP_WRITE_CHECKPOINT
            response = self._call(opcode, bytes(body), fd=fd)
            _probe("client.write_request", request_started)
            report = WriteReport(response)
            self.stats["writes"] += 1
            self.stats["written_bytes"] += len(view)
            if defer_commit:
                self.stats["prepares"] += 1
            return report
        finally:
            try:
                os.close(fd)
            except OSError:
                pass

    def finish_checkpoint(self, name, metadata_path, payload):
        """Publish a prepared checkpoint and one metadata blob as one commit.

        ``name`` must be the checkpoint the open prepare returned and
        ``metadata_path`` must be under ``/checkpoints/<name>/``. Returns
        ``{"head", "generation", "bytes"}``. Exactly one commit is written; on
        any error nothing is published and the previous generation is intact.
        """
        if not metadata_path.startswith("/checkpoints/%s/" % name):
            raise ValueError("metadata path must be under /checkpoints/%s/" % name)
        view = memoryview(payload)
        if len(view) > MAX_FILE_BYTES:
            raise ServiceError(
                "LimitExceeded",
                "metadata payload is %d bytes, above the %d bound"
                % (len(view), MAX_FILE_BYTES),
            )
        fd = self._allocate_memfd(len(view), "weft-finish")
        try:
            offset = 0
            while offset < len(view):
                offset += os.write(fd, view[offset:])
            body = _string(name) + _string(metadata_path)
            request_started = time.perf_counter_ns()
            response = self._call(OP_FINISH_CHECKPOINT, body, fd=fd)
            _probe("client.metadata_request", request_started)
            reader = _Reader(response)
            result = {
                "head": reader.hash_hex(),
                "generation": reader.u64(),
                "bytes": reader.u64(),
                "backend_writes": reader.u64(),
                "backend_bytes": reader.u64(),
                "backend_fsyncs": reader.u64(),
            }
            self.stats["finishes"] = self.stats.get("finishes", 0) + 1
            return result
        finally:
            try:
                os.close(fd)
            except OSError:
                pass

    def abort_checkpoint(self):
        """Discard an open prepared checkpoint; nothing is published."""
        body = self._call(OP_ABORT_CHECKPOINT, b"")
        reader = _Reader(body)
        self.stats["aborts"] = self.stats.get("aborts", 0) + 1
        return {"generation": reader.u64()}

    def write_file(self, store_path, payload):
        """Publish one small file at an existing store path (metadata blobs)."""
        if not store_path.startswith("/"):
            raise ValueError("store path must be absolute")
        view = memoryview(payload)
        if len(view) > MAX_FILE_BYTES:
            raise ServiceError(
                "LimitExceeded",
                "write_file payload is %d bytes, above the %d bound"
                % (len(view), MAX_FILE_BYTES),
            )
        fd = self._allocate_memfd(len(view), "weft-file")
        try:
            offset = 0
            while offset < len(view):
                offset += os.write(fd, view[offset:])
            body = _string(store_path)
            request_started = time.perf_counter_ns()
            response = self._call(OP_WRITE_FILE, body, fd=fd)
            _probe("client.metadata_request", request_started)
            reader = _Reader(response)
            return {"bytes": reader.u64(), "generation": reader.u64()}
        finally:
            try:
                os.close(fd)
            except OSError:
                pass

    def stat_file(self, store_path):
        """Return ``(exists, size)`` for a store path, in process."""
        body = self._call(OP_STAT_FILE, _string(store_path))
        reader = _Reader(body)
        exists = bool(reader.u8())
        size = reader.u64()
        return exists, size

    def read_file(self, store_path):
        """Read one small store file into a caller-owned memfd.

        Returns a :class:`TensorBuffer`-shaped object whose ``bytes`` is the
        file content. The store is never mutated.
        """
        exists, size = self.stat_file(store_path)
        if not exists:
            raise ServiceError("NotFound", "no store file at %s" % (store_path,))
        if size > MAX_FILE_BYTES:
            raise ServiceError(
                "LimitExceeded",
                "%s is %d bytes, above the %d read bound"
                % (store_path, size, MAX_FILE_BYTES),
            )
        fd = self._allocate_memfd(size, "weft-read")
        try:
            response = self._call(OP_READ_FILE, _string(store_path), fd=fd)
            (actual,) = struct.unpack("<Q", response)
            if actual != size:
                raise ServiceProtocolError(
                    "read_file returned %d bytes, stat said %d" % (actual, size)
                )
            if size:
                mapping = mmap.mmap(fd, size, access=mmap.ACCESS_READ)
            else:
                mapping = mmap.mmap(-1, 1)
            return _FileView(mapping, size)
        finally:
            try:
                os.close(fd)
            except OSError:
                pass


class _FileView:
    """A memfd-backed, read-only snapshot of one small store file."""

    __slots__ = ("_mm", "size")

    def __init__(self, mapping, size):
        self._mm = mapping
        self.size = size

    @property
    def view(self):
        return memoryview(self._mm)[: self.size]

    @property
    def bytes(self):
        return bytes(self.view)

    def close(self):
        if self._mm is not None:
            mapping = self._mm
            try:
                mapping.close()
                self._mm = None
            except (BufferError, ValueError):
                self._mm = mapping

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def __del__(self):
        try:
            self.close()
        except Exception:
            pass
