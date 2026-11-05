#!/usr/bin/env python3
"""Reference adapter: independent Python `weftcodec` reader.

The Python package implements WFT1 modes 0-8. Vectors for modes 9-14, lowered
limits, adversarial CLI flags, and the identity record kinds are reported as
explicit skips; they are never silently counted as passes.
"""

import hashlib
import json
import os
import subprocess
import sys
import tempfile

ERROR_CLASSES = (
    "Malformed",
    "Corrupt",
    "UnsupportedVersion",
    "UnsupportedMode",
    "UnsupportedFeature",
    "LimitExceeded",
    "DependencyMissing",
    "DependencyDenied",
    "DependencyMismatch",
    "DependencyCycle",
    "NonCanonical",
    "Io",
)


def emit(result):
    print(json.dumps(result, sort_keys=True))


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def vector_subdir(path):
    parts = path.replace("\\", "/").split("/")
    for index, part in enumerate(parts):
        if part != "conformance":
            continue
        rest = parts[index + 1 :]
        if rest and rest[0] == "v1":
            rest = rest[1:]
        elif len(rest) >= 2 and rest[1] == "v1":
            rest = [rest[0]] + rest[2:]
        return rest[0] if rest else ""
    return ""


def fail(message):
    print("adapter failure: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def main():
    if len(sys.argv) != 2:
        fail("usage: adapter.py VECTOR.json")
    path = sys.argv[1]
    with open(path, "r", encoding="utf-8") as handle:
        vector = json.load(handle)
    sub = vector_subdir(path)
    if sub != "codec":
        emit({"result": "skip", "reason": "Python adapter covers codec frame vectors only"})
        return
    if vector.get("limits"):
        emit({"result": "skip", "reason": "Python CLI does not expose lowered decoder limits"})
        return
    frame_hex = vector.get("frame_hex")
    if not frame_hex:
        emit({"result": "skip", "reason": "vector has no frame_hex"})
        return
    frame = bytes.fromhex(frame_hex)
    mode = frame[7] if len(frame) > 7 else None
    if mode is None or mode > 8:
        emit({"result": "skip", "reason": "Python reader implements modes 0-8"})
        return

    python = os.environ.get("WEFT_PYTHON", sys.executable)
    pythonpath = os.environ.get("WEFT_PYTHONPATH")
    env = dict(os.environ)
    if pythonpath:
        env["PYTHONPATH"] = pythonpath

    work = tempfile.mkdtemp(prefix="weft-conformance-py-")
    try:
        frame_path = os.path.join(work, "frame.wft1")
        output_path = os.path.join(work, "decoded.bin")
        with open(frame_path, "wb") as handle:
            handle.write(frame)
        cmd = [python, "-m", "weftcodec", "decode", frame_path, output_path]
        deps = vector.get("dependencies") or []
        if deps:
            deps_dir = os.path.join(work, "deps")
            os.makedirs(deps_dir, exist_ok=True)
            for entry in deps:
                with open(os.path.join(deps_dir, entry["hash"]), "wb") as handle:
                    handle.write(bytes.fromhex(entry["bytes_hex"]))
            cmd.extend(["--deps", deps_dir])
        completed = subprocess.run(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env
        )
        if completed.returncode != 0:
            stderr = completed.stderr.decode("utf-8", "replace")
            cls = None
            for line in stderr.splitlines():
                head = line.split(":", 1)[0].strip()
                if head in ERROR_CLASSES:
                    cls = head
                    break
            if cls is None:
                fail("no error class in stderr: %s" % stderr.strip())
            emit({"result": "error", "class": cls})
            return
        with open(output_path, "rb") as handle:
            decoded = handle.read()
        root = frame[40:72].hex()
        emit({"result": "ok", "content_root": root, "decoded_sha256": sha256(decoded)})
    finally:
        if os.environ.get("WEFT_CONFORMANCE_KEEP") != "1":
            import shutil

            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
