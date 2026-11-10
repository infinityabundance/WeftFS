#!/usr/bin/env bash
# Training-goodput court (subphase R11).
#
# A real small PyTorch training loop with asynchronous checkpoints, three
# configurations on identical steps and data, fresh-process exactness for every
# checkpoint, and SIGKILL resume old-or-new. Runs inside the Docker python court:
#
#   scripts/docker-court.sh python "bench/scripts/training_goodput_court.sh"
#
# Optional environment (all forwarded by scripts/docker-court.sh):
#   WEFT_GOODPUT_STAMP, WEFT_GOODPUT_RECEIPT, WEFT_GOODPUT_WORK,
#   WEFT_GOODPUT_STEPS, WEFT_GOODPUT_CKPT_EVERY, WEFT_GOODPUT_REPS,
#   WEFT_GOODPUT_KILL_REPS, WEFT_GOODPUT_KILL_STEPS,
#   WEFT_GOODPUT_KILL_AFTER_ACK_STEP, WEFT_GOODPUT_KILL_BEFORE_ACK_STEP
#
# Defaults are the published configuration: 256 steps, checkpoint every 64,
# 5 repetitions, 3 kill/resume repetitions. The receipt and the raw log are
# written beside each other under bench/receipts/.
set -Eeuo pipefail
set -o pipefail

cd /work
export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"

stamp="${WEFT_GOODPUT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_GOODPUT_RECEIPT:-bench/receipts/training-goodput-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_GOODPUT_WORK:-/tmp/weft-training-goodput}"

args=(run --receipt "$receipt" --work-dir "$work")
[[ -n "${WEFT_GOODPUT_STEPS:-}" ]] && args+=(--steps "$WEFT_GOODPUT_STEPS")
[[ -n "${WEFT_GOODPUT_CKPT_EVERY:-}" ]] && args+=(--checkpoint-every "$WEFT_GOODPUT_CKPT_EVERY")
[[ -n "${WEFT_GOODPUT_REPS:-}" ]] && args+=(--reps "$WEFT_GOODPUT_REPS")
[[ -n "${WEFT_GOODPUT_KILL_REPS:-}" ]] && args+=(--kill-reps "$WEFT_GOODPUT_KILL_REPS")
[[ -n "${WEFT_GOODPUT_KILL_STEPS:-}" ]] && args+=(--kill-steps "$WEFT_GOODPUT_KILL_STEPS")
[[ -n "${WEFT_GOODPUT_KILL_AFTER_ACK_STEP:-}" ]] && args+=(--kill-after-ack-step "$WEFT_GOODPUT_KILL_AFTER_ACK_STEP")
[[ -n "${WEFT_GOODPUT_KILL_BEFORE_ACK_STEP:-}" ]] && args+=(--kill-before-ack-step "$WEFT_GOODPUT_KILL_BEFORE_ACK_STEP")

echo "training-goodput court: receipt=$receipt"
python3 python/courts/training_goodput.py "${args[@]}" 2>&1 | tee "$log"
