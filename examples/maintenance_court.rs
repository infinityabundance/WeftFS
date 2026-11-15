//! T080 court harness — bounded interference, restart, and two compactors.
//!
//! Three independent measurements:
//!
//! 1. **Interference.** A reader thread performs whole-object reads of live
//!    artifacts while a maintenance pass runs with a declared budget (10% duty
//!    cycle on 4 CPUs). The same reader runs against a control store with no
//!    maintenance. p50/p95/p99 of both are reported; the cost is whatever it is.
//! 2. **Kill and resume.** A child process runs batches until the parent
//!    SIGKILLs it. The store is then reopened: every live artifact must still
//!    read exactly, `verify --full` must be clean, and the persisted cursor
//!    must show that progress was made and is resumable.
//! 3. **Two compactors.** Two child processes run maintenance against one
//!    store. Their reclaimed totals must sum to the plan (no double-free) and
//!    no live artifact may be lost.
//!
//! Child mode is this same binary with `--child <store> <label>`; it prints one
//! JSON line with its totals when the plan completes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use weftfs::maintenance::{maintenance_run, MaintenanceBudget, MaintenanceState};
use weftfs::store::{OpenMode, Options, Store};

const LIVE_FILES: usize = 120;
const LIVE_BYTES: usize = 4096;
const GARBAGE: usize = 600;
const GARBAGE_BYTES: usize = 8192;
const READ_OPS: usize = 12_000;

fn options() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 4096,
        // No automatic checkpoint: a retained checkpoint state is a GC root and
        // would legitimately keep the garbage this court wants to reclaim.
        checkpoint_interval: 100_000,
        gc_grace_secs: 0,
        ..Options::default()
    }
}

fn populate(path: &Path) {
    let mut store = Store::init(path, options()).unwrap();
    let root = store.resolve_path("/").unwrap();
    let live = store.mkdir(root, b"live", 0o755, 0, 0).unwrap();
    for index in 0..LIVE_FILES {
        // Distinct per file, so no live chunk coincides with a garbage chunk.
        let bytes: Vec<u8> = (0..LIVE_BYTES)
            .map(|offset| ((index * 17 + offset) % 251) as u8)
            .collect();
        store
            .create_file(
                live,
                format!("live-{index:04}").as_bytes(),
                &bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
    }
    let garbage = store.mkdir(root, b"garbage", 0o755, 0, 0).unwrap();
    for index in 0..GARBAGE {
        let bytes: Vec<u8> = (0..GARBAGE_BYTES)
            .map(|offset| ((index * 31 + offset * 3) % 251) as u8)
            .collect();
        store
            .create_file(
                garbage,
                format!("gone-{index:04}").as_bytes(),
                &bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
    }
    for index in 0..GARBAGE {
        store
            .unlink(garbage, format!("gone-{index:04}").as_bytes())
            .unwrap();
    }

}

fn budget() -> MaintenanceBudget {
    MaintenanceBudget {
        bytes_per_second: 4 * 1024 * 1024,
        cpu_share_percent: 10,
        max_batch_bytes: 256 * 1024,
        max_pass_bytes: 64 * 1024 * 1024,
        min_batch_bytes: 32 * 1024,
    }
}

fn percentiles(samples: &[u64]) -> (u64, u64, u64, u64) {
    let mut samples = samples.to_vec();
    samples.sort_unstable();
    let at = |q: f64| {
        let index = ((samples.len() as f64 - 1.0) * q).round() as usize;
        samples[index.min(samples.len() - 1)]
    };
    (at(0.5), at(0.95), at(0.99), samples[samples.len() - 1])
}

fn direct_child(store: &str, label: &str, optimistic: bool, sweep_only: bool) {
    // Same options as the parent: `gc_grace_secs` and `chunk_size` are options,
    // not persisted state, and a default grace would retain every young blob.
    let mut options = options();
    if optimistic {
        options.writer_mode = weftfs::mw2::WriterMode::Optimistic;
    }
    let mut handle = Store::open_with_options(store, OpenMode::ReadWrite, options).unwrap();
    let mut reclaimed = 0u64;
    let mut batches = 0u64;
    if sweep_only {
        // Two-compactor arm: both children sweep the same plan. The epoch CAS
        // serialises them and `delete_if_present` makes a lost race a no-op, so
        // the sum of their deletions can never exceed the plan.
        let plan = handle.maintenance_plan().unwrap();
        for _ in 0..64 {
            match handle.maintenance_reclaim(&plan.candidates) {
                Ok(sweep) => {
                    reclaimed += sweep.deleted_bytes;
                    batches += 1;
                    if sweep.deleted == 0 {
                        break;
                    }
                }
                Err(err) if err.class() == weftfs::error::Class::Busy => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("child {label}: {err}"),
            }
        }
        println!("CHILD {label} reclaimed={reclaimed} batches={batches}");
        return;
    }
    loop {
        match maintenance_run(&mut handle, &budget(), 1, true) {
            Ok(run) => {
                reclaimed += run.reclaimed_bytes;
                batches += run.batches;
                if run.status == "complete" {
                    break;
                }
            }
            Err(err) if err.class() == weftfs::error::Class::Busy => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("child {label}: {err}"),
        }
    }
    println!("CHILD {label} reclaimed={reclaimed} batches={batches}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(position) = args.iter().position(|arg| arg == "--child") {
        let rest = &args[position + 1..];
        let optimistic = rest.iter().any(|arg| arg == "--optimistic");
        let sweep_only = rest.iter().any(|arg| arg == "--sweep");
        direct_child(&rest[0], &rest[1], optimistic, sweep_only);
        return;
    }
    let mut out = PathBuf::from(".docker-cache/maintenance-court");
    let mut json_path: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                out = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--json" => {
                json_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();
    let mut checks = Vec::new();
    let mut measured = json!({});

    // ---------------------------------------------------------- interference
    let control = out.join("control");
    let treated = out.join("treated");
    populate(&control);
    populate(&treated);

    let read_loop = |path: &Path| -> Vec<u64> {
        let store = Store::open_with_options(path, OpenMode::ReadOnly, options()).unwrap();
        let live = store.resolve_components(&[b"live"]).unwrap();
        let mut ids = Vec::new();
        for entry in store.list_dir(live).unwrap() {
            ids.push(entry.target);
        }
        let mut samples = Vec::with_capacity(READ_OPS);
        for index in 0..READ_OPS {
            let id = ids[index % ids.len()];
            let started = Instant::now();
            let bytes = store.read_object(id).unwrap();
            std::hint::black_box(&bytes);
            samples.push(started.elapsed().as_nanos() as u64);
        }
        samples
    };

    let control_samples = read_loop(&control);
    let control_samples_len = control_samples.len();
    let control_stats = percentiles(&control_samples);

    // Treated: reader thread + maintenance on the main thread.
    let stop = Arc::new(AtomicBool::new(false));
    let treated_path = treated.clone();
    let reader_stop = stop.clone();
    let reader = std::thread::spawn(move || {
        let store = Store::open_with_options(&treated_path, OpenMode::ReadOnly, options()).unwrap();
        let live = store.resolve_components(&[b"live"]).unwrap();
        let mut ids = Vec::new();
        for entry in store.list_dir(live).unwrap() {
            ids.push(entry.target);
        }
        let mut samples = Vec::new();
        let mut index = 0usize;
        while !reader_stop.load(Ordering::Relaxed) {
            let id = ids[index % ids.len()];
            let started = Instant::now();
            let bytes = store.read_object(id).unwrap();
            std::hint::black_box(&bytes);
            samples.push(started.elapsed().as_nanos() as u64);
            index += 1;
            if index >= READ_OPS {
                break;
            }
        }
        samples
    });
    let mut store = Store::open_with_options(&treated, OpenMode::ReadWrite, options()).unwrap();
    let maintenance_started = Instant::now();
    let run = maintenance_run(&mut store, &budget(), 10_000, true).unwrap();
    let maintenance_wall = maintenance_started.elapsed();
    stop.store(true, Ordering::Relaxed);
    let treated_samples = reader.join().unwrap();
    let treated_samples_len = treated_samples.len();
    let treated_stats = percentiles(&treated_samples);
    let state = store.maintenance_state().unwrap().unwrap();
    let overhead = |a: u64, b: u64| if a == 0 { 0.0 } else { b as f64 / a as f64 };
    checks.push(json!({
        "name": "maintenance completed under the declared budget",
        "pass": run.status == "complete" && run.reclaimed_bytes > 0,
        "detail": format!(
            "status={} reclaimed={}B batches={} wall={}ms duty={}%",
            run.status, run.reclaimed_bytes, run.batches,
            maintenance_wall.as_millis(), state.last_duty_percent
        ),
    }));
    checks.push(json!({
        "name": "no live artifact was lost under maintenance",
        "pass": store.verify(true).unwrap().problems.is_empty(),
        "detail": format!("verify --full on the treated store"),
    }));
    measured = json!({
        "interference": {
            "reads_each": READ_OPS,
            "control": {
                "p50_ns": control_stats.0, "p95_ns": control_stats.1,
                "p99_ns": control_stats.2, "max_ns": control_stats.3,
                "samples": control_samples_len,
            },
            "treated": {
                "p50_ns": treated_stats.0, "p95_ns": treated_stats.1,
                "p99_ns": treated_stats.2, "max_ns": treated_stats.3,
                "samples": treated_samples_len,
            },
            "p50_ratio": overhead(control_stats.0, treated_stats.0),
            "p95_ratio": overhead(control_stats.1, treated_stats.1),
            "p99_ratio": overhead(control_stats.2, treated_stats.2),
            "maintenance_wall_ms": maintenance_wall.as_millis() as u64,
            "declared_duty_percent": 10,
            "measured_duty_percent": state.last_duty_percent,
            "declared_bytes_per_second": budget().bytes_per_second,
            "measured_bytes_per_second": run.measured_bytes_per_second,
        }
    });

    // ------------------------------------------------------ kill and resume
    let killed = out.join("killed");
    populate(&killed);
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(&exe)
        .arg("--child")
        .arg(&killed)
        .arg("victim")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Kill deterministically after the first batch has been persisted, so the
    // fixture tests "killed mid-run", not "killed before doing anything".
    let mut waited = 0u64;
    loop {
        let progressed = Store::open_with_options(&killed, OpenMode::ReadOnly, options())
            .ok()
            .and_then(|probe| probe.maintenance_state().ok().flatten())
            .map(|state| state.passes >= 1 && state.cursor < state.candidates)
            .unwrap_or(false);
        if progressed || waited >= 200 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        waited += 1;
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    let progress_before = Store::open_with_options(&killed, OpenMode::ReadOnly, options())
        .unwrap()
        .maintenance_state()
        .unwrap();
    let mut handle = Store::open_with_options(&killed, OpenMode::ReadWrite, options()).unwrap();
    let live_before = live_snapshot(&handle);
    let resumed = maintenance_run(&mut handle, &budget(), 10_000, true).unwrap();
    let state_after = handle.maintenance_state().unwrap().unwrap();
    let live_after = live_snapshot(&handle);
    checks.push(json!({
        "name": "a SIGKILLed pass loses no reachable artifact",
        "pass": live_before == live_after && handle.verify(true).unwrap().problems.is_empty(),
        "detail": format!(
            "killed_signal={:?} state_before_passes={} cursor={} after_status={} verify=clean",
            status.code(),
            progress_before.as_ref().map(|s| s.passes).unwrap_or(0),
            progress_before.as_ref().map(|s| s.cursor).unwrap_or(0),
            state_after.status
        ),
    }));
    checks.push(json!({
        "name": "the restart resumes the persisted progress",
        "pass": resumed.resumed && state_after.passes > progress_before.as_ref().map(|s| s.passes).unwrap_or(0),
        "detail": format!(
            "resumed={} passes {} -> {} cursor {} -> {} reclaimed_total={}",
            resumed.resumed,
            progress_before.as_ref().map(|s| s.passes).unwrap_or(0),
            state_after.passes,
            progress_before.as_ref().map(|s| s.cursor).unwrap_or(0),
            state_after.cursor,
            state_after.reclaimed_bytes
        ),
    }));
    measured["restart"] = json!({
        "progress_before": progress_before.as_ref().map(state_json),
        "progress_after": state_json(&state_after),
        "resumed": resumed.resumed,
        "reclaimed_after_restart": resumed.reclaimed_bytes,
    });

    // ------------------------------------------------------ two compactors
    let shared = out.join("shared");
    populate(&shared);
    {
        // Two concurrent writers require the optimistic writer mode. Migrate
        // the store explicitly; nothing enables a second writer silently.
        let mut writer = Store::open_with_options(&shared, OpenMode::ReadWrite, options()).unwrap();
        writer
            .migrate_writer_mode(
                weftfs::mw2::WriterMode::Single,
                weftfs::mw2::WriterMode::Optimistic,
                std::time::Duration::ZERO,
            )
            .unwrap();
    }
    let plan = Store::open_with_options(&shared, OpenMode::ReadOnly, options())
        .unwrap()
        .maintenance_plan()
        .unwrap();
    let planned_bytes: u64 = plan
        .candidates
        .iter()
        .map(|candidate| candidate.bytes)
        .sum();
    let mut children = Vec::new();
    for label in ["a", "b"] {
        children.push(
            Command::new(&exe)
                .arg("--child")
                .arg(&shared)
                .arg(label)
                .arg("--optimistic")
                .arg("--sweep")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut totals = Vec::new();
    let mut child_errors = Vec::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let reclaimed = text
            .split("reclaimed=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        totals.push(reclaimed);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let first = stderr
            .lines()
            .find(|line| line.starts_with("thread") || line.contains("class:"))
            .unwrap_or("")
            .trim()
            .to_owned();
        child_errors.push(format!("exit={:?} stderr={first:?}", output.status.code()));
    }
    let store = Store::open_with_options(&shared, OpenMode::ReadOnly, options()).unwrap();
    let sum: u64 = totals.iter().sum();
    checks.push(json!({
        "name": "two compactors reclaim each blob once",
        "pass": sum == planned_bytes,
        "detail": format!("a={} b={} sum={} planned={} stderr={:?}", totals[0], totals[1], sum, planned_bytes, child_errors),
    }));
    checks.push(json!({
        "name": "two compactors lose no reachable artifact",
        "pass": store.verify(true).unwrap().problems.is_empty() && live_snapshot(&store).len() == LIVE_FILES,
        "detail": format!("{} live artifacts, verify --full clean", live_snapshot(&store).len()),
    }));
    measured["compactors"] = json!({
        "reclaimed_a": totals[0],
        "reclaimed_b": totals[1],
        "sum": sum,
        "planned_bytes": planned_bytes,
        "double_free": sum > planned_bytes,
    });

    let passed = checks
        .iter()
        .filter(|check| check["pass"] == json!(true))
        .count();
    let failed = checks.len() - passed;
    let document = json!({
        "schema": "weft.maintenance-court/v1",
        "checks": checks,
        "passed": passed,
        "failed": failed,
        "metrics": measured,
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => fs::write(path, text).unwrap(),
        None => println!("{text}"),
    }
}

fn live_snapshot(store: &Store) -> Vec<(String, u64, Vec<u8>)> {
    let live = store.resolve_components(&[b"live"]).unwrap();
    let mut out = Vec::new();
    let mut entries = store.list_dir(live).unwrap();
    entries.sort_by_key(|entry| entry.name.clone());
    for entry in entries {
        let bytes = store.read_object(entry.target).unwrap();
        out.push((
            String::from_utf8_lossy(&entry.name).to_string(),
            bytes.len() as u64,
            bytes,
        ));
    }
    out
}

fn state_json(state: &MaintenanceState) -> serde_json::Value {
    json!({
        "plan_root": state.plan_root.to_hex(),
        "candidates": state.candidates,
        "cursor": state.cursor,
        "passes": state.passes,
        "status": state.status,
        "reclaimed_blobs": state.reclaimed_blobs,
        "reclaimed_bytes": state.reclaimed_bytes,
        "deferred": state.deferred,
        "last_rate_bytes_per_second": state.last_rate_bytes_per_second,
        "last_duty_percent": state.last_duty_percent,
    })
}
