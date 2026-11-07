#!/usr/bin/env bash
# Zero-trust KV page court.
#
#   scripts/docker-court.sh rust "cd /work && bash bench/scripts/kv_capability_court.sh"
#
# Writes one receipt with the four reader paths (full envelope load + verify,
# cached verify, decode-only control, denied control) at 1 KiB and 64 KiB, over
# at least 1,000 samples each, plus the ordinary whole-object read as a control.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_STAMP="$stamp"
work="$root/.docker-cache/kv-capability-$stamp"
receipt="bench/receipts/kv-capability-$stamp.json"
reps="${WEFT_KV_REPS:-2000}"
mkdir -p "$work" bench/receipts

echo "kv_capability_court: stamp=$stamp reps=$reps"
echo "kv_capability_court: image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "kv_capability_court: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"

echo "== unit tests =="
cargo test --release --test kv_capability

echo "== measure =="
cargo run --release --quiet --example kv_capability_bench -- \
  --out "$receipt" --work "$work" --reps "$reps"

echo "kv_capability_court: receipt $receipt"
