#!/usr/bin/env bash
# GCS court (subphase R14): a real GCS-compatible emulator, the in-process GCS
# unit tests, and an end-to-end court (same content root, commit sequence,
# reopen, clean full verify, crash proxy).
#
# Which emulator, and why
#   `fake-gcs-server` (fsouza/fake-gcs-server) is a single static Linux binary
#   published on GitHub. It speaks the GCS JSON API including
#   `ifGenerationMatch` preconditions, `alt=media` downloads with `Range`, and
#   bucket/object insert. This court downloads a pinned release into
#   `.docker-cache/gcs`, verifies the release tarball's sha256 against the
#   published checksum, and runs the binary *inside* the OOM-protected rust
#   court on loopback with the memory backend. No Docker network and no
#   privileged capability are needed.
#
#   If the binary cannot be obtained (no network, wrong architecture, checksum
#   mismatch), the court writes an `emulator_unavailable` receipt naming the
#   exact command and error. It never substitutes a different emulator and
#   never claims a run it did not do.
#
# Every container has the same hard limits as `scripts/docker-court.sh`:
# memory 3g, swap 3g, pids 1024, cpus 4, bounded tmpfs, no-new-privileges,
# cap-drop=ALL.
#
# Label: a single GCS-compatible emulator on loopback with a memory backend.
# This proves protocol behaviour and generation-precondition safety. It is not
# a cloud, multi-node, fleet, device-cache, media, power-loss, or production
# claim.
#
# Usage:
#   bench/scripts/gcs_court.sh
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"

stamp="${WEFT_GCS_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

gcs_release_tag="v1.56.1"
gcs_release_version="1.56.1"
gcs_tarball_name="fake-gcs-server_${gcs_release_version}_Linux_amd64.tar.gz"
gcs_tarball_sha256="0f2f9f6884417acd5cc27126a9aa237ccc627372a4aa5008b399641de6db9469"
gcs_url="https://github.com/fsouza/fake-gcs-server/releases/download/${gcs_release_tag}/${gcs_tarball_name}"
gcs_dir="$repo_root/.docker-cache/gcs"
gcs_bin="$gcs_dir/fake-gcs-server"
court_receipt="bench/receipts/gcs-court-${stamp}.json"

write_unavailable() {
  local status="$1"
  local command="$2"
  local error="$3"
  cat > "$court_receipt" <<JSON
{
  "schema": "weft-gcs-court-v1",
  "status": "emulator_unavailable",
  "generated_utc": "$stamp",
  "emulator": {
    "product": "fake-gcs-server (fsouza/fake-gcs-server)",
    "release": "$gcs_release_tag",
    "tarball": "$gcs_tarball_name",
    "tarball_sha256": "$gcs_tarball_sha256",
    "url": "$gcs_url",
    "requested_status": "$status"
  },
  "command": $(printf '%s' "$command" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/^/"/' -e 's/$/"/'),
  "error": $(printf '%s' "$error" | head -c 2000 | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/^/"/' -e 's/$/"/'),
  "unit_tests": {
    "command": "cargo test --test gcs_backend (in-process fake GCS service)",
    "note": "The in-process fake GCS service tests do not need an emulator; run them with `cargo test --test gcs_backend` in the rust court."
  },
  "label": "No external GCS-compatible emulator ran. Nothing in this receipt is a GCS, cloud, multi-node, media, power-loss, or production claim."
}
JSON
  echo "gcs_court: emulator unavailable ($status); receipt $court_receipt" >&2
  echo "gcs_court: command: $command" >&2
  echo "gcs_court: error: $error" >&2
}

if [[ "$(uname -m)" != "x86_64" ]]; then
  write_unavailable "unsupported_arch" \
    "$gcs_url" \
    "the verified fake-gcs-server release is linux-amd64 only; host is $(uname -m)"
  exit 0
fi

if ! command -v curl >/dev/null 2>&1; then
  write_unavailable "no_downloader" "$gcs_url" "curl is not installed on the host"
  exit 0
fi

mkdir -p "$gcs_dir"
obtain_error=""
obtain_command="curl -fL --retry 3 -o $gcs_dir/${gcs_tarball_name}.part $gcs_url && mv $gcs_dir/${gcs_tarball_name}.part $gcs_dir/${gcs_tarball_name} && sha256sum $gcs_dir/${gcs_tarball_name} && tar -xzf $gcs_dir/${gcs_tarball_name} -C $gcs_dir fake-gcs-server"
if [[ ! -s "$gcs_dir/$gcs_tarball_name" ]] || [[ ! -x "$gcs_bin" ]]; then
  obtain_error="$( { curl -fL --retry 3 -o "$gcs_dir/${gcs_tarball_name}.part" "$gcs_url" \
      && mv "$gcs_dir/${gcs_tarball_name}.part" "$gcs_dir/$gcs_tarball_name"; } 2>&1 )" || {
    write_unavailable "download_failed" "$obtain_command" "${obtain_error:-curl failed}"
    exit 0
  }
  actual_sha256="$(sha256sum "$gcs_dir/$gcs_tarball_name" | awk '{print $1}')"
  if [[ "$actual_sha256" != "$gcs_tarball_sha256" ]]; then
    write_unavailable "checksum_mismatch" "$obtain_command" \
      "expected sha256 $gcs_tarball_sha256, got $actual_sha256"
    exit 0
  fi
  obtain_error="$( { tar -xzf "$gcs_dir/$gcs_tarball_name" -C "$gcs_dir" fake-gcs-server; } 2>&1 )" || {
    write_unavailable "extract_failed" "$obtain_command" "${obtain_error:-tar failed}"
    exit 0
  }
fi
if [[ ! -x "$gcs_bin" ]]; then
  write_unavailable "missing_binary" "$obtain_command" "no executable at $gcs_bin"
  exit 0
fi

export WEFT_GCS_BIN="/work/.docker-cache/gcs/fake-gcs-server"
export WEFT_GCS_TEST_ENDPOINT="${WEFT_GCS_TEST_ENDPOINT:-http://127.0.0.1:4443}"
export WEFT_GCS_TEST_BUCKET="weft-gcs-court-${suffix}"
export WEFT_GCS_PROJECT="${WEFT_GCS_PROJECT:-weft-court}"
export WEFT_GCS_ANONYMOUS=1
export WEFT_GCS_LOCK_LEASE_SECS="${WEFT_GCS_LOCK_LEASE_SECS:-2}"
# The fencing suite needs a lease a court can expire; the general court uses a
# short but longer-lived one for the crash proxy. Same split as the S3 court.
export WEFT_GCS_FENCE_LEASE_SECS="${WEFT_GCS_FENCE_LEASE_SECS:-3}"
# Optional real service-account key. The host path is copied into the gitignored
# court cache and passed to the container as a /work path; the key never enters
# the repository, a receipt, or a log.
auth_status="no_credentials"
auth_probe=""
if [[ -n "${WEFT_GCS_SA_KEY:-}" ]]; then
  if [[ ! -f "$WEFT_GCS_SA_KEY" ]]; then
    echo "gcs_court: WEFT_GCS_SA_KEY=$WEFT_GCS_SA_KEY does not exist" >&2
    exit 64
  fi
  mkdir -p "$repo_root/.docker-cache/gcs-sa"
  cp "$WEFT_GCS_SA_KEY" "$repo_root/.docker-cache/gcs-sa/key.json"
  chmod 600 "$repo_root/.docker-cache/gcs-sa/key.json"
  export WEFT_GCS_CREDENTIALS="/work/.docker-cache/gcs-sa/key.json"
  export WEFT_GCS_LIVE_PROBE=1
  auth_status="key_file_present"
else
  unset WEFT_GCS_CREDENTIALS WEFT_GCS_LIVE_PROBE
fi
export WEFT_GCS_RECEIPT="bench/receipts/gcs-${stamp}.json"
export WEFT_GCS_TESTS_LOG="bench/receipts/gcs-${stamp}.tests.log"
export WEFT_GCS_FENCING_LOG="bench/receipts/gcs-${stamp}.fencing.log"
export WEFT_GCS_STAMP="$stamp"
export WEFT_GCS_SERVER_LABEL="fake-gcs-server"
export WEFT_GCS_SERVER_VERSION="$gcs_release_version"
export WEFT_GCS_SERVER_SHA256="$gcs_tarball_sha256"

echo "gcs_court: fake-gcs-server $gcs_release_tag sha256=$gcs_tarball_sha256 endpoint=$WEFT_GCS_TEST_ENDPOINT bucket=$WEFT_GCS_TEST_BUCKET" >&2

inner_status=0
if ! scripts/docker-court.sh rust "cd /work && bash bench/scripts/gcs_inner_court.sh"; then
  inner_status=1
fi

auth_log="bench/receipts/gcs-${stamp}.tests.auth.log"
auth_summary="${auth_log%.log}.txt"
auth_probe_log="bench/receipts/gcs-${stamp}.tests.auth-probe.log"
if [[ -f "$auth_probe_log" ]]; then
  auth_probe="$(head -c 2000 "$auth_probe_log" | tr '\n' ' ')"
  auth_status="live_probe_ran"
fi
if [[ -n "$auth_probe" ]]; then
  auth_probe_escaped="$(printf '%s' "$auth_probe" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"
  auth_probe_json="\"$auth_probe_escaped\""
else
  auth_probe_json="null"
fi

cat > "$court_receipt" <<JSON
{
  "schema": "weft-gcs-court-v1",
  "status": "$(if [[ "$inner_status" == 0 ]]; then echo ok; else echo failed; fi)",
  "generated_utc": "$stamp",
  "emulator": {
    "product": "fake-gcs-server (fsouza/fake-gcs-server)",
    "release": "$gcs_release_tag",
    "tarball": "$gcs_tarball_name",
    "tarball_sha256": "$gcs_tarball_sha256",
    "url": "$gcs_url",
    "backend": "memory",
    "started_by": "bench/scripts/gcs_inner_court.sh inside the rust court",
    "note": "The emulator runs on loopback in the same container as the test client. Its data backend is memory, so this proves protocol behaviour, not media durability."
  },
  "container": {
    "kind": "rust",
    "memory": "$WEFT_MEMORY",
    "memory_swap": "$WEFT_MEMORY",
    "pids": "$WEFT_PIDS",
    "cpus": "$WEFT_CPUS",
    "cap_drop": "ALL",
    "no_new_privileges": true,
    "tmpfs": "/tmp 1g"
  },
  "endpoint": "$WEFT_GCS_TEST_ENDPOINT",
  "bucket": "$WEFT_GCS_TEST_BUCKET",
  "tests": {
    "command": "cargo test --test gcs_backend -- --test-threads=1",
    "log": "${WEFT_GCS_TESTS_LOG}",
    "summary": "${WEFT_GCS_TESTS_LOG%.log}.txt"
  },
  "auth": {
    "status": "$auth_status",
    "offline_command": "cargo test --test service_account -- --test-threads=1",
    "offline_log": "${auth_log}",
    "offline_summary": "${auth_summary}",
    "live_probe_log": "${auth_probe_log}",
    "live_probe": $auth_probe_json,
    "note": "Service-account RS256 signing is verified offline against the published RFC 7515 A.2 vector and the OAuth2 exchange/cache against an in-process endpoint. A status of no_credentials means no live exchange was attempted: no service-account key exists in this environment, so this receipt makes no live-GCS authentication claim. Set WEFT_GCS_SA_KEY to a key file to run the live probe."
  },
  "fencing": {
    "command": "cargo test --test gcs_fencing -- --test-threads=1 --nocapture",
    "log": "${WEFT_GCS_FENCING_LOG}",
    "summary": "${WEFT_GCS_FENCING_LOG%.log}.txt",
    "lease_secs": "$WEFT_GCS_FENCE_LEASE_SECS",
    "shared_bodies": "tests/support/fencing.rs (also run by scripts/s3_court.sh on MinIO and scripts/ceph_court.sh on Ceph RGW)",
    "note": "Generation-precondition fencing: takeover, fence-before-head, renewal, own-deadline stop, and a conditional tombstone release."
  },
  "court": {
    "command": "cargo run --release --quiet --example gcs_court -- --out ${WEFT_GCS_RECEIPT}",
    "receipt": "${WEFT_GCS_RECEIPT}"
  },
  "label": "Single-host GCS-compatible emulator on loopback in the OOM-protected Docker court with a memory backend. Not a cloud, multi-node, fleet, device-cache, media, power-loss, or production claim."
}
JSON

echo "gcs_court: court receipt $court_receipt" >&2
if [[ "$inner_status" != 0 ]]; then
  echo "gcs_court: the GCS court reported failures" >&2
  exit 1
fi
