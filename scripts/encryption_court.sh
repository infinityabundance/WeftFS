#!/usr/bin/env bash
# T079 court (orchestrated): optional encryption at rest, both dedup policies and
# the measured cost, run inside the bench court.
#
#   scripts/encryption_court.sh
#
# Preflight here, work in `bench/scripts/encryption_court.sh` (which asserts
# `WEFT_IN_COURT`). Exit 78 with exactly one `encryption: skip <reason>` line
# means the environment cannot run the court; anything after preflight fails.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [[ "$(uname -m)" != "x86_64" ]]; then
  echo "encryption: skip this court is pinned to the x86_64 images"
  exit 78
fi
if ! command -v docker >/dev/null 2>&1; then
  echo "encryption: skip no docker CLI on PATH"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "encryption: skip no reachable docker daemon"
  exit 78
fi

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
WEFT_STAMP="$stamp" scripts/docker-court.sh bench \
  "cd /work && WEFT_STAMP=${stamp} bash bench/scripts/encryption_court.sh"

receipt="bench/receipts/encryption-${stamp}.json"
if [[ ! -f "$receipt" ]]; then
  echo "encryption: missing receipt $receipt" >&2
  exit 1
fi
echo "encryption: receipt $receipt"
