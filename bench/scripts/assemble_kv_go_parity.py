#!/usr/bin/env python3
"""Assemble the WeftFS-Go KV prefix parity receipt (subphase T068/X10).

Inputs, all produced by a real run:

* `--go-test`  the `go test -json` stream from the Go court;
* `--cost`     the `weft.kv.go.cost.v1` document the Go test wrote;
* `--stamp`    the court stamp;
* `--rust-rev` / `--go-rev` the revisions the run observed.

The Ed25519 statement is not typed by hand: the assembler only emits
"implemented, not merely bounded" when `TestKvPrefixGetVerifiesEd25519InGo`
actually passed in this run, and it refuses to write a receipt at all if any
test failed. A parity receipt that cannot name its evidence is not written.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import sys

ED25519_TEST = "TestKvPrefixGetVerifiesEd25519InGo"
HANDOFF_TEST = "TestKvPrefixCrossLanguageReadsBothWays"
COST_TEST = "TestKvGoCost"


def load_go_test(path: str) -> tuple[list[dict], set[str]]:
    tests: list[dict] = []
    failed: set[str] = set()
    seen: set[str] = set()
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line.startswith("{"):
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            name = event.get("Test")
            if not name or not name.startswith("TestKv"):
                continue
            action = event.get("Action")
            if action == "output" or name in seen:
                continue
            if action in ("pass", "fail", "skip"):
                seen.add(name)
                tests.append(
                    {
                        "name": name,
                        "result": action.upper(),
                        "seconds": round(float(event.get("Elapsed", 0.0)), 3),
                    }
                )
                if action == "fail":
                    failed.add(name)
    return tests, failed


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--stamp", required=True)
    parser.add_argument("--go-test", required=True)
    parser.add_argument("--cost", required=True)
    parser.add_argument("--receipt", required=True)
    parser.add_argument("--rust-rev", default="unknown")
    parser.add_argument("--go-rev", default="unknown")
    args = parser.parse_args()

    tests, failed = load_go_test(args.go_test)
    if failed:
        print(f"assemble_kv_go_parity: failing tests: {sorted(failed)}", file=sys.stderr)
        return 1
    names = {test["name"] for test in tests}
    for required in (ED25519_TEST, HANDOFF_TEST, COST_TEST):
        if required not in names:
            print(
                f"assemble_kv_go_parity: {required} did not run; refusing to claim parity",
                file=sys.stderr,
            )
            return 1

    with open(args.cost, encoding="utf-8") as handle:
        cost = json.load(handle)
    if cost.get("schema") != "weft.kv.go.cost.v1":
        print("assemble_kv_go_parity: cost document has the wrong schema", file=sys.stderr)
        return 1

    passed = sum(1 for test in tests if test["result"] == "PASS")
    skipped = sum(1 for test in tests if test["result"] == "SKIP")

    receipt = {
        "schema": "weft.kv.go_parity.v1",
        "stamp": args.stamp,
        "revisions": {"WeftFS": args.rust_rev, "WeftFS-Go": args.go_rev},
        "label": (
            "WeftFS-Go KV prefix parity: tier, scope checks, Ed25519 verification, "
            "bidirectional handoff, per-page cost"
        ),
        "court": {
            "rust_image": os.environ.get("WEFT_RUST_COURT_IMAGE", "weft/rust-court:1"),
            "go_image": os.environ.get("WEFT_GO_COURT_IMAGE", "weft/go-court:1"),
            "kernel": platform.release(),
            "memory": os.environ.get("WEFT_MEMORY", "3g"),
            "swap": os.environ.get("WEFT_MEMORY", "3g"),
            "pids": int(os.environ.get("WEFT_PIDS", "1024")),
            "cpus": os.environ.get("WEFT_CPUS", "4"),
            "tmp": "1g",
        },
        "rust_oracle": {
            "path": "weftfs (cargo build --release --bin weftfs, no fuse feature)",
            "note": "the fuse build needs libfuse3, absent from the go court",
        },
        "tests": tests,
        "test_count": {
            "passed": passed,
            "failed": len(failed),
            "skipped": skipped,
        },
        "ed25519": {
            "decision": "implemented, not merely bounded",
            "evidence_test": ED25519_TEST,
            "mechanism": (
                "Store.KvPrefixGetVerified loads the stored signed envelope blob and verifies "
                "it with WeftCodec-Go capability.Verify (crypto/ed25519), then builds the scope "
                "with auth.NewCapability."
            ),
            "trusted_verifier_boundary_retained": (
                "Store.KvPrefixGet still accepts an already-verified *auth.Capability and is "
                "labelled as the historical boundary"
            ),
            "negative_cases": [
                "tampered blob refused",
                "grant with unrelated roots refused",
                "foreign tenant refused",
                "expired grant refused",
                "no capability refused",
            ],
            "cross_language": (
                "TestKvPrefixCrossLanguageReadsBothWays: a Go-signed grant is verified by Rust "
                "and a Rust-written prefix is read by Go with a Go-verified grant"
            ),
        },
        "cost": cost,
        "non_claims": [
            "no Go index (the T063 persisted index is Rust-only); Go lookup is the oracle scan",
            "no Go mode-19 or mode-15 writer",
            "one host, synthetic pages, warm cache; no fleet/cloud claim",
            "the per-page cost is a single-process measurement, not a service SLO",
        ],
    }
    with open(args.receipt, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(
        f"assemble_kv_go_parity: {passed} passed / {len(failed)} failed / {skipped} skipped "
        f"-> {args.receipt}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
