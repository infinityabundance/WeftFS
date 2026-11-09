#!/usr/bin/env bash
# SWA-style retention court (subphase R6).
#
# Runs `examples/retention_court.rs` in the bench court under the standard hard
# limits and writes one `weft.retention-court.v1` receipt plus a raw log.
#
# The trace is synthetic and deterministic: 1,000 volatile KV pages with a
# sliding-window reuse distance of minutes, and 300 ephemeral pages reused
# across a simulated six-hour session. It is a reuse-distance model, not a
# serving-workload measurement.
#
# Usage: scripts/retention_court.sh [--keep]
#
# Environment skips print exactly one `retention_court: skip <reason>` line and
# exit 78. The aggregate verification court records that as `skip`, never as a
# pass.
set -Eeuo pipefail

skip() {
  echo "retention_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

keep="${WEFT_RETENTION_KEEP:-0}"
for arg in "$@"; do
  case "$arg" in
    --keep) keep=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

stamp="${WEFT_RETENTION_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_RETENTION_STAMP="$stamp"
receipt="bench/receipts/retention-$stamp.json"
log="bench/receipts/retention-$stamp.log"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

echo "retention_court: bench court, stamp=$stamp" >&2
scripts/docker-court.sh bench \
  "cd /work && cargo run --release --quiet --example retention_court -- \
     --out /work/$receipt \
     --work /work/.docker-cache/retention-court-$stamp" 2>&1 | tee "$log"

if [[ ! -s "$receipt" ]]; then
  echo "retention_court: no receipt at $receipt" >&2
  exit 1
fi
if [[ "$keep" != "1" ]]; then
  rm -rf ".docker-cache/retention-court-$stamp"
fi
echo "retention_court: receipt=$receipt log=$log"
