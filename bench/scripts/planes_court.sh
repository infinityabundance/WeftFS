#!/usr/bin/env bash
# R10 blob/commit plane court.
#
# Compares four physical layouts on one deterministic workload:
#
#   local-local               one local backend (the historical default)
#   s3-s3                     one S3 backend (MinIO)
#   s3-blobs-local-commits    split: chunks/manifests on S3, control state local
#   local-blobs-s3-commits    split: chunks/manifests local, control state on S3
#
# For each layout it reports create / range-write / read latency, per-plane
# request counts and bytes, `verify --full`, reopen-and-recover outcome, peak
# RSS, and a content digest. Every completed layout must report the same content
# root, the same commit generation, and the same verified byte count.
#
# Run inside the OOM-protected bench court:
#
#   bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/planes_court.sh"
#   bash scripts/s3_court.sh --planes     # same court, MinIO started for you
#
# Without `WEFT_S3_TEST_ENDPOINT` only local-local runs; the S3 layouts are
# recorded as skipped with the exact reason. Nothing here is a fleet, cloud,
# media, power-loss, or production claim.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

stamp="${WEFT_PLANES_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
receipt="bench/receipts/planes-${stamp}.json"
log="bench/receipts/planes-${stamp}.log"
tests_log="bench/receipts/planes-${stamp}.tests.log"
tests_summary="bench/receipts/planes-${stamp}.tests.txt"
work="${WEFT_PLANES_ROOT:-/tmp/weft-planes-court}"

{
  echo "== blob/commit plane court =="
  echo "utc=${stamp}"
  echo "court_image=${WEFT_COURT_IMAGE:-unknown}"
  echo "court_image_id=${WEFT_COURT_IMAGE_ID:-unknown}"
  echo "memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
  echo "revision_weftfs=${WEFT_REPO_WEFTFS:-unknown}"
  echo "s3_endpoint=${WEFT_S3_TEST_ENDPOINT:-unset}"
  echo "s3_server=${WEFT_S3_SERVER_LABEL:-unset}"
  echo
} | tee "$log"

# 1. The plane court itself: split identity, capability refusal, process-fault
#    crash proxy. S3 cases skip-pass without an endpoint.
tests_rc=0
if cargo test --release --test planes -- --test-threads=1 >"$tests_log" 2>&1; then
  tests_rc=0
else
  tests_rc=1
fi
cat "$tests_log" >>"$log"
grep -E '^(test |test result)' "$tests_log" >"$tests_summary" || true

echo "---- planes test summary ----" | tee -a "$log"
cat "$tests_summary" | tee -a "$log"

# 2. The measurement harness, one child process per layout.
cargo build --release --example planes_bench >>"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/planes_bench" \
  --out "$receipt" \
  --root "$work" \
  --tests-summary "$tests_summary" 2>&1 | tee -a "$log"

echo "planes receipt: $receipt" | tee -a "$log"
echo "raw log: $log" | tee -a "$log"
sha256sum "$receipt" "$log" "$tests_summary" | tee -a "$log"

if [[ "$tests_rc" != 0 ]]; then
  echo "planes_court: the plane test court reported failures; see $tests_log" >&2
  exit 1
fi
