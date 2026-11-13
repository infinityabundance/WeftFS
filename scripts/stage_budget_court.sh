#!/usr/bin/env bash
# Court: the stage-budget admission contract (X06/N09 defect fix, X31).
#
# The two shipped `stage_budget` tests this court pins were broken: an oversize
# admission never blocked other requests, and one test joined a waiter whose
# bytes were held until after the join. A concurrency primitive that can hang is
# a product defect, so the evidence is a repetition count at several thread
# counts with a per-run `timeout` (a hang is a recorded timeout, not a stall).
#
# Usage:
#   scripts/stage_budget_court.sh [--reps N]
#
# Environment:
#   WEFT_STAGE_BUDGET_REPS   repetitions per thread count (default 30)
#
# Writes `bench/receipts/stage-budget-fix-<stamp>.json` and a `.log` beside it.
# The repetitions run in the rust court at the unchanged hard limits
# (3 GiB / 3 GiB swap / 1024 pids / 4 cpus); there is no way to raise them here.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

reps="${WEFT_STAGE_BUDGET_REPS:-30}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --reps)
      shift
      [[ $# -gt 0 ]] || { echo "--reps needs a value" >&2; exit 64; }
      reps="$1"
      shift
      ;;
    -h|--help)
      sed -n '2,17p' "$0"
      exit 0
      ;;
    *)
      echo "stage_budget_court: unknown argument '$1'" >&2
      exit 64
      ;;
  esac
done
[[ "$reps" =~ ^[1-9][0-9]*$ ]] || {
  echo "stage_budget_court: reps must be a positive integer (got '$reps')" >&2
  exit 64
}

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "stage_budget_court: skip docker daemon is not reachable" >&2
  exit 78
fi
if ! docker image inspect weft/rust-court:1 >/dev/null 2>&1; then
  echo "stage_budget_court: skip court image weft/rust-court:1 is not present" >&2
  exit 78
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
revision="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
go_revision="$(git -C WeftFS-Go rev-parse HEAD 2>/dev/null || echo unknown)"
host_kernel="$(uname -r)"
docker_server="$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo unknown)"
image_id="$(docker image inspect --format '{{.Id}}' weft/rust-court:1 2>/dev/null || echo unknown)"

receipts="bench/receipts"
work=".docker-cache/stage-budget-court-$stamp"
mkdir -p "$work" "$receipts"
log="$receipts/stage-budget-fix-$stamp.log"
: > "$log"

note() { printf '%s\n' "$*" | tee -a "$log" >&2; }

note "stage_budget_court: stamp $stamp revision $revision reps $reps"

overall=0
for threads in 8 2 1; do
  note "stage_budget_court: --test-threads=$threads reps=$reps"
  if ! scripts/docker-court.sh rust \
    "cd /work && bash bench/scripts/stage_budget_repeat.sh $threads $reps /work/$work/threads-$threads.tsv 120" \
    >>"$log" 2>&1; then
    overall=1
    note "stage_budget_court: FAIL at --test-threads=$threads"
  fi
done

python3 - "$stamp" "$revision" "$go_revision" "$host_kernel" "$docker_server" \
  "$image_id" "$work" "$receipts" "$reps" <<'PY'
import json
import os
import sys

stamp, revision, go_revision, kernel, docker_server, image_id, work, receipts, reps = sys.argv[1:]
requested = int(reps)

configs = []
total_runs = 0
total_failed = 0
total_timeouts = 0
missing = []
test_count = None
for threads in (8, 2, 1):
    path = os.path.join(work, f"threads-{threads}.tsv")
    rows = []
    if not os.path.exists(path):
        missing.append(threads)
    else:
        with open(path) as handle:
            for line in handle:
                line = line.rstrip("\n")
                if not line:
                    continue
                if line.startswith("#tests\t"):
                    test_count = int(line.split("\t")[1])
                    continue
                iteration, rc, elapsed = line.split("\t")
                rows.append(
                    {"iteration": int(iteration), "rc": int(rc), "elapsed_ms": int(elapsed)}
                )
    passed = sum(1 for row in rows if row["rc"] == 0)
    failed = len(rows) - passed
    timeouts = sum(1 for row in rows if row["rc"] == 124)
    total_runs += len(rows)
    total_failed += failed
    total_timeouts += timeouts
    configs.append(
        {
            "test_threads": threads,
            "requested_runs": requested,
            "runs": len(rows),
            "passed": passed,
            "failed": failed,
            "timeouts": timeouts,
            "max_elapsed_ms": max((row["elapsed_ms"] for row in rows), default=0),
            "rows": rows,
        }
    )

complete = not missing and all(config["runs"] == requested for config in configs)
result = "pass" if complete and total_failed == 0 and total_timeouts == 0 else "fail"

receipt = {
    "schema": "weft.stage_budget_fix.receipt.v1",
    "gate": "X31",
    "stamp": stamp,
    "revision": revision,
    "result": result,
    "reps": requested,
    "test_count": test_count,
    "configs": configs,
    "total_runs": total_runs,
    "failed": total_failed,
    "timeouts": total_timeouts,
    "missing_configs": missing,
    "court": {
        "kind": "rust",
        "image": "weft/rust-court:1",
        "image_id": image_id,
        "host_kernel": kernel,
        "docker_server": docker_server,
        "limits": {
            "memory": "3g",
            "memory_swap": "3g",
            "pids": 1024,
            "cpus": 4,
            "tmpfs": "1g",
            "no_new_privileges": True,
        },
        "revision_weftfs": revision,
        "revision_weftfs_go": go_revision,
    },
    "defect": {
        "observed_at_revision": "0a7beb7",
        "oversize_failure": "a_byte_request_larger_than_the_budget_runs_alone_and_completes panicked: the normal waiter was granted after 41.358 us while the oversize admission was still live (the assertion required >= 25 ms)",
        "reservation_hang": "releasing_workers_keeps_the_byte_reservation did not terminate; `timeout 25 cargo test ... --exact` exited 124",
        "fifo_hole": "a cancelled non-head waiter left the cursor on a ticket nobody owned; a public-API reproduction at 0a7beb7 failed with `worker did not finish in 5s: FIFO hole`",
        "logs": [
            f"bench/receipts/stage-budget-prefix-oversize-{stamp}.log",
            f"bench/receipts/stage-budget-prefix-hang-{stamp}.log",
            f"bench/receipts/stage-budget-prefix-fifo-hole-{stamp}.log",
        ],
    },
    "command": "scripts/stage_budget_court.sh",
}
out = os.path.join(receipts, f"stage-budget-fix-{stamp}.json")
with open(out, "w") as handle:
    json.dump(receipt, handle, indent=2, sort_keys=True)
    handle.write("\n")
print(out)
print(result)
PY

[[ "$overall" == "0" ]] || exit 1
echo "$stamp"
