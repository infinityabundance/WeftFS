# Build context is the program home repository root.
FROM rust:1-slim-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential pkg-config libfuse3-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*
ENV CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY WeftCodec ./WeftCodec
RUN cargo build --release --locked --features fuse --bin weftfs \
 && strip /src/target/release/weftfs

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates fuse3 libfuse3-3 \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/weftfs /usr/local/bin/weftfs
LABEL org.opencontainers.image.title="WeftFS" \
      org.opencontainers.image.description="WeftFS Rust store and Linux FUSE frontend (Context Roots, causal lineage)" \
      org.opencontainers.image.source="https://github.com/infinityabundance/WeftFS" \
      org.opencontainers.image.licenses="Apache-2.0"
# Mounting requires: --device /dev/fuse --cap-add SYS_ADMIN
ENTRYPOINT ["weftfs"]
