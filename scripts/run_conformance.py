#!/usr/bin/env python3
"""Run every manifest vector against a third-party implementation adapter.

The adapter contract is documented in `docs/CONFORMANCE.md`:

    <adapter> VECTOR.json

prints exactly one JSON object:

    {"result":"ok","content_root":"<hex|null>","decoded_sha256":"<hex|null>"}
    {"result":"error","class":"<class>"}
    {"result":"skip","reason":"..."}

Exit status is non-zero on any mismatch or adapter failure. Skips are reported
and counted but are not passing evidence.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_MANIFEST = os.path.join(ROOT, "conformance", "manifest.json")
FIXTURE_KINDS = ("store", "fuzz")


def load_manifest(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def invoke(command, vector_path):
    timeout = float(os.environ.get("WEFT_CONFORMANCE_TIMEOUT", "60"))
    try:
        completed = subprocess.run(
            command + [vector_path],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except FileNotFoundError:
        return None, "adapter command not found: %s" % command[0]
    except subprocess.TimeoutExpired:
        return None, "adapter timed out after %.0fs" % timeout
    stdout = completed.stdout.decode("utf-8", "replace").strip()
    stderr = completed.stderr.decode("utf-8", "replace").strip()
    if completed.returncode != 0:
        return None, "adapter exited %d: %s" % (completed.returncode, stderr or stdout)
    candidate = None
    for line in reversed(stdout.splitlines()):
        line = line.strip()
        if line.startswith("{"):
            candidate = line
            break
    if candidate is None:
        return None, "adapter printed no JSON object: %r" % stdout
    try:
        record = json.loads(candidate)
    except json.JSONDecodeError as exc:
        return None, "adapter printed invalid JSON: %s" % exc
    if not isinstance(record, dict) or record.get("result") not in ("ok", "error", "skip"):
        return None, "adapter result must be ok|error|skip: %r" % record
    return record, None


def compare(entry, actual):
    expected = entry["expect"]
    if actual["result"] == "skip":
        return None  # handled by the caller
    if expected.get("ok"):
        if actual["result"] != "ok":
            return "expected ok, got %s" % actual["result"]
        for field in ("content_root", "decoded_sha256"):
            want = expected.get(field)
            if want is None:
                continue
            got = actual.get(field)
            if not isinstance(got, str) or got.lower() != want.lower():
                return "expected %s=%s, got %s" % (field, want, got)
        return None
    want = expected.get("error")
    if actual["result"] != "error":
        return "expected error %s, got ok" % want
    got = actual.get("class")
    if got != want:
        return "expected class=%s, got %s" % (want, got)
    return None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("implementation", help="adapter command, e.g. scripts/conformance-adapter-rust.sh")
    parser.add_argument("--manifest", default=DEFAULT_MANIFEST)
    parser.add_argument("--json", default=None, help="write a machine-readable run receipt")
    parser.add_argument("--kind", action="append", default=None, help="only these kinds")
    parser.add_argument("--include-fixtures", action="store_true", help="include store/fuzz fixtures")
    parser.add_argument("--fail-on-skip", action="store_true", help="treat skips as failures")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)

    command = shlex.split(args.implementation)
    if not command:
        parser.error("implementation command is empty")
    manifest = load_manifest(args.manifest)
    entries = manifest["vectors"]
    if args.kind:
        wanted = set(args.kind)
        entries = [entry for entry in entries if entry["kind"] in wanted]

    passes = failures = skips = 0
    mismatches = []
    skipped = []
    for entry in entries:
        path = entry["path"]
        full = os.path.join(ROOT, path)
        if entry.get("superseded_by"):
            skips += 1
            skipped.append({"path": path, "reason": "superseded by %s" % entry["superseded_by"]})
            if not args.quiet:
                print("SKIP %s: superseded by %s" % (path, entry["superseded_by"]))
            continue
        if entry["kind"] in FIXTURE_KINDS and not args.include_fixtures:
            skips += 1
            skipped.append({"path": path, "reason": "composite fixture (use --include-fixtures)"})
            if not args.quiet:
                print("SKIP %s: composite fixture (use --include-fixtures)" % path)
            continue
        if not os.path.exists(full):
            reason = "vector file missing"
            skips += 1
            skipped.append({"path": path, "reason": reason})
            if not args.quiet:
                print("SKIP %s: %s" % (path, reason))
            continue

        actual, error = invoke(command, full)
        if error is not None:
            failures += 1
            message = "VECTOR MISMATCH %s: expected %s got adapter failure: %s" % (
                path,
                json.dumps(entry["expect"], sort_keys=True),
                error,
            )
            print(message)
            mismatches.append({"path": path, "expected": entry["expect"], "error": error})
            continue

        if actual["result"] == "skip":
            skips += 1
            reason = actual.get("reason", "adapter skip")
            skipped.append({"path": path, "reason": reason})
            if not args.quiet:
                print("SKIP %s: %s" % (path, reason))
            if args.fail_on_skip:
                failures += 1
                mismatches.append({"path": path, "expected": entry["expect"], "error": reason})
            continue

        problem = compare(entry, actual)
        if problem is not None:
            failures += 1
            message = "VECTOR MISMATCH %s: expected %s got %s (%s)" % (
                path,
                json.dumps(entry["expect"], sort_keys=True),
                json.dumps(actual, sort_keys=True),
                problem,
            )
            print(message)
            mismatches.append(
                {
                    "path": path,
                    "expected": entry["expect"],
                    "actual": actual,
                    "reason": problem,
                }
            )
            continue

        passes += 1
        if not args.quiet:
            print("PASS %s" % path)

    total = len(entries)
    print(
        "conformance run: %d total, %d pass, %d fail, %d skip (manifest %s)"
        % (total, passes, failures, skips, manifest.get("aggregate_sha256", "?"))
    )
    if failures:
        print("conformance run: FAIL")

    if args.json:
        receipt = {
            "schema": "weft.conformance.run.v1",
            "implementation": args.implementation,
            "manifest": os.path.relpath(args.manifest, ROOT),
            "manifest_aggregate_sha256": manifest.get("aggregate_sha256"),
            "total": total,
            "pass": passes,
            "fail": failures,
            "skip": skips,
            "result": "pass" if failures == 0 else "fail",
            "mismatches": mismatches,
            "skips": skipped,
        }
        with open(args.json, "w", encoding="utf-8") as handle:
            json.dump(receipt, handle, indent=2, sort_keys=True)
            handle.write("\n")

    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
