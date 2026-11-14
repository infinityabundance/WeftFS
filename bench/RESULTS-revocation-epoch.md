# Revocation epoch — measured results (T078)

Receipt: `bench/receipts/revocation-epoch-20261114T234309Z.json` + `.log`.
Court: rust court (3 GiB / 4 CPUs / 1024 pids, bounded tmpfs), one local store,
no object backend.

<!-- BEGIN GENERATED:revocation-epoch-verdict (scripts/perf_numbers.py) -->
**11** checks pass with **0**
failures, and the deny-before-decode counter proof is the separate Rust test
`tests/revocation_epoch.rs` run in the same court.
<!-- END GENERATED:revocation-epoch-verdict -->

Retained, not cited: `revocation-epoch-20261114T225925Z.json`, measured from a
tree ahead of the revision it recorded; the checks are the same and the receipt
above supersedes it.

## What is proven

* A published epoch is canonical WCB, chains by `prev`, and re-publishing an
  entry advances the epoch with a valid chain.
* A client whose observation predates the store's current epoch is refused with
  `StaleEpoch` **before** the target object is touched: with a 1 MiB target,
  `decode_calls` does not move and decoded/backend byte deltas stay under 8 KiB
  (the small epoch control record).
* A revoked content root denies with `CapabilityRevoked` and the fixed text
  `revocation denies this read`; the denial carries no id, size or count.
* An absent id gets the ordinary `NotFound`, identical for two different absent
  ids, so revocation is not an existence oracle for callers who may not read the
  object.
* Revocation is tenant-scoped: tenant A is denied while tenant B reads the exact
  same deduplicated bytes.
* A revocation that arrives mid-session makes the old observation refuse; a
  refreshed observation then denies.
* An observation past its own staleness bound refuses.
* A rolled-back current pointer and a missing history record are `Corrupt`.
* A revoked capability envelope denies even though the envelope's own expiry is
  still valid; a mismatched tenant argument is `InvalidArgument`.
* The CLI publishes, shows, verifies and observes the same chain.

## Costs and losses

* Every authorized read re-reads the current epoch control record. That is one
  small object read per decision, not per byte.
* `verify_revocation_chain` is O(epochs) and is called by `observe`/`verify`, not
  by the per-read gate.
* The history grows with publications; no compaction is implemented. A store
  with millions of revocation publications would need one.
* There is no distribution: a second store has its own chain. No gossip, no
  consensus, no clock-skew defence, no transparency log.
* Refusal for a revoked *and* unreadable object is the ordinary denial, so the
  caller cannot distinguish "revoked" from "absent"; that is deliberate
  non-disclosure, not a precision loss to be "fixed" later.
* Already-decoded bytes in a consumer's memory cannot be retracted. The gate
  governs new authorization decisions.
