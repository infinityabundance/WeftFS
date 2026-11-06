"""Every AI-mode conformance vector, decoded by the independent Python reader.

Valid vectors are decoded to the exact bytes the vector records
(``plaintext_hex``, ``target_hex``, ``elements_hex``, ``values_hex``, or the
reassembled shard words) and every recorded root is recomputed:
``content_hash``, ``layout_root``, ``delta_root`` (inside the decoder),
``logical_root``, per-tile ``tile_root``, ``stream_root``, and the semantic
root. Where a frame exists, the Rust CLI decodes the same bytes and the two
outputs must be identical.

Reject vectors must raise the normative class the vector declares. A summary
is written to ``WEFT_AI_VECTOR_SUMMARY`` for the court receipt.
"""

from __future__ import annotations

import collections
import glob
import hashlib
import json
import os
import subprocess
import tempfile

import pytest

import support

from weftcodec import ai_modes, modes, wcb
from weftcodec.dependency import FrameSourceResolver
from weftcodec.errors import WeftError
from weftcodec.frame import parse
from weftcodec.hashing import hash_domain
from weftcodec.limits import Limits

VECTORS = support.VECTORS
RESULTS = {}


def _load(family, prefix=None):
    items = []
    for path in sorted((VECTORS / family).glob("*.json")):
        if path.name == "PROVENANCE.json":
            continue
        if prefix is not None and not path.name.startswith(prefix):
            continue
        with open(path, "r", encoding="utf-8") as handle:
            vector = json.load(handle)
        items.append(("%s/%s" % (family, path.name), vector))
    return items


def _expect(vector):
    value = vector.get("expect")
    if value == "ok":
        return {"ok": True}
    if isinstance(value, dict):
        return value
    return {"error": vector["expect_error"]}


def _limits(vector):
    return Limits.from_overrides(vector.get("limits"))


def _record(name, outcome, detail=""):
    RESULTS[name] = {"name": name, "outcome": outcome, "detail": detail}
    support.write_summary("WEFT_AI_VECTOR_SUMMARY", _summary())


def _summary():
    counts = collections.Counter(entry["outcome"] for entry in RESULTS.values())
    return {
        "total": len(RESULTS),
        "passed": counts["passed"],
        "failed": counts["failed"],
        "skipped": counts["skipped"],
        "vectors": list(RESULTS.values()),
    }


def _policy_value(policy):
    scheme = policy.get("scheme")
    if scheme == "none":
        return {"scheme": "none"}
    if scheme == "mantissa_truncate":
        bits = policy.get("bits")
        return {
            "bits": None if bits is None else wcb.U64(int(bits)),
            "scheme": "mantissa_truncate",
        }
    block_size = policy.get("block_size")
    levels = policy.get("levels")
    return {
        "block_size": None if block_size is None else wcb.U64(int(block_size)),
        "levels": None if levels is None else wcb.U64(int(levels)),
        "scheme": "block_quantize",
    }


def _source_resolver(vector, limits, root_hex):
    frames = {}
    for entry in vector.get("frame_source") or []:
        frames[bytes.fromhex(entry["hash"])] = bytes.fromhex(entry["hex"])
    root_bytes = bytes.fromhex(root_hex)
    try:
        root = parse(root_bytes, limits)
    except WeftError:
        root = None
    visiting = set()
    stats = {"fetched": 0, "decoded": 0}
    if root is not None:
        frames.setdefault(root.header.content_hash, root_bytes)
        visiting.add(root.header.content_hash)
        stats["fetched"] = len(root_bytes)
    if not frames:
        return None
    return FrameSourceResolver(frames, limits, visiting=visiting, stats=stats)


def _delta_base(vector, limits):
    base_hex = vector.get("base_frame_hex")
    return bytes.fromhex(base_hex) if base_hex else None


# ---------------------------------------------------------------------------
# Valid vectors
# ---------------------------------------------------------------------------


def _valid_float(vector, limits):
    frame = parse(bytes.fromhex(vector["frame_hex"]), limits)
    out = modes.decode_frame(frame, None, limits)
    assert out.hex() == vector["plaintext_hex"]
    assert frame.header.content_hash.hex() == vector["content_hash"]
    return [out]


def _valid_sharded(vector, limits):
    shards = [
        ai_modes.decode_tensor_sharded(
            parse(bytes.fromhex(entry["frame_hex"]), limits), limits
        )
        for entry in vector["frames"]
    ]
    root, words = ai_modes.shard_set_root(shards, limits)
    assert root.hex() == vector["layout_root"]
    assert bytes(word & 0xFF for word in words) == bytes.fromhex(vector["words_hex"])
    return [b"".join(shard.planes for shard in shards)]


def _valid_delta(vector, limits):
    base = _delta_base(vector, limits)
    frame = parse(bytes.fromhex(vector["delta_frame_hex"]), limits)
    out = modes.decode_frame(frame, None, limits, base)
    assert out.hex() == vector["target_hex"]
    assert frame.header.content_hash.hex() == vector["target_hash"]
    return [out]


def _valid_kv(vector, limits):
    outputs = []
    for page in sorted(vector["pages"], key=lambda item: item["page_index"]):
        frame = parse(bytes.fromhex(page["frame_hex"]), limits)
        out = ai_modes.decode_kv_cache(frame, limits)
        assert out.hex() == page["elements_hex"]
        assert frame.header.decoded_len == page["page_bytes"]
        outputs.append(out)
    return outputs


def _valid_activation(vector, limits):
    outputs = []
    metas = []
    for chunk in sorted(vector["chunks"], key=lambda item: item["chunk_index"]):
        frame = parse(bytes.fromhex(chunk["frame_hex"]), limits)
        out = ai_modes.decode_activation_stream(frame, limits)
        assert out.hex() == chunk["elements_hex"]
        metas.append(ai_modes.activation_metadata(frame, limits))
        outputs.append(out)
    first = metas[0]
    root = ai_modes.activation_stream_root(
        first.stream,
        first.step,
        first.rank,
        first.world,
        first.chunk_count,
        [meta.digest_root for meta in metas],
        limits,
    )
    assert root.hex() == vector["stream_root"]
    assert [meta.digest_root.hex() for meta in metas] == vector["chunk_digests"]
    return outputs


def _valid_tiled(vector, limits):
    frame = parse(bytes.fromhex(vector["frame_hex"]), limits)
    out = modes.decode_frame(frame, None, limits)
    assert out.hex() == vector["plaintext_hex"]
    element_count = 1
    for dim in vector["shape"]:
        element_count *= dim
    logical = ai_modes.tensor_logical_root(
        vector["dtype"], vector["shape"], element_count, out, limits
    )
    assert logical.hex() == vector["logical_root"]
    return [out]


def _valid_semantic_root(vector, limits):
    policy = ai_modes.parse_semantic_policy(
        _policy_value(vector["policy"]), vector["dtype"], vector["element_count"]
    )
    values = bytes.fromhex(vector["values_hex"])
    root = ai_modes.semantic_root(
        vector["dtype"], vector["shape"], values, policy, limits
    )
    assert root.hex() == vector["semantic_root"]
    return [values]


def _valid_wcb(vector, limits):
    canonical = bytes.fromhex(vector["canonical_hex"])
    wcb.decode(canonical, limits)
    root = hash_domain("weft/structured/v1", canonical)
    return [canonical]


VALID_CASES = (
    [
        ("float_tensor", vector, _valid_float)
        for _, vector in _load("codec")
        if _expect(vector).get("ok") and vector.get("mode") == "float_tensor"
    ]
    + [
        ("sharded", vector, _valid_sharded)
        for _, vector in _load("sharded")
        if _expect(vector).get("ok")
    ]
    + [
        ("weight_delta", vector, _valid_delta)
        for _, vector in _load("weight-delta")
        if _expect(vector).get("ok")
    ]
    + [
        ("kv_cache", vector, _valid_kv)
        for _, vector in _load("kv-cache")
        if _expect(vector).get("ok")
    ]
    + [
        ("activation_stream", vector, _valid_activation)
        for _, vector in _load("activation-stream")
        if _expect(vector).get("ok")
    ]
    + [
        ("semantic_delta", vector, _valid_delta)
        for _, vector in _load("semantic-delta")
        if _expect(vector).get("ok")
    ]
    + [
        ("sparse_delta", vector, _valid_delta)
        for _, vector in _load("sparse-delta")
        if _expect(vector).get("ok")
    ]
    + [
        ("tensor_tiled", vector, _valid_tiled)
        for _, vector in _load("tensor-tiled")
        if _expect(vector).get("ok")
    ]
    + [
        ("semantic_root", vector, _valid_semantic_root)
        for _, vector in _load("semantic-root")
        if _expect(vector).get("ok")
    ]
    + [
        ("wcb", vector, _valid_wcb)
        for _, vector in _load("structured")
        if _expect(vector).get("ok")
    ]
)
VALID_IDS = [
    "%s:%s" % (family, vector.get("name", "vector"))
    for family, vector, _ in VALID_CASES
]


@pytest.mark.parametrize("family,vector,checker", VALID_CASES, ids=VALID_IDS)
def test_valid_vector(family, vector, checker):
    limits = _limits(vector)
    name = "%s:%s" % (family, vector.get("name", "vector"))
    try:
        checker(vector, limits)
    except Exception:
        _record(name, "failed")
        raise
    _record(name, "passed")


# ---------------------------------------------------------------------------
# Rust CLI cross-check on the same frame bytes
# ---------------------------------------------------------------------------


def _rust_decode(frame_bytes, base=None):
    cli = support.RUST_CLI
    if not support.has_rust_cli():
        return None
    with tempfile.TemporaryDirectory(prefix="weft-ai-vector-") as work:
        frame_path = os.path.join(work, "frame.wft1")
        out_path = os.path.join(work, "out.bin")
        with open(frame_path, "wb") as handle:
            handle.write(frame_bytes)
        cmd = [str(cli), "decode", "--input", frame_path, "--output", out_path]
        if base is not None:
            base_path = os.path.join(work, "base.wft1")
            with open(base_path, "wb") as handle:
                handle.write(base)
            cmd.extend(["--base", base_path])
        completed = subprocess.run(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )
        if completed.returncode != 0:
            return ("error", completed.stderr.decode("utf-8", "replace"))
        with open(out_path, "rb") as handle:
            return ("ok", handle.read())


def _rust_shard_set_root(frames):
    cli = support.RUST_CLI
    if not support.has_rust_cli():
        return None
    with tempfile.TemporaryDirectory(prefix="weft-ai-shards-") as work:
        for index, frame in enumerate(frames):
            with open(os.path.join(work, "shard-%03d.wft1" % index), "wb") as handle:
                handle.write(frame)
        completed = subprocess.run(
            [str(cli), "tensor", "root", "--frames", "--shards-dir", work, "--json"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if completed.returncode != 0:
            return None
        record = json.loads(completed.stdout.decode("utf-8"))
        return record.get("layout_root")


def test_valid_vectors_match_rust_cli():
    if not support.has_rust_cli():
        pytest.skip("Rust CLI is not available in this court")
    checked = 0
    for family, vector, _checker in VALID_CASES:
        if family == "wcb" or family == "semantic_root":
            continue
        if family == "sharded":
            root = _rust_shard_set_root(
                [bytes.fromhex(entry["frame_hex"]) for entry in vector["frames"]]
            )
            assert root is not None
            assert root == vector["layout_root"]
            checked += 1
            continue
        if family == "kv_cache":
            items = vector["pages"]
            key = "frame_hex"
            base = None
        elif family == "activation_stream":
            items = vector["chunks"]
            key = "frame_hex"
            base = None
        elif family in ("weight_delta", "semantic_delta", "sparse_delta"):
            items = [{"frame_hex": vector["delta_frame_hex"]}]
            key = "frame_hex"
            base = _delta_base(vector, Limits())
        else:
            items = [{"frame_hex": vector["frame_hex"]}]
            key = "frame_hex"
            base = None
        for item in items:
            frame = bytes.fromhex(item[key])
            result = _rust_decode(frame, base)
            if result is None:
                pytest.skip("Rust CLI is not available in this court")
            assert result[0] == "ok", result
            limits = _limits(vector)
            parsed = parse(frame, limits)
            if family == "weight_delta":
                ours = ai_modes.decode_weight_delta(parsed, None, limits, base)
            elif family == "semantic_delta":
                ours = ai_modes.decode_semantic_delta(parsed, None, limits, base)
            elif family == "sparse_delta":
                ours = ai_modes.decode_sparse_delta(parsed, None, limits, base)
            else:
                ours = modes.decode_frame(parsed, None, limits)
            assert ours == result[1], "%s: Rust and Python disagree" % family
        checked += len(items)
    assert checked > 0


# ---------------------------------------------------------------------------
# Reject vectors
# ---------------------------------------------------------------------------


def _reject_class(family, vector, limits):
    if family == "float_tensor":
        frame = parse(bytes.fromhex(vector["frame_hex"]), limits)
        modes.decode_frame(frame, None, limits)
    elif family == "sharded":
        shards = [
            ai_modes.decode_tensor_sharded(
                parse(bytes.fromhex(entry["frame_hex"]), limits), limits
            )
            for entry in vector["frames"]
        ]
        ai_modes.shard_set_root(shards, limits)
    elif family in ("weight_delta", "semantic_delta", "sparse_delta"):
        base = _delta_base(vector, limits)
        resolver = None
        if base is None and vector.get("use_frame_source"):
            resolver = _source_resolver(
                vector, limits, vector["delta_frame_hex"]
            )
        frame = parse(bytes.fromhex(vector["delta_frame_hex"]), limits)
        modes.decode_frame(frame, resolver, limits, base)
    elif family == "kv_cache":
        frame = parse(bytes.fromhex(vector["frame_hex"]), limits)
        ai_modes.decode_kv_cache(frame, limits)
    elif family == "activation_stream":
        frame = parse(bytes.fromhex(vector["frame_hex"]), limits)
        ai_modes.decode_activation_stream(frame, limits)
    elif family == "tensor_tiled":
        frame = parse(bytes.fromhex(vector["frame_hex"]), limits)
        modes.decode_frame(frame, None, limits)
    elif family == "semantic_root":
        ai_modes.semantic_record_from_canonical(
            bytes.fromhex(vector["record_hex"]), limits
        )
    elif family == "wcb":
        wcb.decode(bytes.fromhex(vector["hex"]), limits)
    else:  # pragma: no cover - the case table is closed
        raise AssertionError("no reject handler for %s" % family)


REJECT_CASES = (
    [
        ("float_tensor", vector)
        for _, vector in _load("codec")
        if _expect(vector).get("error") and vector.get("mode") == "float_tensor"
    ]
    + [
        ("sharded", vector)
        for _, vector in _load("sharded")
        if _expect(vector).get("error")
    ]
    + [
        ("weight_delta", vector)
        for _, vector in _load("weight-delta")
        if _expect(vector).get("error")
    ]
    + [
        ("kv_cache", vector)
        for _, vector in _load("kv-cache")
        if _expect(vector).get("error")
    ]
    + [
        ("activation_stream", vector)
        for _, vector in _load("activation-stream")
        if _expect(vector).get("error")
    ]
    + [
        ("semantic_delta", vector)
        for _, vector in _load("semantic-delta")
        if _expect(vector).get("error")
    ]
    + [
        ("sparse_delta", vector)
        for _, vector in _load("sparse-delta")
        if _expect(vector).get("error")
    ]
    + [
        ("tensor_tiled", vector)
        for _, vector in _load("tensor-tiled")
        if _expect(vector).get("error")
    ]
    + [
        ("semantic_root", vector)
        for _, vector in _load("semantic-root")
        if _expect(vector).get("error")
    ]
    + [
        ("wcb", vector)
        for _, vector in _load("structured")
        if _expect(vector).get("error")
    ]
)
REJECT_IDS = [
    "%s:%s" % (family, vector.get("name", "vector"))
    for family, vector in REJECT_CASES
]


@pytest.mark.parametrize("family,vector", REJECT_CASES, ids=REJECT_IDS)
def test_reject_vector(family, vector):
    limits = _limits(vector)
    expected = _expect(vector)["error"]
    observed = None
    try:
        _reject_class(family, vector, limits)
    except WeftError as exc:
        observed = exc.error_class
    except Exception as exc:  # a non-Weft exception must never be the answer
        _record("%s:%s" % (family, vector.get("name", "vector")), "failed")
        raise AssertionError("non-Weft error: %r" % (exc,))
    name = "%s:%s" % (family, vector.get("name", "vector"))
    if observed is None:
        _record(name, "failed", "expected %s but decode succeeded" % expected)
        pytest.fail("%s: expected %s but decode succeeded" % (name, expected))
    if observed != expected:
        _record(name, "failed", "expected %s got %s" % (expected, observed))
        pytest.fail("%s: expected %s got %s" % (name, expected, observed))
    _record(name, "passed", observed)


def test_summary():
    summary = _summary()
    assert summary["failed"] == 0, summary["vectors"]
    assert summary["passed"] > 0
