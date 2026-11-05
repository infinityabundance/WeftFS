//! In-memory state, canonical records, and commit-op application.
//!
//! Every type here has a canonical WCB map form matching `WEFT-MODEL.md` §2.2
//! (ops), §2.4 (checkpoint state), and §2.3.1 (derived identities). There is no
//! `serde` on the on-disk path: encoding is `weftcodec::wcb`.

use std::collections::{BTreeMap, BTreeSet};

use weftcodec::context::{CausalClosure, IdentityMode};
use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::wcb_util as wu;

/// Namespace name used by v1.
pub const DEFAULT_NAMESPACE: &str = "default";

/// Attribute key holding a symlink target as WCB bytes.
pub const ATTR_SYMLINK_TARGET: &str = "weft.symlink.target";
/// Attribute key holding a read ACL as an array of u64 uid values.
pub const ATTR_ACL_READ: &str = "weft.acl.read";
/// Reserved predicate naming a saved selector context (`WEFT-MODEL.md` §2.5).
pub const SAVED_CONTEXT_PREDICATE: &str = "saved_context";
/// Attribute key on a `saved_context` relation holding lowercase hex of the
/// canonical selector bytes.
pub const SAVED_CONTEXT_SELECTOR_ATTR: &str = "selector_hex";
/// Prefix of object attributes that hold extended attributes.
pub const XATTR_PREFIX: &str = "xattr.";

/// Reserved causal attributes (`WEFT-CAUSAL.md` §3.1).
pub const ATTR_CAUSAL_PARENTS: &str = "weft.causal.parents";
pub const ATTR_CAUSAL_CLOSURE: &str = "weft.causal.closure";
pub const ATTR_CAUSAL_HASH: &str = "weft.causal.hash";
pub const ATTR_IDENTITY_MODE: &str = "weft.identity.mode";

/// Reserved predicate naming a stored Context Root relation (F2).
pub const CONTEXT_ROOT_PREDICATE: &str = "context_root";
/// Attribute holding lowercase hex of the canonical context-root record.
pub const CONTEXT_ROOT_RECORD_ATTR: &str = "context.record";
/// Attribute holding the authoritative `weft://<namespace>/c/<id>` URI.
pub const CONTEXT_ROOT_URI_ATTR: &str = "uri";

/// Reserved predicate naming a stored capability record (F2).
pub const CAPABILITY_PREDICATE: &str = "capability";
/// Attribute holding lowercase hex of the canonical capability record.
pub const CAPABILITY_RECORD_ATTR: &str = "capability.record";

/// Reserved predicate attaching an authenticated envelope to its grantee
/// (D2). The subject is the grantee identity; the value target is the
/// lowercase-hex `EnvelopeId`.
pub const CAPABILITY_ENVELOPE_PREDICATE: &str = "capability_envelope";
/// Attribute holding lowercase hex of the stored `{envelope, signature}` blob
/// id in the manifest namespace.
pub const CAPABILITY_ENVELOPE_BLOB_ATTR: &str = "capability.envelope_blob";
/// Attribute holding the envelope tenant label (for discovery only; the
/// authoritative value is inside the signed envelope).
pub const CAPABILITY_ENVELOPE_TENANT_ATTR: &str = "capability.envelope_tenant";

/// Object kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKind {
    File,
    Dir,
    Symlink,
}

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::File => "file",
            ObjectKind::Dir => "dir",
            ObjectKind::Symlink => "symlink",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "file" => Ok(ObjectKind::File),
            "dir" => Ok(ObjectKind::Dir),
            "symlink" => Ok(ObjectKind::Symlink),
            other => Err(Error::malformed(format!("unknown object kind `{other}`"))),
        }
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Str(text) => Self::parse(text),
            _ => Err(Error::malformed("object kind must be text")),
        }
    }
}

/// An immutable object record (`WEFT-MODEL.md` §2.4 `object_record`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectRecord {
    pub id: Hash32,
    pub kind: ObjectKind,
    pub mode: u64,
    pub uid: u64,
    pub gid: u64,
    pub mtime_ns: i64,
    pub version: Hash32,
    /// Content root. For files this is the **manifest** hash
    /// (`ManifestId`), not the whole-file `H("weft/chunk/v1", bytes)` digest;
    /// that digest lives in the manifest's `content_hash` field so readers can
    /// locate the manifest from this record. For directories this is the empty
    /// content digest; for symlinks it is the target-bytes digest. Documented in
    /// `docs/store-status.md`.
    pub content: Hash32,
    pub size: u64,
    pub attrs: BTreeMap<String, Value>,
}

impl ObjectRecord {
    pub const KEYS: &'static [&'static str] = &[
        "id", "kind", "mode", "uid", "gid", "mtime_ns", "version", "content", "size", "attrs",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("id", wu::hash_value(self.id)),
            ("kind", Value::Str(self.kind.as_str().to_owned())),
            ("mode", Value::U64(self.mode)),
            ("uid", Value::U64(self.uid)),
            ("gid", Value::U64(self.gid)),
            ("mtime_ns", Value::Timestamp(self.mtime_ns)),
            ("version", wu::hash_value(self.version)),
            ("content", wu::hash_value(self.content)),
            ("size", Value::U64(self.size)),
            ("attrs", Value::Map(self.attrs.clone())),
        ])
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        Self::from_map(wu::as_map(value)?)
    }

    pub fn from_map(map: &BTreeMap<String, Value>) -> Result<Self> {
        wu::check_keys(map, Self::KEYS)?;
        Ok(ObjectRecord {
            id: wu::get_hash(map, "id")?,
            kind: ObjectKind::from_value(wu::field(map, "kind")?)?,
            mode: wu::get_u64(map, "mode")?,
            uid: wu::get_u64(map, "uid")?,
            gid: wu::get_u64(map, "gid")?,
            mtime_ns: wu::get_timestamp(map, "mtime_ns")?,
            version: wu::get_hash(map, "version")?,
            content: wu::get_hash(map, "content")?,
            size: wu::get_u64(map, "size")?,
            attrs: wu::get_map(map, "attrs")?,
        })
    }

    /// The symlink target, if this is a symlink with a well-formed attribute.
    pub fn symlink_target(&self) -> Option<&[u8]> {
        match self.attrs.get(ATTR_SYMLINK_TARGET) {
            Some(Value::Bytes(bytes)) => Some(bytes),
            _ => None,
        }
    }
}

/// A directory entry (`WEFT-MODEL.md` §2.4 `entry_record`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryRecord {
    pub id: Hash32,
    pub parent: Hash32,
    pub name: Vec<u8>,
    pub target: Hash32,
}

/// `EntryId = H("weft/entry/v1", WCB({"parent":bytes32,"name":bytes}))`.
pub fn derive_entry_id(parent: Hash32, name: &[u8]) -> Result<Hash32> {
    let value = wu::map([
        ("parent", wu::hash_value(parent)),
        ("name", Value::Bytes(name.to_vec())),
    ]);
    let bytes = wu::encode(&value)?;
    Ok(identity::h(identity::ENTRY_DOMAIN, &bytes))
}

impl EntryRecord {
    pub const KEYS: &'static [&'static str] = &["id", "parent", "name", "target"];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("id", wu::hash_value(self.id)),
            ("parent", wu::hash_value(self.parent)),
            ("name", Value::Bytes(self.name.clone())),
            ("target", wu::hash_value(self.target)),
        ])
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        Self::from_map(wu::as_map(value)?)
    }

    pub fn from_map(map: &BTreeMap<String, Value>) -> Result<Self> {
        wu::check_keys(map, Self::KEYS)?;
        let parent = wu::get_hash(map, "parent")?;
        let name = wu::get_bytes(map, "name")?;
        let expected = derive_entry_id(parent, &name)?;
        let id = wu::get_hash(map, "id")?;
        if id != expected {
            return Err(Error::corrupt("entry id does not match (parent,name)"));
        }
        Ok(EntryRecord {
            id,
            parent,
            name,
            target: wu::get_hash(map, "target")?,
        })
    }
}

/// Relation target kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RelationKind {
    Object,
    Value,
}

impl RelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RelationKind::Object => "object",
            RelationKind::Value => "value",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "object" => Ok(RelationKind::Object),
            "value" => Ok(RelationKind::Value),
            other => Err(Error::malformed(format!("unknown relation kind `{other}`"))),
        }
    }
}

/// A relation target: another object, or a scalar WCB value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelationTarget {
    Object(Hash32),
    Value(Value),
}

/// A relation assertion (`WEFT-MODEL.md` §2.2 `put_relation`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationRecord {
    pub id: Hash32,
    pub subject: Hash32,
    pub predicate: String,
    pub kind: RelationKind,
    pub target: RelationTarget,
    pub provenance: Option<BTreeMap<String, Value>>,
    pub attrs: BTreeMap<String, Value>,
}

/// `RelationId = H("weft/relation/v1", WCB(assertion without id))`.
pub fn derive_relation_id(
    subject: Hash32,
    predicate: &str,
    kind: RelationKind,
    target: &RelationTarget,
    provenance: &Option<BTreeMap<String, Value>>,
    attrs: &BTreeMap<String, Value>,
) -> Result<Hash32> {
    let object_value = match target {
        RelationTarget::Object(hash) => wu::hash_value(*hash),
        RelationTarget::Value(value) => {
            wu::validate_scalar(value)?;
            value.clone()
        }
    };
    let provenance_value = match provenance {
        Some(map) => Value::Map(map.clone()),
        None => Value::Null,
    };
    let value = wu::map([
        ("subject", wu::hash_value(subject)),
        ("predicate", Value::Str(predicate.to_owned())),
        ("kind", Value::Str(kind.as_str().to_owned())),
        ("object", object_value),
        ("provenance", provenance_value),
        ("attrs", Value::Map(attrs.clone())),
    ]);
    let bytes = wu::encode(&value)?;
    Ok(identity::h(identity::RELATION_DOMAIN, &bytes))
}

impl RelationRecord {
    pub const KEYS: &'static [&'static str] = &[
        "id",
        "subject",
        "predicate",
        "kind",
        "object",
        "provenance",
        "attrs",
    ];

    pub fn new(
        subject: Hash32,
        predicate: impl Into<String>,
        target: RelationTarget,
        provenance: Option<BTreeMap<String, Value>>,
        attrs: BTreeMap<String, Value>,
    ) -> Result<Self> {
        let predicate = predicate.into();
        let kind = match target {
            RelationTarget::Object(_) => RelationKind::Object,
            RelationTarget::Value(_) => RelationKind::Value,
        };
        let id = derive_relation_id(subject, &predicate, kind, &target, &provenance, &attrs)?;
        Ok(RelationRecord {
            id,
            subject,
            predicate,
            kind,
            target,
            provenance,
            attrs,
        })
    }

    pub fn to_value(&self) -> Value {
        let object_value = match &self.target {
            RelationTarget::Object(hash) => wu::hash_value(*hash),
            RelationTarget::Value(value) => value.clone(),
        };
        let provenance_value = match &self.provenance {
            Some(map) => Value::Map(map.clone()),
            None => Value::Null,
        };
        wu::map([
            ("id", wu::hash_value(self.id)),
            ("subject", wu::hash_value(self.subject)),
            ("predicate", Value::Str(self.predicate.clone())),
            ("kind", Value::Str(self.kind.as_str().to_owned())),
            ("object", object_value),
            ("provenance", provenance_value),
            ("attrs", Value::Map(self.attrs.clone())),
        ])
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        Self::from_map(wu::as_map(value)?)
    }

    pub fn from_map(map: &BTreeMap<String, Value>) -> Result<Self> {
        wu::check_keys(map, Self::KEYS)?;
        let subject = wu::get_hash(map, "subject")?;
        let predicate = wu::get_text(map, "predicate")?;
        let kind = RelationKind::parse(&wu::get_text(map, "kind")?)?;
        let target = match kind {
            RelationKind::Object => RelationTarget::Object(wu::get_hash(map, "object")?),
            RelationKind::Value => {
                let value = wu::field(map, "object")?.clone();
                wu::validate_scalar(&value)?;
                RelationTarget::Value(value)
            }
        };
        let provenance = wu::get_opt_map(map, "provenance")?;
        let attrs = wu::get_map(map, "attrs")?;
        let expected = derive_relation_id(subject, &predicate, kind, &target, &provenance, &attrs)?;
        let id = wu::get_hash(map, "id")?;
        if id != expected {
            return Err(Error::corrupt("relation id does not match its assertion"));
        }
        Ok(RelationRecord {
            id,
            subject,
            predicate,
            kind,
            target,
            provenance,
            attrs,
        })
    }
}

/// `VersionId` derivation. The full version record is not stored separately;
/// the record is reconstructed at write time and only its identity is recorded.
///
/// This is the frozen `bytes_only` v1 form (`WEFT-MODEL.md` §2.3.1).
pub fn derive_version_id(
    object: Hash32,
    content: Hash32,
    size: u64,
    mtime_ns: i64,
    parent: Option<Hash32>,
) -> Result<Hash32> {
    derive_version_id_with_identity(
        object,
        content,
        size,
        mtime_ns,
        parent,
        IdentityMode::BytesOnly,
        None,
    )
}

/// `VersionId` derivation with an explicit identity mode (`WEFT-CAUSAL.md` §2).
///
/// `bytes_only` emits exactly the frozen v1 record. `causal_exact` adds
/// `identity_mode` and `causal_hash`; omitting the hash is `NonCanonical`.
pub fn derive_version_id_with_identity(
    object: Hash32,
    content: Hash32,
    size: u64,
    mtime_ns: i64,
    parent: Option<Hash32>,
    mode: IdentityMode,
    causal_hash: Option<Hash32>,
) -> Result<Hash32> {
    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    fields.insert(
        "schema".to_owned(),
        Value::Str("weftfs.version.v1".to_owned()),
    );
    fields.insert("object".to_owned(), wu::hash_value(object));
    fields.insert("content".to_owned(), wu::hash_value(content));
    fields.insert("size".to_owned(), Value::U64(size));
    fields.insert("mtime_ns".to_owned(), Value::Timestamp(mtime_ns));
    fields.insert("parent".to_owned(), wu::opt_hash_value(parent));
    match mode {
        IdentityMode::BytesOnly => {
            if causal_hash.is_some() {
                return Err(Error::non_canonical(
                    "bytes_only version record must not carry a causal hash",
                ));
            }
        }
        IdentityMode::CausalExact => {
            let hash = causal_hash.ok_or_else(|| {
                Error::non_canonical("causal_exact version record requires a causal hash")
            })?;
            if hash == Hash32::ZERO {
                return Err(Error::non_canonical("causal hash must not be all zero"));
            }
            fields.insert(
                "identity_mode".to_owned(),
                Value::Str(IdentityMode::CausalExact.as_str().to_owned()),
            );
            fields.insert("causal_hash".to_owned(), wu::hash_value(hash));
        }
    }
    let bytes = wu::encode(&Value::Map(fields))?;
    Ok(identity::version_id(&bytes))
}

/// Parse a reserved causal attribute array into sorted, validated hashes.
pub fn causal_hashes_from_attr(value: &Value, what: &str) -> Result<Vec<Hash32>> {
    let items = match value {
        Value::Array(items) => items,
        _ => {
            return Err(Error::malformed(format!(
                "{what} must be an array of bytes32"
            )))
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let hash = match item {
            Value::Bytes(bytes) => Hash32::from_slice(bytes)?,
            _ => return Err(Error::malformed(format!("{what} must contain bytes32"))),
        };
        if hash == Hash32::ZERO {
            return Err(Error::malformed(format!(
                "{what} must not contain the zero hash"
            )));
        }
        out.push(hash);
    }
    Ok(out)
}

/// Extract and validate the causal closure stored on an object record.
///
/// Returns `None` when no causal attributes are present. A partial or
/// inconsistent set is `Corrupt` (never silently repaired).
pub fn object_causal_closure(record: &ObjectRecord) -> Result<Option<CausalClosure>> {
    let parents_value = record.attrs.get(ATTR_CAUSAL_PARENTS);
    let closure_value = record.attrs.get(ATTR_CAUSAL_CLOSURE);
    let hash_value = record.attrs.get(ATTR_CAUSAL_HASH);
    let mode_value = record.attrs.get(ATTR_IDENTITY_MODE);

    if parents_value.is_none()
        && closure_value.is_none()
        && hash_value.is_none()
        && mode_value.is_none()
    {
        return Ok(None);
    }
    let (Some(parents_value), Some(closure_value), Some(hash_value)) =
        (parents_value, closure_value, hash_value)
    else {
        return Err(Error::corrupt(
            "partial causal attributes: parents, closure, and hash are all required",
        ));
    };
    let parents = causal_hashes_from_attr(parents_value, "weft.causal.parents")?;
    let closure = causal_hashes_from_attr(closure_value, "weft.causal.closure")?;
    let record_hash = match hash_value {
        Value::Bytes(bytes) => Hash32::from_slice(bytes)?,
        _ => return Err(Error::corrupt("weft.causal.hash must be bytes32")),
    };
    if record_hash == Hash32::ZERO {
        return Err(Error::corrupt("weft.causal.hash must not be all zero"));
    }
    let closure_record = CausalClosure::new(parents, closure)?;
    let expected = closure_record.id()?;
    if expected != record_hash {
        return Err(Error::corrupt(
            "weft.causal.hash does not match the recorded closure",
        ));
    }
    match mode_value {
        None => {
            // An unlabelled record is `bytes_only`; the causal attributes are
            // retained metadata and do not participate in VersionId.
        }
        Some(Value::Str(text)) => {
            let mode = IdentityMode::parse(text)?;
            if mode != IdentityMode::CausalExact {
                return Err(Error::non_canonical(
                    "bytes_only must be the absence of weft.identity.mode",
                ));
            }
        }
        Some(_) => return Err(Error::corrupt("weft.identity.mode must be text")),
    }
    Ok(Some(closure_record))
}

/// The identity mode and causal hash implied by an object's reserved attrs.
pub fn identity_from_attrs(record: &ObjectRecord) -> Result<(IdentityMode, Option<Hash32>)> {
    match object_causal_closure(record)? {
        Some(closure) => Ok((IdentityMode::CausalExact, Some(closure.id()?))),
        None => Ok((IdentityMode::BytesOnly, None)),
    }
}

/// Attach canonical causal attributes to an object attribute map.
pub fn apply_causal_attrs(
    attrs: &mut BTreeMap<String, Value>,
    closure: &CausalClosure,
    mode: IdentityMode,
    causal_hash: Hash32,
) -> Result<()> {
    attrs.insert(
        ATTR_CAUSAL_PARENTS.to_owned(),
        Value::Array(
            closure
                .parents
                .iter()
                .map(|hash| Value::Bytes(hash.as_bytes().to_vec()))
                .collect(),
        ),
    );
    attrs.insert(
        ATTR_CAUSAL_CLOSURE.to_owned(),
        Value::Array(
            closure
                .closure
                .iter()
                .map(|hash| Value::Bytes(hash.as_bytes().to_vec()))
                .collect(),
        ),
    );
    attrs.insert(
        ATTR_CAUSAL_HASH.to_owned(),
        Value::Bytes(causal_hash.as_bytes().to_vec()),
    );
    match mode {
        IdentityMode::CausalExact => {
            attrs.insert(
                ATTR_IDENTITY_MODE.to_owned(),
                Value::Str(IdentityMode::CausalExact.as_str().to_owned()),
            );
        }
        IdentityMode::BytesOnly => {
            attrs.remove(ATTR_IDENTITY_MODE);
        }
    }
    Ok(())
}

/// A commit op (`WEFT-MODEL.md` §2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    PutObject(ObjectRecord),
    DeleteObject(Hash32),
    PutEntry(EntryRecord),
    DelEntry(Hash32),
    PutRelation(RelationRecord),
    DelRelation(Hash32),
    SetSnapshot { name: String, commit: Hash32 },
    SetTip { object: Hash32, version: Hash32 },
}

impl Op {
    pub fn to_value(&self) -> Value {
        match self {
            Op::PutObject(record) => {
                let mut map = match record.to_value() {
                    Value::Map(map) => map,
                    _ => unreachable!("object record is a map"),
                };
                map.insert("op".to_owned(), Value::Str("put_object".to_owned()));
                Value::Map(map)
            }
            Op::DeleteObject(id) => wu::map([
                ("op", Value::Str("delete_object".to_owned())),
                ("id", wu::hash_value(*id)),
            ]),
            Op::PutEntry(record) => {
                let mut map = match record.to_value() {
                    Value::Map(map) => map,
                    _ => unreachable!("entry record is a map"),
                };
                map.insert("op".to_owned(), Value::Str("put_entry".to_owned()));
                Value::Map(map)
            }
            Op::DelEntry(id) => wu::map([
                ("op", Value::Str("del_entry".to_owned())),
                ("id", wu::hash_value(*id)),
            ]),
            Op::PutRelation(record) => {
                let mut map = match record.to_value() {
                    Value::Map(map) => map,
                    _ => unreachable!("relation record is a map"),
                };
                map.insert("op".to_owned(), Value::Str("put_relation".to_owned()));
                Value::Map(map)
            }
            Op::DelRelation(id) => wu::map([
                ("op", Value::Str("del_relation".to_owned())),
                ("id", wu::hash_value(*id)),
            ]),
            Op::SetSnapshot { name, commit } => wu::map([
                ("op", Value::Str("set_snapshot".to_owned())),
                ("name", Value::Str(name.clone())),
                ("commit", wu::hash_value(*commit)),
            ]),
            Op::SetTip { object, version } => wu::map([
                ("op", Value::Str("set_tip".to_owned())),
                ("object", wu::hash_value(*object)),
                ("version", wu::hash_value(*version)),
            ]),
        }
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        let name = wu::get_text(map, "op")?;
        // Records are parsed from a map that also contains "op"; strip it by
        // cloning the remaining fields and validating the exact key set.
        let without_op: BTreeMap<String, Value> = map
            .iter()
            .filter(|(key, _)| key.as_str() != "op")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        match name.as_str() {
            "put_object" => Ok(Op::PutObject(ObjectRecord::from_map(&without_op)?)),
            "delete_object" => {
                wu::check_keys(&without_op, &["id"])?;
                Ok(Op::DeleteObject(wu::get_hash(&without_op, "id")?))
            }
            "put_entry" => Ok(Op::PutEntry(EntryRecord::from_map(&without_op)?)),
            "del_entry" => {
                wu::check_keys(&without_op, &["id"])?;
                Ok(Op::DelEntry(wu::get_hash(&without_op, "id")?))
            }
            "put_relation" => Ok(Op::PutRelation(RelationRecord::from_map(&without_op)?)),
            "del_relation" => {
                wu::check_keys(&without_op, &["id"])?;
                Ok(Op::DelRelation(wu::get_hash(&without_op, "id")?))
            }
            "set_snapshot" => {
                wu::check_keys(&without_op, &["name", "commit"])?;
                Ok(Op::SetSnapshot {
                    name: wu::get_text(&without_op, "name")?,
                    commit: wu::get_hash(&without_op, "commit")?,
                })
            }
            "set_tip" => {
                wu::check_keys(&without_op, &["object", "version"])?;
                Ok(Op::SetTip {
                    object: wu::get_hash(&without_op, "object")?,
                    version: wu::get_hash(&without_op, "version")?,
                })
            }
            other => Err(Error::malformed(format!("unknown commit op `{other}`"))),
        }
    }
}

/// The complete replayed state.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// The generation whose ops have been applied.
    pub generation: u64,
    pub objects: BTreeMap<Hash32, ObjectRecord>,
    pub entries: BTreeMap<Hash32, EntryRecord>,
    pub relations: BTreeMap<Hash32, RelationRecord>,
    pub snapshots: BTreeMap<String, Hash32>,
    pub tips: BTreeMap<Hash32, Hash32>,
}

impl State {
    /// The root object id: 32 zero bytes.
    pub const ROOT_ID: Hash32 = Hash32::ZERO;

    /// Apply one op, validating it as a whole.
    pub fn apply_op(&mut self, op: &Op) -> Result<()> {
        match op {
            Op::PutObject(record) => {
                validate_object_record(record)?;
                self.objects.insert(record.id, record.clone());
            }
            Op::DeleteObject(id) => {
                if *id == Self::ROOT_ID {
                    return Err(Error::malformed("cannot delete the namespace root"));
                }
                if !self.objects.contains_key(id) {
                    return Err(Error::malformed("delete_object for a missing object"));
                }
                if self.entries.values().any(|entry| entry.target == *id) {
                    return Err(Error::malformed(
                        "delete_object while a directory entry still targets it",
                    ));
                }
                if self.entries.values().any(|entry| entry.parent == *id) {
                    return Err(Error::malformed(
                        "delete_object while it is still an entry parent",
                    ));
                }
                self.objects.remove(id);
                // A tip for a deleted object cannot be meaningful; drop it.
                self.tips.remove(id);
            }
            Op::PutEntry(record) => {
                validate_name_bytes(&record.name)?;
                let expected = derive_entry_id(record.parent, &record.name)?;
                if expected != record.id {
                    return Err(Error::corrupt("put_entry id does not match (parent,name)"));
                }
                let parent = self
                    .objects
                    .get(&record.parent)
                    .ok_or_else(|| Error::malformed("put_entry parent does not exist"))?;
                if parent.kind != ObjectKind::Dir {
                    return Err(Error::malformed("put_entry parent is not a directory"));
                }
                if !self.objects.contains_key(&record.target) {
                    return Err(Error::malformed("put_entry target does not exist"));
                }
                match self.entries.get(&record.id) {
                    Some(existing) if existing == record => {}
                    Some(_) => {
                        return Err(Error::malformed(
                            "put_entry would duplicate an existing (parent,name)",
                        ))
                    }
                    None => {
                        self.entries.insert(record.id, record.clone());
                    }
                }
            }
            Op::DelEntry(id) => {
                if self.entries.remove(id).is_none() {
                    return Err(Error::malformed("del_entry for a missing entry"));
                }
            }
            Op::PutRelation(record) => {
                let expected = derive_relation_id(
                    record.subject,
                    &record.predicate,
                    record.kind,
                    &record.target,
                    &record.provenance,
                    &record.attrs,
                )?;
                if expected != record.id {
                    return Err(Error::corrupt("put_relation id does not match assertion"));
                }
                match self.relations.get(&record.id) {
                    Some(existing) if existing == record => {}
                    Some(_) => {
                        return Err(Error::malformed(
                            "put_relation would replace a different assertion with the same id",
                        ))
                    }
                    None => {
                        self.relations.insert(record.id, record.clone());
                    }
                }
            }
            Op::DelRelation(id) => {
                if self.relations.remove(id).is_none() {
                    return Err(Error::malformed("del_relation for a missing relation"));
                }
            }
            Op::SetSnapshot { name, commit } => {
                validate_snapshot_name(name)?;
                if *commit == Hash32::ZERO {
                    return Err(Error::malformed("snapshot points at the zero hash"));
                }
                self.snapshots.insert(name.clone(), *commit);
            }
            Op::SetTip { object, version } => {
                if !self.objects.contains_key(object) {
                    return Err(Error::malformed("set_tip for a missing object"));
                }
                self.tips.insert(*object, *version);
            }
        }
        Ok(())
    }

    /// Apply a list of ops atomically: all or nothing.
    pub fn apply_ops(&mut self, ops: &[Op]) -> Result<()> {
        let mut next = self.clone();
        for op in ops {
            next.apply_op(op)?;
        }
        *self = next;
        Ok(())
    }

    /// Look up a child by exact byte name.
    pub fn lookup(&self, parent: Hash32, name: &[u8]) -> Result<Hash32> {
        let id = derive_entry_id(parent, name)?;
        match self.entries.get(&id) {
            Some(entry) => Ok(entry.target),
            None => Err(Error::not_found("no such directory entry")),
        }
    }

    /// Directory entries sorted by unsigned byte-wise name.
    pub fn list_dir(&self, parent: Hash32) -> Result<Vec<EntryRecord>> {
        let object = self
            .objects
            .get(&parent)
            .ok_or_else(|| Error::not_found("no such parent object"))?;
        if object.kind != ObjectKind::Dir {
            return Err(Error::not_dir("object is not a directory"));
        }
        let mut out: Vec<EntryRecord> = self
            .entries
            .values()
            .filter(|entry| entry.parent == parent)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// All entries pointing at `target`, sorted by (parent,name).
    pub fn entries_for(&self, target: Hash32) -> Vec<&EntryRecord> {
        let mut out: Vec<&EntryRecord> = self
            .entries
            .values()
            .filter(|entry| entry.target == target)
            .collect();
        out.sort_by(|a, b| a.parent.cmp(&b.parent).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// The lexicographically first path for an object, if it has one.
    pub fn path_for(&self, target: Hash32) -> Option<Vec<u8>> {
        if target == Self::ROOT_ID {
            return Some(b"/".to_vec());
        }
        let entry = self.entries_for(target).into_iter().next()?.clone();
        let mut components: Vec<Vec<u8>> = vec![entry.name.clone()];
        let mut current = entry.parent;
        let mut visited = BTreeSet::new();
        while current != Self::ROOT_ID {
            if !visited.insert(current) {
                return None;
            }
            let parent_entry = self.entries_for(current).into_iter().next()?.clone();
            components.push(parent_entry.name.clone());
            current = parent_entry.parent;
        }
        components.reverse();
        let mut path = Vec::new();
        for component in components {
            path.push(b'/');
            path.extend_from_slice(&component);
        }
        Some(path)
    }

    /// POSIX link count: entries naming a non-directory, and
    /// `2 + subdirectories` for a directory. Derived from this state only.
    pub fn nlink(&self, id: Hash32) -> u64 {
        match self.objects.get(&id) {
            Some(record) if record.kind == ObjectKind::Dir => {
                let subdirs = self
                    .entries
                    .values()
                    .filter(|entry| entry.parent == id)
                    .filter(|entry| {
                        self.objects
                            .get(&entry.target)
                            .map(|object| object.kind == ObjectKind::Dir)
                            .unwrap_or(false)
                    })
                    .count();
                2 + subdirs as u64
            }
            Some(_) => self
                .entries
                .values()
                .filter(|entry| entry.target == id)
                .count() as u64,
            None => 0,
        }
    }

    /// True if `node` is `ancestor` or a descendant of it.
    pub fn is_within(&self, ancestor: Hash32, node: Hash32) -> bool {
        let mut current = node;
        let mut visited = BTreeSet::new();
        loop {
            if current == ancestor {
                return true;
            }
            if !visited.insert(current) {
                return false;
            }
            match self.entries_for(current).into_iter().next() {
                Some(entry) => current = entry.parent,
                None => return false,
            }
        }
    }
}

pub(crate) fn validate_object_record(record: &ObjectRecord) -> Result<()> {
    // Reserved causal attributes must be complete and consistent whenever any
    // of them is present (`WEFT-CAUSAL.md` §3.1).
    object_causal_closure(record)?;
    if record.mode > 0o7777 {
        return Err(Error::malformed("mode must fit POSIX permission bits"));
    }
    if record.kind == ObjectKind::Dir && record.size != 0 {
        return Err(Error::malformed("directory objects must have size 0"));
    }
    if record.kind == ObjectKind::Symlink && record.attrs.get(ATTR_SYMLINK_TARGET).is_none() {
        return Err(Error::malformed(
            "symlink object is missing weft.symlink.target",
        ));
    }
    if let Some(value) = record.attrs.get(ATTR_ACL_READ) {
        match value {
            Value::Array(items) => {
                if !items.iter().all(|item| matches!(item, Value::U64(_))) {
                    return Err(Error::malformed(
                        "weft.acl.read must be an array of u64 uid values",
                    ));
                }
            }
            _ => {
                return Err(Error::malformed(
                    "weft.acl.read must be an array of u64 uid values",
                ))
            }
        }
    }
    Ok(())
}

/// Maximum POSIX path-component length in bytes.
pub const MAX_NAME_BYTES: usize = 255;

/// Reject names that cannot be a single POSIX path component.
pub fn validate_name_bytes(name: &[u8]) -> Result<()> {
    if name.is_empty() {
        return Err(Error::invalid_name("name must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(Error::invalid_name("name exceeds 255 bytes"));
    }
    if name.contains(&0) {
        return Err(Error::invalid_name("name must not contain NUL"));
    }
    if name.contains(&b'/') {
        return Err(Error::invalid_name("name must not contain '/'"));
    }
    if name == b"." || name == b".." {
        return Err(Error::invalid_name("'.' and '..' are reserved"));
    }
    Ok(())
}

/// Validate a snapshot name (UTF-8 text, no `/` or NUL).
pub fn validate_snapshot_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::invalid_name("snapshot name must not be empty"));
    }
    if name.len() > 255 {
        return Err(Error::invalid_name("snapshot name exceeds 255 bytes"));
    }
    if name.contains('\0') || name.contains('/') {
        return Err(Error::invalid_name(
            "snapshot name must not contain NUL or '/'",
        ));
    }
    if name == "." || name == ".." {
        return Err(Error::invalid_name("'.' and '..' are reserved"));
    }
    Ok(())
}
