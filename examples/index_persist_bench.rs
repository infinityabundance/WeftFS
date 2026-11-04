//! Persisted per-commit index segment benchmark.
//!
//! Builds one synthetic 10,000-object / 10,000-relation store, then measures in
//! a single process: cold build from state, write-to-disk, load-from-disk, and
//! the first relation/tag query after a fresh Store reopen with the persisted
//! segment present versus deleted.
//!
//! Run in the Docker court (release):
//!   cargo run --release --example index_persist_bench

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

const OBJECTS: usize = 10_000;
const RELATIONS: usize = 10_000;
const TRIALS: usize = 5;

fn principal() -> Principal {
    Principal::new(0, 0, Vec::new())
}

fn timed<T, F: FnMut() -> T>(mut action: F) -> (f64, T) {
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
    // Howard Hinnant's civil-from-days algorithm.
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

fn build_store(dir: &Path) -> (Store, Vec<Hash32>, Hash32) {
    let mut store = Store::init(
        dir,
        Options {
            owner_uid: 0,
            owner_gid: 0,
            chunk_size: 1024,
            checkpoint_interval: 256,
            gc_grace_secs: 0,
            fixed_timestamp_ns: None,
            ..Options::default()
        },
    )
    .expect("init store");
    let root = store.resolve_path("/").expect("root");
    let teams = ["red", "green", "blue"];
    let mut ids = Vec::with_capacity(OBJECTS);
    let mut relation_target = Hash32::ZERO;

    // One group commit: 10,000 creates and 10,000 relations become exactly one
    // durable commit, so the benchmark measures index persistence rather than
    // ten thousand commit publications.
    store
        .group(|s| {
            for index in 0..OBJECTS {
                let (mode, uid, gid) = if index % 7 == 0 {
                    (0o600, 2000, 2000)
                } else {
                    (0o644, 1000, 1000)
                };
                let mut attrs = BTreeMap::new();
                attrs.insert(
                    "team".to_owned(),
                    Value::Str(teams[index % teams.len()].to_owned()),
                );
                attrs.insert("score".to_owned(), Value::U64((index % 100) as u64));
                let mut nested = BTreeMap::new();
                nested.insert("a".to_owned(), Value::U64((index % 7) as u64));
                attrs.insert("nested".to_owned(), Value::Map(nested));
                let id = s.create_file_seeded(
                    root,
                    format!("f{index}").as_bytes(),
                    format!("payload-{index}").as_bytes(),
                    mode,
                    uid,
                    gid,
                    attrs,
                    format!("persist-bench-object-{index}").as_bytes(),
                )?;
                ids.push(id);
            }
            for index in 0..RELATIONS / 2 {
                let subject = ids[index];
                let target = ids[(index * 37 + 11) % OBJECTS];
                // Index 1 has an authorized subject, so the first relation
                // query has a non-empty result after authorization filtering.
                if index == 1 {
                    relation_target = target;
                }
                s.relation_add(
                    subject,
                    "links",
                    RelationTarget::Object(target),
                    None,
                    BTreeMap::new(),
                )?;
                s.relation_add(
                    subject,
                    "tagged_as",
                    RelationTarget::Value(Value::Str(format!("group-{}", index % 50))),
                    None,
                    BTreeMap::new(),
                )?;
            }
            Ok(())
        })
        .expect("group commit");
    (store, ids, relation_target)
}

fn first_query(
    root: &Path,
    selector: &Selector,
    principal: &Principal,
    limits: &weftcodec::Limits,
) -> (f64, u64, u64, u64) {
    let store = Store::open(root, OpenMode::ReadOnly).expect("reopen read-only");
    store.reset_index_stats();
    let (ms, resolution) = timed(|| {
        store
            .resolve(selector, None, principal, limits)
            .expect("indexed resolution")
    });
    let stats = store.index_stats();
    (ms, stats.disk_loads, stats.builds, resolution.matched_count)
}

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, ids, link_target) = build_store(dir.path());
    let head = store.head();
    let root = store.root().to_path_buf();
    let principal = principal();
    let limits = weftcodec::Limits::default();

    let relation_selector = selector::parse(&format!(
        "select relation(links, obj:{})",
        link_target.to_hex()
    ))
    .expect("relation selector");
    let tag_selector = selector::parse("select tag(\"group-0\")").expect("tag selector");
    let path = index_path(&root, head);

    // (a) Cold build from replayed state.
    let mut cold_build_ms = Vec::with_capacity(TRIALS);
    for _ in 0..TRIALS {
        let (ms, count) = timed(|| store.build_index_for_bench(store.current_state()));
        assert_eq!(count, OBJECTS + 1, "indexed objects includes the root");
        cold_build_ms.push(ms);
    }

    // (b) Write-to-disk: build + encode + fsync + rename + directory fsync.
    let mut write_ms = Vec::with_capacity(TRIALS);
    let mut index_bytes = 0u64;
    for _ in 0..TRIALS {
        let (ms, written) = timed(|| store.write_index_for_bench().expect("write index"));
        index_bytes = written;
        write_ms.push(ms);
    }
    assert!(path.exists(), "index file exists");
    assert_eq!(
        fs::metadata(&path).expect("index metadata").len(),
        index_bytes,
        "reported size matches the file"
    );

    // Equivalence against the scan oracle while the persisted index is in use.
    {
        let ro = Store::open(&root, OpenMode::ReadOnly).expect("read-only");
        let indexed = ro
            .resolve(&relation_selector, None, &principal, &limits)
            .expect("indexed relation");
        let scan = ro
            .resolve_scan(&relation_selector, None, &principal, &limits)
            .expect("scan relation");
        assert_eq!(indexed.matched_count, scan.matched_count);
        assert_eq!(indexed.results, scan.results);
        let indexed_tag = ro
            .resolve(&tag_selector, None, &principal, &limits)
            .unwrap();
        let scan_tag = ro
            .resolve_scan(&tag_selector, None, &principal, &limits)
            .unwrap();
        assert_eq!(indexed_tag.matched_count, scan_tag.matched_count);
        assert_eq!(indexed_tag.results, scan_tag.results);
    }

    // (c)/(d) Fresh Store reopen with the persisted segment present.
    let mut load_relation_ms = Vec::with_capacity(TRIALS);
    let mut load_tag_ms = Vec::with_capacity(TRIALS);
    for _ in 0..TRIALS {
        let (ms, loads, builds, matched) =
            first_query(&root, &relation_selector, &principal, &limits);
        assert_eq!((loads, builds), (1, 0), "persisted relation must load");
        assert!(matched > 0, "relation query matches visible objects");
        load_relation_ms.push(ms);
    }
    for _ in 0..TRIALS {
        let (ms, loads, builds, matched) = first_query(&root, &tag_selector, &principal, &limits);
        assert_eq!((loads, builds), (1, 0), "persisted tag must load");
        assert!(matched > 0, "tag query matches visible objects");
        load_tag_ms.push(ms);
    }

    // Same fresh reopens with the segment deleted: an in-memory rebuild.
    fs::remove_file(&path).expect("remove persisted index");
    let mut rebuild_relation_ms = Vec::with_capacity(TRIALS);
    let mut rebuild_tag_ms = Vec::with_capacity(TRIALS);
    for _ in 0..TRIALS {
        let (ms, loads, builds, matched) =
            first_query(&root, &relation_selector, &principal, &limits);
        assert_eq!((loads, builds), (0, 1), "deleted relation must rebuild");
        assert!(matched > 0);
        rebuild_relation_ms.push(ms);
    }
    for _ in 0..TRIALS {
        let (ms, loads, builds, matched) = first_query(&root, &tag_selector, &principal, &limits);
        assert_eq!((loads, builds), (0, 1), "deleted tag must rebuild");
        assert!(matched > 0);
        rebuild_tag_ms.push(ms);
    }
    // Leave the store with its advisory segment present.
    store.write_index_for_bench().expect("restore index");
    assert!(path.exists());

    let generated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let stamp = utc_iso(generated);
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| "unknown".to_owned());
    let receipt = json!({
        "schema": "weftfs.index-persist-bench.v1",
        "label": "synthetic, tmpfs, single host; fresh Store instance in one process; OS page cache warm",
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
            "docker_server": env("WEFT_DOCKER_SERVER"),
        },
        "state": {
            "objects": OBJECTS,
            "relations": RELATIONS,
            "visible_objects": ids.iter().enumerate().filter(|(index, _)| index % 7 != 0).count(),
        },
        "index": {
            "file_bytes": index_bytes,
            "file": path.display().to_string(),
            "commit": head.to_hex(),
        },
        "measurements_ms": {
            "trials": TRIALS,
            "cold_build_from_state": {
                "median": median(&cold_build_ms),
                "samples": cold_build_ms,
            },
            "write_to_disk_including_build": {
                "median": median(&write_ms),
                "samples": write_ms,
            },
            "first_relation_query_persisted": {
                "median": median(&load_relation_ms),
                "samples": load_relation_ms,
            },
            "first_tag_query_persisted": {
                "median": median(&load_tag_ms),
                "samples": load_tag_ms,
            },
            "first_relation_query_index_deleted": {
                "median": median(&rebuild_relation_ms),
                "samples": rebuild_relation_ms,
            },
            "first_tag_query_index_deleted": {
                "median": median(&rebuild_tag_ms),
                "samples": rebuild_tag_ms,
            },
        },
        "checks": {
            "persisted_first_query_disk_loads": 1,
            "persisted_first_query_builds": 0,
            "deleted_first_query_disk_loads": 0,
            "deleted_first_query_builds": 1,
            "relation_matches_scan": true,
            "tag_matches_scan": true,
        },
        "peak_rss_kib": peak_rss_kib(),
    });

    let receipts = Path::new("bench/receipts");
    fs::create_dir_all(receipts).expect("receipts directory");
    let path = receipts.join(format!("index-persist-{stamp}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");

    println!(
        "index-persist bench: objects={OBJECTS} relations={RELATIONS} index_bytes={index_bytes} peak_rss_kib={}",
        peak_rss_kib()
    );
    println!("{:<42} {:>12}", "measurement", "median_ms");
    println!(
        "{:<42} {:>12.3}",
        "cold_build_from_state",
        median(&cold_build_ms)
    );
    println!(
        "{:<42} {:>12.3}",
        "write_to_disk_including_build",
        median(&write_ms)
    );
    println!(
        "{:<42} {:>12.3}",
        "first_relation_query_persisted",
        median(&load_relation_ms)
    );
    println!(
        "{:<42} {:>12.3}",
        "first_tag_query_persisted",
        median(&load_tag_ms)
    );
    println!(
        "{:<42} {:>12.3}",
        "first_relation_query_index_deleted",
        median(&rebuild_relation_ms)
    );
    println!(
        "{:<42} {:>12.3}",
        "first_tag_query_index_deleted",
        median(&rebuild_tag_ms)
    );
    println!("receipt: {}", path.display());
}
