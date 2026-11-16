# X18 — direct-to-destination verified reconstruction

Receipt: `bench/receipts/direct-restore-20261115T235925Z.json` (stamp
`20261115T235925Z`, WeftFS revision `4ab9ed2`). Raw log beside it. Tests:
[`tests/direct_restore.rs`](../tests/direct_restore.rs).

## 1. What was built

The existing restore path decodes a checkpoint into one contiguous container
buffer and then hashes it with SHA-256. That serial whole-file digest is a floor:
it costs one pass over every restored byte, in one thread, after the decode has
already finished.

X18 adds a path that decodes **selected shards or tiles straight into
caller-owned rank coordinates**, verifying at the unit that carries a root:

* `Store::restore_checkpoint_direct_into` fills a set of per-tensor destination
  buffers in canonical container order.
* `Store::restore_row_slab_direct_at` fills one rank's row slab of every tensor
  at a pinned snapshot.
* Every decoded tile is checked against its normative `tile_root`, and the frame
  blob is identified by its manifest id (`Store::tensor_frame_id`). The whole-file
  SHA-256 pass is not performed; `file_sha256` in the report is zeroed and the
  container header hash is kept separately as `header_sha256`.
* Buffers that are missing, duplicated or the wrong length are refused **before**
  any byte is read.

This applies to the tiled representation, which carries per-tile roots. A
non-tiled frame has only a whole-tensor root, so it cannot be verified
incrementally and keeps the existing path.

## 2. Method

Harness `examples/direct_restore_court.rs`, court
`scripts/direct_restore_court.sh`, rust court. Fixture: one tiled checkpoint of
8 tensors, 2,048 × 2,048 BF16 each (67,108,864 logical bytes), tile shape
[128, 512], 4 shards, written once and read-only for the measurement. Five
repetitions per arm, medians reported.

| arm | what it does |
|---|---|
| `container_then_copy` | existing restore into one container + copy into per-tensor buffers |
| `container_unhashed` | the same restore with the whole-file SHA-256 pass skipped (isolates the floor) |
| `direct_into_destinations` | the new direct path |
| `rank_ordinary` | one rank's rows restored the ordinary way |
| `rank_direct_slab` | the same rows through the direct slab path |

The SHA-256 floor is `container_then_copy − container_unhashed`; the direct path
is compared against both, so the delta attributed to "no container" and the delta
attributed to "no hash" are separable. RSS is `VmHWM`, which is monotone inside
one process; it is reported per arm in run order and is not an isolated peak.

## 3. Results

<!-- BEGIN GENERATED:direct-restore-table (scripts/perf_numbers.py) -->
| arm | wall p50 ms | CPU p50 ms | VmHWM delta KiB |
|---|---:|---:|---:|
| container then copy | 260.7 | 260 | 76,520 |
| container, unhashed | 204.3 | 202 | 0 |
| direct into destinations | 183.8 | 183 | 10,320 |
| rank ordinary | 48.4 | 48 | 0 |
| rank direct slab | 41.5 | 41 | 0 |
<!-- END GENERATED:direct-restore-table -->

<!-- BEGIN GENERATED:direct-restore-prose (scripts/perf_numbers.py) -->
The court ran 3 of 3 checks and all passed. The whole-file SHA-256 pass costs 56.4 ms (260.7 → 204.3 ms). The direct path removes both the container copy and the hash: 183.8 ms, 76.9 ms below the copying path and 20.5 ms below the same restore without the hash; CPU tracks wall. The rank slab gains 7.0 ms. Peak-RSS delta: container 76,520 KiB, unhashed 0 KiB, direct 10,320 KiB.
<!-- END GENERATED:direct-restore-prose -->

### 3.1 Equivalence and corruption refusal

Proved by `tests/direct_restore.rs`, and re-checked by the court's three checks:

* every destination byte equals the tensor that was saved, for all 8 tensors;
* the direct row slab equals the ordinary rank restore **and** the saved rows;
* the direct path and the container path agree on every byte (header included).

A corrupted source region is refused, not repaired:

* a flipped byte in the frame the target shard names is `Corrupt` and the
  destination buffer is left untouched;
* a missing or mis-sized destination buffer is refused before any read;
* a duplicated tensor in the destination set is refused.

## 4. Losses

1. **Peak RSS still grows.** The caller owns the destination buffers, so the
   direct path's *delta* is small but the buffers are live: on this fixture the
   direct arm's `VmHWM` delta is larger than the unhashed container arm's. The
   direct path removes the container copy, not the destination itself.
2. **The rank slab is only modestly faster.** Decoding the intersecting tiles
   dominates, so the slab arm gains a few milliseconds, not a factor. The direct
   path is a *verification* win first and a time win second.
3. **Tile roots are not a whole-file digest.** A verifier that requires the
   shard's SHA-256 still has to use the container path or check the checkpoint
   manifest; `file_sha256` is deliberately empty in the direct report so nobody
   mistakes it for a computed digest.
4. **Tiled shards only.** A non-tiled frame, an overlay object, or a tensor
   written without a tile shape keeps the existing whole-file path.
5. **No parallel workers here.** The court measures a single-threaded direct
   restore; the shipped pipelined restore has its own worker bounds and is not
   composed with the direct destinations in this court.
6. **Rust only, one host, warm page cache.** No Go parity, no network backend, no
   fleet or media claim.

## 5. Non-claims

* No claim that the direct path is always faster: it trades a serial hash for
  per-tile verification, and on a tensor with few large tiles the hash is cheap
  relative to decode.
* No claim about end-to-end training-step time; the court times a restore call in
  isolation.
* No change to any frozen byte, root, mode number or checkpoint manifest field.
