//! Ordered producer/consumer restore.
//!
//! The shipped parallel read ([`crate::parallel::ParallelReadOptions`]) decodes
//! several tensors at once, but it returns a fully materialised `Vec<Vec<u8>>`
//! and only then can the caller write the reassembled container. For a 988 MB
//! safetensors file that means the decode and the sequential write never
//! overlap, so `materialize` stays writeback bound.
//!
//! [`run_ordered_pipeline`] overlaps them. Workers decode tensors with the
//! same bounded-worker / bounded-staging policy as
//! [`crate::parallel::Store::read_tensors_bounded`], while one writer (the
//! calling thread) streams each finished tensor to a `Write` sink **in exact
//! input order**. There is no whole-file buffer: a worker holds one decoded
//! tensor, the queue holds only results that have not reached the writer yet,
//! and the writer holds the tensor it is currently writing.
//!
//! Guarantees:
//!
//! * **Exact order.** The sink receives `0, 1, 2, ...` input indices, whatever
//!   the thread count and whatever order decodes complete in.
//! * **Determinism.** A decode is a read-only function of the store state and
//!   one object id, so bytes and errors are functions of the input list alone.
//!   Both the written prefix and the reported error are deterministic.
//! * **One error policy.** The first failure encountered *is* the lowest
//!   failing input index, because the writer consumes strictly in order: every
//!   lower index has already been written successfully. On a failure the
//!   pipeline stops claiming new work, still drains every in-flight decode,
//!   frees every reservation, joins every worker, and returns that error. The
//!   sink therefore holds exactly the prefix `0..index`.
//! * **Memory.** Before decoding, a worker reserves its task's declared size,
//!   and reservations are granted in input order. The sum of non-oversized
//!   reservations (decoding + published-but-unwritten + the tensor being
//!   written) never exceeds `max_inflight_bytes`. A task whose declared size
//!   alone exceeds the bound is tracked separately and at most one is
//!   outstanding, so the peak is at most
//!   `max_inflight_bytes + largest oversized tensor`. The reservation is the
//!   declared logical size, so it bounds staging and queue depth, not a
//!   decoder's transient allocator use.
//! * **One worker means the old path.** A request that resolves to one
//!   effective worker decodes and writes one tensor at a time, in order, with
//!   exactly one tensor staged.
//! * **Read-only in the store.** The pipeline never mutates the store: it only
//!   calls the shipped tensor read path. The `Write` sink is the caller's
//!   destination, not a store namespace.
//!
//! The sink is usually an `std::fs::File`. It may be any `Write`
//! implementation, which is what the ordering, error, and staging tests use.

use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Instant;

use crate::error::{Error, Result};
use crate::parallel::{effective_threads, TileDecodeOptions, DEFAULT_MAX_INFLIGHT_BYTES};
use crate::store::{AuthorizedRead, Store};

/// Worker bound and staging bound for an ordered restore pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestorePipelineOptions {
    /// Requested worker count. `0` means one worker; the effective count is
    /// also capped at [`std::thread::available_parallelism`].
    pub threads: usize,
    /// Soft bound on the declared decoded bytes reserved at any time by
    /// workers that are decoding, whose result has not been written yet, or
    /// that the writer is currently writing.
    pub max_inflight_bytes: usize,
    /// Workers for one tensor's independently decodable tiles (mode 15/17).
    /// `1` is the shipped sequential read; this is separate from `threads`,
    /// which parallelises across tensors.
    pub tile_threads: usize,
}

impl Default for RestorePipelineOptions {
    fn default() -> Self {
        RestorePipelineOptions {
            threads: 1,
            max_inflight_bytes: DEFAULT_MAX_INFLIGHT_BYTES,
            tile_threads: 1,
        }
    }
}

/// What one ordered restore pipeline actually did, for tests and receipts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RestorePipelineStats {
    /// The requested thread count, verbatim.
    pub requested_threads: usize,
    /// The worker count after the `0 -> 1` and `available_parallelism` rules.
    pub effective_threads: usize,
    /// Number of inputs.
    pub tasks: usize,
    /// Tensors handed to the sink.
    pub tensors_written: usize,
    /// Bytes handed to the sink (`write_all` returned `Ok`).
    pub bytes_written: u64,
    /// Tensors decoded after a failure and deliberately not written.
    pub tensors_dropped: usize,
    /// Decoded bytes deliberately not written after a failure.
    pub bytes_dropped: u64,
    /// The configured `max_inflight_bytes`.
    pub max_inflight_bytes: usize,
    /// The largest observed sum of reserved decoded bytes.
    pub observed_max_inflight_bytes: usize,
    /// The largest observed number of reserved tasks (decoding, waiting for the
    /// writer, or being written).
    pub observed_max_inflight_tasks: usize,
    /// Tasks whose declared size alone exceeded `max_inflight_bytes` and that
    /// therefore ran alone.
    pub oversized_tasks: usize,
    /// Wall time from pipeline start until the last decode finished.
    pub decode_wall_ms: f64,
    /// Sum of every decode duration (worker busy time, so it can exceed the
    /// wall time when several workers decode concurrently).
    pub decode_sum_ms: f64,
    /// Time the writer spent blocked waiting for the next in-order result.
    pub transfer_ms: f64,
    /// Time the writer spent inside the sink (`write_all`).
    pub write_ms: f64,
    /// Total pipeline wall time, including the final drain/join.
    pub total_ms: f64,
}

/// Run `work` over `inputs` with a bounded worker pool and write results to
/// `consume` in exact input order.
///
/// `sizes[index]` is the declared size reserved for `inputs[index]` before its
/// work starts; `measure(&value)` is the actual size released once the value is
/// written or dropped. See the [module documentation](self) for the ordering,
/// determinism, error, and memory guarantees.
///
/// This is the general form of the restore pipeline; the store methods
/// [`Store::restore_tensors_pipelined`] and
/// [`Store::restore_tensors_pipelined_authorized`] bind it to the shipped
/// tensor read path.
pub fn run_ordered_pipeline<T, U, F, M, C>(
    inputs: &[T],
    sizes: &[Result<usize>],
    options: &RestorePipelineOptions,
    work: F,
    measure: M,
    consume: C,
) -> Result<RestorePipelineStats>
where
    T: Sync,
    U: Send,
    F: Fn(usize, &T) -> Result<U> + Sync,
    M: Fn(&U) -> usize + Sync,
    C: FnMut(usize, &U) -> Result<()>,
{
    assert_eq!(inputs.len(), sizes.len(), "one declared size per input");
    let effective = effective_threads(options.threads);
    let mut stats = RestorePipelineStats {
        requested_threads: options.threads,
        effective_threads: effective,
        tasks: inputs.len(),
        max_inflight_bytes: options.max_inflight_bytes,
        ..RestorePipelineStats::default()
    };
    if inputs.is_empty() {
        return Ok(stats);
    }

    if effective <= 1 {
        // The shipped one-at-a-time path: one tensor staged, exact order.
        let mut consume = consume;
        let started = Instant::now();
        let mut decode_ns = 0u64;
        let mut write_ns = 0u64;
        for (index, input) in inputs.iter().enumerate() {
            match &sizes[index] {
                Ok(len) => {
                    if *len > options.max_inflight_bytes {
                        stats.oversized_tasks += 1;
                    }
                }
                Err(err) => return Err(err.clone()),
            }
            let decode_started = Instant::now();
            let value = work(index, input)?;
            decode_ns += decode_started.elapsed().as_nanos() as u64;
            let actual = measure(&value);
            stats.tensors_written += 1;
            stats.observed_max_inflight_tasks = stats.observed_max_inflight_tasks.max(1);
            stats.observed_max_inflight_bytes = stats.observed_max_inflight_bytes.max(actual);
            let write_started = Instant::now();
            consume(index, &value)?;
            write_ns += write_started.elapsed().as_nanos() as u64;
            stats.bytes_written += actual as u64;
        }
        stats.decode_wall_ms = decode_ns as f64 / 1_000_000.0;
        stats.decode_sum_ms = stats.decode_wall_ms;
        stats.write_ms = write_ns as f64 / 1_000_000.0;
        stats.total_ms = started.elapsed().as_secs_f64() * 1000.0;
        return Ok(stats);
    }

    let start = Instant::now();
    let queue = Mutex::new(Queue {
        inflight_bytes: 0,
        normal_bytes: 0,
        inflight_tasks: 0,
        next_to_reserve: 0,
        slots: (0..inputs.len()).map(|_| None).collect(),
        oversized_at: None,
        live_workers: effective,
        aborted: false,
    });
    let cv = Condvar::new();
    let next = AtomicUsize::new(0);
    let observed_bytes = AtomicUsize::new(0);
    let observed_tasks = AtomicUsize::new(0);
    let oversized_ran = AtomicUsize::new(0);
    let decode_sum_ns = AtomicU64::new(0);
    let last_decode_finish_ns = AtomicU64::new(0);

    let mut consume = consume;
    let mut written_bytes = 0u64;
    let mut tensors_written = 0usize;
    let mut dropped_tensors = 0usize;
    let mut dropped_bytes = 0u64;
    let mut transfer_ns = 0u64;
    let mut write_ns = 0u64;
    let mut failure: Option<Error> = None;
    let mut cursor = 0usize;

    std::thread::scope(|scope| {
        for _ in 0..effective {
            scope.spawn(|| {
                loop {
                    {
                        let guard = lock(&queue);
                        if guard.aborted {
                            break;
                        }
                    }
                    let Some(index) = claim(&next, inputs.len()) else {
                        break;
                    };
                    // A declared-size error still takes its reservation turn:
                    // reservation is in index order, so skipping a turn would
                    // stall every later worker.
                    let reserve = match &sizes[index] {
                        Ok(len) => *len,
                        Err(_) => 0,
                    };
                    let granted = match reserve_budget(
                        &queue,
                        &cv,
                        index,
                        reserve,
                        options.max_inflight_bytes,
                    ) {
                        Reserve::Granted(granted) => granted,
                        Reserve::Abandoned => break,
                    };
                    if let Err(err) = &sizes[index] {
                        let mut guard = lock(&queue);
                        release_held(&mut guard, granted.oversized, reserve);
                        guard.slots[index] = Some(Pending {
                            oversized: false,
                            held: false,
                            size: 0,
                            outcome: Err(err.clone()),
                        });
                        cv.notify_all();
                        continue;
                    }
                    observed_bytes.fetch_max(granted.bytes, Ordering::Relaxed);
                    observed_tasks.fetch_max(granted.tasks, Ordering::Relaxed);
                    if granted.oversized {
                        oversized_ran.fetch_add(1, Ordering::Relaxed);
                    }

                    let decode_started = Instant::now();
                    let outcome = catch_unwind(AssertUnwindSafe(|| work(index, &inputs[index])));
                    let decode_ns = decode_started.elapsed().as_nanos() as u64;
                    decode_sum_ns.fetch_add(decode_ns, Ordering::Relaxed);
                    last_decode_finish_ns
                        .fetch_max(start.elapsed().as_nanos() as u64, Ordering::Relaxed);

                    let mut guard = lock(&queue);
                    match outcome {
                        Ok(Ok(value)) => {
                            let actual = match catch_unwind(AssertUnwindSafe(|| measure(&value))) {
                                Ok(actual) => actual,
                                Err(_) => {
                                    release_held(&mut guard, granted.oversized, reserve);
                                    guard.slots[index] = Some(Pending {
                                        oversized: granted.oversized,
                                        held: false,
                                        size: 0,
                                        outcome: Err(Error::corrupt(
                                            "ordered restore size measurement panicked",
                                        )),
                                    });
                                    cv.notify_all();
                                    continue;
                                }
                            };
                            adjust_held(&mut guard, granted.oversized, reserve, actual);
                            observed_bytes.fetch_max(guard.inflight_bytes, Ordering::Relaxed);
                            observed_tasks.fetch_max(guard.inflight_tasks, Ordering::Relaxed);
                            guard.slots[index] = Some(Pending {
                                oversized: granted.oversized,
                                held: true,
                                size: actual,
                                outcome: Ok(value),
                            });
                            cv.notify_all();
                        }
                        Ok(Err(err)) => {
                            release_held(&mut guard, granted.oversized, reserve);
                            guard.slots[index] = Some(Pending {
                                oversized: granted.oversized,
                                held: false,
                                size: 0,
                                outcome: Err(err),
                            });
                            cv.notify_all();
                        }
                        Err(_) => {
                            release_held(&mut guard, granted.oversized, reserve);
                            guard.slots[index] = Some(Pending {
                                oversized: granted.oversized,
                                held: false,
                                size: 0,
                                outcome: Err(Error::corrupt("ordered restore decode panicked")),
                            });
                            cv.notify_all();
                        }
                    }
                }
                let mut guard = lock(&queue);
                guard.live_workers = guard.live_workers.saturating_sub(1);
                cv.notify_all();
            });
        }

        // The writer: the calling thread, so in-order consumption always frees
        // reservation while another worker waits for it.
        let mut guard = lock(&queue);
        loop {
            if failure.is_none() && cursor < inputs.len() {
                if let Some(pending) = guard.slots[cursor].take() {
                    let Pending {
                        oversized,
                        held,
                        size,
                        outcome,
                    } = pending;
                    match outcome {
                        Ok(value) => {
                            // Write outside the lock. The reservation stays
                            // counted until the write completes, so the staging
                            // bound covers the tensor being written too.
                            drop(guard);
                            let write_started = Instant::now();
                            let outcome =
                                catch_unwind(AssertUnwindSafe(|| consume(cursor, &value)));
                            write_ns += write_started.elapsed().as_nanos() as u64;
                            guard = lock(&queue);
                            release_pending(&mut guard, oversized, held, size);
                            cv.notify_all();
                            match outcome {
                                Ok(Ok(())) => {
                                    written_bytes += size as u64;
                                    tensors_written += 1;
                                    cursor += 1;
                                }
                                Ok(Err(err)) => {
                                    failure = Some(err);
                                    guard.aborted = true;
                                    cv.notify_all();
                                }
                                Err(_) => {
                                    failure =
                                        Some(Error::corrupt("ordered restore writer panicked"));
                                    guard.aborted = true;
                                    cv.notify_all();
                                }
                            }
                            continue;
                        }
                        Err(err) => {
                            release_pending(&mut guard, oversized, held, size);
                            failure = Some(err);
                            guard.aborted = true;
                            cv.notify_all();
                            continue;
                        }
                    }
                }
            }

            if failure.is_some() || cursor == inputs.len() {
                let mut freed = false;
                for index in 0..guard.slots.len() {
                    if let Some(pending) = guard.slots[index].take() {
                        release_pending(&mut guard, pending.oversized, pending.held, pending.size);
                        if pending.outcome.is_ok() {
                            dropped_tensors += 1;
                            dropped_bytes += pending.size as u64;
                        }
                        freed = true;
                    }
                }
                if freed {
                    cv.notify_all();
                }
                if cursor == inputs.len() && failure.is_none() {
                    break;
                }
                if failure.is_some() && guard.live_workers == 0 {
                    break;
                }
            }

            if guard.live_workers == 0 {
                if failure.is_none() {
                    failure = Some(Error::corrupt(format!(
                        "ordered restore has no result for input {cursor}"
                    )));
                    guard.aborted = true;
                    cv.notify_all();
                    continue;
                }
                break;
            }

            let wait_started = Instant::now();
            guard = cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            transfer_ns += wait_started.elapsed().as_nanos() as u64;
        }
        drop(guard);
    });

    stats.observed_max_inflight_bytes = observed_bytes.into_inner();
    stats.observed_max_inflight_tasks = observed_tasks.into_inner();
    stats.oversized_tasks = oversized_ran.into_inner();
    stats.decode_wall_ms = last_decode_finish_ns.into_inner() as f64 / 1_000_000.0;
    stats.decode_sum_ms = decode_sum_ns.into_inner() as f64 / 1_000_000.0;
    stats.transfer_ms = transfer_ns as f64 / 1_000_000.0;
    stats.write_ms = write_ns as f64 / 1_000_000.0;
    stats.total_ms = start.elapsed().as_secs_f64() * 1000.0;
    stats.tensors_written = tensors_written;
    stats.bytes_written = written_bytes;
    stats.tensors_dropped = dropped_tensors;
    stats.bytes_dropped = dropped_bytes;

    if let Some(err) = failure {
        return Err(err);
    }
    if cursor != inputs.len() {
        return Err(Error::corrupt(format!(
            "ordered restore wrote {} of {} inputs",
            cursor,
            inputs.len()
        )));
    }
    Ok(stats)
}

impl Store {
    /// Decode tensor objects with bounded concurrency and stream them to
    /// `sink` in exact input order.
    ///
    /// See the [module documentation](self) for the ordering, determinism,
    /// error, and memory guarantees. The store is only read.
    pub fn restore_tensors_pipelined<W: Write>(
        &self,
        ids: &[crate::identity::Hash32],
        sink: &mut W,
        options: &RestorePipelineOptions,
    ) -> Result<RestorePipelineStats> {
        let declared: Vec<Result<usize>> = ids
            .iter()
            .map(|id| self.declared_restore_len(*id))
            .collect();
        run_ordered_pipeline(
            ids,
            &declared,
            options,
            |_index, id| self.read_tensor(*id),
            |bytes| bytes.len(),
            |_index, bytes| write_sink(sink, bytes),
        )
    }

    /// Bounded ordered restore after authorization.
    ///
    /// Each task enters the R8 read layers through its token, exactly like
    /// [`Store::read_tensors_bounded_authorized`], and the bytes are streamed
    /// to `sink` in input order.
    pub fn restore_tensors_pipelined_authorized<W: Write>(
        &self,
        authorized: &[AuthorizedRead],
        sink: &mut W,
        options: &RestorePipelineOptions,
    ) -> Result<RestorePipelineStats> {
        let declared: Vec<Result<usize>> = authorized
            .iter()
            .map(|token| self.declared_restore_len(token.object_id()))
            .collect();
        run_ordered_pipeline(
            authorized,
            &declared,
            options,
            |_index, token| {
                self.read_tensor_authorized(
                    token,
                    &TileDecodeOptions {
                        threads: options.tile_threads,
                        ..TileDecodeOptions::default()
                    },
                )
            },
            |bytes| bytes.len(),
            |_index, bytes| write_sink(sink, bytes),
        )
    }

    /// Declared decoded size of one tensor object, from in-memory state only.
    fn declared_restore_len(&self, id: crate::identity::Hash32) -> Result<usize> {
        let record = self.object_meta(id)?;
        self.tensor_metadata(id)?;
        Ok(usize::try_from(record.size).unwrap_or(usize::MAX))
    }
}

fn write_sink<W: Write>(sink: &mut W, bytes: &[u8]) -> Result<()> {
    sink.write_all(bytes)
        .map_err(|err| Error::io(format!("ordered restore write: {err}")))
}

// ---------------------------------------------------------------------------
// queue
// ---------------------------------------------------------------------------

struct Pending<U> {
    /// This task's reservation was oversized for the staging bound.
    oversized: bool,
    /// The reservation is still counted at `size` bytes.
    held: bool,
    /// Currently reserved bytes (0 once an error released them).
    size: usize,
    outcome: Result<U>,
}

struct Queue<U> {
    /// Every reserved byte (decoding, unwritten, or being written).
    inflight_bytes: usize,
    /// The non-oversized part of `inflight_bytes`; this is what the staging
    /// bound limits. Oversized reservations are tracked separately so their
    /// size cannot block a task the writer is about to reach.
    normal_bytes: usize,
    /// Number of tasks holding a reservation.
    inflight_tasks: usize,
    /// Reservation happens in input order. `next_to_reserve` is the index that
    /// may reserve now, which guarantees every task before the writer cursor
    /// always gets its turn (the in-order consumer cannot deadlock against
    /// published-but-unwritten results held by later tasks).
    next_to_reserve: usize,
    /// Published results, indexed by input position.
    slots: Vec<Option<Pending<U>>>,
    /// One oversized reservation is decoding or waiting to be written.
    oversized_at: Option<usize>,
    /// Workers that have not yet exited.
    live_workers: usize,
    /// Set by the writer after the first failure.
    aborted: bool,
}

struct Granted {
    /// `(reserved bytes, reserved tasks)` at grant time.
    bytes: usize,
    tasks: usize,
    oversized: bool,
}

enum Reserve {
    Granted(Granted),
    /// The writer aborted and this task is abandoned.
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

fn release_held<U>(queue: &mut Queue<U>, oversized: bool, reserved: usize) {
    queue.inflight_bytes = queue.inflight_bytes.saturating_sub(reserved);
    if !oversized {
        queue.normal_bytes = queue.normal_bytes.saturating_sub(reserved);
    }
    queue.inflight_tasks = queue.inflight_tasks.saturating_sub(1);
}

/// Replace a reservation of `reserved` with the actual decoded size.
fn adjust_held<U>(queue: &mut Queue<U>, oversized: bool, reserved: usize, actual: usize) {
    if actual > reserved {
        let delta = actual - reserved;
        queue.inflight_bytes = queue.inflight_bytes.saturating_add(delta);
        if !oversized {
            queue.normal_bytes = queue.normal_bytes.saturating_add(delta);
        }
    } else {
        let delta = reserved - actual;
        queue.inflight_bytes = queue.inflight_bytes.saturating_sub(delta);
        if !oversized {
            queue.normal_bytes = queue.normal_bytes.saturating_sub(delta);
        }
    }
}

/// Release everything a consumed/dropped pending result still holds.
fn release_pending<U>(queue: &mut Queue<U>, oversized: bool, held: bool, size: usize) {
    if held {
        release_held(queue, oversized, size);
    }
    if oversized {
        queue.oversized_at = None;
    }
}

/// Wait for this task's reservation turn and budget, then reserve `reserve`
/// bytes.
///
/// Reservations are granted strictly in input order, and the normal budget
/// counts only non-oversized tasks. An oversized task takes no part in
/// `normal_bytes`; at most one oversized reservation is outstanding, so the
/// observed peak is at most `normal_bytes (<= bound) + one oversized`.
fn reserve_budget<U>(
    queue: &Mutex<Queue<U>>,
    cv: &Condvar,
    index: usize,
    reserve: usize,
    bound: usize,
) -> Reserve {
    let mut guard = lock(queue);
    let oversized = reserve > bound;
    loop {
        if guard.aborted {
            return Reserve::Abandoned;
        }
        let turn = guard.next_to_reserve == index;
        let budget = oversized || guard.normal_bytes.saturating_add(reserve) <= bound;
        let oversized_free = !oversized || guard.oversized_at.is_none();
        if turn && budget && oversized_free {
            break;
        }
        guard = cv
            .wait(guard)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    guard.next_to_reserve = index.saturating_add(1);
    guard.inflight_bytes = guard.inflight_bytes.saturating_add(reserve);
    if oversized {
        guard.oversized_at = Some(index);
    } else {
        guard.normal_bytes = guard.normal_bytes.saturating_add(reserve);
    }
    guard.inflight_tasks += 1;
    Reserve::Granted(Granted {
        bytes: guard.inflight_bytes,
        tasks: guard.inflight_tasks,
        oversized,
    })
}

// The whole design rests on `&Store` being usable from several threads. This
// is a compile-time proof, not an assumption.
const _: fn() = || {
    fn assert_sync<T: Sync>() {}
    assert_sync::<Store>();
};
