#!/usr/bin/env bash
# T046 / P1-12 — GPU-resident exact reconstruction court for WeftCodec mode 9.
#
# This court is the documented exception to the Docker-only build rule
# (AGENTS.md directive 1, guardrail 7): CUDA needs the host device. Every other
# court in this repository still runs inside scripts/docker-court.sh.
#
# It measures, on a real device:
#   * the serial Huffman decode of the exponent plane (CPU — it is serial by
#     construction and is the dominant term),
#   * the parallel sign/mantissa join on the CPU and on the GPU (pageable,
#     pinned, and fused device-resident),
# and verifies byte-exact reconstruction against the shipped decoder.
set -euo pipefail

STAMP="${WEFT_GPU_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/bench/receipts"
WORK="$ROOT/.docker-cache/gpu-t046"
mkdir -p "$OUT" "$WORK/out4m" "$WORK/out16m" "$WORK/out64m"

if ! command -v nvidia-smi >/dev/null 2>&1; then
  echo "gpu_mode9_court: skip no nvidia-smi on this host" >&2
  exit 78
fi
if ! command -v nvcc >/dev/null 2>&1; then
  echo "gpu_mode9_court: skip no nvcc on this host" >&2
  exit 78
fi
if [ ! -e /dev/nvidia0 ]; then
  echo "gpu_mode9_court: skip no /dev/nvidia0" >&2
  exit 78
fi

GPU_NAME="$(nvidia-smi --query-gpu=name --format=csv,noheader | head -1)"
GPU_MEM="$(nvidia-smi --query-gpu=memory.total --format=csv,noheader | head -1)"
GPU_DRIVER="$(nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -1)"
GPU_CC="$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader | head -1)"
NVCC_VER="$(nvcc --version | tail -1 | sed 's/^ *//')"

echo "gpu: $GPU_NAME ($GPU_MEM, driver $GPU_DRIVER, cc $GPU_CC)"
echo "nvcc: $NVCC_VER"

(cd "$ROOT/bench/gpu/prep" && cargo build --release --quiet)
nvcc -O3 -arch=sm_89 -std=c++17 "$ROOT/bench/gpu/mode9_gpu.cu" -o "$WORK/mode9_gpu"

REV_WEFTC=$(git -C "$ROOT/WeftCodec" rev-parse --short HEAD)
REV_WEFT=$(git -C "$ROOT" rev-parse --short HEAD)
REV_CODECGO=$(git -C "$ROOT/WeftCodec-Go" rev-parse --short HEAD)
REV_FSGO=$(git -C "$ROOT/WeftFS-Go" rev-parse --short HEAD)

SIZES="4194304 16777216 67108864"
: > "$WORK/rows.jsonl"
for n in $SIZES; do
  dir="$WORK/out${n}"
  prep_line="$("$ROOT/bench/gpu/prep/target/release/prep" "$dir" "$n")"
  echo "prep[$n]: $prep_line"
  harness="$("$WORK/mode9_gpu" "$dir" 7)"
  echo "harness[$n]: $harness"
  python3 - "$n" "$prep_line" "$harness" >> "$WORK/rows.jsonl" <<'PY'
import json, sys
n, prep_line, harness = sys.argv[1], sys.argv[2], sys.argv[3]
prep = dict(kv.split("=") for kv in prep_line.split())
row = json.loads(harness)
row["elements"] = int(n)
row["prep"] = {k: (float(v) if ("_ms" in k) else int(v)) for k, v in prep.items()}
row["logical_bytes"] = int(n) * 2
print(json.dumps(row))
PY
done

python3 - "$OUT" "$STAMP" "$GPU_NAME" "$GPU_MEM" "$GPU_DRIVER" "$GPU_CC" "$NVCC_VER" \
         "$REV_WEFT" "$REV_WEFTC" "$REV_CODECGO" "$REV_FSGO" "$WORK/rows.jsonl" <<'PY'
import json, sys, subprocess, os
(out, stamp, name, mem, driver, cc, nvcc, rw, rc, rcg, rfg, rows_path) = sys.argv[1:13]
rows = [json.loads(line) for line in open(rows_path) if line.strip()]
receipt = {
    "schema": "weft.gpu.mode9.court.v1",
    "stamp": stamp,
    "court": "gpu_mode9",
    "result": "pass" if all(r.get("exact_plaintext") and r.get("fused_exact") for r in rows) else "fail",
    "device": {"name": name, "memory": mem, "driver": driver, "compute_capability": cc, "nvcc": nvcc},
    "revisions": {"weftfs": rw, "weftcodec": rc, "weftcodec_go": rcg, "weftfs_go": rfg},
    "limits": {
        "note": "host device, not the Docker court: CUDA needs the GPU (AGENTS.md directive 1, guardrail 7)",
        "gpu_memory_total": mem,
    },
    "method": {
        "frame": "mode 9 FLOAT_TENSOR, BF16, encoded by the shipped encoder",
        "exponent_plane": "Huffman-coded; decoded serially on the CPU in both arms",
        "gpu_work": "sign/mantissa plane join (vec4), plus a fused device-resident consumer",
        "reps": 7,
        "host_buffers": ["pageable", "pinned"],
    },
    "rows": rows,
    "claims": {
        "exact_reconstruction": all(r.get("exact_plaintext") for r in rows),
        "fused_consumer_exact": all(r.get("fused_exact") for r in rows),
    },
    "not_claimed": [
        "a full GPU Huffman decode (the symbol stream is serial; not implemented here)",
        "any fleet, cloud or multi-GPU claim",
        "any claim that GPU decode wins overall: see the rows, copies dominate at these sizes",
    ],
}
path = os.path.join(out, f"gpu-mode9-{stamp}.json")
with open(path, "w") as f:
    json.dump(receipt, f, indent=2, sort_keys=True)
    f.write("\n")
print(f"wrote {path}")
PY

echo "gpu_mode9_court: receipt bench/receipts/gpu-mode9-${STAMP}.json"
