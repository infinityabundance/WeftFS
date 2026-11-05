"""Cross-language checks: Rust CLI encodes, the Python reader decodes.

Skipped with an explicit reason when the Rust CLI binary is not present.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys

import pytest

import support

from weftcodec import wcb
from weftcodec import varint
from weftcodec.dependency import DictResolver
from weftcodec.frame import parse
from weftcodec.hashing import content_root
from weftcodec.identity import verify_frame
from weftcodec.modes import decode_bytes, decode_token_payload, tensor_metadata

pytestmark = pytest.mark.skipif(
    not support.has_rust_cli(),
    reason="Rust CLI not found at %s; run this test in the Docker court" % support.RUST_CLI,
)

U64_MAX = (1 << 64) - 1
TOKEN_IDS = [0, 1, 2, 2, 3, 1 << 40, U64_MAX, 5, 5, 4]
CASES = [
    "raw",
    "zstd",
    "raw_chunked",
    "zstd_chunked",
    "reference",
    "delta",
    "dictionary",
    "tensor",
    "tokens",
    "quantized",
    "structured",
]

_RESULTS = {}


def _pattern(size: int) -> bytes:
    return bytes(((index * 37 + 11) ^ (index >> 3)) & 0xFF for index in range(size))


def _expected_token_payload(ids) -> bytes:
    out = bytearray()
    varint.encode(ids[0], out)
    previous = ids[0]
    for token in ids[1:]:
        delta = (token - previous) & U64_MAX
        signed = delta if delta < (1 << 63) else delta - (1 << 64)
        varint.encode(varint.zigzag_encode(signed), out)
        previous = token
    return bytes(out)


def _run(*args):
    return subprocess.run(
        [str(support.RUST_CLI), *[str(arg) for arg in args]],
        capture_output=True,
        check=False,
    )


@pytest.fixture(scope="module")
def fixtures(tmp_path_factory):
    root = tmp_path_factory.mktemp("weft-cross")
    pattern = _pattern(300_000)
    base = _pattern(200_000)
    edited = bytearray(base)
    for offset in (0, 1000, 4096, 65536, 130_000, 199_990):
        edited[offset : offset + 8] = b"EDITED!!"
    edited = bytes(edited)
    dictionary = pattern[100_000:164_000]
    tensor_payload = bytes(range(24))
    quantized_payload = bytes([0, 1, 2, 255, 128, 7])
    structured = wcb.encode(
        {
            "schema": wcb.U64(1),
            "name": "cross-language",
            "flags": [None, True, wcb.I64(-3)],
        }
    )

    (root / "pattern.bin").write_bytes(pattern)
    (root / "base.bin").write_bytes(base)
    (root / "edited.bin").write_bytes(edited)
    (root / "dict.bin").write_bytes(dictionary)
    (root / "tensor.bin").write_bytes(tensor_payload)
    (root / "quantized.bin").write_bytes(quantized_payload)
    (root / "structured.wcb").write_bytes(structured)
    (root / "tokens.json").write_text(
        json.dumps({"vocab_schema": "weft.test.vocab.v1", "token_ids": TOKEN_IDS}),
        encoding="utf-8",
    )

    deps = root / "deps"
    deps.mkdir()
    (deps / content_root(base).hex()).write_bytes(base)
    (deps / content_root(dictionary).hex()).write_bytes(dictionary)

    return {
        "root": root,
        "deps": deps,
        "pattern": pattern,
        "base": base,
        "edited": edited,
        "dictionary": dictionary,
        "tensor_payload": tensor_payload,
        "quantized_payload": quantized_payload,
        "structured": structured,
        "token_payload": _expected_token_payload(TOKEN_IDS),
    }


def _case_spec(case: str, data) -> dict:
    root = data["root"]
    output = root / ("%s.wft1" % case)
    if case == "raw":
        args = ["encode", "--mode", "raw", "--input", root / "pattern.bin", "--output", output]
        expected = data["pattern"]
        mode = "raw"
        resolver = None
        deps = None
    elif case == "zstd":
        args = ["encode", "--mode", "zstd", "--level", "3", "--input", root / "pattern.bin", "--output", output]
        expected = data["pattern"]
        mode = "zstd"
        resolver = None
        deps = None
    elif case == "raw_chunked":
        args = ["encode", "--mode", "raw", "--chunk-size", "65536", "--input", root / "pattern.bin", "--output", output]
        expected = data["pattern"]
        mode = "raw"
        resolver = None
        deps = None
    elif case == "zstd_chunked":
        args = ["encode", "--mode", "zstd", "--level", "3", "--chunk-size", "65536", "--input", root / "pattern.bin", "--output", output]
        expected = data["pattern"]
        mode = "zstd"
        resolver = None
        deps = None
    elif case == "reference":
        args = ["encode", "--mode", "reference", "--input", root / "base.bin", "--base", root / "base.bin", "--output", output]
        expected = data["base"]
        mode = "reference"
        resolver = DictResolver({content_root(data["base"]): data["base"]})
        deps = data["deps"]
    elif case == "delta":
        args = ["encode", "--mode", "delta", "--input", root / "edited.bin", "--base", root / "base.bin", "--output", output]
        expected = data["edited"]
        mode = "delta"
        resolver = DictResolver({content_root(data["base"]): data["base"]})
        deps = data["deps"]
    elif case == "dictionary":
        args = ["encode", "--mode", "dictionary", "--level", "3", "--input", root / "pattern.bin", "--dict", root / "dict.bin", "--output", output]
        expected = data["pattern"]
        mode = "dictionary"
        resolver = DictResolver({content_root(data["dictionary"]): data["dictionary"]})
        deps = data["deps"]
    elif case == "tensor":
        args = ["encode", "--mode", "tensor", "--input", root / "tensor.bin", "--output", output, "--dtype", "f32", "--shape", "2,3"]
        expected = data["tensor_payload"]
        mode = "tensor"
        resolver = None
        deps = None
    elif case == "tokens":
        args = ["encode", "--mode", "tokens", "--input", root / "tokens.json", "--output", output]
        expected = data["token_payload"]
        mode = "tokens"
        resolver = None
        deps = None
    elif case == "quantized":
        args = ["encode", "--mode", "quantized", "--input", root / "quantized.bin", "--output", output, "--dtype", "i8", "--shape", "2,3", "--scale", "0.25", "--zero-point=-3"]
        expected = data["quantized_payload"]
        mode = "quantized_tensor"
        resolver = None
        deps = None
    elif case == "structured":
        args = ["encode", "--mode", "structured", "--input", root / "structured.wcb", "--output", output]
        expected = data["structured"]
        mode = "structured"
        resolver = None
        deps = None
    else:  # pragma: no cover
        raise AssertionError("unknown case %s" % case)
    return {
        "args": args,
        "output": output,
        "expected": expected,
        "mode": mode,
        "resolver": resolver,
        "deps": deps,
    }


def _record(case, outcome, detail="", **extra):
    entry = {"case": case, "outcome": outcome, "detail": detail}
    entry.update(extra)
    _RESULTS[case] = entry
    support.write_summary("WEFT_CROSS_SUMMARY", _summary())


def _summary():
    counts = {"passed": 0, "failed": 0, "skipped": 0}
    for entry in _RESULTS.values():
        counts[entry["outcome"]] += 1
    return {
        "rust_cli": str(support.RUST_CLI),
        "rust_cli_sha256": _sha256_file(support.RUST_CLI) if support.has_rust_cli() else None,
        "cases": len(CASES),
        "total": len(_RESULTS),
        "passed": counts["passed"],
        "failed": counts["failed"],
        "skipped": counts["skipped"],
        "results": list(_RESULTS.values()),
    }


def _sha256_file(path) -> "str | None":
    try:
        digest = hashlib.sha256()
        with open(path, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
        return digest.hexdigest()
    except OSError:
        return None


@pytest.mark.parametrize("case", CASES)
def test_rust_encode_python_decode(case, fixtures):
    spec = _case_spec(case, fixtures)
    output = spec["output"]
    try:
        encoded = _run(*spec["args"])
        assert encoded.returncode == 0, encoded.stderr.decode("utf-8", "replace")
        assert output.is_file(), "Rust CLI did not write %s" % output

        verify_args = ["verify", str(output)]
        if spec["deps"] is not None:
            verify_args += ["--deps", str(spec["deps"])]
        rust_verify = _run(*verify_args)
        assert rust_verify.returncode == 0, rust_verify.stderr.decode("utf-8", "replace")

        result = verify_frame(output, spec["resolver"])
        assert result.ok, "%s: %s" % (result.error_class, result.error)
        assert result.mode == spec["mode"], "mode %s != %s" % (result.mode, spec["mode"])

        frame = parse(output.read_bytes())
        plaintext = decode_bytes(output.read_bytes(), spec["resolver"])
        assert plaintext == spec["expected"], "%s plaintext mismatch" % case
        assert content_root(plaintext) == frame.header.content_hash
        assert result.computed_content_hash == frame.header.content_hash.hex()

        rust_json = json.loads(
            _run("inspect", "--json", str(output)).stdout.decode("utf-8")
        )
        python_json = json.loads(
            subprocess.run(
                [sys.executable, "-m", "weftcodec", "inspect", str(output)],
                capture_output=True,
                check=True,
            ).stdout.decode("utf-8")
        )
        shared = [
            "mode",
            "mode_value",
            "format_major",
            "format_minor",
            "domain",
            "level",
            "decoded_len",
            "body_len",
            "table_len",
            "frame_len",
            "chunk_count",
            "content_hash",
        ]
        for key in shared:
            assert rust_json[key] == python_json[key], "inspect %s mismatch" % key
        assert rust_json["dep_count"] == python_json["dep_count"]
        assert rust_json["flags"] == python_json["flags_value"]
        assert rust_json["features"] == python_json["features_value"]
        assert rust_json["flag_names"] == python_json["flags"]
        assert rust_json["feature_names"] == python_json["features"]

        if case == "tokens":
            ids = decode_token_payload(
                plaintext,
                len(TOKEN_IDS),
            )
            assert ids == TOKEN_IDS
        if case == "tensor":
            meta = tensor_metadata(frame)
            assert meta["dtype"] == "f32"
            assert meta["shape"] == [2, 3]
            assert meta["element_count"] == 6
        if case == "structured":
            assert wcb.encode(wcb.decode(plaintext)) == plaintext
        if case in ("raw_chunked", "zstd_chunked"):
            assert rust_json["chunk_count"] > 1
    except Exception as exc:
        _record(
            case,
            "failed",
            "%s: %s" % (type(exc).__name__, exc),
            frame_bytes=output.stat().st_size if output.is_file() else None,
            frame_sha256=_sha256_file(output),
        )
        raise

    _record(
        case,
        "passed",
        frame_bytes=output.stat().st_size,
        frame_sha256=_sha256_file(output),
        mode=spec["mode"],
        decoded_len=len(spec["expected"]),
    )


def test_cross_summary():
    summary = _summary()
    assert summary["failed"] == 0, summary["results"]
    assert summary["total"] == len(CASES)
    assert summary["passed"] == len(CASES)
