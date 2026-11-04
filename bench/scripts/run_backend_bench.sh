#!/usr/bin/env bash
# Local vs immutable-object backend benchmark (1,000 creates + 100 MiB write).
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_backend_bench.sh"
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/backend-${stamp}.json"
log="bench/receipts/backend-${stamp}.log"

cargo build --release --example backend_bench >"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/backend_bench" --out "$receipt" >>"$log" 2>&1

echo "backend receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
