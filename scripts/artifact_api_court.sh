#!/usr/bin/env bash
# T072 court: one explicit artifact API used by DCP, Orbax and an agent consumer.
#
#   scripts/artifact_api_court.sh
#
# Builds the release CLI in the rust court, runs the Rust API tests there, then
# runs the three-consumer proof in the python court (which has torch, jax and
# orbax). Every container keeps the default hard limits.
#
# Exit 78 with exactly one `artifact_api: skip <reason>` line means the
# environment cannot run the court; anything after the preflight is a failure.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [[ "$(uname -m)" != "x86_64" ]]; then
  echo "artifact_api: skip this court is pinned to the x86_64 images"
  exit 78
fi
if ! command -v docker >/dev/null 2>&1; then
  echo "artifact_api: skip no docker CLI on PATH"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "artifact_api: skip no reachable docker daemon"
  exit 78
fi

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
log="bench/receipts/artifact-api-${stamp}.log"
mkdir -p bench/receipts
: > "$log"

echo "== artifact_api rust API tests ==" | tee -a "$log"
scripts/docker-court.sh rust \
  "cargo test --test artifact_api -- --test-threads=1" >>"$log" 2>&1

echo "== artifact_api release CLI ==" | tee -a "$log"
scripts/docker-court.sh rust \
  "cargo build --release --bin weftfs" >>"$log" 2>&1

# Optional jax/orbax trees installed by earlier adapter courts. Without them the
# Orbax half skips inside pytest, so the proof script requires them explicitly.
py_path="/work/python"
if [[ -d "$root/.docker-cache/py-baselines" ]]; then
  py_path="$py_path:/work/.docker-cache/py-baselines"
elif [[ -d "$root/.docker-cache/py-adapters" ]]; then
  py_path="$py_path:/work/.docker-cache/py-adapters"
else
  echo "artifact_api: skip no jax/orbax tree (.docker-cache/py-baselines or py-adapters)" \
    | tee -a "$log"
  exit 78
fi

echo "== artifact_api three-consumer proof ==" | tee -a "$log"
WEFT_STAMP="$stamp" scripts/docker-court.sh python \
  "cd /work && PYTHONPATH=${py_path} WEFTFS_CLI=/work/.docker-cache/target-rust/release/weftfs WEFT_STAMP=${stamp} python3 bench/scripts/artifact_api_proof.py" \
  >>"$log" 2>&1

receipt="bench/receipts/artifact-api-${stamp}.json"
if [[ ! -f "$receipt" ]]; then
  echo "artifact_api: missing receipt $receipt" >&2
  exit 1
fi
python3 - "$receipt" <<'PY' >>"$log" 2>&1
import json, sys
data = json.load(open(sys.argv[1]))
print(json.dumps({"result": data.get("result"), "passed": data.get("passed"),
                  "failed": data.get("failed")}))
raise SystemExit(0 if data.get("result") == "pass" else 1)
PY

echo "artifact_api: pass ($receipt)"
