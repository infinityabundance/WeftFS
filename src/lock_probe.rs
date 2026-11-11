//! Backend-lock probe (subphase R13).
//!
//! The store keeps one `Mutex<Box<dyn StorageBackend + Send>>`. Every durable
//! read and write enters [`crate::store::Store::with_backend`] or
//! [`crate::store::Store::with_backend_mut`], so that one lock is a candidate
//! serialisation point for every concurrent read workload. This module
//! measures it. It does not change what the store does.
//!
//! # Contract
//!
//! * **Off by default.** [`LockProbe::enabled`] is false unless a court
//!   harness calls [`crate::store::Store::set_lock_probe`]. The disabled path
//!   is one relaxed atomic load per backend entry and allocates nothing.
//! * **Wall clock only.** `wait_ns` is the time between entering the helper and
//!   acquiring the mutex; `hold_ns` is the time from acquisition until the
//!   closure returns. Nothing is estimated.
//! * **Samples are bounded.** At most [`MAX_SAMPLES`] wait and hold durations
//!   are retained for percentiles. Beyond that only `count`, `total_ns`, and
//!   `max_ns` grow; the report states the retained sample count.
//! * **Sampling perturbs.** The probe takes one short mutex per observed
//!   backend entry to store the sample. That is why a court must compare
//!   *probe-on baseline* with *probe-on after* rather than mixing probe states.
//! * **Per-tensor attribution.** A worker may wrap one tensor decode in
//!   [`LockProbe::begin_tensor`]. Every backend entry on that thread while the
//!   guard is alive is attributed to that tensor, so a receipt can show how
//!   much of a tensor's decode wall time was spent waiting for, and holding,
//!   the store's backend lock.
//! * **Not a format.** Nothing here is persisted, hashed, or referenced by a
//!   commit or manifest. A probe run publishes exactly the bytes a
//!   non-probe run publishes.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde::Serialize;

/// Durations retained per section for percentile reporting.
pub const MAX_SAMPLES: usize = 250_000;

/// One measured section: mutex wait or mutex hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SectionSamples {
    /// Number of observed entries.
    pub count: u64,
    /// Sum of every entry duration.
    pub total_ns: u128,
    /// Longest single entry.
    pub max_ns: u64,
    /// Retained durations (bounded by [`MAX_SAMPLES`]).
    #[serde(skip)]
    pub samples: Vec<u64>,
}

impl SectionSamples {
    fn note(&mut self, ns: u64) {
        self.count = self.count.saturating_add(1);
        self.total_ns = self.total_ns.saturating_add(u128::from(ns));
        self.max_ns = self.max_ns.max(ns);
        if self.samples.len() < MAX_SAMPLES {
            self.samples.push(ns);
        }
    }

    /// Mean entry duration in nanoseconds.
    pub fn mean_ns(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total_ns as f64 / self.count as f64
        }
    }

    /// Nearest-rank percentile of the retained samples (`quantile` in `0..=1`).
    pub fn percentile_ns(&self, quantile: f64) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let clamped = quantile.clamp(0.0, 1.0);
        let rank = ((sorted.len() as f64 - 1.0) * clamped).round() as usize;
        sorted[rank.min(sorted.len() - 1)]
    }

    /// A serialisable summary without the raw samples.
    pub fn summary(&self) -> SectionSummary {
        SectionSummary {
            count: self.count,
            total_ns: self.total_ns,
            mean_ns: self.mean_ns(),
            max_ns: self.max_ns,
            p50_ns: self.percentile_ns(0.50),
            p95_ns: self.percentile_ns(0.95),
            p99_ns: self.percentile_ns(0.99),
            retained_samples: self.samples.len(),
        }
    }
}

/// Serialisable summary of one measured section.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct SectionSummary {
    pub count: u64,
    pub total_ns: u128,
    pub mean_ns: f64,
    pub max_ns: u64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub retained_samples: usize,
}

/// Backend-lock entries attributed to one tensor decode.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TensorLockRow {
    /// Caller-supplied index (the restore input position).
    pub index: usize,
    /// Backend entries observed on this thread while the guard was alive.
    pub entries: u64,
    /// Bytes the backend was asked to return for those entries.
    pub bytes: u64,
    /// Time spent waiting for the backend mutex.
    pub wait_ns: u128,
    /// Time spent holding the backend mutex.
    pub hold_ns: u128,
    /// Wall time from the start of the tensor decode to its end (including
    /// `wait_ns` and `hold_ns`).
    pub elapsed_ns: u128,
}

/// One probe report.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LockProbeReport {
    pub enabled: bool,
    pub wait: SectionSummary,
    pub hold: SectionSummary,
    /// Peak number of threads inside a backend helper at the same time
    /// (waiting plus holding).
    pub max_demand: u64,
    /// Mean number of threads inside a backend helper, sampled at each entry.
    pub mean_demand: f64,
    /// `hold.total_ns / wall_ns` when a wall time is supplied, else `None`.
    pub hold_share_of_wall: Option<f64>,
    /// `wait.total_ns / wall_ns` when a wall time is supplied, else `None`.
    pub wait_share_of_wall: Option<f64>,
    /// Per-tensor rows since the last reset.
    pub tensors: Vec<TensorLockRow>,
    /// Entries observed per calling thread. A single-threaded workload must
    /// show exactly one entry here; more than one means another thread entered
    /// a backend helper during the measured window.
    pub threads: Vec<ThreadProbeRow>,
}

/// Entries one calling thread contributed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ThreadProbeRow {
    /// Probe-local thread ordinal (assigned on first use, not a TID).
    pub thread: usize,
    pub entries: u64,
    /// Time spent holding the backend mutex on this thread.
    pub hold_ns: u128,
    /// Time spent waiting for it on this thread.
    pub wait_ns: u128,
}

impl LockProbeReport {
    /// Attach wall-clock shares from a measured wall time in milliseconds.
    pub fn with_wall_ms(mut self, wall_ms: f64) -> Self {
        let wall_ns = wall_ms * 1_000_000.0;
        if wall_ns > 0.0 {
            self.hold_share_of_wall = Some(self.hold.total_ns as f64 / wall_ns);
            self.wait_share_of_wall = Some(self.wait.total_ns as f64 / wall_ns);
        }
        self
    }
}

#[derive(Default)]
struct Sections {
    wait: SectionSamples,
    hold: SectionSamples,
    demand_samples: u64,
    demand_sum: u64,
    tensors: Vec<TensorLockRow>,
    threads: std::collections::BTreeMap<usize, ThreadProbeRow>,
}

/// The store's backend-lock probe.
pub struct LockProbe {
    enabled: AtomicBool,
    demand: AtomicU64,
    max_demand: AtomicU64,
    sections: Mutex<Sections>,
}

impl Default for LockProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl LockProbe {
    pub fn new() -> Self {
        LockProbe {
            enabled: AtomicBool::new(false),
            demand: AtomicU64::new(0),
            max_demand: AtomicU64::new(0),
            sections: Mutex::new(Sections::default()),
        }
    }

    /// Whether the probe is recording. One relaxed load on the disabled path.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Enable or disable sampling. Disabling does not discard what was
    /// recorded; call [`LockProbe::reset`] for that.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Discard every recorded sample, counter, and tensor row.
    pub fn reset(&self) {
        let mut guard = self.lock();
        *guard = Sections::default();
        self.demand.store(0, Ordering::Relaxed);
        self.max_demand.store(0, Ordering::Relaxed);
    }

    fn lock(&self) -> MutexGuard<'_, Sections> {
        self.sections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Enter a backend helper. The returned guard decrements demand on drop,
    /// including on a panic inside the closure.
    pub fn enter(&self) -> DemandGuard<'_> {
        let now = self.demand.fetch_add(1, Ordering::Relaxed) + 1;
        self.max_demand.fetch_max(now, Ordering::Relaxed);
        DemandGuard { probe: self }
    }

    /// Record one backend entry. Called on the calling thread, after the
    /// closure returns.
    pub fn note(&self, wait_ns: u64, hold_ns: u64) {
        let thread = thread_ordinal();
        {
            let mut guard = self.lock();
            guard.wait.note(wait_ns);
            guard.hold.note(hold_ns);
            let demand = self.demand.load(Ordering::Relaxed);
            guard.demand_samples = guard.demand_samples.saturating_add(1);
            guard.demand_sum = guard.demand_sum.saturating_add(demand);
            let row = guard.threads.entry(thread).or_insert(ThreadProbeRow {
                thread,
                ..ThreadProbeRow::default()
            });
            row.entries = row.entries.saturating_add(1);
            row.hold_ns = row.hold_ns.saturating_add(u128::from(hold_ns));
            row.wait_ns = row.wait_ns.saturating_add(u128::from(wait_ns));
        }
        TENSOR.with(|cell| {
            if let Some(accum) = cell.borrow_mut().as_mut() {
                accum.entries = accum.entries.saturating_add(1);
                accum.wait_ns = accum.wait_ns.saturating_add(u128::from(wait_ns));
                accum.hold_ns = accum.hold_ns.saturating_add(u128::from(hold_ns));
            }
        });
    }

    /// Attribute `bytes` returned by a backend read to the tensor currently
    /// being decoded on this thread, if a tensor guard is alive.
    pub fn note_bytes(&self, bytes: u64) {
        TENSOR.with(|cell| {
            if let Some(accum) = cell.borrow_mut().as_mut() {
                accum.bytes = accum.bytes.saturating_add(bytes);
            }
        });
    }

    /// Start attributing backend entries on this thread to one tensor input.
    ///
    /// The guard publishes its row when dropped. Nested guards are refused by
    /// construction: a worker runs one tensor at a time.
    pub fn begin_tensor(&self, index: usize) -> TensorGuard<'_> {
        TENSOR.with(|cell| {
            *cell.borrow_mut() = Some(TensorAccum::default());
        });
        TensorGuard {
            probe: self,
            index,
            active: true,
            started: Instant::now(),
        }
    }

    /// Report everything recorded since the last reset.
    pub fn report(&self) -> LockProbeReport {
        let guard = self.lock();
        let mean_demand = if guard.demand_samples == 0 {
            0.0
        } else {
            guard.demand_sum as f64 / guard.demand_samples as f64
        };
        LockProbeReport {
            enabled: self.enabled(),
            wait: guard.wait.summary(),
            hold: guard.hold.summary(),
            max_demand: self.max_demand.load(Ordering::Relaxed),
            mean_demand,
            hold_share_of_wall: None,
            wait_share_of_wall: None,
            tensors: guard.tensors.clone(),
            threads: guard.threads.values().copied().collect(),
        }
    }
}

/// Demand counter guard. Decrements on drop.
pub struct DemandGuard<'a> {
    probe: &'a LockProbe,
}

impl Drop for DemandGuard<'_> {
    fn drop(&mut self) {
        self.probe.demand.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct TensorAccum {
    entries: u64,
    bytes: u64,
    wait_ns: u128,
    hold_ns: u128,
}

thread_local! {
    static TENSOR: RefCell<Option<TensorAccum>> = const { RefCell::new(None) };
    static THREAD_ORDINAL: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

static NEXT_THREAD_ORDINAL: AtomicU64 = AtomicU64::new(0);

/// Probe-local thread ordinal, assigned on first use.
fn thread_ordinal() -> usize {
    THREAD_ORDINAL.with(|cell| {
        if let Some(ordinal) = cell.get() {
            return ordinal;
        }
        let ordinal = NEXT_THREAD_ORDINAL.fetch_add(1, Ordering::Relaxed) as usize;
        cell.set(Some(ordinal));
        ordinal
    })
}

/// Publishes one tensor's attribution row when dropped.
pub struct TensorGuard<'a> {
    probe: &'a LockProbe,
    index: usize,
    active: bool,
    started: Instant,
}

impl Drop for TensorGuard<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let elapsed_ns = self.started.elapsed().as_nanos();
        let accum = TENSOR.with(|cell| cell.borrow_mut().take());
        let Some(accum) = accum else {
            return;
        };
        let mut guard = self.probe.lock();
        guard.tensors.push(TensorLockRow {
            index: self.index,
            entries: accum.entries,
            bytes: accum.bytes,
            wait_ns: accum.wait_ns,
            hold_ns: accum.hold_ns,
            elapsed_ns,
        });
    }
}

/// A guard that measures one section and records it on drop.
///
/// Used by [`crate::store::Store::with_backend`] so a panic inside a backend
/// closure still leaves a balanced demand counter.
pub(crate) struct SectionTimer<'a> {
    probe: &'a LockProbe,
    wait_started: Instant,
    hold_started: Option<Instant>,
}

impl<'a> SectionTimer<'a> {
    pub(crate) fn new(probe: &'a LockProbe) -> Self {
        SectionTimer {
            probe,
            wait_started: Instant::now(),
            hold_started: None,
        }
    }

    /// Call immediately after the mutex is acquired.
    pub(crate) fn acquired(&mut self) {
        self.hold_started = Some(Instant::now());
    }
}

impl Drop for SectionTimer<'_> {
    fn drop(&mut self) {
        let now = Instant::now();
        let (wait_ns, hold_ns) = match self.hold_started {
            Some(hold_started) => (
                hold_started.duration_since(self.wait_started),
                now.duration_since(hold_started),
            ),
            None => (
                now.duration_since(self.wait_started),
                std::time::Duration::ZERO,
            ),
        };
        self.probe
            .note(wait_ns.as_nanos() as u64, hold_ns.as_nanos() as u64);
    }
}
