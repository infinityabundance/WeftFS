#!/usr/bin/env bash
# S13b court: measure the whole-checkpoint restore overhead the affine decode
# table omits, and validate the fitted term on a real pythia-70m checkpoint.
#
#   scripts/docker-court.sh bench \
#     "cd /work && bash bench/scripts/s13b_restore_floor_court.sh"
#
# The harness times real `Store::restore_checkpoint` calls (not in-memory frame
# decode) at a range of object sizes, object counts and a real container. It
# fits `residue = a + b * tensors + c * logical_bytes` where
# `residue = measured_restore - sum(measured decode rows)`, and records the
# per-hop cost of a controlled dependency chain so the chain over-prediction can
# be attributed (S13b hypothesis 3).
#
# Every default here is a court constant; override only to investigate.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_STAMP="$stamp"
work="$root/.docker-cache/s13b-$stamp"
receipt="${WEFT_S13B_RECEIPT:-bench/receipts/s13b-restore-floor-$stamp.json}"
log="bench/receipts/s13b-restore-floor-$stamp.log"
corpus="$root/.docker-cache/real-corpus"
real="${WEFT_S13B_REAL_CONTAINER:-$corpus/pythia-70m-step0.safetensors}"
real_steps="${WEFT_S13B_REAL_STEPS:-0,1,2,4,8,16,64,512,1000}"
reps="${WEFT_S13B_REPS:-7}"
counts="${WEFT_S13B_COUNTS:-1,2,4,8,16,32,64,128,256,512,1024,2048,4096,8192,16384}"
count_total_mib="${WEFT_S13B_COUNT_TOTAL_MIB:-64}"
sizes="${WEFT_S13B_SIZES:-1,2,4,8,16,32,64}"
size_count="${WEFT_S13B_SIZE_COUNT:-8}"
chain_mib="${WEFT_S13B_CHAIN_MIB:-8}"
chain_depth="${WEFT_S13B_CHAIN_DEPTH:-8}"
mkdir -p bench/receipts "$work"
exec > >(tee "$log") 2>&1

echo "s13b: stamp=$stamp"
echo "s13b: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "s13b: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "s13b: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
echo "s13b: reps=$reps counts=$counts total_mib=$count_total_mib sizes=$sizes size_count=$size_count"
echo "s13b: chain=${chain_mib}MiB depth=$chain_depth real=$real"

for file in "$real"; do
  if [[ ! -f "$file" ]]; then
    echo "s13b: missing $file" >&2
    exit 1
  fi
  echo "s13b: real container $file $(stat -c %s "$file") B sha256=$(sha256sum "$file" | awk '{print $1}')"
done
if [[ -n "$real_steps" ]]; then
  for step in ${real_steps//,/ }; do
    file="$corpus/pythia-70m-step$step.safetensors"
    if [[ ! -f "$file" ]]; then
      echo "s13b: missing trajectory file $file" >&2
      exit 1
    fi
    echo "s13b: trajectory step$step $(stat -c %s "$file") B sha256=$(sha256sum "$file" | awk '{print $1}')"
  done
fi

echo "s13b: building the release harness"
cargo build --release --example calibrate_restore >/dev/null 2>&1
bin="$CARGO_TARGET_DIR/release/examples/calibrate_restore"

echo "s13b: measuring"
"$bin" \
  --out "$receipt" \
  --work "$work" \
  --reps "$reps" \
  --object-counts "$counts" \
  --count-total-mib "$count_total_mib" \
  --object-mib "$sizes" \
  --size-count "$size_count" \
  --chain-depth "$chain_depth" \
  --chain-tensor-mib "$chain_mib" \
  --sweep-strategy planned \
  --sweep-dtype f32 \
  --real-container "$real" \
  --real-strategy planned \
  --real-corpus "$corpus" \
  --real-steps "$real_steps"

echo "s13b: receipt=$receipt"
echo "s13b: work=$work"
python3 - "$receipt" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print("s13b: schema", d["schema"])
fit = d.get("fit") or {}
print("s13b: fit per_version_ns=%s per_tensor_ns=%s per_mib_ns=%s samples=%s worst_residual_ppm=%s"
      % (fit.get("per_version_ns"), fit.get("per_tensor_ns"), fit.get("per_mib_ns"),
         fit.get("samples"), fit.get("worst_residual_ppm")))
print("s13b: baked", json.dumps(d.get("baked")))
for row in d.get("real_validation", []):
    print("s13b: real %s predicted=%d measured=%d error_ppm=%d"
          % (row["id"], row["predicted_restore_ns"], row["measured_restore_ns"], row["restore_error_ppm"]))
for row in d.get("trajectory_validation", []):
    print("s13b: trajectory %s predicted=%d measured=%d error_ppm=%d"
          % (row["name"], row["predicted_restore_ns"], row["measured_restore_ns"], row["restore_error_ppm"]))
for kind in ("sparse", "dense"):
    chain = d.get("chain", {}).get(kind) or {}
    hops = chain.get("hops") or []
    if not hops:
        continue
    inc = [h["measured_hop_delta_ns"] for h in hops if h.get("measured_hop_delta_ns")]
    model = [h["model_hop_ns"] for h in hops if h.get("model_hop_ns")]
    if inc and model:
        print("s13b: chain %s measured_hop=[%s] model_hop=%s"
              % (kind, ",".join(str(v // 1000) + "us" for v in inc), model[0]))
print("s13b: checks", json.dumps(d.get("checks"), sort_keys=True))
PY
