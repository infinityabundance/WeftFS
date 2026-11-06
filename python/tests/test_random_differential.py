"""Randomized differential court: Python reader vs the Rust CLI.

For each AI mode 9-16 this test generates at least 2,000 frames from a
deterministic PRNG (``ai_frames``), decodes every frame with both the
independent Python reader and ``weftcodec decode``, and asserts:

* both implementations accept or reject each frame identically;
* when both accept, the decoded bytes are identical;
* for TENSOR_SHARDED, the shard-set logical root matches
  ``weftcodec tensor root --frames``.

A further 200 single-byte mutants per mode exercise the reject paths. Error
*classes* are recorded for information; the hard assertion is accept/reject
equivalence, as required for the differential.

Counts are written to ``WEFT_AI_DIFF_SUMMARY`` for the court receipt.
"""

from __future__ import annotations

import json
import os
import re
import struct
import subprocess
import tempfile

import pytest

import ai_frames
import support

from weftcodec import ai_modes, modes
from weftcodec.crc32c import crc32c
from weftcodec.errors import WeftError
from weftcodec.frame import parse
from weftcodec.limits import Limits

MODES = tuple(range(9, 17))
MIN_FRAMES = 2000
MUTATIONS_PER_MODE = 200
SUMMARY = {}


def _rust_decode(cli, work, frame, base):
    frame_path = os.path.join(work, "frame.wft1")
    out_path = os.path.join(work, "out.bin")
    with open(frame_path, "wb") as handle:
        handle.write(frame)
    cmd = [cli, "decode", "--input", frame_path, "--output", out_path]
    if base is not None:
        base_path = os.path.join(work, "base.wft1")
        with open(base_path, "wb") as handle:
            handle.write(base)
        cmd.extend(["--base", base_path])
    completed = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if completed.returncode != 0:
        text = completed.stderr.decode("utf-8", "replace")
        for line in text.splitlines():
            head = line.split(":", 1)[0].strip()
            if head in (
                "Malformed",
                "Corrupt",
                "UnsupportedVersion",
                "UnsupportedMode",
                "UnsupportedFeature",
                "LimitExceeded",
                "DependencyMissing",
                "DependencyDenied",
                "DependencyMismatch",
                "DependencyCycle",
                "NonCanonical",
                "Io",
            ):
                return ("error", head)
        match = re.search(
            r"\b(Malformed|Corrupt|UnsupportedVersion|UnsupportedMode|"
            r"UnsupportedFeature|LimitExceeded|DependencyMissing|"
            r"DependencyDenied|DependencyMismatch|DependencyCycle|"
            r"NonCanonical|Io)\b",
            text,
        )
        return ("error", match.group(1) if match else "Unknown")
    with open(out_path, "rb") as handle:
        return ("ok", handle.read())


def _python_decode(mode, frame, base):
    parsed = parse(frame, Limits())
    if mode == 10:
        return ai_modes.decode_tensor_sharded(parsed, Limits()).planes
    if mode == 11:
        return ai_modes.decode_weight_delta(parsed, None, Limits(), base)
    if mode == 14:
        return ai_modes.decode_semantic_delta(parsed, None, Limits(), base)
    if mode == 16:
        return ai_modes.decode_sparse_delta(parsed, None, Limits(), base)
    return modes.decode_frame(parsed, None, Limits())


def _python_result(mode, frame, base):
    try:
        return ("ok", _python_decode(mode, frame, base), None)
    except WeftError as exc:
        return ("error", None, exc.error_class)
    except Exception as exc:  # a non-Weft exception is a hard failure
        return ("crash", None, "%s: %s" % (type(exc).__name__, exc))


def _rust_shard_root(cli, work, frames):
    shard_dir = tempfile.mkdtemp(dir=work, prefix="shards-")
    for index, frame in enumerate(frames):
        with open(
            os.path.join(shard_dir, "shard-%03d.wft1" % index), "wb"
        ) as handle:
            handle.write(frame)
    completed = subprocess.run(
        [cli, "tensor", "root", "--frames", "--shards-dir", shard_dir, "--json"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if completed.returncode != 0:
        return None
    return json.loads(completed.stdout.decode("utf-8")).get("layout_root")


def _stream_cases(mode, seed):
    rng = ai_frames.Rng(seed)
    builder = ai_frames.CASE_BUILDERS[mode]
    frames = 0
    while frames < MIN_FRAMES:
        case = builder(rng)
        frames += len(case["frames"])
        yield case


def _summarize():
    support.write_summary("WEFT_AI_DIFF_SUMMARY", {"modes": SUMMARY})


@pytest.mark.parametrize("mode", MODES)
def test_random_differential(mode):
    if not support.has_rust_cli():
        pytest.skip("Rust CLI is not available in this court")
    cli = str(support.RUST_CLI)
    record = {
        "mode": mode,
        "cases": 0,
        "frames": 0,
        "rust_ok": 0,
        "rust_error": 0,
        "bytes_match": 0,
        "accept_reject_agree": 0,
        "class_matches": 0,
        "class_checked": 0,
        "shard_set_roots_checked": 0,
        "shard_set_root_matches": 0,
        "mutation_frames": 0,
        "mutation_agree": 0,
        "mutation_both_ok": 0,
        "mutation_bytes_match": 0,
        "mutation_class_checked": 0,
        "mutation_class_matches": 0,
        "mismatches": [],
    }
    with tempfile.TemporaryDirectory(prefix="weft-ai-diff-") as work:
        for case in _stream_cases(mode, 0x5EED0000 + mode):
            record["cases"] += 1
            base = case.get("base")
            for frame in case["frames"]:
                record["frames"] += 1
                rust_kind, rust_payload = _rust_decode(cli, work, frame, base)
                if rust_kind == "ok":
                    record["rust_ok"] += 1
                else:
                    record["rust_error"] += 1
                py_kind, py_payload, py_class = _python_result(mode, frame, base)
                if py_kind == "crash":
                    record["mismatches"].append(
                        {
                            "kind": "python_crash",
                            "detail": py_class,
                            "frame": frame.hex()[:200],
                        }
                    )
                    continue
                if rust_kind == py_kind:
                    record["accept_reject_agree"] += 1
                else:
                    record["mismatches"].append(
                        {
                            "kind": "accept_reject",
                            "rust": rust_kind,
                            "python": py_kind,
                            "frame": frame.hex()[:200],
                        }
                    )
                if rust_kind == "ok" and py_kind == "ok":
                    if rust_payload == py_payload:
                        record["bytes_match"] += 1
                    else:
                        record["mismatches"].append(
                            {
                                "kind": "bytes",
                                "rust_len": len(rust_payload),
                                "python_len": len(py_payload),
                                "frame": frame.hex()[:200],
                            }
                        )
                if rust_kind == "error" and py_kind == "error":
                    record["class_checked"] += 1
                    if rust_payload == py_class:
                        record["class_matches"] += 1
            if mode == 10:
                root = _rust_shard_root(cli, work, case["frames"])
                try:
                    shards = [
                        ai_modes.decode_tensor_sharded(
                            parse(frame, Limits()), Limits()
                        )
                        for frame in case["frames"]
                    ]
                    ours = ai_modes.shard_set_root(shards, Limits())[0].hex()
                except WeftError:
                    ours = None
                record["shard_set_roots_checked"] += 1
                if root is not None and ours == root:
                    record["shard_set_root_matches"] += 1
                elif root is not None:
                    record["mismatches"].append(
                        {"kind": "shard_set_root", "rust": root, "python": ours}
                    )

    # Mutants: one random byte change per frame, 200 per mode.
    rng = ai_frames.Rng(0xBAD00000 + mode)
    mutant_cases = 0
    with tempfile.TemporaryDirectory(prefix="weft-ai-mut-") as work:
        for case in ai_frames.generate(mode, 0xA11CE000 + mode, MUTATIONS_PER_MODE):
            mutant_cases += 1
            frame = case["frames"][rng.next_u64() % len(case["frames"])]
            # Flip a bit in the body and repair the trailer CRC, so the mutant
            # reaches the descriptor/payload checks instead of stopping at the
            # frame checksum. Header and tables stay intact.
            try:
                parsed = parse(frame, Limits())
                body_start = 96 + parsed.header.table_len
            except WeftError:
                body_start = 96
            if len(frame) - 4 <= body_start:
                continue
            position = body_start + rng.next_u64() % (len(frame) - 4 - body_start)
            mutated = bytearray(frame)
            mutated[position] ^= 1 << (rng.next_u64() % 8)
            mutated[-4:] = struct.pack("<I", crc32c(bytes(mutated[:-4])))
            mutated = bytes(mutated)
            record["mutation_frames"] += 1
            rust_kind, rust_payload = _rust_decode(cli, work, mutated, case.get("base"))
            py_kind, py_payload, py_class = _python_result(
                mode, mutated, case.get("base")
            )
            if py_kind == "crash":
                record["mismatches"].append(
                    {"kind": "mutation_python_crash", "detail": py_class}
                )
                continue
            if rust_kind == py_kind:
                record["mutation_agree"] += 1
            else:
                record["mismatches"].append(
                    {
                        "kind": "mutation_accept_reject",
                        "rust": rust_kind,
                        "python": py_kind,
                        "position": position,
                    }
                )
            if rust_kind == "ok" and py_kind == "ok":
                record["mutation_both_ok"] += 1
                if rust_payload == py_payload:
                    record["mutation_bytes_match"] += 1
                else:
                    record["mismatches"].append(
                        {"kind": "mutation_bytes", "position": position}
                    )
            if rust_kind == "error" and py_kind == "error":
                record["mutation_class_checked"] += 1
                if rust_payload == py_class:
                    record["mutation_class_matches"] += 1

    record["mutant_cases"] = mutant_cases
    SUMMARY[str(mode)] = record
    _summarize()

    assert record["frames"] >= MIN_FRAMES, record
    assert record["rust_ok"] == record["frames"], record["mismatches"][:3]
    assert record["accept_reject_agree"] == record["frames"], record["mismatches"][:3]
    assert record["bytes_match"] == record["frames"], record["mismatches"][:3]
    assert record["mutation_frames"] == MUTATIONS_PER_MODE, record
    assert record["mutation_agree"] == record["mutation_frames"], record["mismatches"][:3]
    assert record["mutation_bytes_match"] == record["mutation_both_ok"], record[
        "mismatches"
    ][:3]
    if mode == 10:
        assert record["shard_set_root_matches"] == record["shard_set_roots_checked"], record[
            "mismatches"
        ][:3]


def test_random_summary():
    assert SUMMARY
    for record in SUMMARY.values():
        assert not record["mismatches"], record["mismatches"][:3]
