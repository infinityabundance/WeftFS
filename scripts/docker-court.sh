#!/usr/bin/env bash
# WeftFS OOM-protected Docker court.
#
# Usage:
#   scripts/docker-court.sh <rust|go|fuse|bench|tla|vm|python> "<shell command>"
#
# `tla` runs TLC only. `vm` additionally gets /dev/kvm when the host exposes
# it (QEMU falls back to TCG inside the guest runner when it is absent); every
# hard limit below is unchanged.
#
# Every container is hard-limited: memory == swap, pids, cpus, bounded /tmp,
# no-new-privileges, and a unique name for deterministic cleanup.
# There is deliberately no flag to disable these limits (operator directive).
set -Eeuo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <rust|go|fuse|bench|tla|vm|python> \"<command>\"" >&2
  exit 64
fi

kind="$1"
shift
cmd="$*"

case "$kind" in
  rust|go|fuse|bench|tla|vm|python) ;;
  *) echo "unknown court '$kind' (expected rust|go|fuse|bench|tla|vm|python)" >&2; exit 64 ;;
esac

# Hard limits. Environment override is allowed for *smaller* values only.
WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"
# Second line of defense: RLIMIT_NPROC for the container's single uid. It is
# always a hard bound; the cgroup pids limit remains the binding bound whenever
# it is the smaller of the two. A scale court may raise both together but never
# disable either.
WEFT_NPROC="${WEFT_NPROC:-2048}"
# Attach stdin only when a caller explicitly streams a secret (for example a
# signing seed) into the court. The default is detached stdin.
WEFT_COURT_STDIN="${WEFT_COURT_STDIN:-0}"

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
validate_limit WEFT_NPROC "$WEFT_NPROC" '^[1-9][0-9]*$'
validate_limit WEFT_COURT_STDIN "$WEFT_COURT_STDIN" '^(0|1)$'
# Optional user-defined Docker network (the S3 court uses one so the test
# container can resolve the MinIO container by name). Never required; when set
# it must be a valid network name.
WEFT_DOCKER_NETWORK="${WEFT_DOCKER_NETWORK:-}"
if [[ -n "$WEFT_DOCKER_NETWORK" ]]; then
  validate_limit WEFT_DOCKER_NETWORK "$WEFT_DOCKER_NETWORK" '^[a-zA-Z0-9][a-zA-Z0-9_.-]*$'
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cache_root="$repo_root/.docker-cache"
image="weft/${kind}-court:1"
if [[ "$kind" == "python" ]]; then
  dockerfile="$repo_root/docker/python-court.Dockerfile"
else
  dockerfile="$repo_root/docker/${kind}.Dockerfile"
fi

mkdir -p "$cache_root"/{cargo-registry,cargo-git,go-mod,go-build,go-tmp,target-rust,target-go}
# The court runs as uid 0 with cap-drop=ALL (no CAP_DAC_OVERRIDE). Host-owned
# cache directories must therefore be writable by any uid; source files are
# never widened. This keeps all compiler/cache writes inside .docker-cache.
mkdir -p "$cache_root"/{powerloss,tla}
chmod -R a+rwX "$cache_root" 2>/dev/null || true
# Receipts are written by the court (uid 0 with cap-drop=ALL, so no DAC
# override). Make the receipts directory writable by any uid exactly like the
# cache; source files stay untouched.
mkdir -p "$repo_root/bench/receipts"
mkdir -p "$repo_root/bench/bundles"
chmod a+rwX "$repo_root/bench/receipts" "$repo_root/bench/bundles" 2>/dev/null || true
# The Python court builds a wheel and records its digest. These are generated
# artifacts, so only the output directories are widened (never source files).
mkdir -p "$repo_root/python/dist" "$repo_root/python/build"
chmod a+rwX "$repo_root/python/dist" "$repo_root/python/build" 2>/dev/null || true
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
  -e "WEFT_COURT_NPROC=$WEFT_NPROC"
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

if [[ "$WEFT_COURT_STDIN" == "1" ]]; then
  common+=(-i)
fi

# Forward only the S3 court's explicitly named environment. Nothing else from
# the host environment reaches the container.
for name in \
  WEFT_S3_ENDPOINT WEFT_S3_BUCKET WEFT_S3_REGION WEFT_S3_ACCESS_KEY \
  WEFT_S3_SECRET_KEY WEFT_S3_SESSION_TOKEN WEFT_S3_LOCK_LEASE_SECS \
  WEFT_S3_FENCE_LEASE_SECS \
  WEFT_S3_TEST_ENDPOINT WEFT_S3_TEST_PREFIX WEFT_S3_RECEIPT \
  WEFT_S3_SERVER_LABEL WEFT_S3_SERVER_VERSION WEFT_S3_SERVER_RELEASE WEFT_S3_SERVER_SHA256 \
  WEFT_S3_CONTAINER_IMAGE WEFT_S3_CONTAINER_IMAGE_ID \
  WEFT_COMPAT_STAMP WEFT_COMPAT_RECEIPT WEFT_CEPH_SERVER_LABEL \
  WEFT_MINIO_BIN WEFT_DIST_STAMP WEFT_DIST_ROOT \
  WEFT_OVERLAY_COURT_BACKEND WEFT_OVERLAY_RECEIPT WEFT_OVERLAY_LOG \
  WEFT_PLANES_STAMP WEFT_PLANES_ROOT WEFT_PACKS_STAMP WEFT_PACKS_ROOT \
  WEFT_GCS_BIN WEFT_GCS_TEST_ENDPOINT WEFT_GCS_TEST_BUCKET WEFT_GCS_PROJECT \
  WEFT_GCS_ANONYMOUS WEFT_GCS_LOCK_LEASE_SECS WEFT_GCS_FENCE_LEASE_SECS \
  WEFT_GCS_RECEIPT WEFT_GCS_TESTS_LOG WEFT_GCS_FENCING_LOG WEFT_GCS_STAMP \
  WEFT_GCS_SERVER_LABEL WEFT_GCS_SERVER_VERSION WEFT_GCS_SERVER_SHA256 \
  WEFT_GCS_CREDENTIALS WEFT_GCS_LIVE_PROBE WEFT_GCS_SCOPE \
  WEFT_3FS_LIB WEFT_3FS_SHIM WEFT_3FS_SHIM_DIR WEFT_THREEFS_REPS \
  WEFT_THREEFS_RECEIPT WEFT_THREEFS_LOG WEFT_STAMP \
  WEFT_TRAJ_SYNTH_STEPS WEFT_TRAJ_PYTHIA_STEPS WEFT_TRAJ_QWEN \
  WEFT_TRAJ_INDEX_MODE WEFT_TRAJ_BASELINES WEFT_TRAJ_QUERY_REOPEN \
  WEFT_TRAJ_INDEX_MAX_DELTAS WEFT_TRAJ_REAL_WEIGHTS WEFT_TRAJECTORY_RECEIPT \
  WEFT_FAMILY_FAMILIES WEFT_FAMILY_CONFIGS WEFT_FAMILY_INGEST_REPS \
  WEFT_FAMILY_MEASURE_REPS WEFT_FAMILY_TOPOLOGY_REPS \
  WEFT_FAMILY_RECOVERY_REPS WEFT_FAMILY_S3 WEFT_FAMILY_S3_PORT \
  WEFT_FAMILY_GC_LIMIT WEFT_FAMILY_TILE_BYTES WEFT_FAMILY_SKIP_FAMILY \
  WEFT_FAMILY_LABEL WEFT_FAMILY_QWEN3B_BASE WEFT_FAMILY_QWEN3B_NEXT \
  WEFT_FAMILY_RECEIPT WEFT_FAMILY_LOG \
  WEFT_AUGMENTATION_STAMP WEFT_AUG_MINIO_RESTART WEFT_AUGMENTATION_REDUCED \
  WEFT_AUGMENTATION_KEEP_WORK WEFT_AUG_ENCODE_THREADS WEFT_AUG_TRACE \
  WEFT_PLANNER_STAMP WEFT_PLANNER_MATRIX_REPS WEFT_PLANNER_OFFICIAL_RECEIPT \
  WEFT_MODE9_STAMP WEFT_MODE9_RECEIPT WEFT_MODE9_THREADS \
  WEFT_R19_STAMP WEFT_R19_LABEL WEFT_R19_REPS WEFT_R19_CONFIGS \
  WEFT_R20_STAMP WEFT_R20_REPS WEFT_R20_CONFIGS \
  WEFT_R21_STAMP WEFT_R21_REPS WEFT_R21_CONFIGS \
  WEFT_R22_STAMP WEFT_R22_REPS WEFT_R22_CONFIGS WEFT_R22_PROBE_STAMP \
  WEFT_R22_FAMILIES WEFT_R22_FAMILY_ROWS \
  WEFT_GOODPUT_STAMP WEFT_GOODPUT_RECEIPT WEFT_GOODPUT_WORK \
  WEFT_LATENCY_STAMP WEFT_LATENCY_RECEIPT WEFT_LATENCY_WORK \
  WEFT_LATENCY_REPS WEFT_LATENCY_FRESH \
  WEFT_GOODPUT_STEPS WEFT_GOODPUT_CKPT_EVERY WEFT_GOODPUT_REPS \
  WEFT_GOODPUT_KILL_REPS WEFT_GOODPUT_KILL_STEPS \
  WEFT_GOODPUT_KILL_AFTER_ACK_STEP WEFT_GOODPUT_KILL_BEFORE_ACK_STEP \
  WEFT_GOODPUT_SWEEP_STEPS WEFT_GOODPUT_SWEEP_REPS WEFT_GOODPUT_INTERVALS \
  WEFT_GOODPUT_CONFIGS WEFT_GOODPUT_RESUME WEFT_GOODPUT_SESSION_IDLE_SECS \
  WEFT_QUEUE_STAMP WEFT_QUEUE_RECEIPT WEFT_QUEUE_WORK WEFT_QUEUE_SECTION \
  WEFT_QUEUE_PHASE WEFT_QUEUE_STEPS WEFT_QUEUE_REPS WEFT_QUEUE_INTERVALS \
  WEFT_QUEUE_CONFIGS WEFT_QUEUE_RESUME WEFT_QUEUE_BOUNDS WEFT_QUEUE_COUNTS \
  WEFT_QUEUE_MAX_OUTSTANDING WEFT_QUEUE_MAX_STAGED_BYTES WEFT_QUEUE_POLICY \
  WEFT_QUEUE_WAIT_MS WEFT_QUEUE_SESSION_IDLE_SECS \
  WEFT_DCPMETA_STAMP WEFT_DCPMETA_RECEIPT WEFT_DCPMETA_WORK \
  WEFT_DCPMETA_SIZES WEFT_DCPMETA_ITERATIONS \
  WEFT_DCP_METADATA_ENCODING \
  WEFTFS_CLI \
  WEFT_RETENTION_STAMP WEFT_RETENTION_TICKS WEFT_RETENTION_KEEP_RUNS WEFT_RETENTION_KEEP \
  WEFT_AZURE_TEST_ENDPOINT WEFT_AZURE_TEST_CONTAINER WEFT_AZURE_ACCOUNT \
  WEFT_AZURE_KEY WEFT_AZURE_SAS WEFT_AZURE_ANONYMOUS \
  WEFT_AZURE_LOCK_LEASE_SECS WEFT_AZURE_FENCE_LEASE_SECS \
  WEFT_AZURE_RECEIPT WEFT_AZURE_TESTS_LOG WEFT_AZURE_FENCING_LOG \
  WEFT_AZURE_STAMP WEFT_AZURE_SERVER_LABEL WEFT_AZURE_SERVER_VERSION \
  WEFT_AZURE_SERVER_DIGEST WEFT_AZURE_ENDPOINT WEFT_AZURE_CONTAINER \
  AZURE_STORAGE_ACCOUNT AZURE_STORAGE_KEY AZURE_STORAGE_SAS_TOKEN \
  AWS_ENDPOINT_URL AWS_ENDPOINT_URL_S3 AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY \
  AWS_SESSION_TOKEN AWS_REGION AWS_DEFAULT_REGION; do
  if [[ -n "${!name:-}" ]]; then
    common+=(-e "$name=${!name}")
  fi
done

if [[ -n "$WEFT_DOCKER_NETWORK" ]]; then
  common+=(--network "$WEFT_DOCKER_NETWORK")
fi

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

# Virtualisation court: KVM is the only addition. The QEMU power-loss runner
# detects a missing/unusable /dev/kvm and records a TCG fallback in its receipt;
# no host file is written by the guest except the court's own cache/receipts.
if [[ "$kind" == "vm" ]]; then
  if [[ -e /dev/kvm ]]; then
    common+=(--device /dev/kvm)
  fi
fi

# Bound in-container scratch and compiler concurrency as a second line of defense.
guarded_cmd="set -Eeuo pipefail; ulimit -n 65536; ulimit -u ${WEFT_NPROC}; export TMPDIR=/tmp; cd /work; ${cmd}"

set -x
exec docker run "${common[@]}" "$image" bash -c "$guarded_cmd"
