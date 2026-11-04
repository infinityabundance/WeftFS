#!/usr/bin/env bash
# Matched Go/Rust WeftCodec performance court.
#
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_matched_codec_bench.sh"
#
# Both implementations are exercised as CLI processes with identical fixtures,
# flags, and default limits. Every decode is verified. Cross-decode is
# re-checked per frame.
#
# Outputs:
#   bench/receipts/bench-codec-matched-<UTC>.json
#   bench/receipts/bench-codec-matched-<UTC>.log
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/bench-codec-matched-${stamp}.json"
log="bench/receipts/bench-codec-matched-${stamp}.log"
scratch="${WEFT_MATCHED_SCRATCH:-$root/.docker-cache/bench-matched/$stamp}"
bin="$scratch/bin"
mkdir -p "$bin" "$scratch"

exec > >(tee -a "$log") 2>&1

echo "matched codec bench: image=${WEFT_COURT_IMAGE:-unknown} kernel=$(uname -r)"

# Rust WeftCodec CLI. Use an isolated target dir so this build cannot reuse a
# stale binary from another crate.
(cd WeftCodec && cargo build --release --target-dir "$root/.docker-cache/target-weftcodec" --bin weftcodec)
RUST_CLI="$root/.docker-cache/target-weftcodec/release/weftcodec"

# Go WeftCodec CLI (independent implementation, CGO disabled).
(cd WeftCodec-Go && CGO_ENABLED=0 go build -trimpath -o "$bin/go-weftcodec" ./cmd/weftcodec)
GO_CLI="$bin/go-weftcodec"

# The court harness itself (stdlib only).
(cd bench/matched && CGO_ENABLED=0 go build -trimpath -o "$bin/matched-court" .)
"$bin/matched-court" \
  --rust-cli "$RUST_CLI" \
  --go-cli "$GO_CLI" \
  --scratch "$scratch" \
  --iterations "${WEFT_MATCHED_ITERATIONS:-7}" \
  --small-iterations "${WEFT_MATCHED_SMALL_ITERATIONS:-15}" \
  --out "$receipt"

echo "matched codec receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
