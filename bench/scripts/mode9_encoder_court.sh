#!/usr/bin/env bash
# R19 mode-9 encoder court.
#
# Runs **inside** the bench court (the hard limits of `scripts/docker-court.sh
# bench` apply unchanged: 3 GiB, 3 GiB swap, 1024 pids, 4 CPUs, bounded /tmp):
#
#   scripts/docker-court.sh bench "bash bench/scripts/mode9_encoder_court.sh"
#
# It times the pre-R19 reference encoder and the shipped encoder on the same
# tensors, asserts they are byte-identical, replicates the pre-R19 pipeline
# stage by stage, and records page-fault counts and a fresh/recycled allocation
# probe so the old "frame assembly" residual is attributed to a mechanism.
#
# `WEFT_MODE9_STAMP` pins the receipt name, `WEFT_MODE9_THREADS` the parallel
# emission thread count used for the `threaded` column (default 4).
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_MODE9_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
threads="${WEFT_MODE9_THREADS:-4}"
receipt="${WEFT_MODE9_RECEIPT:-$root/bench/receipts/mode9-encoder-$stamp.json}"
corpus="$root/.docker-cache/real-corpus"
fixture="$corpus/Qwen2.5-0.5B-Instruct.safetensors"

if [[ ! -f "$fixture" ]]; then
  echo "mode9_encoder_court: missing fixture $fixture" >&2
  exit 1
fi
mkdir -p "$root/bench/receipts"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true

echo "== mode-9 encoder stage profile (7 repetitions, $threads emission threads) =="
cargo run --release --quiet --example mode9_encoder_bench -- \
  --fixture "$fixture" \
  --out "$receipt" \
  --label "R19 mode-9 encoder" \
  --others 8 \
  --reps 7 \
  --threads "$threads"

echo "mode9_encoder_court: receipt $receipt"
