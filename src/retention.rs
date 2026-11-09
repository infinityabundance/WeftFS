//! Retention bindings (subphase R6).
//!
//! An artifact's **identity** is immutable (`descriptor_root` for a mode-19 KV
//! page). Its **retention binding** is a separate, mutable, versioned record
//! that declares the class, the producing run, the declared useful lifetime,
//! and the policy epoch. No field of a binding is an input to any content root,
//! `ContentId`, `ObjectId`, manifest, descriptor, or codec frame: changing a
//! policy writes one metadata record and zero artifact bytes.
//!
//! The normative contract is `spec/WEFT-RETENTION.md`. The load-bearing rule is
//! §5.3: reachability is computed first, and class affects **ordering**, never
//! reachability safety. A binding may make the planner *retain* unreachable
//! bytes (the declared window); it can never make it reclaim a reachable one.
//!
//! Layout (ordinary committed state, so it snapshots, replicates, verifies and
//! recovers with everything else):
//!
//! ```text
//! /retention/bindings/<binding_id_hex>    immutable canonical binding record
//! /retention/current/<descriptor_hex>     fixed 104-byte current pointer
//! ```
//!
//! The pointer names the binding and the artifact's manifest `ContentId`. The
//! anchor is metadata, not identity: it lets a planner map a binding to blobs
//! without replaying history.

use std::collections::{BTreeMap, BTreeSet};

use weftcodec::wcb::Value;

use crate::backend::{Key, KeyKind};
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::kv_capability::ATTR_KV_PROV_ROOT;
use crate::state::ObjectKind;
use crate::store::{collect_unmarked, mark_manifest_tree, GcReport, Reachability, Store};
use crate::wcb_util as wu;

/// The binding-record schema string.
pub const RETENTION_SCHEMA: &str = "weft.retention.v1";
/// Domain separation for `RetentionId = H(RETENTION_DOMAIN, WCB(record))`.
pub const RETENTION_DOMAIN: &str = "weft/retention/v1";
/// Highest declared lifetime accepted (100 years in milliseconds).
pub const MAX_DECLARED_LIFETIME_MS: u64 = 3_155_760_000_000;
/// Longest `supersedes` chain a rebind may extend.
pub const MAX_RETENTION_HISTORY: usize = 4096;
/// Producer string byte bound.
pub const MAX_PRODUCER_BYTES: usize = 256;
/// Fixed current-pointer size: `"RPT1" || version || 3 zero || 3 x 32`.
pub const POINTER_BYTES: usize = 104;
const POINTER_MAGIC: &[u8; 4] = b"RPT1";
const POINTER_VERSION: u8 = 1;
/// Reserved namespace root.
pub const RETENTION_DIR: &str = "retention";
/// Immutable binding records.
pub const RETENTION_BINDINGS_DIR: &str = "bindings";
/// Mutable current pointers.
pub const RETENTION_CURRENT_DIR: &str = "current";

/// The four semantic retention classes. Ordering rank is normative (§5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RetentionClass {
    Volatile,
    Session,
    Ephemeral,
    Durable,
}

impl RetentionClass {
    pub const ALL: [RetentionClass; 4] = [
        RetentionClass::Volatile,
        RetentionClass::Session,
        RetentionClass::Ephemeral,
        RetentionClass::Durable,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RetentionClass::Volatile => "volatile",
            RetentionClass::Session => "session",
            RetentionClass::Ephemeral => "ephemeral",
            RetentionClass::Durable => "durable",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "volatile" => Ok(RetentionClass::Volatile),
            "session" => Ok(RetentionClass::Session),
            "ephemeral" => Ok(RetentionClass::Ephemeral),
            "durable" => Ok(RetentionClass::Durable),
            other => Err(Error::malformed(format!(
                "retention class `{other}` is not one of volatile, session, ephemeral, durable"
            ))),
        }
    }

    /// Reclamation rank: a lower rank is reclaimed first. `durable` is last.
    pub fn rank(self) -> u8 {
        match self {
            RetentionClass::Volatile => 1,
            RetentionClass::Session => 2,
            RetentionClass::Ephemeral => 3,
            RetentionClass::Durable => 4,
        }
    }

    pub fn requires_root(self) -> bool {
        self == RetentionClass::Durable
    }
}

impl std::fmt::Display for RetentionClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Optional Ed25519 signature over the binding record without its signature
/// key. The algorithm name is fixed in v1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionSignature {
    pub public_key: [u8; 32],
    pub signature: [u8; 64],
}

impl RetentionSignature {
    fn to_value(&self) -> Value {
        wu::map([
            ("algorithm", Value::Str("ed25519".to_owned())),
            ("public_key", Value::Bytes(self.public_key.to_vec())),
            ("signature", Value::Bytes(self.signature.to_vec())),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["algorithm", "public_key", "signature"])?;
        let algorithm = wu::get_text(map, "algorithm")?;
        if algorithm != "ed25519" {
            return Err(Error::malformed(format!(
                "retention signature algorithm `{algorithm}` is not `ed25519`"
            )));
        }
        let public_key = wu::get_bytes(map, "public_key")?;
        let signature = wu::get_bytes(map, "signature")?;
        let public_key: [u8; 32] = public_key
            .try_into()
            .map_err(|_| Error::malformed("retention signature public_key must be 32 bytes"))?;
        let signature: [u8; 64] = signature
            .try_into()
            .map_err(|_| Error::malformed("retention signature must be 64 bytes"))?;
        Ok(RetentionSignature {
            public_key,
            signature,
        })
    }
}

/// One canonical `weft.retention.v1` binding record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionBinding {
    pub descriptor_root: Hash32,
    pub class: RetentionClass,
    /// Admission hint in milliseconds; `0` only for `durable`.
    pub declared_lifetime_ms: u64,
    pub producer: String,
    pub policy_epoch: u64,
    pub issued_at_ns: i64,
    pub supersedes: Option<Hash32>,
    pub signature: Option<RetentionSignature>,
}

impl RetentionBinding {
    pub const KEYS: &'static [&'static str] = &[
        "schema",
        "descriptor_root",
        "class",
        "declared_lifetime_ms",
        "producer",
        "policy_epoch",
        "issued_at",
        "supersedes",
        "signature",
    ];

    /// Build and validate a binding (unsigned). Signing is a separate step.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        descriptor_root: Hash32,
        class: RetentionClass,
        declared_lifetime_ms: u64,
        producer: impl Into<String>,
        policy_epoch: u64,
        issued_at_ns: i64,
        supersedes: Option<Hash32>,
    ) -> Result<Self> {
        let binding = RetentionBinding {
            descriptor_root,
            class,
            declared_lifetime_ms,
            producer: producer.into(),
            policy_epoch,
            issued_at_ns,
            supersedes,
            signature: None,
        };
        binding.validate()?;
        Ok(binding)
    }

    /// Structural validation (§3 and §7 steps 3–7).
    pub fn validate(&self) -> Result<()> {
        if self.descriptor_root == Hash32::ZERO {
            return Err(Error::malformed(
                "retention descriptor_root must not be all zero",
            ));
        }
        if self.producer.is_empty() {
            return Err(Error::malformed("retention producer must not be empty"));
        }
        if self.producer.len() > MAX_PRODUCER_BYTES {
            return Err(Error::malformed(format!(
                "retention producer is {} bytes, above the {MAX_PRODUCER_BYTES}-byte bound",
                self.producer.len()
            )));
        }
        match self.class {
            RetentionClass::Durable => {
                if self.declared_lifetime_ms != 0 {
                    return Err(Error::malformed(
                        "a durable retention binding must declare lifetime 0 (indefinite)",
                    ));
                }
            }
            _ => {
                if self.declared_lifetime_ms == 0 {
                    return Err(Error::malformed(format!(
                        "a {} retention binding must declare a positive lifetime",
                        self.class
                    )));
                }
                if self.declared_lifetime_ms > MAX_DECLARED_LIFETIME_MS {
                    return Err(Error::limit_exceeded(format!(
                        "declared lifetime {} ms exceeds the {MAX_DECLARED_LIFETIME_MS} ms bound",
                        self.declared_lifetime_ms
                    )));
                }
            }
        }
        if let Some(supersedes) = self.supersedes {
            if supersedes == Hash32::ZERO {
                return Err(Error::malformed(
                    "retention supersedes must not be all zero (use null)",
                ));
            }
        }
        Ok(())
    }

    fn to_value_without_signature(&self) -> Value {
        wu::map([
            ("schema", Value::Str(RETENTION_SCHEMA.to_owned())),
            ("descriptor_root", wu::hash_value(self.descriptor_root)),
            ("class", Value::Str(self.class.as_str().to_owned())),
            (
                "declared_lifetime_ms",
                Value::U64(self.declared_lifetime_ms),
            ),
            ("producer", Value::Str(self.producer.clone())),
            ("policy_epoch", Value::U64(self.policy_epoch)),
            ("issued_at", Value::Timestamp(self.issued_at_ns)),
            ("supersedes", wu::opt_hash_value(self.supersedes)),
        ])
    }

    pub fn to_value(&self) -> Value {
        let mut map = match self.to_value_without_signature() {
            Value::Map(map) => map,
            _ => unreachable!("retention signing map is always a WCB map"),
        };
        map.insert(
            "signature".to_owned(),
            match &self.signature {
                Some(signature) => signature.to_value(),
                None => Value::Null,
            },
        );
        Value::Map(map)
    }

    /// Parse a canonical record. `signature`, when present, is verified
    /// structurally and cryptographically; trust-set membership is the store's
    /// decision (`verify_signature_with_trust`).
    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != RETENTION_SCHEMA {
            return Err(Error::malformed(format!(
                "retention schema `{schema}` is not `{RETENTION_SCHEMA}`"
            )));
        }
        let descriptor_root = wu::get_hash(map, "descriptor_root")?;
        let class = RetentionClass::parse(&wu::get_text(map, "class")?)?;
        let declared_lifetime_ms = wu::get_u64(map, "declared_lifetime_ms")?;
        let producer = wu::get_text(map, "producer")?;
        let policy_epoch = wu::get_u64(map, "policy_epoch")?;
        let issued_at_ns = wu::get_timestamp(map, "issued_at")?;
        let supersedes = wu::get_opt_hash(map, "supersedes")?;
        let signature = match wu::field(map, "signature")? {
            Value::Null => None,
            value => Some(RetentionSignature::from_value(value)?),
        };
        let binding = RetentionBinding {
            descriptor_root,
            class,
            declared_lifetime_ms,
            producer,
            policy_epoch,
            issued_at_ns,
            supersedes,
            signature,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    /// The exact bytes a signature covers.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value_without_signature())
    }

    /// Decode and, when a signature exists, verify it.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        let binding = Self::from_value(&value)?;
        binding.verify_signature()?;
        Ok(binding)
    }

    /// `RetentionId = H(RETENTION_DOMAIN, WCB(record))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(RETENTION_DOMAIN, &self.encode()?))
    }

    /// Sign in place with an Ed25519 seed. The signature covers the record
    /// without its `signature` key.
    pub fn sign(&mut self, seed: &[u8; 32]) -> Result<()> {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(seed);
        let signature = key.sign(&self.signing_bytes()?);
        self.signature = Some(RetentionSignature {
            public_key: key.verifying_key().to_bytes(),
            signature: signature.to_bytes(),
        });
        Ok(())
    }

    /// Verify a present signature against its embedded key. A missing signature
    /// is accepted here; trust-set policy is enforced separately.
    pub fn verify_signature(&self) -> Result<()> {
        let Some(signature) = &self.signature else {
            return Ok(());
        };
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let key = VerifyingKey::from_bytes(&signature.public_key)
            .map_err(|err| Error::corrupt(format!("retention verification key: {err}")))?;
        let parsed = Signature::from_bytes(&signature.signature);
        key.verify(&self.signing_bytes()?, &parsed)
            .map_err(|_| Error::corrupt("retention binding signature does not verify"))
    }

    /// Verify the signature and require its key to be in `trust`. An unsigned
    /// binding is `Unauthorized` when a trust set is configured.
    pub fn verify_signature_with_trust(&self, trust: &[[u8; 32]]) -> Result<()> {
        if trust.is_empty() {
            return self.verify_signature();
        }
        let Some(signature) = &self.signature else {
            return Err(Error::unauthorized(
                "retention binding is unsigned and this store requires a trusted signer",
            ));
        };
        self.verify_signature()?;
        if trust.iter().any(|key| key == &signature.public_key) {
            Ok(())
        } else {
            Err(Error::unauthorized(
                "retention binding signer is not in the configured trust set",
            ))
        }
    }

    /// Expiry instant, or `None` for `durable`.
    pub fn expires_at_ns(&self) -> Option<i64> {
        match self.class {
            RetentionClass::Durable => None,
            _ => {
                let ms = i64::try_from(self.declared_lifetime_ms).unwrap_or(i64::MAX);
                Some(
                    self.issued_at_ns
                        .saturating_add(ms.saturating_mul(1_000_000)),
                )
            }
        }
    }

    /// Whether the declared window is still open at `now_ns`. `durable` is
    /// protected by its pin/snapshot root, not by a clock, so it reports
    /// `false` here: if it is unreachable, it is ordered last and reclaimed.
    pub fn is_live(&self, now_ns: i64) -> bool {
        match self.expires_at_ns() {
            Some(expiry) => now_ns < expiry,
            None => false,
        }
    }
}

/// The fixed-size mutable current pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPointer {
    pub descriptor_root: Hash32,
    pub binding: Hash32,
    pub artifact: Hash32,
}

impl RetentionPointer {
    pub fn encode(&self) -> [u8; POINTER_BYTES] {
        let mut out = [0u8; POINTER_BYTES];
        out[..4].copy_from_slice(POINTER_MAGIC);
        out[4] = POINTER_VERSION;
        out[8..40].copy_from_slice(self.descriptor_root.as_bytes());
        out[40..72].copy_from_slice(self.binding.as_bytes());
        out[72..104].copy_from_slice(self.artifact.as_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != POINTER_BYTES {
            return Err(Error::corrupt(format!(
                "retention pointer is {} bytes, expected {POINTER_BYTES}",
                bytes.len()
            )));
        }
        if &bytes[..4] != POINTER_MAGIC {
            return Err(Error::corrupt("retention pointer magic is not `RPT1`"));
        }
        if bytes[4] != POINTER_VERSION {
            return Err(Error::unsupported_version(format!(
                "retention pointer version {} is not 1",
                bytes[4]
            )));
        }
        if bytes[5..8] != [0, 0, 0] {
            return Err(Error::malformed(
                "retention pointer reserved bytes are not zero",
            ));
        }
        Ok(RetentionPointer {
            descriptor_root: Hash32::from_slice(&bytes[8..40]).map_err(Error::from)?,
            binding: Hash32::from_slice(&bytes[40..72]).map_err(Error::from)?,
            artifact: Hash32::from_slice(&bytes[72..104]).map_err(Error::from)?,
        })
    }
}

/// Per-class totals in a GC plan. A blob shared by artifacts of two classes is
/// counted in both, so the per-class sums may exceed the physical total; the
/// physical total is reported separately on the plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionClassSummary {
    pub class: &'static str,
    pub reachable_artifacts: u64,
    pub reachable_blobs: u64,
    pub reachable_bytes: u64,
    pub retained_live_artifacts: u64,
    pub retained_live_blobs: u64,
    pub retained_live_bytes: u64,
    pub reclaimable_artifacts: u64,
    pub reclaimable_blobs: u64,
    pub reclaimable_bytes: u64,
}

/// A binding whose bytes were retained by the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedArtifact {
    pub descriptor_root: Hash32,
    pub class: &'static str,
    pub binding: Hash32,
    pub artifact: Hash32,
    pub expired: bool,
    pub bytes: u64,
    /// `reachable from <root>` or `retention live until <ns>`.
    pub reason: String,
}

/// One reclaimable blob, in plan order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionCandidate {
    pub path: String,
    pub bytes: u64,
    /// `unbound` or the most conservative class of its owning bindings.
    pub class: &'static str,
    pub descriptor_roots: Vec<Hash32>,
    pub expiry_ns: Option<i64>,
}

/// The observable output of `gc_plan` / `retention_gc`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionGcPlan {
    pub applied: bool,
    pub now_ns: i64,
    pub budget_bytes: Option<u64>,
    pub grace_secs: u64,
    pub reachable_manifests: u64,
    pub reachable_chunks: u64,
    pub classes: Vec<RetentionClassSummary>,
    pub retained: Vec<RetainedArtifact>,
    pub candidates: Vec<RetentionCandidate>,
    pub planned_bytes: u64,
    pub planned_blobs: u64,
    /// Unreachable bytes kept only because a binding is live, above the budget.
    pub over_budget_bytes: u64,
    pub reclaimed_bytes: u64,
    pub reclaimed_blobs: u64,
    pub reclaimed_pack_bytes: u64,
    pub problems: Vec<String>,
}

struct Owner {
    descriptor_root: Hash32,
    binding: Hash32,
    class: RetentionClass,
    expiry_ns: Option<i64>,
    live: bool,
    artifact: Hash32,
    artifact_bytes: u64,
    artifact_blobs: Vec<Hash32>,
}

/// Resolve the live object that carries `descriptor_root`, if there is one.
/// Returns `Ambiguous` when several live objects with different content roots
/// claim the same descriptor.
pub(crate) fn descriptor_artifact(store: &Store, descriptor_root: Hash32) -> Result<Hash32> {
    let wanted = descriptor_root.to_hex();
    let mut found: Option<Hash32> = None;
    for record in store.current_state().objects.values() {
        if record.kind != ObjectKind::File {
            continue;
        }
        let Some(Value::Str(text)) = record.attrs.get(ATTR_KV_PROV_ROOT) else {
            continue;
        };
        if text != &wanted {
            continue;
        }
        match found {
            None => found = Some(record.content),
            Some(existing) if existing == record.content => {}
            Some(_) => {
                return Err(Error::ambiguous(format!(
                    "descriptor root {descriptor_root} is carried by objects with different content roots"
                )))
            }
        }
    }
    found.ok_or_else(|| {
        Error::not_found(format!(
            "no live object carries provenance descriptor root {descriptor_root}"
        ))
    })
}

impl Store {
    fn retention_root(&self) -> Result<Option<Hash32>> {
        match self.resolve_path(&format!("/{RETENTION_DIR}")) {
            Ok(id) => Ok(Some(id)),
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn retention_bindings_dir(&self) -> Result<Option<Hash32>> {
        let Some(root) = self.retention_root()? else {
            return Ok(None);
        };
        match self.lookup(root, RETENTION_BINDINGS_DIR.as_bytes()) {
            Ok(id) => Ok(Some(id)),
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn retention_current_dir(&self) -> Result<Option<Hash32>> {
        let Some(root) = self.retention_root()? else {
            return Ok(None);
        };
        match self.lookup(root, RETENTION_CURRENT_DIR.as_bytes()) {
            Ok(id) => Ok(Some(id)),
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn ensure_internal_dir(&mut self, parent: Hash32, name: &str) -> Result<Hash32> {
        let uid = self.options().owner_uid;
        let gid = self.options().owner_gid;
        match self.lookup(parent, name.as_bytes()) {
            Ok(id) => {
                if self.object_meta(id)?.kind != ObjectKind::Dir {
                    return Err(Error::not_dir(format!(
                        "`{name}` exists and is not a directory"
                    )));
                }
                Ok(id)
            }
            Err(err) if err.class() == Class::NotFound => {
                self.mkdir(parent, name.as_bytes(), 0o755, uid, gid)
            }
            Err(err) => Err(err),
        }
    }

    fn ensure_retention_dirs(&mut self) -> Result<(Hash32, Hash32)> {
        let root_id = self.resolve_path("/")?;
        let uid = self.options().owner_uid;
        let gid = self.options().owner_gid;
        let retention = self.ensure_internal_dir(root_id, RETENTION_DIR)?;
        let _ = (uid, gid);
        let bindings = self.ensure_internal_dir(retention, RETENTION_BINDINGS_DIR)?;
        let current = self.ensure_internal_dir(retention, RETENTION_CURRENT_DIR)?;
        Ok((bindings, current))
    }

    /// The current pointer for a descriptor root.
    pub fn retention_pointer(&self, descriptor_root: Hash32) -> Result<Option<RetentionPointer>> {
        let Some(dir) = self.retention_current_dir()? else {
            return Ok(None);
        };
        let name = descriptor_root.to_hex();
        let id = match self.lookup(dir, name.as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let bytes = self.read_object(id)?;
        let pointer = RetentionPointer::decode(&bytes)?;
        if pointer.descriptor_root != descriptor_root {
            return Err(Error::corrupt(format!(
                "retention pointer `{name}` names descriptor root {}",
                pointer.descriptor_root
            )));
        }
        Ok(Some(pointer))
    }

    /// Read and verify one immutable binding by id.
    pub fn retention_binding(&self, id: Hash32) -> Result<RetentionBinding> {
        let Some(dir) = self.retention_bindings_dir()? else {
            return Err(Error::not_found(format!(
                "retention binding {id} does not exist"
            )));
        };
        let name = id.to_hex();
        let object = match self.lookup(dir, name.as_bytes()) {
            Ok(object) => object,
            Err(err) if err.class() == Class::NotFound => {
                return Err(Error::not_found(format!(
                    "retention binding {id} does not exist"
                )))
            }
            Err(err) => return Err(err),
        };
        let bytes = self.read_object(object)?;
        let binding = RetentionBinding::decode(&bytes)?;
        if binding.id()? != id {
            return Err(Error::corrupt(format!(
                "retention binding {id} does not re-encode to its own identity"
            )));
        }
        Ok(binding)
    }

    /// The current binding for a descriptor root.
    pub fn retention_of(&self, descriptor_root: Hash32) -> Result<Option<RetentionBinding>> {
        match self.retention_pointer(descriptor_root)? {
            None => Ok(None),
            Some(pointer) => {
                let binding = self.retention_binding(pointer.binding)?;
                if binding.descriptor_root != descriptor_root {
                    return Err(Error::corrupt(format!(
                        "retention binding {} names descriptor root {}, not {}",
                        pointer.binding, binding.descriptor_root, descriptor_root
                    )));
                }
                Ok(Some(binding))
            }
        }
    }

    /// Follow `supersedes` from the current binding, newest first. Returns
    /// `(RetentionId, binding)` pairs. `limit` is clamped to
    /// [`MAX_RETENTION_HISTORY`].
    pub fn retention_history(
        &self,
        descriptor_root: Hash32,
        limit: usize,
    ) -> Result<Vec<(Hash32, RetentionBinding)>> {
        let limit = limit.min(MAX_RETENTION_HISTORY);
        let mut out = Vec::new();
        let Some(pointer) = self.retention_pointer(descriptor_root)? else {
            return Ok(out);
        };
        let mut next = Some(pointer.binding);
        let mut seen = BTreeSet::new();
        while let Some(id) = next {
            if out.len() >= limit {
                break;
            }
            if !seen.insert(id) {
                return Err(Error::corrupt(format!(
                    "retention history for {descriptor_root} contains a cycle at {id}"
                )));
            }
            let binding = self.retention_binding(id)?;
            next = binding.supersedes;
            out.push((id, binding));
        }
        Ok(out)
    }

    /// Publish a retention binding for an artifact.
    ///
    /// The artifact is resolved from the live object that carries
    /// `descriptor_root` (or from `object` when supplied). Nothing about the
    /// artifact's bytes, content root, descriptor, or dedup key changes.
    pub fn set_retention(&mut self, request: RetentionRequest) -> Result<Hash32> {
        self.require_writable()?;
        let descriptor_root = request.descriptor_root;
        let artifact = match request.object {
            Some(id) => {
                let record = self.object_meta(id)?;
                let expected = descriptor_root.to_hex();
                match record.attrs.get(ATTR_KV_PROV_ROOT) {
                    Some(Value::Str(text)) if *text == expected => record.content,
                    Some(Value::Str(text)) => {
                        return Err(Error::invalid_argument(format!(
                            "object {id} carries provenance root {text}, not {descriptor_root}"
                        )))
                    }
                    _ => {
                        return Err(Error::invalid_argument(format!(
                            "object {id} has no provenance descriptor root"
                        )))
                    }
                }
            }
            None => descriptor_artifact(self, descriptor_root)?,
        };

        let now_ns = request.issued_at_ns.unwrap_or_else(|| self.now_ns());
        let existing = self.retention_pointer(descriptor_root)?;
        let supersedes = match existing {
            Some(pointer) => {
                let current = self.retention_binding(pointer.binding)?;
                if pointer.artifact != artifact {
                    return Err(Error::conflict(format!(
                        "descriptor root {descriptor_root} is already bound to artifact {}",
                        pointer.artifact
                    )));
                }
                if request.policy_epoch < current.policy_epoch {
                    return Err(Error::conflict(format!(
                        "retention policy epoch {} is lower than the current epoch {}",
                        request.policy_epoch, current.policy_epoch
                    )));
                }
                let depth = self.retention_history(descriptor_root, MAX_RETENTION_HISTORY)?;
                if depth.len() >= MAX_RETENTION_HISTORY {
                    return Err(Error::limit_exceeded(format!(
                        "retention history for {descriptor_root} reached the {MAX_RETENTION_HISTORY}-binding bound"
                    )));
                }
                Some(pointer.binding)
            }
            None => None,
        };

        if request.class.requires_root()
            && !self.durable_root_holds(descriptor_root, artifact, now_ns)?
        {
            return Err(Error::invalid_argument(format!(
                "a durable retention binding requires an explicit pin or snapshot root for {descriptor_root}"
            )));
        }

        let mut binding = RetentionBinding::new(
            descriptor_root,
            request.class,
            request.declared_lifetime_ms,
            request.producer,
            request.policy_epoch,
            now_ns,
            supersedes,
        )?;
        if let Some(seed) = &request.sign_seed {
            binding.sign(seed)?;
        }
        if let Some(trust) = &request.trust_keys {
            binding.verify_signature_with_trust(trust)?;
        }
        let binding_id = binding.id()?;

        let (bindings_dir, current_dir) = self.ensure_retention_dirs()?;
        let uid = self.options().owner_uid;
        let gid = self.options().owner_gid;
        let name = binding_id.to_hex();
        let bytes = binding.encode()?;
        match self.create_file(
            bindings_dir,
            name.as_bytes(),
            &bytes,
            0o444,
            uid,
            gid,
            BTreeMap::new(),
        ) {
            Ok(_) => {}
            Err(err) if err.class() == Class::Exists => {
                let existing = self.read_object(self.lookup(bindings_dir, name.as_bytes())?)?;
                if existing != bytes {
                    return Err(Error::corrupt(format!(
                        "retention binding {binding_id} already exists with different bytes"
                    )));
                }
            }
            Err(err) => return Err(err),
        }

        let pointer = RetentionPointer {
            descriptor_root,
            binding: binding_id,
            artifact,
        };
        let pointer_bytes = pointer.encode();
        let pointer_name = descriptor_root.to_hex();
        match self.lookup(current_dir, pointer_name.as_bytes()) {
            Ok(id) => {
                let size = self.object_meta(id)?.size;
                if size as usize != POINTER_BYTES {
                    return Err(Error::corrupt(format!(
                        "existing retention pointer for {descriptor_root} is {size} bytes"
                    )));
                }
                self.write_file(id, 0, &pointer_bytes)?;
            }
            Err(err) if err.class() == Class::NotFound => {
                self.create_file(
                    current_dir,
                    pointer_name.as_bytes(),
                    &pointer_bytes,
                    0o644,
                    uid,
                    gid,
                    BTreeMap::new(),
                )?;
            }
            Err(err) => return Err(err),
        }
        Ok(binding_id)
    }

    /// A `durable` binding needs an explicit pin or a snapshot/checkpoint root.
    fn durable_root_holds(
        &self,
        descriptor_root: Hash32,
        artifact: Hash32,
        now_ns: i64,
    ) -> Result<bool> {
        for record in self.current_state().objects.values() {
            if record.kind != ObjectKind::File || record.content != artifact {
                continue;
            }
            if self.object_pin_count(record.id) > 0 {
                return Ok(true);
            }
        }
        let mut problems = Vec::new();
        let mut expired = Vec::new();
        let reach = self.compute_reachability(now_ns, &mut expired, &mut problems)?;
        if let Some(labels) = reach.origins.get(&artifact) {
            if labels.iter().any(|label| {
                label.starts_with("snapshot:")
                    || label.starts_with("checkpoint:")
                    || label.starts_with("transaction:")
                    || label.starts_with("pin:")
            }) {
                return Ok(true);
            }
        }
        let _ = descriptor_root;
        Ok(false)
    }

    /// Compute a class-aware GC plan without deleting anything.
    pub fn gc_plan(&self, now_ns: i64, budget_bytes: Option<u64>) -> Result<RetentionGcPlan> {
        let (plan, _) = self.build_retention_plan(now_ns, budget_bytes)?;
        Ok(plan)
    }

    /// Execute a class-aware GC plan. `apply == false` is [`Store::gc_plan`].
    pub fn retention_gc(
        &mut self,
        apply: bool,
        now_ns: i64,
        budget_bytes: Option<u64>,
    ) -> Result<RetentionGcPlan> {
        self.require_writable()?;
        if self.in_group() {
            return Err(Error::invalid_argument(
                "retention gc cannot run inside a group commit",
            ));
        }
        let (mut plan, keys) = self.build_retention_plan(now_ns, budget_bytes)?;
        plan.applied = apply;
        if !apply || keys.is_empty() {
            return Ok(plan);
        }
        let bytes = self.with_backend_mut(|backend| backend.delete_many(&keys))?;
        let _ = bytes;
        let reclaim = self.with_backend_mut(|backend| backend.reclaim_unreferenced_packs())?;
        plan.reclaimed_bytes = plan.planned_bytes;
        plan.reclaimed_blobs = plan.planned_blobs;
        plan.reclaimed_pack_bytes = reclaim.deleted_pack_bytes;
        Ok(plan)
    }

    fn build_retention_plan(
        &self,
        now_ns: i64,
        budget_bytes: Option<u64>,
    ) -> Result<(RetentionGcPlan, Vec<Key>)> {
        let mut plan = RetentionGcPlan {
            now_ns,
            budget_bytes,
            grace_secs: self.options().gc_grace_secs,
            ..RetentionGcPlan::default()
        };

        let reach = self.compute_reachability(now_ns, &mut Vec::new(), &mut plan.problems)?;
        plan.reachable_manifests = reach.manifests.len() as u64;
        plan.reachable_chunks = reach.chunks.len() as u64;

        // Collect the current bindings and their artifact blob sets.
        let mut owners: Vec<Owner> = Vec::new();
        if let Some(dir) = self.retention_current_dir()? {
            for entry in self.list_dir(dir)? {
                let name = String::from_utf8_lossy(&entry.name).into_owned();
                let pointer = match self
                    .read_object(entry.target)
                    .and_then(|bytes| RetentionPointer::decode(&bytes))
                {
                    Ok(pointer) => pointer,
                    Err(err) => {
                        plan.problems
                            .push(format!("retention pointer `{name}` is unreadable: {err}"));
                        continue;
                    }
                };
                if pointer.descriptor_root.to_hex() != name {
                    plan.problems.push(format!(
                        "retention pointer `{name}` names descriptor root {}",
                        pointer.descriptor_root
                    ));
                    continue;
                }
                let binding = match self.retention_binding(pointer.binding) {
                    Ok(binding) => binding,
                    Err(err) => {
                        plan.problems.push(format!(
                            "retention binding {} is unreadable: {err}",
                            pointer.binding
                        ));
                        continue;
                    }
                };
                if binding.descriptor_root != pointer.descriptor_root {
                    plan.problems.push(format!(
                        "retention binding {} does not name descriptor root {}",
                        pointer.binding, pointer.descriptor_root
                    ));
                    continue;
                }
                let mut artifact_reach = Reachability::default();
                let mut seen = BTreeSet::new();
                mark_manifest_tree(
                    self,
                    pointer.artifact,
                    &mut artifact_reach,
                    "artifact",
                    &mut plan.problems,
                    0,
                    &mut seen,
                );
                let mut blobs: Vec<Hash32> = artifact_reach
                    .manifests
                    .iter()
                    .chain(artifact_reach.chunks.iter())
                    .copied()
                    .collect();
                blobs.sort_unstable();
                blobs.dedup();
                let mut artifact_bytes = 0u64;
                for hash in &blobs {
                    artifact_bytes =
                        artifact_bytes.saturating_add(self.blob_len(*hash).unwrap_or(0));
                }
                owners.push(Owner {
                    descriptor_root: pointer.descriptor_root,
                    binding: pointer.binding,
                    class: binding.class,
                    expiry_ns: binding.expires_at_ns(),
                    live: binding.is_live(now_ns),
                    artifact: pointer.artifact,
                    artifact_bytes,
                    artifact_blobs: blobs,
                });
            }
        }

        let mut owners_by_blob: BTreeMap<Hash32, Vec<usize>> = BTreeMap::new();
        for (index, owner) in owners.iter().enumerate() {
            for blob in &owner.artifact_blobs {
                owners_by_blob.entry(*blob).or_default().push(index);
            }
        }

        // Per-class summaries and the retained-artifact evidence.
        let mut summaries: BTreeMap<u8, RetentionClassSummary> = BTreeMap::new();
        for class in RetentionClass::ALL {
            summaries.insert(
                class.rank(),
                RetentionClassSummary {
                    class: class.as_str(),
                    ..RetentionClassSummary::default()
                },
            );
        }
        summaries.insert(
            0,
            RetentionClassSummary {
                class: "unbound",
                ..RetentionClassSummary::default()
            },
        );

        for owner in &owners {
            let summary = summaries
                .get_mut(&owner.class.rank())
                .expect("retention class summary exists");
            let reachable_blobs = owner
                .artifact_blobs
                .iter()
                .filter(|blob| reach.contains(blob))
                .count() as u64;
            if reachable_blobs > 0 {
                summary.reachable_artifacts += 1;
                summary.reachable_blobs += reachable_blobs;
                summary.reachable_bytes += owner.artifact_bytes;
                if !owner.live {
                    plan.retained.push(RetainedArtifact {
                        descriptor_root: owner.descriptor_root,
                        class: owner.class.as_str(),
                        binding: owner.binding,
                        artifact: owner.artifact,
                        expired: true,
                        bytes: owner.artifact_bytes,
                        reason: reach
                            .origin_of(&owner.artifact)
                            .or_else(|| {
                                owner
                                    .artifact_blobs
                                    .iter()
                                    .find_map(|blob| reach.origin_of(blob))
                            })
                            .map(|origin| format!("reachable from {origin}"))
                            .unwrap_or_else(|| "reachable from head".to_owned()),
                    });
                }
            } else if owner.live {
                summary.retained_live_artifacts += 1;
                summary.retained_live_blobs += owner.artifact_blobs.len() as u64;
                summary.retained_live_bytes += owner.artifact_bytes;
                plan.retained.push(RetainedArtifact {
                    descriptor_root: owner.descriptor_root,
                    class: owner.class.as_str(),
                    binding: owner.binding,
                    artifact: owner.artifact,
                    expired: false,
                    bytes: owner.artifact_bytes,
                    reason: match owner.expiry_ns {
                        Some(expiry) => format!("retention live until {expiry}"),
                        None => "retention live".to_owned(),
                    },
                });
            }
        }

        // Candidate blobs: exactly the set ordinary GC would sweep, so the
        // reachability precondition cannot drift from `Store::gc`.
        let mut collector = GcReport::default();
        let now = std::time::SystemTime::now();
        let grace = self.options().gc_grace_secs;
        let mut raw: Vec<(Key, u64)> = Vec::new();
        raw.extend(collect_unmarked(
            self,
            KeyKind::Manifest,
            &reach.manifests,
            now,
            grace,
            &mut collector,
        )?);
        raw.extend(collect_unmarked(
            self,
            KeyKind::Object,
            &reach.chunks,
            now,
            grace,
            &mut collector,
        )?);
        plan.problems.extend(collector.problems);

        struct Candidate {
            key: Key,
            hash: Hash32,
            bytes: u64,
            rank: u8,
            class: &'static str,
            expiry_ns: Option<i64>,
            descriptor_roots: Vec<Hash32>,
        }
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut retained_live_bytes = 0u64;
        for (key, bytes) in raw {
            let hash = match &key {
                Key::Manifest { hash } | Key::Object { hash } => *hash,
                _ => continue,
            };
            let owners_here = owners_by_blob.get(&hash);
            match owners_here {
                None => candidates.push(Candidate {
                    key,
                    hash,
                    bytes,
                    rank: 0,
                    class: "unbound",
                    expiry_ns: None,
                    descriptor_roots: Vec::new(),
                }),
                Some(indices) => {
                    let live = indices.iter().any(|index| owners[*index].live);
                    let mut roots: Vec<Hash32> = indices
                        .iter()
                        .map(|index| owners[*index].descriptor_root)
                        .collect();
                    roots.sort_unstable();
                    roots.dedup();
                    if live {
                        retained_live_bytes = retained_live_bytes.saturating_add(bytes);
                        continue;
                    }
                    let rank = indices
                        .iter()
                        .map(|index| owners[*index].class.rank())
                        .max()
                        .unwrap_or(0);
                    let class = indices
                        .iter()
                        .max_by_key(|index| owners[**index].class.rank())
                        .map(|index| owners[*index].class.as_str())
                        .unwrap_or("unbound");
                    let expiry_ns = indices
                        .iter()
                        .filter_map(|index| owners[*index].expiry_ns)
                        .max();
                    candidates.push(Candidate {
                        key,
                        hash,
                        bytes,
                        rank,
                        class,
                        expiry_ns,
                        descriptor_roots: roots,
                    });
                }
            }
        }

        candidates.sort_by(|a, b| {
            a.rank
                .cmp(&b.rank)
                .then_with(|| {
                    a.expiry_ns
                        .unwrap_or(i64::MIN)
                        .cmp(&b.expiry_ns.unwrap_or(i64::MIN))
                })
                .then_with(|| a.descriptor_roots.cmp(&b.descriptor_roots))
                .then_with(|| a.key.cmp(&b.key))
        });

        let mut selected: Vec<Key> = Vec::new();
        let mut selected_bytes = 0u64;
        for candidate in candidates {
            let fits = match budget_bytes {
                None => true,
                Some(budget) => {
                    selected_bytes == 0 || selected_bytes.saturating_add(candidate.bytes) <= budget
                }
            };
            if !fits {
                break;
            }
            if let Some(summary) = summaries.get_mut(&candidate.rank) {
                summary.reclaimable_blobs += 1;
                summary.reclaimable_bytes += candidate.bytes;
            }
            selected_bytes = selected_bytes.saturating_add(candidate.bytes);
            plan.candidates.push(RetentionCandidate {
                path: candidate.key.display_path(),
                bytes: candidate.bytes,
                class: candidate.class,
                descriptor_roots: candidate.descriptor_roots,
                expiry_ns: candidate.expiry_ns,
            });
            selected.push(candidate.key);
            let _ = candidate.hash;
        }
        plan.planned_bytes = selected_bytes;
        plan.planned_blobs = selected.len() as u64;
        plan.over_budget_bytes = match budget_bytes {
            Some(budget) => retained_live_bytes.saturating_sub(budget),
            None => 0,
        };
        plan.classes = summaries.into_values().collect();
        plan.retained.sort_by(|a, b| {
            a.class
                .cmp(b.class)
                .then_with(|| a.descriptor_root.cmp(&b.descriptor_root))
        });
        Ok((plan, selected))
    }

    fn blob_len(&self, hash: Hash32) -> Result<u64> {
        for key in [Key::Object { hash }, Key::Manifest { hash }] {
            match self.with_backend(|backend| backend.exists(&key)) {
                Ok(true) => return self.with_backend(|backend| backend.len(&key)),
                Ok(false) => {}
                Err(err) => return Err(err),
            }
        }
        Ok(0)
    }
}

/// A request to publish one binding. The record is built from these fields and
/// the current pointer (for `supersedes`); callers never supply `supersedes`.
#[derive(Clone, Debug)]
pub struct RetentionRequest {
    pub descriptor_root: Hash32,
    pub class: RetentionClass,
    pub declared_lifetime_ms: u64,
    pub producer: String,
    pub policy_epoch: u64,
    pub issued_at_ns: Option<i64>,
    pub object: Option<Hash32>,
    pub sign_seed: Option<[u8; 32]>,
    pub trust_keys: Option<Vec<[u8; 32]>>,
}

impl RetentionRequest {
    pub fn new(
        descriptor_root: Hash32,
        class: RetentionClass,
        declared_lifetime_ms: u64,
        producer: impl Into<String>,
    ) -> Self {
        RetentionRequest {
            descriptor_root,
            class,
            declared_lifetime_ms,
            producer: producer.into(),
            policy_epoch: 0,
            issued_at_ns: None,
            object: None,
            sign_seed: None,
            trust_keys: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> RetentionBinding {
        RetentionBinding::new(
            Hash32::from_bytes([7u8; 32]),
            RetentionClass::Volatile,
            180_000,
            "run-1",
            3,
            1_700_000_000_000_000_000,
            None,
        )
        .unwrap()
    }

    #[test]
    fn record_round_trips_and_hashes_canonically() {
        let record = binding();
        let bytes = record.encode().unwrap();
        let decoded = RetentionBinding::decode(&bytes).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert_eq!(record.id().unwrap(), identity::h(RETENTION_DOMAIN, &bytes));
    }

    #[test]
    fn durable_requires_zero_lifetime_and_others_reject_zero() {
        assert!(RetentionBinding::new(
            Hash32::from_bytes([1u8; 32]),
            RetentionClass::Durable,
            1,
            "run",
            0,
            0,
            None
        )
        .is_err());
        assert!(RetentionBinding::new(
            Hash32::from_bytes([1u8; 32]),
            RetentionClass::Ephemeral,
            0,
            "run",
            0,
            0,
            None
        )
        .is_err());
        assert!(RetentionBinding::new(
            Hash32::from_bytes([1u8; 32]),
            RetentionClass::Durable,
            0,
            "run",
            0,
            0,
            None
        )
        .is_ok());
    }

    #[test]
    fn pointer_round_trips_and_rejects_bad_shapes() {
        let pointer = RetentionPointer {
            descriptor_root: Hash32::from_bytes([1u8; 32]),
            binding: Hash32::from_bytes([2u8; 32]),
            artifact: Hash32::from_bytes([3u8; 32]),
        };
        let bytes = pointer.encode();
        assert_eq!(bytes.len(), POINTER_BYTES);
        assert_eq!(RetentionPointer::decode(&bytes).unwrap(), pointer);
        assert!(RetentionPointer::decode(&bytes[..100]).is_err());
        let mut bad = bytes;
        bad[0] = b'X';
        assert!(RetentionPointer::decode(&bad).is_err());
    }

    #[test]
    fn signature_covers_the_record_without_its_signature_key() {
        let mut record = binding();
        let unsigned = record.signing_bytes().unwrap();
        record.sign(&[9u8; 32]).unwrap();
        assert_eq!(record.signing_bytes().unwrap(), unsigned);
        record.verify_signature().unwrap();
        let mut tampered = record.clone();
        tampered.producer = "run-2".to_owned();
        assert!(tampered.verify_signature().is_err());
    }

    #[test]
    fn identity_is_not_part_of_the_artifact() {
        // Two bindings for the same descriptor differ only in retention and
        // produce different binding ids while the descriptor root is untouched.
        let a = binding();
        let mut b = a.clone();
        b.class = RetentionClass::Ephemeral;
        b.declared_lifetime_ms = 21_600_000;
        assert_ne!(a.id().unwrap(), b.id().unwrap());
        assert_eq!(a.descriptor_root, b.descriptor_root);
    }
}
