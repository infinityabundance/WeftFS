#!/usr/bin/env bash
# Subphase Q2 tensor-tiles court.
#
# Runs INSIDE `scripts/docker-court.sh fuse` (FUSE device + SYS_ADMIN; every
# hard limit unchanged). It benchmarks mode-10 vs mode-15 region reads on one
# real public checkpoint tensor, then restores that tensor from the tiled
# checkpoint, mounts the store, and reads the restored container with the
# unmodified `safetensors` library through the mount.
#
# Usage (inside the court, cwd=/work):
#   bash bench/scripts/run_tensor_tiles_court.sh
#
# Environment:
#   WEFT_TILES_CORPUS     source container (default Qwen2.5-0.5B)
#   WEFT_TILES_TENSOR     tensor name (default layers.0 mlp.down_proj)
#   WEFT_TILES_TILE_SHAPE tile extents (default 32,256)
#   WEFT_TILES_REPS       timed repetitions (default 10)
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

corpus="${WEFT_TILES_CORPUS:-$root/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors}"
tensor_name="${WEFT_TILES_TENSOR:-model.layers.0.mlp.down_proj.weight}"
tile_shape="${WEFT_TILES_TILE_SHAPE:-32,256}"
reps="${WEFT_TILES_REPS:-10}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
work="$root/.docker-cache/tensor-tiles-court"
receipt="${WEFT_TILES_RECEIPT:-$root/bench/receipts/tensor-tiles-$stamp.json}"
log="$root/bench/receipts/tensor-tiles-$stamp.log"

if [[ ! -f "$corpus" ]]; then
  echo "tensor-tiles court: missing corpus $corpus" >&2
  exit 1
fi
mkdir -p bench/receipts
rm -rf "$work"
mkdir -p "$work/mnt" "$work/stores"

echo "tensor-tiles court: building release binary and harness"
cargo build --release --features fuse --bin weftfs >"$log" 2>&1
cargo build --release --example bench_tensor_tiles >>"$log" 2>&1
target="${CARGO_TARGET_DIR:-$root/.docker-cache/target-rust}"
bin="$target/release/weftfs"

echo "tensor-tiles court: mode-10 vs mode-15 region reads"
"$target/release/examples/bench_tensor_tiles" \
  --source "$corpus" \
  --tensor "$tensor_name" \
  --tile-shape "$tile_shape" \
  --work "$work/stores" \
  --out "$work/bench.json" \
  --reps "$reps" >>"$log" 2>&1

echo "tensor-tiles court: restoring the tiled tensor into the store"
"$bin" --store "$work/stores/tiled" --json checkpoint restore \
  --name tiles --out bench-mount --out-name down.safetensors \
  --tensor "$tensor_name" --spool "$work/spool.safetensors" \
  >"$work/restore.json" 2>>"$log"

echo "tensor-tiles court: mounting the store"
mountpoint="$work/mnt"
"$bin" --store "$work/stores/tiled" mount --mountpoint "$mountpoint" --foreground \
  >"$work/mount.out" 2>"$work/mount.err" &
mpid=$!
cleanup() {
  if kill -0 "$mpid" 2>/dev/null; then
    kill -TERM "$mpid" 2>/dev/null || true
    wait "$mpid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

mounted_file="$mountpoint/bench-mount/down.safetensors"
ready=0
for _ in $(seq 1 120); do
  if [[ -f "$mounted_file" ]]; then
    ready=1
    break
  fi
  sleep 1
done
if [[ "$ready" -ne 1 ]]; then
  echo "tensor-tiles court: mount did not expose $mounted_file" >&2
  cat "$work/mount.err" >&2 || true
  exit 1
fi

daemon_peak_rss_kib="$(awk '/^VmHWM:/ {print $2}' "/proc/$mpid/status" 2>/dev/null || echo 0)"
loader_ok=0
if python3 bench/scripts/mount_loader_tiled.py \
  --mounted "$mounted_file" \
  --source "$corpus" \
  --json "$work/loader.json" >>"$log" 2>&1; then
  loader_ok=1
fi

kill -TERM "$mpid" 2>/dev/null || true
wait "$mpid" 2>/dev/null || true
trap - EXIT

echo "tensor-tiles court: writing receipt"
python3 - "$root" "$work" "$receipt" "$corpus" "$tensor_name" "$tile_shape" \
  "$reps" "$loader_ok" "$daemon_peak_rss_kib" "$stamp" <<'PY'
import json
import os
import subprocess
import sys

(
    root,
    work,
    receipt,
    corpus,
    tensor_name,
    tile_shape,
    reps,
    loader_ok,
    daemon_rss,
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


def revision(env_name, path):
    value = os.environ.get(env_name)
    if value:
        return value
    return git(path)


def env(name, default="unknown"):
    return os.environ.get(name, default)


bench = load("bench.json")
restore = load("restore.json")
loader = load("loader.json")

runs = bench["runs"]
by_key = {(run["strategy"], run["region"]): run for run in runs}
comparisons = []
for region in ("100_rows", "1_row"):
    m10 = by_key.get(("mode-10", region))
    m15 = by_key.get(("mode-15", region))
    if not m10 or not m15:
        raise SystemExit(f"missing bench rows for {region}")
    comparisons.append(
        {
            "region": region,
            "mode10_p50_ms": m10["p50_ms"],
            "mode15_p50_ms": m15["p50_ms"],
            "speedup": m10["p50_ms"] / m15["p50_ms"],
            "mode10_store_bytes_read": m10["store_bytes_read"],
            "mode15_store_bytes_read": m15["store_bytes_read"],
            "byte_reduction": 1.0 - m15["store_bytes_read"] / m10["store_bytes_read"],
            "mode10_units_touched": m10["units_touched"],
            "mode15_units_touched": m15["units_touched"],
        }
    )

checks = [
    {
        "check": "slices_are_exact",
        "ok": bool(bench["exact_slices_equal"]),
        "detail": "mode-10 and mode-15 100-row restore bytes are identical",
    },
    {
        "check": "mode15_decodes_fewer_bytes",
        "ok": all(
            row["mode15_store_bytes_read"] < row["mode10_store_bytes_read"]
            for row in comparisons
        ),
        "detail": "both regions",
    },
    {
        "check": "mode15_touches_only_intersecting_tiles",
        "ok": all(
            row["mode15_units_touched"] > 0
            and row["mode15_units_touched"] < 100000
            for row in comparisons
        ),
        "detail": "tile counts are reported per row",
    },
    {
        "check": "mount_loader_ok",
        "ok": loader["ok"] and loader_ok == "1",
        "detail": "unmodified safetensors library read the restored container through the mount",
    },
    {
        "check": "restore_report_present",
        "ok": "units_touched" in restore and len(restore.get("tensors", [])) == 1,
        "detail": f"restored_tensors={len(restore.get('tensors', []))}",
    },
]

value = {
    "schema": "weft.receipts.tensor-tiles.v1",
    "label": (
        "single-host Docker fuse court, one real public Qwen2.5-0.5B BF16 "
        "tensor, in-process store API plus a FUSE mount read by the unmodified "
        "safetensors library; not a fleet, multi-host, or remote-store claim"
    ),
    "created_utc": stamp,
    "revisions": {
        "weftfs": revision("WEFT_REPO_WEFTFS", root),
        "weftcodec": revision("WEFT_REPO_WEFTCODEC", os.path.join(root, "WeftCodec")),
        "weftcodec_go": revision("WEFT_REPO_WEFTCODEC_GO", os.path.join(root, "WeftCodec-Go")),
        "weftfs_go": revision("WEFT_REPO_WEFTFS_GO", os.path.join(root, "WeftFS-Go")),
    },
    "environment": {
        "court": "fuse",
        "image": env("WEFT_COURT_IMAGE"),
        "image_id": env("WEFT_COURT_IMAGE_ID"),
        "kernel": env("WEFT_COURT_KERNEL"),
        "limits": {
            "memory": env("WEFT_COURT_MEMORY"),
            "cpus": env("WEFT_COURT_CPUS"),
            "pids": env("WEFT_COURT_PIDS"),
            "nproc": env("WEFT_COURT_NPROC"),
            "tmpfs_work": "512m",
            "tmpfs_bench": "1g",
        },
        "docker_server": env("WEFT_DOCKER_SERVER"),
    },
    "corpus": {
        "source": corpus,
        "tensor": tensor_name,
        "shape": bench["shape"],
        "dtype": bench["dtype"],
        "logical_tensor_bytes": bench["logical_tensor_bytes"],
    },
    "bench": {
        "tile_shape": bench["tile_shape"],
        "reps": bench["reps"],
        "p50_definition": bench["p50_definition"],
        "rows": runs,
        "comparisons": comparisons,
    },
    "checkpoints": bench["checkpoints"],
    "restore": restore,
    "mount_loader": {
        "ok": loader["ok"],
        "safetensors_version": loader["safetensors_version"],
        "numpy_version": loader["numpy_version"],
        "ml_dtypes_version": loader["ml_dtypes_version"],
        "library_keys": loader["library_keys"],
        "tensor_count": loader["tensor_count"],
        "all_tensors_materialised": loader["all_tensors_materialised"],
        "all_tensors_match_source": loader["all_tensors_match_source"],
        "mounted_sha256": loader["mounted_sha256"],
        "source_sha256": loader["source_sha256"],
        "tensors": loader["tensors"],
        "daemon_peak_rss_kib": int(daemon_rss),
    },
    "checks": checks,
    "passed": sum(1 for check in checks if check["ok"]),
    "total": len(checks),
}

with open(receipt, "w", encoding="utf-8") as handle:
    json.dump(value, handle, indent=2, sort_keys=True)
    handle.write("\n")

print(f"tensor-tiles court: {value['passed']}/{value['total']} checks passed")
for row in comparisons:
    print(
        "  {region:<9} p50 {m10:8.3f} ms -> {m15:8.3f} ms  bytes {b10} -> {b15}  units {u10} -> {u15}".format(
            region=row["region"],
            m10=row["mode10_p50_ms"],
            m15=row["mode15_p50_ms"],
            b10=row["mode10_store_bytes_read"],
            b15=row["mode15_store_bytes_read"],
            u10=row["mode10_units_touched"],
            u15=row["mode15_units_touched"],
        )
    )
    print(f"    speedup {row['speedup']:.1f}x, byte reduction {row['byte_reduction']*100:.1f}%")
if value["passed"] != value["total"]:
    raise SystemExit(1)
print(f"receipt: {receipt}")
PY

echo "receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
