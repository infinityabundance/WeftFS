"""Every committed conformance frame vector, checked by the Python reader.

This mirrors ``WeftCodec/tests/conformance.rs`` exactly:

* ``superseded_by`` vectors are historical and are not asserted;
* valid vectors must decode byte-exactly and match the recorded plaintext hash,
  mode, content hash, and chunk count;
* reject vectors must raise the recorded normative error class.

Vectors for modes 9–14 are recorded as skips because this reader implements
modes 0–8 and raises ``UnsupportedMode`` for the rest. The skip is explicit in
the summary, never silently dropped.
"""

from __future__ import annotations

import collections

import pytest

import support

from weftcodec.errors import WeftError
from weftcodec.frame import (
    IMPLEMENTED_MODES,
    UNIMPLEMENTED_MODES,
    parse,
)
from weftcodec.modes import decode_frame

FILES = []
for _name, _vector in support.frame_vectors():
    FILES.append((_name, _vector))

IDS = [name for name, _ in FILES]

_RESULTS = {}


def _record(name, mode, outcome, detail=""):
    _RESULTS[name] = {
        "name": name,
        "mode_value": mode,
        "outcome": outcome,
        "detail": detail,
    }
    support.write_summary("WEFT_VECTOR_SUMMARY", _summary())


def _summary():
    counts = collections.Counter(entry["outcome"] for entry in _RESULTS.values())
    reasons = collections.Counter(
        entry["detail"].split(":")[0]
        for entry in _RESULTS.values()
        if entry["outcome"] == "skipped"
    )
    return {
        "vector_root": str(support.VECTORS),
        "implemented_modes": sorted(IMPLEMENTED_MODES),
        "unimplemented_modes": sorted(UNIMPLEMENTED_MODES),
        "total_frame_vectors": len(FILES),
        "asserted": counts["passed"] + counts["failed"],
        "passed": counts["passed"],
        "failed": counts["failed"],
        "skipped": counts["skipped"],
        "skip_reasons": dict(sorted(reasons.items())),
        "vectors": [entry for entry in _RESULTS.values()],
    }


@pytest.mark.parametrize("entry", FILES, ids=IDS)
def test_frame_vector(entry):
    name, vector = entry
    mode = support.vector_mode_value(vector)
    limits = support.vector_limits(vector)
    resolver = support.vector_resolver(vector)
    frame_bytes = bytes.fromhex(vector["frame_hex"])
    expect = vector["expect"]

    if vector.get("superseded_by"):
        detail = "superseded: %s" % vector["superseded_by"]
        _record(name, mode, "skipped", detail)
        pytest.skip(detail)

    is_ok_vector = bool(expect.get("ok"))
    expected_class = expect.get("error")

    if is_ok_vector and mode in UNIMPLEMENTED_MODES:
        detail = "unimplemented_mode: mode %d is not implemented by the Python reader" % mode
        _record(name, mode, "skipped", detail)
        pytest.skip(detail)

    if is_ok_vector:
        try:
            frame = parse(frame_bytes, limits)
            plaintext = decode_frame(frame, resolver, limits)
            assert plaintext == bytes.fromhex(vector["plaintext_hex"]), "%s plaintext" % name
            assert (
                support.content_root(plaintext).hex() == vector["plaintext_hash"]
            ), "%s plaintext hash" % name
            header = frame.header
            assert header.mode_name == vector["mode"], "%s mode" % name
            assert header.mode == vector["mode_value"], "%s mode_value" % name
            assert header.content_hash.hex() == vector["content_hash"], "%s content hash" % name
            assert header.chunk_count == vector["chunk_count"], "%s chunk count" % name
            assert header.decoded_len == vector["decoded_len"], "%s decoded_len" % name
            assert header.body_len == vector["body_len"], "%s body_len" % name
            assert header.format_major == vector["format_major"], "%s major" % name
            assert header.format_minor == vector["format_minor"], "%s minor" % name
            assert header.domain == vector["domain"], "%s domain" % name
            assert header.flags == vector["flags"], "%s flags" % name
            assert header.features == vector["features"], "%s features" % name
        except Exception:
            _record(name, mode, "failed", "valid vector did not decode")
            raise
        _record(name, mode, "passed")
        return

    # Reject vector.
    observed = None
    message = ""
    try:
        decode_frame(parse(frame_bytes, limits), resolver, limits)
    except WeftError as exc:
        observed = exc.error_class
        message = exc.message
    except Exception as exc:  # a panic-equivalent must never happen
        _record(name, mode, "failed", "non-Weft error: %r" % (exc,))
        raise

    if observed is None:
        _record(name, mode, "failed", "expected %s but decode succeeded" % expected_class)
        pytest.fail("%s: expected %s but decode succeeded" % (name, expected_class))

    if observed == expected_class:
        _record(name, mode, "passed", message)
        return

    if mode in UNIMPLEMENTED_MODES and observed == "UnsupportedMode":
        detail = (
            "unimplemented_mode_error: Python=%s vector=%s"
            % (observed, expected_class)
        )
        _record(name, mode, "skipped", detail)
        pytest.skip(detail)

    _record(name, mode, "failed", "expected %s got %s" % (expected_class, observed))
    pytest.fail("%s: expected %s got %s (%s)" % (name, expected_class, observed, message))


def test_summary():
    summary = _summary()
    assert summary["failed"] == 0, summary["vectors"]
    assert summary["total_frame_vectors"] == len(FILES)
    assert summary["passed"] + summary["skipped"] == len(FILES)
