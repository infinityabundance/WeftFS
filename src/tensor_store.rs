//! First-class WeftTensor physical representations in the store.
//!
//! A tensor object is stored through a v2 representation manifest of kind
//! `"tensor"` (frame modes 10-14) or, when the planner chooses a byte
//! representation, through the unchanged `"raw"`/`"codec"` kinds. Tensor
//! identity (`dtype`, `shape`, `LogicalTensorRoot`, physical layout) is carried
//! both in the tensor representation and in reserved object attributes, so a
//! tensor object written as raw/zstd codec bytes still has an exact, checkable
//! logical identity.
//!
//! The planner is deliberately small and deterministic: it encodes every
//! applicable candidate, prices it as
//!
//! ```text
//! cost = physical_bytes + lambda_ns * estimated_decode_ns
//! ```
//!
//! and picks the minimum (ties keep the fixed candidate order). `lambda_ns`
//! defaults to `0`, i.e. bytes-only. Estimated decode time is an explicit
//! integer model, not a measurement; the bench harness measures real restore
//! time. Only the chosen representation is written; the full cost table is
//! exposed by `Store::plan_tensor` / `weft plan --json` because the manifest
//! schema has no field for planner bookkeeping.

use std::collections::BTreeMap;

use weftcodec::dependency::MapResolver;
use weftcodec::frame::{Frame, Mode};
use weftcodec::tensor::{PhysicalTensor, Region, RegionRead, TensorLayout};
use weftcodec::wcb::Value;
use weftcodec::{Limits, SemanticPolicy};

use crate::error::{Error, Result};
use crate::format::{Manifest, Representation, RepresentationKind, TensorRepresentation};
use crate::identity::{self, Hash32};
use crate::recovery::{self, BlobKind};
use crate::representation::{TensorElementDtype, MAX_CODEC_DEPENDENCY_DEPTH};
use crate::state::ObjectKind;
use crate::store::Store;
use crate::wcb_util as wu;

/// Reserved object attribute holding the canonical tensor dtype name.
pub const ATTR_TENSOR_DTYPE: &str = "weft.tensor.dtype";
/// Reserved object attribute holding the logical shape as an array of u64.
pub const ATTR_TENSOR_SHAPE: &str = "weft.tensor.shape";
/// Reserved object attribute holding the `LogicalTensorRoot` as bytes32.
pub const ATTR_TENSOR_LOGICAL_ROOT: &str = "weft.tensor.logical_root";
/// Reserved object attribute holding the canonical `weft.tensor.layout.v1`
/// WCB bytes of the declared physical layout.
pub const ATTR_TENSOR_LAYOUT: &str = "weft.tensor.layout";
/// Reserved object attribute holding the chosen representation strategy name.
pub const ATTR_TENSOR_STRATEGY: &str = "weft.tensor.strategy";

/// Default bound on the semantic-delta candidate search.
pub const DEFAULT_TENSOR_MAX_CANDIDATES: usize = 16;

/// Canonical layout schema string (`WEFT-TENSOR.md` §4).
pub const TENSOR_LAYOUT_SCHEMA: &str = "weft.tensor.layout.v1";

/// A physical representation strategy the planner can consider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TensorStrategy {
    /// Concatenated raw chunk blobs (v2 `kind:"raw"`).
    Raw,
    /// One WeftCodec ZSTD frame (v2 `kind:"codec"`).
    Zstd,
    /// One mode-10 `TENSOR_SHARDED` frame.
    TensorSharded,
    /// One mode-11 `WEIGHT_DELTA` frame against the causal parent.
    WeightDelta,
    /// One mode-14 `SEMANTIC_DELTA` frame against a same-semantic-root base.
    SemanticDelta,
    /// One mode-12 `KV_CACHE` frame (readable; never produced by this planner).
    KvCache,
    /// One mode-13 `ACTIVATION_STREAM` frame (readable; never produced here).
    Activation,
}

impl TensorStrategy {
    /// Canonical `representation.strategy` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            TensorStrategy::Raw => "raw",
            TensorStrategy::Zstd => "zstd",
            TensorStrategy::TensorSharded => "tensor-sharded",
            TensorStrategy::WeightDelta => "weight-delta",
            TensorStrategy::SemanticDelta => "semantic-delta",
            TensorStrategy::KvCache => "kv-cache",
            TensorStrategy::Activation => "activation",
        }
    }

    /// Parse a canonical strategy spelling.
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "raw" => Ok(TensorStrategy::Raw),
            "zstd" => Ok(TensorStrategy::Zstd),
            "tensor-sharded" => Ok(TensorStrategy::TensorSharded),
            "weight-delta" => Ok(TensorStrategy::WeightDelta),
            "semantic-delta" => Ok(TensorStrategy::SemanticDelta),
            "kv-cache" => Ok(TensorStrategy::KvCache),
            "activation" => Ok(TensorStrategy::Activation),
            other => Err(Error::malformed(format!(
                "unknown tensor strategy `{other}`"
            ))),
        }
    }

    /// Frame mode this strategy requires, when it is frame-backed.
    pub fn frame_mode(self) -> Option<u32> {
        match self {
            TensorStrategy::Raw | TensorStrategy::Zstd => None,
            TensorStrategy::TensorSharded => Some(10),
            TensorStrategy::WeightDelta => Some(11),
            TensorStrategy::KvCache => Some(12),
            TensorStrategy::Activation => Some(13),
            TensorStrategy::SemanticDelta => Some(14),
        }
    }

    /// True for representations that carry a base content root.
    pub fn uses_dependency(self) -> bool {
        matches!(
            self,
            TensorStrategy::WeightDelta | TensorStrategy::SemanticDelta
        )
    }
}

/// A validated physical layout declaration for one tensor write.
///
/// `bytes` supplied with this descriptor are arranged in this physical order
/// (row-major, column-major, blocked blocks in grid order, or a single shard).
/// The store canonicalises them to row-major before encoding, so the
/// `LogicalTensorRoot` is layout-independent (`WEFT-TENSOR.md` §3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorLayoutSpec {
    pub layout: TensorLayout,
}

impl TensorLayoutSpec {
    pub fn row_major() -> Self {
        TensorLayoutSpec {
            layout: TensorLayout::RowMajor,
        }
    }

    pub fn column_major() -> Self {
        TensorLayoutSpec {
            layout: TensorLayout::ColumnMajor,
        }
    }

    pub fn blocked(block_shape: Vec<u64>) -> Self {
        TensorLayoutSpec {
            layout: TensorLayout::Blocked { block_shape },
        }
    }

    pub fn sharded(axis: u32, shards: u32, index: u32) -> Self {
        TensorLayoutSpec {
            layout: TensorLayout::Sharded {
                axis,
                shards,
                index,
            },
        }
    }

    pub fn name(&self) -> &'static str {
        self.layout.name()
    }

    fn physical_dtype(&self, dtype: TensorElementDtype) -> Result<weftcodec::TensorDtype> {
        dtype.physical_tensor_dtype().ok_or_else(|| {
            Error::unsupported_version(format!(
                "tensor dtype `{}` has no physical layout dtype",
                dtype.as_str()
            ))
        })
    }

    /// Structural validation against a dtype and shape.
    pub fn validate(&self, dtype: TensorElementDtype, shape: &[u64]) -> Result<()> {
        let limits = Limits::default();
        weftcodec::tensor::validate_shape(shape, &limits)?;
        let physical = self.physical_dtype(dtype)?;
        let descriptor = weftcodec::LayoutDescriptor {
            dtype: physical,
            shape: shape.to_vec(),
            layout: self.layout.clone(),
        };
        descriptor.validate(&limits)?;
        if let TensorLayout::Sharded {
            shards,
            index,
            axis,
        } = &self.layout
        {
            if *shards > 1 {
                return Err(Error::invalid_argument(format!(
                    "a kind:\"tensor\" manifest names exactly one frame; a sharded write with \
                     {shards} shards is not a single object (axis {axis}, index {index})"
                )));
            }
            if *index != 0 {
                return Err(Error::invalid_argument(
                    "a single-object sharded layout must use index 0",
                ));
            }
        }
        Ok(())
    }

    /// Canonical `weft.tensor.layout.v1` WCB map.
    pub fn to_value(&self, dtype: TensorElementDtype, shape: &[u64]) -> Result<Value> {
        let shape_values: Vec<Value> = shape.iter().map(|dim| Value::U64(*dim)).collect();
        let mut pairs: Vec<(String, Value)> = vec![
            (
                "schema".to_owned(),
                Value::Str(TENSOR_LAYOUT_SCHEMA.to_owned()),
            ),
            ("dtype".to_owned(), Value::Str(dtype.as_str().to_owned())),
            ("shape".to_owned(), Value::Array(shape_values)),
            ("layout".to_owned(), Value::Str(self.name().to_owned())),
        ];
        match &self.layout {
            TensorLayout::RowMajor | TensorLayout::ColumnMajor => {}
            TensorLayout::Blocked { block_shape } => {
                pairs.push((
                    "block_shape".to_owned(),
                    Value::Array(block_shape.iter().map(|dim| Value::U64(*dim)).collect()),
                ));
            }
            TensorLayout::Sharded {
                axis,
                shards,
                index,
            } => {
                pairs.push(("axis".to_owned(), Value::U64(u64::from(*axis))));
                pairs.push(("shards".to_owned(), Value::U64(u64::from(*shards))));
                pairs.push(("index".to_owned(), Value::U64(u64::from(*index))));
            }
        }
        Ok(wu::map(pairs))
    }

    /// Parse and validate a canonical `weft.tensor.layout.v1` map.
    pub fn from_value(value: &Value) -> Result<(TensorElementDtype, Vec<u64>, Self)> {
        let map = wu::as_map(value)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != TENSOR_LAYOUT_SCHEMA {
            return Err(Error::unknown_schema(format!(
                "tensor layout schema `{schema}` is not `{TENSOR_LAYOUT_SCHEMA}`"
            )));
        }
        let dtype = TensorElementDtype::parse(&wu::get_text(map, "dtype")?)?;
        let mut shape = Vec::new();
        for item in wu::get_array(map, "shape")? {
            match item {
                Value::U64(dim) => shape.push(*dim),
                _ => return Err(Error::malformed("tensor layout shape must be u64")),
            }
        }
        let layout_name = wu::get_text(map, "layout")?;
        let layout = match layout_name.as_str() {
            "row-major" => {
                wu::check_keys(map, &["schema", "dtype", "shape", "layout"])?;
                TensorLayout::RowMajor
            }
            "column-major" => {
                wu::check_keys(map, &["schema", "dtype", "shape", "layout"])?;
                TensorLayout::ColumnMajor
            }
            "blocked" => {
                wu::check_keys(map, &["schema", "dtype", "shape", "layout", "block_shape"])?;
                let mut block_shape = Vec::new();
                for item in wu::get_array(map, "block_shape")? {
                    match item {
                        Value::U64(dim) => block_shape.push(*dim),
                        _ => return Err(Error::malformed("block_shape must be u64")),
                    }
                }
                TensorLayout::Blocked { block_shape }
            }
            "sharded" => {
                wu::check_keys(
                    map,
                    &[
                        "schema", "dtype", "shape", "layout", "axis", "shards", "index",
                    ],
                )?;
                let axis = u32::try_from(wu::get_u64(map, "axis")?)
                    .map_err(|_| Error::malformed("shard axis does not fit u32"))?;
                let shards = u32::try_from(wu::get_u64(map, "shards")?)
                    .map_err(|_| Error::malformed("shard count does not fit u32"))?;
                let index = u32::try_from(wu::get_u64(map, "index")?)
                    .map_err(|_| Error::malformed("shard index does not fit u32"))?;
                TensorLayout::Sharded {
                    axis,
                    shards,
                    index,
                }
            }
            other => return Err(Error::malformed(format!("unknown tensor layout `{other}`"))),
        };
        let spec = TensorLayoutSpec { layout };
        // Structural validation only; the multi-shard single-object rule is a
        // writer restriction, not a manifest restriction.
        let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
            Error::unsupported_version(format!("tensor dtype `{}` is not physical", dtype.as_str()))
        })?;
        let descriptor = weftcodec::LayoutDescriptor {
            dtype: physical,
            shape: shape.clone(),
            layout: spec.layout.clone(),
        };
        descriptor.validate(&Limits::default())?;
        // Exact canonical re-encoding: any alternate spelling or key order is
        // rejected rather than silently normalised.
        let canonical = spec.to_value(dtype, &shape)?;
        if wu::encode(&canonical)? != wu::encode(value)? {
            return Err(Error::non_canonical(
                "tensor layout descriptor is not the canonical encoding of its fields",
            ));
        }
        Ok((dtype, shape, spec))
    }

    /// Reassemble the supplied physical bytes into canonical row-major order.
    pub fn canonicalize(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        bytes: &[u8],
    ) -> Result<Vec<u8>> {
        let limits = Limits::default();
        let physical = self.physical_dtype(dtype)?;
        let logical_len = weftcodec::tensor::payload_len(
            weftcodec::tensor::validate_shape(shape, &limits)?,
            physical,
        )?;
        if logical_len != bytes.len() as u64 {
            return Err(Error::malformed(format!(
                "tensor payload is {} bytes but {} x {} requires {logical_len}",
                bytes.len(),
                dtype.as_str(),
                shape.len()
            )));
        }
        match &self.layout {
            TensorLayout::RowMajor => {
                weftcodec::tensor::check_payload(physical, shape, bytes, &limits)?;
                Ok(bytes.to_vec())
            }
            TensorLayout::ColumnMajor => {
                PhysicalTensor::column_major(physical, shape, bytes.to_vec(), &limits)?
                    .assemble_with_limits(&limits)
                    .map_err(Error::from)
            }
            TensorLayout::Blocked { block_shape } => {
                let extents = weftcodec::tensor::block_extents(shape, block_shape)?;
                let element_bytes = dtype.element_bytes() as usize;
                let mut blocks = Vec::with_capacity(extents.len());
                let mut cursor = 0usize;
                for extent in &extents {
                    let count: u64 = extent.iter().product();
                    let len = usize::try_from(count)
                        .ok()
                        .and_then(|count| count.checked_mul(element_bytes))
                        .ok_or_else(|| Error::limit_exceeded("block payload length overflows"))?;
                    let end = cursor
                        .checked_add(len)
                        .ok_or_else(|| Error::limit_exceeded("block offset overflows"))?;
                    if end > bytes.len() {
                        return Err(Error::malformed("blocked payload is truncated"));
                    }
                    blocks.push(bytes[cursor..end].to_vec());
                    cursor = end;
                }
                if cursor != bytes.len() {
                    return Err(Error::malformed("blocked payload has trailing bytes"));
                }
                PhysicalTensor::blocked(physical, shape, block_shape, blocks, &limits)?
                    .assemble_with_limits(&limits)
                    .map_err(Error::from)
            }
            TensorLayout::Sharded { axis, shards, .. } => {
                if *shards != 1 {
                    return Err(Error::invalid_argument(
                        "a single-object tensor write requires shards == 1",
                    ));
                }
                PhysicalTensor::sharded(physical, shape, *axis, vec![bytes.to_vec()], &limits)?
                    .assemble_with_limits(&limits)
                    .map_err(Error::from)
            }
        }
    }
}

/// Planner options. The default is bytes-only (`lambda_ns = 0`).
#[derive(Clone, Debug)]
pub struct TensorPlanOptions {
    /// Bytes charged per estimated nanosecond of decode time. `0` is
    /// bytes-only.
    pub lambda_ns: u64,
    /// Semantic grouping policy for the semantic-delta candidate. `None` uses
    /// `mantissa_truncate:3` for the floating dtypes.
    pub semantic_policy: Option<SemanticPolicy>,
    /// Bound on the semantic candidate search.
    pub max_candidates: usize,
    /// Explicit candidate tensor ObjectIds. When non-empty, the search uses
    /// exactly this list instead of scanning live tensor objects.
    pub candidates: Vec<Hash32>,
}

impl Default for TensorPlanOptions {
    fn default() -> Self {
        TensorPlanOptions {
            lambda_ns: 0,
            semantic_policy: None,
            max_candidates: DEFAULT_TENSOR_MAX_CANDIDATES,
            candidates: Vec::new(),
        }
    }
}

/// Options for one tensor write.
#[derive(Clone, Debug, Default)]
pub struct TensorWriteSpec {
    /// `None` runs the planner; `Some(strategy)` requires that strategy.
    pub strategy_hint: Option<TensorStrategy>,
    /// Causal parent version ids (weight-delta bases).
    pub causal: Vec<Hash32>,
    /// Planner options.
    pub plan: TensorPlanOptions,
}

/// One priced planner candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorPlanCandidate {
    pub strategy: TensorStrategy,
    pub applicable: bool,
    pub detail: String,
    pub physical_bytes: Option<u64>,
    pub estimated_decode_ns: Option<u64>,
    pub cost: Option<u128>,
    /// Base content root for weight-delta/semantic-delta candidates.
    pub dependency: Option<Hash32>,
}

/// The complete planner decision, including the runner-up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorPlan {
    pub chosen: TensorStrategy,
    pub chosen_physical_bytes: u64,
    pub chosen_estimated_decode_ns: u64,
    pub chosen_cost: u128,
    pub runner_up: Option<TensorStrategy>,
    pub runner_up_physical_bytes: Option<u64>,
    pub runner_up_estimated_decode_ns: Option<u64>,
    pub runner_up_cost: Option<u128>,
    pub candidates: Vec<TensorPlanCandidate>,
}

/// Tensor identity recovered from an object record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTensorMetadata {
    pub dtype: TensorElementDtype,
    pub shape: Vec<u64>,
    pub logical_root: Hash32,
    pub layout: TensorLayoutSpec,
    pub strategy: String,
}

#[derive(Clone, Debug)]
struct PreparedArtifact {
    kind: RepresentationKind,
    strategy_name: String,
    frame: Option<Vec<u8>>,
    dependencies: Vec<Hash32>,
    tensor: Option<TensorRepresentation>,
}

#[derive(Clone, Debug)]
struct PreparedCandidate {
    candidate: TensorPlanCandidate,
    artifact: Option<PreparedArtifact>,
}

/// Integer estimate of decode time in nanoseconds. These are a documented
/// linear cost model, not measurements: raw is a memory pass, zstd inflates,
/// and the tensor modes entropy-decode planes and (for deltas) decode a base.
fn estimated_decode_ns(strategy: TensorStrategy, size: u64) -> u64 {
    match strategy {
        TensorStrategy::Raw => size / 200 + 20_000,
        TensorStrategy::Zstd => size + 50_000,
        TensorStrategy::TensorSharded => size.saturating_mul(4) + 100_000,
        TensorStrategy::WeightDelta => size.saturating_mul(3) + 100_000,
        TensorStrategy::SemanticDelta => size.saturating_mul(3) + 150_000,
        TensorStrategy::KvCache | TensorStrategy::Activation => size + 100_000,
    }
}

fn price(physical_bytes: u64, estimated_ns: u64, lambda_ns: u64) -> u128 {
    u128::from(physical_bytes) + u128::from(lambda_ns) * u128::from(estimated_ns)
}

/// Build the self-contained mode-6 `TENSOR` frame used as a delta base.
///
/// The store never stores this frame: it is reconstructed deterministically
/// from the base content bytes, so the frozen "dependencies are content roots"
/// rule and the existing GC closure keep working unchanged.
fn mode6_base_frame(bytes: &[u8], dtype: TensorElementDtype, shape: &[u64]) -> Result<Vec<u8>> {
    let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
        Error::invalid_argument(format!("dtype `{}` cannot be a delta base", dtype.as_str()))
    })?;
    Ok(weftcodec::modes::encode_tensor(
        bytes,
        physical,
        shape,
        &Limits::default(),
    )?)
}

/// Derive a deterministic q8_0 block scale from each block's maximum
/// magnitude. The scale is descriptive metadata only: the codec never
/// dequantizes, and reconstruction is bit-exact on the stored words.
fn q8_0_scales(words: &[i8], block_size: u64) -> (Vec<u32>, Vec<i64>) {
    let block = block_size as usize;
    let mut scales = Vec::with_capacity(words.len() / block.max(1) + 1);
    let mut zero_points = Vec::with_capacity(scales.capacity());
    for chunk in words.chunks(block) {
        let amax = chunk
            .iter()
            .map(|word| i32::from(*word).unsigned_abs())
            .max()
            .unwrap_or(0);
        let scale = if amax == 0 {
            1.0f32
        } else {
            amax as f32 / 127.0
        };
        scales.push(scale.to_bits());
        zero_points.push(0);
    }
    (scales, zero_points)
}

fn q8_0_block_size(element_count: u64) -> Option<u64> {
    [16u64, 32, 64, 128, 256]
        .into_iter()
        .find(|block| element_count % *block == 0)
}

fn words_from_bytes(bytes: &[u8]) -> Vec<i8> {
    bytes.iter().map(|byte| *byte as i8).collect()
}

impl Store {
    /// Write one logical tensor as a new file object.
    ///
    /// `bytes` are arranged per `layout`; the store canonicalises them to
    /// row-major before encoding. `strategy_hint` selects a representation or
    /// `None` runs the planner.
    #[allow(clippy::too_many_arguments)]
    pub fn write_tensor_file(
        &mut self,
        parent: Hash32,
        name: &[u8],
        dtype: &str,
        shape: &[u64],
        bytes: &[u8],
        layout: &TensorLayoutSpec,
        strategy_hint: Option<TensorStrategy>,
    ) -> Result<Hash32> {
        let spec = TensorWriteSpec {
            strategy_hint,
            ..TensorWriteSpec::default()
        };
        self.write_tensor_file_with_spec(parent, name, dtype, shape, bytes, layout, &spec)
    }

    /// Write one logical tensor with explicit causal/planner options.
    #[allow(clippy::too_many_arguments)]
    pub fn write_tensor_file_with_spec(
        &mut self,
        parent: Hash32,
        name: &[u8],
        dtype: &str,
        shape: &[u64],
        bytes: &[u8],
        layout: &TensorLayoutSpec,
        spec: &TensorWriteSpec,
    ) -> Result<Hash32> {
        self.require_writable()?;
        self.require_dir(parent)?;
        crate::state::validate_name_bytes(name)?;
        self.require_free_name(parent, name)?;
        let dtype = TensorElementDtype::parse(dtype)?;
        layout.validate(dtype, shape)?;
        let logical = layout.canonicalize(dtype, shape, bytes)?;
        let prepared =
            self.prepare_candidates(dtype, shape, &logical, layout, &spec.causal, &spec.plan)?;
        let chosen = self.choose_candidate(&prepared, spec.strategy_hint)?;
        let artifact = chosen
            .artifact
            .clone()
            .ok_or_else(|| Error::invalid_argument("chosen tensor strategy has no artifact"))?;
        let root = match &artifact.tensor {
            Some(tensor) => tensor.logical_root,
            None => self.logical_root_for_bytes(dtype, shape, &logical, None)?,
        };
        let (manifest_id, content_hash, size) =
            self.store_tensor_artifact(dtype, shape, &logical, layout, &artifact)?;
        let mut attrs = BTreeMap::new();
        apply_tensor_attrs(
            &mut attrs,
            dtype,
            shape,
            root,
            layout,
            artifact.strategy_name.as_str(),
        )?;
        let seed = identity::os_random_32()?;
        let (owner_uid, owner_gid) = (self.options().owner_uid, self.options().owner_gid);
        self.commit_created_file(
            parent,
            name,
            &seed,
            manifest_id,
            content_hash,
            size,
            0o644,
            owner_uid,
            owner_gid,
            attrs,
            &spec.causal,
        )
    }

    /// Price every applicable strategy without writing anything.
    pub fn plan_tensor(
        &self,
        dtype: &str,
        shape: &[u64],
        bytes: &[u8],
        layout: &TensorLayoutSpec,
        causal: &[Hash32],
        options: &TensorPlanOptions,
    ) -> Result<TensorPlan> {
        let dtype = TensorElementDtype::parse(dtype)?;
        layout.validate(dtype, shape)?;
        let logical = layout.canonicalize(dtype, shape, bytes)?;
        let prepared = self.prepare_candidates(dtype, shape, &logical, layout, causal, options)?;
        let chosen = self.choose_candidate(&prepared, None)?;
        let mut runner_up: Option<&PreparedCandidate> = None;
        let mut applicable: Vec<&PreparedCandidate> = prepared
            .iter()
            .filter(|item| item.candidate.applicable)
            .collect();
        applicable.sort_by(|a, b| {
            a.candidate
                .cost
                .cmp(&b.candidate.cost)
                .then_with(|| a.candidate.strategy.cmp(&b.candidate.strategy))
        });
        for item in &applicable {
            if item.candidate.strategy != chosen.candidate.strategy {
                runner_up = Some(item);
                break;
            }
        }
        Ok(TensorPlan {
            chosen: chosen.candidate.strategy,
            chosen_physical_bytes: chosen.candidate.physical_bytes.unwrap_or(0),
            chosen_estimated_decode_ns: chosen.candidate.estimated_decode_ns.unwrap_or(0),
            chosen_cost: chosen.candidate.cost.unwrap_or(0),
            runner_up: runner_up.map(|item| item.candidate.strategy),
            runner_up_physical_bytes: runner_up.and_then(|item| item.candidate.physical_bytes),
            runner_up_estimated_decode_ns: runner_up
                .and_then(|item| item.candidate.estimated_decode_ns),
            runner_up_cost: runner_up.and_then(|item| item.candidate.cost),
            candidates: prepared.iter().map(|item| item.candidate.clone()).collect(),
        })
    }

    /// Read one tensor object's canonical logical bytes.
    pub fn read_tensor(&self, id: Hash32) -> Result<Vec<u8>> {
        self.tensor_metadata(id)?;
        self.read_object(id)
    }

    /// Read a logical hyper-rectangle of a tensor object in row-major order.
    ///
    /// Delta and Huffman frames are not independently seekable, so the whole
    /// object is decoded before the region is sliced; the returned region is
    /// exact and the touched-unit count is one.
    pub fn read_tensor_region(&self, id: Hash32, region: &Region) -> Result<RegionRead> {
        let meta = self.tensor_metadata(id)?;
        let bytes = self.read_object(id)?;
        let physical = meta
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("tensor dtype has no physical layout"))?;
        let tensor = PhysicalTensor::row_major(physical, &meta.shape, bytes, &Limits::default())?;
        tensor
            .read_region_with_limits(region, &Limits::default())
            .map_err(Error::from)
    }

    /// Recompute and verify an object's `LogicalTensorRoot`.
    pub fn tensor_logical_root(&self, id: Hash32) -> Result<Hash32> {
        let meta = self.tensor_metadata(id)?;
        let bytes = self.read_object(id)?;
        let root = if meta.dtype == TensorElementDtype::Q8_0 {
            let shard = self.q8_0_shard_meta(id)?;
            let words = words_from_bytes(&bytes);
            weftcodec::tensor_sharded::logical_root(
                shard.dtype,
                &shard.shape,
                &words,
                &shard.scales,
                &shard.zero_points,
                &Limits::default(),
            )?
        } else {
            self.logical_root_for_bytes(meta.dtype, &meta.shape, &bytes, None)?
        };
        if root != meta.logical_root {
            return Err(Error::corrupt(format!(
                "object {id} tensor logical root does not match its recorded identity"
            )));
        }
        Ok(root)
    }

    fn q8_0_shard_meta(&self, id: Hash32) -> Result<weftcodec::TensorShardedMeta> {
        let record = self.object_meta(id)?;
        let manifest = recovery::read_manifest(self.root(), record.content)?;
        let rep = manifest
            .representation
            .as_ref()
            .filter(|rep| rep.kind == RepresentationKind::Tensor)
            .ok_or_else(|| Error::corrupt("q8_0 tensor is not a tensor representation"))?;
        if rep.tensor.as_ref().map(|tensor| tensor.tensor_mode) != Some(10) {
            return Err(Error::corrupt("q8_0 tensor is not stored through mode 10"));
        }
        let frame_id = rep
            .frame
            .ok_or_else(|| Error::corrupt("tensor representation has no frame id"))?;
        let frame_bytes = recovery::read_blob_verified(
            self.root(),
            BlobKind::Manifest,
            frame_id,
            identity::MANIFEST_DOMAIN,
        )?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        Ok(weftcodec::decode_shard_frame(&frame, &limits)?.meta)
    }

    /// Tensor identity recorded for an object.
    pub fn tensor_metadata(&self, id: Hash32) -> Result<StoredTensorMetadata> {
        let record = self.object_meta(id)?;
        if record.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "tensor metadata requires a file object",
            ));
        }
        let manifest = recovery::read_manifest(self.root(), record.content)?;
        if let Some(rep) = &manifest.representation {
            if rep.kind == RepresentationKind::Tensor {
                let tensor = rep
                    .tensor
                    .as_ref()
                    .ok_or_else(|| Error::corrupt("tensor manifest has no tensor fields"))?;
                let (layout_dtype, layout_shape, layout) =
                    TensorLayoutSpec::from_value(&tensor.layout)?;
                if layout_dtype != tensor.dtype || layout_shape != tensor.shape {
                    return Err(Error::corrupt(
                        "tensor layout descriptor disagrees with the manifest dtype/shape",
                    ));
                }
                return Ok(StoredTensorMetadata {
                    dtype: tensor.dtype,
                    shape: tensor.shape.clone(),
                    logical_root: tensor.logical_root,
                    layout,
                    strategy: rep.strategy.clone(),
                });
            }
        }
        let dtype = match record.attrs.get(ATTR_TENSOR_DTYPE) {
            Some(Value::Str(text)) => TensorElementDtype::parse(text)?,
            _ => return Err(Error::not_found("object is not a tensor")),
        };
        let shape = match record.attrs.get(ATTR_TENSOR_SHAPE) {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::U64(dim) => Ok(*dim),
                    _ => Err(Error::corrupt("tensor shape attribute is not u64")),
                })
                .collect::<Result<Vec<u64>>>()?,
            _ => return Err(Error::corrupt("tensor shape attribute is missing")),
        };
        let logical_root = match record.attrs.get(ATTR_TENSOR_LOGICAL_ROOT) {
            Some(Value::Bytes(bytes)) => Hash32::from_slice(bytes)
                .map_err(|_| Error::corrupt("bad tensor root attribute"))?,
            _ => return Err(Error::corrupt("tensor root attribute is missing")),
        };
        let layout = match record.attrs.get(ATTR_TENSOR_LAYOUT) {
            Some(Value::Bytes(bytes)) => {
                let value = wu::decode(bytes)?;
                let (layout_dtype, layout_shape, layout) = TensorLayoutSpec::from_value(&value)?;
                if layout_dtype != dtype || layout_shape != shape {
                    return Err(Error::corrupt(
                        "tensor layout attribute disagrees with the tensor dtype/shape",
                    ));
                }
                layout
            }
            _ => return Err(Error::corrupt("tensor layout attribute is missing")),
        };
        let strategy = match record.attrs.get(ATTR_TENSOR_STRATEGY) {
            Some(Value::Str(text)) => text.clone(),
            _ => "unknown".to_owned(),
        };
        Ok(StoredTensorMetadata {
            dtype,
            shape,
            logical_root,
            layout,
            strategy,
        })
    }

    /// Replace a logical region of an existing tensor object.
    ///
    /// Tensor frames are not independently seekable, so this decodes the
    /// object, splices the region, and re-plans a new version whose causal
    /// parent is the previous version.
    pub fn write_tensor_range(
        &mut self,
        id: Hash32,
        region: &Region,
        bytes: &[u8],
        spec: &TensorWriteSpec,
    ) -> Result<Hash32> {
        self.require_writable()?;
        let meta = self.tensor_metadata(id)?;
        region.validate(&meta.shape)?;
        let physical = meta
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("tensor dtype has no physical layout"))?;
        let extent = region.extent();
        let expected = weftcodec::tensor::payload_len(extent.iter().product(), physical)?;
        if expected != bytes.len() as u64 {
            return Err(Error::invalid_argument(format!(
                "region payload is {} bytes but the region requires {expected}",
                bytes.len()
            )));
        }
        let mut logical = self.read_object(id)?;
        splice_region(&mut logical, &meta.shape, meta.dtype, region, bytes)?;
        let old = self.object_meta(id)?;
        let mut causal = spec.causal.clone();
        causal.push(old.version);
        let prepared = self.prepare_candidates(
            meta.dtype,
            &meta.shape,
            &logical,
            &meta.layout,
            &causal,
            &spec.plan,
        )?;
        let chosen = self.choose_candidate(&prepared, spec.strategy_hint)?;
        let artifact = chosen
            .artifact
            .clone()
            .ok_or_else(|| Error::invalid_argument("chosen tensor strategy has no artifact"))?;
        let root = match &artifact.tensor {
            Some(tensor) => tensor.logical_root,
            None => self.logical_root_for_bytes(meta.dtype, &meta.shape, &logical, None)?,
        };
        let (manifest_id, content_hash, size) =
            self.store_tensor_artifact(meta.dtype, &meta.shape, &logical, &meta.layout, &artifact)?;
        let mut attrs = old.attrs.clone();
        apply_tensor_attrs(
            &mut attrs,
            meta.dtype,
            &meta.shape,
            root,
            &meta.layout,
            artifact.strategy_name.as_str(),
        )?;
        self.publish_file_version_with_attrs(
            id,
            old,
            manifest_id,
            content_hash,
            size,
            &causal,
            attrs,
        )
    }

    // ---- internals -------------------------------------------------------

    fn prepare_candidates(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        layout: &TensorLayoutSpec,
        causal: &[Hash32],
        options: &TensorPlanOptions,
    ) -> Result<Vec<PreparedCandidate>> {
        let limits = Limits::default();
        let size = logical.len() as u64;
        let mut out = Vec::new();
        // The additive quantized LogicalTensorRoot binds the mode-10 scale
        // plane, so a q8_0 tensor is only stored through mode 10. q4_0 store
        // support is not implemented in B1 (the byte-level nibble mapping is
        // specified but not wired into the store planner).
        if dtype == TensorElementDtype::Q4_0 {
            return Err(Error::invalid_argument(
                "q4_0 tensor storage is not implemented in B1; use q8_0 or a full-precision dtype",
            ));
        }
        let quantized = dtype == TensorElementDtype::Q8_0;

        // Raw chunks (v2 kind:"raw").
        out.push(if quantized {
            skipped(
                TensorStrategy::Raw,
                "q8_0 identity requires the mode-10 scale plane".to_owned(),
            )
        } else {
            PreparedCandidate {
                candidate: TensorPlanCandidate {
                    strategy: TensorStrategy::Raw,
                    applicable: true,
                    detail: "canonical row-major chunks".to_owned(),
                    physical_bytes: Some(size),
                    estimated_decode_ns: Some(estimated_decode_ns(TensorStrategy::Raw, size)),
                    cost: Some(price(
                        size,
                        estimated_decode_ns(TensorStrategy::Raw, size),
                        options.lambda_ns,
                    )),
                    dependency: None,
                },
                artifact: Some(PreparedArtifact {
                    kind: RepresentationKind::Raw,
                    strategy_name: "raw".to_owned(),
                    frame: None,
                    dependencies: Vec::new(),
                    tensor: None,
                }),
            }
        });

        // Zstandard frame.
        let codec_chunk =
            crate::representation::codec_chunk_size(self.options().chunk_size.max(1)) as usize;
        if quantized {
            out.push(skipped(
                TensorStrategy::Zstd,
                "q8_0 identity requires the mode-10 scale plane".to_owned(),
            ));
        } else {
            match weftcodec::modes::encode_zstd(
                logical,
                crate::representation::CODEC_ZSTD_LEVEL,
                Some(codec_chunk),
                &limits,
            ) {
                Ok(frame) => {
                    let physical = frame.len() as u64;
                    let estimate = estimated_decode_ns(TensorStrategy::Zstd, size);
                    out.push(PreparedCandidate {
                        candidate: TensorPlanCandidate {
                            strategy: TensorStrategy::Zstd,
                            applicable: true,
                            detail: "one ZSTD frame".to_owned(),
                            physical_bytes: Some(physical),
                            estimated_decode_ns: Some(estimate),
                            cost: Some(price(physical, estimate, options.lambda_ns)),
                            dependency: None,
                        },
                        artifact: Some(PreparedArtifact {
                            kind: RepresentationKind::Codec,
                            strategy_name: "zstd".to_owned(),
                            frame: Some(frame),
                            dependencies: Vec::new(),
                            tensor: None,
                        }),
                    });
                }
                Err(err) => out.push(skipped(TensorStrategy::Zstd, err.to_string())),
            }
        }

        // Mode 10 tensor-sharded. Restricted to q8_0: the additive quantized
        // logical root needs the codec's scale plane, which only mode 10
        // carries, and the store writes one whole-tensor shard.
        if dtype == TensorElementDtype::Q8_0 {
            match self.prepare_mode10(dtype, shape, logical, &limits) {
                Ok((frame, root)) => {
                    let physical = frame.len() as u64;
                    let estimate = estimated_decode_ns(TensorStrategy::TensorSharded, size);
                    out.push(PreparedCandidate {
                        candidate: TensorPlanCandidate {
                            strategy: TensorStrategy::TensorSharded,
                            applicable: true,
                            detail: "one mode-10 shard (shard_count = 1)".to_owned(),
                            physical_bytes: Some(physical),
                            estimated_decode_ns: Some(estimate),
                            cost: Some(price(physical, estimate, options.lambda_ns)),
                            dependency: None,
                        },
                        artifact: Some(PreparedArtifact {
                            kind: RepresentationKind::Tensor,
                            strategy_name: "tensor-sharded".to_owned(),
                            frame: Some(frame),
                            dependencies: Vec::new(),
                            tensor: Some(TensorRepresentation {
                                tensor_mode: 10,
                                dtype,
                                shape: shape.to_vec(),
                                logical_root: root,
                                layout: layout.to_value(dtype, shape)?,
                            }),
                        }),
                    });
                }
                Err(err) => out.push(skipped(TensorStrategy::TensorSharded, err.to_string())),
            }
        } else {
            out.push(skipped(
                TensorStrategy::TensorSharded,
                format!(
                    "mode 10 carries quantized tensors; {} is not q8_0",
                    dtype.as_str()
                ),
            ));
        }

        // Mode 11 weight-delta against the causal parent.
        match self.prepare_delta(
            dtype,
            shape,
            logical,
            causal,
            &limits,
            TensorStrategy::WeightDelta,
        ) {
            Ok(Some((frame, dependency))) => {
                let physical = frame.len() as u64;
                let estimate = estimated_decode_ns(TensorStrategy::WeightDelta, size);
                out.push(PreparedCandidate {
                    candidate: TensorPlanCandidate {
                        strategy: TensorStrategy::WeightDelta,
                        applicable: true,
                        detail: "mode 11 against the causal parent".to_owned(),
                        physical_bytes: Some(physical),
                        estimated_decode_ns: Some(estimate),
                        cost: Some(price(physical, estimate, options.lambda_ns)),
                        dependency: Some(dependency),
                    },
                    artifact: Some(PreparedArtifact {
                        kind: RepresentationKind::Tensor,
                        strategy_name: "weight-delta".to_owned(),
                        frame: Some(frame),
                        dependencies: vec![dependency],
                        tensor: Some(TensorRepresentation {
                            tensor_mode: 11,
                            dtype,
                            shape: shape.to_vec(),
                            logical_root: self
                                .logical_root_for_bytes(dtype, shape, logical, None)?,
                            layout: layout.to_value(dtype, shape)?,
                        }),
                    }),
                });
            }
            Ok(None) => out.push(skipped(
                TensorStrategy::WeightDelta,
                "no usable causal parent with the same dtype/shape".to_owned(),
            )),
            Err(err) => out.push(skipped(TensorStrategy::WeightDelta, err.to_string())),
        }

        // Mode 14 semantic-delta against a same-semantic-root candidate.
        match self.prepare_semantic(dtype, shape, logical, options, &limits) {
            Ok(Some((frame, dependency, policy))) => {
                let physical = frame.len() as u64;
                let estimate = estimated_decode_ns(TensorStrategy::SemanticDelta, size);
                out.push(PreparedCandidate {
                    candidate: TensorPlanCandidate {
                        strategy: TensorStrategy::SemanticDelta,
                        applicable: true,
                        detail: format!(
                            "mode 14 against a same-semantic-root base (policy {policy})"
                        ),
                        physical_bytes: Some(physical),
                        estimated_decode_ns: Some(estimate),
                        cost: Some(price(physical, estimate, options.lambda_ns)),
                        dependency: Some(dependency),
                    },
                    artifact: Some(PreparedArtifact {
                        kind: RepresentationKind::Tensor,
                        strategy_name: "semantic-delta".to_owned(),
                        frame: Some(frame),
                        dependencies: vec![dependency],
                        tensor: Some(TensorRepresentation {
                            tensor_mode: 14,
                            dtype,
                            shape: shape.to_vec(),
                            logical_root: self
                                .logical_root_for_bytes(dtype, shape, logical, None)?,
                            layout: layout.to_value(dtype, shape)?,
                        }),
                    }),
                });
            }
            Ok(None) => out.push(skipped(
                TensorStrategy::SemanticDelta,
                "no same-semantic-root candidate with the same dtype/shape".to_owned(),
            )),
            Err(err) => out.push(skipped(TensorStrategy::SemanticDelta, err.to_string())),
        }

        Ok(out)
    }

    fn prepare_mode10(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        limits: &Limits,
    ) -> Result<(Vec<u8>, Hash32)> {
        let sharded = dtype
            .sharded_dtype()
            .ok_or_else(|| Error::invalid_argument("dtype does not support mode 10"))?;
        let element_count = weftcodec::tensor::validate_shape(shape, limits)?;
        let block_size = q8_0_block_size(element_count).ok_or_else(|| {
            Error::invalid_argument(format!(
                "element count {element_count} is not divisible by any mode-10 block size"
            ))
        })?;
        let words = words_from_bytes(logical);
        let (scales, zero_points) = q8_0_scales(&words, block_size);
        let input = weftcodec::ShardedTensorInput {
            dtype: sharded,
            shape: shape.to_vec(),
            shard_axis: 0,
            shard_count: 1,
            block_size,
            scales,
            zero_points,
            words,
        };
        let mut frames = weftcodec::encode_sharded_tensor(&input, limits)?;
        let frame = frames
            .pop()
            .ok_or_else(|| Error::corrupt("mode-10 encoder produced no frame"))?;
        let parsed = Frame::parse(&frame, limits)?;
        let shard = weftcodec::decode_shard_frame(&parsed, limits)?;
        let root = weftcodec::tensor_sharded::logical_root(
            shard.meta.dtype,
            &shard.meta.shape,
            &shard.words,
            &shard.meta.scales,
            &shard.meta.zero_points,
            limits,
        )?;
        Ok((frame, root))
    }

    fn prepare_delta(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        causal: &[Hash32],
        limits: &Limits,
        strategy: TensorStrategy,
    ) -> Result<Option<(Vec<u8>, Hash32)>> {
        if strategy != TensorStrategy::WeightDelta {
            return Ok(None);
        }
        let Some(weight) = dtype.weight_dtype() else {
            return Ok(None);
        };
        let Some((base_root, base_bytes)) = self.tensor_causal_base(dtype, shape, causal)? else {
            return Ok(None);
        };
        // Respect the frozen dependency depth bound: a delta on a chain at the
        // bound would be unreadable. The planner falls back to an independent
        // representation instead of writing a store it cannot decode.
        let base_depth = self.representation_dep_depth(base_root, &mut Vec::new())?;
        if base_depth + 1 > MAX_CODEC_DEPENDENCY_DEPTH {
            return Ok(None);
        }
        let base_frame = mode6_base_frame(&base_bytes, dtype, shape)?;
        let frame = weftcodec::encode_weight_delta(logical, weight, shape, &base_frame, limits)?;
        Ok(Some((frame, base_root)))
    }

    fn prepare_semantic(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        options: &TensorPlanOptions,
        limits: &Limits,
    ) -> Result<Option<(Vec<u8>, Hash32, String)>> {
        let Some(weight) = dtype.weight_dtype() else {
            return Ok(None);
        };
        let policy = options
            .semantic_policy
            .clone()
            .unwrap_or(SemanticPolicy::MantissaTruncate { bits: 3 });
        let element_count = weftcodec::tensor::validate_shape(shape, limits)?;
        policy.validate(weight, element_count)?;
        let candidates = self.tensor_semantic_candidates(dtype, shape, options)?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let frames: Vec<Vec<u8>> = candidates
            .iter()
            .map(|(_, bytes)| mode6_base_frame(bytes, dtype, shape))
            .collect::<Result<_>>()?;
        let borrows: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
        let Some(index) =
            weftcodec::select_semantic_base(logical, weight, shape, &policy, &borrows, limits)?
        else {
            return Ok(None);
        };
        let base_depth = self.representation_dep_depth(candidates[index].0, &mut Vec::new())?;
        if base_depth + 1 > MAX_CODEC_DEPENDENCY_DEPTH {
            return Ok(None);
        }
        let frame = weftcodec::encode_semantic_delta(
            logical,
            weight,
            shape,
            &frames[index],
            policy.clone(),
            limits,
        )?;
        let detail = policy.to_wcb_value();
        Ok(Some((
            frame,
            candidates[index].0,
            format!("{}: {:?}", dtype.as_str(), detail),
        )))
    }

    /// Maximum representation-dependency depth below `manifest_id`.
    fn representation_dep_depth(
        &self,
        manifest_id: Hash32,
        seen: &mut Vec<Hash32>,
    ) -> Result<usize> {
        if seen.contains(&manifest_id) {
            return Err(Error::corrupt("tensor representation dependency cycle"));
        }
        let manifest = recovery::read_manifest(self.root(), manifest_id)?;
        let Some(rep) = &manifest.representation else {
            return Ok(0);
        };
        if rep.dependencies.is_empty() {
            return Ok(0);
        }
        seen.push(manifest_id);
        let mut deepest = 0usize;
        for dependency in &rep.dependencies {
            let depth = self.representation_dep_depth(*dependency, seen)?;
            deepest = deepest.max(depth);
        }
        seen.pop();
        Ok(deepest + 1)
    }

    fn tensor_causal_base(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        causal: &[Hash32],
    ) -> Result<Option<(Hash32, Vec<u8>)>> {
        if causal.is_empty() {
            return Ok(None);
        }
        let state = self.current_state();
        for parent in causal {
            for record in state.objects.values() {
                if record.kind != ObjectKind::File || record.version != *parent {
                    continue;
                }
                let meta = match self.tensor_metadata(record.id) {
                    Ok(meta) => meta,
                    Err(_) => continue,
                };
                if meta.dtype != dtype || meta.shape != shape {
                    continue;
                }
                match self.read_content(record.content, 0, record.size) {
                    Ok(bytes) if bytes.len() as u64 == record.size => {
                        return Ok(Some((record.content, bytes)));
                    }
                    _ => continue,
                }
            }
        }
        Ok(None)
    }

    fn tensor_semantic_candidates(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        options: &TensorPlanOptions,
    ) -> Result<Vec<(Hash32, Vec<u8>)>> {
        let mut out = Vec::new();
        if !options.candidates.is_empty() {
            if options.candidates.len() > options.max_candidates.max(1) {
                return Err(Error::limit_exceeded(format!(
                    "semantic candidate list has {} entries, bound is {}",
                    options.candidates.len(),
                    options.max_candidates
                )));
            }
            for id in &options.candidates {
                let record = self.object_meta(*id)?;
                let meta = self.tensor_metadata(*id)?;
                if meta.dtype != dtype || meta.shape != shape {
                    continue;
                }
                let bytes = self.read_content(record.content, 0, record.size)?;
                out.push((record.content, bytes));
            }
            return Ok(out);
        }
        let state = self.current_state();
        let mut ids: Vec<Hash32> = state
            .objects
            .values()
            .filter(|record| record.kind == ObjectKind::File)
            .map(|record| record.id)
            .collect();
        ids.sort();
        let bound = options.max_candidates.max(1);
        let mut accepted = 0usize;
        for id in ids {
            if accepted >= bound {
                break;
            }
            let record = match state.objects.get(&id) {
                Some(record) => record,
                None => continue,
            };
            let meta = match self.tensor_metadata(id) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.dtype != dtype || meta.shape != shape {
                continue;
            }
            match self.read_content(record.content, 0, record.size) {
                Ok(bytes) if bytes.len() as u64 == record.size => {
                    out.push((record.content, bytes));
                    accepted += 1;
                }
                _ => continue,
            }
        }
        Ok(out)
    }

    fn choose_candidate<'a>(
        &self,
        prepared: &'a [PreparedCandidate],
        hint: Option<TensorStrategy>,
    ) -> Result<&'a PreparedCandidate> {
        if let Some(strategy) = hint {
            return prepared
                .iter()
                .find(|item| item.candidate.applicable && item.candidate.strategy == strategy)
                .ok_or_else(|| {
                    let detail = prepared
                        .iter()
                        .find(|item| item.candidate.strategy == strategy)
                        .map(|item| item.candidate.detail.clone())
                        .unwrap_or_else(|| "strategy is not a planner candidate".to_owned());
                    Error::invalid_argument(format!(
                        "tensor strategy `{}` is not applicable: {detail}",
                        strategy.as_str()
                    ))
                });
        }
        let mut best: Option<&PreparedCandidate> = None;
        for item in prepared.iter().filter(|item| item.candidate.applicable) {
            let Some(cost) = item.candidate.cost else {
                continue;
            };
            match best {
                None => best = Some(item),
                Some(current) => {
                    let current_cost = current.candidate.cost.unwrap_or(u128::MAX);
                    if cost < current_cost {
                        best = Some(item);
                    }
                }
            }
        }
        best.ok_or_else(|| Error::invalid_argument("no applicable tensor strategy"))
    }

    fn store_tensor_artifact(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        layout: &TensorLayoutSpec,
        artifact: &PreparedArtifact,
    ) -> Result<(Hash32, Hash32, u64)> {
        let content_hash = identity::content_hash(logical);
        let size = logical.len() as u64;
        let representation = match artifact.kind {
            RepresentationKind::Raw => {
                let manifest =
                    self.build_raw_manifest(logical, self.options().chunk_size, content_hash)?;
                Representation {
                    kind: RepresentationKind::Raw,
                    frame: None,
                    dependencies: Vec::new(),
                    strategy: "raw".to_owned(),
                    decoded_size: size,
                    tensor: None,
                }
                .clone_into_manifest(manifest)
            }
            RepresentationKind::Codec => {
                let frame_bytes = artifact
                    .frame
                    .as_ref()
                    .ok_or_else(|| Error::corrupt("codec artifact has no frame"))?;
                let frame_id =
                    self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, frame_bytes)?;
                Manifest {
                    size,
                    chunk_size: self.options().chunk_size,
                    chunks: Vec::new(),
                    content_hash,
                    representation: Some(Representation {
                        kind: RepresentationKind::Codec,
                        frame: Some(frame_id),
                        dependencies: Vec::new(),
                        strategy: "zstd".to_owned(),
                        decoded_size: size,
                        tensor: None,
                    }),
                }
            }
            RepresentationKind::Tensor => {
                let frame_bytes = artifact
                    .frame
                    .as_ref()
                    .ok_or_else(|| Error::corrupt("tensor artifact has no frame"))?;
                let frame_id =
                    self.store_blob(BlobKind::Manifest, identity::MANIFEST_DOMAIN, frame_bytes)?;
                let mut tensor = artifact
                    .tensor
                    .clone()
                    .ok_or_else(|| Error::corrupt("tensor artifact has no tensor fields"))?;
                tensor.layout = layout.to_value(dtype, shape)?;
                Manifest {
                    size,
                    chunk_size: self.options().chunk_size,
                    chunks: Vec::new(),
                    content_hash,
                    representation: Some(Representation {
                        kind: RepresentationKind::Tensor,
                        frame: Some(frame_id),
                        dependencies: artifact.dependencies.clone(),
                        strategy: artifact.strategy_name.clone(),
                        decoded_size: size,
                        tensor: Some(tensor),
                    }),
                }
            }
        };
        self.store_manifest(representation)
    }

    fn logical_root_for_bytes(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        bytes: &[u8],
        mode10: Option<&weftcodec::TensorShardedMeta>,
    ) -> Result<Hash32> {
        if dtype == TensorElementDtype::Q8_0 {
            let meta = mode10.ok_or_else(|| {
                Error::invalid_argument(
                    "a q8_0 logical root requires the mode-10 scale plane".to_owned(),
                )
            })?;
            let words = words_from_bytes(bytes);
            return Ok(weftcodec::tensor_sharded::logical_root(
                meta.dtype,
                &meta.shape,
                &words,
                &meta.scales,
                &meta.zero_points,
                &Limits::default(),
            )?);
        }
        let physical = dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("tensor dtype has no logical root"))?;
        Ok(weftcodec::tensor::logical_root(
            physical,
            shape,
            bytes,
            &Limits::default(),
        )?)
    }

    /// Decode a `kind:"tensor"` representation. Called by the store's read
    /// dispatch with the shared dependency depth/cycle state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn read_tensor_manifest_bytes(
        &self,
        manifest: &Manifest,
        rep: &Representation,
        offset: u64,
        length: u64,
        verify_whole: bool,
        depth: usize,
        seen: &mut Vec<Hash32>,
    ) -> Result<Vec<u8>> {
        if depth > MAX_CODEC_DEPENDENCY_DEPTH {
            return Err(Error::corrupt(format!(
                "tensor representation dependency depth exceeds {MAX_CODEC_DEPENDENCY_DEPTH}"
            )));
        }
        let tensor = rep
            .tensor
            .as_ref()
            .ok_or_else(|| Error::corrupt("tensor representation has no tensor fields"))?;
        let (layout_dtype, layout_shape, _) = TensorLayoutSpec::from_value(&tensor.layout)?;
        if layout_dtype != tensor.dtype || layout_shape != tensor.shape {
            return Err(Error::corrupt(
                "tensor layout descriptor disagrees with the manifest dtype/shape",
            ));
        }
        let frame_id = rep
            .frame
            .ok_or_else(|| Error::corrupt("tensor representation has no frame id"))?;
        let frame_bytes = recovery::read_blob_verified(
            self.root(),
            BlobKind::Manifest,
            frame_id,
            identity::MANIFEST_DOMAIN,
        )?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        let expected_mode = match tensor.tensor_mode {
            10 => Mode::TensorSharded,
            11 => Mode::WeightDelta,
            12 => Mode::KvCache,
            13 => Mode::ActivationStream,
            14 => Mode::SemanticDelta,
            other => {
                return Err(Error::corrupt(format!(
                    "tensor_mode {other} is not a defined tensor frame mode"
                )))
            }
        };
        if frame.header.mode != expected_mode {
            return Err(Error::corrupt(format!(
                "tensor strategy `{}` disagrees with frame mode `{}`",
                rep.strategy,
                frame.header.mode.as_str()
            )));
        }
        let logical = match tensor.tensor_mode {
            10 => self.decode_mode10(&frame, tensor, manifest, &limits)?,
            11 => self.decode_delta(&frame, tensor, rep, &limits, depth, seen, false)?,
            14 => self.decode_delta(&frame, tensor, rep, &limits, depth, seen, true)?,
            12 => self.decode_kv_page(&frame, tensor, &limits)?,
            13 => self.decode_activation(&frame, tensor, &limits)?,
            _ => unreachable!("checked above"),
        };
        if logical.len() as u64 != manifest.size || logical.len() as u64 != rep.decoded_size {
            return Err(Error::corrupt(
                "tensor representation decoded length disagrees with the manifest",
            ));
        }
        if offset >= manifest.size {
            return Ok(Vec::new());
        }
        let end = offset
            .checked_add(length)
            .map(|value| value.min(manifest.size))
            .ok_or_else(|| Error::corrupt("tensor range end overflows"))?;
        let start = usize::try_from(offset)
            .map_err(|_| Error::limit_exceeded("tensor range offset does not fit usize"))?;
        let stop = usize::try_from(end)
            .map_err(|_| Error::limit_exceeded("tensor range end does not fit usize"))?;
        let out = logical[start..stop].to_vec();
        if verify_whole && offset == 0 && end == manifest.size {
            if identity::content_hash(&out) != manifest.content_hash {
                return Err(Error::corrupt("whole-content hash mismatch"));
            }
        }
        Ok(out)
    }

    fn decode_mode10(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        limits: &Limits,
    ) -> Result<Vec<u8>> {
        let shard = weftcodec::decode_shard_frame(frame, limits)?;
        let shard_dtype = match shard.meta.dtype {
            weftcodec::ShardedDtype::Q8_0 => "q8_0",
            weftcodec::ShardedDtype::Q4_0 => "q4_0",
        };
        if shard_dtype != tensor.dtype.as_str() {
            return Err(Error::corrupt(format!(
                "mode-10 shard dtype `{shard_dtype}` disagrees with manifest dtype `{}`",
                tensor.dtype.as_str()
            )));
        }
        if shard.meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-10 shard shape disagrees with the manifest",
            ));
        }
        if shard.meta.shard_count != 1 {
            return Err(Error::corrupt(format!(
                "a kind:\"tensor\" manifest names one frame; mode-10 shard_count is {}",
                shard.meta.shard_count
            )));
        }
        if shard.meta.element_count != manifest.size {
            return Err(Error::corrupt(
                "mode-10 element count disagrees with the manifest size",
            ));
        }
        if shard.meta.layout_root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-10 declared layout_root disagrees with the manifest logical_root",
            ));
        }
        let words = shard.words.clone();
        let root = weftcodec::tensor_sharded::logical_root(
            shard.meta.dtype,
            &shard.meta.shape,
            &words,
            &shard.meta.scales,
            &shard.meta.zero_points,
            limits,
        )?;
        if root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-10 logical root recomputation mismatch",
            ));
        }
        Ok(words.iter().map(|word| *word as u8).collect())
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_delta(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        rep: &Representation,
        limits: &Limits,
        depth: usize,
        seen: &mut Vec<Hash32>,
        semantic: bool,
    ) -> Result<Vec<u8>> {
        let (descriptor_dtype, descriptor_shape, base_hash) = if semantic {
            let meta = weftcodec::semantic_delta_metadata(frame, limits)?;
            (
                TensorElementDtype::from_weight_name(meta.dtype.as_str())?,
                meta.shape,
                meta.base,
            )
        } else {
            let meta = weftcodec::weight_delta_metadata(frame, limits)?;
            (
                TensorElementDtype::from_weight_name(meta.dtype.as_str())?,
                meta.shape,
                meta.base,
            )
        };
        if descriptor_dtype != tensor.dtype {
            return Err(Error::corrupt(format!(
                "delta dtype `{}` disagrees with manifest dtype `{}`",
                descriptor_dtype.as_str(),
                tensor.dtype.as_str()
            )));
        }
        if descriptor_shape != tensor.shape {
            return Err(Error::corrupt(
                "delta shape disagrees with the manifest shape",
            ));
        }
        let dep_root = rep
            .dependencies
            .first()
            .copied()
            .ok_or_else(|| Error::corrupt("delta representation has no dependency"))?;
        if seen.contains(&dep_root) {
            return Err(Error::corrupt("tensor representation dependency cycle"));
        }
        let dep_manifest = recovery::read_manifest(self.root(), dep_root)?;
        seen.push(dep_root);
        let base_bytes = self.read_manifest_bytes_inner(
            &dep_manifest,
            0,
            dep_manifest.size,
            true,
            depth + 1,
            seen,
        )?;
        seen.pop();
        let element_count = weftcodec::tensor::validate_shape(&tensor.shape, limits)?;
        let expected = element_count
            .checked_mul(tensor.dtype.element_bytes())
            .ok_or_else(|| Error::limit_exceeded("delta base length overflows"))?;
        if base_bytes.len() as u64 != expected {
            return Err(Error::corrupt(
                "delta base length disagrees with the tensor dtype/shape",
            ));
        }
        if identity::content_hash(&base_bytes) != base_hash {
            return Err(Error::corrupt(
                "delta base content hash disagrees with the frame descriptor",
            ));
        }
        let base_frame = mode6_base_frame(&base_bytes, tensor.dtype, &tensor.shape)?;
        let logical = if semantic {
            weftcodec::decode_semantic_delta_with_base(
                frame,
                &base_frame,
                &MapResolver::new(),
                limits,
            )?
        } else {
            weftcodec::decode_with_base_frame(frame, &base_frame, &MapResolver::new(), limits)?
        };
        if logical.len() as u64 != rep.decoded_size {
            return Err(Error::corrupt(
                "delta frame decoded length disagrees with the manifest",
            ));
        }
        let physical = tensor
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("delta dtype has no logical root"))?;
        let root = weftcodec::tensor::logical_root(physical, &tensor.shape, &logical, limits)?;
        if root != tensor.logical_root {
            return Err(Error::corrupt("delta logical root recomputation mismatch"));
        }
        Ok(logical)
    }

    fn decode_kv_page(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        limits: &Limits,
    ) -> Result<Vec<u8>> {
        let page = weftcodec::decode_kv_page(frame, limits)?;
        let dtype = TensorElementDtype::from_weight_name(page.meta.dtype.as_str())?;
        if dtype != tensor.dtype || page.meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "kv-cache descriptor dtype/shape disagree with the manifest",
            ));
        }
        let physical = dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("kv-cache dtype has no logical root"))?;
        let root = weftcodec::tensor::logical_root(physical, &tensor.shape, &page.bytes, limits)?;
        if root != tensor.logical_root {
            return Err(Error::corrupt(
                "kv-cache logical root recomputation mismatch",
            ));
        }
        Ok(page.bytes)
    }

    fn decode_activation(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        limits: &Limits,
    ) -> Result<Vec<u8>> {
        let chunk = weftcodec::decode_activation_chunk(frame, limits)?;
        let dtype = TensorElementDtype::from_weight_name(chunk.meta.dtype.as_str())?;
        if dtype != tensor.dtype || chunk.meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "activation descriptor dtype/shape disagree with the manifest",
            ));
        }
        let physical = dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("activation dtype has no logical root"))?;
        let root = weftcodec::tensor::logical_root(physical, &tensor.shape, &chunk.bytes, limits)?;
        if root != tensor.logical_root {
            return Err(Error::corrupt(
                "activation logical root recomputation mismatch",
            ));
        }
        Ok(chunk.bytes)
    }
}

fn skipped(strategy: TensorStrategy, detail: String) -> PreparedCandidate {
    PreparedCandidate {
        candidate: TensorPlanCandidate {
            strategy,
            applicable: false,
            detail,
            physical_bytes: None,
            estimated_decode_ns: None,
            cost: None,
            dependency: None,
        },
        artifact: None,
    }
}

fn apply_tensor_attrs(
    attrs: &mut BTreeMap<String, Value>,
    dtype: TensorElementDtype,
    shape: &[u64],
    root: Hash32,
    layout: &TensorLayoutSpec,
    strategy: &str,
) -> Result<()> {
    attrs.insert(
        ATTR_TENSOR_DTYPE.to_owned(),
        Value::Str(dtype.as_str().to_owned()),
    );
    attrs.insert(
        ATTR_TENSOR_SHAPE.to_owned(),
        Value::Array(shape.iter().map(|dim| Value::U64(*dim)).collect()),
    );
    attrs.insert(
        ATTR_TENSOR_LOGICAL_ROOT.to_owned(),
        Value::Bytes(root.as_bytes().to_vec()),
    );
    attrs.insert(
        ATTR_TENSOR_LAYOUT.to_owned(),
        Value::Bytes(wu::encode(&layout.to_value(dtype, shape)?)?),
    );
    attrs.insert(
        ATTR_TENSOR_STRATEGY.to_owned(),
        Value::Str(strategy.to_owned()),
    );
    Ok(())
}

/// Copy a region into a row-major tensor buffer.
fn splice_region(
    logical: &mut [u8],
    shape: &[u64],
    dtype: TensorElementDtype,
    region: &Region,
    bytes: &[u8],
) -> Result<()> {
    let element_bytes = dtype.element_bytes() as usize;
    let extent = region.extent();
    let total: u64 = extent.iter().product();
    if total == 0 {
        return Ok(());
    }
    let mut position = 0usize;
    // Iterate the region in row-major order.
    let mut index = vec![0u64; shape.len()];
    loop {
        let mut offset: u64 = 0;
        for axis in 0..shape.len() {
            offset = offset
                .checked_mul(shape[axis])
                .and_then(|value| value.checked_add(region.start[axis] + index[axis]))
                .ok_or_else(|| Error::limit_exceeded("region offset overflows"))?;
        }
        let start = usize::try_from(offset)
            .ok()
            .and_then(|value| value.checked_mul(element_bytes))
            .ok_or_else(|| Error::limit_exceeded("region byte offset overflows"))?;
        let end = start
            .checked_add(element_bytes)
            .ok_or_else(|| Error::limit_exceeded("region byte offset overflows"))?;
        if end > logical.len() || position + element_bytes > bytes.len() {
            return Err(Error::invalid_argument("region splice exceeds its buffers"));
        }
        logical[start..end].copy_from_slice(&bytes[position..position + element_bytes]);
        position += element_bytes;
        // Advance the row-major region index.
        let mut axis = shape.len();
        loop {
            if axis == 0 {
                return Ok(());
            }
            axis -= 1;
            index[axis] += 1;
            if index[axis] < extent[axis] {
                break;
            }
            index[axis] = 0;
        }
    }
}

impl Representation {
    fn clone_into_manifest(self, mut manifest: Manifest) -> Manifest {
        manifest.representation = Some(self);
        manifest
    }
}
