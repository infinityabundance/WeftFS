# Selector-index trust (WFX2 + commit attestation)

Status: implemented and enforced in Rust and Go. One additive commit-schema
field (`weftfs.commit.v2`). The index segment layout is unchanged.

## The problem

A WFX2 segment's trailing BLAKE3 digest proves that the bytes have not changed
*by accident* since they were written. It does **not** prove that the segment is
a complete, correct derivative of the authoritative state. An attacker who can
write the index namespace (untrusted storage, a weaker replica, a compromised
cache volume) can:

1. decode an honest segment,
2. remove candidates from any section,
3. leave every header field (commit, generation, object/relation/entry counts)
   untouched,
4. recompute the trailing digest,

and produce a **self-consistent subset**. Every check the loader performed
before this change would pass, and a selector would silently under-select.

## The trust chain

An index segment is loaded only when **all** of the following hold:

| # | check | what it proves |
| --- | --- | --- |
| 1 | WFX2 magic/version, bounds, canonical layout, trailing digest | the bytes are an intact WFX2 segment |
| 2 | segment `commit` == the snapshot key it is stored under | the segment names this state |
| 3 | segment `generation` and all three counts == the replayed state | it describes this state's shape |
| 4 | the **commit record** at that generation re-hashes to that commit **and** carries an `index_attestation` | the authoritative chain attests an index for this commit |
| 5 | the segment's counts == the attestation's counts | it is not a different-shaped index |
| 6 | `H("weft/index/candidates/v1", section bytes)` == `attestation.candidates_root` | the candidate sets are exactly the attested ones |
| 7 | `H("weft/index/state/v1", six-field v1 WCB)` recomputed from the commit == `attestation.state_root` | the attestation itself belongs to this commit's fields |

Any failure is a diagnostic note (`IndexStats::rejects`, bounded notes naming the
segment) and the index is **rebuilt from replayed state**. It is never used to
narrow candidates. The index is advisory: a missing, rejected, or unwritable
segment never affects commit durability or `verify`.

Check 6 is the one that closes the reported gap. Checks 4 and 7 are what make 6
authoritative rather than self-referential.

## The commit field

`weftfs.commit.v2` is the v1 key set plus `index_attestation`:

```
{
  "schema": "weftfs.commit.v2",
  "generation": u64,
  "parent": bytes32 | null,
  "timestamp_ns": timestamp,
  "ops": [ ... ],
  "checkpoint": bytes32 | null,
  "index_attestation": {
    "object_count": u64,
    "relation_count": u64,
    "entry_count": u64,
    "state_root": bytes32,        // H("weft/index/state/v1", six-field v1 WCB)
    "candidates_root": bytes32    // H("weft/index/candidates/v1", WFX2 sections)
  }
}
```

The writer derives the index *before* publishing. It takes the previous
head's index from the in-memory attested cache (validated against the
published commit record: counts, chain kind/base/depth/root, and the
`candidates_root` recorded when that index was written), applies only the rows
the committed ops changed, and records the element-level changes. `encode_delta`
only re-states those rows; a unit test proves its bytes equal the whole-state
diff's. `StateIndex::build` is the **oracle and the fallback**: a cold start, a
missing or inconsistent base, a missing pre-image, or an op the base cannot
explain rebuilds the whole index and publishes a full segment
(`maintenance_fallbacks`). The writer then derives `candidates_root` from the
exact section bytes it will write and `state_root` from the commit fields. The
segment is written after the commit publishes and reuses the same in-memory
index (no second build). If the index is incomplete it is not persisted and the
commit carries no attestation (v1).

Only the in-memory cache skips re-deriving `candidates_root`; every read path
still recomputes it from the bytes it loaded, and the disk-based writer
fallback recomputes it when resolving a chain. See
`bench/RESULTS-incremental-index.md` for the design, the reader-merge bound
(`index_chain_max_deltas` deltas + one full ancestor) and the measurements.

## Migration note

* `weftfs.commit.v1` remains valid, readable, and writable. It carries no
  attestation, so **every index segment named by a v1 commit is rejected and
  rebuilt** — old stores keep working and simply do not use persisted indexes
  until they publish a v2 commit.
* The commit identity domain is unchanged (`H("weft/store/commit/v1", WCB)`),
  but the bytes differ, so the same logical commit has a different id under v2.
  A v2 store is a different commit chain from a v1 store; there is no in-place
  conversion. Replaying a v1 store and writing new commits continues its chain
  (new commits are v2, old ones stay v1). Mixed schemas in one chain are legal
  and both loaders handle them.
* A reader that checks the exact commit key set must accept
  `weftfs.commit.v2`. Both implementations here do; the frozen conformance
  fixture stays v1, reproduced by `Options::attest_indexes = false`
  (`AttestIndexes` in Go), which writes no index segment either.
* The segment format itself did **not** change: WFX2 v1, same bytes, same
  digest domain. No index migration is required.

## Evidence

* `tests/index_persistence.rs::forged_self_consistent_subset_is_rejected_by_the_commit_attestation`
  (Rust): remove one candidate from a valid segment, keep every header count,
  recompute the digest, reopen → `rejects >= 1`, `disk_loads == 0`,
  `builds >= 1`, and the query answers equal the scan oracle.
* `WeftFS-Go/store/index_persistence_test.go::TestForgedSubsetIndexIsRejectedByCommitAttestation`
  (Go): the same forge on a Go-written segment, same result.
* `WeftFS-Go/store/index_handoff_test.go` (both languages): a Go-written
  segment is *loaded* by Rust (`index.loads > 0, builds == 0`); a forged subset
  is rejected by Rust and the true answers survive; Rust-written segments load
  in Go with `DiskLoads=1, Builds=0` and byte-identical re-encoding.
* All other `index_persistence` tests (honest load, scan matrix, corruption,
  new-head keying, GC, read-only never writes) still pass.

## Residual gap (not claimed)

* **An attacker who can rewrite the commit record** is a different threat class.
  The attestation is only as authoritative as the commit chain and the writer
  lock/lease that publish it; a party that can forge commits is already outside
  the single-writer model this project proves. Protection there needs an
  authenticated commit chain (signing / an external anchor), which is not
  implemented.
* The index is still **advisory**: correctness never depends on it. A forged
  segment can at worst cost a rebuild.
* `candidates_root` is computed by re-encoding the decoded index, so it proves
  set equality under the canonical encoder, not byte equality with what a
  writer originally emitted. That is the right claim (both languages re-encode
  to identical bytes, proven by the handoff tests).
* No on-disk index version beyond WFX2, no persisted attestation cache, and no
  Go index *writer* parity claim beyond the existing WFX2 writer (which the
  handoff tests exercise).
