#!/usr/bin/env bash
# F9.1 — run TLC over the WeftFS commit/recovery model.
#
# Host usage (self-dispatches into the hard-limited TLA court):
#   scripts/run_tla.sh
#
# Court usage:
#   scripts/docker-court.sh tla "bash scripts/run_tla.sh"
#
# Runs five models in one court:
#   1. verification/tla/WeftCommit.tla          corrected single-committer model
#   2. verification/tla/WeftCommitBuggy.tla     deliberately flawed recovery
#   3. verification/tla/WeftMultiWriter.tla     corrected MW2 model
#   4. verification/tla/WeftMultiWriterBuggy.tla   missing conflict validation
#   5. verification/tla/WeftMultiWriterGcBuggy.tla GC ignores prepared txns
#
# Each TLC run is bounded by TLA_TIMEOUT_SECS (default 600). A timeout is a
# failure, never a pass.
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
    timeout "${TLA_TIMEOUT_SECS:-600}" java -jar "$jar" -workers 1 -metadir "$meta/$module" \
      -config "$cfg" "$module"
  ) >"$out" 2>&1
  echo $? >"$out.status"
  set -e
}

fixed_log="$work/fixed.tlc"
buggy_log="$work/buggy.tlc"
mw_log="$work/mw.tlc"
mw_buggy_log="$work/mw-buggy.tlc"
mw_gc_log="$work/mw-gc.tlc"
echo "== TLC WeftCommit ==" | tee -a "$log"
run_model WeftCommit WeftCommit.cfg "$fixed_log"
echo "== TLC WeftCommitBuggy ==" | tee -a "$log"
run_model WeftCommitBuggy WeftCommitBuggy.cfg "$buggy_log"
echo "== TLC WeftMultiWriter ==" | tee -a "$log"
run_model WeftMultiWriter WeftMultiWriter.cfg "$mw_log"
echo "== TLC WeftMultiWriterBuggy ==" | tee -a "$log"
run_model WeftMultiWriterBuggy WeftMultiWriterBuggy.cfg "$mw_buggy_log"
echo "== TLC WeftMultiWriterGcBuggy ==" | tee -a "$log"
run_model WeftMultiWriterGcBuggy WeftMultiWriterGcBuggy.cfg "$mw_gc_log"

fixed_status="$(cat "$fixed_log.status")"
buggy_status="$(cat "$buggy_log.status")"
mw_status="$(cat "$mw_log.status")"
mw_buggy_status="$(cat "$mw_buggy_log.status")"
mw_gc_status="$(cat "$mw_gc_log.status")"

num() { grep -oE "$1" "$2" | grep -oE '[0-9]+' | tail -n 1 || true; }

fixed_states="$(num '[0-9]+ states generated' "$fixed_log")"
fixed_distinct="$(num '[0-9]+ distinct states found' "$fixed_log")"
fixed_depth="$(num 'depth of the complete state graph search is [0-9]+' "$fixed_log")"
buggy_states="$(num '[0-9]+ states generated' "$buggy_log")"
buggy_distinct="$(num '[0-9]+ distinct states found' "$buggy_log")"
buggy_depth="$(num 'depth of the complete state graph search is [0-9]+' "$buggy_log")"
mw_states="$(num '[0-9]+ states generated' "$mw_log")"
mw_distinct="$(num '[0-9]+ distinct states found' "$mw_log")"
mw_depth="$(num 'depth of the complete state graph search is [0-9]+' "$mw_log")"
mw_buggy_states="$(num '[0-9]+ states generated' "$mw_buggy_log")"
mw_buggy_distinct="$(num '[0-9]+ distinct states found' "$mw_buggy_log")"
mw_buggy_depth="$(num 'depth of the complete state graph search is [0-9]+' "$mw_buggy_log")"
mw_gc_states="$(num '[0-9]+ states generated' "$mw_gc_log")"
mw_gc_distinct="$(num '[0-9]+ distinct states found' "$mw_gc_log")"
mw_gc_depth="$(num 'depth of the complete state graph search is [0-9]+' "$mw_gc_log")"

fixed_maxgen="$(sed -n 's/^CONSTANT MaxGen = //p' verification/tla/WeftCommit.cfg)"
buggy_maxgen="$(sed -n 's/^CONSTANT MaxGen = //p' verification/tla/WeftCommitBuggy.cfg)"
mw_maxgen="$(sed -n 's/^  MaxGen = //p' verification/tla/WeftMultiWriter.cfg)"
mw_buggy_maxgen="$(sed -n 's/^  MaxGen = //p' verification/tla/WeftMultiWriterBuggy.cfg)"
mw_gc_maxgen="$(sed -n 's/^  MaxGen = //p' verification/tla/WeftMultiWriterGcBuggy.cfg)"

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

mw_result="fail"
if [[ "$mw_status" == "0" ]] && grep -q 'No error has been found' "$mw_log"; then
  mw_result="pass"
fi

mw_buggy_result="unexpected-pass"
mw_violated=""
if grep -q 'is violated' "$mw_buggy_log"; then
  mw_buggy_result="expected-violation"
  mw_violated="$(sed -n 's/.*Invariant \([A-Za-z0-9_]*\) is violated.*/\1/p' "$mw_buggy_log" | head -n 1)"
fi

mw_gc_result="unexpected-pass"
mw_gc_violated=""
if grep -q 'is violated' "$mw_gc_log"; then
  mw_gc_result="expected-violation"
  mw_gc_violated="$(sed -n 's/.*Invariant \([A-Za-z0-9_]*\) is violated.*/\1/p' "$mw_gc_log" | head -n 1)"
fi

# The counterexample trace is preserved verbatim in the committed log. Also
# place it in the receipt for easy consumption.
trace="$(sed -n '/The behavior up to this point is:/,$p' "$buggy_log" | head -n 80)"
mw_trace="$(sed -n '/The behavior up to this point is:/,$p' "$mw_buggy_log" | head -n 120)"
mw_gc_trace="$(sed -n '/The behavior up to this point is:/,$p' "$mw_gc_log" | head -n 120)"
fixed_tail="$(tail -n 12 "$fixed_log")"
mw_tail="$(tail -n 12 "$mw_log")"

{
  echo "===== WeftCommit (corrected) ====="
  cat "$fixed_log"
  echo
  echo "===== WeftCommitBuggy (deliberately flawed) ====="
  cat "$buggy_log"
  echo
  echo "===== WeftMultiWriter (corrected) ====="
  cat "$mw_log"
  echo
  echo "===== WeftMultiWriterBuggy (deliberately flawed: no conflict validation) ====="
  cat "$mw_buggy_log"
  echo
  echo "===== WeftMultiWriterGcBuggy (deliberately flawed: GC ignores prepared txns) ====="
  cat "$mw_gc_log"
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
  --argjson mw_states "${mw_states:-0}" \
  --argjson mw_distinct "${mw_distinct:-0}" \
  --argjson mw_depth "${mw_depth:-0}" \
  --argjson mw_buggy_states "${mw_buggy_states:-0}" \
  --argjson mw_buggy_distinct "${mw_buggy_distinct:-0}" \
  --argjson mw_buggy_depth "${mw_buggy_depth:-0}" \
  --argjson mw_gc_states "${mw_gc_states:-0}" \
  --argjson mw_gc_distinct "${mw_gc_distinct:-0}" \
  --argjson mw_gc_depth "${mw_gc_depth:-0}" \
  --arg mw_result "$mw_result" \
  --arg mw_buggy_result "$mw_buggy_result" \
  --arg mw_violated "$mw_violated" \
  --arg mw_gc_result "$mw_gc_result" \
  --arg mw_gc_violated "$mw_gc_violated" \
  --arg mw_tail "$mw_tail" \
  --arg mw_trace "$mw_trace" \
  --arg mw_gc_trace "$mw_gc_trace" \
  --argjson mw_maxgen "${mw_maxgen:-0}" \
  --argjson mw_buggy_maxgen "${mw_buggy_maxgen:-0}" \
  --argjson mw_gc_maxgen "${mw_gc_maxgen:-0}" \
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
      },
      multiwriter_corrected: {
        module: "WeftMultiWriter", spec: "verification/tla/WeftMultiWriter.tla",
        config: "verification/tla/WeftMultiWriter.cfg",
        constants: {MaxGen: $mw_maxgen},
        invariants: ["TypeOK","HeadWellFormed","NoPartial","NoLostAck",
                     "NoBlindOverwrite","CommittedBlobsDurable",
                     "LivePreparedDurable","ReaderIsAPrefix",
                     "GcNeverReclaimsReachable","CommitOrderIsStable"],
        result: $mw_result,
        states_generated: $mw_states, distinct_states_found: $mw_distinct,
        search_depth: $mw_depth, output_tail: $mw_tail
      },
      multiwriter_flawed_conflict: {
        module: "WeftMultiWriterBuggy", spec: "verification/tla/WeftMultiWriterBuggy.tla",
        config: "verification/tla/WeftMultiWriterBuggy.cfg",
        constants: {MaxGen: $mw_buggy_maxgen},
        injection: "publication and rebase omit the observed-version check (last-writer-wins)",
        result: $mw_buggy_result, violated_invariant: $mw_violated,
        states_generated: $mw_buggy_states, distinct_states_found: $mw_buggy_distinct,
        search_depth: $mw_buggy_depth, counterexample: $mw_trace
      },
      multiwriter_flawed_gc: {
        module: "WeftMultiWriterGcBuggy", spec: "verification/tla/WeftMultiWriterGcBuggy.tla",
        config: "verification/tla/WeftMultiWriterGcBuggy.cfg",
        constants: {MaxGen: $mw_gc_maxgen},
        injection: "GC marks only from the head and ignores a live prepared transaction",
        result: $mw_gc_result, violated_invariant: $mw_gc_violated,
        states_generated: $mw_gc_states, distinct_states_found: $mw_gc_distinct,
        search_depth: $mw_gc_depth, counterexample: $mw_gc_trace
      }
    },
    evidence: {log: $log, log_sha256: $log_sha},
    commands: {
      court: "scripts/docker-court.sh tla \"bash scripts/run_tla.sh\"",
      corrected: "java -jar /opt/tla/tla2tools.jar -workers 1 -config WeftCommit.cfg WeftCommit",
      flawed: "java -jar /opt/tla/tla2tools.jar -workers 1 -config WeftCommitBuggy.cfg WeftCommitBuggy",
      multiwriter_corrected: "java -jar /opt/tla/tla2tools.jar -workers 1 -config WeftMultiWriter.cfg WeftMultiWriter",
      multiwriter_flawed_conflict: "java -jar /opt/tla/tla2tools.jar -workers 1 -config WeftMultiWriterBuggy.cfg WeftMultiWriterBuggy",
      multiwriter_flawed_gc: "java -jar /opt/tla/tla2tools.jar -workers 1 -config WeftMultiWriterGcBuggy.cfg WeftMultiWriterGcBuggy"
    },
    scope: {
      model_checked: "bounded TLC run of the abstract state machine only; each run is bounded by TLA_TIMEOUT_SECS",
      not_checked: "Rust/Go code refinement, checkpoints, capabilities, physical media, linearizability of a finite run"
    }
  }' >"$receipt"

rm -rf "$work"
echo "tla receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"

if [[ "$fixed_result" != "pass" || "$buggy_result" != "expected-violation" \
      || "$mw_result" != "pass" \
      || "$mw_buggy_result" != "expected-violation" \
      || "$mw_gc_result" != "expected-violation" ]]; then
  echo "TLA court FAILED (commit=$fixed_result/$buggy_result mw=$mw_result/$mw_buggy_result gc=$mw_gc_result)" >&2
  exit 1
fi
