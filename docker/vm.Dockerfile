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

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates build-essential pkg-config \
      qemu-system-x86 qemu-utils \
      busybox-static e2fsprogs cpio gzip xz-utils zstd \
      linux-image-amd64 initramfs-tools kmod \
      jq util-linux coreutils procps \
 && rm -rf /var/lib/apt/lists/*

# Debian's initramfs includes virtio-blk and ext4 (MODULES=most). It is built
# here so the court does not depend on the host's /boot or module tree.
RUN update-initramfs -c -k all \
 && ls -l /boot

WORKDIR /work
