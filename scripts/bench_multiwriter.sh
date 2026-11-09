#!/usr/bin/env bash
# R4 multi-writer benchmark. Run in the bench court:
#   bash scripts/docker-court.sh bench "cd /work && bash scripts/bench_multiwriter.sh"
#
# Local backend: N = 1,2,4,8,16,32, disjoint and high contention.
# S3 backend: when WEFT_S3_ENDPOINT is exported (the multiwriter_s3_court sets
# it), one N=4 disjoint run against MinIO.
#
# Every row names its receipt. Nothing here is a fleet, cloud, media, or
# power-loss claim. Hard limits come from docker-court.sh.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="bench/receipts/bench-multiwriter-${stamp}.json"
log="bench/receipts/bench-multiwriter-${stamp}.log"
mkdir -p bench/receipts
: >"$log"
exec > >(tee -a "$log") 2>&1

NOW_NS=1700000000000000000
RUST_BIN=".docker-cache/target-rust/debug/examples/mw2_writer"
GO_BIN="/work/.docker-cache/weft-mw2-writer"
WEFT_BIN=".docker-cache/target-rust/debug/weftfs"

echo "== multiwriter benchmark =="
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"

cargo build --quiet --example mw2_writer
cargo build --quiet --bin weftfs
(cd WeftFS-Go && go build -o "$GO_BIN" ./cmd/mw2writer)

run_rust() { "$RUST_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }
run_go() { "$GO_BIN" "$@" --fixed-timestamp-ns "$NOW_NS"; }

rows_file="$(mktemp)"
printf '[]\n' >"$rows_file"

add_row() {
  python3 - "$rows_file" "$@" <<'PY'
import json, sys
path = sys.argv[1]
row = json.loads(sys.argv[2])
rows = json.load(open(path))
rows.append(row)
json.dump(rows, open(path, "w"), sort_keys=True)
PY
}

percentiles() {
  python3 - "$1" <<'PY'
import glob, sys
values = []
for path in glob.glob(sys.argv[1]):
    with open(path) as handle:
        for line in handle:
            line = line.strip()
            if line:
                values.append(int(line))
values.sort()
def pick(q):
    if not values:
        return 0.0
    index = min(len(values) - 1, int(round(q * (len(values) - 1))))
    return values[index] / 1e6
print(json.dumps({"samples": len(values), "p50_ms": pick(0.50),
                  "p95_ms": pick(0.95), "p99_ms": pick(0.99),
                  "max_ms": (values[-1] / 1e6) if values else 0.0}))
PY
}

run_config() {
  local language="$1" distribution="$2" n="$3" count="$4" backend="$5"
  local dir; dir="$(mktemp -d /tmp/weft-mw2-bench.XXXXXX)"
  local store_dir="$dir/store"
  mkdir -p "$store_dir"
  if [[ "$backend" == "local" ]]; then
    run_rust init --store "$store_dir" >/dev/null
  fi
  local latency_dir="$dir/latencies"
  mkdir -p "$latency_dir"
  local start_ns; start_ns="$(date +%s%N)"
  local pids=()
  for ((i = 0; i < n; i++)); do
    local writer
    if [[ "$distribution" == "hot" ]]; then
      writer="w$i"
    else
      writer="w$i"
    fi
    local args=(write --store "$store_dir" --writer "$writer" --prefix "$writer" --count "$count"
      --latencies-out "$latency_dir/$writer.lat" --prepare-latencies-out "$latency_dir/$writer.prep"
      --backend "$backend")
    if [[ "$distribution" == "hot" ]]; then
      args+=(--hot hot.bin)
    fi
    if [[ "$language" == "go" || ( "$language" == "mixed" && $((i % 2)) -eq 1 ) ]]; then
      run_go "${args[@]}" >"$dir/$writer.json" 2>"$dir/$writer.err" &
    else
      run_rust "${args[@]}" >"$dir/$writer.json" 2>"$dir/$writer.err" &
    fi
    pids+=("$!")
  done
  local failed=0
  for pid in "${pids[@]}"; do
    wait "$pid" || failed=$((failed + 1))
  done
  local end_ns; end_ns="$(date +%s%N)"
  local wall_ms=$(( (end_ns - start_ns) / 1000000 ))

  local verify_ok=false
  if [[ "$backend" == "local" ]]; then
    if "$WEFT_BIN" --store "$store_dir" verify --full >/dev/null 2>"$dir/verify.err"; then
      verify_ok=true
    fi
  elif [[ "$backend" == "s3" ]]; then
    if "$WEFT_BIN" --backend s3 --store "$store_dir" verify --full >/dev/null 2>"$dir/verify.err"; then
      verify_ok=true
    fi
  fi
  local generation
  if [[ "$backend" == "local" ]]; then
    generation="$(python3 - "$store_dir" <<'PY'
import json, subprocess, sys
out = subprocess.check_output([".docker-cache/target-rust/debug/weftfs", "--store", sys.argv[1], "--json", "info"], text=True)
print(json.loads(out).get("generation"))
PY
)"
  else
    generation="null"
  fi
  local summary
  summary="$(python3 - "$dir" "$language" "$distribution" "$n" "$count" "$wall_ms" "$generation" "$backend" "$failed" "$latency_dir" "$verify_ok" <<'PY'
import glob, json, os, sys
(root, language, distribution, n, count, wall_ms, generation, backend, failed, latency_dir, verify_ok) = sys.argv[1:]
rows = [json.load(open(path)) for path in sorted(glob.glob(os.path.join(root, "*.json")))]
committed = sum(r.get("committed", 0) for r in rows)
conflicts = sum(r.get("conflicts", 0) for r in rows)
attempts = sum(r.get("attempts", 0) for r in rows)
errors = [e for r in rows for e in r.get("errors", [])]
stats = {"language": language, "distribution": distribution, "n": int(n),
         "count_per_writer": int(count), "requested": int(n) * int(count),
         "processes": len(rows), "process_failures": int(failed),
         "committed": committed, "conflicts": conflicts, "attempts": attempts,
         "logical_bytes": sum(r.get("logical_bytes", 0) for r in rows),
         "errors": errors, "wall_ms": int(wall_ms),
         "throughput_tx_s": (committed / (int(wall_ms) / 1000.0)) if int(wall_ms) else 0.0,
         "retry_amplification": (attempts / committed) if committed else 0.0,
         "rss_sum_kib": sum(r.get("peak_rss_kib", 0) for r in rows),
         "cpu_user_ms": sum(r.get("cpu_user_ms", 0) for r in rows),
         "cpu_sys_ms": sum(r.get("cpu_sys_ms", 0) for r in rows),
         "backend_reads": sum(r.get("backend_reads", 0) for r in rows),
         "backend_writes": sum(r.get("backend_writes", 0) for r in rows),
         "bytes_read": sum(r.get("bytes_read", 0) for r in rows),
         "bytes_written": sum(r.get("bytes_written", 0) for r in rows),
         "generation": None if generation == "null" else int(generation),
         "backend": backend,
         "verify_full": verify_ok == "true"}
stats["logical_bytes_per_s"] = (
    stats["logical_bytes"] / (int(wall_ms) / 1000.0)) if int(wall_ms) else 0.0
values = []
for path in glob.glob(os.path.join(latency_dir, "*.lat")):
    with open(path) as handle:
        values.extend(int(line) for line in handle if line.strip())
values.sort()
def pick(values, q):
    if not values:
        return 0.0
    return values[min(len(values) - 1, int(round(q * (len(values) - 1))))] / 1e6
stats["latency_samples"] = len(values)
stats["p50_ms"] = pick(values, 0.50)
stats["p95_ms"] = pick(values, 0.95)
stats["p99_ms"] = pick(values, 0.99)
stats["max_ms"] = (values[-1] / 1e6) if values else 0.0
prepares = []
for path in glob.glob(os.path.join(latency_dir, "*.prep")):
    with open(path) as handle:
        prepares.extend(int(line) for line in handle if line.strip())
prepares.sort()
stats["prepare_samples"] = len(prepares)
stats["prepare_p50_ms"] = pick(prepares, 0.50)
stats["prepare_p95_ms"] = pick(prepares, 0.95)
stats["prepare_p99_ms"] = pick(prepares, 0.99)
print(json.dumps(stats, sort_keys=True))
PY
)"
  echo "$summary"
  add_row "$summary"
}

echo "== local disjoint =="
for n in 1 2 4 8 16 32; do
  if (( n <= 8 )); then count=60; else count=$(( 240 / n )); fi
  run_config rust disjoint "$n" "$count" local
done

echo "== local high contention =="
for n in 1 2 4 8 16 32; do
  run_config rust hot "$n" 10 local
done

echo "== mixed-language disjoint (N=8) =="
run_config mixed disjoint 8 30 local

if [[ -n "${WEFT_S3_ENDPOINT:-}" ]]; then
  echo "== MinIO disjoint (N=4) =="
  s3_store="/tmp/weft-mw2-s3-${stamp}"
  run_rust init --store "$s3_store" --backend s3 >/dev/null
  run_config rust disjoint 4 10 s3
else
  echo "== MinIO skipped: WEFT_S3_ENDPOINT is not set =="
fi

# ---------------------------------------------------------------------------
# Scaling table and receipt.
# ---------------------------------------------------------------------------
finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
log_sha="$(sha256sum "$log" | cut -d' ' -f1)"

python3 - "$rows_file" "$receipt" "$stamp" "$finished_at" "$log" "$log_sha" <<'PY'
import json, os, sys
rows_path, receipt, stamp, finished_at, log, log_sha = sys.argv[1:]
rows = json.load(open(rows_path))
disjoint = [r for r in rows if r["distribution"] == "disjoint" and r["backend"] == "local"]
hot = [r for r in rows if r["distribution"] == "hot"]
s3 = [r for r in rows if r["backend"] == "s3"]
base = next((r["throughput_tx_s"] for r in disjoint if r["n"] == 1), 0.0)
for row in rows:
    if row["distribution"] == "disjoint" and row["backend"] == "local":
        row["scaling_efficiency"] = (row["throughput_tx_s"] / (row["n"] * base)) if base else 0.0
    else:
        # No same-backend N=1 baseline was measured for this row, so it has no
        # scaling efficiency. Never divide an S3 row by the local N=1 number.
        row["scaling_efficiency"] = None
doc = {
  "schema": "weftfs.multiwriter.bench/1",
  "generated_at": finished_at,
  "stamp": stamp,
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
    "s3_endpoint": os.environ.get("WEFT_S3_ENDPOINT", ""),
  },
  "repositories": [
    {"name": "WeftFS", "revision": os.environ.get("WEFT_REPO_WEFTFS", "unknown")},
    {"name": "WeftFS-Go", "revision": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown")},
  ],
  "rows": rows,
  "scaling_efficiency_base": base,
  "scaling_note": "throughput(N)/(N*throughput(1)); N>32 is not extrapolated",
  "scope": {
    "checked": "process-level MW2 writers on one local store and one MinIO endpoint (tmpfs data)",
    "not_checked": "fleet/multi-node operation, physical media or power loss, linearizability proof, MW3",
  },
  "evidence": {"log": log, "log_sha256": log_sha},
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "rows": len(rows), "base_tx_s": base}))
PY

echo "receipt: $receipt"
sha256sum "$receipt" "$log"
