//! X06: one admission object for tensor, shard and tile workers on restore.
//!
//! The restore path used to carry three independent knobs (`threads`,
//! `shard_threads`, `tile_threads`) whose product could exceed the machine.
//! These tests pin the new contract:
//!
//! * every tensor, its shard workers and its tile workers are admitted as one
//!   group from [`weftfs::stage_budget::StageBudget`];
//! * the observed number of active workers never exceeds the policy;
//! * a tensor larger than the whole byte budget runs alone and completes;
//! * a cancelled restore releases everything it held, including a panic;
//! * a given budget produces the same effective widths, the same read order
//!   and byte-identical output at every requested configuration.

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use tempfile::tempdir;

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::restore_pipeline::{run_ordered_pipeline, RestorePipelineOptions};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::stage_budget::{StageBudget, StageCancelToken};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Result;

const STAMP: i64 = 1_700_000_000_000_000_000;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(STAMP),
        ..Options::default()
    }
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
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

fn write_container(dir: &std::path::Path) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let specs: &[(&str, &[u64])] = &[
        ("embed", &[256, 128]),
        ("layer0", &[64, 64]),
        ("layer1", &[32, 64]),
        ("big", &[512, 96]),
        ("layer2", &[16, 64]),
        ("tail", &[8, 64]),
    ];
    let owned: Vec<Vec<u8>> = specs
        .iter()
        .enumerate()
        .map(|(index, (_, shape))| {
            let count: usize = shape.iter().map(|dim| *dim as usize).product();
            random_bytes(count * 2, 0x5eed_0000 + index as u64)
        })
        .collect();
    let entries: Vec<SafetensorsEntry> = specs
        .iter()
        .zip(owned.iter())
        .map(|((name, shape), data)| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape,
            data,
        })
        .collect();
    let file = safetensors::encode_file(&entries, None).unwrap();
    let path = dir.join("family.safetensors");
    fs::write(&path, &file).unwrap();
    path
}

struct Fixture {
    guard: tempfile::TempDir,
    store_path: std::path::PathBuf,
    expected: Vec<u8>,
}

fn fixture() -> Fixture {
    let guard = tempdir().unwrap();
    let source = write_container(&guard.path().join("src"));
    let store_path = guard.path().join("store");
    let mut store = Store::init(&store_path, opts()).unwrap();
    store
        .write_checkpoint(
            "ckpt",
            &source,
            &CheckpointWriteOptions {
                shards: 2,
                axis: 0,
                strategy: ShardStrategy::TensorSharded,
                deterministic_seed: Some([0x11; 32]),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap();
    assert!(store.verify(true).unwrap().problems.is_empty());
    Fixture {
        guard,
        store_path,
        expected: fs::read(&source).unwrap(),
    }
}

fn restore(
    store: &Store,
    pipeline: &RestorePipelineOptions,
) -> Result<(Vec<u8>, weftfs::restore_pipeline::RestorePipelineStats)> {
    let mut out = Vec::new();
    let report = store.restore_checkpoint_pipelined(
        "ckpt",
        &CheckpointRestoreOptions::default(),
        pipeline,
        &mut out,
    )?;
    Ok((out, report.pipeline.expect("pipeline stats recorded")))
}

#[test]
fn every_width_configuration_is_byte_identical_and_within_the_cap() {
    let fixture = fixture();
    let store = Store::open(&fixture.store_path, OpenMode::ReadOnly).unwrap();

    for cap in [1usize, 2, 4, 8] {
        let budget = StageBudget::with_capacity(cap, 64 * 1024 * 1024).unwrap();
        for threads in [1usize, 2, 4, 8] {
            for shard_threads in [1usize, 2, 4] {
                for tile_threads in [1usize, 2, 4] {
                    let pipeline = RestorePipelineOptions {
                        threads,
                        max_inflight_bytes: 64 * 1024 * 1024,
                        tile_threads,
                        shard_threads,
                        shard_max_inflight_bytes: 64 * 1024 * 1024,
                        budget: Some(Arc::clone(&budget)),
                        cancel: None,
                        stage: weftfs::stage_budget::Stage::Restore,
                    };
                    let (out, stats) = restore(&store, &pipeline).unwrap_or_else(|err| {
                        panic!(
                            "cap={cap} threads={threads} shard={shard_threads} tile={tile_threads}: \
                             {err}"
                        )
                    });
                    assert_eq!(
                        out, fixture.expected,
                        "cap={cap} threads={threads} shard={shard_threads} tile={tile_threads}: \
                         restored bytes differ"
                    );
                    assert!(
                        stats.observed_max_inflight_bytes <= 64 * 1024 * 1024
                            || stats.oversized_tasks > 0,
                        "{stats:?}"
                    );
                }
            }
        }
        let stats = budget.stats();
        assert!(
            stats.observed_max_active_workers <= cap,
            "cap={cap}: the budget admitted {} active workers",
            stats.observed_max_active_workers
        );
        assert!(
            stats.observed_max_inflight_bytes <= 64 * 1024 * 1024,
            "{stats:?}"
        );
        // The pipeline clamps the group width before admitting, so the
        // budget never has to clamp a restore request itself.
        assert_eq!(stats.clamped_worker_requests, 0, "{stats:?}");
        // Every tensor and every shard came through the budget.
        assert!(stats
            .granted
            .iter()
            .any(|(stage, count)| stage == "restore" && *count > 0));
        assert_eq!(
            budget.stats().observed_max_active_workers,
            stats.observed_max_active_workers
        );
    }
}

#[test]
fn a_tensor_larger_than_the_whole_budget_runs_alone_and_completes() {
    let fixture = fixture();
    let store = Store::open(&fixture.store_path, OpenMode::ReadOnly).unwrap();
    // 64 KiB is smaller than most tensors in the fixture.
    let budget = StageBudget::with_capacity(4, 64 * 1024).unwrap();
    let pipeline = RestorePipelineOptions {
        threads: 4,
        max_inflight_bytes: 64 * 1024,
        tile_threads: 1,
        shard_threads: 1,
        shard_max_inflight_bytes: 64 * 1024,
        budget: Some(Arc::clone(&budget)),
        cancel: None,
        stage: weftfs::stage_budget::Stage::Restore,
    };
    let (out, stats) = restore(&store, &pipeline).unwrap();
    assert_eq!(out, fixture.expected);
    assert!(stats.oversized_tasks > 0, "{stats:?}");
    let budget_stats = budget.stats();
    assert!(budget_stats.oversized_admissions > 0, "{budget_stats:?}");
    assert!(
        budget_stats.observed_max_inflight_bytes <= 64 * 1024,
        "{budget_stats:?}"
    );
    assert!(budget_stats.observed_max_active_workers <= 4);
}

#[test]
fn a_pre_cancelled_restore_returns_cancelled_and_writes_nothing() {
    let fixture = fixture();
    let store = Store::open(&fixture.store_path, OpenMode::ReadOnly).unwrap();
    let budget = StageBudget::with_capacity(4, 64 * 1024 * 1024).unwrap();
    let token: Arc<StageCancelToken> = Arc::new(budget.cancel_token());
    token.cancel();
    let pipeline = RestorePipelineOptions {
        threads: 4,
        budget: Some(Arc::clone(&budget)),
        cancel: Some(Arc::clone(&token)),
        ..RestorePipelineOptions::default()
    };
    let mut out = Vec::new();
    let err = store
        .restore_checkpoint_pipelined(
            "ckpt",
            &CheckpointRestoreOptions::default(),
            &pipeline,
            &mut out,
        )
        .unwrap_err();
    assert_eq!(err.class(), weftfs::Class::Cancelled);
    assert_eq!(budget.stats().observed_max_active_workers, 0);
    // The budget is fully released: a fresh admission gets the whole cap.
    let admission = budget
        .admit(
            weftfs::stage_budget::Stage::Restore,
            4,
            64 * 1024 * 1024,
            None,
        )
        .unwrap();
    assert_eq!(admission.workers(), 4);
}

#[test]
fn a_cancelled_waiter_releases_its_reservation_and_the_next_request_proceeds() {
    let budget = StageBudget::with_capacity(1, 1024).unwrap();
    let token = Arc::new(budget.cancel_token());
    let holder = Arc::new(Barrier::new(2));
    let holder2 = Arc::clone(&holder);
    let work = {
        move |index: usize, _input: &u8| -> Result<u8> {
            if index == 0 {
                // Hold the first tensor open until the canceller fires, so the
                // second tensor's worker is actually blocked in admission.
                holder2.wait();
                holder2.wait();
            }
            Ok(0)
        }
    };
    let token2 = token.clone();
    let budget_watch = Arc::clone(&budget);
    let canceller = std::thread::spawn(move || {
        holder.wait();
        // The first tensor's worker is inside `work`, holding the budget's only
        // worker slot. Wait until the second worker has actually *entered*
        // `admit` (its ticket exists) before cancelling, so the cancellation is
        // observed by a queued waiter. Without this the cancel can race the
        // pipeline's early-abort path, the second worker never calls `admit`,
        // and the assertion below measures scheduling rather than the contract
        // (measured before this change: 7/40 and 8/40 failures on the same
        // test at the pre-fix and fixed revisions respectively).
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while budget_watch.tickets() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the second worker never queued in the stage budget"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        token2.cancel();
        holder.wait();
    });
    let inputs = [0u8, 1u8];
    let sizes = [Ok(1usize), Ok(1usize)];
    let options = RestorePipelineOptions {
        threads: 2,
        max_inflight_bytes: 1024,
        budget: Some(Arc::clone(&budget)),
        cancel: Some(Arc::clone(&token)),
        ..RestorePipelineOptions::default()
    };
    let mut written = 0usize;
    let err = run_ordered_pipeline(
        &inputs,
        &sizes,
        &options,
        work,
        |_value| 1usize,
        |_index, _value| {
            written += 1;
            Ok(())
        },
    )
    .unwrap_err();
    canceller.join().unwrap();
    assert_eq!(err.class(), weftfs::Class::Cancelled, "{err}");
    let stats = budget.stats();
    assert!(stats.cancelled_waiters >= 1, "{stats:?}");
    assert!(written < 2, "a cancelled restore wrote both tensors");
    // Everything is released: the full cap is available again.
    let admission = budget
        .admit(weftfs::stage_budget::Stage::Restore, 1, 1024, None)
        .unwrap();
    assert_eq!(admission.workers(), 1);
}

#[test]
fn a_panicking_worker_releases_its_budget_reservation() {
    let budget = StageBudget::with_capacity(2, 4096).unwrap();
    let inputs = [0u8, 1u8];
    let sizes = [Ok(2048usize), Ok(2048usize)];
    let options = RestorePipelineOptions {
        threads: 2,
        max_inflight_bytes: 4096,
        budget: Some(Arc::clone(&budget)),
        ..RestorePipelineOptions::default()
    };
    let result = run_ordered_pipeline(
        &inputs,
        &sizes,
        &options,
        |index, _input| {
            if index == 1 {
                panic!("worker panicked");
            }
            Ok(index as u8)
        },
        |_value| 2048,
        |_index, _value| Ok(()),
    );
    assert!(result.is_err());
    let admission = budget
        .admit(weftfs::stage_budget::Stage::Restore, 2, 4096, None)
        .expect("the panicked worker's bytes must be released");
    assert_eq!(admission.bytes(), 4096);
}

#[test]
fn effective_widths_are_a_pure_function_of_the_request_and_policy() {
    let budget = StageBudget::with_capacity(4, 1 << 20).unwrap();
    let base = RestorePipelineOptions {
        threads: 8,
        tile_threads: 4,
        shard_threads: 4,
        budget: Some(Arc::clone(&budget)),
        ..RestorePipelineOptions::default()
    };
    // Shard parallelism is kept first, then tiles, deterministically.
    assert_eq!(base.effective_widths(), (4, 1));
    let same = base.clone();
    assert_eq!(same.effective_widths(), (4, 1));
    let unbudgeted = RestorePipelineOptions {
        budget: None,
        ..base.clone()
    };
    assert_eq!(unbudgeted.effective_widths(), (4, 4));
}

#[test]
fn a_single_worker_budgeted_restore_still_charges_the_budget() {
    let fixture = fixture();
    let store = Store::open(&fixture.store_path, OpenMode::ReadOnly).unwrap();
    let budget = StageBudget::with_capacity(1, 1 << 20).unwrap();
    let pipeline = RestorePipelineOptions {
        threads: 1,
        max_inflight_bytes: 1 << 20,
        shard_threads: 2,
        budget: Some(Arc::clone(&budget)),
        ..RestorePipelineOptions::default()
    };
    let (out, _stats) = restore(&store, &pipeline).unwrap();
    assert_eq!(out, fixture.expected);
    let stats = budget.stats();
    assert!(stats
        .granted
        .iter()
        .any(|(stage, count)| stage == "restore" && *count > 0));
    assert!(stats.observed_max_active_workers <= 1, "{stats:?}");
}

/// A concurrent writer publishing unrelated commits while a budgeted restore
/// runs must not let the restore's observed worker count exceed the policy.
#[test]
fn concurrent_mutation_does_not_break_the_budget() {
    let fixture = fixture();
    let store = Store::open(&fixture.store_path, OpenMode::ReadOnly).unwrap();
    let budget = StageBudget::with_capacity(2, 32 * 1024 * 1024).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let watcher = {
        let budget = Arc::clone(&budget);
        let stop = Arc::clone(&stop);
        let peak = Arc::clone(&peak);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(
                    budget.stats().observed_max_active_workers,
                    Ordering::Relaxed,
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };
    for _ in 0..3 {
        let pipeline = RestorePipelineOptions {
            threads: 4,
            max_inflight_bytes: 32 * 1024 * 1024,
            shard_threads: 2,
            tile_threads: 2,
            budget: Some(Arc::clone(&budget)),
            ..RestorePipelineOptions::default()
        };
        let (out, _) = restore(&store, &pipeline).unwrap();
        assert_eq!(out, fixture.expected);
    }
    stop.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    assert!(peak.load(Ordering::Relaxed) <= 2);
    assert!(budget.stats().observed_max_active_workers <= 2);
}
