#!/usr/bin/env bash
# F7 CDC chunk-size sweep. Runs only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_ai_corpus_cdc_sweep.sh"
#
# The default full benchmark (bench/scripts/run_ai_corpus_bench.sh) is
# unchanged. This sweep measures only the CDC strategy plus the forced-zstd
# comparator, at several average chunk sizes, on the unmodified ai-v1 corpus.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
export WEFT_BENCH_STAMP="$stamp"
mkdir -p bench/receipts
receipt="bench/receipts/ai-corpus-cdc-sweep-${stamp}.json"
log="bench/receipts/ai-corpus-cdc-sweep-${stamp}.log"
sweep_kib="${WEFT_CDC_SWEEP_KIB:-8,4,2,1,0.5}"

{
  echo "stamp=$stamp"
  echo "sweep_avg_kib=$sweep_kib"
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
  --iterations "${WEFT_AI_ITERATIONS:-7}" \
  --cdc-sweep "$sweep_kib" >>"$log" 2>&1

echo "cdc sweep receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
