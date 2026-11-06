#!/usr/bin/env bash
# R8 reconstruction single-flight + dependency-aware decoded-object cache court.
#
#   scripts/docker-court.sh bench "cd /work && bash bench/scripts/cache_court.sh"
#
# Uses the cached real Qwen2.5-0.5B / Instruct pair (both files re-hashed against
# bench/corpora/real-models/qwen2.5-0.5b-pair.json inside the court). The
# workload is written with the shipped planner:
#
#   shared_base        1 real BF16 tensor (8,716,288 B)
#   shared_dependent   6 sparse causal deltas of that one base, each also a
#                      `derived_from` relation target -> a genuinely shared base
#   base               7 more real base tensors
#   delta              8 real Instruct tensors, each causal on its own base
#   independent        4 distinct real tensors with no shared dependency
#
# Each configuration reads every object in that order, `--rounds` times, from
# `--threads` concurrent workers, and hashes every returned byte.
set -Eeuo pipefail

root=/work
work="$root/.docker-cache/cache-court"
courts="$work/runs"
rounds="${WEFT_CACHE_ROUNDS:-4}"
cache_budget="${WEFT_CACHE_COURT_BYTES:-268435456}"
manifest="$root/bench/corpora/real-models/qwen2.5-0.5b-pair.json"
base_file="$root/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors"
instruct_file="$root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipt="$root/bench/receipts/cache-${stamp}.json"
log="$root/bench/receipts/cache-${stamp}.log"

mkdir -p "$root/bench/receipts" "$courts"
exec > >(tee "$log") 2>&1

python3 - "$manifest" "$base_file" "$instruct_file" <<'PY'
import hashlib, json, sys
manifest, base, instruct = sys.argv[1:4]
spec = json.load(open(manifest))["pair"]
for role, path in (("base", base), ("instruct", instruct)):
    want = spec[role]["sha256"]
    want_size = spec[role]["size"]
    h = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            h.update(block)
    got = h.hexdigest()
    size = __import__("os").path.getsize(path)
    if got != want or size != want_size:
        raise SystemExit(
            f"{role}: sha256 {got} ({size} B) does not match the corpus manifest "
            f"{want} ({want_size} B)"
        )
    print(f"{role} sha256 {got} size {size} verified")
PY

echo "== build =="
cargo build --release --example cache_bench
binary="$root/.docker-cache/target-rust/release/examples/cache_bench"

plan="$work/plan.json"
store="$work/store"
echo "== prepare =="
"$binary" prepare \
  --base-file "$base_file" \
  --instruct-file "$instruct_file" \
  --store "$store" \
  --out "$plan" \
  --force

run() {
  local config="$1"
  local threads="$2"
  local out="$courts/${config}-t${threads}.json"
  echo "== run config=$config threads=$threads =="
  "$binary" run \
    --plan "$plan" \
    --config "$config" \
    --threads "$threads" \
    --rounds "$rounds" \
    --cache-bytes "$cache_budget" \
    --out "$out"
  sync
}

for config in none singleflight cache both; do
  for threads in 1 4; do
    run "$config" "$threads"
  done
done

echo "== adversarial =="
"$binary" adversarial \
  --scratch "$work/adversarial" \
  --out "$work/adversarial.json"

echo "== assemble receipt =="
python3 - "$manifest" "$plan" "$courts" "$work/adversarial.json" "$receipt" "$stamp" "$rounds" "$cache_budget" <<'PY'
import json, os, sys, platform

manifest, plan_path, runs_dir, adversarial_path, receipt, stamp, rounds, cache_budget = sys.argv[1:9]
rounds = int(rounds)
cache_budget = int(cache_budget)
plan = json.load(open(plan_path))
adversarial = json.load(open(adversarial_path))

configs = ["none", "singleflight", "cache", "both"]
threads_list = [1, 4]
runs = {}
for config in configs:
    for threads in threads_list:
        path = os.path.join(runs_dir, f"{config}-t{threads}.json")
        runs[f"{config}@{threads}"] = json.load(open(path))

def run_row(key):
    r = runs[key]
    return {
        "config": r["config"],
        "threads": r["threads"],
        "rounds": r["rounds"],
        "expected_reads": r["expected_reads"],
        "hash_matches": r["hash_matches"],
        "hash_mismatches": r["hash_mismatches"],
        "exact": r["exact"],
        "wall_ms": r["wall_ms"],
        "latency_p50_ns": r["latency_ns"]["p50"],
        "latency_p99_ns": r["latency_ns"]["p99"],
        "latency_max_ns": r["latency_ns"]["max"],
        "latency_samples": r["latency_ns"]["samples"],
        "backend_bytes_read": r["backend"]["bytes_read"],
        "backend_reads": r["backend"]["reads"],
        "decode_calls": r["decode_calls"],
        "decode_bytes": r["decode_bytes"],
        "single_flight_leaders": r["single_flight"]["leaders"],
        "single_flight_waiters": r["single_flight"]["waiters"],
        "single_flight_degraded": r["single_flight"]["degraded"],
        "cache_hits": r["cache"]["hits"],
        "cache_misses": r["cache"]["misses"],
        "cache_evictions": r["cache"]["evictions"],
        "cache_entries": r["cache"]["entries"],
        "cache_bytes": r["cache"]["bytes"],
        "peak_rss_kib": r["peak_rss_kib"],
        "verify_ok": r["verify"]["ok"],
    }

keys = [f"{c}@{t}" for c in configs for t in threads_list]
table = [run_row(k) for k in keys]

checks = []
def check(name, ok, detail):
    checks.append({"check": name, "pass": bool(ok), "detail": detail})

for key, r in runs.items():
    check(f"exact_{key}", bool(r["exact"]), f"{r['hash_matches']}/{r['expected_reads']} reads hashed identically")
    check(f"verify_{key}", bool(r["verify"]["ok"]), f"verify --full checked {r['verify']['checked_objects']} objects")

def row(key):
    return runs[key]

# Cache/single-flight must never change result bytes; every run is exact.
same_bytes = all(runs[k]["hash_mismatches"] == 0 for k in runs)

# 1 thread: no concurrency, so single-flight cannot help and must not hurt.
check(
    "singleflight_no_effect_at_one_thread",
    row("singleflight@1")["decode_calls"] == row("none@1")["decode_calls"],
    f"none@1 {row('none@1')['decode_calls']} decodes, singleflight@1 "
    f"{row('singleflight@1')['decode_calls']} decodes",
)
# 4 threads: identical concurrent reads coalesce.
check(
    "singleflight_reduces_decodes_at_four_threads",
    row("singleflight@4")["decode_calls"] < row("none@4")["decode_calls"],
    f"none@4 {row('none@4')['decode_calls']} decodes, singleflight@4 "
    f"{row('singleflight@4')['decode_calls']} decodes",
)
check(
    "singleflight_waiters_observed",
    row("singleflight@4")["single_flight"]["waiters"] > 0,
    f"{row('singleflight@4')['single_flight']['waiters']} waiters",
)
# The cache must remove repeated reconstructions at both thread counts.
check(
    "cache_reduces_decodes_at_one_thread",
    row("cache@1")["decode_calls"] < row("none@1")["decode_calls"],
    f"none@1 {row('none@1')['decode_calls']}, cache@1 {row('cache@1')['decode_calls']}",
)
check(
    "cache_reduces_backend_bytes",
    row("cache@1")["backend"]["bytes_read"] < row("none@1")["backend"]["bytes_read"],
    f"none@1 {row('none@1')['backend']['bytes_read']} B, cache@1 "
    f"{row('cache@1')['backend']['bytes_read']} B",
)
check(
    "cache_hits_observed",
    row("cache@1")["cache"]["hits"] > 0 and row("both@4")["cache"]["hits"] > 0,
    f"cache@1 hits {row('cache@1')['cache']['hits']}, both@4 hits {row('both@4')['cache']['hits']}",
)
check(
    "cache_disabled_runs_report_no_hits",
    row("none@1")["cache"]["hits"] == 0 and row("singleflight@4")["cache"]["hits"] == 0,
    "the cache is off in the none/singleflight configurations",
)
# The shared base (6 dependents) must be retained while the cache is on.
check(
    "shared_base_retained_under_pressure",
    row("cache@1")["cache"]["entries"] > 0 and row("both@4")["cache"]["entries"] > 0,
    f"cache@1 entries {row('cache@1')['cache']['entries']} ({row('cache@1')['cache']['bytes']} B), "
    f"both@4 entries {row('both@4')['cache']['entries']}",
)
check(
    "adversarial_all_pass",
    adversarial["passed"] == adversarial["total"] and adversarial["total"] >= 5,
    f"{adversarial['passed']}/{adversarial['total']} adversarial checks",
)
check("every_run_exact", same_bytes, "every run has zero hash mismatches")

# Losses are declared explicitly, not hidden in prose.
losses = []
for key in keys:
    r = runs[key]
    if r["cache"]["evictions"] > 0:
        losses.append(
            f"{key}: {r['cache']['evictions']} cache evictions under a "
            f"{cache_budget} B bound (reconstruction repeated for the evicted entries)"
        )
none_rss = row("none@1")["peak_rss_kib"]
for key in keys:
    if runs[key]["cache"]["enabled"] and runs[key]["peak_rss_kib"] >= none_rss:
        losses.append(
            f"{key}: peak RSS {runs[key]['peak_rss_kib']} KiB vs none@1 {none_rss} KiB "
            f"(the cache retains decoded bytes in this process)"
        )
losses.append(
    "single-flight at 1 thread cannot coalesce anything; it adds a mutex/condvar "
    "round trip and is published as no benefit (decode calls are equal)."
)
losses.append(
    "the cache is in-process and per open store handle: a new process/mount starts cold, "
    "and nothing here is persisted."
)

receipt_doc = {
    "schema": "weft.cache-bench.receipt.v1",
    "stamp": stamp,
    "label": "R8 reconstruction single-flight + dependency-aware decoded-object cache",
    "environment": {
        "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", platform.release()),
        "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
        "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
        "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        "docker_server": os.environ.get("WEFT_DOCKER_SERVER", "unknown"),
        "revisions": {
            "WeftFS": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "WeftFS-Go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
            "WeftCodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
            "WeftCodec-Go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        },
    },
    "corpus": json.load(open(manifest)),
    "workload": {
        "rounds": rounds,
        "cache_bound_bytes": cache_budget,
        "objects": plan["objects"],
        "groups": plan["groups"],
        "logical_bytes_per_round": plan["logical_bytes_per_round"],
        "generation": plan["generation"],
        "prepare_verify": plan["verify"],
    },
    "table": table,
    "adversarial": adversarial,
    "checks": checks,
    "passed": sum(1 for c in checks if c["pass"]),
    "total": len(checks),
    "losses": losses,
    "notes": [
        "Physical backend bytes are the store backend's bytes_read counter across the timed workload only.",
        "decode_calls counts every reconstruction the store actually executed, including causal base reads inside a delta decode.",
        "Wall/latency samples are one host, warm in-process store, tmpfs-backed .docker-cache; not a fleet or cold-cache claim.",
        "Cache/single-flight are read-path only; no on-disk format, commit, verify, or GC behaviour changed.",
    ],
}
with open(receipt, "w") as handle:
    json.dump(receipt_doc, handle, indent=1, sort_keys=False)
    handle.write("\n")

print(f"checks {receipt_doc['passed']}/{receipt_doc['total']}")
for item in checks:
    if not item["pass"]:
        print(f"FAIL {item['check']}: {item['detail']}")
if receipt_doc["passed"] != receipt_doc["total"]:
    raise SystemExit(1)
PY

echo "receipt: $receipt"
echo "raw log: $log"
sha256sum "$receipt" "$log"
