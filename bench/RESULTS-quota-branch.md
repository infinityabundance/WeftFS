# P5a/Q3 — write budgets and branching time travel: measured results

Receipts:

- `bench/receipts/quota-branch-20261105T224337Z.json` — **Q3** quota admission
  overhead (usage in the commit), branch creation, and merge. Raw log beside
  it. Measured at the Q3 code revision; the receipt records the court image,
  kernel, and revision environment. Receipt SHA-256
  `b94f575d7f5e39cccd5f34d28d05db2af0576751de780c50912ada17215b05e0`.
- `bench/receipts/quota-branch-20261105T071244Z.json` — **P5a historical** run
  (usage written to a separate durable `quota` record after every commit). It
  is retained as the before measurement and is not a run of the Q3 binary.
- `bench/receipts/branching-20261105T071457Z.json` — 10,000-version branch
  demo (7/7 steps pass). Raw log and `.results.tsv` beside it.

Label: **single host, synthetic deterministic fixtures, local store on the
court bind mount, one repetition, warm caches. Not a fleet claim.**

## Q3 — usage carried by the commit

1,000 single-object commits, 1 KiB distinct payload each, `chunk_size 4096`,
checkpoint interval disabled. The quota run appends one `set_quota_usage` op to
each commit; it performs **no** separate quota-file write or fsync.

| Workload | p50 per commit | p95 per commit | mean | wall | store bytes | verify |
|---|---:|---:|---:|---:|---:|---|
| no budget | 11.28 ms | 14.93 ms | 11.66 ms | 11.661 s | 1,926,625 | ok |
| budget (`u64::MAX`, enforced), usage in commit | 11.34 ms | 14.85 ms | 11.74 ms | 11.736 s | 2,028,648 | ok |

- Delta **+0.066 ms p50 (+0.59 %)** per commit versus the same-run no-budget
  case. The whole delta is the ~102 B usage op inside the existing commit;
  there is no second durable write.
- Historical P5a before (separate durable record, same court): p50 13.754 ms
  versus 11.348 ms no-budget, **+2.406 ms (+21.2 %)**. The Q3 path is
  **−17.5 %** against that before p50.
- Quota-run usage after 1,000 commits: 1,000 objects, 1,383,000 bytes. Each
  store publishes exactly 1,001 generations (genesis + 1,000 creates).
- The policy file is not rewritten by any of the 1,000 commits; it is written
  only at policy install/migration.

Run wall 54.31 s, peak RSS 16,716 KiB.

### Same-run branch and merge checks

| metric | value |
|---|---:|
| branch p50 | 9.01 ms |
| branch p95 | 11.86 ms |
| snapshots after 50 branches | 51 |
| union merge of 1,000 disjoint paths | 256.6 ms |
| added objects / entries | 1,000 / 1,000 |
| generations | 3,003 → 3,004 |
| full verify after merge | ok |

These are unchanged by Q3; they are reported from the same receipt so the run
is self-contained.

## Historical P5a run (superseded for §A)

Retained for the before/after comparison. The tables below are the original
P5a receipt and describe the old separate-record implementation.

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

- One host, one repetition, warm caches. The per-commit delta tracks the
  storage device and the commit size, not these absolute numbers.
- The Q3 delta is the ~102 B `set_quota_usage` op inside the existing commit.
  There is no separate durable usage write in Q3, so the historical P5a
  "one extra fsync per commit" cost is no longer present. No energy, syscall,
  or fleet claim is made.
- Merge throughput includes copying 1,000 object records and 1,000 entries into
  one commit; it is a union merge, not a content merge, and no attempt is made
  to resolve conflicting bytes.
- The demo's 10,000 versions are synthetic fixed-payload files; the numbers do
  not describe model weights or a fleet.
