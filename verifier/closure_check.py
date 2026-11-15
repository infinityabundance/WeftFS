#!/usr/bin/env python3
"""Clean-room checker for a bounded causal closure bundle.

This module knows nothing about the WeftFS store or the Rust/Go implementations.
It reads the canonical `weft.causal.v1` record bytes that the producer emitted
and recomputes the closure from those records alone. Every rule below is stated
in `spec/WEFT-CAUSAL.md` §1 and §4; the rules that were *not* stated there and
had to be learned from the implementation are recorded in
`docs/CAUSAL-CLOSURE-FINDINGS.md`.

Usage:
    python3 verifier/closure_check.py --bundle FILE

Output: one JSON object on stdout. Exit 0 when the bundle is self-consistent,
1 when it is not, 2 on a malformed invocation or unreadable bundle.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import deque

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import hashing  # noqa: E402
import wcb  # noqa: E402

CAUSAL_SCHEMA = "weft.causal.v1"
CAUSAL_DOMAIN = "weft/causal/v1"
DOMAIN = "weft/training-artifact/v1"


class Failure(Exception):
    def __init__(self, klass, message):
        super().__init__(message)
        self.klass = klass
        self.message = message


def decode_hex(value, what):
    if not isinstance(value, str) or len(value) % 2 != 0:
        raise Failure("Malformed", "%s must be lowercase hex" % what)
    try:
        raw = bytes.fromhex(value)
    except ValueError:
        raise Failure("Malformed", "%s must be hex" % what)
    if len(raw) != 32:
        raise Failure("Malformed", "%s must be 32 bytes" % what)
    return raw


def record_hashes(value, what):
    if not isinstance(value, list):
        raise Failure("Malformed", "%s must be an array" % what)
    out = []
    previous = None
    for item in value:
        if isinstance(item, wcb.Bytes):
            item = item.value.hex()
        raw = decode_hex(item, what)
        if raw == b"\x00" * 32:
            raise Failure("NonCanonical", "%s contains the zero hash" % what)
        if previous is not None and raw <= previous:
            raise Failure("NonCanonical", "%s is not strictly ascending" % what)
        previous = raw
        out.append(raw)
    return out


def parse_record(entry, index):
    raw = entry.get("record_bytes")
    if not isinstance(raw, str):
        raise Failure("Malformed", "record %d has no record_bytes" % index)
    try:
        body = bytes.fromhex(raw)
    except ValueError:
        raise Failure("Malformed", "record %d record_bytes is not hex" % index)
    try:
        value = wcb.decode(body)
    except wcb.WcbError as err:
        raise Failure(err.klass, "record %d: %s" % (index, err.message))
    if not isinstance(value, dict):
        raise Failure("NonCanonical", "record %d is not a map" % index)
    keys = set(value.keys())
    if keys != {"schema", "parents", "closure"}:
        raise Failure("NonCanonical", "record %d key set is %s" % (index, sorted(keys)))
    schema = value["schema"]
    if not isinstance(schema, str) or schema != CAUSAL_SCHEMA:
        raise Failure("UnknownSchema", "record %d schema is not weft.causal.v1" % index)
    parents = record_hashes(value["parents"], "record %d parents" % index)
    closure = record_hashes(value["closure"], "record %d closure" % index)
    closure_set = set(closure)
    for parent in parents:
        if parent not in closure_set:
            raise Failure("NonCanonical",
                          "record %d closure does not contain every parent" % index)
    recomputed = hashing.hash_domain(CAUSAL_DOMAIN, body)
    declared = entry.get("causal_id")
    if declared is not None and decode_hex(declared, "causal_id") != recomputed:
        raise Failure("Corrupt", "record %d causal id does not hash its bytes" % index)
    identity = decode_hex(entry.get("identity"), "identity")
    return identity, parents, closure


def compute(mode, roots, records, max_depth, max_nodes):
    """Recompute the closure. `records` maps identity -> (parents, closure)."""
    depth_of = {}
    queue = deque((root, 0) for root in roots)
    while queue:
        identity, depth = queue.popleft()
        if identity in depth_of and depth_of[identity] <= depth:
            continue
        if identity not in records:
            raise Failure(
                "DependencyMismatch",
                "causal ancestor %s is not resolvable at this snapshot" % identity.hex())
        if len(depth_of) >= max_nodes and identity not in depth_of:
            raise Failure("LimitExceeded", "causal closure exceeds max_nodes (%d)" % max_nodes)
        depth_of[identity] = depth
        parents, closure = records[identity]
        edges = parents if mode == "minimal" else sorted(set(parents) | set(closure))
        if edges and depth >= max_depth:
            raise Failure(
                "LimitExceeded",
                "causal closure depth exceeds max_depth (%d) at ancestor %s"
                % (max_depth, identity.hex()))
        for edge in edges:
            queue.append((edge, depth + 1))
    return depth_of


def check(bundle):
    if not isinstance(bundle, dict):
        raise Failure("Malformed", "bundle must be an object")
    if bundle.get("schema") != "weft.closure.bundle.v1":
        raise Failure("UnknownSchema", "bundle schema is not weft.closure.bundle.v1")
    mode = bundle.get("mode")
    if mode not in ("complete", "minimal"):
        raise Failure("InvalidArgument", "mode must be complete or minimal")
    max_depth = bundle.get("max_depth")
    max_nodes = bundle.get("max_nodes")
    if not isinstance(max_depth, int) or max_depth <= 0:
        raise Failure("LimitExceeded", "max_depth must be positive")
    if not isinstance(max_nodes, int) or max_nodes <= 0:
        raise Failure("LimitExceeded", "max_nodes must be positive")
    roots = record_hashes(bundle.get("roots"), "roots")
    if len(set(roots)) != len(roots):
        # Roots are a request set; duplicates are collapsed by the producer.
        roots = sorted(set(roots))
    entries = bundle.get("records")
    if not isinstance(entries, list):
        raise Failure("Malformed", "records must be an array")
    records = {}
    for index, entry in enumerate(entries):
        identity, parents, closure = parse_record(entry, index)
        if identity in records and records[identity] != (parents, closure):
            raise Failure("Corrupt", "record %s appears twice with different edges" % identity.hex())
        records[identity] = (parents, closure)

    declared = bundle.get("result")
    if not isinstance(declared, dict):
        raise Failure("Malformed", "result must be an object")
    refusal = bundle.get("refusal")
    if refusal is not None:
        try:
            compute(mode, roots, records, max_depth, max_nodes)
        except Failure as err:
            if err.klass != refusal.get("class"):
                raise Failure(
                    "Corrupt",
                    "declared refusal %s but recomputation refused %s"
                    % (refusal.get("class"), err.klass))
            missing = None
            if err.klass == "DependencyMismatch":
                for token in err.message.split():
                    if len(token) == 64:
                        try:
                            missing = bytes.fromhex(token)
                            break
                        except ValueError:
                            continue
                if missing is None:
                    raise Failure("Corrupt", "a missing-ancestor refusal names no identity")
                if missing.hex() not in refusal.get("message", "").lower():
                    raise Failure("Corrupt", "the refusal does not name the missing identity")
            return {
                "ok": True,
                "mode": mode,
                "records": len(records),
                "refusal": {"class": err.klass, "identity": missing.hex() if missing else None},
                "nodes": 0,
            }
        raise Failure("Corrupt", "a refusal was declared but the closure is complete")

    depth_of = compute(mode, roots, records, max_depth, max_nodes)
    nodes = declared.get("nodes")
    if not isinstance(nodes, list):
        raise Failure("Malformed", "result.nodes must be an array")
    seen = []
    previous = None
    for node in nodes:
        identity = decode_hex(node.get("identity"), "result node identity")
        if previous is not None and identity <= previous:
            raise Failure("Corrupt", "result nodes are not strictly ascending")
        previous = identity
        if identity not in depth_of:
            raise Failure("Corrupt", "result names %s outside the recomputed closure" % identity.hex())
        if node.get("depth") != depth_of[identity]:
            raise Failure(
                "Corrupt",
                "result depth for %s is %r, recomputed %d"
                % (identity.hex(), node.get("depth"), depth_of[identity]))
        seen.append(identity)
    if set(seen) != set(depth_of):
        raise Failure(
            "Corrupt",
            "result has %d nodes, recomputation has %d" % (len(seen), len(depth_of)))
    if declared.get("total") != len(depth_of) or declared.get("returned") != len(depth_of):
        raise Failure("Corrupt", "total/returned disagree with the recomputed closure")
    if declared.get("truncated") is not False:
        raise Failure("Corrupt", "v1 never truncates; truncated must be false")
    return {
        "ok": True,
        "mode": mode,
        "records": len(records),
        "nodes": len(depth_of),
        "max_depth_seen": max(depth_of.values()) if depth_of else 0,
        "roots": len(roots),
        "refusal": None,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", required=True, help="closure bundle JSON")
    args = parser.parse_args(argv)
    try:
        with open(args.bundle, "rb") as handle:
            bundle = json.load(handle)
    except OSError as err:
        print(json.dumps({"ok": False, "error": "read bundle: %s" % err}))
        return 2
    except json.JSONDecodeError as err:
        print(json.dumps({"ok": False, "error": "parse bundle: %s" % err}))
        return 2
    try:
        report = check(bundle)
    except Failure as err:
        report = {"ok": False, "class": err.klass, "error": err.message}
    except (KeyError, TypeError, ValueError) as err:
        report = {"ok": False, "class": "Malformed", "error": str(err)}
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
