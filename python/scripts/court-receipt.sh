#!/usr/bin/env bash
# Build the weftcodec wheel, test it from the installed wheel in the Python
# court, and write a receipt with the exact counts and digests.
#
# Run as: scripts/docker-court.sh python "python/scripts/court-receipt.sh"
set -Eeuo pipefail

cd /work

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="bench/receipts/python-bindings-${stamp}.json"
log="bench/receipts/python-bindings-${stamp}.log"

export WEFT_VECTOR_SUMMARY="/tmp/weft-vectors.json"
export WEFT_CROSS_SUMMARY="/tmp/weft-cross.json"
export WEFT_REPO_ROOT="/work"

rm -rf /tmp/weft-python-build /tmp/weft-dist /tmp/weft-venv /tmp/weft-junit.xml
rm -f "$WEFT_VECTOR_SUMMARY" "$WEFT_CROSS_SUMMARY"

# Build from a copy so no build artifact (egg-info, build/) is written into the
# read-only-ish source tree mounted from the host.
cp -r /work/python /tmp/weft-python-build
python -m build --wheel --no-isolation --outdir /tmp/weft-dist \
  /tmp/weft-python-build > /tmp/weft-build.log 2>&1

wheel="$(ls /tmp/weft-dist/*.whl | head -n 1)"
wheel_name="$(basename "$wheel")"
wheel_sha="$(sha256sum "$wheel" | awk '{print $1}')"
wheel_size="$(stat -c %s "$wheel")"
mkdir -p python/dist
cp "$wheel" python/dist/

python -m venv --system-site-packages /tmp/weft-venv
/tmp/weft-venv/bin/python -m pip install --no-index --no-deps "$wheel" > /tmp/weft-pip.log 2>&1

# Run from /work (not /work/python) so the installed wheel, not the source
# tree, is the package under test.
set +e
/tmp/weft-venv/bin/python -m pytest -p no:cacheprovider -q \
  --junitxml=/tmp/weft-junit.xml python/tests > /tmp/weft-pytest.log 2>&1
pytest_status=$?
set -e

cat /tmp/weft-build.log /tmp/weft-pip.log /tmp/weft-pytest.log > "$log"

export WEFT_RECEIPT_PATH="$receipt"
export WEFT_WHEEL_NAME="$wheel_name"
export WEFT_WHEEL_SHA256="$wheel_sha"
export WEFT_WHEEL_SIZE="$wheel_size"
export WEFT_PYTEST_STATUS="$pytest_status"
export WEFT_JUNIT_PATH="/tmp/weft-junit.xml"
export WEFT_LOG_PATH="$log"

python - <<'PY'
import json
import os
import platform
import sys
import xml.etree.ElementTree as ET


def load(path):
    try:
        with open(path, "r", encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, ValueError):
        return None


def junit_counts(path):
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError):
        return {"tests": None, "failures": None, "errors": None, "skipped": None}
    suite = root.find("testsuite") if root.tag != "testsuite" else root
    if suite is None:
        return {"tests": None, "failures": None, "errors": None, "skipped": None}
    return {
        key: int(suite.get(key, 0))
        for key in ("tests", "failures", "errors", "skipped")
    }


env = os.environ
receipt = {
    "receipt": "weftfs-python-bindings/1",
    "generated_at_utc": __import__("datetime").datetime.now(
        __import__("datetime").timezone.utc
    ).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "label": (
        "Independent Python implementation of the WeftCodec v1 reader, tested in "
        "the Docker python court against Rust-generated conformance vectors and "
        "the Rust CLI. Not a PyPI publication; the wheel is a release artifact. "
        "Synthetic fixtures, single host."
    ),
    "environment": {
        "revision_weftfs": env.get("WEFT_REPO_WEFTFS", "unknown"),
        "revision_weftcodec": env.get("WEFT_REPO_WEFTCODEC", "unknown"),
        "revision_weftcodec_go": env.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        "revision_weftfs_go": env.get("WEFT_REPO_WEFTFS_GO", "unknown"),
        "court_image": env.get("WEFT_COURT_IMAGE", "unknown"),
        "court_image_id": env.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "court_kernel": env.get("WEFT_COURT_KERNEL", "unknown"),
        "court_memory": env.get("WEFT_COURT_MEMORY", "unknown"),
        "court_cpus": env.get("WEFT_COURT_CPUS", "unknown"),
        "court_pids": env.get("WEFT_COURT_PIDS", "unknown"),
        "court_tmpfs": "/tmp:rw,size=1g,nosuid,nodev",
        "court_cap_drop": "ALL",
        "court_no_new_privileges": True,
        "docker_client": env.get("WEFT_DOCKER_CLIENT", "unknown"),
        "docker_server": env.get("WEFT_DOCKER_SERVER", "unknown"),
        "python_version": platform.python_version(),
        "python_executable": sys.executable,
        "pytest_status": int(env.get("WEFT_PYTEST_STATUS", "1")),
    },
    "wheel": {
        "filename": env.get("WEFT_WHEEL_NAME"),
        "sha256": env.get("WEFT_WHEEL_SHA256"),
        "size_bytes": int(env.get("WEFT_WHEEL_SIZE", "0")),
        "path": "python/dist/" + (env.get("WEFT_WHEEL_NAME") or ""),
        "name": "weftcodec",
        "version": "0.5.0",
        "license": "Apache-2.0",
        "pypi_published": False,
        "pypi_note": "No PyPI token is available; the wheel is attached to the release.",
    },
    "vectors": load(env.get("WEFT_VECTOR_SUMMARY", "/nonexistent")),
    "cross_cli": load(env.get("WEFT_CROSS_SUMMARY", "/nonexistent")),
    "tests": {
        "junit": junit_counts(env.get("WEFT_JUNIT_PATH", "/nonexistent")),
        "log": env.get("WEFT_LOG_PATH"),
    },
    "commands": [
        "cp -r /work/python /tmp/weft-python-build",
        "python -m build --wheel --no-isolation --outdir /tmp/weft-dist /tmp/weft-python-build",
        "python -m venv --system-site-packages /tmp/weft-venv",
        "/tmp/weft-venv/bin/python -m pip install --no-index --no-deps <wheel>",
        "/tmp/weft-venv/bin/python -m pytest -p no:cacheprovider -q --junitxml=/tmp/weft-junit.xml python/tests",
    ],
}

with open(env["WEFT_RECEIPT_PATH"], "w", encoding="utf-8") as handle:
    json.dump(receipt, handle, indent=2, sort_keys=True)
    handle.write("\n")

print(json.dumps({k: receipt[k] for k in ("wheel", "tests")}, indent=2))
PY

exit "$pytest_status"
