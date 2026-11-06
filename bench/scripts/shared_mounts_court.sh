#!/usr/bin/env bash
# Shared multi-mount daemon court (R7).
#
#   bash bench/scripts/shared_mounts_court.sh [N]
#
# Host orchestrator: every mount, build, and verification runs inside the
# OOM-protected FUSE court. The host only starts containers, copies their
# demo receipts, and assembles one court receipt.
#
# The court runs the same logical corpus (built by
# `examples/context_mount_corpus.rs`, the construction oracle) four times:
#
#   per-daemon-3g     N processes, one `weftfs mount` each, at 3 GiB
#   shared-3g         one `weftfs mount-many` process, N views, at 3 GiB
#   shared-small      the same shared run at the small limit (default 512 MiB)
#   per-daemon-small  the same per-daemon run at the small limit
#
# It then requires:
#   * every listing digest (per context, not just the set) identical between
#     shared and per-daemon at 3 GiB, and identical again at the small limit;
#   * the same number of mounts verified and the same number of file SHA-256
#     checks in every successful case;
#   * the shared run to pass at the small limit with zero cgroup OOM kills,
#     bounded `pids.events max`, and peak memory below that limit;
#   * the per-daemon run at the same small limit to fail (it did in the
#     measurement that chose the limit; if it ever passes the check fails and
#     the limit must be re-chosen).
#
# N defaults to 100 (a multiple of 4, the demo's Context Root shape) and may be
# overridden with the first argument or WEFT_SHARED_N. The small limit may be
# overridden with WEFT_SHARED_LIMIT (validated by scripts/docker-court.sh).
#
# Outputs:
#   bench/receipts/shared-mounts-<UTC>.json
#   bench/receipts/shared-mounts-<UTC>.log
#   bench/receipts/shared-mounts-<case>.json        (raw demo receipts)
#   bench/receipts/shared-mounts-<case>.log
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

n="${1:-${WEFT_SHARED_N:-100}}"
if [[ ! "$n" =~ ^[0-9]+$ ]] || [[ "$n" -lt 4 ]] || [[ $((n % 4)) -ne 0 ]]; then
  echo "shared-mounts-court: N must be a multiple of 4 (Context Root shape), got '$n'" >&2
  exit 64
fi
small="${WEFT_SHARED_LIMIT:-512m}"
if [[ ! "$small" =~ ^[0-9]+(\.[0-9]+)?[mg]$ ]]; then
  echo "shared-mounts-court: WEFT_SHARED_LIMIT='$small' is not a Docker memory size" >&2
  exit 64
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
work="$root/.docker-cache/shared-mounts"
mkdir -p "$work" "$root/bench/receipts"
log="$root/bench/receipts/shared-mounts-${stamp}.log"
receipt="$root/bench/receipts/shared-mounts-${stamp}.json"
: > "$log"
rm -f "$work"/*.json "$work/status.tsv" 2>/dev/null || true

echo "shared-mounts-court: N=$n small_limit=$small" | tee -a "$log"

# run_case <label> <memory> <mode> <skip-build> <extra env>
run_case() {
  local label="$1" memory="$2" mode="$3" skip="$4" extra="${5:-}"
  local demo_base="shared-mounts-${label}-${stamp}"
  local demo_receipt="$root/bench/receipts/${demo_base}.json"
  local demo_log="$root/bench/receipts/${demo_base}.log"
  rm -f "$demo_receipt" "$demo_log"
  local cmd="cd /work && WEFT_CONTEXT_MOUNTS=$n WEFT_CONTEXT_CORPUS_BUILD=harness"
  cmd="$cmd WEFT_CONTEXT_MOUNTS_MODE=$mode WEFT_DEMO_RECEIPT_NAME=$demo_base"
  if [[ "$skip" = 1 ]]; then cmd="$cmd WEFT_DEMO_SKIP_BUILD=1"; fi
  if [[ -n "$extra" ]]; then cmd="$cmd $extra"; fi
  cmd="$cmd bash scripts/demo-context-mounts.sh"

  echo "=== case $label (memory=$memory mode=$mode skip_build=$skip) ===" | tee -a "$log"
  echo "court command: WEFT_MEMORY=$memory scripts/docker-court.sh fuse \"$cmd\"" | tee -a "$log"
  local status=0
  if WEFT_MEMORY="$memory" scripts/docker-court.sh fuse "$cmd" >>"$log" 2>&1; then
    status=0
  else
    status=$?
  fi
  printf '%s\t%s\t%s\t%s\n' "$label" "$status" "$memory" "$mode" >> "$work/status.tsv"
  if [[ -f "$demo_receipt" ]]; then
    cp "$demo_receipt" "$work/$label.json"
    echo "case $label: exit=$status receipt=$demo_base.json" | tee -a "$log"
  else
    echo "case $label: exit=$status NO RECEIPT (container failure); see $log" | tee -a "$log"
  fi
}

# The first case builds the binaries in a roomy container. Every later case
# reuses them, so the small-limit runs measure mounts and verification, not
# the compiler.
# First build once in a roomy container. Every case then reuses those binaries,
# so the small-limit runs measure mounts and verification, not the compiler.
echo "=== build phase ===" | tee -a "$log"
build_status=0
if ! WEFT_MEMORY=3g scripts/docker-court.sh fuse \
    "cd /work && WEFT_DEMO_BUILD_ONLY=1 bash scripts/demo-context-mounts.sh" >>"$log" 2>&1; then
  build_status=$?
fi
build_cmd="cd /work && WEFT_DEMO_BUILD_ONLY=1 bash scripts/demo-context-mounts.sh"
printf '{"exit": %s, "court": "fuse", "memory": "3g", "command": "%s"}\n' \
  "$build_status" "$build_cmd" > "$work/build.json"
echo "build phase exit=$build_status" | tee -a "$log"

run_case per-daemon-3g 3g per-daemon 1
run_case shared-3g 3g shared 1
run_case shared-small "$small" shared 1
# 250 retries x 20 ms bounds the per-mount wait at 5 s instead of 30 s: with 51
# dead daemons the default 30 s waits would make this case 25 minutes long.
run_case per-daemon-small "$small" per-daemon 1 "WEFT_WAIT_MOUNT_TRIES=250"

echo "=== assembling receipt ===" | tee -a "$log"
python3 - "$work" "$receipt" "$n" "$small" "$stamp" "$log" "$root" <<'PY'
import json
import os
import subprocess
import sys
from datetime import datetime, timezone

work, receipt_path, n_text, small, stamp, log_path, root = sys.argv[1:]
n = int(n_text)

def mem_bytes(text):
    unit = text[-1].lower()
    value = float(text[:-1])
    scale = {"m": 1024 * 1024, "g": 1024 * 1024 * 1024}[unit]
    return int(value * scale)

small_bytes = mem_bytes(small)
status = {}
for line in open(os.path.join(work, "status.tsv"), encoding="utf-8"):
    label, code, memory, mode = line.rstrip("\n").split("\t")
    status[label] = {"exit": int(code), "memory": memory, "mode": mode}

cases = {}
for label in ("per-daemon-3g", "shared-3g", "shared-small", "per-daemon-small"):
    path = os.path.join(work, label + ".json")
    if not os.path.isfile(path):
        cases[label] = {"receipt": None, "status": status.get(label)}
        continue
    doc = json.load(open(path, encoding="utf-8"))
    m = doc.get("metrics", {})
    env = doc.get("environment", {})
    cases[label] = {
        "receipt": os.path.relpath(path, root),
        "status": status.get(label),
        "limits": {
            "memory": env.get("limit_memory"),
            "memory_swap": env.get("limit_memory_swap"),
            "cpus": env.get("limit_cpus"),
            "pids": env.get("limit_pids"),
            "nproc": env.get("limit_nproc"),
            "skip_build": env.get("skip_build"),
        },
        "all_pass": doc.get("summary", {}).get("all_pass"),
        "steps": {
            "pass": doc.get("summary", {}).get("pass"),
            "fail": doc.get("summary", {}).get("fail"),
        },
        "mounts_verified": m.get("mounts_verified"),
        "mount_failures": m.get("mount_failures"),
        "mount_processes": m.get("mount_processes"),
        "files_sha256_checked": m.get("files_checked"),
        "unique_listing_digests": m.get("unique_listing_digests"),
        "listing_digests_sha256": m.get("listing_digests_sha256"),
        "mount_latency_ms": {
            "min": m.get("mount_latency_min_ms"),
            "p50": m.get("mount_latency_p50_ms"),
            "p95": m.get("mount_latency_p95_ms"),
            "max": m.get("mount_latency_max_ms"),
            "all_mounted": m.get("all_mounted_ms"),
        },
        "process_rss_kb": {
            "sum": m.get("daemon_rss_kb"),
            "max": m.get("daemon_rss_max_kb"),
            "sampled": m.get("daemons_sampled"),
            "missing_at_sample": m.get("daemons_missing_at_sample"),
        },
        "threads": {"sum": m.get("daemon_threads"), "max": m.get("daemon_threads_max")},
        "cgroup": {
            "peak_bytes": m.get("cgroup_peak_bytes"),
            "pids_max": m.get("cgroup_pids_max"),
            "pids_events_max": m.get("cgroup_pids_max_events"),
            "oom_kills": m.get("cgroup_oom_kills"),
        },
        "clean_unmounts": m.get("clean_unmounts"),
        "daemons_surviving": m.get("daemons_surviving"),
    }

def num(value):
    """The demo receipt generator writes every metric as text."""
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        try:
            return float(value)
        except (TypeError, ValueError):
            return value

NUMERIC_CASE_KEYS = {
    "mounts_verified", "mount_failures", "mount_processes",
    "files_sha256_checked", "unique_listing_digests",
    "clean_unmounts", "daemons_surviving",
}
for case in cases.values():
    for key in NUMERIC_CASE_KEYS:
        if key in case:
            case[key] = num(case[key])
    for group, keys in (
        ("mount_latency_ms", ("min", "p50", "p95", "max", "all_mounted")),
        ("process_rss_kb", ("sum", "max", "sampled", "missing_at_sample")),
        ("threads", ("sum", "max")),
        ("cgroup", ("peak_bytes", "pids_max", "pids_events_max", "oom_kills")),
    ):
        holder = case.get(group)
        if isinstance(holder, dict):
            for key in keys:
                if key in holder:
                    holder[key] = num(holder[key])

def metric(label, key):
    return cases.get(label, {}).get(key)

def peak(label):
    value = (cases.get(label, {}).get("cgroup") or {}).get("peak_bytes")
    try:
        return int(value)
    except (TypeError, ValueError):
        return None

checks = {}

build = None
build_path = os.path.join(work, "build.json")
if os.path.isfile(build_path):
    build = json.load(open(build_path, encoding="utf-8"))

def check(name, condition, detail):
    checks[name] = {"pass": bool(condition), "detail": detail}

ok_cases = ("per-daemon-3g", "shared-3g", "shared-small")
check(
    "build phase succeeded",
    build is not None and build.get("exit") == 0,
    f"build={build}",
)
for label in ok_cases:
    case = cases.get(label, {})
    check(
        f"{label}: demo all_pass",
        case.get("all_pass") is True,
        f"all_pass={case.get('all_pass')} steps={case.get('steps')}",
    )
    check(
        f"{label}: every mount verified",
        case.get("mounts_verified") == n,
        f"mounts_verified={case.get('mounts_verified')} target={n}",
    )
    check(
        f"{label}: every context listing distinct",
        case.get("unique_listing_digests") == n,
        f"unique_listing_digests={case.get('unique_listing_digests')}",
    )

digests = {label: metric(label, "listing_digests_sha256") for label in cases}
check(
    "shared and per-daemon listing digests are identical",
    len({digests.get(l) for l in ok_cases}) == 1 and digests.get("per-daemon-3g") is not None,
    f"digests={digests}",
)
files = {label: metric(label, "files_sha256_checked") for label in ok_cases}
check(
    "shared and per-daemon verify the same file SHA-256 set",
    len(set(files.values())) == 1 and files.get("per-daemon-3g") is not None,
    f"files_sha256_checked={files}",
)
mounts = {label: metric(label, "mounts_verified") for label in ok_cases}
check(
    "shared and per-daemon verify the same number of mounts",
    len(set(mounts.values())) == 1 and mounts.get("per-daemon-3g") == n,
    f"mounts_verified={mounts}",
)

shared_small = cases.get("shared-small", {})
check(
    "shared run completed at the small memory limit",
    shared_small.get("all_pass") is True,
    f"limit={small} all_pass={shared_small.get('all_pass')}",
)
shared_small_peak = peak("shared-small")
check(
    "shared small-limit peak is below the limit",
    shared_small_peak is not None and shared_small_peak <= small_bytes,
    f"peak={shared_small_peak} limit={small_bytes} headroom={None if shared_small_peak is None else small_bytes - shared_small_peak}",
)
check(
    "shared small-limit run has zero cgroup OOM kills",
    str((shared_small.get("cgroup") or {}).get("oom_kills")) in ("0",),
    f"oom_kills={(shared_small.get('cgroup') or {}).get('oom_kills')}",
)
check(
    "shared small-limit run never hit the cgroup pids limit",
    str((shared_small.get("cgroup") or {}).get("pids_events_max")) in ("0",),
    f"pids.events max={(shared_small.get('cgroup') or {}).get('pids_events_max')}",
)

perdaemon_small = cases.get("per-daemon-small", {})
pd_small_mounts = perdaemon_small.get("mounts_verified")
pd_small_oom = str((perdaemon_small.get("cgroup") or {}).get("oom_kills"))
check(
    "per-daemon run at the same small limit did NOT complete the full N",
    perdaemon_small.get("all_pass") is not True and (pd_small_mounts is None or pd_small_mounts < n),
    f"limit={small} all_pass={perdaemon_small.get('all_pass')} mounts_verified={pd_small_mounts} oom_kills={pd_small_oom}",
)

shared_peak = peak("shared-3g")
perdaemon_peak = peak("per-daemon-3g")
check(
    "shared peak is materially smaller than per-daemon peak",
    shared_peak is not None and perdaemon_peak is not None and shared_peak * 2 <= perdaemon_peak,
    f"shared_peak={shared_peak} per-daemon_peak={perdaemon_peak}",
)

gaps = [
    "Read-only by construction: a shared view refuses create/write/truncate/xattr/symlink/"
    "rename/link with EROFS. Writes remain on the single-mount path.",
    "One backend mutex per store serialises physical I/O across views; independent "
    "daemons overlap that I/O. Under concurrent large reads the shared server can be slower.",
    "A shared server pins views at open: there is no live re-read of commits made by "
    "another process while it runs (the same as any read-only mount).",
    "One process is a single failure domain: a panic in one view would end every view. "
    "The view callbacks are covered by tests and the shared read path uses no panics, "
    "but this is a real difference from independent daemons.",
    "Per-mount latency in shared mode includes sequential resolution and mounting of all "
    "views before the last one appears; compare `all_mounted_ms`, not only p50.",
    "No fleet or multi-host claim; one host, synthetic deterministic corpus, warm page cache.",
]

revisions = {}
for name, path in (("WeftFS", "."), ("WeftCodec", "WeftCodec"),
                   ("WeftCodec-Go", "WeftCodec-Go"), ("WeftFS-Go", "WeftFS-Go")):
    try:
        revisions[name] = subprocess.check_output(
            ["git", "-C", os.path.join(root, path), "rev-parse", "HEAD"],
            text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        revisions[name] = "unknown"

build = None
build_path = os.path.join(work, "build.json")
if os.path.isfile(build_path):
    build = json.load(open(build_path, encoding="utf-8"))

doc = {
    "schema": "weftfs.shared-mounts.court/1",
    "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "stamp": stamp,
    "label": (
        "Single-host Docker FUSE court; synthetic deterministic corpus built by "
        "examples/context_mount_corpus.rs; local store on the court bind mount; "
        "hard memory/swap/pids/cpu/nproc limits unchanged except the explicitly "
        "recorded small limit; not a fleet claim."
    ),
    "mounts": n,
    "small_limit": small,
    "small_limit_bytes": small_bytes,
    "build": build,
    "cases": cases,
    "checks": checks,
    "losses_and_limits": gaps,
    "revisions": revisions,
    "raw_log": os.path.relpath(log_path, root),
}
passed = sum(1 for item in checks.values() if item["pass"])
failed = len(checks) - passed
doc["summary"] = {"pass": passed, "fail": failed, "all_pass": failed == 0}
with open(receipt_path, "w", encoding="utf-8") as handle:
    json.dump(doc, handle, indent=2, sort_keys=True)
    handle.write("\n")
print(f"checks: {passed} pass, {failed} fail")
for name, item in checks.items():
    print(("PASS " if item["pass"] else "FAIL ") + name + ": " + item["detail"])
sys.exit(0 if failed == 0 else 1)
PY

echo "receipt: $receipt" | tee -a "$log"
echo "raw log: $log" | tee -a "$log"
