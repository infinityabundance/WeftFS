#!/usr/bin/env bash
# T024 regression proof: `tests/multi_writer_gc.rs` under repetition, load and a
# single CPU.
#
# Host:  bash scripts/multi_writer_gc_stress.sh
# Court: the script re-enters `scripts/docker-court.sh fuse` once per arm, so the
#        host call needs only the docker CLI. Every hard limit comes from
#        docker-court.sh.
#
# Why this exists. CI run 43631714328 failed
# `concurrent_passes_are_serialised_by_the_epoch_cas` on a two-core runner with
# `Busy: another GC pass holds the epoch; retry after it closes`, because the
# epoch compare-and-swap retried sixteen times without waiting and burned its
# whole budget inside the other pass's read-verify-write window. The test also
# asserted nothing about the epoch *record*, so a pass that finished last could
# rewrite `gc/epoch` with its older epoch and go unnoticed on an idle machine.
#
# Arms
#   threads8   4 CPUs (the court default), 50 runs, `--test-threads=8`
#   load       4 CPUs, 4 busy loops competing, 20 runs, `--test-threads=8`
#   singlecpu  `WEFT_CPUS=1` (the only limit this script ever changes; it is
#              recorded in the receipt), 10 runs, `--test-threads=8`
#
# Every arm runs the store on the bind-mounted project filesystem rather than
# the court's tmpfs (`TMPDIR=.docker-cache/mwgc-stress-tmp`): the defect is a
# wait-budget defect, and a tmpfs makes the critical section too short to
# observe. The CI runner used an ordinary disk.
#
# Writes:
#   bench/receipts/multi-writer-gc-stress-<UTC>.json
#   bench/receipts/multi-writer-gc-stress-<UTC>.log
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${WEFT_IN_COURT:-0}" != "1" ]]; then
  skip() { echo "multi_writer_gc_stress: skip $1" >&2; exit 78; }
  command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
  docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
  cd "$root"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  receipt="bench/receipts/multi-writer-gc-stress-${stamp}.json"
  log="bench/receipts/multi-writer-gc-stress-${stamp}.log"
  mkdir -p bench/receipts
  : >"$log"
  arms=(threads8 load singlecpu)
  for arm in "${arms[@]}"; do
    case "$arm" in
      threads8) cpus=4 ;;
      load) cpus=4 ;;
      singlecpu) cpus=1 ;;
    esac
    fragment="bench/receipts/.mwgc-stress-${stamp}-${arm}.json"
    echo "== arm $arm (WEFT_CPUS=$cpus) ==" | tee -a "$log"
    # `WEFT_CPUS` is the sanctioned court override and the only limit this
    # script sets. The 4-CPU arms name it explicitly so an inherited value
    # cannot silently change them.
    WEFT_CPUS="$cpus" "$root/scripts/docker-court.sh" fuse \
      "WEFT_MWGC_ARM=$arm WEFT_MWGC_STAMP=$stamp WEFT_MWGC_FRAGMENT=$fragment \
       WEFT_MWGC_CPUS=$cpus bash scripts/multi_writer_gc_stress.sh" 2>&1 | tee -a "$log"
  done
  python3 - "$receipt" "$log" "$stamp" "${arms[@]}" <<'PY'
import hashlib, json, os, subprocess, sys
receipt, log, stamp, *arms = sys.argv[1:]
fragments = []
for arm in arms:
    path = "bench/receipts/.mwgc-stress-%s-%s.json" % (stamp, arm)
    with open(path) as handle:
        fragments.append(json.load(handle))
    os.unlink(path)
totals = {
    "arms": len(fragments),
    "runs": sum(f["runs"] for f in fragments),
    "failures": sum(f["failures"] for f in fragments),
    "timeouts": sum(f["timeouts"] for f in fragments),
}
doc = {
    "schema": "weftfs.mw_gc_stress/1",
    "role": "T024 regression proof: multi_writer_gc.rs under repetition, CPU load and a single CPU",
    "generated_at": subprocess.run(["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"],
                                   capture_output=True, text=True).stdout.strip(),
    "stamp": stamp,
    "repositories": [{
        "name": "WeftFS",
        "revision": subprocess.run(["git", "--no-pager", "rev-parse", "HEAD"],
                                   capture_output=True, text=True).stdout.strip(),
    }],
    "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
    "kernel": os.uname().release,
    "test": "tests/multi_writer_gc.rs",
    "totals": totals,
    "pass": totals["failures"] == 0 and totals["timeouts"] == 0 and totals["arms"] == 3,
    "arms": fragments,
    "ran_by": "scripts/multi_writer_gc_stress.sh (host orchestrator, one court per arm)",
    "scope": {
        "checked": "the whole multi_writer_gc suite (8 tests) repeated with the store on a real filesystem, under CPU load and with a single CPU",
        "not_checked": "physical power loss, a fleet, or Go parity; a scheduling-stress court, not a benchmark",
        "label": "one host, one local POSIX namespace, synthetic scheduling",
    },
    "evidence": {"log": log,
                 "log_sha256": hashlib.sha256(open(log, "rb").read()).hexdigest()},
}
with open(receipt, "w") as handle:
    json.dump(doc, handle, indent=2, sort_keys=True)
    handle.write("\n")
print(json.dumps({"receipt": receipt, "pass": doc["pass"], **totals}))
raise SystemExit(0 if doc["pass"] else 1)
PY
  sha256sum "$receipt" "$log"
  python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); raise SystemExit(0 if d["pass"] else 1)' "$receipt"
  exit 0
fi

# ---- inside the court -------------------------------------------------------
cd "$root"
arm="${WEFT_MWGC_ARM:?arm is required}"
stamp="${WEFT_MWGC_STAMP:?stamp is required}"
fragment="${WEFT_MWGC_FRAGMENT:?fragment path is required}"
cpus="${WEFT_MWGC_CPUS:?cpu budget is required}"
tmpdir="$root/.docker-cache/mwgc-stress-tmp"
mkdir -p "$tmpdir"
chmod 0777 "$tmpdir" 2>/dev/null || true
export TMPDIR="$tmpdir"

case "$arm" in
  threads8) runs=50 ;;
  load) runs=20 ;;
  singlecpu) runs=10 ;;
  *) echo "unknown arm $arm" >&2; exit 64 ;;
esac
threads=8
load_procs=0
[[ "$arm" == "load" ]] && load_procs=4
# Fail closed: the single-CPU arm is only meaningful with the court quota at 1.
if [[ "$arm" == "singlecpu" && "${WEFT_COURT_CPUS:-}" != "1" ]]; then
  echo "multi_writer_gc_stress: singlecpu arm needs WEFT_CPUS=1, got '${WEFT_COURT_CPUS:-unset}'" >&2
  exit 64
fi

echo "arm=$arm stamp=$stamp cpus=${WEFT_COURT_CPUS:-?} runs=$runs threads=$threads load=$load_procs"
echo "tmpdir=$TMPDIR (bind-mounted project filesystem, not the court tmpfs)"
nproc_all="$(nproc)"
cpu_max="$(cat /sys/fs/cgroup/cpu.max 2>/dev/null || echo unknown)"
echo "nproc=$nproc_all cgroup_cpu_max=$cpu_max"

cargo test --test multi_writer_gc --no-run --message-format=json 2>/dev/null \
  | python3 -c 'import json,sys
for line in sys.stdin:
    try:
        doc = json.loads(line)
    except ValueError:
        continue
    if doc.get("reason") == "compiler-artifact" and doc.get("executable") \
       and doc.get("target", {}).get("name") == "multi_writer_gc":
        print(doc["executable"])
        break' >"$tmpdir/bin.path"
binary="$(cat "$tmpdir/bin.path")"
[[ -n "$binary" && -x "$binary" ]] || { echo "cannot locate the test binary" >&2; exit 1; }
binary_sha="$(sha256sum "$binary" | cut -d' ' -f1)"
echo "binary=$binary sha256=$binary_sha"

load_pids=()
stop_load() {
  local pid
  for pid in ${load_pids[@]+"${load_pids[@]}"}; do
    kill "$pid" 2>/dev/null || true
  done
  load_pids=()
}
trap stop_load EXIT
if [[ "$load_procs" -gt 0 ]]; then
  for _ in $(seq 1 "$load_procs"); do
    sh -c 'while :; do :; done' &
    load_pids+=("$!")
  done
  echo "load: ${load_procs} busy loops running"
fi

failures=0
timeouts=0
started_ns="$(date +%s%N)"
for run in $(seq 1 "$runs"); do
  run_started_ns="$(date +%s%N)"
  status="pass"
  rc=0
  out="$(timeout 180 "$binary" --test-threads="$threads" 2>&1)" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    if [[ "$rc" -eq 124 ]]; then
      timeouts=$((timeouts + 1))
      status="timeout"
    else
      failures=$((failures + 1))
      status="fail"
    fi
    printf '=== %s run %d %s (exit %d) ===\n%s\n' "$arm" "$run" "$status" "$rc" "$out"
  fi
  run_ms=$(( ( $(date +%s%N) - run_started_ns ) / 1000000 ))
  printf 'RESULT arm=%s run=%d status=%s elapsed_ms=%d\n' "$arm" "$run" "$status" "$run_ms"
done
stop_load
trap - EXIT
elapsed_ms=$(( ( $(date +%s%N) - started_ns ) / 1000000 ))

python3 - "$fragment" "$arm" "$runs" "$threads" "$load_procs" "$failures" \
  "$timeouts" "$elapsed_ms" "$binary_sha" "$nproc_all" "$cpu_max" "$cpus" \
  "${WEFT_COURT_CPUS:-unknown}" "$TMPDIR" "$root" <<'PY'
import json, os, sys
(fragment, arm, runs, threads, load, failures, timeouts, elapsed, sha,
 nproc_all, cpu_max, cpus, court_cpus, tmpdir, root) = sys.argv[1:]
prefix = root.rstrip("/") + "/"
doc = {
    "arm": arm,
    "runs": int(runs),
    "test_threads": int(threads),
    "load_processes": int(load),
    "failures": int(failures),
    "timeouts": int(timeouts),
    "elapsed_ms": int(elapsed),
    "court_cpus_override": cpus,
    "court_cpus_reported": court_cpus,
    "host_nproc": int(nproc_all),
    "cgroup_cpu_max": cpu_max,
    "tmpdir": tmpdir[len(prefix):] if tmpdir.startswith(prefix) else tmpdir,
    "binary_sha256": sha,
    "court": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
    "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
}
with open(fragment, "w") as handle:
    json.dump(doc, handle, indent=2, sort_keys=True)
print(json.dumps(doc, sort_keys=True))
PY
[[ "$failures" -eq 0 && "$timeouts" -eq 0 ]]
