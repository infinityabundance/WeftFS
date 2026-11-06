#!/usr/bin/env bash
# R1 planner/float-tensor/rebase court.
#
# Runs inside the bench court (hard memory/pids/cpus limits unchanged):
#
#   scripts/docker-court.sh bench "bash bench/scripts/planner_float_court.sh"
#
# Steps:
#   1. re-hash the cached Qwen2.5-0.5B / Instruct weights against the LFS
#      SHA-256 in bench/corpora/real-models/qwen2.5-0.5b-pair.json;
#   2. calibrate the decode cost table (7 reps, 128 KiB + 16 MiB);
#   3. ingest a base-only store and the base+instruct family with the new
#      planner (measured table + mode 9);
#   4. restore the family and one tensor;
#   5. read every instruct tensor 10x to make the access counters hot;
#   6. run `weft rebase --root /instruct --dry-run`;
#   7. assemble bench/receipts/planner-float-<UTC ISO>.json.
set -Eeuo pipefail

root=/work
corpus="$root/.docker-cache/real-corpus"
work="$root/.docker-cache/planner-float"
mkdir -p "$work"
rm -rf "$work/store" "$work/store-base" "$work/restore"

base="$corpus/Qwen2.5-0.5B.safetensors"
instruct="$corpus/Qwen2.5-0.5B-Instruct.safetensors"
manifest="$root/bench/corpora/real-models/qwen2.5-0.5b-pair.json"

echo "== corpus SHA-256 =="
python3 - "$manifest" "$base" "$instruct" <<'PY'
import hashlib, json, sys
manifest, base, instruct = sys.argv[1:4]
data = json.load(open(manifest))["pair"]
def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()
for name, path in (("base", base), ("instruct", instruct)):
    want = data[name]["sha256"]
    got = digest(path)
    if got != want:
        raise SystemExit(f"{name}: sha256 {got} != {want}")
    print(f"{name}: sha256 {got} ok")
PY

echo "== build =="
cargo build --release --example real_model_bench --example calibrate_decode --bin weftfs

echo "== decode calibration =="
cargo run --release --example calibrate_decode -- \
  --out "$work/decode.json" --reps 7 --large-mib 16 --small-kib 128

echo "== ingest base-only =="
cargo run --release --example real_model_bench -- ingest \
  --base-file "$base" --instruct-file "$instruct" \
  --store "$work/store-base" --out "$work/ingest-base.json" --skip-instruct

echo "== ingest family =="
cargo run --release --example real_model_bench -- ingest \
  --base-file "$base" --instruct-file "$instruct" \
  --store "$work/store" --out "$work/ingest.json"

echo "== restore family + single tensor =="
cargo run --release --example real_model_bench -- restore \
  --store "$work/store" --ingest-json "$work/ingest.json" \
  --out "$work/restore.json" --single-runs 5 \
  --reassemble-dir "$work/restore"

echo "== hot instruct (10 reads per tensor) =="
cargo run --release --example real_model_bench -- hot \
  --store "$work/store" --ingest-json "$work/ingest.json" \
  --out "$work/hot.json" --reads 10 --checkpoint instruct

echo "== rebase dry run =="
threshold="${WEFT_REBASE_THRESHOLD_NS:-1000000000}"
"$root/.docker-cache/target-rust/release/weftfs" \
  --store "$work/store" --json rebase --root /instruct \
  --threshold "$threshold" --dry-run > "$work/rebase.json"

echo "== assemble receipt =="
python3 "$root/bench/scripts/assemble_planner_float.py" \
  --ingest "$work/ingest.json" \
  --ingest-base "$work/ingest-base.json" \
  --restore "$work/restore.json" \
  --hot "$work/hot.json" \
  --rebase "$work/rebase.json" \
  --decode "$work/decode.json" \
  --old-receipt "$root/bench/receipts/real-model-20261105T012000Z.json" \
  --out-dir "$root/bench/receipts"
