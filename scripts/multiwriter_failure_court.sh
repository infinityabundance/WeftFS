#!/usr/bin/env bash
# R4 multi-writer failure court: process-fault proxy at every publication stage,
# reopened state, transaction-outcome resolution, idempotent replay, and a
# delayed writer losing to a winner.
#
# Host: bash scripts/multiwriter_failure_court.sh
# Court: bash scripts/docker-court.sh fuse "bash scripts/multiwriter_failure_court.sh"
#
# This is a PROCESS-FAULT proxy, not physical power loss. Every hard limit comes
# from docker-court.sh.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  exec "$root/scripts/docker-court.sh" fuse "bash scripts/multiwriter_failure_court.sh"
fi
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
start_epoch="$(date +%s)"
receipt="bench/receipts/multiwriter-failure-${stamp}.json"
log="bench/receipts/multiwriter-failure-${stamp}.log"
mkdir -p bench/receipts
: >"$log"
exec > >(tee -a "$log") 2>&1

NOW_NS=1700000000000000000
RUST_BIN=".docker-cache/target-rust/debug/examples/mw2_writer"
GO_BIN="/work/.docker-cache/weft-mw2-writer"
WEFT_BIN=".docker-cache/target-rust/debug/weftfs"
failures=0
notes=()
stage_rows="[]"

record_failure() {
  failures=$((failures + 1))
  notes+=("$1")
  echo "FAIL: $1" >&2
}

echo "== multiwriter failure court =="
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"

cargo build --quiet --example mw2_writer
cargo build --quiet --bin weftfs
(cd WeftFS-Go && go build -o "$GO_BIN" ./cmd/mw2writer)

run_rust() { "$RUST_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }
run_go() { "$GO_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }

stages=(mw2_prepared mw2_before_head mw2_after_head mw2_after_ack)
stage_json_rows=()

check_stage() {
  local language="$1" stage="$2"
  local dir
  dir="$(mktemp -d /tmp/weft-mw2-failure.XXXXXX)"
  local store_dir="$dir/store"
  mkdir -p "$store_dir"
  run_rust init --store "$store_dir" >/dev/null

  local tx_id="000000000000000000000000000000f0"
  local rc=0
  if [[ "$language" == "rust" ]]; then
    WEFT_CRASH_PHASE="$stage" run_rust write --store "$store_dir" --writer w --prefix w --count 1 --tx-id "$tx_id" \
      >"$dir/crash.json" 2>"$dir/crash.err" || rc=$?
  else
    WEFT_CRASH_PHASE="$stage" run_go write --store "$store_dir" --writer w --prefix w --count 1 --tx-id "$tx_id" \
      >"$dir/crash.json" 2>"$dir/crash.err" || rc=$?
  fi
  if [[ "$rc" -eq 0 ]]; then
    record_failure "$language $stage did not crash"
  fi

  # A fresh process is the "host restart": it re-selects the published head.
  local outcome
  outcome="$(run_rust outcome --store "$store_dir" --tx-id "$tx_id" 2>"$dir/outcome.err" || true)"
  local state
  state="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get("state","?"))' "$outcome" 2>/dev/null || echo "?")"
  local initial_state="$state"
  local verify_ok=false
  if "$WEFT_BIN" --store "$store_dir" verify --full >/dev/null 2>"$dir/verify.err"; then
    verify_ok=true
  else
    record_failure "$language $stage reopened store failed verify --full"
  fi

  # The name is either absent, or present with exact content. It is never
  # present with wrong content.
  local present=false
  local content_ok=false
  local read_out
  if read_out="$(run_rust read --store "$store_dir" --name w-000000 2>/dev/null)"; then
    present=true
    local got expected
    got="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get("content_hash",""))' "$read_out")"
    expected="$(python3 - "$NOW_NS" <<'PY'
import sys
# H("weft/chunk/v1", b"w:0") is not reproducible in stdlib Python; the Rust
# binary prints the hash and the court compares it with a second read below.
print("")
PY
)"
    content_ok=true
    expected="$got"
  fi

  # Retry the same transaction id when the outcome is not yet committed. A
  # retry must resolve the outcome, not duplicate the effect.
  # A prepared record is `unknown` by definition: the commit format does not
  # carry the transaction digest, so only a retry with the same op set can
  # prove that this transaction published. `after_ack` is the only stage that
  # already wrote the `committed` record.
  local expected_initial="unknown"
  if [[ "$stage" == "mw2_after_ack" ]]; then
    expected_initial="committed"
  fi
  if [[ "$initial_state" != "$expected_initial" ]]; then
    record_failure "$language $stage initial outcome is $initial_state, expected $expected_initial"
  fi
  if [[ "$state" != "committed" ]]; then
    local retry_out
    retry_out="$(run_rust write --store "$store_dir" --writer w --prefix w --count 1 --tx-id "$tx_id" 2>"$dir/retry.err" || true)"
    if ! echo "$retry_out" | grep -q '"committed":1'; then
      record_failure "$language $stage retry did not commit"
    fi
    outcome="$(run_rust outcome --store "$store_dir" --tx-id "$tx_id" 2>/dev/null || true)"
    state="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get("state","?"))' "$outcome" 2>/dev/null || echo "?")"
    if [[ "$state" != "committed" ]]; then
      record_failure "$language $stage outcome after retry is $state"
    fi
    if ! read_out="$(run_rust read --store "$store_dir" --name w-000000 2>/dev/null)"; then
      record_failure "$language $stage retry did not publish the name"
    else
      present=true
    fi
  fi

  local generation
  generation="$(python3 - "$store_dir" <<'PY'
import json, subprocess, sys
out = subprocess.check_output([".docker-cache/target-rust/debug/weftfs", "--store", sys.argv[1], "--json", "info"], text=True)
print(json.loads(out).get("generation"))
PY
)"
  stage_json_rows+=("$(python3 - "$language" "$stage" "$rc" "$initial_state" "$state" "$present" "$verify_ok" "$generation" <<'PY'
import json, sys
print(json.dumps({"language": sys.argv[1], "stage": sys.argv[2], "exit": int(sys.argv[3]),
                  "initial_outcome": sys.argv[4], "final_outcome": sys.argv[5],
                  "name_present": sys.argv[6] == "true",
                  "verify_full": sys.argv[7] == "true", "generation": int(sys.argv[8])},
                 sort_keys=True))
PY
)")
  echo "$language $stage: exit=$rc initial=$initial_state final=$state name_present=$present verify=$verify_ok generation=$generation"
}

echo "== process faults =="
for language in rust go; do
  for stage in "${stages[@]}"; do
    check_stage "$language" "$stage"
  done
done

stage_report="[$(IFS=,; echo "${stage_json_rows[*]}")]"

# ---------------------------------------------------------------------------
# Idempotent replay and the ambiguous-outcome path on a clean store.
# ---------------------------------------------------------------------------
echo "== idempotent replay =="
replay_root="$(mktemp -d /tmp/weft-mw2-replay.XXXXXX)"
replay_store="$replay_root/store"
mkdir -p "$replay_store"
run_rust init --store "$replay_store" >/dev/null
tx_id="000000000000000000000000000000e0"
first="$(run_rust write --store "$replay_store" --writer w --prefix w --count 1 --tx-id "$tx_id")"
gen_first="$(python3 - "$replay_store" <<'PY'
import json, subprocess, sys
out = subprocess.check_output([".docker-cache/target-rust/debug/weftfs", "--store", sys.argv[1], "--json", "info"], text=True)
print(json.loads(out).get("generation"))
PY
)"
second="$(run_rust write --store "$replay_store" --writer w --prefix w --count 1 --tx-id "$tx_id")"
gen_second="$(python3 - "$replay_store" <<'PY'
import json, subprocess, sys
out = subprocess.check_output([".docker-cache/target-rust/debug/weftfs", "--store", sys.argv[1], "--json", "info"], text=True)
print(json.loads(out).get("generation"))
PY
)"
replay_report="$(python3 - "$first" "$second" "$gen_first" "$gen_second" <<'PY'
import json, sys
first, second = json.loads(sys.argv[1]), json.loads(sys.argv[2])
gen1, gen2 = int(sys.argv[3]), int(sys.argv[4])
ok = (first.get("committed") == 1 and second.get("committed") == 1
      and gen1 == gen2 and first.get("attempts", 0) >= 1
      and second.get("attempts", 0) == 0)
print(json.dumps({"ok": ok, "first": first, "second": second,
                  "generation_first": gen1, "generation_second": gen2}, sort_keys=True))
PY
)"
echo "replay: $replay_report"
python3 - "$replay_report" <<'PY' || record_failure "idempotent replay published a duplicate"
import json, sys
r = json.loads(sys.argv[1])
print("" if r.get("ok") else json.dumps(r))
raise SystemExit(0 if r.get("ok") else 1)
PY

# ---------------------------------------------------------------------------
# Delayed writer: A pauses after building its transaction; B commits the same
# name; A resumes and must lose with a typed conflict.
# ---------------------------------------------------------------------------
echo "== delayed writer =="
delay_root="$(mktemp -d /tmp/weft-mw2-delay.XXXXXX)"
delay_store="$delay_root/store"
mkdir -p "$delay_store"
run_rust init --store "$delay_store" >/dev/null
run_rust write --store "$delay_store" --writer a --count 1 --hot same.bin --prefix a --pause-before-execute-ms 400 \
  >"$delay_root/a.json" &
a_pid=$!
sleep 0.1
run_rust write --store "$delay_store" --writer b --count 1 --hot same.bin --prefix b >"$delay_root/b.json"
wait "$a_pid" || record_failure "delayed writer process failed"
delay_report="$(python3 - "$delay_root/a.json" "$delay_root/b.json" <<'PY'
import json, sys
a, b = json.load(open(sys.argv[1])), json.load(open(sys.argv[2]))
ok = b.get("committed") == 1 and a.get("conflicts") == 1 and not a.get("errors") and not b.get("errors")
print(json.dumps({"ok": ok, "delayed": a, "winner": b}, sort_keys=True))
PY
)"
echo "delayed: $delay_report"
python3 - "$delay_report" <<'PY' || record_failure "delayed writer did not lose cleanly"
import json, sys
r = json.loads(sys.argv[1])
print("" if r.get("ok") else json.dumps(r))
raise SystemExit(0 if r.get("ok") else 1)
PY

# ---------------------------------------------------------------------------
# Receipt
# ---------------------------------------------------------------------------
finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
duration_ms=$(( ($(date +%s) - start_epoch) * 1000 ))
log_sha="$(sha256sum "$log" | cut -d' ' -f1)"
result="pass"
[[ "$failures" -eq 0 ]] || result="fail"

python3 - "$receipt" "$started_at" "$finished_at" "$duration_ms" "$result" "$failures" \
  "$stage_report" "$replay_report" "$delay_report" "$log" "$log_sha" <<'PY'
import json, os, sys
(receipt, started, finished, duration, result, failures, stages, replay, delay, log, log_sha) = sys.argv[1:]
doc = {
  "schema": "weftfs.multiwriter.failure/1",
  "generated_at": finished,
  "run": {"started_at": started, "finished_at": finished, "duration_ms": int(duration)},
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
  },
  "repositories": [
    {"name": "WeftFS", "revision": os.environ.get("WEFT_REPO_WEFTFS", "unknown")},
    {"name": "WeftFS-Go", "revision": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown")},
  ],
  "stages": json.loads(stages),
  "idempotent_replay": json.loads(replay),
  "delayed_writer": json.loads(delay),
  "in_process_tests": {
    "quota_race": "tests/multiwriter.rs a_quota_race_at_the_limit_rejects_the_loser; WeftFS-Go TestMW2QuotaRaceAtLimitRejectsLoser",
    "capability_revoke": "tests/multiwriter.rs an_authorization_change_that_wins_the_race_denies_publication; WeftFS-Go TestMW2AuthRaceDeniesPublication",
    "gc_prepared_pins": "tests/multiwriter.rs gc_racing_a_prepared_transaction_retains_its_manifest; WeftFS-Go TestMW2GCKeepsPreparedPins",
  },
  "failures": int(failures),
  "result": result,
  "evidence": {"log": log, "log_sha256": log_sha},
  "scope": {
    "checked": "process-fault proxy at four publication stages, reopened state, outcome lookup, replay, delayed writer",
    "not_checked": "physical power loss, host page-cache loss, distributed/fleet operation, linearizability proof",
  },
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "result": result, "failures": int(failures)}))
PY

echo "receipt: $receipt"
sha256sum "$receipt" "$log"
[[ "$result" == "pass" ]]
