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
//! * **Bounded.** At most [`MAX_IDLE_HANDLES`] handles are kept; one handle is
//!   checked out per concurrent read and returned on completion.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde::Serialize;

use crate::backend::{BackendKind, BackendStats, Key, StorageBackend};
use crate::error::Result;

/// Maximum idle handles retained by one store handle.
pub const MAX_IDLE_HANDLES: usize = 64;

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
    /// Bytes returned by those reads.
    pub bytes: u64,
    /// Reads in flight at this instant.
    pub in_flight: u64,
    /// Largest number of simultaneous reads observed.
    pub max_in_flight: u64,
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
        }
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
        let mut handle = self.acquire()?;
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
        let mut handle = self.acquire()?;
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
        ReadPoolStats {
            poolable: self.poolable,
            handles_created: self.handles_created.load(Ordering::Relaxed),
            handles_reused: self.handles_reused.load(Ordering::Relaxed),
            idle_handles: Self::lock(&self.idle).len(),
            reads: self.reads.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            max_in_flight: self.max_in_flight.load(Ordering::Relaxed),
            io_total_ns: u128::from(self.io_total_ns.load(Ordering::Relaxed)),
            io_max_ns: self.io_max_ns.load(Ordering::Relaxed),
            epoch: self.epoch.load(Ordering::Relaxed),
        }
    }
}
