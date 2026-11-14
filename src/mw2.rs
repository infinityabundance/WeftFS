//! Multi-writer (MW2) transactions: writer mode, transaction records, and the
//! conflict table.
//!
//! The normative contract is `spec/WEFT-MULTIWRITER.md`. This module holds the
//! data types and pure conflict rules; the publication loop lives in
//! [`crate::store::Store::execute_transaction`], because it needs the store's
//! private head/state.
//!
//! The default remains [`WriterMode::Single`] (MW1). [`WriterMode::Optimistic`]
//! is opt-in and implements MW2 for the explicit transaction API only.

use weftcodec::wcb::Value;

use crate::backend::{Key, StorageBackend};
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::state::{EntryRecord, ObjectRecord, Op, State};
use crate::wcb_util as wu;

/// Schema of the writer-mode policy record.
pub const WRITERS_SCHEMA: &str = "weft.writers.v1";
/// Schema of one transaction discovery record.
pub const TX_SCHEMA: &str = "weft.tx.v1";
/// Domain for the deterministic transaction digest.
pub const TX_DIGEST_DOMAIN: &str = "weft/tx/v1";
/// Default bounded retry budget for one transaction.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 8;
/// Default bounded outcome-lookup window, in generations.
pub const DEFAULT_OUTCOME_WINDOW: u64 = 256;
/// Default first backoff, in microseconds.
pub const DEFAULT_BACKOFF_BASE_MICROS: u64 = 200;
/// Default maximum backoff, in milliseconds.
pub const DEFAULT_BACKOFF_MAX_MILLIS: u64 = 25;
/// Default bound on ops in one transaction.
pub const DEFAULT_MAX_OPS: usize = 4096;

/// How a writable store publishes control state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriterMode {
    /// MW1: one exclusive committer (the default and every pre-R4 surface).
    Single,
    /// MW2: independent committers using a conditional head publication.
    Optimistic,
    /// MW3: independent committers in per-domain heads. Opt-in, additive, and
    /// implemented for the local commit plane only. See `spec/WEFT-MULTIWRITER.md`
    /// §15.
    Sharded,
}

impl Default for WriterMode {
    fn default() -> Self {
        WriterMode::Single
    }
}

impl WriterMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WriterMode::Single => "single",
            WriterMode::Optimistic => "optimistic",
            WriterMode::Sharded => "sharded",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "single" => Ok(WriterMode::Single),
            "optimistic" => Ok(WriterMode::Optimistic),
            "sharded" => Ok(WriterMode::Sharded),
            other => Err(Error::invalid_argument(format!(
                "unknown writer mode `{other}` (expected single, optimistic, or sharded)"
            ))),
        }
    }
}

/// Bounds and retry policy for one transaction.
#[derive(Clone, Debug)]
pub struct TransactionOptions {
    pub max_attempts: u32,
    pub outcome_window: u64,
    pub backoff_base_micros: u64,
    pub backoff_max_millis: u64,
    pub max_ops: usize,
    /// Test/receipt hook: force the transaction to abort after this many
    /// publication attempts without trying again. `0` disables it.
    pub fail_after_attempts: u32,
}

impl Default for TransactionOptions {
    fn default() -> Self {
        TransactionOptions {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            outcome_window: DEFAULT_OUTCOME_WINDOW,
            backoff_base_micros: DEFAULT_BACKOFF_BASE_MICROS,
            backoff_max_millis: DEFAULT_BACKOFF_MAX_MILLIS,
            max_ops: DEFAULT_MAX_OPS,
            fail_after_attempts: 0,
        }
    }
}

impl TransactionOptions {
    /// Deterministic exponential backoff: 200 µs, 400 µs, ... capped at 25 ms.
    pub fn backoff(&self, attempt: u32) -> std::time::Duration {
        let shift = attempt.saturating_sub(1).min(16);
        let micros = self
            .backoff_base_micros
            .saturating_mul(1u64 << shift)
            .min(self.backoff_max_millis.saturating_mul(1000));
        std::time::Duration::from_micros(micros)
    }

    /// Bounded exponential backoff with **deterministic jitter**.
    ///
    /// The jitter is a splitmix64 of `(seed, attempt)` scaled to 75–125 % of the
    /// exponential delay. It is reproducible from the transaction's own digest,
    /// so a run is repeatable while two writers that collided once do not
    /// collide again in lockstep. Bounded: never below 75 % of the base, never
    /// above `backoff_max_millis`.
    pub fn backoff_with_jitter(&self, attempt: u32, seed: u64) -> std::time::Duration {
        let base = self.backoff(attempt).as_micros() as u64;
        let mut z = seed ^ (u64::from(attempt)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let percent = 750 + (z % 501);
        let micros = base.saturating_mul(percent) / 1000;
        let capped = micros.min(self.backoff_max_millis.saturating_mul(1000));
        std::time::Duration::from_micros(capped)
    }
}

/// A caller-supplied 128-bit transaction id, 32 lowercase hex characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransactionId(String);

impl TransactionId {
    pub fn new(text: impl Into<String>) -> Result<TransactionId> {
        let text = text.into();
        if text.len() != 32
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::invalid_argument(
                "transaction id must be 32 lowercase hex characters",
            ));
        }
        Ok(TransactionId(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TransactionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The pre-image a transaction observed for one state key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreValue {
    Absent,
    Hash(Hash32),
    Entry(EntryRecord),
}

/// One precondition, derived by the transaction builder from the base state.
///
/// `expected` is what the builder observed. `idempotent` is a value that also
/// counts as "no semantic change" (for a create, the record the transaction
/// itself intends to publish; for an identical replace, the same). Anything
/// else is a typed conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Precondition {
    pub label: &'static str,
    pub expected: PreValue,
    pub idempotent: Option<PreValue>,
}

impl Precondition {
    fn matches(&self, current: &PreValue) -> bool {
        *current == self.expected
            || self
                .idempotent
                .as_ref()
                .is_some_and(|value| value == current)
    }
}

/// One authorization precondition. Re-evaluated against the rebased state on
/// every attempt, so a capability revoke that wins the race denies publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthCheck {
    pub object: Hash32,
    pub principal: crate::auth::Principal,
}

/// A prepared MW2 transaction.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub(crate) id: TransactionId,
    pub(crate) base_head: Hash32,
    pub(crate) base_generation: u64,
    pub(crate) ops: Vec<Op>,
    pub(crate) preconditions: Vec<Precondition>,
    pub(crate) auth: Vec<AuthCheck>,
    /// Manifest ids a prepared transaction still needs. GC pins these while
    /// the prepared record is live; without them a concurrent sweep could
    /// reclaim a blob the eventual commit names.
    pub(crate) pins: Vec<Hash32>,
}

impl Transaction {
    /// A transaction bound to one observed head. Call through
    /// [`crate::store::Store::begin_transaction`] so the id and base are filled
    /// from the store.
    pub fn id(&self) -> &TransactionId {
        &self.id
    }

    pub fn base_head(&self) -> Hash32 {
        self.base_head
    }

    pub fn base_head_generation(&self) -> u64 {
        self.base_generation
    }

    pub fn op_count(&self) -> usize {
        self.ops.len()
    }

    fn push(&mut self, op: Op, pre: Precondition, max_ops: usize) -> Result<()> {
        if self.ops.len() >= max_ops {
            return Err(Error::limit_exceeded(format!(
                "transaction exceeds the {max_ops}-op bound"
            )));
        }
        if let Op::PutObject(record) = &op {
            if record.kind == crate::state::ObjectKind::File
                && record.content != Hash32::ZERO
                && !self.pins.contains(&record.content)
            {
                self.pins.push(record.content);
                self.pins.sort_unstable();
            }
        }
        self.ops.push(op);
        self.preconditions.push(pre);
        Ok(())
    }

    /// Add or replace an object record. A fresh id may be created; replacing an
    /// existing id requires the version the caller observed in this
    /// transaction.
    pub fn put_object(&mut self, state: &State, record: ObjectRecord) -> Result<()> {
        let current = state.objects.get(&record.id);
        let expected = match current {
            Some(existing) => PreValue::Hash(existing.version),
            None => PreValue::Absent,
        };
        let pre = Precondition {
            label: "object",
            expected,
            idempotent: Some(PreValue::Hash(record.version)),
        };
        self.push(Op::PutObject(record), pre, DEFAULT_MAX_OPS)
    }

    /// Add an object only if its id is absent. A concurrent create of the same
    /// id is a typed conflict rather than a silent replace.
    pub fn put_object_create(&mut self, record: ObjectRecord) -> Result<()> {
        let pre = Precondition {
            label: "object",
            expected: PreValue::Absent,
            idempotent: Some(PreValue::Hash(record.version)),
        };
        self.push(Op::PutObject(record), pre, DEFAULT_MAX_OPS)
    }

    /// Delete an object that must still be exactly the observed version.
    pub fn delete_object(&mut self, state: &State, id: Hash32) -> Result<()> {
        let current = state
            .objects
            .get(&id)
            .ok_or_else(|| Error::not_found("no such object to delete"))?;
        let pre = Precondition {
            label: "object",
            expected: PreValue::Hash(current.version),
            idempotent: None,
        };
        self.push(Op::DeleteObject(id), pre, DEFAULT_MAX_OPS)
    }

    /// Add or replace a directory entry. The entry id is `(parent,name)`.
    pub fn put_entry(&mut self, state: &State, record: EntryRecord) -> Result<()> {
        let expected = match state.entries.get(&record.id) {
            Some(existing) => PreValue::Entry(existing.clone()),
            None => PreValue::Absent,
        };
        let pre = Precondition {
            label: "entry",
            expected,
            idempotent: Some(PreValue::Entry(record.clone())),
        };
        self.push(Op::PutEntry(record), pre, DEFAULT_MAX_OPS)
    }

    /// Add a directory entry only if `(parent,name)` is absent. This is the
    /// create semantics: a concurrent create of the same name is `NameExists`,
    /// never a silent replace.
    pub fn put_entry_create(&mut self, record: EntryRecord) -> Result<()> {
        let pre = Precondition {
            label: "entry",
            expected: PreValue::Absent,
            idempotent: Some(PreValue::Entry(record.clone())),
        };
        self.push(Op::PutEntry(record), pre, DEFAULT_MAX_OPS)
    }

    /// Remove a directory entry that must still name the observed target.
    pub fn del_entry(&mut self, state: &State, id: Hash32) -> Result<()> {
        let current = state
            .entries
            .get(&id)
            .ok_or_else(|| Error::not_found("no such entry to remove"))?;
        let pre = Precondition {
            label: "entry",
            expected: PreValue::Entry(current.clone()),
            idempotent: None,
        };
        self.push(Op::DelEntry(id), pre, DEFAULT_MAX_OPS)
    }

    /// Add a relation (deterministic id).
    pub fn put_relation(
        &mut self,
        state: &State,
        record: crate::state::RelationRecord,
    ) -> Result<()> {
        let expected = if state.relations.contains_key(&record.id) {
            PreValue::Hash(record.id)
        } else {
            PreValue::Absent
        };
        let pre = Precondition {
            label: "relation",
            expected,
            idempotent: Some(PreValue::Hash(record.id)),
        };
        self.push(Op::PutRelation(record), pre, DEFAULT_MAX_OPS)
    }

    /// Remove a relation that must still be present.
    pub fn del_relation(&mut self, state: &State, id: Hash32) -> Result<()> {
        if !state.relations.contains_key(&id) {
            return Err(Error::not_found("no such relation to remove"));
        }
        let pre = Precondition {
            label: "relation",
            expected: PreValue::Hash(id),
            idempotent: None,
        };
        self.push(Op::DelRelation(id), pre, DEFAULT_MAX_OPS)
    }

    /// Publish a named snapshot/context-root pointer. A different commit under
    /// the same name is a conflict; no branch is created automatically.
    pub fn set_snapshot(&mut self, state: &State, name: &str, commit: Hash32) -> Result<()> {
        let expected = match state.snapshots.get(name) {
            Some(existing) => PreValue::Hash(*existing),
            None => PreValue::Absent,
        };
        let pre = Precondition {
            label: "snapshot",
            expected,
            idempotent: Some(PreValue::Hash(commit)),
        };
        self.push(
            Op::SetSnapshot {
                name: name.to_owned(),
                commit,
            },
            pre,
            DEFAULT_MAX_OPS,
        )
    }

    /// Set an object tip that must still be the observed version.
    pub fn set_tip(&mut self, state: &State, object: Hash32, version: Hash32) -> Result<()> {
        let expected = match state.tips.get(&object) {
            Some(existing) => PreValue::Hash(*existing),
            None => PreValue::Absent,
        };
        let pre = Precondition {
            label: "tip",
            expected,
            idempotent: Some(PreValue::Hash(version)),
        };
        self.push(Op::SetTip { object, version }, pre, DEFAULT_MAX_OPS)
    }

    /// Require that `object` is readable by `principal` on every attempt.
    pub fn require_readable(&mut self, object: Hash32, principal: crate::auth::Principal) {
        self.auth.push(AuthCheck { object, principal });
    }

    /// The deterministic op digest a retry uses to discover an outcome.
    pub fn digest(&self) -> Result<Hash32> {
        let ops = self.ops.iter().map(Op::to_value).collect::<Vec<_>>();
        // The digest covers the logical transaction (id + op set), never the
        // attempt's observed parent: a retry from a newer head must resolve to
        // the same outcome.
        let value = wu::map([
            ("id", Value::Str(self.id.as_str().to_owned())),
            ("ops", Value::Array(ops)),
        ]);
        let bytes = wu::encode(&value)?;
        Ok(identity::h(TX_DIGEST_DOMAIN, &bytes))
    }
}

/// The outcome of a successfully published transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionOutcome {
    pub id: String,
    pub commit: Hash32,
    pub generation: u64,
    pub attempts: u32,
    /// True when the outcome was recovered from a previous attempt rather than
    /// published by this call.
    pub idempotent: bool,
}

/// Result of looking a transaction id up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutcomeLookup {
    Committed(TransactionOutcome),
    /// The prepared record exists but no commit in the bounded window contains
    /// the op digest. The caller must not assume failure or success.
    Unknown,
    /// No transaction record exists.
    Absent,
}

// ---------------------------------------------------------------------------
// Policy record
// ---------------------------------------------------------------------------

/// Encode the `writers` policy record.
pub fn encode_writers_record(
    mode: WriterMode,
    min_reader: &str,
    created_ns: i64,
) -> Result<Vec<u8>> {
    let value = wu::map([
        ("schema", Value::Str(WRITERS_SCHEMA.to_owned())),
        ("mode", Value::Str(mode.as_str().to_owned())),
        ("min_reader", Value::Str(min_reader.to_owned())),
        ("created_ns", Value::Timestamp(created_ns)),
    ]);
    wu::encode(&value)
}

/// Decode the writer mode from a policy record.
pub fn decode_writers_mode(bytes: &[u8]) -> Result<WriterMode> {
    let value = wu::decode(bytes)?;
    let map = wu::as_map(&value)?;
    wu::check_keys(map, &["schema", "mode", "min_reader", "created_ns"])?;
    let schema = wu::get_text(map, "schema")?;
    if schema != WRITERS_SCHEMA {
        return Err(Error::unsupported_version(format!(
            "writers record schema `{schema}` is not `{WRITERS_SCHEMA}`"
        )));
    }
    WriterMode::parse(&wu::get_text(map, "mode")?)
}

impl WriterMode {
    /// Read the store's policy, if any. An absent record means `single`.
    pub fn policy(backend: &dyn StorageBackend) -> Result<Option<WriterMode>> {
        let bytes = match backend.read_all(&Key::writers()) {
            Ok(bytes) => bytes,
            Err(err) if err.class() == Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(Some(decode_writers_mode(&bytes)?))
    }
}

// ---------------------------------------------------------------------------
// Transaction record
// ---------------------------------------------------------------------------

/// State stored in one `txs/<id>` record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxRecordState {
    Prepared,
    Committed,
}

impl TxRecordState {
    fn as_str(self) -> &'static str {
        match self {
            TxRecordState::Prepared => "prepared",
            TxRecordState::Committed => "committed",
        }
    }
}

/// One decoded transaction record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxRecord {
    pub id: String,
    pub state: TxRecordState,
    pub digest: Hash32,
    pub parent_generation: u64,
    pub commit: Option<Hash32>,
    pub updated_ns: i64,
    /// Sorted, unique manifest ids the prepared transaction needs.
    pub pins: Vec<Hash32>,
}

/// Encode a transaction record.
pub fn encode_tx_record(record: &TxRecord) -> Result<Vec<u8>> {
    let value = wu::map([
        ("schema", Value::Str(TX_SCHEMA.to_owned())),
        ("id", Value::Str(record.id.clone())),
        ("state", Value::Str(record.state.as_str().to_owned())),
        ("digest", wu::hash_value(record.digest)),
        ("parent_generation", Value::U64(record.parent_generation)),
        ("commit", wu::opt_hash_value(record.commit)),
        ("updated_ns", Value::Timestamp(record.updated_ns)),
        (
            "pins",
            Value::Array(record.pins.iter().map(|id| wu::hash_value(*id)).collect()),
        ),
    ]);
    wu::encode(&value)
}

/// Decode a transaction record, validating the exact key set and schema.
pub fn decode_tx_record(bytes: &[u8]) -> Result<TxRecord> {
    let value = wu::decode(bytes)?;
    let map = wu::as_map(&value)?;
    wu::check_keys(
        map,
        &[
            "schema",
            "id",
            "state",
            "digest",
            "parent_generation",
            "commit",
            "updated_ns",
            "pins",
        ],
    )?;
    let schema = wu::get_text(map, "schema")?;
    if schema != TX_SCHEMA {
        return Err(Error::unsupported_version(format!(
            "transaction record schema `{schema}` is not `{TX_SCHEMA}`"
        )));
    }
    let state = match wu::get_text(map, "state")?.as_str() {
        "prepared" => TxRecordState::Prepared,
        "committed" => TxRecordState::Committed,
        other => {
            return Err(Error::malformed(format!(
                "unknown transaction state `{other}`"
            )))
        }
    };
    let commit = wu::get_opt_hash(map, "commit")?;
    Ok(TxRecord {
        id: wu::get_text(map, "id")?,
        state,
        digest: wu::get_hash(map, "digest")?,
        parent_generation: wu::get_u64(map, "parent_generation")?,
        commit,
        updated_ns: wu::get_timestamp(map, "updated_ns")?,
        pins: wu::get_array(map, "pins")?
            .iter()
            .map(|value| match value {
                Value::Bytes(bytes) => Hash32::from_slice(bytes).map_err(Error::from),
                _ => Err(Error::malformed("pins must be bytes32")),
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

// ---------------------------------------------------------------------------
// Conflict validation
// ---------------------------------------------------------------------------

/// Validate every precondition against a candidate state.
///
/// The `ops` list is parallel to `preconditions` (the builder pushes both). The
/// op supplies the key the precondition refers to, so the rule is explicit and
/// cannot drift from the op list.
pub fn validate_preconditions(state: &State, tx: &Transaction) -> Result<()> {
    if tx.ops.len() != tx.preconditions.len() {
        return Err(Error::corrupt(
            "transaction op and precondition lists have different lengths",
        ));
    }
    for (index, (op, pre)) in tx.ops.iter().zip(tx.preconditions.iter()).enumerate() {
        let current = match op {
            Op::PutObject(record) => match state.objects.get(&record.id) {
                Some(existing) => PreValue::Hash(existing.version),
                None => PreValue::Absent,
            },
            Op::DeleteObject(id) => match state.objects.get(id) {
                Some(existing) => PreValue::Hash(existing.version),
                None => PreValue::Absent,
            },
            Op::PutEntry(record) => match state.entries.get(&record.id) {
                Some(entry) => PreValue::Entry(entry.clone()),
                None => PreValue::Absent,
            },
            Op::DelEntry(id) => match state.entries.get(id) {
                Some(entry) => PreValue::Entry(entry.clone()),
                None => PreValue::Absent,
            },
            Op::PutRelation(record) => {
                if state.relations.contains_key(&record.id) {
                    PreValue::Hash(record.id)
                } else {
                    PreValue::Absent
                }
            }
            Op::DelRelation(id) => {
                if state.relations.contains_key(id) {
                    PreValue::Hash(*id)
                } else {
                    PreValue::Absent
                }
            }
            Op::SetSnapshot { name, .. } => match state.snapshots.get(name) {
                Some(commit) => PreValue::Hash(*commit),
                None => PreValue::Absent,
            },
            Op::SetTip { object, .. } => match state.tips.get(object) {
                Some(version) => PreValue::Hash(*version),
                None => PreValue::Absent,
            },
            Op::SetQuotaUsage(_) => {
                // Accounting ops carry no precondition and no conflict rule.
                continue;
            }
        };
        if !pre.matches(&current) {
            let class = match pre.label {
                "entry" => {
                    if matches!(pre.expected, PreValue::Absent) {
                        ConflictKind::NameExists
                    } else {
                        ConflictKind::EntryChanged
                    }
                }
                "object" => {
                    if matches!(pre.expected, PreValue::Absent) {
                        ConflictKind::ObjectExists
                    } else {
                        ConflictKind::ObjectChanged
                    }
                }
                "relation" => ConflictKind::RelationConflict,
                "snapshot" => ConflictKind::SnapshotConflict,
                "tip" => ConflictKind::TipChanged,
                _ => ConflictKind::ObjectChanged,
            };
            return Err(Error::new(
                Class::Conflict,
                format!(
                    "transaction conflict at op {index}: {} ({})",
                    class.as_str(),
                    pre.label
                ),
            ));
        }
    }
    Ok(())
}

/// The conflict classes named by `spec/WEFT-MULTIWRITER.md` §7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    NameExists,
    ObjectExists,
    ObjectChanged,
    EntryChanged,
    RelationConflict,
    SnapshotConflict,
    TipChanged,
    QuotaExceeded,
    CapabilityDenied,
    TransactionIdReused,
}

impl ConflictKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictKind::NameExists => "NameExists",
            ConflictKind::ObjectExists => "ObjectExists",
            ConflictKind::ObjectChanged => "ObjectChanged",
            ConflictKind::EntryChanged => "EntryChanged",
            ConflictKind::RelationConflict => "RelationConflict",
            ConflictKind::SnapshotConflict => "SnapshotConflict",
            ConflictKind::TipChanged => "TipChanged",
            ConflictKind::QuotaExceeded => "QuotaExceeded",
            ConflictKind::CapabilityDenied => "CapabilityDenied",
            ConflictKind::TransactionIdReused => "TransactionIdReused",
        }
    }
}

/// Canonical op list equality, used by outcome discovery.
pub fn same_ops(left: &[Op], right: &[Op]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a.to_value() == b.to_value())
}

/// One state key a transaction touches, as a stable fingerprint.
///
/// `kind` separates namespaces: 0 object, 1 entry, 2 relation, 3 snapshot,
/// 4 tip. The id is the canonical identity in that namespace (a snapshot name
/// is hashed, because a name is not a 32-byte id).
pub type TouchedKey = (u8, Hash32);

const KEY_DOMAIN: &str = "weft/mw2/key/v1";

fn touched_op_key(op: &Op) -> Option<TouchedKey> {
    match op {
        Op::PutObject(record) => Some((0, record.id)),
        Op::DeleteObject(id) => Some((0, *id)),
        Op::PutEntry(record) => Some((1, record.id)),
        Op::DelEntry(id) => Some((1, *id)),
        Op::PutRelation(record) => Some((2, record.id)),
        Op::DelRelation(id) => Some((2, *id)),
        Op::SetSnapshot { name, .. } => Some((3, identity::h(KEY_DOMAIN, name.as_bytes()))),
        Op::SetTip { object, .. } => Some((4, *object)),
        Op::SetQuotaUsage(_) => None,
    }
}

/// The exact read/write key set of one transaction. Validation consults only
/// these keys; nothing else can conflict with it.
pub fn touched_keys(ops: &[Op]) -> std::collections::BTreeSet<TouchedKey> {
    ops.iter().filter_map(touched_op_key).collect()
}

/// The keys one intervening commit changed. Used to decide whether a rebase
/// needs a touched-key check at all.
pub fn changed_keys(ops: &[Op]) -> std::collections::BTreeSet<TouchedKey> {
    touched_keys(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_id_is_strict() {
        assert!(TransactionId::new("0123456789abcdef0123456789abcdef").is_ok());
        assert!(TransactionId::new("0123").is_err());
        assert!(TransactionId::new("0123456789ABCDEF0123456789abcdef").is_err());
    }

    #[test]
    fn writer_mode_record_round_trips() {
        let bytes = encode_writers_record(WriterMode::Optimistic, "weftfs 1.2.0", 7).unwrap();
        assert_eq!(decode_writers_mode(&bytes).unwrap(), WriterMode::Optimistic);
    }

    #[test]
    fn transaction_record_round_trips() {
        let record = TxRecord {
            id: "0123456789abcdef0123456789abcdef".to_owned(),
            state: TxRecordState::Committed,
            digest: Hash32::ZERO,
            parent_generation: 3,
            commit: Some(Hash32::ZERO),
            updated_ns: 11,
            pins: vec![Hash32::ZERO],
        };
        let bytes = encode_tx_record(&record).unwrap();
        assert_eq!(decode_tx_record(&bytes).unwrap(), record);
    }
}
