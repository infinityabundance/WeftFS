# T100 — a bounded refinement link between `weft.history.v1` and `WeftMultiWriter.tla`

**Status:** research gate. Everything here is a *bounded* correspondence, not a
proof of refinement. The checker, its negative controls and the cross-check
against TLC are executed by `scripts/refinement_court.sh`; the receipt is
`bench/receipts/refinement-<stamp>.json`.

## 0. What was asked and what was delivered

The gate asks for a *machine-verifiable correspondence for bounded operations
with explicit assumptions*, not a new consensus algorithm. Delivered:

1. An explicit, written mapping from the store's existing `weft.history.v1`
   observation records to the abstract actions of
   `verification/tla/WeftMultiWriter.tla`.
2. `verifier/refinement_check.py` — an independent stdlib program that decides
   whether a bounded history is a valid behaviour of the model under that
   mapping, and names the invariant a rejected history breaks.
3. Non-vacuity in both directions: four real histories produced by the shipped
   MW2/MW3 driver are accepted, five mutants derived from a real history and
   eight committed invariant fixtures are rejected with their named invariant.
4. A tie between the *transcription* used by the replay and the actual TLA
   module: `verifier/refinement_crosscheck.py` enumerates the whole transition
   relation for two tiny configurations and the court requires TLC's distinct
   and generated state counts to match exactly (28,713 / 246,445 and
   176,427 / 1,772,785 in the cited receipt). The two negative-control modules
   must also produce the same violated invariants under both tools.

## 1. A gap the model had, and the smallest fix

`WeftMultiWriter.tla` could not describe a committer that publishes more than
one transaction: `Ack(c)` sets `phase[c] = "done"` and no action ever returned
the slot to `"idle"`. Every real history has handles that commit repeatedly, so
under the model as written *no* real history is a behaviour.

T100 adds one action to the corrected module and both negative controls:

```tla
Release(c) ==
  /\ phase[c] = "done" /\ ~crashed[c]
  /\ phase' = [phase EXCEPT ![c] = "idle"]
  /\ UNCHANGED <<head, chain, durableBlob, deletedBlob, txn, observedHead,
                  attempts, acked, pending, crashed, readHead>>
```

It forgets an already-acknowledged slot and touches no safety property. The
cross-check enumerates `Release` in all three modules, so the tie holds for the
model as committed.

## 2. The mapping

One `weft.history.v1` **conflict domain** is one model instance. MW3 domain
projections are checked independently; nothing cross-domain is claimed.

| history record | model actions | notes |
|---|---|---|
| `tx-invoke` + committed `tx-response` at generation `g` | `Start(c)` ; `Publish(c, g)` ; `Ack(c, g)` ; `Release(c)` | `chain[g].writes` = the invoke's key set; `c` = the record's `handle` |
| `tx-invoke` + idempotent `tx-response` | the same three actions | the retry proves the earlier, unacknowledged attempt published |
| `tx-invoke` + rejected `tx-response`, class `Conflict`, a modelled write in `(base, observed]` | `Start(c)` ; `Revalidate(c)` (conflict branch) | abort with a justified conflict |
| `tx-invoke` + rejected `tx-response`, create precondition (`NameExists` / `ObjectExists`) and a declared absent precondition | `Start(c)` then the slot is freed | **abstracted**: the model has no create precondition |
| `tx-invoke` + rejected `tx-response`, any other class (`Busy` exhaustion, quota, capability, id reuse) | `Start(c)` ; `CrashPrepared(c)` | **abstracted**: the model has no such guards |
| `tx-invoke` with no response, handle used again later | `Start(c)` ; `CrashPrepared(c)` at its base generation | the later use proves the process died |
| `tx-invoke` with no response, handle's last transaction | `Start(c)`, left prepared to the end | so `LivePreparedDurable` can be exercised |
| `read-response` | `Read(r, read_generation)` | reader = `handle` |
| synthetic `gc-observe` | `GcReclaim(key)` at `at_generation` | the store does not emit this; it exists to exercise the GC invariants |

### 2.1 Ordering

The append order of the JSON Lines file is **not** a linearisation order: a
commit's response is appended after the acknowledgement, which can be after a
reader has already observed its generation. The replay therefore uses a
canonical order:

1. generations ascending; the commit for generation `g` is published first
   (`Publish` requires `generation == head + 1`, so this is forced);
2. then, within a generation, the store's own `observed_ns` timestamps order
   starts, aborts and reads (a handle is sequential, so its own events are
   monotone);
3. synthetic GC observations, which carry no timestamp, are placed last within
   their generation.

`Start` is placed at the publication of the generation the transaction
observed (`base_generation`), not at wall-clock time. The model's `Start` only
reads the head and the version map; the history records the observation
generation, not the instant of the read. A transaction that observed a
generation *before* the first modelled commit is clamped to the prefix end
(§2.2), which is equivalent for the modelled key universe.

### 2.2 The unmodelled prefix

The store's initialisation commit and anything published before the first
observed generation are not in the history's key universe. The model starts at
`initial_head = min(published generation) - 1` with those generations present
as prefix commits that write no modelled key. Consequence, stated plainly: a
blind overwrite of a key that existed *before* the trace is invisible to this
refinement. The store's own conflict machinery covers it; the model does not.

## 3. What the checker rejects, and how it attributes it

| rejection | invariant |
|---|---|
| a commit overwrites a key whose version differs from what the transaction could have observed | `NoBlindOverwrite` |
| an acknowledgement names an unpublished generation, an unowned generation, or a transaction with no `Start` | `NoLostAck` |
| two transactions claim one generation | `NoLostAck` |
| a generation at or below the head has no complete commit | `HeadWellFormed` |
| a read selects a generation above the head or a generation with no commit | `NoPartial` (and `ReaderIsAPrefix`) |
| a reclaimed key is reachable from the head | `GcNeverReclaimsReachable` (and `CommittedBlobsDurable`) |
| a reclaimed key is needed by a prepared transaction | `LivePreparedDurable` |
| a `Conflict` abort with no modelled write in `(base, observed]` | `NoBlindOverwrite` |
| a record that maps to no action | `mapping` |

`TypeOK` and `CommitOrderIsStable` hold by construction of the replay (chain
indices *are* generations) and are reported as such, not as checked.

### 3.1 Retry coverage, disclosed

The store retries only after a lost `Busy` race; a conflicting key change
aborts the transaction. Therefore a committed transaction with `attempts == 1`
proves it observed every key it writes at `base_generation`, and the check is
exact. With `attempts > 1` the last refresh point is not in the history, so the
checker enforces only the counting bound *number of conflicting generations in
`(base, g)` ≤ attempts − 1*. The receipt reports `fully_checked`,
`bounded_checked` and `abstracted_retries` separately; neither is presented as
the other.

## 4. Cross-check against TLC

`verifier/refinement_crosscheck.py` implements the same transition relation
(corrected model and both flaws) and enumerates it breadth-first over the full
variable domain. The court runs TLC on the same configurations and requires:

* identical distinct-state counts (TLC counts its `CHOOSE`-normalised states;
  the enumerator normalises the same way),
* identical states-generated counts (the enumerator adds one for the initial
  state to match TLC's counter),
* no invariant violation for the corrected model,
* for `WeftMultiWriterBuggy`, TLC reports `NoBlindOverwrite` and the enumerator
  agrees; for `WeftMultiWriterGcBuggy`, TLC reports `LivePreparedDurable`,
  `CommittedBlobsDurable` and `GcNeverReclaimsReachable` in one-invariant-at-a-
  time runs (TLC stops at the first counterexample), and the enumerator agrees.

This is the only machine-checked tie between the replay and the TLA module. It
is bounded: two configurations, one key or two keys, two committers, two
generations.

## 5. What this does not establish

* **No proof of refinement.** It is one bounded projection of one observation
  channel, checked on histories that fit the declared bounds (256 transactions,
  256 keys, 1,024 generations, 256 readers per instance).
* **No unbounded verification.** TLC's runs and the enumerator are bounded
  configurations.
* **No liveness, fairness or starvation claim.** The model has no fairness
  assumption and the checker checks only state invariants.
* **No cross-domain (MW3) property.** Each domain is an independent instance.
* **No value-level read consistency.** `verifier/history_check.py` owns it; the
  abstract model has no values.
* **No statement about the physical commit protocol** below the history
  channel: torn writes, superblock slots, fsync ordering and object durability
  are outside the model (see `verification/tla/WeftCommit.tla`).
* **The retry action subset is not exercised.** The mapping collapses a
  prepare/retry sequence into `Start` at the observed generation.
* **The model has no create preconditions, quotas, capabilities or leases.**
  Twelve of the sixteen transactions in the conflict fixture are abstracted for
  exactly that reason; the receipt counts them.

## 6. Reproduce

```
bash scripts/refinement_court.sh
```

Three stages: a fuse court (histories, checker, enumerator), a TLA court (TLC),
and a fuse court (comparison and receipt). The script prints
`refinement_court: skip <reason>` and exits 78 when Docker, the images or the
daemon are unavailable.
