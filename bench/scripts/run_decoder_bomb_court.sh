#!/usr/bin/env bash
# F5.1 / B1 — decoder-bomb court.
#
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_decoder_bomb_court.sh"
#
# The court builds both independent CLIs, compiles the stdlib-only runner, and
# executes every committed adversarial vector twice per decoder. It asserts:
# nonzero exit, the expected normative class in both, identical classes, no
# output file, no timeout, no OOM kill (137), and a stable class across repeats.
# Each invocation is wrapped in `timeout 10s /usr/bin/time`, so maximum RSS and
# wall time are recorded by the kernel.
set -Eeuo pipefail

ROOT=/work
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RECEIPTS="$ROOT/bench/receipts"
OUT="$RECEIPTS/decoder-bombs-$STAMP.json"
LOG="$RECEIPTS/decoder-bombs-$STAMP.log"
BIN="$ROOT/.docker-cache/f5/bin"
RUST_BIN="${CARGO_TARGET_DIR:-$ROOT/target}/release/weftcodec"
GO_BIN="$BIN/weftcodec-go"
RUNNER="$BIN/decoder_bomb_court"
VECTORS="$ROOT/WeftCodec/conformance/v1/adversarial"
MIRROR="$ROOT/WeftCodec-Go/conformance/v1/adversarial"

mkdir -p "$BIN" "$RECEIPTS"

{
  echo "decoder-bomb court"
  echo "stamp: $STAMP"
  echo "vectors: $VECTORS"
  echo "mirror: $MIRROR"
  echo "rust binary: $RUST_BIN"
  echo "go binary: $GO_BIN"
  echo "command: scripts/docker-court.sh bench \"bash bench/scripts/run_decoder_bomb_court.sh\""
  echo "timeout: 10s per CLI invocation; repeat: 2"
  echo

  echo "building Rust release CLI"
  ( cd "$ROOT/WeftCodec" && cargo build --release --quiet )
  echo "building Go CLI"
  ( cd "$ROOT/WeftCodec-Go" && go build -o "$GO_BIN" ./cmd/weftcodec )
  echo "building court runner"
  ( cd /tmp && GO111MODULE=off go build -o "$RUNNER" "$ROOT/bench/scripts/decoder_bomb_court.go" )
  echo

  "$RUNNER" \
    --rust "$RUST_BIN" \
    --go "$GO_BIN" \
    --vectors "$VECTORS" \
    --mirror "$MIRROR" \
    --out "$OUT" \
    --stamp "$STAMP" \
    --timeout 10 \
    --repeat 2
} 2>&1 | tee "$LOG"

echo "receipt: $OUT"
echo "raw log: $LOG"
