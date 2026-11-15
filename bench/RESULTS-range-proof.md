# T073 — portable authenticated range proofs

Receipt: `bench/receipts/range-proof-20261115T112319Z.json` (stamp
`20261115T112319Z`, WeftFS revision `63ad66c`). Raw log beside it. Normative
record definition: [`spec/WEFT-RANGE-PROOF.md`](../spec/WEFT-RANGE-PROOF.md).

## 1. What was built

A `weft.range-proof.v1` record carries the authentication path over the frozen
Merkle leaf/node structure for exactly the chunks a declared canonical region
intersects. No parallel tree, no new leaf or node domain, no new on-disk
structure: the sibling hashes are derived from the chunk table the manifest
already carries, and the verifier re-derives each level's width from
`chunk_count` alone, so a skipped, extra or wrongly-sided node is a rejection.

A `weft.range-anchor.v1` record binds a checkpoint tensor/shard entry to the
**Merkle root of that shard's chunk table** and is signed with Ed25519. It
exists for one reason, stated plainly in the spec: a `weft.checkpoint.v1` shard
entry records a *whole-file* digest, so a chunk-table root is not authenticated
by the checkpoint alone. The anchor is the smallest additive change that
supplies one, and the verifier separately recomputes the checkpoint manifest
root from the manifest's canonical bytes so an anchor cannot be replayed
against another checkpoint, tensor or shard.

The proof is checked by `verifier/range_proof_check.py`, a separate program
built on the repository's clean-room BLAKE3, WCB and Ed25519 and **sharing no
code with WeftFS**. It re-reads chunk bytes and the shard manifest from the
store directory, or verifies from the proof plus the chunk bytes alone.

## 2. Method

Harness `examples/range_proof_court.rs`, court `scripts/range_proof_court.sh`,
run in the FUSE court (the independent verifier is Python). Five cases:

| case | tensor bytes | chunk | shards |
|---|---:|---:|---:|
| `nano-16k-c4k` | 16 KiB | 4 KiB | 1 |
| `tiny-64k-c4k` | 64 KiB | 4 KiB | 1 |
| `mid-4m-c4k` | 4 MiB | 4 KiB | 1 |
| `mid-32m-c64k` | 32 MiB | 64 KiB | 1 |
| `wide-8m-c64k-s8` | 8 MiB | 64 KiB | 8 (axis 0) |

Each case writes a **raw** (chunk-table) checkpoint, one signed anchor per
shard, and proves regions at an offset deliberately unaligned to the chunk
grid. `full_tensor_ns` is the same store reading and verifying the whole
covered object — the strategy a verifier uses today. Rows are in-process Rust
medians of 7 reps; the separate Python process is timed from inside the
verifier. Nothing is a backend benchmark: the comparison is between two
verification strategies on the same bytes.

## 3. Results

### 3.1 Proof size and verifier cost

<!-- BEGIN GENERATED:range-proof-table (scripts/perf_numbers.py) -->
<!-- END GENERATED:range-proof-table -->

Read it as: at 64 KiB chunks the proof beats whole-object verification by an
order of magnitude for regions up to 1 MiB, and the advantage shrinks to about
3.8x at 8 MiB. At 4 KiB chunks on a 4 MiB tensor the advantage is 7.9x for a
4 KiB region and **0.92x for a region that is the whole object**.

### 3.2 The independent verifier

<!-- BEGIN GENERATED:range-proof-components (scripts/perf_numbers.py) -->
<!-- END GENERATED:range-proof-components -->

The independent verifier's cost is dominated by its pure-Python Ed25519 check
and its pure-Python BLAKE3, not by the Merkle arithmetic. It is a *correctness*
oracle, not a performance path. Its per-row times are in the receipt and are
1,000x slower than the Rust verifier on the same proof.

## 4. Losses

1. **Full hashing wins when the region is the whole covered object.** 0.69x on
   `tiny-64k-c4k`, 0.92x on `mid-4m-c4k` at the whole 4 MiB object, 0.83x on a
   whole 1 MiB shard of `wide-8m-c64k-s8`.
2. **Full hashing wins on a very small object at every region size.** On the
   16 KiB tensor the proof takes 0.40–0.51x of the whole-object cost: the
   proof's fixed cost (~60–80 µs: record decode, anchor check, path fold) is
   larger than hashing 16 KiB. The break-even is roughly a 32–64 KiB covered
   object on this host.
3. **The proof is not small for large regions.** 634,564 B for all 1,024 leaves
   of a 4 MiB object at 4 KiB chunks: the leaf list plus one path per leaf costs
   about as much as the region. A contiguous region already proving every chunk
   gains nothing from the path form and should be verified by table fold
   instead.
4. **Frame-backed shards have no proof.** A mode-9/10/15/17 frame and a
   `weft.manifest.v4` overlay carry no chunk table, so `range-proof anchor`
   refuses them and reports why. Today's checkpoint writer produces frames by
   default; a range proof therefore requires `--shard-strategy raw`.
5. **One shard per proof, axis 0 only.** A region that is not contained in one
   shard, and any request on an axis != 0 multi-shard tensor, is refused rather
   than reinterpreted.
6. **The anchor is the trust root.** An unsigned anchor accepted without a
   trust set proves internal consistency, not authorship. The receipt records
   which of the two happened.
7. The independent verifier reads the shard manifest only when given
   `--store`; in portable mode it reports `manifest_rechecked=false` and claims
   nothing about the dependency.
8. One host, warm cache, one run of seven reps per cell.

## 5. Non-claims

* No new on-disk tree, no change to any frozen byte, mode or root.
* No Go implementation of the records or the verifier.
* Not a privacy mechanism: the proof reveals the region, the intersecting chunk
  hashes and the path.
* No claim that this is the only way to prove a range; a per-level sibling
  table stored on disk would make the proof O(log n) with no leaf list, at the
  cost of a new on-disk structure, and is not implemented.
