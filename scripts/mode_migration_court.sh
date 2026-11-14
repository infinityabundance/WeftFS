#!/usr/bin/env bash
# T028 court: explicit safe mixed-mode migration.
#
# Host:  bash scripts/mode_migration_court.sh
# Court: bash scripts/docker-court.sh fuse "bash scripts/mode_migration_court.sh"
#
# Rolling/mixed-version sequence: an old reader and an old writer (the v0.9.0
# binary, built from the tagged source in the court) against a newer store; new
# readers and writers against an old store; the supported transitions
# (single -> optimistic -> sharded); every downgrade; a live prepared
# transaction; a crash at each side of the transition CAS; and a reader that
# spans the transition.
#
# The property: **no window exists in which two conforming writers both believe
# they are authoritative.** The mode is one record, changed by one
# compare-and-swap; every writable open is checked against it, and a downgrade
# is refused. A pre-record binary is outside the protocol; its behaviour is
# measured and reported as the known, unpreventable hazard (see the receipt).
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
old_tag="${WEFT_MODE_OLD_TAG:-v0.9.0}"
old_dir="$root/.docker-cache/mode-old/$old_tag"

if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  # Export the tagged source on the host (git archive is a file operation, not
  # a build) and then run everything else inside the court.
  mkdir -p "$old_dir"
  if [[ ! -f "$old_dir/.exported" ]]; then
    rm -rf "${old_dir:?}"/.git-archive
    mkdir -p "$old_dir/.git-archive"
    git --no-pager archive "$old_tag" | tar -x -C "$old_dir/.git-archive"
    rm -rf "$old_dir/src-tree"
    mv "$old_dir/.git-archive" "$old_dir/src-tree"
    rm -rf "$old_dir/src-tree/WeftCodec"
    mkdir -p "$old_dir/src-tree/WeftCodec"
    git -C WeftCodec --no-pager archive "$old_tag" | tar -x -C "$old_dir/src-tree/WeftCodec"
    date -u +%Y-%m-%dT%H:%M:%SZ >"$old_dir/.exported"
  fi
  exec "$root/scripts/docker-court.sh" fuse "bash scripts/mode_migration_court.sh"
fi

cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="bench/receipts/mode-migration-${stamp}.json"
log="bench/receipts/mode-migration-${stamp}.log"
work="$(mktemp -d /tmp/weft-mode.XXXXXX)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT
mkdir -p bench/receipts
: >"$log"
exec > >(tee -a "$log") 2>&1

NOW_NS=1700000000000000000
NEW_BIN=".docker-cache/target-rust/debug/weftfs"
DRIVER=".docker-cache/target-rust/debug/examples/mw2_writer"
OLD_BIN=".docker-cache/mode-old/${old_tag}/target/debug/weftfs"
failures=0
record_failure() {
  failures=$((failures + 1))
  echo "FAIL: $1" >&2
}
new() { "$NEW_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }
run_new() { "$NEW_BIN" --store "$1" --fixed-timestamp-ns "$NOW_NS" "${@:2}"; }
driver() { "$DRIVER" "$@" --fixed-timestamp-ns "$NOW_NS"; }
# Register a probe domain and prove it can be written and verified. A sharded
# store has no global head, so this is how its usability is checked.
probe_domain() {
  local store="$1" label="$2" id
  id="$(run_new "$store" mw3 register --name "probe-$label" --kind namespace 2>/dev/null | tr -d '[:space:]')"
  [[ "$id" =~ ^[0-9a-f]{64}$ ]] || return 1
  driver write --store "$store" --domain "$id" --writer "p$label" --count 1 >/dev/null 2>&1 || return 1
  run_new "$store" --writer-mode sharded --domain "$id" verify --full >/dev/null 2>&1 || return 1
}

echo "== T028 mode migration court =="
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"
echo "old binary tag: $old_tag"

cargo build --quiet --bin weftfs --example mw2_writer
if [[ ! -x "$OLD_BIN" || ! -d "$(dirname "$OLD_BIN")" ]]; then
  echo "== building the $old_tag binary from tagged source (in court) =="
  ( cd ".docker-cache/mode-old/${old_tag}/src-tree" && \
    CARGO_TARGET_DIR="/work/.docker-cache/mode-old/${old_tag}/target" \
    cargo build --quiet --offline --locked --bin weftfs )
fi
old_sha="$(sha256sum "$OLD_BIN" | cut -d' ' -f1)"
new_sha="$(sha256sum "$NEW_BIN" | cut -d' ' -f1)"
echo "old sha256=$old_sha"
echo "new sha256=$new_sha"

# --- A: a fresh store ------------------------------------------------------
echo "== A fresh store =="
a="$work/a"; mkdir -p "$a"
new --store "$a/store" init >/dev/null
a_show="$(run_new "$a/store" --json mode show)"
a_single_ok=false
if run_new "$a/store" create a.txt --text "single-mode" >/dev/null 2>"$a/single.err"; then a_single_ok=true; else record_failure "A: a single writer failed on a fresh store"; fi
a_reader=false
if run_new "$a/store" --json info >/dev/null 2>"$a/reader.err"; then a_reader=true; else record_failure "A: a reader failed"; fi
echo "A: $a_show single_write=$a_single_ok reader=$a_reader"
python3 - "$a_show" <<'PY' || record_failure "A: a fresh store did not record `single`"
import json,sys
d=json.loads(sys.argv[1])
raise SystemExit(0 if d.get("effective")=="single" and d.get("record_present") is False else 1)
PY

# --- B: single -> optimistic, and the refusals it enables ------------------
echo "== B single -> optimistic =="
b="$work/b"; mkdir -p "$b"
new --store "$b/store" init >/dev/null
driver write --store "$b/store" --writer o --count 1 >/dev/null
b_show="$(run_new "$b/store" --json mode show)"
b_single_refused=false
if run_new "$b/store" --writer-mode single create bad.txt --text x >"$b/bad.out" 2>"$b/bad.err"; then
  record_failure "B: a single writable open succeeded on an optimistic store"
else
  grep -q UnsupportedVersion "$b/bad.err" && b_single_refused=true
  [[ "$b_single_refused" == "true" ]] || record_failure "B: the refusal was not UnsupportedVersion"
fi
b_old_reader=false
if run_new "$b/store" --json info >/dev/null 2>"$b/reader.err"; then b_old_reader=true; else record_failure "B: a mode-unaware reader failed"; fi
b_migrate_down_refused=false
if run_new "$b/store" --json mode migrate --from optimistic --to single >"$b/down.out" 2>"$b/down.err"; then
  record_failure "B: optimistic -> single succeeded"
else
  grep -q UnsupportedVersion "$b/down.err" && b_migrate_down_refused=true
fi
echo "B: $b_show single_refused=$b_single_refused reader=$b_old_reader downgrade_refused=$b_migrate_down_refused"

# --- C: a live prepared transaction blocks the transition -----------------
echo "== C live prepared transaction =="
c="$work/c"; mkdir -p "$c"
new --store "$c/store" init >/dev/null
driver write --store "$c/store" --writer seed --count 1 >/dev/null
c_tx="000000000000000000000000000000c1"
c_rc=0
WEFT_CRASH_PHASE=mw2_prepared driver write --store "$c/store" --writer k --count 1 --tx-id "$c_tx" \
  >"$c/crash.json" 2>"$c/crash.err" || c_rc=$?
[[ "$c_rc" -ne 0 ]] || record_failure "C: the writer was not killed"
c_blocked=false
if run_new "$c/store" --json mode migrate --from optimistic --to sharded >"$c/blocked.out" 2>"$c/blocked.err"; then
  record_failure "C: the migration ignored a live prepared transaction"
else
  grep -q Busy "$c/blocked.err" && grep -q "$c_tx" "$c/blocked.err" && c_blocked=true
fi
c_show_blocked="$(run_new "$c/store" --json mode show)"
driver write --store "$c/store" --writer k --count 1 --tx-id "$c_tx" >"$c/retry.json" 2>"$c/retry.err" || record_failure "C: the retry failed"
c_migrated=false
if run_new "$c/store" --json mode migrate --from optimistic --to sharded >"$c/migrate.out" 2>"$c/migrate.err"; then
  c_migrated=true
else
  record_failure "C: the migration failed after the transaction resolved"
fi
echo "C: blocked=$c_blocked mode_still=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["effective"])' "$c_show_blocked") migrated=$c_migrated"

# --- D: the sharded store refuses every other writable mode ---------------
echo "== D post-migration identity =="
d_show="$(run_new "$c/store" --json mode show)"
d_single=false d_optimistic=false
if run_new "$c/store" --writer-mode single create bad.txt --text x >/dev/null 2>"$c/bad-single.err"; then
  record_failure "D: single write succeeded on a sharded store"
else
  grep -q UnsupportedVersion "$c/bad-single.err" && d_single=true
fi
if run_new "$c/store" --writer-mode optimistic create bad.txt --text x >/dev/null 2>"$c/bad-opt.err"; then
  record_failure "D: optimistic write succeeded on a sharded store"
else
  grep -q UnsupportedVersion "$c/bad-opt.err" && d_optimistic=true
fi
d_domain_ok=false
if probe_domain "$c/store" after; then d_domain_ok=true; fi
[[ "$d_domain_ok" == "true" ]] || record_failure "D: a domain write/verify failed after migration"
d_down=false
if run_new "$c/store" --json mode migrate --from sharded --to optimistic >/dev/null 2>"$c/down.err"; then
  record_failure "D: sharded -> optimistic succeeded"
else
  grep -q UnsupportedVersion "$c/down.err" && d_down=true
fi
echo "D: $d_show single_refused=$d_single optimistic_refused=$d_optimistic domain_write=$d_domain_ok downgrade_refused=$d_down"

# --- E: single -> sharded is refused --------------------------------------
echo "== E single -> sharded refusal =="
e="$work/e"; mkdir -p "$e"
new --store "$e/store" init >/dev/null
e_refused=false
if run_new "$e/store" --json mode migrate --from single --to sharded >/dev/null 2>"$e/err"; then
  record_failure "E: single -> sharded succeeded (a global head was promoted silently)"
else
  grep -q UnsupportedVersion "$e/err" && grep -q "promoted" "$e/err" && e_refused=true
fi
echo "E: refused=$e_refused"

# --- F: a crash at each side of the transition CAS ------------------------
echo "== F crash during the transition =="
f_rows=()
for hook in mode_after_validate mode_after_cas; do
  f="$work/f-$hook"; mkdir -p "$f"
  new --store "$f/store" init >/dev/null
  driver write --store "$f/store" --writer f --count 1 >/dev/null
  f_rc=0
  WEFT_CRASH_PHASE="$hook" run_new "$f/store" --json mode migrate --from optimistic --to sharded \
    --quiesce-ms 0 >"$f/crash.out" 2>"$f/crash.err" || f_rc=$?
  [[ "$f_rc" -ne 0 ]] || record_failure "F: $hook did not crash"
  f_show="$(run_new "$f/store" --json mode show)"
  f_mode="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["effective"])' "$f_show")"
  f_expected="optimistic"
  [[ "$hook" == "mode_after_cas" ]] && f_expected="sharded"
  [[ "$f_mode" == "$f_expected" ]] || record_failure "F: $hook left mode $f_mode, expected $f_expected"
  f_verify=false
  if [[ "$f_mode" == "sharded" ]]; then
    if probe_domain "$f/store" "$hook" >/dev/null 2>"$f/verify.err"; then f_verify=true; fi
  else
    if run_new "$f/store" --json verify --full >/dev/null 2>"$f/verify.err"; then f_verify=true; fi
  fi
  [[ "$f_verify" == "true" ]] || record_failure "F: $hook left an unverifiable store"
  echo "F $hook: exit=$f_rc mode=$f_mode verify=$f_verify"
  f_rows+=("$(python3 - "$hook" "$f_rc" "$f_mode" "$f_expected" "$f_verify" <<'PY'
import json,sys
print(json.dumps({"hook":sys.argv[1],"exit":int(sys.argv[2]),"mode_after_crash":sys.argv[3],
 "expected_mode":sys.argv[4],"verify_full":sys.argv[5]=="true",
 "single_authority":sys.argv[3]==sys.argv[4]},sort_keys=True))
PY
)")
done

# --- G: a reader that spans the transition -------------------------------
echo "== G reader spanning the transition =="
g="$work/g"; mkdir -p "$g"
new --store "$g/store" init >/dev/null
driver write --store "$g/store" --writer v --count 1 --hot span.bin >/dev/null
driver hold-read --store "$g/store" --name span.bin --ready "$g/ready" --gate "$g/gate" \
  >"$g/reader.json" 2>"$g/reader.err" &
reader_pid=$!
for _ in $(seq 1 600); do [[ -f "$g/ready" ]] && break; sleep 0.01; done
g_migrated=false
if run_new "$g/store" --json mode migrate --from optimistic --to sharded --quiesce-ms 100 \
    >"$g/migrate.out" 2>"$g/migrate.err"; then
  g_migrated=true
else
  record_failure "G: the migration failed while a reader held the store"
fi
touch "$g/gate"
wait "$reader_pid" || record_failure "G: the spanning reader failed"
g_same="$(python3 - "$g/reader.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
b,a=r.get("before",{}),r.get("after",{})
print(str(bool(b.get("content_hash")) and b.get("content_hash")==a.get("content_hash")).lower())
PY
)"
g_verify=false
if probe_domain "$g/store" span >/dev/null 2>"$g/verify.err"; then g_verify=true; else record_failure "G: the store was unusable after the spanned transition"; fi
echo "G: migrated=$g_migrated reader_snapshot_stable=$g_same verify=$g_verify"

# --- H: the old binary against a newer store -----------------------------
echo "== H old binary ($old_tag) =="
h="$work/h"; mkdir -p "$h"
# The old reader predates `weftfs.commit.v2`; this store is written in the v1
# record format so the check is about *mode identity*, not commit decoding.
export WEFT_NO_ATTEST_INDEXES=1
new --store "$h/store" --no-attest-indexes init >/dev/null
driver write --store "$h/store" --writer o --count 1 >/dev/null
unset WEFT_NO_ATTEST_INDEXES
h_reader=false
if "$OLD_BIN" --store "$h/store" --json info >"$h/old-read.json" 2>"$h/old-read.err"; then h_reader=true; else record_failure "H: the old reader failed on a newer store"; fi
h_writer=false
if "$OLD_BIN" --store "$h/store" create old-binary.txt --text "from-v0.9.0" >"$h/old-write.out" 2>"$h/old-write.err"; then
  h_writer=true
fi
h_verify=false
if run_new "$h/store" --json verify --full >/dev/null 2>"$h/verify.err"; then h_verify=true; else record_failure "H: the store failed verify after the old binary wrote"; fi
h_mode_after="$(run_new "$h/store" --json mode show)"
h_new_single_refused=false
if run_new "$h/store" --writer-mode single create still-bad.txt --text x >/dev/null 2>"$h/bad.err"; then
  record_failure "H: a new single writer succeeded after the old write"
else
  grep -q UnsupportedVersion "$h/bad.err" && h_new_single_refused=true
fi
echo "H: old_reads=$h_reader old_writes=$h_writer verify_full=$h_verify record_still_optimistic=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["effective"])' "$h_mode_after") new_single_refused=$h_new_single_refused"

# --- Receipt -------------------------------------------------------------
result="pass"
[[ "$failures" -eq 0 ]] || result="fail"
f_json="[$(IFS=,; echo "${f_rows[*]}")]"
python3 - "$receipt" "$stamp" "$result" "$failures" "$old_tag" "$old_sha" "$new_sha" \
  "$a_show" "$a_single_ok" "$a_reader" "$b_show" "$b_single_refused" "$b_migrate_down_refused" \
  "$c_blocked" "$c_migrated" "$d_show" "$d_single" "$d_optimistic" "$d_domain_ok" "$d_down" \
  "$e_refused" "$f_json" "$g_migrated" "$g_same" "$g_verify" \
  "$h_reader" "$h_writer" "$h_verify" "$h_new_single_refused" "$h_mode_after" "$log" <<'PY'
import json,os,sys
(receipt,stamp,result,failures,old_tag,old_sha,new_sha,a_show,a_single,a_reader,
 b_show,b_single,b_down,c_blocked,c_migrated,d_show,d_single,d_opt,d_domain,d_down,
 e_refused,f_rows,g_migrated,g_same,g_verify,h_reader,h_writer,h_verify,h_single_refused,
 h_mode,log)=sys.argv[1:]
def j(text): return json.loads(text)
doc={
 "schema":"weftfs.mode_migration/1",
 "role":"T028 explicit safe mixed-mode migration",
 "generated_at":stamp,
 "environment":{
   "court":os.environ.get("WEFT_COURT_IMAGE","unknown"),
   "kernel":os.uname().release,
   "limit_memory":os.environ.get("WEFT_COURT_MEMORY","?"),
   "limit_cpus":os.environ.get("WEFT_COURT_CPUS","?"),
   "limit_pids":os.environ.get("WEFT_COURT_PIDS","?"),
 },
 "repositories":[{"name":"WeftFS","revision":os.environ.get("WEFT_REPO_WEFTFS","unknown")}],
 "binaries":{"new_sha256":new_sha,"old_tag":old_tag,"old_sha256":old_sha},
 "mechanism":"the writer mode is one record (`writers`); a migration is one compare-and-swap on it; every writable open is checked against it before the writer lock is taken",
 "transition_table":[
   {"from":"single","to":"optimistic","verdict":"allowed","evidence":"A, B: record created, single writes refused afterwards"},
   {"from":"optimistic","to":"sharded","verdict":"allowed with quiescence","evidence":"C: a live prepared transaction blocks it; D: sharded writes/verify work afterwards"},
   {"from":"single","to":"sharded","verdict":"refused (UnsupportedVersion)","evidence":"E"},
   {"from":"optimistic","to":"single","verdict":"refused (downgrade)","evidence":"B"},
   {"from":"sharded","to":"optimistic","verdict":"refused (downgrade)","evidence":"D"},
   {"from":"sharded","to":"single","verdict":"refused (downgrade)","evidence":"D"},
 ],
 "fresh_store":{"show":j(a_show),"single_write":a_single=="true","reader":a_reader=="true"},
 "optimistic":{"show":j(b_show),"single_open_refused":b_single=="true",
   "downgrade_refused":b_down=="true"},
 "live_prepared_blocks":c_blocked=="true",
 "migration_after_resolution":c_migrated=="true",
 "sharded":{"show":j(d_show),"single_refused":d_single=="true",
   "optimistic_refused":d_opt=="true","domain_write_verified":d_domain=="true",
   "downgrade_refused":d_down=="true"},
 "single_to_sharded_refused":e_refused=="true",
 "crash_during_transition":json.loads(f_rows),
 "reader_spanning_transition":{"migrated":g_migrated=="true",
   "reader_snapshot_stable":g_same=="true","verify_full":g_verify=="true"},
 "old_binary":{
   "tag":old_tag,
   "reader_on_newer_store":h_reader=="true",
   "writer_on_newer_store_succeeded":h_writer=="true",
   "store_verifies_after_old_write":h_verify=="true",
   "record_still_says":j(h_mode)["effective"],
   "new_single_writer_still_refused":h_single_refused=="true",
   "verdict":"a pre-record binary is outside the protocol: it cannot be bound by the mode record, and its write is measured here rather than hidden. This is the documented residual risk; `min_reader` is the forward mechanism.",
 },
 "no_dual_authority":{
   "conforming_binaries":all([
     b_single=="true", d_single=="true", d_opt=="true", d_down=="true",
     e_refused=="true", c_blocked=="true",
   ]) and all(row["single_authority"] for row in json.loads(f_rows)),
   "mechanism":"one record, one CAS, every writable open checked before the lock",
   "window": "none for a conforming writer: before the CAS the record says `from` and after it `to`; a writer opening in either window is admitted only for the recorded mode",
   "outside_the_protocol":"a pre-record binary ignores the record (measured above)",
 },
 "failures":int(failures),
 "result":result,
 "evidence":{"log":log},
 "scope":{
   "label":"one host, one local POSIX namespace, process-fault proxies for the transition crash",
   "checked":"all six transition directions, live prepared transaction, crash at both sides of the CAS, a reader spanning the transition, and the v0.9.0 binary as an old reader and an old writer",
   "not_checked":"multi-node coordination, clock skew, a distributed quiescence barrier, Go parity for the migration verb",
 },
}
json.dump(doc,open(receipt,"w"),indent=2,sort_keys=True)
open(receipt,"a").write("\n")
print(json.dumps({"receipt":receipt,"result":result,"failures":int(failures),
 "no_dual_authority":doc["no_dual_authority"]["conforming_binaries"]}))
PY

echo "receipt: $receipt"
[[ "$result" == "pass" ]]
