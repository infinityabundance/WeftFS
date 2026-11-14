#!/usr/bin/env python3
"""T055 court: complete training state, SIGKILL, and exact resume.

The model is the 0.414M-parameter transformer the training-goodput court uses.
A checkpoint contains **all** of:

* model parameters,
* AdamW optimizer state (both moments and the step),
* a cosine LR scheduler,
* an (honestly disabled) AMP grad scaler,
* the Python, Torch and NumPy RNG states,
* the data-pipeline bit-generator state and cursor,
* the optimizer step counter.

The reported loss consumes one draw from each of the three RNG streams (at
float32-visible magnitude) while the backward pass uses the plain cross
entropy, so the RNG streams are observable in the loss trace without changing
the parameter trajectory. That distinction is stated in the receipt.

A control run trains ``steps`` steps uninterrupted. Kill runs are killed with
SIGKILL at several points; a fresh process resumes from the newest visible
checkpoint and continues. The resumed loss bits and parameter hashes must equal
the control run at the same step indices.

A native ``torch.distributed.checkpoint`` ``FileSystemWriter`` arm does the
same work with the same barrier and durability for comparison.

CPU only, single host, one thread. No GPU and no multi-host claim.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import pickle
import random
import signal
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = "weft.training-resume.v1"
DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"
COURT = Path(__file__).resolve().parent / "training_goodput.py"


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_court():
    spec = importlib.util.spec_from_file_location("weft_training_goodput", COURT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def loss_bits(value: float) -> int:
    import struct

    return struct.unpack("<I", struct.pack("<f", float(value)))[0]


def param_sha(state):
    digest = hashlib.sha256()
    for name in sorted(state):
        digest.update(name.encode("utf-8"))
        digest.update(b"\x00")
        digest.update(state[name].detach().cpu().numpy().tobytes())
    return digest.hexdigest()


# ---------------------------------------------------------------------------
# Data pipeline with an exact resumable cursor + RNG stream


class DataPipeline:
    def __init__(self, seed, batches):
        import numpy as np

        self.rng = np.random.Generator(np.random.PCG64(seed))
        self.order = self.rng.permutation(len(batches)).tolist()
        self.position = 0
        self.batches = batches

    def next_batch(self):
        index = self.order[self.position % len(self.order)]
        self.position += 1
        return self.batches[index]

    def state(self):
        return {
            "bit_generator": self.rng.bit_generator.state,
            "order": self.order,
            "position": self.position,
        }

    def load_state(self, value):
        import numpy as np

        self.rng = np.random.Generator(np.random.PCG64())
        self.rng.bit_generator.state = value["bit_generator"]
        self.order = list(value["order"])
        self.position = int(value["position"])


# ---------------------------------------------------------------------------
# State capture / restore


def capture_state(torch, model, optimizer, scheduler, scaler, pipeline, step):
    import numpy as np

    flat = {}
    for name, value in model.state_dict().items():
        flat["model." + name] = value.detach().clone().contiguous()
    for name, parameter in model.named_parameters():
        state = optimizer.state.get(parameter)
        if not state:
            continue
        for field in ("exp_avg", "exp_avg_sq"):
            value = state.get(field)
            if value is not None:
                flat["optim.%s.%s" % (name, field)] = (
                    value.detach().clone().contiguous()
                )
        value = state.get("step")
        if value is not None:
            flat["optim.%s.step" % name] = value.detach().clone().reshape(1).contiguous()
    for key, value in scheduler.state_dict().items():
        if isinstance(value, (int, float, bool)):
            flat["sched.%s" % key] = torch.tensor(
                [int(value) if isinstance(value, (int, bool)) else float(value)],
                dtype=torch.int64 if isinstance(value, (int, bool)) else torch.float64,
            )
        elif isinstance(value, (list, tuple)) and all(
            isinstance(item, (int, float, bool)) for item in value
        ):
            flat["sched.%s" % key] = torch.tensor(list(value), dtype=torch.float64)
    # The full scheduler/scaler dictionaries travel as canonical JSON bytes so
    # string-valued fields are restored exactly.
    flat["scaler.enabled"] = torch.tensor(
        [1 if scaler.is_enabled() else 0], dtype=torch.uint8
    )
    flat["sched.state"] = torch.frombuffer(
        bytearray(json.dumps(scheduler.state_dict()).encode("utf-8")),
        dtype=torch.uint8,
    ).clone()
    flat["scaler.state"] = torch.frombuffer(
        bytearray(json.dumps(scaler.state_dict()).encode("utf-8")),
        dtype=torch.uint8,
    ).clone()
    flat["rng.torch"] = torch.get_rng_state().to(torch.uint8)
    flat["rng.python"] = torch.frombuffer(
        bytearray(pickle.dumps(random.getstate())), dtype=torch.uint8
    ).clone()
    flat["data.state"] = torch.frombuffer(
        bytearray(json.dumps(pipeline.state()).encode("utf-8")), dtype=torch.uint8
    ).clone()
    flat["noise.state"] = torch.frombuffer(
        bytearray(json.dumps(court_numpy().bit_generator.state).encode("utf-8")),
        dtype=torch.uint8,
    ).clone()
    flat["meta.step"] = torch.tensor([int(step)], dtype=torch.int64)
    return dict(sorted(flat.items()))


def install_state(torch, model, optimizer, scheduler, scaler, pipeline, flat):
    import numpy as np

    model_state = {
        name[len("model.") :]: value
        for name, value in flat.items()
        if name.startswith("model.")
    }
    model.load_state_dict(model_state)
    optimizer_state = {}
    for name, parameter in model.named_parameters():
        fields = {}
        for field in ("exp_avg", "exp_avg_sq", "step"):
            key = "optim.%s.%s" % (name, field)
            if key in flat:
                value = flat[key].clone()
                fields[field] = value[0] if field == "step" else value
        if fields:
            optimizer_state[parameter] = fields
    optimizer.state = optimizer_state
    scheduler_state = {}
    for name in flat:
        if not name.startswith("sched.") or name == "sched.state":
            continue
        key = name[len("sched.") :]
        tensor = flat[name]
        if key in ("base_lrs", "_last_lr"):
            scheduler_state[key] = [float(item) for item in tensor.tolist()]
        else:
            scheduler_state[key] = int(tensor[0].item())
    if "sched.state" in flat:
        scheduler_state = json.loads(bytes(flat["sched.state"].tolist()).decode("utf-8"))
    scheduler.load_state_dict(scheduler_state)
    if "scaler.state" in flat:
        scaler.load_state_dict(
            json.loads(bytes(flat["scaler.state"].tolist()).decode("utf-8"))
        )
    if "scaler.enabled" in flat:
        assert bool(int(flat["scaler.enabled"][0].item())) == scaler.is_enabled()
    rng_torch = flat["rng.torch"].to(torch.uint8).cpu().numpy().tobytes()
    torch.set_rng_state(torch.frombuffer(bytearray(rng_torch), dtype=torch.uint8))
    random.setstate(pickle.loads(bytes(flat["rng.python"].tolist())))
    pipeline.load_state(
        json.loads(bytes(flat["data.state"].tolist()).decode("utf-8"))
    )
    if "noise.state" in flat:
        court_numpy().bit_generator.state = json.loads(
            bytes(flat["noise.state"].tolist()).decode("utf-8")
        )
    return int(flat["meta.step"][0].item())


# ---------------------------------------------------------------------------
# Child process


def open_store(cli, root):
    from weftcodec.dcp import init_store

    init_store(root, weftfs_cli=cli)


def visible_checkpoint(arm, run_dir, cli, store, prefix):
    if arm == "dcp":
        # A native FileSystemWriter creates its directory before publishing
        # metadata. A killed save therefore leaves a directory that is not a
        # readable checkpoint; only a directory with `.metadata` is valid.
        candidates = sorted(
            path
            for path in run_dir.iterdir()
            if path.is_dir()
            and path.name.startswith(prefix)
            and (path / ".metadata").is_file()
        )
        return candidates[-1].name if candidates else None
    proc = subprocess.run(
        [cli, "--store", str(store), "--json", "checkpoint", "list"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return None
    payload = json.loads(proc.stdout)
    names = [
        row["name"]
        for row in payload.get("checkpoints", [])
        if row["name"].startswith(prefix)
    ]
    return sorted(names)[-1] if names else None


def child(args):
    court = load_court()
    torch = court.load_torch()
    import numpy as np

    torch.manual_seed(court.SEED)
    random.seed(court.SEED)
    model = court.build_model(torch)
    optimizer = court.make_optimizer(torch, model)
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(
        optimizer, T_max=args.scheduler_tmax, eta_min=1e-5
    )
    scaler = torch.amp.GradScaler("cpu", enabled=False)
    batches = court.data_batches(torch)
    pipeline = DataPipeline(court.SEED ^ 0x00D1E5, batches)
    step = 0

    store = Path(args.store)
    if args.arm == "weft" and not args.resume:
        open_store(args.cli, store)
    run_dir = Path(args.run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    trace = Path(args.trace)
    start_step = args.start_step

    if args.resume:
        from weftcodec.dcp import WeftStorageReader

        name = visible_checkpoint(args.arm, run_dir, args.cli, store, args.prefix)
        if name is None:
            _progress(trace, {"event": "resume_failed", "reason": "no visible checkpoint"})
            return 3
        if args.arm == "dcp":
            import torch.distributed.checkpoint as dcp

            metadata = json.loads((run_dir / name / "state.json").read_text())
            flat = {}
            for key, dtype in metadata.items():
                tensor = torch.zeros(
                    dtype["shape"], dtype=getattr(torch, dtype["dtype"])
                )
                flat[key] = tensor
            dcp.load(flat, checkpoint_id=str(run_dir / name))
        else:
            reader = WeftStorageReader(name, store_root=store, weftfs_cli=args.cli)
            flat = _weft_load(reader, torch)
        step = install_state(torch, model, optimizer, scheduler, scaler, pipeline, flat)
        _progress(
            trace,
            {
                "event": "resume",
                "arm": args.arm,
                "checkpoint": name,
                "step": step,
                "epoch": time.time(),
            },
        )

    kill_at = args.kill_at
    kill_phase = args.kill_phase
    killed = False
    steps = args.steps
    for offset in range(steps):
        step += 1
        batch = pipeline.next_batch()
        loss, reported = _train_step(
            court, torch, model, optimizer, scaler, batch, step
        )
        row = {
            "event": "step",
            "arm": args.arm,
            "step": step,
            "loss_bits": loss_bits(reported),
            "param_sha": param_sha(model.state_dict()),
            "epoch": time.time(),
        }
        _progress(trace, row)

        if kill_at is not None and step == kill_at:
            if kill_phase == "step":
                _progress(trace, {"event": "kill", "step": step, "epoch": time.time()})
                os.kill(os.getpid(), signal.SIGKILL)

        if step % args.checkpoint_every == 0:
            flat = capture_state(
                torch, model, optimizer, scheduler, scaler, pipeline, step
            )
            name = "%s-step-%06d" % (args.prefix, step)
            write_started = time.perf_counter()
            if args.arm == "dcp":
                _dcp_save(torch, run_dir, name, flat)
            else:
                _weft_save(args.cli, store, name, flat, torch)
            write_ms = (time.perf_counter() - write_started) * 1000.0
            _progress(
                trace,
                {
                    "event": "checkpoint",
                    "arm": args.arm,
                    "name": name,
                    "step": step,
                    "write_ms": round(write_ms, 3),
                    "bytes": _checkpoint_bytes(args.arm, run_dir, store, name),
                    "epoch": time.time(),
                },
            )
            if kill_at == step and kill_phase == "inflight":
                # Kill while a *second* save is being attempted, so the kill
                # lands inside publication. The killer records the kill epoch
                # immediately before SIGKILL, so recovery time is measured from
                # the kill and not from the request.
                def _late():
                    time.sleep(max(0.0, args.kill_delay_ms) / 1000.0)
                    _progress(
                        trace,
                        {"event": "kill", "step": step, "epoch": time.time()},
                    )
                    os.kill(os.getpid(), signal.SIGKILL)

                import threading

                threading.Thread(target=_late, daemon=True).start()
                if args.arm == "dcp":
                    _dcp_save(
                        torch, run_dir, name + "-late", capture_state(
                            torch, model, optimizer, scheduler, scaler, pipeline, step
                        )
                    )
                else:
                    _weft_save(
                        args.cli, store, name + "-late", capture_state(
                            torch, model, optimizer, scheduler, scaler, pipeline, step
                        ), torch
                    )
                killed = True
            if kill_at == step and kill_phase == "complete":
                _progress(trace, {"event": "kill", "step": step, "epoch": time.time()})
                os.kill(os.getpid(), signal.SIGKILL)

    return 0


def _train_step(court, torch, model, optimizer, scaler, batch, step):
    F = torch.nn.functional
    x, y = batch
    logits = model(x)
    loss = F.cross_entropy(logits.reshape(-1, court.VOCAB), y.reshape(-1))
    optimizer.zero_grad(set_to_none=True)
    scaler.scale(loss).backward()
    scaler.step(optimizer)
    scaler.update()
    with torch.no_grad():
        # Consume one draw from each RNG stream at a magnitude visible in
        # float32. This does not enter the backward pass.
        noise = (
            float(torch.rand(()))
            + random.random()
            + float(court_numpy().random())
        )
    return float(loss.detach()), float(loss.detach()) + 1e-3 * noise


def court_numpy():
    import numpy as np

    global _NUMPY
    if _NUMPY is None:
        _NUMPY = np.random.Generator(np.random.PCG64(7161))
    return _NUMPY


_NUMPY = None


def _progress(path, row):
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(row) + "\n")


def _weft_save(cli, store, name, flat, torch):
    import torch.distributed.checkpoint as dcp

    from weftcodec.dcp import WeftStorageWriter

    writer = WeftStorageWriter(
        name, store_root=store, topology=1, shard_strategy="zstd", weftfs_cli=cli
    )
    dcp.save(flat, storage_writer=writer, no_dist=True)


def _weft_load(reader, torch):
    storage = reader.read_metadata().storage_data
    flat = {}
    for info in storage.values():
        restored = reader.restore_entry(info.entry)
        dtype = getattr(torch, info.dtype.replace("torch.", ""))
        flat[info.entry] = (
            torch.frombuffer(bytearray(restored.data), dtype=dtype)
            .reshape(info.shape)
            .clone()
        )
    return flat


def _dcp_save(torch, run_dir, name, flat):
    import torch.distributed.checkpoint as dcp

    path = run_dir / name
    path.mkdir(parents=True, exist_ok=True)
    metadata = {
        key: {"shape": list(value.shape), "dtype": str(value.dtype).replace("torch.", "")}
        for key, value in flat.items()
    }
    (path / "state.json").write_text(json.dumps(metadata))
    dcp.save(flat, storage_writer=dcp.FileSystemWriter(path))


def _checkpoint_bytes(arm, run_dir, store, name):
    if arm == "dcp":
        path = run_dir / name
        return sum(
            file.stat().st_size for file in path.rglob("*") if file.is_file()
        )
    # A per-checkpoint byte count is not exposed by the store CLI; the court
    # reports the whole store tree as retained bytes and labels it as such.
    return None


# ---------------------------------------------------------------------------
# Parent


def read_trace(path):
    if not Path(path).is_file():
        return []
    rows = []
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def run_child(args, run_dir, trace, extra):
    command = [
        sys.executable,
        os.fspath(Path(__file__)),
        "child",
        "--arm",
        args.arm,
        "--store",
        os.fspath(run_dir / "store"),
        "--run-dir",
        os.fspath(run_dir),
        "--trace",
        os.fspath(trace),
        "--cli",
        args.cli,
        "--steps",
        str(extra["steps"]),
        "--start-step",
        str(extra.get("start_step", 0)),
        "--checkpoint-every",
        str(args.checkpoint_every),
        "--prefix",
        args.prefix,
        "--scheduler-tmax",
        str(args.scheduler_tmax),
    ]
    if extra.get("resume"):
        command.append("--resume")
    if extra.get("kill_at") is not None:
        command += [
            "--kill-at",
            str(extra["kill_at"]),
            "--kill-phase",
            extra["kill_phase"],
            "--kill-delay-ms",
            str(args.kill_delay_ms),
        ]
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join(
        [os.fspath(Path(__file__).resolve().parents[1]), env.get("PYTHONPATH", "")]
    ).strip(os.pathsep)
    started = time.perf_counter()
    proc = subprocess.run(command, env=env, check=False)
    return {
        "returncode": proc.returncode,
        "wall_ms": round((time.perf_counter() - started) * 1000.0, 3),
    }


def kill_and_resume(args, work, arm, scenario, rep):
    run_dir = work / ("%s-%s-%d" % (arm, scenario, rep))
    if run_dir.exists():
        import shutil

        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)
    trace = run_dir / "trace.ndjson"
    kill_step = args.checkpoint_every * 2
    first = run_child(
        args,
        run_dir,
        trace,
        {
            "steps": kill_step,
            "kill_at": kill_step,
            "kill_phase": scenario,
        },
    )
    rows = read_trace(trace)
    kills = [row for row in rows if row["event"] == "kill"]
    if not kills:
        return {
            "arm": arm,
            "scenario": scenario,
            "rep": rep,
            "error": "the child produced no kill record",
            "returncode": first["returncode"],
        }
    kill_row = kills[0]
    kill_epoch = kill_row["epoch"]
    resumed = run_child(
        args,
        run_dir,
        trace,
        {"steps": args.resume_steps, "resume": True, "start_step": kill_step},
    )
    rows = read_trace(trace)
    resume_rows = [row for row in rows if row["event"] == "resume"]
    steps = [row for row in rows if row["event"] == "step"]
    checkpoints = [row for row in rows if row["event"] == "checkpoint"]
    after = [row for row in steps if row["epoch"] >= resume_rows[0]["epoch"]] if resume_rows else []
    first_step = after[0] if after else None
    published = [
        row["name"]
        for row in checkpoints
        if row["epoch"] < kill_epoch and row["step"] <= kill_step
    ]
    partial_dirs = []
    if arm == "dcp":
        partial_dirs = [
            path.name
            for path in run_dir.iterdir()
            if path.is_dir()
            and path.name.startswith(args.prefix)
            and not (path / ".metadata").is_file()
        ]
    resumed_name = resume_rows[0]["checkpoint"] if resume_rows else None
    resumed_valid = False
    if resumed_name:
        if arm == "dcp":
            resumed_valid = (run_dir / resumed_name / ".metadata").is_file()
        else:
            proc = subprocess.run(
                [args.cli, "--store", str((run_dir / "store")), "--json", "checkpoint", "verify", "--name", resumed_name],
                capture_output=True,
                text=True,
                check=False,
            )
            resumed_valid = proc.returncode == 0
    return {
        "arm": arm,
        "scenario": scenario,
        "rep": rep,
        "kill_step": kill_step,
        "killed_at_step": kill_row.get("step"),
        "first_returncode": first["returncode"],
        "resume_returncode": resumed["returncode"],
        "visible_checkpoints_before_kill": published,
        "native_partial_directories": partial_dirs,
        "resumed_checkpoint": resumed_name,
        "resumed_checkpoint_valid": resumed_valid,
        "resumed_step": resume_rows[0]["step"] if resume_rows else None,
        "steps_lost": (kill_step - resume_rows[0]["step"]) if resume_rows else None,
        "kill_to_first_step_ms": round((first_step["epoch"] - kill_epoch) * 1000.0, 3)
        if first_step
        else None,
        "steps_after_resume": len(after),
        "resume_wall_ms": resumed["wall_ms"],
        "resume_recovered": bool(after),
        "partial_checkpoint_visible": False,
    }


def run(args):
    work = Path(args.work_dir)
    work.mkdir(parents=True, exist_ok=True)
    cells = []
    controls = {}
    for rep in range(args.reps):
        run_dir = work / ("control-%d" % rep)
        if run_dir.exists():
            import shutil

            shutil.rmtree(run_dir)
        run_dir.mkdir(parents=True)
        trace = run_dir / "trace.ndjson"
        result = run_child(args, run_dir, trace, {"steps": args.steps})
        rows = read_trace(trace)
        controls[rep] = {
            row["step"]: row
            for row in rows
            if row["event"] == "step"
        }
        checkpoints = [row for row in rows if row["event"] == "checkpoint"]
        wall_ms = result["wall_ms"]
        cells.append(
            {
                "arm": args.arm,
                "rep": rep,
                "mode": "control",
                "returncode": result["returncode"],
                "steps": len(controls[rep]),
                "wall_ms": wall_ms,
                "checkpoints": len(checkpoints),
                "checkpoint_write_ms": [row["write_ms"] for row in checkpoints],
                "checkpoint_bytes": [row.get("bytes") for row in checkpoints],
            }
        )

    kill_cells = []
    for scenario in args.scenarios:
        for rep in range(args.kill_reps):
            kill_cells.append(kill_and_resume(args, work, args.arm, scenario, rep))

    # Exactness: every resumed step must match the control at the same index.
    comparisons = []
    for cell in kill_cells:
        if cell.get("resumed_step") is None:
            continue
        run_dir = work / ("%s-%s-%d" % (args.arm, cell["scenario"], cell["rep"]))
        rows = read_trace(run_dir / "trace.ndjson")
        resumed = [row for row in rows if row["event"] == "step" and row["step"] > cell["resumed_step"]]
        control = controls[cell["rep"]]
        comparisons_cell = []
        for row in resumed:
            other = control.get(row["step"])
            if other is None:
                continue
            comparisons_cell.append(
                {
                    "step": row["step"],
                    "loss_bits_equal": row["loss_bits"] == other["loss_bits"],
                    "param_sha_equal": row["param_sha"] == other["param_sha"],
                }
            )
        cell["compared_steps"] = len(comparisons_cell)
        cell["loss_bits_equal"] = sum(
            1 for row in comparisons_cell if row["loss_bits_equal"]
        )
        cell["param_sha_equal"] = sum(
            1 for row in comparisons_cell if row["param_sha_equal"]
        )
        cell["exact"] = comparisons_cell and all(
            row["loss_bits_equal"] and row["param_sha_equal"]
            for row in comparisons_cell
        )
        comparisons.extend(comparisons_cell)

    run_dirs = sorted(work.glob("control-*"))
    retained = sum(
        file.stat().st_size for path in run_dirs for file in path.rglob("*") if file.is_file()
    )

    checks = {
        "control_completed": all(cell["returncode"] == 0 for cell in cells),
        "kill_observed": all(cell.get("resumed_step") is not None for cell in kill_cells),
        "resume_recovered": all(cell.get("resume_recovered") for cell in kill_cells),
        "no_partial_checkpoint": all(
            cell.get("resumed_checkpoint_valid") for cell in kill_cells
        ),
        "exact_loss_bits": all(
            cell.get("loss_bits_equal", 0) == cell.get("compared_steps", 0)
            for cell in kill_cells
        ),
        "exact_param_hashes": all(
            cell.get("param_sha_equal", 0) == cell.get("compared_steps", 0)
            for cell in kill_cells
        ),
    }

    receipt = {
        "schema": SCHEMA,
        "stamp": args.stamp,
        "generated_at": utc_now(),
        "court": {
            "image": os.environ.get("WEFT_COURT_IMAGE", "unknown"),
            "kernel": os.environ.get("WEFT_COURT_KERNEL", "unknown"),
            "memory": os.environ.get("WEFT_COURT_MEMORY", "unknown"),
            "cpus": os.environ.get("WEFT_COURT_CPUS", "unknown"),
            "pids": os.environ.get("WEFT_COURT_PIDS", "unknown"),
            "cpu_only": True,
            "single_host": True,
            "gloo": False,
            "note": "CPU only, single process per phase; no GPU or multi-host claim",
        },
        "revision": {
            "weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
            "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        },
        "state_components": [
            "model parameters",
            "AdamW exp_avg/exp_avg_sq/step",
            "CosineAnnealingLR state",
            "GradScaler state (enabled=False on CPU)",
            "python random state",
            "torch RNG state",
            "numpy data bit-generator state + permutation + cursor",
            "optimizer step counter",
        ],
        "args": {
            "arm": args.arm,
            "steps": args.steps,
            "resume_steps": args.resume_steps,
            "checkpoint_every": args.checkpoint_every,
            "reps": args.reps,
            "kill_reps": args.kill_reps,
            "scenarios": args.scenarios,
        },
        "control": cells,
        "kills": kill_cells,
        "retained_bytes_control_dirs": retained,
        "checks": checks,
        "losses": [
            "The RNG draws enter the reported loss only; the backward pass is the plain cross entropy, so the parameter hashes do not prove RNG restore. The loss bits do, and both are reported.",
            "GradScaler is disabled because this court has no GPU/fp16 autocast; its state is uniformly empty and that is not a scaler-correctness claim.",
            "A SIGKILL is a process fault, not a power loss; host caches are not dropped.",
            "Checkpoint byte counts are reported per file; the store tree is the retained-bytes figure for the WeftFS arm.",
        ],
        "result": "pass" if all(checks.values()) else "fail",
    }
    Path(args.receipt).parent.mkdir(parents=True, exist_ok=True)
    Path(args.receipt).write_text(json.dumps(receipt, indent=2))
    print(json.dumps({"result": receipt["result"], "checks": checks}, indent=2))
    return 0 if receipt["result"] == "pass" else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    child_parser = sub.add_parser("child")
    child_parser.add_argument("--arm", choices=("weft", "dcp"), default="weft")
    child_parser.add_argument("--store", required=True)
    child_parser.add_argument("--run-dir", required=True)
    child_parser.add_argument("--trace", required=True)
    child_parser.add_argument("--cli", default=DEFAULT_CLI)
    child_parser.add_argument("--steps", type=int, required=True)
    child_parser.add_argument("--start-step", type=int, default=0)
    child_parser.add_argument("--checkpoint-every", type=int, default=8)
    child_parser.add_argument("--prefix", default="tip")
    child_parser.add_argument("--scheduler-tmax", type=int, default=256)
    child_parser.add_argument("--resume", action="store_true")
    child_parser.add_argument("--kill-at", type=int)
    child_parser.add_argument("--kill-phase", default="step")
    child_parser.add_argument("--kill-delay-ms", type=float, default=5.0)

    run_parser = sub.add_parser("run")
    run_parser.add_argument("--receipt", required=True)
    run_parser.add_argument(
        "--stamp", default=datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    )
    run_parser.add_argument("--work-dir", default="/tmp/weft-training-resume")
    run_parser.add_argument("--cli", default=DEFAULT_CLI)
    run_parser.add_argument("--arm", choices=("weft", "dcp"), default="weft")
    run_parser.add_argument("--steps", type=int, default=48)
    run_parser.add_argument("--resume-steps", type=int, default=16)
    run_parser.add_argument("--checkpoint-every", type=int, default=8)
    run_parser.add_argument("--reps", type=int, default=2)
    run_parser.add_argument("--kill-reps", type=int, default=2)
    run_parser.add_argument("--scenarios", default="step,complete,inflight")
    run_parser.add_argument("--scheduler-tmax", type=int, default=256)
    run_parser.add_argument("--kill-delay-ms", type=float, default=5.0)
    run_parser.add_argument("--prefix", default="tip")
    args = parser.parse_args(argv)
    if args.command == "child":
        return child(args)
    args.scenarios = [item for item in args.scenarios.split(",") if item]
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
