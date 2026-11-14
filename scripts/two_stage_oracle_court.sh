#!/usr/bin/env bash
# Sampled-vs-exhaustive planner oracle (N11).
#
# Runs the shipped `TwoStage` planner and a forced `Exhaustive` planner on the
# same bytes, over the eight adversarial strata declared in
# `bench/CONTRACTS-heldout.md` §5 and over the real validation and held-out
# tensors, and publishes the estimate-violation rate and skip-regret.
#
# Environment skips print exactly one `two_stage_oracle: skip <reason>` line and
# exit 78. All phases run in the bench court under the standard hard limits.
set -Eeuo pipefail

skip() {
  echo "two_stage_oracle: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

corpus="$repo_root/.docker-cache/real-corpus"
val="$corpus/Qwen2.5-3B.safetensors"
hold="$corpus/Qwen2.5-7B-Instruct.safetensors"
for f in "$val" "$hold"; do
  [[ -f "$f" ]] || skip "corpus $f is not present (the court refuses to substitute another container)"
done

stamp="${WEFT_ORACLE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

work_rel=".docker-cache/oracle-$stamp"
work="$repo_root/$work_rel"
rm -rf "$work" 2>/dev/null || true
mkdir -p "$work"
chmod a+rwX "$work"

synthetic="bench/receipts/two-stage-oracle-synthetic-$stamp.json"
validate="bench/receipts/two-stage-oracle-validate-$stamp.json"
heldout="bench/receipts/two-stage-oracle-heldout-$stamp.json"
receipt="bench/receipts/two-stage-oracle-$stamp.json"
log="bench/receipts/two-stage-oracle-$stamp.log"

cleanup() {
  docker run --rm -v "$repo_root/.docker-cache:/c" alpine:3.20 \
    rm -rf "/c/oracle-$stamp" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "two_stage_oracle: stamp=$stamp strata=8 validate=qwen3b heldout=qwen7b" >&2

scripts/docker-court.sh bench "\
cd /work && cargo build --release --quiet --example two_stage_oracle && \
cargo run --release --quiet --example two_stage_oracle -- \
  --family synthetic --out $synthetic --work /work/$work_rel/synthetic \
  --seed 11400714819323198485 && \
cargo run --release --quiet --example two_stage_oracle -- \
  --family qwen2.5-3b-base --container /work/.docker-cache/real-corpus/Qwen2.5-3B.safetensors \
  --out $validate --work /work/$work_rel/validate \
  --max-tensor-bytes 201326592 --max-tensors 96 --max-total-bytes 1610612736 \
  --seed 11400714819323198485 && \
cargo run --release --quiet --example two_stage_oracle -- \
  --family qwen2.5-7b-instruct --container /work/.docker-cache/real-corpus/Qwen2.5-7B-Instruct.safetensors \
  --out $heldout --work /work/$work_rel/heldout \
  --max-tensor-bytes 167772160 --max-tensors 40 --max-total-bytes 1610612736 \
  --seed 11400714819323198485" \
  2>&1 | tee "$log"

chmod a+rw "$synthetic" "$validate" "$heldout" "$log" 2>/dev/null || true

python3 scripts/assemble_oracle.py "$receipt" "$stamp" "$synthetic" "$validate" "$heldout" "$log"
chmod a+rw "$receipt" 2>/dev/null || true
