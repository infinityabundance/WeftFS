#!/usr/bin/env bash
# R12 increment 2 — fleet cold-start court.
#
# Host orchestrator. Every phase runs in the OOM-protected bench court; the host
# only starts the containers, verifies the MinIO binary, and assembles the
# receipt.
#
# One real MinIO server on loopback holds one real S3-backed store of the real
# SmolLM-135M-Instruct checkpoint (272 BF16 tensors, 269 MB). Two configurations
# run the same fleet over the same store:
#
#   with      one shared read-only server, N FUSE views, single-flight + the
#             decoded-object cache enabled
#   without   the same, with both disabled (the control)
#
# N replica processes each read every tensor through their own view. The server
# reports backend requests/bytes, decode calls and the two read-layer counters.
#
# Usage:
#   scripts/fleet_coldstart_court.sh [N]
#
# Environment:
#   WEFT_FLEET_N        replicas (default 100)
#   WEFT_FLEET_MEMORY   court memory limit (default 8g; recorded, never hidden)
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

n="${1:-${WEFT_FLEET_N:-100}}"
memory="${WEFT_FLEET_MEMORY:-8g}"
if [[ ! "$n" =~ ^[0-9]+$ ]] || [[ "$n" -lt 1 ]]; then
  echo "fleet_coldstart_court: N must be a positive integer, got '$n'" >&2
  exit 64
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
work="$root/.docker-cache/fleet-court-$stamp"
receipt="$root/bench/receipts/fleet-coldstart-$stamp.json"
log="$root/bench/receipts/fleet-coldstart-$stamp.log"
corpus="$root/.docker-cache/real-corpus/SmolLM-135M-Instruct.safetensors"
corpus_sha="c1464c9ebccc6fb497aa4ccc8d74d29b33f9a9eddc4638a4575719357f532f2d"
minio_release="RELEASE.2025-04-22T22-12-26Z"
minio_sha256="53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c"
minio_bin="$root/.docker-cache/minio/minio"

mkdir -p "$work" "$root/bench/receipts" "$root/bench/bundles"
chmod a+rwX "$work" "$root/bench/receipts" 2>/dev/null || true
: >"$log"
echo "fleet_coldstart_court: stamp=$stamp N=$n memory=$memory" | tee -a "$log"

# --- corpus -----------------------------------------------------------------
if [[ ! -f "$corpus" ]]; then
  echo "fleet_coldstart_court: missing corpus $corpus" >&2
  exit 1
fi
actual="$(sha256sum "$corpus" | awk '{print $1}')"
if [[ "$actual" != "$corpus_sha" ]]; then
  echo "fleet_coldstart_court: corpus sha256 mismatch ($actual != $corpus_sha)" >&2
  exit 1
fi
echo "fleet_coldstart_court: verified $(basename "$corpus") sha256=$actual" | tee -a "$log"

# --- MinIO binary -----------------------------------------------------------
mkdir -p "$(dirname "$minio_bin")"
if [[ ! -f "$minio_bin" ]]; then
  echo "fleet_coldstart_court: downloading MinIO $minio_release" | tee -a "$log"
  if ! curl -fL --retry 3 -o "$minio_bin.part" \
    "https://github.com/minio/minio/releases/download/${minio_release}/minio.linux-amd64.${minio_release}"; then
    rm -f "$minio_bin.part"
    echo "fleet_coldstart_court: cannot obtain the pinned MinIO release" >&2
    exit 78
  fi
  mv "$minio_bin.part" "$minio_bin"
fi
chmod +x "$minio_bin"
minio_actual="$(sha256sum "$minio_bin" | awk '{print $1}')"
if [[ "$minio_actual" != "$minio_sha256" ]]; then
  echo "fleet_coldstart_court: MinIO sha256 mismatch ($minio_actual != $minio_sha256)" >&2
  exit 1
fi
minio_version="$("$minio_bin" --version | head -1)"
echo "fleet_coldstart_court: MinIO $minio_version sha256=$minio_actual" | tee -a "$log"

# --- build ------------------------------------------------------------------
build_status=0
if ! scripts/docker-court.sh bench \
  "cd /work && cargo build --release --features fuse --example fleet_coldstart --example scale_7b" \
  >>"$log" 2>&1; then
  build_status=$?
fi
echo "fleet_coldstart_court: build exit=$build_status" | tee -a "$log"
if [[ "$build_status" -ne 0 ]]; then
  echo "fleet_coldstart_court: build failed; see $log" >&2
  exit 1
fi

# --- phases -----------------------------------------------------------------
# run_phase <phase> <memory>
# The inner script writes its evidence under $work/<phase>/.
run_phase() {
  local phase="$1" limit="$2"
  local phase_log="$work/$phase.log"
  local status=0
  echo "=== phase $phase (memory=$limit) ===" | tee -a "$log"
  if WEFT_MEMORY="$limit" scripts/docker-court.sh bench \
    "cd /work && bash bench/scripts/fleet_coldstart_inner.sh $phase $n '/work/.docker-cache/fleet-court-$stamp' '$stamp'" \
    >"$phase_log" 2>&1; then
    status=0
  else
    status=$?
  fi
  chmod -R a+rwX "$work" 2>/dev/null || true
  printf '%s\t%s\t%s\n' "$phase" "$status" "$limit" >>"$work/status.tsv"
  echo "fleet_coldstart_court: phase $phase exit=$status" | tee -a "$log"
  tail -5 "$phase_log" | tee -a "$log"
  return 0
}

run_phase build "$memory"
run_phase with "$memory"
run_phase without "$memory"

# --- receipt ----------------------------------------------------------------
python3 - "$work" "$receipt" "$n" "$memory" "$stamp" "$corpus" "$corpus_sha" \
  "$minio_release" "$minio_actual" "$minio_version" "$log" "$root" <<'PY'
import json
import os
import statistics
import sys

(work, receipt, n, memory, stamp, corpus, corpus_sha, minio_release, minio_sha,
 minio_version, log, root) = sys.argv[1:13]
n = int(n)

def read_json(path):
    try:
        with open(path) as handle:
            return json.load(handle)
    except Exception:
        return None

def read_events(path):
    out = {}
    try:
        for line in open(path):
            if "=" in line:
                key, value = line.strip().split("=", 1)
                out[key] = value
    except Exception:
        pass
    return out

status = {}
status_path = os.path.join(work, "status.tsv")
if os.path.exists(status_path):
    for line in open(status_path):
        parts = line.strip().split("\t")
        if len(parts) == 3:
            status[parts[0]] = {"exit": int(parts[1]), "memory": parts[2]}

court_env = {}
env_path = os.path.join(work, "build", "env.txt")
if os.path.exists(env_path):
    for line in open(env_path):
        if "=" in line:
            key, value = line.rstrip("\n").split("=", 1)
            court_env[key] = value

def pct(values, p):
    if not values:
        return None
    values = sorted(values)
    index = min(len(values) - 1, int(round((p / 100.0) * (len(values) - 1))))
    return values[index]

configs = {}
for cfg in ("with", "without"):
    base = os.path.join(work, cfg)
    server = read_json(os.path.join(base, "server.json"))
    replicas = []
    repl_dir = os.path.join(base, "replicas")
    if os.path.isdir(repl_dir):
        for name in sorted(os.listdir(repl_dir)):
            if name.startswith("r") and name.endswith(".json"):
                data = read_json(os.path.join(repl_dir, name))
                if data:
                    replicas.append(data)
    events = read_events(os.path.join(base, "events.txt"))
    first = [r["time_to_first_tensor_ms"] for r in replicas]
    total = [r["total_ms"] for r in replicas]
    rss = [r["peak_rss_kib"] for r in replicas]
    digests = sorted({r["bytes_digest"] for r in replicas})
    done_ns = [r["done_ns"] for r in replicas]
    ready_ns = server.get("ready_ns") if server else None
    wall_ms = ((max(done_ns) - ready_ns) / 1e6) if (done_ns and ready_ns) else None
    configs[cfg] = {
        "exit": status.get(cfg, {}).get("exit"),
        "memory_limit": status.get(cfg, {}).get("memory"),
        "replicas_expected": n,
        "replicas_completed": len(replicas),
        "server": server,
        "server_peak_rss_kib": server.get("peak_rss_kib") if server else None,
        "replica_time_to_first_ms": {
            "p50": pct(first, 50), "p95": pct(first, 95), "max": max(first) if first else None,
            "min": min(first) if first else None,
        },
        "replica_total_ms": {
            "p50": pct(total, 50), "p95": pct(total, 95), "max": max(total) if total else None,
        },
        "wall_to_all_ready_ms": wall_ms,
        "replica_rss_kib": {
            "max": max(rss) if rss else None,
            "sum": sum(rss) if rss else None,
        },
        "replica_tensors": sorted({r["tensors"] for r in replicas}),
        "replica_bytes": sorted({r["bytes"] for r in replicas}),
        "replica_bytes_digests": digests,
        "replica_bytes_digest_agrees": len(digests) <= 1,
        "cgroup": {
            "memory_peak_bytes": int(events["memory.peak"]) if events.get("memory.peak", "").isdigit() else events.get("memory.peak"),
            "pids_peak": int(events["pids.peak"]) if events.get("pids.peak", "").isdigit() else events.get("pids.peak"),
            "memory_events": {},
            "pids_events": {},
        },
    }
    # memory.events / pids.events blocks are multi-line; re-read them.
    try:
        lines = open(os.path.join(base, "events.txt")).read().splitlines()
        section = None
        for line in lines:
            if line.startswith("memory.events"):
                section = "memory_events"
                continue
            if line.startswith("pids.events"):
                section = "pids_events"
                continue
            if line.startswith("pids.peak") or line.startswith("memory.peak"):
                section = None
                continue
            if section and " " in line:
                key, value = line.split()
                configs[cfg]["cgroup"][section][key] = int(value)
    except Exception:
        pass

data = {
    "schema": "weft-fleet-coldstart-court-v1",
    "generated_utc": stamp,
    "label": "one host, N processes, loopback MinIO; not a fleet, multi-host or cloud claim",
    "replicas": n,
    "corpus": {
        "repo": "HuggingFaceTB/SmolLM-135M-Instruct",
        "dtype": "BF16",
        "tensors": 272,
        "size_bytes": os.path.getsize(corpus),
        "sha256": corpus_sha,
        "source": os.path.basename(corpus),
    },
    "backend": {
        "product": "MinIO",
        "release": minio_release,
        "version_line": minio_version,
        "binary_sha256": minio_sha,
        "started_by": "bench/scripts/fleet_coldstart_inner.sh inside the bench court",
        "data_dir": "a directory under the court's /work bind mount (not tmpfs)",
    },
    "court": {
        "kind": "bench",
        "memory": memory,
        "memory_swap": memory,
        "cpus": 4,
        "pids": 1024,
        "nproc": 2048,
        "cap_drop": "ALL",
        "no_new_privileges": True,
        "image": court_env.get("WEFT_COURT_IMAGE"),
        "image_id": court_env.get("WEFT_COURT_IMAGE_ID"),
        "kernel": court_env.get("WEFT_COURT_KERNEL"),
        "weftfs_revision": court_env.get("WEFT_REPO_WEFTFS"),
    },
    "configurations": configs,
    "log": os.path.relpath(log, root),
}

with open(receipt, "w") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")

print("config|replicas|backend_reads|backend_bytes|decode_calls|first_p50|first_p95|first_max|wall_all_ms|rss_sum_kib|cgroup_peak")
for cfg in ("with", "without"):
    c = configs[cfg]
    server = c["server"] or {}
    backend = server.get("backend", {})
    print("{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}".format(
        cfg, c["replicas_completed"],
        backend.get("reads"), backend.get("bytes_read"),
        server.get("decode_calls"),
        c["replica_time_to_first_ms"]["p50"], c["replica_time_to_first_ms"]["p95"],
        c["replica_time_to_first_ms"]["max"], c["wall_to_all_ready_ms"],
        c["replica_rss_kib"]["sum"], c["cgroup"]["memory_peak_bytes"],
    ))
print("receipt:", receipt)
PY

echo "fleet_coldstart_court: done -> $receipt" | tee -a "$log"
