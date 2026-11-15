"""Thin Python binding for the one explicit artifact API (T072).

This module is deliberately small: it shells out to the shipped
``weftfs artifact ...`` verbs and parses their JSON (or raw tensor bytes). It
contains no store logic of its own, so the DCP adapter, the Orbax adapter and
the agent-state consumer cannot drift from the Rust implementation.

The surface is exactly the documented one:

* :meth:`ArtifactClient.resolve` -- ordered entries at a pinned snapshot
* :meth:`ArtifactClient.plan` -- the ordered plan and its missing dependencies
* :meth:`ArtifactClient.materialize` / :meth:`ArtifactClient.materialize_tensor`
* :meth:`ArtifactClient.verify`
* :meth:`ArtifactClient.pin` / :meth:`ArtifactClient.release` -- lifetime
* :meth:`ArtifactClient.transaction` -- one group commit for every op inside

Snapshot scoping is the caller's: pass ``snapshot=`` to name a snapshot, or
accept the head and read ``provisional=True`` in the response. A named or
hashed snapshot that the store does not hold is a typed ``SnapshotMissing``
error; it is never substituted by the head.
"""

from __future__ import annotations

import json
import mmap
import os
import shutil
import subprocess
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

__all__ = [
    "ARTIFACT_API_SCHEMA",
    "ArtifactApiError",
    "ArtifactClient",
    "ArtifactTensorBuffer",
    "ArtifactTransaction",
    "artifact_ref",
]

ARTIFACT_API_SCHEMA = "weft.artifact.api.v1"
DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"
DEFAULT_TIMEOUT_SECONDS = 600.0

# Error classes the artifact API can return. Mirrors `src/error.rs`; kept here
# so a consumer can branch without parsing prose.
_ERROR_CLASSES = (
    "Ambiguous",
    "BudgetExceeded",
    "Busy",
    "Cancelled",
    "CapabilityRevoked",
    "Conflict",
    "Corrupt",
    "CrossDomain",
    "DependencyMismatch",
    "Exists",
    "InvalidArgument",
    "InvalidName",
    "Io",
    "IsDir",
    "LimitExceeded",
    "Malformed",
    "NamespaceMismatch",
    "NonCanonical",
    "NotDir",
    "NotEmpty",
    "NotFound",
    "Parse",
    "ReadOnly",
    "SnapshotMissing",
    "Unauthorized",
    "UnmeasuredCost",
    "UnknownSchema",
    "UnsupportedRepresentation",
    "UnsupportedVersion",
)


class ArtifactApiError(Exception):
    """A typed refusal from the artifact API.

    ``error_class`` is the normative class string; ``message`` is the
    human-readable remainder. The class is the contract.
    """

    def __init__(self, message, *, error_class="Malformed", command=None, stderr=""):
        super().__init__(message)
        self.message = message
        self.error_class = error_class
        self.command = command or []
        self.stderr = stderr

    def __str__(self):
        return "%s: %s" % (self.error_class, self.message)


def _parse_error_class(stderr: str) -> str:
    first = stderr.strip().splitlines()[0] if stderr.strip() else ""
    head, _, _rest = first.partition(": ")
    if head in _ERROR_CLASSES:
        return head
    return "Malformed"


def _default_cli(cli=None):
    if cli:
        return os.fspath(cli)
    env = os.environ.get("WEFTFS_CLI")
    if env:
        return env
    if os.path.isfile(DEFAULT_CLI) and os.access(DEFAULT_CLI, os.X_OK):
        return DEFAULT_CLI
    found = shutil.which("weftfs")
    if found:
        return found
    raise ArtifactApiError(
        "no weftfs CLI found: set WEFTFS_CLI or pass cli=... (looked at %s and PATH)"
        % DEFAULT_CLI,
        error_class="NotFound",
    )


def artifact_ref(*, root=None, checkpoint=None, tensor=None, object_id=None):
    """Build the one artifact descriptor every verb accepts."""
    chosen = [root is not None, checkpoint is not None, object_id is not None]
    if sum(1 for value in chosen if value) != 1:
        raise ValueError("exactly one of root=, checkpoint=, object_id= is required")
    if tensor is not None and checkpoint is None:
        raise ValueError("tensor= needs checkpoint=")
    if root is not None:
        return {"root": str(root)}
    if checkpoint is not None:
        payload = {"checkpoint": str(checkpoint)}
        if tensor is not None:
            payload["tensor"] = str(tensor)
        return payload
    return {"object_id": str(object_id)}


@dataclass
class _Entry:
    name: str
    kind: str
    content_root: str
    logical_root: str | None
    dtype: str | None
    shape: tuple
    bytes: int
    shards: int
    dependencies: tuple


class ArtifactTensorBuffer:
    """One restored tensor in a caller-owned memfd.

    The interface matches what the DCP and Orbax adapters already use
    (``view``, ``bytes``, ``size``, ``shape``, ``dtype``, ``content_root``,
    ``full_logical_root``, ``close``), so the same code path serves all three
    consumers.
    """

    __slots__ = (
        "_mm",
        "_fd",
        "size",
        "dtype",
        "shape",
        "content_root",
        "full_logical_root",
        "topology_root",
        "full_root_verified",
        "shard_roots_verified",
        "shards_read",
        "units_touched",
        "store_bytes_read",
        "store_reads",
        "plan_root",
        "snapshot",
        "provisional",
    )

    def __init__(self, fd, size, *, entry, plan_root, snapshot, provisional):
        self._fd = fd
        self.size = int(size)
        if self.size:
            self._mm = mmap.mmap(fd, self.size, access=mmap.ACCESS_WRITE)
        else:
            self._mm = mmap.mmap(-1, 1)
        self.dtype = entry.dtype
        self.shape = tuple(entry.shape)
        self.content_root = bytes.fromhex(entry.content_root)
        self.full_logical_root = (
            bytes.fromhex(entry.logical_root) if entry.logical_root else None
        )
        self.topology_root = None
        self.full_root_verified = True
        self.shard_roots_verified = True
        self.shards_read = 0
        self.units_touched = 0
        self.store_bytes_read = 0
        self.store_reads = 0
        self.plan_root = plan_root
        self.snapshot = snapshot
        self.provisional = provisional

    @property
    def view(self):
        return memoryview(self._mm)[: self.size]

    @property
    def bytes(self):
        return bytes(self.view)

    def close(self):
        # A consumer may legitimately keep an exported pointer into this
        # mapping: `_to_target` hands a numpy/JAX array built with
        # `frombuffer`/`asarray` straight to the caller and documents that no
        # extra host copy is made. Unmapping here would be a use-after-free
        # hazard, and `mmap.close()` refuses with BufferError while such a
        # pointer is alive -- which is the guard, not an error to report. Keep
        # the mapping referenced so it is released when the last exporter is
        # collected, exactly as `service.TensorBuffer.close` already does.
        if self._mm is not None:
            mapping = self._mm
            try:
                mapping.close()
                self._mm = None
            except (BufferError, ValueError):
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


class ArtifactClient:
    """Call the one artifact API. One instance per store root."""

    def __init__(self, store_root, *, cli=None, timeout=DEFAULT_TIMEOUT_SECONDS):
        self.store_root = Path(store_root)
        self.cli = _default_cli(cli)
        self.timeout = float(timeout)
        self.command_ms = 0.0
        self.calls = []
        self._entries = {}

    # -- plumbing -----------------------------------------------------------

    def _command(self, args):
        return [self.cli, "--store", os.fspath(self.store_root), *args]

    def _run(self, args, *, binary=False):
        command = self._command(args)
        started = time.monotonic()
        try:
            proc = subprocess.run(command, capture_output=True, timeout=self.timeout)
        except FileNotFoundError as exc:
            raise ArtifactApiError(
                "weftfs CLI not executable: %s" % exc, error_class="NotFound"
            ) from exc
        except subprocess.TimeoutExpired as exc:
            raise ArtifactApiError(
                "weftfs artifact call timed out after %.0fs" % self.timeout,
                error_class="Io",
                command=command,
            ) from exc
        self.command_ms += (time.monotonic() - started) * 1000.0
        if proc.returncode != 0:
            stderr = proc.stderr.decode("utf-8", "replace").strip()
            raise ArtifactApiError(
                stderr or "weftfs artifact call failed (%d)" % proc.returncode,
                error_class=_parse_error_class(stderr),
                command=command,
                stderr=stderr,
            )
        return proc

    def _json(self, verb, args, **payload):
        proc = self._run(["artifact", verb, *args, "--json"])
        self.calls.append(verb)
        text = proc.stdout.decode("utf-8")
        try:
            result = json.loads(text)
        except ValueError as exc:
            raise ArtifactApiError(
                "artifact %s produced invalid JSON: %s" % (verb, text[:400]),
                error_class="Malformed",
                command=self._command(["artifact", verb, *args, "--json"]),
            ) from exc
        if payload:
            result.update(payload)
        return result

    @staticmethod
    def _selector(payload, *, snapshot=None):
        args = []
        if "root" in payload:
            args += ["--root", payload["root"]]
        elif "checkpoint" in payload:
            args += ["--checkpoint", payload["checkpoint"]]
            if "tensor" in payload:
                args += ["--tensor", payload["tensor"]]
        else:
            args += ["--object-id", payload["object_id"]]
        if snapshot is not None:
            args += ["--snapshot", str(snapshot)]
        return args

    # -- verbs --------------------------------------------------------------

    def resolve(self, artifact, *, snapshot=None) -> dict:
        return self._json("resolve", self._selector(artifact, snapshot=snapshot))

    def plan(self, artifact, *, snapshot=None) -> dict:
        return self._json("plan", self._selector(artifact, snapshot=snapshot))

    def materialize(
        self,
        artifact,
        out,
        *,
        snapshot=None,
        topology=None,
        axis=0,
    ) -> dict:
        args = self._selector(artifact, snapshot=snapshot) + [
            "--out",
            os.fspath(out),
        ]
        if topology is not None:
            args += ["--topology", str(topology), "--axis", str(axis)]
        return self._json("materialize", args)

    def verify(self, artifact, *, snapshot=None, out=None) -> dict:
        args = self._selector(artifact, snapshot=snapshot)
        if out is not None:
            args += ["--out", os.fspath(out)]
        return self._json("verify", args)

    def pin(
        self,
        artifact,
        *,
        snapshot,
        policy_epoch=0,
        declared_lifetime_ms=31_536_000_000,
    ) -> dict:
        args = self._selector(artifact) + [
            "--snapshot",
            str(snapshot),
            "--policy-epoch",
            str(int(policy_epoch)),
            "--declared-lifetime-ms",
            str(int(declared_lifetime_ms)),
        ]
        return self._json("pin", args)

    def release(self, artifact, *, snapshot) -> dict:
        args = self._selector(artifact) + ["--snapshot", str(snapshot)]
        return self._json("release", args)

    def execute(self, ops, *, snapshot=None) -> dict:
        """Run `ops` as **one** group commit."""
        with _temp_json({"ops": ops}) as path:
            args = ["--ops", path]
            if snapshot is not None:
                args += ["--snapshot", str(snapshot)]
            return self._json("txn", args)

    @contextmanager
    def transaction(self, *, snapshot=None):
        tx = ArtifactTransaction(self, snapshot=snapshot)
        yield tx
        tx.commit()

    # -- tensor streaming ---------------------------------------------------

    def tensor_entry(self, checkpoint, tensor, *, snapshot=None) -> _Entry:
        key = (
            str(checkpoint),
            str(tensor),
            str(snapshot) if snapshot is not None else "",
        )
        if key not in self._entries:
            artifact = artifact_ref(checkpoint=checkpoint, tensor=tensor)
            resolution = self.resolve(artifact, snapshot=snapshot)
            for row in resolution["entries"]:
                if row["name"] == tensor:
                    self._entries[key] = _Entry(
                        name=row["name"],
                        kind=row["kind"],
                        content_root=row["content_root"],
                        logical_root=row.get("logical_root"),
                        dtype=row.get("dtype"),
                        shape=tuple(row.get("shape", ())),
                        bytes=int(row["bytes"]),
                        shards=int(row.get("shards", 0)),
                        dependencies=tuple(row.get("dependencies", ())),
                    )
                    break
            else:
                raise ArtifactApiError(
                    "checkpoint %r has no tensor %r" % (checkpoint, tensor),
                    error_class="NotFound",
                )
        return self._entries[key]

    def materialize_tensor(
        self,
        checkpoint,
        tensor,
        *,
        snapshot=None,
        topology=None,
        axis=0,
        expected_size=None,
    ) -> ArtifactTensorBuffer:
        entry = self.tensor_entry(checkpoint, tensor, snapshot=snapshot)
        size = int(expected_size) if expected_size is not None else entry.bytes
        if entry.bytes != size:
            raise ArtifactApiError(
                "expected %d bytes for %r, the manifest declares %d"
                % (size, tensor, entry.bytes),
                error_class="Corrupt",
            )
        artifact = artifact_ref(checkpoint=checkpoint, tensor=tensor)
        args = ["artifact", "materialize"]
        args += self._selector(artifact, snapshot=snapshot)
        args += ["--out", "-", "--json"]
        if topology is not None:
            args += ["--topology", str(topology), "--axis", str(axis)]
        proc = self._run(args, binary=True)
        self.calls.append("materialize")
        data = proc.stdout
        metadata = {}
        stderr = proc.stderr.decode("utf-8", "replace").strip()
        if stderr:
            try:
                metadata = json.loads(stderr.splitlines()[-1])
            except ValueError:
                metadata = {}
        if len(data) != size:
            raise ArtifactApiError(
                "materialize streamed %d bytes, expected %d" % (len(data), size),
                error_class="Corrupt",
            )
        fd = os.memfd_create("weft-artifact-tensor", os.MFD_CLOEXEC)
        try:
            os.ftruncate(fd, size)
            view = mmap.mmap(fd, size, access=mmap.ACCESS_WRITE) if size else None
            if size:
                view.write(data)
                view.close()
            from .hashing import content_root as _content_root

            handle = ArtifactTensorBuffer(
                fd,
                size,
                entry=entry,
                plan_root=metadata.get("plan_root"),
                snapshot=snapshot,
                provisional=bool(metadata.get("provisional", snapshot is None)),
            )
            handle.shards_read = int(metadata.get("shards_read", 0))
            handle.units_touched = int(metadata.get("units_touched", 0))
            # The streaming form reports the exact payload; backend request
            # counters are not part of the artifact API response.
            handle.store_bytes_read = 0
            handle.store_reads = 0
            fd = None
            if _content_root(handle.view) != handle.content_root:
                handle.close()
                raise ArtifactApiError(
                    "streamed tensor content root does not match the manifest",
                    error_class="Corrupt",
                )
            return handle
        finally:
            if fd is not None:
                try:
                    os.close(fd)
                except OSError:
                    pass

    def wrapped(self, service, *, checkpoint):
        """Return a proxy whose ``restore_tensor`` uses this API.

        Every other attribute is forwarded to ``service``, so a consumer keeps
        its metadata/protocol path while its tensor reads come from the one
        artifact API.
        """
        return _ArtifactServiceProxy(service, self, checkpoint)


class ArtifactTransaction:
    """Ops accumulated for one group commit. ``commit`` is idempotent."""

    def __init__(self, client, *, snapshot=None):
        self.client = client
        self.snapshot = snapshot
        self.ops = []
        self.result = None

    def pin(self, artifact, *, snapshot=None, policy_epoch=0, declared_lifetime_ms=31_536_000_000):
        self.ops.append(
            {
                "op": "pin",
                "artifact": artifact,
                "snapshot": str(snapshot or self.snapshot),
                "policy_epoch": int(policy_epoch),
                "declared_lifetime_ms": int(declared_lifetime_ms),
            }
        )
        return self

    def release(self, artifact, *, snapshot=None):
        self.ops.append(
            {
                "op": "release",
                "artifact": artifact,
                "snapshot": str(snapshot or self.snapshot),
            }
        )
        return self

    def commit(self):
        if self.result is not None:
            return self.result
        if not self.ops:
            self.result = {"ok": True, "ops": [], "commit": None}
            return self.result
        self.result = self.client.execute(self.ops)
        return self.result


class _ArtifactServiceProxy:
    """Delegates non-tensor calls; tensor restores go through the API."""

    def __init__(self, service, client, checkpoint):
        self._service = service
        self._client = client
        self._checkpoint = checkpoint

    def restore_tensor(
        self,
        name,
        tensor,
        *,
        region=None,
        topology=None,
        expected_size=None,
    ):
        if region is not None:
            # Region reads keep the checkpoint service path; the artifact API's
            # materialize verb exposes whole tensors only (documented).
            return self._service.restore_tensor(
                name,
                tensor,
                region=region,
                topology=topology,
                expected_size=expected_size,
            )
        shards, axis = (None, 0) if topology is None else topology
        return self._client.materialize_tensor(
            name,
            tensor,
            topology=shards,
            axis=axis,
            expected_size=expected_size,
        )

    def __getattr__(self, name):
        return getattr(self._service, name)


@contextmanager
def _temp_json(payload):
    import tempfile

    handle = tempfile.NamedTemporaryFile(
        prefix="weft-artifact-ops-", suffix=".json", delete=False
    )
    try:
        handle.write(json.dumps(payload).encode("utf-8"))
        handle.close()
        yield handle.name
    finally:
        try:
            os.unlink(handle.name)
        except OSError:
            pass
