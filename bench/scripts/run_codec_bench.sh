#!/usr/bin/env bash
# Run the codec microbenchmark court and emit a receipt with raw samples.
# Must run inside the OOM-protected court:
#   scripts/docker-court.sh rust "bash bench/scripts/run_codec_bench.sh"
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/codec-${stamp}.json"
log="bench/receipts/codec-${stamp}.log"

cargo build --release --example bench_codec >"$log" 2>&1
./.docker-cache/target-rust/release/examples/bench_codec \
  --iterations "${WEFT_BENCH_ITERATIONS:-7}" \
  --out "$receipt" >>"$log" 2>&1

echo "codec receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
