#!/usr/bin/env bash
# T049 training-state court (orchestrated on the host).
#
#   bash bench/scripts/training_state_court.sh
#
# Three courts, one receipt:
#   1. rust   builds the ingest/measure harness;
#   2. go     builds the independent WeftFS-Go binary (CGO disabled);
#   3. python runs a real forward/backward/AdamW-step, dumps the state as
#             safetensors, invokes the Rust harness, compares every restored
#             payload with the torch tensors, and runs Go `verify --full` over
#             the same store.
#
# Reduced knobs: none (the fixture model is small by construction). If a court
# image or the corpus is unavailable the script prints one `skip` line and
# exits 78.
set -Eeuo pipefail

cd "$(dirname "$0")/../.."
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work=".docker-cache/training-state-$stamp"
receipt="bench/receipts/training-state-$stamp.json"
log="bench/receipts/training-state-$stamp.log"
mkdir -p "$work" bench/receipts
exec > >(tee "$log") 2>&1

echo "training_state_court: stamp=$stamp"
echo "training_state_court: host=$(uname -srm)"

skip() {
  echo "training_state_court: skip $1"
  exit 78
}

command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
[[ -f python/courts/training_goodput.py ]] || skip "training_goodput court is missing"

echo "training_state_court: building the Rust harness (rust court)"
scripts/docker-court.sh rust "cd /work && cargo build --release --example training_state_court" \
  || skip "cannot build the Rust harness in the rust court"

echo "training_state_court: building WeftFS-Go (go court)"
scripts/docker-court.sh go "cd /work/WeftFS-Go && CGO_ENABLED=0 go build -o /work/.docker-cache/weftfs-go-$stamp ./cmd/weftfs" \
  || skip "cannot build WeftFS-Go in the go court"

echo "training_state_court: running the Python court"
set +e
scripts/docker-court.sh python "cd /work && python3 bench/scripts/training_state_court.py \
  --stamp $stamp \
  --binary .docker-cache/target-rust/release/examples/training_state_court \
  --go-binary .docker-cache/weftfs-go-$stamp \
  --work $work \
  --receipt $receipt"
rc=$?
set -e

if [[ ! -f "$receipt" ]]; then
  echo "training_state_court: no receipt was written (exit $rc)" >&2
  exit 1
fi
# The court owns the receipt (it runs as root with cap-drop=ALL), so this
# host-side step is read-only: it reports the verdict the court wrote.
python3 - "$receipt" <<'PY'
import json
import sys
doc = json.load(open(sys.argv[1]))
print("training_state_court: result", doc["result"], doc.get("checks"))
sys.exit(0 if doc["result"] == "pass" else 1)
PY
if [[ "$rc" -ne 0 ]]; then
  echo "training_state_court: the Python court exited $rc" >&2
  exit 1
fi
echo "training_state_court: wrote $receipt"
