#!/usr/bin/env bash
# Run one court script inside a court container, forwarding its exit code.
#
# Exists so `scripts/verify-all.sh` can use `run_orchestrated_court`, which
# records exit 78 as an explicit skip with the script's own printed reason.
set -Eeuo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <rust|go|fuse|bench> <script> [args...]" >&2
  exit 64
fi
kind="$1"
script="$2"
shift 2
exec scripts/docker-court.sh "$kind" "bash $script $*"
