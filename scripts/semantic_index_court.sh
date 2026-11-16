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
# Three repetitions, each with its own store. ObjectIds are drawn from a random
# seed, so the *id-sorted* implicit scan sees a different candidate order every
# run while the fingerprint index does not. The court measures that difference
# instead of hiding it behind a single lucky order.
reps=3
if scripts/docker-court.sh rust "
  set -Eeuo pipefail
  cd /work
  (cargo build --release --quiet --example semantic_index_court 2>&1 | grep -E '^error' -A 10 | head -40) || true
  rm -rf ${container_work}
  mkdir -p ${container_work}
  for rep in 1 2 3; do
    .docker-cache/target-rust/release/examples/semantic_index_court \
      --json ${container_work}/court-\$rep.json
  done
  echo rc=\$?
" >> "$log" 2>&1; then
  :
else
  status=1
fi

python3 - "$stamp" "$work" "$receipt" "$reps" <<'PY' || status=1
import json, os, subprocess, sys

stamp, work, receipt_path = sys.argv[1], sys.argv[2], sys.argv[3]
reps = int(sys.argv[4])
courts = []
for rep in range(1, reps + 1):
    court_path = os.path.join(work, f'court-{rep}.json')
    if not os.path.isfile(court_path):
        print(f'semantic_index_court: no court-{rep}.json at {court_path}', file=sys.stderr)
        sys.exit(1)
    courts.append(json.load(open(court_path)))
court = courts[0]

def revision(path='.'):
    try:
        return subprocess.run(['git', '-C', path, 'rev-parse', 'HEAD'],
                              capture_output=True, text=True, check=True).stdout.strip()
    except Exception:
        return 'unknown'

# One check per label, true only if every repetition asserted it. The scan arms
# depend on the random ObjectId order, so the per-rep detail is kept.
by_label = {}
for which, one in enumerate(courts, start=1):
    for check in one.get('checks', []):
        label = check.get('label')
        entry = by_label.get(label)
        if entry is None:
            entry = by_label[label] = {'label': label, 'ok': True, 'detail': ''}
        entry['ok'] = entry['ok'] and bool(check.get('ok'))
        detail = str(check.get('detail', ''))
        entry['detail'] = (entry['detail'] + ' | ' if entry['detail'] else '') + f'rep{which}: {detail}'
checks = list(by_label.values())

arms = court.get('arms', {})
scan_all = arms.get('scan_all', {})
index = arms.get('index', {})
scan_bound8 = arms.get('scan_bound8', {})
scan_default = arms.get('scan_default', {})
stale = arms.get('stale_index', {})

def series(name, field):
    return [one.get('arms', {}).get(name, {}).get(field) for one in courts]

repetitions = {
    'count': reps,
    'arms': {
        name: {
            'max_candidates': court.get('arms', {}).get(name, {}).get('max_candidates'),
            'semantic_bytes': series(name, 'semantic_bytes'),
            'chosen_physical_bytes': series(name, 'chosen_physical_bytes'),
            'chosen': series(name, 'chosen'),
            'plan_ms_p50': [round(one.get('arms', {}).get(name, {}).get('plan_ms', {}).get('p50', 0.0), 3)
                            for one in courts],
        }
        for name in ('scan_bound8', 'scan_default', 'scan_all', 'index', 'none', 'stale_index')
    },
}

index_sem = [one.get('arms', {}).get('index', {}).get('semantic_bytes') for one in courts]
exhaustive_sem = [one.get('arms', {}).get('scan_all', {}).get('semantic_bytes') for one in courts]
exhaustive_chosen = [one.get('arms', {}).get('scan_all', {}).get('chosen') for one in courts]
index_chosen = [one.get('arms', {}).get('index', {}).get('chosen') for one in courts]
default_sem = series('scan_default', 'semantic_bytes')
bound8_sem = series('scan_bound8', 'semantic_bytes')

checks.append({
    'label': f'the index matches the exhaustive scan in every one of {reps} repetitions',
    'ok': all(a == b for a, b in zip(index_sem, exhaustive_sem))
          and all(a == b for a, b in zip(index_chosen, exhaustive_chosen)),
    'detail': f'index {index_sem} vs exhaustive {exhaustive_sem}',
})
checks.append({
    'label': f'the index saving is identical in every one of {reps} repetitions',
    'ok': len(set(index_sem)) == 1,
    'detail': f'index {index_sem}; the bounded scans varied: default {default_sem}, bound8 {bound8_sem}',
})
failed = [check for check in checks if not check.get('ok')]

receipt = {
    'schema': 'weft.receipt.semantic-index.v1',
    'gate': 'X14',
    'stamp': stamp,
    'revision': {'weftfs': revision('.')},
    'fixture': court.get('fixture', {}),
    'repetitions': repetitions,
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
        'index_vs_scan_bound8_plan_ms_p50': (
            index.get('plan_ms', {}).get('p50', 0)
            - scan_bound8.get('plan_ms', {}).get('p50', 0)
        ),
        'scan_default_missed_semantic_bytes': scan_default.get('missed_vs_scan_all_bytes', 0),
        'scan_bound8_missed_semantic_bytes': scan_bound8.get('missed_vs_scan_all_bytes', 0),
        'stale_missed_semantic_bytes': stale.get('missed_semantic_bytes', 0),
        'scan_default_semantic_bytes_by_rep': default_sem,
        'scan_bound8_semantic_bytes_by_rep': bound8_sem,
        'index_semantic_bytes_by_rep': index_sem,
    },
    'method': {
        'court': 'rust',
        'limits': {'memory': '3g', 'memory_swap': '3g', 'pids': 1024, 'cpus': 4},
        'store': 'local backend, a fresh store per repetition, page cache warm',
        'repetitions': f'{reps} stores; ObjectIds are drawn from a random seed, so the id-sorted scan sees a different candidate order every repetition while the fingerprint index does not',
        'arms': [
            'scan_bound8: semantic_scan with max_candidates=8 (half the shipped default)',
            'scan_default: semantic_scan with the shipped default max_candidates',
            'scan_all: semantic_scan with max_candidates=20 (every compatible object)',
            'index: the persisted fingerprint index at the shipped default max_candidates',
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
            'build, lookup and plan times are from the first repetition',
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
