# Augmentation benchmark contract (R3, version 2 at R20)

**Status:** frozen before the first executed augmentation measurement, and
revised once (R20, §1 "Publication row variants") when the shipped publication
path changed from one commit per tensor to one group commit with one
durability barrier. This file exists so that the A/B/C/D/E result cannot be
retrofitted to a preferred conclusion. Every number in
`bench/RESULTS-augmentation.md` must be traceable to a receipt produced under
these rules. If a later run changes a rule, the rule change is a new contract
version and both receipts stay published (R17/R19 receipts remain valid at
the revisions they name, and the `-percommit` rows reproduce their form).

Scope label: single host, OOM-protected Docker court, one real S3-compatible
server (MinIO) or the local POSIX filesystem. This is not a cloud, multi-node,
fleet, device-cache, media, or power-loss claim.

## 1. The five configurations

**What is compared.** The comparator is the **same backend with and without
this augmentation**. A is that backend's own durable path for the same bytes;
B/C/D/E add, respectively, the store protocol, the codec, the context/provenance
protocol, and both. The WeftFS reference FUSE filesystem is **not** a comparator:
no row in this contract measures it, and no result may cite it as the baseline
for a write-path claim. A backend's own filesystem, object store or checkpoint
writer is the native side.

The distinction is **where the work happens**, not which feature is advertised.
All five configurations write and restore the **identical logical tensor set**
from one verified source container, and every restored tensor is compared with
the source bytes by SHA-256.

### A — Native

The backend's own optimised path, with no WeftFS code in the data or control
path.

- **Local POSIX:** one plain file per tensor under
  `<work>/A/native/<index>-<name>.bin`. Each file is created with
  `create_new`, written once, and `sync_all`ed; the directory is `sync_all`ed
  after the last tensor. No header, no manifest, no checksum sidecar, no
  compression.
- **S3/MinIO:** one `PUT` per tensor object under the store's ordinary object
  namespace, using the backend client's create-once form (`If-None-Match: *`).
  No WeftFS manifest, commit, index, or probe. One `GET` per tensor on restore.

A provides no stored integrity metadata, no atomic multi-object publication, no
selection metadata, and no provenance. It is therefore labelled
**operational, not functionally matched** for the protocol dimension. The court
verifies its restored bytes against the source; that verification is external
to A.

### B — Pass-through

WeftFS store, raw representation, and nothing else.

- One store file per tensor in `/tensors`.
- Representation forced to `TensorStrategy::Raw` through the shipped Fast write
  policy with `FastCodec::Raw`. The Fast path builds only canonical row-major
  chunks and never runs a planned encoder.
- No Context Root, no selector, no provenance record, no project attribute.
- The store's own required metadata (FORMAT, LOCK, superblock, commit records,
  tensor manifest frames, chunk blobs, advisory index segments) is counted in
  full. Nothing is excluded because it is "infrastructure".

B isolates the integration overhead of using the store at all.

### C — Codec

WeftFS store with the shipped planned representation, and no protocol use.

- Same one-file-per-tensor layout as B.
- `TensorWriteSpec.strategy_hint = None`; `WritePolicy::Planned`.
- Candidate set: canonical raw chunks, one ZSTD frame at the frozen store level
  3, one ZSTD level-19 frame when the tensor is at least 1 MiB and the
  estimated encode time is within the shipped 20 s CPU budget, and the
  `FLOAT_TENSOR` mode-9 candidate for `bf16`/`fp16`/`f32` when the planner can
  meet its size gate.
- **Deliberately disabled:** semantic-delta search (it decodes live candidate
  tensors per write and is measured in its own court), tiled candidates
  (no automatic tile derivation), and causal deltas (no causal bases exist in
  this fixture).
- No Context Root, selector, provenance, or project attribute.

C isolates the codec contribution. It is the only configuration whose chosen
representations may differ per tensor.

### D — Protocol

B's representation plus the context/provenance surface.

- Same raw representation as B.
- Every tensor file carries the attribute `project = "augmentation"`.
- After the last tensor: create a snapshot, parse and persist a Context Root
  over `select attrs.project == "augmentation"`, and record one
  `weft.provenance.v1` receipt covering every output object.
- Restore resolves the Context Root, requires the resolved object set to equal
  the written object set exactly, and verifies the provenance receipt once
  against every output content root.

D isolates the protocol contribution at raw storage cost.

### E — Combined

C's planned representation plus D's context/provenance surface. Both the codec
and the protocol cost are counted.

### Supplementary — N (native + application-level scheme)

The comparison rules in §7 require a functionally matched native baseline where
possible. `N` is therefore run as a **supplementary row, not one of the five**:

- Raw per-tensor files (same bytes as A) on the same backend.
- A canonical JSON manifest containing every tensor name, byte length, and
  SHA-256, published atomically: local writes `manifest.json.tmp`, fsyncs it,
  renames it to `manifest.json`, and fsyncs the directory; S3 publishes one
  object with the backend's atomic PUT.
- A canonical JSON provenance record naming the operation, tool, fixed
  timestamp, and the sorted output SHA-256 set, published the same way.
- A canonical JSON context record naming the exact key set (not a selector
  engine: there is no query language, no ambiguity handling, and no
  snapshot-pinned version graph).
- Restore verifies every tensor against the stored manifest digest and checks
  the provenance output set.

N is labelled **`native-app-equivalent`**: it matches D on stored integrity,
atomic publication of the metadata, selection by an explicit key set, and
provenance capture. It does **not** match D on selector expressiveness, causal
history, capability policy, or exact reconstruction of arbitrary
representations. It is reported separately and never folded into the A–E table.

### Derived ablation rows (R17)

R17 adds **derived** rows used only to attribute cost. They never replace A–E
and are always labelled with the row they derive from:

- `C-noz19` — C with the level-19 ZSTD candidate removed;
- `C-nom9` — C with the mode-9 `FLOAT_TENSOR` candidate removed;
- `C-both` — C with both removed;
- `B-group` — B with all state ops published in one shipped group commit.

The controls are additive and default-off in the product; the default C row is
byte-identical to the contract row (same chosen strategies, same physical
bytes, same per-tensor manifests), which the receipt verifies against the
unmodified contract court.

### Publication row variants (R20, contract version 2)

R20 changes **how** the WeftFS rows publish, not what they write. The rule
change is recorded here because it changes the measured C row:

- The default rows (`A`–`E`) now publish all tensor files in **one group
  commit** with **one deferred durability barrier**, which is what the shipped
  checkpoint writer already did (`Store::group` + `begin_group`). The logical
  state, the chosen representations, every blob byte and every restored tensor
  are unchanged; the number of commit records and advisory index segments
  falls from 101 to 1 (plus the genesis and `/tensors` commits), and the
  footprint is 176,962 B smaller.
- The historical per-commit form remains runnable as `-percommit`
  (`B-percommit`, `C-percommit`), which is what R17/R19 measured. `-group` is a
  group commit with eager per-blob barriers, and `-pack<k|m>` adds the packed
  blob plane at that member threshold.
- Suffixes compose with `-`: `noz19-percommit` is the R17 row with the
  historical publication. Publication never changes the chosen candidate set,
  so `C` and `C-legacy` still differ only in the planner
  (`two_stage_storage_unchanged`).
- Nothing is excluded from the accounting: commit records, superblock slots,
  index segments, blob publications by kind, backend writes and both barrier
  kinds are reported per row in `attribution_rep.ledger.publication_ledger`.

## 2. Fixture and subset

Source: `.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors`,
988,097,824 bytes, SHA-256
`fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe`.

The court re-hashes the **whole file** inside the container and refuses to run
on any mismatch. No tensor weights are committed to the repository.

The full container is 988,065,536 logical tensor bytes across 290 tensors. The
published run uses a **bounded, deterministic subset** to fit the 3 GiB court
and the MinIO one-gigabyte tmpfs:

- always include `model.embed_tokens.weight` (BF16, `[151936, 896]`,
  272,269,312 bytes);
- sort every other tensor by name (ascending), take every
  `floor(289 / 100) = 2`-th tensor, and keep the first 100;
- 101 tensors in total, **424,439,296 logical bytes**.

The exact selected names, shapes, byte lengths, and per-tensor SHA-256 values
are recorded in the receipt. A reduced configuration used by the aggregate
verification court selects 16 evenly spaced non-embedding tensors (no
embedding) and is labelled `reduced` in its receipt. The reduced row is never
used for a published performance claim.

## 3. Mandatory metrics

Every configuration records all of the following, or the string
`NOT MEASURED` with a reason. A missing cell is never zero and never silently
omitted.

- durable write wall time (ms), including source read, store initialisation,
  every encode, index build, commit, protocol step, and the final durability
  barrier. The one-time whole-fixture SHA-256 gate is a court integrity check,
  not an augmentation cost: it is measured and reported as
  `fixture_verify_ms`, and is excluded from every configuration equally;
- logical bytes written;
- **physical bytes written** (see §4 for the accounting source);
- restore cold wall time, from fresh process start through the last verified
  tensor; restore warm wall time measured separately;
- physical bytes read during the same interval;
- per-tensor restore latency: p50, p95, p99 (microseconds), plus the sample
  count;
- peak RSS per phase (`VmHWM` from `/proc/self/status` of the phase process,
  KiB);
- CPU user+system per phase (ms, from `/proc/self/stat` deltas);
- write object/request count and restore object/request count;
- write amplification = physical bytes written / logical bytes;
- read amplification = physical bytes read / logical bytes;
- logical-byte throughput (logical bytes / second) and physical-byte
  throughput (physical bytes / second), **reported separately**;
- physical footprint after write (bytes) and its accounting source;
- exactness: tensors verified, mismatches, restored logical bytes;
- chosen representation histogram (B/C/D/E) or file count (A/N).

## 4. Physical accounting sources

Two independent numbers are recorded and both are published:

1. **Backend write/read counters.** For every WeftFS configuration the harness
   reads `Store::backend_stats()` before and after the phase. These counters
   include capability probes, LOCK rewrites, FORMAT/superblock writes, commit
   records, tensor manifest frames, chunk/frame blobs, and advisory index
   segments. They count bytes actually handed to the backend, including bytes
   later overwritten (for example the two-slot superblock and the LOCK object).
   For configuration A/N the harness counts its own file writes or object
   `PUT`/`GET` bodies because no WeftFS counter exists.
2. **Physical footprint.** Local: recursive walk of every regular file under
   the configuration directory (the `du` source). S3: object listing over every
   store key prefix plus the root control records, summing object lengths.

The receipt names which source produced `physical_bytes_written` and which
produced `physical_footprint_bytes`. The write amplification uses the counter
figure; the storage-reduction delta uses the footprint figure. They answer
different questions and are never conflated.

No backend in this court exposes a device-level byte counter, so "physical"
means "bytes the client sent to, or that the store retains on, the backend",
not media writes. That limitation is stated in every receipt.

## 5. Deltas

For every configuration X against A:

```
latency_reduction_pct(X) =
    100 * (A.cold_restore_wall_ms - X.cold_restore_wall_ms)
        / A.cold_restore_wall_ms

throughput_improvement_pct(X) =
    100 * (X.logical_restore_bytes_per_s - A.logical_restore_bytes_per_s)
        / A.logical_restore_bytes_per_s

storage_reduction_pct(X) =
    100 * (A.physical_footprint_bytes - X.physical_footprint_bytes)
        / A.physical_footprint_bytes

augmentation_overhead_pct(X) =
    100 * ((X.write_wall_ms + X.cold_restore_wall_ms)
           - (A.write_wall_ms + A.cold_restore_wall_ms))
        / (A.write_wall_ms + A.cold_restore_wall_ms)
```

Positive latency reduction, throughput improvement, and storage reduction mean
X is better. Positive augmentation overhead means X costs more end-to-end than
A. A negative value is published as a loss, never clipped.

Physical-byte throughput is reported alongside logical-byte throughput; a
configuration that reads or writes fewer logical bytes per second can still be
ahead on physical bytes, and the prose must say which one is being discussed.

## 6. Measurement rules

1. **Identical logical data and outputs.** Every configuration reads the same
   subset from the same verified container and restores the same 101 tensors.
   The court compares every restored tensor to the source SHA-256. A single
   mismatch fails the run; there is no tolerance.
2. **End-to-end accounting.** Encode, decode, index build, sync, protocol
   resolution, provenance verification, and the harness's SHA-256 check are all
   inside the measured phase. There is no "steady state" subtraction.
3. **Fresh process per phase.** Write and restore run in separate child
   processes. Peak RSS is that process's `VmHWM`. No process is reused between
   configurations.
4. **Cold and warm are separate.** Local cold restore calls
   `posix_fadvise(..., POSIX_FADV_DONTNEED)` on every file the previous write
   phase created before opening. The store's in-process caches are disabled by
   default (no decoded-object cache, no single-flight). Warm restore is a second
   pass in the same process, so it measures OS page cache and backend cache, not
   a store cache that does not exist. A container cannot fully control host page
   cache; this is labelled. S3 has no cold-cache control: cold means a fresh
   process and warm means a repeated GET; MinIO and kernel caches remain warm
   and are labelled.
5. **Determinism.** Fixed `--seed` (deterministic ObjectId seeds), fixed
   timestamp `1_700_000_000_000_000_000`, fixed chunk size (1 MiB), fixed ZSTD
   levels (3 always; 19 only under the shipped gate), and a fixed subset rule.
   Two runs of the same configuration on the same backend must publish the same
   object names and content roots; the receipt records the head.
6. **Baselines are not weakened.** A is the plain per-tensor path that a native
   deployment would actually use. N is the explicitly specified application
   scheme above. No baseline is given a handicap to make WeftFS look better,
   and no WeftFS configuration is given a cache, a warm-up, or a concurrency
   advantage that A does not have.
7. **Microbenchmarks are separate.** Per-tensor latency percentiles come from
   the same end-to-end run and are labelled as per-tensor observations. They are
   never presented as standalone codec microbenchmarks.
8. **Unexecuted cells.** A backend that did not run is listed in the receipt's
   `not_measured` array with its blocker. It is never assigned a zero, a
   placeholder, or an extrapolated number.
9. **Losses are mandatory.** If B is slower than A, if C's manifests and
   indexes make it larger than A, if D's metadata dominates at small object
   counts, or if E loses on either axis, the receipt and the results document
   say so in the headline table and the prose.
10. **Derived-row repetitions.** A derived row (≤ 1 above) is measured with at
    least **seven** independently timed repetitions, interleaved with the other
    rows and rotated so no row always occupies the same position. Median, p95
    and the raw samples are published. If the host is shared, the
    least-contended repetition is used for phase attribution and is labelled as
    such; the medians are never silently replaced by minima.

## 7. Comparison rules

- A provides less functionality than D/E. It is labelled **operational, not
  functionally matched** for selection, atomicity, integrity metadata, and
  provenance. Its latency and footprint are still valid baselines for the
  storage cost of the same bytes.
- N is the **functionally matched native baseline** for the protocol dimension.
  The exact scheme is specified in §1. D-vs-N is the honest protocol delta on
  the same native data plan; it does not replace the required D row.
- C-vs-B is the codec delta and D-vs-B is the protocol delta. E-vs-C and E-vs-D
  show whether the two augmentations compose without extra loss.
- Every comparison states which throughput is being discussed (logical or
  physical) and whether it is cold or warm.

## 8. Courts and reporting

- Local: `scripts/augmentation_court.sh --local`, in `weft/bench-court:1`
  under `--memory=3g --memory-swap=3g --pids-limit=1024 --cpus=4`, bounded
  `/tmp`, `no-new-privileges`, `cap-drop=ALL`.
- MinIO: `scripts/augmentation_court.sh --minio`, with the pinned MinIO release
  binary in a sibling `debian:bookworm-slim` container under the same hard
  limits, exactly as `scripts/s3_court.sh` does. The MinIO data directory is a
  tmpfs; the receipt records the binary SHA-256 and the container image digest.
  **The container is restarted between configurations.** Measured fact: MinIO
  2025-04-22 on a 1 GiB tmpfs keeps deleted-but-open data charged to the
  filesystem (`du` of the bucket reports 0 bytes while `df` still reports the
  pre-delete usage), so one container cannot hold six clean 0.4 GB
  configurations. Each configuration's write and restore complete before its
  restart; no measurement spans a restart, and the restart is recorded in the
  receipt (`configuration.minio_restart_between_configurations`).
- Receipts: `weft.augmentation.v1`, one per backend, plus a raw log with every
  child-process line. The receipt embeds the court image ID, kernel, limits,
  source revisions, fixture hash, subset definition, selected names, and every
  metric above.
- The aggregate verification court adds a **reduced** local configuration
  (16 evenly spaced non-embedding tensors, all five A–E rows, two restore
  passes) and records that configuration in its own receipt. A court that
  cannot run prints `augmentation_court: skip <reason>` and exits 78; the
  aggregate records a skip, never a pass.

## 9. What this contract does not establish

- No network, cloud, multi-node, or fleet claim. The S3 endpoint is MinIO on
  loopback.
- No media, power-loss, or host-cache-loss claim. Durability is `fsync` or a
  provider acknowledgement.
- No production workload. The source is a public model checkpoint subset, not a
  training trace.
- No claim about 3FS, CephFS, Lustre, BeeGFS, DAOS, JuiceFS, IBM Storage Scale,
  WEKA, VAST, AIStore, or live AWS/GCS/Azure. Those rows are `NOT MEASURED`
  with their blocker in `bench/RESULTS-augmentation.md`.
- No claim that A–E bracket every augmentation. They bracket the four
  contributions this program actually ships: integration, codec, protocol, and
  both.
