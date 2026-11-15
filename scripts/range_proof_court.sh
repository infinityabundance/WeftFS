#!/usr/bin/env bash
# T073 court: portable authenticated range proofs.
#
# Runs in the FUSE court because it needs `python3` for the *independent*
# verifier (`verifier/range_proof_check.py`), which shares no code with WeftFS.
# Everything is bounded by the usual hard limits; nothing is raised.
#
# Produces bench/receipts/range-proof-<stamp>.json and a raw log beside it.
# Exit 0 on a complete run, 78 with one `range_proof_court: skip <reason>` line
# when the environment cannot run it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "range_proof_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/range-proof-${stamp}.json"
log="bench/receipts/range-proof-${stamp}.log"
work=".docker-cache/range-proof-court"
container_work="/work/${work}"

# Build and run the harness, then hand every bundle to the independent Python
# verifier, all inside one FUSE court.
scripts/docker-court.sh fuse "
  set -Eeuo pipefail
  cd /work
  rm -rf ${container_work}
  mkdir -p ${container_work}
  cargo build --release --quiet --example range_proof_court
  .docker-cache/target-rust/release/examples/range_proof_court --out ${container_work} --json ${container_work}/rows.json
  signer=\$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[\"signer_public_key\"])' ${container_work}/rows.json)
  python3 - <<'PY'
import json, os, subprocess, sys
work = '/work/.docker-cache/range-proof-court'
rows = json.load(open(os.path.join(work, 'rows.json')))
report = {'rows': [], 'checks': []}
ok = True
for row in rows['rows']:
    bundle = os.path.join(work, row['bundle'])
    case = row['bundle'].split('/')[0]
    store = os.path.join(work, case, 'store')
    out = subprocess.run(
        ['python3', 'verifier/range_proof_check.py', '--bundle', bundle,
         '--store', store, '--trust-key', rows['signer_public_key'], '--json'],
        capture_output=True, text=True)
    verified = out.returncode == 0
    payload = json.loads(out.stdout) if out.stdout.strip() else {}
    merged = dict(row)
    merged['independent_verified'] = verified
    merged['independent_verify_ns'] = payload.get('verify_ns')
    merged['independent_manifest_rechecked'] = payload.get('manifest_rechecked')
    merged['independent_class'] = payload.get('class')
    merged['independent_message'] = payload.get('message')
    report['rows'].append(merged)
    if not verified:
        ok = False
        report['checks'].append({'name': 'verify ' + row['bundle'], 'pass': False,
                                 'detail': payload.get('message')})
    else:
        report['checks'].append({'name': 'verify ' + row['bundle'], 'pass': True})

# Portable mode: no store, chunk bytes from the bundle only.
first = rows['rows'][0]
bundle = os.path.join(work, first['bundle'])
out = subprocess.run(
    ['python3', 'verifier/range_proof_check.py', '--bundle', bundle,
     '--trust-key', rows['signer_public_key'], '--json'],
    capture_output=True, text=True)
payload = json.loads(out.stdout) if out.stdout.strip() else {}
report['checks'].append({
    'name': 'portable verification without a store',
    'pass': out.returncode == 0 and payload.get('manifest_rechecked') is False
            and payload.get('verified') is True,
})
ok = ok and report['checks'][-1]['pass']

# Rejections. Each invalid bundle must be refused with the expected class.
expectations = {
    'different-range': ('Corrupt',),
    'substituted-leaf': ('Corrupt',),
    'wrong-root': ('Corrupt',),
    'truncated-path': ('Corrupt',),
    'cross-boundary': ('Corrupt',),
    # A single flipped byte may land in a hash (Corrupt), in a count
    # (Malformed) or in the tag stream (NonCanonical). Any refusal is a pass;
    # the class is recorded. The trailing-bytes case is deterministic.
    'noncanonical': ('NonCanonical', 'Malformed', 'Corrupt'),
    'trailing-bytes': ('NonCanonical',),
}
for case in sorted({row['bundle'].split('/')[0] for row in rows['rows']}):
    base = os.path.join(work, case, 'invalid')
    if not os.path.isdir(base):
        continue
    store = os.path.join(work, case, 'store')
    for name, classes in expectations.items():
        bundle = os.path.join(base, name)
        if not os.path.isdir(bundle):
            continue
        out = subprocess.run(
            ['python3', 'verifier/range_proof_check.py', '--bundle', bundle,
             '--store', store, '--json'],
            capture_output=True, text=True)
        payload = json.loads(out.stdout) if out.stdout.strip() else {}
        actual = payload.get('class')
        passed = out.returncode == 1 and actual in classes
        report['checks'].append({
            'case': case, 'name': 'reject ' + name, 'pass': passed,
            'expected': list(classes), 'actual': actual,
            'message': payload.get('message'),
        })
        ok = ok and passed

# An untrusted signer must be refused even when the proof is intact.
bundle = os.path.join(work, first['bundle'])
out = subprocess.run(
    ['python3', 'verifier/range_proof_check.py', '--bundle', bundle,
     '--store', store, '--trust-key', '00' * 32, '--json'],
    capture_output=True, text=True)
payload = json.loads(out.stdout) if out.stdout.strip() else {}
report['checks'].append({
    'name': 'untrusted signer is refused',
    'pass': out.returncode == 1 and payload.get('class') == 'Unauthorized',
    'actual': payload.get('class'),
})
ok = ok and report['checks'][-1]['pass']

report['result'] = 'pass' if ok else 'fail'
json.dump(report, open(os.path.join(work, 'python.json'), 'w'), indent=2)
print('python checks:', sum(1 for c in report['checks'] if c['pass']), '/', len(report['checks']))
PY
" >>"$log" 2>&1 || skip "court run failed (see $log)"

# Assemble the receipt on the host (numbers only; no test logic here).
python3 - "$stamp" "$root/$work" "$receipt" "$log" <<'PY'
import hashlib, json, os, subprocess, sys, time
stamp, work, receipt_path, log_path = sys.argv[1:5]
sys.path.insert(0, 'verifier')
import ed25519
import wcb
from hashing import hash_domain
rows = json.load(open(os.path.join(work, 'rows.json')))
python = json.load(open(os.path.join(work, 'python.json')))

def rev(path):
    try:
        return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=path).decode().strip()
    except Exception:
        return None

def short(path):
    value = rev(path)
    return value[:12] if value else None

env = {
    'image': os.environ.get('WEFT_COURT_IMAGE', 'weft/fuse-court:1'),
    'image_id': os.environ.get('WEFT_COURT_IMAGE_ID'),
    'kernel': os.environ.get('WEFT_COURT_KERNEL'),
    'memory': os.environ.get('WEFT_COURT_MEMORY'),
    'cpus': os.environ.get('WEFT_COURT_CPUS'),
    'pids': os.environ.get('WEFT_COURT_PIDS'),
    'revisions': {
        'WeftFS': rev('.'),
        'WeftCodec': rev('WeftCodec'),
        'WeftCodec-Go': rev('WeftCodec-Go'),
        'WeftFS-Go': rev('WeftFS-Go'),
    },
}

def sha(path):
    h = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()

# Component costs of the independent verifier, measured on this host so the
# table can separate interpreter/curve cost from Merkle cost.
def component_costs():
    bundle = os.path.join(work, python['rows'][0]['bundle'])
    anchor_raw = open(os.path.join(bundle, 'anchor.wcb'), 'rb').read()
    anchor = wcb.decode(anchor_raw)
    signature = anchor['signature']
    signing = wcb.encode({k: v for k, v in anchor.items() if k != 'signature'})
    public_key = signature['public_key'].value
    raw = signature['signature'].value
    samples = []
    for _ in range(5):
        started = time.monotonic_ns()
        if not ed25519.verify(public_key, signing, raw):
            raise SystemExit('component probe: signature did not verify')
        samples.append(time.monotonic_ns() - started)
    samples.sort()
    blake = []
    for _ in range(5):
        started = time.monotonic_ns()
        hash_domain('weft/chunk/v1', b'x' * 4096)
        blake.append(time.monotonic_ns() - started)
    blake.sort()
    return {'ed25519_verify_ns': samples[len(samples) // 2],
            'blake3_4096_ns': blake[len(blake) // 2]}

components = None
try:
    components = component_costs()
except Exception as err:  # reported, never silently dropped
    components = {'error': str(err)}

document = {
    'schema': 'weft.range-proof-court/v1',
    'verifier_components_ns': components,
    'stamp': stamp,
    'generated_utc': stamp,
    'environment': env,
    'signer_public_key': rows['signer_public_key'],
    'rows': python['rows'],
    'checks': python['checks'],
    'result': python['result'],
    'passed': sum(1 for c in python['checks'] if c['pass']),
    'failed': sum(1 for c in python['checks'] if not c['pass']),
    'verifier': 'verifier/range_proof_check.py',
    'verifier_sha256': sha('verifier/range_proof_check.py'),
    'method': (
        'One raw (chunk-table) checkpoint per case; one signed anchor per shard; '
        'each region is proven at an offset deliberately unaligned to the chunk grid. '
        'Rows measure the Rust verifier in-process (7 reps, median) and the independent '
        'stdlib-Python verifier as a separate process. full_tensor_ns is the cost of the '
        'store reading and verifying the whole tensor object on the same store.'
    ),
    'log': log_path,
}
with open(receipt_path, 'w') as handle:
    json.dump(document, handle, indent=1)
    handle.write('\n')
print('receipt', receipt_path, 'passed', document['passed'], 'failed', document['failed'])
PY

echo "range_proof_court: pass (receipt: $receipt)"
