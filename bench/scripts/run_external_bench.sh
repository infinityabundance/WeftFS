#!/usr/bin/env bash
# External context/query baseline: schema-indexed SQLite (relational and
# adjacency-table variants) against the exact bench_store corpus.
#
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_external_bench.sh"
#
# Outputs:
#   bench/receipts/bench-external-<UTC>.json
#   bench/receipts/bench-external-<UTC>.log
#   bench/receipts/external-corpus-<UTC>.json  (same hashes/generations)
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/bench-external-${stamp}.json"
log="bench/receipts/bench-external-${stamp}.log"
manifest="bench/receipts/external-corpus-${stamp}.json"
work="${WEFT_EXTERNAL_WORK:-$root/.docker-cache/bench-external}"

exec > >(tee -a "$log") 2>&1

echo "external bench: image=${WEFT_COURT_IMAGE:-unknown} kernel=$(uname -r)"
cargo build --release --example bench_external_manifest
target="${CARGO_TARGET_DIR:-$root/target}"
"$target/release/examples/bench_external_manifest" \
  --sizes "${WEFT_EXTERNAL_SIZES:-100,1000,10000}" \
  --out "$manifest"

python3 bench/external/sqlite_baseline.py \
  --manifest "$manifest" \
  --out "$receipt" \
  --sizes "${WEFT_EXTERNAL_SIZES:-100,1000,10000}" \
  --work-dir "$work" \
  --warm-iterations "${WEFT_EXTERNAL_WARM_ITERS:-0}" \
  --cold-iterations "${WEFT_EXTERNAL_COLD_ITERS:-5}"

echo "external receipt: $receipt"
echo "corpus manifest: $manifest"
echo "raw log: $log"
sha256sum "$receipt" "$manifest" "$log"
