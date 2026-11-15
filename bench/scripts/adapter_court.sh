#!/usr/bin/env bash
# T048 adapter court.
#
#   scripts/docker-court.sh fuse "cd /work && bash bench/scripts/adapter_court.sh"
#
# Measures typed LoRA/DoRA adapter artifacts on the pinned public corpus:
# adapter-only storage/restore, the base+adapter family, precomputed merged
# views, and optionally a full merged-checkpoint copy. Comparators are plain
# files and `zstd -19` on the same bytes, with every store metadata byte
# included in the retained total.
#
# Every adapter restore must reproduce the source `adapter_model.safetensors`
# byte-for-byte (its SHA-256 is checked inside the harness). A NumPy reference
# re-computes a few merged tensors from the original files; its differences
# from the pinned Rust arithmetic are recorded, not hidden.
#
# The court writes one receipt and one raw log beside it, plus the reference
# JSON. Configuration knobs (reduced values are recorded in the receipt):
#   WEFT_ADAPTERS_LIMIT       adapters to ingest                (default: all)
#   WEFT_ADAPTER_MERGE_LIMIT  adapters to merge into views      (default: all)
#   WEFT_ADAPTER_MERGED_FULL  1 writes a full merged checkpoint (default: 0)
#   WEFT_ADAPTER_MERGED_ZSTD  merged containers to zstd -19     (default: 1)
#   WEFT_ADAPTER_DUMPS        tensors dumped for the reference  (default: 3)
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_STAMP="$stamp"
work="$root/.docker-cache/adapter-court-$stamp"
receipt="bench/receipts/adapters-$stamp.json"
raw="bench/receipts/adapters-$stamp.raw.json"
reference="bench/receipts/adapters-reference-$stamp.json"
log="bench/receipts/adapters-$stamp.log"
limit="${WEFT_ADAPTERS_LIMIT:-999}"
merge_limit="${WEFT_ADAPTER_MERGE_LIMIT:-999}"
merged_full="${WEFT_ADAPTER_MERGED_FULL:-0}"
merged_zstd="${WEFT_ADAPTER_MERGED_ZSTD:-1}"
dumps="${WEFT_ADAPTER_DUMPS:-3}"

base="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
corpus="bench/corpora/real-models/adapters-qwen2.5-0.5b.json"
base_sha=fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe

mkdir -p "$work" bench/receipts
exec > >(tee "$log") 2>&1

echo "adapter_court: stamp=$stamp"
echo "adapter_court: court=$(uname -srm) image=${WEFT_COURT_IMAGE:-unknown} id=${WEFT_COURT_IMAGE_ID:-unknown}"
echo "adapter_court: limits memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown}"
echo "adapter_court: revisions weftfs=${WEFT_REPO_WEFTFS:-unknown} weftcodec=${WEFT_REPO_WEFTCODEC:-unknown}"
echo "adapter_court: configuration adapters=$limit merge=$merge_limit merged_full=$merged_full merged_zstd=$merged_zstd dumps=$dumps"

if [[ ! -f "$base" ]]; then
  echo "adapter_court: missing base corpus $base" >&2
  exit 1
fi
if ! grep -q "$base_sha" "$corpus"; then
  echo "adapter_court: corpus manifest does not name the pinned base sha $base_sha" >&2
  exit 1
fi
actual="$(sha256sum "$base" | cut -d' ' -f1)"
if [[ "$actual" != "$base_sha" ]]; then
  echo "adapter_court: base sha256 $actual != $base_sha" >&2
  exit 1
fi
echo "adapter_court: verified base sha256=$actual"

# Every adapter file is re-hashed by the harness against the manifest; count
# them here so the log records how many the corpus names.
adapters_named="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["adapters"]))' "$corpus")"
echo "adapter_court: corpus names $adapters_named adapters"

echo "adapter_court: building release example"
cargo build --release --example adapter_court 2>&1 | tail -2

rm -rf "$work/scratch" "$work/dump"
mkdir -p "$work/scratch" "$work/dump"

echo "adapter_court: running harness"
extra=()
if [[ "$merged_full" == "1" ]]; then
  extra+=(--merged-full)
fi
".docker-cache/target-rust/release/examples/adapter_court" \
  --base "$base" \
  --corpus "$corpus" \
  --repo-root "$root" \
  --out "$raw" \
  --scratch "$work/scratch" \
  --adapters-limit "$limit" \
  --merge-limit "$merge_limit" \
  --merged-zstd "$merged_zstd" \
  --dump-merged "$work/dump" \
  --dump-tensors "$dumps" \
  ${extra[@]+"${extra[@]}"}

echo "adapter_court: running the NumPy reference merge"
python3 bench/scripts/adapter_reference_merge.py \
  --receipt "$raw" \
  --corpus "$corpus" \
  --base "$base" \
  --repo-root "$root" \
  --dump "$work/dump" \
  --out "$reference"

echo "adapter_court: assembling receipt"
python3 - "$raw" "$reference" "$receipt" "$log" "$stamp" <<'PY'
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

raw, reference, out, log, stamp = sys.argv[1:6]
receipt = json.load(open(raw))
reference_doc = json.load(open(reference))
receipt["stamp"] = stamp
receipt["generated_utc"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
receipt["reference_merge"] = reference_doc
receipt["raw_log"] = log

adapters = receipt["adapters"]
views = receipt["merged_views"]
checks = {
    "every_adapter_restore_is_byte_exact": all(cell["restore_exact"] for cell in adapters),
    "every_adapter_verified": all(cell["verify_targets"] == cell["targets"] for cell in adapters),
    "base_restore_is_byte_exact": receipt["base"]["restore_exact"],
    "every_merged_view_restored_exactly": len(views) > 0,
    "store_verify_full_clean": receipt["verify"]["problems"] == 0,
    "reference_merge_present": bool(reference_doc.get("tensors")),
    "reference_merge_differs_from_base": bool(reference_doc.get("none_equal_base")),
}
receipt["checks"] = checks
receipt["result"] = "pass" if all(checks.values()) else "fail"
Path(out).write_text(json.dumps(receipt, indent=1) + "\n")
print("adapter_court: checks", json.dumps(checks))
print("adapter_court: reference mismatches",
      [(row["tensor"], row["f32_matmul_mismatches"], row["f64_accumulation_mismatches"])
       for row in reference_doc.get("tensors", [])])
sys.exit(0 if all(checks.values()) else 1)
PY

rm -f "$raw"
echo "adapter_court: wrote $receipt"
