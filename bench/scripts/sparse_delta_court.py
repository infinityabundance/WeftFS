#!/usr/bin/env python3
"""Real-weight court for SPARSE_DELTA (mode 16) against WEIGHT_DELTA (mode 11).

For every pair this court measures, per tensor and in total and with both
methods lossless:

* mode-11 `WEIGHT_DELTA` frame bytes (the frozen dense delta),
* mode-16 `SPARSE_DELTA` frame bytes (the zero-run stream),
* per-tensor `zstd -19` bytes and decode medians already recorded by the P2
  tensors phase (read from `--prior-receipt`, never re-measured here),
* encode wall time for both codec modes and single-threaded decode time as the
  median of five repetitions.

Consecutive training-checkpoint pairs and the independent fine-tune are
reported in separate lists and are never aggregated.

Corpus files are SHA-256-verified against the manifest before any measurement.
The whole-file `zstd --patch-from` baselines are copied from the prior P2
receipt and labelled with that receipt's name and revision; they are not rerun.

Usage (inside the bench court, repository mounted at /work):

  python3 bench/scripts/sparse_delta_court.py \
      --manifest bench/corpora/real-models/float-tensor-real.json \
      --corpus-dir .docker-cache/real-corpus \
      --results-dir .docker-cache/real-corpus/results/sparse \
      --harness .docker-cache/target-rust/release/examples/real_sparse_delta_bench \
      --prior-receipt bench/receipts/real-float-tensor-20261105T053958Z.json \
      --receipt bench/receipts/sparse-delta-<UTC>.json
"""

import argparse
import datetime
import hashlib
import json
import os
import platform
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))


def sha256_file(path, chunk=1 << 20):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while True:
            block = handle.read(chunk)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def read_json(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def write_json(path, doc):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(doc, handle, indent=1, sort_keys=False)
        handle.write("\n")


def now_utc():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def run_timed(args, timeout=7200):
    started = time.perf_counter()
    completed = subprocess.run(
        args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout
    )
    wall = time.perf_counter() - started
    if completed.returncode != 0:
        raise SystemExit(
            "command failed (%d): %s\nstdout:\n%s\nstderr:\n%s"
            % (
                completed.returncode,
                " ".join(args),
                completed.stdout.decode("utf-8", "replace")[-4000:],
                completed.stderr.decode("utf-8", "replace")[-4000:],
            )
        )
    return {
        "wall_seconds": round(wall, 3),
        "stdout_tail": completed.stdout.decode("utf-8", "replace").splitlines()[-5:],
        "stderr_tail": completed.stderr.decode("utf-8", "replace").splitlines()[-5:],
    }


def environment():
    def env(name):
        value = os.environ.get(name)
        return value if value else None

    return {
        "kernel": env("WEFT_COURT_KERNEL") or platform.release(),
        "platform": platform.platform(),
        "court_image": env("WEFT_COURT_IMAGE"),
        "court_image_id": env("WEFT_COURT_IMAGE_ID"),
        "docker_server": env("WEFT_DOCKER_SERVER"),
        "docker_client": env("WEFT_DOCKER_CLIENT"),
        "limits": {
            "memory": env("WEFT_COURT_MEMORY"),
            "cpus": env("WEFT_COURT_CPUS"),
            "pids": env("WEFT_COURT_PIDS"),
            "nproc": env("WEFT_COURT_NPROC"),
        },
        "revisions": {
            "weftfs": env("WEFT_REPO_WEFTFS"),
            "weftcodec": env("WEFT_REPO_WEFTCODEC"),
            "weftcodec_go": env("WEFT_REPO_WEFTCODEC_GO"),
            "weftfs_go": env("WEFT_REPO_WEFTFS_GO"),
        },
        "python": sys.version.split()[0],
    }


def verify_corpus(manifest, corpus_dir):
    verified = []
    for case in manifest["cases"]:
        path = os.path.join(REPO, case["path"])
        if not os.path.isfile(path):
            raise SystemExit("missing corpus file: %s" % case["path"])
        size = os.path.getsize(path)
        if size != case["size"]:
            raise SystemExit(
                "%s size %d != manifest %d" % (case["path"], size, case["size"])
            )
        digest = sha256_file(path)
        if digest != case["sha256"]:
            raise SystemExit(
                "%s sha256 %s != manifest %s" % (case["path"], digest, case["sha256"])
            )
        verified.append(
            {
                "id": case["id"],
                "repo": case["repo"],
                "revision": case.get("revision"),
                "path": case["path"],
                "size": size,
                "sha256": digest,
                "verified": True,
            }
        )
    return verified


def prior_pair_rows(prior, pair_id):
    """Find one pair's recorded rows in the prior receipt."""
    delta = prior.get("delta") or {}
    for group in ("consecutive_training_checkpoints", "independent_fine_tune"):
        for entry in delta.get(group) or []:
            if entry.get("id") == pair_id:
                return group, entry
    return None, None


def patch_from_summary(entry):
    if not entry:
        return None
    rows = []
    for record in entry.get("patch_from") or []:
        rows.append(
            {
                "level": record.get("level"),
                "compressed_bytes": record.get("compressed_bytes"),
                "ratio_of_target": record.get("ratio_of_target"),
                "outcome": record.get("outcome"),
                "compressed_sha256": record.get("compressed_sha256"),
            }
        )
    return rows


def summarise(pair_doc):
    totals = pair_doc["totals"]
    losses = totals.get("mode16_worst10_losses") or []
    wins = []
    for row in pair_doc["per_tensor"]:
        if row["mode16_bytes"] is not None and row["mode16_bytes"] < row["mode11_bytes"]:
            wins.append(
                {
                    "name": row["name"],
                    "mode11_bytes": row["mode11_bytes"],
                    "mode16_bytes": row["mode16_bytes"],
                    "ratio": row["mode16_over_mode11"],
                }
            )
    wins.sort(key=lambda item: item["ratio"])
    return {
        "totals": totals,
        "per_tensor": pair_doc["per_tensor"],
        "mode16_best10_wins": wins[:10],
        "mode16_worst10_losses": losses,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest", default="bench/corpora/real-models/float-tensor-real.json"
    )
    parser.add_argument("--corpus-dir", default=".docker-cache/real-corpus")
    parser.add_argument("--results-dir", default=".docker-cache/real-corpus/results/sparse")
    parser.add_argument(
        "--harness",
        default=".docker-cache/target-rust/release/examples/real_sparse_delta_bench",
    )
    parser.add_argument(
        "--prior-receipt",
        default="bench/receipts/real-float-tensor-20261105T053958Z.json",
    )
    parser.add_argument("--receipt", required=True)
    args = parser.parse_args(argv)

    manifest_path = os.path.join(REPO, args.manifest)
    manifest = read_json(manifest_path)
    prior_path = os.path.join(REPO, args.prior_receipt)
    prior = read_json(prior_path)
    harness = os.path.join(REPO, args.harness)
    if not os.path.isfile(harness):
        raise SystemExit("harness not found: %s" % args.harness)

    corpus = verify_corpus(manifest, args.corpus_dir)
    cases = {case["id"]: case for case in manifest["cases"]}
    results_dir = os.path.join(REPO, args.results_dir)
    os.makedirs(results_dir, exist_ok=True)

    consecutive = []
    fine_tune = []
    patch_baselines = []

    for pair in manifest["pairs"]:
        pair_id = pair["id"]
        base = cases[pair["base"]]
        target = cases[pair["target"]]
        group, entry = prior_pair_rows(prior, pair_id)
        if entry is None or "per_tensor" not in entry:
            raise SystemExit(
                "prior receipt has no per_tensor rows for pair %s" % pair_id
            )
        target_json = os.path.join(results_dir, pair_id + ".target-rows.json")
        write_json(target_json, {"per_tensor": entry["per_tensor"]})
        out = os.path.join(results_dir, pair_id + ".json")
        run = run_timed(
            [
                harness,
                "pair",
                "--base-file",
                os.path.join(REPO, base["path"]),
                "--target-file",
                os.path.join(REPO, target["path"]),
                "--target-json",
                target_json,
                "--label",
                pair_id,
                "--kind",
                pair["kind"],
                "--out",
                out,
            ]
        )
        doc = read_json(out)
        summary = summarise(doc)
        record = {
            "id": pair_id,
            "kind": pair["kind"],
            "note": pair.get("note"),
            "base": pair["base"],
            "target": pair["target"],
            "base_sha256": base["sha256"],
            "target_sha256": target["sha256"],
            "harness_run": run,
            "prior_receipt_pair_group": group,
            # Whole-file `zstd --patch-from` numbers as recorded by the P2
            # receipt; they were not re-run for this court.
            "patch_from_recorded": patch_from_summary(entry),
            **summary,
        }
        if pair["kind"] == "independent-fine-tune":
            fine_tune.append(record)
        else:
            consecutive.append(record)
        patch_baselines.append(
            {
                "id": pair_id,
                "source_receipt": args.prior_receipt,
                "source_revision": (prior.get("environment") or {})
                .get("revisions", {})
                .get("weftcodec"),
                "patch_from": patch_from_summary(entry),
            }
        )

    def totals_for(records):
        return {
            "pairs": len(records),
            "mode11_bytes_sum_over_pairs": sum(r["totals"]["mode11_bytes"] for r in records),
            "mode16_bytes_sum_over_pairs": sum(
                r["totals"]["mode16_bytes"] for r in records
            ),
            "mode16_tensors_refused": sum(
                r["totals"]["mode16_refused"] for r in records
            ),
            "zstd19_bytes_sum_over_pairs": sum(r["totals"]["zstd19_bytes"] for r in records),
            "tensors_mode16_smaller": sum(
                r["totals"]["tensors_mode16_smaller"] for r in records
            ),
            "tensors_mode11_smaller": sum(
                r["totals"]["tensors_mode11_smaller"] for r in records
            ),
            "tensors_equal": sum(r["totals"]["tensors_equal"] for r in records),
        }

    findings = []
    for record in consecutive + fine_tune:
        totals = record["totals"]
        findings.append(
            "{}: mode 11 {} B, mode 16 {} B over {} encodable tensor(s) "
            "({}x mode 11, {}x per-tensor zstd-19); {} tensors smaller with "
            "mode 16, {} with mode 11, {} equal, {} refused by max_sparse_runs".format(
                record["id"],
                totals["mode11_bytes"],
                totals["mode16_bytes"],
                totals["mode16_frames"],
                round(totals["mode16_over_mode11"], 4),
                round(totals["mode16_over_zstd19"], 4),
                totals["tensors_mode16_smaller"],
                totals["tensors_mode11_smaller"],
                totals["tensors_equal"],
                totals["mode16_refused"],
            )
        )

    receipt = {
        "schema": "weft.real-sparse-delta.receipt.v1",
        "label": (
            "single host, public weights, warm cache, no fleet claim; codec-level "
            "per-tensor frames, not the store planner; consecutive training "
            "checkpoints and the independent fine-tune are reported separately"
        ),
        "generated_utc": now_utc(),
        "environment": environment(),
        "corpus": {
            "manifest": args.manifest,
            "manifest_sha256": sha256_file(manifest_path),
            "cases": corpus,
        },
        "method": (
            "per tensor: the exact base tensor bytes are encoded as a mode-6 "
            "TENSOR frame; mode 11 and mode 16 are encoded against that frame and "
            "both decoded, and each decode is compared against the original bytes "
            "and against the other mode. Encode wall time is one measurement per "
            "mode per tensor. Decode time is the median of five repetitions of "
            "parse+decode. zstd -19 bytes and decode medians come from the target "
            "checkpoint's existing P2 tensors phase; they are not re-measured. "
            "Totals are sums over the tensors of one pair and are never combined "
            "across pair kinds."
        ),
        "consecutive_training_checkpoints": consecutive,
        "independent_fine_tune": fine_tune,
        "totals_by_kind": {
            "consecutive_training_checkpoints": totals_for(consecutive),
            "independent_fine_tune": totals_for(fine_tune),
        },
        "patch_from_baselines": {
            "source_receipt": args.prior_receipt,
            "note": (
                "whole-file `zstd --patch-from` sizes were recorded by the P2 court "
                "in the receipt named above and were not re-run for this court"
            ),
            "pairs": patch_baselines,
        },
        "findings": findings,
        "reproduce": [
            "scripts/docker-court.sh rust \"cd /work/WeftCodec && cargo build --release --example real_sparse_delta_bench\"",
            "scripts/docker-court.sh bench \"python3 bench/scripts/sparse_delta_court.py --receipt bench/receipts/sparse-delta-<UTC>.json\"",
        ],
        "residual_risks": [
            "one host, one court image, warm page cache; no cold-cache claim",
            "codec-level frames only; the store planner's own choice is covered by tests, not by this receipt",
            "zstd baselines are copied from a prior receipt (not re-measured) and are per-tensor zstd -19 only",
            "decode totals are sums of per-tensor medians, not one-pass wall time",
            "mode 16 is only beneficial for sparse deltas; the dense fine-tune pair is expected to lose and is reported",
        ],
    }
    write_json(os.path.join(REPO, args.receipt), receipt)
    print(
        "wrote %s: %d consecutive pairs, %d fine-tune pair(s)"
        % (args.receipt, len(consecutive), len(fine_tune))
    )
    for line in findings:
        print("  " + line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
