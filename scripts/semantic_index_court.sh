#!/usr/bin/env bash
# X14 court: indexed semantic candidate discovery.
#
# Runs the harness in the rust court. Arms: the historical scan with the default
# candidate bound, the exhaustive scan (every compatible object), the index at
# the same small bound, and no discovery at all. The court also measures the
# build, the lookup, and what a stale index costs.
#
# Produces bench/receipts/semantic-index-<stamp>.json and a raw log beside it.
# Exit 0 on a complete run, 78 with one `semantic_index_court: skip <reason>`
# line when the environment cannot run it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "semantic_index_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
receipt="bench/receipts/semantic-index-${stamp}.json"
log="bench/receipts/semantic-index-${stamp}.log"
work=".docker-cache/semantic-index-court"
container_work="/work/${work}"

{
  echo "== semantic index court ${stamp} =="
  echo "revision: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "kernel: $(uname -r)"
} > "$log"

status=0
if scripts/docker-court.sh rust "
  set -Eeuo pipefail
  cd /work
  (cargo build --release --quiet --example semantic_index_court 2>&1 | grep -E '^error' -A 10 | head -40) || true
  rm -rf ${container_work}
  mkdir -p ${container_work}
  .docker-cache/target-rust/release/examples/semantic_index_court \
    --json ${container_work}/court.json
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
    print(f'semantic_index_court: no court.json at {court_path}', file=sys.stderr)
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
arms = court.get('arms', {})
scan_all = arms.get('scan_all', {})
index = arms.get('index', {})
scan_default = arms.get('scan_default_bound', {})
stale = arms.get('stale_index', {})

receipt = {
    'schema': 'weft.receipt.semantic-index.v1',
    'gate': 'X14',
    'stamp': stamp,
    'revision': {'weftfs': revision('.')},
    'fixture': court.get('fixture', {}),
    'build': court.get('build', {}),
    'lookup': court.get('lookup', {}),
    'stale_lookup': court.get('stale_lookup', {}),
    'arms': arms,
    'deltas': {
        'index_vs_scan_all_semantic_bytes': (
            index.get('semantic_bytes', 0) - scan_all.get('semantic_bytes', 0)
        ),
        'index_vs_scan_all_physical_bytes': (
            index.get('chosen_physical_bytes', 0) - scan_all.get('chosen_physical_bytes', 0)
        ),
        'index_vs_scan_all_plan_ms_p50': (
            index.get('plan_ms', {}).get('p50', 0) - scan_all.get('plan_ms', {}).get('p50', 0)
        ),
        'index_vs_scan_default_plan_ms_p50': (
            index.get('plan_ms', {}).get('p50', 0)
            - scan_default.get('plan_ms', {}).get('p50', 0)
        ),
        'scan_default_missed_semantic_bytes': scan_default.get('missed_vs_scan_all_bytes', 0),
        'stale_missed_semantic_bytes': stale.get('missed_semantic_bytes', 0),
    },
    'method': {
        'court': 'rust',
        'limits': {'memory': '3g', 'memory_swap': '3g', 'pids': 1024, 'cpus': 4},
        'store': 'local backend, one store per run, page cache warm',
        'arms': [
            'scan_default_bound: semantic_scan with max_candidates=8 (the historical default)',
            'scan_all: semantic_scan with max_candidates=20 (every compatible object)',
            'index: the persisted fingerprint index, max_candidates=8',
            'none: no discovery',
            'stale_index: the index after every family-A tensor was unlinked',
        ],
        'note': 'lookup_ms includes reading the returned candidate tensors (8 MiB per query); '
                'the fingerprint comparison itself is reported separately by stale_lookup, '
                'where no candidate bytes are read',
        'notes': [
            'one host, warm page cache; no fleet, cloud or media claim',
            'the fixture is synthetic and generated in-process; no real model weights',
            'the index is a prefilter: every base is still priced by the real mode-14 encoder',
        ],
    },
    'checks': checks,
    'environment': court.get('environment', {}),
    'verification': court.get('verification', {}),
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
