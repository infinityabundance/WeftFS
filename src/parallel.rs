//! Bounded, deterministic parallel reads.
//!
//! [`Store::read_tensors_bounded`] decodes several tensor objects at once. It
//! exists because a real-checkpoint restore is dominated by independent
//! per-tensor decodes, and the decode never holds the backend mutex:
//! [`Store::with_backend`] locks only for the I/O closure, so two decodes can
//! run concurrently on the same `&Store`.
//!
//! The same runner also decodes the independently coded tiles of one mode-15
//! `TENSOR_TILED` object ([`TileDecodeOptions`]).
//!
//! Guarantees:
//!
//! * **Order.** The result vector is exactly the input order, whatever the
//!   thread count and the scheduling.
//! * **Determinism.** Decoding is a read-only function of the store state and
//!   one object id (or one tile), so both the bytes and the error are functions
//!   of the input list alone. Nothing here depends on completion order.
//! * **One error policy.** If any read fails the call returns `Err`, and the
//!   error is the one for the **lowest failing input index**. Every claimed
//!   task still runs to completion and every worker is joined before the error
//!   is returned, so a panic or a leaked thread cannot escape. A decode panic
//!   is caught and converted into a `Corrupt` error for that index.
//! * **One worker means the old path.** `threads` values that resolve to one
//!   effective worker run the shipped sequential loop, so bytes and cost are
//!   those of [`Store::read_tensor`] in a loop (or one tile decode in a loop).
//! * **Memory.** Before decoding, a worker reserves its task's declared size.
//!   The sum of reservations (decoding + published-but-unconsumed results)
//!   never exceeds `max_inflight_bytes`, so workers cannot run arbitrarily far
//!   ahead of the consumer. A task whose declared size alone exceeds the bound
//!   waits until nothing else is reserved and then runs alone; two oversized
//!   tasks are never concurrent. The reservation is the *declared* size, so
//!   this bounds staging and queue depth, not a decoder's peak allocator use
//!   (a decoder may temporarily need a multiple of the logical size).
//!
//! The consumer (the calling thread) drains results as they are published, in
//! no particular order, and re-orders them at the end; that is what keeps the
//! reservation bound from deadlocking against `max_inflight_bytes`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

use crate::error::{Error, Result};
use crate::identity::Hash32;
use crate::store::Store;

/// Default soft bound on reserved decoded bytes: 64 MiB.
pub const DEFAULT_MAX_INFLIGHT_BYTES: usize = 64 * 1024 * 1024;

/// Worker bound and staging bound for [`Store::read_tensors_bounded`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParallelReadOptions {
    /// Requested worker count. `0` means one worker; the effective count is
    /// also capped at [`std::thread::available_parallelism`].
    pub threads: usize,
    /// Soft bound on the declared decoded bytes reserved at any time by
    /// workers that are decoding or whose result has not been consumed yet.
    pub max_inflight_bytes: usize,
}

impl Default for ParallelReadOptions {
    fn default() -> Self {
        ParallelReadOptions {
            threads: 1,
            max_inflight_bytes: DEFAULT_MAX_INFLIGHT_BYTES,
        }
    }
}

/// Worker bound and staging bound for decoding the tiles of one mode-15 object.
///
/// The default is deliberately the shipped sequential behaviour (`threads = 1`):
/// no existing read changes unless a caller asks for more workers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileDecodeOptions {
    /// Requested worker count. `0` means one worker; the effective count is
    /// also capped at [`std::thread::available_parallelism`].
    pub threads: usize,
    /// Soft bound on the decoded tile bytes reserved at any time by workers
    /// that are decoding or whose result has not been consumed yet.
    pub max_inflight_bytes: usize,
}

impl Default for TileDecodeOptions {
    fn default() -> Self {
        TileDecodeOptions {
            threads: 1,
            max_inflight_bytes: DEFAULT_MAX_INFLIGHT_BYTES,
        }
    }
}

/// What one bounded read actually did, for tests and receipts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParallelReadStats {
    /// The requested thread count, verbatim.
    pub requested_threads: usize,
    /// The worker count after the `0 -> 1` and `available_parallelism` rules.
    pub effective_threads: usize,
    /// Number of tensor ids read.
    pub tasks: usize,
    /// The configured `max_inflight_bytes`.
    pub max_inflight_bytes: usize,
    /// The largest observed sum of reserved decoded bytes.
    pub observed_max_inflight_bytes: usize,
    /// The largest observed number of concurrently reserved tasks (decoding
    /// plus published-but-unconsumed).
    pub observed_max_inflight_tasks: usize,
    /// Tasks whose declared size alone exceeded `max_inflight_bytes` and that
    /// therefore ran alone.
    pub oversized_tasks: usize,
}

/// What one bounded tile decode actually did, for tests and receipts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileDecodeStats {
    /// The requested thread count, verbatim.
    pub requested_threads: usize,
    /// The worker count after the `0 -> 1` and `available_parallelism` rules.
    pub effective_threads: usize,
    /// Number of tiles decoded and root-verified.
    pub tiles: usize,
    /// The configured `max_inflight_bytes`.
    pub max_inflight_bytes: usize,
    /// The largest observed sum of reserved decoded tile bytes.
    pub observed_max_inflight_bytes: usize,
    /// The largest observed number of concurrently reserved tiles (decoding
    /// plus published-but-unconsumed).
    pub observed_max_inflight_tasks: usize,
    /// Tiles whose declared size alone exceeded `max_inflight_bytes` and that
    /// therefore ran alone.
    pub oversized_tiles: usize,
}

impl TileDecodeStats {
    /// Stats for a read that did not decode any tile (a non-tiled fallback).
    pub(crate) fn none(options: &TileDecodeOptions) -> Self {
        TileDecodeStats {
            requested_threads: options.threads,
            effective_threads: effective_threads(options.threads),
            max_inflight_bytes: options.max_inflight_bytes,
            ..TileDecodeStats::default()
        }
    }
}

/// The worker count actually used for a requested count.
pub fn effective_threads(requested: usize) -> usize {
    let available = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);
    requested.max(1).min(available.max(1))
}

/// Internal counters shared by every bounded runner.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BoundedRunStats {
    pub requested_threads: usize,
    pub effective_threads: usize,
    pub tasks: usize,
    pub max_inflight_bytes: usize,
    pub observed_max_inflight_bytes: usize,
    pub observed_max_inflight_tasks: usize,
    pub oversized_tasks: usize,
}

/// Run `work` over `inputs` with a bounded worker pool and a staging bound.
///
/// See the [module documentation](self) for the ordering, determinism, error,
/// and memory guarantees. `sizes[index]` is the declared size reserved for
/// `inputs[index]` before its work starts.
pub(crate) fn run_bounded<T, U, F, M>(
    inputs: &[T],
    sizes: &[Result<usize>],
    threads: usize,
    bound: usize,
    work: F,
    measure: M,
) -> Result<(Vec<U>, BoundedRunStats)>
where
    T: Sync,
    U: Send,
    F: Fn(usize, &T) -> Result<U> + Sync,
    M: Fn(&U) -> usize + Sync,
{
    assert_eq!(inputs.len(), sizes.len(), "one declared size per input");
    let effective = effective_threads(threads);
    let mut stats = BoundedRunStats {
        requested_threads: threads,
        effective_threads: effective,
        tasks: inputs.len(),
        max_inflight_bytes: bound,
        ..BoundedRunStats::default()
    };
    if inputs.is_empty() {
        return Ok((Vec::new(), stats));
    }

    stats.oversized_tasks = sizes
        .iter()
        .filter(|item| matches!(item, Ok(len) if *len > bound))
        .count();

    if effective <= 1 {
        // The shipped sequential path: identical bytes at essentially the
        // existing cost.
        let mut out = Vec::with_capacity(inputs.len());
        for (index, input) in inputs.iter().enumerate() {
            match &sizes[index] {
                Ok(_) => out.push(work(index, input)?),
                Err(err) => return Err(err.clone()),
            }
        }
        return Ok((out, stats));
    }

    let queue = Mutex::new(Queue {
        inflight_bytes: 0,
        inflight_tasks: 0,
        slots: (0..inputs.len()).map(|_| None).collect(),
        oversized_waiting: false,
        live_workers: effective,
        drained: false,
    });
    let cv = Condvar::new();
    let next = AtomicUsize::new(0);
    let observed_bytes = AtomicUsize::new(0);
    let observed_tasks = AtomicUsize::new(0);
    let oversized_ran = AtomicUsize::new(0);

    let mut collected: Vec<Option<U>> = (0..inputs.len()).map(|_| None).collect();
    let mut failure: Option<(usize, Error)> = None;
    let mut missing: Option<usize> = None;

    std::thread::scope(|scope| {
        for _ in 0..effective {
            scope.spawn(|| {
                while let Some(index) = claim(&next, inputs.len()) {
                    let reserve = match &sizes[index] {
                        Ok(len) => *len,
                        Err(err) => {
                            publish_error(&queue, &cv, index, err.clone());
                            continue;
                        }
                    };
                    // Reserve before decoding so the bound covers work in
                    // progress, not just finished results.
                    match reserve_budget(&queue, &cv, reserve, bound) {
                        Reserve::Granted(stats_snapshot) => {
                            observed_bytes.fetch_max(stats_snapshot.0, Ordering::Relaxed);
                            observed_tasks.fetch_max(stats_snapshot.1, Ordering::Relaxed);
                            if stats_snapshot.2 {
                                oversized_ran.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Reserve::Abandoned => break,
                    }
                    let outcome = catch_unwind(AssertUnwindSafe(|| work(index, &inputs[index])));
                    let mut guard = lock(&queue);
                    match outcome {
                        Ok(Ok(value)) => {
                            let actual = measure(&value);
                            adjust_reservation(&mut guard, reserve, actual);
                            guard.slots[index] = Some(Ok(value));
                            cv.notify_all();
                        }
                        Ok(Err(err)) => {
                            release_reservation(&mut guard, reserve);
                            guard.slots[index] = Some(Err(err));
                            cv.notify_all();
                        }
                        Err(_) => {
                            release_reservation(&mut guard, reserve);
                            guard.slots[index] =
                                Some(Err(Error::corrupt("parallel bounded read panicked")));
                            cv.notify_all();
                        }
                    }
                }
                let mut guard = lock(&queue);
                guard.live_workers = guard.live_workers.saturating_sub(1);
                cv.notify_all();
            });
        }

        // Consumer: the calling thread, so a published result can always free
        // reservation while another worker waits for it.
        let mut guard = lock(&queue);
        let mut filled = 0usize;
        while filled < inputs.len() && missing.is_none() {
            let mut progressed = false;
            for index in 0..inputs.len() {
                if let Some(outcome) = guard.slots[index].take() {
                    match outcome {
                        Ok(value) => {
                            guard.inflight_bytes =
                                guard.inflight_bytes.saturating_sub(measure(&value));
                            guard.inflight_tasks = guard.inflight_tasks.saturating_sub(1);
                            collected[index] = Some(value);
                        }
                        Err(err) => {
                            // Lowest failing index wins, independent of
                            // completion order.
                            if failure
                                .as_ref()
                                .map(|(seen, _)| index < *seen)
                                .unwrap_or(true)
                            {
                                failure = Some((index, err));
                            }
                        }
                    }
                    filled += 1;
                    progressed = true;
                }
            }
            if filled == inputs.len() {
                break;
            }
            if progressed {
                cv.notify_all();
                continue;
            }
            if guard.live_workers == 0 {
                // Defensive: a worker died without publishing. Report the
                // first hole instead of waiting forever.
                missing = (0..inputs.len())
                    .find(|index| guard.slots[*index].is_none() && collected[*index].is_none());
                break;
            }
            guard = cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        guard.drained = true;
        cv.notify_all();
    });

    stats.observed_max_inflight_bytes = observed_bytes.into_inner();
    stats.observed_max_inflight_tasks = observed_tasks.into_inner();
    stats.oversized_tasks = oversized_ran.into_inner();

    if let Some(index) = missing {
        return Err(Error::corrupt(format!(
            "parallel bounded read has no result for input {index}"
        )));
    }
    if let Some((_index, err)) = failure {
        return Err(err);
    }
    let mut out = Vec::with_capacity(inputs.len());
    for (index, slot) in collected.into_iter().enumerate() {
        match slot {
            Some(value) => out.push(value),
            None => {
                return Err(Error::corrupt(format!(
                    "parallel bounded read has no result for input {index}"
                )))
            }
        }
    }
    Ok((out, stats))
}

impl Store {
    /// Decode several tensor objects with bounded concurrency.
    ///
    /// See the [module documentation](self) for the ordering, determinism,
    /// error, and memory guarantees.
    pub fn read_tensors_bounded(
        &self,
        ids: &[Hash32],
        options: &ParallelReadOptions,
    ) -> Result<Vec<Vec<u8>>> {
        self.read_tensors_bounded_with_stats(ids, options)
            .map(|(bytes, _stats)| bytes)
    }

    /// [`Store::read_tensors_bounded`] plus the observed counters.
    pub fn read_tensors_bounded_with_stats(
        &self,
        ids: &[Hash32],
        options: &ParallelReadOptions,
    ) -> Result<(Vec<Vec<u8>>, ParallelReadStats)> {
        // Declared sizes are in-memory state lookups, so the oversized count is
        // reported for the sequential path too.
        let declared: Vec<Result<usize>> =
            ids.iter().map(|id| self.declared_decode_len(*id)).collect();
        let (bytes, stats) = run_bounded(
            ids,
            &declared,
            options.threads,
            options.max_inflight_bytes,
            |_index, id| self.read_tensor(*id),
            |bytes| bytes.len(),
        )?;
        Ok((
            bytes,
            ParallelReadStats {
                requested_threads: stats.requested_threads,
                effective_threads: stats.effective_threads,
                tasks: stats.tasks,
                max_inflight_bytes: stats.max_inflight_bytes,
                observed_max_inflight_bytes: stats.observed_max_inflight_bytes,
                observed_max_inflight_tasks: stats.observed_max_inflight_tasks,
                oversized_tasks: stats.oversized_tasks,
            },
        ))
    }

    /// Bounded-parallel tensor restore after authorization.
    ///
    /// Same ordering, determinism, error, and staging guarantees as
    /// [`Store::read_tensors_bounded`]; each task enters the R8 read layers
    /// through its token, so whole-object tensors are cached/coalesced and a
    /// mode-15 object keeps its shipped tile path.
    pub fn read_tensors_bounded_authorized(
        &self,
        authorized: &[crate::store::AuthorizedRead],
        options: &ParallelReadOptions,
    ) -> Result<(Vec<Vec<u8>>, ParallelReadStats)> {
        let declared: Vec<Result<usize>> = authorized
            .iter()
            .map(|token| self.declared_decode_len(token.object_id()))
            .collect();
        let (bytes, stats) = run_bounded(
            authorized,
            &declared,
            options.threads,
            options.max_inflight_bytes,
            |_index, token| self.read_tensor_authorized(token, &TileDecodeOptions::default()),
            |bytes| bytes.len(),
        )?;
        Ok((
            bytes,
            ParallelReadStats {
                requested_threads: stats.requested_threads,
                effective_threads: stats.effective_threads,
                tasks: stats.tasks,
                max_inflight_bytes: stats.max_inflight_bytes,
                observed_max_inflight_bytes: stats.observed_max_inflight_bytes,
                observed_max_inflight_tasks: stats.observed_max_inflight_tasks,
                oversized_tasks: stats.oversized_tasks,
            },
        ))
    }

    /// Declared decoded size of one tensor object, from in-memory state only.
    fn declared_decode_len(&self, id: Hash32) -> Result<usize> {
        let record = self.object_meta(id)?;
        self.tensor_metadata(id)?;
        Ok(usize::try_from(record.size).unwrap_or(usize::MAX))
    }
}

// The whole design rests on `&Store` being usable from several threads. This
// is a compile-time proof, not an assumption: the backend is behind a `Mutex`
// and every other field is plain data or behind its own lock.
const _: fn() = || {
    fn assert_sync<T: Sync>() {}
    assert_sync::<Store>();
};

struct Queue<U> {
    /// Declared decoded bytes reserved by decoding or unconsumed tasks.
    inflight_bytes: usize,
    /// Number of tasks holding a reservation.
    inflight_tasks: usize,
    /// Published results, indexed by input position.
    slots: Vec<Option<Result<U>>>,
    /// An oversized task is waiting for the queue to drain.
    oversized_waiting: bool,
    /// Workers that have not yet exited.
    live_workers: usize,
    /// Set by the consumer once every input has been consumed.
    drained: bool,
}

enum Reserve {
    /// `(reserved bytes, reserved tasks, ran oversized)`.
    Granted((usize, usize, bool)),
    /// The consumer finished and this task is abandoned.
    Abandoned,
}

fn lock<U>(queue: &Mutex<Queue<U>>) -> MutexGuard<'_, Queue<U>> {
    queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn claim(next: &AtomicUsize, len: usize) -> Option<usize> {
    let index = next.fetch_add(1, Ordering::Relaxed);
    (index < len).then_some(index)
}

fn release_reservation<U>(queue: &mut Queue<U>, reserved: usize) {
    queue.inflight_bytes = queue.inflight_bytes.saturating_sub(reserved);
    queue.inflight_tasks = queue.inflight_tasks.saturating_sub(1);
}

/// Replace a reservation of `reserved` with the actual decoded size.
fn adjust_reservation<U>(queue: &mut Queue<U>, reserved: usize, actual: usize) {
    if actual > reserved {
        queue.inflight_bytes = queue.inflight_bytes.saturating_add(actual - reserved);
    } else {
        queue.inflight_bytes = queue.inflight_bytes.saturating_sub(reserved - actual);
    }
}

fn reserve_budget<U>(
    queue: &Mutex<Queue<U>>,
    cv: &Condvar,
    reserve: usize,
    bound: usize,
) -> Reserve {
    let mut guard = lock(queue);
    let oversized = reserve > bound;
    if oversized {
        guard.oversized_waiting = true;
        while guard.inflight_bytes != 0 {
            if guard.drained {
                guard.oversized_waiting = false;
                return Reserve::Abandoned;
            }
            guard = cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        guard.oversized_waiting = false;
    } else {
        while guard.inflight_bytes.saturating_add(reserve) > bound || guard.oversized_waiting {
            if guard.drained {
                return Reserve::Abandoned;
            }
            guard = cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
    guard.inflight_bytes = guard.inflight_bytes.saturating_add(reserve);
    guard.inflight_tasks += 1;
    Reserve::Granted((guard.inflight_bytes, guard.inflight_tasks, oversized))
}

fn publish_error<U>(queue: &Mutex<Queue<U>>, cv: &Condvar, index: usize, err: Error) {
    let mut guard = lock(queue);
    guard.slots[index] = Some(Err(err));
    cv.notify_all();
}
