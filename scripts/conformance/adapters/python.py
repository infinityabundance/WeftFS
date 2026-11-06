#!/usr/bin/env python3
"""Reference adapter: independent Python `weftcodec` reader.

The adapter imports the Python package directly (never the Rust or Go
implementation) and exercises every vector kind the reader claims:

* codec frames, modes 0-16;
* adversarial decode vectors (limits, dependency depth/cycle, frame size);
* TENSOR_SHARDED shard sets (decode + shard-set root);
* WEIGHT_DELTA, SEMANTIC_DELTA, SPARSE_DELTA (with ``--base`` or a frame
  source);
* KV_CACHE pages and ACTIVATION_STREAM chunks;
* TENSOR_TILED frames;
* semantic roots;
* canonical WCB values.

Kinds outside the WeftCodec reader (selectors, Context Roots, capability
envelopes, checkpoint manifests, WEFT-TENSOR layout/shard containers, and the
WFA1 archive) are reported as explicit skips with a reason. A skip is never
evidence of a pass.
"""

import hashlib
import json
import os
import sys

_PYTHONPATH = os.environ.get("WEFT_PYTHONPATH")
if _PYTHONPATH and _PYTHONPATH not in sys.path:
    sys.path.insert(0, _PYTHONPATH)

from weftcodec import ai_modes, modes, wcb  # noqa: E402
from weftcodec.dependency import (  # noqa: E402
    DictResolver,
    FrameSourceResolver,
)
from weftcodec.errors import WeftError  # noqa: E402
from weftcodec.frame import parse  # noqa: E402
from weftcodec.hashing import hash_domain  # noqa: E402
from weftcodec.limits import Limits  # noqa: E402

ERROR_CLASSES = (
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
)


def emit(result):
    print(json.dumps(result, sort_keys=True))


def ok(content_root_hex=None, decoded_sha256=None):
    emit(
        {
            "result": "ok",
            "content_root": content_root_hex,
            "decoded_sha256": decoded_sha256,
        }
    )


def err(cls):
    emit({"result": "error", "class": cls})


def skip(reason):
    emit({"result": "skip", "reason": reason})


def fail(message):
    print("adapter failure: %s" % message, file=sys.stderr)
    raise SystemExit(2)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def vector_subdir(path):
    parts = path.replace("\\", "/").split("/")
    for index, part in enumerate(parts):
        if part != "conformance":
            continue
        rest = parts[index + 1 :]
        if rest and rest[0] == "v1":
            rest = rest[1:]
        elif len(rest) >= 2 and rest[1] == "v1":
            rest = [rest[0]] + rest[2:]
        return rest[0] if rest else ""
    return ""


def limits_for(vector):
    overrides = dict(vector.get("limits") or {})
    flags = vector.get("flags")
    if isinstance(flags, dict):
        for key, value in flags.items():
            name = key[2:].replace("-", "_") if key.startswith("--") else key
            overrides[name] = int(value)
    known = set(Limits.__dataclass_fields__)
    unknown = set(overrides) - known
    if unknown:
        raise ValueError(
            "adapter has no Python limit for %s" % ", ".join(sorted(unknown))
        )
    return Limits.from_overrides(overrides)


def _entries(vector, camel, snake):
    entries = vector.get(camel)
    if entries is None:
        entries = vector.get(snake)
    return entries or []


def plain_resolver(vector):
    entries = _entries(vector, "dependencies", "deps")
    if not entries:
        return None
    resolver = DictResolver()
    for entry in entries:
        text = entry.get("bytes_hex", entry.get("hex"))
        if text is None:
            raise ValueError("dependency entry has no bytes")
        resolver.add(entry["hash"], bytes.fromhex(text))
    return resolver


def frame_source_resolver(vector, limits, root_hex=None, extra_frames=None):
    frames = {}
    for entry in _entries(vector, "frame_source", "frame_source"):
        frames[bytes.fromhex(entry["hash"])] = bytes.fromhex(entry["hex"])
    if extra_frames:
        frames.update(extra_frames)
    visiting = set()
    stats = {"fetched": 0, "decoded": 0}
    if root_hex is not None:
        # Mirror the Rust DAG resolver: the root frame is already fetched and
        # is already on the visiting stack while its dependency is resolved.
        # The root is also addressable by its own content hash, which is how a
        # cycle vector names itself as its base. A root that does not parse is
        # simply not seeded; the decode itself then reports the real class.
        root_bytes = bytes.fromhex(root_hex)
        try:
            root = parse(root_bytes, limits)
        except WeftError:
            root = None
        if root is not None:
            frames.setdefault(root.header.content_hash, root_bytes)
            visiting.add(root.header.content_hash)
            stats["fetched"] = len(root_bytes)
    if not frames:
        return None
    return FrameSourceResolver(frames, limits, visiting=visiting, stats=stats)


def _decode(frame_hex, limits, resolver=None, base_frame=None):
    data = bytes.fromhex(frame_hex)
    frame = parse(data, limits)
    plaintext = modes.decode_frame(frame, resolver, limits, base_frame)
    return frame, plaintext


def handle_frame_bytes(frame_hex, vector, limits):
    """Decode one ordinary frame and report its root and decoded SHA-256."""
    resolver = plain_resolver(vector)
    if resolver is None:
        source = frame_source_resolver(vector, limits)
        if source is not None:
            resolver = source
    try:
        frame, plaintext = _decode(frame_hex, limits, resolver)
    except WeftError as exc:
        err(exc.error_class)
        return
    ok(frame.header.content_hash.hex(), sha256(plaintext))


def handle_codec(vector, limits):
    handle_frame_bytes(vector["frame_hex"], vector, limits)


def handle_adversarial(vector, limits):
    resolver = plain_resolver(vector)
    if resolver is None:
        source = frame_source_resolver(
            vector, limits, root_hex=vector["input_hex"]
        )
        if source is not None:
            resolver = source
    frame_hex = vector["input_hex"]
    try:
        frame, plaintext = _decode(frame_hex, limits, resolver)
    except WeftError as exc:
        err(exc.error_class)
        return
    ok(frame.header.content_hash.hex(), sha256(plaintext))


def handle_sharded(vector, limits):
    frames = vector.get("frames")
    if not frames:
        skip("sharded vector has no frames")
        return
    try:
        shards = [
            ai_modes.decode_tensor_sharded(
                parse(bytes.fromhex(entry["frame_hex"]), limits), limits
            )
            for entry in frames
        ]
        root, _words = ai_modes.shard_set_root(shards, limits)
    except WeftError as exc:
        err(exc.error_class)
        return
    ok(root.hex(), None)


def handle_delta(vector, limits):
    delta_hex = vector.get("delta_frame_hex")
    if not delta_hex:
        skip("delta vector has no delta frame")
        return
    base_hex = vector.get("base_frame_hex")
    base_frame = bytes.fromhex(base_hex) if base_hex else None
    resolver = plain_resolver(vector)
    if resolver is None and vector.get("use_frame_source") and base_frame is None:
        resolver = frame_source_resolver(
            vector, limits, root_hex=delta_hex
        )
    try:
        frame, plaintext = _decode(delta_hex, limits, resolver, base_frame)
    except WeftError as exc:
        err(exc.error_class)
        return
    ok(frame.header.content_hash.hex(), sha256(plaintext))


def handle_kv_cache(vector, limits):
    if vector.get("frame_hex") is not None:
        handle_frame_bytes(vector["frame_hex"], vector, limits)
        return
    pages = vector.get("pages")
    if not pages:
        skip("kv-cache vector has no pages")
        return
    chunks = []
    for page in sorted(pages, key=lambda item: item.get("page_index", 0)):
        try:
            _frame, plaintext = _decode(page["frame_hex"], limits)
        except WeftError as exc:
            err(exc.error_class)
            return
        chunks.append(plaintext)
    ok(None, sha256(b"".join(chunks)))


def handle_activation_stream(vector, limits):
    if vector.get("frame_hex") is not None:
        handle_frame_bytes(vector["frame_hex"], vector, limits)
        return
    chunks = vector.get("chunks")
    if not chunks:
        skip("activation-stream vector has no chunks")
        return
    decoded = []
    for chunk in sorted(chunks, key=lambda item: item.get("chunk_index", 0)):
        try:
            _frame, plaintext = _decode(chunk["frame_hex"], limits)
        except WeftError as exc:
            err(exc.error_class)
            return
        decoded.append(plaintext)
    ok(None, sha256(b"".join(decoded)))


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
    if scheme == "block_quantize":
        block_size = policy.get("block_size")
        levels = policy.get("levels")
        return {
            "block_size": (
                None if block_size is None else wcb.U64(int(block_size))
            ),
            "levels": None if levels is None else wcb.U64(int(levels)),
            "scheme": "block_quantize",
        }
    return {"scheme": scheme}


def handle_semantic_root(vector, limits):
    if vector.get("record_hex") is not None:
        try:
            ai_modes.semantic_record_from_canonical(
                bytes.fromhex(vector["record_hex"]), limits
            )
        except WeftError as exc:
            err(exc.error_class)
            return
        ok(None, None)
        return
    try:
        policy = ai_modes.parse_semantic_policy(
            _policy_value(vector["policy"]),
            vector["dtype"],
            vector["element_count"],
        )
        values = bytes.fromhex(vector["values_hex"])
        root = ai_modes.semantic_root(
            vector["dtype"], vector["shape"], values, policy, limits
        )
    except WeftError as exc:
        err(exc.error_class)
        return
    ok(root.hex(), sha256(values))


def handle_wcb(vector, limits):
    if vector.get("canonical_hex") is not None:
        canonical = bytes.fromhex(vector["canonical_hex"])
        try:
            wcb.decode(canonical, limits)
        except WeftError as exc:
            err(exc.error_class)
            return
        ok(hash_domain("weft/structured/v1", canonical).hex(), sha256(canonical))
        return
    raw = bytes.fromhex(vector["hex"])
    try:
        wcb.decode(raw, limits)
    except WeftError as exc:
        err(exc.error_class)
        return
    ok(None, None)


def main():
    if len(sys.argv) != 2:
        fail("usage: adapter.py VECTOR.json")
    path = sys.argv[1]
    try:
        with open(path, "r", encoding="utf-8") as handle:
            vector = json.load(handle)
    except (OSError, ValueError) as exc:
        fail("cannot read vector %s: %s" % (path, exc))

    sub = vector_subdir(path)
    if sub in ("store", "fuzz", "PROVENANCE"):
        skip("composite fixture (runner skips it by default)")
        return
    if sub == "adversarial" and vector.get("verb") != "decode":
        skip("adversarial verb %r is a non-decoder surface" % vector.get("verb"))
        return
    try:
        limits = limits_for(vector)
    except ValueError as exc:
        fail(str(exc))
        return

    if sub == "codec":
        handle_codec(vector, limits)
    elif sub == "adversarial":
        handle_adversarial(vector, limits)
    elif sub == "sharded":
        handle_sharded(vector, limits)
    elif sub in ("weight-delta", "semantic-delta", "sparse-delta"):
        handle_delta(vector, limits)
    elif sub == "kv-cache":
        handle_kv_cache(vector, limits)
    elif sub == "activation-stream":
        handle_activation_stream(vector, limits)
    elif sub == "tensor-tiled":
        handle_frame_bytes(vector["frame_hex"], vector, limits)
    elif sub == "semantic-root":
        handle_semantic_root(vector, limits)
    elif sub == "structured":
        handle_wcb(vector, limits)
    elif sub == "tensor":
        skip("WEFT-TENSOR layout/shard containers are outside the WeftCodec reader")
    elif sub == "selectors":
        skip("WeftQL selector grammar is not implemented by the Python reader")
    elif sub == "context":
        skip("Context Root resolution is not implemented by the Python reader")
    elif sub == "causal":
        skip("causal closure identity is not implemented by the Python reader")
    elif sub == "capability":
        skip("capability envelope verification is not implemented by the Python reader")
    elif sub == "checkpoint":
        skip("weft.checkpoint.v1 manifests are not implemented by the Python reader")
    else:
        skip("no Python adapter for vector kind %r" % sub)


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as exc:  # an adapter crash is a failure, never a pass
        fail("unhandled %s: %s" % (type(exc).__name__, exc))
