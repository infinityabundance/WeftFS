#!/usr/bin/env python3
"""Compare two X06/N09 resource-court receipts, number by number.

The stage-budget fix changed only the oversize/reservation/FIFO paths. The
X06 restore rows and the N09 overlap arms are non-oversize, so a delta there
must be host noise; the unbudgeted X06 control arm is the proof, because it
never calls `StageBudget::admit` at all and therefore cannot see the patch.

This script resolves every non-expression `budget` group number from
`docs/perf-numbers.json` against the two receipts and writes a receipt-shaped
comparison (old value, new value, percentage delta per key). It is deterministic
and host-runnable (standard library only).

Usage:
  python3 scripts/stage_budget_delta.py \
    --old-restore bench/receipts/restore-budget-<old>.json \
    --new-restore bench/receipts/restore-budget-<new>.json \
    --old-stage bench/receipts/stage-budget-<old>.json \
    --new-stage bench/receipts/stage-budget-<new>.json \
    --out bench/receipts/stage-budget-delta-<new>.json
"""

from __future__ import annotations

import argparse
import json
import os
import sys


def read_json(path: str):
    with open(path) as handle:
        return json.load(handle)


def resolve(doc, entry):
    if "path" in entry:
        value = doc
        for part in entry["path"].split("."):
            value = value[int(part)] if isinstance(value, list) else value[part]
        return value
    if "list" in entry:
        where = entry.get("where", {})
        for row in resolve(doc, {"path": entry["list"]}):
            if all(row.get(key) == want for key, want in where.items()):
                return resolve(row, {"path": entry["select_path"]})
        raise KeyError(f"no row in {entry['list']} matches {where}")
    raise KeyError(f"entry {entry.get('key')} has no resolvable selector")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".")
    parser.add_argument("--manifest", default="docs/perf-numbers.json")
    parser.add_argument("--old-restore", required=True)
    parser.add_argument("--new-restore", required=True)
    parser.add_argument("--old-stage", required=True)
    parser.add_argument("--new-stage", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args(argv)

    os.chdir(args.root)
    manifest = read_json(args.manifest)
    old = {
        "restore-budget": read_json(args.old_restore),
        "stage-budget": read_json(args.old_stage),
    }
    new = {
        "restore-budget": read_json(args.new_restore),
        "stage-budget": read_json(args.new_stage),
    }

    rows = []
    for entry in manifest["numbers"]:
        if entry.get("group") != "budget" or "expr" in entry:
            continue
        # Historical quotations of the old receipt are not part of the comparison.
        if "_prev_" in entry["key"]:
            continue
        receipt = entry["receipt"]
        base = os.path.basename(receipt)
        if base.startswith("restore-budget-"):
            which = "restore-budget"
        elif base.startswith("stage-budget-"):
            which = "stage-budget"
        else:
            # N08 lives in the restore-cost receipt and X07 in the handle-cap
            # receipt; neither is the stage-budget comparison.
            continue
        old_value = resolve(old[which], entry)
        new_value = resolve(new[which], entry)
        delta_pct = None
        if old_value not in (0, 0.0):
            delta_pct = 100.0 * (new_value - old_value) / old_value
        rows.append(
            {
                "key": entry["key"],
                "receipt_kind": which,
                "old": old_value,
                "new": new_value,
                "delta_pct": delta_pct,
            }
        )

    receipt = {
        "schema": "weft.stage_budget_fix.delta.v1",
        "old": {
            "restore-budget": args.old_restore,
            "stage-budget": args.old_stage,
            "stamp": old["restore-budget"]["environment"]["generated_utc"]
            if "environment" in old["restore-budget"]
            else None,
            "revision": old["restore-budget"]["environment"]["revisions"]["WeftFS"]
            if "environment" in old["restore-budget"]
            else None,
        },
        "new": {
            "restore-budget": args.new_restore,
            "stage-budget": args.new_stage,
            "stamp": new["restore-budget"]["environment"]["generated_utc"],
            "revision": new["restore-budget"]["environment"]["revisions"]["WeftFS"],
        },
        "note": (
            "The changed code is reached only by oversize admissions, cancelled "
            "waiters and release_workers(). The X06 control arm (control_no_budget) "
            "never calls StageBudget::admit, so its delta is noise; every other row "
            "here is a non-oversize arm whose only new work is one boolean test and "
            "one empty-set lookup per grant."
        ),
        "control_arm_executes_changed_code": False,
        "rows": rows,
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
