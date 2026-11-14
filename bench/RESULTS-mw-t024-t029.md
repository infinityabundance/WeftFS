# T024-T029: multi-writer GC, distributed faults, history checking, migration and FUSE retention

Status: implemented and measured (Rust). All five courts pass at revision
`fd56f32f0625af6a84600e2eb1634c46d0d1a6ef` in the `weft/fuse-court:1` image
(3 GiB / 4 CPUs / 1024 pids, limits unchanged):

| gate | court | receipt |
|---|---|---|
| T024 multi-writer GC | `scripts/mw_gc_court.sh` | `bench/receipts/mw-gc-20261114T101410Z.json` |
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
<!-- END GENERATED:results-mw-t024-t029 -->

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
