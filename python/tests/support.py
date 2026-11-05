"""Shared helpers for the Python court tests."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

from weftcodec import wcb
from weftcodec.dependency import DictResolver
from weftcodec.hashing import content_root
from weftcodec.limits import Limits


def repo_root() -> Path:
    override = os.environ.get("WEFT_REPO_ROOT")
    if override:
        return Path(override).resolve()
    # .../python/tests/support.py -> .../python -> repo root
    return Path(__file__).resolve().parents[2]


VECTORS = repo_root() / "WeftCodec" / "conformance" / "v1"
RUST_CLI = Path(
    os.environ.get(
        "WEFTCODEC_CLI", "/work/.docker-cache/target-rust/release/weftcodec"
    )
)


def has_rust_cli() -> bool:
    return RUST_CLI.is_file() and os.access(RUST_CLI, os.X_OK)


def load_json(path: Path) -> dict:
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def frame_vectors():
    """Yield ``(name, vector)`` for every frame-bearing JSON vector.

    Some AI-mode vectors store one frame per chunk/page (activation-stream and
    kv-cache); those nested frames are yielded with a unique ``name``. Reject
    vectors use the older ``expect_error`` key, normalized to ``expect`` here.
    """
    for path in sorted(VECTORS.rglob("*.json")):
        if path.name == "PROVENANCE.json":
            continue
        vector = load_json(path)
        expect = vector.get("expect")
        if expect is None and "expect_error" in vector:
            expect = {"error": vector["expect_error"]}
        base_name = "%s/%s" % (path.parent.name, vector.get("name", path.stem))

        if "frame_hex" in vector:
            normalized = dict(vector)
            normalized["expect"] = expect or {"ok": True}
            normalized.setdefault("name", base_name)
            yield base_name, normalized

        for collection in ("chunks", "pages"):
            for index, item in enumerate(vector.get(collection, [])):
                if "frame_hex" not in item:
                    continue
                nested = {
                    "name": "%s#%s%d" % (base_name, collection, index),
                    "frame_hex": item["frame_hex"],
                    "plaintext_hex": item.get("elements_hex", ""),
                    "limits": vector.get("limits"),
                    "dependencies": vector.get("dependencies", []),
                    "expect": expect or {"ok": True},
                }
                yield nested["name"], nested


def vector_limits(vector: dict) -> Limits:
    return Limits.from_overrides(vector.get("limits"))


def vector_resolver(vector: dict) -> DictResolver:
    resolver = DictResolver()
    for dependency in vector.get("dependencies", []):
        resolver.add(dependency["hash"], bytes.fromhex(dependency["bytes_hex"]))
    return resolver


def vector_mode_value(vector: dict) -> "int | None":
    if "mode_value" in vector:
        return int(vector["mode_value"])
    raw = bytes.fromhex(vector["frame_hex"])
    if len(raw) >= 8 and raw[0:4] == b"WFT1":
        return raw[7]
    return None


def wcb_map(pairs) -> bytes:
    return wcb.encode(dict(pairs))


def write_summary(env_name: str, payload: dict) -> None:
    target = os.environ.get(env_name)
    if not target:
        return
    path = Path(target)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def python_executable() -> str:
    return sys.executable
