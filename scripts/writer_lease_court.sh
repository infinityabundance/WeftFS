#!/usr/bin/env bash
# X09 court: writer session lease — expiry, renewal, fencing, fairness, and the
# cost the lease adds to a publication.
#
# Runs the harness in the rust court (it needs no FUSE device and no Python: it
# assembles its own JSON, which this script merges into a receipt). The
# process-fault cases (SIGKILL, SIGSTOP/SIGCONT) re-execute the same binary as a
# real second process.
#
# Produces bench/receipts/writer-lease-<stamp>.json and a raw log beside it.
# Exit 0 on a complete run, 78 with one `writer_lease_court: skip <reason>` line
# when the environment cannot run it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "writer_lease_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
receipt="bench/receipts/writer-lease-${stamp}.json"
log="bench/receipts/writer-lease-${stamp}.log"
work=".docker-cache/writer-lease-court"
container_work="/work/${work}"

{
  echo "== writer lease court ${stamp} =="
  echo "revision: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "kernel: $(uname -r)"
} > "$log"

status=0
if scripts/docker-court.sh rust "
  set -Eeuo pipefail
  cd /work
  (cargo build --release --quiet --example writer_lease_court 2>&1 | grep -E '^error' -A 10 | head -40) || true
  rm -rf ${container_work}
  mkdir -p ${container_work}
  .docker-cache/target-rust/release/examples/writer_lease_court \
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
    print(f'writer_lease_court: no court.json at {court_path}', file=sys.stderr)
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
receipt = {
    'schema': 'weft.receipt.writer-lease.v1',
    'gate': 'X09',
    'stamp': stamp,
    'revision': {'weftfs': revision('.')},
    'method': {
        'court': 'rust',
        'limits': {'memory': '3g', 'memory_swap': '3g', 'pids': 1024, 'cpus': 4},
        'store': 'local backend, optimistic (MW2) writer mode, one store per case',
        'faults': 'real SIGKILL and SIGSTOP/SIGCONT of a second process (the same binary)',
        'clock': 'host wall clock; no fixed timestamps (a lease deadline is a real duration)',
        'notes': [
            'one host, warm page cache; no fleet, cloud or media claim',
            'acquisition latency includes the whole optimistic open (state recovery, policy record)',
            'the fairness control arm (--no-lease-fairness) is reported, not asserted',
        ],
    },
    'environment': court.get('environment', {}),
    'cases': court.get('cases', []),
    'checks': checks,
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
