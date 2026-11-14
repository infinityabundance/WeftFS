# T024-T029: multi-writer GC, distributed faults, history checking, migration and FUSE retention

Status: implemented and measured (Rust). All five courts pass at revision
`fd56f32f0625af6a84600e2eb1634c46d0d1a6ef` in the `weft/fuse-court:1` image
(3 GiB / 4 CPUs / 1024 pids, limits unchanged). The T024 gate was re-recorded
at `2b93e5f` after the epoch-CAS regression fix described below, together with
a repetition proof (see the T024 rows in the table).

| gate | court | receipt |
|---|---|---|
| T024 multi-writer GC | `scripts/mw_gc_court.sh` | `bench/receipts/mw-gc-20261114T214308Z.json` |
| T024 repetition proof | `scripts/multi_writer_gc_stress.sh` | `bench/receipts/multi-writer-gc-stress-20261114T214326Z.json` |
| T025 distributed faults | `scripts/distributed_faults_court.sh` | `bench/receipts/distributed-faults-20261114T101423Z.json` |
| T026 history checker | `scripts/history_check_court.sh` | `bench/receipts/history-check-20261114T101358Z.json` |
| T028 mode migration | `scripts/mode_migration_court.sh` | `bench/receipts/mode-migration-20261114T101419Z.json` |
| T029 FUSE under MW2 | `scripts/fuse_mw2_court.sh` | `bench/receipts/fuse-mw2-20261114T101315Z.json` |

Mechanism documents: [`docs/MULTI-WRITER-GC.md`](../docs/MULTI-WRITER-GC.md),
[`docs/DISTRIBUTED-FAULTS.md`](../docs/DISTRIBUTED-FAULTS.md),
[`docs/HISTORY-CHECKER.md`](../docs/HISTORY-CHECKER.md),
[`docs/MODE-MIGRATION.md`](../docs/MODE-MIGRATION.md),
[`docs/FUSE-MW2.md`](../docs/FUSE-MW2.md). Normative text:
`spec/WEFT-MULTIWRITER.md` §11, §15.9, §16, §17.

## Method

Every court runs in the Docker court with the hard limits unchanged. The
multi-writer courts start real processes (the `mw2_writer` driver); the FUSE
court mounts a real kernel mount; the distributed court runs real client
processes against a loopback MinIO sidecar through a userspace fault-injecting
proxy. No number here is a hand-written estimate: the generated block below is
emitted from the receipts by `scripts/perf_numbers.py`.

<!-- BEGIN GENERATED:results-mw-t024-t029 (scripts/perf_numbers.py) -->
**T024.** The adversarial pass reclaimed **16** genuinely unreachable blobs (**1,392 B**) in its control pass, reclaimed **0** on an immediate second pass, and lost **0** reachable objects across **8** process kills. It retained **2** blobs by cut/floor in the reservation-window scenario, marked **2** MW3 domain heads in one coordinated pass, and served **600** reads to **3** concurrent readers during the sweep.
**T025.** With the acknowledged head lost (S1) the store is at generation **2** after the retry; a duplicated object PUT (S2) leaves **1** object; reordered publications (S3) converge on generation **2**; the stale reader (S5) keeps generation **2**. All seven verdicts are true.
**T026.** The four generated histories carry **52** transactions, **134** records and **16** reads; every history is accepted and six deliberately violating fixtures are rejected with the expected class.
**T029.** The Rust suite runs **8** tests; the shell driver uses **6** create processes, **4** rename processes, **4** overwrite processes and **4** link/unlink processes. The same-inode truncating-open control fails **1** time(s) under MW1 and **4** time(s) under MW2, which is why it is recorded as a kernel limitation rather than a transaction loss.
**T024 concurrent passes.** Two independent processes then ran **24** passes over one store: **24** distinct epochs, contiguous from 1, disjoint across the loops, and no `Busy` refusal. The suite repeated **80** times across **3** arms (8 threads, artificial CPU load, a single CPU) with **0** failures and **0** timeouts.
<!-- END GENERATED:results-mw-t024-t029 -->

## T024 regression: the epoch CAS did not wait, and its close went backwards

CI run 43631714328 failed `concurrent_passes_are_serialised_by_the_epoch_cas`
with `Busy: another GC pass holds the epoch; retry after it closes`. Two real
defects, both only visible when two passes overlap:

* `acquire_gc_epoch` retried a lost compare-and-swap sixteen times **without
  waiting**, so a pass spent its whole budget inside the other pass's
  read-verify-write of `gc/epoch` (plus the `fsync`s that publish it). The retry
  now uses the contract's deterministic backoff (200 µs doubling to 25 ms) for a
  bounded **wall-clock** budget of 3 s. It is a duration, not an attempt count,
  because the critical section grows with storage latency: the first version of
  this fix kept 16 attempts (225.4 ms) and still failed 5 of 50 full-suite runs
  once the store was on a real filesystem beside eight concurrent suites
  (retained log `multi-writer-gc-stress-20261114T213203Z.log`).
* the epoch was closed with an **unconditional replace**, so a pass that
  acquired an earlier epoch and finished last wrote its older epoch over the
  newer pass's record. The close is now a compare-and-swap on the bytes this
  pass published at acquisition; a superseded pass leaves the newer record and
  reports `gc_epoch_closed = false`.
* the sweep tolerates its twin: a candidate the other pass unlinked first is
  `NotFound`, which used to fail the whole pass. "Already gone" is the outcome
  both wanted, and `deleted_bytes` now counts only blobs this pass actually
  unlinked, so a contested blob is never double counted.

The regression is proven two ways. The ordering-specific interleaving is not
reachable through the public API (a test cannot hold an epoch open across
another pass), so `src/store.rs::gc_epoch_record_tests` drives acquire and
close directly and fails deterministically with the old close
(`a superseded pass must not rewrite the record`). The acquisition half is
reachable: `tests/multi_writer_gc.rs` releases both passes on a barrier
immediately before `gc`, and with the old tight loop 50 of 50 runs of that test
fail with the CI message once the store is on a real filesystem, against 0 of
50 with the backoff. The court gained scenario F: two independent processes,
24 passes, 24 distinct contiguous epochs, zero refusals.

## Losses

* **T024 residual risk.** A publisher that stalls past its reservation lifetime
  and then publishes without the helper's pre-commit reservation check is
  outside the protocol; the pass may already have reclaimed its uncommitted
  blobs. The helpers and the FUSE path check; a direct `begin_transaction`
  caller must do the same.
* **T024 scope.** The pass is a single-store administration operation. Physical
  power loss, host cache loss, media failure and a fleet-wide GC are not
  claimed. `local`/`object`/`3fs` GC still republishes logical state for orphan
  removal; the epoch machinery covers blob reclamation, not that commit.
* **T024 Go parity is absent.** `WeftFS-Go` GC remains quiescent under a
  multi-writer handle.
* **T025 is a proxy court.** One host, one backend instance, userspace HTTP
  faults above the connection. It is not a partition of physical nodes, not a
  server crash, not media or power loss, and not a clock-skew experiment.
* **T026 does not check global serializability.** It checks per-key register
  linearizability against the publication order, per-domain multi-key prefix
  consistency, conflict justification and handle monotonicity. Cross-domain
  disagreement is expected by the contract and is deliberately not flagged.
* **T026 is Rust-only** for emission; the checker is independent Python.
* **T028 leaves pre-record binaries outside the protocol.** The real v0.9.0
  binary reads and writes a newer store and the store verifies; it cannot be
  bound by a record it does not know. `min_reader` is the forward mechanism and
  is not enforced by this revision.
* **T028 quiescence is a one-host heuristic.** A writer idle for the whole
  window and publishing afterwards is not detected.
* **T029 is not one transaction per POSIX call sequence.** Each `write(2)` is
  one transaction, and a truncating open is a truncate transaction followed by
  writes. A crash between them leaves the truncated version.
* **T029 snapshot isolation is per open handle.** Reads use the pinned version;
  `stat` reports the current version.
* **T029 bounds the rewrite.** A write past the whole-file bound is `E2BIG`;
  large files need MW1 or a future range-write transaction.
* **T029 measures a pre-existing kernel limitation.** Concurrent same-inode
  `O_TRUNC` opens fail with `EIO` under both MW1 and MW2 on kernel 7.3. It is
  recorded as a control, not attributed to the transaction path.
* **No Go parity** for T025, T026 emission, T028 migration or T029 FUSE
  mutations.

## Non-claims

No physical power loss, host cache loss, media failure, multi-node or fleet
claim; no global serializability across MW3 domains; no cross-domain snapshot
isolation; no multi-writer FUSE daemon; no claim that optimistic publication is
faster for one writer. MW1 remains the default everywhere.
