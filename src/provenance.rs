//! First-class provenance receipts and provenance-guided reconstruction.
//!
//! A provenance record is a canonical WCB map (`weft.provenance.v1`) that
//! describes *one computation*: its operation, parameters, verified inputs,
//! declared outputs, environment, determinism class, principal, and timestamp.
//! It is content-addressed by
//! `ProvenanceId = H("weft/provenance/v1", WCB(record))` and stored as a
//! content-addressed blob in the manifest namespace under that hash. A
//! `provenance` relation on every output object (value target = lowercase hex
//! ProvenanceId) attaches the record to the bytes it produced.
//!
//! Provenance describes **computation**. Representation manifests describe
//! **bytes**. Neither replaces the other: a representation answers "how are
//! these bytes stored", a provenance record answers "how were these bytes
//! produced, from what, and can that computation be repeated".
//!
//! The recovery planner is pure and deterministic. It never falls back: if no
//! candidate strategy is eligible it returns [`NoEligibleStrategy`], which names
//! every rejected candidate and why.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::backend::StorageBackend;
use crate::error::{Class, Error, Result};
use crate::format::{Manifest, RepresentationKind};
use crate::identity::{self, Hash32};
use crate::recovery::{self, BlobKind};
use crate::state::{ObjectKind, Op, RelationRecord, RelationTarget, State};
use crate::store::Store;
use crate::wcb_util as wu;

/// Canonical schema string of a provenance record.
pub const PROVENANCE_SCHEMA: &str = "weft.provenance.v1";

/// Domain separation for `ProvenanceId`.
pub const PROVENANCE_DOMAIN: &str = "weft/provenance/v1";

/// Relation predicate that attaches a provenance record to an output object.
pub const PROVENANCE_PREDICATE: &str = "provenance";

/// Maximum canonical bytes of the `params` map.
pub const MAX_PROVENANCE_PARAMS_BYTES: u64 = 64 * 1024;

/// Maximum canonical bytes of the `environment` map.
pub const MAX_PROVENANCE_ENVIRONMENT_BYTES: u64 = 16 * 1024;

/// Maximum number of declared inputs.
pub const MAX_PROVENANCE_INPUTS: usize = 1_024;

/// Maximum number of declared outputs.
pub const MAX_PROVENANCE_OUTPUTS: usize = 1_024;

/// Maximum bytes of a single operator/role/tool/version text field.
pub const MAX_PROVENANCE_TEXT_BYTES: usize = 1_024;

/// Maximum canonical bytes of a whole encoded record.
pub const MAX_PROVENANCE_RECORD_BYTES: u64 = 1 << 20;

/// Default provenance closure depth.
pub const DEFAULT_PROVENANCE_CLOSURE_DEPTH: u32 = 8;

/// Hard provenance closure depth bound.
pub const MAX_PROVENANCE_CLOSURE_DEPTH: u32 = 64;

fn provenance_limits() -> Limits {
    Limits {
        max_structured_items: 65_536,
        max_structured_depth: 32,
        max_string_bytes: MAX_PROVENANCE_PARAMS_BYTES,
        ..Limits::default()
    }
}

fn encode_with_limits(value: &Value) -> Result<Vec<u8>> {
    weftcodec::wcb::encode(value, &provenance_limits()).map_err(Error::from)
}

fn decode_with_limits(bytes: &[u8]) -> Result<Value> {
    weftcodec::wcb::decode(bytes, &provenance_limits()).map_err(Error::from)
}

fn check_text_len(value: &str, what: &str) -> Result<()> {
    if value.len() > MAX_PROVENANCE_TEXT_BYTES {
        return Err(Error::limit_exceeded(format!(
            "provenance {what} is {} bytes, above the {MAX_PROVENANCE_TEXT_BYTES}-byte bound",
            value.len()
        )));
    }
    Ok(())
}

fn check_hash(value: Hash32, what: &str) -> Result<()> {
    if value == Hash32::ZERO {
        return Err(Error::malformed(format!(
            "provenance {what} must not be the all-zero hash"
        )));
    }
    Ok(())
}

/// Declared determinism of a recorded computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Determinism {
    /// Re-running the operation on the verified inputs yields the exact outputs.
    Exact,
    /// The producer declares determinism but the store cannot enforce it.
    Declared,
    /// Determinism is unknown.
    Unknown,
}

impl Determinism {
    pub fn as_str(self) -> &'static str {
        match self {
            Determinism::Exact => "exact",
            Determinism::Declared => "declared",
            Determinism::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "exact" => Ok(Determinism::Exact),
            "declared" => Ok(Determinism::Declared),
            "unknown" => Ok(Determinism::Unknown),
            other => Err(Error::malformed(format!(
                "provenance determinism `{other}` is not exact|declared|unknown"
            ))),
        }
    }
}

/// One declared input of a computation.
///
/// `content` is the store content root (manifest id) of the input bytes. `object`
/// is an optional ObjectId hint that resolved to that content root. Records are
/// sorted strictly by `(role, content)`; duplicates are non-canonical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceInput {
    pub role: String,
    pub content: Hash32,
    pub object: Option<Hash32>,
}

impl ProvenanceInput {
    pub const KEYS: &'static [&'static str] = &["role", "content", "object"];

    pub fn new(role: impl Into<String>, content: Hash32, object: Option<Hash32>) -> Self {
        ProvenanceInput {
            role: role.into(),
            content,
            object,
        }
    }

    fn cmp_key(&self, other: &Self) -> std::cmp::Ordering {
        self.role
            .cmp(&other.role)
            .then_with(|| self.content.cmp(&other.content))
    }

    fn to_value(&self) -> Value {
        wu::map([
            ("role", Value::Str(self.role.clone())),
            ("content", wu::hash_value(self.content)),
            ("object", wu::opt_hash_value(self.object)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let role = wu::get_text(map, "role")?;
        check_text_len(&role, "input role")?;
        if role.is_empty() {
            return Err(Error::malformed("provenance input role must not be empty"));
        }
        let content = wu::get_hash(map, "content")?;
        check_hash(content, "input content root")?;
        let object = wu::get_opt_hash(map, "object")?;
        if let Some(object) = object {
            check_hash(object, "input object id")?;
        }
        Ok(ProvenanceInput {
            role,
            content,
            object,
        })
    }
}

/// Declared environment: at least the tool, its version, and an optional
/// environment hash. Additional canonical fields are preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceEnvironment {
    pub tool: String,
    pub version: String,
    pub env_hash: Option<Hash32>,
    pub fields: BTreeMap<String, Value>,
}

impl ProvenanceEnvironment {
    pub const RESERVED: &'static [&'static str] = &["tool", "version", "env_hash"];

    pub fn new(tool: impl Into<String>, version: impl Into<String>) -> Self {
        ProvenanceEnvironment {
            tool: tool.into(),
            version: version.into(),
            env_hash: None,
            fields: BTreeMap::new(),
        }
    }

    pub fn with_env_hash(mut self, hash: Hash32) -> Result<Self> {
        check_hash(hash, "environment hash")?;
        self.env_hash = Some(hash);
        Ok(self)
    }

    pub fn with_field(mut self, key: impl Into<String>, value: Value) -> Result<Self> {
        let key = key.into();
        if Self::RESERVED.contains(&key.as_str()) {
            return Err(Error::invalid_argument(format!(
                "provenance environment field `{key}` is reserved"
            )));
        }
        self.fields.insert(key, value);
        Ok(self)
    }

    fn validate(&self) -> Result<()> {
        check_text_len(&self.tool, "tool")?;
        check_text_len(&self.version, "version")?;
        if self.tool.is_empty() {
            return Err(Error::malformed(
                "provenance environment tool must not be empty",
            ));
        }
        for key in self.fields.keys() {
            if Self::RESERVED.contains(&key.as_str()) {
                return Err(Error::malformed(format!(
                    "provenance environment field `{key}` is reserved"
                )));
            }
        }
        if encode_with_limits(&self.to_value())?.len() as u64 > MAX_PROVENANCE_ENVIRONMENT_BYTES {
            return Err(Error::limit_exceeded(format!(
                "provenance environment exceeds the {MAX_PROVENANCE_ENVIRONMENT_BYTES}-byte bound"
            )));
        }
        Ok(())
    }

    fn to_value(&self) -> Value {
        let mut map = self.fields.clone();
        map.insert("tool".to_owned(), Value::Str(self.tool.clone()));
        map.insert("version".to_owned(), Value::Str(self.version.clone()));
        map.insert("env_hash".to_owned(), wu::opt_hash_value(self.env_hash));
        Value::Map(map)
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        let tool = wu::get_text(map, "tool")?;
        let version = wu::get_text(map, "version")?;
        let env_hash = wu::get_opt_hash(map, "env_hash")?;
        if let Some(hash) = env_hash {
            check_hash(hash, "environment hash")?;
        }
        let mut fields = map.clone();
        for key in Self::RESERVED {
            fields.remove(*key);
        }
        let environment = ProvenanceEnvironment {
            tool,
            version,
            env_hash,
            fields,
        };
        environment.validate()?;
        Ok(environment)
    }
}

/// The principal that declared the computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProvenancePrincipal {
    pub uid: u64,
    pub gid: u64,
}

impl ProvenancePrincipal {
    pub fn new(uid: u64, gid: u64) -> Self {
        ProvenancePrincipal { uid, gid }
    }

    fn to_value(self) -> Value {
        wu::map([("uid", Value::U64(self.uid)), ("gid", Value::U64(self.gid))])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["uid", "gid"])?;
        Ok(ProvenancePrincipal {
            uid: wu::get_u64(map, "uid")?,
            gid: wu::get_u64(map, "gid")?,
        })
    }
}

/// A canonical provenance receipt (`weft.provenance.v1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceRecord {
    pub operation: String,
    pub params: BTreeMap<String, Value>,
    pub inputs: Vec<ProvenanceInput>,
    pub outputs: Vec<Hash32>,
    pub environment: ProvenanceEnvironment,
    pub determinism: Determinism,
    pub principal: ProvenancePrincipal,
    pub timestamp_ns: i64,
}

impl ProvenanceRecord {
    pub const KEYS: &'static [&'static str] = &[
        "schema",
        "operation",
        "params",
        "inputs",
        "outputs",
        "environment",
        "determinism",
        "principal",
        "timestamp_ns",
    ];

    /// Build a record, canonically ordering `inputs` (by role then content) and
    /// `outputs` (ascending). Duplicate inputs/outputs and out-of-bound fields
    /// are rejected.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation: impl Into<String>,
        params: BTreeMap<String, Value>,
        inputs: Vec<ProvenanceInput>,
        outputs: Vec<Hash32>,
        environment: ProvenanceEnvironment,
        determinism: Determinism,
        principal: ProvenancePrincipal,
        timestamp_ns: i64,
    ) -> Result<Self> {
        let mut record = ProvenanceRecord {
            operation: operation.into(),
            params,
            inputs,
            outputs,
            environment,
            determinism,
            principal,
            timestamp_ns,
        };
        record.inputs.sort_by(ProvenanceInput::cmp_key);
        record.outputs.sort_unstable();
        record.validate(true)?;
        Ok(record)
    }

    fn validate(&self, sorting_expected: bool) -> Result<()> {
        check_text_len(&self.operation, "operation")?;
        if self.operation.is_empty() {
            return Err(Error::malformed("provenance operation must not be empty"));
        }
        let params_bytes = encode_with_limits(&Value::Map(self.params.clone())).map_err(|err| {
            if err.class() == Class::LimitExceeded {
                Error::limit_exceeded(format!(
                    "provenance params exceed the {MAX_PROVENANCE_PARAMS_BYTES}-byte bound: {err}"
                ))
            } else {
                err
            }
        })?;
        if params_bytes.len() as u64 > MAX_PROVENANCE_PARAMS_BYTES {
            return Err(Error::limit_exceeded(format!(
                "provenance params are {} bytes, above the {MAX_PROVENANCE_PARAMS_BYTES}-byte bound",
                params_bytes.len()
            )));
        }
        if self.inputs.len() > MAX_PROVENANCE_INPUTS {
            return Err(Error::limit_exceeded(format!(
                "provenance declares {} inputs, above the {MAX_PROVENANCE_INPUTS}-input bound",
                self.inputs.len()
            )));
        }
        if self.outputs.len() > MAX_PROVENANCE_OUTPUTS {
            return Err(Error::limit_exceeded(format!(
                "provenance declares {} outputs, above the {MAX_PROVENANCE_OUTPUTS}-output bound",
                self.outputs.len()
            )));
        }
        for input in &self.inputs {
            check_text_len(&input.role, "input role")?;
        }
        for output in &self.outputs {
            check_hash(*output, "output content root")?;
        }
        if sorting_expected {
            for pair in self.inputs.windows(2) {
                if pair[0].cmp_key(&pair[1]) != std::cmp::Ordering::Less {
                    return Err(Error::non_canonical(
                        "provenance inputs must be strictly ordered by (role, content) with no duplicates",
                    ));
                }
            }
            for pair in self.outputs.windows(2) {
                if pair[0] >= pair[1] {
                    return Err(Error::non_canonical(
                        "provenance outputs must be strictly ordered with no duplicates",
                    ));
                }
            }
        }
        self.environment.validate()?;
        let bytes = encode_with_limits(&self.to_value())?;
        if bytes.len() as u64 > MAX_PROVENANCE_RECORD_BYTES {
            return Err(Error::limit_exceeded(format!(
                "provenance record is {} bytes, above the {MAX_PROVENANCE_RECORD_BYTES}-byte bound",
                bytes.len()
            )));
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        let inputs: Vec<Value> = self.inputs.iter().map(ProvenanceInput::to_value).collect();
        let outputs: Vec<Value> = self
            .outputs
            .iter()
            .map(|hash| wu::hash_value(*hash))
            .collect();
        wu::map([
            ("schema", Value::Str(PROVENANCE_SCHEMA.to_owned())),
            ("operation", Value::Str(self.operation.clone())),
            ("params", Value::Map(self.params.clone())),
            ("inputs", Value::Array(inputs)),
            ("outputs", Value::Array(outputs)),
            ("environment", self.environment.to_value()),
            (
                "determinism",
                Value::Str(self.determinism.as_str().to_owned()),
            ),
            ("principal", self.principal.to_value()),
            ("timestamp_ns", Value::Timestamp(self.timestamp_ns)),
        ])
    }

    /// Parse a canonical record value. Inputs/outputs MUST already be in
    /// canonical order; this function never sorts, so a hand-built or rewritten
    /// record with unsorted or duplicate entries is `NonCanonical`.
    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != PROVENANCE_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "provenance schema `{schema}` is not `{PROVENANCE_SCHEMA}`"
            )));
        }
        let operation = wu::get_text(map, "operation")?;
        let params = wu::get_map(map, "params")?;
        let mut inputs = Vec::new();
        for item in wu::get_array(map, "inputs")? {
            inputs.push(ProvenanceInput::from_value(item)?);
        }
        let mut outputs = Vec::new();
        for item in wu::get_array(map, "outputs")? {
            match item {
                Value::Bytes(bytes) => {
                    outputs.push(Hash32::from_slice(bytes).map_err(Error::from)?)
                }
                _ => return Err(Error::malformed("provenance output must be bytes32")),
            }
        }
        let environment = ProvenanceEnvironment::from_value(wu::field(map, "environment")?)?;
        let determinism = Determinism::parse(&wu::get_text(map, "determinism")?)?;
        let principal = ProvenancePrincipal::from_value(wu::field(map, "principal")?)?;
        let timestamp_ns = wu::get_timestamp(map, "timestamp_ns")?;
        let record = ProvenanceRecord {
            operation,
            params,
            inputs,
            outputs,
            environment,
            determinism,
            principal,
            timestamp_ns,
        };
        record.validate(true)?;
        Ok(record)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_with_limits(&self.to_value())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = decode_with_limits(bytes)?;
        Self::from_value(&value)
    }

    /// `ProvenanceId = H("weft/provenance/v1", WCB(record))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(PROVENANCE_DOMAIN, &self.encode()?))
    }
}

/// Parse the value target of a `provenance` relation.
///
/// The value target is the lowercase 64-hex ProvenanceId. v1 relation value
/// targets admit text but not raw `bytes32`; hexadecimal is the canonical text
/// encoding of a hash everywhere else in the format, so this keeps the relation
/// record inside the frozen relation grammar.
pub(crate) fn provenance_id_from_relation(relation: &RelationRecord) -> Result<Hash32> {
    if relation.predicate != PROVENANCE_PREDICATE {
        return Err(Error::corrupt("relation is not a provenance relation"));
    }
    let text = match &relation.target {
        RelationTarget::Value(Value::Str(text)) => text,
        _ => {
            return Err(Error::corrupt(
                "provenance relation value target must be a hexadecimal ProvenanceId",
            ))
        }
    };
    let id = Hash32::from_hex(text)
        .map_err(|_| Error::corrupt("provenance relation value target is not a 32-byte hash"))?;
    if id.to_hex() != *text {
        return Err(Error::corrupt(
            "provenance relation value target must be lowercase hexadecimal",
        ));
    }
    Ok(id)
}

/// Read and verify a provenance blob.
pub(crate) fn read_provenance_via(
    backend: &dyn StorageBackend,
    id: Hash32,
) -> Result<ProvenanceRecord> {
    let bytes =
        recovery::read_blob_verified_via(backend, BlobKind::Manifest, id, PROVENANCE_DOMAIN)?;
    let record = ProvenanceRecord::decode(&bytes)?;
    if record.id()? != id {
        return Err(Error::corrupt(format!(
            "provenance blob {id} does not re-encode to its own identity"
        )));
    }
    Ok(record)
}

/// One node of a transitive provenance closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceNode {
    /// Distance from the requested target (the target itself is depth 0).
    pub depth: u32,
    /// Content root of this node.
    pub content: Hash32,
    /// Object id, when the node resolves to a live object.
    pub object: Option<Hash32>,
    /// Role by which this node was reached (`None` for the target).
    pub role: Option<String>,
    /// Provenance record attached to this node's object, when one exists.
    pub provenance: Option<Hash32>,
}

/// A bounded, deterministic provenance closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceClosure {
    pub target: Hash32,
    pub depth_limit: u32,
    /// Nodes ordered by `(depth, content, object, role, provenance)`. The target
    /// is always the first node.
    pub nodes: Vec<ProvenanceNode>,
}

impl Store {
    /// Record a provenance receipt and attach it atomically to every output
    /// object in one commit.
    ///
    /// The record's `outputs` MUST equal the sorted, deduplicated content roots
    /// of the supplied output objects. Any mismatch (a declared root that does
    /// not match an object's actual content, a missing output, or an extra
    /// output) is rejected before anything is written.
    pub fn record_provenance(
        &mut self,
        outputs: &[Hash32],
        record: &ProvenanceRecord,
    ) -> Result<Hash32> {
        self.require_writable()?;
        if outputs.is_empty() {
            return Err(Error::invalid_argument(
                "record_provenance requires at least one output object",
            ));
        }
        let mut seen = BTreeSet::new();
        let mut actual = Vec::with_capacity(outputs.len());
        for id in outputs {
            if !seen.insert(*id) {
                return Err(Error::invalid_argument(format!(
                    "duplicate output object {id}"
                )));
            }
            let object =
                self.current_state().objects.get(id).ok_or_else(|| {
                    Error::not_found(format!("output object {id} does not exist"))
                })?;
            if object.kind != ObjectKind::File {
                return Err(Error::invalid_argument(format!(
                    "output object {id} is not a regular file"
                )));
            }
            actual.push(object.content);
        }
        actual.sort_unstable();
        actual.dedup();
        if actual != record.outputs {
            return Err(Error::invalid_argument(
                "provenance outputs do not match the supplied objects' content roots",
            ));
        }

        let bytes = record.encode()?;
        let id = identity::h(PROVENANCE_DOMAIN, &bytes);
        if record.id()? != id {
            return Err(Error::corrupt(
                "provenance record does not re-encode to its own identity",
            ));
        }
        // The blob is durable before the relation becomes visible. A crash
        // between the two leaves an orphan blob (collected by GC), never a
        // relation pointing at missing bytes.
        self.store_blob(BlobKind::Manifest, PROVENANCE_DOMAIN, &bytes)?;

        let mut ops = Vec::with_capacity(outputs.len());
        for output in outputs {
            let relation = RelationRecord::new(
                *output,
                PROVENANCE_PREDICATE,
                RelationTarget::Value(Value::Str(id.to_hex())),
                None,
                BTreeMap::new(),
            )?;
            ops.push(Op::PutRelation(relation));
        }
        self.commit_ops(ops, false)?;
        Ok(id)
    }

    /// Provenance ids attached to an object, sorted and deduplicated.
    pub fn provenance_ids_of(&self, object_id: Hash32) -> Result<Vec<Hash32>> {
        let mut ids = Vec::new();
        for relation in self.current_state().relations.values() {
            if relation.subject == object_id && relation.predicate == PROVENANCE_PREDICATE {
                ids.push(provenance_id_from_relation(relation)?);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Read a provenance record by identity.
    pub fn read_provenance(&self, id: Hash32) -> Result<ProvenanceRecord> {
        self.with_backend(|backend| read_provenance_via(backend, id))
    }

    /// All provenance records attached to an object, ordered by ProvenanceId.
    pub fn provenance_records_of(
        &self,
        object_id: Hash32,
    ) -> Result<Vec<(Hash32, ProvenanceRecord)>> {
        let mut out = Vec::new();
        for id in self.provenance_ids_of(object_id)? {
            out.push((id, self.read_provenance(id)?));
        }
        Ok(out)
    }

    /// The first (lowest-id) provenance record attached to an object.
    ///
    /// Deterministic when an object carries several receipts (for example after
    /// a re-run). Use [`Store::provenance_records_of`] to see all of them.
    pub fn provenance_of(&self, object_id: Hash32) -> Result<Option<ProvenanceRecord>> {
        match self.provenance_ids_of(object_id)?.first() {
            Some(id) => self.read_provenance(*id).map(Some),
            None => Ok(None),
        }
    }

    /// Walk the transitive provenance closure of an object.
    ///
    /// Inputs are followed through their optional `object` hint. When the hint
    /// is absent, a content root that resolves to exactly one live object is
    /// followed; otherwise the input is an external leaf. A repeated object on
    /// the current path is a cycle (`Corrupt`); a closure that would exceed
    /// `max_depth` is `LimitExceeded`. No silent truncation.
    pub fn provenance_closure(&self, target: Hash32, max_depth: u32) -> Result<ProvenanceClosure> {
        if max_depth == 0 || max_depth > MAX_PROVENANCE_CLOSURE_DEPTH {
            return Err(Error::invalid_argument(format!(
                "provenance closure depth must be 1..={MAX_PROVENANCE_CLOSURE_DEPTH}"
            )));
        }
        let state = self.current_state();
        let record = state
            .objects
            .get(&target)
            .ok_or_else(|| Error::not_found("no such object"))?;
        let mut nodes = Vec::new();
        let mut visited: BTreeSet<Hash32> = BTreeSet::new();
        let mut path: BTreeSet<Hash32> = BTreeSet::new();
        let provenance = self.provenance_ids_of(target)?.first().copied();
        nodes.push(ProvenanceNode {
            depth: 0,
            content: record.content,
            object: Some(target),
            role: None,
            provenance,
        });
        visited.insert(target);
        self.walk_provenance(
            state,
            target,
            0,
            max_depth,
            &mut visited,
            &mut path,
            &mut nodes,
        )?;
        nodes.sort_by(|a, b| {
            (
                a.depth,
                a.content,
                a.object,
                a.role.as_deref(),
                a.provenance,
            )
                .cmp(&(
                    b.depth,
                    b.content,
                    b.object,
                    b.role.as_deref(),
                    b.provenance,
                ))
        });
        Ok(ProvenanceClosure {
            target,
            depth_limit: max_depth,
            nodes,
        })
    }

    fn walk_provenance(
        &self,
        state: &State,
        object_id: Hash32,
        depth: u32,
        max_depth: u32,
        visited: &mut BTreeSet<Hash32>,
        path: &mut BTreeSet<Hash32>,
        nodes: &mut Vec<ProvenanceNode>,
    ) -> Result<()> {
        let ids = self.provenance_ids_of(object_id)?;
        if ids.is_empty() {
            return Ok(());
        }
        if depth >= max_depth {
            return Err(Error::limit_exceeded(format!(
                "provenance closure depth {max_depth} reached at object {object_id}, which still has provenance"
            )));
        }
        path.insert(object_id);
        for id in ids {
            let record = self.read_provenance(id)?;
            for input in &record.inputs {
                let child = match input.object {
                    Some(object) if state.objects.contains_key(&object) => Some(object),
                    _ => resolve_content_hint(state, input.content),
                };
                let child_depth = depth + 1;
                let child_provenance = match child {
                    Some(child) => self.provenance_ids_of(child)?.first().copied(),
                    None => None,
                };
                nodes.push(ProvenanceNode {
                    depth: child_depth,
                    content: input.content,
                    object: child,
                    role: Some(input.role.clone()),
                    provenance: child_provenance,
                });
                if let Some(child) = child {
                    if path.contains(&child) {
                        return Err(Error::corrupt(format!(
                            "provenance closure cycle: object {child} is its own ancestor"
                        )));
                    }
                    if visited.insert(child) {
                        self.walk_provenance(
                            state,
                            child,
                            child_depth,
                            max_depth,
                            visited,
                            path,
                            nodes,
                        )?;
                    }
                }
            }
        }
        path.remove(&object_id);
        Ok(())
    }

    /// Build the recovery candidates for a live object.
    ///
    /// A raw (chunked) manifest offers `fetch`; a frame-backed codec/tensor
    /// manifest offers `decode`; every attached provenance record offers
    /// `recompute` with its declared determinism and the inputs that cannot be
    /// verified. Costs are caller-declared cost units (nanoseconds by
    /// convention); a strategy declares a non-zero component only for work it
    /// actually performs.
    pub fn recovery_candidates(
        &self,
        object_id: Hash32,
        costs: RecoveryCosts,
    ) -> Result<Vec<RecoveryCandidate>> {
        let object = self.object_meta(object_id)?;
        let manifest =
            self.read_manifest_keyed(object.content)?;
        let raw = manifest
            .representation
            .as_ref()
            .map(|rep| rep.kind == RepresentationKind::Raw)
            .unwrap_or(true);
        let frame_backed = manifest
            .representation
            .as_ref()
            .map(|rep| {
                rep.kind == RepresentationKind::Codec || rep.kind == RepresentationKind::Tensor
            })
            .unwrap_or(false);

        let mut candidates = Vec::new();
        candidates.push(RecoveryCandidate {
            strategy: RecoveryStrategy::Fetch,
            name: "fetch".to_owned(),
            available: object.kind == ObjectKind::File
                && raw
                && self.verify_raw_content(&manifest).is_ok(),
            determinism: Determinism::Unknown,
            missing_inputs: Vec::new(),
            fetch_cost: costs.fetch,
            decode_cost: 0,
            recompute_cost: 0,
        });
        candidates.push(RecoveryCandidate {
            strategy: RecoveryStrategy::Decode,
            name: "decode".to_owned(),
            available: object.kind == ObjectKind::File
                && frame_backed
                && self
                    .verify_framed_content(object.content, &manifest)
                    .is_ok(),
            determinism: Determinism::Unknown,
            missing_inputs: Vec::new(),
            fetch_cost: 0,
            decode_cost: costs.decode,
            recompute_cost: 0,
        });

        for id in self.provenance_ids_of(object_id)? {
            let record = self.read_provenance(id)?;
            let mut missing = Vec::new();
            for input in &record.inputs {
                if !self.content_verified(input.content) {
                    missing.push(input.content);
                }
            }
            missing.sort_unstable();
            missing.dedup();
            candidates.push(RecoveryCandidate {
                strategy: RecoveryStrategy::Recompute,
                name: format!("recompute:{}", id.to_hex()),
                available: true,
                determinism: record.determinism,
                missing_inputs: missing,
                fetch_cost: 0,
                decode_cost: 0,
                recompute_cost: costs.recompute,
            });
        }
        Ok(candidates)
    }

    /// Verify that a raw chunked manifest's bytes are retained and hash-correct.
    fn verify_raw_content(&self, manifest: &Manifest) -> Result<()> {
        if manifest.size == 0 {
            // The empty root is fixed by the identity, not by a blob read.
            let expected = match manifest.identity {
                crate::format::ContentIdentity::WholeFileV1 => identity::content_hash(b""),
                crate::format::ContentIdentity::MerkleV3 => identity::merkle_empty_root(),
            };
            if manifest.content_hash != expected {
                return Err(Error::corrupt(
                    "empty content root mismatch while verifying retention",
                ));
            }
            return Ok(());
        }
        let merkle = manifest.identity == crate::format::ContentIdentity::MerkleV3;
        let mut hasher = identity::content_hash_hasher();
        let mut leaves: Vec<(u64, Hash32)> = Vec::new();
        let mut total: u64 = 0;
        for chunk in &manifest.chunks {
            let bytes = self.read_blob_verified_keyed(BlobKind::Object, chunk.hash, identity::CHUNK_DOMAIN)?;
            if bytes.len() as u64 != chunk.len {
                return Err(Error::corrupt(format!(
                    "chunk {} length disagrees with the manifest",
                    chunk.hash
                )));
            }
            total = total.saturating_add(bytes.len() as u64);
            if merkle {
                leaves.push((chunk.len, chunk.hash));
            } else {
                hasher.update(&bytes);
            }
        }
        let digest = if merkle {
            identity::merkle_root_from_chunks(leaves)
        } else {
            identity::finish_content_hash(hasher)
        };
        if total != manifest.size || digest != manifest.content_hash {
            return Err(Error::corrupt(
                "content hash mismatch while verifying retention",
            ));
        }
        Ok(())
    }

    /// Verify that a frame-backed manifest's dependency chain decodes exactly.
    fn verify_framed_content(&self, content_root: Hash32, manifest: &Manifest) -> Result<()> {
        let mut seen = vec![content_root];
        self.read_manifest_bytes_inner(manifest, 0, manifest.size, true, 0, &mut seen)?;
        Ok(())
    }

    /// Is this content root present and exactly verifiable in this store?
    fn content_verified(&self, content_root: Hash32) -> bool {
        let manifest =
            match self.read_manifest_keyed(content_root) {
                Ok(manifest) => manifest,
                Err(_) => return false,
            };
        let frame_backed = manifest
            .representation
            .as_ref()
            .map(|rep| {
                rep.kind == RepresentationKind::Codec || rep.kind == RepresentationKind::Tensor
            })
            .unwrap_or(false);
        if frame_backed {
            self.verify_framed_content(content_root, &manifest).is_ok()
        } else {
            self.verify_raw_content(&manifest).is_ok()
        }
    }
}

fn resolve_content_hint(state: &State, content: Hash32) -> Option<Hash32> {
    let mut found = None;
    for record in state.objects.values() {
        if record.kind == ObjectKind::File && record.content == content {
            if found.is_some() {
                // Ambiguous content root: do not guess which object the input
                // refers to. The node stays an external leaf.
                return None;
            }
            found = Some(record.id);
        }
    }
    found
}

/// Cost components declared for each recovery strategy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryCosts {
    pub fetch: u64,
    pub decode: u64,
    pub recompute: u64,
}

/// A recovery strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecoveryStrategy {
    /// Read retained (raw chunked) bytes.
    Fetch,
    /// Reconstruct through a representation dependency chain.
    Decode,
    /// Run the declared operation from verified inputs.
    Recompute,
}

impl RecoveryStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            RecoveryStrategy::Fetch => "fetch",
            RecoveryStrategy::Decode => "decode",
            RecoveryStrategy::Recompute => "recompute",
        }
    }
}

/// Why a candidate was rejected by the planner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryRejection {
    /// The bytes or representation the strategy needs are not available.
    Unavailable,
    /// `recompute` requires `determinism == "exact"`.
    NonExactDeterminism(Determinism),
    /// `recompute` requires every declared input to be available and verified.
    MissingInputs(Vec<Hash32>),
}

impl fmt::Display for RecoveryRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecoveryRejection::Unavailable => f.write_str("unavailable"),
            RecoveryRejection::NonExactDeterminism(determinism) => {
                write!(f, "non-exact determinism ({})", determinism.as_str())
            }
            RecoveryRejection::MissingInputs(inputs) => {
                f.write_str("missing input")?;
                if inputs.len() > 1 {
                    f.write_str("s")?;
                }
                f.write_str(" ")?;
                let rendered: Vec<String> = inputs.iter().map(|hash| hash.to_hex()).collect();
                f.write_str(&rendered.join(","))
            }
        }
    }
}

/// One declared recovery candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryCandidate {
    pub strategy: RecoveryStrategy,
    /// Deterministic tie-break name, unique within a strategy where possible.
    pub name: String,
    /// Whether the bytes/representation this strategy needs are present.
    pub available: bool,
    /// Determinism declared by the provenance record (used by `recompute`).
    pub determinism: Determinism,
    /// Declared inputs that could not be verified (used by `recompute`).
    pub missing_inputs: Vec<Hash32>,
    pub fetch_cost: u64,
    pub decode_cost: u64,
    pub recompute_cost: u64,
}

impl RecoveryCandidate {
    /// Total cost of the components this candidate actually incurs.
    pub fn total_cost(&self) -> u64 {
        self.fetch_cost
            .saturating_add(self.decode_cost)
            .saturating_add(self.recompute_cost)
    }

    /// `None` when the candidate is eligible; otherwise the reason it is not.
    pub fn rejection(&self) -> Option<RecoveryRejection> {
        self.rejections().into_iter().next()
    }

    /// Every reason this candidate is ineligible (empty when eligible).
    pub fn rejections(&self) -> Vec<RecoveryRejection> {
        let mut reasons = Vec::new();
        if !self.available {
            reasons.push(RecoveryRejection::Unavailable);
        }
        if self.strategy == RecoveryStrategy::Recompute {
            if self.determinism != Determinism::Exact {
                reasons.push(RecoveryRejection::NonExactDeterminism(self.determinism));
            }
            if !self.missing_inputs.is_empty() {
                reasons.push(RecoveryRejection::MissingInputs(
                    self.missing_inputs.clone(),
                ));
            }
        }
        reasons
    }
}

/// The planner's chosen strategy and its declared cost components.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryChoice {
    pub target: Hash32,
    pub strategy: RecoveryStrategy,
    pub name: String,
    pub cost: u64,
    pub fetch_cost: u64,
    pub decode_cost: u64,
    pub recompute_cost: u64,
}

/// Every candidate was ineligible. Each name and every reason is preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoEligibleStrategy {
    pub target: Hash32,
    pub rejections: Vec<(String, Vec<RecoveryRejection>)>,
}

impl fmt::Display for NoEligibleStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "no eligible recovery strategy for {}: ",
            self.target.to_hex()
        )?;
        if self.rejections.is_empty() {
            return f.write_str("no candidates were supplied");
        }
        for (index, (name, reasons)) in self.rejections.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{name} (")?;
            for (reason_index, reason) in reasons.iter().enumerate() {
                if reason_index > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{reason}")?;
            }
            f.write_str(")")?;
        }
        Ok(())
    }
}

impl std::error::Error for NoEligibleStrategy {}

impl From<NoEligibleStrategy> for Error {
    fn from(error: NoEligibleStrategy) -> Self {
        Error::not_found(error.to_string())
    }
}

/// Choose a recovery strategy.
///
/// Eligible candidates are ordered by
/// `(total_cost, strategy rank, name)` where the rank is
/// `fetch < decode < recompute`. The first candidate wins, so the result is
/// independent of input order and fully deterministic. If no candidate is
/// eligible, [`NoEligibleStrategy`] names every rejection.
pub fn choose_recovery(
    target: Hash32,
    candidates: &[RecoveryCandidate],
) -> std::result::Result<RecoveryChoice, NoEligibleStrategy> {
    let mut best: Option<(&RecoveryCandidate, u64)> = None;
    let mut rejections = Vec::new();
    for candidate in candidates {
        let reasons = candidate.rejections();
        if !reasons.is_empty() {
            rejections.push((candidate.name.clone(), reasons));
            continue;
        }
        let total = candidate.total_cost();
        let replace = match best {
            None => true,
            Some((current, current_total)) => {
                (total, candidate.strategy, candidate.name.as_str())
                    < (current_total, current.strategy, current.name.as_str())
            }
        };
        if replace {
            best = Some((candidate, total));
        }
    }
    match best {
        Some((candidate, cost)) => Ok(RecoveryChoice {
            target,
            strategy: candidate.strategy,
            name: candidate.name.clone(),
            cost,
            fetch_cost: candidate.fetch_cost,
            decode_cost: candidate.decode_cost,
            recompute_cost: candidate.recompute_cost,
        }),
        None => Err(NoEligibleStrategy { target, rejections }),
    }
}
