#!/usr/bin/env python3
"""Append the MW3/T021/T027 numbers and blocks to docs/perf-numbers.json.

Run once from the repository root; idempotent (removes an existing `mw3` group
and every `mw3.*` number/block first).
"""
import json
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PATH = os.path.join(ROOT, "docs", "perf-numbers.json")

MW3 = "bench/receipts/mw3-20261114T025943Z.json"
RETRY = "bench/receipts/mw2-retry-20261114T025943Z.json"
SCALE = "bench/receipts/writer-scale-20261114T025943Z.json"
STAMP = "20261114T025943Z"
REVISION = "d2a8b7e1993c62ea10167c8a9fb6fe7a599cb541"

data = json.load(open(PATH))
data["groups"] = [g for g in data["groups"] if g["id"] != "mw3"]
data["numbers"] = [n for n in data["numbers"] if not n["key"].startswith("mw3.")]
data["blocks"] = [b for b in data["blocks"] if not b["id"].startswith("mw3-")]

data["groups"].append({
    "id": "mw3",
    "title": "MW3 conflict domains, the T021 rebase and the T027 writer envelope "
             "(stamp 20261114T025943Z, revision d2a8b7e1)",
})

NUMBERS = []


def add(key, label, unit, fmt, receipt, path=None, selector=None):
    entry = {
        "key": key,
        "label": label,
        "unit": unit,
        "group": "mw3",
        "format": fmt,
        "receipt": receipt,
        "stamp_path": "stamp",
        "stamp": STAMP,
        "revision_path": "revisions.WeftFS",
        "revision": REVISION,
    }
    if selector is not None:
        entry["list"] = selector.get("list", "rows")
        entry["where"] = selector["where"]
        if selector.get("path") is not None:
            entry["select_path"] = selector["path"]
    else:
        entry["path"] = path
    NUMBERS.append(entry)


MW3_SELECT = lambda **where: {"list": "rows", "where": where}

for n in [1, 2, 4, 8, 16, 32]:
    sel = dict(distribution="disjoint", n=n)
    add(f"mw3.d{n}.tx_s", f"MW3 disjoint N={n} aggregate throughput", "tx/s", "fixed:0",
        MW3, selector=dict(where=sel, path="throughput_tx_s"))
    add(f"mw3.d{n}.eff", f"MW3 disjoint N={n} scaling efficiency", "", "ratio:3",
        MW3, selector=dict(where=sel, path="scaling_efficiency"))
    add(f"mw3.d{n}.p99", f"MW3 disjoint N={n} p99", "ms", "fixed:3",
        MW3, selector=dict(where=sel, path="p99_ms"))
    add(f"mw3.d{n}.rpc", f"MW3 disjoint N={n} backend reads per commit", "", "fixed:2",
        MW3, selector=dict(where=sel, path="reads_per_commit"))
    add(f"mw3.d{n}.committed", f"MW3 disjoint N={n} committed", "transactions", "int",
        MW3, selector=dict(where=sel, path="committed"))
    add(f"mw3.d{n}.gap", f"MW3 disjoint N={n} intervening commits read", "commits", "int",
        MW3, selector=dict(where=sel, path="gap_commits_read"))

for n in [2, 8, 32]:
    sel = dict(distribution="same-domain", n=n)
    add(f"mw3.c{n}.requested", f"MW3 same-domain N={n} requested", "transactions", "int",
        MW3, selector=dict(where=sel, path="requested"))
    add(f"mw3.c{n}.committed", f"MW3 same-domain N={n} committed", "transactions", "int",
        MW3, selector=dict(where=sel, path="committed"))
    add(f"mw3.c{n}.conflicts", f"MW3 same-domain N={n} typed conflicts", "conflicts", "int",
        MW3, selector=dict(where=sel, path="conflicts"))
    add(f"mw3.c{n}.p99", f"MW3 same-domain N={n} p99 (conflict path)", "ms", "fixed:3",
        MW3, selector=dict(where=sel, path="p99_ms"))
    NUMBERS.append({
        "key": f"mw3.c{n}.rate",
        "label": f"MW3 same-domain N={n} conflict rate",
        "unit": "",
        "group": "mw3",
        "format": "ratio:4",
        "expr": "{mw3.c%d.conflicts} / {mw3.c%d.requested}" % (n, n),
        "receipt": MW3,
        "stamp_path": "stamp",
        "stamp": STAMP,
        "revision_path": "revisions.WeftFS",
        "revision": REVISION,
    })

add("mw3.retry.before_rpc", "T021 MW2 N=32 reads per commit, before", "", "fixed:2",
    RETRY, "arms.before.reads_per_commit_median")
add("mw3.retry.after_rpc", "T021 MW2 N=32 reads per commit, after", "", "fixed:2",
    RETRY, "arms.after.reads_per_commit_median")
add("mw3.retry.before_p99", "T021 MW2 N=32 p99, before", "ms", "fixed:1",
    RETRY, "arms.before.p99_ms_median")
add("mw3.retry.after_p99", "T021 MW2 N=32 p99, after", "ms", "fixed:1",
    RETRY, "arms.after.p99_ms_median")
add("mw3.retry.before_tx", "T021 MW2 N=32 throughput, before", "tx/s", "fixed:0",
    RETRY, "arms.before.throughput_tx_s_median")
add("mw3.retry.after_tx", "T021 MW2 N=32 throughput, after", "tx/s", "fixed:0",
    RETRY, "arms.after.throughput_tx_s_median")
add("mw3.retry.before_amp", "T021 MW2 N=32 retry amplification, before", "", "fixed:2",
    RETRY, "arms.before.retry_amplification_median")
add("mw3.retry.after_amp", "T021 MW2 N=32 retry amplification, after", "", "fixed:2",
    RETRY, "arms.after.retry_amplification_median")
add("mw3.retry.rpc_delta", "T021 reads-per-commit change, percent", "%", "fixed:1",
    RETRY, "comparison.reads_per_commit.delta_percent")
add("mw3.retry.p99_delta", "T021 p99 change, percent", "%", "fixed:1",
    RETRY, "comparison.p99_ms.delta_percent")
add("mw3.retry.cache_hits", "T021 intervening-commit cache hits (median)", "hits", "int",
    RETRY, "arms.after.gap_commits_cached_median")
add("mw3.retry.gap_reads", "T021 after intervening commits read (median)", "commits", "int",
    RETRY, "arms.after.gap_commits_read_median")

for n in [8, 16, 32, 64, 128, 256, 512, 1024]:
    mode = "threads-in-one-process-hold" if n >= 512 else "threads-in-one-process"
    sel = dict(n=n, mode=mode)
    add(f"mw3.s{n}.tx_s", f"T027 compact {n} writers throughput", "tx/s", "fixed:0",
        SCALE, selector=dict(where=sel, path="throughput_tx_s"))
    add(f"mw3.s{n}.p99", f"T027 compact {n} writers p99", "ms", "fixed:2",
        SCALE, selector=dict(where=sel, path="p99_ms"))
    add(f"mw3.s{n}.rss_kib", f"T027 compact {n} writers peak RSS", "KiB", "int",
        SCALE, selector=dict(where=sel, path="peak_rss_kib"))
    add(f"mw3.s{n}.spawned", f"T027 compact {n} writers spawned", "threads", "int",
        SCALE, selector=dict(where=sel, path="threads_spawned"))
    add(f"mw3.s{n}.spawn_failures", f"T027 compact {n} writers spawn failures", "threads", "int",
        SCALE, selector=dict(where=sel, path="spawn_failures"))
    add(f"mw3.s{n}.state_rows", f"T027 compact {n} writers max head-state rows per writer",
        "rows", "int", SCALE, selector=dict(where=sel, path="max_state_rows_per_writer"))
    add(f"mw3.s{n}.bytes_per_writer", f"T027 compact {n} writers observed RSS per writer",
        "B", "int", SCALE, selector=dict(where=sel, path="peak_rss_kib"))
    NUMBERS[-1]["expr"] = "{%s} * 1024 / {%s}" % (
        f"mw3.s{n}.rss_kib", f"mw3.s{n}.spawned")
    NUMBERS[-1].pop("path", None)
    NUMBERS[-1].pop("list", None)
    NUMBERS[-1].pop("where", None)

add("mw3.process32.tx_s", "T027 process-per-writer N=32 throughput", "tx/s", "fixed:0",
    SCALE, selector=dict(where={"n": 32, "mode": "one-process-per-writer"},
                         path="throughput_tx_s"))
add("mw3.process32.p99", "T027 process-per-writer N=32 p99", "ms", "fixed:2",
    SCALE, selector=dict(where={"n": 32, "mode": "one-process-per-writer"}, path="p99_ms"))

data["numbers"].extend(NUMBERS)


def block(bid, rows):
    return {"id": bid, "file": "bench/RESULTS-mw3.md", "kind": "lines",
            "rows": [{"template": r} for r in rows]}


disjoint_rows = [
    "| N | committed | tx/s | efficiency | p99 ms | reads/commit | gap commits read |",
    "|---:|---:|---:|---:|---:|---:|---:|",
]
for n in [1, 2, 4, 8, 16, 32]:
    disjoint_rows.append(
        f"| {n} | {{{{mw3.d{n}.committed}}}} | {{{{mw3.d{n}.tx_s}}}} | {{{{mw3.d{n}.eff}}}} | "
        f"{{{{mw3.d{n}.p99}}}} | {{{{mw3.d{n}.rpc}}}} | {{{{mw3.d{n}.gap}}}} |")

conflict_rows = [
    "| N | requested | committed | typed conflicts | conflict rate | p99 ms |",
    "|---:|---:|---:|---:|---:|---:|",
]
for n in [2, 8, 32]:
    conflict_rows.append(
        f"| {n} | {{{{mw3.c{n}.requested}}}} | {{{{mw3.c{n}.committed}}}} | "
        f"{{{{mw3.c{n}.conflicts}}}} | {{{{mw3.c{n}.rate}}}} | {{{{mw3.c{n}.p99}}}} |")

retry_rows = [
    "| arm | reads/commit | p99 ms | tx/s | retry amp | cache hits | gap commits read |",
    "|---|---:|---:|---:|---:|---:|---:|",
    "| before | {{mw3.retry.before_rpc}} | {{mw3.retry.before_p99}} | {{mw3.retry.before_tx}} | "
    "{{mw3.retry.before_amp}} | n/a | n/a |",
    "| after | {{mw3.retry.after_rpc}} | {{mw3.retry.after_p99}} | {{mw3.retry.after_tx}} | "
    "{{mw3.retry.after_amp}} | {{mw3.retry.cache_hits}} | {{mw3.retry.gap_reads}} |",
    "| change | **{{mw3.retry.rpc_delta}} %** | **{{mw3.retry.p99_delta}} %** | | | | |",
]

scale_rows = [
    "| writers | actor | spawned | spawn failures | tx/s | p99 ms | peak RSS KiB | RSS/writer B | max state rows |",
    "|---:|---|---:|---:|---:|---:|---:|---:|---:|",
]
for n in [8, 16, 32, 64, 128, 256, 512, 1024]:
    actor = "threads + hold" if n >= 512 else "threads"
    scale_rows.append(
        f"| {n} | {actor} | {{{{mw3.s{n}.spawned}}}} | {{{{mw3.s{n}.spawn_failures}}}} | "
        f"{{{{mw3.s{n}.tx_s}}}} | {{{{mw3.s{n}.p99}}}} | {{{{mw3.s{n}.rss_kib}}}} | "
        f"{{{{mw3.s{n}.bytes_per_writer}}}} | {{{{mw3.s{n}.state_rows}}}} |")
scale_rows.append(
    "| 32 | one process per writer | 32 | 0 | {{mw3.process32.tx_s}} | "
    "{{mw3.process32.p99}} | n/a | n/a | n/a |")

envelope_rows = [
    "At {{mw3.s1024.spawned}} spawned writers in one process the court's pid bound "
    "refuses {{mw3.s1024.spawn_failures}} further threads, and the observed peak RSS is "
    "**{{mw3.s1024.rss_kib}} KiB**, i.e. **{{mw3.s1024.bytes_per_writer}} B/writer**.",
    "The declared envelope is **512 KiB of thread stack (virtual) and one thread per "
    "writer**; the observed resident high-water mark per writer is "
    "{{mw3.s1024.bytes_per_writer}} B at 1024 writers and "
    "{{mw3.s32.bytes_per_writer}} B at 32, both inside it.",
]

data["blocks"].extend([
    block("mw3-disjoint-table", disjoint_rows),
    block("mw3-conflict-table", conflict_rows),
    block("mw3-retry-table", retry_rows),
    block("mw3-scale-table", scale_rows),
    block("mw3-envelope", envelope_rows),
    {"id": "mw3-status", "file": "docs/STATUS.md", "kind": "lines",
     "rows": [
       {"template": "MW3 (generated): disjoint domain writers reach **{{mw3.d16.tx_s}} tx/s** at N=16 "
                    "against **{{mw3.d1.tx_s}} tx/s** at N=1, with **{{mw3.d1.rpc}}** backend reads per "
                    "commit and **{{mw3.d16.gap}}** intervening commits read. Same-domain N=32: "
                    "**{{mw3.c32.committed}}** winner, **{{mw3.c32.conflicts}}** typed conflicts, p99 "
                    "**{{mw3.c32.p99}} ms** (stamp {{mw3.d16.tx_s|stamp}}, revision {{mw3.d16.tx_s|revision}})."},
       {"template": "T021 (generated): MW2 N=32 reads/commit **{{mw3.retry.before_rpc}} \u2192 "
                    "{{mw3.retry.after_rpc}}** (**{{mw3.retry.rpc_delta}} %**), p99 **{{mw3.retry.before_p99}} "
                    "\u2192 {{mw3.retry.after_p99}} ms** (**{{mw3.retry.p99_delta}} %**), throughput "
                    "**{{mw3.retry.before_tx}} \u2192 {{mw3.retry.after_tx}} tx/s**; the intervening-commit "
                    "cache recorded **{{mw3.retry.cache_hits}}** hits, so the win is jittered fair backoff, "
                    "not the cache (stamp {{mw3.retry.after_rpc|stamp}}, revision {{mw3.retry.after_rpc|revision}})."},
       {"template": "T027 (generated): **{{mw3.s1024.spawned}}** of 1024 writer threads spawn under "
                    "`--hold` (**{{mw3.s1024.spawn_failures}}** refused by the court's pid bound), peak RSS "
                    "**{{mw3.s1024.rss_kib}} KiB** = **{{mw3.s1024.bytes_per_writer}} B/writer**; peak "
                    "aggregate **{{mw3.s16.tx_s}} tx/s** at N=16 (stamp {{mw3.s1024.rss_kib|stamp}}, revision "
                    "{{mw3.s1024.rss_kib|revision}})."},
     ]},
])

json.dump(data, open(PATH, "w"), indent=1, sort_keys=False)
open(PATH, "a").write("\n")
print("numbers:", len(NUMBERS), "total:", len(data["numbers"]), "blocks:", len(data["blocks"]))
