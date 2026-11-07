#!/usr/bin/env python3
"""Checkpoint-service court.

Measures the persistent service against the CLI-per-call oracle on the same
store, same tensors, same court:

* one-leaf restore latency (service vs spawn-the-CLI),
* whole-checkpoint restore latency (all leaves, service, vs one CLI container),
* checkpoint write latency (memfd container vs staging file + CLI),
* manifest latency,
* request/process counts from the service's own counters,
* store bytes/generation before and after 100 restores (must be unchanged),
* per-leaf SHA-256 equality between both paths.

Synthetic fixture, single host, warm cache. Writes
``bench/receipts/checkpoint-service-<stamp>.json``.
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import resource
import shutil
import statistics
import struct
import subprocess
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

from weftcodec.dcp import _Storage, _parse_safetensors
from weftcodec.service import CheckpointService

CLI = Path(
    os.environ.get("WEFTFS_CLI", "/work/.docker-cache/target-rust/release/weftfs")
)

TENSOR_COUNT = 32
ELEMENT_BYTES = 1024 * 1024  # 1 MiB per tensor
SHAPE = [1024, 256]  # 1 MiB at f32
CHECKPOINT = "svc-court"


def utc_stamp():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def now_ms():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def median(values):
    return statistics.median(values)


def rss_kib():
    return int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)


def tree_bytes(root):
    total = 0
    files = 0
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            try:
                total += os.path.getsize(os.path.join(dirpath, name))
                files += 1
            except OSError:
                pass
    return {"bytes": total, "files": files}


def payload(index):
    state = (index + 1) * 0x9E3779B97F4A7C15
    state &= 0xFFFFFFFFFFFFFFFF
    out = bytearray()
    block = state.to_bytes(8, "little")
    while len(out) < ELEMENT_BYTES:
        out += block
        state = (state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        block = state.to_bytes(8, "little")
    return bytes(out[:ELEMENT_BYTES])


def container(payloads):
    header = {}
    cursor = 0
    for name, data in payloads:
        header[name] = {
            "dtype": "F32",
            "shape": SHAPE,
            "data_offsets": [cursor, cursor + len(data)],
        }
        cursor += len(data)
    raw = json.dumps(header, separators=(",", ":")).encode("utf-8")
    while len(raw) % 8:
        raw += b" "
    out = bytearray(struct.pack("<Q", len(raw)))
    out += raw
    for _name, data in payloads:
        out += data
    return bytes(out)


def run():
    stamp = os.environ.get("WEFT_SERVICE_STAMP") or utc_stamp()
    receipt_path = Path("bench/receipts/checkpoint-service-%s.json" % stamp)
    receipt_path.parent.mkdir(parents=True, exist_ok=True)

    work = Path(tempfile.mkdtemp(prefix="weft-svc-court-"))
    store = work / "store"
    checks = []

    def check(name, ok, detail=None):
        checks.append({"name": name, "ok": bool(ok), "detail": detail})

    subprocess.run(
        [os.fspath(CLI), "--store", os.fspath(store), "init"],
        check=True,
        capture_output=True,
    )

    payloads = [("tensor_%02d" % index, payload(index)) for index in range(TENSOR_COUNT)]
    names = [name for name, _data in payloads]
    expected = {name: hashlib.sha256(data).hexdigest() for name, data in payloads}
    image = container(payloads)

    results = {}

    with CheckpointService(store, cli=CLI, max_tensor_bytes=64 << 20) as service:
        # -- write (memfd) vs staging file + CLI --------------------------
        started = time.perf_counter()
        report = service.write_checkpoint(
            CHECKPOINT, image, shards=4, axis=0, strategy="zstd"
        )
        service_write_ms = (time.perf_counter() - started) * 1000.0
        check("service_write_complete", report.complete)
        check("service_write_tensor_count", report.tensor_count == TENSOR_COUNT)
        check(
            "service_write_source_sha",
            report.source_sha256 == hashlib.sha256(image).hexdigest(),
        )

        staging = work / "staging.safetensors"
        staging.write_bytes(image)
        legacy = _Storage(store, cli=CLI, timeout=600)
        legacy_calls_before = legacy.calls
        started = time.perf_counter()
        legacy.checkpoint_write(
            staging, "svc-court-cli", shards=4, axis=0, strategy="zstd"
        )
        cli_write_ms = (time.perf_counter() - started) * 1000.0
        cli_write_calls = legacy.calls - legacy_calls_before

        # -- manifest -----------------------------------------------------
        manifest_ms = []
        for _ in range(10):
            started = time.perf_counter()
            manifest = service.manifest(CHECKPOINT)
            manifest_ms.append((time.perf_counter() - started) * 1000.0)
        check("manifest_rows", len(manifest.tensors) == TENSOR_COUNT)
        check(
            "manifest_element_bytes",
            manifest.element_bytes == TENSOR_COUNT * ELEMENT_BYTES,
        )

        # -- one-leaf restore: service vs CLI -----------------------------
        def service_leaf(name):
            buffer = service.restore_tensor(
                CHECKPOINT, name, expected_size=ELEMENT_BYTES
            )
            try:
                return hashlib.sha256(buffer.view).hexdigest()
            finally:
                buffer.close()

        def cli_leaf(name):
            out = work / "leaf.safetensors"
            legacy.restore(CHECKPOINT, entry=name, host_out=out)
            parsed = _parse_safetensors(out)
            return hashlib.sha256(parsed[name][2]).hexdigest()

        service_leaf_ms = []
        cli_leaf_ms = []
        for index in range(20):
            name = names[index % TENSOR_COUNT]
            started = time.perf_counter()
            digest = service_leaf(name)
            service_leaf_ms.append((time.perf_counter() - started) * 1000.0)
            assert digest == expected[name], name
            started = time.perf_counter()
            digest = cli_leaf(name)
            cli_leaf_ms.append((time.perf_counter() - started) * 1000.0)
            assert digest == expected[name], name
        check("service_leaf_exact", True)
        check("cli_leaf_exact", True)

        # -- whole checkpoint: service per leaf vs one CLI container ------
        def service_all():
            digests = {}
            for name in names:
                buffer = service.restore_tensor(
                    CHECKPOINT, name, expected_size=ELEMENT_BYTES
                )
                try:
                    digests[name] = hashlib.sha256(buffer.view).hexdigest()
                finally:
                    buffer.close()
            return digests

        def cli_all():
            out = work / "all.safetensors"
            legacy.restore(CHECKPOINT, host_out=out)
            parsed = _parse_safetensors(out)
            return {
                name: hashlib.sha256(parsed[name][2]).hexdigest() for name in names
            }

        service_all_ms = []
        cli_all_ms = []
        for _ in range(3):
            started = time.perf_counter()
            digests = service_all()
            service_all_ms.append((time.perf_counter() - started) * 1000.0)
            assert digests == expected
            started = time.perf_counter()
            digests = cli_all()
            cli_all_ms.append((time.perf_counter() - started) * 1000.0)
            assert digests == expected
        check("service_all_exact", True)
        check("cli_all_exact", True)

        # -- restore must not mutate the store ----------------------------
        before = tree_bytes(store)
        info_before = service.info()
        for _ in range(100):
            buffer = service.restore_tensor(
                CHECKPOINT, names[0], expected_size=ELEMENT_BYTES
            )
            buffer.close()
        after = tree_bytes(store)
        info_after = service.info()
        check(
            "restore_does_not_mutate",
            before == after and info_before["generation"] == info_after["generation"],
            {"before": before, "after": after},
        )

        stats = dict(service.stats)
        results["service"] = {
            "stats": stats,
            "manifest_ms": {
                "samples_ms": [round(v, 3) for v in manifest_ms],
                "median_ms": round(median(manifest_ms), 3),
            },
        }

    results.update(
        {
            "write_checkpoint_ms": {
                "service_memfd_ms": round(service_write_ms, 3),
                "cli_staging_file_ms": round(cli_write_ms, 3),
                "cli_processes": cli_write_calls,
            },
            "restore_one_leaf_ms": {
                "service_samples_ms": [round(v, 3) for v in service_leaf_ms],
                "service_median_ms": round(median(service_leaf_ms), 3),
                "cli_samples_ms": [round(v, 3) for v in cli_leaf_ms],
                "cli_median_ms": round(median(cli_leaf_ms), 3),
                "ratio_cli_over_service": round(
                    median(cli_leaf_ms) / median(service_leaf_ms), 3
                ),
            },
            "restore_whole_checkpoint_ms": {
                "service_samples_ms": [round(v, 3) for v in service_all_ms],
                "service_median_ms": round(median(service_all_ms), 3),
                "cli_samples_ms": [round(v, 3) for v in cli_all_ms],
                "cli_median_ms": round(median(cli_all_ms), 3),
                "ratio_cli_over_service": round(
                    median(cli_all_ms) / median(service_all_ms), 3
                ),
            },
            "store": {
                "bytes_before": before,
                "bytes_after": after,
                "generation_before": info_before["generation"],
                "generation_after": info_after["generation"],
                "physical_after_write": tree_bytes(store),
            },
        }
    )

    check(
        "service_one_leaf_faster_than_cli",
        results["restore_one_leaf_ms"]["service_median_ms"]
        < results["restore_one_leaf_ms"]["cli_median_ms"],
        results["restore_one_leaf_ms"],
    )

    receipt = {
        "receipt": "weftfs-checkpoint-service/1",
        "generated_at_utc": now_ms(),
        "label": (
            "Persistent checkpoint service court: one store, %d x 1 MiB f32 "
            "tensors, 4-way zstd shards. The service path is one long-lived "
            "helper with one read handle and memfd payloads; the CLI path is one "
            "process per operation plus a host staging file. Single host, warm "
            "cache, synthetic data." % TENSOR_COUNT
        ),
        "environment": {
            "python_version": platform.python_version(),
            "platform": platform.platform(),
            "weftfs_cli": os.fspath(CLI),
            "weftfs_version": subprocess.run(
                [os.fspath(CLI), "--version"], capture_output=True, text=True
            ).stdout.strip(),
            "court_image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
            "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
            "court_kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
            "court_memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
            "court_cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
            "court_pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
            "court_cap_drop": "ALL",
            "court_no_new_privileges": True,
            "revision_weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "revision_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
            "peak_rss_kib": rss_kib(),
        },
        "fixture": {
            "tensors": TENSOR_COUNT,
            "shape": SHAPE,
            "dtype": "f32",
            "bytes_per_tensor": ELEMENT_BYTES,
            "logical_bytes": TENSOR_COUNT * ELEMENT_BYTES,
            "container_bytes": len(image),
            "shards": 4,
            "shard_strategy": "zstd",
            "synthetic": True,
        },
        "results": results,
        "checks": checks,
        "not_claimed": [
            "no GPU, multi-host, fleet, or cloud run",
            "no media power-loss or host cache claim",
            "warm cache; a cold store open is measured separately by the store bench",
            "the CLI path here includes a host staging file, which is the pre-S3 code path",
        ],
    }
    with open(receipt_path, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(json.dumps({"receipt": str(receipt_path), "checks": len(checks),
                      "failed": sum(1 for c in checks if not c["ok"])}, indent=2))
    shutil.rmtree(work, ignore_errors=True)
    return 0 if all(c["ok"] for c in checks) else 1


if __name__ == "__main__":
    raise SystemExit(run())
