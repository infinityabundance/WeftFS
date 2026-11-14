#!/usr/bin/env bash
# Inference-engine court (subphase X17/T064/T069).
#
#   bash bench/scripts/kv_engine_court.sh
#
# Two phases, two courts, one receipt:
#
#   1. python court: try to install and import vLLM and SGLang from PyPI under
#      the unchanged limits (3 GiB memory / 1 GiB /tmp / 1024 pids / 4 cpus);
#   2. bench court:  run the engine-shaped trace replay over real KV prefix
#      records, measuring reuse locally.
#
# Then the host assembles both into `kv-engine-<stamp>.json`, which states
# exactly which numbers are measured, which are derived, and which are
# hypothetical (the prefill grid). This court is NOT registered in
# `scripts/verify-all.sh`: it needs network access and performs downloads that
# are expected to fail, so it is run on demand. Its skip path exists anyway.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$root/.docker-cache/kv-engine-$stamp"
receipt="bench/receipts/kv-engine-$stamp.json"
log="bench/receipts/kv-engine-$stamp.log"

skip() {
  echo "kv_engine_court: skip $1"
  exit 78
}

echo "kv_engine_court: stamp=$stamp"
echo "kv_engine_court: host=$(uname -srm)"

command -v docker >/dev/null 2>&1 || skip "docker CLI is not installed"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
for image in "${WEFT_PYTHON_COURT_IMAGE:-weft/python-court:1}" "${WEFT_BENCH_COURT_IMAGE:-weft/bench-court:1}"; do
  docker image inspect "$image" >/dev/null 2>&1 || skip "court image $image is not present"
done

mkdir -p bench/receipts "$work"
exec > >(tee "$log") 2>&1

echo "== network preflight (python court) =="
if ! bash scripts/docker-court.sh python \
  "python3 -c \"import urllib.request; urllib.request.urlopen('https://pypi.org/simple/', timeout=20)\""; then
  skip "the python court cannot reach https://pypi.org (no network route)"
fi

echo "== engine install attempts (python court) =="
WEFT_STAMP="$stamp" bash scripts/docker-court.sh python \
  "cd /work && WEFT_STAMP=$stamp timeout 2400 python3 bench/scripts/kv_engine_attempt.py --out /work/.docker-cache/kv-engine-$stamp/attempts.json"

[ -s "$work/attempts.json" ] || {
  echo "kv_engine_court: attempts document was not written" >&2
  exit 1
}

echo "== trace replay (bench court) =="
bash scripts/docker-court.sh bench \
  "cd /work && WEFT_STAMP=$stamp cargo run --release --quiet --example kv_trace_replay -- --out bench/receipts/kv-engine-$stamp.replay.json"

[ -s "bench/receipts/kv-engine-$stamp.replay.json" ] || {
  echo "kv_engine_court: trace replay receipt was not written" >&2
  exit 1
}

echo "== assemble =="
kernel="$(uname -r)"
python3 - "$work/court.json" "$kernel" <<'PY'
import json
import sys

path, kernel = sys.argv[1], sys.argv[2]
court = {
    "kernel": kernel,
    "memory": "3g",
    "swap": "3g",
    "pids": 1024,
    "cpus": "4",
    "gpu_devices": "none: ls /dev/nvidia* -> No such file or directory",
}
json.dump(court, open(path, "w"), indent=2, sort_keys=True)
PY

python3 bench/scripts/assemble_kv_engine.py \
  --stamp "$stamp" \
  --attempts "$work/attempts.json" \
  --replay "bench/receipts/kv-engine-$stamp.replay.json" \
  --court-json "$work/court.json" \
  --receipt "$receipt" \
  --rust-rev "$(git rev-parse HEAD)" \
  --go-rev "$(git -C WeftFS-Go rev-parse HEAD)" \
  --codec-rev "$(git -C WeftCodec rev-parse HEAD)" \
  --codec-go-rev "$(git -C WeftCodec-Go rev-parse HEAD)"

# The replay receipt is embedded in the assembled one; keep the tree to a
# single evidence file plus its log.
rm -f "bench/receipts/kv-engine-$stamp.replay.json"

echo "kv_engine_court: receipt $receipt"
echo "kv_engine_court: log $log"
