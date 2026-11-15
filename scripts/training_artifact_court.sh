#!/usr/bin/env bash
# P01/P02 court: one composable Training Artifact, resolved and validated by an
# independent Go consumer from the same store, plus bounded causal closure and
# the clean-room verifier.
#
#   scripts/training_artifact_court.sh
#
# All work happens inside one hard-limited fuse court (Rust + Go). Exit 78 with
# exactly one `training_artifact: skip <reason>` line means the environment
# cannot run the court; anything after that preflight is a failure.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [[ "$(uname -m)" != "x86_64" ]]; then
  echo "training_artifact: skip this court is pinned to the x86_64 images"
  exit 78
fi
if ! command -v docker >/dev/null 2>&1; then
  echo "training_artifact: skip no docker CLI on PATH"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "training_artifact: skip no reachable docker daemon"
  exit 78
fi
if ! docker image inspect weft/fuse-court:1 >/dev/null 2>&1; then
  echo "training_artifact: skip the pinned weft/fuse-court:1 image is absent"
  exit 78
fi

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
log="bench/receipts/training-artifact-${stamp}.log"
mkdir -p bench/receipts
: > "$log"
chmod 0666 "$log"

scripts/docker-court.sh fuse \
  "WEFT_STAMP=${stamp} bash bench/scripts/training_artifact_court_inner.sh ${stamp}" \
  >>"$log" 2>&1 || {
    rc=$?
    echo "training artifact court failed (exit $rc); see $log" >&2
    tail -40 "$log" >&2
    exit 1
  }

receipt="bench/receipts/training-artifact-${stamp}.json"
if [[ ! -f "$receipt" ]]; then
  echo "training artifact court produced no receipt" >&2
  exit 1
fi

python3 - "$receipt" <<'PY'
import json
import sys

with open(sys.argv[1]) as handle:
    receipt = json.load(handle)
print("training_artifact: %s (%d/%d checks)" % (
    receipt.get("result"), receipt["summary"]["passed"], receipt["summary"]["total"]))
if receipt.get("result") != "pass":
    sys.exit(1)
PY
