#!/usr/bin/env bash
# R12 lineage-aware rebase scale court.
#
# Runs inside the bench court (hard limits unchanged):
#
#   scripts/docker-court.sh bench "bash bench/scripts/rebase_scale_court.sh"
#
# Steps:
#   1. re-hash the cached Qwen2.5-0.5B / Instruct weights against the LFS
#      SHA-256 in bench/corpora/real-models/qwen2.5-0.5b-pair.json;
#   2. measure a host-local decode calibration with `weftfs calibrate`
#      (release build) and use it for every planner call;
#   3. ingest base + instruct, so every instruct tensor with an available
#      causal parent is a mode-11 weight delta;
#   4. snapshot the pre-rebase state, then read a hot subset 32x and a warm
#      subset 4x, recording access counters;
#   5. time per-tensor reads and a full checkpoint restore before rebasing;
#   6. price the rebase twice: at the policy threshold and at threshold=1
#      (the counterfactual economics for every read tensor);
#   7. apply, re-time, re-restore, re-stat, verify --full, re-apply (must be
#      a no-op), and reassemble the exact source container byte-for-byte;
#   8. assemble bench/receipts/rebase-scale-<UTC ISO>.json and
#      bench/RESULTS-rebase-scale.md.
#
# There is no fleet or portable-constant claim: one host, warm page cache,
# cached public weights, a single process at a time.
set -Eeuo pipefail

root=/work
corpus="$root/.docker-cache/real-corpus"
work="$root/.docker-cache/rebase-scale"
store="$work/store"
base="$corpus/Qwen2.5-0.5B.safetensors"
instruct="$corpus/Qwen2.5-0.5B-Instruct.safetensors"
manifest="$root/bench/corpora/real-models/qwen2.5-0.5b-pair.json"
threshold="${WEFT_REBASE_THRESHOLD_NS:-200000000}"
hot_reads="${WEFT_REBASE_HOT_READS:-32}"
warm_reads="${WEFT_REBASE_WARM_READS:-2}"
hot_count="${WEFT_REBASE_HOT_TENSORS:-16}"
warm_count="${WEFT_REBASE_WARM_TENSORS:-16}"
timing_reads="${WEFT_REBASE_TIMING_READS:-5}"

rm -rf "$work"
mkdir -p "$work"

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
cargo build --release --example real_model_bench --bin weftfs

bin="$CARGO_TARGET_DIR/release/weftfs"
harness="$CARGO_TARGET_DIR/release/examples/real_model_bench"

echo "== host decode calibration =="
"$bin" calibrate --out "$work/calibration.json" \
  --reps 7 --large-mib 16 --small-kib 128 > "$work/calibration.txt" 2>&1
cat "$work/calibration.txt"

echo "== ingest base + instruct =="
"$harness" ingest \
  --base-file "$base" --instruct-file "$instruct" \
  --store "$store" --out "$work/ingest.json" \
  --label "R12 rebase scale court"

echo "== choose hot and warm subsets =="
python3 - "$work/ingest.json" "$work/subsets.json" "$hot_count" "$warm_count" <<'PY'
import json, sys
ingest_path, out_path, hot_count, warm_count = sys.argv[1:5]
hot_count, warm_count = int(hot_count), int(warm_count)
ingest = json.load(open(ingest_path))
rows = [t for t in ingest["ingest"]["instruct"]["tensors"]
        if t["strategy"] == "weight-delta" and t["bytes"] >= 2 * 1024 * 1024]
if len(rows) < hot_count + warm_count:
    raise SystemExit(f"only {len(rows)} rebaseable deltas; need {hot_count + warm_count}")
hot = [t["name"] for t in rows[:hot_count]]
warm = [t["name"] for t in rows[hot_count:hot_count + warm_count]]
unread = [t["name"] for t in rows[hot_count + warm_count:]]
for name, names in (("hot.txt", hot), ("warm.txt", warm), ("timing.txt", hot + warm)):
    with open(out_path.rsplit("/", 1)[0] + "/" + name, "w") as f:
        f.write("\n".join(names) + "\n")
json.dump({
    "hot": hot, "warm": warm,
    "rebaseable_deltas": len(rows),
    "unread_deltas": len(unread),
    "minimum_tensor_bytes": 2 * 1024 * 1024,
}, open(out_path, "w"), indent=2)
print(f"hot={len(hot)} warm={len(warm)} unread_rebaseable={len(unread)}")
PY

echo "== snapshot + before state =="
"$bin" --store "$store" --json snapshot create before-rebase > "$work/snapshot.json"
"$harness" stats --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint instruct --sha256 --out "$work/stats-before.json"
"$harness" stats --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint base --out "$work/stats-base-before.json"

echo "== hot subset ($hot_reads reads) then warm subset ($warm_reads reads) =="
"$harness" hot --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint instruct --tensors-file "$work/hot.txt" \
  --reads "$hot_reads" --out "$work/hot-a.json"
"$harness" hot --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint instruct --tensors-file "$work/warm.txt" \
  --reads "$warm_reads" --out "$work/hot-b.json"

echo "== timing and restore before =="
"$harness" timing --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint instruct --tensors-file "$work/timing.txt" \
  --reads "$timing_reads" --out "$work/timing-before.json"
"$harness" restore --store "$store" --ingest-json "$work/ingest.json" \
  --out "$work/restore-before.json" --single-runs 3

echo "== rebase economics (counterfactual threshold=1) =="
"$bin" --store "$store" --calibration "$work/calibration.json" --json \
  rebase --root /instruct --threshold 1 --dry-run > "$work/rebase-dry-all.json"

echo "== rebase dry run at $threshold ns =="
"$bin" --store "$store" --calibration "$work/calibration.json" --json \
  rebase --root /instruct --threshold "$threshold" --dry-run > "$work/rebase-dry.json"

echo "== rebase apply =="
"$bin" --store "$store" --calibration "$work/calibration.json" --json \
  rebase --root /instruct --threshold "$threshold" > "$work/rebase-apply.json"

echo "== rebase apply again (idempotence) =="
"$bin" --store "$store" --calibration "$work/calibration.json" --json \
  rebase --root /instruct --threshold "$threshold" > "$work/rebase-apply2.json"

echo "== provenance receipt read-back =="
python3 - "$work/rebase-apply.json" "$work/first-applied.txt" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
applied = report.get("applied", [])
if not applied:
    raise SystemExit("rebase applied nothing; cannot read a provenance receipt")
open(sys.argv[2], "w").write(applied[0]["object"])
print(applied[0]["object"], applied[0]["provenance_id"])
PY
"$bin" --store "$store" --json provenance show "$(cat "$work/first-applied.txt")" \
  > "$work/provenance-show.json"

echo "== after state =="
"$harness" timing --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint instruct --tensors-file "$work/timing.txt" \
  --reads "$timing_reads" --out "$work/timing-after.json"
"$harness" restore --store "$store" --ingest-json "$work/ingest.json" \
  --out "$work/restore-after.json" --single-runs 3
"$harness" stats --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint instruct --out "$work/stats-after.json"
"$harness" stats --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint instruct --snapshot before-rebase --sha256 \
  --out "$work/stats-snapshot.json"

echo "== verify --full =="
"$bin" --store "$store" --json verify --full > "$work/verify.json"

echo "== exact source reassembly after rebase =="
"$harness" tiled --store "$store" --ingest-json "$work/ingest.json" \
  --checkpoint instruct --file "$instruct" --threads 1 \
  --out "$work/exact.json" --remove-materialized

echo "== assemble receipt =="
python3 "$root/bench/scripts/assemble_rebase_scale.py" \
  --work "$work" --threshold "$threshold" \
  --out-dir "$root/bench/receipts" \
  --results "$work/RESULTS-rebase-scale.md"
echo "results draft: $work/RESULTS-rebase-scale.md"
