#!/usr/bin/env bash
# R22.5 cross-family court: the same write frontier on different real families.
#
# Runs **inside** the bench court (`scripts/docker-court.sh bench "bash
# bench/scripts/r22_family_court.sh"`), so the hard limits are the usual ones:
# 3 GiB memory == swap, 1024 pids, 4 CPUs, bounded /tmp. Nothing is raised.
#
# Families:
#   qwen0.5b-base       Qwen/Qwen2.5-0.5B          (BF16, the base checkpoint)
#   qwen0.5b-instruct   Qwen/Qwen2.5-0.5B-Instruct (the R3/R21 fixture)
#   pythia70m-step0     EleutherAI/pythia-70m step 0 (F32)
#   pythia70m-step8     EleutherAI/pythia-70m step 8 (F32)
#   qwen3b-base         Qwen/Qwen2.5-3B           (best effort: the per-tensor
#   qwen3b-instruct     Qwen/Qwen2.5-3B-Instruct   planner OOMs at 7B, so 3B is
#                                                  the largest family this court
#                                                  claims)
#
# Rows per family: A (native), C (sequential planned writer), C-pipe4 (tree
# default), C-pipe4-zpar3 (chunk-parallel ZSTD), C-legacy (exhaustive planner,
# the level-19 question: does level 19 still win where it used to?).
#
# Every row records wall, CPU, footprint, bytes written, strategies, peak RSS and
# cold restore. One repetition per (family, row): this is a **reported sweep**,
# not the 7-rep frontier court, and it is labelled as such in the receipt.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_R22_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
corpus="$root/.docker-cache/real-corpus"
rows="${WEFT_R22_FAMILY_ROWS:-A,C,C-pipe4,C-pipe4-zpar3,C-legacy}"
work="$root/.docker-cache/r22-family-$stamp"
out="$root/bench/receipts/r22-family-$stamp.json"
log="$root/bench/receipts/r22-family-$stamp.log"

mkdir -p "$root/bench/receipts"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true
rm -rf "$work"
mkdir -p "$work"

declare -A fixtures=(
  [qwen0.5b-base]="$corpus/Qwen2.5-0.5B.safetensors"
  [qwen0.5b-instruct]="$corpus/Qwen2.5-0.5B-Instruct.safetensors"
  [pythia70m-step0]="$corpus/pythia-70m-step0.safetensors"
  [pythia70m-step8]="$corpus/pythia-70m-step8.safetensors"
  [qwen3b-base]="$corpus/Qwen2.5-3B.safetensors"
  [qwen3b-instruct]="$corpus/Qwen2.5-3B-Instruct.safetensors"
)
# The 3B family gets a smaller subset: its per-tensor planned encode already
# peaks near the court's 3 GiB (measured 2,765,636 KiB on the full family in
# S12), so this court does not stack 100 extra tensors on top of it.
declare -A max_others=(
  [qwen0.5b-base]=100
  [qwen0.5b-instruct]=100
  [pythia70m-step0]=76
  [pythia70m-step8]=76
  [qwen3b-base]=24
  [qwen3b-instruct]=24
)
declare -A embeddings=(
  [qwen0.5b-base]=1
  [qwen0.5b-instruct]=1
  [pythia70m-step0]=0
  [pythia70m-step8]=0
  [qwen3b-base]=1
  [qwen3b-instruct]=1
)

results="$work/results.jsonl"
: >"$results"

families="${WEFT_R22_FAMILIES:-qwen0.5b-base qwen0.5b-instruct pythia70m-step0 pythia70m-step8 qwen3b-base qwen3b-instruct}"
for family in $families; do
  fixture="${fixtures[$family]:-}"
  if [[ -z "$fixture" || ! -f "$fixture" ]]; then
    echo "r22_family_court: $family OPEN (fixture not present)"
    python3 - "$results" "$family" <<'PY'
import json, sys
path, family = sys.argv[1:]
with open(path, "a") as handle:
    handle.write(json.dumps({"family": family, "status": "open", "reason": "fixture not present in .docker-cache/real-corpus"}) + "\n")
PY
    continue
  fi
  dir="$work/$family"
  mkdir -p "$dir"
  echo "== family $family =="
  # The court pins the exact container it measured: the harness refuses a
  # fixture whose SHA-256 differs from the value it is given.
  # Streamed: a whole-file read of the 6.2 GB 3B container would be an OOM in a
  # 3 GiB court (measured: the container is killed with no receipt).
  sha="$(python3 -c "
import hashlib, sys
digest = hashlib.sha256()
with open(sys.argv[1], 'rb') as handle:
    while True:
        block = handle.read(1 << 20)
        if not block:
            break
        digest.update(block)
print(digest.hexdigest())
" "$fixture")"
  echo "   fixture sha256 $sha"
  if ! cargo run --release --quiet --example augmentation_bench -- prepare \
      --fixture "$fixture" \
      --fixture-sha256 "$sha" \
      --work "$dir" \
      --max-others "${max_others[$family]}" --include-embedding "${embeddings[$family]}" >"$dir/prepare.log" 2>&1; then
    echo "r22_family_court: $family OPEN (prepare failed; see log)"
    python3 - "$results" "$family" <<'PY'
import json, sys
path, family = sys.argv[1:]
with open(path, "a") as handle:
    handle.write(json.dumps({"family": family, "status": "open", "reason": "prepare failed inside the court"}) + "\n")
PY
    continue
  fi
  for config in ${rows//,/ }; do
    echo "-- $family $config"
    rowdir="$dir/$config"
    mkdir -p "$rowdir/phases"
    if ! cargo run --release --quiet --example augmentation_bench -- write \
        --work "$rowdir" \
        --subset "$dir/subset.json" \
        --config "$config" \
        --backend local \
        --phase-out "$rowdir/phases/$config-write.json" \
        --chunk-size 1048576 \
        --seed weft-augmentation-r3 \
        --force >>"$log" 2>&1; then
      echo "r22_family_court: $family $config OPEN (write failed; see log)"
      continue
    fi
    if ! cargo run --release --quiet --example augmentation_bench -- restore \
        --work "$rowdir" \
        --subset "$dir/subset.json" \
        --config "$config" \
        --phase-out "$rowdir/phases/$config-restore.json" >>"$log" 2>&1; then
      echo "r22_family_court: $family $config restore failed (see log)"
    fi
    python3 - "$results" "$family" "$config" "$rowdir/phases/$config-write.json" "$rowdir/phases/$config-restore.json" "$sha" <<'PY'
import json, os, sys
path, family, config, write_path, restore_path, sha = sys.argv[1:]
with open(write_path) as handle:
    write = json.load(handle)
restore = None
if os.path.exists(restore_path):
    with open(restore_path) as handle:
        restore = json.load(handle)
pipeline = write.get("pipeline") or {}
barrier = pipeline.get("early_barrier") or {}
ledger = write.get("ledger") or {}
row = {
    "family": family,
    "config": config,
    "fixture_sha256": sha,
    "status": "measured",
    "wall_ms": write.get("wall_ms"),
    "cpu_ms": write.get("cpu_user_sys_ms"),
    "peak_rss_kib": write.get("peak_rss_kib"),
    "physical_footprint_bytes": (write.get("storage") or {}).get("physical_footprint_bytes"),
    "bytes_written": write.get("physical_bytes_written"),
    "representations": write.get("representations"),
    "tensors": (write.get("subset") or {}).get("tensors"),
    "logical_bytes": (write.get("subset") or {}).get("logical_bytes"),
    "writeback_busy_ms": barrier.get("busy_ms"),
    "stages_ms": pipeline.get("stages_ms") or {},
    "prepare_breakdown": ledger.get("prepare_breakdown") or {},
    "planner": ledger.get("planner") or {},
    "restore": None if restore is None else {
        "wall_ms": (restore.get("cold") or {}).get("wall_ms"),
        "exact": (restore.get("exactness") or {}).get("match"),
        "mismatches": (restore.get("exactness") or {}).get("mismatches"),
        "tensors_verified": (restore.get("exactness") or {}).get("tensors_verified"),
    },
}
with open(path, "a") as handle:
    handle.write(json.dumps(row) + "\n")
PY
    rm -rf "$rowdir"/*/store
  done
  rm -rf "$dir"
done

python3 - "$results" "$out" "$stamp" <<'PY'
import json, sys
source, target, stamp = sys.argv[1:]
rows = []
with open(source) as handle:
    for line in handle:
        line = line.strip()
        if line:
            rows.append(json.loads(line))
receipt = {
    "schema": "weft.r22-family.v1",
    "stamp": stamp,
    "role": "R22.5 cross-family write frontier: one repetition per (family, row), same fixture selection rule as the R21 court, local POSIX backend",
    "methodology": "bench/scripts/r22_family_court.sh inside the bench court (3 GiB / 4 cpus / 1024 pids); subset = embedding + up to 100 evenly spaced tensors; exactness checked by the restore row",
    "rows": rows,
    "result": "measured",
}
with open(target, "w") as handle:
    json.dump(receipt, handle, indent=1, sort_keys=True)
    handle.write("\n")
print("r22_family_court: receipt", target)
PY
