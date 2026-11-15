//! Typed causal adapters (LoRA / DoRA) and precomputed merged views.
//!
//! An adapter is stored as its **own** artifact: one `weft.adapter.v1` manifest
//! that names its exact base by the base checkpoint's manifest content root and
//! records every adapter tensor by object id and content root. Restoring an
//! adapter verifies each tensor's root; resolving the base verifies the base
//! checkpoint's own roots. Neither artifact can silently stand in for the
//! other.
//!
//! Applying an adapter is a separate, explicit operation that publishes a
//! `weft.merged-view.v1` artifact with its own identity. The arithmetic is
//! pinned by a `weft.merge-contract.v1` record carried inside that manifest
//! (accumulation dtype, output dtype, rounding rule, operation order, and the
//! exact scaling value as f64 bits). **Mathematical equivalence is not byte
//! equivalence**: a merged view produced under one contract is not claimed to
//! equal a native fine-tune, an upstream merge, or a merge performed with a
//! different order or rounding. Every manifest therefore carries
//! `merge_equivalence: "not_claimed"`, and the spec repeats it.
//!
//! Nothing in this module changes a frozen wire byte, mode number or root. The
//! adapter and merged-view manifests are new canonical WCB schemas; adapter and
//! merged tensors are ordinary store objects written through the existing
//! planner.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use weftcodec::wcb::Value;

use crate::checkpoint::{LoadedCheckpointManifest, Sha256Digest};
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use crate::safetensors::{self, SafetensorsFile, SafetensorsTensor};
use crate::state::{ObjectKind, State};
use crate::store::Store;
use crate::tensor_store::{TensorLayoutSpec, TensorStrategy, TensorWriteSpec};
use crate::wcb_util as wu;

/// Canonical adapter manifest schema.
pub const ADAPTER_SCHEMA: &str = "weft.adapter.v1";
/// Domain separation for the adapter identity.
pub const ADAPTER_DOMAIN: &str = "weft/adapter/v1";
/// Store directory that holds adapter artifacts.
pub const ADAPTER_ROOT: &str = "adapters";

/// Canonical merged-view manifest schema.
pub const MERGED_VIEW_SCHEMA: &str = "weft.merged-view.v1";
/// Domain separation for the merged-view identity.
pub const MERGED_VIEW_DOMAIN: &str = "weft/merged-view/v1";
/// Store directory that holds merged views.
pub const MERGED_VIEW_ROOT: &str = "merged-views";

/// Canonical merge-contract schema.
pub const MERGE_CONTRACT_SCHEMA: &str = "weft.merge-contract.v1";

/// Maximum number of target modules one adapter manifest may declare.
pub const MAX_ADAPTER_TARGETS: usize = 16_384;
/// Maximum canonical bytes of one adapter manifest.
pub const MAX_ADAPTER_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum canonical bytes of one merged-view manifest.
pub const MAX_MERGED_VIEW_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

/// The explicit statement every artifact carries: bytes computed under one
/// merge contract are not claimed to equal any other producer's bytes.
pub const MERGE_EQUIVALENCE_NOT_CLAIMED: &str = "not_claimed";

fn get_u64_array(map: &BTreeMap<String, Value>, key: &str) -> Result<Vec<u64>> {
    match wu::field(map, key)? {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::U64(value) => out.push(*value),
                    _ => {
                        return Err(Error::malformed(format!(
                            "key `{key}` must be an array of u64"
                        )))
                    }
                }
            }
            Ok(out)
        }
        _ => Err(Error::malformed(format!("key `{key}` must be an array"))),
    }
}

fn get_bool(map: &BTreeMap<String, Value>, key: &str) -> Result<bool> {
    match wu::field(map, key)? {
        Value::Bool(value) => Ok(*value),
        _ => Err(Error::malformed(format!("key `{key}` must be a bool"))),
    }
}

fn digest32(bytes: &[u8], what: &str) -> Result<Sha256Digest> {
    if bytes.len() != 32 {
        return Err(Error::malformed(format!(
            "{what} must be exactly 32 bytes (got {})",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

/// Adapter arithmetic family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    /// `W' = W + (alpha / r) * B @ A`.
    Lora,
    /// `W' = m * (W + delta) / ||W + delta||_2`, with `delta =
    /// (alpha / r) * B @ A`; the norm runs over the input dimension for each
    /// output row and `m` is the per-output-row magnitude vector.
    Dora,
}

impl AdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AdapterKind::Lora => "lora",
            AdapterKind::Dora => "dora",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "lora" => Ok(AdapterKind::Lora),
            "dora" => Ok(AdapterKind::Dora),
            other => Err(Error::malformed(format!(
                "unknown adapter kind `{other}` (expected `lora` or `dora`)"
            ))),
        }
    }
}

/// One exact tensor reference inside an adapter manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorRef {
    pub object: Hash32,
    pub content: Hash32,
    pub dtype: String,
    pub shape: Vec<u64>,
}

impl TensorRef {
    pub const KEYS: &'static [&'static str] = &["content", "dtype", "object", "shape"];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("object", wu::hash_value(self.object)),
            ("content", wu::hash_value(self.content)),
            ("dtype", Value::Str(self.dtype.clone())),
            (
                "shape",
                Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        Ok(TensorRef {
            object: wu::get_hash(map, "object")?,
            content: wu::get_hash(map, "content")?,
            dtype: wu::get_text(map, "dtype")?,
            shape: get_u64_array(map, "shape")?,
        })
    }

    pub fn logical_bytes(&self) -> Result<u64> {
        let dtype = crate::representation::TensorElementDtype::parse(&self.dtype)?;
        let elements = self
            .shape
            .iter()
            .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
            .ok_or_else(|| Error::limit_exceeded("tensor shape overflows the element count"))?;
        elements
            .checked_mul(dtype.element_bytes())
            .ok_or_else(|| Error::limit_exceeded("tensor shape overflows the byte length"))
    }
}

/// One target module's adapter arithmetic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterTarget {
    /// Target tensor name in the base checkpoint (e.g.
    /// `model.layers.0.self_attn.q_proj.weight`).
    pub base_tensor: String,
    /// Content root the base checkpoint records for this tensor.
    pub base_content: Hash32,
    pub a: TensorRef,
    pub b: TensorRef,
    /// DoRA magnitude vector (required for `dora`, forbidden for `lora`).
    pub magnitude: Option<TensorRef>,
    /// `data_offsets` of A, B and (when present) magnitude in the source
    /// container. Present only when the adapter was ingested from a container,
    /// and required for a byte-exact container restore.
    pub source_offsets: Option<SourceOffsets>,
}

/// One tensor's `[begin, end)` data range in the source container. Offsets are
/// relative to the first data byte, exactly as safetensors records them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceOffsets {
    pub a: (u64, u64),
    pub b: (u64, u64),
    pub magnitude: Option<(u64, u64)>,
}

impl SourceOffsets {
    fn range_to_value((begin, end): (u64, u64)) -> Value {
        wu::map([("begin", Value::U64(begin)), ("end", Value::U64(end))])
    }

    fn range_from_value(value: &Value) -> Result<(u64, u64)> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["begin", "end"])?;
        let (begin, end) = (wu::get_u64(map, "begin")?, wu::get_u64(map, "end")?);
        if end < begin {
            return Err(Error::malformed(format!(
                "source offsets [{begin}, {end}) are reversed"
            )));
        }
        Ok((begin, end))
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("a", Self::range_to_value(self.a)),
            ("b", Self::range_to_value(self.b)),
            (
                "magnitude",
                match self.magnitude {
                    Some(range) => Self::range_to_value(range),
                    None => Value::Null,
                },
            ),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["a", "b", "magnitude"])?;
        let magnitude = match wu::field(map, "magnitude")? {
            Value::Null => None,
            other => Some(Self::range_from_value(other)?),
        };
        Ok(SourceOffsets {
            a: Self::range_from_value(wu::field(map, "a")?)?,
            b: Self::range_from_value(wu::field(map, "b")?)?,
            magnitude,
        })
    }
}

impl AdapterTarget {
    pub const KEYS: &'static [&'static str] = &[
        "a",
        "b",
        "base_content",
        "base_tensor",
        "magnitude",
        "source_offsets",
    ];

    fn to_value(&self) -> Value {
        wu::map([
            ("base_tensor", Value::Str(self.base_tensor.clone())),
            ("base_content", wu::hash_value(self.base_content)),
            ("a", self.a.to_value()),
            ("b", self.b.to_value()),
            (
                "magnitude",
                match &self.magnitude {
                    Some(reference) => reference.to_value(),
                    None => Value::Null,
                },
            ),
            (
                "source_offsets",
                match &self.source_offsets {
                    Some(offsets) => offsets.to_value(),
                    None => Value::Null,
                },
            ),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let magnitude = match wu::field(map, "magnitude")? {
            Value::Null => None,
            other => Some(TensorRef::from_value(other)?),
        };
        let source_offsets = match wu::field(map, "source_offsets")? {
            Value::Null => None,
            other => Some(SourceOffsets::from_value(other)?),
        };
        Ok(AdapterTarget {
            base_tensor: wu::get_text(map, "base_tensor")?,
            base_content: wu::get_hash(map, "base_content")?,
            a: TensorRef::from_value(wu::field(map, "a")?)?,
            b: TensorRef::from_value(wu::field(map, "b")?)?,
            magnitude,
            source_offsets,
        })
    }
}

/// The exact base an adapter names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseRef {
    /// Checkpoint directory name in this store.
    pub checkpoint: String,
    /// Content address of the checkpoint's manifest file object.
    pub manifest_content: Hash32,
    /// `H("weft/checkpoint/manifest/v1", canonical manifest bytes)`.
    pub manifest_root: Hash32,
    /// SHA-256 of the exact safetensors container the base was ingested from.
    pub container_sha256: Sha256Digest,
    pub tensor_count: u64,
    pub element_bytes: u64,
}

impl BaseRef {
    pub const KEYS: &'static [&'static str] = &[
        "checkpoint",
        "container_sha256",
        "element_bytes",
        "manifest_content",
        "manifest_root",
        "tensor_count",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("checkpoint", Value::Str(self.checkpoint.clone())),
            ("manifest_content", wu::hash_value(self.manifest_content)),
            ("manifest_root", wu::hash_value(self.manifest_root)),
            (
                "container_sha256",
                Value::Bytes(self.container_sha256.to_vec()),
            ),
            ("tensor_count", Value::U64(self.tensor_count)),
            ("element_bytes", Value::U64(self.element_bytes)),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        Ok(BaseRef {
            checkpoint: wu::get_text(map, "checkpoint")?,
            manifest_content: wu::get_hash(map, "manifest_content")?,
            manifest_root: wu::get_hash(map, "manifest_root")?,
            container_sha256: digest32(
                &wu::get_bytes(map, "container_sha256")?,
                "container_sha256",
            )?,
            tensor_count: wu::get_u64(map, "tensor_count")?,
            element_bytes: wu::get_u64(map, "element_bytes")?,
        })
    }
}

/// Provenance of the adapter bytes (where they were fetched from).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterSource {
    pub repo: String,
    pub revision: String,
    pub file: String,
    pub url: String,
    /// SHA-256 of `adapter_model.safetensors` exactly as fetched.
    pub file_sha256: Sha256Digest,
    /// SHA-256 of `adapter_config.json` exactly as fetched.
    pub config_sha256: Sha256Digest,
}

impl AdapterSource {
    pub const KEYS: &'static [&'static str] = &[
        "config_sha256",
        "file",
        "file_sha256",
        "repo",
        "revision",
        "url",
    ];

    fn to_value(&self) -> Value {
        wu::map([
            ("repo", Value::Str(self.repo.clone())),
            ("revision", Value::Str(self.revision.clone())),
            ("file", Value::Str(self.file.clone())),
            ("url", Value::Str(self.url.clone())),
            ("file_sha256", Value::Bytes(self.file_sha256.to_vec())),
            ("config_sha256", Value::Bytes(self.config_sha256.to_vec())),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        Ok(AdapterSource {
            repo: wu::get_text(map, "repo")?,
            revision: wu::get_text(map, "revision")?,
            file: wu::get_text(map, "file")?,
            url: wu::get_text(map, "url")?,
            file_sha256: digest32(&wu::get_bytes(map, "file_sha256")?, "file_sha256")?,
            config_sha256: digest32(&wu::get_bytes(map, "config_sha256")?, "config_sha256")?,
        })
    }
}

/// The exact source container an adapter was ingested from, recorded verbatim so
/// [`Store::restore_adapter`] can reproduce the file byte-for-byte instead of
/// only its tensors. `header` is the exact `u64le(header_len) || JSON header`
/// bytes; offsets in [`SourceOffsets`] are relative to the first data byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceContainer {
    pub header: Vec<u8>,
    pub file_bytes: u64,
}

impl SourceContainer {
    pub const KEYS: &'static [&'static str] = &["file_bytes", "header"];
    /// Upper bound on a recorded header. A container with more header bytes than
    /// this is refused rather than inflating every manifest read.
    pub const MAX_HEADER_BYTES: u64 = 16 * 1024 * 1024;

    fn to_value(&self) -> Value {
        wu::map([
            ("header", Value::Bytes(self.header.clone())),
            ("file_bytes", Value::U64(self.file_bytes)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let header = wu::get_bytes(map, "header")?;
        if header.len() < 8 {
            return Err(Error::malformed(
                "recorded source header is shorter than its 8-byte length prefix",
            ));
        }
        if header.len() as u64 > Self::MAX_HEADER_BYTES {
            return Err(Error::limit_exceeded(format!(
                "recorded source header is {} bytes, above the {}-byte bound",
                header.len(),
                Self::MAX_HEADER_BYTES
            )));
        }
        let mut length = [0u8; 8];
        length.copy_from_slice(&header[..8]);
        let declared = u64::from_le_bytes(length);
        if declared as usize != header.len() - 8 {
            return Err(Error::malformed(format!(
                "recorded source header declares {declared} JSON bytes but carries {}",
                header.len() - 8
            )));
        }
        let file_bytes = wu::get_u64(map, "file_bytes")?;
        if file_bytes < header.len() as u64 {
            return Err(Error::malformed(
                "recorded source container is shorter than its header",
            ));
        }
        Ok(SourceContainer { header, file_bytes })
    }
}

/// A complete `weft.adapter.v1` manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterManifest {
    pub name: String,
    pub kind: AdapterKind,
    pub rank: u64,
    /// `lora_alpha` as f64 bits (exact).
    pub alpha_bits: u64,
    /// Effective scaling as f64 bits (exact): `alpha/r`, or `alpha/sqrt(r)`
    /// when `use_rslora` is declared.
    pub scaling_bits: u64,
    pub use_rslora: bool,
    /// `bias` field of the PEFT config (`none` for every adapter here).
    pub bias: String,
    /// Sorted, unique PEFT `target_modules` names.
    pub target_modules: Vec<String>,
    pub source: AdapterSource,
    /// The source container's exact header, when the adapter came from one.
    pub source_container: Option<SourceContainer>,
    pub base: BaseRef,
    /// Sorted by `base_tensor`, unique.
    pub targets: Vec<AdapterTarget>,
}

impl AdapterManifest {
    pub const KEYS: &'static [&'static str] = &[
        "alpha_bits",
        "base",
        "bias",
        "kind",
        "merge_equivalence",
        "name",
        "rank",
        "scaling_bits",
        "schema",
        "source",
        "source_container",
        "target_modules",
        "targets",
        "use_rslora",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(ADAPTER_SCHEMA.to_owned())),
            ("name", Value::Str(self.name.clone())),
            ("kind", Value::Str(self.kind.as_str().to_owned())),
            ("rank", Value::U64(self.rank)),
            ("alpha_bits", Value::U64(self.alpha_bits)),
            ("scaling_bits", Value::U64(self.scaling_bits)),
            ("use_rslora", Value::Bool(self.use_rslora)),
            ("bias", Value::Str(self.bias.clone())),
            (
                "merge_equivalence",
                Value::Str(MERGE_EQUIVALENCE_NOT_CLAIMED.to_owned()),
            ),
            (
                "target_modules",
                Value::Array(
                    self.target_modules
                        .iter()
                        .map(|module| Value::Str(module.clone()))
                        .collect(),
                ),
            ),
            ("source", self.source.to_value()),
            (
                "source_container",
                match &self.source_container {
                    Some(container) => container.to_value(),
                    None => Value::Null,
                },
            ),
            ("base", self.base.to_value()),
            (
                "targets",
                Value::Array(self.targets.iter().map(AdapterTarget::to_value).collect()),
            ),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != ADAPTER_SCHEMA {
            return Err(Error::malformed(format!(
                "adapter manifest schema is `{schema}`, expected `{ADAPTER_SCHEMA}`"
            )));
        }
        let merge_equivalence = wu::get_text(map, "merge_equivalence")?;
        if merge_equivalence != MERGE_EQUIVALENCE_NOT_CLAIMED {
            return Err(Error::malformed(format!(
                "adapter merge_equivalence must be `{MERGE_EQUIVALENCE_NOT_CLAIMED}` (got `{merge_equivalence}`)"
            )));
        }
        let target_modules = match wu::field(map, "target_modules")? {
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Value::Str(text) => out.push(text.clone()),
                        _ => {
                            return Err(Error::malformed(
                                "adapter target_modules entries must be text",
                            ))
                        }
                    }
                }
                out
            }
            _ => return Err(Error::malformed("adapter target_modules must be an array")),
        };
        let targets = match wu::field(map, "targets")? {
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(AdapterTarget::from_value(item)?);
                }
                out
            }
            _ => return Err(Error::malformed("adapter targets must be an array")),
        };
        let manifest = AdapterManifest {
            name: wu::get_text(map, "name")?,
            kind: AdapterKind::parse(&wu::get_text(map, "kind")?)?,
            rank: wu::get_u64(map, "rank")?,
            alpha_bits: wu::get_u64(map, "alpha_bits")?,
            scaling_bits: wu::get_u64(map, "scaling_bits")?,
            use_rslora: get_bool(map, "use_rslora")?,
            bias: wu::get_text(map, "bias")?,
            target_modules,
            source: AdapterSource::from_value(wu::field(map, "source")?)?,
            source_container: match wu::field(map, "source_container")? {
                Value::Null => None,
                other => Some(SourceContainer::from_value(other)?),
            },
            base: BaseRef::from_value(wu::field(map, "base")?)?,
            targets,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Structural validation. Content roots are checked by the store.
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::malformed("adapter name must not be empty"));
        }
        if self.rank == 0 {
            return Err(Error::malformed("adapter rank must be at least 1"));
        }
        if !self.alpha().is_finite() || !self.scaling().is_finite() {
            return Err(Error::malformed("adapter alpha and scaling must be finite"));
        }
        if self.targets.is_empty() {
            return Err(Error::malformed(
                "adapter declares no target modules; an empty adapter is not an artifact",
            ));
        }
        if self.targets.len() > MAX_ADAPTER_TARGETS {
            return Err(Error::limit_exceeded(format!(
                "adapter declares {} targets, above the {MAX_ADAPTER_TARGETS} bound",
                self.targets.len()
            )));
        }
        if self.target_modules.is_empty() {
            return Err(Error::malformed("adapter target_modules must not be empty"));
        }
        if self.source_container.is_some()
            && self
                .targets
                .iter()
                .any(|target| target.source_offsets.is_none())
        {
            return Err(Error::malformed(
                "an adapter that records its source container must record every target's offsets",
            ));
        }
        if self.source_container.is_none()
            && self
                .targets
                .iter()
                .any(|target| target.source_offsets.is_some())
        {
            return Err(Error::malformed(
                "an adapter with target offsets must record the source container header",
            ));
        }
        for target in &self.targets {
            if let Some(offsets) = &target.source_offsets {
                let rows: [(&str, Option<&TensorRef>, Option<(u64, u64)>); 3] = [
                    ("a", Some(&target.a), Some(offsets.a)),
                    ("b", Some(&target.b), Some(offsets.b)),
                    ("magnitude", target.magnitude.as_ref(), offsets.magnitude),
                ];
                for (label, reference, range) in rows {
                    let Some(reference) = reference else {
                        if range.is_some() {
                            return Err(Error::malformed(format!(
                                "adapter target `{}` records {label} offsets but has no {label} tensor",
                                target.base_tensor
                            )));
                        }
                        continue;
                    };
                    let Some((begin, end)) = range else {
                        return Err(Error::malformed(format!(
                            "adapter target `{}` has a {label} tensor but no {label} offsets",
                            target.base_tensor
                        )));
                    };
                    let expected = reference.logical_bytes()?;
                    if end - begin != expected {
                        return Err(Error::malformed(format!(
                            "adapter target `{}` {label} offsets span {} bytes but the tensor is {expected}",
                            target.base_tensor,
                            end - begin
                        )));
                    }
                }
            }
        }
        for pair in self.target_modules.windows(2) {
            if pair[0] >= pair[1] {
                return Err(Error::non_canonical(
                    "adapter target_modules must be sorted and unique",
                ));
            }
        }
        for pair in self.targets.windows(2) {
            if pair[0].base_tensor >= pair[1].base_tensor {
                return Err(Error::non_canonical(
                    "adapter targets must be sorted by base_tensor and unique",
                ));
            }
        }
        for module in &self.target_modules {
            let suffix = format!(".{module}.weight");
            if !self.targets.iter().any(|target| {
                target.base_tensor == format!("{module}.weight")
                    || target.base_tensor.ends_with(&suffix)
            }) {
                return Err(Error::malformed(format!(
                    "adapter declares target module `{module}` with no target tensors"
                )));
            }
        }
        match self.kind {
            AdapterKind::Lora => {
                if let Some(target) = self
                    .targets
                    .iter()
                    .find(|target| target.magnitude.is_some())
                {
                    return Err(Error::malformed(format!(
                        "lora adapter target `{}` carries a DoRA magnitude vector",
                        target.base_tensor
                    )));
                }
            }
            AdapterKind::Dora => {
                if let Some(target) = self
                    .targets
                    .iter()
                    .find(|target| target.magnitude.is_none())
                {
                    return Err(Error::malformed(format!(
                        "dora adapter target `{}` is missing its magnitude vector",
                        target.base_tensor
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn alpha(&self) -> f64 {
        f64::from_bits(self.alpha_bits)
    }

    pub fn scaling(&self) -> f64 {
        f64::from_bits(self.scaling_bits)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = wu::encode(&self.to_value())?;
        if bytes.len() as u64 > MAX_ADAPTER_MANIFEST_BYTES {
            return Err(Error::limit_exceeded(format!(
                "adapter manifest is {} bytes, above the {MAX_ADAPTER_MANIFEST_BYTES}-byte bound",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(&wu::decode(bytes)?)
    }

    /// `AdapterId = H("weft/adapter/v1", WCB(manifest))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(ADAPTER_DOMAIN, &self.encode()?))
    }

    /// Total exact adapter arithmetic bytes (A, B and magnitude payloads).
    pub fn adapter_bytes(&self) -> Result<u64> {
        let mut total = 0u64;
        for target in &self.targets {
            total += target.a.logical_bytes()? + target.b.logical_bytes()?;
            if let Some(magnitude) = &target.magnitude {
                total += magnitude.logical_bytes()?;
            }
        }
        Ok(total)
    }
}

/// The pinned arithmetic of one merge. Every field is recorded so another
/// producer can see exactly which computation the merged bytes represent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeContract {
    pub dtype_accum: String,
    pub dtype_output: String,
    pub rounding: String,
    pub order: String,
    /// Effective LoRA scaling as f64 bits.
    pub scaling_bits: u64,
    pub dora_norm: String,
    pub clamp: String,
}

impl MergeContract {
    pub const KEYS: &'static [&'static str] = &[
        "clamp",
        "dora_norm",
        "dtype_accum",
        "dtype_output",
        "order",
        "rounding",
        "scaling_bits",
        "schema",
    ];

    pub fn for_kind(kind: AdapterKind, scaling: f64, dtype_output: &str) -> Self {
        MergeContract {
            dtype_accum: "f32".to_owned(),
            dtype_output: dtype_output.to_owned(),
            rounding: "round-half-to-even".to_owned(),
            order: match kind {
                AdapterKind::Lora => "delta_then_add".to_owned(),
                AdapterKind::Dora => "delta_then_add_then_row_normalize".to_owned(),
            },
            scaling_bits: scaling.to_bits(),
            dora_norm: match kind {
                AdapterKind::Lora => "none".to_owned(),
                AdapterKind::Dora => "l2_over_input_dim".to_owned(),
            },
            clamp: "none".to_owned(),
        }
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(MERGE_CONTRACT_SCHEMA.to_owned())),
            ("dtype_accum", Value::Str(self.dtype_accum.clone())),
            ("dtype_output", Value::Str(self.dtype_output.clone())),
            ("rounding", Value::Str(self.rounding.clone())),
            ("order", Value::Str(self.order.clone())),
            ("scaling_bits", Value::U64(self.scaling_bits)),
            ("dora_norm", Value::Str(self.dora_norm.clone())),
            ("clamp", Value::Str(self.clamp.clone())),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != MERGE_CONTRACT_SCHEMA {
            return Err(Error::malformed(format!(
                "merge contract schema is `{schema}`, expected `{MERGE_CONTRACT_SCHEMA}`"
            )));
        }
        Ok(MergeContract {
            dtype_accum: wu::get_text(map, "dtype_accum")?,
            dtype_output: wu::get_text(map, "dtype_output")?,
            rounding: wu::get_text(map, "rounding")?,
            order: wu::get_text(map, "order")?,
            scaling_bits: wu::get_u64(map, "scaling_bits")?,
            dora_norm: wu::get_text(map, "dora_norm")?,
            clamp: wu::get_text(map, "clamp")?,
        })
    }

    /// This implementation performs exactly one arithmetic. A manifest that
    /// declares any other contract is refused rather than guessed at.
    pub fn require_supported(&self) -> Result<()> {
        if self.dtype_accum != "f32"
            || self.rounding != "round-half-to-even"
            || self.clamp != "none"
            || (self.order != "delta_then_add" && self.order != "delta_then_add_then_row_normalize")
            || (self.dora_norm != "none" && self.dora_norm != "l2_over_input_dim")
        {
            return Err(Error::unsupported_representation(format!(
                "merge contract (accum {}, rounding {}, order {}, norm {}, clamp {}) is not one this implementation performs",
                self.dtype_accum, self.rounding, self.order, self.dora_norm, self.clamp
            )));
        }
        Ok(())
    }
}

/// One tensor of a merged view: either the base tensor by reference or a newly
/// stored merged tensor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedTensorEntry {
    pub name: String,
    pub merged: bool,
    pub content: Hash32,
    pub object: Option<Hash32>,
    pub dtype: String,
    pub shape: Vec<u64>,
}

impl MergedTensorEntry {
    pub const KEYS: &'static [&'static str] =
        &["content", "dtype", "merged", "name", "object", "shape"];

    fn to_value(&self) -> Value {
        wu::map([
            ("name", Value::Str(self.name.clone())),
            ("merged", Value::Bool(self.merged)),
            ("content", wu::hash_value(self.content)),
            ("object", wu::opt_hash_value(self.object)),
            ("dtype", Value::Str(self.dtype.clone())),
            (
                "shape",
                Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        Ok(MergedTensorEntry {
            name: wu::get_text(map, "name")?,
            merged: get_bool(map, "merged")?,
            content: wu::get_hash(map, "content")?,
            object: wu::get_opt_hash(map, "object")?,
            dtype: wu::get_text(map, "dtype")?,
            shape: get_u64_array(map, "shape")?,
        })
    }
}

/// A `weft.merged-view.v1` manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedViewManifest {
    pub name: String,
    pub adapter: Hash32,
    pub adapter_manifest_content: Hash32,
    pub base: BaseRef,
    pub contract: MergeContract,
    /// Sorted by name; the name set equals the base checkpoint's tensor set.
    pub tensors: Vec<MergedTensorEntry>,
}

impl MergedViewManifest {
    pub const KEYS: &'static [&'static str] = &[
        "adapter",
        "adapter_manifest_content",
        "base",
        "contract",
        "merge_equivalence",
        "name",
        "schema",
        "tensors",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(MERGED_VIEW_SCHEMA.to_owned())),
            ("name", Value::Str(self.name.clone())),
            ("adapter", wu::hash_value(self.adapter)),
            (
                "adapter_manifest_content",
                wu::hash_value(self.adapter_manifest_content),
            ),
            ("base", self.base.to_value()),
            ("contract", self.contract.to_value()),
            (
                "merge_equivalence",
                Value::Str(MERGE_EQUIVALENCE_NOT_CLAIMED.to_owned()),
            ),
            (
                "tensors",
                Value::Array(
                    self.tensors
                        .iter()
                        .map(MergedTensorEntry::to_value)
                        .collect(),
                ),
            ),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != MERGED_VIEW_SCHEMA {
            return Err(Error::malformed(format!(
                "merged-view schema is `{schema}`, expected `{MERGED_VIEW_SCHEMA}`"
            )));
        }
        let equivalence = wu::get_text(map, "merge_equivalence")?;
        if equivalence != MERGE_EQUIVALENCE_NOT_CLAIMED {
            return Err(Error::malformed(format!(
                "merged-view merge_equivalence must be `{MERGE_EQUIVALENCE_NOT_CLAIMED}`"
            )));
        }
        let tensors = match wu::field(map, "tensors")? {
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(MergedTensorEntry::from_value(item)?);
                }
                out
            }
            _ => return Err(Error::malformed("merged-view tensors must be an array")),
        };
        let manifest = MergedViewManifest {
            name: wu::get_text(map, "name")?,
            adapter: wu::get_hash(map, "adapter")?,
            adapter_manifest_content: wu::get_hash(map, "adapter_manifest_content")?,
            base: BaseRef::from_value(wu::field(map, "base")?)?,
            contract: MergeContract::from_value(wu::field(map, "contract")?)?,
            tensors,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::malformed("merged-view name must not be empty"));
        }
        self.contract.require_supported()?;
        if self.tensors.is_empty() {
            return Err(Error::malformed("merged view has no tensors"));
        }
        for pair in self.tensors.windows(2) {
            if pair[0].name >= pair[1].name {
                return Err(Error::non_canonical(
                    "merged-view tensors must be sorted by name and unique",
                ));
            }
        }
        for entry in &self.tensors {
            if entry.merged != entry.object.is_some() {
                return Err(Error::malformed(format!(
                    "merged-view tensor `{}` must name an object exactly when it is merged",
                    entry.name
                )));
            }
            if entry.dtype != self.contract.dtype_output {
                return Err(Error::malformed(format!(
                    "merged-view tensor `{}` dtype `{}` differs from the contract output dtype `{}`",
                    entry.name, entry.dtype, self.contract.dtype_output
                )));
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = wu::encode(&self.to_value())?;
        if bytes.len() as u64 > MAX_MERGED_VIEW_MANIFEST_BYTES {
            return Err(Error::limit_exceeded(format!(
                "merged-view manifest is {} bytes, above the {MAX_MERGED_VIEW_MANIFEST_BYTES}-byte bound",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_value(&wu::decode(bytes)?)
    }

    /// `MergedViewId = H("weft/merged-view/v1", WCB(manifest))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(MERGED_VIEW_DOMAIN, &self.encode()?))
    }
}

// ---------------------------------------------------------------------------
// BF16 / F16 / F32 conversion used by the pinned merge arithmetic
// ---------------------------------------------------------------------------

/// Interpret one element as `f32`. Only the dtypes a LoRA merge can target are
/// accepted; anything else is a typed error, never a guess.
pub fn element_as_f32(dtype: &str, bytes: &[u8], index: usize) -> Result<f32> {
    let width = match dtype {
        "f32" => 4,
        "f16" | "bf16" => 2,
        other => {
            return Err(Error::unsupported_representation(format!(
                "adapter merge cannot read element dtype `{other}` (only f16, bf16, f32)"
            )))
        }
    };
    let offset = index
        .checked_mul(width)
        .ok_or_else(|| Error::limit_exceeded("element index overflows"))?;
    let slice = bytes
        .get(offset..offset + width)
        .ok_or_else(|| Error::malformed("element index is past the end of the tensor bytes"))?;
    if width == 4 {
        let mut word = [0u8; 4];
        word.copy_from_slice(slice);
        Ok(f32::from_le_bytes(word))
    } else {
        let bits = u16::from_le_bytes([slice[0], slice[1]]);
        Ok(if dtype == "bf16" {
            f32::from_bits((bits as u32) << 16)
        } else {
            f16_to_f32(bits)
        })
    }
}

fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x03ff) as u32;
    let out = if exp == 0 {
        if frac == 0 {
            sign << 31
        } else {
            let mut exp = exp;
            let mut frac = frac;
            while frac & 0x0400 == 0 {
                frac <<= 1;
                exp = exp.wrapping_sub(1);
            }
            // The loop leaves the implicit bit at position 10; putting it back
            // into the exponent is what makes the scale 2^(exp-25) rather than
            // 2^(exp-26). Without this every f16 subnormal decodes at half its
            // value.
            exp = exp.wrapping_add(1);
            frac &= 0x03ff;
            let e = 127u32.wrapping_sub(15).wrapping_add(exp);
            (sign << 31) | (e << 23) | (frac << 13)
        }
    } else if exp == 0x1f {
        (sign << 31) | 0x7f80_0000 | (frac << 13)
    } else {
        let e = exp + (127 - 15);
        (sign << 31) | (e << 23) | (frac << 13)
    };
    f32::from_bits(out)
}

/// Round `value` to the nearest representable element of `dtype`, ties to even.
pub fn f32_to_element(dtype: &str, value: f32) -> Result<Vec<u8>> {
    match dtype {
        "f32" => Ok(value.to_le_bytes().to_vec()),
        "bf16" => Ok(f32_to_bf16_bits(value).to_le_bytes().to_vec()),
        "f16" => Ok(f32_to_f16_bits(value).to_le_bytes().to_vec()),
        other => Err(Error::unsupported_representation(format!(
            "adapter merge cannot write element dtype `{other}` (only f16, bf16, f32)"
        ))),
    }
}

/// `f32` → `bf16` with round-to-nearest-even; NaN stays a quiet NaN.
pub fn f32_to_bf16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    if value.is_nan() {
        return ((bits >> 16) as u16) | 0x0040;
    }
    let lsb = (bits >> 16) & 1;
    let rounded = bits.wrapping_add(0x7fff + lsb);
    (rounded >> 16) as u16
}

/// `f32` → `f16` with round-to-nearest-even; overflow becomes infinity.
pub fn f32_to_f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 31) & 1) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let frac = bits & 0x007f_ffff;
    if exp == 0xff {
        if frac != 0 {
            return (sign << 15) | 0x7e00;
        }
        return (sign << 15) | 0x7c00;
    }
    let unbiased = exp - 127;
    if unbiased > 15 {
        return (sign << 15) | 0x7c00;
    }
    // Half the smallest f16 subnormal is 2^-25. Anything strictly below that
    // (and the exact tie, which rounds to the even zero) is zero; anything at or
    // above it must go through the subnormal rounding path so a value just
    // above the midpoint becomes 0x0001 and not zero.
    if unbiased < -25 {
        return sign << 15;
    }
    if unbiased >= -14 {
        let exp16 = (unbiased + 15) as u32;
        let mantissa = frac >> 13;
        let remainder = frac & 0x1fff;
        let mut out = ((sign as u32) << 15) | (exp16 << 10) | mantissa;
        let halfway = 0x1000;
        let lsb = mantissa & 1;
        if remainder > halfway || (remainder == halfway && lsb == 1) {
            out += 1;
        }
        return out as u16;
    }
    let shift = (-14 - unbiased) as u32;
    let full = (frac | 0x0080_0000) >> (shift + 13);
    let rem_shift = shift + 13;
    let remainder = if rem_shift >= 32 {
        0
    } else {
        (frac | 0x0080_0000) & ((1u32 << rem_shift) - 1)
    };
    let halfway = 1u32 << (rem_shift - 1);
    let mut out = ((sign as u32) << 15) | full;
    let lsb = full & 1;
    if remainder > halfway || (remainder == halfway && lsb == 1) {
        out += 1;
    }
    out as u16
}

// ---------------------------------------------------------------------------
// PEFT key mapping
// ---------------------------------------------------------------------------

/// Which of an adapter's matrices a PEFT key names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixPart {
    A,
    B,
    Magnitude,
}

/// What one PEFT tensor key means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeftKey {
    Matrix {
        base_tensor: String,
        which: MatrixPart,
    },
    /// A tensor that is not part of a LoRA/DoRA module (e.g. `modules_to_save`).
    Other,
}

/// Map one PEFT `adapter_model.safetensors` key to its base tensor name.
///
/// PEFT writes `base_model.model.model.layers.0.self_attn.q_proj.lora_A.weight`
/// (adapter name segment optional). The base tensor is the module path with a
/// `.weight` suffix. A key that is not a LoRA/DoRA matrix is `Other` and is
/// reported by the caller, never silently dropped.
pub fn map_peft_key(key: &str) -> PeftKey {
    let stripped = key.strip_prefix("base_model.model.").unwrap_or(key);
    for (marker, which) in [
        (".lora_A", MatrixPart::A),
        (".lora_B", MatrixPart::B),
        (".lora_magnitude_vector", MatrixPart::Magnitude),
    ] {
        if let Some(index) = stripped.find(marker) {
            let module = &stripped[..index];
            let rest = &stripped[index + marker.len()..];
            let rest_ok = match which {
                MatrixPart::A | MatrixPart::B => {
                    rest.is_empty()
                        || rest == ".weight"
                        || rest
                            .strip_prefix('.')
                            .map(|tail| tail.ends_with(".weight") || !tail.contains('.'))
                            .unwrap_or(false)
                }
                MatrixPart::Magnitude => true,
            };
            if rest_ok && !module.is_empty() {
                return PeftKey::Matrix {
                    base_tensor: format!("{module}.weight"),
                    which,
                };
            }
        }
    }
    PeftKey::Other
}

// ---------------------------------------------------------------------------
// Write / read
// ---------------------------------------------------------------------------

/// Options for one adapter write.
#[derive(Clone, Debug, Default)]
pub struct AdapterWriteOptions {
    /// Force one strategy for the adapter tensor objects. `None` uses the
    /// planner.
    pub strategy: Option<TensorStrategy>,
    /// Record a provenance receipt linking the adapter manifest to its base
    /// manifest input.
    pub record_provenance: bool,
    /// Deterministic identity seed for the adapter directory and tensors.
    pub deterministic_seed: Option<[u8; 32]>,
}

/// Result of an adapter write.
#[derive(Clone, Debug)]
pub struct AdapterWriteReport {
    pub name: String,
    pub adapter_id: Hash32,
    pub manifest_object: Hash32,
    pub manifest_content: Hash32,
    pub manifest_bytes: u64,
    pub targets: usize,
    /// Exact bytes of the adapter arithmetic (A, B and magnitude payloads).
    pub adapter_bytes: u64,
    pub objects: usize,
    pub base: BaseRef,
    pub provenance: Option<Hash32>,
}

fn seed_from(label: &str) -> [u8; 32] {
    *identity::h("weft/adapter/seed/v1", label.as_bytes()).as_bytes()
}

fn entry_child(store: &Store, parent: Hash32, name: &str) -> Option<Hash32> {
    store
        .current_state()
        .entries
        .values()
        .find(|entry| entry.parent == parent && entry.name == name.as_bytes())
        .map(|entry| entry.target)
}

fn ensure_dir(
    store: &mut Store,
    parent: Hash32,
    name: &str,
    seed: Option<&[u8; 32]>,
) -> Result<Hash32> {
    if let Some(id) = entry_child(store, parent, name) {
        if store.object_meta(id)?.kind != ObjectKind::Dir {
            return Err(Error::not_dir(format!(
                "`{name}` exists and is not a directory"
            )));
        }
        return Ok(id);
    }
    let (uid, gid) = (store.options().owner_uid, store.options().owner_gid);
    match seed {
        Some(seed) => store
            .mkdir_seeded(parent, name.as_bytes(), 0o755, uid, gid, seed)
            .map_err(|err| {
                if err.class() == Class::Exists {
                    Error::exists(format!("directory `{name}` appeared concurrently"))
                } else {
                    err
                }
            }),
        None => store.mkdir(parent, name.as_bytes(), 0o755, uid, gid),
    }
}

impl Store {
    /// Write one adapter from an already-parsed PEFT `adapter_model.safetensors`.
    ///
    /// Every A/B/magnitude tensor is stored as its own object through the
    /// tensor planner; the base is referenced by content root only. Nothing is
    /// merged here: see [`Store::merge_adapter`].
    #[allow(clippy::too_many_arguments)]
    pub fn write_adapter(
        &mut self,
        name: &str,
        base_checkpoint: &str,
        source: &AdapterSource,
        kind: AdapterKind,
        rank: u64,
        alpha: f64,
        use_rslora: bool,
        bias: &str,
        target_modules: &[String],
        model: &SafetensorsFile,
        options: &AdapterWriteOptions,
    ) -> Result<AdapterWriteReport> {
        self.require_writable()?;
        crate::state::validate_name_bytes(name.as_bytes())?;
        if rank == 0 {
            return Err(Error::invalid_argument("adapter rank must be at least 1"));
        }
        if !alpha.is_finite() {
            return Err(Error::invalid_argument(
                "adapter alpha must be a finite number",
            ));
        }
        let scaling = if use_rslora {
            alpha / (rank as f64).sqrt()
        } else {
            alpha / rank as f64
        };
        if !scaling.is_finite() {
            return Err(Error::invalid_argument(
                "adapter scaling must be a finite number",
            ));
        }
        let loaded = self.load_checkpoint_manifest(base_checkpoint)?;
        let base = self.base_ref(base_checkpoint, &loaded)?;

        let mut groups: BTreeMap<
            String,
            (
                Option<&SafetensorsTensor>,
                Option<&SafetensorsTensor>,
                Option<&SafetensorsTensor>,
            ),
        > = BTreeMap::new();
        let mut others: BTreeSet<String> = BTreeSet::new();
        for tensor in &model.tensors {
            match map_peft_key(&tensor.name) {
                PeftKey::Matrix { base_tensor, which } => {
                    let slot = groups.entry(base_tensor).or_default();
                    let target = match which {
                        MatrixPart::A => &mut slot.0,
                        MatrixPart::B => &mut slot.1,
                        MatrixPart::Magnitude => &mut slot.2,
                    };
                    if target.is_some() {
                        return Err(Error::malformed(
                            "adapter container has duplicate matrices for one module",
                        ));
                    }
                    *target = Some(tensor);
                }
                PeftKey::Other => {
                    others.insert(tensor.name.clone());
                }
            }
        }
        if !others.is_empty() {
            return Err(Error::malformed(format!(
                "adapter container carries tensors that are not LoRA/DoRA matrices ({}); refusing to drop them silently",
                others.into_iter().collect::<Vec<_>>().join(", ")
            )));
        }
        if groups.is_empty() {
            return Err(Error::malformed(
                "adapter container has no LoRA/DoRA matrices",
            ));
        }

        let base_by_name: BTreeMap<&str, &crate::checkpoint::CheckpointTensor> = loaded
            .manifest
            .tensors
            .iter()
            .map(|tensor| (tensor.name.as_str(), tensor))
            .collect();

        let seed = options
            .deterministic_seed
            .unwrap_or_else(|| seed_from(name));
        let adapters_root = ensure_dir(self, State::ROOT_ID, ADAPTER_ROOT, None)?;
        let dir = ensure_dir(self, adapters_root, name, Some(&seed))?;
        let tensor_dir = ensure_dir(self, dir, "t", Some(&seed_from(&format!("{name}/t"))))?;

        let (uid, gid) = (self.options().owner_uid, self.options().owner_gid);
        let mut targets = Vec::with_capacity(groups.len());
        let mut adapter_bytes = 0u64;
        let mut objects = 0usize;
        for (base_tensor, (a, b, magnitude)) in groups {
            let base_entry = base_by_name.get(base_tensor.as_str()).ok_or_else(|| {
                Error::not_found(format!(
                    "adapter target `{base_tensor}` is not a tensor of base checkpoint `{base_checkpoint}`"
                ))
            })?;
            let (a, b) = match (a, b) {
                (Some(a), Some(b)) => (a, b),
                _ => {
                    return Err(Error::malformed(format!(
                        "adapter target `{base_tensor}` is missing its A or B matrix"
                    )))
                }
            };
            if kind == AdapterKind::Dora && magnitude.is_none() {
                return Err(Error::malformed(format!(
                    "DoRA target `{base_tensor}` has no lora_magnitude_vector"
                )));
            }
            let base_shape = &base_entry.shape;
            if base_shape.len() != 2 {
                return Err(Error::invalid_argument(format!(
                    "adapter target `{base_tensor}` is rank {}; only rank-2 weight matrices are supported",
                    base_shape.len()
                )));
            }
            let (out_dim, in_dim) = (base_shape[0], base_shape[1]);
            if a.shape.len() != 2 || b.shape.len() != 2 {
                return Err(Error::malformed(format!(
                    "adapter matrices for `{base_tensor}` must be rank 2"
                )));
            }
            if a.shape[0] != rank || b.shape[1] != rank {
                return Err(Error::malformed(format!(
                    "adapter target `{base_tensor}` declares rank {rank} but A is {:?} and B is {:?}",
                    a.shape, b.shape
                )));
            }
            if a.shape[1] != in_dim || b.shape[0] != out_dim {
                return Err(Error::malformed(format!(
                    "adapter target `{base_tensor}` matrices do not compose with base shape {:?} (A {:?}, B {:?})",
                    base_shape, a.shape, b.shape
                )));
            }
            if let Some(magnitude) = magnitude {
                if magnitude.shape.len() != 1 || magnitude.shape[0] != out_dim {
                    return Err(Error::malformed(format!(
                        "DoRA magnitude for `{base_tensor}` must have shape [{out_dim}]"
                    )));
                }
            }

            let mut write_one =
                |tensor: &SafetensorsTensor| -> Result<(TensorRef, u64, (u64, u64))> {
                    crate::representation::TensorElementDtype::parse(&tensor.dtype)?;
                    let bytes = model.read_tensor(tensor)?;
                    let spec = TensorWriteSpec {
                        strategy_hint: options.strategy,
                        ..TensorWriteSpec::default()
                    };
                    let slug = tensor
                        .name
                        .replace("base_model.model.", "")
                        .replace(['.', '/'], "_");
                    let object = self.write_tensor_file_with_spec_seeded(
                        tensor_dir,
                        format!("{slug}.t").as_bytes(),
                        &tensor.dtype,
                        &tensor.shape,
                        &bytes,
                        &TensorLayoutSpec::row_major(),
                        &spec,
                        &seed_from(&format!("{name}/{slug}")),
                    )?;
                    let meta = self.object_meta(object)?;
                    Ok((
                        TensorRef {
                            object,
                            content: meta.content,
                            dtype: tensor.dtype.clone(),
                            shape: tensor.shape.clone(),
                        },
                        bytes.len() as u64,
                        (tensor.begin, tensor.end),
                    ))
                };

            let (a_ref, a_bytes, a_offsets) = write_one(a)?;
            let (b_ref, b_bytes, b_offsets) = write_one(b)?;
            adapter_bytes += a_bytes + b_bytes;
            objects += 2;
            let (magnitude_ref, magnitude_offsets) = if let Some(magnitude) = magnitude {
                let (reference, magnitude_bytes, offsets) = write_one(magnitude)?;
                adapter_bytes += magnitude_bytes;
                objects += 1;
                (Some(reference), Some(offsets))
            } else {
                (None, None)
            };
            targets.push(AdapterTarget {
                base_tensor,
                base_content: base_entry.content_root,
                a: a_ref,
                b: b_ref,
                magnitude: magnitude_ref,
                source_offsets: Some(SourceOffsets {
                    a: a_offsets,
                    b: b_offsets,
                    magnitude: magnitude_offsets,
                }),
            });
        }

        let mut target_modules = target_modules.to_vec();
        target_modules.sort();
        target_modules.dedup();
        let manifest = AdapterManifest {
            name: name.to_owned(),
            kind,
            rank,
            alpha_bits: alpha.to_bits(),
            scaling_bits: scaling.to_bits(),
            use_rslora,
            bias: bias.to_owned(),
            target_modules,
            source: source.clone(),
            source_container: Some(SourceContainer {
                header: model.header_bytes.clone(),
                file_bytes: model.file_bytes,
            }),
            base: base.clone(),
            targets,
        };
        manifest.validate()?;
        let bytes = manifest.encode()?;
        let manifest_object = self.create_file_seeded(
            dir,
            b"manifest",
            &bytes,
            0o644,
            uid,
            gid,
            BTreeMap::new(),
            &seed_from(&format!("{name}/manifest")),
        )?;
        let manifest_meta = self.object_meta(manifest_object)?;
        let provenance = if options.record_provenance {
            Some(self.record_adapter_provenance(&manifest, &loaded, manifest_object)?)
        } else {
            None
        };

        Ok(AdapterWriteReport {
            name: name.to_owned(),
            adapter_id: manifest.id()?,
            manifest_object,
            manifest_content: manifest_meta.content,
            manifest_bytes: bytes.len() as u64,
            targets: manifest.targets.len(),
            adapter_bytes,
            objects,
            base,
            provenance,
        })
    }

    /// The exact base reference of one checkpoint directory in this store.
    pub fn checkpoint_base_ref(&self, checkpoint: &str) -> Result<BaseRef> {
        let loaded = self.load_checkpoint_manifest(checkpoint)?;
        self.base_ref(checkpoint, &loaded)
    }

    fn base_ref(&self, checkpoint: &str, loaded: &LoadedCheckpointManifest) -> Result<BaseRef> {
        let meta = self.object_meta(loaded.manifest_id)?;
        let source_sha256 = loaded.manifest.source_sha256.ok_or_else(|| {
            Error::unsupported_representation(format!(
                "base checkpoint `{checkpoint}` has no source container digest, so an adapter cannot name it exactly"
            ))
        })?;
        Ok(BaseRef {
            checkpoint: checkpoint.to_owned(),
            manifest_content: meta.content,
            manifest_root: loaded.manifest_root,
            container_sha256: source_sha256,
            tensor_count: loaded.manifest.tensors.len() as u64,
            element_bytes: loaded.manifest.element_bytes,
        })
    }

    fn record_adapter_provenance(
        &mut self,
        manifest: &AdapterManifest,
        base: &LoadedCheckpointManifest,
        adapter_manifest_object: Hash32,
    ) -> Result<Hash32> {
        let base_meta = self.object_meta(base.manifest_id)?;
        let adapter_meta = self.object_meta(adapter_manifest_object)?;
        let mut params = BTreeMap::new();
        params.insert("name".to_owned(), Value::Str(manifest.name.clone()));
        params.insert(
            "kind".to_owned(),
            Value::Str(manifest.kind.as_str().to_owned()),
        );
        params.insert("rank".to_owned(), Value::U64(manifest.rank));
        params.insert(
            "base_checkpoint".to_owned(),
            Value::Str(manifest.base.checkpoint.clone()),
        );
        params.insert(
            "targets".to_owned(),
            Value::U64(manifest.targets.len() as u64),
        );
        let record = ProvenanceRecord::new(
            "adapter-ingest",
            params,
            vec![
                ProvenanceInput::new(
                    "base-checkpoint-manifest",
                    base_meta.content,
                    Some(base.manifest_id),
                ),
                ProvenanceInput::new(
                    "adapter-source",
                    Hash32::from_bytes(manifest.source.file_sha256),
                    None,
                ),
            ],
            vec![adapter_meta.content],
            ProvenanceEnvironment::new("weftfs", env!("CARGO_PKG_VERSION")),
            Determinism::Exact,
            ProvenancePrincipal::new(self.options().owner_uid, self.options().owner_gid),
            self.clock_now_ns(),
        )?;
        self.record_provenance(&[adapter_manifest_object], &record)
    }

    /// Read one adapter manifest by directory name.
    pub fn load_adapter(&self, name: &str) -> Result<AdapterManifest> {
        let bytes = self.read_object(self.adapter_manifest_object(name)?)?;
        let manifest = AdapterManifest::decode(&bytes)?;
        if manifest.name != name {
            return Err(Error::corrupt(format!(
                "adapter manifest in `{name}` declares name `{}`",
                manifest.name
            )));
        }
        Ok(manifest)
    }

    /// Object id of an adapter's manifest file.
    pub fn adapter_manifest_object(&self, name: &str) -> Result<Hash32> {
        self.resolve_path(&format!("/{ADAPTER_ROOT}/{name}/manifest"))
    }

    /// Object id of a merged view's manifest file.
    pub fn merged_view_manifest_object(&self, name: &str) -> Result<Hash32> {
        self.resolve_path(&format!("/{MERGED_VIEW_ROOT}/{name}/manifest"))
    }

    /// Read one merged-view manifest by directory name.
    pub fn load_merged_view(&self, name: &str) -> Result<MergedViewManifest> {
        let bytes = self.read_object(self.merged_view_manifest_object(name)?)?;
        let manifest = MergedViewManifest::decode(&bytes)?;
        if manifest.name != name {
            return Err(Error::corrupt(format!(
                "merged-view manifest in `{name}` declares name `{}`",
                manifest.name
            )));
        }
        Ok(manifest)
    }

    /// Adapter directory names, sorted.
    pub fn list_adapters(&self) -> Result<Vec<String>> {
        let root = match self.lookup(State::ROOT_ID, ADAPTER_ROOT.as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut names = Vec::new();
        for entry in self.list_dir(root)? {
            if self.object_meta(entry.target)?.kind == ObjectKind::Dir {
                if let Ok(name) = String::from_utf8(entry.name.clone()) {
                    names.push(name);
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// Verify an adapter: the base still resolves to the recorded manifest
    /// root, every base tensor still has the recorded content root, and every
    /// adapter tensor object still hashes to the recorded content root.
    pub fn verify_adapter(&self, name: &str) -> Result<AdapterVerification> {
        let manifest = self.load_adapter(name)?;
        let loaded = self.load_checkpoint_manifest(&manifest.base.checkpoint)?;
        if loaded.manifest_root != manifest.base.manifest_root {
            return Err(Error::dependency_mismatch(format!(
                "adapter `{name}` names base manifest root {} but `{}` now resolves to {}",
                manifest.base.manifest_root, manifest.base.checkpoint, loaded.manifest_root
            )));
        }
        let meta = self.object_meta(loaded.manifest_id)?;
        if meta.content != manifest.base.manifest_content {
            return Err(Error::dependency_mismatch(format!(
                "adapter `{name}` names base manifest content {} but the live manifest content is {}",
                manifest.base.manifest_content, meta.content
            )));
        }
        let by_name: BTreeMap<&str, &crate::checkpoint::CheckpointTensor> = loaded
            .manifest
            .tensors
            .iter()
            .map(|tensor| (tensor.name.as_str(), tensor))
            .collect();
        let mut checked = 0usize;
        for target in &manifest.targets {
            let base = by_name.get(target.base_tensor.as_str()).ok_or_else(|| {
                Error::dependency_mismatch(format!(
                    "adapter `{name}` names base tensor `{}` which the base checkpoint does not have",
                    target.base_tensor
                ))
            })?;
            if base.content_root != target.base_content {
                return Err(Error::dependency_mismatch(format!(
                    "adapter `{name}` names base tensor `{}` content {} but the base records {}",
                    target.base_tensor, target.base_content, base.content_root
                )));
            }
            for (label, reference) in [
                ("a", Some(&target.a)),
                ("b", Some(&target.b)),
                ("magnitude", target.magnitude.as_ref()),
            ] {
                let Some(reference) = reference else { continue };
                let meta = self.object_meta(reference.object)?;
                if meta.content != reference.content {
                    return Err(Error::corrupt(format!(
                        "adapter `{name}` tensor `{}` {label} object {} content is {} but the manifest records {}",
                        target.base_tensor, reference.object, meta.content, reference.content
                    )));
                }
                if meta.size != reference.logical_bytes()? {
                    return Err(Error::corrupt(format!(
                        "adapter `{name}` tensor `{}` {label} is {} bytes, expected {}",
                        target.base_tensor,
                        meta.size,
                        reference.logical_bytes()?
                    )));
                }
                checked += 1;
            }
            if let (Some(container), Some(offsets)) =
                (&manifest.source_container, &target.source_offsets)
            {
                let data_len = container.file_bytes - container.header.len() as u64;
                let rows: [(&str, Option<&TensorRef>, Option<(u64, u64)>); 3] = [
                    ("a", Some(&target.a), Some(offsets.a)),
                    ("b", Some(&target.b), Some(offsets.b)),
                    ("magnitude", target.magnitude.as_ref(), offsets.magnitude),
                ];
                for (label, reference, range) in rows {
                    let (Some(reference), Some((begin, end))) = (reference, range) else {
                        continue;
                    };
                    if reference.logical_bytes()? != end - begin || end > data_len {
                        return Err(Error::corrupt(format!(
                            "adapter `{name}` tensor `{}` {label} offsets [{begin}, {end}) do not describe its {}-byte tensor inside a {data_len}-byte data section",
                            target.base_tensor,
                            reference.logical_bytes()?
                        )));
                    }
                }
            }
        }
        Ok(AdapterVerification {
            adapter: manifest.id()?,
            targets: manifest.targets.len(),
            tensor_objects: checked,
            base_manifest_root: loaded.manifest_root,
        })
    }

    /// Read one adapter tensor's exact bytes back through the store.
    pub fn read_adapter_tensor(
        &self,
        name: &str,
        base_tensor: &str,
        part: &str,
    ) -> Result<Vec<u8>> {
        let manifest = self.load_adapter(name)?;
        let target = manifest
            .targets
            .iter()
            .find(|target| target.base_tensor == base_tensor)
            .ok_or_else(|| {
                Error::not_found(format!("adapter `{name}` has no target `{base_tensor}`"))
            })?;
        let reference = match part {
            "a" => &target.a,
            "b" => &target.b,
            "magnitude" => target.magnitude.as_ref().ok_or_else(|| {
                Error::not_found(format!(
                    "adapter `{name}` target `{base_tensor}` has no magnitude"
                ))
            })?,
            other => {
                return Err(Error::invalid_argument(format!(
                    "adapter tensor part must be `a`, `b` or `magnitude` (got `{other}`)"
                )))
            }
        };
        let bytes = self.read_object(reference.object)?;
        if self.object_meta(reference.object)?.content != reference.content {
            return Err(Error::corrupt(format!(
                "adapter tensor object {} failed its content check",
                reference.object
            )));
        }
        Ok(bytes)
    }

    /// Restore an adapter as an `adapter_model.safetensors` container.
    ///
    /// When the manifest records the source container (every adapter written by
    /// [`Store::write_adapter`] does), the original file is reproduced
    /// **byte-for-byte**: the recorded header is written verbatim, each tensor is
    /// placed at its recorded `data_offsets`, and the result must hash to the
    /// recorded source SHA-256 or the restore fails closed. Otherwise a canonical
    /// container carrying the same tensors is written and the report says
    /// `byte_exact: false`.
    pub fn restore_adapter<W: Write>(
        &self,
        name: &str,
        mut writer: W,
    ) -> Result<AdapterRestoreReport> {
        let manifest = self.load_adapter(name)?;
        let verification = self.verify_adapter(name)?;
        if let Some(container) = &manifest.source_container {
            let mut entries: Vec<(u64, usize, &TensorRef, Vec<u8>)> = Vec::new();
            let mut total = 0u64;
            let mut ranges: Vec<(u64, u64)> = Vec::new();
            for target in &manifest.targets {
                let offsets = target.source_offsets.as_ref().ok_or_else(|| {
                    Error::corrupt(format!(
                        "adapter `{name}` records a source container but not `{}` offsets",
                        target.base_tensor
                    ))
                })?;
                let rows: [(&str, Option<&TensorRef>, Option<(u64, u64)>); 3] = [
                    ("a", Some(&target.a), Some(offsets.a)),
                    ("b", Some(&target.b), Some(offsets.b)),
                    ("magnitude", target.magnitude.as_ref(), offsets.magnitude),
                ];
                for (label, reference, range) in rows {
                    let (Some(reference), Some((begin, _end))) = (reference, range) else {
                        continue;
                    };
                    let bytes = self.read_object(reference.object)?;
                    if self.object_meta(reference.object)?.content != reference.content {
                        return Err(Error::corrupt(format!(
                            "adapter tensor object {} failed its content check during restore",
                            reference.object
                        )));
                    }
                    if bytes.len() as u64 != reference.logical_bytes()? {
                        return Err(Error::corrupt(format!(
                            "adapter tensor object {} read {} bytes, expected {}",
                            reference.object,
                            bytes.len(),
                            reference.logical_bytes()?
                        )));
                    }
                    total += bytes.len() as u64;
                    ranges.push((begin, begin + bytes.len() as u64));
                    entries.push((begin, bytes.len(), reference, bytes));
                }
            }
            let data_len = container.file_bytes - container.header.len() as u64;
            if total != data_len {
                return Err(Error::corrupt(format!(
                    "adapter `{name}` records {total} tensor bytes but its source container has a {data_len}-byte data section; the source cannot be reproduced byte-exactly"
                )));
            }
            // The recorded ranges must tile the data section exactly. A gap
            // would mean bytes nobody stored; an overlap would mean two tensors
            // claiming one range. Refusing is the only exact answer.
            ranges.sort_unstable();
            let mut cursor = 0u64;
            for (begin, end) in &ranges {
                if *begin != cursor {
                    return Err(Error::corrupt(format!(
                        "adapter `{name}` source data section is not covered exactly (gap or overlap at {cursor}..{begin}); refusing to guess the missing bytes"
                    )));
                }
                cursor = *end;
            }
            if cursor != data_len {
                return Err(Error::corrupt(format!(
                    "adapter `{name}` source data section ends at {cursor}, expected {data_len}"
                )));
            }
            let mut out = container.header.clone();
            out.resize(container.file_bytes as usize, 0);
            for (begin, len, _reference, bytes) in &entries {
                let start = container.header.len() + *begin as usize;
                out[start..start + len].copy_from_slice(bytes);
            }
            let digest = safetensors::sha256(&out);
            if digest != manifest.source.file_sha256 {
                return Err(Error::corrupt(format!(
                    "adapter `{name}` restored container hashes to {} but the manifest records the source as {}; the source container cannot be reproduced byte-exactly",
                    hex(&digest),
                    hex(&manifest.source.file_sha256)
                )));
            }
            writer
                .write_all(&out)
                .map_err(|err| Error::io(format!("adapter restore write: {err}")))?;
            return Ok(AdapterRestoreReport {
                adapter: verification.adapter,
                targets: verification.targets,
                tensors: entries.len(),
                container_bytes: out.len() as u64,
                container_sha256: digest,
                byte_exact: true,
            });
        }

        let mut entries: Vec<(String, String, Vec<u64>, Vec<u8>)> = Vec::new();
        for target in &manifest.targets {
            let prefix = target.base_tensor.strip_suffix(".weight").ok_or_else(|| {
                Error::corrupt(format!(
                    "adapter target `{}` does not end in .weight",
                    target.base_tensor
                ))
            })?;
            let peft_prefix = format!("base_model.model.{prefix}");
            for (suffix, reference) in [
                (".lora_A.weight", Some(&target.a)),
                (".lora_B.weight", Some(&target.b)),
                (".lora_magnitude_vector", target.magnitude.as_ref()),
            ] {
                let Some(reference) = reference else { continue };
                let bytes = self.read_object(reference.object)?;
                if self.object_meta(reference.object)?.content != reference.content {
                    return Err(Error::corrupt(format!(
                        "adapter tensor object {} failed its content check during restore",
                        reference.object
                    )));
                }
                entries.push((
                    format!("{peft_prefix}{suffix}"),
                    reference.dtype.clone(),
                    reference.shape.clone(),
                    bytes,
                ));
            }
        }
        let refs: Vec<safetensors::SafetensorsEntry<'_>> = entries
            .iter()
            .map(|(name, dtype, shape, data)| safetensors::SafetensorsEntry {
                name,
                dtype,
                shape,
                data,
            })
            .collect();
        let container = safetensors::encode_file(&refs, None)?;
        writer
            .write_all(&container)
            .map_err(|err| Error::io(format!("adapter restore write: {err}")))?;
        Ok(AdapterRestoreReport {
            adapter: verification.adapter,
            targets: verification.targets,
            tensors: entries.len(),
            container_bytes: container.len() as u64,
            container_sha256: safetensors::sha256(&container),
            byte_exact: false,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Result of [`Store::verify_adapter`].
#[derive(Clone, Debug)]
pub struct AdapterVerification {
    pub adapter: Hash32,
    pub targets: usize,
    pub tensor_objects: usize,
    pub base_manifest_root: Hash32,
}

/// Result of [`Store::restore_adapter`].
#[derive(Clone, Debug)]
pub struct AdapterRestoreReport {
    pub adapter: Hash32,
    pub targets: usize,
    pub tensors: usize,
    pub container_bytes: u64,
    pub container_sha256: Sha256Digest,
    /// True when the written container is the exact source file, proven by its
    /// SHA-256 matching the manifest's recorded source digest.
    pub byte_exact: bool,
}

// ---------------------------------------------------------------------------
// Merging
// ---------------------------------------------------------------------------

/// Options for one merge.
#[derive(Clone, Debug, Default)]
pub struct MergeOptions {
    pub strategy: Option<TensorStrategy>,
    pub record_provenance: bool,
    pub deterministic_seed: Option<[u8; 32]>,
    /// Read every merged tensor back before publishing the view manifest.
    pub verify_root: bool,
}

/// Result of merging an adapter into a view.
#[derive(Clone, Debug)]
pub struct MergeReport {
    pub merged_view: Hash32,
    pub name: String,
    pub adapter: Hash32,
    pub merged_tensors: usize,
    pub base_tensors: usize,
    pub merged_logical_bytes: u64,
    pub manifest_object: Hash32,
    pub manifest_content: Hash32,
    pub manifest_bytes: u64,
    pub merge_ms: f64,
    pub provenance: Option<Hash32>,
}

impl Store {
    /// Apply an adapter to its base and publish a `weft.merged-view.v1`
    /// artifact.
    ///
    /// Tensors the adapter does not target are referenced from the base
    /// checkpoint by content root (never copied). Every merged tensor is
    /// recomputed here under the pinned [`MergeContract`]; the resulting bytes
    /// are published through the tensor planner and their own content roots are
    /// recorded, so the view has an identity that is neither the base's nor the
    /// adapter's.
    pub fn merge_adapter(&mut self, name: &str, options: &MergeOptions) -> Result<MergeReport> {
        self.require_writable()?;
        crate::state::validate_name_bytes(name.as_bytes())?;
        let started = std::time::Instant::now();
        let manifest = self.load_adapter(name)?;
        let loaded = self.load_checkpoint_manifest(&manifest.base.checkpoint)?;
        if loaded.manifest_root != manifest.base.manifest_root {
            return Err(Error::dependency_mismatch(format!(
                "adapter `{name}` names base manifest root {} but the base now resolves to {}",
                manifest.base.manifest_root, loaded.manifest_root
            )));
        }
        let merged_root = ensure_dir(self, State::ROOT_ID, MERGED_VIEW_ROOT, None)?;
        let seed = options
            .deterministic_seed
            .unwrap_or_else(|| seed_from(&format!("merged/{name}")));
        let dir = ensure_dir(self, merged_root, name, Some(&seed))?;
        let tensor_dir = ensure_dir(
            self,
            dir,
            "t",
            Some(&seed_from(&format!("merged/{name}/t"))),
        )?;
        let (uid, gid) = (self.options().owner_uid, self.options().owner_gid);

        let target_by_name: BTreeMap<&str, &AdapterTarget> = manifest
            .targets
            .iter()
            .map(|target| (target.base_tensor.as_str(), target))
            .collect();

        let mut output_dtype: Option<String> = None;
        for tensor in &loaded.manifest.tensors {
            match &output_dtype {
                Some(existing) if *existing != tensor.dtype => {
                    return Err(Error::unsupported_representation(format!(
                        "base checkpoint `{}` mixes tensor dtypes (`{existing}` and `{}`); a merge contract names exactly one output dtype",
                        manifest.base.checkpoint, tensor.dtype
                    )))
                }
                Some(_) => {}
                None => output_dtype = Some(tensor.dtype.clone()),
            }
        }
        let output_dtype = output_dtype
            .ok_or_else(|| Error::malformed("base checkpoint has no tensors to merge"))?;
        let contract = MergeContract::for_kind(manifest.kind, manifest.scaling(), &output_dtype);

        let mut tensors = Vec::with_capacity(loaded.manifest.tensors.len());
        let mut merged_tensors = 0usize;
        let mut merged_logical_bytes = 0u64;
        for tensor in &loaded.manifest.tensors {
            let Some(target) = target_by_name.get(tensor.name.as_str()) else {
                tensors.push(MergedTensorEntry {
                    name: tensor.name.clone(),
                    merged: false,
                    content: tensor.content_root,
                    object: None,
                    dtype: tensor.dtype.clone(),
                    shape: tensor.shape.clone(),
                });
                continue;
            };
            if tensor.content_root != target.base_content {
                return Err(Error::dependency_mismatch(format!(
                    "base tensor `{}` content changed since the adapter was written",
                    tensor.name
                )));
            }
            let base_bytes = self
                .restore_named_tensor_with_manifest(
                    None,
                    &manifest.base.checkpoint,
                    &loaded.manifest,
                    &tensor.name,
                    None,
                    None,
                )?
                .bytes;
            let a_bytes = self.read_object(target.a.object)?;
            let b_bytes = self.read_object(target.b.object)?;
            let magnitude_bytes = match &target.magnitude {
                Some(reference) => Some(self.read_object(reference.object)?),
                None => None,
            };
            let magnitude_arg = match (&target.magnitude, magnitude_bytes.as_deref()) {
                (Some(reference), Some(bytes)) => {
                    Some((reference.dtype.as_str(), reference.shape.as_slice(), bytes))
                }
                _ => None,
            };
            let merged = merge_one_tensor(
                manifest.kind,
                manifest.scaling(),
                &tensor.dtype,
                &tensor.shape,
                &base_bytes,
                &target.a.dtype,
                &target.a.shape,
                &a_bytes,
                &target.b.dtype,
                &target.b.shape,
                &b_bytes,
                magnitude_arg,
            )?;
            let spec = TensorWriteSpec {
                strategy_hint: options.strategy,
                ..TensorWriteSpec::default()
            };
            let slug = tensor.name.replace(['.', '/'], "_");
            let object = self.write_tensor_file_with_spec_seeded(
                tensor_dir,
                format!("{slug}.m").as_bytes(),
                &tensor.dtype,
                &tensor.shape,
                &merged,
                &TensorLayoutSpec::row_major(),
                &spec,
                &seed_from(&format!("merged/{name}/{slug}")),
            )?;
            let meta = self.object_meta(object)?;
            if meta.size != merged.len() as u64 {
                return Err(Error::corrupt(format!(
                    "merged tensor `{}` stored {} bytes, expected {}",
                    tensor.name,
                    meta.size,
                    merged.len()
                )));
            }
            if options.verify_root && self.read_object(object)? != merged {
                return Err(Error::corrupt(format!(
                    "merged tensor `{}` did not read back byte-identically",
                    tensor.name
                )));
            }
            merged_tensors += 1;
            merged_logical_bytes += merged.len() as u64;
            tensors.push(MergedTensorEntry {
                name: tensor.name.clone(),
                merged: true,
                content: meta.content,
                object: Some(object),
                dtype: tensor.dtype.clone(),
                shape: tensor.shape.clone(),
            });
        }
        tensors.sort_by(|a, b| a.name.cmp(&b.name));
        let view = MergedViewManifest {
            name: name.to_owned(),
            adapter: manifest.id()?,
            adapter_manifest_content: self
                .object_meta(self.adapter_manifest_object(name)?)?
                .content,
            base: manifest.base.clone(),
            contract,
            tensors,
        };
        view.validate()?;
        let bytes = view.encode()?;
        let manifest_object = self.create_file_seeded(
            dir,
            b"manifest",
            &bytes,
            0o644,
            uid,
            gid,
            BTreeMap::new(),
            &seed_from(&format!("merged/{name}/manifest")),
        )?;
        let manifest_meta = self.object_meta(manifest_object)?;
        let provenance = if options.record_provenance {
            let adapter_object = self.adapter_manifest_object(name)?;
            let mut params = BTreeMap::new();
            params.insert("name".to_owned(), Value::Str(name.to_owned()));
            params.insert("adapter".to_owned(), Value::Str(manifest.id()?.to_hex()));
            params.insert(
                "contract".to_owned(),
                Value::Str(view.contract.order.clone()),
            );
            params.insert(
                "merged_tensors".to_owned(),
                Value::U64(merged_tensors as u64),
            );
            let record = ProvenanceRecord::new(
                "adapter-merge",
                params,
                vec![
                    ProvenanceInput::new(
                        "adapter-manifest",
                        self.object_meta(adapter_object)?.content,
                        Some(adapter_object),
                    ),
                    ProvenanceInput::new(
                        "base-checkpoint-manifest",
                        manifest.base.manifest_content,
                        Some(loaded.manifest_id),
                    ),
                ],
                vec![manifest_meta.content],
                ProvenanceEnvironment::new("weftfs", env!("CARGO_PKG_VERSION")),
                Determinism::Exact,
                ProvenancePrincipal::new(self.options().owner_uid, self.options().owner_gid),
                self.clock_now_ns(),
            )?;
            Some(self.record_provenance(&[manifest_object], &record)?)
        } else {
            None
        };

        Ok(MergeReport {
            merged_view: view.id()?,
            name: name.to_owned(),
            adapter: manifest.id()?,
            merged_tensors,
            base_tensors: loaded.manifest.tensors.len(),
            merged_logical_bytes,
            manifest_object,
            manifest_content: manifest_meta.content,
            manifest_bytes: bytes.len() as u64,
            merge_ms: started.elapsed().as_secs_f64() * 1000.0,
            provenance,
        })
    }

    /// Verify a merged view: adapter and base still resolve to their recorded
    /// roots, every base-referenced tensor still has the base's content root,
    /// and every merged object hashes to its recorded content root.
    pub fn verify_merged_view(&self, name: &str) -> Result<MergedViewVerification> {
        let view = self.load_merged_view(name)?;
        let adapter = self.load_adapter(&view.name)?;
        if adapter.id()? != view.adapter {
            return Err(Error::dependency_mismatch(format!(
                "merged view `{name}` names adapter {} but `{}` now has id {}",
                view.adapter,
                view.name,
                adapter.id()?
            )));
        }
        let loaded = self.load_checkpoint_manifest(&view.base.checkpoint)?;
        if loaded.manifest_root != view.base.manifest_root {
            return Err(Error::dependency_mismatch(format!(
                "merged view `{name}` names base manifest root {} but the base now resolves to {}",
                view.base.manifest_root, loaded.manifest_root
            )));
        }
        let base_names: BTreeSet<&str> = loaded
            .manifest
            .tensors
            .iter()
            .map(|tensor| tensor.name.as_str())
            .collect();
        let view_names: BTreeSet<&str> = view
            .tensors
            .iter()
            .map(|tensor| tensor.name.as_str())
            .collect();
        if base_names != view_names {
            return Err(Error::dependency_mismatch(format!(
                "merged view `{name}` does not cover the base tensor set exactly"
            )));
        }
        let base_by_name: BTreeMap<&str, &crate::checkpoint::CheckpointTensor> = loaded
            .manifest
            .tensors
            .iter()
            .map(|tensor| (tensor.name.as_str(), tensor))
            .collect();
        let mut merged_objects = 0usize;
        for entry in &view.tensors {
            if entry.merged {
                let object = entry
                    .object
                    .expect("validated: merged entries name an object");
                let meta = self.object_meta(object)?;
                if meta.content != entry.content {
                    return Err(Error::corrupt(format!(
                        "merged-view tensor `{}` object {object} has content {} but the manifest records {}",
                        entry.name, meta.content, entry.content
                    )));
                }
                merged_objects += 1;
            } else {
                let base = base_by_name.get(entry.name.as_str()).ok_or_else(|| {
                    Error::dependency_mismatch(format!(
                        "merged-view tensor `{}` is not a base tensor",
                        entry.name
                    ))
                })?;
                if base.content_root != entry.content {
                    return Err(Error::dependency_mismatch(format!(
                        "merged-view tensor `{}` records base content {} but the base records {}",
                        entry.name, entry.content, base.content_root
                    )));
                }
            }
        }
        Ok(MergedViewVerification {
            merged_view: view.id()?,
            tensors: view.tensors.len(),
            merged_tensors: merged_objects,
            base_tensors: view.tensors.len() - merged_objects,
        })
    }

    /// Restore a merged view as a complete safetensors container, streaming one
    /// tensor at a time. Every entry is verified before it is written.
    pub fn restore_merged_view<W: Write>(
        &self,
        name: &str,
        mut writer: W,
    ) -> Result<MergedViewRestoreReport> {
        let view = self.load_merged_view(name)?;
        self.verify_merged_view(name)?;
        let loaded = self.load_checkpoint_manifest(&view.base.checkpoint)?;
        let mut header_entries: Vec<(String, String, Vec<u64>, u64)> = Vec::new();
        let mut total = 0u64;
        for entry in &view.tensors {
            let element = crate::representation::TensorElementDtype::parse(&entry.dtype)?;
            let elements = entry
                .shape
                .iter()
                .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
                .ok_or_else(|| Error::limit_exceeded("merged-view tensor shape overflows"))?;
            let len = elements
                .checked_mul(element.element_bytes())
                .ok_or_else(|| Error::limit_exceeded("merged-view tensor bytes overflow"))?;
            total += len;
            header_entries.push((
                entry.name.clone(),
                entry.dtype.clone(),
                entry.shape.clone(),
                len,
            ));
        }
        let header_refs: Vec<(&str, &str, &[u64], u64)> = header_entries
            .iter()
            .map(|(name, dtype, shape, len)| {
                (name.as_str(), dtype.as_str(), shape.as_slice(), *len)
            })
            .collect();
        let header = safetensors::encode_header(&header_refs, None)?;
        writer
            .write_all(&header)
            .map_err(|err| Error::io(format!("merged-view restore header: {err}")))?;
        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut hasher, &header);
        for entry in &view.tensors {
            let bytes = if entry.merged {
                self.read_object(entry.object.expect("validated"))?
            } else {
                self.restore_named_tensor_with_manifest(
                    None,
                    &view.base.checkpoint,
                    &loaded.manifest,
                    &entry.name,
                    None,
                    None,
                )?
                .bytes
            };
            let expected = header_entries
                .iter()
                .find(|(name, _, _, _)| *name == entry.name)
                .map(|(_, _, _, len)| *len)
                .unwrap_or_default();
            if bytes.len() as u64 != expected {
                return Err(Error::corrupt(format!(
                    "merged-view tensor `{}` is {} bytes, expected {expected}",
                    entry.name,
                    bytes.len()
                )));
            }
            writer
                .write_all(&bytes)
                .map_err(|err| Error::io(format!("merged-view restore write: {err}")))?;
            sha2::Digest::update(&mut hasher, &bytes);
        }
        Ok(MergedViewRestoreReport {
            merged_view: view.id()?,
            tensors: view.tensors.len(),
            logical_bytes: total,
            container_bytes: header.len() as u64 + total,
            file_sha256: sha2::Digest::finalize(hasher).into(),
        })
    }
}

/// Result of [`Store::verify_merged_view`].
#[derive(Clone, Debug)]
pub struct MergedViewVerification {
    pub merged_view: Hash32,
    pub tensors: usize,
    pub merged_tensors: usize,
    pub base_tensors: usize,
}

/// Result of [`Store::restore_merged_view`].
#[derive(Clone, Debug)]
pub struct MergedViewRestoreReport {
    pub merged_view: Hash32,
    pub tensors: usize,
    pub logical_bytes: u64,
    pub container_bytes: u64,
    pub file_sha256: Sha256Digest,
}

/// Compute one merged tensor under the pinned contract.
///
/// `base_bytes` is canonical row-major element bytes. `A` is `[rank, in_dim]` and
/// `B` is `[out_dim, rank]`; both row-major. `magnitude` is `[out_dim]` for DoRA.
#[allow(clippy::too_many_arguments)]
pub fn merge_one_tensor(
    kind: AdapterKind,
    scaling: f64,
    base_dtype: &str,
    base_shape: &[u64],
    base_bytes: &[u8],
    a_dtype: &str,
    a_shape: &[u64],
    a_bytes: &[u8],
    b_dtype: &str,
    b_shape: &[u64],
    b_bytes: &[u8],
    magnitude: Option<(&str, &[u64], &[u8])>,
) -> Result<Vec<u8>> {
    if base_shape.len() != 2 || a_shape.len() != 2 || b_shape.len() != 2 {
        return Err(Error::invalid_argument(
            "adapter merge requires rank-2 base, A and B tensors",
        ));
    }
    let (out_dim, in_dim) = (base_shape[0] as usize, base_shape[1] as usize);
    let rank = a_shape[0] as usize;
    if b_shape[1] as usize != rank
        || b_shape[0] as usize != out_dim
        || a_shape[1] as usize != in_dim
    {
        return Err(Error::invalid_argument(format!(
            "adapter matrices do not compose: base {base_shape:?}, A {a_shape:?}, B {b_shape:?}"
        )));
    }
    let elements = out_dim
        .checked_mul(in_dim)
        .ok_or_else(|| Error::limit_exceeded("merged tensor element count overflows"))?;
    let base_element =
        crate::representation::TensorElementDtype::parse(base_dtype)?.element_bytes() as usize;
    if base_bytes.len() != elements * base_element {
        return Err(Error::malformed(format!(
            "base tensor is {} bytes, expected {} for {:?} x {base_dtype}",
            base_bytes.len(),
            elements * base_element,
            base_shape
        )));
    }
    let scale = scaling as f32;
    let mut a_values = vec![0f32; rank * in_dim];
    for index in 0..rank * in_dim {
        a_values[index] = element_as_f32(a_dtype, a_bytes, index)?;
    }
    let mut b_values = vec![0f32; out_dim * rank];
    for index in 0..out_dim * rank {
        b_values[index] = element_as_f32(b_dtype, b_bytes, index)?;
    }
    let magnitude_values = match magnitude {
        Some((dtype, shape, bytes)) => {
            if shape.len() != 1 || shape[0] as usize != out_dim {
                return Err(Error::invalid_argument(format!(
                    "DoRA magnitude shape {shape:?} does not match the {out_dim}-row base tensor"
                )));
            }
            let mut values = vec![0f32; out_dim];
            for index in 0..out_dim {
                values[index] = element_as_f32(dtype, bytes, index)?;
            }
            Some(values)
        }
        None => None,
    };
    if kind == AdapterKind::Dora && magnitude_values.is_none() {
        return Err(Error::invalid_argument(
            "DoRA merge requires a magnitude vector",
        ));
    }
    let mut out = Vec::with_capacity(elements * base_element);
    let mut delta = vec![0f32; in_dim];
    let mut row = vec![0f32; in_dim];
    for o in 0..out_dim {
        delta.iter_mut().for_each(|value| *value = 0.0);
        for k in 0..rank {
            let b = b_values[o * rank + k];
            if b == 0.0 {
                continue;
            }
            let a_row = &a_values[k * in_dim..(k + 1) * in_dim];
            for i in 0..in_dim {
                delta[i] += b * a_row[i];
            }
        }
        for i in 0..in_dim {
            let value = element_as_f32(base_dtype, base_bytes, o * in_dim + i)?;
            row[i] = value + delta[i] * scale;
        }
        if let Some(magnitude) = &magnitude_values {
            let mut sum_sq = 0f64;
            for value in &row {
                sum_sq += (*value as f64) * (*value as f64);
            }
            let norm = sum_sq.sqrt();
            if norm > 0.0 {
                let factor = (magnitude[o] as f64) / norm;
                for value in row.iter_mut() {
                    *value = ((*value as f64) * factor) as f32;
                }
            } else {
                row.iter_mut().for_each(|value| *value = 0.0);
            }
        }
        for value in &row {
            out.extend_from_slice(&f32_to_element(base_dtype, *value)?);
        }
    }
    if out.len() != elements * base_element {
        return Err(Error::corrupt(format!(
            "merged tensor has {} bytes, expected {}",
            out.len(),
            elements * base_element
        )));
    }
    Ok(out)
}
