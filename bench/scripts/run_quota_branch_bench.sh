#!/usr/bin/env bash
# Quota admission overhead and branching time-travel benchmark.
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_quota_branch_bench.sh"
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/quota-branch-${stamp}.json"
log="bench/receipts/quota-branch-${stamp}.log"

cargo build --release --example bench_quota_branch >"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
work="${WEFT_BENCH_DIR:-$root/.docker-cache/bench-quota-branch}"
"$target/release/examples/bench_quota_branch" --out "$receipt" --work-dir "$work" >>"$log" 2>&1

echo "quota/branch receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
