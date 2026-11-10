//! Incremental advisory index segments (WFX3): amplification, commit overhead,
//! chain read cost, compaction, and GC.
//!
//! Builds the same 1,000-commit file workload twice: once with the pre-WFX3
//! writer (`incremental_index = false`, a full-state segment per commit) and
//! once with the default delta chain. It reports, per mode:
//!
//! * total advisory index bytes and the data-tree bytes they describe;
//! * per-commit wall time and per-commit index bytes written;
//! * GC of unreferenced segments;
//! * first-query p50 on a fresh read-only handle (chain load vs full segment);
//! * compaction cost and the post-compaction query p50.
//!
//! It then builds one more delta store with a snapshot pinned at every commit,
//! which is the retention shape that produced the 3,466,466,150 B figure.
//!
//! Run in the Docker court (release):
//!   cargo run --release --example index_chain_bench

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use weftcodec::selector::{self, Selector};
use weftcodec::wcb::Value;

use weftfs::auth::Principal;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};

const COMMITS: usize = 1_000;
const QUERY_OPENS: usize = 7;
const RELATION_EVERY: usize = 4;

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn timed<T, F: FnOnce() -> T>(action: F) -> (f64, T) {
    let start = Instant::now();
    let value = action();
    (start.elapsed().as_secs_f64() * 1000.0, value)
}

fn median(values: &[f64]) -> f64 {
    assert!(!values.is_empty());
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    assert!(!values.is_empty());
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn dir_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_bytes(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

fn data_bytes(root: &Path) -> u64 {
    dir_bytes(&root.join("objects")) + dir_bytes(&root.join("manifests"))
}

fn index_path(root: &Path, commit: Hash32) -> PathBuf {
    let hex = commit.to_hex();
    root.join("indexes")
        .join(&hex[0..2])
        .join(format!("{hex}.idx"))
}

fn peak_rss_kib() -> u64 {
    let text = fs::read_to_string("/proc/self/status").unwrap_or_default();
    text.lines()
        .find_map(|line| {
            let rest = line.strip_prefix("VmHWM:")?;
            rest.split_whitespace().next()?.parse().ok()
        })
        .unwrap_or(0)
}

fn utc_iso(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

struct Built {
    root: PathBuf,
    commit_ms: Vec<f64>,
    index_bytes_per_commit: Vec<f64>,
    data_bytes: u64,
    index_bytes: u64,
    head: Hash32,
    index_notes: usize,
    segments: u64,
    ids: Vec<Hash32>,
}

fn build_one(root: &Path, full: bool, snapshots: bool) -> Built {
    if root.exists() {
        fs::remove_dir_all(root).expect("clean store dir");
    }
    let mut store = Store::init(
        root,
        Options {
            owner_uid: 0,
            owner_gid: 0,
            chunk_size: 1024,
            checkpoint_interval: 256,
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            incremental_index: !full,
            ..Options::default()
        },
    )
    .expect("init store");
    let dir = store.resolve_path("/").expect("root");
    let mut commit_ms = Vec::with_capacity(COMMITS);
    let mut index_bytes_per_commit = Vec::with_capacity(COMMITS);
    let mut ids = Vec::with_capacity(COMMITS);
    for index in 0..COMMITS {
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "team".to_owned(),
            Value::Str(["red", "green", "blue"][index % 3].to_owned()),
        );
        attrs.insert("score".to_owned(), Value::U64((index % 100) as u64));
        let (ms, id) = timed(|| {
            store
                .create_file_seeded(
                    dir,
                    format!("f{index}").as_bytes(),
                    format!("payload-{index}").as_bytes(),
                    0o644,
                    0,
                    0,
                    attrs,
                    format!("index-chain-bench-{index}").as_bytes(),
                )
                .expect("create file")
        });
        if index % RELATION_EVERY == 0 && !ids.is_empty() {
            let target = ids[index / 2];
            store
                .relation_add(
                    id,
                    "links",
                    RelationTarget::Object(target),
                    None,
                    BTreeMap::new(),
                )
                .expect("relation");
        }
        ids.push(id);
        if snapshots {
            store
                .snapshot_create(&format!("s{index}"))
                .expect("snapshot");
        }
        let head = store.head();
        let size = fs::metadata(index_path(root, head))
            .map(|meta| meta.len())
            .unwrap_or(0);
        commit_ms.push(ms);
        index_bytes_per_commit.push(size as f64);
    }
    let head = store.head();
    let index_notes = store.index_stats().notes.len();
    let bytes = store.index_bytes_total().expect("index bytes total");
    let segments = fs::read_dir(root.join("indexes"))
        .expect("indexes dir")
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .flat_map(|entry| fs::read_dir(entry.path()).into_iter().flatten().flatten())
        .count() as u64;
    Built {
        root: root.to_path_buf(),
        commit_ms,
        index_bytes_per_commit,
        data_bytes: data_bytes(root),
        index_bytes: bytes,
        head,
        index_notes,
        segments,
        ids,
    }
}

fn reopen_query(
    root: &Path,
    selector: &Selector,
    principal: &Principal,
) -> (Vec<f64>, Vec<u64>, Vec<u64>) {
    let limits = weftcodec::Limits::default();
    let mut samples = Vec::with_capacity(QUERY_OPENS);
    let mut loads = Vec::with_capacity(QUERY_OPENS);
    let mut builds = Vec::with_capacity(QUERY_OPENS);
    for _ in 0..QUERY_OPENS {
        let store = Store::open(root, OpenMode::ReadOnly).expect("reopen read-only");
        store.reset_index_stats();
        let (ms, _) = timed(|| {
            store
                .resolve(selector, None, principal, &limits)
                .expect("indexed resolution")
        });
        let stats = store.index_stats();
        samples.push(ms);
        loads.push(stats.disk_loads);
        builds.push(stats.builds);
    }
    (samples, loads, builds)
}

fn mode_report(
    built: &Built,
    root: &Path,
    principal: &Principal,
    selectors: &[(&str, Selector)],
) -> serde_json::Value {
    let mut queries = Vec::new();
    for (label, selector) in selectors {
        let (samples, loads, builds) = reopen_query(root, selector, principal);
        queries.push(json!({
            "label": label,
            "p50_ms": median(&samples),
            "samples_ms": samples,
            "disk_loads": loads,
            "builds": builds,
        }));
    }
    let mut stats = Store::open(root, OpenMode::ReadWrite).expect("reopen writable");
    let (gc_ms, report) = timed(|| stats.gc(true).expect("gc"));
    let index_after_gc = dir_bytes(&root.join("indexes"));
    let (compact_ms, new_head) = timed(|| stats.compact_index().expect("compact index"));
    let (kind, depth) = stats.index_head_chain().expect("head chain");
    let index_after_compact = dir_bytes(&root.join("indexes"));
    let (gc2_ms, report2) = timed(|| stats.gc(true).expect("gc after compaction"));
    let index_after_compact_gc = dir_bytes(&root.join("indexes"));
    let compacted_queries = {
        let (samples, loads, builds) = reopen_query(root, &selectors[0].1, principal);
        json!({
            "label": selectors[0].0,
            "p50_ms": median(&samples),
            "samples_ms": samples,
            "disk_loads": loads,
            "builds": builds,
        })
    };
    json!({
        "root": root.display().to_string(),
        "head": built.head.to_hex(),
        "segments": built.segments,
        "data_bytes": built.data_bytes,
        "index_bytes": built.index_bytes,
        "index_bytes_ppm_of_data": if built.data_bytes == 0 {
            0
        } else {
            (built.index_bytes as f64 / built.data_bytes as f64 * 1_000_000.0).round() as u64
        },
        "index_notes": built.index_notes,
        "commit_ms": {
            "median": median(&built.commit_ms),
            "p50": percentile(&built.commit_ms, 0.50),
            "p95": percentile(&built.commit_ms, 0.95),
            "max": built.commit_ms.iter().copied().fold(0.0, f64::max),
            "samples": built.commit_ms,
        },
        "index_bytes_per_commit": {
            "median": median(&built.index_bytes_per_commit),
            "p95": percentile(&built.index_bytes_per_commit, 0.95),
            "max": built.index_bytes_per_commit.iter().copied().fold(0.0, f64::max),
            "samples": built.index_bytes_per_commit,
        },
        "first_query_after_reopen": queries,
        "gc": {
            "ms": gc_ms,
            "deleted_blobs": report.deleted.len(),
            "deleted_indexes": report.deleted_indexes.len(),
            "deleted_index_bytes": report.deleted_index_bytes,
            "index_bytes_after_gc": index_after_gc,
            "problems": report.problems,
        },
        "compaction": {
            "ms": compact_ms,
            "new_head": new_head.to_hex(),
            "head_segment": kind,
            "head_chain_depth": depth,
            "index_bytes_after_compaction": index_after_compact,
            "gc_after_compaction_ms": gc2_ms,
            "gc_after_compaction_deleted_index_bytes": report2.deleted_index_bytes,
            "index_bytes_after_compaction_gc": index_after_compact_gc,
        },
        "first_query_after_compaction": compacted_queries,
    })
}

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let principal = principal();
    let full_root = dir.path().join("full");
    let delta_root = dir.path().join("delta");
    let pinned_root = dir.path().join("delta-pinned");
    let full = build_one(&full_root, true, false);
    let delta = build_one(&delta_root, false, false);
    let pinned = build_one(&pinned_root, false, true);

    let relation = selector::parse(&format!(
        "select relation(links, obj:{})",
        full.ids[0].to_hex()
    ))
    .expect("relation");
    let attr = selector::parse("select attrs.team == \"red\"").expect("attr");
    let has = selector::parse("select has(attrs.team)").expect("has");
    let all = selector::parse("select true").expect("all");
    let selectors = vec![
        ("relation", relation),
        ("attr_eq", attr),
        ("has_attr", has),
        ("all_true", all),
    ];

    let full_report = mode_report(&full, &full_root, &principal, &selectors);
    let delta_report = mode_report(&delta, &delta_root, &principal, &selectors);

    // The pinned store keeps a snapshot at every commit, which is the
    // trajectory retention shape. Report its live index tree and what GC may
    // drop; no compaction here.
    let mut pinned_store = Store::open(&pinned_root, OpenMode::ReadWrite).expect("pinned store");
    let (pinned_gc_ms, pinned_gc) = timed(|| pinned_store.gc(true).expect("pinned gc"));
    drop(pinned_store);
    let pinned_report = json!({
        "root": pinned_root.display().to_string(),
        "snapshots": COMMITS,
        "segments": pinned.segments,
        "data_bytes": pinned.data_bytes,
        "index_bytes": pinned.index_bytes,
        "index_bytes_ppm_of_data": if pinned.data_bytes == 0 {
            0
        } else {
            (pinned.index_bytes as f64 / pinned.data_bytes as f64 * 1_000_000.0).round() as u64
        },
        "gc_ms": pinned_gc_ms,
        "gc_deleted_indexes": pinned_gc.deleted_indexes.len(),
        "gc_deleted_index_bytes": pinned_gc.deleted_index_bytes,
        "index_bytes_after_gc": dir_bytes(&pinned_root.join("indexes")),
        "gc_problems": pinned_gc.problems,
    });

    let generated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let stamp = utc_iso(generated);
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| "unknown".to_owned());
    let receipt = json!({
        "schema": "weftfs.index-chain-bench.v1",
        "label": "synthetic, tmpfs, single host; one process; 1,000 sequential commits; warm page cache",
        "generated_unix_secs": generated,
        "generated_utc": stamp,
        "revision": env("WEFT_REPO_WEFTFS"),
        "package_version": env!("CARGO_PKG_VERSION"),
        "profile": "release",
        "environment": {
            "court_image": env("WEFT_COURT_IMAGE"),
            "court_image_id": env("WEFT_COURT_IMAGE_ID"),
            "kernel": env("WEFT_COURT_KERNEL"),
            "memory_limit": env("WEFT_COURT_MEMORY"),
            "cpus": env("WEFT_COURT_CPUS"),
        },
        "workload": {
            "commits": COMMITS,
            "objects": COMMITS,
            "relations": (COMMITS + RELATION_EVERY - 1) / RELATION_EVERY,
            "delta_chain_max": 32,
        },
        "full_segments": full_report,
        "delta_chain": delta_report,
        "delta_chain_pinned_every_commit": pinned_report,
        "peak_rss_kib": peak_rss_kib(),
    });

    let receipts = Path::new("bench/receipts");
    fs::create_dir_all(receipts).expect("receipts directory");
    let path = receipts.join(format!("index-chain-{stamp}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");

    println!("index-chain bench receipt: {}", path.display());
    for (label, report) in [("full", &full_report), ("delta", &delta_report)] {
        println!(
            "{label}: data={} index={} ratio_ppm={} commit_p50={:.3} ms commit_p95={:.3} ms \
             index_bytes_per_commit_p50={:.0} segments={}",
            report["data_bytes"],
            report["index_bytes"],
            report["index_bytes_ppm_of_data"],
            report["commit_ms"]["p50"].as_f64().unwrap_or(0.0),
            report["commit_ms"]["p95"].as_f64().unwrap_or(0.0),
            report["index_bytes_per_commit"]["median"]
                .as_f64()
                .unwrap_or(0.0),
            report["segments"],
        );
    }
    println!(
        "pinned: data={} index={} ratio_ppm={} gc_deleted_index_bytes={}",
        pinned_report["data_bytes"],
        pinned_report["index_bytes"],
        pinned_report["index_bytes_ppm_of_data"],
        pinned_report["gc_deleted_index_bytes"],
    );
}
