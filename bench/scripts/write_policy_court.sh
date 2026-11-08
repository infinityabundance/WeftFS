#!/usr/bin/env bash
# Stage 1 court: critical-path write policy + bounded background transcode.
#
#   scripts/docker-court.sh bench \
#     "cd /work && bash bench/scripts/write_policy_court.sh"
#
# Measures, on the real Qwen2.5-0.5B base and Instruct containers:
#
#   * save pause (the write call returns durably) and process wall time for
#     `planned`, `fast`-raw, `fast`-zstd-1 and `fast`-zstd-3;
#   * background transcode wall/CPU/bytes moved and the resulting physical
#     bytes + strategy mix;
#   * full / single-tensor / 1-row restore before and after transcode;
#   * a real process abort mid-transcode (after a durable publication and just
#     before one), then a reopen that must restore the exact source file.
#
# Every phase is a separate process. The store for one configuration is
# deleted before the next configuration runs, so the court never needs eight
# 1 GiB stores at once.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_STAMP="$stamp"
work="$root/.docker-cache/write-policy-$stamp"
receipt="${WEFT_WRITE_POLICY_RECEIPT:-bench/receipts/write-policy-$stamp.json}"
log="bench/receipts/write-policy-$stamp.log"
corpus="$root/.docker-cache/real-corpus"
base="$corpus/Qwen2.5-0.5B.safetensors"
instruct="$corpus/Qwen2.5-0.5B-Instruct.safetensors"
single="${WEFT_WRITE_POLICY_SINGLE:-model.embed_tokens.weight}"
row_range="${WEFT_WRITE_POLICY_RANGE:-0:1,0:896}"
shards="${WEFT_WRITE_POLICY_SHARDS:-8}"
configs="${WEFT_WRITE_POLICY_CONFIGS-planned fast-raw fast-zstd1 fast-zstd3}"
models="${WEFT_WRITE_POLICY_MODELS-base instruct}"

mkdir -p "$work/phases" bench/receipts
exec > >(tee "$log") 2>&1

echo "write_policy: stamp=$stamp"
echo "write_policy: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "write_policy: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "write_policy: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
echo "write_policy: configs=$configs models=$models"

verify_corpus() {
  local file="$1"
  [[ -f "$file" ]] || { echo "write_policy: missing corpus $file" >&2; exit 1; }
  sha256sum "$file"
}
verify_corpus "$base"
verify_corpus "$instruct"

echo "write_policy: building release harness"
cargo build --release --example write_policy_bench >/dev/null 2>&1
bin="$CARGO_TARGET_DIR/release/examples/write_policy_bench"

save_args() {
  local cfg="$1"
  case "$cfg" in
    planned) echo "--policy planned --codec raw --level 1" ;;
    fast-raw) echo "--policy fast --codec raw --level 1" ;;
    fast-zstd1) echo "--policy fast --codec zstd --level 1" ;;
    fast-zstd3) echo "--policy fast --codec zstd --level 3" ;;
    auto-planned) echo "--policy auto" ;;
    auto-critical) echo "--policy auto --codec zstd --level 1 --critical-path" ;;
    *) echo "unknown config $cfg" >&2; exit 1 ;;
  esac
}

run() {
  local out="$1"; shift
  "$@" >"$out"
}

for model in $models; do
  case "$model" in
    base) src="$base" ;;
    instruct) src="$instruct" ;;
    *) echo "unknown model $model" >&2; exit 1 ;;
  esac
  for cfg in $configs; do
    store="$work/store-$model-$cfg"
    prefix="$model-$cfg"
    echo "write_policy: $prefix save"
    read -r -a args <<<"$(save_args "$cfg")"
    run "$work/phases/$prefix-save.json" \
      "$bin" save --source "$src" --store "$store" --name model --shards "$shards" "${args[@]}"
    run "$work/phases/$prefix-info-before.json" "$bin" info --store "$store"

    echo "write_policy: $prefix restore (full / single / 1-row)"
    run "$work/phases/$prefix-restore-full.json" \
      "$bin" restore --store "$store" --name model
    run "$work/phases/$prefix-restore-single.json" \
      "$bin" restore --store "$store" --name model --tensor "$single"
    run "$work/phases/$prefix-restore-row.json" \
      "$bin" restore --store "$store" --name model --tensor "$single" --range "$row_range"

    if [[ "$cfg" != planned ]]; then
      echo "write_policy: $prefix transcode"
      run "$work/phases/$prefix-transcode.json" \
        "$bin" transcode --store "$store" --root /checkpoints/model
      run "$work/phases/$prefix-info-after.json" "$bin" info --store "$store"
      run "$work/phases/$prefix-restore-after-full.json" \
        "$bin" restore --store "$store" --name model
      run "$work/phases/$prefix-verify.json" "$bin" verify --store "$store" --name model
    fi
    rm -rf "$store"
  done
done

# --- crash proxy -----------------------------------------------------------
# The transcode child aborts inside `Store::transcode`. The store must reopen
# and restore exactly, with each object either at its old or its new
# representation.
crash_store="$work/store-crash"
echo "write_policy: crash proxy (fast-raw save)"
"$bin" save --source "$base" --store "$crash_store" --name model \
  --policy fast --codec raw --level 1 --shards "$shards" >"$work/phases/crash-save.json"

crash_phase() {
  local label="$1"; shift
  set +e
  "$bin" transcode --store "$crash_store" --root /checkpoints/model "$@" \
    >"$work/phases/crash-$label-transcode.json" \
    2>"$work/phases/crash-$label-transcode.err"
  local status=$?
  set -e
  echo "$status" >"$work/phases/crash-$label-status.txt"
  "$bin" verify --store "$crash_store" --name model \
    >"$work/phases/crash-$label-verify.json"
  "$bin" info --store "$crash_store" >"$work/phases/crash-$label-info.json"
  echo "write_policy: crash $label exit=$status"
}

crash_phase after-publish --crash-after 1
crash_phase before-publish --crash-before 1

# A clean transcode after the crash must finish the job and still restore.
"$bin" transcode --store "$crash_store" --root /checkpoints/model \
  >"$work/phases/crash-recover-transcode.json"
"$bin" verify --store "$crash_store" --name model \
  >"$work/phases/crash-recover-verify.json"
rm -rf "$crash_store"

# --- receipt ---------------------------------------------------------------
"$bin" assemble --work "$work/phases" --out "$receipt"
echo "write_policy: receipt=$receipt"
du -sh "$work" || true
