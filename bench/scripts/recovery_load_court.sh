#!/usr/bin/env bash
# Stage 3/4 court: load receipts (lazy vs full) and recovery-first restore.
#
#   scripts/docker-court.sh bench \
#     "cd /work && bash bench/scripts/recovery_load_court.sh"
#
# Fixture: a synthetic 16 x [4096,4096] BF16 tensor (512 MiB) written as a
# mode-15 TENSOR_TILED checkpoint (tile 128x128, one shard) so the loader has
# independent tile roots to verify lazily. A whole public 0.5B model cannot be
# tiled with one global tile shape, which is why this court uses a synthetic
# shape that divides; the Qwen pair is measured in the write-policy court.
#
# Phases:
#   * loadreceipt lazy x3 and full x3: median ms, bytes, units, exactness, and
#     the canonical receipt root;
#   * recovery-first cold: time-to-first-usable vs time-to-complete and bytes
#     fetched before the first usable tensor;
#   * a real process abort after the required set is cached, then a restart
#     that serves the required set from the verified cache.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_STAMP="$stamp"
work="$root/.docker-cache/recovery-load-$stamp"
receipt="${WEFT_RECOVERY_LOAD_RECEIPT:-bench/receipts/recovery-load-$stamp.json}"
log="bench/receipts/recovery-load-$stamp.log"
mkdir -p "$work/phases" bench/receipts
exec > >(tee "$log") 2>&1

echo "recovery_load: stamp=$stamp"
echo "recovery_load: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "recovery_load: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "recovery_load: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"

echo "recovery_load: building release harness"
cargo build --release --example write_policy_bench >/dev/null 2>&1
cargo build --release --example trajectory_court >/dev/null 2>&1
bin="$CARGO_TARGET_DIR/release/examples/write_policy_bench"
gen="$CARGO_TARGET_DIR/release/examples/trajectory_court"
cli="$CARGO_TARGET_DIR/release/weftfs"

echo "recovery_load: generating a synthetic 16 x [4096,4096] BF16 tensor"
"$gen" gen --dir "$work/source" --steps 1 --tensors 16 --rows 4096 --cols 4096 \
  >"$work/phases/gen.txt"
source="$work/source/step001.safetensors"
sha256sum "$source"

store="$work/store"
"$cli" --store "$store" init --chunk-size 65536 --checkpoint-interval 256 --gc-grace-secs 0 \
  >/dev/null
echo "recovery_load: checkpoint write (mode 15, tile 128x128)"
"$cli" --store "$store" --json checkpoint write \
  --file "$source" --name tiled --shards 1 --axis 0 \
  --shard-strategy tensor-tiled --tile-shape 128,128 \
  >"$work/phases/checkpoint-write.json"
du -sh "$store" | tee "$work/phases/store-size.txt"

# --- load receipts ---------------------------------------------------------
for verification in lazy full; do
  echo "recovery_load: load receipt ($verification)"
  "$bin" loadreceipt --store "$store" --name tiled --verification "$verification" \
    --reps 3 --host-out "$work/loaded-$verification.safetensors" \
    --receipt-out "$work/receipt-$verification.wcb" \
    >"$work/phases/loadreceipt-$verification.json"
done

# --- recovery-first --------------------------------------------------------
required="synthetic.block00.weight,synthetic.block01.weight,synthetic.block02.weight,synthetic.block03.weight"
cache="$work/first-cache"
echo "recovery_load: recovery-first (cold cache)"
"$bin" recovery --store "$store" --name tiled --required "$required" \
  --cache "$cache" --host-out "$work/recovery-cold.safetensors" \
  >"$work/phases/recovery-cold.json"

echo "recovery_load: restart after the required set is cached (verify only)"
"$bin" recovery --store "$store" --name tiled --required "$required" \
  --cache "$cache" --host-out "$work/recovery-warm.safetensors" \
  >"$work/phases/recovery-warm.json"

echo "recovery_load: abort after the required set is verified and cached"
set +e
"$bin" recovery --store "$store" --name tiled --required "$required" \
  --cache "$cache" --host-out "$work/recovery-abort.safetensors" \
  --abort-after-required >"$work/phases/recovery-abort.json" 2>"$work/phases/recovery-abort.err"
status=$?
set -e
echo "$status" >"$work/phases/recovery-abort-status.txt"
echo "recovery_load: abort exit=$status (SIGABRT is expected)"
"$bin" recovery --store "$store" --name tiled --required "$required" \
  --cache "$cache" --host-out "$work/recovery-restart.safetensors" \
  >"$work/phases/recovery-restart.json"

"$bin" assemble --work "$work/phases" --out "$receipt"
echo "recovery_load: receipt=$receipt"
du -sh "$work" || true
