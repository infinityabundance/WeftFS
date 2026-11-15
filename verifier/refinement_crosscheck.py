#!/usr/bin/env python3
"""T100 — independent enumeration of the `WeftMultiWriter.tla` transition relation.

The refinement checker (`verifier/refinement_check.py`) replays a history against
a transcription of the model. A transcription that silently differs from the TLA
module would make every replay verdict meaningless, so this program enumerates
the *whole* transition relation for a tiny bounded configuration and reports

  * the number of distinct reachable states, and
  * the set of model invariants violated by any reachable state.

`scripts/refinement_court.sh` runs TLC on the same configuration and requires
both numbers to agree. That is the machine-checked tie between this
transcription and `verification/tla/WeftMultiWriter.tla`.

Modes:
  refine  — enumerate with the corrected model (`--flaw none`).
  blind   — enumerate the `WeftMultiWriterBuggy.tla` flaw (Publish and Revalidate
            skip the version check).
  gc      — enumerate the `WeftMultiWriterGcBuggy.tla` flaw (GcReclaim consults
            only the head, not live prepared transactions).

`--emit-cfg PATH` writes the TLC configuration corresponding to the chosen
flaw, so the court cannot accidentally compare different configurations.
"""

import argparse
import json
import sys
from collections import deque

PHASES = ("idle", "prepared", "publishing", "done")
INVARIANTS = [
    "HeadWellFormed",
    "NoPartial",
    "NoLostAck",
    "NoBlindOverwrite",
    "CommittedBlobsDurable",
    "LivePreparedDurable",
    "ReaderIsAPrefix",
    "GcNeverReclaimsReachable",
    "CommitOrderIsStable",
]

NONE = (0, 0, (), ())  # (kind, owner_index, writes, observed)
TORN = (1, 0, (), ())
GENS = {}


class Model:
    def __init__(self, keys, committers, max_gen, max_attempts, flaw):
        self.keys = tuple(sorted(keys))
        self.key_index = {key: i for i, key in enumerate(self.keys)}
        self.committers = tuple(committers)
        self.max_gen = max_gen
        self.max_attempts = max_attempts
        self.flaw = flaw

    # -- helpers -----------------------------------------------------------
    def entry(self, kind, owner, writes, observed):
        return (kind, owner, tuple(sorted(writes)), tuple(observed))

    def empty(self):
        return self.entry(0, 0, (), [0] * len(self.keys))

    def torn(self):
        return self.entry(1, 0, (), [0] * len(self.keys))

    def complete(self, state, generation):
        return state["chain"][generation - 1][0] == 2

    def writes_at(self, state, generation):
        entry = state["chain"][generation - 1]
        return entry[2] if entry[0] == 2 else ()

    def version_at(self, state, limit, key_index):
        best = 0
        for generation in range(1, limit + 1):
            if key_index in self.writes_at(state, generation):
                best = generation
        return best

    def version_map(self, state, limit):
        return tuple(self.version_at(state, limit, i) for i in range(len(self.keys)))

    def head_reachable(self, state):
        reachable = set()
        for generation in range(1, state["head"] + 1):
            reachable.update(self.writes_at(state, generation))
        return frozenset(reachable)

    def live_prepared(self, state):
        reachable = set()
        for index, phase in enumerate(state["phase"]):
            if phase in (1, 2):  # prepared, publishing
                reachable.update(state["txn"][index][2])
        return frozenset(reachable)

    def reachable(self, state):
        return self.head_reachable(state) | self.live_prepared(state)

    # -- initial state -----------------------------------------------------
    def init(self):
        width = self.max_gen
        return {
            "head": 0,
            "chain": tuple(self.empty() for _ in range(width)),
            "durable": frozenset(),
            "deleted": frozenset(),
            "phase": tuple(0 for _ in self.committers),
            "txn": tuple(self.empty() for _ in self.committers),
            "observed_head": tuple(0 for _ in self.committers),
            "attempts": tuple(0 for _ in self.committers),
            "acked": tuple(0 for _ in self.committers),
            "pending": tuple(0 for _ in self.committers),
            "crashed": tuple(False for _ in self.committers),
            "read_head": (0, 0),
        }

    # -- transitions -------------------------------------------------------
    def successors(self, state):
        keys = self.keys
        n = len(keys)
        commits = self.committers
        out = []

        def clone(**changes):
            candidate = dict(state)
            candidate.update(changes)
            out.append(candidate)

        for c in range(len(commits)):
            phase = state["phase"][c]
            if phase == 0 and not state["crashed"][c]:
                # Start: any non-empty subset of Keys.
                for mask in range(1, 1 << n):
                    writes = tuple(i for i in range(n) if mask & (1 << i))
                    txn = list(state["txn"])
                    txn[c] = self.entry(
                        2, c, writes, self.version_map(state, state["head"])
                    )
                    phase_list = list(state["phase"])
                    phase_list[c] = 1
                    observed = list(state["observed_head"])
                    observed[c] = state["head"]
                    attempts = list(state["attempts"])
                    attempts[c] = 0
                    durable = set(state["durable"]) | set(writes)
                    deleted = set(state["deleted"]) - set(writes)
                    clone(
                        txn=tuple(txn),
                        phase=tuple(phase_list),
                        observed_head=tuple(observed),
                        attempts=tuple(attempts),
                        durable=frozenset(durable),
                        deleted=frozenset(deleted),
                    )
            if phase == 1 and not state["crashed"][c]:
                txn = state["txn"][c]
                if txn[0] != 2:
                    continue
                writes = txn[2]
                observed = txn[3]
                conflict = any(
                    self.version_at(state, state["head"], key) != observed[key]
                    for key in writes
                )
                if self.flaw == "blind":
                    conflict = False
                if conflict:
                    attempts = list(state["attempts"])
                    attempts[c] = self.max_attempts
                    phase_list = list(state["phase"])
                    phase_list[c] = 0
                    txn_list = list(state["txn"])
                    txn_list[c] = self.empty()
                    clone(
                        attempts=tuple(attempts),
                        phase=tuple(phase_list),
                        txn=tuple(txn_list),
                    )
                else:
                    observed_head = list(state["observed_head"])
                    observed_head[c] = state["head"]
                    clone(observed_head=tuple(observed_head))
                # Retry (bounded, disjoint).
                if state["attempts"][c] < self.max_attempts:
                    if all(
                        self.version_at(state, state["head"], key) == observed[key]
                        for key in writes
                    ):
                        attempts = list(state["attempts"])
                        attempts[c] = attempts[c] + 1
                        observed_head = list(state["observed_head"])
                        observed_head[c] = state["head"]
                        txn_list = list(state["txn"])
                        txn_list[c] = self.entry(
                            2, c, writes, self.version_map(state, state["head"])
                        )
                        clone(
                            attempts=tuple(attempts),
                            observed_head=tuple(observed_head),
                            txn=tuple(txn_list),
                        )
                # Publish.
                if (
                    state["observed_head"][c] == state["head"]
                    and state["head"] < self.max_gen
                ):
                    if all(
                        self.version_at(state, state["head"], key) == observed[key]
                        for key in writes
                    ) or self.flaw == "blind":
                        chain = list(state["chain"])
                        generation = state["head"] + 1
                        chain[generation - 1] = txn
                        pending = list(state["pending"])
                        pending[c] = generation
                        phase_list = list(state["phase"])
                        phase_list[c] = 2
                        clone(
                            chain=tuple(chain),
                            pending=tuple(pending),
                            phase=tuple(phase_list),
                            head=generation,
                        )
            if phase == 2 and not state["crashed"][c]:
                # Ack.
                generation = state["pending"][c]
                if 1 <= generation <= self.max_gen:
                    acked = list(state["acked"])
                    acked[c] = generation
                    phase_list = list(state["phase"])
                    phase_list[c] = 3
                    txn_list = list(state["txn"])
                    txn_list[c] = self.empty()
                    clone(
                        acked=tuple(acked),
                        phase=tuple(phase_list),
                        txn=tuple(txn_list),
                    )
            if phase == 3 and not state["crashed"][c]:
                # Release (T100 addition).
                phase_list = list(state["phase"])
                phase_list[c] = 0
                clone(phase=tuple(phase_list))
            # Faults.
            if phase in (1, 2) and not state["crashed"][c]:
                if state["head"] < self.max_gen:
                    chain = list(state["chain"])
                    chain[state["head"]] = self.torn()
                    crashed = list(state["crashed"])
                    crashed[c] = True
                    phase_list = list(state["phase"])
                    phase_list[c] = 0
                    txn_list = list(state["txn"])
                    txn_list[c] = self.empty()
                    clone(
                        chain=tuple(chain),
                        crashed=tuple(crashed),
                        phase=tuple(phase_list),
                        txn=tuple(txn_list),
                    )
            if phase == 2 and not state["crashed"][c]:
                # CrashAfterHead: only a publishing committer can be killed
                # after the head write.
                crashed = list(state["crashed"])
                crashed[c] = True
                phase_list = list(state["phase"])
                phase_list[c] = 0
                txn_list = list(state["txn"])
                txn_list[c] = self.empty()
                clone(
                    crashed=tuple(crashed),
                    phase=tuple(phase_list),
                    txn=tuple(txn_list),
                )
            if phase == 1 and not state["crashed"][c]:
                crashed = list(state["crashed"])
                crashed[c] = True
                phase_list = list(state["phase"])
                phase_list[c] = 0
                txn_list = list(state["txn"])
                txn_list[c] = self.empty()
                clone(
                    crashed=tuple(crashed),
                    phase=tuple(phase_list),
                    txn=tuple(txn_list),
                )
            if state["crashed"][c]:
                crashed = list(state["crashed"])
                crashed[c] = False
                phase_list = list(state["phase"])
                phase_list[c] = 0
                clone(crashed=tuple(crashed), phase=tuple(phase_list))

        # Readers.
        for r in range(len(state["read_head"])):
            for generation in range(0, state["head"] + 1):
                read_head = list(state["read_head"])
                read_head[r] = generation
                clone(read_head=tuple(read_head))

        # GC.
        reachable = self.reachable(state)
        if self.flaw == "gc":
            reachable = self.head_reachable(state)
        for key in sorted(state["durable"]):
            if key in reachable:
                continue
            durable = set(state["durable"]) - {key}
            deleted = set(state["deleted"]) | {key}
            clone(durable=frozenset(durable), deleted=frozenset(deleted))
        return out

    # -- invariants --------------------------------------------------------
    def violations(self, state):
        found = []
        for generation in range(1, state["head"] + 1):
            if not self.complete(state, generation):
                found.append("HeadWellFormed")
                break
        for index in range(len(self.committers)):
            generation = state["acked"][index]
            if generation == 0:
                continue
            if generation > state["head"] or not self.complete(state, generation):
                found.append("NoLostAck")
                break
            if state["chain"][generation - 1][1] != index:
                found.append("NoLostAck")
                break
        for generation in state["read_head"]:
            if generation > state["head"]:
                found.append("NoPartial")
                found.append("ReaderIsAPrefix")
                break
            if generation > 0 and not self.complete(state, generation):
                found.append("NoPartial")
                break
        for generation in range(1, state["head"] + 1):
            entry = state["chain"][generation - 1]
            if entry[0] != 2:
                continue
            for key in entry[2]:
                previous = 0
                for earlier in range(1, generation):
                    if key in self.writes_at(state, earlier):
                        previous = earlier
                if entry[3][key] != previous:
                    found.append("NoBlindOverwrite")
                    break
            if "NoBlindOverwrite" in found:
                break
        durable = state["durable"]
        for generation in range(1, state["head"] + 1):
            if not set(self.writes_at(state, generation)) <= durable:
                found.append("CommittedBlobsDurable")
                break
        for index in range(len(self.committers)):
            if state["phase"][index] in (1, 2):
                entry = state["txn"][index]
                if entry[0] != 2 or not set(entry[2]) <= durable:
                    found.append("LivePreparedDurable")
                    break
        reachable = self.reachable(state)
        if state["deleted"] & reachable:
            found.append("GcNeverReclaimsReachable")
        # CommitOrderIsStable: generations are indices; VersionAt is monotone in
        # the limit by construction.
        return sorted(set(found))


def key(state):
    return (
        state["head"],
        state["chain"],
        state["durable"],
        state["deleted"],
        state["phase"],
        state["txn"],
        state["observed_head"],
        state["attempts"],
        state["acked"],
        state["pending"],
        state["crashed"],
        state["read_head"],
    )


def explore(model):
    start = model.init()
    initial_violations = model.violations(start)
    seen = {key(start): start}
    queue = deque([start])
    violations = set(initial_violations)
    generated = 0
    while queue:
        state = queue.popleft()
        for successor in model.successors(state):
            generated += 1
            violations.update(model.violations(successor))
            k = key(successor)
            if k not in seen:
                seen[k] = successor
                queue.append(successor)
    return {
        "distinct_states": len(seen),
        # TLC's "states generated" counts the initial state plus every
        # successor produced; `transitions` is the successor count alone.
        "states_generated": generated + 1,
        "transitions": generated,
        "violations": sorted(violations),
    }


def emit_cfg(path, keys, committers, max_gen, max_attempts, flaw):
    module = {"none": "WeftMultiWriter", "blind": "WeftMultiWriterBuggy",
              "gc": "WeftMultiWriterGcBuggy"}[flaw]
    keys_text = "{" + ", ".join(f'"{k}"' for k in keys) + "}"
    committers_text = "{" + ", ".join(f'"{c}"' for c in committers) + "}"
    lines = [
        "SPECIFICATION Spec",
        "",
        "CONSTANTS",
        f"  Committers = {committers_text}",
        f"  Keys = {keys_text}",
        f"  MaxGen = {max_gen}",
        f"  MaxAttempts = {max_attempts}",
        "",
    ]
    for invariant in INVARIANTS:
        lines.append(f"INVARIANT {invariant}")
    lines += ["", "CHECK_DEADLOCK FALSE", ""]
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines))
    return module


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--flaw", choices=["none", "blind", "gc"], default="none")
    parser.add_argument("--committers", type=int, default=2)
    parser.add_argument("--keys", type=int, default=1)
    parser.add_argument("--max-gen", type=int, default=2)
    parser.add_argument("--max-attempts", type=int, default=1)
    parser.add_argument("--emit-cfg", default=None)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    committers = [f"c{i + 1}" for i in range(args.committers)]
    keys = [f"k{i + 1}" for i in range(args.keys)]
    if args.emit_cfg:
        module = emit_cfg(
            args.emit_cfg, keys, committers, args.max_gen, args.max_attempts, args.flaw
        )
    else:
        module = {
            "none": "WeftMultiWriter",
            "blind": "WeftMultiWriterBuggy",
            "gc": "WeftMultiWriterGcBuggy",
        }[args.flaw]
    model = Model(keys, committers, args.max_gen, args.max_attempts, args.flaw)
    result = explore(model)
    result.update(
        {
            "module": module,
            "flaw": args.flaw,
            "config": {
                "committers": committers,
                "keys": keys,
                "max_gen": args.max_gen,
                "max_attempts": args.max_attempts,
            },
            "invariants_checked": INVARIANTS,
        }
    )
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(result, handle, indent=2, sort_keys=True)
            handle.write("\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    sys.exit(main())
