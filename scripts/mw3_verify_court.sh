#!/usr/bin/env bash
# Aggregate-court wrapper for the MW3 / T021 / T027 benches.
#
#   bash scripts/mw3_verify_court.sh
#
# Runs the reduced MW3 court inside the hard-limited bench court and exits 0 on
# success. An environment that cannot run it prints exactly one
# `mw3: skip <reason>` line and exits 78, which `verify-all.sh` records as a
# skip — never as a pass.
#
# This wrapper deliberately does not supply a T021 baseline binary: the
# committed comparison at WeftFS d2a8b7e1 is the evidence for the before/after,
# and a fresh checkout has no `.docker-cache` build of the start revision. The
# reduced receipt therefore carries the after arm only and says so. The full
# court, with WEFT_MW3_BEFORE pointing at a built baseline, is
# `scripts/mw3_court.sh` with WEFT_MW3_PROFILE=full.
set -Eeuo pipefail

if [[ "$(uname -m)" != "x86_64" ]]; then
  echo "mw3: skip unsupported architecture $(uname -m)"
  exit 78
fi
if ! command -v docker >/dev/null 2>&1; then
  echo "mw3: skip no docker CLI"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "mw3: skip no docker daemon"
  exit 78
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

exec bash scripts/docker-court.sh bench \
  "cd /work && WEFT_MW3_PROFILE=reduced bash scripts/mw3_court.sh"
