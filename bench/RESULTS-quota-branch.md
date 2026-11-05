# P5a — write budgets and branching time travel: measured results

Receipts:

- `bench/receipts/quota-branch-20261105T071244Z.json` — quota admission
  overhead, branch creation, and merge. Raw log beside it. Measured at WeftFS
  `dd1f433`, WeftFS-Go `8372e6f`.
- `bench/receipts/branching-20261105T071457Z.json` — 10,000-version branch
  demo (7/7 steps pass). Raw log and `.results.tsv` beside it.

The earlier `quota-branch-20261105T070251Z.json` and
`branching-20261105T070415Z.json` were generated one commit earlier (WeftFS
`dedf1c2`) and are retained as history; the numbers agree within noise.

Environment (both receipts): `weft/bench-court:1`
(`sha256:e80c438516a509c251d6f3ca4848363a86d12a92287a1d9284117acd9cc0d85a`)
and `weft/fuse-court:1`
(`sha256:e74926322e19ab08833ddce3db2086d8b6be3434880ba969dd879266d7f7cba8`),
kernel 7.3.1-1-cachyos, 4 CPUs, 3 GiB memory, 3 GiB swap, 1024 pids, no host
cache dropping. Revisions are recorded in each receipt; the code under test is
the P5a commit.

Label: **single host, synthetic deterministic fixtures, local store on the
court bind mount, one repetition, warm caches. Not a fleet claim.**

## A. Quota admission overhead (1,000 single-object commits)

| Workload | p50 per commit | p95 per commit | mean | wall | store bytes | verify |
|---|---:|---:|---:|---:|---:|---|
| no budget | 11.35 ms | 14.55 ms | 11.70 ms | 11.70 s | 1,926,625 | ok |
| budget (`u64::MAX`, enforced) | 13.75 ms | 17.75 ms | 14.26 ms | 14.26 s | 1,926,914 | ok |

- Delta **+2.41 ms p50 (+21.2 %)** per commit. The whole delta is the durable
  usage record: a tmp file, fsync, rename, and directory fsync per commit, so
  usage survives reopen and a crash cannot leave a silently stale counter.
- Quota-run usage after 1,000 commits: 1,000 objects, 1,383,000 bytes.
- Each store publishes exactly 1,001 generations (genesis + 1,000 creates).
- No budget-aware fast path skips the record when nothing is charged; every
  commit when a policy exists rewrites it, keeping generation/commit keys exact.

## B. Snapshot branch creation (50 branches over a 32-object store)

| metric | value |
|---|---:|
| p50 | 9.11 ms |
| mean | 9.25 ms |
| p95 | 10.61 ms |
| max | 13.11 ms |
| snapshots after 50 branches | 51 |

Each branch is one commit (the `set_snapshot` op) plus one snapshot pointer
write. A branch never rewrites history; it is a second name for an immutable
commit.

## C. Union merge of 1,000 disjoint paths

| metric | value |
|---|---:|
| merge wall | 255.6 ms |
| added objects | 1,000 |
| added entries | 1,000 |
| generations | 3,003 → 3,004 |
| full verify after merge | ok |

Setup: 1,000 left-only paths and 1,000 right-only paths (distinct content),
two snapshots, the right-only paths removed from the live head, then one merge.
The merge copies object records verbatim (ids free), publishes exactly one
commit, and is idempotent on a repeat run (bench asserts 0 additions).

## D. 10,000-version branch demo

| metric | value |
|---|---:|
| versions | 10,000 |
| snapshots (`s1`, `s500`, … `s10000`) | 21 |
| ingest wall | 220.49 s |
| `s5000` commit | `72ace03186863e95c9fe2c7f3ae166c8d5cfe2eed9a118cea6a34f5e0443cc7d` |
| `s10000` commit | `da172164095e35da79952b4a22b52130cc47a50a65b10367ec004088036a2762` |
| branch Context Root id | `0c3918b7fbfeb356e168fbb0e7adf4875781efe3ef87a3cd58f2a81e324fe575` |
| mounts (2 snapshots + snapshot ref + context) | 4 |
| mount wall | 1.35 s |
| daemons surviving unmount | 0 |

After `snapshot branch s5000 by:task=agent-43`, `context branch fork-5000
by:task=agent-43 --task agent-43`, and five further writes on the live head:

- both `s5000` and `s10000` commits are unchanged;
- the branch ref still names `s5000`;
- snapshot mounts of `s5000` and `s10000`, the snapshot ref, and the branch
  Context Root all read the exact `version-5000` / `version-10000` bytes;
- the live head reads `version-10005`;
- `verify --full` passes before and after the writes.

## Honest limits

- One host, one repetition, warm caches. The quota delta is a per-commit fsync
  cost and will track the storage device, not these absolute numbers.
- The quota delta is the whole persistent-record cost; there is no measured
  case where usage is kept only in memory, because that would not survive
  reopen.
- Merge throughput includes copying 1,000 object records and 1,000 entries
  into one commit; it is a union merge, not a content merge, and no attempt is
  made to resolve conflicting bytes.
- The demo's 10,000 versions are synthetic fixed-payload files; the numbers do
  not describe model weights or a fleet.
