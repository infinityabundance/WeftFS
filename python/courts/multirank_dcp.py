#!/usr/bin/env python3
"""T056 court: four real ranks publish one checkpoint; four ranks restore it.

Launched by ``torchrun --nproc_per_node=4 --nnodes=1`` with the gloo backend on
localhost. Each rank owns one contiguous slice of every tensor, writes its own
fragment (binary + manifest) to a shared host directory, and waits at a gloo
barrier. Rank 0 reassembles the fragments into one safetensors container and
publishes it with **one** WeftFS commit.

Load: each destination rank asks for only its own extents with the
rank-directed restore (T053) under 4->2, 4->1 and 4->8 target topologies, and
compares every byte with the deterministic full model every rank can rebuild.

Faults (rank killed at a chosen phase, then a clean retry):
``worker_pre`` (a worker dies before writing its fragment), ``coordinator_pre``
(rank 0 dies after all fragments, before publishing), ``during_publish`` (rank 0
kills the publishing CLI while it runs).

CPU only, one host, gloo over loopback. No GPU and no multi-host claim.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "weft.multirank-dcp.v1"
DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"
COURT = Path(__file__).resolve().parent / "training_goodput.py"


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_court():
    import importlib.util

    spec = importlib.util.spec_from_file_location("weft_training_goodput", COURT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def dist():
    import torch.distributed as dist_module

    return dist_module


def rank_env():
    return int(os.environ.get("RANK", "0")), int(os.environ.get("WORLD_SIZE", "1"))


def full_state(torch, court):
    torch.manual_seed(court.SEED)
    model = court.build_model(torch)
    return {
        name: value.detach().clone().contiguous()
        for name, value in model.state_dict().items()
    }


def slice_for(value, rank, world):
    if value.dim() == 0:
        return value
    length = value.shape[0]
    if length % world:
        raise SystemExit(
            "tensor shape %r is not divisible by %d ranks" % (tuple(value.shape), world)
        )
    per = length // world
    return value[rank * per : (rank + 1) * per]


def fragment_dir(work, tag):
    return Path(work) / ("fragments-" + tag)


def run_save(args):
    import numpy as np
    import torch

    dist_module = dist()
    dist_module.init_process_group("gloo")
    rank, world = rank_env()
    court = load_court()
    state = full_state(torch, court)
    fragments = fragment_dir(args.work, args.tag)
    if rank == 0:
        if fragments.exists():
            shutil.rmtree(fragments)
        fragments.mkdir(parents=True)
    dist_module.barrier()

    fault = args.fault
    if fault == "worker_pre" and rank == args.victim and rank != 0:
        _progress(args, {"event": "kill", "phase": fault, "rank": rank})
        os.kill(os.getpid(), signal.SIGKILL)

    payload = bytearray()
    manifest = {}
    for name, value in state.items():
        piece = slice_for(value, rank, world)
        raw = piece.numpy().tobytes()
        manifest[name] = {
            "dtype": str(value.dtype).replace("torch.", ""),
            "full_shape": list(value.shape),
            "shape": list(piece.shape),
            "offset": len(payload),
            "bytes": len(raw),
        }
        payload.extend(raw)
    (fragments / ("rank%d.bin" % rank)).write_bytes(payload)
    (fragments / ("rank%d.json" % rank)).write_text(json.dumps(manifest))
    (fragments / ("rank%d.done" % rank)).write_text("ok")

    if rank != 0:
        # Workers are done contributing. No collective follows, so a killed
        # worker can never hang a barrier; rank 0 polls the completion markers.
        _progress(args, {"event": "fragment_written", "rank": rank})
        dist_module.destroy_process_group()
        return 0

    if fault == "coordinator_pre":
        _progress(args, {"event": "kill", "phase": fault, "rank": rank})
        os.kill(os.getpid(), signal.SIGKILL)

    deadline = time.time() + args.wait_secs
    while time.time() < deadline:
        if all((fragments / ("rank%d.done" % index)).is_file() for index in range(world)):
            break
        time.sleep(0.02)

    result = {"phase": "save", "tag": args.tag, "fault": fault, "ranks": world}
    if rank == 0:  # the only committer (every other rank returned above)
        missing = [
            index
            for index in range(world)
            if not (fragments / ("rank%d.done" % index)).is_file()
        ]
        if missing:
            result.update(
                {
                    "published": False,
                    "aborted": True,
                    "missing_ranks": missing,
                    "reason": "not every rank contributed its fragment",
                }
            )
        else:
            array = {}
            for index in range(world):
                manifest = json.loads((fragments / ("rank%d.json" % index)).read_text())
                raw = (fragments / ("rank%d.bin" % index)).read_bytes()
                for name, meta in manifest.items():
                    dtype = np.dtype(meta["dtype"])
                    piece = np.frombuffer(
                        raw, dtype=dtype, count=meta["bytes"] // dtype.itemsize,
                        offset=meta["offset"],
                    ).reshape(meta["shape"])
                    array.setdefault(name, {})[index] = piece
            tensors = {}
            for name, pieces in array.items():
                ordered = [pieces[index] for index in range(world)]
                tensors[name] = np.concatenate(ordered, axis=0)
            import numpy as np

            container = Path(args.work) / ("%s.safetensors" % args.tag)
            _save_safetensors(container, tensors)

            store = Path(args.store)
            if store.exists() and args.clean_store:
                shutil.rmtree(store)
            if not (store / "FORMAT").is_file():
                subprocess.run(
                    [args.cli, "--store", str(store), "init", "--chunk-size", "65536",
                     "--checkpoint-interval", "256", "--gc-grace-secs", "0"],
                    check=True,
                    capture_output=True,
                )
            if fault == "during_publish":
                proc = subprocess.Popen(
                    [args.cli, "--store", str(store), "--json", "checkpoint", "write",
                     "--file", os.fspath(container), "--name", args.tag,
                     "--shards", str(world), "--axis", "0"],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
                time.sleep(max(0.0, args.kill_delay_ms) / 1000.0)
                proc.kill()
                proc.wait(timeout=60)
                _progress(args, {"event": "kill", "phase": fault, "rank": 0})
                # The committing process is dead; the store must expose either
                # nothing or the previous whole state.
                listed = subprocess.run(
                    [args.cli, "--store", str(store), "--json", "checkpoint", "list"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                names = []
                if listed.returncode == 0:
                    names = [row["name"] for row in json.loads(listed.stdout).get("checkpoints", [])]
                result.update(
                    {
                        "published": args.tag in names,
                        "visible_checkpoints": names,
                        "published_cli_returncode": proc.returncode,
                    }
                )
            else:
                write_started = time.perf_counter()
                proc = subprocess.run(
                    [args.cli, "--store", str(store), "--json", "checkpoint", "write",
                     "--file", os.fspath(container), "--name", args.tag,
                     "--shards", str(world), "--axis", "0"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                write_ms = (time.perf_counter() - write_started) * 1000.0
                payload_result = json.loads(proc.stdout) if proc.stdout.strip() else {}
                verify = subprocess.run(
                    [args.cli, "--store", str(store), "--json", "checkpoint", "verify",
                     "--name", args.tag],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                result.update(
                    {
                        "published": proc.returncode == 0,
                        "write_ms": round(write_ms, 3),
                        "source_bytes": container.stat().st_size,
                        "shard_objects": payload_result.get("shard_objects"),
                        "verified": verify.returncode == 0,
                        "verify_stdout": verify.stdout.strip()[:400],
                    }
                )
        (Path(args.work) / ("result-save-%s.json" % args.tag)).write_text(
            json.dumps(result, indent=2)
        )
        _progress(args, {"event": "save_result", **result})
    dist_module.destroy_process_group()
    return 0


def run_load(args):
    import numpy as np
    import torch

    dist_module = dist()
    dist_module.init_process_group("gloo")
    rank, world = rank_env()
    court = load_court()
    state = full_state(torch, court)
    store = Path(args.store)
    tag = args.tag
    rows = {}
    for target_shards in [int(value) for value in args.topologies.split(",")]:
        for target_rank in range(target_shards):
            if target_rank % world != rank:
                continue
            out_dir = Path(args.work) / ("load-%s-%d-%d" % (tag, target_shards, target_rank))
            if out_dir.exists():
                shutil.rmtree(out_dir)
            proc = subprocess.run(
                [args.cli, "--store", str(store), "--json", "checkpoint", "restore",
                 "--name", tag, "--topology", str(target_shards), "--axis", "0",
                 "--rank", str(target_rank), "--rank-out", os.fspath(out_dir)],
                capture_output=True,
                text=True,
                check=False,
            )
            if proc.returncode != 0:
                rows["%d:%d" % (target_shards, target_rank)] = {
                    "topology": target_shards,
                    "rank": target_rank,
                    "error": proc.stderr.strip()[:600],
                }
                continue
            payload = json.loads(proc.stdout)
            mismatched = []
            for row in payload["tensors"]:
                file_name = row["file"]
                data = (out_dir / file_name).read_bytes()
                meta = _tensor_meta(state, row["name"])
                expected = _region(
                    meta, np.dtype(meta["dtype"]), row["start"], row["end"]
                )
                if data != expected:
                    mismatched.append(row["name"])
            rows["%d:%d" % (target_shards, target_rank)] = {
                "topology": target_shards,
                "rank": target_rank,
                "tensors": len(payload["tensors"]),
                "needed_bytes": payload["needed_bytes"],
                "backend_bytes_read": payload["backend_bytes_read"],
                "representation_bytes": payload["representation_bytes"],
                "read_amplification": payload["read_amplification"],
                "fetched_set_matches_plan": payload["fetched_set_matches_plan"],
                "mismatched_tensors": mismatched,
                "exact": not mismatched,
            }
    dist_module.barrier()
    (Path(args.work) / ("result-load-%s-rank%d.json" % (tag, rank))).write_text(
        json.dumps({"phase": "load", "rank": rank, "rows": rows}, indent=2)
    )
    _progress(args, {"event": "load_result", "rank": rank, "rows": len(rows)})
    dist_module.barrier()
    dist_module.destroy_process_group()
    return 0


def _save_safetensors(path, tensors):
    """Minimal safetensors writer (no optional dependency)."""
    import json
    import struct

    names = sorted(tensors)
    dtype_names = {
        "float32": "F32",
        "float64": "F64",
        "float16": "F16",
        "bfloat16": "BF16",
        "int64": "I64",
        "int32": "I32",
        "int16": "I16",
        "int8": "I8",
        "uint8": "U8",
        "bool": "BOOL",
    }
    header = {}
    offset = 0
    payload = bytearray()
    for name in names:
        value = tensors[name]
        raw = value.tobytes()
        header[name] = {
            "dtype": dtype_names[str(value.dtype)],
            "shape": list(value.shape),
            "data_offsets": [offset, offset + len(raw)],
        }
        offset += len(raw)
        payload.extend(raw)
    encoded = json.dumps(header, separators=(",", ":")).encode("utf-8")
    padding = (-len(encoded)) % 8
    encoded += b" " * padding
    Path(path).write_bytes(struct.pack("<Q", len(encoded)) + encoded + bytes(payload))


def _tensor_meta(state, name):
    value = state[name]
    return {
        "dtype": str(value.dtype).replace("torch.", ""),
        "shape": tuple(value.shape),
        "value": value,
    }


def _region(meta, dtype, start, end):
    import numpy as np

    value = meta["value"].numpy()
    start = [int(item) for item in start]
    end = [int(item) for item in end]
    if value.ndim == 1:
        return value[start[0] : end[0]].tobytes()
    if value.ndim == 2:
        return value[start[0] : end[0], start[1] : end[1]].tobytes()
    raise SystemExit("unsupported tensor rank %d" % value.ndim)


def _progress(args, row):
    if rank_env()[0] != 0:
        return
    path = Path(args.work) / ("trace-%s.ndjson" % args.tag)
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(row) + "\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("save", "load"):
        child = sub.add_parser(name)
        child.add_argument("--work", required=True)
        child.add_argument("--store", required=True)
        child.add_argument("--cli", default=DEFAULT_CLI)
        child.add_argument("--tag", required=True)
        child.add_argument("--clean-store", action="store_true")
        if name == "save":
            child.add_argument("--fault", default="none")
            child.add_argument("--victim", type=int, default=1)
            child.add_argument("--kill-delay-ms", type=float, default=4.0)
            child.add_argument("--wait-secs", type=float, default=60.0)
        else:
            child.add_argument("--topologies", default="2,1,8")
    args = parser.parse_args(argv)
    if args.command == "save":
        return run_save(args)
    if args.command == "load":
        return run_load(args)
    return 2


if __name__ == "__main__":
    sys.exit(main())
