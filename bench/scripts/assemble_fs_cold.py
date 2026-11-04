#!/usr/bin/env python3
"""Assemble the cold-cache fio receipt from guest-extracted raw files.

Inputs are the directories written by vm_fs_court.sh:
  <raw-dir>/<target>/status-<target>.txt
  <raw-dir>/<target>/commands-<target>.txt
  <raw-dir>/<target>/*.json          (raw fio JSON, one per run)
  <raw-dir>/<target>/serial.log
  <work-dir>/targets.txt             (target accel exit)

Every raw fio percentile and IOPS value is retained per run. Group medians are
computed from the per-run values; no run is dropped.
"""

import argparse
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import time
from pathlib import Path

WORKLOADS = ["seq-write-1m", "seq-read-1m", "rand-read-4k", "rand-write-4k", "fsync-write-4k"]


def stats(values):
    values = [v for v in values if isinstance(v, (int, float))]
    if not values:
        return None
    values = sorted(values)

    def pct(p):
        if len(values) == 1:
            return values[0]
        rank = int(round((p / 100.0) * (len(values) - 1)))
        return values[max(0, min(len(values) - 1, rank))]

    return {
        "samples": len(values),
        "median": statistics.median(values),
        "min": values[0],
        "p95": pct(95),
        "max": values[-1],
        "raw": values,
    }


def first_percentile(block, key):
    if not block:
        return None
    pct = block.get("percentile") or {}
    for name in (key, f"{float(key):.6f}", f"{float(key):.1f}"):
        if name in pct:
            return pct[name]
    # fio emits keys like "50.000000"; fall back to a numeric comparison.
    for k, v in pct.items():
        try:
            if abs(float(k) - float(key)) < 1e-6:
                return v
        except ValueError:
            continue
    return None


def summarize_fio(path):
    data = json.loads(path.read_text())
    jobs = data.get("jobs") or []
    if not jobs:
        return {"error": "no jobs in fio JSON"}
    job = jobs[0]
    direction = "read" if job.get("read", {}).get("iops", 0) > job.get("write", {}).get("iops", 0) else "write"
    block = job.get(direction) or {}
    clat = block.get("clat_ns") or {}
    lat = block.get("lat_ns") or {}
    sync = (job.get("sync") or {}).get("lat_ns") or {}
    return {
        "fio_version": data.get("fio version"),
        "direction": direction,
        "error": job.get("error"),
        "iops": block.get("iops"),
        "bw_bytes": block.get("bw_bytes"),
        "bw_kib_s": block.get("bw"),
        "runtime_ms": job.get("job_runtime"),
        "clat_p50_ns": first_percentile(clat, 50),
        "clat_p95_ns": first_percentile(clat, 95),
        "clat_p99_ns": first_percentile(clat, 99),
        "clat_mean_ns": clat.get("mean"),
        "lat_p50_ns": first_percentile(lat, 50),
        "lat_p95_ns": first_percentile(lat, 95),
        "lat_p99_ns": first_percentile(lat, 99),
        "sync_p50_ns": first_percentile(sync, 50),
        "sync_p95_ns": first_percentile(sync, 95),
        "sync_p99_ns": first_percentile(sync, 99),
        "usr_cpu_pct": job.get("usr_cpu"),
        "sys_cpu_pct": job.get("sys_cpu"),
        "file": path.name,
    }


def parse_name(name):
    # <target>--<workload>--nj<N>--<cold|warm>-r<rep>.json
    # Targets such as zfs-whole contain hyphens, so split on the "--"
    # separators rather than matching a hyphen-free target.
    stem = name[:-5] if name.endswith(".json") else name
    parts = stem.split("--")
    if len(parts) != 4:
        return None
    target, workload, nj_part, mode_part = parts
    match = re.match(r"^nj(?P<nj>\d+)$", nj_part)
    mode = re.match(r"^(?P<mode>cold|warm)-r(?P<rep>\d+)$", mode_part)
    if not target or not workload or not match or not mode:
        return None
    return {
        "target": target,
        "workload": workload,
        "nj": match.group("nj"),
        "mode": mode.group("mode"),
        "rep": mode.group("rep"),
    }


def read_text(path):
    try:
        return path.read_text(errors="replace")
    except OSError:
        return ""


def command_version(cmd):
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=15)
        text = (out.stdout or out.stderr).strip().splitlines()
        return text[0] if text else None
    except (OSError, subprocess.SubprocessError):
        return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--raw-dir", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--work-dir", required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--targets", required=True)
    parser.add_argument("--reps", type=int, required=True)
    parser.add_argument("--timeout", type=int, required=True)
    parser.add_argument("--xfs-status", required=True)
    parser.add_argument("--btrfs-status", default="0")
    parser.add_argument("--zfs-status", required=True)
    args = parser.parse_args()

    raw_dir = Path(args.raw_dir)
    work_dir = Path(args.work_dir)
    target_meta = {}
    for line in read_text(work_dir / "targets.txt").splitlines():
        parts = line.split()
        if len(parts) == 3:
            target_meta[parts[0]] = {"accel": parts[1], "exit_code": int(parts[2])}

    targets = [t for t in args.targets.split(",") if t]
    results = []
    blockers = []
    target_info = {}

    for target in targets:
        tdir = raw_dir / target
        status = read_text(tdir / f"status-{target}.txt")
        commands = read_text(tdir / f"commands-{target}.txt")
        serial = read_text(tdir / "serial.log")
        info = {
            "target": target,
            "extracted_dir": str(tdir),
            "status_lines": [line for line in status.splitlines() if line.strip()],
            "commands": [line for line in commands.splitlines() if line.strip()],
            "serial_log": str(tdir / "serial.log"),
            "accel": target_meta.get(target, {}).get("accel", "none"),
            "exit_code": target_meta.get(target, {}).get("exit_code", 255),
            "fio_runs": 0,
        }
        for line in status.splitlines():
            if line.startswith("blocker="):
                blockers.append({"target": target, "blocker": line.split("=", 1)[1], "detail": status})
        target_info[target] = info

        grouped = {}
        for path in sorted(tdir.glob("*.json")):
            parsed = parse_name(path.name)
            if not parsed:
                continue
            summary = summarize_fio(path)
            info["fio_runs"] += 1
            key = (parsed["workload"], int(parsed["nj"]))
            grouped.setdefault(key, {"cold": [], "warm": []})
            summary["rep"] = int(parsed["rep"])
            grouped[key][parsed["mode"]].append(summary)

        for (workload, nj), modes in sorted(grouped.items()):
            row = {
                "target": target,
                "workload": workload,
                "numjobs": nj,
                "cold_runs": modes["cold"],
                "warm_runs": modes["warm"],
            }
            for mode in ("cold", "warm"):
                runs = modes[mode]
                row[f"{mode}_summary"] = {
                    "iops": stats([r.get("iops") for r in runs]),
                    "bw_bytes": stats([r.get("bw_bytes") for r in runs]),
                    "clat_p50_ns": stats([r.get("clat_p50_ns") for r in runs]),
                    "clat_p95_ns": stats([r.get("clat_p95_ns") for r in runs]),
                    "clat_p99_ns": stats([r.get("clat_p99_ns") for r in runs]),
                    "lat_p50_ns": stats([r.get("lat_p50_ns") for r in runs]),
                    "lat_p95_ns": stats([r.get("lat_p95_ns") for r in runs]),
                    "lat_p99_ns": stats([r.get("lat_p99_ns") for r in runs]),
                    "sync_p50_ns": stats([r.get("sync_p50_ns") for r in runs]),
                    "sync_p95_ns": stats([r.get("sync_p95_ns") for r in runs]),
                    "sync_p99_ns": stats([r.get("sync_p99_ns") for r in runs]),
                    "usr_cpu_pct": stats([r.get("usr_cpu_pct") for r in runs]),
                    "sys_cpu_pct": stats([r.get("sys_cpu_pct") for r in runs]),
                }
            results.append(row)

    report = {
        "schema": "weft-fs-cold-bench-v1",
        "kind": "cold-cache-concurrent-fio",
        "generated_at_unix": int(time.time()),
        "environment": {
            "host": {
                "kernel": platform.release(),
                "uname": platform.uname()._asdict(),
                "cpu_model": next(
                    (line.split(":", 1)[1].strip() for line in read_text(Path("/proc/cpuinfo")).splitlines() if line.startswith("model name")),
                    None,
                ),
                "nproc": os.cpu_count(),
                "mem_total_kib": next(
                    (int(line.split()[1]) for line in read_text(Path("/proc/meminfo")).splitlines() if line.startswith("MemTotal:")),
                    None,
                ),
                "court_image": os.environ.get("WEFT_COURT_IMAGE"),
                "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID"),
                "court_memory": os.environ.get("WEFT_COURT_MEMORY"),
                "court_cpus": os.environ.get("WEFT_COURT_CPUS"),
                "court_pids": os.environ.get("WEFT_COURT_PIDS"),
                "revision_weftfs": os.environ.get("WEFT_REPO_WEFTFS"),
                "revision_weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO"),
                "revision_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC"),
            },
            "tools": {
                "qemu": command_version(["qemu-system-x86_64", "--version"]),
                "fio": command_version(["fio", "--version"]),
                "mkfs_xfs": command_version(["mkfs.xfs", "-V"]),
                "guest_kernel": next(
                    (line.split("=", 1)[1] for line in read_text(Path(args.raw_dir) / targets[0] / f"status-{targets[0]}.txt").splitlines() if line.startswith("kernel=")),
                    None,
                ),
            },
            "xfs_available": args.xfs_status == "1",
            "btrfs": args.btrfs_status,
            "zfs": args.zfs_status,
        },
        "method": {
            "cold": "guest runs `sync; echo 3 > /proc/sys/vm/drop_caches` immediately before the run",
            "warm": "the next run on the same files, with no cache drop",
            "io": "fio psync, direct=0 (buffered), group_reporting, percentile_list=50:95:99",
            "data_disk": "raw virtio-blk, QEMU cache=none",
            "zfs_whole_disk": "zfs-whole passes /dev/vdb directly to `zpool create -f -m /data/tank tank /dev/vdb`; no partition, loop device, or file-backed vdev is used for that target",
            "zfs_legacy_file_vdev": "the separate zfs target remains the earlier file-backed vdev and is not a whole-disk result",
            "btrfs": "mkfs.btrfs on the host, guest insmods btrfs.ko and mounts /dev/vdb",
            "results_disk": "raw ext4 virtio-blk, extracted with debugfs; no loop mount",
            "root": "read-only ext4 image built in the vm court",
            "reps": args.reps,
            "target_timeout_s": args.timeout,
            "numjobs": [1, 4, 16],
            "seq_size": "8M per job",
            "rand_size": "512k per job",
            "fsync_size": "64k per job, fsync every write",
            "vms": "QEMU -smp 4 -m 1536M; the host court is limited to 4 CPUs and 3 GiB",
            "weftfs": "Rust and Go WeftFS are mounted over FUSE inside the guest; store and mount live on the ext4 data disk",
            "zero_buffers": "fio write buffers default to zeros; WeftFS chunk dedup can therefore reduce stored bytes. This receipt measures latency/IOPS, not storage amplification.",
            "not_claimed": "No physical power loss, no host page-cache drop, no device write-cache loss. ZFS reads are ARC-served unless the pool cache is separately emptied; drop_caches does not drop the ZFS ARC.",
        },
        "targets": target_info,
        "results": results,
        "blockers": blockers,
    }
    Path(args.out).write_text(json.dumps(report, indent=2))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
