#!/usr/bin/env bash
# Azure Blob court (subphase R21): a real Azurite emulator, the in-process
# Azure backend tests, the shared fenced-writer invariants, and an end-to-end
# court (same content root, commit sequence, reopen, clean full verify, crash
# proxy).
#
# Which emulator, and why
#   Azurite is Microsoft's official Azure Storage emulator. It is only
#   distributed as an image on `mcr.microsoft.com` (there is no single static
#   Linux binary on Docker Hub), so — exactly like the MinIO court's use of the
#   official release binary — this script records the *image digest* it ran and
#   never claims to run a Docker Hub image. The emulator runs as a sibling
#   container on a private user-defined network with the same hard limits as
#   every other court: memory 3g, swap 3g, pids 1024, cpus 4, bounded tmpfs,
#   no-new-privileges, cap-drop=ALL.
#
#   The Azurite Blob service implements the Blob REST surface and, importantly,
#   the SharedKey signature and the two conditional forms (`If-None-Match: *`,
#   `If-Match: <ETag>`) this backend depends on. The court therefore proves the
#   request shapes, the signature canonicalisation, create-once behaviour, stale
#   ETag refusal, ranged reads, listing, and the fenced writer protocol against
#   an implementation nobody on this project wrote.
#
#   If the image cannot be obtained (no network, wrong architecture, registry
#   failure) or `docker` is unavailable, the court prints exactly one
#   `azure_court: skip <reason>` line and exits 78. The aggregate verification
#   court records that as `skip`, never as a pass, and it never substitutes a
#   different emulator.
#
# Label: a single Azure Blob emulator on loopback in the OOM-protected Docker
# court with a tmpfs-backed data directory. This proves protocol behaviour,
# signature acceptance, conditional-write safety, and fencing. It is not an
# Azure, cloud, multi-node, fleet, device-cache, media, power-loss, or
# production claim.
#
# Usage:
#   bench/scripts/azure_court.sh
set -Eeuo pipefail

skip() {
  echo "azure_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

azurite_image="${WEFT_AZURE_IMAGE:-mcr.microsoft.com/azure-storage/azurite@sha256:647c63a91102a9d8e8000aab803436e1fc85fbb285e7ce830a82ee5d6661cf37}"
azurite_version="${WEFT_AZURE_VERSION:-3.35.0}"
if ! docker image inspect "$azurite_image" >/dev/null 2>&1; then
  if ! docker pull "$azurite_image" >/dev/null 2>&1; then
    skip "cannot obtain the pinned Azurite image $azurite_image"
  fi
fi
azurite_image_id="$(docker image inspect "$azurite_image" --format '{{.Id}}')"
azurite_digest="$(docker image inspect "$azurite_image" --format '{{if .RepoDigests}}{{index .RepoDigests 0}}{{else}}{{.Id}}{{end}}')"

stamp="${WEFT_AZURE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

azurite_name="weft-azure-azurite-$$"
network="weft-azure-net-$$"
# A Blob container name is 3..=63 lowercase letters/digits/hyphens.
court_container="weft-az-${suffix}"
account="devstoreaccount1"
# The documented Azurite development account key. Not a secret: it is published
# by Microsoft for the emulator only.
account_key="Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="

cleanup() {
  docker rm -f "$azurite_name" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

docker network create "$network" >/dev/null 2>&1 || skip "cannot create the court Docker network $network"

docker run -d --name "$azurite_name" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 \
  --security-opt no-new-privileges \
  --cap-drop=ALL \
  --network "$network" \
  --tmpfs /data:rw,size=1g,nosuid,nodev \
  --entrypoint azurite-blob \
  "$azurite_image" \
  --blobHost 0.0.0.0 --blobPort 10000 --location /data --skipApiVersionCheck \
  >/dev/null

ready=0
for _ in $(seq 1 120); do
  if docker logs "$azurite_name" 2>&1 | grep -q "successfully listens"; then
    ready=1
    break
  fi
  if [[ "$(docker inspect -f '{{.State.Status}}' "$azurite_name" 2>/dev/null || echo gone)" != "running" ]]; then
    echo "azure_court: the Azurite container exited before becoming ready; logs follow" >&2
    docker logs "$azurite_name" >&2 || true
    exit 1
  fi
  sleep 0.25
done
if [[ "$ready" != 1 ]]; then
  echo "azure_court: Azurite never reported a listening Blob service; logs follow" >&2
  docker logs "$azurite_name" >&2 || true
  exit 1
fi

export WEFT_DOCKER_NETWORK="$network"
export WEFT_AZURE_TEST_ENDPOINT="http://${azurite_name}:10000/devstoreaccount1"
export WEFT_AZURE_TEST_CONTAINER="$court_container"
export WEFT_AZURE_ACCOUNT="$account"
export WEFT_AZURE_KEY="$account_key"
# The general suites do many operations between publications; a writer idle
# longer than its lease must reopen (that is the R17 contract), so give them a
# lease long enough not to trip on a slow emulator. The fencing suite
# deliberately uses a short one.
export WEFT_AZURE_LOCK_LEASE_SECS="${WEFT_AZURE_LOCK_LEASE_SECS:-60}"
export WEFT_AZURE_FENCE_LEASE_SECS="${WEFT_AZURE_FENCE_LEASE_SECS:-3}"
export WEFT_AZURE_RECEIPT="bench/receipts/azure-${stamp}.json"
export WEFT_AZURE_TESTS_LOG="bench/receipts/azure-${stamp}.tests.log"
export WEFT_AZURE_FENCING_LOG="bench/receipts/azure-${stamp}.fencing.log"
export WEFT_AZURE_STAMP="$stamp"
export WEFT_AZURE_SERVER_LABEL="Azurite Blob"
export WEFT_AZURE_SERVER_VERSION="$azurite_version"
export WEFT_AZURE_SERVER_DIGEST="$azurite_digest"

echo "azure_court: Azurite $azurite_version image=$azurite_digest endpoint=$WEFT_AZURE_TEST_ENDPOINT container=$court_container" >&2

inner_status=0
if ! scripts/docker-court.sh rust "cd /work && bash bench/scripts/azure_inner_court.sh"; then
  inner_status=1
fi

court_receipt="bench/receipts/azure-court-${stamp}.json"
cat > "$court_receipt" <<JSON
{
  "schema": "weft-azure-court-v1",
  "status": "$(if [[ "$inner_status" == 0 ]]; then echo ok; else echo failed; fi)",
  "generated_utc": "$stamp",
  "emulator": {
    "product": "Azurite Blob service (mcr.microsoft.com/azure-storage/azurite)",
    "release": "$azurite_version",
    "image": "$azurite_image",
    "image_id": "$azurite_image_id",
    "image_digest": "$azurite_digest",
    "backend": "tmpfs /data, memory-backed file layout",
    "started_by": "bench/scripts/azure_court.sh as a sibling container on a private user-defined network",
    "api_version": "2021-12-02",
    "auth": "SharedKey with the published Azurite development account key",
    "note": "Azurite is distributed on mcr.microsoft.com, not Docker Hub. The digest above is the exact image that ran; no Docker Hub image is claimed."
  },
  "container": {
    "kind": "rust",
    "memory": "$WEFT_MEMORY",
    "memory_swap": "$WEFT_MEMORY",
    "pids": "$WEFT_PIDS",
    "cpus": "$WEFT_CPUS",
    "cap_drop": "ALL",
    "no_new_privileges": true,
    "tmpfs": "/tmp 1g (client), /data 1g (emulator)"
  },
  "endpoint": "$WEFT_AZURE_TEST_ENDPOINT",
  "container_name_prefix": "$court_container",
  "account": "$account",
  "tests": {
    "command": "cargo test --test azure_backend -- --test-threads=1 --nocapture",
    "log": "${WEFT_AZURE_TESTS_LOG}",
    "summary": "${WEFT_AZURE_TESTS_LOG%.log}.txt"
  },
  "fencing": {
    "command": "cargo test --test azure_fencing -- --test-threads=1 --nocapture",
    "log": "${WEFT_AZURE_FENCING_LOG}",
    "summary": "${WEFT_AZURE_FENCING_LOG%.log}.txt",
    "lease_secs": "$WEFT_AZURE_FENCE_LEASE_SECS",
    "shared_bodies": "tests/support/fencing.rs (also run by scripts/s3_court.sh on MinIO and bench/scripts/gcs_court.sh on fake-gcs-server)",
    "note": "ETag-precondition fencing: takeover, fence-before-head, renewal, own-deadline stop, and a conditional tombstone release."
  },
  "court": {
    "command": "cargo run --release --quiet --example azure_court -- --out ${WEFT_AZURE_RECEIPT}",
    "receipt": "${WEFT_AZURE_RECEIPT}"
  },
  "label": "Single-host Azurite emulator on loopback in the OOM-protected Docker court with a tmpfs data directory. Not an Azure, cloud, multi-node, fleet, device-cache, media, power-loss, or production claim."
}
JSON

echo "azure_court: court receipt $court_receipt" >&2
if [[ "$inner_status" != 0 ]]; then
  echo "azure_court: the Azure court reported failures" >&2
  exit 1
fi
