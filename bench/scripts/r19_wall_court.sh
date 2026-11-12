#!/usr/bin/env bash
# R19 before/after write-wall court.
#
# Runs **inside** the bench court (`scripts/docker-court.sh bench "bash
# bench/scripts/r19_wall_court.sh"`), so the hard limits are the usual ones:
# 3 GiB memory == swap, 1024 pids, 4 CPUs, bounded /tmp.
#
# Same fixture, same seed, same fixed timestamp, same configuration list as the
# R17/R18 ablation matrix, minus the two raw-publication controls whose 405
# per-blob `fsync` publications dominate their own wall (B, B-group) and which
# contain no mode-9 encode. The rows that matter for R19 are A (native anchor),
# C and C-noz19 (the shipped planner), C-legacy (the exhaustive planner that
# produces the decision stream), C-nom9 and C-both (ablations), D (fast raw) and
# E (fast raw + one commit).
#
# `WEFT_R19_STAMP` pins the receipt name and `WEFT_R19_LABEL` names the side
# (`before` / `after`). `WEFT_R19_REPS` defaults to 7.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_R19_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
label="${WEFT_R19_LABEL:-unknown}"
reps="${WEFT_R19_REPS:-7}"
configs="${WEFT_R19_CONFIGS:-A,C,C-legacy,C-noz19,C-nom9,C-both,D,E}"
receipt="$root/bench/receipts/r19-wall-$label-$stamp.json"
log="$root/bench/receipts/r19-wall-$label-$stamp.log"
work="$root/.docker-cache/r19-wall-$label-$stamp"
corpus="$root/.docker-cache/real-corpus"

if [[ ! -f "$corpus/Qwen2.5-0.5B-Instruct.safetensors" ]]; then
  echo "r19_wall_court: missing fixture $corpus/Qwen2.5-0.5B-Instruct.safetensors" >&2
  exit 1
fi
mkdir -p "$root/bench/receipts" "$work"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true

# The R3 contract-court receipt for the same stamp, when present, is the
# same-revision default-C identity reference (`default_c_matches_official_court`).
official="$root/bench/receipts/augmentation-local-$stamp.json"
official_args=()
if [[ -f "$official" ]]; then
  official_args=(--official-receipt "$official")
fi

echo "== R19 wall, label=$label, configs=$configs, reps=$reps =="
cargo run --release --quiet --example augmentation_bench -- ablate \
  --fixture "$corpus/Qwen2.5-0.5B-Instruct.safetensors" \
  --work "$work" \
  --out "$receipt" \
  --log "$log" \
  --configs "$configs" \
  --reps "$reps" \
  --max-others 100 --include-embedding 1 \
  --backend local --chunk-size 1048576 --seed weft-augmentation-r3 \
  "${official_args[@]}" \
  --force

echo "r19_wall_court: receipt $receipt"
