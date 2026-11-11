#!/usr/bin/env bash
# MinIO-backed restore-scaling run (subphase R13).
#
# Called by `restore_scaling_court.sh`, inside the bench court, with the usual
# hard limits. Starts the official MinIO binary on loopback, builds one
# checkpoint per family directly in an S3-backed store, then runs the same
# restore matrix with the backend-lock probe on. Assembly is delegated to
# `family_lifecycle throughput`, so this receipt is a nested, unchanged copy of
# every phase JSON it read.
set -Eeuo pipefail

stamp="$1"
bin="$2"
families="$3"
threads="$4"
tile_threads="$5"
reps="$6"
shards="$7"
work="$8"

minio_bin=/work/.docker-cache/minio/minio
corpus=/work/.docker-cache/real-corpus

export WEFT_S3_ENDPOINT=http://127.0.0.1:9000
export WEFT_S3_REGION=us-east-1
export WEFT_S3_ACCESS_KEY=weftrestore
export WEFT_S3_SECRET_KEY=weftrestore-secret
export MINIO_ROOT_USER="$WEFT_S3_ACCESS_KEY"
export MINIO_ROOT_PASSWORD="$WEFT_S3_SECRET_KEY"

mkdir -p "$work/minio-data"
chmod a+rwX "$work" "$work/minio-data" 2>/dev/null || true

echo "restore_scaling_minio: starting MinIO"
"$minio_bin" server "$work/minio-data" --address 127.0.0.1:9000 \
  --console-address 127.0.0.1:9001 >"$work/minio.log" 2>&1 &
minio_pid=$!
cleanup() { kill "$minio_pid" 2>/dev/null || true; }
trap cleanup EXIT

ready=0
for _ in $(seq 1 600); do
  if (exec 3<>/dev/tcp/127.0.0.1/9000) 2>/dev/null; then ready=1; break; fi
  if ! kill -0 "$minio_pid" 2>/dev/null; then break; fi
  sleep 0.1
done
if [[ "$ready" != 1 ]]; then
  echo "restore_scaling_minio: MinIO did not become ready" >&2
  tail -5 "$work/minio.log" >&2 || true
  exit 1
fi
echo "restore_scaling_minio: MinIO up pid=$minio_pid"

inputs=()
add_input() { inputs+=(--input "$1"); }
thread_csv="$(echo "$threads" | tr ' ' ',')"
tile_csv="$(echo "$tile_threads" | tr ' ' ',')"

for family in $families; do
  case "$family" in
    qwen0.5b)
      model="$corpus/Qwen2.5-0.5B.safetensors"
      expected=88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342
      ;;
    qwen3b)
      model="$corpus/Qwen2.5-3B.safetensors"
      expected=52da3d997beeb4739e5f4f315f0d36b5a9d6f7ad038935774cac670abd1917ab
      ;;
    *)
      echo "restore_scaling_minio: unknown family $family" >&2
      exit 64
      ;;
  esac
  actual="$(sha256sum "$model" | awk '{print $1}')"
  if [[ "$actual" != "$expected" ]]; then
    echo "restore_scaling_minio: sha256 mismatch for $model ($actual != $expected)" >&2
    exit 1
  fi

  family_work="$work/$family"
  mkdir -p "$family_work"
  # One bucket per family; the harness appends `-<storedirname>-kept`. S3 bucket
  # names are DNS-style (lowercase, no dots), so sanitise the stamp and family.
  bucket_suffix="$(printf '%s-%s' "$stamp" "$family" | tr 'A-Z' 'a-z' | tr -cd 'a-z0-9-')"
  export WEFT_S3_BUCKET="weft-rs-$bucket_suffix"
  store="$family_work/store"

  echo "== minio checkpoint $family =="
  "$bin" checkpoint \
    --backend s3 --store "$store" --file "$model" --name "$family" \
    --shards "$shards" --axis 0 --strategy tensor-sharded \
    --write-policy fast --fast-codec raw \
    --out "$family_work/checkpoint.json"
  add_input "checkpoint_${family}=$family_work/checkpoint.json"

  for combo in "0:counting" "1:counting"; do
    probe="${combo%%:*}"
    sink="${combo##*:}"
    echo "== minio restore $family threads=$thread_csv probe=$probe sink=$sink =="
    WEFT_LOCK_PROBE="$probe" "$bin" restore \
      --backend s3 --store "$store" --name "$family" \
      --threads "$thread_csv" --tile-threads "$tile_csv" \
      --max-inflight-bytes 268435456 \
      --sink "$sink" \
      --reps "$reps" \
      --out "$family_work/restore-$sink-probe$probe.json"
    add_input "restore_${family}_${sink}_probe$probe=$family_work/restore-$sink-probe$probe.json"
  done
  echo "== minio restore $family exactness (sha256 sink) =="
  WEFT_LOCK_PROBE=0 "$bin" restore \
    --backend s3 --store "$store" --name "$family" \
    --threads "$thread_csv" --tile-threads "$tile_csv" \
    --max-inflight-bytes 268435456 \
    --sink sha256 --reps 1 \
    --out "$family_work/restore-sha256.json"
  add_input "restore_${family}_sha256=$family_work/restore-sha256.json"

  echo "== minio independent readers $family =="
  WEFT_LOCK_PROBE=1 "$bin" readers \
    --backend s3 --store "$store" --names "$family" \
    --readers 4 --per-reader 2 --reps "$reps" --sink counting \
    --out "$family_work/readers.json"
  add_input "readers_${family}=$family_work/readers.json"
done

"$bin" throughput \
  --label "MinIO (loopback, official binary) restore scaling with the backend-lock probe" \
  "${inputs[@]}" \
  --out "$work/minio.json"
echo "restore_scaling_minio: receipt $work/minio.json"
