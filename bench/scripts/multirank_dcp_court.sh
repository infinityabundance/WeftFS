#!/usr/bin/env bash
# T056: four real ranks publish one checkpoint; four ranks restore it.
#
#   scripts/docker-court.sh python "bash bench/scripts/multirank_dcp_court.sh"
#
# CPU only, one host, gloo over loopback. No GPU and no multi-host claim.
set -Eeuo pipefail

cd /work
stamp="${WEFT_MR_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="${WEFT_MR_RECEIPT:-bench/receipts/multirank-dcp-${stamp}.json}"
log="${receipt%.json}.log"
work="${WEFT_MR_WORK:-/work/.docker-cache/multirank-${stamp}}"
cli="${WEFTFS_CLI:-/work/.docker-cache/target-rust/release/weftfs}"
port="${WEFT_MR_PORT:-29511}"

skip() {
  echo "multirank_dcp_court: skip $1"
  exit 78
}

[[ -x "$cli" ]] || skip "missing release binary $cli"
command -v torchrun >/dev/null || skip "torchrun is not on PATH"
mkdir -p "$(dirname "$receipt")" "$work"

export PYTHONPATH="/work/python${PYTHONPATH:+:$PYTHONPATH}"
export OMP_NUM_THREADS=1 MKL_NUM_THREADS=1 OPENBLAS_NUM_THREADS=1

run_torchrun() {
  torchrun --nnodes=1 --nproc_per_node=4 --master_port="$port" \
    python/courts/multirank_dcp.py "$@"
}

echo "multirank court: clean 4-rank publish" | tee "$log"
run_torchrun save --work "$work" --store "$work/store" --cli "$cli" \
  --tag multi --clean-store 2>&1 | tee -a "$log"

for fault in worker_pre coordinator_pre during_publish; do
  echo "multirank court: fault $fault" | tee -a "$log"
  set +e
  run_torchrun save --work "$work" --store "$work/store" --cli "$cli" \
    --tag "fault-$fault" --fault "$fault" >>"$log" 2>&1
  echo "torchrun fault=$fault exit=$?" | tee -a "$log"
  set -e
  "$cli" --store "$work/store" --json checkpoint list >"$work/list-$fault.json" 2>>"$log" || true
done

echo "multirank court: retry after faults (new tag)" | tee -a "$log"
run_torchrun save --work "$work" --store "$work/store" --cli "$cli" \
  --tag retry 2>&1 | tee -a "$log"

echo "multirank court: 4-rank load into 4->2, 4->1, 4->8" | tee -a "$log"
run_torchrun load --work "$work" --store "$work/store" --cli "$cli" \
  --tag multi --topologies 2,1,8 2>&1 | tee -a "$log"

python3 - "$work" "$receipt" "$stamp" "$cli" <<'PY'
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

work = Path(sys.argv[1])
receipt_path = Path(sys.argv[2])
stamp = sys.argv[3]
cli = sys.argv[4]

checks = {}
save_result = json.loads((work / "result-save-multi.json").read_text())
verify_stdout = save_result.get("verify_stdout", "")
try:
    verified_tensors = json.loads(verify_stdout).get("tensors_verified", 0)
except ValueError:
    verified_tensors = 0
checks["publish_ok"] = bool(save_result.get("published"))
checks["verify_ok"] = bool(save_result.get("verified"))
checks["shard_objects"] = (
    verified_tensors > 0 and save_result.get("shard_objects") == 4 * verified_tensors
)

faults = {}
for fault in ("worker_pre", "coordinator_pre", "during_publish"):
    listed = work / ("list-%s.json" % fault)
    names = []
    if listed.is_file() and listed.stat().st_size:
        try:
            names = [row["name"] for row in json.loads(listed.read_text()).get("checkpoints", [])]
        except ValueError:
            names = []
    result_path = work / ("result-save-fault-%s.json" % fault)
    result = json.loads(result_path.read_text()) if result_path.is_file() else None
    if fault == "during_publish" and result is not None:
        # The committer was killed; rank 0 inspected the store itself.
        published = bool(result.get("published"))
        names = result.get("visible_checkpoints", names)
    else:
        published = ("fault-%s" % fault) in names
    faults[fault] = {
        "torchrun_result": result,
        "visible_checkpoints": names,
        "published": published,
        "no_checkpoint": not published,
    }
    checks["fault_%s_no_checkpoint" % fault] = not published

retry = json.loads((work / "result-save-retry.json").read_text())
checks["retry_publishes"] = bool(retry.get("published")) and bool(retry.get("verified"))

load_rows = []
for path in sorted(work.glob("result-load-multi-rank*.json")):
    payload = json.loads(path.read_text())
    for row in payload["rows"].values():
        load_rows.append(row)
checks["load_all_ranks_exact"] = all(row.get("exact") for row in load_rows)
checks["load_fetched_set_matches_plan"] = all(
    row.get("fetched_set_matches_plan") for row in load_rows
)
by_topology = {}
for row in load_rows:
    bucket = by_topology.setdefault(str(row["topology"]), {
        "ranks": 0,
        "needed_bytes": 0,
        "backend_bytes_read": 0,
        "representation_bytes": 0,
        "mismatched": [],
    })
    bucket["ranks"] += 1
    bucket["needed_bytes"] += row.get("needed_bytes", 0)
    bucket["backend_bytes_read"] += row.get("backend_bytes_read", 0)
    bucket["representation_bytes"] += row.get("representation_bytes", 0)
    bucket["mismatched"].extend(row.get("mismatched_tensors", []))
for bucket in by_topology.values():
    bucket["read_amplification"] = (
        bucket["backend_bytes_read"] / bucket["needed_bytes"]
        if bucket["needed_bytes"]
        else None
    )
    bucket["representation_amplification"] = (
        bucket["representation_bytes"] / bucket["needed_bytes"]
        if bucket["needed_bytes"]
        else None
    )
checks["load_representation_amplification_matches_arithmetic"] = all(
    abs(bucket["representation_amplification"] - max(1.0, int(topology) / 4.0))
    < 1e-6
    for topology, bucket in by_topology.items()
)

receipt = {
    "schema": "weft.multirank-dcp.v1",
    "stamp": stamp,
    "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "court": {
        "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
        "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
        "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
        "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        "cpu_only": True,
        "single_host": True,
        "backend": "gloo",
        "ranks": 4,
        "note": "localhost gloo, CPU only; no GPU, multi-host, or fleet claim",
    },
    "revision": {
        "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
    },
    "method": {
        "publish": "each rank writes its own tensor slices; rank 0 reassembles and issues one checkpoint write (one commit)",
        "load": "each destination rank uses checkpoint restore --rank/--rank-out (T053)",
        "faults": "SIGKILL of a worker before its fragment, of rank 0 after all fragments, and of the publishing CLI during publication",
    },
    "publish": save_result,
    "faults": faults,
    "retry": retry,
    "load": {
        "rows": load_rows,
        "by_topology": by_topology,
        "stored_shards": 4,
        "expected_representation_amplification": "max(1, target_shards / 4): a target finer than the stored split makes several destination ranks fetch the same stored shard",
        "full_reshuffle_needed": False,
        "note": "the rank-directed path fetches only intersecting stored shards; no rank materialises the full container",
    },
    "checks": checks,
    "losses": [
        "The four ranks contribute through a shared host staging directory; the store publication itself is one process. This is not an N-way parallel store writer.",
        "The model is built deterministically by every rank, so the court can verify any slice locally; it is not a distributed-training accuracy claim.",
        "SIGKILL is a process fault; the host page cache is not dropped.",
        "A target topology finer than the stored split (8 ranks over a 4-way checkpoint) makes two destination ranks fetch the same stored shard; the measured representation amplification is 2.0 and is published.",
    ],
    "result": "pass" if all(checks.values()) else "fail",
}
receipt_path.parent.mkdir(parents=True, exist_ok=True)
receipt_path.write_text(json.dumps(receipt, indent=2))
print(json.dumps({"result": receipt["result"], "checks": checks}, indent=2))
sys.exit(0 if receipt["result"] == "pass" else 1)
PY
