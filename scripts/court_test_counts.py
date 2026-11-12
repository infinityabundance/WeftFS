#!/usr/bin/env python3
"""Extract per-court test counts from a `scripts/verify-all.sh` log.

The aggregate receipt records pass/fail per court but not how many tests ran.
This script reads the single log the aggregate writes, splits it on the
`=== <court> ===` markers, and sums the `test result:` lines and `Running`
binaries inside each section. It is a pure post-processing step: it does not run
anything and cannot change a result.

Usage:
    scripts/court_test_counts.py bench/receipts/verify-<stamp>.log \
        --out bench/receipts/verify-<stamp>.tests.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys

SECTION_RE = re.compile(r"^=== ([A-Za-z0-9_]+) ===$")
RESULT_RE = re.compile(
    r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;"
    r" (\d+) measured; (\d+) filtered out"
)
RUNNING_RE = re.compile(r"^\s*Running (?:unittests )?(\S+)")
GO_OK_RE = re.compile(r"^ok\s+\S+\s+([0-9.]+)s")
GO_FAIL_RE = re.compile(r"^(FAIL|--- FAIL:)")


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("log")
    parser.add_argument("--out", required=True)
    args = parser.parse_args(argv)

    with open(args.log, "rb") as handle:
        raw = handle.read()
    digest = hashlib.sha256(raw).hexdigest()
    text = raw.decode("utf-8", "replace")

    courts = {}
    current = None
    for line in text.splitlines():
        match = SECTION_RE.match(line.strip())
        if match:
            current = match.group(1)
            courts.setdefault(
                current,
                {
                    "passed": 0,
                    "failed": 0,
                    "ignored": 0,
                    "test_result_lines": 0,
                    "test_binaries": 0,
                    "go_packages_ok": 0,
                },
            )
            continue
        if current is None:
            continue
        entry = courts[current]
        result = RESULT_RE.search(line)
        if result:
            entry["test_result_lines"] += 1
            entry["passed"] += int(result.group(2))
            entry["failed"] += int(result.group(3))
            entry["ignored"] += int(result.group(4))
            continue
        if RUNNING_RE.match(line):
            entry["test_binaries"] += 1
            continue
        if GO_OK_RE.match(line):
            entry["go_packages_ok"] += 1

    # drop sections that reported neither a Rust result line nor a Go package,
    # so the sidecar only names courts that actually count tests
    kept = {
        name: entry
        for name, entry in sorted(courts.items())
        if entry["test_result_lines"] or entry["go_packages_ok"]
    }

    receipt = {
        "schema": "weft.court_test_counts.v1",
        "log": args.log,
        "log_sha256": digest,
        "parser": {
            "rust": "sum of `test result: <status>. N passed; M failed; K ignored` lines per section",
            "binaries": "count of `Running ...` lines per section",
            "go": "count of `ok <package> <seconds>` lines per section",
            "note": "counts are derived from the log; the log is the evidence and its sha256 is recorded",
        },
        "courts": kept,
    }
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print(
        "court_test_counts: %d court(s) with counts from %s -> %s"
        % (len(kept), args.log, args.out)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
