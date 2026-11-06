#!/usr/bin/env bash
# Q6 — store-backed provenance linkage for a signed receipt bundle.
#
# Takes a signed bundle, stores its canonical manifest as a real WeftFS file,
# pins a snapshot, records a `weft.provenance.v1` receipt whose output is that
# file, publishes a `derived_from` relation to the snapshot, and proves with
# `weft provenance show` plus a tamper case that the linkage is resolvable and
# that a modified bundle no longer matches either the signature or the recorded
# output hash.
#
# Everything that compiles or touches a store runs in the hardened Rust court.
# The host only orchestrates tar, JSON formatting, and temp paths.
#
# Usage: scripts/demo-signed-bundle.sh BUNDLE
# Output: one JSON object on stdout; log on stderr.
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ $# -ne 1 ]; then
  echo "usage: $0 BUNDLE" >&2
  exit 64
fi
BUNDLE="$1"
if [ ! -f "$BUNDLE" ]; then
  echo "bundle not found: $BUNDLE" >&2
  exit 66
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
CACHE="$ROOT/.docker-cache"
mkdir -p "$CACHE"
EXTRACT="$CACHE/q6-demo-bundle-$STAMP"
STORE="$CACHE/q6-demo-store-$STAMP"
TAMPER="$CACHE/q6-demo-tamper-$STAMP"
TAMPER_STORE="$CACHE/q6-demo-tamper-store-$STAMP"
RECORD="$CACHE/q6-demo-provenance-$STAMP.json"
rm -rf "$EXTRACT" "$STORE" "$TAMPER" "$TAMPER_STORE"
mkdir -p "$EXTRACT"
tar --zstd -xf "$BUNDLE" -C "$EXTRACT"

rel() { printf '%s' "${1#"$ROOT/"}"; }
log() { printf '%s\n' "$*" >&2; }

# Run the shipped CLI in the Rust court. Arguments are shell-quoted; none of
# the caller-controlled values contain whitespace beyond JSON/hex.
weft() {
  local quoted=""
  local arg
  for arg in "$@"; do
    quoted+=" $(printf '%q' "$arg")"
  done
  "$ROOT/scripts/docker-court.sh" rust \
    "cd /work && cargo run --quiet --bin weftfs --${quoted}" 2>>"$CACHE/q6-demo-$STAMP.log"
}

helper() {
  "$ROOT/scripts/receipt-bundle-court.sh" "$@" 2>>"$CACHE/q6-demo-$STAMP.log"
}

json_get() {
  python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"
}

EXTRACT_REL="$(rel "$EXTRACT")"
STORE_REL="$(rel "$STORE")"
TAMPER_REL="$(rel "$TAMPER")"
TAMPER_STORE_REL="$(rel "$TAMPER_STORE")"
RECORD_REL="$(rel "$RECORD")"

log "digest"
DIGEST_JSON="$(helper digest --root "$EXTRACT_REL" | tail -1)"
MANIFEST_SHA="$(printf '%s' "$DIGEST_JSON" | json_get manifest_sha256)"
MANIFEST_CONTENT_ROOT="$(printf '%s' "$DIGEST_JSON" | json_get manifest_content_root)"
PUBLIC_KEY="$(tr -d '\n' < "$EXTRACT/public_key")"
FINGERPRINT="$(helper fingerprint --public-key "$PUBLIC_KEY" | tail -1)"
SIGNATURE="$(tr -d '\n' < "$EXTRACT/signature")"

log "store init + manifest file"
weft --store "$STORE_REL" init >/dev/null
OBJECT_ID="$(weft --store "$STORE_REL" create /bundle.manifest --file "$EXTRACT_REL/receipt-manifest.wcb" | tail -1)"
SNAP_JSON="$(weft --store "$STORE_REL" --json snapshot create q6-signed-bundle)"
SNAPSHOT="$(printf '%s' "$SNAP_JSON" | json_get snapshot_id)"
STAT_JSON="$(weft --store "$STORE_REL" --json stat /bundle.manifest)"
CONTENT_ID="$(printf '%s' "$STAT_JSON" | json_get content_id)"

log "record provenance"
python3 - "$RECORD" "$MANIFEST_SHA" "$MANIFEST_CONTENT_ROOT" "$FINGERPRINT" "$SNAPSHOT" "$CONTENT_ID" <<'PY'
import json, sys
path, manifest_sha, content_root, fingerprint, snapshot, content_id = sys.argv[1:]
record = {
    "schema": "weft.provenance.v1",
    "operation": "package-receipts",
    "params": {
        "bundle_stamp": path.rsplit("-", 1)[-1].split(".")[0],
        "manifest_sha256": manifest_sha,
        "manifest_content_root": content_root,
        "signature_fingerprint": fingerprint,
    },
    "inputs": [{"role": "derived_from", "content": snapshot, "object": None}],
    "outputs": [content_id],
    "environment": {"tool": "weft-receipt-bundle", "version": "0.7.0"},
    "determinism": "exact",
    "principal": {"uid": 0, "gid": 0},
    "timestamp_ns": 1700000000000000000,
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(record, handle, sort_keys=True)
PY
PROV_JSON="$(weft --store "$STORE_REL" --json provenance record --record "$RECORD_REL" --output /bundle.manifest)"
PROVENANCE="$(printf '%s' "$PROV_JSON" | json_get provenance_id)"
REL_JSON="$(weft --store "$STORE_REL" --json relation add --subject /bundle.manifest --predicate derived_from --value "$SNAPSHOT")"
RELATION="$(printf '%s' "$REL_JSON" | json_get relation_id)"
SHOW="$(weft --store "$STORE_REL" provenance show /bundle.manifest)"
grep -F -q "provenance_id=$PROVENANCE" <<<"$SHOW"
grep -F -q "$SNAPSHOT" <<<"$SHOW"
grep -F -q "derived_from" <<<"$SHOW"
REL_LIST="$(weft --store "$STORE_REL" --json relation list --subject /bundle.manifest --predicate derived_from)"
grep -F -q "$SNAPSHOT" <<<"$REL_LIST"
VERIFY_REPORT="$(weft --store "$STORE_REL" verify --full)"
grep -F -q "ok=true" <<<"$VERIFY_REPORT"

log "tamper: modify a payload, rebuild the manifest, compare output hash"
cp -a "$EXTRACT/." "$TAMPER/"
PAYLOAD="$(cd "$TAMPER" && find receipts -type f | LC_ALL=C sort | sed -n '1p')"
python3 - "$TAMPER/$PAYLOAD" <<'PY'
import sys
path = sys.argv[1]
with open(path, "r+b") as handle:
    first = handle.read(1)
    handle.seek(0)
    handle.write(bytes([first[0] ^ 0xff]))
PY
TAMPER_DIGEST_JSON="$(helper create --root "$TAMPER_REL" --manifest "$TAMPER_REL/receipt-manifest.wcb" --generated-at 2026-11-06T00:00:00Z --stamp "$STAMP" | tail -1)"
TAMPER_MANIFEST_SHA="$(printf '%s' "$TAMPER_DIGEST_JSON" | json_get manifest_sha256)"
[ "$TAMPER_MANIFEST_SHA" != "$MANIFEST_SHA" ]
set +e
TAMPER_VERIFY="$("$ROOT/scripts/receipt-bundle-court.sh" verify --root "$TAMPER_REL" --public-key "$PUBLIC_KEY" 2>&1)"
TAMPER_RC=$?
set -e
[ "$TAMPER_RC" -ne 0 ]
grep -E -q "sha256|does not verify|unlisted" <<<"$TAMPER_VERIFY"

weft --store "$TAMPER_STORE_REL" init >/dev/null
weft --store "$TAMPER_STORE_REL" create /bundle.manifest --file "$TAMPER_REL/receipt-manifest.wcb" >/dev/null
TAMPER_STAT_JSON="$(weft --store "$TAMPER_STORE_REL" --json stat /bundle.manifest)"
TAMPER_CONTENT_ID="$(printf '%s' "$TAMPER_STAT_JSON" | json_get content_id)"
[ "$TAMPER_CONTENT_ID" != "$CONTENT_ID" ]

python3 - "$STAMP" "$BUNDLE" "$OBJECT_ID" "$SNAPSHOT" "$PROVENANCE" "$RELATION" \
  "$CONTENT_ID" "$MANIFEST_SHA" "$MANIFEST_CONTENT_ROOT" "$FINGERPRINT" \
  "$TAMPER_MANIFEST_SHA" "$TAMPER_CONTENT_ID" "$SIGNATURE" <<'PY'
import json, sys
(stamp, bundle, object_id, snapshot, provenance, relation, content_id,
 manifest_sha, content_root, fingerprint, tamper_sha, tamper_content,
 signature) = sys.argv[1:]
print(json.dumps({
    "schema": "weftfs.signed-bundle-linkage/1",
    "stamp": stamp,
    "bundle": bundle,
    "object_id": object_id,
    "snapshot_id": snapshot,
    "provenance_id": provenance,
    "relation_id": relation,
    "output_content_id": content_id,
    "manifest_sha256": manifest_sha,
    "manifest_content_root": content_root,
    "signature_fingerprint": fingerprint,
    "signature_hex": signature,
    "tamper_manifest_sha256": tamper_sha,
    "tamper_output_content_id": tamper_content,
    "provenance_show_resolved": True,
    "relation_resolved": True,
    "tamper_detected": True,
}, sort_keys=True))
PY
