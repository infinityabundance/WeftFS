//! Synthetic measurement of extent-level copy-on-write range writes.
//!
//! One 64 MiB object (64 KiB chunks) receives 20 overwrites of 4 KiB at
//! deterministic 4 KiB-aligned offsets. The same workload runs twice, each in
//! its own child process so peak RSS is attributable to that path only:
//!
//! * `new` - `Store::write_file`, the chunk-granular COW path.
//! * `old` - `Store::write_file_whole_file_for_bench`, the previous whole-file
//!   implementation (read the entire object, splice, re-store every chunk).
//!
//! Per iteration we record wall time and the `rchar`/`wchar` deltas from
//! `/proc/self/io`; peak RSS is `VmHWM` from `/proc/self/status`.
//!
//! The run is synthetic: a single file in a listable store on the host page
//! cache inside the Docker court. It is not a device-level claim, an fsync
//! claim, or an end-to-end filesystem claim.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::{json, Value};

use weftfs::store::{OpenMode, Options, Store};

const FILE_SIZE: usize = 64 * 1024 * 1024;
const CHUNK_SIZE: u64 = 64 * 1024;
const ITERATIONS: usize = 20;
const WRITE_LEN: usize = 4096;
const ALIGNMENT: u64 = 4096;

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK_SIZE,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("all");
    match mode {
        "setup" => setup(Path::new(&args[2])),
        "new" | "old" => {
            let stats = run(Path::new(&args[2]), mode);
            println!("{}", stats);
        }
        _ => orchestrate(),
    }
}

/// Build the benchmark object once, in its own process, so neither measured
/// path pays for (or inherits) a 64 MiB construction peak.
fn setup(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).expect("clear benchmark store");
    }
    let mut store = Store::init(path, options()).expect("init benchmark store");
    let root = store.resolve_path("/").expect("resolve root");
    let mut content = Vec::with_capacity(FILE_SIZE);
    while content.len() < FILE_SIZE {
        let take = (FILE_SIZE - content.len()).min(1024 * 1024);
        content.extend((0..take).map(|index| (index % 251) as u8));
    }
    store
        .create_file(root, b"f", &content, 0o644, 0, 0, Default::default())
        .expect("create benchmark object");
    let meta = store
        .object_meta(store.resolve_path("/f").expect("resolve f"))
        .expect("metadata");
    assert_eq!(meta.size, FILE_SIZE as u64);
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn offsets() -> Vec<u64> {
    let mut rng = Rng(0x5eed_2026_1104);
    let span = (FILE_SIZE - WRITE_LEN) as u64;
    (0..ITERATIONS)
        .map(|_| (rng.next() % span) & !(ALIGNMENT - 1))
        .collect()
}

fn io_counters() -> (u64, u64) {
    let text = fs::read_to_string("/proc/self/io").unwrap_or_default();
    let mut rchar = 0;
    let mut wchar = 0;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("rchar: ") {
            rchar = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("wchar: ") {
            wchar = value.trim().parse().unwrap_or(0);
        }
    }
    (rchar, wchar)
}

fn peak_rss_kib() -> u64 {
    let text = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("VmHWM:") {
            return value
                .split_whitespace()
                .next()
                .and_then(|number| number.parse().ok())
                .unwrap_or(0);
        }
    }
    0
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn median_u64(values: &[u64]) -> u64 {
    median(values.iter().map(|value| *value as f64).collect()) as u64
}

fn run(path: &Path, mode: &str) -> Value {
    let mut store = Store::open(path, OpenMode::ReadWrite).expect("open benchmark store");
    let id = store.resolve_path("/f").expect("resolve f");
    let payload = vec![0x5au8; WRITE_LEN];

    let mut times = Vec::with_capacity(ITERATIONS);
    let mut rchar = Vec::with_capacity(ITERATIONS);
    let mut wchar = Vec::with_capacity(ITERATIONS);

    for offset in offsets() {
        let (rchar_before, wchar_before) = io_counters();
        let started = Instant::now();
        match mode {
            "new" => store
                .write_file(id, offset, &payload)
                .expect("COW range write"),
            _ => store
                .write_file_whole_file_for_bench(id, offset, &payload)
                .expect("whole-file write"),
        };
        let elapsed = started.elapsed();
        let (rchar_after, wchar_after) = io_counters();
        times.push(elapsed.as_secs_f64() * 1000.0);
        rchar.push(rchar_after.saturating_sub(rchar_before));
        wchar.push(wchar_after.saturating_sub(wchar_before));
    }

    let meta = store.object_meta(id).expect("metadata after benchmark");
    assert_eq!(meta.size, FILE_SIZE as u64, "benchmark changed the size");

    json!({
        "mode": mode,
        "iterations": ITERATIONS,
        "time_ms": times,
        "rchar_bytes": rchar,
        "wchar_bytes": wchar,
        "median_time_ms": median(times),
        "median_rchar_bytes": median_u64(&rchar),
        "median_wchar_bytes": median_u64(&wchar),
        "peak_rss_kib": peak_rss_kib(),
    })
}

fn child(mode: &str, store: &Path) -> Value {
    let exe = std::env::current_exe().expect("current executable");
    let output = Command::new(exe)
        .arg(mode)
        .arg(store)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .expect("spawn benchmark child");
    assert!(
        output.status.success(),
        "benchmark child {mode} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse benchmark child JSON")
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn verify(path: &Path) -> bool {
    match Store::open(path, OpenMode::ReadOnly) {
        Ok(store) => store.verify(true).map(|report| report.ok).unwrap_or(false),
        Err(_) => false,
    }
}

fn orchestrate() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scratch = root.join(".docker-cache").join("extent-cow-bench");
    let new_store = scratch.join("new");
    let old_store = scratch.join("old");
    fs::create_dir_all(&scratch).expect("create bench scratch");

    let exe = std::env::current_exe().expect("current executable");
    for store in [&new_store, &old_store] {
        let status = Command::new(&exe)
            .arg("setup")
            .arg(store)
            .status()
            .expect("spawn setup child");
        assert!(status.success(), "setup failed for {}", store.display());
    }

    let new_stats = child("new", &new_store);
    let old_stats = child("old", &old_store);

    let new_ok = verify(&new_store);
    let old_ok = verify(&old_store);

    let revision = env_or(
        "WEFT_REPO_WEFTFS",
        &command_stdout("git", &["rev-parse", "HEAD"]),
    );
    let timestamp = command_stdout("date", &["-u", "+%Y%m%dT%H%M%SZ"]);
    let receipt = json!({
        "label": "synthetic single-file extent-COW measurement on the host page cache inside a Docker court; not a device-level, fsync, or end-to-end filesystem claim",
        "revision": revision,
        "court": {
            "image": env_or("WEFT_COURT_IMAGE", "unknown"),
            "image_id": env_or("WEFT_COURT_IMAGE_ID", "unknown"),
            "kernel": env_or("WEFT_COURT_KERNEL", &command_stdout("uname", &["-r"])),
            "memory_limit": env_or("WEFT_COURT_MEMORY", "unknown"),
            "cpus": env_or("WEFT_COURT_CPUS", "unknown"),
            "pids_limit": env_or("WEFT_COURT_PIDS", "unknown"),
        },
        "workload": {
            "file_size_bytes": FILE_SIZE,
            "chunk_size_bytes": CHUNK_SIZE,
            "iterations": ITERATIONS,
            "write_bytes": WRITE_LEN,
            "alignment_bytes": ALIGNMENT,
            "access": "overwrite in place; file size unchanged",
        },
        "cow_range_write": new_stats,
        "whole_file_baseline": old_stats,
        "store_verified_after_run": { "cow": new_ok, "whole_file": old_ok },
        "receipt_timestamp_utc": timestamp,
    });

    let receipts = root.join("bench").join("receipts");
    fs::create_dir_all(&receipts).expect("create receipts dir");
    let path = receipts.join(format!("extent-cow-{timestamp}.json"));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write receipt");
    println!("{}", path.display());
    println!("{}", serde_json::to_string_pretty(&receipt).unwrap());
}
