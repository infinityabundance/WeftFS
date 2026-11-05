#!/usr/bin/env python3
"""Generate or check `conformance/manifest.json`.

The manifest enumerates every committed conformance vector: its path, kind,
mode (when a frame mode applies), the expected outcome (a content root and
decoded SHA-256, or a normative error class), supersession, the SHA-256 of the
vector file, and the byte-identical mirror path in the independent Go codec.

This tool is deterministic and stdlib-only. It never compiles or executes an
implementation. `--check` regenerates the manifest in memory and fails if the
committed file differs, so drift is caught by `scripts/verify-all.sh` without
Docker.

Usage:
  scripts/gen-conformance-manifest.py            # write conformance/manifest.json
  scripts/gen-conformance-manifest.py --check    # fail on drift
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import struct
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join(ROOT, "conformance", "manifest.json")

CODEC_RUST = "WeftCodec/conformance/v1"
CODEC_GO = "WeftCodec-Go/conformance/v1"
STORE_RUST = "conformance/store/v1"
STORE_GO = "WeftFS-Go/conformance/store/v1"
CHECKPOINT_RUST = "conformance/checkpoint/v1"

# Frame modes implemented by WEFT-AI-CODEC and WEFTCODEC-FORMAT.
MODE_NAMES = {
    0: "raw",
    1: "zstd",
    2: "reference",
    3: "delta",
    4: "dictionary",
    5: "structured",
    6: "tensor",
    7: "tokens",
    8: "quantized_tensor",
    9: "float_tensor",
    10: "tensor_sharded",
    11: "weight_delta",
    12: "kv_cache",
    13: "activation_stream",
    14: "semantic_delta",
    15: "tensor_tiled",
}

# Directory -> (kind, subkind, mode, expectation builder name)
CODEC_DIRS = {
    "codec": ("frame", None, None, "frame"),
    "adversarial": ("frame", "adversarial", None, "adversarial"),
    "sharded": ("frame", "sharded", 10, "sharded"),
    "weight-delta": ("frame", "weight_delta", 11, "weight_delta"),
    "kv-cache": ("frame", "kv_cache", 12, "kv_cache"),
    "activation-stream": ("frame", "activation_stream", 13, "activation_stream"),
    "semantic-delta": ("frame", "semantic_delta", 14, "semantic_delta"),
    "tensor-tiled": ("frame", "tensor_tiled", 15, "frame"),
    "structured": ("wcb", None, None, "wcb"),
    "selectors": ("selector", None, None, "selector"),
    "context": ("context", "context_root", None, "context"),
    "causal": ("context", "causal_closure", None, "causal"),
    "tensor": ("tensor", None, None, "tensor"),
    "semantic-root": ("tensor", "semantic_root", None, "semantic_root"),
    "capability": ("capability", None, None, "capability"),
}

IDENTITY_KINDS = {
    "wcb",
    "selector",
    "context",
    "tensor",
    "checkpoint",
    "capability",
}


# ---------------------------------------------------------------------------
# BLAKE3 (domain-separated hashing), stdlib-only.
# ---------------------------------------------------------------------------

_IV = [
    0x6A09E667,
    0xBB67AE85,
    0x3C6EF372,
    0xA54FF53A,
    0x510E527F,
    0x9B05688C,
    0x1F83D9AB,
    0x5BE0CD19,
]
_MSG_PERMUTATION = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8]
_CHUNK_START = 1
_CHUNK_END = 2
_PARENT = 4
_ROOT = 8


def _rotr(value: int, count: int) -> int:
    return ((value >> count) | (value << (32 - count))) & 0xFFFFFFFF


def _g(state, a, b, c, d, mx, my):
    state[a] = (state[a] + state[b] + mx) & 0xFFFFFFFF
    state[d] = _rotr(state[d] ^ state[a], 16)
    state[c] = (state[c] + state[d]) & 0xFFFFFFFF
    state[b] = _rotr(state[b] ^ state[c], 12)
    state[a] = (state[a] + state[b] + my) & 0xFFFFFFFF
    state[d] = _rotr(state[d] ^ state[a], 8)
    state[c] = (state[c] + state[d]) & 0xFFFFFFFF
    state[b] = _rotr(state[b] ^ state[c], 7)


def _round(state, message):
    _g(state, 0, 4, 8, 12, message[0], message[1])
    _g(state, 1, 5, 9, 13, message[2], message[3])
    _g(state, 2, 6, 10, 14, message[4], message[5])
    _g(state, 3, 7, 11, 15, message[6], message[7])
    _g(state, 0, 5, 10, 15, message[8], message[9])
    _g(state, 1, 6, 11, 12, message[10], message[11])
    _g(state, 2, 7, 8, 13, message[12], message[13])
    _g(state, 3, 4, 9, 14, message[14], message[15])


def _compress(cv, block, counter, block_len, flags):
    message = list(struct.unpack("<16I", block))
    state = list(cv) + _IV[0:4] + [
        counter & 0xFFFFFFFF,
        (counter >> 32) & 0xFFFFFFFF,
        block_len,
        flags,
    ]
    for _ in range(7):
        _round(state, message)
        message = [message[index] for index in _MSG_PERMUTATION]
    out = [0] * 16
    for index in range(8):
        out[index] = (state[index] ^ state[index + 8]) & 0xFFFFFFFF
        out[index + 8] = (state[index + 8] ^ cv[index]) & 0xFFFFFFFF
    return out


class _Output:
    def __init__(self, input_cv, block, counter, block_len, flags):
        self.input_cv = input_cv
        self.block = block
        self.counter = counter
        self.block_len = block_len
        self.flags = flags

    def chaining_value(self):
        return _compress(
            self.input_cv, self.block, self.counter, self.block_len, self.flags
        )[:8]

    def root_bytes(self, length=32):
        words = _compress(
            self.input_cv, self.block, self.counter, self.block_len, self.flags | _ROOT
        )
        return b"".join(struct.pack("<I", word) for word in words)[:length]


def _chunk_output(chunk, counter):
    cv = list(_IV)
    blocks = [chunk[index : index + 64] for index in range(0, len(chunk), 64)]
    if not blocks:
        blocks = [b""]
    for index, block in enumerate(blocks):
        flags = 0
        if index == 0:
            flags |= _CHUNK_START
        if index == len(blocks) - 1:
            flags |= _CHUNK_END
            return _Output(
                cv, block.ljust(64, b"\0"), counter, len(block), flags
            )
        cv = _compress(cv, block.ljust(64, b"\0"), counter, 64, flags)[:8]
    raise AssertionError("unreachable")


def _parent_output(left_cv, right_cv):
    block = b"".join(struct.pack("<I", word) for word in left_cv + right_cv)
    return _Output(list(_IV), block, 0, 64, _PARENT)


def blake3(data: bytes) -> bytes:
    chunks = [data[index : index + 1024] for index in range(0, len(data), 1024)]
    if not chunks:
        chunks = [b""]
    stack = []
    processed = 0
    for chunk in chunks[:-1]:
        cv = _chunk_output(chunk, processed).chaining_value()
        processed += 1
        total = processed
        while total & 1 == 0:
            left = stack.pop()
            cv = _parent_output(left, cv).chaining_value()
            total >>= 1
        stack.append(cv)
    output = _chunk_output(chunks[-1], processed)
    while stack:
        output = _parent_output(stack.pop(), output.chaining_value())
    return output.root_bytes(32)


def hash_domain(domain: str, data: bytes) -> bytes:
    encoded = domain.encode("ascii")
    return blake3(struct.pack("<I", len(encoded)) + encoded + data)


# ---------------------------------------------------------------------------
# Helpers.
# ---------------------------------------------------------------------------


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_bytes(path: str) -> bytes:
    with open(path, "rb") as handle:
        return handle.read()


def load_json(path: str):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def rel(path: str) -> str:
    return os.path.relpath(path, ROOT)


def frame_mode(raw_hex):
    if not raw_hex:
        return None
    try:
        raw = bytes.fromhex(raw_hex)
    except ValueError:
        return None
    if len(raw) >= 8 and raw[0:4] == b"WFT1":
        return raw[7]
    return None


def ok(content_root=None, decoded_sha256=None):
    return {
        "ok": True,
        "content_root": content_root,
        "decoded_sha256": decoded_sha256,
    }


def error(cls):
    return {"error": cls}


def decode_sha(hex_text):
    if hex_text is None:
        return None
    try:
        return sha256(bytes.fromhex(hex_text))
    except ValueError:
        return None


def tree_files(root_abs):
    out = {}
    if not os.path.isdir(root_abs):
        return out
    for dirpath, _dirs, files in os.walk(root_abs):
        for name in sorted(files):
            path = os.path.join(dirpath, name)
            out[rel(path)] = path
    return out


def aggregate(digests):
    payload = "".join("%s  %s\n" % (digests[path], path) for path in sorted(digests))
    return sha256(payload.encode("utf-8"))


# ---------------------------------------------------------------------------
# Expectation extraction per vector family.
# ---------------------------------------------------------------------------


def expectation_codec(vector):
    expect = vector.get("expect") or {}
    if "error" in expect:
        return error(expect["error"]), None
    plaintext_hex = vector.get("plaintext_hex")
    root = vector.get("plaintext_hash") or vector.get("content_hash")
    if plaintext_hex is not None:
        computed = hash_domain("weft/chunk/v1", bytes.fromhex(plaintext_hex)).hex()
        if root is not None and computed != root:
            raise SystemExit(
                "BLAKE3 self-check failed for %s: recorded %s computed %s"
                % (vector.get("name"), root, computed)
            )
        root = computed
    return ok(root, decode_sha(plaintext_hex)), None


def expectation_adversarial(vector):
    cls = vector.get("expect_class")
    if not cls:
        raise SystemExit("adversarial vector %r has no expect_class" % vector.get("name"))
    return error(cls), None


def expectation_wcb(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    hex_text = vector.get("canonical_hex")
    if hex_text is None:
        raise SystemExit("WCB vector %r has no canonical_hex" % vector.get("name"))
    raw = bytes.fromhex(hex_text)
    return ok(hash_domain("weft/structured/v1", raw).hex(), sha256(raw)), None


def expectation_selector(vector):
    return ok(vector["selector_id"], decode_sha(vector.get("canonical_hex"))), None


def expectation_context(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    return ok(vector["context_root_id"], decode_sha(vector.get("canonical_hex"))), None


def expectation_causal(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    return ok(vector["closure_hash"], decode_sha(vector.get("canonical_hex"))), None


def expectation_tensor(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    kind = vector.get("kind")
    root = vector.get("logical_root") if kind in ("logical", "shards") else None
    decoded = decode_sha(vector.get("values_hex"))
    if decoded is None:
        decoded = decode_sha(vector.get("canonical_hex"))
    return ok(root, decoded), kind


def expectation_semantic_root(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    return ok(vector.get("semantic_root"), decode_sha(vector.get("values_hex"))), None


def expectation_sharded(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    return ok(vector.get("layout_root"), None), None


def expectation_weight_delta(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    return ok(vector.get("target_hash"), decode_sha(vector.get("target_hex"))), None


def expectation_semantic_delta(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    return ok(vector.get("target_hash"), decode_sha(vector.get("target_hex"))), None


def expectation_kv_cache(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    elements = b"".join(
        bytes.fromhex(page.get("elements_hex", ""))
        for page in sorted(vector.get("pages", []), key=lambda p: p["page_index"])
    )
    return ok(None, sha256(elements)), None


def expectation_activation_stream(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    elements = b"".join(
        bytes.fromhex(chunk.get("elements_hex", ""))
        for chunk in sorted(vector.get("chunks", []), key=lambda c: c["chunk_index"])
    )
    # No aggregate CLI identity is exposed for a stream; the frozen
    # `stream_root` remains in the vector file. The runner checks decoded bytes.
    return ok(None, sha256(elements)), None


def expectation_capability(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    return ok(vector.get("envelope_id"), decode_sha(vector.get("canonical_hex"))), None


def expectation_checkpoint(vector):
    if "expect_error" in vector:
        return error(vector["expect_error"]), None
    return ok(vector.get("manifest_root"), decode_sha(vector.get("canonical_hex"))), None


EXPECTATIONS = {
    "frame": expectation_codec,
    "adversarial": expectation_adversarial,
    "wcb": expectation_wcb,
    "selector": expectation_selector,
    "context": expectation_context,
    "causal": expectation_causal,
    "tensor": expectation_tensor,
    "semantic_root": expectation_semantic_root,
    "sharded": expectation_sharded,
    "weight_delta": expectation_weight_delta,
    "semantic_delta": expectation_semantic_delta,
    "kv_cache": expectation_kv_cache,
    "activation_stream": expectation_activation_stream,
    "capability": expectation_capability,
    "checkpoint": expectation_checkpoint,
}


# ---------------------------------------------------------------------------
# Entry construction.
# ---------------------------------------------------------------------------


def build_codec_entries():
    entries = []
    base = os.path.join(ROOT, CODEC_RUST)
    for subdir in sorted(CODEC_DIRS):
        directory = os.path.join(base, subdir)
        if not os.path.isdir(directory):
            continue
        kind, subkind, fixed_mode, builder = CODEC_DIRS[subdir]
        for name in sorted(os.listdir(directory)):
            if not name.endswith(".json") or name == "PROVENANCE.json":
                continue
            path = os.path.join(directory, name)
            vector = load_json(path)
            expect, tensor_kind = EXPECTATIONS[builder](vector)
            mode = fixed_mode
            if mode is None:
                mode = vector.get("mode_value")
            if mode is None and kind == "frame":
                mode = frame_mode(vector.get("frame_hex") or vector.get("input_hex"))
            entry = {
                "path": rel(path),
                "kind": kind,
                "vector_name": vector.get("name", os.path.splitext(name)[0]),
                "sha256": sha256(read_bytes(path)),
                "expect": expect,
            }
            if subkind:
                entry["subkind"] = subkind
            if tensor_kind:
                entry["subkind"] = "tensor_" + tensor_kind
            if isinstance(mode, int):
                entry["mode"] = mode
                entry["mode_name"] = MODE_NAMES.get(mode)
            if "verb" in vector:
                entry["operation"] = vector["verb"]
            if vector.get("superseded_by"):
                entry["superseded_by"] = vector["superseded_by"]
            entries.append(entry)
    return entries


def build_checkpoint_entries():
    entries = []
    directory = os.path.join(ROOT, CHECKPOINT_RUST)
    if not os.path.isdir(directory):
        return entries
    for name in sorted(os.listdir(directory)):
        if not name.endswith(".json"):
            continue
        path = os.path.join(directory, name)
        vector = load_json(path)
        expect, _ = expectation_checkpoint(vector)
        entries.append(
            {
                "path": rel(path),
                "kind": "checkpoint",
                "vector_name": vector.get("name", os.path.splitext(name)[0]),
                "sha256": sha256(read_bytes(path)),
                "expect": expect,
            }
        )
    return entries


def build_causal_versions_entry():
    path = os.path.join(ROOT, "conformance", "context", "v1", "causal_versions.json")
    if not os.path.isfile(path):
        return []
    vector = load_json(path)
    closure = vector["closure"]
    return [
        {
            "path": rel(path),
            "kind": "context",
            "subkind": "causal_versions",
            "vector_name": "causal_versions",
            "sha256": sha256(read_bytes(path)),
            "expect": ok(closure.get("causal_hash"), decode_sha(closure.get("canonical_hex"))),
        }
    ]


def build_fixture_entry():
    root_abs = os.path.join(ROOT, STORE_RUST)
    if not os.path.isdir(root_abs):
        return []
    files = tree_files(root_abs)
    digests = {path: sha256(read_bytes(full)) for path, full in files.items()}
    expected = load_json(os.path.join(root_abs, "EXPECTED.json"))
    return [
        {
            "path": STORE_RUST,
            "kind": "store",
            "vector_name": "store_fixture_v1",
            "sha256": aggregate(digests),
            "sha256_kind": "directory-aggregate",
            "file_count": len(files),
            "expect": ok(expected.get("snapshot_root"), None),
        }
    ]


def build_fuzz_entry():
    path = os.path.join(ROOT, "conformance", "fuzz", "v1", "seeds.txt")
    if not os.path.isfile(path):
        return []
    data = read_bytes(path)
    seeds = [line for line in data.decode("ascii").splitlines() if line.strip()]
    return [
        {
            "path": rel(path),
            "kind": "fuzz",
            "vector_name": "fuzz_seeds_v1",
            "sha256": sha256(data),
            "seed_count": len(seeds),
            "expect": ok(None, None),
        }
    ]


def attach_supersession(entries):
    by_dir = {}
    for entry in entries:
        directory = os.path.dirname(entry["path"])
        by_dir.setdefault(directory, {})[entry["vector_name"]] = entry
    for entry in entries:
        target = entry.get("superseded_by")
        if not target:
            continue
        directory = os.path.dirname(entry["path"])
        candidates = by_dir.get(directory, {})
        newer = candidates.get(target)
        if newer is None:
            # One frozen vector records a target without its numeric prefix.
            # Resolve an unambiguous suffix; never guess between candidates.
            suffix = [
                candidate
                for name, candidate in candidates.items()
                if name.endswith(target)
            ]
            if len(suffix) == 1:
                newer = suffix[0]
        if newer is None:
            raise SystemExit(
                "%s names superseded_by %r but no such vector exists"
                % (entry["path"], target)
            )
        newer.setdefault("supersedes", []).append(entry["vector_name"])


def attach_mirrors(entries):
    mirrors = {
        "WeftCodec/conformance/v1": CODEC_GO,
    }
    for entry in entries:
        for prefix, mirror_prefix in mirrors.items():
            if not entry["path"].startswith(prefix + "/"):
                continue
            candidate = mirror_prefix + entry["path"][len(prefix) :]
            if not os.path.isfile(os.path.join(ROOT, candidate)):
                raise SystemExit(
                    "mirror missing for %s: %s" % (entry["path"], candidate)
                )
            digest = sha256(read_bytes(os.path.join(ROOT, candidate)))
            if digest != entry["sha256"]:
                raise SystemExit(
                    "mirror drift: %s and %s differ" % (entry["path"], candidate)
                )
            entry.setdefault("mirrors", []).append(candidate)


def compare_tree(prefix, mirror_prefix, allow_extra_in_mirror=()):
    left_files = tree_files(os.path.join(ROOT, prefix))
    right_files = tree_files(os.path.join(ROOT, mirror_prefix))
    left_rel = {os.path.relpath(path, os.path.join(ROOT, prefix)): path for path in left_files.values()}
    right_rel = {os.path.relpath(path, os.path.join(ROOT, mirror_prefix)): path for path in right_files.values()}
    left_digests = {path: sha256(read_bytes(full)) for path, full in left_rel.items()}
    right_digests = {path: sha256(read_bytes(full)) for path, full in right_rel.items()}
    only_left = sorted(set(left_digests) - set(right_digests))
    only_right = sorted(set(right_digests) - set(left_digests))
    differing = sorted(
        path
        for path in set(left_digests) & set(right_digests)
        if left_digests[path] != right_digests[path]
    )
    unexpected_right = [
        path
        for path in only_right
        if os.path.basename(path) not in allow_extra_in_mirror
    ]
    identical = not only_left and not differing and not unexpected_right
    common = sorted(set(left_digests) & set(right_digests))
    common_left = {path: left_digests[path] for path in common}
    result = {
        "from": prefix,
        "to": mirror_prefix,
        "files_from": len(left_digests),
        "files_to": len(right_digests),
        "common_files": len(common),
        "identical": identical,
        "aggregate_sha256_from": aggregate(left_digests),
        "aggregate_sha256_to": aggregate(right_digests),
        "aggregate_sha256_common": aggregate(common_left),
        "only_from": only_left,
        "only_to": only_right,
        "differing": differing,
    }
    if not identical:
        raise SystemExit(
            "cross-repo identity failure for %s: missing=%s differing=%s extra=%s"
            % (prefix, only_left, differing, unexpected_right)
        )
    return result


def build_manifest():
    entries = []
    entries.extend(build_codec_entries())
    entries.extend(build_checkpoint_entries())
    entries.extend(build_causal_versions_entry())
    entries.extend(build_fixture_entry())
    entries.extend(build_fuzz_entry())
    entries.sort(key=lambda entry: entry["path"])
    attach_supersession(entries)
    attach_mirrors(entries)

    counts = {}
    modes = {}
    identity = {}
    for entry in entries:
        counts[entry["kind"]] = counts.get(entry["kind"], 0) + 1
        if entry["kind"] in IDENTITY_KINDS:
            bucket = identity.setdefault(
                entry["kind"], {"valid": 0, "error": 0, "vectors": []}
            )
            bucket["valid" if entry["expect"].get("ok") else "error"] += 1
            bucket["vectors"].append(entry["path"])
        if isinstance(entry.get("mode"), int):
            bucket = modes.setdefault(
                str(entry["mode"]), {"valid": 0, "error": 0, "vectors": []}
            )
            if entry.get("superseded_by"):
                # Historical expectations are frozen but not current coverage.
                continue
            bucket["valid" if entry["expect"].get("ok") else "error"] += 1
            bucket["vectors"].append(entry["path"])

    canonical_digests = {
        entry["path"]: entry["sha256"]
        for entry in entries
        if entry["kind"] not in ("store", "fuzz")
    }

    manifest = {
        "schema": "weft.conformance.manifest.v1",
        "generated_by": "scripts/gen-conformance-manifest.sh",
        "description": (
            "Deterministic enumeration of every conformance vector. Paths are "
            "relative to the program home. `content_root` is the BLAKE3 "
            "domain-separated identity recorded by the vector (for WCB, "
            "H(\"weft/structured/v1\", canonical bytes)); `decoded_sha256` is "
            "SHA-256 over the exact decoded byte string the vector records. "
            "Null means the format does not define that value for the kind."
        ),
        "vector_count": len(entries),
        "counts": dict(sorted(counts.items())),
        "modes": dict(sorted(modes.items(), key=lambda item: int(item[0]))),
        "identity_types": dict(sorted(identity.items())),
        "aggregate_sha256": aggregate(canonical_digests),
        "aggregate_sha256_definition": (
            "SHA-256 over sorted '<sha256>  <path>\\n' lines for every entry "
            "whose kind is not `store` or `fuzz`."
        ),
        "mirrors": [
            compare_tree(CODEC_RUST, CODEC_GO),
            compare_tree(STORE_RUST, STORE_GO, allow_extra_in_mirror=("PROVENANCE.md",)),
        ],
        "vectors": entries,
    }
    return manifest


def serialized(manifest):
    return json.dumps(manifest, indent=2, sort_keys=True) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if conformance/manifest.json is stale",
    )
    args = parser.parse_args(argv)

    manifest = build_manifest()
    rendered = serialized(manifest)

    if args.check:
        if not os.path.isfile(MANIFEST):
            print("conformance manifest is missing: %s" % rel(MANIFEST), file=sys.stderr)
            return 1
        with open(MANIFEST, "r", encoding="utf-8") as handle:
            committed = handle.read()
        if committed != rendered:
            print(
                "conformance manifest is stale: regenerate with "
                "scripts/gen-conformance-manifest.sh",
                file=sys.stderr,
            )
            return 1
        print(
            "conformance manifest: OK (%d entries, aggregate %s)"
            % (manifest["vector_count"], manifest["aggregate_sha256"])
        )
        return 0

    os.makedirs(os.path.dirname(MANIFEST), exist_ok=True)
    with open(MANIFEST, "w", encoding="utf-8") as handle:
        handle.write(rendered)
    print(
        "wrote %s (%d entries, aggregate %s)"
        % (rel(MANIFEST), manifest["vector_count"], manifest["aggregate_sha256"])
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
