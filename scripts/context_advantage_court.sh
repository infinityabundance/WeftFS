#!/usr/bin/env bash
# Context Advantage Court (R26).
#
# One backend — the WeftFS local POSIX data plane — used three ways:
#   N  backend alone, no metadata at all;
#   M  backend + a strong application-level metadata system (zstd-3 blob,
#      canonical manifest, key->value index on the brief's five dimensions plus
#      position, full value validation before reuse, TTL/eviction, own subset
#      selection);
#   W  backend + the WeftFS context plane (provenance-bound KV pages, mode 19,
#      capability-scoped reads, weft.checkpoint.v1 manifests, causal closure).
#
# The frozen methodology is `bench/CONTRACTS-context-advantage.md`, committed
# before this court produced any measurement. Every arm writes the same logical
# objects through the same commit protocol with the same chunk size, the same
# durability barrier, no decoded-object cache, no single-flight, one thread and
# one request at a time.
#
# Usage:
#   scripts/context_advantage_court.sh                  # 7 interleaved reps
#   scripts/context_advantage_court.sh --reduced        # 3 reps, aggregate court
#   scripts/context_advantage_court.sh --reps=5
#   scripts/context_advantage_court.sh --keep-work
#
# Every repetition runs each arm in a fresh process against a fresh store root.
# The arm order rotates by one position per repetition, so no arm always
# occupies the same slot; the order of every repetition is recorded in the
# receipt.
#
# Every container uses the same hard limits as `scripts/docker-court.sh`
# (memory 3g == swap, pids 1024, cpus 4, bounded /tmp, no-new-privileges,
# cap-drop=ALL, nproc 2048). Nothing here is a throughput, media, power-loss or
# fleet claim; see the contract §7.
#
# Environment skips print exactly one `context_advantage_court: skip <reason>`
# line and exit 78. The aggregate verification court records that as `skip`,
# never as a pass.
set -Eeuo pipefail

skip() {
  echo "context_advantage_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

reps="${WEFT_CONTEXT_ADVANTAGE_REPS:-7}"
reduced=0
keep_work=0
for arg in "$@"; do
  case "$arg" in
    --reduced) reduced=1 ;;
    --keep-work) keep_work=1 ;;
    --reps=*) reps="${arg#--reps=}" ;;
    --reps) echo "--reps needs a value (use --reps=N)" >&2; exit 64 ;;
    -h|--help)
      sed -n '2,36p' "$0"
      exit 0
      ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done
case "$reps" in
  ''|*[!0-9]*) echo "--reps must be a positive integer" >&2; exit 64 ;;
esac
[[ "$reps" -ge 1 ]] || { echo "--reps must be a positive integer" >&2; exit 64; }
if [[ "$reduced" == "1" ]]; then
  reps=3
  label="reduced"
else
  label="${WEFT_CONTEXT_ADVANTAGE_LABEL:-full}"
fi

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

stamp="${WEFT_CONTEXT_ADVANTAGE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_CONTEXT_ADVANTAGE_STAMP="$stamp"
work="$repo_root/.docker-cache/context-advantage-$stamp"
receipt="bench/receipts/context-advantage-$stamp.json"
log="bench/receipts/context-advantage-$stamp.log"
mkdir -p "$work" bench/receipts
chmod a+rwX "$work" bench/receipts 2>/dev/null || true

arms=(N M W)
phase_flag=""
if [[ "$reduced" == "1" ]]; then
  phase_flag="--reduced"
fi

echo "context_advantage_court: label=$label reps=$reps stamp=$stamp" >&2
echo "context_advantage_court: one backend (local POSIX data plane), three arms, interleaved" >&2

for ((rep = 1; rep <= reps; rep++)); do
  rotation=$(( (rep - 1) % ${#arms[@]} ))
  order=("${arms[@]:rotation}" "${arms[@]:0:rotation}")
  joined="$(IFS=,; echo "${order[*]}")"
  rep_dir="$work/rep-$(printf '%02d' "$rep")"
  mkdir -p "$rep_dir"
  order_index=0
  for arm in "${order[@]}"; do
    order_index=$((order_index + 1))
    echo "context_advantage_court: rep $rep/$reps arm $arm order=$joined slot=$order_index" >&2
    if ! scripts/docker-court.sh bench \
      "cd /work && WEFT_CONTEXT_ADVANTAGE_STAMP=$stamp WEFT_CONTEXT_ADVANTAGE_LABEL=$label cargo run --release --quiet --example context_advantage_court -- run \
         --arm $arm \
         --work /work/.docker-cache/context-advantage-$stamp/rep-$(printf '%02d' "$rep") \
         --out /work/.docker-cache/context-advantage-$stamp/rep-$(printf '%02d' "$rep")/$arm.json \
         --rep $rep --order $joined --order-index $order_index --reps $reps $phase_flag" \
      >>"$work/run.log" 2>&1; then
      echo "context_advantage_court: rep $rep arm $arm failed; tail of $work/run.log follows" >&2
      tail -40 "$work/run.log" >&2 || true
      exit 1
    fi
  done
done

echo "context_advantage_court: assembling $receipt" >&2
if ! scripts/docker-court.sh bench \
  "cd /work && WEFT_CONTEXT_ADVANTAGE_STAMP=$stamp WEFT_CONTEXT_ADVANTAGE_LABEL=$label cargo run --release --quiet --example context_advantage_court -- assemble \
     --work /work/.docker-cache/context-advantage-$stamp \
     --out /work/$receipt --log /work/$log --reps $reps $phase_flag" \
  >>"$work/run.log" 2>&1; then
  echo "context_advantage_court: assemble failed; tail of $work/run.log follows" >&2
  tail -40 "$work/run.log" >&2 || true
  exit 1
fi

[[ -f "$receipt" ]] || { echo "context_advantage_court: expected receipt $receipt was not written" >&2; exit 1; }
result="$(jq -r '.result' "$receipt")"
claim="$(jq -r '.claim_holds' "$receipt")"
echo "context_advantage_court: receipt $receipt (result=$result claim_holds=$claim label=$label reps=$reps)" >&2
echo "context_advantage_court: log $log" >&2

if [[ "$keep_work" != "1" ]]; then
  rm -rf "$work" 2>/dev/null || true
fi

if [[ "$result" != "pass" ]]; then
  echo "context_advantage_court: measurement was not valid (result=$result); see $receipt" >&2
  exit 1
fi
