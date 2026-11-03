FROM golang:1.23-bookworm

# Minimal Go build/test court. Debian (not Alpine) so the race detector and
# pprof have a working C toolchain. No extra network tooling.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

ENV GOTOOLCHAIN=local \
    GOFLAGS=-p=2
WORKDIR /work
