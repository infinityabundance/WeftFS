#!/usr/bin/env bash
# Access-pattern-aware planner court on the real Qwen embedding.
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/access_pattern_court.sh"
#
# Runs `weftfs plan` for Whole (bytes-only and latency-weighted) and for
# Region/Mixed patterns, using the same tile shape as the float-tiled court.
# The measured per-read costs come from `bench/receipts/float-tiled-*.json`
# and are cited in `bench/RESULTS-mode17.md`.
set -Eeuo pipefail

root=/work
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$root/.docker-cache/access-pattern-court"
receipt="$root/bench/receipts/access-pattern-$stamp.json"
rows="$work/rows.jsonl"
manifest="$root/bench/corpora/real-models/qwen2.5-0.5b-pair.json"
source_file="${WEFT_REAL_MODEL:-$root/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors}"
tensor="${WEFT_ACCESS_TENSOR:-model.embed_tokens.weight}"
tile="${WEFT_ACCESS_TILE:-64,896}"

mkdir -p "$work" "$root/bench/receipts"
rm -f "$rows"

expected="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["pair"]["base"]["sha256"])' "$manifest" 2>/dev/null || true)"
if [[ -z "$expected" ]]; then
  expected="88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342"
fi
actual="$(sha256sum "$source_file" | cut -d' ' -f1)"
if [[ "$actual" != "$expected" ]]; then
  echo "access_pattern_court: source sha256 $actual != $expected" >&2
  exit 1
fi

echo "== build =="
cargo build --release --example decode_regions_bench
cargo build --release --bin weftfs
extractor="$root/.docker-cache/target-rust/release/examples/decode_regions_bench"
planner="$root/.docker-cache/target-rust/release/weftfs"

echo "== extract =="
"$extractor" --safetensors "$source_file" --tensor "$tensor" --extract "$work/embedding.bin"
embedding_bytes="$(stat -c %s "$work/embedding.bin")"
shape_text="${tensor_shape:-151936,896}"

plan() {
  local lambda="$1" pattern="$2"
  echo "== plan lambda=$lambda pattern=$pattern =="
  "$planner" plan \
    --store "$work/store" \
    --file "$work/embedding.bin" \
    --dtype bf16 \
    --shape "$shape_text" \
    --tile-shape "$tile" \
    --lambda-ns "$lambda" \
    --lambda-write-ns "$lambda" \
    --access-pattern "$pattern" \
    --json | tee -a "$rows"
}

mkdir -p "$work/store"
rm -rf "$work/store"
mkdir -p "$work/store"
"$planner" --store "$work/store" init >/dev/null
# Bytes-only whole: expect float-tensor (mode 9).
plan 0 whole
# Latency-weighted whole: mode 9 wins because its one-time encode is 3.25x
# cheaper than mode 17's.
plan 1 whole
# One read of one row (1,792 B): the write term blocks the tiled form.
plan 1 "region:1792:1"
# 1,000 reads of one row: the encode is amortised and the tiled form wins.
plan 1 "region:1792:1000"
# 1,000 reads of 100 rows (179,200 B).
plan 1 "region:179200:1000"
# 100 reads of the whole tensor.
plan 1 "region:$embedding_bytes:100"
# Mixed: one write plus 1,000 region reads.
plan 1 "mixed:0:1792:1000"

analysis="$work/analysis.json"
python3 - "$rows" "$analysis" <<'PY'
import json, math, sys

rows_path, out_path = sys.argv[1], sys.argv[2]
plans = [json.loads(line) for line in open(rows_path) if line.strip()]

def case(pattern):
    for plan in plans:
        if plan["access_pattern"] == pattern:
            return plan
    return None

def candidate(plan, strategy):
    for item in plan["candidates"]:
        if item["strategy"] == strategy and item["applicable"]:
            return item
    return None

cases = []
for plan in plans:
    cases.append({
        "access_pattern": plan["access_pattern"],
        "chosen": plan["chosen"],
        "chosen_physical_bytes": plan["chosen_physical_bytes"],
        "chosen_estimated_decode_ns": plan["chosen_estimated_decode_ns"],
        "chosen_estimated_encode_ns": plan["chosen_estimated_encode_ns"],
        "chosen_cost": plan["chosen_cost"],
        "runner_up": plan.get("runner_up"),
    })

# Break-even from the model:
#   cost = physical + lambda_read*decode(reads) + lambda_write*encode
# Solve for the read count where the tiled form beats the single-frame form.
break_even = None
region_once = case("region:1792:1")
if region_once:
    lambda_write = int(region_once["lambda_write_ns"])
    mode9 = candidate(region_once, "float-tensor")
    mode17 = candidate(region_once, "float-tiled")
    if mode9 and mode17:
        base9 = mode9["physical_bytes"] + lambda_write * mode9["estimated_encode_ns"]
        base17 = mode17["physical_bytes"] + lambda_write * mode17["estimated_encode_ns"]
        per9 = mode9["estimated_decode_ns"]
        per17 = mode17["estimated_decode_ns"]
        saving = per9 - per17
        if saving > 0:
            reads = math.ceil(max(base17 - base9, 0) / saving)
            break_even = {
                "model_break_even_reads": reads,
                "mode9_base_cost": base9,
                "mode17_base_cost": base17,
                "mode9_per_read_ns": per9,
                "mode17_per_read_ns": per17,
                "per_read_saving_ns": saving,
                "source": "weft plan --json at region:1792:1",
            }

# Measured counterpart from the float-tiled court (store-level write_ms and
# 1-row region reads). Recorded to say whether the model is in the right
# neighbourhood; never an input to a decision.
measured = {
    "source": "bench/receipts/float-tiled-20261107T042955Z.json",
    "mode9_encode_ms": 2286.7359669999996,
    "mode17_encode_ms": 7434.698213,
    "mode9_region_1_row_ms": 749.257607,
    "mode17_region_1_row_ms": 2.120317,
}
measured["break_even_reads"] = (measured["mode17_encode_ms"] - measured["mode9_encode_ms"]) / (
    measured["mode9_region_1_row_ms"] - measured["mode17_region_1_row_ms"]
)
if break_even:
    break_even["measured_break_even_reads"] = measured["break_even_reads"]
    break_even["abs_difference_reads"] = abs(
        break_even["model_break_even_reads"] - measured["break_even_reads"])
    break_even["matches_within_two_reads"] = break_even["abs_difference_reads"] <= 2.0

json.dump({"cases": cases, "break_even": break_even, "measured": measured},
          open(out_path, "w"), indent=2)
PY

{
  printf '{\n'
  printf '  "schema": "weft.receipt.access-pattern.v1",\n'
  printf '  "stamp": "%s",\n' "$stamp"
  printf '  "command": "scripts/docker-court.sh bench \\"cd /work && bash bench/scripts/access_pattern_court.sh\\"",\n'
  printf '  "environment": {\n'
  printf '    "court_image": "%s",\n' "${WEFT_COURT_IMAGE:-unknown}"
  printf '    "court_image_id": "%s",\n' "${WEFT_COURT_IMAGE_ID:-unknown}"
  printf '    "court_cpus": "%s",\n' "${WEFT_COURT_CPUS:-unknown}"
  printf '    "source": "%s",\n' "$source_file"
  printf '    "source_sha256": "%s",\n' "$actual"
  printf '    "tensor": "%s",\n' "$tensor"
  printf '    "tensor_bytes": %s,\n' "$embedding_bytes"
  printf '    "tile_shape": "%s",\n' "$tile"
  printf '    "planner_cost_model": "measured"\n'
  printf '  },\n'
  printf '  "measured_cost_source": "bench/receipts/float-tiled-20261107T042955Z.json",\n'
  printf '  "write_cost_source": "weftfs-write-cost/v1",\n'
  printf '  "analysis": '
  cat "$analysis"
  printf ',\n  "plans": [\n'
  awk 'BEGIN { first=1 } { if (!first) printf ",\n"; printf "    %s", $0; first=0 } END { printf "\n" }' "$rows"
  printf '  ]\n'
  printf '}\n'
} > "$receipt"

echo "receipt $receipt"
