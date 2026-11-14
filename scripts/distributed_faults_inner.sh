#!/usr/bin/env bash
# T025 inner court: runs *inside* the fuse court with a real MinIO upstream.
#
# Independent client **processes** talk to the store through a userspace
# fault-injecting proxy (`scripts/fault_proxy.py`). Verdicts are machine-checked
# from the resulting store through a *direct* endpoint that bypasses the proxy.
#
# This is one host, one backend instance, and proxy-level faults. It is not a
# partition of physical nodes, not a crash of the storage server, and not a
# media or power-loss test.
set -Eeuo pipefail
cd /work

stamp="${WEFT_DIST_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/distributed-faults-${stamp}.json"
log="bench/receipts/distributed-faults-${stamp}.log"
work="$(mktemp -d /tmp/weft-faults.XXXXXX)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT
: >"$log"
exec > >(tee -a "$log") 2>&1

NOW_NS=1700000000000000000
RUST_BIN=".docker-cache/target-rust/debug/examples/mw2_writer"
DIRECT="${WEFT_S3_TEST_ENDPOINT:?}"
export WEFT_S3_REGION="${WEFT_S3_REGION:-us-east-1}"
export WEFT_S3_ACCESS_KEY="${WEFT_S3_ACCESS_KEY:-weftcourt}"
export WEFT_S3_SECRET_KEY="${WEFT_S3_SECRET_KEY:-weftcourt-secret}"
export WEFT_S3_LOCK_LEASE_SECS="${WEFT_S3_LOCK_LEASE_SECS:-60}"

failures=0
scenario_rows=()
record_failure() {
  failures=$((failures + 1))
  echo "FAIL: $1" >&2
}
sha256_of() { sha256sum "$1" | cut -d' ' -f1; }

echo "== T025 distributed write/read failure court =="
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown}"
echo "limit: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"
echo "direct endpoint: $DIRECT"
echo "label: one host, one backend instance, proxy-level faults, not a node partition"

cargo build --quiet --example mw2_writer
rust_sha="$(sha256_of "$RUST_BIN")"
proxy_sha="$(sha256_of scripts/fault_proxy.py)"

PORT=""
PROXY_PID=""
PROXY_LOG=""
PROXY_SUMMARY=""
PROXY_READY=""
PROXY_SCHEDULE=""

free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

start_proxy() {
  local label="$1" schedule_json="$2"
  PROXY_SCHEDULE="bench/receipts/distributed-faults-${stamp}-${label}.schedule.json"
  PROXY_LOG="bench/receipts/distributed-faults-${stamp}-${label}.proxy.jsonl"
  PROXY_SUMMARY="bench/receipts/distributed-faults-${stamp}-${label}.proxy.summary.json"
  PROXY_READY="$work/proxy-$label.ready"
  printf '%s\n' "$schedule_json" >"$PROXY_SCHEDULE"
  PORT="$(free_port)"
  python3 scripts/fault_proxy.py \
    --listen "127.0.0.1:${PORT}" --upstream "$DIRECT" \
    --schedule "$PROXY_SCHEDULE" --log "$PROXY_LOG" \
    --summary "$PROXY_SUMMARY" --ready "$PROXY_READY" \
    >"$work/proxy-$label.out" 2>&1 &
  PROXY_PID=$!
  for _ in $(seq 1 200); do
    [[ -f "$PROXY_READY" ]] && return 0
    sleep 0.01
  done
  record_failure "$label: proxy never became ready"
  return 1
}

stop_proxy() {
  if [[ -n "$PROXY_PID" ]]; then
    kill -TERM "$PROXY_PID" 2>/dev/null || true
    for _ in $(seq 1 100); do
      kill -0 "$PROXY_PID" 2>/dev/null || break
      sleep 0.01
    done
    kill -KILL "$PROXY_PID" 2>/dev/null || true
    wait "$PROXY_PID" 2>/dev/null || true
  fi
  PROXY_PID=""
}

proxy_url() { echo "http://127.0.0.1:${PORT}"; }

run_direct() { WEFT_S3_ENDPOINT="$DIRECT" "$RUST_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }
run_proxied() { WEFT_S3_ENDPOINT="$(proxy_url)" "$RUST_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }
new_bucket() { export WEFT_S3_BUCKET="$1"; }

field() { python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get(sys.argv[2]))' "$1" "$2"; }

echo "== build =="
echo "driver sha256=$rust_sha proxy sha256=$proxy_sha"

# ---------------------------------------------------------------------------
# S1: the head conditional PUT is applied and its acknowledgement is dropped.
# ---------------------------------------------------------------------------
echo "== S1 head acknowledged-then-lost =="
s1="$work/s1"; mkdir -p "$s1"
new_bucket "weft-dist-$(echo "$stamp" | tr '[:upper:]' '[:lower:]')-s1"
run_direct init --store "$s1/store" --backend s3 >/dev/null
s1_tx="10000000000000000000000000000001"
start_proxy s1 '{"label":"s1","rules":[{"method":"PUT","path_regex":"/superblock$","occurrence":1,"action":"after_apply_drop"}]}'
W1="$(run_proxied write --store "$s1/store" --backend s3 --writer s1 --count 1 --tx-id "$s1_tx" 2>"$s1/writer.err" || true)"
s1_gen_after_fault="$(run_direct head --store "$s1/store" --backend s3 | python3 -c 'import json,sys; print(json.load(sys.stdin)["generation"])')"
stop_proxy
s1_outcome_before="$(run_direct outcome --store "$s1/store" --backend s3 --tx-id "$s1_tx")"
s1_state_before="$(field "$s1_outcome_before" state)"
s1_retry="$(run_direct write --store "$s1/store" --backend s3 --writer s1 --count 1 --tx-id "$s1_tx" 2>"$s1/retry.err" || true)"
s1_gen_after_retry="$(run_direct head --store "$s1/store" --backend s3 | python3 -c 'import json,sys; print(json.load(sys.stdin)["generation"])')"
s1_verify="$(run_direct verify-full --store "$s1/store" --backend s3)"
s1_exact="$(run_direct verify --store "$s1/store" --backend s3 --writer s1 --count 1)"
s1_ok="$(python3 - "$W1" "$s1_state_before" "$s1_retry" "$s1_exact" "$s1_verify" "$s1_gen_after_fault" "$s1_gen_after_retry" <<'PY'
import json,sys
writer=json.loads(sys.argv[1]) if sys.argv[1].strip() else {}
before=sys.argv[2]; retry=json.loads(sys.argv[3]); exact=json.loads(sys.argv[4]); verify=json.loads(sys.argv[5])
gen_fault=int(sys.argv[6]); gen_retry=int(sys.argv[7])
ambiguous = writer.get("committed") == 0 and bool(writer.get("errors"))
resolved = retry.get("committed") == 1 and int(retry.get("attempts") or 0) == 0
no_second_commit = gen_fault == gen_retry
ok = ambiguous and before in ("unknown", "prepared", "committed") and resolved and no_second_commit and exact.get("ok") and verify.get("ok")
print(json.dumps({"ok":ok,"writer_ambiguous":ambiguous,"outcome_before_retry":before,
 "retry_idempotent":resolved,"generation_after_fault":gen_fault,
 "generation_after_retry":gen_retry,"no_second_commit":no_second_commit,
 "file_exact":bool(exact.get("ok")),"verify_full":bool(verify.get("ok")),
 "writer":writer,"retry":retry},sort_keys=True))
PY
)"
s1_ok_bool="$(field "$s1_ok" ok)"
[[ "$s1_ok_bool" == "True" || "$s1_ok_bool" == "true" ]] || record_failure "S1: ambiguous success was not resolved idempotently"
echo "S1: $s1_ok"
scenario_rows+=("$(python3 - "$s1_ok" "$s1" "$PROXY_SCHEDULE" <<'PY'
import json,sys
row=json.loads(sys.argv[1])
row.update({"scenario":"s1_head_ack_dropped","fault_schedule":json.load(open(sys.argv[3]))})
print(json.dumps(row,sort_keys=True))
PY
)")

# ---------------------------------------------------------------------------
# S2: a duplicated blob PUT (at-least-once delivery) is idempotent.
# ---------------------------------------------------------------------------
echo "== S2 duplicated blob PUT =="
s2="$work/s2"; mkdir -p "$s2"
new_bucket "weft-dist-$(echo "$stamp" | tr '[:upper:]' '[:lower:]')-s2"
run_direct init --store "$s2/store" --backend s3 >/dev/null
s2_objects_before="$(run_direct head --store "$s2/store" --backend s3 | python3 -c 'import json,sys; print(json.load(sys.stdin)["objects"])')"
start_proxy s2 '{"label":"s2","rules":[{"method":"PUT","path_regex":"/objects/","occurrence":1,"action":"duplicate"}]}'
W2="$(run_proxied write --store "$s2/store" --backend s3 --writer s2 --count 1 --tx-id 20000000000000000000000000000002 2>"$s2/writer.err" || true)"
stop_proxy
s2_objects_after="$(run_direct head --store "$s2/store" --backend s3 | python3 -c 'import json,sys; print(json.load(sys.stdin)["objects"])')"
s2_exact="$(run_direct verify --store "$s2/store" --backend s3 --writer s2 --count 1)"
s2_verify="$(run_direct verify-full --store "$s2/store" --backend s3)"
s2_ok="$(python3 - "$W2" "$s2_exact" "$s2_verify" "$s2_objects_before" "$s2_objects_after" <<'PY'
import json,sys
writer=json.loads(sys.argv[1]); exact=json.loads(sys.argv[2]); verify=json.loads(sys.argv[3])
before=int(sys.argv[4]); after=int(sys.argv[5])
ok = writer.get("committed")==1 and not writer.get("errors") and exact.get("ok") and verify.get("ok") and after-before==1
print(json.dumps({"ok":ok,"committed":writer.get("committed"),"errors":writer.get("errors"),
 "objects_before":before,"objects_after":after,"file_exact":bool(exact.get("ok")),
 "verify_full":bool(verify.get("ok"))},sort_keys=True))
PY
)"
[[ "$(field "$s2_ok" ok)" == "True" ]] || record_failure "S2: a duplicated PUT was not idempotent"
echo "S2: $s2_ok"
scenario_rows+=("$(python3 - "$s2_ok" "$PROXY_SCHEDULE" <<'PY'
import json,sys
row=json.loads(sys.argv[1]); row.update({"scenario":"s2_duplicate_put","fault_schedule":json.load(open(sys.argv[2]))})
print(json.dumps(row,sort_keys=True))
PY
)")

# ---------------------------------------------------------------------------
# S3: reordered concurrent head publications; exactly one winner.
# ---------------------------------------------------------------------------
echo "== S3 reordered concurrent writers =="
s3="$work/s3"; mkdir -p "$s3"
new_bucket "weft-dist-$(echo "$stamp" | tr '[:upper:]' '[:lower:]')-s3"
run_direct init --store "$s3/store" --backend s3 >/dev/null
start_proxy s3 '{"label":"s3","rules":[{"method":"PUT","path_regex":"/superblock$","occurrence":2,"action":"delay_request:400"}]}'
(
  run_proxied write --store "$s3/store" --backend s3 --writer a --count 1 --hot hot.bin --tx-id 30000000000000000000000000000003 >"$s3/a.json" 2>"$s3/a.err"
) &
a_pid=$!
(
  run_proxied write --store "$s3/store" --backend s3 --writer b --count 1 --hot hot.bin --tx-id 30000000000000000000000000000004 >"$s3/b.json" 2>"$s3/b.err"
) &
b_pid=$!
wait "$a_pid" || true
wait "$b_pid" || true
stop_proxy
s3_read="$(run_direct read --store "$s3/store" --backend s3 --name hot.bin)"
s3_verify="$(run_direct verify-full --store "$s3/store" --backend s3)"
s3_ok="$(python3 - "$s3/a.json" "$s3/b.json" "$s3_read" "$s3_verify" <<'PY'
import json,sys
def load(p):
    try:
        return json.load(open(p))
    except Exception:
        return {"committed":0,"conflicts":0,"errors":["no output"]}
a,b=load(sys.argv[1]),load(sys.argv[2]); r=json.loads(sys.argv[3]); v=json.loads(sys.argv[4])
committed=a.get("committed",0)+b.get("committed",0)
losers=a.get("conflicts",0)+b.get("conflicts",0)+a.get("aborted",0)+b.get("aborted",0)
# Exactly one create of one name can win; the other must be a typed conflict or
# an unresolved Busy, never a silent overwrite.
ok = committed==1 and losers>=1 and r.get("ok") and v.get("ok")
print(json.dumps({"ok":ok,"committed":committed,"typed_losers":losers,
 "winner_content_len":r.get("len"),"a":a,"b":b,"verify_full":bool(v.get("ok"))},sort_keys=True))
PY
)"
[[ "$(field "$s3_ok" ok)" == "True" ]] || record_failure "S3: reordered writers did not resolve to one winner"
echo "S3: $s3_ok"
scenario_rows+=("$(python3 - "$s3_ok" "$PROXY_SCHEDULE" <<'PY'
import json,sys
row=json.loads(sys.argv[1]); row.update({"scenario":"s3_reordered_concurrent","fault_schedule":json.load(open(sys.argv[2]))})
print(json.dumps(row,sort_keys=True))
PY
)")

# ---------------------------------------------------------------------------
# S4: a client killed between prepare and commit, mid-flight through the proxy.
# ---------------------------------------------------------------------------
echo "== S4 client killed between prepare and commit =="
s4="$work/s4"; mkdir -p "$s4"
new_bucket "weft-dist-$(echo "$stamp" | tr '[:upper:]' '[:lower:]')-s4"
run_direct init --store "$s4/store" --backend s3 >/dev/null
start_proxy s4 '{"label":"s4","rules":[{"method":"PUT","path_regex":"/superblock$","occurrence":1,"action":"delay_request:300"}]}'
s4_rc=0
WEFT_CRASH_PHASE=mw2_prepared run_proxied write --store "$s4/store" --backend s3 --writer k \
  --count 1 --tx-id 40000000000000000000000000000005 >"$s4/crash.json" 2>"$s4/crash.err" || s4_rc=$?
stop_proxy
[[ "$s4_rc" -ne 0 ]] || record_failure "S4: the writer was not killed"
s4_outcome="$(run_direct outcome --store "$s4/store" --backend s3 --tx-id 40000000000000000000000000000005)"
s4_state="$(field "$s4_outcome" state)"
s4_retry="$(run_direct write --store "$s4/store" --backend s3 --writer k --count 1 --tx-id 40000000000000000000000000000005 2>"$s4/retry.err" || true)"
s4_exact="$(run_direct verify --store "$s4/store" --backend s3 --writer k --count 1)"
s4_verify="$(run_direct verify-full --store "$s4/store" --backend s3)"
s4_ok="$(python3 - "$s4_state" "$s4_retry" "$s4_exact" "$s4_verify" "$s4_rc" <<'PY'
import json,sys
state=sys.argv[1]; retry=json.loads(sys.argv[2]); exact=json.loads(sys.argv[3]); verify=json.loads(sys.argv[4])
ok = int(sys.argv[5])!=0 and state in ("unknown","prepared","absent") and retry.get("committed")==1 \
     and exact.get("ok") and verify.get("ok")
print(json.dumps({"ok":ok,"killed":int(sys.argv[5])!=0,"outcome_before_retry":state,
 "retry_committed":retry.get("committed"),"file_exact":bool(exact.get("ok")),
 "verify_full":bool(verify.get("ok"))},sort_keys=True))
PY
)"
[[ "$(field "$s4_ok" ok)" == "True" ]] || record_failure "S4: a killed writer was not resolved cleanly"
echo "S4: $s4_ok"
scenario_rows+=("$(python3 - "$s4_ok" "$PROXY_SCHEDULE" <<'PY'
import json,sys
row=json.loads(sys.argv[1]); row.update({"scenario":"s4_kill_between_prepare_and_commit","fault_schedule":json.load(open(sys.argv[2]))})
print(json.dumps(row,sort_keys=True))
PY
)")

# ---------------------------------------------------------------------------
# S5: a stale reader keeps its captured snapshot across a newer head.
# ---------------------------------------------------------------------------
echo "== S5 stale reader =="
s5="$work/s5"; mkdir -p "$s5"
new_bucket "weft-dist-$(echo "$stamp" | tr '[:upper:]' '[:lower:]')-s5"
run_direct init --store "$s5/store" --backend s3 >/dev/null
run_direct write --store "$s5/store" --backend s3 --writer v --count 1 --hot stale.bin \
  --tx-id 50000000000000000000000000000006 >/dev/null
start_proxy s5 '{"label":"s5","rules":[]}'
(
  run_proxied hold-read --store "$s5/store" --backend s3 --name stale.bin \
    --ready "$s5/ready" --gate "$s5/gate" >"$s5/reader.json" 2>"$s5/reader.err"
) &
reader_pid=$!
for _ in $(seq 1 600); do [[ -f "$s5/ready" ]] && break; sleep 0.01; done
run_proxied overwrite --store "$s5/store" --backend s3 --name stale.bin \
  --content "w:0" --seed "w:0:seed" --tx-id 50000000000000000000000000000007 >/dev/null
touch "$s5/gate"
wait "$reader_pid" || record_failure "S5: the stale reader failed"
stop_proxy
s5_new="$(run_direct read --store "$s5/store" --backend s3 --name stale.bin)"
s5_verify="$(run_direct verify-full --store "$s5/store" --backend s3)"
s5_ok="$(python3 - "$s5/reader.json" "$s5/ready" "$s5_new" "$s5_verify" <<'PY'
import json,sys
reader=json.load(open(sys.argv[1])); ready=json.load(open(sys.argv[2])); fresh=json.loads(sys.argv[3]); verify=json.loads(sys.argv[4])
before=reader.get("before",{}); after=reader.get("after",{})
same = before.get("content_hash") and before.get("content_hash")==after.get("content_hash")
newer = fresh.get("content_hash") and fresh.get("content_hash")!=before.get("content_hash")
ok = bool(same) and bool(newer) and reader.get("generation")==ready.get("generation") and verify.get("ok")
print(json.dumps({"ok":ok,"verify_full":bool(verify.get("ok")),"stale_reader_unchanged":bool(same),"new_reader_sees_newer":bool(newer),
 "reader_generation":reader.get("generation"),"ready_generation":ready.get("generation"),
 "before_len":before.get("len"),"after_len":after.get("len"),"fresh_len":fresh.get("len")},sort_keys=True))
PY
)"
[[ "$(field "$s5_ok" ok)" == "True" ]] || record_failure "S5: the stale reader did not keep its snapshot"
echo "S5: $s5_ok"
scenario_rows+=("$(python3 - "$s5_ok" "$PROXY_SCHEDULE" <<'PY'
import json,sys
row=json.loads(sys.argv[1]); row.update({"scenario":"s5_stale_reader","fault_schedule":json.load(open(sys.argv[2]))})
print(json.dumps(row,sort_keys=True))
PY
)")

# ---------------------------------------------------------------------------
# S6: the commit record is applied and its acknowledgement is dropped; the head
#     is never published. The retry must not treat the orphan record as success.
# ---------------------------------------------------------------------------
echo "== S6 commit record acknowledged-then-lost =="
s6="$work/s6"; mkdir -p "$s6"
new_bucket "weft-dist-$(echo "$stamp" | tr '[:upper:]' '[:lower:]')-s6"
run_direct init --store "$s6/store" --backend s3 >/dev/null
start_proxy s6 '{"label":"s6","rules":[{"method":"PUT","path_regex":"/commits/","occurrence":1,"action":"after_apply_drop"}]}'
W6="$(run_proxied write --store "$s6/store" --backend s3 --writer r --count 1 --tx-id 60000000000000000000000000000008 2>"$s6/writer.err" || true)"
stop_proxy
s6_outcome="$(run_direct outcome --store "$s6/store" --backend s3 --tx-id 60000000000000000000000000000008)"
s6_state="$(field "$s6_outcome" state)"
s6_before_retry="$(run_direct head --store "$s6/store" --backend s3)"
s6_retry="$(run_direct write --store "$s6/store" --backend s3 --writer r --count 1 --tx-id 60000000000000000000000000000008 2>"$s6/retry.err" || true)"
s6_exact="$(run_direct verify --store "$s6/store" --backend s3 --writer r --count 1)"
s6_verify="$(run_direct verify-full --store "$s6/store" --backend s3)"
s6_ok="$(python3 - "$W6" "$s6_state" "$s6_retry" "$s6_exact" "$s6_verify" "$s6_before_retry" <<'PY'
import json,sys
writer=json.loads(sys.argv[1]) if sys.argv[1].strip() else {}
state=sys.argv[2]; retry=json.loads(sys.argv[3]); exact=json.loads(sys.argv[4]); verify=json.loads(sys.argv[5])
before=json.loads(sys.argv[6])
# The record was applied; no head names it. The retry must publish a *new*
# generation (the old file is never reused) and the store must stay exact.
ok = writer.get("committed")==0 and bool(writer.get("errors")) and state in ("unknown","prepared","absent") \
     and retry.get("committed")==1 and exact.get("ok") and verify.get("ok")
print(json.dumps({"ok":ok,"outcome_before_retry":state,"writer_ambiguous":writer.get("committed")==0,
 "retry_committed":retry.get("committed"),"generation_before_retry":before.get("generation"),
 "file_exact":bool(exact.get("ok")),"verify_full":bool(verify.get("ok")),"writer":writer},sort_keys=True))
PY
)"
[[ "$(field "$s6_ok" ok)" == "True" ]] || record_failure "S6: a dropped commit-record ack was not resolved cleanly"
echo "S6: $s6_ok"
scenario_rows+=("$(python3 - "$s6_ok" "$PROXY_SCHEDULE" <<'PY'
import json,sys
row=json.loads(sys.argv[1]); row.update({"scenario":"s6_commit_record_ack_dropped","fault_schedule":json.load(open(sys.argv[2]))})
print(json.dumps(row,sort_keys=True))
PY
)")

# ---------------------------------------------------------------------------
# Receipt
# ---------------------------------------------------------------------------
rows="[$(IFS=,; echo "${scenario_rows[*]}")]"
python3 - "$receipt" "$stamp" "$rows" "$failures" "$rust_sha" "$proxy_sha" "$log" "$DIRECT" <<'PY'
import json, os, sys
(receipt, stamp, rows, failures, rust_sha, proxy_sha, log, direct) = sys.argv[1:]
rows = json.loads(rows)
checks = {row["scenario"]: bool(row.get("ok")) for row in rows}
verdicts = {
    "no_acknowledged_lost_update": checks.get("s1_head_ack_dropped") and checks.get("s6_commit_record_ack_dropped"),
    "no_partial_checkpoint": all(row.get("verify_full") for row in rows),
    "no_blind_overwrite": checks.get("s3_reordered_concurrent"),
    "ambiguous_success_resolved": checks.get("s1_head_ack_dropped") and checks.get("s6_commit_record_ack_dropped"),
    "duplicated_request_idempotent": checks.get("s2_duplicate_put"),
    "kill_between_prepare_and_commit_resolved": checks.get("s4_kill_between_prepare_and_commit"),
    "stale_reader_snapshot_isolated": checks.get("s5_stale_reader"),
}
doc = {
  "schema": "weftfs.distributed_faults/1",
  "role": "T025 distributed write/read failure court",
  "generated_at": stamp,
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
    "backend": "MinIO (official release binary, started by the court)",
    "server_label": os.environ.get("WEFT_S3_SERVER_LABEL", "unknown"),
    "server_release": os.environ.get("WEFT_S3_SERVER_RELEASE", "unknown"),
    "server_sha256": os.environ.get("WEFT_S3_SERVER_SHA256", "unknown"),
    "direct_endpoint": direct,
    "clients": "independent mw2_writer processes; one process per writer",
    "fault_surface": "userspace HTTP/1.1 forwarding proxy (scripts/fault_proxy.py)",
  },
  "repositories": [{"name": "WeftFS", "revision": os.environ.get("WEFT_REPO_WEFTFS", "unknown")}],
  "binaries": {"driver_sha256": rust_sha, "proxy_sha256": proxy_sha},
  "fault_model": {
    "actions": ["forward", "delay_request", "delay_response", "drop",
                "after_apply_drop", "after_apply_truncate", "abort_after_apply",
                "duplicate"],
    "ambiguous_success": "after_apply_* forwards the request, waits for the upstream to apply it, and then destroys the response",
    "not_modeled": "node partition, server crash, clock skew, packet loss inside the kernel, multi-host ordering",
  },
  "scenarios": rows,
  "verdicts": verdicts,
  "failures": int(failures),
  "result": "pass" if int(failures) == 0 and all(verdicts.values()) else "fail",
  "evidence": {"log": log},
  "scope": {
    "label": "one host, one backend instance, proxy-level faults; NOT a partition of physical nodes, not a server crash, not media or power loss",
    "checked": "acknowledged-then-lost head and commit-record responses, duplicated PUT, reordered concurrent head publications, a client killed between prepare and commit, a stale reader, and every outcome resolved through the shipped transaction-outcome path",
    "not_checked": "multi-host clusters, consensus, quorum, clock skew, physical network partitions, server-side crashes, Go parity",
  },
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "result": doc["result"], "verdicts": doc["verdicts"]}))
PY

echo "receipt: $receipt"
[[ "$failures" -eq 0 ]]
