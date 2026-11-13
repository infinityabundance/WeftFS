//! Immutable-content read handles (subphase R13).
//!
//! The store's mutable control state (superblock, commit records, indexes,
//! optional caches) needs one exclusive writer and a serialising mutex. The
//! immutable content-addressed blobs in `objects/` and `manifests/` do not:
//! once published they never change, and a reader only needs a backend handle
//! and the object's identity.
//!
//! [`ReadHandlePool`] keeps a small set of backend handles for those reads, so
//! independent readers never contend on the store's state mutex. Properties:
//!
//! * **Never used for control state.** Only [`crate::backend::Key::object`] and
//!   [`crate::backend::Key::manifest`] reads go through this pool. The store's
//!   mutex remains the only path for mutable records and all publication.
//! * **Never used for bytes this handle has buffered.** A batching blob plane
//!   keeps a freshly packed member in its own open pack (or deferred list)
//!   until the catalog names it. Those keys resolve only through the handle
//!   that buffered them, so [`crate::store::Store`] reads them through the
//!   writer and counts them separately (`buffered_blob_reads`).
//! * **Bounded.** At most [`MAX_IDLE_HANDLES`] handles are kept; one handle is
//!   checked out per concurrent read and returned on completion. At most
//!   [`MAX_ACTIVE_HANDLES`] handles may be *active* at once (X07): the cap is
//!   acquired before a handle is checked out and released on drop, so a fanout
//!   of readers cannot hand the backend an unbounded number of concurrent
//!   handles. A `0` cap restores the pre-X07 unbounded behaviour for a control
//!   arm and is labelled as such in every receipt.
//! * **Invalidated by mutation.** Every backend mutation bumps the pool's
//!   epoch; a handle from an older epoch is dropped rather than reused. That
//!   matters for a packed blob plane, whose handle caches the pack catalog as
//!   of its creation: a stale catalog could miss a member the writer just
//!   published.
//! * **Counted.** A returned handle's backend counters are diffed into a
//!   harvested [`BackendStats`], so `Store::backend_stats()` still reports the
//!   bytes and requests the pool served.
//! * **Fail-closed.** A handle that cannot be constructed is an error; there
//!   is no fallback that would read control state from the wrong plane.
//! * **3FS is not pooled.** Each 3FS handle opens its own USRBIO ring set, so
//!   the pool is disabled for that transport and reads keep the state-mutex
//!   path (documented limitation, not a silent substitution).

use std::mem;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Instant;

use serde::Serialize;

use crate::backend::{BackendKind, BackendStats, Key, StorageBackend};
use crate::error::Result;

/// Maximum idle handles retained by one store handle.
pub const MAX_IDLE_HANDLES: usize = 64;

/// Default cap on simultaneously *active* handles (X07).
///
/// Idle handles and active handles are different resources. The idle cap bounds
/// cached descriptors; the active cap bounds what the backend is asked to open
/// at once. `1/4/16/64` concurrent readers are measured in
/// `bench/RESULTS-handle-cap.md`; the cap is set above the point where the
/// backend stops scaling on the courts, so it never serialises the measured
/// fanout.
pub const MAX_ACTIVE_HANDLES: usize = 64;

struct PooledHandle {
    epoch: u64,
    backend: Box<dyn StorageBackend + Send>,
    /// Backend counters as of the last harvest, so a release can add only the
    /// delta this handle produced.
    last: BackendStats,
}

/// Counters for receipts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReadPoolStats {
    /// Whether immutable reads use the pool at all for this transport.
    pub poolable: bool,
    /// Handles constructed since the store opened.
    pub handles_created: u64,
    /// Times a read reused an idle handle.
    pub handles_reused: u64,
    /// Handles currently idle.
    pub idle_handles: usize,
    /// Blob reads served by the pool.
    pub reads: u64,
    /// Reads this handle served itself because it held a blob member that no
    /// other handle could resolve yet (a batching plane's open pack). They are
    /// excluded from `reads` and from `bytes`.
    #[serde(default)]
    pub buffered_blob_reads: u64,
    /// Bytes returned by those reads.
    pub bytes: u64,
    /// Reads in flight at this instant.
    pub in_flight: u64,
    /// Largest number of simultaneous reads observed.
    pub max_in_flight: u64,
    /// Configured cap on simultaneously active handles (0 = unbounded).
    pub active_cap: usize,
    /// Handles active at this instant (checked out or in the checkout path).
    pub active_handles: u64,
    /// Largest number of simultaneously active handles observed (the X07
    /// high-water mark; it is asserted never to exceed `active_cap`).
    pub observed_max_active_handles: u64,
    /// Reads that had to wait for an active-handle slot.
    pub cap_waits: u64,
    /// Total time blocked waiting for an active-handle slot, in nanoseconds.
    pub cap_wait_ns: u128,
    /// Total time inside backend read calls.
    pub io_total_ns: u128,
    /// Longest single backend read call.
    pub io_max_ns: u64,
    /// Current invalidation epoch.
    pub epoch: u64,
}

/// A bounded pool of handles for immutable blob reads.
pub struct ReadHandlePool {
    kind: BackendKind,
    root: PathBuf,
    poolable: bool,
    epoch: AtomicU64,
    idle: Mutex<Vec<PooledHandle>>,
    handles_created: AtomicU64,
    handles_reused: AtomicU64,
    reads: AtomicU64,
    bytes: AtomicU64,
    in_flight: AtomicU64,
    max_in_flight: AtomicU64,
    io_total_ns: AtomicU64,
    io_max_ns: AtomicU64,
    harvested: Mutex<BackendStats>,
    /// Set when the pool observed that constructing a handle failed once, so a
    /// later receipt can say reads had to fall back rather than guess.
    construction_failures: AtomicBool,
    /// X07: cap on simultaneously active handles. `0` means the pre-X07
    /// unbounded behaviour (control arm only).
    active_cap: AtomicUsize,
    active: Mutex<ActiveHandles>,
    active_cv: Condvar,
    cap_waits: AtomicU64,
    cap_wait_ns: AtomicU64,
}

/// Active-handle bookkeeping for the X07 cap.
struct ActiveHandles {
    active: usize,
    observed_max: usize,
}

/// RAII lease on one active-handle slot.
///
/// Acquired before a handle is checked out and released on drop, so a panic or
/// an early return can never leak a slot (there is no manual release path).
struct ActiveLease<'a> {
    pool: &'a ReadHandlePool,
}

impl Drop for ActiveLease<'_> {
    fn drop(&mut self) {
        let mut guard = Self::lock(&self.pool.active);
        guard.active = guard.active.saturating_sub(1);
        drop(guard);
        self.pool.active_cv.notify_one();
    }
}

impl<'a> ActiveLease<'a> {
    fn lock<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
        lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Wait for a slot under `cap`, or return `None` when the cap is disabled.
    fn acquire(pool: &'a ReadHandlePool, cap: usize) -> Option<Self> {
        if cap == 0 {
            return None;
        }
        let mut guard = Self::lock(&pool.active);
        let mut waited = false;
        let mut wait_started = Instant::now();
        while guard.active >= cap {
            if !waited {
                pool.cap_waits.fetch_add(1, Ordering::Relaxed);
                wait_started = Instant::now();
                waited = true;
            }
            guard = pool
                .active_cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if waited {
            pool.cap_wait_ns
                .fetch_add(wait_started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        guard.active += 1;
        guard.observed_max = guard.observed_max.max(guard.active);
        drop(guard);
        Some(ActiveLease { pool })
    }
}

impl ReadHandlePool {
    /// Build a pool for `kind` rooted at `root`.
    pub fn new(kind: BackendKind, root: impl Into<PathBuf>) -> Self {
        // A 3FS handle opens its own USRBIO rings; constructing one per reader
        // would duplicate a scarce transport resource. Reads keep the state
        // mutex for that transport and the limitation is reported.
        let poolable = !matches!(kind, BackendKind::ThreeFs { .. });
        ReadHandlePool {
            kind,
            root: root.into(),
            poolable,
            epoch: AtomicU64::new(0),
            idle: Mutex::new(Vec::new()),
            handles_created: AtomicU64::new(0),
            handles_reused: AtomicU64::new(0),
            reads: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            max_in_flight: AtomicU64::new(0),
            io_total_ns: AtomicU64::new(0),
            io_max_ns: AtomicU64::new(0),
            harvested: Mutex::new(BackendStats::default()),
            construction_failures: AtomicBool::new(false),
            active_cap: AtomicUsize::new(MAX_ACTIVE_HANDLES),
            active: Mutex::new(ActiveHandles {
                active: 0,
                observed_max: 0,
            }),
            active_cv: Condvar::new(),
            cap_waits: AtomicU64::new(0),
            cap_wait_ns: AtomicU64::new(0),
        }
    }

    /// The configured cap on simultaneously active handles (`0` = unbounded).
    pub fn active_cap(&self) -> usize {
        self.active_cap.load(Ordering::Acquire)
    }

    /// Change the active-handle cap (X07 court and embedded callers).
    ///
    /// `0` disables the cap and restores the pre-X07 unbounded behaviour; that
    /// is the control arm of the handle-cap court and is recorded as such. The
    /// change takes effect for the next read; in-flight leases finish under the
    /// cap they acquired.
    pub fn set_active_cap(&self, cap: usize) {
        self.active_cap.store(cap, Ordering::Release);
        self.active_cv.notify_all();
    }

    /// Whether immutable reads use this pool for the current transport.
    pub fn poolable(&self) -> bool {
        self.poolable
    }

    /// Whether a handle construction failed at least once.
    pub fn construction_failed(&self) -> bool {
        self.construction_failures.load(Ordering::Relaxed)
    }

    fn lock<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
        lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drop every idle handle and make every checked-out handle stale.
    ///
    /// Called after any backend mutation. In-flight readers keep the handle
    /// they are using (their blob is immutable); they simply do not return it
    /// to the pool.
    pub fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        let drained = {
            let mut idle = Self::lock(&self.idle);
            mem::take(&mut *idle)
        };
        for handle in drained {
            self.harvest(handle);
        }
    }

    /// Harvest a handle's counter delta into the pool total, then drop it.
    fn harvest(&self, mut handle: PooledHandle) {
        let now = handle.backend.stats();
        let delta = now.delta_since(&handle.last);
        handle.last = now;
        {
            let mut harvested = Self::lock(&self.harvested);
            *harvested = harvested.merged(&delta);
        }
        drop(handle);
    }

    fn acquire(&self) -> Result<PooledHandle> {
        let epoch = self.epoch.load(Ordering::Acquire);
        loop {
            let popped = {
                let mut idle = Self::lock(&self.idle);
                idle.pop()
            };
            match popped {
                Some(handle) if handle.epoch == epoch => {
                    self.handles_reused.fetch_add(1, Ordering::Relaxed);
                    return Ok(handle);
                }
                Some(handle) => {
                    // A stale handle (the store mutated since it was created).
                    self.harvest(handle);
                }
                None => break,
            }
        }
        self.handles_created.fetch_add(1, Ordering::Relaxed);
        let backend = match self.kind.open(&self.root) {
            Ok(backend) => backend,
            Err(err) => {
                self.construction_failures.store(true, Ordering::Relaxed);
                return Err(err);
            }
        };
        Ok(PooledHandle {
            epoch,
            backend,
            last: BackendStats::default(),
        })
    }

    fn release(&self, mut handle: PooledHandle) {
        let now = handle.backend.stats();
        let delta = now.delta_since(&handle.last);
        handle.last = now;
        {
            let mut harvested = Self::lock(&self.harvested);
            *harvested = harvested.merged(&delta);
        }
        if handle.epoch == self.epoch.load(Ordering::Acquire) {
            let mut idle = Self::lock(&self.idle);
            if idle.len() < MAX_IDLE_HANDLES {
                idle.push(handle);
                return;
            }
        }
        drop(handle);
    }

    /// Read a whole immutable blob through a pooled handle.
    ///
    /// Returns the bytes and the time spent inside the backend read call. The
    /// hash check is the caller's job and must run after this returns, so it
    /// does not hold a handle.
    pub fn read_all(&self, key: &Key) -> Result<(Vec<u8>, u64)> {
        let _lease = ActiveLease::acquire(self, self.active_cap());
        let handle = self.acquire()?;
        let now = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.max_in_flight.fetch_max(now, Ordering::Relaxed);
        let started = Instant::now();
        let outcome = handle.backend.read_all(key);
        let io_ns = started.elapsed().as_nanos() as u64;
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.io_total_ns.fetch_add(io_ns, Ordering::Relaxed);
        self.io_max_ns.fetch_max(io_ns, Ordering::Relaxed);
        self.release(handle);
        let bytes = outcome?;
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok((bytes, io_ns))
    }

    /// Read a byte range of an immutable blob through a pooled handle.
    pub fn read_range(&self, key: &Key, offset: u64, len: u64) -> Result<(Vec<u8>, u64)> {
        let _lease = ActiveLease::acquire(self, self.active_cap());
        let handle = self.acquire()?;
        let now = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.max_in_flight.fetch_max(now, Ordering::Relaxed);
        let started = Instant::now();
        let outcome = handle.backend.read(key, offset, len);
        let io_ns = started.elapsed().as_nanos() as u64;
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.io_total_ns.fetch_add(io_ns, Ordering::Relaxed);
        self.io_max_ns.fetch_max(io_ns, Ordering::Relaxed);
        self.release(handle);
        let bytes = outcome?;
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok((bytes, io_ns))
    }

    /// Backend counters harvested from returned handles.
    pub fn harvested(&self) -> BackendStats {
        Self::lock(&self.harvested).clone()
    }

    /// A point-in-time counter snapshot.
    pub fn stats(&self) -> ReadPoolStats {
        let active = Self::lock(&self.active);
        ReadPoolStats {
            poolable: self.poolable,
            handles_created: self.handles_created.load(Ordering::Relaxed),
            handles_reused: self.handles_reused.load(Ordering::Relaxed),
            idle_handles: Self::lock(&self.idle).len(),
            reads: self.reads.load(Ordering::Relaxed),
            buffered_blob_reads: 0,
            bytes: self.bytes.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            max_in_flight: self.max_in_flight.load(Ordering::Relaxed),
            active_cap: self.active_cap.load(Ordering::Relaxed),
            active_handles: active.active as u64,
            observed_max_active_handles: active.observed_max as u64,
            cap_waits: self.cap_waits.load(Ordering::Relaxed),
            cap_wait_ns: u128::from(self.cap_wait_ns.load(Ordering::Relaxed)),
            io_total_ns: u128::from(self.io_total_ns.load(Ordering::Relaxed)),
            io_max_ns: self.io_max_ns.load(Ordering::Relaxed),
            epoch: self.epoch.load(Ordering::Relaxed),
        }
    }
}
