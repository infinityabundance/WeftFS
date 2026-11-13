#!/usr/bin/env bash
# Court: repeat the `writer_lock` suite and record the result.
#
# The two-process exclusion test used to race its own child: the holder closed
# the descriptor it then read from, exited immediately, and the parent's probe
# sometimes ran after the lock was released. A single run of the suite is
# therefore not evidence that the test is deterministic; the evidence is a
# repetition count under stated conditions.
#
# Usage (inside the rust court, which mounts the repository at /work):
#
#   WEFT_WL_RECEIPT=bench/receipts/writer-lock-<stamp>.json \
#   WEFT_WL_STAMP=<stamp> WEFT_WL_ITERS=60 WEFT_WL_LOAD=0 \
#     bash bench/scripts/writer_lock_court.sh
#
# Environment:
#   WEFT_WL_RECEIPT   output receipt path, relative to the repository root
#   WEFT_WL_STAMP     stamp recorded in the receipt; also written beside the
#                     receipt as <receipt without .json>.log
#   WEFT_WL_ITERS     suite repetitions (default 60)
#   WEFT_WL_LOAD      busy-loop processes run alongside the suite (default 0)
#   WEFT_WL_THREADS   libtest --test-threads (default 8)
#   WEFT_WL_SKIP_BUILD=1  reuse the newest built test binary
#
# The receipt records the identity the court ran as (`id -u`), so the same
# script run under `WEFT_COURT_UID=1001` is a separate row in the evidence.
set -Eeuo pipefail

cd /work

receipt="${WEFT_WL_RECEIPT:?WEFT_WL_RECEIPT is required}"
stamp="${WEFT_WL_STAMP:?WEFT_WL_STAMP is required}"
iterations="${WEFT_WL_ITERS:-60}"
load="${WEFT_WL_LOAD:-0}"
threads="${WEFT_WL_THREADS:-8}"

log="${receipt%.json}.log"
mkdir -p "$(dirname "$receipt")"
: > "$log"

say() { printf '%s\n' "$*" | tee -a "$log" >&2; }

say "writer_lock court stamp=$stamp iterations=$iterations load=$load threads=$threads uid=$(id -u)"

if [[ "${WEFT_WL_SKIP_BUILD:-0}" != "1" ]]; then
  cargo test --test writer_lock --no-run >>"$log" 2>&1
fi

bin=""
while IFS= read -r candidate; do
  case "$candidate" in *.d) continue ;; esac
  bin="$candidate"
  break
done < <(ls -t .docker-cache/target-rust/debug/deps/writer_lock-* 2>/dev/null)
if [[ -z "$bin" ]]; then
  echo "writer_lock court: no test binary; build it first" >&2
  exit 1
fi
say "binary=$bin"

load_pids=()
cleanup() {
  for pid in "${load_pids[@]:-}"; do
    [[ -n "$pid" ]] && kill -9 "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
}
trap cleanup EXIT

if [[ "$load" -gt 0 ]]; then
  for _ in $(seq 1 "$load"); do
    ( while :; do :; done ) &
    load_pids+=("$!")
  done
  say "load: $load busy processes started"
fi

passed=0
failed=0
first_failure=""
for i in $(seq 1 "$iterations"); do
  if "$bin" --test-threads="$threads" >>"$log" 2>&1; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    say "FAIL iteration $i"
    if [[ -z "$first_failure" ]]; then
      first_failure="iteration $i"
    fi
  fi
done

say "RESULT iterations=$iterations passed=$passed failed=$failed"

image="${WEFT_COURT_IMAGE:-unknown}"
image_id="${WEFT_COURT_IMAGE_ID:-unknown}"
kernel="${WEFT_COURT_KERNEL:-$(uname -r)}"
memory="${WEFT_COURT_MEMORY:-unknown}"
cpus="${WEFT_COURT_CPUS:-unknown}"
pids="${WEFT_COURT_PIDS:-unknown}"
rev_weftfs="${WEFT_REPO_WEFTFS:-unknown}"
rev_codec="${WEFT_REPO_WEFTCODEC:-unknown}"
rev_codec_go="${WEFT_REPO_WEFTCODEC_GO:-unknown}"
rev_fs_go="${WEFT_REPO_WEFTFS_GO:-unknown}"
uid_now="$(id -u)"
epoch="$(date +%s)"
suite_hash="$(sha256sum tests/writer_lock.rs | awk '{print $1}')"
if [[ -n "$first_failure" ]]; then
  first_failure_json="\"$first_failure\""
else
  first_failure_json="null"
fi

cat > "$receipt" <<JSON
{
  "schema": "weft.writer-lock-court.v1",
  "stamp": "$stamp",
  "generated_at_epoch": $epoch,
  "court": {
    "image": "$image",
    "image_id": "$image_id",
    "kernel": "$kernel",
    "memory": "$memory",
    "cpus": "$cpus",
    "pids": "$pids",
    "uid": $uid_now,
    "revision_weftfs": "$rev_weftfs",
    "revision_weftcodec": "$rev_codec",
    "revision_weftcodec_go": "$rev_codec_go",
    "revision_weftfs_go": "$rev_fs_go"
  },
  "suite": "tests/writer_lock.rs",
  "suite_sha256": "$suite_hash",
  "test_threads": $threads,
  "load_procs": $load,
  "iterations": $iterations,
  "passed": $passed,
  "failed": $failed,
  "first_failure": $first_failure_json,
  "binary": "$bin",
  "log": "$log"
}
JSON

echo "receipt: $receipt"
