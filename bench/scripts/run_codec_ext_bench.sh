#!/usr/bin/env bash
# Extended codec benchmark: external CLI baselines plus WeftCodec modes with
# whole-object accounting. Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_codec_ext_bench.sh"
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/bench-codec-${stamp}.json"
log="bench/receipts/bench-codec-${stamp}.log"

cargo build --release --example bench_codec_ext >"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/bench_codec_ext" \
  --iterations "${WEFT_BENCH_ITERATIONS:-7}" \
  --out "$receipt" >>"$log" 2>&1

echo "codec receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
