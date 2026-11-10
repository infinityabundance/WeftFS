# Incremental advisory index segments (WFX3)

Status: implemented in R9 (Rust `src/index_chain.rs`, Go `index/chain.go`).
The index remains **advisory**: correctness never depends on it, and every
inconsistency fails closed to a rebuild from replayed state.

## The measured defect

A WFX2 segment is a **full-state** snapshot of the four candidate maps. The
trajectory court writes one commit per tensor (the synthetic 50-step series) and
every commit carries a full 1.06 MB segment. The advisory directory reached
**3,466,466,150 B for a 74 MB data tree** (3,271 segments), reported in
`bench/RESULTS-gc-accounting.md`. GC can drop unreferenced segments, but the
live commit chain itself still names every segment, so the amplification is real
storage, not a counter artifact.

The fix is to stop rewriting unchanged state. A segment may now be:

* **full** — the complete four maps (genesis, a periodic checkpoint, or a
  compaction); or
* **delta** — a base segment id plus the changed index rows only.

A read loads the full ancestor and applies the deltas in order. The resolved
index must equal what a full segment would have contained.

## WFX3 layout

Little-endian, internal, rebuildable. `WFX3` is a new magic; WFX2 full segments
stay readable and are never reinterpreted as deltas.

```
magic "WFX3" (4)
version u32 = 1
kind u8                      0 = full, 1 = delta
generation u64               the commit's generation
base_generation u64          0 for full
base_id [32]                 zero for full; immediate base segment id for delta
base_commit [32]             zero for full; commit the base is stored under
object_count u64             resolved counts (not delta counts)
relation_count u64
entry_count u64
section objects_with_attr    changed/full rows
section objects_by_attr_eq
section relations_from
section tags
digest [32]                  BLAKE3("weft/index/segment/v3", all preceding bytes)
```

A **row** is one map key and its value. The flag byte selects one of three
forms:

* `0` **absent** — the row was removed (tombstone);
* `1` **present** — the full value (used in a full segment and for a key the
  base does not have);
* `2` **patch** — an element-level delta against the base row:
  `u32 added count, added elements, u32 removed count, removed elements`.

Row keys:

* `objects_with_attr`: `str name`, flag, then ids;
* `objects_by_attr_eq`: `str name`, `bytes key`, flag, then ids;
* `relations_from`: `hash32 subject`, `str predicate`, flag, then target edges;
* `tags`: `str tag`, flag, then ids.

A full segment writes every row with flag `1`. A delta writes only keys whose
value differs from the base. Using element-level patches rather than rewriting
whole sets is essential: an attribute such as `weft.tensor.dtype` has a set of
every tensor object, so a row-level delta would re-emit the whole growing set
on every append (measured: 97,066,019 B for 1,250 commits at row level). With
patches the same trajectory is 3,342,744 B. A patch is validated strictly: every
removal must name a present element, every addition an absent one, and no patch
may empty a row (an empty row is written as `absent`). Rows are sorted by their
canonical key and elements within a row are sorted, so the encoding is
deterministic and a load is a sequential walk plus map insertion. Duplicate keys
are rejected.

`segment_id = BLAKE3("weft/index/segment/v3", file bytes without the trailing
digest)`. The segment intentionally carries no commit hash: the commit's
attestation names the segment, so the segment can be hashed before the commit is
written (the circularity that would otherwise exist between segment bytes and
commit id).

## Commit attestation

`weftfs.commit.v3` is `weftfs.commit.v2` plus four fields inside
`index_attestation`:

```
segment_kind:  "full" | "delta"
segment_base:  bytes32 | null   immediate base segment id (null for full)
segment_depth: u64              0 for full; number of deltas above the full base
segment_root:  bytes32          H("weft/index/chain/v1",
                                  WCB({generation, chain: [head_id, ..., full_id]}))
```

The attestation deliberately names the head's immediate base, its depth, and a
root over the **whole** chain rather than embedding the chain array: the root
binds every segment id (so a substituted ancestor changes the root) while the
attestation size stays constant. The loader reconstructs the chain from the
segment headers and compares its length, order, and root with these fields,
which is equivalent to comparing the array byte for byte but bounded.

`v1` and `v2` commits remain valid and are read exactly as before. A v1/v2
commit carries no chain, so its segment is accepted only through the old path
(WFX2, counts + candidates_root + state_root); a **v3** commit requires the
chain and the segment root to match.

The loader reconstructs the chain from the head segment by following
`base_id`/`base_commit` until a full segment, checks each segment's own digest
and id, checks generation decrements by one per hop, then:

1. resolves the full segment and applies every delta;
2. verifies the resolved generation and the three counts against the replayed
   state and the attestation;
3. re-derives `candidates_root` from the resolved maps and compares it;
4. compares the reconstructed chain array and `segment_root` with the
   attestation.

Any failure is a bounded diagnostic note and a rebuild. A forged or truncated
chain therefore costs a rebuild and cannot under-select. A self-consistent
subset still fails `candidates_root`.

## Bounds and policy

* `index_chain_max_deltas` (default 32): a new delta beyond the bound forces a
  full segment. The hard reader bound is 256 deltas.
* `index_bytes_budget` (optional): total bytes in `indexes/` that the store is
  allowed to hold. Before writing, the store sums the retained segments; a
  projected total over budget skips the write (advisory, noted) and the commit
  carries no attestation. Exposed by `weftfs info --json` as
  `index.bytes` and `index.budget_bytes`.
* A delta is used only when it is smaller than the full segment it replaces;
  otherwise the writer emits a full segment. This keeps a pathological
  many-key-change commit from making the chain worse.

## Read cost, rebuild cost, GC, snapshots

| question | full segment | chain of D deltas |
|---|---|---|
| read | one sequential file, one decode | D+1 files, D+1 decodes, D applies |
| read bytes | full state | full ancestor + changed rows only |
| rebuild | one state replay | same state replay (fallback) |
| write | full state | changed rows only |
| GC | delete if unreferenced | delete only if **no** retained root names any segment in the chain |
| snapshot pinning an old generation | keeps that commit's segment | keeps that commit's ancestor chain; the head's chain may share ancestors |

GC computes reachability first, then adds every segment in the chain of every
retained commit (head, snapshots, checkpoint-carrying commits, pins, live
transactions). It never deletes a segment named by a retained commit. A segment
is unreferenced only when no retained commit's chain contains it.

A snapshot pinned at generation `g` keeps `g`'s segment and every ancestor up
to its full segment. Later deltas above `g` do not affect it. Compaction of the
head writes a full segment for a **new** commit (a no-op commit with the same
state), then GC may drop the old head chain when no snapshot pins it. A snapshot
of the old head keeps the old chain, so compaction never breaks history.

The tradeoff is explicit: a chain read costs `D` extra file reads and `D` map
applies. Because a delta applies element-level patches, a per-commit delta is
proportional to the rows that actually changed, not to the size of the state;
for a commit that changes most rows the writer emits a full segment, so the read
cost is never worse than `D` full segments. The measured costs are in
`bench/RESULTS-index-amplification.md`.

## Evidence

Rust tests `tests/index_chain.rs` (chain load == full-segment answers,
forged/partial chain rejection, compaction, GC of unreferenced segments, a
snapshot pinning an old generation, chain-depth and byte-budget bounds) and
`tests/index_persistence.rs` (trust, forgery, corruption). Go:
`store/index_chain_test.go` and the three handoff tests. A full WFX3 segment is
byte-identical across Rust and Go, a Rust delta chain loads in Go, and a Go
chain loads in Rust.

Measured amplification, commit cost, chain read cost, compaction, GC, and the
pinned-snapshot retention shape are in
[`bench/RESULTS-index-amplification.md`](../bench/RESULTS-index-amplification.md).
Headline: 97,066,019 B -> 3,342,744 B in a 1,250-commit court; 104.6 MB ->
6.2 MB for the trajectory series; peak index on the old pinned trajectory
3.53 GB -> 0.11 GB. Losses: commit p50 +0.605 ms, first query after reopen
0.258 -> 1.411 ms, and snapshots keep their chains.
