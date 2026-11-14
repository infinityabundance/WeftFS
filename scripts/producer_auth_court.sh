#!/usr/bin/env bash
# T074 court: producer-attested provenance, key rotation/revocation, adversarial
# refusals, and cross-language verification (Rust <-> Go <-> clean-room Python).
#
#   scripts/producer_auth_court.sh
#
# Every container keeps the default hard limits. Exit 78 with exactly one
# `producer_auth: skip <reason>` line means the environment cannot run it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v docker >/dev/null 2>&1; then
  echo "producer_auth: skip no docker CLI on PATH"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "producer_auth: skip no reachable docker daemon"
  exit 78
fi

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work=".docker-cache/producer-auth-${stamp}"
rm -rf "$work"
mkdir -p "$work/bin"
results="$work/results.txt"
log="bench/receipts/producer-auth-${stamp}.log"
mkdir -p bench/receipts "$work"
: > "$log"
: > "$results"

rust_example="/work/.docker-cache/target-rust/release/examples/producer_attest"
rust_cli="/work/.docker-cache/target-rust/release/weftfs"
go_sign="/work/${work}/bin/producer-attest-sign"
go_verify="/work/${work}/bin/producer-attest-verify"
seed_a="1111111111111111111111111111111111111111111111111111111111111111"
seed_c="3333333333333333333333333333333333333333333333333333333333333333"

record() {
  local name="$1" rc="$2"
  if [[ "$rc" -eq 0 ]]; then
    echo "$name=pass" >> "$results"
    echo "PASS $name"
  else
    echo "$name=fail" >> "$results"
    echo "FAIL $name"
  fi
}

step() {
  local name="$1"
  shift
  local rc=0
  "$@" >>"$log" 2>&1 || rc=$?
  record "$name" "$rc"
}

echo "== producer_auth rust tests ==" | tee -a "$log"
step rust_unit scripts/docker-court.sh rust \
  "cargo test --test producer_auth -- --test-threads=1"

echo "== producer_auth rust binaries ==" | tee -a "$log"
step rust_build scripts/docker-court.sh rust \
  "cargo build --release --example producer_attest --bin weftfs"

echo "== producer_auth go binaries ==" | tee -a "$log"
step go_build scripts/docker-court.sh go \
  "cd /work/WeftFS-Go && go build -o /work/${work}/bin/producer-attest-sign ./cmd/producer-attest-sign && go build -o /work/${work}/bin/producer-attest-verify ./cmd/producer-attest-verify && go vet ./cmd/producer-attest-sign ./cmd/producer-attest-verify"

# ---- fixture: a store, two seeds, three key sets --------------------------
cat > "$work/keyset-current.json" <<EOF
{"epoch": 2, "keys": [{"label": "run-a", "public_key": "PUB_A", "valid_from_epoch": 0}]}
EOF
cat > "$work/keyset-rotated.json" <<EOF
{"epoch": 5, "keys": [
  {"label": "run-a", "public_key": "PUB_A", "valid_from_epoch": 0, "rotated_at_epoch": 3},
  {"label": "run-c", "public_key": "PUB_C", "valid_from_epoch": 0, "revoked_at_epoch": 4}
]}
EOF

step keygen_a scripts/docker-court.sh rust \
  "$rust_example keygen --key ${work}/seed-a --seed-hex ${seed_a} --json"
step keygen_c scripts/docker-court.sh rust \
  "$rust_example keygen --key ${work}/seed-c --seed-hex ${seed_c} --json"

# The example derives the public key from a seed file, so ask it again and
# capture only its JSON on stdout (the court's trace goes to stderr).
scripts/docker-court.sh rust \
  "$rust_example keygen --key ${work}/seed-a --seed-hex ${seed_a} --json" \
  >"$work/pub-a.json" 2>>"$log"
scripts/docker-court.sh rust \
  "$rust_example keygen --key ${work}/seed-c --seed-hex ${seed_c} --json" \
  >"$work/pub-c.json" 2>>"$log"

python3 - "$work" <<'PY'
import json, sys
from pathlib import Path
work = Path(sys.argv[1])
pub_a = json.loads((work / "pub-a.json").read_text())["public_key"]
pub_c = json.loads((work / "pub-c.json").read_text())["public_key"]
current = json.loads((work / "keyset-current.json").read_text())
current["keys"][0]["public_key"] = pub_a
(work / "keyset-current.json").write_text(json.dumps(current))
rotated = json.loads((work / "keyset-rotated.json").read_text())
rotated["keys"][0]["public_key"] = pub_a
rotated["keys"][1]["public_key"] = pub_c
(work / "keyset-rotated.json").write_text(json.dumps(rotated))
PY

step keyset_current scripts/docker-court.sh rust \
  "$rust_example keyset --spec ${work}/keyset-current.json --out ${work}/keyset-current.wcb --json"
step keyset_rotated scripts/docker-court.sh rust \
  "$rust_example keyset --spec ${work}/keyset-rotated.json --out ${work}/keyset-rotated.wcb --json"

# ---- a store with real content roots -------------------------------------
step store_init scripts/docker-court.sh rust \
  "$rust_cli --store ${work}/store init"
step store_dataset scripts/docker-court.sh rust \
  "$rust_cli --store ${work}/store create /dataset.bin --text 'dataset bytes'"
step store_output scripts/docker-court.sh rust \
  "$rust_cli --store ${work}/store create /output.bin --text 'output bytes'"
dataset_id="$(scripts/docker-court.sh rust "$rust_cli --store ${work}/store --json stat /dataset.bin" 2>>"$log" | python3 -c 'import json,sys;print(json.load(sys.stdin)["object_id"])')"
output_id="$(scripts/docker-court.sh rust "$rust_cli --store ${work}/store --json stat /output.bin" 2>>"$log" | python3 -c 'import json,sys;print(json.load(sys.stdin)["object_id"])')"
dataset_root="$(scripts/docker-court.sh rust "$rust_cli --store ${work}/store --json artifact resolve --object-id ${dataset_id}" 2>>"$log" | python3 -c 'import json,sys;print(json.load(sys.stdin)["entries"][0]["content_root"])')"
output_root="$(scripts/docker-court.sh rust "$rust_cli --store ${work}/store --json artifact resolve --object-id ${output_id}" 2>>"$log" | python3 -c 'import json,sys;print(json.load(sys.stdin)["entries"][0]["content_root"])')"
echo "dataset_root=${dataset_root} output_root=${output_root}" >>"$log"

# ---- Rust signs; Go and Python verify ------------------------------------
step sign_current scripts/docker-court.sh rust \
  "$rust_example sign --key ${work}/seed-a --keyset ${work}/keyset-current.wcb --producer trainer-1 --run-id run-001 --tool weftfs --tool-version 1.4.2 --determinism exact --signing-epoch 1 --input dataset=${dataset_root} --output ${output_root} --out ${work}/attest-current.wcb --json"
step rust_verify_current scripts/docker-court.sh rust \
  "$rust_example verify --attestation ${work}/attest-current.wcb --keyset ${work}/keyset-current.wcb --mode current --json"
step go_verify_current scripts/docker-court.sh go \
  "$go_verify /work/${work}/attest-current.wcb --keyset /work/${work}/keyset-current.wcb --mode current --json"
step python_verify_current scripts/docker-court.sh python \
  "cd /work && python3 verifier/producer_attest.py /work/${work}/attest-current.wcb --keyset /work/${work}/keyset-current.wcb --mode current --json"

# ---- rotated and revoked keys -------------------------------------------
step sign_old scripts/docker-court.sh rust \
  "$rust_example sign --key ${work}/seed-a --keyset ${work}/keyset-rotated.wcb --producer trainer-1 --run-id run-002 --tool weftfs --tool-version 1.4.2 --determinism exact --signing-epoch 2 --input dataset=${dataset_root} --output ${output_root} --out ${work}/attest-old.wcb --json"
step sign_revoked scripts/docker-court.sh rust \
  "$rust_example sign --key ${work}/seed-c --keyset ${work}/keyset-rotated.wcb --producer trainer-1 --run-id run-003 --tool weftfs --tool-version 1.4.2 --determinism exact --signing-epoch 3 --input dataset=${dataset_root} --output ${output_root} --out ${work}/attest-revoked.wcb --json"

refuse() {
  local name="$1"
  shift
  local rc=0
  "$@" >>"$log" 2>&1 || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "$name=pass" >> "$results"
    echo "PASS $name (refused)"
  else
    echo "$name=fail" >> "$results"
    echo "FAIL $name (accepted)"
  fi
}

refuse rust_rejects_rotated_current scripts/docker-court.sh rust \
  "$rust_example verify --attestation ${work}/attest-old.wcb --keyset ${work}/keyset-rotated.wcb --mode current --json"
step rust_accepts_rotated_historical scripts/docker-court.sh rust \
  "$rust_example verify --attestation ${work}/attest-old.wcb --keyset ${work}/keyset-rotated.wcb --mode historical --json"
refuse go_rejects_rotated_current scripts/docker-court.sh go \
  "$go_verify /work/${work}/attest-old.wcb --keyset /work/${work}/keyset-rotated.wcb --mode current --json"
refuse python_rejects_rotated_current scripts/docker-court.sh python \
  "cd /work && python3 verifier/producer_attest.py /work/${work}/attest-old.wcb --keyset /work/${work}/keyset-rotated.wcb --mode current --json"

refuse rust_rejects_revoked scripts/docker-court.sh rust \
  "$rust_example verify --attestation ${work}/attest-revoked.wcb --keyset ${work}/keyset-rotated.wcb --mode historical --json"
refuse go_rejects_revoked scripts/docker-court.sh go \
  "$go_verify /work/${work}/attest-revoked.wcb --keyset /work/${work}/keyset-rotated.wcb --mode historical --json"
refuse python_rejects_revoked scripts/docker-court.sh python \
  "cd /work && python3 verifier/producer_attest.py /work/${work}/attest-revoked.wcb --keyset /work/${work}/keyset-rotated.wcb --mode historical --json"

# ---- tampered signature --------------------------------------------------
python3 - "$work" <<'PY'
import sys
from pathlib import Path
work = Path(sys.argv[1])
raw = bytearray((work / "attest-current.wcb").read_bytes())
marker = bytes([0x06, 0x40])
position = raw.find(marker)
assert position >= 0, "signature marker"
raw[position + 2] ^= 1
(work / "attest-tampered.wcb").write_bytes(raw)
PY
refuse rust_rejects_tampered scripts/docker-court.sh rust \
  "$rust_example verify --attestation ${work}/attest-tampered.wcb --keyset ${work}/keyset-current.wcb --json"
refuse go_rejects_tampered scripts/docker-court.sh go \
  "$go_verify /work/${work}/attest-tampered.wcb --keyset /work/${work}/keyset-current.wcb --json"
refuse python_rejects_tampered scripts/docker-court.sh python \
  "cd /work && python3 verifier/producer_attest.py /work/${work}/attest-tampered.wcb --keyset /work/${work}/keyset-current.wcb --json"

# ---- Go signs; Rust and Python verify -----------------------------------
step go_sign scripts/docker-court.sh go \
  "$go_sign --key ${seed_a} --keyset /work/${work}/keyset-current.wcb --producer trainer-1 --run-id run-go --tool weftfs-go --tool-version 1.4.2 --determinism exact --signing-epoch 1 --input dataset=${dataset_root} --output ${output_root} --out /work/${work}/attest-go.wcb --json"
step rust_verifies_go scripts/docker-court.sh rust \
  "$rust_example verify --attestation ${work}/attest-go.wcb --keyset ${work}/keyset-current.wcb --json"
step python_verifies_go scripts/docker-court.sh python \
  "cd /work && python3 verifier/producer_attest.py /work/${work}/attest-go.wcb --keyset /work/${work}/keyset-current.wcb --json"

# ---- the store cross-check ----------------------------------------------
step store_check_ok scripts/docker-court.sh rust \
  "$rust_example verify --attestation ${work}/attest-current.wcb --keyset ${work}/keyset-current.wcb --store ${work}/store --json"
refuse store_check_wrong_input scripts/docker-court.sh rust \
  "$rust_example sign --key ${work}/seed-a --keyset ${work}/keyset-current.wcb --producer trainer-1 --run-id run-004 --tool weftfs --tool-version 1.4.2 --determinism exact --signing-epoch 1 --input dataset=$(printf '99%.0s' {1..32}) --output ${output_root} --out ${work}/attest-bad-input.wcb --json && ${rust_example} verify --attestation ${work}/attest-bad-input.wcb --keyset ${work}/keyset-current.wcb --store ${work}/store --json"

# ---- receipt -------------------------------------------------------------
total="$(grep -c . "$results" || true)"
passed="$(grep -c '=pass$' "$results" || true)"
failed="$(grep -c '=fail$' "$results" || true)"
stamp_value="$stamp" \
total="$total" passed="$passed" failed="$failed" work="$work" results="$results" \
python3 - <<'PY'
import json, os, subprocess
from pathlib import Path
root = Path(".")
data = {
    "kind": "producer-auth",
    "schema": "weft.producer-attestation.receipt.v1",
    "generated_utc": os.environ["stamp_value"],
    "environment": {
        "revisions": {"WeftFS": subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()},
        "courts": "rust (tests + example), go (signer + verifier), python (clean-room verifier)",
        "limits": "3 GiB / 3 GiB swap / 1024 pids / 4 cpus / bounded tmpfs",
        "label": "single host, synthetic fixture; the signature authenticates a declaration, not execution",
    },
    "checks": [],
    "passed": int(os.environ["passed"]),
    "failed": int(os.environ["failed"]),
    "workdir": os.environ["work"],
}
for line in Path(os.environ["results"]).read_text().splitlines():
    name, _, value = line.partition("=")
    data["checks"].append({"check": name, "result": value})
data["result"] = "pass" if data["failed"] == 0 else "fail"
path = Path("bench/receipts") / ("producer-auth-%s.json" % os.environ["stamp_value"])
path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
print(json.dumps({"receipt": str(path), "passed": data["passed"],
                  "failed": data["failed"], "result": data["result"]}))
PY

echo "producer_auth: $([ "$failed" -eq 0 ] && echo pass || echo fail) ($results)"
[[ "$failed" -eq 0 ]]
