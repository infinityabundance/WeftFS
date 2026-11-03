FROM rust:1-slim-bookworm

# Minimal build court: compiler, linker, pkg-config, and libfuse3 for the
# filesystem crate. No interpreters, no editors, no convenience tooling.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential pkg-config libfuse3-dev fuse3 ca-certificates \
 && rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/cargo \
    CARGO_NET_RETRY=3 \
    CARGO_TERM_COLOR=never
RUN mkdir -p /cargo
WORKDIR /work
