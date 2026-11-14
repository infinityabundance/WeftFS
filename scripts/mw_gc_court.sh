#!/usr/bin/env bash
# T024 court: multi-writer GC and concurrent retention.
#
# Host:  bash scripts/mw_gc_court.sh
# Court: bash scripts/docker-court.sh fuse "bash scripts/mw_gc_court.sh"
#
# The receipt is assembled with python3, which the minimal rust court does not
# ship, so the court is the fuse image. No FUSE mount is used by this court.
#
# Scenarios
#   A  reservation window: a writer publishes a blob, a *different* handle runs
#      a GC pass with the age gate disabled, then the writer publishes.
#   B  process kills at every publication stage, each with a concurrent GC loop,
#      then outcome resolution/retry and a full verify.
#   C  vacuity control: genuinely unreachable blobs must be reclaimed.
#   D  MW3: two conflict domains, a coordinated pass, per-domain verification.
#   E  readers concurrent with a sweep: every read is exact or absent, never
#      wrong bytes.
#
# Every hard limit comes from scripts/docker-court.sh; this script adds none and
# disables none. One host, one local POSIX namespace. No fleet, media or
# power-loss claim: the kills are process-fault proxies.
#
# Writes:
#   bench/receipts/mw-gc-<UTC>.json
#   bench/receipts/mw-gc-<UTC>.log
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  skip() { echo "mw_gc_court: skip $1" >&2; exit 78; }
  command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
  docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
  exec "$root/scripts/docker-court.sh" fuse "bash scripts/mw_gc_court.sh"
fi
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
start_epoch="$(date +%s)"
receipt="bench/receipts/mw-gc-${stamp}.json"
log="bench/receipts/mw-gc-${stamp}.log"
mkdir -p bench/receipts
: >"$log"
exec > >(tee -a "$log") 2>&1

NOW_NS=1700000000000000000
RUST_BIN=".docker-cache/target-rust/debug/examples/mw2_writer"
WEFT_BIN=".docker-cache/target-rust/debug/weftfs"
work="$(mktemp -d /tmp/weft-mw-gc.XXXXXX)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT
failures=0
notes=()

record_failure() {
  failures=$((failures + 1))
  notes+=("$1")
  echo "FAIL: $1" >&2
}

# The age gate is disabled for every handle in this court: the epoch cut and the
# reservation floor must do the work, not a wall-clock grace.
export WEFT_MW2_GC_GRACE_SECS=0

run_rust() { "$RUST_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }
json_get() { python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get(sys.argv[2],""))' "$1" "$2"; }

echo "== T024 multi-writer GC court =="
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"
echo "grace_secs: 0 (age gate disabled)"

echo "== build =="
cargo build --quiet --example mw2_writer --bin weftfs
rust_sha="$(sha256sum "$RUST_BIN" | cut -d' ' -f1)"

# ---------------------------------------------------------------------------
# A. The reservation window.
# ---------------------------------------------------------------------------
echo "== A: reservation window =="
a="$work/a"
mkdir -p "$a"
run_rust init --store "$a/store" >/dev/null
run_rust reserve-write --store "$a/store" --writer a --name reserved.bin \
  --content "reserved-content" --seed "seed-reserved" \
  --tx-id 000000000000000000000000000000a1 --ready "$a/ready" --gate "$a/gate" \
  --hold-ms 30000 >"$a/writer.json" 2>"$a/writer.err" &
a_pid=$!
ready=false
for _ in $(seq 1 600); do
  if [[ -f "$a/ready" ]]; then ready=true; break; fi
  sleep 0.01
done
[[ "$ready" == "true" ]] || record_failure "A: writer never published its blob"
run_rust gc --store "$a/store" --apply >"$a/gc.json" 2>"$a/gc.err" || record_failure "A: gc failed"
touch "$a/gate"
if ! wait "$a_pid"; then record_failure "A: reserve-write failed"; fi
a_retained="$(python3 - "$a/gc.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
print(int(r.get("retained_newer_than_cut",0))+int(r.get("retained_reservation_floor",0)))
PY
)"
a_read="$(run_rust read --store "$a/store" --name reserved.bin)"
a_read_ok="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print(str(bool(d.get("ok")) and d.get("len")==16).lower())' "$a_read")"
a_verify=false
if "$WEFT_BIN" --store "$a/store" verify --full >/dev/null 2>"$a/verify.err"; then a_verify=true; else record_failure "A: verify --full failed"; fi
echo "A: retained_by_cut_or_floor=$a_retained read_exact=$a_read_ok verify_full=$a_verify gc=$(cat "$a/gc.json")"
[[ "$a_retained" -ge 1 ]] || record_failure "A: the sweep reclaimed a blob inside a live reservation"
[[ "$a_read_ok" == "true" ]] || record_failure "A: published file did not read back exactly"

# ---------------------------------------------------------------------------
# B. Process kills at every publication stage with a concurrent sweep.
# ---------------------------------------------------------------------------
echo "== B: kill matrix with concurrent GC =="
b="$work/b"
mkdir -p "$b"
run_rust init --store "$b/store" >/dev/null
stages=(mw2_prepared mw2_before_head mw2_after_head mw2_after_ack)
kill_rows=()
crash_total=0
for stage in "${stages[@]}"; do
  for iteration in 1 2; do
    tx_id="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(("b:"+sys.argv[1]+":"+sys.argv[2]).encode()).hexdigest()[:32])' "$stage" "$iteration")"
    # A GC loop runs for the whole kill+retry window.
    run_rust gc-loop --store "$b/store" --count 25 --interval-ms 4 >"$b/gc-$stage-$iteration.json" 2>&1 &
    gc_pid=$!
    rc=0
    WEFT_CRASH_PHASE="$stage" run_rust write --reserve --store "$b/store" --writer k \
      --count 1 --tx-id "$tx_id" >"$b/crash-$stage-$iteration.json" 2>"$b/crash-$stage-$iteration.err" || rc=$?
    [[ "$rc" -eq 0 ]] && record_failure "B: $stage did not kill the writer"
    crash_total=$((crash_total + 1))
    wait "$gc_pid" || record_failure "B: gc-loop failed beside $stage"
    outcome_before="$(run_rust outcome --store "$b/store" --tx-id "$tx_id" 2>/dev/null || true)"
    state_before="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get("state","?"))' "$outcome_before" 2>/dev/null || echo "?")"
    retry="$(run_rust write --reserve --store "$b/store" --writer k --count 1 --tx-id "$tx_id" 2>"$b/retry-$stage-$iteration.err" || true)"
    retry_ok="$(python3 -c 'import json,sys; print(str(json.loads(sys.argv[1]).get("committed")==1).lower())' "$retry" 2>/dev/null || echo false)"
    read_out="$(run_rust read --store "$b/store" --name 'k-000000' 2>/dev/null || true)"
    read_exact="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print(str(bool(d.get("ok")) and d.get("len")==3).lower())' "$read_out" 2>/dev/null || echo false)"
    [[ "$retry_ok" == "true" ]] || record_failure "B: $stage retry did not commit"
    [[ "$read_exact" == "true" ]] || record_failure "B: $stage file is not exact after retry"
    kill_rows+=("$(python3 - "$stage" "$iteration" "$rc" "$state_before" "$retry_ok" "$read_exact" <<'PY'
import json,sys
print(json.dumps({"stage":sys.argv[1],"iteration":int(sys.argv[2]),"exit":int(sys.argv[3]),
 "outcome_before_retry":sys.argv[4],"retry_committed":sys.argv[5]=="true",
 "read_exact":sys.argv[6]=="true"},sort_keys=True))
PY
)")
  done
done
b_verify=false
if "$WEFT_BIN" --store "$b/store" verify --full >/dev/null 2>"$b/verify.err"; then b_verify=true; else record_failure "B: verify --full after the kill matrix failed"; fi
b_names="$(run_rust verify --store "$b/store" --writer k --count 1)"
b_names_ok="$(python3 -c 'import json,sys; print(str(json.loads(sys.argv[1]).get("ok")).lower())' "$b_names")"
echo "B: crashes=$crash_total verify_full=$b_verify names_exact=$b_names_ok"
[[ "$b_names_ok" == "true" ]] || record_failure "B: a surviving name has wrong bytes"

# ---------------------------------------------------------------------------
# C. Vacuity: unreachable blobs must be reclaimed.
# ---------------------------------------------------------------------------
echo "== C: reclaim control =="
c="$work/c"
mkdir -p "$c"
run_rust init --store "$c/store" >/dev/null
run_rust garbage --store "$c/store" --writer g --count 8 >"$c/garbage.json"
# The cut carries a one-second clock slack; the garbage must be older.
sleep 1.2
run_rust gc --store "$c/store" --apply >"$c/gc1.json"
run_rust gc --store "$c/store" --apply >"$c/gc2.json"
c_deleted="$(json_get "$c/gc1.json" deleted)"
c_bytes="$(json_get "$c/gc1.json" deleted_bytes)"
c_second="$(json_get "$c/gc2.json" deleted)"
c_verify=false
if "$WEFT_BIN" --store "$c/store" verify --full >/dev/null 2>"$c/verify.err"; then c_verify=true; else record_failure "C: verify --full failed"; fi
echo "C: first_pass_deleted=$c_deleted bytes=$c_bytes second_pass_deleted=$c_second verify_full=$c_verify"
[[ "$c_deleted" -ge 2 ]] || record_failure "C: the sweep reclaimed nothing (the retention checks would be vacuous)"
[[ "$c_bytes" -ge 1 ]] || record_failure "C: deleted_bytes is zero"

# ---------------------------------------------------------------------------
# D. MW3: one coordinated pass over two conflict domains.
# ---------------------------------------------------------------------------
echo "== D: MW3 coordinated pass =="
d="$work/d"
mkdir -p "$d"
run_rust init --store "$d/store" >/dev/null
domain_a="$(run_rust register --store "$d/store" --name family-a --kind checkpoint_family --plain)"
domain_b="$(run_rust register --store "$d/store" --name family-b --kind checkpoint_family --plain)"
run_rust gc-loop --store "$d/store" --writer-mode sharded --count 20 --interval-ms 5 >"$d/gc-loop.json" 2>&1 &
d_gc_pid=$!
pids=()
for spec in "wa:$domain_a" "wb:$domain_b" "wc:$domain_a" "wd:$domain_b"; do
  writer="${spec%%:*}"
  domain="${spec##*:}"
  run_rust write --reserve --store "$d/store" --domain "$domain" --writer "$writer" \
    --prefix "$writer" --count 6 >"$d/$writer.json" 2>"$d/$writer.err" &
  pids+=("$!")
done
for pid in "${pids[@]}"; do wait "$pid" || record_failure "D: a domain writer failed"; done
wait "$d_gc_pid" || record_failure "D: MW3 gc-loop failed"
d_verify_ok=true
for domain in "$domain_a" "$domain_b"; do
  if ! "$WEFT_BIN" --store "$d/store" --writer-mode sharded --domain "$domain" verify --full \
      >"$d/verify-$domain.out" 2>&1; then
    d_verify_ok=false
    record_failure "D: domain $domain failed verify --full"
  fi
done
for spec in "wa:$domain_a" "wb:$domain_b" "wc:$domain_a" "wd:$domain_b"; do
  writer="${spec%%:*}"
  domain="${spec##*:}"
  run_rust verify --store "$d/store" --domain "$domain" --writer "$writer" --count 6 \
    >"$d/$writer.verify.json" 2>/dev/null || record_failure "D: $writer files are not exact"
done
run_rust gc --store "$d/store" --writer-mode sharded --apply >"$d/gc.json"
d_heads="$(json_get "$d/gc.json" domain_heads_marked)"
echo "D: domain_heads_marked=$d_heads verify_full=$d_verify_ok gc=$(cat "$d/gc.json")"
[[ "$d_heads" -ge 2 ]] || record_failure "D: the coordinated pass marked fewer than two domain heads"
[[ "$d_verify_ok" == "true" ]] || record_failure "D: a domain failed verification"

# ---------------------------------------------------------------------------
# E. Readers concurrent with a sweep.
# ---------------------------------------------------------------------------
echo "== E: readers under sweep =="
e="$work/e"
mkdir -p "$e"
run_rust init --store "$e/store" >/dev/null
run_rust write --store "$e/store" --writer r --prefix r --count 8 >"$e/seed.json"
run_rust gc-loop --store "$e/store" --count 40 --interval-ms 5 >"$e/gc-loop.json" 2>&1 &
e_gc_pid=$!
reader_pids=()
for reader in 1 2 3; do
  run_rust read-loop --store "$e/store" --writer r --count 8 --rounds 25 \
    >"$e/reader-$reader.json" 2>"$e/reader-$reader.err" &
  reader_pids+=("$!")
done
run_rust write --store "$e/store" --reserve --writer x --prefix x --count 20 >"$e/x.json" 2>&1 &
e_writer_pid=$!
for pid in "${reader_pids[@]}"; do wait "$pid" || record_failure "E: a reader failed"; done
wait "$e_writer_pid" || record_failure "E: the concurrent writer failed"
wait "$e_gc_pid" || record_failure "E: gc-loop failed"
e_report="$(python3 - "$e" <<'PY'
import glob,json,os,sys
rows=[json.load(open(p)) for p in sorted(glob.glob(os.path.join(sys.argv[1],"reader-*.json")))]
print(json.dumps({"readers":len(rows),
 "reads":sum(r.get("reads",0) for r in rows),
 "absent":sum(r.get("absent",0) for r in rows),
 "mismatches":[m for r in rows for m in r.get("mismatches",[])],
 "ok":all(r.get("ok") for r in rows)},sort_keys=True))
PY
)"
echo "E: $e_report"
python3 - "$e_report" <<'PY' || record_failure "E: a concurrent reader saw wrong bytes"
import json,sys
r=json.loads(sys.argv[1])
ok=r["ok"] and not r["mismatches"]
print("" if ok else json.dumps(r))
raise SystemExit(0 if ok else 1)
PY
e_verify=false
if "$WEFT_BIN" --store "$e/store" verify --full >/dev/null 2>"$e/verify.err"; then e_verify=true; else record_failure "E: verify --full failed"; fi
run_rust verify --store "$e/store" --writer x --count 20 >"$e/x.verify.json"
e_x_ok="$(python3 -c 'import json,sys; print(str(bool(json.loads(open(sys.argv[1]).read()).get("ok"))).lower())' "$e/x.verify.json")"
if [[ "$e_x_ok" != "true" ]]; then echo "E verify report: $(cat "$e/x.verify.json" | head -c 1200)"; fi
[[ "$e_x_ok" == "true" ]] || record_failure "E: the concurrent writer's files are not exact"

# ---------------------------------------------------------------------------
# Receipt
# ---------------------------------------------------------------------------
finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
duration_ms=$(( ($(date +%s) - start_epoch) * 1000 ))
log_sha="$(sha256sum "$log" | cut -d' ' -f1)"
result="pass"
[[ "$failures" -eq 0 ]] || result="fail"
kill_json="[$(IFS=,; echo "${kill_rows[*]}")]"
notes_joined="$(IFS='|'; echo "${notes[*]:-}")"

python3 - "$receipt" "$started_at" "$finished_at" "$duration_ms" "$result" "$failures" \
  "$rust_sha" "$a_retained" "$a_read_ok" "$a_verify" "$kill_json" "$crash_total" \
  "$b_verify" "$b_names_ok" "$c_deleted" "$c_bytes" "$c_second" "$c_verify" \
  "$d_heads" "$d_verify_ok" "$e_report" "$e_verify" "$e_x_ok" \
  "$notes_joined" "$log" "$log_sha" <<'PY'
import json, os, sys
(receipt, started, finished, duration, result, failures, rust_sha, a_retained,
 a_read, a_verify, kills, crash_total, b_verify, b_names, c_deleted, c_bytes,
 c_second, c_verify, d_heads, d_verify, e_report, e_verify, e_x, notes, log,
 log_sha) = sys.argv[1:]
doc = {
  "schema": "weftfs.mw_gc/1",
  "role": "T024 adversarial court: multi-writer GC and concurrent retention",
  "generated_at": finished,
  "run": {"started_at": started, "finished_at": finished, "duration_ms": int(duration)},
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
    "gc_grace_secs": 0,
  },
  "repositories": [
    {"name": "WeftFS", "revision": os.environ.get("WEFT_REPO_WEFTFS", "unknown")},
  ],
  "binary_sha256": rust_sha,
  "mechanism": {
    "epoch": "one compare-and-swap on gc/epoch; the acquisition instant is the reclaim cut",
    "reservation": "gc/reservations/<digest> written before the first blob, with a deadline",
    "floor": "earliest live reservation start minus one second of timestamp slack",
    "evidence": "bench/receipts/mw-gc-*.json; docs/MULTI-WRITER-GC.md",
  },
  "scenario_a_reservation_window": {
    "retained_by_cut_or_floor": int(a_retained),
    "read_exact": a_read == "true",
    "verify_full": a_verify == "true",
  },
  "scenario_b_kill_matrix": {
    "crashes": int(crash_total),
    "rows": json.loads(kills),
    "verify_full": b_verify == "true",
    "surviving_names_exact": b_names == "true",
    "reachable_objects_lost": 0 if b_names == "true" and b_verify == "true" else None,
  },
  "scenario_c_reclaim_control": {
    "first_pass_deleted_blobs": int(c_deleted),
    "first_pass_deleted_bytes": int(c_bytes),
    "second_pass_deleted_blobs": int(c_second),
    "verify_full": c_verify == "true",
    "note": "proves the retention assertions above are not vacuous",
  },
  "scenario_d_mw3": {
    "domain_heads_marked": int(d_heads),
    "verify_full_each_domain": d_verify == "true",
    "note": "unlinked object records are not removed by a coordinated pass; only blobs unreachable from every domain head are swept",
  },
  "scenario_e_readers": json.loads(e_report),
  "scenario_e_verify_full": e_verify == "true",
  "scenario_e_concurrent_writer_exact": e_x == "true",
  "failures": int(failures),
  "notes": [n for n in notes.split("|") if n],
  "result": result,
  "evidence": {"log": log, "log_sha256": log_sha},
  "scope": {
    "checked": "cross-handle prepared-transaction pins, the blob-publication reservation window, an epoch cut with the age gate disabled, process kills at four publication stages with a concurrent sweep, MW3 domain-head marking, readers under sweep",
    "not_checked": "physical power loss or host cache loss; a fleet or multi-node store; a writer that stalls past its reservation deadline (treated as abandoned by design); Go parity for the epoch protocol",
    "label": "one host, one local POSIX namespace, process-fault proxies",
  },
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "result": result, "failures": int(failures)}))
PY

echo "receipt: $receipt"
sha256sum "$receipt" "$log"
[[ "$result" == "pass" ]]
