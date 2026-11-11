# Identity without representation

### A falsifiable evaluation of a codec-augmented store for AI state

**Status:** research note, 2026-11-11 (R13). Every number below is traceable to a
committed receipt in `bench/receipts/`; a number without a receipt was deleted,
not softened. This paper claims no superiority and no production readiness.

---

## Abstract

We argue a specific, disagreeable thesis:

> **For AI state, the identity of an artifact should be independent of its
> physical representation, and representation choice should be optimised against
> a measured lifecycle cost surface. When this is done, exactness and selective
> reconstruction are affordable — but the surface is not globally favourable:
> on one write-dominated workload a codec-augmented store is 73.9× slower to
> write for 33.1 % fewer physical bytes, and the same design loses 28 % of
> bytes to a whole-file `zstd --patch-from` on a 19-byte checkpoint change.**

The first clause is a design constraint, not a performance result. The second
clause is a measurement result, and it is the part we expect a storage engineer
to attack. Our contribution is not a faster store. It is a set of identities,
refusals, and cost terms whose predictions can be checked — and falsified — from
committed artifacts without rerunning our courts. The three headline numbers
come from `bench/receipts/augmentation-local-20261109T134132Z.json` (73.9× write,
33.1 % bytes) and `bench/receipts/family-lifecycle-20261108T053655Z.json`
(42,211 B versus 30,265 B for the whole-file patch).

---

## 1. Claims

Each claim states the experiment, the receipt that supports it, the measured
numbers, and the condition under which it fails. C1–C7 are independent; a
reader may accept one and reject another.

### C1 — Identity is independent of physical representation

**Experiment.** Store the same 101-tensor logical set five ways on the same
backend (`A` native files, `B` raw store, `C` planned codec, `D` raw + context
and provenance, `E` planned + context and provenance), then restore every
tensor and compare SHA-256 with the source. A second experiment stores one
object as a copy-on-write v3 manifest and as a depth-8 v4 overlay, then
compacts it back to v3.

**Receipts.** `bench/receipts/augmentation-local-20261109T134132Z.json`;
`bench/receipts/overlay-20261106T103115Z.json`.

**Measured.** All five configurations restored all 101 tensors with
`mismatches: 0`; the physical footprints were 424,439,296 B (A), 426,630,999 B
(B), 284,139,539 B (C), 427,146,560 B (D), 284,655,442 B (E). C chose
35 float-tensor, 50 zstd, and 16 raw representations for the same logical
bytes. In the overlay court the COW run, the overlay run, and the
post-compaction run all produced content root `c00367f0…`, and the snapshot's
content digest was `6db387cc…` before compaction, after compaction, and after
GC (`snapshot.unchanged: true`).

**Fails if.** Two representations of the same logical object yield different
`ContentId`/`LogicalTensorRoot`, or a restore under one representation returns
different bytes than another. A single counterexample would refute the claim.

**Bound.** The identity is over logical bytes. A representation that loses
information is not a representation in this sense; the system has no lossy
decode path, and a hash mismatch fails closed.

### C2 — Provenance-bound identity makes unsafe reuse rejectable

**Experiment.** Give two KV pages identical token bytes and geometry but
different model/adapter/tokenizer/attention/layout provenance. Resolve each by
content hash and by descriptor; tamper with each derived root; swap the payload
under a descriptor; ask the store for pages under five different scope tokens.

**Receipt.**
`bench/receipts/kv-provenance-court-20261109T182750Z.json`; the wire contract is
`spec/WEFT-KV-PROVENANCE.md` (mode 19).

**Measured.** The two pages produced roots `ba5ec110…` and `ef7309ea…` over the
same 4,096-byte payload whose hash is
`a126fc3d…`. A cross-scope resolve returned `Unauthorized` and emitted **0
bytes**; the matching resolve returned 4,096 bytes. Tampering with
`adapter_set_root` or `tokenizer_root` returned `Malformed`; swapping the
payload returned `Corrupt`. The store-scope matrix denied a bare descriptor id,
`ContentId`/`ObjectId`, another provenance descriptor, and no envelope; the
mode-12 reader and the mode-19 reader each rejected the other's frame
(`Corrupt`). `verify --full` was clean.

**Fails if.** A page can be resolved without its provenance dimensions, or a
tampered derived root is accepted, or a foreign scope returns bytes. One
accepted foreign page refutes the claim.

### C3 — Region addressing makes small reads cheap at small byte cost

**Experiment.** Store one real 8,716,288-byte BF16 MLP tensor as a single
mode-10 shard and as mode 15 with `[32,256]` tiles; read 100 rows and 1 row.
Separately, store the 272,269,312-byte embedding as mode 9 and as mode 17 with
`[64,896]` tiles and read one and 100 rows.

**Receipts.** `bench/receipts/tensor-tiles-20261105T222531Z.json`;
`bench/receipts/float-tiled-20261107T042955Z.json`.

**Measured (MLP tensor, 10 reps).** A 100-row read fell from 130.13 ms /
7,213,077 store bytes / 1 unit to 12.05 ms / 934,413 B / 76 tiles — 10.8×
faster, 87.0 % fewer bytes. A 1-row read fell from 129.98 ms / 7,213,077 B to
3.22 ms / 281,816 B — 40.4× faster, 96.1 % fewer bytes. Outputs were byte-equal.
**Measured (embedding).** One-row read 749.26 ms → 2.12 ms (353×); 100-row
777.08 ms → 2.30 ms (337×), for +0.51 % physical bytes (180,623,816 →
181,541,557 B) and a 3.25× write (2,286.7 → 7,434.7 ms).

**Fails if** a region read decodes or fetches bytes outside the intersecting
tiles and is not proportionally slower, or the byte overhead is not paid back
by the access pattern. The first is a correctness defect; the second is the
economic boundary in C7.

**Bound.** Full tiles only; a partial boundary tile is not representable
without padding, which the writer refuses. The prefix region path does not
verify the whole-frame trailer, by design; it relies on per-tile roots.

### C4 — Representation choice must be priced with write, read, and retention terms or it misprices

**Experiment.** Price the same logical checkpoint with (a) bytes only,
(b) bytes + measured decode cost, and (c) bytes + measured encode + decode +
restore overhead, with an explicit retention term. Measure the write path
separately and attribute it to phases.

**Receipts.** `bench/receipts/parent-graph-20261110T093000Z.json`;
`bench/receipts/augmentation-local-20261109T134132Z.json`;
`bench/receipts/throughput-20261110T145341Z.json`;
`bench/receipts/parent-graph-20261108T232016Z.json` (the retained defect).

**Measured.** On the 424 MB fixture the planned codec cut physical bytes by
**33.1 %** and raised durable write wall from **335.9 ms to 24,840.1 ms**
(**73.9×**), with **23,210 ms of write CPU** versus 270 ms for raw. A bytes-only
comparison calls that a win; a write-aware comparison does not. The 3B planned
ingest spent **865,052.9 ms of 920,052.4 ms (89.8 %)** in encode, so the write
term is not accounting noise. Adding the write term to the parent-graph
objective changed the selected plan from 809,012,976 B to 654,498,531 B and
made an 850 ms SLA refuse instead of accept-and-miss; with the measured
uncertainty term, **22/22 measured acceptances held, 0 missed**.

**Fails if** a bytes-only plan is never dominated on a measured workload, or a
write-aware plan is dominated by a bytes-only plan under the same
write/read weighting. We have the first case; we do not claim it is universal.
The retention term is implemented but **has no measured table** in this
release; it is reported as units and defaults to zero, and we do not claim
retention-aware pricing is validated.

### C5 — Class-aware retention beats fixed TTL on the measured trace

**Experiment.** Replay a 360-tick trace: 1,000 volatile pages (300 s lifetime)
and 300 ephemeral pages (6 h), at 10 and 2 writes per tick, into a 3.0 MB
capacity, with 20 snapshot-protected pages. Compare fixed TTL+LRU,
reachability-only, and class-aware ordering. Reachability is a precondition in
all three; every hit re-hashes the frame.

**Receipt.** `bench/receipts/retention-20261109T220237Z.json`; the contract is
`spec/WEFT-RETENTION.md`.

**Measured (fast rate).** Safe hit rate: fixed TTL **35.31 %** with **45 unsafe
reclaims / 154,080 unsafe bytes**; reachability-only **1.50 %**, 0 unsafe;
class-aware **74.94 %**, 0 unsafe, at **4,340,836 B** physical versus
**6,457,356 B** for fixed TTL. At the slow rate: **0.97 % / 37 unsafe** for
fixed TTL, **2.64 % / 0** for reachability-only, **73.47 % / 0** for
class-aware.

**Fails if** class-aware ordering reclaims a reachable blob, or fixed TTL has a
higher safe hit rate at equal capacity on a trace with the same class
distribution. Our own misclassification runs show the boundary: declaring
volatile pages ephemeral retained **1,448,352 B** it did not need; declaring
ephemeral pages volatile cost **7.69 points** of hit rate and 453 early
reclaims, still with 0 unsafe reclaims.

**Bound.** A class is a hint, not a guarantee. The run ends **2,848 B over** the
capacity bound and peaks **1,451,200 B over** during the seeding burst. One
synthetic trace, one host.

### C6 — Multi-writer publication can be MW2 without consensus

**Experiment.** Eight independent processes publish disjoint transactions to
one store; eight contend on one name; four crash stages are injected in both
languages; a delayed writer resumes after a successor committed; readers read
across writers. A TLA+ model checks the protocol on two committers.

**Receipts.** `bench/receipts/multiwriter-court-20261109T175012Z.json`,
`bench/receipts/multiwriter-failure-20261109T175014Z.json`,
`bench/receipts/tla-20261109T174959Z.json`; contract
`spec/WEFT-MULTIWRITER.md`.

**Measured.** 160/160 disjoint transactions committed with **0 conflicts**
(208 attempts; retry amplification 1.3). On the hot name, **1 of 80** committed
and 79 returned typed conflicts, exactly one winner. Rust and Go produced
byte-identical commit and superblock bytes and read each other's objects. The
failure court recovered old-or-new at every injected stage, with outcome
lookup, idempotent replay, and `verify --full`. The TLA+ model checked
2,690,909 states / 268,641 distinct with nine invariants; the buggy variant
violates `NoBlindOverwrite` and the GC variant violates
`GcNeverReclaimsReachable`.

**Fails if** two disjoint transactions lose an acknowledged commit, a hot-name
race produces two winners or a blind overwrite, or a reader observes a
non-prefix state. Any one refutes the claim.

**Bound and loss.** This is **not** a consensus protocol and not a scaling
result. Throughput falls from 4,000 tx/s at N=1 to 580 tx/s at N=32 (efficiency
0.0045) on the local backend because there is still one publication point.
MW2 is opt-in on explicit transactions; FUSE mutations and generic checkpoint
publication remain MW1; MW3 is not implemented.

### C7 — The augmentation cost surface has break-evens that must be published

**Experiment.** Run the same five configurations at 424,439,296 logical bytes
and at 1,849,344 bytes, on local POSIX and on MinIO, and report every delta.
Price a representation change against reads on a real model family.

**Receipts.** `bench/receipts/augmentation-local-20261109T134132Z.json`,
`bench/receipts/augmentation-minio-20261109T134257Z.json`,
`bench/receipts/augmentation-local-20261109T134434Z.json`,
`bench/receipts/rebase-scale-20261106T130054Z.json`.

**Measured.** At 424 MB, pass-through overhead over native is **+396.5 %**
(local) and **+111.3 %** (MinIO); at 1.8 MB it is **+1,032 %**. The codec saves
**33.1 %** of bytes and costs **74×** write at 424 MB; at 1.8 MB it saves
**27.4 %** and costs **3.0×** restore. The protocol layer over raw costs
**+0.9 %** write and **+1.6 %** restore locally, **+3.1 % / +6.3 %** on MinIO.
On the real Qwen family, an 8.7 MB MLP tensor's policy break-even is
**87.2 ms/read**; with a 200 ms threshold only **16 of 291** objects were
selected, and the selected set's decode fell **57.8 %** while full restore fell
**22.9 %**, at **+19.45 %** physical bytes on the selected set.

**Fails if** an overhead is independent of fixture size, or a break-even
published here is not reproducible from the same receipt at the stated
threshold. The numbers are workload-specific; the claim is that they must be
published, not that they are universal.

---

## 2. Method

**Courts.** Every measurement ran inside an OOM-protected Docker container:
memory 3 GiB and swap 3 GiB, 4 CPUs, 1,024 pids, a 1 GiB bounded `/tmp`,
`no-new-privileges`, and `cap-drop=ALL`. The runner is
`scripts/docker-court.sh`; it refuses to run without the limits. Images and IDs
are recorded per receipt (`weft/bench-court:1` `sha256:4c6c41fb…` for the
augmentation and planner courts; `weft/fuse-court:1` `sha256:29cf60e7…` for
mixed-language store/FUSE courts). The host is used only for editing, git, and
Docker orchestration.

**Fixtures and identities.** No weights are committed. Public corpora are
re-hashed in the court and the run is refused on mismatch:

| Fixture | Bytes | SHA-256 |
|---|---:|---|
| Qwen2.5-0.5B-Instruct container | 988,097,824 | `fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe` |
| Qwen2.5-0.5B container | 988,097,824 | `88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342` |
| pythia-70m step 0 | 281,715,176 | `17a077abfddb6c589091e292adfcc2ee82d39bfd839f5dd80aaaea42861aaf8e` |
| pythia-70m step 1000 | 281,715,176 | `09883f904c4a50de85c682cdfa0434aaf34e62392fe883e58410d12d39b1ee88` |
| Qwen2.5-7B-Instruct merged container | 15,231,272,088 | `5527ae977f6b2a2fc0add1d76834f1300a607f79aa4bdce1457bd738523c1ef1` |

The augmentation fixture is a deterministic subset: the embedding plus 100
name-sorted, evenly spaced tensors = 101 tensors / 424,439,296 logical bytes.
The synthetic `bench/corpora/ai-v1` corpus is deterministic, CC0-1.0, and
labelled synthetic wherever it is used.

**Exactness.** Every restore compared SHA-256 with the source container; every
store run finished with `verify --full`; every cross-language run compared
frames byte-for-byte or decode-equivalence as the mode requires (RAW through
STRUCTURED promise byte-identical encoding; ZSTD promises decode equivalence).
A hash mismatch fails closed. The conformance corpus is replayed from one
manifest by independent Rust, Go, and Python readers.

**Cost model.** Decode and encode rows are host-local calibrations measured on
the same court and stored in a strict document (`weftfs-r2/decode-calibration`,
`weftfs-write-cost`, `weftfs-s13b/restore-overhead`, `weftfs-r9/hop-uncertainty`).
A document that fails schema, monotonicity, sample-count, or digest checks is
refused; there is no fallback to a neighbouring row. The planner refuses to
price a strategy with no measured row (`UnmeasuredCost`).

---

## 3. Results table

All values are from the receipts named in §1. "Loss" is the column we would
remove first if we were selling something.

| Claim | Experiment | Primary result | Loss / failure boundary |
|---|---|---|---|
| C1 | 101 tensors, five representations; COW vs overlay vs compaction | 101/101 exact, 0 mismatches; footprints 424,439,296 → 284,139,539 B; same content root `c00367f0…` across representations | Identity is over logical bytes only; lossy representations are outside the claim |
| C2 | Two KV pages, same bytes, different provenance; tamper; scope matrix | Cross-scope `Unauthorized`, 0 bytes; tampered roots `Malformed`; payload swap `Corrupt` | Provenance dimensions are not a confidentiality boundary; salt offers no timing/size privacy |
| C3 | 1-row and 100-row reads, mode 10 vs 15; mode 9 vs 17 | 40.4× / 10.8× on the MLP tensor; 353× / 337× on the embedding at +0.51 % bytes | Full tiles only; prefix reads skip the whole-frame trailer; one tensor |
| C4 | Bytes-only vs write+read+overhead pricing; 424 MB fixture; 3B ingest profile | 73.9× write for 33.1 % fewer bytes; 89.8 % of ingest is encode; 22/22 accepted SLAs held | Retention term has no measured table; SLA floor is at the 15 % prediction boundary |
| C5 | 360-tick synthetic trace, three policies | 74.94 % vs 35.31 % safe hit rate; 0 vs 45 unsafe reclaims; 4.34 MB vs 6.46 MB | Class is a hint; 2,848 B over capacity at the end; one trace |
| C6 | 8 writers, hot name, 4 crash stages, TLA+ | 160/160 disjoint; 1/80 hot winner; 0 lost acks; 2.69M states | Efficiency 0.0045 at N=32; no consensus, no fleet, MW1 default |
| C7 | Five configurations at two sizes, two backends; rebase on a real family | Overhead +396.5 % → +1,032 % as size falls; 87.2 ms/read break-even; +19.45 % bytes for −57.8 % decode | Workload-specific; one host; warm caches; no cloud |

---

## 4. Threats to validity

1. **Synthetic fixtures.** The ai-v1 corpus is synthetic and is never used for
   a superiority claim. The model-family results use public weights, but the
   *workloads* (write once, read 1/100/1,000 times; 10/2 pages per tick) are
   synthetic traces.
2. **Warm caches, one host.** Every number is a single-host, page-cache-warm
   measurement unless a receipt says otherwise. The 7B restore reports
   1.089× against a 13.97× Amdahl cap because one backend mutex serialises
   reads; that is a measured limit, not a hardware limit.
3. **No fleet, no GPU, no live cloud.** MinIO and Ceph RGW are real
   S3-compatible servers; `fake-gcs-server` and Azurite are emulators. No AWS,
   Google Cloud, or Azure account was exercised. No GPU-direct path exists.
4. **No physical power loss.** Crash courts are process-fault proxies
   (`SIGKILL`/exit 134). The dm-flakey and QEMU courts are the closest we have;
   host cache loss, media loss, and RAPL energy are unclaimed.
5. **MW1 is the default.** MW2 is opt-in on explicit transactions. FUSE
   mutations and generic checkpoint publication are single-committer. MW3 does
   not exist.
6. **Go gaps.** Go lacks the Azure backend, streaming per-tensor ingest, the
   parent-graph planner, and a writer for mode 19; its overlay and pack support
   is read/verify/GC, with a writer added in R20. Cross-language claims are
   limited to the surfaces named in the receipts.
7. **Planner pricing is partly unvalidated.** Encode and decode rows are
   measured; the retention term is explicit and unmeasured; modes 17 and 20
   cannot be priced at all (`UnmeasuredCost`). Over-pricing sparse-delta and
   zstd-patch keeps chain predictions conservative.
8. **Percentiles are observations.** Per-tensor p95/p99 are dominated by the
   272 MB embedding; they are not a codec microbenchmark.

---

## 5. What would falsify this

| Claim | A concrete refutation |
|---|---|
| C1 | Produce two representations of one logical object with different content roots, or a restore whose SHA-256 differs. |
| C2 | Resolve a KV page by content hash alone and receive bytes from a foreign provenance scope; or get a tampered derived root accepted. |
| C3 | Show a region read whose fetched bytes or decode work scale with the whole object rather than the intersecting tiles, or a byte overhead that never repays on any access pattern. |
| C4 | Find a bytes-only plan that is Pareto-better on a real write-heavy workload under the same write/read weighting; or show the encode phase is not the dominant write cost. |
| C5 | Run the same trace with the same class distribution and find fixed TTL with a higher safe hit rate at equal capacity, or any class-aware unsafe reclaim. |
| C6 | Produce two acknowledged disjoint MW2 commits where one is lost, or two hot-name winners, or a non-prefix reader state. |
| C7 | Reproduce a cell and find an overhead independent of fixture size, or a break-even that does not hold at the stated threshold. |

The cheapest refutation is C7 at a third fixture size: it needs only the
augmentation harness and one more tensor subset. The most valuable refutation
is C4 on a production write-dominated trace, because it would show the
74× write penalty is an artifact of our encoder rather than an inherent cost of
exact planned representation.

---

## 6. Where to check us

- Claims and receipts: this file, §1.
- Normative documents and their status: [`RFC.md`](RFC.md).
- Third-party conformance bundle: `bench/bundles/conformance-1.2.0.tar.zst`
  (build with `scripts/package-conformance.sh`), and
  [`CONFORMANCE.md`](CONFORMANCE.md).
- Every loss, per subphase: `bench/RESULTS-*.md` and
  [`RELEASE-v1.2.0.md`](RELEASE-v1.2.0.md).
- Adversarial reading of our own headlines: [`CLAIM-AUDIT.md`](CLAIM-AUDIT.md).

If a number here disagrees with its receipt, the receipt wins and this file is
wrong.
