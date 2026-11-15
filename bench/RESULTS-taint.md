# T075 — taint tracing and quarantine

Receipt: `bench/receipts/taint-20261115T124905Z.json` (stamp `20261115T124905Z`,
WeftFS revision `63ad66c`). Raw log beside it. Normative record definition:
[`spec/WEFT-TAINT.md`](../spec/WEFT-TAINT.md).

## 1. What was built, and what it is not

`taint_trace(roots, artifacts)` walks the provenance graph forward from one or
more tainted **content roots** and gives every named artifact one of three
verdicts: `tainted` (a path exists, with its witness), `clear` (the artifact's
whole backward closure is complete and contains no tainted node), or `unproven`
(the graph is incomplete on that closure, with the gaps named). An incomplete
graph is never reported as `clear`.

`quarantine(roots, policy)` publishes a signed `weft.quarantine.v1` record per
content root, append-only and epoch-fenced, and enforcement is checked on the
object's content root, so a copy under a new name and a rewrite that produced
the same bytes are both refused. In `non-disclosing` mode the refusal is
`NotFound: no such object`, byte-identical to an absent object; in `explicit`
mode it is a typed `Quarantined` error.

**This is not unlearning.** Quarantining cannot remove information already
learned into model weights. The deliverable is traceability plus refusal to
serve. Nothing in the module inspects, edits, retrains or repairs a model, and
no claim of retroactive removal is made anywhere in this repository.

## 2. Method

Harness `examples/taint_court.rs`, court `scripts/taint_court.sh`, Rust court.
Fixture: one dataset member, one untainted model declaration, two raw
checkpoints, and one provenance receipt per checkpoint that names the dataset as
an input. Then:

* trace from the dataset root to both checkpoints;
* 2,000 warm whole-object reads of one checkpoint manifest in three states: no
  quarantine state, one unrelated quarantine present (lookup miss), and the
  branch quarantined (lookup hit and refusal);
* quarantine one branch and restore the other;
* an artifact with no receipt (must be `unproven`);
* a signed release, then a reopen that must still enforce.

## 3. Results

<!-- BEGIN GENERATED:taint-costs (scripts/perf_numbers.py) -->
Trace and gate cost, medians of 2,000 warm whole-object reads on one host: a trace of this fixture takes **46 µs**; a plain read takes **6,222 ns**; with the gate present and not matching it takes **7,113 ns** (**+891 ns**); a refused read takes **41,007 ns** (**+34,785 ns**).
<!-- END GENERATED:taint-costs -->

The mechanism is cheap where it must be: the gate is a namespace lookup on the
object's content, so an unquarantined read pays one miss. A *refused* read pays
a hit plus a record decode, which is why the refusal column is the expensive
one — the cost of an enforcement decision, not of the fast path.

## 4. Correctness evidence (suite)

`tests/taint.rs` (12 tests) pins the parts the harness only samples:

* the closure is precise: exactly the dataset member and the two checkpoints are
  tainted, and the model declaration (an input, not an output) is not;
* quarantining one branch leaves the other restoring byte-identical output, and
  the quarantined branch is refused at `read_object`, `load_checkpoint_manifest`,
  `restore_checkpoint_bytes` and `restore_named_tensor_at`;
* the non-disclosing refusal is byte-identical to an absent object's;
* an empty authority set refuses both quarantining and lifting; a signer outside
  the set is `Unauthorized` and writes nothing;
* a release below the active epoch is `Conflict`; rebinding at the same epoch is
  `Conflict`; a correct release restores exact service;
* a tampered record does not decode, a record re-signed by the wrong key fails
  the authority check, and a manually overwritten pointer file is `Corrupt`;
* an artifact with no receipt, and a receipt whose input hint no longer
  resolves, are both `unproven` — not `clear`;
* a depth bound that would truncate a two-hop chain is `LimitExceeded`;
* a copy of a quarantined artifact under a new name is refused;
* the decision survives a read-only reopen and still enforces.

## 5. Losses

1. **`clear` is relative to the recorded graph.** An actor that omits a receipt
   makes an artifact `unproven`; an actor that records a false receipt makes it
   `clear` dishonestly. The mechanism detects missing evidence, not lying
   evidence.
2. **A refusal is expensive relative to a read.** The refusal column of the
   generated block is dominated by reading and decoding the quarantine record on
   every refused read. There is no cache: cross-process visibility was chosen
   over speed, and a cache would need an invalidation rule the store does not
   have.
3. **Enforcement is store-level.** Bytes already handed to a client cannot be
   recalled, and a backend that still holds the blob is not erased by this
   record. A quarantine is a refusal to serve through this store.
4. **One snapshot of the graph.** A trace reads the handle's current state; a
   concurrent commit is visible only after `refresh_to_head` or a reopen.
5. **No Go parity.** WeftFS-Go rejects both schemas.
6. One host, one run, warm cache.

## 6. Non-claims

* No unlearning, no model surgery, no retroactive training removal.
* No fleet, cloud or media claim; no cross-cluster propagation of a quarantine.
* No claim that a trace can prove a negative about the world — only about the
  recorded graph.
