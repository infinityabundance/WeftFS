#!/usr/bin/env bash
# T083 / N10 paired-statistics court.
#
# Runs A/N/B/C/D/E with N=10 paired repetitions in a **randomised order within
# each repetition** (`--order-seed`), retains every raw sample in the receipt,
# and publishes a paired percentile bootstrap 95 % CI on `arm - A`, plus
# whether the orderings claimed by the R3 publication survive.
#
# Telemetry (load average, memory, disk, thermals) is captured before and after
# in `paired-stats-<stamp>.telemetry.json`. A missing source is reported as
# `unavailable`, never as zero.
#
# Environment skips print exactly one `paired_stats: skip <reason>` line and
# exit 78.
set -Eeuo pipefail

skip() {
  echo "paired_stats: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

fixture="$repo_root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
[[ -f "$fixture" ]] || skip "fixture $fixture is not present (the court refuses to substitute another container)"

stamp="${WEFT_PAIRED_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_PAIRED_REPS:-10}"
order_seed="${WEFT_PAIRED_ORDER_SEED:-20261114}"
bootstrap_seed="${WEFT_PAIRED_BOOTSTRAP_SEED:-424242}"

mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

work_rel=".docker-cache/paired-$stamp"
work="$repo_root/$work_rel"
rm -rf "$work" 2>/dev/null || true
mkdir -p "$work"
chmod a+rwX "$work"

raw="bench/receipts/paired-stats-raw-$stamp.json"
log="bench/receipts/paired-stats-$stamp.log"
telemetry="bench/receipts/paired-stats-$stamp.telemetry.json"
receipt="bench/receipts/paired-stats-$stamp.json"

cleanup() {
  docker run --rm -v "$repo_root/.docker-cache:/c" alpine:3.20 \
    rm -rf "/c/paired-$stamp" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "paired_stats: stamp=$stamp reps=$reps order_seed=$order_seed configs=A,N,B,C,D,E" >&2

python3 scripts/capture_telemetry.py "$telemetry" "$stamp" before || true

scripts/docker-court.sh bench "\
cd /work && cargo build --release --quiet --example augmentation_bench && \
cargo run --release --quiet --example augmentation_bench -- run \
  --fixture /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  --fixture-sha256 fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe \
  --work /work/$work_rel \
  --out $raw --log $log \
  --backend local --chunk-size 1048576 \
  --max-others 100 --include-embedding 1 \
  --configs A,N,B,C,D,E --reps $reps --order-seed $order_seed \
  --with-native-app --passes 2 --label paired-stats --seed weft-paired-stats --force"

chmod a+rw "$raw" "$log" 2>/dev/null || true

python3 scripts/capture_telemetry.py "$telemetry" "$stamp" after || true
python3 scripts/paired_stats.py "$raw" "$telemetry" "$receipt" "$bootstrap_seed"
chmod a+rw "$receipt" "$telemetry" 2>/dev/null || true
