#!/usr/bin/env python3
"""Clean-room verifier for `weft.producer-attestation.v1` (T074).

Independent of Rust and Go: this uses the verifier's own WCB reader and its own
pure-Python Ed25519 implementation. It exists so a third party can check a
producer declaration without importing WeftFS.

What it proves: the bytes are canonical, the signer is in the supplied key set,
the signature verifies over the signed body, and the key's lifecycle state at
the signing epoch. What it does **not** prove: that the declared computation
ran, that the tool produced the bytes, or that the inputs were read. A
signature authenticates a declaration.

Usage:
    python3 verifier/producer_attest.py ATTESTATION.wcb --keyset KEYSET.wcb \
        [--mode current|historical] [--json]
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

import ed25519  # noqa: E402
import wcb  # noqa: E402
from hashing import hash_domain  # noqa: E402

ATTESTATION_SCHEMA = "weft.producer-attestation.v1"
KEY_SET_SCHEMA = "weft.producer-key-set.v1"
KEY_SET_DOMAIN = "weft/producer-keys/v1"


class Refusal(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def canonical(path):
    raw = Path(path).read_bytes()
    value = wcb.decode_and_reencode(raw)
    if wcb.encode(value) != raw:
        raise Refusal("NonCanonical", "record is not canonical WCB")
    return raw, value


def as_map(value, where):
    if not isinstance(value, dict):
        raise Refusal("Malformed", "%s is not a map" % where)
    return value


def exact_keys(row, keys, where):
    if set(row) != set(keys) or len(row) != len(keys):
        raise Refusal("Malformed", "%s has an unknown key set" % where)


def as_root(value, where):
    if not isinstance(value, wcb.Bytes) or len(value.value) != 32:
        raise Refusal("Malformed", "%s is not bytes32" % where)
    return value.value


def opt_u64(value, where):
    if value is None:
        return None
    if not isinstance(value, wcb.U64):
        raise Refusal("Malformed", "%s is not u64|null" % where)
    return value.value


def as_text(value, where):
    if not isinstance(value, str):
        raise Refusal("Malformed", "%s is not text" % where)
    return value


def load_key_set(path):
    raw, value = canonical(path)
    row = as_map(value, "key set")
    exact_keys(row, ["schema", "epoch", "rotated_from", "keys"], "key set")
    if as_text(row["schema"], "key set schema") != KEY_SET_SCHEMA:
        raise Refusal("Malformed", "unknown key-set schema")
    epoch = row["epoch"].value if isinstance(row["epoch"], wcb.U64) else None
    if epoch is None:
        raise Refusal("Malformed", "key set epoch is not u64")
    keys = []
    seen = set()
    for item in row["keys"]:
        entry = as_map(item, "key entry")
        exact_keys(
            entry,
            [
                "key",
                "label",
                "valid_from_epoch",
                "valid_until_epoch",
                "revoked_at_epoch",
                "rotated_at_epoch",
            ],
            "key entry",
        )
        key = as_root(entry["key"], "key")
        if key in seen:
            raise Refusal("NonCanonical", "key set repeats a key")
        seen.add(key)
        keys.append(
            {
                "key": key,
                "label": as_text(entry["label"], "key label"),
                "valid_from_epoch": entry["valid_from_epoch"].value,
                "valid_until_epoch": opt_u64(entry["valid_until_epoch"], "valid_until_epoch"),
                "revoked_at_epoch": opt_u64(entry["revoked_at_epoch"], "revoked_at_epoch"),
                "rotated_at_epoch": opt_u64(entry["rotated_at_epoch"], "rotated_at_epoch"),
            }
        )
    if not keys:
        raise Refusal("Malformed", "key set has no keys")
    root = hash_domain(KEY_SET_DOMAIN, raw)
    return root, keys, epoch


def status(entry, set_epoch):
    if entry["revoked_at_epoch"] is not None and set_epoch >= entry["revoked_at_epoch"]:
        return "revoked"
    if entry["rotated_at_epoch"] is not None and set_epoch >= entry["rotated_at_epoch"]:
        return "retired"
    if set_epoch < entry["valid_from_epoch"]:
        return "not_yet_valid"
    if (
        entry["valid_until_epoch"] is not None
        and set_epoch >= entry["valid_until_epoch"]
    ):
        return "expired"
    return "current"


def valid_at(entry, epoch):
    if epoch < entry["valid_from_epoch"]:
        return False
    if entry["valid_until_epoch"] is not None and epoch >= entry["valid_until_epoch"]:
        return False
    if entry["rotated_at_epoch"] is not None and epoch >= entry["rotated_at_epoch"]:
        return False
    if entry["revoked_at_epoch"] is not None and epoch >= entry["revoked_at_epoch"]:
        return False
    return True


def verify(attestation_path, keyset_path, mode="current"):
    raw, value = canonical(attestation_path)
    row = as_map(value, "attestation")
    exact_keys(
        row,
        [
            "schema",
            "producer",
            "run_id",
            "tool",
            "tool_version",
            "determinism",
            "signing_epoch",
            "key_set_root",
            "signer",
            "inputs",
            "output",
            "snapshot",
            "issued_at_ns",
            "signature",
        ],
        "attestation",
    )
    if as_text(row["schema"], "attestation schema") != ATTESTATION_SCHEMA:
        raise Refusal("Malformed", "unknown attestation schema")
    determinism = as_text(row["determinism"], "determinism")
    if determinism not in ("exact", "declared", "unknown"):
        raise Refusal("Malformed", "determinism is not exact|declared|unknown")
    if not isinstance(row["signing_epoch"], wcb.U64):
        raise Refusal("Malformed", "signing_epoch is not u64")
    signing_epoch = row["signing_epoch"].value
    key_set_root = as_root(row["key_set_root"], "key_set_root")
    signer = as_root(row["signer"], "signer")
    output = as_root(row["output"], "output")
    if row["snapshot"] is not None:
        as_root(row["snapshot"], "snapshot")
    for item in row["inputs"]:
        entry = as_map(item, "input")
        exact_keys(entry, ["role", "content_root"], "input")
        as_text(entry["role"], "input role")
        as_root(entry["content_root"], "input content_root")
    signature = row["signature"]
    if not isinstance(signature, wcb.Bytes) or len(signature.value) != 64:
        raise Refusal("Malformed", "signature is not bytes64")

    actual_root, keys, set_epoch = load_key_set(keyset_path)
    if actual_root != key_set_root:
        raise Refusal(
            "KeySetRootMismatch",
            "key set root %s does not match the attestation's %s"
            % (actual_root.hex(), key_set_root.hex()),
        )
    entry = next((candidate for candidate in keys if candidate["key"] == signer), None)
    if entry is None:
        raise Refusal("UnknownSigner", "signer is not in the supplied key set")

    body = {key: value for key, value in row.items() if key != "signature"}
    if not ed25519.verify(signer, wcb.encode(body), signature.value):
        raise Refusal("BadSignature", "signature does not verify under the named signer")

    state = status(entry, set_epoch)
    at_signing = valid_at(entry, signing_epoch)
    if entry["revoked_at_epoch"] is not None and set_epoch >= entry["revoked_at_epoch"]:
        raise Refusal("RevokedKey", "signing key is revoked")
    if mode == "current":
        if state in ("retired", "expired"):
            raise Refusal("KeyRotated", "signing key is %s" % state)
        if state != "current":
            raise Refusal("UnknownSigner", "signing key is %s" % state)
    elif not at_signing:
        raise Refusal("KeyNotYetValid", "signing key was not valid at the signing epoch")

    return {
        "ok": True,
        "signer": signer.hex(),
        "key_set_root": actual_root.hex(),
        "key_set_epoch": set_epoch,
        "signing_epoch": signing_epoch,
        "key_status": state,
        "valid_at_signing": at_signing,
        "currently_trusted": state == "current",
        "producer": as_text(row["producer"], "producer"),
        "run_id": as_text(row["run_id"], "run_id"),
        "tool": as_text(row["tool"], "tool"),
        "tool_version": as_text(row["tool_version"], "tool_version"),
        "determinism": determinism,
        "output": output.hex(),
        "mode": mode,
        "verified_by": "clean-room verifier/producer_attest.py (independent WCB + Ed25519)",
        "limits": "authenticates a declaration; does not prove the computation ran",
    }


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("attestation")
    parser.add_argument("--keyset", required=True)
    parser.add_argument("--mode", default="current", choices=["current", "historical"])
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        result = verify(args.attestation, args.keyset, args.mode)
    except Refusal as refusal:
        if args.json:
            print(json.dumps({"ok": False, "error": refusal.code, "message": refusal.message}))
        else:
            print("%s: %s" % (refusal.code, refusal.message), file=sys.stderr)
        return 1
    except Exception as exc:  # malformed input the reader rejected
        if args.json:
            print(json.dumps({"ok": False, "error": "Malformed", "message": str(exc)}))
        else:
            print("Malformed: %s" % exc, file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(result, sort_keys=True))
    else:
        print(
            "ok signer=%s key_status=%s currently_trusted=%s"
            % (result["signer"], result["key_status"], result["currently_trusted"])
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
