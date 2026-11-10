#!/usr/bin/env bash
# R12 increment 3 — attested-load court.
#
# Host orchestrator. One real checkpoint of SmolLM-135M-Instruct (272 BF16
# tensors, 269 MB) is written into a local store; then:
#
#   load_all      signed attestation of every tensor (verifies group roots)
#   verify_all    an independent process re-reads every recorded range
#   load_partial  20 tensors loaded; the other 252 are reported `unverified`
#   verify_partial the same independent verification, which lists the 252
#   load_region   the first 50 rows of one tensor (whole-tensor root NOT claimed)
#   verify_region the same independent verification
#   tests         tests/attested_load.rs (tamper cases, trust set, totals)
#
# Usage: bench/scripts/attested_load_court.sh
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
work="$root/.docker-cache/attested-load-$stamp"
work_c="/work/.docker-cache/attested-load-$stamp"
receipt="$root/bench/receipts/attested-loads-$stamp.json"
log="$root/bench/receipts/attested-loads-$stamp.log"
corpus="$root/.docker-cache/real-corpus/SmolLM-135M-Instruct.safetensors"
corpus_c="/work/.docker-cache/real-corpus/$(basename "$corpus")"
corpus_sha="c1464c9ebccc6fb497aa4ccc8d74d29b33f9a9eddc4638a4575719357f532f2d"
memory="${WEFT_ATTEST_MEMORY:-3g}"
mkdir -p "$work" "$root/bench/receipts"
chmod a+rwX "$work" "$root/bench/receipts" 2>/dev/null || true
: >"$log"

echo "attested_load_court: stamp=$stamp memory=$memory" | tee -a "$log"
if [[ ! -f "$corpus" ]]; then
  echo "attested_load_court: missing corpus $corpus" >&2
  exit 1
fi
actual="$(sha256sum "$corpus" | awk '{print $1}')"
if [[ "$actual" != "$corpus_sha" ]]; then
  echo "attested_load_court: corpus sha256 mismatch" >&2
  exit 1
fi

# A fixed 32-byte Ed25519 seed (test-only; never a production key).
python3 - "$work/seed.bin" <<'PY'
import sys
open(sys.argv[1], "wb").write(bytes(range(32)))
PY

run_phase() {
  local name="$1" cmd="$2"
  local phase_log="$work/$name.log"
  local status=0
  echo "=== phase $name ===" | tee -a "$log"
  if scripts/docker-court.sh rust "cd /work && set +e; $cmd; s=\$?; chmod -R a+rwX '$work_c' 2>/dev/null || true; exit \$s" \
    >"$phase_log" 2>&1; then
    status=0
  else
    status=$?
  fi
  printf '%s\t%s\n' "$name" "$status" >>"$work/status.tsv"
  echo "attested_load_court: phase $name exit=$status" | tee -a "$log"
  tail -3 "$phase_log" | tee -a "$log"
}

build_status=0
if ! scripts/docker-court.sh rust \
  "cd /work && cargo build --release --example attested_load --example scale_7b" \
  >>"$log" 2>&1; then
  build_status=$?
fi
echo "attested_load_court: build exit=$build_status" | tee -a "$log"
if [[ "$build_status" -ne 0 ]]; then
  exit 1
fi

scale=/work/.docker-cache/target-rust/release/examples/scale_7b
attest=/work/.docker-cache/target-rust/release/examples/attested_load
store="$work_c/store"

# Build the checkpoint store once: a tensor-file ingest (the scale court's
# ingest path) is not a checkpoint, so the attestation needs a real
# `weft.checkpoint.v1` written from the same source.
run_phase ingest "$scale ingest --source $corpus_c --source-sha256 $corpus_sha --store $store --fresh --out $work_c/ingest.json"
run_phase checkpoint "$scale checkpoint --source $corpus_c --source-sha256 $corpus_sha --store $store --name model --shards 8 --axis 0 --write-policy fast --fast-codec raw --max-ingest-bytes 1610612736 --out $work_c/checkpoint.json"

# The 20 smallest tensors, by name, for the partial load.
python3 - "$work/ingest.json" "$work/partial.txt" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
rows = sorted(data["rows"], key=lambda row: (row["logical_bytes"], row["name"]))[:20]
open(sys.argv[2], "w").write(",".join(row["name"] for row in rows))
PY
partial="$(cat "$work/partial.txt")"
region_tensor="$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print([r["name"] for r in d["rows"] if r["name"].endswith("mlp.up_proj.weight")][0])' "$work/ingest.json")"

run_phase load_all "$attest load --store $store --checkpoint model --seed $work_c/seed.bin --replica court --principal court --out $work_c/all.wcb"
run_phase verify_all "$attest verify --store $store --receipt $work_c/all.wcb --require-signature"
run_phase load_partial "$attest load --store $store --checkpoint model --tensors $partial --seed $work_c/seed.bin --replica court-partial --principal court --out $work_c/partial.wcb"
run_phase verify_partial "$attest verify --store $store --receipt $work_c/partial.wcb --require-signature"
run_phase load_region "$attest load --store $store --checkpoint model --tensors $region_tensor --region-rows 50 --seed $work_c/seed.bin --replica court-region --principal court --out $work_c/region.wcb"
run_phase verify_region "$attest verify --store $store --receipt $work_c/region.wcb --require-signature"

run_phase tests "cargo test --release --test attested_load -- --test-threads=1"

# Tamper the signed record itself and require both verifiers to reject it. The
# specific "one loaded range" case is covered by the Rust test above; here the
# last byte of the record is flipped, which changes a signature-covered field.
python3 - "$work/all.wcb" "$work/tampered.wcb" <<'PY'
import sys
raw = bytearray(open(sys.argv[1], "rb").read())
raw[-1] ^= 0xFF
open(sys.argv[2], "wb").write(raw)
PY
run_phase verify_tampered "$attest verify --store $store --receipt $work_c/tampered.wcb --require-signature"

# Independent Go verification of the same signed record (signature + record
# shape; this repository has no weft.checkpoint.v1 reader for byte replay).
go_status=0
if scripts/docker-court.sh go \
  "set +e; cd /work/WeftFS-Go; go build -o /work/.docker-cache/attest-verify ./cmd/attest-verify || exit 2; /work/.docker-cache/attest-verify '$work_c/all.wcb' --require-signature" \
  >"$work/go_verify.log" 2>&1; then
  go_status=0
else
  go_status=$?
fi
printf 'go_verify\t%s\n' "$go_status" >>"$work/status.tsv"
echo "attested_load_court: phase go_verify exit=$go_status" | tee -a "$log"
tail -12 "$work/go_verify.log" | tee -a "$log"

go_tamper=0
if scripts/docker-court.sh go \
  "set +e; cd /work/WeftFS-Go; go build -o /work/.docker-cache/attest-verify ./cmd/attest-verify || exit 2; /work/.docker-cache/attest-verify '$work_c/tampered.wcb' --require-signature; s=\$?; [ \$s -eq 1 ]" \
  >"$work/go_tamper.log" 2>&1; then
  go_tamper=0
else
  go_tamper=$?
fi
printf 'go_tamper\t%s\n' "$go_tamper" >>"$work/status.tsv"
echo "attested_load_court: phase go_tamper exit=$go_tamper" | tee -a "$log"
tail -4 "$work/go_tamper.log" | tee -a "$log"

python3 - "$work" "$receipt" "$stamp" "$memory" "$corpus" "$corpus_sha" "$log" "$root" <<'PY'
import json, os, sys

work, receipt, stamp, memory, corpus, corpus_sha, log, root = sys.argv[1:9]
status = {}
for line in open(os.path.join(work, "status.tsv")):
    name, code = line.strip().split("\t")
    status[name] = int(code)

def read(name):
    # The example prints one JSON object on its last output line.
    path = os.path.join(work, name + ".log")
    try:
        found = None
        for line in open(path):
            line = line.strip()
            if line.startswith("{") and line.endswith("}"):
                found = line
        return json.loads(found) if found else None
    except Exception:
        return None

phases = {name: {"exit": status.get(name), "report": read(name)}
          for name in ("load_all", "verify_all", "load_partial", "verify_partial",
                       "load_region", "verify_region", "verify_tampered")}
phases["tests"] = {"exit": status.get("tests")}
phases["go_verify"] = {"exit": status.get("go_verify")}
phases["go_tamper"] = {"exit": status.get("go_tamper")}

def log_line(phase, needle):
    try:
        for line in open(os.path.join(work, phase + ".log")):
            if needle in line:
                return line.strip()
    except Exception:
        pass
    return None

tests_line = log_line("tests", "test result:")

data = {
    "schema": "weft-attested-loads-court-v1",
    "generated_utc": stamp,
    "label": "one host, real checkpoint, signed local loads; not a fleet or serving claim",
    "corpus": {"repo": "HuggingFaceTB/SmolLM-135M-Instruct", "dtype": "BF16",
               "size_bytes": os.path.getsize(corpus), "sha256": corpus_sha},
    "court": {"kind": "rust", "memory": memory, "cpus": 4, "pids": 1024,
              "cap_drop": "ALL", "no_new_privileges": True},
    "phases": phases,
    "tamper_tests": tests_line,
    "key_note": "the seed is a fixed test key (bytes 0..31); never a production key",
    "log": os.path.relpath(log, root),
}
with open(receipt, "w") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")

print("phase|exit|load_ms|verify_wall_ms|receipt_bytes|loaded_bytes|us_per_mib|unverified")
for name in ("load_all", "verify_all", "load_partial", "verify_partial", "load_region", "verify_region"):
    report = phases[name]["report"] or {}
    print("{}|{}|{}|{}|{}|{}|{}|{}".format(
        name, phases[name]["exit"], report.get("load_ms"), report.get("wall_ms"),
        report.get("receipt_bytes"), report.get("replayed_bytes"),
        report.get("us_per_mib"), report.get("unverified_tensors")))
print("tests:", tests_line)
print("receipt:", receipt)
PY

echo "attested_load_court: done -> $receipt" | tee -a "$log"
