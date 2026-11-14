#!/usr/bin/env python3
"""Merge independent MW3 court runs into one median receipt.

The court is cheap enough to run several times, but a single run of a
`tmpfs`-backed, four-vCPU workload has a wide cell-to-cell spread. This script
takes N receipts produced by `scripts/mw3_court.sh` at the same revision, and
emits one receipt of the same schema whose row values are the **median across
runs**, with `spread` recording min/max per numeric field.

It is a presentation step, not a measurement: every merged cell names the runs
it came from, and each source receipt's path, stamp and sha256 is recorded.
Refuses to merge receipts with different schemas, revisions or row keys, so a
merge can never mix two fixtures.
"""

import argparse
import hashlib
import json
import os
import statistics
import sys


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def row_key(row):
    if "distribution" in row:
        return "%s/%s" % (row["distribution"], row["n"])
    if "mode" in row:
        return "%s/%s" % (row["mode"], row["n"])
    return row.get("label", "?")


def merge(receipts, out_path, schema, stamp):
    documents = [json.load(open(path)) for path in receipts]
    first = documents[0]
    if any(doc.get("schema") != schema for doc in documents):
        raise SystemExit("merge: every source must have schema %r" % schema)
    revisions = {json.dumps(doc.get("revisions"), sort_keys=True) for doc in documents}
    if len(revisions) != 1:
        raise SystemExit("merge: sources were not produced at one revision")
    ordered = []
    for doc in documents:
        keys = [row_key(row) for row in doc["rows"]]
        if keys != [row_key(row) for row in first["rows"]]:
            raise SystemExit("merge: sources do not have the same row set")
        ordered.append({row_key(row): row for row in doc["rows"]})

    merged_rows = []
    spread = {}
    for row in first["rows"]:
        key = row_key(row)
        out = dict(row)
        for field, value in row.items():
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                continue
            values = [run[key][field] for run in ordered
                      if isinstance(run[key].get(field), (int, float))]
            if not values:
                continue
            out[field] = statistics.median(values)
            spread.setdefault(key, {})[field] = {
                "min": min(values),
                "max": max(values),
                "runs": len(values),
            }
        merged_rows.append(out)

    # Recompute every ratio from the merged values so a median-of-ratios is
    # never presented as a ratio-of-medians.
    if schema == "weftfs.mw3.bench/1":
        base = next((r["throughput_tx_s"] for r in merged_rows
                     if r.get("distribution") == "disjoint" and r.get("n") == 1), 0.0)
        steady_base = next((r["steady_tx_s"] for r in merged_rows
                            if r.get("distribution") == "disjoint" and r.get("n") == 1), 0.0)
        for row in merged_rows:
            row["tx_per_writer_s"] = (row["throughput_tx_s"] / row["n"]
                                       if row.get("n") else None)
            row["scaling_efficiency"] = (
                row["throughput_tx_s"] / (row["n"] * base)
                if row.get("distribution") == "disjoint" and base else None)
            row["steady_efficiency"] = (
                row["steady_tx_s"] / (row["n"] * steady_base)
                if row.get("distribution") == "disjoint" and steady_base else None)
    elif schema == "weftfs.writer.scale/1":
        best = max((r["throughput_tx_s"] / r["n"] for r in merged_rows
                    if r.get("mode") == "threads-in-one-process" and r.get("n")),
                   default=0.0)
        for row in merged_rows:
            row["tx_per_writer_s"] = (row["throughput_tx_s"] / row["n"]
                                       if row.get("n") else None)
            row["per_writer_rate_vs_best"] = (
                row["tx_per_writer_s"] / best
                if best and row.get("tx_per_writer_s") is not None else None)

    document = {
        "schema": schema,
        "generated_at": first["generated_at"],
        "stamp": stamp,
        "merged_from": [
            {"receipt": path, "stamp": doc.get("stamp"), "sha256": sha256(path)}
            for path, doc in zip(receipts, documents)
        ],
        "runs": len(receipts),
        "aggregation": "median across runs; `spread` records min/max per numeric field",
        "revisions": first.get("revisions"),
        "environment": first.get("environment"),
        "rows": merged_rows,
        "spread": spread,
    }
    for field in ("profile", "contract", "method", "isolation", "not_claimed",
                  "scaling_efficiency_base", "baseline", "baseline_note",
                  "resource_envelope", "evidence"):
        if field in first:
            document[field] = first[field]
    json.dump(document, open(out_path, "w"), indent=2, sort_keys=True)
    open(out_path, "a").write("\n")
    return document


def merge_retry_runs(receipts, out_path, stamp):
    """Summarise several T021 before/after receipts.

    Each court invocation is one sample of a bimodal retry schedule, so a
    single run's delta is not the result. This keeps every run's own
    before/after pair and adds the median across runs, so a regression cannot
    be hidden by an average.
    """
    docs = [json.load(open(path)) for path in receipts]
    revisions = {json.dumps(doc.get("revisions"), sort_keys=True) for doc in docs}
    if len(revisions) != 1:
        raise SystemExit("merge: sources were not produced at one revision")
    rows = []
    for index, doc in enumerate(docs):
        arms = doc["arms"]
        before = arms["before"]["reads_per_commit_median"]
        after = arms["after"]["reads_per_commit_median"]
        rows.append({
            "run": index + 1,
            "stamp": doc["stamp"],
            "before_rpc": before,
            "after_rpc": after,
            "rpc_delta_percent": (after - before) / before * 100.0 if before else None,
            "before_p99_ms": arms["before"]["p99_ms_median"],
            "after_p99_ms": arms["after"]["p99_ms_median"],
            "before_tx_s": arms["before"]["throughput_tx_s_median"],
            "after_tx_s": arms["after"]["throughput_tx_s_median"],
            "before_amp": arms["before"]["retry_amplification_median"],
            "after_amp": arms["after"]["retry_amplification_median"],
            "after_cache_hits": arms["after"]["gap_commits_cached_median"],
            "after_gap_reads": arms["after"]["gap_commits_read_median"],
            "repetitions_per_arm": arms["after"]["repetitions"],
        })
    def median(field):
        values = sorted(row[field] for row in rows if row[field] is not None)
        return values[len(values) // 2] if values else None
    medians = {field: median(field) for field in (
        "before_rpc", "after_rpc", "rpc_delta_percent", "before_p99_ms",
        "after_p99_ms", "before_tx_s", "after_tx_s", "before_amp", "after_amp",
        "after_cache_hits", "after_gap_reads")}
    improvements = sum(1 for row in rows if (row["rpc_delta_percent"] or 0) < 0)
    p99_improvements = sum(
        1 for row in rows if row["after_p99_ms"] < row["before_p99_ms"])
    document = {
        "schema": "weftfs.mw2.retry.runs/1",
        "stamp": stamp,
        "runs": len(rows),
        "sources": [
            {"receipt": path, "stamp": doc.get("stamp"), "sha256": sha256(path)}
            for path, doc in zip(receipts, docs)
        ],
        "revisions": docs[0].get("revisions"),
        "baseline": docs[0].get("baseline"),
        "environment": docs[0].get("environment"),
        "method": docs[0].get("method"),
        "rows": rows,
        "medians": medians,
        "runs_with_lower_reads_per_commit": improvements,
        "runs_with_lower_p99": p99_improvements,
    }
    json.dump(document, open(out_path, "w"), indent=2, sort_keys=True)
    open(out_path, "a").write("\n")
    return document


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schema", required=True,
                        help="weftfs.mw3.bench/1, weftfs.writer.scale/1, or retry-runs")
    parser.add_argument("--stamp", required=True)
    parser.add_argument("-o", "--out", required=True)
    parser.add_argument("receipts", nargs="+")
    args = parser.parse_args(argv)
    for path in args.receipts:
        if not os.path.isfile(path):
            raise SystemExit("merge: missing receipt %s" % path)
    if args.schema == "retry-runs":
        document = merge_retry_runs(args.receipts, args.out, args.stamp)
        print(json.dumps({"out": args.out, "runs": document["runs"],
                          "medians": document["medians"]}))
        return 0
    document = merge(args.receipts, args.out, args.schema, args.stamp)
    print(json.dumps({"out": args.out, "runs": document["runs"],
                      "rows": len(document["rows"])}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
