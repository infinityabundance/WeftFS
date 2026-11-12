#!/usr/bin/env bash
# DCP metadata serialisation cost court (R12c item 3).
#
# Builds real torch.distributed.checkpoint metadata at 1, 16, 64, 169 and 1024
# entries and measures the pure-Python canonical WCB encoder that `finish()`
# calls, split into tree construction, encoder, memfd copy and hashing.
#
#   scripts/docker-court.sh python "bench/scripts/dcp_metadata_court.sh"
#
# Optional environment (forwarded by scripts/docker-court.sh):
#   WEFT_DCPMETA_STAMP, WEFT_DCPMETA_RECEIPT, WEFT_DCPMETA_WORK,
#   WEFT_DCPMETA_SIZES, WEFT_DCPMETA_ITERATIONS
#
# The receipt and the raw log are written beside each other under
# bench/receipts/. A missing court binary fails closed.
set -Eeuo pipefail

cd /work
export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"
export WEFT_PHASE_PROFILE=0

stamp="${WEFT_DCPMETA_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_DCPMETA_RECEIPT:-bench/receipts/dcp-metadata-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_DCPMETA_WORK:-/tmp/weft-dcp-metadata}"

[[ -x /work/.docker-cache/target-rust/release/weftfs ]] || {
  echo "dcp_metadata_court: missing release binary at /work/.docker-cache/target-rust/release/weftfs" >&2
  exit 1
}
mkdir -p "$(dirname "$receipt")"

args=(run --receipt "$receipt" --stamp "$stamp" --work-dir "$work")
[[ -n "${WEFT_DCPMETA_SIZES:-}" ]] && args+=(--sizes "$WEFT_DCPMETA_SIZES")
[[ -n "${WEFT_DCPMETA_ITERATIONS:-}" ]] && args+=(--iterations "$WEFT_DCPMETA_ITERATIONS")

echo "dcp-metadata court: receipt=$receipt"
python3 python/courts/dcp_metadata_cost.py "${args[@]}" 2>&1 | tee "$log"
