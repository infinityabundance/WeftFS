//! MW3 conflict domains and independent heads.
//!
//! The normative contract is `spec/WEFT-MULTIWRITER.md` §15. This module holds
//! the pure identity/record types: a domain record and its derived
//! `DomainId`, the discovery record, and the read-view capture. The publication
//! loop and the per-domain head live in [`crate::store`] because they need the
//! store's private state.
//!
//! MW3 is **opt-in** ([`crate::mw2::WriterMode::Sharded`]) and Rust-only. It
//! changes no MW1 or MW2 byte.

use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::wcb_util as wu;

/// Schema of one domain record.
pub const DOMAIN_SCHEMA: &str = "weft.domain.v1";
/// Hash domain for a derived `DomainId`.
pub const DOMAIN_DOMAIN: &str = "weft/domain/v1";
/// Schema of the discovery record.
pub const DOMAINS_SCHEMA: &str = "weft.domains.v1";
/// Schema of a durable cross-domain view pin.
pub const VIEW_PIN_SCHEMA: &str = "weft.view.pin.v1";
/// Hash domain for `view_root`.
pub const VIEW_DOMAIN: &str = "weft/view/v1";
/// Schema of the (specified, not implemented) domain link record.
pub const DOMAIN_LINK_SCHEMA: &str = "weft.domain.link.v1";
/// Hard bound on the number of registered domains in one discovery record.
pub const MAX_DOMAINS: usize = 65_536;
/// Hard bound on the domains one read view may name.
pub const MAX_VIEW_DOMAINS: usize = 4_096;

/// What a domain names. It is part of the identity, so two records with the
/// same display name but different kinds are different domains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DomainKind {
    CheckpointFamily,
    ModelBranch,
    Namespace,
    Root,
}

impl DomainKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DomainKind::CheckpointFamily => "checkpoint_family",
            DomainKind::ModelBranch => "model_branch",
            DomainKind::Namespace => "namespace",
            DomainKind::Root => "root",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "checkpoint_family" => Ok(DomainKind::CheckpointFamily),
            "model_branch" => Ok(DomainKind::ModelBranch),
            "namespace" => Ok(DomainKind::Namespace),
            "root" => Ok(DomainKind::Root),
            other => Err(Error::invalid_argument(format!(
                "unknown domain kind `{other}` (expected checkpoint_family, model_branch, \
                 namespace, or root)"
            ))),
        }
    }
}

/// A canonical `weft.domain.v1` record.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DomainRecord {
    pub name: String,
    pub kind: DomainKind,
    pub scope: String,
}

fn validate_text(label: &str, text: &str, max: usize, allow_empty: bool) -> Result<()> {
    if text.is_empty() && !allow_empty {
        return Err(Error::invalid_argument(format!(
            "domain {label} must not be empty"
        )));
    }
    if text.len() > max {
        return Err(Error::limit_exceeded(format!(
            "domain {label} is {} bytes, above the {max}-byte bound",
            text.len()
        )));
    }
    if text.as_bytes().contains(&0) {
        return Err(Error::invalid_argument(format!(
            "domain {label} must not contain NUL"
        )));
    }
    Ok(())
}

impl DomainRecord {
    pub fn new(
        name: impl Into<String>,
        kind: DomainKind,
        scope: impl Into<String>,
    ) -> Result<Self> {
        let record = DomainRecord {
            name: name.into(),
            kind,
            scope: scope.into(),
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<()> {
        validate_text("name", &self.name, 255, false)?;
        validate_text("scope", &self.scope, 255, true)?;
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(DOMAIN_SCHEMA.to_owned())),
            ("name", Value::Str(self.name.clone())),
            ("kind", Value::Str(self.kind.as_str().to_owned())),
            ("scope", Value::Str(self.scope.clone())),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    /// `H("weft/domain/v1", WCB(record))`. Stable for the life of the domain.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(DOMAIN_DOMAIN, &self.encode()?))
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["schema", "name", "kind", "scope"])?;
        let schema = wu::get_text(map, "schema")?;
        if schema != DOMAIN_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "domain record schema `{schema}` is not `{DOMAIN_SCHEMA}`"
            )));
        }
        let record = DomainRecord {
            name: wu::get_text(map, "name")?,
            kind: DomainKind::parse(&wu::get_text(map, "kind")?)?,
            scope: wu::get_text(map, "scope")?,
        };
        record.validate()?;
        Ok(record)
    }
}

/// One registered domain in the discovery record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainEntry {
    pub record: DomainRecord,
    pub id: Hash32,
    pub first_generation: u64,
}

impl DomainEntry {
    pub fn new(record: DomainRecord) -> Result<Self> {
        let id = record.id()?;
        Ok(DomainEntry {
            record,
            id,
            first_generation: 1,
        })
    }

    fn to_value(&self) -> Value {
        wu::map([
            ("id", wu::hash_value(self.id)),
            ("name", Value::Str(self.record.name.clone())),
            ("kind", Value::Str(self.record.kind.as_str().to_owned())),
            ("scope", Value::Str(self.record.scope.clone())),
            ("first_generation", Value::U64(self.first_generation)),
        ])
    }

    fn from_wcb(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["id", "name", "kind", "scope", "first_generation"])?;
        let record = DomainRecord {
            name: wu::get_text(map, "name")?,
            kind: DomainKind::parse(&wu::get_text(map, "kind")?)?,
            scope: wu::get_text(map, "scope")?,
        };
        record.validate()?;
        let id = wu::get_hash(map, "id")?;
        if record.id()? != id {
            return Err(Error::corrupt(
                "discovery entry id is not the derived DomainId of its record",
            ));
        }
        Ok(DomainEntry {
            record,
            id,
            first_generation: wu::get_u64(map, "first_generation")?,
        })
    }
}

/// The canonical discovery record `domains.root`.
///
/// It is a *discovery aid*: a domain head is authoritative even when it is
/// absent here. The epoch is monotonic and increases on every change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainIndex {
    pub epoch: u64,
    pub domains: Vec<DomainEntry>,
}

impl Default for DomainIndex {
    fn default() -> Self {
        DomainIndex {
            epoch: 0,
            domains: Vec::new(),
        }
    }
}

impl DomainIndex {
    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(DOMAINS_SCHEMA.to_owned())),
            ("epoch", Value::U64(self.epoch)),
            (
                "domains",
                Value::Array(self.domains.iter().map(DomainEntry::to_value).collect()),
            ),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        let map = wu::as_map(&value)?;
        wu::check_keys(map, &["schema", "epoch", "domains"])?;
        let schema = wu::get_text(map, "schema")?;
        if schema != DOMAINS_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "discovery schema `{schema}` is not `{DOMAINS_SCHEMA}`"
            )));
        }
        let mut domains = wu::get_array(map, "domains")?
            .iter()
            .map(DomainEntry::from_wcb)
            .collect::<Result<Vec<_>>>()?;
        if domains.len() > MAX_DOMAINS {
            return Err(Error::limit_exceeded(format!(
                "discovery record names {} domains, above the {MAX_DOMAINS} bound",
                domains.len()
            )));
        }
        // Canonical order: sorted by DomainId, unique.
        let mut previous: Option<Hash32> = None;
        for entry in &domains {
            if let Some(prev) = previous {
                if entry.id <= prev {
                    return Err(Error::non_canonical(
                        "discovery domains are not sorted by unique DomainId",
                    ));
                }
            }
            previous = Some(entry.id);
        }
        domains.shrink_to_fit();
        Ok(DomainIndex {
            epoch: wu::get_u64(map, "epoch")?,
            domains,
        })
    }

    pub fn find(&self, id: &Hash32) -> Option<&DomainEntry> {
        self.domains.iter().find(|entry| &entry.id == id)
    }

    pub fn find_name(&self, name: &str, kind: DomainKind, scope: &str) -> Option<&DomainEntry> {
        self.domains.iter().find(|entry| {
            entry.record.name == name && entry.record.kind == kind && entry.record.scope == scope
        })
    }

    /// Insert a domain, keeping the record canonical. Returns `false` when the
    /// id was already present with the same record (idempotent registration).
    pub fn insert(&mut self, entry: DomainEntry) -> Result<bool> {
        match self
            .domains
            .iter()
            .position(|existing| existing.id == entry.id)
        {
            Some(index) => {
                if self.domains[index] == entry {
                    Ok(false)
                } else {
                    Err(Error::conflict(
                        "a domain with this DomainId is already registered with a different \
                         record",
                    ))
                }
            }
            None => {
                if self.domains.len() >= MAX_DOMAINS {
                    return Err(Error::limit_exceeded(format!(
                        "discovery record already names {MAX_DOMAINS} domains"
                    )));
                }
                self.domains.push(entry);
                self.domains.sort_by_key(|entry| entry.id);
                self.epoch = self.epoch.saturating_add(1);
                Ok(true)
            }
        }
    }
}

/// One `(domain, generation, head)` pair in a read view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ViewPair {
    pub domain: Hash32,
    pub generation: u64,
    pub head: Hash32,
}

/// A declared read view. **Not** a global linearizable point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadView {
    pub pairs: Vec<ViewPair>,
    pub view_root: Hash32,
}

impl ReadView {
    pub fn capture(pairs: Vec<ViewPair>) -> Result<Self> {
        if pairs.len() > MAX_VIEW_DOMAINS {
            return Err(Error::limit_exceeded(format!(
                "read view names {} domains, above the {MAX_VIEW_DOMAINS} bound",
                pairs.len()
            )));
        }
        let mut sorted = pairs.clone();
        sorted.sort_by_key(|pair| pair.domain);
        // A view names each domain at most once.
        for window in sorted.windows(2) {
            if window[0].domain == window[1].domain {
                return Err(Error::invalid_argument(
                    "read view names the same domain twice",
                ));
            }
        }
        Ok(ReadView {
            view_root: view_root(&sorted)?,
            pairs,
        })
    }

    pub fn pair(&self, domain: Hash32) -> Option<ViewPair> {
        self.pairs
            .iter()
            .copied()
            .find(|pair| pair.domain == domain)
    }
}

fn pair_value(pair: &ViewPair) -> Value {
    wu::map([
        ("domain", wu::hash_value(pair.domain)),
        ("generation", Value::U64(pair.generation)),
        ("head", wu::hash_value(pair.head)),
    ])
}

/// `H("weft/view/v1", WCB({capture_kind:"sequential", pairs}))`.
///
/// The root identifies *this capture*. It is not a state root and cannot be
/// compared across captures.
pub fn view_root(pairs: &[ViewPair]) -> Result<Hash32> {
    let value = wu::map([
        ("capture_kind", Value::Str("sequential".to_owned())),
        (
            "pairs",
            Value::Array(pairs.iter().map(pair_value).collect()),
        ),
    ]);
    let bytes = wu::encode(&value)?;
    Ok(identity::h(VIEW_DOMAIN, &bytes))
}

/// Encode a durable cross-domain view pin (`weft.view.pin.v1`).
pub fn encode_view_pin(view: &ReadView, principal: &str, pinned_ns: i64) -> Result<Vec<u8>> {
    let mut sorted = view.pairs.clone();
    sorted.sort_by_key(|pair| pair.domain);
    let value = wu::map([
        ("schema", Value::Str(VIEW_PIN_SCHEMA.to_owned())),
        ("capture_kind", Value::Str("sequential".to_owned())),
        ("view_root", wu::hash_value(view.view_root)),
        ("principal", Value::Str(principal.to_owned())),
        ("pinned_ns", Value::Timestamp(pinned_ns)),
        (
            "pairs",
            Value::Array(sorted.iter().map(pair_value).collect()),
        ),
    ]);
    wu::encode(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> DomainRecord {
        DomainRecord::new("qwen2.5", DomainKind::ModelBranch, "instruct").unwrap()
    }

    #[test]
    fn domain_id_is_derived_and_kind_sensitive() {
        let a = record();
        assert_eq!(a.id().unwrap(), record().id().unwrap());
        let b = DomainRecord::new("qwen2.5", DomainKind::CheckpointFamily, "instruct").unwrap();
        assert_ne!(a.id().unwrap(), b.id().unwrap());
    }

    #[test]
    fn domain_record_round_trips_and_validates() {
        let bytes = record().encode().unwrap();
        let decoded = DomainRecord::from_wcb(&wu::decode(&bytes).unwrap()).unwrap();
        assert_eq!(decoded, record());
        assert!(DomainRecord::new("", DomainKind::Namespace, "").is_err());
        assert!(DomainRecord::new("x", DomainKind::Namespace, "a\0b").is_err());
    }

    #[test]
    fn discovery_is_sorted_and_epochs_are_monotonic() {
        let mut index = DomainIndex::default();
        assert!(index.insert(DomainEntry::new(record()).unwrap()).unwrap());
        assert_eq!(index.epoch, 1);
        assert!(!index.insert(DomainEntry::new(record()).unwrap()).unwrap());
        assert_eq!(index.epoch, 1);
        let bytes = index.encode().unwrap();
        assert_eq!(DomainIndex::decode(&bytes).unwrap(), index);
    }

    #[test]
    fn view_root_separates_domains_and_generations() {
        let a = ViewPair {
            domain: Hash32::ZERO,
            generation: 1,
            head: Hash32::ZERO,
        };
        let mut b = a;
        b.generation = 2;
        assert_ne!(view_root(&[a]).unwrap(), view_root(&[b]).unwrap());
        let view = ReadView::capture(vec![a, b]).unwrap();
        assert_eq!(view.pairs.len(), 2);
        assert!(ReadView::capture(vec![a, a]).is_err());
    }
}
