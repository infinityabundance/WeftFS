#!/usr/bin/env bash
# Azure MW2 mixed-language court (subphase X13/N14).
#
# What this proves, and against what
#   The pinned Azurite Blob emulator (a sibling container on a private
#   user-defined network) hosts one store. Independent Rust and Go writer
#   *processes* publish into it through the opt-in MW2 transaction API
#   (`--writer-mode optimistic`), and the court records:
#
#     S1  disjoint mixed-language writers on one generation
#     S2  conflicting mixed-language writers on one name
#     S3  ambiguous success on the conditional head write (Rust-killed/Go-
#         resolved and Go-killed/Rust-resolved)
#     S4  fenced takeover with a suspended stale writer (both languages stale)
#     S5  a held reader never observes a newer generation (both languages)
#     S6  concurrent GC while mixed writers publish, with snapshot roots
#     S7  fail-closed mode detection against an endpoint that accepts a stale
#         If-Match or ignores If-None-Match: * (Rust and Go)
#     S8  cross-language verification and deterministic parity
#
#   Every scenario is run by `bench/scripts/azure_mw2_inner_court.py` inside the
#   `fuse` court, the only image carrying both toolchains. FUSE itself is not
#   used; the hard limits are the same as every other court (3g/3g, 4 cpus,
#   1024 pids, bounded /tmp, cap-drop=ALL, no-new-privileges).
#
# Label
#   A single Azure Blob emulator on loopback. Azurite is not Azure: service
#   versions, block limits, listing paging, replication, media and cloud failure
#   modes are not exercised. Process-fault proxies only; no physical fault. No
#   fleet, cloud, power-loss, or production claim. MW1 remains the default
#   everywhere; this court opts into MW2 explicitly.
#
# Skip contract
#   A missing Docker CLI/daemon, an unobtainable pinned image, or an unavailable
#   court network prints exactly one `azure_mw2_court: skip <reason>` line and
#   exits 78. The aggregate records that as `skip`, never as a pass.
#
# Usage:
#   bash scripts/azure_mw2_court.sh
set -Eeuo pipefail

skip() {
  echo "azure_mw2_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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

stamp="${WEFT_AZURE_MW2_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

receipt="bench/receipts/azure-mw2-${stamp}.json"
log="bench/receipts/azure-mw2-${stamp}.log"
inner_receipt="$receipt"

azurite_name="weft-azm2-azurite-$$"
network="weft-azm2-net-$$"
container_base="weft-azm2-${suffix}"
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
    echo "azure_mw2_court: the Azurite container exited before becoming ready; logs follow" >&2
    docker logs "$azurite_name" >&2 || true
    exit 1
  fi
  sleep 0.25
done
if [[ "$ready" != 1 ]]; then
  echo "azure_mw2_court: Azurite never reported a listening Blob service; logs follow" >&2
  docker logs "$azurite_name" >&2 || true
  exit 1
fi

export WEFT_DOCKER_NETWORK="$network"
export WEFT_AZURE_TEST_ENDPOINT="http://${azurite_name}:10000/devstoreaccount1"
export WEFT_AZURE_ENDPOINT="$WEFT_AZURE_TEST_ENDPOINT"
export WEFT_AZURE_TEST_CONTAINER="$container_base"
export WEFT_AZURE_ACCOUNT="$account"
export WEFT_AZURE_KEY="$account_key"
# The general lease. S4 overrides it to 3 s for its two takeover rounds.
export WEFT_AZURE_LOCK_LEASE_SECS="${WEFT_AZURE_LOCK_LEASE_SECS:-60}"
export WEFT_AZURE_IMAGE="$azurite_image"
export WEFT_AZURE_VERSION="$azurite_version"
export WEFT_AZURE_IMAGE_ID="$azurite_image_id"
export WEFT_AZURE_SERVER_DIGEST="$azurite_digest"
export WEFT_AZURE_MW2_STAMP="$stamp"
export WEFT_AZURE_MW2_RECEIPT="$inner_receipt"
export WEFT_AZURE_MW2_SCENARIOS="${WEFT_AZURE_MW2_SCENARIOS:-S1,S2,S3,S4,S5,S6,S7,S8}"
export WEFT_AZURE_MW2_LOG="$log"

echo "azure_mw2_court: Azurite $azurite_version image=$azurite_digest endpoint=$WEFT_AZURE_TEST_ENDPOINT scenarios=$WEFT_AZURE_MW2_SCENARIOS" >&2

inner_status=0
if ! scripts/docker-court.sh fuse \
  "cargo build --quiet --example mw2_writer && cargo build --quiet --bin weftfs && (cd WeftFS-Go && go build -o /work/.docker-cache/weft-mw2-writer ./cmd/mw2writer) && python3 bench/scripts/azure_mw2_inner_court.py" \
  >"$log" 2>&1; then
  inner_status=1
fi

# Always leave a machine-readable record, even when the inner court died before
# writing one. Never invent scenario counts.
if [[ ! -s "$inner_receipt" ]]; then
  cat >"$inner_receipt" <<JSON
{
  "schema": "weft-azure-mw2-court-v1",
  "stamp": "$stamp",
  "generated_utc": "$stamp",
  "result": "fail",
  "label": "The inner court did not produce a receipt; see the log.",
  "azurite": {
    "release": "$azurite_version",
    "image": "$azurite_image",
    "image_id": "$azurite_image_id",
    "image_digest": "$azurite_digest"
  },
  "court": {
    "kind": "fuse",
    "memory": "$WEFT_MEMORY",
    "cpus": "$WEFT_CPUS",
    "pids": "$WEFT_PIDS"
  },
  "scenarios": [],
  "summary": {"scenarios": 0, "scenarios_passed": 0, "checks": 0, "checks_passed": 0, "failures": 1},
  "failures": ["the inner court produced no receipt"],
  "log": "$log"
}
JSON
fi

echo "azure_mw2_court: receipt $inner_receipt" >&2
if [[ "$inner_status" != 0 ]]; then
  echo "azure_mw2_court: the Azure MW2 court reported failures" >&2
  tail -40 "$log" >&2 || true
  exit 1
fi
