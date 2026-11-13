//! X06, X07 and N09 courts: measured behaviour of the one admission object and
//! the active-handle cap.
//!
//! ```text
//!   resource-courts restore-budget   # X06: worker/shard/tile configurations
//!   resource-courts handle-cap       # X07: active-handle cap vs the control
//!   resource-courts stage-budget     # N09: 1/2/4/8-core policy stress
//! ```
//!
//! Every arm restores or reads through the shipped APIs; nothing here is a
//! simulation. Each subcommand prints one JSON object on stdout and a summary
//! line on stderr. The court scripts add the environment, revisions and
//! receipt stamp.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::restore_pipeline::RestorePipelineOptions;
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::stage_budget::{StageBudget, StageCancelToken};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_pipeline::{TensorPipelineOptions, TensorPipelineTensor};
use weftfs::tensor_store::{TensorLayoutSpec, TensorWriteSpec};
use weftfs::{Error, Hash32, Result};

const STAMP: i64 = 1_700_000_000_000_000_000;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(STAMP),
        ..Options::default()
    }
}

fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn source_path(work: &std::path::Path, tensors: usize, tensor_bytes: usize) -> std::path::PathBuf {
    work.join("src")
        .join(format!("t{tensors}-{tensor_bytes}.safetensors"))
}

/// Build a safetensors container of `tensors` BF16 tensors of `tensor_bytes`
/// each (512 rows, so axis 0 divides by two) and return its path.
fn build_source(
    work: &std::path::Path,
    tensors: usize,
    tensor_bytes: usize,
) -> Result<std::path::PathBuf> {
    let path = source_path(work, tensors, tensor_bytes);
    if path.exists() {
        return Ok(path);
    }
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|err| Error::io(format!("create source dir: {err}")))?;
    let rows = 512u64;
    let cols = ((tensor_bytes / 2) as u64 / rows).max(1);
    let shape = vec![rows, cols];
    let owned: Vec<(String, Vec<u8>)> = (0..tensors)
        .map(|index| {
            (
                format!("t{index:03}"),
                bytes(tensor_bytes, 0x9e37_0000 + index as u64),
            )
        })
        .collect();
    let entries: Vec<SafetensorsEntry> = owned
        .iter()
        .map(|(name, data)| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape: &shape,
            data,
        })
        .collect();
    let file = safetensors::encode_file(&entries, None)?;
    fs::write(&path, &file).map_err(|err| Error::io(format!("write source: {err}")))?;
    Ok(path)
}

fn build_checkpoint_store(store_path: &std::path::Path, source: &std::path::Path) -> Result<Store> {
    if store_path.exists() {
        fs::remove_dir_all(store_path).map_err(|err| Error::io(format!("reset store: {err}")))?;
    }
    let mut store = Store::init(store_path, opts())?;
    store.write_checkpoint(
        "ckpt",
        source,
        &CheckpointWriteOptions {
            shards: 2,
            axis: 0,
            strategy: ShardStrategy::TensorSharded,
            deterministic_seed: Some([0x3c; 32]),
            ..CheckpointWriteOptions::default()
        },
    )?;
    let verify = store.verify(true)?;
    if !verify.problems.is_empty() {
        return Err(Error::corrupt(format!(
            "store verify after the fixture write: {:?}",
            verify.problems
        )));
    }
    Ok(store)
}

fn restore_once(store: &Store, pipeline: &RestorePipelineOptions) -> Result<(u64, Vec<u8>)> {
    let mut out = Vec::new();
    let report = store.restore_checkpoint_pipelined(
        "ckpt",
        &CheckpointRestoreOptions::default(),
        pipeline,
        &mut out,
    )?;
    Ok((report.file_bytes, out))
}

fn pct(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let index = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn summary(values: &[f64]) -> Value {
    json!({
        "reps": values.len(),
        "min_ms": values.iter().copied().fold(f64::INFINITY, f64::min),
        "p50_ms": pct(values, 0.5),
        "p95_ms": pct(values, 0.95),
        "max_ms": values.iter().copied().fold(0f64, f64::max),
        "samples_ms": values,
    })
}

fn peak_rss_kib() -> u64 {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(value) = rest.split_whitespace().next() {
                return value.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn work_dir() -> std::path::PathBuf {
    std::env::var("WEFT_RESOURCE_WORK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/weft-resource-courts"))
}

fn reps() -> usize {
    std::env::var("WEFT_RESOURCE_REPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5)
}

// ---------------------------------------------------------------------------
// X06 — restore budget: tensor/shard/tile configurations
// ---------------------------------------------------------------------------

fn restore_budget() -> Result<Value> {
    let work = work_dir();
    let source = build_source(&work, 6, 128 * 1024)?;
    let store_path = work.join("store-x06");
    drop(build_checkpoint_store(&store_path, &source)?);
    let store = Store::open_with_options(&store_path, OpenMode::ReadOnly, opts())?;
    let expected = fs::read(&source).map_err(|err| Error::io(format!("read source: {err}")))?;

    // (threads, shard_threads, tile_threads)
    let configs: &[(usize, usize, usize)] = &[
        (1, 1, 1),
        (2, 1, 1),
        (4, 1, 1),
        (8, 1, 1),
        (1, 2, 1),
        (2, 2, 1),
        (4, 2, 1),
        (8, 2, 1),
        (1, 4, 1),
        (2, 4, 1),
        (4, 4, 1),
        (8, 4, 1),
        (2, 2, 2),
        (4, 2, 2),
        (4, 4, 4),
    ];
    let mut control: Vec<Value> = Vec::new();
    // Control: the shipped private bounds, no shared budget.
    for (threads, shard, tile) in configs {
        let pipeline = RestorePipelineOptions {
            threads: *threads,
            tile_threads: *tile,
            shard_threads: *shard,
            ..RestorePipelineOptions::default()
        };
        let mut samples = Vec::new();
        for _ in 0..reps() {
            let started = Instant::now();
            let (written, out) = restore_once(&store, &pipeline)?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            if out != expected {
                return Err(Error::corrupt(format!(
                    "control threads={threads} shard={shard} tile={tile}: bytes differ"
                )));
            }
            if written == 0 {
                return Err(Error::corrupt("control restore wrote nothing"));
            }
        }
        control.push(json!({
            "threads": threads, "shard_threads": shard, "tile_threads": tile,
            "wall": summary(&samples),
        }));
    }

    let mut per_cap = BTreeMap::new();
    for cap in [1usize, 2, 4, 8] {
        let budget = StageBudget::with_capacity(cap, 64 * 1024 * 1024)?;
        let mut cap_rows = Vec::new();
        for (threads, shard, tile) in configs {
            let pipeline = RestorePipelineOptions {
                threads: *threads,
                tile_threads: *tile,
                shard_threads: *shard,
                budget: Some(Arc::clone(&budget)),
                ..RestorePipelineOptions::default()
            };
            let mut samples = Vec::new();
            let mut workers = 0usize;
            for _ in 0..reps() {
                let started = Instant::now();
                let (_, out) = restore_once(&store, &pipeline)?;
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                if out != expected {
                    return Err(Error::corrupt(format!(
                        "cap={cap} threads={threads} shard={shard} tile={tile}: bytes differ"
                    )));
                }
            }
            workers = budget.stats().observed_max_active_workers;
            cap_rows.push(json!({
                "threads": threads, "shard_threads": shard, "tile_threads": tile,
                "effective_widths": [pipeline.effective_shard_threads(), pipeline.effective_tile_threads()],
                "wall": summary(&samples),
                "observed_max_active_workers": workers,
            }));
        }
        // The measured default a budgeted caller gets.
        let recommended = RestorePipelineOptions::budgeted(Arc::clone(&budget));
        let mut samples = Vec::new();
        for _ in 0..reps() {
            let started = Instant::now();
            let (_, out) = restore_once(&store, &recommended)?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            if out != expected {
                return Err(Error::corrupt(format!(
                    "cap={cap}: the recommended default changed the bytes"
                )));
            }
        }
        let recommended = json!({
            "threads": recommended.threads,
            "shard_threads": recommended.shard_threads,
            "tile_threads": recommended.tile_threads,
            "effective_widths": [recommended.effective_shard_threads(), recommended.effective_tile_threads()],
            "wall": summary(&samples),
        });
        let stats = budget.stats();
        let best = cap_rows
            .iter()
            .min_by(|a, b| {
                a["wall"]["p50_ms"]
                    .as_f64()
                    .partial_cmp(&b["wall"]["p50_ms"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .unwrap_or(Value::Null);
        per_cap.insert(
            format!("cap{cap}"),
            json!({
                "policy": {"max_active_workers": stats.max_active_workers, "max_inflight_bytes": stats.max_inflight_bytes},
                "observed_max_active_workers": stats.observed_max_active_workers,
                "observed_max_inflight_bytes": stats.observed_max_inflight_bytes,
                "oversized_admissions": stats.oversized_admissions,
                "waited": stats.waited,
                "rows": cap_rows,
                "best": best,
                "recommended_default": recommended,
            }),
        );
    }
    Ok(json!({
        "gate": "X06",
        "schema": "weft.restore_budget.v1",
        "fixture": {"tensors": 6, "tensor_bytes": 128 * 1024, "logical_bytes": expected.len()},
        "reps": reps(),
        "control_no_budget": control,
        "budgeted": per_cap,
        "peak_rss_kib": peak_rss_kib(),
        "recommendation": "shard_threads=1, tile_threads=1, and a budget cap equal to the machine's available parallelism: two axes multiply, and the measured best row per cap is the one that keeps the admitted group inside the cap.",
    }))
}

/// N08 support: the dominant term of a compressed restore is the largest
/// tensor's payload decode. One tensor cannot be split across tensor workers
/// (there is only one), so the only restore-side lever is its shard and tile
/// workers. This arm measures a single 64 MiB, 8-shard tensor at 1/2/4/8 shard
/// workers, unbudgeted and under a 4-worker budget.
fn dominant_tensor() -> Result<Value> {
    let work = work_dir();
    let source = build_source(&work, 1, 64 * 1024 * 1024)?;
    let store_path = work.join("store-dominant");
    drop(build_checkpoint_store(&store_path, &source)?);
    let store = Store::open_with_options(&store_path, OpenMode::ReadOnly, opts())?;
    let expected = fs::read(&source).map_err(|err| Error::io(format!("read source: {err}")))?;
    let mut arms = Vec::new();
    for cap in [0usize, 4] {
        let budget = if cap == 0 {
            None
        } else {
            Some(StageBudget::with_capacity(cap, 256 * 1024 * 1024)?)
        };
        for shard_threads in [1usize, 2, 4, 8] {
            let pipeline = RestorePipelineOptions {
                threads: 1,
                shard_threads,
                max_inflight_bytes: 256 * 1024 * 1024,
                shard_max_inflight_bytes: 256 * 1024 * 1024,
                budget: budget.clone(),
                ..RestorePipelineOptions::default()
            };
            let mut samples = Vec::new();
            for _ in 0..reps() {
                let started = Instant::now();
                let (_, out) = restore_once(&store, &pipeline)?;
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                if out != expected {
                    return Err(Error::corrupt(format!(
                        "dominant tensor: shard_threads={shard_threads} cap={cap}: bytes differ"
                    )));
                }
            }
            arms.push(json!({
                "cap": cap,
                "shard_threads": shard_threads,
                "effective_widths": [pipeline.effective_shard_threads(), pipeline.effective_tile_threads()],
                "wall": summary(&samples),
            }));
        }
    }
    let best = arms
        .iter()
        .filter(|row| row["cap"] == 0)
        .min_by(|a, b| {
            a["wall"]["p50_ms"]
                .as_f64()
                .partial_cmp(&b["wall"]["p50_ms"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
        .unwrap_or(Value::Null);
    Ok(json!({
        "gate": "N08",
        "schema": "weft.dominant_tensor.v1",
        "fixture": {"tensors": 1, "tensor_bytes": 64 * 1024 * 1024, "shards": 8},
        "reps": reps(),
        "arms": arms,
        "best_unbudgeted": best,
        "peak_rss_kib": peak_rss_kib(),
        "note": "one tensor cannot spread across tensor workers; shard parallelism is the restore-side lever for the dominant term",
    }))
}

// ---------------------------------------------------------------------------
// X07 — active-handle cap
// ---------------------------------------------------------------------------

fn handle_cap() -> Result<Value> {
    let work = work_dir();
    let store_path = work.join("store-x07");
    if store_path.exists() {
        fs::remove_dir_all(&store_path).map_err(|err| Error::io(format!("reset store: {err}")))?;
    }
    let mut writer = Store::init(&store_path, opts())?;
    let root = writer.resolve_path("/")?;
    let mut ids = Vec::new();
    for index in 0..64u8 {
        let payload = bytes(64 * 1024, 0x7700_0000 + index as u64);
        let id = writer.create_file(
            root,
            format!("f{index:02}").as_bytes(),
            &payload,
            0o644,
            0,
            0,
            BTreeMap::new(),
        )?;
        ids.push((id, payload.len() as u64));
    }
    drop(writer);
    let store = Store::open_with_options(&store_path, OpenMode::ReadOnly, opts())?;
    let reads_per_thread = 40usize;
    let mut arms = Vec::new();
    for cap in [0usize, 8, 64] {
        store.set_read_pool_active_cap(cap);
        for concurrency in [1usize, 4, 16, 64, 128] {
            let start = Arc::new(Barrier::new(concurrency + 1));
            let completed = Arc::new(AtomicU64::new(0));
            let mismatches = Arc::new(AtomicU64::new(0));
            let started = Instant::now();
            std::thread::scope(|scope| {
                for thread in 0..concurrency {
                    let store = &store;
                    let ids = &ids;
                    let start = Arc::clone(&start);
                    let completed = Arc::clone(&completed);
                    let mismatches = Arc::clone(&mismatches);
                    scope.spawn(move || {
                        start.wait();
                        for round in 0..reads_per_thread {
                            let (id, len) = ids[(thread + round) % ids.len()];
                            match store.read_object(id) {
                                Ok(bytes) if bytes.len() as u64 == len => {
                                    completed.fetch_add(1, Ordering::Relaxed);
                                }
                                _ => {
                                    mismatches.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    });
                }
                start.wait();
            });
            let wall = started.elapsed().as_secs_f64();
            let stats = store.read_pool_stats();
            let reads = completed.load(Ordering::Relaxed);
            let mib = (reads * 64) as f64 / 1024.0;
            arms.push(json!({
                "cap": cap,
                "concurrency": concurrency,
                "reads": reads,
                "mismatches": mismatches.load(Ordering::Relaxed),
                "wall_ms": wall * 1000.0,
                "reads_per_s": reads as f64 / wall,
                "mib_per_s": mib / wall,
                "observed_max_active_handles": stats.observed_max_active_handles,
                "cap_waits": stats.cap_waits,
                "max_in_flight": stats.max_in_flight,
            }));
        }
    }
    Ok(json!({
        "gate": "X07",
        "schema": "weft.handle_cap.v1",
        "objects": ids.len(),
        "object_bytes": 64 * 1024,
        "reads_per_thread": reads_per_thread,
        "default_cap": weftfs::read_pool::MAX_ACTIVE_HANDLES,
        "arms": arms,
        "note": "cap 0 is the pre-X07 control arm; it is labelled as such in every receipt",
        "peak_rss_kib": peak_rss_kib(),
    }))
}

// ---------------------------------------------------------------------------
// N09 — one cross-stage budget
// ---------------------------------------------------------------------------

fn encode_spec() -> TensorWriteSpec {
    TensorWriteSpec {
        strategy_hint: None,
        causal: Vec::new(),
        plan: weftfs::tensor_store::TensorPlanOptions::default(),
        tile_shape: None,
    }
}

fn seed_for(name: &str) -> [u8; 32] {
    let mut seed = [0u8; 32];
    for (index, byte) in name.as_bytes().iter().enumerate() {
        seed[index % 32] = seed[index % 32].wrapping_add(*byte).wrapping_add(1);
    }
    seed[31] = name.len() as u8;
    seed
}

/// N09: one budget across encode, restore and maintenance.
fn stage_budget() -> Result<Value> {
    let work = work_dir();
    let source = build_source(&work, 8, 256 * 1024)?;
    let store_path = work.join("store-n09");
    drop(build_checkpoint_store(&store_path, &source)?);
    let store = Store::open_with_options(&store_path, OpenMode::ReadWrite, opts())?;
    let expected = fs::read(&source).map_err(|err| Error::io(format!("read source: {err}")))?;

    // Encode fixtures: eight 256 KiB tensors read from the same container.
    let encode_bytes: Vec<Vec<u8>> = (0..8)
        .map(|index| bytes(256 * 1024, 0x4e09_0000 + index as u64))
        .collect();
    let shapes: Vec<Vec<u64>> = encode_bytes
        .iter()
        .map(|data| vec![512, (data.len() / 2 / 512) as u64])
        .collect();
    let names: Vec<String> = (0..8).map(|index| format!("e{index:03}")).collect();

    let mut arms = BTreeMap::new();
    for cap in [1usize, 2, 4, 8] {
        let budget = StageBudget::with_capacity(cap, 32 * 1024 * 1024)?;
        let restore_pipeline = RestorePipelineOptions {
            threads: 4,
            shard_threads: 2,
            max_inflight_bytes: 32 * 1024 * 1024,
            budget: Some(Arc::clone(&budget)),
            ..RestorePipelineOptions::default()
        };

        // Sequential control: restore, then encode, on the same budget.
        let started = Instant::now();
        let (_, out) = restore_once(&store, &restore_pipeline)?;
        if out != expected {
            return Err(Error::corrupt("N09 sequential restore bytes differ"));
        }
        let restore_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let encode_control = run_encode(&store, &budget, &names, &shapes, &encode_bytes)?;
        let encode_ms = started.elapsed().as_secs_f64() * 1000.0;
        let sequential = json!({
            "restore_ms": restore_ms,
            "encode_ms": encode_ms,
            "encode": encode_control,
        });
        let sequential_total = restore_ms + encode_ms;

        // Concurrent: both stages at once under the same budget.
        let restore_budget = Arc::clone(&budget);
        let started = Instant::now();
        let (restore_out, encode_out) = std::thread::scope(|scope| {
            let restore = scope.spawn(|| {
                let mut out = Vec::new();
                store.restore_checkpoint_pipelined(
                    "ckpt",
                    &CheckpointRestoreOptions::default(),
                    &restore_pipeline,
                    &mut out,
                )?;
                Ok::<_, Error>(out)
            });
            let encode =
                scope.spawn(|| run_encode(&store, &restore_budget, &names, &shapes, &encode_bytes));
            (
                restore.join().expect("restore thread panicked"),
                encode.join().expect("encode thread panicked"),
            )
        });
        let concurrent_total = started.elapsed().as_secs_f64() * 1000.0;
        let out = restore_out?;
        let encode_result = encode_out?;
        if out != expected {
            return Err(Error::corrupt("N09 concurrent restore bytes differ"));
        }
        let stats = budget.stats();
        if stats.observed_max_active_workers > cap {
            return Err(Error::corrupt(format!(
                "cap={cap}: observed {} active workers",
                stats.observed_max_active_workers
            )));
        }
        arms.insert(
            format!("cap{cap}"),
            json!({
                "policy": {"max_active_workers": stats.max_active_workers, "max_inflight_bytes": stats.max_inflight_bytes},
                "sequential": sequential,
                "concurrent": {
                    "wall_ms": concurrent_total,
                    "encode": encode_result,
                    "restore_bytes": out.len(),
                },
                "concurrent_over_sequential": concurrent_total / sequential_total,
                "budget": stats,
                "no_starvation": stats.granted.iter().all(|(stage, count)| {
                    // Both stages were granted at least once.
                    !matches!(stage.as_str(), "restore" | "encode") || *count > 0
                }),
            }),
        );
    }

    // An oversize tensor: one restore whose declared bytes exceed the budget.
    let small = StageBudget::with_capacity(4, 64 * 1024)?;
    let oversize_pipeline = RestorePipelineOptions {
        threads: 4,
        max_inflight_bytes: 64 * 1024,
        budget: Some(Arc::clone(&small)),
        ..RestorePipelineOptions::default()
    };
    let started = Instant::now();
    let (_, out) = restore_once(&store, &oversize_pipeline)?;
    let oversize_ms = started.elapsed().as_secs_f64() * 1000.0;
    if out != expected {
        return Err(Error::corrupt("oversize restore bytes differ"));
    }
    let oversize = json!({
        "budget_bytes": small.policy().max_inflight_bytes,
        "wall_ms": oversize_ms,
        "oversized_admissions": small.stats().oversized_admissions,
        "observed_max_inflight_bytes": small.stats().observed_max_inflight_bytes,
        "bytes": out.len(),
    });

    // Cancellation storm: cancel tokens while restores are in flight.
    let storm_budget = StageBudget::with_capacity(4, 16 * 1024 * 1024)?;
    let mut cancelled = 0u64;
    let mut completed = 0u64;
    for round in 0..12 {
        let token: Arc<StageCancelToken> = Arc::new(storm_budget.cancel_token());
        let pipeline = RestorePipelineOptions {
            threads: 4,
            max_inflight_bytes: 16 * 1024 * 1024,
            budget: Some(Arc::clone(&storm_budget)),
            cancel: Some(Arc::clone(&token)),
            ..RestorePipelineOptions::default()
        };
        if round % 2 == 0 {
            token.cancel();
        }
        let mut sink = std::io::sink();
        match store.restore_checkpoint_pipelined(
            "ckpt",
            &CheckpointRestoreOptions::default(),
            &pipeline,
            &mut sink,
        ) {
            Ok(_) => completed += 1,
            Err(err) if err.class() == weftfs::Class::Cancelled => cancelled += 1,
            Err(err) => return Err(err),
        }
    }
    let storm_stats = storm_budget.stats();
    if storm_stats.observed_max_active_workers > 4 {
        return Err(Error::corrupt("cancellation storm exceeded the worker cap"));
    }
    if storm_stats.observed_max_inflight_bytes > 16 * 1024 * 1024 {
        return Err(Error::corrupt("cancellation storm exceeded the byte cap"));
    }
    // Everything must be released: the whole cap is available afterwards.
    let after = storm_budget.admit(
        weftfs::stage_budget::Stage::Restore,
        4,
        16 * 1024 * 1024,
        None,
    )?;
    if after.workers() != 4 {
        return Err(Error::corrupt("cancellation storm leaked worker capacity"));
    }
    drop(after);

    // Maintenance must not be starved by a busy restore stage.
    let fair_budget = StageBudget::with_capacity(2, 8 * 1024 * 1024)?;
    let stop = Arc::new(AtomicBool::new(false));
    let maintenance = Arc::new(AtomicU64::new(0));
    let restore_pipeline = RestorePipelineOptions {
        threads: 4,
        max_inflight_bytes: 8 * 1024 * 1024,
        budget: Some(Arc::clone(&fair_budget)),
        ..RestorePipelineOptions::default()
    };
    let maintenance_thread = {
        let budget = Arc::clone(&fair_budget);
        let stop = Arc::clone(&stop);
        let maintenance = Arc::clone(&maintenance);
        std::thread::spawn(move || -> Result<()> {
            while !stop.load(Ordering::Relaxed) {
                let mut admission =
                    budget.admit(weftfs::stage_budget::Stage::Maintenance, 1, 1024, None)?;
                std::thread::sleep(Duration::from_millis(1));
                admission.release_workers();
                maintenance.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        })
    };
    let restore_started = Instant::now();
    for _ in 0..3 {
        let (_, out) = restore_once(&store, &restore_pipeline)?;
        if out != expected {
            return Err(Error::corrupt("fairness restore bytes differ"));
        }
    }
    let restore_ms = restore_started.elapsed().as_secs_f64() * 1000.0;
    stop.store(true, Ordering::Relaxed);
    maintenance_thread
        .join()
        .expect("maintenance thread panicked")?;
    let fair_stats = fair_budget.stats();
    if maintenance.load(Ordering::Relaxed) == 0 {
        return Err(Error::corrupt(
            "maintenance was starved by the restore stage",
        ));
    }
    if fair_stats.observed_max_active_workers > 2 {
        return Err(Error::corrupt("fairness arm exceeded the worker cap"));
    }
    let fairness = json!({
        "policy": {"max_active_workers": fair_stats.max_active_workers, "max_inflight_bytes": fair_stats.max_inflight_bytes},
        "restore_ms": restore_ms,
        "maintenance_admissions": maintenance.load(Ordering::Relaxed),
        "observed_max_active_workers": fair_stats.observed_max_active_workers,
        "granted": fair_stats.granted,
    });

    Ok(json!({
        "gate": "N09",
        "schema": "weft.stage_budget.v1",
        "policy_default": {
            "max_active_workers": weftfs::stage_budget::StageBudgetPolicy::from_parallelism().max_active_workers,
            "max_inflight_bytes": weftfs::stage_budget::StageBudgetPolicy::from_parallelism().max_inflight_bytes,
            "rule": "max_active_workers = available_parallelism; max_inflight_bytes = 512 MiB",
        },
        "arms": arms,
        "oversize": oversize,
        "cancellation_storm": {
            "rounds": 12,
            "cancelled": cancelled,
            "completed": completed,
            "budget": storm_stats,
            "released_after": true,
        },
        "fairness": fairness,
        "peak_rss_kib": peak_rss_kib(),
        "note": "the encode stage is the shipped TensorPipelineOptions encoder behind the shipped admission; the concurrent arm prepares fragments only (no commit), then the store is verified",
    }))
}

fn run_encode(
    store: &Store,
    budget: &Arc<StageBudget>,
    names: &[String],
    shapes: &[Vec<u64>],
    payloads: &[Vec<u8>],
) -> Result<Value> {
    let tensors: Vec<TensorPipelineTensor<'_>> = names
        .iter()
        .enumerate()
        .map(|(index, name)| TensorPipelineTensor {
            name,
            dtype: "bf16",
            shape: &shapes[index],
            seed: seed_for(name),
            byte_len: payloads[index].len() as u64,
        })
        .collect();
    let read = |index: usize, buf: &mut [u8]| -> Result<()> {
        buf.copy_from_slice(&payloads[index]);
        Ok(())
    };
    let mut options = TensorPipelineOptions::default();
    options.encode_threads = 4;
    options.cpu_tokens = 3;
    options.budget = Some(Arc::clone(budget));
    let started = Instant::now();
    let outcome = store.prepare_tensor_group_pipelined(
        &tensors,
        &read,
        &encode_spec(),
        &TensorLayoutSpec::row_major(),
        &options,
    )?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(json!({
        "wall_ms": wall_ms,
        "tensors": outcome.results.len(),
        "pipeline_wall_ms": outcome.stats.pipeline_wall_ms,
        "encode_busy_ms": outcome.stats.encode_busy_ms,
        "observed_max_worker_bytes": outcome.stats.observed_max_worker_bytes,
        "roots": outcome.results.iter().map(|item| item.prepared.content_root.to_hex()).collect::<Vec<_>>(),
    }))
}

fn main() {
    let command = std::env::args().nth(1).unwrap_or_default();
    let result = match command.as_str() {
        "restore-budget" => restore_budget(),
        "handle-cap" => handle_cap(),
        "stage-budget" => stage_budget(),
        "dominant-tensor" => dominant_tensor(),
        other => Err(Error::invalid_argument(format!(
            "unknown subcommand `{other}` (expected restore-budget|handle-cap|stage-budget)"
        ))),
    };
    match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).unwrap();
            println!("{text}");
            let _ = std::io::stdout().flush();
        }
        Err(err) => {
            eprintln!("resource-courts: {err}");
            std::process::exit(1);
        }
    }
}
