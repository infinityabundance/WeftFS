#!/usr/bin/env bash
# X21 court: Go pooling, separated counters, and a restoration-shaped workload
# compared with Rust on the same fixture.
#
# Both languages read the *same* store directory, built once by the Rust
# harness. Arms: flat blobs, a "hot" set that repeats the same few blobs (where a
# pool can actually help), packed blobs, and one vanished chunk (both languages
# must fail closed).
#
# Produces bench/receipts/go-pool-counters-<stamp>.json (+ .log).
# Exit 0 on a complete run, 78 with one `go_pool_court: skip <reason>` line.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "go_pool_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
receipt="bench/receipts/go-pool-counters-${stamp}.json"
log="bench/receipts/go-pool-counters-${stamp}.log"
work=".docker-cache/go-pool-court"
container_work="/work/${work}"
rounds="${WEFT_GO_POOL_ROUNDS:-3}"
hot_rounds="${WEFT_GO_POOL_HOT_ROUNDS:-100}"

{
  echo "== go pool court ${stamp} =="
  echo "revision: $(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "weftfs-go: $(git -C WeftFS-Go rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "kernel: $(uname -r)"
  echo "rounds: ${rounds} hot_rounds: ${hot_rounds}"
} > "$log"

status=0
if scripts/docker-court.sh fuse "
  set -Eeuo pipefail
  cd /work
  (cargo build --release --quiet --example pool_restore_bench 2>&1 | grep -E '^error' -A 10 | head -40) || true
  rm -rf ${container_work}
  mkdir -p ${container_work}
  rust=.docker-cache/target-rust/release/examples/pool_restore_bench
  (cd WeftFS-Go && go build -o ${container_work}/poolbench ./cmd/poolbench)
  go=${container_work}/poolbench

  # flat fixture
  \$rust --build ${container_work}/flat --objects 64 --ids ${container_work}/flat.ids
  \$rust --store ${container_work}/flat --ids ${container_work}/flat.ids --rounds ${rounds} --json ${container_work}/rust-flat.json
  \$go --store ${container_work}/flat --ids ${container_work}/flat.ids --rounds ${rounds} --json ${container_work}/go-flat.json

  # hot fixture: the same four blobs, many rounds
  head -4 ${container_work}/flat.ids > ${container_work}/hot.ids
  \$rust --store ${container_work}/flat --ids ${container_work}/hot.ids --rounds ${hot_rounds} --json ${container_work}/rust-hot.json
  \$go --store ${container_work}/flat --ids ${container_work}/hot.ids --rounds ${hot_rounds} --json ${container_work}/go-hot.json

  # hot-single fixture: one object's blob set, well inside the 64-handle bound
  head -1 ${container_work}/flat.ids > ${container_work}/hot1.ids
  \$rust --store ${container_work}/flat --ids ${container_work}/hot1.ids --rounds 200 --json ${container_work}/rust-hot1.json
  \$go --store ${container_work}/flat --ids ${container_work}/hot1.ids --rounds 200 --json ${container_work}/go-hot1.json

  # packed fixture
  \$rust --build ${container_work}/packed --objects 64 --ids ${container_work}/packed.ids --packed
  \$rust --store ${container_work}/packed --ids ${container_work}/packed.ids --rounds ${rounds} --packed --json ${container_work}/rust-packed.json
  \$go --store ${container_work}/packed --ids ${container_work}/packed.ids --rounds ${rounds} --packed --json ${container_work}/go-packed.json

  # vanished fixture: one stored chunk deleted behind the store
  \$rust --build ${container_work}/vanished --objects 64 --ids ${container_work}/vanished.ids
  \$rust --vanish --store ${container_work}/vanished --ids ${container_work}/vanished.ids
  ( \$rust --store ${container_work}/vanished --ids ${container_work}/vanished.ids --rounds 1 --json ${container_work}/rust-vanished.json || true )
  ( \$go --store ${container_work}/vanished --ids ${container_work}/vanished.ids --rounds 1 --json ${container_work}/go-vanished.json || true )
  echo rc=0
" >> "$log" 2>&1; then
  :
else
  status=1
fi

python3 - "$stamp" "$work" "$receipt" <<'PY' || status=1
import json, os, subprocess, sys

stamp, work, receipt_path = sys.argv[1], sys.argv[2], sys.argv[3]

def load(name):
    path = os.path.join(work, name)
    if not os.path.isfile(path):
        return None
    return json.load(open(path))

def revision(path):
    try:
        return subprocess.run(['git', '-C', path, 'rev-parse', 'HEAD'],
                              capture_output=True, text=True, check=True).stdout.strip()
    except Exception:
        return 'unknown'

arms = {}
for name in ('flat', 'hot', 'hot1', 'packed', 'vanished'):
    arms[name] = {'rust': load(f'rust-{name}.json'), 'go': load(f'go-{name}.json')}

checks = []
def check(label, ok, detail):
    checks.append({'label': label, 'ok': bool(ok), 'detail': detail})

def counter(arm, language, key):
    document = arms[arm][language]
    if document is None:
        return None
    return document.get('counters', {}).get(key)

for arm in ('flat', 'packed'):
    rust, go = arms[arm]['rust'], arms[arm]['go']
    check(f'{arm}: both languages performed the same number of reads',
          rust and go and rust['reads'] == go['reads'],
          f"{rust and rust['reads']} vs {go and go['reads']}")
    check(f'{arm}: the total-blob counters agree',
          rust and go and counter(arm, 'rust', 'total_blob_reads') == counter(arm, 'go', 'total_blob_reads')
          and counter(arm, 'rust', 'total_blob_bytes') == counter(arm, 'go', 'total_blob_bytes'),
          f"reads {counter(arm,'rust','total_blob_reads')} vs {counter(arm,'go','total_blob_reads')}; "
          f"bytes {counter(arm,'rust','total_blob_bytes')} vs {counter(arm,'go','total_blob_bytes')}")
    if arm == 'flat':
        check('flat: the backend counters agree',
              rust and go and counter(arm, 'rust', 'backend_reads') == counter(arm, 'go', 'backend_reads'),
              f"backend reads {counter(arm,'rust','backend_reads')} vs {counter(arm,'go','backend_reads')}")
    else:
        # The packed planes issue a different number of physical reads per
        # member (Rust resolves the catalog and then ranges the member; the Go
        # plane caches the catalog). The logical counters still agree, so this is
        # published as an access-shape difference, not asserted equal.
        check('packed: the logical counters agree while the physical read shape differs (published)',
              rust and go
              and counter(arm, 'rust', 'total_blob_reads') == counter(arm, 'go', 'total_blob_reads'),
              f"backend reads rust {counter(arm,'rust','backend_reads')} vs go {counter(arm,'go','backend_reads')}")
    check(f'{arm}: no read failed',
          rust and go and rust['failures'] == 0 and go['failures'] == 0,
          f"failures {rust and rust['failures']} vs {go and go['failures']}")

vanished = arms['vanished']
check('vanished: both languages fail closed on a missing chunk',
      vanished['rust'] and vanished['go']
      and vanished['rust']['failures'] > 0 and vanished['go']['failures'] > 0,
      f"failures {vanished['rust'] and vanished['rust']['failures']} vs "
      f"{vanished['go'] and vanished['go']['failures']}; "
      f"first error rust={vanished['rust'] and vanished['rust']['first_error'][:80]!r} "
      f"go={vanished['go'] and vanished['go']['first_error'][:80]!r}")

hot = arms['hot']
hot1 = arms['hot1']
check('hot: the pool is exercised by repeated reads of the same blobs',
      hot['rust'] and hot['go']
      and hot['go']['counters'].get('pool_reuses', 0) > 0,
      f"go reuses {hot['go'] and hot['go']['counters'].get('pool_reuses')}, "
      f"opens {hot['go'] and hot['go'].get('pool_stats', {}).get('opens')}")
check('hot1: a working set inside the idle bound reuses most descriptors',
      hot1['go'] and hot1['go']['counters'].get('pool_reuses', 0) > hot1['go'].get('reads', 1) // 2,
      f"go reuses {hot1['go'] and hot1['go']['counters'].get('pool_reuses')} of "
      f"{hot1['go'] and hot1['go'].get('reads')} backend reads; "
      f"opens {hot1['go'] and hot1['go'].get('pool_stats', {}).get('opens')}")

def ratio(arm):
    rust, go = arms[arm]['rust'], arms[arm]['go']
    if not rust or not go or rust['wall_ms'] <= 0:
        return None
    return go['wall_ms'] / rust['wall_ms']

summary = {arm: ratio(arm) for arm in ('flat', 'hot', 'hot1', 'packed')}
failed = [c for c in checks if not c['ok']]
receipt = {
    'schema': 'weft.receipt.go-pool-counters.v1',
    'gate': 'X21',
    'stamp': stamp,
    'revision': {'weftfs': revision('.'), 'weftfs_go': revision('WeftFS-Go')},
    'arms': arms,
    'go_over_rust_wall_ratio': summary,
    'checks': checks,
    'method': {
        'court': 'fuse',
        'limits': {'memory': '3g', 'memory_swap': '3g', 'pids': 1024, 'cpus': 4},
        'fixture': 'one store per arm, built by the Rust harness; both languages read it',
        'workload': 'read every listed object once per round in a seeded random order',
        'counter_definitions': [
            'total_blob_reads: logical verified blob reads the store performed (one per ReadObject/ReadContent)',
            'backend_reads: physical blob-plane reads issued',
            'Rust pool handles_reused counts pooled-handle re-acquisitions, not path-matched reuses; '
            'the Go pool counts a reuse only when the idle descriptor is for the same path',
        ],
        'notes': [
            'one host, warm page cache; no fleet, cloud or media claim',
            'the fixture is synthetic BF16 with a 4 KiB chunk size so each object has many chunks',
            'this is a read-path court, not a restore of a real checkpoint',
        ],
    },
    'summary': {
        'checks': len(checks),
        'passed': len(checks) - len(failed),
        'failed': len(failed),
    },
    'result': 'pass' if not failed else 'fail',
}
json.dump(receipt, open(receipt_path, 'w'), indent=2, sort_keys=True)
print(f'receipt: {receipt_path} (result={receipt["result"]}, {len(checks) - len(failed)}/{len(checks)} checks)')
for entry in failed:
    print(f'FAILED {entry["label"]}: {entry["detail"]}')
print('go/rust wall ratio:', summary)
sys.exit(0 if not failed else 1)
PY

exit "$status"
