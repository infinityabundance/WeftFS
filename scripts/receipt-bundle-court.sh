#!/usr/bin/env bash
# Run the signed-bundle helper inside the hardened Rust Docker court.
#
# The helper is compiled from this repository (no prebuilt binary, no network
# beyond the cached crate registry) and always runs under scripts/docker-court.sh
# limits. All paths passed to it must be relative to the repository root,
# because the court mounts the repository at /work.
#
# Usage: scripts/receipt-bundle-court.sh create|verify|digest|fingerprint [args]
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# A seed streamed on stdin (--sign-key -) needs an attached container stdin.
stream_stdin=0
previous=""
for arg in "$@"; do
  case "$arg" in
    --sign-key=-|--sign-key=/dev/stdin) stream_stdin=1 ;;
    -|/dev/stdin) [ "$previous" = "--sign-key" ] && stream_stdin=1 ;;
  esac
  previous="$arg"
done
if [ "$stream_stdin" -eq 1 ]; then
  export WEFT_COURT_STDIN=1
fi

quoted=""
for arg in "$@"; do
  quoted+=" $(printf '%q' "$arg")"
done

exec "$ROOT/scripts/docker-court.sh" rust \
  "cd /work && cargo run --quiet --bin weft-receipt-bundle --${quoted}"
