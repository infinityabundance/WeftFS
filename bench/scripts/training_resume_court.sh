#!/usr/bin/env bash
# T055: complete training state, SIGKILL, exact resume (WeftFS + DCP baseline).
#
#   scripts/docker-court.sh python "bash bench/scripts/training_resume_court.sh"
#
# CPU only, single host, one thread. No GPU and no multi-host claim.
set -Eeuo pipefail

cd /work
stamp="${WEFT_RESUME_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_RESUME_RECEIPT:-bench/receipts/training-resume-${stamp}.json}"
dcp_receipt="${WEFT_RESUME_DCP_RECEIPT:-bench/receipts/training-resume-dcp-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_RESUME_WORK:-/work/.docker-cache/training-resume-${stamp}}"
cli="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"

skip() {
  echo "training_resume_court: skip $1"
  exit 78
}

[[ -x "$cli" ]] || skip "missing release binary $cli"
mkdir -p "$(dirname "$receipt")"

export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"

common=(
  run
  --stamp "$stamp"
  --cli "$cli"
  --steps "${WEFT_RESUME_STEPS:-48}"
  --resume-steps "${WEFT_RESUME_STEPS_AFTER:-16}"
  --checkpoint-every "${WEFT_RESUME_EVERY:-8}"
  --reps "${WEFT_RESUME_REPS:-2}"
  --kill-reps "${WEFT_RESUME_KILL_REPS:-2}"
  --scenarios "${WEFT_RESUME_SCENARIOS:-step,complete,inflight}"
)

echo "training-resume court: WeftFS arm" | tee "$log"
python3 python/courts/training_resume.py "${common[@]}" \
  --arm weft --receipt "$receipt" --work-dir "$work/weft" 2>&1 | tee -a "$log"

echo "training-resume court: DCP baseline arm" | tee -a "$log"
python3 python/courts/training_resume.py "${common[@]}" \
  --arm dcp --receipt "$dcp_receipt" --work-dir "$work/dcp" 2>&1 | tee -a "$log"

python3 - "$receipt" "$dcp_receipt" <<'PY'
import json, sys
results = {}
for path in sys.argv[1:]:
    receipt = json.load(open(path))
    results[path] = receipt["result"]
    print("%s: %s %s" % (path, receipt["result"], receipt["checks"]))
sys.exit(0 if all(value == "pass" for value in results.values()) else 1)
PY
