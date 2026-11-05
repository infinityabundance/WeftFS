#!/usr/bin/env bash
# Generate or check conformance/manifest.json. Host-only: no Docker, no build.
#
# Usage:
#   scripts/gen-conformance-manifest.sh          # write the manifest
#   scripts/gen-conformance-manifest.sh --check  # fail on drift
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$root/scripts/gen_conformance_manifest.py" "$@"
