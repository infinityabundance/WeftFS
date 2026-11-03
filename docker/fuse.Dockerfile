FROM golang:1.23-bookworm AS go

FROM rust:1-slim-bookworm

# FUSE + Rust + Go court. Two independent toolchains so Rust and Go FUSE
# frontends can be tested under identical limits without a host toolchain.
COPY --from=go /usr/local/go /usr/local/go
ENV PATH=/usr/local/go/bin:$PATH \
    GOTOOLCHAIN=local \
    CARGO_HOME=/cargo \
    CARGO_TERM_COLOR=never
RUN mkdir -p /cargo

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates fuse3 libfuse3-dev build-essential pkg-config \
      strace fio attr acl util-linux coreutils findutils \
      psmisc procps \
 && (apt-get install -y --no-install-recommends pjdfstest || true) \
 && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /mnt/weft-fuse
WORKDIR /work
