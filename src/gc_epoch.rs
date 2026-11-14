//! Multi-writer GC epochs and publication reservations (subphase T024).
//!
//! The normative contract is `spec/WEFT-MULTIWRITER.md` §11 (MW1/MW2) and
//! §15.9 (MW3), extended by `spec/WEFT-MULTIWRITER.md` §16.6 and
//! `docs/MULTI-WRITER-GC.md`.
//!
//! **The problem this solves.** A writer publishes immutable, content-addressed
//! blobs *before* any commit or prepared-transaction record names them. A GC
//! pass that lists the store between the blob publication and the record
//! publication sees an unreferenced blob. Grace-by-age only narrows that
//! window; it does not close it, and it closes nothing at all when the operator
//! configures a zero grace to reclaim aggressively.
//!
//! **The mechanism.** Two durable records and one cut:
//!
//! 1. `gc/epoch` is a single control record holding the current epoch, the run
//!    id of the pass that last acquired it, and the wall-clock instant at which
//!    it was acquired. A GC pass acquires the next epoch with **one
//!    compare-and-swap**; that CAS is the pass's linearization point.
//! 2. `gc/reservations/<tx id>` is written by a writer **before** it publishes
//!    any blob that no commit yet names. It carries the epoch the writer
//!    observed, the instant the reservation was taken, and a deadline.
//! 3. A pass may only reclaim a data blob whose backend modification time is
//!    **strictly older than** the epoch acquisition instant, and strictly older
//!    than every live reservation's start instant that was taken at or before
//!    the acquired epoch (the *reservation floor*).
//!
//! Why that is safe, stated as the invariant the adversarial court exercises:
//!
//! * A reservation is written before its blobs, so a blob published by a
//!   reserved writer has an mtime later than its reservation's start.
//! * A writer that read epoch `E` and registered before the pass acquired
//!   `E + 1` is covered by the reservation floor.
//! * A writer that read `E + 1` registered after the pass's acquisition
//!   instant, so its blobs are newer than the epoch cut.
//! * Therefore every blob a live writer still needs is either marked through a
//!   record (a prepared transaction's `pins`) or newer than the cut/floor.
//!
//! **What this is not.** It is not a distributed barrier. The epoch is one
//! compare-and-swap on one control object, the cut is the local wall clock, and
//! the reservation is a cooperative record. A writer that stalls past its
//! reservation deadline is treated as abandoned exactly like a prepared record
//! past the grace. Two writers on different hosts with skewed clocks are not
//! covered; the courts are one host. See `docs/MULTI-WRITER-GC.md` §6.

use weftcodec::wcb::Value;

use crate::error::{Class, Error, Result};
use crate::wcb_util as wu;

/// Schema of the GC epoch control record.
pub const GC_EPOCH_SCHEMA: &str = "weft.gc.epoch.v1";
/// Schema of one publication reservation record.
pub const RESERVATION_SCHEMA: &str = "weft.tx.reservation.v1";
/// Schema of one completed GC run record (an audit trail, never a root).
pub const GC_RUN_SCHEMA: &str = "weft.gc.run.v1";
/// Hard bound on live reservation records a pass will read.
pub const MAX_RESERVATIONS: usize = 4096;
/// Default reservation lifetime. A writer that stalls longer is abandoned.
pub const DEFAULT_RESERVATION_SECS: u64 = 300;
/// Bounded acquisition retries for the epoch compare-and-swap.
pub const DEFAULT_EPOCH_ATTEMPTS: u32 = 16;

/// One decoded `gc/epoch` control record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GcEpoch {
    /// Monotonic epoch. Absent record means epoch 0.
    pub epoch: u64,
    /// Identity of the pass that acquired this epoch.
    pub run_id: String,
    /// Wall-clock instant at which the epoch was acquired. The reclaim cut.
    pub started_ns: i64,
    /// Wall-clock instant at which the pass finished, or `started_ns` while it
    /// is in flight.
    pub finished_ns: i64,
    /// Principal that ran the pass.
    pub owner: String,
}

/// One decoded publication reservation record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub tx_id: String,
    /// Epoch the writer observed before registering.
    pub epoch: u64,
    /// Wall-clock instant the reservation was taken (before any blob).
    pub started_ns: i64,
    /// Wall-clock instant after which the reservation is abandoned.
    pub deadline_ns: i64,
    /// Principal that took the reservation.
    pub owner: String,
}

impl Reservation {
    pub fn is_live(&self, now_ns: i64) -> bool {
        now_ns <= self.deadline_ns
    }
}

/// Encode a `gc/epoch` record.
pub fn encode_epoch(record: &GcEpoch) -> Result<Vec<u8>> {
    if record.run_id.is_empty() || record.run_id.len() > 128 {
        return Err(Error::malformed(
            "gc epoch run id must be 1..=128 bytes".to_owned(),
        ));
    }
    let value = wu::map([
        ("schema", Value::Str(GC_EPOCH_SCHEMA.to_owned())),
        ("epoch", Value::U64(record.epoch)),
        ("run_id", Value::Str(record.run_id.clone())),
        ("started_ns", Value::Timestamp(record.started_ns)),
        ("finished_ns", Value::Timestamp(record.finished_ns)),
        ("owner", Value::Str(record.owner.clone())),
    ]);
    wu::encode(&value)
}

/// Decode a `gc/epoch` record, validating the exact key set and schema.
pub fn decode_epoch(bytes: &[u8]) -> Result<GcEpoch> {
    let value = wu::decode(bytes)?;
    let map = wu::as_map(&value)?;
    wu::check_keys(
        map,
        &[
            "schema",
            "epoch",
            "run_id",
            "started_ns",
            "finished_ns",
            "owner",
        ],
    )?;
    let schema = wu::get_text(map, "schema")?;
    if schema != GC_EPOCH_SCHEMA {
        return Err(Error::unsupported_version(format!(
            "gc epoch schema `{schema}` is not `{GC_EPOCH_SCHEMA}`"
        )));
    }
    Ok(GcEpoch {
        epoch: wu::get_u64(map, "epoch")?,
        run_id: wu::get_text(map, "run_id")?,
        started_ns: wu::get_timestamp(map, "started_ns")?,
        finished_ns: wu::get_timestamp(map, "finished_ns")?,
        owner: wu::get_text(map, "owner")?,
    })
}

/// Encode a publication reservation.
pub fn encode_reservation(record: &Reservation) -> Result<Vec<u8>> {
    if record.tx_id.is_empty() || record.tx_id.len() > 128 {
        return Err(Error::malformed(
            "reservation transaction id must be 1..=128 bytes".to_owned(),
        ));
    }
    let value = wu::map([
        ("schema", Value::Str(RESERVATION_SCHEMA.to_owned())),
        ("tx_id", Value::Str(record.tx_id.clone())),
        ("epoch", Value::U64(record.epoch)),
        ("started_ns", Value::Timestamp(record.started_ns)),
        ("deadline_ns", Value::Timestamp(record.deadline_ns)),
        ("owner", Value::Str(record.owner.clone())),
    ]);
    wu::encode(&value)
}

/// Decode a publication reservation.
pub fn decode_reservation(bytes: &[u8]) -> Result<Reservation> {
    let value = wu::decode(bytes)?;
    let map = wu::as_map(&value)?;
    wu::check_keys(
        map,
        &[
            "schema",
            "tx_id",
            "epoch",
            "started_ns",
            "deadline_ns",
            "owner",
        ],
    )?;
    let schema = wu::get_text(map, "schema")?;
    if schema != RESERVATION_SCHEMA {
        return Err(Error::unsupported_version(format!(
            "reservation schema `{schema}` is not `{RESERVATION_SCHEMA}`"
        )));
    }
    Ok(Reservation {
        tx_id: wu::get_text(map, "tx_id")?,
        epoch: wu::get_u64(map, "epoch")?,
        started_ns: wu::get_timestamp(map, "started_ns")?,
        deadline_ns: wu::get_timestamp(map, "deadline_ns")?,
        owner: wu::get_text(map, "owner")?,
    })
}

/// One completed GC run. Recorded for audit only; it is never a root and never
/// interprets or repairs anything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcRun {
    pub run_id: String,
    pub epoch: u64,
    pub started_ns: i64,
    pub finished_ns: i64,
    pub applied: bool,
    pub reservations_live: u64,
    pub reservation_floor_ns: i64,
    pub retained_newer_than_cut: u64,
    pub retained_reservation_floor: u64,
    pub marked_manifests: u64,
    pub marked_chunks: u64,
    pub deleted_blobs: u64,
    pub deleted_bytes: u64,
    pub deleted_indexes: u64,
    pub deleted_reservations: u64,
    pub removed_orphans: u64,
    pub domain_heads_marked: u64,
}

/// Encode a completed GC run record.
pub fn encode_run(record: &GcRun) -> Result<Vec<u8>> {
    let value = wu::map([
        ("schema", Value::Str(GC_RUN_SCHEMA.to_owned())),
        ("run_id", Value::Str(record.run_id.clone())),
        ("epoch", Value::U64(record.epoch)),
        ("started_ns", Value::Timestamp(record.started_ns)),
        ("finished_ns", Value::Timestamp(record.finished_ns)),
        ("applied", Value::Bool(record.applied)),
        ("reservations_live", Value::U64(record.reservations_live)),
        (
            "reservation_floor_ns",
            Value::Timestamp(record.reservation_floor_ns),
        ),
        (
            "retained_newer_than_cut",
            Value::U64(record.retained_newer_than_cut),
        ),
        (
            "retained_reservation_floor",
            Value::U64(record.retained_reservation_floor),
        ),
        ("marked_manifests", Value::U64(record.marked_manifests)),
        ("marked_chunks", Value::U64(record.marked_chunks)),
        ("deleted_blobs", Value::U64(record.deleted_blobs)),
        ("deleted_bytes", Value::U64(record.deleted_bytes)),
        ("deleted_indexes", Value::U64(record.deleted_indexes)),
        (
            "deleted_reservations",
            Value::U64(record.deleted_reservations),
        ),
        ("removed_orphans", Value::U64(record.removed_orphans)),
        (
            "domain_heads_marked",
            Value::U64(record.domain_heads_marked),
        ),
    ]);
    wu::encode(&value)
}

/// Decode a completed GC run record. Used by the court to read a pass back
/// from the store rather than trusting the process that wrote it.
pub fn decode_run(bytes: &[u8]) -> Result<GcRun> {
    let value = wu::decode(bytes)?;
    let map = wu::as_map(&value)?;
    wu::check_keys(
        map,
        &[
            "schema",
            "run_id",
            "epoch",
            "started_ns",
            "finished_ns",
            "applied",
            "reservations_live",
            "reservation_floor_ns",
            "retained_newer_than_cut",
            "retained_reservation_floor",
            "marked_manifests",
            "marked_chunks",
            "deleted_blobs",
            "deleted_bytes",
            "deleted_indexes",
            "deleted_reservations",
            "removed_orphans",
            "domain_heads_marked",
        ],
    )?;
    let schema = wu::get_text(map, "schema")?;
    if schema != GC_RUN_SCHEMA {
        return Err(Error::unsupported_version(format!(
            "gc run schema `{schema}` is not `{GC_RUN_SCHEMA}`"
        )));
    }
    let bool_of = |key: &str| -> Result<bool> {
        match map.get(key) {
            Some(Value::Bool(value)) => Ok(*value),
            _ => Err(Error::malformed(format!("{key} must be a bool"))),
        }
    };
    Ok(GcRun {
        run_id: wu::get_text(map, "run_id")?,
        epoch: wu::get_u64(map, "epoch")?,
        started_ns: wu::get_timestamp(map, "started_ns")?,
        finished_ns: wu::get_timestamp(map, "finished_ns")?,
        applied: bool_of("applied")?,
        reservations_live: wu::get_u64(map, "reservations_live")?,
        reservation_floor_ns: wu::get_timestamp(map, "reservation_floor_ns")?,
        retained_newer_than_cut: wu::get_u64(map, "retained_newer_than_cut")?,
        retained_reservation_floor: wu::get_u64(map, "retained_reservation_floor")?,
        marked_manifests: wu::get_u64(map, "marked_manifests")?,
        marked_chunks: wu::get_u64(map, "marked_chunks")?,
        deleted_blobs: wu::get_u64(map, "deleted_blobs")?,
        deleted_bytes: wu::get_u64(map, "deleted_bytes")?,
        deleted_indexes: wu::get_u64(map, "deleted_indexes")?,
        deleted_reservations: wu::get_u64(map, "deleted_reservations")?,
        removed_orphans: wu::get_u64(map, "removed_orphans")?,
        domain_heads_marked: wu::get_u64(map, "domain_heads_marked")?,
    })
}

/// The reservation state one pass observed. Pure data; the store builds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReservationView {
    /// Live reservations taken at or before the acquired epoch.
    pub live: Vec<Reservation>,
    /// Reservation record keys whose deadline had passed (removed on apply).
    pub expired: Vec<String>,
    /// The reservation floor: the earliest `started_ns` among `live`.
    /// `i64::MAX` when no reservation protects anything.
    pub floor_ns: i64,
}

impl ReservationView {
    pub const NO_FLOOR: i64 = i64::MAX;

    /// Clock slack applied to **both** the epoch cut and the reservation floor.
    ///
    /// The safety arguments are `blob mtime >= epoch acquisition instant` (for
    /// a writer that registered after the cut) and `blob mtime >= reservation
    /// start` (for one that registered before it). Filesystem timestamps are
    /// quantised, so an mtime can be truncated below an adjacent nanosecond
    /// clock read. Both comparisons therefore compare against an instant moved
    /// one second *earlier*. This is deliberately conservative: it can retain a
    /// blob written up to a second before the cut, and it can never reclaim one
    /// a live reservation or a newer-than-cut publication protects. The cost is
    /// bounded (one second of writes) and the safety direction is the only one
    /// that matters.
    pub const CLOCK_SLACK_NS: i64 = 1_000_000_000;

    /// Build the view from decoded records. A record naming a later epoch is
    /// not part of this pass: it registered after the cut.
    pub fn build(records: Vec<Reservation>, acquired_epoch: u64, now_ns: i64) -> Self {
        let mut view = ReservationView {
            live: Vec::new(),
            expired: Vec::new(),
            floor_ns: Self::NO_FLOOR,
        };
        for record in records {
            if !record.is_live(now_ns) {
                view.expired.push(record.tx_id);
                continue;
            }
            if record.epoch > acquired_epoch {
                // Registered after this pass acquired its epoch; its blobs are
                // newer than the cut, so it neither pins nor protects.
                continue;
            }
            view.floor_ns = view.floor_ns.min(record.started_ns);
            view.live.push(record);
        }
        if view.floor_ns != Self::NO_FLOOR {
            view.floor_ns = view.floor_ns.saturating_sub(Self::CLOCK_SLACK_NS);
        }
        view
    }
}

/// Refuse a reservation older than the bound rather than silently accept it.
pub fn validate_reservation_deadline(started_ns: i64, deadline_ns: i64) -> Result<()> {
    if deadline_ns <= started_ns {
        return Err(Error::invalid_argument(
            "a publication reservation deadline must be after its start",
        ));
    }
    Ok(())
}

/// The typed refusal used when a store cannot take reservations safely.
pub fn reservations_unsupported(why: &str) -> Error {
    Error::new(Class::UnsupportedVersion, why.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reservation(tx_id: &str, epoch: u64, started_ns: i64, deadline_ns: i64) -> Reservation {
        Reservation {
            tx_id: tx_id.to_owned(),
            epoch,
            started_ns,
            deadline_ns,
            owner: "test".to_owned(),
        }
    }

    #[test]
    fn epoch_record_round_trips() {
        let record = GcEpoch {
            epoch: 7,
            run_id: "run-7".to_owned(),
            started_ns: 1_000,
            finished_ns: 2_000,
            owner: "operator".to_owned(),
        };
        let bytes = encode_epoch(&record).expect("encode");
        assert_eq!(decode_epoch(&bytes).expect("decode"), record);
    }

    #[test]
    fn reservation_record_round_trips() {
        let record = reservation("tx", 3, 10, 20);
        let bytes = encode_reservation(&record).expect("encode");
        assert_eq!(decode_reservation(&bytes).expect("decode"), record);
    }

    #[test]
    fn a_reservation_from_a_later_epoch_is_not_part_of_the_pass() {
        let view = ReservationView::build(vec![reservation("late", 5, 50, 100)], 4, 60);
        assert!(view.live.is_empty());
        assert_eq!(view.floor_ns, ReservationView::NO_FLOOR);
    }

    #[test]
    fn an_expired_reservation_is_reported_not_fenced() {
        let view = ReservationView::build(vec![reservation("old", 2, 10, 20)], 3, 30);
        assert!(view.live.is_empty());
        assert_eq!(view.expired, vec!["old".to_owned()]);
    }

    #[test]
    fn the_floor_is_the_earliest_live_reservation() {
        let view = ReservationView::build(
            vec![
                reservation("a", 2, 30, 100),
                reservation("b", 3, 10, 100),
                reservation("c", 1, 20, 5),
            ],
            3,
            50,
        );
        assert_eq!(
            view.floor_ns,
            10_i64.saturating_sub(ReservationView::CLOCK_SLACK_NS),
            "the floor is the earliest live start minus the clock slack"
        );
        assert_eq!(view.live.len(), 2);
        assert_eq!(view.expired, vec!["c".to_owned()]);
    }
}
