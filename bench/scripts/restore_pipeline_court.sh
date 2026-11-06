#!/usr/bin/env bash
# End-to-end restore court: batched materialize vs the ordered restore pipeline.
#
# Runs inside the bench court with the usual hard limits:
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/restore_pipeline_court.sh"
#
# Two destinations, because the host bind mount's writeback is far noisier than
# the decoder:
#
#   disk   both checkpoints, output under the host-backed cache directory. This
#          is the real end-to-end path and the honest comparison to earlier
#          receipts; its write times are host-state dependent.
#   tmpfs  base checkpoint only, output on the court's 1 GiB /mnt/weft-bench
#          tmpfs. With the write effectively free this isolates decode/transfer
#          overlap, and 988,097,824 B fits under the 1 GiB mount.
#
# Steps:
#   1. Reuses `/work/.docker-cache/planner-float/{store,ingest.json}` when both
#      exist. When either is missing it runs
#      `bench/scripts/planner_float_court.sh` first and says so in the receipt.
#      It never re-ingests when the cached store is present.
#   2. Re-hashes both cached source files against the corpus manifest.
#   3. Runs `real_model_bench restore` with the shipped batched materialize and
#      with `--materialize-pipeline`, at 1/2/4 threads, interleaved per
#      destination and repetition.
#   4. Verifies every run: per-tensor BLAKE3, every logical root, exact
#      reassembled file, and the SHA-256 of each restored safetensors file
#      against the corpus manifest.
#   5. Assembles `bench/receipts/restore-pipeline-<UTC>.json`.
set -Eeuo pipefail

root=/work
staging_mib="${WEFT_RESTORE_PIPELINE_MIB:-64}"
reps="${WEFT_RESTORE_PIPELINE_REPS:-3}"
work="$root/.docker-cache/restore-pipeline"
planner_work="$root/.docker-cache/planner-float"
store="$planner_work/store"
ingest="$planner_work/ingest.json"
manifest="$root/bench/corpora/real-models/qwen2.5-0.5b-pair.json"
tmpfs_root=/mnt/weft-bench/restore-pipeline

mkdir -p "$work"
rm -rf "$work"/run-*.json "$work"/out-* "$work"/sha256.txt "$tmpfs_root"
mkdir -p "$tmpfs_root"

ingest_source="reused-cached-planner-float"
if [[ ! -d "$store" || ! -f "$ingest" ]]; then
  echo "== cached planner-float store absent: running the planner court =="
  bash "$root/bench/scripts/planner_float_court.sh"
  ingest_source="ran-planner_float_court_ingest"
fi
if [[ ! -d "$store" || ! -f "$ingest" ]]; then
  echo "restore_pipeline_court: no store/ingest receipt after the ingest step" >&2
  exit 1
fi

echo "== build =="
cargo build --release --example real_model_bench
binary="$root/.docker-cache/target-rust/release/examples/real_model_bench"

base_sha="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["pair"]["base"]["sha256"])' "$manifest")"
instruct_sha="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["pair"]["instruct"]["sha256"])' "$manifest")"
echo "base sha256     $base_sha"
echo "instruct sha256 $instruct_sha"

source_check="absent"
if [[ -f "$root/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors" \
   && -f "$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors" ]]; then
  got_base="$(sha256sum "$root/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors" | cut -d' ' -f1)"
  got_instruct="$(sha256sum "$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors" | cut -d' ' -f1)"
  if [[ "$got_base" != "$base_sha" || "$got_instruct" != "$instruct_sha" ]]; then
    echo "source sha256 mismatch: base=$got_base instruct=$got_instruct" >&2
    exit 1
  fi
  source_check="rehashed_in_court"
  echo "source files re-hashed and match the manifest"
fi

run_destination() {
  local dest="$1" out_base="$2" reps="$3"
  shift 3
  local extra=("$@")
  mkdir -p "$out_base"
  for rep in $(seq 1 "$reps"); do
    for threads in 1 2 4; do
      for mode in batch pipeline; do
        local out="$work/run-$dest-$mode-t$threads-r$rep.json"
        local out_dir="$out_base/$mode-t$threads-r$rep"
        local args=(
          restore
          --store "$store"
          --ingest-json "$ingest"
          --out "$out"
          --threads "$threads"
          --max-inflight-mib "$staging_mib"
          --single-runs 1
          --reassemble-dir "$out_dir"
          "${extra[@]}"
        )
        mkdir -p "$out_dir"
        echo "== restore dest=$dest mode=$mode threads=$threads rep=$rep =="
        if [[ "$mode" == "pipeline" ]]; then
          args+=(--materialize-pipeline)
        fi
        "$binary" "${args[@]}"
        mapfile -t cps < <(python3 -c \
          'import json,sys;print("\n".join(c["checkpoint"] for c in json.load(open(sys.argv[1]))["checkpoints"]))' \
          "$out")
        for cp in "${cps[@]}"; do
          sha="$(sha256sum "$out_dir/$cp.safetensors" | cut -d' ' -f1)"
          echo "dest=$dest mode=$mode threads=$threads rep=$rep $cp sha256=$sha" \
            | tee -a "$work/sha256.txt"
        done
        # Remove the restored file before the next timed run. The 2 GB of dirty
        # output per run otherwise accumulates across the court and turns every
        # later write into a host-writeback measurement.
        rm -rf "$out_dir"
        sync
      done
    done
  done
}

run_destination disk "$work/out-disk" "$reps"
run_destination tmpfs "$tmpfs_root" "$reps" --checkpoint base

echo "== assemble receipt =="
python3 "$root/bench/scripts/assemble_restore_pipeline.py" \
  --work "$work" \
  --reps "$reps" \
  --staging-mib "$staging_mib" \
  --ingest-source "$ingest_source" \
  --store "$store" \
  --source-check "$source_check" \
  --base-sha "$base_sha" \
  --instruct-sha "$instruct_sha" \
  --out-dir "$root/bench/receipts"
