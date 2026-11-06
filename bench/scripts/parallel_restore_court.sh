#!/usr/bin/env bash
# Parallel tensor reconstruction court (R3).
#
# Runs inside the bench court with the usual hard limits:
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/parallel_restore_court.sh"
#
# What it does:
#   1. Reuses `/work/.docker-cache/planner-float/{store,ingest.json}` when both
#      exist (the R2 planner/store court output). When either is missing it
#      runs `bench/scripts/planner_float_court.sh` first and says so in the
#      receipt. It never re-ingests when the cached store is present.
#   2. Runs `real_model_bench restore --threads 1|2|4`, three repetitions each,
#      writing one JSON per run. Repetitions are interleaved across thread
#      counts so host drift is spread over the configurations instead of
#      landing on one of them.
#   3. Verifies every run: `hash_exact`, `logical_roots_exact` for every
#      checkpoint, `materialize.exact`, and the SHA-256 of the reassembled
#      `.safetensors` file against the source SHA-256 (the corpus manifest, and
#      the source file itself when it is still cached).
#   4. Assembles `bench/receipts/parallel-restore-<UTC ISO>.json` from the run
#      JSONs: court environment, four repo revisions, per-thread medians for
#      read and materialize, backend bytes read, peak RSS, effective worker
#      counts, and a `checks` object.
set -Eeuo pipefail

root=/work
staging_mib="${WEFT_PARALLEL_MAX_INFLIGHT_MIB:-64}"
reps="${WEFT_PARALLEL_REPS:-3}"
work="$root/.docker-cache/parallel-restore"
planner_work="$root/.docker-cache/planner-float"
store="$planner_work/store"
ingest="$planner_work/ingest.json"
manifest="$root/bench/corpora/real-models/qwen2.5-0.5b-pair.json"

mkdir -p "$work"
rm -f "$work"/restore-t*.json "$work"/sha256.txt

ingest_source="reused-cached-planner-float"
if [[ ! -d "$store" || ! -f "$ingest" ]]; then
  echo "== cached planner-float store absent: running the ingest court =="
  bash "$root/bench/scripts/planner_float_court.sh"
  ingest_source="ran-planner_float_court_ingest"
fi
if [[ ! -d "$store" || ! -f "$ingest" ]]; then
  echo "parallel_restore_court: no store/ingest receipt after the ingest step" >&2
  exit 1
fi

echo "== build =="
cargo build --release --example real_model_bench

binary="$root/.docker-cache/target-rust/release/examples/real_model_bench"
restore_dir="$work/restore"
mkdir -p "$restore_dir"

# Source SHA-256, straight from the corpus manifest (the values the ingest
# court verified against the Hugging Face LFS pointers).
base_sha="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["pair"]["base"]["sha256"])' "$manifest")"
instruct_sha="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["pair"]["instruct"]["sha256"])' "$manifest")"
echo "base sha256     $base_sha"
echo "instruct sha256 $instruct_sha"

# Re-hash the cached source files when they are present. This is the same
# value the ingest step checks, recomputed here rather than trusted.
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

for rep in $(seq 1 "$reps"); do
  for threads in 1 2 4; do
    out="$work/restore-t$threads-r$rep.json"
    echo "== restore threads=$threads rep=$rep =="
    "$binary" restore \
      --store "$store" \
      --ingest-json "$ingest" \
      --out "$out" \
      --threads "$threads" \
      --max-inflight-mib "$staging_mib" \
      --single-runs 1 \
      --reassemble-dir "$restore_dir"
    for cp in base instruct; do
      sha="$(sha256sum "$restore_dir/$cp.safetensors" | cut -d' ' -f1)"
      echo "threads=$threads rep=$rep $cp sha256=$sha" | tee -a "$work/sha256.txt"
    done
    # Flush the ~2 GB just written before the next repetition starts. Without
    # this, one run's writeback lands inside the next run's materialize timer
    # and the single-thread materialize numbers swing by 50%.
    sync
  done
done

echo "== assemble receipt =="
python3 "$root/bench/scripts/assemble_parallel_restore.py" \
  --work "$work" \
  --reps "$reps" \
  --staging-mib "$staging_mib" \
  --ingest-source "$ingest_source" \
  --store "$store" \
  --source-check "$source_check" \
  --base-sha "$base_sha" \
  --instruct-sha "$instruct_sha" \
  --out-dir "$root/bench/receipts"
