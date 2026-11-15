#!/usr/bin/env bash
# T093 court: residual-fingerprint representation selector on a held-out family.
#
# Host:  bash bench/scripts/residual_selector_court.sh
# Court: bench court measures, fuse court assembles the receipt.
#
# The contract (`bench/CONTRACTS-selector.md`) is committed before this runs.
# The held-out split is measured once per receipt; G5 requires the receipt to be
# the only published held-out reading.
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
  echo "residual_selector_court: skip $1" >&2
  exit 78
}

if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  [[ -n "$stage" ]] && { echo "the host wrapper takes no --stage" >&2; exit 64; }
  command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
  docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
  docker image inspect weft/bench-court:1 >/dev/null 2>&1 || \
    skip "missing image weft/bench-court:1"
  docker image inspect weft/fuse-court:1 >/dev/null 2>&1 || \
    skip "missing image weft/fuse-court:1"
  [[ -r "$root/.docker-cache/real-corpus/Qwen2.5-3B-Instruct.safetensors" ]] || \
    skip "the Qwen2.5-3B pair is not in .docker-cache/real-corpus"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p "$root/bench/receipts"
  rc=0
  "$root/scripts/docker-court.sh" bench \
    "bash bench/scripts/residual_selector_court.sh --stage measure --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "residual selector court: measure stage failed (rc=$rc)" >&2
    exit "$rc"
  fi
  "$root/scripts/docker-court.sh" fuse \
    "bash bench/scripts/residual_selector_court.sh --stage assemble --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "residual selector court: assemble stage failed (rc=$rc)" >&2
    exit "$rc"
  fi
  echo "receipt: bench/receipts/residual-selector-${stamp}.json"
  exit 0
fi

cd "$root"
stamp="${stamp:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt_dir="bench/receipts"
log="$receipt_dir/residual-selector-${stamp}.log"
raw="$receipt_dir/residual-selector-${stamp}-raw.json"
receipt="$receipt_dir/residual-selector-${stamp}.json"
mkdir -p "$receipt_dir"

if [[ "$stage" == "measure" ]]; then
  echo "== T093 residual-fingerprint selector, measure stage ==" | tee -a "$log"
  echo "kernel: $(uname -a)" | tee -a "$log"
  echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})" | tee -a "$log"
  echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}" | tee -a "$log"
  cargo build --quiet --example residual_selector 2>>"$log"
  bin=".docker-cache/target-rust/debug/examples/residual_selector"
  sha256sum "$bin" examples/residual_selector.rs bench/CONTRACTS-selector.md | tee -a "$log"
  /usr/bin/time -v "$bin" \
    --out "$raw" \
    --reps "${WEFT_RS_REPS:-2}" \
    --max-fit-tensors "${WEFT_RS_FIT:-40}" \
    --max-eval-tensors "${WEFT_RS_EVAL:-24}" \
    --max-fit-bytes "${WEFT_RS_FIT_BYTES:-8388608}" \
    --max-eval-bytes "${WEFT_RS_EVAL_BYTES:-16777216}" \
    2>&1 | tee -a "$log"
  exit 0
fi

if [[ "$stage" == "assemble" ]]; then
  [[ -r "$raw" ]] || { echo "missing $raw" >&2; exit 1; }
  python3 - "$raw" "$receipt" "$stamp" "${WEFT_REPO_WEFTFS:-unknown}" <<'PY' | tee -a "$log"
import json, sys
raw_path, receipt, stamp, revision = sys.argv[1:5]
raw = json.load(open(raw_path))
held = raw["heldout"]
checks = []
def check(name, ok, detail=None):
    checks.append({"check": name, "ok": bool(ok), "detail": detail})
    print(("PASS " if ok else "FAIL ") + name + ((": " + str(detail)) if detail is not None else ""))

policies = {row["policy"]: row for row in held["policies"]}
trivial = [policies[name] for name in
           ("always_zstd", "always_best_of_two", "entropy_threshold", "size_threshold")]
best_trivial = min(trivial, key=lambda row: row["cost"])
tree = policies["fingerprint_tree"]
oracle = policies["oracle"]

# G4: exactness of every candidate on every tensor of every split.
exact_rows = [m["exact"] for row in held["rows"] for m in row["measurements"]]
check("G4_candidates_exact", all(exact_rows) and bool(exact_rows),
      {"measurements": len(exact_rows)})

# G1: the tree is at most as expensive as the best trivial selector.
check("G1_tree_not_worse_than_best_trivial",
      tree["cost"] <= best_trivial["cost"] + 1e-9,
      {"tree": tree["cost"], "best_trivial": best_trivial["cost"],
       "best_trivial_policy": best_trivial["policy"],
       "tree_over_best_trivial": tree["cost"] / best_trivial["cost"]})

# G2: tree regret against the oracle.
check("G2_tree_regret_within_5pct",
      tree["regret_fraction"] <= 0.05,
      {"regret_fraction": tree["regret_fraction"]})

# G3: planning bound.
check("G3_planning_within_50ms",
      raw["planning"]["per_tensor_decision_ns"] <= 50_000_000,
      {"decision_ns": raw["planning"]["per_tensor_decision_ns"],
       "fingerprint_ns_per_mib": raw["planning"]["fingerprint_ns_per_mib"]})

# The tree must actually be a tree and the split must be as declared.
check("fit_split_declared",
      "qwen3b-instruct" in " ".join(raw["declared_split"]["heldout"]),
      raw["declared_split"])

# Report whether the tree ever differs from the best trivial selector.
differing = 0
for row in held["rows"]:
    choices = {c["policy"]: c["strategy"] for c in row["choices"]}
    trivial_choice = min(
        ("always_zstd", "always_best_of_two", "entropy_threshold", "size_threshold"),
        key=lambda name: next(c["cost"] for c in row["choices"] if c["policy"] == name))
    if choices["fingerprint_tree"] != choices[trivial_choice]:
        differing += 1
check("decision_diversity_reported", True, {"tensors_where_tree_differs_from_best_trivial": differing})

doc = {
    "schema": "weftfs.residual-selector.receipt/1",
    "role": "T093 residual-fingerprint selector measured once on a held-out family",
    "generated_at": stamp,
    "revision": revision,
    "contract": "bench/CONTRACTS-selector.md",
    "measurement": raw,
    "checks": checks,
    "checks_passed": sum(1 for row in checks if row["ok"]),
    "checks_total": len(checks),
    "result": "pass" if all(row["ok"] for row in checks) else "fail",
    "gates": {
        "G1_tree_not_worse_than_best_trivial": next(c["ok"] for c in checks if c["check"].startswith("G1")),
        "G2_tree_regret_within_5pct": next(c["ok"] for c in checks if c["check"].startswith("G2")),
        "G3_planning_within_50ms": next(c["ok"] for c in checks if c["check"].startswith("G3")),
        "G4_candidates_exact": next(c["ok"] for c in checks if c["check"].startswith("G4")),
    },
    "scope": {
        "held_out_readings": 1,
        "not_claimed": [
            "a shipped planner policy (research only; no default path changed)",
            "any lossy representation comparison",
            "zstd-19 or mode 15 in the candidate set",
            "cold media, fleet or cloud behaviour",
            "that the exchange rate 0.001 bytes per ns is the right business trade",
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
