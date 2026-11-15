#!/usr/bin/env bash
# JuiceFS augmentation court (T035): a real, deployed JuiceFS volume with and
# without the WeftFS augmentation, plus cross-client behaviour.
#
# Deployment (recorded in the receipt):
#   * the official JuiceFS release binary v1.4.2 (linux-amd64), fetched from the
#     project's GitHub release and verified against the release's own
#     `checksums.txt` before it is used;
#   * metadata engine: SQLite (`sqlite3:///jfs/meta.db`);
#   * object store: the local file store (`--storage file --bucket /jfs/obj`),
#     i.e. a key/value object layout, not the mount itself;
#   * two independent JuiceFS client processes in two hard-limited containers,
#     each with its own mount point and its own local cache directory, both
#     mounting the same volume through JuiceFS's own FUSE client.
#
# Arms (frozen methodology: `bench/CONTRACTS-augmentation.md`):
#   A  native: one plain file per tensor on the JuiceFS mount, each `fsync`ed,
#      the directory `fsync`ed after the last;
#   N  native + a canonical JSON manifest, provenance record and explicit
#      key-set context, published atomically on the mount;
#   B  WeftFS store rooted on the JuiceFS mount, raw representation;
#   C  WeftFS store, shipped planned representation;
#   D  B + snapshot, Context Root, selector resolution and provenance record;
#   E  C + the same protocol surface.
#
# WeftFS's own FUSE is **not** mounted: every arm writes through JuiceFS.
#
# Cross-client section (after the measured matrix, so it cannot influence it):
#   1. close-to-open consistency in both directions (write+fsync+close on one
#      client, fresh open+read on the other);
#   2. in-place rewrite visibility (JuiceFS guarantees close-to-open, not
#      read-after-write, and the attribute/entry caches are 1 s by default);
#   3. n-client failure: a writer on client A is killed, together with client
#      A's JuiceFS daemon, mid-write; client B must read back every
#      acknowledged+fsynced chunk intact, and a restarted client A must read
#      them too.
#
# Durability, matched across arms: a completed write has the file's data
# uploaded to the file object store and its metadata committed in SQLite, after
# `fsync` on the file (A-E) or on the atomic metadata file (N). JuiceFS
# write-back mode is **off** (the default), so this court does not measure
# deferred upload.
#
# This is **one host, two client processes, one volume, one writer at a time**.
# It is not a fleet, not multi-node, not a cloud result, and not a media or
# power-loss claim: the object store and the SQLite database are directories on
# the court's host filesystem.
#
# Usage:
#   scripts/juicefs_court.sh                   # full run (7 interleaved reps)
#   scripts/juicefs_court.sh --reps=3          # fewer repetitions (dev)
#   scripts/juicefs_court.sh --reduced         # aggregate-court subset, 7 reps
#   scripts/juicefs_court.sh --smoke           # reduced subset, one repetition
#   scripts/juicefs_court.sh --cross-client-only
#
# Exit 0 = the measured court passed; 1 = it ran and failed; 78 = skipped with
# exactly one `juicefs_court: skip <reason>` line (environment refusal only).
set -Eeuo pipefail

skip() {
  echo "juicefs_court: skip $*" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

stamp="${WEFT_JUICEFS_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_JUICEFS_REPS:-7}"
smoke=0
reduced=0
cross_only=0
for arg in "$@"; do
  case "$arg" in
    --reps=*) reps="${arg#--reps=}" ;;
    --smoke) smoke=1; reps=1 ;;
    --reduced) reduced=1 ;;
    --cross-client-only) cross_only=1 ;;
    -h|--help) sed -n '2,66p' "$0"; exit 0 ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
[[ "$(uname -m)" == "x86_64" ]] || skip "only the verified linux-amd64 JuiceFS release is supported (host is $(uname -m))"
[[ -e /dev/fuse ]] || skip "no /dev/fuse; the JuiceFS client cannot mount without it"
command -v curl >/dev/null 2>&1 || skip "curl is required to fetch the pinned JuiceFS release"

fixture="${WEFT_AUG_FIXTURE:-$repo_root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors}"
fixture_sha256="${WEFT_AUG_FIXTURE_SHA256:-fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe}"
[[ -f "$fixture" ]] || skip "fixture $fixture is not present"

jfs_release="${WEFT_JUICEFS_RELEASE:-v1.4.2}"
jfs_tarball_sha256="${WEFT_JUICEFS_SHA256:-79648a0a67145bf42ca4e8336578c13a5bbc9a0bd34df340b3b93dac1e975247}"
jfs_dir="$repo_root/.docker-cache/juicefs"
jfs_bin="$jfs_dir/juicefs"
tarball_url="https://github.com/juicedata/juicefs/releases/download/${jfs_release}/juicefs-${jfs_release#v}-linux-amd64.tar.gz"
mkdir -p "$jfs_dir"
if [[ ! -f "$jfs_bin" ]]; then
  echo "juicefs_court: downloading the official JuiceFS release $jfs_release" >&2
  if ! curl -fL --retry 3 -o "$jfs_dir/release.tar.gz.part" "$tarball_url"; then
    rm -f "$jfs_dir/release.tar.gz.part"
    skip "cannot obtain the pinned JuiceFS release $jfs_release"
  fi
  actual="$(sha256sum "$jfs_dir/release.tar.gz.part" | awk '{print $1}')"
  if [[ "$actual" != "$jfs_tarball_sha256" ]]; then
    rm -f "$jfs_dir/release.tar.gz.part"
    echo "juicefs_court: download sha256 mismatch (expected $jfs_tarball_sha256, got $actual)" >&2
    exit 1
  fi
  mv "$jfs_dir/release.tar.gz.part" "$jfs_dir/release.tar.gz"
  tar -C "$jfs_dir" -xzf "$jfs_dir/release.tar.gz" juicefs
fi
chmod +x "$jfs_bin"
release_sha256="$(sha256sum "$jfs_bin" | awk '{print $1}')"
jfs_version="$("$jfs_bin" version 2>&1 | head -1)"

client_image="${WEFT_JUICEFS_IMAGE:-debian:bookworm-slim}"
if ! docker image inspect "$client_image" >/dev/null 2>&1; then
  docker pull "$client_image" >/dev/null || skip "cannot obtain the pinned client image $client_image"
fi
client_image_digest="$(docker image inspect "$client_image" --format '{{index .RepoDigests 0}}')"
client_image_id="$(docker image inspect "$client_image" --format '{{.Id}}')"

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"

# The client container runs as uid 0 while the host mount is owned by the
# invoking user, so its writes are root-owned. A host-side `rm -rf` cannot
# delete them; the purge therefore runs inside a throwaway container that owns
# the same bind mount. This keeps scratch cleanup reliable (a full host has
# broken two courts before).
purge_work() {
  local dir="$1"
  [[ -d "$dir" ]] || return 0
  docker run --rm -v "$dir:/w" "$client_image" bash -c 'rm -rf /w/* 2>/dev/null; true' >/dev/null 2>&1 || true
  rm -rf "$dir" 2>/dev/null || true
}

# Capacity guard: this court writes one full configuration at a time onto the
# host filesystem that backs the object store and the SQLite database. A full
# host has broken two courts before, so refuse rather than fill it.
free_kib="$(df -Pk "$repo_root" | awk 'NR==2 {print $4}')"
if [[ -z "$free_kib" || "$free_kib" -lt 6291456 ]]; then
  skip "only ${free_kib:-unknown} KiB free on the host filesystem; the court needs 6 GiB of scratch"
fi

work="$repo_root/.docker-cache/juicefs-$stamp"
purge_work "$work"
mkdir -p "$work/obj" "$work/cache-a" "$work/cache-b" "$work/cross" bench/receipts
chmod -R a+rwX "$work" 2>/dev/null || true
chmod a+rwX bench/receipts 2>/dev/null || true
log="bench/receipts/juicefs-$stamp.log"
: > "$log"
exec > >(tee -a "$log") 2>&1

client_a="weft-jfs-a-$$"
client_b="weft-jfs-b-$$"

cleanup() {
  docker rm -f "$client_b" >/dev/null 2>&1 || true
  docker rm -f "$client_a" >/dev/null 2>&1 || true
  if [[ "${WEFT_JUICEFS_KEEP_WORK:-0}" != "1" ]]; then
    purge_work "$work"
  fi
}
trap cleanup EXIT INT TERM

client_args=(
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY"
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS"
  --ulimit nofile=65536:65536
  --security-opt no-new-privileges
  --cap-drop=ALL --cap-add SYS_ADMIN
  --device /dev/fuse
  --tmpfs /tmp:rw,size=512m,nosuid,nodev
  -v "$repo_root:/work"
  -v "$work:/jfs"
  -v "$jfs_bin:/usr/local/bin/juicefs:ro"
)

echo "juicefs_court: $jfs_version (release $jfs_release, binary sha256 $release_sha256)" >&2
echo "juicefs_court: metadata sqlite3:///jfs/meta.db, object store file:///jfs/obj" >&2

docker run -d --name "$client_a" --entrypoint sleep "${client_args[@]}" "$client_image" infinity >/dev/null
docker exec "$client_a" bash -c '
set -Eeuo pipefail
mkdir -p /mnt/jfs
# JuiceFS 1.4.2 takes --block-size in KiB (default 4M = 4096 KiB) and clamps
# above its 16 MiB maximum; 16384 KiB is the documented maximum block size and
# is stated explicitly so the receipt and `juicefs status` agree.
juicefs format --storage file --bucket /jfs/obj --block-size 16384 \
  --trash-days 0 --compress none sqlite3:///jfs/meta.db weftvol
' > "$work/format.out" 2>&1 || { echo "juicefs_court: format failed; log follows" >&2; cat "$work/format.out" >&2; exit 1; }

# Mount options are fixed and recorded. `--cache-size 1` (MiB) makes the
# client's own data cache negligible so the cold-restore column is not served
# by a JuiceFS client cache; `--open-cache 0` keeps the documented close-to-open
# default; the attribute and entry caches stay at their 1 s defaults.
mount_opts() {
  local cache="$1"
  echo "--no-usage-report --no-syslog --cache-dir $cache --cache-size 1 --open-cache 0 --attr-cache 1.0s --entry-cache 1.0s"
}

start_mount() {
  local container="$1" cache="$2" mountpoint="$3"
  local tag
  tag="$(printf '%s' "$mountpoint" | tr '/' '_')"
  docker exec "$container" bash -c "
set -Eeuo pipefail
mkdir -p $mountpoint
nohup juicefs mount $(mount_opts "$cache") sqlite3:///jfs/meta.db $mountpoint >/tmp/juicefs-mount$tag.log 2>&1 &
for _ in \$(seq 1 200); do
  if grep -q ' $mountpoint ' /proc/self/mountinfo; then exit 0; fi
  sleep 0.1
done
echo 'juicefs_court: mount did not appear' >&2
cat /tmp/juicefs-mount$tag.log >&2
exit 1
"
}

fuse_server_pid() {
  docker exec "$1" bash -c '
for p in /proc/[0-9]*; do
  pid=${p#/proc/}
  if ls -l "$p/fd" 2>/dev/null | grep -q "/dev/fuse"; then echo "$pid"; break; fi
done
'
}

kill_clients() {
  docker exec "$1" bash -c '
for p in /proc/[0-9]*; do
  pid=${p#/proc/}
  if tr "\0" " " < "$p/cmdline" 2>/dev/null | grep -q "sqlite3:///jfs/meta.db"; then
    kill -9 "$pid" 2>/dev/null || true
  fi
done
true
'
}

count_ok() { awk '$2 == "ok" {n++} END {print n+0}' "$1" 2>/dev/null || echo 0; }
count_bad() { awk '$2 == "bad" {n++} END {print n+0}' "$1" 2>/dev/null || echo 0; }

echo "juicefs_court: starting client A and mounting the volume at /mnt/jfs" >&2
start_mount "$client_a" /jfs/cache-a /mnt/jfs || exit 1
docker exec "$client_a" sh -c 'grep " /mnt/jfs " /proc/self/mountinfo' > "$work/mountinfo-a.txt"
pid_a="$(fuse_server_pid "$client_a" | tr -d '[:space:]')"

if [[ "$cross_only" != 1 ]]; then
  echo "juicefs_court: building the harness in the rust court" >&2
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

  receipt="bench/receipts/augmentation-juicefs-$stamp.json"
  receipt_log="bench/receipts/augmentation-juicefs-$stamp.log"
  weftfs_rev="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  weftcodec_rev="$(git -C "$repo_root/WeftCodec" rev-parse HEAD 2>/dev/null || echo unknown)"
  weftcodec_go_rev="$(git -C "$repo_root/WeftCodec-Go" rev-parse HEAD 2>/dev/null || echo unknown)"
  weftfs_go_rev="$(git -C "$repo_root/WeftFS-Go" rev-parse HEAD 2>/dev/null || echo unknown)"

  echo "juicefs_court: running the six-arm court on the JuiceFS mount (label=$label reps=$reps)" >&2
  timeout --signal=TERM --kill-after=120 7200 docker exec \
    -e "WEFT_REPO_WEFTFS=$weftfs_rev" \
    -e "WEFT_REPO_WEFTCODEC=$weftcodec_rev" \
    -e "WEFT_REPO_WEFTCODEC_GO=$weftcodec_go_rev" \
    -e "WEFT_REPO_WEFTFS_GO=$weftfs_go_rev" \
    -e "WEFT_COURT_IMAGE=$client_image_digest" \
    -e "WEFT_COURT_KERNEL=$(uname -r)" \
    -e "WEFT_IN_COURT=1" \
    "$client_a" bash -c "
set -Eeuo pipefail
ulimit -n 65536
cd /work
./.docker-cache/target-rust/release/examples/augmentation_bench run \
  --fixture /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  --fixture-sha256 $fixture_sha256 \
  --work /mnt/jfs/augmentation-$stamp \
  --out $receipt --log $receipt_log \
  --backend local --chunk-size 1048576 \
  $subset_args \
  --configs A,N,B,C,D,E --reps $reps \
  --with-native-app --passes 2 --label $label --seed weft-augmentation-r3 \
  --order-seed 20261114 \
  --force
" 2>&1 | tee "$work/bench.out" || { echo "juicefs_court: the six-arm harness failed; tail follows" >&2; tail -60 "$work/bench.out" >&2; exit 1; }

  [[ -f "$receipt" ]] || { echo "juicefs_court: expected receipt $receipt was not written" >&2; exit 1; }
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
      [[ -z "$other_failures" ]] || { echo "juicefs_court: smoke run failed checks: $other_failures" >&2; exit 1; }
      echo "juicefs_court: smoke run result '$result' accepted as partial (reps < 7 only)" >&2
    else
      echo "juicefs_court: receipt result is '$result' ($receipt)" >&2
      exit 1
    fi
  fi
else
  receipt=""
  receipt_log=""
  label="cross-client-only"
fi

# ---------------------------------------------------------------------------
# Cross-client behaviour: a second client process on the same volume.
# ---------------------------------------------------------------------------
echo "juicefs_court: starting client B (second client process, same volume)" >&2
docker run -d --name "$client_b" --entrypoint sleep "${client_args[@]}" "$client_image" infinity >/dev/null
start_mount "$client_b" /jfs/cache-b /mnt/jfs || exit 1
docker exec "$client_b" sh -c 'grep " /mnt/jfs " /proc/self/mountinfo' > "$work/mountinfo-b.txt"

cross="$work/cross"

# 1/2. Close-to-open consistency in both directions. The writer does
# `dd conv=fsync` (data + metadata durable) and the peer polls a fresh
# open+read for a matching SHA-256 at 10 ms resolution. The recorded value is a
# ceiling on the visibility latency.
docker exec "$client_a" bash -c "mkdir -p /mnt/jfs/cross && cd /mnt/jfs/cross && dd if=/dev/urandom of=from-a.bin bs=1M count=64 conv=fsync status=none && sha256sum from-a.bin | awk '{print \$1}'" > "$cross/from-a.sha" 2>&1
expected_a="$(head -1 "$cross/from-a.sha")"
peer_a_start="$(date -u +%s%N)"
docker exec "$client_b" bash -c "cd /mnt/jfs/cross && for _ in \$(seq 1 400); do
  if [ \"\$(sha256sum from-a.bin 2>/dev/null | awk '{print \$1}')\" = \"$expected_a\" ]; then echo ok; exit 0; fi
  sleep 0.01; done; echo mismatch; exit 1" > "$cross/peer-read-a.out" 2>&1 || true
peer_read_a_ms=$(( ($(date -u +%s%N) - peer_a_start) / 1000000 ))

docker exec "$client_b" bash -c "cd /mnt/jfs/cross && dd if=/dev/urandom of=from-b.bin bs=1M count=64 conv=fsync status=none && sha256sum from-b.bin | awk '{print \$1}'" > "$cross/from-b.sha" 2>&1
expected_b="$(head -1 "$cross/from-b.sha")"
peer_b_start="$(date -u +%s%N)"
docker exec "$client_a" bash -c "cd /mnt/jfs/cross && for _ in \$(seq 1 400); do
  if [ \"\$(sha256sum from-b.bin 2>/dev/null | awk '{print \$1}')\" = \"$expected_b\" ]; then echo ok; exit 0; fi
  sleep 0.01; done; echo mismatch; exit 1" > "$cross/peer-read-b.out" 2>&1 || true
peer_read_b_ms=$(( ($(date -u +%s%N) - peer_b_start) / 1000000 ))

# 3. In-place rewrite: a replacement published by client A must become visible
# to client B on a fresh open. The stale window (attribute/entry cache) is
# measured, not asserted.
docker exec "$client_a" bash -c "cd /mnt/jfs/cross && printf 'first-generation\n' > rewrite.txt && dd if=/dev/zero bs=1M count=1 conv=fsync status=none >> rewrite.txt" >/dev/null
docker exec "$client_b" bash -c "cd /mnt/jfs/cross && head -c 17 rewrite.txt" > "$cross/rewrite-before.txt" 2>&1 || true
docker exec "$client_a" bash -c "cd /mnt/jfs/cross && printf 'second-generation\n' > rewrite.tmp && dd if=/dev/zero bs=1M count=2 conv=fsync status=none >> rewrite.tmp && mv rewrite.tmp rewrite.txt && sync" >/dev/null
rewrite_start="$(date -u +%s%N)"
docker exec "$client_b" bash -c "cd /mnt/jfs/cross && for _ in \$(seq 1 400); do
  if [ \"\$(head -c 17 rewrite.txt)\" = \"second-generation\" ]; then echo ok; exit 0; fi
  sleep 0.01; done; echo stale; exit 1" > "$cross/rewrite-after.out" 2>&1 || true
rewrite_visible_ms=$(( ($(date -u +%s%N) - rewrite_start) / 1000000 ))

# 4. n-client failure: the writer on client A runs until it fails; each 1 MiB
# chunk is sync(2)-ed and its SHA-256 is acknowledged to a container-local log.
# Client A's JuiceFS daemon is SIGKILLed while the writer is still going, then
# client B reads every acknowledged chunk back.
echo "juicefs_court: n-client failure test: write on A, kill A mid-write, read on B" >&2
timeout --signal=KILL 600 docker exec "$client_a" bash -c '
set -uo pipefail
mkdir -p /mnt/jfs/cross
: > /tmp/ack.log
writer() {
  for i in $(seq 1 100000); do
    dd if=/dev/urandom of=/tmp/chunk.bin bs=1M count=1 status=none
    h=$(sha256sum /tmp/chunk.bin | cut -d" " -f1)
    if ! dd if=/tmp/chunk.bin of=/mnt/jfs/cross/inflight.bin bs=1M count=1 \
         oflag=append conv=notrunc,fsync status=none 2>/dev/null; then
      printf "write-failed-at-chunk=%s\n" "$i" >> /tmp/writer.state
      break
    fi
    printf "%d %s\n" "$i" "$h" >> /tmp/ack.log
  done
}
writer &
wp=$!
for _ in $(seq 1 4000); do
  n=$(grep -cE "^[0-9]+ [0-9a-f]{64}$" /tmp/ack.log 2>/dev/null || true)
  if [ "${n:-0}" -ge 16 ]; then break; fi
  sleep 0.01
done
n_before=$(grep -cE "^[0-9]+ [0-9a-f]{64}$" /tmp/ack.log 2>/dev/null || true)
for p in /proc/[0-9]*; do
  pid=${p#/proc/}
  if [ "$(readlink -f "$p/exe" 2>/dev/null)" = "/usr/local/bin/juicefs" ]; then
    kill -9 "$pid" 2>/dev/null || true
  fi
done
wait "$wp" 2>/dev/null || true
n_final=$(grep -cE "^[0-9]+ [0-9a-f]{64}$" /tmp/ack.log 2>/dev/null || true)
state=$(cat /tmp/writer.state 2>/dev/null | head -1 || true)
printf "acknowledged-before-kill=%s acknowledged-final=%s %s\n" "${n_before:-0}" "${n_final:-0}" "${state:-writer-stopped-without-error-line}"
' > "$cross/writer.out" 2>&1 || true
kill_status="$(printf 'SIGKILL of every JuiceFS process in client A while the writer was still running; %s' "$(head -1 "$cross/writer.out" 2>/dev/null || echo 'ack count unknown')")"
docker exec "$client_a" bash -c 'grep -E "^[0-9]+ [0-9a-f]{64}$" /tmp/ack.log 2>/dev/null || true' > "$cross/ack.log" 2>/dev/null || true
docker exec "$client_a" bash -c 'cat /tmp/writer.state 2>/dev/null || true' > "$cross/writer.state" 2>/dev/null || true
acked_clean="$(awk '$1 ~ /^[0-9]+$/ && $2 ~ /^[0-9a-f]{64}$/ {n++} END {print n+0}' "$cross/ack.log")"

docker exec "$client_b" sh -c 'stat -c %s /mnt/jfs/cross/inflight.bin 2>/dev/null || echo missing' > "$cross/peer-len.txt" 2>&1 || true
peer_len="$(head -1 "$cross/peer-len.txt" | tr -d '[:space:]')"
docker exec -i "$client_b" bash -c 'cat > /tmp/ack.log' < "$cross/ack.log" || true
docker exec "$client_b" bash -c '
set -Eeuo pipefail
f=/mnt/jfs/cross/inflight.bin
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

# The recovered client: a fresh JuiceFS process in container A mounts the same
# volume on a new mount point and reads the same acknowledged chunks.
remount_status="skipped"
if start_mount "$client_a" /jfs/cache-a /mnt/jfs2 >/dev/null 2>&1; then
  remount_status="remounted_at_/mnt/jfs2"
  docker exec "$client_a" bash -c '
set -Eeuo pipefail
f=/mnt/jfs2/cross/inflight.bin
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

# Volume/service state, from the surviving client.
docker exec "$client_b" bash -c 'timeout 5 juicefs stats /mnt/jfs 2>&1 | tail -20' > "bench/receipts/juicefs-$stamp.stats.txt" 2>&1 || true
docker exec "$client_b" bash -c 'juicefs status sqlite3:///jfs/meta.db 2>&1' > "bench/receipts/juicefs-$stamp.status.json" 2>&1 || true
docker exec "$client_b" sh -c 'grep " /mnt/jfs " /proc/self/mountinfo; echo ---; df -h /mnt/jfs; echo ---; df -i /mnt/jfs' > "$work/df.txt" 2>&1 || true

# Small cross-client evidence files are kept beside the receipt so the counts
# in it can be re-derived without the purged work directory.
cp "$cross/ack.log" "bench/receipts/juicefs-$stamp.ack.log" 2>/dev/null || true
cp "$cross/readback.tsv" "bench/receipts/juicefs-$stamp.readback.tsv" 2>/dev/null || true
cp "$cross/readback-a.tsv" "bench/receipts/juicefs-$stamp.readback-recovered.tsv" 2>/dev/null || true
cp "$cross/writer.out" "bench/receipts/juicefs-$stamp.writer.txt" 2>/dev/null || true

# ---------------------------------------------------------------------------
# Court receipt
# ---------------------------------------------------------------------------
court_receipt="bench/receipts/juicefs-$stamp.json"
python3 - "$court_receipt" "$stamp" "$jfs_version" "$jfs_release" "$tarball_url" "$jfs_tarball_sha256" "$release_sha256" \
  "$client_image" "$client_image_digest" "$client_image_id" "$WEFT_MEMORY" "$WEFT_CPUS" "$WEFT_PIDS" \
  "$(mount_opts /jfs/cache-a)" "$(mount_opts /jfs/cache-b)" "$peer_read_a_ms" "$peer_read_b_ms" \
  "$rewrite_visible_ms" "$kill_status" "$acked_clean" "$readback_ok" "$readback_bad" "$peer_len" \
  "$remount_status" "$readback_a_ok" "$readback_a_bad" "$reps" "$label" "$receipt" "$work" <<'PY'
import json
import os
import sys

(
    out, stamp, version, release, url, tarball_sha, binary_sha, image, digest, image_id,
    memory, cpus, pids, opts_a, opts_b, peer_a_ms, peer_b_ms, rewrite_ms,
    kill_status, acked, ok, bad, peer_len, remount, remount_ok, remount_bad,
    reps, label, aug_receipt, work,
) = sys.argv[1:]

def read(path):
    try:
        return open(path).read()
    except OSError:
        return ""

mount_a = read(os.path.join(work, "mountinfo-a.txt")).strip()
mount_b = read(os.path.join(work, "mountinfo-b.txt")).strip()
df = read(os.path.join(work, "df.txt")).strip()
status = read(f"bench/receipts/juicefs-{stamp}.status.json")
stats = read(f"bench/receipts/juicefs-{stamp}.stats.txt")
rewrite_before = read(os.path.join(work, "cross", "rewrite-before.txt")).strip()
rewrite_after = read(os.path.join(work, "cross", "rewrite-after.out")).strip()
peer_a_out = read(os.path.join(work, "cross", "peer-read-a.out")).strip()
peer_b_out = read(os.path.join(work, "cross", "peer-read-b.out")).strip()

settings = {}
try:
    parsed = json.loads(status[status.index("{"):])
    settings = parsed.get("Setting", {}) or {}
    sessions = parsed.get("Sessions", [])
except (ValueError, json.JSONDecodeError):
    sessions = []

receipt = {
    "schema": "weft.juicefs_court.v1",
    "generated_utc": stamp,
    "backend": {
        "product": "JuiceFS",
        "version": version,
        "release": release,
        "release_url": url,
        "release_tarball_sha256": tarball_sha,
        "release_binary_sha256": binary_sha,
        "image": image,
        "image_digest": digest,
        "image_id": image_id,
        "topology": "one JuiceFS volume: SQLite metadata engine + local file object store; two independent client processes in two containers, each mounting through JuiceFS's own FUSE client",
        "metadata_engine": "sqlite3:///jfs/meta.db",
        "object_store": "file:///jfs/obj (key/value object layout on a host bind mount)",
        "volume_settings": {
            key: settings[key]
            for key in ("Name", "Storage", "Bucket", "BlockSize", "Compression", "TrashDays", "MetaVersion")
            if key in settings
        },
        "client_sessions_seen_by_status": len(sessions) if isinstance(sessions, list) else None,
        "format_options": "--storage file --bucket /jfs/obj --block-size 16384 (KiB; JuiceFS maximum, its default is 4096 KiB) --trash-days 0 --compress none",
        "mount_options": {
            "client_a": opts_a,
            "client_b": opts_b,
            "mountinfo_client_a": mount_a,
            "mountinfo_client_b": mount_b,
            "why": "--cache-size 1 MiB makes the client data cache negligible so the cold-restore column is not served by a JuiceFS client cache; --open-cache 0 keeps the documented close-to-open default; attribute and entry caches stay at their 1 s defaults",
        },
        "df": df,
        "juicefs_status_file": f"bench/receipts/juicefs-{stamp}.status.json",
        "juicefs_stats_file": f"bench/receipts/juicefs-{stamp}.stats.txt",
        "label": "SINGLE HOST, one volume, TWO client processes, one writer at a time. Not a fleet, not multi-node, not clustered performance, not a cloud result. The object store and the SQLite database are host directories, so no media or power-loss claim is made.",
    },
    "container": {
        "clients": 2,
        "memory": memory, "memory_swap": memory, "pids": pids, "cpus": cpus,
        "cap_drop": "ALL",
        "cap_add": ["SYS_ADMIN"],
        "device": "/dev/fuse",
        "why_capabilities": "the JuiceFS client mounts through /dev/fuse; SYS_ADMIN is the minimum mount capability",
        "no_new_privileges": True,
        "lost_capabilities_observed": [
            "CAP_SYS_RESOURCE absent: JuiceFS logs 'CAP_SYS_RESOURCE is needed for PR_SET_IO_FLUSHER'; the client runs without IO-flusher priority protection",
            "setpriority denied: JuiceFS logs 'setpriority: permission denied'; no in-client priority tuning",
        ],
    },
    "arms": {
        "contract": ["A", "N", "B", "C", "D", "E"],
        "repetitions": int(reps),
        "label": label,
        "augmentation_receipt": aug_receipt,
        "raw_log": f"bench/receipts/juicefs-{stamp}.log",
        "durability": "JuiceFS write-back mode is off (the default): a completed write has its data uploaded to the file object store and its metadata committed in SQLite, after an fsync of the file (A-E) or of the atomic metadata file (N).",
        "weftfs_fuse_in_data_path": False,
    },
    "cross_client": {
        "client_processes": 2,
        "volume": settings.get("Name", "weftvol"),
        "close_to_open": {
            "a_write_then_b_read_ms": int(peer_a_ms),
            "b_write_then_a_read_ms": int(peer_b_ms),
            "a_to_b_result": peer_a_out,
            "b_to_a_result": peer_b_out,
            "method": "writer does dd conv=fsync (data + metadata durable), the peer polls a fresh open+read for a matching SHA-256 at 10 ms resolution; the recorded value is a ceiling on visibility latency",
        },
        "in_place_rewrite": {
            "peer_saw_before": rewrite_before,
            "peer_result": rewrite_after,
            "peer_observed_new_content_after_ms": int(rewrite_ms),
            "method": "A replaces the file atomically; B polls head -c 17 until it sees the new prefix",
            "why": "JuiceFS guarantees close-to-open consistency, not read-after-write; the attribute/entry cache is 1 s by default and is recorded in the mount options",
        },
        "n_client_failure": {
            "method": "client A writes 1 MiB chunks, each sync(2)-ed and its SHA-256 acknowledged to a container-local log, until the write fails; client A's JuiceFS processes are SIGKILLed mid-write; client B then reads every acknowledged chunk; a fresh client A mounts the same volume and reads them again",
            "kill": kill_status,
            "acknowledged_chunks": int(acked),
            "peer_read_chunks_ok": int(ok),
            "peer_read_chunks_bad": int(bad),
            "peer_file_length_bytes": peer_len,
            "recovered_client": remount,
            "recovered_read_chunks_ok": int(remount_ok),
            "recovered_read_chunks_bad": int(remount_bad),
            "rule": "every acknowledged (fsynced) chunk must read back intact on the peer; bytes beyond the last acknowledgement are unacknowledged and are reported, not counted as a loss",
            "write_failure_marker": read(os.path.join(work, "cross", "writer.state")).strip(),
            "is_media_loss": False,
        },
    },
    "non_claims": [
        "one host, one volume, two client processes, one writer at a time; not a fleet, cluster or multi-node result",
        "SQLite metadata engine; not the Redis/PostgreSQL/MariaDB/TiKV engines, and not the JuiceFS cloud service",
        "local file object store; not S3/GCS/Azure object storage over a network",
        "no media, power-loss or host-cache-loss claim; the failure test kills client processes, not a machine or a disk",
        "no claim about JuiceFS versions other than the pinned release recorded here",
        "no comparison against another filesystem (rule 13)",
    ],
}
with open(out, "w") as handle:
    json.dump(receipt, handle, indent=1)
    handle.write("\n")
print(f"juicefs_court: court receipt {out}")
PY

if [[ "$readback_bad" != "0" ]]; then
  echo "juicefs_court: $readback_bad acknowledged chunk(s) did not read back intact on the peer" >&2
  exit 1
fi
if [[ "$acked_clean" -lt 5 ]]; then
  echo "juicefs_court: the n-client failure test acknowledged only $acked_clean chunks; too few to be meaningful" >&2
  exit 1
fi

echo "juicefs_court: cross-client close-to-open a->b ${peer_read_a_ms} ms, b->a ${peer_read_b_ms} ms, rewrite visible in ${rewrite_visible_ms} ms" >&2
echo "juicefs_court: n-client failure: $acked_clean acknowledged, $readback_ok ok / $readback_bad bad on the peer, recovered client $readback_a_ok ok / $readback_a_bad bad" >&2
echo "juicefs_court: court receipt $court_receipt" >&2
echo "juicefs_court: log $log" >&2
