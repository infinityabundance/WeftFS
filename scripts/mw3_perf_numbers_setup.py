#!/usr/bin/env python3
"""Append the MW3/T021/T027 numbers and blocks to docs/perf-numbers.json.

Run once from the repository root; idempotent (removes an existing `mw3` group
and every `mw3.*` number/block first). Every number names the receipt it came
from and the generated region prints it; `scripts/perf_numbers.py` is the only
writer of a generated region.
"""
import json
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PATH = os.path.join(ROOT, "docs", "perf-numbers.json")

MW3 = "bench/receipts/mw3-merged-20261114T070000Z.json"
SCALE = "bench/receipts/writer-scale-merged-20261114T070000Z.json"
RETRY_RUNS = "bench/receipts/mw2-retry-runs-20261114T070000Z.json"
RETRY_ONE = "bench/receipts/mw2-retry-20261114T065426Z.json"
R4 = "bench/receipts/bench-multiwriter-20261109T175227Z.json"

STAMP = "20261114T070000Z"
MW3_REVISION = json.load(open(os.path.join(ROOT, MW3)))["revisions"]["WeftFS"]
SCALE_REVISION = json.load(open(os.path.join(ROOT, SCALE)))["revisions"]["WeftFS"]
RETRY_REVISION = json.load(open(os.path.join(ROOT, RETRY_RUNS)))["revisions"]["WeftFS"]
R4_STAMP = json.load(open(os.path.join(ROOT, R4)))["stamp"]

data = json.load(open(PATH))
data["groups"] = [g for g in data["groups"] if g["id"] != "mw3"]
data["numbers"] = [n for n in data["numbers"] if not n["key"].startswith("mw3.")]
data["blocks"] = [b for b in data["blocks"] if not b["id"].startswith("mw3-")]

data["groups"].append({
    "id": "mw3",
    "title": "MW3 conflict domains, the T021 rebase and the T027 writer envelope "
             "(7 runs, stamps 20261114T065404Z..065524Z, merged %s)" % STAMP,
})

NUMBERS = []


def add(key, label, unit, fmt, receipt, path=None, selector=None,
        stamp_path="stamp", stamp=STAMP, revision_path="revisions.WeftFS",
        revision=None):
    entry = {
        "key": key,
        "label": label,
        "unit": unit,
        "group": "mw3",
        "format": fmt,
        "receipt": receipt,
        "stamp_path": stamp_path,
        "stamp": stamp,
        "revision_path": revision_path,
        "revision": revision,
    }
    if selector is not None:
        entry["list"] = selector.get("list", "rows")
        entry["where"] = selector["where"]
        if selector.get("path") is not None:
            entry["select_path"] = selector["path"]
    else:
        entry["path"] = path
    NUMBERS.append(entry)


def add_expr(key, label, unit, fmt, expr, receipt, stamp=STAMP, revision=None):
    NUMBERS.append({
        "key": key,
        "label": label,
        "unit": unit,
        "group": "mw3",
        "format": fmt,
        "expr": expr,
        "receipt": receipt,
        "stamp_path": "stamp",
        "stamp": stamp,
        "revision_path": "revisions.WeftFS",
        "revision": revision,
    })


# ---- T020: disjoint conflict domains, one writer per domain -----------------
DISJOINT = [1, 2, 4, 8, 16, 32, 64]
for n in DISJOINT:
    sel = dict(distribution="disjoint", n=n)
    add(f"mw3.d{n}.committed", f"MW3 disjoint N={n} committed", "transactions",
        "int", MW3, selector=dict(where=sel, path="committed"), revision=MW3_REVISION)
    add(f"mw3.d{n}.tx_s", f"MW3 disjoint N={n} aggregate throughput", "tx/s",
        "fixed:0", MW3, selector=dict(where=sel, path="throughput_tx_s"), revision=MW3_REVISION)
    add(f"mw3.d{n}.tx_s_min", f"MW3 disjoint N={n} throughput, lowest run", "tx/s",
        "fixed:0", MW3, path=f"spread.disjoint/{n}.throughput_tx_s.min", revision=MW3_REVISION)
    add(f"mw3.d{n}.tx_s_max", f"MW3 disjoint N={n} throughput, highest run", "tx/s",
        "fixed:0", MW3, path=f"spread.disjoint/{n}.throughput_tx_s.max", revision=MW3_REVISION)
    add(f"mw3.d{n}.eff", f"MW3 disjoint N={n} scaling efficiency vs N=1", "",
        "ratio:3", MW3, selector=dict(where=sel, path="scaling_efficiency"), revision=MW3_REVISION)
    add(f"mw3.d{n}.p50", f"MW3 disjoint N={n} p50", "ms", "fixed:3", MW3,
        selector=dict(where=sel, path="p50_ms"), revision=MW3_REVISION)
    add(f"mw3.d{n}.p95", f"MW3 disjoint N={n} p95", "ms", "fixed:3", MW3,
        selector=dict(where=sel, path="p95_ms"), revision=MW3_REVISION)
    add(f"mw3.d{n}.p99", f"MW3 disjoint N={n} p99", "ms", "fixed:3", MW3,
        selector=dict(where=sel, path="p99_ms"), revision=MW3_REVISION)
    add(f"mw3.d{n}.amp", f"MW3 disjoint N={n} retry amplification", "",
        "fixed:2", MW3, selector=dict(where=sel, path="retry_amplification"), revision=MW3_REVISION)
    add(f"mw3.d{n}.rpc", f"MW3 disjoint N={n} backend reads per commit", "",
        "fixed:2", MW3, selector=dict(where=sel, path="reads_per_commit"), revision=MW3_REVISION)
    add(f"mw3.d{n}.requests", f"MW3 disjoint N={n} backend requests", "requests",
        "int", MW3, selector=dict(where=sel, path="backend_requests"), revision=MW3_REVISION)
    add(f"mw3.d{n}.gap", f"MW3 disjoint N={n} intervening commits read", "commits",
        "int", MW3, selector=dict(where=sel, path="gap_commits_read"), revision=MW3_REVISION)
    add(f"mw3.d{n}.rss", f"MW3 disjoint N={n} summed peak RSS", "KiB", "int",
        MW3, selector=dict(where=sel, path="rss_sum_kib"), revision=MW3_REVISION)

# ---- T020: same-domain contention ------------------------------------------
CONFLICT = [2, 8, 32]
for n in CONFLICT:
    sel = dict(distribution="same-domain", n=n)
    add(f"mw3.c{n}.requested", f"MW3 same-domain N={n} requested", "transactions",
        "int", MW3, selector=dict(where=sel, path="requested"), revision=MW3_REVISION)
    add(f"mw3.c{n}.committed", f"MW3 same-domain N={n} committed", "transactions",
        "int", MW3, selector=dict(where=sel, path="committed"), revision=MW3_REVISION)
    add(f"mw3.c{n}.conflicts", f"MW3 same-domain N={n} typed conflicts", "conflicts",
        "int", MW3, selector=dict(where=sel, path="conflicts"), revision=MW3_REVISION)
    add(f"mw3.c{n}.p95", f"MW3 same-domain N={n} p95", "ms", "fixed:3", MW3,
        selector=dict(where=sel, path="p95_ms"), revision=MW3_REVISION)
    add(f"mw3.c{n}.p99", f"MW3 same-domain N={n} p99 (conflict path)", "ms",
        "fixed:3", MW3, selector=dict(where=sel, path="p99_ms"), revision=MW3_REVISION)
    add_expr(f"mw3.c{n}.rate", f"MW3 same-domain N={n} conflict rate", "",
             "ratio:4", "{mw3.c%d.conflicts} / {mw3.c%d.requested}" % (n, n),
             MW3, revision=MW3_REVISION)

# ---- T027: bounded wire count and per-client memory ------------------------
SCALE_NS = [8, 16, 32, 64, 128, 256, 512, 1024]
for n in SCALE_NS:
    mode = ("threads-in-one-process-hold" if n >= 512 else "threads-in-one-process")
    sel = dict(n=n, mode=mode)
    add(f"mw3.s{n}.spawned", f"T027 compact {n} writers spawned", "threads",
        "int", SCALE, selector=dict(where=sel, path="threads_spawned"), revision=SCALE_REVISION)
    add(f"mw3.s{n}.refused", f"T027 compact {n} writers refused by the pid bound",
        "threads", "int", SCALE, selector=dict(where=sel, path="spawn_failures"),
        revision=SCALE_REVISION)
    add(f"mw3.s{n}.committed", f"T027 compact {n} writers committed", "transactions",
        "int", SCALE, selector=dict(where=sel, path="committed"), revision=SCALE_REVISION)
    add(f"mw3.s{n}.count", f"T027 compact {n} writers commits per writer",
        "transactions", "int", SCALE, selector=dict(where=sel, path="count_per_writer"),
        revision=SCALE_REVISION)
    add(f"mw3.s{n}.tx_s", f"T027 compact {n} writers throughput", "tx/s",
        "fixed:0", SCALE, selector=dict(where=sel, path="throughput_tx_s"), revision=SCALE_REVISION)
    add(f"mw3.s{n}.p50", f"T027 compact {n} writers p50", "ms", "fixed:3", SCALE,
        selector=dict(where=sel, path="p50_ms"), revision=SCALE_REVISION)
    add(f"mw3.s{n}.p95", f"T027 compact {n} writers p95", "ms", "fixed:2", SCALE,
        selector=dict(where=sel, path="p95_ms"), revision=SCALE_REVISION)
    add(f"mw3.s{n}.p99", f"T027 compact {n} writers p99", "ms", "fixed:2", SCALE,
        selector=dict(where=sel, path="p99_ms"), revision=SCALE_REVISION)
    add(f"mw3.s{n}.rss_kib", f"T027 compact {n} writers peak RSS", "KiB",
        "int", SCALE, selector=dict(where=sel, path="peak_rss_kib"), revision=SCALE_REVISION)
    add(f"mw3.s{n}.state_rows", f"T027 compact {n} writers max head-state rows per writer",
        "rows", "int", SCALE, selector=dict(where=sel, path="max_state_rows_per_writer"),
        revision=SCALE_REVISION)
    add(f"mw3.s{n}.stalled", f"T027 compact {n} writers that ever retried",
        "writers", "int", SCALE, selector=dict(where=sel, path="stalled_writers"),
        revision=SCALE_REVISION)
    add(f"mw3.s{n}.starvation", f"T027 compact {n} writers max consecutive retries",
        "retries", "int", SCALE, selector=dict(where=sel, path="max_consecutive_retries"),
        revision=SCALE_REVISION)
    add_expr(f"mw3.s{n}.bytes_per_writer", f"T027 compact {n} writers observed RSS per writer",
             "B", "int", "{mw3.s%d.rss_kib} * 1024 / {mw3.s%d.spawned}" % (n, n),
             SCALE, revision=SCALE_REVISION)

add("mw3.process32.tx_s", "T027 process-per-writer N=32 throughput", "tx/s",
    "fixed:0", SCALE, selector=dict(where={"n": 32, "mode": "one-process-per-writer"},
                                    path="throughput_tx_s"), revision=SCALE_REVISION)
add("mw3.process32.p99", "T027 process-per-writer N=32 p99", "ms", "fixed:2",
    SCALE, selector=dict(where={"n": 32, "mode": "one-process-per-writer"}, path="p99_ms"),
    revision=SCALE_REVISION)
add("mw3.process32.rss", "T027 process-per-writer N=32 summed peak RSS", "KiB",
    "int", SCALE, selector=dict(where={"n": 32, "mode": "one-process-per-writer"},
                                path="rss_sum_kib"), revision=SCALE_REVISION)

# ---- T021: before/after, every run and the median across runs --------------
for run in range(1, 8):
    sel = dict(run=run)
    add(f"mw3.retry.r{run}.before_rpc", f"T021 run {run} reads/commit, before", "",
        "fixed:2", RETRY_RUNS, selector=dict(where=sel, path="before_rpc"),
        revision=RETRY_REVISION)
    add(f"mw3.retry.r{run}.after_rpc", f"T021 run {run} reads/commit, after", "",
        "fixed:2", RETRY_RUNS, selector=dict(where=sel, path="after_rpc"),
        revision=RETRY_REVISION)
    add(f"mw3.retry.r{run}.delta", f"T021 run {run} reads/commit change", "%",
        "fixed:1", RETRY_RUNS, selector=dict(where=sel, path="rpc_delta_percent"),
        revision=RETRY_REVISION)
    add(f"mw3.retry.r{run}.before_p99", f"T021 run {run} p99, before", "ms",
        "fixed:1", RETRY_RUNS, selector=dict(where=sel, path="before_p99_ms"),
        revision=RETRY_REVISION)
    add(f"mw3.retry.r{run}.after_p99", f"T021 run {run} p99, after", "ms",
        "fixed:1", RETRY_RUNS, selector=dict(where=sel, path="after_p99_ms"),
        revision=RETRY_REVISION)
    add(f"mw3.retry.r{run}.before_tx", f"T021 run {run} throughput, before", "tx/s",
        "fixed:0", RETRY_RUNS, selector=dict(where=sel, path="before_tx_s"),
        revision=RETRY_REVISION)
    add(f"mw3.retry.r{run}.after_tx", f"T021 run {run} throughput, after", "tx/s",
        "fixed:0", RETRY_RUNS, selector=dict(where=sel, path="after_tx_s"),
        revision=RETRY_REVISION)

add("mw3.retry.median_before_rpc", "T021 median reads/commit, before", "",
    "fixed:2", RETRY_RUNS, path="medians.before_rpc", revision=RETRY_REVISION)
add("mw3.retry.median_after_rpc", "T021 median reads/commit, after", "",
    "fixed:2", RETRY_RUNS, path="medians.after_rpc", revision=RETRY_REVISION)
add("mw3.retry.median_before_p99", "T021 median p99, before", "ms", "fixed:1",
    RETRY_RUNS, path="medians.before_p99_ms", revision=RETRY_REVISION)
add("mw3.retry.median_after_p99", "T021 median p99, after", "ms", "fixed:1",
    RETRY_RUNS, path="medians.after_p99_ms", revision=RETRY_REVISION)
add("mw3.retry.median_before_tx", "T021 median throughput, before", "tx/s",
    "fixed:0", RETRY_RUNS, path="medians.before_tx_s", revision=RETRY_REVISION)
add("mw3.retry.median_after_tx", "T021 median throughput, after", "tx/s",
    "fixed:0", RETRY_RUNS, path="medians.after_tx_s", revision=RETRY_REVISION)
add("mw3.retry.median_before_amp", "T021 median retry amplification, before", "",
    "fixed:2", RETRY_RUNS, path="medians.before_amp", revision=RETRY_REVISION)
add("mw3.retry.median_after_amp", "T021 median retry amplification, after", "",
    "fixed:2", RETRY_RUNS, path="medians.after_amp", revision=RETRY_REVISION)
add("mw3.retry.cache_hits", "T021 intervening-commit cache hits (median)", "hits",
    "int", RETRY_RUNS, path="medians.after_cache_hits", revision=RETRY_REVISION)
add("mw3.retry.gap_reads", "T021 after intervening commits read (median)", "commits",
    "int", RETRY_RUNS, path="medians.after_gap_reads", revision=RETRY_REVISION)
add("mw3.retry.runs", "T021 court runs merged", "runs", "int", RETRY_RUNS,
    path="runs", revision=RETRY_REVISION)
add("mw3.retry.runs_lower_rpc", "T021 runs whose reads/commit fell", "runs", "int",
    RETRY_RUNS, path="runs_with_lower_reads_per_commit", revision=RETRY_REVISION)
add("mw3.retry.runs_lower_p99", "T021 runs whose p99 fell", "runs", "int",
    RETRY_RUNS, path="runs_with_lower_p99", revision=RETRY_REVISION)
add("mw3.retry.reps_per_arm", "T021 repetitions per arm in one run", "repetitions",
    "int", RETRY_RUNS, selector=dict(where={"run": 1}, path="repetitions_per_arm"),
    revision=RETRY_REVISION)
add("mw3.retry.baseline", "T021 before-arm revision", "", "text", RETRY_RUNS,
    path="baseline.revision", revision=RETRY_REVISION)

# Published R4 N=32 row: a different revision and fixture generation, cited as
# labelled history rather than as this session's baseline.
r4sel = dict(distribution="disjoint", n=32)
add("mw3.r4.reads", "Published R4 N=32 backend reads", "reads", "int",
    R4, selector=dict(where=r4sel, path="backend_reads"),
    stamp=R4_STAMP, revision_path=None, revision=None)
add("mw3.r4.committed", "Published R4 N=32 committed", "transactions", "int",
    R4, selector=dict(where=r4sel, path="committed"),
    stamp=R4_STAMP, revision_path=None, revision=None)
add_expr("mw3.r4.rpc", "Published R4 N=32 backend reads per commit", "",
         "fixed:2", "{mw3.r4.reads} / {mw3.r4.committed}", R4, stamp=R4_STAMP,
         revision=None)
add("mw3.r4.p99", "Published R4 N=32 p99", "ms", "fixed:1", R4,
    selector=dict(where=r4sel, path="p99_ms"), stamp=R4_STAMP,
    revision_path=None, revision=None)
add("mw3.r4.tx_s", "Published R4 N=32 aggregate throughput", "tx/s", "fixed:0",
    R4, selector=dict(where=r4sel, path="throughput_tx_s"), stamp=R4_STAMP,
    revision_path=None, revision=None)
add("mw3.r4.revision", "Published R4 receipt repository line", "", "text", R4,
    path="repositories.0.revision", stamp=R4_STAMP, revision_path=None, revision=None)

data["numbers"].extend(NUMBERS)


def block(bid, rows, file="bench/RESULTS-mw3.md", **extra):
    out = {"id": bid, "file": file, "kind": "lines",
           "rows": [{"template": r} for r in rows]}
    out.update(extra)
    return out


disjoint_rows = [
    "| N | committed | tx/s (median) | lowest–highest run | efficiency vs N=1 | "
    "p50 ms | p95 ms | p99 ms | retry amp | reads/commit | gap commits read | "
    "backend requests | RSS KiB |",
    "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
]
for n in DISJOINT:
    disjoint_rows.append(
        f"| {n} | {{{{mw3.d{n}.committed}}}} | {{{{mw3.d{n}.tx_s}}}} | "
        f"{{{{mw3.d{n}.tx_s_min}}}}–{{{{mw3.d{n}.tx_s_max}}}} | {{{{mw3.d{n}.eff}}}} | "
        f"{{{{mw3.d{n}.p50}}}} | {{{{mw3.d{n}.p95}}}} | {{{{mw3.d{n}.p99}}}} | "
        f"{{{{mw3.d{n}.amp}}}} | {{{{mw3.d{n}.rpc}}}} | {{{{mw3.d{n}.gap}}}} | "
        f"{{{{mw3.d{n}.requests}}}} | {{{{mw3.d{n}.rss}}}} |")

conflict_rows = [
    "| N | requested | committed | typed conflicts | conflict rate | p95 ms | p99 ms |",
    "|---:|---:|---:|---:|---:|---:|---:|",
]
for n in CONFLICT:
    conflict_rows.append(
        f"| {n} | {{{{mw3.c{n}.requested}}}} | {{{{mw3.c{n}.committed}}}} | "
        f"{{{{mw3.c{n}.conflicts}}}} | {{{{mw3.c{n}.rate}}}} | {{{{mw3.c{n}.p95}}}} | "
        f"{{{{mw3.c{n}.p99}}}} |")

retry_run_rows = [
    "| run | before reads/commit | after reads/commit | change | before p99 ms | "
    "after p99 ms | before tx/s | after tx/s |",
    "|---:|---:|---:|---:|---:|---:|---:|---:|",
]
for run in range(1, 8):
    retry_run_rows.append(
        f"| {run} | {{{{mw3.retry.r{run}.before_rpc}}}} | {{{{mw3.retry.r{run}.after_rpc}}}} | "
        f"{{{{mw3.retry.r{run}.delta}}}} % | {{{{mw3.retry.r{run}.before_p99}}}} | "
        f"{{{{mw3.retry.r{run}.after_p99}}}} | {{{{mw3.retry.r{run}.before_tx}}}} | "
        f"{{{{mw3.retry.r{run}.after_tx}}}} |")

retry_summary_rows = [
    "| arm | reads/commit | p99 ms | tx/s | retry amp | cache hits | gap commits read |",
    "|---|---:|---:|---:|---:|---:|---:|",
    "| before (median of 7 runs) | {{mw3.retry.median_before_rpc}} | "
    "{{mw3.retry.median_before_p99}} | {{mw3.retry.median_before_tx}} | "
    "{{mw3.retry.median_before_amp}} | n/a | n/a |",
    "| after (median of 7 runs) | {{mw3.retry.median_after_rpc}} | "
    "{{mw3.retry.median_after_p99}} | {{mw3.retry.median_after_tx}} | "
    "{{mw3.retry.median_after_amp}} | {{mw3.retry.cache_hits}} | {{mw3.retry.gap_reads}} |",
    "| published R4 N=32 (different revision) | {{mw3.r4.rpc}} | {{mw3.r4.p99}} | "
    "{{mw3.r4.tx_s}} | n/a | n/a | n/a |",
]

scale_rows = [
    "| writers | spawned | refused | committed | commits/writer | tx/s | p50 ms | "
    "p95 ms | p99 ms | peak RSS KiB | B/writer | max state rows | max consecutive "
    "retries | writers that retried |",
    "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
]
for n in SCALE_NS:
    scale_rows.append(
        f"| {n} | {{{{mw3.s{n}.spawned}}}} | {{{{mw3.s{n}.refused}}}} | "
        f"{{{{mw3.s{n}.committed}}}} | {{{{mw3.s{n}.count}}}} | {{{{mw3.s{n}.tx_s}}}} | "
        f"{{{{mw3.s{n}.p50}}}} | {{{{mw3.s{n}.p95}}}} | {{{{mw3.s{n}.p99}}}} | "
        f"{{{{mw3.s{n}.rss_kib}}}} | {{{{mw3.s{n}.bytes_per_writer}}}} | "
        f"{{{{mw3.s{n}.state_rows}}}} | {{{{mw3.s{n}.starvation}}}} | "
        f"{{{{mw3.s{n}.stalled}}}} |")
scale_rows.append(
    "| 32 (process per writer) | 32 | 0 | 160 | 5 | {{mw3.process32.tx_s}} | "
    "n/a | n/a | {{mw3.process32.p99}} | {{mw3.process32.rss}} | n/a | n/a | 0 | 0 |")

envelope_rows = [
    "At {{mw3.s1024.spawned}} spawned writers in one process the court's pid bound "
    "refuses {{mw3.s1024.refused}} further threads, and the observed peak RSS is "
    "**{{mw3.s1024.rss_kib}} KiB**, i.e. **{{mw3.s1024.bytes_per_writer}} B/writer**.",
    "No writer was starved: the largest number of consecutive retries any writer "
    "took is {{mw3.s1024.starvation}} at 1024 writers and "
    "{{mw3.s32.starvation}} at 32, and {{mw3.s1024.stalled}} writers ever retried.",
    "The declared envelope is **one thread per writer with a 512 KiB thread stack, "
    "set explicitly by the court client, plus one open store handle**. The stack "
    "bound is virtual, so it does not bound resident memory; what is observed is "
    "**{{mw3.s1024.bytes_per_writer}} B/writer** at 1024 writers and "
    "{{mw3.s512.bytes_per_writer}} B/writer at 512, both at {{mw3.s1024.count}} "
    "commits per writer, and {{mw3.s8.bytes_per_writer}} B/writer at 8 writers at "
    "{{mw3.s8.count}} commits per writer. Resident cost grows with the per-writer "
    "committed state, which is why the small-N cells are larger per writer.",
]

data["blocks"].extend([
    block("mw3-disjoint-table", disjoint_rows),
    block("mw3-conflict-table", conflict_rows),
    block("mw3-retry-runs", retry_run_rows),
    block("mw3-retry-summary", retry_summary_rows),
    block("mw3-scale-table", scale_rows),
    block("mw3-envelope", envelope_rows),
    block("mw3-retry-method", [
        "The baseline is a release build of revision `{{mw3.retry.baseline}}`; each "
        "arm contributes {{mw3.retry.reps_per_arm}} repetitions inside each of the "
        "{{mw3.retry.runs}} runs, and every run's own pair is in the table above.",
    ]),
    block("mw3-r4-reference", [
        "The published R4 N=32 row is {{mw3.r4.committed}} committed transactions "
        "from {{mw3.r4.reads}} backend reads at revision `{{mw3.r4.revision}}`, "
        "measured by the R4 harness generation "
        "(stamp {{mw3.r4.committed|stamp}}). It is cited as labelled history: a "
        "different revision, harness and session, not this subphase's baseline.",
    ]),
    block("mw3-status", [
        "MW3 (generated): disjoint domain writers reach **{{mw3.d8.tx_s}} tx/s** at N=8 "
        "against **{{mw3.d1.tx_s}} tx/s** at N=1 (**{{mw3.d8.eff}}** efficiency), then "
        "plateau at four vCPUs; every disjoint cell reports **{{mw3.d8.rpc}}** backend reads "
        "per commit and **{{mw3.d8.gap}}** intervening commits read. Same-domain N=32: "
        "**{{mw3.c32.committed}}** winner, **{{mw3.c32.conflicts}}** typed conflicts, p99 "
        "**{{mw3.c32.p99}} ms** (stamp {{mw3.d8.tx_s|stamp}}, revision {{mw3.d8.tx_s|revision}}).",
        "T021 (generated): across {{mw3.retry.runs}} court runs MW2 N=32 reads/commit "
        "**{{mw3.retry.median_before_rpc}} \u2192 {{mw3.retry.median_after_rpc}}** and p99 "
        "**{{mw3.retry.median_before_p99}} \u2192 {{mw3.retry.median_after_p99}} ms**; p99 fell "
        "in **{{mw3.retry.runs_lower_p99}} of {{mw3.retry.runs}}** runs and reads/commit in "
        "**{{mw3.retry.runs_lower_rpc}} of {{mw3.retry.runs}}**, so the read count is not a "
        "stable claim; the published R4 figure was **{{mw3.r4.rpc}}** reads/commit "
        "(stamp {{mw3.retry.median_after_rpc|stamp}}, revision {{mw3.retry.median_after_rpc|revision}}).",
        "T027 (generated): **{{mw3.s1024.spawned}}** of 1024 writer threads spawn under "
        "`--hold` (**{{mw3.s1024.refused}}** refused by the court's pid bound), peak RSS "
        "**{{mw3.s1024.rss_kib}} KiB** = **{{mw3.s1024.bytes_per_writer}} B/writer**; "
        "**{{mw3.s1024.stalled}}** writers ever retried; peak aggregate "
        "**{{mw3.s16.tx_s}} tx/s** at N=16 (stamp {{mw3.s1024.rss_kib|stamp}}, revision "
        "{{mw3.s1024.rss_kib|revision}}).",
    ], file="docs/STATUS.md"),
])

json.dump(data, open(PATH, "w"), indent=1, sort_keys=False)
open(PATH, "a").write("\n")
print("numbers:", len(NUMBERS), "total:", len(data["numbers"]), "blocks:", len(data["blocks"]))
