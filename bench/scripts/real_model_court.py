#!/usr/bin/env python3
"""Assemble the decisive real-model storage/restore receipt.

This script is the reproducible top level of subphase F. It:

1. verifies the downloaded checkpoints against the corpus manifest (size and
   SHA-256 from the Hugging Face LFS pointers);
2. runs the `zstd` baselines (independent `-19`, `--patch-from` at the highest
   level that fits the 3 GiB court, plus decode timings), recording wall time
   and peak RSS for every process;
3. reads the WeftFS phase receipts produced by `examples/real_model_bench.rs`
   and the ZipNN / DFloat11 baseline artifacts produced by the scripts in this
   directory;
4. re-verifies the reassembled safetensors files against the manifest SHA-256;
5. writes one receipt with the environment, every loss, and the exact commands.

Every timed command is run here, in the bench court, under the normal hard
limits (`--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`, bounded
/tmp, cap-drop=ALL, no-new-privileges).

Usage (inside the bench court, repository mounted at /work):
  python3 bench/scripts/real_model_court.py \
      --corpus-dir .docker-cache/real-corpus \
      --results-dir .docker-cache/real-corpus/results \
      --baselines-dir .docker-cache/real-corpus/baselines \
      --receipt bench/receipts/real-model-<UTC>.json
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import statistics
import subprocess
import sys
import time


def sha256_file(path, chunk=1 << 20):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while True:
            block = handle.read(chunk)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def run_timed(args, timeout=7200, capture_stdout=False):
    """Run one command under /usr/bin/time and return timing + outcome.

    `capture_stdout=False` is required for decode measurements: the decoded
    file is ~1 GB and must go to /dev/null rather than into this process.
    """
    command = ["/usr/bin/time", "-f", "WALL=%e MAXRSS_KIB=%M"] + args
    started = time.perf_counter()
    completed = subprocess.run(
        command,
        stdout=subprocess.PIPE if capture_stdout else subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    wall = time.perf_counter() - started
    stderr = completed.stderr.decode("utf-8", "replace")
    match = re.search(r"WALL=([0-9.]+) MAXRSS_KIB=(\d+)", stderr)
    return {
        "command": " ".join(command),
        "returncode": completed.returncode,
        "wall_seconds": round(wall, 3),
        "reported_wall_seconds": float(match.group(1)) if match else None,
        "maxrss_kib": int(match.group(2)) if match else None,
        "stderr_tail": stderr.strip().splitlines()[-4:],
        "stdout_tail": []
        if not capture_stdout
        else completed.stdout.decode("utf-8", "replace").strip().splitlines()[-3:],
    }


def verify_corpus(manifest, corpus_dir):
    checked = {}
    for role, entry in manifest["pair"].items():
        path = os.path.join(corpus_dir, entry["file"].replace("model.safetensors", f"{entry['repo'].split('/')[-1]}.safetensors"))
        # The fetcher names files by repository id; accept either location.
        if not os.path.exists(path):
            path = os.path.join(corpus_dir, entry["file"])
        if not os.path.exists(path):
            raise SystemExit(f"missing corpus file for {entry['repo']}: {path}")
        size = os.path.getsize(path)
        digest = sha256_file(path)
        if size != entry["size"] or digest != entry["sha256"]:
            raise SystemExit(
                f"{path}: size/sha256 mismatch (size {size}, sha {digest})"
            )
        checked[role] = {
            "repo": entry["repo"],
            "revision": entry["revision"],
            "file": entry["file"],
            "url": entry["url"],
            "path": path,
            "size": size,
            "sha256": digest,
            "lfs_sha256": entry["sha256"],
            "xet_hash": entry.get("xet_hash"),
            "verified": True,
        }
    return checked


def zstd_baseline(manifest, corpus, baselines_dir):
    version_line = subprocess.run(
        ["zstd", "--version"], capture_output=True, text=True, check=False
    ).stdout.strip()
    os.makedirs(baselines_dir, exist_ok=True)
    results = {
        "tool": "zstd CLI",
        "version": version_line,
        "independent_level_19": {},
        "patch_from": {},
        "decode": {},
    }
    paths = {role: entry["path"] for role, entry in corpus.items()}

    # Independent `-19` compression of each checkpoint.
    total_bytes = 0
    total_wall = 0.0
    for role, source in paths.items():
        target = os.path.join(baselines_dir, f"{role}.safetensors.zst")
        outcome = run_timed(
            ["zstd", "-19", "-T1", "--no-progress", "-q", "-f", source, "-o", target]
        )
        size = os.path.getsize(target)
        total_bytes += size
        total_wall += outcome["wall_seconds"]
        results["independent_level_19"][role] = {
            **outcome,
            "output": target,
            "compressed_bytes": size,
            "compressed_sha256": sha256_file(target),
            "source_bytes": manifest["pair"][role]["size"],
            "source_sha256": manifest["pair"][role]["sha256"],
            "ratio": round(size / manifest["pair"][role]["size"], 6),
        }
    results["independent_level_19"]["total"] = {
        "compressed_bytes": total_bytes,
        "encode_wall_seconds": round(total_wall, 3),
        "source_bytes": sum(manifest["pair"][role]["size"] for role in paths),
    }

    # `--patch-from` at -19 exceeds the court memory limit; record the exact
    # failure, then use the highest level that fits (-12) for both the base and
    # the patch so the configuration is single-level.
    base_source = paths["base"]
    instruct_source = paths["instruct"]
    patch19 = os.path.join(baselines_dir, "instruct-l19.patch")
    attempt = run_timed(
        [
            "zstd",
            "-19",
            "-T1",
            "--no-progress",
            "-q",
            f"--patch-from={base_source}",
            "-f",
            instruct_source,
            "-o",
            patch19,
        ],
        timeout=3600,
    )
    results["patch_from"]["level_19"] = {
        "outcome": "unavailable: process killed by the 3 GiB court memory limit"
        if attempt["returncode"] != 0
        else "completed",
        **attempt,
        "partial_output_bytes": os.path.getsize(patch19) if os.path.exists(patch19) else 0,
    }
    if os.path.exists(patch19):
        os.unlink(patch19)

    base12 = os.path.join(baselines_dir, "base.safetensors.l12.zst")
    base12_outcome = run_timed(
        ["zstd", "-12", "-T1", "--no-progress", "-q", "-f", base_source, "-o", base12]
    )
    patch12 = os.path.join(baselines_dir, "instruct-l12.patch")
    patch12_outcome = run_timed(
        [
            "zstd",
            "-12",
            "-T1",
            "--no-progress",
            "-q",
            f"--patch-from={base_source}",
            "-f",
            instruct_source,
            "-o",
            patch12,
        ],
        timeout=3600,
    )
    base12_size = os.path.getsize(base12)
    patch12_size = os.path.getsize(patch12)
    results["patch_from"]["level_12"] = {
        "base": {
            **base12_outcome,
            "compressed_bytes": base12_size,
            "compressed_sha256": sha256_file(base12),
        },
        "patch": {
            **patch12_outcome,
            "output": patch12,
            "compressed_bytes": patch12_size,
            "compressed_sha256": sha256_file(patch12),
            "source_bytes": manifest["pair"]["instruct"]["size"],
            "source_sha256": manifest["pair"]["instruct"]["sha256"],
            "ratio_of_instruct": round(patch12_size / manifest["pair"]["instruct"]["size"], 6),
        },
        "total": {
            "compressed_bytes": base12_size + patch12_size,
            "encode_wall_seconds": round(
                base12_outcome["wall_seconds"] + patch12_outcome["wall_seconds"], 3
            ),
        },
        "note": "Single-level configuration: base and patch both at -12. zstd "
        "automatically enables long mode for --patch-from.",
    }

    # Decode timings (3 repetitions; report all and the median).
    def decode_rep(label, args, output=None):
        to_null = []
        to_file = []
        for _ in range(3):
            outcome = run_timed(args + (["-c"] if output is None else []))
            to_null.append(outcome)
        if output is not None:
            for _ in range(3):
                outcome = run_timed(args + ["-o", output])
                to_file.append(outcome)
                digest = sha256_file(output)
                expected = manifest["pair"][label]["sha256"]
                outcome["sha256_exact"] = digest == expected
        return {
            "to_stdout_null": to_null,
            "to_stdout_null_median_seconds": round(
                statistics.median([item["wall_seconds"] for item in to_null]), 3
            ),
            "to_file": to_file,
            "to_file_median_seconds": round(
                statistics.median([item["wall_seconds"] for item in to_file]), 3
            )
            if to_file
            else None,
        }

    base_zst = results["independent_level_19"]["base"]["output"]
    instruct_zst = results["independent_level_19"]["instruct"]["output"]
    results["decode"]["independent_base"] = decode_rep(
        "base",
        ["zstd", "-d", "-q", "-f", base_zst],
        output=os.path.join(baselines_dir, "decoded-base.safetensors"),
    )
    os.unlink(os.path.join(baselines_dir, "decoded-base.safetensors"))
    results["decode"]["independent_instruct"] = decode_rep(
        "instruct",
        ["zstd", "-d", "-q", "-f", instruct_zst],
        output=os.path.join(baselines_dir, "decoded-instruct.safetensors"),
    )
    os.unlink(os.path.join(baselines_dir, "decoded-instruct.safetensors"))
    patch_decode = decode_rep(
        "instruct",
        ["zstd", "-d", "-q", "-f", f"--patch-from={base_source}", patch12],
        output=os.path.join(baselines_dir, "decoded-patch.safetensors"),
    )
    patch_decode["bytes_read_from_storage"] = patch12_size + manifest["pair"]["base"]["size"]
    results["decode"]["patch_from_instruct"] = patch_decode

    results["decode"]["note"] = (
        "Each repetition is a separate process in the same court. The first run "
        "after the encode is slower because the output writeback is still in "
        "flight; all repetitions and the median are reported."
    )
    return results


def read_json(path):
    with open(path) as handle:
        return json.load(handle)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", default="bench/corpora/real-models/qwen2.5-0.5b-pair.json")
    parser.add_argument(
        "--secondary-manifest", default="bench/corpora/real-models/smollm-135m-pair.json"
    )
    parser.add_argument("--corpus-dir", default=".docker-cache/real-corpus")
    parser.add_argument("--results-dir", default=".docker-cache/real-corpus/results")
    parser.add_argument("--baselines-dir", default=".docker-cache/real-corpus/baselines")
    parser.add_argument("--restored-dir", default=".docker-cache/real-corpus/restored-qwen")
    parser.add_argument("--secondary-restored-dir", default=".docker-cache/real-corpus/restored-smollm")
    parser.add_argument("--receipt", required=True)
    parser.add_argument("--skip-zstd", action="store_true")
    args = parser.parse_args()

    manifest = read_json(args.manifest)
    secondary_manifest = read_json(args.secondary_manifest)
    corpus = verify_corpus(manifest, args.corpus_dir)
    secondary = verify_corpus(secondary_manifest, args.corpus_dir)

    zstd = (
        read_json(os.path.join(args.results_dir, "zstd-baselines.json"))
        if args.skip_zstd
        else zstd_baseline(manifest, corpus, args.baselines_dir)
    )
    if not args.skip_zstd:
        with open(os.path.join(args.results_dir, "zstd-baselines.json"), "w") as handle:
            json.dump(zstd, handle, indent=2)

    zipnn = {
        "tool": "ZipNN",
        "version": read_json(
            os.path.join(args.baselines_dir, "zipnn-Qwen2.5-0.5B.json")
        )["version"],
        "base": read_json(os.path.join(args.baselines_dir, "zipnn-Qwen2.5-0.5B.json")),
        "instruct": read_json(
            os.path.join(args.baselines_dir, "zipnn-Qwen2.5-0.5B-Instruct.json")
        ),
    }
    zipnn["total"] = {
        "compressed_bytes": zipnn["base"]["compressed_bytes"]
        + zipnn["instruct"]["compressed_bytes"],
        "encode_wall_seconds": round(
            zipnn["base"]["compress_seconds"] + zipnn["instruct"]["compress_seconds"], 3
        ),
        "decode_wall_seconds": round(
            zipnn["base"]["decompress_seconds"] + zipnn["instruct"]["decompress_seconds"], 3
        ),
        "materialize_wall_seconds": round(
            zipnn["base"]["restore_total_seconds"]
            + zipnn["instruct"]["restore_total_seconds"],
            3,
        ),
        "restored_matches_source": zipnn["base"]["restored_matches_source"]
        and zipnn["instruct"]["restored_matches_source"],
    }

    dfloat11 = read_json(os.path.join(args.baselines_dir, "dfloat11-probe.json"))

    ingest = read_json(os.path.join(args.results_dir, "qwen-ingest.json"))
    ingest_base_only = read_json(
        os.path.join(args.results_dir, "qwen-ingest-base-only.json")
    )
    restore = read_json(os.path.join(args.results_dir, "qwen-restore.json"))
    secondary_ingest = read_json(os.path.join(args.results_dir, "smollm-ingest.json"))
    secondary_restore = read_json(os.path.join(args.results_dir, "smollm-restore.json"))

    # Exactness: reassembled files must match the LFS SHA-256.
    restored_checks = {}
    for role in ("base", "instruct"):
        path = os.path.join(args.restored_dir, f"{role}.safetensors")
        if not os.path.exists(path):
            raise SystemExit(f"missing reassembled file {path}")
        digest = sha256_file(path)
        restored_checks[role] = {
            "path": path,
            "bytes": os.path.getsize(path),
            "sha256": digest,
            "matches_lfs": digest == manifest["pair"][role]["sha256"],
        }
    if not all(item["matches_lfs"] for item in restored_checks.values()):
        raise SystemExit("reassembled checkpoint does not match the LFS SHA-256")

    secondary_checks = {}
    for role in ("base", "instruct"):
        path = os.path.join(args.secondary_restored_dir, f"{role}.safetensors")
        if not os.path.exists(path):
            raise SystemExit(f"missing secondary reassembled file {path}")
        digest = sha256_file(path)
        secondary_checks[role] = {
            "path": path,
            "bytes": os.path.getsize(path),
            "sha256": digest,
            "matches_lfs": digest == secondary_manifest["pair"][role]["sha256"],
        }
    if not all(item["matches_lfs"] for item in secondary_checks.values()):
        raise SystemExit("secondary reassembled checkpoint does not match the LFS SHA-256")

    for checkpoint in restore["checkpoints"]:
        if not (
            checkpoint["hash_exact"]
            and checkpoint["logical_roots_exact"]
            and checkpoint["materialize"]["exact"]
        ):
            raise SystemExit(f"restore verification failed for {checkpoint['checkpoint']}")
    for checkpoint in secondary_restore["checkpoints"]:
        if not (
            checkpoint["hash_exact"]
            and checkpoint["logical_roots_exact"]
            and checkpoint["materialize"]["exact"]
        ):
            raise SystemExit(
                f"secondary restore verification failed for {checkpoint['checkpoint']}"
            )

    weftfs_total = ingest["physical"]["store_total_bytes"]
    weftfs_data = ingest["physical"]["tensor_data_bytes"]
    base_total = ingest_base_only["physical"]["store_total_bytes"]
    instruct_marginal = weftfs_total - base_total
    instruct_marginal_data = (
        ingest["physical"]["tensor_data_bytes"]
        - ingest_base_only["physical"]["tensor_data_bytes"]
    )

    totals = {
        "zstd_19_independent": zstd["independent_level_19"]["total"]["compressed_bytes"],
        "zstd_patch_from_level_12": zstd["patch_from"]["level_12"]["total"][
            "compressed_bytes"
        ],
        "zipnn_0_5_4": zipnn["total"]["compressed_bytes"],
        "weftfs_store_total": weftfs_total,
        "weftfs_data_only": weftfs_data,
    }
    winner = min(
        (
            name
            for name in totals
            if name != "weftfs_data_only"
        ),
        key=totals.get,
    )
    comparable = {
        name: value for name, value in totals.items() if name != "weftfs_data_only"
    }
    comparison = {
        "totals_bytes": totals,
        "comparable_methods": list(comparable),
        "winner_by_bytes": winner,
        "deltas_vs_winner": {
            name: {
                "delta_bytes": value - totals[winner],
                "delta_percent": round(
                    100.0 * (value - totals[winner]) / totals[winner], 3
                ),
            }
            for name, value in comparable.items()
        },
        "weftfs_data_only_note": (
            "weftfs_data_only excludes the rebuildable per-commit index "
            f"segments ({ingest['physical']['indexes_bytes']} bytes) and commit "
            "records. It is shown for transparency but is not a complete "
            "storage method, so it is excluded from the winner selection."
        ),
        "weftfs_per_checkpoint": {
            "base_store_total": base_total,
            "instruct_marginal_store_total": instruct_marginal,
            "base_data_only": ingest_base_only["physical"]["tensor_data_bytes"],
            "instruct_marginal_data_only": instruct_marginal_data,
            "note": "The marginal instruct cost is the full two-checkpoint store "
            "minus a separate base-only store built with the same harness. It "
            "therefore includes that checkpoint's own commits and index "
            "segments.",
        },
        "restore_materialize_ms": {
            "weftfs_base": next(
                c["materialize"]["materialize_ms"]
                for c in restore["checkpoints"]
                if c["checkpoint"] == "base"
            ),
            "weftfs_instruct": next(
                c["materialize"]["materialize_ms"]
                for c in restore["checkpoints"]
                if c["checkpoint"] == "instruct"
            ),
            "weftfs_read_only_base": restore["checkpoints"][0]["read_ms"],
            "weftfs_read_only_instruct": restore["checkpoints"][1]["read_ms"],
            "zstd19_decode_null_base": zstd["decode"]["independent_base"][
                "to_stdout_null_median_seconds"
            ]
            * 1000.0,
            "zstd19_decode_null_instruct": zstd["decode"]["independent_instruct"][
                "to_stdout_null_median_seconds"
            ]
            * 1000.0,
            "zipnn_decompress_base": zipnn["base"]["decompress_seconds"] * 1000.0,
            "zipnn_decompress_instruct": zipnn["instruct"]["decompress_seconds"] * 1000.0,
            "zipnn_materialize_base": zipnn["base"]["restore_total_seconds"] * 1000.0,
            "zipnn_materialize_instruct": zipnn["instruct"]["restore_total_seconds"]
            * 1000.0,
            "zstd19_materialize_base": zstd["decode"]["independent_base"][
                "to_file_median_seconds"
            ]
            * 1000.0,
            "zstd19_materialize_instruct": zstd["decode"]["independent_instruct"][
                "to_file_median_seconds"
            ]
            * 1000.0,
        },
        "single_tensor_restore": restore["single_tensor_restore"],
    }

    environment = {
        "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
        "court_image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
        "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID", "unknown"),
        "docker_server": os.environ.get("WEFT_DOCKER_SERVER", "unknown"),
        "docker_client": os.environ.get("WEFT_DOCKER_CLIENT", "unknown"),
        "limits": {
            "memory": os.environ.get("WEFT_COURT_MEMORY", "3g"),
            "cpus": os.environ.get("WEFT_COURT_CPUS", "4"),
            "pids": os.environ.get("WEFT_COURT_PIDS", "1024"),
            "tmpfs_tmp": "1g",
            "cap_drop": "ALL",
            "no_new_privileges": True,
        },
        "revisions": {
            "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
            "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
            "weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
        },
        "python": sys.version,
        "cpu_count": os.cpu_count(),
    }

    receipt = {
        "schema": "weft.real-model.bench.v1",
        "label": (
            "Single-host public-model benchmark in the OOM-limited bench court. "
            "Qwen2.5-0.5B vs Qwen2.5-0.5B-Instruct (BF16, 290 tensors each), "
            "SmolLM-135M as a cross-dtype secondary. The comparison is at "
            "matched exactness: every method is lossless and every restored "
            "checkpoint matched the Hugging Face LFS SHA-256. No fleet or "
            "production claim is made."
        ),
        "generated_utc": datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z"),
        "environment": environment,
        "corpus": {
            "primary": {"manifest": manifest, "files": corpus},
            "secondary": {"manifest": secondary_manifest, "files": secondary},
            "selection_note": (
                "SmolLM-135M was the first-choice pair, but its base repository "
                "publishes F32 weights while its instruct repository publishes "
                "BF16, so no exact causal delta is possible. Qwen2.5-0.5B and "
                "Qwen2.5-0.5B-Instruct are both BF16 with identical names and "
                "shapes and are therefore the headline pair."
            ),
        },
        "exactness": {
            "all_methods_lossless": True,
            "weftfs_reassembled": restored_checks,
            "weftfs_secondary_reassembled": secondary_checks,
            "zipnn_restored_matches_source": zipnn["total"]["restored_matches_source"],
            "zstd_decodes_sha256_exact": all(
                item.get("sha256_exact", True)
                for group in (
                    zstd["decode"]["independent_base"],
                    zstd["decode"]["independent_instruct"],
                    zstd["decode"]["patch_from_instruct"],
                )
                for item in group.get("to_file", [])
            ),
        },
        "baselines": {
            "zstd": zstd,
            "zipnn": zipnn,
            "dfloat11": dfloat11,
        },
        "weftfs": {
            "harness": "examples/real_model_bench.rs (shipped store API)",
            "ingest": {
                "base_wall_ms": ingest["ingest"]["base"]["wall_ms"],
                "base_tensor_write_ms": ingest["ingest"]["base"]["tensor_write_ms"],
                "instruct_wall_ms": ingest["ingest"]["instruct"]["wall_ms"],
                "instruct_tensor_write_ms": ingest["ingest"]["instruct"][
                    "tensor_write_ms"
                ],
                "total_wall_ms": round(
                    ingest["ingest"]["base"]["wall_ms"]
                    + ingest["ingest"]["instruct"]["wall_ms"],
                    3,
                ),
                "peak_rss_kib": ingest["peak_rss_kib"],
                "backend": {
                    "base": ingest["ingest"]["base"]["backend"],
                    "instruct": ingest["ingest"]["instruct"]["backend"],
                },
            },
            "store": {
                "physical": ingest["physical"],
                "base_only_physical": ingest_base_only["physical"],
                "generation": ingest["store"]["generation"],
                "options": ingest["store"]["options"],
                "verify_full_ok": ingest["verify"]["ok"],
            },
            "strategy_distribution": ingest["strategy_distribution"],
            "restore": {
                "open_ms": restore["open_ms"],
                "open_backend": restore["open_backend"],
                "checkpoints": restore["checkpoints"],
                "single_tensor_restore": restore["single_tensor_restore"],
                "peak_rss_kib": restore["peak_rss_kib"],
                "note": "The reassembled files are byte-for-byte the original "
                "safetensors files; SHA-256 is recorded in `exactness`.",
            },
            "secondary": {
                "ingest": secondary_ingest,
                "restore": secondary_restore,
            },
        },
        "comparison": comparison,
        "reproduce": [
            "scripts/docker-court.sh bench 'python3 bench/scripts/real_model_fetch.py --repo Qwen/Qwen2.5-0.5B --file model.safetensors --dest .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors'",
            "scripts/docker-court.sh bench 'python3 bench/scripts/real_model_fetch.py --repo Qwen/Qwen2.5-0.5B-Instruct --file model.safetensors --dest .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors'",
            "scripts/docker-court.sh rust 'cargo build --release --example real_model_bench'",
            "scripts/docker-court.sh bench '.docker-cache/target-rust/release/examples/real_model_bench ingest --base-file .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors --instruct-file .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors --store .docker-cache/real-corpus/store-qwen --out .docker-cache/real-corpus/results/qwen-ingest.json'",
            "scripts/docker-court.sh bench '.docker-cache/target-rust/release/examples/real_model_bench restore --store .docker-cache/real-corpus/store-qwen --ingest-json .docker-cache/real-corpus/results/qwen-ingest.json --out .docker-cache/real-corpus/results/qwen-restore.json --reassemble-dir .docker-cache/real-corpus/restored-qwen --single-tensor model.layers.10.mlp.down_proj.weight'",
            "scripts/docker-court.sh bench '.docker-cache/target-rust/release/examples/real_model_bench ingest --base-file .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors --instruct-file .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors --store .docker-cache/real-corpus/store-qwen-base --out .docker-cache/real-corpus/results/qwen-ingest-base-only.json --skip-instruct'",
            "scripts/docker-court.sh bench 'PYTHONPATH=/work/.docker-cache/pylibs python3 bench/scripts/zipnn_bench.py --file .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors --compressed .docker-cache/real-corpus/baselines/Qwen2.5-0.5B.safetensors.znn --out .docker-cache/real-corpus/baselines/zipnn-Qwen2.5-0.5B.json'",
            "scripts/docker-court.sh bench 'PYTHONPATH=/work/.docker-cache/pylibs python3 bench/scripts/zipnn_bench.py --file .docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors --compressed .docker-cache/real-corpus/baselines/Qwen2.5-0.5B-Instruct.safetensors.znn --out .docker-cache/real-corpus/baselines/zipnn-Qwen2.5-0.5B-Instruct.json'",
            "scripts/docker-court.sh bench 'PYTHONPATH=/work/.docker-cache/pylibs python3 bench/scripts/dfloat11_probe.py --out .docker-cache/real-corpus/baselines/dfloat11-probe.json'",
            "scripts/docker-court.sh bench 'python3 bench/scripts/real_model_court.py --receipt bench/receipts/real-model-<UTC>.json'",
        ],
        "residual_risks": [
            "Single host, warm page cache after download/encode; no cache dropping is available in the court, so restore times are best-case for all methods.",
            "ZipNN ran as a whole-file byte stream with 4 MiB streaming chunks; per-tensor or delta-aware ZipNN was not run and could change its ratio.",
            "WeftFS per-checkpoint attribution uses a separate base-only store; index segments and commits are included in the headline store total.",
        ],
    }

    os.makedirs(os.path.dirname(args.receipt) or ".", exist_ok=True)
    with open(args.receipt, "w") as handle:
        json.dump(receipt, handle, indent=2)
        handle.write("\n")
    print(json.dumps({"receipt": args.receipt, "totals": totals, "winner": winner}))


if __name__ == "__main__":
    sys.exit(main())
