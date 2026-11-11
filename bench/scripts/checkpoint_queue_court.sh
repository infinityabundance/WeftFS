#!/usr/bin/env bash
# Checkpoint queue court (R12c item 4).
#
# Three sections, each its own receipt:
#
#   sweep     the interval sweep (1, 2, 4, 8, 16, 32 steps) over configurations;
#   overload  N concurrent submissions that never wait for the previous save;
#   bound     the `max_outstanding` sweep that picks the default bound.
#
# The fixture and the loop are `python/courts/training_goodput.py`'s own code,
# so every cell is the R11/R12b 169-entry ~5 MB checkpoint by construction and
# every checkpoint is restored by a separate process and compared exactly.
#
# Run inside the Docker python court, after building the release CLI:
#
#   scripts/docker-court.sh rust "cargo build --release --features fuse --bin weftfs"
#   WEFT_QUEUE_SECTION=sweep WEFT_QUEUE_PHASE=baseline \
#   WEFT_QUEUE_INTERVALS=1,2,4,8,16,32 WEFT_QUEUE_CONFIGS=weft,weft-session \
#     scripts/docker-court.sh python "bench/scripts/checkpoint_queue_court.sh"
#
# Optional environment (forwarded by scripts/docker-court.sh):
#   WEFT_QUEUE_STAMP, WEFT_QUEUE_RECEIPT, WEFT_QUEUE_WORK, WEFT_QUEUE_SECTION,
#   WEFT_QUEUE_PHASE, WEFT_QUEUE_STEPS, WEFT_QUEUE_REPS, WEFT_QUEUE_INTERVALS,
#   WEFT_QUEUE_CONFIGS, WEFT_QUEUE_RESUME, WEFT_QUEUE_BOUNDS, WEFT_QUEUE_COUNTS,
#   WEFT_QUEUE_MAX_OUTSTANDING, WEFT_QUEUE_MAX_STAGED_BYTES, WEFT_QUEUE_POLICY,
#   WEFT_QUEUE_WAIT_MS, WEFT_QUEUE_SESSION_IDLE_SECS
#
# The receipt and the raw log are written beside each other under
# bench/receipts/.
set -Eeuo pipefail

cd /work
export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"

cli="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"
[[ -x "$cli" ]] || {
  echo "checkpoint_queue_court: missing release binary at $cli" >&2
  echo "build it first: scripts/docker-court.sh rust 'cargo build --release --features fuse --bin weftfs'" >&2
  exit 1
}

section="${WEFT_QUEUE_SECTION:-sweep}"
case "$section" in
  sweep|overload|bound) ;;
  *) echo "checkpoint_queue_court: unknown section '$section'" >&2; exit 64 ;;
esac

stamp="${WEFT_QUEUE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
phase="${WEFT_QUEUE_PHASE:-after}"
receipt="${WEFT_QUEUE_RECEIPT:-bench/receipts/checkpoint-queue-${section}-${phase}-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_QUEUE_WORK:-/tmp/weft-checkpoint-queue-${section}-${phase}}"
mkdir -p "$(dirname "$receipt")"

args=("$section" --receipt "$receipt" --stamp "$stamp" --phase "$phase"
      --work-dir "$work" --weftfs-cli "$cli")
[[ -n "${WEFT_QUEUE_STEPS:-}" ]] && args+=(--steps "$WEFT_QUEUE_STEPS")
[[ -n "${WEFT_QUEUE_REPS:-}" ]] && args+=(--reps "$WEFT_QUEUE_REPS")
[[ -n "${WEFT_QUEUE_RESUME:-}" ]] && args+=(--resume "$WEFT_QUEUE_RESUME")
[[ -n "${WEFT_QUEUE_SESSION_IDLE_SECS:-}" ]] && \
  args+=(--session-idle-secs "$WEFT_QUEUE_SESSION_IDLE_SECS")
[[ -n "${WEFT_QUEUE_MAX_OUTSTANDING:-}" ]] && \
  args+=(--queue-max-outstanding "$WEFT_QUEUE_MAX_OUTSTANDING")
[[ -n "${WEFT_QUEUE_MAX_STAGED_BYTES:-}" ]] && \
  args+=(--queue-max-staged-bytes "$WEFT_QUEUE_MAX_STAGED_BYTES")
[[ -n "${WEFT_QUEUE_POLICY:-}" ]] && \
  args+=(--queue-admission-policy "$WEFT_QUEUE_POLICY")
[[ -n "${WEFT_QUEUE_WAIT_MS:-}" ]] && \
  args+=(--queue-admission-wait-ms "$WEFT_QUEUE_WAIT_MS")

case "$section" in
  sweep)
    [[ -n "${WEFT_QUEUE_INTERVALS:-}" ]] && args+=(--intervals "$WEFT_QUEUE_INTERVALS")
    [[ -n "${WEFT_QUEUE_CONFIGS:-}" ]] && args+=(--configs "$WEFT_QUEUE_CONFIGS")
    ;;
  overload)
    [[ -n "${WEFT_QUEUE_COUNTS:-}" ]] && args+=(--counts "$WEFT_QUEUE_COUNTS")
    ;;
  bound)
    [[ -n "${WEFT_QUEUE_BOUNDS:-}" ]] && args+=(--bounds "$WEFT_QUEUE_BOUNDS")
    [[ -n "${WEFT_QUEUE_INTERVALS:-}" ]] && args+=(--intervals "$WEFT_QUEUE_INTERVALS")
    ;;
esac

echo "checkpoint queue court: section=$section phase=$phase receipt=$receipt"
echo "checkpoint queue court: args=${args[*]}"
python3 python/courts/checkpoint_queue.py "${args[@]}" 2>&1 | tee "$log"
