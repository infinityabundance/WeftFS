#!/usr/bin/env bash
# R22 probe driver: one container per row, with a measured quiescence gate.
#
# The probe answers "what binds the write wall now?". A row is only meaningful
# if the host is quiet, so this driver:
#
#   1. runs the **native control** (A: the same backend's own durable per-tensor
#      write, no WeftFS layer) first and refuses to continue unless it lands in
#      `300..420 ms` (the R3/R21 measured band is 326-365 ms). A load average or
#      a free-memory reading is not evidence that a timing court can run; A is;
#   2. runs `sync` before every row, and
#   3. gives every row its own container, so a row cannot inherit the previous
#      row's unflushed pages.
#
# One repetition per row: this is a **probe**, not the cited sweep.
#
# Usage:
#   WEFT_R22_PROBE_STAMP=<stamp> bench/scripts/r22_probe_court.sh
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

stamp="${WEFT_R22_PROBE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
dir="bench/receipts/r22-probe-$stamp"


scripts/docker-court.sh bench "bash bench/scripts/r22_probe.sh $stamp --prepare"

row() {
  local name="$1"
  shift
  sync
  echo "-- probe row $name"
  scripts/docker-court.sh bench "bash bench/scripts/r22_probe.sh $stamp --row $name $*"
}

CONFIG=A row A
wall="$(python3 -c "import json;print(json.load(open('$dir/A.json'))['wall_ms'])")"
ok="$(python3 -c "print(1 if 300 <= $wall <= 420 else 0)")"
if [[ "$ok" != "1" ]]; then
  echo "r22_probe_court: native control A wrote in ${wall} ms, outside the 300-420 ms band; the host is contended and no row is timed" >&2
  exit 75
fi
echo "r22_probe_court: native control A = ${wall} ms (in band), timing the rows"

# The reference point: the new encode, shipped budgets, 4 workers.
row base
# Smaller in-flight windows: does fewer resident frames reduce writeback cost?
row win64 --pipeline-staging-bytes 67108864 --pipeline-encode-bytes 67108864 \
  --pipeline-publish-bytes 67108864 --pipeline-worker-bytes 134217728
# The whole group on one barrier (the early barrier removed).
row nosync --pipeline-sync-bytes 0
# Coarser early-barrier granularity.
row sync32 --pipeline-sync-bytes 33554432
# Three CPU tokens (R21's measured best) at the new encode.
row tokens3 --pipeline-cpu-tokens 3
# Two workers: is publication now the only thing that matters?
row pipe2 --pipeline-workers 2
# The pack plane at the new operating point: fewer files to fsync.
CONFIG=C-pipe4-zpar3-pack1m row pack1m
CONFIG=C-pipe4-zpar3-pack8m row pack8m

python3 - "$dir" "$stamp" <<'PY'
import json, os, sys
directory, stamp = sys.argv[1:]
rows = []
for name in sorted(os.listdir(directory)):
    if not name.endswith(".json") or name == "probe.json":
        continue
    with open(os.path.join(directory, name)) as handle:
        rows.append(json.load(handle))
rows.sort(key=lambda row: ["A", "base", "win64", "nosync", "sync32", "tokens3", "pipe2", "pack1m", "pack8m"].index(row["row"])
          if row["row"] in ["A", "base", "win64", "nosync", "sync32", "tokens3", "pipe2", "pack1m", "pack8m"] else 99)
receipt = {
    "schema": "weft.r22-probe.v1",
    "stamp": stamp,
    "role": "R22 probe: one repetition per row in its own container, to locate the binding constraint. NOT the cited sweep.",
    "methodology": "bench/scripts/r22_probe_court.sh: native control A in band before any row, host sync before every row, one container per row, same fixture/seed/subset as the R21 court",
    "fixture": "/work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors",
    "rows": rows,
    "result": "probe",
}
path = os.path.join(directory, "probe.json")
with open(path, "w") as handle:
    json.dump(receipt, handle, indent=1, sort_keys=True)
    handle.write("\n")
print("r22_probe_court: receipt", path)
PY
