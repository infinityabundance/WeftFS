#!/usr/bin/env bash
# T026 court: independent transactional history checker.
#
# Host:  bash scripts/history_check_court.sh
# Court: bash scripts/docker-court.sh fuse "bash scripts/history_check_court.sh"
#
# Produces real histories from MW2 (disjoint, conflicting, multi-key reads,
# process faults) and MW3 (two conflict domains), checks each with the
# independent stdlib checker (`verifier/history_check.py`), and proves the
# checker fails deliberately violating histories with the expected class.
#
# The store emits the history as a test/audit artifact (`WEFT_HISTORY`). The
# checker imports no WeftFS code, calls no WeftFS binary, and reads no store
# state: it consumes JSON Lines only. One host, one local POSIX namespace.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  exec "$root/scripts/docker-court.sh" fuse "bash scripts/history_check_court.sh"
fi
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="bench/receipts/history-check-${stamp}.json"
log="bench/receipts/history-check-${stamp}.log"
work="$(mktemp -d /tmp/weft-history.XXXXXX)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT
mkdir -p bench/receipts
hist_dir="$work/histories"
viol_dir="$work/violations"
ctrl_dir="$work/controls"
mkdir -p "$hist_dir" "$viol_dir" "$ctrl_dir"
: >"$log"
exec > >(tee -a "$log") 2>&1

NOW_NS=1700000000000000000
RUST_BIN=".docker-cache/target-rust/debug/examples/mw2_writer"
CHECK="verifier/history_check.py"
failures=0
record_failure() {
  failures=$((failures + 1))
  echo "FAIL: $1" >&2
}
run_rust() { "$RUST_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }

h1_history="bench/receipts/history-check-${stamp}-h1.jsonl"
h2_history="bench/receipts/history-check-${stamp}-h2.jsonl"
h3_history="bench/receipts/history-check-${stamp}-h3.jsonl"
h4_history="bench/receipts/history-check-${stamp}-h4.jsonl"

echo "== T026 independent history checker court =="
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"

cargo build --quiet --example mw2_writer
sha256sum "$RUST_BIN" "$CHECK"

add_history() {
  local label="$1" path="$2" detail="$3"
  local report="$work/report-$label.json"
  local ok=false
  if python3 "$CHECK" "$path" --expect accept --out "$report" >"$work/check-$label.out" 2>&1; then
    ok=true
  else
    record_failure "$label: the checker rejected a real store history"
    tail -20 "$work/check-$label.out" >&2 || true
  fi
  echo "$label: accepted=$ok"
  python3 - "$label" "$path" "$report" "$detail" "$ok" "$hist_dir/$label.json" <<'PY'
import json,sys
label,path,report,detail,ok,dest=sys.argv[1:7]
data=json.load(open(report))
row={"label":label,"history":path,"detail":detail,"ok":ok=="true",
     "records":data["records"],"transactions":data["transactions"],
     "committed":data["committed"],"rejected":data["rejected"],
     "reads":data["reads"],"keys":data["keys"],"domains":data["domains"],
     "multi_key_generations":data["multi_key_generations"],
     "violations":data["violations"]}
json.dump(row,open(dest,"w"),indent=2,sort_keys=True)
print(json.dumps(row,sort_keys=True))
PY
}

# ---------------------------------------------------------------------------
# H1: MW2 disjoint writers, then a multi-key read from one handle.
# ---------------------------------------------------------------------------
echo "== H1 MW2 disjoint =="
h1="$work/h1"; mkdir -p "$h1"
run_rust init --store "$h1/store" >/dev/null
pids=()
for writer in w0 w1 w2 w3; do
  WEFT_HISTORY="$h1_history" run_rust write --store "$h1/store" --writer "$writer" \
    --prefix "$writer" --count 5 >"$h1/$writer.json" &
  pids+=("$!")
done
for pid in "${pids[@]}"; do wait "$pid" || record_failure "H1: a writer failed"; done
WEFT_HISTORY="$h1_history" run_rust read-multi --store "$h1/store" \
  --names w0-000000,w1-000000,w0-000001 >"$h1/read.json"
add_history mw2_disjoint "$h1_history" "four disjoint writers, 20 commits, one three-key read"

# ---------------------------------------------------------------------------
# H2: MW2 conflicting writers on one name (typed conflicts must be justified).
# ---------------------------------------------------------------------------
echo "== H2 MW2 conflict =="
h2="$work/h2"; mkdir -p "$h2"
run_rust init --store "$h2/store" >/dev/null
pids=()
for writer in c0 c1 c2 c3; do
  WEFT_HISTORY="$h2_history" run_rust write --store "$h2/store" --writer "$writer" \
    --count 4 --hot hot.bin >"$h2/$writer.json" &
  pids+=("$!")
done
for pid in "${pids[@]}"; do wait "$pid" || record_failure "H2: a writer failed"; done
WEFT_HISTORY="$h2_history" run_rust read-multi --store "$h2/store" --names hot.bin >"$h2/read.json"
add_history mw2_conflict "$h2_history" "four writers on one name; one winner, justified typed conflicts"

# ---------------------------------------------------------------------------
# H3: MW2 with process kills and retries.
# ---------------------------------------------------------------------------
echo "== H3 MW2 process faults =="
h3="$work/h3"; mkdir -p "$h3"
run_rust init --store "$h3/store" >/dev/null
for stage in mw2_prepared mw2_after_head; do
  tx_id="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(("h3:"+sys.argv[1]).encode()).hexdigest()[:32])' "$stage")"
  rc=0
  WEFT_HISTORY="$h3_history" WEFT_CRASH_PHASE="$stage" run_rust write --store "$h3/store" \
    --writer k --count 1 --tx-id "$tx_id" >"$h3/crash-$stage.json" 2>"$h3/crash-$stage.err" || rc=$?
  [[ "$rc" -ne 0 ]] || record_failure "H3: $stage did not kill the writer"
  WEFT_HISTORY="$h3_history" run_rust write --store "$h3/store" --writer k --count 1 \
    --tx-id "$tx_id" >"$h3/retry-$stage.json" 2>"$h3/retry-$stage.err" || record_failure "H3: $stage retry failed"
done
add_history mw2_faults "$h3_history" "two kills (prepared, after head) each followed by an idempotent retry"

# ---------------------------------------------------------------------------
# H4: MW3, two conflict domains, per-domain reads.
# ---------------------------------------------------------------------------
echo "== H4 MW3 domains =="
h4="$work/h4"; mkdir -p "$h4"
run_rust init --store "$h4/store" >/dev/null
domain_a="$(run_rust register --store "$h4/store" --name family-a --kind checkpoint_family --plain)"
domain_b="$(run_rust register --store "$h4/store" --name family-b --kind checkpoint_family --plain)"
pids=()
for spec in "da:$domain_a" "db:$domain_b" "dc:$domain_a"; do
  writer="${spec%%:*}"
  domain="${spec##*:}"
  WEFT_HISTORY="$h4_history" run_rust write --store "$h4/store" --domain "$domain" \
    --writer "$writer" --prefix "$writer" --count 4 >"$h4/$writer.json" &
  pids+=("$!")
done
for pid in "${pids[@]}"; do wait "$pid" || record_failure "H4: a domain writer failed"; done
WEFT_HISTORY="$h4_history" run_rust read-multi --store "$h4/store" --domain "$domain_a" \
  --names da-000000,dc-000000 >"$h4/read-a.json"
WEFT_HISTORY="$h4_history" run_rust read-multi --store "$h4/store" --domain "$domain_b" \
  --names db-000000,db-000001 >"$h4/read-b.json"
add_history mw3_domains "$h4_history" "two MW3 domains, per-domain reads; cross-domain disagreement is expected"

# ---------------------------------------------------------------------------
# Violation detection: every fixture must be rejected with its declared class.
# ---------------------------------------------------------------------------
echo "== violation detection =="
for fixture in tests/history/violating/*.jsonl; do
  name="$(basename "$fixture" .jsonl)"
  case "$name" in
    duplicate_generation) expect_class="generation_conflict" ;;
    partial_transaction) expect_class="stale_read" ;;
    phantom_value) expect_class="phantom_value" ;;
    read_before_any_write) expect_class="read_before_any_write" ;;
    stale_read) expect_class="stale_read" ;;
    unexplained_conflict) expect_class="unexplained_conflict" ;;
    *) expect_class="" ;;
  esac
  out="$work/violation-$name.json"
  ok=false
  if python3 "$CHECK" "$fixture" --expect reject --expect-class "$expect_class" --out "$out" \
      >"$work/violation-$name.out" 2>&1; then
    ok=true
  else
    record_failure "violation fixture $name was not rejected with $expect_class"
  fi
  kinds="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(",".join(d["violation_kinds"]))' "$out" 2>/dev/null || echo "?")"
  echo "violation $name: rejected=$ok kinds=$kinds expected=$expect_class"
  python3 - "$name" "$expect_class" "$kinds" "$ok" "$viol_dir/$name.json" <<'PY'
import json,sys
name,expected,kinds,ok,dest=sys.argv[1:6]
json.dump({"fixture":name,"expected_class":expected,
 "detected_classes":kinds.split(",") if kinds and kinds!="?" else [],
 "ok":ok=="true"},open(dest,"w"),indent=2,sort_keys=True)
PY
done

# ---------------------------------------------------------------------------
# False-positive control: a correct history the checker must accept.
# ---------------------------------------------------------------------------
echo "== false-positive control =="
for fixture in tests/history/valid/*.jsonl; do
  name="$(basename "$fixture" .jsonl)"
  ok=false
  if python3 "$CHECK" "$fixture" --expect accept --out "$work/valid-$name.json" \
      >"$work/valid-$name.out" 2>&1; then
    ok=true
  else
    record_failure "valid fixture $name was rejected"
  fi
  echo "valid $name: accepted=$ok"
  python3 - "$name" "$ok" "$ctrl_dir/$name.json" <<'PY'
import json,sys
json.dump({"fixture":sys.argv[1],"accepted":sys.argv[2]=="true"},open(sys.argv[3],"w"),indent=2,sort_keys=True)
PY
done

# ---------------------------------------------------------------------------
# Coverage assertions: the histories must exercise what they claim.
# ---------------------------------------------------------------------------
coverage="$(python3 - "$hist_dir" <<'PY'
import glob,json,sys
rows=[json.load(open(path)) for path in sorted(glob.glob(sys.argv[1]+"/*.json"))]
by={row["label"]:row for row in rows}
checks={
 "every_history_accepted": all(row["ok"] for row in rows),
 "conflicts_exercised": by.get("mw2_conflict",{}).get("rejected",0)>=1,
 "multi_key_read_exercised": any(row["multi_key_generations"]>=1 for row in rows),
 "mw3_two_domains": by.get("mw3_domains",{}).get("domains",0)>=2,
 "faults_committed_after_retry": by.get("mw2_faults",{}).get("committed",0)>=1,
 "records": sum(row["records"] for row in rows),
 "transactions": sum(row["transactions"] for row in rows),
 "reads": sum(row["reads"] for row in rows),
}
print(json.dumps(checks,sort_keys=True))
PY
)"
echo "coverage: $coverage"
python3 - "$coverage" <<'PY' || record_failure "coverage assertion failed"
import json,sys
c=json.loads(sys.argv[1])
ok=all(v for k,v in c.items() if isinstance(v,bool))
print("" if ok else json.dumps(c))
raise SystemExit(0 if ok else 1)
PY

# ---------------------------------------------------------------------------
# Receipt
# ---------------------------------------------------------------------------
result="pass"
[[ "$failures" -eq 0 ]] || result="fail"
python3 - "$receipt" "$stamp" "$result" "$failures" "$hist_dir" "$viol_dir" "$ctrl_dir" \
  "$coverage" "$log" <<'PY'
import glob,json,os,sys
(receipt,stamp,result,failures,hist_dir,viol_dir,ctrl_dir,coverage,log)=sys.argv[1:10]
def rows(path): return [json.load(open(item)) for item in sorted(glob.glob(path+"/*.json"))]
doc={
 "schema":"weftfs.history_check/1",
 "role":"T026 independent transactional history checker",
 "generated_at":stamp,
 "environment":{
   "court":os.environ.get("WEFT_COURT_IMAGE","unknown"),
   "kernel":os.uname().release,
   "limit_memory":os.environ.get("WEFT_COURT_MEMORY","?"),
   "limit_cpus":os.environ.get("WEFT_COURT_CPUS","?"),
   "limit_pids":os.environ.get("WEFT_COURT_PIDS","?"),
 },
 "repositories":[{"name":"WeftFS","revision":os.environ.get("WEFT_REPO_WEFTFS","unknown")}],
 "history_schema":"weft.history.v1 (JSON Lines, append-only; emitted only when WEFT_HISTORY is set)",
 "property_checked":{
   "per_key":"register linearizability against the store's publication order: a read at generation g returns the latest write with generation <= g",
   "multi_key":"per-domain prefix consistency: reads at generation g see exactly the commits up to g, never half a transaction",
   "conflicts":"every typed conflict is justified by a write in (base_generation, observed_generation] or by a create finding the key taken at the base generation",
   "handles":"one handle's observed generation never decreases within a domain",
   "not_checked":["global serializability across MW3 domains","cross-domain snapshot isolation","a single global linearization point"],
 },
 "histories":rows(hist_dir),
 "violation_detection":rows(viol_dir),
 "false_positive_controls":rows(ctrl_dir),
 "coverage":json.loads(coverage),
 "failures":int(failures),
 "result":result,
 "evidence":{"log":log},
 "scope":{
   "label":"one host, one local POSIX namespace; synthetic workloads generated by the shipped driver",
   "checked":"MW2 disjoint and conflicting writers, multi-key reads, process kills with idempotent retries, MW3 two-domain histories, six deliberately violating fixtures, two accepted controls",
   "not_checked":"cross-domain serializability (not a promised property), fault injection through the S3 proxy (see the T025 receipt), Go parity",
 },
}
json.dump(doc,open(receipt,"w"),indent=2,sort_keys=True)
open(receipt,"a").write("\n")
print(json.dumps({"receipt":receipt,"result":result,"failures":int(failures)}))
PY

echo "receipt: $receipt"
[[ "$result" == "pass" ]]
