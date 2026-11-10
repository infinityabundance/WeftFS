#!/usr/bin/env bash
# R12 increment 2 — one fleet cold-start configuration inside the bench court.
#
#   bash bench/scripts/fleet_coldstart_inner.sh <build|with|without> <N> <work> <stamp>
#
# `build` starts a real MinIO server on loopback, creates the bucket, and ingests
# the real SmolLM-135M-Instruct tensors into one S3-backed store.
# `with` / `without` start the same MinIO over the same data directory, start one
# shared read-only server with N FUSE views (single-flight + decoded-object cache
# enabled, or both disabled), then start N replica processes that each read the
# whole model through their own view.
#
# Everything runs in one container so the server, its views, MinIO and the
# replicas share one cgroup and one pids limit. Every hard limit is set by
# scripts/docker-court.sh; this script never changes one.
set -Eeuo pipefail

phase="$1"
n="$2"
root_work="$3"
stamp="$4"
# Artifacts for this phase live under $root_work/<phase>; the MinIO data
# directory is shared by every phase so the store built once is reopened by
# both configurations.
work="$root_work/$phase"
minio_data="$root_work/minio-data"

bin=/work/.docker-cache/target-rust/release/examples/fleet_coldstart
scale=/work/.docker-cache/target-rust/release/examples/scale_7b
minio_bin=/work/.docker-cache/minio/minio
corpus=/work/.docker-cache/real-corpus/SmolLM-135M-Instruct.safetensors
corpus_sha=c1464c9ebccc6fb497aa4ccc8d74d29b33f9a9eddc4638a4575719357f532f2d

export WEFT_S3_ENDPOINT=http://127.0.0.1:9000
export WEFT_S3_BUCKET=weft-fleet
export WEFT_S3_REGION=us-east-1
export WEFT_S3_ACCESS_KEY=weftfleet
export WEFT_S3_SECRET_KEY=weftfleet-secret
# MinIO only accepts the credentials it was started with.
export MINIO_ROOT_USER="$WEFT_S3_ACCESS_KEY"
export MINIO_ROOT_PASSWORD="$WEFT_S3_SECRET_KEY"

mkdir -p "$work" "$minio_data"
chmod a+rwX "$root_work" "$work" "$minio_data" 2>/dev/null || true

echo "fleet_coldstart_inner: phase=$phase N=$n work=$work"
echo "fleet_coldstart_inner: corpus sha256=$corpus_sha"

# --- MinIO ------------------------------------------------------------------
"$minio_bin" server "$minio_data" --address 127.0.0.1:9000 \
  --console-address 127.0.0.1:9001 >"$work/minio.log" 2>&1 &
minio_pid=$!
cleanup() { kill "$minio_pid" 2>/dev/null || true; }
trap cleanup EXIT

ready=0
for _ in $(seq 1 600); do
  if (exec 3<>/dev/tcp/127.0.0.1/9000) 2>/dev/null; then ready=1; break; fi
  if ! kill -0 "$minio_pid" 2>/dev/null; then break; fi
  sleep 0.1
done
if [[ "$ready" != 1 ]]; then
  echo "fleet_coldstart_inner: MinIO did not become ready" >&2
  tail -5 "$work/minio.log" >&2 || true
  exit 1
fi
echo "fleet_coldstart_inner: MinIO up pid=$minio_pid"

if [[ "$phase" == "build" ]]; then
  env | grep -E '^WEFT_(COURT|REPO)_' >"$work/env.txt" || true
  if [[ ! -f "$corpus" ]]; then
    echo "fleet_coldstart_inner: missing corpus $corpus" >&2
    exit 1
  fi
  actual="$(sha256sum "$corpus" | awk '{print $1}')"
  if [[ "$actual" != "$corpus_sha" ]]; then
    echo "fleet_coldstart_inner: corpus sha256 mismatch ($actual != $corpus_sha)" >&2
    exit 1
  fi
  "$scale" ingest --backend s3 --source "$corpus" --source-sha256 "$corpus_sha" \
    --store fleet --out "$work/ingest.json" >"$work/ingest.stdout" 2>"$work/ingest.stderr"
  echo "fleet_coldstart_inner: store built"
  cat "$work/ingest.json"
  exit 0
fi

if [[ "$phase" != "with" && "$phase" != "without" ]]; then
  echo "fleet_coldstart_inner: unknown phase $phase" >&2
  exit 64
fi

# --- layout -----------------------------------------------------------------
rm -rf "$work/mounts" "$work/done" "$work/replicas" "$work/ready" "$work/go"
mkdir -p "$work/mounts" "$work/done" "$work/replicas"
: >"$work/mounts.txt"
for i in $(seq 0 $((n - 1))); do
  v="$(printf 'v%03d' "$i")"
  mkdir -p "$work/mounts/$v"
  echo "$work/mounts/$v" >>"$work/mounts.txt"
done

if [[ "$phase" == "with" ]]; then
  sf=(--single-flight on --cache-bytes 134217728 --cache-entries 4096)
else
  sf=(--single-flight off --cache-bytes 0)
fi

# --- shared server ----------------------------------------------------------
"$bin" server --store fleet --backend s3 --mounts "$work/mounts.txt" \
  --expect-replicas "$n" --done-dir "$work/done" --ready-file "$work/ready" \
  --stats-out "$work/server.json" --timeout-secs 900 --label "$phase" "${sf[@]}" \
  >"$work/server.stdout" 2>"$work/server.stderr" &
server_pid=$!

for _ in $(seq 1 600); do
  [[ -f "$work/ready" ]] && break
  if ! kill -0 "$server_pid" 2>/dev/null; then break; fi
  sleep 0.1
done
if [[ ! -f "$work/ready" ]]; then
  echo "fleet_coldstart_inner: server never became ready" >&2
  tail -20 "$work/server.stderr" >&2 || true
  exit 1
fi
server_ready_ns="$(cat "$work/ready")"
echo "fleet_coldstart_inner: server ready at $server_ready_ns"

# --- replicas ---------------------------------------------------------------
replica_pids=()
for i in $(seq 0 $((n - 1))); do
  v="$(printf 'v%03d' "$i")"
  "$bin" replica --mount "$work/mounts/$v" --dir model --go-file "$work/go" \
    --done-file "$work/done/r$i.done" --result-out "$work/replicas/r$i.json" \
    --label "$phase-$i" >"$work/replicas/r$i.stdout" 2>"$work/replicas/r$i.stderr" &
  replica_pids+=("$!")
done

sleep 1
date +%s%N >"$work/go"
echo "fleet_coldstart_inner: go signal published"

# Wait for the server (it exits when every replica marker exists), bounded.
deadline=$(( $(date +%s) + 900 ))
while kill -0 "$server_pid" 2>/dev/null; do
  if [[ "$(date +%s)" -ge "$deadline" ]]; then
    echo "fleet_coldstart_inner: server did not finish within 900 s" >&2
    kill "$server_pid" 2>/dev/null || true
    break
  fi
  sleep 0.2
done
wait "$server_pid" || true
# Wait only for the replicas: MinIO is a long-lived child and a bare `wait`
# would block on it forever.
for pid in "${replica_pids[@]}"; do wait "$pid" || true; done

# --- evidence ---------------------------------------------------------------
{
  echo "memory.peak=$(cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo unknown)"
  echo "memory.events:"; cat /sys/fs/cgroup/memory.events 2>/dev/null || true
  echo "pids.peak=$(cat /sys/fs/cgroup/pids.peak 2>/dev/null || echo unknown)"
  echo "pids.events:"; cat /sys/fs/cgroup/pids.events 2>/dev/null || true
} >"$work/events.txt" 2>&1

done_count=$(find "$work/done" -name '*.done' 2>/dev/null | wc -l)
result_count=$(find "$work/replicas" -name 'r*.json' 2>/dev/null | wc -l)
echo "fleet_coldstart_inner: done=$done_count results=$result_count of $n"
cat "$work/events.txt"

if [[ "$done_count" -ne "$n" ]]; then
  echo "fleet_coldstart_inner: incomplete replica set" >&2
  exit 1
fi
