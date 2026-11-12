#!/usr/bin/env bash
# R21 pipelined-group-writer court.
#
# Runs **inside** the bench court (`scripts/docker-court.sh bench "bash
# bench/scripts/r21_pipeline_court.sh"`), so the hard limits are the usual ones:
# 3 GiB memory == swap, 1024 pids, 4 CPUs, bounded /tmp. Nothing is raised.
#
# Rows:
#   A            native per-tensor, no WeftFS code in the data path (comparator)
#   C            shipped sequential planned writer (the R20 default)
#   C-pipe1      the same planner through the pipeline, one encode thread
#   C-pipe2      two encode threads
#   C-pipe4      four encode threads (the court's CPU count)
#   C-pipe8      eight encode threads (deliberate oversubscription of 4 CPUs)
#   C-pipe4-nosync  four threads, one durability batch for the whole group
#   C-legacy     exhaustive planner (the R18 decision-diff control)
#   E            planned writer + context/snapshot/provenance protocol
#   E-pipe4      E through the pipeline
#
# The contract court is run separately with the **same stamp**
# (`WEFT_AUGMENTATION_STAMP=<stamp> scripts/augmentation_court.sh --local`),
# because nesting `docker-court.sh` is impossible inside the court; when its
# receipt exists it is passed as `--official-receipt`.
#
# `WEFT_R21_STAMP` pins the receipt names, `WEFT_R21_REPS` defaults to 7 and
# `WEFT_R21_CONFIGS` overrides the row list.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_R21_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_R21_REPS:-7}"
configs="${WEFT_R21_CONFIGS:-A,C,C-pipe1,C-pipe2,C-pipe4,C-pipe8,C-pipe4-nosync,C-legacy,E,E-pipe4}"
fixture="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
receipt="$root/bench/receipts/r21-pipeline-$stamp.json"
log="$root/bench/receipts/r21-pipeline-$stamp.log"
work="$root/.docker-cache/r21-pipeline-$stamp"
official="$root/bench/receipts/augmentation-local-$stamp.json"

if [[ ! -f "$fixture" ]]; then
  echo "r21_pipeline_court: missing fixture $fixture" >&2
  exit 1
fi
mkdir -p "$root/bench/receipts"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true

echo "== R21 pipeline matrix, configs=$configs, reps=$reps =="
official_args=()
if [[ -f "$official" ]]; then
  official_args=(--official-receipt "$official")
else
  echo "r21_pipeline_court: warning: no contract-court receipt at $official; the identity check will be absent" >&2
fi
cargo run --release --quiet --example augmentation_bench -- ablate \
  --fixture "$fixture" \
  --work "$work" \
  --out "$receipt" \
  --log "$log" \
  --configs "$configs" \
  --reps "$reps" \
  --max-others 100 --include-embedding 1 \
  --backend local --chunk-size 1048576 --seed weft-augmentation-r3 \
  "${official_args[@]}" \
  --force

echo "r21_pipeline_court: receipt $receipt"

# --- product-path verify: a store written by the pipeline, checked by the CLI.
verify_work="$root/.docker-cache/r21-verify-$stamp"
rm -rf "$verify_work"
mkdir -p "$verify_work"
cargo run --release --quiet --example augmentation_bench -- prepare \
  --fixture "$fixture" \
  --work "$verify_work" \
  --max-others 12 --include-embedding 1 >/dev/null
cargo run --release --quiet --example augmentation_bench -- write \
  --work "$verify_work" \
  --subset "$verify_work/subset.json" \
  --config C-pipe4 \
  --backend local \
  --phase-out "$verify_work/write.json" \
  --chunk-size 1048576 \
  --seed weft-augmentation-r3 >/dev/null
store="$verify_work/C-pipe4/store"
echo "== verify --full over the pipeline-written store $store =="
cargo run --release --quiet --bin weftfs -- --store "$store" verify --full
echo "r21_pipeline_court: verify --full ok"
