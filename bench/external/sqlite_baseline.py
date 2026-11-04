#!/usr/bin/env python3
"""External context/query baseline: schema-indexed SQLite, relational and
adjacency-table variants.

This is a baseline, not a model of WeftFS. It uses the exact corpus emitted by
`examples/bench_external_manifest.rs` (same payload bytes, ObjectIds,
relations, tags, snapshot names, and commit generations as the Rust store
benchmark). It measures the same query shapes with correct indexes, relation
insert/delete churn, snapshot resolve, index build time/bytes, cold/warm
latency, and bytes on disk.

The adjacency variant is a SQLite adjacency table. It is NOT a dedicated graph
database and no Neo4j/SPARQL comparison is made.

Run only inside the OOM-protected bench court:
  scripts/docker-court.sh bench \
    "python3 bench/external/sqlite_baseline.py --manifest ... --out ..."
"""

import argparse
import hashlib
import json
import os
import platform
import sqlite3
import statistics
import sys
import time
from pathlib import Path


def ns() -> int:
    return time.perf_counter_ns()


def stats(samples):
    values = sorted(int(v) for v in samples)
    if not values:
        return {"samples": 0}

    def pct(p):
        if len(values) == 1:
            return values[0]
        rank = max(0, min(len(values) - 1, int(round((p / 100.0) * (len(values) - 1)))))
        return values[rank]

    return {
        "samples": len(values),
        "min_ns": values[0],
        "median_ns": statistics.median(values),
        "mean_ns": statistics.fmean(values),
        "p95_ns": pct(95),
        "p99_ns": pct(99),
        "max_ns": values[-1],
        "stdev_ns": statistics.pstdev(values) if len(values) > 1 else 0.0,
        "raw_ns": values,
    }


def evict_file(path: Path) -> bool:
    """Advise the kernel to drop page-cache pages for one file.

    This is a file-scoped page-cache eviction (POSIX_FADV_DONTNEED), not a
    device-cache drop. It needs no privilege. The VM fio court uses a real
    guest `drop_caches`; this baseline cannot.
    """
    if not path.exists():
        return False
    fd = os.open(path, os.O_RDONLY)
    try:
        size = os.fstat(fd).st_size
        os.posix_fadvise(fd, 0, size, os.POSIX_FADV_DONTNEED)
    finally:
        os.close(fd)
    return True


class Db:
    def __init__(self, path: Path, variant: str):
        self.path = path
        self.variant = variant
        self.conn = None

    def open(self):
        self.conn = sqlite3.connect(str(self.path), isolation_level=None)
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA synchronous=FULL")
        self.conn.execute("PRAGMA cache_size=-8000")
        self.conn.execute("PRAGMA mmap_size=0")
        self.conn.execute("PRAGMA temp_store=MEMORY")
        return self.conn

    def close(self):
        if self.conn is not None:
            self.conn.close()
            self.conn = None

    def checkpoint(self):
        if self.conn is not None:
            self.conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")

    def disk_bytes(self):
        total = 0
        files = {}
        for suffix in ("", "-wal", "-shm"):
            p = Path(str(self.path) + suffix)
            n = p.stat().st_size if p.exists() else 0
            files[p.name] = n
            total += n
        return total, files


RELATIONAL_SCHEMA = """
CREATE TABLE objects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  size INTEGER NOT NULL,
  generation INTEGER NOT NULL
);
CREATE TABLE relations (
  id INTEGER PRIMARY KEY,
  subject TEXT NOT NULL,
  predicate TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_value TEXT NOT NULL,
  generation INTEGER NOT NULL
);
CREATE TABLE snapshots (
  name TEXT PRIMARY KEY,
  generation INTEGER NOT NULL,
  object_count INTEGER NOT NULL
);
CREATE TABLE payloads (
  object_id TEXT PRIMARY KEY,
  bytes BLOB NOT NULL
);
"""

ADJACENCY_SCHEMA = """
CREATE TABLE nodes (
  id INTEGER PRIMARY KEY,
  object_id TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  size INTEGER NOT NULL,
  generation INTEGER NOT NULL
);
CREATE TABLE edges (
  id INTEGER PRIMARY KEY,
  src INTEGER NOT NULL,
  label TEXT NOT NULL,
  dst INTEGER,
  dst_value TEXT,
  generation INTEGER NOT NULL
);
CREATE TABLE snapshots (
  name TEXT PRIMARY KEY,
  generation INTEGER NOT NULL,
  object_count INTEGER NOT NULL
);
CREATE TABLE payloads (
  node_id INTEGER PRIMARY KEY,
  bytes BLOB NOT NULL
);
"""

RELATIONAL_INDEXES = [
    ("objects_kind", "CREATE INDEX objects_kind ON objects(kind)"),
    ("objects_generation", "CREATE INDEX objects_generation ON objects(generation)"),
    (
        "relations_predicate_target",
        "CREATE INDEX relations_predicate_target ON relations(predicate, target_kind, target_value)",
    ),
    (
        "relations_subject_predicate",
        "CREATE INDEX relations_subject_predicate ON relations(subject, predicate)",
    ),
]

ADJACENCY_INDEXES = [
    ("nodes_kind", "CREATE INDEX nodes_kind ON nodes(kind)"),
    ("nodes_generation", "CREATE INDEX nodes_generation ON nodes(generation)"),
    ("edges_label_dst", "CREATE INDEX edges_label_dst ON edges(label, dst)"),
    (
        "edges_label_value",
        "CREATE INDEX edges_label_value ON edges(label, dst_value)",
    ),
    ("edges_src_label", "CREATE INDEX edges_src_label ON edges(src, label)"),
]


def table_build(db, size, manifest):
    """Create tables (no secondary indexes), bulk-load, and durable-sample."""
    conn = db.open()
    conn.executescript(RELATIONAL_SCHEMA if db.variant == "relational" else ADJACENCY_SCHEMA)

    objects = manifest["objects"]
    start = ns()
    conn.execute("BEGIN")
    if db.variant == "relational":
        conn.executemany(
            "INSERT INTO objects(id,name,kind,content_hash,size,generation) VALUES(?,?,?,?,?,?)",
            [
                (o["object_id"], o["name"], "file", o["content_blake3"], o["bytes"], o["generation"])
                for o in objects
            ],
        )
        conn.executemany(
            "INSERT INTO payloads(object_id,bytes) VALUES(?,?)",
            [(o["object_id"], bytes.fromhex(o["payload_hex"])) for o in objects],
        )
    else:
        conn.executemany(
            "INSERT INTO nodes(id,object_id,name,kind,content_hash,size,generation) VALUES(?,?,?,?,?,?,?)",
            [
                (i, o["object_id"], o["name"], "file", o["content_blake3"], o["bytes"], o["generation"])
                for i, o in enumerate(objects, start=1)
            ],
        )
        conn.executemany(
            "INSERT INTO payloads(node_id,bytes) VALUES(?,?)",
            [(i, bytes.fromhex(o["payload_hex"])) for i, o in enumerate(objects, start=1)],
        )
    conn.execute("COMMIT")
    object_bulk_ns = ns() - start

    start = ns()
    conn.execute("BEGIN")
    if db.variant == "relational":
        conn.executemany(
            "INSERT INTO relations(subject,predicate,target_kind,target_value,generation) VALUES(?,?,?,?,?)",
            [
                (r["subject"], r["predicate"], r["target_kind"], r["target_value"], r["generation"])
                for r in manifest["relations"]
            ],
        )
    else:
        node_of = {o["object_id"]: i for i, o in enumerate(objects, start=1)}
        conn.executemany(
            "INSERT INTO edges(src,label,dst,dst_value,generation) VALUES(?,?,?,?,?)",
            [
                (
                    node_of[r["subject"]],
                    r["predicate"],
                    node_of[r["target_value"]] if r["target_kind"] == "object" else None,
                    None if r["target_kind"] == "object" else r["target_value"],
                    r["generation"],
                )
                for r in manifest["relations"]
            ],
        )
    conn.executemany(
        "INSERT INTO snapshots(name,generation,object_count) VALUES(?,?,?)",
        [(s["name"], s["generation"], s["object_count"]) for s in manifest["snapshots"]],
    )
    conn.execute("COMMIT")
    relation_bulk_ns = ns() - start

    # Durable per-object insert sample (own commit and fsync per object), from
    # a scratch table so the bulk-loaded corpus is untouched.
    durable_rows = min(100, size)
    conn.execute("CREATE TABLE durable_sample(id TEXT PRIMARY KEY, bytes BLOB)")
    durable_ns = []
    for i in range(durable_rows):
        o = objects[i]
        start = ns()
        conn.execute("BEGIN")
        conn.execute(
            "INSERT INTO durable_sample(id,bytes) VALUES(?,?)",
            (o["object_id"], bytes.fromhex(o["payload_hex"])),
        )
        conn.execute("COMMIT")
        durable_ns.append(ns() - start)
    conn.execute("DROP TABLE durable_sample")

    return {
        "object_bulk_load_ns": object_bulk_ns,
        "relation_bulk_load_ns": relation_bulk_ns,
        "durable_insert_ns": durable_ns,
        "rows": {
            "objects": len(objects),
            "relations": len(manifest["relations"]),
            "snapshots": len(manifest["snapshots"]),
        },
    }


def build_indexes(db):
    conn = db.open()
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    before_bytes, _ = db.disk_bytes()
    samples = []
    for name, statement in RELATIONAL_INDEXES if db.variant == "relational" else ADJACENCY_INDEXES:
        start = ns()
        conn.execute(statement)
        samples.append({"index": name, "ns": ns() - start, "sql": statement})
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    after_bytes, files = db.disk_bytes()
    return {
        "indexes": samples,
        "total_ns": sum(s["ns"] for s in samples),
        "bytes_before": before_bytes,
        "bytes_after": after_bytes,
        "index_bytes": after_bytes - before_bytes,
        "files": files,
        "sql": [s["sql"] for s in samples],
    }


def query_specs(variant, manifest):
    size = manifest["size"]
    objects = manifest["objects"]
    first = objects[0]["object_id"]
    middle = objects[size // 2]["object_id"]
    half_gen = next(s["generation"] for s in manifest["snapshots"] if s["name"] == "half")
    if variant == "relational":
        return [
            ("q0_tag_absent", "SELECT subject FROM relations WHERE predicate=? AND target_kind=? AND target_value=?", ("tagged_as", "value", "absent_tag"), 0),
            ("q1_relation_incoming", "SELECT subject FROM relations WHERE predicate=? AND target_kind=? AND target_value=?", ("refers_to", "object", first), 1),
            ("qn_kind_file", "SELECT id FROM objects WHERE kind=?", ("file",), size),
            ("qeven_tag", "SELECT subject FROM relations WHERE predicate=? AND target_kind=? AND target_value=?", ("tagged_as", "value", "even"), manifest["tagged_even"]),
            ("qhop_relation", "SELECT subject FROM relations WHERE predicate=? AND target_kind=? AND target_value=?", ("refers_to", "object", middle), 1),
            ("qoutgoing_hop", "SELECT target_value FROM relations WHERE subject=? AND predicate=?", (first, "refers_to"), 1),
            ("qsnapshot_resolve", "SELECT generation, object_count FROM snapshots WHERE name=?", ("half",), 1),
            ("qsnapshot_visible", "SELECT count(*) FROM objects WHERE generation<=?", (half_gen,), size),
        ]
    return [
        ("q0_tag_absent", "SELECT src FROM edges WHERE label=? AND dst_value=?", ("tagged_as", "absent_tag"), 0),
        ("q1_relation_incoming", "SELECT src FROM edges WHERE label=? AND dst=?", ("refers_to", 1), 1),
        ("qn_kind_file", "SELECT object_id FROM nodes WHERE kind=?", ("file",), size),
        ("qeven_tag", "SELECT src FROM edges WHERE label=? AND dst_value=?", ("tagged_as", "even"), manifest["tagged_even"]),
        ("qhop_relation", "SELECT src FROM edges WHERE label=? AND dst=?", ("refers_to", (size // 2) + 1), 1),
        ("qoutgoing_hop", "SELECT dst FROM edges WHERE src=? AND label=?", (1, "refers_to"), 1),
        ("qsnapshot_resolve", "SELECT generation, object_count FROM snapshots WHERE name=?", ("half",), 1),
        ("qsnapshot_visible", "SELECT count(*) FROM nodes WHERE generation<=?", (half_gen,), size),
    ]


def run_queries(db, manifest, warm_iterations, cold_iterations):
    conn = db.open()
    specs = query_specs(db.variant, manifest)
    plans = {}
    for name, sql, params, expected in specs:
        plan = [list(row) for row in conn.execute("EXPLAIN QUERY PLAN " + sql, params)]
        rows = conn.execute(sql, params).fetchall()
        got = int(rows[0][0]) if name == "qsnapshot_visible" else len(rows)
        if got != expected:
            raise SystemExit(f"{db.variant}: {name} matched {got}, expected {expected}")
        plans[name] = plan

    results = {}
    for name, sql, params, _ in specs:
        for _ in range(5):
            conn.execute(sql, params).fetchall()
        samples = []
        for _ in range(warm_iterations):
            start = ns()
            conn.execute(sql, params).fetchall()
            samples.append(ns() - start)
        results[name] = {"query": sql, "params": list(params), "warm": stats(samples)}

    # Cold: close, evict the DB/WAL/SHM page cache, reopen, then time one query.
    for name, sql, params, _ in specs:
        cold_query = []
        cold_open = []
        for _ in range(cold_iterations):
            db.close()
            for suffix in ("", "-wal", "-shm"):
                evict_file(Path(str(db.path) + suffix))
            start = ns()
            conn = db.open()
            conn.execute("SELECT count(*) FROM sqlite_master").fetchone()
            cold_open.append(ns() - start)
            start = ns()
            conn.execute(sql, params).fetchall()
            cold_query.append(ns() - start)
        results[name]["cold"] = stats(cold_query)
        results[name]["cold_open"] = stats(cold_open)
    db.checkpoint()
    return results, plans


def run_churn(db, manifest, rounds):
    conn = db.open()
    add_ns = []
    delete_ns = []
    for round_index in range(rounds):
        predicate = f"bench_churn_{round_index}"
        generation = manifest["generation_after_ingest"] + 1 + round_index * 2
        if db.variant == "relational":
            start = ns()
            conn.execute("BEGIN")
            cur = conn.execute(
                "INSERT INTO relations(subject,predicate,target_kind,target_value,generation) VALUES(?,?,?,?,?)",
                (manifest["objects"][0]["object_id"], predicate, "value", f"churn-{round_index}", generation),
            )
            rowid = cur.lastrowid
            conn.execute("COMMIT")
            add_ns.append(ns() - start)
            start = ns()
            conn.execute("BEGIN")
            conn.execute("DELETE FROM relations WHERE id=?", (rowid,))
            conn.execute("COMMIT")
            delete_ns.append(ns() - start)
        else:
            start = ns()
            conn.execute("BEGIN")
            cur = conn.execute(
                "INSERT INTO edges(src,label,dst,dst_value,generation) VALUES(?,?,?,?,?)",
                (1, predicate, None, f"churn-{round_index}", generation),
            )
            rowid = cur.lastrowid
            conn.execute("COMMIT")
            add_ns.append(ns() - start)
            start = ns()
            conn.execute("BEGIN")
            conn.execute("DELETE FROM edges WHERE id=?", (rowid,))
            conn.execute("COMMIT")
            delete_ns.append(ns() - start)
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    return {"rounds": rounds, "add": stats(add_ns), "delete": stats(delete_ns)}


def environment():
    def cpu_model():
        try:
            for line in Path("/proc/cpuinfo").read_text().splitlines():
                if line.startswith("model name"):
                    return line.split(":", 1)[1].strip()
        except OSError:
            pass
        return None

    def mem_total():
        try:
            for line in Path("/proc/meminfo").read_text().splitlines():
                if line.startswith("MemTotal:"):
                    return int(line.split()[1])
        except OSError:
            pass
        return None

    return {
        "uname": platform.uname()._asdict(),
        "kernel": platform.release(),
        "nproc": os.cpu_count(),
        "cpu_model": cpu_model(),
        "mem_total_kib": mem_total(),
        "python": sys.version,
        "sqlite_version": sqlite3.sqlite_version,
        "court_image": os.environ.get("WEFT_COURT_IMAGE"),
        "court_image_id": os.environ.get("WEFT_COURT_IMAGE_ID"),
        "court_memory": os.environ.get("WEFT_COURT_MEMORY"),
        "court_cpus": os.environ.get("WEFT_COURT_CPUS"),
        "court_pids": os.environ.get("WEFT_COURT_PIDS"),
        "docker_client": os.environ.get("WEFT_DOCKER_CLIENT"),
        "docker_server": os.environ.get("WEFT_DOCKER_SERVER"),
        "revision_weftfs": os.environ.get("WEFT_REPO_WEFTFS"),
        "revision_weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO"),
        "revision_weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC"),
        "revision_weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO"),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--sizes", default="")
    parser.add_argument("--work-dir", default="/work/.docker-cache/bench-external")
    parser.add_argument("--warm-iterations", type=int, default=0)
    parser.add_argument("--cold-iterations", type=int, default=5)
    args = parser.parse_args()

    manifest_doc = json.loads(Path(args.manifest).read_text())
    sizes = set(int(x) for x in args.sizes.split(",") if x.strip())
    work_dir = Path(args.work_dir)
    work_dir.mkdir(parents=True, exist_ok=True)

    results = []
    started = time.time()
    for manifest in manifest_doc["sizes"]:
        size = int(manifest["size"])
        if sizes and size not in sizes:
            continue
        warm_iterations = args.warm_iterations or (10 if size >= 10_000 else 50)
        for variant in ("relational", "adjacency"):
            print(f"[external] {variant} size={size}", flush=True)
            path = work_dir / f"{variant}-{size}.sqlite"
            for suffix in ("", "-wal", "-shm"):
                p = Path(str(path) + suffix)
                if p.exists():
                    p.unlink()
            db = Db(path, variant)
            build_start = ns()
            build = table_build(db, size, manifest)
            build_ns = ns() - build_start
            index = build_indexes(db)
            queries, plans = run_queries(db, manifest, warm_iterations, args.cold_iterations)
            churn = run_churn(db, manifest, 10 if size >= 10_000 else 25)
            db.checkpoint()
            total_bytes, files = db.disk_bytes()
            row_counts = {
                "objects": db.conn.execute(
                    "SELECT count(*) FROM objects" if variant == "relational" else "SELECT count(*) FROM nodes"
                ).fetchone()[0],
                "relations": db.conn.execute(
                    "SELECT count(*) FROM relations" if variant == "relational" else "SELECT count(*) FROM edges"
                ).fetchone()[0],
                "snapshots": db.conn.execute("SELECT count(*) FROM snapshots").fetchone()[0],
            }
            db.close()
            results.append(
                {
                    "variant": variant,
                    "size": size,
                    "db_path": str(path),
                    "schema_sql": RELATIONAL_SCHEMA if variant == "relational" else ADJACENCY_SCHEMA,
                    "index_sql": index["sql"],
                    "build_ns": build_ns,
                    "load": build,
                    "index_build": index,
                    "queries": queries,
                    "query_plans": plans,
                    "churn": churn,
                    "disk": {
                        "total_bytes": total_bytes,
                        "files": files,
                        "row_counts": row_counts,
                    },
                }
            )

    report = {
        "schema": "weft-external-bench-v1",
        "kind": "external-sqlite-baseline",
        "generated_at_unix": int(started),
        "wall_time_s": time.time() - started,
        "environment": environment(),
        "corpus_manifest": args.manifest,
        "corpus_manifest_sha256": hashlib.sha256(Path(args.manifest).read_bytes()).hexdigest(),
        "command": " ".join(sys.argv),
        "method": {
            "engine": "Python 3 stdlib sqlite3 (SQLite linked into the interpreter)",
            "journal_mode": "WAL",
            "synchronous": "FULL",
            "cache_size_kib": 8000,
            "mmap_size": 0,
            "warm": "repeated executions on one open connection after 5 warmups",
            "cold": "close connection, POSIX_FADV_DONTNEED on db/-wal/-shm, reopen, then one query",
            "adjacency_label": "SQLite relational/adjacency baseline; not a dedicated graph database",
            "no_graph_db_claim": "No Neo4j, SPARQL, or other graph/triple store was run.",
            "same_corpus": "Payload bytes, ObjectIds, relations, tags, snapshot names, and commit generations come from examples/bench_external_manifest.rs, which uses the same definitions as examples/bench_store.rs.",
            "durability": "WAL + synchronous=FULL; each churn insert/delete is its own committed transaction.",
            "cold_limitation": "Cold uses per-file page-cache eviction, not a device cache drop; the VM fio court performs a guest drop_caches.",
        },
        "results": results,
    }
    Path(args.out).write_text(json.dumps(report, indent=2, sort_keys=False))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
