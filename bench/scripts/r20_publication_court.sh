#!/usr/bin/env bash
# R20 publication-tax court.
#
# Runs **inside** the bench court (`scripts/docker-court.sh bench "bash
# bench/scripts/r20_publication_court.sh"`), so the hard limits are the usual
# ones: 3 GiB memory == swap, 1024 pids, 4 CPUs, bounded /tmp.
#
# The contract court is run separately on the host with the **same stamp**
# (`WEFT_AUGMENTATION_STAMP=<stamp> scripts/augmentation_court.sh --local`),
# exactly as the R19 wall court does; nesting `docker-court.sh` is impossible
# inside the court. When that receipt exists it is passed as
# `--official-receipt`, which supplies the same-revision identity proof that
# the default C row did not move.
#
# Rows:
#   A               native per-tensor, no WeftFS code in the data path
#   B               raw representation, one atomic publication
#   B-percommit     raw representation, one commit and barrier per tensor (R17)
#   C               shipped planner, one atomic publication          <- default
#   C-percommit     shipped planner, one commit and barrier per tensor (R19)
#   C-group         shipped planner, one group commit, eager blob barriers
#   C-pack64k       shipped planner, packed blob plane at a 64 KiB member bound
#   C-pack1m        shipped planner, packed blob plane at a 1 MiB member bound
#   C-pack8m        shipped planner, packed blob plane at an 8 MiB member bound
#   C-legacy        exhaustive planner, one atomic publication (decision diff)
#   D               raw representation + context/provenance protocol
#   E               planned representation + context/provenance protocol
#   C-noz19         R17 attribution row (exhaustive planner, level-19 off)
#   C-nom9          R17 attribution row (exhaustive planner, mode 9 off)
#   C-both          R17 attribution row (both candidates off)
#
# `WEFT_R20_STAMP` pins the receipt names, `WEFT_R20_REPS` defaults to 7.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_R20_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_R20_REPS:-7}"
configs="${WEFT_R20_CONFIGS:-A,B,B-percommit,C,C-percommit,C-group,C-pack64k,C-pack1m,C-pack8m,C-legacy,D,E,C-noz19,C-nom9,C-both}"
fixture="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
receipt="$root/bench/receipts/r20-publication-$stamp.json"
log="$root/bench/receipts/r20-publication-$stamp.log"
work="$root/.docker-cache/r20-publication-$stamp"
official="$root/bench/receipts/augmentation-local-$stamp.json"

if [[ ! -f "$fixture" ]]; then
  echo "r20_publication_court: missing fixture $fixture" >&2
  exit 1
fi
mkdir -p "$root/bench/receipts"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true

echo "== R20 publication matrix, configs=$configs, reps=$reps =="
official_args=()
if [[ -f "$official" ]]; then
  official_args=(--official-receipt "$official")
else
  echo "r20_publication_court: warning: no contract-court receipt at $official; the identity check will be absent" >&2
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

echo "r20_publication_court: receipt $receipt"
