#!/usr/bin/env python3
"""Training goodput / time-to-safe-resume court (subphase R11).

This is a *real* small PyTorch training loop, not a fixture save:

* a three-block transformer stack (explicit multi-head attention, GELU MLP,
  LayerNorm, token+position embeddings) trained with AdamW on a deterministic
  seeded synthetic next-token dataset;
* exactly fixed: seed, data, batch order, no dropout, one BLAS thread,
  ``torch.use_deterministic_algorithms(True)``;
* checkpoints every ``k`` steps are asynchronous: the training thread clones
  the live state (the synchronisation point) and submits the encode/store to a
  background worker; it never waits for the encode;
* three configurations on identical steps and data:
  A. ``torch.save`` to a local file (async worker),
  B. PyTorch DCP ``FileSystemWriter`` to a local directory (async worker),
  C. PyTorch DCP through the WeftFS adapter (``save_checkpoint_async``);
* every checkpoint is resumed into a *fresh process* and compared exactly
  (tensor SHA-256, optimizer state, the next step's loss bits, and the state
  reached after that step);
* a SIGKILL court kills after the checkpoint ack and while a save is in
  flight, then reports which checkpoint the store exposes (old-or-new, never
  partial) and the time from kill to the first completed optimizer step.

Runs inside the Docker python court:

  scripts/docker-court.sh python \
    "PYTHONPATH=/work/python \
     python3 python/courts/training_goodput.py run --receipt <file>"

numpy is optional (it makes tensor hashing faster); the harness falls back to
pure Python bytes. There is no GPU in the court and no GPU claim is made.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import statistics
import struct
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# Fix BLAS thread counts before torch is imported in a child.
for _name in (
    "OMP_NUM_THREADS",
    "MKL_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "NUMEXPR_NUM_THREADS",
):
    os.environ.setdefault(_name, "1")

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CLI = "/work/.docker-cache/target-rust/release/weftfs"

SCHEMA_RECEIPT = "weft.training-goodput.v1"
SCHEMA_CHECKPOINT = "weft.goodput.checkpoint.v1"

# Loop shape. These are the only values that define "identical steps and data".
SEED = 20261110
D_MODEL = 96
LAYERS = 3
HEADS = 4
VOCAB = 384
SEQ = 48
BATCH = 8
N_BATCHES = 64
DEFAULT_STEPS = 256
DEFAULT_EVERY = 64

DEFAULT_REPS = 5
DEFAULT_KILL_REPS = 3
STAGING_BYTES = 64 << 20
LEARNING_RATE = 3e-3
BETAS = (0.9, 0.95)
WEIGHT_DECAY = 0.01
EPS = 1e-8

CONFIGS = ("torchsave", "torchsave-durable", "dcp", "weft", "weft-fast")

#: Configs whose checkpoint is a local file or directory (no WeftFS store).
NATIVE_CONFIGS = ("torchsave", "torchsave-durable", "dcp")
CONFIG_LABEL = {
    "torchsave": "A: torch.save file",
    "torchsave-durable": "D: torch.save + fsync + atomic rename (equivalent durability)",
    "dcp": "B: PyTorch DCP directory",
    "weft": "C: DCP -> WeftFS adapter",
    "weft-fast": "E: WeftFS fast policy, raw frames (performance floor)",
}

DTYPE_NAMES = {
    "float16": "float16",
    "bfloat16": "bfloat16",
    "float32": "float32",
    "float64": "float64",
    "int8": "int8",
    "uint8": "uint8",
    "int16": "int16",
    "int32": "int32",
    "int64": "int64",
}


def stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def vmhwm_kib() -> int | None:
    try:
        with open("/proc/self/status", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return None


def proc_stat() -> dict:
    try:
        with open("/proc/self/stat", encoding="utf-8") as handle:
            raw = handle.read()
    except OSError:
        return {}
    fields = raw.rsplit(")", 1)[1].split()
    cpu_ms = (int(fields[11]) + int(fields[12])) * 10.0
    return {"cpu_ms": round(cpu_ms, 1), "threads": int(fields[17])}


def dir_bytes(root: Path) -> dict:
    total = 0
    files = 0
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            try:
                total += os.path.getsize(os.path.join(dirpath, name))
                files += 1
            except OSError:
                pass
    return {"bytes": total, "files": files}


def durable_torch_save(torch, state, path):
    """`torch.save` plus the durability a WeftFS commit provides.

    Writes a sibling temp file, fsyncs it, renames it over the destination and
    fsyncs the directory. The guarantees matched are: the bytes are on stable
    storage before the save returns, and the destination is either the previous
    complete artifact or the new one (atomic rename), never a partial file.
    """
    path = Path(path)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "wb") as handle:
        torch.save(state, handle)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp, path)
    directory = os.open(str(path.parent), os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp, path)
    try:
        dir_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
    except OSError:
        pass


def append_progress(path: Path, record: dict) -> None:
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")
        handle.flush()
        os.fsync(handle.fileno())


def read_progress(path: Path) -> list:
    rows = []
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    rows.append(json.loads(line))
                except ValueError:
                    pass
    except OSError:
        pass
    return rows


# ---------------------------------------------------------------------------
# Deterministic model, data and state capture


def load_torch():
    import torch

    torch.use_deterministic_algorithms(True)
    torch.set_num_threads(1)
    try:
        torch.set_num_interop_threads(1)
    except RuntimeError:
        pass
    return torch


def build_model(torch):
    nn = torch.nn
    F = torch.nn.functional
    head_dim = D_MODEL // HEADS
    scale = head_dim ** -0.5

    class Block(nn.Module):
        def __init__(self):
            super().__init__()
            self.ln1 = nn.LayerNorm(D_MODEL)
            self.qkv = nn.Linear(D_MODEL, 3 * D_MODEL)
            self.proj = nn.Linear(D_MODEL, D_MODEL)
            self.ln2 = nn.LayerNorm(D_MODEL)
            self.fc1 = nn.Linear(D_MODEL, 4 * D_MODEL)
            self.fc2 = nn.Linear(4 * D_MODEL, D_MODEL)
            self.register_buffer(
                "causal_mask",
                torch.triu(torch.ones(SEQ, SEQ, dtype=torch.bool), diagonal=1),
                persistent=False,
            )

        def forward(self, x):
            h = self.ln1(x)
            qkv = self.qkv(h).view(x.shape[0], SEQ, 3, HEADS, head_dim)
            q, k, v = qkv.permute(2, 0, 3, 1, 4)
            att = (q @ k.transpose(-1, -2)) * scale
            att = att.masked_fill(self.causal_mask, float("-inf")).softmax(-1)
            y = (att @ v).transpose(1, 2).reshape(x.shape[0], SEQ, D_MODEL)
            x = x + self.proj(y)
            return x + self.fc2(F.gelu(self.fc1(self.ln2(x))))

    class TinyTransformer(nn.Module):
        def __init__(self):
            super().__init__()
            self.tok = nn.Embedding(VOCAB, D_MODEL)
            self.pos = nn.Embedding(SEQ, D_MODEL)
            self.blocks = nn.ModuleList([Block() for _ in range(LAYERS)])
            self.ln_f = nn.LayerNorm(D_MODEL)
            self.head = nn.Linear(D_MODEL, VOCAB)

        def forward(self, idx):
            positions = torch.arange(idx.shape[1]).unsqueeze(0)
            x = self.tok(idx) + self.pos(positions)
            for block in self.blocks:
                x = block(x)
            return self.head(self.ln_f(x))

    return TinyTransformer()


def make_optimizer(torch, model):
    return torch.optim.AdamW(
        model.parameters(),
        lr=LEARNING_RATE,
        betas=BETAS,
        weight_decay=WEIGHT_DECAY,
        eps=EPS,
    )


def data_batches(torch):
    generator = torch.Generator().manual_seed(SEED ^ 0x5EED5EED)
    batches = []
    for _ in range(N_BATCHES):
        offset = torch.randint(0, VOCAB, (BATCH, 1), generator=generator)
        seq = (offset + torch.arange(SEQ + 1).unsqueeze(0)) % VOCAB
        batches.append((seq[:, :-1].contiguous(), seq[:, 1:].contiguous()))
    return batches


def batch_for_step(batches, step):
    return batches[(step - 1) % len(batches)]


def tensor_sha256(tensor) -> str:
    import torch

    # `.view(torch.uint8)` reinterprets the same storage bytes; contiguous first.
    u8 = tensor.detach().to("cpu").contiguous().view(torch.uint8)
    try:
        import numpy as np

        payload = np.asarray(u8).tobytes()
    except Exception:
        payload = bytes(u8.reshape(-1).tolist())
    return hashlib.sha256(payload).hexdigest()


def loss_bits(value) -> int:
    import torch

    scalar = value.detach().to(torch.float32).reshape(()).view(torch.int32)
    return int(scalar.item()) & 0xFFFFFFFF


def capture_state(torch, model, optimizer, step):
    """Exact clone of parameters, optimizer state and the step counter."""
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
                flat["optim.%s.%s" % (name, field)] = value.detach().clone().contiguous()
        value = state.get("step")
        if value is not None:
            flat["optim.%s.step" % name] = (
                value.detach().clone().reshape(1).contiguous()
            )
    flat["meta.step"] = torch.tensor([int(step)], dtype=torch.int64)
    return dict(sorted(flat.items()))


def flat_sha256(torch, flat) -> tuple[str, int]:
    digest = hashlib.sha256()
    for name in sorted(flat):
        digest.update(name.encode("utf-8"))
        digest.update(b"\x00")
        digest.update(bytes.fromhex(tensor_sha256(flat[name])))
    return digest.hexdigest(), len(flat)


def state_spec(torch, flat):
    rows = []
    for name in sorted(flat):
        tensor = flat[name]
        rows.append(
            {
                "name": name,
                "dtype": str(tensor.dtype).replace("torch.", ""),
                "shape": [int(dim) for dim in tensor.shape],
                "sha256": tensor_sha256(tensor),
            }
        )
    return rows


def build_target(torch, rows):
    target = {}
    for row in rows:
        dtype_name = row["dtype"]
        if dtype_name not in DTYPE_NAMES:
            raise RuntimeError("unsupported dtype in checkpoint spec: %s" % dtype_name)
        target[row["name"]] = torch.empty(
            [int(dim) for dim in row["shape"]], dtype=getattr(torch, dtype_name)
        )
    return target


def install_state(torch, model, optimizer, flat, step):
    model_state = {
        key[len("model.") :]: value
        for key, value in flat.items()
        if key.startswith("model.")
    }
    model.load_state_dict(model_state, strict=True)
    by_name = dict(model.named_parameters())
    for key, value in flat.items():
        if not key.startswith("optim."):
            continue
        remainder = key[len("optim.") :]
        name, field = remainder.rsplit(".", 1)
        if name not in by_name:
            raise RuntimeError("optimizer state names unknown parameter %r" % name)
        parameter = by_name[name]
        state = optimizer.state[parameter]
        if field == "step":
            state["step"] = value.detach().clone().reshape(())
        else:
            state[field] = value.detach().clone()
    observed = int(flat["meta.step"].reshape(()).item())
    if observed != int(step):
        raise RuntimeError("checkpoint says step %d, spec says %d" % (observed, step))
    return model, optimizer


def forward_loss(torch, model, batches, step):
    F = torch.nn.functional
    x, y = batch_for_step(batches, step)
    with torch.no_grad():
        logits = model(x)
        return F.cross_entropy(logits.reshape(-1, VOCAB), y.reshape(-1))


def train_step(torch, model, optimizer, batches, step):
    F = torch.nn.functional
    x, y = batch_for_step(batches, step)
    logits = model(x)
    loss = F.cross_entropy(logits.reshape(-1, VOCAB), y.reshape(-1))
    optimizer.zero_grad(set_to_none=True)
    loss.backward()
    optimizer.step()
    return loss.detach()


# ---------------------------------------------------------------------------
# Paths


def checkpoint_paths(config, run_dir, step):
    name = "ckpt-%04d" % step
    if config in ("torchsave", "torchsave-durable"):
        return {"artifact": run_dir / "ckpts" / (name + ".pt"), "checkpoint": name}
    if config == "dcp":
        return {"artifact": run_dir / "ckpts" / name, "checkpoint": name}
    return {
        "artifact": run_dir / "store",
        "store": run_dir / "store",
        "checkpoint": name,
    }


# ---------------------------------------------------------------------------
# Async save handles


class SaveHandle:
    """Uniform async handle for the three configurations."""

    def __init__(self, config, *, future=None, async_handle=None, submit_perf=None):
        self.config = config
        self.future = future
        self.async_handle = async_handle
        self.submit_perf = submit_perf
        self.done_perf = None
        self.finished_perf = None
        self.stats = {}
        self.error = None
        target = async_handle if async_handle is not None else future
        if target is not None:
            target.add_done_callback(self._on_done)

    def _on_done(self, _future):
        self.done_perf = time.perf_counter()

    def done(self):
        if self.async_handle is not None:
            return self.async_handle.done()
        return self.future.done()

    def finalize(self):
        if self.async_handle is not None:
            try:
                self.async_handle.result()
                self.stats = dict(self.async_handle.stats)
            finally:
                try:
                    self.async_handle.writer.close()
                except Exception:
                    pass
        else:
            self.future.result()
        self.finished_perf = time.perf_counter()
        if self.done_perf is None:
            self.done_perf = self.finished_perf

    def completion_ms(self):
        end = self.done_perf or self.finished_perf
        if end is None or self.submit_perf is None:
            return None
        return (end - self.submit_perf) * 1000.0


# ---------------------------------------------------------------------------
# Child: the training loop


def child_loop(args):
    from weftcodec.dcp import save_checkpoint_async

    process_start_epoch = time.time()
    process_start_perf = time.perf_counter()
    torch = load_torch()
    torch.manual_seed(SEED)
    model = build_model(torch)
    optimizer = make_optimizer(torch, model)
    batches = data_batches(torch)

    run_dir = Path(args.run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    progress = run_dir / "progress.ndjson"
    if progress.exists():
        progress.unlink()
    (run_dir / "ckpts").mkdir(parents=True, exist_ok=True)

    def spec_path(step):
        return run_dir / ("ckpt-%04d.expected.json" % step)

    def submit(step, snapshot):
        paths = checkpoint_paths(args.config, run_dir, step)
        submit_perf = time.perf_counter()
        if args.config == "torchsave":
            future = executor.submit(torch.save, snapshot, str(paths["artifact"]))
            handle = SaveHandle("torchsave", future=future, submit_perf=submit_perf)
        elif args.config == "torchsave-durable":
            # Equivalent durability to a WeftFS commit: write a temp file,
            # fsync it, rename it over the destination, fsync the directory.
            future = executor.submit(
                durable_torch_save, torch, snapshot, paths["artifact"]
            )
            handle = SaveHandle(
                "torchsave-durable", future=future, submit_perf=submit_perf
            )
        elif args.config == "dcp":
            import torch.distributed.checkpoint as dcp

            future = executor.submit(
                dcp.save, snapshot, checkpoint_id=str(paths["artifact"]), no_dist=True
            )
            handle = SaveHandle("dcp", future=future, submit_perf=submit_perf)
        else:
            fast = args.config == "weft-fast"
            async_handle = save_checkpoint_async(
                snapshot,
                checkpoint_id=paths["checkpoint"],
                store_root=str(paths["store"]),
                topology=1,
                shard_strategy="raw" if fast else "zstd",
                staging_bytes=STAGING_BYTES,
                init_store=False,
                weftfs_cli=args.weftfs_cli,
                max_workers=1,
                write_policy="fast" if fast else None,
                fast_codec="raw" if fast else None,
            )
            handle = SaveHandle(
                args.config, async_handle=async_handle, submit_perf=submit_perf
            )
        return handle

    def physical_bytes():
        if args.config in ("torchsave", "torchsave-durable"):
            total = 0
            for path in sorted((run_dir / "ckpts").glob("*.pt")):
                total += path.stat().st_size
            return {"bytes": total, "files": len(list((run_dir / "ckpts").glob("*.pt")))}
        if args.config == "dcp":
            total = 0
            files = 0
            for child in sorted((run_dir / "ckpts").iterdir()):
                info = dir_bytes(child)
                total += info["bytes"]
                files += info["files"]
            return {"bytes": total, "files": files}
        return dir_bytes(run_dir / "store")

    executor = None
    if args.config in ("torchsave", "torchsave-durable", "dcp"):
        from concurrent.futures import ThreadPoolExecutor

        executor = ThreadPoolExecutor(
            max_workers=1, thread_name_prefix="weft-goodput-save"
        )

    events = []
    step_ms = []
    loss_bits_trace = []
    inflight = None
    previous_storage = physical_bytes()
    checkpoint_steps = set(range(args.checkpoint_every, args.steps + 1, args.checkpoint_every))
    # Post-step augmentation for the checkpoint immediately before this step.
    pending_augment = None
    final_state_sha = None
    final_tensor_count = None
    storage_at_end = None
    rss = None
    errors = []

    def record_event(event, step, **extra):
        row = {
            "event": event,
            "step": int(step),
            "epoch": time.time(),
            "rss_kib": vmhwm_kib(),
        }
        row.update(extra)
        events.append(row)
        append_progress(progress, row)

    def finish_handle(handle, landed_step, extra=None):
        nonlocal previous_storage
        handle.finalize()
        total = physical_bytes()
        delta = total["bytes"] - previous_storage["bytes"]
        previous_storage = total
        event = {
            "checkpoint_step": handle.step,
            "landed_step": landed_step,
            "completion_ms": round(handle.completion_ms() or 0.0, 3),
            "physical_bytes_total": total["bytes"],
            "physical_bytes_delta": delta,
            "physical_files": total["files"],
            "worker": handle.stats,
        }
        if extra:
            event.update(extra)
        events.append({"event": "checkpoint_done", **event})
        append_progress(progress, {"event": "checkpoint_done", **event})
        return

    run_start_perf = time.perf_counter()
    run_start_epoch = time.time()
    record_event("run_start", 0, pid=os.getpid(), config=args.config)

    for step in range(1, args.steps + 1):
        step_started = time.perf_counter()
        loss = train_step(torch, model, optimizer, batches, step)
        bits = loss_bits(loss)
        loss_bits_trace.append(bits)
        step_row = {"step": step, "loss_bits": bits, "checkpoint": False}

        if pending_augment is not None:
            # The checkpoint at ``pending_augment`` was taken before this step;
            # record what the *next* step produced so a fresh process can prove
            # the optimizer step itself is bit-identical.
            augment_started = time.perf_counter()
            target_step = pending_augment
            post = capture_state(torch, model, optimizer, step)
            post_sha, post_count = flat_sha256(torch, post)
            spec = json.loads(spec_path(target_step).read_text(encoding="utf-8"))
            spec["loss_step_bits"] = bits
            spec["post_step_state_sha256"] = post_sha
            spec["post_step_tensor_count"] = post_count
            write_json(spec_path(target_step), spec)
            step_row["augment_ms"] = round((time.perf_counter() - augment_started) * 1000.0, 3)
            pending_augment = None

        if step in checkpoint_steps:
            step_row["checkpoint"] = True
            # Never run two saves at once. A wait here is a property of the
            # checkpoint interval, not of the synchronisation point; it is
            # measured and reported separately.
            wait_ms = 0.0
            if inflight is not None:
                wait_started = time.perf_counter()
                inflight.finalize()
                wait_ms = (time.perf_counter() - wait_started) * 1000.0
                finish_handle(
                    inflight,
                    landed_step=step,
                    extra={"waited_for_previous_ms": round(wait_ms, 3)},
                )
                inflight = None

            snapshot_started = time.perf_counter()
            snapshot = capture_state(torch, model, optimizer, step)
            snapshot_ms = (time.perf_counter() - snapshot_started) * 1000.0

            harness_started = time.perf_counter()
            probe = forward_loss(torch, model, batches, step + 1)
            probe_bits = loss_bits(probe)
            probe_ms = (time.perf_counter() - harness_started) * 1000.0

            paths = checkpoint_paths(args.config, run_dir, step)
            spec_started = time.perf_counter()
            write_json(
                spec_path(step),
                {
                    "schema": SCHEMA_CHECKPOINT,
                    "config": args.config,
                    "step": int(step),
                    "seed": SEED,
                    "tensors": state_spec(torch, snapshot),
                    "probe_loss_bits": probe_bits,
                    "loss_step_bits": None,
                    "post_step_state_sha256": None,
                },
            )
            spec_ms = (time.perf_counter() - spec_started) * 1000.0

            handle = submit(step, snapshot)
            handle.step = step
            inflight = handle
            event = {
                "event": "checkpoint_submitted",
                "epoch": time.time(),
                "config": args.config,
                "checkpoint_step": int(step),
                "snapshot_ms": round(snapshot_ms, 3),
                "submit_ms": round((time.perf_counter() - handle.submit_perf) * 1000.0, 3),
                "pause_ms": round(
                    snapshot_ms + (time.perf_counter() - handle.submit_perf) * 1000.0, 3
                ),
                "probe_ms": round(probe_ms, 3),
                "spec_ms": round(spec_ms, 3),
                "waited_for_previous_ms": round(wait_ms, 3),
                "probe_loss_bits": probe_bits,
                "rss_kib": vmhwm_kib(),
            }
            events.append(event)
            append_progress(progress, event)

            if args.kill_before_ack_step == step:
                delay = max(0.0, float(args.kill_before_ack_delay_ms)) / 1000.0
                if delay:
                    time.sleep(delay)
                record_event(
                    "kill",
                    0,
                    kind="before_ack",
                    checkpoint_step=step,
                    checkpoint_artifact=str(paths["artifact"]),
                )
                os.kill(os.getpid(), 9)
                raise SystemExit("unreachable")

        step_ms.append((time.perf_counter() - step_started) * 1000.0)
        step_row["ms"] = round(step_ms[-1], 3)
        events.append({"event": "step", **step_row})

        if inflight is not None and inflight.done():
            handle = inflight
            inflight = None
            finish_handle(handle, landed_step=step)

        if args.kill_after_ack_step == step:
            if inflight is not None:
                inflight.finalize()
                finish_handle(inflight, landed_step=step)
                inflight = None
            record_event("kill", 0, kind="after_ack", kill_step=step)
            os.kill(os.getpid(), 9)
            raise SystemExit("unreachable")

        if step in checkpoint_steps:
            pending_augment = step

    # Close out any still-running save so its bytes and completion are measured.
    if inflight is not None:
        inflight.finalize()
        finish_handle(inflight, landed_step=args.steps)
        inflight = None

    storage_at_end = physical_bytes()
    final_flat = capture_state(torch, model, optimizer, args.steps)
    final_state_sha, final_tensor_count = flat_sha256(torch, final_flat)
    rss = vmhwm_kib()
    run_wall_ms = (time.perf_counter() - run_start_perf) * 1000.0
    process_wall_ms = (time.perf_counter() - process_start_perf) * 1000.0
    record_event(
        "run_done",
        args.steps,
        rss_kib=rss,
        run_wall_ms=round(run_wall_ms, 3),
        process_wall_ms=round(process_wall_ms, 3),
    )

    result = {
        "schema": "weft.goodput.run.v1",
        "config": args.config,
        "steps": args.steps,
        "checkpoint_every": args.checkpoint_every,
        "seed": SEED,
        "step_ms": [round(value, 3) for value in step_ms],
        "loss_bits": loss_bits_trace,
        "events": events,
        "final_state_sha256": final_state_sha,
        "final_tensor_count": final_tensor_count,
        "storage_end": storage_at_end,
        "peak_rss_kib": rss,
        "run_wall_ms": round(run_wall_ms, 3),
        "process_wall_ms": round(process_wall_ms, 3),
        "process_start_epoch": process_start_epoch,
        "run_start_epoch": run_start_epoch,
        "errors": errors,
        "proc": proc_stat(),
        "pid": os.getpid(),
    }
    write_json(run_dir / "results.json", result)
    if executor is not None:
        executor.shutdown(wait=True)
    print(json.dumps({"child": "loop", "config": args.config, "result": "ok"}), flush=True)
    return 0


# ---------------------------------------------------------------------------
# Child: resume into a fresh process


def child_resume(args):
    from weftcodec.dcp import WeftStorageReader

    process_start_epoch = time.time()
    process_start_perf = time.perf_counter()
    spec = json.loads(Path(args.spec).read_text(encoding="utf-8"))
    step = int(spec["step"])
    torch = load_torch()
    torch.manual_seed(SEED)
    model = build_model(torch)
    optimizer = make_optimizer(torch, model)
    batches = data_batches(torch)

    rows = spec["tensors"]
    target = build_target(torch, rows)
    load_started = time.perf_counter()
    if args.config in ("torchsave", "torchsave-durable"):
        flat = torch.load(args.path, map_location="cpu", weights_only=True)
    elif args.config == "dcp":
        import torch.distributed.checkpoint as dcp

        dcp.load(target, checkpoint_id=str(args.path), no_dist=True)
        flat = target
    else:
        import torch.distributed.checkpoint as dcp

        reader = WeftStorageReader(
            args.checkpoint, store_root=args.store, weftfs_cli=args.weftfs_cli
        )
        try:
            dcp.load(target, storage_reader=reader, no_dist=True)
            metadata_source = reader.metadata_source
        finally:
            reader.close()
        flat = target
    load_ms = (time.perf_counter() - load_started) * 1000.0
    if args.config not in ("weft", "weft-fast"):
        metadata_source = "native"

    expected = {row["name"]: row["sha256"] for row in rows}
    mismatches = []
    if set(flat) != set(expected):
        missing = sorted(set(expected) - set(flat))
        extra = sorted(set(flat) - set(expected))
        mismatches.append({"missing": missing, "extra": extra})
    for name in sorted(set(flat) & set(expected)):
        digest = tensor_sha256(flat[name])
        if digest != expected[name]:
            mismatches.append({"name": name, "expected": expected[name], "got": digest})

    install_started = time.perf_counter()
    install_state(torch, model, optimizer, flat, step)
    install_ms = (time.perf_counter() - install_started) * 1000.0

    probe = forward_loss(torch, model, batches, step + 1)
    probe_bits = loss_bits(probe)

    step_started = time.perf_counter()
    loss = train_step(torch, model, optimizer, batches, step + 1)
    step_bits = loss_bits(loss)
    first_step_epoch = time.time()
    step_ms = (time.perf_counter() - step_started) * 1000.0

    post = capture_state(torch, model, optimizer, step + 1)
    post_sha, post_count = flat_sha256(torch, post)
    post_match = (
        None
        if spec.get("post_step_state_sha256") is None
        else (post_sha == spec["post_step_state_sha256"])
    )

    expected_loss = spec.get("loss_step_bits")
    if expected_loss is None:
        expected_loss = spec.get("probe_loss_bits")
    loss_match = expected_loss is None or step_bits == int(expected_loss)
    probe_match = (
        spec.get("probe_loss_bits") is None
        or probe_bits == int(spec["probe_loss_bits"])
    )

    rss = vmhwm_kib()
    result = {
        "schema": "weft.goodput.resume.v1",
        "config": args.config,
        "checkpoint": args.checkpoint,
        "checkpoint_step": step,
        "tensors_compared": len(expected),
        "mismatches": mismatches,
        "exact": not mismatches,
        "metadata_source": metadata_source,
        "load_ms": round(load_ms, 3),
        "install_ms": round(install_ms, 3),
        "probe_loss_bits": probe_bits,
        "probe_loss_match": probe_match,
        "first_step_bits": step_bits,
        "first_step_loss_match": loss_match,
        "first_step_ms": round(step_ms, 3),
        "post_step_state_match": post_match,
        "post_step_tensor_count": post_count,
        "process_start_epoch": process_start_epoch,
        "first_step_epoch": first_step_epoch,
        "process_start_to_first_step_ms": round(
            (first_step_epoch - process_start_epoch) * 1000.0, 3
        ),
        "rss_kib": rss,
        "proc": proc_stat(),
    }
    write_json(Path(args.out), result)
    print(
        json.dumps(
            {
                "child": "resume",
                "config": args.config,
                "checkpoint": args.checkpoint,
                "exact": result["exact"],
                "loss_match": result["first_step_loss_match"],
            }
        ),
        flush=True,
    )
    return 0 if result["exact"] and loss_match and probe_match else 2


# ---------------------------------------------------------------------------
# Parent helpers


def run_cli(cli, store, *args):
    command = [str(cli), "--store", str(store), *[str(a) for a in args]]
    proc = subprocess.run(command, capture_output=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(
            "weftfs %s failed (%d): %s"
            % (" ".join(str(a) for a in args), proc.returncode, proc.stderr.decode()[:2000])
        )
    return proc


def run_cli_json(cli, store, *args):
    proc = run_cli(cli, store, *args, "--json")
    return json.loads(proc.stdout.decode("utf-8"))


def list_checkpoints(cli, store):
    payload = run_cli_json(cli, store, "checkpoint", "list")
    return {row["name"]: row for row in payload.get("checkpoints", [])}


def verify_checkpoint(cli, store, name):
    try:
        return run_cli_json(cli, store, "checkpoint", "verify", "--name", name)
    except RuntimeError as exc:
        return {"error": str(exc)}


def resume_child(
    config,
    *,
    spec,
    out,
    checkpoint,
    path=None,
    store=None,
    cli=DEFAULT_CLI,
    log=None,
):
    command = [
        sys.executable,
        os.path.abspath(__file__),
        "child-resume",
        "--config",
        config,
        "--spec",
        str(spec),
        "--out",
        str(out),
        "--checkpoint",
        str(checkpoint),
        "--weftfs-cli",
        str(cli),
    ]
    if path is not None:
        command += ["--path", str(path)]
    if store is not None:
        command += ["--store", str(store)]
    proc = subprocess.run(command, capture_output=True, check=False, timeout=900)
    if log is not None:
        with open(log, "a", encoding="utf-8") as handle:
            handle.write(proc.stdout.decode("utf-8", "replace"))
            handle.write(proc.stderr.decode("utf-8", "replace"))
    if proc.returncode != 0:
        raise RuntimeError(
            "resume child failed (%d): %s"
            % (proc.returncode, proc.stderr.decode("utf-8", "replace")[-2000:])
        )
    return json.loads(out.read_text(encoding="utf-8"))


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    index = int(round(fraction * (len(ordered) - 1)))
    return ordered[index]


def median(values):
    values = [value for value in values if value is not None]
    return statistics.median(values) if values else None


def round3(value):
    return None if value is None else round(float(value), 3)


def run_one_loop(args, config, rep, run_dir):
    run_dir.mkdir(parents=True, exist_ok=True)
    child_log = run_dir / "child.log"
    command = [
        sys.executable,
        os.path.abspath(__file__),
        "child-loop",
        "--config",
        config,
        "--run-dir",
        str(run_dir),
        "--steps",
        str(args.steps),
        "--checkpoint-every",
        str(args.checkpoint_every),
        "--weftfs-cli",
        str(args.weftfs_cli),
    ]
    started = time.perf_counter()
    with open(child_log, "wb") as handle:
        proc = subprocess.run(
            command,
            stdout=handle,
            stderr=subprocess.STDOUT,
            check=False,
            timeout=3600,
        )
    wall_ms = (time.perf_counter() - started) * 1000.0
    results_path = run_dir / "results.json"
    if proc.returncode != 0 or not results_path.is_file():
        tail = ""
        try:
            tail = child_log.read_text(encoding="utf-8", errors="replace")[-4000:]
        except OSError:
            pass
        raise RuntimeError(
            "loop child %s rep %d failed (rc=%d)\n%s"
            % (config, rep, proc.returncode, tail)
        )
    result = json.loads(results_path.read_text(encoding="utf-8"))
    result["parent_wall_ms"] = round(wall_ms, 3)
    result["rep"] = rep
    return result


def checkpoint_events(result):
    return [
        event
        for event in result["events"]
        if event.get("event") == "checkpoint_submitted"
    ]


def done_events(result):
    return [
        event for event in result["events"] if event.get("event") == "checkpoint_done"
    ]


def spec_for(run_dir, step):
    return run_dir / ("ckpt-%04d.expected.json" % step)


def verify_run_checkpoints(args, config, rep, run_dir, result):
    """Resume every checkpoint into a fresh process and compare exactly."""
    rows = []
    for event in checkpoint_events(result):
        step = int(event["checkpoint_step"])
        paths = checkpoint_paths(config, run_dir, step)
        out = run_dir / ("resume-%04d.json" % step)
        row = resume_child(
            config,
            spec=spec_for(run_dir, step),
            out=out,
            checkpoint=paths["checkpoint"],
            path=(
                paths.get("artifact")
                if config not in ("weft", "weft-fast")
                else None
            ),
            store=paths.get("store"),
            cli=args.weftfs_cli,
            log=run_dir / "child.log",
        )
        row["rep"] = rep
        rows.append(row)
    return rows


def save_pause_rows(result):
    return [event for event in checkpoint_events(result)]


def summarise_run(result):
    checkpoints = set(
        int(event["checkpoint_step"]) for event in checkpoint_events(result)
    )
    clean = [
        value
        for index, value in enumerate(result["step_ms"], start=1)
        if index not in checkpoints
    ]
    augment_steps = {
        int(event["step"])
        for event in result["events"]
        if event.get("event") == "step" and event.get("augment_ms") is not None
    }
    clean_no_augment = [
        value
        for index, value in enumerate(result["step_ms"], start=1)
        if index not in checkpoints and index not in augment_steps
    ]
    pauses = [event["pause_ms"] for event in checkpoint_events(result)]
    snapshots = [event["snapshot_ms"] for event in checkpoint_events(result)]
    submits = [event["submit_ms"] for event in checkpoint_events(result)]
    completions = [event["completion_ms"] for event in done_events(result)]
    landing = [
        int(event["landed_step"]) - int(event["checkpoint_step"])
        for event in done_events(result)
    ]
    physical = [event["physical_bytes_delta"] for event in done_events(result)]
    waits = [event.get("waited_for_previous_ms", 0.0) for event in checkpoint_events(result)]
    worker_write_ms = [
        float(event["worker"].get("checkpoint_write_ms", 0.0))
        for event in done_events(result)
        if event.get("worker")
    ]
    worker_service_ms = [
        float((event["worker"].get("service") or {}).get("request_ms", 0.0))
        for event in done_events(result)
        if event.get("worker")
    ]
    completions_sorted = [event["completion_ms"] for event in done_events(result)]
    final_done = [
        event["completion_ms"]
        for event in done_events(result)
        if int(event["checkpoint_step"]) == int(result["steps"])
    ]
    augment_sum = sum(
        float(event.get("augment_ms") or 0.0)
        for event in result["events"]
        if event.get("event") == "step"
    )
    step_sum = sum(result["step_ms"])
    final_completion = final_done[0] if final_done else None
    return {
        "steps": result["steps"],
        "checkpoints": len(checkpoints),
        "loop_wall_ms": result["run_wall_ms"],
        "process_wall_ms": result["process_wall_ms"],
        "parent_wall_ms": result.get("parent_wall_ms"),
        "step_sum_ms": round(step_sum, 3),
        "augment_sum_ms": round(augment_sum, 3),
        "final_completion_ms": final_completion,
        "post_loop_tail_ms": round(result["run_wall_ms"] - step_sum, 3),
        "loop_cpu_ms": (result.get("proc") or {}).get("cpu_ms"),
        "step_ms_median_of_run": median(clean),
        "step_ms_p95": percentile(clean_no_augment, 0.95),
        "step_ms_max": max(clean_no_augment) if clean_no_augment else None,
        "checkpoint_step_ms": [
            result["step_ms"][step - 1] for step in sorted(checkpoints)
        ],
        "pause_ms_p50": median(pauses),
        "pause_ms_p95": percentile(pauses, 0.95),
        "pause_ms_max": max(pauses) if pauses else None,
        "snapshot_ms_p50": median(snapshots),
        "submit_ms_p50": median(submits),
        "completion_ms_p50": median(completions),
        "completion_ms_first": completions_sorted[0] if completions_sorted else None,
        "completion_ms_later_p50": (
            median(completions_sorted[1:]) if len(completions_sorted) > 1 else None
        ),
        "completion_ms_p95": percentile(completions, 0.95),
        "completion_ms_max": max(completions) if completions else None,
        "landing_steps_p50": median(landing),
        "physical_bytes_per_checkpoint_p50": median(physical),
        "physical_bytes_total": result["storage_end"]["bytes"],
        "waited_for_previous_ms_max": max(waits) if waits else 0.0,
        "weft_checkpoint_write_ms_p50": median(worker_write_ms) if worker_write_ms else None,
        "weft_service_request_ms_p50": median(worker_service_ms) if worker_service_ms else None,
        "peak_rss_kib": result["peak_rss_kib"],
        "final_state_sha256": result["final_state_sha256"],
        "loss_bits": result["loss_bits"],
    }


def aggregate_config(rows, resume_rows):
    summaries = [row["summary"] for row in rows]

    def values(key):
        return [item[key] for item in summaries if item.get(key) is not None]

    pooled_steps = []
    pooled_pauses = []
    for row in rows:
        result = row["result"]
        checkpoints = set(
            int(event["checkpoint_step"]) for event in checkpoint_events(result)
        )
        augment_steps = {
            int(event["step"])
            for event in result["events"]
            if event.get("event") == "step" and event.get("augment_ms") is not None
        }
        pooled_steps.extend(
            value
            for index, value in enumerate(result["step_ms"], start=1)
            if index not in checkpoints and index not in augment_steps
        )
        pooled_pauses.extend(
            event["pause_ms"] for event in checkpoint_events(result)
        )

    return {
        "reps": len(rows),
        "pooled_steps": len(pooled_steps),
        "step_ms_p50_pooled": median(pooled_steps),
        "step_ms_p95_pooled": percentile(pooled_steps, 0.95),
        "step_ms_max_pooled": max(pooled_steps) if pooled_steps else None,
        "pause_ms_p50_pooled": median(pooled_pauses),
        "pause_ms_p95_pooled": percentile(pooled_pauses, 0.95),
        "pause_ms_max_pooled": max(pooled_pauses) if pooled_pauses else None,
        "loop_wall_ms_median": median(values("loop_wall_ms")),
        "process_wall_ms_median": median(values("process_wall_ms")),
        "step_sum_ms_median": median(values("step_sum_ms")),
        "augment_sum_ms_median": median(values("augment_sum_ms")),
        "final_completion_ms_median": median(values("final_completion_ms")),
        "post_loop_tail_ms_median": median(values("post_loop_tail_ms")),
        "loop_cpu_ms_median": median(values("loop_cpu_ms")),
        "step_ms_p50_median": median(values("step_ms_median_of_run")),
        "step_ms_p95_median": median(values("step_ms_p95")),
        "step_ms_max": max(values("step_ms_max")) if values("step_ms_max") else None,
        "pause_ms_p50": median(values("pause_ms_p50")),
        "pause_ms_max": max(values("pause_ms_max")) if values("pause_ms_max") else None,
        "snapshot_ms_p50": median(values("snapshot_ms_p50")),
        "submit_ms_p50": median(values("submit_ms_p50")),
        "completion_ms_p50": median(values("completion_ms_p50")),
        "completion_ms_first_p50": median(values("completion_ms_first")),
        "completion_ms_later_p50": median(values("completion_ms_later_p50")),
        "completion_ms_p95": median(values("completion_ms_p95")),
        "completion_ms_max": max(values("completion_ms_max")) if values("completion_ms_max") else None,
        "landing_steps_p50": median(values("landing_steps_p50")),
        "physical_bytes_per_checkpoint_p50": median(
            values("physical_bytes_per_checkpoint_p50")
        ),
        "physical_bytes_total_median": median(values("physical_bytes_total")),
        "waited_for_previous_ms_max": max(values("waited_for_previous_ms_max") or [0.0]),
        "weft_checkpoint_write_ms_p50": median(
            values("weft_checkpoint_write_ms_p50")
        ),
        "weft_service_request_ms_p50": median(
            values("weft_service_request_ms_p50")
        ),
        "peak_rss_kib_max": max(values("peak_rss_kib") or [0]),
        "resume_checks": len(resume_rows),
        "resume_exact": sum(1 for row in resume_rows if row["exact"]),
        "resume_loss_match": sum(
            1 for row in resume_rows if row["first_step_loss_match"]
        ),
        "resume_probe_match": sum(1 for row in resume_rows if row["probe_loss_match"]),
        "resume_post_state_match": sum(
            1 for row in resume_rows if row["post_step_state_match"] is True
        ),
        "resume_post_state_checked": sum(
            1 for row in resume_rows if row["post_step_state_match"] is not None
        ),
        "resume_load_ms_p50": median([row["load_ms"] for row in resume_rows]),
        "resume_process_to_first_step_ms_p50": median(
            [row["process_start_to_first_step_ms"] for row in resume_rows]
        ),
        "resume_first_step_ms_p50": median(
            [row["first_step_ms"] for row in resume_rows]
        ),
        "resume_rss_kib_max": max([row["rss_kib"] or 0 for row in resume_rows] or [0]),
    }


def run_kill_scenario(args, scenario, rep, delay_ms):
    work = Path(args.work_dir) / ("kill-%s-%d" % (scenario, rep))
    if work.exists():
        shutil.rmtree(work)
    run_dir = work / "run"
    run_dir.mkdir(parents=True)
    store = run_dir / "store"
    run_cli(args.weftfs_cli, store, "init")
    child_log = run_dir / "child.log"
    command = [
        sys.executable,
        os.path.abspath(__file__),
        "child-loop",
        "--config",
        "weft",
        "--run-dir",
        str(run_dir),
        "--steps",
        str(args.kill_steps),
        "--checkpoint-every",
        str(args.checkpoint_every),
        "--weftfs-cli",
        str(args.weftfs_cli),
    ]
    if scenario == "after_ack":
        command += ["--kill-after-ack-step", str(args.kill_after_ack_step)]
    else:
        command += [
            "--kill-before-ack-step",
            str(args.kill_before_ack_step),
            "--kill-before-ack-delay-ms",
            str(delay_ms),
        ]
    started = time.perf_counter()
    with open(child_log, "wb") as handle:
        proc = subprocess.run(
            command, stdout=handle, stderr=subprocess.STDOUT, check=False, timeout=1800
        )
    wall_before_kill_ms = (time.perf_counter() - started) * 1000.0
    kills = [row for row in read_progress(run_dir / "progress.ndjson") if row.get("event") == "kill"]
    acked = [
        row
        for row in read_progress(run_dir / "progress.ndjson")
        if row.get("event") == "checkpoint_done"
    ]
    if not kills:
        raise RuntimeError(
            "kill scenario %s rep %d produced no kill record (rc=%d)"
            % (scenario, rep, proc.returncode)
        )
    kill = kills[0]
    kill_epoch = kill["epoch"]
    existing = list_checkpoints(args.weftfs_cli, store)
    if scenario == "after_ack":
        ack_checkpoint = (args.kill_after_ack_step // args.checkpoint_every) * args.checkpoint_every
        if ack_checkpoint < args.checkpoint_every:
            ack_checkpoint = args.checkpoint_every
        candidate = "ckpt-%04d" % ack_checkpoint
    else:
        ack_checkpoint = args.kill_before_ack_step
        candidate = "ckpt-%04d" % ack_checkpoint
    acked_steps = sorted({int(row["checkpoint_step"]) for row in acked})
    previous = [step for step in acked_steps if step < ack_checkpoint]
    previous_step = max(previous) if previous else None
    previous_name = "ckpt-%04d" % previous_step if previous_step is not None else None

    target = candidate if candidate in existing else previous_name
    if target is None:
        raise RuntimeError(
            "kill scenario %s rep %d exposed no resumable checkpoint" % (scenario, rep)
        )
    selected = "new" if target == candidate else "old"
    target_step = int(target.split("-")[1])
    # Discovery (listing the store and picking the newest exposed checkpoint) is
    # part of the resume path, so it stays inside time-to-safe-resume. The
    # whole-store verification is evidence, not resume work, and runs after.
    discovery_ms = (time.time() - kill_epoch) * 1000.0
    spawn_epoch = time.time()
    out = run_dir / "resume.json"
    resume = resume_child(
        "weft",
        spec=spec_for(run_dir, target_step),
        out=out,
        checkpoint=target,
        store=store,
        cli=args.weftfs_cli,
        log=run_dir / "child.log",
    )
    time_to_safe_resume_ms = (resume["first_step_epoch"] - kill_epoch) * 1000.0
    kill_to_spawn_ms = (spawn_epoch - kill_epoch) * 1000.0
    verification = verify_checkpoint(args.weftfs_cli, store, target)
    candidate_verify = (
        verify_checkpoint(args.weftfs_cli, store, candidate)
        if candidate != target and candidate in existing
        else None
    )
    full = None
    try:
        full = run_cli_json(args.weftfs_cli, store, "verify", "--full")
    except RuntimeError as exc:
        full = {"error": str(exc)}

    rows = [
        row
        for row in read_progress(run_dir / "progress.ndjson")
        if row.get("event") == "run_done"
    ]
    result = {
        "scenario": scenario,
        "rep": rep,
        "delay_ms": delay_ms,
        "kill_step": kill.get("kill_step") or kill.get("checkpoint_step"),
        "checkpoint_killed_at": ack_checkpoint,
        "child_exit": proc.returncode,
        "wall_before_kill_ms": round(wall_before_kill_ms, 3),
        "acked_checkpoints": acked_steps,
        "exposed": sorted(existing),
        "candidate_present": candidate in existing,
        "candidate_complete": bool(existing.get(candidate, {}).get("complete")),
        "candidate_verify": candidate_verify,
        "resumed_from": target,
        "resumed_from_step": target_step,
        "old_or_new": selected,
        "steps_lost": int(kill.get("kill_step") or kill.get("checkpoint_step")) - target_step,
        "new_checkpoint_verify": verification,
        "full_store_verify": full,
        "resume": resume,
        "time_to_safe_resume_ms": round(time_to_safe_resume_ms, 3),
        "kill_to_spawn_ms": round(kill_to_spawn_ms, 3),
        "discovery_ms": round(discovery_ms, 3),
        "run_done_before_kill": bool(rows),
    }
    shutil.rmtree(work, ignore_errors=True)
    return result


def summarise_kill(rows):
    def values(key):
        return [row[key] for row in rows if row.get(key) is not None]

    return {
        "reps": len(rows),
        "time_to_safe_resume_ms_p50": median(values("time_to_safe_resume_ms")),
        "time_to_safe_resume_ms_p95": percentile(values("time_to_safe_resume_ms"), 0.95),
        "time_to_safe_resume_ms_max": max(values("time_to_safe_resume_ms")) if rows else None,
        "kill_to_spawn_ms_p50": median(values("kill_to_spawn_ms")),
        "discovery_ms_p50": median(values("discovery_ms")),
        "steps_lost_median": median(values("steps_lost")),
        "resumed_from": [row["resumed_from"] for row in rows],
        "old_count": sum(1 for row in rows if row["old_or_new"] == "old"),
        "new_count": sum(1 for row in rows if row["old_or_new"] == "new"),
        "all_exact": all(row["resume"]["exact"] for row in rows),
        "all_loss_match": all(row["resume"]["first_step_loss_match"] for row in rows),
        "all_full_verify_ok": all(
            isinstance(row["full_store_verify"], dict)
            and "error" not in row["full_store_verify"]
            and not row["full_store_verify"].get("problems")
            for row in rows
        ),
        "candidate_present_reps": sum(1 for row in rows if row["candidate_present"]),
        "resume_load_ms_p50": median(
            [row["resume"]["load_ms"] for row in rows]
        ),
    }


# ---------------------------------------------------------------------------
# Staging bound evidence


def store_growth_probe(args):
    """Why a later checkpoint in the same store costs more than the first.

    Three fresh stores save the same container once; one store then saves four
    *distinct* containers in a row. The data written per checkpoint is
    identical; only the accumulated store state differs.
    """
    from weftcodec.dcp import init_store, save_checkpoint_async

    torch = load_torch()
    torch.manual_seed(SEED)
    model = build_model(torch)
    optimizer = make_optimizer(torch, model)
    batches = data_batches(torch)
    for step in range(1, 4):
        train_step(torch, model, optimizer, batches, step)

    root = Path(args.work_dir) / "growth-probe"
    if root.exists():
        shutil.rmtree(root)

    def breakdown(store):
        out = {}
        for name in ("commits", "indexes", "manifests", "objects", "refs"):
            out[name] = dir_bytes(store / name)["bytes"] if (store / name).exists() else 0
        out["total"] = dir_bytes(store)["bytes"]
        return out

    def save(store, name, snapshot):
        started = time.perf_counter()
        handle = save_checkpoint_async(
            snapshot,
            checkpoint_id=name,
            store_root=str(store),
            staging_bytes=STAGING_BYTES,
            weftfs_cli=args.weftfs_cli,
        )
        handle.result()
        completion = (time.perf_counter() - started) * 1000.0
        stats = dict(handle.stats)
        handle.writer.close()
        return completion, stats

    fresh_rows = []
    for index in range(3):
        store = root / ("fresh-%d" % index)
        init_store(store, weftfs_cli=args.weftfs_cli)
        snapshot = capture_state(torch, model, optimizer, 3)
        completion, stats = save(store, "c001", snapshot)
        fresh_rows.append(
            {
                "fresh_store": index,
                "completion_ms": round(completion, 3),
                "checkpoint_write_ms": round(stats.get("checkpoint_write_ms", 0.0), 3),
                "store": breakdown(store),
            }
        )

    store = root / "sequential"
    init_store(store, weftfs_cli=args.weftfs_cli)
    sequential_rows = []
    for index in range(1, 5):
        step = 100 + index
        train_step(torch, model, optimizer, batches, step)
        snapshot = capture_state(torch, model, optimizer, step)
        before = breakdown(store)
        completion, stats = save(store, "c%03d" % index, snapshot)
        after = breakdown(store)
        sequential_rows.append(
            {
                "checkpoint": index,
                "completion_ms": round(completion, 3),
                "checkpoint_write_ms": round(stats.get("checkpoint_write_ms", 0.0), 3),
                "container_bytes": stats.get("container_bytes"),
                "store": after,
                "store_delta_bytes": after["total"] - before["total"],
                "index_delta_bytes": after["indexes"] - before["indexes"],
                "manifest_delta_bytes": after["manifests"] - before["manifests"],
                "object_delta_bytes": after["objects"] - before["objects"],
                "commit_delta_bytes": after["commits"] - before["commits"],
            }
        )

    shutil.rmtree(root, ignore_errors=True)
    return {
        "fresh_store": fresh_rows,
        "sequential": sequential_rows,
        "interpretation": (
            "A checkpoint in a fresh store has a constant completion; repeated "
            "checkpoints in the same store get slower even though the container "
            "and the data bytes per checkpoint are constant. The per-commit "
            "advisory index segment and the commit record are reported above so "
            "the growth can be attributed rather than guessed."
        ),
    }


def staging_bound_check(args):
    from weftcodec.dcp import init_store, save_checkpoint_async

    torch = load_torch()
    torch.manual_seed(SEED)
    model = build_model(torch)
    optimizer = make_optimizer(torch, model)
    batches = data_batches(torch)
    train_step(torch, model, optimizer, batches, 1)
    snapshot = capture_state(torch, model, optimizer, 1)
    largest = max(int(value.numel() * value.element_size()) for value in snapshot.values())
    work = Path(args.work_dir) / "staging-bound"
    if work.exists():
        shutil.rmtree(work)
    store = work / "store"
    init_store(store, weftfs_cli=args.weftfs_cli)
    bounded_error = None
    try:
        handle = save_checkpoint_async(
            snapshot,
            checkpoint_id="too-small",
            store_root=str(store),
            staging_bytes=1024,
            weftfs_cli=args.weftfs_cli,
        )
        handle.result()
        refused = False
        error_class = None
    except BaseException as exc:  # DCP wraps storage errors in a BaseException
        if isinstance(exc, (SystemExit, KeyboardInterrupt)):
            raise
        text = str(exc)
        refused = "StagingLimitExceeded" in text and "staging_bytes bound" in text
        error_class = type(exc).__name__
        bounded_error = text[-800:]
    with_bound = None
    handle = save_checkpoint_async(
        snapshot,
        checkpoint_id="within-bound",
        store_root=str(store),
        staging_bytes=STAGING_BYTES,
        weftfs_cli=args.weftfs_cli,
    )
    handle.result()
    with_bound = dict(handle.stats)
    handle.writer.close()
    shutil.rmtree(work, ignore_errors=True)
    return {
        "largest_tensor_bytes": largest,
        "bound_bytes": 1024,
        "refused": refused,
        "error_class": error_class,
        "error_tail": bounded_error,
        "within_bound_accepted": with_bound is not None,
        "within_bound_staging_peak_bytes": (
            None if with_bound is None else with_bound.get("staging_peak_bytes")
        ),
    }


# ---------------------------------------------------------------------------
# Parent: the whole court


def startup_probe():
    """Honest fresh-process startup cost, measured in the same court."""
    code = (
        "import json,time;"
        "a=time.perf_counter(); import weftcodec.dcp; b=time.perf_counter();"
        "import torch; c=time.perf_counter();"
        "print(json.dumps({'python_start_to_dcp_import_ms':(b-a)*1000.0,"
        "'dcp_import_includes_torch': True,"
        "'torch_import_after_dcp_ms':(c-b)*1000.0}))"
    )
    proc = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, check=False
    )
    if proc.returncode != 0:
        return {
            "error": proc.stderr.decode("utf-8", "replace")[-500:],
            "python_start_to_dcp_import_ms": None,
        }
    try:
        return json.loads(proc.stdout.decode("utf-8").strip().splitlines()[-1])
    except (ValueError, IndexError) as exc:
        return {"error": "%s: %s" % (type(exc).__name__, exc)}


def run(args):
    work = Path(args.work_dir)
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)

    court = {
        "image": os.environ.get("WEFT_COURT_IMAGE", ""),
        "image_id": os.environ.get("WEFT_COURT_IMAGE_ID", ""),
        "kernel": os.environ.get("WEFT_COURT_KERNEL", ""),
        "memory": os.environ.get("WEFT_COURT_MEMORY", ""),
        "cpus": os.environ.get("WEFT_COURT_CPUS", ""),
        "pids": os.environ.get("WEFT_COURT_PIDS", ""),
        "cap_drop": "ALL",
        "no_new_privileges": True,
        "tmpfs": "/tmp:rw,size=1g,nosuid,nodev",
        "docker_server": os.environ.get("WEFT_DOCKER_SERVER", ""),
        "python": sys.version.split()[0],
        "weftfs_cli": str(args.weftfs_cli),
        "revision_weftfs": os.environ.get("WEFT_REPO_WEFTFS", "unknown"),
        "revision_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC", "unknown"),
        "revision_weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO", "unknown"),
        "revision_weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO", "unknown"),
    }

    import torch

    loop = {
        "description": (
            "explicit multi-head-attention transformer block stack (LayerNorm, "
            "GELU MLP, causal attention) trained with AdamW on a deterministic "
            "seeded synthetic next-token dataset"
        ),
        "parameters": int(sum(p.numel() for p in build_model(torch).parameters())),
        "steps": args.steps,
        "checkpoint_every": args.checkpoint_every,
        "checkpoints_per_run": len(range(args.checkpoint_every, args.steps + 1, args.checkpoint_every)),
        "seed": SEED,
        "data": {
            "batches": N_BATCHES,
            "batch": BATCH,
            "sequence": SEQ,
            "vocab": VOCAB,
            "rule": "seq[b,t] = (offset[b] + t) mod vocab; predict the next token",
            "generator": "torch.Generator().manual_seed(SEED ^ 0x5EED5EED)",
        },
        "optimizer": {
            "name": "AdamW",
            "lr": LEARNING_RATE,
            "betas": list(BETAS),
            "weight_decay": WEIGHT_DECAY,
            "eps": EPS,
        },
        "fixed": [
            "torch.manual_seed(%d) before model construction" % SEED,
            "torch.use_deterministic_algorithms(True)",
            "torch.set_num_threads(1) and set_num_interop_threads(1)",
            "OMP/MKL/OPENBLAS/NUMEXPR thread counts forced to 1 before torch import",
            "no dropout, no data augmentation, fixed batch order",
            "single CPU host; no CUDA runtime in the court",
        ],
        "checkpoint": {
            "async": True,
            "snapshot": "clone of parameters, AdamW exp_avg/exp_avg_sq/step and the step counter",
            "staging_bytes": STAGING_BYTES,
            "weft_shard_strategy": "zstd",
            "weft_topology": 1,
            "dcp_no_dist": True,
        },
    }
    torch_version = torch.__version__

    configs = {}
    resume_by_config = {}
    all_rows = {}
    for config in CONFIGS:
        rows = []
        resume_rows = []
        for rep in range(args.reps):
            run_dir = work / ("%s-run-%d" % (config, rep))
            if run_dir.exists():
                shutil.rmtree(run_dir)
            run_dir.mkdir(parents=True)
            if config in ("weft", "weft-fast"):
                run_cli(args.weftfs_cli, run_dir / "store", "init")
            print(
                "run config=%s rep=%d/%d steps=%d every=%d"
                % (config, rep + 1, args.reps, args.steps, args.checkpoint_every),
                flush=True,
            )
            result = run_one_loop(args, config, rep, run_dir)
            summary = summarise_run(result)
            rows.append({"rep": rep, "result": result, "summary": summary})
            checked = verify_run_checkpoints(args, config, rep, run_dir, result)
            resume_rows.extend(checked)
            bad = [row for row in checked if not (row["exact"] and row["first_step_loss_match"] and row["probe_loss_match"])]
            if bad:
                raise RuntimeError(
                    "exactness failure for %s rep %d: %s"
                    % (config, rep, json.dumps(bad[:2], indent=2)[:4000])
                )
            print(
                "  loop_wall=%.1fms step_p50=%.2fms pause_p50=%.2fms "
                "completion_p50=%.1fms bytes=%d resumes=%d/%d"
                % (
                    summary["loop_wall_ms"],
                    summary["step_ms_median_of_run"],
                    summary["pause_ms_p50"],
                    summary["completion_ms_p50"],
                    summary["physical_bytes_total"],
                    sum(1 for row in checked if row["exact"]),
                    len(checked),
                ),
                flush=True,
            )
            shutil.rmtree(run_dir, ignore_errors=True)
        configs[config] = {
            "label": CONFIG_LABEL[config],
            "runs": [
                {
                    "rep": row["rep"],
                    "summary": row["summary"],
                    "checkpoint_events": checkpoint_events(row["result"]),
                    "done_events": done_events(row["result"]),
                }
                for row in rows
            ],
            "summary": aggregate_config(rows, resume_rows),
            "resume_checks": resume_rows,
        }
        all_rows[config] = rows

    # Cross-configuration exactness: same seed/data/steps must give the same
    # loss trajectory and the same final state in all three configurations.
    trajectories = {
        config: all_rows[config][0]["result"]["loss_bits"] for config in CONFIGS
    }
    final_states = {
        config: {
            row["result"]["final_state_sha256"] for row in all_rows[config]
        }
        for config in CONFIGS
    }
    baseline = trajectories["torchsave"]
    identical_trajectory = all(
        trajectories[config] == baseline for config in CONFIGS
    )
    identical_final_state = all(
        final_states[config] == final_states["torchsave"] for config in CONFIGS
    )

    # Kill / resume court (WeftFS only: the store defines old-or-new).
    kill_rows = {}
    for scenario in (
        "after_ack",
        "before_ack_immediate",
        "before_ack_half",
        "before_ack_midflight",
        "before_ack_after_completion",
    ):
        rows = []
        for rep in range(args.kill_reps):
            delay = 0.0
            if scenario in (
                "before_ack_half",
                "before_ack_midflight",
                "before_ack_after_completion",
            ):
                p50 = configs["weft"]["summary"]["completion_ms_p50"] or 0.0
                # Half and near/after the end of a save, so the kill can land on
                # either side of the commit without ever producing a partial
                # checkpoint.
                factor = {
                    "before_ack_half": 0.5,
                    "before_ack_midflight": 0.9,
                    "before_ack_after_completion": 1.15,
                }[scenario]
                delay = max(10.0, factor * float(p50))
            print(
                "kill scenario=%s rep=%d/%d delay_ms=%.1f"
                % (scenario, rep + 1, args.kill_reps, delay),
                flush=True,
            )
            row = run_kill_scenario(args, scenario, rep, delay)
            rows.append(row)
            print(
                "  kill=%s resumed_from=%s step=%d TTSR=%.1fms exact=%s"
                % (
                    row["scenario"],
                    row["resumed_from"],
                    row["resumed_from_step"],
                    row["time_to_safe_resume_ms"],
                    row["resume"]["exact"],
                ),
                flush=True,
            )
        kill_rows[scenario] = {"rows": rows, "summary": summarise_kill(rows)}
        if not kill_rows[scenario]["summary"]["all_exact"]:
            raise RuntimeError("kill/resume exactness failure in %s" % scenario)
        if not kill_rows[scenario]["summary"]["all_full_verify_ok"]:
            raise RuntimeError("full verify failed after kill in %s" % scenario)

    staging = staging_bound_check(args)
    startup = startup_probe()
    growth = store_growth_probe(args)

    # Comparisons (every loss is named plainly in the receipt and the results doc).
    a = configs["torchsave"]["summary"]
    b = configs["dcp"]["summary"]
    c = configs["weft"]["summary"]
    comparisons = {
        "weft_loop_wall_vs_torchsave_ratio": (
            None if not a["loop_wall_ms_median"] else c["loop_wall_ms_median"] / a["loop_wall_ms_median"]
        ),
        "weft_loop_wall_vs_dcp_ratio": (
            None if not b["loop_wall_ms_median"] else c["loop_wall_ms_median"] / b["loop_wall_ms_median"]
        ),
        "weft_pause_vs_dcp_ratio": (
            None if not b["pause_ms_p50"] else c["pause_ms_p50"] / b["pause_ms_p50"]
        ),
        "weft_pause_vs_torchsave_ratio": (
            None if not a["pause_ms_p50"] else c["pause_ms_p50"] / a["pause_ms_p50"]
        ),
        "weft_completion_vs_dcp_ratio": (
            None if not b["completion_ms_p50"] else c["completion_ms_p50"] / b["completion_ms_p50"]
        ),
        "weft_bytes_per_checkpoint_vs_torchsave_ratio": (
            None
            if not a["physical_bytes_per_checkpoint_p50"]
            else c["physical_bytes_per_checkpoint_p50"]
            / a["physical_bytes_per_checkpoint_p50"]
        ),
        "weft_bytes_per_checkpoint_vs_dcp_ratio": (
            None
            if not b["physical_bytes_per_checkpoint_p50"]
            else c["physical_bytes_per_checkpoint_p50"]
            / b["physical_bytes_per_checkpoint_p50"]
        ),
        "weft_resume_load_vs_dcp_ratio": (
            None
            if not b["resume_load_ms_p50"]
            else c["resume_load_ms_p50"] / b["resume_load_ms_p50"]
        ),
    }

    losses = []
    def add_loss(text):
        losses.append(text)

    if comparisons["weft_loop_wall_vs_torchsave_ratio"] and comparisons["weft_loop_wall_vs_torchsave_ratio"] > 1.0:
        add_loss(
            "WeftFS total loop wall is %.2fx the torch.save file baseline "
            "(%.1f ms vs %.1f ms)."
            % (
                comparisons["weft_loop_wall_vs_torchsave_ratio"],
                c["loop_wall_ms_median"],
                a["loop_wall_ms_median"],
            )
        )
    if comparisons["weft_loop_wall_vs_dcp_ratio"] and comparisons["weft_loop_wall_vs_dcp_ratio"] > 1.0:
        add_loss(
            "WeftFS total loop wall is %.2fx the local PyTorch DCP baseline "
            "(%.1f ms vs %.1f ms)."
            % (
                comparisons["weft_loop_wall_vs_dcp_ratio"],
                c["loop_wall_ms_median"],
                b["loop_wall_ms_median"],
            )
        )
    if comparisons["weft_pause_vs_dcp_ratio"] and comparisons["weft_pause_vs_dcp_ratio"] > 1.0:
        add_loss(
            "WeftFS synchronisation pause is %.2fx the DCP pause (%.2f ms vs "
            "%.2f ms); the pause is the state clone plus submit, not the encode."
            % (
                comparisons["weft_pause_vs_dcp_ratio"],
                c["pause_ms_p50"],
                b["pause_ms_p50"],
            )
        )
    if comparisons["weft_completion_vs_dcp_ratio"] and comparisons["weft_completion_vs_dcp_ratio"] > 1.0:
        add_loss(
            "A WeftFS checkpoint completion is %.2fx the local DCP completion "
            "(%.1f ms vs %.1f ms); it does not stall the loop but it needs a "
            "longer checkpoint interval to stay off the next checkpoint's path."
            % (
                comparisons["weft_completion_vs_dcp_ratio"],
                c["completion_ms_p50"],
                b["completion_ms_p50"],
            )
        )
    if comparisons["weft_bytes_per_checkpoint_vs_dcp_ratio"] and comparisons["weft_bytes_per_checkpoint_vs_dcp_ratio"] > 1.0:
        add_loss(
            "WeftFS writes %.2fx the physical bytes per checkpoint of local DCP "
            "(%d vs %d bytes, including index segments)."
            % (
                comparisons["weft_bytes_per_checkpoint_vs_dcp_ratio"],
                c["physical_bytes_per_checkpoint_p50"],
                b["physical_bytes_per_checkpoint_p50"],
            )
        )
    if comparisons["weft_resume_load_vs_dcp_ratio"] and comparisons["weft_resume_load_vs_dcp_ratio"] > 1.0:
        add_loss(
            "WeftFS checkpoint load on resume is %.2fx the local DCP load "
            "(%.1f ms vs %.1f ms)."
            % (
                comparisons["weft_resume_load_vs_dcp_ratio"],
                c["resume_load_ms_p50"],
                b["resume_load_ms_p50"],
            )
        )
    if a["completion_ms_p50"]:
        add_loss(
            "A WeftFS checkpoint completion is %.0fx the torch.save worker "
            "(%.1f ms vs %.1f ms); the plain local file baseline is much "
            "further ahead than DCP."
            % (
                c["completion_ms_p50"] / a["completion_ms_p50"],
                c["completion_ms_p50"],
                a["completion_ms_p50"],
            )
        )
    if c["step_ms_max"] and a["step_ms_max"] and c["step_ms_max"] > 2 * a["step_ms_max"]:
        add_loss(
            "The worst single WeftFS training step was %.1f ms versus %.1f ms "
            "and %.1f ms for the baselines; the checkpoint *submit* path is "
            "not that stall (pause max %.2f ms), so it is host scheduling or "
            "background encode contention and is reported, not explained away."
            % (
                c["step_ms_max"],
                a["step_ms_max"],
                b["step_ms_max"],
                c["pause_ms_max"],
            )
        )
    ttsr = [
        row["summary"]["time_to_safe_resume_ms_p50"]
        for row in kill_rows.values()
        if row["summary"].get("time_to_safe_resume_ms_p50") is not None
    ]
    disc = [
        row["summary"]["discovery_ms_p50"]
        for row in kill_rows.values()
        if row["summary"].get("discovery_ms_p50") is not None
    ]
    if c["waited_for_previous_ms_max"]:
        add_loss(
            "At least one WeftFS checkpoint had to wait %.1f ms for the previous "
            "save because the interval was shorter than the completion."
            % c["waited_for_previous_ms_max"]
        )
    add_loss(
        "The kill court always loses the steps after the last acked checkpoint; "
        "the measured loss is the median steps_lost per scenario, not zero."
    )
    growth_first = growth["sequential"][0]["completion_ms"] if growth["sequential"] else None
    growth_later = median(
        [row["completion_ms"] for row in growth["sequential"][1:]]
    )
    growth_fresh = median([row["completion_ms"] for row in growth["fresh_store"]])
    if growth_first and growth_later:
        add_loss(
            "A checkpoint in a fresh store completes in %.1f ms (fresh-store "
            "control median %.1f ms); the second and later checkpoints in the "
            "same store take %.1f ms for the same 4.99 MB container. The "
            "per-checkpoint deltas are constant (manifest ~%.2f MB, objects "
            "~%.2f MB, commit ~%.2f MB, advisory index ~%.0f KB), so the growth "
            "is store-state bookkeeping, measured in store_growth_probe and "
            "not localised further here."
            % (
                growth_first,
                growth_fresh or 0.0,
                growth_later,
                growth["sequential"][0]["manifest_delta_bytes"] / 1e6,
                growth["sequential"][0]["object_delta_bytes"] / 1e6,
                growth["sequential"][0]["commit_delta_bytes"] / 1e6,
                growth["sequential"][0]["index_delta_bytes"] / 1e3,
            )
        )

    non_claims = [
        "Single CPU host inside the Docker python court; there is no GPU in the "
        "image and no GPU-direct, NCCL, or multi-rank claim.",
        "The dataset is deterministic synthetic next-token data, not a public "
        "corpus; the model is a 0.4M-parameter transformer stack, not a "
        "production model.",
        "Kill tests are SIGKILL process faults against a local store, not "
        "physical media power loss or host-cache loss.",
        "A checkpoint interval shorter than the async completion requires the "
        "next checkpoint to wait for the previous save; this court measures "
        "that wait but does not claim a queueing policy.",
        "The store is single-writer (MW1); no multi-writer training claim.",
        "torch.save and local DCP are single-host local files; no distributed "
        "checkpoint or cloud backend was exercised.",
        "Wall-clock medians are from one host and warm page cache after the "
        "first repetition; no fleet or energy claim.",
        "time-to-safe-resume includes fresh-process startup (CPython, the "
        "adapter import and torch); the measured store share is the load, and "
        "the startup share is reported separately.",
    ]

    wins = [
        "Every one of the %d checkpoints in the %d normal runs was restored by "
        "a separate process and matched the in-process copy exactly: %d/%d "
        "state comparisons, %d/%d next-step loss bits, %d/%d post-step states."
        % (
            sum(configs[config]["summary"]["resume_checks"] for config in CONFIGS),
            args.reps * len(CONFIGS),
            sum(configs[config]["summary"]["resume_exact"] for config in CONFIGS),
            sum(configs[config]["summary"]["resume_checks"] for config in CONFIGS),
            sum(configs[config]["summary"]["resume_loss_match"] for config in CONFIGS),
            sum(configs[config]["summary"]["resume_checks"] for config in CONFIGS),
            sum(configs[config]["summary"]["resume_post_state_match"] for config in CONFIGS),
            sum(configs[config]["summary"]["resume_post_state_checked"] for config in CONFIGS),
        ),
        "The training thread never runs the encoder: the WeftFS pause is "
        "%.2f ms p50 and %.2f ms max, and contains only the state clone and "
        "the async submit."
        % (c["pause_ms_p50"], c["pause_ms_max"]),
        "Time to safe resume was %.0f-%.0f ms (p50 per kill scenario, three "
        "reps each) and every resumed state was bit-exact. Of that, the "
        "checkpoint load is %.1f ms and store discovery is %.1f-%.1f ms; the "
        "rest is fresh-process startup, whose adapter+torch import alone is "
        "%s ms in this court."
        % (
            min(ttsr) if ttsr else 0.0,
            max(ttsr) if ttsr else 0.0,
            c["resume_load_ms_p50"],
            min(disc) if disc else 0.0,
            max(disc) if disc else 0.0,
            ("%.0f" % startup["python_start_to_dcp_import_ms"])
            if startup.get("python_start_to_dcp_import_ms")
            else "unknown",
        ),
        "WeftFS writes %.2f%% fewer physical bytes per checkpoint than torch.save "
        "and %.2f%% fewer than local DCP (zstd shards and one commit per "
        "checkpoint)."
        % (
            100.0
            * (1.0 - c["physical_bytes_per_checkpoint_p50"] / a["physical_bytes_per_checkpoint_p50"]),
            100.0
            * (1.0 - c["physical_bytes_per_checkpoint_p50"] / b["physical_bytes_per_checkpoint_p50"]),
        ),
        "All kill scenarios resumed at a complete checkpoint; the store never "
        "exposed a partial one, and every resumed state was exact.",
    ]

    receipt = {
        "receipt": SCHEMA_RECEIPT,
        "generated_at_utc": utc_now(),
        "stamp": stamp(),
        "label": (
            "Real CPU-only PyTorch training loop with asynchronous checkpoints; "
            "A/B/C comparison, fresh-process exactness, and SIGKILL resume. "
            "Synthetic seeded model/data, single host, no GPU, no fleet claim."
        ),
        "court": court,
        "torch_version": torch_version,
        "loop": loop,
        "configs": configs,
        "resume_exactness": {
            "total": sum(configs[config]["summary"]["resume_checks"] for config in CONFIGS),
            "exact": sum(configs[config]["summary"]["resume_exact"] for config in CONFIGS),
            "loss_match": sum(
                configs[config]["summary"]["resume_loss_match"] for config in CONFIGS
            ),
            "probe_match": sum(
                configs[config]["summary"]["resume_probe_match"] for config in CONFIGS
            ),
            "post_state_checked": sum(
                configs[config]["summary"]["resume_post_state_checked"]
                for config in CONFIGS
            ),
            "post_state_match": sum(
                configs[config]["summary"]["resume_post_state_match"]
                for config in CONFIGS
            ),
            "note": (
                "Every checkpoint of every normal run was loaded by a separate "
                "process. 'probe' is the no-grad next-step loss recorded at "
                "snapshot time; 'loss' is the actual next-step loss and the "
                "post-step state covers the optimizer step itself."
            ),
        },
        "cross_config": {
            "identical_loss_trajectory": identical_trajectory,
            "identical_final_state": identical_final_state,
            "runs_compared": {config: len(all_rows[config]) for config in CONFIGS},
        },
        "kill_resume": kill_rows,
        "staging_bound": staging,
        "resume_startup_probe": startup,
        "store_growth_probe": growth,
        "comparisons": {
            key: round3(value) for key, value in comparisons.items()
        },
        "losses": losses,
        "wins": wins,
        "non_claims": non_claims,
        "method": {
            "pause_ms": "state clone + async submit on the training thread",
            "completion_ms": "submit to the save worker's completion callback",
            "landing_step": "first training step that observes the completed save",
            "step_ms": "training step wall time including the checkpoint pause on checkpoint steps and the post-checkpoint harness state verification on the next step; medians exclude both of those steps",
            "step_sum_ms": "sum of every step timer; comparable across configurations because the harness work is identical",
            "augment_sum_ms": "the part of step_sum spent cloning and hashing the state after a checkpoint (harness verification, not training)",
            "post_loop_tail_ms": "loop wall after the last step: the final in-flight checkpoint has no later step to overlap",
            "time_to_safe_resume_ms": "SIGKILL epoch to the first completed optimizer step in a fresh process, including store discovery and fresh-process startup",
            "wall": "loop wall from before step 1 to after the last save completes",
        },
        "commands": [
            "scripts/docker-court.sh python \"PYTHONPATH=/work/python python3 python/courts/training_goodput.py run --receipt <file>\"",
        ],
    }
    write_json(Path(args.receipt), receipt)
    print("receipt", args.receipt, flush=True)
    print(
        json.dumps(
            {
                "configs": {
                    config: {
                        "wall_ms": configs[config]["summary"]["loop_wall_ms_median"],
                        "step_p50_ms": configs[config]["summary"]["step_ms_p50_median"],
                        "pause_p50_ms": configs[config]["summary"]["pause_ms_p50"],
                        "completion_p50_ms": configs[config]["summary"]["completion_ms_p50"],
                        "bytes_per_ckpt": configs[config]["summary"]["physical_bytes_per_checkpoint_p50"],
                        "rss_kib": configs[config]["summary"]["peak_rss_kib_max"],
                    }
                    for config in CONFIGS
                },
                "resume_exactness": {
                    "total": sum(
                        configs[config]["summary"]["resume_checks"]
                        for config in CONFIGS
                    ),
                    "exact": sum(
                        configs[config]["summary"]["resume_exact"]
                        for config in CONFIGS
                    ),
                },
                "kill": {
                    name: kill_rows[name]["summary"] for name in kill_rows
                },
            },
            indent=2,
        ),
        flush=True,
    )
    shutil.rmtree(work, ignore_errors=True)
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    run_parser = sub.add_parser("run", help="run the whole court")
    run_parser.add_argument("--receipt", default=str(REPO_ROOT / "bench" / "receipts" / ("training-goodput-%s.json" % stamp())))
    run_parser.add_argument("--work-dir", default="/tmp/weft-training-goodput")
    run_parser.add_argument("--steps", type=int, default=DEFAULT_STEPS)
    run_parser.add_argument("--checkpoint-every", type=int, default=DEFAULT_EVERY)
    run_parser.add_argument("--reps", type=int, default=DEFAULT_REPS)
    run_parser.add_argument("--kill-reps", type=int, default=DEFAULT_KILL_REPS)
    run_parser.add_argument("--kill-steps", type=int, default=160)
    run_parser.add_argument("--kill-after-ack-step", type=int, default=140)
    run_parser.add_argument("--kill-before-ack-step", type=int, default=128)
    run_parser.add_argument("--weftfs-cli", default=os.environ.get("WEFTFS_CLI", DEFAULT_CLI))
    run_parser.set_defaults(func=run)

    loop_parser = sub.add_parser("child-loop", help=argparse.SUPPRESS)
    loop_parser.add_argument("--config", choices=CONFIGS, required=True)
    loop_parser.add_argument("--run-dir", required=True)
    loop_parser.add_argument("--steps", type=int, required=True)
    loop_parser.add_argument("--checkpoint-every", type=int, required=True)
    loop_parser.add_argument("--weftfs-cli", default=DEFAULT_CLI)
    loop_parser.add_argument("--kill-after-ack-step", type=int, default=None)
    loop_parser.add_argument("--kill-before-ack-step", type=int, default=None)
    loop_parser.add_argument("--kill-before-ack-delay-ms", type=float, default=0.0)
    loop_parser.set_defaults(func=child_loop)

    resume_parser = sub.add_parser("child-resume", help=argparse.SUPPRESS)
    resume_parser.add_argument("--config", choices=CONFIGS, required=True)
    resume_parser.add_argument("--spec", required=True)
    resume_parser.add_argument("--out", required=True)
    resume_parser.add_argument("--checkpoint", required=True)
    resume_parser.add_argument("--path", default=None)
    resume_parser.add_argument("--store", default=None)
    resume_parser.add_argument("--weftfs-cli", default=DEFAULT_CLI)
    resume_parser.set_defaults(func=child_resume)

    staging_parser = sub.add_parser("staging-bound", help=argparse.SUPPRESS)
    staging_parser.add_argument("--work-dir", default="/tmp/weft-training-goodput")
    staging_parser.add_argument("--weftfs-cli", default=DEFAULT_CLI)
    staging_parser.set_defaults(func=lambda args: (print(json.dumps(staging_bound_check(args), indent=2)), 0)[1])

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
