#!/usr/bin/env bash
# Reference conformance adapter for the Go `weftcodec` CLI.
# Set WEFTCODEC_BIN to the binary; it defaults to .docker-cache/weftcodec-go.
set -Eeuo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$here/conformance/adapters/go.py" "$@"
