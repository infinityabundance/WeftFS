#!/bin/sh
# Guest PID 1 for the WeftFS power-loss court.
#
# Booted by QEMU with a Debian kernel/initramfs and a read-only ext4 root; the
# data disk is /dev/vdb (qcow2 overlay). The host either SIGKILLs QEMU during
# the write loop or lets the guest power off after a verification pass.
#
# Modes (kernel cmdline):
#   weft.mode=write    initialise the store and commit in a loop
#   weft.mode=verify   open the crashed store read-only with Rust and Go
#
# The write loop prints COMMIT i=<n> gen=<g> only after the CLI returns, i.e.
# after the store's final fsync. It deliberately never calls sync(1): a sync
# would hide missing fsyncs inside the store.

export PATH=/bin:/sbin:/usr/bin
umask 022

mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mkdir -p /tmp /data
mount -t tmpfs -o size=64m,nosuid,nodev tmpfs /tmp 2>/dev/null

mode=verify
iters=100000
for arg in $(cat /proc/cmdline); do
  case "$arg" in
    weft.mode=*) mode="${arg#weft.mode=}" ;;
    weft.iters=*) iters="${arg#weft.iters=}" ;;
  esac
done

echo "GUEST_BOOT kernel=$(uname -r) mode=$mode"
echo "GUEST_CMDLINE $(cat /proc/cmdline)"

# The data disk is virtio-blk. devtmpfs creates the node once the driver probes.
try=0
while [ ! -b /dev/vdb ] && [ "$try" -lt 200 ]; do
  sleep 0.1
  try=$((try + 1))
done
if [ ! -b /dev/vdb ]; then
  echo "PL_BLOCKER data_disk_missing"
  poweroff -f
fi

# No fsck: the kernel may replay the ext4 journal, which is what a real boot
# after power loss does. A mount failure is a court failure, not a pass.
if ! mount -t ext4 -o rw,data=ordered,barrier=1 /dev/vdb /data; then
  echo "PL_MOUNT_FAIL"
  dmesg | tail -n 40
  poweroff -f
fi
echo "GUEST_MOUNT_OK"

STORE=/data/store
RUST=/usr/bin/weftfs
GO=/usr/bin/go-weftfs

json_num() {
  # $1 = json text, $2 = lowercase or capitalised key, $3 = op (gen|ok)
  if [ "$3" = gen ]; then
    printf '%s' "$1" | sed -n "s/.*\"$2\": *\([0-9][0-9]*\).*/\1/p" | head -n 1
  else
    printf '%s' "$1" | grep -c "\"$2\": true"
  fi
}

if [ "$mode" = "write" ]; then
  if [ ! -f "$STORE/FORMAT" ]; then
    $RUST --store "$STORE" init --chunk-size 65536 --checkpoint-interval 5 --gc-grace-secs 0 \
      || { echo "PL_INIT_FAIL"; poweroff -f; }
    dd if=/dev/zero of=/tmp/base bs=4096 count=256 2>/dev/null
    $RUST --store "$STORE" create /f --file /tmp/base --seed powerloss-base \
      || { echo "PL_CREATE_FAIL"; poweroff -f; }
  fi

  echo "GUEST_WRITING pid=$$"
  # Control for the rig itself: an unsynced append on the data filesystem
  # (outside the store). If SIGKILL really discards the guest page cache, some
  # of these bytes must be missing after reboot. It is never fsynced or synced.
  rm -f /data/unsynced-control 2>/dev/null || true
  i=1
  while [ "$i" -le "$iters" ]; do
    prefix="POWERLOSS:${i}:"
    pad=$((65536 - ${#prefix}))
    { printf '%s' "$prefix"; head -c "$pad" /dev/zero; } > /tmp/payload
    out=$($RUST --store "$STORE" --json write /f --offset 0 --file /tmp/payload 2>/tmp/err)
    status=$?
    if [ "$status" -ne 0 ]; then
      echo "COMMIT_FAIL i=$i status=$status err=$(cat /tmp/err)"
      break
    fi
    gen=$(json_num "$out" generation gen)
    echo "COMMIT i=$i gen=$gen"
    printf 'X' >> /data/unsynced-control 2>/dev/null || true
    i=$((i + 1))
  done
  echo "WRITE_LOOP_EXIT i=$i"
  sync
  poweroff -f
fi

# ---- verification pass ----------------------------------------------------
echo "VERIFY_BEGIN"
rinfo=$($RUST --store "$STORE" --json info 2>&1)
echo "RS_INFO $rinfo"
rverify=$($RUST --store "$STORE" --json verify --full 2>&1)
rvstatus=$?
echo "RS_VERIFY $rverify"
ginfo=$($GO --store "$STORE" --json info 2>&1)
echo "GO_INFO $ginfo"
gverify=$($GO --store "$STORE" --json verify --full 2>&1)
gvstatus=$?
echo "GO_VERIFY $gverify"

rgen=$(json_num "$rinfo" generation gen)
rroot=$(printf '%s' "$rinfo" | sed -n 's/.*"snapshot_root": *"\([0-9a-f]*\)".*/\1/p' | head -n 1)
rok=$(json_num "$rverify" ok ok)
ggen=$(json_num "$ginfo" generation gen)
groot=$(printf '%s' "$ginfo" | sed -n 's/.*"snapshot_root": *"\([0-9a-f]*\)".*/\1/p' | head -n 1)
gok=$(json_num "$gverify" OK ok)

echo "RS_SUMMARY gen=$rgen root=$rroot ok=$rok status=$rvstatus"
echo "RS_PROBLEMS total=$(printf '%s\n' "$rverify" | grep -cE '"(class|Class)"')"
printf '%s\n' "$rverify" | grep -E '"(detail|Detail)":' | while IFS= read -r line; do
  echo "RS_PROBLEM_DETAIL $line"
done
echo "RS_REJECTED $(printf '%s' "$rinfo" | sed -n 's/.*"rejected_generations": *\[\([^]]*\)\].*/\1/p' | head -n 1)"
echo "GO_SUMMARY gen=$ggen root=$groot ok=$gok status=$gvstatus"
echo "GO_PROBLEMS total=$(printf '%s\n' "$gverify" | grep -cE '"(class|Class)"')"
printf '%s\n' "$gverify" | grep -E '"(detail|Detail)":' | while IFS= read -r line; do
  echo "GO_PROBLEM_DETAIL $line"
done
echo "GO_REJECTED $(printf '%s' "$ginfo" | sed -n 's/.*"rejected_generations": *\[\([^]]*\)\].*/\1/p' | head -n 1)"

$RUST --store "$STORE" cat /f > /tmp/rs.bin 2>/tmp/rs.err
echo "RS_CAT_STATUS $?"
echo "RS_CAT_SIZE $(wc -c < /tmp/rs.bin)"
echo "RS_CAT_SHA $(sha256sum /tmp/rs.bin | cut -d' ' -f1)"
$GO --store "$STORE" cat /f > /tmp/go.bin 2>/tmp/go.err
echo "GO_CAT_STATUS $?"
echo "GO_CAT_SIZE $(wc -c < /tmp/go.bin)"
echo "GO_CAT_SHA $(sha256sum /tmp/go.bin | cut -d' ' -f1)"

if [ -e /data/unsynced-control ]; then
  echo "CONTROL_UNSYNCED_SIZE $(wc -c < /data/unsynced-control)"
else
  echo "CONTROL_UNSYNCED_SIZE missing"
fi

echo "GUEST_DMESG_EXT4_BEGIN"
dmesg | grep -iE 'ext4|journal' | tail -n 30
echo "GUEST_DMESG_EXT4_END"
echo "VERIFY_DONE"
poweroff -f
