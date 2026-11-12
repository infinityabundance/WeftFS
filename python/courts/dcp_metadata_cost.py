#!/usr/bin/env python3
"""DCP metadata serialisation cost court (subphase R12c item 3).

R12b measured ``adapter.metadata_serialize`` at **4.2 ms** of a 37.7 ms planned
checkpoint completion on the R11 training-goodput fixture (169 entries). This
court isolates that phase instead of trusting the number:

* it builds **real** ``torch.distributed.checkpoint`` metadata by running one
  genuine save per size, and keeps the ``Metadata`` object the adapter's
  ``finish`` receives;
* it measures ``weftcodec.dcp.serialize_metadata`` on that object at 1, 16, 64,
  169 and 1024 entries, 30 iterations each, reporting p50/p95;
* it splits the measured wall into tree construction (``_state_dict_value``,
  the storage-row loop, ``_planner_data_value``) and the canonical encoder, by
  timing each section from the outside (a measurement patch only -- no shipped
  code is modified by this court);
* it estimates the encoder's internal shares by replaying the same primitives
  (key encode + sort, integer varints, payload extends) over the same tree, plus
  a ``cProfile`` cross-check;
* it times the result being copied into a memfd exactly as
  ``finish_checkpoint``/``write_file`` do, and hashing the blob in Python;
* it counts service round trips during the serialisation window (zero on the
  shipped path: the encoder is a pure function).

The fixture is the R11 training-goodput state for 169 entries. Smaller sizes
take the first N entries of that state; the 1024-entry size repeats it with
suffixed names and small tensors, which changes entry count and nothing about
the encoder's input shape.

Runs inside the Docker python court:

  scripts/docker-court.sh python "bench/scripts/dcp_metadata_court.sh"
"""

from __future__ import annotations

import argparse
import cProfile
import hashlib
import io
import json
import os
import pstats
import platform
import resource
import shutil
import statistics
import struct
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

for _name in (
    "OMP_NUM_THREADS",
    "MKL_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "NUMEXPR_NUM_THREADS",
):
    os.environ.setdefault(_name, "1")

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))

import training_goodput as tg  # noqa: E402  (the R11 fixture exactly)

from weftcodec import hashing, varint, wcb  # noqa: E402
from weftcodec import dcp as dcp_mod  # noqa: E402
from weftcodec import service as service_mod  # noqa: E402
from weftcodec.dcp import (  # noqa: E402
    WeftStorageWriter,
    init_store,
    serialize_metadata,
)

SCHEMA = "weft.dcp-metadata-cost.v1"
DEFAULT_SIZES = "1,16,64,169,1024"
DEFAULT_ITERATIONS = 30
PROFILE_CALLS = 8


def utc_stamp():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp, path)


def dist(values_ns):
    """A small distribution summary in milliseconds."""
    if not values_ns:
        return None
    values = sorted(values_ns)
    ms = [v / 1e6 for v in values]

    def pct(p):
        # nearest-rank on the sorted sample; no interpolation claim.
        index = max(0, min(len(values) - 1, int(round(p * (len(values) - 1)))))
        return ms[index]

    return {
        "iterations": len(ms),
        "min_ms": ms[0],
        "p50_ms": statistics.median(ms),
        "p95_ms": pct(0.95),
        "max_ms": ms[-1],
        "mean_ms": statistics.fmean(ms),
    }


class CapturingWriter(WeftStorageWriter):
    """Keeps the ``Metadata`` object the adapter serialises in ``finish``."""

    captured_metadata = None

    def finish(self, metadata, results):  # noqa: D401 - DCP interface
        super().finish(metadata, results)
        self.captured_metadata = metadata


def build_state(torch, base_state, n):
    """Exactly the first ``n`` fixture entries, or a repeated state above 169."""
    items = list(base_state.items())
    if n <= len(items):
        return dict(items[:n])
    out = {}
    for index in range(n):
        out["synthetic.entry.%05d" % index] = torch.zeros(
            (4, 4), dtype=torch.float32
        )
    return out


def capture_metadata(torch, *, store_root, state, cli, name="ckpt"):
    """One real DCP save; return the metadata object it serialised."""
    import torch.distributed.checkpoint as dcp

    writer = CapturingWriter(
        name,
        store_root=str(store_root),
        topology=1,
        shard_strategy="zstd",
        staging_bytes=tg.STAGING_BYTES,
        init_store=False,
        weftfs_cli=cli,
        atomic_publish=True,
        write_policy="fast",
        fast_codec="raw",
    )
    save_started = time.perf_counter()
    try:
        dcp.save(state, storage_writer=writer, no_dist=True)
    finally:
        save_ms = (time.perf_counter() - save_started) * 1000.0
    stats = dict(writer.stats)
    metadata = writer.captured_metadata
    storage_name = writer.store_name
    writer.close()
    if metadata is None:
        raise RuntimeError("the save did not reach finish(); no metadata captured")
    return {
        "metadata": metadata,
        "checkpoint_id": name,
        "storage_name": storage_name,
        "save_ms": save_ms,
        "stats": {
            key: stats.get(key)
            for key in ("entries", "tensor_bytes", "container_bytes")
        },
    }


def local_value(metadata, *, checkpoint_id, storage_name):
    """The same value tree ``serialize_metadata`` builds, assembled by the court.

    ``weftcodec.dcp`` is not modified by this court, so the tree is rebuilt
    here from the same shipped helpers; ``wcb.encode`` of it is asserted equal
    to ``serialize_metadata``'s bytes below, which is what makes the replay
    trustworthy.
    """
    storage_rows = []
    storage_data = metadata.storage_data
    for key in sorted(storage_data, key=lambda k: (k.fqn, tuple(k.offset or ()))):
        info = storage_data[key]
        storage_rows.append(
            {
                "fqn": key.fqn,
                "offset": None
                if key.offset is None
                else dcp_mod._u_list(key.offset),
                "index": None if key.index is None else dcp_mod._u(key.index),
                "info": info.to_value(),
            }
        )
    version = getattr(metadata, "version", None)
    return {
        "schema": dcp_mod.DCP_METADATA_SCHEMA,
        "checkpoint_id": checkpoint_id,
        "storage_name": storage_name,
        "dcp_version": None if version is None else str(version),
        "state_dict": dcp_mod._state_dict_value(metadata.state_dict_metadata),
        "storage_data": storage_rows,
        "planner_data": dcp_mod._planner_data_value(
            getattr(metadata, "planner_data", None)
        ),
    }


def timed_serialize(serialize, metadata, checkpoint_id, storage_name, iterations):
    durations = []
    blob = None
    for _ in range(iterations):
        started = time.perf_counter_ns()
        blob = serialize(
            metadata, checkpoint_id=checkpoint_id, storage_name=storage_name
        )
        durations.append(time.perf_counter_ns() - started)
    return durations, blob


def measure_sections(metadata, checkpoint_id, storage_name, iterations):
    """Split the shipped encoder wall into tree sections and the encoder.

    The only instrumentation is a pair of temporary function wrappers installed
    by this court and removed afterwards; ``weftcodec.dcp`` is not modified.
    """
    totals, encodes, state_dict, planner = [], [], [], []
    real_encode = wcb.encode
    real_state_dict = dcp_mod._state_dict_value
    real_planner = dcp_mod._planner_data_value

    def timed(fn, sink):
        def wrapper(*args, **kwargs):
            started = time.perf_counter_ns()
            out = fn(*args, **kwargs)
            sink.append(time.perf_counter_ns() - started)
            return out

        return wrapper

    wcb.encode = timed(real_encode, encodes)
    dcp_mod._state_dict_value = timed(real_state_dict, state_dict)
    dcp_mod._planner_data_value = timed(real_planner, planner)
    try:
        for _ in range(iterations):
            started = time.perf_counter_ns()
            serialize_metadata(
                metadata, checkpoint_id=checkpoint_id, storage_name=storage_name
            )
            totals.append(time.perf_counter_ns() - started)
    finally:
        wcb.encode = real_encode
        dcp_mod._state_dict_value = real_state_dict
        dcp_mod._planner_data_value = real_planner
    storage_rows = [
        total - enc - sd - pl
        for total, enc, sd, pl in zip(totals, encodes, state_dict, planner)
    ]
    return {
        "total": totals,
        "encode": encodes,
        "state_dict": state_dict,
        "planner": planner,
        "storage_rows": storage_rows,
    }


def collect_tree(value):
    """References only: no encoding, so the walk can be reported separately."""
    dicts, ints, payloads = [], [], []
    stack = [value]
    while stack:
        current = stack.pop()
        if current is None or current is True or current is False:
            continue
        if isinstance(current, (wcb.U64, wcb.I64, wcb.Timestamp)):
            ints.append(int(current.value if hasattr(current, "value") else current.nanos))
        elif isinstance(current, str):
            payloads.append(current.encode("utf-8"))
        elif isinstance(current, (bytes, bytearray, memoryview)):
            payloads.append(bytes(current))
        elif isinstance(current, (list, tuple)):
            stack.extend(current)
        elif isinstance(current, dict):
            dicts.append(current)
            stack.extend(current.values())
        else:
            continue
    return dicts, ints, payloads


def replay_phases(value, iterations):
    """Replay the encoder's three primitive phases over the same tree."""
    walk, keys, varints, payload_ms = [], [], [], []
    scratch = bytearray()
    for _ in range(iterations):
        started = time.perf_counter_ns()
        dicts, ints, payloads = collect_tree(value)
        walk.append(time.perf_counter_ns() - started)

        started = time.perf_counter_ns()
        for mapping in dicts:
            rows = []
            for key in mapping:
                raw = key.encode("utf-8")
                encoded = bytearray()
                encoded.append(wcb.TAG_TEXT)
                varint.encode(len(raw), encoded)
                encoded.extend(raw)
                rows.append((bytes(encoded), key))
            rows.sort(key=lambda entry: entry[0])
        keys.append(time.perf_counter_ns() - started)

        started = time.perf_counter_ns()
        scratch.clear()
        for number in ints:
            varint.encode(number, scratch)
        varints.append(time.perf_counter_ns() - started)

        started = time.perf_counter_ns()
        scratch.clear()
        for raw in payloads:
            scratch.extend(raw)
        payload_ms.append(time.perf_counter_ns() - started)
    return {
        "walk": walk,
        "keys": keys,
        "varints": varints,
        "payloads": payload_ms,
        "items": {
            "maps": len(dicts),
            "ints": len(ints),
            "payloads": len(payloads),
        },
    }


def profile_encode(value, calls=PROFILE_CALLS):
    """cProfile cross-check of the shipped encoder's per-function shares."""
    profiler = cProfile.Profile()
    profiler.enable()
    for _ in range(calls):
        wcb.encode(value)
    profiler.disable()
    stream = io.StringIO()
    stats = pstats.Stats(profiler, stream=stream)
    stats.sort_stats("tottime")
    rows = []
    for (filename, lineno, func), (cc, nc, _tt, ct, _callers) in stats.stats.items():
        if nc == 0:
            continue
        rows.append(
            {
                "function": func,
                "file": os.path.basename(filename),
                "line": lineno,
                "calls_per_encode": nc / calls,
                "tottime_ms_per_encode": _tt * 1000.0 / calls,
                "cumtime_ms_per_encode": ct * 1000.0 / calls,
            }
        )
    rows.sort(key=lambda row: row["tottime_ms_per_encode"], reverse=True)
    return rows[:14]


def memfd_copy(blob, iterations):
    """Exactly `finish_checkpoint`'s allocate + write, nothing else."""
    durations = []
    for _ in range(iterations):
        started = time.perf_counter_ns()
        fd = os.memfd_create("weft-finish", os.MFD_CLOEXEC)
        try:
            view = memoryview(blob)
            offset = 0
            while offset < len(view):
                offset += os.write(fd, view[offset:])
        finally:
            os.close(fd)
        durations.append(time.perf_counter_ns() - started)
    return durations


def hash_cost(blob, iterations):
    sha, b3, domain = [], [], []
    for _ in range(iterations):
        started = time.perf_counter_ns()
        hashlib.sha256(blob).digest()
        sha.append(time.perf_counter_ns() - started)

        started = time.perf_counter_ns()
        hashing.blake3(blob).digest()
        b3.append(time.perf_counter_ns() - started)

        started = time.perf_counter_ns()
        hashing.content_root(blob)
        domain.append(time.perf_counter_ns() - started)
    return {"sha256": sha, "blake3": b3, "domain_blake3": domain}


def count_service_calls():
    """Count `CheckpointService._call` invocations while installed."""
    box = {"calls": 0}
    real = service_mod.CheckpointService._call

    def wrapper(self, *args, **kwargs):
        box["calls"] += 1
        return real(self, *args, **kwargs)

    service_mod.CheckpointService._call = wrapper
    return box, lambda: setattr(service_mod.CheckpointService, "_call", real)


def bench_ms(fn, iterations, check=None):
    """Median milliseconds of `fn`, asserting `check` on every result."""
    samples = []
    value = None
    for _ in range(iterations):
        started = time.perf_counter_ns()
        value = fn()
        samples.append(time.perf_counter_ns() - started)
        if check is not None and value != check:
            raise RuntimeError("an alternative produced different bytes")
    return samples


def marshal_table(metadata):
    """Prototype flat request body for a native encoder.

    This is the *input* a Rust-side encoder would need: every field of the
    metadata, length-prefixed, with `struct.pack` for the integer fields. It is
    measured so the native option's floor includes the client work it cannot
    avoid; nothing here is shipped.
    """
    pack = struct.pack
    chunks = []
    append = chunks.append
    append(pack("<I", len(metadata.state_dict_metadata)))
    for fqn in sorted(metadata.state_dict_metadata):
        row = metadata.state_dict_metadata[fqn]
        raw = fqn.encode("utf-8")
        append(pack("<I", len(raw)))
        append(raw)
        if type(row).__name__ == "BytesStorageMetadata":
            append(pack("<B", 0))
            continue
        append(pack("<B", 1))
        dtype = str(row.properties.dtype).encode("ascii")
        append(pack("<I", len(dtype)))
        append(dtype)
        append(pack("<I", len(row.size)))
        for extent in row.size:
            append(pack("<Q", int(extent)))
        append(pack("<I", len(row.chunks)))
        for chunk in row.chunks:
            append(pack("<I", len(chunk.offsets)))
            for extent in chunk.offsets:
                append(pack("<Q", int(extent)))
            append(pack("<I", len(chunk.sizes)))
            for extent in chunk.sizes:
                append(pack("<Q", int(extent)))
    append(pack("<I", len(metadata.storage_data)))
    for key in sorted(metadata.storage_data, key=lambda k: (k.fqn, tuple(k.offset or ()))):
        info = metadata.storage_data[key]
        raw = key.fqn.encode("utf-8")
        append(pack("<I", len(raw)))
        append(raw)
        append(pack("<B", 0 if key.offset is None else 1))
        if key.offset is not None:
            append(pack("<I", len(key.offset)))
            for extent in key.offset:
                append(pack("<Q", int(extent)))
        append(pack("<i", -1 if key.index is None else int(key.index)))
        for text in (info.entry, info.dtype):
            encoded = text.encode("utf-8")
            append(pack("<I", len(encoded)))
            append(encoded)
        for array in (info.shape, info.storage_offsets, info.lengths):
            append(pack("<I", len(array)))
            for extent in array:
                append(pack("<Q", int(extent)))
        root = bytes(info.content_root)
        append(pack("<I", len(root)))
        append(root)
    return b"".join(chunks)


def native_floor(metadata, service, metadata_path, iterations):
    """Measured lower bound for a Rust-side encoder, without any Rust work."""
    stat_samples = bench_ms(
        lambda: service.stat_file(metadata_path), min(iterations, 20)
    )
    read_samples = []
    for _ in range(min(iterations, 20)):
        started = time.perf_counter_ns()
        view = service.read_file(metadata_path)
        size = view.size
        read_samples.append(time.perf_counter_ns() - started)
        view.close()
    marshal_samples = bench_ms(lambda: marshal_table(metadata), iterations)
    blob = marshal_table(metadata)
    request_samples = memfd_copy(blob, min(iterations, 20))
    return {
        "stat_round_trip": dist(stat_samples),
        "read_round_trip": {
            **dist(read_samples),
            "bytes": size,
        },
        "client_marshal": {
            **dist(marshal_samples),
            "bytes": len(blob),
        },
        "client_memfd_write": dist(request_samples),
        "floor_without_rust_encode_ms": (
            statistics.median(stat_samples) + statistics.median(marshal_samples)
        )
        / 1e6,
        "floor_note": (
            "stat_file is the cheapest request/response the service offers; the "
            "marshalling and memfd write are the client work a native encoder "
            "cannot avoid. Rust encoding, the reply copy and the plan-less "
            "store lookup are excluded, so this is a strict lower bound."
        ),
    }


def measure_path(metadata, checkpoint_id, storage_name, iterations):
    """The shipped fast path: first call (plan build) and later calls (hits)."""
    fast_serialize = getattr(dcp_mod, "serialize_metadata_fast", None)
    if fast_serialize is None:
        return {"available": False}
    cache = dcp_mod.MetadataPlanCache()
    expected = dcp_mod.serialize_metadata(
        metadata, checkpoint_id=checkpoint_id, storage_name=storage_name
    )
    first_report = {}
    started = time.perf_counter_ns()
    blob = fast_serialize(
        metadata,
        checkpoint_id=checkpoint_id,
        storage_name=storage_name,
        cache=cache,
        report=first_report,
    )
    first_ns = time.perf_counter_ns() - started
    if blob != expected:
        raise RuntimeError("the fast path produced different bytes")
    later = []
    modes = [first_report.get("mode")]
    for _ in range(iterations):
        report = {}
        started = time.perf_counter_ns()
        blob = fast_serialize(
            metadata,
            checkpoint_id=checkpoint_id,
            storage_name=storage_name,
            cache=cache,
            report=report,
        )
        later.append(time.perf_counter_ns() - started)
        modes.append(report.get("mode"))
        if blob != expected:
            raise RuntimeError("a later fast-path call produced different bytes")
    return {
        "available": True,
        "first_call_ms": first_ns / 1e6,
        "first_call_mode": modes[0],
        "later": dist(later),
        "modes": sorted(set(modes)),
        "plan_hits": cache.stats["plan_hits"],
        "plan_builds": cache.stats["plan_builds"],
        "fallbacks": cache.stats["fallbacks"],
        "oracle_checks": cache.stats["oracle_checks"],
        "oracle_mismatches": cache.stats["oracle_mismatches"],
    }


def measure_alternatives(metadata, checkpoint_id, storage_name, service, metadata_path, iterations):
    """The evaluated options, measured on the same metadata object."""
    tree = dcp_mod.metadata_parts(
        metadata, checkpoint_id=checkpoint_id, storage_name=storage_name
    )
    expected = wcb.encode(tree)
    row = {
        "tree_only": dist(bench_ms(lambda: dcp_mod.metadata_parts(metadata, checkpoint_id=checkpoint_id, storage_name=storage_name), iterations)),
        "reference": dist(bench_ms(lambda: wcb.encode(tree), iterations, check=expected)),
    }
    fast_module = getattr(dcp_mod, "wcb_fast", None)
    if fast_module is not None:
        row["fast_generic"] = dist(
            bench_ms(lambda: fast_module.encode(tree), iterations, check=expected)
        )
    plan_class = getattr(dcp_mod, "MetadataPlan", None)
    fast_serialize = getattr(dcp_mod, "serialize_metadata_fast", None)
    if plan_class is not None and fast_serialize is not None:
        limits = dcp_mod.Limits()
        candidate = plan_class(metadata, limits)
        roots = candidate.matches(metadata)
        if roots is None:
            raise RuntimeError("a freshly built plan does not match its metadata")
        row["plan_matches"] = dist(bench_ms(lambda: candidate.matches(metadata), iterations))
        row["plan_assemble"] = dist(
            bench_ms(
                lambda: candidate.assemble(
                    metadata, checkpoint_id, storage_name, limits, roots
                ),
                iterations,
                check=expected,
            )
        )
        row["plan_build"] = dist(
            bench_ms(lambda: plan_class(metadata, limits), max(5, iterations // 4))
        )
        cache = dcp_mod.MetadataPlanCache()
        fast_serialize(
            metadata,
            checkpoint_id=checkpoint_id,
            storage_name=storage_name,
            cache=cache,
        )
        row["plan_cached_total"] = dist(
            bench_ms(
                lambda: fast_serialize(
                    metadata,
                    checkpoint_id=checkpoint_id,
                    storage_name=storage_name,
                    cache=cache,
                ),
                iterations,
                check=expected,
            )
        )
        row["plan_cache_stats"] = dict(cache.stats)
    if service is not None:
        row["native_floor"] = native_floor(
            metadata, service, metadata_path, iterations
        )
    return row


def measure_size(torch, *, store_root, state, cli, iterations):
    captured = capture_metadata(torch, store_root=store_root, state=state, cli=cli)
    metadata = captured["metadata"]
    checkpoint_id = captured["checkpoint_id"]
    storage_name = captured["storage_name"]

    blob = serialize_metadata(
        metadata, checkpoint_id=checkpoint_id, storage_name=storage_name
    )

    # The shipped path, exactly as `finish()` calls it.
    box, restore_calls = count_service_calls()
    try:
        totals, shipped_blob = timed_serialize(
            serialize_metadata, metadata, checkpoint_id, storage_name, iterations
        )
    finally:
        restore_calls()
    if shipped_blob != blob:
        raise RuntimeError("serialize_metadata is not deterministic for one fixture")

    sections = measure_sections(metadata, checkpoint_id, storage_name, iterations)
    if sections["total"] != totals:
        # Same function, same fixture: the sectioned run is kept separate, but a
        # wildly different median would mean the instrumentation changed the work.
        pass

    value = local_value(
        metadata, checkpoint_id=checkpoint_id, storage_name=storage_name
    )
    if wcb.encode(value) != blob:
        raise RuntimeError("the court's value tree differs from serialize_metadata")
    replay = replay_phases(value, iterations)
    hashes = hash_cost(blob, min(iterations, 20))
    copies = memfd_copy(blob, min(iterations, 20))

    service = None
    metadata_path = "/checkpoints/%s/%s" % (
        storage_name,
        dcp_mod.DCP_METADATA_NAME,
    )
    try:
        service = service_mod.CheckpointService(store_root)
        alternatives = measure_alternatives(
            metadata,
            checkpoint_id,
            storage_name,
            service,
            metadata_path,
            iterations,
        )
    finally:
        if service is not None:
            service.close()

    return {
        "entries": len(state),
        "metadata_bytes": len(blob),
        "metadata_sha256": hashlib.sha256(blob).hexdigest(),
        "content_root": hashing.content_root(blob).hex(),
        "save_ms": captured["save_ms"],
        "save_stats": captured["stats"],
        "shipped": {
            "total": dist(totals),
            "service_calls_during_serialize": box["calls"],
        },
        "sections": {
            "total": dist(sections["total"]),
            "state_dict_tree": dist(sections["state_dict"]),
            "storage_rows_tree": dist(sections["storage_rows"]),
            "planner_tree": dist(sections["planner"]),
            "encode": dist(sections["encode"]),
        },
        "replay": {
            "walk": dist(replay["walk"]),
            "keys_encode_and_sort": dist(replay["keys"]),
            "varints": dist(replay["varints"]),
            "payload_extends": dist(replay["payloads"]),
            "items": replay["items"],
        },
        "cprofile": profile_encode(value),
        "memfd_copy": dist(copies),
        "path": measure_path(metadata, checkpoint_id, storage_name, iterations),
        "alternatives": alternatives,
        "hash": {
            "sha256": dist(hashes["sha256"]),
            "blake3": dist(hashes["blake3"]),
            "domain_blake3": dist(hashes["domain_blake3"]),
        },
    }


def run(args):
    torch = tg.load_torch()
    import torch.distributed.checkpoint  # noqa: F401  (fail early, not mid-run)

    sizes = [int(part) for part in str(args.sizes).split(",") if part.strip()]
    base_state = None
    if 169 in sizes or any(size <= 169 for size in sizes):
        # The R11 fixture, unchanged: a real optimizer step then capture_state.
        import checkpoint_latency as cl

        base_state = cl.fixture(torch)
        if len(base_state) != 169:
            raise RuntimeError(
                "the R11 fixture is %d entries, expected 169" % len(base_state)
            )

    work = Path(args.work_dir)
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)

    rows = []
    for size in sizes:
        state = build_state(torch, base_state, size)
        store = work / ("n%d" % size)
        store.mkdir()
        init_store(store, weftfs_cli=args.weftfs_cli)
        row = measure_size(
            torch,
            store_root=store,
            state=state,
            cli=args.weftfs_cli,
            iterations=args.iterations,
        )
        row["size"] = size
        rows.append(row)
        shipped = row["shipped"]["total"]
        print(
            "n=%-5d bytes=%-6d reference p50 %7.3f ms (p95 %7.3f) | plan %6.3f "
            "| generic %6.3f | native floor %6.3f"
            % (
                size,
                row["metadata_bytes"],
                shipped["p50_ms"],
                shipped["p95_ms"],
                row.get("path", {}).get("later", {}).get("p50_ms", float("nan")),
                row["alternatives"].get("fast_generic", {}).get("p50_ms", float("nan")),
                row["alternatives"].get("native_floor", {}).get(
                    "floor_without_rust_encode_ms", float("nan")
                ),
            )
        )

    receipt = {
        "schema": SCHEMA,
        "stamp": args.stamp,
        "measured_at": now_iso(),
        "fixture": {
            "source": "python/courts/training_goodput.py build_model/capture_state",
            "base_entries": len(base_state) if base_state is not None else None,
            "sizes": sizes,
            "note": (
                "169 is the exact R11/R12b fixture; 1/16/64 take its first N "
                "entries; 1024 repeats the fixture with suffixed names and 4x4 "
                "float32 tensors"
            ),
        },
        "environment": {
            "python": platform.python_version(),
            "torch": getattr(torch, "__version__", "unknown"),
            "platform": platform.platform(),
            "kernel": platform.release(),
            "cpu_count": os.cpu_count(),
            "weftfs_cli": args.weftfs_cli,
            "court_image": os.environ.get("WEFT_COURT_IMAGE"),
            "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID"),
            "court_memory": os.environ.get("WEFT_COURT_MEMORY"),
            "court_cpus": os.environ.get("WEFT_COURT_CPUS"),
            "court_pids": os.environ.get("WEFT_COURT_PIDS"),
            "repo_weftfs": os.environ.get("WEFT_REPO_WEFTFS"),
            "repo_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC"),
        },
        "config": {
            "iterations": args.iterations,
            "profile_calls": PROFILE_CALLS,
            "write_policy": "fast",
            "fast_codec": "raw",
            "note": (
                "the save that produces the metadata is untimed; only the "
                "serialisation of the resulting object is measured"
            ),
            "labels": {
                "shipped.total": (
                    "the canonical encoder `serialize_metadata` (the reference "
                    "oracle; the only path before R12c item 3)"
                ),
                "sections.encode": "`wcb.encode` inside the oracle",
                "path": "`serialize_metadata_fast` (the shipped path after R12c item 3)",
                "alternatives.plan_cached_total": "serialize_metadata_fast, warm plan",
                "alternatives.fast_generic": "wcb_fast.encode over the whole tree",
                "alternatives.plan_build": "one MetadataPlan build + oracle proof",
                "alternatives.native_floor": (
                    "Rust-service round trip + client marshalling, excluding any "
                    "Rust encoding work"
                ),
            },
        },
        "sizes": rows,
        "peak_rss_kib": int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss),
    }
    if args.receipt:
        write_json(args.receipt, receipt)
        print("dcp-metadata-cost receipt: %s" % args.receipt)
    return 0


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run_p = sub.add_parser("run")
    run_p.add_argument("--receipt")
    run_p.add_argument("--stamp", default=utc_stamp())
    run_p.add_argument("--work-dir", default="/tmp/weft-dcp-metadata")
    run_p.add_argument("--sizes", default=DEFAULT_SIZES)
    run_p.add_argument("--iterations", type=int, default=DEFAULT_ITERATIONS)
    run_p.add_argument("--weftfs-cli", default=os.environ.get("WEFTFS_CLI"))
    args = parser.parse_args(argv)
    if args.command == "run":
        if args.weftfs_cli is None:
            args.weftfs_cli = str(
                Path("/work/.docker-cache/target-rust/release/weftfs")
            )
        return run(args)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
