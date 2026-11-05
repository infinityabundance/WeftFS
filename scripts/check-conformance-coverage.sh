#!/usr/bin/env bash
# Fail closed on conformance coverage gaps and manifest drift. Host-only.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$root/scripts/check_conformance_coverage.py" "$@"
