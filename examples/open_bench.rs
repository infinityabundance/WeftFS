//! Open/replay benchmark: generation-addressed bounded open vs. full scan.
//!
//! Builds one deterministic 10,000-generation store (small object per
//! generation, checkpoint interval 256) under the court's `/tmp` tmpfs, then
//! times `Store::open(ReadOnly)` ten times on the fast path and ten times on
//! `Store::open_full_scan_for_bench`, which forces the pre-optimisation full
//! commit-directory replay.
//!
//! The measured store is synthetic and lives on the host page cache inside the
//! Docker court. This is not a device, fleet, power-loss, or production claim.
//!
//! Usage: `open_bench [--generations N] [--samples N] [--work-dir DIR] [--out PATH]`
//! Run through `scripts/docker-court.sh rust "cargo run --release --example open_bench"`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use weftfs::recovery;
use weftfs::store::{OpenMode, Options, Store};

const DEFAULT_GENERATIONS: u64 = 10_000;
const DEFAULT_SAMPLES: usize = 10;
const CHECKPOINT_INTERVAL: u64 = 256;

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 1024,
        checkpoint_interval: CHECKPOINT_INTERVAL,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_767_225_600_000_000_000), // 2026-01-01T00:00:00Z
    }
}

fn payload(generation: u64) -> Vec<u8> {
    format!("weft open bench object {generation:06}\n").into_bytes()
}

fn build_store(work_dir: &Path, generations: u64) {
    if work_dir.exists() {
        fs::remove_dir_all(work_dir).expect("clear benchmark store");
    }
    fs::create_dir_all(work_dir).expect("create benchmark dir");
    let mut store = Store::init(work_dir, options()).expect("init benchmark store");
    let root = store.resolve_path("/").expect("resolve root");
    while store.generation() < generations {
        let generation = store.generation();
        let name = format!("f{generation:06}");
        store
            .create_file(
                root,
                name.as_bytes(),
                &payload(generation),
                0o644,
                0,
                0,
                Default::default(),
            )
            .expect("create benchmark object");
    }
    assert_eq!(store.generation(), generations, "generation count");
    eprintln!(
        "open_bench: built {generations}-generation store at {}",
        work_dir.display()
    );
}

fn sample(path: &Path, full_scan: bool) -> (f64, u64) {
    recovery::reset_commit_file_reads();
    let started = Instant::now();
    let store = if full_scan {
        Store::open_full_scan_for_bench(path, OpenMode::ReadOnly).expect("full-scan open")
    } else {
        Store::open(path, OpenMode::ReadOnly).expect("fast open")
    };
    let millis = started.elapsed().as_secs_f64() * 1_000.0;
    let reads = recovery::commit_file_reads();
    drop(store);
    (millis, reads)
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

/// Nearest-rank 95th percentile.
fn p95(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rank = ((sorted.len() as f64) * 0.95).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn cpu_model() -> Option<String> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| {
            rest.split_once(':')
                .map(|(_, value)| value.trim().to_owned())
        })
}

fn peak_rss_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmHWM:")
            .and_then(|rest| rest.trim().trim_end_matches(" kB").parse().ok())
    })
}

fn utc_stamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        remainder / 3_600,
        (remainder % 3_600) / 60,
        remainder % 60
    )
}

/// Howard Hinnant's civil-from-days algorithm (days since 1970-01-01).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn main() {
    let mut generations = DEFAULT_GENERATIONS;
    let mut samples = DEFAULT_SAMPLES;
    let mut work_dir: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let args: Vec<String> = env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--generations" => {
                generations = args[index + 1].parse().expect("generations");
                index += 2;
            }
            "--samples" => {
                samples = args[index + 1].parse().expect("samples");
                index += 2;
            }
            "--work-dir" => {
                work_dir = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            "--out" => {
                out_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    assert!(samples > 0, "need at least one sample");
    let work_dir = work_dir.unwrap_or_else(|| env::temp_dir().join("weft-open-bench"));
    let out_path = out_path
        .unwrap_or_else(|| PathBuf::from(format!("bench/receipts/fast-open-{}.json", utc_stamp())));

    build_store(&work_dir, generations);

    // Warm both paths once, unmeasured, so the page cache is populated the same
    // way for every sample.
    let _ = sample(&work_dir, false);
    let _ = sample(&work_dir, true);

    let mut fast_wall = Vec::with_capacity(samples);
    let mut fast_reads = Vec::with_capacity(samples);
    for _ in 0..samples {
        let (millis, reads) = sample(&work_dir, false);
        fast_wall.push(millis);
        fast_reads.push(reads);
    }

    let mut full_wall = Vec::with_capacity(samples);
    let mut full_reads = Vec::with_capacity(samples);
    for _ in 0..samples {
        let (millis, reads) = sample(&work_dir, true);
        full_wall.push(millis);
        full_reads.push(reads);
    }

    // Both paths must produce the same recovered state.
    let fast = Store::open(&work_dir, OpenMode::ReadOnly).expect("fast state open");
    let full =
        Store::open_full_scan_for_bench(&work_dir, OpenMode::ReadOnly).expect("full state open");
    assert_eq!(fast.generation(), full.generation(), "generation mismatch");
    assert_eq!(fast.snapshot_root(), full.snapshot_root(), "head mismatch");
    assert_eq!(
        fast.current_state().objects,
        full.current_state().objects,
        "objects mismatch"
    );
    assert_eq!(
        fast.current_state().entries,
        full.current_state().entries,
        "entries mismatch"
    );
    assert_eq!(
        fast.current_state().relations,
        full.current_state().relations,
        "relations mismatch"
    );
    assert_eq!(
        fast.current_state().snapshots,
        full.current_state().snapshots,
        "snapshots mismatch"
    );
    assert_eq!(
        fast.current_state().tips,
        full.current_state().tips,
        "tips mismatch"
    );
    let objects = fast.current_state().objects.len();
    drop(fast);
    drop(full);

    let report = json!({
        "schema": "weft-open-bench-v1",
        "kind": "bounded-open-bench",
        "label": "Local synthetic store on the host page cache inside the Docker court; not a device, fleet, power-loss, or production claim.",
        "environment": {
            "kernel": env::var("WEFT_COURT_KERNEL").ok(),
            "court_image": env::var("WEFT_COURT_IMAGE").ok(),
            "court_image_id": env::var("WEFT_COURT_IMAGE_ID").ok(),
            "court_memory": env::var("WEFT_COURT_MEMORY").ok(),
            "court_cpus": env::var("WEFT_COURT_CPUS").ok(),
            "court_pids": env::var("WEFT_COURT_PIDS").ok(),
            "docker_client": env::var("WEFT_DOCKER_CLIENT").ok(),
            "docker_server": env::var("WEFT_DOCKER_SERVER").ok(),
            "cpu_model": cpu_model(),
            "revision_weftfs": env::var("WEFT_REPO_WEFTFS").ok(),
            "revision_weftcodec": env::var("WEFT_REPO_WEFTCODEC").ok(),
        },
        "work_dir": work_dir.display().to_string(),
        "generations": generations,
        "checkpoint_interval": CHECKPOINT_INTERVAL,
        "objects": objects,
        "samples": samples,
        "state_match": true,
        "fast": {
            "path": "Store::open (generation-addressed bounded replay)",
            "commit_file_reads": fast_reads,
            "wall_ms": fast_wall,
            "median_ms": median(&fast_wall),
            "p95_ms": p95(&fast_wall),
        },
        "full": {
            "path": "Store::open_full_scan_for_bench (full commit-directory replay)",
            "commit_file_reads": full_reads,
            "wall_ms": full_wall,
            "median_ms": median(&full_wall),
            "p95_ms": p95(&full_wall),
        },
        "peak_rss_kib": peak_rss_kib(),
        "notes": [
            "The store is built once, sequentially, in this process under the court's /tmp tmpfs (1 GiB) and is read from the host page cache.",
            "The full path is the pre-optimisation behaviour: it scans the commit directory and decodes every commit file. The removed duplicate directory scan in Store::open is not timed separately.",
            "Commit-file reads are counted by the process-wide recovery::commit_file_reads() counter, incremented once per load_commit_file call.",
            "The fast path is ReadOnly and therefore directory-free: it starts from the highest published superblock generation and walks parent links down to the nearest verified checkpoint.",
            "No device-level, fsync, power-loss, fleet, or production claim is made."
        ],
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize receipt");
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).expect("create receipt dir");
    }
    fs::write(&out_path, text).expect("write receipt");
    eprintln!("open_bench: wrote {}", out_path.display());
    println!(
        "fast median {:.1} ms (p95 {:.1} ms, {} reads), full median {:.1} ms (p95 {:.1} ms, {} reads)",
        median(&fast_wall),
        p95(&fast_wall),
        fast_reads.iter().copied().max().unwrap_or(0),
        median(&full_wall),
        p95(&full_wall),
        full_reads.iter().copied().max().unwrap_or(0),
    );
}
