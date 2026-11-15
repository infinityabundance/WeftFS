#!/usr/bin/env bash
# T096 court: store / fetch / reconstruct / recompute decision planner.
#
# Host:  bash bench/scripts/recompute_planner_court.sh
# Court: rust court measures, fuse court assembles the receipt.
#
# The contract gate is the shipped `weftfs::provenance::choose_recovery`. Every
# fixture also carries an *approximate* provenance record (declared determinism,
# an operation that does not reproduce the target); the court fails if that
# candidate is ever eligible or selected for an exact request.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

stage=""
stamp=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --stage) stage="$2"; shift 2 ;;
    --stamp) stamp="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 64 ;;
  esac
done

skip() {
  echo "recompute_planner_court: skip $1" >&2
  exit 78
}

if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  [[ -n "$stage" ]] && { echo "the host wrapper takes no --stage" >&2; exit 64; }
  command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
  docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
  docker image inspect weft/rust-court:1 >/dev/null 2>&1 || \
    skip "missing image weft/rust-court:1"
  docker image inspect weft/fuse-court:1 >/dev/null 2>&1 || \
    skip "missing image weft/fuse-court:1"
  [[ -r "$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors" ]] || \
    skip "the Qwen2.5-0.5B pair is not in .docker-cache/real-corpus"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p "$root/bench/receipts"
  rc=0
  "$root/scripts/docker-court.sh" rust \
    "bash bench/scripts/recompute_planner_court.sh --stage measure --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "recompute planner court: measure stage failed (rc=$rc)" >&2
    exit "$rc"
  fi
  "$root/scripts/docker-court.sh" fuse \
    "bash bench/scripts/recompute_planner_court.sh --stage assemble --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "recompute planner court: assemble stage failed (rc=$rc)" >&2
    exit "$rc"
  fi
  echo "receipt: bench/receipts/recompute-planner-${stamp}.json"
  exit 0
fi

cd "$root"
stamp="${stamp:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt_dir="bench/receipts"
log="$receipt_dir/recompute-planner-${stamp}.log"
raw="$receipt_dir/recompute-planner-${stamp}-raw.json"
receipt="$receipt_dir/recompute-planner-${stamp}.json"
mkdir -p "$receipt_dir"

if [[ "$stage" == "measure" ]]; then
  echo "== T096 recompute planner, measure stage ==" | tee -a "$log"
  echo "kernel: $(uname -a)" | tee -a "$log"
  echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})" | tee -a "$log"
  echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}" | tee -a "$log"
  cargo build --quiet --example recompute_planner_court 2>>"$log"
  bin=".docker-cache/target-rust/debug/examples/recompute_planner_court"
  sha256sum "$bin" examples/recompute_planner_court.rs | tee -a "$log"
  work="$(mktemp -d /tmp/weft-t096.XXXXXX)"
  "$bin" \
    --work "$work" \
    --out "$raw" \
    --tensors "${WEFT_RP_TENSORS:-12}" \
    --reps "${WEFT_RP_REPS:-3}" \
    --max-bytes "${WEFT_RP_MAX_BYTES:-1048576}" \
    2>&1 | tee -a "$log"
  rm -rf "$work"
  exit 0
fi

if [[ "$stage" == "assemble" ]]; then
  [[ -r "$raw" ]] || { echo "missing $raw" >&2; exit 1; }
  python3 - "$raw" "$receipt" "$stamp" "${WEFT_REPO_WEFTFS:-unknown}" <<'PY' | tee -a "$log"
import json, sys
raw_path, receipt, stamp, revision = sys.argv[1:5]
raw = json.load(open(raw_path))
held = raw["heldout"]
contract = raw["contract"]
checks = []
def check(name, ok, detail=None):
    checks.append({"check": name, "ok": bool(ok), "detail": detail})
    print(("PASS " if ok else "FAIL ") + name + ((": " + str(detail)) if detail is not None else ""))

policies = {row["policy"]: row for row in held["policies"]}
oracle = held["oracle_ns"]
best_fixed = policies["best_fixed_priority"]
mean_utility = policies["mean_utility"]
declared = policies["declared_cost"]
shipped = policies["shipped_planner"]

check("fixtures_present", raw["configuration"]["fixtures"] >= 4,
      raw["configuration"]["fixtures"])
check("every_observed_strategy_exact",
      all(obs["exact"] for row in held["rows"] for obs in row["observed_ns"]),
      sum(len(row["observed_ns"]) for row in held["rows"]))
check("recompute_chosen_only_when_exact",
      contract["recompute_chosen"] > 0
      and contract["recompute_correct_fraction"] == 1.0,
      {"chosen": contract["recompute_chosen"],
       "correct": contract["recompute_correct"],
       "fraction": contract["recompute_correct_fraction"]})
check("approximate_never_selected",
      contract["approximate_selected"] == 0
      and contract["approximate_refusals_checked"] == held["requests"],
      {"selected": contract["approximate_selected"],
       "refusals": contract["approximate_refusals_checked"]})
check("mean_utility_regret_within_1pct",
      mean_utility["regret_fraction"] <= 0.01, mean_utility["regret_fraction"])
check("fixed_priority_measurably_worse",
      best_fixed["regret_fraction"] > mean_utility["regret_fraction"],
      {"fixed": best_fixed["regret_fraction"],
       "mean_utility": mean_utility["regret_fraction"]})

doc = {
    "schema": "weftfs.recompute-planner.receipt/1",
    "role": "T096 four-strategy decision planner with the shipped exactness gate",
    "generated_at": stamp,
    "revision": revision,
    "measurement": raw,
    "checks": checks,
    "checks_passed": sum(1 for row in checks if row["ok"]),
    "checks_total": len(checks),
    "result": "pass" if all(row["ok"] for row in checks) else "fail",
    "scope": {
        "contract": "weftfs::provenance::choose_recovery (shipped, unmodified)",
        "exact_only": "an approximate provenance record (determinism declared) is attached to every fixture and is never eligible or selected",
        "not_claimed": [
            "a shipped planner change (research only)",
            "recompute of a real training step (the declared operation is a concatenation of verified inputs)",
            "cold media, fleet, cloud or power-loss behaviour",
            "that the held-out availability masks cover production outage patterns",
        ],
    },
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "result": doc["result"],
                  "checks": f"{doc['checks_passed']}/{doc['checks_total']}"}))
PY
  python3 - "$receipt" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
raise SystemExit(0 if doc["result"] == "pass" else 1)
PY
  exit 0
fi

echo "unknown stage '$stage'" >&2
exit 64
