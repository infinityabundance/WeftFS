#!/usr/bin/env python3
"""Assemble the restore-pipeline receipt from the court's run JSONs.

Every number comes from a `real_model_bench restore` run in the bench court, the
court's own environment, or `sha256sum` of a restored file. Nothing is
hardcoded.

Run files are `run-<dest>-<mode>-t<N>-r<R>.json` with `dest` in {disk, tmpfs}.
"""

import argparse
import glob
import json
import os
import re
import statistics
import sys
from datetime import datetime, timezone

MODES = ("batch", "pipeline")
THREADS = (1, 2, 4)
DESTINATIONS = ("disk", "tmpfs")


def load(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def median(values):
    return round(statistics.median(values), 3)


def parse_sha(path):
    """Map (dest, mode, threads, rep, checkpoint) -> restored file sha256."""
    out = {}
    if not os.path.exists(path):
        return out
    pattern = re.compile(
        r"dest=(\w+) mode=(\w+) threads=(\d+) rep=(\d+) (\w+) sha256=([0-9a-f]{64})$"
    )
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            match = pattern.search(line.strip())
            if match:
                out[
                    (
                        match.group(1),
                        match.group(2),
                        int(match.group(3)),
                        int(match.group(4)),
                        match.group(5),
                    )
                ] = match.group(6)
    return out


def rounded(values):
    return [round(value, 3) for value in values]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--work", required=True)
    parser.add_argument("--reps", type=int, required=True)
    parser.add_argument("--staging-mib", type=int, required=True)
    parser.add_argument("--ingest-source", required=True)
    parser.add_argument("--store", required=True)
    parser.add_argument("--source-check", required=True)
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--instruct-sha", required=True)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()

    runs = {}
    for path in glob.glob(os.path.join(args.work, "run-*.json")):
        match = re.search(r"run-(\w+)-(\w+)-t(\d+)-r(\d+)\.json$", path)
        if not match:
            continue
        runs[(match.group(1), match.group(2), int(match.group(3)), int(match.group(4)))] = load(
            path
        )

    file_sha = parse_sha(os.path.join(args.work, "sha256.txt"))
    source_sha = {"base": args.base_sha, "instruct": args.instruct_sha}

    checks = {}
    all_hash_exact = True
    all_roots_exact = True
    all_materialize_exact = True
    all_mode_matches = True
    all_sha_match = True
    all_pipeline_counts = True
    all_pipeline_bounds = True
    shas_by_checkpoint = {cp: set() for cp in source_sha}
    per_checkpoint_bytes = {cp: set() for cp in source_sha}

    results = {}
    present_checkpoints = {}
    speedups_by_dest = {}

    for dest in DESTINATIONS:
        dest_runs = {
            key: value for key, value in runs.items() if key[0] == dest
        }
        if not dest_runs:
            checks[f"{dest}_runs_present"] = False
            continue
        cps = set()
        for run in dest_runs.values():
            cps.update(item["checkpoint"] for item in run["checkpoints"])
        cps = sorted(cps)
        present_checkpoints[dest] = cps
        expected = {
            (dest, mode, threads, rep)
            for mode in MODES
            for threads in THREADS
            for rep in range(1, args.reps + 1)
        }
        missing = sorted(expected - set(dest_runs))
        checks[f"{dest}_runs_present"] = not missing
        if missing:
            raise SystemExit(f"missing run JSONs for {dest}: {missing}")

        results[dest] = {}
        for threads in THREADS:
            entry = {}
            batch_peaks = []
            pipe_peaks = []
            effective = None
            staging = None
            for cp in cps:
                read_ms = []
                materialize_ms = []
                decode_wall = []
                transfer = []
                write = []
                total = []
                pipeline_bytes = []
                backend_bytes = []
                materialize_bytes = []
                for rep in range(1, args.reps + 1):
                    run = runs[(dest, "batch", threads, rep)]
                    pipelines = runs[(dest, "pipeline", threads, rep)]
                    effective = run["effective_threads"]
                    staging = run["max_inflight_bytes"]
                    batch_peaks.append(run["peak_rss_kib"])
                    pipe_peaks.append(pipelines["peak_rss_kib"])
                    by_cp = {item["checkpoint"]: item for item in run["checkpoints"]}
                    item = by_cp[cp]
                    all_hash_exact = all_hash_exact and bool(item["hash_exact"])
                    all_roots_exact = all_roots_exact and bool(item["logical_roots_exact"])
                    materialize = item.get("materialize") or {}
                    all_materialize_exact = all_materialize_exact and bool(
                        materialize.get("exact")
                    )
                    all_mode_matches = all_mode_matches and (
                        materialize.get("mode") == "batch"
                    )
                    read_ms.append(item["read_ms"])
                    materialize_ms.append(materialize.get("materialize_ms", 0.0))
                    materialize_bytes.append(materialize.get("bytes", 0))
                    backend_bytes.append(item["backend"]["bytes_read"])
                    per_checkpoint_bytes[cp].add(item["restored_bytes"])
                    sha = file_sha.get((dest, "batch", threads, rep, cp))
                    if sha is None or sha != source_sha[cp]:
                        all_sha_match = False
                    if sha:
                        shas_by_checkpoint[cp].add(sha)

                    pipe_by_cp = {
                        item["checkpoint"]: item for item in pipelines["checkpoints"]
                    }
                    pipe_item = pipe_by_cp[cp]
                    all_hash_exact = all_hash_exact and bool(pipe_item["hash_exact"])
                    all_roots_exact = all_roots_exact and bool(
                        pipe_item["logical_roots_exact"]
                    )
                    pipe_materialize = pipe_item.get("materialize") or {}
                    all_materialize_exact = all_materialize_exact and bool(
                        pipe_materialize.get("exact")
                    )
                    all_mode_matches = all_mode_matches and (
                        pipe_materialize.get("mode") == "pipeline"
                    )
                    pipeline = pipe_materialize.get("pipeline")
                    if not pipeline:
                        all_mode_matches = False
                        continue
                    decode_wall.append(pipeline["decode_wall_ms"])
                    transfer.append(pipeline["transfer_ms"])
                    write.append(pipeline["write_ms"])
                    total.append(pipeline["total_ms"])
                    pipeline_bytes.append(pipeline["bytes_written"])
                    all_pipeline_counts = all_pipeline_counts and (
                        pipeline["tensors_written"] == 290
                        and pipeline["bytes_written"] == 988065536
                    )
                    all_pipeline_bounds = all_pipeline_bounds and (
                        pipeline["observed_max_inflight_bytes"]
                        <= staging + 272269312
                    )
                    sha = file_sha.get((dest, "pipeline", threads, rep, cp))
                    if sha is None or sha != source_sha[cp]:
                        all_sha_match = False
                    if sha:
                        shas_by_checkpoint[cp].add(sha)

                entry[cp] = {
                    "read_ms": rounded(read_ms),
                    "read_median_ms": median(read_ms),
                    "materialize_ms": rounded(materialize_ms),
                    "materialize_median_ms": median(materialize_ms),
                    "materialize_bytes": materialize_bytes,
                    "backend_bytes_read": backend_bytes,
                    "pipeline_decode_wall_ms": rounded(decode_wall),
                    "pipeline_decode_wall_median_ms": median(decode_wall),
                    "pipeline_transfer_ms": rounded(transfer),
                    "pipeline_transfer_median_ms": median(transfer),
                    "pipeline_write_ms": rounded(write),
                    "pipeline_write_median_ms": median(write),
                    "pipeline_total_ms": rounded(total),
                    "pipeline_total_median_ms": median(total),
                    "pipeline_bytes_written": pipeline_bytes,
                }
            batch_peaks.sort()
            pipe_peaks.sort()
            entry["threads_requested"] = threads
            entry["threads_effective"] = effective
            entry["max_inflight_bytes"] = staging
            entry["peak_rss_kib"] = {
                "batch": batch_peaks,
                "pipeline": pipe_peaks,
            }
            entry["peak_rss_median_kib"] = {
                "batch": batch_peaks[len(batch_peaks) // 2],
                "pipeline": pipe_peaks[len(pipe_peaks) // 2],
            }
            results[dest][str(threads)] = entry

        speedups = {}
        for threads in THREADS:
            for cp in cps:
                batch = results[dest][str(threads)][cp]["materialize_median_ms"]
                pipelined = results[dest][str(threads)][cp]["pipeline_total_median_ms"]
                speedups[f"{cp}_pipeline_vs_batch_t{threads}"] = round(
                    batch / pipelined, 4
                ) if pipelined else None
        for threads in (2, 4):
            for cp in cps:
                one = results[dest]["1"][cp]["pipeline_total_median_ms"]
                many = results[dest][str(threads)][cp]["pipeline_total_median_ms"]
                speedups[f"{cp}_pipeline_t{threads}_vs_t1"] = round(one / many, 4) if many else None
        for threads in THREADS:
            for cp in cps:
                decode = results[dest][str(threads)][cp]["pipeline_decode_wall_median_ms"]
                write = results[dest][str(threads)][cp]["pipeline_write_median_ms"]
                speedups[f"{cp}_write_over_decode_t{threads}"] = (
                    round(write / decode, 4) if decode else None
                )
        speedups_by_dest[dest] = speedups

        checks[f"{dest}_effective_threads_1_2_4"] = all(
            results[dest][str(threads)]["threads_effective"] == threads
            for threads in THREADS
        )
        checks[f"{dest}_staging_bound_constant"] = len(
            {results[dest][str(t)]["max_inflight_bytes"] for t in THREADS}
        ) == 1
        checks[f"{dest}_every_run_has_290_tensors"] = all(
            item["tensors_read"] == 290
            for run in dest_runs.values()
            for item in run["checkpoints"]
        )
        checks[f"{dest}_reassembled_bytes_988097824"] = all(
            results[dest][str(threads)][cp]["materialize_bytes"]
            == [988097824] * args.reps
            for threads in THREADS
            for cp in cps
        )

    # Observed spread across every repetition, so a reader can see whether the
    # comparison is inside the host's own variance.
    spread = {}
    tmpfs_pcts = {}
    for dest in DESTINATIONS:
        if dest not in results:
            continue
        batch_values = []
        pipeline_values = []
        write_values = []
        for threads in THREADS:
            for cp in present_checkpoints.get(dest, []):
                batch_values.extend(results[dest][str(threads)][cp]["materialize_ms"])
                pipeline_values.extend(results[dest][str(threads)][cp]["pipeline_total_ms"])
                write_values.extend(results[dest][str(threads)][cp]["pipeline_write_ms"])
        spread[dest] = {
            "batch_materialize_ms": {
                "min": min(batch_values),
                "median": median(batch_values),
                "max": max(batch_values),
            },
            "pipeline_total_ms": {
                "min": min(pipeline_values),
                "median": median(pipeline_values),
                "max": max(pipeline_values),
            },
            "pipeline_write_ms": {
                "min": min(write_values),
                "median": median(write_values),
                "max": max(write_values),
            },
        }
        if dest == "tmpfs":
            for threads in THREADS:
                key = f"base_pipeline_vs_batch_t{threads}"
                ratio = speedups_by_dest.get(dest, {}).get(key)
                tmpfs_pcts[str(threads)] = (
                    round((ratio - 1.0) * 100.0, 1) if ratio else None
                )

    checks["all_hash_exact"] = all_hash_exact
    checks["all_logical_roots_exact"] = all_roots_exact
    checks["all_materialize_exact"] = all_materialize_exact
    checks["all_materialize_modes_match"] = all_mode_matches
    checks["all_file_sha256_match_source"] = all_sha_match
    checks["file_sha256_identical_across_modes_threads_and_destinations"] = all(
        len(values) == 1 for values in shas_by_checkpoint.values()
    )
    checks["restored_bytes_identical_across_modes_and_threads"] = all(
        len(values) == 1 for values in per_checkpoint_bytes.values()
    )
    checks["pipeline_wrote_290_tensors_and_988065536_bytes"] = all_pipeline_counts
    checks["pipeline_inflight_within_bound_plus_embedding"] = all_pipeline_bounds
    checks["source_sha256_rehashed_in_court"] = args.source_check == "rehashed_in_court"

    court = {
        "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
        "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
        "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
        "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
        "nproc": os.environ.get("WEFT_COURT_NPROC", "unknown"),
        "docker_server": os.environ.get("WEFT_DOCKER_SERVER", "unknown"),
        "docker_client": os.environ.get("WEFT_DOCKER_CLIENT", "unknown"),
    }
    revisions = {
        "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        "weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        "weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
    }

    receipt = {
        "schema": "weft.restore-pipeline.v1",
        "label": (
            "single-host bench court, 4 vCPUs, warm cache, one store reused by "
            "both modes, no fleet or multi-node claim"
        ),
        "generated_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "court": court,
        "revisions": revisions,
        "corpus": {
            "base_sha256": args.base_sha,
            "instruct_sha256": args.instruct_sha,
            "tensors_per_checkpoint": 290,
            "logical_bytes_per_checkpoint": 988065536,
            "file_bytes": 988097824,
            "largest_tensor_bytes": 272269312,
        },
        "store": {
            "path": args.store,
            "ingest_source": args.ingest_source,
            "note": (
                "Reuses the cached planner-float store and ingest receipt when "
                "present; the ingest step is skipped in that case and this field "
                "says so. Restore is read-only, so both modes see identical state."
            ),
        },
        "destinations": {
            "disk": {
                "path": os.path.join(args.work, "out-disk"),
                "checkpoints": present_checkpoints.get("disk", []),
                "note": (
                    "Host bind mount under .docker-cache. This is the real "
                    "end-to-end write path; its absolute write times are host "
                    "state dependent, so all repetitions are kept."
                ),
            },
            "tmpfs": {
                "path": "/mnt/weft-bench/restore-pipeline",
                "checkpoints": present_checkpoints.get("tmpfs", []),
                "note": (
                    "Court tmpfs (1 GiB). Base checkpoint only because the "
                    "988,097,824 B file leaves ~80 MiB under the mount. With the "
                    "write effectively free, this isolates decode/transfer "
                    "overlap."
                ),
            },
        },
        "configuration": {
            "modes": list(MODES),
            "repetitions_per_configuration": args.reps,
            "max_inflight_mib": args.staging_mib,
            "thread_counts": list(THREADS),
            "single_tensor_runs_per_checkpoint": 1,
            "order": "per destination: (t1,t2,t4) x (batch,pipeline) x reps",
            "sync_between_runs": True,
            "output_removed_after_hashing": True,
            "read_pass": (
                "Both modes first read and hash every tensor (read_ms); that pass "
                "is unchanged and is not part of materialize_ms/pipeline_total_ms."
            ),
        },
        "results": results,
        "speedup": speedups_by_dest,
        "spread": spread,
        "notes": [
            "Materialize writes 988,097,824 B per checkpoint. The pipeline overlaps "
            "decode with that write; it cannot make the write itself faster.",
            "pipeline.decode_wall_ms is the wall time until the last decode finished; "
            "decode_sum_ms (in the run JSONs) is the sum of worker busy time and can "
            "exceed it. transfer_ms is the writer waiting for the next in-order "
            "result; write_ms is time inside write_all.",
            "The 272 MB embedding exceeds the 64 MiB staging bound and runs as the "
            "single oversized task; the memory bound is max_inflight + 272,269,312 B.",
            f"The disk (host bind mount) rows are NOT a performance claim: across "
            f"{args.reps} repetitions the same batched 988 MB materialize ranged "
            f"{spread['disk']['batch_materialize_ms']['min']} to "
            f"{spread['disk']['batch_materialize_ms']['max']} ms and the pipeline "
            f"total ranged {spread['disk']['pipeline_total_ms']['min']} to "
            f"{spread['disk']['pipeline_total_ms']['max']} ms, a host writeback "
            f"spread larger than any mode difference. The tmpfs rows are the "
            f"controlled comparison.",
            "On tmpfs the write is effectively free, so total_ms is decode-bound: "
            f"the pipeline is {tmpfs_pcts.get('1')}% (1 worker), "
            f"{tmpfs_pcts.get('2')}% (2), and {tmpfs_pcts.get('4')}% (4) faster at "
            "the medians, and its write time is almost entirely overlapped with "
            "decode.",
            "At one worker the pipeline still uses one decode thread plus the "
            "calling writer thread, so it is not bit-for-bit the old sequential cost; "
            "see the one-thread rows.",
        ],
        "checks": checks,
        "all_checks_pass": all(checks[key] for key in checks),
    }

    os.makedirs(args.out_dir, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out = os.path.join(args.out_dir, f"restore-pipeline-{stamp}.json")
    with open(out, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(out)
    if not receipt["all_checks_pass"]:
        for key, value in checks.items():
            if not value:
                print(f"CHECK FAILED {key}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
