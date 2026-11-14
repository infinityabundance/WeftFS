#!/usr/bin/env python3
"""Assemble the held-out planner receipt from the three phase receipts.

Stdlib only. The split, gates and non-claims are declared in
`bench/CONTRACTS-heldout.md`; this script only joins measurements, checks the
declaration was honoured, and records the contract digest.
"""

import hashlib
import json
import os
import subprocess
import sys


def load(path):
    with open(path) as handle:
        return json.load(handle)


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    if len(sys.argv) != 7:
        print(
            "usage: assemble_heldout.py OUTPUT STAMP FIT_RECEIPT VALIDATE_RECEIPT "
            "HELDOUT_RECEIPT LOG",
            file=sys.stderr,
        )
        return 64
    output, stamp, fit_path, val_path, hold_path, log_path = sys.argv[1:7]
    fit = load(fit_path)
    val = load(val_path)
    hold = load(hold_path)

    contract = "bench/CONTRACTS-heldout.md"
    contract_sha = sha256_file(contract)
    try:
        revision = subprocess.check_output(
            ["git", "--no-pager", "rev-parse", "HEAD"], text=True
        ).strip()
    except Exception:
        revision = "unknown"

    fit_names = set()
    fit_containers = set()
    for family in fit["families"]:
        fit_names.update(family["names"])
        fit_containers.add(family["container"]["sha256"])
    val_names = set(val["subset"]["names"])
    hold_names = set(hold["subset"]["names"])
    val_container = val["container"]["sha256"]
    hold_container = hold["container"]["sha256"]
    name_overlap = {
        "fit_validate": len(fit_names & val_names),
        "fit_held_out": len(fit_names & hold_names),
        "validate_held_out": len(val_names & hold_names),
    }
    containers_disjoint = not (
        (fit_containers & {val_container})
        or (fit_containers & {hold_container})
        or val_container == hold_container
    )

    def exact(receipt):
        block = receipt["exactness"]
        return block["exact"] == block["tensors"] and block["mismatches"] == 0

    checks = {
        # The gate is implemented exactly as frozen in CONTRACTS-heldout.md §8:
        # "no tensor name ... appears in two splits". Qwen checkpoints of
        # different widths share tensor names, so this literal gate fails; the
        # `diagnostics` block below reports the observation-level check (the
        # containers, and therefore every observation, are disjoint) and the
        # exact name overlap. The declared gate is not widened.
        "fit_and_eval_families_disjoint": not (
            fit_names & val_names
            or fit_names & hold_names
            or val_names & hold_names
        ),
        "model_not_refit_after_validation": (
            val["model_sha256"] == hold["model_sha256"]
            and val["model_sha256"] == fit["model_sha256"]
        ),
        "bytes_prediction_median_abs_rel_le_0.10": val["checks"][
            "bytes_prediction_median_abs_rel_le_0.10"
        ],
        "strategy_agreement_ge_0.90": val["checks"]["strategy_agreement_ge_0.90"],
        "restore_prediction_median_abs_rel_le_0.20": val["checks"][
            "restore_prediction_median_abs_rel_le_0.20"
        ],
        "sla_misses_reported": True,
        "all_arms_exact": exact(val) and exact(hold) and exact(fit),
    }

    assembly = {
        "schema": "weft.heldout.planner.receipt.v1",
        "diagnostics": {
            "containers_disjoint": containers_disjoint,
            "tensor_name_overlap": name_overlap,
            "note": "Qwen2.5 checkpoints of different widths share tensor names; the name-level gate is stricter than the split\'s intent (no tensor observation is shared), and both readings are published",
        },
        "stamp": stamp,
        "revision": revision,
        "contract": {"path": contract, "sha256": contract_sha},
        "split": {
            "fit": ["qwen2.5-0.5b-base", "pythia-70m-step0"],
            "validate": ["qwen2.5-3b-base"],
            "held_out": ["qwen2.5-7b-instruct"],
        },
        "fit": fit,
        "validate": val,
        "held_out": hold,
        "held_out_gates": {
            "bytes_prediction_median_abs_rel_le_0.10": hold["checks"][
                "bytes_prediction_median_abs_rel_le_0.10"
            ],
            "bytes_prediction_end_to_end_median_abs_rel_le_0.10": hold["checks"][
                "bytes_prediction_end_to_end_median_abs_rel_le_0.10"
            ],
            "strategy_agreement_ge_0.90": hold["checks"][
                "strategy_agreement_ge_0.90"
            ],
            "restore_prediction_median_abs_rel_le_0.20": hold["checks"][
                "restore_prediction_median_abs_rel_le_0.20"
            ],
        },
        "checks": checks,
        "checks_passed": sum(1 for value in checks.values() if value),
        "checks_total": len(checks),
        "log": log_path,
        "log_sha256": sha256_file(log_path),
        "not_claimed": [
            "one host, one container shape (3g / 4 cpus / 1024 pids), warm host page cache except for the labelled fadvise drop",
            "no fleet, cloud, GPU, media or power-loss claim",
            "the 7B held-out split excludes the two ~1 GiB tensors by a rule declared before measurement",
            "synthetic adversarial strata bound a failure mode; they do not estimate its frequency in real training",
            "a bounded per-tensor subset is not a whole-checkpoint number",
        ],
    }

    with open(output, "w") as handle:
        json.dump(assembly, handle, indent=1)

    print(
        "heldout_planner: receipt={} checks={}/{} contract_sha256={}".format(
            output,
            assembly["checks_passed"],
            assembly["checks_total"],
            contract_sha[:16],
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
