#!/usr/bin/env bash
# T057: asynchronous save overlapping a sustained training loop.
#
#   scripts/docker-court.sh python "bash bench/scripts/async_save_court.sh"
#
# CPU only, single host. GPU idle time is unmeasured and is not claimed.
set -Eeuo pipefail

cd /work
stamp="${WEFT_ASYNC_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_ASYNC_RECEIPT:-bench/receipts/async-save-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_ASYNC_WORK:-/work/.docker-cache/async-overlap-${stamp}}"
cli="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"

skip() {
  echo "async_save_court: skip $1"
  exit 78
}

[[ -x "$cli" ]] || skip "missing release binary $cli"
mkdir -p "$(dirname "$receipt")"

export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"
args=(
  run
  --receipt "$receipt"
  --stamp "$stamp"
  --work-dir "$work"
  --cli "$cli"
  --steps "${WEFT_ASYNC_STEPS:-160}"
  --every "${WEFT_ASYNC_EVERY:-16}"
  --reps "${WEFT_ASYNC_REPS:-3}"
  --queue-outstanding "${WEFT_ASYNC_OUTSTANDING:-2}"
)
python3 python/courts/async_save_overlap.py "${args[@]}" 2>&1 | tee "$log"
