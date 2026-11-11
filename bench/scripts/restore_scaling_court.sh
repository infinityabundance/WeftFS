#!/usr/bin/env bash
# Restore-scaling court (subphase R13).
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/restore_scaling_court.sh"
#
# Measures the ordered checkpoint restore on real public weights at
# 1/2/4/8/16 requested workers, with the backend-lock probe on and off, and
# (optionally) against a MinIO S3 store.
#
# Every phase is a separate process so peak RSS is honest per phase. The
# receipt embeds each phase's JSON unchanged and records the court image,
# limits, corpus hashes, and repository revisions.
#
# Reduced configuration (for `verify-all.sh`):
#   WEFT_RESTORE_FAMILIES="qwen0.5b"
#   WEFT_RESTORE_THREADS="1 2 4"
#   WEFT_RESTORE_REPS=1
#   WEFT_RESTORE_MINIO=0
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$root/.docker-cache/restore-scaling-$stamp"
receipt="bench/receipts/restore-scaling-$stamp.json"
log="bench/receipts/restore-scaling-$stamp.log"

families="${WEFT_RESTORE_FAMILIES:-qwen0.5b}"
threads="${WEFT_RESTORE_THREADS:-1 2 4 8 16}"
tile_threads="${WEFT_RESTORE_TILE_THREADS:-1}"
reps="${WEFT_RESTORE_REPS:-3}"
shards="${WEFT_RESTORE_SHARDS:-8}"
minio="${WEFT_RESTORE_MINIO:-1}"
second="${WEFT_RESTORE_SECOND:-1}"
readers_on="${WEFT_RESTORE_READERS:-1}"
keep_store="${WEFT_RESTORE_KEEP_STORE:-0}"

corpus="$root/.docker-cache/real-corpus"
mkdir -p "$work" bench/receipts
exec > >(tee "$log") 2>&1

echo "restore_scaling_court: stamp=$stamp"
echo "restore_scaling_court: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "restore_scaling_court: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "restore_scaling_court: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
echo "restore_scaling_court: families=$families threads=$threads tiles=$tile_threads reps=$reps shards=$shards minio=$minio second=$second readers=$readers_on"

# Committed corpus digests (bench/corpora/real-models/*.json). A court never
# measures a file it has not identified.
sha_qwen=88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342
sha_qwen_instruct=fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe
# Qwen2.5-3B base/instruct merged containers; per-tensor identity was recorded
# by bench/scripts/safetensors_merge.py (bench/corpora/real-models/
# qwen2.5-3b-merge.json and qwen2.5-3b-instruct-merge.json) and the merged
# files were re-hashed in this court before use.
sha_qwen3b=52da3d997beeb4739e5f4f315f0d36b5a9d6f7ad038935774cac670abd1917ab
sha_qwen3b_instruct=54917aba5570e50db3f135b6e268a2b67bf951d06610713e5b6e69b8b20d91d2

verify_sha256() {
  local file="$1" expected="$2" label="$3"
  if [[ ! -f "$file" ]]; then
    echo "restore_scaling_court: missing corpus $file" >&2
    exit 1
  fi
  local actual
  actual="$(sha256sum "$file" | cut -d' ' -f1)"
  if [[ "$actual" != "$expected" ]]; then
    echo "restore_scaling_court: sha256 mismatch for $label ($file)" >&2
    echo "  expected $expected" >&2
    echo "  actual   $actual" >&2
    exit 1
  fi
  echo "restore_scaling_court: verified $label sha256=$actual"
}

echo "== build =="
cargo build --release --example family_lifecycle
bin="${CARGO_TARGET_DIR:-/work/.docker-cache/target-rust}/release/examples/family_lifecycle"

inputs=()
add_input() { inputs+=(--input "$1"); }

thread_csv="$(echo "$threads" | tr ' ' ',')"
tile_csv="$(echo "$tile_threads" | tr ' ' ',')"

for family in $families; do
  case "$family" in
    qwen0.5b)
      model="$corpus/Qwen2.5-0.5B.safetensors"
      other="$corpus/Qwen2.5-0.5B-Instruct.safetensors"
      verify_sha256 "$model" "$sha_qwen" "Qwen2.5-0.5B"
      verify_sha256 "$other" "$sha_qwen_instruct" "Qwen2.5-0.5B-Instruct"
      ;;
    qwen3b)
      model="$corpus/Qwen2.5-3B.safetensors"
      other="$corpus/Qwen2.5-3B-Instruct.safetensors"
      verify_sha256 "$model" "$sha_qwen3b" "Qwen2.5-3B"
      verify_sha256 "$other" "$sha_qwen3b_instruct" "Qwen2.5-3B-Instruct"
      ;;
    *)
      echo "restore_scaling_court: unknown family $family" >&2
      exit 64
      ;;
  esac
  family_work="$work/$family"
  store="$family_work/store"
  mkdir -p "$family_work"

  echo "== checkpoint $family (fast raw, $shards-way) =="
  "$bin" checkpoint \
    --store "$store" --file "$model" --name "$family" \
    --shards "$shards" --axis 0 --strategy tensor-sharded \
    --write-policy fast --fast-codec raw \
    --out "$family_work/checkpoint.json"
  add_input "checkpoint_${family}=$family_work/checkpoint.json"

  if [[ "$second" == "1" ]]; then
    echo "== checkpoint $family second record (distinct checkpoint over the instruct container) =="
    "$bin" checkpoint \
      --append \
      --store "$store" --file "$other" --name "$family-instruct" \
      --shards "$shards" --axis 0 --strategy tensor-sharded \
      --write-policy fast --fast-codec raw \
      --out "$family_work/checkpoint-instruct.json"
    add_input "checkpoint_${family}_instruct=$family_work/checkpoint-instruct.json"
  fi

  for spec in "0:counting" "1:counting" "1:counting-filehash" "1:sha256"; do
    probe="${spec%%:*}"
    sink="${spec##*:}"
    extra=()
    tag=""
    if [[ "$sink" == "counting-filehash" ]]; then
      sink=counting
      extra=(--file-hash)
      tag="-filehash"
    fi
    echo "== restore $family threads=$thread_csv probe=$probe sink=$sink${tag} =="
    WEFT_LOCK_PROBE="$probe" "$bin" restore \
      --store "$store" --name "$family" \
      --threads "$thread_csv" --tile-threads "$tile_csv" \
      --max-inflight-bytes 268435456 \
      --sink "$sink" "${extra[@]}" \
      --reps "$reps" \
      --out "$family_work/restore-$sink$tag-probe$probe.json"
    add_input "restore_${family}_${sink}${tag//-/_}_probe$probe=$family_work/restore-$sink$tag-probe$probe.json"
  done

  if [[ "${WEFT_RESTORE_SHARD_MATRIX:-1}" == "1" ]]; then
    echo "== restore $family threads=$thread_csv shard-threads=4 (store only) =="
    WEFT_LOCK_PROBE=1 "$bin" restore \
      --store "$store" --name "$family" \
      --threads "$thread_csv" --tile-threads "$tile_csv" \
      --max-inflight-bytes 268435456 \
      --shard-threads 4 --shard-max-inflight-bytes 268435456 \
      --sink counting --reps "$reps" \
      --out "$family_work/restore-counting-shard4-probe1.json"
    add_input "restore_${family}_counting_shard4_probe1=$family_work/restore-counting-shard4-probe1.json"
  fi

  if [[ "$readers_on" == "1" ]]; then
    echo "== independent readers $family (4 readers, 2 distinct checkpoints) =="
    WEFT_LOCK_PROBE=1 "$bin" readers \
      --store "$store" --names "$family,$family-instruct" \
      --readers 4 --per-reader 2 --reps "$reps" --sink counting \
      --out "$family_work/readers.json"
    add_input "readers_${family}=$family_work/readers.json"
  fi

  # Keep the disk bounded: only the JSON is evidence, unless a caller asked to
  # keep the store for a follow-up run.
  if [[ "$keep_store" == "1" ]]; then
    echo "restore_scaling_court: keeping store $store"
  else
    rm -rf "$store"
  fi
done

if [[ "$minio" == "1" ]]; then
  echo "== MinIO restore scaling =="
  bash "$root/bench/scripts/restore_scaling_minio_inner.sh" \
    "$stamp" "$bin" "$families" "$thread_csv" "$tile_csv" "$reps" "$shards" \
    "$work/minio"
  add_input "minio=$work/minio/minio.json"
fi

echo "== assemble =="
WEFT_COURT_COMMAND="scripts/docker-court.sh bench \"cd /work && bash bench/scripts/restore_scaling_court.sh\"" \
  "$bin" throughput \
  --label "ordered checkpoint restore scaling with the backend-lock probe (families: $families; threads: $threads; tile threads: $tile_threads; reps: $reps; shards: $shards; minio: $minio)" \
  "${inputs[@]}" \
  --out "$receipt"

echo "restore_scaling_court: receipt $receipt"
echo "restore_scaling_court: log $log"
