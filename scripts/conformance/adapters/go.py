#!/usr/bin/env python3
"""Reference adapter: Go `weftcodec` CLI."""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import _shared  # noqa: E402


def default_binary():
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
    candidates = [
        os.path.join(root, ".docker-cache", "weftcodec-go"),
        os.path.join(root, "WeftCodec-Go", "weftcodec"),
    ]
    for candidate in candidates:
        if os.path.isfile(candidate):
            return candidate
    return candidates[0]


if __name__ == "__main__":
    os.environ.setdefault("WEFTCODEC_BIN", default_binary())
    try:
        raise SystemExit(_shared.main("go", "WEFTCODEC_BIN"))
    except _shared.Skip as exc:
        raise SystemExit(_shared.skip(str(exc)))
    except _shared.AdapterFailure as exc:
        print("adapter failure: %s" % exc, file=sys.stderr)
        raise SystemExit(2)
