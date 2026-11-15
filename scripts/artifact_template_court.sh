#!/usr/bin/env bash
# T077 court: evaluation and synthetic-data artifact templates.
#
# Runs the suite and the harness in the Rust court, then assembles a receipt on
# the host. Exit 0 on a complete run, 78 with one `artifact_template_court: skip <reason>`
# line when the environment cannot run it.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "artifact_template_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/artifact-templates-${stamp}.json"
log="bench/receipts/artifact-templates-${stamp}.log"

if ! scripts/docker-court.sh rust "
  set -Eeuo pipefail
  cd /work
  cargo test --test artifact_template --quiet 2>&1 | tail -3
  cargo run --release --quiet --example artifact_template_court -- \
    --out .docker-cache/artifact-template-court \
    --json .docker-cache/artifact-template-court/result.json
  cat .docker-cache/artifact-template-court/result.json
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
    raise SystemExit("artifact template court: could not read the suite result from the log")
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
    if isinstance(candidate, dict) and candidate.get("schema") == "weft.artifact-template-court/v1":
        harness = candidate
if harness is None:
    raise SystemExit("artifact template court: harness JSON missing from the log")

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
    "name": "tests/artifact_template.rs",
    "pass": unit_failed == 0 and unit_passed >= 11,
    "detail": "%d passed, %d failed" % (unit_passed, unit_failed),
})
passed = sum(1 for check in checks if check["pass"])
failed = len(checks) - passed

document = {
    "schema": "weft.artifact-template-court/v1",
    "stamp": stamp,
    "generated_utc": stamp,
    "environment": env,
    "checks": checks,
    "passed": passed,
    "failed": failed,
    "result": "pass" if failed == 0 else "fail",
    "eval_template_bytes": harness["eval_template_bytes"],
    "synth_template_bytes": harness["synth_template_bytes"],
    "eval_record_ns": harness["eval_record_ns"],
    "synth_record_ns": harness["synth_record_ns"],
    "eval_replay_ns": harness["eval_replay_ns"],
    "synth_replay_ns": harness["synth_replay_ns"],
    "output_bytes": harness["output_bytes"],
    "method": (
        "One evaluation run (dataset, model and config roots plus a 4 MiB metrics output) "
        "and one synthetic-data run (config, generator, 4-byte seed, one output) recorded "
        "through the shipped API, replayed seven times each. Template bytes are the "
        "canonical WCB record; replay re-reads every declared input and recorded output "
        "through the verified read path and does not re-run the computation."
    ),
    "log": log_path,
    "log_sha256": hashlib.sha256(log.encode()).hexdigest(),
}
with open(receipt_path, "w") as handle:
    json.dump(document, handle, indent=1)
    handle.write("\n")
print("receipt", receipt_path, "passed", passed, "failed", failed)
PY

echo "artifact_template_court: pass (receipt: $receipt)"
