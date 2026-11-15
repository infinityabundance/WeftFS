#!/usr/bin/env python3
"""T100 — bounded refinement checker: `weft.history.v1` -> WeftMultiWriter.tla.

The checker decides whether a bounded transaction history produced by the store
is a valid behaviour of the abstract multi-writer model under the explicit
mapping in `verification/REFINEMENT.md`. It shares no code with the store: it
consumes JSON Lines, imports nothing from WeftFS, and runs no WeftFS binary.

# The mapping (see verification/REFINEMENT.md for the full table)

The model has abstract committers (`Committers`), a set of keys, a published
`chain` indexed by generation, a `head`, per-committer `phase`/`txn`/`attempts`/
`acked`/`pending`/`crashed`, a `readHead` per reader, and `durableBlob`/
`deletedBlob` sets.

* One `weft.history.v1` **conflict domain** is one model instance. MW3 domain
  projections are checked independently; no cross-domain property is claimed.
* A committed `tx-response` at generation `g` maps to
  `Start(c) ; Publish(c, g) ; Ack(c, g)` where `c` is the committer allocated
  to that logical transaction and `chain[g].writes` is the invoke's key set.
* A rejected conflict `tx-response` maps to `Start(c) ; Revalidate(c)` with the
  conflict observed at `observed_generation`; the model's `Revalidate` aborts.
* Any other rejected response (Busy after the retry budget, quota, capability,
  id reuse) maps to `Start(c)` followed by `CrashPrepared(c)`: the model has no
  quota/authorization guard, so those aborts are counted as **abstracted**, not
  checked here.
* An invoke with no response at all maps to `Start(c) ; CrashPrepared(c)`.
* An `idempotent` response collapses a crashed attempt and its retry into one
  `Start ; Publish ; Ack`: the retry proves the earlier attempt published.
* A `read-response` maps to `Read(r, read_generation)`.
* A synthetic `gc-observe` record (the store does not emit it; it exists so the
  GC invariants can be exercised) maps to `GcReclaim(key)` at `at_generation`.
* `Start` is placed at the publication of the generation the transaction
  observed (`base_generation`), not at wall-clock time: the model's `Start`
  only reads the head and the version map, and the history records the
  observation generation, not the instant of the read.

# What the checker rejects

Each rejection names the model invariant the rejected behaviour would break.
A history that is not a behaviour of the model is rejected even when no
invariant fails (for example an abort with no conflicting write); those cases
are reported as `not_a_model_behaviour` with the action that cannot fire.

# Retry abstraction (declared, not hidden)

The store retries only after a lost `Busy` race; a conflicting key change aborts
the transaction. A committed transaction with `attempts == 1` therefore proves
it observed the version of every key it writes at `base_generation`, and the
check is exact (`fully_checked`). With `attempts > 1` the last refresh point is
not in the history, so the checker can only enforce the counting bound
`#conflicting generations in (base, g) <= attempts - 1` (`bounded_checked`).
Both counts are reported; neither is silently treated as the other.

Usage:
  refinement_check.py HISTORY.jsonl [--expect accept|reject]
                                    [--expect-invariant NAME]
                                    [--bound transactions=N] [--out REPORT.json]
"""

import argparse
import json
import sys
from collections import OrderedDict, defaultdict

SCHEMA = "weft.history.v1"
GC_EVENT = "gc-observe"

# Model bounds. A history outside them is refused rather than silently
# analysed under a configuration the model was never run with.
DEFAULT_BOUNDS = {
    "transactions": 256,
    "keys": 256,
    "generations": 1024,
    "readers": 256,
}


class Rejected(Exception):
    """A history is not a valid behaviour of the model."""

    def __init__(self, invariant, detail, action=None):
        super().__init__(detail)
        self.invariant = invariant
        self.detail = detail
        self.action = action


def load(path):
    records = []
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as err:
                raise Rejected("mapping", f"{path}:{number}: not JSON: {err}")
            if record.get("schema") != SCHEMA:
                raise Rejected(
                    "mapping", f"{path}:{number}: schema is not {SCHEMA}"
                )
            record["_line"] = number
            records.append(record)
    return records


def domain_of(record):
    return record.get("domain") or ""


def op_key(op):
    key = op.get("key")
    if not isinstance(key, str):
        raise Rejected("mapping", f"op without a string key: {op!r}")
    return key


# ---------------------------------------------------------------------------
# Ingestion: one logical transaction per tx id, one record stream per domain.
# ---------------------------------------------------------------------------


class LogicalTransaction:
    __slots__ = (
        "tx",
        "handle",
        "base",
        "writes",
        "values",
        "first_line",
        "invokes",
        "response",
        "kind",
        "invoke_ns",
        "response_ns",
        "create_preconditions",
    )

    def __init__(self, tx, handle, base, ops, line, create_preconditions):
        self.tx = tx
        self.handle = handle
        self.base = int(base or 0)
        self.writes = []
        self.values = {}
        for op in ops or []:
            key = op_key(op)
            if key not in self.values:
                self.writes.append(key)
            self.values[key] = op.get("value")
        self.first_line = line
        self.invokes = 1
        self.response = None
        self.kind = None
        self.invoke_ns = 0
        self.response_ns = 0
        self.create_preconditions = create_preconditions


def ingest(records):
    transactions = OrderedDict()
    reads = []
    gcs = []
    for record in records:
        event = record.get("event")
        if event == "tx-invoke":
            tx = record.get("tx")
            if not isinstance(tx, str):
                raise Rejected("mapping", f"tx-invoke without a tx id: {record!r}")
            if tx in transactions:
                # A crash + retry with the same id. The ops are identical by
                # construction; the first invoke's observation base is used.
                transactions[tx].invokes += 1
            else:
                transactions[tx] = LogicalTransaction(
                    tx,
                    record.get("handle") or "",
                    record.get("base_generation", 0),
                    record.get("ops"),
                    record["_line"],
                    any(
                        isinstance(pre.get("expected"), dict)
                        and pre["expected"].get("kind") == "absent"
                        for pre in record.get("preconditions") or []
                    ),
                )
                transactions[tx].invoke_ns = int(record.get("observed_ns", 0))
        elif event == "tx-response":
            tx = record.get("tx")
            entry = transactions.get(tx)
            if entry is None:
                # An acknowledgement for a transaction with no Start is not a
                # model behaviour: the model cannot ack what it never prepared.
                raise Rejected(
                    "NoLostAck",
                    f"line {record['_line']}: tx-response for {tx!r} without tx-invoke",
                    "Ack",
                )
            outcome = record.get("outcome")
            if outcome in ("committed", "idempotent"):
                if entry.response is not None:
                    previous = entry.response
                    if (
                        previous.get("generation") == record.get("generation")
                        and previous.get("outcome") == "idempotent"
                    ):
                        continue
                    raise Rejected(
                        "NoLostAck",
                        f"tx {tx} acknowledged twice "
                        f"({previous.get('generation')} then {record.get('generation')})",
                        "Ack",
                    )
                entry.response = record
                entry.kind = "commit"
                entry.response_ns = int(record.get("observed_ns", 0))
            else:
                entry.response = record
                entry.kind = "reject"
                entry.response_ns = int(record.get("observed_ns", 0))
        elif event == "read-response":
            reads.append(record)
        elif event == GC_EVENT:
            gcs.append(record)
        elif event in ("read-invoke", "note"):
            continue
        else:
            raise Rejected("mapping", f"line {record['_line']}: unknown event {event!r}")
    return transactions, reads, gcs


# ---------------------------------------------------------------------------
# One model instance per conflict domain.
# ---------------------------------------------------------------------------


class Instance:
    """A replayed `WeftMultiWriter.tla` state machine over one domain."""

    def __init__(self, domain, transactions, reads, gcs, bounds):
        self.domain = domain
        self.transactions = transactions
        self.reads = reads
        self.gcs = gcs
        self.bounds = bounds
        self.violations = []
        self.counts = defaultdict(int)
        self.notes = []

        # chain[g] = {"owner": committer, "tx": id, "writes": [...],
        #             "values": {...}} ; generation g with no commit is absent.
        self.chain = {}
        self.head = 0
        self.phase = {}
        self.prepared = {}
        self.acked = {}
        self.read_head = {}
        self.durable = set()
        self.deleted = set()
        self.keys = set()

    # -- helpers -----------------------------------------------------------
    def violate(self, invariant, detail, action=None):
        self.violations.append(
            {"invariant": invariant, "detail": detail, "action": action}
        )

    def complete(self, generation):
        return generation in self.chain

    def writes_at(self, generation):
        entry = self.chain.get(generation)
        return entry["writes"] if entry else []

    def version_at(self, limit, key):
        best = 0
        for generation in range(1, limit + 1):
            if key in self.writes_at(generation):
                best = generation
        return best

    def head_reachable(self):
        reachable = set()
        for generation in range(1, self.head + 1):
            reachable.update(self.writes_at(generation))
        return reachable

    def live_prepared(self):
        reachable = set()
        for committer, entry in self.prepared.items():
            if self.phase.get(committer) in ("prepared", "publishing"):
                reachable.update(entry["writes"])
        return reachable

    def reachable(self):
        return self.head_reachable() | self.live_prepared()

    # -- invariants (evaluated after every mapped action) ------------------
    def check_invariants(self):
        # HeadWellFormed: every generation up to the head is a complete commit.
        for generation in range(1, self.head + 1):
            if not self.complete(generation):
                self.violate(
                    "HeadWellFormed",
                    f"generation {generation} is at or below the head {self.head} "
                    f"but no complete commit names it",
                )
                return
        # NoLostAck: every acknowledgement names a published commit it owns.
        for committer, generation in self.acked.items():
            if generation == 0:
                continue
            if generation > self.head or not self.complete(generation):
                self.violate(
                    "NoLostAck",
                    f"committer {committer} acked generation {generation}, "
                    f"head is {self.head}",
                    "Ack",
                )
                return
            owner = self.chain[generation]["owner"]
            if owner != committer:
                self.violate(
                    "NoLostAck",
                    f"committer {committer} acked generation {generation}, "
                    f"which is owned by {owner}",
                    "Ack",
                )
                return
        # NoPartial / ReaderIsAPrefix: no reader selected an unpublished head.
        for reader, generation in self.read_head.items():
            if generation > self.head:
                self.violate(
                    "NoPartial",
                    f"reader {reader} selected generation {generation} above the "
                    f"head {self.head}",
                    "Read",
                )
                self.violate(
                    "ReaderIsAPrefix",
                    f"reader {reader} selected generation {generation} above the "
                    f"head {self.head}",
                    "Read",
                )
                return
            if generation > 0 and not self.complete(generation):
                self.violate(
                    "NoPartial",
                    f"reader {reader} selected generation {generation}, which is "
                    f"not a complete commit",
                    "Read",
                )
                return
        # CommittedBlobsDurable / GcNeverReclaimsReachable / LivePreparedDurable.
        for key in sorted(self.deleted):
            if key in self.head_reachable():
                self.violate(
                    "GcNeverReclaimsReachable",
                    f"key {key} was reclaimed but is reachable from the head "
                    f"{self.head}",
                    "GcReclaim",
                )
                self.violate(
                    "CommittedBlobsDurable",
                    f"key {key} was reclaimed but a published commit names it",
                    "GcReclaim",
                )
                return
            if key in self.live_prepared():
                self.violate(
                    "LivePreparedDurable",
                    f"key {key} was reclaimed while a prepared transaction needs "
                    f"it",
                    "GcReclaim",
                )
                return
        # CommitOrderIsStable: chain[] indices are generations, so it holds by
        # construction. TypeOK holds by construction of this replay.

    # -- the replay --------------------------------------------------------
    def replay(self):
        transactions = self.transactions
        commits = {}
        rejects = []
        silent = []
        for tx, entry in transactions.items():
            if entry.kind == "commit":
                generation = int(entry.response.get("generation", 0))
                if generation <= 0:
                    raise Rejected(
                        "NoLostAck",
                        f"tx {tx} committed at non-positive generation {generation}",
                        "Ack",
                    )
                if generation in commits:
                    other = commits[generation]["tx"]
                    raise Rejected(
                        "NoLostAck",
                        f"generation {generation} is claimed by {other} and {tx}",
                        "Publish",
                    )
                commits[generation] = {
                    "owner": entry.handle,
                    "tx": tx,
                    "writes": list(entry.writes),
                    "values": dict(entry.values),
                    "base": entry.base,
                    "attempts": int(entry.response.get("attempts", 1)),
                }
            elif entry.kind == "reject":
                rejects.append((tx, entry))
            else:
                silent.append((tx, entry))

        base_generation = min(
            [entry.base for entry in transactions.values()] or [0]
        )
        if base_generation < 0:
            raise Rejected("mapping", "negative base_generation")
        # The trace begins after an unmodelled prefix (the store's init commit
        # and anything written before the first observed generation). The
        # model starts at the last prefix generation: for the modelled key
        # universe every prefix version is 0 by definition, so an observation
        # earlier than the prefix end is equivalent to observing the prefix end.
        if commits:
            initial_head = max(0, min(commits) - 1)
        else:
            initial_head = max(0, base_generation)
        self.initial_head = initial_head
        for entry in transactions.values():
            entry.base = max(entry.base, initial_head)
        self.head = initial_head
        # The unmodelled prefix: generations 1..initial_head exist and wrote no
        # key the history models. Declared in verification/REFINEMENT.md.
        for generation in range(1, initial_head + 1):
            self.chain[generation] = {
                "owner": "prefix",
                "tx": "prefix",
                "writes": [],
                "values": {},
                "base": 0,
                "attempts": 0,
            }

        max_generation = max(
            [self.head] + [generation for generation in commits]
        )
        if max_generation > self.bounds["generations"]:
            raise Rejected(
                "bounds",
                f"{max_generation} generations exceeds the declared bound "
                f"{self.bounds['generations']}",
            )
        if any(generation <= initial_head for generation in commits):
            raise Rejected(
                "HeadWellFormed",
                f"a commit claims a generation at or below the unmodelled prefix "
                f"end {initial_head}",
                "Publish",
            )
        if len(transactions) > self.bounds["transactions"]:
            raise Rejected(
                "bounds",
                f"{len(transactions)} transactions exceeds the declared bound "
                f"{self.bounds['transactions']}",
            )
        for entry in transactions.values():
            self.keys.update(entry.writes)
        if len(self.keys) > self.bounds["keys"]:
            raise Rejected(
                "bounds",
                f"{len(self.keys)} keys exceeds the declared bound "
                f"{self.bounds['keys']}",
            )
        committers = {entry.handle for entry in transactions.values()}
        if len(committers) > self.bounds["readers"]:
            raise Rejected(
                "bounds",
                f"{len(committers)} committers exceeds the declared bound "
                f"{self.bounds['readers']}",
            )

        # Start each transaction at the generation it observed. The model's
        # Start is enabled whenever its slot is idle and only reads the head;
        # its position in the canonical order is the publication of the
        # generation the store reports as the observation point.
        def start(tx, entry):
            committer = entry.handle or f"tx:{tx}"
            if entry.base > max_generation:
                raise Rejected(
                    "mapping",
                    f"tx {tx} observed generation {entry.base} above the highest "
                    f"published generation {max_generation}",
                )
            self.phase.setdefault(committer, "idle")
            if self.phase[committer] != "idle":
                raise Rejected(
                    "mapping",
                    f"committer {committer} already has a transaction in flight "
                    f"when tx {tx} starts",
                )
            self.phase[committer] = "prepared"
            self.prepared[committer] = {
                "tx": tx,
                "writes": list(entry.writes),
                "base": entry.base,
            }
            self.counts["starts"] += 1

        # Event placement. Generations order the *state* actions: the commit
        # for generation g is published before anything that observed g. Within
        # one generation the store's own observation timestamps order starts and
        # aborts (a handle is sequential, so its own events are monotone). The
        # synthetic GC observations carry no timestamp and are placed last in
        # their generation, so a prepared transaction is visible to them.
        events = defaultdict(list)

        def add(generation, ns, rank, kind, payload):
            events[int(generation)].append((int(ns), rank, kind, payload))

        # A transaction with no response belongs to a process that died. If
        # its handle is used again later, the death is observable (the model
        # needs CrashPrepared + a new Start); if it is the handle's last
        # transaction, it is kept prepared to the end of the trace, so the
        # LivePreparedDurable fixture can be exercised.
        silent_crash = set()
        for tx, entry in transactions.items():
            if entry.kind is not None:
                continue
            later = [
                other_id
                for other_id, other in transactions.items()
                if other_id != tx
                and (other.handle or f"tx:{other_id}") == (entry.handle or f"tx:{tx}")
                and other.first_line > entry.first_line
            ]
            if later:
                silent_crash.add(tx)
        for tx, entry in transactions.items():
            add(entry.base, entry.invoke_ns, 0, "start", (tx, entry))
            if tx in silent_crash:
                add(entry.base, entry.invoke_ns, 1, "crash", (tx, entry))
        for tx, entry in rejects:
            observed = int(entry.response.get("observed_generation", entry.base))
            add(observed, entry.response_ns, 1, "abort", (tx, entry))
        for record in self.gcs:
            add(record.get("at_generation", self.head), 2**63, 2, "gc", record)
        for record in self.reads:
            add(record.get("read_generation", 0), record.get("observed_ns", 0),
                3, "read", record)

        event_generations = sorted(
            set(range(initial_head, max_generation + 1)) | set(events)
        )
        for generation in event_generations:
            if initial_head < generation <= max_generation:
                self.publish(generation, commits.get(generation))
            for _ns, _rank, kind, payload in sorted(
                events.get(generation, []), key=lambda item: (item[0], item[1])
            ):
                if kind == "start":
                    start(*payload)
                elif kind == "abort":
                    self.revalidate(*payload)
                elif kind == "crash":
                    tx, entry = payload
                    committer = entry.handle or f"tx:{tx}"
                    self.phase[committer] = "idle"
                    self.prepared.pop(committer, None)
                    self.counts["crashed_prepared"] += 1
                    self.notes.append(
                        {"tx": tx, "action": "CrashPrepared", "class": None}
                    )
                elif kind == "gc":
                    self.reclaim(payload)
                else:
                    self.read(payload)
            self.check_invariants()

        # Any generation above the highest read/publish point that carries a
        # promise must still be consistent; the loop above already covered it.
        for generation in range(1, self.head + 1):
            if not self.complete(generation):
                self.violate(
                    "HeadWellFormed",
                    f"generation {generation} has no complete commit",
                )
        # Invariants over the final state.
        self.check_invariants()
        self.check_blind_overwrite(commits)
        self.finish_silent(silent)

    # -- actions -----------------------------------------------------------
    def publish(self, generation, commit):
        if commit is None:
            # A hole: no commit claims this generation while a later one does.
            self.head = max(self.head, generation)
            return
        committer = commit["owner"] or f"tx:{commit['tx']}"
        if self.phase.get(committer) != "prepared":
            raise Rejected(
                "NoLostAck",
                f"tx {commit['tx']} published generation {generation} without a "
                f"Start (phase {self.phase.get(committer)!r})",
                "Publish",
            )
        if self.prepared.get(committer, {}).get("tx") != commit["tx"]:
            raise Rejected(
                "NoLostAck",
                f"tx {commit['tx']} published generation {generation} but "
                f"committer {committer} prepared "
                f"{self.prepared.get(committer, {}).get('tx')!r}",
                "Publish",
            )
        if generation != self.head + 1:
            raise Rejected(
                "HeadWellFormed",
                f"tx {commit['tx']} published generation {generation} while the "
                f"head is {self.head}: generations are not contiguous",
                "Publish",
            )
        observed = self.prepared[committer]
        # Publish requires that every observed version is still current. The
        # observation map is VersionAt(base, key) unless the transaction
        # retried; see check_blind_overwrite for the exact/bounded split.
        attempts = commit["attempts"]
        if attempts <= 1:
            for key in observed["writes"]:
                if self.version_at(generation - 1, key) != self.version_at(
                    observed["base"], key
                ):
                    raise Rejected(
                        "NoBlindOverwrite",
                        f"tx {commit['tx']} published generation {generation} "
                        f"overwriting key {key} at version "
                        f"{self.version_at(generation - 1, key)} while observing "
                        f"version {self.version_at(observed['base'], key)} at "
                        f"generation {observed['base']}",
                        "Publish",
                    )
            self.counts["fully_checked"] += 1
        else:
            conflicting = 0
            for earlier in range(observed["base"] + 1, generation):
                if any(
                    key in observed["writes"] for key in self.writes_at(earlier)
                ):
                    conflicting += 1
            if conflicting > attempts - 1:
                raise Rejected(
                    "NoBlindOverwrite",
                    f"tx {commit['tx']} published generation {generation} after "
                    f"{conflicting} conflicting generations with only "
                    f"{attempts} attempts; the last refresh cannot cover them",
                    "Publish",
                )
            self.counts["bounded_checked"] += 1
            self.counts["abstracted_retries"] += attempts - 1
        self.chain[generation] = {
            "owner": committer,
            "tx": commit["tx"],
            "writes": list(commit["writes"]),
            "values": dict(commit["values"]),
            "base": commit["base"],
            "attempts": attempts,
        }
        self.head = generation
        self.durable.update(commit["writes"])
        self.phase[committer] = "publishing"
        self.counts["publishes"] += 1
        # The publication returns: Ack.
        self.phase[committer] = "done"
        self.acked[committer] = generation
        self.prepared.pop(committer, None)
        self.counts["acks"] += 1
        # The model's Release action: the slot returns to idle so the same
        # committer can publish again. Added to the model by T100.
        self.phase[committer] = "idle"
        self.counts["releases"] += 1

    def revalidate(self, tx, entry):
        committer = entry.handle or f"tx:{tx}"
        if self.phase.get(committer) != "prepared":
            raise Rejected(
                "mapping",
                f"tx {tx} aborted without a prepared state",
                "Revalidate",
            )
        observed = self.prepared[committer]
        base = int(entry.base)
        resolution = int(entry.response.get("observed_generation", base))
        klass = entry.response.get("class")
        conflict = False
        for generation in range(base + 1, resolution + 1):
            if any(key in observed["writes"] for key in self.writes_at(generation)):
                conflict = True
                break
        if klass == "Conflict" and not conflict:
            # A create precondition ("the entry must be absent") is a guard the
            # model does not have: the model's transaction writes a key set and
            # has no read-modify-write assumption. Such an abort is counted as
            # abstracted, never silently accepted as a version conflict.
            text = str(entry.response.get("error", ""))
            if entry.create_preconditions and (
                "NameExists" in text or "ObjectExists" in text
            ):
                self.counts["precondition_aborts"] += 1
                self.notes.append(
                    {"tx": tx, "action": "Start", "class": "precondition"}
                )
                self.phase[committer] = "idle"
                self.prepared.pop(committer, None)
                return
            raise Rejected(
                "NoBlindOverwrite",
                f"tx {tx} aborted with a Conflict but no modelled write touched "
                f"its keys in ({base}, {resolution}]",
                "Revalidate",
            )
        if klass == "Conflict":
            self.counts["conflict_aborts"] += 1
        else:
            # Busy/quota/capability/id-reuse: guards the model does not have.
            self.counts["abstracted_aborts"] += 1
        self.notes.append(
            {
                "tx": tx,
                "action": "Revalidate" if klass == "Conflict" else "CrashPrepared",
                "class": klass,
            }
        )
        self.phase[committer] = "idle"
        self.prepared.pop(committer, None)

    def reclaim(self, record):
        keys = record.get("deleted") or []
        for key in keys:
            if not isinstance(key, str):
                raise Rejected("mapping", f"gc-observe with a non-string key {key!r}")
            self.deleted.add(key)
            self.counts["gc_reclaims"] += 1

    def read(self, record):
        generation = int(record.get("read_generation", 0))
        reader = record.get("handle") or "unknown"
        if record.get("error") is not None:
            # A failed read observed the generation it reports and returned
            # nothing; it is still a Read action (the model has no error path).
            pass
        self.read_head[reader] = max(self.read_head.get(reader, 0), generation)
        self.counts["reads"] += 1

    def finish_silent(self, silent):
        for tx, entry in silent:
            committer = entry.handle or f"tx:{tx}"
            if self.phase.get(committer) == "prepared":
                self.phase[committer] = "idle"
                self.prepared.pop(committer, None)
                self.counts["crashed_prepared"] += 1
                self.notes.append({"tx": tx, "action": "CrashPrepared", "class": None})

    def check_blind_overwrite(self, commits):
        # Generation order already fixed the chain; NoBlindOverwrite is checked
        # at Publish. This final pass reports the *coverage* split plainly.
        return


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------


def analyse(path, bounds):
    records = load(path)
    by_domain = OrderedDict()
    for record in records:
        by_domain.setdefault(domain_of(record), []).append(record)
    report = {
        "history": path,
        "records": len(records),
        "domains": [],
        "accepted": True,
        "rejections": [],
        "counts": {},
    }
    totals = defaultdict(int)
    for domain, domain_records in by_domain.items():
        transactions, reads, gcs = ingest(domain_records)
        instance = Instance(domain, transactions, reads, gcs, bounds)
        try:
            instance.replay()
        except Rejected as rejection:
            report["accepted"] = False
            report["rejections"].append(
                {
                    "domain": domain,
                    "invariant": rejection.invariant,
                    "action": rejection.action,
                    "detail": rejection.detail,
                }
            )
            continue
        if instance.violations:
            report["accepted"] = False
            for violation in instance.violations:
                report["rejections"].append({"domain": domain, **violation})
            continue
        report["domains"].append(
            {
                "domain": domain or "(global)",
                "transactions": len(transactions),
                "committed": sum(
                    1 for entry in transactions.values() if entry.kind == "commit"
                ),
                "rejected": sum(
                    1 for entry in transactions.values() if entry.kind == "reject"
                ),
                "silent": sum(
                    1 for entry in transactions.values() if entry.kind is None
                ),
                "reads": len(reads),
                "gc_observations": len(gcs),
                "head": instance.head,
                "keys": len(instance.keys),
                "checks": dict(instance.counts),
                "abstracted": instance.notes,
            }
        )
        for name, value in instance.counts.items():
            totals[name] += value
    report["counts"] = dict(totals)
    report["invariants"] = sorted(
        {violation["invariant"] for violation in report["rejections"]}
    )
    report["checked"] = [
        "HeadWellFormed",
        "NoPartial",
        "NoLostAck",
        "NoBlindOverwrite",
        "CommittedBlobsDurable",
        "LivePreparedDurable",
        "ReaderIsAPrefix",
        "GcNeverReclaimsReachable",
    ]
    report["not_checked"] = [
        "TypeOK (holds by construction of the replay)",
        "CommitOrderIsStable (chain indices are generations)",
        "liveness, starvation, fairness, retry-count liveness",
        "any cross-domain (MW3) property",
        "value-level read consistency (verifier/history_check.py owns it)",
        "blind overwrite of keys that existed before the trace (unmodelled prefix)",
        "the retry action subset: an exactly-observed refresh point for attempts > 1",
    ]
    return report


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("history")
    parser.add_argument("--expect", choices=["accept", "reject"], default="accept")
    parser.add_argument("--expect-invariant", default=None)
    parser.add_argument("--out", default=None)
    parser.add_argument(
        "--bound",
        action="append",
        default=[],
        metavar="NAME=N",
        help="override a declared bound (transactions, keys, generations, readers)",
    )
    args = parser.parse_args()

    bounds = dict(DEFAULT_BOUNDS)
    for item in args.bound:
        name, _, value = item.partition("=")
        if name not in bounds:
            raise SystemExit(f"unknown bound {name!r}")
        bounds[name] = int(value)
    try:
        report = analyse(args.history, bounds)
    except Rejected as rejection:
        report = {
            "history": args.history,
            "accepted": False,
            "rejections": [
                {
                    "domain": None,
                    "invariant": rejection.invariant,
                    "action": rejection.action,
                    "detail": rejection.detail,
                }
            ],
            "invariants": [rejection.invariant],
            "counts": {},
        }
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(report, handle, indent=2, sort_keys=True)
            handle.write("\n")
    print(json.dumps(report, sort_keys=True))

    if args.expect == "accept":
        return 0 if report["accepted"] else 1
    if report["accepted"]:
        return 1
    if args.expect_invariant and args.expect_invariant not in report.get(
        "invariants", []
    ):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
