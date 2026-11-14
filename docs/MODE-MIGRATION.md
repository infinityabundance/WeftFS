# Explicit safe mixed-mode migration (T028)

Status: **implemented and measured** (Rust). Normative text:
`spec/WEFT-MULTIWRITER.md` §16. Court: `scripts/mode_migration_court.sh`,
receipt `bench/receipts/mode-migration-20261114T101419Z.json` (revision
`fd56f32`).

## The mode record

The writer mode is one canonical record at `writers`
(`weft.writers.v1`: `schema`, `mode`, `min_reader`, `created_ns`). An absent
record means `single` (MW1). `weftfs mode show` reports the effective mode;
`weftfs mode migrate --from --to --quiesce-ms` changes it.

## Transition table

| from | to | verdict |
|---|---|---|
| `single` | `optimistic` | allowed |
| `optimistic` | `sharded` | allowed, with a quiescence window and no live prepared transaction |
| `single` | `sharded` | refused `UnsupportedVersion` |
| `optimistic` | `single` | refused `UnsupportedVersion` (downgrade) |
| `sharded` | `optimistic` | refused `UnsupportedVersion` (downgrade) |
| `sharded` | `single` | refused `UnsupportedVersion` (downgrade) |

Naming a mode the store does not actually record is refused; the caller must
name what is there.

## Why there is no dual authority

The migration is **one compare-and-swap** on the `writers` record. Before it the
store records `from`, after it `to`; there is no instant where both are
recorded. Every writable open reads the record and refuses a mode it does not
permit **before** it takes the exclusive writer lock.

Two conforming writers cannot both be authoritative: a `from` writer that lost
the CAS is refused at its next open; a `to` writer cannot open before the CAS.
The court injects a process fault immediately before and immediately after the
CAS and shows the store is old-or-new, never mixed, and verifies.

The quiescence window is a **one-host heuristic**: the head must not move during
it, and no prepared transaction may be live. It is not a distributed barrier; a
writer idle for the whole window and publishing afterwards is not detected. That
is stated, not claimed safe.

## The court

* A fresh store; `single` write and read.
* `single → optimistic`: the record is created and `single` writes are refused.
* A live prepared transaction blocks `optimistic → sharded` (`Busy`); after the
  outcome is resolved the migration succeeds.
* After `sharded`: `single` and `optimistic` opens are refused, a domain write
  verifies, and every downgrade is refused.
* `single → sharded` is refused.
* A crash before and after the CAS leaves the old or the new mode and a store
  that verifies.
* A reader that spans the transition keeps its snapshot and the store verifies.
* The **real v0.9.0 binary** (built from the tagged source in the court) reads
  and writes a newer store and the store still verifies; the record still says
  `optimistic`, and a new `single` writer is still refused.

## The residual hazard: pre-record binaries

v0.9.0 knows nothing about the mode record and writes to a store whose mode
would not admit it. The court measures this rather than hiding it: it is the one
window the migration cannot close for software that is not part of the protocol.
The forward mechanism is `min_reader` in the record, enforced by a future
revision; it is not enforced here.

## Not claimed

No multi-node quiescence, no clock-skew defence, no automatic downgrade, no
automatic `single → sharded` promotion, and no Go migration verb.
