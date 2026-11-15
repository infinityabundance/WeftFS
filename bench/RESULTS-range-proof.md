# T073 — portable authenticated range proofs

Receipt: `bench/receipts/range-proof-20261115T114737Z.json` (stamp
`20261115T114737Z`, WeftFS revision `63ad66c`). Raw log beside it. Normative
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
| case | chunk | region | leaves | proof B | chunk B | verify ns | store ns | full ns | adv |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nano-16k-c4k-64 | 4,096 | 64 | 1 | 504 | 4,096 | 66,124 | 67,616 | 32,260 | 0.49 |
| nano-16k-c4k-16384 | 4,096 | 16,384 | 4 | 1,047 | 16,384 | 80,561 | 98,835 | 32,260 | 0.40 |
| tiny-64k-c4k-64 | 4,096 | 64 | 1 | 614 | 4,096 | 73,418 | 76,974 | 98,404 | 1.34 |
| tiny-64k-c4k-4096 | 4,096 | 4,096 | 2 | 906 | 8,192 | 79,849 | 88,936 | 98,404 | 1.23 |
| tiny-64k-c4k-65536 | 4,096 | 65,536 | 16 | 4,975 | 65,536 | 145,903 | 222,286 | 98,404 | 0.67 |
| mid-4m-c4k-64 | 4,096 | 64 | 1 | 946 | 4,096 | 726,578 | 733,431 | 5,749,491 | 7.91 |
| mid-4m-c4k-4096 | 4,096 | 4,096 | 2 | 1,568 | 8,192 | 738,651 | 746,165 | 5,749,491 | 7.78 |
| mid-4m-c4k-65536 | 4,096 | 65,536 | 17 | 10,880 | 69,632 | 829,340 | 910,693 | 5,749,491 | 6.93 |
| mid-4m-c4k-1048576 | 4,096 | 1,048,576 | 257 | 159,539 | 1,052,672 | 2,191,408 | 3,451,905 | 5,749,491 | 2.62 |
| mid-4m-c4k-4194304 | 4,096 | 4,194,304 | 1,024 | 634,564 | 4,194,304 | 6,468,827 | 11,519,381 | 5,749,491 | 0.89 |
| mid-32m-c64k-4096 | 65,536 | 4,096 | 1 | 895 | 65,536 | 417,631 | 429,123 | 11,207,056 | 26.83 |
| mid-32m-c64k-65536 | 65,536 | 65,536 | 2 | 1,464 | 131,072 | 442,558 | 464,759 | 11,207,056 | 25.32 |
| mid-32m-c64k-1048576 | 65,536 | 1,048,576 | 17 | 9,967 | 1,114,112 | 756,906 | 957,530 | 11,207,056 | 14.81 |
| mid-32m-c64k-8388608 | 65,536 | 8,388,608 | 129 | 73,380 | 8,454,144 | 2,908,177 | 4,440,043 | 11,207,056 | 3.85 |
| wide-8m-c64k-s8-4096 | 65,536 | 4,096 | 1 | 618 | 65,536 | 104,766 | 115,576 | 331,470 | 3.16 |
| wide-8m-c64k-s8-1048576 | 65,536 | 1,048,576 | 16 | 4,995 | 1,048,576 | 394,969 | 583,882 | 331,470 | 0.84 |
<!-- END GENERATED:range-proof-table -->

A ratio above 1.00 means the proof is cheaper than whole-object verification.

<!-- BEGIN GENERATED:range-proof-summary (scripts/perf_numbers.py) -->
Breaking the table down: the proof costs **0.49x** the whole-object check on a 16 KiB object, **0.67x** for the whole 64 KiB object, **0.89x** for the whole 4 MiB object and **0.84x** for a whole 1 MiB shard (below 1.00 means whole-object verification is cheaper). Above 1.00 the proof wins: **7.78x** for a 4 KiB region of the 4 MiB object and **3.85x** for an 8 MiB region of the 32 MiB object.
<!-- END GENERATED:range-proof-summary -->

The break-even is the covered object's size, not the region's: a proof's fixed
cost is a record decode, an anchor check and one path fold, and whole-object
verification costs the object's bytes at the store's read rate. Below roughly a
few tens of KiB of covered object the hash wins at every region size; above
that the proof wins until the region covers the whole object.

### 3.2 The independent verifier

<!-- BEGIN GENERATED:range-proof-components (scripts/perf_numbers.py) -->
The independent verifier pays **2,111,128 ns** for one Ed25519 verification and **2,756,654 ns** for one 4 KiB BLAKE3, both in pure Python (medians of five on the court host).
<!-- END GENERATED:range-proof-components -->

The independent verifier's cost is dominated by its pure-Python Ed25519 check
and its pure-Python BLAKE3, not by the Merkle arithmetic. It is a *correctness*
oracle, not a performance path; per-row times are in the receipt and are orders
of magnitude above the Rust verifier on the same proof.

## 4. Losses

1. **Full hashing wins when the region is the whole covered object.** Every
   whole-object row above 1.00's mirror image is below 1.00: the row set in
   §3.1 shows it for the 64 KiB object, the 4 MiB object and a 1 MiB shard.
2. **Full hashing wins on a very small object at every region size.** On the
   16 KiB tensor the proof is more expensive than hashing the object, at every
   region size in the table. The break-even is a covered object of a few tens of
   KiB on this host.
3. **The proof is not small for large regions.** A region covering every chunk
   gets one leaf and one path per chunk, so the proof costs about as much as the
   region itself (see the whole-object row of the 4 MiB case). A contiguous
   region that already covers the table gains nothing from the path form and
   should be verified by a table fold instead.
4. **Frame-backed shards have no proof.** A mode-9/10/15/17 frame and a
   `weft.manifest.v4` overlay carry no chunk table, so `range-proof anchor`
   refuses them with `UnsupportedRepresentation` and reports why. Today's checkpoint writer produces frames by
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
