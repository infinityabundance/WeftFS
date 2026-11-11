#!/usr/bin/env bash
# Training-goodput session sweep (R12c subphase B).
#
# The R11/R12b goodput fixture (169 entries, ~5 MB) driven through two
# configurations on identical steps and data:
#
#   weft          - one `save_checkpoint_async` call per checkpoint, so every
#                   save starts and closes its own checkpoint service;
#   weft-session  - one reusable `CheckpointSession` per (store, run), reused by
#                   every checkpoint in the loop.
#
# It sweeps 4, 8, 16 and 32 steps between checkpoints (the 4-step cell is
# shorter than a save, so a save must wait for the previous one), reports cold
# and warm service separately, and runs the SIGKILL old-or-new contract through
# a session.
#
# Run inside the Docker python court, after building the release CLI in the
# rust court:
#
#   scripts/docker-court.sh rust "cargo build --release --features fuse --bin weftfs"
#   scripts/docker-court.sh python "bench/scripts/training_goodput_session_court.sh"
#
# Optional environment (forwarded by scripts/docker-court.sh):
#   WEFT_GOODPUT_STAMP, WEFT_GOODPUT_RECEIPT, WEFT_GOODPUT_WORK,
#   WEFT_GOODPUT_SWEEP_STEPS, WEFT_GOODPUT_SWEEP_REPS,
#   WEFT_GOODPUT_INTERVALS, WEFT_GOODPUT_CONFIGS, WEFT_GOODPUT_RESUME,
#   WEFT_GOODPUT_KILL_REPS, WEFT_GOODPUT_SESSION_IDLE_SECS
#
# The receipt and the raw log are written beside each other under
# bench/receipts/.
set -Eeuo pipefail
set -o pipefail

cd /work
export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"

cli="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"
[[ -x "$cli" ]] || {
  echo "training_goodput_session_court: missing release binary at $cli" >&2
  echo "build it first: scripts/docker-court.sh rust 'cargo build --release --features fuse --bin weftfs'" >&2
  exit 1
}

stamp="${WEFT_GOODPUT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_GOODPUT_RECEIPT:-bench/receipts/training-goodput-session-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_GOODPUT_WORK:-/tmp/weft-training-goodput-session}"
mkdir -p "$(dirname "$receipt")"

args=(sweep --receipt "$receipt" --stamp "$stamp" --work-dir "$work" --weftfs-cli "$cli")
[[ -n "${WEFT_GOODPUT_SWEEP_STEPS:-}" ]] && args+=(--steps "$WEFT_GOODPUT_SWEEP_STEPS")
[[ -n "${WEFT_GOODPUT_SWEEP_REPS:-}" ]] && args+=(--reps "$WEFT_GOODPUT_SWEEP_REPS")
[[ -n "${WEFT_GOODPUT_INTERVALS:-}" ]] && args+=(--intervals "$WEFT_GOODPUT_INTERVALS")
[[ -n "${WEFT_GOODPUT_CONFIGS:-}" ]] && args+=(--configs "$WEFT_GOODPUT_CONFIGS")
[[ -n "${WEFT_GOODPUT_RESUME:-}" ]] && args+=(--resume "$WEFT_GOODPUT_RESUME")
[[ -n "${WEFT_GOODPUT_KILL_REPS:-}" ]] && args+=(--kill-reps "$WEFT_GOODPUT_KILL_REPS")
[[ -n "${WEFT_GOODPUT_SESSION_IDLE_SECS:-}" ]] && \
  args+=(--session-idle-secs "$WEFT_GOODPUT_SESSION_IDLE_SECS")

echo "training-goodput session court: receipt=$receipt"
python3 python/courts/training_goodput.py "${args[@]}" 2>&1 | tee "$log"
