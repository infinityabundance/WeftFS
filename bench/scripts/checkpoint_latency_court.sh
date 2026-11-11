#!/usr/bin/env bash
# Checkpoint completion latency court (subphase R12).
#
# Measures the R11 training-goodput checkpoint (169 entries, ~5 MB) through the
# shipped PyTorch DCP adapter and decomposes the completion time into client
# and service phases. Runs inside the Docker python court:
#
#   scripts/docker-court.sh python "bench/scripts/checkpoint_latency_court.sh"
#
# Optional environment (forwarded by scripts/docker-court.sh):
#   WEFT_LATENCY_STAMP, WEFT_LATENCY_RECEIPT, WEFT_LATENCY_WORK,
#   WEFT_LATENCY_REPS, WEFT_LATENCY_FRESH
#
# The receipt and the raw log are written beside each other under
# bench/receipts/. A missing court binary fails closed.
set -Eeuo pipefail

cd /work
export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"
export WEFT_PHASE_PROFILE=0

stamp="${WEFT_LATENCY_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_LATENCY_RECEIPT:-bench/receipts/checkpoint-latency-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_LATENCY_WORK:-/tmp/weft-checkpoint-latency}"

[[ -x /work/.docker-cache/target-rust/release/weftfs ]] || {
  echo "checkpoint_latency_court: missing release binary at /work/.docker-cache/target-rust/release/weftfs" >&2
  exit 1
}
mkdir -p "$(dirname "$receipt")"

args=(run --receipt "$receipt" --stamp "$stamp" --work-dir "$work")
[[ -n "${WEFT_LATENCY_REPS:-}" ]] && args+=(--reps "$WEFT_LATENCY_REPS")
[[ -n "${WEFT_LATENCY_FRESH:-}" ]] && args+=(--fresh "$WEFT_LATENCY_FRESH")

echo "checkpoint-latency court: receipt=$receipt"
python3 python/courts/checkpoint_latency.py "${args[@]}" 2>&1 | tee "$log"
