#!/usr/bin/env python3
"""Orbax/WeftFS comparison court.

Runs inside the Docker python court (``bench/scripts/orbax_court.sh``) with
``jax[cpu]`` and ``orbax-checkpoint`` installed under
``/work/.docker-cache/py-orbax``. It is deliberately self-contained:

* a real 8-device CPU mesh (``XLA_FLAGS`` forces the host device count);
* one sharded PyTree saved with Orbax's own OCDBT/Sync+Async checkpointer and
  with the WeftFS Orbax adapter;
* save pause, total save, full restore, single-leaf restore, physical bytes,
  leaf exactness, a different restore sharding topology, and the Rust CLI's
  ``checkpoint verify`` + ``verify --full``;
* a JSON receipt under ``bench/receipts/orbax-<stamp>.json``.

If JAX/Orbax cannot be imported it writes an ``orbax_unavailable`` receipt with
the exact error and exits 0. That is the only "stop early" branch, and it makes
no comparison claim.
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import resource
import shutil
import subprocess
import sys
import tempfile
import time
import traceback
import xml.etree.ElementTree as ET
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CLI = Path(
    os.environ.get(
        "WEFTFS_CLI", "/work/.docker-cache/target-rust/release/weftfs"
    )
)


def utc_stamp():
    return time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())


STAMP = os.environ.get("WEFT_ORBAX_STAMP") or utc_stamp()
RECEIPT = Path(
    os.environ.get("WEFT_ORBAX_RECEIPT", REPO / "bench" / "receipts" / ("orbax-%s.json" % STAMP))
)


# ---------------------------------------------------------------------------
# Small reporting helpers


def read_text_tail(path, limit=4000):
    try:
        raw = Path(path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    return raw[-limit:]


def junit_counts(path):
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError):
        return None
    suite = root.find("testsuite") if root.tag != "testsuite" else root
    if suite is None:
        return None
    return {
        key: int(suite.get(key, 0))
        for key in ("tests", "failures", "errors", "skipped")
    } | {"time_s": float(suite.get("time", 0.0) or 0.0)}


def rss_kib():
    try:
        for line in Path("/proc/self/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
            if line.startswith("VmHWM:"):
                return int(line.split()[1])
    except OSError:
        return None
    return None


def vm_hwm_kib():
    try:
        for line in Path("/proc/self/status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1])
    except OSError:
        return None
    return None


def peak_rss_kib():
    return int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)


def directory_physical(path):
    total = 0
    files = 0
    for dirpath, _dirnames, filenames in os.walk(path):
        for name in filenames:
            full = os.path.join(dirpath, name)
            try:
                total += os.path.getsize(full)
                files += 1
            except OSError:
                pass
    return {"bytes": total, "files": files}


def command_version(argv):
    try:
        proc = subprocess.run(
            [str(x) for x in argv], capture_output=True, text=True, check=False
        )
    except OSError as exc:
        return {"argv": [str(x) for x in argv], "error": str(exc)}
    return {
        "argv": [str(x) for x in argv],
        "returncode": proc.returncode,
        "stdout": proc.stdout.strip()[:400],
        "stderr": proc.stderr.strip()[:400],
    }


def environment():
    return {
        "python_version": platform.python_version(),
        "python_executable": sys.executable,
        "platform": platform.platform(),
        "cpu_count": os.cpu_count(),
        "xla_flags": os.environ.get("XLA_FLAGS"),
        "weftfs_cli": str(CLI),
        "weftfs_version": command_version([CLI, "--version"]),
        "court_image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "court_kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
        "court_memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
        "court_cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
        "court_pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        "court_tmpfs": "/tmp:rw,size=1g,nosuid,nodev",
        "court_cap_drop": "ALL",
        "court_no_new_privileges": True,
        "docker_client": os.environ.get("WEFT_DOCKER_CLIENT", "unknown"),
        "docker_server": os.environ.get("WEFT_DOCKER_SERVER", "unknown"),
        "revision_weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "revision_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        "revision_weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        "revision_weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
    }


def write_receipt(receipt):
    RECEIPT.parent.mkdir(parents=True, exist_ok=True)
    tmp = RECEIPT.with_name(RECEIPT.name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")
    os.replace(tmp, RECEIPT)
    print("receipt:", RECEIPT)
    print(
        json.dumps(
            {
                "status": receipt.get("status"),
                "orbax_available": receipt.get("orbax_available"),
                "checks": {
                    "total": len(receipt.get("checks", [])),
                    "failed": sum(
                        1 for check in receipt.get("checks", []) if not check["ok"]
                    ),
                },
                "receipt": str(RECEIPT),
            },
            indent=2,
        )
    )


def base_receipt():
    install_status = os.environ.get("WEFT_ORBAX_INSTALL_STATUS")
    return {
        "receipt": "weftfs-orbax-court/1",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "label": (
            "WeftFS Orbax adapter court: a real 8-device CPU JAX mesh, one "
            "sharded PyTree, compared against Orbax's own OCDBT checkpoint. "
            "Single process, single host, synthetic random weights, warm cache. "
            "No TPU, GPU, multi-host, or fleet claim."
        ),
        "environment": environment(),
        "install": {
            "target": os.environ.get("WEFT_ORBAX_TARGET"),
            "performed": os.environ.get("WEFT_ORBAX_INSTALL_PERFORMED") == "1",
            "command": os.environ.get("WEFT_ORBAX_INSTALL_CMD"),
            "status": int(install_status) if install_status not in (None, "") else None,
            "log": os.environ.get("WEFT_ORBAX_INSTALL_LOG"),
            "log_tail": read_text_tail(os.environ.get("WEFT_ORBAX_INSTALL_LOG")),
        },
        "tests": {
            "status": os.environ.get("WEFT_ORBAX_TEST_STATUS"),
            "junit": junit_counts(os.environ.get("WEFT_ORBAX_JUNIT", "/nonexistent")),
            "log": os.environ.get("WEFT_ORBAX_TESTS_LOG"),
        },
        "orbax_available": False,
        "status": "pending",
        "workload": None,
        "results": None,
        "checks": [],
        "observations": [],
        "losses": [],
        "not_covered": [],
        "commands": [],
    }


# ---------------------------------------------------------------------------
# Unavailable path


def unavailable(exc):
    receipt = base_receipt()
    receipt["status"] = "orbax_unavailable"
    receipt["orbax_available"] = False
    receipt["error"] = {
        "class": type(exc).__name__,
        "message": str(exc),
        "traceback": traceback.format_exc()[-4000:],
    }
    receipt["not_covered"] = [
        "orbax-checkpoint could not be imported in this court, so no save/restore "
        "comparison was run and no claim about Orbax compatibility is made.",
        "The adapter and its unit tests are delivered; the unit tests skip with an "
        "explicit reason when JAX/Orbax is absent.",
    ]
    write_receipt(receipt)
    return 0


# ---------------------------------------------------------------------------
# Workload


def build_workload():
    """Return ``(source, specs)`` for a deterministic, sharded PyTree."""

    import jax
    import jax.numpy as jnp
    import numpy as np
    from jax.sharding import Mesh, NamedSharding, PartitionSpec as P

    devices = jax.devices()
    if len(devices) < 2:
        raise RuntimeError(
            "only %d JAX device(s); set XLA_FLAGS=--xla_force_host_platform_device_count=8"
            % len(devices)
        )
    mesh = Mesh(np.array(devices), ("x",))
    sharding = NamedSharding(mesh, P("x"))
    key = jax.random.PRNGKey(20261106)
    keys = jax.random.split(key, 11)

    def rand(key, shape, dtype):
        if dtype in (jnp.float16, jnp.bfloat16, jnp.float32, jnp.float64):
            return jax.random.normal(key, shape, dtype=jnp.float32).astype(dtype)
        if dtype == jnp.uint8:
            return jax.random.randint(key, shape, 0, 256, dtype=jnp.uint8)
        if dtype == jnp.int8:
            return jax.random.randint(key, shape, -128, 127, dtype=jnp.int8)
        if dtype == jnp.int32:
            return jax.random.randint(key, shape, 0, 1000, dtype=jnp.int32)
        raise ValueError("no workload generator for %s" % dtype)

    def put(value):
        return jax.device_put(value, sharding)

    source = {
        "params": {
            "embed": put(rand(keys[0], (8192, 512), jnp.bfloat16)),
            "layers": [
                {
                    "w": put(rand(keys[1], (4096, 1024), jnp.bfloat16)),
                    "b": put(rand(keys[2], (4096,), jnp.float32)),
                    "scale": put(rand(keys[3], (4096,), jnp.float16)),
                },
                {
                    "w": put(rand(keys[4], (4096, 1024), jnp.float32)),
                    "b": put(rand(keys[5], (4096,), jnp.float32)),
                    "mask": put(rand(keys[6], (4096,), jnp.int8)),
                },
            ],
        },
        "opt_state": {
            "momentum": {"embed": put(rand(keys[7], (8192, 512), jnp.bfloat16))},
            "variance": {"embed": put(rand(keys[8], (8192, 512), jnp.float32))},
            "step": put(rand(keys[9], (8,), jnp.int32)),
        },
        "counters": put(rand(keys[10], (8,), jnp.uint8)),
    }
    specs = []
    for path, leaf in jax.tree_util.tree_flatten_with_path(source)[0]:
        specs.append(
            {
                "path": jax.tree_util.keystr(path),
                "dtype": str(leaf.dtype),
                "shape": list(leaf.shape),
                "bytes": int(np.prod(leaf.shape))
                * _dtype_size(str(leaf.dtype)),
                "sharding": str(leaf.sharding),
            }
        )
    return source, specs, mesh, sharding


def _dtype_size(name):
    return {
        "float16": 2,
        "bfloat16": 2,
        "float32": 4,
        "float64": 8,
        "int8": 1,
        "uint8": 1,
        "int16": 2,
        "int32": 4,
        "int64": 8,
    }[name]


def abstract_target(tree, sharding=None):
    import jax

    def make(value):
        if sharding is None:
            return jax.ShapeDtypeStruct(value.shape, value.dtype)
        return jax.ShapeDtypeStruct(value.shape, value.dtype, sharding=sharding)

    return jax.tree_util.tree_map(make, tree)


def flatten_rows(tree):
    import jax
    import numpy as np

    rows = {}
    for path, leaf in jax.tree_util.tree_flatten_with_path(tree)[0]:
        key = jax.tree_util.keystr(path)
        array = np.asarray(jax.device_get(leaf))
        payload = array.tobytes()
        rows[key] = {
            "dtype": str(leaf.dtype),
            "shape": list(leaf.shape),
            "sha256": hashlib.sha256(payload).hexdigest(),
            "bytes": len(payload),
        }
    return rows


def compare_trees(source, restored):
    left = flatten_rows(source)
    right = flatten_rows(restored)
    mismatches = []
    for key in sorted(set(left) | set(right)):
        if key not in left:
            mismatches.append({"path": key, "problem": "extra in restored"})
        elif key not in right:
            mismatches.append({"path": key, "problem": "missing in restored"})
        elif left[key]["dtype"] != right[key]["dtype"]:
            mismatches.append(
                {
                    "path": key,
                    "problem": "dtype",
                    "expected": left[key]["dtype"],
                    "actual": right[key]["dtype"],
                }
            )
        elif left[key]["shape"] != right[key]["shape"]:
            mismatches.append(
                {
                    "path": key,
                    "problem": "shape",
                    "expected": left[key]["shape"],
                    "actual": right[key]["shape"],
                }
            )
        elif left[key]["sha256"] != right[key]["sha256"]:
            mismatches.append(
                {
                    "path": key,
                    "problem": "bytes",
                    "expected": left[key]["sha256"],
                    "actual": right[key]["sha256"],
                }
            )
    return {
        "leaves_checked": len(left),
        "ok": not mismatches,
        "mismatches": mismatches,
        "leaf_sha256": {key: row["sha256"] for key, row in sorted(left.items())},
    }


def compare_one(source_leaf, restored_leaf, path):
    left = flatten_rows({path: source_leaf})
    right = flatten_rows({path: restored_leaf})
    key = next(iter(left))
    left_row = left[key]
    right_row = right[key]
    ok = (
        left_row["dtype"] == right_row["dtype"]
        and left_row["shape"] == right_row["shape"]
        and left_row["sha256"] == right_row["sha256"]
    )
    return {
        "path": path,
        "ok": ok,
        "dtype": right_row["dtype"],
        "shape": right_row["shape"],
        "expected_sha256": left_row["sha256"],
        "actual_sha256": right_row["sha256"],
    }


# ---------------------------------------------------------------------------
# Court


def run_court(receipt):
    import jax
    import jax.numpy as jnp
    import numpy as np
    import orbax.checkpoint as ocp
    import tensorstore
    from jax.sharding import Mesh, NamedSharding, PartitionSpec as P

    from weftcodec import orbax as wo
    from weftcodec.dcp import init_store

    receipt["orbax_available"] = True
    receipt["versions"] = {
        "jax": jax.__version__,
        "jaxlib": jax.lib.__version__,
        "orbax_checkpoint": ocp.__version__,
        "tensorstore": getattr(tensorstore, "__version__", "unknown"),
        "numpy": np.__version__,
        "weft_orbax_adapter": wo.__name__,
    }

    if not CLI.is_file() or not os.access(CLI, os.X_OK):
        raise RuntimeError("weftfs CLI not executable at %s" % CLI)

    root = Path(
        tempfile.mkdtemp(
            prefix="weft-orbax-court-", dir="/work/.docker-cache"
        )
    )
    phases = {}
    started = time.monotonic()

    def phase(name):
        phases[name] = {
            "elapsed_s": round(time.monotonic() - started, 3),
            "rss_kib": rss_kib(),
            "vm_hwm_kib": vm_hwm_kib(),
        }
        print(
            "[orbax-court] phase %-20s elapsed=%8.3fs rss=%s KiB hwm=%s KiB"
            % (
                name,
                phases[name]["elapsed_s"],
                phases[name]["rss_kib"],
                phases[name]["vm_hwm_kib"],
            ),
            flush=True,
        )

    try:
        source, specs, mesh, sharding = build_workload()
        leaf_count = len(specs)
        logical_bytes = sum(spec["bytes"] for spec in specs)
        print(
            "[orbax-court] workload leaves=%d logical=%d bytes devices=%d"
            % (leaf_count, logical_bytes, len(jax.devices())),
            flush=True,
        )
        receipt["workload"] = {
            "description": (
                "one deterministic sharded PyTree: params + optimizer state, "
                "11 leaves, bf16/f16/f32/i8/u8/i32"
            ),
            "devices": len(jax.devices()),
            "device_kind": str(jax.devices()[0].device_kind),
            "mesh": str(mesh),
            "sharding": str(sharding),
            "leaf_count": leaf_count,
            "logical_bytes": logical_bytes,
            "dtypes": sorted({spec["dtype"] for spec in specs}),
            "leaves": specs,
            "seed": "jax.random.PRNGKey(20261106)",
            "synthetic": True,
        }
        phase("source_built")

        # -- Orbax's own checkpoint ------------------------------------------
        standard_dir = root / "orbax-ocdbt"
        standard_handler = ocp.PyTreeCheckpointHandler(
            use_ocdbt=True, use_compression=True
        )
        standard_async = ocp.AsyncCheckpointer(
            standard_handler, async_options=ocp.options.AsyncOptions(timeout_secs=1800)
        )
        try:
            t0 = time.monotonic()
            standard_async.save(
                standard_dir, args=ocp.args.PyTreeSave(item=source)
            )
            save_pause = time.monotonic() - t0
            standard_async.wait_until_finished()
            save_total = time.monotonic() - t0
        finally:
            standard_async.close()
        phase("standard_saved")

        standard_sync = ocp.PyTreeCheckpointer(
            use_ocdbt=True, use_compression=True
        )
        target = abstract_target(source, sharding)
        t0 = time.monotonic()
        restored_standard = standard_sync.restore(
            standard_dir, args=ocp.args.PyTreeRestore(item=target)
        )
        restore_ms = (time.monotonic() - t0) * 1000.0
        standard_exact = compare_trees(source, restored_standard)
        phase("standard_restored")

        subset = {
            "params": {
                "embed": jax.ShapeDtypeStruct(
                    source["params"]["embed"].shape,
                    source["params"]["embed"].dtype,
                    sharding=sharding,
                )
            }
        }
        try:
            t0 = time.monotonic()
            restored_one = standard_sync.restore(
                standard_dir,
                args=ocp.args.PyTreeRestore(item=subset, partial_restore=True),
            )
            single_leaf_ms = (time.monotonic() - t0) * 1000.0
            single_leaf = compare_one(
                source["params"]["embed"],
                restored_one["params"]["embed"],
                "params.embed",
            )
            single_leaf_supported = True
        except Exception as exc:  # honest: Orbax may reject a subset restore
            single_leaf_ms = None
            single_leaf_supported = False
            single_leaf = {
                "ok": False,
                "error_class": type(exc).__name__,
                "error": str(exc)[:1000],
            }
        standard_physical = directory_physical(standard_dir)
        phase("standard_single_leaf")

        # Orbax standard does not honour a different target sharding in 0.12.9;
        # record exactly what happened instead of assuming.
        if len(jax.devices()) >= 2:
            small_mesh = Mesh(np.array(jax.devices()[:2]), ("x",))
            small_sharding = NamedSharding(small_mesh, P("x"))
            small_target = abstract_target(source, small_sharding)
            t0 = time.monotonic()
            try:
                restored_small = standard_sync.restore(
                    standard_dir,
                    args=ocp.args.PyTreeRestore(item=small_target),
                )
                standard_reshard_ms = (time.monotonic() - t0) * 1000.0
                standard_reshard_exact = compare_trees(source, restored_small)
                returned = str(restored_small["params"]["embed"].sharding)
                standard_reshard = {
                    "supported": True,
                    "requested_sharding": str(small_sharding),
                    "returned_sharding": returned,
                    "requested_sharding_honored": returned == str(small_sharding),
                    "restore_ms": round(standard_reshard_ms, 3),
                    "exactness": standard_reshard_exact,
                }
            except Exception as exc:
                standard_reshard = {
                    "supported": False,
                    "error_class": type(exc).__name__,
                    "error": str(exc)[:1000],
                }
        else:
            standard_reshard = {"supported": False, "error": "fewer than 2 devices"}
        standard_sync.close()
        phase("standard_reshard")

        # -- WeftFS adapter -------------------------------------------------
        store = root / "weft-store"
        init_store(store, weftfs_cli=CLI)
        weft_handler = wo.WeftOrbaxHandler(
            weftfs_cli=CLI, staging_bytes=256 * 1024 * 1024, max_workers=1
        )
        weft_dir = root / "weft-orbax"
        weft_async = ocp.AsyncCheckpointer(
            weft_handler, async_options=ocp.options.AsyncOptions(timeout_secs=1800)
        )
        try:
            t0 = time.monotonic()
            weft_async.save(
                weft_dir,
                args=wo.WeftSaveArgs(
                    item=source,
                    store_root=store,
                    checkpoint_id="orbax-court",
                    write_topology=(8, 0),
                    shard_strategy="zstd",
                    init_store=True,
                ),
            )
            weft_save_pause = time.monotonic() - t0
            weft_async.wait_until_finished()
            weft_save_total = time.monotonic() - t0
            weft_result = weft_handler.last_result
        finally:
            weft_async.close()
        phase("weft_saved")
        store_physical_after_save = directory_physical(store)
        rust_checkpoint_after_save = wo.verify_checkpoint(
            store, weft_result.storage_name, full=False
        )
        rust_full_after_save = wo.verify_checkpoint(
            store, weft_result.storage_name, full=True
        )
        phase("rust_verify_after_save")

        weft_pointer = weft_dir / wo.ORBAX_POINTER_NAME
        metadata = wo.metadata_of(weft_dir)
        weft_sync = ocp.Checkpointer(weft_handler)
        t0 = time.monotonic()
        restored_weft = weft_sync.restore(
            weft_dir, args=wo.WeftRestoreArgs(item=target)
        )
        weft_restore_ms = (time.monotonic() - t0) * 1000.0
        weft_exact = compare_trees(source, restored_weft)
        phase("weft_restored")

        t0 = time.monotonic()
        restored_leaf = weft_handler.restore_leaf(weft_dir, "['params']['embed']")
        weft_single_leaf_ms = (time.monotonic() - t0) * 1000.0
        weft_single_leaf = compare_one(
            source["params"]["embed"], restored_leaf, "params.embed"
        )
        phase("weft_single_leaf")

        topology = {}
        for shards in (8, 2):
            t0 = time.monotonic()
            restored = weft_handler.restore(
                weft_dir,
                wo.WeftRestoreArgs(item=target, restore_topology=shards),
            )
            elapsed = (time.monotonic() - t0) * 1000.0
            topology[str(shards)] = {
                "restore_ms": round(elapsed, 3),
                "exactness": compare_trees(source, restored),
            }
        phase("weft_topology")

        weft_reshard = {"supported": False}
        if len(jax.devices()) >= 2:
            t0 = time.monotonic()
            restored_small = weft_handler.restore(
                weft_dir, wo.WeftRestoreArgs(item=small_target)
            )
            elapsed = (time.monotonic() - t0) * 1000.0
            returned = str(restored_small["params"]["embed"].sharding)
            weft_reshard = {
                "supported": True,
                "requested_sharding": str(small_sharding),
                "returned_sharding": returned,
                "requested_sharding_honored": returned == str(small_sharding),
                "restore_ms": round(elapsed, 3),
                "exactness": compare_trees(source, restored_small),
            }
        phase("weft_reshard")

        rust_checkpoint = rust_checkpoint_after_save
        rust_full = rust_full_after_save
        store_physical_after_restores = directory_physical(store)
        rust_full_after_restores = wo.verify_checkpoint(
            store, weft_result.storage_name, full=True
        )
        phase("rust_verify_after_restores")

        standard_dir_physical = standard_physical
        weft_manifest = weft_result.manifest

        results = {
            "orbax_standard": {
                "checkpoint_dir": str(standard_dir),
                "save_pause_ms": round(save_pause * 1000.0, 3),
                "save_total_ms": round(save_total * 1000.0, 3),
                "restore_ms": round(restore_ms, 3),
                "single_leaf_restore_ms": (
                    None if single_leaf_ms is None else round(single_leaf_ms, 3)
                ),
                "single_leaf_supported": single_leaf_supported,
                "single_leaf_exactness": single_leaf,
                "physical": standard_dir_physical,
                "physical_vs_logical": round(
                    standard_dir_physical["bytes"] / logical_bytes, 6
                ),
                "leaf_count": leaf_count,
                "exactness": standard_exact,
                "different_sharding": standard_reshard,
            },
            "weft_orbax": {
                "checkpoint_dir": str(weft_dir),
                "storage_name": weft_result.storage_name,
                "checkpoint_id": weft_result.checkpoint_id,
                "pointer": str(weft_pointer),
                "pointer_bytes": (
                    weft_pointer.stat().st_size if weft_pointer.is_file() else None
                ),
                "save_pause_ms": round(weft_save_pause * 1000.0, 3),
                "save_total_ms": round(weft_save_total * 1000.0, 3),
                "store_command_ms": round(weft_result.store_command_ms, 3),
                "checkpoint_write_ms": round(weft_result.checkpoint_write_ms, 3),
                "restore_ms": round(weft_restore_ms, 3),
                "single_leaf_restore_ms": round(weft_single_leaf_ms, 3),
                "single_leaf_exactness": weft_single_leaf,
                "physical": store_physical_after_save,
                "physical_after_restores": store_physical_after_restores,
                "physical_vs_logical": round(
                    store_physical_after_save["bytes"] / logical_bytes, 6
                ),
                "leaf_count": leaf_count,
                "metadata_leaf_count": metadata.leaf_count,
                "logical_bytes": weft_result.leaf_bytes,
                "manifest": {
                    "source_file_bytes": int(
                        weft_manifest.get("source_file_bytes", 0)
                    ),
                    "source_sha256": weft_manifest.get("source_sha256"),
                    "schema": weft_manifest.get("schema"),
                    "complete": weft_manifest.get("complete"),
                },
                "exactness": weft_exact,
                "different_sharding": weft_reshard,
                "rust_topology_proof": topology,
                "rust_checkpoint_verify": rust_checkpoint,
                "rust_verify_full": rust_full,
                "rust_verify_full_after_restores": rust_full_after_restores,
            },
            "rss": {
                "peak_rss_kib": peak_rss_kib(),
                "vm_hwm_kib": vm_hwm_kib(),
                "phases": phases,
            },
        }
        receipt["results"] = results

        checks = []

        def check(name, ok, detail=None):
            checks.append({"name": name, "ok": bool(ok), "detail": detail})

        check(
            "orbax_importable",
            True,
            "jax %s, orbax-checkpoint %s" % (jax.__version__, ocp.__version__),
        )
        check(
            "standard_save_restore_exact",
            standard_exact["ok"],
            standard_exact["mismatches"][:5],
        )
        check(
            "weft_save_restore_exact",
            weft_exact["ok"],
            weft_exact["mismatches"][:5],
        )
        check(
            "weft_single_leaf_exact",
            weft_single_leaf["ok"],
            weft_single_leaf,
        )
        check(
            "weft_metadata_leaf_count",
            metadata.leaf_count == leaf_count,
            {"metadata": metadata.leaf_count, "workload": leaf_count},
        )
        for shards, row in topology.items():
            check(
                "weft_topology_%s_exact" % shards,
                row["exactness"]["ok"],
                row["exactness"]["mismatches"][:5],
            )
        check(
            "weft_different_sharding_exact",
            weft_reshard.get("supported", False)
            and weft_reshard.get("exactness", {}).get("ok", False),
            weft_reshard,
        )
        check(
            "weft_requested_sharding_honored",
            weft_reshard.get("requested_sharding_honored", False),
            {
                "requested": weft_reshard.get("requested_sharding"),
                "returned": weft_reshard.get("returned_sharding"),
            },
        )
        if standard_reshard.get("supported"):
            check(
                "standard_different_sharding_exact",
                standard_reshard.get("exactness", {}).get("ok", False),
                standard_reshard.get("exactness", {}).get("mismatches", [])[:5],
            )
            receipt["observations"].append(
                {
                    "name": "orbax_standard_requested_sharding",
                    "honored": standard_reshard.get(
                        "requested_sharding_honored", False
                    ),
                    "requested": standard_reshard.get("requested_sharding"),
                    "returned": standard_reshard.get("returned_sharding"),
                    "note": (
                        "Orbax 0.12.9 restores with the saved sharding and warns "
                        "instead of re-sharding to the requested target; this is "
                        "recorded as an observation, not a WeftFS failure."
                    ),
                }
            )
        check(
            "rust_checkpoint_verify",
            rust_checkpoint.get("tensors_verified") == leaf_count,
            rust_checkpoint,
        )
        check(
            "rust_verify_full",
            bool(rust_full.get("full", {}).get("ok")),
            {
                "problems": rust_full.get("full", {}).get("problems"),
                "generation": rust_full.get("full", {}).get("generation"),
            },
        )
        check(
            "rust_verify_full_after_restores",
            bool(rust_full_after_restores.get("full", {}).get("ok")),
            {
                "problems": rust_full_after_restores.get("full", {}).get(
                    "problems"
                ),
                "generation": rust_full_after_restores.get("full", {}).get(
                    "generation"
                ),
            },
        )
        receipt["checks"] = checks

        losses = []
        standard_bytes = standard_dir_physical["bytes"]
        weft_bytes = store_physical_after_save["bytes"]
        losses.append(
            {
                "metric": "physical_bytes",
                "orbax_standard": standard_bytes,
                "weft_orbax": weft_bytes,
                "delta_bytes": weft_bytes - standard_bytes,
                "ratio_weft_over_standard": round(weft_bytes / standard_bytes, 6),
                "note": (
                    "WeftFS bytes are the whole store directory immediately after "
                    "the checkpoint publish (objects, manifests, commits, "
                    "index/lock files, and the Orbax metadata object); Orbax bytes "
                    "are its checkpoint directory. Restored-container imports are "
                    "excluded here and reported separately."
                ),
            }
        )
        losses.append(
            {
                "metric": "restored_container_import_bytes",
                "weft_orbax": store_physical_after_restores["bytes"]
                - store_physical_after_save["bytes"],
                "phase_bytes_after_save": store_physical_after_save["bytes"],
                "phase_bytes_after_restores": store_physical_after_restores["bytes"],
                "note": (
                    "`weftfs checkpoint restore` imports each restored container "
                    "into the store under /dcp-reads/<uuid>; the DCP and Orbax "
                    "adapters reuse that CLI verb, so repeated restores grow the "
                    "store. This is real adapter/CLI overhead and is not included "
                    "in the checkpoint physical-byte comparison above."
                ),
            }
        )
        losses.append(
            {
                "metric": "save_pause_ms",
                "orbax_standard": results["orbax_standard"]["save_pause_ms"],
                "weft_orbax": results["weft_orbax"]["save_pause_ms"],
                "delta_ms": round(
                    results["weft_orbax"]["save_pause_ms"]
                    - results["orbax_standard"]["save_pause_ms"],
                    3,
                ),
            }
        )
        losses.append(
            {
                "metric": "save_total_ms",
                "orbax_standard": results["orbax_standard"]["save_total_ms"],
                "weft_orbax": results["weft_orbax"]["save_total_ms"],
                "delta_ms": round(
                    results["weft_orbax"]["save_total_ms"]
                    - results["orbax_standard"]["save_total_ms"],
                    3,
                ),
            }
        )
        losses.append(
            {
                "metric": "restore_ms",
                "orbax_standard": results["orbax_standard"]["restore_ms"],
                "weft_orbax": results["weft_orbax"]["restore_ms"],
                "delta_ms": round(
                    results["weft_orbax"]["restore_ms"]
                    - results["orbax_standard"]["restore_ms"],
                    3,
                ),
            }
        )
        if single_leaf_supported:
            losses.append(
                {
                    "metric": "single_leaf_restore_ms",
                    "orbax_standard": results["orbax_standard"][
                        "single_leaf_restore_ms"
                    ],
                    "weft_orbax": results["weft_orbax"]["single_leaf_restore_ms"],
                    "delta_ms": round(
                        results["weft_orbax"]["single_leaf_restore_ms"]
                        - results["orbax_standard"]["single_leaf_restore_ms"],
                        3,
                    ),
                }
            )
        else:
            losses.append(
                {
                    "metric": "single_leaf_restore",
                    "orbax_standard": "not supported by this Orbax path",
                    "weft_orbax": results["weft_orbax"]["single_leaf_restore_ms"],
                }
            )
        if standard_reshard.get("supported"):
            losses.append(
                {
                    "metric": "different_sharding_request",
                    "orbax_standard": {
                        "requested": standard_reshard.get("requested_sharding"),
                        "returned": standard_reshard.get("returned_sharding"),
                        "honored": standard_reshard.get(
                            "requested_sharding_honored"
                        ),
                    },
                    "weft_orbax": {
                        "requested": weft_reshard.get("requested_sharding"),
                        "returned": weft_reshard.get("returned_sharding"),
                        "honored": weft_reshard.get("requested_sharding_honored"),
                    },
                    "note": (
                        "Orbax 0.12.9 restores with the sharding recorded in the "
                        "checkpoint and warns about a different topology; the "
                        "WeftFS adapter re-shards the restored leaves into the "
                        "requested target sharding."
                    ),
                }
            )
        losses.append(
            {
                "metric": "staging_copy",
                "weft_orbax": (
                    "the adapter writes a safetensors staging container inside the "
                    "Orbax temporary directory before the Rust CLI ingests it, then "
                    "removes it; this is an extra full-size host write"
                ),
                "orbax_standard": "TensorStore writes its own chunk files",
            }
        )
        receipt["losses"] = losses
        receipt["not_covered"] = [
            "CPU only (8 forced CPU devices in one process); no TPU and no GPU.",
            "Single process and single host; no jax.distributed.initialize, no "
            "multi-host run, and no real distributed JAX.",
            "Orbax's own checkpoint is OCDBT/TensorStore on a local filesystem; "
            "the WeftFS store is a separate format. The comparison measures two "
            "independent implementations, not interchangeable files.",
            "Orbax features not exercised: Zarr3, per-leaf save_args/restore_args, "
            "transforms, custom metadata, step metadata contents, retention/"
            "deletion internals, emergency checkpointing, multi-axis meshes, and "
            "dtypes outside the exact WEFT-TENSOR v1 set.",
            "No process crash, power loss, or media failure during an Orbax save.",
            "Synthetic random weights, warm caches, one run; timings are not a "
            "fleet or production claim.",
            "The adapter's save pause is the time until Orbax's AsyncCheckpointer "
            "returns; the actual copy/store work runs on Orbax's background commit "
            "thread via a bounded worker.",
        ]
        receipt["status"] = "ok" if all(check["ok"] for check in checks) else "failed"
        phase("done")
        receipt["results"]["rss"]["phases"] = phases
        write_receipt(receipt)
        return 0 if receipt["status"] == "ok" else 1
    finally:
        shutil.rmtree(root, ignore_errors=True)


def main():
    receipt = base_receipt()
    try:
        import jax  # noqa: F401
        import orbax.checkpoint  # noqa: F401
        import tensorstore  # noqa: F401
        import numpy  # noqa: F401
        from weftcodec import orbax  # noqa: F401
    except Exception as exc:
        return unavailable(exc)

    try:
        return run_court(receipt)
    except BaseException as exc:
        receipt["status"] = "failed"
        receipt["orbax_available"] = True
        receipt["error"] = {
            "class": type(exc).__name__,
            "message": str(exc),
            "traceback": traceback.format_exc()[-8000:],
        }
        receipt["checks"] = receipt.get("checks", []) + [
            {"name": "court_completed", "ok": False, "detail": str(exc)}
        ]
        receipt["not_covered"] = receipt.get("not_covered", [])
        write_receipt(receipt)
        raise


if __name__ == "__main__":
    sys.exit(main())
