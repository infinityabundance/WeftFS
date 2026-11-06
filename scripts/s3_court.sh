#!/usr/bin/env bash
# S3 court (subphase Q4): a real MinIO server in Docker, the Rust S3 backend
# tests, and an optional local-vs-S3 benchmark.
#
# Why not `minio/minio`?
#   The official Docker Hub repository `minio/minio` is no longer pullable
#   (registry returns 404/unauthorized as of 2026-11-05). MinIO still publishes
#   signed release binaries on GitHub. This court runs the official linux-amd64
#   release binary inside a minimal `debian:bookworm-slim` container under the
#   same hard limits as every other court, and records both the binary sha256
#   and the container image digest in its receipt. It does not claim to run the
#   official container image.
#
# Usage:
#   scripts/s3_court.sh            # tests only
#   scripts/s3_court.sh --bench    # tests plus the local-vs-S3 benchmark
#   scripts/s3_court.sh --overlay  # the R9 overlay court on the S3 backend
#                                  # (instead of the rust S3 unit tests)
#   scripts/s3_court.sh --planes   # the R10 blob/commit plane court: four
#                                  # layouts (local/local, s3/s3, s3-blobs/
#                                  # local-commits, local-blobs/s3-commits)
#   scripts/s3_court.sh --packs    # the R11 physical pack court: flat vs packed
#                                  # over local and MinIO
#   scripts/s3_court.sh --distributed  # the R13 distributed correctness court:
#                                  # one MinIO server started *inside* the bench
#                                  # container so the harness can stop/restart
#                                  # the process; S1-S9
#   scripts/s3_court.sh --matrix   # the R16 compatibility matrix only: the
#                                  # eight behaviours "S3-compatible" servers
#                                  # differ on, measured against MinIO
#
# Every container has the same hard limits as `scripts/docker-court.sh`:
# memory 3g, swap 3g, pids 1024, cpus 4, bounded tmpfs, no-new-privileges,
# cap-drop=ALL. The MinIO data directory is a tmpfs, so this court proves
# protocol and conditional-write behaviour, not media or power-loss durability.
set -Eeuo pipefail

# Environment-only skips print exactly one reason line and exit 78. The
# aggregate verification court records that as `skip`, never as a pass.
skip() {
  echo "s3_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bench=0
overlay=0
planes=0
packs=0
distributed=0
matrix=0
for arg in "$@"; do
  case "$arg" in
    --bench) bench=1 ;;
    --overlay) overlay=1 ;;
    --planes) planes=1 ;;
    --packs) packs=1 ;;
    --distributed) distributed=1 ;;
    --matrix) matrix=1 ;;
    -h|--help)
      sed -n '2,35p' "$0"
      exit 0
      ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"
WEFT_NPROC="${WEFT_NPROC:-2048}"

if [[ "$(uname -m)" != "x86_64" ]]; then
  skip "only the verified linux-amd64 MinIO release is supported (host is $(uname -m))"
fi
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
  echo "s3_court: downloading official MinIO binary $minio_release" >&2
  if ! curl -fL --retry 3 -o "$minio_bin.part" "$minio_url"; then
    rm -f "$minio_bin.part"
    skip "cannot obtain the pinned MinIO release $minio_release"
  fi
  mv "$minio_bin.part" "$minio_bin"
fi
chmod +x "$minio_bin"
actual_sha256="$(sha256sum "$minio_bin" | awk '{print $1}')"
if [[ "$actual_sha256" != "$minio_sha256" ]]; then
  echo "s3_court: MinIO binary sha256 mismatch (expected $minio_sha256, got $actual_sha256)" >&2
  exit 1
fi
minio_version="$("$minio_bin" --version | head -1)"

if [[ "$distributed" == 1 ]]; then
  # R13 distributed correctness court. The harness starts this verified binary
  # itself, on loopback inside the bench container, so it can kill and restart
  # the server process mid-run. No external MinIO container or network is used.
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p bench/receipts
  chmod a+rwX bench/receipts 2>/dev/null || true
  export WEFT_DIST_STAMP="$stamp"
  export WEFT_MINIO_BIN="/work/.docker-cache/minio/minio"
  export WEFT_S3_SERVER_LABEL="$minio_version"
  export WEFT_S3_SERVER_VERSION="$minio_release"
  export WEFT_S3_SERVER_RELEASE="$minio_release"
  export WEFT_S3_SERVER_SHA256="$actual_sha256"

  echo "s3_court: distributed court, MinIO $minio_release sha256=$actual_sha256 (started by the harness)" >&2
  scripts/docker-court.sh bench \
    "cd /work && bash bench/scripts/distributed_court.sh"

  distributed_receipt="bench/receipts/distributed-${stamp}.json"
  court_receipt="bench/receipts/s3-court-${stamp}.json"
  cat > "$court_receipt" <<JSON
{
  "schema": "weft-s3-court-v1",
  "generated_utc": "$stamp",
  "server": {
    "product": "MinIO",
    "release": "$minio_release",
    "version_line": "$minio_version",
    "binary_sha256": "$actual_sha256",
    "binary_url": "$minio_url",
    "started_by": "bench/scripts/distributed_court.sh inside the bench court",
    "note": "The harness starts the verified MinIO release binary on loopback inside the bench container so it can stop and restart the server process during S6. This is a process-lifecycle test, not a container or media test."
  },
  "container": {
    "kind": "bench",
    "memory": "$WEFT_MEMORY",
    "memory_swap": "$WEFT_MEMORY",
    "pids": "$WEFT_PIDS",
    "cpus": "$WEFT_CPUS",
    "nproc": "$WEFT_NPROC",
    "cap_drop": "ALL",
    "no_new_privileges": true,
    "tmpfs": "/tmp 1g (MinIO data)"
  },
  "tests": {
    "command": "bash bench/scripts/distributed_court.sh",
    "deterministic_court": "cargo test --release --test distributed_semantics"
  },
  "benchmark": "$distributed_receipt",
  "label": "Single-host distributed correctness court on loopback. Not a cloud, multi-node, fleet, device-cache, media, power-loss, or production claim."
}
JSON
  echo "s3_court: receipt $court_receipt" >&2
  exit 0
fi

container_image="debian:bookworm-slim"
if ! docker image inspect "$container_image" >/dev/null 2>&1; then
  if ! docker pull "$container_image" >/dev/null; then
    skip "cannot obtain the pinned $container_image court image"
  fi
fi
container_image_digest="$(docker image inspect "$container_image" --format '{{index .RepoDigests 0}}')"
container_image_id="$(docker image inspect "$container_image" --format '{{.Id}}')"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
if [[ -n "${WEFT_COMPAT_STAMP:-}" ]]; then
  stamp="$WEFT_COMPAT_STAMP"
fi
suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"
access_key="${WEFT_S3_ACCESS_KEY:-weftcourt}"
secret_key="${WEFT_S3_SECRET_KEY:-weftcourt-secret}"
minio_name="weft-s3-minio-$$"
network="weft-s3-net-$$"
court_bucket="weft-s3-court-$suffix"
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

cleanup() {
  docker rm -f "$minio_name" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

if ! docker network create "$network" >/dev/null 2>&1; then
  skip "cannot create the court Docker network $network"
fi
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
  echo "s3_court: MinIO did not become ready; logs follow" >&2
  docker logs "$minio_name" >&2 || true
  exit 1
fi

endpoint="http://${minio_name}:9000"
export WEFT_DOCKER_NETWORK="$network"
export WEFT_S3_ENDPOINT="$endpoint"
export WEFT_S3_BUCKET="$court_bucket"
export WEFT_S3_REGION="${WEFT_S3_REGION:-us-east-1}"
export WEFT_S3_ACCESS_KEY="$access_key"
export WEFT_S3_SECRET_KEY="$secret_key"
export WEFT_S3_TEST_ENDPOINT="$endpoint"
export WEFT_S3_TEST_PREFIX="weft-s3-test-$suffix"
# The general suites do many operations between publications; a writer that is
# idle longer than its lease must reopen (that is the R17 contract), so give
# them a lease long enough not to trip on a slow endpoint. The fencing tests
# deliberately use a short one.
export WEFT_S3_LOCK_LEASE_SECS="${WEFT_S3_LOCK_LEASE_SECS:-60}"
export WEFT_S3_FENCE_LEASE_SECS="${WEFT_S3_FENCE_LEASE_SECS:-3}"
export WEFT_S3_SERVER_LABEL="$minio_version"
export WEFT_S3_SERVER_VERSION="$minio_release"
export WEFT_S3_SERVER_SHA256="$actual_sha256"
export WEFT_S3_CONTAINER_IMAGE="$container_image_digest"
export WEFT_S3_CONTAINER_IMAGE_ID="$container_image_id"

echo "s3_court: MinIO $minio_release sha256=$actual_sha256 endpoint=$endpoint bucket=$court_bucket" >&2

overlay_receipt=""
planes_receipt=""
packs_receipt=""
matrix_receipt=""
if [[ "$matrix" == 1 ]]; then
  # R16: the compatibility matrix alone. Same MinIO, same hard limits, same
  # bucket prefixing as the backend court; this receipt is what the Ceph RGW
  # court compares its own matrix against.
  export WEFT_S3_SERVER_LABEL="MinIO $minio_release"
  export WEFT_S3_SERVER_VERSION="$minio_release"
  export WEFT_COMPAT_STAMP="$stamp"
  matrix_receipt="bench/receipts/s3-compat-minio-${stamp}.json"
  scripts/docker-court.sh rust \
    "cd /work && cargo run --quiet --example s3_court_bucket && cargo run --quiet --example s3_compat_matrix -- --out $matrix_receipt"
elif [[ "$planes" == 1 ]]; then
  # Same bench court, same hard limits, same MinIO container. The plane court
  # derives one bucket per S3 layout so the four layouts never share control
  # state, and stamps every artifact with the same UTC second.
  export WEFT_PLANES_STAMP="$stamp"
  planes_receipt="bench/receipts/planes-${stamp}.json"
  scripts/docker-court.sh bench \
    "cd /work && cargo run --release --quiet --example s3_court_bucket && bash bench/scripts/planes_court.sh"
elif [[ "$packs" == 1 ]]; then
  # Same bench court, same hard limits, same MinIO container. The harness
  # derives one bucket per configuration so the four configurations never
  # share control state, and stamps every artifact with the same UTC second.
  export WEFT_PACKS_STAMP="$stamp"
  packs_receipt="bench/receipts/packs-${stamp}.json"
  scripts/docker-court.sh bench \
    "cd /work && cargo run --release --quiet --example s3_court_bucket && bash bench/scripts/packs_court.sh"
elif [[ "$overlay" == 1 ]]; then
  # Same benchmark court, same hard limits, same MinIO container. The bucket is
  # created first, then the overlay harness derives one bucket per
  # configuration so the two runs never share a store.
  overlay_receipt="bench/receipts/overlay-s3-${stamp}.json"
  export WEFT_OVERLAY_RECEIPT="$overlay_receipt"
  export WEFT_OVERLAY_LOG="bench/receipts/overlay-s3-${stamp}.log"
  scripts/docker-court.sh bench \
    "cd /work && cargo run --release --quiet --example s3_court_bucket && bash bench/scripts/overlay_court.sh --s3"
else
  scripts/docker-court.sh rust \
    "cd /work && cargo run --quiet --example s3_court_bucket && cargo test --test s3_backend --test s3_fencing -- --test-threads=1 --nocapture"
fi

receipt="bench/receipts/s3-backend-${stamp}.json"
if [[ "$bench" == 1 && "$overlay" == 0 && "$packs" == 0 && "$matrix" == 0 ]]; then
  scripts/docker-court.sh rust \
    "cd /work && cargo run --release --quiet --example bench_s3_backend -- --out ${receipt}"
fi

court_receipt="bench/receipts/s3-court-${stamp}.json"
cat > "$court_receipt" <<JSON
{
  "schema": "weft-s3-court-v1",
  "generated_utc": "$stamp",
  "server": {
    "product": "MinIO",
    "release": "$minio_release",
    "version_line": "$minio_version",
    "binary_sha256": "$actual_sha256",
    "binary_url": "$minio_url",
    "container_image": "$container_image_digest",
    "container_image_id": "$container_image_id",
    "note": "The official minio/minio Docker Hub repository was not pullable on 2026-11-05 (registry 404/unauthorized). The official release binary runs in a minimal debian:bookworm-slim container under the same hard limits."
  },
  "container": {
    "memory": "$WEFT_MEMORY",
    "memory_swap": "$WEFT_MEMORY",
    "pids": "$WEFT_PIDS",
    "cpus": "$WEFT_CPUS",
    "nproc": "$WEFT_NPROC",
    "cap_drop": "ALL",
    "no_new_privileges": true,
    "data": "tmpfs 1g",
    "network": "user-defined bridge (container-name DNS)",
    "host_port": $host_port
  },
  "tests": {
    "command": "$(if [[ "$matrix" == 1 ]]; then echo "cargo run --example s3_compat_matrix -- --out $matrix_receipt"; elif [[ "$planes" == 1 ]]; then echo "bash bench/scripts/planes_court.sh"; elif [[ "$packs" == 1 ]]; then echo "bash bench/scripts/packs_court.sh"; elif [[ "$overlay" == 1 ]]; then echo "bash bench/scripts/overlay_court.sh --s3"; else echo "cargo test --test s3_backend --test s3_fencing -- --test-threads=1 --nocapture"; fi)",
    "bucket_prefix": "$WEFT_S3_TEST_PREFIX"
  },
  "benchmark": $(if [[ "$matrix" == 1 && -n "$matrix_receipt" ]]; then echo "\"$matrix_receipt\""; elif [[ "$planes" == 1 && -n "$planes_receipt" ]]; then echo "\"$planes_receipt\""; elif [[ "$packs" == 1 && -n "$packs_receipt" ]]; then echo "\"$packs_receipt\""; elif [[ "$overlay" == 1 && -n "$overlay_receipt" ]]; then echo "\"$overlay_receipt\""; elif [[ "$bench" == 1 ]]; then echo "\"$receipt\""; else echo "null"; fi),
  "label": "Single-node MinIO on localhost in the OOM-protected Docker court with a tmpfs data directory. Not a cloud, multi-node, fleet, device-cache, media, power-loss, or production claim."
}
JSON

echo "s3_court: receipt $court_receipt" >&2
