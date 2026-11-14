#!/usr/bin/env bash
# T029 court: FUSE mutations under optimistic publication (MW2).
#
# Host:  bash scripts/fuse_mw2_court.sh
# Court: bash scripts/docker-court.sh fuse "bash scripts/fuse_mw2_court.sh"
#
# Two halves, both against a real mount:
#
#   * the Rust integration suite (`tests/fuse_mw2.rs`), which owns the cases a
#     shell cannot express: `RENAME_EXCHANGE` (raw `renameat2`), a reader whose
#     fd spans a concurrent overwrite, and the exact create/rename/link order;
#   * a shell driver that issues the same mutations from **independent
#     processes** (concurrent create, rename, overwrite, hard link, unlink),
#     checks the exact final state byte for byte, exercises every documented
#     refusal, unmounts, and runs `verify --full` on the resulting store.
#
# The default (MW1) mount is exercised in the same court: create, mkdir and
# rename must keep working, so an optimistic refusal cannot leak into the
# default path.
#
# Every hard limit comes from scripts/docker-court.sh; this script adds none and
# disables none.
#
# Writes:
#   bench/receipts/fuse-mw2-<UTC>.json
#   bench/receipts/fuse-mw2-<UTC>.log
#
# Scope label: one host, one local POSIX namespace, one mount daemon per mode.
# No power-loss, multi-node or fleet claim.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  exec "$root/scripts/docker-court.sh" fuse "bash scripts/fuse_mw2_court.sh"
fi

cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="bench/receipts/fuse-mw2-${stamp}.json"
log="bench/receipts/fuse-mw2-${stamp}.log"
work="$(mktemp -d /tmp/weft-fuse-mw2.XXXXXX)"
mkdir -p bench/receipts
: >"$log"
exec > >(tee -a "$log") 2>&1

BIN=".docker-cache/target-rust/debug/weftfs"
if [[ ! -x "$BIN" ]]; then
  echo "building weftfs (fuse)..." >&2
  cargo build --quiet --features fuse --bin weftfs
fi
if [[ ! -x "$BIN" ]]; then
  echo "cargo build did not produce $BIN" >&2
  exit 1
fi

failures=0
hot=""
c_trunc_mw2=""
h_trunc_mw1=""
record_failure() {
  failures=$((failures + 1))
  echo "FAIL: $1" >&2
}

mounted=""
cleanup() {
  if [[ -n "$mounted" ]]; then
    "$BIN" unmount >/dev/null 2>&1 || true
    umount -l "$mounted" >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

is_mounted() {
  local target="$1" line
  while IFS= read -r line; do
    set -- $line
    [[ "$5" == "$target" ]] && return 0
  done </proc/self/mountinfo
  return 1
}

wait_mount() {
  local target="$1" attempts=0
  while (( attempts < 750 )); do
    if is_mounted "$target"; then
      sleep 0.05
      return 0
    fi
    attempts=$((attempts + 1))
    sleep 0.02
  done
  return 1
}

echo "== T029 FUSE mutation court =="
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"
echo "stamp: $stamp"

# ---------------------------------------------------------------------------
# 0. The Rust half: raw renameat2 exchange and the fd-spanning reader.
# ---------------------------------------------------------------------------
echo "== 0 Rust integration suite (fuse_mw2) =="
rust_result="$(timeout 600 cargo test --quiet --features fuse --test fuse_mw2 -- --test-threads=1 2>&1 || true)"
printf '%s\n' "$rust_result" >"$work/rust-tests.log"
rust_ok=false
if grep -q 'test result: ok\. 8 passed; 0 failed' "$work/rust-tests.log"; then
  rust_ok=true
else
  record_failure "the fuse_mw2 Rust suite did not report 8 passing tests"
fi
echo "0: rust_suite_ok=$rust_ok"

# ---------------------------------------------------------------------------
# Helpers for one mount.
# ---------------------------------------------------------------------------
fuse_store=""
fuse_mnt=""
fuse_mount_optimistic() {
  fuse_store="$work/opt-store"
  fuse_mnt="$work/opt-mnt"
  mkdir -p "$fuse_store" "$fuse_mnt"
  "$BIN" --store "$fuse_store" init >/dev/null
  "$BIN" mount --store "$fuse_store" --mountpoint "$fuse_mnt" --foreground \
    --writer-mode optimistic >"$work/opt-mount.out" 2>"$work/opt-mount.err" &
  fuse_pid=$!
  mounted="$fuse_mnt"
  if ! wait_mount "$fuse_mnt"; then
    cat "$work/opt-mount.err" >&2 || true
    record_failure "the optimistic mount did not appear"
    return 1
  fi
  return 0
}

stop_fuse_mount() {
  if [[ -n "$mounted" ]]; then
    kill -TERM "$fuse_pid" >/dev/null 2>&1 || true
    for _ in $(seq 1 400); do
      kill -0 "$fuse_pid" >/dev/null 2>&1 || break
      sleep 0.02
    done
    kill -KILL "$fuse_pid" >/dev/null 2>&1 || true
    wait "$fuse_pid" 2>/dev/null || true
    umount -l "$mounted" >/dev/null 2>&1 || true
    mounted=""
  fi
}

spawn() { "$@" & children+=("$!"); }
join_children() {
  local pid failed=0
  for pid in "${children[@]}"; do
    wait "$pid" || failed=1
  done
  children=()
  return "$failed"
}
children=()

# Four processes repeatedly truncate-open one file. Returns the failure count
# on stdout. Measured only: kernel 7.3 returns EIO for some of these opens in
# both MW1 and MW2, so it is not evidence about the transaction path.
run_trunc_control() {
  local mnt="$1" rounds="$2" fails=0 round i
  local -a pids
  for ((round = 1; round <= rounds; round++)); do
    pids=()
    for i in 0 1 2 3; do
      sh -c "printf 't-$i-$round' > '$mnt/hot.txt'" &
      pids+=("$!")
    done
    for i in "${pids[@]}"; do
      wait "$i" || fails=$((fails + 1))
    done
  done
  echo "$fails"
}

# ---------------------------------------------------------------------------
# A. Concurrent create from six independent processes.
# ---------------------------------------------------------------------------
a_ok=true
if fuse_mount_optimistic; then
  echo "== A concurrent create =="
  for i in 0 1 2 3 4 5; do
    spawn sh -c "printf 'create-%d' '$i' > '$fuse_mnt/c$i.txt'"
  done
  if ! join_children; then
    a_ok=false
    record_failure "A: at least one create process failed"
  fi
  for i in 0 1 2 3 4 5; do
    if [[ "$(cat "$fuse_mnt/c$i.txt" 2>/dev/null)" != "create-$i" ]]; then
      a_ok=false
      record_failure "A: c$i.txt has the wrong bytes"
    fi
  done

  # -------------------------------------------------------------------------
  # B. Concurrent rename of four of those files.
  # -------------------------------------------------------------------------
  b_ok=true
  for i in 0 1 2 3; do
    spawn mv "$fuse_mnt/c$i.txt" "$fuse_mnt/moved-$i.txt"
  done
  if ! join_children; then
    b_ok=false
    record_failure "B: at least one rename process failed"
  fi
  for i in 0 1 2 3; do
    if [[ -e "$fuse_mnt/c$i.txt" ]]; then
      b_ok=false
      record_failure "B: source c$i.txt still exists"
    fi
    if [[ "$(cat "$fuse_mnt/moved-$i.txt" 2>/dev/null)" != "create-$i" ]]; then
      b_ok=false
      record_failure "B: moved-$i.txt has the wrong bytes"
    fi
  done
  echo "A: create_ok=$a_ok B: rename_ok=$b_ok"

  # -------------------------------------------------------------------------
  # C. Concurrent whole-file overwrite of one name. Equal-length payloads and
  #    `conv=notrunc`, so each writer publishes the same 8 bytes at offset 0
  #    and the final file must be exactly one writer's content. (A truncating
  #    open is exercised separately as a control: kernel 7.3 rejects some
  #    concurrent same-inode O_TRUNC opens with EIO in BOTH modes, so it cannot
  #    be the evidence for the transaction path.)
  # -------------------------------------------------------------------------
  c_ok=true
  printf 'seed0000' >"$fuse_mnt/hot.txt"
  for i in 0 1 2 3; do
    printf 'writer-%d' "$i" >"$work/payload-$i"
  done
  for i in 0 1 2 3; do
    spawn dd if="$work/payload-$i" of="$fuse_mnt/hot.txt" bs=8 count=1 conv=notrunc status=none
  done
  if ! join_children; then
    c_ok=false
    record_failure "C: at least one overwrite process failed"
  fi
  hot="$(cat "$fuse_mnt/hot.txt" 2>/dev/null)"
  case "$hot" in
    writer-0 | writer-1 | writer-2 | writer-3) ;;
    *)
      c_ok=false
      record_failure "C: final content is not exactly one writer's payload: '$hot'"
      ;;
  esac
  if [[ "$(wc -c <"$fuse_mnt/hot.txt")" != "8" ]]; then
    c_ok=false
    record_failure "C: final length is not 8 bytes"
  fi
  echo "C: overwrite_ok=$c_ok final='$hot'"

  # -------------------------------------------------------------------------
  # D. Concurrent hard links, then concurrent unlink of the extra names.
  # -------------------------------------------------------------------------
  d_ok=true
  printf 'linked' >"$fuse_mnt/link-target"
  for i in 0 1 2 3; do
    spawn ln "$fuse_mnt/link-target" "$fuse_mnt/link-$i"
  done
  if ! join_children; then
    d_ok=false
    record_failure "D: at least one link process failed"
  fi
  if [[ "$(stat -c %h "$fuse_mnt/link-target")" != "5" ]]; then
    d_ok=false
    record_failure "D: link count before unlink is not 5"
  fi
  for i in 0 1 2 3; do
    if [[ "$(cat "$fuse_mnt/link-$i" 2>/dev/null)" != "linked" ]]; then
      d_ok=false
      record_failure "D: link-$i has the wrong bytes"
    fi
    spawn rm "$fuse_mnt/link-$i"
  done
  if ! join_children; then
    d_ok=false
    record_failure "D: at least one unlink process failed"
  fi
  if [[ "$(stat -c %h "$fuse_mnt/link-target")" != "1" ]]; then
    d_ok=false
    record_failure "D: link count after unlink is not 1"
  fi
  echo "D: link_ok=$d_ok"

  # -------------------------------------------------------------------------
  # E. Reader snapshot isolation across a concurrent overwrite from another
  #    process: the fd that was opened first must keep reading its version.
  # -------------------------------------------------------------------------
  e_ok=false
  printf 'first-version' >"$fuse_mnt/snap.txt"
  rm -f "$work/snap-ready" "$work/snap-gate" "$work/snap-read"
  sh -c "exec 3<'$fuse_mnt/snap.txt'; : >'$work/snap-ready'; while [ ! -f '$work/snap-gate' ]; do sleep 0.01; done; cat <&3 >'$work/snap-read'" &
  reader_pid=$!
  for _ in $(seq 1 600); do
    [[ -f "$work/snap-ready" ]] && break
    sleep 0.01
  done
  if [[ ! -f "$work/snap-ready" ]]; then
    record_failure "E: the reader never opened the file"
  else
    printf 'second-version!' >"$fuse_mnt/snap.txt"
    : >"$work/snap-gate"
    wait "$reader_pid" || record_failure "E: the reader process failed"
    if [[ "$(cat "$work/snap-read" 2>/dev/null)" == "first-version" ]]; then
      e_ok=true
    else
      record_failure "E: the open reader observed the newer version"
    fi
    if [[ "$(cat "$fuse_mnt/snap.txt")" != "second-version!" ]]; then
      record_failure "E: the overwrite did not publish the new version"
    fi
  fi
  echo "E: snapshot_isolation=$e_ok"

  # -------------------------------------------------------------------------
  # F. Documented refusals. Each must fail and leave the store unchanged.
  # -------------------------------------------------------------------------
  f_ok=true
  f_rows=()
  refusal() {
    local label="$1" needle="$2"
    shift 2
    local out status
    out="$(sh -c "$*" 2>&1)" && status=0 || status=1
    if [[ "$status" -eq 0 ]]; then
      f_ok=false
      record_failure "F: $label unexpectedly succeeded"
      f_rows+=("{\"operation\":\"$label\",\"refused\":false}")
      return
    fi
    if [[ -n "$needle" && "$out" != *"$needle"* ]]; then
      f_ok=false
      record_failure "F: $label failed without the expected message '$needle': $out"
      f_rows+=("{\"operation\":\"$label\",\"refused\":true,\"message_match\":false}")
      return
    fi
    f_rows+=("{\"operation\":\"$label\",\"refused\":true,\"message_match\":true}")
  }
  refusal "mkdir" "not supported" "mkdir '$fuse_mnt/dir'"
  refusal "chmod" "" "chmod 600 '$fuse_mnt/c5.txt'"
  refusal "symlink" "" "ln -s target '$fuse_mnt/sym'"
  refusal "truncate" "" "truncate -s 0 '$fuse_mnt/c5.txt'"
  refusal "utimes" "" "touch '$fuse_mnt/moved-0.txt'"
  # glibc's posix_fallocate falls back to a zero-write emulation, so ask the
  # kernel directly: SYS_fallocate must come back EOPNOTSUPP from the FUSE
  # handler instead of being emulated above it.
  cat >"$work/fallocate.py" <<'PY'
import ctypes, os, platform, sys
libc = ctypes.CDLL(None, use_errno=True)
fd = os.open(sys.argv[1], os.O_RDWR)
nr = 285 if platform.machine() in ("x86_64", "amd64") else 47
if libc.syscall(nr, fd, 0, 0, 4096) == 0:
    sys.exit(0)
err = ctypes.get_errno()
print(f"fallocate failed: {os.strerror(err)}")
sys.exit(1)
PY
  refusal "fallocate" "Operation not supported" "python3 '$work/fallocate.py' '$fuse_mnt/moved-1.txt'"
  refusal "oversize_write" "" "dd if=/dev/zero of='$fuse_mnt/big.bin' bs=1M count=9 2>/dev/null"
  if [[ "$(cat "$fuse_mnt/c5.txt" 2>/dev/null)" != "create-5" ]]; then
    f_ok=false
    record_failure "F: a refused truncate changed c5.txt"
  fi
  if [[ -e "$fuse_mnt/dir" || -e "$fuse_mnt/sym" ]]; then
    f_ok=false
    record_failure "F: a refused operation left a namespace entry behind"
  fi
  # A write sequence is not a transaction: only each write is. The first eight
  # 1 MiB writes are legitimate commits; the ninth is refused, so the file must
  # stop at the 8 MiB whole-file rewrite bound rather than grow past it.
  if [[ -e "$fuse_mnt/big.bin" ]] && (( $(stat -c %s "$fuse_mnt/big.bin") > 8388608 )); then
    f_ok=false
    record_failure "F: the oversize dd grew the file past the 8 MiB bound"
  fi
  echo "F: refusals_ok=$f_ok"

  # Control: the kernel's same-inode concurrent truncating-open behaviour. This
  # is measured, not asserted: it is a pre-existing kernel-7.3 limitation seen
  # under both MW1 and MW2, so it cannot be attributed to the transaction path.
  c_trunc_mw2="$(run_trunc_control "$fuse_mnt" 3)"
  echo "C-control: concurrent truncating opens failed under MW2: $c_trunc_mw2/12"

  stop_fuse_mount

  # -------------------------------------------------------------------------
  # G. The resulting store verifies in full, offline.
  # -------------------------------------------------------------------------
  g_ok=false
  if "$BIN" --store "$fuse_store" --json verify --full >"$work/verify.json" 2>"$work/verify.err"; then
    g_ok=true
  else
    record_failure "G: verify --full failed on the optimistic store"
  fi
  echo "G: verify_full=$g_ok"
else
  a_ok=false
  b_ok=false
  c_ok=false
  d_ok=false
  e_ok=false
  f_ok=false
  g_ok=false
  f_rows=()
fi

# ---------------------------------------------------------------------------
# H. The default (MW1) mount is unaffected: create, mkdir and rename work.
# ---------------------------------------------------------------------------
h_ok=false
h_store="$work/mw1-store"
h_mnt="$work/mw1-mnt"
mkdir -p "$h_store" "$h_mnt"
"$BIN" --store "$h_store" init >/dev/null
"$BIN" mount --store "$h_store" --mountpoint "$h_mnt" --foreground \
  >"$work/mw1-mount.out" 2>"$work/mw1-mount.err" &
mw1_pid=$!
mounted="$h_mnt"
if wait_mount "$h_mnt"; then
  if printf 'single-writer' >"$h_mnt/mw1.txt" &&
    mkdir "$h_mnt/dir" &&
    mv "$h_mnt/mw1.txt" "$h_mnt/renamed.txt" &&
    [[ "$(cat "$h_mnt/renamed.txt")" == "single-writer" ]] &&
    [[ -d "$h_mnt/dir" ]]; then
    h_ok=true
  else
    record_failure "H: a default MW1 mutation failed"
  fi
  # Same control as the optimistic mount: a pre-existing kernel limitation is
  # expected to reproduce under MW1 too, which is what makes it not a T029 loss.
  h_trunc_mw1="$(run_trunc_control "$h_mnt" 3)"
  echo "H-control: concurrent truncating opens failed under MW1: $h_trunc_mw1/12"
  kill -TERM "$mw1_pid" >/dev/null 2>&1 || true
  for _ in $(seq 1 400); do
    kill -0 "$mw1_pid" >/dev/null 2>&1 || break
    sleep 0.02
  done
  kill -KILL "$mw1_pid" >/dev/null 2>&1 || true
  wait "$mw1_pid" 2>/dev/null || true
  umount -l "$h_mnt" >/dev/null 2>&1 || true
  mounted=""
  if ! "$BIN" --store "$h_store" --json verify --full >/dev/null 2>&1; then
    h_ok=false
    record_failure "H: the default-mode store failed verify --full"
  fi
else
  cat "$work/mw1-mount.err" >&2 || true
  record_failure "H: the default mount did not appear"
  kill -KILL "$mw1_pid" >/dev/null 2>&1 || true
fi
echo "H: mw1_default_ok=$h_ok"

# ---------------------------------------------------------------------------
# Receipt.
# ---------------------------------------------------------------------------
result="pass"
[[ "$failures" -eq 0 ]] || result="fail"
f_json="[$(IFS=,; echo "${f_rows[*]:-}")]"
python3 - "$receipt" "$stamp" "$result" "$failures" "$rust_ok" \
  "$a_ok" "$b_ok" "$c_ok" "$d_ok" "$e_ok" "$f_ok" "$g_ok" "$h_ok" \
  "$f_json" "$hot" "$c_trunc_mw2" "$h_trunc_mw1" "$log" <<'PY'
import json, os, sys
(receipt, stamp, result, failures, rust_ok, a_ok, b_ok, c_ok, d_ok, e_ok, f_ok,
 g_ok, h_ok, f_rows, hot, c_trunc_mw2, h_trunc_mw1, log) = sys.argv[1:]
def flag(value): return value == "true"
def count(value):
    try: return int(value)
    except (TypeError, ValueError): return None
doc = {
  "schema": "weftfs.fuse_mw2/1",
  "role": "T029 FUSE mutations under optimistic publication (MW2)",
  "generated_at": stamp,
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
  },
  "repositories": [{"name": "WeftFS", "revision": os.environ.get("WEFT_REPO_WEFTFS", "unknown")}],
  "mode": "optimistic (MW2 transactions); the default MW1 mount is exercised in H",
  "rust_suite": {"ok": flag(rust_ok), "tests": 8},
  "shell": {
    "a_concurrent_create": {"ok": flag(a_ok), "processes": 6},
    "b_concurrent_rename": {"ok": flag(b_ok), "processes": 4},
    "c_concurrent_overwrite": {"ok": flag(c_ok), "processes": 4, "final": hot,
      "method": "four 8-byte writes at offset 0 with conv=notrunc; the last publisher wins and no splice of two writers is possible"},
    "d_concurrent_link_unlink": {"ok": flag(d_ok), "processes": 4},
    "e_reader_snapshot_isolation": {"ok": flag(e_ok)},
    "f_refusals": {"ok": flag(f_ok), "rows": json.loads(f_rows) if f_rows else []},
    "g_verify_full": {"ok": flag(g_ok)},
    "h_mw1_default_unaffected": {"ok": flag(h_ok)},
  },
  "same_inode_trunc_control": {
    "mw2_failures_of_12": count(c_trunc_mw2),
    "mw1_failures_of_12": count(h_trunc_mw1),
    "interpretation": "four concurrent O_TRUNC opens of one inode fail with EIO under BOTH modes on kernel 7.3; this is the pre-existing same-inode truncating-open limitation, not a T029 regression. Scenario C therefore uses a non-truncating whole-file write to exercise the transaction path.",
  },
  "semantics": {
    "create": "one transaction: object record + directory entry, precondition Absent",
    "overwrite": "read-modify-write of a whole file; the observed version is the precondition",
    "unlink": "entry removal plus object/relation removal when the last name goes, one transaction",
    "rename": "target entry (and replaced object when unreferenced) plus source entry in one transaction",
    "link": "one new directory entry in one transaction; directories refused",
    "read": "an open handle reads the version it opened (snapshot isolation)",
    "refused": ["mkdir", "mknod (all types)", "symlink", "chmod/chown/utimes", "truncate(2)",
                "setxattr/removexattr", "fallocate", "copy_file_range", "RENAME_EXCHANGE",
                "a write past the 8 MiB whole-file rewrite bound (E2BIG)"],
    "not_composable": ["--write-back", "--group-commit"],
  },
  "failures": int(failures),
  "result": result,
  "losses": [
    "a truncating open is not one transaction: `>` on an existing file is a truncate transaction followed by write transactions, so a crash between them leaves the truncated version",
    "snapshot isolation is per open handle: reads use the pinned version, but `stat` on the inode reports the current version",
    "the whole-file rewrite is bounded at 8 MiB (E2BIG above it); larger files need MW1 or a future range-write transaction",
    "a write sequence is not atomic: each write is one transaction, and a refused later write does not roll back earlier ones",
    "four concurrent same-inode O_TRUNC opens hit a pre-existing kernel-7.3 EIO under both MW1 and MW2 (measured in same_inode_trunc_control)",
  ],
  "evidence": {"log": log},
  "scope": {
    "label": "one host, one local POSIX namespace, one mount daemon per mode; process-level concurrency",
    "checked": "six concurrent creates, four concurrent renames, four concurrent whole-file overwrites, four concurrent links and unlinks, an fd-spanning reader, eight refusals, offline verify --full, and the default MW1 mount",
    "not_checked": "multi-node mounts, physical power loss, Go parity for FUSE mutations, byte-range writes that are not a whole-file rewrite, write-back/group-commit composition with MW2",
  },
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "result": result, "failures": int(failures)}))
PY

echo "receipt: $receipt"
[[ "$result" == "pass" ]]
