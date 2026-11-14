#!/usr/bin/env bash
# N02 — the durability tail, quantified.
#
# Runs **inside** the bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/n02_durability_tail_court.sh"
#
# Steps
#   1. Write a real C-pipe4 store of the frozen Qwen2.5-0.5B-Instruct subset with
#      the phase profile on, so the store's own write wall and its phases are in
#      the same session as the probe.
#   2. Run `n02_durability_tail.py` over that store's published blobs: per-object
#      files, 16 larger files + index, and one contiguous pack, seven interleaved
#      repetitions, seeded order, same acknowledgement contract.
#   3. Assemble one receipt with a critical-path account whose terms sum to the
#      store wall (the residual is stated, never zeroed).
#
# The probe is a *device and acknowledgement* comparison of the bytes a real
# store publishes. It does not change a format, a root or a store layout.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_N02_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_N02_REPS:-7}"
fixture="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
receipt="$root/bench/receipts/n02-durability-tail-$stamp.json"
log="$root/bench/receipts/n02-durability-tail-$stamp.log"
work="$root/.docker-cache/n02-durability-$stamp"

if [[ ! -f "$fixture" ]]; then
  echo "n02_durability_tail: skip missing fixture $fixture" >&2
  exit 78
fi
mkdir -p "$root/bench/receipts"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true
rm -rf "$work"
mkdir -p "$work"

{
  echo "== N02 durability tail, stamp=$stamp reps=$reps =="
  df -h "$root" | tail -1
  echo "== device control =="
  dd if=/dev/zero of="$work/dd" bs=1M count=200 conv=fsync 2>&1 | tail -1
  rm -f "$work/dd"

  echo "== write the store (C-pipe4, phase profile on) =="
  cargo run --release --quiet --example augmentation_bench -- prepare \
    --fixture "$fixture" --work "$work" --max-others 100 --include-embedding 1
  cargo run --release --quiet --example augmentation_bench -- write \
    --work "$work" --subset "$work/subset.json" --config C-pipe4 \
    --backend local --phase-profile \
    --phase-out "$work/write.json" --chunk-size 1048576 \
    --seed weft-augmentation-r3

  echo "== durability-tail variants =="
  python3 bench/scripts/n02_durability_tail.py \
    --store "$work/C-pipe4/store" \
    --work "$work/probe" \
    --out "$work/probe.json" \
    --reps "$reps" \
    --seed 20261114

  echo "== assemble receipt =="
  python3 bench/scripts/n02_assemble_receipt.py \
    "$work/write.json" "$work/probe.json" "$receipt" "$stamp"
} 2>&1 | tee -a "$log"

echo "n02_durability_tail: receipt $receipt"
echo "n02_durability_tail: log $log"
