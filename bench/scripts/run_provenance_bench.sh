#!/usr/bin/env bash
# Provenance recovery benchmark (synthetic 100-artifact agent trace).
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_provenance_bench.sh"
#
# The harness stores 10 operations x 10 artefacts x 1 MiB, records one
# provenance receipt per operation, removes one full operation (10 artefacts),
# and measures fetch/decode/recompute recovery under injected and measured
# costs. Warm-cache, single host; see the receipt label.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/provenance-${stamp}.json"
log="bench/receipts/provenance-${stamp}.log"

cargo build --release --example provenance_bench >"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/provenance_bench" --out "$receipt" >>"$log" 2>&1

echo "provenance receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
