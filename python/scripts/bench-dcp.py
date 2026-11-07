#!/usr/bin/env python3
"""Benchmark the PyTorch DCP adapter in the Docker python court.

8 tensors x 4 MiB. Measures full save, full load, single-tensor load, async
submit/completion vs synchronous save, peak RSS, and physical bytes against a
plain ``torch.save`` baseline of the same tensors. Every WeftFS checkpoint is
verified with the Rust CLI (``checkpoint verify`` + ``verify --full``).

Writes ``bench/receipts/pytorch-dcp-<UTC ISO>.json``. Synthetic single-host CPU
fixtures; no GPU, no cluster, no media-durability claim.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

import torch
import torch.distributed.checkpoint as dcp

from weftcodec.dcp import (
    DEFAULT_CLI,
    WeftStorageReader,
    WeftStorageWriter,
    _Storage,
    _parse_safetensors,
    init_store,
    save_checkpoint_async,
    verify_checkpoint,
)

TENSOR_COUNT = 8
TENSOR_ELEMENTS = 1024 * 1024  # 4 MiB at f32
SHAPE = (8, TENSOR_ELEMENTS // 8)


def _utc_stamp():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _stamp_compact():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def _rss_kib():
    try:
        with open("/proc/self/status", "r", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except OSError:
        pass
    import resource

    return int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)


def _store_bytes(root: Path):
    total = 0
    files = 0
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            path = Path(dirpath) / name
            try:
                total += path.stat().st_size
                files += 1
            except OSError:
                continue
    return total, files


def _median(values):
    return statistics.median(values) if values else None


def _timed(fn, reps):
    samples = []
    result = None
    for _ in range(reps):
        started = time.perf_counter()
        result = fn()
        samples.append((time.perf_counter() - started) * 1000.0)
    return {
        "samples_ms": [round(value, 3) for value in samples],
        "median_ms": round(_median(samples), 3) if samples else None,
        "min_ms": round(min(samples), 3) if samples else None,
        "max_ms": round(max(samples), 3) if samples else None,
    }, result


def _state():
    generator = torch.Generator().manual_seed(20261105)
    return {
        "layer%02d.weight" % index: torch.randn(SHAPE, generator=generator)
        for index in range(TENSOR_COUNT)
    }


def _check_exact(expected, actual):
    if set(expected) != set(actual):
        raise AssertionError("tensor names differ")
    for name in expected:
        if not torch.equal(expected[name], actual[name]):
            raise AssertionError("tensor %s differs" % name)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--receipt", default=None)
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--single-reps", type=int, default=5)
    parser.add_argument("--staging-mib", type=int, default=64)
    args = parser.parse_args()

    stamp = _stamp_compact()
    receipt_path = Path(
        args.receipt
        or "bench/receipts/pytorch-dcp-%s.json" % stamp
    )
    receipt_path.parent.mkdir(parents=True, exist_ok=True)

    cli = os.environ.get("WEFTFS_CLI", DEFAULT_CLI)
    work = Path(tempfile.mkdtemp(prefix="weft-dcp-bench-"))
    store = work / "store"
    init_store(store, weftfs_cli=cli)
    state = _state()
    source_bytes = sum(tensor.numel() * tensor.element_size() for tensor in state.values())
    staging_bytes = args.staging_mib * 1024 * 1024

    phases = {}
    manifests = {}

    def save(config_id, topology, strategy):
        writer = WeftStorageWriter(
            config_id,
            store_root=store,
            topology=topology,
            shard_strategy=strategy,
            staging_bytes=staging_bytes,
            weftfs_cli=cli,
        )
        metadata = dcp.save(state, storage_writer=writer, no_dist=True)
        return writer, metadata

    # ---- primary configuration: topology 8, zstd shards ----------------
    primary = "weft-dcp-bench-zstd"
    primary_writer = None
    primary_metadata = None
    save_samples = []
    physical_after_save = None
    file_count_after_save = None

    for rep in range(args.reps):
        started = time.perf_counter()
        writer, metadata = save(primary, 8, "zstd")
        save_samples.append((time.perf_counter() - started) * 1000.0)
        if rep == 0:
            primary_writer = writer
            primary_metadata = metadata
            physical_after_save, file_count_after_save = _store_bytes(store)
    phases["full_save_ms"] = {
        "samples_ms": [round(value, 3) for value in save_samples],
        "median_ms": round(_median(save_samples), 3),
        "min_ms": round(min(save_samples), 3),
        "max_ms": round(max(save_samples), 3),
    }
    phases["rss_after_save_kib"] = _rss_kib()
    assert primary_writer.stats["entries"] == TENSOR_COUNT
    assert primary_metadata is not None
    assert primary_writer.stats["staging_peak_bytes"] <= staging_bytes

    reader = WeftStorageReader(primary, store_root=store, weftfs_cli=cli)
    reader.read_metadata()
    show = reader._store.checkpoint_show(reader.storage_name)["manifest"]
    manifests["primary"] = {
        "checkpoint": reader.storage_name,
        "tensors": len(show["tensors"]),
        "shards_per_tensor": [len(row["shards"]) for row in show["tensors"]],
        "manifest_root": reader._store.checkpoint_show(reader.storage_name)["manifest_root"],
        "source_file_bytes": show["source_file_bytes"],
        "element_bytes": show["element_bytes"],
    }

    verification = verify_checkpoint(
        store, reader.storage_name, full=True, weftfs_cli=cli
    )
    phases["rust_verification"] = {
        "checkpoint": reader.storage_name,
        "tensors_verified": verification["tensors_verified"],
        "verify_full_ok": verification["full"]["ok"],
        "problems": verification["full"]["problems"],
    }

    # ---- secondary configuration: topology 8, mode-10 tensor shards ----
    # Measured before any restore so the delta is the checkpoint itself.
    secondary = "weft-dcp-bench-tensor"
    secondary_writer = None
    before_secondary, before_secondary_files = _store_bytes(store)

    def secondary_save():
        nonlocal secondary_writer
        secondary_writer, _ = save(secondary, 8, "tensor-sharded")

    phases["tensor_sharded_save_ms"], _ = _timed(secondary_save, 1)
    secondary_physical, secondary_files = _store_bytes(store)
    phases["tensor_sharded_store_bytes"] = {
        "physical_bytes": secondary_physical - before_secondary,
        "files": secondary_files - before_secondary_files,
        "note": "incremental over the primary checkpoint, before any restore",
    }
    secondary_reader = WeftStorageReader(secondary, store_root=store, weftfs_cli=cli)
    secondary_reader.read_metadata()
    phases["tensor_sharded_verification"] = verify_checkpoint(
        store, secondary_reader.storage_name, full=True, weftfs_cli=cli
    )["tensors_verified"]

    # The store must not grow from a restore: nothing is imported.
    before_restores, before_restores_files = _store_bytes(store)

    def full_load():
        target = {name: torch.empty_like(tensor) for name, tensor in state.items()}
        dcp.load(target, storage_reader=WeftStorageReader(primary, store_root=store,
                                                          weftfs_cli=cli), no_dist=True)
        _check_exact(state, target)

    phases["full_load_ms"], _ = _timed(full_load, args.reps)
    phases["rss_after_full_load_kib"] = _rss_kib()

    single_name = "layer03.weight"

    def single_load():
        target = {single_name: torch.empty_like(state[single_name])}
        dcp.load(target, storage_reader=WeftStorageReader(primary, store_root=store,
                                                          weftfs_cli=cli), no_dist=True)
        _check_exact({single_name: state[single_name]}, target)

    phases["single_tensor_load_ms"], _ = _timed(single_load, args.single_reps)
    phases["rss_after_single_load_kib"] = _rss_kib()
    after_restores, after_restores_files = _store_bytes(store)
    load_reader = WeftStorageReader(primary, store_root=store, weftfs_cli=cli)
    load_reader.read_metadata()
    phases["restore_store_growth"] = {
        "bytes_before": before_restores,
        "bytes_after": after_restores,
        "growth_bytes": after_restores - before_restores,
        "files_before": before_restores_files,
        "files_after": after_restores_files,
        "growth_files": after_restores_files - before_restores_files,
    }
    phases["adapter_stats"] = {
        "writer": primary_writer.stats,
        "reader": load_reader.stats,
        "load_reader_cli_calls": load_reader._store.calls,
        "writer_cli_calls": primary_writer._store.calls,
    }

    # ---- control: the pre-service path, same court, same fixture --------
    # This is exactly what the adapter did before S3: a safetensors staging
    # file on disk, one CLI process per operation, and a restored container
    # parsed on the host. It is a control, not the current path.
    controls = {}
    control_dir = Path(tempfile.mkdtemp(prefix="weft-dcp-control-"))

    def _entry_for(fqn):
        for key, info in primary_metadata.storage_data.items():
            if key.fqn == fqn:
                return info.entry
        raise KeyError(fqn)

    control_staging = control_dir / "checkpoint.safetensors"
    legacy = _Storage(store, cli=cli, timeout=600)
    legacy.restore(primary_writer.store_name, host_out=control_staging)
    legacy_calls_before = legacy.calls
    started = time.perf_counter()
    legacy.checkpoint_write(
        control_staging, "weft-dcp-control", shards=8, axis=0, strategy="zstd"
    )
    controls["save_ms"] = round((time.perf_counter() - started) * 1000.0, 3)
    controls["save_cli_calls"] = legacy.calls - legacy_calls_before

    def legacy_full_load():
        target_path = control_dir / "full.safetensors"
        legacy.restore(primary_writer.store_name, host_out=target_path)
        parsed = _parse_safetensors(target_path)
        assert len(parsed) == TENSOR_COUNT
        for name, (_dtype, _shape, payload) in parsed.items():
            actual = torch.frombuffer(bytearray(payload), dtype=torch.uint8)
            expected = state[name].contiguous().view(torch.uint8).reshape(-1)
            assert torch.equal(actual, expected), name

    controls["full_load_ms"], _ = _timed(legacy_full_load, 1)

    def legacy_single_load():
        target_path = control_dir / "single.safetensors"
        legacy.restore(
            primary_writer.store_name,
            entry=_entry_for("layer03.weight"),
            host_out=target_path,
        )
        parsed = _parse_safetensors(target_path)
        assert len(parsed) == 1

    controls["single_load_ms"], _ = _timed(legacy_single_load, 1)
    controls["cli_calls_total"] = legacy.calls
    controls["cli_ms_total"] = round(legacy.command_ms, 3)

    # ---- synchronous vs asynchronous save wall -------------------------
    sync_ms, _ = _timed(
        lambda: save("weft-dcp-bench-sync", 8, "zstd"), args.reps
    )
    phases["synchronous_save_ms"] = sync_ms

    async_submit = []
    async_completion = []
    async_staging_peak = []
    for _ in range(args.reps):
        started = time.perf_counter()
        handle = save_checkpoint_async(
            state,
            checkpoint_id="weft-dcp-bench-async",
            store_root=store,
            topology=8,
            shard_strategy="zstd",
            staging_bytes=staging_bytes,
            weftfs_cli=cli,
        )
        async_submit.append((time.perf_counter() - started) * 1000.0)
        handle.result(timeout=600)
        async_completion.append((time.perf_counter() - started) * 1000.0)
        async_staging_peak.append(handle.stats["staging_peak_bytes"])
    phases["async_save_submit_ms"] = {
        "samples_ms": [round(value, 3) for value in async_submit],
        "median_ms": round(_median(async_submit), 3),
    }
    phases["async_save_completion_ms"] = {
        "samples_ms": [round(value, 3) for value in async_completion],
        "median_ms": round(_median(async_completion), 3),
    }
    phases["async_staging_peak_bytes"] = max(async_staging_peak)
    phases["rss_after_async_kib"] = _rss_kib()

    # ---- plain torch.save baseline -------------------------------------
    baseline = work / "baseline.pt"

    def torch_save():
        torch.save(state, baseline)

    phases["torch_save_ms"], _ = _timed(torch_save, args.reps)
    baseline_bytes = baseline.stat().st_size

    def torch_load():
        loaded = torch.load(baseline, map_location="cpu", weights_only=True)
        _check_exact(state, loaded)

    phases["torch_load_ms"], _ = _timed(torch_load, args.reps)
    phases["rss_after_torch_baseline_kib"] = _rss_kib()
    phases["legacy_cli_control"] = controls

    final_physical, final_files = _store_bytes(store)

    receipt = {
        "receipt": "weftfs-pytorch-dcp/1",
        "generated_at_utc": _utc_stamp(),
        "label": (
            "PyTorch Distributed Checkpoint adapter over a WeftFS store, CPU only, "
            "single host, synthetic 8 x 4 MiB f32 tensors. Save and load run "
            "through one persistent in-process checkpoint service (no per-op CLI "
            "process); the Rust CLI is used only as the verification oracle. "
            "Restores decode straight into caller-owned memfds and never import "
            "into the store. No GPU-direct path, no multi-rank cluster run, no "
            "durability claim."
        ),
        "environment": {
            "revision_weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "revision_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
            "revision_weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
            "revision_weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
            "court_image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
            "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
            "court_kernel": os.environ.get("WEFT_COURT_KERNEL", platform.release()),
            "court_memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
            "court_cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
            "court_pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
            "court_tmpfs": "/tmp:rw,size=1g,nosuid,nodev",
            "court_cap_drop": "ALL",
            "court_no_new_privileges": True,
            "docker_client": os.environ.get("WEFT_DOCKER_CLIENT", "unknown"),
            "docker_server": os.environ.get("WEFT_DOCKER_SERVER", "unknown"),
            "python_version": platform.python_version(),
            "torch_version": torch.__version__,
            "cuda_available": bool(torch.cuda.is_available()),
            "weftfs_cli": cli,
        },
        "fixture": {
            "tensors": TENSOR_COUNT,
            "shape": list(SHAPE),
            "dtype": "torch.float32",
            "elements_per_tensor": TENSOR_ELEMENTS,
            "bytes_per_tensor": TENSOR_ELEMENTS * 4,
            "source_bytes": source_bytes,
            "staging_bytes": staging_bytes,
            "topology": [8, 0],
            "reps": args.reps,
            "single_reps": args.single_reps,
        },
        "weftfs": {
            "store_dir": "temporary directory inside the court (not committed)",
            "checkpoint": manifests["primary"],
            "store_bytes_after_first_save": physical_after_save,
            "store_files_after_first_save": file_count_after_save,
            "store_bytes_final": final_physical,
            "store_files_final": final_files,
            "note": (
                "Final store bytes include every timing repetition (each save is a "
                "new <id>.vN generation). Restores add nothing: see "
                "phases.restore_store_growth."
            ),
        },
        "baseline": {
            "kind": "torch.save(state_dict, file) on the same tensors",
            "file_bytes": baseline_bytes,
            "weftfs_over_baseline_bytes": physical_after_save - baseline_bytes,
            "weftfs_over_baseline_ratio": round(physical_after_save / baseline_bytes, 4),
        },
        "phases": phases,
        "rss_peak_kib": _rss_kib(),
        "commands": [
            "scripts/docker-court.sh python \"python python/scripts/bench-dcp.py\"",
            "weftfs --store <store> checkpoint verify --name <checkpoint> --json",
            "weftfs --store <store> verify --full --json",
        ],
        "not_claimed": [
            "GPU-direct or zero-copy staging (tensors are copied through host memory)",
            "multi-rank distributed training or a cluster test",
            "stock PyTorch DCP binaries; the adapter implements the documented "
            "StorageWriter/StorageReader interfaces",
            "media power loss or host cache loss durability",
        ],
    }

    with open(receipt_path, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(json.dumps(receipt, indent=2, sort_keys=True))
    print("receipt=%s" % receipt_path)
    shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
