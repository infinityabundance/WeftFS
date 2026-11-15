# P03 — training-ready cost at scale, cached plans, no silent freshness loss

**Date:** 2026-11-15 (refreshed 2026-11-15T155500Z). **Court:**
`bench/scripts/training_ready_cost_court.sh` (fuse court, default limits).
**Receipt:** `bench/receipts/training-ready-cost-20261115T155500Z.json` and
`.log`, at WeftFS `10fde89`, WeftFS-Go `21c41b1`, WeftCodec `98019d8`,
WeftCodec-Go `d45fcb4`. **Corpus:** Qwen/Qwen2.5-0.5B BF16, two raw shards, in
a copy of the cached P3 store (weights never committed).

<!-- BEGIN GENERATED:trc-corpus (scripts/perf_numbers.py) -->
Corpus: 290 tensors, 988,065,536 element bytes; materialized 988,649,840 bytes.
<!-- END GENERATED:trc-corpus -->

## 1. The cache and its invalidation contract

`src/ready_cache.rs` derives a key from:

* the store head and generation;
* a whole-state digest over every object's `(id, version, content, size, kind,
  mode, uid, gid, attrs digest)`, every entry, every relation (including the
  target bytes), every snapshot and every tip;
* the Context Root name, recorded `ContextRootId` and `SelectorId`, and the
  pinned snapshot;
* a policy digest over `TrainingReadyOptions` (byte/object/decode envelopes,
  `require_tensors`) plus the capability hash and expiry;
* a mutable-authority digest over the entire `training/grants` subtree
  (including each epoch file) and the `quarantine` subtree.

The key is recomputed on every call. A found entry is used only when its
schema, key, head, generation, state digest, authority digest, options digest,
evidence digest and every cached `(object, version, content, size)` still
match. Otherwise the entry is rejected with a recorded reason and the full
verification runs. A cache miss never returns a pass; a hit never widens the
pinned snapshot.

`tests/training_ready_cache.rs` (5/5) mutates each input in turn:

| Mutation | Result | Evidence |
|---|---|---|
| unrelated commit (generation) | miss, re-verify | `every_input_invalidates` |
| member manifest/content change, root retargeted | miss, new snapshot verified | `every_input_invalidates` |
| causal parent added to a member | miss, re-verify | `every_input_invalidates` |
| policy envelope changed | miss, re-verify | `every_input_invalidates` |
| grant added (authority epoch) | miss, re-verify | `every_input_invalidates` |
| quarantine record added | miss, re-verification **refuses** `Quarantined` | `every_input_invalidates` |
| tampered evidence field | miss, rejection recorded, entry rebuilt | `a_tampered_cache_entry_reverifies` |
| entry from another state copied to the new key | miss, rejected | `a_cache_entry_from_another_state_is_rejected` |
| cold → warm → refresh | miss → hit → miss | `cold_then_warm_then_refresh` |

## 2. Time-to-ready

The court reports the split between validation (ready-check), decode and
materialization (output write). All times are wall-clock in the 3 GiB/4-cpu
court.

| Phase | ms |
|---|---:|
<!-- BEGIN GENERATED:trc-table (scripts/perf_numbers.py) -->
| Cold ready-check (cache miss) | 42,530.2 |
| Warm-first ready-check (cache hit) | 50.9 |
| Warm ready-check p50 / min / max | 49.0 / 47.9 / 50.4 |
| Check after a policy change | 42,711.2 |
| Check after a grant edit | 43,068.7 |
| Cold materialize | 54,576.2 |
| Warm materialize | 63,355.9 |
| Cold check validation+decode | 42,415.0 |
| Materialize write after decode | 11,745.2 |
| Time-to-ready cold | 97,106.3 |
| Time-to-ready warm | 63,406.8 |
| Warm plan time | 1.111 |
| Savings from a warm check | 42,481.1 (867.3×) |
<!-- END GENERATED:trc-table -->

The cold check in the table is below the 77,250.0 ms of the earlier P7 court
(receipt `bench/receipts/training-ready-20261105T171104Z.json`, stamp
`20261105T171104Z`, revision `807d2ee`); the shard layout and a warm cache
differ. The cache removes that validation, and the warm/cold ratio is in the
table.

The achieved **time-to-ready** is in the table: 97,106.3 ms cold and
63,406.8 ms warm. That materially exceeds the earlier 77,250.0 ms P7 number,
because time-to-ready now includes materialization and the P7 court measured
the ready-check alone. The check alone is 42,530.2 ms cold — below the earlier
number — and the dominant term of time-to-ready is materialization, not
validation: decode plus the output write, cold and warm, in the table. The cache
does **not** skip materialization: `materialize --training-ready` always
re-verifies and writes, so a warm load still pays the decode + write cost.

The time-to-ready numbers are published as measured and are not re-framed. In
this run the **warm** materialize (63,355.9 ms) is 16.1 % **slower** than the
cold one (54,576.2 ms) on the same host. The two passes write to separate output
directories (`materialized`, then `materialized-2`), so the second pass writes
988 MB of fresh files beside the first on the same filesystem rather than
re-materializing in place; that is a loss, not a cache effect, and the runs are
not interleaved. The earlier stamp showed the warm pass marginally faster.
Treat the direction as host writeback noise and the magnitude as real work.

## 3. What each cache actually saves

* Saves: the entire cold validation pass (cold/warm ratio in the table),
  including re-hashing every member and re-decoding every tensor.
* Does not save: materialization decode or the output write; warm and cold
  materialize differ by run-to-run writeback variance (table, and §2: warm was
  16.1 % slower on this stamp), and the cache does not change that path.
* Does not save: the plan/state digest, which is O(state); the warm plan time
  is in the table.

## 4. Losses

* **Over-invalidation.** The key uses a whole-state digest, so any unrelated
  commit invalidates. That is safe but wasteful; an incremental per-object
  digest is not implemented.
* **O(state) planning.** Every cached check walks every object, relation,
  snapshot and tip to compute the digests. On this fixture the warm plan
time is in the table; it grows with state, not with the requested closure.
* **The cache stores a full report.** Each entry includes every file, tensor
  and checkpoint record as JSON. The on-disk size was not measured as a
  separate metric; it is bounded by the member count, not the tensor bytes.
* **Warm materialize is not faster.** The cache is not wired into the
  materialization copy path; a production consumer that wants to skip the
  second decode needs a different (and riskier) contract. On this stamp the warm
  pass was slower than the cold one (§2).
* **No repeated-load measurement at a larger scale.** The gate asks for repeated
  loads; the court measures one cold, five warm checks and one warm materialize
  on one public checkpoint. A long training-load loop (hundreds of resumes) was
  not run, so there is no steady-state number and no cold-page-cache arm.
* **Unsigned local state.** The cache entry is evidence-digested, not signed.
  A process that can write the cache directory can forge an entry; the cache is
  derived state, like an index, and `--no-cache` / `weft verify --full` remain.
* **No cross-process lock.** Two processes sharing one cache directory can
  write the same key concurrently; the atomic rename makes the last writer win,
  and a torn read is rejected, but there is no single-writer guarantee.
* **Capability expiry.** A cached entry is not used past the capability's
  expiry, but there is no per-request revocation-epoch check in this path.

## 5. Non-claims

No fleet, multi-node, cloud, GPU or physical-media claim. The corpus is one
public model, one host, a warm page cache and a copy of a cached store. The
cache does not make training-ready true; it only avoids repeating a verification
whose inputs are provably unchanged. `verify --full` remains the independent
check.
