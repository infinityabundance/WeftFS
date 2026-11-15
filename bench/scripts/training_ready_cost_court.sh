#!/usr/bin/env bash
# P03 court: incremental ready-state cache and time-to-ready on a real
# checkpoint.
#
#   bench/scripts/training_ready_cost_court.sh
#
# Uses the cached Qwen2.5-0.5B BF16 container (never committed) in a copy of the
# cached P3 store, writes a 2-shard raw checkpoint, and measures:
#   * cold check (full validation + decode),
#   * warm-first and repeated warm checks (cache hit),
#   * cold/warm `materialize --training-ready` (decode + write),
#   * one policy-invalidation miss.
# Everything runs in the fuse court under the default hard limits. Exit 78 with
# one `training_ready_cost: skip <reason>` line when the environment cannot run.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

if [[ "$(uname -m)" != "x86_64" ]]; then
  echo "training_ready_cost: skip this court is pinned to the x86_64 images"
  exit 78
fi
if ! command -v docker >/dev/null 2>&1; then
  echo "training_ready_cost: skip no docker CLI on PATH"
  exit 78
fi
if ! docker info >/dev/null 2>&1; then
  echo "training_ready_cost: skip no reachable docker daemon"
  exit 78
fi
if ! docker image inspect weft/fuse-court:1 >/dev/null 2>&1; then
  echo "training_ready_cost: skip the pinned weft/fuse-court:1 image is absent"
  exit 78
fi
if [[ ! -d .docker-cache/real-corpus/store-qwen-base ]] \
  || [[ ! -f .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors ]]; then
  echo "training_ready_cost: skip the cached Qwen2.5-0.5B store/container is absent"
  exit 78
fi

stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work=".docker-cache/training-ready-cost-${stamp}"
mkdir -p "$work" bench/receipts
rm -rf "$work"/*
log="bench/receipts/training-ready-cost-${stamp}.log"
: > "$log"
chmod 0666 "$log"

scripts/docker-court.sh fuse \
  "WEFT_STAMP=${stamp} bash bench/scripts/training_ready_cost_inner.sh ${stamp}" \
  >>"$log" 2>&1 || {
    rc=$?
    echo "training-ready cost court failed (exit $rc); see $log" >&2
    tail -40 "$log" >&2
    exit 1
  }

receipt="bench/receipts/training-ready-cost-${stamp}.json"
if [[ ! -f "$receipt" ]]; then
  echo "training-ready cost court produced no receipt" >&2
  exit 1
fi
python3 - "$receipt" <<'PY'
import json
import sys
with open(sys.argv[1]) as handle:
    receipt = json.load(handle)
print("training_ready_cost: %s (cold check %.1f ms, warm check %.1f ms)" % (
    receipt.get("result"),
    receipt["timings"]["check_cold_ms"],
    receipt["timings"]["check_warm_p50_ms"]))
if receipt.get("result") != "pass":
    sys.exit(1)
PY
