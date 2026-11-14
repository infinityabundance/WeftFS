# N04 — the bounded writer pipeline on a remote object backend

**Status:** executed on `staging`. Same-backend A/N/B/C/D/E court on a real
MinIO server, seven interleaved repetitions, plus a provider-fault adversarial
court. The pipeline is a **store API** (`Store::prepare_tensor_group_pipelined`),
not a shipped CLI verb; every pipelined row below is labelled as an ablation,
exactly as R22 required.

Receipts:

| Receipt | What it is |
|---|---|
| [`augmentation-minio-20261113T190000Z.json`](receipts/augmentation-minio-20261113T190000Z.json) | **cited.** The frozen augmentation contract court (`bench/CONTRACTS-augmentation.md`, version 2) with six contract arms and four pipelined rows, seven interleaved repetitions, rotated order. `generated_utc` `20261114T000237Z`, WeftFS `37a03c0dbb62`. |
| [`n04-object-pipeline-20261113T190000Z.json`](receipts/n04-object-pipeline-20261113T190000Z.json) | **cited.** The N04 receipt: the court above, the adverse scenarios, the fencing suite result and the declared durability contract. |
| [`n04-s3-fencing-20261113T190000Z.log`](receipts/n04-s3-fencing-20261113T190000Z.log) | **cited.** `cargo test --test s3_fencing` against the same server (stale conditional write refused). |
| [`n04-proxy-reset-abort-20261113T190000Z.jsonl`](receipts/n04-proxy-reset-abort-20261113T190000Z.jsonl) | **cited.** Provider-abort fault log: which PUT was dropped and never forwarded. |
| [`n04-proxy-swallow-lost-ack-20261113T190000Z.jsonl`](receipts/n04-proxy-swallow-lost-ack-20261113T190000Z.jsonl) | **cited.** Lost-acknowledgement fault log: which PUT was applied but its acknowledgement withheld. |
| [`n04-object-pipeline-20261113T190000Z.log`](receipts/n04-object-pipeline-20261113T190000Z.log) | **cited.** Raw court log. |

Backend: MinIO `RELEASE.2025-04-22T22-12-26Z`, binary SHA-256
`53e2a2cb16c5366ea6fbbc479c19ddb4c6a0948273e752f740fb1fbf27bb817c`, in
`debian@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc`,
data directory a 1 GiB tmpfs. Court `weft/bench-court:1`
`sha256:4c6c41fb2b549f37ba846b5e51424d773cef9194bc3a6b3632ab208f66501559`,
kernel `7.3.3-1-cachyos`, memory 3 GiB == swap, 4 CPUs, 1024 pids, bounded
`/tmp`, `no-new-privileges`, `cap-drop=ALL`. The MinIO container is restarted
between cells (its tmpfs does not release deleted-but-open bytes; see the R3
contract §8).

Fixture: `.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors`, whole-file
SHA-256 re-hashed in the court. <!-- BEGIN GENERATED:n04-fixture (scripts/perf_numbers.py) --> 101 tensors and 424,439,296 logical bytes, identical in every arm and every repetition. <!-- END GENERATED:n04-fixture --> Every arm
restored every tensor; the receipt's `mismatches` is `0` in all ten rows.

## What this court changes

R21's pipeline overlapped encoding with publication on a local POSIX backend,
where the publisher drains published files and hands the `fsync`s to a
writeback thread. A remote object backend has no published file to barrier: its
`PUT` returns only after the provider accepted the bytes. N04 therefore:

1. adds `StorageBackend::durability_is_provider_ack` (S3, GCS and Azure true;
   local, immutable-object and POSIX false; a split store follows its blob
   plane, a packed store its inner backend);
2. reports it in `TensorPipelineStats` as `durability_model`,
   `early_barrier_applicable`, `provider_acknowledged_bytes` and
   `publish_sync_kicks`, and on a provider-ack backend **issues no barrier**;
3. keeps the ordering rule unchanged: every immutable blob is published before
   the caller's commit record, so a crash leaves unreferenced blobs and never a
   dangling commit or catalog entry.

## The six-arm table (plus the pipelined rows)

Median of seven interleaved repetitions unless the column says otherwise.
`min`/`p50`/`p95` are over the seven repetitions, not over tensors. A is the
backend's own per-object `PUT`; N is A plus an application-maintained canonical
manifest, provenance record and explicit key-set context; B/C/D/E are the WeftFS
store with the frozen representation and protocol definitions. `-pipe4` adds
four encode workers with one publisher (three CPU tokens).

<!-- BEGIN GENERATED:n04-table (scripts/perf_numbers.py) -->
| row | write min ms | write p50 ms | write p95 ms | cold p50 ms | footprint B | written B | write req | cold req | write RSS KiB | write CPU ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `A` | 836.8 | 846.6 | 853.8 | 307.7 | 424,439,296 | 424,439,296 | 101 | 101 | 272,812 | 260 |
| `N` | 850.2 | 861.9 | 869.1 | 302.2 | 424,477,171 | 424,439,296 | 104 | 104 | 272,708 | 260 |
| `B` | 1,462.4 | 1,480.7 | 1,499.7 | 795.0 | 424,608,804 | 424,635,048 | 620 | 1,008 | 539,592 | 460 |
| `B-pipe4` | 1,379.9 | 1,391.8 | 1,452.2 | 792.9 | 424,608,804 | 424,635,048 | 620 | 1,008 | 723,848 | 520 |
| `C` | 1,257.8 | 1,275.6 | 1,294.1 | 1,513.0 | 282,106,311 | 282,132,555 | 225 | 613 | 691,960 | 1,220 |
| `C-pipe4` | 826.9 | 858.7 | 887.7 | 1,520.4 | 282,106,311 | 282,132,555 | 225 | 613 | 938,376 | 1,350 |
| `D` | 1,489.1 | 1,513.6 | 1,537.5 | 792.6 | 424,659,998 | 424,643,900 | 620 | 1,027 | 539,424 | 470 |
| `D-pipe4` | 1,381.3 | 1,412.7 | 1,423.9 | 793.3 | 424,660,004 | 424,643,900 | 620 | 1,027 | 740,744 | 510 |
| `E` | 1,280.8 | 1,295.5 | 1,307.1 | 1,527.2 | 282,157,505 | 282,141,407 | 225 | 632 | 692,108 | 1,240 |
| `E-pipe4` | 843.4 | 865.4 | 897.6 | 1,526.1 | 282,157,511 | 282,141,407 | 225 | 632 | 935,120 | 1,360 |
<!-- END GENERATED:n04-table -->

### What the pipeline changed, per arm

<!-- BEGIN GENERATED:n04-pipeline (scripts/perf_numbers.py) -->
| arm | sequential p50 ms | pipelined p50 ms | change |
|---|---:|---:|---:|
| B pass-through | 1,480.7 | 1,391.8 | **6.0 %** |
| C codec | 1,275.6 | 858.7 | **32.7 %** |
| D protocol | 1,513.6 | 1,412.7 | **6.7 %** |
| E combined | 1,295.5 | 865.4 | **33.2 %** |
Positive means the pipeline shortened the wall.
<!-- END GENERATED:n04-pipeline -->

## What won, what lost

<!-- BEGIN GENERATED:n04-headline (scripts/perf_numbers.py) -->
**On a remote object backend the pipeline is the difference between the codec
losing badly and the codec being nearly free.** C's durable write wall falls
from 1,275.6 ms to 858.7 ms (826.9 ms least-contended),
which is **1.01x** the native per-object path's 846.6 ms while
storing **33.5 %** fewer physical bytes. The storage win is the
codec's; the write-time win is the pipeline overlapping encode with the many
small PUTs a compressed representation produces.

Cold restore is unchanged by the pipeline and remains the codec's cost:
1,520.4 ms against A's 307.7 ms (A reads one object per tensor; C reads
fewer bytes but decodes them). All 101 tensors restored byte-exact in every
arm, with 0 mismatches.
<!-- END GENERATED:n04-headline -->

<!-- BEGIN GENERATED:n04-losses (scripts/perf_numbers.py) -->
* **The pipeline costs memory and CPU.** C-pipe4's write peak RSS is
  938,376 KiB against 691,960 KiB for sequential C, and its write CPU is
  1,350 ms against 1,220 ms. Four encoders hold up to four tensors'
  working sets at once by design; the byte reservations bound it, they do not
  remove it.
* **The pipeline does not help B or D as much as it helps C/E.** They publish raw
  blobs, so publication, not encoding, dominates; the overlap has less to hide.
* **The pipelined rows are not shipped.** No CLI verb reaches
  `Store::prepare_tensor_group_pipelined`; every `-pipe4` row is an ablation.
* **E-pipe4 keeps 33.5 %** of the storage reduction, so the protocol layer
  adds no measurable footprint on top of the codec.
* Restore is single-threaded per tensor here; the R13 pipelined restore is a
  separate API and was not used in this court.
<!-- END GENERATED:n04-losses -->

## Adverse court (provider faults)

<!-- BEGIN GENERATED:n04-adverse (scripts/perf_numbers.py) -->
The baseline write issues 43 PUTs; the fault target is PUT
30, which the proxy log shows is a `manifests/` blob.

| scenario | provider behaviour | writer result | reopen | retry |
|---|---|---|---|---|
| `abort` | connection dropped before the write is applied | fails closed (`Io`, `Unexpected EOF`) | previous head, `verify --full` clean, no visible entry | reaches the reference head and manifest ids |
| `lost-ack` | write applied, acknowledgement withheld | fails closed (`Io`, `Unexpected EOF`) | previous head, `verify --full` clean | reaches the reference head; already-published blobs are accepted, not rewritten |
| `stale-reader` | none | a second handle commits | the open reader stays pinned and sees no new entry | a fresh reader sees the commit |
| stale conditional write | client presents an old `If-Match` | `s3_fencing` suite: refused | head unchanged | — |
<!-- END GENERATED:n04-adverse -->

The fault proxy (`bench/scripts/s3_fault_proxy.py`) forwards the client's exact
bytes, so the client's SigV4 signature and every header reach MinIO verbatim. It
identifies the fault target by counting framed HTTP requests and choosing a
`manifests/` or `objects/` `PUT` from the middle of the write — an advisory index
segment is published best-effort by design, so faulting *it* would not have
proved anything (the first attempt did exactly that and is retained in the
earlier untracked run log).

## Durability and visibility contract (identical on every arm)

- **A/N:** a completed write is every object `PUT` acknowledged by the provider.
- **B/C/D/E:** every blob `PUT` acknowledged by the provider, then one commit
  record accepted that names them; a reader sees all-or-nothing.
- **No arm `fsync`s anything.** The pipeline reports `durability_model =
  provider-ack` on S3 and the receipt records `fsync_claimed: false`. The MinIO
  data directory is a tmpfs, so this is not a media or power-loss claim.
- Visibility: single-node read-after-write and list-after-write for published
  keys; conditional `PUT` (`If-None-Match`/`If-Match`) is required and probed at
  writable open.

## Reproduce

```sh
# full published court: six contract arms + four pipelined rows, 7 reps,
# adverse phase, fencing suite
scripts/object_pipeline_court.sh

# aggregate-court configuration: reduced 16-tensor subset, 7 reps
scripts/object_pipeline_court.sh --reduced
```

One command, from a clean checkout with the fixture present:

```sh
scripts/docker-court.sh bench "cd /work && bash scripts/object_pipeline_court.sh"
```

Both print `object_pipeline_court: skip <reason>` and exit 78 when Docker, the
pinned MinIO binary or the fixture is unavailable; `scripts/verify-all.sh`
records a skip, never a pass.

## Not measured / non-claims

- One host, one loopback MinIO process, one writer. No cloud, multi-node,
  fleet, media, power-loss or production claim.
- Live AWS S3, GCS and Azure were not run; only their capability declarations
  changed, and each already had its own emulator court.
- The pipeline is not reachable from a shipped CLI verb. `C-pipe4` is a
  store-API composition; the shipped default remains the sequential writer.
- Restore is unaffected by this change. The pipelined rows' cold-restore
  columns differ only within the repetition band, and the receipt publishes all
  seven repetitions.
- The fault proxy is test infrastructure: a dropped connection and a withheld
  acknowledgement are two real provider failure modes, not a model of every
  partial-failure mode a cloud provider can exhibit.
