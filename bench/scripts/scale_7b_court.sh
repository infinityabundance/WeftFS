#!/usr/bin/env bash
# R12 increment 1 — 7B-scale court.
#
# Host orchestrator (like bench/scripts/shared_mounts_court.sh): every phase
# runs in its own OOM-protected bench container, so peak RSS is honest per
# phase and an out-of-memory kill cannot take the court down with it.
#
# Corpus: the real Qwen/Qwen2.5-7B-Instruct checkpoint (4 published shards
# merged into one container by bench/scripts/safetensors_merge.py, which
# verified every tensor against its source shard). The merged file's SHA-256 is
# checked here before any phase runs.
#
# Phases:
#   ingest      per-tensor planner ingest (whole-tensor bytes; 3 GiB court)
#   verify      verify --full of the ingest store
#   flat        8-way streaming checkpoint write, fast raw, flat
#   packed      the same write through the physical pack plane
#   restore1/4/8 full pipelined restore at 1, 4 and 8 workers
#
# Disk: the ingest store is deleted before the checkpoint stores are created,
# and the flat store before the packed one. The source container and one store
# are the widest overlap; headroom is recorded in the receipt.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
work="$root/.docker-cache/scale-7b-$stamp"
receipt="$root/bench/receipts/scale-7b-$stamp.json"
log="$root/bench/receipts/scale-7b-$stamp.log"
corpus="$root/.docker-cache/real-corpus/Qwen2.5-7B-Instruct.safetensors"
# Container-side paths: the repo is bind-mounted at /work.
work_c="/work/.docker-cache/scale-7b-$stamp"
corpus_c="/work/.docker-cache/real-corpus/$(basename "$corpus")"
corpus_sha="5527ae977f6b2a2fc0add1d76834f1300a607f79aa4bdce1457bd738523c1ef1"
memory="${WEFT_SCALE_MEMORY:-3g}"
mkdir -p "$work" "$root/bench/receipts"
chmod a+rwX "$work" "$root/bench/receipts" 2>/dev/null || true
: > "$log"

echo "scale_7b_court: stamp=$stamp memory=$memory" | tee -a "$log"

if [[ ! -f "$corpus" ]]; then
  echo "scale_7b_court: missing corpus $corpus" >&2
  exit 1
fi
actual_sha="$(sha256sum "$corpus" | awk '{print $1}')"
if [[ "$actual_sha" != "$corpus_sha" ]]; then
  echo "scale_7b_court: corpus sha256 mismatch (expected $corpus_sha, got $actual_sha)" >&2
  exit 1
fi
echo "scale_7b_court: verified $(basename "$corpus") sha256=$actual_sha size=$(stat -c %s "$corpus")" | tee -a "$log"

# One build, reused by every phase.
build_status=0
if ! scripts/docker-court.sh bench \
  "cd /work && cargo build --release --example scale_7b" >>"$log" 2>&1; then
  build_status=$?
fi
echo "scale_7b_court: build exit=$build_status" | tee -a "$log"
if [[ "$build_status" -ne 0 ]]; then
  echo "scale_7b_court: build failed; see $log" >&2
  exit 1
fi

bin="/work/.docker-cache/target-rust/release/examples/scale_7b"

# run_phase <name> <cmd>
# The harness writes its JSON to $work_c/<name>.json; the container's cgroup
# memory peak is captured beside it. The phase directory is widened so the host
# orchestrator can delete a store between phases (the container runs as uid 0
# with cap-drop=ALL, so files it creates are not host-user-writable).
run_phase() {
  local name="$1" cmd="$2"
  local phase_log="$work/$name.log"
  local peak_file="$work_c/$name.cgroup_peak"
  local status=0
  echo "=== phase $name ===" | tee -a "$log"
  echo "scale_7b_court: $cmd" | tee -a "$log"
  if scripts/docker-court.sh bench \
    "cd /work && set +e; $cmd; s=\$?; cat /sys/fs/cgroup/memory.peak > '$peak_file' 2>/dev/null || true; chmod -R a+rwX '$work_c' 2>/dev/null || true; exit \$s" \
    >"$phase_log" 2>&1; then
    status=0
  else
    status=$?
  fi
  chmod a+rw "$phase_log" "$peak_file" 2>/dev/null || true
  printf '%s\t%s\n' "$name" "$status" >> "$work/status.tsv"
  echo "scale_7b_court: phase $name exit=$status" | tee -a "$log"
  if [[ -f "$work/$name.json" ]]; then
    echo "scale_7b_court: phase $name wrote $name.json" | tee -a "$log"
  fi
  cat "$phase_log" >> "$log"
  return 0
}

source_args="--source $corpus_c --source-sha256 $corpus_sha"
pack_args="--pack --pack-catalog-bytes 67108864"

# Optional phase filter (space-separated names). Empty means every phase.
only="${WEFT_SCALE_PHASES:-}"
want() { [[ -z "$only" || " $only " == *" $1 "* ]]; }

# Record the exact court environment (image, revisions, kernel) for the receipt.
run_phase court_env "env | grep -E '^WEFT_(COURT|REPO)_' > $work_c/court.env"

# 1. Planned per-tensor ingest. This is the phase that needs whole tensor bytes.
if want ingest; then run_phase ingest "$bin ingest $source_args --store $work_c/ingest-store --fresh --out $work_c/ingest.json"; fi
# 2. Full verification of whatever the ingest published.
if want verify_ingest; then run_phase verify_ingest "$bin verify --store $work_c/ingest-store --out $work_c/verify_ingest.json"; fi

# Free the ingest store before the checkpoint writes.
rm -rf "$work/ingest-store"
sync

# 3. Eight-way streaming checkpoint write, flat.
if want flat; then run_phase flat "$bin checkpoint $source_args --store $work_c/flat-store --name qwen7b --shards 8 --axis 0 --write-policy fast --fast-codec raw --max-ingest-bytes 1610612736 --fresh --out $work_c/flat.json"; fi
if want verify_flat; then run_phase verify_flat "$bin verify --store $work_c/flat-store --out $work_c/verify_flat.json"; fi
if want flat || want verify_flat; then rm -rf "$work/flat-store"; sync; fi

# 4. The same write through the pack plane. Every later phase that reopens the
# store must name the same plane, or it would look for flat keys that a packed
# store does not have.
if want packed; then run_phase packed "$bin checkpoint $source_args --store $work_c/packed-store --name qwen7b --shards 8 --axis 0 --write-policy fast --fast-codec raw --max-ingest-bytes 1610612736 $pack_args --fresh --out $work_c/packed.json"; fi
if want verify_packed; then run_phase verify_packed "$bin verify $pack_args --store $work_c/packed-store --out $work_c/verify_packed.json"; fi

# 5. Full restores at 1, 4 and 8 workers, from the packed store.
for threads in 1 4 8; do
  if want "restore_$threads"; then run_phase "restore_$threads" "$bin restore $pack_args --store $work_c/packed-store --name qwen7b --threads $threads --expect-sha256 $corpus_sha --out $work_c/restore_$threads.json"; fi
done

# Delete the packed store only when this run created it.
if want packed || want verify_packed || want restore_1 || want restore_4 || want restore_8; then
  rm -rf "$work/packed-store"
  sync
fi

# ---------------------------------------------------------------------------
# Assemble the receipt on the host.
# ---------------------------------------------------------------------------
python3 - "$work" "$receipt" "$stamp" "$memory" "$corpus" "$corpus_sha" "$log" "$root" <<'PY'
import json
import os
import shutil
import sys

work, receipt, stamp, memory, corpus, corpus_sha, log, root = sys.argv[1:9]

court_env = {}
court_env_path = os.path.join(work, "court.env")
if os.path.exists(court_env_path):
    for line in open(court_env_path):
        if "=" in line:
            key, value = line.rstrip("\n").split("=", 1)
            court_env[key] = value

def read(name):
    path = os.path.join(work, name)
    if not os.path.exists(path):
        return None
    try:
        with open(path) as handle:
            return json.load(handle)
    except Exception:
        return None

def peak(name):
    path = os.path.join(work, name + ".cgroup_peak")
    try:
        with open(path) as handle:
            return int(handle.read().strip())
    except Exception:
        return None

status = {}
status_path = os.path.join(work, "status.tsv")
if os.path.exists(status_path):
    for line in open(status_path):
        parts = line.strip().split("\t")
        if len(parts) == 2:
            status[parts[0]] = int(parts[1])

phases = {}
for name in ["ingest", "verify_ingest", "flat", "verify_flat", "packed", "verify_packed",
             "restore_1", "restore_4", "restore_8"]:
    phases[name] = {
        "exit": status.get(name),
        "cgroup_memory_peak_bytes": peak(name),
        "report": read(name + ".json"),
    }

data = {
    "schema": "weft-scale-7b-court-v1",
    "generated_utc": stamp,
    "label": "real public checkpoint, single host, warm page cache; not a fleet/media/cloud claim",
    "corpus": {
        "repo": "Qwen/Qwen2.5-7B-Instruct",
        "file": os.path.basename(corpus),
        "size_bytes": os.path.getsize(corpus),
        "sha256": corpus_sha,
        "shards": 4,
        "merge_tool": "bench/scripts/safetensors_merge.py (per-tensor sha256 verified against each shard)",
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
        "tmpfs": "1g",
        "image": court_env.get("WEFT_COURT_IMAGE"),
        "image_id": court_env.get("WEFT_COURT_IMAGE_ID"),
        "kernel": court_env.get("WEFT_COURT_KERNEL"),
        "weftfs_revision": court_env.get("WEFT_REPO_WEFTFS"),
        "weftcodec_revision": court_env.get("WEFT_REPO_WEFTCODEC"),
    },
    "phases": phases,
    "disk_headroom_bytes_after": shutil.disk_usage(root).free,
    "log": os.path.relpath(log, root),
}

with open(receipt, "w") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")

# Print the summary table the report will quote.
def g(phase, *keys, default=None):
    report = phases.get(phase, {}).get("report") or {}
    value = report
    for key in keys:
        if not isinstance(value, dict) or key not in value:
            return default
        value = value[key]
    return value

print("phase|exit|wall_ms|MB/s|peak_rss_kib|cgroup_peak_bytes")
for name in ["ingest", "flat", "packed", "restore_1", "restore_4", "restore_8"]:
    print("{}|{}|{}|{}|{}|{}".format(
        name,
        phases[name]["exit"],
        g(name, "wall_ms"),
        g(name, "mb_per_s"),
        g(name, "peak_rss_kib"),
        phases[name]["cgroup_memory_peak_bytes"],
    ))
print("receipt:", receipt)
PY

echo "scale_7b_court: done -> $receipt" | tee -a "$log"
