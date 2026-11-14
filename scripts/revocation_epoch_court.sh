#!/usr/bin/env bash
# T078 court: revocation epochs, stale-client refusal, tenant scoping and
# deny-before-decode.
#
#   scripts/revocation_epoch_court.sh
#
# The counter-level deny-before-decode proof is `tests/revocation_epoch.rs`,
# run here in the rust court; the CLI scenario below records the on-disk chain.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v docker >/dev/null 2>&1; then
  echo "revocation_epoch: skip no docker CLI on PATH"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "revocation_epoch: skip no reachable docker daemon"
  exit 78
fi

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work=".docker-cache/revocation-epoch-${stamp}"
rm -rf "$work"
mkdir -p "$work"
results="$work/results.txt"
log="bench/receipts/revocation-epoch-${stamp}.log"
mkdir -p bench/receipts
: > "$results"
: > "$log"
cli="/work/.docker-cache/target-rust/release/weftfs"

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
  local name="$1"; shift
  local rc=0
  "$@" >>"$log" 2>&1 || rc=$?
  record "$name" "$rc"
}

echo "== revocation_epoch rust tests ==" | tee -a "$log"
step rust_tests scripts/docker-court.sh rust \
  "cargo test --test revocation_epoch -- --test-threads=1"
grep -q "test result: ok. 9 passed" "$log" && record rust_tests_count 0 || record rust_tests_count 1

step rust_build scripts/docker-court.sh rust \
  "cargo build --release --bin weftfs"

step cli_init scripts/docker-court.sh rust \
  "$cli --store ${work}/store init"
step cli_create scripts/docker-court.sh rust \
  "$cli --store ${work}/store create /artifact.bin --text 'epoch scoped bytes'"
object_id="$(scripts/docker-court.sh rust "$cli --store ${work}/store --json stat /artifact.bin" 2>>"$log" | python3 -c 'import json,sys;print(json.load(sys.stdin)["object_id"])')"
content_root="$(scripts/docker-court.sh rust "$cli --store ${work}/store --json artifact resolve --object-id ${object_id}" 2>>"$log" | python3 -c 'import json,sys;print(json.load(sys.stdin)["entries"][0]["content_root"])')"

step cli_publish scripts/docker-court.sh rust \
  "$cli --store ${work}/store --json revocation publish --tenant tenant-a --kind content --id ${content_root} --valid-for-secs 300"
step cli_show scripts/docker-court.sh rust \
  "$cli --store ${work}/store --json revocation show"
step cli_verify scripts/docker-court.sh rust \
  "$cli --store ${work}/store --json revocation verify"
step cli_observe scripts/docker-court.sh rust \
  "$cli --store ${work}/store --json revocation observe --max-staleness-secs 60"

# A second epoch advances the chain; the earlier observation is now stale.
scripts/docker-court.sh rust \
  "$cli --store ${work}/store --json revocation publish --tenant tenant-b --kind content --id $(printf 'ab%.0s' {1..32}) --valid-for-secs 300" \
  >>"$log" 2>&1
epoch_two="$(scripts/docker-court.sh rust "$cli --store ${work}/store --json revocation show" 2>>"$log" | python3 -c 'import json,sys;print(json.load(sys.stdin)["epoch"])')"
if [[ "$epoch_two" == "2" ]]; then record epoch_advanced 0; else record epoch_advanced 1; fi
step cli_verify_after_step scripts/docker-court.sh rust \
  "$cli --store ${work}/store --json revocation verify"

# Forged and rolled-back epoch records are exercised at the record level by
# tests/revocation_epoch.rs (the CLI has no safe way to forge bytes).

total="$(grep -c . "$results" || true)"
passed="$(grep -c '=pass$' "$results" || true)"
failed="$(grep -c '=fail$' "$results" || true)"
stamp_value="$stamp" total="$total" passed="$passed" failed="$failed" \
work="$work" results="$results" epoch_two="$epoch_two" \
object_id="$object_id" content_root="$content_root" python3 - <<'PY'
import json, os, subprocess
from pathlib import Path
checks = []
for line in Path(os.environ["results"]).read_text().splitlines():
    name, _, value = line.partition("=")
    checks.append({"check": name, "result": value})
data = {
    "kind": "revocation-epoch",
    "schema": "weft.revocation-epoch.receipt.v1",
    "generated_utc": os.environ["stamp_value"],
    "environment": {
        "revisions": {"WeftFS": subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()},
        "court": "rust court (3g/4cpu/pids 1024), bounded tmpfs",
        "label": "single store, single host; store-local epoch, not a gossip protocol",
    },
    "object_id": os.environ["object_id"],
    "content_root": os.environ["content_root"],
    "final_epoch": int(os.environ["epoch_two"] or 0),
    "checks": checks,
    "passed": int(os.environ["passed"]),
    "failed": int(os.environ["failed"]),
    "workdir": os.environ["work"],
}
data["result"] = "pass" if data["failed"] == 0 else "fail"
path = Path("bench/receipts") / ("revocation-epoch-%s.json" % os.environ["stamp_value"])
path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
print(json.dumps({"receipt": str(path), "passed": data["passed"],
                  "failed": data["failed"], "result": data["result"]}))
PY

echo "revocation_epoch: $([ "$failed" -eq 0 ] && echo pass || echo fail)"
[[ "$failed" -eq 0 ]]
