# JuiceFS — the augmentation on a deployed volume, with cross-client behaviour (T035)

**Status:** executed on `staging`. This is a same-backend A/N/B/C/D/E court on a
real JuiceFS volume: the official client binary, a SQLite metadata engine, a
local file object store, and **two independent JuiceFS client processes** each
mounting the same volume through JuiceFS's own FUSE client.

Receipts (all committed):

| Receipt | What it is |
|---|---|
| [`augmentation-juicefs-20261115T005509Z.json`](receipts/augmentation-juicefs-20261115T005509Z.json) | **cited.** The frozen augmentation contract court on the JuiceFS mount: six arms, 101 tensors, 424,439,296 logical bytes, seven interleaved randomised repetitions (`--order-seed 20261114`). |
| [`juicefs-20261115T005509Z.json`](receipts/juicefs-20261115T005509Z.json) | **cited.** The backend receipt: binary and tarball digests, image digest, volume settings from `juicefs status`, both clients' mount options and mountinfo, capabilities, and the whole cross-client section. |
| [`juicefs-20261115T005509Z.status.json`](receipts/juicefs-20261115T005509Z.status.json) | **cited.** Verbatim `juicefs status` output (volume settings and live sessions). |
| [`juicefs-20261115T005509Z.stats.txt`](receipts/juicefs-20261115T005509Z.stats.txt) | **cited.** Verbatim `juicefs stats` sample from the surviving client. |
| [`juicefs-20261115T005509Z.ack.log`](receipts/juicefs-20261115T005509Z.ack.log), [`.readback.tsv`](receipts/juicefs-20261115T005509Z.readback.tsv), [`.readback-recovered.tsv`](receipts/juicefs-20261115T005509Z.readback-recovered.tsv), [`.writer.txt`](receipts/juicefs-20261115T005509Z.writer.txt) | **cited.** Raw n-client-failure evidence: the acknowledged chunk list and both read-backs. |
| [`juicefs-20261115T005509Z.log`](receipts/juicefs-20261115T005509Z.log) | **cited.** Raw court log. |
| [`augmentation-juicefs-20261115T001844Z.json`](receipts/augmentation-juicefs-20261115T001844Z.json), [`juicefs-20261115T001844Z.json`](receipts/juicefs-20261115T001844Z.json) | **retained, superseded.** The first full run of this method, before the cross-client writer was bounded to 64 MiB with a per-chunk timeout. The measurement is valid and the method is the same; the change to the script is why the court was re-run. Registered in `docs/superseded-receipts.json`. |

## Deployment

- **JuiceFS** `1.4.2+2026-10-22.46af7af`, from the project's official
  `juicefs-1.4.2-linux-amd64.tar.gz` release asset. The download is verified
  against the release's own `checksums.txt` before use
  (`79648a0a…e975247`), and the extracted binary's digest is recorded too.
- **Metadata engine:** SQLite (`sqlite3:///jfs/meta.db`).
- **Object store:** the local file store (`--storage file`, bucket `/jfs/obj`),
  i.e. JuiceFS's own key/value object layout on a host directory — not the mount
  directory, and not a loopback S3 gateway pretending to be a filesystem.
- **Volume format:** `--block-size 16384` (KiB; JuiceFS 1.4.2 takes KiB and this
  is its documented maximum — its default is 4096 KiB), `--trash-days 0`,
  `--compress none`. `juicefs status` reports the settings that actually took
  effect and they are copied verbatim into the receipt.
- **Two clients, one volume.** Two `debian:bookworm-slim` containers from the
  same pinned image, each with its own mount point and its own cache directory:
  `--no-usage-report --no-syslog --cache-dir … --cache-size 1 --open-cache 0
  --attr-cache 1.0s --entry-cache 1.0s`. A 1 MiB client data cache makes the
  cold-restore column a backend read, not a client-cache hit; `--open-cache 0`
  keeps JuiceFS's documented close-to-open default.
- **Write-back is off** (the JuiceFS default), so a completed write means the
  data was uploaded to the object store and the metadata committed, after an
  `fsync` of the file (A–E) or of the atomic metadata replacement (N).
- **Limits and capabilities:** memory 3 GiB == swap, 4 CPUs, 1024 pids,
  `no-new-privileges`, bounded `/tmp`, `cap-drop=ALL` plus `SYS_ADMIN` and
  `/dev/fuse` — the minimum a FUSE client needs. Two capabilities the client
  would like are absent and the client says so in its own log: `CAP_SYS_RESOURCE`
  (for `PR_SET_IO_FLUSHER`) and `setpriority`. Both are recorded in the receipt
  as observed lost capabilities rather than silently dropped.

**Label:** one host, one volume, two client processes, one writer at a time.
Not a fleet, not multi-node, not clustered performance, not a cloud result. The
object store and the SQLite database are host directories, so no media or
power-loss claim is made.

## The six-arm table

Median of seven interleaved randomised repetitions. `min`/`p50`/`p95` are over
those seven repetitions. `footprint` is the size of the tree on the JuiceFS
mount, exactly as the contract defines for a POSIX backend; see the losses below
for what it does not include.

<!-- BEGIN GENERATED:juicefs-table (scripts/perf_numbers.py) -->
| row | write min ms | write p50 ms | write p95 ms | cold p50 ms | footprint B | written B | write req | cold req | write RSS KiB | write CPU ms | restore CPU ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `A` | 258.7 | 526.0 | 2,639.8 | 362.0 | 424,439,296 | 424,439,296 | 101 | 101 | 270,212 | 130 | 260 |
| `N` | 373.7 | 580.3 | 2,960.8 | 367.9 | 424,477,171 | 424,477,171 | 104 | 104 | 270,104 | 130 | 270 |
| `B` | 2,437.3 | 5,151.2 | 6,444.6 | 595.0 | 424,608,542 | 424,621,048 | 611 | 1,008 | 537,772 | 340 | 340 |
| `C` | 1,425.7 | 3,139.6 | 4,189.1 | 1,476.1 | 282,106,049 | 282,118,555 | 216 | 613 | 690,580 | 1,160 | 1,350 |
| `D` | 2,318.1 | 5,489.7 | 7,847.0 | 586.1 | 424,659,735 | 424,629,900 | 611 | 1,027 | 538,000 | 340 | 340 |
| `E` | 1,302.0 | 3,240.1 | 3,807.7 | 1,472.2 | 282,157,242 | 282,127,407 | 216 | 632 | 690,464 | 1,180 | 1,350 |
<!-- END GENERATED:juicefs-table -->

<!-- BEGIN GENERATED:juicefs-headline (scripts/perf_numbers.py) -->
**The codec wins on bytes on JuiceFS and loses on time.** C stores **33.5 %** fewer physical bytes than A
(**282,106,049 B** against **424,439,296 B**) and its durable write p50 is
**3,139.6 ms** against A's **526.0 ms** (least-contended
**1,425.7 ms** against **258.7 ms**); cold restore is
**1,476.1 ms** against **362.0 ms**. The contract's
end-to-end overhead for C is **415.1 %** and its latency change is
**-298.9 %** (negative is a loss). Seven interleaved randomised
repetitions; `min` is the least-contended repetition and is used for phase attribution
only, never in place of the median.
<!-- END GENERATED:juicefs-headline -->

<!-- BEGIN GENERATED:juicefs-losses (scripts/perf_numbers.py) -->
* **Writes are slower than native.** C's p50 is **3,139.6 ms** against A's
  **526.0 ms**; its least-contended repetition is **1,425.7 ms**
  against **258.7 ms**, so the gap is real and not only contention.
* **The host was contended and the spread is large.** C's slowest repetition is
  **4,189.1 ms** against a median of **3,139.6 ms**, and A's is
  **2,639.8 ms** against **526.0 ms**. Both the median and the
  least-contended repetition are published; neither replaces the other.
* **Restore is slower with the codec:** **1,476.1 ms** against
  **362.0 ms**. Fewer bytes do not pay for decode on this client.
* **The footprint columns are the mount tree, not the object store.** JuiceFS's own
  16 MiB block granularity, its SQLite metadata and its object-store layout are not in
  them; the arm-vs-arm comparison is like-for-like, the absolute storage number is not a
  JuiceFS storage claim.
* **No FUSE-on-FUSE:** every arm writes through the JuiceFS mount and no WeftFS FUSE is
  mounted, so no row here prices the filesystem's own frontend.
* No comparison of any kind is made against another filesystem (rule 13).
<!-- END GENERATED:juicefs-losses -->

## Cross-client behaviour

JuiceFS is a client-side filesystem: every client caches attributes, entries and
(optionally) data, and the consistency model it documents is close-to-open. The
court therefore measures four things with two independent client processes on
one volume, after the measured matrix has finished so it cannot influence it.

<!-- BEGIN GENERATED:juicefs-cross (scripts/perf_numbers.py) -->
* **Close-to-open, A→B:** **143 ms**; **B→A:**
  **142 ms**. Both are ceilings: the peer polls a fresh open+read
  at 10 ms resolution and the recorded number is when the bytes were observed, so the
  real visibility latency is at most that.
* **In-place rewrite:** the peer observed the replaced content after
  **24 ms**. JuiceFS guarantees close-to-open, not
  read-after-write, and its attribute/entry caches are 1 s by default, so this is a
  measurement of the documented model rather than a surprise.
* **n-client failure:** the writer and client A were killed together; **9**
  chunks had been acknowledged, the other client read **9** of them
  back intact (**0** bad), and after A remounted it read
  **9** of them back intact. The file on the peer was
  **9,437,184 B** — larger than the acknowledged prefix, because the write
  that was in flight is unacknowledged by construction.
<!-- END GENERATED:juicefs-cross -->

**What the n-client failure test proves and does not prove.** The writer's own
acknowledgement — `dd … conv=fsync` returning — is the durability contract; the
test kills the client and its writer after several such acknowledgements and
requires every acknowledged chunk to read back intact on the *other* client and,
after a restart, on the killed client itself. It does **not** prove that a
partial, unacknowledged write is discarded or that any specific byte beyond the
last acknowledgement does or does not exist: the file length is published, and
those bytes are unacknowledged by definition. It is a process fault, not media
loss, and it is a two-client test on one host, not an N-node failure claim.

## Reproduce

```sh
# full published court: two JuiceFS clients on one SQLite + file-store volume,
# six arms, seven randomised repetitions, then the cross-client section
scripts/juicefs_court.sh

# aggregate-court configuration (16 non-embedding tensors, seven repetitions)
scripts/juicefs_court.sh --reduced

# cross-client section alone (no six-arm matrix), for a quick check
scripts/juicefs_court.sh --cross-client-only
```

The script prints `juicefs_court: skip <reason>` and exits 78 when Docker,
`/dev/fuse`, the pinned release, the fixture or enough host scratch is
unavailable. A skip is never a pass.

## Not measured / non-claims

- **Single host.** One volume, two client processes, one writer at a time. No
  fleet, multi-node, clustered, cloud, WAN or production-workload claim.
- **SQLite metadata.** Not the Redis, PostgreSQL, MariaDB or TiKV engines, and
  not the hosted JuiceFS service. A SQLite metadata engine is a single-machine
  choice by design; the two-client test is a consistency test, not a scalability
  result.
- **Local file object store.** Not S3/GCS/Azure object storage over a network,
  so nothing here is a network, egress or provider-latency claim.
- **The footprint is the mount tree.** It is what the contract counts for a
  POSIX backend and what the application sees. The JuiceFS object store was not
  instrumented per arm, so object-store amplification (16 MiB block granularity,
  no compression) and SQLite metadata growth are **not** in the footprint
  columns. That is a real gap: the arm-vs-arm comparison is like-for-like, but
  the absolute storage number is a mount-tree number.
- **No media, power-loss or host-cache-loss claim.** The failure test kills
  client processes, not a machine or a disk.
- **Host contention.** The host was shared with another project's courts during
  these runs; the write distributions are visibly bimodal and both the median
  and the least-contended repetition are published. The medians are not
  silently replaced by minima.
- **No comparison against another filesystem** (rule 13). This court measures
  JuiceFS with and without the augmentation.
- The N arm is a harness composition (plain files plus a canonical JSON
  manifest, provenance record and explicit key-set context), not a shipped CLI
  verb.
