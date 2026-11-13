#!/usr/bin/env bash
# Context Advantage Court (R26, extended by R29 with a second backend).
#
# One backend at a time, used three ways:
#   N  backend alone, no metadata at all;
#   M  backend + a strong application-level metadata system (zstd-3 blob,
#      canonical manifest, key->value index on the brief's five dimensions plus
#      position, full value validation before reuse, TTL/eviction, own subset
#      selection);
#   W  backend + the WeftFS context plane (provenance-bound KV pages, mode 19,
#      capability-scoped reads, weft.checkpoint.v1 manifests, causal closure).
#
# The frozen methodology is `bench/CONTRACTS-context-advantage.md` (version 3),
# committed before this court produced any measurement. Every arm writes the
# same logical objects through the same commit protocol with the same chunk
# size, the same barrier the backend declares, no decoded-object cache, no
# single-flight, one thread and one request at a time.
#
# Usage:
#   scripts/context_advantage_court.sh                  # 7 interleaved reps, local POSIX
#   scripts/context_advantage_court.sh --backend s3     # 7 reps against one loopback MinIO
#   scripts/context_advantage_court.sh --reduced        # 3 reps, aggregate court
#   scripts/context_advantage_court.sh --reps=5
#   scripts/context_advantage_court.sh --keep-work
#
# `--backend s3` starts one pinned MinIO release in a sibling container on a
# private bridge network, with a tmpfs data directory and the same hard limits
# as every other court. It is a single-node loopback server: not a cloud,
# multi-node, media or power-loss claim. The S3 backend honours no local store
# root prefix, so the harness derives one bucket per (arm, repetition, store)
# from WEFT_S3_BUCKET; a bucket is the isolation unit a directory is locally.
#
# Every repetition runs each arm in a fresh process against a fresh store. The
# arm order rotates by one position per repetition, so no arm always occupies
# the same slot; the order of every repetition is recorded in the receipt.
#
# Every container uses the same hard limits as `scripts/docker-court.sh`
# (memory 3g == swap, pids 1024, cpus 4, bounded /tmp, no-new-privileges,
# cap-drop=ALL, nproc 2048). Nothing here is a throughput, media, power-loss or
# fleet claim; see the contract §7.
#
# Environment skips print exactly one `context_advantage_court: skip <reason>`
# line and exit 78. The aggregate verification court records that as `skip`,
# never as a pass.
set -Eeuo pipefail

skip() {
  echo "context_advantage_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

reps="${WEFT_CONTEXT_ADVANTAGE_REPS:-7}"
reduced=0
keep_work=0
backend="local"
for arg in "$@"; do
  case "$arg" in
    --reduced) reduced=1 ;;
    --keep-work) keep_work=1 ;;
    --backend=*) backend="${arg#--backend=}" ;;
    --backend) echo "--backend needs a value (use --backend=local|s3)" >&2; exit 64 ;;
    --reps=*) reps="${arg#--reps=}" ;;
    --reps) echo "--reps needs a value (use --reps=N)" >&2; exit 64 ;;
    -h|--help)
      sed -n '2,40p' "$0"
      exit 0
      ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done
case "$backend" in
  local|s3) ;;
  *) echo "--backend must be local or s3 (got '$backend')" >&2; exit 64 ;;
esac
case "$reps" in
  ''|*[!0-9]*) echo "--reps must be a positive integer" >&2; exit 64 ;;
esac
[[ "$reps" -ge 1 ]] || { echo "--reps must be a positive integer" >&2; exit 64; }
if [[ "$reduced" == "1" ]]; then
  reps=3
  label="reduced"
else
  label="${WEFT_CONTEXT_ADVANTAGE_LABEL:-full}"
fi

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

stamp="${WEFT_CONTEXT_ADVANTAGE_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_CONTEXT_ADVANTAGE_STAMP="$stamp"

suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"
if [[ "$backend" == "s3" ]]; then
  work="$repo_root/.docker-cache/context-advantage-s3-$stamp"
  receipt="bench/receipts/context-advantage-s3-$stamp.json"
  log="bench/receipts/context-advantage-s3-$stamp.log"
else
  work="$repo_root/.docker-cache/context-advantage-$stamp"
  receipt="bench/receipts/context-advantage-$stamp.json"
  log="bench/receipts/context-advantage-$stamp.log"
fi
mkdir -p "$work" bench/receipts
chmod a+rwX "$work" bench/receipts 2>/dev/null || true

minio_name=""
network=""
cleanup_s3() {
  if [[ -n "$minio_name" ]]; then
    docker rm -f "$minio_name" >/dev/null 2>&1 || true
  fi
  if [[ -n "$network" ]]; then
    docker network rm "$network" >/dev/null 2>&1 || true
  fi
}
trap cleanup_s3 EXIT INT TERM

if [[ "$backend" == "s3" ]]; then
  # ---- The second backend: one pinned MinIO release, loopback, tmpfs data.
  [[ "$(uname -m)" == "x86_64" ]] || skip \
    "only the verified linux-amd64 MinIO release is supported (host is $(uname -m))"
  command -v curl >/dev/null 2>&1 || skip "curl is required to fetch the pinned MinIO release"

  minio_release="${WEFT_S3_SERVER_RELEASE:-RELEASE.2025-04-22T22-12-26Z}"
  minio_sha256="${WEFT_S3_SERVER_SHA256:-53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c}"
  minio_dir="$repo_root/.docker-cache/minio"
  minio_bin="$minio_dir/minio"
  minio_url="https://github.com/minio/minio/releases/download/${minio_release}/minio.linux-amd64.${minio_release}"
  mkdir -p "$minio_dir"
  if [[ ! -f "$minio_bin" ]]; then
    echo "context_advantage_court: downloading official MinIO binary $minio_release" >&2
    if ! curl -fL --retry 3 -o "$minio_bin.part" "$minio_url"; then
      rm -f "$minio_bin.part"
      skip "cannot obtain the pinned MinIO release $minio_release"
    fi
    mv "$minio_bin.part" "$minio_bin"
  fi
  chmod +x "$minio_bin"
  actual_sha256="$(sha256sum "$minio_bin" | awk '{print $1}')"
  if [[ "$actual_sha256" != "$minio_sha256" ]]; then
    echo "context_advantage_court: MinIO binary sha256 mismatch (expected $minio_sha256, got $actual_sha256)" >&2
    exit 1
  fi
  minio_version="$("$minio_bin" --version | head -1)"

  container_image="debian:bookworm-slim"
  docker image inspect "$container_image" >/dev/null 2>&1 || {
    docker pull "$container_image" >/dev/null || skip "cannot obtain the pinned debian:bookworm-slim court image"
  }
  container_image_digest="$(docker image inspect "$container_image" --format '{{index .RepoDigests 0}}')"

  access_key="${WEFT_S3_ACCESS_KEY:-weftcourt}"
  secret_key="${WEFT_S3_SECRET_KEY:-weftcourt-secret}"
  minio_name="weft-ca-minio-$$"
  network="weft-ca-net-$$"

  docker network create "$network" >/dev/null 2>&1 \
    || skip "cannot create the court Docker network $network"
  docker run -d --name "$minio_name" \
    --memory=3g --memory-swap=3g \
    --pids-limit=1024 --cpus=4 \
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
    echo "context_advantage_court: MinIO did not become ready; logs follow" >&2
    docker logs "$minio_name" >&2 || true
    exit 1
  fi

  export WEFT_DOCKER_NETWORK="$network"
  export WEFT_S3_ENDPOINT="http://${minio_name}:9000"
  # One base bucket name; the harness appends -r<rep><arm><store>.
  export WEFT_S3_BUCKET="weft-ca-$suffix"
  export WEFT_S3_REGION="${WEFT_S3_REGION:-us-east-1}"
  export WEFT_S3_ACCESS_KEY="$access_key"
  export WEFT_S3_SECRET_KEY="$secret_key"
  # The writer lock is released on every store drop and re-acquired after; a
  # long lease only avoids tripping the idle-expiry rule between phases. It is
  # recorded in the receipt's backend block.
  export WEFT_S3_LOCK_LEASE_SECS=900
  export WEFT_S3_SERVER_PRODUCT="MinIO"
  export WEFT_S3_SERVER_LABEL="$minio_version"
  export WEFT_S3_SERVER_VERSION="$minio_release"
  export WEFT_S3_SERVER_RELEASE="$minio_release"
  export WEFT_S3_SERVER_SHA256="$actual_sha256"
  export WEFT_S3_CONTAINER_IMAGE="$container_image_digest"

  echo "context_advantage_court: backend=s3 minio $minio_release sha256=$actual_sha256 host_port=$host_port" >&2
fi

arms=(N M W)
phase_flag=""
if [[ "$reduced" == "1" ]]; then
  phase_flag="--reduced"
fi

echo "context_advantage_court: label=$label reps=$reps stamp=$stamp backend=$backend" >&2
echo "context_advantage_court: one backend ($backend), three arms, interleaved" >&2

for ((rep = 1; rep <= reps; rep++)); do
  rotation=$(( (rep - 1) % ${#arms[@]} ))
  order=("${arms[@]:rotation}" "${arms[@]:0:rotation}")
  joined="$(IFS=,; echo "${order[*]}")"
  rep_dir="$work/rep-$(printf '%02d' "$rep")"
  mkdir -p "$rep_dir"
  order_index=0
  for arm in "${order[@]}"; do
    order_index=$((order_index + 1))
    echo "context_advantage_court: rep $rep/$reps arm $arm order=$joined slot=$order_index" >&2
    if ! scripts/docker-court.sh bench \
      "cd /work && WEFT_CONTEXT_ADVANTAGE_STAMP=$stamp WEFT_CONTEXT_ADVANTAGE_LABEL=$label cargo run --release --quiet --example context_advantage_court -- run \
         --arm $arm \
         --backend $backend \
         --work /work/.docker-cache/$(basename "$work")/rep-$(printf '%02d' "$rep") \
         --out /work/.docker-cache/$(basename "$work")/rep-$(printf '%02d' "$rep")/$arm.json \
         --rep $rep --order $joined --order-index $order_index --reps $reps $phase_flag" \
      >>"$work/run.log" 2>&1; then
      echo "context_advantage_court: rep $rep arm $arm failed; tail of $work/run.log follows" >&2
      tail -40 "$work/run.log" >&2 || true
      exit 1
    fi
  done
done

echo "context_advantage_court: assembling $receipt" >&2
if ! scripts/docker-court.sh bench \
  "cd /work && WEFT_CONTEXT_ADVANTAGE_STAMP=$stamp WEFT_CONTEXT_ADVANTAGE_LABEL=$label cargo run --release --quiet --example context_advantage_court -- assemble \
     --backend $backend \
     --work /work/.docker-cache/$(basename "$work") \
     --out /work/$receipt --log /work/$log --reps $reps $phase_flag" \
  >>"$work/run.log" 2>&1; then
  echo "context_advantage_court: assemble failed; tail of $work/run.log follows" >&2
  tail -40 "$work/run.log" >&2 || true
  exit 1
fi

[[ -f "$receipt" ]] || { echo "context_advantage_court: expected receipt $receipt was not written" >&2; exit 1; }
result="$(jq -r '.result' "$receipt")"
claim="$(jq -r '.claim_holds' "$receipt")"
echo "context_advantage_court: receipt $receipt (result=$result claim_holds=$claim label=$label reps=$reps backend=$backend)" >&2
echo "context_advantage_court: log $log" >&2

if [[ "$keep_work" != "1" ]]; then
  rm -rf "$work" 2>/dev/null || true
fi

if [[ "$result" != "pass" ]]; then
  echo "context_advantage_court: measurement was not valid (result=$result); see $receipt" >&2
  exit 1
fi
