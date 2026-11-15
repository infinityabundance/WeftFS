#!/usr/bin/env bash
# Inner half of the P03 court. Runs in the fuse court.
set -Eeuo pipefail
cd /work

stamp="${1:?stamp}"
work=".docker-cache/training-ready-cost-${stamp}"
mkdir -p "$work" bench/receipts
log="bench/receipts/training-ready-cost-${stamp}.log"
: > "$log"

say() { echo "$@" | tee -a "$log"; }

now_ns() { date +%s%N; }
timed() {
  local label="$1"; shift
  local start end
  start="$(now_ns)"
  "$@" > "$work/$label.out" 2> "$work/$label.err"
  end="$(now_ns)"
  python3 - "$work/$label.ns" "$start" "$end" <<'PY'
import sys
open(sys.argv[1], "w").write("%s %s\n" % (sys.argv[2], sys.argv[3]))
PY
}

say "== build CLI =="
cargo build --release --quiet --bin weftfs
weft="./.docker-cache/target-rust/release/weftfs"
fixed="--fixed-timestamp-ns 1767225600000000000"

say "== copy the cached P3 store =="
rm -rf "$work/store"
cp -a .docker-cache/real-corpus/store-qwen-base "$work/store"
weights=".docker-cache/real-corpus/Qwen2.5-0.5B.safetensors"

say "== checkpoint write (2 raw shards) =="
timed checkpoint "$weft" --store "$work/store" $fixed --json checkpoint write \
  --file "$weights" --name qwen --shards 2 --axis 0 --shard-strategy raw

say "== snapshot and context root =="
timed snapshot "$weft" --store "$work/store" $fixed --json snapshot create training
selector='select has(attrs.weft.checkpoint.schema)'
timed context "$weft" --store "$work/store" $fixed --json context create \
  --name qwen-training --selector "$selector" --snapshot training

cache="$work/cache"
materialized="$work/materialized"
rm -rf "$cache" "$materialized"

say "== cold check (cache miss) =="
timed check_cold "$weft" --store "$work/store" $fixed --json training-ready \
  --root qwen-training --check --require-tensors --cache-dir "$cache"

say "== warm-first check (cache hit) =="
timed check_warm_first "$weft" --store "$work/store" $fixed --json training-ready \
  --root qwen-training --check --require-tensors --cache-dir "$cache"

say "== repeated warm checks =="
for i in 1 2 3 4 5; do
  timed "check_warm_$i" "$weft" --store "$work/store" $fixed --json training-ready \
    --root qwen-training --check --require-tensors --cache-dir "$cache"
done

say "== policy change invalidates (larger envelope) =="
actual="$(python3 -c "import json;print(json.load(open('$work/check_cold.out'))['cost']['actual_bytes'])")"
timed check_policy "$weft" --store "$work/store" $fixed --json training-ready \
  --root qwen-training --check --require-tensors --cache-dir "$cache" \
  --max-bytes "$((actual + 1))"

say "== grant edit invalidates (mutable authority) =="
context_id="$(python3 -c "import json;print(json.load(open('$work/context.out'))['context_root_id'])")"
timed grant "$weft" --store "$work/store" $fixed --json training-artifact grant \
  --root "$context_id" --uid 4242 --gid 4242 --action train
timed check_grant "$weft" --store "$work/store" $fixed --json training-ready \
  --root qwen-training --check --require-tensors --cache-dir "$cache"

say "== cold materialize =="
timed materialize_cold "$weft" --store "$work/store" $fixed --json materialize \
  --root qwen-training --training-ready --require-tensors --out "$materialized"

say "== verify the materialized directory offline =="
timed verify_out "$weft" $fixed --json training-ready --verify-out "$materialized"

say "== warm materialize (a second full load) =="
timed materialize_warm "$weft" --store "$work/store" $fixed --json materialize \
  --root qwen-training --training-ready --require-tensors --out "$materialized-2"

say "== assemble receipt =="
src_sha="$(sha256sum "$weights" | awk '{print $1}')"
du_bytes="$(du -sb "$materialized" | awk '{print $1}')"
python3 - "$stamp" "$work" "$src_sha" "$du_bytes" <<'PY'
import json
import os
import sys

stamp, work, src_sha, materialized_bytes = sys.argv[1:5]


def out(name):
    with open(os.path.join(work, name + ".out")) as handle:
        return json.load(handle)


def ms(name):
    start, end = map(int, open(os.path.join(work, name + ".ns")).read().split())
    return (end - start) / 1e6


checkpoint = out("checkpoint")
snapshot = out("snapshot")
context = out("context")
cold = out("check_cold")
warm_first = out("check_warm_first")
warm = [out("check_warm_%d" % i) for i in range(1, 6)]
policy = out("check_policy")
grant = out("check_grant")
materialize_cold = out("materialize_cold")
materialize_warm = out("materialize_warm")
verify_out = out("verify_out")

warm_walls = sorted(ms("check_warm_%d" % i) for i in range(1, 6))
warm_p50 = warm_walls[len(warm_walls) // 2]


def cache_of(report):
    return report.get("cache") or {}


checks = [
    {"name": "cold_check_missed", "ok": cache_of(cold).get("hit") is False,
     "detail": "cold cache hit=%s" % cache_of(cold).get("hit")},
    {"name": "warm_check_hit", "ok": cache_of(warm_first).get("hit") is True,
     "detail": "warm hit=%s" % cache_of(warm_first).get("hit")},
    {"name": "repeated_checks_hit", "ok": all(cache_of(r).get("hit") is True for r in warm),
     "detail": "5/5 hits" if all(cache_of(r).get("hit") is True for r in warm) else "misses"},
    {"name": "policy_change_invalidates", "ok": cache_of(policy).get("hit") is False,
     "detail": "hit=%s" % cache_of(policy).get("hit")},
    {"name": "grant_edit_invalidates", "ok": cache_of(grant).get("hit") is False,
     "detail": "hit=%s" % cache_of(grant).get("hit")},
    {"name": "cold_and_warm_report_identical",
     "ok": cold.get("context_root_id") == warm_first.get("context_root_id")
           and cold.get("snapshot_id") == warm_first.get("snapshot_id")
           and cold.get("cost", {}).get("actual_bytes") == warm_first.get("cost", {}).get("actual_bytes"),
     "detail": "same root, snapshot and bytes"},
    {"name": "source_digest_matches",
     "ok": checkpoint.get("source_sha256") == src_sha, "detail": src_sha},
    {"name": "materialize_exact",
     "ok": materialize_cold.get("checkpoint_tensors_verified") == len(checkpoint.get("tensors", []))
           and verify_out.get("checkpoints_verified") == 1,
     "detail": "checkpoint tensors verified"},
    {"name": "warm_materialize_exact",
     "ok": materialize_warm.get("checkpoint_tensors_verified") == len(checkpoint.get("tensors", [])),
     "detail": "second load exact"},
]

cold_check_ms = ms("check_cold")
warm_first_ms = ms("check_warm_first")
materialize_cold_ms = ms("materialize_cold")
materialize_warm_ms = ms("materialize_warm")
verify_ms = ms("verify_out")
policy_ms = ms("check_policy")
grant_ms = ms("check_grant")

receipt = {
    "kind": "training-ready-cost",
    "generated_utc": stamp,
    "result": "pass" if all(c["ok"] for c in checks) else "fail",
    "environment": {
        "revisions": {
            "WeftFS": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "WeftCodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
            "WeftCodec-Go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
            "WeftFS-Go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
        },
        "court": "fuse",
        "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
        "limits": {
            "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
            "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
            "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        },
        "cache": "warm, single host",
    },
    "corpus": {
        "label": "Qwen/Qwen2.5-0.5B BF16, 2 raw shards, cached store copy",
        "source_sha256": src_sha,
        "tensors": len(checkpoint.get("tensors", [])),
        "shard_objects": checkpoint.get("shard_objects"),
        "element_bytes": checkpoint.get("total_element_bytes"),
        "materialized_bytes": int(materialized_bytes),
        "not_committed": "weights are referenced from the local cache, never committed",
    },
    "ids": {
        "context_root_id": context.get("context_root_id"),
        "snapshot_id": snapshot.get("snapshot_id") or snapshot.get("commit"),
        "selector_id": context.get("selector_id"),
        "artifact_root_used_for_grant": context.get("context_root_id"),
    },
    "timings": {
        "checkpoint_write_ms": ms("checkpoint"),
        "snapshot_ms": ms("snapshot"),
        "context_ms": ms("context"),
        "check_cold_ms": cold_check_ms,
        "check_warm_first_ms": warm_first_ms,
        "check_warm_p50_ms": warm_p50,
        "check_warm_min_ms": warm_walls[0],
        "check_warm_max_ms": warm_walls[-1],
        "check_policy_invalidated_ms": policy_ms,
        "check_grant_invalidated_ms": grant_ms,
        "materialize_cold_ms": materialize_cold_ms,
        "materialize_warm_ms": materialize_warm_ms,
        "verify_out_ms": verify_ms,
    },
    "split": {
        "cold_check_validation_and_decode_ms": cold.get("cost", {}).get("actual_decode_ms"),
        "cold_check_reported_ms": cold_check_ms,
        "materialize_cold_reported_decode_ms": materialize_cold.get("cost", {}).get("actual_decode_ms"),
        "materialize_cold_write_ms": max(0.0, materialize_cold_ms - materialize_cold.get("cost", {}).get("actual_decode_ms", 0)),
        "warm_check_plan_ms": cache_of(warm_first).get("plan_ms"),
        "warm_check_verify_ms": cache_of(warm_first).get("verify_ms"),
        "time_to_ready_cold_ms": cold_check_ms + materialize_cold_ms,
        "time_to_ready_warm_ms": warm_first_ms + materialize_warm_ms,
    },
    "cache": {
        "cold": cache_of(cold),
        "warm_first": cache_of(warm_first),
        "policy_invalidated": cache_of(policy),
        "grant_invalidated": cache_of(grant),
    },
    "checks": checks,
}
with open("bench/receipts/training-ready-cost-%s.json" % stamp, "w") as handle:
    json.dump(receipt, handle, indent=2, sort_keys=True)
    handle.write("\n")
for check in checks:
    print(("PASS " if check["ok"] else "FAIL ") + check["name"] + ": " + str(check["detail"]))
print("result=" + receipt["result"])
if receipt["result"] != "pass":
    sys.exit(1)
PY

rm -rf "$work/materialized" "$work/materialized-2"
