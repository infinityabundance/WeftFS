FROM debian:bookworm-slim

# FUSE runtime court: mount tooling, syscall traces, fio, and the standard
# POSIX conformance suite if available from apt.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates fuse3 libfuse3-dev build-essential pkg-config \
      strace fio attr acl util-linux coreutils findutils \
 && (apt-get install -y --no-install-recommends pjdfstest || true) \
 && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /mnt/weft-fuse
WORKDIR /work
