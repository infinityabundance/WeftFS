FROM golang:1.23-bookworm AS go

FROM rust:1-slim-bookworm

# F9.2 power-loss court: QEMU + a Debian kernel/initramfs for a tiny read-only
# root filesystem, plus both independent WeftFS CLIs can be built from the
# mounted working tree. The guest never runs on the host: QEMU runs here, under
# the same hard memory/pid/cpu limits as every other court.
COPY --from=go /usr/local/go /usr/local/go
ENV PATH=/usr/local/go/bin:$PATH \
    GOTOOLCHAIN=local \
    GOFLAGS=-p=2 \
    CARGO_HOME=/cargo \
    CARGO_TERM_COLOR=never

RUN mkdir -p /cargo

RUN sed -i 's/^Components: main$/Components: main contrib non-free non-free-firmware/' \
      /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates build-essential pkg-config \
      qemu-system-x86 qemu-utils \
      busybox-static e2fsprogs cpio gzip xz-utils zstd \
      linux-image-amd64 linux-headers-amd64 initramfs-tools kmod \
      zfsutils-linux zfs-dkms \
      fio xfsprogs libfuse3-dev fuse3 python3 \
      jq util-linux coreutils procps \
 && rm -rf /var/lib/apt/lists/*

# Btrfs and strace are in a separate layer so adding them does not rerun the
# DKMS zfs build. strace records the exact zpool label/partition syscalls in the
# whole-disk guest and is not used on the fio I/O path.
RUN apt-get update \
 && apt-get install -y --no-install-recommends btrfs-progs strace \
 && rm -rf /var/lib/apt/lists/*

# Build the OpenZFS module for the Debian guest kernel at image-build time so
# the cold-cache fio court can run a real ZFS target in the guest. The module
# is built by DKMS; no module is loaded on the host.
RUN KREL="$(ls -1 /lib/modules | sort -V | tail -n 1)" \
 && depmod -a "$KREL" \
 && ls -l "/lib/modules/$KREL/updates/dkms/" \
 && modinfo -k "$KREL" zfs | head -5

# Debian's initramfs includes virtio-blk and ext4 (MODULES=most). It is built
# here so the court does not depend on the host's /boot or module tree.
RUN update-initramfs -c -k all \
 && ls -l /boot

WORKDIR /work
