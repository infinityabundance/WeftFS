#!/usr/bin/env bash
# R22 probe: where is the wall actually spent at the new operating point?
#
# Runs **inside** the bench court (`scripts/docker-court.sh bench "bash
# bench/scripts/r22_probe.sh ..."`), so the hard limits are the usual ones:
# 3 GiB memory == swap, 1024 pids, 4 CPUs, bounded /tmp. Nothing is raised.
#
# One `prepare` builds the frozen subset (the 272 MB embedding plus 100 evenly
# spaced tensors, 424,439,296 logical bytes); then one `write` per row, each in
# its own directory, so a row can carry extra pipeline flags the JSON matrix
# cannot express. One repetition each: this is a **probe**, not the cited sweep,
# and its numbers are labelled as such.
#
# Usage, inside the court:
#   bash bench/scripts/r22_probe.sh STAMP --prepare            # subset only
#   bash bench/scripts/r22_probe.sh STAMP --row NAME [flags]   # one write
#
# The host-side driver (`bench/scripts/r22_probe_court.sh`) gives every row its
# own container so a row cannot inherit another row's unflushed writes.
set -Eeuo pipefail

root=/work
cd "$root"
# The stamp is passed explicitly: `docker-court.sh` forwards only its own
# whitelisted environment, so an exported variable would not survive entry.
stamp_arg=""
if [[ "${1:-}" != "" && "${1:0:2}" != "--" ]]; then
  stamp_arg="$1"
  shift
fi
stamp="${stamp_arg:-$(date -u +%Y%m%dT%H%M%SZ)}"
fixture="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
work="$root/.docker-cache/r22-probe-$stamp"
outdir="$root/bench/receipts/r22-probe-$stamp"

if [[ ! -f "$fixture" ]]; then
  echo "r22_probe: missing fixture $fixture" >&2
  exit 1
fi
mkdir -p "$root/bench/receipts" "$outdir"
chmod a+rwX "$root/bench/receipts" "$outdir" 2>/dev/null || true

mode="${1:-}"
shift || true

case "$mode" in
  --prepare)
    rm -rf "$work"
    mkdir -p "$work"
    cargo run --release --quiet --example augmentation_bench -- prepare \
      --fixture "$fixture" \
      --work "$work" \
      --max-others 100 --include-embedding 1
    ;;
  --row)
    name="${1:?row name}"
    shift
    config="${CONFIG:-C-pipe4-zpar3}"
    dir="$work/row-$name"
    rm -rf "$dir"
    mkdir -p "$dir"
    echo "== R22 probe row $name (config $config) =="
    cargo run --release --quiet --example augmentation_bench -- write \
      --work "$dir" \
      --subset "$work/subset.json" \
      --config "$config" \
      --backend local \
      --phase-out "$dir/write.json" \
      --chunk-size 1048576 \
      --seed weft-augmentation-r3 \
      --force \
      "$@"
    python3 - "$dir/write.json" "$outdir/$name.json" "$name" <<'PY'
import json, sys
source, target, name = sys.argv[1:]
with open(source) as handle:
    data = json.load(handle)
pipeline = data.get("pipeline") or {}
ledger = data.get("ledger") or {}
barrier = pipeline.get("early_barrier") or {}
embed = next((t for t in data.get("tensors", []) if "embed" in t["name"]), {})
row = {
    "row": name,
    "wall_ms": data.get("wall_ms"),
    "prepare_ms": data.get("prepare_ms"),
    "commit_ms": data.get("commit_ms"),
    "cpu_ms": data.get("cpu_user_sys_ms"),
    "peak_rss_kib": data.get("peak_rss_kib"),
    "physical_footprint_bytes": (data.get("storage") or {}).get("physical_footprint_bytes"),
    "store_head": data.get("store_head"),
    "representations": data.get("representations"),
    "stages_ms": pipeline.get("stages_ms") or {},
    "waits_ms": pipeline.get("waits_ms") or {},
    "overlap": pipeline.get("overlap") or {},
    "budgets": pipeline.get("budgets") or {},
    "writeback_busy_ms": barrier.get("busy_ms"),
    "writeback_files": barrier.get("files"),
    "writeback_passes": barrier.get("passes"),
    "prepare_breakdown": ledger.get("prepare_breakdown") or {},
    "embedding_encode_ms": (embed.get("pipeline") or {}).get("encode_ms"),
    "embedding_publish_ms": (embed.get("pipeline") or {}).get("publish_ms"),
    "environment_notes": data.get("environment"),
}
with open(target, "w") as handle:
    json.dump(row, handle, indent=1, sort_keys=True)
    handle.write("\n")
print("r22_probe: row file", target)
PY
    rm -rf "$dir"
    ;;
  *)
    echo "usage: r22_probe.sh --prepare | --row NAME [bench flags]" >&2
    exit 64
    ;;
esac
