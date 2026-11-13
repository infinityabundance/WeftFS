#!/usr/bin/env bash
# Streaming mode-9 encoder court (X04).
#
# Measures the same real tensors through three processes:
#   oneshot   read the whole tensor, encode_float_tensor (historical)
#   stream    encode_float_tensor_stream over a bounded file source
#   analysis  the streamed counting pass only (histogram + hash + exact length)
#
# Every process is separate, so `VmHWM` is the honest peak for that phase. The
# frames must be byte-identical and the receipt fails if they are not.
#
# Corpus: the real Qwen2.5-0.5B-Instruct and Qwen2.5-7B-Instruct containers in
# `.docker-cache/real-corpus/`, read in place. Default court limits (3 GiB,
# 3 GiB swap, 1024 pids, 4 cpus, 1 GiB /tmp); never raised.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="$root/bench/receipts/mode9-stream-$stamp.json"
log="$root/bench/receipts/mode9-stream-$stamp.log"
reps="${WEFT_MODE9_STREAM_REPS:-3}"
work="$root/.docker-cache/mode9-stream-$stamp"
work_c="/work/.docker-cache/mode9-stream-$stamp"
mkdir -p "$work" "$root/bench/receipts"
chmod a+rwX "$work" "$root/bench/receipts" 2>/dev/null || true
: > "$log"

df -h "$root" | tee -a "$log"

small=".docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
large=".docker-cache/real-corpus/Qwen2.5-7B-Instruct.safetensors"
for corpus in "$small" "$large"; do
  if [[ ! -f "$corpus" ]]; then
    echo "mode9_stream_court: skip missing corpus $corpus" >&2
    exit 78
  fi
done

if ! scripts/docker-court.sh bench \
  "cd /work && cargo build --release --example mode9_stream_bench" >>"$log" 2>&1; then
  echo "mode9_stream_court: build failed; see $log" >&2
  exit 1
fi

cmd="B=/work/.docker-cache/target-rust/release/examples/mode9_stream_bench;"
cmd+=" W=$work_c; mkdir -p \$W;"
for pair in "Qwen2.5-0.5B-Instruct.safetensors model.embed_tokens.weight small" \
            "Qwen2.5-7B-Instruct.safetensors lm_head.weight large"; do
  set -- $pair
  corpus="$1"; tensor="$2"; label="$3"
  for rep in $(seq 1 "$reps"); do
    for phase in oneshot stream analysis; do
      cmd+=" \$B --phase $phase --source /work/.docker-cache/real-corpus/$corpus"
      cmd+=" --tensor $tensor --out \$W/$label-$phase-$rep.json >/dev/null 2>&1 || echo FAIL $label $phase $rep;"
    done
  done
done
cmd+=" chmod a+rw \$W/*.json 2>/dev/null || true"

if ! scripts/docker-court.sh bench "$cmd" >>"$log" 2>&1; then
  echo "mode9_stream_court: measurement failed; see $log" >&2
fi

python3 - "$work" "$receipt" "$stamp" "$reps" "$root" "$log" <<'PY'
import json
import os
import statistics
import sys

work, receipt, stamp, reps, root, log = sys.argv[1:7]
reps = int(reps)

rows = []
for name in sorted(os.listdir(work)):
    if not name.endswith(".json"):
        continue
    with open(os.path.join(work, name)) as handle:
        rows.append(json.load(handle))

def cells(label, phase):
    return [row for row in rows if row["tensor"].startswith(("lm_head", "model.embed_tokens")) and row["phase"] == phase and (("large" in label and row["logical_bytes"] > 500_000_000) or ("small" in label and row["logical_bytes"] < 500_000_000))]

measures = {}
for label in ("small", "large"):
    for phase in ("oneshot", "stream", "analysis"):
        selected = cells(label, phase)
        if not selected:
            continue
        measures[f"{label}-{phase}"] = {
            "reps": len(selected),
            "logical_bytes": selected[0]["logical_bytes"],
            "frame_bytes": selected[0]["frame_bytes"],
            "frame_sha256": selected[0]["frame_sha256"],
            "wall_ms_median": round(statistics.median(r["wall_ms"] for r in selected), 3),
            "wall_ms_min": round(min(r["wall_ms"] for r in selected), 3),
            "peak_rss_kib_median": int(statistics.median(r["peak_rss_kib"] for r in selected)),
            "mb_per_s_median": round(statistics.median(r["mb_per_s"] for r in selected), 3),
        }

checks = {}
for label in ("small", "large"):
    one = measures.get(f"{label}-oneshot")
    stream = measures.get(f"{label}-stream")
    checks[f"{label}_frames_identical"] = bool(one and stream and one["frame_sha256"] == stream["frame_sha256"])
    if one and stream:
        checks[f"{label}_rss_ratio_oneshot_over_stream"] = round(one["peak_rss_kib_median"] / stream["peak_rss_kib_median"], 3)
        checks[f"{label}_wall_ratio_stream_over_oneshot"] = round(stream["wall_ms_median"] / one["wall_ms_median"], 3)

data = {
    "schema": "weft-mode9-stream-court-v1",
    "generated_utc": stamp,
    "reps": reps,
    "label": "single host, warm page cache, real public tensors; not a fleet/media claim",
    "court": {"kind": "bench", "memory": "3g", "swap": "3g", "pids": 1024, "cpus": 4, "tmpfs": "1g"},
    "measures": measures,
    "checks": checks,
    "result": "pass" if all(checks.get(f"{label}_frames_identical") for label in ("small", "large")) else "fail",
    "log": os.path.relpath(log, root),
}
with open(receipt, "w") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")

for key, value in measures.items():
    print(f"{key:16s} logical={value['logical_bytes']:>11d} frame={value['frame_bytes']:>11d} "
          f"wall_ms={value['wall_ms_median']:9.3f} MB/s={value['mb_per_s_median']:8.1f} "
          f"rss_kib={value['peak_rss_kib_median']:>9d}")
for key, value in checks.items():
    print(f"check {key} = {value}")
print("result:", data["result"])
print("receipt:", receipt)
PY

echo "mode9_stream_court: done -> $receipt" | tee -a "$log"
