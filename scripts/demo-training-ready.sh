#!/usr/bin/env bash
# P7 training-ready court.
#
# Run only inside the OOM-protected court:
#   scripts/docker-court.sh fuse "bash scripts/demo-training-ready.sh"
#
# Uses the cached P3 Qwen2.5-0.5B material. A copy of the cached store is used
# so the original remains untouched. The checkpoint is written from the cached
# BF16 safetensors container, a Context Root is pinned over only the checkpoint
# objects, and `materialize --training-ready --require-tensors` must verify
# every byte and every tensor root before publishing an output directory.
set -Eeuo pipefail

ROOT=/work
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
WORK="$ROOT/.docker-cache/training-ready/$STAMP"
STORE="$WORK/store"
OUT="$WORK/materialized"
SRC_STORE="${WEFT_TRAINING_SOURCE_STORE:-$ROOT/.docker-cache/real-corpus/store-qwen-base}"
WEIGHTS="${WEFT_TRAINING_SOURCE_WEIGHTS:-$ROOT/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors}"
RECEIPT="$ROOT/bench/receipts/training-ready-$STAMP.json"
LOG="$ROOT/bench/receipts/training-ready-$STAMP.log"
RUSTFS="${CARGO_TARGET_DIR:-$ROOT/.docker-cache/target-rust}/release/weftfs"
FIXED_TS=1767225600000000000

mkdir -p "$WORK" "$ROOT/bench/receipts"
exec > >(tee "$LOG") 2>&1
trap 'echo "FAILED at line $LINENO" >&2' ERR

echo "kind: training-ready"
echo "timestamp_utc: $STAMP"
echo "court: fuse"
echo "revisions:"
echo "  WeftFS: ${WEFT_REPO_WEFTFS:-unknown}"
echo "  WeftCodec: ${WEFT_REPO_WEFTCODEC:-unknown}"
echo "  WeftCodec-Go: ${WEFT_REPO_WEFTCODEC_GO:-unknown}"
echo "  WeftFS-Go: ${WEFT_REPO_WEFTFS_GO:-unknown}"
echo "limits: memory=${WEFT_COURT_MEMORY:-unknown} cpus=${WEFT_COURT_CPUS:-unknown} pids=${WEFT_COURT_PIDS:-unknown} nproc=${WEFT_COURT_NPROC:-unknown}"
echo "kernel: ${WEFT_COURT_KERNEL:-unknown}"
echo "image: ${WEFT_COURT_IMAGE:-unknown} ${WEFT_COURT_IMAGE_ID:-unknown}"
echo "source_store: $SRC_STORE"
echo "source_weights: $WEIGHTS"

echo "== build rust binary =="
( cd "$ROOT" && cargo build --release --quiet --bin weftfs )
test -x "$RUSTFS"

CORPUS_LABEL="real cached Qwen/Qwen2.5-0.5B weights; copy of the P3 cached store"
if [ -d "$SRC_STORE" ] && [ -f "$WEIGHTS" ]; then
  echo "== copy cached P3 store =="
  rm -rf "$STORE"
  cp -a "$SRC_STORE" "$STORE"
else
  CORPUS_LABEL="synthetic fallback (cached Qwen store/weights unavailable)"
  echo "== cached corpus unavailable; creating a small store and container =="
  rm -rf "$STORE"
  mkdir -p "$WORK"
  "$RUSTFS" --store "$STORE" --fixed-timestamp-ns "$FIXED_TS" init >"$WORK/init.out" 2>"$WORK/init.err"
  WEIGHTS="$WORK/tiny.safetensors"
  python3 - "$WEIGHTS" <<'PY'
import sys
import numpy as np
from safetensors.numpy import save_file
path = sys.argv[1]
save_file(
    {
        "layer0.weight": np.arange(4096, dtype=np.float32).reshape(64, 64),
        "layer0.bias": np.arange(64, dtype=np.float32),
        "layer1.weight": (np.arange(8192, dtype=np.float32).reshape(64, 128) * 0.5),
    },
    path,
)
PY
fi

timeit() {
  local label="$1"; shift
  local start end
  start="$(date +%s%N)"
  "$@" >"$WORK/$label.out" 2>"$WORK/$label.err"
  end="$(date +%s%N)"
  printf '%s %s\n' "$start" "$end" >"$WORK/$label.ns"
}

echo "== checkpoint write =="
timeit checkpoint "$RUSTFS" --store "$STORE" --fixed-timestamp-ns "$FIXED_TS" --json \
  checkpoint write --file "$WEIGHTS" --name qwen --shards 2 --axis 0 --shard-strategy raw
python3 - "$WORK/checkpoint.out" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
print("tensors=%s shards=%s element_bytes=%s source_sha256=%s" % (
    len(r["tensors"]), r["shards"], r["total_element_bytes"], r["source_sha256"]))
open(sys.argv[1] + ".summary", "w").write(json.dumps({
    "tensors": len(r["tensors"]),
    "shards": r["shards"],
    "total_element_bytes": r["total_element_bytes"],
    "source_sha256": r["source_sha256"],
    "shard_objects": r["shard_objects"],
}))
PY

echo "== snapshot =="
timeit snapshot "$RUSTFS" --store "$STORE" --fixed-timestamp-ns "$FIXED_TS" --json \
  snapshot create training

echo "== context root over checkpoint objects only =="
SELECTOR='select has(attrs.weft.checkpoint.schema)'
timeit context "$RUSTFS" --store "$STORE" --fixed-timestamp-ns "$FIXED_TS" --json \
  context create --name qwen-training --selector "$SELECTOR" --snapshot training

echo "== training-ready --check (writes nothing) =="
timeit check "$RUSTFS" --store "$STORE" --fixed-timestamp-ns "$FIXED_TS" --json \
  training-ready --root qwen-training --check --require-tensors

echo "== materialize --training-ready =="
timeit materialize "$RUSTFS" --store "$STORE" --fixed-timestamp-ns "$FIXED_TS" --json \
  materialize --root qwen-training --training-ready --out "$OUT" --require-tensors

echo "== verify the materialized directory offline =="
timeit verify "$RUSTFS" --fixed-timestamp-ns "$FIXED_TS" --json \
  training-ready --verify-out "$OUT"

echo "== compare the checkpoint's source tensor digest with the source container =="
SRC_SHA="$(sha256sum "$WEIGHTS" | awk '{print $1}')"
CKPT_SHA="$(python3 - "$WORK/checkpoint.out.summary" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))["source_sha256"])
PY
)"
test "$SRC_SHA" = "$CKPT_SHA"
echo "source_sha256=$SRC_SHA checkpoint_source_sha256=$CKPT_SHA"
python3 - "$WORK/check.out" "$WORK/materialize.out" "$WORK/verify.out" "$WORK/checkpoint.out.summary" <<'PY'
import json, sys
check = json.load(open(sys.argv[1]))
materialize = json.load(open(sys.argv[2]))
verify = json.load(open(sys.argv[3]))
checkpoint = json.load(open(sys.argv[4]))
assert check["preconditions"]["resolution_exact"] is True
assert check["preconditions"]["content_verified"] is True
assert check["preconditions"]["causal_complete"] is True
assert check["preconditions"]["authorized"] is True
assert check["preconditions"]["tensors_verified"] is True
assert check["preconditions"]["budget_enforced"] is True
assert materialize["preconditions"]["tensors_verified"] is True
assert materialize["checkpoint_tensors_verified"] == checkpoint["tensors"]
assert verify["checkpoint_tensors_verified"] == checkpoint["tensors"]
assert verify["checkpoints_verified"] == 1
assert len(materialize["files"]) == verify["files"]
print("assertions: ok")
PY

echo "== receipt =="
python3 - "$RECEIPT" "$WORK" "$STAMP" "$CORPUS_LABEL" "$SRC_SHA" "$CKPT_SHA" "$SRC_STORE" "$WEIGHTS" <<'PY'
import json, os, sys

receipt_path, work, stamp, label, src_sha, ckpt_sha, src_store, weights = sys.argv[1:9]

def timing(name):
    start, end = map(int, open(os.path.join(work, name + ".ns")).read().split())
    rss = None
    try:
        rss = json.load(open(os.path.join(work, name + ".out"))).get("peak_rss_kib")
    except Exception:
        pass
    return {"wall_s": (end - start) / 1e9, "max_rss_kib": rss}

def out_json(name):
    return json.load(open(os.path.join(work, name + ".out")))

checkpoint = out_json("checkpoint")
snapshot = out_json("snapshot")
context = out_json("context")
check = out_json("check")
materialize = out_json("materialize")
verify = out_json("verify")
summary = json.load(open(os.path.join(work, "checkpoint.out.summary")))

receipt = {
    "kind": "weftfs.training-ready.receipt/1",
    "timestamp_utc": stamp,
    "label": label,
    "court": "fuse",
    "revision": {
        "WeftFS": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "WeftCodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        "WeftCodec-Go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        "WeftFS-Go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
    },
    "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
    "limits": {
        "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
        "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
        "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        "nproc": os.environ.get("WEFT_COURT_NPROC", "unknown"),
    },
    "corpus": {
        "source_store": src_store,
        "source_weights": weights,
        "source_sha256": src_sha,
        "checkpoint_source_sha256": ckpt_sha,
        "source_match": src_sha == ckpt_sha,
        "tensors": summary["tensors"],
        "shards_per_tensor": summary["shards"],
        "shard_objects": summary["shard_objects"],
        "element_bytes": summary["total_element_bytes"],
        "not_committed": "weights are referenced from the local cache, never committed",
    },
    "ids": {
        "context_root_id": context.get("context_root_id"),
        "snapshot_id": snapshot.get("snapshot_id") or snapshot.get("commit"),
        "selector_id": context.get("selector_id"),
    },
    "timings": {
        "checkpoint_write": timing("checkpoint"),
        "snapshot": timing("snapshot"),
        "context_create": timing("context"),
        "check": timing("check"),
        "materialize": timing("materialize"),
        "verify_out": timing("verify"),
    },
    "check": {
        "preconditions": check["preconditions"],
        "cost": check["cost"],
        "tensor_objects_verified": check["tensor_objects_verified"],
        "checkpoint_tensors_verified": check["checkpoint_tensors_verified"],
        "peak_rss_kib": check.get("peak_rss_kib"),
    },
    "materialize": {
        "preconditions": materialize["preconditions"],
        "cost": materialize["cost"],
        "files": len(materialize["files"]),
        "tensor_objects_verified": materialize["tensor_objects_verified"],
        "checkpoint_tensors_verified": materialize["checkpoint_tensors_verified"],
        "peak_rss_kib": materialize.get("peak_rss_kib"),
    },
    "verify_out": {
        "files": verify["files"],
        "bytes": verify["bytes"],
        "tensor_roots_verified": verify["tensor_roots_verified"],
        "checkpoints_verified": verify["checkpoints_verified"],
        "checkpoint_tensors_verified": verify["checkpoint_tensors_verified"],
        "peak_rss_kib": verify.get("peak_rss_kib"),
    },
    "assertions": {
        "all_preconditions": all(check["preconditions"].values()),
        "source_digest_matches": src_sha == ckpt_sha,
        "materialized_files_verified": verify["files"] > 0,
        "tensor_roots_verified": verify["tensor_roots_verified"] > 0,
        "checkpoint_tensors_match": verify["checkpoint_tensors_verified"] == summary["tensors"],
    },
    "notes": [
        "single host, one court run, warm caches",
        "training-ready is a refusal contract over storage preconditions; it is not a model-quality claim",
        "the source store is a copy of the P3 cached Qwen base store; the checkpoint is written from the cached BF16 container",
        "output files are decoded bytes at their store paths plus manifest.wcb and training-ready.wcb (canonical WCB)",
    ],
}
with open(receipt_path, "w") as handle:
    json.dump(receipt, handle, indent=2, sort_keys=True)
    handle.write("\n")
print("receipt=" + receipt_path)
PY

echo "== done =="
