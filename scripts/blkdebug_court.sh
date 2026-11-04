#!/usr/bin/env bash
# F9.3 — block-layer lying-disk court.
#
# Host usage (self-dispatches into the hard-limited VM court):
#   scripts/blkdebug_court.sh
#
# Court usage:
#   scripts/docker-court.sh vm "bash scripts/blkdebug_court.sh"
#
# Rig (all inside Docker; the host only runs Docker):
#   - Debian kernel + initramfs from the vm court image
#   - read-only raw ext4 root disk, guest PID 1 = verification/powerloss/guest-init.sh
#   - one clean prepare boot creates the store and base file on a raw ext4 image;
#     every fault iteration starts from a byte copy of that prepared image
#   - QEMU blkdebug wraps the raw data disk and injects block-layer events:
#       write-error       EIO on every write_aio
#       write-error-once  EIO on the first write_aio, then normal service
#       flush-error       EIO on every flush_to_os
#       write-drop-all    QEMU snapshot=on: every write/flush is acknowledged and
#                         written to a temporary overlay that is discarded on
#                         SIGKILL, so no acknowledged write is persisted
#       control           no injection, cache=none
#   - the host waits for GUEST_WRITING and one returned COMMIT, sleeps a seeded
#     pseudo-random delay, then SIGKILLs qemu-system-x86_64
#   - the surviving image is booted again; Rust and Go independently open,
#     verify --full, and read the file; both are compared to the exact expected
#     whole state for the recovered generation
#
# This is a block-layer lying-disk model, NOT physical media power loss. QEMU
# 7.2's blkdebug has no silent write-drop primitive: a config with errno=0
# still writes the bytes (tested with qemu-io), and flush_do_sync is not an
# accepted event. write-drop-all is therefore modelled with QEMU's volatile
# snapshot overlay, and the receipt labels it as total write drop. Host page
# cache is not dropped and media caches are not physically removed.
#
# Limits: inherited Docker limits (3g memory/3g swap/4 cpus/1024 pids, 1g
# bounded /tmp), guest 512 MiB, root disk 160 MiB read-only, data disk 128 MiB
# raw ext4, bounded modes/iterations/boot timeouts. Nothing disables a limit.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  exec "$root/scripts/docker-court.sh" vm "bash scripts/blkdebug_court.sh"
fi
cd "$root"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
START_EPOCH="$(date +%s)"
RECEIPT="bench/receipts/blkdebug-${STAMP}.json"
LOG="bench/receipts/blkdebug-${STAMP}.log"
mkdir -p bench/receipts

MODES="${BLKDEBUG_MODES:-control,dm-flakey-drop,dm-flakey-error,write-drop-all,blkdebug-write-error,blkdebug-flush-error}"
ITERS="${BLKDEBUG_ITERATIONS:-6}"
SEED="${BLKDEBUG_SEED:-20261104}"
BOOT_TIMEOUT="${BLKDEBUG_BOOT_TIMEOUT:-120}"
VERIFY_TIMEOUT="${BLKDEBUG_VERIFY_TIMEOUT:-180}"
KILL_MIN_MS="${BLKDEBUG_KILL_MIN_MS:-100}"
KILL_MAX_MS="${BLKDEBUG_KILL_MAX_MS:-1000}"
GUEST_MEM_MB=512
FILE_SIZE=1048576
PAYLOAD_SIZE=65536

[[ "$ITERS" =~ ^[1-9][0-9]*$ ]] || { echo "invalid BLKDEBUG_ITERATIONS" >&2; exit 64; }
(( KILL_MIN_MS < KILL_MAX_MS )) || { echo "invalid kill delay range" >&2; exit 64; }
for m in ${MODES//,/ }; do
  case "$m" in
    control|dm-flakey-drop|dm-flakey-error|write-drop-all|blkdebug-write-error|blkdebug-write-error-once|blkdebug-flush-error) ;;
    *) echo "invalid mode '$m' in BLKDEBUG_MODES=$MODES" >&2; exit 64 ;;
  esac
done

WORK="$root/.docker-cache/blkdebug/$STAMP"
BIN="$WORK/bin"
ROOTFS="$WORK/rootfs"
ROOT_IMG="$WORK/root.img"
DATA_BASE="$WORK/data-base.img"
RUN_IMG="$WORK/run.img"
mkdir -p "$BIN" "$ROOTFS" "$WORK"
chmod -R a+rwX "$WORK"

say() { echo "$*" | tee -a "$LOG"; }
: >"$LOG"

say "== WeftFS block-layer lying-disk court =="
say "started: $STARTED_AT"
say "kernel: $(uname -a)"
say "court image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
say "limits: memory=${WEFT_COURT_MEMORY:-?} swap=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?} tmpfs_tmp=1g"
say "modes=$MODES iterations_per_mode=$ITERS seed=$SEED guest_mem=${GUEST_MEM_MB}M kill_delay_ms=${KILL_MIN_MS}..${KILL_MAX_MS}"
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
              ln ls true false ps kill env blockdev; do
  ln -sf /bin/busybox "$ROOTFS/bin/$applet"
done
cp "$BIN/weftfs" "$ROOTFS/usr/bin/weftfs"
cp "$BIN/go-weftfs" "$ROOTFS/usr/bin/go-weftfs"
cp verification/powerloss/guest-init.sh "$ROOTFS/sbin/init"
chmod 755 "$ROOTFS/sbin/init"
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

truncate -s 128M "$DATA_BASE"
mke2fs -t ext4 -F -q -b 4096 "$DATA_BASE"

KERNEL="$(ls -1 /boot/vmlinuz-*-amd64 2>/dev/null | sort -V | tail -n 1)"
KDIR=""
INITRD=""
if [[ -n "$KERNEL" ]]; then
  KREL="${KERNEL#/boot/vmlinuz-}"
  INITRD="/boot/initrd.img-${KREL}"
  [[ -r "$INITRD" ]] || INITRD="$(ls -1 /boot/initrd.img-* 2>/dev/null | sort -V | tail -n 1)"
fi
if [[ -z "$KERNEL" || ! -r "${INITRD:-}" ]]; then
  say "BLK_BLOCKER no_kernel_or_initrd kernel=${KERNEL:-none} initrd=${INITRD:-none}"
  exit 2
fi
say "guest kernel: $KERNEL"
say "guest initramfs: $INITRD ($(stat -c %s "$INITRD") bytes)"

# dm-flakey modules and dmsetup are needed only for the dm-flakey modes, but
# they are included in the read-only guest root so every mode boots the same
# image. The modules are the court kernel's own Debian modules.
MODSRC="/lib/modules/$KREL/kernel/drivers/md"
if [[ -e "$MODSRC/dm-mod.ko" && -e "$MODSRC/dm-flakey.ko" ]]; then
  mkdir -p "$ROOTFS/lib/modules/$KREL/kernel/drivers/md"
  cp "$MODSRC/dm-mod.ko" "$MODSRC/dm-flakey.ko" "$ROOTFS/lib/modules/$KREL/kernel/drivers/md/"
  say "guest dm modules: dm-mod.ko dm-flakey.ko"
else
  say "BLK_WARN dm-flakey modules missing under $MODSRC"
fi
if [[ -x /usr/sbin/dmsetup ]]; then
  cp -L /usr/sbin/dmsetup "$ROOTFS/usr/bin/dmsetup"
  chmod 755 "$ROOTFS/usr/bin/dmsetup"
  ldd /usr/sbin/dmsetup | awk '$2 == "=>" && $3 ~ /^\// {print $3} $1 ~ /^\// {print $1}' | sort -u |
  while read -r lib; do
    [ -e "$lib" ] || continue
    mkdir -p "$ROOTFS$(dirname "$lib")"
    cp -L "$lib" "$ROOTFS$lib"
  done
  say "guest dmsetup: $(/usr/sbin/dmsetup --version 2>&1 | head -n 1)"
fi

# The root image is built after every guest file (including dm modules and
# dmsetup) has been staged.
truncate -s 160M "$ROOT_IMG"
mke2fs -t ext4 -F -q -b 4096 -d "$ROOTFS" "$ROOT_IMG"
say "root image: $ROOT_IMG ($(stat -c %s "$ROOT_IMG") bytes)"

qemu_version="$(qemu-system-x86_64 --version | head -n 1)"
KERNEL_REL="$(basename "$KERNEL")"

# ---- execution ------------------------------------------------------------
ACCEL="tcg"
if [[ -r /dev/kvm && -w /dev/kvm ]]; then
  ACCEL="kvm"
fi
ACCEL_FALLBACK=0

# Config text is written verbatim into the receipt.
mode_config() {
  case "$1" in
    control|dm-flakey-drop|dm-flakey-error|write-drop-all) printf '%s' "" ;;
    blkdebug-write-error) printf '[inject-error]\nevent = "write_aio"\nerrno = "5"\nonce = "off"\n' ;;
    blkdebug-write-error-once) printf '[inject-error]\nevent = "write_aio"\nerrno = "5"\nonce = "on"\n' ;;
    blkdebug-flush-error) printf '[inject-error]\nevent = "flush_to_os"\nerrno = "5"\nonce = "off"\n' ;;
    *) return 1 ;;
  esac
}

mode_cache() {
  case "$1" in
    control) echo none ;;
    dm-flakey-drop|dm-flakey-error) echo none ;;
    write-drop-all) echo writeback ;;
    blkdebug-*) echo writeback ;;
    *) echo none ;;
  esac
}

mode_drive() {
  local mode="$1" img="$2" cfg="$3"
  case "$mode" in
    control|dm-flakey-drop|dm-flakey-error) echo "file=$img,if=virtio,format=raw,cache=none" ;;
    write-drop-all) echo "file=$img,if=virtio,format=raw,snapshot=on,cache=writeback" ;;
    blkdebug-*) echo "file=blkdebug:$cfg:$img,if=virtio,format=raw,cache=writeback" ;;
    *) return 1 ;;
  esac
}

is_blocking_mode() {
  [[ "$1" == blkdebug-* ]]
}

qemu_start_write() {
  local mode="$1" img="$2" cfg="$3" serial="$4"
  local drive cpu="max"
  if [[ "$ACCEL" == "kvm" ]]; then cpu="host"; fi
  drive="$(mode_drive "$mode" "$img" "$cfg")"
  qemu-system-x86_64 \
    -accel "$ACCEL" -cpu "$cpu" -smp 1 -m "${GUEST_MEM_MB}M" \
    -kernel "$KERNEL" -initrd "$INITRD" \
    -append "root=/dev/vda ro rootfstype=ext4 rootwait console=ttyS0,115200 panic=-1 loglevel=4 weft.mode=write weft.iters=100000 weft.fault=${mode} weft.dm_up_secs=2" \
    -drive "file=$ROOT_IMG,if=virtio,format=raw,readonly=on" \
    -drive "$drive" \
    -display none -vga none -no-user-config -nodefaults -no-reboot \
    -serial stdio -monitor none </dev/null >"$serial" 2>&1 &
  QPID=$!
}

qemu_start_verify() {
  local img="$1" serial="$2"
  local cpu="max"
  if [[ "$ACCEL" == "kvm" ]]; then cpu="host"; fi
  qemu-system-x86_64 \
    -accel "$ACCEL" -cpu "$cpu" -smp 1 -m "${GUEST_MEM_MB}M" \
    -kernel "$KERNEL" -initrd "$INITRD" \
    -append "root=/dev/vda ro rootfstype=ext4 rootwait console=ttyS0,115200 panic=-1 loglevel=4 weft.mode=verify weft.iters=0" \
    -drive "file=$ROOT_IMG,if=virtio,format=raw,readonly=on" \
    -drive "file=$img,if=virtio,format=raw,cache=none" \
    -display none -vga none -no-user-config -nodefaults -no-reboot \
    -serial stdio -monitor none </dev/null >"$serial" 2>&1 &
  QPID=$!
}

qemu_start_prepare() {
  local img="$1" serial="$2"
  local cpu="max"
  if [[ "$ACCEL" == "kvm" ]]; then cpu="host"; fi
  qemu-system-x86_64 \
    -accel "$ACCEL" -cpu "$cpu" -smp 1 -m "${GUEST_MEM_MB}M" \
    -kernel "$KERNEL" -initrd "$INITRD" \
    -append "root=/dev/vda ro rootfstype=ext4 rootwait console=ttyS0,115200 panic=-1 loglevel=4 weft.mode=prepare weft.iters=0" \
    -drive "file=$ROOT_IMG,if=virtio,format=raw,readonly=on" \
    -drive "file=$img,if=virtio,format=raw,cache=none" \
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
  grep -q "$pattern" "$file" 2>/dev/null
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
  local gen="$1" out="$2"
  if (( gen == 2 )); then
    head -c "$FILE_SIZE" /dev/zero >"$out"
    return 0
  fi
  (( gen > 2 )) || return 1
  local n=$((gen - 2))
  local prefix="POWERLOSS:${n}:"
  local pad=$((PAYLOAD_SIZE - ${#prefix}))
  (( pad >= 0 )) || return 1
  { printf '%s' "$prefix"; head -c "$pad" /dev/zero; head -c $((FILE_SIZE - PAYLOAD_SIZE)) /dev/zero; } >"$out"
}

field_from_summary() {
  local file="$1" prefix="$2" key="$3"
  grep "^${prefix} " "$file" 2>/dev/null | tail -n 1 | sed -n "s/.* ${key}=\([^ ]*\).*/\1/p" || true
}

classify_problems() {
  local file="$1" prefix="$2" recovered="$3"
  local total line gen
  local unpublished=0 torn=0 bad=0
  total="$(grep "^${prefix}_PROBLEMS total=" "$file" 2>/dev/null | tail -n 1 | sed 's/.*total=//')"
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
  done < <(grep "^${prefix}_PROBLEM_DETAIL " "$file" 2>/dev/null | sed "s/^${prefix}_PROBLEM_DETAIL //")
  (( total == unpublished + torn + bad )) || bad=$((bad + 1))
  printf '%s %s %s %s\n' "$total" "$unpublished" "$torn" "$bad"
}

# ---- one clean prepare boot ----------------------------------------------
say "== prepare base store (no fault injection) =="
prep_serial="$WORK/prepare.log"
qemu_start_prepare "$DATA_BASE" "$prep_serial"
ppid="$QPID"
wait_for_grep "$ppid" "$prep_serial" '^PREPARE_END' "$BOOT_TIMEOUT" || true
stop_qemu "$ppid"
prep_clean="$WORK/prepare.clean"
tr -d '\r' <"$prep_serial" >"$prep_clean"
if ! grep -q '^PREPARE_END' "$prep_clean"; then
  say "BLK_BLOCKER prepare_boot_failed"
  sed -n '1,80p' "$prep_clean" | tee -a "$LOG"
  exit 2
fi
prep_gen="$(sed -n 's/^PREPARE_DONE gen=\([0-9][0-9]*\).*/\1/p' "$prep_clean" | tail -n 1)"
say "prepare generation=${prep_gen:-unknown}"
if [[ "$prep_gen" != "2" ]]; then
  say "BLK_BLOCKER prepare_generation_not_2 (${prep_gen:-none})"
  exit 2
fi

# Validate a copy of the prepared base with both implementations. The verify
# boot mounts read-write, so it must not touch the pristine base image.
BASE_VERIFY_IMG="$WORK/base-verify.img"
cp --sparse=always "$DATA_BASE" "$BASE_VERIFY_IMG"
base_verify_serial="$WORK/base-verify.log"
qemu_start_verify "$BASE_VERIFY_IMG" "$base_verify_serial"
bpid="$QPID"
wait_for_grep "$bpid" "$base_verify_serial" '^VERIFY_DONE' "$VERIFY_TIMEOUT" || true
stop_qemu "$bpid"
tr -d '\r' <"$base_verify_serial" >"$WORK/base-verify.clean"
base_rs_gen="$(field_from_summary "$WORK/base-verify.clean" RS_SUMMARY gen)"
base_go_gen="$(field_from_summary "$WORK/base-verify.clean" GO_SUMMARY gen)"
base_rs_ok="$(field_from_summary "$WORK/base-verify.clean" RS_SUMMARY ok)"
base_go_ok="$(field_from_summary "$WORK/base-verify.clean" GO_SUMMARY ok)"
if [[ "$base_rs_gen" != "2" || "$base_go_gen" != "2" || "$base_rs_ok" != "1" || "$base_go_ok" != "1" ]]; then
  say "BLK_BLOCKER base_verify_failed rs_gen=${base_rs_gen:-none} go_gen=${base_go_gen:-none} rs_ok=${base_rs_ok:-none} go_ok=${base_go_ok:-none}"
  exit 2
fi
say "base verify: rust gen=2 ok=1, go gen=2 ok=1"

ITER_JSONL="$WORK/iterations.jsonl"
: >"$ITER_JSONL"
CONSECUTIVE_BOOT_FAILURES=0
BLOCKED=0
RANDOM="$SEED"
iteration=0

for mode in ${MODES//,/ }; do
  cfg="$WORK/blkdebug-${mode}.cfg"
  mode_config "$mode" >"$cfg"
  say "== mode $mode =="
  say "drive_cache=$(mode_cache "$mode")"
  if [[ -s "$cfg" ]]; then
    say "blkdebug_config:"
    sed 's/^/  /' "$cfg" | tee -a "$LOG"
  else
    say "blkdebug_config: (none)"
  fi
  if [[ "$mode" == "write-drop-all" ]]; then
    say "device_model=snapshot=on: writes are acknowledged into a volatile overlay discarded by SIGKILL; total write drop, not selective media loss"
  fi

  for ((k = 1; k <= ITERS; k++)); do
    iteration=$((iteration + 1))
    write_serial="$WORK/$mode-$k-write.log"
    verify_serial="$WORK/$mode-$k-verify.log"
    rm -f "$RUN_IMG"
    cp --sparse=always "$DATA_BASE" "$RUN_IMG"

    delay_ms=$(( KILL_MIN_MS + RANDOM % (KILL_MAX_MS - KILL_MIN_MS + 1) ))
    effective_delay_ms="$delay_ms"
    if [[ "$mode" == dm-flakey-* ]]; then
      # dm-flakey is up for 2s from device creation so mount and the first
      # commits complete; add that interval plus 300ms so SIGKILL lands in the
      # down (fault) window.
      effective_delay_ms=$((delay_ms + 2000 + 300))
    fi
    qemu_start_write "$mode" "$RUN_IMG" "$cfg" "$write_serial"
    qpid="$QPID"

    reached=0
    if wait_for_grep "$qpid" "$write_serial" '^GUEST_WRITING' "$BOOT_TIMEOUT"; then
      reached=1
    elif [[ "$ACCEL" == "kvm" ]]; then
      stop_qemu "$qpid"
      ACCEL="tcg"
      ACCEL_FALLBACK=1
      say "iteration $iteration ($mode/$k): KVM boot failed; retrying with TCG"
      rm -f "$RUN_IMG"
      cp --sparse=always "$DATA_BASE" "$RUN_IMG"
      qemu_start_write "$mode" "$RUN_IMG" "$cfg" "$write_serial"
      qpid="$QPID"
      if wait_for_grep "$qpid" "$write_serial" '^GUEST_WRITING' "$BOOT_TIMEOUT"; then
        reached=1
      fi
    fi

    if (( reached == 0 )); then
      stop_qemu "$qpid"
      tr -d '\r' <"$write_serial" >"$WORK/$mode-$k-write.clean"
      say "iteration $iteration ($mode/$k): boot_fail (no GUEST_WRITING)"
      jq -n --arg mode "$mode" --argjson iteration "$iteration" --argjson k "$k" \
        --argjson delay_ms "$delay_ms" --arg error "guest did not reach GUEST_WRITING" \
        '{mode:$mode,iteration:$iteration,within_mode:$k,kill_delay_ms:$delay_ms,outcome:"boot_fail",error:$error}' >>"$ITER_JSONL"
      if is_blocking_mode "$mode"; then
        # blkdebug injects at device open, which lands in ext4's mount
        # superblock write. Record the boot failure as a blocker; do not treat
        # it as a rig failure or substitute a different result.
        CONSECUTIVE_BOOT_FAILURES=0
        continue
      fi
      CONSECUTIVE_BOOT_FAILURES=$((CONSECUTIVE_BOOT_FAILURES + 1))
      if (( CONSECUTIVE_BOOT_FAILURES >= 2 )); then
        BLOCKED=1
        break
      fi
      continue
    fi
    CONSECUTIVE_BOOT_FAILURES=0

    # A fault may stop the write loop before a commit. Wait briefly for the
    # first commit, then use the remaining seeded delay and SIGKILL regardless.
    wait_for_commits "$qpid" "$write_serial" 1 20 || true
    sleep "$(printf '%d.%03d' $((effective_delay_ms / 1000)) $((effective_delay_ms % 1000)))"
    guest_alive=1
    kill -0 "$qpid" 2>/dev/null || guest_alive=0
    stop_qemu "$qpid"

    verify_img="$RUN_IMG"
    if [[ "$mode" == "write-drop-all" ]]; then verify_img="$DATA_BASE"; fi

    qemu_start_verify "$verify_img" "$verify_serial"
    vpid="$QPID"
    wait_for_grep "$vpid" "$verify_serial" '^VERIFY_DONE' "$VERIFY_TIMEOUT" || true
    stop_qemu "$vpid"

    write_clean="$WORK/$mode-$k-write.clean"
    verify_clean="$WORK/$mode-$k-verify.clean"
    tr -d '\r' <"$write_serial" >"$write_clean"
    tr -d '\r' <"$verify_serial" >"$verify_clean"

    commits_observed="$(grep -cE '^COMMIT .*gen=[0-9]+' "$write_clean" 2>/dev/null || true)"
    commits_observed="${commits_observed:-0}"
    commit_failures="$(grep -c '^COMMIT_FAIL' "$write_clean" 2>/dev/null || true)"
    commit_failures="${commit_failures:-0}"
    last_gen="$(sed -n 's/^COMMIT .*gen=\([0-9][0-9]*\).*/\1/p' "$write_clean" 2>/dev/null | tail -n 1 || true)"
    last_gen="${last_gen:-0}"
    write_loop_exit="$(grep '^WRITE_LOOP_EXIT' "$write_clean" 2>/dev/null | tail -n 1 | sed 's/.*i=//' || true)"
    sync_status="$(grep '^SYNC_STATUS ' "$write_clean" 2>/dev/null | tail -n 1 | awk '{print $2}' || true)"
    mount_ok="$(grep -c '^GUEST_MOUNT_OK' "$write_clean" 2>/dev/null || true)"
    verify_mount_fail="$(grep -c '^PL_MOUNT_FAIL' "$verify_clean" 2>/dev/null || true)"

    rs_gen="$(field_from_summary "$verify_clean" RS_SUMMARY gen)"
    rs_root="$(field_from_summary "$verify_clean" RS_SUMMARY root)"
    rs_ok="$(field_from_summary "$verify_clean" RS_SUMMARY ok)"
    go_gen="$(field_from_summary "$verify_clean" GO_SUMMARY gen)"
    go_root="$(field_from_summary "$verify_clean" GO_SUMMARY root)"
    go_ok="$(field_from_summary "$verify_clean" GO_SUMMARY ok)"
    rs_sha="$(grep '^RS_CAT_SHA ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}' || true)"
    go_sha="$(grep '^GO_CAT_SHA ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}' || true)"
    rs_size="$(grep '^RS_CAT_SIZE ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}' || true)"
    go_size="$(grep '^GO_CAT_SIZE ' "$verify_clean" 2>/dev/null | tail -n 1 | awk '{print $2}' || true)"
    rs_rejected="$(grep '^RS_REJECTED' "$verify_clean" 2>/dev/null | tail -n 1 | cut -d' ' -f2- || true)"
    go_rejected="$(grep '^GO_REJECTED' "$verify_clean" 2>/dev/null | tail -n 1 | cut -d' ' -f2- || true)"

    read -r rs_problems rs_unpublished rs_torn rs_bad <<<"$(classify_problems "$verify_clean" RS "$rs_gen")"
    read -r go_problems go_unpublished go_torn go_bad <<<"$(classify_problems "$verify_clean" GO "$go_gen")"
    rs_gen="${rs_gen:-0}"
    go_gen="${go_gen:-0}"
    rs_ok="${rs_ok:-0}"
    go_ok="${go_ok:-0}"

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

    expected_sha=""
    size_ok=0
    sha_ok=0
    if (( rs_gen >= 2 )); then
      if build_expected "$rs_gen" "$WORK/expected-$iteration.bin"; then
        expected_sha="$(sha256sum "$WORK/expected-$iteration.bin" | cut -d' ' -f1)"
        if [[ "$rs_size" == "$FILE_SIZE" && "$go_size" == "$FILE_SIZE" ]]; then size_ok=1; fi
        if [[ -n "$rs_sha" && "$rs_sha" == "$expected_sha" && \
              -n "$go_sha" && "$go_sha" == "$expected_sha" ]]; then sha_ok=1; fi
      fi
    fi

    cross_impl_ok=0
    if [[ "$rs_gen" == "$go_gen" && -n "$rs_root" && "$rs_root" == "$go_root" && \
          "$rs_accept" == "1" && "$go_accept" == "1" && \
          "$rs_sha" == "$go_sha" ]]; then cross_impl_ok=1; fi

    verify_done=0
    if grep -q '^VERIFY_DONE' "$verify_clean" 2>/dev/null; then verify_done=1; fi

    partial=0
    if (( verify_done == 0 || verify_mount_fail != 0 || rs_gen < 2 || rs_accept == 0 || go_accept == 0 || \
          size_ok == 0 || sha_ok == 0 || cross_impl_ok == 0 )); then
      partial=1
    fi

    acked_lost=0
    recovered_new=0
    recovered_old=0
    if (( last_gen > 0 && rs_gen < last_gen )); then
      acked_lost=1
    elif (( last_gen > 0 && rs_gen == last_gen )); then
      recovered_old=1
    elif (( last_gen > 0 && rs_gen > last_gen )); then
      recovered_new=1
    fi

    outcome="partial"
    error=""
    if (( partial == 1 )); then
      error="verification or exact-state check failed (rust_gen=${rs_gen} go_gen=${go_gen} rust_ok=${rs_ok} go_ok=${go_ok} rust_bad=${rs_bad} go_bad=${go_bad} mount_fail=${verify_mount_fail} verify_done=${verify_done})"
    elif (( last_gen == 0 )); then
      outcome="no_commit"
    elif (( acked_lost == 1 )); then
      outcome="lost"
    elif (( recovered_new == 1 )); then
      outcome="new"
    else
      outcome="old"
    fi

    say "iteration $iteration ($mode/$k): outcome=$outcome recovered=$rs_gen go=$go_gen last_returned=$last_gen commits=$commits_observed commit_failures=$commit_failures fault_observed=$([[ "$commit_failures" -gt 0 ]] && echo 1 || echo 0) delay=${delay_ms}ms effective_delay=${effective_delay_ms}ms guest_alive_at_kill=$guest_alive"

    jq -n \
      --arg mode "$mode" \
      --argjson iteration "$iteration" \
      --argjson within_mode "$k" \
      --argjson kill_delay_ms "$delay_ms" \
      --argjson effective_kill_delay_ms "$effective_delay_ms" \
      --argjson commits_observed "$commits_observed" \
      --argjson commit_failures "$commit_failures" \
      --argjson fault_fired "$([[ "$commit_failures" -gt 0 ]] && echo 1 || echo 0)" \
      --argjson last_returned_generation "$last_gen" \
      --arg write_loop_exit "$write_loop_exit" \
      --arg sync_status "$sync_status" \
      --argjson guest_alive_at_kill "$guest_alive" \
      --argjson write_mount_ok "$mount_ok" \
      --argjson verify_mount_fail "$verify_mount_fail" \
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
      --arg rust_cat_sha256 "$rs_sha" \
      --arg go_cat_sha256 "$go_sha" \
      --arg expected_sha256 "$expected_sha" \
      --argjson size_ok "$size_ok" \
      --argjson sha_ok "$sha_ok" \
      --argjson cross_impl_ok "$cross_impl_ok" \
      --argjson acked_commit_lost "$acked_lost" \
      --argjson partial "$partial" \
      --arg outcome "$outcome" \
      --arg error "$error" \
      '{mode:$mode,iteration:$iteration,within_mode:$within_mode,
        kill_delay_ms:$kill_delay_ms,effective_kill_delay_ms:$effective_kill_delay_ms,
        commits_observed:$commits_observed,
        commit_failures:$commit_failures,fault_fired:$fault_fired,
        last_returned_generation:$last_returned_generation,
        write_loop_exit:$write_loop_exit,sync_status:$sync_status,
        guest_alive_at_kill:$guest_alive_at_kill,
        write_mount_ok:$write_mount_ok,verify_mount_fail:$verify_mount_fail,
        rust_generation:$rust_generation,go_generation:$go_generation,
        snapshot_root:{rust:$rust_root,go:$go_root},
        rejected_generations:{rust:$rust_rejected,go:$go_rejected},
        rust_verify_ok:$rust_verify_ok,go_verify_ok:$go_verify_ok,
        rust_verify_accepted:$rust_verify_accepted,go_verify_accepted:$go_verify_accepted,
        problems:{rust:{total:$rust_problems,unpublished_tail:$rust_unpublished_tail,
                         torn_unpublished:$rust_torn_unpublished},
                  go:{total:$go_problems,unpublished_tail:$go_unpublished_tail,
                      torn_unpublished:$go_torn_unpublished}},
        rust_cat_sha256:$rust_cat_sha256,go_cat_sha256:$go_cat_sha256,
        expected_sha256:$expected_sha256,size_ok:$size_ok,sha_ok:$sha_ok,
        cross_impl_ok:$cross_impl_ok,acked_commit_lost:$acked_commit_lost,
        partial:$partial,outcome:$outcome,error:$error}' >>"$ITER_JSONL"
  done
  if (( BLOCKED == 1 )); then break; fi
done

# ---- evidence log and receipt --------------------------------------------
{
  echo "===== prepare ====="
  cat "$prep_clean" 2>/dev/null || true
  echo "===== base verify ====="
  cat "$WORK/base-verify.clean" 2>/dev/null || true
  for mode in ${MODES//,/ }; do
    for ((k = 1; k <= ITERS; k++)); do
      f="$WORK/$mode-$k-write.clean"
      [[ -e "$f" ]] && { echo "===== $mode/$k write ====="; cat "$f"; }
      f="$WORK/$mode-$k-verify.clean"
      [[ -e "$f" ]] && { echo "===== $mode/$k verify ====="; cat "$f"; }
    done
  done
} >>"$LOG"

PER_MODE_JSON="$(jq -s '
  group_by(.mode) | map({
    mode: .[0].mode,
    iterations: length,
    old: (map(select(.outcome == "old")) | length),
    new: (map(select(.outcome == "new")) | length),
    lost: (map(select(.outcome == "lost")) | length),
    no_commit: (map(select(.outcome == "no_commit")) | length),
    partial: (map(select(.outcome == "partial")) | length),
    blocked: (map(select(.outcome == "boot_fail" and (.mode | startswith("blkdebug-")))) | length),
    boot_fail: (map(select(.outcome == "boot_fail" and ((.mode | startswith("blkdebug-")) | not))) | length),
    acked_commits_lost: (map(select(.acked_commit_lost == 1)) | length),
    fault_fired_iterations: (map(select(.fault_fired == 1)) | length),
    commit_failures_total: (map(.commit_failures) | add),
    verify_mount_failures: (map(select(.verify_mount_fail > 0)) | length),
    cross_impl_disagreements: (map(select(.cross_impl_ok == 0 and .partial == 0)) | length)
  })' "$ITER_JSONL")"

SUMMARY_JSON="$(jq -s '{
  iterations: length,
  old: (map(select(.outcome == "old")) | length),
  new: (map(select(.outcome == "new")) | length),
  lost: (map(select(.outcome == "lost")) | length),
  no_commit: (map(select(.outcome == "no_commit")) | length),
  partial: (map(select(.outcome == "partial")) | length),
  blocked: (map(select(.outcome == "boot_fail" and (.mode | startswith("blkdebug-")))) | length),
  boot_fail: (map(select(.outcome == "boot_fail" and ((.mode | startswith("blkdebug-")) | not))) | length),
  acked_commits_lost: (map(select(.acked_commit_lost == 1)) | length),
  fault_fired_iterations: (map(select(.fault_fired == 1)) | length),
  verify_failures: (map(select(.partial == 1)) | length)
}' "$ITER_JSONL")"

FINISHED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
DURATION_MS=$(( ($(date +%s) - START_EPOCH) * 1000 ))
LOG_SHA="$(sha256sum "$LOG" | cut -d' ' -f1)"
REV_WEFTFS="${WEFT_REPO_WEFTFS:-unknown}"
REV_GO="${WEFT_REPO_WEFTFS_GO:-unknown}"

MODES_JSON="$(printf '%s\n' "${MODES//,/ }" | jq -R . | jq -s .)"

jq -n \
  --arg schema "weftfs.blkdebug.receipt/1" \
  --arg generated_at "$FINISHED_AT" \
  --arg started_at "$STARTED_AT" \
  --arg finished_at "$FINISHED_AT" \
  --argjson duration_ms "$DURATION_MS" \
  --arg qemu_version "$qemu_version" \
  --arg accel "$ACCEL" \
  --argjson accel_fallback "$ACCEL_FALLBACK" \
  --arg kernel "$KERNEL_REL" \
  --arg initrd "$(basename "$INITRD")" \
  --arg court "${WEFT_COURT_IMAGE:-unknown}" \
  --arg image_id "${WEFT_COURT_IMAGE_ID:-unknown}" \
  --arg host_kernel "$(uname -a)" \
  --arg limit_memory "${WEFT_COURT_MEMORY:-?}" \
  --arg limit_cpus "${WEFT_COURT_CPUS:-?}" \
  --arg limit_pids "${WEFT_COURT_PIDS:-?}" \
  --argjson iterations_per_mode "$ITERS" \
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
  --argjson modes "$MODES_JSON" \
  --argjson per_mode "$PER_MODE_JSON" \
  --argjson summary "$SUMMARY_JSON" \
  --slurpfile iterations "$ITER_JSONL" \
  '{
    schema: $schema,
    generated_at: $generated_at,
    run: {started_at: $started_at, finished_at: $finished_at, duration_ms: $duration_ms},
    rig: {
      type: "qemu-block-layer-fault-injection",
      claim: "block-layer lying-disk model combined with SIGKILL and reboot; dm-flakey injects silent write drop or EIO below the guest filesystem after a bounded up interval, write-drop-all discards a volatile snapshot overlay, and blkdebug write/flush-error attempts are recorded separately. This is not physical media power loss and does not drop the host page cache",
      physical_media: false,
      host_page_cache_loss: false,
      available: ($blocked == 0),
      blockers: (if $blocked == 0 then [] else ["guest did not boot twice consecutively"] end),
      qemu_version: $qemu_version,
      accel: $accel,
      accel_fallback_to_tcg: ($accel_fallback == 1),
      kernel: $kernel,
      initramfs: $initrd,
      root_disk: "raw ext4 160 MiB, mounted read-only",
      data_disk: "raw ext4 128 MiB; one clean prepare boot, one byte copy per iteration",
      guest_filesystem: "ext4 data=ordered barrier=1; no fsck",
      kill: "SIGKILL to qemu-system-x86_64 with no guest shutdown and no cache flush",
      blkdebug_limits: "QEMU 7.2 accepts read_aio, write_aio, and flush_to_os injection; flush_do_sync is rejected and errno=0 still writes (tested with qemu-io), so no silent selective write-drop primitive exists in blkdebug. blkdebug arms at device open, so its first injected write lands in the ext4 mount superblock and the guest cannot reach GUEST_WRITING; those iterations are recorded as blocked, not as data results. dm-flakey is the arm-after-mount equivalent used for the lying-disk data matrix.",
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
      modes: $modes, iterations_per_mode: $iterations_per_mode, seed: $seed,
      guest_memory_mb: $guest_mem_mb, file_size_bytes: $file_size,
      payload_size_bytes: $payload_size,
      kill_delay_ms: {min: $kill_min_ms, max: $kill_max_ms},
      dm_flakey_up_seconds: 2,
      dm_flakey_down_seconds: 3600,
      dm_flakey_effective_extra_ms: 2300,
      boot_timeout_seconds: $boot_timeout, verify_timeout_seconds: $verify_timeout,
      no_fsck: true, sync_after_commit_in_guest: false
    },
    modes: {
      control: "no fault injection, cache=none",
      "dm-flakey-drop": "guest dm-flakey over /dev/vdb, up 2s then down 3600s with drop_writes: writes are acknowledged and silently discarded",
      "dm-flakey-error": "guest dm-flakey over /dev/vdb, up 2s then down 3600s with error_writes: write bios fail with EIO",
      "write-drop-all": "QEMU snapshot=on: all writes acknowledged into a volatile overlay, discarded by SIGKILL; total write drop, not selective",
      "blkdebug-write-error": "QEMU blkdebug inject-error write_aio errno=5 once=off, armed at device open",
      "blkdebug-flush-error": "QEMU blkdebug inject-error flush_to_os errno=5 once=off, armed at device open"
    },
    per_mode: $per_mode,
    iterations: $iterations,
    summary: $summary,
    evidence: {raw_log: $log, raw_log_sha256: $log_sha},
    commands: {
      court: "scripts/docker-court.sh vm \"bash scripts/blkdebug_court.sh\"",
      harness: "bash scripts/blkdebug_court.sh"
    },
    scope: {
      asserted: "for every completed iteration, both implementations recover the same whole generation and read exactly the expected bytes of that generation, or the iteration is classified partial; old, new, lost, no_commit, and partial are reported per mode, including acknowledged commits lost under write-drop-all",
      not_asserted: [
        "physical power loss (media/device write-cache loss)",
        "a real disk firmware that silently discards individual writes while reporting success",
        "host page-cache loss",
        "torn-sector or reordered-write models",
        "power loss during mkfs, store init, or the read-only root mount",
        "flush-specific error injection below a mounted ext4 (blkdebug arms before mount; dm-flakey models write drop/EIO, not flush errors)"
      ]
    }
  }' >"$RECEIPT"

say "receipt: $RECEIPT"
say "raw log: $LOG"
sha256sum "$RECEIPT" "$LOG"

fail_count="$(jq -r '.summary.partial + .summary.boot_fail' "$RECEIPT")"
if (( BLOCKED == 1 || fail_count > 0 )); then
  say "BLK_FAIL partial or boot-failed iterations"
  exit 1
fi
say "BLK_PASS iterations=$(jq -r '.summary.iterations' "$RECEIPT") lost=$(jq -r '.summary.lost' "$RECEIPT") blocked=$(jq -r '.summary.blocked' "$RECEIPT")"
