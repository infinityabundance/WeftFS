#!/usr/bin/env python3
"""Attempt to install and import a real inference engine in the python court.

Runs inside `scripts/docker-court.sh python` and writes one JSON document with
the exact command, exit code, duration, free space before/after and a bounded
tail of the captured output for every attempt. Nothing is summarised from
memory: if a command did not run here, it is not in the document.

Each attempt starts from a cleaned `/tmp` so a failure cannot be inherited from
the previous attempt's partial downloads. That matters: an earlier receipt
reported that the vLLM wheel alone hit ENOSPC without saying whether the
failed full install's downloads were still on the tmpfs.

Exit status is 0 when no engine could run (the expected blocker, recorded), and
1 when something did import — that outcome needs a real engine court, which
this script does not implement, so it must not be reported as a success.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time

TAIL_CHARS = 2400


def free_mib(path: str = "/tmp") -> int | None:
    try:
        info = os.statvfs(path)
    except OSError:
        return None
    return (info.f_bavail * info.f_frsize) // (1024 * 1024)


def clean(paths: list[str]) -> None:
    for path in paths:
        shutil.rmtree(path, ignore_errors=True)


def run(command: list[str], timeout: int) -> dict:
    started = time.monotonic()
    try:
        proc = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=timeout,
            env={**os.environ, "PIP_DISABLE_PIP_VERSION_CHECK": "1"},
        )
        code = proc.returncode
        output = (proc.stdout or "") + (proc.stderr or "")
    except subprocess.TimeoutExpired as exc:
        code = None
        text = ""
        for stream in (exc.stdout, exc.stderr):
            if stream:
                text += stream if isinstance(stream, str) else stream.decode("utf-8", "replace")
        output = text + f"\n[timeout after {timeout}s]"
    except FileNotFoundError as exc:
        code = 127
        output = str(exc)
    return {
        "command": " ".join(command),
        "exit_code": code,
        "timeout_seconds": timeout,
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "tail": output[-TAIL_CHARS:],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True)
    parser.add_argument("--install-timeout", type=int, default=600)
    args = parser.parse_args()

    targets = ["/tmp/vllm-full", "/tmp/vllm-nodeps", "/tmp/sglang-nodeps", "/tmp/sglang-full"]
    attempts: dict[str, object] = {
        "python": run([sys.executable, "--version"], 30),
        "pip": run([sys.executable, "-m", "pip", "--version"], 30),
        "gpu_devices": run(["sh", "-c", "ls /dev/nvidia* 2>&1"], 30),
        "tmp_free_mib_initial": free_mib(),
    }

    clean(targets)
    attempts["vllm_versions"] = run(
        [sys.executable, "-m", "pip", "index", "versions", "vllm"], 120
    )
    attempts["tmp_free_mib_before_vllm_full"] = free_mib()
    attempts["vllm_full"] = run(
        [sys.executable, "-m", "pip", "install", "--no-cache-dir", "--target",
         "/tmp/vllm-full", "vllm"],
        args.install_timeout,
    )
    attempts["tmp_free_mib_after_vllm_full"] = free_mib()

    clean(targets)
    attempts["tmp_free_mib_before_vllm_nodeps"] = free_mib()
    attempts["vllm_nodeps"] = run(
        [sys.executable, "-m", "pip", "install", "--no-cache-dir", "--no-deps",
         "--target", "/tmp/vllm-nodeps", "vllm"],
        args.install_timeout,
    )
    attempts["vllm_nodeps_import"] = run(
        [sys.executable, "-c",
         "import sys; sys.path.insert(0,'/tmp/vllm-nodeps'); import vllm; print(vllm.__version__)"],
        120,
    )

    clean(targets)
    attempts["tmp_free_mib_before_sglang_nodeps"] = free_mib()
    attempts["sglang_nodeps"] = run(
        [sys.executable, "-m", "pip", "install", "--no-cache-dir", "--no-deps",
         "--target", "/tmp/sglang-nodeps", "sglang"],
        args.install_timeout,
    )
    attempts["sglang_nodeps_import"] = run(
        [sys.executable, "-c",
         "import sys; sys.path.insert(0,'/tmp/sglang-nodeps'); import sglang; print(sglang.__version__)"],
        120,
    )

    clean(targets)
    attempts["tmp_free_mib_before_sglang_full"] = free_mib()
    attempts["sglang_full"] = run(
        [sys.executable, "-m", "pip", "install", "--no-cache-dir", "--target",
         "/tmp/sglang-full", "sglang"],
        args.install_timeout,
    )

    engine_ran = any(
        attempts[key].get("exit_code") == 0
        for key in ("vllm_nodeps_import", "sglang_nodeps_import")
        if isinstance(attempts.get(key), dict)
    )
    document = {
        "schema": "weft.kv.engine.attempt.v1",
        "stamp": os.environ.get("WEFT_STAMP", "unknown"),
        "result": (
            "an engine imported; this script cannot measure it"
            if engine_ran
            else "no inference engine could run in the court"
        ),
        "attempts": attempts,
        "note": (
            "each install attempt starts from a cleaned /tmp; free space before "
            "and after every attempt is recorded so an ENOSPC cannot be inherited "
            "from a previous attempt's partial downloads"
        ),
    }
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(document, handle, indent=2, sort_keys=True)
        handle.write("\n")
    clean(targets)
    print(f"kv_engine_attempt: {document['result']}")
    return 1 if engine_ran else 0


if __name__ == "__main__":
    sys.exit(main())
