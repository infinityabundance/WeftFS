#!/usr/bin/env bash
# Real-weight mode-9 vs mode-17 decode court.
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/float_tiled_court.sh"
#
# One tensor from the cached Qwen2.5-0.5B BF16 container (the embedding by
# default) is written twice into a fresh store, then read whole / 100 rows /
# 1 row at 1 and 4 threads. Every read is checked byte-identical.
set -Eeuo pipefail

root=/work
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
work="$root/.docker-cache/float-tiled-court"
receipt="$root/bench/receipts/float-tiled-$stamp.json"
rows="$work/rows.jsonl"
manifest="$root/bench/corpora/real-models/qwen2.5-0.5b-pair.json"
source_file="${WEFT_REAL_MODEL:-$root/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors}"
tensor="${WEFT_FLOAT_TILED_TENSOR:-model.embed_tokens.weight}"
tile="${WEFT_FLOAT_TILED_TILE:-64,896}"
reps="${WEFT_FLOAT_TILED_REPS:-3}"

mkdir -p "$work" "$root/bench/receipts"
rm -rf "$work/store"
rm -f "$rows"

expected="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["pair"]["base"]["sha256"])' "$manifest")"
actual="$(sha256sum "$source_file" | cut -d' ' -f1)"
if [[ "$actual" != "$expected" ]]; then
  echo "float_tiled_court: source sha256 $actual != manifest $expected" >&2
  exit 1
fi
echo "source sha256 verified in court: $actual"

echo "== build =="
cargo build --release --example decode_regions_bench
binary="$root/.docker-cache/target-rust/release/examples/decode_regions_bench"

"$binary" \
  --store "$work/store" \
  --safetensors "$source_file" \
  --tensor "$tensor" \
  --tile-shape "$tile" \
  --reps "$reps" | tee "$rows"

{
  printf '{\n'
  printf '  "schema": "weft.receipt.float-tiled.v1",\n'
  printf '  "stamp": "%s",\n' "$stamp"
  printf '  "command": "scripts/docker-court.sh bench \\"cd /work && bash bench/scripts/float_tiled_court.sh\\"",\n'
  printf '  "environment": {\n'
  printf '    "court_image": "%s",\n' "${WEFT_COURT_IMAGE:-unknown}"
  printf '    "court_image_id": "%s",\n' "${WEFT_COURT_IMAGE_ID:-unknown}"
  printf '    "court_kernel": "%s",\n' "${WEFT_COURT_KERNEL:-unknown}"
  printf '    "court_cpus": "%s",\n' "${WEFT_COURT_CPUS:-unknown}"
  printf '    "source": "%s",\n' "$source_file"
  printf '    "source_sha256": "%s",\n' "$actual"
  printf '    "tensor": "%s",\n' "$tensor"
  printf '    "tile_shape": "%s",\n' "$tile"
  printf '    "reps": %s,\n' "$reps"
  printf '    "cache_state": "warm in-process store; source file read once"\n'
  printf '  },\n'
  printf '  "rows": [\n'
  awk 'BEGIN { first=1 } { if (!first) printf ",\n"; printf "    %s", $0; first=0 } END { printf "\n" }' "$rows"
  printf '  ]\n'
  printf '}\n'
} > "$receipt"

echo "receipt $receipt"
