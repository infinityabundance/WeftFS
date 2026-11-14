"""Root-to-output materialization receipts for the framework adapters.

``WeftFS`` writes a canonical ``weft.materialization.receipt.v1`` by
re-reading a checkpoint from the store and recomputing every root; a separate
process (``weftfs receipt verify``) repeats that derivation. This module is the
adapter side:

* it asks the store for that receipt;
* it checks every tensor the adapter actually restored against the row the
  store derived (dtype, shape, bytes, BLAKE3 content root, SHA-256);
* it writes a canonical ``weft.adapter.receipt.v1`` binding framework paths
  (DCP FQNs, Orbax leaf paths) to those rows;
* it runs the independent verifier on both records and fails closed.

Nothing here trusts a recorded number: the comparison target is always the row
the verifier re-derives from the store.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import time
from pathlib import Path

from . import wcb
from .hashing import hash_domain
from .wcb import U64

MATERIALIZATION_SCHEMA = "weft.materialization.receipt.v1"
MATERIALIZATION_DOMAIN = "weft/materialization/receipt/v1"
ADAPTER_SCHEMA = "weft.adapter.receipt.v1"
ADAPTER_DOMAIN = "weft/adapter/receipt/v1"
ADAPTER_SET_DOMAIN = "weft/adapter/set/v1"

DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"
DEFAULT_TIMEOUT_SECONDS = 600.0


class MaterializationReceiptError(RuntimeError):
    """A receipt is missing, non-canonical, or disagrees with the adapter."""


def _cli(cli=None) -> str:
    return cli or os.environ.get("WEFTFS_CLI") or DEFAULT_CLI


def receipt_root(blob: bytes) -> bytes:
    return hash_domain(MATERIALIZATION_DOMAIN, blob)


def adapter_root(blob: bytes) -> bytes:
    return hash_domain(ADAPTER_DOMAIN, blob)


def _adapter_set_root(rows) -> bytes:
    ordered = sorted(rows, key=lambda row: row["fqn"])
    value = []
    for row in ordered:
        value.append(
            {
                "fqn": row["fqn"],
                "tensor_name": row["tensor_name"],
                "content_root": bytes(row["content_root"]),
            }
        )
    return hash_domain(ADAPTER_SET_DOMAIN, wcb.encode(value))


def decode_materialization(blob: bytes) -> dict:
    try:
        value = wcb.decode(blob)
    except Exception as exc:  # noqa: BLE001 - surface the WCB class verbatim
        raise MaterializationReceiptError(
            "materialization receipt is not canonical WCB: %s" % exc
        ) from exc
    if not isinstance(value, dict):
        raise MaterializationReceiptError("materialization receipt is not a map")
    if value.get("schema") != MATERIALIZATION_SCHEMA:
        raise MaterializationReceiptError(
            "materialization schema %r is not %r"
            % (value.get("schema"), MATERIALIZATION_SCHEMA)
        )
    if value.get("verification") != "full":
        raise MaterializationReceiptError(
            "materialization verification is not `full`"
        )
    shaped = {}
    set_rows = []
    for row in value.get("tensors", []):
        name = row["name"]
        if name in shaped:
            raise MaterializationReceiptError("duplicate tensor %r" % name)
        entry = {
            "name": name,
            "dtype": row["dtype"],
            "shape": tuple(int(dim.value) for dim in row["shape"]),
            "logical_root": bytes(row["logical_root"]),
            "content_root": bytes(row["content_root"]),
            "sha256": bytes(row["sha256"]),
            "bytes": int(row["bytes"].value),
            "source_objects": row["source_objects"],
            "destination": row["destination"],
            "verification": row["verification"],
        }
        if entry["verification"] != "exact":
            raise MaterializationReceiptError(
                "tensor %r is not row `exact`" % name
            )
        shaped[name] = entry
        set_rows.append({"name": name, "logical_root": entry["logical_root"]})
    recomputed = hash_domain(
        "weft/materialization/set/v1", wcb.encode(sorted(set_rows, key=lambda r: r["name"]))
    )
    if recomputed != bytes(value["set_root"]):
        raise MaterializationReceiptError(
            "materialization set_root does not match the tensor roster"
        )
    value["_by_name"] = shaped
    return value


def encode_adapter_receipt(
    *,
    adapter: str,
    checkpoint: str,
    materialization_root: bytes,
    manifest_root: bytes,
    producer: str,
    rows,
    wall_ms: int,
) -> tuple[bytes, bytes]:
    set_root = _adapter_set_root(rows)
    value = {
        "schema": ADAPTER_SCHEMA,
        "adapter": adapter,
        "checkpoint": checkpoint,
        "materialization_root": bytes(materialization_root),
        "manifest_root": bytes(manifest_root),
        "producer": producer,
        "entries": [
            {
                "fqn": row["fqn"],
                "tensor_name": row["tensor_name"],
                "dtype": row["dtype"],
                "shape": [U64(dim) for dim in row["shape"]],
                "bytes": U64(int(row["bytes"])),
                "sha256": bytes(row["sha256"]),
                "content_root": bytes(row["content_root"]),
            }
            for row in sorted(rows, key=lambda row: row["fqn"])
        ],
        "set_root": set_root,
        "wall_ms": U64(int(wall_ms)),
    }
    blob = wcb.encode(value)
    return blob, adapter_root(blob)


def _run(cli: str, store_root, args, timeout: float):
    command = [cli, "--store", os.fspath(store_root), *[str(a) for a in args]]
    proc = subprocess.run(command, capture_output=True, timeout=timeout, check=False)
    if proc.returncode != 0:
        raise MaterializationReceiptError(
            "weftfs %s failed (%d): %s"
            % (" ".join(str(a) for a in args), proc.returncode, proc.stderr.decode("utf-8", "replace").strip())
        )
    return proc.stdout


def _current_manifest_root(cli: str, store_root, checkpoint: str, timeout: float) -> str:
    out = _run(
        cli,
        store_root,
        ["--json", "checkpoint", "show", "--name", checkpoint],
        timeout,
    )
    payload = json.loads(out.decode("utf-8"))
    root = payload.get("manifest_root")
    if not isinstance(root, str) or not root:
        raise MaterializationReceiptError(
            "the store did not report a manifest root for %r" % checkpoint
        )
    return root


def ensure_materialization_receipt(
    store_root,
    checkpoint: str,
    receipt_path,
    *,
    cli=None,
    replica: str = "adapter",
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
) -> Path:
    """Ask the store to derive and write the receipt.

    An existing file is reused **only** when it names the current checkpoint
    and the store's current manifest root. A stale receipt bound to an older
    generation would otherwise make the adapter attest a different state than
    the one it restored.
    """
    path = Path(receipt_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    reusable = False
    if path.is_file() and path.stat().st_size:
        try:
            value = decode_materialization(path.read_bytes())
            current = _current_manifest_root(_cli(cli), store_root, checkpoint, timeout)
            reusable = (
                value.get("checkpoint") == checkpoint
                and bytes(value["manifest_root"]).hex() == current
            )
        except Exception:  # noqa: BLE001 - a stale or invalid file is rewritten
            reusable = False
    if not reusable:
        tmp = path.with_suffix(path.suffix + ".tmp")
        if tmp.exists():
            tmp.unlink()
        _run(
            _cli(cli),
            store_root,
            [
                "checkpoint",
                "receipt",
                "--name",
                checkpoint,
                "--out",
                os.fspath(tmp),
                "--replica",
                replica,
            ],
            timeout,
        )
        os.replace(tmp, path)
    if not path.is_file() or path.stat().st_size == 0:
        raise MaterializationReceiptError(
            "the store did not produce a materialization receipt at %s" % path
        )
    return path


def emit_adapter_receipt(
    *,
    store_root,
    checkpoint: str,
    rows,
    adapter: str,
    producer: str,
    receipt_path,
    adapter_path=None,
    cli=None,
    replica: str = "adapter",
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    verify: bool = True,
) -> dict:
    """Bind restored framework entries to a store-derived receipt.

    ``rows`` must come from the adapter's **actual** restored bytes:
    ``{fqn, tensor_name, dtype, shape, bytes, sha256, content_root}``.
    """
    started = time.time()
    receipt_path = ensure_materialization_receipt(
        store_root, checkpoint, receipt_path, cli=cli, replica=replica, timeout=timeout
    )
    blob = receipt_path.read_bytes()
    materialization = decode_materialization(blob)
    by_name = materialization["_by_name"]

    mismatches = []
    bound = []
    for row in rows:
        tensor = by_name.get(row["tensor_name"])
        if tensor is None:
            mismatches.append(
                "adapter entry %r names tensor %r, absent from the receipt"
                % (row["fqn"], row["tensor_name"])
            )
            continue
        if tensor["dtype"] != row["dtype"]:
            mismatches.append("dtype mismatch for %r" % row["fqn"])
        if tuple(tensor["shape"]) != tuple(row["shape"]):
            mismatches.append("shape mismatch for %r" % row["fqn"])
        if tensor["bytes"] != int(row["bytes"]):
            mismatches.append("byte-length mismatch for %r" % row["fqn"])
        if tensor["sha256"] != bytes(row["sha256"]):
            mismatches.append(
                "sha256 mismatch for %r (%s != %s)"
                % (
                    row["fqn"],
                    bytes(row["sha256"]).hex()[:16],
                    tensor["sha256"].hex()[:16],
                )
            )
        if tensor["content_root"] != bytes(row["content_root"]):
            mismatches.append("content root mismatch for %r" % row["fqn"])
        bound.append(
            {
                "fqn": row["fqn"],
                "tensor_name": row["tensor_name"],
                "dtype": row["dtype"],
                "shape": tuple(row["shape"]),
                "bytes": int(row["bytes"]),
                "sha256": bytes(row["sha256"]),
                "content_root": bytes(row["content_root"]),
            }
        )
    if mismatches:
        raise MaterializationReceiptError(
            "adapter outputs disagree with the store-derived receipt: %s"
            % "; ".join(mismatches[:8])
        )

    materialization_root = receipt_root(blob)
    path = Path(adapter_path) if adapter_path else receipt_path.with_suffix(
        receipt_path.suffix + ".adapter.wcb"
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    adapter_blob, adapter_digest = encode_adapter_receipt(
        adapter=adapter,
        checkpoint=checkpoint,
        materialization_root=materialization_root,
        manifest_root=bytes(materialization["manifest_root"]),
        producer=producer,
        rows=bound,
        wall_ms=int((time.time() - started) * 1000.0),
    )
    path.write_bytes(adapter_blob)

    report = None
    if verify:
        out = _run(
            _cli(cli),
            store_root,
            [
                "--json",
                "receipt",
                "verify",
                "--receipt",
                os.fspath(receipt_path),
                "--adapter",
                os.fspath(path),
            ],
            timeout,
        )
        report = json.loads(out.decode("utf-8"))
        if not report.get("ok"):
            raise MaterializationReceiptError(
                "independent verifier refused the receipts: %s"
                % json.dumps(report.get("mismatches", [])[:8])
            )
    return {
        "materialization_receipt": os.fspath(receipt_path),
        "materialization_root": materialization_root.hex(),
        "adapter_receipt": os.fspath(path),
        "adapter_root": adapter_digest.hex(),
        "entries": len(bound),
        "tensors": len(by_name),
        "verification": report,
        "schema": MATERIALIZATION_SCHEMA,
    }


def tensor_digest(data, dtype: str, shape) -> dict:
    """A row from actual restored bytes (BLAKE3 + SHA-256)."""
    raw = bytes(data)
    return {
        "dtype": dtype,
        "shape": tuple(int(dim) for dim in shape),
        "bytes": len(raw),
        "sha256": hashlib.sha256(raw).digest(),
        "content_root": hash_domain("weft/chunk/v1", raw),
    }
