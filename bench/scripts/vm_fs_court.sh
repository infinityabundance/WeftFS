#!/usr/bin/env bash
# Cold-cache and concurrent fio court for ext4, XFS, Btrfs, ZFS (whole-disk
# vdev), and the two independent WeftFS FUSE implementations.
#
# Host usage (self-dispatches into the hard-limited VM court):
#   scripts/docker-court.sh vm "bash bench/scripts/vm_fs_court.sh"
#
# Rig:
#   - QEMU boots the Debian kernel/initramfs from the vm court image
#   - read-only ext4 root image containing busybox, fio, both WeftFS CLIs, and
#     the exact kernel modules used by the guest (fuse, xfs, crc32c)
#   - a fresh data disk per target; ext4/XFS formatted on the host with the
#     court's mkfs tools, or ext4 for a WeftFS store
#   - fio JSON is written to a small ext4 results disk and extracted with
#     debugfs (no loop mount and no extra capability)
#   - cold = guest `sync; echo 3 > /proc/sys/vm/drop_caches`; warm = the next
#     run on the same files. QEMU data disk uses cache=none.
#
# Outputs:
#   bench/receipts/bench-fs-cold-<UTC>.json
#   bench/receipts/bench-fs-cold-<UTC>.log
#   raw fio JSON under .docker-cache/vm-fs/<UTC>/extracted/
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipts="$root/bench/receipts"
mkdir -p "$receipts"
receipt="$receipts/bench-fs-cold-${stamp}.json"
log="$receipts/bench-fs-cold-${stamp}.log"
work="${WEFT_FS_COLD_WORK:-$root/.docker-cache/vm-fs/$stamp}"
bin="$work/bin"
rootfs="$work/rootfs"
root_img="$work/root.img"
results_img="$work/results.img"
data_img="$work/data.img"
targets="${WEFT_FS_COLD_TARGETS:-ext4,xfs,rust,go}"
reps="${WEFT_FS_COLD_REPS:-2}"
target_timeout="${WEFT_FS_COLD_TIMEOUT:-1800}"
mkdir -p "$bin" "$rootfs" "$work/extracted" "$work/serial"

exec > >(tee -a "$log") 2>&1

say() { echo "$*"; }
say "cold-cache fio court: image=${WEFT_COURT_IMAGE:-unknown} kernel=$(uname -r)"
say "targets=$targets reps=$reps timeout_s=$target_timeout"

# ---- build the two independent implementations ---------------------------
say "building Rust WeftFS (fuse)"
cargo build --release --features fuse --bin weftfs
cp "${CARGO_TARGET_DIR:-$root/target}/release/weftfs" "$bin/weftfs"
say "building Go WeftFS (CGO disabled)"
(cd WeftFS-Go && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -o "$bin/go-weftfs" ./cmd/weftfs)

# ---- guest kernel and modules --------------------------------------------
kernel="$(ls -1 /boot/vmlinuz-*-amd64 2>/dev/null | sort -V | tail -n 1)"
if [ -z "$kernel" ]; then
  echo "BLOCKER no guest kernel in the vm court image" >&2
  jq -n --arg schema "weft-fs-cold-bench-v1" --arg blocker "no guest kernel" \
    '{schema:$schema,blockers:[$blocker],results:[]}' >"$receipt"
  exit 2
fi
krel="${kernel#/boot/vmlinuz-}"
initrd="/boot/initrd.img-$krel"
[ -r "$initrd" ] || initrd="$(ls -1 /boot/initrd.img-* | sort -V | tail -n 1)"
modules="/lib/modules/$krel"
say "guest kernel: $kernel ($krel)"
say "guest initramfs: $initrd"
say "qemu: $(qemu-system-x86_64 --version | head -n 1)"
say "fio: $(fio --version)"
say "mkfs.xfs: $(mkfs.xfs -V 2>&1 || echo unavailable)"

# ---- root filesystem ------------------------------------------------------
mkdir -p "$rootfs"/{bin,sbin,usr/bin,usr/sbin,proc,sys,dev,tmp,etc,etc/zfs,lib,lib64,run,results,data,data/mnt,data/fio,data/tank}
cp /bin/busybox "$rootfs/bin/busybox"
chmod 755 "$rootfs/bin/busybox"
for applet in sh ash mount umount mkdir cat echo sed grep head tail dd wc cut \
              sha256sum sync poweroff reboot dmesg sleep mknod printf chmod \
              ln ls true false ps kill env insmod modprobe seq rm cp mv tee \
              nproc df du find xargs awk truncate blockdev; do
  ln -sf /bin/busybox "$rootfs/bin/$applet"
done
ln -sf /proc/mounts "$rootfs/etc/mtab"

copy_binary() {
  local src="$1" dst="$2"
  cp -L "$src" "$dst"
  chmod 755 "$dst"
  ldd "$src" 2>/dev/null | awk '$2 == "=>" && $3 ~ /^\// {print $3} $1 ~ /^\// {print $1}' | sort -u |
  while read -r lib; do
    [ -e "$lib" ] || continue
    mkdir -p "$rootfs$(dirname "$lib")"
    cp -L "$lib" "$rootfs$lib"
  done
}
copy_binary /usr/bin/fio "$rootfs/usr/bin/fio"
copy_binary /usr/bin/partx "$rootfs/usr/bin/partx"
copy_binary /usr/bin/strace "$rootfs/usr/bin/strace"
copy_binary "$bin/weftfs" "$rootfs/usr/bin/weftfs"
if [ -x /usr/sbin/zpool ] && [ -x /usr/sbin/zfs ]; then
  copy_binary /usr/sbin/zpool "$rootfs/usr/sbin/zpool"
  copy_binary /usr/sbin/zfs "$rootfs/usr/sbin/zfs"
fi
# libzfs on Debian is built with libudev and waits for its own GPT partition
# events through the udev database. The guest runs the stock Debian udevd so
# whole-disk vdev creation can complete without a host udev or a file-backed
# vdev. If udevd is unavailable the guest records the exact libudev error.
if [ -x /usr/lib/systemd/systemd-udevd ]; then
  mkdir -p "$rootfs/usr/lib/systemd" "$rootfs/usr/bin"
  copy_binary /usr/lib/systemd/systemd-udevd "$rootfs/usr/lib/systemd/systemd-udevd"
  copy_binary /usr/bin/udevadm "$rootfs/usr/bin/udevadm"
  mkdir -p "$rootfs/etc/udev" "$rootfs/usr/lib/udev"
  cp -a /etc/udev/. "$rootfs/etc/udev/" 2>/dev/null || true
  cp -a /usr/lib/udev/. "$rootfs/usr/lib/udev/" 2>/dev/null || true
fi
cp "$bin/go-weftfs" "$rootfs/usr/bin/go-weftfs"
chmod 755 "$rootfs/usr/bin/go-weftfs"
if [ -x /usr/bin/fusermount3 ]; then
  copy_binary /usr/bin/fusermount3 "$rootfs/usr/bin/fusermount3"
  ln -sf /usr/bin/fusermount3 "$rootfs/usr/bin/fusermount"
fi
[ -e "$rootfs/lib64/ld-linux-x86-64.so.2" ] || {
  mkdir -p "$rootfs/lib64"
  cp -L /lib64/ld-linux-x86-64.so.2 "$rootfs/lib64/ld-linux-x86-64.so.2"
}

# Only the modules this guest needs: fuse, xfs, zfs (OpenZFS via DKMS), and
# their dependencies. modules.dep is copied so the guest's busybox modprobe
# can resolve the zfs module stack without running depmod in the guest.
for rel in \
  kernel/fs/fuse/fuse.ko \
  kernel/fs/xfs/xfs.ko \
  kernel/fs/btrfs/btrfs.ko \
  kernel/lib/libcrc32c.ko \
  kernel/crypto/crc32c_generic.ko \
  kernel/arch/x86/crypto/crc32c-intel.ko; do
  if [ -e "$modules/$rel" ]; then
    mkdir -p "$rootfs/lib/modules/$krel/$(dirname "$rel")"
    cp "$modules/$rel" "$rootfs/lib/modules/$krel/$rel"
  fi
done
if [ -d "$modules/updates/dkms" ]; then
  mkdir -p "$rootfs/lib/modules/$krel/updates/dkms"
  cp "$modules/updates/dkms/"*.ko "$rootfs/lib/modules/$krel/updates/dkms/" 2>/dev/null || true
  for meta in modules.dep modules.dep.bin modules.alias modules.alias.bin \
              modules.builtin modules.builtin.bin modules.order modules.symbols \
              modules.symbols.bin; do
    [ -e "$modules/$meta" ] && cp "$modules/$meta" "$rootfs/lib/modules/$krel/$meta"
  done
fi
cp bench/guest/fs-init.sh "$rootfs/sbin/init"
chmod 755 "$rootfs/sbin/init"

truncate -s 768M "$root_img"
mke2fs -t ext4 -F -q -b 4096 -d "$rootfs" "$root_img"
say "root image: $root_img ($(stat -c %s "$root_img") bytes)"

# ---- data filesystem bases ------------------------------------------------
base_ext4="$work/data-ext4.img"
truncate -s 8G "$base_ext4"
mke2fs -t ext4 -F -q -b 4096 "$base_ext4"
base_xfs="$work/data-xfs.img"
xfs_ok=0
if command -v mkfs.xfs >/dev/null 2>&1; then
  truncate -s 8G "$base_xfs"
  if mkfs.xfs -f -q "$base_xfs" >/dev/null 2>&1; then
    xfs_ok=1
  fi
fi
base_btrfs="$work/data-btrfs.img"
btrfs_ok=0
if command -v mkfs.btrfs >/dev/null 2>&1; then
  truncate -s 8G "$base_btrfs"
  if mkfs.btrfs -f -q "$base_btrfs" >/dev/null 2>&1; then
    btrfs_ok=1
  fi
fi
# Whole-disk ZFS needs an unpartitioned, signature-free block device. The guest
# passes /dev/vdb straight to zpool; there is no file-backed vdev for this
# target.
base_zfswhole="$work/data-zfswhole.img"
truncate -s 8G "$base_zfswhole"
dd if=/dev/zero of="$base_zfswhole" bs=1M count=8 conv=notrunc status=none
say "ext4 base: $base_ext4; xfs base: $base_xfs (ok=$xfs_ok); btrfs base: $base_btrfs (ok=$btrfs_ok); zfs whole-disk base: $base_zfswhole"

zfs_ok=0
if command -v zpool >/dev/null 2>&1 && [ -e "$modules/updates/dkms/zfs.ko" ]; then
  zfs_ok=1
  zfs_status="available: OpenZFS $(modinfo -k "$krel" -F version zfs 2>/dev/null || echo unknown) built by DKMS for $krel; zfs-whole passes the raw /dev/vdb block device straight to zpool with no partition, loop device, or file-backed vdev; zfs (legacy) keeps its separately labelled file-backed vdev"
else
  zfs_status="unavailable: zpool/zfs tools or the DKMS zfs.ko for $krel are missing in the vm court image"
fi
btrfs_status="unavailable: mkfs.btrfs or the guest btrfs.ko is missing in the vm court image"
if [ "$btrfs_ok" = "1" ] && [ -e "$modules/kernel/fs/btrfs/btrfs.ko" ]; then
  btrfs_status="available: mkfs.btrfs $(mkfs.btrfs --version 2>/dev/null | head -n 1) on the host; guest mounts a real Btrfs filesystem on /dev/vdb"
fi
say "zfs: $zfs_status"
say "btrfs: $btrfs_status"

# ---- QEMU -----------------------------------------------------------------
accel="tcg"
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then accel="kvm"; fi
cpu="max"
[ "$accel" = kvm ] && cpu="host"
say "accel=$accel cpu=$cpu smp=4 guest_mem=1536M"

run_qemu() {
  local target="$1" data="$2" accel="$3" cpu="$4" serial="$5"
  timeout --signal=KILL "$target_timeout" qemu-system-x86_64 \
    -accel "$accel" -cpu "$cpu" -smp 4 -m 1536 \
    -kernel "$kernel" -initrd "$initrd" \
    -append "root=/dev/vda ro rootfstype=ext4 rootwait console=ttyS0,115200 panic=-1 loglevel=4 weft.target=$target weft.reps=$reps" \
    -drive "file=$root_img,if=virtio,format=raw,readonly=on" \
    -drive "file=$data,if=virtio,format=raw,cache=none" \
    -drive "file=$results_img,if=virtio,format=raw,cache=writeback" \
    -display none -vga none -no-user-config -nodefaults -no-reboot \
    -serial "file:$serial" -monitor none
}

declare -Ag target_accel=()
declare -Ag target_exit=()
: > "$work/targets.txt"
for target in ${targets//,/ }; do
  case "$target" in
    ext4|rust|go) base="$base_ext4" ;;
    xfs)
      if [ "$xfs_ok" != "1" ]; then
        say "BLOCKER xfs: mkfs.xfs unavailable or failed; no XFS baseline run"
        target_exit[$target]=255
        echo "$target none 255" >> "$work/targets.txt"
        continue
      fi
      base="$base_xfs"
      ;;
    btrfs)
      if [ "$btrfs_ok" != "1" ]; then
        say "BLOCKER btrfs: mkfs.btrfs unavailable or failed; no Btrfs baseline run"
        target_exit[$target]=255
        echo "$target none 255" >> "$work/targets.txt"
        continue
      fi
      base="$base_btrfs"
      ;;
    zfs-whole)
      if [ "$zfs_ok" != "1" ]; then
        say "BLOCKER zfs-whole: zpool or the DKMS zfs.ko is unavailable; no whole-disk ZFS baseline run"
        target_exit[$target]=255
        echo "$target none 255" >> "$work/targets.txt"
        continue
      fi
      base="$base_zfswhole"
      ;;
    zfs)
      if [ "$zfs_ok" != "1" ]; then
        say "BLOCKER zfs: zpool or the DKMS zfs.ko is unavailable; no ZFS baseline run"
        target_exit[$target]=255
        echo "$target none 255" >> "$work/targets.txt"
        continue
      fi
      base="$base_ext4"
      ;;
    *) say "unknown target $target"; target_exit[$target]=255; echo "$target none 255" >> "$work/targets.txt"; continue ;;
  esac
  say "== target $target =="
  cp --sparse=always "$base" "$data_img"
  truncate -s 256M "$results_img"
  mke2fs -t ext4 -F -q -b 4096 "$results_img"
  serial="$work/serial/$target.log"
  if run_qemu "$target" "$data_img" "$accel" "$cpu" "$serial"; then
    target_exit[$target]=0
    target_accel[$target]="$accel"
  else
    rc=$?
    if [ "$accel" = "kvm" ]; then
      say "target $target: KVM boot failed (rc=$rc); retrying with TCG"
      cp --sparse=always "$base" "$data_img"
      truncate -s 256M "$results_img"
      mke2fs -t ext4 -F -q -b 4096 "$results_img"
      if run_qemu "$target" "$data_img" "tcg" "max" "$serial"; then
        target_exit[$target]=0
        target_accel[$target]="tcg"
      else
        target_exit[$target]=$?
        target_accel[$target]="tcg"
      fi
    else
      target_exit[$target]=$rc
      target_accel[$target]="$accel"
    fi
  fi
  dest="$work/extracted/$target"
  rm -rf "$dest"
  mkdir -p "$dest"
  if debugfs -R "rdump / $dest" "$results_img" >/dev/null 2>&1; then
    say "extracted results for $target to $dest"
  else
    say "BLOCKER results extraction failed for $target"
  fi
  say "target $target exit=${target_exit[$target]} accel=${target_accel[$target]}"
  echo "$target ${target_accel[$target]:-none} ${target_exit[$target]:-255}" >> "$work/targets.txt"
  # Keep the guest serial log beside the extracted results for the receipt.
  cp "$serial" "$dest/serial.log" 2>/dev/null || true
done

# ---- receipt --------------------------------------------------------------
python3 bench/scripts/assemble_fs_cold.py \
  --raw-dir "$work/extracted" \
  --out "$receipt" \
  --work-dir "$work" \
  --log "$log" \
  --targets "$targets" \
  --reps "$reps" \
  --timeout "$target_timeout" \
  --xfs-status "$xfs_ok" \
  --btrfs-status "$btrfs_ok" \
  --zfs-status "$zfs_status"

echo "cold fs receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
