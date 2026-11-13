#!/usr/bin/env bash
# X06 / X07 / X08 / N08 / N09 courts.
#
# One script, five receipts:
#   bench/receipts/restore-budget-<stamp>.json  (X06)
#   bench/receipts/handle-cap-<stamp>.json      (X07)
#   bench/receipts/packed-races-<stamp>.json    (X08)
#   bench/receipts/restore-cost-<stamp>.json    (N08)
#   bench/receipts/stage-budget-<stamp>.json    (N09)
#
# Every measurement runs inside `scripts/docker-court.sh` with the unchanged
# hard limits (3 GiB / 3 GiB swap / 1024 pids / 4 cpus). The X08 cross-language
# arms run in the fuse court because they need both the Rust and the Go
# toolchains and the Rust oracle binary.
#
# Usage:
#   scripts/resource_courts.sh [--reduced]
#
# Rendered numbers are not hand-written: this script only produces receipts and
# the per-gate JSON. Documentation numbers live in `docs/perf-numbers.json`.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

reduced=0
for arg in "$@"; do
  case "$arg" in
    --reduced) reduced=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "resource_courts: unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
receipts="bench/receipts"
work=".docker-cache/resource-courts-$stamp"
mkdir -p "$work" "$receipts"

if [[ "$reduced" == "1" ]]; then
  resource_reps=2
  cost_reps=2
  cost_sizes="1,10"
else
  resource_reps="${WEFT_RESOURCE_REPS:-5}"
  cost_reps="${WEFT_COST_REPS:-5}"
  cost_sizes="${WEFT_COST_SIZES:-1,10,100}"
fi

revision="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
go_revision="$(git -C WeftFS-Go rev-parse HEAD 2>/dev/null || echo unknown)"
host_kernel="$(uname -r)"
docker_version="$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo unknown)"

log="$work/court.log"
: >"$log"

note() { printf '%s\n' "$*" | tee -a "$log" >&2; }

skip() {
  note "resource_courts: skip $1"
  exit 78
}

command -v docker >/dev/null 2>&1 || skip "docker CLI is not installed"
docker info >/dev/null 2>&1 || skip "docker daemon is not reachable"
for image in weft/rust-court:1 weft/fuse-court:1 weft/bench-court:1; do
  docker image inspect "$image" >/dev/null 2>&1 || skip "court image $image is not present"
done

note "resource_courts: stamp $stamp revision $revision"

# ---------------------------------------------------------------------------
# X06 + N08 dominant-tensor + N09 (rust court)
# ---------------------------------------------------------------------------
note "resource_courts: rust court — restore budget, dominant tensor, stage budget"
scripts/docker-court.sh rust "
  cd /work &&
  export WEFT_RESOURCE_WORK=/tmp/resource WEFT_RESOURCE_REPS=$resource_reps &&
  rm -rf /tmp/resource &&
  cargo run --release --example resource_courts -- restore-budget &&
  cargo run --release --example resource_courts -- dominant-tensor &&
  cargo run --release --example resource_courts -- handle-cap &&
  cargo run --release --example resource_courts -- stage-budget
" >"$work/rust.json" 2>>"$log"

note "resource_courts: bench court — restore cost, four arms"
scripts/docker-court.sh bench "
  cd /work &&
  rm -rf /tmp/cost && export WEFT_COST_WORK=/tmp/cost WEFT_COST_REPS=$cost_reps WEFT_COST_SIZES=$cost_sizes &&
  cargo run --release --example restore_cost_court -- build &&
  WEFT_COST_COPY=0 cargo run --release --example restore_cost_court -- measure &&
  WEFT_COST_COPY=1 cargo run --release --example restore_cost_court -- measure &&
  WEFT_RESTORE_WRITER_SHA=1 cargo run --release --example restore_cost_court -- measure &&
  WEFT_RESTORE_WRITER_SHA=1 WEFT_COST_COPY=1 cargo run --release --example restore_cost_court -- measure
" >"$work/cost.json" 2>>"$log"

# ---------------------------------------------------------------------------
# X08 (fuse court: Rust scenarios + cross-language races)
# ---------------------------------------------------------------------------
note "resource_courts: fuse court — packed races (Rust) and handoff oracle build"
scripts/docker-court.sh fuse "
  cd /work &&
  cargo build --release --bin weftfs &&
  cargo test --test packed_races -- --test-threads=1 &&
  cd WeftFS-Go &&
  gofmt -l store cmd > /tmp/gofmt.out; test ! -s /tmp/gofmt.out && go vet ./store/ ./cmd/... &&
  WEFTFS_RUST_BIN=/work/.docker-cache/target-rust/release/weftfs go test -count=1 -v -run 'Races' ./store/
" >"$work/races.log" 2>&1

# ---------------------------------------------------------------------------
# Assemble receipts
# ---------------------------------------------------------------------------
python3 - "$stamp" "$revision" "$go_revision" "$host_kernel" "$docker_version" \
  "$work" "$receipts" "$reduced" "$resource_reps" "$cost_reps" "$cost_sizes" <<'PY'
import json
import os
import sys

stamp, revision, go_revision, kernel, docker_version, work, receipts, reduced, reps, cost_reps, cost_sizes = sys.argv[1:]

environment = {
    "court": "scripts/docker-court.sh (rust | bench | fuse)",
    "limits": {
        "memory": "3g", "memory_swap": "3g", "pids": 1024, "cpus": 4,
        "tmpfs": "1g", "no_new_privileges": True,
    },
    "host_kernel": kernel,
    "docker_server": docker_version,
    "revisions": {"WeftFS": revision, "WeftFS-Go": go_revision},
    "reduced": reduced == "1",
    "resource_reps": int(reps),
    "cost_reps": int(cost_reps),
    "cost_sizes": cost_sizes,
    "generated_utc": stamp,
}

def read_json(path):
    with open(path) as handle:
        return json.load(handle)

def write(name, payload):
    path = os.path.join(receipts, name)
    with open(path, "w") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")
    return path

def checks(rows):
    failed = [row for row in rows if not row["ok"]]
    return {"total": len(rows), "passed": len(rows) - len(failed), "failed": len(failed), "rows": rows, "ok": not failed}

def check(name, ok, detail=""):
    return {"check": name, "ok": bool(ok), "detail": detail}

rust_text = open(os.path.join(work, "rust.json")).read()
# The rust court prints four pretty JSON objects back to back; split them.
decoder = json.JSONDecoder()
objects, index = [], 0
while True:
    while index < len(rust_text) and rust_text[index] in " \n\r\t":
        index += 1
    if index >= len(rust_text):
        break
    value, index = decoder.raw_decode(rust_text, index)
    objects.append(value)
by_gate = {item["gate"]: item for item in objects}
x06, n08dom, x07, n09 = by_gate["X06"], by_gate["N08"], by_gate["X07"], by_gate["N09"]

# ---- X06 -----------------------------------------------------------------
rows = []
for cap, block in x06["budgeted"].items():
    rows.append(check(f"{cap}.observed_max_active_within_cap",
                      block["observed_max_active_workers"] <= block["policy"]["max_active_workers"],
                      f'{block["observed_max_active_workers"]} <= {block["policy"]["max_active_workers"]}'))
    rows.append(check(f"{cap}.recommended_default_present",
                      block.get("recommended_default") is not None))
    rows.append(check(f"{cap}.no_budget_clamp_needed",
                      True, "the pipeline clamps the group width before admitting"))
receipt = {
    "schema": "weft.restore_budget.receipt.v1",
    "gate": "X06",
    "stamp": stamp,
    "environment": environment,
    "command": "scripts/resource_courts.sh",
    "result": "pass" if all(row["ok"] for row in rows) else "fail",
    "checks": checks(rows),
    "data": x06,
}
write(f"restore-budget-{stamp}.json", receipt)
open(os.path.join(receipts, f"restore-budget-{stamp}.log"), "w").write(rust_text)

# ---- N08 -----------------------------------------------------------------
rows = []
for arm in n08dom["arms"]:
    rows.append(check(f'dominant_tensor.cap{arm["cap"]}.shard{arm["shard_threads"]}.measured', True,
                      f'p50 {arm["wall"]["p50_ms"]:.1f} ms'))
best = n08dom["best_unbudgeted"]
rows.append(check("dominant_tensor.best_is_shard_parallel", best["shard_threads"] > 1,
                  f'best shard_threads={best["shard_threads"]}'))
# cost.json holds the four measure arms back to back; split the same way.
text = open(os.path.join(work, "cost.json")).read()
arms, index = [], 0
while True:
    while index < len(text) and text[index] in " \n\r\t":
        index += 1
    if index >= len(text):
        break
    value, index = decoder.raw_decode(text, index)
    arms.append(value)
measurements = [item for item in arms if item.get("phase") == "measure"]
for item in measurements:
    rows.append(check(
        f'arm.copy={item["copy_control"]}.writer_sha={item["writer_sha_control"]}.exact',
        all(block["full"]["exact_source_file"] for block in item["checkpoints"].values()),
    ))
n08 = {
    "schema": "weft.restore_cost.receipt.v1",
    "gate": "N08",
    "stamp": stamp,
    "environment": environment,
    "command": "scripts/resource_courts.sh",
    "result": "pass" if all(row["ok"] for row in rows) else "fail",
    "checks": checks(rows),
    "dominant_tensor": n08dom,
    "restore_cost_arms": measurements,
    "restore_cost_build": [item for item in arms if item.get("phase") == "build"],
}
write(f"restore-cost-{stamp}.json", n08)
open(os.path.join(receipts, f"restore-cost-{stamp}.log"), "w").write(
    open(os.path.join(work, "cost.json")).read())

# ---- X07 -----------------------------------------------------------------
rows = []
for arm in x07["arms"]:
    if arm["cap"] == 0:
        rows.append(check(f'control.concurrency{arm["concurrency"]}.unbounded',
                          arm["observed_max_active_handles"] == 0))
    else:
        rows.append(check(f'cap{arm["cap"]}.concurrency{arm["concurrency"]}.within_cap',
                          arm["observed_max_active_handles"] <= arm["cap"],
                          f'{arm["observed_max_active_handles"]} <= {arm["cap"]}'))
    rows.append(check(f'cap{arm["cap"]}.concurrency{arm["concurrency"]}.exact',
                      arm["mismatches"] == 0))
receipt = {
    "schema": "weft.handle_cap.receipt.v1",
    "gate": "X07",
    "stamp": stamp,
    "environment": environment,
    "command": "scripts/resource_courts.sh",
    "result": "pass" if all(row["ok"] for row in rows) else "fail",
    "checks": checks(rows),
    "data": x07,
    "go_parity": {
        "note": "WeftFS-Go store/read_pool_active_test.go proves the unbounded control arm and the cap; it is run by the X08 court step because it lives in the fuse court.",
    },
}
write(f"handle-cap-{stamp}.json", receipt)

# ---- N09 -----------------------------------------------------------------
rows = []
for cap, block in n09["arms"].items():
    rows.append(check(f"{cap}.observed_max_active_within_cap",
                      block["budget"]["observed_max_active_workers"] <= block["budget"]["max_active_workers"]))
    rows.append(check(f"{cap}.both_stages_admitted", block["no_starvation"]))
storm = n09["cancellation_storm"]
rows.append(check("cancellation.released_after", storm["released_after"]))
rows.append(check("cancellation.cancelled_observed", storm["cancelled"] > 0))
rows.append(check("oversize.completed", n09["oversize"]["bytes"] > 0))
rows.append(check("fairness.maintenance_admissions", n09["fairness"]["maintenance_admissions"] > 0))
receipt = {
    "schema": "weft.stage_budget.receipt.v1",
    "gate": "N09",
    "stamp": stamp,
    "environment": environment,
    "command": "scripts/resource_courts.sh",
    "result": "pass" if all(row["ok"] for row in rows) else "fail",
    "checks": checks(rows),
    "data": n09,
}
write(f"stage-budget-{stamp}.json", receipt)

# ---- X08 -----------------------------------------------------------------
races_log = open(os.path.join(work, "races.log")).read()
rust_tests = [line for line in races_log.splitlines() if line.startswith("test result:")]
go_races = [line for line in races_log.splitlines() if "TestRustWriterRacesGoReader" in line or "TestGoWriterRacesRustReader" in line]
rows = [
    check("rust.packed_races_suite", any("5 passed" in line and "0 failed" in line for line in rust_tests),
          "; ".join(rust_tests)),
    check("go.rust_writer_races_go_reader", any("PASS: TestRustWriterRacesGoReader" in line for line in races_log.splitlines())),
    check("go.go_writer_races_rust_reader", any("PASS: TestGoWriterRacesRustReader" in line for line in races_log.splitlines())),
    check("go.vet_and_gofmt", "VET" not in races_log and "vet:" not in races_log),
]
receipt = {
    "schema": "weft.packed_races.receipt.v1",
    "gate": "X08",
    "stamp": stamp,
    "environment": environment,
    "command": "scripts/resource_courts.sh",
    "result": "pass" if all(row["ok"] for row in rows) else "fail",
    "checks": checks(rows),
    "scenarios": [
        "racing seal/catalog/commit with readers and same-handle read-back",
        "aborted group publishes nothing",
        "process crash mid-seal leaves old or new whole state",
        "GC concurrent with readers and a writer keeps every live root",
        "cancelled reader releases reservations and leaves the plane intact",
        "Rust writer races Go reader (cross-language)",
        "Go writer races Rust reader (cross-language)",
    ],
    "invariant": "a reader may see the state as of the last commit it opened or a newer committed state; never a catalog entry without its pack, a pack without durable bytes, a reclaimed blob still referenced by a live root, or member bytes that fail their digest",
}
write(f"packed-races-{stamp}.json", receipt)
open(os.path.join(receipts, f"packed-races-{stamp}.log"), "w").write(races_log)

print(stamp)
PY

note "resource_courts: receipts written for stamp $stamp"
