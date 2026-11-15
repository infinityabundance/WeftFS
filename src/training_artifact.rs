//! The composable Training Artifact (P01).
//!
//! Normative description: `spec/WEFT-TRAINING-ARTIFACT.md`.
//!
//! A Training Artifact composes identities that already exist — per-tensor
//! `LogicalTensorRoot`s, whole-tensor content roots, a `weft.checkpoint.v1`
//! manifest, a Context Root, a causal closure and provenance receipts — into
//! one canonical declaration. It adds no new byte-level identity.
//!
//! # The three classes are never merged
//!
//! * exact logical tensor roots identify decoded values;
//! * `TrainingArtifactRoot` identifies the immutable declaration;
//! * grants, retention bindings and quarantine decisions are mutable
//!   authority records and are **not** inputs to the root.
//!
//! [`Store::set_training_grant`] edits a grant and leaves
//! `TrainingArtifactRoot` unchanged; [`Store::validate_training_artifact_at`]
//! recomputes every member identity and never trusts a stored hash.

use std::collections::{BTreeMap, BTreeSet};

use weftcodec::context::CausalClosure;
use weftcodec::tensor::{dtype_from_name, logical_root};
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::causal_closure::{self, CausalGraph, ClosureBounds, ClosureMode, ClosureResult};
use crate::checkpoint::CheckpointManifest;
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::state::{ObjectRecord, State};
use crate::store::Store;
use crate::wcb_util as wu;

/// Artifact record schema.
pub const TRAINING_ARTIFACT_SCHEMA: &str = "weft.training-artifact.v1";
/// Artifact root domain.
pub const TRAINING_ARTIFACT_DOMAIN: &str = "weft/training-artifact/v1";
/// Directory holding artifact records.
pub const TRAINING_ARTIFACT_DIR: &str = "training/artifacts";
/// Grant schema.
pub const TRAINING_GRANT_SCHEMA: &str = "weft.training-grant.v1";
/// Grant domain.
pub const TRAINING_GRANT_DOMAIN: &str = "weft/training-grant/v1";
/// Directory holding grant records.
pub const TRAINING_GRANT_DIR: &str = "training/grants";
/// Name of the monotonic grant epoch file inside a grant directory.
pub const TRAINING_GRANT_EPOCH: &str = "epoch";

/// Hard bound on tensors in one artifact.
pub const MAX_ARTIFACT_TENSORS: usize = 1_000_000;
/// Hard bound on shards in one artifact.
pub const MAX_ARTIFACT_SHARDS: usize = 16_777_216;
/// Hard bound on the artifact name.
pub const MAX_ARTIFACT_NAME: usize = 256;

/// Actions a grant may name.
pub const GRANT_ACTIONS: [&str; 4] = ["evaluate", "materialize", "replicate", "train"];

/// The artifact's declared lineage claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineageMode {
    None,
    Declared,
    Complete,
}

impl LineageMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LineageMode::None => "none",
            LineageMode::Declared => "declared",
            LineageMode::Complete => "complete",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "none" => Ok(LineageMode::None),
            "declared" => Ok(LineageMode::Declared),
            "complete" => Ok(LineageMode::Complete),
            other => Err(Error::invalid_argument(format!(
                "lineage mode `{other}` is not `none`, `declared` or `complete`"
            ))),
        }
    }
}

/// An immutable declaration of what the artifact claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingPolicy {
    pub lineage: LineageMode,
    /// Sorted unique dtype names; empty means any.
    pub allowed_dtypes: Vec<String>,
    pub minimum_tensors: u64,
    pub checkpoint_complete: bool,
}

impl Default for TrainingPolicy {
    fn default() -> Self {
        TrainingPolicy {
            lineage: LineageMode::Declared,
            allowed_dtypes: Vec::new(),
            minimum_tensors: 1,
            checkpoint_complete: true,
        }
    }
}

impl TrainingPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.minimum_tensors == 0 {
            return Err(Error::malformed("policy minimum_tensors must be >= 1"));
        }
        let mut previous: Option<&str> = None;
        for dtype in &self.allowed_dtypes {
            dtype_from_name(dtype).map_err(|err| {
                Error::malformed(format!("policy allowed dtype `{dtype}`: {err}"))
            })?;
            if let Some(previous) = previous {
                if dtype.as_str() <= previous {
                    return Err(Error::non_canonical(
                        "policy allowed_dtypes must be sorted and unique",
                    ));
                }
            }
            previous = Some(dtype.as_str());
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("lineage", Value::Str(self.lineage.as_str().to_owned())),
            (
                "allowed_dtypes",
                Value::Array(
                    self.allowed_dtypes
                        .iter()
                        .map(|dtype| Value::Str(dtype.clone()))
                        .collect(),
                ),
            ),
            ("minimum_tensors", Value::U64(self.minimum_tensors)),
            ("checkpoint_complete", Value::Bool(self.checkpoint_complete)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(
            map,
            &[
                "lineage",
                "allowed_dtypes",
                "minimum_tensors",
                "checkpoint_complete",
            ],
        )?;
        let lineage = LineageMode::parse(&wu::get_text(map, "lineage")?)?;
        let mut allowed_dtypes = Vec::new();
        for item in wu::get_array(map, "allowed_dtypes")? {
            match item {
                Value::Str(text) => allowed_dtypes.push(text.clone()),
                _ => return Err(Error::malformed("allowed_dtypes entries must be text")),
            }
        }
        let minimum_tensors = wu::get_u64(map, "minimum_tensors")?;
        let checkpoint_complete = match wu::field(map, "checkpoint_complete")? {
            Value::Bool(value) => *value,
            _ => return Err(Error::malformed("checkpoint_complete must be a bool")),
        };
        let policy = TrainingPolicy {
            lineage,
            allowed_dtypes,
            minimum_tensors,
            checkpoint_complete,
        };
        policy.validate()?;
        Ok(policy)
    }
}

/// One member shard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingShard {
    pub axis: u32,
    pub shards: u32,
    pub index: u32,
    pub object: Hash32,
    pub content_root: Hash32,
    pub logical_root: Hash32,
    pub dtype: String,
    pub shape: Vec<u64>,
    pub byte_len: u64,
}

impl TrainingShard {
    fn to_value(&self) -> Value {
        wu::map([
            ("axis", Value::U64(u64::from(self.axis))),
            ("shards", Value::U64(u64::from(self.shards))),
            ("index", Value::U64(u64::from(self.index))),
            ("object", wu::hash_value(self.object)),
            ("content_root", wu::hash_value(self.content_root)),
            ("logical_root", wu::hash_value(self.logical_root)),
            ("dtype", Value::Str(self.dtype.clone())),
            (
                "shape",
                Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
            ("byte_len", Value::U64(self.byte_len)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(
            map,
            &[
                "axis",
                "shards",
                "index",
                "object",
                "content_root",
                "logical_root",
                "dtype",
                "shape",
                "byte_len",
            ],
        )?;
        let axis = u32_from_u64(wu::get_u64(map, "axis")?, "shard axis")?;
        let shards = u32_from_u64(wu::get_u64(map, "shards")?, "shard count")?;
        let index = u32_from_u64(wu::get_u64(map, "index")?, "shard index")?;
        if shards == 0 || index >= shards {
            return Err(Error::non_canonical(
                "shard index must be in 0..shards and shards must be positive",
            ));
        }
        Ok(TrainingShard {
            axis,
            shards,
            index,
            object: wu::get_hash(map, "object")?,
            content_root: wu::get_hash(map, "content_root")?,
            logical_root: wu::get_hash(map, "logical_root")?,
            dtype: wu::get_text(map, "dtype")?,
            shape: get_u64_array(map, "shape")?,
            byte_len: wu::get_u64(map, "byte_len")?,
        })
    }
}

/// One member tensor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    pub logical_root: Hash32,
    pub content_root: Hash32,
    pub element_bytes: u64,
    pub source_offset: u64,
    pub shards: Vec<TrainingShard>,
}

impl TrainingTensor {
    fn to_value(&self) -> Value {
        wu::map([
            ("name", Value::Str(self.name.clone())),
            ("dtype", Value::Str(self.dtype.clone())),
            (
                "shape",
                Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
            ("logical_root", wu::hash_value(self.logical_root)),
            ("content_root", wu::hash_value(self.content_root)),
            ("element_bytes", Value::U64(self.element_bytes)),
            ("source_offset", Value::U64(self.source_offset)),
            (
                "shards",
                Value::Array(self.shards.iter().map(TrainingShard::to_value).collect()),
            ),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(
            map,
            &[
                "name",
                "dtype",
                "shape",
                "logical_root",
                "content_root",
                "element_bytes",
                "source_offset",
                "shards",
            ],
        )?;
        let mut shards = Vec::new();
        for item in wu::get_array(map, "shards")? {
            shards.push(TrainingShard::from_value(item)?);
        }
        Ok(TrainingTensor {
            name: wu::get_text(map, "name")?,
            dtype: wu::get_text(map, "dtype")?,
            shape: get_u64_array(map, "shape")?,
            logical_root: wu::get_hash(map, "logical_root")?,
            content_root: wu::get_hash(map, "content_root")?,
            element_bytes: wu::get_u64(map, "element_bytes")?,
            source_offset: wu::get_u64(map, "source_offset")?,
            shards,
        })
    }
}

/// The optional checkpoint anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingCheckpointAnchor {
    pub name: String,
    pub manifest_object: Hash32,
    pub manifest_content: Hash32,
    pub manifest_root: Hash32,
    pub complete: bool,
    pub source_sha256: Option<Hash32>,
    pub tensor_count: u64,
    pub element_bytes: u64,
}

impl TrainingCheckpointAnchor {
    fn to_value(&self) -> Value {
        wu::map([
            ("name", Value::Str(self.name.clone())),
            ("manifest_object", wu::hash_value(self.manifest_object)),
            ("manifest_content", wu::hash_value(self.manifest_content)),
            ("manifest_root", wu::hash_value(self.manifest_root)),
            ("complete", Value::Bool(self.complete)),
            ("source_sha256", wu::opt_hash_value(self.source_sha256)),
            ("tensor_count", Value::U64(self.tensor_count)),
            ("element_bytes", Value::U64(self.element_bytes)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(
            map,
            &[
                "name",
                "manifest_object",
                "manifest_content",
                "manifest_root",
                "complete",
                "source_sha256",
                "tensor_count",
                "element_bytes",
            ],
        )?;
        let complete = match wu::field(map, "complete")? {
            Value::Bool(value) => *value,
            _ => return Err(Error::malformed("checkpoint complete must be a bool")),
        };
        Ok(TrainingCheckpointAnchor {
            name: wu::get_text(map, "name")?,
            manifest_object: wu::get_hash(map, "manifest_object")?,
            manifest_content: wu::get_hash(map, "manifest_content")?,
            manifest_root: wu::get_hash(map, "manifest_root")?,
            complete,
            source_sha256: wu::get_opt_hash(map, "source_sha256")?,
            tensor_count: wu::get_u64(map, "tensor_count")?,
            element_bytes: wu::get_u64(map, "element_bytes")?,
        })
    }
}

/// The canonical training artifact record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingArtifact {
    pub name: String,
    pub context_root: Option<Hash32>,
    pub checkpoint: Option<TrainingCheckpointAnchor>,
    pub tensors: Vec<TrainingTensor>,
    pub causal: Option<CausalClosure>,
    pub provenance: Vec<Hash32>,
    pub policy: TrainingPolicy,
}

impl TrainingArtifact {
    pub const KEYS: &'static [&'static str] = &[
        "schema",
        "name",
        "context_root",
        "checkpoint",
        "tensors",
        "causal",
        "provenance",
        "policy",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(TRAINING_ARTIFACT_SCHEMA.to_owned())),
            ("name", Value::Str(self.name.clone())),
            ("context_root", wu::opt_hash_value(self.context_root)),
            (
                "checkpoint",
                match &self.checkpoint {
                    Some(anchor) => anchor.to_value(),
                    None => Value::Null,
                },
            ),
            (
                "tensors",
                Value::Array(self.tensors.iter().map(TrainingTensor::to_value).collect()),
            ),
            (
                "causal",
                match &self.causal {
                    Some(closure) => {
                        causal_closure::causal_record_value(&closure.parents, &closure.closure)
                    }
                    None => Value::Null,
                },
            ),
            (
                "provenance",
                Value::Array(
                    self.provenance
                        .iter()
                        .map(|hash| wu::hash_value(*hash))
                        .collect(),
                ),
            ),
            ("policy", self.policy.to_value()),
        ])
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    /// `TrainingArtifactRoot = H("weft/training-artifact/v1", WCB(record))`.
    pub fn root(&self) -> Result<Hash32> {
        Ok(identity::h(
            TRAINING_ARTIFACT_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }

    /// Strict decode: structure, every rule, canonical re-encoding.
    pub fn decode(bytes: &[u8]) -> Result<TrainingArtifact> {
        let value = wu::decode(bytes)?;
        let artifact = TrainingArtifact::from_value(&value)?;
        if artifact.canonical_bytes()? != bytes {
            return Err(Error::non_canonical(
                "training artifact bytes are not the canonical encoding of its fields",
            ));
        }
        Ok(artifact)
    }

    pub fn from_value(value: &Value) -> Result<TrainingArtifact> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        for key in Self::KEYS {
            wu::field(map, key)?;
        }
        let schema = wu::get_text(map, "schema")?;
        if schema != TRAINING_ARTIFACT_SCHEMA {
            return Err(Error::unknown_schema(format!(
                "training artifact schema `{schema}` is not `{TRAINING_ARTIFACT_SCHEMA}`"
            )));
        }
        let name = wu::get_text(map, "name")?;
        validate_name(&name)?;
        let context_root = wu::get_opt_hash(map, "context_root")?;
        let checkpoint = match wu::field(map, "checkpoint")? {
            Value::Null => None,
            value => Some(TrainingCheckpointAnchor::from_value(value)?),
        };
        let mut tensors = Vec::new();
        for item in wu::get_array(map, "tensors")? {
            tensors.push(TrainingTensor::from_value(item)?);
        }
        let causal = match wu::field(map, "causal")? {
            Value::Null => None,
            value => {
                let causal_map = wu::as_map(value)?;
                wu::check_keys(causal_map, &["schema", "parents", "closure"])?;
                let schema = wu::get_text(causal_map, "schema")?;
                if schema != causal_closure::CAUSAL_SCHEMA {
                    return Err(Error::unknown_schema(
                        "artifact causal record is not `weft.causal.v1`",
                    ));
                }
                let parents = hash_array(causal_map, "parents")?;
                let closure = hash_array(causal_map, "closure")?;
                causal_closure::validate_causal_arrays(
                    &parents,
                    &closure,
                    usize::try_from(Limits::default().max_causal_entries).unwrap_or(usize::MAX),
                )?;
                Some(CausalClosure::new(parents, closure)?)
            }
        };
        let mut provenance = hash_array(map, "provenance")?;
        let mut previous: Option<Hash32> = None;
        for id in &provenance {
            if *id == Hash32::ZERO {
                return Err(Error::non_canonical("provenance id must not be zero"));
            }
            if let Some(previous) = previous {
                if *id <= previous {
                    return Err(Error::non_canonical(
                        "provenance ids must be strictly ascending",
                    ));
                }
            }
            previous = Some(*id);
        }
        provenance.shrink_to_fit();
        let policy = TrainingPolicy::from_value(wu::field(map, "policy")?)?;
        let artifact = TrainingArtifact {
            name,
            context_root,
            checkpoint,
            tensors,
            causal,
            provenance,
            policy,
        };
        artifact.validate_structure()?;
        Ok(artifact)
    }

    /// Structural rules that do not need a store.
    pub fn validate_structure(&self) -> Result<()> {
        self.policy.validate()?;
        validate_name(&self.name)?;
        if self.tensors.is_empty() {
            return Err(Error::malformed(
                "a training artifact must declare at least one tensor",
            ));
        }
        if self.tensors.len() > MAX_ARTIFACT_TENSORS {
            return Err(Error::limit_exceeded(format!(
                "artifact declares more than {MAX_ARTIFACT_TENSORS} tensors"
            )));
        }
        let mut previous: Option<&str> = None;
        let mut shard_total = 0usize;
        for tensor in &self.tensors {
            if tensor.name.is_empty() {
                return Err(Error::malformed("tensor name must not be empty"));
            }
            if let Some(previous) = previous {
                if tensor.name.as_str() <= previous {
                    return Err(Error::non_canonical(
                        "artifact tensors must be sorted by name and unique",
                    ));
                }
            }
            previous = Some(tensor.name.as_str());
            validate_tensor(tensor)?;
            shard_total = shard_total.saturating_add(tensor.shards.len());
        }
        if shard_total > MAX_ARTIFACT_SHARDS {
            return Err(Error::limit_exceeded(format!(
                "artifact declares more than {MAX_ARTIFACT_SHARDS} shards"
            )));
        }
        if let Some(anchor) = &self.checkpoint {
            if anchor.name.is_empty() {
                return Err(Error::malformed("checkpoint name must not be empty"));
            }
            if anchor.tensor_count != self.tensors.len() as u64 {
                return Err(Error::corrupt(
                    "checkpoint tensor_count disagrees with the declared tensors",
                ));
            }
        }
        if self.policy.lineage != LineageMode::None && self.causal.is_none() {
            return Err(Error::malformed("a lineage claim requires a causal record"));
        }
        Ok(())
    }

    pub fn total_element_bytes(&self) -> u64 {
        self.tensors.iter().map(|tensor| tensor.element_bytes).sum()
    }

    pub fn shard_count(&self) -> usize {
        self.tensors.iter().map(|tensor| tensor.shards.len()).sum()
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_ARTIFACT_NAME {
        return Err(Error::invalid_name(format!(
            "artifact name must be 1..={MAX_ARTIFACT_NAME} bytes"
        )));
    }
    if !name.bytes().all(|byte| (0x20..0x7f).contains(&byte)) {
        return Err(Error::invalid_name(
            "artifact name must be printable ASCII without NUL",
        ));
    }
    Ok(())
}

fn validate_tensor(tensor: &TrainingTensor) -> Result<()> {
    let dtype = dtype_from_name(&tensor.dtype)
        .map_err(|err| Error::malformed(format!("tensor `{}`: {err}", tensor.name)))?;
    if tensor.shape.is_empty() || tensor.shape.iter().any(|dim| *dim == 0) {
        return Err(Error::malformed(format!(
            "tensor `{}` shape must be non-empty with nonzero dimensions",
            tensor.name
        )));
    }
    let elements = element_count(&tensor.shape)
        .map_err(|err| Error::limit_exceeded(format!("tensor `{}` shape: {err}", tensor.name)))?;
    let expected = elements
        .checked_mul(dtype.element_size())
        .ok_or_else(|| Error::limit_exceeded("tensor element bytes overflow u64"))?;
    if expected != tensor.element_bytes {
        return Err(Error::corrupt(format!(
            "tensor `{}` records {} element bytes but its dtype/shape imply {expected}",
            tensor.name, tensor.element_bytes
        )));
    }
    if tensor.shards.is_empty() {
        return Err(Error::malformed(format!(
            "tensor `{}` must declare at least one shard",
            tensor.name
        )));
    }
    let first = &tensor.shards[0];
    if first.axis as usize >= tensor.shape.len() {
        return Err(Error::non_canonical(format!(
            "tensor `{}` shard axis is out of range",
            tensor.name
        )));
    }
    if first.shards == 0 || tensor.shards.len() != first.shards as usize {
        return Err(Error::non_canonical(format!(
            "tensor `{}` shard count disagrees with its shard list",
            tensor.name
        )));
    }
    let axis_len = tensor.shape[first.axis as usize];
    if axis_len % u64::from(first.shards) != 0 {
        return Err(Error::non_canonical(format!(
            "tensor `{}` axis length is not divisible by its shard count",
            tensor.name
        )));
    }
    let shard_axis_len = axis_len / u64::from(first.shards);
    let mut shard_total = 0u64;
    for (position, shard) in tensor.shards.iter().enumerate() {
        if shard.index as usize != position {
            return Err(Error::non_canonical(format!(
                "tensor `{}` shards are not ordered by index",
                tensor.name
            )));
        }
        if shard.axis != first.axis || shard.shards != first.shards || shard.dtype != tensor.dtype {
            return Err(Error::non_canonical(format!(
                "tensor `{}` shard {position} disagrees on axis, count or dtype",
                tensor.name
            )));
        }
        for (dim, expected) in tensor.shape.iter().enumerate() {
            let want = if dim == shard.axis as usize {
                shard_axis_len
            } else {
                *expected
            };
            if shard.shape.get(dim) != Some(&want) {
                return Err(Error::non_canonical(format!(
                    "tensor `{}` shard {position} has the wrong shape",
                    tensor.name
                )));
            }
        }
        if shard.shape.len() != tensor.shape.len() {
            return Err(Error::non_canonical(format!(
                "tensor `{}` shard {position} has the wrong rank",
                tensor.name
            )));
        }
        let elements = element_count(&shard.shape)?;
        let expected = elements
            .checked_mul(dtype.element_size())
            .ok_or_else(|| Error::limit_exceeded("shard element bytes overflow u64"))?;
        if expected != shard.byte_len {
            return Err(Error::corrupt(format!(
                "tensor `{}` shard {position} records {} bytes but its shape implies {expected}",
                tensor.name, shard.byte_len
            )));
        }
        shard_total = shard_total
            .checked_add(shard.byte_len)
            .ok_or_else(|| Error::limit_exceeded("shard byte total overflows u64"))?;
    }
    if shard_total != tensor.element_bytes {
        return Err(Error::corrupt(format!(
            "tensor `{}` shard byte total {shard_total} disagrees with element_bytes {}",
            tensor.name, tensor.element_bytes
        )));
    }
    Ok(())
}

fn element_count(shape: &[u64]) -> Result<u64> {
    shape.iter().try_fold(1u64, |acc, dim| {
        acc.checked_mul(*dim)
            .ok_or_else(|| Error::limit_exceeded("tensor dimension product overflows u64"))
    })
}

fn u32_from_u64(value: u64, what: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::malformed(format!("{what} does not fit u32")))
}

fn get_u64_array(map: &BTreeMap<String, Value>, key: &str) -> Result<Vec<u64>> {
    let items = wu::get_array(map, key)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::U64(value) => out.push(*value),
            _ => return Err(Error::malformed(format!("{key} entries must be u64"))),
        }
    }
    Ok(out)
}

fn hash_array(map: &BTreeMap<String, Value>, key: &str) -> Result<Vec<Hash32>> {
    let items = wu::get_array(map, key)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::Bytes(bytes) => out.push(Hash32::from_slice(bytes).map_err(Error::from)?),
            _ => return Err(Error::malformed(format!("{key} entries must be bytes32"))),
        }
    }
    Ok(out)
}

/// What to build.
#[derive(Clone, Debug)]
pub struct TrainingArtifactSpec {
    pub name: String,
    /// A `weft.checkpoint.v1` checkpoint name, when the artifact is
    /// checkpoint-backed.
    pub checkpoint: Option<String>,
    /// `(tensor name, object id)` members, when the artifact is a tensor set.
    pub tensors: Vec<(String, Hash32)>,
    pub context_root: Option<String>,
    pub provenance: Vec<Hash32>,
    pub causal_entries: Vec<Hash32>,
    pub policy: TrainingPolicy,
}

impl TrainingArtifactSpec {
    pub fn checkpoint(name: impl Into<String>) -> TrainingArtifactSpec {
        TrainingArtifactSpec {
            name: name.into(),
            checkpoint: None,
            tensors: Vec::new(),
            context_root: None,
            provenance: Vec::new(),
            causal_entries: Vec::new(),
            policy: TrainingPolicy::default(),
        }
    }
}

/// Path of the artifact record in the store namespace.
pub fn artifact_store_path(root: Hash32) -> String {
    format!("{TRAINING_ARTIFACT_DIR}/{}", root.to_hex())
}

/// Path of one grant record.
pub fn grant_store_path(root: Hash32, grant: Hash32) -> String {
    format!("{TRAINING_GRANT_DIR}/{}/{}", root.to_hex(), grant.to_hex())
}

/// Path of the grant epoch file.
pub fn grant_epoch_store_path(root: Hash32) -> String {
    format!(
        "{TRAINING_GRANT_DIR}/{}/{TRAINING_GRANT_EPOCH}",
        root.to_hex()
    )
}

/// One mutable grant record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingGrant {
    pub artifact_root: Hash32,
    pub uid: u64,
    pub gid: u64,
    pub actions: Vec<String>,
    pub policy_epoch: u64,
    pub issued_at_ns: i64,
    pub expires_at_ns: Option<i64>,
    pub supersedes: Option<Hash32>,
    pub signature: Option<[u8; 64]>,
}

impl TrainingGrant {
    pub const KEYS: &'static [&'static str] = &[
        "schema",
        "artifact_root",
        "principal",
        "actions",
        "policy_epoch",
        "issued_at_ns",
        "expires_at_ns",
        "supersedes",
        "signature",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(TRAINING_GRANT_SCHEMA.to_owned())),
            ("artifact_root", wu::hash_value(self.artifact_root)),
            (
                "principal",
                wu::map([("uid", Value::U64(self.uid)), ("gid", Value::U64(self.gid))]),
            ),
            (
                "actions",
                Value::Array(
                    self.actions
                        .iter()
                        .map(|action| Value::Str(action.clone()))
                        .collect(),
                ),
            ),
            ("policy_epoch", Value::U64(self.policy_epoch)),
            ("issued_at_ns", Value::Timestamp(self.issued_at_ns)),
            (
                "expires_at_ns",
                match self.expires_at_ns {
                    Some(value) => Value::Timestamp(value),
                    None => Value::Null,
                },
            ),
            ("supersedes", wu::opt_hash_value(self.supersedes)),
            (
                "signature",
                match self.signature {
                    Some(bytes) => Value::Bytes(bytes.to_vec()),
                    None => Value::Null,
                },
            ),
        ])
    }

    /// Bytes a signature covers: the record without `signature`.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut map = match self.to_value() {
            Value::Map(map) => map,
            _ => return Err(Error::malformed("grant is not a map")),
        };
        map.insert("signature".to_owned(), Value::Null);
        wu::encode(&Value::Map(map))
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    /// `GrantId = H("weft/training-grant/v1", WCB(record))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(TRAINING_GRANT_DOMAIN, &self.canonical_bytes()?))
    }

    pub fn decode(bytes: &[u8]) -> Result<TrainingGrant> {
        let value = wu::decode(bytes)?;
        let grant = TrainingGrant::from_value(&value)?;
        if grant.canonical_bytes()? != bytes {
            return Err(Error::non_canonical(
                "training grant bytes are not the canonical encoding of its fields",
            ));
        }
        Ok(grant)
    }

    pub fn from_value(value: &Value) -> Result<TrainingGrant> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != TRAINING_GRANT_SCHEMA {
            return Err(Error::unknown_schema(format!(
                "training grant schema `{schema}` is not `{TRAINING_GRANT_SCHEMA}`"
            )));
        }
        let principal = wu::get_map(map, "principal")?;
        wu::check_keys(&principal, &["uid", "gid"])?;
        let mut actions = Vec::new();
        for item in wu::get_array(map, "actions")? {
            match item {
                Value::Str(text) => actions.push(text.clone()),
                _ => return Err(Error::malformed("grant actions must be text")),
            }
        }
        let signature = match wu::field(map, "signature")? {
            Value::Null => None,
            Value::Bytes(bytes) => {
                let bytes: [u8; 64] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::malformed("grant signature must be 64 bytes"))?;
                Some(bytes)
            }
            _ => return Err(Error::malformed("grant signature must be bytes or null")),
        };
        let grant = TrainingGrant {
            artifact_root: wu::get_hash(map, "artifact_root")?,
            uid: wu::get_u64(&principal, "uid")?,
            gid: wu::get_u64(&principal, "gid")?,
            actions,
            policy_epoch: wu::get_u64(map, "policy_epoch")?,
            issued_at_ns: wu::get_timestamp(map, "issued_at_ns")?,
            expires_at_ns: match wu::field(map, "expires_at_ns")? {
                Value::Null => None,
                Value::Timestamp(value) => Some(*value),
                _ => {
                    return Err(Error::malformed(
                        "expires_at_ns must be a timestamp or null",
                    ))
                }
            },
            supersedes: wu::get_opt_hash(map, "supersedes")?,
            signature,
        };
        grant.validate()?;
        Ok(grant)
    }

    pub fn validate(&self) -> Result<()> {
        if self.artifact_root == Hash32::ZERO {
            return Err(Error::non_canonical("grant names the zero artifact root"));
        }
        let mut previous: Option<&str> = None;
        for action in &self.actions {
            if !GRANT_ACTIONS.contains(&action.as_str()) {
                return Err(Error::malformed(format!(
                    "grant action `{action}` is not one of {GRANT_ACTIONS:?}"
                )));
            }
            if let Some(previous) = previous {
                if action.as_str() <= previous {
                    return Err(Error::non_canonical(
                        "grant actions must be sorted and unique",
                    ));
                }
            }
            previous = Some(action.as_str());
        }
        if let Some(expires) = self.expires_at_ns {
            if expires <= self.issued_at_ns {
                return Err(Error::malformed(
                    "grant expiry must be after its issue time",
                ));
            }
        }
        Ok(())
    }

    pub fn authorizes(&self, uid: u64, gid: u64, action: &str, now_ns: i64) -> bool {
        self.uid == uid
            && self.gid == gid
            && self.actions.iter().any(|candidate| candidate == action)
            && self
                .expires_at_ns
                .map(|expires| now_ns < expires)
                .unwrap_or(true)
    }
}

/// A validation summary. It is evidence, not identity.
#[derive(Clone, Debug)]
pub struct TrainingArtifactValidation {
    pub artifact_root: Hash32,
    pub snapshot: Hash32,
    pub name: String,
    pub tensor_count: usize,
    pub shard_count: usize,
    pub total_element_bytes: u64,
    pub tensors_exact: usize,
    pub checkpoint_verified: bool,
    pub context_root_verified: bool,
    pub manifest_root: Option<Hash32>,
    pub manifest_content: Option<Hash32>,
    pub lineage_mode: String,
    pub causal_nodes: usize,
    pub causal_closure: Vec<Hash32>,
    pub causal_max_depth: u32,
    pub provenance_verified: usize,
    pub policy_ok: bool,
    pub grant_epoch: u64,
    pub grants: usize,
    pub artifact_bytes: usize,
}

/// Options for validation. `visible` is a capability filter: an identity that
/// is not visible produces the same refusal as an absent one.
pub struct ValidationOptions {
    pub closure_mode: ClosureMode,
    pub closure_bounds: ClosureBounds,
    pub check_grants: bool,
    pub now_ns: i64,
}

impl Default for ValidationOptions {
    fn default() -> Self {
        ValidationOptions {
            closure_mode: ClosureMode::Complete,
            closure_bounds: ClosureBounds::default(),
            check_grants: false,
            now_ns: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Store surface
// ---------------------------------------------------------------------------

impl Store {
    /// Build a training artifact from immutable state at `snapshot`.
    pub fn build_training_artifact(
        &self,
        snapshot: Hash32,
        spec: &TrainingArtifactSpec,
    ) -> Result<(TrainingArtifact, Hash32)> {
        let state = self.state_at(snapshot)?;
        let limits = Limits::default();
        let mut tensors: Vec<TrainingTensor> = Vec::new();
        let mut checkpoint: Option<TrainingCheckpointAnchor> = None;

        if let Some(name) = &spec.checkpoint {
            let loaded = self.load_checkpoint_manifest_at(snapshot, name)?;
            let manifest = &loaded.manifest;
            for tensor in &manifest.tensors {
                let mut shards = Vec::with_capacity(tensor.shards.len());
                for shard in &tensor.shards {
                    shards.push(TrainingShard {
                        axis: shard.axis,
                        shards: shard.shards,
                        index: shard.index,
                        object: shard.object_id,
                        content_root: shard.content_root,
                        logical_root: shard.logical_root,
                        dtype: tensor.dtype.clone(),
                        shape: shard_shape(&tensor.shape, shard.axis, shard.shards, shard.index)?,
                        byte_len: shard.byte_len,
                    });
                }
                tensors.push(TrainingTensor {
                    name: tensor.name.clone(),
                    dtype: tensor.dtype.clone(),
                    shape: tensor.shape.clone(),
                    logical_root: tensor.logical_root,
                    content_root: tensor.content_root,
                    element_bytes: tensor.element_bytes,
                    source_offset: tensor.source_offset,
                    shards,
                });
            }
            let manifest_bytes = self.read_object_at(Some(snapshot), loaded.manifest_id)?;
            checkpoint = Some(TrainingCheckpointAnchor {
                name: manifest.name.clone(),
                manifest_object: loaded.manifest_id,
                manifest_content: identity::content_hash(&manifest_bytes),
                manifest_root: loaded.manifest_root,
                complete: manifest.complete,
                source_sha256: manifest.source_sha256.map(Hash32::from_bytes),
                tensor_count: manifest.tensors.len() as u64,
                element_bytes: manifest.element_bytes,
            });
        } else {
            for (name, object_id) in &spec.tensors {
                let record = state
                    .objects
                    .get(object_id)
                    .ok_or_else(|| Error::not_found("tensor object is absent at the snapshot"))?;
                let metadata = self.tensor_metadata_at(&state, *object_id)?;
                let bytes = self.read_object_at(Some(snapshot), *object_id)?;
                let content_root = identity::content_hash(&bytes);
                let expected = element_count(&metadata.shape)?
                    .checked_mul(dtype_from_name(metadata.dtype.as_str())?.element_size())
                    .ok_or_else(|| Error::limit_exceeded("tensor bytes overflow u64"))?;
                if expected != bytes.len() as u64 {
                    return Err(Error::corrupt(format!(
                        "tensor object {} decodes to {} bytes, expected {expected}",
                        object_id.to_hex(),
                        bytes.len()
                    )));
                }
                let root = logical_root(
                    dtype_from_name(metadata.dtype.as_str())?,
                    &metadata.shape,
                    &bytes,
                    &limits,
                )?;
                if root != metadata.logical_root {
                    return Err(Error::corrupt(format!(
                        "tensor object {} recomputes to a different logical root",
                        object_id.to_hex()
                    )));
                }
                tensors.push(TrainingTensor {
                    name: name.clone(),
                    dtype: metadata.dtype.as_str().to_owned(),
                    shape: metadata.shape.clone(),
                    logical_root: root,
                    content_root,
                    element_bytes: bytes.len() as u64,
                    source_offset: 0,
                    shards: vec![TrainingShard {
                        axis: 0,
                        shards: 1,
                        index: 0,
                        object: *object_id,
                        content_root,
                        logical_root: root,
                        dtype: metadata.dtype.as_str().to_owned(),
                        shape: metadata.shape.clone(),
                        byte_len: bytes.len() as u64,
                    }],
                });
            }
        }
        tensors.sort_by(|a, b| a.name.cmp(&b.name));
        for pair in tensors.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(Error::invalid_argument(format!(
                    "duplicate tensor name `{}` in the artifact",
                    pair[0].name
                )));
            }
        }

        let context_root = match &spec.context_root {
            Some(name) => {
                let stored = self.load_context_root_at(&state, name)?;
                Some(stored.context_root_id)
            }
            None => None,
        };

        let causal = if spec.causal_entries.is_empty() {
            None
        } else {
            Some(crate::context::compute_causal_closure(
                &state,
                &spec.causal_entries,
                &limits,
            )?)
        };

        let mut provenance = spec.provenance.clone();
        provenance.sort_unstable();
        provenance.dedup();
        for id in &provenance {
            if *id == Hash32::ZERO {
                return Err(Error::invalid_argument("provenance id must not be zero"));
            }
            self.read_provenance(*id).map_err(|err| {
                if err.class() == Class::NotFound {
                    Error::dependency_mismatch(format!(
                        "provenance receipt {} is not present at the snapshot",
                        id.to_hex()
                    ))
                } else {
                    err
                }
            })?;
        }

        let artifact = TrainingArtifact {
            name: spec.name.clone(),
            context_root,
            checkpoint,
            tensors,
            causal,
            provenance,
            policy: spec.policy.clone(),
        };
        artifact.validate_structure()?;
        let root = artifact.root()?;
        Ok((artifact, root))
    }

    /// Publish an artifact record. Idempotent for identical bytes; a different
    /// record at the same root is `Conflict`.
    pub fn write_training_artifact(&mut self, artifact: &TrainingArtifact) -> Result<Hash32> {
        let root = artifact.root()?;
        let bytes = artifact.canonical_bytes()?;
        let dir = ensure_dir_path(self, TRAINING_ARTIFACT_DIR)?;
        let name = root.to_hex();
        match self.lookup(dir, name.as_bytes()) {
            Ok(existing) => {
                let existing_bytes = self.read_object(existing)?;
                if existing_bytes == bytes {
                    Ok(root)
                } else {
                    Err(Error::conflict(format!(
                        "artifact record {name} already exists with different bytes"
                    )))
                }
            }
            Err(err) if err.class() == Class::NotFound => {
                let mut attrs = BTreeMap::new();
                attrs.insert(
                    "weft.training-artifact.schema".to_owned(),
                    Value::Str(TRAINING_ARTIFACT_SCHEMA.to_owned()),
                );
                self.create_file(dir, name.as_bytes(), &bytes, 0o644, 0, 0, attrs)?;
                Ok(root)
            }
            Err(err) => Err(err),
        }
    }

    /// Read an artifact record at a pinned snapshot and return it with its
    /// recomputed root.
    pub fn load_training_artifact_at(
        &self,
        state: &State,
        snapshot: Option<Hash32>,
        root: Hash32,
    ) -> Result<(TrainingArtifact, usize)> {
        let dir = resolve_dir_at(self, state, TRAINING_ARTIFACT_DIR)?;
        let name = root.to_hex();
        let object = state.lookup(dir, name.as_bytes()).map_err(|err| {
            if err.class() == Class::NotFound {
                Error::not_found(format!("no training artifact {name} at this snapshot"))
            } else {
                err
            }
        })?;
        let bytes = match snapshot {
            Some(snapshot) => self.read_object_at(Some(snapshot), object)?,
            None => self.read_object(object)?,
        };
        let artifact = TrainingArtifact::decode(&bytes)?;
        let observed = artifact.root()?;
        if observed != root {
            return Err(Error::corrupt(format!(
                "training artifact record at {name} recomputes to {}",
                observed.to_hex()
            )));
        }
        Ok((artifact, bytes.len()))
    }

    /// Validate an artifact end to end at a pinned snapshot.
    pub fn validate_training_artifact_at(
        &self,
        snapshot: Hash32,
        root: Hash32,
        options: &ValidationOptions,
    ) -> Result<TrainingArtifactValidation> {
        let state = self.state_at(snapshot)?;
        let (artifact, artifact_bytes) =
            self.load_training_artifact_at(&state, Some(snapshot), root)?;
        let limits = Limits::default();
        let mut tensors_exact = 0usize;

        // 2. Checkpoint anchor: content hash, manifest hash, strict decode and
        //    full equality with the declaration.
        let mut checkpoint_verified = false;
        if let Some(anchor) = &artifact.checkpoint {
            let bytes = self
                .read_object_at(Some(snapshot), anchor.manifest_object)
                .map_err(|err| match err.class() {
                    Class::NotFound => Error::dependency_mismatch(format!(
                        "checkpoint manifest object {} is absent at the snapshot",
                        anchor.manifest_object.to_hex()
                    )),
                    _ => err,
                })?;
            let content = identity::content_hash(&bytes);
            if content != anchor.manifest_content {
                return Err(Error::corrupt(format!(
                    "checkpoint manifest object {} hashes to {} not {}",
                    anchor.manifest_object.to_hex(),
                    content.to_hex(),
                    anchor.manifest_content.to_hex()
                )));
            }
            let observed_root = identity::h(crate::checkpoint::CHECKPOINT_MANIFEST_DOMAIN, &bytes);
            if observed_root != anchor.manifest_root {
                return Err(Error::corrupt(format!(
                    "checkpoint manifest root is {} not {}",
                    observed_root.to_hex(),
                    anchor.manifest_root.to_hex()
                )));
            }
            let manifest = CheckpointManifest::decode(&bytes)?;
            compare_manifest(anchor, &artifact.tensors, &manifest)?;
            checkpoint_verified = true;
        }

        // 3. Exact tensor values.
        for tensor in &artifact.tensors {
            let mut assembled = vec![
                0u8;
                usize::try_from(tensor.element_bytes).map_err(|_| {
                    Error::limit_exceeded("tensor is too large for this host")
                })?
            ];
            let dtype = dtype_from_name(&tensor.dtype)?;
            for shard in &tensor.shards {
                let bytes = self
                    .read_object_at(Some(snapshot), shard.object)
                    .map_err(|err| match err.class() {
                        Class::NotFound => Error::dependency_mismatch(format!(
                            "tensor `{}` shard {} object {} is absent at the snapshot",
                            tensor.name,
                            shard.index,
                            shard.object.to_hex()
                        )),
                        _ => err,
                    })?;
                let content = identity::content_hash(&bytes);
                if content != shard.content_root {
                    return Err(Error::corrupt(format!(
                        "tensor `{}` shard {} value hashes to {} not {}",
                        tensor.name,
                        shard.index,
                        content.to_hex(),
                        shard.content_root.to_hex()
                    )));
                }
                let shard_dtype = dtype_from_name(&shard.dtype)?;
                let observed = logical_root(shard_dtype, &shard.shape, &bytes, &limits)?;
                if observed != shard.logical_root {
                    return Err(Error::corrupt(format!(
                        "tensor `{}` shard {} logical root is {} not {}",
                        tensor.name,
                        shard.index,
                        observed.to_hex(),
                        shard.logical_root.to_hex()
                    )));
                }
                scatter_shard(
                    &mut assembled,
                    &bytes,
                    &tensor.shape,
                    &shard.shape,
                    shard.axis,
                    shard.index,
                    shard.shards,
                    dtype.element_size(),
                )?;
            }
            let content = identity::content_hash(&assembled);
            if content != tensor.content_root {
                return Err(Error::corrupt(format!(
                    "tensor `{}` assembled value hashes to {} not {}",
                    tensor.name,
                    content.to_hex(),
                    tensor.content_root.to_hex()
                )));
            }
            let observed = logical_root(dtype, &tensor.shape, &assembled, &limits)?;
            if observed != tensor.logical_root {
                return Err(Error::corrupt(format!(
                    "tensor `{}` assembled logical root is {} not {}",
                    tensor.name,
                    observed.to_hex(),
                    tensor.logical_root.to_hex()
                )));
            }
            tensors_exact += 1;
        }

        // 4. Declared lineage. Build the graph once and compute the requested
        //    closure from the artifact's own parents.
        let mut causal_nodes = 0usize;
        let mut causal_closure = Vec::new();
        let mut causal_max_depth = 0u32;
        if let Some(closure) = &artifact.causal {
            let graph = CausalGraph::build(&state, &limits)?;
            let result = graph.compute(
                options.closure_mode,
                &closure.parents,
                options.closure_bounds,
                &|_identity| true,
            )?;
            causal_nodes = result.nodes.len();
            causal_closure = result.identities();
            causal_max_depth = result.max_depth_seen;
            if artifact.policy.lineage == LineageMode::Complete {
                // Every declared closure entry must resolve, not only the ones
                // the parent edges reach.
                for entry in &closure.closure {
                    if graph.node(*entry).is_none() {
                        return Err(causal_closure::missing_ancestor(*entry, None));
                    }
                }
                let declared: BTreeSet<Hash32> = closure.closure.iter().copied().collect();
                let computed: BTreeSet<Hash32> = causal_closure.iter().copied().collect();
                if !declared.is_subset(&computed) {
                    return Err(Error::dependency_mismatch(format!(
                        "declared causal closure names {} ancestor(s) the parent edges do not reach",
                        declared.difference(&computed).count()
                    )));
                }
            }
        }

        // 5. Provenance receipts and the declared lineage policy.
        let mut provenance_verified = 0usize;
        let declared_provenance: BTreeSet<Hash32> = artifact.provenance.iter().copied().collect();
        for id in &artifact.provenance {
            let record = self.read_provenance(*id).map_err(|err| {
                if err.class() == Class::NotFound {
                    Error::dependency_mismatch(format!(
                        "provenance receipt {} is absent at the snapshot",
                        id.to_hex()
                    ))
                } else {
                    err
                }
            })?;
            let recomputed = record.id()?;
            if recomputed != *id {
                return Err(Error::corrupt(format!(
                    "provenance receipt {} recomputes to {}",
                    id.to_hex(),
                    recomputed.to_hex()
                )));
            }
            provenance_verified += 1;
        }
        // A member value is covered when a `provenance` relation on its shard
        // object names one of the artifact's declared receipts. Relations are
        // store state, so the check is snapshot-scoped.
        let mut covered_objects: BTreeMap<Hash32, BTreeSet<Hash32>> = BTreeMap::new();
        for relation in state.relations.values() {
            if relation.predicate != crate::provenance::PROVENANCE_PREDICATE {
                continue;
            }
            if let crate::state::RelationTarget::Value(Value::Str(text)) = &relation.target {
                if let Ok(id) = Hash32::from_hex(text) {
                    covered_objects
                        .entry(relation.subject)
                        .or_default()
                        .insert(id);
                }
            }
        }
        let mut policy_ok = true;
        if artifact.tensors.len() < artifact.policy.minimum_tensors as usize {
            return Err(Error::corrupt(format!(
                "policy minimum_tensors is {} but the artifact has {}",
                artifact.policy.minimum_tensors,
                artifact.tensors.len()
            )));
        }
        if !artifact.policy.allowed_dtypes.is_empty() {
            for tensor in &artifact.tensors {
                if !artifact
                    .policy
                    .allowed_dtypes
                    .iter()
                    .any(|dtype| *dtype == tensor.dtype)
                {
                    return Err(Error::corrupt(format!(
                        "policy allowed_dtypes does not permit `{}` for tensor `{}`",
                        tensor.dtype, tensor.name
                    )));
                }
            }
        }
        if artifact.policy.checkpoint_complete {
            match &artifact.checkpoint {
                Some(anchor) if anchor.complete => {}
                Some(_) => {
                    return Err(Error::corrupt(
                        "policy checkpoint_complete is contradicted by an incomplete checkpoint",
                    ))
                }
                None => {
                    return Err(Error::corrupt(
                        "policy checkpoint_complete requires a checkpoint anchor",
                    ))
                }
            }
        }
        if artifact.policy.lineage != LineageMode::None {
            for tensor in &artifact.tensors {
                for shard in &tensor.shards {
                    let covered = covered_objects
                        .get(&shard.object)
                        .map(|ids| ids.iter().any(|id| declared_provenance.contains(id)))
                        .unwrap_or(false);
                    if !covered {
                        return Err(Error::corrupt(format!(
                            "policy lineage `{}` is contradicted: tensor `{}` shard {} names no declared provenance receipt",
                            artifact.policy.lineage.as_str(),
                            tensor.name,
                            shard.index
                        )));
                    }
                }
            }
        }
        let mut context_root_verified = false;
        if let Some(context_root) = &artifact.context_root {
            if *context_root == Hash32::ZERO {
                return Err(Error::corrupt("artifact context_root is the zero hash"));
            }
            let stored = self.list_context_roots_at(&state)?;
            if !stored.iter().any(|root| root.context_root_id == *context_root) {
                return Err(Error::dependency_mismatch(format!(
                    "artifact context root {} is not present at the snapshot",
                    context_root.to_hex()
                )));
            }
            context_root_verified = true;
        }
        let _ = &mut policy_ok;

        // 6. Mutable authority is reported, never folded into the root.
        let (grant_epoch, grants) = if options.check_grants {
            let (epoch, records) = self.training_grants_at(&state, root)?;
            let mut live = 0usize;
            for grant in &records {
                let _ = grant;
                if grant
                    .expires_at_ns
                    .map(|expires| options.now_ns < expires)
                    .unwrap_or(true)
                {
                    live += 1;
                }
            }
            (epoch, live)
        } else {
            (self.training_grant_epoch_at(&state, root)?, 0)
        };

        Ok(TrainingArtifactValidation {
            artifact_root: root,
            snapshot,
            name: artifact.name.clone(),
            tensor_count: artifact.tensors.len(),
            shard_count: artifact.shard_count(),
            total_element_bytes: artifact.total_element_bytes(),
            tensors_exact,
            checkpoint_verified,
            context_root_verified,
            manifest_root: artifact
                .checkpoint
                .as_ref()
                .map(|anchor| anchor.manifest_root),
            manifest_content: artifact
                .checkpoint
                .as_ref()
                .map(|anchor| anchor.manifest_content),
            lineage_mode: artifact.policy.lineage.as_str().to_owned(),
            causal_nodes,
            causal_closure,
            causal_max_depth,
            provenance_verified,
            policy_ok: {
                policy_ok = true;
                policy_ok
            },
            grant_epoch,
            grants,
            artifact_bytes,
        })
    }

    /// Publish a grant and increment the artifact's grant epoch.
    pub fn set_training_grant(&mut self, grant: &TrainingGrant) -> Result<Hash32> {
        grant.validate()?;
        let id = grant.id()?;
        let dir = ensure_dir_path(
            self,
            &format!("{TRAINING_GRANT_DIR}/{}", grant.artifact_root.to_hex()),
        )?;
        let name = id.to_hex();
        let bytes = grant.canonical_bytes()?;
        match self.lookup(dir, name.as_bytes()) {
            Ok(existing) => {
                if self.read_object(existing)? != bytes {
                    return Err(Error::conflict("grant id already names different bytes"));
                }
            }
            Err(err) if err.class() == Class::NotFound => {
                self.create_file(dir, name.as_bytes(), &bytes, 0o644, 0, 0, BTreeMap::new())?;
            }
            Err(err) => return Err(err),
        }
        self.bump_grant_epoch(grant.artifact_root)?;
        Ok(id)
    }

    fn bump_grant_epoch(&mut self, root: Hash32) -> Result<u64> {
        let dir = ensure_dir_path(self, &format!("{TRAINING_GRANT_DIR}/{}", root.to_hex()))?;
        let current = match self.lookup(dir, TRAINING_GRANT_EPOCH.as_bytes()) {
            Ok(object) => {
                let bytes = self.read_object(object)?;
                std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| text.trim().parse::<u64>().ok())
                    .ok_or_else(|| Error::corrupt("grant epoch is not a decimal integer"))?
            }
            Err(err) if err.class() == Class::NotFound => 0,
            Err(err) => return Err(err),
        };
        let next = current
            .checked_add(1)
            .ok_or_else(|| Error::limit_exceeded("grant epoch overflows u64"))?;
        let text = format!("{next}\n");
        match self.lookup(dir, TRAINING_GRANT_EPOCH.as_bytes()) {
            Ok(object) => {
                self.write_file(object, 0, text.as_bytes())?;
            }
            Err(err) if err.class() == Class::NotFound => {
                self.create_file(
                    dir,
                    TRAINING_GRANT_EPOCH.as_bytes(),
                    text.as_bytes(),
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                )?;
            }
            Err(err) => return Err(err),
        }
        Ok(next)
    }

    /// Current grant epoch at the head.
    pub fn training_grant_epoch(&self, root: Hash32) -> Result<u64> {
        self.training_grant_epoch_at(self.current_state(), root)
    }

    fn training_grant_epoch_at(&self, state: &State, root: Hash32) -> Result<u64> {
        let path = format!("{TRAINING_GRANT_DIR}/{}", root.to_hex());
        let dir = match resolve_dir_at(self, state, &path) {
            Ok(dir) => dir,
            Err(err) if err.class() == Class::NotFound => return Ok(0),
            Err(err) => return Err(err),
        };
        match state.lookup(dir, TRAINING_GRANT_EPOCH.as_bytes()) {
            Ok(object) => {
                let bytes = self.read_object_exempt(object)?;
                std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| text.trim().parse::<u64>().ok())
                    .ok_or_else(|| Error::corrupt("grant epoch is not a decimal integer"))
            }
            Err(err) if err.class() == Class::NotFound => Ok(0),
            Err(err) => Err(err),
        }
    }

    /// All grants for one artifact at a state plus the current epoch.
    pub fn training_grants_at(
        &self,
        state: &State,
        root: Hash32,
    ) -> Result<(u64, Vec<TrainingGrant>)> {
        let epoch = self.training_grant_epoch_at(state, root)?;
        let path = format!("{TRAINING_GRANT_DIR}/{}", root.to_hex());
        let dir = match resolve_dir_at(self, state, &path) {
            Ok(dir) => dir,
            Err(err) if err.class() == Class::NotFound => return Ok((epoch, Vec::new())),
            Err(err) => return Err(err),
        };
        let mut grants = Vec::new();
        for entry in state.list_dir(dir)? {
            let name = match std::str::from_utf8(&entry.name) {
                Ok(name) => name,
                Err(_) => continue,
            };
            if name == TRAINING_GRANT_EPOCH {
                continue;
            }
            let bytes = self.read_object_exempt(entry.target)?;
            let grant = TrainingGrant::decode(&bytes)?;
            if grant.artifact_root != root {
                return Err(Error::corrupt(
                    "stored grant names a different artifact root than its directory",
                ));
            }
            grants.push(grant);
        }
        Ok((epoch, grants))
    }

    /// Current grants at the head.
    pub fn training_grants(&self, root: Hash32) -> Result<(u64, Vec<TrainingGrant>)> {
        self.training_grants_at(self.current_state(), root)
    }

    /// Compute a bounded causal closure for an artifact and return the
    /// evidence bundle a clean-room verifier can re-check.
    pub fn training_artifact_closure(
        &self,
        snapshot: Hash32,
        root: Hash32,
        mode: ClosureMode,
        bounds: ClosureBounds,
    ) -> Result<ClosureEvidence> {
        let state = self.state_at(snapshot)?;
        let (artifact, _) = self.load_training_artifact_at(&state, Some(snapshot), root)?;
        let limits = Limits::default();
        let graph = CausalGraph::build(&state, &limits)?;
        let mut roots: Vec<Hash32> = Vec::new();
        match mode {
            ClosureMode::Minimal => {
                // Only the artifact's own direct ancestry and the member
                // objects it must read. Provenance ids are receipts, not graph
                // nodes, and are checked separately.
                if let Some(causal) = &artifact.causal {
                    roots.extend(causal.parents.iter().copied());
                }
                for tensor in &artifact.tensors {
                    for shard in &tensor.shards {
                        if let Some(object) = state.objects.get(&shard.object) {
                            roots.push(object.version);
                        }
                    }
                }
            }
            ClosureMode::Complete => {
                // The declared ancestry plus every member object; the graph
                // then follows each member's declared closure too.
                if let Some(causal) = &artifact.causal {
                    roots.extend(causal.parents.iter().copied());
                    roots.extend(causal.closure.iter().copied());
                }
                for tensor in &artifact.tensors {
                    for shard in &tensor.shards {
                        if let Some(object) = state.objects.get(&shard.object) {
                            roots.push(object.version);
                        }
                    }
                }
            }
        }
        roots.sort_unstable();
        roots.dedup();
        let outcome = graph.compute(mode, &roots, bounds, &|_identity| true);
        Ok(ClosureEvidence {
            snapshot,
            artifact_root: root,
            mode,
            bounds,
            roots,
            outcome,
            state,
        })
    }
}

/// The evidence for one closure query. `records` is filled from the state so a
/// verifier never has to read the store format.
pub struct ClosureEvidence {
    pub snapshot: Hash32,
    pub artifact_root: Hash32,
    pub mode: ClosureMode,
    pub bounds: ClosureBounds,
    pub roots: Vec<Hash32>,
    pub outcome: Result<ClosureResult>,
    pub state: State,
}

impl ClosureEvidence {
    /// Canonical JSON bundle for `verifier/closure_check.py`.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let mut records = Vec::new();
        if let Ok(result) = &self.outcome {
            for node in &result.nodes {
                let record_bytes = self
                    .state
                    .objects
                    .values()
                    .find(|record| record.version == node.identity)
                    .and_then(|record| causal_record_bytes(&record.attrs))
                    .unwrap_or_default();
                records.push(json!({
                    "identity": node.identity.to_hex(),
                    "object": node.object.map(|id| id.to_hex()),
                    "parents": node.parents.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
                    "closure": node.declared_closure.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
                    "record_bytes": hex_encode(&record_bytes),
                    "causal_id": causal_closure::causal_id(&node.parents, &node.declared_closure)
                        .map(|id| id.to_hex()).ok(),
                    "depth": node.depth,
                }));
            }
        } else {
            // A refusal must still be checkable from canonical records. Emit
            // every causal record in the snapshot; the verifier recomputes the
            // walk and must run into the same unresolvable identity.
            for record in self.state.objects.values() {
                let Some(record_bytes) = causal_record_bytes(&record.attrs) else {
                    continue;
                };
                let (parent_values, closure_values) = match (
                    record.attrs.get(causal_closure::ATTR_PARENTS),
                    record.attrs.get(causal_closure::ATTR_CLOSURE),
                ) {
                    (Some(Value::Array(p)), Some(Value::Array(c))) => (p, c),
                    _ => continue,
                };
                let parents: Vec<Hash32> = parent_values
                    .iter()
                    .filter_map(|item| match item {
                        Value::Bytes(bytes) => Hash32::from_slice(bytes).ok(),
                        _ => None,
                    })
                    .collect();
                let declared: Vec<Hash32> = closure_values
                    .iter()
                    .filter_map(|item| match item {
                        Value::Bytes(bytes) => Hash32::from_slice(bytes).ok(),
                        _ => None,
                    })
                    .collect();
                records.push(json!({
                    "identity": record.version.to_hex(),
                    "object": record.id.to_hex(),
                    "parents": parents.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
                    "closure": declared.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
                    "record_bytes": hex_encode(&record_bytes),
                    "causal_id": causal_closure::causal_id(&parents, &declared)
                        .map(|id| id.to_hex()).ok(),
                    "depth": 0,
                }));
            }
        }
        let (nodes, refusal) = match &self.outcome {
            Ok(result) => (
                serde_json::Value::Array(
                    result
                        .nodes
                        .iter()
                        .map(
                            |node| json!({"identity": node.identity.to_hex(), "depth": node.depth}),
                        )
                        .collect(),
                ),
                serde_json::Value::Null,
            ),
            Err(err) => (
                serde_json::Value::Array(Vec::new()),
                json!({"class": err.class().as_str(), "message": err.to_string()}),
            ),
        };
        json!({
            "schema": "weft.closure.bundle.v1",
            "artifact_root": self.artifact_root.to_hex(),
            "snapshot": self.snapshot.to_hex(),
            "mode": self.mode.as_str(),
            "max_depth": self.bounds.max_depth,
            "max_nodes": self.bounds.max_nodes,
            "roots": self.roots.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
            "records": records,
            "result": {
                "nodes": nodes,
                "total": self.outcome.as_ref().map(|r| r.total_closure_size).unwrap_or(0),
                "returned": self.outcome.as_ref().map(|r| r.returned_closure_size).unwrap_or(0),
                "truncated": self.outcome.as_ref().map(|r| r.truncated).unwrap_or(false),
            },
            "refusal": refusal,
        })
    }
}

fn causal_record_bytes(attrs: &BTreeMap<String, Value>) -> Option<Vec<u8>> {
    // A node with no declared ancestry is still a node: its record is the
    // canonical empty `weft.causal.v1` record, not an absent one. A partial
    // attribute set is left absent; the graph builder already refused it.
    let empty = Vec::new();
    let parents = match attrs.get(causal_closure::ATTR_PARENTS) {
        Some(Value::Array(items)) => items,
        None => {
            if attrs.contains_key(causal_closure::ATTR_CLOSURE) {
                return None;
            }
            &empty
        }
        _ => return None,
    };
    let closure = match attrs.get(causal_closure::ATTR_CLOSURE) {
        Some(Value::Array(items)) => items,
        None => {
            if attrs.contains_key(causal_closure::ATTR_PARENTS) {
                return None;
            }
            &empty
        }
        _ => return None,
    };
    let value = Value::map([
        (
            "schema",
            Value::Str(causal_closure::CAUSAL_SCHEMA.to_owned()),
        ),
        ("parents", Value::Array(parents.clone())),
        ("closure", Value::Array(closure.clone())),
    ]);
    wu::encode(&value).ok()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Numeric shape of one shard on `axis`.
fn shard_shape(shape: &[u64], axis: u32, shards: u32, index: u32) -> Result<Vec<u64>> {
    let axis = axis as usize;
    if axis >= shape.len() || shards == 0 || index >= shards {
        return Err(Error::non_canonical("invalid shard descriptor"));
    }
    if shape[axis] % u64::from(shards) != 0 {
        return Err(Error::non_canonical(
            "tensor axis length is not divisible by the shard count",
        ));
    }
    let mut out = shape.to_vec();
    out[axis] /= u64::from(shards);
    Ok(out)
}

/// Copy one shard's row-major bytes into the assembled tensor.
#[allow(clippy::too_many_arguments)]
fn scatter_shard(
    out: &mut [u8],
    shard_bytes: &[u8],
    tensor_shape: &[u64],
    shard_shape: &[u64],
    axis: u32,
    index: u32,
    shards: u32,
    element_size: u64,
) -> Result<()> {
    if index >= shards {
        return Err(Error::non_canonical("shard index out of range"));
    }
    let axis = axis as usize;
    let outer: u64 = tensor_shape[..axis].iter().product();
    let inner: u64 = tensor_shape[axis + 1..].iter().product();
    let axis_len = tensor_shape[axis];
    let shard_axis_len = shard_shape[axis];
    if shard_axis_len * u64::from(shards) != axis_len {
        return Err(Error::non_canonical(
            "shard shape does not tile the tensor axis",
        ));
    }
    let block_bytes = shard_axis_len
        .checked_mul(inner)
        .and_then(|count| count.checked_mul(element_size))
        .ok_or_else(|| Error::limit_exceeded("shard byte length overflows u64"))?;
    let expected_src = outer
        .checked_mul(block_bytes)
        .ok_or_else(|| Error::limit_exceeded("shard byte length overflows u64"))?;
    if shard_bytes.len() as u64 != expected_src {
        return Err(Error::corrupt(format!(
            "shard {index} decodes to {} bytes, expected {expected_src}",
            shard_bytes.len()
        )));
    }
    let stride = axis_len * inner * element_size;
    for o in 0..outer {
        let dst_start = (o * stride + index as u64 * block_bytes) as usize;
        let src_start = (o * block_bytes) as usize;
        let dst_end = dst_start
            .checked_add(block_bytes as usize)
            .ok_or_else(|| Error::limit_exceeded("shard destination offset overflows"))?;
        let src_end = src_start + block_bytes as usize;
        if dst_end > out.len() || src_end > shard_bytes.len() {
            return Err(Error::corrupt("shard does not fit the assembled tensor"));
        }
        out[dst_start..dst_end].copy_from_slice(&shard_bytes[src_start..src_end]);
    }
    Ok(())
}

/// Compare the checkpoint manifest a record names with the artifact's members.
fn compare_manifest(
    anchor: &TrainingCheckpointAnchor,
    tensors: &[TrainingTensor],
    manifest: &CheckpointManifest,
) -> Result<()> {
    if manifest.name != anchor.name {
        return Err(Error::corrupt(format!(
            "checkpoint manifest names `{}` not `{}`",
            manifest.name, anchor.name
        )));
    }
    if manifest.complete != anchor.complete {
        return Err(Error::corrupt(
            "checkpoint manifest completeness disagrees with the artifact",
        ));
    }
    if manifest.source_sha256.map(Hash32::from_bytes) != anchor.source_sha256 {
        return Err(Error::corrupt(
            "checkpoint manifest source digest disagrees with the artifact",
        ));
    }
    if manifest.element_bytes != anchor.element_bytes {
        return Err(Error::corrupt(
            "checkpoint manifest element bytes disagree with the artifact",
        ));
    }
    if manifest.tensors.len() != tensors.len() {
        return Err(Error::corrupt(format!(
            "checkpoint manifest declares {} tensors but the artifact declares {}",
            manifest.tensors.len(),
            tensors.len()
        )));
    }
    let by_name: BTreeMap<&str, &crate::checkpoint::CheckpointTensor> = manifest
        .tensors
        .iter()
        .map(|tensor| (tensor.name.as_str(), tensor))
        .collect();
    for tensor in tensors {
        let declared = by_name.get(tensor.name.as_str()).ok_or_else(|| {
            Error::corrupt(format!(
                "checkpoint manifest does not declare tensor `{}`",
                tensor.name
            ))
        })?;
        if declared.dtype != tensor.dtype
            || declared.shape != tensor.shape
            || declared.logical_root != tensor.logical_root
            || declared.content_root != tensor.content_root
            || declared.element_bytes != tensor.element_bytes
            || declared.source_offset != tensor.source_offset
            || declared.shards.len() != tensor.shards.len()
        {
            return Err(Error::corrupt(format!(
                "tensor `{}` disagrees with the checkpoint manifest",
                tensor.name
            )));
        }
        for (shard, declared) in tensor.shards.iter().zip(declared.shards.iter()) {
            if shard.axis != declared.axis
                || shard.shards != declared.shards
                || shard.index != declared.index
                || shard.object != declared.object_id
                || shard.content_root != declared.content_root
                || shard.logical_root != declared.logical_root
                || shard.byte_len != declared.byte_len
            {
                return Err(Error::corrupt(format!(
                    "tensor `{}` shard {} disagrees with the checkpoint manifest",
                    tensor.name, shard.index
                )));
            }
        }
    }
    Ok(())
}

/// Resolve a store directory path at the head.
pub(crate) fn resolve_dir_path(store: &Store, path: &str) -> Result<Hash32> {
    let mut current = Hash32::ZERO;
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(current);
    }
    for part in trimmed.split('/') {
        if part.is_empty() {
            continue;
        }
        current = store.lookup(current, part.as_bytes()).map_err(|err| {
            if err.class() == Class::NotFound {
                Error::not_found(format!("store directory `{path}` does not exist"))
            } else {
                err
            }
        })?;
        let record = store.object_meta(current)?;
        if record.kind != crate::state::ObjectKind::Dir {
            return Err(Error::not_dir(format!("`{path}` is not a directory")));
        }
    }
    Ok(current)
}

fn resolve_dir_at(_store: &Store, state: &State, path: &str) -> Result<Hash32> {
    let mut current = Hash32::ZERO;
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(current);
    }
    for part in trimmed.split('/') {
        if part.is_empty() {
            continue;
        }
        current = state.lookup(current, part.as_bytes()).map_err(|err| {
            if err.class() == Class::NotFound {
                Error::not_found(format!("store directory `{path}` does not exist"))
            } else {
                err
            }
        })?;
        let record = state
            .objects
            .get(&current)
            .ok_or_else(|| Error::corrupt("directory entry names an absent object"))?;
        if record.kind != crate::state::ObjectKind::Dir {
            return Err(Error::not_dir(format!("`{path}` is not a directory")));
        }
    }
    Ok(current)
}

/// Create every missing component of `path` and return the final directory.
pub(crate) fn ensure_dir_path(store: &mut Store, path: &str) -> Result<Hash32> {
    let mut current = Hash32::ZERO;
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(current);
    }
    for part in trimmed.split('/') {
        if part.is_empty() {
            continue;
        }
        match store.lookup(current, part.as_bytes()) {
            Ok(next) => {
                let record = store.object_meta(next)?;
                if record.kind != crate::state::ObjectKind::Dir {
                    return Err(Error::not_dir(format!("`{path}` is not a directory")));
                }
                current = next;
            }
            Err(err) if err.class() == Class::NotFound => {
                current = store.mkdir(current, part.as_bytes(), 0o755, 0, 0)?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(current)
}

/// Structural equality helper used by the court: artifact root only.
pub fn same_root(left: &TrainingArtifact, right: &TrainingArtifact) -> Result<bool> {
    Ok(left.root()? == right.root()?)
}

/// Read an object record from a state by its ObjectId.
pub fn object_in_state(state: &State, id: Hash32) -> Result<&ObjectRecord> {
    state
        .objects
        .get(&id)
        .ok_or_else(|| Error::not_found("object is absent at the snapshot"))
}
