#!/usr/bin/env bash
# Negative test for the untracked-receipt rule in `scripts/perf_numbers.py`.
#
# CI run 43408465922 failed with "missing receipt
# bench/receipts/checkpoint-queue-bound-after-20261112T043000Z.json": the file
# existed in the author's tree and had never been committed. `--check` only
# tested existence, so the defect was invisible until a fresh checkout.
#
# This script proves the rule now catches that class *without* needing a fresh
# checkout: it copies a real receipt to an uncommitted path, points a synthetic
# manifest at it, and requires `--check` to fail with the "not tracked by git"
# message. It removes the copy on exit.
#
# Host-only: no Docker, no network, no build. Safe to run repeatedly.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
source_receipt="bench/receipts/checkpoint-queue-bound-after-20261112T043000Z.json"
untracked="bench/receipts/.untracked-negative-$stamp.json"
manifest=".docker-cache/perf-numbers-untracked-$stamp.json"

cleanup() { rm -f "$untracked" "$manifest"; }
trap cleanup EXIT

if [[ ! -f "$source_receipt" ]]; then
  echo "perf_numbers untracked demo: SKIP source receipt is absent: $source_receipt" >&2
  exit 78
fi
mkdir -p .docker-cache
cp "$source_receipt" "$untracked"

python3 - "$untracked" "$manifest" <<'PY'
import json
import sys

receipt, out = sys.argv[1], sys.argv[2]
manifest = {
    "schema": "weft.perf_numbers.v1",
    "description": "negative test: one number sourced from an untracked receipt",
    "groups": [{"id": "demo"}],
    "numbers": [
        {
            "key": "demo.value",
            "label": "untracked receipt must fail closed",
            "unit": "ms",
            "group": "demo",
            "format": "fixed:1",
            "receipt": receipt,
            "list": "bound_sweep.rows",
            "where": {"interval_steps": 1, "max_outstanding": 2},
            "stamp_path": "stamp",
            "stamp": "20261112T043000Z",
            "revision_path": "court.revision_weftfs",
            "revision": "1d716632d02440e9d0b443dff5207a69d83cfe4f",
            "select_path": "loop_wall_ms_median",
        }
    ],
    "blocks": [],
}
with open(out, "w", encoding="utf-8") as handle:
    json.dump(manifest, handle, indent=2)
    handle.write("\n")
PY

set +e
output="$(python3 scripts/perf_numbers.py --check --manifest "$manifest" 2>&1)"
status=$?
set -e

echo "$output"
if [[ "$status" -eq 0 ]]; then
  echo "perf_numbers untracked demo: FAIL (an untracked receipt was accepted)" >&2
  exit 1
fi
if ! grep -Fq "is not tracked by git" <<<"$output"; then
  echo "perf_numbers untracked demo: FAIL (failed, but not for the tracked-file rule)" >&2
  exit 1
fi
echo "perf_numbers untracked demo: PASS (exit $status, untracked receipt refused)"
