#!/usr/bin/env bash
# R18 two-stage planner court.
#
# Runs **inside** the bench court (the hard limits of `scripts/docker-court.sh
# bench` apply unchanged: 3 GiB, 3 GiB swap, 1024 pids, 4 CPUs, bounded /tmp):
#
#   scripts/docker-court.sh bench "bash bench/scripts/planner_two_stage_court.sh"
#
# Steps:
#   1. the 7-repetition interleaved matrix on the frozen 101-tensor fixture,
#      with the exhaustive planner (`C-legacy`) and the R17 ablations alongside
#      the shipped two-stage planner (`C`, `E`);
#   2. a cross-family decision diff (1 repetition each) on the real
#      Qwen2.5-0.5B base/instruct pair and the pythia-70m trajectory, so the
#      level-19 gate is checked where level 19 used to be worth encoding;
#   3. one assembled receipt with the headline numbers and every check.
#
# `WEFT_PLANNER_STAMP` pins the receipt names. `WEFT_PLANNER_MATRIX_REPS`
# overrides 7 (the checks that require seven repetitions are then omitted, and
# the receipt records the mode).
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_PLANNER_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_PLANNER_MATRIX_REPS:-7}"
work="$root/.docker-cache/planner-two-stage-$stamp"
corpus="$root/.docker-cache/real-corpus"
receipts="$root/bench/receipts"
mkdir -p "$work" "$receipts"
chmod a+rwX "$receipts" 2>/dev/null || true

for fixture in \
  "Qwen2.5-0.5B-Instruct.safetensors:fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe" \
  "Qwen2.5-0.5B.safetensors:88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342" \
  "pythia-70m-step0.safetensors:17a077abfddb6c589091e292adfcc2ee82d39bfd839f5dd80aaaea42861aaf8e" \
  "pythia-70m-step8.safetensors:17254dbf6e86d12b32a5da7b7fa5d51df3e5d3a12907b3b1ea1442ef51870e48"
do
  file="${fixture%%:*}"
  if [[ ! -f "$corpus/$file" ]]; then
    echo "planner_two_stage_court: missing corpus file $corpus/$file" >&2
    exit 1
  fi
done

echo "== matrix ($reps interleaved repetitions) =="
cargo run --release --quiet --example augmentation_bench -- ablate \
  --fixture "$corpus/Qwen2.5-0.5B-Instruct.safetensors" \
  --work "$work/matrix" \
  --out "$receipts/planner-two-stage-matrix-$stamp.json" \
  --log "$receipts/planner-two-stage-matrix-$stamp.log" \
  --configs "A,B,C,C-legacy,C-noz19,C-nom9,C-both,B-group,D,E,N" \
  --reps "$reps" --max-others 100 --include-embedding 1 \
  --backend local --chunk-size 1048576 --seed weft-augmentation-r3 \
  --official-receipt "${WEFT_PLANNER_OFFICIAL_RECEIPT:-$receipts/augmentation-local-$stamp.json}" \
  --force

run_family() {
  local name="$1" file="$2" sha="$3" embedding="$4" max_others="$5"
  echo "== cross-family decision diff: $name =="
  cargo run --release --quiet --example augmentation_bench -- ablate \
    --fixture "$corpus/$file" \
    --fixture-sha256 "$sha" \
    --work "$work/$name" \
    --out "$receipts/planner-two-stage-$name-$stamp.json" \
    --log "$receipts/planner-two-stage-$name-$stamp.log" \
    --configs "C,C-legacy" --reps 1 --max-others "$max_others" \
    --include-embedding "$embedding" \
    --backend local --chunk-size 1048576 --seed weft-augmentation-r3 \
    --diff-only --force
}

# Qwen names its embedding `model.embed_tokens.weight`; the pythia containers
# have no tensor by that name, so their 76 tensors (including the two
# 103,022,592-byte embeddings) are selected by the ordinary even-spacing rule.
run_family qwen0.5b-base Qwen2.5-0.5B.safetensors \
  88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342 1 100
run_family pythia70m-step0 pythia-70m-step0.safetensors \
  17a077abfddb6c589091e292adfcc2ee82d39bfd839f5dd80aaaea42861aaf8e 0 76
run_family pythia70m-step8 pythia-70m-step8.safetensors \
  17254dbf6e86d12b32a5da7b7fa5d51df3e5d3a12907b3b1ea1442ef51870e48 0 76

echo "== assemble =="
python3 "$root/bench/scripts/assemble_planner_two_stage.py" \
  --stamp "$stamp" \
  --matrix "$receipts/planner-two-stage-matrix-$stamp.json" \
  --family "qwen0.5b-base:$receipts/planner-two-stage-qwen0.5b-base-$stamp.json" \
  --family "pythia70m-step0:$receipts/planner-two-stage-pythia70m-step0-$stamp.json" \
  --family "pythia70m-step8:$receipts/planner-two-stage-pythia70m-step8-$stamp.json" \
  --out "$receipts/planner-two-stage-$stamp.json"

echo "planner_two_stage_court: receipt $receipts/planner-two-stage-$stamp.json"
