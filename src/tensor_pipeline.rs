//! Bounded parallel encode-and-publish pipeline (subphase R21).
//!
//! The sequential planned writer charges one tensor at a time: read, analyse,
//! encode every candidate, hash, publish the blobs, then move to the next
//! tensor. On the R20 fixture that is 1,236.8 ms of which 704.6 ms is CPU
//! encoding and 272.2 ms is blob publication, and the two never overlap.
//!
//! This module turns the loop into a three-stage pipeline:
//!
//! ```text
//!   reader ──► staging queue ──► encode workers ──► publish queue ──► publisher
//!  (one, I/O)   (staging_bytes)    (N, CPU + hash)   (encode_bytes)   (one, I/O)
//! ```
//!
//! * The **reader** owns the source bytes. It admits one tensor at a time
//!   against `staging_bytes`, so the decoded working set is bounded no matter
//!   how large the container is.
//! * **Encode workers** run the planner and the candidate encoders
//!   ([`Store::encode_tensor_object`]) and publish nothing. Each holds at most
//!   one tensor's decoded bytes and at most two candidate frames (see
//!   [`WORKER_TENSOR_MULTIPLIER`]), so a worker's reservation is a function of
//!   the tensor's length and not of the thread count.
//! * The **publisher** is the only thread that touches the backend. It
//!   publishes each finished fragment ([`Store::publish_encoded_tensor`]) and,
//!   every `publish_sync_bytes`, closes the deferred durability batch and opens
//!   a new one, so writeback happens *while* the remaining tensors encode
//!   instead of as one 97 ms barrier after the last one.
//!
//! # What this module cannot change
//!
//! Every blob is content-addressed and create-once, every ObjectId seed comes
//! from the caller, and the committer installs the fragments in input order.
//! The committed state is therefore a pure function of the inputs and is
//! **identical for every worker count** (`tests/tensor_pipeline.rs` proves it
//! across 1/2/4/8 workers at the head, manifest, root and frame level).
//! Control-state publication (the commit record, the head, the index segment)
//! stays on the caller's single writer; this module only ever publishes
//! immutable blobs.
//!
//! # Scheduling policy
//!
//! * Work is handed out **FIFO**, so a single large tensor cannot be starved by
//!   a stream of small ones: it is claimed before any tensor behind it.
//! * A worker that claims a tensor at or above
//!   `candidate_parallel_min_bytes` requires **two** CPU tokens (it will run the
//!   level-3 and mode-9 candidates concurrently); a smaller tensor requires one.
//!   Two tokens are granted only when two are free, so two workers can never
//!   deadlock each holding one, and the number of busy encoder threads can
//!   never exceed `cpu_tokens`.
//! * Byte budgets never block forever: a tensor larger than the whole staging
//!   budget, or a fragment larger than the whole publish budget, is admitted
//!   alone (the same oversize rule the shipped `run_bounded` reader uses).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Instant;

use crate::error::{Error, Result};
use crate::store::Store;
use crate::tensor_store::{
    EncodedTensor, PreparedTensorObject, TensorLayoutSpec, TensorPlan, TensorWriteSpec,
};

/// Tensors at or above this length encode their level-3 and mode-9 candidates
/// on two threads when the pipeline allows it. Below it the spawn costs more
/// than the overlap: the measured level-3 encode of an 8.7 MB tensor is 12 ms.
pub const DEFAULT_CANDIDATE_PARALLEL_MIN_BYTES: u64 = 16 * 1024 * 1024;

/// Default bound on decoded tensor bytes queued for an encoder.
pub const DEFAULT_STAGING_BYTES: u64 = 256 * 1024 * 1024;

/// Default bound on encoded fragments queued for the publisher.
pub const DEFAULT_ENCODE_BYTES: u64 = 256 * 1024 * 1024;

/// Default bound on the bytes the publisher is writing at once.
pub const DEFAULT_PUBLISH_BYTES: u64 = 128 * 1024 * 1024;

/// Default bound on the bytes all encode workers hold at once.
pub const DEFAULT_WORKER_BYTES: u64 = 1024 * 1024 * 1024;

/// Publish this many bytes, then hand the batch's files to the early-barrier
/// thread. Measured on the 4-CPU court (0.5B fixture, 282 MB): 8 MiB 651 ms,
/// 16 MiB 648 ms, 64 MiB 713 ms, none 899 ms — the last number is the whole
/// group's writeback landing on the commit's barrier.
pub const DEFAULT_PUBLISH_SYNC_BYTES: u64 = 8 * 1024 * 1024;

/// Bytes a worker may hold while it encodes one tensor of `n` bytes, divided by
/// `n` — the proof behind the worker reservation.
///
/// One tensor encode holds: the canonical decoded bytes (`n`); the frame of the
/// candidate being built (`<= n` plus a codec header); and the single frame
/// [`crate::tensor_store`] retains for the running best candidate
/// (`retain_candidate` drops every other artifact, and drops same-strategy
/// duplicates such as level 3 / level 19). That is `3n`, rounded up with a
/// fixed slack for codec headers and the manifest encoding.
pub const WORKER_TENSOR_MULTIPLIER: u64 = 3;

/// Fixed slack added to every worker reservation (manifest WCB, headers).
pub const WORKER_RESERVATION_SLACK_BYTES: u64 = 1024 * 1024;

/// One tensor the pipeline encodes, described by the caller.
#[derive(Clone, Debug)]
pub struct TensorPipelineTensor<'a> {
    /// File name the committer will create.
    pub name: &'a str,
    /// Element dtype spelling (`bf16`, `f32`, ...).
    pub dtype: &'a str,
    /// Logical shape.
    pub shape: &'a [u64],
    /// Deterministic ObjectId seed for this tensor.
    pub seed: [u8; 32],
    /// Canonical byte length the reader must produce.
    pub byte_len: u64,
}

/// Byte and thread budgets of the pipeline.
#[derive(Clone, Debug)]
pub struct TensorPipelineOptions {
    /// Encode worker threads. Defaults to `min(available_parallelism, 4)`.
    pub encode_threads: usize,
    /// CPU tokens: the hard bound on concurrently *busy* encoder threads,
    /// counting the second thread a large tensor may use. Defaults to
    /// [`default_cpu_tokens`] of the encode thread count, which leaves one CPU
    /// for the reader, the publisher and the writeback thread.
    pub cpu_tokens: usize,
    /// Decoded bytes queued ahead of the encoders.
    pub staging_bytes: u64,
    /// Encoded fragment bytes queued ahead of the publisher.
    pub encode_bytes: u64,
    /// Bytes the publisher holds while writing one fragment.
    pub publish_bytes: u64,
    /// Bytes all encode workers hold at once.
    pub worker_bytes: u64,
    /// Tensor length at which a worker runs the level-3 and mode-9 candidates
    /// concurrently. `0` disables candidate parallelism.
    pub candidate_parallel_min_bytes: u64,
    /// Publish this many bytes, then start background writeback for the blobs
    /// published so far and keep going. `0` never kicks (the R20 behaviour: a
    /// quiet disk writes the batch back on its own).
    ///
    /// A kick is **not** a barrier: it waits for nothing, claims nothing and
    /// leaves every file in the open deferred batch, so the commit's own
    /// barrier still `fsync`s every file and directory before the commit
    /// record. It only stops the whole group's writeback from landing on the
    /// critical path of the last barrier.
    pub publish_sync_bytes: u64,
}

/// The measured default token count for `encode_threads` (R22.3).
///
/// One token per encode thread starves the single publisher: on the 4-CPU court
/// `cpu_tokens = 3` measured 593.2 ms (least-contended) and 597.9 ms (median) of
/// seven interleaved repetitions against 619.4 / 659.4 at `cpu_tokens = 4`, in
/// two independent sessions (R21 measured the same 4-5 % gap). Two tokens is
/// publisher-bound at 967.6 ms. The rule is therefore "one fewer busy encoder
/// thread than CPUs", with a floor of one so a single-CPU host still encodes.
pub fn default_cpu_tokens(encode_threads: usize) -> usize {
    encode_threads.saturating_sub(1).max(1)
}

impl Default for TensorPipelineOptions {
    fn default() -> Self {
        let threads = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
            .clamp(1, 4);
        TensorPipelineOptions {
            encode_threads: threads,
            cpu_tokens: default_cpu_tokens(threads),
            staging_bytes: DEFAULT_STAGING_BYTES,
            encode_bytes: DEFAULT_ENCODE_BYTES,
            publish_bytes: DEFAULT_PUBLISH_BYTES,
            worker_bytes: DEFAULT_WORKER_BYTES,
            candidate_parallel_min_bytes: DEFAULT_CANDIDATE_PARALLEL_MIN_BYTES,
            publish_sync_bytes: DEFAULT_PUBLISH_SYNC_BYTES,
        }
    }
}

impl TensorPipelineOptions {
    /// Reject a configuration that could not bound memory or would not
    /// terminate.
    pub fn validate(&self) -> Result<()> {
        if self.encode_threads == 0 {
            return Err(Error::invalid_argument(
                "pipeline needs at least one encode thread",
            ));
        }
        if self.encode_threads > 64 {
            return Err(Error::limit_exceeded(
                "pipeline encode thread count is above the 64-thread bound",
            ));
        }
        if self.cpu_tokens == 0 || self.cpu_tokens > self.encode_threads.saturating_mul(2) {
            return Err(Error::invalid_argument(
                "pipeline cpu_tokens must be between 1 and twice the encode thread count",
            ));
        }
        for (name, value) in [
            ("staging_bytes", self.staging_bytes),
            ("encode_bytes", self.encode_bytes),
            ("publish_bytes", self.publish_bytes),
            ("worker_bytes", self.worker_bytes),
        ] {
            if value == 0 {
                return Err(Error::invalid_argument(format!(
                    "pipeline {name} must be positive (a zero bound is unbounded memory)"
                )));
            }
        }
        Ok(())
    }

    /// The worker-share budget a tensor of `byte_len` reserves for the whole
    /// time it is being encoded.
    pub fn worker_reservation(&self, byte_len: u64) -> u64 {
        byte_len
            .saturating_mul(WORKER_TENSOR_MULTIPLIER)
            .saturating_add(WORKER_RESERVATION_SLACK_BYTES)
    }

    /// Upper bound on pipeline-owned resident bytes, given the largest tensor
    /// the caller will submit. This is the arithmetic the results document
    /// reports next to the measured peak RSS.
    pub fn reservation_bound_bytes(&self, largest_tensor: u64) -> u64 {
        self.staging_bytes
            .saturating_add(self.encode_bytes)
            .saturating_add(self.publish_bytes)
            // Workers share one budget; a single oversized tensor may take all
            // of it, so the contribution is the budget, not workers x largest.
            .saturating_add(
                self.worker_bytes
                    .max(self.worker_reservation(largest_tensor)),
            )
    }
}

/// One stage interval, in nanoseconds since the pipeline started.
#[derive(Clone, Copy, Debug, Default)]
pub struct StageInterval {
    pub start_ns: u64,
    pub end_ns: u64,
}

/// Measured pipeline behaviour. Every field is a measurement or a configured
/// bound; nothing here is estimated.
#[derive(Clone, Debug, Default)]
pub struct TensorPipelineStats {
    pub tensors: usize,
    pub encode_threads: usize,
    pub cpu_tokens: usize,
    // ---- configured budgets ----
    pub staging_bytes: u64,
    pub encode_bytes: u64,
    pub publish_bytes: u64,
    pub worker_bytes: u64,
    pub publish_sync_bytes: u64,
    pub candidate_parallel_min_bytes: u64,
    /// `reservation_bound_bytes(largest tensor)`.
    pub reservation_bound_bytes: u64,
    // ---- observed reservations ----
    pub observed_max_staging_bytes: u64,
    pub observed_max_encode_bytes: u64,
    pub observed_max_publish_bytes: u64,
    pub observed_max_worker_bytes: u64,
    pub observed_max_resident_tensors: usize,
    pub oversize_staging: u64,
    pub oversize_publish: u64,
    pub candidate_parallel_tensors: u64,
    // ---- stage walls (milliseconds) ----
    pub reader_ms: f64,
    pub encode_busy_ms: f64,
    pub publish_busy_ms: f64,
    pub encode_wall_ms: f64,
    pub publish_wall_ms: f64,
    pub pipeline_wall_ms: f64,
    /// Encode-thread time during which a publication was in progress.
    pub encode_overlapped_ms: f64,
    /// Publication time during which at least one encoder was running.
    pub publish_overlapped_ms: f64,
    // ---- waits, i.e. bounds that actually bound ----
    pub staging_wait_ms: f64,
    pub worker_bytes_wait_ms: f64,
    pub encode_queue_wait_ms: f64,
    pub publish_idle_ms: f64,
    pub cpu_token_wait_ms: f64,
    // ---- early writeback (durability) ----
    /// Early-barrier passes the writeback thread completed.
    pub writeback_passes: u64,
    /// Files the early barrier `fsync`ed out of band.
    pub writeback_files: u64,
    /// Total writeback-thread busy time across passes (overlapped with encode
    /// and publication, so it is not a wall).
    pub writeback_ms: f64,
    /// Encode and publication intervals, for a reader that wants to plot them.
    pub encode_intervals: Vec<StageInterval>,
    pub publish_intervals: Vec<StageInterval>,
}

impl TensorPipelineStats {
    /// Fraction of encoder-thread time that overlapped publication.
    pub fn encode_overlap_fraction(&self) -> f64 {
        if self.encode_busy_ms <= 0.0 {
            0.0
        } else {
            self.encode_overlapped_ms / self.encode_busy_ms
        }
    }

    /// Fraction of publication time that was hidden under encoding.
    pub fn publish_hidden_fraction(&self) -> f64 {
        if self.publish_busy_ms <= 0.0 {
            0.0
        } else {
            self.publish_overlapped_ms / self.publish_busy_ms
        }
    }
}

/// One prepared tensor, in the caller's input order.
#[derive(Clone, Debug)]
pub struct PipelineTensorResult {
    pub prepared: PreparedTensorObject,
    pub plan: TensorPlan,
    pub encode_ms: f64,
    pub publish_ms: f64,
}

/// One result per input tensor (input order) plus the measured behaviour.
pub struct TensorPipelineOutcome {
    pub results: Vec<PipelineTensorResult>,
    pub stats: TensorPipelineStats,
}

impl Store {
    /// Encode and publish a group of tensor objects through the bounded
    /// pipeline, returning prepared fragments **in input order**.
    ///
    /// Nothing is committed: the caller installs the fragments with
    /// [`Store::commit_prepared_tensor_object`] (normally inside an open group
    /// so the whole group is one commit). `read(index, buf)` fills a
    /// pre-sized buffer with tensor `index`'s canonical bytes; it is called
    /// only from the reader thread, one tensor at a time.
    pub fn prepare_tensor_group_pipelined(
        &self,
        tensors: &[TensorPipelineTensor<'_>],
        read: &(dyn Fn(usize, &mut [u8]) -> Result<()> + Sync),
        spec: &TensorWriteSpec,
        layout: &TensorLayoutSpec,
        options: &TensorPipelineOptions,
    ) -> Result<TensorPipelineOutcome> {
        options.validate()?;
        if tensors.is_empty() {
            let mut stats = TensorPipelineStats {
                encode_threads: options.encode_threads,
                cpu_tokens: options.cpu_tokens,
                staging_bytes: options.staging_bytes,
                encode_bytes: options.encode_bytes,
                publish_bytes: options.publish_bytes,
                worker_bytes: options.worker_bytes,
                publish_sync_bytes: options.publish_sync_bytes,
                candidate_parallel_min_bytes: options.candidate_parallel_min_bytes,
                ..TensorPipelineStats::default()
            };
            stats.reservation_bound_bytes = options.reservation_bound_bytes(0);
            return Ok(TensorPipelineOutcome {
                results: Vec::new(),
                stats,
            });
        }
        let largest = tensors
            .iter()
            .map(|tensor| tensor.byte_len)
            .max()
            .unwrap_or(0);
        let started = Instant::now();
        let shared = Mutex::new(Shared::new(tensors.len()));
        let cv = Condvar::new();

        let encode_intervals = Mutex::new(vec![StageInterval::default(); tensors.len()]);
        let publish_intervals = Mutex::new(vec![StageInterval::default(); tensors.len()]);
        let reader_ns = AtomicUsize::new(0);
        let wait_staging_ns = AtomicUsize::new(0);
        let wait_worker_bytes_ns = AtomicUsize::new(0);
        let wait_encode_queue_ns = AtomicUsize::new(0);
        let wait_token_ns = AtomicUsize::new(0);
        let wait_publish_idle_ns = AtomicUsize::new(0);
        let parallel_tensors = AtomicUsize::new(0);
        let oversize_staging = AtomicUsize::new(0);
        let oversize_publish = AtomicUsize::new(0);

        let writeback = Writeback::new();
        std::thread::scope(|scope| {
            // ---- reader -------------------------------------------------
            {
                let shared = &shared;
                let cv = &cv;
                let options = options;
                let read = read;
                let wait_staging_ns = &wait_staging_ns;
                let oversize_staging = &oversize_staging;
                let reader_ns = &reader_ns;
                scope.spawn(move || {
                    let reader_start = Instant::now();
                    for (index, tensor) in tensors.iter().enumerate() {
                        let need = tensor.byte_len;
                        if need > options.staging_bytes {
                            oversize_staging.fetch_add(1, Ordering::Relaxed);
                        }
                        if !admit_staging(shared, cv, need, options.staging_bytes, wait_staging_ns)
                        {
                            return;
                        }
                        let mut bytes = vec![0u8; usize::try_from(need).unwrap_or(0)];
                        match read(index, &mut bytes) {
                            Ok(()) => {
                                let mut guard = lock(shared);
                                if guard.error.is_some() {
                                    drop(guard);
                                    release_staging(shared, cv, need);
                                    return;
                                }
                                guard.staged.push_back(StagedTensor { index, bytes });
                                guard.observe_staging();
                                cv.notify_all();
                            }
                            Err(err) => {
                                release_staging(shared, cv, need);
                                fail(shared, cv, index, err);
                                return;
                            }
                        }
                    }
                    reader_ns.store(
                        reader_start.elapsed().as_nanos() as usize,
                        Ordering::Relaxed,
                    );
                    let mut guard = lock(shared);
                    guard.reader_done = true;
                    cv.notify_all();
                });
            }

            // ---- encode workers -----------------------------------------
            for _ in 0..options.encode_threads {
                let shared = &shared;
                let cv = &cv;
                let options = options;
                let layout = layout;
                let encode_intervals = &encode_intervals;
                let wait_worker_bytes_ns = &wait_worker_bytes_ns;
                let wait_encode_queue_ns = &wait_encode_queue_ns;
                let wait_token_ns = &wait_token_ns;
                let parallel_tensors = &parallel_tensors;
                scope.spawn(move || {
                    {
                        let mut guard = lock(shared);
                        guard.live_encoders += 1;
                    }
                    loop {
                        let (claimed, tokens) = match claim_tensor(
                            shared,
                            cv,
                            options,
                            wait_worker_bytes_ns,
                            wait_token_ns,
                        ) {
                            Some(claim) => claim,
                            None => break,
                        };
                        let StagedTensor { index, bytes } = claimed;
                        let tensor = &tensors[index];
                        let reservation = options.worker_reservation(tensor.byte_len) as usize;
                        let mut tensor_spec = spec.clone();
                        tensor_spec.plan.parallel_candidates = options.candidate_parallel_min_bytes
                            > 0
                            && tensor.byte_len >= options.candidate_parallel_min_bytes
                            && tokens == 2;
                        tensor_spec.plan.candidate_parallel_min_bytes =
                            options.candidate_parallel_min_bytes;
                        if tensor_spec.plan.parallel_candidates {
                            parallel_tensors.fetch_add(1, Ordering::Relaxed);
                        }
                        let encode_start = Instant::now();
                        let encoded = self.encode_tensor_object(
                            tensor.dtype,
                            tensor.shape,
                            &bytes,
                            layout,
                            &tensor_spec,
                            &tensor.seed,
                        );
                        drop(bytes);
                        let encode_end = Instant::now();
                        {
                            let mut intervals = lock_vec(encode_intervals);
                            intervals[index] = StageInterval {
                                start_ns: ns_since(started, encode_start),
                                end_ns: ns_since(started, encode_end),
                            };
                        }
                        release_worker(shared, cv, reservation);
                        release_tokens(shared, cv, tokens);
                        match encoded {
                            Ok(encoded) => {
                                let resident =
                                    usize::try_from(encoded.resident_bytes()).unwrap_or(usize::MAX);
                                if !admit_encode_queue(
                                    shared,
                                    cv,
                                    resident,
                                    options.encode_bytes,
                                    wait_encode_queue_ns,
                                ) {
                                    return;
                                }
                                let mut guard = lock(shared);
                                if guard.error.is_some() {
                                    guard.encode_queue =
                                        guard.encode_queue.saturating_sub(resident);
                                    cv.notify_all();
                                    return;
                                }
                                guard.encoded.push_back(EncodedItem {
                                    index,
                                    encoded,
                                    resident,
                                });
                                guard.observe_encode();
                                cv.notify_all();
                            }
                            Err(err) => {
                                fail(shared, cv, index, err);
                                return;
                            }
                        }
                    }
                    let mut guard = lock(shared);
                    guard.live_encoders = guard.live_encoders.saturating_sub(1);
                    cv.notify_all();
                });
            }

            // ---- early writeback ----------------------------------------
            // A separate thread `fsync`s the blobs the publisher has drained
            // from the open deferred batch, so the group's writeback runs while
            // the encoders are still working instead of landing whole on the
            // commit's barrier. It never holds the backend lock for the
            // `fsync`s; it only takes the path list and reports the barriers
            // back for the counters. The scope joins it before this function
            // returns, and a failure fails the pipeline before anything is
            // committed (the ordering rule is never weakened).
            {
                let writeback = &writeback;
                let options = options;
                scope.spawn(move || {
                    loop {
                        let batch = {
                            let mut state = lock_wb(writeback);
                            loop {
                                if state.closed && state.queue.is_empty() {
                                    return;
                                }
                                if let Some(batch) = state.queue.pop_front() {
                                    break Some(batch);
                                }
                                state = writeback
                                    .cv
                                    .wait(state)
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                            }
                        };
                        let Some(batch) = batch else { return };
                        let started = Instant::now();
                        let mut synced = 0u64;
                        for path in &batch {
                            match std::fs::File::open(path) {
                                Ok(file) => {
                                    if let Err(err) = file.sync_data() {
                                        let error = Error::io(format!(
                                            "early barrier cannot fsync {}: {err}",
                                            path.display()
                                        ));
                                        let mut state = lock_wb(writeback);
                                        if state.error.is_none() {
                                            state.error = Some(error);
                                        }
                                        return;
                                    }
                                    synced += 1;
                                }
                                Err(err) => {
                                    let error = Error::io(format!(
                                        "early barrier cannot open {}: {err}",
                                        path.display()
                                    ));
                                    let mut state = lock_wb(writeback);
                                    if state.error.is_none() {
                                        state.error = Some(error);
                                    }
                                    return;
                                }
                            }
                        }
                        let elapsed = started.elapsed().as_nanos() as usize;
                        let mut state = lock_wb(writeback);
                        state.files = state.files.saturating_add(synced);
                        state.passes += 1;
                        state.ns = state.ns.saturating_add(elapsed);
                        drop(state);
                        if let Err(err) = self.note_external_file_barriers(synced) {
                            let mut state = lock_wb(writeback);
                            if state.error.is_none() {
                                state.error = Some(err);
                            }
                            return;
                        }
                        let _ = options;
                    }
                });
            }

            // ---- publisher ----------------------------------------------
            {
                let shared = &shared;
                let cv = &cv;
                let options = options;
                let publish_intervals = &publish_intervals;
                let wait_publish_idle_ns = &wait_publish_idle_ns;
                let oversize_publish = &oversize_publish;
                let writeback = &writeback;
                scope.spawn(move || {
                    let mut since_sync: u64 = 0;
                    loop {
                        let item = {
                            let mut guard = lock(shared);
                            loop {
                                if let Some(item) = guard.encoded.pop_front() {
                                    break Some(item);
                                }
                                if guard.error.is_some() {
                                    break None;
                                }
                                if guard.done() {
                                    break None;
                                }
                                let wait_start = Instant::now();
                                guard = cv
                                    .wait(guard)
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                wait_publish_idle_ns.fetch_add(
                                    wait_start.elapsed().as_nanos() as usize,
                                    Ordering::Relaxed,
                                );
                            }
                        };
                        let Some(item) = item else {
                            trace("publisher-exit", || format!("done={}", lock(shared).done()));
                            break;
                        };
                        trace("publish", || {
                            format!("index={} resident={}", item.index, item.resident)
                        });
                        let resident = item.resident;
                        if resident as u64 > options.publish_bytes {
                            oversize_publish.fetch_add(1, Ordering::Relaxed);
                        }
                        {
                            let mut guard = lock(shared);
                            guard.publish_used = guard.publish_used.saturating_add(resident);
                            guard.observe_publish();
                        }
                        let EncodedItem {
                            index,
                            encoded,
                            resident,
                        } = item;
                        let plan = encoded.plan.clone();
                        let publish_start = Instant::now();
                        let published = self.publish_encoded_tensor(encoded);
                        let publish_end = Instant::now();
                        {
                            let mut intervals = lock_vec(publish_intervals);
                            intervals[index] = StageInterval {
                                start_ns: ns_since(started, publish_start),
                                end_ns: ns_since(started, publish_end),
                            };
                        }
                        let mut guard = lock(shared);
                        guard.publish_used = guard.publish_used.saturating_sub(resident);
                        guard.encode_queue = guard.encode_queue.saturating_sub(resident);
                        match published {
                            Ok(prepared) => {
                                guard.results[index] = Some((prepared, plan));
                                guard.published += 1;
                                cv.notify_all();
                                drop(guard);
                            }
                            Err(err) => {
                                cv.notify_all();
                                drop(guard);
                                fail(shared, cv, index, err);
                                return;
                            }
                        }
                        if options.publish_sync_bytes > 0 {
                            since_sync = since_sync.saturating_add(resident as u64);
                            if since_sync >= options.publish_sync_bytes {
                                since_sync = 0;
                                // Drain the batch's file list under the backend
                                // lock (cheap) and hand the `fsync`s to the
                                // writeback thread.
                                match self.take_published_blob_writeback_files() {
                                    Ok(files) if files.is_empty() => {}
                                    Ok(files) => {
                                        let mut state = lock_wb(writeback);
                                        state.queue.push_back(files);
                                        writeback.cv.notify_all();
                                    }
                                    Err(err) => {
                                        fail(shared, cv, index, err);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    // Every published blob is either drained to the writeback
                    // thread or still in the open batch; closing the queue lets
                    // the writer finish the drained ones before we return.
                    {
                        let mut state = lock_wb(writeback);
                        state.closed = true;
                        writeback.cv.notify_all();
                    }
                });
            }
        });

        // Every thread in the scope has joined: the early-barrier thread has
        // finished every pass it accepted, so a drained file is durable or the
        // pipeline reports an error and nothing is committed.
        let mut writeback = writeback;
        writeback.harvest();
        if let Some(err) = writeback.error() {
            return Err(err);
        }

        let mut guard = lock(&shared);
        if let Some((index, err)) = guard.error.take() {
            let name = tensors
                .get(index)
                .map(|tensor| tensor.name)
                .unwrap_or("<unknown>");
            return Err(Error::new(
                err.class(),
                format!("pipeline tensor `{name}` ({index}): {}", err.message()),
            ));
        }
        let results = std::mem::take(&mut guard.results);
        drop(guard);
        let mut out = Vec::with_capacity(results.len());
        for (index, slot) in results.into_iter().enumerate() {
            match slot {
                Some(result) => out.push(result),
                None => {
                    return Err(Error::corrupt(format!(
                        "pipeline produced no fragment for tensor {index}"
                    )))
                }
            }
        }

        let encode_intervals = std::mem::take(&mut *lock_vec(&encode_intervals));
        let publish_intervals = std::mem::take(&mut *lock_vec(&publish_intervals));
        let encode_busy_ns: u64 = encode_intervals
            .iter()
            .map(|item| item.end_ns.saturating_sub(item.start_ns))
            .sum();
        let publish_busy_ns: u64 = publish_intervals
            .iter()
            .map(|item| item.end_ns.saturating_sub(item.start_ns))
            .sum();
        let encode_wall_ns = span(&encode_intervals);
        let publish_wall_ns = span(&publish_intervals);
        let publish_union = union(&publish_intervals);
        let encode_union = union(&encode_intervals);
        let encode_overlapped_ns = intersect_total(&encode_intervals, &publish_union);
        let publish_overlapped_ns = intersect_total(&publish_intervals, &encode_union);

        let mut results_out = Vec::with_capacity(out.len());
        for (index, (prepared, plan)) in out.into_iter().enumerate() {
            results_out.push(PipelineTensorResult {
                prepared,
                plan,
                encode_ms: encode_intervals[index]
                    .end_ns
                    .saturating_sub(encode_intervals[index].start_ns)
                    as f64
                    / 1.0e6,
                publish_ms: publish_intervals[index]
                    .end_ns
                    .saturating_sub(publish_intervals[index].start_ns)
                    as f64
                    / 1.0e6,
            });
        }

        // The reservations are gone once every thread has joined, so the
        // observed maxima are final.
        let (max_staging, max_encode, max_publish, max_worker, max_resident) = {
            let guard = lock(&shared);
            (
                guard.max_staging,
                guard.max_encode,
                guard.max_publish,
                guard.max_worker,
                guard.max_resident,
            )
        };

        let stats = TensorPipelineStats {
            tensors: tensors.len(),
            encode_threads: options.encode_threads,
            cpu_tokens: options.cpu_tokens,
            staging_bytes: options.staging_bytes,
            encode_bytes: options.encode_bytes,
            publish_bytes: options.publish_bytes,
            worker_bytes: options.worker_bytes,
            publish_sync_bytes: options.publish_sync_bytes,
            candidate_parallel_min_bytes: options.candidate_parallel_min_bytes,
            reservation_bound_bytes: options.reservation_bound_bytes(largest),
            observed_max_staging_bytes: max_staging as u64,
            observed_max_encode_bytes: max_encode as u64,
            observed_max_publish_bytes: max_publish as u64,
            observed_max_worker_bytes: max_worker as u64,
            observed_max_resident_tensors: max_resident,
            oversize_staging: oversize_staging.load(Ordering::Relaxed) as u64,
            oversize_publish: oversize_publish.load(Ordering::Relaxed) as u64,
            candidate_parallel_tensors: parallel_tensors.load(Ordering::Relaxed) as u64,
            reader_ms: reader_ns.load(Ordering::Relaxed) as f64 / 1.0e6,
            encode_busy_ms: encode_busy_ns as f64 / 1.0e6,
            publish_busy_ms: publish_busy_ns as f64 / 1.0e6,
            encode_wall_ms: encode_wall_ns as f64 / 1.0e6,
            publish_wall_ms: publish_wall_ns as f64 / 1.0e6,
            pipeline_wall_ms: started.elapsed().as_secs_f64() * 1000.0,
            encode_overlapped_ms: encode_overlapped_ns as f64 / 1.0e6,
            publish_overlapped_ms: publish_overlapped_ns as f64 / 1.0e6,
            staging_wait_ms: wait_staging_ns.load(Ordering::Relaxed) as f64 / 1.0e6,
            worker_bytes_wait_ms: wait_worker_bytes_ns.load(Ordering::Relaxed) as f64 / 1.0e6,
            encode_queue_wait_ms: wait_encode_queue_ns.load(Ordering::Relaxed) as f64 / 1.0e6,
            publish_idle_ms: wait_publish_idle_ns.load(Ordering::Relaxed) as f64 / 1.0e6,
            cpu_token_wait_ms: wait_token_ns.load(Ordering::Relaxed) as f64 / 1.0e6,
            writeback_passes: writeback.passes,
            writeback_files: writeback.files,
            writeback_ms: writeback.ns as f64 / 1.0e6,
            encode_intervals,
            publish_intervals,
        };
        Ok(TensorPipelineOutcome {
            results: results_out,
            stats,
        })
    }

    /// Drain the file list of the store's open deferred durability batch so
    /// the caller can `fsync` those files on its own thread (R21).
    ///
    /// The ordering rule is untouched: the caller owes the `fsync`s before the
    /// commit record, and the commit path still closes the batch and `fsync`s
    /// every remaining file and every touched directory before it publishes.
    /// Nothing here claims durability.
    pub fn take_published_blob_writeback_files(&self) -> Result<Vec<std::path::PathBuf>> {
        self.with_backend_mut(|backend| Ok(backend.take_pending_writeback_files()))
    }

    /// Record `files` file barriers performed outside the backend, so the
    /// durability counters stay exact.
    pub fn note_external_file_barriers(&self, files: u64) -> Result<()> {
        self.with_backend_mut(|backend| {
            backend.note_external_file_barriers(files);
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Early writeback
// ---------------------------------------------------------------------------

/// State shared with the early-barrier thread.
struct Writeback {
    queue: Mutex<WritebackQueue>,
    cv: Condvar,
    /// Read once every thread in the scope has joined; the mutex makes those
    /// reads safe without a second lock discipline.
    passes: u64,
    files: u64,
    ns: usize,
}

struct WritebackQueue {
    queue: VecDeque<Vec<std::path::PathBuf>>,
    closed: bool,
    error: Option<Error>,
    passes: u64,
    files: u64,
    ns: usize,
}

impl Writeback {
    fn new() -> Self {
        Writeback {
            queue: Mutex::new(WritebackQueue {
                queue: VecDeque::new(),
                closed: false,
                error: None,
                passes: 0,
                files: 0,
                ns: 0,
            }),
            cv: Condvar::new(),
            passes: 0,
            files: 0,
            ns: 0,
        }
    }

    /// Copy the counters out of the shared state (after the scope joined).
    fn harvest(&mut self) {
        let (passes, files, ns) = {
            let state = lock_wb(self);
            (state.passes, state.files, state.ns)
        };
        self.passes = passes;
        self.files = files;
        self.ns = ns;
    }

    fn error(&self) -> Option<Error> {
        lock_wb(self).error.clone()
    }
}

fn lock_wb(writeback: &Writeback) -> MutexGuard<'_, WritebackQueue> {
    writeback
        .queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct StagedTensor {
    index: usize,
    bytes: Vec<u8>,
}

struct EncodedItem {
    index: usize,
    encoded: EncodedTensor,
    resident: usize,
}

struct Shared {
    staged: VecDeque<StagedTensor>,
    staged_reserved: usize,
    encoded: VecDeque<EncodedItem>,
    encode_queue: usize,
    publish_used: usize,
    worker_used: usize,
    tokens_used: usize,
    reader_done: bool,
    live_encoders: usize,
    published: usize,
    results: Vec<Option<(PreparedTensorObject, TensorPlan)>>,
    error: Option<(usize, Error)>,
    max_staging: usize,
    max_encode: usize,
    max_publish: usize,
    max_worker: usize,
    max_resident: usize,
}

impl Shared {
    fn new(tensors: usize) -> Self {
        Shared {
            staged: VecDeque::new(),
            staged_reserved: 0,
            encoded: VecDeque::new(),
            encode_queue: 0,
            publish_used: 0,
            worker_used: 0,
            tokens_used: 0,
            reader_done: false,
            live_encoders: 0,
            published: 0,
            results: (0..tensors).map(|_| None).collect(),
            error: None,
            max_staging: 0,
            max_encode: 0,
            max_publish: 0,
            max_worker: 0,
            max_resident: 0,
        }
    }

    fn observe_staging(&mut self) {
        self.max_staging = self.max_staging.max(self.staged_reserved);
    }

    fn observe_encode(&mut self) {
        self.max_encode = self.max_encode.max(self.encode_queue);
        self.max_resident = self.max_resident.max(self.encoded.len());
    }

    fn observe_publish(&mut self) {
        self.max_publish = self.max_publish.max(self.publish_used);
    }

    fn observe_worker(&mut self) {
        self.max_worker = self.max_worker.max(self.worker_used);
    }

    /// No encoder can produce another fragment and none is waiting.
    fn done(&self) -> bool {
        self.reader_done && self.staged.is_empty() && self.live_encoders == 0
    }
}

#[inline]
fn trace(what: &str, detail: impl FnOnce() -> String) {
    if std::env::var_os("WEFT_PIPELINE_TRACE").is_some() {
        eprintln!("pl[{:?}] {what} {}", std::thread::current().id(), detail());
    }
}

fn ns_since(started: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(started).as_nanos() as u64
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_vec<T>(mutex: &Mutex<Vec<T>>) -> MutexGuard<'_, Vec<T>> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fail(shared: &Mutex<Shared>, cv: &Condvar, index: usize, err: Error) {
    let mut guard = lock(shared);
    if guard.error.is_none() {
        guard.error = Some((index, err));
    }
    cv.notify_all();
}

fn admit_staging(
    shared: &Mutex<Shared>,
    cv: &Condvar,
    need: u64,
    budget: u64,
    wait_ns: &AtomicUsize,
) -> bool {
    let need = usize::try_from(need).unwrap_or(usize::MAX);
    let budget = usize::try_from(budget).unwrap_or(usize::MAX);
    let mut guard = lock(shared);
    loop {
        if guard.error.is_some() {
            return false;
        }
        // Oversize admission: a tensor larger than the whole budget may run
        // when nothing else is staged, exactly like `run_bounded`.
        if guard.staged_reserved + need <= budget || guard.staged_reserved == 0 {
            guard.staged_reserved = guard.staged_reserved.saturating_add(need);
            guard.observe_staging();
            return true;
        }
        trace("staging-wait", || {
            format!(
                "need={need} reserved={} budget={budget}",
                guard.staged_reserved
            )
        });
        let wait_start = Instant::now();
        guard = cv
            .wait(guard)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        wait_ns.fetch_add(wait_start.elapsed().as_nanos() as usize, Ordering::Relaxed);
    }
}

fn release_staging(shared: &Mutex<Shared>, cv: &Condvar, need: u64) {
    let mut guard = lock(shared);
    guard.staged_reserved = guard.staged_reserved.saturating_sub(need as usize);
    cv.notify_all();
}

fn release_worker(shared: &Mutex<Shared>, cv: &Condvar, reservation: usize) {
    let mut guard = lock(shared);
    guard.worker_used = guard.worker_used.saturating_sub(reservation);
    cv.notify_all();
}

fn release_tokens(shared: &Mutex<Shared>, cv: &Condvar, tokens: usize) {
    let mut guard = lock(shared);
    guard.tokens_used = guard.tokens_used.saturating_sub(tokens);
    cv.notify_all();
}

/// Claim the next tensor FIFO with its CPU tokens and worker-byte reservation.
fn claim_tensor(
    shared: &Mutex<Shared>,
    cv: &Condvar,
    options: &TensorPipelineOptions,
    worker_wait_ns: &AtomicUsize,
    token_wait_ns: &AtomicUsize,
) -> Option<(StagedTensor, usize)> {
    let worker_budget = usize::try_from(options.worker_bytes).unwrap_or(usize::MAX);
    let mut guard = lock(shared);
    loop {
        if guard.error.is_some() {
            return None;
        }
        let Some(front_len) = guard.staged.front().map(|item| item.bytes.len()) else {
            if guard.reader_done {
                return None;
            }
            guard = cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            continue;
        };
        let reservation =
            usize::try_from(options.worker_reservation(front_len as u64)).unwrap_or(usize::MAX);
        // Oversize admission when nothing else is in flight.
        let admitted = guard.worker_used.saturating_add(reservation) <= worker_budget
            || guard.worker_used == 0;
        if !admitted {
            trace("worker-wait", || {
                format!(
                    "len={front_len} used={} budget={worker_budget}",
                    guard.worker_used
                )
            });
            let wait_start = Instant::now();
            guard = cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            worker_wait_ns.fetch_add(wait_start.elapsed().as_nanos() as usize, Ordering::Relaxed);
            continue;
        }
        let free = options.cpu_tokens.saturating_sub(guard.tokens_used);
        let wants_two = options.candidate_parallel_min_bytes > 0
            && (front_len as u64) >= options.candidate_parallel_min_bytes;
        let tokens = if wants_two && free >= 2 {
            2
        } else if free >= 1 {
            1
        } else {
            trace("token-wait", || {
                format!("free={free} used={}", guard.tokens_used)
            });
            let wait_start = Instant::now();
            guard = cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            token_wait_ns.fetch_add(wait_start.elapsed().as_nanos() as usize, Ordering::Relaxed);
            continue;
        };
        let item = match guard.staged.pop_front() {
            Some(item) => item,
            None => continue,
        };
        trace("claim", || {
            format!(
                "index={} len={} worker={} tokens={} staged_reserved={}",
                item.index,
                item.bytes.len(),
                guard.worker_used,
                tokens,
                guard.staged_reserved
            )
        });
        // The decoder's reservation moves from the staging queue to the
        // worker's own byte reservation: those bytes are now held by the
        // encoder, and the reader may admit the next tensor.
        guard.staged_reserved = guard.staged_reserved.saturating_sub(item.bytes.len());
        guard.worker_used = guard.worker_used.saturating_add(reservation);
        guard.tokens_used = guard.tokens_used.saturating_add(tokens);
        guard.observe_worker();
        cv.notify_all();
        return Some((item, tokens));
    }
}

fn admit_encode_queue(
    shared: &Mutex<Shared>,
    cv: &Condvar,
    resident: usize,
    budget: u64,
    wait_ns: &AtomicUsize,
) -> bool {
    let budget = usize::try_from(budget).unwrap_or(usize::MAX);
    let mut guard = lock(shared);
    loop {
        if guard.error.is_some() {
            return false;
        }
        if guard.encode_queue.saturating_add(resident) <= budget || guard.encode_queue == 0 {
            trace("queue-admit", || {
                format!("resident={resident} queue={}", guard.encode_queue)
            });
            guard.encode_queue = guard.encode_queue.saturating_add(resident);
            guard.observe_encode();
            return true;
        }
        trace("queue-wait", || {
            format!(
                "resident={resident} queue={} budget={budget}",
                guard.encode_queue
            )
        });
        let wait_start = Instant::now();
        guard = cv
            .wait(guard)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        wait_ns.fetch_add(wait_start.elapsed().as_nanos() as usize, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Interval arithmetic
// ---------------------------------------------------------------------------

fn span(intervals: &[StageInterval]) -> u64 {
    let mut first: Option<u64> = None;
    let mut last = 0u64;
    for item in intervals.iter().filter(|item| item.end_ns > item.start_ns) {
        first = Some(first.map_or(item.start_ns, |value| value.min(item.start_ns)));
        last = last.max(item.end_ns);
    }
    match first {
        Some(first) => last.saturating_sub(first),
        None => 0,
    }
}

/// Merge intervals into a sorted, disjoint set.
fn union(intervals: &[StageInterval]) -> Vec<StageInterval> {
    let mut sorted: Vec<StageInterval> = intervals
        .iter()
        .copied()
        .filter(|item| item.end_ns > item.start_ns)
        .collect();
    sorted.sort_by_key(|item| item.start_ns);
    let mut out: Vec<StageInterval> = Vec::with_capacity(sorted.len());
    for item in sorted {
        match out.last_mut() {
            Some(last) if item.start_ns <= last.end_ns => {
                last.end_ns = last.end_ns.max(item.end_ns);
            }
            _ => out.push(item),
        }
    }
    out
}

/// Total length of the intersection of `intervals` with a merged union.
fn intersect_total(intervals: &[StageInterval], other: &[StageInterval]) -> u64 {
    let mut total = 0u64;
    for item in intervals.iter().filter(|item| item.end_ns > item.start_ns) {
        for block in other {
            let start = item.start_ns.max(block.start_ns);
            let end = item.end_ns.min(block.end_ns);
            if end > start {
                total = total.saturating_add(end - start);
            }
        }
    }
    total
}

const _: fn() = || {
    fn assert_sync<T: Sync>() {}
    assert_sync::<Store>();
};
