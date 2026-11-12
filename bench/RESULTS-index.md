# Results index — where to start and what to read next

`bench/RESULTS-*.md` are analyses; the numbers in them come from receipts in
`bench/receipts/`. This page is the map. It is deliberately not a summary: if a
document and a receipt disagree, the receipt and the document's revision line
win, and the disagreement is a defect worth reporting.

**If you read three things:** the write-cost ledger
([`RESULTS-write-cost-ledger.md`](RESULTS-write-cost-ledger.md)), the write
frontier ([`RESULTS-pareto.md`](RESULTS-pareto.md)), and the honest headline in
[`../README.md#demonstrated-results`](../README.md#demonstrated-results). The
first explains *where the write time goes*; the second is the current
wall-versus-footprint frontier; the third leads with the losses.

**One rule for every table.** A row is only a result if it names its court,
fixture, revision and receipt. Every document below that measured something
also lists what it did **not** measure. A retained failure is evidence, not
noise: `bench/receipts/` keeps the receipts that found defects, including the
ones whose fix came later.

## The two current documents

| Document | Why it is current |
|---|---|
| [`RESULTS-write-cost-ledger.md`](RESULTS-write-cost-ledger.md) | R17 attribution: durable write wall by phase, per-candidate cost, and the ablation set. Every later write rung quotes this ledger or corrects it. |
| [`RESULTS-pareto.md`](RESULTS-pareto.md) | R22 frontier: 25 configurations × 7 interleaved repetitions, the Pareto set, G1/G2/G3, and the measured reason the ≤ 336 ms rung is not reached. |

Supporting current documents: [`RESULTS-mode9-encoder.md`](RESULTS-mode9-encoder.md)
(R19), [`RESULTS-publication-tax.md`](RESULTS-publication-tax.md) (R20),
[`RESULTS-parallel-pipeline.md`](RESULTS-parallel-pipeline.md) (R21),
[`RESULTS-planner-two-stage.md`](RESULTS-planner-two-stage.md) (R18), and
[`CONTRACTS-augmentation.md`](CONTRACTS-augmentation.md) (the frozen
same-backend A/B method that defines "native" as *the same backend without
WeftFS*, never the reference FUSE filesystem).

## Every retained loss, by surface

| Surface | Document | The loss |
|---|---|---|
| Write path | [`RESULTS-pareto.md`](RESULTS-pareto.md), [`RESULTS-write-cost-ledger.md`](RESULTS-write-cost-ledger.md) | compressed write is 1.78× native at the frontier; the ≤ 336 ms rung is not reached |
| Planner candidates | [`RESULTS-compression-candidates.md`](RESULTS-compression-candidates.md) | level-19 zstd and mode-4 patch both lose where they lose; the patch total is plan-only, not store bytes |
| Checkpoint queue | [`RESULTS-checkpoint-queue.md`](RESULTS-checkpoint-queue.md) | a bounded queue improves loop wall but worsens completion p95 and raises RSS |
| DCP metadata | [`RESULTS-dcp-metadata.md`](RESULTS-dcp-metadata.md) | the first save of a shape pays a plan build; a one-save process never amortises |
| Incremental index | [`RESULTS-incremental-index.md`](RESULTS-incremental-index.md) | index bytes still grow in the number of commits; 1M generations does not fit the court |
| Overlay | [`RESULTS-overlay.md`](RESULTS-overlay.md) | cold reads pay chain resolution; disk after compaction can exceed COW while a snapshot pins bytes |
| Packs | [`RESULTS-packs.md`](RESULTS-packs.md) | cold-read amplification, catalogue bytes, `verify --full` slower |
| Restore | [`RESULTS-restore-scaling.md`](RESULTS-restore-scaling.md), [`RESULTS-restore-pipeline.md`](RESULTS-restore-pipeline.md) | whole-file SHA is a serial floor; 4×4 oversubscription is slower; RSS +14–20 % |
| Real model | [`RESULTS-real-model.md`](RESULTS-real-model.md), [`RESULTS-float-tensor-real.md`](RESULTS-float-tensor-real.md) | mode 9 loses on F32; restore is slower than zstd strategies; delta loses on a 19-byte change |
| Multi-writer | [`RESULTS-multiwriter.md`](RESULTS-multiwriter.md) | MW2 scales *down* (4,000 → 638 tx/s at N=32); MW3 absent |
| Augmentation | [`RESULTS-augmentation.md`](RESULTS-augmentation.md) | 33.1 % fewer physical bytes costs 73.9× the write wall (native A 336 ms → C 24,840 ms) |
| 7B scale | [`RESULTS-scale-7b.md`](RESULTS-scale-7b.md) | per-tensor planned ingest OOM-killed; packed sequential +51.2 % footprint |
| Fleet | [`RESULTS-fleet-coldstart.md`](RESULTS-fleet-coldstart.md) | single host, one shared handle, one backend mutex; no fleet claim |
| Cache | [`RESULTS-cache.md`](RESULTS-cache.md) | cache tiles bypass single-flight; round 1 still races; per-process only |
| Transport | [`RESULTS-transport.md`](RESULTS-transport.md) | containers add bytes; 64 KiB alignment costs up to 64 KiB per section |
| Entropy coders | [`RESULTS-entropy-coders.md`](RESULTS-entropy-coders.md) | rANS is smaller but 2× slower; Tunstall loses at every setting — nothing promoted |
| Attested loads | [`RESULTS-attested-loads.md`](RESULTS-attested-loads.md) | a second full pass; cannot prove unread bytes |
| Parent graph | [`RESULTS-parent-graph.md`](RESULTS-parent-graph.md) | the 850 ms SLA misses at 886.5 ms (measured floor) |
| Fencing | [`RESULTS-fencing.md`](RESULTS-fencing.md) | single writer; two conditional writes on two objects |
| Comparative | [`RESULTS-comparative-v1.md`](RESULTS-comparative-v1.md) | store selectors ~412 ms at 10k objects vs SQLite ~1.2 µs; seq-write and fsync p99 losses |
| FUSE | [`RESULTS-fuse-v0.5.md`](RESULTS-fuse-v0.5.md) | kernel/fuser EIO boundaries; no match for a mature local filesystem |
| 3FS | [`RESULTS-threefs.md`](RESULTS-threefs.md) | ABI/conformance only; no deployed cluster |

## Storage and write path

| Document | Scope |
|---|---|
| [`RESULTS-write-cost-ledger.md`](RESULTS-write-cost-ledger.md) | R17 phase attribution and ablations |
| [`RESULTS-planner-two-stage.md`](RESULTS-planner-two-stage.md) | R18 exact two-stage selection |
| [`RESULTS-mode9-encoder.md`](RESULTS-mode9-encoder.md) | R19 byte-identical mode-9 encoder |
| [`RESULTS-publication-tax.md`](RESULTS-publication-tax.md) | R20 group publication and one durability barrier |
| [`RESULTS-parallel-pipeline.md`](RESULTS-parallel-pipeline.md) | R21 bounded reader/encoder/publisher pipeline |
| [`RESULTS-pareto.md`](RESULTS-pareto.md) | R22 frontier and measured defaults |
| [`RESULTS-checkpoint-latency.md`](RESULTS-checkpoint-latency.md) | R12b completion profiler |
| [`RESULTS-checkpoint-queue.md`](RESULTS-checkpoint-queue.md) | R12c item 4 bound and backpressure |
| [`RESULTS-dcp-metadata.md`](RESULTS-dcp-metadata.md) | R12c item 3 metadata encoding |
| [`RESULTS-incremental-index.md`](RESULTS-incremental-index.md) | R12c item 2 + 2.1 index maintenance and GC bound |
| [`RESULTS-training-goodput.md`](RESULTS-training-goodput.md) | R11 loop, not a fixture |
| [`RESULTS-training-goodput-session.md`](RESULTS-training-goodput-session.md) | R12c item 1 persistent session |
| [`RESULTS-overlay.md`](RESULTS-overlay.md) | overlay write and compaction |
| [`RESULTS-merkle-identity.md`](RESULTS-merkle-identity.md) | v3 Merkle identity write cost |

## Codec

[`RESULTS-codec-v1.md`](RESULTS-codec-v1.md),
[`RESULTS-decode-simd.md`](RESULTS-decode-simd.md),
[`RESULTS-mode17.md`](RESULTS-mode17.md),
[`RESULTS-mode18.md`](RESULTS-mode18.md),
[`RESULTS-engram.md`](RESULTS-engram.md),
[`RESULTS-sparse-delta.md`](RESULTS-sparse-delta.md),
[`RESULTS-tensor-tiles.md`](RESULTS-tensor-tiles.md),
[`RESULTS-tiled-parallel.md`](RESULTS-tiled-parallel.md),
[`RESULTS-float-tensor-real.md`](RESULTS-float-tensor-real.md),
[`RESULTS-entropy-coders.md`](RESULTS-entropy-coders.md),
[`RESULTS-compression-candidates.md`](RESULTS-compression-candidates.md),
[`RESULTS-transport.md`](RESULTS-transport.md).

## Models, checkpoints and AI workloads

[`RESULTS-real-model.md`](RESULTS-real-model.md),
[`RESULTS-family-lifecycle.md`](RESULTS-family-lifecycle.md),
[`RESULTS-trajectory.md`](RESULTS-trajectory.md),
[`RESULTS-scale-7b.md`](RESULTS-scale-7b.md),
[`RESULTS-checkpoint-topology.md`](RESULTS-checkpoint-topology.md),
[`RESULTS-checkpoint-service.md`](RESULTS-checkpoint-service.md),
[`RESULTS-bounded-ingest.md`](RESULTS-bounded-ingest.md),
[`RESULTS-orbax.md`](RESULTS-orbax.md),
[`RESULTS-parent-graph.md`](RESULTS-parent-graph.md),
[`RESULTS-rebase-scale.md`](RESULTS-rebase-scale.md),
[`RESULTS-ai-v1.md`](RESULTS-ai-v1.md).

## Storage backends and distribution

[`RESULTS-planes.md`](RESULTS-planes.md),
[`RESULTS-packs.md`](RESULTS-packs.md),
[`RESULTS-distributed.md`](RESULTS-distributed.md),
[`RESULTS-multiwriter.md`](RESULTS-multiwriter.md),
[`RESULTS-fencing.md`](RESULTS-fencing.md),
[`RESULTS-ceph.md`](RESULTS-ceph.md),
[`RESULTS-gcs.md`](RESULTS-gcs.md),
[`RESULTS-azure-go.md`](RESULTS-azure-go.md),
[`RESULTS-azure-sas.md`](RESULTS-azure-sas.md),
[`RESULTS-threefs.md`](RESULTS-threefs.md).

## Mounts, context and lifecycle

[`RESULTS-fuse-v0.5.md`](RESULTS-fuse-v0.5.md),
[`RESULTS-shared-mounts.md`](RESULTS-shared-mounts.md),
[`RESULTS-context-mounts-1000.md`](RESULTS-context-mounts-1000.md),
[`RESULTS-augmentation.md`](RESULTS-augmentation.md),
[`RESULTS-cache.md`](RESULTS-cache.md),
[`RESULTS-retention.md`](RESULTS-retention.md),
[`RESULTS-gc-accounting.md`](RESULTS-gc-accounting.md),
[`RESULTS-index-amplification.md`](RESULTS-index-amplification.md),
[`RESULTS-quota-branch.md`](RESULTS-quota-branch.md).

## Governance, comparison and packaging

[`RESULTS-capability-envelope.md`](RESULTS-capability-envelope.md),
[`RESULTS-kv-provenance.md`](RESULTS-kv-provenance.md),
[`RESULTS-kv-prefix.md`](RESULTS-kv-prefix.md),
[`RESULTS-kv-go-parity.md`](RESULTS-kv-go-parity.md),
[`RESULTS-attested-loads.md`](RESULTS-attested-loads.md),
[`RESULTS-comparative-v1.md`](RESULTS-comparative-v1.md),
[`RESULTS-v1.md`](RESULTS-v1.md),
[`RESULTS-v0.4.0-storage-path.md`](RESULTS-v0.4.0-storage-path.md),
[`RESULTS-buck2-cas-store.md`](RESULTS-buck2-cas-store.md),
[`RESULTS-write-policy.md`](RESULTS-write-policy.md).

## How a result becomes a receipt

1. A contract or court script freezes the method
   ([`CONTRACTS-augmentation.md`](CONTRACTS-augmentation.md),
   [`CONTRACTS-multiwriter.md`](CONTRACTS-multiwriter.md)).
2. The court runs inside `scripts/docker-court.sh` with the unchanged hard
   limits, records the four repository revisions and the court image id, and
   writes `bench/receipts/<name>-<stamp>.json` plus the raw log beside it.
3. The document under `bench/RESULTS-*.md` states the method, every number,
   and every loss, and cites the receipt and revision.
4. A claim is only a claim once it is in `docs/claims.json` and
   `scripts/check-claims.sh` resolves its receipt.
