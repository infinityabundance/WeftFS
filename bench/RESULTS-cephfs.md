# CephFS — the augmentation on a deployed distributed filesystem (T030/T038/T081)

**Status:** executed on `staging`. This is the first same-backend A/N/B/C/D/E
court on a **real, deployed** distributed filesystem rather than a loopback
object store or the court's own disk. Single node is the deployment; it is
labelled everywhere below and is not a fleet or clustered result.

Receipts:

| Receipt | What it is |
|---|---|
| [`augmentation-cephfs-20261114T040000Z.json`](receipts/augmentation-cephfs-20261114T040000Z.json) | **cited.** The frozen augmentation contract court on the CephFS mount: six arms, seven interleaved repetitions, `generated_utc` `20261114T015202Z`, WeftFS `4be2b44a3a6f`. |
| [`cephfs-20261114T040000Z.json`](receipts/cephfs-20261114T040000Z.json) | **cited.** The backend receipt: image digest, topology, mount method, pool settings, container limits. |
| [`cephfs-20261114T040000Z.cluster.txt`](receipts/cephfs-20261114T040000Z.cluster.txt) | **cited.** Verbatim `ceph -s` at court start. |
| [`cephfs-20261114T040000Z.fs.txt`](receipts/cephfs-20261114T040000Z.fs.txt) | **cited.** Verbatim `ceph fs status`. |
| [`cephfs-20261114T040000Z.log`](receipts/cephfs-20261114T040000Z.log) | **cited.** Raw court log. |
| [`augmentation-cephfs-20261114T031000Z.json`](receipts/augmentation-cephfs-20261114T031000Z.json) | **cited.** The aggregate court's reduced configuration (16 evenly spaced non-embedding tensors, seven repetitions), which is a genuine measurement but never a published performance claim. |

## Deployment

- **Ceph** `19.2.0 (16063ff2022298c9300e49a547a16ffda59baf13) squid (stable)`,
  from `quay.io/ceph/demo@sha256:522483cf07cfce6386b8e18a3edfa88a1b32c688dee231d0a6962b1513557723`
  (CentOS Stream 9), image id recorded in the receipt.
- **Topology:** one mon, one mgr, one bluestore OSD, one MDS in a single
  container; pools `cephfs_data` and `cephfs_metadata`; one `ceph-fuse` client.
- **Mount:** `ceph-fuse` in a second container from the same pinned image. The
  court kernel has no `ceph.ko` (the host's module directory is absent), so a
  kernel mount is impossible; `/dev/fuse` and `SYS_ADMIN` are the only
  capability the client gets. No WeftFS FUSE is mounted, so no arm is
  FUSE-on-FUSE.
- **Pool settings:** `size 1`/`min_size 1` on both pools (one OSD makes `size 2`
  permanently degraded and doubles the bytes the OSD must hold), and
  `mds_session_timeout 300` (a 4-CPU client running a large FUSE workload can
  pause between MDS messages; a blocklisted client wedges the mount). Both are
  in the backend receipt.
- **OSD data directory:** a host bind mount, not a tmpfs. The first attempt used
  a 2 GiB tmpfs and the cluster wedged when it filled; the retained log
  [`cephfs-20261114T003000Z.log`](receipts/cephfs-20261114T003000Z.log) shows the
  client hung in `fuse_chan_send`. Capacity is the backend's business; no hard
  limit changed.
- **Limits:** memory 3 GiB == swap, 4 CPUs, 1024 pids, `no-new-privileges`,
  bounded `/tmp`, `cap-drop=ALL` on both containers. The Ceph container needs
  CHOWN/DAC_OVERRIDE/FOWNER/SETGID/SETUID because its daemons run as uid/gid
  167; the client needs SYS_ADMIN plus `/dev/fuse`.

**Label:** single-node Ceph on one host, one client, one writer. Not
multi-node, not a fleet, not clustered performance, not a cloud result. The OSD
media is ordinary host storage, so no media or power-loss claim is made.

## Durability, matched across arms

Every arm gives the same guarantee: a completed write has `fsync`ed each file
and the directory (A), published its metadata atomically on the mount (N), or
published through the store's `LocalBackend` on the same mount (B/C/D/E). CephFS
`fsync` orders against the MDS/OSD journal. `requests` in the tables are
client-visible I/O operations, not RADOS RPCs — the receipt says so.

## The six-arm table

Median of seven interleaved repetitions unless the column says otherwise.
`min`/`p50`/`p95` are over the seven repetitions.

<!-- BEGIN GENERATED:cephfs-table (scripts/perf_numbers.py) -->
| row | write min ms | write p50 ms | write p95 ms | cold p50 ms | footprint B | written B | write req | cold req | write RSS KiB | write CPU ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `A` | 926.0 | 1,077.0 | 19,319.9 | 981.3 | 424,439,296 | 424,439,296 | 101 | 101 | 270,776 | 110 |
| `N` | 1,095.3 | 1,117.8 | 18,338.0 | 756.5 | 424,477,171 | 424,477,171 | 104 | 104 | 270,796 | 110 |
| `B` | 2,462.0 | 10,547.1 | 29,536.8 | 1,284.0 | 424,608,542 | 424,621,048 | 611 | 1,008 | 538,088 | 360 |
| `C` | 1,864.1 | 2,023.8 | 17,306.1 | 1,783.1 | 282,106,049 | 282,118,555 | 216 | 613 | 690,756 | 1,140 |
| `D` | 2,578.1 | 3,246.2 | 22,829.0 | 1,428.5 | 424,659,735 | 424,629,900 | 611 | 1,027 | 538,176 | 370 |
| `E` | 1,958.8 | 2,132.0 | 16,109.0 | 1,853.6 | 282,157,242 | 282,127,407 | 216 | 632 | 690,556 | 1,140 |
<!-- END GENERATED:cephfs-table -->

## What won, what lost

<!-- BEGIN GENERATED:cephfs-headline (scripts/perf_numbers.py) -->
**The codec wins on bytes on CephFS and loses on time.** C stores
**33.5 %** fewer physical bytes than A (101 tensors,
424,439,296 logical bytes, 0 mismatches) and its durable write wall is
2,023.8 ms p50 (1,864.1 ms least-contended) against A's
1,077.0 ms — **1.88x** — while its cold restore is
1,783.1 ms against A's 981.3 ms (1.82x); the
contract's latency change is -131.3 % (negative is a loss).
Seven interleaved repetitions; p50 is the median, not the minimum.

The native baseline N is essentially A, so the honest protocol comparison is
D-vs-N, and at this object count D's snapshot, Context Root, selector resolution
and provenance record cost wall time rather than bytes.
<!-- END GENERATED:cephfs-headline -->

<!-- BEGIN GENERATED:cephfs-losses (scripts/perf_numbers.py) -->
* **Writes are slower than native on every WeftFS arm.** C's p50 is
  2,023.8 ms against A's 1,077.0 ms; D is 3,246.2 ms and E
  2,132.0 ms.
* **The tail is long and the spread is real.** A's p95 is 19,319.9 ms,
  B's is 29,536.8 ms and C's is 17,306.1 ms over seven
  repetitions on one 4-CPU client; a single-OSD single-node cluster has no
  second OSD or MDS to absorb a pause. The full samples are in the receipt.
* **Restore is slower with the codec:** 1,783.1 ms against
  981.3 ms; fewer bytes do not offset decode on this backend.
* **The crash phases are process faults, not media loss**, and the first
* **The crash phases are process faults, not media loss**, and the first
  records it and the shipped court uses a host-directory OSD.
* No comparison of any kind is made against another filesystem (rule 13).
<!-- END GENERATED:cephfs-losses -->

## Crash point and reopen-verify

The harness runs three process-fault phases per WeftFS arm on the first
repetition: after the objects are published, after the commit record, and after
the superblock write. Each child is killed and the store is reopened and
verified; the reopened state must be the **whole old generation or the whole new
one**, never partial. The native arms have no WeftFS store to crash.

<!-- BEGIN GENERATED:cephfs-crash (scripts/perf_numbers.py) -->
| row | status | old | new | partial |
|---|---|---:|---:|---:|
| `B` | measured | 2 | 1 | 0 |
| `C` | measured | 2 | 1 | 0 |
| `D` | measured | 2 | 1 | 0 |
| `E` | measured | 2 | 1 | 0 |
<!-- END GENERATED:cephfs-crash -->

## Reproduce

```sh
# full published court: single-node Ceph + ceph-fuse client, six arms, 7 reps
scripts/cephfs_court.sh

# aggregate-court configuration: reduced subset, 7 reps
scripts/cephfs_court.sh --reduced
```

One command:

```sh
scripts/docker-court.sh rust "cd /work && bash scripts/cephfs_court.sh"
```

The script prints `cephfs_court: skip <reason>` and exits 78 when Docker,
`/dev/fuse`, the pinned image or the fixture is unavailable. A skip is never a
pass.

## Not measured / non-claims

- Single-node deployment: no multi-node, clustered, fleet, cloud, WAN or
  production-workload claim.
- No media, power-loss or host-cache-loss claim; the crash phases are process
  faults.
- No comparison against another distributed filesystem. Rule 13: this court
  measures CephFS with and without the augmentation, never WeftFS-the-filesystem
  against CephFS.
- JuiceFS and GlusterFS were not attempted once CephFS worked; they remain
  unimplemented rows, not losers.
- The N arm is a harness composition (plain files plus a canonical JSON
  manifest, provenance record and explicit key-set context), not a shipped CLI
  verb.
