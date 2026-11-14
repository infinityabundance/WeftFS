#!/usr/bin/env bash
# T053 rank-directed cross-topology restore court.
#
#   scripts/docker-court.sh bench "bash bench/scripts/rank_restore_court.sh"
#
# Writes a real 8-way (axis 0) checkpoint from the cached Qwen2.5-0.5B-Instruct
# container, then asks every rank of every target topology for only its own
# extents, compares every byte with a full-tensor restore, and reports read
# amplification per tensor and in aggregate.
#
# Environment:
#   WEFT_RANKRESTORE_STAMP    receipt stamp (default: UTC now)
#   WEFT_RANKRESTORE_RECEIPT  receipt path
#   WEFT_RANKRESTORE_SOURCE   safetensors container
#   WEFT_RANKRESTORE_STORE    store root
#   WEFT_RANKRESTORE_TOPOLOGIES   default 1,2,4,8
#   WEFT_RANKRESTORE_WORK     work directory
#
# CPU only, single host. No GPU and no multi-host claim.
set -Eeuo pipefail

cd /work
stamp="${WEFT_RANKRESTORE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_RANKRESTORE_RECEIPT:-bench/receipts/rank-restore-${stamp}.json}"
log="${receipt%.json}.log"
waiting_log="${receipt%.json}.rank-restore.log"
work="${WEFT_RANKRESTORE_WORK:-/work/.docker-cache/rank-restore}"
source_file="${WEFT_RANKRESTORE_SOURCE:-/work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors}"
store="${WEFT_RANKRESTORE_STORE:-${work}/store}"
binary="/work/.docker-cache/target-rust/release/examples/rank_restore_court"
weftfs="/work/.docker-cache/target-rust/release/weftfs"

skip() {
  echo "rank_restore_court: skip $1"
  exit 78
}

[[ -x "$weftfs" ]] || skip "missing release binary $weftfs"
[[ -x "$binary" ]] || skip "missing example binary $binary (cargo build --release --example rank_restore_court)"
[[ -f "$source_file" ]] || skip "missing corpus $source_file"

mkdir -p "$(dirname "$receipt")" "$work"

if [[ ! -f "$store/FORMAT" ]]; then
  "$weftfs" --store "$store" init --chunk-size 65536 --checkpoint-interval 256 --gc-grace-secs 0 \
    >>"$log" 2>&1
fi

if ! "$weftfs" --store "$store" --json checkpoint list 2>>"$log" | grep -q '"qwen"'; then
  echo "rank-restore court: writing 8-way axis-0 checkpoint (this is the ingest)" | tee -a "$log"
  /usr/bin/time -v "$weftfs" --store "$store" --json checkpoint write \
    --file "$source_file" --name qwen --shards 8 --axis 0 \
    >>"$log" 2>&1 || skip "checkpoint write failed; see $log"
fi

"$binary" \
  --store "$store" \
  --checkpoint qwen \
  --source "$source_file" \
  --receipt "$receipt" \
  --stamp "$stamp" \
  --topologies "${WEFT_RANKRESTORE_TOPOLOGIES:-1,2,4,8}" \
  --axis "${WEFT_RANKRESTORE_AXIS:-0}" \
  --cross-axis "${WEFT_RANKRESTORE_CROSS_AXIS:-1}" \
  --cross-shards "${WEFT_RANKRESTORE_CROSS_SHARDS:-4}" \
  --non-divisible "${WEFT_RANKRESTORE_NON_DIVISIBLE:-3}" \
  2>&1 | tee "$waiting_log"

"$weftfs" --store "$store" --json verify --full >>"$log" 2>&1 || {
  echo "rank_restore_court: fail store verification after the court" >&2
  exit 1
}

echo "rank_restore_court: pass receipt=$receipt"
