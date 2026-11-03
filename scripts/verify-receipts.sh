#!/usr/bin/env bash
# F5.4 / B3 — independent receipt-bundle verifier.
#
# Usage:
#   scripts/verify-receipts.sh BUNDLE [--repo-root DIR]
#
# The verifier never trusts the producer's claims:
#   - rejects absolute or parent-traversing tar entries;
#   - extracts to a private temporary directory;
#   - verifies every file against CHECKSUMS.sha256;
#   - rejects unlisted extra files;
#   - checks the manifest schema and dmesg status;
#   - checks every revision is 40 lowercase hex and matches revisions/<repo>.txt;
#   - with --repo-root, checks every revision exists in the corresponding git
#     repository and resolves to that exact commit.
set -Eeuo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 BUNDLE [--repo-root DIR]" >&2
  exit 64
fi
BUNDLE="$1"; shift
REPO_ROOT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo-root) REPO_ROOT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done
if [ ! -f "$BUNDLE" ]; then
  echo "bundle not found: $BUNDLE" >&2
  exit 66
fi

BUNDLE_SHA="$(sha256sum "$BUNDLE" | awk '{print $1}')"
BUNDLE_SIZE="$(stat -c %s "$BUNDLE")"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/weft-verify-receipts.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

echo "bundle: $BUNDLE"
echo "sha256: $BUNDLE_SHA"
echo "bytes: $BUNDLE_SIZE"

# 1. Tar entry safety before extraction.
if tar -tf "$BUNDLE" | grep -Eq '^/|(^|/)\.\.(/|$)'; then
  echo "FAIL: unsafe absolute or parent-traversing tar entry" >&2
  exit 1
fi

tar --zstd -xf "$BUNDLE" -C "$TMP"
cd "$TMP"

for required in manifest.json CHECKSUMS.sha256 REVISIONS.txt dmesg.txt; do
  if [ ! -f "$required" ]; then
    echo "FAIL: missing $required" >&2
    exit 1
  fi
done

# 2. Every checksum.
if ! sha256sum -c CHECKSUMS.sha256 --quiet; then
  echo "FAIL: checksum mismatch" >&2
  exit 1
fi

# 3. No unlisted files.
find . -type f ! -name '.actual-files' ! -name '.listed-files' ! -name '.file-diff' \
  -printf '%P\n' | LC_ALL=C sort > "$TMP/.actual-files"
{
  awk '{print $2}' CHECKSUMS.sha256 | sed 's|^\./||'
  echo CHECKSUMS.sha256
} | LC_ALL=C sort > "$TMP/.listed-files"
if ! diff -u "$TMP/.listed-files" "$TMP/.actual-files" > "$TMP/.file-diff"; then
  echo "FAIL: checksum list and actual files differ" >&2
  cat "$TMP/.file-diff" >&2 || true
  exit 1
fi

# 4. Manifest shape and dmesg honesty.
if ! grep -q '"schema": "weftfs.receipt-bundle/1"' manifest.json; then
  echo "FAIL: manifest schema is not weftfs.receipt-bundle/1" >&2
  exit 1
fi
if ! grep -q '"dmesg"' manifest.json; then
  echo "FAIL: manifest has no dmesg status" >&2
  exit 1
fi

# 5. Revisions.
REVISION_COUNT=0
while read -r name revision branch worktree changed; do
  [ -n "$name" ] || continue
  if ! printf '%s' "$revision" | grep -Eq '^[0-9a-f]{40}$'; then
    echo "FAIL: revision for $name is not 40 lowercase hex: $revision" >&2
    exit 1
  fi
  if [ ! -f "revisions/$name.txt" ]; then
    echo "FAIL: missing revisions/$name.txt" >&2
    exit 1
  fi
  if [ "$(cat "revisions/$name.txt")" != "$revision" ]; then
    echo "FAIL: revisions/$name.txt does not match REVISIONS.txt" >&2
    exit 1
  fi
  if [ -n "$REPO_ROOT" ]; then
    case "$name" in
      WeftFS) path="$REPO_ROOT" ;;
      WeftCodec) path="$REPO_ROOT/WeftCodec" ;;
      WeftCodec-Go) path="$REPO_ROOT/WeftCodec-Go" ;;
      WeftFS-Go) path="$REPO_ROOT/WeftFS-Go" ;;
      *) echo "FAIL: unknown repository $name" >&2; exit 1 ;;
    esac
    if ! git -C "$path" cat-file -e "$revision^{commit}" 2>/dev/null; then
      echo "FAIL: $name revision $revision is not a commit in $path" >&2
      exit 1
    fi
    resolved="$(git -C "$path" rev-parse --verify "$revision^{commit}")"
    if [ "$resolved" != "$revision" ]; then
      echo "FAIL: $name revision $revision resolved to $resolved" >&2
      exit 1
    fi
  fi
  REVISION_COUNT=$((REVISION_COUNT + 1))
done < REVISIONS.txt

if [ "$REVISION_COUNT" -lt 4 ]; then
  echo "FAIL: expected four repository revisions, found $REVISION_COUNT" >&2
  exit 1
fi

# 6. No research tree, no producer host paths retained.
if find . -type f -path '*research/*' | grep -q .; then
  echo "FAIL: research path present in bundle" >&2
  exit 1
fi
if grep -RIl -e '/mnt/1tb_kingston' . 2>/dev/null \
    | grep -v '^\./harness/scripts/verify-receipts\.sh$' \
    | grep -q .; then
  echo "FAIL: producer host path remains in bundle" >&2
  exit 1
fi

FILE_COUNT="$(wc -l < "$TMP/.listed-files" | tr -d ' ')"
echo "verified files: $FILE_COUNT"
echo "verified revisions: $REVISION_COUNT"
echo "dmesg: $(head -1 dmesg.txt)"
echo "result: pass"
