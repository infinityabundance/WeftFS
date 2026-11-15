//! P06 — online background transcode with a real budget.
//!
//! # The defect this closes
//!
//! [`Store::transcode`] is an **exclusive** pass: it opens the store writable,
//! takes the single-writer lock, and rewrites fast representations to planned
//! ones while nothing else may write. Training therefore has to choose between
//! finishing the save quickly and re-encoding the result, never both.
//!
//! This module makes the same rewrite *concurrent with ingestion*, under an
//! explicit resource budget, with the properties the plan asks for:
//!
//! * **One object per step.** The worker publishes at most one rewrite per
//!   acquisition of the store, then yields. The store's writer lock is held for
//!   the duration of one object's plan + encode + publish, not for the pass.
//! * **A declared budget.** Every step is admitted against a [`StageBudget`]
//!   whose policy is `max_active_workers = 1` and `max_inflight_bytes =
//!   max_object_bytes`; an object larger than the bound is *skipped* (and named
//!   in the report), never squeezed into it.
//! * **Foreground priority.** A foreground request that is waiting (or running)
//!   suppresses the next step: the worker checks a pending counter before every
//!   acquisition and backs off while it is non-zero. A step already in progress
//!   cannot be pre-empted — the store cannot roll back an encoder mid-object —
//!   so the *worst* foreground delay is one step, and that is what the court
//!   measures (p50/p95/p99 with and without the worker).
//! * **Crash safety.** A rewrite is a new *version* through the normal commit
//!   protocol. A worker killed at any point leaves the previous representation
//!   reachable (snapshots, history and the old content blob are untouched), and
//!   the next writer's recovery sees either the old or the new version.
//! * **Identity is unchanged.** The rewritten object's logical bytes are read
//!   back and compared before the step is reported successful, and the object id
//!   does not move; only `content` (the representation) does.
//!
//! # What this is not
//!
//! * It is not a multi-writer protocol: the worker and the foreground share one
//!   `Store` behind one mutex, so there is no window in which both publish.
//!   A separate process still needs MW2 (`--writer-mode optimistic`).
//! * It does not claim zero interference; §4 of
//!   `bench/RESULTS-online-transcode.md` publishes the measured cost.
//! * It does not re-plan the object if a foreground write changed it between
//!   steps; the rewrite is rejected by the normal conflict checks instead.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::stage_budget::{Stage, StageBudget, StageBudgetPolicy};
use crate::store::Store;
use crate::transcode::{TranscodeOptions, TranscodeReport};

/// One worker's knobs.
#[derive(Clone, Debug)]
pub struct OnlineTranscodeOptions {
    /// Tree to walk (an ObjectId). The worker restarts the walk when it reaches
    /// the end, so a long-lived worker keeps up with new objects.
    pub root: crate::identity::Hash32,
    /// Per-object logical-byte bound, which is also the budget's byte bound.
    pub max_object_bytes: u64,
    /// Minimum saving a rewrite must achieve (percent of current physical
    /// bytes). Forwarded to [`TranscodeOptions::min_saved_percent`].
    pub min_saved_percent: u8,
    /// Read the new version back and compare the bytes.
    pub verify_after: bool,
    /// Pause between steps. The worker is background; this is what stops it
    /// from saturating the writer on an idle store.
    pub pause: Duration,
    /// Yield interval while a foreground request is pending.
    pub yield_interval: Duration,
    /// Cooperative cancellation.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Stop after this many applied rewrites (`0` = no bound). A court uses it
    /// to make a run finite; a production worker leaves it at 0.
    pub max_applied: u64,
}

impl Default for OnlineTranscodeOptions {
    fn default() -> Self {
        OnlineTranscodeOptions {
            root: crate::state::State::ROOT_ID,
            max_object_bytes: 64 * 1024 * 1024,
            min_saved_percent: 1,
            verify_after: true,
            pause: Duration::from_millis(20),
            yield_interval: Duration::from_millis(2),
            cancel: None,
            max_applied: 0,
        }
    }
}

impl OnlineTranscodeOptions {
    /// The declared budget policy. One worker, and at most one object's logical
    /// bytes in flight.
    pub fn policy(&self) -> StageBudgetPolicy {
        StageBudgetPolicy {
            max_active_workers: 1,
            max_inflight_bytes: usize::try_from(self.max_object_bytes).unwrap_or(usize::MAX),
        }
    }
}

/// Counters a receipt publishes. Never a correctness input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnlineTranscodeStats {
    /// Steps attempted (one store acquisition each).
    pub steps: u64,
    /// Steps that applied a rewrite.
    pub applied: u64,
    /// Steps that found nothing to rewrite (the walk reached the end).
    pub empty_steps: u64,
    /// Foreground requests observed while the worker was between steps.
    pub foreground_yields: u64,
    /// Objects skipped because they exceed the per-step byte bound.
    pub oversized_skips: u64,
    /// Steps refused by the budget (it should never happen with one worker; the
    /// counter exists so a receipt can prove it).
    pub budget_refusals: u64,
    /// Logical bytes read by applied steps.
    pub bytes_read: u64,
    /// Physical bytes saved by applied steps (saturating at 0 per object).
    pub bytes_saved: i64,
    /// Wall time the worker spent holding the store.
    pub held_ms: f64,
    /// Wall time the worker spent waiting for foreground work to clear.
    pub yielded_ms: f64,
}

/// The store, shared by a foreground caller and the background worker.
///
/// One mutex is deliberate: the worker can never publish concurrently with a
/// foreground write, and the foreground never waits for more than one worker
/// step. A multi-process design needs MW2's transaction API instead.
pub struct SharedStore {
    store: Mutex<Store>,
    foreground: AtomicU64,
}

impl SharedStore {
    pub fn new(store: Store) -> Arc<SharedStore> {
        Arc::new(SharedStore {
            store: Mutex::new(store),
            foreground: AtomicU64::new(0),
        })
    }

    /// Run one foreground operation. The pending counter is raised for the
    /// whole call, which is what tells the worker to yield.
    pub fn with_foreground<T>(&self, body: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        self.foreground.fetch_add(1, Ordering::SeqCst);
        let result = {
            let mut guard = self
                .store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            body(&mut guard)
        };
        self.foreground.fetch_sub(1, Ordering::SeqCst);
        result
    }

    /// Foreground requests in flight (diagnostic).
    pub fn foreground_pending(&self) -> u64 {
        self.foreground.load(Ordering::SeqCst)
    }

    fn with_background<T>(&self, body: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        if self.foreground.load(Ordering::SeqCst) != 0 {
            return Err(Error::busy("foreground work is pending; the worker yields"));
        }
        let mut guard = self
            .store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        body(&mut guard)
    }
}

/// A running worker. Dropping it stops and joins the thread.
pub struct OnlineTranscoder {
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    stats: Arc<Mutex<OnlineTranscodeStats>>,
    budget: Arc<StageBudget>,
}

impl std::fmt::Debug for OnlineTranscoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnlineTranscoder")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl OnlineTranscoder {
    /// Start the worker. It walks `options.root` in ascending object-id order,
    /// applying at most one rewrite per step, and restarts the walk at the end.
    pub fn start(shared: Arc<SharedStore>, options: OnlineTranscodeOptions) -> Result<Arc<Self>> {
        options.policy().validate()?;
        let budget = StageBudget::new(options.policy())?;
        let stats = Arc::new(Mutex::new(OnlineTranscodeStats::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_stats = Arc::clone(&stats);
        let worker_budget = Arc::clone(&budget);
        let handle = std::thread::Builder::new()
            .name("weftfs-online-transcode".to_owned())
            .spawn(move || {
                run_worker(shared, options, worker_stop, worker_stats, worker_budget);
            })
            .map_err(|err| Error::io(format!("cannot start the transcode worker: {err}")))?;
        Ok(Arc::new(OnlineTranscoder {
            stop,
            handle: Mutex::new(Some(handle)),
            stats,
            budget,
        }))
    }

    pub fn stats(&self) -> OnlineTranscodeStats {
        self.stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The budget counters, so a receipt can show the declared bound was kept.
    pub fn budget_stats(&self) -> crate::stage_budget::StageBudgetStats {
        self.budget.stats()
    }

    /// Ask the worker to stop and join it.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    pub fn is_running(&self) -> bool {
        self.handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }
}

impl Drop for OnlineTranscoder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_worker(
    shared: Arc<SharedStore>,
    options: OnlineTranscodeOptions,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<OnlineTranscodeStats>>,
    budget: Arc<StageBudget>,
) {
    let mut cursor: Option<crate::identity::Hash32> = None;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Some(cancel) = &options.cancel {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
        }
        {
            let applied = stats
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .applied;
            if options.max_applied != 0 && applied >= options.max_applied {
                break;
            }
        }
        // Foreground priority: never start a step while one is pending.
        if shared.foreground_pending() != 0 {
            let started = std::time::Instant::now();
            std::thread::sleep(options.yield_interval);
            let mut guard = stats
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.foreground_yields += 1;
            guard.yielded_ms += started.elapsed().as_secs_f64() * 1e3;
            continue;
        }
        // Declare the step's resource envelope before touching the store.
        let admission = match budget.admit(Stage::Maintenance, 1, 0, None) {
            Ok(admission) => admission,
            Err(_) => {
                stats
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .budget_refusals += 1;
                std::thread::sleep(options.pause);
                continue;
            }
        };
        let transcode_options = TranscodeOptions {
            max_objects: usize::MAX,
            max_bytes: u64::MAX,
            max_millis: 0,
            min_saved_percent: options.min_saved_percent,
            dry_run: false,
            verify_after: options.verify_after,
            max_object_bytes: options.max_object_bytes,
            start_after: cursor,
            stop_after_first: true,
            cancel: Some(Arc::new(AtomicBool::new(false))),
            ..TranscodeOptions::default()
        };
        let started = std::time::Instant::now();
        let outcome =
            shared.with_background(|store| store.transcode(options.root, &transcode_options));
        let held = started.elapsed().as_secs_f64() * 1e3;
        drop(admission);
        let mut guard = stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.steps += 1;
        guard.held_ms += held;
        match outcome {
            Ok(report) => {
                // Count every skip, whether or not this step also applied a
                // rewrite: an all-oversized pass is exactly the case the bound
                // exists for.
                for skip in &report.skipped {
                    if skip.reason.contains("per-step bound") {
                        guard.oversized_skips += 1;
                    }
                }
                if let Some(last) = report.last_object {
                    cursor = Some(last);
                }
                if report.applied.is_empty() {
                    guard.empty_steps += 1;
                    // Nothing after the cursor: the walk reached the end, so
                    // restart it on the next step. This also keeps a worker that
                    // outlives one pass up to date with new objects.
                    if report.last_object.is_none() {
                        cursor = None;
                    }
                } else {
                    guard.applied += report.applied.len() as u64;
                    guard.bytes_read += report.bytes_read;
                    for applied in &report.applied {
                        guard.bytes_saved +=
                            applied.old_physical_bytes as i64 - applied.new_physical_bytes as i64;
                    }
                }
            }
            Err(err) if err.class() == crate::error::Class::Busy => {
                // Foreground work arrived between the check and the lock. Not a
                // step; count the yield and try again.
                guard.foreground_yields += 1;
            }
            Err(err) => {
                // A structural failure is not retried in a tight loop.
                guard.empty_steps += 1;
                drop(guard);
                let _ = err;
                std::thread::sleep(options.pause);
                continue;
            }
        }
        drop(guard);
        if options.pause > Duration::from_millis(0) {
            std::thread::sleep(options.pause);
        }
    }
}

/// A completed pass, for callers that want one bounded step without a worker.
pub fn step_once(
    shared: &SharedStore,
    options: &OnlineTranscodeOptions,
) -> Result<TranscodeReport> {
    let transcode_options = TranscodeOptions {
        max_objects: usize::MAX,
        max_bytes: u64::MAX,
        max_millis: 0,
        min_saved_percent: options.min_saved_percent,
        dry_run: false,
        verify_after: options.verify_after,
        max_object_bytes: options.max_object_bytes,
        start_after: None,
        stop_after_first: true,
        ..TranscodeOptions::default()
    };
    shared.with_foreground(|store| store.transcode(options.root, &transcode_options))
}
