#!/usr/bin/env python3
"""Probe the court for energy and hardware-counter support.

Run only inside the OOM-protected bench court:
  scripts/docker-court.sh bench "python3 bench/scripts/probe_energy.py --out ..."

The probe attempts `perf stat` and reads RAPL/powercap `energy_uj` if exposed.
If either is unavailable it records the exact blocker and states that CPU user/
system time (from wait4 rusage) is the supported metric. CPU time is never
called energy.
"""

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import time
from pathlib import Path


def run(cmd, timeout=20):
    started = time.time()
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return {
            "command": cmd,
            "available": True,
            "exit_code": proc.returncode,
            "stdout": proc.stdout.strip()[:4000],
            "stderr": proc.stderr.strip()[:4000],
            "wall_s": time.time() - started,
        }
    except FileNotFoundError:
        return {"command": cmd, "available": False, "reason": f"{cmd[0]} not installed"}
    except PermissionError as err:
        return {"command": cmd, "available": False, "reason": f"permission denied: {err}"}
    except subprocess.TimeoutExpired:
        return {"command": cmd, "available": False, "reason": "timed out"}
    except OSError as err:
        return {"command": cmd, "available": False, "reason": str(err)}


def read_text(path):
    try:
        return Path(path).read_text().strip()
    except OSError as err:
        return f"unavailable: {err}"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    powercap = Path("/sys/class/powercap")
    rapl = []
    if powercap.exists():
        for entry in sorted(powercap.iterdir()):
            energy = entry / "energy_uj"
            rapl.append(
                {
                    "path": str(entry),
                    "energy_uj": read_text(energy) if energy.exists() else "absent",
                    "energy_uj_exists": energy.exists(),
                    "name": read_text(entry / "name") if (entry / "name").exists() else None,
                }
            )

    report = {
        "schema": "weft-energy-probe-v1",
        "generated_at_unix": int(time.time()),
        "environment": {
            "kernel": platform.release(),
            "uname": platform.uname()._asdict(),
            "python": sys.version,
            "court_image": os.environ.get("WEFT_COURT_IMAGE"),
            "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID"),
            "court_memory": os.environ.get("WEFT_COURT_MEMORY"),
            "court_cpus": os.environ.get("WEFT_COURT_CPUS"),
            "court_pids": os.environ.get("WEFT_COURT_PIDS"),
            "revision_weftfs": os.environ.get("WEFT_REPO_WEFTFS"),
            "revision_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC"),
        },
        "perf": {
            "installed": shutil.which("perf") is not None,
            "which": shutil.which("perf"),
            "perf_event_paranoid": read_text("/proc/sys/kernel/perf_event_paranoid"),
            "stat_cycles_instructions_task_clock": run(
                ["perf", "stat", "-e", "cycles,instructions,task-clock", "--", "true"]
            ),
            "stat_software_task_clock": run(
                ["perf", "stat", "-e", "task-clock", "--", "true"]
            ),
        },
        "rapl_powercap": {
            "directory_exists": powercap.exists(),
            "entries": rapl,
            "readable_energy_files": sum(
                1 for entry in rapl if entry["energy_uj_exists"] and not str(entry["energy_uj"]).startswith("unavailable")
            ),
        },
        "other": {
            "cpu_scaling_governor": read_text("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
            "cgroup_cpu_stat": read_text("/sys/fs/cgroup/cpu.stat"),
            "cgroup_cpu_max": read_text("/sys/fs/cgroup/cpu.max"),
            "cgroup_memory_current": read_text("/sys/fs/cgroup/memory.current"),
            "schedstat": read_text("/proc/self/schedstat"),
        },
        "conclusion": {
            "supported_metric": "CPU user and system time per process from wait4 rusage (nanoseconds), plus wall time and max RSS",
            "energy_measured": False,
            "why": "perf counters and RAPL energy_uj were probed inside this court; the fields above record exactly what was available",
            "not_claimed": "CPU time is not energy; no joule, watt, or package-power number is reported or inferred",
        },
    }
    Path(args.out).write_text(json.dumps(report, indent=2))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
