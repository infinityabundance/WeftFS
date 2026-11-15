# BeeGFS — attempted, blocked at the client kernel module (T036)

**Status:** the attempt ran; the deliverable is the blocker, not a substitute.
BeeGFS 7.4.6 installs and its **servers run** inside a `--cap-drop=ALL`
container. The **client cannot be mounted in this court**, and the receipt
records the exact command, exit status and error text of every attempt.

Receipt (committed): [`beegfs-20261115T010922Z.json`](receipts/beegfs-20261115T010922Z.json)
(`schema: weft.beegfs_court.v1`, `result: blocked`, `mounted: false`) and the raw
log [`beegfs-20261115T010922Z.log`](receipts/beegfs-20261115T010922Z.log).

## What did run

- **Packages:** BeeGFS `7.4.6` from the vendor's own Debian-12 repository
  (`https://www.beegfs.io/release/beegfs_7.4.6`, suite `bookworm`, component
  `non-free`), baked into `weft/beegfs-court:1` at build time because `apt-get`
  cannot run inside the court's `cap-drop=ALL` containers. The repository's
  `Release` file digest, the installed package versions and the image id are in
  the receipt. The repository is used with `trusted=yes` because the vendor does
  not publish the signing key at a stable URL; that is recorded as a caveat
  rather than presented as a verified chain.
- **Services:** `beegfs-mgmtd`, `beegfs-meta` and `beegfs-storage` start as
  ordinary userspace processes with `connDisableAuthentication = true` (a
  single-node court has no shared secret). `beegfs-setup-mgmtd`,
  `beegfs-setup-meta` and `beegfs-setup-storage` create the node and the two
  targets, and `beegfs-ctl` reports metadata target `1` and storage target `101`
  both `Online` / `Good`. **No capability was needed** for any of this; the
  receipt's `cap_add` is empty.
- This part is real and repeatable, and it is all the court claims: the BeeGFS
  *services* run; no BeeGFS *filesystem* was mounted.

## The blocker, with exact text

The BeeGFS client is a kernel module (`beegfs.ko`); BeeGFS ships no FUSE client
(the receipt lists `/opt/beegfs/sbin` to show that). Each attempt fails for a
different, separable reason:

| Attempt | Result |
|---|---|
| `uname -r` / `ls /lib/modules` / `ls /usr/src` | host kernel `7.3.3-1-cachyos`; `/lib/modules` **does not exist** in the container; `/usr/src` is empty |
| `modprobe beegfs` | `modprobe: FATAL: Module beegfs not found in directory /lib/modules/7.3.3-1-cachyos` (exit 1) |
| `grep beegfs /proc/filesystems` | no match: the type is not registered |
| `mount -t beegfs …` with `cap-drop=ALL` | `mount: /mnt/beegfs: permission denied.` (exit 32) — **this does not isolate the cause**: the kernel refuses the mount system call before it looks at the filesystem type |
| `mount -t beegfs …` in a dedicated probe container with `cap-drop=ALL --cap-add SYS_ADMIN` and nothing else | `mount: /mnt/beegfs: unknown filesystem type 'beegfs'.` (exit 32) — with the mount capability present, the missing module is what remains |
| `/etc/init.d/beegfs-client start` | the package's own autobuild runs and stops: `Makefile:188: *** Linux kernel build directory not found. Please check if the kernel module development packages are installed for the current kernel version. (RHEL: kernel-devel; SLES: kernel-default-devel; Debian: linux-headers). Stop.` then `make: *** [AutoRebuild.mk:34: auto_rebuild] Error 2` |

**Why it is structural, not a packaging gap.** Building the module would need a
kernel build tree for the *running host kernel*, and Debian bookworm carries no
headers for `7.3.3-1-cachyos`. Even a successfully built module would have to be
inserted into the **host** kernel with `CAP_SYS_MODULE`, which the court's rules
forbid and which is not a container-local act: it would change the host for
every other workload, including the rest of this programme. There is no
userspace path — the same wall that Lustre (patched kernel) and any other
kernel-client filesystem hits in a container court.

## Consequence

- **No A/N/B/C/D/E arm was run on BeeGFS and no number in any programme document
  may cite BeeGFS.** The row stays `blocked` with this receipt as evidence.
- The court is therefore **not registered** in `scripts/verify-all.sh`. A court
  that always produces a blocker would inflate an aggregate "courts passed"
  count without measuring anything; the aggregate is for measurement.
- `scripts/beegfs_court.sh` exits 0 when it has produced the blocker receipt and
  78 when the environment itself is unavailable (no Docker, no network to reach
  the vendor repository). The receipt always says `result: blocked`.

## Not claimed

- No BeeGFS filesystem was mounted; nothing was measured on BeeGFS.
- Server-side registration is not a filesystem result and is not a performance
  number.
- No comparison against another filesystem (rule 13).
