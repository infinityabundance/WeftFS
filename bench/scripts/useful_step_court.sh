#!/usr/bin/env bash
# T058: recovery-first useful computation (layer-scoped, labelled).
#
#   scripts/docker-court.sh python "bash bench/scripts/useful_step_court.sh"
#
# CPU only, single host. No GPU and no multi-host claim.
set -Eeuo pipefail

cd /work
stamp="${WEFT_USEFUL_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_USEFUL_RECEIPT:-bench/receipts/useful-step-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_USEFUL_WORK:-/work/.docker-cache/useful-step-${stamp}}"
cli="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"

skip() {
  echo "useful_step_court: skip $1"
  exit 78
}

[[ -x "$cli" ]] || skip "missing release binary $cli"
mkdir -p "$(dirname "$receipt")"

export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"

python3 python/courts/useful_step.py run \
  --receipt "$receipt" \
  --stamp "$stamp" \
  --work-dir "$work" \
  --cli "$cli" \
  --reps "${WEFT_USEFUL_REPS:-3}" 2>&1 | tee "$log"
