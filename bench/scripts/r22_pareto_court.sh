#!/usr/bin/env bash
# R22.4 Pareto court: the write frontier at the new operating point.
#
# Runs **inside** the bench court (`scripts/docker-court.sh bench "bash
# bench/scripts/r22_pareto_court.sh"`), so the hard limits are the usual ones:
# 3 GiB memory == swap, 1024 pids, 4 CPUs, bounded /tmp. Nothing is raised.
#
# The court records a **device control** first (`dd` of 200 MiB with `conv=fsync`,
# three times). The R22 write wall is durability-bound once the encode falls
# below publication, so the device's fsynced throughput is part of the
# measurement's environment and is published with every row. A run whose device
# control is far below the 1.2-1.3 GB/s the R21 court saw is labelled
# `degraded_device` in the receipt rather than silently compared with it.
#
# Rows (all 7 interleaved repetitions, one rotated order per rep):
#   A                          native per-tensor durable write, no WeftFS layer
#   C                          shipped sequential planned writer (R20 default)
#   C-legacy                   exhaustive planner (the R18 decision-diff control)
#   C-pipe4                    the shipped default: R21 pipeline + R22 defaults
#                              (chunk-parallel ZSTD where it has the CPUs,
#                              cpu_tokens = default_cpu_tokens(4) = 3)
#   C-pipe4-tokens1/2/4        CPU tokens (the default is 3 on four CPUs)
#   C-pipe2 / C-pipe8          encode workers (the default is 4)
#   C-pipe4-zpar1              the same writer with the sequential ZSTD encoder
#   C-pipe4-zpar2/3/4          chunk-parallel ZSTD threads inside one tensor
#   C-pipe4-sync16/32          early-barrier granularity (8 MiB is the default)
#   C-pipe4-pack1m/8m          the pack plane, re-measured at this operating point
#   C-pipe4-znochunk           one ZSTD frame per tensor
#   C-pipe4-zchunk16384        16 KiB codec chunks
#   C-pipe4-tm9off             two-stage planner without the mode-9 candidate
#   C-pipe4-t19off             two-stage planner without the level-19 candidate
#   C-pipe4-gate4              level-19 gate demands a 4 MiB predicted win
#   C-pipe4-leeway4            level-19 gate tolerates a 4 MiB predicted loss
#   C-pipe4-an1                sequential analyse histogram (R22.2 control)
#   E-pipe4                    planned writer + context/snapshot/provenance
#
# `WEFT_R22_STAMP` pins the receipt name, `WEFT_R22_REPS` defaults to 7 and
# `WEFT_R22_CONFIGS` overrides the row list.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_R22_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_R22_REPS:-7}"
configs="${WEFT_R22_CONFIGS:-A,C,C-legacy,C-pipe4,C-pipe4-tokens1,C-pipe4-tokens2,C-pipe4-tokens4,C-pipe4-zpar1,C-pipe2,C-pipe8,C-pipe4-zpar2,C-pipe4-zpar3,C-pipe4-zpar4,C-pipe4-sync16,C-pipe4-sync32,C-pipe4-pack1m,C-pipe4-pack8m,C-pipe4-znochunk,C-pipe4-zchunk16384,C-pipe4-tm9off,C-pipe4-t19off,C-pipe4-gate4,C-pipe4-leeway4,C-pipe4-an1,E-pipe4}"
fixture="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
receipt="$root/bench/receipts/r22-pareto-$stamp.json"
log="$root/bench/receipts/r22-pareto-$stamp.log"
work="$root/.docker-cache/r22-pareto-$stamp"
official="$root/bench/receipts/augmentation-local-$stamp.json"
device="$root/bench/receipts/r22-pareto-$stamp.device.json"

if [[ ! -f "$fixture" ]]; then
  echo "r22_pareto_court: missing fixture $fixture" >&2
  exit 1
fi
mkdir -p "$root/bench/receipts"
chmod a+rwX "$root/bench/receipts" 2>/dev/null || true

echo "== device control: 200 MiB with conv=fsync, three times =="
samples=()
for index in 1 2 3; do
  out="$(dd if=/dev/zero of="$root/.docker-cache/r22-dd-$stamp-$index" bs=1M count=200 conv=fsync 2>&1 | tail -1)"
  rm -f "$root/.docker-cache/r22-dd-$stamp-$index"
  echo "$out"
  mbps="$(printf '%s' "$out" | sed -n 's#.*, \([0-9.]*\) MB/s.*#\1#p')"
  gbps="$(printf '%s' "$out" | sed -n 's#.*, \([0-9.]*\) GB/s.*#\1#p')"
  if [[ -n "$gbps" ]]; then
    mbps="$(python3 -c "print(float('$gbps') * 1000.0)")"
  fi
  samples+=("${mbps:-0}")
done
python3 - "$device" "$stamp" "${samples[@]}" <<'PY'
import json, sys
path, stamp, *values = sys.argv[1:]
rates = [float(value) for value in values]
rates.sort()
median = rates[len(rates) // 2] if rates else 0.0
receipt = {
    "schema": "weft.r22-device-control.v1",
    "stamp": stamp,
    "operation": "dd if=/dev/zero bs=1M count=200 conv=fsync, three times, inside the bench court",
    "fsync_megabytes_per_second": rates,
    "median_megabytes_per_second": median,
    "clean_session_reference_megabytes_per_second": 1280.0,
    "regime": "degraded_device" if median < 600.0 else "clean",
    "note": "the R21 court's native control A (424 MB, 101 fsyncs) wrote in 332 ms, i.e. 1.28 GB/s; a median far below that means the durability path has degraded and every durable-write row in this session is device-bound",
}
with open(path, "w") as handle:
    json.dump(receipt, handle, indent=1, sort_keys=True)
    handle.write("\n")
print("r22_pareto_court: device", median, "MB/s", receipt["regime"])
PY

echo "== R22 Pareto matrix, configs=$configs, reps=$reps =="
official_args=()
if [[ -f "$official" ]]; then
  official_args=(--official-receipt "$official")
else
  echo "r22_pareto_court: warning: no contract-court receipt at $official; the identity check will be absent" >&2
fi
cargo run --release --quiet --example augmentation_bench -- ablate \
  --fixture "$fixture" \
  --work "$work" \
  --out "$receipt" \
  --log "$log" \
  --configs "$configs" \
  --reps "$reps" \
  --max-others 100 --include-embedding 1 \
  --backend local --chunk-size 1048576 --seed weft-augmentation-r3 \
  "${official_args[@]}" \
  --force

echo "r22_pareto_court: receipt $receipt"

# --- product-path verify: a store written with the new options, checked by the
# --- shipped CLI.
verify_work="$root/.docker-cache/r22-verify-$stamp"
rm -rf "$verify_work"
mkdir -p "$verify_work"
cargo run --release --quiet --example augmentation_bench -- prepare \
  --fixture "$fixture" \
  --work "$verify_work" \
  --max-others 12 --include-embedding 1 >/dev/null
cargo run --release --quiet --example augmentation_bench -- write \
  --work "$verify_work" \
  --subset "$verify_work/subset.json" \
  --config C-pipe4-zpar3 \
  --backend local \
  --phase-out "$verify_work/write.json" \
  --chunk-size 1048576 \
  --seed weft-augmentation-r3 >/dev/null
store="$verify_work/C-pipe4-zpar3/store"
echo "== verify --full over the chunk-parallel-written store $store =="
cargo run --release --quiet --bin weftfs -- --store "$store" verify --full
rm -rf "$verify_work"
echo "r22_pareto_court: verify --full ok"
