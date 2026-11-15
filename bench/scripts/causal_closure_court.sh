#!/usr/bin/env bash
# P02 court: matched causal-closure costs and the clean-room verifier.
#
#   bench/scripts/causal_closure_court.sh
#
# Runs the release harness in the bench court (3 GiB / 4 cpus / 1024 pids) for a
# 3,845-object graph and a small 245-object graph, then checks both closure
# bundles with `verifier/closure_check.py` (stdlib only, no WeftFS import).
# Exit 78 with one `causal_closure: skip <reason>` line when the environment
# cannot run it; anything after the preflight is a failure.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

if [[ "$(uname -m)" != "x86_64" ]]; then
  echo "causal_closure: skip this court is pinned to the x86_64 images"
  exit 78
fi
if ! command -v docker >/dev/null 2>&1; then
  echo "causal_closure: skip no docker CLI on PATH"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "causal_closure: skip no reachable docker daemon"
  exit 78
fi

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work=".docker-cache/causal-closure-${stamp}"
mkdir -p "$work" bench/receipts
rm -rf "$work"/*
log="bench/receipts/causal-closure-${stamp}.log"
: > "$log"
chmod 0666 "$log"

run() {
  local bundles="$1" branches="$2" reps="$3" out="$4"
  WEFT_STAMP="$stamp" WEFT_CLOSURE_BUNDLE="/work/$bundles" \
    scripts/docker-court.sh bench \
    "WEFT_STAMP=${stamp} WEFT_CLOSURE_BUNDLE=/work/${bundles} cargo run --release --quiet --example causal_closure_bench -- /work/${out} ${branches} ${reps}" \
    >>"$log" 2>&1
}

echo "== main graph ==" | tee -a "$log"
run "$work/bundle.json" 64 50 "bench/receipts/causal-closure-${stamp}.json"

echo "== small graph (index-build amortisation boundary) ==" | tee -a "$log"
run "$work/bundle-small.json" 4 50 "bench/receipts/causal-closure-small-${stamp}.json"

echo "== clean-room verifier ==" | tee -a "$log"
python3 verifier/closure_check.py --bundle "$work/bundle.json" > "$work/verify-main.json" 2>>"$log"
python3 verifier/closure_check.py --bundle "$work/bundle-small.json" > "$work/verify-small.json" 2>>"$log"

python3 - "$stamp" "$work" <<'PY'
import json
import sys

stamp, work = sys.argv[1], sys.argv[2]
main_path = "bench/receipts/causal-closure-%s.json" % stamp
small_path = "bench/receipts/causal-closure-small-%s.json" % stamp
with open(main_path) as handle:
    main = json.load(handle)
with open(small_path) as handle:
    small = json.load(handle)
with open(work + "/verify-main.json") as handle:
    verify_main = json.load(handle)
with open(work + "/verify-small.json") as handle:
    verify_small = json.load(handle)

checks = list(main.get("checks", []))
checks.append({"name": "verifier_main_bundle", "ok": verify_main.get("ok") is True,
               "detail": verify_main.get("error") or ("%s records, %s nodes" % (
                   verify_main.get("records"), verify_main.get("nodes")))})
checks.append({"name": "verifier_small_bundle", "ok": verify_small.get("ok") is True,
               "detail": verify_small.get("error") or ("%s records, %s nodes" % (
                   verify_small.get("records"), verify_small.get("nodes")))})
checks.append({"name": "verifier_counts_match",
               "ok": verify_main.get("nodes") == main["index"]["node_counts"]["complete"]["nodes"]
                     and verify_small.get("nodes") == small["index"]["node_counts"]["complete"]["nodes"],
               "detail": "%s / %s" % (verify_main.get("nodes"), verify_small.get("nodes"))})
result = "pass" if all(check["ok"] for check in checks) else "fail"
main["verifier"] = {"main": verify_main, "small": verify_small}
main["small_graph"] = small
main["checks"] = checks
main["result"] = result
with open(main_path, "w") as handle:
    json.dump(main, handle, indent=2, sort_keys=True)
    handle.write("\n")
for check in checks:
    print(("PASS " if check["ok"] else "FAIL ") + check["name"] + ": " + str(check.get("detail", "")))
print("result=" + result)
if result != "pass":
    sys.exit(1)
PY
