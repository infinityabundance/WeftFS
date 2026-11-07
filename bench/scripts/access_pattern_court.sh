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
    --access-pattern "$pattern" \
    --json | tee -a "$rows"
}

mkdir -p "$work/store"
rm -rf "$work/store"
mkdir -p "$work/store"
"$planner" --store "$work/store" init >/dev/null
# Bytes-only whole: expect float-tensor (mode 9).
plan 0 whole
# Latency-weighted whole: mode 17 is priced with the cheaper tiled decode row.
plan 1 whole
# 1,000 reads of one 1-row region (1,792 B): expect float-tiled.
plan 1 "region:1792:1000"
# The reviewer's case: 1,000 reads of 100 rows (179,200 B).
plan 1 "region:179200:1000"
# A single region read: the estimate still picks float-tiled, but the write cost
# (not in the objective) is 3.3x larger. See RESULTS-mode17.md.
plan 1 "region:1792:1"
# 100 reads of the whole tensor: still a tile-representation win in the model.
plan 1 "region:$embedding_bytes:100"
# Mixed: one write plus 1,000 region reads.
plan 1 "mixed:0:1792:1000"

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
  printf '  "plans": [\n'
  awk 'BEGIN { first=1 } { if (!first) printf ",\n"; printf "    %s", $0; first=0 } END { printf "\n" }' "$rows"
  printf '  ]\n'
  printf '}\n'
} > "$receipt"

echo "receipt $receipt"
