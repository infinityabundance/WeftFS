#!/usr/bin/env bash
# R11 physical object pack court.
#
# Compares four configurations on one deterministic workload:
#
#   local-flat     local backend, one physical object per blob
#   local-packed   local backend, small blobs grouped into packs
#   s3-flat        MinIO/S3, one physical object per blob
#   s3-packed      MinIO/S3, small blobs grouped into packs
#
# For each configuration it publishes 10,000 one-kilobyte files in one group
# commit, reads 1,000 chunk blobs sequentially and 1,000 randomly (cold and warm
# catalog), reads one 4 KiB range out of an isolated 16 MiB pack, and runs
# `verify --full` on the writer and after a read-only reopen. It reports object
# requests, bytes written/read, p50/p99 latency, physical footprint, pack
# counters (sealed packs, packed/flat members, catalog entries), and peak RSS.
#
# Run inside the OOM-protected bench court:
#
#   bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/packs_court.sh"
#   bash scripts/s3_court.sh --packs     # same court, MinIO started for you
#
# Without `WEFT_S3_TEST_ENDPOINT` only the local configurations run; the S3
# configurations are recorded as skipped with the exact reason. Nothing here is
# a cloud, fleet, media, power-loss, or production claim.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

stamp="${WEFT_PACKS_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
receipt="bench/receipts/packs-${stamp}.json"
log="bench/receipts/packs-${stamp}.log"
tests_log="bench/receipts/packs-${stamp}.tests.log"
tests_summary="bench/receipts/packs-${stamp}.tests.txt"
work="${WEFT_PACKS_ROOT:-/tmp/weft-packs-court}"

{
  echo "== physical object pack court =="
  echo "utc=${stamp}"
  echo "court_image=${WEFT_COURT_IMAGE:-unknown}"
  echo "court_image_id=${WEFT_COURT_IMAGE_ID:-unknown}"
  echo "memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
  echo "revision_weftfs=${WEFT_REPO_WEFTFS:-unknown}"
  echo "s3_endpoint=${WEFT_S3_TEST_ENDPOINT:-unset}"
  echo "s3_server=${WEFT_S3_SERVER_LABEL:-unset}"
  echo "work_root=${work}"
  echo
} | tee "$log"

# 1. The pack plane court itself: exactness, narrow range reads, digest and
#    truncation failures, GC reclamation, the catalog-bound fallback, layout
#    determinism, and the two-phase pack crash proxy.
tests_rc=0
if cargo test --release --test packs -- --test-threads=1 >"$tests_log" 2>&1; then
  tests_rc=0
else
  tests_rc=1
fi
cat "$tests_log" >>"$log"
grep -E '^(test |test result)' "$tests_log" >"$tests_summary" || true

echo "---- packs test summary ----" | tee -a "$log"
cat "$tests_summary" | tee -a "$log"

# 2. The measurement harness, one child process per configuration.
cargo build --release --example packs_bench >>"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/packs_bench" \
  --out "$receipt" \
  --root "$work" \
  --tests-summary "$tests_summary" 2>&1 | tee -a "$log"

echo "packs receipt: $receipt" | tee -a "$log"
echo "raw log: $log" | tee -a "$log"
sha256sum "$receipt" "$log" "$tests_summary" | tee -a "$log"

if [[ "$tests_rc" != 0 ]]; then
  echo "packs_court: the pack test court reported failures; see $tests_log" >&2
  exit 1
fi
