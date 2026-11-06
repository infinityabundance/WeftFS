#!/usr/bin/env python3
"""Assemble the experimental entropy-coder court receipt.

The harness already emits one JSON object per row and a summary line. This
assembler adds the court environment, the verified corpus, the dictionary-scope
comparison, and the persistent-format check pointers. It performs no
measurement of its own.

Usage (inside the bench court):

  python3 bench/scripts/assemble_entropy_court.py \
      --rows /work/.docker-cache/entropy-court/rows.ndjson \
      --corpus /work/.docker-cache/entropy-court/corpus.json \
      --court-log /work/.docker-cache/entropy-court/court.log \
      --out bench/receipts/entropy-coder-<UTC>.json \
      --timestamp <UTC>
"""

import argparse
import datetime
import hashlib
import json
import os
import platform
import subprocess
import sys


def read_json(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def write_json(path, doc):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(doc, handle, indent=1, sort_keys=False)
        handle.write("\n")


def sha256_file(path, chunk=1 << 20):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while True:
            block = handle.read(chunk)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def load_rows(path):
    rows = []
    summary = None
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            doc = json.loads(line)
            if doc.get("type") == "row":
                rows.append(doc)
            elif doc.get("type") == "summary":
                summary = doc
    return rows, summary


def config_key(row):
    if row.get("coder") == "tunstall":
        return "tunstall-%d/%s" % (
            row["tunstall_k"],
            row["dictionary_scope"],
        )
    return row["coder"]


def aggregate(rows):
    totals = {}
    for row in rows:
        key = config_key(row)
        entry = totals.setdefault(
            key,
            {
                "rows": 0,
                "decoded_bytes": 0,
                "encoded_bytes": 0,
                "sum_encoded_amortized": 0.0,
                "sum_encode_ms_median": 0.0,
                "sum_decode_ms_p50": 0.0,
                "sum_p99_decode_ms": 0.0,
                "max_row_peak_rss_kib": 0,
            },
        )
        entry["rows"] += 1
        entry["decoded_bytes"] += row["decoded_bytes"]
        entry["encoded_bytes"] += row["encoded_bytes"]
        if row.get("encoded_bytes_amortized") is not None:
            entry["sum_encoded_amortized"] += row["encoded_bytes_amortized"]
        entry["sum_encode_ms_median"] += row["encode_ms_median"]
        entry["sum_decode_ms_p50"] += row["decode_ms_p50"]
        entry["sum_p99_decode_ms"] += row["decode_ms_p99"]
        entry["max_row_peak_rss_kib"] = max(
            entry["max_row_peak_rss_kib"], row.get("process_peak_rss_kib", 0)
        )
    for entry in totals.values():
        decoded = entry["decoded_bytes"]
        entry["encoded_ratio"] = (
            entry["encoded_bytes"] / decoded if decoded else 0.0
        )
        entry["pooled_encode_mib_s"] = (
            decoded / (1024.0 * 1024.0) / (entry["sum_encode_ms_median"] / 1000.0)
            if entry["sum_encode_ms_median"]
            else 0.0
        )
        entry["pooled_decode_mib_s"] = (
            decoded / (1024.0 * 1024.0) / (entry["sum_decode_ms_p50"] / 1000.0)
            if entry["sum_decode_ms_p50"]
            else 0.0
        )
        entry["sum_p99_decode_ms"] = round(entry["sum_p99_decode_ms"], 6)
        entry["sum_encode_ms_median"] = round(entry["sum_encode_ms_median"], 6)
        entry["sum_decode_ms_p50"] = round(entry["sum_decode_ms_p50"], 6)
    return totals


def scope_comparison(rows):
    """One entry per (input class, workload shape, k)."""
    buckets = {}
    for row in rows:
        if row.get("coder") != "tunstall":
            continue
        key = (row["input_class"], row["workload_shape"], row["tunstall_k"])
        entry = buckets.setdefault(
            key,
            {
                "input_class": row["input_class"],
                "workload_shape": row["workload_shape"],
                "decoded_bytes": row["decoded_bytes"],
                "tunstall_k": row["tunstall_k"],
                "family_members": None,
                "scopes": {},
            },
        )
        scope = row["dictionary_scope"]
        entry["scopes"][scope] = {
            "dictionary_bytes": row["dictionary_bytes"],
            "encoded_bytes": row["encoded_bytes"],
            "encoded_bytes_amortized": row.get("encoded_bytes_amortized"),
            "encode_ms_median": row["encode_ms_median"],
            "decode_ms_p50": row["decode_ms_p50"],
            "decode_ms_p99": row["decode_ms_p99"],
        }
        if scope == "family-shared":
            entry["family_members"] = row.get("family_members")
    return [buckets[key] for key in sorted(buckets)]


def class_summary(rows):
    by_class = {}
    for row in rows:
        entry = by_class.setdefault(
            row["input_class"],
            {
                "description": row["description"],
                "tensor": row["tensor"],
                "dtype": row["dtype"],
                "tensor_shape": row["tensor_shape"],
                "anchor_row": row["anchor_row"],
                "workload_shapes": {},
            },
        )
        shape = entry["workload_shapes"].setdefault(
            row["workload_shape"],
            {
                "rows": row["rows"],
                "columns": row["columns"],
                "elements": row["elements"],
                "plane_count": row["plane_count"],
                "decoded_bytes": row["decoded_bytes"],
                "plane_bytes": row["plane_bytes"],
            },
        )
        # Every coder row for a shape carries the same shape metadata.
        if shape["decoded_bytes"] != row["decoded_bytes"]:
            raise SystemExit("shape metadata disagrees across coder rows")
    return by_class


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", required=True)
    parser.add_argument("--corpus", required=True)
    parser.add_argument("--court-log", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--timestamp", required=True)
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--harness-sha256", required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--experimental-refs", required=True)
    parser.add_argument("--format-checks", default=None)
    parser.add_argument("--court-image", default=os.environ.get("WEFT_COURT_IMAGE", "unknown"))
    parser.add_argument("--court-image-id", default=os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"))
    parser.add_argument("--kernel", default=os.environ.get("WEFT_COURT_KERNEL", "unknown"))
    parser.add_argument("--memory", default=os.environ.get("WEFT_COURT_MEMORY", "unknown"))
    parser.add_argument("--cpus", default=os.environ.get("WEFT_COURT_CPUS", "unknown"))
    parser.add_argument("--pids", default=os.environ.get("WEFT_COURT_PIDS", "unknown"))
    parser.add_argument("--nproc", default=os.environ.get("WEFT_COURT_NPROC", "unknown"))
    args = parser.parse_args()

    rows, harness_summary = load_rows(args.rows)
    if not rows:
        raise SystemExit("no rows in %s" % args.rows)
    if harness_summary is None:
        raise SystemExit("no summary line in %s" % args.rows)
    if not all(row.get("roundtrip_ok") for row in rows):
        raise SystemExit("a row failed its round trip")

    try:
        rustc = subprocess.check_output(
            ["rustc", "--version"], text=True, stderr=subprocess.STDOUT
        ).strip()
    except Exception as error:  # pragma: no cover - diagnostics only
        rustc = "unknown (%s)" % error

    revisions = {
        "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        "weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        "weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
    }
    if revisions["weftcodec"] == "unknown":
        try:
            revisions["weftcodec"] = subprocess.check_output(
                ["git", "-C", "/work/WeftCodec", "rev-parse", "HEAD"],
                text=True,
                stderr=subprocess.DEVNULL,
            ).strip()
        except Exception:
            pass

    corpus = read_json(args.corpus)
    format_checks = read_json(args.format_checks) if args.format_checks else None

    totals = aggregate(rows)
    receipt = {
        "schema": "weft.entropy-coder-court.v1",
        "label": (
            "Experimental entropy-coder court on real public weight planes. "
            "Not a wire-format decision; no mode was added or changed."
        ),
        "generated_utc": args.timestamp,
        "environment": {
            "platform": platform.platform(),
            "kernel": args.kernel,
            "court_image": args.court_image,
            "court_image_id": args.court_image_id,
            "docker_server": os.environ.get("WEFT_DOCKER_SERVER", "unknown"),
            "docker_client": os.environ.get("WEFT_DOCKER_CLIENT", "unknown"),
            "limits": {
                "memory": args.memory,
                "cpus": args.cpus,
                "pids": args.pids,
                "nproc": args.nproc,
                "tmpfs": "1g",
                "cap_drop": "ALL",
                "no_new_privileges": True,
            },
            "rustc": rustc,
            "python": sys.version.split()[0],
            "os_cpu_count": os.cpu_count(),
            "court_cpu_limit": args.cpus,
            "revisions": revisions,
        },
        "corpus": corpus,
        "harness": {
            "example": "WeftCodec/examples/entropy_coder_court.rs",
            "sha256": args.harness_sha256,
            "rows_ndjson": os.path.basename(args.rows),
            "rows_ndjson_sha256": sha256_file(args.rows),
            "court_log": os.path.basename(args.court_log),
            "row_count": len(rows),
        },
        "experimental": {
            "module": "weftcodec::experimental::entropy",
            "reachable_from_persistent_paths": False,
            "experimental_refs_check": args.experimental_refs,
            "wire_modes_added": 0,
            "persistent_format_change": "none",
        },
        "method": (
            "All plane inputs are extracted through the shipped mode encoders "
            "(modes 9/11/12/15). Each coder encodes each plane independently; "
            "encoded_bytes sums the streams and charges the serialised Tunstall "
            "dictionary (per-plane: one per plane, per-tensor: one per tensor, "
            "family-shared: one per declared family, with encoded_bytes_amortized "
            "dividing that dictionary by the number of family members). "
            "Encode MiB/s uses the median of encode repetitions; decode MiB/s "
            "uses the p50 of decode repetitions. p99 is the observed percentile "
            "of the decode sample set. The pooled summary sums per-row medians "
            "weighted by decoded bytes; it is not one pooled timing run. "
            "The process peak RSS is the kernel high-water mark reset immediately "
            "before each row; the six input classes stay resident, so small-region "
            "rows report a floor set by resident inputs. All numbers are from this "
            "court run, single host, warm cache."
        ),
        "tunstall_dictionary_rule": (
            "Alphabet = all 256 bytes; one-byte phrases start with the observed "
            "probabilities. Repeatedly split the highest-probability phrase "
            "(ties: phrase bytes ascending, insertion index ascending) into its "
            "256 one-byte extensions; each split adds 255 entries. Stop when the "
            "next split would exceed 2^k or the serialised dictionary would exceed "
            "1 MiB. A greedy longest match can fail only at the final suffix; "
            "that suffix is carried literally in an explicit tail section. "
            "Codewords >= dictionary length and non-zero padding bits are rejected."
        ),
        "input_classes": class_summary(rows),
        "rows": rows,
        "summary": harness_summary,
        "coder_totals": totals,
        "dictionary_scopes": scope_comparison(rows),
        "persistent_formats": {
            "status": "unchanged",
            "evidence": format_checks,
            "note": (
                "This court adds an experimental module and its harness only. "
                "It defines no mode, changes no table, and writes no WFT1 frame. "
                "The cross-language gate and both conformance replays are run "
                "separately at the same code revision and cited here when supplied."
            ),
        },
        "residual_risks": [
            "Single host, warm page cache; no fleet or cold-cache claim.",
            "The court measures codec-level plane streams, not a shipped frame or store planner.",
            "Tunstall's 256-byte alphabet means each split consumes 255 codewords; k=8 cannot split at all and k=12 fits only 15 splits.",
            "The family-shared dictionary is built from a declared family (all tensors of the checkpoint pair where the mode applies, or 8 KV pages); its amortised cost is reported next to its full cost.",
            "The rANS encoder uses integer division per symbol and was not micro-optimised; its throughput is a real property of this implementation, not a bound on rANS.",
            "The process peak RSS of a small-region row includes the resident extracted planes of all six input classes.",
            "No zstd dictionary or long-window mode is tested; levels 3 and 19 only.",
        ],
    }
    write_json(args.out, receipt)
    print(
        "wrote %s: %d rows, %d coder configurations"
        % (args.out, len(rows), len(totals))
    )


if __name__ == "__main__":
    main()
