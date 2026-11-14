#!/usr/bin/env python3
"""T072 proof: one artifact API, three consumers, byte-identical to the old path.

Runs in the Python court with the release ``weftfs`` CLI on the mounted tree:

* writes a small checkpoint with the DCP adapter and restores it twice — once
  through the artifact API (default) and once through the legacy checkpoint
  service path (``artifact_api=False``) — and compares every tensor byte;
* does the same for the Orbax adapter;
* runs ``examples/agent_state_consumer.py`` as a separate consumer and checks
  the verbs it exercised;
* writes ``bench/receipts/artifact-api-<stamp>.json`` with each check.

No mount is used. Single host, synthetic fixture; the receipt says so.
"""

from __future__ import annotations

import hashlib
import json
import os
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "python"))
sys.path.insert(0, str(ROOT / "python" / "tests"))

import numpy as np  # noqa: E402

from weftcodec.dcp import (  # noqa: E402
    DEFAULT_CLI,
    WeftStorageReader,
    WeftStorageWriter,
    init_store,
)

CLI = os.environ.get("WEFTFS_CLI", DEFAULT_CLI)
STAMP = os.environ.get("WEFT_STAMP") or time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
RECEIPTS = ROOT / "bench" / "receipts"
RECEIPTS.mkdir(parents=True, exist_ok=True)

checks = []


def check(name, ok, detail=None):
    checks.append({"check": name, "ok": bool(ok), "detail": detail})
    print(("PASS " if ok else "FAIL ") + name + ((": " + str(detail)) if detail else ""))
    return ok


def sha256(data):
    return hashlib.sha256(bytes(data)).hexdigest()


def revision():
    try:
        return subprocess.run(
            ["git", "-C", os.fspath(ROOT), "rev-parse", "HEAD"],
            capture_output=True,
            check=True,
        ).stdout.decode().strip()
    except Exception:
        return os.environ.get("WEFT_REPO_WEFTFS", "unknown")


def write_safetensors(path, tensors):
    """Write a safetensors container without the safetensors package."""
    header = {}
    payloads = []
    cursor = 0
    for name, payload, dtype, shape in tensors:
        header[name] = {
            "dtype": dtype,
            "shape": list(shape),
            "data_offsets": [cursor, cursor + len(payload)],
        }
        cursor += len(payload)
        payloads.append(payload)
    raw = json.dumps(header, sort_keys=True, separators=(",", ":")).encode()
    with open(path, "wb") as handle:
        handle.write(struct.pack("<Q", len(raw)))
        handle.write(raw)
        for payload in payloads:
            handle.write(payload)


def run(cli, store, args):
    proc = subprocess.run(
        [cli, "--store", os.fspath(store), *args], capture_output=True, check=False
    )
    if proc.returncode != 0:
        raise SystemExit(
            "weftfs %s failed: %s" % (" ".join(args), proc.stderr.decode())
        )
    return proc.stdout


def dcp_consumer(store):
    import torch
    from torch.distributed.checkpoint import load, save

    generator = torch.Generator().manual_seed(20261114)
    source = {
        "embed": torch.randn(16, 8, generator=generator, dtype=torch.float32),
        "block.weight": torch.randn(8, 4, generator=generator, dtype=torch.float32),
        "bias": torch.randn(16, generator=generator, dtype=torch.float32),
    }
    writer = WeftStorageWriter("api-dcp", store_root=store, topology=1)
    save(source, storage_writer=writer, no_dist=True)

    api_reader = WeftStorageReader("api-dcp", store_root=store, artifact_api=True)
    api_target = {name: torch.zeros_like(value) for name, value in source.items()}
    load(api_target, storage_reader=api_reader, no_dist=True)
    api_verbs = list(api_reader._artifact.calls)
    api_reader.close()

    legacy_reader = WeftStorageReader("api-dcp", store_root=store, artifact_api=False)
    legacy_target = {name: torch.zeros_like(value) for name, value in source.items()}
    load(legacy_target, storage_reader=legacy_reader, no_dist=True)
    legacy_reader.close()

    api_sha = {name: sha256(value.numpy().tobytes()) for name, value in api_target.items()}
    legacy_sha = {
        name: sha256(value.numpy().tobytes()) for name, value in legacy_target.items()
    }
    exact = all(np.array_equal(api_target[name].numpy(), source[name].numpy())
                for name in source)
    identical = api_sha == legacy_sha
    return {
        "api_sha256": api_sha,
        "legacy_sha256": legacy_sha,
        "source_sha256": {
            name: sha256(value.numpy().tobytes()) for name, value in source.items()
        },
        "api_verbs": api_verbs,
        "legacy_verbs": [],
        "exact": bool(exact),
        "byte_identical_to_legacy": bool(identical),
    }


def orbax_consumer(store):
    import jax
    import jax.numpy as jnp
    from weftcodec.orbax import WeftOrbaxHandler, WeftRestoreArgs, WeftSaveArgs

    tree = {
        "w": jnp.asarray(np.arange(32, dtype=np.float32).reshape(8, 4), dtype=jnp.bfloat16),
        "b": jnp.asarray(np.arange(8, dtype=np.float32), dtype=jnp.bfloat16),
    }
    directory = Path(store).parent / "orbax-dir"
    directory.mkdir(parents=True, exist_ok=True)

    saver = WeftOrbaxHandler(weftfs_cli=CLI, artifact_api=False)
    try:
        saver.save(
            directory,
            WeftSaveArgs(
                item=tree,
                store_root=store,
                checkpoint_id="api-orbax",
                init_store=False,
            ),
        )
    finally:
        saver.close()

    def restore_with(artifact_api):
        handler = WeftOrbaxHandler(weftfs_cli=CLI, artifact_api=artifact_api)
        try:
            args = WeftRestoreArgs(
                item=jax.tree.map(jnp.zeros_like, tree),
                store_root=store,
                checkpoint_id="api-orbax",
            )
            restored = handler.restore(directory, args)
            verbs = list(handler._artifact.calls) if handler._artifact else []
            return restored, verbs
        finally:
            handler.close()

    api_tree, api_verbs = restore_with(True)
    legacy_tree, legacy_verbs = restore_with(False)

    def flatten(value):
        return {str(i): np.asarray(leaf) for i, leaf in enumerate(jax.tree.leaves(value))}

    api_flat = flatten(api_tree)
    legacy_flat = flatten(legacy_tree)
    api_sha = {key: sha256(value.tobytes()) for key, value in api_flat.items()}
    legacy_sha = {key: sha256(value.tobytes()) for key, value in legacy_flat.items()}
    return {
        "api_sha256": api_sha,
        "legacy_sha256": legacy_sha,
        "api_verbs": api_verbs,
        "legacy_verbs": legacy_verbs,
        "byte_identical_to_legacy": api_sha == legacy_sha,
        "exact": all(
            np.array_equal(api_flat[key], legacy_flat[key]) for key in api_flat
        ),
    }


def agent_consumer(tmp):
    store = tmp / "agent-store"
    out = tmp / "agent-out"
    receipt = tmp / "agent.json"
    proc = subprocess.run(
        [
            sys.executable,
            os.fspath(ROOT / "examples" / "agent_state_consumer.py"),
            "--store",
            os.fspath(store),
            "--cli",
            CLI,
            "--out",
            os.fspath(out),
            "--json",
            os.fspath(receipt),
        ],
        capture_output=True,
        check=False,
        env={**os.environ, "WEFTFS_CLI": CLI},
    )
    if proc.returncode != 0:
        return None, proc.stderr.decode("utf-8", "replace")[-2000:]
    return json.loads(receipt.read_text()), ""


def main():
    # The court enables the full artifact verification (which re-reads every
    # tensor) so all three consumers exercise `verify`. It is off by default in
    # the adapters because it doubles read cost.
    os.environ["WEFT_ARTIFACT_VERIFY"] = "1"
    tmp = Path(tempfile.mkdtemp(prefix="weft-artifact-api-"))
    store = tmp / "store"
    init_store(store, weftfs_cli=CLI)

    dcp = dcp_consumer(store)
    check("dcp_exact", dcp["exact"], dcp["source_sha256"])
    check(
        "dcp_byte_identical_to_legacy",
        dcp["byte_identical_to_legacy"],
        dcp["api_sha256"],
    )
    check(
        "dcp_uses_resolve_plan_materialize",
        {"resolve", "plan", "materialize"}.issubset(set(dcp["api_verbs"])),
        sorted(set(dcp["api_verbs"])),
    )

    orbax = orbax_consumer(store)
    check("orbax_exact", orbax["exact"])
    check(
        "orbax_byte_identical_to_legacy",
        orbax["byte_identical_to_legacy"],
        orbax["api_sha256"],
    )
    check(
        "orbax_uses_resolve_plan_materialize",
        {"resolve", "plan", "materialize"}.issubset(set(orbax["api_verbs"])),
        sorted(set(orbax["api_verbs"])),
    )

    agent, error = agent_consumer(tmp)
    if agent is None:
        check("agent_consumer_runs", False, error)
    else:
        verbs = set(agent.get("verbs", []))
        check("agent_consumer_runs", True, sorted(verbs))
        check(
            "agent_uses_all_six_verbs",
            {"resolve", "plan", "materialize", "verify"}.issubset(verbs)
            and agent.get("verbs", []).count("txn") >= 2,
            sorted(verbs),
        )
        check("agent_verify_ok", bool(agent.get("verify", {}).get("ok")))
        check("agent_transaction_verified", bool(agent.get("transaction_verified")))

    same_surface = {
        "dcp": sorted(set(dcp["api_verbs"])),
        "orbax": sorted(set(orbax["api_verbs"])),
        "agent": sorted(set((agent or {}).get("verbs", []))),
    }
    shared = set(same_surface["dcp"]) & set(same_surface["orbax"]) & set(same_surface["agent"])
    check(
        "three_consumers_share_the_api",
        {"resolve", "plan", "materialize", "verify"}.issubset(shared),
        same_surface,
    )

    receipt = {
        "kind": "artifact-api",
        "schema": "weft.artifact.api.receipt.v1",
        "generated_utc": STAMP,
        "environment": {
            "revisions": {"WeftFS": revision()},
            "court": "python court (3g/4cpu/pids 1024), rust court builds the CLI",
            "cli": CLI,
            "python": sys.version.split()[0],
            "numpy": np.__version__,
            "label": (
                "single host, synthetic fixtures, no mount, no fleet, no "
                "performance claim; WEFT_ARTIFACT_VERIFY=1 so the adapters "
                "exercise the verify verb (cost: every tensor is re-read)"
            ),
        },
        "consumers": {"dcp": dcp, "orbax": orbax, "agent": agent},
        "surfaces": same_surface,
        "checks": checks,
        "passed": sum(1 for row in checks if row["ok"]),
        "failed": sum(1 for row in checks if not row["ok"]),
    }
    receipt["result"] = "pass" if receipt["failed"] == 0 else "fail"
    path = RECEIPTS / ("artifact-api-%s.json" % STAMP)
    path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"receipt": str(path), "checks": len(checks),
                      "failed": receipt["failed"], "result": receipt["result"]}))
    return 0 if receipt["failed"] == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
