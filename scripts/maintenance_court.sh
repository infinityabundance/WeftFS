#!/usr/bin/env bash
# T080 court: bounded maintenance, restart, two compactors, and the cost of the
# interference.
#
# Runs the suite and the harness in the Rust court (the harness spawns its own
# child processes for the kill and two-compactor arms), then assembles a
# receipt on the host. Exit 0 on a complete run, 78 with one
# `maintenance_court: skip <reason>` line when it cannot run.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

skip() { echo "maintenance_court: skip $1"; exit 78; }

case "$(uname -m)" in
  x86_64|aarch64) ;;
  *) skip "unsupported architecture $(uname -m)" ;;
esac
command -v docker >/dev/null 2>&1 || skip "no docker CLI"
docker info >/dev/null 2>&1 || skip "no docker daemon"
command -v python3 >/dev/null 2>&1 || skip "no host python3 for receipt assembly"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/maintenance-${stamp}.json"
log="bench/receipts/maintenance-${stamp}.log"

if ! scripts/docker-court.sh rust "
  set -Eeuo pipefail
  cd /work
  cargo test --test maintenance --quiet 2>&1 | tail -3
  cargo run --release --quiet --example maintenance_court -- \
    --out .docker-cache/maintenance-court --json .docker-cache/maintenance-court/result.json
  cat .docker-cache/maintenance-court/result.json
" >"$log" 2>&1; then
  skip "court run failed (see $log)"
fi

python3 - "$stamp" "$receipt" "$log" <<'PY'
import hashlib, json, os, re, subprocess, sys

stamp, receipt_path, log_path = sys.argv[1:4]
log = open(log_path, encoding="utf-8", errors="replace").read()

match = re.search(r"test result: ok\. (\d+) passed; (\d+) failed", log)
if not match:
    raise SystemExit("maintenance court: could not read the suite result from the log")
unit_passed, unit_failed = int(match.group(1)), int(match.group(2))

decoder = json.JSONDecoder()
harness = None
for start in [index for index, char in enumerate(log) if char == "{"]:
    try:
        candidate, _ = decoder.raw_decode(log[start:])
    except ValueError:
        continue
    if isinstance(candidate, dict) and candidate.get("schema") == "weft.maintenance-court/v1":
        harness = candidate
if harness is None:
    raise SystemExit("maintenance court: harness JSON missing from the log")

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
    "name": "tests/maintenance.rs",
    "pass": unit_failed == 0 and unit_passed >= 8,
    "detail": "%d passed, %d failed" % (unit_passed, unit_failed),
})
passed = sum(1 for check in checks if check["pass"])
failed = len(checks) - passed

document = {
    "schema": "weft.maintenance-court/v1",
    "stamp": stamp,
    "generated_utc": stamp,
    "environment": env,
    "checks": checks,
    "passed": passed,
    "failed": failed,
    "result": "pass" if failed == 0 else "fail",
    "metrics": harness["metrics"],
    "method": (
        "120 live 4 KiB artifacts and 600 unlinked 8 KiB files (distinct content, no "
        "automatic checkpoint, 4 KiB chunks). Interference: 12,000 whole-object reads "
        "against a control store and the same reads while a maintenance pass runs at a "
        "declared 4 MiB/s and 10% duty cycle on 4 CPUs. Restart: a child process is "
        "SIGKILLed after its first persisted batch, then the store is reopened and "
        "resumed. Compactors: two optimistic-mode child processes sweep the same plan; "
        "their deletions must sum to the plan exactly."
    ),
    "log": log_path,
    "log_sha256": hashlib.sha256(log.encode()).hexdigest(),
}
with open(receipt_path, "w") as handle:
    json.dump(document, handle, indent=1)
    handle.write("\n")
print("receipt", receipt_path, "passed", passed, "failed", failed)
PY

echo "maintenance_court: pass (receipt: $receipt)"
