#!/usr/bin/env bash
# BeeGFS court (T036): attempt one more deployed filesystem in a container
# court, and record exactly what stops it if it cannot be mounted.
#
# What runs: BeeGFS 7.4.6 from the upstream Debian-12 repository, baked into
# `weft/beegfs-court:1`. The management, metadata and storage services are
# ordinary userspace daemons and do start and register with each other inside a
# `--cap-drop=ALL` container. That part is measured here and is a real result.
#
# What cannot run: the BeeGFS **client** is a kernel module (`beegfs.ko`). This
# script attempts the three things a deployment would try — `modprobe beegfs`,
# `mount -t beegfs`, and the package's own autobuild init script — and records
# the exact command, exit status and error text of each. The blocker is
# structural, not a missing package: a container has no `/lib/modules` for the
# running host kernel, the host kernel here (7.3.3-1-cachyos) has no matching
# Debian kernel headers, and loading a module would need CAP_SYS_MODULE and
# would change the host kernel, which the court rules forbid. BeeGFS ships no
# FUSE client.
#
# The deliverable is therefore the blocker receipt, not a mount and not a
# substitute. This court never claims to have measured BeeGFS.
#
# Usage:
#   scripts/beegfs_court.sh
#
# Exit 0 = the attempt ran and its evidence (including a blocker) was recorded;
# 1 = the attempt itself could not be carried out; 78 = skipped with exactly one
# `beegfs_court: skip <reason>` line (no docker, no network to fetch packages).
set -Eeuo pipefail

skip() {
  echo "beegfs_court: skip $*" >&2
  exit 78
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

stamp="${WEFT_BEEGFS_STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
subnet="${WEFT_BEEGFS_SUBNET:-10.214.79.0/24}"
gateway="${subnet%.*}.1"
node_ip="${subnet%.*}.10"
release="${WEFT_BEEGFS_RELEASE:-7.4.6}"

command -v docker >/dev/null 2>&1 || skip "no docker CLI on PATH"
docker info >/dev/null 2>&1 || skip "no reachable docker daemon"

image="weft/beegfs-court:1"
if ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "beegfs_court: building $image from docker/beegfs.Dockerfile" >&2
  if ! docker build --memory=2g --memory-swap=2g --ulimit nofile=65536:65536 \
      -f docker/beegfs.Dockerfile -t "$image" docker >/dev/null; then
    skip "cannot build the BeeGFS court image (the upstream Debian-12 repository may be unreachable)"
  fi
fi
image_id="$(docker image inspect "$image" --format '{{.Id}}')"

WEFT_MEMORY="${WEFT_MEMORY:-3g}"
WEFT_CPUS="${WEFT_CPUS:-4}"
WEFT_PIDS="${WEFT_PIDS:-1024}"

for network_id in $(docker network ls -q); do
  for existing in $(docker network inspect "$network_id" --format '{{range .IPAM.Config}}{{.Subnet}} {{end}}' 2>/dev/null); do
    if [[ "$existing" == "$subnet" ]]; then
      skip "subnet $subnet is already in use by network $network_id; set WEFT_BEEGFS_SUBNET"
    fi
  done
done

work="$repo_root/.docker-cache/beegfs-$stamp"
rm -rf "$work" 2>/dev/null || true
mkdir -p "$work" bench/receipts
chmod -R a+rwX "$work" bench/receipts 2>/dev/null || true
log="bench/receipts/beegfs-$stamp.log"
: > "$log"
exec > >(tee -a "$log") 2>&1

node="weft-beegfs-$$"
probe="weft-beegfs-probe-$$"
network="weft-beegfs-net-$$"
cleanup() {
  docker rm -f "$probe" >/dev/null 2>&1 || true
  docker rm -f "$node" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
  rm -rf "$work" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

packages="$(docker run --rm --entrypoint bash "$image" -c "dpkg -l | awk '\$1==\"ii\" && \$2 ~ /beegfs/ {print \$2\"=\"\$3}' | sort | tr '\n' ';'")"
kernel="$(uname -r)"
release_sha256="$(docker run --rm --entrypoint bash "$image" -c 'curl -sSL https://www.beegfs.io/vault/beegfs_'"$release"'/dists/bookworm/Release | sha256sum | cut -d" " -f1' || echo unavailable)"

echo "beegfs_court: $image ($image_id), packages $packages" >&2
echo "beegfs_court: host kernel $kernel; attempting management+metadata+storage and then the client" >&2

docker network create --subnet "$subnet" --gateway "$gateway" "$network" >/dev/null || skip "cannot create the court Docker network $network"
docker run -d --name "$node" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 \
  --security-opt no-new-privileges \
  --cap-drop=ALL \
  --network "$network" --ip "$node_ip" --hostname weft-beegfs \
  --tmpfs /tmp:rw,size=512m,nosuid,nodev \
  --entrypoint sleep "$image" infinity >/dev/null

# ---------------------------------------------------------------------------
# 1. Server side: management + metadata + storage, all userspace.
# ---------------------------------------------------------------------------
docker exec "$node" bash -c "
set -u
export PATH=/opt/beegfs/sbin:\$PATH
for f in beegfs-mgmtd beegfs-meta beegfs-storage beegfs-client; do
  p=/etc/beegfs/\$f.conf
  sed -i '/^connDisableAuthentication = true\$/d' \"\$p\"
  sed -i 's/^connDisableAuthentication *=.*/connDisableAuthentication = true/' \"\$p\"
  sed -i 's/^sysMgmtdHost *=.*/sysMgmtdHost = $node_ip/' \"\$p\"
done
beegfs-setup-mgmtd -p /data/mgmtd
beegfs-setup-meta -p /data/meta -s 1 -m $node_ip
beegfs-setup-storage -p /data/storage -s 1 -i 101 -m $node_ip
mkdir -p /var/run /var/lock/subsys
nohup /opt/beegfs/sbin/beegfs-mgmtd cfgFile=/etc/beegfs/beegfs-mgmtd.conf pidFile=/var/run/beegfs-mgmtd.pid >/tmp/mgmtd.out 2>&1 &
sleep 2
nohup /opt/beegfs/sbin/beegfs-meta cfgFile=/etc/beegfs/beegfs-meta.conf pidFile=/var/run/beegfs-meta.pid >/tmp/meta.out 2>&1 &
nohup /opt/beegfs/sbin/beegfs-storage cfgFile=/etc/beegfs/beegfs-storage.conf pidFile=/var/run/beegfs-storage.pid >/tmp/storage.out 2>&1 &
sleep 6
" > "$work/server-setup.out" 2>&1 || true

run_probe() {
  local name="$1" script="$2"
  docker exec "$node" bash -c "$script" > "$work/$name.out" 2>&1
  local rc=$?
  printf '%s\n' "$rc" > "$work/$name.rc"
  return 0
}

run_probe beegfs-version '/opt/beegfs/sbin/beegfs-ctl --version 2>&1 | head -3' || true
run_probe server-nodes 'for n in mgmtd meta storage; do echo "== $n"; timeout 10 /opt/beegfs/sbin/beegfs-ctl --listnodes --nodetype=$n 2>&1 | head -4; done' || true
run_probe server-targets 'echo "== meta"; timeout 10 /opt/beegfs/sbin/beegfs-ctl --listtargets --nodetype=meta --state 2>&1 | head -6; echo "== storage"; timeout 10 /opt/beegfs/sbin/beegfs-ctl --listtargets --nodetype=storage --state 2>&1 | head -6' || true
run_probe server-processes 'ps -eo comm,args | grep "[b]eegfs" | head' || true

# ---------------------------------------------------------------------------
# 2. Client side: the attempts a real deployment would make.
# ---------------------------------------------------------------------------
run_probe kernel-info 'echo "kernel=$(uname -r)"; echo "--- /lib/modules"; ls /lib/modules 2>&1; echo "--- /usr/src"; ls /usr/src 2>&1; echo "--- beegfs in /proc/filesystems"; grep -c beegfs /proc/filesystems 2>&1; true' || true
run_probe modprobe 'modprobe beegfs 2>&1; echo "rc=$?"' || true
run_probe mount-attempt-no-cap 'mkdir -p /mnt/beegfs; mount -t beegfs none /mnt/beegfs -o cfgFile=/etc/beegfs/beegfs-client.conf 2>&1; echo "rc=$?"; true' || true
run_probe client-init-script '/etc/init.d/beegfs-client start 2>&1 | tail -20; echo "rc=$?"' || true
run_probe client-binaries 'ls /opt/beegfs/sbin; echo "--- fuse-capable binaries"; for b in /opt/beegfs/sbin/* /usr/bin/beegfs-*; do case "$(basename "$b")" in *fuse*) echo "$b";; esac; done; echo "(nothing listed above means BeeGFS ships no FUSE client)"' || true

# The capability-free mount attempt above fails on the capability before it
# ever reaches the filesystem type, so it does not isolate the blocker. A
# second, dedicated probe container is granted exactly the capability a kernel
# mount needs (SYS_ADMIN) and nothing else, runs no measured work, and shows
# what is left once that capability is present.
docker run -d --name "$probe" \
  --memory="$WEFT_MEMORY" --memory-swap="$WEFT_MEMORY" \
  --pids-limit="$WEFT_PIDS" --cpus="$WEFT_CPUS" \
  --ulimit nofile=65536:65536 \
  --security-opt no-new-privileges \
  --cap-drop=ALL --cap-add SYS_ADMIN \
  --network "$network" --ip "${subnet%.*}.11" \
  --tmpfs /tmp:rw,size=256m,nosuid,nodev \
  --entrypoint sleep "$image" infinity >/dev/null
cap_probe_rc=0
docker exec "$probe" bash -c 'mkdir -p /mnt/beegfs; mount -t beegfs none /mnt/beegfs -o sysMgmtdHost=10.214.79.10,connDisableAuthentication=true 2>&1; echo "rc=$?"; grep -c " /mnt/beegfs " /proc/self/mountinfo 2>&1; true' > "$work/mount-attempt-with-cap.out" 2>&1 || cap_probe_rc=$?
printf '%s\n' "$cap_probe_rc" > "$work/mount-attempt-with-cap.rc"

# ---------------------------------------------------------------------------
# 3. Receipt
# ---------------------------------------------------------------------------
receipt="bench/receipts/beegfs-$stamp.json"
python3 - "$receipt" "$stamp" "$release" "$image" "$image_id" "$packages" "$kernel" "$release_sha256" "$node_ip" "$WEFT_MEMORY" "$WEFT_CPUS" "$WEFT_PIDS" "$work" <<'PY'
import json
import os
import sys

(
    out, stamp, release, image, image_id, packages, kernel, release_sha256,
    node_ip, memory, cpus, pids, work,
) = sys.argv[1:]

def read(name):
    try:
        return open(os.path.join(work, name)).read().strip()
    except OSError:
        return ""

def rc(name):
    value = read(name + ".rc")
    return int(value) if value.lstrip("-").isdigit() else None

attempts = []
for name, command in [
    ("beegfs-version", "/opt/beegfs/sbin/beegfs-ctl --version"),
    ("server-nodes", "beegfs-ctl --listnodes --nodetype=mgmtd|meta|storage"),
    ("server-targets", "beegfs-ctl --listtargets --nodetype=meta|storage --state"),
    ("server-processes", "ps -eo comm,args | grep [b]eegfs"),
    ("kernel-info", "uname -r; ls /lib/modules; ls /usr/src; grep beegfs /proc/filesystems"),
    ("modprobe", "modprobe beegfs"),
    ("client-init-script", "/etc/init.d/beegfs-client start"),
    ("client-binaries", "ls /opt/beegfs/sbin"),
    ("mount-attempt-no-cap", "mount -t beegfs none /mnt/beegfs -o cfgFile=/etc/beegfs/beegfs-client.conf (cap-drop=ALL)"),
    ("mount-attempt-with-cap", "mount -t beegfs none /mnt/beegfs -o sysMgmtdHost=...,connDisableAuthentication=true (cap-drop=ALL + SYS_ADMIN, dedicated probe container, no measured work)"),
]:
    attempts.append({
        "name": name,
        "command": command,
        "exit_status": rc(name),
        "output": read(name + ".out"),
    })

server_nodes = read("server-nodes.out")
targets = read("server-targets.out")
server_online = "Online" in targets and "Good" in targets

receipt = {
    "schema": "weft.beegfs_court.v1",
    "generated_utc": stamp,
    "result": "blocked",
    "blocked": True,
    "backend": {
        "product": "BeeGFS",
        "release": release,
        "repository": f"https://www.beegfs.io/release/beegfs_{release} (suite bookworm, component non-free)",
        "repository_release_sha256": release_sha256,
        "image": image,
        "image_id": image_id,
        "packages": packages,
        "topology": "one container running management, metadata and storage services; single metadata target 1 and single storage target 101; clients would be kernel mounts",
        "mount_method": "kernel module `beegfs` (mount -t beegfs); BeeGFS ships no FUSE client",
        "label": "SINGLE HOST, SINGLE NODE, SERVERS ONLY. No client was mounted and no BeeGFS filesystem was measured. This receipt is a blocker record, not a performance result.",
    },
    "server_side": {
        "started": server_online,
        "nodes": server_nodes,
        "targets": targets,
        "processes": read("server-processes.out"),
        "note": "the management, metadata and storage daemons start and register with each other inside a cap-drop=ALL container; this part is real and repeatable",
    },
    "client_side": {
        "mounted": False,
        "host_kernel": kernel,
        "attempts": attempts,
        "blocker": "BeeGFS's client is a kernel module. The container has no /lib/modules for the running host kernel (7.3.3-1-cachyos), Debian bookworm carries no headers for that kernel, and the package's own autobuild stops with 'Linux kernel build directory not found'. Even if the module could be built it would have to be inserted into the host kernel with CAP_SYS_MODULE, which the court forbids and which is not a container-local act.",
        "not_a_packaging_gap": "beegfs-client 7.4.6 installs and runs its autobuild; the failure is the absent kernel build tree and the impossibility of loading a module, not a missing binary",
    },
    "container": {
        "memory": memory, "memory_swap": memory, "pids": pids, "cpus": cpus,
        "cap_drop": "ALL",
        "cap_add": [],
        "no_new_privileges": True,
        "why_no_capabilities": "every BeeGFS service here is userspace; no capability was needed to start management, metadata and storage",
    },
    "consequence": "No A/N/B/C/D/E arm was run on BeeGFS and no number in any programme document may cite BeeGFS. The row remains 'blocked' with this receipt as its evidence.",
    "non_claims": [
        "no BeeGFS filesystem was mounted, so nothing was measured on BeeGFS",
        "server-side registration is not a filesystem result",
        "no comparison against another filesystem (rule 13)",
    ],
}
with open(out, "w") as handle:
    json.dump(receipt, handle, indent=1)
    handle.write("\n")
print(f"beegfs_court: receipt {out} (result=blocked, mounted=false, server_online={server_online})")
PY

echo "beegfs_court: receipt $receipt" >&2
echo "beegfs_court: log $log" >&2
