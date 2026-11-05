"""CLI contract: exit codes, JSON inspection, and exact decode output."""

from __future__ import annotations

import json
import subprocess
import sys

import support

from weftcodec.hashing import content_root

VECTORS = support.VECTORS / "codec"


def _run(*args):
    return subprocess.run(
        [sys.executable, "-m", "weftcodec", *[str(arg) for arg in args]],
        capture_output=True,
        check=False,
    )


def test_verify_ok(tmp_path):
    vector = support.load_json(VECTORS / "003_raw_4k.json")
    frame = tmp_path / "frame.wft1"
    frame.write_bytes(bytes.fromhex(vector["frame_hex"]))
    proc = _run("verify", frame)
    assert proc.returncode == 0, proc.stderr.decode()
    stdout = proc.stdout.decode()
    assert stdout.startswith("OK mode=raw ")
    assert "content_hash=%s" % vector["content_hash"] in stdout


def test_verify_rejects_with_typed_exit(tmp_path):
    vector = support.load_json(VECTORS / "013_bad_magic.json")
    frame = tmp_path / "bad.wft1"
    frame.write_bytes(bytes.fromhex(vector["frame_hex"]))
    proc = _run("verify", frame)
    assert proc.returncode == 1
    assert proc.stderr.decode().startswith("Malformed:")


def test_verify_reference_with_deps(tmp_path):
    vector = support.load_json(VECTORS / "009_reference_equal.json")
    frame = tmp_path / "ref.wft1"
    frame.write_bytes(bytes.fromhex(vector["frame_hex"]))
    deps = tmp_path / "deps"
    deps.mkdir()
    for dependency in vector["dependencies"]:
        (deps / dependency["hash"]).write_bytes(bytes.fromhex(dependency["bytes_hex"]))
    proc = _run("verify", frame, "--deps", deps)
    assert proc.returncode == 0, proc.stderr.decode()


def test_inspect_is_json(tmp_path):
    vector = support.load_json(VECTORS / "006_raw_chunked_1024.json")
    frame = tmp_path / "chunked.wft1"
    frame.write_bytes(bytes.fromhex(vector["frame_hex"]))
    proc = _run("inspect", frame)
    assert proc.returncode == 0, proc.stderr.decode()
    record = json.loads(proc.stdout)
    assert record["ok"] is True
    assert record["mode"] == "raw"
    assert record["chunk_count"] == 5
    assert record["flags_value"] & 0x0001
    assert record["content_hash"] == vector["content_hash"]
    assert len(record["chunks"]) == 5


def test_decode_output_is_byte_exact(tmp_path):
    vector = support.load_json(VECTORS / "005_raw_64k_repetitive.json")
    frame = tmp_path / "data.wft1"
    output = tmp_path / "out.bin"
    frame.write_bytes(bytes.fromhex(vector["frame_hex"]))
    proc = _run("decode", frame, output)
    assert proc.returncode == 0, proc.stderr.decode()
    plaintext = output.read_bytes()
    assert plaintext == bytes.fromhex(vector["plaintext_hex"])
    assert content_root(plaintext).hex() == vector["plaintext_hash"]


def test_usage_error_exit_code():
    proc = _run()
    assert proc.returncode == 2
