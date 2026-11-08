#!/usr/bin/env bash
# Checkpoint-family lifecycle court.
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/checkpoint_family_court.sh"
#
# ONE court that measures the whole artifact lifecycle on real public weights:
# ingest, retention, snapshot, restore (five shapes x 1/4 threads), cross-topology
# restore, process-fault recovery, and GC with and without a pinned snapshot.
# It compares the planner's bytes-only default against an access-pattern plan for
# 1-row and 100-row reads, against independent per-tensor `zstd -19`, and against
# `zstd --patch-from`. An optional reduced MinIO configuration measures the same
# phases over the object-store path.
#
# Consecutive training checkpoints (pythia step0 -> step1) and the independent
# fine-tune (qwen base -> Instruct) are separate families and never aggregated.
#
# Every phase runs as a separate process so peak RSS is honest per phase. The
# court writes one receipt plus a raw log beside it.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_STAMP="$stamp"
work="$root/.docker-cache/family-lifecycle-$stamp"
receipt="bench/receipts/family-lifecycle-$stamp.json"
log="bench/receipts/family-lifecycle-$stamp.log"
inputs="$work/inputs.txt"
reps="${WEFT_FAMILY_INGEST_REPS:-3}"
measure_reps="${WEFT_FAMILY_MEASURE_REPS:-3}"
topology_reps="${WEFT_FAMILY_TOPOLOGY_REPS:-3}"
recovery_reps="${WEFT_FAMILY_RECOVERY_REPS:-3}"
configs="${WEFT_FAMILY_CONFIGS-default region1 region100}"
families="${WEFT_FAMILY_FAMILIES-pythia-consecutive qwen-finetune}"
run_s3="${WEFT_FAMILY_S3:-1}"
gc_limit="${WEFT_FAMILY_GC_LIMIT:-8}"
tile_budget="${WEFT_FAMILY_TILE_BYTES:-1048576}"

corpus="$root/.docker-cache/real-corpus"
qwen_base="$corpus/Qwen2.5-0.5B.safetensors"
qwen_instruct="$corpus/Qwen2.5-0.5B-Instruct.safetensors"
pythia0="$corpus/pythia-70m-step0.safetensors"
pythia1="$corpus/pythia-70m-step1.safetensors"
# Large-model family (Stage 1 of the v1.2.0 work). Both files are merged
# single-container views of the two published shards each; the merge tool
# verified every tensor byte-for-byte. Override only to point at a differently
# rooted copy of the same verified files.
qwen3b_base="${WEFT_FAMILY_QWEN3B_BASE:-$corpus/Qwen2.5-3B.safetensors}"
qwen3b_instruct="${WEFT_FAMILY_QWEN3B_NEXT:-$corpus/Qwen2.5-3B-Instruct.safetensors}"

mkdir -p "$work" bench/receipts
: > "$inputs"
exec > >(tee "$log") 2>&1

echo "family_lifecycle: stamp=$stamp"
echo "family_lifecycle: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "family_lifecycle: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "family_lifecycle: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftfs-go=${WEFT_REPO_WEFTFS_GO:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown} weftcodec-go=${WEFT_REPO_WEFTCODEC_GO:-unknown}"
echo "family_lifecycle: reps ingest=$reps measure=$measure_reps topology=$topology_reps recovery=$recovery_reps configs=$configs"

# --- corpus verification ---------------------------------------------------
verify_sha256() {
  local file="$1" expected="$2"
  if [[ ! -f "$file" ]]; then
    echo "family_lifecycle: missing corpus $file" >&2
    exit 1
  fi
  local actual
  actual="$(sha256sum "$file" | cut -d' ' -f1)"
  if [[ "$actual" != "$expected" ]]; then
    echo "family_lifecycle: sha256 mismatch for $file" >&2
    echo "  expected $expected" >&2
    echo "  actual   $actual" >&2
    exit 1
  fi
  echo "family_lifecycle: verified $(basename "$file") sha256=$actual"
}

qwen_base_sha=88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342
qwen_instruct_sha=fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe
pythia0_sha=17a077abfddb6c589091e292adfcc2ee82d39bfd839f5dd80aaaea42861aaf8e
pythia1_sha=652e38fd15e118cc7c43d0aeb5e30675c7180cf1d8782cb401ba5d0f933fa109
# Merged-container sha256 (shard hashes are in the pair manifest and are
# verified by the merge tool at creation time; the court re-verifies the
# merged file it actually reads).
qwen3b_base_sha=52da3d997beeb4739e5f4f315f0d36b5a9d6f7ad038935774cac670abd1917ab
qwen3b_instruct_sha=54917aba5570e50db3f135b6e268a2b67bf951d06610713e5b6e69b8b20d91d2

# The values above are copies of the manifest entries; refuse to run if the
# committed manifests disagree (the manifest is the source of truth).
for pair in \
  "bench/corpora/real-models/float-tensor-real.json:$qwen_base_sha" \
  "bench/corpora/real-models/float-tensor-real.json:$qwen_instruct_sha" \
  "bench/corpora/real-models/float-tensor-real.json:$pythia0_sha" \
  "bench/corpora/real-models/float-tensor-real.json:$pythia1_sha" \
  "bench/corpora/real-models/qwen2.5-0.5b-pair.json:$qwen_base_sha" \
  "bench/corpora/real-models/qwen2.5-0.5b-pair.json:$qwen_instruct_sha"; do
  manifest="${pair%%:*}"
  digest="${pair##*:}"
  if ! grep -q "$digest" "$manifest"; then
    echo "family_lifecycle: manifest $manifest does not contain $digest" >&2
    exit 1
  fi
done
verify_sha256 "$qwen_base" "$qwen_base_sha"
verify_sha256 "$qwen_instruct" "$qwen_instruct_sha"
verify_sha256 "$pythia0" "$pythia0_sha"
verify_sha256 "$pythia1" "$pythia1_sha"

# The 3B pair is only verified when that family is selected: it is 12.3 GiB of
# source weights and must not be hashed by the small-family runs.
if [[ " $families " == *" qwen3b-finetune "* ]]; then
  for pair in \
    "bench/corpora/real-models/qwen2.5-3b-pair.json:$qwen3b_base_sha" \
    "bench/corpora/real-models/qwen2.5-3b-pair.json:$qwen3b_instruct_sha"; do
    manifest="${pair%%:*}"
    digest="${pair##*:}"
    if ! grep -q "$digest" "$manifest"; then
      echo "family_lifecycle: manifest $manifest does not contain $digest" >&2
      exit 1
    fi
  done
  verify_sha256 "$qwen3b_base" "$qwen3b_base_sha"
  verify_sha256 "$qwen3b_instruct" "$qwen3b_instruct_sha"
fi

echo "== build =="
cargo build --release --example family_lifecycle
bin="${CARGO_TARGET_DIR:-/work/.docker-cache/target-rust}/release/examples/family_lifecycle"
if [[ ! -x "$bin" ]]; then
  echo "family_lifecycle: missing binary $bin" >&2
  exit 1
fi

baselines_for() {
  local family="$1" base="$2" next="$3"
  local dir="$work/$family"
  mkdir -p "$dir/baseline-scratch"
  echo "== baselines $family =="
  "$bin" baselines \
    --base-file "$base" --next-file "$next" \
    --scratch "$dir/baseline-scratch" \
    --reps "$measure_reps" \
    --family "$family" \
    --out "$dir/baselines.json"
  rm -rf "$dir/baseline-scratch"
}

checkpoint_for() {
  local family="$1" file="$2"
  local dir="$work/$family"
  echo "== checkpoint 8-way $family =="
  "$bin" checkpoint \
    --store "$dir/checkpoint-store" --file "$file" --name "$family" \
    --shards 8 --axis 0 --strategy tensor-sharded \
    --out "$dir/checkpoint.json"
  echo "== cross-topology $family =="
  "$bin" topology \
    --store "$dir/checkpoint-store" --name "$family" --reps "$topology_reps" \
    --out "$dir/topology.json"
}

recovery_for() {
  local family="$1" file="$2" filter="$3"
  local dir="$work/$family"
  echo "== process-fault recovery $family ($filter) =="
  "$bin" recovery \
    --work "$dir/recovery" --file "$file" --name "$family" \
    --tensor-filter "$filter" \
    --reps "$recovery_reps" \
    --out "$dir/recovery.json"
  rm -rf "$dir/recovery"
}

# One configuration: full ingest + measure + the GC pair, then assemble.
run_config() {
  local family="$1" variant="$2" base="$3" next="$4" pattern="$5" lambda="$6"
  local backend="${7:-local}" s3_prefix="${8:-}" extra="${9:-}"
  local extra_args=()
  if [[ -n "$extra" ]]; then
    read -r -a extra_args <<<"$extra"
  fi
  local dir="$work/$family"
  local store="$dir/$variant/store"
  local kept_store="$store"
  mkdir -p "$dir"

  echo "== ingest $family/$variant pattern=$pattern lambda=$lambda backend=$backend =="
  local backend_args=()
  if [[ "$backend" != "local" ]]; then
    backend_args=(--backend "$backend")
  fi
  "$bin" ingest \
    --store "$store" --base-file "$base" --next-file "$next" \
    --family "$family" --variant "$variant" \
    --access-pattern "$pattern" --lambda-ns "$lambda" \
    --reps "$reps" "${extra_args[@]}" "${backend_args[@]}" \
    --out "$dir/$variant/ingest.json"

  echo "== measure $family/$variant =="
  "$bin" measure \
    --store "$kept_store" --ingest "$dir/$variant/ingest.json" \
    --reps "$measure_reps" --threads 1,4 "${backend_args[@]}" \
    --out "$dir/$variant/measure.json"

  # The GC pair uses a small real-weight subset so both stores can exist at
  # once. GC mechanics do not depend on corpus size; both runs state the limit.
  local gc_dir="$dir/$variant/gc"
  local gc_plain_store="$gc_dir/store-plain"
  local gc_pinned_store="$gc_dir/store-pinned"
  mkdir -p "$gc_dir"
  echo "== gc fixture $family/$variant (limit=$gc_limit) =="
  "$bin" ingest \
    --store "$gc_dir/store-plain" --base-file "$base" --next-file "$next" \
    --family "$family" --variant "gc-plain-$variant" \
    --access-pattern "$pattern" --lambda-ns "$lambda" \
    --limit "$gc_limit" --reps 1 --no-snapshot --only-changed --strategy zstd "${backend_args[@]}" \
    --out "$gc_dir/ingest-plain.json"
  "$bin" gc \
    --store "$gc_plain_store" --ingest "$gc_dir/ingest-plain.json" \
    "${backend_args[@]}" --out "$gc_dir/gc-plain.json"

  "$bin" ingest \
    --store "$gc_dir/store-pinned" --base-file "$base" --next-file "$next" \
    --family "$family" --variant "gc-pinned-$variant" \
    --access-pattern "$pattern" --lambda-ns "$lambda" \
    --limit "$gc_limit" --reps 1 --no-snapshot --only-changed --strategy zstd "${backend_args[@]}" \
    --out "$gc_dir/ingest-pinned.json"
  "$bin" gc \
    --store "$gc_pinned_store" --ingest "$gc_dir/ingest-pinned.json" \
    --snapshot "${backend_args[@]}" --out "$gc_dir/gc-pinned.json"

  local label="$family/$variant"
  local out="$dir/$variant/family-lifecycle.json"
  echo "== assemble $label =="
  "$bin" assemble \
    --family "$family" --variant "$variant" \
    --ingest "$dir/$variant/ingest.json" \
    --measure "$dir/$variant/measure.json" \
    --baselines "$dir/baselines.json" \
    --topology "$dir/topology.json" \
    --recovery "$dir/recovery.json" \
    --gc-plain "$gc_dir/gc-plain.json" \
    --gc-pinned "$gc_dir/gc-pinned.json" \
    --checkpoint "$dir/checkpoint.json" \
    --out "$out"
  echo "$out" >> "$inputs"

  # Keep the disk bounded: the retained store is only needed by measure.
  if [[ "$backend" == "local" ]]; then
    rm -rf "$store"
  fi
  rm -rf "$gc_dir"
}

# --- consecutive training checkpoints -------------------------------------
if [[ " $families " == *" pythia-consecutive "* ]]; then
family=pythia-consecutive
if [[ "${WEFT_FAMILY_SKIP_FAMILY:-0}" != "1" ]]; then
baselines_for "$family" "$pythia0" "$pythia1"
checkpoint_for "$family" "$pythia0"
recovery_for "$family" "$pythia0" '*.layers.0.*'
fi
for variant in $configs; do
  case "$variant" in
    default) run_config "$family" "$variant" "$pythia0" "$pythia1" whole 0 ;;
    region1) run_config "$family" "$variant" "$pythia0" "$pythia1" rows:1 1 local "" "--auto-tile-bytes $tile_budget" ;;
    region100) run_config "$family" "$variant" "$pythia0" "$pythia1" rows:100 1 local "" "--auto-tile-bytes $tile_budget" ;;
    *) echo "family_lifecycle: unknown configuration $variant" >&2; exit 64 ;;
  esac
done
fi

# --- independent fine-tune -------------------------------------------------
if [[ " $families " == *" qwen-finetune "* ]]; then
family=qwen-finetune
if [[ "${WEFT_FAMILY_SKIP_FAMILY:-0}" != "1" ]]; then
baselines_for "$family" "$qwen_base" "$qwen_instruct"
checkpoint_for "$family" "$qwen_base"
recovery_for "$family" "$qwen_base" '*.layers.0.*'
fi
for variant in $configs; do
  case "$variant" in
    default) run_config "$family" "$variant" "$qwen_base" "$qwen_instruct" whole 0 ;;
    region1) run_config "$family" "$variant" "$qwen_base" "$qwen_instruct" rows:1 1 local "" "--auto-tile-bytes $tile_budget" ;;
    region100) run_config "$family" "$variant" "$qwen_base" "$qwen_instruct" rows:100 1 local "" "--auto-tile-bytes $tile_budget" ;;
    *) echo "family_lifecycle: unknown configuration $variant" >&2; exit 64 ;;
  esac
done
fi

# --- large real fine-tune (Qwen2.5-3B base -> Instruct) -------------------
if [[ " $families " == *" qwen3b-finetune "* ]]; then
family=qwen3b-finetune
if [[ "${WEFT_FAMILY_SKIP_FAMILY:-0}" != "1" ]]; then
baselines_for "$family" "$qwen3b_base" "$qwen3b_instruct"
checkpoint_for "$family" "$qwen3b_base"
recovery_for "$family" "$qwen3b_base" '*.layers.0.*'
fi
for variant in $configs; do
  case "$variant" in
    default) run_config "$family" "$variant" "$qwen3b_base" "$qwen3b_instruct" whole 0 ;;
    region1) run_config "$family" "$variant" "$qwen3b_base" "$qwen3b_instruct" rows:1 1 local "" "--auto-tile-bytes $tile_budget" ;;
    region100) run_config "$family" "$variant" "$qwen3b_base" "$qwen3b_instruct" rows:100 1 local "" "--auto-tile-bytes $tile_budget" ;;
    *) echo "family_lifecycle: unknown configuration $variant" >&2; exit 64 ;;
  esac
done
fi

# --- object-store path (reduced, real MinIO on loopback) -------------------
if [[ "$run_s3" == "1" ]]; then
  minio_bin="$root/.docker-cache/minio/minio"
  if [[ ! -x "$minio_bin" ]]; then
    echo "family_lifecycle: no MinIO binary at $minio_bin; S3 configuration skipped" >&2
    echo "# s3 configuration skipped: no pinned MinIO binary" >> "$inputs"
  else
    s3_port="${WEFT_FAMILY_S3_PORT:-9311}"
    s3_data="$work/minio-data"
    rm -rf "$s3_data"
    mkdir -p "$s3_data"
    echo "== starting MinIO on 127.0.0.1:$s3_port =="
    MINIO_ROOT_USER=weftcourt MINIO_ROOT_PASSWORD=weftcourtsecret \
      "$minio_bin" server --address "127.0.0.1:$s3_port" --quiet "$s3_data" \
      >"$work/minio.log" 2>&1 &
    minio_pid=$!
    trap 'kill "$minio_pid" 2>/dev/null || true' EXIT
    # The bench court has no curl; a bash /dev/tcp connect is the readiness probe.
    ready=0
    for _ in $(seq 1 120); do
      if (exec 3<>/dev/tcp/127.0.0.1/"$s3_port") 2>/dev/null; then
        ready=1
        break
      fi
      sleep 0.5
    done
    if [[ "$ready" != "1" ]]; then
      echo "family_lifecycle: MinIO did not become ready; S3 configuration skipped" >&2
      echo "# s3 configuration skipped: MinIO did not start" >> "$inputs"
    else
      export WEFT_S3_ENDPOINT="http://127.0.0.1:$s3_port"
      export WEFT_S3_BUCKET="weft-family-lifecycle"
      export WEFT_S3_REGION="us-east-1"
      export WEFT_S3_ACCESS_KEY="weftcourt"
      export WEFT_S3_SECRET_KEY="weftcourtsecret"
      export WEFT_S3_LOCK_LEASE_SECS="${WEFT_S3_LOCK_LEASE_SECS:-60}"
      echo "family_lifecycle: MinIO ready (pid $minio_pid)"
      run_config qwen-finetune s3-reduced "$qwen_base" "$qwen_instruct" whole 0 s3
      rm -rf "$s3_data"
    fi
    kill "$minio_pid" 2>/dev/null || true
    wait "$minio_pid" 2>/dev/null || true
    trap - EXIT
  fi
fi

echo "== merge =="
# Build the label from the families that actually ran: a reduced aggregate run
# must not claim a family it skipped.
label=""
case " $families " in
  *" pythia-consecutive "*) label="pythia-70m step0->step1 (consecutive training checkpoints)" ;;
esac
case " $families " in
  *" qwen-finetune "*)
    [[ -n "$label" ]] && label="$label and "
    label="${label}Qwen2.5-0.5B base->Instruct (independent fine-tune)"
    ;;
esac
case " $families " in
  *" qwen3b-finetune "*)
    [[ -n "$label" ]] && label="$label and "
    label="${label}Qwen2.5-3B base->Qwen2.5-3B-Instruct (independent fine-tune, 434 BF16 tensors per checkpoint, 6,171,877,376 tensor data bytes each; published as two shards each, merged into one container with per-tensor sha256 parity)"
    ;;
esac
[[ -z "$label" ]] && label="no family selected"
label="real public weights: $label (configurations: $configs; ingest reps: $reps)"
WEFT_COURT_COMMAND="scripts/docker-court.sh bench \"cd /work && bash bench/scripts/checkpoint_family_court.sh\"" \
WEFT_FAMILY_LABEL="$label" \
  "$bin" merge --inputs "$inputs" --out "$receipt"

echo "family_lifecycle: receipt $receipt"
echo "family_lifecycle: log $log"
