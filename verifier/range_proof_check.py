#!/usr/bin/env python3
"""Independent verifier for `weft.range-proof.v1` (task T073).

This program shares no code with WeftFS. It uses only the Python standard
library plus three files in this directory:

    blake3.py    clean-room BLAKE3-256
    hashing.py   H(domain, data) exactly as WEFTCODEC-FORMAT §1 defines it
    wcb.py       canonical WCB decode / re-encode
    ed25519.py   Ed25519 signature verification

What it checks, in order:

 1. The three records in the bundle are canonical WCB (decode + re-encode is
    byte-identical) and carry exactly the documented key sets.
 2. The checkpoint manifest bytes hash to the anchor's `checkpoint_root`
    (`H("weft/checkpoint/manifest/v1", canonical bytes)`).
 3. The anchor is the record the proof names, and its signature verifies when
    present. When `--trust-key` is given the signer must be one of them.
 4. The checkpoint manifest's tensor entry and shard entry equal the anchor's
    recorded identity fields, so the anchor cannot be replayed against another
    checkpoint, tensor or shard.
 5. The anchor's Merkle root is re-derived **from the proof's own leaves and
    authentication paths**: each leaf is hashed with the frozen leaf domain,
    folded through the frozen node domain with the frozen promotion rule, and
    compared with `chunk_table_root`. The verifier derives every level's width
    from `chunk_count`, so a skipped, extra or wrongly sided node is rejected
    rather than reinterpreted.
 6. The leaf set is exactly the set of chunks the declared region intersects,
    derived from `offset`, `length`, `chunk_size`, `size`, `chunk_count`.
 7. Chunk bytes (from `--store` or from `bundle/chunks`) hash to `leaf.hash`
    with the chunk domain, and the requested region's bytes hash to
    `region_root` with the range domain.
 8. With `--store`, the shard manifest is re-read from the store by its
    content address, and the chunk table is re-derived from it: size,
    chunk_size, chunk count, the Merkle root, and the manifest's whole-file
    `content_hash` must all agree with the anchor and the checkpoint entry.
    This is the dependency check; without `--store` that check is reported as
    `manifest_rechecked=false` and nothing is claimed about it.

Exit status: 0 verified, 1 rejected/mismatch, 2 usage error.

`--json` writes one JSON object on stdout. Without it, `key=value` lines.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time

import ed25519
from blake3 import blake3_256
from hashing import hash_domain
import wcb

CHECKPOINT_MANIFEST_DOMAIN = "weft/checkpoint/manifest/v1"
ANCHOR_DOMAIN = "weft/range-anchor/v1"
PROOF_DOMAIN = "weft/range-proof/v1"
CHUNK_DOMAIN = "weft/chunk/v1"
MERKLE_LEAF_DOMAIN = "weft/merkle/leaf/v1"
MERKLE_NODE_DOMAIN = "weft/merkle/node/v1"
RANGE_DOMAIN = "weft/range/v1"
MANIFEST_DOMAIN = "weft/manifest/v1"

ANCHOR_KEYS = {
    "schema",
    "checkpoint_name",
    "checkpoint_root",
    "checkpoint_manifest_id",
    "tensor",
    "dtype",
    "shape",
    "logical_root",
    "content_root",
    "element_bytes",
    "chunk_size",
    "bytes",
    "chunk_count",
    "chunk_table_root",
    "shards",
    "load_attestation",
    "issued_at",
    "signature",
}
SHARD_KEYS = {
    "index",
    "object_name",
    "object_id",
    "content_root",
    "logical_root",
    "byte_len",
    "manifest",
    "chunk_table_root",
    "chunk_count",
}
PROOF_KEYS = {
    "schema",
    "anchor",
    "manifest",
    "content_root",
    "tensor",
    "shard_index",
    "chunk_size",
    "chunk_count",
    "size",
    "offset",
    "length",
    "region_root",
    "leaves",
    "paths",
}
LEAF_KEYS = {"index", "offset", "len", "hash"}
NODE_KEYS = {"side", "hash"}
MANIFEST_KEYS = {
    "schema",
    "name",
    "container",
    "complete",
    "element_bytes",
    "source_header",
    "source_file_bytes",
    "source_sha256",
    "tensors",
}
TENSOR_KEYS = {
    "name",
    "dtype",
    "shape",
    "logical_root",
    "content_root",
    "element_bytes",
    "source_offset",
    "shards",
}
CP_SHARD_KEYS = {
    "axis",
    "shards",
    "index",
    "object_name",
    "object_id",
    "content_root",
    "logical_root",
    "byte_len",
}


class Rejected(Exception):
    def __init__(self, kind: str, message: str):
        super().__init__(message)
        self.kind = kind
        self.message = message


def u64(value, where: str) -> int:
    if not isinstance(value, wcb.U64):
        raise Rejected("Malformed", "%s must be u64" % where)
    return value.value


def digest(value, where: str) -> bytes:
    if not isinstance(value, wcb.Bytes) or len(value.value) != 32:
        raise Rejected("Malformed", "%s must be 32 bytes" % where)
    return value.value


def text(value, where: str) -> str:
    if not isinstance(value, str):
        raise Rejected("Malformed", "%s must be text" % where)
    return value


def expect_map(value, keys, where: str) -> dict:
    if not isinstance(value, dict):
        raise Rejected("Malformed", "%s must be a map" % where)
    if set(value) != keys:
        raise Rejected(
            "Malformed",
            "%s key set is %s, expected %s"
            % (where, sorted(value), sorted(keys)),
        )
    return value


def expect_array(value, where: str) -> list:
    if not isinstance(value, list):
        raise Rejected("Malformed", "%s must be an array" % where)
    return value


def canonical(path: str) -> bytes:
    try:
        data = open(path, "rb").read()
    except OSError as err:
        raise Rejected("Io", "%s: %s" % (path, err))
    try:
        wcb.decode_and_reencode(data)
    except wcb.WcbError as err:
        raise Rejected("NonCanonical", "%s: %s" % (path, err))
    return data


def merkle_leaf(length: int, chunk_hash: bytes) -> bytes:
    return hash_domain(
        MERKLE_LEAF_DOMAIN, length.to_bytes(8, "little") + chunk_hash
    )


def merkle_node(left: bytes, right: bytes) -> bytes:
    return hash_domain(MERKLE_NODE_DOMAIN, left + right)


def verify_path(chunk_count, leaf_index, leaf_hash, path, expected_root):
    """Mirror of the frozen promotion rule, derived from the table width."""
    if chunk_count <= 0:
        raise Rejected("Malformed", "chunk_count must be positive")
    if leaf_index >= chunk_count:
        raise Rejected("Corrupt", "leaf index outside the chunk table")
    hash_ = leaf_hash
    index = leaf_index
    width = chunk_count
    consumed = 0
    while width > 1:
        has_sibling = index % 2 == 1 or index + 1 < width
        if has_sibling:
            if consumed >= len(path):
                raise Rejected(
                    "Corrupt", "authentication path is truncated at node %d" % consumed
                )
            node = path[consumed]
            expected_side = "left" if index % 2 == 1 else "right"
            if node["side"] != expected_side:
                raise Rejected(
                    "Corrupt",
                    "path node %d is on the %s side but the geometry requires %s"
                    % (consumed, node["side"], expected_side),
                )
            if node["side"] == "left":
                hash_ = merkle_node(node["hash"], hash_)
            else:
                hash_ = merkle_node(hash_, node["hash"])
            consumed += 1
        index //= 2
        width = (width + 1) // 2
    if consumed != len(path):
        raise Rejected(
            "Corrupt",
            "authentication path has %d nodes the tree geometry does not admit"
            % (len(path) - consumed),
        )
    if hash_ != expected_root:
        raise Rejected(
            "Corrupt",
            "authentication path does not reconstruct the anchored Merkle root",
        )


def expected_span(offset, length, size, chunk_size, chunk_count):
    if chunk_size <= 0 or chunk_count <= 0 or size <= 0:
        raise Rejected("Malformed", "geometry fields must be positive")
    if length == 0:
        raise Rejected("Malformed", "region length must be positive")
    if offset >= size:
        raise Rejected("InvalidArgument", "region offset is at or past the size")
    if offset + length > size:
        raise Rejected("InvalidArgument", "region extends past the size")
    if (size + chunk_size - 1) // chunk_size != chunk_count:
        raise Rejected(
            "Corrupt", "chunk_count disagrees with size and chunk_size"
        )
    first = offset // chunk_size
    last = (offset + length - 1) // chunk_size
    span = []
    for index in range(first, last + 1):
        start = index * chunk_size
        end_len = size - start if index == chunk_count - 1 else chunk_size
        span.append((index, start, end_len))
    return span


def load_anchor(raw: bytes) -> dict:
    value = wcb.decode_and_reencode(raw)
    anchor = expect_map(value, ANCHOR_KEYS, "anchor")
    if text(anchor["schema"], "anchor.schema") != "weft.range-anchor.v1":
        raise Rejected("UnsupportedVersion", "anchor schema is not weft.range-anchor.v1")
    shards = []
    for item in expect_array(anchor["shards"], "anchor.shards"):
        shard = expect_map(item, SHARD_KEYS, "anchor.shard")
        shards.append(
            {
                "index": u64(shard["index"], "shard.index"),
                "object_name": text(shard["object_name"], "shard.object_name"),
                "object_id": digest(shard["object_id"], "shard.object_id"),
                "content_root": digest(shard["content_root"], "shard.content_root"),
                "logical_root": digest(shard["logical_root"], "shard.logical_root"),
                "byte_len": u64(shard["byte_len"], "shard.byte_len"),
                "manifest": digest(shard["manifest"], "shard.manifest"),
                "chunk_table_root": digest(
                    shard["chunk_table_root"], "shard.chunk_table_root"
                ),
                "chunk_count": u64(shard["chunk_count"], "shard.chunk_count"),
            }
        )
    shards.sort(key=lambda shard: shard["index"])
    for a, b in zip(shards, shards[1:]):
        if a["index"] >= b["index"]:
            raise Rejected("NonCanonical", "anchor shard indices are not ascending")
    shape = []
    for dim in expect_array(anchor["shape"], "anchor.shape"):
        shape.append(u64(dim, "anchor.shape[]"))
    signature = anchor["signature"]
    return {
        "raw": raw,
        "value": anchor,
        "checkpoint_name": text(anchor["checkpoint_name"], "anchor.checkpoint_name"),
        "checkpoint_root": digest(anchor["checkpoint_root"], "anchor.checkpoint_root"),
        "checkpoint_manifest_id": digest(
            anchor["checkpoint_manifest_id"], "anchor.checkpoint_manifest_id"
        ),
        "tensor": text(anchor["tensor"], "anchor.tensor"),
        "dtype": text(anchor["dtype"], "anchor.dtype"),
        "shape": shape,
        "logical_root": digest(anchor["logical_root"], "anchor.logical_root"),
        "content_root": digest(anchor["content_root"], "anchor.content_root"),
        "element_bytes": u64(anchor["element_bytes"], "anchor.element_bytes"),
        "chunk_size": u64(anchor["chunk_size"], "anchor.chunk_size"),
        "bytes": u64(anchor["bytes"], "anchor.bytes"),
        "chunk_count": u64(anchor["chunk_count"], "anchor.chunk_count"),
        "chunk_table_root": digest(
            anchor["chunk_table_root"], "anchor.chunk_table_root"
        ),
        "shards": shards,
        "load_attestation": (
            None
            if anchor["load_attestation"] is None
            else digest(anchor["load_attestation"], "anchor.load_attestation")
        ),
        "signature_present": signature is not None,
        "signing_bytes": wcb.encode(
            {key: value for key, value in anchor.items() if key != "signature"}
        ),
        "signature": signature,
    }


def load_proof(raw: bytes) -> dict:
    value = wcb.decode_and_reencode(raw)
    proof = expect_map(value, PROOF_KEYS, "proof")
    if text(proof["schema"], "proof.schema") != "weft.range-proof.v1":
        raise Rejected("UnsupportedVersion", "proof schema is not weft.range-proof.v1")
    leaves = []
    for item in expect_array(proof["leaves"], "proof.leaves"):
        leaf = expect_map(item, LEAF_KEYS, "proof.leaf")
        length = u64(leaf["len"], "leaf.len")
        if length == 0:
            raise Rejected("Malformed", "leaf has zero length")
        leaves.append(
            {
                "index": u64(leaf["index"], "leaf.index"),
                "offset": u64(leaf["offset"], "leaf.offset"),
                "len": length,
                "hash": digest(leaf["hash"], "leaf.hash"),
            }
        )
    paths = []
    for item in expect_array(proof["paths"], "proof.paths"):
        nodes = []
        for node in expect_array(item, "proof.paths[]"):
            entry = expect_map(node, NODE_KEYS, "proof.path.node")
            side = text(entry["side"], "node.side")
            if side not in ("left", "right"):
                raise Rejected("Malformed", "unknown path side %r" % side)
            nodes.append({"side": side, "hash": digest(entry["hash"], "node.hash")})
        paths.append(nodes)
    if len(paths) != len(leaves):
        raise Rejected("Malformed", "proof must carry one path per leaf")
    return {
        "anchor": digest(proof["anchor"], "proof.anchor"),
        "manifest": digest(proof["manifest"], "proof.manifest"),
        "content_root": digest(proof["content_root"], "proof.content_root"),
        "tensor": text(proof["tensor"], "proof.tensor"),
        "shard_index": u64(proof["shard_index"], "proof.shard_index"),
        "chunk_size": u64(proof["chunk_size"], "proof.chunk_size"),
        "chunk_count": u64(proof["chunk_count"], "proof.chunk_count"),
        "size": u64(proof["size"], "proof.size"),
        "offset": u64(proof["offset"], "proof.offset"),
        "length": u64(proof["length"], "proof.length"),
        "region_root": digest(proof["region_root"], "proof.region_root"),
        "leaves": leaves,
        "paths": paths,
    }


def load_checkpoint(raw: bytes) -> dict:
    value = wcb.decode_and_reencode(raw)
    manifest = expect_map(value, MANIFEST_KEYS, "checkpoint")
    if text(manifest["schema"], "checkpoint.schema") != "weft.checkpoint.v1":
        raise Rejected(
            "UnsupportedVersion", "checkpoint schema is not weft.checkpoint.v1"
        )
    tensors = []
    for item in expect_array(manifest["tensors"], "checkpoint.tensors"):
        tensor = expect_map(item, TENSOR_KEYS, "checkpoint.tensor")
        shards = []
        for raw_shard in expect_array(tensor["shards"], "tensor.shards"):
            shard = expect_map(raw_shard, CP_SHARD_KEYS, "tensor.shard")
            shards.append(
                {
                    "axis": u64(shard["axis"], "shard.axis"),
                    "shards": u64(shard["shards"], "shard.shards"),
                    "index": u64(shard["index"], "shard.index"),
                    "object_name": text(shard["object_name"], "shard.object_name"),
                    "object_id": digest(shard["object_id"], "shard.object_id"),
                    "content_root": digest(shard["content_root"], "shard.content_root"),
                    "logical_root": digest(shard["logical_root"], "shard.logical_root"),
                    "byte_len": u64(shard["byte_len"], "shard.byte_len"),
                }
            )
        shape = [u64(dim, "tensor.shape[]") for dim in expect_array(tensor["shape"], "tensor.shape")]
        tensors.append(
            {
                "name": text(tensor["name"], "tensor.name"),
                "dtype": text(tensor["dtype"], "tensor.dtype"),
                "shape": shape,
                "logical_root": digest(tensor["logical_root"], "tensor.logical_root"),
                "content_root": digest(tensor["content_root"], "tensor.content_root"),
                "element_bytes": u64(tensor["element_bytes"], "tensor.element_bytes"),
                "shards": shards,
            }
        )
    return {"raw": raw, "tensors": tensors}


def store_path(store: str, kind: str, digest_bytes: bytes) -> str:
    hexed = digest_bytes.hex()
    return os.path.join(store, kind, hexed[:2], hexed)


def read_store_blob(store: str, kind: str, digest_bytes: bytes, domain: str) -> bytes:
    path = store_path(store, kind, digest_bytes)
    try:
        data = open(path, "rb").read()
    except OSError as err:
        raise Rejected("NotFound", "%s: %s" % (path, err))
    if hash_domain(domain, data) != digest_bytes:
        raise Rejected("Corrupt", "%s does not hash to its content address" % path)
    return data


def verify_manifest_dependency(store, anchor, shard):
    """Re-read the shard manifest and re-derive its chunk table."""
    raw = read_store_blob(store, "manifests", shard["manifest"], MANIFEST_DOMAIN)
    try:
        value = wcb.decode_and_reencode(raw)
    except wcb.WcbError as err:
        raise Rejected("NonCanonical", "shard manifest: %s" % err)
    if not isinstance(value, dict):
        raise Rejected("Malformed", "shard manifest is not a WCB map")
    representation = value.get("representation")
    if representation is not None:
        if not isinstance(representation, dict):
            raise Rejected("Malformed", "manifest representation is not a map")
        kind = representation.get("kind")
        if kind != "raw":
            raise Rejected(
                "UnsupportedRepresentation",
                "shard manifest carries a %r representation, not a chunk table" % (kind,),
            )
    for key in ("size", "chunk_size", "chunks", "content_hash"):
        if key not in value:
            raise Rejected("Malformed", "shard manifest is missing %r" % key)
    size = u64(value["size"], "manifest.size")
    chunk_size = u64(value["chunk_size"], "manifest.chunk_size")
    chunks = []
    for item in expect_array(value["chunks"], "manifest.chunks"):
        if not isinstance(item, dict) or set(item) != {"hash", "len"}:
            raise Rejected("Malformed", "manifest chunk entry has the wrong key set")
        length = u64(item["len"], "chunk.len")
        if length == 0:
            raise Rejected("Corrupt", "manifest chunk has zero length")
        chunks.append((length, digest(item["hash"], "chunk.hash")))
    if size != shard["byte_len"] or chunk_size != anchor["chunk_size"]:
        raise Rejected(
            "Corrupt",
            "shard manifest geometry (%d bytes, %d per chunk) disagrees with the anchor "
            "(%d bytes, %d per chunk)"
            % (size, chunk_size, shard["byte_len"], anchor["chunk_size"]),
        )
    if len(chunks) != anchor["chunk_count"]:
        raise Rejected(
            "Corrupt",
            "shard manifest lists %d chunks, the anchor declares %d"
            % (len(chunks), anchor["chunk_count"]),
        )
    root = merkle_root(chunks)
    if root != anchor["chunk_table_root"]:
        raise Rejected(
            "Corrupt", "shard manifest does not fold to the anchored Merkle root"
        )
    content_hash = digest(value["content_hash"], "manifest.content_hash")
    if content_hash != shard["content_root"]:
        raise Rejected(
            "Corrupt",
            "shard manifest whole-file content hash disagrees with the checkpoint entry",
        )
    return {"chunks": chunks, "size": size, "chunk_size": chunk_size}


def merkle_root(chunks):
    level = [merkle_leaf(length, chunk_hash) for length, chunk_hash in chunks]
    if not level:
        raise Rejected("Corrupt", "empty chunk table")
    while len(level) > 1:
        nxt = []
        index = 0
        while index + 1 < len(level):
            nxt.append(merkle_node(level[index], level[index + 1]))
            index += 2
        if index < len(level):
            nxt.append(level[index])
        level = nxt
    return level[0]


def run(args):
    proof_raw = canonical(os.path.join(args.bundle, "proof.wcb"))
    anchor_raw = canonical(os.path.join(args.bundle, "anchor.wcb"))
    checkpoint_raw = canonical(os.path.join(args.bundle, "checkpoint.wcb"))
    proof = load_proof(proof_raw)
    anchor = load_anchor(anchor_raw)
    checkpoint = load_checkpoint(checkpoint_raw)

    anchor_id = hash_domain(ANCHOR_DOMAIN, anchor_raw)
    if anchor_id != proof["anchor"]:
        raise Rejected(
            "Corrupt", "the proof names a different anchor than the bundle carries"
        )
    proof_id = hash_domain(PROOF_DOMAIN, proof_raw)

    checkpoint_root = hash_domain(CHECKPOINT_MANIFEST_DOMAIN, checkpoint_raw)
    if checkpoint_root != anchor["checkpoint_root"]:
        raise Rejected(
            "Corrupt",
            "checkpoint manifest hashes to %s, the anchor declares %s"
            % (checkpoint_root.hex(), anchor["checkpoint_root"].hex()),
        )

    signature_verified = False
    if anchor["signature_present"]:
        signature = anchor["signature"]
        if not isinstance(signature, dict) or set(signature) != {
            "algorithm",
            "public_key",
            "signature",
        }:
            raise Rejected("Malformed", "anchor signature has the wrong key set")
        if text(signature["algorithm"], "signature.algorithm") != "ed25519":
            raise Rejected("Malformed", "anchor signature algorithm is not ed25519")
        public_key = signature["public_key"]
        raw_signature = signature["signature"]
        if not isinstance(public_key, wcb.Bytes) or len(public_key.value) != 32:
            raise Rejected("Malformed", "anchor public key must be 32 bytes")
        if not isinstance(raw_signature, wcb.Bytes) or len(raw_signature.value) != 64:
            raise Rejected("Malformed", "anchor signature must be 64 bytes")
        if not ed25519.verify(public_key.value, anchor["signing_bytes"], raw_signature.value):
            raise Rejected("Corrupt", "anchor signature does not verify")
        signature_verified = True
        if args.trust_key:
            trusted = {key.lower() for key in args.trust_key}
            if public_key.value.hex() not in trusted:
                raise Rejected(
                    "Unauthorized", "anchor signer is not in the supplied trust set"
                )
    elif args.trust_key:
        raise Rejected(
            "Unauthorized", "an unsigned anchor is refused when --trust-key is given"
        )

    tensor = next(
        (entry for entry in checkpoint["tensors"] if entry["name"] == anchor["tensor"]),
        None,
    )
    if tensor is None:
        raise Rejected("Corrupt", "the checkpoint has no tensor named %r" % anchor["tensor"])
    for field in ("dtype", "shape", "logical_root", "content_root", "element_bytes"):
        if tensor[field] != anchor[field]:
            raise Rejected(
                "Corrupt",
                "anchor %s does not equal the checkpoint tensor entry" % field,
            )
    shard = next(
        (entry for entry in tensor["shards"] if entry["index"] == proof["shard_index"]),
        None,
    )
    if shard is None:
        raise Rejected("Corrupt", "the tensor has no shard %d" % proof["shard_index"])
    anchor_shard = next(
        (entry for entry in anchor["shards"] if entry["index"] == proof["shard_index"]),
        None,
    )
    if anchor_shard is None:
        raise Rejected("Corrupt", "the anchor does not cover the proof's shard")
    for field in ("object_id", "content_root", "logical_root", "byte_len"):
        if anchor_shard[field] != shard[field]:
            raise Rejected(
                "Corrupt",
                "anchor shard %s does not equal the checkpoint shard entry" % field,
            )

    if proof["manifest"] != anchor_shard["manifest"]:
        raise Rejected("Corrupt", "proof names a different shard manifest")
    if proof["content_root"] != anchor_shard["content_root"]:
        raise Rejected("Corrupt", "proof names a different shard content root")
    if proof["tensor"] != anchor["tensor"]:
        raise Rejected("Corrupt", "proof names a different tensor")
    if (
        proof["chunk_size"] != anchor["chunk_size"]
        or proof["chunk_count"] != anchor["chunk_count"]
        or proof["size"] != anchor["bytes"]
    ):
        raise Rejected("Corrupt", "proof geometry disagrees with the anchor")

    span = expected_span(
        proof["offset"],
        proof["length"],
        proof["size"],
        proof["chunk_size"],
        proof["chunk_count"],
    )
    if len(span) != len(proof["leaves"]):
        raise Rejected(
            "Corrupt",
            "proof carries %d leaves but the region requires %d"
            % (len(proof["leaves"]), len(span)),
        )
    for leaf, (index, offset, length) in zip(proof["leaves"], span):
        if leaf["index"] != index or leaf["offset"] != offset or leaf["len"] != length:
            raise Rejected(
                "Corrupt",
                "leaf %d does not cover chunk %d at %d+%d"
                % (leaf["index"], index, offset, length),
            )
    path_nodes = 0
    for position, (leaf, path) in enumerate(zip(proof["leaves"], proof["paths"])):
        try:
            verify_path(
                proof["chunk_count"],
                leaf["index"],
                merkle_leaf(leaf["len"], leaf["hash"]),
                path,
                anchor["chunk_table_root"],
            )
        except Rejected as err:
            raise Rejected(
                "Corrupt",
                "path for leaf %d (chunk %d) is invalid: %s"
                % (position, leaf["index"], err.message),
            )
        path_nodes += len(path)

    chunk_bytes = 0
    resolved = []
    if args.store:
        for leaf in proof["leaves"]:
            data = read_store_blob(args.store, "objects", leaf["hash"], CHUNK_DOMAIN)
            resolved.append(data)
    else:
        chunks_dir = os.path.join(args.bundle, "chunks")
        names = sorted(os.listdir(chunks_dir)) if os.path.isdir(chunks_dir) else []
        if len(names) != len(proof["leaves"]):
            raise Rejected(
                "Malformed",
                "bundle carries %d chunk files, the proof has %d leaves"
                % (len(names), len(proof["leaves"])),
            )
        for name in names:
            resolved.append(open(os.path.join(chunks_dir, name), "rb").read())

    buffer = bytearray()
    for leaf, data in zip(proof["leaves"], resolved):
        if len(data) != leaf["len"]:
            raise Rejected(
                "Corrupt",
                "chunk %d is %d bytes, the proof declares %d"
                % (leaf["index"], len(data), leaf["len"]),
            )
        if hash_domain(CHUNK_DOMAIN, data) != leaf["hash"]:
            raise Rejected(
                "Corrupt", "chunk %d bytes do not hash to the proof leaf" % leaf["index"]
            )
        chunk_bytes += len(data)
        buffer.extend(data)
    start = proof["offset"] - proof["leaves"][0]["offset"]
    end = start + proof["length"]
    region = bytes(buffer[start:end])
    if len(region) != proof["length"]:
        raise Rejected("Corrupt", "region assembly produced the wrong number of bytes")
    if hash_domain(RANGE_DOMAIN, region) != proof["region_root"]:
        raise Rejected(
            "Corrupt", "region digest does not match the supplied chunk bytes"
        )

    manifest_rechecked = False
    if args.store:
        verify_manifest_dependency(args.store, anchor, anchor_shard)
        manifest_rechecked = True

    return {
        "verified": True,
        "proof": proof_id.hex(),
        "anchor": anchor_id.hex(),
        "checkpoint_root": checkpoint_root.hex(),
        "chunk_table_root": anchor["chunk_table_root"].hex(),
        "tensor": anchor["tensor"],
        "shard_index": proof["shard_index"],
        "chunk_size": proof["chunk_size"],
        "chunk_count": proof["chunk_count"],
        "leaves_verified": len(proof["leaves"]),
        "path_nodes_verified": path_nodes,
        "chunk_bytes": chunk_bytes,
        "region_bytes": proof["length"],
        "region_root": proof["region_root"].hex(),
        "manifest_rechecked": manifest_rechecked,
        "anchor_signature_verified": signature_verified,
        "anchor_signature_present": anchor["signature_present"],
        "trust_required": bool(args.trust_key),
    }


def main(argv):
    parser = argparse.ArgumentParser(
        description="Verify a weft.range-proof.v1 bundle independently."
    )
    parser.add_argument("--bundle", required=True, help="directory with proof.wcb, anchor.wcb, checkpoint.wcb, chunks/")
    parser.add_argument("--store", help="store root; re-read chunk bytes and the shard manifest")
    parser.add_argument("--trust-key", action="append", default=[], help="accepted anchor signer (64 hex); repeatable")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    started = time.monotonic_ns()
    try:
        report = run(args)
    except Rejected as err:
        payload = {"verified": False, "class": err.kind, "message": err.message}
        if args.json:
            print(json.dumps(payload, indent=2, sort_keys=True))
        else:
            print("verified=false")
            print("class=%s" % err.kind)
            print("message=%s" % err.message)
        return 1
    report["verify_ns"] = time.monotonic_ns() - started
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        for key in (
            "verified",
            "tensor",
            "shard_index",
            "chunk_size",
            "chunk_count",
            "leaves_verified",
            "path_nodes_verified",
            "chunk_bytes",
            "region_bytes",
            "manifest_rechecked",
            "anchor_signature_verified",
            "verify_ns",
        ):
            value = report[key]
            if isinstance(value, bool):
                value = "true" if value else "false"
            print("%s=%s" % (key, value))
        print("anchor=%s" % report["anchor"])
        print("region_root=%s" % report["region_root"])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
