#!/usr/bin/env bash
# Held-out predictive-planner court (T042 / N03).
#
# One invocation:
#   1. fits the model on the FIT families (Qwen2.5-0.5B base + pythia-70m step 0),
#   2. predicts and measures the VALIDATION family (Qwen2.5-3B base subset),
#   3. predicts and measures the HELD-OUT family (Qwen2.5-7B-Instruct subset),
#   4. assembles one `weft.heldout.planner.v1` receipt with the error
#      distributions, the SLA admission grid and the contract's gates.
#
# The split, the fitted functional form, the 10 % SLA margin and the gates are
# frozen in `bench/CONTRACTS-heldout.md`, committed before any number here.
#
# Every phase runs in `weft/bench-court:1` under the standard hard limits
# (3g memory == swap, 1024 pids, 4 cpus, bounded /tmp, cap-drop=ALL). Nothing
# in this script raises a limit.
#
# Environment skips print exactly one `heldout_planner: skip <reason>` line and
# exit 78; an aggregate court records that as a skip, never as a pass.
set -Eeuo pipefail

skip() {
  echo "heldout_planner: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

corpus="$repo_root/.docker-cache/real-corpus"
qwen_fit="$corpus/Qwen2.5-0.5B.safetensors"
pythia_fit="$corpus/pythia-70m-step0.safetensors"
qwen_val="$corpus/Qwen2.5-3B.safetensors"
qwen_hold="$corpus/Qwen2.5-7B-Instruct.safetensors"
for f in "$qwen_fit" "$pythia_fit" "$qwen_val" "$qwen_hold"; do
  [[ -f "$f" ]] || skip "corpus $f is not present (the court refuses to substitute another container)"
done

stamp="${WEFT_HELDOUT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

work_rel=".docker-cache/heldout-$stamp"
work="$repo_root/$work_rel"
rm -rf "$work" 2>/dev/null || true
mkdir -p "$work"
chmod a+rwX "$work"

fit_receipt="bench/receipts/heldout-fit-$stamp.json"
val_receipt="bench/receipts/heldout-validate-$stamp.json"
hold_receipt="bench/receipts/heldout-heldout-$stamp.json"
receipt="bench/receipts/heldout-planner-$stamp.json"
log="bench/receipts/heldout-planner-$stamp.log"
model="$work_rel/model.json"

cleanup() {
  docker run --rm -v "$repo_root/.docker-cache:/c" alpine:3.20 \
    rm -rf "/c/heldout-$stamp" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "heldout_planner: stamp=$stamp fit=qwen0.5b-base+pythia70m-step0 validate=qwen3b heldout=qwen7b-instruct" >&2

# FIT bounds: the embedding (272 MiB) is included on purpose — it anchors the
# per-MiB slope. VALIDATION and HELD-OUT bounds are the contract's declared
# court-memory bounds (192 MiB / 160 MiB).
scripts/docker-court.sh bench "\
cd /work && cargo build --release --quiet --example heldout_planner && \
cargo run --release --quiet --example heldout_planner -- \
  --mode fit \
  --fit qwen0.5b-base=/work/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors \
  --fit pythia70m-step0=/work/.docker-cache/real-corpus/pythia-70m-step0.safetensors \
  --out $fit_receipt --model-out $model --work /work/$work_rel \
  --max-tensor-bytes 335544320 --max-tensors 400 --max-total-bytes 2147483648 && \
cargo run --release --quiet --example heldout_planner -- \
  --mode predict --family qwen2.5-3b-base \
  --container /work/.docker-cache/real-corpus/Qwen2.5-3B.safetensors \
  --out $val_receipt --model-in $model --work /work/$work_rel/validate-store \
  --max-tensor-bytes 201326592 --max-tensors 96 --max-total-bytes 1610612736 && \
cargo run --release --quiet --example heldout_planner -- \
  --mode predict --family qwen2.5-7b-instruct \
  --container /work/.docker-cache/real-corpus/Qwen2.5-7B-Instruct.safetensors \
  --out $hold_receipt --model-in $model --work /work/$work_rel/heldout-store \
  --max-tensor-bytes 167772160 --max-tensors 40 --max-total-bytes 1610612736" \
  2>&1 | tee "$log"

chmod a+rw "$fit_receipt" "$val_receipt" "$hold_receipt" "$log" 2>/dev/null || true

python3 scripts/assemble_heldout.py "$receipt" "$stamp" "$fit_receipt" "$val_receipt" "$hold_receipt" "$log"

chmod a+rw "$receipt" 2>/dev/null || true
