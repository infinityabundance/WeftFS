#!/usr/bin/env bash
# T075 court: taint tracing, quarantine, and the cost of the enforcement gate.
#
# Runs the suite and the harness in the Rust court, then assembles a receipt on
# the host. Exit 0 on a complete run, 78 with one `taint_court: skip <reason>`
# line when the environment cannot run it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "taint_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/taint-${stamp}.json"
log="bench/receipts/taint-${stamp}.log"

if ! scripts/docker-court.sh rust "
  set -Eeuo pipefail
  cd /work
  cargo test --test taint --quiet 2>&1 | tail -3
  cargo run --release --quiet --example taint_court -- \
    --out .docker-cache/taint-court --json .docker-cache/taint-court/result.json
  cat .docker-cache/taint-court/result.json
" >"$log" 2>&1; then
  echo "$(basename "$0"): FAILED (see $log)" >&2
  exit 1
fi

python3 - "$stamp" "$receipt" "$log" <<'PY'
import hashlib, json, os, re, subprocess, sys

stamp, receipt_path, log_path = sys.argv[1:4]
log = open(log_path, encoding="utf-8", errors="replace").read()

match = re.search(r"test result: ok\. (\d+) passed; (\d+) failed", log)
if not match:
    raise SystemExit("taint court: could not read the suite result from the log")
unit_passed, unit_failed = int(match.group(1)), int(match.group(2))

# The harness prints one JSON object; find the last balanced object carrying
# its schema tag (key order is not guaranteed).
decoder = json.JSONDecoder()
harness = None
for start in [index for index, char in enumerate(log) if char == "{"]:
    try:
        candidate, _ = decoder.raw_decode(log[start:])
    except ValueError:
        continue
    if isinstance(candidate, dict) and candidate.get("schema") == "weft.taint-court/v1":
        harness = candidate
if harness is None:
    raise SystemExit("taint court: harness JSON missing from the log")

def rev(path):
    try:
        return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=path).decode().strip()
    except Exception:
        return None

env = {
    "image": os.environ.get("WEFT_COURT_IMAGE", "weft/rust-court:1"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID"),
    "kernel": os.environ.get("WEFT_COURT_KERNEL"),
    "memory": os.environ.get("WEFT_COURT_MEMORY"),
    "cpus": os.environ.get("WEFT_COURT_CPUS"),
    "pids": os.environ.get("WEFT_COURT_PIDS"),
    "revisions": {
        "WeftFS": rev("."),
        "WeftCodec": rev("WeftCodec"),
        "WeftCodec-Go": rev("WeftCodec-Go"),
        "WeftFS-Go": rev("WeftFS-Go"),
    },
}

checks = list(harness["checks"])
checks.insert(0, {
    "name": "tests/taint.rs",
    "pass": unit_failed == 0 and unit_passed >= 12,
    "detail": "%d passed, %d failed" % (unit_passed, unit_failed),
})
passed = sum(1 for check in checks if check["pass"])
failed = len(checks) - passed

document = {
    "schema": "weft.taint-court/v1",
    "stamp": stamp,
    "generated_utc": stamp,
    "environment": env,
    "checks": checks,
    "passed": passed,
    "failed": failed,
    "result": "pass" if failed == 0 else "fail",
    "trace_us": harness["trace_us"],
    "reads": harness["reads"],
    "read_ns": harness["read_ns"],
    "gate_overhead_ns": harness["gate_overhead_ns"],
    "refusal_overhead_ns": harness["refusal_overhead_ns"],
    "signer_public_key": harness["signer_public_key"],
    "quarantine_record": harness["quarantine_record"],
    "method": (
        "One dataset member, one model declaration, two raw checkpoints, one provenance "
        "receipt per checkpoint. Reads are 2,000 warm whole-object reads of one checkpoint "
        "manifest object in three states: no quarantine state at all, one unrelated "
        "quarantine present (gate lookup miss), and the branch quarantined (gate hit and "
        "refusal). Traces are the forward+backward walk over exactly these records."
    ),
    "log": log_path,
    "log_sha256": hashlib.sha256(log.encode()).hexdigest(),
}
with open(receipt_path, "w") as handle:
    json.dump(document, handle, indent=1)
    handle.write("\n")
print("receipt", receipt_path, "passed", passed, "failed", failed)
PY

echo "taint_court: pass (receipt: $receipt)"
