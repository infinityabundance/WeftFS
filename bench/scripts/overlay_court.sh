#!/usr/bin/env bash
# R9 overlay court: chunk-COW vs immutable write overlays, with compaction.
#
# Run inside the OOM-protected bench court:
#
#   bash scripts/docker-court.sh bench "cd /work && bash bench/scripts/overlay_court.sh"
#   bash scripts/s3_court.sh --overlay            # same court, MinIO backend
#
# Workload (deterministic, seeded): one 128 MiB object with 64 KiB chunks, then
# 500 overwrites of 4 KiB at 4 KiB-aligned pseudo-random offsets, through
# (a) the existing Merkle chunk-COW writer and (b) the immutable v4 overlay
# writer. Both use the same content identity, so their content roots must be
# equal. Each configuration runs in its own child process and reports write
# p50/p99, backend requests and bytes, a 4 KiB range read, a full read,
# `verify --full`, retained blob bytes and (local only) on-disk bytes. The
# overlay configuration creates a snapshot, compacts, and proves the root, the
# object bytes and the snapshot bytes are unchanged; the COW configuration
# records the documented `UnsupportedRepresentation` refusal.
#
# The receipt is bench/receipts/overlay-<UTC>.json with the raw log beside it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

backend="local"
for arg in "$@"; do
  case "$arg" in
    --s3) backend="s3" ;;
    --local) backend="local" ;;
    -h|--help)
      sed -n '2,30p' "$0"
      exit 0
      ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="${WEFT_OVERLAY_RECEIPT:-bench/receipts/overlay-${stamp}.json}"
log="${WEFT_OVERLAY_LOG:-bench/receipts/overlay-${stamp}.log}"

export WEFT_OVERLAY_COURT_BACKEND="$backend"

{
  echo "== overlay court =="
  echo "backend=$backend"
  echo "utc=$stamp"
  echo "court_image=${WEFT_COURT_IMAGE:-unknown}"
  echo "court_image_id=${WEFT_COURT_IMAGE_ID:-unknown}"
  echo "memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
  echo "revision_weftfs=${WEFT_REPO_WEFTFS:-unknown}"
  echo "revision_weftfs_go=${WEFT_REPO_WEFTFS_GO:-unknown}"
  echo "revision_weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
  if [[ "$backend" == "s3" ]]; then
    echo "s3_endpoint=${WEFT_S3_ENDPOINT:-unset}"
    echo "s3_bucket=${WEFT_S3_BUCKET:-unset}"
    echo "s3_server=${WEFT_S3_SERVER_LABEL:-unknown}"
  fi
  echo
} | tee "$log"

cargo build --release --example overlay_bench >>"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
"$target/release/examples/overlay_bench" --out "$receipt" 2>&1 | tee -a "$log"

echo "overlay receipt: $receipt" | tee -a "$log"
echo "raw log: $log" | tee -a "$log"
sha256sum "$receipt" "$log" | tee -a "$log"
