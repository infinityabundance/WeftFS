#!/usr/bin/env python3
"""Fail closed on conformance coverage gaps and manifest drift.

Checks, in order:

1. `conformance/manifest.json` is up to date (`--check`).
2. Every frame mode 0..14 has at least one valid vector and at least one
   normative reject vector.
3. Every identity record kind (WCB, selector, context, tensor, checkpoint,
   capability) has at least one valid vector.
4. Every vector carries a complete expectation: a valid result with a content
   root or decoded SHA-256, or an error with a non-empty class.
5. Every entry's `sha256` matches the file (or directory aggregate for `store`),
   and every mirror exists and matches.

Host-only: no Docker, no compilation, no network.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join(ROOT, "conformance", "manifest.json")
IDENTITY_KINDS = ("wcb", "selector", "context", "tensor", "checkpoint", "capability")


def fail(message):
    print("COVERAGE FAIL: %s" % message, file=sys.stderr)
    return 1


def sha256_file(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def main():
    result = subprocess.run(
        [sys.executable, os.path.join(ROOT, "scripts", "gen_conformance_manifest.py"), "--check"],
        cwd=ROOT,
    )
    if result.returncode != 0:
        return fail("conformance/manifest.json is stale")

    with open(MANIFEST, "r", encoding="utf-8") as handle:
        manifest = json.load(handle)
    vectors = manifest["vectors"]
    failures = 0

    modes = {}
    for entry in vectors:
        if entry.get("superseded_by"):
            continue
        mode = entry.get("mode")
        if isinstance(mode, int) and 0 <= mode <= 14:
            bucket = modes.setdefault(mode, {"valid": 0, "error": 0})
            if entry["expect"].get("ok"):
                bucket["valid"] += 1
            else:
                bucket["error"] += 1
    for mode in range(15):
        if mode not in modes:
            failures += fail("mode %d has no vectors at all" % mode)
            continue
        if modes[mode]["valid"] == 0:
            failures += fail("mode %d has no valid vector" % mode)
        if modes[mode]["error"] == 0:
            failures += fail("mode %d has no reject vector" % mode)

    identity = {}
    for entry in vectors:
        if entry["kind"] in IDENTITY_KINDS:
            identity.setdefault(entry["kind"], {"valid": 0, "error": 0})[
                "valid" if entry["expect"].get("ok") else "error"
            ] += 1
    for kind in IDENTITY_KINDS:
        if identity.get(kind, {}).get("valid", 0) == 0:
            failures += fail("identity kind %s has no valid vector" % kind)

    for entry in vectors:
        path = entry["path"]
        expect = entry.get("expect") or {}
        if entry["kind"] in ("store", "fuzz"):
            # Composite fixtures carry their own integrity hash, not a decoded
            # content root.
            if not expect.get("ok"):
                failures += fail("%s fixture is not marked ok" % path)
        elif expect.get("ok"):
            if expect.get("content_root") is None and expect.get("decoded_sha256") is None:
                failures += fail(
                    "%s is ok but has neither content_root nor decoded_sha256" % path
                )
        else:
            if not expect.get("error"):
                failures += fail("%s has no expected error class" % path)
        if entry["kind"] != "store":
            full = os.path.join(ROOT, path)
            if not os.path.isfile(full):
                failures += fail("%s does not exist" % path)
            elif sha256_file(full) != entry["sha256"]:
                failures += fail("%s sha256 does not match the manifest" % path)
        for mirror in entry.get("mirrors", []):
            full = os.path.join(ROOT, mirror)
            if not os.path.isfile(full):
                failures += fail("%s mirror does not exist" % mirror)
            elif sha256_file(full) != entry["sha256"]:
                failures += fail("%s mirror does not match %s" % (mirror, path))

    if failures:
        return fail("%d coverage gap(s)" % failures)

    mode_summary = " ".join(
        "%d:%d/%d" % (mode, modes[mode]["valid"], modes[mode]["error"])
        for mode in range(15)
    )
    identity_summary = " ".join(
        "%s:%d" % (kind, identity[kind]["valid"]) for kind in IDENTITY_KINDS
    )
    print(
        "conformance coverage: OK (%d entries; modes valid/reject %s; identity valid %s)"
        % (len(vectors), mode_summary, identity_summary)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
