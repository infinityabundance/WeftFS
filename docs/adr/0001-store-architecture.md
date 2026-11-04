# ADR-0001: Store architecture — immutable objects, commit log, rebuildable indexes

**Status:** Accepted for v1 (2026-11-03).
**Context:** WeftFS needs a portable on-disk format that Rust and Go can open
independently, with crash-safe commits, snapshots, graph relations, and
context selectors.

## Options considered

1. **Immutable content objects + append-only commit records + superblock root +
   rebuildable in-memory indexes.** (chosen)
2. **SQLite as the metadata engine plus content files.**
3. **Custom LSM/B-tree metadata engine.**

## Decision

Option 1. The authoritative state is a chain of canonical WCB commit records
whose root is published by one of two alternating CRC-protected superblock
slots. Content and manifests are immutable, content-addressed files. Object,
entry, relation, snapshot, and tip indexes are derived by replay and rebuilt on
open. A full-state checkpoint manifest bounds replay cost.

## Rationale

- **Portability:** the format uses only documented bytes, `fsync`, `rename`, and
  `flock`; Go needs no Rust or SQLite ABI.
- **Crash contract:** a single root publication point makes the visible prefix
  unambiguous and testable by truncation/corruption injection.
- **Reference integrity:** immutable files are never mutated in place, so open
  handles and pinned snapshots cannot observe torn content.
- **Auditability:** every mutation is a canonical record with a hash.
- **Simplicity of recovery:** worst case is replay from genesis; indexes are
  disposable.

## Tradeoffs / rejected options

- SQLite would give fast indexed queries and mature transactions, but it makes
  byte-level portability, crash semantics, and independent Go/Rust readers
  depend on a third-party file format and library versions. It remains a
  candidate for a future **derived index** (rebuildable), not the source of
  truth.
- A custom LSM/B-tree would add write amplification and recovery complexity
  before any measurement justifies it. Not adopted.
- Full-state-per-commit is rejected: it is O(state) per commit.
- Replay cost grows with history; checkpoints every 256 commits bound it. This
  is a documented v1 cost, to be measured before any performance claim.

## Consequences

- `weftfs verify` and `repair` are first-class.
- GC must mark from all snapshots, tips, checkpoints, and open-handle pins.
- The commit record schema and checkpoint schema are normative (`WEFT-MODEL.md`
  §2.2, §2.4) and shared with WeftFS-Go.
- No multi-writer claim: `flock` enforces one writer.

## Evidence status

Architecture decision, not a performance claim. Correctness evidence is tracked
in `bench/receipts/` and the crash court; no comparative benchmark exists yet.
