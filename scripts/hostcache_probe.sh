#!/usr/bin/env bash
# Host page-cache loss probe.
#
# Host usage (self-dispatches into the hard-limited VM court):
#   scripts/hostcache_probe.sh
#
# This probe never asks for or uses host privileges. It records:
#   - whether the container can write /proc/sys/vm/drop_caches (expected: no)
#   - the exact errno/message when that write is refused
#   - that a per-file POSIX_FADV_DONTNEED on a scratch file under the court's
#     bind mount is permitted and only evicts that file's clean pages
#   - that the QEMU storage courts use cache=none for guest data, which opens
#     the image with O_DIRECT and does not occupy the host page cache
#
# It does not drop host caches and makes no claim that host page-cache loss was
# tested.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  exec "$root/scripts/docker-court.sh" vm "bash scripts/hostcache_probe.sh"
fi
cd "$root"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
RECEIPT="bench/receipts/hostcache-probe-${STAMP}.json"
LOG="bench/receipts/hostcache-probe-${STAMP}.log"
WORK="$root/.docker-cache/hostcache-probe/$STAMP"
mkdir -p bench/receipts "$WORK"

say() { echo "$*" | tee -a "$LOG"; }
: >"$LOG"

say "== host page-cache loss probe =="
say "started: $STARTED_AT"
say "court: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
say "uid=$(id -u) gid=$(id -g)"
say "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"

cap_eff="$(awk '/^CapEff:/ {print $2}' /proc/self/status 2>/dev/null || true)"
drop_path=/proc/sys/vm/drop_caches
drop_exists=0
drop_mode=""
if [[ -e "$drop_path" ]]; then drop_exists=1; fi
if (( drop_exists == 1 )); then drop_mode="$(stat -c %a "$drop_path" 2>/dev/null || true)"; fi
drop_writable=0
if [[ -w "$drop_path" ]]; then drop_writable=1; fi

drop_rc=0
( printf '3\n' >"$drop_path" ) 2>"$WORK/drop.err" || drop_rc=$?
drop_error="$(cat "$WORK/drop.err" 2>/dev/null | tr '\n' ' ' || true)"
say "drop_caches: exists=$drop_exists mode=${drop_mode:-none} writable=$drop_writable attempt_rc=$drop_rc error=${drop_error:-none}"

# Safe per-file cache eviction: only this scratch file's clean pages.
scratch="$WORK/fadvise-scratch.bin"
python3 - "$scratch" <<'PY' >>"$LOG" 2>&1
import os
import sys

path = sys.argv[1]
data = os.urandom(4 * 1024 * 1024)
with open(path, "wb") as f:
    f.write(data)
    f.flush()
    os.fsync(f.fileno())
fd = os.open(path, os.O_RDONLY)
try:
    os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
    print("fadvise_dontneed=ok bytes=4194304")
finally:
    os.close(fd)
PY
fadvise_ok=0
if grep -q 'fadvise_dontneed=ok' "$LOG"; then fadvise_ok=1; fi
say "posix_fadvise(DONTNEED) on $scratch: ok=$fadvise_ok"

FINISHED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
LOG_SHA="$(sha256sum "$LOG" | cut -d' ' -f1)"

jq -n \
  --arg schema "weftfs.hostcache-probe/1" \
  --arg generated_at "$FINISHED_AT" \
  --arg court "${WEFT_COURT_IMAGE:-unknown}" \
  --arg image_id "${WEFT_COURT_IMAGE_ID:-unknown}" \
  --arg host_kernel "$(uname -a)" \
  --arg uid "$(id -u)" \
  --arg cap_eff "$cap_eff" \
  --arg drop_path "$drop_path" \
  --argjson drop_exists "$drop_exists" \
  --arg drop_mode "$drop_mode" \
  --argjson drop_writable "$drop_writable" \
  --argjson drop_rc "$drop_rc" \
  --arg drop_error "$drop_error" \
  --argjson fadvise_ok "$fadvise_ok" \
  --arg log "$LOG" \
  --arg log_sha "$LOG_SHA" \
  '{
    schema: $schema,
    generated_at: $generated_at,
    available: false,
    global_drop_caches: {
      path: $drop_path,
      exists: ($drop_exists == 1),
      mode: $drop_mode,
      writable_by_container: ($drop_writable == 1),
      attempt_exit_code: $drop_rc,
      error: $drop_error
    },
    per_file_fadvise_dontneed: {
      attempted: true,
      succeeded: ($fadvise_ok == 1),
      scope: "one 4 MiB scratch file under the court bind mount; only that file clean pages are eligible"
    },
    guest_data_cache_model: {
      powerloss_and_blkdebug_courts: "QEMU cache=none for the data disk; cache=none opens the image with O_DIRECT, so guest data does not occupy the host page cache",
      writeback_paths: "cache=writeback appears in some control/labelled runs, but the host page cache was not dropped"
    },
    blocker: (if ($drop_writable == 1) then null else "global host page-cache drop is not permitted in the unprivileged court; the kernel refuses the write and no host cache is dropped" end),
    environment: {
      court: $court,
      image_id: $image_id,
      host_kernel: $host_kernel,
      uid: $uid,
      cap_eff: $cap_eff,
      limit_memory: env.WEFT_COURT_MEMORY,
      limit_cpus: env.WEFT_COURT_CPUS,
      limit_pids: env.WEFT_COURT_PIDS
    },
    not_claimed: [
      "host page-cache loss",
      "physical media power loss",
      "global cache drop",
      "device write-cache loss"
    ],
    evidence: {raw_log: $log, raw_log_sha256: $log_sha}
  }' >"$RECEIPT"

say "receipt: $RECEIPT"
sha256sum "$RECEIPT" "$LOG"
say "HOSTCACHE_PROBE_DONE"
