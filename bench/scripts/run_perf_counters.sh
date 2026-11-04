#!/usr/bin/env bash
# Perf-counter and energy-availability probe.
#
# Run only inside the OOM-protected bench court:
#   scripts/docker-court.sh bench "bash bench/scripts/run_perf_counters.sh"
#
# This script does two separate things and never conflates them:
#   1. It attempts `perf stat` on the WeftCodec CLI. If the perf binary is
#      absent it installs Debian's linux-perf package inside the ephemeral
#      court container (no host package is changed) and records the exact
#      package/version. Counters are cycles/instructions/branches/cache, NOT
#      energy.
#   2. It probes RAPL `energy_uj`. If no readable file exists, it records the
#      blocker and reports no joule, watt, or energy number.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p bench/receipts
receipt="bench/receipts/perf-counters-${stamp}.json"
log="bench/receipts/perf-counters-${stamp}.log"
scratch="${WEFT_PERF_SCRATCH:-$root/.docker-cache/perf-counters/$stamp}"
mkdir -p "$scratch"

exec > >(tee -a "$log") 2>&1

echo "perf/energy probe: image=${WEFT_COURT_IMAGE:-unknown} kernel=$(uname -r)"

# ---- RAPL energy availability (the only energy source) --------------------
powercap_class=absent
if [ -d /sys/class/powercap ]; then powercap_class=present; fi
energy_files="$(find /sys/class/powercap -name energy_uj -type f 2>/dev/null || true)"
energy_count=0
readable_count=0
readable_paths=()
if [ -n "$energy_files" ]; then
  while IFS= read -r file; do
    [ -n "$file" ] || continue
    energy_count=$((energy_count + 1))
    if [ -r "$file" ]; then
      readable_count=$((readable_count + 1))
      readable_paths+=("$file")
    fi
  done <<<"$energy_files"
fi
rapl_blocker=""
if [ "$readable_count" -eq 0 ]; then
  if [ "$powercap_class" = absent ]; then
    rapl_blocker="no /sys/class/powercap in the court (sysfs powercap class absent)"
  elif [ "$energy_count" -eq 0 ]; then
    rapl_blocker="powercap class exists but no energy_uj file is visible in the container; /sys/devices/virtual/powercap is masked/empty inside Docker"
  else
    rapl_blocker="energy_uj files exist but none is readable by uid 0 with cap-drop=ALL"
  fi
fi
echo "RAPL powercap_class=$powercap_class energy_files=$energy_count readable=$readable_count blocker=${rapl_blocker:-none}"

# ---- perf binary and jq ---------------------------------------------------
perf_package=not-installed
perf_install=none
if ! command -v perf >/dev/null 2>&1 || ! command -v jq >/dev/null 2>&1; then
  echo "installing Debian linux-perf and jq inside the ephemeral bench court"
  mkdir -p /tmp/aptcache/archives/partial /tmp/aptlists/partial
  chmod -R 777 /tmp/aptcache /tmp/aptlists
  apt_opts=(-o APT::Sandbox::User=root -o Dir::Cache=/tmp/aptcache
            -o Dir::State::lists=/tmp/aptlists -o Dir::State::status=/var/lib/dpkg/status)
  if apt-get "${apt_opts[@]}" update -qq >/tmp/perf-apt.log 2>&1 &&
     apt-get "${apt_opts[@]}" install -y --no-install-recommends linux-perf jq >>/tmp/perf-apt.log 2>&1; then
    perf_install=apt-installed
    perf_package="$(dpkg-query -W -f='${Package} ${Version}' linux-perf 2>/dev/null || echo unknown)"
    jq_package="$(dpkg-query -W -f='${Package} ${Version}' jq 2>/dev/null || echo unknown)"
  else
    perf_install=failed
    jq_package=unavailable
    rapl_blocker="$rapl_blocker; perf/jq install failed"
  fi
fi
if command -v perf >/dev/null 2>&1; then
  perf_package="${perf_package:-$(perf --version | head -n 1)}"
  perf_version="$(perf --version | head -n 1)"
else
  perf_version="unavailable"
fi
jq_package="${jq_package:-$(jq --version 2>/dev/null || echo unavailable)}"
echo "PERF package=$perf_package install=$perf_install version=$perf_version jq=$jq_package"

# ---- fixture and counters -------------------------------------------------
if command -v python3 >/dev/null 2>&1; then
  python3 - "$scratch/input.bin" <<'PY'
import sys
path = sys.argv[1]
chunk = bytes((i * 37 + 11) & 0xFF for i in range(65536))
with open(path, "wb") as f:
    for _ in range(32):
        f.write(chunk)
PY
else
  head -c 2097152 /dev/urandom >"$scratch/input.bin"
fi
input_sha="$(sha256sum "$scratch/input.bin" | cut -d' ' -f1)"
input_size="$(stat -c %s "$scratch/input.bin")"
echo "fixture bytes=$input_size sha256=$input_sha"

(cd WeftCodec && cargo build --release --target-dir "$root/.docker-cache/target-weftcodec" --bin weftcodec)
codec="$root/.docker-cache/target-weftcodec/release/weftcodec"

CSV="$scratch/perf.csv"
: >"$CSV"
perf_reps="${WEFT_PERF_REPS:-3}"
events="cycles,instructions,branches,branch-misses,cache-references,cache-misses,context-switches,page-faults"

run_perf() {
  local label="$1"; shift
  local i out
  for ((i = 1; i <= perf_reps; i++)); do
    out="$scratch/perf-$label-$i.csv"
    if perf stat -x, -e "$events" -o "$out" -- "$@" >/dev/null 2>"$scratch/perf-$label-$i.err"; then
      while IFS=, read -r value unit event _rest; do
        [ -n "${event:-}" ] || continue
        case "$event" in
          cycles|instructions|branches|branch-misses|cache-references|cache-misses|context-switches|page-faults)
            printf '%s,%s,%s,%s,%s\n' "$label" "$i" "$event" "$value" "$unit" >>"$CSV"
            ;;
        esac
      done <"$out"
    else
      echo "PERF_RUN_FAILED label=$label rep=$i"
      printf '%s,%s,%s,%s,%s\n' "$label" "$i" "run-failed" "0" "" >>"$CSV"
    fi
  done
}

if command -v perf >/dev/null 2>&1; then
  run_perf encode_raw "$codec" encode --input "$scratch/input.bin" --output "$scratch/raw.frame" --mode raw
  run_perf decode_raw "$codec" decode --input "$scratch/raw.frame" --output "$scratch/raw.out"
  run_perf encode_zstd "$codec" encode --input "$scratch/input.bin" --output "$scratch/zstd.frame" --mode zstd
  run_perf decode_zstd "$codec" decode --input "$scratch/zstd.frame" --output "$scratch/zstd.out"
  cmp -s "$scratch/input.bin" "$scratch/raw.out" && echo "raw decode round-trip ok"
  cmp -s "$scratch/input.bin" "$scratch/zstd.out" && echo "zstd decode round-trip ok"
fi

COUNTERS_JSON="$(awk -F, 'NR>0 {printf "{\"label\":\"%s\",\"rep\":%d,\"event\":\"%s\",\"value\":\"%s\",\"unit\":\"%s\"}\n", $1,$2,$3,$4,$5}' "$CSV" | jq -s '.')"
ENERGY_READABLE=false
if [ "$readable_count" -gt 0 ]; then ENERGY_READABLE=true; fi

jq -n \
  --arg schema "weftfs.perf-counters.receipt/1" \
  --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg court "${WEFT_COURT_IMAGE:-unknown}" \
  --arg image_id "${WEFT_COURT_IMAGE_ID:-unknown}" \
  --arg kernel "$(uname -r)" \
  --arg perf_version "$perf_version" \
  --arg perf_package "$perf_package" \
  --arg perf_install "$perf_install" \
  --argjson perf_paranoid "$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo -1)" \
  --arg fixture_sha "$input_sha" \
  --argjson fixture_bytes "$input_size" \
  --argjson reps "${WEFT_PERF_REPS:-3}" \
  --argjson counters "$COUNTERS_JSON" \
  --arg powercap_class "$powercap_class" \
  --argjson energy_files "$energy_count" \
  --argjson energy_readable_files "$readable_count" \
  --arg rapl_blocker "$rapl_blocker" \
  --argjson energy_readable "$ENERGY_READABLE" \
  --arg log "$log" \
  --arg log_sha "$(sha256sum "$log" | cut -d' ' -f1)" \
  '{
    schema:$schema, generated_at:$generated_at,
    environment:{court:$court,image_id:$image_id,kernel:$kernel,
      limit_memory:"3g",limit_memory_swap:"3g",limit_cpus:"4",limit_pids:"1024"},
    fixture:{bytes:$fixture_bytes,sha256:$fixture_sha},
    perf:{version:$perf_version,package:$perf_package,install:$perf_install,
      perf_event_paranoid:$perf_paranoid,repetitions:$reps,
      events:"cycles,instructions,branches,branch-misses,cache-references,cache-misses,context-switches,page-faults",
      counters:$counters,
      claim:"hardware performance counters only; counters are not energy"},
    energy:{
      source:"RAPL /sys/class/powercap/*/energy_uj",
      powercap_class:$powercap_class,
      energy_uj_files:$energy_files,
      readable_energy_uj_files:$energy_readable_files,
      readable:$energy_readable,
      blocker:$rapl_blocker,
      claim:"no energy number is reported unless a readable energy_uj was sampled; CPU time is not energy"
    },
    evidence:{raw_log:$log,raw_log_sha256:$log_sha}
  }' >"$receipt"

echo "perf receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
