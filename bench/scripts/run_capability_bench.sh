#!/usr/bin/env bash
# D2 authenticated capability envelope benchmark.
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_capability_bench.sh"
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/capability-envelope-${stamp}.json"
log="bench/receipts/capability-envelope-${stamp}.log"

cargo build --release --example capability_bench >"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/capability_bench" --out "$receipt" >>"$log" 2>&1

echo "capability-envelope receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
