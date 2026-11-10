#!/usr/bin/env bash
# Throughput court (subphase R10).
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/throughput_court.sh"
#
# Measures, on real public weights and inside the OOM-protected bench court:
#
#   1. the phase breakdown of a checkpoint save (source read, digest, encode,
#      blob publication, manifest/commit, index segment, verify) for fast raw,
#      fast zstd-1, and the planned writer;
#   2. flat vs packed publication at 1/2/4/8 ranks (same head, same manifest
#      root, measured requests and bytes);
#   3. full-checkpoint restore throughput at 1/2/4/8 workers and 1/4 tile
#      workers, with the SHA-256 of every restored container checked against the
#      checkpoint's recorded source SHA-256.
#
# Every phase is a separate process so peak RSS is honest per phase. The
# receipt embeds each phase's JSON unchanged.
#
# Reduced configuration (for `verify-all.sh`): set
#   WEFT_THROUGHPUT_FAMILIES="qwen0.5b"   (default "qwen0.5b qwen3b")
#   WEFT_THROUGHPUT_REPS=1
#   WEFT_THROUGHPUT_RANKS="0 8"
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$root/.docker-cache/throughput-$stamp"
receipt="bench/receipts/throughput-$stamp.json"
log="bench/receipts/throughput-$stamp.log"

families="${WEFT_THROUGHPUT_FAMILIES:-qwen0.5b qwen3b}"
reps="${WEFT_THROUGHPUT_REPS:-3}"
ranks="${WEFT_THROUGHPUT_RANKS:-0 1 2 4 8}"
restore_threads="${WEFT_THROUGHPUT_RESTORE_THREADS:-1 2 4 8}"
tile_threads="${WEFT_THROUGHPUT_TILE_THREADS:-1 4}"
shards="${WEFT_THROUGHPUT_SHARDS:-8}"
save_configs="${WEFT_THROUGHPUT_SAVE_CONFIGS:-raw zstd planned}"
# Reduced aggregate runs ingest a tensor prefix (`--limit`) instead of the whole
# family; the receipt records the limit and its own concatenated digest covers
# exactly the selected tensors.
ingest_limit="${WEFT_THROUGHPUT_INGEST_LIMIT:-}"

corpus="$root/.docker-cache/real-corpus"
mkdir -p "$work" bench/receipts
exec > >(tee "$log") 2>&1

echo "throughput_court: stamp=$stamp"
echo "throughput_court: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "throughput_court: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "throughput_court: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
echo "throughput_court: families=$families reps=$reps ranks=$ranks restore_threads=$restore_threads tile_threads=$tile_threads configs=$save_configs"

qwen_c="$corpus/Qwen2.5-0.5B.safetensors"
qwen3b="$corpus/Qwen2.5-3B.safetensors"

# The corpus digests are the committed manifest values; the court refuses to
# measure a file it has not identified. The 3B file is only hashed when that
# family is selected.
sha_qwen=88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342
sha_qwen3b=52da3d997beeb4739e5f4f315f0d36b5a9d6f7ad038935774cac670abd1917ab

verify_sha256() {
  local file="$1" expected="$2"
  if [[ ! -f "$file" ]]; then
    echo "throughput_court: missing corpus $file" >&2
    exit 1
  fi
  local actual
  actual="$(sha256sum "$file" | cut -d' ' -f1)"
  if [[ "$actual" != "$expected" ]]; then
    echo "throughput_court: sha256 mismatch for $file" >&2
    echo "  expected $expected" >&2
    echo "  actual   $actual" >&2
    exit 1
  fi
  echo "throughput_court: verified $(basename "$file") sha256=$actual"
}

echo "== build =="
cargo build --release --example bench_parallel_checkpoint --example family_lifecycle
bin="${CARGO_TARGET_DIR:-/work/.docker-cache/target-rust}/release/examples/family_lifecycle"
save_bin="${CARGO_TARGET_DIR:-/work/.docker-cache/target-rust}/release/examples/bench_parallel_checkpoint"

inputs=()
add_input() { inputs+=(--input "$1"); }

for family in $families; do
  case "$family" in
    qwen0.5b)
      model="$qwen_c"
      verify_sha256 "$model" "$sha_qwen"
      ;;
    qwen3b)
      model="$qwen3b"
      verify_sha256 "$model" "$sha_qwen3b"
      ;;
    *)
      echo "throughput_court: unknown family $family" >&2
      exit 64
      ;;
  esac
  family_work="$work/$family"
  mkdir -p "$family_work"

  # ---- save: phase breakdown, flat vs packed ------------------------------
  for config in $save_configs; do
    case "$config" in
      raw) policy_args=(--fast --fast-codec raw) ;;
      zstd) policy_args=(--fast --fast-codec zstd) ;;
      planned) policy_args=() ;;
      *) echo "throughput_court: unknown save config $config" >&2; exit 64 ;;
    esac
    echo "== save $family $config (flat vs packed, ranks: $ranks) =="
    # The harness always measures [0,1,2,4,8]; the reduced aggregate trims the
    # configurations it does not need by filtering the receipt later, so a
    # reduced run is honestly labelled rather than silently different. The full
    # standalone run uses every rank.
    "$save_bin" \
      --model "$model" \
      --work "$family_work/save-$config" \
      --out "$family_work/save-$config.json" \
      --stamp "$stamp-$family-$config" \
      --phases --pack-compare --reps "$reps" --shards "$shards" \
      --ranks "$ranks" \
      "${policy_args[@]}"
    add_input "save_${family}_${config}=$family_work/save-$config.json"
  done

  # ---- ingest phase breakdown (per tensor and per checkpoint) -------------
  echo "== ingest $family (phase profile, $reps rep, limit=${ingest_limit:-all}) =="
  local ingest_limit_args=()
  if [[ -n "$ingest_limit" ]]; then
    ingest_limit_args=(--limit "$ingest_limit")
  fi
  WEFT_PHASE_PROFILE=1 "$bin" ingest \
    --store "$family_work/ingest-store" \
    --base-file "$model" --next-file "$model" \
    --family "$family-throughput" --variant throughput \
    --access-pattern whole --lambda-ns 0 \
    --reps "$reps" --no-snapshot "${ingest_limit_args[@]}" \
    --out "$family_work/ingest.json"
  add_input "ingest_${family}=$family_work/ingest.json"

  # ---- checkpoint write + pipelined restore ------------------------------
  echo "== checkpoint $family (fast raw, $shards-way, phase profile) =="
  WEFT_PHASE_PROFILE=1 "$bin" checkpoint \
    --store "$family_work/checkpoint-store" --file "$model" --name "$family" \
    --shards "$shards" --axis 0 --strategy tensor-sharded \
    --write-policy fast --fast-codec raw \
    --out "$family_work/checkpoint.json"
  add_input "checkpoint_${family}=$family_work/checkpoint.json"

  echo "== restore $family (threads: $restore_threads; tile threads: $tile_threads) =="
  "$bin" restore \
    --store "$family_work/checkpoint-store" --name "$family" \
    --threads "$(echo "$restore_threads" | tr ' ' ',')" \
    --tile-threads "$(echo "$tile_threads" | tr ' ' ',')" \
    --max-inflight-bytes 268435456 \
    --reps "$reps" \
    --out "$family_work/restore.json"
  add_input "restore_${family}=$family_work/restore.json"

  # Keep the disk bounded: the stores are large and only the JSON is evidence.
  rm -rf "$family_work"/save-*/store-* "$family_work/ingest-store" "$family_work/checkpoint-store"
done

echo "== assemble =="
WEFT_COURT_COMMAND="scripts/docker-court.sh bench \"cd /work && bash bench/scripts/throughput_court.sh\"" \
  "$bin" throughput \
  --label "phase breakdown, pack-plane save, and pipelined restore on real public weights (families: $families; save configs: $save_configs; reps: $reps; ranks: $ranks; restore threads: $restore_threads; tile threads: $tile_threads)" \
  "${inputs[@]}" \
  --out "$receipt"

echo "throughput_court: receipt $receipt"
echo "throughput_court: log $log"
