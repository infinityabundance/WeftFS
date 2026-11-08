#!/usr/bin/env bash
# Prepare/commit checkpoint court (local backend).
#
# Measures `Store::write_checkpoint` (sequential) and
# `Store::write_checkpoint_parallel` at 1/2/4/8 ranks on the real
# Qwen2.5-0.5B base container, and proves every path publishes the same head
# and the same checkpoint manifest root for the same deterministic seed.
#
# Runs inside the OOM-protected bench court; the same hard limits as every
# other court apply.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

stamp="${WEFT_PARALLEL_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/parallel-checkpoint-${stamp}.json"
model="/work/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors"
work_host=".docker-cache/bench-parallel"
work="/work/.docker-cache/bench-parallel"

if [[ ! -f ".docker-cache/real-corpus/Qwen2.5-0.5B.safetensors" ]]; then
  echo "parallel_checkpoint_court: missing .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors" >&2
  exit 78
fi

mkdir -p bench/receipts "$work_host"
chmod a+rwX bench/receipts "$work_host" 2>/dev/null || true

scripts/docker-court.sh bench \
  "cd /work && cargo run --release --quiet --example bench_parallel_checkpoint -- \
     --fast --reps 5 --model ${model} --work ${work} --out ${receipt} --stamp ${stamp}"

echo "parallel_checkpoint_court: receipt ${receipt}" >&2
