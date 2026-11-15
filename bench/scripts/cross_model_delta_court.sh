#!/usr/bin/env bash
# T051 cross-model lossless structural delta court.
#
#   scripts/docker-court.sh fuse "cd /work && bash bench/scripts/cross_model_delta_court.sh"
#
# Pairs: consecutive and distant pythia checkpoints (accept), an independent
# Qwen fine-tune slice (accept), the whole Qwen fine-tune through the shipped
# planner (store arm only), a different-size/dtype embedding pair (decline), a
# same-dtype/different-shape pair (decline), and a SYNTHETIC nothing-in-common
# pair (decline). Every accepted delta is replayed and compared byte for byte.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$root/.docker-cache/cross-model-$stamp"
receipt="bench/receipts/cross-model-delta-$stamp.json"
raw="$work/receipt.raw.json"
log="bench/receipts/cross-model-delta-$stamp.log"
slice="${WEFT_CROSS_SLICE_MIB:-256}"

mkdir -p "$work" bench/receipts
exec > >(tee "$log") 2>&1

echo "cross_model_delta: stamp=$stamp"
echo "cross_model_delta: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "cross_model_delta: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "cross_model_delta: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
echo "cross_model_delta: slice_mib=$slice"

for file in pythia-70m-step0 pythia-70m-step1 pythia-70m-step8 Qwen2.5-0.5B Qwen2.5-0.5B-Instruct; do
  path="$root/.docker-cache/real-corpus/$file.safetensors"
  [[ -f "$path" ]] || { echo "cross_model_delta: missing $path" >&2; exit 1; }
done

echo "cross_model_delta: building release example"
cargo build --release --example cross_model_court 2>&1 | tail -2

rm -rf "$work/scratch"
mkdir -p "$work/scratch"

echo "cross_model_delta: running harness"
".docker-cache/target-rust/release/examples/cross_model_court" \
  --corpus "$root" \
  --out "$raw" \
  --scratch "$work/scratch" \
  --slice-mib "$slice"

python3 - "$raw" "$receipt" "$log" "$stamp" <<'PY'
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

raw, out, log, stamp = sys.argv[1:5]
doc = json.load(open(raw))
doc["stamp"] = stamp
doc["generated_utc"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
doc["raw_log"] = log
cells = doc["cells"]
accepts = [cell for cell in cells if cell["decision"] == "accept"]
declines = [cell for cell in cells if cell["decision"] == "decline"]
checks = {
    # One accept is the honest expectation: byte-identical regions only exist
    # between near-identical checkpoints, not between independent fine-tunes.
    "at_least_one_accept": len(accepts) >= 1,
    "every_accept_replays_exactly": all(cell.get("replay_exact") for cell in accepts),
    "at_least_three_declines": len(declines) >= 3,
    "every_decline_is_typed": all(cell.get("reason") for cell in declines),
    "synthetic_cell_is_labelled": all(
        cell.get("synthetic") for cell in cells if cell["pair"] == "synthetic-nothing-in-common"
    ),
}
doc["checks"] = checks
doc["result"] = "pass" if all(checks.values()) else "fail"
Path(out).write_text(json.dumps(doc, indent=1) + "\n")
print("cross_model_delta: checks", json.dumps(checks))
for cell in cells:
    print(
        "cross_model_delta:",
        cell["pair"],
        cell["decision"],
        cell.get("frame_bytes"),
        cell.get("reason"),
    )
sys.exit(0 if all(checks.values()) else 1)
PY

echo "cross_model_delta: wrote $receipt"
