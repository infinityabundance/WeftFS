#!/usr/bin/env bash
# T020/T021/T027 courts. Run in the bench court:
#
#   bash scripts/docker-court.sh bench "cd /work && bash scripts/mw3_court.sh"
#
# Produces three receipts under bench/receipts/:
#   mw3-<stamp>.json           MW3 disjoint/conflict scaling, process-per-writer
#   mw2-retry-<stamp>.json     T021 before/after on MW2 N=32 (two binaries)
#   writer-scale-<stamp>.json  T027 compact-client envelope at 32..1024 writers
#
# Environment:
#   WEFT_MW3_PROFILE=full|reduced   (default full)
#   WEFT_MW3_BEFORE=<dir>           a checked-out baseline tree for the T021
#                                   before row; omitted means the after row only
#                                   and the receipt says so.
#
# Hard limits come from docker-court.sh; nothing here raises them. No fleet,
# cloud, media, or power-loss claim.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
profile="${WEFT_MW3_PROFILE:-full}"
receipt_dir="bench/receipts"
mkdir -p "$receipt_dir"
mw3_receipt="$receipt_dir/mw3-${stamp}.json"
retry_receipt="$receipt_dir/mw2-retry-${stamp}.json"
scale_receipt="$receipt_dir/writer-scale-${stamp}.json"
log="$receipt_dir/mw3-${stamp}.log"
: >"$log"
exec > >(tee -a "$log") 2>&1

NOW_NS=1700000000000000000
RUST_BIN=".docker-cache/target-rust/release/examples/mw2_writer"
COMPACT_BIN=".docker-cache/target-rust/release/examples/mw3_compact"
WEFT_BIN=".docker-cache/target-rust/release/weftfs"

echo "== MW3 / T021 / T027 courts =="
echo "profile: $profile"
echo "kernel: $(uname -a)"
echo "image: ${WEFT_COURT_IMAGE:-unknown} (${WEFT_COURT_IMAGE_ID:-unknown})"
echo "limits: memory=${WEFT_COURT_MEMORY:-?} cpus=${WEFT_COURT_CPUS:-?} pids=${WEFT_COURT_PIDS:-?}"

cargo build --quiet --release --example mw2_writer --example mw3_compact --bin weftfs

if [[ "$profile" == "reduced" ]]; then
  DISJOINT_NS=(1 4 32)
  CONFLICT_NS=(8 32)
  SCALE_NS=(8 32 256 1024)
else
  DISJOINT_NS=(1 2 4 8 16 32 64)
  CONFLICT_NS=(2 8 32)
  SCALE_NS=(8 16 32 64 128 256 512 1024)
fi

work="$(mktemp -d /tmp/weft-mw3.XXXXXX)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

# ---------------------------------------------------------------------------
# MW3: per-domain registration and disjoint/conflict writers.
# ---------------------------------------------------------------------------
echo "== MW3 =="
run_mw3_config() {
  local distribution="$1" n="$2" count="$3"
  local dir="$work/mw3-$distribution-$n"
  mkdir -p "$dir/store" "$dir/latencies"
  "$RUST_BIN" init --store "$dir/store" --fixed-timestamp-ns "$NOW_NS" >/dev/null
  local -a domains=()
  for ((i = 0; i < n; i++)); do
    local id
    id="$("$RUST_BIN" register --store "$dir/store" --name "bench-$i" \
      --kind checkpoint_family --fixed-timestamp-ns "$NOW_NS" --plain)"
    domains+=("$id")
  done
  # The conflict case uses every writer in ONE domain.
  if [[ "$distribution" == "same-domain" ]]; then
    local shared
    shared="$("$RUST_BIN" register --store "$dir/store" --name hot --kind namespace \
      --fixed-timestamp-ns "$NOW_NS" --plain)"
    domains=()
    for ((i = 0; i < n; i++)); do domains+=("$shared"); done
  fi
  local start_ns; start_ns="$(date +%s%N)"
  local pids=()
  for ((i = 0; i < n; i++)); do
    local args=(write --store "$dir/store" --writer "w$i" --prefix "w$i" --count "$count"
      --latencies-out "$dir/latencies/w$i.lat" --prepare-latencies-out "$dir/latencies/w$i.prep"
      --domain "${domains[$i]}" --fixed-timestamp-ns "$NOW_NS")
    if [[ "$distribution" == "same-domain" ]]; then
      args+=(--hot hot.bin)
    fi
    "$RUST_BIN" "${args[@]}" >"$dir/w$i.json" 2>"$dir/w$i.err" &
    pids+=("$!")
  done
  local failed=0
  for pid in "${pids[@]}"; do wait "$pid" || failed=$((failed + 1)); done
  local end_ns; end_ns="$(date +%s%N)"
  local wall_ms=$(( (end_ns - start_ns) / 1000000 ))
  # Verify every domain separately; the global verifier must refuse.
  local verify_ok=true
  for ((i = 0; i < n; i++)); do
    if ! "$WEFT_BIN" --store "$dir/store" --writer-mode sharded --domain "${domains[$i]}" \
        verify --full >"$dir/verify-$i.out" 2>&1; then
      verify_ok=false
    fi
  done
  local global_refused=false
  if ! "$WEFT_BIN" --store "$dir/store" verify >/dev/null 2>"$dir/global.err"; then
    if grep -q UnsupportedVersion "$dir/global.err"; then global_refused=true; fi
  fi
  python3 - "$dir" "$distribution" "$n" "$count" "$wall_ms" "$failed" "$verify_ok" \
    "$global_refused" <<'PY'
import glob, json, os, sys
(root, distribution, n, count, wall_ms, failed, verify_ok, global_refused) = sys.argv[1:]
rows = [json.load(open(p)) for p in sorted(glob.glob(os.path.join(root, "w*.json")))]
committed = sum(r.get("committed", 0) for r in rows)
conflicts = sum(r.get("conflicts", 0) for r in rows)
attempts = sum(r.get("attempts", 0) for r in rows)
spans = [r.get("wall_ms", 0) for r in rows]
# End-to-end includes every writer's process start; `steady` is the widest
# writer span, i.e. the window in which the writers actually overlapped.
steady_ms = max(spans) if spans else 0
values = []
for p in glob.glob(os.path.join(root, "latencies", "*.lat")):
    values.extend(int(line) for line in open(p) if line.strip())
values.sort()
def pick(q):
    if not values:
        return 0.0
    return values[min(len(values) - 1, int(round(q * (len(values) - 1))))] / 1e6
def sumfield(name):
    return sum(r.get(name, 0) for r in rows)
row = {
  "distribution": distribution,
  "client": "process-per-writer",
  "n": int(n),
  "count_per_writer": int(count),
  "requested": int(n) * int(count),
  "processes": len(rows),
  "process_failures": int(failed),
  "committed": committed,
  "conflicts": conflicts,
  "aborted": sumfield("aborted"),
  "attempts": attempts,
  "retry_amplification": (attempts / committed) if committed else 0.0,
  "wall_ms": int(wall_ms),
  "steady_ms": steady_ms,
  "startup_ms": max(0, int(wall_ms) - steady_ms),
  "throughput_tx_s": (committed / (int(wall_ms) / 1000.0)) if int(wall_ms) else 0.0,
  "steady_tx_s": (committed / (steady_ms / 1000.0)) if steady_ms else 0.0,
  "p50_ms": pick(0.50), "p95_ms": pick(0.95), "p99_ms": pick(0.99),
  "max_ms": (values[-1] / 1e6) if values else 0.0,
  "backend_reads": sumfield("backend_reads"),
  "backend_writes": sumfield("backend_writes"),
  "backend_requests": sumfield("backend_reads") + sumfield("backend_writes"),
  "bytes_read": sumfield("bytes_read"),
  "bytes_written": sumfield("bytes_written"),
  "reads_per_commit": (sumfield("backend_reads") / committed) if committed else 0.0,
  "writes_per_commit": (sumfield("backend_writes") / committed) if committed else 0.0,
  "gap_commits_read": sumfield("rebase_gap_commits_read"),
  "gap_commits_cached": sumfield("rebase_gap_commits_cached"),
  "touched_intersections": sumfield("rebase_touched_intersections"),
  "max_consecutive_retries": max((r.get("rebase_max_consecutive_retries", 0) for r in rows), default=0),
  "rss_sum_kib": sumfield("peak_rss_kib"),
  "verify_full": verify_ok == "true",
  "global_reader_refused": global_refused == "true",
  "errors": [e for r in rows for e in r.get("errors", [])],
}
print(json.dumps(row, sort_keys=True))
PY
  # The row is complete; the court /tmp is a 1 GiB tmpfs, so free the store
  # before the next cell.
  rm -rf "$dir"
}

mw3_rows="$work/mw3-rows.ndjson"
: >"$mw3_rows"
for n in "${DISJOINT_NS[@]}"; do
  run_mw3_config disjoint "$n" 400 >>"$mw3_rows"
done
for n in "${CONFLICT_NS[@]}"; do
  run_mw3_config same-domain "$n" 10 >>"$mw3_rows"
done

# ---------------------------------------------------------------------------
# T027: compact client (threads in one process) at high writer counts.
# ---------------------------------------------------------------------------
echo "== writer scale (compact client) =="
scale_rows="$work/scale-rows.ndjson"
: >"$scale_rows"
for n in "${SCALE_NS[@]}"; do
  dir="$work/scale-$n"
  mkdir -p "$dir/store"
  "$RUST_BIN" init --store "$dir/store" --fixed-timestamp-ns "$NOW_NS" >/dev/null
  : >"$dir/domains.txt"
  for ((i = 0; i < n; i++)); do
    "$RUST_BIN" register --store "$dir/store" --name "scale-$i" --kind checkpoint_family \
      --fixed-timestamp-ns "$NOW_NS" --plain >>"$dir/domains.txt"
  done
  # Registration is one process per domain by construction. The measured cell
  # is the compact client itself: N threads in ONE process.
  # The 512/1024 cells hold every worker alive until spawning finishes, so the
  # court's pid limit is measured (spawn failures are reported), not assumed.
  hold=()
  if (( n >= 512 )); then hold=(--hold); fi
  if (( n <= 64 )); then compact_count=50; elif (( n <= 256 )); then compact_count=20; else compact_count=10; fi
  start_ns="$(date +%s%N)"
  set +e
  timeout 900 "$COMPACT_BIN" --store "$dir/store" --domain-list "$dir/domains.txt" \
    --threads "$n" --count "$compact_count" --fixed-timestamp-ns "$NOW_NS" \
    "${hold[@]}" --latencies-dir "$dir/latencies" >"$dir/compact.json" 2>"$dir/compact.err"
  rc=$?
  set -e
  end_ns="$(date +%s%N)"
  wall_ms=$(( (end_ns - start_ns) / 1000000 ))
  python3 - "$n" "$dir" "$wall_ms" "$rc" <<'PY' >>"$scale_rows"
import json, os, sys
n, root, wall_ms, rc = sys.argv[1:]
path = os.path.join(root, "compact.json")
doc = {}
if os.path.exists(path) and os.path.getsize(path):
    try:
        doc = json.load(open(path))
    except Exception:
        doc = {}
err = ""
p = os.path.join(root, "compact.err")
if os.path.exists(p):
    err = open(p).read()[-2000:]
doc.update({
    "n": int(n),
    "mode": "threads-in-one-process-hold" if doc.get("hold") else "threads-in-one-process",
    "timeout_rc": int(rc),
    "outer_wall_ms": int(wall_ms),
    "stderr_tail": err,
})
print(json.dumps(doc, sort_keys=True))
PY
  rm -rf "$dir"
done
# A real-process subset proves the process-per-writer path is not required and
# is not what the high counts use.
dir="$work/scale-process-32"
mkdir -p "$dir/store" "$dir/latencies"
"$RUST_BIN" init --store "$dir/store" --fixed-timestamp-ns "$NOW_NS" >/dev/null
: >"$dir/domains.txt"
for ((i = 0; i < 32; i++)); do
  "$RUST_BIN" register --store "$dir/store" --name "proc-$i" --kind checkpoint_family \
    --fixed-timestamp-ns "$NOW_NS" --plain >>"$dir/domains.txt"
done
mapfile -t proc_domains <"$dir/domains.txt"
start_ns="$(date +%s%N)"
pids=()
for ((i = 0; i < 32; i++)); do
  "$RUST_BIN" write --store "$dir/store" --writer "p$i" --prefix "p$i" --count 5 \
    --latencies-out "$dir/latencies/p$i.lat" --domain "${proc_domains[$i]}" \
    --fixed-timestamp-ns "$NOW_NS" >"$dir/p$i.json" 2>"$dir/p$i.err" &
  pids+=("$!")
done
failed=0
for pid in "${pids[@]}"; do wait "$pid" || failed=$((failed + 1)); done
end_ns="$(date +%s%N)"
python3 - "$dir" "$(( (end_ns - start_ns) / 1000000 ))" "$failed" <<'PY' >>"$scale_rows"
import glob, json, os, sys
root, wall_ms, failed = sys.argv[1:]
rows = [json.load(open(p)) for p in sorted(glob.glob(os.path.join(root, "p*.json")))]
committed = sum(r.get("committed", 0) for r in rows)
values = []
for p in glob.glob(os.path.join(root, "latencies", "*.lat")):
    values.extend(int(line) for line in open(p) if line.strip())
values.sort()
def pick(q):
    if not values:
        return 0.0
    return values[min(len(values) - 1, int(round(q * (len(values) - 1))))] / 1e6
print(json.dumps({
    "n": 32, "mode": "one-process-per-writer",
    "processes": len(rows), "process_failures": int(failed),
    "committed": committed, "requested": 32 * 5,
    "throughput_tx_s": (committed / (int(wall_ms) / 1000.0)) if int(wall_ms) else 0.0,
    "p50_ms": pick(0.50), "p95_ms": pick(0.95), "p99_ms": pick(0.99),
    "reads_per_commit": (sum(r.get("backend_reads", 0) for r in rows) / committed) if committed else 0.0,
    "max_consecutive_retries": max((r.get("rebase_max_consecutive_retries", 0) for r in rows), default=0),
    "rss_sum_kib": sum(r.get("peak_rss_kib", 0) for r in rows),
}, sort_keys=True))
PY
rm -rf "$dir"

# ---------------------------------------------------------------------------
# T021: MW2 before/after at N=32.
# ---------------------------------------------------------------------------
echo "== T021 MW2 retry at N=32 =="
retry_rows="$work/retry-rows.ndjson"
: >"$retry_rows"
run_mw2_n32() {
  local label="$1" binary="$2"
  local dir="$work/mw2-$label"
  mkdir -p "$dir/store" "$dir/latencies"
  "$binary" init --store "$dir/store" --fixed-timestamp-ns "$NOW_NS" >/dev/null
  start_ns="$(date +%s%N)"
  pids=()
  for ((i = 0; i < 32; i++)); do
    "$binary" write --store "$dir/store" --writer "w$i" --prefix "w$i" --count 7 \
      --latencies-out "$dir/latencies/w$i.lat" --fixed-timestamp-ns "$NOW_NS" \
      >"$dir/w$i.json" 2>"$dir/w$i.err" &
    pids+=("$!")
  done
  failed=0
  for pid in "${pids[@]}"; do wait "$pid" || failed=$((failed + 1)); done
  end_ns="$(date +%s%N)"
  python3 - "$dir" "$label" "$(( (end_ns - start_ns) / 1000000 ))" "$failed" <<'PY' >>"$retry_rows"
import glob, json, os, sys
root, label, wall_ms, failed = sys.argv[1:]
rows = []
for path in sorted(glob.glob(os.path.join(root, "w*.json"))):
    try:
        rows.append(json.load(open(path)))
    except Exception:
        pass
committed = sum(r.get("committed", 0) for r in rows)
attempts = sum(r.get("attempts", 0) for r in rows)
values = []
for p in glob.glob(os.path.join(root, "latencies", "*.lat")):
    values.extend(int(line) for line in open(p) if line.strip())
values.sort()
def pick(q):
    if not values:
        return 0.0
    return values[min(len(values) - 1, int(round(q * (len(values) - 1))))] / 1e6
reads = sum(r.get("backend_reads", 0) for r in rows)
print(json.dumps({
    "label": label, "n": 32, "committed": committed, "processes": len(rows),
    "process_failures": int(failed), "attempts": attempts,
    "retry_amplification": (attempts / committed) if committed else 0.0,
    "backend_reads": reads,
    "backend_writes": sum(r.get("backend_writes", 0) for r in rows),
    "bytes_read": sum(r.get("bytes_read", 0) for r in rows),
    "bytes_written": sum(r.get("bytes_written", 0) for r in rows),
    "reads_per_commit": (reads / committed) if committed else 0.0,
    "throughput_tx_s": (committed / (int(wall_ms) / 1000.0)) if int(wall_ms) else 0.0,
    "p50_ms": pick(0.50), "p95_ms": pick(0.95), "p99_ms": pick(0.99),
    "max_ms": (values[-1] / 1e6) if values else 0.0,
    "gap_commits_read": sum(r.get("rebase_gap_commits_read", 0) for r in rows),
    "gap_commits_cached": sum(r.get("rebase_gap_commits_cached", 0) for r in rows),
    "max_consecutive_retries": max((r.get("rebase_max_consecutive_retries", 0) for r in rows), default=0),
    "max_consecutive_retries_measured": all("rebase_max_consecutive_retries" in r for r in rows),
    "rss_sum_kib": sum(r.get("peak_rss_kib", 0) for r in rows),
}, sort_keys=True))
PY
  rm -rf "$dir"
}
# Interleaved repetitions: the arms alternate so page-cache warmth and CPU
# state cannot silently favour one of them.
before_rev="${WEFT_MW3_BEFORE_REV:-unknown}"
before_sha="unknown"
if [[ -n "${WEFT_MW3_BEFORE:-}" && -x "${WEFT_MW3_BEFORE}/example-mw2-writer" ]]; then
  before_sha="$(sha256sum "${WEFT_MW3_BEFORE}/example-mw2-writer" | cut -d' ' -f1)"
fi
for rep in 1 2 3; do
  if [[ -n "${WEFT_MW3_BEFORE:-}" && -x "${WEFT_MW3_BEFORE}/example-mw2-writer" ]]; then
    run_mw2_n32 "before-$rep" "${WEFT_MW3_BEFORE}/example-mw2-writer"
  fi
  run_mw2_n32 "after-$rep" "$RUST_BIN"
done
if [[ -z "${WEFT_MW3_BEFORE:-}" ]]; then
  echo "T021: no baseline binary supplied (WEFT_MW3_BEFORE); only the after rows are measured"
fi

# ---------------------------------------------------------------------------
# Receipts.
# ---------------------------------------------------------------------------
finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
log_sha="$(sha256sum "$log" | cut -d' ' -f1)"

python3 - "$mw3_rows" "$receipt_dir/mw3-${stamp}.json" "$stamp" "$finished_at" \
  "$log" "$log_sha" "$profile" <<'PY'
import json, os, sys
(rows_path, receipt, stamp, finished_at, log, log_sha, profile) = sys.argv[1:]
rows = [json.loads(line) for line in open(rows_path) if line.strip()]
disjoint = [r for r in rows if r["distribution"] == "disjoint"]
base = next((r["throughput_tx_s"] for r in disjoint if r["n"] == 1), 0.0)
steady_base = next((r["steady_tx_s"] for r in disjoint if r["n"] == 1), 0.0)
for row in rows:
    row["scaling_efficiency"] = (
        row["throughput_tx_s"] / (row["n"] * base)
        if row["distribution"] == "disjoint" and base else None)
    row["steady_efficiency"] = (
        row["steady_tx_s"] / (row["n"] * steady_base)
        if row["distribution"] == "disjoint" and steady_base else None)
    row["tx_per_writer_s"] = (
        row["throughput_tx_s"] / row["n"] if row["n"] else None)
doc = {
  "schema": "weftfs.mw3.bench/1",
  "generated_at": finished_at, "stamp": stamp, "profile": profile,
  "contract": "spec/WEFT-MULTIWRITER.md section 15; bench/CONTRACTS-multiwriter.md section 8",
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
  },
  "revisions": {"WeftFS": os.environ.get("WEFT_REPO_WEFTFS", "unknown")},
  "rows": rows,
  "scaling_efficiency_base": base,
  "isolation": "per-domain linearizable publication; the read view is a sequential capture, never a global linearizable point",
  "not_claimed": ["fleet/multi-node", "cross-domain atomicity", "coordinated GC", "physical power loss", "Go MW3 parity"],
  "evidence": {"log": log, "log_sha256": log_sha},
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "rows": len(rows)}))
PY

python3 - "$retry_rows" "$retry_receipt" "$stamp" "$finished_at" "$log" "$log_sha" \
  "$before_rev" "$before_sha" <<'PY'
import json, os, sys
(rows_path, receipt, stamp, finished_at, log, log_sha, before_rev, before_sha) = sys.argv[1:]
rows = [json.loads(line) for line in open(rows_path) if line.strip()]
def median(values):
    values = sorted(values)
    if not values:
        return None
    return values[len(values) // 2]
arms = {}
for arm in ("before", "after"):
    own = [r for r in rows if r["label"].split("-")[0] == arm]
    if own:
        arms[arm] = {
          "repetitions": len(own),
          "reads_per_commit_median": median([r["reads_per_commit"] for r in own]),
          "p99_ms_median": median([r["p99_ms"] for r in own]),
          "throughput_tx_s_median": median([r["throughput_tx_s"] for r in own]),
          "retry_amplification_median": median([r["retry_amplification"] for r in own]),
          "gap_commits_read_median": median([r["gap_commits_read"] for r in own]),
          "gap_commits_cached_median": median([r["gap_commits_cached"] for r in own]),
        }
comparison = None
if "before" in arms and "after" in arms:
    b, a = arms["before"], arms["after"]
    comparison = {
      "reads_per_commit": {"before": b["reads_per_commit_median"],
                           "after": a["reads_per_commit_median"],
                           "delta_percent": ((a["reads_per_commit_median"] - b["reads_per_commit_median"])
                                             / b["reads_per_commit_median"] * 100.0)
                           if b["reads_per_commit_median"] else None},
      "p99_ms": {"before": b["p99_ms_median"], "after": a["p99_ms_median"],
                 "delta_percent": ((a["p99_ms_median"] - b["p99_ms_median"]) / b["p99_ms_median"] * 100.0)
                 if b["p99_ms_median"] else None},
      "throughput_tx_s": {"before": b["throughput_tx_s_median"],
                          "after": a["throughput_tx_s_median"]},
    }
doc = {
  "schema": "weftfs.mw2.retry.bench/1",
  "generated_at": finished_at, "stamp": stamp, "n": 32,
  "method": "seven disjoint transactions per writer, 32 process writers, one local store, one repetition",
  "rows": rows, "arms": arms, "comparison": comparison,
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
  },
  "revisions": {"WeftFS": os.environ.get("WEFT_REPO_WEFTFS", "unknown")},
  "baseline": {"revision": before_rev, "binary_sha256": before_sha},
  "baseline_note": ("before == the tree at WEFT_MW3_BEFORE (revision above); after == this tree; "
                    "arms are interleaved and the comparison uses the median of 3 repetitions"
                    if "before" in arms else
                    "no baseline binary was supplied; only the after rows are measured"),
  "evidence": {"log": log, "log_sha256": log_sha},
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "comparison": comparison}))
PY

python3 - "$scale_rows" "$scale_receipt" "$stamp" "$finished_at" "$log" "$log_sha" <<'PY'
import json, os, sys
(rows_path, receipt, stamp, finished_at, log, log_sha) = sys.argv[1:]
rows = [json.loads(line) for line in open(rows_path) if line.strip()]
best_rate = max(
    ((r.get("throughput_tx_s", 0.0) / r["n"]) for r in rows
     if r.get("mode") == "threads-in-one-process" and r.get("n")),
    default=0.0)
for row in rows:
    # There is no N=1 compact cell, so a true scale-up efficiency cannot be
    # formed. Report the per-writer rate and its ratio to the best per-writer
    # rate observed; 1.0 means "the best per-writer throughput in this court".
    row["tx_per_writer_s"] = (
        row.get("throughput_tx_s", 0.0) / row["n"] if row.get("n") else None)
    row["per_writer_rate_vs_best"] = (
        row["tx_per_writer_s"] / best_rate
        if best_rate and row.get("tx_per_writer_s") is not None else None)
    row.pop("scaling_efficiency", None)
    row.pop("scaling_efficiency_vs_smallest_compact", None)
doc = {
  "schema": "weftfs.writer.scale/1",
  "generated_at": finished_at, "stamp": stamp,
  "method": ("threads in one process for 32..1024 (each thread its own store handle and "
             "conflict domain); one real-process-per-writer subset at 32 proves the process "
             "path is not required"),
  "rows": rows,
  "resource_envelope": {
    "declared_per_writer_threads": 1,
    "commit_delta_cache_entries_per_handle": 4096,
    "commit_delta_cache_bytes_per_handle": 32 * 1024 * 1024,
    "note": ("the cache bound is a ceiling, not an allocation; observed RSS is in each row"),
  },
  "environment": {
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.uname().release,
    "limit_memory": os.environ.get("WEFT_COURT_MEMORY", "?"),
    "limit_cpus": os.environ.get("WEFT_COURT_CPUS", "?"),
    "limit_pids": os.environ.get("WEFT_COURT_PIDS", "?"),
  },
  "revisions": {"WeftFS": os.environ.get("WEFT_REPO_WEFTFS", "unknown")},
  "evidence": {"log": log, "log_sha256": log_sha},
}
json.dump(doc, open(receipt, "w"), indent=2, sort_keys=True)
open(receipt, "a").write("\n")
print(json.dumps({"receipt": receipt, "rows": len(rows)}))
PY

echo "receipts:"
sha256sum "$mw3_receipt" "$retry_receipt" "$scale_receipt" "$log"
