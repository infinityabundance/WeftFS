"""Shared implementation of the third-party conformance adapter contract.

An adapter receives one vector path and prints exactly one JSON object:

    {"result": "ok", "content_root": "<hex or null>", "decoded_sha256": "<hex or null>"}
    {"result": "error", "class": "<normative class>"}
    {"result": "skip", "reason": "<why this implementation cannot run this kind>"}

This module implements that contract by shelling out to a shipped CLI. It does
not import implementation code and it never reads the manifest.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

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
CAPABILITY_REJECTIONS = (
    "BadSignature",
    "WrongGrantee",
    "Expired",
    "Revoked",
    "UnknownSchema",
    "NonCanonical",
    "Malformed",
)


class Skip(Exception):
    """The implementation has no CLI surface for this vector kind."""


class AdapterFailure(Exception):
    """The implementation could not be invoked, or its output was unreadable."""


def emit(result):
    print(json.dumps(result, sort_keys=True))
    return 0


def ok(content_root=None, decoded_sha256=None):
    return emit(
        {
            "result": "ok",
            "content_root": content_root,
            "decoded_sha256": decoded_sha256,
        }
    )


def err(cls):
    return emit({"result": "error", "class": cls})


def skip(reason):
    return emit({"result": "skip", "reason": reason})


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    with open(path, "rb") as handle:
        return sha256(handle.read())


def write_hex(path, text):
    with open(path, "wb") as handle:
        handle.write(bytes.fromhex(text))


def run(cmd, timeout=None):
    if timeout is None:
        timeout = float(os.environ.get("WEFT_CONFORMANCE_TIMEOUT", "60"))
    try:
        completed = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except FileNotFoundError as exc:
        raise AdapterFailure("implementation command not found: %s" % (exc,))
    except subprocess.TimeoutExpired:
        raise AdapterFailure("implementation timed out after %.1fs: %s" % (timeout, cmd[0]))
    return (
        completed.returncode,
        completed.stdout.decode("utf-8", "replace"),
        completed.stderr.decode("utf-8", "replace"),
    )


def class_from(text, capability=False):
    """Extract a normative class (or capability rejection) from CLI stderr."""
    names = CAPABILITY_REJECTIONS if capability else ERROR_CLASSES
    for line in text.splitlines():
        head = line.split(":", 1)[0].strip()
        if head in names:
            return head
    pattern = re.compile(r"\b(" + "|".join(names) + r")\b")
    match = pattern.search(text)
    if match:
        return match.group(1)
    return None


def limits_flags(vector):
    flags = []
    for key in sorted(vector.get("limits") or {}):
        flags.extend(["--" + key.replace("_", "-"), str(vector["limits"][key])])
    adversarial_flags = vector.get("flags")
    if isinstance(adversarial_flags, dict):
        # Adversarial vectors spell the CLI flag explicitly. Codec vectors use
        # an integer header `flags` field and must not be interpreted here.
        for key in sorted(adversarial_flags):
            flags.extend([str(key), str(adversarial_flags[key])])
    return flags


def frame_root(frame_bytes):
    if len(frame_bytes) < 72:
        return None
    return frame_bytes[40:72].hex()


def write_dependencies(vector, directory):
    entries = vector.get("dependencies")
    if entries is None:
        entries = vector.get("deps")
    if not entries:
        return False
    os.makedirs(directory, exist_ok=True)
    for entry in entries:
        hash_hex = entry["hash"]
        text = entry.get("bytes_hex", entry.get("hex"))
        write_hex(os.path.join(directory, hash_hex), text)
    return True


def write_frame_source(vector, directory):
    entries = vector.get("frame_source")
    if not entries:
        return False
    os.makedirs(directory, exist_ok=True)
    for entry in entries:
        write_hex(os.path.join(directory, entry["hash"]), entry["hex"])
    return True


def decode_frame(
    binary,
    vector,
    work,
    frame_hex,
    extra=None,
    dependencies=True,
    frame_source=True,
):
    frame_path = os.path.join(work, "frame.wft1")
    write_hex(frame_path, frame_hex)
    with open(frame_path, "rb") as handle:
        frame_bytes = handle.read()
    output_path = os.path.join(work, "decoded.bin")
    cmd = [binary, "decode", "--input", frame_path, "--output", output_path]
    deps_dir = os.path.join(work, "deps")
    frames_dir = os.path.join(work, "frames")
    if dependencies and write_dependencies(vector, deps_dir):
        cmd.extend(["--deps", deps_dir])
    if frame_source and write_frame_source(vector, frames_dir):
        cmd.extend(["--frame-source", frames_dir])
    if extra:
        cmd.extend(extra)
    cmd.extend(limits_flags(vector))
    code, _out, stderr = run(cmd)
    if code != 0:
        cls = class_from(stderr)
        if cls is None:
            raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
        return err(cls)
    with open(output_path, "rb") as handle:
        decoded = handle.read()
    return ok(frame_root(frame_bytes), sha256(decoded))


def decode_delta(binary, vector, work):
    delta_hex = vector.get("delta_frame_hex")
    base_hex = vector.get("base_frame_hex") or ""
    if not delta_hex:
        raise Skip("delta vector has no delta frame")
    use_source = vector.get("use_frame_source")
    if not use_source and not base_hex:
        # A missing-base vector supplies no base at all; the decoder resolves
        # the dependency from an empty resolver and must fail closed.
        return decode_frame(
            binary,
            vector,
            work,
            delta_hex,
            extra=None,
            dependencies=False,
            frame_source=False,
        )
    base_path = os.path.join(work, "base.wft1")
    extra = ["--base", base_path]
    if use_source:
        source_dir = os.path.join(work, "frames")
        os.makedirs(source_dir, exist_ok=True)
        if base_hex:
            write_hex(os.path.join(source_dir, vector["base"]), base_hex)
        if not base_hex and vector.get("expect_error") == "DependencyCycle":
            # A cycle vector resolves the delta frame as its own dependency.
            delta_bytes = bytes.fromhex(delta_hex)
            root = frame_root(delta_bytes)
            if root:
                with open(os.path.join(source_dir, root), "wb") as handle:
                    handle.write(delta_bytes)
        extra = ["--resolve", "--frame-source", source_dir]
    if base_hex:
        write_hex(base_path, base_hex)
    return decode_frame(
        binary,
        vector,
        work,
        delta_hex,
        extra=extra,
        dependencies=False,
        frame_source=False,
    )


def sharded(binary, vector, work):
    frames = vector.get("frames")
    if not frames:
        raise Skip("sharded vector has no frames")
    shard_dir = os.path.join(work, "shards")
    os.makedirs(shard_dir, exist_ok=True)
    ordered = sorted(frames, key=lambda frame: frame.get("index", 0))
    for position, frame in enumerate(ordered):
        write_hex(os.path.join(shard_dir, "shard-%03d.wft1" % position), frame["frame_hex"])
    cmd = [binary, "tensor", "root", "--frames", "--shards-dir", shard_dir, "--json"]
    code, stdout, stderr = run(cmd)
    if code != 0:
        cls = class_from(stderr)
        if cls is None:
            raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
        return err(cls)
    try:
        record = json.loads(stdout)
    except json.JSONDecodeError as exc:
        raise AdapterFailure("tensor root did not print JSON: %s" % (exc,))
    return ok(record.get("layout_root"), None)


def decode_many_frames(binary, frames, work, index_key):
    decoded = []
    ordered = sorted(frames, key=lambda frame: frame[index_key])
    for position, frame in enumerate(ordered):
        frame_path = os.path.join(work, "part-%03d.wft1" % position)
        output_path = os.path.join(work, "part-%03d.bin" % position)
        write_hex(frame_path, frame["frame_hex"])
        code, _out, stderr = run(
            [binary, "decode", "--input", frame_path, "--output", output_path]
        )
        if code != 0:
            cls = class_from(stderr)
            if cls is None:
                raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
            return err(cls), None
        with open(output_path, "rb") as handle:
            decoded.append(handle.read())
    return None, sha256(b"".join(decoded))


def kv_cache(binary, vector, work):
    if vector.get("expect_error"):
        return decode_frame(binary, vector, work, vector["frame_hex"])
    pages = vector.get("pages")
    if not pages:
        raise Skip("kv-cache vector has no pages")
    failure, digest = decode_many_frames(binary, pages, work, "page_index")
    if failure is not None:
        return failure
    return ok(None, digest)


def activation_stream(binary, vector, work):
    if vector.get("expect_error"):
        return decode_frame(binary, vector, work, vector["frame_hex"])
    chunks = vector.get("chunks")
    if not chunks:
        raise Skip("activation-stream vector has no chunks")
    failure, digest = decode_many_frames(binary, chunks, work, "chunk_index")
    if failure is not None:
        return failure
    return ok(None, digest)


def selector(binary, vector, work):
    source_path = os.path.join(work, "selector.wql")
    with open(source_path, "w", encoding="utf-8") as handle:
        handle.write(vector["source"])
    code, stdout, stderr = run(
        [binary, "selector", "id", "--input", source_path, "--json"]
    )
    if code != 0:
        cls = class_from(stderr)
        if cls is None:
            raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
        return err(cls)
    record = json.loads(stdout)
    canonical = bytes.fromhex(record["canonical_hex"])
    return ok(record.get("selector_id"), sha256(canonical))


def context_root(binary, vector, work):
    source_path = os.path.join(work, "selector.wql")
    with open(source_path, "w", encoding="utf-8") as handle:
        handle.write(vector["selector_source"])
    cmd = [
        binary,
        "context",
        "id",
        "--selector",
        source_path,
        "--namespace",
        vector["namespace"],
        "--snapshot",
        vector["snapshot"],
        "--json",
    ]
    if vector.get("causal"):
        cmd.extend(["--causal", ",".join(vector["causal"])])
    if vector.get("resolver"):
        cmd.extend(["--resolver", vector["resolver"]])
    if vector.get("capability_hash"):
        cmd.extend(["--capability-hash", vector["capability_hash"]])
    budget = vector.get("budget") or {}
    if budget:
        spec = ",".join(
            "%s=%s" % (key, budget[key])
            for key in ("max_results", "max_bytes", "max_depth", "max_work")
            if budget.get(key) is not None
        )
        if spec:
            cmd.extend(["--budget", spec])
    code, stdout, stderr = run(cmd)
    if code != 0:
        cls = class_from(stderr)
        if cls is None:
            raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
        return err(cls)
    record = json.loads(stdout)
    canonical = bytes.fromhex(record["canonical_hex"])
    return ok(record.get("context_root_id"), sha256(canonical))


def causal_closure(binary, vector, work):
    parents = vector.get("parents_input") or vector.get("parents") or []
    code, stdout, stderr = run(
        [binary, "causal", "id", "--parents", ",".join(parents), "--json"]
    )
    if code != 0:
        cls = class_from(stderr)
        if cls is None:
            raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
        return err(cls)
    record = json.loads(stdout)
    canonical = bytes.fromhex(record["canonical_hex"])
    return ok(record.get("closure_hash"), sha256(canonical))


def tensor_logical(binary, vector, work):
    values_path = os.path.join(work, "values.bin")
    write_hex(values_path, vector["values_hex"])
    shape = ",".join(str(dimension) for dimension in vector["shape"])
    code, stdout, stderr = run(
        [
            binary,
            "tensor",
            "root",
            "--input",
            values_path,
            "--dtype",
            vector["dtype"],
            "--shape",
            shape,
            "--layout",
            "row-major",
            "--json",
        ]
    )
    if code != 0:
        cls = class_from(stderr)
        if cls is None:
            raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
        return err(cls)
    record = json.loads(stdout)
    return ok(record.get("logical_root"), sha256(bytes.fromhex(vector["values_hex"])))


def tensor_shards(binary, vector, work):
    shard_dir = os.path.join(work, "shards")
    os.makedirs(shard_dir, exist_ok=True)
    for index, container in enumerate(vector["shard_container_hex"]):
        write_hex(os.path.join(shard_dir, "shard-%d.bin" % index), container)
    shape = ",".join(str(dimension) for dimension in vector["shape"])
    code, stdout, stderr = run(
        [
            binary,
            "tensor",
            "root",
            "--shards-dir",
            shard_dir,
            "--dtype",
            vector["dtype"],
            "--shape",
            shape,
            "--axis",
            str(vector["axis"]),
            "--shards",
            str(vector["shards"]),
            "--json",
        ]
    )
    if code != 0:
        cls = class_from(stderr)
        if cls is None:
            raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
        return err(cls)
    record = json.loads(stdout)
    return ok(record.get("logical_root"), sha256(bytes.fromhex(vector["values_hex"])))


def capability(binary, vector, work):
    if vector.get("expect_error"):
        blob_path = os.path.join(work, "capability.blob")
        write_hex(blob_path, vector["blob_hex"])
        cmd = [
            binary,
            "capability",
            "verify",
            "--blob",
            blob_path,
            "--now-ns",
            str(vector.get("now_ns", 0)),
        ]
        if vector.get("grantee"):
            cmd.extend(["--grantee", vector["grantee"]])
        if vector.get("revocations"):
            path = os.path.join(work, "revocations.txt")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write("\n".join(vector["revocations"]) + "\n")
            cmd.extend(["--revocations", path])
        code, _stdout, stderr = run(cmd)
        if code == 0:
            return err("Accepted")
        cls = class_from(stderr, capability=True)
        if cls is None:
            raise AdapterFailure("no capability rejection in stderr: %s" % stderr.strip())
        return err(cls)

    envelope_path = os.path.join(work, "envelope.json")
    with open(envelope_path, "w", encoding="utf-8") as handle:
        json.dump(vector["envelope_json"], handle, sort_keys=True)
    code, stdout, stderr = run(
        [binary, "capability", "id", "--envelope", envelope_path, "--json"]
    )
    if code != 0:
        cls = class_from(stderr, capability=True)
        if cls is None:
            raise AdapterFailure("no capability rejection in stderr: %s" % stderr.strip())
        return err(cls)
    record = json.loads(stdout)
    canonical = bytes.fromhex(record["canonical_hex"])
    return ok(record.get("envelope_id"), sha256(canonical))


def frame_vector(binary, vector, work):
    subkind = vector.get("_subkind")
    if subkind == "adversarial":
        verb = vector.get("verb")
        if verb == "decode":
            return decode_frame(binary, vector, work, vector["input_hex"])
        if verb == "selector-id":
            source_path = os.path.join(work, "selector.wql")
            write_hex(source_path, vector["input_hex"])
            cmd = [binary, "selector", "id", "--input", source_path, "--json"]
            cmd.extend(limits_flags(vector))
            code, _stdout, stderr = run(cmd)
            if code == 0:
                return err("Accepted")
            cls = class_from(stderr)
            if cls is None:
                raise AdapterFailure("no error class in stderr: %s" % stderr.strip())
            return err(cls)
        raise Skip("adversarial verb %r has no adapter" % verb)
    if subkind in ("weight_delta", "semantic_delta", "sparse_delta"):
        return decode_delta(binary, vector, work)
    if subkind == "sharded":
        return sharded(binary, vector, work)
    if subkind == "kv_cache":
        return kv_cache(binary, vector, work)
    if subkind == "activation_stream":
        return activation_stream(binary, vector, work)
    return decode_frame(binary, vector, work, vector["frame_hex"])


def dispatch(binary, vector, work):
    kind = vector.get("_kind")
    if kind == "frame":
        return frame_vector(binary, vector, work)
    if kind == "selector":
        return selector(binary, vector, work)
    if kind == "context":
        if vector.get("expect_error"):
            raise Skip("record-level reject has no CLI verb in this implementation")
        subkind = vector.get("_subkind")
        if subkind == "causal_closure":
            return causal_closure(binary, vector, work)
        if subkind == "context_root":
            return context_root(binary, vector, work)
        raise Skip("context subkind %r has no adapter" % subkind)
    if kind == "tensor":
        if vector.get("expect_error"):
            raise Skip("record-level reject has no CLI verb in this implementation")
        subkind = vector.get("_subkind")
        if subkind == "tensor_logical":
            return tensor_logical(binary, vector, work)
        if subkind == "tensor_shards":
            return tensor_shards(binary, vector, work)
        raise Skip("tensor subkind %r has no adapter" % subkind)
    if kind == "capability":
        return capability(binary, vector, work)
    raise Skip("kind %r has no adapter" % kind)


def annotate(path, vector):
    """Attach the manifest-independent kind/subkind inferred from the path."""
    parts = path.replace("\\", "/").split("/")
    sub = ""
    for index, part in enumerate(parts):
        if part != "conformance":
            continue
        rest = parts[index + 1 :]
        if rest and rest[0] == "v1":
            rest = rest[1:]
        elif len(rest) >= 2 and rest[1] == "v1":
            rest = [rest[0]] + rest[2:]
        if rest:
            sub = rest[0]
        break

    mapping = {
        "codec": ("frame", None),
        "adversarial": ("frame", "adversarial"),
        "sharded": ("frame", "sharded"),
        "weight-delta": ("frame", "weight_delta"),
        "kv-cache": ("frame", "kv_cache"),
        "activation-stream": ("frame", "activation_stream"),
        "semantic-delta": ("frame", "semantic_delta"),
        "sparse-delta": ("frame", "sparse_delta"),
        "tensor-tiled": ("frame", None),
        "float-tiled": ("frame", None),
        "structured": ("wcb", None),
        "selectors": ("selector", None),
        "context": ("context", "context_root"),
        "causal": ("context", "causal_closure"),
        "tensor": ("tensor", None),
        "semantic-root": ("tensor", "semantic_root"),
        "capability": ("capability", None),
        "checkpoint": ("checkpoint", None),
        "store": ("store", None),
        "fuzz": ("fuzz", None),
    }
    kind, subkind = mapping.get(sub, (None, None))
    if sub == "context" and os.path.basename(path) == "causal_versions.json":
        subkind = "causal_versions"
    if sub == "tensor":
        tensor_kind = vector.get("kind")
        subkind = {
            "logical": "tensor_logical",
            "layout": "tensor_layout",
            "shards": "tensor_shards",
        }.get(tensor_kind)
    vector["_kind"] = kind
    vector["_subkind"] = subkind
    return vector


def main(impl, binary_env):
    del impl  # kept for adapter-specific logging only
    if len(sys.argv) != 2:
        print("usage: adapter.py VECTOR.json", file=sys.stderr)
        return 2
    path = sys.argv[1]
    try:
        with open(path, "r", encoding="utf-8") as handle:
            vector = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        raise AdapterFailure("cannot read vector %s: %s" % (path, exc))
    annotate(path, vector)

    binary = os.environ.get(binary_env, "")
    if not binary:
        raise AdapterFailure("%s is not set" % binary_env)
    if not os.path.isfile(binary):
        raise AdapterFailure("%s does not exist: %s" % (binary_env, binary))

    work = tempfile.mkdtemp(prefix="weft-conformance-")
    try:
        return dispatch(binary, vector, work)
    finally:
        if os.environ.get("WEFT_CONFORMANCE_KEEP") != "1":
            shutil.rmtree(work, ignore_errors=True)
