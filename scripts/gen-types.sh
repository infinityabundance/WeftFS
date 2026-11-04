#!/usr/bin/env bash
# Generate (or check) the Rust and Go identity types from the canonical schema.
#
#   scripts/gen-types.sh          # write WeftCodec/src/generated.rs and
#                                 # WeftCodec-Go/generated/generated.go
#   scripts/gen-types.sh --check  # fail (exit 1) on any drift
#
# Generation runs inside the OOM-protected Go court; nothing executes on the
# host. The court writes into its own cache because source directories are not
# writable by the unprivileged court identity; the host then installs the two
# generated files (a file copy, not a build). Generated files are committed so
# the codec repositories build without a generation step.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [[ $# -gt 0 && "${1:-}" != "--check" ]]; then
  echo "usage: $0 [--check]" >&2
  exit 64
fi

common="cd /work && GO111MODULE=off go run scripts/gen-types.go \
  -schema spec/schema/identity-v1.schema"

if [[ "${1:-}" == "--check" ]]; then
  exec scripts/docker-court.sh go "$common \
    -rust WeftCodec/src/generated.rs \
    -go WeftCodec-Go/generated/generated.go --check"
fi

mkdir -p .docker-cache/generated WeftCodec-Go/generated
scripts/docker-court.sh go "$common \
  -rust .docker-cache/generated/generated.rs \
  -go .docker-cache/generated/generated.go"
cp .docker-cache/generated/generated.rs WeftCodec/src/generated.rs
cp .docker-cache/generated/generated.go WeftCodec-Go/generated/generated.go
echo "installed WeftCodec/src/generated.rs and WeftCodec-Go/generated/generated.go"
