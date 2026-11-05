#!/usr/bin/env bash
# P3 checkpoint-topology court.
#
# Runs INSIDE `scripts/docker-court.sh fuse` (FUSE device + SYS_ADMIN, every
# hard limit unchanged). It ingests a real safetensors checkpoint 8-way on
# axis 0, restores it under topologies 8 and 4, restores one tensor and one
# region, then mounts the store and lets an unmodified Python loader
# (safetensors + NumPy + ml_dtypes) read the restored container from the
# mount and compare tensors and SHA-256 with the source file.
#
# Usage (inside the court, cwd=/work):
#   bash bench/scripts/checkpoint_topology_court.sh
#
# Environment:
#   WEFT_CHECKPOINT_CORPUS   source container (default Qwen2.5-0.5B)
#   WEFT_CHECKPOINT_SHARDS   write topology (default 8)
#   WEFT_CHECKPOINT_AXIS     write axis (default 0)
#   WEFT_CHECKPOINT_TENSORS  optional comma-separated tensor-name filters
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

corpus="${WEFT_CHECKPOINT_CORPUS:-$root/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors}"
shards="${WEFT_CHECKPOINT_SHARDS:-8}"
axis="${WEFT_CHECKPOINT_AXIS:-0}"
filters="${WEFT_CHECKPOINT_TENSORS:-}"
single_tensor="${WEFT_CHECKPOINT_SINGLE_TENSOR:-model.embed_tokens.weight}"
slice_range="${WEFT_CHECKPOINT_SLICE_RANGE:-1000:1100,0:896}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
work="$root/.docker-cache/checkpoint-p3"
receipt="${WEFT_CHECKPOINT_RECEIPT:-$root/bench/receipts/checkpoint-topology-$stamp.json}"

if [[ ! -f "$corpus" ]]; then
  echo "checkpoint court: missing corpus $corpus" >&2
  exit 1
fi

echo "checkpoint court: building release binary"
cargo build --release --features fuse --bin weftfs >/dev/null 2>&1
bin="$CARGO_TARGET_DIR/release/weftfs"

rm -rf "$work"
mkdir -p "$work/mnt"

echo "checkpoint court: init store"
"$bin" --store "$work/store" init --chunk-size 65536 --checkpoint-interval 256 --gc-grace-secs 0 >/dev/null

filter_args=()
if [[ -n "$filters" ]]; then
  IFS=',' read -r -a names <<<"$filters"
  for name in "${names[@]}"; do
    filter_args+=(--tensor-filter "$name")
  done
fi

echo "checkpoint court: ingest (${shards}-way, axis ${axis})"
"$bin" --store "$work/store" --json checkpoint write \
  --file "$corpus" --name qwen \
  --shards "$shards" --axis "$axis" \
  "${filter_args[@]}" \
  >"$work/write.json"

objects_bytes="$(du -sb "$work/store/objects" | awk '{print $1}')"
manifests_bytes="$(du -sb "$work/store/manifests" | awk '{print $1}')"
store_bytes="$(du -sb "$work/store" | awk '{print $1}')"
source_bytes="$(stat -c %s "$corpus")"

restore_once() {
  local out_dir="$1" topology="$2" json="$3" spool="$4"
  sync
  "$bin" --store "$work/store" --json checkpoint restore \
    --name qwen --out "$out_dir" --topology "$topology" --axis "$axis" \
    --spool "$spool" \
    >"$json"
}

# Two runs per topology: the first is cold for the output path, the second is
# warm. `sync` before each timed run keeps the previous run's dirty output from
# contaminating the next measurement.
echo "checkpoint court: full restore, topology ${shards} (run 1)"
restore_once /restored8a "$shards" "$work/restore8a.json" "$work/spool8a.safetensors"
echo "checkpoint court: full restore, topology 4 (run 1)"
restore_once /restored4a 4 "$work/restore4a.json" "$work/spool4a.safetensors"
echo "checkpoint court: full restore, topology ${shards} (run 2, warm)"
restore_once /restored8b "$shards" "$work/restore8b.json" "$work/spool8b.safetensors"
echo "checkpoint court: full restore, topology 4 (run 2, warm)"
restore_once /restored4b 4 "$work/restore4b.json" "$work/spool4b.safetensors"

sync
echo "checkpoint court: single-tensor restore"
"$bin" --store "$work/store" --json checkpoint restore \
  --name qwen --out /restored-single --out-name embed.safetensors \
  --tensor "$single_tensor" \
  --spool "$work/spool-single.safetensors" \
  >"$work/single.json"

echo "checkpoint court: slice restore (rows 1000..1100 of embed_tokens)"
"$bin" --store "$work/store" --json checkpoint restore \
  --name qwen --out /restored-slice --out-name embed-rows.safetensors \
  --tensor "$single_tensor" --range "$slice_range" \
  --spool "$work/spool-slice.safetensors" \
  >"$work/slice.json"

sync
echo "checkpoint court: full checkpoint verify"
"$bin" --store "$work/store" --json checkpoint verify --name qwen >"$work/verify.json"

echo "checkpoint court: mounting store"
mountpoint="$work/mnt"
"$bin" --store "$work/store" mount --mountpoint "$mountpoint" --foreground \
  >"$work/mount.out" 2>"$work/mount.err" &
mpid=$!
cleanup() {
  if kill -0 "$mpid" 2>/dev/null; then
    kill -TERM "$mpid" 2>/dev/null || true
    wait "$mpid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

mounted_file="$mountpoint/restored4b/qwen.safetensors"
ready=0
for _ in $(seq 1 120); do
  if [[ -f "$mounted_file" ]]; then
    ready=1
    break
  fi
  sleep 1
done
if [[ "$ready" -ne 1 ]]; then
  echo "checkpoint court: mount did not expose $mounted_file" >&2
  cat "$work/mount.err" >&2 || true
  exit 1
fi

daemon_peak_rss_kib="$(awk '/^VmHWM:/ {print $2}' "/proc/$mpid/status" 2>/dev/null || echo 0)"
mount_loader_ok=0
if python3 bench/scripts/mount_loader.py \
  --mounted "$mounted_file" \
  --source "$corpus" \
  --json "$work/loader.json"; then
  mount_loader_ok=1
fi
daemon_peak_rss_kib_after="$(awk '/^VmHWM:/ {print $2}' "/proc/$mpid/status" 2>/dev/null || echo 0)"

kill -TERM "$mpid" 2>/dev/null || true
wait "$mpid" 2>/dev/null || true
trap - EXIT

echo "checkpoint court: writing receipt"
python3 - "$root" "$work" "$receipt" "$corpus" "$source_bytes" \
  "$objects_bytes" "$manifests_bytes" "$store_bytes" \
  "$daemon_peak_rss_kib" "$daemon_peak_rss_kib_after" "$mount_loader_ok" "$stamp" <<'PY'
import json
import os
import subprocess
import sys

(
    root,
    work,
    receipt,
    corpus,
    source_bytes,
    objects_bytes,
    manifests_bytes,
    store_bytes,
    daemon_rss,
    daemon_rss_after,
    mount_loader_ok,
    stamp,
) = sys.argv[1:]


def load(name):
    with open(os.path.join(work, name), encoding="utf-8") as handle:
        return json.load(handle)


def git(path, rev="HEAD"):
    try:
        return subprocess.check_output(
            ["git", "-C", path, "rev-parse", rev], text=True
        ).strip()
    except Exception:
        return "unknown"


write = load("write.json")
restore_runs = [
    ("topology8-cold", 8, load("restore8a.json")),
    ("topology4-cold", 4, load("restore4a.json")),
    ("topology8-warm", 8, load("restore8b.json")),
    ("topology4-warm", 4, load("restore4b.json")),
]
restore8 = restore_runs[0][2]
restore4 = restore_runs[1][2]
restore4_warm = restore_runs[3][2]
single = load("single.json")
slice_report = load("slice.json")
verify = load("verify.json")
loader = load("loader.json")

roots8 = {row["name"]: row["full_logical_root"] for row in restore8["tensors"]}
roots4 = {row["name"]: row["full_logical_root"] for row in restore4["tensors"]}
root_sets_equal = all(
    {row["name"]: row["full_logical_root"] for row in run["tensors"]} == roots8
    for _, _, run in restore_runs
) and len(roots8) == len(write["tensors"])
topology_roots = {
    row["name"]: row["topology_root"]
    for row in restore4["tensors"]
    if row.get("topology_root")
}

checks = [
    {
        "check": "ingest_complete",
        "ok": bool(write["complete"]),
        "detail": f"tensors={len(write['tensors'])} shard_objects={write['shard_objects']}",
    },
    {
        "check": "every_full_restore_reproduces_the_source_sha256",
        "ok": all(
            run["exact_source_file"] and run["file_sha256"] == loader["source_sha256"]
            for _, _, run in restore_runs
        ),
        "detail": f"runs={len(restore_runs)}",
    },
    {
        "check": "logical_roots_identical_across_topologies",
        "ok": root_sets_equal,
        "detail": f"tensors={len(roots8)}",
    },
    {
        "check": "topology4_reroot_matches_manifest_root",
        "ok": all(
            run["tensors"][i]["topology_root"] == roots4[run["tensors"][i]["name"]]
            for _, _, run in (restore_runs[1], restore_runs[3])
            for i in range(len(run["tensors"]))
        )
        and len(topology_roots) == len(roots4),
        "detail": f"rerooted={len(topology_roots)}",
    },
    {
        "check": "checkpoint_verify_all_tensors",
        "ok": verify["tensors_verified"] == len(write["tensors"]),
        "detail": f"tensors_verified={verify['tensors_verified']}",
    },
    {
        "check": "slice_touches_one_shard_only",
        "ok": slice_report["shards_read"] == 1
        and slice_report["units_touched"] == 1
        and slice_report["store_bytes_read"] < single["store_bytes_read"],
        "detail": "rows 1000..1100 lie inside shard 0 of an 8-way axis-0 split",
    },
    {
        "check": "mount_loader_ok",
        "ok": loader["ok"] and mount_loader_ok == "1",
        "detail": (
            f"tensors={loader['tensor_count']} sha256_match={loader['sha256_match']} "
            f"materialised={loader['all_tensors_materialised']}"
        ),
    },
]

document = {
    "label": (
        "P3 checkpoint topology court: sharded write, topology-independent restore, "
        "and a mounted-container loader. Single-host Docker court, synthetic-free "
        "public corpus, no fleet claim."
    ),
    "timestamp_utc": stamp,
    "court": {
        "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
        "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
        "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
        "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        "limit_note": "memory == swap, bounded /tmp, no-new-privileges, cap-drop=ALL plus /dev/fuse and SYS_ADMIN",
    },
    "revisions": {
        "weftfs": os.environ.get("WEFT_REPO_WEFTFS") or git(root),
        "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC")
        or git(os.path.join(root, "WeftCodec")),
        "weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO")
        or git(os.path.join(root, "WeftCodec-Go")),
        "weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO")
        or git(os.path.join(root, "WeftFS-Go")),
    },
    "corpus": {
        "path": corpus,
        "bytes": int(source_bytes),
        "sha256": loader["source_sha256"],
        "tensors": loader["tensor_count"],
        "note": (
            "Qwen/Qwen2.5-0.5B weights, BF16, cached under .docker-cache/real-corpus"
            " and re-hashed in the court."
        ),
    },
    "ingest": {
        "shards": write["shards"],
        "axis": write["axis"],
        "shard_strategy": write["shard_strategy"],
        "tensors": len(write["tensors"]),
        "shard_objects": write["shard_objects"],
        "element_bytes": write["total_element_bytes"],
        "elapsed_ms": write["elapsed_ms"],
        "peak_rss_kib": write["peak_rss_kib"],
        "commit": write["commit"],
        "manifest_root": write["manifest_root"],
        "physical_bytes": {
            "store_total": int(store_bytes),
            "objects_chunks": int(objects_bytes),
            "manifests_and_frames": int(manifests_bytes),
        },
        "ratio_to_source": round(int(store_bytes) / int(source_bytes), 6),
    },
    "full_restore_runs": [
        {
            "label": label,
            "topology": topology,
            "axis": write["axis"],
            "elapsed_ms": run["elapsed_ms"],
            "peak_rss_kib": run["peak_rss_kib"],
            "store_bytes_read": run["store_bytes_read"],
            "store_reads": run["store_reads"],
            "shards_read": run["shards_read"],
            "file_bytes": run["file_bytes"],
            "file_sha256": run["file_sha256"],
            "exact_source_file": run["exact_source_file"],
            "header_reproduced": run["header_reproduced"],
        }
        for label, topology, run in restore_runs
    ],
    "restore_topology8": restore8,
    "restore_topology4": restore4,
    "restore_topology4_warm": restore4_warm,
    "single_tensor_restore": single,
    "slice_restore": slice_report,
    "checkpoint_verify": verify,
    "mount_loader": loader,
    "mount_daemon_peak_rss_kib": {
        "before_loader": int(daemon_rss or 0),
        "after_loader": int(daemon_rss_after or 0),
    },
    "checks": checks,
    "all_checks_pass": all(check["ok"] for check in checks),
    "limitations": [
        "Mode-10 shard frames are not independently seekable: a region restore reads only intersecting shard objects, but decodes each intersecting shard's whole frame.",
        "The shard payload is carried as a q8_0 byte-word transport (exact); it is not a lossy quantization of the weights. Scale-plane metadata adds about 1.6% at block size 256.",
        "Restore topologies other than the stored one are re-layout round-trip proofs over the reassembled bytes; the storage topology itself is immutable.",
        "Restore spools the container to a host file and imports it with bounded streaming, so peak memory is one tensor, not the whole container.",
        "The full-container restore's SHA-256 equals the source because the exact source header is stored; a partial restore rebuilds the header and reports that explicitly.",
        "One host, warm page cache, local store; no network backend and no cross-machine claim.",
    ],
}

os.makedirs(os.path.dirname(receipt), exist_ok=True)
with open(receipt, "w", encoding="utf-8") as handle:
    json.dump(document, handle, indent=2, sort_keys=True)
    handle.write("\n")

print(json.dumps({"receipt": receipt, "all_checks_pass": document["all_checks_pass"]}, indent=2))
if not document["all_checks_pass"]:
    sys.exit(1)
PY

echo "checkpoint court: receipt $receipt"
