//! One admission object for every stage that runs work concurrently (X06/N09).
//!
//! Before this module the process had two unrelated schedulers. The write side
//! ([`crate::tensor_pipeline`]) had a real admission policy: FIFO over tensors,
//! a CPU-token budget, byte reservations per stage, and an oversize-single
//! rule. The restore side had loose knobs — `threads`, `shard_threads`,
//! `tile_threads`, `max_inflight_bytes`, `shard_max_inflight_bytes` — that each
//! bounded their own axis and could multiply: R13 measured `4 × 4` losing to
//! `2 × 4` on four vCPUs, and shard workers cost 14–20 % more RSS. Two
//! schedulers that each assume they own the machine is the defect this module
//! removes.
//!
//! [`StageBudget`] is the single object. Every stage acquires from it:
//!
//! * **Active workers.** A request declares how many threads it will run at
//!   once (a tensor decode, a tensor's shard workers, a tensor's tile workers,
//!   the write pipeline's encode workers with their CPU tokens). The request is
//!   granted only when the total granted at once stays within
//!   [`StageBudgetPolicy::max_active_workers`]. A request larger than the whole
//!   cap is **clamped** to the cap and reported (`clamped_worker_requests`),
//!   never silently allowed to exceed it.
//! * **In-flight bytes.** A request declares how many bytes it will hold. The
//!   sum over granted requests stays within
//!   [`StageBudgetPolicy::max_inflight_bytes`]. A request whose own size
//!   exceeds that bound runs **alone** (it waits until no bytes are reserved),
//!   so a tensor larger than the budget always completes and never deadlocks.
//! * **Cancellation.** Each request may carry a [`StageCancelToken`]. A waiter that
//!   observes a cancelled token releases its ticket, wakes the next waiter, and
//!   returns [`Class::Cancelled`]. A grant already held is released by its
//!   RAII [`Admission`] on drop, including during unwinding, so a panic in a
//!   stage can never leak a reservation.
//! * **Fairness.** Admission is strictly FIFO over tickets. A stage that is
//!   waiting is served before any later request from any stage, so no stage can
//!   starve another (encode cannot lock out restore, restore cannot lock out
//!   background maintenance).
//! * **Determinism.** Callers hand tickets in a deterministic order (the
//!   restore pipeline reserves in input index order; the write pipeline is FIFO
//!   over the staged tensors). Given that order, the *set* of admissions and
//!   therefore the read order and the output bytes are a function of the
//!   policy and the inputs, not of thread completion order.
//!
//! The policy defaults are derived from the machine
//! ([`StageBudgetPolicy::from_parallelism`]): `max_active_workers` is
//! [`std::thread::available_parallelism`] and `max_inflight_bytes` is 512 MiB.
//! A caller that wants a different policy constructs it explicitly; there is no
//! silent upgrade path, and `0` is rejected rather than read as "unlimited".
//!
//! The budget is process-wide only when a caller shares one `Arc`. Flipping the
//! default of every entry point would silently change published write and
//! restore numbers, so the shipped defaults of `TensorPipelineOptions` and
//! `RestorePipelineOptions` remain their historical private bounds; setting
//! `Options::shared_stage_budget` (or passing a budget explicitly) opts a store
//! into the one scheduler. See `docs/STAGE-BUDGET.md` for the measured
//! consequence of that choice.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

use serde::Serialize;

use crate::error::{Error, Result};

/// Default in-flight byte budget across every stage: 512 MiB.
pub const DEFAULT_MAX_INFLIGHT_BYTES: usize = 512 * 1024 * 1024;

/// A concurrent stage that can ask the budget for capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The checkpoint write pipeline's encode workers.
    Encode,
    /// Codec-internal encode/decode threads.
    Codec,
    /// Backend I/O workers.
    Io,
    /// Checkpoint restore workers (tensor, shard and tile threads).
    Restore,
    /// Background maintenance (GC, compaction, verification).
    Maintenance,
}

impl Stage {
    /// The stable lower-case name used in receipts.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Encode => "encode",
            Stage::Codec => "codec",
            Stage::Io => "io",
            Stage::Restore => "restore",
            Stage::Maintenance => "maintenance",
        }
    }
}

/// The one policy every stage is admitted against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StageBudgetPolicy {
    /// Maximum number of concurrently active worker threads across all stages.
    pub max_active_workers: usize,
    /// Maximum number of in-flight bytes across all stages.
    pub max_inflight_bytes: usize,
}

impl StageBudgetPolicy {
    /// The declared default for this machine.
    ///
    /// `max_active_workers` is the machine's available parallelism (never less
    /// than one); `max_inflight_bytes` is 512 MiB. The worker cap is the number
    /// the whole process may run at once, not a per-stage allowance: two stages
    /// running at the same time share it.
    pub fn from_parallelism() -> Self {
        let available = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        StageBudgetPolicy {
            max_active_workers: available.max(1),
            max_inflight_bytes: DEFAULT_MAX_INFLIGHT_BYTES,
        }
    }

    /// Validate a caller-supplied policy.
    ///
    /// Fail closed: a zero worker cap or a zero byte bound is rejected with
    /// [`Class::InvalidArgument`]. "Unlimited" is not expressible.
    pub fn validate(&self) -> Result<()> {
        if self.max_active_workers == 0 {
            return Err(Error::invalid_argument(
                "stage budget: max_active_workers must be at least 1",
            ));
        }
        if self.max_inflight_bytes == 0 {
            return Err(Error::invalid_argument(
                "stage budget: max_inflight_bytes must be at least 1",
            ));
        }
        Ok(())
    }
}

/// Counters a receipt can publish. Never a correctness input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StageBudgetStats {
    /// The configured policy, copied for the receipt.
    pub max_active_workers: usize,
    /// The configured byte bound.
    pub max_inflight_bytes: usize,
    /// Largest number of granted active workers observed.
    pub observed_max_active_workers: usize,
    /// Largest number of reserved bytes observed.
    pub observed_max_inflight_bytes: usize,
    /// Admissions granted, by stage.
    pub granted: Vec<(String, u64)>,
    /// Waiters that had to block at least once, by stage.
    pub waited: Vec<(String, u64)>,
    /// Requests clamped to the worker cap.
    pub clamped_worker_requests: u64,
    /// Requests admitted alone because their own size exceeded the byte bound.
    pub oversized_admissions: u64,
    /// Waiters woken by cancellation.
    pub cancelled_waiters: u64,
    /// Largest number of simultaneously waiting requests.
    pub observed_max_waiters: usize,
    /// Total time blocked in admission, in nanoseconds.
    pub wait_ns: u128,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StageCounters {
    granted: u64,
    waited: u64,
}

struct State {
    /// Sum of granted worker widths.
    active_workers: usize,
    /// Sum of granted non-oversize byte reservations.
    inflight_bytes: usize,
    /// Next ticket to hand out.
    next_ticket: u64,
    /// Ticket currently allowed to attempt admission (strict FIFO).
    serving: u64,
    /// Requests blocked in `admit`.
    waiters: usize,
    /// Bookkeeping for stats, indexed by `Stage` discriminant.
    counters: [StageCounters; 5],
    clamped: u64,
    oversized: u64,
    cancelled: u64,
    observed_active: usize,
    observed_bytes: usize,
    observed_waiters: usize,
    wait_ns: u128,
}

impl State {
    fn counters_mut(&mut self, stage: Stage) -> &mut StageCounters {
        &mut self.counters[stage_index(stage)]
    }
}

fn stage_index(stage: Stage) -> usize {
    match stage {
        Stage::Encode => 0,
        Stage::Codec => 1,
        Stage::Io => 2,
        Stage::Restore => 3,
        Stage::Maintenance => 4,
    }
}

const STAGE_NAMES: [&str; 5] = ["encode", "codec", "io", "restore", "maintenance"];

struct Inner {
    policy: StageBudgetPolicy,
    state: Mutex<State>,
    cv: Condvar,
    /// Budget-wide cancellation (in addition to per-request tokens).
    cancelled: AtomicBool,
    /// Tickets ever handed out, for diagnostics.
    tickets: AtomicU64,
    /// Grants in total, for diagnostics.
    grants: AtomicU64,
}

/// A per-request cancellation token.
///
/// Cancelling wakes every waiter of the budget this token belongs to and makes
/// waiters that reference it return [`Class::Cancelled`]. Grants already held
/// are *not* revoked: the holder releases them on drop, which is what keeps
/// cancellation free of leaks and free of a second reclamation path.
#[derive(Clone)]
pub struct StageCancelToken {
    inner: Arc<Inner>,
    flag: Arc<AtomicBool>,
}

impl std::fmt::Debug for StageCancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageCancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl StageCancelToken {
    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire) || self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Request cancellation and wake every waiter.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        let _guard = lock(&self.inner.state);
        self.inner.cv.notify_all();
    }
}

/// The single admission object.
pub struct StageBudget {
    inner: Arc<Inner>,
}

impl StageBudget {
    /// Build a budget from an explicit policy.
    pub fn new(policy: StageBudgetPolicy) -> Result<Arc<StageBudget>> {
        policy.validate()?;
        Ok(Arc::new(StageBudget {
            inner: Arc::new(Inner {
                policy,
                state: Mutex::new(State {
                    active_workers: 0,
                    inflight_bytes: 0,
                    next_ticket: 0,
                    serving: 0,
                    waiters: 0,
                    counters: Default::default(),
                    clamped: 0,
                    oversized: 0,
                    cancelled: 0,
                    observed_active: 0,
                    observed_bytes: 0,
                    observed_waiters: 0,
                    wait_ns: 0,
                }),
                cv: Condvar::new(),
                cancelled: AtomicBool::new(false),
                tickets: AtomicU64::new(0),
                grants: AtomicU64::new(0),
            }),
        }))
    }

    /// The declared policy for this machine (see
    /// [`StageBudgetPolicy::from_parallelism`]).
    pub fn process_default() -> Result<Arc<StageBudget>> {
        StageBudget::new(StageBudgetPolicy::from_parallelism())
    }

    /// A budget explicitly sized for a court or an embedded caller.
    pub fn with_capacity(
        max_active_workers: usize,
        max_inflight_bytes: usize,
    ) -> Result<Arc<StageBudget>> {
        StageBudget::new(StageBudgetPolicy {
            max_active_workers,
            max_inflight_bytes,
        })
    }

    /// The policy this budget enforces.
    pub fn policy(&self) -> StageBudgetPolicy {
        self.inner.policy
    }

    /// A cancellation token bound to this budget.
    pub fn cancel_token(&self) -> StageCancelToken {
        StageCancelToken {
            inner: Arc::clone(&self.inner),
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Cancel every waiter of this budget.
    pub fn cancel_all(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        let _guard = lock(&self.inner.state);
        self.inner.cv.notify_all();
    }

    /// Admit `workers` threads holding `bytes` for `stage`.
    ///
    /// Blocks in FIFO order until the policy allows it, the token is cancelled,
    /// or the budget is cancelled. The returned [`Admission`] releases
    /// everything on drop. `workers` is clamped to the policy's
    /// `max_active_workers` and the clamp is reported in the stats; the caller
    /// must use [`Admission::workers`] as the width it actually runs at.
    pub fn admit(
        &self,
        stage: Stage,
        workers: usize,
        bytes: usize,
        cancel: Option<&StageCancelToken>,
    ) -> Result<Admission> {
        let policy = self.inner.policy;
        let requested = workers.max(1);
        let granted_workers = requested.min(policy.max_active_workers);
        let clamped = requested > policy.max_active_workers;
        let oversized = bytes > policy.max_inflight_bytes;

        let mut guard = lock(&self.inner.state);
        let ticket = guard.next_ticket;
        guard.next_ticket += 1;
        guard.waiters += 1;
        guard.observed_waiters = guard.observed_waiters.max(guard.waiters);
        self.inner.tickets.fetch_add(1, Ordering::Relaxed);

        let mut waited = false;
        loop {
            if cancellation(&self.inner, cancel) {
                guard.cancelled += 1;
                abandon(&self.inner, guard, ticket, stage);
                return Err(Error::new(
                    crate::error::Class::Cancelled,
                    "stage budget: request cancelled while waiting for admission",
                ));
            }
            if guard.serving == ticket {
                let active_ok = guard.active_workers + granted_workers <= policy.max_active_workers;
                let bytes_ok = if oversized {
                    guard.inflight_bytes == 0
                } else {
                    guard.inflight_bytes + bytes <= policy.max_inflight_bytes
                };
                if active_ok && bytes_ok {
                    guard.active_workers += granted_workers;
                    if !oversized {
                        guard.inflight_bytes += bytes;
                    }
                    guard.serving = ticket + 1;
                    guard.waiters -= 1;
                    guard.observed_active = guard.observed_active.max(guard.active_workers);
                    guard.observed_bytes = guard.observed_bytes.max(guard.inflight_bytes);
                    let counters = guard.counters_mut(stage);
                    counters.granted += 1;
                    if waited {
                        counters.waited += 1;
                    }
                    if clamped {
                        guard.clamped += 1;
                    }
                    if oversized {
                        guard.oversized += 1;
                    }
                    drop(guard);
                    self.inner.grants.fetch_add(1, Ordering::Relaxed);
                    self.inner.cv.notify_all();
                    return Ok(Admission {
                        inner: Arc::clone(&self.inner),
                        stage,
                        workers: granted_workers,
                        bytes: if oversized { 0 } else { bytes },
                        requested_bytes: bytes,
                        oversized,
                    });
                }
            }
            waited = true;
            let wait_started = Instant::now();
            guard = self
                .inner
                .cv
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.wait_ns = guard
                .wait_ns
                .saturating_add(wait_started.elapsed().as_nanos());
        }
    }

    /// A point-in-time snapshot.
    pub fn stats(&self) -> StageBudgetStats {
        let guard = lock(&self.inner.state);
        let mut granted = Vec::with_capacity(5);
        let mut waited = Vec::with_capacity(5);
        for (index, name) in STAGE_NAMES.iter().enumerate() {
            granted.push(((*name).to_owned(), guard.counters[index].granted));
            waited.push(((*name).to_owned(), guard.counters[index].waited));
        }
        StageBudgetStats {
            max_active_workers: self.inner.policy.max_active_workers,
            max_inflight_bytes: self.inner.policy.max_inflight_bytes,
            observed_max_active_workers: guard.observed_active,
            observed_max_inflight_bytes: guard.observed_bytes,
            granted,
            waited,
            clamped_worker_requests: guard.clamped,
            oversized_admissions: guard.oversized,
            cancelled_waiters: guard.cancelled,
            observed_max_waiters: guard.observed_waiters,
            wait_ns: guard.wait_ns,
        }
    }

    /// The number of tickets handed out (diagnostic).
    pub fn tickets(&self) -> u64 {
        self.inner.tickets.load(Ordering::Relaxed)
    }

    /// The number of admissions granted (diagnostic).
    pub fn grants(&self) -> u64 {
        self.inner.grants.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for StageBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageBudget")
            .field("policy", &self.inner.policy)
            .finish()
    }
}

/// A granted reservation. Drop releases it; there is no other release path.
pub struct Admission {
    inner: Arc<Inner>,
    #[allow(dead_code)]
    stage: Stage,
    workers: usize,
    bytes: usize,
    requested_bytes: usize,
    oversized: bool,
}

impl Admission {
    /// The worker width actually granted (≤ the policy cap).
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// The bytes actually reserved (0 for an oversize-single admission).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The bytes this request declared.
    pub fn requested_bytes(&self) -> usize {
        self.requested_bytes
    }

    /// Whether this request was admitted alone because it exceeded the bound.
    pub fn oversized(&self) -> bool {
        self.oversized
    }

    /// Release the worker slots but keep the byte reservation.
    ///
    /// A restore worker stops running threads as soon as its decode is done but
    /// keeps its decoded bytes reserved until the ordered writer has consumed
    /// them, so the byte bound covers the whole staging window while the worker
    /// cap covers only actual work. Calling this twice is a no-op.
    pub fn release_workers(&mut self) {
        if self.workers == 0 {
            return;
        }
        let mut guard = lock(&self.inner.state);
        guard.active_workers = guard.active_workers.saturating_sub(self.workers);
        self.workers = 0;
        drop(guard);
        self.inner.cv.notify_all();
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut guard = lock(&self.inner.state);
        guard.active_workers = guard.active_workers.saturating_sub(self.workers);
        guard.inflight_bytes = guard.inflight_bytes.saturating_sub(self.bytes);
        self.workers = 0;
        self.bytes = 0;
        drop(guard);
        self.inner.cv.notify_all();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cancellation(inner: &Inner, token: Option<&StageCancelToken>) -> bool {
    inner.cancelled.load(Ordering::Acquire)
        || token.map(|token| token.is_cancelled()).unwrap_or(false)
}

/// Give up a ticket: advance FIFO service past it and wake the next waiter.
fn abandon(inner: &Inner, guard: MutexGuard<'_, State>, ticket: u64, stage: Stage) {
    let mut guard = guard;
    if guard.serving == ticket {
        guard.serving = ticket + 1;
    }
    guard.waiters = guard.waiters.saturating_sub(1);
    guard.counters_mut(stage).waited += 1;
    drop(guard);
    inner.cv.notify_all();
}

// ---------------------------------------------------------------------------
// Process-wide policy snapshot (N09)
// ---------------------------------------------------------------------------

/// Read `WEFT_STAGE_MAX_ACTIVE` / `WEFT_STAGE_MAX_INFLIGHT_BYTES` overrides.
///
/// Used by the stress court so one binary can exercise 1/2/4/8-worker policies
/// without recompiling. An unparsable or zero value is an error: the budget
/// never falls back to "unlimited".
pub fn policy_from_env() -> Result<StageBudgetPolicy> {
    let mut policy = StageBudgetPolicy::from_parallelism();
    if let Ok(raw) = std::env::var("WEFT_STAGE_MAX_ACTIVE") {
        let value: usize = raw.parse().map_err(|_| {
            Error::invalid_argument(format!(
                "WEFT_STAGE_MAX_ACTIVE must be a positive integer (got {raw:?})"
            ))
        })?;
        policy.max_active_workers = value;
    }
    if let Ok(raw) = std::env::var("WEFT_STAGE_MAX_INFLIGHT_BYTES") {
        let value: u64 = raw.parse().map_err(|_| {
            Error::invalid_argument(format!(
                "WEFT_STAGE_MAX_INFLIGHT_BYTES must be a positive integer (got {raw:?})"
            ))
        })?;
        policy.max_inflight_bytes = usize::try_from(value).unwrap_or(usize::MAX);
    }
    policy.validate()?;
    Ok(policy)
}

/// The number of `&StageBudget`-visible tickets, for tests that assert a
/// waiter really reached the queue instead of failing early.
pub fn waiting_now(budget: &StageBudget) -> usize {
    lock(&budget.inner.state).waiters
}

/// Whether the request this thread is executing was admitted as one group.
///
/// A budgeted restore admits a tensor together with the worker width of its
/// shard and tile runners, so the nested runners must not admit a second time
/// (that would count the same threads twice and could deadlock against the
/// outer admission). This flag makes that rule explicit and testable rather
/// than implied.
pub fn in_admitted_group() -> bool {
    GROUP_DEPTH.with(|depth| depth.get() > 0)
}

/// Marks the calling thread as running inside an admitted group.
#[must_use = "the group scope ends when the guard is dropped"]
pub(crate) struct GroupScope(());

impl Drop for GroupScope {
    fn drop(&mut self) {
        GROUP_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Enter an admitted group for the calling thread.
pub(crate) fn enter_group() -> GroupScope {
    GROUP_DEPTH.with(|depth| depth.set(depth.get() + 1));
    GroupScope(())
}

thread_local! {
    static GROUP_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// The budget is shared across threads by construction.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<StageBudget>();
    assert_send_sync::<Admission>();
    assert_send_sync::<StageCancelToken>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn a_zero_policy_is_rejected() {
        assert_eq!(
            StageBudget::with_capacity(0, 1024).unwrap_err().class(),
            crate::error::Class::InvalidArgument
        );
        assert_eq!(
            StageBudget::with_capacity(4, 0).unwrap_err().class(),
            crate::error::Class::InvalidArgument
        );
    }

    #[test]
    fn the_worker_cap_is_never_exceeded_and_oversized_groups_are_clamped() {
        let budget = StageBudget::with_capacity(4, 1 << 20).unwrap();
        let observed = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let budget = Arc::clone(&budget);
            let observed = Arc::clone(&observed);
            let active = Arc::clone(&active);
            handles.push(thread::spawn(move || {
                // A request for 16 workers is clamped to the cap, never denied
                // and never allowed to exceed it.
                let admission = budget.admit(Stage::Restore, 16, 4096, None).unwrap();
                assert!(admission.workers() <= 4);
                let now =
                    active.fetch_add(admission.workers(), Ordering::SeqCst) + admission.workers();
                observed.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                active.fetch_sub(admission.workers(), Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(
            observed.load(Ordering::SeqCst) <= 4,
            "observed {observed:?}"
        );
        let stats = budget.stats();
        assert!(stats.observed_max_active_workers <= 4);
        assert_eq!(stats.clamped_worker_requests, 8);
        assert!(stats.observed_max_waiters > 1, "{stats:?}");
    }

    #[test]
    fn a_byte_request_larger_than_the_budget_runs_alone_and_completes() {
        let budget = StageBudget::with_capacity(8, 1 << 20).unwrap();
        let big = budget.admit(Stage::Restore, 1, 4 << 20, None).unwrap();
        assert!(big.oversized());
        let started = Instant::now();
        let budget2 = Arc::clone(&budget);
        let waiter = thread::spawn(move || {
            // A normal request must wait behind the oversize admission.
            let admission = budget2.admit(Stage::Restore, 1, 1024, None).unwrap();
            assert!(!admission.oversized());
            started.elapsed()
        });
        thread::sleep(Duration::from_millis(30));
        drop(big);
        let waited = waiter.join().unwrap();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert_eq!(budget.stats().oversized_admissions, 1);
    }

    #[test]
    fn cancellation_releases_the_ticket_and_returns_cancelled() {
        let budget = StageBudget::with_capacity(4, 1 << 20).unwrap();
        let holder = budget.admit(Stage::Restore, 4, 1 << 20, None).unwrap();
        let token = budget.cancel_token();
        let budget2 = Arc::clone(&budget);
        let token2 = token.clone();
        let waiter = thread::spawn(move || {
            budget2
                .admit(Stage::Restore, 1, 1, Some(&token2))
                .map(|admission| admission.workers())
        });
        // Wait until the worker is really queued.
        while waiting_now(&budget) == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        token.cancel();
        let outcome = waiter.join().unwrap();
        assert_eq!(outcome.unwrap_err().class(), crate::error::Class::Cancelled);
        // The cancelled ticket must not block the queue: a later request is
        // served as soon as capacity is free.
        drop(holder);
        let after = budget
            .admit(Stage::Restore, 4, 1 << 20, None)
            .expect("queue advanced past the cancelled ticket");
        assert_eq!(after.workers(), 4);
        let stats = budget.stats();
        assert_eq!(stats.cancelled_waiters, 1);
    }

    #[test]
    fn a_panicking_holder_releases_its_reservation() {
        let budget = StageBudget::with_capacity(2, 1 << 20).unwrap();
        let budget2 = Arc::clone(&budget);
        let outcome = thread::spawn(move || {
            let _admission = budget2.admit(Stage::Restore, 1, 1 << 20, None).unwrap();
            panic!("stage panicked while holding an admission");
        })
        .join();
        assert!(outcome.is_err());
        let admission = budget
            .admit(Stage::Restore, 2, 1 << 20, None)
            .expect("the panicked holder's reservation must be released");
        assert_eq!(admission.workers(), 2);
    }

    #[test]
    fn releasing_workers_keeps_the_byte_reservation() {
        let budget = StageBudget::with_capacity(1, 64).unwrap();
        let mut first = budget.admit(Stage::Restore, 1, 64, None).unwrap();
        first.release_workers();
        // A second worker may now decode, but its bytes must wait.
        let budget2 = Arc::clone(&budget);
        let second = thread::spawn(move || {
            let mut admission = budget2.admit(Stage::Restore, 1, 1, None).unwrap();
            admission.release_workers();
            admission.workers()
        });
        // Wait for the second to grant and exit, then check bytes are intact.
        second.join().unwrap();
        assert_eq!(budget.stats().observed_max_inflight_bytes, 64);
        drop(first);
    }

    #[test]
    fn fifo_serves_the_earliest_waiter_first() {
        let budget = StageBudget::with_capacity(1, 1 << 20).unwrap();
        let held = budget.admit(Stage::Restore, 1, 1024, None).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for index in 0..4u32 {
            let budget = Arc::clone(&budget);
            let order = Arc::clone(&order);
            handles.push(thread::spawn(move || {
                let _admission = budget.admit(Stage::Restore, 1, 1024, None).unwrap();
                lock(&order).push(index);
            }));
            // Space the arrivals so ticket order is the arrival order.
            thread::sleep(Duration::from_millis(10));
        }
        drop(held);
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(*lock(&order), vec![0, 1, 2, 3]);
    }

    #[test]
    fn the_declared_default_comes_from_available_parallelism() {
        let policy = StageBudgetPolicy::from_parallelism();
        assert!(policy.max_active_workers >= 1);
        assert_eq!(policy.max_inflight_bytes, DEFAULT_MAX_INFLIGHT_BYTES);
        policy.validate().unwrap();
    }
}
