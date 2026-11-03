#!/usr/bin/env bash
# F9.1 — run TLC over the WeftFS commit/recovery model.
#
# Host usage (self-dispatches into the hard-limited TLA court):
#   scripts/run_tla.sh
#
# Court usage:
#   scripts/docker-court.sh tla "bash scripts/run_tla.sh"
#
# Runs two models in one court:
#   1. verification/tla/WeftCommit.tla      corrected model, all invariants
#   2. verification/tla/WeftCommitBuggy.tla deliberately flawed recovery
#
# Writes only under bench/receipts/ (the court cannot write other repo paths
# without CAP_DAC_OVERRIDE):
#   bench/receipts/tla-<UTC>.json
#   bench/receipts/tla-<UTC>.log
#
# The receipt records TLC's exact state-space size, search depth, and result.
# "Model checked" means these bounded TLC runs; it does NOT mean the Rust or
# Go implementation is formally verified. See verification/README.md.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  exec "$root/scripts/docker-court.sh" tla "bash scripts/run_tla.sh"
fi
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
start_epoch="$(date +%s)"
receipt="bench/receipts/tla-${stamp}.json"
log="bench/receipts/tla-${stamp}.log"
mkdir -p bench/receipts

jar="${TLA_JAR:-/opt/tla/tla2tools.jar}"
[[ -r "$jar" ]] || { echo "missing $jar" >&2; exit 1; }
jar_sha="$(sha256sum "$jar" | cut -d' ' -f1)"
tla_version="$(sed -n 's/^TLA_VERSION=//p' /opt/tla/VERSION 2>/dev/null || echo unknown)"
java_version="$(java -version 2>&1 | head -n 1)"

work="$(mktemp -d /tmp/weft-tla.XXXXXX)"
meta="$work/meta"
mkdir -p "$meta"
cp verification/tla/*.tla verification/tla/*.cfg "$work/"

: > "$log"
echo "== TLA court ==" | tee -a "$log"
echo "kernel: $(uname -a)" | tee -a "$log"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})" | tee -a "$log"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} swap=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}" | tee -a "$log"
echo "java: $java_version" | tee -a "$log"
echo "tla2tools: $tla_version sha256=$jar_sha" | tee -a "$log"

run_model() {
  local module="$1" cfg="$2" out="$3"
  set +e
  (
    cd "$work"
    java -jar "$jar" -workers 1 -metadir "$meta/$module" \
      -config "$cfg" "$module"
  ) >"$out" 2>&1
  echo $? >"$out.status"
  set -e
}

fixed_log="$work/fixed.tlc"
buggy_log="$work/buggy.tlc"
echo "== TLC WeftCommit ==" | tee -a "$log"
run_model WeftCommit WeftCommit.cfg "$fixed_log"
echo "== TLC WeftCommitBuggy ==" | tee -a "$log"
run_model WeftCommitBuggy WeftCommitBuggy.cfg "$buggy_log"

fixed_status="$(cat "$fixed_log.status")"
buggy_status="$(cat "$buggy_log.status")"

num() { grep -oE "$1" "$2" | grep -oE '[0-9]+' | tail -n 1 || true; }

fixed_states="$(num '[0-9]+ states generated' "$fixed_log")"
fixed_distinct="$(num '[0-9]+ distinct states found' "$fixed_log")"
fixed_depth="$(num 'depth of the complete state graph search is [0-9]+' "$fixed_log")"
buggy_states="$(num '[0-9]+ states generated' "$buggy_log")"
buggy_distinct="$(num '[0-9]+ distinct states found' "$buggy_log")"
buggy_depth="$(num 'depth of the complete state graph search is [0-9]+' "$buggy_log")"

fixed_maxgen="$(sed -n 's/^CONSTANT MaxGen = //p' verification/tla/WeftCommit.cfg)"
buggy_maxgen="$(sed -n 's/^CONSTANT MaxGen = //p' verification/tla/WeftCommitBuggy.cfg)"

fixed_result="fail"
if [[ "$fixed_status" == "0" ]] && grep -q 'No error has been found' "$fixed_log"; then
  fixed_result="pass"
fi

buggy_result="unexpected-pass"
violated=""
if grep -q 'is violated' "$buggy_log"; then
  buggy_result="expected-violation"
  violated="$(sed -n 's/.*Invariant \([A-Za-z0-9_]*\) is violated.*/\1/p' "$buggy_log" | head -n 1)"
fi

# The counterexample trace is preserved verbatim in the committed log. Also
# place it in the receipt for easy consumption.
trace="$(sed -n '/The behavior up to this point is:/,$p' "$buggy_log" | head -n 80)"
fixed_tail="$(tail -n 12 "$fixed_log")"

{
  echo "===== WeftCommit (corrected) ====="
  cat "$fixed_log"
  echo
  echo "===== WeftCommitBuggy (deliberately flawed) ====="
  cat "$buggy_log"
} >>"$log"

finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
duration_ms=$(( ($(date +%s) - start_epoch) * 1000 ))
log_sha="$(sha256sum "$log" | cut -d' ' -f1)"
rev_weftfs="${WEFT_REPO_WEFTFS:-unknown}"
rev_go="${WEFT_REPO_WEFTFS_GO:-unknown}"

jq -n \
  --arg schema "weftfs.tla.receipt/1" \
  --arg generated_at "$finished_at" \
  --arg started_at "$started_at" \
  --arg finished_at "$finished_at" \
  --argjson duration_ms "$duration_ms" \
  --arg court "${WEFT_COURT_IMAGE:-unknown}" \
  --arg image_id "${WEFT_COURT_IMAGE_ID:-unknown}" \
  --arg kernel "$(uname -a)" \
  --arg java "$java_version" \
  --arg tla_version "$tla_version" \
  --arg tla_sha256 "$jar_sha" \
  --arg limit_memory "${WEFT_COURT_MEMORY:-?}" \
  --arg limit_cpus "${WEFT_COURT_CPUS:-?}" \
  --arg limit_pids "${WEFT_COURT_PIDS:-?}" \
  --arg rev_weftfs "$rev_weftfs" \
  --arg rev_go "$rev_go" \
  --argjson fixed_maxgen "${fixed_maxgen:-0}" \
  --argjson buggy_maxgen "${buggy_maxgen:-0}" \
  --arg fixed_result "$fixed_result" \
  --arg buggy_result "$buggy_result" \
  --arg violated "$violated" \
  --arg fixed_tail "$fixed_tail" \
  --arg trace "$trace" \
  --arg log "$log" \
  --arg log_sha "$log_sha" \
  --argjson fixed_states "${fixed_states:-0}" \
  --argjson fixed_distinct "${fixed_distinct:-0}" \
  --argjson fixed_depth "${fixed_depth:-0}" \
  --argjson buggy_states "${buggy_states:-0}" \
  --argjson buggy_distinct "${buggy_distinct:-0}" \
  --argjson buggy_depth "${buggy_depth:-0}" \
  '{
    schema: $schema,
    generated_at: $generated_at,
    run: {started_at: $started_at, finished_at: $finished_at, duration_ms: $duration_ms},
    environment: {
      court: $court, image_id: $image_id, kernel: $kernel, java: $java,
      tla_version: $tla_version, tla2tools_sha256: $tla_sha256,
      limit_memory: $limit_memory, limit_memory_swap: $limit_memory,
      limit_cpus: $limit_cpus, limit_pids: $limit_pids
    },
    repositories: [
      {name: "WeftFS", revision: $rev_weftfs},
      {name: "WeftFS-Go", revision: $rev_go}
    ],
    models: {
      corrected: {
        module: "WeftCommit", spec: "verification/tla/WeftCommit.tla",
        config: "verification/tla/WeftCommit.cfg", constants: {MaxGen: $fixed_maxgen},
        invariants: ["TypeOK","NoPartial","HighestValid","NoLostDependency",
                     "Durability","ReturnedObjectsDurable","ReplayIdempotent"],
        result: $fixed_result,
        states_generated: $fixed_states, distinct_states_found: $fixed_distinct,
        search_depth: $fixed_depth,
        output_tail: $fixed_tail
      },
      flawed: {
        module: "WeftCommitBuggy", spec: "verification/tla/WeftCommitBuggy.tla",
        config: "verification/tla/WeftCommitBuggy.cfg", constants: {MaxGen: $buggy_maxgen},
        injection: "recovery trusts the highest commit file, ignoring superblock visibility and CRC",
        result: $buggy_result, violated_invariant: $violated,
        states_generated: $buggy_states, distinct_states_found: $buggy_distinct,
        search_depth: $buggy_depth,
        counterexample: $trace
      }
    },
    evidence: {log: $log, log_sha256: $log_sha},
    scope: {
      model_checked: "bounded TLC run of the abstract state machine only",
      not_checked: "Rust/Go code refinement, checkpoints, GC, capabilities, physical media"
    }
  }' >"$receipt"

rm -rf "$work"
echo "tla receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"

if [[ "$fixed_result" != "pass" || "$buggy_result" != "expected-violation" ]]; then
  echo "TLA court FAILED (corrected=$fixed_result buggy=$buggy_result)" >&2
  exit 1
fi
