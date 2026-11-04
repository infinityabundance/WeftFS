//! Group-commit benchmark: N small mutations as N durable commits versus one.
//!
//! Label: a local **tmpfs** synthetic workload run in the `weft/rust-court:1`
//! Docker court (`TMPDIR=/tmp`, a 1 GiB tmpfs). It measures the store API, not a
//! device, FUSE mount, or fleet. Absolute numbers are dominated by tmpfs, so
//! the meaningful output is the ratio and the generation count.
//!
//! Run in the Docker court:
//!   cargo run --release --example group_commit_bench

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tempfile::TempDir;

use weftfs::identity::Hash32;
use weftfs::store::{Options, Store};

const OBJECTS: usize = 1_000;
const RUNS: usize = 5;
const FIXED_TIMESTAMP_NS: i64 = 1_700_000_000_000_000_000;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
    }
}

fn payload(index: usize) -> Vec<u8> {
    // 32-byte payload: representative of a small metadata-ish object.
    format!("group-commit-bench payload {index:04}\n").into_bytes()
}

fn name(index: usize) -> Vec<u8> {
    format!("obj-{index:04}").into_bytes()
}

fn seed(index: usize) -> Vec<u8> {
    format!("group-bench-seed-{index:04}").into_bytes()
}

fn create(store: &mut Store, root: Hash32, index: usize) -> weftfs::Result<Hash32> {
    store.create_file_seeded(
        root,
        &name(index),
        &payload(index),
        0o644,
        0,
        0,
        BTreeMap::new(),
        &seed(index),
    )
}

/// One fresh store in the court's tmpfs.
fn fresh_store(tag: &str) -> (TempDir, Store) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("weft-group-bench-{tag}-"))
        .tempdir_in(std::env::temp_dir())
        .expect("temp dir on the court tmpfs");
    let store = Store::init(dir.path(), opts()).expect("init store");
    (dir, store)
}

fn timed<T, F: FnOnce() -> T>(action: F) -> (f64, T) {
    let start = Instant::now();
    let value = action();
    (start.elapsed().as_secs_f64() * 1000.0, value)
}

fn median(values: &mut [f64]) -> f64 {
    assert!(!values.is_empty());
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite timings"));
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn max(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

/// Verify every object of a finished run is present and readable.
fn check_store(store: &Store, expected_generation: u64) -> (usize, bool) {
    assert_eq!(store.generation(), expected_generation, "generation count");
    let root = store.resolve_path("/").expect("root");
    for index in 0..OBJECTS {
        let id = store.lookup(root, &name(index)).expect("entry");
        let bytes = store.read_object(id).expect("readable object");
        assert_eq!(bytes, payload(index), "payload {index}");
    }
    let report = store.verify(true).expect("verify");
    assert!(report.ok, "problems: {:?}", report.problems);
    (OBJECTS, true)
}

fn run_individual() -> (f64, u64) {
    let (_dir, mut store) = fresh_store("individual");
    let root = store.resolve_path("/").expect("root");
    let before = store.generation();
    let (ms, result) = timed(|| {
        let mut out = Vec::with_capacity(OBJECTS);
        for index in 0..OBJECTS {
            out.push(create(&mut store, root, index).expect("create"));
        }
        out
    });
    assert_eq!(result.len(), OBJECTS);
    let generations = store.generation() - before;
    check_store(&store, before + generations);
    (ms, generations)
}

fn run_grouped() -> (f64, u64) {
    let (_dir, mut store) = fresh_store("grouped");
    let root = store.resolve_path("/").expect("root");
    let before = store.generation();
    let (ms, result) = timed(|| {
        store
            .group(|store| {
                for index in 0..OBJECTS {
                    create(store, root, index)?;
                }
                Ok(())
            })
            .expect("group of creates")
    });
    let generations = store.generation() - before;
    assert_eq!(generations, 1, "a group publishes one commit");
    assert_eq!(store.snapshot_root(), result.1);
    check_store(&store, before + generations);
    (ms, generations)
}

/// Create the corpus in one group (untimed), then time one group of 1,000 range
/// writes.
fn run_grouped_writes() -> (f64, u64) {
    let (_dir, mut store) = fresh_store("writes");
    let root = store.resolve_path("/").expect("root");
    let ids: Vec<Hash32> = store
        .group(|store| {
            let mut ids = Vec::with_capacity(OBJECTS);
            for index in 0..OBJECTS {
                ids.push(create(store, root, index)?);
            }
            Ok(ids)
        })
        .expect("setup group")
        .0;

    let before = store.generation();
    let (ms, _commit) = timed(|| {
        store
            .group(|store| {
                for (index, id) in ids.iter().enumerate() {
                    store.write_file(*id, 8, &format!("write-{index:04}").into_bytes())?;
                }
                Ok(())
            })
            .expect("group of writes")
    });
    let generations = store.generation() - before;
    assert_eq!(generations, 1, "a group of writes publishes one commit");
    // Every write landed: bytes 8..18 were replaced.
    for (index, id) in ids.iter().enumerate() {
        let bytes = store.read_object(*id).expect("readable after group write");
        assert_eq!(&bytes[8..18], format!("write-{index:04}").as_bytes());
    }
    let report = store.verify(true).expect("verify");
    assert!(report.ok, "problems: {:?}", report.problems);
    (ms, generations)
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unknown".to_owned())
}

/// `YYYYMMDDThhmmssZ` for the receipt filename.
fn utc_stamp(unix_secs: u64) -> String {
    let (year, month, day) = civil_from_days((unix_secs / 86_400) as i64);
    let seconds_of_day = unix_secs % 86_400;
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60
    )
}

/// RFC 3339 UTC for the receipt body.
fn utc_iso(unix_secs: u64) -> String {
    let stamp = utc_stamp(unix_secs);
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        &stamp[0..4],
        &stamp[4..6],
        &stamp[6..8],
        &stamp[9..11],
        &stamp[11..13],
        &stamp[13..15]
    )
}

/// Howard Hinnant's civil-from-days, days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn summary(samples: &[f64]) -> serde_json::Value {
    let mut copy = samples.to_vec();
    json!({
        "samples_ms": samples,
        "median_ms": median(&mut copy),
        "max_ms": max(samples),
    })
}

fn main() {
    println!(
        "group-commit bench (tmpfs, rust court): objects={OBJECTS} runs={RUNS} payload={}B",
        payload(0).len()
    );

    let mut individual_ms = Vec::with_capacity(RUNS);
    let mut grouped_ms = Vec::with_capacity(RUNS);
    let mut writes_ms = Vec::with_capacity(RUNS);
    let mut individual_generations = 0;
    let mut grouped_generations = 0;
    let mut write_generations = 0;

    for run in 0..RUNS {
        let (ms, generations) = run_individual();
        individual_ms.push(ms);
        individual_generations = generations;
        let (ms, generations) = run_grouped();
        grouped_ms.push(ms);
        grouped_generations = generations;
        let (ms, generations) = run_grouped_writes();
        writes_ms.push(ms);
        write_generations = generations;
        println!(
            "run {}: individual={individual_ms:.2} ms  group-create={grouped_ms:.2} ms  group-writes={writes_ms:.2} ms",
            run + 1,
            individual_ms = individual_ms[run],
            grouped_ms = grouped_ms[run],
            writes_ms = writes_ms[run]
        );
    }

    let mut individual_sorted = individual_ms.clone();
    let mut grouped_sorted = grouped_ms.clone();
    let mut writes_sorted = writes_ms.clone();
    let individual_median = median(&mut individual_sorted);
    let grouped_median = median(&mut grouped_sorted);
    let writes_median = median(&mut writes_sorted);

    println!();
    println!("{:<22} {:>12} {:>10}", "workload", "median_ms", "commits");
    println!(
        "{:<22} {:>12.2} {:>10}",
        "1000 creates singly", individual_median, individual_generations
    );
    println!(
        "{:<22} {:>12.2} {:>10}",
        "1000 creates one group", grouped_median, grouped_generations
    );
    println!(
        "{:<22} {:>12.2} {:>10}",
        "1000 writes one group", writes_median, write_generations
    );
    println!(
        "speedup (creates): {:.2}x",
        individual_median / grouped_median
    );

    // Final sanity: a read-only reopen sees the last grouped store intact.
    let generated_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let stamp = utc_stamp(generated_unix);

    let receipt = json!({
        "schema": "weftfs.group-commit-bench.v1",
        "label": "local tmpfs synthetic workload (TMPDIR=/tmp in the rust Docker court); store API timing only, not a device, FUSE, or fleet claim",
        "generated_utc": utc_iso(generated_unix),
        "generated_unix_secs": generated_unix,
        "revision": env("WEFT_REPO_WEFTFS"),
        "package_version": env!("CARGO_PKG_VERSION"),
        "profile": "release",
        "environment": {
            "court_image": env("WEFT_COURT_IMAGE"),
            "court_image_id": env("WEFT_COURT_IMAGE_ID"),
            "kernel": env("WEFT_COURT_KERNEL"),
            "memory_limit": env("WEFT_COURT_MEMORY"),
            "cpus": env("WEFT_COURT_CPUS"),
            "pids_limit": env("WEFT_COURT_PIDS"),
            "docker_server": env("WEFT_DOCKER_SERVER"),
            "storage": "tmpfs at TMPDIR=/tmp (court 1 GiB tmpfs mount)",
        },
        "workload": {
            "objects": OBJECTS,
            "payload_bytes": payload(0).len(),
            "iterations": RUNS,
            "chunk_size": 65_536,
            "checkpoint_interval": 256,
            "fixed_timestamp_ns": FIXED_TIMESTAMP_NS,
            "statistic": "median wall-clock milliseconds over 5 runs; each run uses a fresh store",
        },
        "results": {
            "creates_individual": summary(&individual_ms),
            "creates_one_group": summary(&grouped_ms),
            "writes_one_group": summary(&writes_ms),
            "create_speedup": individual_median / grouped_median,
        },
        "generations": {
            "creates_individual": individual_generations,
            "creates_one_group": grouped_generations,
            "writes_one_group": write_generations,
        },
        "correctness": {
            "generation_increase_per_group": 1,
            "objects_read_back": OBJECTS,
            "verify_full": "ok on every run",
        },
    });

    let receipts = Path::new("bench/receipts");
    fs::create_dir_all(receipts).expect("receipts directory");
    let path: PathBuf = receipts.join(format!("group-commit-{stamp}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
    println!("receipt: {}", path.display());
}
