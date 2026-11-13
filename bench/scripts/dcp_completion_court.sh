#!/usr/bin/env bash
# DCP checkpoint completion court: interleaved encoder arms + durability control.
#
#   scripts/docker-court.sh python "bench/scripts/dcp_completion_court.sh"
#
# Environment (forwarded by scripts/docker-court.sh):
#   WEFT_DCPCOMPLETION_STAMP, WEFT_DCPCOMPLETION_RECEIPT,
#   WEFT_DCPCOMPLETION_WORK, WEFT_DCPCOMPLETION_REPS
set -Eeuo pipefail

cd /work
export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"
export WEFT_PHASE_PROFILE=0

stamp="${WEFT_DCPCOMPLETION_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_DCPCOMPLETION_RECEIPT:-bench/receipts/dcp-latency-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_DCPCOMPLETION_WORK:-/tmp/weft-dcp-completion}"

[[ -x /work/.docker-cache/target-rust/release/weftfs ]] || {
  echo "dcp_completion_court: missing release binary at /work/.docker-cache/target-rust/release/weftfs" >&2
  exit 1
}
mkdir -p "$(dirname "$receipt")"

args=(--receipt "$receipt" --stamp "$stamp" --work-dir "$work" --weftfs-cli /work/.docker-cache/target-rust/release/weftfs)
[[ -n "${WEFT_DCPCOMPLETION_REPS:-}" ]] && args+=(--reps "$WEFT_DCPCOMPLETION_REPS")

echo "dcp-completion court: receipt=$receipt"
python3 python/courts/dcp_completion.py "${args[@]}" 2>&1 | tee "$log"
