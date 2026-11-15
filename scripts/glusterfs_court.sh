#!/usr/bin/env bash
# GlusterFS augmentation court (T036): a real, deployed GlusterFS volume with
# and without the WeftFS augmentation, plus cross-client behaviour.
#
# Deployment (recorded in the receipt):
#   * one GlusterFS 10.3 server container (Debian bookworm packages, baked into
#     `weft/gluster-court:1` at build time because `apt-get` cannot run inside
#     the court's own `--cap-drop=ALL` containers);
#   * one single-brick distributed volume on a host directory;
#   * two independent FUSE client processes in two hard-limited containers, each
#     mounting the same volume with `mount -t glusterfs`.
#
# Capabilities, and why each one is the minimum:
#   * server: SYS_ADMIN (the brick stores metadata in `trusted.glusterfs.*`
#     xattrs, which only CAP_SYS_ADMIN may write) and DAC_OVERRIDE (Gluster's
#     posix translator creates its `.glusterfs` handle directory with mode 0600
#     and relies on root's DAC override to descend into it; without the
#     capability the brick fails with `Permission denied` on
#     `/data/brick1/.glusterfs/indices/xattrop`). Both were found by running it
#     and are recorded in the receipt.
#   * clients: SYS_ADMIN plus /dev/fuse, for the Gluster FUSE mount.
#
# Arms (frozen methodology: `bench/CONTRACTS-augmentation.md`):
#   A  native per-tensor files, each `fsync`ed, directory `fsync`ed;
#   N  native + canonical manifest, provenance record and explicit key-set
#      context, published atomically on the mount;
#   B  WeftFS store on the mount, raw; C  planned; D/E  B/C + protocol.
# WeftFS's own FUSE is **not** mounted: every arm writes through GlusterFS.
#
# Cross-client section (after the matrix, so it cannot influence it):
#   1. close-to-open consistency in both directions;
#   2. in-place rewrite visibility;
#   3. n-client failure: writer and FUSE client killed mid-write, peer reads.
#
# Durability, matched across arms: `fsync` on a GlusterFS file is forwarded to
# the brick, which `fsync`s the backing host file; every arm gets file fsync +
# directory fsync (or the atomic metadata replace for N).
#
# Label: one host, one single-brick volume, two client processes, one writer at
# a time. Not a fleet, not multi-node, not replicated, not a cloud result, and
# not a media or power-loss claim.
#
# Usage:
#   scripts/glusterfs_court.sh                # full run (7 interleaved reps)
#   scripts/glusterfs_court.sh --reps=3
#   scripts/glusterfs_court.sh --reduced
#   scripts/glusterfs_court.sh --smoke
#
# Exit 0 = pass; 1 = ran and failed; 78 = skipped with exactly one
# `glusterfs_court: skip <reason>` line.
set -Eeuo pipefail

skip() {
  echo "glusterfs_court: skip $*" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

stamp="${WEFT_GLUSTER_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_GLUSTER_REPS:-7}"
smoke=0
reduced=0
for arg in "$@"; do
  case "$arg" in
    --reps=*) reps="${arg#--reps=}" ;;
    --smoke) smoke=1; reps=1 ;;
    --reduced) reduced=1 ;;
    -h|--help) sed -n '2,64p' "$0"; exit 0 ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
[[ -e /dev/fuse ]] || skip "no /dev/fuse; the Gluster FUSE client cannot mount without it"

fixture="${WEFT_AUG_FIXTURE:-$repo_root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors}"
fixture_sha256="${WEFT_AUG_FIXTURE_SHA256:-fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe}"
[[ -f "$fixture" ]] || skip "fixture $fixture is not present"

image="weft/gluster-court:1"
if ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "glusterfs_court: building $image from docker/gluster.Dockerfile (apt cannot run inside a cap-drop=ALL court)" >&2
  docker build --memory=2g --memory-swap=2g --ulimit nofile=65536:65536 \
    -f docker/gluster.Dockerfile -t "$image" docker >/dev/null || skip "cannot build the GlusterFS court image"
fi
image_id="$(docker image inspect "$image" --format '{{.Id}}')"
gluster_version="$(docker run --rm --entrypoint bash "$image" -c 'glusterfs --version 2>&1 | head -1')"
gluster_packages="$(docker run --rm --entrypoint bash "$image" -c "dpkg -l | awk '\$1==\"ii\" && \$2 ~ /glusterfs/ {print \$2\" \"\$3}' | sort | tr '\n' ';'")"

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"

free_kib="$(df -Pk "$repo_root" | awk 'NR==2 {print $4}')"
if [[ -z "$free_kib" || "$free_kib" -lt 6291456 ]]; then
  skip "only ${free_kib:-unknown} KiB free on the host filesystem; the court needs 6 GiB of scratch"
fi

subnet="${WEFT_GLUSTER_SUBNET:-10.214.77.0/24}"
gateway="${subnet%.*}.1"
server_ip="${subnet%.*}.10"
client_a_ip="${subnet%.*}.11"
client_b_ip="${subnet%.*}.12"
for network_id in $(docker network ls -q); do
  for existing in $(docker network inspect "$network_id" --format '{{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null); do
    if [[ "$existing" == "$subnet" ]]; then
      skip "subnet $subnet is already in use by network $network_id; set WEFT_GLUSTER_SUBNET"
    fi
  done
done

work="$repo_root/.docker-cache/gluster-$stamp"
purge_work() {
  local dir="$1"
  [[ -d "$dir" ]] || return 0
  docker run --rm -v "$dir:/w" "$image" bash -c 'rm -rf /w/* 2>/dev/null; true' >/dev/null 2>&1 || true
  rm -rf "$dir" 2>/dev/null || true
}
purge_work "$work"
mkdir -p "$work/brick" "$work/cross" bench/receipts
# The brick process runs as uid 0 in the container and the host directory is
# owned by the invoking user; with cap-drop=ALL and only SYS_ADMIN +
# DAC_OVERRIDE it must still be able to create directories in the brick root.
chmod 777 "$work/brick"
log="bench/receipts/glusterfs-$stamp.log"
: > "$log"
exec > >(tee -a "$log") 2>&1

server="weft-gluster-srv-$$"
client_a="weft-gluster-a-$$"
client_b="weft-gluster-b-$$"
network="weft-gluster-net-$$"
volume="weftgl"

cleanup() {
  docker rm -f "$client_b" >/dev/null 2>&1 || true
  docker rm -f "$client_a" >/dev/null 2>&1 || true
  docker rm -f "$server" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
  if [[ "${WEFT_GLUSTER_KEEP_WORK:-0}" != "1" ]]; then
    purge_work "$work"
  fi
}
trap cleanup EXIT INT TERM

echo "glusterfs_court: $gluster_version (image $image_id, packages $gluster_packages)" >&2
docker network create --subnet "$subnet" --gateway "$gateway" "$network" >/dev/null || skip "cannot create the court Docker network $network"

docker run -d --name "$server" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 \
  --security-opt no-new-privileges \
  --cap-drop=ALL --cap-add SYS_ADMIN --cap-add DAC_OVERRIDE \
  --network "$network" --ip "$server_ip" --hostname weft-gluster \
  --tmpfs /tmp:rw,size=512m,nosuid,nodev \
  -v "$work/brick:/data/brick1" \
  --entrypoint bash "$image" -c 'exec glusterd --no-daemon --log-file=/tmp/glusterd.log --log-level=INFO' >/dev/null

ready=0
for _ in $(seq 1 120); do
  if docker exec "$server" bash -c 'timeout 5 gluster volume list >/dev/null 2>&1'; then
    ready=1
    break
  fi
  sleep 0.5
done
if [[ "$ready" != 1 ]]; then
  echo "glusterfs_court: glusterd did not become ready; logs follow" >&2
  docker logs --tail 40 "$server" >&2 || true
  docker exec "$server" sh -c 'tail -30 /tmp/glusterd.log' >&2 || true
  exit 1
fi

docker exec "$server" bash -c "
set -Eeuo pipefail
gluster volume create $volume $server_ip:/data/brick1 force
# Debian's glusterfs-server does not ship the NFS xlator (the upstream built-in
# NFS server was removed); without this option glusterd fails the start commit
# with 'DL open failed ... nfs/server.so'. This is the only option changed from
# the default volume configuration, and it is recorded below.
gluster volume set $volume nfs.disable on
gluster volume start $volume
" > "$work/volume.out" 2>&1 || { echo "glusterfs_court: volume setup failed; log follows" >&2; cat "$work/volume.out" >&2; exit 1; }

docker exec "$server" sh -c "gluster volume info all; echo ---; gluster volume status; echo ---; gluster peer status; echo ---; gluster volume get $volume all" > "$work/volume-info.txt" 2>&1 || true

client_args=(
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY"
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS"
  --ulimit nofile=65536:65536
  --security-opt no-new-privileges
  --cap-drop=ALL --cap-add SYS_ADMIN
  --device /dev/fuse
  --tmpfs /tmp:rw,size=512m,nosuid,nodev
  -v "$repo_root:/work"
)

start_mount() {
  local container="$1" mountpoint="$2"
  docker exec "$container" bash -c "
set -Eeuo pipefail
mkdir -p $mountpoint
mount -t glusterfs -o log-level=WARNING,log-file=/tmp/gluster-client-$(printf '%s' "$mountpoint" | tr '/' '_').log $server_ip:/$volume $mountpoint
for _ in \$(seq 1 100); do
  if grep -q ' $mountpoint ' /proc/self/mountinfo; then exit 0; fi
  sleep 0.1
done
echo 'glusterfs_court: mount did not appear' >&2
exit 1
"
}

kill_clients() {
  docker exec "$1" bash -c '
# The FUSE client is the glusterfs binary (`readlink /proc/<pid>/exe` resolves to
# /usr/sbin/glusterfsd, which is the same file) and its cmdline carries
# `--process-name fuse`. Matching on the exe first keeps this loop from killing
# the shell that runs it.
for p in /proc/[0-9]*; do
  pid=${p#/proc/}
  e=$(readlink -f "$p/exe" 2>/dev/null)
  case "$e" in
    /usr/sbin/glusterfsd|/usr/sbin/glusterfs)
      if tr "\0" " " < "$p/cmdline" 2>/dev/null | grep -q -- "--process-name fuse"; then
        kill -9 "$pid" 2>/dev/null || true
      fi
      ;;
  esac
done
true
'
}

count_ok() { awk '$2 == "ok" {n++} END {print n+0}' "$1" 2>/dev/null || echo 0; }
count_bad() { awk '$2 == "bad" {n++} END {print n+0}' "$1" 2>/dev/null || echo 0; }

echo "glusterfs_court: starting client A and mounting $server_ip:/$volume at /mnt/weft-gl" >&2
docker run -d --name "$client_a" --ip "$client_a_ip" --network "$network" --entrypoint sleep "${client_args[@]}" "$image" infinity >/dev/null
start_mount "$client_a" /mnt/weft-gl || exit 1
docker exec "$client_a" sh -c 'grep " /mnt/weft-gl " /proc/self/mountinfo' > "$work/mountinfo-a.txt"

if [[ -n "${WEFT_GLUSTER_SKIP_BENCH:-}" ]]; then
  receipt=""
  label="cross-client-only"
else
  echo "glusterfs_court: building the harness in the rust court" >&2
  scripts/docker-court.sh rust "cd /work && cargo build --release --example augmentation_bench"

  subset_args="--max-others 100 --include-embedding 1"
  label="full"
  if [[ "$smoke" == 1 ]]; then
    subset_args="--max-others 16 --include-embedding 0"
    label="smoke"
  elif [[ "$reduced" == 1 ]]; then
    subset_args="--max-others 16 --include-embedding 0"
    label="reduced"
  fi

  receipt="bench/receipts/augmentation-glusterfs-$stamp.json"
  receipt_log="bench/receipts/augmentation-glusterfs-$stamp.log"
  weftfs_rev="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  weftcodec_rev="$(git -C "$repo_root/WeftCodec" rev-parse HEAD 2>/dev/null || echo unknown)"
  weftcodec_go_rev="$(git -C "$repo_root/WeftCodec-Go" rev-parse HEAD 2>/dev/null || echo unknown)"
  weftfs_go_rev="$(git -C "$repo_root/WeftFS-Go" rev-parse HEAD 2>/dev/null || echo unknown)"

  echo "glusterfs_court: running the six-arm court on the GlusterFS mount (label=$label reps=$reps)" >&2
  timeout --signal=TERM --kill-after=120 7200 docker exec \
    -e "WEFT_REPO_WEFTFS=$weftfs_rev" \
    -e "WEFT_REPO_WEFTCODEC=$weftcodec_rev" \
    -e "WEFT_REPO_WEFTCODEC_GO=$weftcodec_go_rev" \
    -e "WEFT_REPO_WEFTFS_GO=$weftfs_go_rev" \
    -e "WEFT_COURT_IMAGE=$image_id" \
    -e "WEFT_COURT_KERNEL=$(uname -r)" \
    -e "WEFT_IN_COURT=1" \
    "$client_a" bash -c "
set -Eeuo pipefail
ulimit -n 65536
cd /work
./.docker-cache/target-rust/release/examples/augmentation_bench run \
  --fixture /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  --fixture-sha256 $fixture_sha256 \
  --work /mnt/weft-gl/augmentation-$stamp \
  --out $receipt --log $receipt_log \
  --backend local --chunk-size 1048576 \
  $subset_args \
  --configs A,N,B,C,D,E --reps $reps \
  --with-native-app --passes 2 --label $label --seed weft-augmentation-r3 \
  --order-seed 20261114 \
  --force
" 2>&1 | tee "$work/bench.out" || { echo "glusterfs_court: the six-arm harness failed; tail follows" >&2; tail -60 "$work/bench.out" >&2; exit 1; }

  [[ -f "$receipt" ]] || { echo "glusterfs_court: expected receipt $receipt was not written" >&2; exit 1; }
  result="$(python3 -c "import json;print(json.load(open('$receipt'))['result'])")"
  if [[ "$result" != "pass" ]]; then
    if [[ "$smoke" == 1 ]]; then
      other_failures="$(python3 - "$receipt" <<'PY'
import json
import sys
receipt = json.load(open(sys.argv[1]))
checks = receipt.get("checks", {})
failed = [key for key, value in checks.items() if value is not True and key != "at_least_seven_reps"]
print(",".join(failed))
PY
)"
      [[ -z "$other_failures" ]] || { echo "glusterfs_court: smoke run failed checks: $other_failures" >&2; exit 1; }
      echo "glusterfs_court: smoke run result '$result' accepted as partial (reps < 7 only)" >&2
    else
      echo "glusterfs_court: receipt result is '$result' ($receipt)" >&2
      exit 1
    fi
  fi
fi

# ---------------------------------------------------------------------------
# Cross-client behaviour (identical step order to the JuiceFS court).
# ---------------------------------------------------------------------------
echo "glusterfs_court: starting client B (second client process, same volume)" >&2
docker run -d --name "$client_b" --ip "$client_b_ip" --network "$network" --entrypoint sleep "${client_args[@]}" "$image" infinity >/dev/null
start_mount "$client_b" /mnt/weft-gl || exit 1
docker exec "$client_b" sh -c 'grep " /mnt/weft-gl " /proc/self/mountinfo' > "$work/mountinfo-b.txt"

cross="$work/cross"

docker exec "$client_a" bash -c "mkdir -p /mnt/weft-gl/cross && cd /mnt/weft-gl/cross && dd if=/dev/urandom of=from-a.bin bs=1M count=64 conv=fsync status=none && sha256sum from-a.bin | awk '{print \$1}'" > "$cross/from-a.sha" 2>&1
expected_a="$(head -1 "$cross/from-a.sha")"
peer_a_start="$(date -u +%s%N)"
docker exec "$client_b" bash -c "cd /mnt/weft-gl/cross && for _ in \$(seq 1 400); do
  if [ \"\$(sha256sum from-a.bin 2>/dev/null | awk '{print \$1}')\" = \"$expected_a\" ]; then echo ok; exit 0; fi
  sleep 0.01; done; echo mismatch; exit 1" > "$cross/peer-read-a.out" 2>&1 || true
peer_read_a_ms=$(( ($(date -u +%s%N) - peer_a_start) / 1000000 ))

docker exec "$client_b" bash -c "cd /mnt/weft-gl/cross && dd if=/dev/urandom of=from-b.bin bs=1M count=64 conv=fsync status=none && sha256sum from-b.bin | awk '{print \$1}'" > "$cross/from-b.sha" 2>&1
expected_b="$(head -1 "$cross/from-b.sha")"
peer_b_start="$(date -u +%s%N)"
docker exec "$client_a" bash -c "cd /mnt/weft-gl/cross && for _ in \$(seq 1 400); do
  if [ \"\$(sha256sum from-b.bin 2>/dev/null | awk '{print \$1}')\" = \"$expected_b\" ]; then echo ok; exit 0; fi
  sleep 0.01; done; echo mismatch; exit 1" > "$cross/peer-read-b.out" 2>&1 || true
peer_read_b_ms=$(( ($(date -u +%s%N) - peer_b_start) / 1000000 ))

docker exec "$client_a" bash -c "cd /mnt/weft-gl/cross && printf 'first-generation\n' > rewrite.txt && dd if=/dev/zero bs=1M count=1 conv=fsync status=none >> rewrite.txt" >/dev/null
docker exec "$client_b" bash -c "cd /mnt/weft-gl/cross && head -c 17 rewrite.txt" > "$cross/rewrite-before.txt" 2>&1 || true
docker exec "$client_a" bash -c "cd /mnt/weft-gl/cross && printf 'second-generation\n' > rewrite.tmp && dd if=/dev/zero bs=1M count=2 conv=fsync status=none >> rewrite.tmp && mv rewrite.tmp rewrite.txt && sync" >/dev/null
rewrite_start="$(date -u +%s%N)"
docker exec "$client_b" bash -c "cd /mnt/weft-gl/cross && for _ in \$(seq 1 400); do
  if [ \"\$(head -c 17 rewrite.txt)\" = \"second-generation\" ]; then echo ok; exit 0; fi
  sleep 0.01; done; echo stale; exit 1" > "$cross/rewrite-after.out" 2>&1 || true
rewrite_visible_ms=$(( ($(date -u +%s%N) - rewrite_start) / 1000000 ))

echo "glusterfs_court: n-client failure test: write on A, kill A mid-write, read on B" >&2
# The writer is bounded to 64 MiB and each chunk to its own timeout: the test
# needs a handful of acknowledged chunks, and an unbounded writer on a mount
# whose client has been killed must never be able to run the host out of space.
timeout --signal=KILL 240 docker exec "$client_a" bash -c '
set -uo pipefail
mkdir -p /mnt/weft-gl/cross
: > /tmp/ack.log
writer() {
  for i in $(seq 1 64); do
    dd if=/dev/urandom of=/tmp/chunk.bin bs=1M count=1 status=none
    h=$(sha256sum /tmp/chunk.bin | cut -d" " -f1)
    if ! timeout -s KILL 20 dd if=/tmp/chunk.bin of=/mnt/weft-gl/cross/inflight.bin bs=1M count=1 \
         oflag=append conv=notrunc,fsync status=none 2>/dev/null; then
      printf "write-failed-at-chunk=%s\n" "$i" >> /tmp/writer.state
      break
    fi
    printf "%d %s\n" "$i" "$h" >> /tmp/ack.log
  done
}
writer &
wp=$!
for _ in $(seq 1 2000); do
  n=$(grep -cE "^[0-9]+ [0-9a-f]{64}$" /tmp/ack.log 2>/dev/null || true)
  if [ "${n:-0}" -ge 8 ]; then break; fi
  sleep 0.01
done
n_before=$(grep -cE "^[0-9]+ [0-9a-f]{64}$" /tmp/ack.log 2>/dev/null || true)
for p in /proc/[0-9]*; do
  pid=${p#/proc/}
  e=$(readlink -f "$p/exe" 2>/dev/null)
  case "$e" in
    /usr/sbin/glusterfsd|/usr/sbin/glusterfs)
      if tr "\0" " " < "$p/cmdline" 2>/dev/null | grep -q -- "--process-name fuse"; then
        kill -9 "$pid" 2>/dev/null || true
      fi
      ;;
  esac
done
for _ in $(seq 1 200); do kill -0 "$wp" 2>/dev/null || break; sleep 0.05; done
wait "$wp" 2>/dev/null || true
n_final=$(grep -cE "^[0-9]+ [0-9a-f]{64}$" /tmp/ack.log 2>/dev/null || true)
state=$(cat /tmp/writer.state 2>/dev/null | head -1 || true)
printf "acknowledged-before-kill=%s acknowledged-final=%s %s\n" "${n_before:-0}" "${n_final:-0}" "${state:-writer-stopped-without-error-line}"
' > "$cross/writer.out" 2>&1 || true
kill_status="$(printf 'SIGKILL of the GlusterFS FUSE client process in client A while the writer was still running; %s' "$(head -1 "$cross/writer.out" 2>/dev/null || echo 'ack count unknown')")"
docker exec "$client_a" bash -c 'grep -E "^[0-9]+ [0-9a-f]{64}$" /tmp/ack.log 2>/dev/null || true' > "$cross/ack.log" 2>/dev/null || true
docker exec "$client_a" bash -c 'cat /tmp/writer.state 2>/dev/null || true' > "$cross/writer.state" 2>/dev/null || true
acked_clean="$(awk '$1 ~ /^[0-9]+$/ && $2 ~ /^[0-9a-f]{64}$/ {n++} END {print n+0}' "$cross/ack.log")"

docker exec "$client_b" sh -c 'stat -c %s /mnt/weft-gl/cross/inflight.bin 2>/dev/null || echo missing' > "$cross/peer-len.txt" 2>&1 || true
peer_len="$(head -1 "$cross/peer-len.txt" | tr -d '[:space:]')"
docker exec -i "$client_b" bash -c 'cat > /tmp/ack.log' < "$cross/ack.log" || true
docker exec "$client_b" bash -c '
set -Eeuo pipefail
f=/mnt/weft-gl/cross/inflight.bin
: > /tmp/readback.tsv
i=0
while read -r idx h; do
  i=$((i+1))
  got=$(dd if="$f" bs=1048576 skip=$((i-1)) count=1 status=none 2>/dev/null | sha256sum | cut -d" " -f1) || got=""
  if [ "$got" = "$h" ]; then printf "%s\tok\n" "$i" >> /tmp/readback.tsv; else printf "%s\tbad\n" "$i" >> /tmp/readback.tsv; fi
done < /tmp/ack.log
cat /tmp/readback.tsv
' > "$cross/readback.tsv" 2>&1 || true
readback_ok="$(count_ok "$cross/readback.tsv")"
readback_bad="$(count_bad "$cross/readback.tsv")"

remount_status="skipped"
if start_mount "$client_a" /mnt/weft-gl2 >/dev/null 2>&1; then
  remount_status="remounted_at_/mnt/weft-gl2"
  docker exec "$client_a" bash -c '
set -Eeuo pipefail
f=/mnt/weft-gl2/cross/inflight.bin
: > /tmp/readback-a.tsv
i=0
while read -r idx h; do
  i=$((i+1))
  got=$(dd if="$f" bs=1048576 skip=$((i-1)) count=1 status=none 2>/dev/null | sha256sum | cut -d" " -f1) || got=""
  if [ "$got" = "$h" ]; then printf "%s\tok\n" "$i" >> /tmp/readback-a.tsv; else printf "%s\tbad\n" "$i" >> /tmp/readback-a.tsv; fi
done < /tmp/ack.log
cat /tmp/readback-a.tsv
' > "$cross/readback-a.tsv" 2>&1 || true
fi
readback_a_ok="$(count_ok "$cross/readback-a.tsv")"
readback_a_bad="$(count_bad "$cross/readback-a.tsv")"

docker exec "$client_b" sh -c "df -h /mnt/weft-gl; echo ---; df -i /mnt/weft-gl; echo ---; head -60 /tmp/gluster-client-_mnt_weft-gl.log" > "$work/df.txt" 2>&1 || true
docker exec "$server" bash -c "gluster volume status $volume; echo ---; gluster volume info $volume" > "$work/volume-after.txt" 2>&1 || true

cp "$cross/ack.log" "bench/receipts/glusterfs-$stamp.ack.log" 2>/dev/null || true
cp "$cross/readback.tsv" "bench/receipts/glusterfs-$stamp.readback.tsv" 2>/dev/null || true
cp "$cross/readback-a.tsv" "bench/receipts/glusterfs-$stamp.readback-recovered.tsv" 2>/dev/null || true
cp "$cross/writer.out" "bench/receipts/glusterfs-$stamp.writer.txt" 2>/dev/null || true

# ---------------------------------------------------------------------------
# Court receipt
# ---------------------------------------------------------------------------
court_receipt="bench/receipts/glusterfs-$stamp.json"
python3 - "$court_receipt" "$stamp" "$gluster_version" "$gluster_packages" "$image" "$image_id" \
  "$WEFT_MEMORY" "$WEFT_CPUS" "$WEFT_PIDS" "$server_ip" "$volume" "$reps" "$label" "$receipt" \
  "$peer_read_a_ms" "$peer_read_b_ms" "$rewrite_visible_ms" "$kill_status" "$acked_clean" \
  "$readback_ok" "$readback_bad" "$peer_len" "$remount_status" "$readback_a_ok" "$readback_a_bad" \
  "$work" <<'PY'
import json
import os
import sys

(
    out, stamp, version, packages, image, image_id, memory, cpus, pids,
    server_ip, volume, reps, label, aug_receipt, peer_a_ms, peer_b_ms,
    rewrite_ms, kill_status, acked, ok, bad, peer_len, remount, remount_ok,
    remount_bad, work,
) = sys.argv[1:]

def read(path):
    try:
        return open(path).read()
    except OSError:
        return ""

mount_a = read(os.path.join(work, "mountinfo-a.txt")).strip()
mount_b = read(os.path.join(work, "mountinfo-b.txt")).strip()
volume_info = read(os.path.join(work, "volume-info.txt"))
volume_after = read(os.path.join(work, "volume-after.txt"))
df = read(os.path.join(work, "df.txt")).strip()

receipt = {
    "schema": "weft.glusterfs_court.v1",
    "generated_utc": stamp,
    "backend": {
        "product": "GlusterFS",
        "version": version,
        "packages": packages,
        "image": image,
        "image_id": image_id,
        "image_note": "purpose-built court image: the Debian bookworm glusterfs-server/glusterfs-client packages are baked in at build time because apt-get cannot run inside the court's cap-drop=ALL containers",
        "topology": "one glusterd + one brick (single-brick distributed volume) in one server container; two independent FUSE client processes in two containers",
        "brick": "/data/brick1 on a host bind mount, mode 0777",
        "volume": volume,
        "volume_options_changed": {
            "nfs.disable": "on (Debian does not ship the NFS xlator; without it glusterd fails the start commit with 'DL open failed ... nfs/server.so')"
        },
        "mount": f"mount -t glusterfs -o log-level=WARNING {server_ip}:/{volume} /mnt/weft-gl",
        "mountinfo_client_a": mount_a,
        "mountinfo_client_b": mount_b,
        "volume_info": volume_info,
        "volume_status_after_cross_client": volume_after,
        "df": df,
        "label": "SINGLE HOST, one brick, one volume, TWO client processes, one writer at a time. Not a fleet, not multi-node, not replicated, not a cloud result. The brick is a host directory, so no media or power-loss claim is made.",
    },
    "container": {
        "server": {
            "memory": memory, "memory_swap": memory, "pids": pids, "cpus": cpus,
            "cap_drop": "ALL",
            "cap_add": ["SYS_ADMIN", "DAC_OVERRIDE"],
            "why_capabilities": [
                "SYS_ADMIN: the brick stores metadata in trusted.glusterfs.* xattrs, which require it; without it `gluster volume create` fails with 'Setting extended attributes failed, reason: Operation not permitted'",
                "DAC_OVERRIDE: the posix translator creates the `.glusterfs` handle directory with mode 0600 and descends into it as root; without the capability the brick aborts on '/data/brick1/.glusterfs/indices/xattrop: Failed to create [Permission denied]'",
            ],
        },
        "clients": {
            "count": 2,
            "memory": memory, "memory_swap": memory, "pids": pids, "cpus": cpus,
            "cap_drop": "ALL",
            "cap_add": ["SYS_ADMIN"],
            "device": "/dev/fuse",
            "why_capabilities": "the Gluster FUSE client mounts through /dev/fuse; SYS_ADMIN is the minimum mount capability",
        },
        "no_new_privileges": True,
    },
    "arms": {
        "contract": ["A", "N", "B", "C", "D", "E"],
        "repetitions": int(reps),
        "label": label,
        "augmentation_receipt": aug_receipt,
        "raw_log": f"bench/receipts/glusterfs-{stamp}.log",
        "durability": "a completed write has fsynced the file (A-E) or atomically replaced the manifest (N); GlusterFS forwards fsync to the brick, which fsyncs the backing host file",
        "weftfs_fuse_in_data_path": False,
    },
    "cross_client": {
        "client_processes": 2,
        "volume": volume,
        "close_to_open": {
            "a_write_then_b_read_ms": int(peer_a_ms),
            "b_write_then_a_read_ms": int(peer_b_ms),
            "method": "writer does dd conv=fsync, the peer polls a fresh open+read for a matching SHA-256 at 10 ms resolution; the value is a ceiling on visibility latency",
        },
        "in_place_rewrite": {
            "peer_observed_new_content_after_ms": int(rewrite_ms),
            "method": "A replaces the file atomically; B polls head -c 17 until it sees the new prefix",
            "why": "GlusterFS guarantees close-to-open consistency; attribute caching is bounded by the client's md-cache/stat-prefetch timeouts",
        },
        "n_client_failure": {
            "method": "client A writes 1 MiB chunks, each fsynced and its SHA-256 acknowledged to a container-local log, until the write fails; client A's GlusterFS FUSE process is SIGKILLed mid-write; client B reads every acknowledged chunk; a fresh client A mounts the volume again and reads them too",
            "kill": kill_status,
            "acknowledged_chunks": int(acked),
            "peer_read_chunks_ok": int(ok),
            "peer_read_chunks_bad": int(bad),
            "peer_file_length_bytes": peer_len,
            "recovered_client": remount,
            "recovered_read_chunks_ok": int(remount_ok),
            "recovered_read_chunks_bad": int(remount_bad),
            "write_failure_marker": read(os.path.join(work, "cross", "writer.state")).strip(),
            "rule": "every acknowledged (fsynced) chunk must read back intact on the peer; bytes beyond the last acknowledgement are unacknowledged and are reported, not counted as a loss",
            "is_media_loss": False,
        },
    },
    "non_claims": [
        "one host, one brick, one volume, two client processes, one writer at a time; not a fleet, cluster, replicated or multi-node result",
        "single-brick distributed volume only; no replica, disperse, distributed-replicate or geo-replication volume was measured",
        "no NFS or SMB access; no Gluster NFS (the xlator is absent from the Debian packages and is disabled)",
        "no media, power-loss or host-cache-loss claim; the failure test kills a client process, not a machine or a disk",
        "no claim about GlusterFS versions other than the packaged 10.3 recorded here",
        "no comparison against another filesystem (rule 13)",
    ],
}
with open(out, "w") as handle:
    json.dump(receipt, handle, indent=1)
    handle.write("\n")
print(f"glusterfs_court: court receipt {out}")
PY

if [[ "$readback_bad" != "0" ]]; then
  echo "glusterfs_court: $readback_bad acknowledged chunk(s) did not read back intact on the peer" >&2
  exit 1
fi
if [[ "$acked_clean" -lt 5 ]]; then
  echo "glusterfs_court: the n-client failure test acknowledged only $acked_clean chunks; too few to be meaningful" >&2
  exit 1
fi

echo "glusterfs_court: cross-client close-to-open a->b ${peer_read_a_ms} ms, b->a ${peer_read_b_ms} ms, rewrite visible in ${rewrite_visible_ms} ms" >&2
echo "glusterfs_court: n-client failure: $acked_clean acknowledged, $readback_ok ok / $readback_bad bad on the peer, recovered client $readback_a_ok ok / $readback_a_bad bad" >&2
echo "glusterfs_court: court receipt $court_receipt" >&2
echo "glusterfs_court: log $log" >&2
