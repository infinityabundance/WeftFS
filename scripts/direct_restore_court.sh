#!/usr/bin/env bash
# X18 court: direct-to-destination verified reconstruction.
#
# Runs the harness in the rust court. The harness checks byte equality against
# the saved tensors and the ordinary restore, and measures wall, CPU and RSS for
# five arms (container+copy, container without SHA-256, direct into caller
# buffers, ordinary rank restore, direct row slab).
#
# Produces bench/receipts/direct-restore-<stamp>.json and a raw log beside it.
# Exit 0 on a complete run, 78 with one `direct_restore_court: skip <reason>`
# line when the environment cannot run it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "direct_restore_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
receipt="bench/receipts/direct-restore-${stamp}.json"
log="bench/receipts/direct-restore-${stamp}.log"
work=".docker-cache/direct-restore-court"
container_work="/work/${work}"
reps="${WEFT_DIRECT_RESTORE_REPS:-5}"

{
  echo "== direct restore court ${stamp} =="
  echo "revision: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "kernel: $(uname -r)"
  echo "reps: ${reps}"
} > "$log"

status=0
if scripts/docker-court.sh rust "
  set -Eeuo pipefail
  cd /work
  (cargo build --release --quiet --example direct_restore_court 2>&1 | grep -E '^error' -A 10 | head -40) || true
  rm -rf ${container_work}
  mkdir -p ${container_work}
  .docker-cache/target-rust/release/examples/direct_restore_court \
    --out ${container_work} --json ${container_work}/court.json --reps ${reps}
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
    print(f'direct_restore_court: no court.json at {court_path}', file=sys.stderr)
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
arms = {arm['arm']: arm for arm in court.get('arms', [])}

def p50(name, field='wall_ms'):
    return arms.get(name, {}).get(field, {}).get('p50')

receipt = {
    'schema': 'weft.receipt.direct-restore.v1',
    'gate': 'X18',
    'stamp': stamp,
    'revision': {'weftfs': revision('.')},
    'fixture': court.get('fixture', {}),
    'reps': court.get('reps'),
    'method': {
        'court': 'rust',
        'limits': {'memory': '3g', 'memory_swap': '3g', 'pids': 1024, 'cpus': 4},
        'store': 'local backend, read-only handle over one tiled checkpoint',
        'arms': [
            'container_then_copy: restore_checkpoint_pipelined + copy into per-tensor buffers',
            'container_unhashed: restore_checkpoint_pipelined_unhashed (isolates the SHA-256 pass)',
            'direct_into_destinations: restore_checkpoint_direct_into (no container buffer, no SHA-256)',
            'rank_ordinary: restore_checkpoint_rank (one quarter of the rows, per-tensor SHA-256)',
            'rank_direct_slab: restore_row_slab_direct_at (the same rows, per-tile roots, no SHA-256)',
        ],
        'rss': 'VmHWM is monotone within one process, so rss_delta is reported per arm in run order and is not an isolated per-arm peak',
        'notes': [
            'one host, warm page cache; no fleet, cloud or media claim',
            'the CPU column is getrusage user+sys, not wall',
            'tile roots authenticate each decoded tile; the frame blob is authenticated by its manifest id',
        ],
    },
    'arms': court.get('arms', []),
    'deltas': {
        'sha256_pass_ms_at_p50': (p50('container_then_copy') or 0) - (p50('container_unhashed') or 0),
        'direct_vs_container_then_copy_p50': (
            (p50('direct_into_destinations') or 0) - (p50('container_then_copy') or 0)
        ),
        'direct_vs_container_unhashed_p50': (
            (p50('direct_into_destinations') or 0) - (p50('container_unhashed') or 0)
        ),
        'rank_direct_vs_rank_ordinary_p50': (
            (p50('rank_direct_slab') or 0) - (p50('rank_ordinary') or 0)
        ),
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
