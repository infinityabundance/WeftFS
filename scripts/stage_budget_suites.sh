#!/usr/bin/env bash
# Court: full Rust suites at the stage-budget fix revision.
#
# Records the two required full-suite runs as one receipt so the counts quoted
# beside the X31 fix are generated from a committed log rather than typed:
#
#   cargo test --all-targets                        (rust court)
#   cargo test --features fuse --all-targets        (fuse court: needs /dev/fuse)
#
# Usage: scripts/stage_budget_suites.sh
#
# Writes bench/receipts/stage-budget-suites-<stamp>.json and the two .log files.
# A non-zero court exit or a non-zero failed count makes `result` "fail"; the
# receipt is still written so the failure is recorded rather than lost.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "stage_budget_suites: skip docker daemon is not reachable" >&2
  exit 78
fi
for image in weft/rust-court:1 weft/fuse-court:1; do
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "stage_budget_suites: skip court image $image is not present" >&2
    exit 78
  fi
done

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
revision="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
go_revision="$(git -C WeftFS-Go rev-parse HEAD 2>/dev/null || echo unknown)"
host_kernel="$(uname -r)"
docker_server="$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo unknown)"

receipts="bench/receipts"
rust_log="$receipts/stage-budget-suites-$stamp.rust.log"
fuse_log="$receipts/stage-budget-suites-$stamp.fuse.log"

echo "stage_budget_suites: stamp $stamp revision $revision"
echo "stage_budget_suites: rust court --all-targets"
rust_rc=0
timeout 3000 scripts/docker-court.sh rust "cd /work && timeout 2400 cargo test --all-targets" \
  >"$rust_log" 2>&1 || rust_rc=$?

echo "stage_budget_suites: fuse court --features fuse --all-targets"
fuse_rc=0
timeout 3000 scripts/docker-court.sh fuse "cd /work && timeout 2400 cargo test --features fuse --all-targets" \
  >"$fuse_log" 2>&1 || fuse_rc=$?

python3 - "$stamp" "$revision" "$go_revision" "$host_kernel" "$docker_server" \
  "$rust_log" "$fuse_log" "$rust_rc" "$fuse_rc" "$receipts" <<'PY'
import json
import os
import sys

stamp, revision, go_revision, kernel, docker_server, rust_log, fuse_log, rust_rc, fuse_rc, receipts = sys.argv[1:]


def summarise(path):
    suites = 0
    passed = 0
    failed = 0
    ignored = 0
    with open(path, errors="replace") as handle:
        for line in handle:
            if not line.startswith("test result:"):
                continue
            suites += 1
            fields = line.split()
            passed += int(fields[3])
            failed += int(fields[5])
            ignored += int(fields[7])
    return {"suites": suites, "passed": passed, "failed": failed, "ignored": ignored}


rust = summarise(rust_log)
rust["exit_code"] = int(rust_rc)
rust["log"] = rust_log
fuse = summarise(fuse_log)
fuse["exit_code"] = int(fuse_rc)
fuse["log"] = fuse_log

result = (
    "pass"
    if int(rust_rc) == 0 and int(fuse_rc) == 0 and rust["failed"] == 0 and fuse["failed"] == 0
    else "fail"
)

receipt = {
    "schema": "weft.stage_budget_suites.receipt.v1",
    "stamp": stamp,
    "revision": revision,
    "result": result,
    "rust": rust,
    "fuse": fuse,
    "ignored_note": (
        "The two FUSE #[ignore]s are pre-existing and environment-blocked: "
        "permissions_with_allow_other needs CAP_SETUID, which the court drops; "
        "concurrent_readers_and_one_writer_see_whole_versions hits a documented "
        "kernel 7.3 + fuser 0.15.1 direct-I/O EIO. Neither is filtered by this run."
    ),
    "court": {
        "kinds": ["rust", "fuse"],
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
    "command": "scripts/stage_budget_suites.sh",
}
out = os.path.join(receipts, f"stage-budget-suites-{stamp}.json")
with open(out, "w") as handle:
    json.dump(receipt, handle, indent=2, sort_keys=True)
    handle.write("\n")
print(out)
print(result)
PY

echo "$stamp"
