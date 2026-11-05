#!/usr/bin/env bash
# Run every conformance vector against an implementation adapter.
#
#   scripts/run-conformance.sh scripts/conformance-adapter-rust.sh
#   scripts/run-conformance.sh scripts/conformance-adapter-go.sh --json out.json
#
# See docs/CONFORMANCE.md for the adapter contract. The runner is host-only
# shell/Python and never compiles anything; build the implementation in its
# Docker court first.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$root/scripts/run_conformance.py" "$@"
