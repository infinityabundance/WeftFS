#!/usr/bin/env bash
# Bounded parallel mode-15 tile decode + automatic tiling court.
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/tiled_parallel_court.sh"
#
# Configurations (each a fresh store, base checkpoint only):
#   default               the shipped planner (no auto-tile)
#   auto_tile             planner with --auto-tile-bytes 65536
#   forced_tiled_control  mode 15 forced for rank-2 tensors with the same
#                         auto-derived tile shape. This is a labelled control,
#                         not one of the two required configurations: on BF16
#                         Qwen the bytes-only price never selects mode 15, so
#                         without it there is no tiled real-weight object to
#                         exercise the bounded tile decoder.
#
# Every run restores the full checkpoint at --threads 1 and --threads 4,
# verifies per-tensor BLAKE3, every logical root, the reassembled file SHA-256,
# and performs embedding region reads (whole, 100 rows, 1 row).
set -Eeuo pipefail

root=/work
work="$root/.docker-cache/tiled-parallel"
staging_mib="${WEFT_TILED_MAX_INFLIGHT_MIB:-64}"
auto_bytes="${WEFT_TILED_AUTO_BYTES:-65536}"
manifest="$root/bench/corpora/real-models/qwen2.5-0.5b-pair.json"
base_file="$root/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors"
instruct_file="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"

mkdir -p "$work"
rm -rf "$work"/store-default "$work"/store-auto "$work"/store-forced
rm -f "$work"/ingest-*.json "$work"/tiled-*.json "$work"/*.safetensors

base_sha="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["pair"]["base"]["sha256"])' "$manifest")"
instruct_sha="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["pair"]["instruct"]["sha256"])' "$manifest")"
echo "base sha256     $base_sha"
echo "instruct sha256 $instruct_sha"

source_check="absent"
if [[ -f "$base_file" && -f "$instruct_file" ]]; then
  got_base="$(sha256sum "$base_file" | cut -d' ' -f1)"
  got_instruct="$(sha256sum "$instruct_file" | cut -d' ' -f1)"
  if [[ "$got_base" != "$base_sha" || "$got_instruct" != "$instruct_sha" ]]; then
    echo "source sha256 mismatch: base=$got_base instruct=$got_instruct" >&2
    exit 1
  fi
  source_check="rehashed_in_court"
  echo "source files re-hashed and match the manifest"
else
  echo "cached corpus is missing" >&2
  exit 1
fi

echo "== build =="
cargo build --release --example real_model_bench
binary="$root/.docker-cache/target-rust/release/examples/real_model_bench"

ingest() {
  local config="$1"
  shift
  echo "== ingest $config =="
  "$binary" ingest \
    --base-file "$base_file" \
    --instruct-file "$instruct_file" \
    --skip-instruct \
    --store "$work/store-$config" \
    --out "$work/ingest-$config.json" \
    --label "tiled-parallel-$config" \
    "$@"
}

ingest default
ingest auto --auto-tile-bytes "$auto_bytes"
ingest forced --auto-tile-bytes "$auto_bytes" --force-tiled

for config in default auto forced; do
  for threads in 1 4; do
    echo "== tiled config=$config threads=$threads =="
    "$binary" tiled \
      --store "$work/store-$config" \
      --ingest-json "$work/ingest-$config.json" \
      --file "$base_file" \
      --out "$work/tiled-$config-t$threads.json" \
      --threads "$threads" \
      --max-inflight-mib "$staging_mib" \
      --config "$config" \
      --checkpoint base \
      --dir base \
      --out-file "$work/restored-$config-t$threads.safetensors" \
      --remove-materialized
    sync
  done
done

echo "== assemble receipt =="
python3 "$root/bench/scripts/assemble_tiled_parallel.py" \
  --work "$work" \
  --auto-bytes "$auto_bytes" \
  --staging-mib "$staging_mib" \
  --source-check "$source_check" \
  --base-sha "$base_sha" \
  --instruct-sha "$instruct_sha" \
  --out-dir "$root/bench/receipts"
