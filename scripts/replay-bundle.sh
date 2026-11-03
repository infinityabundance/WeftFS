#!/usr/bin/env bash
# Extract and verify a WeftFS replay bundle using only Go for the runtime
# verification. Rust is not invoked anywhere in this script.
#
# Usage (inside the FUSE court, from the WeftFS repo root):
#   scripts/docker-court.sh fuse \
#     "bash scripts/replay-bundle.sh --bundle bench/bundles/weft-receipt.tar.zst --out /tmp/replay-out"
#
# The script:
#   1. extracts the tar.zst with the system tar/zstd,
#   2. verifies every file against the bundle's checks.sha256,
#   3. opens the store with the Go frontend and runs `verify --full`,
#   4. resolves every Context Root with the Go frontend,
#   5. materializes every Context Root with the Go frontend,
#   6. re-hashes every store entry and every materialized file against
#      manifest.json with a stdlib-only Go verifier.
set -Eeuo pipefail

ROOT=/work
BUNDLE=""
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --bundle) BUNDLE="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    -h|--help) echo "usage: $0 --bundle FILE --out DIR"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done
if [ -z "$BUNDLE" ] || [ -z "$OUT" ]; then
  echo "usage: $0 --bundle FILE --out DIR" >&2
  exit 64
fi
if [ ! -f "$BUNDLE" ]; then
  echo "replay: bundle not found: $BUNDLE" >&2
  exit 66
fi

BIN="${WEFTFS_BIN:-$ROOT/.docker-cache/f3-demos/bin/go-weftfs}"
VERIFIER=""
if [ -n "${WEFT_REPLAY_VERIFIER:-}" ]; then
  VERIFIER="$WEFT_REPLAY_VERIFIER"
else
  VERIFIER="$OUT/../replay-verify-$$"
fi
if [ ! -x "$BIN" ]; then
  echo "replay: building the independent Go frontend" >&2
  mkdir -p "$(dirname "$BIN")"
  ( cd "$ROOT/WeftFS-Go" && CGO_ENABLED=0 go build -o "$BIN" ./cmd/weftfs )
fi

rm -rf "$OUT"
mkdir -p "$OUT"
echo "== extract =="
tar --zstd -xf "$BUNDLE" -C "$OUT"
echo "== checks.sha256 =="
( cd "$OUT" && sha256sum -c checks.sha256 )

echo "== go open + verify --full =="
"$BIN" --store "$OUT/store" --json verify --full > "$OUT/go-verify.json"
grep -q '"OK": true' "$OUT/go-verify.json"

echo "== go resolve + materialize every Context Root =="
mkdir -p "$OUT/materialized"
while IFS=$'\t' read -r name context_root_id snapshot_id selector_id; do
  [ -n "$name" ] || continue
  "$BIN" --store "$OUT/store" --json context resolve "$name" > "$OUT/resolve-$name.json"
  "$BIN" --store "$OUT/store" --json context materialize "$name" --out "$OUT/materialized/$name" >/dev/null
done < "$OUT/contexts.tsv"

echo "== go manifest verification =="
( cd /tmp && GO111MODULE=off go build -o "$VERIFIER" "$ROOT/scripts/replay/verify_manifest.go" )
"$VERIFIER" "$OUT" "$OUT/materialized" | tee "$OUT/go-manifest-verification.json"
grep -q '"ok": true' "$OUT/go-manifest-verification.json"

echo "replay: ok"
