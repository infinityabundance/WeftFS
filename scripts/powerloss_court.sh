#!/usr/bin/env bash
# F9.2 — QEMU virtual-machine power-cut court.
#
# Host usage (self-dispatches into the hard-limited VM court):
#   scripts/powerloss_court.sh
#
# Court usage:
#   scripts/docker-court.sh vm "bash scripts/powerloss_court.sh"
#
# Rig (all inside Docker; the host only runs Docker):
#   - Debian kernel + initramfs built into the court image
#   - read-only raw ext4 root disk, guest PID 1 = verification/powerloss/guest-init.sh
#   - qcow2 overlay data disk (fresh per iteration, raw ext4 empty base)
#   - guest writes with the Rust CLI, one commit per write, no sync(1)
#   - host waits for the first COMMIT line, sleeps a seeded pseudo-random delay,
#     then SIGKILLs qemu-system-x86_64 (no guest shutdown, no cache flush)
#   - the same qcow2 image is booted again; Rust and Go independently open,
#     verify --full, and read the file; the host compares both to the exact
#     expected content for the recovered generation
#
# This is a VM power-cut proxy, NOT physical media power loss: SIGKILL loses the
# guest page cache and QEMU's user-space state, but the host page cache and the
# virtual disk's own write cache are not physically removed. No fsck is run.
# A software process abort alone would not be evidence and is not used.
#
# Limits: inherited Docker limits (3g memory/3g swap/4 cpus/1024 pids, 1g
# bounded /tmp), guest 512 MiB, root disk 160 MiB read-only, data disk 128 MiB
# qcow2, boot timeouts, bounded iterations. Nothing here disables a limit.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  exec "$root/scripts/docker-court.sh" vm "bash scripts/powerloss_court.sh"
fi
cd "$root"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
START_EPOCH="$(date +%s)"
RECEIPT="bench/receipts/powerloss-${STAMP}.json"
LOG="bench/receipts/powerloss-${STAMP}.log"
mkdir -p bench/receipts

ITERS="${POWERLOSS_ITERATIONS:-10}"
SEED="${POWERLOSS_SEED:-20261103}"
CACHE_MODE="${POWERLOSS_CACHE_MODE:-writeback}"
BOOT_TIMEOUT="${POWERLOSS_BOOT_TIMEOUT:-120}"
VERIFY_TIMEOUT="${POWERLOSS_VERIFY_TIMEOUT:-180}"
KILL_MIN_MS="${POWERLOSS_KILL_MIN_MS:-100}"
KILL_MAX_MS="${POWERLOSS_KILL_MAX_MS:-1200}"
GUEST_MEM_MB=512
FILE_SIZE=1048576
PAYLOAD_SIZE=65536

case "$CACHE_MODE" in
  writeback|none|unsafe|writethrough) ;;
  *) echo "invalid POWERLOSS_CACHE_MODE=$CACHE_MODE" >&2; exit 64 ;;
esac
[[ "$ITERS" =~ ^[1-9][0-9]*$ ]] || { echo "invalid POWERLOSS_ITERATIONS" >&2; exit 64; }
(( KILL_MIN_MS < KILL_MAX_MS )) || { echo "invalid kill delay range" >&2; exit 64; }

WORK="$root/.docker-cache/powerloss/$STAMP"
BIN="$WORK/bin"
ROOTFS="$WORK/rootfs"
ROOT_IMG="$WORK/root.img"
DATA_BASE="$WORK/data-base.img"
DATA_QCOW2="$WORK/data.qcow2"
mkdir -p "$BIN" "$ROOTFS" "$WORK"
chmod -R a+rwX "$WORK"

say() { echo "$*" | tee -a "$LOG"; }
: >"$LOG"

say "== WeftFS power-loss court =="
say "started: $STARTED_AT"
say "kernel: $(uname -a)"
say "court image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
say "limits: memory=${WEFT_COURT_MEMORY:-?} swap=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?} tmpfs_tmp=1g"
say "iterations=$ITERS seed=$SEED cache=$CACHE_MODE guest_mem=${GUEST_MEM_MB}M kill_delay_ms=${KILL_MIN_MS}..${KILL_MAX_MS}"
say "qemu: $(qemu-system-x86_64 --version | head -n 1)"

# ---- build both independent CLIs -----------------------------------------
say "building Rust CLI"
cargo build --release --bin weftfs >>"$LOG" 2>&1
cp "${CARGO_TARGET_DIR:-$root/target}/release/weftfs" "$BIN/weftfs"

say "building Go CLI (CGO disabled, static)"
(
  cd WeftFS-Go
  CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -o "$BIN/go-weftfs" ./cmd/weftfs
) >>"$LOG" 2>&1

# ---- root filesystem (read-only) -----------------------------------------
say "building read-only guest root filesystem"
mkdir -p "$ROOTFS"/{bin,sbin,usr/bin,proc,sys,dev,tmp,data,lib,lib64,lib/x86_64-linux-gnu}
cp /bin/busybox "$ROOTFS/bin/busybox"
chmod 755 "$ROOTFS/bin/busybox"
for applet in sh ash mount umount mkdir cat echo sed grep head tail dd wc cut \
              sha256sum sync poweroff reboot dmesg sleep mknod printf chmod \
              ln ls true false ps kill env; do
  ln -sf /bin/busybox "$ROOTFS/bin/$applet"
done
cp "$BIN/weftfs" "$ROOTFS/usr/bin/weftfs"
cp "$BIN/go-weftfs" "$ROOTFS/usr/bin/go-weftfs"
cp verification/powerloss/guest-init.sh "$ROOTFS/sbin/init"
chmod 755 "$ROOTFS/sbin/init"

# Share the host glibc binary's load path into the guest. Go is static.
ldd "$BIN/weftfs" | awk '$2 == "=>" && $3 ~ /^\// {print $3} $1 ~ /^\// {print $1}' | sort -u |
while read -r lib; do
  [ -e "$lib" ] || continue
  mkdir -p "$ROOTFS$(dirname "$lib")"
  cp -L "$lib" "$ROOTFS$lib"
done
[ -e "$ROOTFS/lib64/ld-linux-x86-64.so.2" ] || {
  mkdir -p "$ROOTFS/lib64"
  cp -L /lib64/ld-linux-x86-64.so.2 "$ROOTFS/lib64/ld-linux-x86-64.so.2"
}

truncate -s 160M "$ROOT_IMG"
mke2fs -t ext4 -F -q -b 4096 -d "$ROOTFS" "$ROOT_IMG"
truncate -s 128M "$DATA_BASE"
mke2fs -t ext4 -F -q -b 4096 "$DATA_BASE"

KERNEL="$(ls -1 /boot/vmlinuz-*-amd64 2>/dev/null | sort -V | tail -n 1)"
KDIR=""
if [[ -n "$KERNEL" ]]; then
  KREL="${KERNEL#/boot/vmlinuz-}"
  INITRD="/boot/initrd.img-${KREL}"
  [[ -r "$INITRD" ]] || INITRD="$(ls -1 /boot/initrd.img-* 2>/dev/null | sort -V | tail -n 1)"
fi
if [[ -z "$KERNEL" || ! -r "${INITRD:-}" ]]; then
  say "PL_BLOCKER no_kernel_or_initrd kernel=${KERNEL:-none} initrd=${INITRD:-none}"
  jq -n --arg schema "weftfs.powerloss.receipt/1" --arg generated_at "$STARTED_AT" \
    --arg blocker "no Debian kernel/initramfs in the court image" \
    '{schema:$schema,generated_at:$generated_at,rig:{type:"qemu-vm",available:false},
      blockers:[[$blocker]],summary:{iterations:0,pass:0,fail:0,old:0,new:0,partial:0}}' \
    >"$RECEIPT"
  exit 2
fi
say "guest kernel: $KERNEL"
say "guest initramfs: $INITRD ($(stat -c %s "$INITRD") bytes)"

qemu_version="$(qemu-system-x86_64 --version | head -n 1)"
KERNEL_REL="$(basename "$KERNEL")"

# ---- execution ------------------------------------------------------------
ACCEL="tcg"
if [[ -r /dev/kvm && -w /dev/kvm ]]; then
  ACCEL="kvm"
fi
ACCEL_FALLBACK=0

qemu_start() {
  local mode="$1" serial="$2"
  local cpu="max"
  [[ "$ACCEL" == "kvm" ]] && cpu="host"
  qemu-system-x86_64 \
    -accel "$ACCEL" -cpu "$cpu" -smp 1 -m "${GUEST_MEM_MB}M" \
    -kernel "$KERNEL" -initrd "$INITRD" \
    -append "root=/dev/vda ro rootfstype=ext4 rootwait console=ttyS0,115200 panic=-1 loglevel=4 weft.mode=${mode} weft.iters=100000" \
    -drive "file=$ROOT_IMG,if=virtio,format=raw,readonly=on" \
    -drive "file=$DATA_QCOW2,if=virtio,format=qcow2,cache=$CACHE_MODE" \
    -display none -vga none -no-user-config -nodefaults -no-reboot \
    -serial stdio -monitor none </dev/null >"$serial" 2>&1 &
  QPID=$!
}

wait_for_grep() {
  local pid="$1" file="$2" pattern="$3" seconds="$4"
  local start deadline
  start="$(date +%s)"
  deadline=$((start + seconds))
  while kill -0 "$pid" 2>/dev/null; do
    if grep -q "$pattern" "$file" 2>/dev/null; then
      return 0
    fi
    (( $(date +%s) < deadline )) || return 1
    sleep 0.1
  done
  return 1
}

wait_for_commits() {
  local pid="$1" file="$2" want="$3" seconds="$4"
  local start deadline count
  start="$(date +%s)"
  deadline=$((start + seconds))
  while kill -0 "$pid" 2>/dev/null; do
    count="$(grep -c '^COMMIT ' "$file" 2>/dev/null || true)"
    [[ "${count:-0}" -ge "$want" ]] && return 0
    (( $(date +%s) < deadline )) || return 1
    sleep 0.1
  done
  return 1
}

stop_qemu() {
  local pid="$1"
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

build_expected() {
  local n="$1" out="$2"
  local prefix="POWERLOSS:${n}:"
  local pad=$((PAYLOAD_SIZE - ${#prefix}))
  (( pad >= 0 )) || return 1
  { printf '%s' "$prefix"; head -c "$pad" /dev/zero; head -c $((FILE_SIZE - PAYLOAD_SIZE)) /dev/zero; } >"$out"
}

field_from_summary() {
  local file="$1" prefix="$2" key="$3"
  grep "^${prefix} " "$file" 2>/dev/null | tail -n 1 | sed -n "s/.* ${key}=\([^ ]*\).*/\1/p"
}

ITER_JSONL="$WORK/iterations.jsonl"
: >"$ITER_JSONL"
CONSECUTIVE_BOOT_FAILURES=0
BLOCKED=0

RANDOM="$SEED"

for ((i = 1; i <= ITERS; i++)); do
  write_serial="$WORK/iter-${i}-write.log"
  verify_serial="$WORK/iter-${i}-verify.log"
  rm -f "$DATA_QCOW2"
  qemu-img create -f qcow2 -F raw -b "$DATA_BASE" "$DATA_QCOW2" >/dev/null

  # Seeded pseudo-random kill delay, drawn before boot so the receipt can record
  # the exact value even if the boot fails.
  delay_ms=$(( KILL_MIN_MS + RANDOM % (KILL_MAX_MS - KILL_MIN_MS + 1) ))
  write_start="$(date +%s)"
  qemu_start write "$write_serial"
  qpid="$QPID"

  if ! wait_for_grep "$qpid" "$write_serial" '^GUEST_WRITING' "$BOOT_TIMEOUT"; then
    stop_qemu "$qpid"
    if [[ "$ACCEL" == "kvm" ]]; then
      ACCEL="tcg"
      ACCEL_FALLBACK=1
      say "iteration $i: KVM boot failed; retrying with TCG"
      qemu_start write "$write_serial"
      qpid="$QPID"
      if ! wait_for_grep "$qpid" "$write_serial" '^GUEST_WRITING' "$BOOT_TIMEOUT"; then
        stop_qemu "$qpid"
      fi
    fi
  fi

  if ! grep -q '^GUEST_WRITING' "$write_serial" 2>/dev/null; then
    CONSECUTIVE_BOOT_FAILURES=$((CONSECUTIVE_BOOT_FAILURES + 1))
    say "iteration $i: PL_FAIL boot (see $write_serial)"
    jq -n --argjson iteration "$i" --argjson delay_ms "$delay_ms" \
      --arg error "guest did not reach GUEST_WRITING; accel=$ACCEL" \
      '{iteration:$iteration,kill_delay_ms:$delay_ms,outcome:"fail",error:$error}' >>"$ITER_JSONL"
    if (( CONSECUTIVE_BOOT_FAILURES >= 2 )); then
      BLOCKED=1
      break
    fi
    continue
  fi

  # Require at least one committed write before the clock starts, so the kill
  # lands inside the write loop rather than during boot.
  if ! wait_for_commits "$qpid" "$write_serial" 1 30; then
    stop_qemu "$qpid"
    CONSECUTIVE_BOOT_FAILURES=$((CONSECUTIVE_BOOT_FAILURES + 1))
    say "iteration $i: PL_FAIL no commit before timeout"
    jq -n --argjson iteration "$i" --argjson delay_ms "$delay_ms" \
      --arg error "no COMMIT line before kill window" \
      '{iteration:$iteration,kill_delay_ms:$delay_ms,outcome:"fail",error:$error}' >>"$ITER_JSONL"
    continue
  fi
  CONSECUTIVE_BOOT_FAILURES=0

  sleep "$(printf '%d.%03d' $((delay_ms / 1000)) $((delay_ms % 1000)))"
  stop_qemu "$qpid"
  write_seconds=$(( $(date +%s) - write_start ))

  # Verify boot on the crashed image.
  verify_start="$(date +%s)"
  qemu_start verify "$verify_serial"
  vpid="$QPID"
  wait_for_grep "$vpid" "$verify_serial" '^VERIFY_DONE' "$VERIFY_TIMEOUT" || true
  stop_qemu "$vpid"
  verify_seconds=$(( $(date +%s) - verify_start ))

  # QEMU's serial stream uses CRLF; parse a CR-stripped copy, keep the raw log.
  write_clean="$WORK/iter-${i}-write.clean"
  verify_clean="$WORK/iter-${i}-verify.clean"
  tr -d '\r' <"$write_serial" >"$write_clean"
  tr -d '\r' <"$verify_serial" >"$verify_clean"

  commits_observed="$(grep -cE '^COMMIT .*gen=[0-9]+' "$write_clean" 2>/dev/null || true)"
  commits_observed="${commits_observed:-0}"
  last_gen="$(sed -n 's/^COMMIT .*gen=\([0-9][0-9]*\).*/\1/p' "$write_clean" 2>/dev/null | tail -n 1)"
  last_gen="${last_gen:-0}"

  rs_gen="$(field_from_summary "$verify_clean" RS_SUMMARY gen)"
  rs_root="$(field_from_summary "$verify_clean" RS_SUMMARY root)"
  rs_ok="$(field_from_summary "$verify_clean" RS_SUMMARY ok)"
  rs_status="$(field_from_summary "$verify_clean" RS_SUMMARY status)"
  go_gen="$(field_from_summary "$verify_clean" GO_SUMMARY gen)"
  go_root="$(field_from_summary "$verify_clean" GO_SUMMARY root)"
  go_ok="$(field_from_summary "$verify_clean" GO_SUMMARY ok)"
  go_status="$(field_from_summary "$verify_clean" GO_SUMMARY status)"
  rs_rejected="$(grep '^RS_REJECTED' "$verify_clean" 2>/dev/null | tail -n 1 | cut -d' ' -f2-)"
  go_rejected="$(grep '^GO_REJECTED' "$verify_clean" 2>/dev/null | tail -n 1 | cut -d' ' -f2-)"
  rs_sha="$(grep '^RS_CAT_SHA ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}')"
  go_sha="$(grep '^GO_CAT_SHA ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}')"
  rs_size="$(grep '^RS_CAT_SIZE ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}')"
  go_size="$(grep '^GO_CAT_SIZE ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}')"
  rs_cat_status="$(grep '^RS_CAT_STATUS ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}')"
  go_cat_status="$(grep '^GO_CAT_STATUS ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}')"

  # Verify may be non-clean for two documented crash artifacts: a valid but
  # unpublished commit tail (the "above the highest published" note) and a
  # torn commit file at a generation strictly above the recovered one. Any
  # other problem fails the iteration.
  classify_problems() {
    local prefix="$1" recovered="$2"
    local total line gen
    local unpublished=0 torn=0 bad=0
    total="$(grep "^${prefix}_PROBLEMS total=" "$verify_clean" 2>/dev/null | tail -n 1 | sed 's/.*total=//')"
    [[ "${total:-}" =~ ^[0-9]+$ ]] || total=0
    while IFS= read -r line; do
      if [[ "$line" == *"above the highest published superblock generation"* ]]; then
        unpublished=$((unpublished + 1))
      elif [[ "$line" =~ commits/([0-9]+)\.commit ]]; then
        gen="${BASH_REMATCH[1]}"
        if (( gen > recovered )); then
          torn=$((torn + 1))
        else
          bad=$((bad + 1))
        fi
      else
        bad=$((bad + 1))
      fi
    done < <(grep "^${prefix}_PROBLEM_DETAIL " "$verify_clean" 2>/dev/null | sed "s/^${prefix}_PROBLEM_DETAIL //")
    (( total == unpublished + torn + bad )) || bad=$((bad + 1))
    printf '%s %s %s %s\n' "$total" "$unpublished" "$torn" "$bad"
  }

  read -r rs_problems rs_unpublished rs_torn rs_bad <<<"$(classify_problems RS "$rs_gen")"
  read -r go_problems go_unpublished go_torn go_bad <<<"$(classify_problems GO "$go_gen")"

  rs_accept=0
  if (( rs_bad == 0 && rs_problems == rs_unpublished + rs_torn )); then
    if (( rs_problems == 0 && rs_ok == 1 )) || (( rs_problems > 0 && rs_ok == 0 )); then
      rs_accept=1
    fi
  fi
  go_accept=0
  if (( go_bad == 0 && go_problems == go_unpublished + go_torn )); then
    if (( go_problems == 0 && go_ok == 1 )) || (( go_problems > 0 && go_ok == 0 )); then
      go_accept=1
    fi
  fi
  control_size="$(grep '^CONTROL_UNSYNCED_SIZE ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}')"
  if ! [[ "${control_size:-}" =~ ^[0-9]+$ ]]; then
    control_size=-1
  fi
  control_loss="inconclusive"
  if (( control_size < commits_observed )); then
    control_loss="loss-observed"
  fi

  rs_gen="${rs_gen:-0}"
  go_gen="${go_gen:-0}"
  rs_ok="${rs_ok:-0}"
  go_ok="${go_ok:-0}"
  rs_status="${rs_status:-1}"
  go_status="${go_status:-1}"
  rs_cat_status="${rs_cat_status:-1}"
  go_cat_status="${go_cat_status:-1}"

  expected_sha=""
  size_ok=0
  sha_ok=0
  if (( rs_gen >= 2 )); then
    n=$((rs_gen - 2))
    if build_expected "$n" "$WORK/expected-${i}.bin"; then
      expected_sha="$(sha256sum "$WORK/expected-${i}.bin" | cut -d' ' -f1)"
      [[ "$rs_size" == "$FILE_SIZE" && "$go_size" == "$FILE_SIZE" ]] && size_ok=1
      [[ -n "$rs_sha" && "$rs_sha" == "$expected_sha" && \
         -n "$go_sha" && "$go_sha" == "$expected_sha" ]] && sha_ok=1
    fi
  fi

  cross_impl_ok=0
  [[ "$rs_gen" == "$go_gen" && -n "$rs_root" && "$rs_root" == "$go_root" && \
     "$rs_accept" == "1" && "$go_accept" == "1" && \
     "$rs_sha" == "$go_sha" ]] && cross_impl_ok=1

  durability_ok=0
  (( last_gen == 0 || rs_gen >= last_gen )) && durability_ok=1

  outcome="fail"
  error=""
  if (( rs_gen < 2 )); then
    error="recovered generation below init+create (${rs_gen})"
  elif ! grep -q '^VERIFY_DONE' "$verify_clean" 2>/dev/null; then
    error="verification boot did not finish"
  elif (( rs_accept == 0 || go_accept == 0 )); then
    error="verify --full reported unexpected problems (rust_ok=${rs_ok} go_ok=${go_ok} rust_bad=${rs_bad} go_bad=${go_bad})"
  elif (( size_ok == 0 )); then
    error="recovered file size mismatch (rust=${rs_size:-none} go=${go_size:-none})"
  elif (( sha_ok == 0 )); then
    error="recovered content is not the exact state of generation ${rs_gen}"
  elif (( cross_impl_ok == 0 )); then
    error="Rust and Go disagree (rust_gen=${rs_gen} go_gen=${go_gen})"
  elif (( durability_ok == 0 )); then
    error="returned commit ${last_gen} was lost (recovered ${rs_gen})"
  elif [[ "$rs_gen" -eq "$last_gen" ]]; then
    outcome="old"
  else
    outcome="new"
  fi

  say "iteration $i: outcome=$outcome recovered=$rs_gen go=$go_gen last_returned=$last_gen delay=${delay_ms}ms commits=$commits_observed"

  jq -n \
    --argjson iteration "$i" \
    --argjson kill_delay_ms "$delay_ms" \
    --argjson commits_observed "$commits_observed" \
    --argjson last_returned_generation "$last_gen" \
    --argjson write_boot_seconds "$write_seconds" \
    --argjson verify_boot_seconds "$verify_seconds" \
    --argjson rust_generation "$rs_gen" \
    --argjson go_generation "$go_gen" \
    --arg rust_root "$rs_root" \
    --arg go_root "$go_root" \
    --arg rust_rejected "$rs_rejected" \
    --arg go_rejected "$go_rejected" \
    --argjson rust_verify_ok "$rs_ok" \
    --argjson go_verify_ok "$go_ok" \
    --argjson rust_verify_accepted "$rs_accept" \
    --argjson go_verify_accepted "$go_accept" \
    --argjson rust_problems "$rs_problems" \
    --argjson go_problems "$go_problems" \
    --argjson rust_unpublished_tail "$rs_unpublished" \
    --argjson rust_torn_unpublished "$rs_torn" \
    --argjson go_unpublished_tail "$go_unpublished" \
    --argjson go_torn_unpublished "$go_torn" \
    --argjson rust_verify_status "$rs_status" \
    --argjson go_verify_status "$go_status" \
    --argjson rust_cat_status "$rs_cat_status" \
    --argjson go_cat_status "$go_cat_status" \
    --arg rust_cat_sha256 "$rs_sha" \
    --arg go_cat_sha256 "$go_sha" \
    --arg expected_sha256 "$expected_sha" \
    --argjson size_ok "$size_ok" \
    --argjson sha_ok "$sha_ok" \
    --argjson cross_impl_ok "$cross_impl_ok" \
    --argjson durability_ok "$durability_ok" \
    --argjson control_size "$control_size" \
    --arg control_loss "$control_loss" \
    --arg outcome "$outcome" \
    --arg error "$error" \
    '{iteration:$iteration,kill_delay_ms:$kill_delay_ms,
      commits_observed:$commits_observed,
      last_returned_generation:$last_returned_generation,
      write_boot_seconds:$write_boot_seconds,verify_boot_seconds:$verify_boot_seconds,
      rust_generation:$rust_generation,go_generation:$go_generation,
      snapshot_root:{rust:$rust_root,go:$go_root},
      rejected_generations:{rust:$rust_rejected,go:$go_rejected},
      rust_verify_ok:$rust_verify_ok,go_verify_ok:$go_verify_ok,
      rust_verify_accepted:$rust_verify_accepted,go_verify_accepted:$go_verify_accepted,
      problems:{rust:{total:$rust_problems,unpublished_tail:$rust_unpublished_tail,
                       torn_unpublished:$rust_torn_unpublished},
                go:{total:$go_problems,unpublished_tail:$go_unpublished_tail,
                    torn_unpublished:$go_torn_unpublished}},
      rust_verify_status:$rust_verify_status,go_verify_status:$go_verify_status,
      rust_cat_status:$rust_cat_status,go_cat_status:$go_cat_status,
      rust_cat_sha256:$rust_cat_sha256,go_cat_sha256:$go_cat_sha256,
      expected_sha256:$expected_sha256,size_ok:$size_ok,sha_ok:$sha_ok,
      cross_impl_ok:$cross_impl_ok,durability_ok:$durability_ok,
      unsynced_control_size:$control_size,unsynced_control_verdict:$control_loss,
      outcome:$outcome,error:$error}' >>"$ITER_JSONL"

  if (( i < ITERS )) && (( BLOCKED == 0 )); then
    :
  fi
done

# ---- evidence log and receipt --------------------------------------------
{
  echo "===== combined serial logs ====="
  for ((i = 1; i <= ITERS; i++)); do
    f="$WORK/iter-${i}-write.log"
    [[ -e "$f" ]] && { echo "===== iter $i write ====="; cat "$f"; }
    f="$WORK/iter-${i}-verify.log"
    [[ -e "$f" ]] && { echo "===== iter $i verify ====="; cat "$f"; }
  done
} >>"$LOG"

SUMMARY_JSON="$(jq -s '{
  iterations: length,
  pass: (map(select(.outcome == "old" or .outcome == "new")) | length),
  fail: (map(select(.outcome == "fail")) | length),
  old: (map(select(.outcome == "old")) | length),
  new: (map(select(.outcome == "new")) | length),
  partial: (map(select(.sha_ok == 0 or .size_ok == 0)) | length),
  durability_failures: (map(select(.durability_ok == 0)) | length),
  verify_failures: (map(select(.rust_verify_accepted == 0 or .go_verify_accepted == 0)) | length),
  iterations_with_unpublished_tail: (map(select(((.problems.rust.unpublished_tail + .problems.rust.torn_unpublished + .problems.go.unpublished_tail + .problems.go.torn_unpublished) > 0))) | length),
  cross_impl_disagreements: (map(select(.cross_impl_ok == 0)) | length),
  unsynced_control_loss_observed: (map(select(.unsynced_control_verdict == "loss-observed")) | length)
}' "$ITER_JSONL")"

FINISHED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
DURATION_MS=$(( ($(date +%s) - START_EPOCH) * 1000 ))
LOG_SHA="$(sha256sum "$LOG" | cut -d' ' -f1)"
REV_WEFTFS="${WEFT_REPO_WEFTFS:-unknown}"
REV_GO="${WEFT_REPO_WEFTFS_GO:-unknown}"
QEMU_CMD="qemu-system-x86_64 -accel $ACCEL -cpu $([[ "$ACCEL" == kvm ]] && echo host || echo max) -smp 1 -m ${GUEST_MEM_MB}M -kernel $KERNEL -initrd $INITRD -append 'root=/dev/vda ro rootfstype=ext4 rootwait console=ttyS0,115200 panic=-1 loglevel=4 weft.mode=<write|verify> weft.iters=100000' -drive file=$ROOT_IMG,if=virtio,format=raw,readonly=on -drive file=$DATA_QCOW2,if=virtio,format=qcow2,cache=$CACHE_MODE -display none -vga none -no-user-config -nodefaults -no-reboot -serial stdio -monitor none"
COURT_CMD="scripts/docker-court.sh vm \"POWERLOSS_ITERATIONS=$ITERS POWERLOSS_SEED=$SEED POWERLOSS_CACHE_MODE=$CACHE_MODE bash scripts/powerloss_court.sh\""
GUEST_WRITE_CMD="weftfs --store /data/store init --chunk-size 65536 --checkpoint-interval 5 --gc-grace-secs 0; weftfs --store /data/store create /f --file /tmp/base --seed powerloss-base; loop: weftfs --store /data/store --json write /f --offset 0 --file /tmp/payload"
GUEST_VERIFY_CMD="weftfs --store /data/store --json info; weftfs --store /data/store --json verify --full; weftfs --store /data/store cat /f; go-weftfs --store /data/store --json info; go-weftfs --store /data/store --json verify --full; go-weftfs --store /data/store cat /f"

jq -n \
  --arg schema "weftfs.powerloss.receipt/1" \
  --arg generated_at "$FINISHED_AT" \
  --arg started_at "$STARTED_AT" \
  --arg finished_at "$FINISHED_AT" \
  --argjson duration_ms "$DURATION_MS" \
  --arg qemu_version "$qemu_version" \
  --arg accel "$ACCEL" \
  --argjson accel_fallback "$ACCEL_FALLBACK" \
  --arg cache_mode "$CACHE_MODE" \
  --arg kernel "$KERNEL_REL" \
  --arg initrd "$(basename "$INITRD")" \
  --arg qemu_cmd "$QEMU_CMD" \
  --arg court_cmd "$COURT_CMD" \
  --arg guest_write_cmd "$GUEST_WRITE_CMD" \
  --arg guest_verify_cmd "$GUEST_VERIFY_CMD" \
  --arg court "${WEFT_COURT_IMAGE:-unknown}" \
  --arg image_id "${WEFT_COURT_IMAGE_ID:-unknown}" \
  --arg host_kernel "$(uname -a)" \
  --arg limit_memory "${WEFT_COURT_MEMORY:-?}" \
  --arg limit_cpus "${WEFT_COURT_CPUS:-?}" \
  --arg limit_pids "${WEFT_COURT_PIDS:-?}" \
  --argjson iterations_requested "$ITERS" \
  --argjson seed "$SEED" \
  --argjson guest_mem_mb "$GUEST_MEM_MB" \
  --argjson file_size "$FILE_SIZE" \
  --argjson payload_size "$PAYLOAD_SIZE" \
  --argjson kill_min_ms "$KILL_MIN_MS" \
  --argjson kill_max_ms "$KILL_MAX_MS" \
  --argjson boot_timeout "$BOOT_TIMEOUT" \
  --argjson verify_timeout "$VERIFY_TIMEOUT" \
  --arg rev_weftfs "$REV_WEFTFS" \
  --arg rev_go "$REV_GO" \
  --arg log "$LOG" \
  --arg log_sha "$LOG_SHA" \
  --argjson blocked "$BLOCKED" \
  --argjson summary "$SUMMARY_JSON" \
  --slurpfile iterations "$ITER_JSONL" \
  '{
    schema: $schema,
    generated_at: $generated_at,
    run: {started_at: $started_at, finished_at: $finished_at, duration_ms: $duration_ms},
    rig: {
      type: "qemu-virtual-machine",
      claim: "VM power-cut proxy: SIGKILL of QEMU loses guest page cache and QEMU user-space state; not physical media power loss and not host/device cache loss",
      available: ($blocked == 0),
      blockers: (if $blocked == 0 then [] else ["guest did not boot twice consecutively"] end),
      qemu_version: $qemu_version,
      accel: $accel,
      accel_fallback_to_tcg: ($accel_fallback == 1),
      cache_mode: $cache_mode,
      kernel: $kernel,
      initramfs: $initrd,
      root_disk: "raw ext4 160 MiB, mounted read-only",
      data_disk: "qcow2 overlay on raw ext4 128 MiB, fresh per iteration",
      guest_filesystem: "ext4 data=ordered barrier=1; no fsck",
      kill: "SIGKILL to qemu-system-x86_64 with no guest shutdown and no cache flush",
      qemu_command_template: $qemu_cmd,
      guest_init: "verification/powerloss/guest-init.sh"
    },
    environment: {
      court: $court, image_id: $image_id, host_kernel: $host_kernel,
      limit_memory: $limit_memory, limit_memory_swap: $limit_memory,
      limit_cpus: $limit_cpus, limit_pids: $limit_pids, tmpfs_tmp: "1g"
    },
    repositories: [
      {name: "WeftFS", revision: $rev_weftfs},
      {name: "WeftFS-Go", revision: $rev_go}
    ],
    limits: {
      iterations: $iterations_requested, seed: $seed,
      guest_memory_mb: $guest_mem_mb, file_size_bytes: $file_size,
      payload_size_bytes: $payload_size,
      kill_delay_ms: {min: $kill_min_ms, max: $kill_max_ms},
      boot_timeout_seconds: $boot_timeout, verify_timeout_seconds: $verify_timeout,
      no_fsck: true, sync_after_commit_in_guest: false,
      unsynced_control: "one byte appended to /data/unsynced-control after each returned commit, never fsynced; a size below the observed commit count proves the rig discarded guest page cache"
    },
    iterations: $iterations,
    summary: $summary,
    evidence: {raw_log: $log, raw_log_sha256: $log_sha},
    commands: {
      court: $court_cmd,
      harness: "bash scripts/powerloss_court.sh",
      guest_write: $guest_write_cmd,
      guest_verify: $guest_verify_cmd,
      qemu: $qemu_cmd
    },
    scope: {
      asserted: "for every completed iteration, both implementations recover the same generation; that state is exactly the expected whole state; every commit that returned before SIGKILL is still present; verify --full is clean",
      not_asserted: [
        "physical power loss (media/device write-cache loss)",
        "host page-cache loss",
        "fsync that lies to the guest",
        "power loss during mkfs or during the read-only root mount",
        "10,000-iteration campaign"
      ]
    }
  }' >"$RECEIPT"

say "receipt: $RECEIPT"
say "raw log: $LOG"
sha256sum "$RECEIPT" "$LOG"

fail_count="$(jq -r '.summary.fail' "$RECEIPT")"
if (( BLOCKED == 1 || fail_count > 0 )); then
  say "PL_FAIL failed iterations or blocked rig"
  exit 1
fi
say "PL_PASS iterations=$ITERS"
