//! Write-budget (quota) records.
//!
//! A quota is an **admission** budget, not a footprint measurement: every
//! commit is charged the bytes and object records it would add, and a
//! principal that would cross its configured limit is refused *before* the
//! commit is published. Reusing content that is already reachable from
//! committed state costs zero.
//!
//! The persisted `quota` record holds the policy plus the cumulative usage
//! counters and the generation/commit they were computed for. Usage is only
//! ever derived from committed state (see `Store::quota_rebuild`), never from
//! process memory, so it survives reopen. A record that does not match the
//! committed head fails closed: quota-enforcing writes are refused until the
//! explicit `quota rebuild` admin operation recomputes it.
//!
//! Principal attribution:
//!
//! - `Posix { uid, gid }`: every new or changed object record is charged to its
//!   own `(uid, gid)`.
//! - `Tenant(name)`: additionally charged for every such record carrying the
//!   reserved `weft.tenant` attribute. The attribute is ordinary object state,
//!   so tenant usage is rebuildable from the commit chain exactly like POSIX
//!   usage.
//!
//! Denial messages are fixed text. They never contain current usage, limits of
//! another principal, or the identity of the principal that was refused.

use std::collections::BTreeMap;

use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::wcb_util as wu;

/// Legacy combined policy+usage record schema (read-only input to migration).
pub const QUOTA_SCHEMA: &str = "weftfs.quota.v1";
/// File magic for the persisted legacy record.
pub const QUOTA_MAGIC: &[u8; 4] = b"WFQ1";
/// Domain separation for the legacy record digest.
pub const QUOTA_DOMAIN: &str = "weft/quota/v1";
/// Policy-only record schema. Usage lives in the commit log as
/// [`QuotaUsageOp`] and is never written to this file.
pub const QUOTA_POLICY_SCHEMA: &str = "weftfs.quota.v2";
/// File magic for the persisted policy-only record.
pub const QUOTA_POLICY_MAGIC: &[u8; 4] = b"WFQ2";
/// Domain separation for the policy-only record digest.
pub const QUOTA_POLICY_DOMAIN: &str = "weft/quota/v2";
/// Maximum number of configured budgets in one store.
pub const MAX_QUOTA_PRINCIPALS: usize = 4_096;
/// Maximum tenant label length, matching the capability envelope bound.
pub const MAX_TENANT_BYTES: usize = 256;

/// The principal a budget is attached to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuotaPrincipal {
    Posix { uid: u64, gid: u64 },
    Tenant(String),
}

impl QuotaPrincipal {
    /// Canonical ordering key. POSIX principals sort before tenant principals;
    /// the key is the only sort input, so Rust and Go build the same arrays.
    pub fn sort_key(&self) -> String {
        match self {
            QuotaPrincipal::Posix { uid, gid } => format!("p:{uid:020}:{gid:020}"),
            QuotaPrincipal::Tenant(name) => format!("t:{name}"),
        }
    }

    /// `uid:gid` or `tenant:<name>`.
    pub fn display(&self) -> String {
        match self {
            QuotaPrincipal::Posix { uid, gid } => format!("{uid}:{gid}"),
            QuotaPrincipal::Tenant(name) => format!("tenant:{name}"),
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            QuotaPrincipal::Posix { .. } => Ok(()),
            QuotaPrincipal::Tenant(name) => {
                if name.is_empty() || name.len() > MAX_TENANT_BYTES {
                    return Err(Error::invalid_argument(format!(
                        "tenant label must be 1..={MAX_TENANT_BYTES} bytes"
                    )));
                }
                Ok(())
            }
        }
    }
}

/// One configured budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaBudget {
    pub principal: QuotaPrincipal,
    pub max_new_bytes: u64,
    pub max_objects: u64,
}

/// Cumulative admitted usage for one principal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct QuotaCounters {
    pub used_bytes: u64,
    pub used_objects: u64,
}

impl QuotaCounters {
    pub fn add(&mut self, other: &QuotaCounters) -> Result<()> {
        self.used_bytes = self
            .used_bytes
            .checked_add(other.used_bytes)
            .ok_or_else(|| Error::limit_exceeded("quota byte counter overflow"))?;
        self.used_objects = self
            .used_objects
            .checked_add(other.used_objects)
            .ok_or_else(|| Error::limit_exceeded("quota object counter overflow"))?;
        Ok(())
    }
}

/// The committed usage assertion carried by one commit.
///
/// `reset == false` asserts the cumulative admitted usage after every other op
/// in the same commit. `reset == true` is the explicit admin reset: it must be
/// the only op in its commit and must declare an empty usage map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuotaUsageOp {
    pub reset: bool,
    pub usage: Vec<(QuotaPrincipal, QuotaCounters)>,
}

impl QuotaUsageOp {
    /// Validate and sort: canonical principal order, no duplicates, bounded
    /// tenant labels.
    pub fn normalize(mut self) -> Result<Self> {
        if self.usage.len() > MAX_QUOTA_PRINCIPALS {
            return Err(Error::limit_exceeded(format!(
                "at most {MAX_QUOTA_PRINCIPALS} quota principals are supported"
            )));
        }
        for (principal, _) in &self.usage {
            principal.validate()?;
        }
        self.usage
            .sort_by(|a, b| a.0.sort_key().cmp(&b.0.sort_key()));
        self.usage.dedup_by(|a, b| a.0 == b.0);
        if self.reset && !self.usage.is_empty() {
            return Err(Error::malformed(
                "a quota usage reset must declare an empty usage map",
            ));
        }
        Ok(self)
    }

    /// Canonical map value, including the `op` discriminator.
    pub fn to_value(&self) -> Value {
        wu::map([
            ("op", Value::Str("set_quota_usage".to_owned())),
            ("reset", Value::Bool(self.reset)),
            ("usage", Value::Array(usage_to_values(&self.usage))),
        ])
    }

    /// Parse from the op map with the `op` key removed.
    pub fn from_map(map: &std::collections::BTreeMap<String, Value>) -> Result<Self> {
        wu::check_keys(map, &["reset", "usage"])?;
        let reset = match wu::field(map, "reset")? {
            Value::Bool(value) => *value,
            _ => return Err(Error::malformed("quota usage reset must be a bool")),
        };
        let usage = match wu::field(map, "usage")? {
            Value::Array(items) => usage_from_values(items)?,
            _ => return Err(Error::malformed("quota usage must be an array")),
        };
        let op = QuotaUsageOp {
            reset,
            usage: usage.clone(),
        }
        .normalize()?;
        if op.usage != usage {
            return Err(Error::non_canonical(
                "quota usage is not in canonical principal order",
            ));
        }
        Ok(op)
    }
}

/// The policy-only `quota` file (`weftfs.quota.v2`). Usage is intentionally
/// absent: it is committed to the commit log instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaPolicyRecord {
    pub budgets: Vec<QuotaBudget>,
}

impl QuotaPolicyRecord {
    pub fn new(mut budgets: Vec<QuotaBudget>) -> Result<Self> {
        budgets = normalize_policy(budgets)?;
        Ok(QuotaPolicyRecord { budgets })
    }

    pub fn to_value(&self) -> Value {
        let budgets: Vec<Value> = self
            .budgets
            .iter()
            .map(|budget| {
                let mut map = principal_value(&budget.principal);
                map.insert("max_new_bytes".to_owned(), Value::U64(budget.max_new_bytes));
                map.insert("max_objects".to_owned(), Value::U64(budget.max_objects));
                Value::Map(map)
            })
            .collect();
        wu::map([
            ("schema", Value::Str(QUOTA_POLICY_SCHEMA.to_owned())),
            ("budgets", Value::Array(budgets)),
        ])
    }

    pub fn encode_bytes(&self) -> Result<Vec<u8>> {
        let wcb = wu::encode(&self.to_value())?;
        if wcb.len() > 64 * 1024 * 1024 {
            return Err(Error::limit_exceeded(
                "quota policy record is implausibly large",
            ));
        }
        let digest = identity::h(QUOTA_POLICY_DOMAIN, &wcb);
        let mut out = Vec::with_capacity(4 + 8 + wcb.len() + 32);
        out.extend_from_slice(QUOTA_POLICY_MAGIC);
        out.extend_from_slice(&(wcb.len() as u64).to_le_bytes());
        out.extend_from_slice(&wcb);
        out.extend_from_slice(digest.as_bytes());
        Ok(out)
    }

    pub fn decode_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 + 8 + 32 {
            return Err(Error::corrupt("quota policy record is truncated"));
        }
        if &bytes[0..4] != QUOTA_POLICY_MAGIC {
            return Err(Error::corrupt("quota policy record has the wrong magic"));
        }
        let len = u64::from_le_bytes(
            bytes[4..12]
                .try_into()
                .map_err(|_| Error::corrupt("quota policy record length is truncated"))?,
        );
        let len = usize::try_from(len)
            .map_err(|_| Error::corrupt("quota policy record length overflows"))?;
        let expected = 12usize
            .checked_add(len)
            .and_then(|value| value.checked_add(32))
            .ok_or_else(|| Error::corrupt("quota policy record length overflows"))?;
        if bytes.len() != expected {
            return Err(Error::corrupt(
                "quota policy record length disagrees with its frame",
            ));
        }
        let wcb = &bytes[12..12 + len];
        let digest = &bytes[12 + len..];
        if identity::h(QUOTA_POLICY_DOMAIN, wcb).as_bytes() != digest {
            return Err(Error::corrupt("quota policy record fails its digest check"));
        }
        let value = wu::decode(wcb)?;
        Self::from_value(&value)
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["schema", "budgets"])?;
        let schema = wu::get_text(map, "schema")?;
        if schema != QUOTA_POLICY_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "quota policy schema `{schema}` is not `{QUOTA_POLICY_SCHEMA}`"
            )));
        }
        let budget_values = match wu::field(map, "budgets")? {
            Value::Array(items) => items,
            _ => return Err(Error::malformed("quota budgets must be an array")),
        };
        let budgets = budgets_from_values(budget_values)?;
        let record = QuotaPolicyRecord::new(budgets.clone())?;
        if record.budgets != budgets {
            return Err(Error::non_canonical(
                "quota budgets are not in canonical principal order",
            ));
        }
        Ok(record)
    }
}

/// The decoded legacy `quota` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaRecord {
    pub generation: u64,
    pub commit: Hash32,
    pub budgets: Vec<QuotaBudget>,
    pub usage: Vec<(QuotaPrincipal, QuotaCounters)>,
}

impl QuotaRecord {
    /// Validate and normalize: budgets and usage are sorted by the canonical
    /// principal key with no duplicates, every counter fits, and every tenant
    /// label is within bounds.
    pub fn normalize(mut self) -> Result<Self> {
        if self.budgets.len() > MAX_QUOTA_PRINCIPALS {
            return Err(Error::limit_exceeded(format!(
                "at most {MAX_QUOTA_PRINCIPALS} quota principals are supported"
            )));
        }
        for budget in &self.budgets {
            budget.principal.validate()?;
        }
        self.budgets
            .sort_by(|a, b| a.principal.sort_key().cmp(&b.principal.sort_key()));
        self.budgets.dedup_by(|a, b| a.principal == b.principal);
        for (principal, _) in &self.usage {
            principal.validate()?;
        }
        self.usage
            .sort_by(|a, b| a.0.sort_key().cmp(&b.0.sort_key()));
        self.usage.dedup_by(|a, b| a.0 == b.0);
        Ok(self)
    }

    /// Canonical map value.
    pub fn to_value(&self) -> Value {
        let budgets: Vec<Value> = self
            .budgets
            .iter()
            .map(|budget| {
                let mut map = principal_value(&budget.principal);
                map.insert("max_new_bytes".to_owned(), Value::U64(budget.max_new_bytes));
                map.insert("max_objects".to_owned(), Value::U64(budget.max_objects));
                Value::Map(map)
            })
            .collect();
        let usage: Vec<Value> = self
            .usage
            .iter()
            .map(|(principal, counters)| {
                let mut map = principal_value(principal);
                map.insert("used_bytes".to_owned(), Value::U64(counters.used_bytes));
                map.insert("used_objects".to_owned(), Value::U64(counters.used_objects));
                Value::Map(map)
            })
            .collect();
        wu::map([
            ("schema", Value::Str(QUOTA_SCHEMA.to_owned())),
            ("generation", Value::U64(self.generation)),
            ("commit", wu::hash_value(self.commit)),
            ("budgets", Value::Array(budgets)),
            ("usage", Value::Array(usage)),
        ])
    }

    /// Encode the on-disk record: `WFQ1 || u64le(len) || WCB || digest`.
    pub fn encode_bytes(&self) -> Result<Vec<u8>> {
        let wcb = wu::encode(&self.to_value())?;
        if wcb.len() > 64 * 1024 * 1024 {
            return Err(Error::limit_exceeded("quota record is implausibly large"));
        }
        let digest = identity::h(QUOTA_DOMAIN, &wcb);
        let mut out = Vec::with_capacity(4 + 8 + wcb.len() + 32);
        out.extend_from_slice(QUOTA_MAGIC);
        out.extend_from_slice(&(wcb.len() as u64).to_le_bytes());
        out.extend_from_slice(&wcb);
        out.extend_from_slice(digest.as_bytes());
        Ok(out)
    }

    /// Decode and validate. Any non-canonical or mismatched bytes are
    /// `Corrupt`; there is no permissive read.
    pub fn decode_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 + 8 + 32 {
            return Err(Error::corrupt("quota record is truncated"));
        }
        if &bytes[0..4] != QUOTA_MAGIC {
            return Err(Error::corrupt("quota record has the wrong magic"));
        }
        let len = u64::from_le_bytes(
            bytes[4..12]
                .try_into()
                .map_err(|_| Error::corrupt("quota record length is truncated"))?,
        );
        let len =
            usize::try_from(len).map_err(|_| Error::corrupt("quota record length overflows"))?;
        let expected = 12usize
            .checked_add(len)
            .and_then(|value| value.checked_add(32))
            .ok_or_else(|| Error::corrupt("quota record length overflows"))?;
        if bytes.len() != expected {
            return Err(Error::corrupt(
                "quota record length disagrees with its frame",
            ));
        }
        let wcb = &bytes[12..12 + len];
        let digest = &bytes[12 + len..];
        if identity::h(QUOTA_DOMAIN, wcb).as_bytes() != digest {
            return Err(Error::corrupt("quota record fails its digest check"));
        }
        let value = wu::decode(wcb)?;
        Self::from_value(&value)
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["schema", "generation", "commit", "budgets", "usage"])?;
        let schema = wu::get_text(map, "schema")?;
        if schema != QUOTA_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "quota schema `{schema}` is not `{QUOTA_SCHEMA}`"
            )));
        }
        let generation = wu::get_u64(map, "generation")?;
        let commit = wu::get_hash(map, "commit")?;
        let budgets = budgets_from_values(match wu::field(map, "budgets")? {
            Value::Array(items) => items,
            _ => return Err(Error::malformed("quota budgets must be an array")),
        })?;
        let usage = usage_from_values(match wu::field(map, "usage")? {
            Value::Array(items) => items,
            _ => return Err(Error::malformed("quota usage must be an array")),
        })?;
        let record = QuotaRecord {
            generation,
            commit,
            budgets: budgets.clone(),
            usage: usage.clone(),
        }
        .normalize()?;
        // Sorting is checked, not silently accepted: canonical bytes must
        // already be in order, so a re-encode round trip must match.
        if record.budgets != budgets {
            return Err(Error::non_canonical(
                "quota budgets are not in canonical principal order",
            ));
        }
        if record.usage != usage {
            return Err(Error::non_canonical(
                "quota usage is not in canonical principal order",
            ));
        }
        Ok(record)
    }
}

fn principal_value(principal: &QuotaPrincipal) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    match principal {
        QuotaPrincipal::Posix { uid, gid } => {
            map.insert("kind".to_owned(), Value::Str("posix".to_owned()));
            map.insert("uid".to_owned(), Value::U64(*uid));
            map.insert("gid".to_owned(), Value::U64(*gid));
        }
        QuotaPrincipal::Tenant(name) => {
            map.insert("kind".to_owned(), Value::Str("tenant".to_owned()));
            map.insert("tenant".to_owned(), Value::Str(name.clone()));
        }
    }
    map
}

fn principal_from_map(map: &BTreeMap<String, Value>) -> Result<QuotaPrincipal> {
    match wu::get_text(map, "kind")?.as_str() {
        "posix" => Ok(QuotaPrincipal::Posix {
            uid: wu::get_u64(map, "uid")?,
            gid: wu::get_u64(map, "gid")?,
        }),
        "tenant" => Ok(QuotaPrincipal::Tenant(wu::get_text(map, "tenant")?)),
        other => Err(Error::malformed(format!(
            "unknown quota principal kind `{other}`"
        ))),
    }
}

fn budgets_from_values(values: &[Value]) -> Result<Vec<QuotaBudget>> {
    let mut budgets = Vec::with_capacity(values.len());
    for item in values {
        let entry = wu::as_map(item)?;
        wu::check_keys(
            entry,
            &[
                "kind",
                "uid",
                "gid",
                "tenant",
                "max_new_bytes",
                "max_objects",
            ],
        )?;
        budgets.push(QuotaBudget {
            principal: principal_from_map(entry)?,
            max_new_bytes: wu::get_u64(entry, "max_new_bytes")?,
            max_objects: wu::get_u64(entry, "max_objects")?,
        });
    }
    Ok(budgets)
}

fn usage_to_values(usage: &[(QuotaPrincipal, QuotaCounters)]) -> Vec<Value> {
    usage
        .iter()
        .map(|(principal, counters)| {
            let mut map = principal_value(principal);
            map.insert("used_bytes".to_owned(), Value::U64(counters.used_bytes));
            map.insert("used_objects".to_owned(), Value::U64(counters.used_objects));
            Value::Map(map)
        })
        .collect()
}

fn usage_from_values(values: &[Value]) -> Result<Vec<(QuotaPrincipal, QuotaCounters)>> {
    let mut usage = Vec::with_capacity(values.len());
    for item in values {
        let entry = wu::as_map(item)?;
        wu::check_keys(
            entry,
            &["kind", "uid", "gid", "tenant", "used_bytes", "used_objects"],
        )?;
        usage.push((
            principal_from_map(entry)?,
            QuotaCounters {
                used_bytes: wu::get_u64(entry, "used_bytes")?,
                used_objects: wu::get_u64(entry, "used_objects")?,
            },
        ));
    }
    Ok(usage)
}

/// Runtime quota state held by an open [`Store`](crate::store::Store).
///
/// `valid == false` means the persisted record disagreed with committed state
/// (or could not be decoded); quota-enforcing writes then fail closed until
/// `quota rebuild` recomputes it. A store with no budgets never consults it.
#[derive(Clone, Debug)]
pub struct QuotaState {
    pub policy: BTreeMap<QuotaPrincipal, QuotaBudget>,
    pub usage: BTreeMap<QuotaPrincipal, QuotaCounters>,
    pub generation: u64,
    pub commit: Hash32,
    pub valid: bool,
    /// True when a persisted record was found (even if unusable).
    pub record_present: bool,
    /// True when `usage` came from a committed `set_quota_usage` op (or was
    /// just published as one). False means it was derived by replay and the
    /// head does not yet carry an assertion.
    pub usage_committed: bool,
    /// True when a legacy `quota.migrated` record is present.
    pub migrated_present: bool,
    pub reason: Option<String>,
}

impl Default for QuotaState {
    fn default() -> Self {
        QuotaState {
            policy: BTreeMap::new(),
            usage: BTreeMap::new(),
            generation: 0,
            commit: Hash32::ZERO,
            valid: true,
            record_present: false,
            usage_committed: false,
            migrated_present: false,
            reason: None,
        }
    }
}

impl QuotaState {
    pub fn enforced(&self) -> bool {
        !self.policy.is_empty()
    }
}

/// Parse one `--quota` argument.
///
/// Accepted forms:
/// - `uid:GID:BYTES:OBJECTS`
/// - `tenant:NAME:BYTES:OBJECTS`
pub fn parse_budget(text: &str) -> Result<QuotaBudget> {
    let parts: Vec<&str> = text.split(':').collect();
    let number = |value: &str, what: &str| -> Result<u64> {
        value.parse::<u64>().map_err(|_| {
            Error::invalid_argument(format!(
                "--quota {what} must be a non-negative integer (got `{value}`)"
            ))
        })
    };
    match parts.as_slice() {
        ["tenant", name, bytes, objects] => Ok(QuotaBudget {
            principal: QuotaPrincipal::Tenant((*name).to_owned()),
            max_new_bytes: number(bytes, "bytes")?,
            max_objects: number(objects, "objects")?,
        }),
        [uid, gid, bytes, objects] => Ok(QuotaBudget {
            principal: QuotaPrincipal::Posix {
                uid: number(uid, "uid")?,
                gid: number(gid, "gid")?,
            },
            max_new_bytes: number(bytes, "bytes")?,
            max_objects: number(objects, "objects")?,
        }),
        _ => Err(Error::invalid_argument(format!(
            "--quota expects `uid:GID:BYTES:OBJECTS` or `tenant:NAME:BYTES:OBJECTS` (got `{text}`)"
        ))),
    }
}

/// Parse a quota file: one directive per line, blank lines and `#` comments
/// ignored.
pub fn parse_quota_file(contents: &str) -> Result<Vec<QuotaBudget>> {
    let mut budgets = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        budgets.push(parse_budget(line).map_err(|err| {
            Error::invalid_argument(format!("quota file line {}: {err}", index + 1))
        })?);
    }
    Ok(budgets)
}

/// Normalize a policy: sort by canonical key, reject duplicate principals.
pub fn normalize_policy(mut budgets: Vec<QuotaBudget>) -> Result<Vec<QuotaBudget>> {
    if budgets.len() > MAX_QUOTA_PRINCIPALS {
        return Err(Error::limit_exceeded(format!(
            "at most {MAX_QUOTA_PRINCIPALS} quota principals are supported"
        )));
    }
    for budget in &budgets {
        budget.principal.validate()?;
    }
    budgets.sort_by(|a, b| a.principal.sort_key().cmp(&b.principal.sort_key()));
    for pair in budgets.windows(2) {
        if pair[0].principal == pair[1].principal {
            return Err(Error::invalid_argument(format!(
                "duplicate quota for principal `{}`",
                pair[0].principal.display()
            )));
        }
    }
    Ok(budgets)
}
