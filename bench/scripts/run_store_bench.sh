#!/usr/bin/env bash
# Store/context benchmark against the real WeftFS store API.
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_store_bench.sh"
#
# Override the object counts with WEFT_STORE_SIZES=100,1000 (or a single size)
# when the full 10,000-object workload cannot fit the court budget. Any
# reduction must be stated in the receipt and in bench/RESULTS-v1.md.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/bench-store-${stamp}.json"
log="bench/receipts/bench-store-${stamp}.log"

cargo build --release --example bench_store >"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
sizes="${WEFT_STORE_SIZES:-100,1000,10000}"
"$target/release/examples/bench_store" \
  --sizes "$sizes" \
  --out "$receipt" >>"$log" 2>&1

echo "store receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
