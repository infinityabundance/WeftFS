#!/usr/bin/env bash
# P06 court: online background transcode with a real budget.
#
# Measures foreground write latency with and without the worker, the worker's
# throughput against its declared budget, and a real SIGKILL of a second process
# running the worker.
#
# Produces bench/receipts/online-transcode-<stamp>.json (+ .log).
# Exit 0 on a complete run, 78 with one `online_transcode_court: skip <reason>`
# line when the environment cannot run it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "online_transcode_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
receipt="bench/receipts/online-transcode-${stamp}.json"
log="bench/receipts/online-transcode-${stamp}.log"
work=".docker-cache/online-transcode-court"
container_work="/work/${work}"

{
  echo "== online transcode court ${stamp} =="
  echo "revision: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "kernel: $(uname -r)"
} > "$log"

status=0
if scripts/docker-court.sh rust "
  set -Eeuo pipefail
  cd /work
  (cargo build --release --quiet --example online_transcode_court 2>&1 | grep -E '^error' -A 10 | head -40) || true
  rm -rf ${container_work}
  mkdir -p ${container_work}
  .docker-cache/target-rust/release/examples/online_transcode_court \
    --out ${container_work} --json ${container_work}/court.json
  echo rc=\$?
" >> "$log" 2>&1; then
  :
else
  status=1
fi

python3 - "$stamp" "$work" "$receipt" <<'PY' || status=1
import json, os, subprocess, sys

stamp, work, receipt_path = sys.argv[1], sys.argv[2], sys.argv[3]
court_path = os.path.join(work, 'court.json')
if not os.path.isfile(court_path):
    print(f'online_transcode_court: no court.json at {court_path}', file=sys.stderr)
    sys.exit(1)
court = json.load(open(court_path))

def revision(path='.'):
    try:
        return subprocess.run(['git', '-C', path, 'rev-parse', 'HEAD'],
                              capture_output=True, text=True, check=True).stdout.strip()
    except Exception:
        return 'unknown'

checks = court.get('checks', [])
failed = [check for check in checks if not check.get('ok')]
interference = court.get('interference', {})

receipt = {
    'schema': 'weft.receipt.online-transcode.v1',
    'gate': 'P06',
    'stamp': stamp,
    'revision': {'weftfs': revision('.')},
    'fixture': court.get('fixture', {}),
    'interference': interference,
    'throughput': court.get('throughput', {}),
    'crash': court.get('crash', {}),
    'deltas': {
        'foreground_p50_ratio': interference.get('p50_ratio', 0.0),
        'foreground_p99_ratio': interference.get('p99_ratio', 0.0),
        'worker_hold_share': interference.get('worker_hold_share', 0.0),
    },
    'method': {
        'court': 'rust',
        'limits': {'memory': '3g', 'memory_swap': '3g', 'pids': 1024, 'cpus': 4},
        'store': 'local backend; worker and foreground share one Store behind one mutex',
        'foreground_op': 'write_tensor_range over one row of a 1 MiB bf16 tensor',
        'arms': [
            'quiet: the same 60 foreground writes with no worker',
            'busy: the same 60 foreground writes with the worker active',
        ],
        'budget': 'StageBudget(max_active_workers=1, max_inflight_bytes=object bytes+1)',
        'crash': 'real SIGKILL of a second process running the worker',
        'notes': [
            'one host, warm page cache; no fleet, cloud or media claim',
            'the foreground operation dominates the measurement (a range write re-encodes '
            'the whole tensor), so interference is reported both as a ratio and as the '
            "worker's share of wall time held",
            'the fixture is synthetic structured BF16, not real weights',
        ],
    },
    'checks': checks,
    'environment': court.get('environment', {}),
    'summary': {
        'checks': len(checks),
        'passed': len(checks) - len(failed),
        'failed': len(failed),
    },
    'result': 'pass' if not failed else 'fail',
}
json.dump(receipt, open(receipt_path, 'w'), indent=2, sort_keys=True)
print(f'receipt: {receipt_path} (result={receipt["result"]}, {len(checks) - len(failed)}/{len(checks)} checks)')
for check in failed:
    print(f'FAILED {check.get("label")}: {check.get("detail")}')
sys.exit(0 if not failed else 1)
PY

exit "$status"
