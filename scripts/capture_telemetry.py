#!/usr/bin/env python3
"""Capture host telemetry around a court run. Missing sources say `unavailable`."""

import json
import os
import subprocess
import sys
import time


def read(path):
    try:
        with open(path) as handle:
            return handle.read().strip()
    except OSError:
        return "unavailable"


def meminfo():
    out = {}
    try:
        with open("/proc/meminfo") as handle:
            for line in handle:
                key, _, rest = line.partition(":")
                if key in ("MemTotal", "MemFree", "MemAvailable", "Cached", "Dirty", "Writeback"):
                    out[key] = rest.strip()
    except OSError:
        return "unavailable"
    return out


def thermals():
    values = {}
    base = "/sys/class/thermal"
    try:
        entries = sorted(os.listdir(base))
    except OSError:
        return "unavailable"
    for entry in entries:
        if not entry.startswith("thermal_zone"):
            continue
        value = read(os.path.join(base, entry, "temp"))
        kind = read(os.path.join(base, entry, "type"))
        values[entry] = {"type": kind, "temp": value}
    return values or "unavailable"


def command(args):
    try:
        proc = subprocess.run(args, capture_output=True, text=True, timeout=30)
        if proc.returncode != 0:
            return {"unavailable": proc.stderr.strip()[-400:]}
        return proc.stdout.strip()
    except Exception as err:  # noqa: BLE001 - telemetry must never fail a court
        return {"unavailable": str(err)}


def main():
    if len(sys.argv) != 4:
        print("usage: capture_telemetry.py OUT STAMP PHASE", file=sys.stderr)
        return 64
    out_path, stamp, phase = sys.argv[1:4]
    try:
        with open(out_path) as handle:
            receipt = json.load(handle)
    except (OSError, json.JSONDecodeError):
        receipt = {"schema": "weft.paired-telemetry.v1", "stamp": stamp, "phases": {}}
    receipt["phases"][phase] = {
        "captured_unix": time.time(),
        "loadavg": read("/proc/loadavg"),
        "meminfo": meminfo(),
        "thermal": thermals(),
        "vmstat": command(["vmstat", "1", "5"]),
        "iostat": command(["iostat", "-x", "1", "3"]),
        "df_work": command(["df", "-h", os.getcwd()]),
        "docker_ps": command(["docker", "ps", "--format", "{{.Names}} {{.Status}}"]),
        "cgroup_memory_current": read("/sys/fs/cgroup/memory.current"),
        "cgroup_cpu_stat": read("/sys/fs/cgroup/cpu.stat"),
    }
    with open(out_path, "w") as handle:
        json.dump(receipt, handle, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
