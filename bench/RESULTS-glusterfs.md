# GlusterFS — the augmentation on a deployed single-brick volume (T036)

**Status:** executed on `staging`. A same-backend A/N/B/C/D/E court on a real
GlusterFS volume: one server container running `glusterd` and one brick, and
**two independent FUSE client processes** mounting the same volume with
`mount -t glusterfs`.

Receipts (all committed):

| Receipt | What it is |
|---|---|
| [`augmentation-glusterfs-20261115T010035Z.json`](receipts/augmentation-glusterfs-20261115T010035Z.json) | **cited.** The frozen augmentation contract court on the GlusterFS mount: six arms, 101 tensors, 424,439,296 logical bytes, seven interleaved randomised repetitions. |
| [`glusterfs-20261115T010035Z.json`](receipts/glusterfs-20261115T010035Z.json) | **cited.** The backend receipt: package versions, image id, capabilities and why each is needed, volume options, both clients' mountinfo, full `gluster volume info/status/peer status`, and the cross-client section. |
| [`glusterfs-20261115T010035Z.ack.log`](receipts/glusterfs-20261115T010035Z.ack.log), [`.readback.tsv`](receipts/glusterfs-20261115T010035Z.readback.tsv), [`.readback-recovered.tsv`](receipts/glusterfs-20261115T010035Z.readback-recovered.tsv), [`.writer.txt`](receipts/glusterfs-20261115T010035Z.writer.txt) | **cited.** Raw n-client-failure evidence. |
| [`glusterfs-20261115T010035Z.log`](receipts/glusterfs-20261115T010035Z.log) | **cited.** Raw court log. |

## Deployment

- **GlusterFS `10.3-5`** from the Debian bookworm `glusterfs-server` /
  `glusterfs-client` packages. The package versions and the built image id are
  in the receipt.
- **The image is purpose-built**, and that is stated rather than hidden:
  `apt-get` cannot run inside a `--cap-drop=ALL` container (the package manager
  cannot drop privileges), so `docker/gluster.Dockerfile` bakes the packages in
  and the court builds `weft/gluster-court:1` once. Server and clients are the
  same build, so they are the same GlusterFS version by construction.
- **Topology:** one `glusterd`, one brick (`/data/brick1` on a host bind mount,
  mode 0777 so the root brick process can create its own directories), one
  single-brick distributed volume.
- **Volume options:** `nfs.disable on` is the only option changed from the
  package default. Debian does not ship the Gluster NFS xlator (the upstream
  built-in NFS server was removed), and without this option `gluster volume
  start` fails its commit with `DL open failed … nfs/server.so`. The failure and
  the fix are in the receipt.
- **Capabilities, and why each is the measured minimum:**
  - the **server** gets `SYS_ADMIN` (the brick stores metadata in
    `trusted.glusterfs.*` xattrs; without it, `gluster volume create` fails with
    `Setting extended attributes failed, reason: Operation not permitted`) and
    `DAC_OVERRIDE` (Gluster's posix translator creates `.glusterfs` with mode
    0600 and relies on root's DAC override to descend into it; without it the
    brick aborts on `.glusterfs/indices/xattrop: Failed to create [Permission
    denied]`). Both were found by running it, not guessed, and both are recorded
    as the reason the court is not capability-free.
  - the **clients** get `SYS_ADMIN` and `/dev/fuse`, the minimum for a FUSE
    mount.
- **Durability, matched across arms:** `fsync` on a GlusterFS file is forwarded
  to the brick, which `fsync`s the backing host file. Every arm fsyncs each file
  and the directory (A, B, C, D, E) or writes and fsyncs the atomic metadata
  replacement (N).

**Label:** one host, one brick, one volume, two client processes, one writer at
a time. Not a fleet, not multi-node, not replicated, not a cloud result, and not
a media or power-loss claim. The six-arm matrix runs on **client A alone**;
client B is started afterwards for the cross-client section only. `requests` in
the table are application-level file operations on the mount, not Gluster RPC
counts, which this court does not instrument.

## The six-arm table

Median of seven interleaved randomised repetitions; `min`/`p50`/`p95` over those
repetitions.

<!-- BEGIN GENERATED:glusterfs-table (scripts/perf_numbers.py) -->
| row | write min ms | write p50 ms | write p95 ms | cold p50 ms | footprint B | written B | write req | cold req | write RSS KiB | write CPU ms | restore CPU ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `A` | 459.6 | 513.4 | 7,095.3 | 307.4 | 424,439,296 | 424,439,296 | 101 | 101 | 270,056 | 120 | 250 |
| `N` | 457.0 | 494.7 | 7,820.2 | 306.4 | 424,477,171 | 424,477,171 | 104 | 104 | 270,144 | 120 | 250 |
| `B` | 1,408.3 | 2,243.0 | 17,980.5 | 455.9 | 424,608,542 | 424,621,048 | 611 | 1,008 | 537,924 | 320 | 340 |
| `C` | 1,020.7 | 8,945.3 | 9,548.2 | 1,396.6 | 282,106,049 | 282,118,555 | 216 | 613 | 690,560 | 1,130 | 1,310 |
| `D` | 1,221.9 | 2,063.2 | 17,910.9 | 466.8 | 424,659,735 | 424,629,900 | 611 | 1,027 | 537,732 | 320 | 330 |
| `E` | 1,313.8 | 1,341.2 | 9,946.1 | 1,400.9 | 282,157,242 | 282,127,407 | 216 | 632 | 690,564 | 1,140 | 1,310 |
<!-- END GENERATED:glusterfs-table -->

<!-- BEGIN GENERATED:glusterfs-headline (scripts/perf_numbers.py) -->
**The codec wins on bytes on GlusterFS and loses on time.** C stores
**33.5 %** fewer physical bytes than A (**282,106,049 B**
against **424,439,296 B**) and its durable write p50 is
**8,945.3 ms** against A's **513.4 ms**
(least-contended **1,020.7 ms** against **459.6 ms**); cold
restore is **1,396.6 ms** against **307.4 ms**.
End-to-end overhead for C is **1,127.0 %**; the contract's latency
change is **-319.9 %** (negative is a loss). Seven interleaved
randomised repetitions.
<!-- END GENERATED:glusterfs-headline -->

<!-- BEGIN GENERATED:glusterfs-losses (scripts/perf_numbers.py) -->
* **Writes are much slower than native.** C's p50 is **8,945.3 ms** against
  A's **513.4 ms**; its least-contended repetition is
  **1,020.7 ms** against **459.6 ms**. The single brick's
  fsync path, not the codec, dominates this backend.
* **The write distribution is bimodal on this host.** C's slowest repetition is
  **9,548.2 ms** against a median of **8,945.3 ms**; A's
  slowest is **7,095.3 ms** against **513.4 ms**. The
  host was shared with another project's courts. Medians and least-contended values are
  both published and neither is substituted for the other.
* **Restore is slower with the codec:** **1,396.6 ms** against
  **307.4 ms**.
* **The footprint columns are the mount tree.** Brick filesystem allocation and Gluster's
  `trusted.*` metadata are not separately instrumented.
* **No FUSE-on-FUSE:** no WeftFS FUSE is mounted in any arm.
* No comparison of any kind is made against another filesystem (rule 13).
<!-- END GENERATED:glusterfs-losses -->

## Crash and acknowledgement

1. **Store-level process-fault proxy**, once per WeftFS arm on the first
   repetition: killed after the objects, after the commit record and after the
   superblock write, then reopened and verified. Old-whole or new-whole, never
   partial. The native arms have no store to crash.

<!-- BEGIN GENERATED:glusterfs-crash (scripts/perf_numbers.py) -->
| arm | old | new | partial |
|---|---:|---:|---:|
| `B` | 2 | 1 | 0 |
| `C` | 2 | 1 | 0 |
| `D` | 2 | 1 | 0 |
| `E` | 2 | 1 | 0 |
<!-- END GENERATED:glusterfs-crash -->

2. **File-level kill after acknowledgement** in the cross-client section below.

Per-tensor restore percentiles and the raw sample lists are in the receipt.

## Cross-client behaviour

<!-- BEGIN GENERATED:glusterfs-cross (scripts/perf_numbers.py) -->
* **Close-to-open, A→B:** **136 ms**; **B→A:**
  **135 ms**. Both are ceilings from a 10 ms polling loop.
* **In-place rewrite:** the peer observed the replaced content after
  **963 ms**. Close-to-open is the documented model;
  attribute caching sits behind the client's md-cache/stat-prefetch timeouts.
* **n-client failure:** the writer and its FUSE client were killed together;
  **8** acknowledged chunks, **8** read back
  intact on the other client (**0** bad), and
  **8** read back intact after the killed client remounted.
  The peer's file was **9,437,184 B**, i.e. one chunk longer than the
  acknowledged prefix: that chunk was in flight when the client died and is unacknowledged.
<!-- END GENERATED:glusterfs-cross -->

**What the n-client failure test proves and does not prove.** Same contract as
the JuiceFS court and the same step order. Every chunk acknowledged by
`dd … conv=fsync` must read back intact on the other client and, after a
restart, on the killed client. Bytes beyond the last acknowledgement are
unacknowledged and are reported, not counted as a loss. It is a process fault on
a two-client, one-brick deployment — not a brick failure, not a node failure,
not media loss.

## Reproduce

```sh
# full published court: one brick, two GlusterFS FUSE clients, six arms,
# seven randomised repetitions, then the cross-client section
scripts/glusterfs_court.sh

# aggregate-court configuration (16 non-embedding tensors, seven repetitions)
scripts/glusterfs_court.sh --reduced
```

The script prints `glusterfs_court: skip <reason>` and exits 78 when Docker,
`/dev/fuse`, the image build, the fixture or enough host scratch is
unavailable. A skip is never a pass.

## Not measured / non-claims

- **Single brick.** No replica, disperse, distributed-replicate or
  geo-replication volume was measured, so nothing here says anything about
  Gluster's redundancy or healing.
- **No NFS/SMB access.** The NFS xlator is absent from the Debian packages and
  is disabled; only the native GlusterFS FUSE client is used.
- **Single host**, one volume, two client processes, one writer at a time. Not a
  fleet, cluster or multi-node result.
- **No media, power-loss or host-cache-loss claim.** The failure test kills a
  client process.
- **Footprint** is the tree on the mount (the contract's POSIX definition). Brick
  filesystem block allocation, Gluster's `trusted.*` xattr metadata and any
  internal sparse-file behaviour are not separately instrumented.
- **Host contention.** The host was shared with another project's courts; the
  write distributions are visibly bimodal (the C row's seven samples span more
  than an order of magnitude) and both the median and the least-contended
  repetition are published. Medians are never replaced by minima.
- **No comparison against another filesystem** (rule 13).
- The N arm is a harness composition, not a shipped CLI verb.
