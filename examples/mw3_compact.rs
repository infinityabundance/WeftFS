//! T027 compact MW3 client: `--threads` writers in **one process**.
//!
//! Each worker thread opens its own store handle on its own conflict domain and
//! commits `--count` disjoint transactions. The process reports aggregate
//! throughput, per-writer retry/starvation statistics, the observed peak RSS,
//! the thread count, and the head-state row counts that make up the per-writer
//! memory. It is a court driver, not a product CLI.
//!
//! Court:
//!   bash scripts/docker-court.sh bench "cd /work && bash scripts/mw3_court.sh"
//!
//! The process-per-writer path is exercised by `examples/mw2_writer.rs`; this
//! binary exists so the 128/256/1024-writer cells fit the court's 1024-pid
//! bound without pretending a thread is a process.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use weftfs::error::Class;
use weftfs::mw2::{TransactionOptions, WriterMode};
use weftfs::state::{
    derive_entry_id, derive_version_id, EntryRecord, ObjectKind, ObjectRecord, State,
};
use weftfs::{BackendKind, OpenMode, Options, Store};

fn arg(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|value| value == name)?;
    args.get(index + 1).cloned()
}

fn options(now: i64) -> Options {
    Options {
        fixed_timestamp_ns: Some(now),
        writer_mode: WriterMode::Sharded,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn hex(hash: weftfs::Hash32) -> String {
    hash.to_hex()
}

fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

fn current_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

fn threads_now() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("Threads:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

fn percentile(values: &[u64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = (((values.len() - 1) as f64) * q).round() as usize;
    values[index.min(values.len() - 1)] as f64 / 1e6
}

struct WorkerReport {
    writer: usize,
    domain: weftfs::Hash32,
    committed: u64,
    conflicts: u64,
    aborted: u64,
    attempts: u64,
    max_consecutive: u64,
    gap_reads: u64,
    gap_cached: u64,
    touched_intersections: u64,
    state_objects: u64,
    state_entries: u64,
    state_relations: u64,
    latencies: Vec<u64>,
}

fn run_worker(
    path: std::path::PathBuf,
    domain: weftfs::Hash32,
    writer: usize,
    count: u64,
    now: i64,
    gate: Option<Arc<(Mutex<bool>, Condvar)>>,
) -> WorkerReport {
    // `--hold` makes every worker wait until the process has finished spawning,
    // so the observed thread high-water mark is the true peak. Without it a
    // short cell can recycle thread slots and never touch the court's pid
    // bound; with it, the bound is measured rather than assumed.
    if let Some(gate) = gate {
        let (lock, ready) = &*gate;
        let mut go = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*go {
            go = ready
                .wait(go)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
    let mut store = Store::open_domain_with_options_and_backend(
        &path,
        OpenMode::ReadWrite,
        options(now),
        BackendKind::Local,
        domain,
    )
    .expect("open domain");
    let mut report = WorkerReport {
        writer,
        domain,
        committed: 0,
        conflicts: 0,
        aborted: 0,
        attempts: 0,
        max_consecutive: 0,
        gap_reads: 0,
        gap_cached: 0,
        touched_intersections: 0,
        state_objects: 0,
        state_entries: 0,
        state_relations: 0,
        latencies: Vec::with_capacity(count as usize),
    };
    for index in 0..count {
        let name = format!("w{writer}-{index:06}");
        let content = format!("{writer}:{index}");
        let seed = format!("{writer}:{index}:seed");
        let (manifest, _content_hash, size) =
            store.prepare_content(content.as_bytes()).expect("content");
        let object_id = weftfs::identity::object_id_from_seed(seed.as_bytes());
        let version = derive_version_id(object_id, manifest, size, now, None).expect("version");
        let record = ObjectRecord {
            id: object_id,
            kind: ObjectKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime_ns: now,
            version,
            content: manifest,
            size,
            attrs: BTreeMap::new(),
        };
        let entry = EntryRecord {
            id: derive_entry_id(State::ROOT_ID, name.as_bytes()).expect("entry"),
            parent: State::ROOT_ID,
            name: name.as_bytes().to_vec(),
            target: object_id,
        };
        let tx_id = weftfs::identity::h("weft/tx/id/v1", format!("{writer}:{index}").as_bytes())
            .to_hex()[..32]
            .to_owned();
        let mut tx = store.begin_transaction(tx_id).expect("begin");
        {
            let state = store.current_state();
            tx.put_object(state, record).expect("object");
            tx.put_entry(state, entry).expect("entry");
            tx.set_tip(state, object_id, version).expect("tip");
        }
        let started = Instant::now();
        let outcome = store.execute_transaction(&tx, &TransactionOptions::default());
        report.latencies.push(started.elapsed().as_nanos() as u64);
        match outcome {
            Ok(outcome) => {
                report.committed += 1;
                report.attempts += u64::from(outcome.attempts);
                report.max_consecutive = report
                    .max_consecutive
                    .max(u64::from(outcome.attempts.saturating_sub(1)));
            }
            Err(err) if err.class() == Class::Conflict => report.conflicts += 1,
            Err(err) if err.class() == Class::Busy => report.aborted += 1,
            Err(_) => report.aborted += 1,
        }
    }
    let stats = store.rebase_stats();
    report.gap_reads = stats.gap_commits_read;
    report.gap_cached = stats.gap_commits_cached;
    report.touched_intersections = stats.touched_intersections;
    report.max_consecutive = report.max_consecutive.max(stats.max_consecutive_retries);
    let state = store.current_state();
    report.state_objects = state.objects.len() as u64;
    report.state_entries = state.entries.len() as u64;
    report.state_relations = state.relations.len() as u64;
    report
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let store_path = arg(&args, "--store").unwrap_or_else(|| ".".to_owned());
    let domain_list = arg(&args, "--domain-list").expect("--domain-list");
    let threads: usize = arg(&args, "--threads")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let count: u64 = arg(&args, "--count")
        .and_then(|value| value.parse().ok())
        .unwrap_or(10);
    let now: i64 = arg(&args, "--fixed-timestamp-ns")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_700_000_000_000_000_000);
    let latencies_dir = arg(&args, "--latencies-dir");

    let text = std::fs::read_to_string(&domain_list).expect("domain list");
    let domains: Vec<weftfs::Hash32> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| weftfs::Hash32::from_lower_hex(line).expect("domain id"))
        .collect();
    assert!(
        domains.len() >= threads,
        "domain list has {} ids for {threads} threads",
        domains.len()
    );

    let hold = args.iter().any(|value| value == "--hold");
    let gate = hold.then(|| Arc::new((Mutex::new(false), Condvar::new())));
    let threads_before = threads_now();
    let started = Instant::now();
    let mut handles = Vec::with_capacity(threads);
    let mut spawned = 0usize;
    let mut spawn_failures = 0usize;
    for index in 0..threads {
        let path = std::path::PathBuf::from(&store_path);
        let domain = domains[index];
        let gate = gate.clone();
        match std::thread::Builder::new()
            .name(format!("mw3-w{index}"))
            .stack_size(512 * 1024)
            .spawn(move || run_worker(path, domain, index, count, now, gate))
        {
            Ok(handle) => {
                handles.push(handle);
                spawned += 1;
            }
            Err(_) => spawn_failures += 1,
        }
    }
    let _ = threads_now();
    if let Some(gate) = &gate {
        let (lock, ready) = &**gate;
        *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        ready.notify_all();
    }
    let reports: Vec<WorkerReport> = handles
        .into_iter()
        .filter_map(|handle| handle.join().ok())
        .collect();
    let wall_us = started.elapsed().as_micros() as u64;
    let committed: u64 = reports.iter().map(|r| r.committed).sum();
    let conflicts: u64 = reports.iter().map(|r| r.conflicts).sum();
    let aborted: u64 = reports.iter().map(|r| r.aborted).sum();
    let attempts: u64 = reports.iter().map(|r| r.attempts).sum();
    let mut latencies: Vec<u64> = reports.iter().flat_map(|r| r.latencies.clone()).collect();
    latencies.sort_unstable();

    if let Some(dir) = latencies_dir {
        let _ = std::fs::create_dir_all(&dir);
        for report in &reports {
            if let Ok(mut file) = std::fs::File::create(format!("{dir}/w{}.lat", report.writer)) {
                for value in &report.latencies {
                    let _ = writeln!(file, "{value}");
                }
            }
        }
    }

    let max_state_rows = reports
        .iter()
        .map(|r| r.state_objects + r.state_entries + r.state_relations)
        .max()
        .unwrap_or(0);
    let doc = serde_json::json!({
        "ok": aborted == 0 && spawn_failures == 0,
        "threads": threads,
        "hold": hold,
        "threads_spawned": spawned,
        "spawn_failures": spawn_failures,
        "count_per_writer": count,
        "requested": threads as u64 * count,
        "committed": committed,
        "conflicts": conflicts,
        "aborted": aborted,
        "attempts": attempts,
        "retry_amplification": if committed == 0 { 0.0 } else { attempts as f64 / committed as f64 },
        "wall_ms": wall_us as f64 / 1000.0,
        "throughput_tx_s": if wall_us == 0 { 0.0 } else { committed as f64 / (wall_us as f64 / 1e6) },
        "p50_ms": percentile(&latencies, 0.50),
        "p95_ms": percentile(&latencies, 0.95),
        "p99_ms": percentile(&latencies, 0.99),
        "max_ms": if latencies.is_empty() { 0.0 } else { *latencies.last().unwrap() as f64 / 1e6 },
        "max_consecutive_retries": reports.iter().map(|r| r.max_consecutive).max().unwrap_or(0),
        "per_writer_max_consecutive_retries": reports.iter().map(|r| r.max_consecutive).collect::<Vec<_>>(),
        "stalled_writers": reports.iter().filter(|r| r.max_consecutive > 0).count(),
        "gap_commits_read": reports.iter().map(|r| r.gap_reads).sum::<u64>(),
        "gap_commits_cached": reports.iter().map(|r| r.gap_cached).sum::<u64>(),
        "touched_intersections": reports.iter().map(|r| r.touched_intersections).sum::<u64>(),
        "max_state_rows_per_writer": max_state_rows,
        "sum_state_rows": reports.iter().map(|r| r.state_objects + r.state_entries + r.state_relations).sum::<u64>(),
        "peak_rss_kib": peak_rss_kib(),
        "rss_kib": current_rss_kib(),
        "threads_in_process_before": threads_before,
        "threads_in_process_after": threads_now(),
        "threads_in_process_peak_declared": threads + 1,
        "domains": reports.iter().map(|r| hex(r.domain)).collect::<Vec<_>>(),
        "commit_delta_cache_bound": {"entries": 4096, "bytes": 32 * 1024 * 1024},
    });
    println!("{doc}");
}
