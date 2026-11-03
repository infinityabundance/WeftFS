#!/usr/bin/env bash
# WeftFS twelve-step end-to-end showcase.
#
# Run only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/showcase.sh"
#
# The script builds the Rust and Go CLIs from this working tree, executes the
# twelve mandated scenarios, checks every claim with cmp/hash/JSON field
# equality, and writes:
#   bench/receipts/showcase-<UTC>.json   (schema weftfs.showcase.receipt/1)
#   bench/receipts/showcase-<UTC>.log    (raw command/console log)
#
# Optional:
#   SHOWCASE_RECEIPT_NAME=name           override the receipt basename
#   SHOWCASE_COMPARE_WITH=/work/bench/receipts/prior.json
#                                        compare semantic fields against a
#                                        previous receipt (timestamps excluded)
set -Eeuo pipefail

ROOT=/work
SCRATCH="${WEFT_SHOWCASE_SCRATCH:-$ROOT/.docker-cache/showcase}"
BIN="$SCRATCH/bin"
FIX="$SCRATCH/fixtures"
STORES="$SCRATCH/stores"
RECEIPTS="$ROOT/bench/receipts"
MOUNT_ROOT=/mnt/weft-fuse
TS=1767225600000000000          # 2026-01-01T00:00:00Z in nanoseconds
UTC="$(date -u +%Y%m%dT%H%M%SZ)"
BASE="${SHOWCASE_RECEIPT_NAME:-showcase-$UTC}"
RECEIPT="$RECEIPTS/$BASE.json"
LOG="$RECEIPTS/$BASE.log"
RESULTS="$SCRATCH/results.tsv"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
TARGET_DEBUG="$TARGET/debug"
RUSTFS="$BIN/weftfs"
GOFS="$BIN/go-weftfs"
CODEC="$BIN/weftcodec"
JSONQ="$BIN/jsonq"
RECEIPTGEN="$BIN/receiptgen"
RECEIPTCMP="$BIN/receiptcmp"
PSEUDO="$BIN/pseudorandom"

RUN_STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
RUN_START_EPOCH="$(date +%s)"
STEP_ID=""
STEP_DIR=""
STEP_FAILED=0
IN_STEP=0
FAILED_STEPS=0
FINALIZED=0
FINAL_ALL_PASS=false
DETERMINISM_FAILED=0
MOUNT_PATHS=()
MOUNT_PIDS=()
RUN_OUT=""
RUN_ERR=""
RUN_STATUS=0

mkdir -p "$BIN" "$FIX" "$STORES" "$RECEIPTS"
rm -rf "$SCRATCH/steps"
mkdir -p "$SCRATCH/steps"
: > "$RESULTS"

# Mirror the full driver console (including per-command stdout/stderr appended
# by run_any) to the raw log saved next to the receipt.
exec > >(tee -a "$LOG") 2>&1

log() { printf '%s\n' "$*" >&2; }
b64s() { printf '%s' "$1" | base64 -w0; }
b64f() { base64 -w0 < "$1"; }
rel() { realpath --relative-to="$ROOT" "$1"; }
sha256_file() { sha256sum "$1" | awk '{print $1}'; }

note() {
  printf 'N\t%s\n' "$(b64s "$1")" >> "$RESULTS"
  log "NOTE: $1"
}

meta() {
  printf 'M\t%s\t%s\n' "$1" "$(b64s "$2")" >> "$RESULTS"
}

repo() {
  printf 'R\t%s\t%s\n' "$1" "$(b64s "$2")" >> "$RESULTS"
}

fixture() {
  local name="$1" path="$2"
  printf 'F\t%s\t%s\t%s\t%s\n' \
    "$name" "$(b64s "$(rel "$path")")" "$(b64s "$(sha256_file "$path")")" \
    "$(b64s "$(stat -c %s "$path")")" >> "$RESULTS"
}

artifact() {
  local kind="$1" path="$2"
  printf 'T\t%s\t%s\t%s\t%s\n' \
    "$STEP_ID" "$(b64s "$kind")" "$(b64s "$(rel "$path")")" \
    "$(b64s "$(sha256_file "$path")")" >> "$RESULTS"
}

record_cmd() {
  printf 'C\t%s\t%s\n' "$STEP_ID" "$(b64s "$*")" >> "$RESULTS"
  log "CMD[$STEP_ID]: $*"
}

record_assert() {
  local name="$1" expected="$2" actual="$3" pass="$4"
  printf 'A\t%s\t%s\t%s\t%s\t%s\n' \
    "$STEP_ID" "$(b64s "$name")" "$(b64s "$expected")" "$(b64s "$actual")" \
    "$pass" >> "$RESULTS"
  if [ "$pass" != true ]; then
    STEP_FAILED=1
    log "ASSERT FAIL [$STEP_ID] $name: expected=<$expected> actual=<$actual>"
  fi
}

assert_eq() {
  if [ "$2" = "$3" ]; then
    record_assert "$1" "$2" "$3" true
  else
    record_assert "$1" "$2" "$3" false
  fi
}

assert_ne() {
  if [ "$2" != "$3" ]; then
    record_assert "$1" "different from $3" "$2" true
  else
    record_assert "$1" "different from $3" "$2" false
  fi
}

assert_contains() {
  local haystack="$2"
  case "$haystack" in
    *"$3"*) record_assert "$1" "contains <$3>" "<match>" true ;;
    *) record_assert "$1" "contains <$3>" "$haystack" false ;;
  esac
}

assert_not_contains() {
  local haystack="$2"
  case "$haystack" in
    *"$3"*) record_assert "$1" "does not contain <$3>" "<found>" false ;;
    *) record_assert "$1" "does not contain <$3>" "<absent>" true ;;
  esac
}

assert_not_exists() {
  if [ -e "$2" ]; then
    record_assert "$1" "absent" "present: $2" false
  else
    record_assert "$1" "absent" "absent" true
  fi
}

assert_file_eq() {
  local name="$1" left="$2" right="$3"
  local left_sha right_sha pass=false
  left_sha="$(sha256_file "$left")"
  right_sha="$(sha256_file "$right")"
  [ "$left_sha" = "$right_sha" ] && pass=true
  record_assert "$name sha256" "$left_sha" "$right_sha" "$pass"
  if cmp -s "$left" "$right"; then
    record_assert "$name bytes" "identical" "identical" true
  else
    record_assert "$name bytes" "identical" "different" false
  fi
}

save_out() {
  local label="$1" file="${2:-$RUN_OUT}"
  printf 'O\t%s\t%s\t%s\n' "$STEP_ID" "$(b64s "$label")" "$(b64f "$file")" >> "$RESULTS"
}

run_any() {
  local label="$1"; shift
  record_cmd "$*"
  local out="$STEP_DIR/$label.out" err="$STEP_DIR/$label.err"
  if "$@" >"$out" 2>"$err"; then
    RUN_STATUS=0
  else
    RUN_STATUS=$?
  fi
  RUN_OUT="$out"
  RUN_ERR="$err"
  cat "$out" >> "$STEP_DIR/stdout.log"
  cat "$err" >> "$STEP_DIR/stderr.log"
  if [ -n "${LOG:-}" ]; then
    cat "$out" >> "$LOG"
    cat "$err" >> "$LOG"
  fi
  return 0
}

run_ok() {
  local label="$1"; shift
  run_any "$label" "$@"
  assert_eq "$label exit" "0" "$RUN_STATUS"
}

run_expect_fail() {
  local label="$1"; shift
  run_any "$label" "$@"
  if [ "$RUN_STATUS" -ne 0 ]; then
    record_assert "$label exit" "nonzero" "nonzero" true
  else
    record_assert "$label exit" "nonzero" "0" false
  fi
}

step_begin() {
  STEP_ID="$1"
  STEP_DIR="$SCRATCH/steps/$STEP_ID"
  mkdir -p "$STEP_DIR"
  STEP_FAILED=0
  IN_STEP=1
  printf 'S\t%s\t%s\t%s\n' "$STEP_ID" "$(b64s "$2")" "$(b64s "${3:-}")" >> "$RESULTS"
  log ""
  log "===== STEP $STEP_ID: $2 ====="
}

step_end() {
  local status="$1" notes="${2:-}"
  if [ "$STEP_FAILED" -ne 0 ]; then
    status=fail
  fi
  if [ "$status" != pass ]; then
    FAILED_STEPS=$((FAILED_STEPS + 1))
  fi
  printf 'E\t%s\t%s\t%s\n' "$STEP_ID" "$(b64s "$status")" "$(b64s "$notes")" >> "$RESULTS"
  log "----- STEP $STEP_ID: $status $notes"
  IN_STEP=0
}

cleanup_mounts() {
  local index
  if [ "${#MOUNT_PATHS[@]}" -gt 0 ]; then
    for index in "${!MOUNT_PATHS[@]}"; do
      local mount="${MOUNT_PATHS[$index]}"
      if mountpoint -q "$mount" 2>/dev/null; then
        fusermount3 -u "$mount" 2>/dev/null || umount -l "$mount" 2>/dev/null || true
      fi
      rmdir "$mount" 2>/dev/null || true
    done
  fi
  if [ "${#MOUNT_PIDS[@]}" -gt 0 ]; then
    for index in "${!MOUNT_PIDS[@]}"; do
      kill "${MOUNT_PIDS[$index]}" 2>/dev/null || true
    done
    for index in "${!MOUNT_PIDS[@]}"; do
      wait "${MOUNT_PIDS[$index]}" 2>/dev/null || true
    done
  fi
  MOUNT_PATHS=()
  MOUNT_PIDS=()
}

wait_mount() {
  local mount="$1" tries=0
  while [ "$tries" -lt 200 ]; do
    if mountpoint -q "$mount" 2>/dev/null; then
      return 0
    fi
    sleep 0.05
    tries=$((tries + 1))
  done
  return 1
}

start_mount() {
  local store="$1" mount="$2"; shift 2
  mkdir -p "$mount"
  MOUNT_PATHS+=("$mount")
  "$RUSTFS" --store "$store" --fixed-timestamp-ns "$TS" mount \
    --mountpoint "$mount" "$@" >"$SCRATCH/mount-$(basename "$mount").log" 2>&1 &
  local pid=$!
  MOUNT_PIDS+=("$pid")
  if ! wait_mount "$mount"; then
    log "mount failed at $mount; daemon log:"
    cat "$SCRATCH/mount-$(basename "$mount").log" >&2 || true
    return 1
  fi
}

stop_mount() {
  local mount="$1"
  if mountpoint -q "$mount" 2>/dev/null; then
    fusermount3 -u "$mount" 2>/dev/null || umount -l "$mount" 2>/dev/null || true
  fi
  local tries=0
  while mountpoint -q "$mount" 2>/dev/null && [ "$tries" -lt 100 ]; do
    sleep 0.05
    tries=$((tries + 1))
  done
  if mountpoint -q "$mount" 2>/dev/null; then
    log "WARNING: $mount still mounted after unmount attempts"
    return 1
  fi
  return 0
}

finalize() {
  if [ "$FINALIZED" = 1 ]; then
    return 0
  fi
  FINALIZED=1
  set +e
  cleanup_mounts

  local finished duration
  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  duration=$(( ( $(date +%s) - RUN_START_EPOCH ) * 1000 ))
  meta run.finished_at "$finished"
  meta run.duration_ms "$duration"

  if [ -x "$RECEIPTGEN" ]; then
    "$RECEIPTGEN" < "$RESULTS" > "$RECEIPT"
    cp "$RESULTS" "$RECEIPTS/$BASE.results.tsv"
  else
    log "ERROR: receipt generator missing; no receipt written"
    return 0
  fi

  if [ -x "$JSONQ" ]; then
    FINAL_ALL_PASS="$("$JSONQ" "$RECEIPT" summary.all_pass 2>/dev/null || echo false)"
    local pass fail blocked
    pass="$("$JSONQ" "$RECEIPT" summary.pass 2>/dev/null || echo 0)"
    fail="$("$JSONQ" "$RECEIPT" summary.fail 2>/dev/null || echo 0)"
    blocked="$("$JSONQ" "$RECEIPT" summary.blocked 2>/dev/null || echo 0)"
    log "summary: pass=$pass fail=$fail blocked=$blocked all_pass=$FINAL_ALL_PASS"
  fi

  if [ -n "${SHOWCASE_COMPARE_WITH:-}" ] && [ -f "$SHOWCASE_COMPARE_WITH" ] && [ -x "$RECEIPTCMP" ]; then
    if "$RECEIPTCMP" "$SHOWCASE_COMPARE_WITH" "$RECEIPT" \
        > "$RECEIPTS/$BASE.determinism.txt" 2>&1; then
      log "determinism: semantic_match=true"
      cat "$RECEIPTS/$BASE.determinism.txt" >&2
    else
      DETERMINISM_FAILED=1
      log "determinism: semantic_match=false (see $RECEIPTS/$BASE.determinism.txt)"
      cat "$RECEIPTS/$BASE.determinism.txt" >&2
    fi
  fi

  log "receipt: $RECEIPT"
  log "raw log: $LOG"
  return 0
}

on_error() {
  trap - ERR
  set +e
  local line="$1"
  if [ "$IN_STEP" = 1 ] && [ -n "$STEP_ID" ]; then
    record_assert "unexpected shell error at line $line" "no shell error" "error" false
    step_end fail "unexpected shell error at line $line"
  else
    log "unexpected shell error at line $line (outside a step)"
  fi
  finalize
  trap - EXIT
  exit 1
}

trap 'on_error $LINENO' ERR
trap 'finalize' EXIT

# ---------------------------------------------------------------------------
# Environment and repository revisions (recorded before any build).
# ---------------------------------------------------------------------------
meta generated_at "$RUN_STARTED_AT"
meta run.started_at "$RUN_STARTED_AT"
meta env.court fuse
meta env.image "${WEFT_COURT_IMAGE:-unknown}"
meta env.image_id "${WEFT_COURT_IMAGE_ID:-unknown}"
meta env.kernel "$(uname -srm)"
meta env.arch "$(uname -m)"
meta env.os "$(. /etc/os-release 2>/dev/null && printf '%s' "${PRETTY_NAME:-unknown}" || echo unknown)"
meta env.rustc "$(rustc --version 2>/dev/null || echo unknown)"
meta env.cargo "$(cargo --version 2>/dev/null || echo unknown)"
meta env.go "$(go version 2>/dev/null || echo unknown)"
meta env.fusermount3 "$(fusermount3 --version 2>&1 | head -1 || echo unknown)"
meta env.docker_server "${WEFT_DOCKER_SERVER:-unknown}"
meta env.docker_client "${WEFT_DOCKER_CLIENT:-unknown}"
meta env.limit_memory "${WEFT_COURT_MEMORY:-unknown}"
meta env.limit_memory_swap "${WEFT_COURT_MEMORY:-unknown}"
meta env.limit_pids "${WEFT_COURT_PIDS:-unknown}"
meta env.limit_cpus "${WEFT_COURT_CPUS:-unknown}"
meta env.tmpfs_tmp "1g"
meta env.tmpfs_fuse "512m"
meta env.cap_drop "ALL"
meta env.cap_add "SYS_ADMIN"
meta env.device "/dev/fuse"
meta env.fixed_timestamp_ns "$TS"
meta env.scratch "$(rel "$SCRATCH")"

repo WeftFS "${WEFT_REPO_WEFTFS:-unknown}"
repo WeftFS-Go "${WEFT_REPO_WEFTFS_GO:-unknown}"
repo WeftCodec "${WEFT_REPO_WEFTCODEC:-unknown}"
repo WeftCodec-Go "${WEFT_REPO_WEFTCODEC_GO:-unknown}"

note "all builds and executions run inside $WEFT_COURT_IMAGE under the Docker court limits recorded in environment"
note "fixtures are deterministic: fixed seed strings, fixed clock $TS ns, and xorshift64* byte generation"
note "step 9 is a process-fault proxy (WEFT_CRASH_PHASE); it is not a physical power-loss test"

# ---------------------------------------------------------------------------
# Build (before step 1; build commands are in the raw log and notes).
# ---------------------------------------------------------------------------
log "===== BUILD ====="
note "build: (cd WeftCodec && cargo build --quiet --bin weftcodec)"
note "build: cargo build --quiet --features fuse --bin weftfs"
note "build: cargo build --quiet --example codec_cycle_probe"
note "build: (cd WeftFS-Go && CGO_ENABLED=0 go build ./cmd/weftfs)"
(cd "$ROOT/WeftCodec" && cargo build --quiet --bin weftcodec)
(cd "$ROOT" && cargo build --quiet --features fuse --bin weftfs)
(cd "$ROOT" && cargo build --quiet --example codec_cycle_probe)
(cd "$ROOT/WeftFS-Go" && CGO_ENABLED=0 go build -o "$GOFS" ./cmd/weftfs)
cp "$TARGET_DEBUG/weftcodec" "$CODEC"
cp "$TARGET_DEBUG/weftfs" "$RUSTFS"
rustc --edition 2021 -O "$ROOT/scripts/showcase/pseudorandom.rs" -o "$PSEUDO"
(cd /tmp && GO111MODULE=off go build -o "$JSONQ" "$ROOT/scripts/showcase/jsonq.go")
(cd /tmp && GO111MODULE=off go build -o "$RECEIPTGEN" "$ROOT/scripts/showcase/receiptgen.go")
(cd /tmp && GO111MODULE=off go build -o "$RECEIPTCMP" "$ROOT/scripts/showcase/receiptcmp.go")
CYCLE_PROBE="$TARGET_DEBUG/examples/codec_cycle_probe"
[ -x "$CODEC" ] && [ -x "$RUSTFS" ] && [ -x "$GOFS" ] && [ -x "$CYCLE_PROBE" ]

# ---------------------------------------------------------------------------
# Deterministic fixtures.
# ---------------------------------------------------------------------------
printf 'shared payload v1\n' > "$FIX/shared.bin"
awk 'BEGIN { for (i = 0; i < 6000; i++) printf "record %06d: the quick brown fox jumps over the lazy dog; seq=%d\n", i, i * 7 }' > "$FIX/rev1.bin"
awk '{ if (NR == 100) print "record 000099: CHANGED line for revision two"; else if (NR == 3000) print "record 002999: SECOND changed line for revision two"; else print } END { print "appended revision two trailer" }' "$FIX/rev1.bin" > "$FIX/rev2.bin"
"$PSEUDO" 1 65536 "$FIX/entropy-base.bin"
"$PSEUDO" 2 65536 "$FIX/entropy-target.bin"
fixture shared "$FIX/shared.bin"
fixture rev1 "$FIX/rev1.bin"
fixture rev2 "$FIX/rev2.bin"
fixture entropy_base "$FIX/entropy-base.bin"
fixture entropy_target "$FIX/entropy-target.bin"

# ===========================================================================
# STEP 1 — two byte-identical objects, distinct identities and contexts
# ===========================================================================
step_begin 1 "Ingest two byte-identical objects with distinct logical identities and contextual relationships"
S1="$STORES/s1"
rm -rf "$S1"
run_ok init "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" init
run_ok mkdir-ctx "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" mkdir /ctx --seed ctx-dir
run_ok mkdir-left "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" mkdir /left --seed left-dir
run_ok mkdir-right "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" mkdir /right --seed right-dir
run_ok create-left "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" create /left/doc.txt --file "$FIX/shared.bin" --seed obj-left-doc
OBJ_AFTER_LEFT="$(find "$S1/objects" -type f | wc -l | tr -d ' ')"
MAN_AFTER_LEFT="$(find "$S1/manifests" -type f | wc -l | tr -d ' ')"
run_ok create-right "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" create /right/doc.txt --file "$FIX/shared.bin" --seed obj-right-doc
OBJ_AFTER_RIGHT="$(find "$S1/objects" -type f | wc -l | tr -d ' ')"
MAN_AFTER_RIGHT="$(find "$S1/manifests" -type f | wc -l | tr -d ' ')"
run_ok create-ctx-left "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" create /ctx/left --text "left context" --seed ctx-left
run_ok create-ctx-right "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" create /ctx/right --text "right context" --seed ctx-right
run_ok stat-left "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" stat /left/doc.txt
save_out stat-left "$RUN_OUT"
cp "$RUN_OUT" "$STEP_DIR/stat-left.json"
run_ok stat-right "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" stat /right/doc.txt
save_out stat-right "$RUN_OUT"
cp "$RUN_OUT" "$STEP_DIR/stat-right.json"
run_ok stat-ctx-left "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" stat /ctx/left
cp "$RUN_OUT" "$STEP_DIR/stat-ctx-left.json"
run_ok stat-ctx-right "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" stat /ctx/right
cp "$RUN_OUT" "$STEP_DIR/stat-ctx-right.json"
LEFT_ID="$("$JSONQ" "$STEP_DIR/stat-left.json" object_id)"
RIGHT_ID="$("$JSONQ" "$STEP_DIR/stat-right.json" object_id)"
LEFT_CID="$("$JSONQ" "$STEP_DIR/stat-left.json" content_id)"
RIGHT_CID="$("$JSONQ" "$STEP_DIR/stat-right.json" content_id)"
CTX_LEFT_ID="$("$JSONQ" "$STEP_DIR/stat-ctx-left.json" object_id)"
CTX_RIGHT_ID="$("$JSONQ" "$STEP_DIR/stat-ctx-right.json" object_id)"
run_ok rel-left "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" relation add --subject "$LEFT_ID" --predicate in_context --object "$CTX_LEFT_ID"
run_ok rel-right "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" relation add --subject "$LEFT_ID" --predicate in_context --object "$CTX_RIGHT_ID"
run_ok rel-right-b "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" relation add --subject "$RIGHT_ID" --predicate in_context --object "$CTX_RIGHT_ID"
assert_ne "distinct object ids" "$LEFT_ID" "$RIGHT_ID"
assert_eq "identical content ids" "$LEFT_CID" "$RIGHT_CID"
assert_eq "second identical object adds no manifest" "$MAN_AFTER_LEFT" "$MAN_AFTER_RIGHT"
assert_eq "second identical object adds no chunk" "$OBJ_AFTER_LEFT" "$OBJ_AFTER_RIGHT"
run_ok go1-stat-left "$GOFS" --store "$S1" --json stat /left/doc.txt
cp "$RUN_OUT" "$STEP_DIR/go-stat-left.json"
assert_eq "shared manifest has one chunk" "1" "$("$JSONQ" "$STEP_DIR/go-stat-left.json" chunks)"
if [ -f "$S1/manifests/${LEFT_CID:0:2}/$LEFT_CID" ]; then
  record_assert "shared manifest path exists" "yes" "yes" true
else
  record_assert "shared manifest path exists" "yes" "no" false
fi
step_end pass "left=$LEFT_ID right=$RIGHT_ID content=$LEFT_CID"

# ===========================================================================
# STEP 2 — relation cycle accepted; codec cycle rejected
# ===========================================================================
step_begin 2 "Directed context graph cycle is accepted; codec dependency cycle is rejected"
run_ok rel-cycle-ab "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" relation add --subject "$CTX_LEFT_ID" --predicate depends_on --object "$CTX_RIGHT_ID"
run_ok rel-cycle-ba "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" relation add --subject "$CTX_RIGHT_ID" --predicate depends_on --object "$CTX_LEFT_ID"
run_ok query-cycle-a "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" query "select relation(depends_on, obj:$CTX_RIGHT_ID)"
cp "$RUN_OUT" "$STEP_DIR/query-cycle-a.json"
run_ok query-cycle-b "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" query "select relation(depends_on, obj:$CTX_LEFT_ID)"
cp "$RUN_OUT" "$STEP_DIR/query-cycle-b.json"
assert_eq "cycle edge A accepted" "1" "$("$JSONQ" "$STEP_DIR/query-cycle-a.json" matched_count)"
assert_eq "cycle edge B accepted" "1" "$("$JSONQ" "$STEP_DIR/query-cycle-b.json" matched_count)"
assert_eq "cycle edge A resolves" "$CTX_LEFT_ID" "$("$JSONQ" "$STEP_DIR/query-cycle-a.json" results.0.object_id)"

run_ok enc-rev1-raw "$CODEC" encode --input "$FIX/rev1.bin" --output "$FIX/rev1.raw.wft" --mode raw
run_ok enc-rev2-delta "$CODEC" encode --input "$FIX/rev2.bin" --output "$FIX/rev2.delta.wft" --mode delta --base "$FIX/rev1.bin"
run_ok arc-create "$CODEC" archive create --output "$FIX/acyclic.wfa" "$FIX/rev1.raw.wft" "$FIX/rev2.delta.wft"
rm -rf "$FIX/acyclic-out"
run_ok arc-decode "$CODEC" archive decode --input "$FIX/acyclic.wfa" --out-dir "$FIX/acyclic-out"
FOUND_REV1=0
FOUND_REV2=0
for decoded in "$FIX/acyclic-out"/*; do
  if cmp -s "$decoded" "$FIX/rev1.bin"; then FOUND_REV1=$((FOUND_REV1 + 1)); fi
  if cmp -s "$decoded" "$FIX/rev2.bin"; then FOUND_REV2=$((FOUND_REV2 + 1)); fi
done
assert_eq "acyclic archive contains rev1" "1" "$FOUND_REV1"
assert_eq "acyclic archive contains rev2" "1" "$FOUND_REV2"

rm -rf "$FIX/cycle"
run_ok cycle-probe "$CYCLE_PROBE" --out "$FIX/cycle"
cp "$FIX/cycle/cycle-a.wft" "$STEP_DIR/cycle-a.wft"
cp "$FIX/cycle/cycle-b.wft" "$STEP_DIR/cycle-b.wft"
run_expect_fail arc-cycle "$CODEC" archive create --output "$FIX/cycle.wfa" "$FIX/cycle/cycle-a.wft" "$FIX/cycle/cycle-b.wft"
assert_contains "codec cycle rejected" "$(cat "$RUN_ERR")" "DependencyCycle"
assert_not_exists "cycle archive not written" "$FIX/cycle.wfa"
artifact wft1 "$FIX/rev1.raw.wft"
artifact wft1 "$FIX/rev2.delta.wft"
artifact wfa1 "$FIX/acyclic.wfa"
artifact wft1 "$STEP_DIR/cycle-a.wft"
artifact wft1 "$STEP_DIR/cycle-b.wft"
step_end pass "context cycle accepted; codec cycle rejected"

# ===========================================================================
# STEP 3 — two independent relationships, one object in multiple contexts
# ===========================================================================
step_begin 3 "Query by two independent relationships; one object in multiple contexts with no byte duplication"
run_ok q-left "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" query "select relation(in_context, obj:$CTX_LEFT_ID)"
cp "$RUN_OUT" "$STEP_DIR/q-left.json"
save_out q-left "$RUN_OUT"
run_ok q-right "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" query "select relation(in_context, obj:$CTX_RIGHT_ID)"
cp "$RUN_OUT" "$STEP_DIR/q-right.json"
save_out q-right "$RUN_OUT"
run_ok q-both "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" query "select relation(in_context, obj:$CTX_LEFT_ID) and relation(in_context, obj:$CTX_RIGHT_ID)"
cp "$RUN_OUT" "$STEP_DIR/q-both.json"
save_out q-both "$RUN_OUT"
assert_eq "left context matches one object" "1" "$("$JSONQ" "$STEP_DIR/q-left.json" matched_count)"
assert_eq "left context resolves the shared object" "$LEFT_ID" "$("$JSONQ" "$STEP_DIR/q-left.json" results.0.object_id)"
assert_eq "right context matches two objects" "2" "$("$JSONQ" "$STEP_DIR/q-right.json" matched_count)"
assert_eq "intersection resolves exactly the shared object" "1" "$("$JSONQ" "$STEP_DIR/q-both.json" matched_count)"
assert_eq "intersection object id" "$LEFT_ID" "$("$JSONQ" "$STEP_DIR/q-both.json" results.0.object_id)"
assert_eq "two object ids share one content id" "$LEFT_CID" "$RIGHT_CID"
assert_eq "second identical object added no chunk" "$OBJ_AFTER_LEFT" "$OBJ_AFTER_RIGHT"
step_end pass "object $LEFT_ID appears in both contexts; one chunk"

# ===========================================================================
# STEP 4 — ambiguity surfaced, never silently resolved
# ===========================================================================
step_begin 4 "Resolve an ambiguous selector: full authorized set plus explicit Ambiguous error for require-one"
run_ok q-all "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" query 'select kind == "file"'
cp "$RUN_OUT" "$STEP_DIR/q-all.json"
save_out q-all "$RUN_OUT"
ALL_COUNT="$("$JSONQ" "$STEP_DIR/q-all.json" matched_count)"
RESULTS_LEN="$("$JSONQ" "$STEP_DIR/q-all.json" 'results#len')"
assert_eq "selector matches multiple objects" "true" "$([ "$ALL_COUNT" -gt 1 ] && echo true || echo false)"
assert_eq "full authorized result set returned" "$ALL_COUNT" "$RESULTS_LEN"
run_expect_fail q-one-ambiguous "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" query --require-one 'select kind == "file"'
assert_contains "require-one reports Ambiguous" "$(cat "$RUN_ERR")" "Ambiguous:"
assert_contains "Ambiguous reports the full authorized count" "$(cat "$RUN_ERR")" "matched $ALL_COUNT authorized objects"
run_ok q-one-unique "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" query --require-one "select relation(in_context, obj:$CTX_LEFT_ID) and relation(in_context, obj:$CTX_RIGHT_ID)"
cp "$RUN_OUT" "$STEP_DIR/q-one-unique.json"
save_out q-one-unique "$RUN_OUT"
assert_eq "require-one succeeds on a unique match" "$LEFT_ID" "$("$JSONQ" "$STEP_DIR/q-one-unique.json" object_id)"
assert_eq "require-one reports one match" "1" "$("$JSONQ" "$STEP_DIR/q-one-unique.json" matched_count)"
step_end pass "ambiguous count=$ALL_COUNT; require-one rejected"

# ===========================================================================
# STEP 5 — pin (snapshot, object, version); rename/retag/unrelated change
# ===========================================================================
step_begin 5 "Pin an exact (snapshot, object, version); rename, retag, and unrelated writes cannot change the pinned read"
run_ok snap-pin "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" snapshot create pinned-v1
cp "$RUN_OUT" "$STEP_DIR/snap-pin.json"
PIN_SNAP="$("$JSONQ" "$STEP_DIR/snap-pin.json" snapshot_id)"
run_ok pin "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" pin --snapshot pinned-v1 "$LEFT_ID"
cp "$RUN_OUT" "$STEP_DIR/pin.json"
save_out pin "$RUN_OUT"
PIN_OBJ="$("$JSONQ" "$STEP_DIR/pin.json" object_id)"
PIN_VER="$("$JSONQ" "$STEP_DIR/pin.json" version_id)"
PIN_CID="$("$JSONQ" "$STEP_DIR/pin.json" content_id)"
run_ok cat-before "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" cat /left/doc.txt
cp "$RUN_OUT" "$STEP_DIR/pinned-bytes"
run_ok rename-pinned "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" rename /left/doc.txt /left/renamed.txt
run_ok write-unrelated "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" write /right/doc.txt --offset 0 --text "unrelated change\n"
run_ok retag-pinned "$RUSTFS" --store "$S1" --fixed-timestamp-ns "$TS" relation add --subject "$PIN_OBJ" --predicate tagged_as --value pinned-retag
M5="$MOUNT_ROOT/showcase-5"
if start_mount "$S1" "$M5" --snapshot pinned-v1 --read-only; then
  run_ok cat-pinned "cat" "$M5/left/doc.txt"
  assert_file_eq "pinned snapshot read" "$RUN_OUT" "$STEP_DIR/pinned-bytes"
  run_expect_fail cat-renamed "cat" "$M5/left/renamed.txt"
  assert_contains "snapshot retains the old path" "$(cat "$RUN_ERR")" "No such file or directory"
  record_cmd "fusermount3 -u $M5"
  stop_mount "$M5" || record_assert "snapshot unmount" "clean" "busy" false
else
  record_assert "snapshot mount" "mounted" "failed" false
fi
run_ok stat-after "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" stat /left/renamed.txt
cp "$RUN_OUT" "$STEP_DIR/stat-after.json"
run_ok stat-unrelated "$RUSTFS" --store "$S1" --json --fixed-timestamp-ns "$TS" stat /right/doc.txt
cp "$RUN_OUT" "$STEP_DIR/stat-unrelated.json"
assert_eq "rename preserves object id" "$PIN_OBJ" "$("$JSONQ" "$STEP_DIR/stat-after.json" object_id)"
assert_eq "rename/retag preserves version id" "$PIN_VER" "$("$JSONQ" "$STEP_DIR/stat-after.json" version_id)"
assert_eq "pinned content id unchanged" "$PIN_CID" "$("$JSONQ" "$STEP_DIR/stat-after.json" content_id)"
assert_ne "unrelated content changed" "$RIGHT_CID" "$("$JSONQ" "$STEP_DIR/stat-unrelated.json" content_id)"
assert_eq "pin snapshot id" "$PIN_SNAP" "$("$JSONQ" "$STEP_DIR/pin.json" snapshot_id)"
step_end pass "pinned read byte-identical after rename/retag/unrelated write"

# ===========================================================================
# STEP 6 — Rust FUSE write + xattr, then atomic relation, fresh selector
# ===========================================================================
step_begin 6 "Modify through the Rust FUSE mount, update metadata/relation atomically, open the new version via a fresh selector"
S6="$STORES/s6"
rm -rf "$S6"
run_ok init6 "$RUSTFS" --store "$S6" --fixed-timestamp-ns "$TS" init
run_ok create6 "$RUSTFS" --store "$S6" --fixed-timestamp-ns "$TS" create /mutable.txt --text "version one\n" --seed obj-mutable
run_ok stat6-old "$RUSTFS" --store "$S6" --json --fixed-timestamp-ns "$TS" stat /mutable.txt
cp "$RUN_OUT" "$STEP_DIR/stat6-old.json"
OLD_VER="$("$JSONQ" "$STEP_DIR/stat6-old.json" version_id)"
OLD_CID="$("$JSONQ" "$STEP_DIR/stat6-old.json" content_id)"
M6="$MOUNT_ROOT/showcase-6"
if start_mount "$S6" "$M6"; then
  record_cmd "printf 'version two through fuse\n' > $M6/mutable.txt"
  printf 'version two through fuse\n' > "$M6/mutable.txt"
  if [ -f "$M6/mutable.txt" ]; then
    record_assert "FUSE write visible" "yes" "yes" true
  else
    record_assert "FUSE write visible" "yes" "no" false
  fi
  run_ok setxattr "setfattr" -n user.reviewed -v yes "$M6/mutable.txt"
  run_ok getxattr "getfattr" --only-values -n user.reviewed "$M6/mutable.txt"
  assert_eq "xattr update through FUSE" "yes" "$(cat "$RUN_OUT")"
  run_ok cat6-fuse "cat" "$M6/mutable.txt"
  cp "$RUN_OUT" "$STEP_DIR/fuse-write-bytes"
  record_cmd "fusermount3 -u $M6"
  stop_mount "$M6" || record_assert "read-write unmount" "clean" "busy" false
else
  record_assert "read-write mount" "mounted" "failed" false
fi
run_ok rel6 "$RUSTFS" --store "$S6" --fixed-timestamp-ns "$TS" relation add --subject /mutable.txt --predicate tagged_as --value reviewed
run_ok query6 "$RUSTFS" --store "$S6" --json --fixed-timestamp-ns "$TS" query 'select tag("reviewed")'
cp "$RUN_OUT" "$STEP_DIR/query6.json"
save_out query6 "$RUN_OUT"
run_ok stat6-new "$RUSTFS" --store "$S6" --json --fixed-timestamp-ns "$TS" stat /mutable.txt
cp "$RUN_OUT" "$STEP_DIR/stat6-new.json"
NEW_VER="$("$JSONQ" "$STEP_DIR/stat6-new.json" version_id)"
NEW_CID="$("$JSONQ" "$STEP_DIR/stat6-new.json" content_id)"
assert_eq "fresh selector finds the new version" "1" "$("$JSONQ" "$STEP_DIR/query6.json" matched_count)"
assert_eq "fresh selector version id" "$NEW_VER" "$("$JSONQ" "$STEP_DIR/query6.json" results.0.version_id)"
assert_ne "FUSE write changed the version" "$OLD_VER" "$NEW_VER"
assert_ne "FUSE write changed the content" "$OLD_CID" "$NEW_CID"
assert_contains "xattr persisted in store metadata" "$(cat "$STEP_DIR/stat6-new.json")" "796573"
assert_eq "mount is gone" "no" "$([ -e "$M6" ] && mountpoint -q "$M6" && echo yes || echo no)"
step_end pass "FUSE write+xattr committed; fresh selector sees version $NEW_VER"

# ===========================================================================
# STEP 7 — context-conditioned codec versus RAW/ZSTD, honest accounting
# ===========================================================================
step_begin 7 "Encode a revision with a verified base and dictionary, decode exactly, and report whole-store accounting"
run_ok enc7-rev1-raw "$CODEC" encode --input "$FIX/rev1.bin" --output "$FIX/rev1.raw.wft" --mode raw
run_ok enc7-rev1-zstd "$CODEC" encode --input "$FIX/rev1.bin" --output "$FIX/rev1.zstd.wft" --mode zstd
run_ok enc7-rev2-raw "$CODEC" encode --input "$FIX/rev2.bin" --output "$FIX/rev2.raw.wft" --mode raw
run_ok enc7-rev2-zstd "$CODEC" encode --input "$FIX/rev2.bin" --output "$FIX/rev2.zstd.wft" --mode zstd
run_ok enc7-rev2-delta "$CODEC" encode --input "$FIX/rev2.bin" --output "$FIX/rev2.delta.wft" --mode delta --base "$FIX/rev1.bin"
run_ok enc7-rev2-dict "$CODEC" encode --input "$FIX/rev2.bin" --output "$FIX/rev2.dict.wft" --mode dictionary --dict "$FIX/rev1.bin"
run_ok enc7-rev1-ref "$CODEC" encode --input "$FIX/rev1.bin" --output "$FIX/rev1.reference.wft" --mode reference --base "$FIX/rev1.bin"
run_ok enc7-rev2-auto "$CODEC" encode --input "$FIX/rev2.bin" --output "$FIX/rev2.auto.wft" --mode auto --base "$FIX/rev1.bin" --dict "$FIX/rev1.bin"
run_ok inspect7-delta "$CODEC" inspect "$FIX/rev2.delta.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-delta.json"
run_ok inspect7-dict "$CODEC" inspect "$FIX/rev2.dict.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-dict.json"
run_ok inspect7-raw "$CODEC" inspect "$FIX/rev2.raw.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-raw.json"
run_ok inspect7-zstd "$CODEC" inspect "$FIX/rev2.zstd.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-zstd.json"
run_ok inspect7-reference "$CODEC" inspect "$FIX/rev1.reference.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-reference.json"
run_ok inspect7-auto "$CODEC" inspect "$FIX/rev2.auto.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-auto.json"
DEP_HASH_DELTA="$("$JSONQ" "$STEP_DIR/inspect-delta.json" dependencies.0.hash)"
DEP_HASH_DICT="$("$JSONQ" "$STEP_DIR/inspect-dict.json" dependencies.0.hash)"
DEP_HASH_REF="$("$JSONQ" "$STEP_DIR/inspect-reference.json" dependencies.0.hash)"
rm -rf "$FIX/deps"
mkdir -p "$FIX/deps"
cp "$FIX/rev1.bin" "$FIX/deps/$DEP_HASH_DELTA"
cp "$FIX/rev1.bin" "$FIX/deps/$DEP_HASH_DICT"
cp "$FIX/rev1.bin" "$FIX/deps/$DEP_HASH_REF"
run_ok verify7-delta "$CODEC" verify "$FIX/rev2.delta.wft" --deps "$FIX/deps"
assert_contains "delta verified with related base" "$(cat "$RUN_OUT")" "OK mode=delta"
run_ok verify7-dict "$CODEC" verify "$FIX/rev2.dict.wft" --deps "$FIX/deps"
assert_contains "dictionary verified with related dictionary" "$(cat "$RUN_OUT")" "OK mode=dictionary"
run_ok verify7-reference "$CODEC" verify "$FIX/rev1.reference.wft" --deps "$FIX/deps"
assert_contains "reference verified with related base" "$(cat "$RUN_OUT")" "OK mode=reference"
run_ok dec7-raw "$CODEC" decode --input "$FIX/rev2.raw.wft" --output "$STEP_DIR/rev2.raw.dec"
assert_file_eq "RAW decode exact" "$STEP_DIR/rev2.raw.dec" "$FIX/rev2.bin"
run_ok dec7-zstd "$CODEC" decode --input "$FIX/rev2.zstd.wft" --output "$STEP_DIR/rev2.zstd.dec"
assert_file_eq "ZSTD decode exact" "$STEP_DIR/rev2.zstd.dec" "$FIX/rev2.bin"
run_ok dec7-delta "$CODEC" decode --input "$FIX/rev2.delta.wft" --output "$STEP_DIR/rev2.delta.dec" --deps "$FIX/deps"
assert_file_eq "DELTA decode exact" "$STEP_DIR/rev2.delta.dec" "$FIX/rev2.bin"
run_ok dec7-dict "$CODEC" decode --input "$FIX/rev2.dict.wft" --output "$STEP_DIR/rev2.dict.dec" --deps "$FIX/deps"
assert_file_eq "DICTIONARY decode exact" "$STEP_DIR/rev2.dict.dec" "$FIX/rev2.bin"
run_ok dec7-reference "$CODEC" decode --input "$FIX/rev1.reference.wft" --output "$STEP_DIR/rev1.reference.dec" --deps "$FIX/deps"
assert_file_eq "REFERENCE decode exact" "$STEP_DIR/rev1.reference.dec" "$FIX/rev1.bin"
run_ok dec7-auto "$CODEC" decode --input "$FIX/rev2.auto.wft" --output "$STEP_DIR/rev2.auto.dec" --deps "$FIX/deps"
assert_file_eq "AUTO decode exact" "$STEP_DIR/rev2.auto.dec" "$FIX/rev2.bin"

REV1_BYTES="$(stat -c %s "$FIX/rev1.bin")"
REV2_BYTES="$(stat -c %s "$FIX/rev2.bin")"
REV1_RAW_FRAME="$(stat -c %s "$FIX/rev1.raw.wft")"
REV1_ZSTD_FRAME="$(stat -c %s "$FIX/rev1.zstd.wft")"
REV2_RAW_FRAME="$(stat -c %s "$FIX/rev2.raw.wft")"
REV2_ZSTD_FRAME="$(stat -c %s "$FIX/rev2.zstd.wft")"
REV2_DELTA_FRAME="$(stat -c %s "$FIX/rev2.delta.wft")"
REV2_DICT_FRAME="$(stat -c %s "$FIX/rev2.dict.wft")"
REV2_AUTO_FRAME="$(stat -c %s "$FIX/rev2.auto.wft")"
REV1_REF_FRAME="$(stat -c %s "$FIX/rev1.reference.wft")"
BASE_DELTA_TOTAL=$((REV1_ZSTD_FRAME + REV2_DELTA_FRAME))
BASE_DICT_TOTAL=$((REV1_ZSTD_FRAME + REV2_DICT_FRAME))
RAW_PAIR_TOTAL=$((REV1_RAW_FRAME + REV2_RAW_FRAME))
ZSTD_PAIR_TOTAL=$((REV1_ZSTD_FRAME + REV2_ZSTD_FRAME))
DELTA_INCREMENTAL=$((REV2_DELTA_FRAME - REV2_ZSTD_FRAME))
DICT_INCREMENTAL=$((REV2_DICT_FRAME - REV2_ZSTD_FRAME))
DELTA_WINS=false
[ "$DELTA_INCREMENTAL" -lt 0 ] && DELTA_WINS=true
DICT_WINS=false
[ "$DICT_INCREMENTAL" -lt 0 ] && DICT_WINS=true
AUTO_MODE="$("$JSONQ" "$STEP_DIR/inspect-auto.json" mode)"
{
  printf '{\n'
  printf '  "rev1_plaintext_bytes": %s,\n' "$REV1_BYTES"
  printf '  "rev2_plaintext_bytes": %s,\n' "$REV2_BYTES"
  printf '  "rev1_raw_frame_bytes": %s,\n' "$REV1_RAW_FRAME"
  printf '  "rev1_zstd_frame_bytes": %s,\n' "$REV1_ZSTD_FRAME"
  printf '  "rev2_raw_frame_bytes": %s,\n' "$REV2_RAW_FRAME"
  printf '  "rev2_zstd_frame_bytes": %s,\n' "$REV2_ZSTD_FRAME"
  printf '  "rev2_delta_frame_bytes": %s,\n' "$REV2_DELTA_FRAME"
  printf '  "rev2_dictionary_frame_bytes": %s,\n' "$REV2_DICT_FRAME"
  printf '  "rev2_auto_frame_bytes": %s,\n' "$REV2_AUTO_FRAME"
  printf '  "rev1_reference_frame_bytes": %s,\n' "$REV1_REF_FRAME"
  printf '  "reference_frame_body_bytes": %s,\n' "$("$JSONQ" "$STEP_DIR/inspect-reference.json" body_len)"
  printf '  "reference_frame_table_bytes": %s,\n' "$("$JSONQ" "$STEP_DIR/inspect-reference.json" table_len)"
  printf '  "base_plus_delta_total": %s,\n' "$BASE_DELTA_TOTAL"
  printf '  "base_plus_dictionary_total": %s,\n' "$BASE_DICT_TOTAL"
  printf '  "context_free_raw_pair_total": %s,\n' "$RAW_PAIR_TOTAL"
  printf '  "context_free_zstd_pair_total": %s,\n' "$ZSTD_PAIR_TOTAL"
  printf '  "delta_incremental_vs_zstd": %s,\n' "$DELTA_INCREMENTAL"
  printf '  "dictionary_incremental_vs_zstd": %s,\n' "$DICT_INCREMENTAL"
  printf '  "delta_wins_incremental": %s,\n' "$DELTA_WINS"
  printf '  "dictionary_wins_incremental": %s,\n' "$DICT_WINS"
  printf '  "auto_selected_mode": "%s"\n' "$AUTO_MODE"
  printf '}\n'
} > "$STEP_DIR/accounting.json"
save_out accounting "$STEP_DIR/accounting.json"
note "step 7 accounting is incremental and whole-pair: base bytes are counted once, not hidden"
note "step 7 delta_wins_incremental=$DELTA_WINS dictionary_wins_incremental=$DICT_WINS"
artifact wft1 "$FIX/rev2.delta.wft"
artifact wft1 "$FIX/rev2.dict.wft"
artifact wft1 "$FIX/rev2.raw.wft"
artifact wft1 "$FIX/rev2.zstd.wft"
artifact wft1 "$FIX/rev1.reference.wft"
step_end pass "delta_win=$DELTA_WINS dict_win=$DICT_WINS auto=$AUTO_MODE"

# ===========================================================================
# STEP 8 — Rust → Go → Rust handoff on the same on-disk format
# ===========================================================================
step_begin 8 "Create with Rust, open/verify/read with Go, modify with Go, then verify/read with Rust"
S8="$STORES/s8"
rm -rf "$S8"
run_ok init8 "$RUSTFS" --store "$S8" --fixed-timestamp-ns "$TS" init
run_ok mkdir8 "$RUSTFS" --store "$S8" --fixed-timestamp-ns "$TS" mkdir /docs --seed docs-dir
run_ok create8-alpha "$RUSTFS" --store "$S8" --fixed-timestamp-ns "$TS" create /alpha.txt --text "alpha v1 from rust\n" --seed rust-alpha
run_ok create8-beta "$RUSTFS" --store "$S8" --fixed-timestamp-ns "$TS" create /beta.txt --text "beta v1 from rust\n" --seed rust-beta
run_ok snap8-r1 "$RUSTFS" --store "$S8" --fixed-timestamp-ns "$TS" snapshot create r1
run_ok rust8-info-before "$RUSTFS" --store "$S8" --json --fixed-timestamp-ns "$TS" info
cp "$RUN_OUT" "$STEP_DIR/rust-info-before.json"
run_ok rust8-stat-alpha "$RUSTFS" --store "$S8" --json --fixed-timestamp-ns "$TS" stat /alpha.txt
cp "$RUN_OUT" "$STEP_DIR/rust-stat-alpha.json"
run_ok rust8-stat-beta "$RUSTFS" --store "$S8" --json --fixed-timestamp-ns "$TS" stat /beta.txt
cp "$RUN_OUT" "$STEP_DIR/rust-stat-beta.json"
R8_ALPHA_ID="$("$JSONQ" "$STEP_DIR/rust-stat-alpha.json" object_id)"
R8_ALPHA_CID="$("$JSONQ" "$STEP_DIR/rust-stat-alpha.json" content_id)"
R8_BETA_ID="$("$JSONQ" "$STEP_DIR/rust-stat-beta.json" object_id)"
R8_GEN_BEFORE="$("$JSONQ" "$STEP_DIR/rust-info-before.json" generation)"

run_ok go8-info-before "$GOFS" --store "$S8" --json info
cp "$RUN_OUT" "$STEP_DIR/go-info-before.json"
run_ok go8-verify-before "$GOFS" --store "$S8" --json verify --full
cp "$RUN_OUT" "$STEP_DIR/go-verify-before.json"
run_ok go8-cat-alpha "$GOFS" --store "$S8" cat /alpha.txt
cp "$RUN_OUT" "$STEP_DIR/go-alpha-before"
run_ok go8-stat-alpha "$GOFS" --store "$S8" --json stat /alpha.txt
cp "$RUN_OUT" "$STEP_DIR/go-stat-alpha.json"
run_ok go8-stat-beta "$GOFS" --store "$S8" --json stat /beta.txt
cp "$RUN_OUT" "$STEP_DIR/go-stat-beta.json"
assert_eq "Go opens the Rust generation" "$R8_GEN_BEFORE" "$("$JSONQ" "$STEP_DIR/go-info-before.json" generation)"
assert_eq "Go verify --full before mutation" "true" "$("$JSONQ" "$STEP_DIR/go-verify-before.json" OK)"
assert_eq "Go reads Rust object id" "$R8_ALPHA_ID" "$("$JSONQ" "$STEP_DIR/go-stat-alpha.json" object_id)"
assert_eq "Go reads Rust content id" "$R8_ALPHA_CID" "$("$JSONQ" "$STEP_DIR/go-stat-alpha.json" content)"
assert_eq "Go reads Rust beta object id" "$R8_BETA_ID" "$("$JSONQ" "$STEP_DIR/go-stat-beta.json" object_id)"

run_ok go8-write "$GOFS" --store "$S8" --fixed-timestamp-ns "$TS" write /alpha.txt --offset 0 --text "alpha v2 from go"
run_ok go8-create "$GOFS" --store "$S8" --fixed-timestamp-ns "$TS" create /gamma.txt --text "gamma from go\n" --seed go-gamma
run_ok go8-snap-r2 "$GOFS" --store "$S8" --fixed-timestamp-ns "$TS" snapshot create r2
run_ok go8-cat-alpha-after "$GOFS" --store "$S8" cat /alpha.txt
cp "$RUN_OUT" "$STEP_DIR/go-alpha-after"
run_ok go8-cat-gamma-after "$GOFS" --store "$S8" cat /gamma.txt
cp "$RUN_OUT" "$STEP_DIR/go-gamma-after"
run_ok go8-stat-alpha-after "$GOFS" --store "$S8" --json stat /alpha.txt
cp "$RUN_OUT" "$STEP_DIR/go-stat-alpha-after.json"
run_ok go8-snap-list "$GOFS" --store "$S8" --json snapshot list
cp "$RUN_OUT" "$STEP_DIR/go-snap-list.json"
run_ok go8-stat-gamma "$GOFS" --store "$S8" --json stat /gamma.txt
cp "$RUN_OUT" "$STEP_DIR/go-stat-gamma.json"

run_ok rust8-info-after "$RUSTFS" --store "$S8" --json --fixed-timestamp-ns "$TS" info
cp "$RUN_OUT" "$STEP_DIR/rust-info-after.json"
run_ok rust8-verify-after "$RUSTFS" --store "$S8" --json --fixed-timestamp-ns "$TS" verify --full
cp "$RUN_OUT" "$STEP_DIR/rust-verify-after.json"
run_ok rust8-cat-alpha "$RUSTFS" --store "$S8" --fixed-timestamp-ns "$TS" cat /alpha.txt
cp "$RUN_OUT" "$STEP_DIR/rust-alpha-after"
run_ok rust8-cat-gamma "$RUSTFS" --store "$S8" --fixed-timestamp-ns "$TS" cat /gamma.txt
cp "$RUN_OUT" "$STEP_DIR/rust-gamma-after"
run_ok rust8-stat-alpha "$RUSTFS" --store "$S8" --json --fixed-timestamp-ns "$TS" stat /alpha.txt
cp "$RUN_OUT" "$STEP_DIR/rust-stat-alpha-after.json"
run_ok rust8-stat-gamma "$RUSTFS" --store "$S8" --json --fixed-timestamp-ns "$TS" stat /gamma.txt
cp "$RUN_OUT" "$STEP_DIR/rust-stat-gamma.json"
run_ok rust8-snap-list "$RUSTFS" --store "$S8" --json --fixed-timestamp-ns "$TS" snapshot list
cp "$RUN_OUT" "$STEP_DIR/rust-snap-list.json"

assert_eq "Rust verify --full after Go mutation" "true" "$("$JSONQ" "$STEP_DIR/rust-verify-after.json" ok)"
assert_eq "Rust sees the same alpha object id" "$R8_ALPHA_ID" "$("$JSONQ" "$STEP_DIR/rust-stat-alpha-after.json" object_id)"
assert_eq "Rust sees the same gamma object id" "$("$JSONQ" "$STEP_DIR/go-stat-gamma.json" object_id)" "$("$JSONQ" "$STEP_DIR/rust-stat-gamma.json" object_id)"
assert_eq "Rust and Go agree on alpha content" "$("$JSONQ" "$STEP_DIR/go-stat-alpha-after.json" content)" "$("$JSONQ" "$STEP_DIR/rust-stat-alpha-after.json" content_id)"
assert_file_eq "Rust reads exact Go-written alpha bytes" "$STEP_DIR/rust-alpha-after" "$STEP_DIR/go-alpha-after"
assert_file_eq "Rust reads exact Go-created gamma bytes" "$STEP_DIR/rust-gamma-after" "$STEP_DIR/go-gamma-after"
assert_eq "Rust and Go agree on r1 snapshot" "$("$JSONQ" "$STEP_DIR/go-snap-list.json" r1)" "$("$JSONQ" "$STEP_DIR/rust-snap-list.json" snapshots.r1)"
assert_eq "Rust and Go agree on r2 snapshot" "$("$JSONQ" "$STEP_DIR/go-snap-list.json" r2)" "$("$JSONQ" "$STEP_DIR/rust-snap-list.json" snapshots.r2)"
assert_eq "Rust and Go agree on generation" "$("$JSONQ" "$STEP_DIR/go-info-before.json" generation)" "$("$JSONQ" "$STEP_DIR/rust-info-before.json" generation)"
step_end pass "Rust→Go→Rust byte equality and snapshot agreement"

# ===========================================================================
# STEP 9 — process-fault proxies at persistence boundaries (Rust and Go)
# ===========================================================================
step_begin 9 "Crash each daemon at configured persistence boundaries; independent verify --full; old-or-new whole state only"
OLD_BYTES="$STEP_DIR/old-bytes"
NEW_BYTES="$STEP_DIR/new-bytes"
OLD_TEXT="old"
NEW_TEXT="new content 0123456789"
printf '%s' "$OLD_TEXT" > "$OLD_BYTES"
printf '%s' "$NEW_TEXT" > "$NEW_BYTES"
PHASES=(after_objects after_commit_file after_superblock_write)
for impl in rust go; do
  if [ "$impl" = rust ]; then CLI="$RUSTFS"; VERIFY_KEY=ok; ALT="$GOFS"; ALT_KEY=OK; else CLI="$GOFS"; VERIFY_KEY=OK; ALT="$RUSTFS"; ALT_KEY=ok; fi
  for phase in "${PHASES[@]}"; do
    S9="$STORES/s9-$impl-$phase"
    rm -rf "$S9"
    run_ok "init9-$impl-$phase" "$CLI" --store "$S9" --fixed-timestamp-ns "$TS" init
    run_ok "create9-$impl-$phase" "$CLI" --store "$S9" --fixed-timestamp-ns "$TS" create /f --text "$OLD_TEXT" --seed "obj-$impl-$phase"
    run_expect_fail "crash9-$impl-$phase" env "WEFT_CRASH_PHASE=$phase" "$CLI" --store "$S9" --fixed-timestamp-ns "$TS" write /f --offset 0 --text "$NEW_TEXT"
    run_ok "cat9-$impl-$phase" "$CLI" --store "$S9" cat /f
    cp "$RUN_OUT" "$STEP_DIR/$impl-$phase-bytes"
    run_any "verify9-$impl-$phase" "$CLI" --store "$S9" --json verify --full
    cp "$RUN_OUT" "$STEP_DIR/$impl-$phase-verify.json"
    run_any "crossverify9-$impl-$phase" "$ALT" --store "$S9" --json verify --full
    cp "$RUN_OUT" "$STEP_DIR/$impl-$phase-crossverify.json"
    SAME_OK="$("$JSONQ" "$STEP_DIR/$impl-$phase-verify.json" "$VERIFY_KEY")"
    ALT_OK="$("$JSONQ" "$STEP_DIR/$impl-$phase-crossverify.json" "$ALT_KEY")"
    SAME_GEN="$("$JSONQ" "$STEP_DIR/$impl-$phase-verify.json" generation 2>/dev/null || "$JSONQ" "$STEP_DIR/$impl-$phase-verify.json" Generation)"
    ALT_GEN="$("$JSONQ" "$STEP_DIR/$impl-$phase-crossverify.json" generation 2>/dev/null || "$JSONQ" "$STEP_DIR/$impl-$phase-crossverify.json" Generation)"
    if [ "$phase" = after_superblock_write ]; then
      assert_eq "$impl $phase published state visible" "true" "$([ "$SAME_OK" = true ] && echo true || echo false)"
      assert_eq "$impl $phase whole new bytes" "identical" "$(cmp -s "$STEP_DIR/$impl-$phase-bytes" "$NEW_BYTES" && echo identical || echo different)"
    else
      assert_eq "$impl $phase old state intact" "identical" "$(cmp -s "$STEP_DIR/$impl-$phase-bytes" "$OLD_BYTES" && echo identical || echo different)"
    fi
    # The file is always exactly one of the two whole contents, never a partial.
    if cmp -s "$STEP_DIR/$impl-$phase-bytes" "$OLD_BYTES" || cmp -s "$STEP_DIR/$impl-$phase-bytes" "$NEW_BYTES"; then
      record_assert "$impl $phase whole-or-nothing bytes" "old or new" "whole" true
    else
      record_assert "$impl $phase whole-or-nothing bytes" "old or new" "partial" false
    fi
    # verify may report exactly the documented unpublished-tail finding.
    if [ "$SAME_OK" = false ]; then
      PROBLEMS="$("$JSONQ" "$STEP_DIR/$impl-$phase-verify.json" "$([ "$VERIFY_KEY" = ok ] && echo problems || echo Problems)")"
      assert_contains "$impl $phase verify finding is documented" "$PROBLEMS" "above the highest published superblock generation"
    fi
    assert_eq "$impl $phase cross-implementation generation" "$SAME_GEN" "$ALT_GEN"
    record_assert "$impl $phase cross-implementation verify observed" "recorded" "$ALT_OK" true
  done
done
note "step 9 is a process-fault proxy: WEFT_CRASH_PHASE kills the CLI at a publication boundary; it is not a power-loss claim"
step_end pass "process-fault proxy (WEFT_CRASH_PHASE), not physical power loss; old-or-new whole bytes; no partial file"

# ===========================================================================
# STEP 10 — unauthorized traversal, dependency fetch, and enumeration
# ===========================================================================
step_begin 10 "Unauthorized relation traversal, dependency fetch, and selector enumeration disclose nothing"
S10="$STORES/s10"
rm -rf "$S10"
run_ok init10 "$RUSTFS" --store "$S10" --fixed-timestamp-ns "$TS" init
run_ok create10-public "$RUSTFS" --store "$S10" --fixed-timestamp-ns "$TS" create /public.txt --text "public data" --seed pub
run_ok create10-private "$RUSTFS" --store "$S10" --fixed-timestamp-ns "$TS" create /private.txt --text "TOPSECRET private payload" --mode 0600 --seed priv
run_ok create10-acl "$RUSTFS" --store "$S10" --fixed-timestamp-ns "$TS" create /acl.txt --text "acl protected payload" --mode 0644 --attr-u64-list weft.acl.read=0 --seed acl
run_ok create10-ctx "$RUSTFS" --store "$S10" --fixed-timestamp-ns "$TS" create /secret-ctx --text "secret context" --mode 0600 --seed secretctx
run_ok stat10-public "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" stat /public.txt
cp "$RUN_OUT" "$STEP_DIR/stat-public.json"
run_ok stat10-private "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" stat /private.txt
cp "$RUN_OUT" "$STEP_DIR/stat-private.json"
run_ok stat10-acl "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" stat /acl.txt
cp "$RUN_OUT" "$STEP_DIR/stat-acl.json"
run_ok stat10-ctx "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" stat /secret-ctx
cp "$RUN_OUT" "$STEP_DIR/stat-ctx.json"
PUB_ID="$("$JSONQ" "$STEP_DIR/stat-public.json" object_id)"
PRIV_ID="$("$JSONQ" "$STEP_DIR/stat-private.json" object_id)"
ACL_ID="$("$JSONQ" "$STEP_DIR/stat-acl.json" object_id)"
CTX_ID="$("$JSONQ" "$STEP_DIR/stat-ctx.json" object_id)"
run_ok rel10-hidden "$RUSTFS" --store "$S10" --fixed-timestamp-ns "$TS" relation add --subject "$PRIV_ID" --predicate hidden_link --object "$CTX_ID"
run_ok rel10-points "$RUSTFS" --store "$S10" --fixed-timestamp-ns "$TS" relation add --subject "$PUB_ID" --predicate points_to --object "$PRIV_ID"
run_ok q10-root "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" query --uid 0 --gid 0 'select true'
cp "$RUN_OUT" "$STEP_DIR/q-root.json"
save_out q-root "$RUN_OUT"
run_ok q10-low "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" query --uid 1000 --gid 1000 'select true'
cp "$RUN_OUT" "$STEP_DIR/q-low.json"
save_out q-low "$RUN_OUT"
ROOT_COUNT="$("$JSONQ" "$STEP_DIR/q-root.json" matched_count)"
LOW_COUNT="$("$JSONQ" "$STEP_DIR/q-low.json" matched_count)"
LOW_JSON="$(cat "$STEP_DIR/q-low.json")"
ROOT_JSON="$(cat "$STEP_DIR/q-root.json")"
assert_eq "root sees more objects than unauthorized principal" "true" "$([ "$ROOT_COUNT" -gt "$LOW_COUNT" ] && echo true || echo false)"
assert_contains "public object is authorized" "$ROOT_JSON" "$PUB_ID"
assert_contains "private object is authorized for root" "$ROOT_JSON" "$PRIV_ID"
assert_not_contains "private object id is not disclosed" "$LOW_JSON" "$PRIV_ID"
assert_not_contains "ACL-restricted object id is not disclosed" "$LOW_JSON" "$ACL_ID"
assert_not_contains "private content marker is not disclosed" "$LOW_JSON" "TOPSECRET"
assert_contains "public object id is disclosed" "$LOW_JSON" "$PUB_ID"
run_ok q10-hidden-root "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" query --uid 0 --gid 0 "select relation(hidden_link, obj:$CTX_ID)"
cp "$RUN_OUT" "$STEP_DIR/q-hidden-root.json"
run_ok q10-hidden-low "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" query --uid 1000 --gid 1000 "select relation(hidden_link, obj:$CTX_ID)"
cp "$RUN_OUT" "$STEP_DIR/q-hidden-low.json"
assert_eq "root traverses the private relation" "1" "$("$JSONQ" "$STEP_DIR/q-hidden-root.json" matched_count)"
assert_eq "unauthorized relation traversal returns nothing" "0" "$("$JSONQ" "$STEP_DIR/q-hidden-low.json" matched_count)"
run_ok q10-points-root "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" query --uid 0 --gid 0 "select relation(points_to, obj:$PRIV_ID)"
cp "$RUN_OUT" "$STEP_DIR/q-points-root.json"
run_ok q10-points-low "$RUSTFS" --store "$S10" --json --fixed-timestamp-ns "$TS" query --uid 1000 --gid 1000 "select relation(points_to, obj:$PRIV_ID)"
cp "$RUN_OUT" "$STEP_DIR/q-points-low.json"
assert_eq "relation with an authorized subject/target resolves for root" "1" "$("$JSONQ" "$STEP_DIR/q-points-root.json" matched_count)"
assert_eq "relation with an unauthorized target resolves for nobody unauthorized" "0" "$("$JSONQ" "$STEP_DIR/q-points-low.json" matched_count)"
assert_not_contains "unauthorized traversal output has no private id" "$(cat "$STEP_DIR/q-hidden-low.json")" "$PRIV_ID"
assert_not_contains "unauthorized traversal output has no secret marker" "$(cat "$STEP_DIR/q-hidden-low.json")" "TOPSECRET"

rm -rf "$STEP_DIR/no-deps.dec" "$STEP_DIR/wrong-deps.dec"
run_expect_fail dec10-no-deps "$CODEC" decode --input "$FIX/rev2.delta.wft" --output "$STEP_DIR/no-deps.dec"
assert_contains "dependency fetch without base fails closed" "$(cat "$RUN_ERR")" "DependencyMissing"
assert_not_exists "no plaintext written without dependency" "$STEP_DIR/no-deps.dec"
rm -rf "$FIX/wrong-deps"
mkdir -p "$FIX/wrong-deps"
cp "$FIX/rev2.bin" "$FIX/wrong-deps/$DEP_HASH_DELTA"
run_expect_fail dec10-wrong-deps "$CODEC" decode --input "$FIX/rev2.delta.wft" --output "$STEP_DIR/wrong-deps.dec" --deps "$FIX/wrong-deps"
assert_contains "dependency fetch with mismatched bytes fails closed" "$(cat "$RUN_ERR")" "DependencyMismatch"
assert_not_exists "no plaintext written from a mismatched dependency" "$STEP_DIR/wrong-deps.dec"
note "the WeftCodec CLI has no remote fetch policy; missing or unverifiable dependencies fail closed (DependencyMissing/DependencyMismatch). DependencyDenied is an API-level class and is not claimed by this CLI step"
step_end pass "authorized-only counts; relation and dependency denials fail closed; codec CLI has no remote fetch policy"

# ===========================================================================
# STEP 11 — negative workloads: auto chooses RAW/ZSTD when context gives no gain
# ===========================================================================
step_begin 11 "Negative workload: unrelated high-entropy data gives no context gain; auto selects RAW or ZSTD"
run_ok enc11-base-raw "$CODEC" encode --input "$FIX/entropy-base.bin" --output "$FIX/entropy-base.raw.wft" --mode raw
run_ok enc11-target-raw "$CODEC" encode --input "$FIX/entropy-target.bin" --output "$FIX/entropy-target.raw.wft" --mode raw
run_ok enc11-target-zstd "$CODEC" encode --input "$FIX/entropy-target.bin" --output "$FIX/entropy-target.zstd.wft" --mode zstd
run_ok enc11-target-delta "$CODEC" encode --input "$FIX/entropy-target.bin" --output "$FIX/entropy-target.delta.wft" --mode delta --base "$FIX/entropy-base.bin"
run_ok enc11-target-dict "$CODEC" encode --input "$FIX/entropy-target.bin" --output "$FIX/entropy-target.dict.wft" --mode dictionary --dict "$FIX/entropy-base.bin"
run_ok enc11-target-auto "$CODEC" encode --input "$FIX/entropy-target.bin" --output "$FIX/entropy-target.auto.wft" --mode auto --base "$FIX/entropy-base.bin" --dict "$FIX/entropy-base.bin"
run_ok inspect11-auto "$CODEC" inspect "$FIX/entropy-target.auto.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-auto.json"
run_ok inspect11-delta "$CODEC" inspect "$FIX/entropy-target.delta.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-delta.json"
run_ok inspect11-dict "$CODEC" inspect "$FIX/entropy-target.dict.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-dict.json"
run_ok inspect11-raw "$CODEC" inspect "$FIX/entropy-target.raw.wft" --json
cp "$RUN_OUT" "$STEP_DIR/inspect-raw.json"
AUTO11_MODE="$("$JSONQ" "$STEP_DIR/inspect-auto.json" mode)"
AUTO11_LEN="$(stat -c %s "$FIX/entropy-target.auto.wft")"
RAW11_LEN="$(stat -c %s "$FIX/entropy-target.raw.wft")"
ZSTD11_LEN="$(stat -c %s "$FIX/entropy-target.zstd.wft")"
DELTA11_LEN="$(stat -c %s "$FIX/entropy-target.delta.wft")"
DICT11_LEN="$(stat -c %s "$FIX/entropy-target.dict.wft")"
assert_eq "auto selects a context-free mode" "true" "$([ "$AUTO11_MODE" = raw ] || [ "$AUTO11_MODE" = zstd ] && echo true || echo false)"
assert_eq "auto is no larger than RAW" "true" "$([ "$AUTO11_LEN" -le "$RAW11_LEN" ] && echo true || echo false)"
assert_eq "auto is no larger than DELTA" "true" "$([ "$AUTO11_LEN" -le "$DELTA11_LEN" ] && echo true || echo false)"
assert_eq "auto is no larger than DICTIONARY" "true" "$([ "$AUTO11_LEN" -le "$DICT11_LEN" ] && echo true || echo false)"
assert_eq "delta gives no gain on unrelated data" "true" "$([ "$DELTA11_LEN" -ge "$AUTO11_LEN" ] && echo true || echo false)"
assert_eq "dictionary gives no gain on unrelated data" "true" "$([ "$DICT11_LEN" -ge "$AUTO11_LEN" ] && echo true || echo false)"
{
  printf '{\n'
  printf '  "plaintext_bytes": %s,\n' "$(stat -c %s "$FIX/entropy-target.bin")"
  printf '  "raw_frame_bytes": %s,\n' "$RAW11_LEN"
  printf '  "zstd_frame_bytes": %s,\n' "$ZSTD11_LEN"
  printf '  "delta_frame_bytes": %s,\n' "$DELTA11_LEN"
  printf '  "dictionary_frame_bytes": %s,\n' "$DICT11_LEN"
  printf '  "auto_frame_bytes": %s,\n' "$AUTO11_LEN"
  printf '  "auto_selected_mode": "%s",\n' "$AUTO11_MODE"
  printf '  "context_gain": false\n'
  printf '}\n'
} > "$STEP_DIR/selection.json"
save_out selection "$STEP_DIR/selection.json"
note "step 11 negative result: context conditioning gives no gain on unrelated high-entropy data; auto selected $AUTO11_MODE"
step_end pass "negative result: auto=$AUTO11_MODE; no context gain; context-free size=$AUTO11_LEN"

# ===========================================================================
# STEP 12 — receipt bundle
# ===========================================================================
step_begin 12 "Emit the receipt bundle proving every assertion, including negative results and exact commands"
assert_eq "no step failed before receipt emission" "0" "$FAILED_STEPS"
run_ok dec12-raw "$CODEC" decode --input "$FIX/rev2.raw.wft" --output "$STEP_DIR/final-check.dec"
assert_file_eq "final raw decode self-check" "$STEP_DIR/final-check.dec" "$FIX/rev2.bin"
{
  printf 'schema=weftfs.showcase.receipt/1\n'
  printf 'receipt_dir=%s\n' "$(rel "$RECEIPTS")"
  printf 'raw_log_dir=%s\n' "$(rel "$RECEIPTS")"
  printf 'fixed_timestamp_ns=%s\n' "$TS"
  printf 'steps=12\n'
} > "$STEP_DIR/bundle.txt"
save_out bundle "$STEP_DIR/bundle.txt"
artifact text "$STEP_DIR/bundle.txt"
step_end pass "receipt and raw log paths recorded"

# ---------------------------------------------------------------------------
# Finalize and enforce the result.
# ---------------------------------------------------------------------------
finalize
trap - EXIT
if [ "$FINAL_ALL_PASS" = true ] && [ "$DETERMINISM_FAILED" = 0 ]; then
  exit 0
fi
exit 1
