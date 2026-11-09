#!/usr/bin/env bash
# Mode-20 ENGRAM_SHARD court. Runs the harness inside the OOM-limited bench
# court and writes `weft.engram.v1` to bench/receipts/.
#
# Usage:
#   scripts/engram_court.sh [--table-mib 256] [--groups 64] [--dim 1024]
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="$root/bench/receipts/engram-$stamp.json"
log="$root/bench/receipts/engram-$stamp.log"
mkdir -p "$root/bench/receipts"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true

args=("$@")
cmd="cd /work && cargo run --release --quiet --example engram_court -- --out /work/bench/receipts/engram-$stamp.json"
for arg in "${args[@]}"; do
  cmd="$cmd $arg"
done

set +e
"$root/scripts/docker-court.sh" bench "$cmd" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
set -e
if [[ "$status" -ne 0 ]]; then
  echo "engram court failed (exit $status); log $log" >&2
  exit "$status"
fi
if [[ ! -f "$receipt" ]]; then
  echo "engram court produced no receipt at $receipt" >&2
  exit 1
fi
echo "engram court receipt: $receipt"
echo "engram court log:     $log"
