#!/usr/bin/env bash
# Q6 — signed receipt-bundle creation and verification benchmark + rejection
# court. Runs on the host (it calls Docker for every build, signature, store,
# and Go cross-check). It never touches a store or compiler directly.
#
# Produces:
#   bench/receipts/signed-bundles-<UTC>.json
#   bench/receipts/signed-bundles-<UTC>.log
#
# Usage: bench/scripts/signed_bundle_bench.sh
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RECEIPT="bench/receipts/signed-bundles-$STAMP.json"
LOG_FINAL="bench/receipts/signed-bundles-$STAMP.log"
CACHE="$ROOT/.docker-cache"
mkdir -p "$CACHE" bench/bundles bench/receipts
# Keep the log outside the repository until the end so packaging sees clean
# worktrees. It is copied into bench/receipts after all measurements.
LOG_TMP="$CACHE/signed-bench-$STAMP.log"
: > "$LOG_TMP"
START_REVISION="$(git rev-parse HEAD)"
START_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
START_DIRTY="$(git status --porcelain | wc -l | tr -d ' ')"
METRICS="$CACHE/signed-bench-$STAMP.metrics.tsv"
CASES="$CACHE/signed-bench-$STAMP.cases.tsv"
: > "$METRICS"
: > "$CASES"

# Deterministic benchmark-only keys. These are not secrets and are not an
# identity; a real deployment supplies --sign-key from its own secret store.
KEY="$CACHE/signed-bench-key.hex"
OTHER_KEY="$CACHE/signed-bench-other.hex"
umask 077
printf '%s\n' "$(printf '44%.0s' {1..32})" > "$KEY"
printf '%s\n' "$(printf '55%.0s' {1..32})" > "$OTHER_KEY"
chmod 600 "$KEY" "$OTHER_KEY"

now_ms() { printf '%s' "$(( $(date +%s%N) / 1000000 ))"; }
metric() { printf '%s\t%s\n' "$1" "$2" >> "$METRICS"; }
case_row() { printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$CASES"; }
log() { printf '%s\n' "$*" | tee -a "$LOG_TMP" >&2; }
first_json_line() { sed -n '/^{/p' | sed -n '1p'; }
json_get() { python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"; }

log "Q6 signed-bundle bench $STAMP"
log "revision $START_REVISION"
log "worktree changed files at start: $START_DIRTY"
log "bundle payload files: $(find bench/receipts -maxdepth 1 -type f | wc -l) receipts + corpus"

# ---- signed package -------------------------------------------------------
T0="$(now_ms)"
if ! SIGNED_OUT="$(scripts/package-receipts.sh --sign-key "$KEY" 2>&1)"; then
  printf '%s\n' "$SIGNED_OUT" >> "$LOG_TMP"
  echo "signed packaging failed" >&2
  exit 1
fi
T1="$(now_ms)"
printf '%s\n' "$SIGNED_OUT" >> "$LOG_TMP"
SIGNED_MS=$((T1 - T0))
SIGNED_BUNDLE="$(sed -n 's/^bundle: //p' <<<"$SIGNED_OUT" | sed -n '1p')"
SIGNED_SHA="$(sed -n 's/^sha256: //p' <<<"$SIGNED_OUT" | sed -n '1p')"
SIGNED_BYTES="$(stat -c %s "$SIGNED_BUNDLE")"
PUBLIC_KEY="$(sed -n 's/^public_key: //p' <<<"$SIGNED_OUT" | sed -n '1p')"
FINGERPRINT="$(sed -n 's/^fingerprint: //p' <<<"$SIGNED_OUT" | sed -n '1p')"
CREATE_JSON="$(first_json_line <<<"$SIGNED_OUT")"
FILES="$(json_get files <<<"$CREATE_JSON")"
REVISIONS="$(json_get revisions <<<"$CREATE_JSON")"
MANIFEST_SHA="$(json_get manifest_sha256 <<<"$CREATE_JSON")"
MANIFEST_CONTENT_ROOT="$(json_get manifest_content_root <<<"$CREATE_JSON")"
SIGNATURE_HEX="$(json_get signature <<<"$CREATE_JSON")"
TAR_ENTRIES="$(tar --zstd -tf "$SIGNED_BUNDLE" | wc -l | tr -d ' ')"

metric package_signed_ms "$SIGNED_MS"
metric signed_bundle "$SIGNED_BUNDLE"
metric signed_bundle_sha256 "$SIGNED_SHA"
metric signed_bundle_bytes "$SIGNED_BYTES"
metric manifest_files "$FILES"
metric manifest_revisions "$REVISIONS"
metric manifest_sha256 "$MANIFEST_SHA"
metric manifest_content_root "$MANIFEST_CONTENT_ROOT"
metric tar_entries "$TAR_ENTRIES"
metric public_key "$PUBLIC_KEY"
metric fingerprint "$FINGERPRINT"
metric signature_raw_bytes 64
SIGNATURE_FILE_BYTES="$(tar --zstd -xOf "$SIGNED_BUNDLE" ./signature | wc -c | tr -d ' ')"
PUBLIC_KEY_FILE_BYTES="$(tar --zstd -xOf "$SIGNED_BUNDLE" ./public_key | wc -c | tr -d ' ')"
MANIFEST_FILE_BYTES="$(tar --zstd -xOf "$SIGNED_BUNDLE" ./receipt-manifest.wcb | wc -c | tr -d ' ')"
metric signature_file_bytes "$SIGNATURE_FILE_BYTES"
metric public_key_file_bytes "$PUBLIC_KEY_FILE_BYTES"
metric manifest_file_bytes "$MANIFEST_FILE_BYTES"
log "signed: $SIGNED_BUNDLE ($SIGNED_BYTES bytes, ${SIGNED_MS} ms, $FILES files)"

# ---- signed verify --------------------------------------------------------
T0="$(now_ms)"
if ! SIGNED_VERIFY="$(scripts/verify-receipts.sh "$SIGNED_BUNDLE" --public-key "$PUBLIC_KEY" --repo-root . 2>&1)"; then
  printf '%s\n' "$SIGNED_VERIFY" >> "$LOG_TMP"
  echo "signed verification failed" >&2
  exit 1
fi
T1="$(now_ms)"
printf '%s\n' "$SIGNED_VERIFY" >> "$LOG_TMP"
grep -F -q "SIGNATURE OK $FINGERPRINT" <<<"$SIGNED_VERIFY"
metric verify_signed_ms "$((T1 - T0))"
case_row pass signature_ok "$FINGERPRINT"
log "pinned-key verify: $(grep -F 'SIGNATURE OK' <<<"$SIGNED_VERIFY" | sed -n '1p')"

# ---- unsigned package + extra bytes --------------------------------------
T0="$(now_ms)"
if ! UNSIGNED_OUT="$(scripts/package-receipts.sh 2>&1)"; then
  printf '%s\n' "$UNSIGNED_OUT" >> "$LOG_TMP"
  echo "unsigned packaging failed" >&2
  exit 1
fi
T1="$(now_ms)"
printf '%s\n' "$UNSIGNED_OUT" >> "$LOG_TMP"
UNSIGNED_MS=$((T1 - T0))
UNSIGNED_BUNDLE="$(sed -n 's/^bundle: //p' <<<"$UNSIGNED_OUT" | sed -n '1p')"
UNSIGNED_BYTES="$(stat -c %s "$UNSIGNED_BUNDLE")"
UNSIGNED_FILES="$(json_get files <<<"$(first_json_line <<<"$UNSIGNED_OUT")")"
metric package_unsigned_ms "$UNSIGNED_MS"
metric unsigned_bundle "$UNSIGNED_BUNDLE"
metric unsigned_bundle_bytes "$UNSIGNED_BYTES"
metric unsigned_manifest_files "$UNSIGNED_FILES"
metric unsigned_marker_bytes "$(tar --zstd -xOf "$UNSIGNED_BUNDLE" ./UNSIGNED.txt | wc -c | tr -d ' ')"
metric observed_compressed_size_delta "$((SIGNED_BYTES - UNSIGNED_BYTES))"
metric observed_compressed_size_delta_note "signed_bytes_minus_unsigned_bytes; payload timestamps and UNSIGNED.txt differ between the two runs, so this is not attributable to signing"
metric raw_metadata_added_bytes "$((MANIFEST_FILE_BYTES + SIGNATURE_FILE_BYTES + PUBLIC_KEY_FILE_BYTES - $(tar --zstd -xOf "$UNSIGNED_BUNDLE" ./UNSIGNED.txt | wc -c | tr -d ' ')))"
metric unsigned_marker_bytes "$(tar --zstd -xOf "$UNSIGNED_BUNDLE" ./UNSIGNED.txt | wc -c | tr -d ' ')"
log "unsigned: $UNSIGNED_BUNDLE ($UNSIGNED_BYTES bytes, ${UNSIGNED_MS} ms)"

T0="$(now_ms)"
if ! UNSIGNED_VERIFY="$(scripts/verify-receipts.sh "$UNSIGNED_BUNDLE" --repo-root . 2>&1)"; then
  printf '%s\n' "$UNSIGNED_VERIFY" >> "$LOG_TMP"
  echo "unsigned verification failed" >&2
  exit 1
fi
T1="$(now_ms)"
printf '%s\n' "$UNSIGNED_VERIFY" >> "$LOG_TMP"
grep -F -q "signature: absent (unsigned bundle)" <<<"$UNSIGNED_VERIFY"
metric verify_unsigned_ms "$((T1 - T0))"
case_row pass unsigned_detected "signature: absent (unsigned bundle)"

# ---- rejected cases ------------------------------------------------------
run_fail_case() {
  local name="$1" expected="$2"
  shift 2
  set +e
  local out rc
  out="$("$@" 2>&1)"
  rc=$?
  set -e
  if [ "$rc" -eq 0 ]; then
    case_row FAIL "$name" "unexpected success"
    echo "case $name unexpectedly passed" >&2
    return 1
  fi
  if ! grep -E -q -- "$expected" <<<"$out"; then
    case_row FAIL "$name" "expected '$expected'; got: $(tail -1 <<<"$out")"
    echo "case $name failed with wrong reason: $out" >&2
    return 1
  fi
  printf '%s\n' "--- $name ---" >> "$LOG_TMP"
  printf '%s\n' "$out" >> "$LOG_TMP"
  case_row pass "$name" "$expected"
}

OTHER_PUB="$(scripts/receipt-bundle-court.sh derive --sign-key ".docker-cache/$(basename "$OTHER_KEY")" 2>>"$LOG_TMP" | tail -1 | json_get public_key)"
run_fail_case wrong_key "public_key file does not match the requested key" \
  scripts/verify-receipts.sh "$SIGNED_BUNDLE" --public-key "$OTHER_PUB"
run_fail_case unsigned_with_key "missing signature file; bundle is unsigned" \
  scripts/verify-receipts.sh "$UNSIGNED_BUNDLE" --public-key "$PUBLIC_KEY"

# Extract a fresh copy for tamper cases.
TAMPER="$CACHE/signed-bench-tamper-$STAMP"
rm -rf "$TAMPER"
mkdir -p "$TAMPER"
tar --zstd -xf "$SIGNED_BUNDLE" -C "$TAMPER"
TAMPER_REL="${TAMPER#"$ROOT/"}"
PAYLOAD="$(cd "$TAMPER" && find receipts -type f | LC_ALL=C sort | sed -n '1p')"
python3 - "$TAMPER/$PAYLOAD" <<'PY'
import sys
path = sys.argv[1]
with open(path, "r+b") as handle:
    first = handle.read(1)
    handle.seek(0)
    handle.write(bytes([first[0] ^ 0xff]))
PY

run_fail_case tampered_file_sha "sha256" \
  scripts/receipt-bundle-court.sh verify --root "$TAMPER_REL" --public-key "$PUBLIC_KEY"

# Rebuild CHECKSUMS so the tampered tarball reaches the canonical manifest
# check rather than stopping at the outer checksum list.
( cd "$TAMPER" && find . -type f ! -name CHECKSUMS.sha256 -print0 \
    | LC_ALL=C sort -z | xargs -0 -r sha256sum > CHECKSUMS.sha256 )
TAMPERED_TAR="$CACHE/signed-bench-tampered-$STAMP.tar.zst"
tar --zstd -cf "$TAMPERED_TAR" -C "$TAMPER" .
run_fail_case tampered_file_manifest "sha256" \
  scripts/verify-receipts.sh "$TAMPERED_TAR" --public-key "$PUBLIC_KEY"

# Rebuild a canonical (unsigned) manifest over the tampered tree; the old
# signature must now fail.
TAMPER_CREATE="$(scripts/receipt-bundle-court.sh create --root "$TAMPER_REL" \
  --manifest "$TAMPER_REL/receipt-manifest.wcb" \
  --generated-at 2026-11-06T00:00:00Z --stamp "$STAMP" 2>>"$LOG_TMP" | tail -1)"
TAMPER_SHA="$(json_get manifest_sha256 <<<"$TAMPER_CREATE")"
if [ "$TAMPER_SHA" = "$MANIFEST_SHA" ]; then
  echo "tampered manifest digest did not change" >&2
  exit 1
fi
metric tampered_manifest_sha256 "$TAMPER_SHA"
run_fail_case tampered_manifest_signature "signature does not verify under" \
  scripts/receipt-bundle-court.sh verify --root "$TAMPER_REL" --public-key "$PUBLIC_KEY"

# Missing signature on an otherwise complete bundle, through the full script.
MISSING="$CACHE/signed-bench-missing-$STAMP"
rm -rf "$MISSING"
mkdir -p "$MISSING"
tar --zstd -xf "$SIGNED_BUNDLE" -C "$MISSING"
rm -f "$MISSING/signature"
( cd "$MISSING" && find . -type f ! -name CHECKSUMS.sha256 -print0 \
    | LC_ALL=C sort -z | xargs -0 -r sha256sum > CHECKSUMS.sha256 )
MISSING_TAR="$CACHE/signed-bench-missing-$STAMP.tar.zst"
tar --zstd -cf "$MISSING_TAR" -C "$MISSING" .
run_fail_case missing_signature "missing signature file" \
  scripts/verify-receipts.sh "$MISSING_TAR" --public-key "$PUBLIC_KEY"

# Unlisted payload file with a consistent CHECKSUMS list.
UNLISTED="$CACHE/signed-bench-unlisted-$STAMP"
rm -rf "$UNLISTED"
mkdir -p "$UNLISTED"
tar --zstd -xf "$SIGNED_BUNDLE" -C "$UNLISTED"
printf '{}\n' > "$UNLISTED/receipts/extra.json"
( cd "$UNLISTED" && find . -type f ! -name CHECKSUMS.sha256 -print0 \
    | LC_ALL=C sort -z | xargs -0 -r sha256sum > CHECKSUMS.sha256 )
UNLISTED_REL="${UNLISTED#"$ROOT/"}"
run_fail_case unlisted_file "unlisted payload file" \
  scripts/receipt-bundle-court.sh verify --root "$UNLISTED_REL" --public-key "$PUBLIC_KEY"

# Non-canonical manifest bytes.
CANON="$CACHE/signed-bench-canonical-$STAMP"
rm -rf "$CANON"
mkdir -p "$CANON"
tar --zstd -xf "$SIGNED_BUNDLE" -C "$CANON"
python3 - "$CANON/receipt-manifest.wcb" <<'PY'
import sys
path = sys.argv[1]
with open(path, "r+b") as handle:
    data = bytearray(handle.read())
    data[len(data) // 2] ^= 0xff
    handle.seek(0)
    handle.write(data)
PY
CANON_REL="${CANON#"$ROOT/"}"
run_fail_case non_canonical_manifest "not in canonical form|Malformed|canonical" \
  scripts/receipt-bundle-court.sh verify --root "$CANON_REL"

# ---- Go cross-check ------------------------------------------------------
EXTRACT="$CACHE/signed-bench-extract-$STAMP"
rm -rf "$EXTRACT"
mkdir -p "$EXTRACT"
tar --zstd -xf "$SIGNED_BUNDLE" -C "$EXTRACT"
EXTRACT_REL="${EXTRACT#"$ROOT/"}"
DIGEST_JSON="$(scripts/receipt-bundle-court.sh digest --root "$EXTRACT_REL" 2>>"$LOG_TMP" | tail -1)"
GO_DIGEST="$(json_get manifest_sha256 <<<"$DIGEST_JSON")"
GO_PUB="$(tr -d '\n' < "$EXTRACT/public_key")"
GO_SIG="$(tr -d '\n' < "$EXTRACT/signature")"
if GO_OUT="$(scripts/docker-court.sh go "cd /work/WeftCodec-Go && go run ./cmd/verify-ed25519 $GO_PUB $GO_DIGEST $GO_SIG" 2>&1)"; then
  grep -F -q "VERIFY OK" <<<"$GO_OUT"
  case_row pass go_signature_ok "VERIFY OK"
  metric go_verified true
else
  printf '%s\n' "$GO_OUT" >> "$LOG_TMP"
  case_row FAIL go_signature_ok "$(tail -1 <<<"$GO_OUT")"
  exit 1
fi
set +e
GO_TAMPER="$(scripts/docker-court.sh go "cd /work/WeftCodec-Go && go run ./cmd/verify-ed25519 $GO_PUB $(printf '0%.0s' {1..64}) $GO_SIG" 2>&1)"
GO_TAMPER_RC=$?
set -e
if [ "$GO_TAMPER_RC" -eq 0 ]; then
  case_row FAIL go_tamper_reject "unexpected success"
  exit 1
fi
grep -F -q "VERIFY FAIL" <<<"$GO_TAMPER"
printf '%s\n' "$GO_TAMPER" >> "$LOG_TMP"
case_row pass go_tamper_reject "VERIFY FAIL"

# ---- store-backed provenance linkage ------------------------------------
log "provenance linkage demo"
if ! LINKAGE="$(scripts/demo-signed-bundle.sh "$SIGNED_BUNDLE" 2>>"$LOG_TMP" | tail -1)"; then
  echo "linkage demo failed" >&2
  exit 1
fi
grep -F -q '"provenance_show_resolved": true' <<<"$LINKAGE"
grep -F -q '"tamper_detected": true' <<<"$LINKAGE"
printf '%s\n' "$LINKAGE" >> "$LOG_TMP"
metric provenance_id "$(json_get provenance_id <<<"$LINKAGE")"
metric snapshot_id "$(json_get snapshot_id <<<"$LINKAGE")"
metric output_content_id "$(json_get output_content_id <<<"$LINKAGE")"
metric tamper_output_content_id "$(json_get tamper_output_content_id <<<"$LINKAGE")"
case_row pass provenance_show_resolved "derived_from + snapshot"

# ---- receipt -------------------------------------------------------------
KERNEL="$(uname -r)"
RUST_IMAGE="$(docker image inspect --format '{{.Id}}' weft/rust-court:1 2>/dev/null || echo unknown)"
GO_IMAGE="$(docker image inspect --format '{{.Id}}' weft/go-court:1 2>/dev/null || echo unknown)"
END_REVISION="$(git rev-parse HEAD)"
DIRTY_LINES="$(git status --porcelain | wc -l | tr -d ' ')"

python3 - "$RECEIPT" "$METRICS" "$CASES" "$STAMP" "$START_REVISION" \
  "$START_BRANCH" "$START_DIRTY" "$END_REVISION" "$DIRTY_LINES" "$KERNEL" \
  "$RUST_IMAGE" "$GO_IMAGE" <<'PY'
import json, sys
(receipt_path, metrics_path, cases_path, stamp, revision, branch, start_dirty,
 end_revision, dirty, kernel, rust_image, go_image) = sys.argv[1:]
metrics = {}
with open(metrics_path, encoding="utf-8") as handle:
    for line in handle:
        key, _, value = line.rstrip("\n").partition("\t")
        if key:
            metrics[key] = value
cases = []
with open(cases_path, encoding="utf-8") as handle:
    for line in handle:
        result, _, rest = line.rstrip("\n").partition("\t")
        name, _, detail = rest.partition("\t")
        if result:
            cases.append({"result": result, "case": name, "detail": detail})
receipt = {
    "schema": "weftfs.signed-bundles/1",
    "stamp": stamp,
    "revision": revision,
    "revision_at_end": end_revision,
    "branch": branch,
    "worktree_dirty_at_start": int(start_dirty) > 0,
    "worktree_changed_files_at_start": int(start_dirty),
    "worktree_dirty_before_output": int(dirty) > 0,
    "worktree_changed_files_before_output": int(dirty),
    "court": {
        "rust_image": rust_image,
        "go_image": go_image,
        "kernel": kernel,
        "limits": {
            "memory": "3g",
            "memory_swap": "3g",
            "pids": 1024,
            "cpus": 4,
            "tmpfs_tmp": "1g",
            "no_new_privileges": True,
            "cap_drop": "ALL",
        },
    },
    "hosts": 1,
    "label": (
        "single host; deterministic benchmark-only signing keys; real receipt "
        "bundle built from the committed bench/receipts and conformance corpus; "
        "process-level signature court, not physical power loss and not a "
        "statement about measurement truth"
    ),
    "signing": {
        "algorithm": "Ed25519",
        "sign_input": 'H("weft/receipt-bundle/v1", WCB(manifest))',
        "public_key": metrics.get("public_key"),
        "fingerprint": metrics.get("fingerprint"),
        "signature_bytes": int(metrics.get("signature_raw_bytes", "0")),
        "key_is_a_real_identity": False,
    },
    "metrics": metrics,
    "cases": cases,
    "all_cases_pass": all(case["result"] == "pass" for case in cases),
}
with open(receipt_path, "w", encoding="utf-8") as handle:
    json.dump(receipt, handle, indent=2, sort_keys=True)
    handle.write("\n")
print(receipt_path)
PY

log "receipt: $RECEIPT"
log "log: $LOG_FINAL"
cp "$LOG_TMP" "$LOG_FINAL"
