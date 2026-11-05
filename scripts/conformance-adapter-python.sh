#!/usr/bin/env bash
# Reference conformance adapter for the independent Python `weftcodec` reader.
# Set WEFT_PYTHONPATH to the directory containing the `weftcodec` package.
set -Eeuo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$here/conformance/adapters/python.py" "$@"
