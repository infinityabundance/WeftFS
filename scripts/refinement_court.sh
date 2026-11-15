#!/usr/bin/env bash
# T100 court: a bounded, machine-checked refinement link between the store's
# `weft.history.v1` observations and verification/tla/WeftMultiWriter.tla.
#
# Host:  bash scripts/refinement_court.sh
# Court: dispatched by the host wrapper in three stages, because the checker and
#        the independent enumerator need Python 3 (fuse court) while TLC needs
#        the java-only TLA court. Nothing runs on the host.
#
# Stage 1 (fuse): build the MW2 driver, produce real store histories, run the
#   independent checker on them (they must be accepted), mutate real histories
#   into violating ones (each must be rejected with its named invariant), run
#   the eight committed invariant fixtures, and enumerate the whole model
#   transition relation for two tiny configurations plus the two negative
#   controls.
# Stage 2 (tla): run TLC on the same configurations.
# Stage 3 (fuse): compare the enumerator with TLC and write the receipt.
#
# Exit 78 with one `<name>: skip <reason>` line when the environment cannot run
# the court; the aggregate records a skip, never a pass.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

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
  echo "refinement_court: skip $1" >&2
  exit 78
}

if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  [[ -n "$stage" ]] && { echo "the host wrapper takes no --stage" >&2; exit 64; }
  command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
  docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
  docker image inspect weft/fuse-court:1 >/dev/null 2>&1 || \
    skip "missing image weft/fuse-court:1"
  docker image inspect weft/tla-court:1 >/dev/null 2>&1 || \
    skip "missing image weft/tla-court:1"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p "$root/bench/receipts"
  # The enumerator and the checker need Python 3 (fuse court); TLC needs the
  # java-only TLA court. Three stages, each in the court that can run it. The
  # court creates its own log (it runs as root with cap-drop=ALL, so it cannot
  # append to a host-created file); stage output goes to the caller.
  rc=0
  "$root/scripts/docker-court.sh" fuse \
    "bash scripts/refinement_court.sh --stage fuse --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "refinement court stage fuse failed (rc=$rc)" >&2
    exit "$rc"
  fi
  "$root/scripts/docker-court.sh" tla \
    "bash scripts/refinement_court.sh --stage tla --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "refinement court stage tla failed (rc=$rc)" >&2
    exit "$rc"
  fi
  "$root/scripts/docker-court.sh" fuse \
    "bash scripts/refinement_court.sh --stage assemble --stamp ${stamp}" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "refinement court stage assemble failed (rc=$rc)" >&2
    exit "$rc"
  fi
  echo "receipt: bench/receipts/refinement-${stamp}.json"
  exit 0
fi

cd "$root"
stamp="${stamp:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt_dir="bench/receipts"
log="$receipt_dir/refinement-${stamp}.log"
mkdir -p "$receipt_dir"
[[ -w "$receipt_dir" ]] || { echo "receipt directory is not writable" >&2; exit 1; }

PY_EVIDENCE="$receipt_dir/refinement-${stamp}-py.json"
TLA_EVIDENCE="$receipt_dir/refinement-${stamp}-tla.json"
CFG_A="$receipt_dir/.t100-${stamp}-a.cfg"
CFG_B="$receipt_dir/.t100-${stamp}-b.cfg"
CFG_BLIND="$receipt_dir/.t100-${stamp}-blind.cfg"
CFG_GC="$receipt_dir/.t100-${stamp}-gc.cfg"

echo "== T100 refinement court, stage ${stage} ==" | tee -a "$log"
echo "kernel: $(uname -a)" | tee -a "$log"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})" | tee -a "$log"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}" | tee -a "$log"

# ---------------------------------------------------------------------------
# Stage 1: real histories, mutants, and the transition-relation enumeration.
# ---------------------------------------------------------------------------
if [[ "$stage" == "fuse" ]]; then
  CHECK="verifier/refinement_check.py"
  XCHECK="verifier/refinement_crosscheck.py"
  work="$(mktemp -d /tmp/weft-refinement.XXXXXX)"
  trap 'rm -rf "$work"' EXIT

  cargo build --quiet --example mw2_writer
  RUST_BIN=".docker-cache/target-rust/debug/examples/mw2_writer"
  sha256sum "$RUST_BIN" "$CHECK" "$XCHECK" | tee -a "$log"

  NOW_NS=1700000000000000000
  run_rust() { "$RUST_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }

  # -- real histories -----------------------------------------------------
  k1="$work/k1"; mkdir -p "$k1"
  h1="$receipt_dir/refinement-${stamp}-h1.jsonl"
  run_rust init --store "$k1/store" >/dev/null
  pids=()
  for writer in w0 w1 w2 w3; do
    WEFT_HISTORY="$h1" run_rust write --store "$k1/store" --writer "$writer" \
      --prefix "$writer" --count 6 >"$k1/$writer.json" &
    pids+=("$!")
  done
  for pid in "${pids[@]}"; do wait "$pid"; done
  WEFT_HISTORY="$h1" run_rust read-multi --store "$k1/store" \
    --names w0-000000,w1-000000,w0-000005,w3-000003 >"$k1/read.json"

  k2="$work/k2"; mkdir -p "$k2"
  h2="$receipt_dir/refinement-${stamp}-h2.jsonl"
  run_rust init --store "$k2/store" >/dev/null
  pids=()
  for writer in c0 c1 c2 c3; do
    WEFT_HISTORY="$h2" run_rust write --store "$k2/store" --writer "$writer" \
      --count 4 --hot hot.bin >"$k2/$writer.json" &
    pids+=("$!")
  done
  for pid in "${pids[@]}"; do wait "$pid"; done
  WEFT_HISTORY="$h2" run_rust read-multi --store "$k2/store" --names hot.bin \
    >"$k2/read.json"

  k3="$work/k3"; mkdir -p "$k3"
  h3="$receipt_dir/refinement-${stamp}-h3.jsonl"
  run_rust init --store "$k3/store" >/dev/null
  for phase_name in mw2_prepared mw2_after_head; do
    tx_id="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(("t100:"+sys.argv[1]).encode()).hexdigest()[:32])' "$phase_name")"
    rc=0
    WEFT_HISTORY="$h3" WEFT_CRASH_PHASE="$phase_name" run_rust write \
      --store "$k3/store" --writer k --count 1 --tx-id "$tx_id" \
      >"$k3/crash-$phase_name.json" 2>"$k3/crash-$phase_name.err" || rc=$?
    [[ "$rc" -ne 0 ]] || { echo "FAIL: $phase_name did not kill the writer" >&2; exit 1; }
    WEFT_HISTORY="$h3" run_rust write --store "$k3/store" --writer k --count 1 \
      --tx-id "$tx_id" >"$k3/retry-$phase_name.json" 2>&1
  done

  k4="$work/k4"; mkdir -p "$k4"
  h4="$receipt_dir/refinement-${stamp}-h4.jsonl"
  run_rust init --store "$k4/store" >/dev/null
  domain_a="$(run_rust register --store "$k4/store" --name family-a --kind checkpoint_family --plain)"
  domain_b="$(run_rust register --store "$k4/store" --name family-b --kind checkpoint_family --plain)"
  pids=()
  for spec in "da:$domain_a" "db:$domain_b" "dc:$domain_a"; do
    writer="${spec%%:*}"; domain="${spec##*:}"
    WEFT_HISTORY="$h4" run_rust write --store "$k4/store" --domain "$domain" \
      --writer "$writer" --prefix "$writer" --count 4 >"$k4/$writer.json" &
    pids+=("$!")
  done
  for pid in "${pids[@]}"; do wait "$pid"; done
  WEFT_HISTORY="$h4" run_rust read-multi --store "$k4/store" --domain "$domain_a" \
    --names da-000000,dc-000000 >"$k4/read-a.json"

  # -- accepted real histories -------------------------------------------
  accepted_dir="$work/accepted"; mkdir -p "$accepted_dir"
  for pair in "mw2_disjoint:$h1" "mw2_conflict:$h2" "mw2_faults:$h3" "mw3_domains:$h4"; do
    label="${pair%%:*}"; path="${pair#*:}"
    if ! python3 "$CHECK" "$path" --expect accept --out "$accepted_dir/$label.json" \
        >>"$log" 2>&1; then
      echo "FAIL: the checker rejected the real history $label" | tee -a "$log" >&2
      exit 1
    fi
    python3 - "$label" "$accepted_dir/$label.json" <<'PY' | tee -a "$log"
import json,sys
row=json.load(open(sys.argv[2]))
print(json.dumps({"accepted":sys.argv[1],"ok":row["accepted"],
  "counts":row["counts"],
  "domains":[{"domain":d["domain"],"transactions":d["transactions"],
              "committed":d["committed"],"rejected":d["rejected"],
              "head":d["head"],"checks":d["checks"],
              "abstracted":len(d["abstracted"])} for d in row["domains"]]},
  sort_keys=True))
PY
  done

  # -- mutants of real histories -----------------------------------------
  mutant_dir="$work/mutants"; mkdir -p "$mutant_dir"
  python3 - "$h1" "$mutant_dir" <<'PYMUT' | tee -a "$log"
import json, sys, pathlib
source, outdir = sys.argv[1], pathlib.Path(sys.argv[2])
outdir.mkdir(parents=True, exist_ok=True)
records = [json.loads(line) for line in open(source) if line.strip()]
invokes = {}
for record in records:
    if record.get("event") == "tx-invoke":
        invokes[record["tx"]] = record
commits = []
for record in records:
    if record.get("event") == "tx-response" and record.get("outcome") in ("committed", "idempotent"):
        invoke = invokes[record["tx"]]
        keys = [op["key"] for op in invoke.get("ops") or [] if op.get("op") == "put_entry"]
        commits.append({"response": record, "invoke": invoke,
                        "base": int(invoke.get("base_generation", 0)),
                        "generation": int(record["generation"]), "keys": keys})
commits.sort(key=lambda row: row["generation"])
if len(commits) < 4:
    raise SystemExit("need at least four commits to mutate")

def write(name, mutated, note):
    path = outdir / f"{name}.jsonl"
    with path.open("w") as handle:
        for record in mutated:
            handle.write(json.dumps(record, sort_keys=True) + "\n")
    print(json.dumps({"mutant": name, "records": len(mutated),
                      "construction": note}))

# 1. blind overwrite, on a real history whose writers never touch one key:
#    give a late commit an op on a key written after that commit's own
#    observation point. The store never produced this; the chain is real.
target = None
for i in range(len(commits) - 1, 1, -1):
    later = commits[i]
    for j in range(i - 1, -1, -1):
        earlier = commits[j]
        if (earlier["keys"] and earlier["base"] < later["generation"]
                and earlier["generation"] > later["base"]):
            target = (later, earlier)
            break
    if target:
        break
if target is None:
    raise SystemExit("no commit pair with an intervening write to mutate")
later, earlier = target
key = earlier["keys"][0]
mutated = [dict(r) for r in records]
for record in mutated:
    if record.get("event") == "tx-invoke" and record.get("tx") == later["response"]["tx"]:
        record["ops"] = list(record.get("ops") or []) + [
            {"key": key, "op": "put_entry", "value": "target:mutant"}
        ]
    if record.get("event") == "tx-response" and record.get("tx") == later["response"]["tx"]:
        record["attempts"] = 1
write("blind_overwrite", mutated,
      "tx at generation %d (base %d) also writes a key written at generation %d"
      % (later["generation"], later["base"], earlier["generation"]))

# 2. lost ack: an acknowledgement for a transaction that never started.
mutated = [dict(r) for r in records]
ghost = json.loads(json.dumps(commits[-1]["response"]))
ghost["tx"] = "ghost-" + ghost["tx"][:8]
mutated.append(ghost)
write("lost_ack", mutated, "append one ack whose transaction has no invoke")

# 3. dropped commit: remove one committed response, leaving a generation hole.
dropped = commits[len(commits) // 2]
mutated = [r for r in records
           if not (r.get("event") == "tx-response"
                   and r.get("tx") == dropped["response"]["tx"])]
write("dropped_commit", mutated,
      "remove the response for generation %d" % dropped["generation"])

# 4. GC reclaims a committed key.
mutated = [dict(r) for r in records]
mutated.append({"schema": "weft.history.v1", "event": "gc-observe",
                "at_generation": commits[-1]["generation"], "deleted": [key]})
write("gc_reclaims_reachable", mutated,
      "a gc-observe claims key %s was reclaimed" % key)

# 5. a reader selects a generation above the head.
mutated = [dict(r) for r in records]
mutated.append({"schema": "weft.history.v1", "event": "read-response",
                "handle": "t100-reader", "kind": "lookup", "key": key,
                "value": "target:future",
                "read_generation": commits[-1]["generation"] + 3})
write("read_above_head", mutated,
      "a read claims generation %d" % (commits[-1]["generation"] + 3))
PYMUT

  mutant_rows="$work/mutant-rows"
  : >"$mutant_rows"
  for pair in \
    "blind_overwrite:NoBlindOverwrite" \
    "lost_ack:NoLostAck" \
    "dropped_commit:HeadWellFormed" \
    "gc_reclaims_reachable:GcNeverReclaimsReachable" \
    "read_above_head:NoPartial"; do
    name="${pair%%:*}"
    want="${pair#*:}"
    report="$mutant_dir/$name.report.json"
    if ! python3 "$CHECK" "$mutant_dir/$name.jsonl" --expect reject \
        --expect-invariant "$want" --out "$report" >>"$log" 2>&1; then
      echo "FAIL: mutant $name was not rejected with $want" | tee -a "$log" >&2
      exit 1
    fi
    python3 - "$name" "$want" "$report" "$mutant_rows" <<'PY'
import json,sys
name,want,report,dest=sys.argv[1:5]
row=json.load(open(report))
line={"mutant":name,"expected_invariant":want,
      "detected":row["invariants"],"ok":want in row["invariants"]}
open(dest,"a").write(json.dumps(line,sort_keys=True)+"\n")
print(json.dumps(line,sort_keys=True))
PY
  done

  # -- committed invariant fixtures --------------------------------------
  fixture_rows="$work/fixture-rows"
  : >"$fixture_rows"
  for fixture in tests/history/refinement/*.jsonl; do
    name="$(basename "$fixture" .jsonl)"
    case "$name" in
      no_blind_overwrite|conflict_without_write|retry_bound_overrun) want=NoBlindOverwrite ;;
      no_lost_ack) want=NoLostAck ;;
      head_well_formed) want=HeadWellFormed ;;
      no_partial) want=NoPartial ;;
      gc_reclaims_reachable) want=GcNeverReclaimsReachable ;;
      live_prepared_durable) want=LivePreparedDurable ;;
      *) want="" ;;
    esac
    report="$work/fixture-$name.json"
    if ! python3 "$CHECK" "$fixture" --expect reject --expect-invariant "$want" \
        --out "$report" >>"$log" 2>&1; then
      echo "FAIL: fixture $name was not rejected with $want" | tee -a "$log" >&2
      exit 1
    fi
    python3 - "$name" "$want" "$report" "$fixture_rows" <<'PY'
import json,sys
name,want,report,dest=sys.argv[1:5]
row=json.load(open(report))
line={"fixture":name,"expected_invariant":want,
      "detected":row["invariants"],"ok":want in row["invariants"]}
open(dest,"a").write(json.dumps(line,sort_keys=True)+"\n")
print(json.dumps(line,sort_keys=True))
PY
  done

  # -- accepted fixtures (false-positive control) -------------------------
  for fixture in tests/history/valid/*.jsonl; do
    if ! python3 "$CHECK" "$fixture" --expect accept >>"$log" 2>&1; then
      echo "FAIL: valid fixture $fixture was rejected" | tee -a "$log" >&2
      exit 1
    fi
    echo "control accepted: $(basename "$fixture")" | tee -a "$log"
  done

  # -- enumeration of the transition relation ----------------------------
  python3 "$XCHECK" --keys 1 --committers 2 --max-gen 2 --max-attempts 1 \
    --emit-cfg "$CFG_A" --out "$work/xcheck-a.json" | tee -a "$log"
  python3 "$XCHECK" --keys 2 --committers 2 --max-gen 2 --max-attempts 0 \
    --emit-cfg "$CFG_B" --out "$work/xcheck-b.json" | tee -a "$log"
  python3 "$XCHECK" --flaw blind --keys 1 --committers 2 --max-gen 2 \
    --max-attempts 1 --emit-cfg "$CFG_BLIND" --out "$work/xcheck-blind.json" | tee -a "$log"
  python3 "$XCHECK" --flaw gc --keys 1 --committers 2 --max-gen 2 \
    --max-attempts 1 --emit-cfg "$CFG_GC" --out "$work/xcheck-gc.json" | tee -a "$log"

  python3 - "$PY_EVIDENCE" "$work" "$accepted_dir" "$mutant_rows" "$fixture_rows" <<'PY'
import glob,json,os,sys
dest,work,accepted,mutants,fixtures=sys.argv[1:6]
def rows(path):
    return [json.loads(line) for line in open(path) if line.strip()]
doc={
 "schema":"weftfs.refinement.py-stage/1",
 "accepted_histories":[json.load(open(p)) for p in sorted(glob.glob(accepted+"/*.json"))],
 "real_history_mutants":rows(mutants),
 "invariant_fixtures":rows(fixtures),
 "enumeration":{
   "correct_a":json.load(open(work+"/xcheck-a.json")),
   "correct_b":json.load(open(work+"/xcheck-b.json")),
   "blind":json.load(open(work+"/xcheck-blind.json")),
   "gc":json.load(open(work+"/xcheck-gc.json")),
 },
 "environment":{
   "court":os.environ.get("WEFT_COURT_IMAGE","unknown"),
   "kernel":os.uname().release,
   "limit_memory":os.environ.get("WEFT_COURT_MEMORY","?"),
   "limit_cpus":os.environ.get("WEFT_COURT_CPUS","?"),
   "limit_pids":os.environ.get("WEFT_COURT_PIDS","?"),
 },
 "repositories":[{"name":"WeftFS","revision":os.environ.get("WEFT_REPO_WEFTFS","unknown")}],
}
json.dump(doc,open(dest,"w"),indent=2,sort_keys=True)
open(dest,"a").write("\n")
print("py evidence:",dest)
PY
  exit 0
fi

# ---------------------------------------------------------------------------
# Stage 2: TLC on the same configurations.
# ---------------------------------------------------------------------------
if [[ "$stage" == "tla" ]]; then
  [[ -r "$CFG_A" && -r "$CFG_B" && -r "$CFG_BLIND" && -r "$CFG_GC" ]] || {
    echo "missing generated configuration; run the fuse stage first" >&2
    exit 1
  }
  jar="${TLA_JAR:-/opt/tla/tla2tools.jar}"
  [[ -r "$jar" ]] || { echo "missing $jar" >&2; exit 1; }
  echo "tla2tools sha256: $(sha256sum "$jar" | cut -d' ' -f1)" | tee -a "$log"
  echo "java: $(java -version 2>&1 | head -n 1)" | tee -a "$log"
  work="$(mktemp -d /tmp/weft-refinement-tla.XXXXXX)"
  trap 'rm -rf "$work"' EXIT
  cp verification/tla/*.tla "$work/"
  cp "$CFG_A" "$work/a.cfg"
  cp "$CFG_B" "$work/b.cfg"
  cp "$CFG_BLIND" "$work/blind.cfg"
  cp "$CFG_GC" "$work/gc.cfg"

  # One invariant at a time for the flawed models: TLC stops at the first
  # counterexample, so a single run cannot enumerate every reachable violation.
  # No Python in this court: the configurations are three static files.
  for invariant in LivePreparedDurable CommittedBlobsDurable GcNeverReclaimsReachable; do
    {
      printf 'SPECIFICATION Spec\n\nCONSTANTS\n'
      grep -E '^  (Committers|Keys|MaxGen|MaxAttempts) =' "$work/gc.cfg"
      printf '\nINVARIANT %s\n\nCHECK_DEADLOCK FALSE\n' "$invariant"
    } >"$work/gc-${invariant}.cfg"
  done

  results="$work/results"
  : >"$results"
  run_tlc() {
    local label="$1" module="$2" cfg="$3" expect="$4"
    local out="$work/${label}.tlc"
    local rc=0
    ( cd "$work" && timeout 600 java -jar "$jar" -workers 1 \
        -metadir "$work/meta-$label" -config "$cfg" "$module" ) >"$out" 2>&1 || rc=$?
    local distinct generated violated last_line
    distinct="$(grep -oE '[0-9]+ distinct states found' "$out" | grep -oE '[0-9]+' | tail -1 || true)"
    generated="$(grep -oE '[0-9]+ states generated' "$out" | grep -oE '[0-9]+' | tail -1 || true)"
    violated="$(sed -n 's/.*Invariant \([A-Za-z0-9_]*\) is violated.*/\1/p' "$out" | head -1 || true)"
    last_line="$(tail -n 1 "$out")"
    completed=false
    [[ -n "$distinct" ]] && completed=true
    jq -nc --arg run "$label" --arg expect "$expect" \
      --argjson distinct "${distinct:-null}" --argjson generated "${generated:-null}" \
      --arg violated "$violated" --argjson rc "$rc" --arg last "$last_line" \
      --argjson completed "$completed" \
      '{run:$run,expect:$expect,distinct_states:$distinct,
        states_generated:$generated,
        violated_invariant:(if $violated=="" then null else $violated end),
        exit_code:$rc,completed:$completed,last_line:$last}' \
      | tee -a "$results"
    echo "--- tlc $label ---" >>"$log"
    cat "$out" >>"$log"
  }

  run_tlc correct_a WeftMultiWriter "$work/a.cfg" no-error
  run_tlc correct_b WeftMultiWriter "$work/b.cfg" no-error
  run_tlc blind WeftMultiWriterBuggy "$work/blind.cfg" NoBlindOverwrite
  for invariant in LivePreparedDurable CommittedBlobsDurable GcNeverReclaimsReachable; do
    run_tlc "gc-$invariant" WeftMultiWriterGcBuggy "$work/gc-$invariant.cfg" "$invariant"
  done

  jq -s --arg court "${WEFT_COURT_IMAGE:-unknown}" --arg kernel "$(uname -r)" \
    --arg revision "${WEFT_REPO_WEFTFS:-unknown}" \
    '{schema:"weftfs.refinement.tla-stage/1",runs:.,
      environment:{court:$court,kernel:$kernel},
      repositories:[{name:"WeftFS",revision:$revision}]}' \
    "$results" >"$TLA_EVIDENCE"
  echo "tla evidence: $TLA_EVIDENCE" | tee -a "$log"
  exit 0
fi

# ---------------------------------------------------------------------------
# Stage 3: compare the independent enumerator with TLC and write the receipt.
# ---------------------------------------------------------------------------
if [[ "$stage" == "assemble" ]]; then
  python3 - "$PY_EVIDENCE" "$TLA_EVIDENCE" "$receipt_dir/refinement-${stamp}.json" \
    "$stamp" "${WEFT_REPO_WEFTFS:-unknown}" <<'PY' | tee -a "$log"
import json,os,sys
py_path,tla_path,dest,stamp,revision=sys.argv[1:6]
py=json.load(open(py_path)); tla=json.load(open(tla_path))
enum=py["enumeration"]; runs={row["run"]:row for row in tla["runs"]}
checks=[]
def check(name, ok, detail):
    checks.append({"check":name,"ok":bool(ok),"detail":detail})
    print(("PASS " if ok else "FAIL ") + name + (": " + str(detail) if detail else ""))

for label, key in (("correct_a","correct_a"),("correct_b","correct_b")):
    row=runs.get(label,{})
    check(f"{label}_tlc_completed", row.get("completed"),
          row.get("last_line"))
    check(f"{label}_tlc_no_error", row.get("violated_invariant") is None and row.get("exit_code")==0,
          {"exit":row.get("exit_code"),"violated":row.get("violated_invariant")})
    check(f"{label}_distinct_states_match",
          enum[key]["distinct_states"]==row.get("distinct_states"),
          {"enumerator":enum[key]["distinct_states"],"tlc":row.get("distinct_states")})
    check(f"{label}_generated_states_match",
          enum[key]["states_generated"]==row.get("states_generated"),
          {"enumerator":enum[key]["states_generated"],"tlc":row.get("states_generated")})
    check(f"{label}_enumerator_finds_no_violation", enum[key]["violations"]==[],
          enum[key]["violations"])

blind=runs.get("blind",{})
check("blind_tlc_reports_no_blind_overwrite",
      blind.get("violated_invariant")=="NoBlindOverwrite",
      {"exit":blind.get("exit_code"),"violated":blind.get("violated_invariant")})
check("blind_enumerator_reports_no_blind_overwrite",
      "NoBlindOverwrite" in enum["blind"]["violations"], enum["blind"]["violations"])
gc_enum=set(enum["gc"]["violations"])
for invariant in ("LivePreparedDurable","CommittedBlobsDurable","GcNeverReclaimsReachable"):
    row=runs.get(f"gc-{invariant}",{})
    check(f"gc_tlc_reports_{invariant}", row.get("violated_invariant")==invariant,
          {"exit":row.get("exit_code"),"violated":row.get("violated_invariant")})
    check(f"gc_enumerator_reports_{invariant}", invariant in gc_enum, sorted(gc_enum))

accepted=py["accepted_histories"]
check("real_histories_accepted", all(row["accepted"] for row in accepted) and len(accepted)==4,
      [row["history"] for row in accepted])
mutants=py["real_history_mutants"]
check("real_history_mutants_all_rejected",
      len(mutants)==5 and all(row["ok"] for row in mutants), mutants)
fixtures=py["invariant_fixtures"]
check("invariant_fixtures_all_rejected",
      len(fixtures)==8 and all(row["ok"] for row in fixtures),
      [row["fixture"] for row in fixtures])
check("invariant_coverage",
      {"NoBlindOverwrite","NoLostAck","HeadWellFormed","NoPartial",
       "GcNeverReclaimsReachable","LivePreparedDurable"} <=
      {row["expected_invariant"] for row in mutants+fixtures},
      sorted({row["expected_invariant"] for row in mutants+fixtures}))
counts={}
for row in accepted:
    for name,value in row.get("counts",{}).items():
        counts[name]=counts.get(name,0)+value
check("blind_overwrite_fully_checked", counts.get("fully_checked",0) >= 10, counts)

doc={
 "schema":"weftfs.refinement/1",
 "role":"T100 bounded refinement link: weft.history.v1 -> WeftMultiWriter.tla",
 "generated_at":stamp,
 "revision":revision,
 "environment":py["environment"],
 "repositories":py["repositories"],
 "mapping_document":"verification/REFINEMENT.md",
 "checker":"verifier/refinement_check.py",
 "enumerator":"verifier/refinement_crosscheck.py",
 "model":"verification/tla/WeftMultiWriter.tla (+Buggy, +GcBuggy) with the T100 Release action",
 "configurations":{
   "correct_a":{"committers":2,"keys":1,"max_gen":2,"max_attempts":1},
   "correct_b":{"committers":2,"keys":2,"max_gen":2,"max_attempts":0},
   "flaws":{"committers":2,"keys":1,"max_gen":2,"max_attempts":1},
 },
 "accepted_histories":accepted,
 "real_history_mutants":mutants,
 "invariant_fixtures":fixtures,
 "enumeration":enum,
 "tlc":runs,
 "checks":checks,
 "checks_passed":sum(1 for row in checks if row["ok"]),
 "checks_total":len(checks),
 "assumptions":[
  "one conflict domain = one model instance; no cross-domain property is claimed",
  "Start is placed at the publication of the generation the transaction observed",
  "the unmodelled prefix (store init and anything before the first observed generation) wrote no modelled key",
  "an idempotent response collapses a crashed attempt and its retry into one Start/Publish/Ack",
  "Busy/quota/capability/id-reuse aborts are abstracted as Start+CrashPrepared (the model has no such guards)",
  "the model's Retry action is not used by the mapping: the last refresh point of a committed transaction is not in the history, so attempts==1 is checked exactly and attempts>1 only by the counting bound",
  "GC is not observed by the store; gc-observe records exist only to exercise the GC invariants",
 ],
 "not_established":[
  "full refinement (T100 is one bounded projection, not a proof)",
  "unbounded verification",
  "liveness, fairness, starvation freedom",
  "any cross-domain (MW3) safety property",
  "value-level read consistency (verifier/history_check.py owns it)",
  "blind overwrite of keys that existed before the trace",
  "that the Rust implementation's physical commit protocol matches the model below the history channel",
 ],
 "result":"pass" if all(row["ok"] for row in checks) else "fail",
}
json.dump(doc,open(dest,"w"),indent=2,sort_keys=True)
open(dest,"a").write("\n")
print(json.dumps({"receipt":dest,"result":doc["result"],
                  "checks":f"{doc['checks_passed']}/{doc['checks_total']}"}))
PY
  python3 - "$receipt_dir/refinement-${stamp}.json" <<'PY'
import json,sys
doc=json.load(open(sys.argv[1]))
raise SystemExit(0 if doc["result"]=="pass" else 1)
PY
  # The generated configurations are reproducible inputs, not evidence: the
  # enumerator regenerates them from the command line recorded above.
  rm -f "$CFG_A" "$CFG_B" "$CFG_BLIND" "$CFG_GC"
  exit 0
fi

echo "unknown stage '$stage'" >&2
exit 64
