#!/usr/bin/env bash
# Stage 2 court: a whole training trajectory in near-final size.
#
#   scripts/docker-court.sh bench \
#     "cd /work && bash bench/scripts/trajectory_court.sh"
#
# Series:
#   * pythia-70m step0/1/2/4/8 (cached) + step16/64/512/1000 (fetched and
#     LFS-verified in a python court before this run) - real public weights;
#   * Qwen2.5-0.5B base -> Instruct - real public weights, an independent
#     fine-tune;
#   * a DECLARED SYNTHETIC 50-step series (documented deterministic rule), so
#     the curve has enough points to be meaningful. Labelled synthetic
#     everywhere.
#
# Per step: marginal/cumulative physical bytes, exact restore (file + tensor
# SHA-256), full/single/1-row restore time, deepest dependency chain, and three
# independent `zstd` baselines (full -19, XOR delta -19, --patch-from).
# Then GC retention with a snapshot pinned at every step and without snapshots.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_STAMP="$stamp"
work="$root/.docker-cache/trajectory-$stamp"
receipt="${WEFT_TRAJECTORY_RECEIPT:-bench/receipts/trajectory-$stamp.json}"
log="bench/receipts/trajectory-$stamp.log"
corpus="$root/.docker-cache/real-corpus"
synth_steps="${WEFT_TRAJ_SYNTH_STEPS:-50}"
mkdir -p "$work/phases" bench/receipts
exec > >(tee "$log") 2>&1

echo "trajectory: stamp=$stamp"
echo "trajectory: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "trajectory: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "trajectory: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"

echo "trajectory: building release harness"
cargo build --release --example trajectory_court >/dev/null 2>&1
bin="$CARGO_TARGET_DIR/release/examples/trajectory_court"

# --- corpus ----------------------------------------------------------------
pythia_steps="${WEFT_TRAJ_PYTHIA_STEPS:-0 1 2 4 8 16 64 512 1000}"
# The Qwen half re-reads 988 MB per step; the aggregate verification court sets
# this to 0 and says so in its receipt.
run_qwen="${WEFT_TRAJ_QWEN:-1}"
pythia_arg=""
{
  printf '{"phase":"corpus","pythia":{'
  first=1
  for step in $pythia_steps; do
    file="$corpus/pythia-70m-step$step.safetensors"
    if [[ ! -f "$file" ]]; then
      echo "trajectory: missing $file" >&2
      continue
    fi
    sha="$(sha256sum "$file" | awk '{print $1}')"
    [[ $first -eq 0 ]] && printf ','
    first=0
    printf '"step%s":{"path":"%s","sha256":"%s","bytes":%s}' "$step" "$file" "$sha" "$(stat -c %s "$file")"
    pythia_arg+="pythia-step$step=$file,"
  done
  printf '},"qwen":{"base":{"path":"%s","sha256":"%s"},"instruct":{"path":"%s","sha256":"%s"}}' \
    "$corpus/Qwen2.5-0.5B.safetensors" "$(sha256sum "$corpus/Qwen2.5-0.5B.safetensors" | awk '{print $1}')" \
    "$corpus/Qwen2.5-0.5B-Instruct.safetensors" "$(sha256sum "$corpus/Qwen2.5-0.5B-Instruct.safetensors" | awk '{print $1}')"
  printf ',"fetch":{"denser_pythia":"step16,step64,step512,step1000 fetched and LFS-SHA-256 verified in the python court before this run","source":"https://huggingface.co/EleutherAI/pythia-70m"}}\n'
} >"$work/phases/corpus.json"

pythia_arg="${pythia_arg%,}"

# Record the configuration in the receipt: the aggregate verification court runs
# a reduced trajectory, and that must be visible rather than inferred.
printf '{"phase":"configuration","synthetic_steps":%s,"pythia_steps":"%s","qwen":%s,"snapshots":true,"note":"a reduced run is labelled here; the standalone full-size receipt is the full evidence"}\n' \
  "$(printf '%s' "$synth_steps")" "$pythia_steps" "$([ "$run_qwen" = "1" ] && echo true || echo false)" >"$work/phases/configuration.json"

# --- synthetic 50-step series ----------------------------------------------
echo "trajectory: generating a declared synthetic ${synth_steps}-step series"
"$bin" gen --dir "$work/synthetic" --steps "$synth_steps" --tensors 64 --rows 256 --cols 256 \
  >"$work/phases/synthetic-gen.txt"
synth_arg=""
for file in "$work"/synthetic/step*.safetensors; do
  name="$(basename "$file" .safetensors)"
  synth_arg+="synthetic-$name=$file,"
done
synth_arg="${synth_arg%,}"
synth_last="synthetic-step$(printf '%03d' "$synth_steps")"

echo "trajectory: synthetic series (snapshot pinned at every step)"
"$bin" series --store "$work/store-synth" --out "$work/phases/synthetic-series.json" \
  --steps "$synth_arg" --strategy planned --shards 1 --snapshots --range "0:1,0:64"
echo "trajectory: synthetic GC with snapshots pinned"
"$bin" gc --store "$work/store-synth" --out "$work/phases/synthetic-gc-pinned.json" \
  --keep "$synth_last"

echo "trajectory: synthetic series (no snapshots) for the GC-reclaim phase"
"$bin" series --store "$work/store-synth-nosnap" --out "$work/phases/synthetic-series-nosnap.json" \
  --steps "$synth_arg" --strategy planned --shards 1 --range "0:1,0:64"
echo "trajectory: synthetic GC without snapshots"
"$bin" gc --store "$work/store-synth-nosnap" --out "$work/phases/synthetic-gc-reclaim.json" \
  --keep "$synth_last"

# --- real public weights ---------------------------------------------------
echo "trajectory: pythia-70m consecutive steps"
"$bin" series --store "$work/store-pythia" --out "$work/phases/pythia-series.json" \
  --steps "$pythia_arg" --strategy planned --shards 1 --snapshots --no-semantic --range "0:1,0:256"

if [[ "$run_qwen" == "1" ]]; then
echo "trajectory: qwen base -> instruct"
"$bin" series --store "$work/store-qwen" --out "$work/phases/qwen-series.json" \
  --steps "qwen-base=$corpus/Qwen2.5-0.5B.safetensors,qwen-instruct=$corpus/Qwen2.5-0.5B-Instruct.safetensors" \
  --strategy planned --shards 1 --snapshots --no-semantic --range "0:1,0:896"
else
  echo "trajectory: qwen half skipped (WEFT_TRAJ_QWEN=0)"
fi

"$bin" assemble --work "$work/phases" --out "$receipt"
echo "trajectory: receipt=$receipt"
du -sh "$work" || true
