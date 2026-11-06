#!/usr/bin/env bash
# Shared driver for the F3 demo courts.
#
# Every demo runs only inside the OOM-protected FUSE court:
#   scripts/docker-court.sh fuse "bash scripts/demo-<name>.sh"
#
# The driver records commands, machine-checked assertions, observed outputs,
# environment, repository revisions, and court limits as a TSV stream. The
# stdlib-only Go receipt generator turns that stream into the JSON receipt.
set -Eeuo pipefail

ROOT=/work
RECEIPTS="$ROOT/bench/receipts"
SCRATCH="${WEFT_DEMO_SCRATCH:-$ROOT/.docker-cache/f3-demos}"
BIN="$SCRATCH/bin"
STORES="$SCRATCH/stores"
FIXTURES="$SCRATCH/fixtures"
MOUNT_ROOT=/mnt/weft-fuse
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
TARGET_DEBUG="$TARGET/debug"
RUSTFS="$BIN/weftfs"
GOFS="$BIN/go-weftfs"
JSONQ="$BIN/jsonq"
RECEIPTGEN="$BIN/receiptgen"
DEMOCMP="$BIN/democmp"
MOUNT_PID_LAST=""
MOUNT_LOG_LAST=""

TS1=1767225600000000000          # 2026-01-01T00:00:00Z
TS2=1767225660000000000          # 2026-01-01T00:01:00Z
TS3=1767225720000000000          # 2026-01-01T00:02:00Z
UTC="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_ID="$$-$UTC"
BASE=""
RECEIPT=""
LOG=""
RESULTS=""
STEP_ID=""
STEP_DIR=""
STEP_FAILED=0
IN_STEP=0
FAILED_STEPS=0
FINALIZED=0
FINAL_ALL_PASS=false
MOUNT_PATHS=()
MOUNT_PIDS=()
MOUNT_PID_LAST=""
MOUNT_LOG_LAST=""
RUN_OUT=""
RUN_ERR=""
RUN_STATUS=0

mkdir -p "$BIN" "$STORES" "$FIXTURES" "$RECEIPTS" "$SCRATCH/steps"

log() { printf '%s\n' "$*" >&2; }
b64s() { printf '%s' "$1" | base64 -w0; }
b64f() { base64 -w0 < "$1"; }
rel() { realpath --relative-to="$ROOT" "$1"; }
sha256_file() { sha256sum "$1" | awk '{print $1}'; }
jsonq() { "$JSONQ" "$@"; }

note() {
  printf 'N\t%s\n' "$(b64s "$1")" >> "$RESULTS"
  log "NOTE: $1"
}

meta() { printf 'M\t%s\t%s\n' "$1" "$(b64s "$2")" >> "$RESULTS"; }
repo() { printf 'R\t%s\t%s\n' "$1" "$(b64s "$2")" >> "$RESULTS"; }
artifact_local() {
  local kind="$1" path="$2"
  printf 'T\t%s\t%s\t%s\t%s\n' "$STEP_ID" "$(b64s "$kind")" \
    "$(b64s "$(rel "$path")")" "$(b64s "$(sha256_file "$path")")" >> "$RESULTS"
}
record_cmd() {
  printf 'C\t%s\t%s\n' "$STEP_ID" "$(b64s "$*")" >> "$RESULTS"
  log "CMD[$STEP_ID]: $*"
}
record_assert() {
  local name="$1" expected="$2" actual="$3" pass="$4"
  printf 'A\t%s\t%s\t%s\t%s\t%s\n' "$STEP_ID" "$(b64s "$name")" "$(b64s "$expected")" \
    "$(b64s "$actual")" "$pass" >> "$RESULTS"
  if [ "$pass" != true ]; then
    STEP_FAILED=1
    log "ASSERT FAIL [$STEP_ID] $name: expected=<$expected> actual=<$actual>"
  fi
}
assert_eq() {
  if [ "$2" = "$3" ]; then record_assert "$1" "$2" "$3" true; else record_assert "$1" "$2" "$3" false; fi
}
assert_ne() {
  if [ "$2" != "$3" ]; then record_assert "$1" "different from $3" "$2" true; else record_assert "$1" "different from $3" "$2" false; fi
}
assert_contains() {
  case "$2" in
    *"$3"*) record_assert "$1" "contains <$3>" "<match>" true ;;
    *) record_assert "$1" "contains <$3>" "$2" false ;;
  esac
}
assert_not_exists() {
  if [ -e "$2" ]; then record_assert "$1" absent "present: $2" false; else record_assert "$1" absent absent true; fi
}
assert_sha256() {
  local name="$1" file="$2" expected="$3" actual
  if [ ! -e "$file" ]; then record_assert "$name" "$expected" "missing: $file" false; return; fi
  actual="$(sha256_file "$file")"
  if [ "$expected" = "$actual" ]; then record_assert "$name sha256" "$expected" "$actual" true; else record_assert "$name sha256" "$expected" "$actual" false; fi
}
assert_file_eq() {
  local name="$1" left="$2" right="$3" left_sha right_sha
  left_sha="$(sha256_file "$left")"; right_sha="$(sha256_file "$right")"
  if [ "$left_sha" = "$right_sha" ] && cmp -s "$left" "$right"; then
    record_assert "$name" "identical" "identical" true
  else
    record_assert "$name" "identical" "different" false
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
  if "$@" >"$out" 2>"$err"; then RUN_STATUS=0; else RUN_STATUS=$?; fi
  RUN_OUT="$out"; RUN_ERR="$err"
  cat "$out" >> "$STEP_DIR/stdout.log"; cat "$err" >> "$STEP_DIR/stderr.log"
  cat "$out" >> "$LOG"; cat "$err" >> "$LOG"
  return 0
}
run_ok() { local label="$1"; shift; run_any "$label" "$@"; assert_eq "$label exit" 0 "$RUN_STATUS"; }
run_expect_fail() {
  local label="$1"; shift; run_any "$label" "$@"
  if [ "$RUN_STATUS" -ne 0 ]; then record_assert "$label exit" nonzero nonzero true; else record_assert "$label exit" nonzero 0 false; fi
}
step_begin() {
  STEP_ID="$1"; STEP_DIR="$SCRATCH/steps/$RUN_ID/$STEP_ID"
  mkdir -p "$STEP_DIR"; STEP_FAILED=0; IN_STEP=1
  printf 'S\t%s\t%s\t%s\n' "$STEP_ID" "$(b64s "$2")" "$(b64s "${3:-}")" >> "$RESULTS"
  log ""; log "===== STEP $STEP_ID: $2 ====="
}
step_end() {
  local status="$1" notes="${2:-}"
  if [ "$STEP_FAILED" -ne 0 ]; then status=fail; fi
  if [ "$status" != pass ]; then FAILED_STEPS=$((FAILED_STEPS + 1)); fi
  printf 'E\t%s\t%s\t%s\n' "$STEP_ID" "$(b64s "$status")" "$(b64s "$notes")" >> "$RESULTS"
  log "----- STEP $STEP_ID: $status $notes"; IN_STEP=0
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
    for index in "${!MOUNT_PIDS[@]}"; do kill "${MOUNT_PIDS[$index]}" 2>/dev/null || true; done
    for index in "${!MOUNT_PIDS[@]}"; do wait "${MOUNT_PIDS[$index]}" 2>/dev/null || true; done
  fi
  MOUNT_PATHS=(); MOUNT_PIDS=()
}
wait_mount() {
  local mount="$1" tries=0 interval="${2:-0.05}" limit="${3:-300}"
  while [ "$tries" -lt "$limit" ]; do
    mountpoint -q "$mount" 2>/dev/null && return 0
    sleep "$interval"; tries=$((tries + 1))
  done
  return 1
}
# start_mount_async <rust|go> <store> <mount> [mount flags...]
# Spawns the foreground daemon and records its pid in MOUNT_PID_LAST.
# The caller waits with wait_mount (or a bulk poll) so concurrent mounts can
# overlap. At least one daemon must exist per mount; this never retries.
start_mount_async() {
  local impl="$1" store="$2" mount="$3"; shift 3
  mkdir -p "$mount"
  MOUNT_PATHS+=("$mount")
  local logfile="$SCRATCH/mount-$RUN_ID-$(basename "$mount").log"
  MOUNT_PID_LAST=""
  case "$impl" in
    rust)
      "$RUSTFS" --store "$store" --fixed-timestamp-ns "$TS1" mount \
        --mountpoint "$mount" --foreground "$@" >"$logfile" 2>&1 &
      ;;
    go)
      "$GOFS" --store "$store" mount --mountpoint "$mount" "$@" >"$logfile" 2>&1 &
      ;;
    *) echo "unknown mount implementation $impl" >&2; return 1 ;;
  esac
  MOUNT_PID_LAST=$!
  MOUNT_PIDS+=("$MOUNT_PID_LAST")
  MOUNT_LOG_LAST="$logfile"
  record_cmd "$impl mount $store -> $mount $*"
}
# start_mount <rust|go> <store> <mount> [mount flags...]
start_mount() {
  local mount="$3"
  start_mount_async "$@" || return 1
  if ! wait_mount "$mount"; then
    log "mount failed at $mount; daemon log:"; cat "$MOUNT_LOG_LAST" >&2 || true
    return 1
  fi
  return 0
}
# start_shared_mount_async <store> <spec>
# Starts one `mount-many` process that serves every view listed in the spec.
# Sets MOUNT_PID_LAST/MOUNT_LOG_LAST and records the pid for cleanup.
start_shared_mount_async() {
  local store="$1" spec="$2"
  local logfile="$SCRATCH/mount-shared-$RUN_ID.log"
  "$RUSTFS" --store "$store" --fixed-timestamp-ns "$TS1" mount-many --spec "$spec" \
    >"$logfile" 2>&1 &
  MOUNT_PID_LAST=$!
  MOUNT_PIDS+=("$MOUNT_PID_LAST")
  MOUNT_LOG_LAST="$logfile"
  record_cmd "rust mount-many --store $store --spec $spec"
}
stop_mount() {
  local mount="$1"
  if mountpoint -q "$mount" 2>/dev/null; then
    fusermount3 -u "$mount" 2>/dev/null || umount -l "$mount" 2>/dev/null || true
  fi
  local tries=0
  while mountpoint -q "$mount" 2>/dev/null && [ "$tries" -lt 100 ]; do sleep 0.05; tries=$((tries + 1)); done
  mountpoint -q "$mount" 2>/dev/null && return 1
  return 0
}
listing_digest() {
  local mount="$1" out
  # A dead FUSE daemon leaves an ENOTCONN mountpoint behind; make that an
  # explicit failure instead of a digest over a partial listing.
  out="$( cd "$mount" 2>/dev/null && find . -mindepth 1 -not -path './.weft*' -printf '%y %p\n' 2>/dev/null | LC_ALL=C sort )" || return 1
  [ -n "$out" ] || return 1
  # `$(...)` strips the trailing newline; restore it so the digest is over the
  # exact sorted listing with its final newline.
  printf '%s\n' "$out" | sha256sum | awk '{print $1}'
}

# ---- timing and measurement helpers ---------------------------------------
now_ns() { date +%s%N; }
# percentile_file <file-of-numbers> <p>  -> nearest-rank percentile
percentile_file() {
  sort -n "$1" 2>/dev/null | awk -v p="$2" '{v[NR]=$1} END {
    if (NR == 0) { print "n/a"; exit }
    idx = int(p * NR / 100 + 0.999999)
    if (idx < 1) idx = 1
    if (idx > NR) idx = NR
    printf "%d", v[idx]
  }'
}
max_file() { sort -n "$1" 2>/dev/null | tail -1; [ -s "$1" ] || printf 'n/a'; }
mean_file() {
  awk '{s+=$1; n++} END {if (n == 0) print "n/a"; else printf "%.2f", s/n}' "$1" 2>/dev/null
}
sample_free_mb() { free -m 2>/dev/null | awk '/^Mem:/ {print $3}' || true; }
# cgroup peak in bytes when the runtime exposes it; "unavailable" otherwise.
cgroup_peak_bytes() {
  local path
  for path in /sys/fs/cgroup/memory.peak \
              /sys/fs/cgroup/memory/memory.max_usage_in_bytes \
              /sys/fs/cgroup/memory/memory.peak; do
    if [ -r "$path" ]; then cat "$path"; return 0; fi
  done
  printf 'unavailable'
}
sum_rss_kb() {
  local total=0 pid rss
  for pid in "$@"; do
    [ -r "/proc/$pid/status" ] || continue
    rss="$(awk '/^VmRSS:/ {print $2}' "/proc/$pid/status" 2>/dev/null)"
    total=$((total + ${rss:-0}))
  done
  printf '%s' "$total"
}
# sample_daemons <pid...> -> DAEMON_RSS_KB (sum), DAEMON_RSS_MAX_KB,
# DAEMON_THREADS (sum), DAEMON_THREADS_MAX, DAEMON_SAMPLED (processes read).
# One awk pass over every /proc/<pid>/status keeps a 1,000-daemon sample cheap.
DAEMON_RSS_KB=0
DAEMON_RSS_MAX_KB=0
DAEMON_THREADS=0
DAEMON_THREADS_MAX=0
DAEMON_SAMPLED=0
DAEMON_MISSING=0
sample_daemons() {
  local present=() pid
  for pid in "$@"; do
    [ -r "/proc/$pid/status" ] && present+=("/proc/$pid/status")
  done
  DAEMON_MISSING=$(( $# - ${#present[@]} ))
  DAEMON_RSS_KB=0
  DAEMON_RSS_MAX_KB=0
  DAEMON_THREADS=0
  DAEMON_THREADS_MAX=0
  DAEMON_SAMPLED=0
  [ "${#present[@]}" -gt 0 ] || return 0
  local out
  # A process can exit between the readability check and awk; a missing file
  # makes awk exit non-zero, which is a sample miss, not a demo failure.
  out="$(awk '
    FNR == 1 { if (n > 0) { sum_rss += rss; if (rss > max_rss) max_rss = rss;
                           sum_thr += thr; if (thr > max_thr) max_thr = thr }
               rss = 0; thr = 0; n++ }
    /^VmRSS:/ { rss = $2 }
    /^Threads:/ { thr = $2 }
    END { if (n > 0) { sum_rss += rss; if (rss > max_rss) max_rss = rss;
                      sum_thr += thr; if (thr > max_thr) max_thr = thr }
          printf "%d %d %d %d %d", sum_rss, max_rss, sum_thr, max_thr, n }
  ' "${present[@]}" 2>/dev/null || true)"
  read -r DAEMON_RSS_KB DAEMON_RSS_MAX_KB DAEMON_THREADS DAEMON_THREADS_MAX DAEMON_SAMPLED \
    <<< "${out:-0 0 0 0 0}" || true
  DAEMON_RSS_KB="${DAEMON_RSS_KB:-0}"
  DAEMON_RSS_MAX_KB="${DAEMON_RSS_MAX_KB:-0}"
  DAEMON_THREADS="${DAEMON_THREADS:-0}"
  DAEMON_THREADS_MAX="${DAEMON_THREADS_MAX:-0}"
  DAEMON_SAMPLED="${DAEMON_SAMPLED:-0}"
}
# cgroup v2 pids accounting, when the runtime exposes it. "unavailable" otherwise.
cgroup_pids_current() {
  if [ -r /sys/fs/cgroup/pids.current ]; then cat /sys/fs/cgroup/pids.current; return 0; fi
  printf 'unavailable'
}
cgroup_pids_max() {
  if [ -r /sys/fs/cgroup/pids.max ]; then cat /sys/fs/cgroup/pids.max; return 0; fi
  printf 'unavailable'
}
# Number of times the cgroup pids limit refused a fork (`pids.events: max N`).
cgroup_pids_events_max() {
  local path=/sys/fs/cgroup/pids.events
  if [ ! -r "$path" ]; then printf 'unavailable'; return 0; fi
  awk '$1 == "max" { print $2; found = 1 } END { if (!found) print 0 }' "$path" 2>/dev/null || printf 'unavailable'
}
# Number of OOM kills charged to the cgroup (`memory.events: oom_kill N`).
cgroup_oom_kills() {
  local path=/sys/fs/cgroup/memory.events
  if [ ! -r "$path" ]; then printf 'unavailable'; return 0; fi
  awk '$1 == "oom_kill" { print $2; found = 1 } END { if (!found) print 0 }' "$path" 2>/dev/null || printf 'unavailable'
}

build_tools() {
  log "===== BUILD ====="
  if [ "${WEFT_DEMO_SKIP_BUILD:-0}" = 1 ]; then
    # Used only by courts that pre-built in a larger container and then run a
    # deliberately small memory limit; it does not change what is measured.
    log "WEFT_DEMO_SKIP_BUILD=1: reusing the binaries built by the build phase"
    [ -x "$RUSTFS" ] && [ -x "$GOFS" ] && [ -x "$JSONQ" ] && [ -x "$RECEIPTGEN" ] \
      && [ -x "$BIN/context-corpus" ]
    return
  fi
  ( cd "$ROOT" && cargo build --quiet --features fuse --bin weftfs )
  ( cd "$ROOT/WeftFS-Go" && CGO_ENABLED=0 go build -o "$GOFS" ./cmd/weftfs )
  ( cd /tmp && GO111MODULE=off go build -o "$JSONQ" "$ROOT/scripts/showcase/jsonq.go" )
  ( cd /tmp && GO111MODULE=off go build -o "$RECEIPTGEN" "$ROOT/scripts/showcase/receiptgen.go" )
  ( cd /tmp && GO111MODULE=off go build -o "$DEMOCMP" "$ROOT/scripts/showcase/democmp.go" )
  ( cd "$ROOT" && cargo build --quiet --example context_mount_corpus )
  cp "$TARGET_DEBUG/weftfs" "$RUSTFS"
  cp "$TARGET_DEBUG/examples/context_mount_corpus" "$BIN/context-corpus"
  [ -x "$RUSTFS" ] && [ -x "$GOFS" ] && [ -x "$JSONQ" ] && [ -x "$RECEIPTGEN" ] && [ -x "$BIN/context-corpus" ]
}

demo_finalize() {
  if [ "$FINALIZED" = 1 ]; then return 0; fi
  FINALIZED=1
  set +e
  cleanup_mounts
  local finished duration
  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  duration=$(( ( $(date +%s) - DEMO_START_EPOCH ) * 1000 ))
  meta run.finished_at "$finished"
  meta run.duration_ms "$duration"
  if [ -x "$RECEIPTGEN" ]; then
    WEFT_RECEIPT_SCHEMA="$DEMO_SCHEMA" "$RECEIPTGEN" < "$RESULTS" > "$RECEIPT"
    cp "$RESULTS" "$RECEIPTS/$BASE.results.tsv"
    FINAL_ALL_PASS="$("$JSONQ" "$RECEIPT" summary.all_pass 2>/dev/null || echo false)"
    log "summary: pass=$("$JSONQ" "$RECEIPT" summary.pass 2>/dev/null || echo 0) fail=$("$JSONQ" "$RECEIPT" summary.fail 2>/dev/null || echo 0) blocked=$("$JSONQ" "$RECEIPT" summary.blocked 2>/dev/null || echo 0) all_pass=$FINAL_ALL_PASS"
    if [ -n "${WEFT_DEMO_COMPARE_WITH:-}" ] && [ -f "$WEFT_DEMO_COMPARE_WITH" ] && [ -x "$DEMOCMP" ]; then
      if "$DEMOCMP" "$WEFT_DEMO_COMPARE_WITH" "$RECEIPT" \
          > "$RECEIPTS/$BASE.determinism.txt" 2>&1; then
        log "determinism: semantic_match=true"
      else
        log "determinism: semantic_match=false (see $RECEIPTS/$BASE.determinism.txt)"
      fi
      cat "$RECEIPTS/$BASE.determinism.txt" >&2
    fi
  else
    log "ERROR: receipt generator missing; no receipt written"
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
    record_assert "unexpected shell error at line $line" "no shell error" error false
    step_end fail "unexpected shell error at line $line"
  else
    log "unexpected shell error at line $line (outside a step)"
  fi
  demo_finalize
  trap - EXIT
  exit 1
}

# demo_init <schema> <receipt-prefix> <title>
demo_init() {
  DEMO_SCHEMA="$1"
  local prefix="$2" title="$3"
  BASE="${WEFT_DEMO_RECEIPT_NAME:-$prefix-$UTC}"
  RECEIPT="$RECEIPTS/$BASE.json"
  LOG="$RECEIPTS/$BASE.log"
  RESULTS="$SCRATCH/results-$RUN_ID.tsv"
  DEMO_START_EPOCH="$(date +%s)"
  : > "$RESULTS"
  exec > >(tee -a "$LOG") 2>&1
  trap 'on_error $LINENO' ERR
  trap 'demo_finalize' EXIT
  meta generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  meta run.started_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  meta demo.title "$title"
  meta env.court fuse
  meta env.image "${WEFT_COURT_IMAGE:-unknown}"
  meta env.image_id "${WEFT_COURT_IMAGE_ID:-unknown}"
  meta env.kernel "$(uname -srm)"
  meta env.arch "$(uname -m)"
  meta env.rustc "$(rustc --version 2>/dev/null || echo unknown)"
  meta env.go "$(go version 2>/dev/null || echo unknown)"
  meta env.fusermount3 "$(fusermount3 --version 2>&1 | head -1 || echo unknown)"
  meta env.tar "$(tar --version 2>&1 | head -1 || echo unknown)"
  meta env.zstd "$(zstd --version 2>&1 | head -1 || echo unknown)"
  meta env.limit_memory "${WEFT_COURT_MEMORY:-unknown}"
  meta env.limit_memory_swap "${WEFT_COURT_MEMORY:-unknown}"
  meta env.limit_pids "${WEFT_COURT_PIDS:-unknown}"
  meta env.limit_nproc "${WEFT_COURT_NPROC:-unknown}"
  meta env.limit_cpus "${WEFT_COURT_CPUS:-unknown}"
  meta env.tmpfs_tmp "1g"
  meta env.tmpfs_fuse "512m"
  meta env.cap_drop "ALL"
  meta env.cap_add "SYS_ADMIN"
  meta env.device "/dev/fuse"
  meta env.fixed_timestamps "$TS1,$TS2,$TS3"
  meta env.skip_build "${WEFT_DEMO_SKIP_BUILD:-0}"
  meta env.host_nproc "$(nproc 2>/dev/null || echo unknown)"
  meta env.scratch "$(rel "$SCRATCH")"
  repo WeftFS "${WEFT_REPO_WEFTFS:-unknown}"
  repo WeftFS-Go "${WEFT_REPO_WEFTFS_GO:-unknown}"
  repo WeftCodec "${WEFT_REPO_WEFTCODEC:-unknown}"
  repo WeftCodec-Go "${WEFT_REPO_WEFTCODEC_GO:-unknown}"
  build_tools
  if [ "${WEFT_DEMO_BUILD_ONLY:-0}" = 1 ]; then
    # Courts that need the binaries before a tightly limited run use this to
    # build once in a roomy container. It writes no receipt and mounts nothing.
    log "WEFT_DEMO_BUILD_ONLY=1: build phase complete"
    FINALIZED=1
    trap - EXIT ERR
    exit 0
  fi
  log "receipt: $RECEIPT"
}
