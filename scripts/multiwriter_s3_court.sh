#!/usr/bin/env bash
# R4 MinIO MW2 benchmark court. Starts the pinned MinIO release in a sibling
# container (same hard limits as every court) and runs the N=4 disjoint
# benchmark inside the bench court on the same Docker network.
#
# Usage: scripts/multiwriter_s3_court.sh
#
# Environment-only skips print one reason line and exit 78. MinIO data is a
# tmpfs: this proves the conditional-write protocol, not media durability.
set -Eeuo pipefail

skip() { echo "multiwriter_s3_court: skip $1" >&2; exit 78; }

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"

[[ "$(uname -m)" == "x86_64" ]] || skip "only the verified linux-amd64 MinIO release is supported"
command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
command -v curl >/dev/null 2>&1 || skip "curl is required"

minio_release="${WEFT_S3_SERVER_RELEASE:-RELEASE.2025-04-22T22-12-26Z}"
minio_sha256="${WEFT_S3_SERVER_SHA256:-53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c}"
minio_bin="$root/.docker-cache/minio/minio"
mkdir -p "$(dirname "$minio_bin")"
if [[ ! -f "$minio_bin" ]]; then
  if ! curl -fL --retry 3 -o "$minio_bin.part" \
    "https://github.com/minio/minio/releases/download/${minio_release}/minio.linux-amd64.${minio_release}"; then
    rm -f "$minio_bin.part"
    skip "cannot obtain the pinned MinIO release $minio_release"
  fi
  mv "$minio_bin.part" "$minio_bin"
fi
chmod +x "$minio_bin"
actual="$(sha256sum "$minio_bin" | awk '{print $1}')"
[[ "$actual" == "$minio_sha256" ]] || { echo "MinIO sha256 mismatch: $actual" >&2; exit 1; }

case "$(docker image inspect debian:bookworm-slim --format '{{.Id}}' 2>/dev/null || true)" in
  "") docker pull debian:bookworm-slim >/dev/null || skip "cannot obtain debian:bookworm-slim" ;;
esac
image_digest="$(docker image inspect debian:bookworm-slim --format '{{index .RepoDigests 0}}')"
image_id="$(docker image inspect debian:bookworm-slim --format '{{.Id}}')"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"
access_key="${WEFT_S3_ACCESS_KEY:-weftcourt}"
secret_key="${WEFT_S3_SECRET_KEY:-weftcourt-secret}"
minio_name="weft-mw2-minio-$$"
network="weft-mw2-net-$$"
bucket="weft-mw2-bench-$suffix"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

cleanup() {
  docker rm -f "$minio_name" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM
docker network create "$network" >/dev/null || skip "cannot create Docker network $network"
docker run -d --name "$minio_name" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 --security-opt no-new-privileges --cap-drop=ALL \
  --network "$network" --tmpfs /data:rw,size=1g,nosuid,nodev \
  -p 127.0.0.1::9000 \
  -e MINIO_ROOT_USER="$access_key" -e MINIO_ROOT_PASSWORD="$secret_key" \
  -v "$minio_bin":/minio:ro debian:bookworm-slim \
  /minio server /data --address :9000 --console-address :9001 >/dev/null

host_port="$(docker port "$minio_name" 9000/tcp | head -1 | sed 's/.*://')"
ready=0
for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:${host_port}/minio/health/ready" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.5
done
if [[ "$ready" != 1 ]]; then
  docker logs "$minio_name" >&2 || true
  skip "MinIO did not become ready"
fi

export WEFT_DOCKER_NETWORK="$network"
export WEFT_S3_ENDPOINT="http://${minio_name}:9000"
export WEFT_S3_BUCKET="$bucket"
export WEFT_S3_REGION="${WEFT_S3_REGION:-us-east-1}"
export WEFT_S3_ACCESS_KEY="$access_key"
export WEFT_S3_SECRET_KEY="$secret_key"
export WEFT_S3_SERVER_LABEL="$(docker exec "$minio_name" /minio --version | head -1 2>/dev/null || echo "$minio_release")"
export WEFT_S3_SERVER_VERSION="$minio_release"
export WEFT_S3_SERVER_SHA256="$actual"
export WEFT_S3_CONTAINER_IMAGE="$image_digest"
export WEFT_S3_CONTAINER_IMAGE_ID="$image_id"

echo "multiwriter_s3_court: MinIO $minio_release sha256=$actual endpoint=$WEFT_S3_ENDPOINT bucket=$bucket" >&2
scripts/docker-court.sh bench "cd /work && bash scripts/bench_multiwriter.sh"
