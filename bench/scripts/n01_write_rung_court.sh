#!/usr/bin/env bash
# N01 court: the lane change and the published receipt's own assertions.
#
# Runs inside the bench court (it needs cargo and python3):
#   scripts/docker-court.sh bench "bash bench/scripts/n01_write_rung_court.sh"
#
# It re-runs the determinism test that one versus eight lanes publishes the same
# committed state, then asserts the published N01 receipt still says what this
# document claims: seven repetitions, every court check true, the footprint, the
# least-contended and median walls, and the rung verdict.
set -Eeuo pipefail

root=/work
cd "$root"
stamp="${WEFT_N01_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/n01-write-rung-20261114T195000Z.json"
out="bench/receipts/n01-write-rung-court-$stamp.json"

if [[ ! -f "$receipt" ]]; then
  echo "n01_write_rung: skip missing $receipt" >&2
  exit 78
fi

echo "== N01 lane determinism =="
cargo test --release --test tensor_pipeline -- --test-threads=1 writeback_lanes_do_not_change_a_single_byte

echo "== N01 receipt assertions =="
python3 - "$receipt" "$out" "$stamp" <<'PY'
import json
import sys

receipt_path, out_path, stamp = sys.argv[1:]
receipt = json.load(open(receipt_path))
checks = receipt["court_checks"]
assert receipt["court_result"] == "pass", "the cited court must have passed"
assert all(checks.values()), checks
assert checks["at_least_seven_reps"] and checks["all_restores_exact"]
assert checks["default_c_matches_official_court"] and checks["two_stage_matches_exhaustive"]
rows = receipt["rows"]
shipped = rows["C-pipe4"]
pre = rows["C-pipe4-wb1"]
assert shipped["footprint_bytes"] == 282106049
assert pre["footprint_bytes"] == 282106049
assert shipped["single_run"] is False and len(shipped["wall_ms"]["samples"]) == 7
assert shipped["wall_ms"]["min"] <= shipped["wall_ms"]["median"]
assert receipt["verdict"]["rung_ms"] == 336.0
assert receipt["verdict"]["met"] is False, "a met rung would need a new document"
assert receipt["verdict"]["achieved_min_ms"] == shipped["wall_ms"]["min"]
assert receipt["identity"]["fields_equal"]["output_contents"] is True
assert receipt["identity"]["fields_equal"]["physical_footprint_bytes"] is True
record = {
    "schema": "weft.n01.court.v1",
    "stamp": stamp,
    "receipt": receipt_path,
    "assertions": "7 reps, all checks true, footprint 282,106,049 B, rung not met, identity true",
    "min_ms": shipped["wall_ms"]["min"],
    "median_ms": shipped["wall_ms"]["median"],
    "pre_min_ms": pre["wall_ms"]["min"],
    "result": "pass",
}
with open(out_path, "w") as handle:
    json.dump(record, handle, indent=1, sort_keys=True)
    handle.write("\n")
print("n01_write_rung: pass (%s)" % out_path)
PY
