#!/usr/bin/env python3
"""Independent transactional history checker for WeftFS MW2/MW3 (T026).

It consumes the JSON Lines history the store emits when `WEFT_HISTORY` (or
`Options::history_path`) is set, and decides properties of the *contract*, not
of an implementation. It shares no code with the store: it does not import
WeftFS, call its binary, or read its state.

**What is checked** (state this precisely; the contract does not promise more):

1. `generation_unique` — within one conflict domain, one generation names
   exactly one commit. Two different transactions that both claim generation
   *g* with different ops are a violation. (MW2 guarantee P1, MW3 §15.7.)
2. Register linearizability per key — for every key in a domain, the successful
   writes define the value at each generation (`None` for a delete). Every read
   must return the value of the latest write whose generation is `<=` the
   generation the read observed. A read that returns a value from an older
   generation (`stale_read`), a value never written (`phantom_value`), or a
   value when nothing was ever written (`read_before_any_write`) is a violation.
   Because the store's publication order *is* the linearization order (one CAS
   on the head; per-domain heads in MW3), "latest write with generation <= g" is
   the whole register check, not a search for an ordering.
3. Multi-key atomicity, per domain — every key a committed transaction wrote
   carries that transaction's generation, and a read that observes generation
   *g* sees the state after *exactly* the commits up to *g*. A multi-key read at
   one generation therefore cannot see half of a transaction
   (`partial_transaction`).
4. Conflict justification — a rejected transaction must name a conflict that is
   explained by the history: some key it wrote changed in
   `(base_generation, observed_generation]`, or a create found the key already
   taken at the base generation. An unexplained typed conflict is a violation.
5. Handle monotonicity — one handle's observed generation never decreases.

**What is deliberately not checked**: global serializability across MW3
conflict domains, cross-domain snapshot isolation, a single global
linearization point, or any property of the physical store below the commit
record. Cross-domain disagreement is *expected* and is not flagged; the checker
groups reads and writes by domain and never compares across them. This is the
declared isolation sentence of `spec/WEFT-MULTIWRITER.md` §15.4.

Usage:
  history_check.py HISTORY.jsonl [--expect accept|reject] [--expect-class NAME]
                                 [--out REPORT.json]
"""

import argparse
import json
import sys
from collections import defaultdict

SCHEMA = "weft.history.v1"
CONFLICT_CLASSES = {
    "NameExists",
    "ObjectExists",
    "ObjectChanged",
    "ObjectAbsent",
    "EntryChanged",
    "RelationConflict",
    "SnapshotConflict",
    "TipChanged",
    "TransactionIdReused",
    "ChildRace",
}


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
                raise SystemExit(f"{path}:{number}: not JSON: {err}")
            if record.get("schema") != SCHEMA:
                raise SystemExit(f"{path}:{number}: schema is not {SCHEMA}")
            records.append(record)
    return records


def domain_of(record):
    return record.get("domain") or ""


class Checker:
    def __init__(self, path, records):
        self.path = path
        self.records = records
        self.violations = []
        self.invokes = {}
        self.responses = []
        self.reads = []
        self.commit_sets = defaultdict(dict)  # (domain, generation) -> {key: value}
        self.commit_owner = {}
        self.writes = defaultdict(list)  # (domain, key) -> [(generation, value, tx)]
        self.handle_generation = {}
        self.counts = defaultdict(int)

    def violate(self, kind, detail):
        self.violations.append({"kind": kind, "detail": detail})

    def run(self):
        self.collect()
        self.check_generation_uniqueness()
        self.check_reads()
        self.check_conflicts()
        self.check_handle_monotonicity()
        return self.report()

    # -- ingestion ---------------------------------------------------------
    def collect(self):
        for record in self.records:
            event = record.get("event")
            domain = domain_of(record)
            if event == "tx-invoke":
                self.invokes[(domain, record["tx"])] = record
                self.counts["transactions"] += 1
                self.counts["ops"] += len(record.get("ops") or [])
            elif event == "tx-response":
                self.responses.append(record)
                if record.get("outcome") in ("committed", "idempotent"):
                    self.counts["committed"] += 1
                else:
                    self.counts["rejected"] += 1
            elif event == "read-response":
                if record.get("value") is not None or record.get("error") is None:
                    self.reads.append(record)
                self.counts["reads"] += 1

        # Build the write set from committed transactions only.
        for response in self.responses:
            if response.get("outcome") not in ("committed", "idempotent"):
                continue
            domain = domain_of(response)
            key = (domain, response["tx"])
            invoke = self.invokes.get(key)
            if invoke is None:
                self.violate(
                    "missing_invoke",
                    f"transaction {response['tx']} responded without an invoke",
                )
                continue
            generation = response["generation"]
            slot = self.commit_sets[(domain, generation)]
            owner = self.commit_owner.get((domain, generation))
            if owner is not None and owner != response["tx"]:
                self.violate(
                    "generation_conflict",
                    f"domain {domain or '(global)'} generation {generation} is claimed by "
                    f"{owner} and {response['tx']}",
                )
                continue
            self.commit_owner[(domain, generation)] = response["tx"]
            for op in invoke.get("ops") or []:
                value = op.get("value")
                previous = slot.get(op["key"], "__absent__")
                if previous not in ("__absent__", value):
                    self.violate(
                        "generation_conflict",
                        f"domain {domain or '(global)'} generation {generation} writes key "
                        f"{op['key']} twice with different values",
                    )
                slot[op["key"]] = value
                entry = (generation, value, response["tx"])
                bucket = self.writes[(domain, op["key"])]
                if entry not in bucket:
                    bucket.append(entry)
        for bucket in self.writes.values():
            bucket.sort(key=lambda item: item[0])
        self.counts["keys"] = len(self.writes)
        self.counts["domains"] = len({domain for domain, _ in self.writes})

    # -- checks ------------------------------------------------------------
    def check_generation_uniqueness(self):
        # Already reported while building the commit sets. This method exists so
        # the check has a named step; nothing further is needed.
        return

    def value_at(self, domain, key, generation):
        latest = "__none__"
        for committed_generation, value, _tx in self.writes.get((domain, key), []):
            if committed_generation <= generation:
                latest = value
            else:
                break
        return latest

    def check_reads(self):
        multi = set()
        for read in self.reads:
            if read.get("error") is not None:
                continue
            domain = domain_of(read)
            key = read["key"]
            generation = read.get("read_generation", 0)
            observed = read.get("value")
            expected = self.value_at(domain, key, generation)
            if expected == "__none__":
                if observed is not None:
                    self.violate(
                        "read_before_any_write",
                        f"domain {domain or '(global)'} key {key} read at generation "
                        f"{generation} returned a value, but no write had happened",
                    )
                continue
            if observed is None and expected is not None:
                self.violate(
                    "stale_read",
                    f"domain {domain or '(global)'} key {key} read at generation "
                    f"{generation} returned absent, expected {expected}",
                )
            elif observed is not None and expected is None:
                self.violate(
                    "phantom_value",
                    f"domain {domain or '(global)'} key {key} read at generation "
                    f"{generation} returned {observed}, expected a delete",
                )
            elif observed != expected:
                earlier = any(
                    value == observed for committed_generation, value, _tx in
                    self.writes.get((domain, key), [])
                    if committed_generation < generation
                )
                self.violate(
                    "stale_read" if earlier else "phantom_value",
                    f"domain {domain or '(global)'} key {key} read at generation "
                    f"{generation} returned {observed}, expected {expected}",
                )
            multi.add((domain, read.get("handle"), generation))
        # A multi-key generation is one where a handle read more than one key at
        # one generation. Record how many were exercised; the court asserts the
        # coverage separately, the checker just reports it.
        grouped = defaultdict(list)
        for read in self.reads:
            if read.get("error") is None:
                grouped[(domain_of(read), read.get("handle"), read.get("read_generation"))].append(
                    read["key"]
                )
        self.counts["multi_key_generations"] = sum(
            1 for keys in grouped.values() if len(set(keys)) > 1
        )

    def check_conflicts(self):
        for response in self.responses:
            if response.get("outcome") != "rejected":
                continue
            klass = response.get("class")
            if klass not in CONFLICT_CLASSES:
                continue
            domain = domain_of(response)
            invoke = self.invokes.get((domain, response["tx"]))
            if invoke is None:
                continue
            base = invoke.get("base_generation", 0)
            observed = response.get("observed_generation", base)
            keys = [op["key"] for op in invoke.get("ops") or []]
            justified = False
            for key in keys:
                for generation, _value, _tx in self.writes.get((domain, key), []):
                    if base < generation <= observed:
                        justified = True
                        break
                if justified:
                    break
            if not justified:
                # A create that found the key already taken at the base
                # generation is also an explained conflict.
                preconditions = {
                    pre.get("label"): pre.get("expected")
                    for pre in invoke.get("preconditions") or []
                }
                if preconditions.get("entry", {}).get("kind") == "absent":
                    for key in keys:
                        if self.value_at(domain, key, base) not in ("__none__", None):
                            justified = True
                            break
            if not justified:
                self.violate(
                    "unexplained_conflict",
                    f"transaction {response['tx']} reported {klass} in domain "
                    f"{domain or '(global)'} with no write in ({base}, {observed}]",
                )

    def check_handle_monotonicity(self):
        # Generations are per domain in MW3, so monotonicity is per
        # (domain, handle): a handle's read view is a vector, and one domain may
        # legitimately be behind another. Comparing across domains would be the
        # global-order claim the contract explicitly does not make.
        for read in self.reads:
            handle = read.get("handle")
            generation = read.get("read_generation", 0)
            if handle is None:
                continue
            slot = (domain_of(read), handle)
            previous = self.handle_generation.get(slot)
            if previous is not None and generation < previous:
                self.violate(
                    "handle_generation_regression",
                    f"handle {handle} read generation {generation} after {previous} "
                    f"in domain {slot[0] or '(global)'}",
                )
            self.handle_generation[slot] = max(previous or 0, generation)

    def report(self):
        return {
            "history": self.path,
            "ok": not self.violations,
            "records": len(self.records),
            "transactions": self.counts.get("transactions", 0),
            "committed": self.counts.get("committed", 0),
            "rejected": self.counts.get("rejected", 0),
            "reads": self.counts.get("reads", 0),
            "keys": self.counts.get("keys", 0),
            "domains": self.counts.get("domains", 0),
            "ops": self.counts.get("ops", 0),
            "multi_key_generations": self.counts.get("multi_key_generations", 0),
            "violations": self.violations,
            "checked": [
                "generation_unique",
                "per_key_register_linearizability",
                "per_domain_multi_key_atomicity",
                "conflict_justification",
                "handle_monotonicity",
            ],
            "not_checked": [
                "global serializability across MW3 domains",
                "cross-domain snapshot isolation",
                "a single global linearization point",
            ],
        }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("history")
    parser.add_argument("--expect", choices=["accept", "reject"], default="accept")
    parser.add_argument("--expect-class", default=None)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    report = Checker(args.history, load(args.history)).run()
    kinds = sorted({violation["kind"] for violation in report["violations"]})
    report["violation_kinds"] = kinds
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(report, handle, indent=2, sort_keys=True)
            handle.write("\n")
    print(json.dumps(report, sort_keys=True))

    if args.expect == "accept":
        return 0 if report["ok"] else 1
    if not kinds:
        return 1
    if args.expect_class and args.expect_class not in kinds:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
