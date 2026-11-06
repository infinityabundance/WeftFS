//! In-process single-flight for identical reconstruction work.
//!
//! Concurrent readers of the same reconstruction identity (object id + content
//! or representation version + requested region) share one backend read and one
//! decode. The first caller becomes the **leader** and runs the operation; any
//! caller that arrives while it runs becomes a **waiter** and receives a clone
//! of the leader's result (or its error).
//!
//! Design constraints (R8):
//!
//! * **No lock is held while work runs.** `run` drops the registry lock before
//!   invoking the operation, so a decode may take the backend mutex, call back
//!   into this registry for a dependency, or spawn scoped threads.
//! * **Bounded.** At most [`SingleFlightOptions::max_entries`] flights and
//!   [`SingleFlightOptions::max_bytes`] reserved decoded bytes may be in flight.
//!   A request that cannot be admitted runs independently instead: the layer
//!   degrades, it never queues without bound and it never deadlocks.
//! * **Cancellation is a waiter's decision only.** A waiter that cancels is
//!   returned [`crate::Class::Cancelled`] and stops waiting; the leader keeps
//!   running for every other waiter. A token that is already cancelled before
//!   a caller becomes leader simply declines to start.
//! * **Errors fan out.** Every waiter (and the leader) observes the same error.
//! * **Panic safe.** A panicking operation is caught and published as
//!   `Corrupt`, so waiters can never hang on a flight that will never finish.
//! * **No `unsafe`.** `Mutex` + `Condvar` only, so a `&Store` can be shared.
//!
//! The registry is deliberately not persistent and not cross-process: it only
//! coalesces work inside one open store handle.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::identity::Hash32;

/// How often a cancellable waiter re-examines its token.
pub const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Default bounded flight count. Small on purpose: each flight holds one
/// decoded result until its last waiter has copied it.
pub const DEFAULT_MAX_ENTRIES: usize = 64;

/// Default bound on reserved decoded bytes across all flights: 256 MiB.
pub const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Configuration for [`SingleFlight`]. Disabled by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SingleFlightOptions {
    pub enabled: bool,
    pub max_entries: usize,
    pub max_bytes: u64,
}

impl Default for SingleFlightOptions {
    fn default() -> Self {
        SingleFlightOptions {
            enabled: false,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl SingleFlightOptions {
    /// Reject a configuration that could not bound anything.
    pub fn validate(&self) -> Result<()> {
        if self.max_entries == 0 {
            return Err(Error::invalid_argument(
                "single-flight max_entries must be at least 1",
            ));
        }
        if self.max_bytes == 0 {
            return Err(Error::invalid_argument(
                "single-flight max_bytes must be at least 1",
            ));
        }
        Ok(())
    }
}

/// The byte region a reconstruction identity names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegionKey {
    Whole,
    Bytes { offset: u64, length: u64 },
}

impl RegionKey {
    /// The region for a read of `length` bytes at `offset` over an object of
    /// `size` bytes. A read that covers the whole object is [`RegionKey::Whole`]
    /// so a whole read and a range read of the entire object coalesce.
    pub fn for_read(offset: u64, length: u64, size: u64) -> Self {
        if offset == 0 && length >= size {
            RegionKey::Whole
        } else {
            RegionKey::Bytes { offset, length }
        }
    }

    /// Logical bytes this read may return; used as the reservation hint.
    /// A range is capped at the object size so a caller cannot reserve more
    /// than the object could ever produce.
    pub fn bytes_hint(&self, size: u64) -> u64 {
        match self {
            RegionKey::Whole => size,
            RegionKey::Bytes { length, .. } => *length,
        }
        .min(size)
        .max(1)
    }
}

/// A validated reconstruction identity.
///
/// `object` is the ObjectId (or the content root when a caller only has one),
/// `content` is the manifest / representation identity. A different version of
/// an object has a different `content`, so old in-flight work and cache entries
/// can never be confused with the new version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReconstructionKey {
    pub object: Hash32,
    pub content: Hash32,
    pub region: RegionKey,
    /// Whether this read verifies the whole-content hash. A verifying and a
    /// non-verifying read of the same bytes do different work, so they are
    /// distinct identities.
    pub verified: bool,
}

impl ReconstructionKey {
    pub fn whole(object: Hash32, content: Hash32) -> Self {
        ReconstructionKey {
            object,
            content,
            region: RegionKey::Whole,
            verified: true,
        }
    }
}

/// A cooperative cancellation handle. Cloning shares the same flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask every waiter that holds a clone to stop waiting.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct FlightState<V> {
    done: bool,
    result: Option<Result<V>>,
    waiters: usize,
}

struct Flight<V> {
    state: Mutex<FlightState<V>>,
    ready: Condvar,
}

impl<V> Flight<V> {
    fn new() -> Self {
        Flight {
            state: Mutex::new(FlightState {
                done: false,
                result: None,
                waiters: 0,
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FlightState<V>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct Registry<K, V> {
    flights: BTreeMap<K, Arc<Flight<V>>>,
    reserved_bytes: u64,
}

/// What the registry has done, for tests and receipts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SingleFlightStats {
    pub enabled: bool,
    pub max_entries: usize,
    pub max_bytes: u64,
    /// Operations that ran as the single leader for their identity.
    pub leaders: u64,
    /// Callers that joined an existing flight.
    pub waiters: u64,
    /// Waiters that returned `Cancelled` while their leader kept running.
    pub cancellations: u64,
    /// Requests that ran independently because a bound was already reached.
    pub degraded: u64,
    /// Leaders whose operation panicked (published as `Corrupt`).
    pub panics: u64,
    pub in_flight: usize,
    pub reserved_bytes: u64,
    pub max_observed_entries: usize,
}

/// A bounded registry of in-flight reconstructions.
pub struct SingleFlight<K, V> {
    options: Mutex<SingleFlightOptions>,
    registry: Mutex<Registry<K, V>>,
    leaders: AtomicU64,
    waiters: AtomicU64,
    cancellations: AtomicU64,
    degraded: AtomicU64,
    panics: AtomicU64,
    max_observed_entries: AtomicU64,
}

impl<K, V> SingleFlight<K, V>
where
    K: Ord + Clone,
    V: Clone + Send,
{
    pub fn new(options: SingleFlightOptions) -> Self {
        let options = if options.validate().is_ok() {
            options
        } else {
            SingleFlightOptions::default()
        };
        SingleFlight {
            options: Mutex::new(options),
            registry: Mutex::new(Registry {
                flights: BTreeMap::new(),
                reserved_bytes: 0,
            }),
            leaders: AtomicU64::new(0),
            waiters: AtomicU64::new(0),
            cancellations: AtomicU64::new(0),
            degraded: AtomicU64::new(0),
            panics: AtomicU64::new(0),
            max_observed_entries: AtomicU64::new(0),
        }
    }

    /// Replace the configuration. Callers must only do this while the store is
    /// not being read (the store applies it once, at open).
    pub fn set_options(&self, options: SingleFlightOptions) {
        if options.validate().is_err() {
            return;
        }
        *self
            .options
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = options;
    }

    pub fn enabled(&self) -> bool {
        self.options
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .enabled
    }

    pub fn stats(&self) -> SingleFlightStats {
        let options = *self
            .options
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        SingleFlightStats {
            enabled: options.enabled,
            max_entries: options.max_entries,
            max_bytes: options.max_bytes,
            leaders: self.leaders.load(Ordering::Relaxed),
            waiters: self.waiters.load(Ordering::Relaxed),
            cancellations: self.cancellations.load(Ordering::Relaxed),
            degraded: self.degraded.load(Ordering::Relaxed),
            panics: self.panics.load(Ordering::Relaxed),
            in_flight: registry.flights.len(),
            reserved_bytes: registry.reserved_bytes,
            max_observed_entries: usize::try_from(
                self.max_observed_entries.load(Ordering::Relaxed),
            )
            .unwrap_or(usize::MAX),
        }
    }

    /// Coalesce `op` with every caller that names the same `key` while it runs.
    ///
    /// When the layer is disabled, `op` runs unconditionally (the shipped
    /// behaviour). Every bound breach also runs `op` independently, after
    /// recording a `degraded` count.
    pub fn run<F>(&self, key: K, bytes_hint: u64, cancel: Option<&CancelToken>, op: F) -> Result<V>
    where
        F: FnOnce() -> Result<V>,
    {
        let options = *self
            .options
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !options.enabled {
            return op();
        }
        // A caller that already gave up must not start new work. Once a caller
        // is the leader it never re-checks the token: other waiters depend on
        // its result.
        if cancel.map(|token| token.is_cancelled()).unwrap_or(false) {
            self.cancellations.fetch_add(1, Ordering::Relaxed);
            return Err(cancelled_error());
        }

        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(flight) = registry.flights.get(&key).cloned() {
            {
                let mut state = flight.lock();
                state.waiters += 1;
            }
            self.waiters.fetch_add(1, Ordering::Relaxed);
            drop(registry);
            return self.await_flight(flight, cancel);
        }

        let hint = bytes_hint.max(1);
        let too_many_entries = registry.flights.len() >= options.max_entries;
        let too_many_bytes = registry.reserved_bytes.saturating_add(hint) > options.max_bytes;
        if too_many_entries || too_many_bytes {
            drop(registry);
            self.degraded.fetch_add(1, Ordering::Relaxed);
            return op();
        }

        let flight = Arc::new(Flight::new());
        registry.flights.insert(key.clone(), Arc::clone(&flight));
        registry.reserved_bytes = registry.reserved_bytes.saturating_add(hint);
        let observed = u64::try_from(registry.flights.len()).unwrap_or(u64::MAX);
        let _ = self.max_observed_entries.try_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| {
                if current < observed {
                    Some(observed)
                } else {
                    None
                }
            },
        );
        drop(registry);
        self.leaders.fetch_add(1, Ordering::Relaxed);

        let result = match catch_unwind(AssertUnwindSafe(op)) {
            Ok(result) => result,
            Err(_) => {
                self.panics.fetch_add(1, Ordering::Relaxed);
                Err(Error::corrupt(
                    "reconstruction panicked inside single-flight",
                ))
            }
        };
        {
            let mut state = flight.lock();
            state.result = Some(result.clone());
            state.done = true;
            flight.ready.notify_all();
        }
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.flights.remove(&key);
        registry.reserved_bytes = registry.reserved_bytes.saturating_sub(hint);
        drop(registry);
        result
    }

    fn await_flight(&self, flight: Arc<Flight<V>>, cancel: Option<&CancelToken>) -> Result<V> {
        let mut state = flight.lock();
        loop {
            if state.done {
                return state.result.clone().unwrap_or_else(|| {
                    Err(Error::corrupt("single-flight finished without a result"))
                });
            }
            if let Some(token) = cancel {
                if token.is_cancelled() {
                    state.waiters = state.waiters.saturating_sub(1);
                    self.cancellations.fetch_add(1, Ordering::Relaxed);
                    return Err(cancelled_error());
                }
                let (guard, _) = flight
                    .ready
                    .wait_timeout(state, CANCEL_POLL_INTERVAL)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state = guard;
            } else {
                state = flight
                    .ready
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        }
    }
}

fn cancelled_error() -> Error {
    Error::cancelled("read cancelled by the caller while waiting for an identical reconstruction")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Barrier;

    fn key(byte: u8) -> ReconstructionKey {
        let mut raw = [0u8; 32];
        raw[0] = byte;
        raw[31] = byte;
        ReconstructionKey {
            object: Hash32::from_bytes(raw),
            content: Hash32::from_bytes(raw),
            region: RegionKey::Whole,
            verified: true,
        }
    }

    #[test]
    fn disabled_runs_every_call_independently() {
        let flight: SingleFlight<ReconstructionKey, Vec<u8>> =
            SingleFlight::new(SingleFlightOptions::default());
        let calls = AtomicUsize::new(0);
        for _ in 0..3 {
            flight
                .run(key(1), 4, None, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![1, 2, 3, 4])
                })
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(flight.stats().leaders, 0);
    }

    #[test]
    fn concurrent_identical_calls_run_once() {
        let flight: SingleFlight<ReconstructionKey, Vec<u8>> =
            SingleFlight::new(SingleFlightOptions {
                enabled: true,
                ..SingleFlightOptions::default()
            });
        let calls = AtomicUsize::new(0);
        let barrier = Barrier::new(4);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let barrier = &barrier;
                let flight = &flight;
                let calls = &calls;
                scope.spawn(move || {
                    barrier.wait();
                    let out = flight
                        .run(key(7), 4, None, || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(50));
                            Ok(vec![7; 4])
                        })
                        .unwrap();
                    assert_eq!(out, vec![7; 4]);
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let stats = flight.stats();
        assert_eq!(stats.leaders, 1);
        assert_eq!(stats.waiters, 3);
        assert_eq!(stats.in_flight, 0);
    }

    #[test]
    fn distinct_keys_are_not_coalesced() {
        let flight: SingleFlight<ReconstructionKey, Vec<u8>> =
            SingleFlight::new(SingleFlightOptions {
                enabled: true,
                ..SingleFlightOptions::default()
            });
        let barrier = Barrier::new(2);
        std::thread::scope(|scope| {
            for byte in [1u8, 2] {
                let barrier = &barrier;
                let flight = &flight;
                scope.spawn(move || {
                    barrier.wait();
                    flight
                        .run(key(byte), 1, None, || {
                            std::thread::sleep(Duration::from_millis(30));
                            Ok(vec![byte])
                        })
                        .unwrap();
                });
            }
        });
        let stats = flight.stats();
        assert_eq!(stats.leaders, 2);
        assert_eq!(stats.waiters, 0);
    }

    #[test]
    fn cancelling_waiter_does_not_cancel_the_leader() {
        let flight: SingleFlight<ReconstructionKey, Vec<u8>> =
            SingleFlight::new(SingleFlightOptions {
                enabled: true,
                ..SingleFlightOptions::default()
            });
        let token = CancelToken::new();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let leader = scope.spawn(|| {
                flight
                    .run(key(9), 1, None, || {
                        started_tx.send(()).unwrap();
                        std::thread::sleep(Duration::from_millis(200));
                        Ok(vec![9])
                    })
                    .unwrap()
            });
            started_rx.recv().unwrap();
            let waiter_token = token.clone();
            let flight_ref = &flight;
            let waiter = scope.spawn(move || {
                flight_ref.run(key(9), 1, Some(&waiter_token), || Ok(vec![0]))
            });
            std::thread::sleep(Duration::from_millis(30));
            token.cancel();
            let err = waiter.join().unwrap().unwrap_err();
            assert_eq!(err.class(), crate::Class::Cancelled);
            assert_eq!(leader.join().unwrap(), vec![9]);
        });
        assert_eq!(flight.stats().cancellations, 1);
    }

    #[test]
    fn errors_reach_every_waiter() {
        let flight: SingleFlight<ReconstructionKey, Vec<u8>> =
            SingleFlight::new(SingleFlightOptions {
                enabled: true,
                ..SingleFlightOptions::default()
            });
        let barrier = Barrier::new(3);
        let classes = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..3 {
                let barrier = &barrier;
                let flight = &flight;
                let classes = &classes;
                scope.spawn(move || {
                    barrier.wait();
                    let err = flight
                        .run(key(3), 1, None, || {
                            std::thread::sleep(Duration::from_millis(40));
                            Err(Error::corrupt("synthetic decode failure"))
                        })
                        .unwrap_err();
                    classes.lock().unwrap().push(err.class());
                });
            }
        });
        let classes = classes.into_inner().unwrap();
        assert_eq!(classes.len(), 3);
        assert!(classes.iter().all(|class| *class == crate::Class::Corrupt));
        assert_eq!(flight.stats().leaders, 1);
    }

    #[test]
    fn entry_and_byte_bounds_degrade_instead_of_queueing() {
        // One entry, but two distinct keys are requested concurrently.
        let flight: SingleFlight<ReconstructionKey, Vec<u8>> =
            SingleFlight::new(SingleFlightOptions {
                enabled: true,
                max_entries: 1,
                max_bytes: 1024,
            });
        let barrier = Barrier::new(2);
        std::thread::scope(|scope| {
            for byte in [4u8, 5] {
                let barrier = &barrier;
                let flight = &flight;
                scope.spawn(move || {
                    barrier.wait();
                    flight
                        .run(key(byte), 8, None, || {
                            std::thread::sleep(Duration::from_millis(40));
                            Ok(vec![byte; 8])
                        })
                        .unwrap();
                });
            }
        });
        let stats = flight.stats();
        assert_eq!(stats.leaders, 1);
        assert_eq!(stats.degraded, 1);
        assert_eq!(stats.max_observed_entries, 1);
    }

    #[test]
    fn byte_bound_degrades_an_oversized_request() {
        let flight: SingleFlight<ReconstructionKey, Vec<u8>> =
            SingleFlight::new(SingleFlightOptions {
                enabled: true,
                max_entries: 4,
                max_bytes: 16,
            });
        let out = flight
            .run(key(6), 1024, None, || Ok(vec![6; 1024]))
            .unwrap();
        assert_eq!(out.len(), 1024);
        let stats = flight.stats();
        assert_eq!(stats.leaders, 0);
        assert_eq!(stats.degraded, 1);
    }

    #[test]
    fn panicking_operation_publishes_an_error() {
        let flight: SingleFlight<ReconstructionKey, Vec<u8>> =
            SingleFlight::new(SingleFlightOptions {
                enabled: true,
                ..SingleFlightOptions::default()
            });
        let err = flight
            .run(key(8), 1, None, || -> Result<Vec<u8>> {
                panic!("synthetic decode panic");
            })
            .unwrap_err();
        assert_eq!(err.class(), crate::Class::Corrupt);
        assert_eq!(flight.stats().panics, 1);
        assert_eq!(flight.stats().in_flight, 0);
    }
}
