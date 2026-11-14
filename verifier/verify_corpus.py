#!/usr/bin/env python3
"""Independent semantic verifier for the WeftFS context plane (draft RFC 0001).

Runs only against the published specification text and the frozen corpus in
`conformance/`. It does not import, link or shell out to `weftcodec`,
`weftfs`, the Go modules or `python/weftcodec`.

    python3 verifier/verify_corpus.py --json bench/receipts/context-plane-verifier-<stamp>.json

Exit status: 0 when every case it ran passed (skips are allowed and counted),
1 when any case failed, 2 on a harness error.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

import blake3 as blake3_module  # noqa: E402
import ed25519  # noqa: E402
import hashing  # noqa: E402
import records  # noqa: E402
import wcb  # noqa: E402
from wcb import Bytes, I64, Limits, Timestamp, U64, WcbError  # noqa: E402

ROOT = os.path.dirname(HERE)


def utc_stamp() -> str:
    return time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())


def git_revision(path: str, env_name: str | None = None) -> str:
    if env_name and os.environ.get(env_name):
        return os.environ[env_name]
    import subprocess

    try:
        return subprocess.check_output(
            ["git", "-C", path, "rev-parse", "HEAD"], text=True
        ).strip()
    except Exception:  # pragma: no cover - outside a checkout
        return "unknown"


def verifier_digest() -> tuple:
    files = sorted(
        name for name in os.listdir(HERE) if name.endswith(".py")
    )
    digest = hashlib.sha256()
    total = 0
    for name in files:
        with open(os.path.join(HERE, name), "rb") as handle:
            data = handle.read()
        total += data.count(b"\n")
        digest.update(name.encode("ascii") + b"\x00" + data)
    return digest.hexdigest(), files, total


def load_json(path: str):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def as_bytes(value, where: str) -> bytes:
    if not isinstance(value, str):
        raise ValueError("%s is not a hex string" % where)
    return bytes.fromhex(value)


class Case:
    __slots__ = ("path", "family", "expect_ok", "expected_class", "outcome",
                 "detail", "computed", "expected")

    def __init__(self, path, family):
        self.path = path
        self.family = family
        self.expect_ok = True
        self.expected_class = None
        self.outcome = "fail"
        self.detail = ""
        self.computed = None
        self.expected = None

    def to_json(self):
        return {
            "path": self.path,
            "family": self.family,
            "expect": "ok" if self.expect_ok else self.expected_class,
            "outcome": self.outcome,
            "detail": self.detail,
            "computed": self.computed,
            "expected": self.expected,
        }


def _classify(exc: Exception) -> str:
    if isinstance(exc, WcbError):
        return exc.klass
    return "HarnessError"


def _refusal_or_failure(case: Case, entry, exc: WcbError) -> None:
    if entry["expect"].get("error"):
        _expect_error(case, entry, exc)
    else:
        case.outcome = "fail"
        case.detail = "unexpected refusal %s: %s" % (exc.klass, exc.message)


def _expect_ok(case: Case, entry, computed_id, declared_id, domain):
    case.expect_ok = True
    if declared_id is not None and computed_id != declared_id:
        case.outcome = "fail"
        case.detail = "computed %s does not match the declared identity" % domain
        case.computed = computed_id.hex() if isinstance(computed_id, bytes) else computed_id
        case.expected = declared_id.hex() if isinstance(declared_id, bytes) else declared_id
        return
    expected = entry["expect"].get("content_root")
    if expected and isinstance(computed_id, bytes) and expected != computed_id.hex():
        case.outcome = "fail"
        case.detail = "computed identity does not match the manifest content_root"
        case.computed = computed_id.hex()
        case.expected = expected
        return
    case.outcome = "pass"
    case.detail = "identity and canonical bytes verified"
    case.computed = computed_id.hex() if isinstance(computed_id, bytes) else computed_id


def _expect_error(case: Case, entry, exc):
    case.expect_ok = False
    case.expected_class = entry["expect"]["error"]
    actual = _classify(exc)
    if actual == case.expected_class:
        case.outcome = "pass"
        case.detail = "refused with the declared class"
        case.computed = actual
        case.expected = case.expected_class
    else:
        case.outcome = "fail"
        case.detail = "refused with %s, expected %s" % (actual, case.expected_class)
        case.computed = actual
        case.expected = case.expected_class


# ---------------------------------------------------------------------------
# Family handlers


def case_wcb(entry, path):
    case = Case(path, "wcb")
    data = load_json(path)
    try:
        if "canonical_hex" in data:
            raw = as_bytes(data["canonical_hex"], "canonical_hex")
            wcb.decode_and_reencode(raw)
            ident = hashing.hash_domain("weft/structured/v1", raw)
            # `decoded_sha256` is the SHA-256 of the canonical bytes.
            digest = hashlib.sha256(raw).hexdigest()
            declared = entry["expect"].get("decoded_sha256")
            if declared and declared != digest:
                case.outcome = "fail"
                case.detail = "canonical bytes do not match decoded_sha256"
                return case
            _expect_ok(case, entry, ident, None, "weft/structured/v1")
        else:
            raw = as_bytes(data["hex"], "hex")
            try:
                wcb.decode_and_reencode(raw)
            except WcbError as exc:
                _expect_error(case, entry, exc)
                return case
            raise AssertionError("noncanonical WCB input decoded cleanly")
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


def case_selector(entry, path):
    case = Case(path, "selector")
    data = load_json(path)
    try:
        if "canonical_hex" in data:
            raw = as_bytes(data["canonical_hex"], "canonical_hex")
            result = records.validate_selector(raw)
            declared = (
                as_bytes(data["selector_id"], "selector_id")
                if "selector_id" in data else None
            )
            _expect_ok(case, entry, result["selector_id"], declared, "weft/selector/v1")
        else:
            raw = as_bytes(data.get("record_hex") or data.get("hex"), "record")
            try:
                records.validate_selector(raw)
            except WcbError as exc:
                _expect_error(case, entry, exc)
                return case
            raise AssertionError("selector refusal decoded cleanly")
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


def case_context_root(entry, path):
    case = Case(path, "context_root")
    data = load_json(path)
    try:
        if "canonical_hex" in data:
            raw = as_bytes(data["canonical_hex"], "canonical_hex")
            result = records.validate_context_root(raw)
            declared = (
                as_bytes(data["context_root_id"], "context_root_id")
                if "context_root_id" in data else None
            )
            _expect_ok(case, entry, result["context_root_id"], declared,
                       "weft/context-root/v1")
        else:
            raw = as_bytes(data["record_hex"], "record_hex")
            try:
                records.validate_context_root(raw)
            except WcbError as exc:
                _expect_error(case, entry, exc)
                return case
            raise AssertionError("context-root refusal decoded cleanly")
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


def case_causal(entry, path):
    case = Case(path, "causal")
    data = load_json(path)
    try:
        if "canonical_hex" in data:
            raw = as_bytes(data["canonical_hex"], "canonical_hex")
            result = records.validate_causal(raw)
            declared = (
                as_bytes(data["closure_hash"], "closure_hash")
                if "closure_hash" in data else None
            )
            _expect_ok(case, entry, result["closure_hash"], declared, "weft/causal/v1")
        else:
            raw = as_bytes(data["record_hex"], "record_hex")
            try:
                records.validate_causal(raw)
            except WcbError as exc:
                _expect_error(case, entry, exc)
                return case
            raise AssertionError("causal refusal decoded cleanly")
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


def case_logical_tensor(entry, path):
    case = Case(path, "tensor_logical")
    data = load_json(path)
    try:
        if "canonical_hex" in data:
            raw = as_bytes(data["canonical_hex"], "canonical_hex")
            result = records.validate_logical_tensor(raw)
            declared = (
                as_bytes(data["logical_root"], "logical_root")
                if "logical_root" in data else None
            )
            if "values_hex" in data:
                values = as_bytes(data["values_hex"], "values_hex")
                values_hash = hashing.hash_domain("weft/tensor/values/v1", values)
                if values_hash != result["values_hash"]:
                    case.outcome = "fail"
                    case.detail = "values_hash does not hash the declared values"
                    return case
            if "values_hash" in data and as_bytes(data["values_hash"], "values_hash") != result["values_hash"]:
                case.outcome = "fail"
                case.detail = "record values_hash differs from the vector"
                return case
            _expect_ok(case, entry, result["logical_root"], declared,
                       "weft/tensor/logical/v1")
        else:
            raw = as_bytes(data["record_hex"], "record_hex")
            try:
                records.validate_logical_tensor(raw)
            except WcbError as exc:
                _expect_error(case, entry, exc)
                return case
            raise AssertionError("logical-tensor refusal decoded cleanly")
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


def case_tensor_layout(entry, path):
    case = Case(path, "tensor_layout")
    data = load_json(path)
    try:
        if "canonical_hex" in data:
            raw = as_bytes(data["canonical_hex"], "canonical_hex")
            records.validate_tensor_layout(raw)
            case.outcome = "pass"
            case.detail = "layout descriptor is canonical and self-consistent"
        else:
            raw = as_bytes(data["record_hex"], "record_hex")
            try:
                records.validate_tensor_layout(raw)
            except WcbError as exc:
                _expect_error(case, entry, exc)
                return case
            raise AssertionError("layout refusal decoded cleanly")
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


def case_semantic_root(entry, path):
    case = Case(path, "semantic_root")
    data = load_json(path)
    try:
        if "canonical_hex" in data:
            raw = as_bytes(data["canonical_hex"], "canonical_hex")
            result = records.validate_semantic_root(raw)
            if "values_hex" in data:
                records.verify_semantic_values(
                    as_bytes(data["values_hex"], "values_hex"), result
                )
            declared = (
                as_bytes(data["semantic_root"], "semantic_root")
                if "semantic_root" in data else None
            )
            _expect_ok(case, entry, result["semantic_root"], declared,
                       "weft/tensor/semantic/v1")
        else:
            raw = as_bytes(data["record_hex"], "record_hex")
            try:
                records.validate_semantic_root(raw)
            except WcbError as exc:
                _expect_error(case, entry, exc)
                return case
            raise AssertionError("semantic-root refusal decoded cleanly")
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


def case_capability(entry, path):
    case = Case(path, "capability")
    data = load_json(path)
    expected_class = entry["expect"].get("error")
    try:
        raw = as_bytes(data.get("blob_hex"), "blob_hex")
        if expected_class is None:
            blob = records.validate_capability_blob(raw)
            envelope = blob["envelope"]
            if not ed25519.verify(
                envelope["grantor"], envelope["sign_input"], blob["signature"]
            ):
                case.outcome = "fail"
                case.detail = "valid envelope signature did not verify"
                return case
            if "sign_input" in data and envelope["sign_input"].hex() != data["sign_input"]:
                case.outcome = "fail"
                case.detail = "sign_input differs from the vector"
                return case
            for field, identity in (
                ("blob_id", blob["blob_id"]),
                ("envelope_id", envelope["envelope_id"]),
            ):
                if field in data and identity.hex() != data[field]:
                    case.outcome = "fail"
                    case.detail = "%s differs from the vector" % field
                    return case
            _expect_ok(case, entry, envelope["envelope_id"], None,
                       "weft/capability/envelope/v1")
            return case

        if expected_class == "NonCanonical":
            try:
                records.validate_capability_blob(raw)
            except WcbError as exc:
                _expect_error(case, entry, exc)
                return case
            raise AssertionError("noncanonical blob decoded cleanly")

        blob = records.validate_capability_blob(raw)
        envelope = blob["envelope"]
        now = data.get("now_ns")
        if expected_class == "BadSignature":
            if ed25519.verify(envelope["grantor"], envelope["sign_input"], blob["signature"]):
                raise AssertionError("bad signature verified")
            _expect_error(case, entry, WcbError("BadSignature", "signature rejected"))
        elif expected_class == "WrongGrantee":
            if "grantee" not in data:
                raise AssertionError("vector has no expected grantee")
            if envelope["grantee"] == as_bytes(data["grantee"], "grantee"):
                raise AssertionError("grantee unexpectedly matched")
            _expect_error(case, entry, WcbError("WrongGrantee", "grantee mismatch"))
        elif expected_class == "Expired":
            if not isinstance(now, int) or envelope["expires_at_ns"] is None:
                raise AssertionError("vector has no expiry evidence")
            if now <= envelope["expires_at_ns"]:
                raise AssertionError("capability is not expired at now_ns")
            _expect_error(case, entry, WcbError("Expired", "expiry passed"))
        elif expected_class == "Revoked":
            revocations = {
                as_bytes(item, "revocation") for item in data.get("revocations", [])
            }
            if envelope["revocation_id"] not in revocations:
                raise AssertionError("revocation id is not in the supplied set")
            _expect_error(case, entry, WcbError("Revoked", "revocation id present"))
        else:
            raise AssertionError("unsupported capability expectation %r" % expected_class)
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


def case_kv_prov(entry, path):
    case = Case(path, "kv_prov")
    data = load_json(path)
    try:
        if "frame_hex" in data:
            frames = [(data["frame_hex"], data.get("descriptor_root"))]
        else:
            frames = [
                (page["frame_hex"], page.get("descriptor_root"))
                for page in data.get("pages", [])
            ]
        if not frames:
            raise AssertionError("vector carries no frame")
        roots = []
        for frame_hex, declared in frames:
            raw = as_bytes(frame_hex, "frame_hex")
            try:
                result = records.validate_kv_prov_frame(raw)
            except WcbError as exc:
                _refusal_or_failure(case, entry, exc)
                return case
            if declared is not None:
                expected = as_bytes(declared, "descriptor_root")
                if result["descriptor_root"] != expected:
                    case.outcome = "fail"
                    case.detail = "descriptor_root differs from the vector"
                    return case
            roots.append(result["descriptor_root"])
        if entry["expect"].get("error"):
            case.outcome = "fail"
            case.detail = "frame decoded cleanly but the vector expects a refusal"
            return case
        _expect_ok(case, entry, roots[0], None, "weft/kv/prov/v1")
    except WcbError as exc:
        _refusal_or_failure(case, entry, exc)
    return case


# ---------------------------------------------------------------------------
# Negative suite


def negative_case(name, expectation, callable_):
    entry = {"name": name, "expect": expectation, "outcome": "fail", "detail": ""}
    try:
        callable_()
        if expectation is None:
            entry["outcome"] = "pass"
            entry["detail"] = "accepted as canonical"
            return entry
        entry["detail"] = "accepted, expected %s" % expectation
    except WcbError as exc:
        if expectation is None:
            entry["outcome"] = "pass"
            entry["detail"] = "refused with %s (acceptable for this case)" % exc.klass
        elif exc.klass == expectation:
            entry["outcome"] = "pass"
            entry["detail"] = "refused with %s" % exc.klass
        else:
            entry["detail"] = "refused with %s, expected %s" % (exc.klass, expectation)
    except AssertionError as exc:
        entry["detail"] = str(exc)
    except Exception as exc:  # noqa: BLE001 - report, do not hide
        entry["detail"] = "%s: %s" % (type(exc).__name__, exc)
    return entry


def run_negative_suite():
    selector_path = os.path.join(
        ROOT, "WeftCodec/conformance/v1/selectors/001_select_true.json"
    )
    selector_data = load_json(selector_path)
    selector_raw = as_bytes(selector_data["canonical_hex"], "canonical_hex")
    selector_id = as_bytes(selector_data["selector_id"], "selector_id")
    results = []

    def flipped():
        mutated = bytearray(selector_raw)
        mutated[-1] ^= 0x01
        try:
            record = wcb.decode_and_reencode(bytes(mutated))
            del record
        except WcbError:
            return
        result = records.validate_selector(bytes(mutated))
        if result["selector_id"] == selector_id:
            raise AssertionError("flipped byte kept the declared selector id")

    results.append(negative_case("flipped_byte", None, flipped))

    results.append(negative_case(
        "noncanonical_map_order", "NonCanonical",
        lambda: wcb.decode_and_reencode(
            bytes.fromhex("080205016203010501610302")
        ),
    ))
    results.append(negative_case(
        "duplicate_key", "NonCanonical",
        lambda: wcb.decode_and_reencode(
            bytes.fromhex("080205016103010501610302")
        ),
    ))

    def extra_key():
        value = wcb.decode(selector_raw)
        value["zzz_extra"] = U64(1)
        records.validate_selector(wcb.encode(value))

    results.append(negative_case("extra_unknown_key", "NonCanonical", extra_key))

    def wrong_domain():
        other = hashing.hash_domain("weft/chunk/v1", selector_raw)
        if other == selector_id:
            raise AssertionError("wrong-domain hash collided with the declared id")

    results.append(negative_case("wrong_hash_domain", None, wrong_domain))

    results.append(negative_case(
        "truncated_record", "Malformed",
        lambda: wcb.decode_and_reencode(selector_raw[:-1]),
    ))
    results.append(negative_case(
        "oversized_record", "LimitExceeded",
        lambda: wcb.decode(
            wcb.encode([U64(0)] * 5), Limits(max_items=4)
        ),
    ))
    return results


def run_provenance_self_test():
    """No frozen `weft.provenance.v1` vector exists; these are self-tests."""
    record = {
        "schema": "weft.provenance.v1",
        "operation": "transcode",
        "params": {"level": U64(3)},
        "inputs": [
            {"role": "base", "content": Bytes(b"\x11" * 32)},
        ],
        "outputs": [Bytes(b"\x22" * 32)],
        "environment": {
            "tool": "weftfs",
            "version": "1.4.2",
            "env_hash": Bytes(b"\x33" * 32),
        },
        "determinism": "exact",
        "principal": {"uid": U64(1000), "gid": U64(1000)},
        "timestamp_ns": Timestamp(1700000000000000000),
    }
    raw = wcb.encode(record)
    results = []

    def positive():
        result = records.validate_provenance(raw)
        expected = hashing.hash_domain("weft/provenance/v1", raw)
        if result["provenance_id"] != expected:
            raise AssertionError("provenance id mismatch")

    results.append(negative_case("provenance_positive", None, positive))
    if results[-1]["outcome"] == "fail":
        return results

    def unsorted_inputs():
        bad = dict(record)
        bad["inputs"] = [
            {"role": "z", "content": Bytes(b"\x11" * 32)},
            {"role": "a", "content": Bytes(b"\x11" * 32)},
        ]
        records.validate_provenance(wcb.encode(bad))

    results.append(negative_case("provenance_unsorted_inputs", "NonCanonical",
                                 unsorted_inputs))

    def duplicate_outputs():
        bad = dict(record)
        bad["outputs"] = [Bytes(b"\x22" * 32), Bytes(b"\x22" * 32)]
        records.validate_provenance(wcb.encode(bad))

    results.append(negative_case("provenance_duplicate_outputs", "NonCanonical",
                                 duplicate_outputs))

    def unknown_key():
        bad = dict(record)
        bad["extra"] = U64(1)
        records.validate_provenance(wcb.encode(bad))

    results.append(negative_case("provenance_unknown_key", "NonCanonical",
                                 unknown_key))

    results.append(negative_case(
        "provenance_truncated", "Malformed",
        lambda: records.validate_provenance(raw[:-1]),
    ))

    def oversized_params():
        bad = dict(record)
        bad["params"] = {"blob": "x" * 70000}
        records.validate_provenance(wcb.encode(bad))

    results.append(negative_case("provenance_oversized_params", "LimitExceeded",
                                 oversized_params))

    def wrong_domain():
        other = hashing.hash_domain("weft/causal/v1", raw)
        expected = hashing.hash_domain("weft/provenance/v1", raw)
        if other == expected:
            raise AssertionError("wrong-domain hash collided")

    results.append(negative_case("provenance_wrong_domain", None, wrong_domain))
    return results


# ---------------------------------------------------------------------------
# Manifest drive


def freeze_matrix_rows() -> int:
    path = os.path.join(ROOT, "spec/CONTEXT-PLANE-FREEZE.md")
    if not os.path.isfile(path):
        return 0
    count = 0
    in_section = False
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            if line.startswith("## 2."):
                in_section = True
                continue
            if in_section and line.startswith("## 3."):
                break
            if in_section and line.startswith("| "):
                first = line.split("|")[1].strip()
                if first.isdigit():
                    count += 1
    return count


def run(root: str, manifest_path: str):
    manifest = load_json(manifest_path)
    cases = []
    for entry in manifest["vectors"]:
        path = entry["path"]
        family = None
        if entry["kind"] == "wcb":
            family = "wcb"
        elif entry["kind"] == "selector":
            family = "selector"
        elif entry["kind"] == "context":
            subkind = entry.get("subkind")
            if subkind == "context_root":
                family = "context_root"
            elif subkind == "causal_closure":
                family = "causal"
            else:
                case = Case(path, "context_other")
                case.outcome = "skip"
                case.detail = "composite store-level vector, not a single record"
                cases.append(case)
                continue
        elif entry["kind"] == "tensor":
            directory = os.path.dirname(path)
            if directory.endswith("/semantic-root"):
                family = "semantic_root"
            elif directory.endswith("/tensor"):
                data = load_json(os.path.join(root, path))
                kind = data.get("kind")
                if kind == "layout":
                    family = "tensor_layout"
                elif kind == "logical":
                    family = "tensor_logical"
                else:
                    case = Case(path, "tensor_other")
                    case.outcome = "skip"
                    case.detail = "shard-set container is outside the reader families"
                    cases.append(case)
                    continue
            else:
                case = Case(path, "tensor_other")
                case.outcome = "skip"
                case.detail = "family not implemented by this verifier"
                cases.append(case)
                continue
        elif entry["kind"] == "capability":
            family = "capability"
        elif entry["kind"] == "frame" and entry.get("mode") == 19:
            directory = os.path.dirname(path)
            if "/kv-prov" not in directory:
                case = Case(path, "kv_prov")
                case.outcome = "skip"
                case.detail = "superseded reserved-mode vector"
                cases.append(case)
                continue
            family = "kv_prov"
        else:
            continue

        if entry.get("superseded_by"):
            case = Case(path, family)
            case.outcome = "skip"
            case.detail = "superseded by %s" % entry["superseded_by"]
            cases.append(case)
            continue

        full = os.path.join(root, path)
        try:
            if family == "wcb":
                case = case_wcb(entry, full)
            elif family == "selector":
                case = case_selector(entry, full)
            elif family == "context_root":
                case = case_context_root(entry, full)
            elif family == "causal":
                case = case_causal(entry, full)
            elif family == "tensor_logical":
                case = case_logical_tensor(entry, full)
            elif family == "tensor_layout":
                case = case_tensor_layout(entry, full)
            elif family == "semantic_root":
                case = case_semantic_root(entry, full)
            elif family == "capability":
                case = case_capability(entry, full)
            elif family == "kv_prov":
                case = case_kv_prov(entry, full)
            else:  # pragma: no cover
                raise AssertionError("unknown family")
        except Exception as exc:  # noqa: BLE001 - a harness error is a failure
            case = Case(path, family)
            case.outcome = "fail"
            case.detail = "harness error: %s: %s" % (type(exc).__name__, exc)
        cases.append(case)
    return manifest, cases


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=ROOT)
    parser.add_argument("--manifest", default=None)
    parser.add_argument("--json", default=None, help="receipt path")
    parser.add_argument("--log", default=None, help="raw log path")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)

    root = os.path.abspath(args.root)
    manifest_path = args.manifest or os.path.join(root, "conformance/manifest.json")
    receipt_path = args.json
    log_path = args.log
    if receipt_path and not log_path:
        log_path = os.path.splitext(receipt_path)[0] + ".log"

    # Self-tests are part of the evidence.
    blake3_module.self_test()

    manifest, cases = run(root, manifest_path)
    negatives = run_negative_suite()
    provenance = run_provenance_self_test()

    counts = {}
    for case in cases:
        counts.setdefault(case.family, {"total": 0, "pass": 0, "fail": 0, "skip": 0})
        counts[case.family]["total"] += 1
        counts[case.family][case.outcome] += 1

    negative_pass = sum(1 for item in negatives if item["outcome"] == "pass")
    provenance_pass = sum(1 for item in provenance if item["outcome"] == "pass")
    passed = sum(1 for case in cases if case.outcome == "pass")
    failed = sum(1 for case in cases if case.outcome == "fail")
    skipped = sum(1 for case in cases if case.outcome == "skip")

    lines = []
    lines.append("context-plane verifier (clean-room, stdlib only)")
    lines.append("manifest: %s" % os.path.relpath(manifest_path, root))
    lines.append("aggregate: %s" % manifest.get("aggregate_sha256", "?"))
    lines.append("vectors: %d" % len(cases))
    lines.append("")
    for case in cases:
        lines.append("%-6s %-18s %s%s" % (
            case.outcome.upper(), case.family, case.path,
            (": " + case.detail) if case.detail else "",
        ))
    lines.append("")
    lines.append("negative suite:")
    for item in negatives:
        lines.append("  %-6s %-32s %s" % (item["outcome"].upper(), item["name"],
                                          item["detail"]))
    lines.append("provenance self-test (no frozen corpus):")
    for item in provenance:
        lines.append("  %-6s %-32s %s" % (item["outcome"].upper(), item["name"],
                                          item["detail"]))
    lines.append("")
    lines.append(
        "summary: %d cases, %d pass, %d fail, %d skip; negative %d/%d; "
        "provenance self-test %d/%d"
        % (len(cases), passed, failed, skipped, negative_pass, len(negatives),
           provenance_pass, len(provenance))
    )
    text = "\n".join(lines) + "\n"

    if not args.quiet:
        sys.stdout.write(text)
    if log_path:
        with open(log_path, "w", encoding="utf-8") as handle:
            handle.write(text)

    digest, files, line_count = verifier_digest()
    receipt = {
        "schema": "weft.context_plane_verifier.v1",
        "generated_utc": utc_stamp(),
        "environment": {
            "revisions": {
                "WeftFS": git_revision(root, "WEFT_REPO_WEFTFS"),
                "WeftCodec": git_revision(
                    os.path.join(root, "WeftCodec"), "WEFT_REPO_WEFTCODEC"
                ),
                "WeftCodec-Go": git_revision(
                    os.path.join(root, "WeftCodec-Go"), "WEFT_REPO_WEFTCODEC_GO"
                ),
                "WeftFS-Go": git_revision(
                    os.path.join(root, "WeftFS-Go"), "WEFT_REPO_WEFTFS_GO"
                ),
            },
            "python": sys.version.split()[0],
            "implementation": "verifier/ (stdlib only)",
            "blake3": "pure-Python implementation in verifier/blake3.py",
        },
        "verifier": {
            "language": "Python",
            "stdlib_only": True,
            "files": files,
            "source_lines": line_count,
            "source_sha256": digest,
            "linkage": "none: no import, FFI or subprocess call into WeftFS code",
            "independence": (
                "Authored by the same organisation from the published text. "
                "Independent of the implementation code path, not an external "
                "third-party implementation."
            ),
        },
        "manifest": {
            "path": "conformance/manifest.json",
            "vector_count": manifest.get("vector_count"),
            "aggregate_sha256": manifest.get("aggregate_sha256"),
        },
        "freeze_matrix_rows": freeze_matrix_rows(),
        "summary": {
            "cases": len(cases),
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "negative_cases": len(negatives),
            "negative_passed": negative_pass,
            "provenance_self_test_cases": len(provenance),
            "provenance_self_test_passed": provenance_pass,
            "result": "pass" if failed == 0 and negative_pass == len(negatives)
                      and provenance_pass == len(provenance) else "fail",
        },
        "families": counts,
        "cases": [case.to_json() for case in cases],
        "negative_suite": negatives,
        "provenance_self_test": provenance,
        "unspecified": records.UNSPECIFIED,
    }
    if receipt_path:
        with open(receipt_path, "w", encoding="utf-8") as handle:
            json.dump(receipt, handle, indent=1, sort_keys=True)
            handle.write("\n")
    return 0 if receipt["summary"]["result"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
