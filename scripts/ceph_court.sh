#!/usr/bin/env bash
# Ceph RGW court (subphase R16): an *independent* S3 implementation.
#
# The Q4 court runs MinIO, and the GCS court runs fake-gcs-server. Both are a
# single implementation of their protocol. "S3-compatible" often means "mostly
# compatible", so this court runs the same S3 backend tests and the same
# compatibility matrix against Ceph's RADOS Gateway, which shares no code with
# MinIO.
#
# What runs:
#   1. `quay.io/ceph/demo` (Ceph 19.2.0 squid, all-in-one: mon + mgr + OSD +
#      RGW) in a container under the same hard limits as every other court.
#   2. `cargo test --test s3_backend` — the existing S3 court logic: same
#      content root, generation sequence, read-only reopen, `verify --full`,
#      writer lock, lost conditional head update, crash proxy, GC, and
#      local-vs-S3 byte identity.
#   3. `cargo run --example s3_compat_matrix` — the eight behaviours that
#      commonly differ between "S3-compatible" servers.
#   4. The same matrix against MinIO (`scripts/s3_court.sh --matrix`) and a
#      side-by-side comparison.
#
# Usage:
#   scripts/ceph_court.sh                 # full run
#   scripts/ceph_court.sh --matrix-only   # skip the s3_backend test suite
#   scripts/ceph_court.sh --no-minio      # skip the MinIO comparison
#
# Exit status: 0 = every check passed, 1 = the court ran and failed, 78 =
# **skipped**. 78 is printed as exactly one `ceph_court: skip <reason>` line and
# is used only for environment refusals detected before the server starts:
# non-x86_64, no Docker daemon, the pinned image cannot be obtained, or the
# court's private subnet is already taken. `scripts/verify-all.sh` records that
# as an explicit `skip` in its receipt; a skip never counts as a pass.
#
# The RGW container needs a *bounded* capability set that MinIO does not:
# CHOWN, DAC_OVERRIDE, FOWNER, SETGID, SETUID, because Ceph runs its daemons as
# uid/gid 167 and writes keyrings owned by that user. Every memory, swap, pid,
# CPU, and tmpfs limit is exactly the court default; `--cap-drop=ALL` stays,
# and no capability is added to the *test* container.
#
# Two RGW-specific settings are required and recorded in the receipt:
#   * `--rgw-thread-pool-size 32` (via `CEPH_ARGS`). The default 512-thread RGW
#     pool plus `radosgw-admin`'s own 512-thread pool exceeds the 1024-pid
#     court bound; the kernel then refuses thread creation and both `ceph-mon`
#     and `radosgw` abort silently. This is the same thread-bound lesson as the
#     1,000-mount court, not a WeftFS behaviour.
#   * the client addresses the container by IP. RGW's demo config sets
#     `rgw dns name = <container hostname>`; a request whose Host header equals
#     that name is parsed as *virtual-hosted* style and the whole hostname is
#     taken as the bucket name, so path-style addressing requires an endpoint
#     whose Host is not the RGW hostname. The receipt states this explicitly.
#
# A single-node RGW with a tmpfs OSD proves protocol behaviour, not media,
# power-loss, cloud, or fleet durability.
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

matrix_only=0
with_minio=1
for arg in "$@"; do
  case "$arg" in
    --matrix-only) matrix_only=1 ;;
    --no-minio) with_minio=0 ;;
    -h|--help)
      sed -n '2,56p' "$0"
      exit 0
      ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"
WEFT_NPROC="${WEFT_NPROC:-2048}"

# A skip is an environment refusal, not a result and never a pass. Every skip
# path prints exactly one `ceph_court: skip <reason>` line and exits 78
# (EX_CONFIG); `scripts/verify-all.sh` maps that to an explicit `skip` entry in
# its receipt. Everything after this preflight is the court's own
# responsibility and fails closed with exit 1.
skip() {
  echo "ceph_court: skip $*" >&2
  exit 78
}

if [[ "$(uname -m)" != "x86_64" ]]; then
  skip "unsupported architecture $(uname -m); only linux-amd64 is verified"
fi
if ! command -v docker >/dev/null 2>&1; then
  skip "no docker CLI on PATH"
fi
if ! docker info >/dev/null 2>&1; then
  skip "docker daemon unavailable: $(docker info 2>&1 | head -1)"
fi

# Pinned by digest so a tag move cannot silently change the server under test.
ceph_image="${WEFT_CEPH_IMAGE:-quay.io/ceph/demo@sha256:522483cf07cfce6386b8e18a3edfa88a1b32c688dee231d0a6962b1513557723}"
ceph_tag="quay.io/ceph/demo:latest"
subnet="${WEFT_CEPH_SUBNET:-10.213.77.0/24}"
gateway="${subnet%.*}.1"
rgw_ip="${subnet%.*}.10"
access_key="${WEFT_S3_ACCESS_KEY:-weftcourt}"
secret_key="${WEFT_S3_SECRET_KEY:-weftcourt-secret}"
rgw_uid="${WEFT_CEPH_DEMO_UID:-weftcourt}"
rgw_port=8080
stamp="${WEFT_COMPAT_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"

mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

if ! docker image inspect "$ceph_image" >/dev/null 2>&1; then
  echo "ceph_court: pulling pinned image $ceph_image" >&2
  if ! pull_error="$(docker pull "$ceph_image" 2>&1 >/dev/null)"; then
    skip "cannot obtain pinned image $ceph_image: ${pull_error:-docker pull failed}"
  fi
fi
if ! ceph_image_id="$(docker image inspect "$ceph_image" --format '{{.Id}}' 2>/dev/null)"; then
  skip "pinned image $ceph_image is not present after pull"
fi
ceph_image_digest="$(docker image inspect "$ceph_image" --format '{{index .RepoDigests 0}}')"

# A fixed subnet is needed because the monitor must be told its own public
# address before it starts. Refuse to reuse a subnet that Docker already knows;
# this is an environment collision (another court or a stale network), so it is
# an explicit skip with the exact owner named.
for network_id in $(docker network ls -q); do
  for existing in $(docker network inspect "$network_id" --format '{{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null); do
    if [[ "$existing" == "$subnet" ]]; then
      skip "subnet $subnet is already in use by network $network_id; set WEFT_CEPH_SUBNET"
    fi
  done
done

rgw_name="weft-ceph-rgw-$$"
network="weft-ceph-net-$$"
court_bucket="weft-ceph-court-$suffix"

cleanup() {
  docker rm -f "$rgw_name" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

docker network create --subnet "$subnet" --gateway "$gateway" "$network" >/dev/null

# The demo image bootstraps mon, mgr, osd, rgw and the S3 user from one
# entrypoint. DEMO_DAEMONS omits mds/nfs/rest_api/crash, which this court does
# not need.
docker run -d --name "$rgw_name" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 \
  --security-opt no-new-privileges \
  --cap-drop=ALL \
  --cap-add=CHOWN --cap-add=DAC_OVERRIDE --cap-add=FOWNER \
  --cap-add=SETGID --cap-add=SETUID \
  --network "$network" \
  --ip "$rgw_ip" \
  --hostname "weft-rgw" \
  --tmpfs /var/lib/ceph:rw,size=2g,nosuid,nodev \
  --tmpfs /etc/ceph:rw,size=64m,nosuid,nodev \
  --tmpfs /tmp:rw,size=512m,nosuid,nodev \
  --tmpfs /var/log:rw,size=256m,nosuid,nodev \
  -p "127.0.0.1::$rgw_port" \
  -e MON_IP="$rgw_ip" \
  -e CEPH_PUBLIC_NETWORK="$subnet" \
  -e DEMO_DAEMONS=osd,rgw \
  -e CEPH_ARGS="--rgw-thread-pool-size 32" \
  -e CEPH_DEMO_UID="$rgw_uid" \
  -e CEPH_DEMO_ACCESS_KEY="$access_key" \
  -e CEPH_DEMO_SECRET_KEY="$secret_key" \
  -e RGW_FRONTEND_PORT="$rgw_port" \
  "$ceph_image" >/dev/null

host_port_line="$(docker port "$rgw_name" "$rgw_port/tcp" | head -1)"
host_port="${host_port_line##*:}"
ready=0
for _ in $(seq 1 120); do
  # Any HTTP answer (403 AccessDenied for an anonymous request) proves the
  # RGW frontend is listening, not merely that the mon quorum is up.
  code="$(curl -s -o /dev/null -m 2 -w '%{http_code}' "http://127.0.0.1:${host_port}/" || true)"
  if [[ "$code" == "403" || "$code" == "200" ]]; then
    ready=1
    break
  fi
  sleep 2
done
if [[ "$ready" != 1 ]]; then
  echo "ceph_court: RGW did not answer HTTP within 240 s; logs follow" >&2
  docker exec "$rgw_name" sh -c 'ceph -s 2>&1 | head -20' >&2 || true
  docker logs --tail 60 "$rgw_name" >&2 || true
  exit 1
fi

ceph_version="$(docker exec "$rgw_name" ceph --version 2>&1 | head -1 || true)"
ceph_version_short="$(printf '%s' "$ceph_version" | awk '{print $3}')"
if [[ -z "$ceph_version_short" ]]; then ceph_version_short=unknown; fi
ceph_status="$(docker exec "$rgw_name" sh -c 'timeout 30 ceph -s 2>&1 | sed -n "1,24p"' || true)"
ceph_health="$(printf '%s' "$ceph_status" | grep -m1 'health:' | sed 's/["\\]//g' | xargs || true)"
if [[ -z "$ceph_health" ]]; then ceph_health='health: unavailable'; fi
status_file="bench/receipts/ceph-rgw-${stamp}.cluster.txt"
printf '%s\n' "$ceph_status" > "$status_file"
rgw_threads="$(docker exec "$rgw_name" sh -c 'ps -eL --no-headers | wc -l')"
cgroup_id="$(docker inspect "$rgw_name" --format '{{.Id}}')"
cgroup="/sys/fs/cgroup/system.slice/docker-${cgroup_id}.scope"
pids_current="$(cat "$cgroup/pids.current" 2>/dev/null || echo unknown)"
pids_max="$(cat "$cgroup/pids.max" 2>/dev/null || echo unknown)"
memory_peak="$(cat "$cgroup/memory.peak" 2>/dev/null || echo unknown)"
memory_events="$(awk '/^oom /{print $2}' "$cgroup/memory.events" 2>/dev/null || echo unknown)"
oom_kills="$(awk '/^oom_kill /{print $2}' "$cgroup/memory.events" 2>/dev/null || echo unknown)"

endpoint="http://${rgw_ip}:${rgw_port}"
export WEFT_DOCKER_NETWORK="$network"
matrix_receipt="bench/receipts/s3-compat-ceph-${stamp}.json"

echo "ceph_court: $ceph_version at $endpoint (image $ceph_image_digest)" >&2

# MinIO first, so the comparison has both receipts and the RGW container is
# never idle for long. The RGW environment is exported only afterwards: the
# MinIO court reuses WEFT_S3_SERVER_* for its own receipt and must not see
# Ceph's values.
minio_receipt=""
if [[ "$with_minio" == 1 ]]; then
  export WEFT_COMPAT_STAMP="$stamp"
  scripts/s3_court.sh --matrix
  minio_receipt="bench/receipts/s3-compat-minio-${stamp}.json"
fi

# The S3 backend court and its matrix talk to the RGW container.
export WEFT_DOCKER_NETWORK="$network"
export WEFT_S3_ENDPOINT="$endpoint"
export WEFT_S3_BUCKET="$court_bucket"
export WEFT_S3_REGION="${WEFT_S3_REGION:-us-east-1}"
export WEFT_S3_ACCESS_KEY="$access_key"
export WEFT_S3_SECRET_KEY="$secret_key"
export WEFT_S3_TEST_ENDPOINT="$endpoint"
export WEFT_S3_TEST_PREFIX="weft-ceph-test-$suffix"
# General suites: long enough that ordinary idle time between publications does
# not expire the lease on a slower endpoint (RGW round trips are ~10x MinIO's).
# The fencing suite uses its own short lease on purpose.
export WEFT_S3_LOCK_LEASE_SECS="${WEFT_S3_LOCK_LEASE_SECS:-60}"
export WEFT_S3_FENCE_LEASE_SECS="${WEFT_S3_FENCE_LEASE_SECS:-3}"
export WEFT_S3_SERVER_LABEL="Ceph RGW $ceph_version_short"
export WEFT_S3_SERVER_VERSION="$ceph_version_short"
export WEFT_S3_SERVER_SHA256="none (container image, not a standalone binary)"
export WEFT_S3_CONTAINER_IMAGE="$ceph_image_digest"
export WEFT_S3_CONTAINER_IMAGE_ID="$ceph_image_id"
export WEFT_COMPAT_STAMP="$stamp"
export WEFT_COMPAT_RECEIPT="$matrix_receipt"

if [[ "$matrix_only" == 1 ]]; then
  court_cmd="cd /work && cargo run --quiet --example s3_court_bucket && cargo run --quiet --example s3_compat_matrix -- --out $matrix_receipt"
else
  court_cmd="cd /work && cargo run --quiet --example s3_court_bucket && cargo test --test s3_backend --test s3_fencing -- --test-threads=1 --nocapture && cargo run --quiet --example s3_compat_matrix -- --out $matrix_receipt"
fi
scripts/docker-court.sh rust "$court_cmd"

comparison_receipt=""
if [[ "$with_minio" == 1 ]]; then
  comparison_receipt="bench/receipts/s3-compat-matrix-${stamp}.json"
  scripts/docker-court.sh rust \
    "cd /work && cargo run --quiet --example s3_compat_matrix -- --compare $minio_receipt $matrix_receipt --out $comparison_receipt"
fi

court_receipt="bench/receipts/ceph-rgw-${stamp}.json"
cat > "$court_receipt" <<JSON
{
  "schema": "weft-ceph-rgw-court-v1",
  "generated_utc": "$stamp",
  "server": {
    "product": "Ceph RADOS Gateway (independent S3 implementation)",
    "image": "$ceph_image",
    "image_digest": "$ceph_image_digest",
    "image_id": "$ceph_image_id",
    "tag": "$ceph_tag",
    "version_line": "$ceph_version",
    "daemons": "mon + mgr + 1 bluestore OSD + rgw (beast), all in one demo container",
    "rgw_endpoint": "$endpoint",
    "addressing": "path-style; the client uses an IP-literal Host because RGW's demo config sets rgw dns name to the container hostname and treats a matching Host as a virtual-hosted bucket",
    "server_settings": {
      "rgw_thread_pool_size": 32,
      "why_thread_pool": "the default 512-thread RGW pool plus radosgw-admin's own 512-thread pool exceeds the 1024-pid court bound; thread-creation failure aborts ceph-mon and radosgw",
      "rgw_frontends": "beast endpoint=0.0.0.0:$rgw_port"
    }
  },
  "container": {
    "kind": "sidecar for the rust court",
    "memory": "$WEFT_MEMORY",
    "memory_swap": "$WEFT_MEMORY",
    "pids": "$WEFT_PIDS",
    "cpus": "$WEFT_CPUS",
    "nproc": "$WEFT_NPROC",
    "cap_drop": "ALL",
    "cap_add": ["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETGID", "SETUID"],
    "why_capabilities": "Ceph daemons run as uid/gid 167 and write keyrings owned by that user; MinIO needs no capability",
    "no_new_privileges": true,
    "tmpfs": {"/var/lib/ceph": "2g", "/etc/ceph": "64m", "/tmp": "512m", "/var/log": "256m"},
    "observed": {
      "threads": $rgw_threads,
      "pids_current": $(if [[ "$pids_current" =~ ^[0-9]+$ ]]; then echo "$pids_current"; else echo '"unknown"'; fi),
      "pids_max": $(if [[ "$pids_max" =~ ^[0-9]+$ ]]; then echo "$pids_max"; else echo '"unknown"'; fi),
      "memory_peak_bytes": $(if [[ "$memory_peak" =~ ^[0-9]+$ ]]; then echo "$memory_peak"; else echo '"unknown"'; fi),
      "cgroup_oom": $(if [[ "$memory_events" =~ ^[0-9]+$ ]]; then echo "$memory_events"; else echo '"unknown"'; fi),
      "cgroup_oom_kills": $(if [[ "$oom_kills" =~ ^[0-9]+$ ]]; then echo "$oom_kills"; else echo '"unknown"'; fi)
    },
    "cluster_status": "$ceph_health",
    "cluster_status_file": "$status_file"
  },
  "tests": {
    "suite": $(if [[ "$matrix_only" == 1 ]]; then echo '"matrix only"'; else echo '"cargo test --test s3_backend --test s3_fencing -- --test-threads=1"'; fi),
    "matrix": "$matrix_receipt",
    "minio_matrix": $(if [[ -n "$minio_receipt" ]]; then echo "\"$minio_receipt\""; else echo "null"; fi),
    "comparison": $(if [[ -n "$comparison_receipt" ]]; then echo "\"$comparison_receipt\""; else echo "null"; fi),
    "bucket_prefix": "$WEFT_S3_TEST_PREFIX"
  },
  "label": "Single-node Ceph RGW 19.2.0 with a tmpfs bluestore OSD in the OOM-protected Docker court. Not a cloud, multi-node, fleet, media, power-loss, or production claim."
}
JSON

echo "ceph_court: receipt $court_receipt" >&2
if [[ -n "$comparison_receipt" ]]; then
  echo "ceph_court: comparison $comparison_receipt" >&2
fi
