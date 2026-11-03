#!/usr/bin/env bash
# WeftFS OOM-protected Docker court.
#
# Usage:
#   scripts/docker-court.sh <rust|go|fuse|bench> "<shell command>"
#
# Every container is hard-limited: memory == swap, pids, cpus, bounded /tmp,
# no-new-privileges, and a unique name for deterministic cleanup.
# There is deliberately no flag to disable these limits (operator directive).
set -Eeuo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <rust|go|fuse|bench> \"<command>\"" >&2
  exit 64
fi

kind="$1"
shift
cmd="$*"

case "$kind" in
  rust|go|fuse|bench) ;;
  *) echo "unknown court '$kind' (expected rust|go|fuse|bench)" >&2; exit 64 ;;
esac

# Hard limits. Environment override is allowed for *smaller* values only.
WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"

validate_limit() {
  local name="$1" value="$2" re="$3"
  if [[ ! "$value" =~ $re ]]; then
    echo "invalid $name='$value' (must match $re); refusing to run unbounded" >&2
    exit 78
  fi
}
validate_limit WEFT_MEMORY "$WEFT_MEMORY" '^[0-9]+(\.[0-9]+)?[mg]$'
validate_limit WEFT_CPUS "$WEFT_CPUS" '^[1-9][0-9]*(\.[0-9]+)?$'
validate_limit WEFT_PIDS "$WEFT_PIDS" '^[1-9][0-9]*$'

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cache_root="$repo_root/.docker-cache"
image="weft/${kind}-court:1"
dockerfile="$repo_root/docker/${kind}.Dockerfile"

mkdir -p "$cache_root"/{cargo-registry,cargo-git,go-mod,go-build,go-tmp,target-rust,target-go}
# The court runs as uid 0 with cap-drop=ALL (no CAP_DAC_OVERRIDE). Host-owned
# cache directories must therefore be writable by any uid; source files are
# never widened. This keeps all compiler/cache writes inside .docker-cache.
chmod -R a+rwX "$cache_root" 2>/dev/null || true
# Receipts are written by the court (uid 0 with cap-drop=ALL, so no DAC
# override). Make the receipts directory writable by any uid exactly like the
# cache; source files stay untouched.
mkdir -p "$repo_root/bench/receipts"
mkdir -p "$repo_root/bench/bundles"
chmod a+rwX "$repo_root/bench/receipts" "$repo_root/bench/bundles" 2>/dev/null || true
# `Cargo.lock` is a generated build artifact: dependency resolution inside the
# court must be able to update it. Source files stay read-only for the court.
chmod a+rw "$repo_root/Cargo.lock" 2>/dev/null || true

if ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "court: building missing image $image from $dockerfile" >&2
  # Buildx accepts --memory/--memory-swap but not --pids-limit. The image build
  # is a trusted minimal Dockerfile; runtime containers always get pids limits.
  docker build --memory=2g --memory-swap=2g --ulimit nofile=65536:65536 \
    -f "$dockerfile" -t "$image" "$repo_root/docker"
fi

# Record the exact court image and engine versions in receipts without giving
# the container any extra capability. These are informational only.
image_id="$(docker image inspect --format '{{.Id}}' "$image" 2>/dev/null || echo unknown)"
docker_server="$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo unknown)"
docker_client="$(docker version --format '{{.Client.Version}}' 2>/dev/null || echo unknown)"
weftfs_rev="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
weftfs_go_rev="$(git -C "$repo_root/WeftFS-Go" rev-parse HEAD 2>/dev/null || echo unknown)"
weftcodec_rev="$(git -C "$repo_root/WeftCodec" rev-parse HEAD 2>/dev/null || echo unknown)"
weftcodec_go_rev="$(git -C "$repo_root/WeftCodec-Go" rev-parse HEAD 2>/dev/null || echo unknown)"

common=(
  --rm
  --init
  --name "weft-${kind}-court-$$"
  --memory="$WEFT_MEMORY"
  --memory-swap="$WEFT_MEMORY"
  --pids-limit="$WEFT_PIDS"
  --cpus="$WEFT_CPUS"
  --ulimit nofile=65536:65536
  --security-opt no-new-privileges
  --tmpfs /tmp:rw,size=1g,nosuid,nodev
  --cap-drop=ALL
  -e "WEFT_IN_COURT=1"
  -e "WEFT_COURT_IMAGE=$image"
  -e "WEFT_COURT_KERNEL=$(uname -r)"
  -e "WEFT_COURT_IMAGE_ID=$image_id"
  -e "WEFT_COURT_MEMORY=$WEFT_MEMORY"
  -e "WEFT_COURT_CPUS=$WEFT_CPUS"
  -e "WEFT_COURT_PIDS=$WEFT_PIDS"
  -e "WEFT_DOCKER_SERVER=$docker_server"
  -e "WEFT_DOCKER_CLIENT=$docker_client"
  -e "WEFT_REPO_WEFTFS=$weftfs_rev"
  -e "WEFT_REPO_WEFTFS_GO=$weftfs_go_rev"
  -e "WEFT_REPO_WEFTCODEC=$weftcodec_rev"
  -e "WEFT_REPO_WEFTCODEC_GO=$weftcodec_go_rev"
  -e "RUST_BACKTRACE=1"
  -e "CARGO_BUILD_JOBS=2"
  -e "CARGO_INCREMENTAL=0"
  -e "CARGO_HOME=/cargo"
  -e "CARGO_TARGET_DIR=/work/.docker-cache/target-rust"
  -e "CARGO_NET_RETRY=3"
  -e "GOCACHE=/work/.docker-cache/go-build"
  -e "GOMODCACHE=/work/.docker-cache/go-mod"
  -e "GOFLAGS=-p=2 -buildvcs=false"
  -e "GOTMPDIR=/work/.docker-cache/go-tmp"
  -e "GOMAXPROCS=4"
  -v "$repo_root:/work"
  -v "$cache_root/cargo-registry:/cargo/registry"
  -v "$cache_root/cargo-git:/cargo/git"
  -w /work
)

# FUSE-capable courts. `bench` is included because the filesystem benchmark
# mounts a real WeftFS image; the device and capability are the only additions
# and every hard limit above is unchanged.
if [[ "$kind" == "fuse" || "$kind" == "bench" ]]; then
  common+=(
    --device /dev/fuse
    --cap-add SYS_ADMIN
    --tmpfs /mnt/weft-fuse:rw,size=512m,nosuid,nodev
    --tmpfs /mnt/weft-bench:rw,size=1g,nosuid,nodev
  )
fi

# Bound in-container scratch and compiler concurrency as a second line of defense.
guarded_cmd="set -Eeuo pipefail; ulimit -n 65536; ulimit -u 2048; export TMPDIR=/tmp; cd /work; ${cmd}"

set -x
exec docker run "${common[@]}" "$image" bash -c "$guarded_cmd"
