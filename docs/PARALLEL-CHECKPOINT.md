# Prepare/commit checkpoint writes: N blob writers, one committer

Status: implemented (Rust store + CLI), measured, tested. No wire-format change.
Two additive options were introduced: a deterministic checkpoint ObjectId seed
and the split write path itself. Frozen manifests, commit records, and the
single-writer protocol are untouched.

## The split

```
                 prepare (N workers)                        commit (1)
  ┌────────────┐  put_if_absent(chunks)   ┌───────────────┐  validate every fragment
  │ worker 1   │ ───────────────────────► │ fragment      │  (re-read, re-hash, roots)
  │ read-only  │                          │ tensor +      │           │
  │ handle     │                          │ manifests     │           ▼
  └────────────┘                          └───────────────┘   one group commit:
  ┌────────────┐                                  │           mkdir + every PutObject/
  │ worker 2   │ ─────────────────────────────────┤           PutEntry/SetTip + manifest
  └────────────┘                                  │           file, one commit record,
        …                                         …           one superblock publication
  ┌────────────┐                                  │
  │ worker N   │ ─────────────────────────────────┘
  └────────────┘
```

* A **prepare task is one tensor**, all of its shards. The tensor-level
  `LogicalTensorRoot` spans every shard, so only the worker holding the tensor
  bytes can compute it. Tasks are statically assigned (`tensor_index % ranks`).
* Each worker opens **its own read-only store handle** (`Store::open_with_options_and_backend`,
  `OpenMode::ReadOnly`): its own backend instance and its own replayed state, so
  it can read causal base versions. It writes only **immutable, content-addressed
  blobs** through `Store::prepare_tensor_object_with_spec_seeded`
  (`put_if_absent`; on the local backend an existing blob is verified, never
  overwritten). Blob publication needs no ordering, no coordination and no
  agreement, because the key *is* the digest.
* The **single committer** is the handle that already owns the writer lock and
  the existing lease/fence. It validates every fragment against its **own**
  backend (re-reads each manifest, decodes the bytes, recomputes the content
  root, the per-shard `LogicalTensorRoot`, and the reassembled tensor root),
  then publishes **one** group commit. `Store::commit_prepared_tensor_object`
  only stages state ops: no blob is written during the commit phase.

### The single-writer proof is untouched

There is no second writer of control state. Workers never touch the superblock,
a commit record, the writer lock, `LOCK`, the lease, the fence, or the pack
catalog. They call one primitive: create-once blob publication on an immutable
key. The existing single-writer/lease/fence proof therefore applies verbatim:
one committer, one `HeadExpectation`, one superblock CAS. The split needs no
multi-writer consensus and does not weaken any guarantee.

### Failure semantics

* A worker that fails or panics returns a typed error naming the tensor and
  shard; the committer publishes nothing. The next `gc --apply` reclaims the
  unreferenced blobs.
* A committer that dies after workers published leaves the previous whole
  generation; the next open sees no checkpoint directory. The same GC
  reclaims the orphans. `tests/parallel_checkpoint.rs` proves this with a real
  `SIGKILL` of the writer process mid-prepare.
* A duplicated checkpoint name fails closed `Exists`; a blob whose existing
  bytes disagree with its key is `Corrupt`; a name already owned by state is
  `Exists`. The sequential path and the split path use the same
  `commit_created_file` checks, so their failure classes are identical.
* A duplicate shard with different bytes cannot collide: different bytes have a
  different content address. The duplicate *name* case is refused by state.

### Bounds

| bound | option | default |
| --- | --- | --- |
| worker ranks | `--ranks` | 1 (split path opt-in; omitted = sequential) |
| hard rank cap | `MAX_PARALLEL_RANKS` | 64 |
| in-flight decoded bytes | `--max-inflight-bytes` | 1 GiB |
| one rank's cumulative bytes | `--per-rank-bytes` | 512 MiB |
| prepare deadline | `--prepare-deadline-ms` | none |
| committer fragment re-verification | on | `--no-fragment-verify` to disable |

An oversized task (one tensor larger than the in-flight bound) runs alone,
exactly like `parallel::read_tensors_bounded`. Exceeding a rank's budget or the
deadline fails with `LimitExceeded`/`Cancelled` before publication.

### Byte-identical proof

`CheckpointWriteOptions::deterministic_seed` (CLI `--checkpoint-seed HEX`)
derives the checkpoint directory, manifest file, and per-shard ObjectId seeds
from one 32-byte value (`weft/checkpoint/seed/v1`). With a fixed clock
(`--fixed-timestamp-ns`) the sequential path and the split path therefore
publish **the same head, the same checkpoint manifest root, and the same store
bytes**. `tests/parallel_checkpoint.rs` asserts it for ranks 1/2/4/8; the court
asserts it on the real model.

## Measurements

Qwen2.5-0.5B base, BF16, 290 tensors, 988,097,824 B, SHA-256
`88c142557820ccad55bb59756bfcfcf891de9cc6202816bd346445188a0ed342`, `--shards 8`,
`--write-policy fast --fast-codec zstd` (level 1), fixed clock, deterministic
seed, 4 vCPU / 3 GiB court, 5 repetitions per configuration.

Local backend, receipt `bench/receipts/parallel-checkpoint-20261108T030000Z.json`:

| ranks | best prepare ms | best wall ms | median prepare ms | median wall ms | peak RSS KiB |
| --- | --- | --- | --- | --- | --- |
| sequential | — | 13,747.9 | — | 13,941.5 | 393,932 |
| 1 | 13,184.9 | 14,913.3 | 73,830.3 | 75,572.9 | 430,184 |
| 2 | 11,338.4 | 13,092.4 | 11,391.6 | 13,130.8 | 437,736 |
| 4 | 6,175.5 | 7,922.2 | 6,198.9 | 7,931.8 | 461,652 |
| 8 | 3,386.3 | 5,128.1 | 12,278.7 | 14,784.2 | 508,208 |

* Committer time (median): fragment verification 1,252–1,258 ms; the single
  commit 63.4–64.9 ms.
* Prepare speedup vs ranks=1 (best): 1.16× (2), 2.14× (4), 3.89× (8) on 4 vCPUs.
* Wall speedup vs the sequential path (best): 1.05× (2), 1.74× (4), 2.68× (8).
* Every configuration stored **775,692,888 B** and published head
  `64f685d8341098bfc15112e07339979116031da8c9124b3fc9970c8b316773a6` and manifest
  root `a2c5ec8e21c5fc354a613880d1329e97fd970615e8e3c21588b88bf924769666`.

MinIO (single node, tmpfs data dir, tensor subset `model.layers.0.*`, 96 shard
objects, 29,824,768 element bytes), receipt
`bench/receipts/parallel-checkpoint-s3-20261108T015555Z.json`:

| ranks | best prepare ms | commit ms | wall ms |
| --- | --- | --- | --- |
| sequential | — | — | 595.9 |
| 1 | 176.2 | 3.9 | 675.9 |
| 2 | 110.2 | 4.0 | 611.9 |
| 4 | 60.5 | 4.0 | 559.8 |
| 8 | 53.5 | 4.0 | 553.0 |

All five configurations publish head
`844648f4694936b72274dfff73c7647d298d7f81af9be98a25308acab0649490` and manifest
root `334375091246f6b2a8e903804960d8e46a09797f1711c3dec2066017e72157d1`.
Create-once PUTs are concurrency-safe on MinIO; the split needs no conditional
write in the prepare phase.

## Losses and limits (published, not hidden)

* The host page cache makes single wall-clock runs bimodal: a cold 988 MB read
  costs ~60 s, a warm one ~14 s. The table publishes `best` (warm, comparable)
  and `median` (raw spread). Neither is a throughput guarantee.
* Peak RSS grows with ranks (394 → 508 MiB) because every worker holds its own
  replayed state and one tensor's encoders.
* 8 ranks is not better than 4 on 4 vCPUs in the median (12.3 s vs 6.2 s
  prepare): oversubscription plus I/O interference. The best-of-N ordering is
  8 > 4, but the court deliberately publishes the median loss.
* Committer fragment verification re-decodes every shard (~1.25 s here). It is
  on by default; `--no-fragment-verify` is a performance choice, never a claim
  about untrusted workers.
* The S3 phase uses a tensor subset because the court MinIO data directory is a
  1 GiB tmpfs; it proves the create-once PUT path under concurrency, not a
  full-model S3 ingest, and the wall time there is dominated by per-handle
  store open/replay over the network.
* Rust only. WeftFS-Go has no checkpoint writer, so there is no Go parity for
  this path. No multi-writer, fleet, cloud, media, power-loss, or GPU claim.

## Reproduce

```
bash bench/scripts/parallel_checkpoint_court.sh                 # local backend
scripts/s3_court.sh --parallel                                 # MinIO subset
scripts/docker-court.sh bench "cd /work && cargo run --release --quiet \
  --example bench_parallel_checkpoint -- --fast --model \
  /work/.docker-cache/real-corpus/Qwen2.5-0.5B.safetensors --work /work/.docker-cache/parallel \
  --out /tmp/parallel.json"
```

Tests: `cargo test --test parallel_checkpoint` (6/6).
