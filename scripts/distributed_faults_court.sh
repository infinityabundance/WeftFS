#!/usr/bin/env bash
# T025 host runner: a real MinIO upstream on loopback, the fuse court as the
# client container, and the fault-injecting proxy *inside* the client (so every
# client process is an independent process behind the same proxy).
#
#   bash scripts/distributed_faults_court.sh
#
# What this is: one host, one backend instance (the official MinIO release
# binary in a sidecar container under the same hard limits), several independent
# client processes, and proxy-level faults. What it is **not**: a partition of
# physical nodes, a storage-server crash, a clock-skew experiment, media
# failure, or power loss. Those are named as non-claims in the receipt.
#
# Environment-only skips print exactly one `distributed_faults_court: skip ...`
# line and exit 78; the aggregate court records a skip, never a pass.
set -Eeuo pipefail

skip() {
  echo "distributed_faults_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"
WEFT_NPROC="${WEFT_NPROC:-2048}"

[[ "$(uname -m)" == "x86_64" ]] || skip "only the verified linux-amd64 MinIO release is supported (host is $(uname -m))"
command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
command -v curl >/dev/null 2>&1 || skip "curl is required to fetch the pinned MinIO release"

minio_release="${WEFT_S3_SERVER_RELEASE:-RELEASE.2025-04-22T22-12-26Z}"
minio_sha256="${WEFT_S3_SERVER_SHA256:-53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c}"
minio_dir="$repo_root/.docker-cache/minio"
minio_bin="$minio_dir/minio"
minio_url="https://github.com/minio/minio/releases/download/${minio_release}/minio.linux-amd64.${minio_release}"
mkdir -p "$minio_dir"
if [[ ! -f "$minio_bin" ]]; then
  echo "distributed_faults_court: downloading pinned MinIO $minio_release" >&2
  if ! curl -fL --retry 3 -o "$minio_bin.part" "$minio_url"; then
    rm -f "$minio_bin.part"
    skip "cannot obtain the pinned MinIO release $minio_release"
  fi
  mv "$minio_bin.part" "$minio_bin"
fi
chmod +x "$minio_bin"
actual_sha256="$(sha256sum "$minio_bin" | awk '{print $1}')"
[[ "$actual_sha256" == "$minio_sha256" ]] || {
  echo "distributed_faults_court: MinIO sha256 mismatch (expected $minio_sha256, got $actual_sha256)" >&2
  exit 1
}
minio_version="$("$minio_bin" --version | head -1)"

container_image="debian:bookworm-slim"
if ! docker image inspect "$container_image" >/dev/null 2>&1; then
  docker pull "$container_image" >/dev/null || skip "cannot obtain $container_image"
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
if [[ -n "${WEFT_COMPAT_STAMP:-}" ]]; then stamp="$WEFT_COMPAT_STAMP"; fi
suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"
access_key="${WEFT_S3_ACCESS_KEY:-weftcourt}"
secret_key="${WEFT_S3_SECRET_KEY:-weftcourt-secret}"
minio_name="weft-fault-minio-$$"
network="weft-fault-net-$$"

cleanup() {
  docker rm -f "$minio_name" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

docker network create "$network" >/dev/null 2>&1 || skip "cannot create the court Docker network $network"
docker run -d --name "$minio_name" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 \
  --security-opt no-new-privileges \
  --cap-drop=ALL \
  --network "$network" \
  --tmpfs /data:rw,size=1g,nosuid,nodev \
  -p 127.0.0.1::9000 \
  -e MINIO_ROOT_USER="$access_key" \
  -e MINIO_ROOT_PASSWORD="$secret_key" \
  -v "$minio_bin":/minio:ro \
  "$container_image" \
  /minio server /data --address :9000 --console-address :9001 >/dev/null

host_port_line="$(docker port "$minio_name" 9000/tcp | head -1)"
host_port="${host_port_line##*:}"
ready=0
for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:${host_port}/minio/health/ready" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.5
done
if [[ "$ready" != 1 ]]; then
  echo "distributed_faults_court: MinIO did not become ready; logs follow" >&2
  docker logs "$minio_name" >&2 || true
  exit 1
fi

export WEFT_DOCKER_NETWORK="$network"
# The *direct* endpoint bypasses the proxy; verdicts are read through it.
export WEFT_S3_TEST_ENDPOINT="http://${minio_name}:9000"
export WEFT_S3_ENDPOINT="$WEFT_S3_TEST_ENDPOINT"
export WEFT_S3_REGION="${WEFT_S3_REGION:-us-east-1}"
export WEFT_S3_ACCESS_KEY="$access_key"
export WEFT_S3_SECRET_KEY="$secret_key"
export WEFT_S3_LOCK_LEASE_SECS="${WEFT_S3_LOCK_LEASE_SECS:-60}"
export WEFT_S3_SERVER_LABEL="$minio_version"
export WEFT_S3_SERVER_RELEASE="$minio_release"
export WEFT_S3_SERVER_SHA256="$actual_sha256"
export WEFT_S3_SERVER_PRODUCT="MinIO"
export WEFT_DIST_STAMP="$stamp"
export WEFT_S3_COURT_BUCKET_PREFIX="weft-dist-${suffix}"

mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

echo "distributed_faults_court: MinIO $minio_release sha256=$actual_sha256 on $network" >&2
if ! scripts/docker-court.sh fuse "cd /work && bash scripts/distributed_faults_inner.sh"; then
  echo "distributed_faults_court: inner court failed" >&2
  exit 1
fi

receipt="bench/receipts/distributed-faults-${stamp}.json"
if [[ ! -f "$receipt" ]]; then
  echo "distributed_faults_court: inner court produced no receipt at $receipt" >&2
  exit 1
fi
python3 - "$receipt" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
ok = doc.get("result") == "pass" and all(doc.get("verdicts", {}).values())
print(json.dumps({"receipt": sys.argv[1], "result": doc.get("result"),
                  "verdicts": doc.get("verdicts")}, sort_keys=True))
raise SystemExit(0 if ok else 1)
PY
