//! X09 — a durable writer **session** lease with a real expiry and a fair queue.
//!
//! # The defect this closes
//!
//! Before this module a writer's exclusivity was the kernel lock it held for the
//! whole session (MW1) or the conditional head compare-and-swap of MW2. Neither
//! has a *lifetime*: an MW1 holder that is alive but idle blocks every other
//! writer for as long as the process exists, and an MW2 writer holds no lease at
//! all, so there is nothing a successor could observe as "this session has gone
//! quiet". The provider planes (S3/GCS/Azure) already fence a session through a
//! lease record, but only the *publication* path consulted it and only a
//! publication renewed it: a writer that stopped publishing stayed live until
//! its deadline, and a writer that never published was never renewed at all.
//!
//! # The mechanism
//!
//! `writer/lease` is one canonical control record,
//! [`LeaseRecord`] (`weft.writer.lease.v1`), replaced by compare-and-swap. It
//! carries the owner, a monotonically increasing `fence`, a `sequence` (renewal
//! counter), an **absolute** `deadline_ns` and the last head the writer
//! published. The invariants:
//!
//! * **Acquisition** replaces the record only when the observed record is absent
//!   or its deadline has passed. The new `fence` is `observed.fence + 1`, so a
//!   successor is always distinguishable from the writer it replaced.
//! * **Renewal** is explicit and durable ([`Store::renew_writer_lease`]) and is
//!   also performed inline by every publication when the remaining lifetime is
//!   at most half the TTL. A `LeaseKeeper` thread renews an *idle* session on a
//!   timer, which is what makes "a live renewing holder is never stolen" true
//!   even when the session publishes nothing.
//! * **Expiry** is a property of the record, not of a request. A session's own
//!   deadline is checked before any mutation and again inside the publication
//!   critical section. An expired or superseded holder gets
//!   [`Class::LeaseExpired`] and **mutates nothing**.
//! * **Fencing.** Every publication re-reads the lease *inside* the publication
//!   critical section (a POSIX plane's `LOCK.publish`, a provider's conditional
//!   write) and refuses when the record no longer names this handle at this
//!   fence. A resurrected stale writer therefore cannot publish after a
//!   successor has taken over. There is no path in which two writers both
//!   publish believing they hold the lease.
//! * **Release** is a content-addressed tombstone (`state = "released"`,
//!   `released_from` = token of the bytes it replaced) written by
//!   compare-and-swap. A graceful close releases immediately; a killed holder
//!   leaves a live record that expires at its deadline. Never an unconditional
//!   delete, so a successor's record is never removed by a straggler.
//!
//! # Fairness
//!
//! Acquisition is a queue, not a race. A contender joins by creating the
//! one-next ticket (`writer/tickets/<ticket>`), and may take the lease only when
//! **no live ticket with a lower number exists**. Tickets are created once
//! (`put_atomic`) and deleted on leave; a ticket whose deadline has passed is
//! ignored (and may be collected), so a crashed waiter cannot block the store
//! forever. The ordering guarantee is therefore:
//!
//! > Among contenders that are live at the same instant, the lease is granted in
//! > ascending ticket order, and a contender that arrives after a waiter always
//! > receives a higher ticket than that waiter.
//!
//! A stream of short-lived writers can therefore not starve a waiter: each
//! newcomer queues behind it. This is deliberately *stricter* than the store's
//! optimistic head CAS, which lets a lucky writer pass a waiter indefinitely.
//! The cost is one listing and one create per acquisition; §3 of
//! `bench/RESULTS-writer-lease.md` measures it.
//!
//! # What this is not
//!
//! * It is not a distributed consensus protocol. The clock is the local wall
//!   clock (as in `src/s3.rs`), so clock skew between hosts is not defended
//!   against; the courts are one host.
//! * It does not replace MW1's kernel lock. A *leased* session is an MW2
//!   (`optimistic`) session by construction, because MW1's authority is the
//!   kernel lock and a live kernel lock cannot expire.
//! * A lease is not an authorization decision. Anyone who can write the store
//!   can write the record.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use weftcodec::wcb::Value;

use crate::error::{Class, Error, Result};
use crate::identity::Hash32;
use crate::wcb_util as wu;

/// Schema of the writer lease control record.
pub const LEASE_SCHEMA: &str = "weft.writer.lease.v1";
/// Schema of one fairness ticket.
pub const TICKET_SCHEMA: &str = "weft.writer.ticket.v1";
/// Default session lifetime when the operator enables a lease but names none.
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
/// Hard lower bound: a lease shorter than this cannot cover one publication on
/// a slow endpoint, and the fencing courts measured that exact failure
/// (`verify-20261106T170554Z`: a 1 s lease expired mid-publication on Ceph RGW).
pub const MIN_LEASE_TTL: Duration = Duration::from_secs(1);
/// Hard upper bound on a configured lifetime.
pub const MAX_LEASE_TTL: Duration = Duration::from_secs(86_400);
/// Default lifetime of a queue ticket. A waiter that dies stops blocking after
/// this, without any garbage-collection pass. A ticket outlives one lease TTL on
/// purpose: a waiter may have to wait through several renewals of the holder.
pub const DEFAULT_TICKET_TTL: Duration = Duration::from_secs(10);
/// Hard upper bound on a configured ticket lifetime.
pub const MAX_TICKET_TTL: Duration = Duration::from_secs(3_600);
/// Bound on live tickets a single acquisition will read.
pub const MAX_TICKETS: usize = 4096;
/// Bound on acquisition attempts before a typed `Busy` refusal. The *wait*
/// budget is the real bound; this only stops a pathological loop from spinning
/// forever when the budget is very long. 4,096 attempts at the 25 ms cap is
/// about 100 s, far above any sane `wait`.
pub const MAX_ACQUIRE_ATTEMPTS: u32 = 4096;
/// Default bound on how long an acquisition will wait for the queue.
pub const DEFAULT_ACQUIRE_WAIT: Duration = Duration::from_secs(5);

/// How a writer session acquires and keeps its lease.
#[derive(Clone, Debug)]
pub struct WriterLeaseOptions {
    /// Absolute lifetime granted by one acquisition or renewal.
    pub ttl: Duration,
    /// Spawn a keeper thread that renews an idle session every `ttl / 3`.
    pub keeper: bool,
    /// Enforce the FIFO ticket queue.
    pub fairness: bool,
    /// Lifetime of a queue ticket. Ignored when `fairness` is false.
    pub ticket_ttl: Duration,
    /// How long acquisition waits for the queue and for a live lease to expire.
    pub wait: Duration,
}

impl Default for WriterLeaseOptions {
    fn default() -> Self {
        WriterLeaseOptions {
            ttl: DEFAULT_LEASE_TTL,
            keeper: true,
            fairness: true,
            ticket_ttl: DEFAULT_TICKET_TTL,
            wait: DEFAULT_ACQUIRE_WAIT,
        }
    }
}

impl WriterLeaseOptions {
    /// Validate the bounds once, at open, so a bad configuration fails before a
    /// single byte is written.
    pub fn validate(&self) -> Result<()> {
        if self.ttl < MIN_LEASE_TTL || self.ttl > MAX_LEASE_TTL {
            return Err(Error::invalid_argument(format!(
                "writer lease TTL must be {}..={} seconds, got {} ms",
                MIN_LEASE_TTL.as_secs(),
                MAX_LEASE_TTL.as_secs(),
                self.ttl.as_millis()
            )));
        }
        if self.ticket_ttl.is_zero() || self.ticket_ttl > MAX_TICKET_TTL {
            return Err(Error::invalid_argument(format!(
                "writer lease ticket TTL must be 1 ms..={} s",
                MAX_TICKET_TTL.as_secs()
            )));
        }
        Ok(())
    }

    /// Renewal interval of the keeper thread.
    pub fn renew_interval(&self) -> Duration {
        // A third of the lifetime leaves room for two failed attempts before the
        // deadline. Never zero.
        let third = self.ttl / 3;
        if third.is_zero() {
            Duration::from_millis(1)
        } else {
            third
        }
    }
}

/// State of a lease record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseState {
    /// A writer holds the lease (possibly past its deadline).
    Held,
    /// The holder released it; a successor may take over immediately.
    Released,
}

impl LeaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            LeaseState::Held => "held",
            LeaseState::Released => "released",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "held" => Ok(LeaseState::Held),
            "released" => Ok(LeaseState::Released),
            other => Err(Error::malformed(format!(
                "writer lease state `{other}` is not `held` or `released`"
            ))),
        }
    }
}

/// One decoded `writer/lease` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseRecord {
    /// Session identity of the holder.
    pub owner: String,
    /// Fencing token. Monotonically increases on every takeover.
    pub fence: u64,
    /// Renewal counter within one tenure.
    pub sequence: u64,
    /// Instant the tenure began.
    pub acquired_ns: i64,
    /// Instant of the most recent renewal.
    pub renewed_ns: i64,
    /// Absolute deadline. A contender may take over at or after this instant.
    pub deadline_ns: i64,
    /// Lifetime granted by the acquisition that produced this record.
    pub ttl_ns: i64,
    /// Held or released.
    pub state: LeaseState,
    /// Token of the bytes this record replaced when it is a release tombstone.
    pub released_from: Option<String>,
    /// A head this writer attests it published (advisory; never a root).
    pub head_generation: u64,
    /// Commit hash of that head.
    pub head_commit: Option<Hash32>,
    /// Encoded length of that commit record.
    pub head_commit_len: u64,
}

impl LeaseRecord {
    /// Whether the record is a live hold at `now_ns`.
    pub fn is_live(&self, now_ns: i64) -> bool {
        self.state == LeaseState::Held && now_ns < self.deadline_ns
    }

    /// Whether a contender may take over at `now_ns`: a released tombstone or a
    /// held record whose absolute deadline has passed.
    pub fn may_take_over(&self, now_ns: i64) -> bool {
        self.state == LeaseState::Released || now_ns >= self.deadline_ns
    }

    /// Remaining lifetime in milliseconds (may be negative).
    pub fn remaining_ms(&self, now_ns: i64) -> i64 {
        (self.deadline_ns - now_ns) / 1_000_000
    }
}

/// Token of a lease record's bytes, used as the compare-and-swap expectation.
pub fn lease_token(bytes: &[u8]) -> String {
    crate::planes::control_token(bytes)
}

/// Encode a lease record as canonical WCB.
pub fn encode_lease(record: &LeaseRecord) -> Result<Vec<u8>> {
    validate_record(record)?;
    let value = wu::map([
        ("schema", Value::Str(LEASE_SCHEMA.to_owned())),
        ("owner", Value::Str(record.owner.clone())),
        ("fence", Value::U64(record.fence)),
        ("sequence", Value::U64(record.sequence)),
        ("acquired_ns", Value::Timestamp(record.acquired_ns)),
        ("renewed_ns", Value::Timestamp(record.renewed_ns)),
        ("deadline_ns", Value::Timestamp(record.deadline_ns)),
        ("ttl_ns", Value::U64(record.ttl_ns.max(0) as u64)),
        ("state", Value::Str(record.state.as_str().to_owned())),
        (
            "released_from",
            match &record.released_from {
                Some(token) => Value::Str(token.clone()),
                None => Value::Null,
            },
        ),
        ("head_generation", Value::U64(record.head_generation)),
        ("head_commit", wu::opt_hash_value(record.head_commit)),
        ("head_commit_len", Value::U64(record.head_commit_len)),
    ]);
    wu::encode(&value)
}

/// Decode a lease record, rejecting any byte sequence that is not canonical.
pub fn decode_lease(bytes: &[u8]) -> Result<LeaseRecord> {
    let value = wu::decode(bytes)?;
    let map = wu::as_map(&value)?;
    wu::check_keys(
        map,
        &[
            "schema",
            "owner",
            "fence",
            "sequence",
            "acquired_ns",
            "renewed_ns",
            "deadline_ns",
            "ttl_ns",
            "state",
            "released_from",
            "head_generation",
            "head_commit",
            "head_commit_len",
        ],
    )?;
    let schema = wu::get_text(map, "schema")?;
    if schema != LEASE_SCHEMA {
        return Err(Error::unsupported_version(format!(
            "writer lease schema `{schema}` is not `{LEASE_SCHEMA}`"
        )));
    }
    let released_from = match wu::field(map, "released_from")? {
        Value::Null => None,
        Value::Str(token) => Some(token.clone()),
        _ => {
            return Err(Error::malformed(
                "writer lease `released_from` must be a string or null".to_owned(),
            ))
        }
    };
    let record = LeaseRecord {
        owner: wu::get_text(map, "owner")?,
        fence: wu::get_u64(map, "fence")?,
        sequence: wu::get_u64(map, "sequence")?,
        acquired_ns: wu::get_timestamp(map, "acquired_ns")?,
        renewed_ns: wu::get_timestamp(map, "renewed_ns")?,
        deadline_ns: wu::get_timestamp(map, "deadline_ns")?,
        ttl_ns: wu::get_u64(map, "ttl_ns")? as i64,
        state: LeaseState::parse(&wu::get_text(map, "state")?)?,
        released_from,
        head_generation: wu::get_u64(map, "head_generation")?,
        head_commit: wu::get_opt_hash(map, "head_commit")?,
        head_commit_len: wu::get_u64(map, "head_commit_len")?,
    };
    validate_record(&record)?;
    Ok(record)
}

fn validate_record(record: &LeaseRecord) -> Result<()> {
    if record.owner.is_empty() || record.owner.len() > 128 {
        return Err(Error::malformed(
            "writer lease owner must be 1..=128 bytes".to_owned(),
        ));
    }
    if record.fence == 0 {
        return Err(Error::malformed(
            "writer lease fence must be at least 1; fence 0 means no record".to_owned(),
        ));
    }
    if record.deadline_ns < record.acquired_ns {
        return Err(Error::malformed(
            "writer lease deadline precedes its acquisition".to_owned(),
        ));
    }
    match (record.state, &record.released_from) {
        (LeaseState::Held, Some(_)) => Err(Error::malformed(
            "a held writer lease cannot carry `released_from`".to_owned(),
        )),
        (LeaseState::Released, None) => Err(Error::malformed(
            "a released writer lease must carry the token it replaced".to_owned(),
        )),
        (LeaseState::Released, Some(token)) if token.is_empty() || token.len() > 128 => Err(
            Error::malformed("writer lease release token must be 1..=128 bytes".to_owned()),
        ),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Fairness tickets
// ---------------------------------------------------------------------------

/// One decoded queue ticket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    /// Ticket number. Ascending order is grant order.
    pub ticket: u64,
    /// Session identity of the waiter.
    pub owner: String,
    /// Instant after which the ticket no longer blocks anybody.
    pub deadline_ns: i64,
}

impl Ticket {
    pub fn is_live(&self, now_ns: i64) -> bool {
        now_ns < self.deadline_ns
    }
}

/// Encode a ticket record.
pub fn encode_ticket(ticket: &Ticket) -> Result<Vec<u8>> {
    if ticket.owner.is_empty() || ticket.owner.len() > 128 {
        return Err(Error::malformed(
            "writer ticket owner must be 1..=128 bytes".to_owned(),
        ));
    }
    let value = wu::map([
        ("schema", Value::Str(TICKET_SCHEMA.to_owned())),
        ("ticket", Value::U64(ticket.ticket)),
        ("owner", Value::Str(ticket.owner.clone())),
        ("deadline_ns", Value::Timestamp(ticket.deadline_ns)),
    ]);
    wu::encode(&value)
}

/// Decode a ticket record.
pub fn decode_ticket(bytes: &[u8]) -> Result<Ticket> {
    let value = wu::decode(bytes)?;
    let map = wu::as_map(&value)?;
    wu::check_keys(map, &["schema", "ticket", "owner", "deadline_ns"])?;
    let schema = wu::get_text(map, "schema")?;
    if schema != TICKET_SCHEMA {
        return Err(Error::unsupported_version(format!(
            "writer ticket schema `{schema}` is not `{TICKET_SCHEMA}`"
        )));
    }
    let ticket = Ticket {
        ticket: wu::get_u64(map, "ticket")?,
        owner: wu::get_text(map, "owner")?,
        deadline_ns: wu::get_timestamp(map, "deadline_ns")?,
    };
    if ticket.owner.is_empty() || ticket.owner.len() > 128 {
        return Err(Error::malformed(
            "writer ticket owner must be 1..=128 bytes".to_owned(),
        ));
    }
    Ok(ticket)
}

// ---------------------------------------------------------------------------
// The handle the store holds
// ---------------------------------------------------------------------------

/// The installed lease of one open handle.
#[derive(Clone, Debug)]
pub struct InstalledLease {
    /// Session identity.
    pub owner: String,
    /// The fencing token this handle owns.
    pub fence: u64,
    /// The renewal count of the record it last wrote.
    pub sequence: u64,
    /// Absolute deadline of the record it last wrote.
    pub deadline_ns: i64,
    /// Compare-and-swap expectation for its own next write.
    pub token: String,
}

impl InstalledLease {
    /// Whether this handle's own deadline has passed.
    pub fn expired(&self, now_ns: i64) -> bool {
        now_ns >= self.deadline_ns
    }
}

/// Diagnostic counters for one handle's lease machinery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeaseStats {
    /// Acquisitions that produced a record (including takeovers).
    pub acquisitions: u64,
    /// Takeovers of an expired or released record.
    pub takeovers: u64,
    /// Renewals written by this handle.
    pub renewals: u64,
    /// Publications refused because the lease was expired or superseded.
    pub refusals: u64,
    /// Tickets joined.
    pub tickets_joined: u64,
    /// Times a ticket made this handle wait for a lower ticket.
    pub queue_waits: u64,
    /// Live lower tickets observed by the most recent acquisition.
    pub max_lower_tickets: u64,
}

/// One handle's writer-lease machinery: options, installed lease, counters.
pub struct WriterLeaseSession {
    options: WriterLeaseOptions,
    owner: String,
    io: LeaseIo,
    installed: Mutex<Option<InstalledLease>>,
    stats: Mutex<LeaseStats>,
    stop: Arc<AtomicBool>,
    keeper: Mutex<Option<std::thread::JoinHandle<()>>>,
    keeper_ticks: AtomicU64,
}

impl std::fmt::Debug for WriterLeaseSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterLeaseSession")
            .field("owner", &self.owner)
            .field("ttl_ms", &self.options.ttl.as_millis())
            .field("keeper", &self.options.keeper)
            .field("fairness", &self.options.fairness)
            .finish_non_exhaustive()
    }
}

impl WriterLeaseSession {
    /// Build a session. No record is written until [`WriterLeaseSession::acquire`].
    pub fn new(options: WriterLeaseOptions, owner: String, io: LeaseIo) -> Result<Arc<Self>> {
        options.validate()?;
        Ok(Arc::new(WriterLeaseSession {
            options,
            owner,
            io,
            installed: Mutex::new(None),
            stats: Mutex::new(LeaseStats::default()),
            stop: Arc::new(AtomicBool::new(false)),
            keeper: Mutex::new(None),
            keeper_ticks: AtomicU64::new(0),
        }))
    }

    pub fn options(&self) -> &WriterLeaseOptions {
        &self.options
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn installed(&self) -> Option<InstalledLease> {
        self.installed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn set_installed(&self, lease: Option<InstalledLease>) {
        *self
            .installed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = lease;
    }

    pub fn stats(&self) -> LeaseStats {
        *self
            .stats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn record(&self, edit: impl FnOnce(&mut LeaseStats)) {
        edit(&mut self.stats.lock().unwrap_or_else(|p| p.into_inner()));
    }

    /// Number of keeper renewals attempted (diagnostic).
    pub fn keeper_ticks(&self) -> u64 {
        self.keeper_ticks.load(Ordering::Relaxed)
    }

    pub(crate) fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    pub(crate) fn store_keeper(&self, handle: std::thread::JoinHandle<()>) {
        *self
            .keeper
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle);
    }

    /// Tell the keeper to stop and join it. Idempotent.
    pub fn stop_keeper(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let handle = self
            .keeper
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    pub(crate) fn bump_keeper_ticks(&self) {
        self.keeper_ticks.fetch_add(1, Ordering::Relaxed);
    }

    /// Acquire the lease, honouring the FIFO queue and waiting up to
    /// `options.wait` for a live lease to expire.
    ///
    /// Terminal failures: `Busy` when a live holder (or a live lower ticket)
    /// still owns the store after the wait budget, `LeaseExpired` never (an
    /// expired *own* lease is a later-operation refusal).
    pub fn acquire(&self, clock: &dyn Fn() -> i64) -> Result<InstalledLease> {
        let started = Instant::now();
        let mut ticket = None;
        if self.options.fairness {
            let now = clock();
            let joined = self
                .io
                .join_queue(&self.owner, self.options.ticket_ttl, now)?;
            self.record(|stats| stats.tickets_joined += 1);
            ticket = Some(joined);
        }
        let mut attempt = 0u32;
        loop {
            let now = clock();
            if let Some(mine) = &ticket {
                let lower = self.io.live_lower_tickets(mine.ticket, now)?;
                if !lower.is_empty() {
                    self.record(|stats| {
                        stats.queue_waits += 1;
                        stats.max_lower_tickets = stats.max_lower_tickets.max(lower.len() as u64);
                    });
                    let remaining = wait_budget(started, self.options.wait);
                    if remaining.is_zero() || attempt >= MAX_ACQUIRE_ATTEMPTS {
                        self.io.leave_queue(mine.ticket);
                        return Err(Error::busy(format!(
                            "writer acquisition is queued behind {} live writer(s) with a lower
                             ticket; nothing was mutated",
                            lower.len()
                        )));
                    }
                    std::thread::sleep(acquire_sleep(attempt, remaining));
                    attempt += 1;
                    continue;
                }
            }
            let observed = self.io.read()?;
            let (expected, fence, takeover) = match &observed {
                None => (None, 1u64, false),
                Some((record, bytes)) => {
                    if !record.may_take_over(now) {
                        let remaining = wait_budget(started, self.options.wait);
                        if remaining.is_zero() || attempt >= MAX_ACQUIRE_ATTEMPTS {
                            if let Some(mine) = &ticket {
                                self.io.leave_queue(mine.ticket);
                            }
                            return Err(Error::busy(format!(
                                "store is held by a live writer lease (fence {}, {} ms left);
                                 nothing was mutated",
                                record.fence,
                                record.remaining_ms(now).max(0)
                            )));
                        }
                        std::thread::sleep(acquire_sleep(attempt, remaining));
                        attempt += 1;
                        continue;
                    }
                    let fence = record
                        .fence
                        .checked_add(1)
                        .ok_or_else(|| Error::limit_exceeded("writer fence would overflow u64"))?;
                    (Some(lease_token(bytes)), fence, true)
                }
            };
            let deadline_ns = now.saturating_add(self.options.ttl.as_nanos() as i64);
            let next = LeaseRecord {
                owner: self.owner.clone(),
                fence,
                sequence: 1,
                acquired_ns: now,
                renewed_ns: now,
                deadline_ns,
                ttl_ns: self.options.ttl.as_nanos().min(i64::MAX as u128) as i64,
                state: LeaseState::Held,
                released_from: None,
                head_generation: 0,
                head_commit: None,
                head_commit_len: 0,
            };
            match self.io.compare_and_swap(expected.as_deref(), &next) {
                Ok(()) => {
                    if let Some(mine) = &ticket {
                        self.io.leave_queue(mine.ticket);
                    }
                    let installed = InstalledLease {
                        owner: self.owner.clone(),
                        fence,
                        sequence: 1,
                        deadline_ns,
                        token: lease_token(&encode_lease(&next)?),
                    };
                    self.set_installed(Some(installed.clone()));
                    self.record(|stats| {
                        stats.acquisitions += 1;
                        if takeover {
                            stats.takeovers += 1;
                        }
                    });
                    return Ok(installed);
                }
                Err(err) if err.class() == Class::Busy => {
                    let remaining = wait_budget(started, self.options.wait);
                    if remaining.is_zero() || attempt >= MAX_ACQUIRE_ATTEMPTS {
                        if let Some(mine) = &ticket {
                            self.io.leave_queue(mine.ticket);
                        }
                        return Err(Error::busy(format!(
                            "writer lease compare-and-swap lost {attempt} time(s); nothing was
                             mutated"
                        )));
                    }
                    std::thread::sleep(acquire_sleep(attempt, remaining));
                    attempt += 1;
                }
                Err(err) => {
                    if let Some(mine) = &ticket {
                        self.io.leave_queue(mine.ticket);
                    }
                    return Err(err);
                }
            }
        }
    }

    /// The fence a publication must pass **inside** its critical section.
    ///
    /// Read-only on purpose: the publication already holds the plane's
    /// publication lock, and re-entering it from a second descriptor in this
    /// process would refuse itself. Renewal is a separate write performed after
    /// the publication (and by the keeper while idle), so the fence cannot
    /// deadlock and cannot be omitted.
    pub fn verify_in_publication(&self, now_ns: i64) -> Result<()> {
        let Some(lease) = self.installed() else {
            return Ok(());
        };
        if lease.expired(now_ns) {
            let now = now_ns;
            self.record(|stats| stats.refusals += 1);
            return Err(lease_expired(format!(
                "this writer's lease expired {} ms ago (fence {}); publication refused before
                 anything was mutated, and the session must be reopened",
                (now - lease.deadline_ns) / 1_000_000,
                lease.fence
            )));
        }
        let observed = self.io.read()?;
        let Some((record, _bytes)) = observed else {
            self.record(|stats| stats.refusals += 1);
            return Err(lease_expired(
                "the writer lease record is absent; this session no longer holds the store",
            ));
        };
        if record.owner != self.owner || record.fence != lease.fence {
            self.record(|stats| stats.refusals += 1);
            return Err(lease_expired(format!(
                "this writer's lease (fence {}) was superseded by fence {} (owner {}); publication
                 refused before anything was mutated",
                lease.fence, record.fence, record.owner
            )));
        }
        if record.state != LeaseState::Held {
            self.record(|stats| stats.refusals += 1);
            return Err(lease_expired(
                "the writer lease record is a release tombstone; this session no longer holds the
                 store",
            ));
        }
        if now_ns >= record.deadline_ns {
            self.record(|stats| stats.refusals += 1);
            return Err(lease_expired(format!(
                "the published writer lease expired {} ms ago (fence {}); publication refused",
                (now_ns - record.deadline_ns) / 1_000_000,
                record.fence
            )));
        }
        Ok(())
    }

    /// Renew when the remaining lifetime is at most half the TTL (or always
    /// when `force`), and record the pending head. Safe to call from any
    /// thread; a lost race is `LeaseExpired`.
    pub fn renew(
        &self,
        now_ns: i64,
        force: bool,
        head: Option<(u64, Hash32, u64)>,
    ) -> Result<bool> {
        let Some(lease) = self.installed() else {
            return Ok(false);
        };
        if lease.expired(now_ns) {
            return Err(lease_expired("the session's own lease deadline has passed"));
        }
        let observed = self.io.read()?;
        let Some((record, bytes)) = observed else {
            return Err(lease_expired("the writer lease record is absent"));
        };
        if record.owner != self.owner || record.fence != lease.fence {
            return Err(lease_expired(format!(
                "this writer's lease (fence {}) was superseded by fence {}",
                lease.fence, record.fence
            )));
        }
        let half = (record.deadline_ns - record.acquired_ns) / 2;
        let remaining = record.deadline_ns - now_ns;
        // A record whose head has not been written yet is refreshed once so a
        // successor can see what this session published.
        let head_unknown = record.head_commit.is_none() && head.is_some();
        if !force && !head_unknown && remaining > half {
            return Ok(false);
        }
        let deadline_ns = now_ns.saturating_add(self.options.ttl.as_nanos() as i64);
        let (head_generation, head_commit, head_commit_len) = match head {
            Some((generation, commit, len)) => (generation, Some(commit), len),
            None => (
                record.head_generation,
                record.head_commit,
                record.head_commit_len,
            ),
        };
        let next = LeaseRecord {
            owner: self.owner.clone(),
            fence: record.fence,
            sequence: record.sequence.saturating_add(1),
            acquired_ns: record.acquired_ns,
            renewed_ns: now_ns,
            deadline_ns,
            ttl_ns: self.options.ttl.as_nanos().min(i64::MAX as u128) as i64,
            state: LeaseState::Held,
            released_from: None,
            head_generation,
            head_commit,
            head_commit_len,
        };
        let token = lease_token(&bytes);
        self.io.compare_and_swap(Some(token.as_str()), &next)?;
        let installed = InstalledLease {
            owner: self.owner.clone(),
            fence: next.fence,
            sequence: next.sequence,
            deadline_ns,
            token: lease_token(&encode_lease(&next)?),
        };
        self.set_installed(Some(installed));
        self.record(|stats| stats.renewals += 1);
        Ok(true)
    }

    /// Publish a release tombstone. Never an unconditional delete: the write is
    /// conditional on the bytes this session last wrote, so a successor's
    /// record is never removed by a straggler.
    pub fn release(&self, now_ns: i64) -> Result<bool> {
        let Some(lease) = self.installed() else {
            return Ok(false);
        };
        let observed = self.io.read()?;
        let Some((record, bytes)) = observed else {
            self.set_installed(None);
            return Ok(false);
        };
        if record.owner != self.owner || record.fence != lease.fence {
            // A successor owns the record; leave it exactly as it is.
            self.set_installed(None);
            return Ok(false);
        }
        let token = lease_token(&bytes);
        let tombstone = LeaseRecord {
            owner: self.owner.clone(),
            fence: record.fence,
            sequence: record.sequence.saturating_add(1),
            acquired_ns: record.acquired_ns,
            renewed_ns: now_ns,
            deadline_ns: record.deadline_ns,
            ttl_ns: record.ttl_ns,
            state: LeaseState::Released,
            released_from: Some(token.clone()),
            head_generation: record.head_generation,
            head_commit: record.head_commit,
            head_commit_len: record.head_commit_len,
        };
        match self.io.compare_and_swap(Some(&token), &tombstone) {
            Ok(()) => {
                self.set_installed(None);
                Ok(true)
            }
            Err(err) if err.class() == Class::Busy => {
                // Lost the race to a successor: its record stays untouched.
                self.set_installed(None);
                Ok(false)
            }
            Err(err) => {
                self.set_installed(None);
                Err(err)
            }
        }
    }
}

/// The wall clock in nanoseconds since the Unix epoch. Used before a `Store`
/// exists (lease acquisition at open) and by the keeper thread.
pub fn wall_now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

/// Deterministic, human-readable session identity.
///
/// `pid` + a process-monotonic counter + the wall clock. Two handles in one
/// process never share an owner, and a new process does not reuse a name.
pub fn session_owner(pid: u32, counter: u64, now_ns: i64) -> String {
    format!("pid-{pid}-{counter}-{now_ns}")
}

/// The typed refusal for an expired or superseded session.
pub fn lease_expired(message: impl Into<String>) -> Error {
    Error::lease_expired(message)
}

/// Exponential backoff between acquisition attempts, capped.
///
/// Same shape as `gc_epoch::epoch_retry_delay`; the critical section being
/// waited on is a single control-record compare-and-swap.
pub fn acquire_retry_delay(attempt: u32) -> Duration {
    let micros = 200u64.saturating_mul(1u64 << attempt.min(12));
    Duration::from_micros(micros.min(25_000))
}

/// How long one acquisition attempt should sleep before retrying.
pub fn acquire_sleep(attempt: u32, remaining: Duration) -> Duration {
    let want = acquire_retry_delay(attempt);
    if want > remaining {
        remaining
    } else {
        want
    }
}

/// A monotonic stopwatch for the acquisition budget.
pub fn wait_budget(started: Instant, wait: Duration) -> Duration {
    let elapsed = started.elapsed();
    if elapsed >= wait {
        Duration::from_millis(0)
    } else {
        wait - elapsed
    }
}

// ---------------------------------------------------------------------------
// The control-plane I/O the lease needs
// ---------------------------------------------------------------------------

/// Control-record I/O for the lease, on a backend handle the lease owns.
///
/// The handle is separate from the store's because the keeper thread must be
/// able to renew a session that is doing nothing else, and the store's backend
/// is behind a non-reentrant mutex owned by the store. Both handles address the
/// same record with the same compare-and-swap, so they cannot disagree about who
/// holds it; a lost race is a `Busy` refusal, never a silent overwrite.
///
/// On a POSIX commit plane the compare-and-swap runs inside the short
/// publication critical section (`LOCK.publish`), which is what makes
/// read-compare-replace atomic across processes. On a provider plane the native
/// conditional write does the same job and `begin_publication` is a no-op.
pub struct LeaseIo {
    backend: Mutex<Box<dyn crate::backend::StorageBackend + Send>>,
}

impl std::fmt::Debug for LeaseIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LeaseIo")
    }
}

impl LeaseIo {
    /// Open a dedicated handle for the lease record.
    pub fn open(kind: &crate::backend::BackendKind, root: &std::path::Path) -> Result<Self> {
        Ok(LeaseIo {
            backend: Mutex::new(kind.open(root)?),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Box<dyn crate::backend::StorageBackend + Send>> {
        self.backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Read the record and its compare-and-swap token.
    pub fn read(&self) -> Result<Option<(LeaseRecord, Vec<u8>)>> {
        let backend = self.lock();
        match backend.read_all(&crate::backend::Key::writer_lease()) {
            Ok(bytes) => {
                let record = decode_lease(&bytes)?;
                Ok(Some((record, bytes)))
            }
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Replace the record iff it still holds `expected_token` (`None` = absent).
    pub fn compare_and_swap(
        &self,
        expected_token: Option<&str>,
        record: &LeaseRecord,
    ) -> Result<()> {
        let bytes = encode_lease(record)?;
        let mut backend = self.lock();
        // `begin_publication` is the POSIX plane's short critical section and a
        // no-op on a plane whose conditional write is native. Always released,
        // including on the error path.
        backend.begin_publication()?;
        let result =
            backend.compare_and_swap(&crate::backend::Key::writer_lease(), expected_token, &bytes);
        let ended = backend.end_publication();
        match result {
            Ok(()) => {
                ended?;
                Ok(())
            }
            Err(err) => {
                let _ = ended;
                Err(err)
            }
        }
    }

    /// Create one ticket numbered above every ticket already present.
    pub fn join_queue(&self, owner: &str, ttl: Duration, now_ns: i64) -> Result<Ticket> {
        let mut backend = self.lock();
        let deadline_ns = now_ns.saturating_add(ttl.as_nanos().min(i64::MAX as u128) as i64);
        for _ in 0..MAX_ACQUIRE_ATTEMPTS {
            let existing = backend.list_prefix(&crate::backend::Key::prefix(
                crate::backend::KeyKind::WriterTicket,
            ))?;
            let mut highest = 0u64;
            for key in &existing {
                if let crate::backend::Key::WriterTicket { ticket } = key {
                    highest = highest.max(*ticket);
                }
            }
            let ticket = highest.saturating_add(1);
            let record = Ticket {
                ticket,
                owner: owner.to_owned(),
                deadline_ns,
            };
            let bytes = encode_ticket(&record)?;
            match backend.put_atomic(&crate::backend::Key::writer_ticket(ticket), &bytes) {
                Ok(()) => return Ok(record),
                // Another waiter took this number between the listing and the
                // create. Retry from a fresh listing; never overwrite.
                Err(err) if err.class() == Class::Exists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(Error::busy(
            "writer acquisition queue could not create a ticket; retry",
        ))
    }

    /// Remove this handle's ticket. Absence is success (a stale reaper or the
    /// same handle already left).
    pub fn leave_queue(&self, ticket: u64) {
        let mut backend = self.lock();
        let _ = backend.delete(&crate::backend::Key::writer_ticket(ticket));
    }

    /// Live tickets with a lower number than `mine`.
    pub fn live_lower_tickets(&self, mine: u64, now_ns: i64) -> Result<Vec<Ticket>> {
        let backend = self.lock();
        let existing = backend.list_prefix(&crate::backend::Key::prefix(
            crate::backend::KeyKind::WriterTicket,
        ))?;
        if existing.len() > MAX_TICKETS {
            return Err(Error::limit_exceeded(format!(
                "writer acquisition queue holds {} tickets, bound is {MAX_TICKETS}",
                existing.len()
            )));
        }
        let mut out = Vec::new();
        for key in existing {
            let crate::backend::Key::WriterTicket { ticket } = key else {
                continue;
            };
            if ticket >= mine {
                continue;
            }
            match backend.read_all(&crate::backend::Key::writer_ticket(ticket)) {
                Ok(bytes) => {
                    let decoded = decode_ticket(&bytes)?;
                    if decoded.is_live(now_ns) {
                        out.push(decoded);
                    }
                }
                Err(err) if err.class() == Class::NotFound => continue,
                Err(err) => return Err(err),
            }
        }
        out.sort_by_key(|ticket| ticket.ticket);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held() -> LeaseRecord {
        LeaseRecord {
            owner: "pid-1-1-1700000000000000000".to_owned(),
            fence: 3,
            sequence: 2,
            acquired_ns: 1_700_000_000_000_000_000,
            renewed_ns: 1_700_000_001_000_000_000,
            deadline_ns: 1_700_000_030_000_000_000,
            ttl_ns: 30_000_000_000,
            state: LeaseState::Held,
            released_from: None,
            head_generation: 7,
            head_commit: Some(Hash32::ZERO),
            head_commit_len: 512,
        }
    }

    #[test]
    fn record_round_trips() {
        let record = held();
        let bytes = encode_lease(&record).unwrap();
        assert_eq!(decode_lease(&bytes).unwrap(), record);
        // Canonical: re-encoding the decoded record is the same bytes.
        assert_eq!(encode_lease(&decode_lease(&bytes).unwrap()).unwrap(), bytes);
    }

    #[test]
    fn release_round_trips_and_requires_its_predecessor() {
        let mut record = held();
        record.state = LeaseState::Released;
        record.released_from = Some("abcd".to_owned());
        let bytes = encode_lease(&record).unwrap();
        assert_eq!(decode_lease(&bytes).unwrap(), record);

        let mut bad = record.clone();
        bad.released_from = None;
        assert_eq!(encode_lease(&bad).unwrap_err().class(), Class::Malformed);
        let mut bad = held();
        bad.released_from = Some("abcd".to_owned());
        assert_eq!(encode_lease(&bad).unwrap_err().class(), Class::Malformed);
    }

    #[test]
    fn liveness_and_takeover_follow_the_absolute_deadline() {
        let record = held();
        assert!(record.is_live(record.deadline_ns - 1));
        assert!(!record.is_live(record.deadline_ns));
        assert!(!record.may_take_over(record.deadline_ns - 1));
        assert!(record.may_take_over(record.deadline_ns));
        let mut released = record.clone();
        released.state = LeaseState::Released;
        released.released_from = Some("t".to_owned());
        assert!(!released.is_live(0));
        assert!(released.may_take_over(0));
    }

    #[test]
    fn a_zero_fence_is_rejected() {
        let mut record = held();
        record.fence = 0;
        assert_eq!(encode_lease(&record).unwrap_err().class(), Class::Malformed);
    }

    #[test]
    fn ticket_round_trips() {
        let ticket = Ticket {
            ticket: 42,
            owner: "pid-1-1-1".to_owned(),
            deadline_ns: 1_700_000_000_000_000_000,
        };
        let bytes = encode_ticket(&ticket).unwrap();
        assert_eq!(decode_ticket(&bytes).unwrap(), ticket);
        assert!(ticket.is_live(ticket.deadline_ns - 1));
        assert!(!ticket.is_live(ticket.deadline_ns));
    }

    #[test]
    fn options_reject_impossible_lifetimes() {
        let mut options = WriterLeaseOptions::default();
        assert!(options.validate().is_ok());
        options.ttl = Duration::from_millis(500);
        assert_eq!(
            options.validate().unwrap_err().class(),
            Class::InvalidArgument
        );
        options.ttl = Duration::from_secs(30);
        options.ticket_ttl = MAX_TICKET_TTL + Duration::from_secs(1);
        assert_eq!(
            options.validate().unwrap_err().class(),
            Class::InvalidArgument
        );
        options.ticket_ttl = Duration::ZERO;
        assert_eq!(
            options.validate().unwrap_err().class(),
            Class::InvalidArgument
        );
        // A ticket lifetime longer than the lease lifetime is legal on purpose:
        // a waiter may outlive several renewals of the holder.
        options.ticket_ttl = Duration::from_secs(60);
        assert!(options.validate().is_ok());
    }
}
