#!/usr/bin/env python3
"""Assemble the FUSE write-back benchmark receipt from raw JSON lines."""

import argparse
import json
import os
import platform
import statistics

CONCLUSION = (
    "Mount identity defaults to merkle; write-back batches a handle's writes into one "
    "commit; group-commit coalesces independent mutations into one commit."
)


def median(values):
    return statistics.median(values) if values else 0


def summarise_cell(rows):
    first = rows[0]
    write_p50 = [r["overwrite"]["writes"]["p50_ns"] for r in rows]
    write_p95 = [r["overwrite"]["writes"]["p95_ns"] for r in rows]
    write_p99 = [r["overwrite"]["writes"]["p99_ns"] for r in rows]
    write_ops = [r["overwrite"]["writes"]["ops_per_s"] for r in rows]
    final_sync = [r["overwrite"]["final_fsync_ns"] for r in rows]
    overwrite_wall = [r["overwrite"]["wall_ns"] for r in rows]
    meta = {}
    for op in ("create", "stat", "readdir", "unlink"):
        meta[op] = {
            "ops_per_s": median([r["meta"][op]["ops_per_s"] for r in rows]),
            "p50_ns": median([r["meta"][op]["p50_ns"] for r in rows]),
            "p95_ns": median([r["meta"][op]["p95_ns"] for r in rows]),
            "p99_ns": median([r["meta"][op]["p99_ns"] for r in rows]),
        }
    commits = median([r["generation_after"] - r["generation_before"] for r in rows])
    return {
        "impl": first["impl"],
        "identity": first["identity"],
        "mode": first["mode"],
        "reps": len(rows),
        "overwrite": {
            "count": first["overwrite"]["count"],
            "block_bytes": first["overwrite"]["block_bytes"],
            "file_size": first["overwrite"]["file_size"],
            "fsync": first["overwrite"]["fsync"],
            "p50_ns": median(write_p50),
            "p95_ns": median(write_p95),
            "p99_ns": median(write_p99),
            "ops_per_s": median(write_ops),
            "final_fsync_ns": median(final_sync),
            "wall_ns": median(overwrite_wall),
        },
        "metadata": meta,
        "commits_published": commits,
        "mount_peak_rss_kib": max(r["mount_peak_rss_kib"] for r in rows),
        "store_bytes": median([r["store_bytes"] for r in rows]),
        "verify_ok": all(r["verify_ok"] for r in rows),
        "raw": rows,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--raw", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--label", required=True)
    args = parser.parse_args()

    rows = []
    with open(os.path.join(args.raw, "cells.jsonl")) as fh:
        for line in fh:
            if line.strip():
                rows.append(json.loads(line))
    crash = []
    crash_path = os.path.join(args.raw, "crash.jsonl")
    if os.path.exists(crash_path):
        with open(crash_path) as fh:
            for line in fh:
                if line.strip():
                    crash.append(json.loads(line))

    grouped = {}
    for row in rows:
        key = (row["impl"], row["identity"], row["mode"])
        grouped.setdefault(key, []).append(row)
    cells = [summarise_cell(grouped[key]) for key in sorted(grouped)]

    env = {
        key: os.environ.get(key, "")
        for key in (
            "WEFT_COURT_IMAGE",
            "WEFT_COURT_IMAGE_ID",
            "WEFT_COURT_KERNEL",
            "WEFT_COURT_MEMORY",
            "WEFT_COURT_CPUS",
            "WEFT_COURT_PIDS",
            "WEFT_COURT_NPROC",
            "WEFT_REPO_WEFTFS",
            "WEFT_REPO_WEFTFS_GO",
        )
    }
    env["kernel"] = platform.release()
    env["machine"] = platform.machine()
    env["python"] = platform.python_version()

    commands = ""
    commands_path = os.path.join(args.raw, "commands.txt")
    if os.path.exists(commands_path):
        with open(commands_path) as fh:
            commands = fh.read()

    receipt = {
        "schema": "weft-fuse-writeback-v1",
        "kind": "mounted-filesystem-write-path",
        "generated_at": os.environ.get("WEFT_WB_GENERATED_AT", ""),
        "label": args.label,
        "environment": env,
        "revisions": {
            "weftfs": env["WEFT_REPO_WEFTFS"],
            "weftfs_go": env["WEFT_REPO_WEFTFS_GO"],
        },
        "images": {
            "court": env["WEFT_COURT_IMAGE"],
            "court_id": env["WEFT_COURT_IMAGE_ID"],
        },
        "kernel": env["WEFT_COURT_KERNEL"],
        "limits": {
            "memory": env["WEFT_COURT_MEMORY"],
            "memory_swap": env["WEFT_COURT_MEMORY"],
            "cpus": env["WEFT_COURT_CPUS"],
            "pids": env["WEFT_COURT_PIDS"],
            "nproc": env["WEFT_COURT_NPROC"],
            "tmpfs": "/tmp 1g",
            "no_new_privileges": True,
            "cap_drop": "ALL",
            "cap_add": "SYS_ADMIN",
            "device": "/dev/fuse",
        },
        "parameters": {
            "overwrites": rows[0]["overwrite"]["count"] if rows else 0,
            "block_bytes": rows[0]["overwrite"]["block_bytes"] if rows else 0,
            "file_size": rows[0]["overwrite"]["file_size"] if rows else 0,
            "fsync": "once at the end",
            "metadata_tree": "20 directories x 10 files",
            "reps": max((len(v) for v in grouped.values()), default=0),
            "mount_default_content_identity": "merkle",
            "fuse_writeback": "opt-in; buffer per handle, publish at flush/fsync/release",
            "fuse_group_commit": "opt-in; independent mutations coalesce within a 10 ms window, max 256 ops",
        },
        "available_cells": {
            "impl": ["rust", "go"],
            "identity": ["whole-file", "merkle"],
            "mode": ["off", "write-back", "group-commit", "write-back+group"],
        },
        "cells": cells,
        "crash_proxy": crash,
        "durability": {
            "default": "every mutation is a durably published commit before its reply",
            "write_back": "buffered bytes are returned to the writing handle immediately; other readers and the store see them only after flush/fsync/release; fsync is durable before it replies; a crash loses unflushed writes",
            "group_commit": "a group is durable only when published (window elapsed, 256 ops, fsync, or unmount); a crash leaves the previous whole generation",
            "crash_proxy_note": "SIGKILL of the daemon is a process-fault proxy, not physical power loss and not a device-cache claim",
        },
        "conclusion": CONCLUSION,
        "commands": commands,
    }
    with open(args.out, "w") as fh:
        json.dump(receipt, fh, indent=2, sort_keys=False)
        fh.write("\n")


if __name__ == "__main__":
    main()
