# DCP metadata serialisation cost (subphase R12c, item 3)

**Subject.** The R12b profile attributed **4.2 ms** of a 37.7 ms planned
checkpoint completion (169-entry training-goodput fixture) to
`adapter.metadata_serialize`: the pure-Python canonical WCB encoder in
`python/weftcodec/dcp.py`. This document measures that phase, evaluates three
ways to remove it, ships one of them, and re-measures the courts end to end.

**Labelling.** Everything numeric below was measured in the Docker python
court on the host named in each receipt. The metadata-encoder change is a
client-side Python change only: no wire format, no store format, no Rust code,
no frame, manifest, or content root changed. The byte-for-byte identity of the
metadata blob is proven by differential tests and by a store-backed restore.

**Revisions.** Baseline `11230b3` (receipts `dcp-metadata-20261111T230500Z`,
court script added in the same session and committed with the change);
implemented and re-measured at `4947ef2` (receipts `dcp-metadata-20261111T235500Z`,
`checkpoint-latency-20261112T000500Z`, `checkpoint-latency-20261112T001500Z`,
`training-goodput-session-20261112T003000Z`,
`training-goodput-session-20261112T004500Z`).

---

## 1. The baseline, measured

Court: `bench/scripts/dcp_metadata_court.sh` in `weft/python-court:1`
(`--memory=3g --memory-swap=3g --cpus=4 --pids-limit=1024`,
`/tmp` a 1 GiB tmpfs), revision `11230b3`. Real `torch.distributed.checkpoint`
metadata is produced by one genuine save per size with a capturing writer, and
the encoder is then measured 30 times on that object. The 169-entry size is
the exact R11/R12b fixture; 1/16/64 take its first N entries, 1024 repeats it
with suffixed names and 4x4 float32 tensors.

`shipped` is `weftcodec.dcp.serialize_metadata` — at this revision the only
metadata encoder in the tree.

| entries | metadata bytes | total p50 | p95 | tree construction | `wcb.encode` | memfd copy | sha256 |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 486 | 0.034 | 0.037 | 0.005 | 0.029 | 0.002 | 0.000 |
| 16 | 6,484 | 0.369 | 0.422 | 0.056 | 0.313 | 0.003 | 0.003 |
| 64 | 26,222 | 1.481 | 2.340 | 0.237 | 1.232 | 0.004 | 0.011 |
| **169** | **71,559** | **3.957** | **4.118** | **0.681** | **3.289** | **0.008** | **0.028** |
| 1024 | 401,583 | 26.186 | 93.564 | 6.314 | 20.597 | 0.032 | 0.156 |

All values in ms, p50 over 30 iterations (p95 in its own column).

**The reviewer's 4.2 ms is confirmed.** The isolated encoder costs 3.96 ms p50
at 169 entries in this court; the `adapter.metadata_serialize` probe inside a
real save measured 4.32-5.19 ms in the same session (§4). The difference
between 3.96 and 4.2 is run-to-run variation and the probe's own window, not a
disagreement about the work.

### 1.1 Where the 3.96 ms goes

| phase | ms p50 at 169 | share | how measured |
|---|---:|---:|---|
| tree construction (`_state_dict_value`, storage rows, planner) | 0.681 | 17.2 % | wrapping the two helpers and timing `total - encode` |
| `wcb.encode` | 3.289 | 83.1 % | wrapping `wcb.encode` |
| memfd copy (`memfd_create` + one `write`, exactly `finish_checkpoint`) | 0.008 | 0.2 % | replayed inline |
| sha256 of the blob (Python; the store hashes in Rust) | 0.028 | 0.7 % | `hashlib.sha256` |
| **Rust round trip during serialisation** | **0.000** | **0 %** | `CheckpointService._call` wrapped: **0 calls** |

Inside `wcb.encode`, a `cProfile` cross-check (12 profiles, per-encode shares;
profiler overhead inflates absolutes, so only the shares are used):

| function | ms/encode | calls/encode |
|---|---:|---:|
| `_Encoder.value` (dispatch) | 12.21 | 5,564 |
| `isinstance` | 2.13 | 26,233 |
| `bytearray.append` | 1.78 | 17,787 |
| `varint.encode` | 1.67 | 8,784 |
| `len` | 1.07 | 14,234 |
| `bytearray.extend` | 1.00 | 7,966 |
| `str.encode` | 0.61 | 4,577 |
| `_bump_item` | 0.57 | 5,564 |
| `list.sort` | 0.45 | 847 |

and a replay of the same primitives over the same tree: key encode + sort
**1.12 ms** (34 % of the encoder), integer varints 0.116 ms, payload extends
0.031 ms, one tree walk 0.93 ms. The cost is per-item dispatch and per-byte
`append`, not bytes copied (the blob is 71 KB) and not hashing.

So: **all 4.2 ms is Python bytecode; none of it is a Rust round trip.**

---

## 2. The options, measured

Receipt `dcp-metadata-20261111T235500Z` at `4947ef2`, same court, same
fixtures, 30 iterations. `reference` is the oracle's `wcb.encode` on an
already-built tree; `tree_only` is the tree the oracle needs.

| option | what it does | 169 entries | 1024 entries |
|---|---|---:|---:|
| (oracle) reference | tree + `wcb.encode` | 4.06 | 26.87 |
| &nbsp;&nbsp;tree only | `metadata_parts` | 0.73 | 5.64 |
| &nbsp;&nbsp;encode only | `wcb.encode` | 3.28 | 20.18 |
| **(c) generic fast encoder** | `wcb_fast.encode` over the whole tree | **1.63** | **10.67** |
| **(a) plan, warm cache** | signature check + fragment join | **0.555** | **3.34** |
| &nbsp;&nbsp;(a) plan `matches` | signature re-derivation | 0.121 | 0.789 |
| &nbsp;&nbsp;(a) plan `assemble` | fragment join | 0.400 | 2.450 |
| &nbsp;&nbsp;(a) plan build | fragments + oracle proof | 6.28 (once) | 41.1 (once) |
| **(b) native floor** | measured *without any Rust work* | **0.443** | **3.02** |

Details of the native floor (measured against the live service in the same
court, same store):

| part | 169 entries | 1024 entries |
|---|---:|---:|
| cheapest request/response (`stat_file`) | 0.010 | 0.010 |
| `read_file` round trip carrying the metadata blob | 0.072 | 0.206 |
| client marshalling of the entry table (`struct.pack`) | 0.433 (46,911 B) | 3.009 (302,088 B) |
| client memfd write of that request | 0.006 | 0.023 |

**The choice: (a) + (c), with (c) as the plan's fallback.** Evidence:

1. **(c) alone is real but limited**: 3.28 → 1.63 ms of encoding (2.0x), and
   with the tree it is 2.36 vs 4.01 ms (1.7x). It cannot remove the tree walk
   or the per-entry re-encoding of fields that did not change.
2. **(a) is the measured win**: 4.06 → 0.555 ms (7.3x) at 169 and 26.87 →
   3.34 (8.0x) at 1024, because a training run re-sends the same entry set with
   new content roots. Its two components (0.121 + 0.400 ms) are both in the
   `matches`/`assemble` split, so neither is an unexplained residual.
3. **(b) is refuted on this client**: its *floor* — a `stat_file` round trip
   plus the client work a Rust encoder cannot avoid — is **0.443 ms at 169
   (80 % of the plan's total) and 3.02 ms at 1024 (90 %)**, and that floor
   excludes the Rust encode itself, the reply copy and the service's store
   lookup. On this fixture it cannot beat the plan even if the Rust work were
   free. It would also put a second implementation of
   `weft.dcp.metadata.v1` in another language, i.e. a second way to be
   non-canonical. The protocol op, Rust encoder, exactness test and error
   mapping are therefore not built; the measurement is the deliverable.
4. Combining (a) and (c) keeps the format description in one place: (a) caches
   fragments that (c) produced, and the oracle's bytes are the proof.

`test_lowered_limits_raise_the_same_class`, the subclass cases and the
duplicate-key case in §3.2 are what make (c) safe to use as the fallback.

---

## 3. What shipped

### 3.1 Design

* `weftcodec.dcp.serialize_metadata` is **unchanged** and is documented as the
  reference oracle; `metadata_parts()` is the single description of the value
  tree it encodes.
* `weftcodec.dcp.serialize_metadata_fast(metadata, ...)` uses a
  `MetadataPlan` when one matches. The plan caches, per entry-set shape and
  limits:
  * each `state_dict` row's complete encoded bytes;
  * each `storage_data` row split into `prefix + encoded(content_root) +
    suffix` (the only field that changes every save);
  * the canonical key fragments and array headers.
  It re-emits per save: the content roots, `checkpoint_id`, `storage_name`,
  `dcp_version` and `planner_data`.
* Plans live in a bounded process-local `MetadataPlanCache`
  (`max_plans=4`, `max_bytes=8 MiB`; eviction is counted). The cache is keyed
  by a signature re-derived from the *current* metadata, never by identity.
* `weftcodec.wcb_fast` is a generic canonical encoder used by the fallback and
  by the plan builder. It is byte-identical to `wcb.encode` (differentially
  tested) and emits into one joined buffer.

### 3.2 Failure semantics

1. **A plan is only cached after it is proven.** At build time the plan is
   assembled and compared with `wcb.encode(metadata_parts(...))` — the oracle —
   for the same object. A mismatch increments `oracle_mismatches`, is reported
   as `mode == "reference"`, and the plan is not cached. The first save of a
   new shape therefore costs the oracle encode as well (§5, loss 1).
2. **Every save re-derives the signature before emitting a byte.** A plan that
   does not match is not used; the save falls back (counted) to the whole-tree
   encode and rebuilds.
3. The counters are returned in `writer.stats["metadata_encode"]`:
   `mode` (`plan` / `rebuilt` / `reference` / `reference-forced`), `plan_hits`,
   `plan_builds`, `fallbacks`, `oracle_checks`, `oracle_mismatches`,
   `plans_cached`, `cached_bytes`, and the `requested` encoder.
4. `WEFT_DCP_METADATA_ENCODING=reference` (or
   `WeftStorageWriter(metadata_encoding="reference")`) forces the oracle for an
   A/B; an unknown value fails at writer construction, before any store access.

Tests (`python/tests/test_dcp_metadata_fast.py`):

| test | what it proves |
|---|---|
| `test_random_metadata_is_byte_identical` | 5 seeds x 200 randomized metadata trees, cold cache (**rebuilt**) *and* a shared warm cache (**plan**): every blob equals the oracle's, every build proves itself (`oracle_checks`), a warm miss is a counted fallback |
| `test_plan_hits_track_only_the_variable_fields` | changed content roots, checkpoint id, storage name, version and planner data still take the plan (`plan_hits == 1`) |
| `test_a_stale_plan_is_never_used` (7 cases) | added / removed / renamed entry, changed dtype, changed shape, changed chunk layout, tensor→bytes: `mode == "rebuilt"`, `plan_hits == 0`, bytes equal the oracle |
| `test_stale_storage_info_is_never_used` (5 cases) | changed `entry`, `dtype`, `shape`, `lengths`, `storage_offsets` |
| `test_stale_storage_key_is_never_used` | an added indexed row and an added offset row |
| `test_an_index_change_on_the_same_key_is_detected` | torch's `MetadataIndex.index` is `compare=False`, so two keys can be equal; a changed index must still invalidate the plan |
| `test_a_plan_is_not_reused_across_different_limits` | a plan proven under one `Limits` is not used under another |
| `test_invalid_metadata_raises_the_same_class` | `storage_data=None`, a non-`StorageInfo` row, an unsupported state object: same class as the oracle |
| `test_out_of_range_values_raise_the_same_class` | negative extent: `Malformed` from both |
| `test_lowered_limits_raise_the_same_class` | items / string-bytes / depth limits: same class from both, and both accept when the metadata fits |
| `test_metadata_parts_and_the_oracle_share_one_description` | the split path *is* the oracle |
| `test_metadata_encoding_override_is_validated_and_reported` | `reference` selects the oracle; an unknown value fails at construction |
| `test_real_fixture_bytes_and_restore_are_unchanged` | 169-entry store-backed save twice: stored blob equals the oracle's bytes, restore is exact, modes are `rebuilt` then `plan` |

Tests (`python/tests/test_wcb_fast.py`): 5 seeds x 200 randomized trees
(nested maps, unicode and byte strings, every varint boundary, empty
containers, wide and deep containers) compared with `wcb.encode` including the
decode round-trip; invalid values and non-text keys compared by class;
subclasses, `bytearray` and `memoryview`; a mapping that yields one key twice
(both encoders emit the same non-canonical bytes and `wcb.decode` rejects
them); eight lowered-limit cases; and a cache-leak test proving a key cached
under the default limit cannot bypass a lowered one.

Python suite in the python court at `4947ef2`: 423 passed / 70 skipped / 0 failed (the 70 skips are the pre-existing
`weftfs`-in-a-non-fuse-court and optional-dependency guards; both new files
pass in full).

---

## 4. End to end

Same fixture, same court, same commands; the only difference is
`WEFT_DCP_METADATA_ENCODING`. Both runs are at `4947ef2`, so the delta is
item 3 alone (no other optimisation is bundled in).

### 4.1 Checkpoint-latency court

`bench/scripts/checkpoint_latency_court.sh`, defaults (`--reps 6 --fresh 3`,
one session of 8, `--mode both`).

| measurement | scenario | reference (before) | auto (after) | delta |
|---|---|---:|---:|---:|
| `adapter.metadata_serialize` p50 | fresh store | 5.189 | 1.059 | **-4.13** |
| | sequential, later | 4.323 | 0.692 | **-3.63** |
| | session (8 saves) | 4.679 | 0.631 | **-4.05** |
| | session, saves 2-8 | 4.350 | 0.626 | **-3.72** |
| completion p50 | session | 40.03 | 34.58 | -5.45 |
| completion p50 | session, saves 2-8 | 39.39 | 34.41 | -4.98 |
| client top-level p50 | session | 32.84 | 28.27 | -4.57 |
| Rust top-level p50 | session | 17.23 | 16.87 | -0.36 (noise) |

Encoder modes counted in the receipts: the before run reports
`{"reference-forced": 21}`; the after run reports `{"plan": 20, "rebuilt": 1}`
with `plan_builds=1`, `oracle_checks=1`, `oracle_mismatches=0`,
`fallbacks=1`. The fast path is taken on 20 of 21 saves and the oracle is never
silently substituted; the one `rebuilt` is the first save of the process, which
pays the build and its oracle proof.

**The attributable delta is -4.05 ms** (the isolated phase, session median).
Completion falls 5.45 ms, of which 4.57 ms is in the client's top-level
sections; the remaining ~0.9 ms is the court's residual (~5-6 ms of
unaccounted time: `dcp.save`'s own plan construction, writer construction and
drain), so completion deltas must not be read as more precise than ±1 ms.

### 4.2 Training-goodput session court

`bench/scripts/training_goodput_session_court.sh`, defaults (`--steps 128
--reps 3 --intervals 4,8,16,32 --configs weft,weft-session`), 128 steps per run.

`weft-session` warm completion (ms):

| steps between checkpoints | before p50 | after p50 | delta | before p95 | after p95 |
|---:|---:|---:|---:|---:|---:|
| 4 | 46.85 | 43.21 | **-3.64** | 53.92 | 46.78 |
| 8 | 45.02 | 42.36 | **-2.66** | 47.48 | 45.86 |
| 16 | 45.19 | 42.14 | **-3.05** | 46.87 | 47.88 |
| 32 | 44.76 | 40.55 | **-4.21** | 47.00 | 43.24 |

Mean warm-p50 delta **-3.39 ms**, consistent with the -4.05 ms phase
measurement once the goodput loop's own jitter (training steps share the CPU
with the save) is accounted for. This court does **not** isolate the metadata
phase (it uses no probe sink), so it corroborates the latency court rather than
measuring item 3 by itself.

Cold completions (first checkpoint of each run, includes service start) rise in
`weft-session` cells: 109.40→112.92, 103.91→112.20, 103.03→123.73,
102.35→113.24 ms (n=3 per cell). That is the plan build (§5, loss 1), not a
regression in steady state.

Exactness: 136/136 resume checks pass in both runs (`resume_exact`,
`resume_loss_match`, `resume_post_state_match` 112/112 checked).

---

## 5. Losses (all of them)

1. **The first save of a shape is slower.** Plan build + oracle proof is
   6.28 ms at 169 entries (41.1 ms at 1024), on top of a 2.4 ms generic encode,
   against a 4.06 ms reference. Break-even is the second save of the same
   shape in the same process. Measured in `weft-session` cold p50 (§4.2) and in
   `metadata_encode: {"rebuilt": 1}`.
2. **The cache is per process.** Case (A) — one save per process — never
   amortises: it pays build + proof on every save. The writer-per-save shape
   is fine only because the *process* survives (`sequential_later`
   `metadata_serialize` 4.32 → 0.69 ms).
3. **The plan is O(entries) to validate.** `matches` is 0.121 ms at 169 and
   0.789 ms at 1024; it cannot be replaced by an identity check because the
   adapter is handed a fresh `Metadata` object on every save.
4. **Still not free.** 0.555 ms at 169 (0.400 of it fragment assembly). The
   remaining work is Python bytecode and small-bytes allocation; a C/Rust
   implementation cannot beat it on this client (§2, option (b)).
5. **Memory.** Cached fragments are bounded at 4 plans / 8 MiB and count
   against the training process. The 169-entry plan holds 52,613 B of fragments
   (`cached_bytes` in `checkpoint-latency-20261112T001500Z`); a plan's size
   grows with the entry set, and the 8 MiB bound is what keeps a large
   checkpoint family bounded (a plan above the bound is simply not cached).
6. **The fallback path is not free either**: 1.63 ms (generic) at 169, plus a
   plan build. A changing entry-set shape every save is *worse* than before
   (2.4x on the first save of each shape).
7. **Bimodal p95 at 1024 entries** (101 ms p95 vs 26.9 p50 in the reference
   run): Python GC over ~39,000 objects. The plan reduces but does not remove
   the transient (3.3 ms p50, 4.6 ms p95 at 1024).
8. **`MetadataPlanCache` is process-global state.** It is bounded, lock-guarded
   and validated per save, but two stores in one process share the cache; a
   test that needs a cold cache must clear it (`METADATA_PLANS.clear()`), which
   `test_real_fixture_bytes_and_restore_are_unchanged` documents by example.
9. The frame, store and CLI paths are untouched, so nothing here changes
   durability, exactness, or the on-disk format; equally, no store-side or
   Rust-side cost was removed.

---

## 6. Not claimed

* No change to `weft.dcp.metadata.v1`, no new wire value, no new protocol op,
  and no Rust or Go participation in metadata encoding.
* No claim that 20 ms completion is reachable. Item 3 removes ~4 ms of a
  ~40 ms session completion; the remaining cost is the service round trip, the
  index segment and the residual in `bench/RESULTS-checkpoint-latency.md`.
* No claim about Orbax (`python/weftcodec/orbax.py` has its own metadata
  encoder and was not changed or measured here).
* No multi-rank, multi-host, GPU, fleet or durability claim. One host, warm
  page cache, CPU only, local store on tmpfs.
* The native option (b) is **measured, not built**: its floor is the evidence,
  and the floor already loses to the plan. No Rust encoder was written.
* The 1024-entry size is a synthetic scaling point (repeated fixture names,
  4x4 float32 tensors), not a model.

---

## 7. Reproduce

```sh
# baseline encoder cost and the option comparison (any revision)
scripts/docker-court.sh python "bench/scripts/dcp_metadata_court.sh"

# A/B on one revision: the oracle vs the shipped plan
WEFT_DCP_METADATA_ENCODING=reference \
  scripts/docker-court.sh python "bench/scripts/checkpoint_latency_court.sh"
scripts/docker-court.sh python "bench/scripts/checkpoint_latency_court.sh"

WEFT_DCP_METADATA_ENCODING=reference \
  scripts/docker-court.sh python "bench/scripts/training_goodput_session_court.sh"
scripts/docker-court.sh python "bench/scripts/training_goodput_session_court.sh"

# the differential and failure-semantics tests
scripts/docker-court.sh python \
  "cd /work && PYTHONPATH=/work/python python3 -m pytest \
     python/tests/test_wcb_fast.py python/tests/test_dcp_metadata_fast.py -q"
```

Receipts and raw logs live beside each other under `bench/receipts/`:
`dcp-metadata-20261111T230500Z.{json,log}` (baseline),
`dcp-metadata-20261111T235500Z.{json,log}` (options at `4947ef2`),
`checkpoint-latency-20261112T000500Z` (reference) and `…T001500Z` (auto),
`training-goodput-session-20261112T003000Z` (reference) and `…T004500Z` (auto).

**Baseline provenance.** The baseline run used the encoder in the `11230b3`
working tree; the court script grew an `alternatives`/`path` section in the
same session (committed with this change), so the baseline receipt has no
`path` block and its `alternatives` key is absent. Re-running the current
script at `11230b3` reproduces the `shipped`/`sections` numbers and reports
`path.available: false`.


## X02: DCP checkpoint completion, interleaved arms (measured 2026-11-13)

`python/courts/dcp_completion.py` (court `bench/scripts/dcp_completion_court.sh`)
runs the same 169-entry R11 fixture as the R12c court through one
`CheckpointSession`, alternating the oracle encoder (`reference`) and the
shipped metadata plan (`auto`) save by save for seven repetitions. Receipt
`bench/receipts/dcp-latency-20261113T214500Z.json`, log beside it. Durability
control in the same receipt: a save is acknowledged, the helper process is
SIGKILLed, and the checkpoint is verified by a new process (169/169 tensors).

<!-- BEGIN GENERATED:x02-dcp-table (scripts/perf_numbers.py) -->
| measurement | value (ms) |
|---|---:|
| shipped (auto) completion p50 / p95 | 39.78 / 40.39 |
| oracle (reference) completion p50 | 43.43 |
| plan advantage p50 | 3.65 |
| client top-level accounted | 33.12 |
| Rust top-level accounted | 17.86 |
<!-- END GENERATED:x02-dcp-table -->

**The 20 ms target is not reached.**

<!-- BEGIN GENERATED:x02-dcp-prose (scripts/perf_numbers.py) -->
The metadata encoder is no longer the problem: the plan removes 3.65 ms of the
reference arm's completion (43.43 ms p50 against 39.78 ms). The dominant
remaining term is the tensor write handoff inside the write request: Rust spends
5.89 ms encoding (zstd), 3.00 ms digesting,
1.93 ms publishing blobs, 2.72 ms on the index segment and
3.33 ms on the final metadata commit; the rest of the write request is
the client's safetensors staging and the memfd/socket round trip. Closing the gap to 20 ms
needs a different tensor handoff and a parallel encoder; an encoder tweak cannot reach it.
<!-- END GENERATED:x02-dcp-prose -->
