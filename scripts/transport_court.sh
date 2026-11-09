#!/usr/bin/env bash
# weft.transport.v1 court (subphase R8). Runs the harness inside the OOM-limited
# bench court and writes a `weft.transport.v1` receipt.
#
# Usage:
#   scripts/transport_court.sh [--corpus FILE] [--reps N]
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="$root/bench/receipts/transport-$stamp.json"
log="$root/bench/receipts/transport-$stamp.log"
mkdir -p "$root/bench/receipts"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true

export WEFT_STAMP="$stamp"
cmd="cd /work && WEFT_STAMP='$stamp' cargo run --release --quiet --example transport_court -- --out /work/bench/receipts/transport-$stamp.json"
for arg in "$@"; do
  cmd="$cmd $(printf '%q' "$arg")"
done

set +e
"$root/scripts/docker-court.sh" bench "$cmd" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
set -e
if [[ "$status" -ne 0 ]]; then
  echo "transport court failed (exit $status); log $log" >&2
  exit "$status"
fi
if [[ ! -f "$receipt" ]]; then
  echo "transport court produced no receipt at $receipt" >&2
  exit 1
fi
echo "transport court receipt: $receipt"
echo "transport court log:     $log"
