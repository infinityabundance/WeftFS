# Augmentation results — same backend, A/B/C/D/E (R3)

**Status:** first executed same-backend augmentation result. Methodology is
frozen in [`CONTRACTS-augmentation.md`](CONTRACTS-augmentation.md). This
document reports only what the receipts below measured; every unexecuted
backend is listed as `NOT MEASURED` with its blocker.

## Receipts

| Backend | Receipt | Configuration |
|---|---|---|
| Local POSIX | [`bench/receipts/augmentation-local-20261109T134132Z.json`](receipts/augmentation-local-20261109T134132Z.json) | full: embedding + 100 tensors, 424,439,296 logical bytes |
| MinIO (S3) | [`bench/receipts/augmentation-minio-20261109T134257Z.json`](receipts/augmentation-minio-20261109T134257Z.json) | full: same subset |
| Local POSIX (reduced) | [`bench/receipts/augmentation-local-20261109T134434Z.json`](receipts/augmentation-local-20261109T134434Z.json) | reduced: 16 evenly spaced non-embedding tensors, 1,849,344 logical bytes; no embedding |
| Local POSIX (retained repeat) | [`bench/receipts/augmentation-local-20261109T133923Z.json`](receipts/augmentation-local-20261109T133923Z.json) | same full subset; a host-I/O-contended repeat (D write 15.3 s with 0.3 s CPU; E 49.2 s). Retained, not used for the table. |
| MinIO (retained failure) | [`bench/receipts/augmentation-minio-20261109T131418Z.json`](receipts/augmentation-minio-20261109T131418Z.json) | C write failed `HTTP 500 ... no space left on device`; found the tmpfs delete-reclaim defect. |

Fixture: `.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors`,
988,097,824 bytes, SHA-256
`fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe`.
The court re-hashes the whole file and refuses to run on mismatch. That
whole-fixture gate is measured as `fixture_verify_ms` (412–438 ms) and is
**excluded from every write wall equally**; it is a court integrity check, not
an augmentation cost. The selected 101 tensors include
`model.embed_tokens.weight` (BF16 `[151936,896]`, 272,269,312 bytes) and 100
name-sorted, evenly spaced tensors.

Court: `weft/bench-court:1`
`sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559`,
kernel `7.3.3-1-cachyos`, memory 3g == swap, 4 CPUs, pids 1024, bounded `/tmp`,
`no-new-privileges`, `cap-drop=ALL`. MinIO:
`RELEASE.2025-04-22T22-12-26Z`, binary SHA-256
`53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`, in
`debian@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc`.
The MinIO container was restarted between configurations because its 1 GiB
tmpfs does not release deleted-but-open bytes (measured: bucket `du` 0 bytes,
`df` still 406 MiB). Write and restore for one configuration both complete
before its restart; the receipt records
`configuration.minio_restart_between_configurations: true`.

Exactness: **every configuration restored all 101 tensors and every SHA-256
matched the source**, on both backends. `mismatches: 0` in every A–E/N row of
the three published receipts.

## Local POSIX — all metrics

Cold restore is a fresh process after `posix_fadvise(DONTNEED)` on the store
files; warm is a second pass in the same process. A is plain per-tensor files;
B is WeftFS raw; C is WeftFS planned; D is B + Context Root/selector/provenance;
E is C + D. N is the supplementary native + application-level manifest scheme.

| Metric | A Native | B Pass-through | C Codec | D Protocol | E Combined | N (supplementary) |
|---|---:|---:|---:|---:|---:|---:|
| Durable write wall (ms) | 335.9 | 2,852.7 | 24,840.1 | 2,879.2 | 24,789.2 | 337.7 |
| Fixture gate (ms, excluded) | 413.9 | 413.8 | 414.4 | 419.6 | 415.2 | 414.4 |
| Logical bytes | 424,439,296 | 424,439,296 | 424,439,296 | 424,439,296 | 424,439,296 | 424,439,296 |
| Physical bytes written | 424,439,296 | 427,053,105 | 284,561,645 | 427,391,455 | 284,899,995 | 424,477,171 |
| Physical footprint | 424,439,296 | 426,630,999 | 284,139,539 | 427,146,560 | 284,655,442 | 424,477,171 |
| Cold restore wall (ms) | 348.2 | 543.6 | 1,425.8 | 552.1 | 1,418.2 | 345.8 |
| Warm restore wall (ms) | 191.7 | 284.0 | 1,301.4 | 284.4 | 1,277.8 | 190.1 |
| Cold physical bytes read | 424,439,296 | 424,782,640 | 282,220,893 | 425,177,636 | 282,616,733 | 424,477,171 |
| Write amplification | 1.0000 | 1.0062 | 0.6704 | 1.0070 | 0.6712 | 1.0001 |
| Read amplification (cold) | 1.0000 | 1.0008 | 0.6649 | 1.0017 | 0.6659 | 1.0001 |
| Write requests | 101 | 911 | 516 | 911 | 516 | 104 |
| Cold read requests | 101 | 1,108 | 713 | 1,327 | 932 | 104 |
| Logical restore throughput (MB/s) | 1,218.9 | 780.8 | 297.7 | 768.8 | 299.3 | 1,227.5 |
| Physical restore throughput (MB/s) | 1,218.9 | 781.5 | 197.9 | 770.1 | 199.3 | 1,227.6 |
| Restore p50 / p95 / p99 (µs) | 74 / 7,159 / 7,209 | 104 / 10,500 / 11,078 | 127 / 28,485 / 29,341 | 109 / 10,515 / 11,248 | 128 / 27,921 / 28,985 | 74 / 7,091 / 7,226 |
| Write peak RSS (KiB) | 270,584 | 271,692 | 1,187,552 | 271,424 | 1,187,136 | 270,380 |
| Restore peak RSS (KiB) | 269,184 | 273,032 | 714,360 | 273,628 | 714,828 | 269,548 |
| Write CPU user+sys (ms) | 100 | 270 | 23,210 | 280 | 23,180 | 100 |
| Restore CPU user+sys (ms) | 190 | 310 | 1,300 | 320 | 1,300 | 190 |
| Chosen representations | 101 files | 101 raw | 35 float-tensor, 50 zstd, 16 raw | 101 raw | 35 float-tensor, 50 zstd, 16 raw | 101 files + 3 metadata |

### Deltas against A (local)

| Configuration | Latency reduction | Logical throughput | Physical throughput | Storage reduction | Augmentation overhead |
|---|---:|---:|---:|---:|---:|
| B Pass-through | −56.1 % | −35.9 % | −35.9 % | −0.52 % | **+396.5 %** |
| C Codec | −309.5 % | −75.6 % | −83.8 % | **+33.1 %** | **+3,739.6 %** |
| D Protocol | −58.6 % | −36.9 % | −36.8 % | −0.64 % | **+401.6 %** |
| E Combined | −307.3 % | −75.4 % | −83.7 % | **+32.9 %** | **+3,731.1 %** |
| N (supplementary) | +0.7 % | +0.7 % | +0.7 % | −0.009 % | −0.08 % |

Positive latency/throughput/storage means X is better; positive overhead means
X costs more end-to-end. Negative values are losses and are not clipped.

## MinIO (S3) — all metrics

Same subset, same court, a real MinIO server in a sibling container. Cold means
a fresh process; there is no remote cache control.

| Metric | A Native | B Pass-through | C Codec | D Protocol | E Combined | N (supplementary) |
|---|---:|---:|---:|---:|---:|---:|
| Durable write wall (ms) | 825.7 | 1,603.5 | 23,708.1 | 1,653.1 | 23,605.9 | 838.2 |
| Fixture gate (ms, excluded) | 413.9 | 414.2 | 419.4 | 414.3 | 415.6 | 414.5 |
| Logical bytes | 424,439,296 | 424,439,296 | 424,439,296 | 424,439,296 | 424,439,296 | 424,439,296 |
| Physical bytes written | 424,439,296 | 427,529,404 | 285,037,944 | 427,867,754 | 285,376,294 | 424,439,296 |
| Physical footprint | 424,439,296 | 426,631,264 | 284,139,804 | 427,146,826 | 284,655,708 | 424,477,171 |
| Cold restore wall (ms) | 293.2 | 760.8 | 1,522.5 | 808.5 | 1,566.1 | 299.1 |
| Warm restore wall (ms) | 295.1 | 564.8 | 1,437.3 | 555.0 | 1,443.2 | 297.7 |
| Cold physical bytes read | 424,439,296 | 424,782,640 | 282,220,893 | 425,177,636 | 282,616,733 | 424,477,171 |
| Write amplification | 1.0000 | 1.0073 | 0.6716 | 1.0081 | 0.6724 | 1.0000 |
| Read amplification (cold) | 1.0000 | 1.0008 | 0.6649 | 1.0017 | 0.6659 | 1.0001 |
| Write requests | 101 | 1,120 | 725 | 1,120 | 725 | 104 |
| Cold read requests | 101 | 1,108 | 713 | 1,327 | 932 | 104 |
| Logical restore throughput (MB/s) | 1,447.7 | 557.9 | 278.8 | 524.9 | 271.0 | 1,419.3 |
| Physical restore throughput (MB/s) | 1,447.7 | 558.4 | 185.4 | 525.9 | 180.5 | 1,419.4 |
| Restore p50 / p95 / p99 (µs) | 229 / 5,679 / 6,245 | 899 / 10,430 / 12,891 | 1,121 / 27,755 / 29,584 | 921 / 10,364 / 15,402 | 1,015 / 27,783 / 31,832 | 246 / 5,739 / 6,189 |
| Write peak RSS (KiB) | 272,404 | 272,692 | 1,189,004 | 272,692 | 1,188,600 | 272,500 |
| Restore peak RSS (KiB) | 274,408 | 274,380 | 716,300 | 274,744 | 717,152 | 272,356 |
| Write CPU user+sys (ms) | 250 | 450 | 22,840 | 460 | 22,800 | 250 |
| Restore CPU user+sys (ms) | 220 | 310 | 1,330 | 310 | 1,310 | 220 |
| Chosen representations | 101 files | 101 raw | 35 float-tensor, 50 zstd, 16 raw | 101 raw | 35 float-tensor, 50 zstd, 16 raw | 101 files + 3 metadata |

### Deltas against A (MinIO)

| Configuration | Latency reduction | Logical throughput | Physical throughput | Storage reduction | Augmentation overhead |
|---|---:|---:|---:|---:|---:|
| B Pass-through | −159.5 % | −61.5 % | −61.4 % | −0.52 % | **+111.3 %** |
| C Codec | −419.3 % | −80.7 % | −87.2 % | **+33.1 %** | **+2,155.1 %** |
| D Protocol | −175.8 % | −63.7 % | −63.7 % | −0.64 % | **+120.0 %** |
| E Combined | −434.2 % | −81.3 % | −87.5 % | **+32.9 %** | **+2,149.8 %** |
| N (supplementary) | −2.0 % | −2.0 % | −2.0 % | −0.009 % | +1.65 % |

## What won, what lost

**The codec wins on bytes and loses on time.** C and E cut the physical
footprint by **33.1 % / 32.9 %** and cold physical reads by **33.5 %** on both
backends. They pay for it: local durable write wall **24.8 s vs 0.34 s**
(73.9x), cold restore **1.43 s vs 0.35 s** (4.1x), write peak RSS **1.19 GiB
vs 270 MiB** (4.4x), and restore peak RSS **714 MiB vs 269 MiB** (2.7x). MinIO
shows the same pattern (28.7x write wall, 5.2x restore). The planned write is
dominated by ZSTD-19 and mode-9 encoding: **23.2 s of CPU** versus 0.27 s for
raw. That is the measured codec contribution, not a scheduling artefact.

**The store integration loses on time and is nearly neutral on bytes.** B's
footprint is **+0.52 %** over A and its cold restore is **+56 %** slower on
local; its write wall is **8.5x** A's because it initialises a store, publishes
101 commits, writes 101 manifest frames, and writes an advisory index segment
per commit. On MinIO the write penalty is smaller (**1.9x**) because A's 101
PUTs already pay network round trips. B is not a free abstraction.

**The protocol is cheap on top of B.** D adds one snapshot, one Context Root,
one selector resolution, and one provenance record. Against B it costs
**+0.9 % write wall** and **+1.6 % cold restore** locally; on MinIO **+3.1 %**
and **+6.3 %**. Its footprint is **+0.12 %** over B (427,146,560 vs
426,630,999 bytes). The protocol does not dominate at 101 objects, but it is
not free and the cold path verifies one provenance record and every selected
object.

**E composes without extra loss.** E's footprint is within 0.18 % of C
(284,655,442 vs 284,139,539) and its restore is 0.5 % faster than C in this run.
The protocol layer on top of the codec costs about the same as it does on top
of raw.

**The functionally matched native baseline is essentially A.** N's write wall
is **+0.5 %** (local) and **+1.5 %** (MinIO) versus A; its footprint is
**+0.009 %** (three metadata objects); its end-to-end delta is within 0.1 %
(local) and 1.7 % (MinIO). An application can add per-tensor SHA-256, an
atomically published manifest, an explicit key-set context, and a provenance
record without the store machinery — because it has no commit log, no
per-commit index, no selector engine, no snapshots, no capability model, and no
version graph. This is the honest protocol comparison: D's extra cost buys
features N does not implement, not just the same three JSON files.

**Small-object cost is severe.** The reduced 1.8 MB run
(local, `augmentation-local-20261109T134434Z.json`) measured B at **+1,032 %**
end-to-end overhead and D at **+1,246 %**, with A at 19.2 ms write / 4.0 ms
restore. At that size, store initialisation and per-commit index/manifest
metadata dominate. C still cut storage by 27.4 % and was 3.0x slower to restore
than A. The 424 MB headline is kinder to B/D than a small checkpoint namespace
would be.

**Percentiles are dominated by the embedding.** p50 is 74–1,121 µs across all
configurations, while p95/p99 are 5.7–31.8 ms because the 272 MB embedding is
one sample. The per-tensor percentiles are published as observations, not as a
codec microbenchmark.

**Warm versus cold is not fully controllable.** Local cold used
`fadvise(DONTNEED)` on every store file; warm is a second pass in the same
process. The published local A cold read is 348 ms and its warm read is 192 ms;
B/D show 544/284 and 552/284 ms; C/E show 1,426/1,301 and 1,418/1,278 ms
because decode, not page faults, dominates. An earlier clean local run
(`augmentation-local-20261109T131243Z.json`, gate-included timer) measured A
563 ms / B 777 ms for the same store; the difference is page-cache state, not
the configuration. A repeat (`augmentation-local-20261109T133923Z.json`) was
contaminated by host I/O wait (D write 15.3 s with 0.3 s CPU; E 49.2 s) and is
retained but not used in the table. MinIO has no cold-cache control; its A
restore (293 ms) is faster than local A because the server and loopback caches
are warm. None of these numbers is a cold-media claim.

**Physical accounting is client-visible, not media.** For WeftFS the write
figure is `Store::backend_stats().bytes_written`, which includes the capability
probe, LOCK rewrites, FORMAT/superblock, commit records, tensor manifest frames,
chunk/frame blobs, and advisory index segments. The footprint is a recursive
file walk (local) or an object listing over every key prefix plus root control
records (S3). For A/N it is harness-counted file or PUT/GET bytes. No backend
exposed a device-level counter; the receipts say so.

**MinIO restart caveat.** MinIO 2025-04-22 on a 1 GiB tmpfs reported `du` 0 for
a deleted bucket while `df` still showed 406 MiB used, so deleted-but-open data
is charged until the process restarts. The court restarts the container between
configurations, after both write and restore for that configuration. This does
not change any within-configuration measurement; it is recorded in every MinIO
receipt and in the contract. The failing receipt
`bench/receipts/augmentation-minio-20261109T131418Z.json` is retained: C's
write died with `HTTP 500 ... no space left on device` after A and B had been
reported clean, which is the measurement that forced the restart methodology.

## NOT MEASURED (no result is claimed)

| Backend | Why no result |
|---|---|
| 3FS | No deployed 3FS cluster. The only executed 3FS evidence is an ABI-compatible `dlopen` shim (`docs/THREEFS.md`); no cluster, client, or RDMA transport was available. |
| CephFS | No CephFS mount or MDS. The Ceph evidence in this repository is Ceph RGW, an object-store S3 server, not a POSIX filesystem. |
| Lustre | No Lustre server, client, or mount in the court. |
| BeeGFS | No BeeGFS server, client, or mount in the court. |
| DAOS | No DAOS server, client library, or pool in the court. |
| JuiceFS | No JuiceFS binary, metadata engine, or object store configured. |
| IBM Storage Scale | Not installed; no licensed client or cluster. |
| WEKA | Not installed; no licensed client or cluster. |
| VAST | Not installed; no appliance or client. |
| AIStore | Not installed; no cluster. |
| Live AWS S3 | No credentials or bucket. The executed S3 row is MinIO on loopback. |
| Live Google Cloud Storage | No credentials or bucket. Existing GCS evidence is `fake-gcs-server` plus a service-account unit court. |
| Live Azure Blob | No credentials or account. Existing Azure evidence is Azurite. |
| Physical power loss / host cache loss | The court is process-level. Crash and power-loss courts are separate; no media claim is made here. |
| Multi-node / fleet | One host, one server process, one writer. No distributed publication claim. |

## Reproduce

```sh
# Local, full (embedding + 100 tensors, 424,439,296 logical bytes)
scripts/augmentation_court.sh --local

# MinIO, full (same subset; one real MinIO container, restarted between configs)
scripts/augmentation_court.sh --minio

# Aggregate-court reduced configuration (16 non-embedding tensors)
WEFT_AUGMENTATION_REDUCED=1 scripts/augmentation_court.sh --local
```

Each command builds `examples/augmentation_bench` in `weft/bench-court:1`,
runs configurations A/B/C/D/E plus the supplementary N, and writes
`bench/receipts/augmentation-<backend>-<stamp>.json` plus a raw log. A missing
fixture, Docker daemon, pinned MinIO binary, or court network prints
`augmentation_court: skip <reason>` and exits 78.

## Non-claims

No cloud, multi-node, fleet, media, power-loss, or production claim. The source
is one public model checkpoint subset, not a training trace. The numbers are one
host with warm source reads and one published run per cell; the repeats are
retained above and the cold-restore variance is stated. No comparison is made
against 3FS, DAOS, Lustre, BeeGFS, CephFS, JuiceFS, IBM Storage Scale, WEKA,
VAST, or AIStore because none executed.
