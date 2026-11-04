#!/bin/busybox sh
# Guest init for the cold-cache fio court.
#
# The guest runs on a read-only ext4 root image built by vm_fs_court.sh. The
# target filesystem/data disk is /dev/vdb; fio JSON is written to /dev/vdc
# (an ext4 results disk) and extracted on the host with debugfs (no loop mount,
# no extra capability).
#
# Kernel cmdline: weft.target=ext4|xfs|btrfs|zfs|zfs-whole|rust|go
# This script never runs fsck and never calls sync(1) on the host.
set -x

/bin/busybox --install -s /bin 2>/dev/null || true
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sys /sys 2>/dev/null || true
mount -t devtmpfs dev /dev 2>/dev/null || true
mkdir -p /results /data /data/mnt /data/fio /tmp /run /etc/zfs
mount -t tmpfs tmpfs /tmp 2>/dev/null || true
mount -t tmpfs tmpfs /run 2>/dev/null || true
mount -t tmpfs tmpfs /etc/zfs 2>/dev/null || true
ln -sf /proc/mounts /etc/mtab 2>/dev/null || true

TARGET="$(sed -n 's/.*weft.target=\([^ ]*\).*/\1/p' /proc/cmdline)"
REPS="$(sed -n 's/.*weft.reps=\([^ ]*\).*/\1/p' /proc/cmdline)"
[ -n "$REPS" ] || REPS=2
KREL="$(uname -r)"
MOD=/lib/modules/$KREL

# The results disk must be mounted before any status file is created: the
# guest root is read-only.
if ! mount -t ext4 -o rw /dev/vdc /results; then
  echo "blocker=results_mount_failed"
  poweroff -f
fi
STATUS=/results/status-$TARGET.txt
COMMANDS=/results/commands-$TARGET.txt
: > "$STATUS"
: > "$COMMANDS"

say() { echo "$*" | tee -a "$STATUS"; }
cmd() { echo "$*" >> "$COMMANDS"; }

say "target=$TARGET"
say "kernel=$(uname -a)"
say "nproc=$(nproc)"
say "memtotal=$(grep MemTotal /proc/meminfo)"
say "cmdline=$(cat /proc/cmdline)"

loadmods() {
  insmod "$MOD/kernel/crypto/crc32c_generic.ko" 2>/dev/null || true
  insmod "$MOD/kernel/arch/x86/crypto/crc32c-intel.ko" 2>/dev/null || true
  insmod "$MOD/kernel/lib/libcrc32c.ko" 2>/dev/null || true
}

run_fio() {
  dir="$1"; workload="$2"; nj="$3"; mode="$4"; out="$5"
  fsync_opt=""
  end_fsync_opt=""
  case "$workload" in
    seq-write-1m)  rw=write;     bs=1M; size=8M;   pattern='seqw_$jobnum.dat'; end_fsync_opt="--end_fsync=1" ;;
    seq-read-1m)   rw=read;      bs=1M; size=8M;   pattern='seqr_$jobnum.dat' ;;
    rand-read-4k)  rw=randread;  bs=4k; size=512k; pattern='randr_$jobnum.dat' ;;
    rand-write-4k) rw=randwrite; bs=4k; size=512k; pattern='randw_$jobnum.dat'; end_fsync_opt="--end_fsync=1" ;;
    fsync-write-4k) rw=write;    bs=4k; size=64k;  pattern='fsyncw_$jobnum.dat'; fsync_opt="--fsync=1" ;;
    *) say "blocker=unknown_workload_$workload"; return 1 ;;
  esac
  if [ "$mode" = cold ]; then
    sync
    echo 3 > /proc/sys/vm/drop_caches
  fi
  cmd "fio --name=$workload --directory=$dir --rw=$rw --bs=$bs --size=$size --numjobs=$nj --ioengine=psync --direct=0 --group_reporting --output-format=json --output=$out --eta=never --percentile_list=50:95:99 --filename_format=$pattern $fsync_opt $end_fsync_opt"
  fio --name="$workload" --directory="$dir" --rw="$rw" --bs="$bs" --size="$size" \
      --numjobs="$nj" --ioengine=psync --direct=0 --group_reporting \
      --output-format=json --output="$out" --eta=never --percentile_list=50:95:99 \
      --filename_format="$pattern" $fsync_opt $end_fsync_opt >>/results/fio-$TARGET.log 2>&1
}

precreate() {
  dir="$1"; nj="$2"
  j=1
  while [ "$j" -le "$nj" ]; do
    dd if=/dev/zero of="$dir/seqr_$j.dat" bs=1M count=8 2>/dev/null
    dd if=/dev/zero of="$dir/randr_$j.dat" bs=512k count=1 2>/dev/null
    j=$((j + 1))
  done
}

run_matrix() {
  dir="$1"
  precreate "$dir" 16
  rep=1
  while [ "$rep" -le "$REPS" ]; do
    for wl in seq-write-1m seq-read-1m rand-read-4k rand-write-4k fsync-write-4k; do
      for nj in 1 4 16; do
        run_fio "$dir" "$wl" "$nj" cold "/results/$TARGET--$wl--nj$nj--cold-r$rep.json"
        run_fio "$dir" "$wl" "$nj" warm "/results/$TARGET--$wl--nj$nj--warm-r$rep.json"
      done
    done
    rep=$((rep + 1))
  done
}

case "$TARGET" in
  ext4|xfs|btrfs)
    loadmods
    if [ "$TARGET" = xfs ]; then
      insmod "$MOD/kernel/fs/xfs/xfs.ko" 2>>"$STATUS" || say "blocker=xfs_module_failed"
    fi
    if [ "$TARGET" = btrfs ]; then
      insmod "$MOD/kernel/fs/btrfs/btrfs.ko" 2>>"$STATUS" || true
      if awk '$1 == "btrfs" {found=1} END {exit !found}' /proc/modules \
         || grep -qw btrfs /proc/filesystems; then
        say "btrfs_module=loaded"
      else
        say "blocker=btrfs_module_failed"
      fi
    fi
    if mount -t "$TARGET" -o rw /dev/vdb /data; then
      say "mount=ok"
      mkdir -p /data/fio
      grep " /data " /proc/mounts >> "$STATUS"
      run_matrix /data/fio
    else
      say "blocker=mount_${TARGET}_failed"
    fi
    ;;
  zfs-whole)
    # Whole-disk vdev: /dev/vdb is passed straight to zpool, with no partition
    # table, loop device, or udev-created node from the host. zpool may create
    # its own GPT labels; if the kernel cannot create the partition nodes in
    # this minimal guest, the exact zpool error is written to the status file
    # and no file-backed vdev is substituted for this target.
    loadmods
    modprobe zfs 2>>"$STATUS" || insmod "$MOD/updates/dkms/zfs.ko" 2>>"$STATUS"
    if awk '$1 == "zfs" {found=1} END {exit !found}' /proc/modules; then
      say "zfs_module=loaded"
    else
      say "blocker=zfs_module_failed"
    fi
    minor="$(awk '$2 == "zfs" {print $1}' /proc/misc 2>/dev/null | head -n 1)"
    if [ -n "$minor" ] && [ ! -c /dev/zfs ]; then
      mknod /dev/zfs c 10 "$minor" 2>>"$STATUS" || true
    fi
    if [ -c /dev/zfs ]; then say "zfs_device=ok"; else say "blocker=zfs_device_missing"; fi
    say "zpool_create_target=/dev/vdb (whole disk)"
    if [ -x /usr/lib/systemd/systemd-udevd ]; then
      mkdir -p /run/udev
      /usr/lib/systemd/systemd-udevd --daemon 2>>"$STATUS" || true
      udevadm trigger --action=add --subsystem-match=block >>"$STATUS" 2>&1 || true
      udevadm settle --timeout=3 >>"$STATUS" 2>&1 || true
      say "udev=started"
    else
      say "udev=absent"
    fi
    # libzfs on Debian waits for its own GPT partition through libudev. The
    # guest starts the stock Debian udevd so that wait can complete without a
    # host udev or a file-backed vdev. The explicit partx/blockdev rescan below
    # is a fallback for a no-udev guest; it does not substitute a partition or
    # file for the vdev argument.
    (
      n=0
      first_err=1
      while [ "$n" -lt 200 ]; do
        [ -b /dev/vdb1 ] && [ -b /dev/vdb9 ] && break
        err="$(partx -a /dev/vdb 2>&1)"
        if [ -n "$err" ] && [ "$first_err" -eq 1 ]; then
          say "partx_first_error=$err"
          first_err=0
        fi
        blockdev --rereadpt /dev/vdb >/dev/null 2>&1 || true
        sleep 0.05
        n=$((n + 1))
      done
    ) &
    rescan_pid=$!
    strace -f -tt -o /results/zpool-whole.strace -e trace=file,ioctl \
      zpool create -f -m /data/tank tank /dev/vdb >>"$STATUS" 2>&1
    zpool_rc=$?
    if [ "$zpool_rc" -ne 0 ]; then
      # The first zpool attempt can race the kernel registering the new GPT
      # partitions. Wait for the background rescan (bounded) and retry once
      # on the same whole block device; no partition or file is substituted.
      wait "$rescan_pid" 2>/dev/null || true
      if [ -b /dev/vdb1 ] && [ -b /dev/vdb9 ]; then
        say "zpool_create_retry_after_partition_nodes"
        sleep 0.5
        zpool create -f -m /data/tank tank /dev/vdb >>"$STATUS" 2>&1
        zpool_rc=$?
      fi
    fi
    kill "$rescan_pid" 2>/dev/null || true
    wait "$rescan_pid" 2>/dev/null || true
    if [ "$zpool_rc" -eq 0 ]; then
      say "zpool_create=ok"
      zfs create tank/fio >>"$STATUS" 2>&1 || say "blocker=zfs_dataset_failed"
      say "mount=zfs-whole-ok"
      zpool status -P tank >> "$STATUS" 2>&1
      # The vdev argument was the raw whole disk /dev/vdb. ZFS creates and owns
      # its own GPT partition (shown as /dev/vdb1); no partition, loop device,
      # or file was supplied to zpool.
      say "zpool_create_vdev=/dev/vdb"
      say "zpool_status_vdev=$(zpool status -P tank | awk '/\/dev\/vdb/ {print $1; exit}')"
      zfs list >> "$STATUS" 2>&1
      run_matrix /data/tank/fio
      zpool export tank >>"$STATUS" 2>&1 || say "blocker=zpool_export_failed"
      say "zpool_export=done"
    else
      say "blocker=zpool_whole_disk_create_failed"
      say "dev_nodes=$(ls -1 /dev/vd* 2>&1 | tr '\n' ' ')"
      say "proc_partitions=$(tr '\n' ';' < /proc/partitions)"
    fi
    ;;
  zfs)
    loadmods
    modprobe zfs 2>>"$STATUS" || insmod "$MOD/updates/dkms/zfs.ko" 2>>"$STATUS"
    if awk '$1 == "zfs" {found=1} END {exit !found}' /proc/modules; then
      say "zfs_module=loaded"
    else
      say "blocker=zfs_module_failed"
    fi
    minor="$(awk '$2 == "zfs" {print $1}' /proc/misc 2>/dev/null | head -n 1)"
    if [ -n "$minor" ] && [ ! -c /dev/zfs ]; then
      mknod /dev/zfs c 10 "$minor" 2>>"$STATUS" || true
    fi
    if [ -c /dev/zfs ]; then say "zfs_device=ok"; else say "blocker=zfs_device_missing"; fi
    # A whole-disk vdev makes zpool create wait for a /dev/vdb1 partition node
    # that only udev would create; the minimal guest runs no udev. A file-backed
    # vdev is a real ZFS pool vdev and avoids inventing a partition node. The
    # ext4 data disk is the backing layer; this is recorded as a caveat.
    if mount -t ext4 -o rw /dev/vdb /data; then
      say "mount=ext4-backing-ok"
      truncate -s 6G /data/zfs-vdev.img
      if zpool create -f -m /data/tank -O compression=off tank /data/zfs-vdev.img >>"$STATUS" 2>&1; then
        zfs create tank/fio >>"$STATUS" 2>&1 || say "blocker=zfs_dataset_failed"
        say "mount=zfs-ok"
        zpool status >> "$STATUS" 2>&1
        zfs list >> "$STATUS" 2>&1
        run_matrix /data/tank/fio
        zpool export tank >>"$STATUS" 2>&1 || true
      else
        say "blocker=zpool_create_failed"
      fi
    else
      say "blocker=mount_ext4_backing_failed"
    fi
    ;;
  rust|go)
    loadmods
    mknod /dev/fuse c 10 229 2>/dev/null || true
    insmod "$MOD/kernel/fs/fuse/fuse.ko" 2>>"$STATUS" || say "blocker=fuse_module_failed"
    if mount -t ext4 -o rw /dev/vdb /data; then
      say "mount=ok"
      mkdir -p /data/store /data/mnt
      if [ "$TARGET" = rust ]; then CLI=/usr/bin/weftfs; FG="--foreground"; else CLI=/usr/bin/go-weftfs; FG=""; fi
      cmd "$CLI --store /data/store --fixed-timestamp-ns 1767225600000000000 init"
      "$CLI" --store /data/store --fixed-timestamp-ns 1767225600000000000 init >>"$STATUS" 2>&1
      cmd "$CLI --store /data/store --fixed-timestamp-ns 1767225600000000000 mount --mountpoint /data/mnt $FG"
      "$CLI" --store /data/store --fixed-timestamp-ns 1767225600000000000 mount --mountpoint /data/mnt $FG >/results/mount-$TARGET.log 2>&1 &
      pid=$!
      tries=0
      while [ "$tries" -lt 600 ]; do
        if grep -q " /data/mnt " /proc/mounts; then break; fi
        if ! kill -0 "$pid" 2>/dev/null; then break; fi
        sleep 0.1
        tries=$((tries + 1))
      done
      if grep -q " /data/mnt " /proc/mounts; then
        say "fuse_mount=ok"
        grep " /data/mnt " /proc/mounts >> "$STATUS"
        run_matrix /data/mnt
        fusermount3 -u /data/mnt 2>/dev/null || umount -l /data/mnt 2>/dev/null || true
      else
        say "blocker=fuse_mount_failed"
        cat /results/mount-$TARGET.log >> "$STATUS" 2>/dev/null || true
      fi
      kill "$pid" 2>/dev/null || true
    else
      say "blocker=mount_ext4_failed"
    fi
    ;;
  *)
    say "blocker=unknown_target_$TARGET"
    ;;
esac

say "done"
sync
umount /results 2>/dev/null || true
poweroff -f
