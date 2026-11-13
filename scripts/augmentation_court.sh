#!/usr/bin/env bash
# Same-backend augmentation court (subphase R3).
#
# One invocation runs configurations A/B/C/D/E (plus the supplementary native +
# application-level baseline N) over ONE backend and writes one
# `weft.augmentation.v1` receipt plus a raw log. The frozen methodology is
# `bench/CONTRACTS-augmentation.md`.
#
# Usage:
#   scripts/augmentation_court.sh --local            # local POSIX backend
#   scripts/augmentation_court.sh --minio            # a real MinIO server
#   scripts/augmentation_court.sh --local --reduced  # aggregate-court subset
#
# `--reduced` (or WEFT_AUGMENTATION_REDUCED=1) selects 16 evenly spaced
# non-embedding tensors, no embedding, all five A-E rows and N, two restore
# passes. It is recorded in the receipt. The full standalone run selects the
# 272 MB embedding plus 100 evenly spaced tensors (424,439,296 logical bytes).
#
# `--reps N` (or WEFT_AUGMENTATION_REPS, default 7) runs every arm N times and
# **interleaves** the arms, rotating the order by one position per repetition so
# no arm always occupies the same slot. Every repetition's raw phase JSON is
# published in the receipt and min/p50/p95/max is reported per mandatory metric.
# `--configs A,C` (or WEFT_AUGMENTATION_CONFIGS) narrows the row list; the MinIO
# arm uses it because the MinIO tmpfs holds six full stores poorly.
#
# Every container uses the same hard limits as `scripts/docker-court.sh`
# (memory 3g == swap, pids 1024, cpus 4, bounded /tmp, no-new-privileges,
# cap-drop=ALL). The MinIO data directory is a tmpfs, so this court proves
# protocol, codec, and client-visible cost, not media or power-loss durability.
#
# Environment skips print exactly one `augmentation_court: skip <reason>` line
# and exit 78. The aggregate verification court records that as `skip`, never
# as a pass.
set -Eeuo pipefail

skip() {
  echo "augmentation_court: skip $1" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

mode="local"
reduced="${WEFT_AUGMENTATION_REDUCED:-0}"
keep_work="${WEFT_AUGMENTATION_KEEP_WORK:-0}"
reps="${WEFT_AUGMENTATION_REPS:-7}"
configs="${WEFT_AUGMENTATION_CONFIGS:-}"
for arg in "$@"; do
  case "$arg" in
    --local) mode="local" ;;
    --minio) mode="minio" ;;
    --reduced) reduced=1 ;;
    --keep-work) keep_work=1 ;;
    --reps=*) reps="${arg#--reps=}" ;;
    --reps) echo "--reps needs a value (use --reps=N)" >&2; exit 64 ;;
    --configs=*) configs="${arg#--configs=}" ;;
    --configs) echo "--configs needs a value (use --configs=A,C)" >&2; exit 64 ;;
    -h|--help)
      sed -n '2,40p' "$0"
      exit 0
      ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done
case "$reps" in
  ''|*[!0-9]*) echo "--reps must be a positive integer" >&2; exit 64 ;;
esac
[[ "$reps" -ge 1 ]] || { echo "--reps must be a positive integer" >&2; exit 64; }
if [[ -z "$configs" ]]; then
  configs="A,B,C,D,E,N"
fi

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

fixture="$repo_root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors"
[[ -f "$fixture" ]] || skip "fixture $fixture is not present (the court refuses to substitute a different container)"

if [[ "$mode" == "minio" ]]; then
  [[ "$(uname -m)" == "x86_64" ]] || skip "only the verified linux-amd64 MinIO release is supported (host is $(uname -m))"
  command -v curl >/dev/null 2>&1 || skip "curl is required to fetch the pinned MinIO release"
fi

stamp="${WEFT_AUGMENTATION_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
export WEFT_AUGMENTATION_STAMP="$stamp"
if [[ "$reduced" == "1" ]]; then
  max_others=16
  include_embedding=0
  label="reduced"
else
  max_others=100
  include_embedding=1
  label="full"
fi

work="$repo_root/.docker-cache/augmentation-$mode-$stamp"
receipt="bench/receipts/augmentation-$mode-$stamp.json"
log="bench/receipts/augmentation-$mode-$stamp.log"
trace_env=""
if [[ -n "${WEFT_AUG_TRACE:-}" ]]; then
  trace_env="WEFT_AUG_TRACE=1"
fi
mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

device_receipt="bench/receipts/augmentation-$mode-$stamp.device.json"

# Device control: the same 200 MiB `dd conv=fsync` probe the R22 Pareto court
# records. A durable-write row is device-bound, so the fsynced throughput of the
# volume this court actually writes to is part of the environment. It runs in
# the same court image with the same hard limits, before the measured matrix.
echo "augmentation_court: device control -> $device_receipt" >&2
scripts/docker-court.sh bench \
  "cd /work && python3 bench/scripts/augmentation_device_control.py $device_receipt $stamp"

if [[ "$mode" == "local" ]]; then
  echo "augmentation_court: local POSIX backend, label=$label, subset=max_others:$max_others include_embedding:$include_embedding reps=$reps configs=$configs" >&2
  scripts/docker-court.sh bench \
    "cd /work && $trace_env cargo run --release --quiet --example augmentation_bench -- run \
       --fixture /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
       --work /work/.docker-cache/augmentation-local-$stamp \
       --out $receipt --log $log \
       --backend local --chunk-size 1048576 \
       --max-others $max_others --include-embedding $include_embedding \
       --configs $configs --reps $reps \
       --with-native-app --passes 2 --label $label --seed weft-augmentation-r3 \
       --force"
else
  minio_release="${WEFT_S3_SERVER_RELEASE:-RELEASE.2025-04-22T22-12-26Z}"
  minio_sha256="${WEFT_S3_SERVER_SHA256:-53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c}"
  minio_dir="$repo_root/.docker-cache/minio"
  minio_bin="$minio_dir/minio"
  minio_url="https://github.com/minio/minio/releases/download/${minio_release}/minio.linux-amd64.${minio_release}"
  mkdir -p "$minio_dir"
  if [[ ! -f "$minio_bin" ]]; then
    echo "augmentation_court: downloading official MinIO binary $minio_release" >&2
    if ! curl -fL --retry 3 -o "$minio_bin.part" "$minio_url"; then
      rm -f "$minio_bin.part"
      skip "cannot obtain the pinned MinIO release $minio_release"
    fi
    mv "$minio_bin.part" "$minio_bin"
  fi
  chmod +x "$minio_bin"
  actual_sha256="$(sha256sum "$minio_bin" | awk '{print $1}')"
  if [[ "$actual_sha256" != "$minio_sha256" ]]; then
    echo "augmentation_court: MinIO binary sha256 mismatch (expected $minio_sha256, got $actual_sha256)" >&2
    exit 1
  fi
  minio_version="$("$minio_bin" --version | head -1)"

  container_image="debian:bookworm-slim"
  if ! docker image inspect "$container_image" >/dev/null 2>&1; then
    if ! docker pull "$container_image" >/dev/null; then
      skip "cannot obtain the pinned $container_image court image"
    fi
  fi
  container_image_digest="$(docker image inspect "$container_image" --format '{{index .RepoDigests 0}}')"

  suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"
  access_key="${WEFT_S3_ACCESS_KEY:-weftcourt}"
  secret_key="${WEFT_S3_SECRET_KEY:-weftcourt-secret}"
  minio_name="weft-aug-minio-$$"
  network="weft-aug-net-$$"
  bucket="weft-augmentation-$suffix"

  cleanup() {
    docker rm -f "$minio_name" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT INT TERM

  if ! docker network create "$network" >/dev/null 2>&1; then
    skip "cannot create the court Docker network $network"
  fi
  WEFT_MEMORY="${WEFT_MEMORY:-3g}"
  WEFT_CPUS="${WEFT_CPUS:-4}"
  WEFT_PIDS="${WEFT_PIDS:-1024}"
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
    echo "augmentation_court: MinIO did not become ready; logs follow" >&2
    docker logs "$minio_name" >&2 || true
    exit 1
  fi

  export WEFT_DOCKER_NETWORK="$network"
  export WEFT_S3_ENDPOINT="http://${minio_name}:9000"
  export WEFT_S3_BUCKET="$bucket"
  export WEFT_S3_REGION="${WEFT_S3_REGION:-us-east-1}"
  export WEFT_S3_ACCESS_KEY="$access_key"
  export WEFT_S3_SECRET_KEY="$secret_key"
  export WEFT_S3_LOCK_LEASE_SECS="${WEFT_S3_LOCK_LEASE_SECS:-900}"
  export WEFT_S3_SERVER_LABEL="$minio_version"
  export WEFT_S3_SERVER_RELEASE="$minio_release"
  export WEFT_S3_SERVER_SHA256="$actual_sha256"
  export WEFT_S3_CONTAINER_IMAGE="$container_image_digest"
  export WEFT_AUG_MINIO_RESTART=1

  work_container="/work/.docker-cache/augmentation-minio-$stamp"
  mkdir -p "$work/phases"

  wait_minio() {
    local port="$1"
    for _ in $(seq 1 60); do
      if curl -sf "http://127.0.0.1:${port}/minio/health/ready" >/dev/null 2>&1; then
        return 0
      fi
      sleep 0.5
    done
    return 1
  }

  run_harness() {
    local name="$1"
    shift
    echo "augmentation_court: $name" >&2
    if ! scripts/docker-court.sh bench \
      "cd /work && $trace_env cargo run --release --quiet --example augmentation_bench -- $*" \
      >"$work/phases/$name.log" 2>&1; then
      echo "augmentation_court: $name failed; tail of $work/phases/$name.log follows" >&2
      tail -40 "$work/phases/$name.log" >&2 || true
      exit 1
    fi
  }

  echo "augmentation_court: MinIO $minio_release sha256=$actual_sha256 bucket_prefix=$bucket label=$label reps=$reps configs=$configs" >&2
  echo "augmentation_court: the MinIO container is restarted between cells; its tmpfs does not release deleted-but-open bytes until the process restarts, so one container cannot hold several clean configurations. Write and restore for one cell both happen before its restart." >&2

  run_harness prepare prepare \
    --fixture /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
    --work "$work_container" \
    --max-others "$max_others" --include-embedding "$include_embedding"

  IFS=',' read -r -a config_list <<<"$configs"
  for ((rep = 1; rep <= reps; rep++)); do
    order=("${config_list[@]}")
    rotation=$(( (rep - 1) % ${#config_list[@]} ))
    # Rotate the interleaved order by one position per repetition.
    order=("${order[@]:rotation}" "${order[@]:0:rotation}")
    rep_container="$work_container/rep-$(printf '%02d' "$rep")"
    mkdir -p "$work/phases" "$work/rep-$(printf '%02d' "$rep")/phases"
    for config in "${order[@]}"; do
      lower="$(printf '%s' "$config" | tr '[:upper:]' '[:lower:]')"
      cbucket="$bucket-$lower"
      run_harness "r$rep-$config-write" write \
        --work "$rep_container" --subset "$work_container/subset.json" \
        --config "$config" --backend s3 --s3-bucket "$cbucket" \
        --phase-out "$rep_container/phases/$config-write.json" \
        --chunk-size 1048576 --seed weft-augmentation-r3 --passes 2
      run_harness "r$rep-$config-restore" restore \
        --work "$rep_container" --subset "$work_container/subset.json" \
        --config "$config" --backend s3 --s3-bucket "$cbucket" \
        --phase-out "$rep_container/phases/$config-restore.json" \
        --chunk-size 1048576 --seed weft-augmentation-r3 --passes 2
      echo "augmentation_court: restarting MinIO to reclaim the tmpfs before the next cell" >&2
      docker restart "$minio_name" >/dev/null
      # Docker may publish a new ephemeral host port after a restart; re-read it.
      host_port_line="$(docker port "$minio_name" 9000/tcp | head -1)"
      host_port="${host_port_line##*:}"
      if ! wait_minio "$host_port"; then
        echo "augmentation_court: MinIO did not become ready after restart; logs follow" >&2
        docker logs "$minio_name" >&2 || true
        exit 1
      fi
    done
  done

  run_harness assemble assemble \
    --work "$work_container" --out "$receipt" --log "$log" \
    --backend s3 --label "$label" \
    --max-others "$max_others" --include-embedding "$include_embedding" \
    --configs "$configs" --reps "$reps" \
    --with-native-app --passes 2 --seed weft-augmentation-r3
fi

[[ -f "$receipt" ]] || { echo "augmentation_court: expected receipt $receipt was not written" >&2; exit 1; }
result="$(jq -r '.result' "$receipt")"
if [[ "$result" != "pass" ]]; then
  echo "augmentation_court: receipt result is '$result' ($receipt)" >&2
  exit 1
fi
echo "augmentation_court: receipt $receipt (result=$result)" >&2
echo "augmentation_court: log $log" >&2

if [[ "$keep_work" != "1" ]]; then
  rm -rf "$work" 2>/dev/null || true
fi
