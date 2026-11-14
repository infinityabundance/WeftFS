#!/usr/bin/env bash
# Independent context-plane verifier court.
#
# Runs `verifier/verify_corpus.py` (Python standard library only, no WeftFS
# import) against the frozen conformance corpus and writes a receipt. This is
# a host-runnable analysis gate, like `perf_numbers`: it compiles nothing and
# touches no implementation code path.
#
# Exit 0 on pass, 1 on any case failure, 78 with exactly one
# `context_plane_verifier: skip <reason>` line when Python is unavailable.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v python3 >/dev/null 2>&1; then
  echo "context_plane_verifier: skip no python3 interpreter"
  exit 78
fi

stamp="${WEFT_VERIFIER_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
receipt="bench/receipts/context-plane-verifier-${stamp}.json"
log="bench/receipts/context-plane-verifier-${stamp}.log"

python3 verifier/verify_corpus.py --json "$receipt" --log "$log" --quiet
python3 - "$receipt" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    receipt = json.load(handle)
summary = receipt["summary"]
print(
    "context_plane_verifier: %d cases, %d pass, %d fail, %d skip; "
    "negative %d/%d; provenance self-test %d/%d; manifest %s"
    % (
        summary["cases"],
        summary["passed"],
        summary["failed"],
        summary["skipped"],
        summary["negative_passed"],
        summary["negative_cases"],
        summary["provenance_self_test_passed"],
        summary["provenance_self_test_cases"],
        receipt["manifest"]["aggregate_sha256"],
    )
)
if summary["result"] != "pass":
    raise SystemExit(1)
PY
echo "context_plane_verifier: receipt $receipt"
