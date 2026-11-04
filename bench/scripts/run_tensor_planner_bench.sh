#!/usr/bin/env bash
# Tensor representation planner benchmark (synthetic model family).
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_tensor_planner_bench.sh"
#
# Three planners (independent ZSTD, bytes-only, latency-weighted) run in
# separate processes so each peak RSS is honest. Full restore is timed with
# 1, 2, and 4 concurrent read-only workers.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/tensor-planner-${stamp}.json"
log="bench/receipts/tensor-planner-${stamp}.log"

cargo build --release --example tensor_planner_bench >"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
exe="$target/release/examples/tensor_planner_bench"

parts=()
for planner in zstd bytes latency; do
  part="bench/receipts/tensor-planner-${stamp}-part-${planner}.json"
  "$exe" --planner "$planner" --out "$part" >>"$log" 2>&1
  parts+=("$part")
done

"$exe" --merge "${parts[@]}" --out "$receipt" >>"$log" 2>&1
rm -f "${parts[@]}"

echo "tensor planner receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
