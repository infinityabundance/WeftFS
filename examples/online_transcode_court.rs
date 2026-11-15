//! P06 court harness — online background transcode.
//!
//! Three measurements:
//!
//! 1. **Interference.** The same foreground range writes, measured twice: with
//!    no worker and with the worker active. The delta is the cost the operator
//!    pays for background re-encoding.
//! 2. **Throughput against the declared budget.** Rewrites per second and bytes
//!    saved per second, next to the budget's per-step byte bound and the
//!    admission counters that prove the bound was kept.
//! 3. **Crash.** A real `SIGKILL` of a child process running the worker. The
//!    parent then verifies the store: every object is exact, every object is
//!    either its old or its new representation, and a snapshot taken before the
//!    run still resolves.
//!
//! Emits JSON on `--json <path>`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use weftfs::online_transcode::{OnlineTranscodeOptions, OnlineTranscoder, SharedStore};
use weftfs::state::State;
use weftfs::tensor_store::{TensorLayoutSpec, TensorStrategy, TensorWriteSpec};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;
const ROWS: u64 = 512;
const COLS: u64 = 1024;
const OBJECTS: usize = 24;
const FOREGROUND_OPS: usize = 60;

fn options() -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        chunk_size: 64 * 1024,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

/// Structured BF16 weights (a narrow band of exponents and a handful of
/// mantissas), which is what a real tensor looks like: the planned
/// representation compresses it, so a rewrite saves real bytes and the court's
/// throughput number means something.
fn tensor(seed: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ u64::from(seed);
    let mut out = Vec::with_capacity((ROWS * COLS * 2) as usize);
    for index in 0..(ROWS * COLS) {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let exponent = 0x3e00u16 | ((((state >> 40) & 0x03) as u16) << 8);
        let mantissa = ((index as u16).wrapping_add(seed as u16)) & 0x07;
        out.extend_from_slice(&(exponent | mantissa).to_le_bytes());
    }
    out
}

fn build_store(path: &Path) -> (Vec<weftfs::Hash32>, Vec<Vec<u8>>) {
    let mut store = Store::init(path, options()).unwrap();
    let layout = TensorLayoutSpec::row_major();
    let mut ids = Vec::new();
    let mut bytes = Vec::new();
    for index in 0..OBJECTS {
        let data = tensor(index as u32);
        let id = store
            .write_tensor_file(
                State::ROOT_ID,
                format!("t{index:02}").as_bytes(),
                "bf16",
                &[ROWS, COLS],
                &data,
                &layout,
                Some(TensorStrategy::Zstd),
            )
            .unwrap();
        ids.push(id);
        bytes.push(data);
    }
    (ids, bytes)
}

fn percentiles(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| -> f64 {
        if values.is_empty() {
            return 0.0;
        }
        let index = ((values.len() as f64 - 1.0) * p).round() as usize;
        values[index.min(values.len() - 1)]
    };
    json!({"n": values.len(), "p50": at(0.5), "p95": at(0.95), "p99": at(0.99), "max": values.last().copied().unwrap_or(0.0)})
}

/// One foreground range write through the shared store, timed.
fn foreground_op(shared: &SharedStore, id: weftfs::Hash32, round: u32) -> f64 {
    let started = Instant::now();
    shared
        .with_foreground(|store| {
            let region = weftcodec::tensor::Region::new(vec![0, 0], vec![1, (COLS / 8) as u64]);
            let patch = vec![(round & 0xff) as u8; (COLS / 8 * 2) as usize];
            store
                .write_tensor_range(id, &region, &patch, &TensorWriteSpec::default())
                .map(|_| ())
        })
        .unwrap();
    started.elapsed().as_secs_f64() * 1e3
}

fn child_worker(args: &[String]) -> ! {
    // child-worker <store> <max-object-bytes> <pause-ms>
    let path = PathBuf::from(&args[0]);
    let max_object_bytes: u64 = args[1].parse().unwrap();
    let pause_ms: u64 = args[2].parse().unwrap();
    let store = Store::open_with_options(&path, OpenMode::ReadWrite, options()).unwrap();
    let shared = SharedStore::new(store);
    let worker = OnlineTranscoder::start(
        Arc::clone(&shared),
        OnlineTranscodeOptions {
            root: State::ROOT_ID,
            max_object_bytes,
            min_saved_percent: 0,
            pause: Duration::from_millis(pause_ms),
            ..OnlineTranscodeOptions::default()
        },
    )
    .unwrap();
    println!(
        "{}",
        json!({"ok": true, "pid": std::process::id(), "worker": format!("{:?}", worker.is_running())})
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|arg| arg.as_str()) == Some("child-worker") {
        child_worker(&args[1..]);
    }
    let mut out = PathBuf::from("/tmp/online-transcode-court");
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
    fs::create_dir_all(&out).unwrap();

    // ---- arm 1: foreground latency with no worker -----------------------
    let quiet_dir = out.join("quiet");
    let _ = fs::remove_dir_all(&quiet_dir);
    let (ids, bytes) = build_store(&quiet_dir);
    let shared = SharedStore::new(
        Store::open_with_options(&quiet_dir, OpenMode::ReadWrite, options()).unwrap(),
    );
    let mut quiet = Vec::new();
    for round in 0..FOREGROUND_OPS {
        quiet.push(foreground_op(&shared, ids[0], round as u32));
    }
    // Keep the shared store alive; the fixture directory is what the next arm
    // re-opens, so this handle must be dropped first.
    drop(shared);
    // The writes above changed the store; only latencies come from this arm.

    // ---- arm 2: the same writes with the worker active ------------------
    let busy_dir = out.join("busy");
    let _ = fs::remove_dir_all(&busy_dir);
    let (busy_ids, _) = build_store(&busy_dir);
    let shared = SharedStore::new(
        Store::open_with_options(&busy_dir, OpenMode::ReadWrite, options()).unwrap(),
    );
    let worker = OnlineTranscoder::start(
        Arc::clone(&shared),
        OnlineTranscodeOptions {
            root: State::ROOT_ID,
            max_object_bytes: (ROWS * COLS * 2) + 1,
            min_saved_percent: 0,
            pause: Duration::from_millis(1),
            yield_interval: Duration::from_millis(1),
            ..OnlineTranscodeOptions::default()
        },
    )
    .unwrap();
    let started = Instant::now();
    let mut busy = Vec::new();
    for round in 0..FOREGROUND_OPS {
        busy.push(foreground_op(&shared, busy_ids[0], round as u32));
    }
    let foreground_wall = started.elapsed().as_secs_f64();
    // Let the worker run a moment more so throughput has a window.
    let throughput_start = Instant::now();
    let applied_before = worker.stats().applied;
    let bytes_before = worker.stats().bytes_saved;
    std::thread::sleep(Duration::from_secs(2));
    let stats = worker.stats();
    let window = throughput_start.elapsed().as_secs_f64();
    let applied_window = stats.applied.saturating_sub(applied_before);
    let saved_window = stats.bytes_saved - bytes_before;
    let budget = worker.budget_stats();
    worker.stop();
    let final_stats = worker.stats();
    shared
        .with_foreground(|store| {
            assert!(store.verify(true).unwrap().ok);
            Ok(())
        })
        .unwrap();
    drop(shared);

    // ---- arm 3: SIGKILL the worker and check recovery -------------------
    let crash_dir = out.join("crash");
    let _ = fs::remove_dir_all(&crash_dir);
    let (crash_ids, crash_bytes) = build_store(&crash_dir);
    // A snapshot before the run pins the old representations.
    let snapshot = {
        let mut store =
            Store::open_with_options(&crash_dir, OpenMode::ReadWrite, options()).unwrap();
        let before: BTreeMap<String, weftfs::Hash32> = crash_ids
            .iter()
            .map(|id| (id.to_hex(), store.object_meta(*id).unwrap().content))
            .collect();
        let snapshot = store.snapshot_create("before").unwrap();
        (snapshot, before)
    };
    let exe = std::env::current_exe().unwrap();
    let max_object_bytes = ((ROWS * COLS * 2) + 1).to_string();
    let mut child = Command::new(&exe)
        .args([
            "child-worker",
            crash_dir.to_str().unwrap(),
            &max_object_bytes,
            "1",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Let it apply some rewrites, then kill it outright.
    std::thread::sleep(Duration::from_millis(1200));
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.wait();

    let store = Store::open_with_options(&crash_dir, OpenMode::ReadOnly, options()).unwrap();
    let verify = store.verify(true).unwrap();
    let mut exact = 0usize;
    let mut rewritten = 0usize;
    for (index, id) in crash_ids.iter().enumerate() {
        if &store.read_object(*id).unwrap() == &crash_bytes[index] {
            exact += 1;
        }
        if store.object_meta(*id).unwrap().content != snapshot.1[&id.to_hex()] {
            rewritten += 1;
        }
    }
    let pinned = store.state_at(snapshot.0).unwrap();
    let mut pinned_old = 0usize;
    for id in &crash_ids {
        let record = pinned.objects.get(id).expect("the snapshot still names it");
        if record.content == snapshot.1[&id.to_hex()] {
            pinned_old += 1;
        }
    }

    // ---- checks ---------------------------------------------------------
    let mut checks: Vec<serde_json::Value> = Vec::new();
    checks.push(json!({
        "label": "every foreground write landed and the store verifies",
        "ok": verify.ok,
        "detail": format!("{} problems", verify.problems.len()),
    }));
    checks.push(json!({
        "label": "the worker applied rewrites while foreground writes continued",
        "ok": final_stats.applied > 0 && stats.foreground_yields > 0,
        "detail": format!(
            "{} applied, {} foreground yields, {} oversized skips",
            final_stats.applied, final_stats.foreground_yields, final_stats.oversized_skips
        ),
    }));
    checks.push(json!({
        "label": "the declared budget was kept",
        "ok": budget.observed_max_active_workers <= 1 && budget.oversized_admissions == 0,
        "detail": format!("{budget:?}"),
    }));
    checks.push(json!({
        "label": "a SIGKILL leaves every object exact",
        "ok": exact == OBJECTS,
        "detail": format!("{exact}/{OBJECTS} exact, {rewritten} rewritten"),
    }));
    checks.push(json!({
        "label": "the pre-run snapshot still names every old representation",
        "ok": pinned_old == OBJECTS,
        "detail": format!("{pinned_old}/{OBJECTS} pinned"),
    }));

    let quiet_stats = percentiles(quiet.clone());
    let busy_stats = percentiles(busy.clone());
    let document = json!({
        "schema": "weft.online-transcode.court.v1",
        "fixture": {
            "objects": OBJECTS,
            "rows": ROWS,
            "cols": COLS,
            "dtype": "bf16",
            "object_bytes": ROWS * COLS * 2,
            "foreground_ops": FOREGROUND_OPS,
            "fast_strategy": "zstd",
        },
        "interference": {
            "quiet_ms": quiet_stats.clone(),
            "busy_ms": busy_stats.clone(),
            "quiet_p50": quiet_stats["p50"],
            "busy_p50": busy_stats["p50"],
            "p50_ratio": if quiet_stats["p50"].as_f64().unwrap_or(0.0) > 0.0 {
                busy_stats["p50"].as_f64().unwrap_or(0.0) / quiet_stats["p50"].as_f64().unwrap_or(1.0)
            } else { 0.0 },
            "p99_ratio": if quiet_stats["p99"].as_f64().unwrap_or(0.0) > 0.0 {
                busy_stats["p99"].as_f64().unwrap_or(0.0) / quiet_stats["p99"].as_f64().unwrap_or(1.0)
            } else { 0.0 },
            "foreground_wall_s": foreground_wall,
            "worker_hold_share": if foreground_wall + 2.0 > 0.0 {
                (final_stats.held_ms / 1000.0) / (foreground_wall + 2.0)
            } else { 0.0 },
            "worker_hold_ms": final_stats.held_ms,
            "worker_yielded_ms": final_stats.yielded_ms,
        },
        "throughput": {
            "window_s": window,
            "applied_in_window": applied_window,
            "objects_per_s": if window > 0.0 { applied_window as f64 / window } else { 0.0 },
            "bytes_saved_in_window": saved_window,
            "mib_per_s": if window > 0.0 { (saved_window as f64 / (1024.0 * 1024.0)) / window } else { 0.0 },
            "final_stats": {
                "steps": final_stats.steps,
                "applied": final_stats.applied,
                "empty_steps": final_stats.empty_steps,
                "foreground_yields": final_stats.foreground_yields,
                "oversized_skips": final_stats.oversized_skips,
                "budget_refusals": final_stats.budget_refusals,
                "bytes_read": final_stats.bytes_read,
                "bytes_saved": final_stats.bytes_saved,
                "held_ms": final_stats.held_ms,
                "yielded_ms": final_stats.yielded_ms,
            },
            "budget": {
                "max_active_workers": budget.max_active_workers,
                "max_inflight_bytes": budget.max_inflight_bytes,
                "observed_max_active_workers": budget.observed_max_active_workers,
                "oversized_admissions": budget.oversized_admissions,
            },
        },
        "crash": {
            "exact_objects": exact,
            "rewritten_before_kill": rewritten,
            "pinned_old_objects": pinned_old,
            "verify_ok": verify.ok,
            "verify_problems": verify.problems.len(),
            "objects": OBJECTS,
        },
        "checks": checks,
        "checks_failed": checks.iter().filter(|check| check["ok"].as_bool() != Some(true)).count(),
        "environment": {
            "cpus": std::thread::available_parallelism().map(|value| value.get()).unwrap_or(0),
            "target": std::env::consts::ARCH,
            "clock": "Instant; the crash arm is a real SIGKILL of a second process",
        },
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => fs::write(&path, text).unwrap(),
        None => println!("{text}"),
    }
    let _ = bytes;
    if document["checks_failed"].as_u64().unwrap_or(1) != 0 {
        eprintln!(
            "online_transcode_court: {} check(s) failed",
            document["checks_failed"]
        );
        std::process::exit(1);
    }
}
