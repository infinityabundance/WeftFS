#!/usr/bin/env bash
# T052 exact-vs-approximate view court.
#
#   scripts/docker-court.sh fuse "cd /work && bash bench/scripts/view_identity_court.sh"
#
# Writes q8_0-block views of real Qwen (bf16) and pythia (f32) tensors at three
# block sizes, and measures stored bytes, read time and the measured error
# against the descriptor's declared per-element bound. Safety gates: the exact
# object must be unchanged and still verify, the declared bound must hold, and
# the exact resolution door must refuse every view descriptor.
#
# Knobs (recorded in the receipt): WEFT_VIEW_TENSORS (comma separated).
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$root/.docker-cache/view-identity-$stamp"
receipt="bench/receipts/view-identity-$stamp.json"
log="bench/receipts/view-identity-$stamp.log"
mkdir -p "$work" bench/receipts
exec > >(tee "$log") 2>&1

echo "view_identity: stamp=$stamp"
echo "view_identity: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "view_identity: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "view_identity: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"

qwen="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
pythia="$root/.docker-cache/real-corpus/pythia-70m-step0.safetensors"
[[ -f "$qwen" ]] || { echo "view_identity: missing $qwen" >&2; exit 1; }
[[ -f "$pythia" ]] || { echo "view_identity: missing $pythia" >&2; exit 1; }

cargo build --release --example view_court 2>&1 | tail -2
rm -rf "$work/scratch"; mkdir -p "$work/scratch"

tensors="${WEFT_VIEW_TENSORS:-model.layers.0.mlp.down_proj.weight,model.embed_tokens.weight}"
blocks="${WEFT_VIEW_BLOCKS:-32,64,128}"
".docker-cache/target-rust/release/examples/view_court" \
  --model "$qwen" --tensors "$tensors" --blocks "$blocks" \
  --scratch "$work/scratch" --out "$work/qwen.json"
".docker-cache/target-rust/release/examples/view_court" \
  --model "$pythia" --tensors "gpt_neox.layers.0.attention.query_key_value.weight" \
  --blocks "$blocks" --scratch "$work/scratch-pythia" --out "$work/pythia.json"

python3 - "$work/qwen.json" "$work/pythia.json" "$receipt" "$stamp" "$log" "$tensors" "$blocks" <<'PY'
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

qwen_path, pythia_path, out, stamp, log, tensors, blocks = sys.argv[1:8]
qwen = json.load(open(qwen_path))
pythia = json.load(open(pythia_path))
qwen["pythia_cells"] = pythia["tensors"]
qwen["stamp"] = stamp
qwen["generated_utc"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
qwen["raw_log"] = log
qwen["configuration"] = {"qwen_tensors": tensors.split(","), "pythia_tensor": "gpt_neox.layers.0.attention.query_key_value.weight", "blocks": [int(b) for b in blocks.split(",")]}

views = []
for cell in qwen["tensors"] + pythia["tensors"]:
    for view in cell["views"]:
        if not view.get("refused"):
            views.append(view)
checks = {
    "every_view_bound_holds": all(view["error_holds"] for view in views),
    "every_exact_object_intact": all(view["exact_object_intact"] for view in views),
    "exact_door_refuses_every_view": all(view["exact_door_refused"] for view in views),
    "every_view_is_lossy": all(not view["resolved_bytes_equal_exact"] for view in views),
    "store_verify_full_clean": qwen["verify"]["problems"] == 0 and pythia["verify"]["problems"] == 0,
}
qwen["checks"] = checks
qwen["result"] = "pass" if all(checks.values()) else "fail"
Path(out).write_text(json.dumps(qwen, indent=1) + "\n")
print("view_identity: checks", json.dumps(checks))
for cell in qwen["tensors"] + pythia["tensors"]:
    for view in cell["views"]:
        if view.get("refused"):
            print("view_identity:", cell["tensor"], view["block"], "refused", view["message"])
        else:
            print(
                "view_identity:", cell["tensor"], "block", view["block"],
                "bytes", view["stored_physical_bytes"], "vs exact", cell["exact_stored_physical_bytes"],
                "ratio", round(view["physical_vs_exact"], 4),
                "read", round(view["view_read_ms"], 3), "vs", round(cell["exact_read_ms"], 3),
                "err", view["max_abs_error"], "bound", view["max_bound"],
            )
sys.exit(0 if all(checks.values()) else 1)
PY

echo "view_identity: wrote $receipt"
