#!/usr/bin/env bash
# F7 AI-corpus cost graph. Runs only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_ai_corpus_bench.sh"
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
export WEFT_BENCH_STAMP="$stamp"
mkdir -p bench/receipts
receipt="bench/receipts/ai-corpus-${stamp}.json"
graph="bench/receipts/ai-cost-graph-${stamp}.json"
csv="bench/receipts/ai-cost-graph-${stamp}.csv"
log="bench/receipts/ai-corpus-${stamp}.log"

{
  echo "stamp=$stamp"
  echo "revisions (from the court environment):"
  echo "  WeftFS=${WEFT_REPO_WEFTFS:-unknown}"
  echo "  WeftCodec=${WEFT_REPO_WEFTCODEC:-unknown}"
  echo "  WeftCodec-Go=${WEFT_REPO_WEFTCODEC_GO:-unknown}"
  echo "  WeftFS-Go=${WEFT_REPO_WEFTFS_GO:-unknown}"
  echo "environment:"
  uname -srm
  nproc
  grep -m1 'model name' /proc/cpuinfo || true
  grep -m1 'MemTotal' /proc/meminfo || true
  echo "court: ${WEFT_COURT_IMAGE:-unset} ${WEFT_COURT_IMAGE_ID:-unset} mem=${WEFT_COURT_MEMORY:-unset} cpus=${WEFT_COURT_CPUS:-unset} pids=${WEFT_COURT_PIDS:-unset}"
  echo "corpus check:"
} >"$log" 2>&1

# Prove the committed corpus still matches the generator's manifest before
# measuring it. This is a correctness check, not a benchmark.
( cd bench/corpora/ai-v1 && GO111MODULE=off go run generate.go -check ) >>"$log" 2>&1

cargo build --release --example bench_ai_corpus >>"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/bench_ai_corpus" \
  --corpus bench/corpora/ai-v1 \
  --out "$receipt" \
  --graph "$graph" \
  --csv "$csv" \
  --iterations "${WEFT_AI_ITERATIONS:-7}" >>"$log" 2>&1

echo "ai-corpus receipt: $receipt"
echo "cost graph JSON: $graph"
echo "cost graph CSV: $csv"
echo "raw log: $log"
sha256sum "$receipt" "$graph" "$csv" "$log"
