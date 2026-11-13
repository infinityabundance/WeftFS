#!/usr/bin/env python3
"""Device control for the augmentation court.

Runs **inside** the bench court (same image, same hard limits, same volume the
measured store is written to) and records the fsynced throughput of that
volume. A durable-write row is device-bound once encoding is cheap, so the
number is part of the measurement's environment and is published beside the
receipt rather than assumed.

Usage: `python3 bench/scripts/augmentation_device_control.py OUT STAMP`

Three `dd if=/dev/zero bs=1M count=200 conv=fsync` runs; the median is the
reported rate. `regime` is `degraded_device` below 600 MB/s because the native
control (424,439,296 B in 101 fsynced files) writes at roughly 1.28 GB/s on a
quiet host; a session below that is not comparable with one above it.
"""

from __future__ import annotations

import json
import platform
import subprocess
import sys


def rate_mb_per_second(line: str) -> float:
    """Parse the `dd` progress line into MB/s."""
    if not line:
        return 0.0
    tail = line.rsplit(",", 1)[-1].strip().split()
    if len(tail) < 2:
        return 0.0
    try:
        value = float(tail[-2])
    except ValueError:
        return 0.0
    unit = tail[-1]
    if unit.startswith("GB"):
        return value * 1000.0
    if unit.startswith("kB"):
        return value * 0.001
    if unit.startswith("B/"):
        return value / 1_000_000.0
    return value


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__.strip().splitlines()[0], file=sys.stderr)
        return 64
    path, stamp = argv[1], argv[2]
    rates: list[float] = []
    for index in range(3):
        target = "/work/.docker-cache/aug-device-%s-%d" % (stamp, index)
        completed = subprocess.run(
            ["dd", "if=/dev/zero", "of=" + target, "bs=1M", "count=200",
             "conv=fsync"],
            capture_output=True,
            text=True,
            check=True,
        )
        subprocess.run(["rm", "-f", target], check=False)
        line = completed.stderr.strip().splitlines()[-1]
        rates.append(rate_mb_per_second(line))
    ordered = sorted(rates)
    median = ordered[len(ordered) // 2]
    receipt = {
        "schema": "weft.augmentation.device-control.v1",
        "stamp": stamp,
        "uname": platform.platform(),
        "operation": (
            "dd if=/dev/zero bs=1M count=200 conv=fsync, three times, inside "
            "the bench court"
        ),
        "fsync_megabytes_per_second": ordered,
        "median_megabytes_per_second": median,
        "clean_session_reference_megabytes_per_second": 1280.0,
        "regime": "degraded_device" if median < 600.0 else "clean",
        "note": (
            "the native control A writes 424,439,296 B in 101 fsynced files; a "
            "median far below 1.28 GB/s means the durability path has degraded "
            "and every durable-write row in this session is device-bound"
        ),
    }
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print("augmentation_court: device %.0f MB/s (%s)" % (median, receipt["regime"]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
