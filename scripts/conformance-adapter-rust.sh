#!/usr/bin/env bash
# Reference conformance adapter for the Rust `weftcodec` CLI.
# Set WEFTCODEC_BIN to the binary; it defaults to the court build path.
set -Eeuo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$here/conformance/adapters/rust.py" "$@"
