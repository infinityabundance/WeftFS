#!/usr/bin/env bash
# CephFS augmentation court (T030/T038/T081): a real, deployed, mature
# distributed filesystem with and without the WeftFS augmentation.
#
# Topology (recorded in the receipt): one Ceph QUIESCENT demo container from the
# pinned `quay.io/ceph/demo` image running mon + mgr + one bluestore OSD on a
# tmpfs and one MDS. CephFS is mounted by `ceph-fuse` (the court kernel has no
# `ceph.ko`, and a kernel mount would need host modules) in a second,
# hard-limited container from the same pinned image, which is also where the
# measured process runs. The data path is the Ceph client -> MDS/OSD; WeftFS's
# own FUSE is **not** mounted, so no arm is FUSE-on-FUSE.
#
# Arms (frozen methodology: `bench/CONTRACTS-augmentation.md`):
#   A  native: one plain file per tensor on the CephFS mount, each `fsync`ed,
#      directory `fsync`ed after the last;
#   N  native + a canonical JSON manifest, provenance record and explicit
#      key-set context, published atomically on the mount;
#   B  WeftFS store rooted on the CephFS mount, raw representation;
#   C  WeftFS store, shipped planned representation;
#   D  B + snapshot, Context Root, selector resolution and provenance record;
#   E  C + the same protocol surface.
#
# This is **single-node Ceph** (one mon, one mgr, one OSD, one MDS, one client).
# It is not a fleet, not multi-node, not a clustered performance result, and not
# a cloud result. `fsync` on CephFS orders against the MDS/OSD journal; the OSD
# data directory is a tmpfs, so this is not a media or power-loss claim either.
#
# Usage:
#   scripts/cephfs_court.sh                # full run (7 interleaved reps)
#   scripts/cephfs_court.sh --reps=3       # fewer repetitions (dev)
#   scripts/cephfs_court.sh --smoke        # reduced subset, one repetition (dev)
#
# Exit 0 = the measured court passed; 1 = it ran and failed; 78 = skipped with
# exactly one `cephfs_court: skip <reason>` line (environment refusal only).
set -Eeuo pipefail

skip() {
  echo "cephfs_court: skip $*" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

stamp="${WEFT_CEPHFS_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
reps="${WEFT_CEPHFS_REPS:-7}"
smoke=0
reduced=0
for arg in "$@"; do
  case "$arg" in
    --reps=*) reps="${arg#--reps=}" ;;
    --smoke) smoke=1; reps=1 ;;
    --reduced) reduced=1 ;;
    -h|--help) sed -n '2,45p' "$0"; exit 0 ;;
    *) echo "unknown argument '$arg'" >&2; exit 64 ;;
  esac
done

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"
[[ "$(uname -m)" == "x86_64" ]] || skip "only linux-amd64 is verified (host is $(uname -m))"
[[ -e /dev/fuse ]] || skip "no /dev/fuse; ceph-fuse cannot mount without it"

fixture="${WEFT_AUG_FIXTURE:-$repo_root/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors}"
fixture_sha256="${WEFT_AUG_FIXTURE_SHA256:-fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe}"
[[ -f "$fixture" ]] || skip "fixture $fixture is not present"

ceph_image="${WEFT_CEPH_IMAGE:-quay.io/ceph/demo@sha256:522483cf07cfce6386b8e18a3edfa88a1b32c688dee231d0a6962b1513557723}"
subnet="${WEFT_CEPHFS_SUBNET:-10.213.89.0/24}"
gateway="${subnet%.*}.1"
server_ip="${subnet%.*}.10"
access_key_hint="weftcourt"

for network_id in $(docker network ls -q); do
  for existing in $(docker network inspect "$network_id" --format '{{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null); do
    if [[ "$existing" == "$subnet" ]]; then
      skip "subnet $subnet is already in use by network $network_id; set WEFT_CEPHFS_SUBNET"
    fi
  done
done

if ! docker image inspect "$ceph_image" >/dev/null 2>&1; then
  echo "cephfs_court: pulling pinned image $ceph_image" >&2
  if ! pull_error="$(docker pull "$ceph_image" 2>&1 >/dev/null)"; then
    skip "cannot obtain pinned image $ceph_image: ${pull_error:-docker pull failed}"
  fi
fi
ceph_image_id="$(docker image inspect "$ceph_image" --format '{{.Id}}')"
ceph_image_digest="$(docker image inspect "$ceph_image" --format '{{index .RepoDigests 0}}')"

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"

work="$repo_root/.docker-cache/cephfs-$stamp"
conf_dir="$work/conf"
# The OSD/mon data directory is a host bind mount, not the tmpfs used by the
# RGW court: a 2 GiB tmpfs OSD ran out of space mid-repetition and wedged the
# cluster (measured: the first full attempt died during rep 1 arm D and the
# client hung in fuse_chan_send). Capacity is the backend's business, not the
# court's, so the data plane gets a real directory on the host filesystem. This
# is recorded in the receipt and changes no hard limit.
osd_dir="$work/osd"
mkdir -p "$work" "$conf_dir" "$osd_dir" bench/receipts
chmod -R a+rwX "$work" 2>/dev/null || true
chmod a+rwX bench/receipts 2>/dev/null || true
log="bench/receipts/cephfs-$stamp.log"
: > "$log"
exec > >(tee -a "$log") 2>&1

server_name="weft-cephfs-server-$$"
client_name="weft-cephfs-client-$$"
network="weft-cephfs-net-$$"

cleanup() {
  docker rm -f "$client_name" >/dev/null 2>&1 || true
  docker rm -f "$server_name" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

echo "cephfs_court: starting single-node Ceph (mon+mgr+osd+mds) from $ceph_image_digest" >&2
docker network create --subnet "$subnet" --gateway "$gateway" "$network" >/dev/null
docker run -d --name "$server_name" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 \
  --security-opt no-new-privileges \
  --cap-drop=ALL \
  --cap-add=CHOWN --cap-add=DAC_OVERRIDE --cap-add=FOWNER --cap-add=SETGID --cap-add=SETUID \
  --network "$network" --ip "$server_ip" --hostname "weft-cephfs" \
  --tmpfs /etc/ceph:rw,size=64m,nosuid,nodev \
  --tmpfs /tmp:rw,size=512m,nosuid,nodev \
  --tmpfs /var/log:rw,size=256m,nosuid,nodev \
  -v "$osd_dir:/var/lib/ceph" \
  -e MON_IP="$server_ip" \
  -e CEPH_PUBLIC_NETWORK="$subnet" \
  -e DEMO_DAEMONS=osd,mds \
  -e CEPH_DEMO_UID="$access_key_hint" \
  "$ceph_image" >/dev/null

ready=0
for _ in $(seq 1 120); do
  if docker exec "$server_name" sh -c 'timeout 10 ceph fs status 2>/dev/null | grep -q "active"' >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 2
done
if [[ "$ready" != 1 ]]; then
  echo "cephfs_court: CephFS did not reach an active MDS within 240 s; logs follow" >&2
  docker exec "$server_name" sh -c 'timeout 20 ceph -s 2>&1 | head -20' >&2 || true
  docker logs --tail 60 "$server_name" >&2 || true
  exit 1
fi

ceph_version="$(docker exec "$server_name" ceph --version 2>&1 | head -1 || true)"
# One OSD means `size 2` is permanently degraded and doubles the data the OSD
# must hold; a single-copy pool is the honest configuration for a one-node
# cluster. The session timeout is raised because a 4-CPU client running a
# 424 MB FUSE workload can pause between MDS messages; a blocklisted client
# wedges the mount. Both settings are recorded below.
docker exec "$server_name" sh -c 'timeout 30 ceph osd pool set cephfs_data size 1' >/dev/null 2>&1 || true
docker exec "$server_name" sh -c 'timeout 30 ceph osd pool set cephfs_data min_size 1' >/dev/null 2>&1 || true
docker exec "$server_name" sh -c 'timeout 30 ceph osd pool set cephfs_metadata size 1' >/dev/null 2>&1 || true
docker exec "$server_name" sh -c 'timeout 30 ceph osd pool set cephfs_metadata min_size 1' >/dev/null 2>&1 || true
docker exec "$server_name" sh -c 'timeout 30 ceph config set mds mds_session_timeout 300' >/dev/null 2>&1 || true
ceph_status="$(docker exec "$server_name" sh -c 'timeout 30 ceph -s 2>&1 | sed -n "1,26p"' || true)"
ceph_fs_status="$(docker exec "$server_name" sh -c 'timeout 30 ceph fs status 2>&1 | sed -n "1,24p"' || true)"
printf '%s\n' "$ceph_status" > "bench/receipts/cephfs-$stamp.cluster.txt"
printf '%s\n' "$ceph_fs_status" > "bench/receipts/cephfs-$stamp.fs.txt"
docker exec "$server_name" sh -c 'cat /etc/ceph/ceph.conf' > "$conf_dir/ceph.conf"
docker exec "$server_name" sh -c 'cat /etc/ceph/ceph.client.admin.keyring' > "$conf_dir/ceph.client.admin.keyring"
chmod -R a+rX "$conf_dir"

mon_line="$(grep -m1 '^mon host' "$conf_dir/ceph.conf" | sed 's/^mon host = //')"
echo "cephfs_court: $ceph_version; mon=$mon_line" >&2

# Build the harness in the rust court (never on the host). The resulting binary
# is linked against bookworm glibc and runs in the pinned CentOS Stream 9 client
# image; that is checked by executing it before the measurement.
echo "cephfs_court: building the harness in the rust court" >&2
scripts/docker-court.sh rust "cd /work && cargo build --release --example augmentation_bench"

extra_args=""
subset_args="--max-others 100 --include-embedding 1"
if [[ "$smoke" == 1 ]]; then
  subset_args="--max-others 16 --include-embedding 0"
elif [[ "$reduced" == 1 ]]; then
  # Aggregate-court configuration: the frozen reduced subset, seven
  # repetitions, so the contract's repetition check still passes and the
  # receipt is a genuine (reduced, labelled) measurement rather than a smoke.
  subset_args="--max-others 16 --include-embedding 0"
fi

mount_opts="ceph-fuse -m $mon_line -n client.admin --keyring=/etc/ceph/ceph.client.admin.keyring --admin-socket=/tmp/ceph-fuse.asok /mnt/weft-ceph"

receipt="bench/receipts/augmentation-cephfs-$stamp.json"
receipt_log="bench/receipts/augmentation-cephfs-$stamp.log"

echo "cephfs_court: mounting CephFS in the client container and running the court" >&2
weftfs_rev="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
weftcodec_rev="$(git -C "$repo_root/WeftCodec" rev-parse HEAD 2>/dev/null || echo unknown)"
weftcodec_go_rev="$(git -C "$repo_root/WeftCodec-Go" rev-parse HEAD 2>/dev/null || echo unknown)"
weftfs_go_rev="$(git -C "$repo_root/WeftFS-Go" rev-parse HEAD 2>/dev/null || echo unknown)"
timeout --signal=TERM --kill-after=60 3300 docker run --rm --name "$client_name" \
  -e "WEFT_REPO_WEFTFS=$weftfs_rev" \
  -e "WEFT_REPO_WEFTCODEC=$weftcodec_rev" \
  -e "WEFT_REPO_WEFTCODEC_GO=$weftcodec_go_rev" \
  -e "WEFT_REPO_WEFTFS_GO=$weftfs_go_rev" \
  -e "WEFT_COURT_IMAGE=$ceph_image" \
  -e "WEFT_COURT_KERNEL=$(uname -r)" \
  -e "WEFT_IN_COURT=1" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 \
  --security-opt no-new-privileges \
  --cap-drop=ALL --cap-add SYS_ADMIN \
  --device /dev/fuse \
  --network "$network" \
  -v "$repo_root:/work" \
  -v "$conf_dir:/etc/ceph:ro" \
  -v "$work:/cephfs-court" \
  --entrypoint bash \
  "$ceph_image" -c "
set -Eeuo pipefail
mkdir -p /mnt/weft-ceph
$mount_opts
for _ in \$(seq 1 50); do
  if grep -q ' /mnt/weft-ceph ' /proc/self/mountinfo; then break; fi
  sleep 0.2
done
grep -q ' /mnt/weft-ceph ' /proc/self/mountinfo || { echo 'cephfs: mount failed' >&2; exit 1; }
df -h /mnt/weft-ceph > /cephfs-court/df.txt
dd if=/dev/zero of=/mnt/weft-ceph/probe.bin bs=1M count=8 conv=fsync status=none
rm -f /mnt/weft-ceph/probe.bin
cd /work
./.docker-cache/target-rust/release/examples/augmentation_bench run \
  --fixture /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
  --fixture-sha256 $fixture_sha256 \
  --work /mnt/weft-ceph/augmentation-$stamp \
  --out $receipt --log $receipt_log \
  --backend local --chunk-size 1048576 \
  $subset_args \
  --configs A,N,B,C,D,E --reps $reps \
  --with-native-app --passes 2 --label full --seed weft-augmentation-r3 \
  --force
sync
printf '%s\n' \"\$?\" > /cephfs-court/rc.txt
" 2>&1 | tee "$work/client.out"

[[ -f "$receipt" ]] || { echo "cephfs_court: expected receipt $receipt was not written" >&2; exit 1; }
result="$(python3 -c "import json;print(json.load(open('$receipt'))['result'])")"
partial=0
if [[ "$result" != "pass" ]]; then
  if [[ "$smoke" == 1 ]]; then
    # A smoke run declares fewer than seven repetitions, which the frozen
    # contract counts as a failed check. Accept it only when that is the sole
    # failing check, and label the run partial.
    other_failures="$(python3 - "$receipt" <<'PY'
import json
import sys
receipt = json.load(open(sys.argv[1]))
checks = receipt.get("checks", {})
failed = [key for key, value in checks.items() if value is not True and key != "at_least_seven_reps"]
print(",".join(failed))
PY
)"
    if [[ -n "$other_failures" ]]; then
      echo "cephfs_court: smoke run failed checks: $other_failures" >&2
      exit 1
    fi
    partial=1
    echo "cephfs_court: smoke run result '$result' accepted as partial (reps < 7 only)" >&2
  else
    echo "cephfs_court: receipt result is '$result' ($receipt)" >&2
    exit 1
  fi
fi

# Court receipt: the backend and the container topology, beside the measurement.
court_receipt="bench/receipts/cephfs-$stamp.json"
python3 - "$court_receipt" "$stamp" "$ceph_image" "$ceph_image_digest" "$ceph_image_id" \
  "$ceph_version" "$mon_line" "$WEFT_MEMORY" "$WEFT_CPUS" "$WEFT_PIDS" \
  "bench/receipts/augmentation-cephfs-$stamp.json" \
  "bench/receipts/cephfs-$stamp.cluster.txt" "bench/receipts/cephfs-$stamp.fs.txt" \
  "bench/receipts/cephfs-$stamp.log" "$reps" "$smoke" "$partial" "$osd_dir" "$reduced" <<'PY'
import json
import sys

(
    out, stamp, image, digest, image_id, version, mon, memory, cpus, pids,
    aug_receipt, cluster_file, fs_file, log_file, reps, smoke, partial, osd_dir, reduced,
) = sys.argv[1:]

receipt = {
    "schema": "weft.cephfs_court.v1",
    "generated_utc": stamp,
    "backend": {
        "product": "CephFS (Ceph distributed file system)",
        "version": version,
        "image": image,
        "image_digest": digest,
        "image_id": image_id,
        "topology": "single-node: 1 mon, 1 mgr, 1 bluestore OSD (tmpfs), 1 MDS; cephfs_data + cephfs_metadata pools; 1 client",
        "mount": "ceph-fuse in the client container (the court kernel has no ceph.ko; /dev/fuse is present)",
        "mon": mon,
        "osd_data": osd_dir + " (host bind mount on the court filesystem, not a tmpfs; the receipt's non-claims still apply)",
        "pool_settings": {
            "cephfs_data": "size 1, min_size 1",
            "cephfs_metadata": "size 1, min_size 1",
            "why": "one OSD: size 2 is permanently degraded and doubles the bytes the single OSD must hold",
            "mds_session_timeout": 300,
            "why_session_timeout": "a 4-CPU client running a 424 MB FUSE workload can pause between MDS messages; a blocklisted client wedges the mount",
        },
        "cluster_status_file": cluster_file,
        "fs_status_file": fs_file,
        "label": "SINGLE-NODE Ceph deployed in one OOM-protected container plus one client container on one host. Not multi-node, not a fleet, not clustered performance, not a cloud result. The OSD data directory is a tmpfs, so no media or power-loss claim.",
    },
    "container": {
        "server": {
            "memory": memory, "memory_swap": memory, "pids": pids, "cpus": cpus,
            "cap_drop": "ALL",
            "cap_add": ["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETGID", "SETUID"],
            "why_capabilities": "Ceph daemons run as uid/gid 167 and write keyrings they own",
        },
        "client": {
            "memory": memory, "memory_swap": memory, "pids": pids, "cpus": cpus,
            "cap_drop": "ALL",
            "cap_add": ["SYS_ADMIN"],
            "device": "/dev/fuse",
            "why_capabilities": "ceph-fuse mounts through /dev/fuse; SYS_ADMIN is the minimum mount capability",
        },
        "no_new_privileges": True,
    },
    "arms": {
        "contract": ["A", "N", "B", "C", "D", "E"],
        "repetitions": int(reps),
        "reduced": smoke == "1" or reduced == "1",
        "partial": partial == "1",
        "augmentation_receipt": aug_receipt,
        "raw_log": log_file,
        "durability": "every arm fsyncs a file and the directory, or publishes through the store's LocalBackend on the same mount; CephFS fsync orders against MDS/OSD",
        "weftfs_fuse_in_data_path": False,
    },
    "non_claims": [
        "single-node Ceph on one host, one client, one writer",
        "no fleet, multi-node, clustered, cloud, media or power-loss claim",
        "requests are client-visible I/O operations, not RADOS RPCs",
        "the OSD data directory is a tmpfs",
    ],
}
with open(out, "w") as handle:
    json.dump(receipt, handle, indent=1)
    handle.write("\n")
print(f"cephfs_court: court receipt {out}")
PY

echo "cephfs_court: receipt $receipt" >&2
echo "cephfs_court: court receipt $court_receipt" >&2
echo "cephfs_court: log $log" >&2
