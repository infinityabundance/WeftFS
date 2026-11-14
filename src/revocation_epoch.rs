//! Distributed revocation epochs and deny-before-decode (subphase T078).
//!
//! Normative description: `docs/REVOCATION-EPOCH.md`.
//!
//! **What was wrong before.** Capability revocation was a caller-supplied list
//! (`verify_envelope(blob, revocations, …)`) and the KV prefix path had a
//! local count. A client that had not been told about a revocation kept
//! serving, and the decision depended on what the client chose to pass.
//!
//! **What replaces it.** One monotonic, store-published revocation epoch:
//!
//! ```text
//! /revocation/epoch            current canonical `weft.revocation-epoch.v1`
//! /revocation/history/<epoch>  immutable record for every epoch (chain)
//! ```
//!
//! A client `observe`s the epoch once, keeps the record id, epoch and the
//! moment it observed it, and passes that [`RevocationObservation`] with every
//! authorization. [`Store::check_read_at_epoch`] re-reads the store's current
//! epoch *before touching object metadata or bytes* and refuses with
//! `StaleEpoch` if the client's epoch is behind, or with `CapabilityRevoked`
//! if an entry matches. Denial is a fixed message: it does not disclose
//! existence, size or count, and it is identical for a revoked object and for
//! an object that does not exist.
//!
//! **Enforcement boundary.** This is a store-local epoch record with a chain,
//! not a gossip protocol. A client that cannot read the store's epoch must
//! refuse to serve; there is no cross-store propagation, no consensus, no
//! wall-clock skew defence and no transparency log. See the document §7.

use std::collections::BTreeSet;

use weftcodec::wcb::{self, Value};
use weftcodec::Limits;

use crate::auth::Principal;
use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::state::State;
use crate::store::{AuthorizedRead, Store};

/// Record schema.
pub const REVOCATION_EPOCH_SCHEMA: &str = "weft.revocation-epoch.v1";
/// Identity domain.
pub const REVOCATION_EPOCH_DOMAIN: &str = "weft/revocation/epoch/v1";
/// Reserved namespace.
pub const REVOCATION_DIR: &str = "revocation";
/// Current pointer name inside the namespace.
pub const REVOCATION_CURRENT: &str = "epoch";
/// History directory name.
pub const REVOCATION_HISTORY: &str = "history";
/// Default freshness window a client may rely on.
pub const DEFAULT_FRESHNESS_NS: i64 = 30_000_000_000;
/// Hard bound on entries one epoch may carry.
pub const MAX_REVOCATIONS: usize = 1_000_000;
/// Hard bound on the history chain a verification walks.
pub const MAX_HISTORY: u64 = 1_000_000;
/// Fixed, non-disclosing denial text. Never includes an id, size or count.
pub const REVOCATION_DENIED: &str = "revocation denies this read";

/// What was revoked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RevocationKind {
    /// A capability envelope id or revocation id.
    Capability,
    /// A content root (the whole-file identity) or an object id.
    Content,
}

impl RevocationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RevocationKind::Capability => "capability",
            RevocationKind::Content => "content",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "capability" => Ok(RevocationKind::Capability),
            "content" => Ok(RevocationKind::Content),
            other => Err(Error::invalid_argument(format!(
                "revocation kind `{other}` is not capability|content"
            ))),
        }
    }
}

/// One revocation entry. Tenant-scoped: revoking tenant A's content does not
/// revoke tenant B's read of the same deduplicated bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RevocationEntry {
    pub tenant: String,
    pub kind: RevocationKind,
    pub id: Hash32,
}

impl RevocationEntry {
    pub fn new(tenant: impl Into<String>, kind: RevocationKind, id: Hash32) -> Result<Self> {
        let tenant = tenant.into();
        if tenant.is_empty() || tenant.len() > 512 {
            return Err(Error::invalid_argument(
                "revocation tenant must be 1..=512 bytes",
            ));
        }
        if id == Hash32::ZERO {
            return Err(Error::invalid_argument(
                "revocation id must not be all zero",
            ));
        }
        Ok(RevocationEntry { tenant, kind, id })
    }

    fn to_value(&self) -> Value {
        let mut map = std::collections::BTreeMap::new();
        map.insert("tenant".to_owned(), Value::Str(self.tenant.clone()));
        map.insert("kind".to_owned(), Value::Str(self.kind.as_str().to_owned()));
        map.insert("id".to_owned(), Value::Bytes(self.id.as_bytes().to_vec()));
        Value::Map(map)
    }

    fn from_value(value: &Value) -> Result<Self> {
        let Value::Map(map) = value else {
            return Err(Error::malformed("revocation entry is not a map"));
        };
        if map.len() != 3 {
            return Err(Error::malformed("revocation entry has an unknown key set"));
        }
        let tenant = match map.get("tenant") {
            Some(Value::Str(text)) => text.clone(),
            _ => return Err(Error::malformed("revocation tenant is not text")),
        };
        let kind = match map.get("kind") {
            Some(Value::Str(text)) => RevocationKind::parse(text)?,
            _ => return Err(Error::malformed("revocation kind is not text")),
        };
        let id = match map.get("id") {
            Some(Value::Bytes(bytes)) if bytes.len() == 32 => Hash32::from_bytes(
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::malformed("revocation id is not 32 bytes"))?,
            ),
            _ => return Err(Error::malformed("revocation id is not bytes32")),
        };
        RevocationEntry::new(tenant, kind, id)
    }
}

/// One canonical revocation epoch record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevocationEpoch {
    pub epoch: u64,
    pub issued_at_ns: i64,
    pub valid_for_ns: u64,
    /// Record id of the previous epoch (`None` for epoch 1).
    pub prev: Option<Hash32>,
    /// Sorted by `(tenant, kind, id)`, unique.
    pub revocations: Vec<RevocationEntry>,
}

impl RevocationEpoch {
    pub fn new(
        epoch: u64,
        issued_at_ns: i64,
        valid_for_ns: u64,
        prev: Option<Hash32>,
        mut revocations: Vec<RevocationEntry>,
    ) -> Result<Self> {
        if epoch == 0 {
            return Err(Error::invalid_argument("revocation epoch starts at 1"));
        }
        if valid_for_ns == 0 {
            return Err(Error::invalid_argument(
                "revocation epoch needs a non-zero freshness window",
            ));
        }
        if revocations.len() > MAX_REVOCATIONS {
            return Err(Error::limit_exceeded(format!(
                "revocation epoch exceeds {MAX_REVOCATIONS} entries"
            )));
        }
        revocations.sort();
        let mut seen = revocations.clone();
        seen.dedup();
        if seen.len() != revocations.len() {
            return Err(Error::non_canonical("revocation epoch repeats an entry"));
        }
        match (epoch, prev) {
            (1, Some(_)) => return Err(Error::non_canonical("epoch 1 cannot name a predecessor")),
            (other, None) if other > 1 => {
                return Err(Error::non_canonical(
                    "a later epoch must name its predecessor",
                ))
            }
            _ => {}
        }
        Ok(RevocationEpoch {
            epoch,
            issued_at_ns,
            valid_for_ns,
            prev,
            revocations,
        })
    }

    fn to_value(&self) -> Value {
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "schema".to_owned(),
            Value::Str(REVOCATION_EPOCH_SCHEMA.to_owned()),
        );
        map.insert("epoch".to_owned(), Value::U64(self.epoch));
        map.insert(
            "issued_at_ns".to_owned(),
            Value::U64(u64::try_from(self.issued_at_ns).unwrap_or(0)),
        );
        map.insert("valid_for_ns".to_owned(), Value::U64(self.valid_for_ns));
        map.insert(
            "prev".to_owned(),
            self.prev
                .map(|prev| Value::Bytes(prev.as_bytes().to_vec()))
                .unwrap_or(Value::Null),
        );
        map.insert(
            "revocations".to_owned(),
            Value::Array(
                self.revocations
                    .iter()
                    .map(RevocationEntry::to_value)
                    .collect(),
            ),
        );
        Value::Map(map)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wcb::encode(&self.to_value(), &Limits::default()).map_err(Error::from)
    }

    /// `H("weft/revocation/epoch/v1", canonical bytes)`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(
            REVOCATION_EPOCH_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let value = wcb::decode(bytes, &Limits::default()).map_err(Error::from)?;
        let encoded = wcb::encode(&value, &Limits::default()).map_err(Error::from)?;
        if encoded != bytes {
            return Err(Error::non_canonical(
                "revocation epoch is not canonical WCB",
            ));
        }
        let Value::Map(map) = value else {
            return Err(Error::malformed("revocation epoch is not a map"));
        };
        const KEYS: &[&str] = &[
            "schema",
            "epoch",
            "issued_at_ns",
            "valid_for_ns",
            "prev",
            "revocations",
        ];
        if map.len() != KEYS.len() || !KEYS.iter().all(|key| map.contains_key(*key)) {
            return Err(Error::malformed("revocation epoch has an unknown key set"));
        }
        match map.get("schema") {
            Some(Value::Str(text)) if text == REVOCATION_EPOCH_SCHEMA => {}
            _ => return Err(Error::malformed("unknown revocation epoch schema")),
        }
        let number = |name: &str| -> Result<u64> {
            match map.get(name) {
                Some(Value::U64(value)) => Ok(*value),
                _ => Err(Error::malformed(format!(
                    "revocation epoch `{name}` is not u64"
                ))),
            }
        };
        let epoch = number("epoch")?;
        let issued_at_ns = i64::try_from(number("issued_at_ns")?)
            .map_err(|_| Error::malformed("revocation issued_at_ns is out of range"))?;
        let valid_for_ns = number("valid_for_ns")?;
        let prev = match map.get("prev") {
            Some(Value::Null) => None,
            Some(Value::Bytes(bytes)) if bytes.len() == 32 => {
                Some(Hash32::from_bytes(bytes.as_slice().try_into().map_err(
                    |_| Error::malformed("revocation prev is not 32 bytes"),
                )?))
            }
            _ => {
                return Err(Error::malformed(
                    "revocation epoch `prev` is not bytes32|null",
                ))
            }
        };
        let revocations = match map.get("revocations") {
            Some(Value::Array(items)) => items
                .iter()
                .map(RevocationEntry::from_value)
                .collect::<Result<Vec<_>>>()?,
            _ => {
                return Err(Error::malformed(
                    "revocation epoch `revocations` is not an array",
                ))
            }
        };
        let record = RevocationEpoch::new(epoch, issued_at_ns, valid_for_ns, prev, revocations)?;
        if record.canonical_bytes()? != bytes {
            return Err(Error::non_canonical(
                "revocation epoch entries are not in canonical order",
            ));
        }
        Ok(record)
    }

    /// Does this epoch revoke `entry`?
    pub fn revokes(&self, entry: &RevocationEntry) -> bool {
        self.revocations.binary_search(entry).is_ok()
    }
}

/// A client's observation of the store's revocation epoch.
///
/// It is a *pointer plus freshness*, not a caller-supplied set: the per-read
/// gate re-reads the store's current record and compares epochs. The entries
/// are kept for diagnostics only and are never used to decide a read.
#[derive(Clone, Debug)]
pub struct RevocationObservation {
    pub epoch: u64,
    pub id: Hash32,
    pub observed_at_ns: i64,
    pub max_staleness_ns: i64,
}

impl RevocationObservation {
    pub fn new(id: Hash32, epoch: u64, observed_at_ns: i64, max_staleness_ns: i64) -> Self {
        RevocationObservation {
            epoch,
            id,
            observed_at_ns,
            max_staleness_ns,
        }
    }

    fn check_fresh(&self, now_ns: i64) -> Result<()> {
        if self.max_staleness_ns < 0 {
            return Err(Error::invalid_argument(
                "revocation staleness bound cannot be negative",
            ));
        }
        if now_ns.saturating_sub(self.observed_at_ns) > self.max_staleness_ns {
            return Err(Error::stale_epoch(format!(
                "revocation observation is older than {} ns; refresh before serving",
                self.max_staleness_ns
            )));
        }
        Ok(())
    }
}

impl Store {
    /// The current revocation epoch, if any has been published.
    pub fn revocation_epoch(&self) -> Result<Option<RevocationEpoch>> {
        let dir = match self.resolve_path(&format!("/{REVOCATION_DIR}")) {
            Ok(dir) => dir,
            Err(err) if err.class() == crate::error::Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let current = match self.lookup(dir, REVOCATION_CURRENT.as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == crate::error::Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let bytes = self.read_object_uncounted(current)?;
        Ok(Some(RevocationEpoch::from_canonical_bytes(&bytes)?))
    }

    /// The history record at `epoch`, if present.
    pub fn revocation_epoch_at(&self, epoch: u64) -> Result<Option<RevocationEpoch>> {
        let dir = match self.resolve_path(&format!("/{REVOCATION_DIR}/{REVOCATION_HISTORY}")) {
            Ok(dir) => dir,
            Err(err) if err.class() == crate::error::Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let id = match self.lookup(dir, epoch.to_string().as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == crate::error::Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let bytes = self.read_object_uncounted(id)?;
        Ok(Some(RevocationEpoch::from_canonical_bytes(&bytes)?))
    }

    /// Validate the chain up to the current epoch.
    ///
    /// Every history record must exist, name its predecessor's record id, and
    /// the current record must be the highest history record with the same
    /// bytes. A gap, a forged record or a rolled-back pointer is `Corrupt`.
    pub fn verify_revocation_chain(&self) -> Result<Option<RevocationEpoch>> {
        let Some(current) = self.revocation_epoch()? else {
            return Ok(None);
        };
        if current.epoch > MAX_HISTORY {
            return Err(Error::limit_exceeded(format!(
                "revocation chain exceeds {MAX_HISTORY} epochs"
            )));
        }
        // A rolled-back current pointer names an older epoch while newer history
        // entries still exist. Compare against the highest history slot, not
        // against the pointer itself.
        let history_dir = self
            .resolve_path(&format!("/{REVOCATION_DIR}/{REVOCATION_HISTORY}"))
            .map_err(|err| {
                Error::corrupt(format!(
                    "revocation history directory is unreadable: {}",
                    err.message()
                ))
            })?;
        let max_history = self
            .list_dir(history_dir)?
            .iter()
            .filter_map(|entry| String::from_utf8(entry.name.clone()).ok())
            .filter_map(|name| name.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        if max_history > current.epoch {
            return Err(Error::corrupt(format!(
                "revocation current pointer is rolled back to epoch {} while history reaches {}",
                current.epoch, max_history
            )));
        }
        let mut previous_id: Option<Hash32> = None;
        for epoch in 1..=current.epoch {
            let record = self.revocation_epoch_at(epoch)?.ok_or_else(|| {
                Error::corrupt(format!("revocation history is missing epoch {epoch}"))
            })?;
            if record.epoch != epoch {
                return Err(Error::corrupt(format!(
                    "revocation history slot {epoch} holds epoch {}",
                    record.epoch
                )));
            }
            if record.prev != previous_id {
                return Err(Error::corrupt(format!(
                    "revocation history epoch {epoch} does not link to its predecessor"
                )));
            }
            let id = record.id()?;
            if epoch == current.epoch {
                if record != current {
                    return Err(Error::corrupt(
                        "the current revocation record differs from its history entry",
                    ));
                }
                if current.id()? != id {
                    return Err(Error::corrupt("revocation current record id mismatch"));
                }
            }
            previous_id = Some(id);
        }
        Ok(Some(current))
    }

    /// Observe the revocation epoch for a client.
    ///
    /// Validates the chain and the record's own freshness window, then returns
    /// a pointer the client passes to [`Store::check_read_at_epoch`]. An absent
    /// epoch record yields a synthetic epoch-0 observation: no revocations have
    /// ever been published.
    pub fn observe_revocation_epoch(
        &self,
        max_staleness_ns: i64,
        now_ns: i64,
    ) -> Result<RevocationObservation> {
        if max_staleness_ns < 0 {
            return Err(Error::invalid_argument(
                "revocation staleness bound cannot be negative",
            ));
        }
        match self.verify_revocation_chain()? {
            None => Ok(RevocationObservation::new(
                Hash32::ZERO,
                0,
                now_ns,
                max_staleness_ns,
            )),
            Some(current) => {
                if now_ns.saturating_sub(current.issued_at_ns)
                    > i64::try_from(current.valid_for_ns).unwrap_or(i64::MAX)
                {
                    return Err(Error::stale_epoch(format!(
                        "the store's revocation epoch {} is older than its {} ns freshness window; \
                         a client must not serve on it",
                        current.epoch, current.valid_for_ns
                    )));
                }
                Ok(RevocationObservation::new(
                    current.id()?,
                    current.epoch,
                    now_ns,
                    max_staleness_ns,
                ))
            }
        }
    }

    /// Publish the next revocation epoch carrying `additions`, in one commit.
    ///
    /// Additions are merged with the current entries and sorted canonically.
    /// Re-adding an existing entry is idempotent but still publishes a new
    /// epoch: a reader that observed the older epoch must refresh.
    pub fn publish_revocation_epoch(
        &mut self,
        additions: &[RevocationEntry],
        valid_for_ns: u64,
    ) -> Result<RevocationEpoch> {
        let current = self.revocation_epoch()?;
        let (epoch, prev, mut entries) = match current {
            Some(current) => (
                current
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| Error::limit_exceeded("revocation epoch counter overflowed"))?,
                Some(current.id()?),
                current.revocations,
            ),
            None => (1, None, Vec::new()),
        };
        for addition in additions {
            if !entries.contains(addition) {
                entries.push(addition.clone());
            }
        }
        let now_ns = self.clock_now_ns();
        let record = RevocationEpoch::new(epoch, now_ns, valid_for_ns, prev, entries)?;
        let bytes = record.canonical_bytes()?;
        let uid = self.options().owner_uid;
        let gid = self.options().owner_gid;
        self.group(|store| {
            let root = store.resolve_path("/")?;
            let dir = match store.lookup(root, REVOCATION_DIR.as_bytes()) {
                Ok(id) => id,
                Err(err) if err.class() == crate::error::Class::NotFound => store.mkdir_seeded(
                    root,
                    REVOCATION_DIR.as_bytes(),
                    0o755,
                    uid,
                    gid,
                    b"revocation-dir",
                )?,
                Err(err) => return Err(err),
            };
            let history = match store.lookup(dir, REVOCATION_HISTORY.as_bytes()) {
                Ok(id) => id,
                Err(err) if err.class() == crate::error::Class::NotFound => store.mkdir_seeded(
                    dir,
                    REVOCATION_HISTORY.as_bytes(),
                    0o755,
                    uid,
                    gid,
                    b"revocation-history",
                )?,
                Err(err) => return Err(err),
            };
            let name = epoch.to_string();
            match store.lookup(history, name.as_bytes()) {
                Ok(_) => {
                    return Err(Error::corrupt(format!(
                        "revocation history epoch {epoch} already exists"
                    )))
                }
                Err(err) if err.class() == crate::error::Class::NotFound => {}
                Err(err) => return Err(err),
            }
            store.create_file(
                history,
                name.as_bytes(),
                &bytes,
                0o444,
                uid,
                gid,
                std::collections::BTreeMap::new(),
            )?;
            match store.lookup(dir, REVOCATION_CURRENT.as_bytes()) {
                Ok(id) => {
                    store.write_file(id, 0, &bytes)?;
                }
                Err(err) if err.class() == crate::error::Class::NotFound => {
                    store.create_file(
                        dir,
                        REVOCATION_CURRENT.as_bytes(),
                        &bytes,
                        0o444,
                        uid,
                        gid,
                        std::collections::BTreeMap::new(),
                    )?;
                }
                Err(err) => return Err(err),
            }
            Ok(())
        })?;
        Ok(record)
    }

    /// Authorize one object read at a client's revocation observation.
    ///
    /// Order, and nothing may reorder it:
    ///
    /// 1. the observation is fresh (age <= its declared bound);
    /// 2. the store's current epoch is read and must equal the observation's
    ///    epoch and record id (a revocation since the observation, or a forged
    ///    record, refuses here);
    /// 3. a matching `capability` entry on the principal's envelope refuses;
    /// 4. a matching `content` entry for this tenant refuses;
    /// 5. only then is the object's metadata consulted for POSIX/ACL.
    ///
    /// Steps 1-2 touch no object metadata; steps 3-4 touch no bytes. A refusal
    /// returns one fixed message and one class, so it reveals no existence,
    /// size or count.
    pub fn check_read_at_epoch(
        &self,
        snapshot: Option<Hash32>,
        id: Hash32,
        principal: &Principal,
        tenant: &str,
        observation: &RevocationObservation,
        now_ns: i64,
    ) -> Result<AuthorizedRead> {
        observation.check_fresh(now_ns)?;
        // The tenant is the authenticated one. When the principal carries a
        // verified envelope, a disagreeing argument is refused rather than
        // silently overriding the envelope.
        if let Some(envelope) = principal.envelope() {
            if envelope.envelope.tenant != tenant {
                return Err(Error::invalid_argument(
                    "the tenant argument does not match the principal's verified envelope",
                ));
            }
        }
        // One record read, not a chain walk: the client already validated the
        // chain when it observed. The per-read gate only has to prove the store
        // has not advanced (or been rolled back) since that observation.
        let current = self.revocation_epoch()?;
        match (&current, observation.epoch) {
            (None, 0) => {}
            (None, _) => {
                return Err(Error::stale_epoch(
                    "the store holds no revocation epoch; refresh the observation",
                ))
            }
            (Some(record), epoch) => {
                if record.epoch != epoch {
                    return Err(Error::stale_epoch(format!(
                        "observed revocation epoch {epoch}, the store is at {}; refresh before \
                         serving",
                        record.epoch
                    )));
                }
                if record.id()? != observation.id {
                    return Err(Error::corrupt(
                        "observed revocation record id does not match the store's record",
                    ));
                }
            }
        }
        if let Some(record) = &current {
            // A capability revocation is decidable from the principal's own
            // envelope, before any object metadata is consulted. The bearer of
            // a revoked envelope already knows it holds it.
            if let Some(envelope) = principal.envelope() {
                for candidate in [envelope.envelope_id, envelope.envelope.revocation_id] {
                    let entry =
                        RevocationEntry::new(tenant, RevocationKind::Capability, candidate)?;
                    if record.revokes(&entry) {
                        return Err(Error::capability_revoked(REVOCATION_DENIED));
                    }
                }
            }
            // Authorize on metadata first. A caller that may not read the
            // object (or names an absent one) gets the ordinary denial, so the
            // content-revocation answer is never an existence oracle for
            // unauthorized callers. No bytes are read by this step.
            let authorized = match snapshot {
                Some(commit) => self.authorize_read_at(commit, id, principal, now_ns)?,
                None => self.authorize_read(id, principal, now_ns)?,
            };
            let state = match snapshot {
                Some(commit) => self.state_at(commit)?,
                None => self.current_state().clone(),
            };
            if let Some(object) = state.objects.get(&id) {
                for candidate in [object.content, object.id] {
                    let entry = RevocationEntry::new(tenant, RevocationKind::Content, candidate)?;
                    if record.revokes(&entry) {
                        return Err(Error::capability_revoked(REVOCATION_DENIED));
                    }
                }
            }
            return Ok(authorized);
        }
        match snapshot {
            Some(commit) => self.authorize_read_at(commit, id, principal, now_ns),
            None => self.authorize_read(id, principal, now_ns),
        }
    }
}

/// Diagnostic helper: the record id an epoch would have. Used by courts.
pub fn epoch_id(record: &RevocationEpoch) -> Result<Hash32> {
    record.id()
}
