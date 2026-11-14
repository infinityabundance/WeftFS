#!/usr/bin/env bash
# N04: the R22 bounded encode-and-publish pipeline on a remote object backend,
# with a same-backend A/N/B/C/D/E court and provider-fault adversarial tests.
#
# One invocation:
#   1. runs the frozen augmentation court against a real MinIO server for the
#      six contract arms (A native, N native + application manifest, B protocol
#      pass-through, C codec, D protocol, E combined) **and** the four pipelined
#      rows (B-pipe4/C-pipe4/D-pipe4/E-pipe4) so the pipeline's effect is
#      measured per arm instead of asserted;
#   2. runs the adverse court against a second MinIO server through a byte-level
#      fault proxy: provider abort mid-publish, lost acknowledgement with a
#      retry, and a reader opened before the commits;
#   3. runs the existing S3 fencing suite for "a stale conditional write is
#      refused" against the same server;
#   4. assembles `bench/receipts/n04-object-pipeline-<stamp>.json`.
#
# Durability contract, identical on every arm: a completed write is one the
# provider acknowledged (`PUT` returned). No arm claims an `fsync`; the MinIO
# data directory is a tmpfs, so this is not a media or power-loss claim. A native
# arm's guarantee is "every object PUT returned"; a WeftFS arm's guarantee is
# "every blob PUT returned and the one commit record naming them was accepted",
# which is the same provider-acknowledgement level plus all-or-nothing naming.
#
# Usage:
#   scripts/object_pipeline_court.sh                    # full run
#   scripts/object_pipeline_court.sh --augmentation-only
#   scripts/object_pipeline_court.sh --adverse-only
#   scripts/object_pipeline_court.sh --reps=7 --configs=A,N,B,C,D,E,B-pipe4,C-pipe4,D-pipe4,E-pipe4
#
# Every container uses the same hard limits as `scripts/docker-court.sh`
# (memory 3g == swap, pids 1024, cpus 4, bounded /tmp, no-new-privileges,
# cap-drop=ALL). The MinIO sidecar needs no capability.
#
# Environment refusals print exactly one `object_pipeline_court: skip <reason>`
# line and exit 78; `scripts/verify-all.sh` records that as a skip, never a pass.
set -Eeuo pipefail

skip() {
  echo "object_pipeline_court: skip $*" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

stamp="${WEFT_N04_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_N04_REPS:-7}"
configs="${WEFT_N04_CONFIGS:-}"
augmentation_only=0
adverse_only=0
reduced=0
for arg in "$@"; do
  case "$arg" in
    --augmentation-only) augmentation_only=1 ;;
    --adverse-only) adverse_only=1 ;;
    --reduced) reduced=1 ;;
    --reps=*) reps="${arg#--reps=}" ;;
    --configs=*) configs="${arg#--configs=}" ;;
    -h|--help)
      sed -n '2,40p' "$0"
      exit 0
      ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

if [[ -z "$configs" ]]; then
  if [[ "$reduced" == 1 ]]; then
    # Aggregate-court configuration: the frozen reduced subset (16 evenly
    # spaced non-embedding tensors), the six contract arms with the pipelined
    # WeftFS rows. The full published run uses the 101-tensor subset.
    configs="A,N,B-pipe4,C-pipe4,D-pipe4,E-pipe4"
  else
    configs="A,N,B,C,D,E,B-pipe4,C-pipe4,D-pipe4,E-pipe4"
  fi
fi
if [[ "$reduced" == 1 ]]; then
  export WEFT_AUGMENTATION_REDUCED=1
fi
[[ "$(uname -m)" == "x86_64" ]] || skip "only the verified linux-amd64 MinIO release is supported (host is $(uname -m))"
command -v curl >/dev/null 2>&1 || skip "curl is required to fetch the pinned MinIO release"

fixture="${WEFT_AUG_FIXTURE:-$repo_root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors}"
fixture_sha256="${WEFT_AUG_FIXTURE_SHA256:-fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe}"
[[ -f "$fixture" ]] || skip "fixture $fixture is not present (the court refuses to substitute a different container)"

minio_release="${WEFT_S3_SERVER_RELEASE:-RELEASE.2025-04-22T22-12-26Z}"
minio_sha256="${WEFT_S3_SERVER_SHA256:-53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c}"
minio_dir="$repo_root/.docker-cache/minio"
minio_bin="$minio_dir/minio"
minio_url="https://github.com/minio/minio/releases/download/${minio_release}/minio.linux-amd64.${minio_release}"
mkdir -p "$minio_dir"
if [[ ! -f "$minio_bin" ]]; then
  echo "object_pipeline_court: downloading official MinIO binary $minio_release" >&2
  if ! curl -fL --retry 3 -o "$minio_bin.part" "$minio_url"; then
    rm -f "$minio_bin.part"
    skip "cannot obtain the pinned MinIO release $minio_release"
  fi
  mv "$minio_bin.part" "$minio_bin"
fi
chmod +x "$minio_bin"
actual_sha256="$(sha256sum "$minio_bin" | awk '{print $1}')"
if [[ "$actual_sha256" != "$minio_sha256" ]]; then
  echo "object_pipeline_court: MinIO binary sha256 mismatch (expected $minio_sha256, got $actual_sha256)" >&2
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

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"
subnet="${WEFT_N04_SUBNET:-10.213.88.0/24}"
gateway="${subnet%.*}.1"
minio_ip="${subnet%.*}.10"
access_key="${WEFT_S3_ACCESS_KEY:-weftcourt}"
secret_key="${WEFT_S3_SECRET_KEY:-weftcourt-secret}"

for network_id in $(docker network ls -q); do
  for existing in $(docker network inspect "$network_id" --format '{{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null); do
    if [[ "$existing" == "$subnet" ]]; then
      skip "subnet $subnet is already in use by network $network_id; set WEFT_N04_SUBNET"
    fi
  done
done

mkdir -p bench/receipts
chmod a+rwX bench/receipts 2>/dev/null || true

work="$repo_root/.docker-cache/n04-$stamp"
work_container="/work/.docker-cache/n04-$stamp"
mkdir -p "$work/op" "$work/aug"
log="bench/receipts/n04-object-pipeline-$stamp.log"
: > "$log"
exec > >(tee -a "$log") 2>&1

minio_name="weft-n04-minio-$$"
network="weft-n04-net-$$"
suffix="$(printf '%s' "$stamp" | tr '[:upper:]' '[:lower:]')"

cleanup() {
  docker rm -f "$minio_name" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

start_minio() {
  docker rm -f "$minio_name" >/dev/null 2>&1 || true
  docker run -d --name "$minio_name" \
    --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
    --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
    --ulimit nofile=65536:65536 \
    --security-opt no-new-privileges \
    --cap-drop=ALL \
    --network "$network" --ip "$minio_ip" \
    --tmpfs /data:rw,size=1g,nosuid,nodev \
    -p 127.0.0.1::9000 \
    -e MINIO_ROOT_USER="$access_key" \
    -e MINIO_ROOT_PASSWORD="$secret_key" \
    -v "$minio_bin":/minio:ro \
    "$container_image" \
    /minio server /data --address :9000 --console-address :9001 >/dev/null
  local port
  port="$(docker port "$minio_name" 9000/tcp | head -1)"
  port="${port##*:}"
  for _ in $(seq 1 120); do
    if curl -sf "http://127.0.0.1:${port}/minio/health/ready" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  echo "object_pipeline_court: MinIO did not become ready; logs follow" >&2
  docker logs "$minio_name" >&2 || true
  return 1
}

# ---------------------------------------------------------------------------
# Phase 1: the contract court plus the four pipelined rows
# ---------------------------------------------------------------------------
if [[ "$adverse_only" != 1 ]]; then
  echo "object_pipeline_court: phase 1 (augmentation court, configs=$configs reps=$reps)" >&2
  WEFT_AUGMENTATION_CONFIGS="$configs" \
  WEFT_AUGMENTATION_REPS="$reps" \
  WEFT_AUGMENTATION_STAMP="$stamp" \
    scripts/augmentation_court.sh --minio --reps="$reps" --configs="$configs"
  aug_receipt="bench/receipts/augmentation-minio-$stamp.json"
  [[ -f "$aug_receipt" ]] || { echo "object_pipeline_court: missing $aug_receipt" >&2; exit 1; }
fi

# ---------------------------------------------------------------------------
# Phase 2: adverse court
# ---------------------------------------------------------------------------
if [[ "$augmentation_only" != 1 ]]; then
  echo "object_pipeline_court: phase 2 (adverse court against a second MinIO)" >&2
  docker network create --subnet "$subnet" --gateway "$gateway" "$network" >/dev/null
  start_minio

  export WEFT_DOCKER_NETWORK="$network"
  export WEFT_S3_ENDPOINT="http://$minio_ip:9000"
  export WEFT_S3_BUCKET="weft-n04-$suffix"
  export WEFT_S3_REGION="${WEFT_S3_REGION:-us-east-1}"
  export WEFT_S3_ACCESS_KEY="$access_key"
  export WEFT_S3_SECRET_KEY="$secret_key"
  export WEFT_S3_TEST_ENDPOINT="http://$minio_ip:9000"
  export WEFT_S3_TEST_PREFIX="weft-n04-test-$suffix"
  export WEFT_S3_LOCK_LEASE_SECS="${WEFT_S3_LOCK_LEASE_SECS:-60}"
  export WEFT_S3_FENCE_LEASE_SECS="${WEFT_S3_FENCE_LEASE_SECS:-3}"
  export WEFT_S3_SERVER_LABEL="$minio_version"
  export WEFT_S3_SERVER_RELEASE="$minio_release"
  export WEFT_S3_SERVER_SHA256="$actual_sha256"
  export WEFT_S3_CONTAINER_IMAGE="$container_image_digest"

  run_adverse() {
    local mode="$1" target="$2" scenario="$3" out="$4"
    local out_container="$work_container/op/$(basename "$out")"
    local proxy_log_container="$work_container/op/proxy-$mode-$scenario.jsonl"
    echo "object_pipeline_court: adverse $scenario (proxy mode=$mode target_put=$target)" >&2
    scripts/docker-court.sh bench "cd /work && \
      python3 bench/scripts/s3_fault_proxy.py --listen 10001 --upstream $minio_ip:9000 \
        --mode $mode --target-put $target --log $proxy_log_container & \
      PROXY=\$!; \
      READY=0; for i in \$(seq 1 100); do \
        if (exec 3<>/dev/tcp/127.0.0.1/10001) 2>/dev/null; then READY=1; break; fi; \
        sleep 0.1; \
      done; \
      if [ \"\$READY\" != 1 ]; then echo 'proxy did not listen' >&2; kill \$PROXY 2>/dev/null || true; exit 1; fi; \
      cargo run --release --quiet --example object_pipeline_adverse -- \
        --scenario $scenario --endpoint http://127.0.0.1:10001 \
        --reference-endpoint http://$minio_ip:9000 \
        --bucket weft-n04-$suffix-$mode --out $out_container; \
      RC=\$?; kill \$PROXY 2>/dev/null || true; wait \$PROXY 2>/dev/null || true; exit \$RC"
    [[ -f "$out" ]] || { echo "object_pipeline_court: missing $out" >&2; exit 1; }
    cp "$work/op/proxy-$mode-$scenario.jsonl" "bench/receipts/n04-proxy-$mode-$scenario-$stamp.jsonl" 2>/dev/null || true
  }

  # Learn how many PUTs one write performs, then fault at a point inside blob
  # publication rather than at random.
  run_adverse count 0 baseline "$work/op/baseline.json"
  puts="$(grep -c '"event": "put"' "$work/op/proxy-count-baseline.jsonl" 2>/dev/null || true)"
  puts="${puts:-0}"
  if [[ ! "$puts" =~ ^[0-9]+$ || "$puts" -lt 4 ]]; then
    echo "object_pipeline_court: proxy counted $puts PUTs; cannot choose a fault target" >&2
    exit 1
  fi
  target=$(( puts / 2 ))
  # The fault must land on a blob that the write cannot do without. An advisory
  # index segment is published best-effort by design (a failed index write is a
  # stat note, never a commit failure), so target a `manifests/` or `objects/`
  # PUT near the middle of the write instead of an arbitrary request.
  blob_target="$(python3 - "$work/op/proxy-count-baseline.jsonl" <<'PY'
import json
import sys

rows = []
with open(sys.argv[1]) as handle:
    for line in handle:
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("event") != "put":
            continue
        path = record.get("path", "")
        if "/manifests/" in path or "/objects/" in path:
            rows.append(record["n"])
print(rows[len(rows) // 2] if rows else 0)
PY
  )"
  if [[ -z "$blob_target" || "$blob_target" == "0" ]]; then
    echo "object_pipeline_court: no blob PUT in the baseline log; cannot choose a fault target" >&2
    exit 1
  fi
  target="$blob_target"
  export WEFT_N04_PUTS="$puts"
  echo "object_pipeline_court: the baseline write issued $puts PUTs; fault target is PUT $target" >&2

  run_adverse reset "$target" abort "$work/op/abort.json"
  # A lost acknowledgement must target a PUT that was forwarded, so the same
  # index is used; the proxy closes the client after the provider applied it.
  run_adverse swallow "$target" lost-ack "$work/op/lost-ack.json"
  run_adverse count 0 stale-reader "$work/op/stale-reader.json"

  echo "object_pipeline_court: S3 fencing suite (stale conditional write refused)" >&2
  scripts/docker-court.sh rust \
    "cd /work && cargo test --test s3_fencing -- --test-threads=1 --nocapture" \
    >"$work/op/fencing.log" 2>&1 || { tail -40 "$work/op/fencing.log" >&2; exit 1; }
  cp "$work/op/fencing.log" "bench/receipts/n04-s3-fencing-$stamp.log"

  # -------------------------------------------------------------------------
  # Phase 3: assemble the N04 receipt
  # -------------------------------------------------------------------------
  receipt="bench/receipts/n04-object-pipeline-$stamp.json"
  aug_receipt="bench/receipts/augmentation-minio-$stamp.json"
  image_id="$(docker image inspect weft/bench-court:1 --format '{{.Id}}' 2>/dev/null || echo unknown)"
  python3 - "$receipt" "$stamp" "$aug_receipt" "$reps" "$configs" "$puts" "$target" \
    "$minio_release" "$actual_sha256" "$minio_version" "$container_image_digest" \
    "$fixture_sha256" "$image_id" "$work/op" "$augmentation_only" "$adverse_only" <<'PY'
import json
import os
import subprocess
import sys

(
    out_path,
    stamp,
    aug_receipt,
    reps,
    configs,
    puts,
    target,
    minio_release,
    minio_sha,
    minio_version,
    container_image,
    fixture_sha,
    image_id,
    op_dir,
    augmentation_only,
    adverse_only,
) = sys.argv[1:]


def load(path):
    with open(path) as handle:
        return json.load(handle)


adverse = {}
for name in ("abort", "lost-ack", "stale-reader", "baseline"):
    path = os.path.join(op_dir, f"{name}.json")
    adverse[name] = load(path) if os.path.exists(path) else None

aug = load(aug_receipt) if os.path.exists(aug_receipt) else None

revisions = {}
if aug:
    revisions = aug.get("environment", {}).get("revisions", {})

checks = {
    "augmentation_court_ran": aug is not None,
    "augmentation_all_restored_exact": bool(
        aug and aug.get("checks", {}).get("all_restored_tensors_exact")
    ),
    "augmentation_mandatory_metrics_present": bool(
        aug and aug.get("checks", {}).get("mandatory_metrics_present")
    ),
    "provider_abort_failed_closed": bool(
        adverse["abort"] and adverse["abort"].get("verify_after_fault_ok")
    ),
    "provider_abort_left_previous_head": bool(
        adverse["abort"]
        and adverse["abort"].get("head_after_fault") == adverse["abort"].get("pre_fault_head")
    ),
    "provider_abort_retry_matches_reference": bool(
        adverse["abort"] and adverse["abort"].get("retry_matches_reference")
    ),
    "lost_ack_failed_closed": bool(
        adverse["lost-ack"] and adverse["lost-ack"].get("verify_after_fault_ok")
    ),
    "lost_ack_retry_matches_reference": bool(
        adverse["lost-ack"] and adverse["lost-ack"].get("retry_matches_reference")
    ),
    "stale_reader_stayed_pinned": bool(
        adverse["stale-reader"] and adverse["stale-reader"].get("reader_stayed_pinned")
    ),
    "fresh_reader_sees_the_commit": bool(
        adverse["stale-reader"] and adverse["stale-reader"].get("fresh_reader_sees_new_entry")
    ),
}

adverse_checks = {
    key: value
    for key, value in checks.items()
    if not key.startswith("augmentation_")
}
partial = False
if augmentation_only == "1":
    for key in list(checks):
        if not key.startswith("augmentation_"):
            checks.pop(key)
    partial = True
elif adverse_only == "1":
    for key in list(checks):
        if key.startswith("augmentation_"):
            checks.pop(key)
    partial = True
result = "pass" if all(checks.values()) else "fail"
if partial:
    result = "partial" if all(checks.values()) else "fail"

receipt = {
    "schema": "weft.n04.object_pipeline.v1",
    "generated_utc": stamp,
    "result": result,
    "partial": partial,
    "checks": checks,
    "environment": {
        "court_image": "weft/bench-court:1",
        "court_image_id": image_id,
        "kernel": subprocess.run(["uname", "-r"], capture_output=True, text=True).stdout.strip(),
        "memory": os.environ.get("WEFT_MEMORY", "3g"),
        "memory_swap": os.environ.get("WEFT_MEMORY", "3g"),
        "pids": os.environ.get("WEFT_PIDS", "1024"),
        "cpus": os.environ.get("WEFT_CPUS", "4"),
        "cap_drop": "ALL",
        "cap_add": [],
        "revisions": revisions,
    },
    "backend": {
        "product": "MinIO (S3-compatible object store)",
        "release": minio_release,
        "binary_sha256": minio_sha,
        "version_line": minio_version,
        "client_container_image": container_image,
        "data_directory": "tmpfs, 1 GiB",
        "scope": "single-node loopback object store in one OOM-protected container; not a cloud, multi-node, fleet, media or power-loss claim",
    },
    "fixture": {
        "path": ".docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors",
        "sha256_verified": fixture_sha,
        "subset": "the frozen augmentation subset: the 272 MB embedding + 100 name-sorted evenly spaced tensors = 101 tensors, 424,439,296 logical bytes",
    },
    "durability_contract": {
        "native_arms": "a completed write is every object PUT acknowledged by the provider",
        "weftfs_arms": "every blob PUT acknowledged by the provider, then one commit record accepted that names them; all-or-nothing for a reader",
        "fsync_claimed": False,
        "note": "the pipeline reports durability_model=provider-ack on S3 and issues no file barrier; the MinIO data directory is a tmpfs",
        "visibility": "single-node read-after-write and list-after-write for published keys; conditional PUT (If-None-Match/If-Match) required and probed at open",
    },
    "arms": {
        "contract": ["A", "N", "B", "C", "D", "E"],
        "pipelined": ["B-pipe4", "C-pipe4", "D-pipe4", "E-pipe4"],
        "requested": configs.split(","),
        "repetitions": int(reps),
        "reduced": os.environ.get("WEFT_AUGMENTATION_REDUCED") == "1",
        "interleaving": "rotated by one position per repetition, as the augmentation contract requires",
        "augmentation_receipt": aug_receipt,
    },
    "adverse": {
        "put_target": int(target),
        "baseline_put_count": int(puts),
        "proxy": "bench/scripts/s3_fault_proxy.py (byte-level forwarder, one fault per run)",
        "scenarios": adverse,
        "s3_fencing_suite": f"bench/receipts/n04-s3-fencing-{stamp}.log",
        "proxy_logs": [
            f"bench/receipts/n04-proxy-{mode}-{scenario}-{stamp}.jsonl"
            for mode, scenario in (
                ("count", "baseline"),
                ("reset", "abort"),
                ("swallow", "lost-ack"),
                ("count", "stale-reader"),
            )
        ],
    },
    "losses_and_non_claims": [
        "the provider-ack durability model is not a media or power-loss guarantee",
        "one host, one loopback object-store process, one writer",
        "the fault proxy is test infrastructure and cannot reproduce a real provider's partial-failure modes",
        "no claim about AWS S3, GCS, Azure or any cloud service",
        "pipelined rows are labelled ablations: the pipeline is a store API (Store::prepare_tensor_group_pipelined), not a shipped CLI verb",
    ],
}
with open(out_path, "w") as handle:
    json.dump(receipt, handle, indent=1, sort_keys=False)
    handle.write("\n")
print(json.dumps({"result": receipt["result"], "checks": checks}, indent=1))
PY

  result="$(python3 -c "import json,sys;print(json.load(open('$receipt'))['result'])")"
  if [[ "$result" == "fail" ]]; then
    echo "object_pipeline_court: receipt result is '$result' ($receipt)" >&2
    exit 1
  fi
  echo "object_pipeline_court: receipt $receipt" >&2
fi

echo "object_pipeline_court: log $log" >&2
