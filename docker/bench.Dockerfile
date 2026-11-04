FROM golang:1.23-bookworm AS go

FROM rust:1-slim-bookworm

# Benchmark court: Rust toolchain (from the base image) plus the external
# codec CLIs, fio, FUSE userspace, and a Go toolchain so the filesystem
# benchmark can mount both independent implementations in one container.
# No editors, shells beyond bash, or convenience tooling are added.
COPY --from=go /usr/local/go /usr/local/go
ENV PATH=/usr/local/go/bin:$PATH \
    GOTOOLCHAIN=local \
    CARGO_HOME=/cargo \
    CARGO_TERM_COLOR=never

RUN mkdir -p /cargo /mnt/weft-fuse /mnt/weft-bench

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates build-essential pkg-config libfuse3-dev fuse3 \
      zstd xz-utils gzip brotli \
      sqlite3 fio time \
      util-linux coreutils findutils procps \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /work
