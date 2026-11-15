# P03 — training-ready cost at scale, cached plans, no silent freshness loss

**Date:** 2026-11-15. **Court:** `bench/scripts/training_ready_cost_court.sh`
(fuse court, default limits). **Receipt:**
`bench/receipts/training-ready-cost-20261115T140742Z.json` and `.log`.
**Corpus:** Qwen/Qwen2.5-0.5B BF16, two raw shards, in a copy of the cached P3
store (weights never committed).

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
| Cold ready-check (cache miss) | 42,612.6 |
| Warm-first ready-check (cache hit) | 49.4 |
| Warm ready-check p50 / min / max | 47.7 / 46.8 / 48.8 |
| Check after a policy change | 42,789.3 |
| Check after a grant edit | 43,090.6 |
| Cold materialize | 54,945.7 |
| Warm materialize | 54,467.9 |
| Cold check validation+decode | 42,493.0 |
| Materialize write after decode | 11,937.7 |
| Time-to-ready cold | 97,558.3 |
| Time-to-ready warm | 54,517.2 |
| Warm plan time | 1.152 |
| Savings from a warm check | 42,564.8 (892.8×) |
<!-- END GENERATED:trc-table -->

The cold check in the table is below the ~77 s of the earlier P7 court
(receipt `bench/receipts/training-ready-20261105T171104Z.json`, revision
recorded there); the shard layout and a warm cache differ. The cache removes
that validation, and the warm/cold ratio is in the table. The dominant term of
time-to-ready is now materialization: decode plus the output write, cold and
warm, in the table. The cache does **not** skip materialization:
`materialize --training-ready` always re-verifies and writes, so a warm load
still pays the decode + write cost.

Both time-to-ready totals are in the table. The warm total is below the earlier
check-only court because the check is gone; the cold total includes
materialization. Neither is re-framed: the achieved numbers and the dominant
term (materialization write/decode) are published as measured.

## 3. What each cache actually saves

* Saves: the entire cold validation pass (cold/warm ratio in the table),
  including re-hashing every member and re-decoding every tensor.
* Does not save: materialization decode or the output write; warm and cold
  materialize are close on this run (table), and the cache does not change that
  path.
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
  second decode needs a different (and riskier) contract.
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
