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
use std::sync::Arc;

use weftcodec::dependency::MapResolver;
use weftcodec::frame::{Frame, Mode};
use weftcodec::tensor::{PhysicalTensor, Region, RegionRead, TensorLayout};
use weftcodec::wcb::Value;
use weftcodec::{Limits, SemanticPolicy};

use crate::error::{Error, Result};
use crate::format::{Manifest, Representation, RepresentationKind, TensorRepresentation};
use crate::identity::{self, Hash32};
use crate::parallel::{run_bounded, BoundedRunStats, TileDecodeOptions, TileDecodeStats};
use crate::recovery::{self, BlobKind};
use crate::representation::{TensorElementDtype, MAX_CODEC_DEPENDENCY_DEPTH};
use crate::singleflight::CancelToken;
use crate::state::{ObjectKind, ObjectRecord, State};
use crate::store::{AuthorizedRead, Store};
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
/// Reserved object attribute holding the derived/explicit mode-15 tile extents
/// as an array of u64. Absent for every non-tiled representation.
pub const ATTR_TENSOR_TILE_SHAPE: &str = "weft.tensor.tile_shape";

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
    /// One mode-9 `FLOAT_TENSOR` frame (v2 `kind:"tensor"`).
    FloatTensor,
    /// One mode-10 `TENSOR_SHARDED` frame.
    TensorSharded,
    /// One mode-15 `TENSOR_TILED` frame.
    TensorTiled,
    /// One mode-17 `FLOAT_TILED` frame: per-tile mode-9 plane split.
    FloatTiled,
    /// One mode-11 `WEIGHT_DELTA` frame against the causal parent.
    WeightDelta,
    /// One mode-16 `SPARSE_DELTA` frame against the causal parent. The same
    /// exact word-wise delta as mode 11, stored as a zero-run stream; it is
    /// cheaper only when the delta is sparse.
    SparseDelta,
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
            TensorStrategy::FloatTensor => "float-tensor",
            TensorStrategy::TensorSharded => "tensor-sharded",
            TensorStrategy::TensorTiled => "tensor-tiled",
            TensorStrategy::FloatTiled => "float-tiled",
            TensorStrategy::WeightDelta => "weight-delta",
            TensorStrategy::SparseDelta => "sparse-delta",
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
            "float-tensor" => Ok(TensorStrategy::FloatTensor),
            "tensor-sharded" => Ok(TensorStrategy::TensorSharded),
            "tensor-tiled" => Ok(TensorStrategy::TensorTiled),
            "float-tiled" => Ok(TensorStrategy::FloatTiled),
            "weight-delta" => Ok(TensorStrategy::WeightDelta),
            "sparse-delta" => Ok(TensorStrategy::SparseDelta),
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
            TensorStrategy::FloatTensor => Some(9),
            TensorStrategy::TensorSharded => Some(10),
            TensorStrategy::TensorTiled => Some(15),
            TensorStrategy::FloatTiled => Some(17),
            TensorStrategy::WeightDelta => Some(11),
            TensorStrategy::SparseDelta => Some(16),
            TensorStrategy::KvCache => Some(12),
            TensorStrategy::Activation => Some(13),
            TensorStrategy::SemanticDelta => Some(14),
        }
    }

    /// True for representations that carry a base content root.
    pub fn uses_dependency(self) -> bool {
        matches!(
            self,
            TensorStrategy::WeightDelta
                | TensorStrategy::SparseDelta
                | TensorStrategy::SemanticDelta
        )
    }

    /// True when a representation stands alone (no base frame). Rebase
    /// requires one of these; a delta is the thing being rebased away.
    pub fn is_independent(self) -> bool {
        !self.uses_dependency()
    }

    /// Cost-model class for this strategy.
    pub fn cost_class(self) -> crate::cost_model::CostClass {
        use crate::cost_model::CostClass;
        match self {
            TensorStrategy::Raw => CostClass::Raw,
            TensorStrategy::Zstd => CostClass::Zstd,
            TensorStrategy::FloatTensor => CostClass::FloatTensor,
            TensorStrategy::TensorSharded => CostClass::TensorSharded,
            TensorStrategy::TensorTiled => CostClass::TensorTiled,
            // Mode 17 is priced with the mode-15 tile coefficients: it runs
            // the same per-tile geometry and a Huffman exponent decode, plus
            // the BF16 join. The calibration schema's class set is frozen, so
            // no new cost class was added.
            TensorStrategy::FloatTiled => CostClass::TensorTiled,
            TensorStrategy::WeightDelta => CostClass::WeightDelta,
            TensorStrategy::SparseDelta => CostClass::SparseDelta,
            TensorStrategy::SemanticDelta => CostClass::SemanticDelta,
            TensorStrategy::KvCache => CostClass::KvCache,
            TensorStrategy::Activation => CostClass::Activation,
        }
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

/// Planner options. The default is bytes-only (`lambda_ns = 0`) with the
/// host-calibrated decode cost table.
#[derive(Clone, Debug)]
pub struct TensorPlanOptions {
    /// Bytes charged per estimated nanosecond of decode time. `0` is
    /// bytes-only.
    pub lambda_ns: u64,
    /// Which baked decode-cost table prices candidates. Ignored when
    /// `calibration` is set.
    pub cost_model: crate::cost_model::CostModelKind,
    /// Optional host-local calibration table. `None` uses the baked
    /// `cost_model` rows; `Some` replaces the entire table, so a calibration
    /// must be complete (the loader enforces that).
    pub calibration: Option<Arc<crate::cost_model::CostTable>>,
    /// Semantic grouping policy for the semantic-delta candidate. `None` uses
    /// `mantissa_truncate:3` for the floating dtypes.
    pub semantic_policy: Option<SemanticPolicy>,
    /// Bound on the semantic candidate search.
    pub max_candidates: usize,
    /// Explicit candidate tensor ObjectIds. When non-empty, the search uses
    /// exactly this list instead of scanning live tensor objects.
    pub candidates: Vec<Hash32>,
    /// Tile extents for the mode-15 `TENSOR_TILED` candidate. `None` skips
    /// mode 15 entirely unless `auto_tile_bytes` is set, which keeps existing
    /// objects on their current representations.
    pub tile_shape: Option<Vec<u64>>,
    /// Opt-in automatic mode-15 tile derivation. When set and `tile_shape` is
    /// `None`, the planner derives a dividing tile shape whose canonical tile
    /// bytes fit in this budget and offers mode 15 alongside the other
    /// candidates. `None` (the default) never offers mode 15 implicitly.
    pub auto_tile_bytes: Option<u64>,
}

impl Default for TensorPlanOptions {
    fn default() -> Self {
        TensorPlanOptions {
            lambda_ns: 0,
            cost_model: crate::cost_model::CostModelKind::default(),
            calibration: None,
            semantic_policy: None,
            max_candidates: DEFAULT_TENSOR_MAX_CANDIDATES,
            candidates: Vec::new(),
            tile_shape: None,
            auto_tile_bytes: None,
        }
    }
}

impl TensorPlanOptions {
    /// Price one class: the host calibration when present, otherwise the baked
    /// table. Both use the same integer arithmetic.
    pub fn estimate_ns(&self, class: crate::cost_model::CostClass, size: u64) -> u64 {
        match &self.calibration {
            Some(table) => table.estimate_ns(class, size),
            None => crate::cost_model::estimate_ns(class, size, self.cost_model),
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
    /// Tile extents for a mode-15 `TENSOR_TILED` write. `None` never offers
    /// the tiled strategy; the caller opts in explicitly.
    pub tile_shape: Option<Vec<u64>>,
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

/// The priced decision among standalone representations (rebase input).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorIndependentPlan {
    pub chosen: TensorStrategy,
    pub physical_bytes: u64,
    pub estimated_decode_ns: u64,
    pub cost: u128,
    pub candidates: Vec<TensorPlanCandidate>,
}

/// Result of re-publishing one object with a standalone representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorRebase {
    pub old_version: Hash32,
    pub new_version: Hash32,
    /// Manifest id of the previous version (the old content root).
    pub old_content: Hash32,
    /// Manifest id of the new version (the new content root).
    pub new_content: Hash32,
    pub old_strategy: String,
    pub plan: TensorIndependentPlan,
}

/// Tensor identity recovered from an object record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredTensorMetadata {
    pub dtype: TensorElementDtype,
    pub shape: Vec<u64>,
    pub logical_root: Hash32,
    pub layout: TensorLayoutSpec,
    pub strategy: String,
    /// Tile extents recorded for a mode-15 `TENSOR_TILED` representation.
    /// `None` for every other representation and for mode-15 objects written
    /// before the attribute existed.
    pub tile_shape: Option<Vec<u64>>,
}

#[derive(Clone, Debug)]
struct PreparedArtifact {
    kind: RepresentationKind,
    strategy_name: String,
    frame: Option<Vec<u8>>,
    dependencies: Vec<Hash32>,
    tensor: Option<TensorRepresentation>,
    /// Tile extents for a mode-15 artifact, recorded in the object attributes.
    tile_shape: Option<Vec<u64>>,
}

#[derive(Clone, Debug)]
struct PreparedCandidate {
    candidate: TensorPlanCandidate,
    artifact: Option<PreparedArtifact>,
}

fn price(physical_bytes: u64, estimated_ns: u64, lambda_ns: u64) -> u128 {
    u128::from(physical_bytes) + u128::from(lambda_ns) * u128::from(estimated_ns)
}

/// Project the generic bounded-run counters onto the tile-facing stats type.
fn tile_stats(run: &BoundedRunStats) -> TileDecodeStats {
    TileDecodeStats {
        requested_threads: run.requested_threads,
        effective_threads: run.effective_threads,
        tiles: run.tasks,
        max_inflight_bytes: run.max_inflight_bytes,
        observed_max_inflight_bytes: run.observed_max_inflight_bytes,
        observed_max_inflight_tasks: run.observed_max_inflight_tasks,
        oversized_tiles: run.oversized_tasks,
    }
}

/// Ascending positive divisors of `n` (`n >= 1`).
fn positive_divisors(n: u64) -> Vec<u64> {
    let mut small = Vec::new();
    let mut large = Vec::new();
    let mut d = 1u64;
    while d.checked_mul(d).map(|square| square <= n).unwrap_or(false) {
        if n % d == 0 {
            small.push(d);
            let other = n / d;
            if other != d {
                large.push(other);
            }
        }
        d += 1;
    }
    large.reverse();
    small.extend(large);
    small
}

/// `(max, min)` of a tile pair; `min` is never zero because divisors are >= 1.
fn tile_balance(a: u64, b: u64) -> (u64, u64) {
    if a >= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Derive a mode-15 tile shape from a byte budget.
///
/// Mode 15 defines full tiles only, so every tile extent must divide the
/// corresponding logical extent exactly. This searches the divisors of each
/// extent and chooses the pair whose element count is the largest at or below
/// `budget_bytes / dtype.element_bytes()`. Ties go to the more balanced pair
/// (the smaller max/min extent ratio, compared without floats) and then to the
/// lexicographically smaller pair.
///
/// v1 implements this for rank-2 tensors; any other rank returns a typed error
/// naming the shape rather than guessing a padding rule.
pub fn derive_auto_tile_shape(
    shape: &[u64],
    dtype: TensorElementDtype,
    budget_bytes: u64,
) -> Result<Vec<u64>> {
    if shape.len() != 2 {
        return Err(Error::invalid_argument(format!(
            "auto-tile derivation requires a rank-2 tensor; shape {shape:?} has rank {}",
            shape.len()
        )));
    }
    let (rows, cols) = (shape[0], shape[1]);
    if rows == 0 || cols == 0 {
        return Err(Error::invalid_argument(format!(
            "auto-tile derivation needs non-zero extents, got {rows} and {cols}"
        )));
    }
    let element_bytes = dtype.element_bytes();
    let capacity = budget_bytes / element_bytes;
    if capacity == 0 {
        return Err(Error::invalid_argument(format!(
            "auto-tile budget {budget_bytes} B is smaller than one {} element ({element_bytes} B) \
             for extents {rows} and {cols}",
            dtype.as_str()
        )));
    }
    let row_divisors = positive_divisors(rows);
    let col_divisors = positive_divisors(cols);
    let mut best: Option<(u64, u64, u64)> = None;
    for &r in &row_divisors {
        for &c in &col_divisors {
            let Some(elements) = r.checked_mul(c) else {
                continue;
            };
            if elements > capacity {
                continue;
            }
            let candidate = (elements, r, c);
            let replace = match best {
                None => true,
                Some(current) => {
                    if candidate.0 != current.0 {
                        candidate.0 > current.0
                    } else {
                        let (c_max, c_min) = tile_balance(candidate.1, candidate.2);
                        let (b_max, b_min) = tile_balance(current.1, current.2);
                        let left = u128::from(c_max) * u128::from(b_min);
                        let right = u128::from(b_max) * u128::from(c_min);
                        if left != right {
                            left < right
                        } else {
                            (candidate.1, candidate.2) < (current.1, current.2)
                        }
                    }
                }
            };
            if replace {
                best = Some(candidate);
            }
        }
    }
    let (_, r, c) = best.ok_or_else(|| {
        Error::invalid_argument(format!(
            "no divisor pair of extents {rows} and {cols} fits the {capacity}-element tile budget \
             ({budget_bytes} B / {element_bytes} B per {} element)",
            dtype.as_str()
        ))
    })?;
    Ok(vec![r, c])
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
    // Prefer the largest block that divides the payload: the scale plane costs
    // one 4-byte record per block, so 16-element blocks would add 25 % metadata
    // to every q8_0 object while 256-element blocks add about 1.6 %.
    [256u64, 128, 64, 32, 16]
        .into_iter()
        .find(|block| element_count % *block == 0)
}

fn words_from_bytes(bytes: &[u8]) -> Vec<i8> {
    bytes.iter().map(|byte| *byte as i8).collect()
}

/// Mode-9 float layout for a manifest dtype, when the mode implements it.
fn float_tensor_dtype(dtype: TensorElementDtype) -> Option<weftcodec::modes::FloatDtype> {
    match dtype {
        TensorElementDtype::F16 => Some(weftcodec::modes::FloatDtype::F16),
        TensorElementDtype::Bf16 => Some(weftcodec::modes::FloatDtype::Bf16),
        TensorElementDtype::F32 => Some(weftcodec::modes::FloatDtype::F32),
        _ => None,
    }
}

/// True when a mode-17 descriptor dtype names the manifest's element dtype.
fn float_dtype_matches_tensor(
    float: weftcodec::modes::FloatDtype,
    tensor: TensorElementDtype,
) -> bool {
    float_tensor_dtype(tensor) == Some(float)
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
        let prepared = self.prepare_candidates(
            dtype,
            shape,
            &logical,
            layout,
            &spec.causal,
            &spec.plan,
            spec.tile_shape
                .as_deref()
                .or(spec.plan.tile_shape.as_deref()),
            false,
        )?;
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
            artifact.tile_shape.as_deref(),
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
        let prepared = self.prepare_candidates(
            dtype,
            shape,
            &logical,
            layout,
            causal,
            options,
            options.tile_shape.as_deref(),
            false,
        )?;
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

    /// Price only the standalone representations of an existing tensor object.
    ///
    /// Used by the rebase planner: deltas are excluded, so the result is a
    /// representation that a read can decode without touching another object.
    pub fn plan_tensor_independent(
        &self,
        id: Hash32,
        options: &TensorPlanOptions,
        hint: Option<TensorStrategy>,
    ) -> Result<TensorIndependentPlan> {
        let meta = self.tensor_metadata(id)?;
        let logical = self.read_object_uncounted(id)?;
        let prepared = self.prepare_candidates(
            meta.dtype,
            &meta.shape,
            &logical,
            &meta.layout,
            &[],
            options,
            None,
            true,
        )?;
        let chosen = self.choose_candidate(&prepared, hint)?;
        Ok(TensorIndependentPlan {
            chosen: chosen.candidate.strategy,
            physical_bytes: chosen.candidate.physical_bytes.unwrap_or(0),
            estimated_decode_ns: chosen.candidate.estimated_decode_ns.unwrap_or(0),
            cost: chosen.candidate.cost.unwrap_or(0),
            candidates: prepared.iter().map(|item| item.candidate.clone()).collect(),
        })
    }

    /// Re-publish an existing tensor object with an independent representation.
    ///
    /// The logical bytes are unchanged, so the object's content root and
    /// `LogicalTensorRoot` are unchanged; a **new version** is published and
    /// the previous version and every snapshot stay intact. The caller records
    /// provenance and consumes the read counter.
    pub fn rebase_tensor_object(
        &mut self,
        id: Hash32,
        options: &TensorPlanOptions,
        hint: Option<TensorStrategy>,
    ) -> Result<TensorRebase> {
        self.require_writable()?;
        let meta = self.tensor_metadata(id)?;
        let old = self.object_meta(id)?;
        if old.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "rebase requires a regular file object",
            ));
        }
        let logical = self.read_object_uncounted(id)?;
        let prepared = self.prepare_candidates(
            meta.dtype,
            &meta.shape,
            &logical,
            &meta.layout,
            &[],
            options,
            None,
            true,
        )?;
        let chosen = self.choose_candidate(&prepared, hint)?;
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
            artifact.tile_shape.as_deref(),
        )?;
        let causal = vec![old.version];
        let old_content = old.content;
        let new_version = self.publish_file_version_with_attrs(
            id,
            old.clone(),
            manifest_id,
            content_hash,
            size,
            &causal,
            attrs,
        )?;
        let new_content = self.object_meta(id)?.content;
        Ok(TensorRebase {
            old_version: old.version,
            new_version,
            old_content,
            new_content,
            old_strategy: meta.strategy,
            plan: TensorIndependentPlan {
                chosen: chosen.candidate.strategy,
                physical_bytes: chosen.candidate.physical_bytes.unwrap_or(0),
                estimated_decode_ns: chosen.candidate.estimated_decode_ns.unwrap_or(0),
                cost: chosen.candidate.cost.unwrap_or(0),
                candidates: prepared.iter().map(|item| item.candidate.clone()).collect(),
            },
        })
    }

    /// Read one tensor object's canonical logical bytes.
    ///
    /// This is the default [`TileDecodeOptions`] form (`threads = 1`), so it is
    /// byte- and cost-equivalent to the shipped sequential read.
    pub fn read_tensor(&self, id: Hash32) -> Result<Vec<u8>> {
        self.read_tensor_with_options(id, &TileDecodeOptions::default())
    }

    /// Read one tensor object after authorization, through the R8 read layers.
    ///
    /// A `tensor_mode` 15 (`TENSOR_TILED`) object keeps its shipped tile path
    /// (and returns no per-request tile counters here); every other tensor
    /// representation is a whole-object decode and is served by the
    /// decoded-object cache / single-flight registry. Callers that need tile
    /// statistics for mode 15 must use
    /// [`Store::read_tensor_with_options_and_stats`].
    pub fn read_tensor_authorized(
        &self,
        authorized: &AuthorizedRead,
        options: &TileDecodeOptions,
    ) -> Result<Vec<u8>> {
        let record = self.object_meta(authorized.object_id())?;
        let manifest =
            self.with_backend(|backend| recovery::read_manifest_via(backend, record.content))?;
        if manifest.size != record.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        if let Some(rep) = &manifest.representation {
            if rep.kind == RepresentationKind::Tensor {
                if let Some(tensor) = &rep.tensor {
                    if tensor.tensor_mode == 15 || tensor.tensor_mode == 17 {
                        let frame_id = rep.frame.ok_or_else(|| {
                            Error::corrupt("tensor representation has no frame id")
                        })?;
                        return self
                            .read_tiled_object(frame_id, tensor, &manifest, rep, options)
                            .map(|(bytes, _stats)| bytes);
                    }
                }
            }
        }
        self.read_object_authorized(authorized)
    }

    /// Read one tensor object with an explicit mode-15 tile worker bound.
    ///
    /// A mode-15 `TENSOR_TILED` object decodes its tiles on the bounded pool and
    /// still verifies every tile root, the assembled logical root, and the
    /// whole-content hash. Every other representation has no independent tile
    /// unit, so it falls back to the shipped sequential read.
    pub fn read_tensor_with_options(
        &self,
        id: Hash32,
        options: &TileDecodeOptions,
    ) -> Result<Vec<u8>> {
        self.read_tensor_with_options_and_stats(id, options)
            .map(|(bytes, _stats)| bytes)
    }

    /// [`Store::read_tensor_with_options`] plus the observed tile counters.
    pub fn read_tensor_with_options_and_stats(
        &self,
        id: Hash32,
        options: &TileDecodeOptions,
    ) -> Result<(Vec<u8>, TileDecodeStats)> {
        let record = self.object_meta(id)?;
        let manifest =
            self.with_backend(|backend| recovery::read_manifest_via(backend, record.content))?;
        if manifest.size != record.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        if let Some(rep) = &manifest.representation {
            if rep.kind == RepresentationKind::Tensor {
                if let Some(tensor) = &rep.tensor {
                    if tensor.tensor_mode == 15 || tensor.tensor_mode == 17 {
                        let frame_id = rep.frame.ok_or_else(|| {
                            Error::corrupt("tensor representation has no frame id")
                        })?;
                        let (bytes, stats) =
                            self.read_tiled_object(frame_id, tensor, &manifest, rep, options)?;
                        self.note_object_read(id);
                        return Ok((bytes, stats));
                    }
                }
            }
        }
        // Non-tiled representation: the shipped sequential read, which verifies
        // the whole-content hash. `tensor_metadata` keeps the historical
        // "object is not a tensor" error for non-tensor files.
        self.tensor_metadata(id)?;
        let bytes = self.read_object(id)?;
        Ok((bytes, TileDecodeStats::none(options)))
    }

    /// Read a logical hyper-rectangle of a tensor object in row-major order.
    ///
    /// A mode-15 `TENSOR_TILED` object reads only the byte ranges of the tiles
    /// the region intersects and decodes only those tiles, in canonical tile
    /// order. Every other tensor representation is not independently seekable,
    /// so the whole object is decoded before the region is sliced; the returned
    /// region is exact and the touched-unit count is one.
    pub fn read_tensor_region(&self, id: Hash32, region: &Region) -> Result<RegionRead> {
        self.read_tensor_region_with_options(id, region, &TileDecodeOptions::default())
    }

    /// Region read with an explicit mode-15 tile worker bound.
    pub fn read_tensor_region_with_options(
        &self,
        id: Hash32,
        region: &Region,
        options: &TileDecodeOptions,
    ) -> Result<RegionRead> {
        self.read_tensor_region_with_options_and_stats(id, region, options)
            .map(|(read, _stats)| read)
    }

    /// [`Store::read_tensor_region_with_options`] plus the observed counters.
    pub fn read_tensor_region_with_options_and_stats(
        &self,
        id: Hash32,
        region: &Region,
        options: &TileDecodeOptions,
    ) -> Result<(RegionRead, TileDecodeStats)> {
        let record = self.object_meta(id)?;
        let manifest =
            self.with_backend(|backend| recovery::read_manifest_via(backend, record.content))?;
        if let Some(rep) = &manifest.representation {
            if rep.kind == RepresentationKind::Tensor {
                if let Some(tensor) = &rep.tensor {
                    if tensor.tensor_mode == 15 || tensor.tensor_mode == 17 {
                        let frame_id = rep.frame.ok_or_else(|| {
                            Error::corrupt("tensor representation has no frame id")
                        })?;
                        return self.read_tiled_region(frame_id, tensor, region, options);
                    }
                }
            }
        }
        let meta = self.tensor_metadata(id)?;
        let bytes = self.read_object(id)?;
        let physical = meta
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("tensor dtype has no physical layout"))?;
        let tensor = PhysicalTensor::row_major(physical, &meta.shape, bytes, &Limits::default())?;
        let read = tensor
            .read_region_with_limits(region, &Limits::default())
            .map_err(Error::from)?;
        Ok((read, TileDecodeStats::none(options)))
    }

    /// Decode a complete mode-15 object from its verified frame blob.
    fn read_tiled_object(
        &self,
        frame_id: Hash32,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        rep: &Representation,
        options: &TileDecodeOptions,
    ) -> Result<(Vec<u8>, TileDecodeStats)> {
        let (layout_dtype, layout_shape, _) = TensorLayoutSpec::from_value(&tensor.layout)?;
        if layout_dtype != tensor.dtype || layout_shape != tensor.shape {
            return Err(Error::corrupt(
                "tensor layout descriptor disagrees with the manifest dtype/shape",
            ));
        }
        let frame_bytes = self.with_backend(|backend| {
            recovery::read_blob_verified_via(
                backend,
                BlobKind::Manifest,
                frame_id,
                identity::MANIFEST_DOMAIN,
            )
        })?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        let expected = match tensor.tensor_mode {
            15 => Mode::TensorTiled,
            17 => Mode::FloatTiled,
            other => {
                return Err(Error::corrupt(format!(
                    "tensor mode {other} is not a tiled representation"
                )))
            }
        };
        if frame.header.mode != expected {
            return Err(Error::corrupt(format!(
                "tensor representation mode {} has frame mode `{}`",
                tensor.tensor_mode,
                frame.header.mode.as_str()
            )));
        }
        let (logical, stats) = if tensor.tensor_mode == 17 {
            self.decode_mode17_with_options(&frame, tensor, manifest, rep, options)?
        } else {
            self.decode_mode15_with_options(&frame, tensor, manifest, rep, options)?
        };
        if identity::content_hash(&logical) != manifest.content_hash {
            return Err(Error::corrupt("whole-content hash mismatch"));
        }
        Ok((logical, stats))
    }

    /// Region read for a mode-15 object using per-tile backend range reads.
    ///
    /// Only the frame prefix (header plus descriptor) and the stored ranges of
    /// intersecting tiles are read. Each decoded tile is verified against its
    /// own `tile_root`; the descriptor's dtype, shape and logical root are
    /// checked against the representation manifest before any tile is read.
    /// This path deliberately does **not** re-verify the whole-frame trailer or
    /// content hash: it ranges over a frame it never reads in full.
    fn read_tiled_region(
        &self,
        frame_id: Hash32,
        tensor: &TensorRepresentation,
        region: &Region,
        options: &TileDecodeOptions,
    ) -> Result<(RegionRead, TileDecodeStats)> {
        if tensor.tensor_mode == 17 {
            return self.read_float_tiled_region(frame_id, tensor, region, options);
        }
        let limits = Limits::default();
        let key = crate::backend::Key::manifest(frame_id);

        // Probe the fixed header plus the 4-byte descriptor length.
        let probe = self.with_backend(|backend| backend.read(&key, 0, 100))?;
        if probe.len() < 100 {
            return Err(Error::corrupt(
                "mode-15 frame is shorter than its header and descriptor length",
            ));
        }
        let (header, _) = weftcodec::frame::parse_prefix(&probe, &limits)?;
        if header.mode != Mode::TensorTiled {
            return Err(Error::corrupt(format!(
                "tensor representation mode 15 has frame mode `{}`",
                header.mode.as_str()
            )));
        }
        if header.table_len != 0 || header.dep_count != 0 || header.chunk_count != 0 {
            return Err(Error::corrupt(
                "mode-15 frame must not carry dependency or chunk tables",
            ));
        }
        // AI-mode frames have table_len 0, so the body starts at byte 96.
        let meta_len = u64::from(u32::from_le_bytes([
            probe[96], probe[97], probe[98], probe[99],
        ]));
        if meta_len
            .checked_add(4)
            .map(|end| end > header.body_len)
            .unwrap_or(true)
        {
            return Err(Error::corrupt(
                "mode-15 descriptor length extends past the frame body",
            ));
        }
        weftcodec::limits::check_len(meta_len, limits.max_string_bytes, "mode-15 descriptor")?;
        let prefix_len = 96u64
            .checked_add(4)
            .and_then(|value| value.checked_add(meta_len))
            .ok_or_else(|| Error::limit_exceeded("mode-15 prefix length overflows"))?;
        let prefix = if probe.len() as u64 >= prefix_len {
            probe
        } else {
            self.with_backend(|backend| backend.read(&key, 0, prefix_len))?
        };
        if prefix.len() as u64 != prefix_len {
            return Err(Error::corrupt("mode-15 frame prefix is truncated"));
        }
        let descriptor_bytes = &prefix[100..];
        let stored_payload_len = header.body_len - 4 - meta_len;
        let meta = weftcodec::modes::parse_tiled_descriptor(
            descriptor_bytes,
            header.decoded_len,
            stored_payload_len,
            &limits,
        )?;
        let physical = tensor
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("mode-15 dtype has no element layout"))?;
        if meta.dtype != physical {
            return Err(Error::corrupt(format!(
                "mode-15 dtype `{}` disagrees with manifest dtype `{}`",
                meta.dtype.as_str(),
                tensor.dtype.as_str()
            )));
        }
        if meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-15 shape disagrees with the representation manifest",
            ));
        }
        if meta.logical_root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-15 logical_root disagrees with the manifest logical_root",
            ));
        }

        let indices = weftcodec::tiled_region_indices(&meta, region)?;
        if indices.is_empty() {
            return Ok((
                RegionRead {
                    shape: region.extent(),
                    bytes: Vec::new(),
                    units_touched: 0,
                },
                TileDecodeStats::none(options),
            ));
        }
        let extent = region.extent();
        let element_size = meta.dtype.element_size();
        let count = extent.iter().try_fold(1u64, |acc, dim| {
            acc.checked_mul(*dim)
                .ok_or_else(|| Error::limit_exceeded("region element count overflows u64"))
        })?;
        let byte_len = count
            .checked_mul(element_size)
            .ok_or_else(|| Error::limit_exceeded("region payload length overflows u64"))?;
        weftcodec::limits::check_len(byte_len, limits.max_decoded_bytes, "region payload")?;
        let byte_len = usize::try_from(byte_len)
            .map_err(|_| Error::limit_exceeded("region payload does not fit usize"))?;
        let mut out = vec![0u8; byte_len];

        let sizes: Vec<Result<usize>> = indices
            .iter()
            .map(|index| {
                usize::try_from(meta.tiles[*index].element_len)
                    .map_err(|_| Error::limit_exceeded("tensor tile length does not fit usize"))
            })
            .collect();
        let payload_base = prefix_len;
        let (tiles, run) = run_bounded(
            &indices,
            &sizes,
            options.threads,
            options.max_inflight_bytes,
            |_position, index| {
                let entry = &meta.tiles[*index];
                let offset = payload_base
                    .checked_add(entry.offset)
                    .ok_or_else(|| Error::limit_exceeded("tile offset overflows"))?;
                let stored =
                    self.with_backend(|backend| backend.read(&key, offset, entry.compressed_len))?;
                weftcodec::decode_tiled_tile(meta.dtype, entry, &stored, &limits)
                    .map_err(Error::from)
            },
            |tile| tile.len(),
        )?;
        for (index, tile) in indices.iter().zip(tiles.iter()) {
            weftcodec::place_tiled_tile_region(&mut out, region, &meta, &meta.tiles[*index], tile)?;
        }
        Ok((
            RegionRead {
                shape: extent,
                bytes: out,
                units_touched: indices.len() as u64,
            },
            tile_stats(&run),
        ))
    }

    /// Region read for a mode-17 float-tiled object.
    ///
    /// Only the frame prefix (header plus descriptor) and the stored ranges of
    /// intersecting tiles are read. Each decoded tile is verified against its
    /// own `tile_root`; dtype, shape and logical root are checked against the
    /// representation manifest before any tile is read. Like the mode-15 path
    /// this does not re-verify the whole-frame trailer or content hash.
    fn read_float_tiled_region(
        &self,
        frame_id: Hash32,
        tensor: &TensorRepresentation,
        region: &Region,
        options: &TileDecodeOptions,
    ) -> Result<(RegionRead, TileDecodeStats)> {
        let limits = Limits::default();
        let key = crate::backend::Key::manifest(frame_id);
        let probe = self.with_backend(|backend| backend.read(&key, 0, 100))?;
        if probe.len() < 100 {
            return Err(Error::corrupt(
                "mode-17 frame is shorter than its header and descriptor length",
            ));
        }
        let (header, _) = weftcodec::frame::parse_prefix(&probe, &limits)?;
        if header.mode != Mode::FloatTiled {
            return Err(Error::corrupt(format!(
                "tensor representation mode 17 has frame mode `{}`",
                header.mode.as_str()
            )));
        }
        if header.table_len != 0 || header.dep_count != 0 || header.chunk_count != 0 {
            return Err(Error::corrupt(
                "mode-17 frame must not carry dependency or chunk tables",
            ));
        }
        let meta_len = u64::from(u32::from_le_bytes([
            probe[96], probe[97], probe[98], probe[99],
        ]));
        if meta_len
            .checked_add(4)
            .map(|end| end > header.body_len)
            .unwrap_or(true)
        {
            return Err(Error::corrupt(
                "mode-17 descriptor length extends past the frame body",
            ));
        }
        weftcodec::limits::check_len(meta_len, limits.max_string_bytes, "mode-17 descriptor")?;
        let prefix_len = 96u64
            .checked_add(4)
            .and_then(|value| value.checked_add(meta_len))
            .ok_or_else(|| Error::limit_exceeded("mode-17 prefix length overflows"))?;
        let prefix = if probe.len() as u64 >= prefix_len {
            probe
        } else {
            self.with_backend(|backend| backend.read(&key, 0, prefix_len))?
        };
        if prefix.len() as u64 != prefix_len {
            return Err(Error::corrupt("mode-17 frame prefix is truncated"));
        }
        let descriptor_bytes = &prefix[100..];
        let stored_payload_len = header.body_len - 4 - meta_len;
        let meta = weftcodec::modes::parse_float_tiled_descriptor(
            descriptor_bytes,
            header.decoded_len,
            stored_payload_len,
            &limits,
        )?;
        if !float_dtype_matches_tensor(meta.dtype, tensor.dtype) {
            return Err(Error::corrupt(format!(
                "mode-17 dtype `{}` disagrees with manifest dtype `{}`",
                meta.dtype.as_str(),
                tensor.dtype.as_str()
            )));
        }
        if meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-17 shape disagrees with the representation manifest",
            ));
        }
        if meta.logical_root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-17 logical_root disagrees with the manifest logical_root",
            ));
        }
        let indices = weftcodec::float_tiled_region_indices(&meta, region)?;
        if indices.is_empty() {
            return Ok((
                RegionRead {
                    shape: region.extent(),
                    bytes: Vec::new(),
                    units_touched: 0,
                },
                TileDecodeStats::none(options),
            ));
        }
        let extent = region.extent();
        let element_size = meta.dtype.element_size();
        let count = extent.iter().try_fold(1u64, |acc, dim| {
            acc.checked_mul(*dim)
                .ok_or_else(|| Error::limit_exceeded("region element count overflows u64"))
        })?;
        let byte_len = count
            .checked_mul(element_size)
            .ok_or_else(|| Error::limit_exceeded("region payload length overflows u64"))?;
        weftcodec::limits::check_len(byte_len, limits.max_decoded_bytes, "region payload")?;
        let byte_len = usize::try_from(byte_len)
            .map_err(|_| Error::limit_exceeded("region payload does not fit usize"))?;
        let mut out = vec![0u8; byte_len];
        let sizes: Vec<Result<usize>> = indices
            .iter()
            .map(|index| {
                usize::try_from(meta.tiles[*index].element_len)
                    .map_err(|_| Error::limit_exceeded("float tile length does not fit usize"))
            })
            .collect();
        let payload_base = prefix_len;
        let (tiles, run) = run_bounded(
            &indices,
            &sizes,
            options.threads,
            options.max_inflight_bytes,
            |_position, index| {
                let entry = &meta.tiles[*index];
                let offset = payload_base
                    .checked_add(entry.offset)
                    .ok_or_else(|| Error::limit_exceeded("float tile offset overflows"))?;
                let stored =
                    self.with_backend(|backend| backend.read(&key, offset, entry.compressed_len))?;
                weftcodec::decode_float_tile(meta.dtype, entry, &stored, &limits)
                    .map_err(Error::from)
            },
            |tile| tile.len(),
        )?;
        for (index, tile) in indices.iter().zip(tiles.iter()) {
            weftcodec::place_float_tile_region(&mut out, region, &meta, &meta.tiles[*index], tile)?;
        }
        Ok((
            RegionRead {
                shape: extent,
                bytes: out,
                units_touched: indices.len() as u64,
            },
            tile_stats(&run),
        ))
    }

    /// Recompute and verify an object's `LogicalTensorRoot`.
    pub fn tensor_logical_root(&self, id: Hash32) -> Result<Hash32> {
        let record = self.object_meta(id)?;
        let meta = self.tensor_metadata_record(&record)?;
        let bytes = self.read_object(id)?;
        let root = if meta.dtype == TensorElementDtype::Q8_0 {
            let shard = self.q8_0_shard_meta_record(&record)?;
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

    /// Recompute and verify a tensor's `LogicalTensorRoot` at a pinned snapshot.
    ///
    /// The record, representation manifest, and decoded bytes all come from the
    /// pinned state; the live head is never consulted. This is the form used by
    /// training-ready materialization.
    pub fn tensor_logical_root_at(
        &self,
        state: &State,
        snapshot: Hash32,
        id: Hash32,
    ) -> Result<Hash32> {
        let record = state
            .objects
            .get(&id)
            .ok_or_else(|| Error::not_found("no such object at this snapshot"))?;
        let meta = self.tensor_metadata_record(record)?;
        let bytes = self.read_object_at(Some(snapshot), id)?;
        let root = if meta.dtype == TensorElementDtype::Q8_0 {
            let shard = self.q8_0_shard_meta_record(record)?;
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
                "object {id} tensor logical root does not match its recorded identity at the \
                 pinned snapshot"
            )));
        }
        Ok(root)
    }

    fn q8_0_shard_meta_record(
        &self,
        record: &ObjectRecord,
    ) -> Result<weftcodec::TensorShardedMeta> {
        let manifest =
            self.with_backend(|backend| recovery::read_manifest_via(backend, record.content))?;
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
        let frame_bytes = self.with_backend(|backend| {
            recovery::read_blob_verified_via(
                backend,
                BlobKind::Manifest,
                frame_id,
                identity::MANIFEST_DOMAIN,
            )
        })?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        Ok(weftcodec::decode_shard_frame(&frame, &limits)?.meta)
    }

    /// Tensor identity recorded for an object.
    pub fn tensor_metadata(&self, id: Hash32) -> Result<StoredTensorMetadata> {
        let record = self.object_meta(id)?;
        self.tensor_metadata_record(&record)
    }

    /// Tensor identity for an object record taken from an explicit state.
    ///
    /// The record determines every identity field; no live state is consulted.
    /// This is the pinned-snapshot form used by training-ready verification.
    pub fn tensor_metadata_at(&self, state: &State, id: Hash32) -> Result<StoredTensorMetadata> {
        let record = state
            .objects
            .get(&id)
            .ok_or_else(|| Error::not_found("no such object at this snapshot"))?;
        self.tensor_metadata_record(record)
    }

    fn tensor_metadata_record(&self, record: &ObjectRecord) -> Result<StoredTensorMetadata> {
        if record.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "tensor metadata requires a file object",
            ));
        }
        let manifest =
            self.with_backend(|backend| recovery::read_manifest_via(backend, record.content))?;
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
                    tile_shape: tile_shape_attr(record)?,
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
            tile_shape: tile_shape_attr(record)?,
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
            None,
            false,
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
            artifact.tile_shape.as_deref(),
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
        tile_shape: Option<&[u64]>,
        independent_only: bool,
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
                    estimated_decode_ns: Some(
                        options.estimate_ns(crate::cost_model::CostClass::Raw, size),
                    ),
                    cost: Some(price(
                        size,
                        options.estimate_ns(crate::cost_model::CostClass::Raw, size),
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
                    tile_shape: None,
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
                    let estimate = options.estimate_ns(crate::cost_model::CostClass::Zstd, size);
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
                            tile_shape: None,
                        }),
                    });
                }
                Err(err) => out.push(skipped(TensorStrategy::Zstd, err.to_string())),
            }
        }

        // Mode 9 float-tensor. Offered for the byte-aligned float dtypes the
        // mode implements. fp32 is offered but loses to zstd on measured
        // corpora (P2: +62 % physical), so the planner only selects it when it
        // actually wins; an explicit `float-tensor` hint still forces it.
        //
        // Mode 9 decodes about 19x slower than zstd, so it is only *priced*
        // when it saves at least 1/64 (1.56 %) of the zstd frame. On
        // incompressible data the two are within noise of each other; without
        // this margin an accidental 0.06 % win would select the slow decoder.
        let zstd_physical = out
            .iter()
            .find(|item| {
                item.candidate.strategy == TensorStrategy::Zstd && item.candidate.applicable
            })
            .and_then(|item| item.candidate.physical_bytes);
        match float_tensor_dtype(dtype) {
            Some(float_dtype) => {
                match weftcodec::modes::encode_float_tensor(logical, float_dtype, shape, &limits) {
                    Ok(frame) => {
                        let physical = frame.len() as u64;
                        let worthwhile = match zstd_physical {
                            Some(zstd) => physical.saturating_mul(64) <= zstd.saturating_mul(63),
                            None => true,
                        };
                        if !worthwhile {
                            out.push(skipped(
                                TensorStrategy::FloatTensor,
                                format!(
                                    "mode 9 saves less than 1/64 of the zstd frame                                      ({physical} vs {} bytes); not worth the decode cost",
                                    zstd_physical.unwrap_or(0)
                                ),
                            ));
                        } else {
                            let estimate = options
                                .estimate_ns(crate::cost_model::CostClass::FloatTensor, size);
                            let root = self.logical_root_for_bytes(dtype, shape, logical, None)?;
                            out.push(PreparedCandidate {
                                candidate: TensorPlanCandidate {
                                    strategy: TensorStrategy::FloatTensor,
                                    applicable: true,
                                    detail: format!(
                                        "one mode-9 FLOAT_TENSOR frame ({})",
                                        float_dtype.as_str()
                                    ),
                                    physical_bytes: Some(physical),
                                    estimated_decode_ns: Some(estimate),
                                    cost: Some(price(physical, estimate, options.lambda_ns)),
                                    dependency: None,
                                },
                                artifact: Some(PreparedArtifact {
                                    kind: RepresentationKind::Tensor,
                                    strategy_name: "float-tensor".to_owned(),
                                    frame: Some(frame),
                                    dependencies: Vec::new(),
                                    tensor: Some(TensorRepresentation {
                                        tensor_mode: 9,
                                        dtype,
                                        shape: shape.to_vec(),
                                        logical_root: root,
                                        layout: layout.to_value(dtype, shape)?,
                                    }),
                                    tile_shape: None,
                                }),
                            });
                        }
                    }
                    Err(err) => out.push(skipped(TensorStrategy::FloatTensor, err.to_string())),
                }
            }
            None => out.push(skipped(
                TensorStrategy::FloatTensor,
                format!("dtype {} has no mode-9 float layout", dtype.as_str()),
            )),
        }

        // Mode 10 tensor-sharded. Restricted to q8_0: the additive quantized
        // logical root needs the codec's scale plane, which only mode 10
        // carries, and the store writes one whole-tensor shard.
        if dtype == TensorElementDtype::Q8_0 {
            match self.prepare_mode10(dtype, shape, logical, &limits) {
                Ok((frame, root)) => {
                    let physical = frame.len() as u64;
                    let estimate =
                        options.estimate_ns(crate::cost_model::CostClass::TensorSharded, size);
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
                            tile_shape: None,
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

        // Mode 15 tensor-tiled. The tiled representation is never planned
        // implicitly: a caller must name the tile shape or opt into
        // `auto_tile_bytes`, so existing objects stay on mode 10 / zstd / raw.
        let tiled = match tile_shape {
            Some(tile_shape) => Some((tile_shape.to_vec(), "explicit".to_owned())),
            None => match options.auto_tile_bytes {
                Some(budget) => match derive_auto_tile_shape(shape, dtype, budget) {
                    Ok(tile_shape) => {
                        Some((tile_shape, format!("auto from a {budget}-byte budget")))
                    }
                    Err(err) => {
                        out.push(skipped(TensorStrategy::TensorTiled, err.to_string()));
                        None
                    }
                },
                None => None,
            },
        };
        match tiled.clone() {
            Some((tile_shape, origin)) => {
                match self.prepare_mode15(dtype, shape, logical, &tile_shape, &limits) {
                    Ok((frame, root)) => {
                        let physical = frame.len() as u64;
                        let estimate =
                            options.estimate_ns(crate::cost_model::CostClass::TensorTiled, size);
                        out.push(PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::TensorTiled,
                                applicable: true,
                                detail: format!(
                                    "one mode-15 frame tiled {tile_shape:?} ({origin})"
                                ),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                cost: Some(price(physical, estimate, options.lambda_ns)),
                                dependency: None,
                            },
                            artifact: Some(PreparedArtifact {
                                kind: RepresentationKind::Tensor,
                                strategy_name: "tensor-tiled".to_owned(),
                                frame: Some(frame),
                                dependencies: Vec::new(),
                                tensor: Some(TensorRepresentation {
                                    tensor_mode: 15,
                                    dtype,
                                    shape: shape.to_vec(),
                                    logical_root: root,
                                    layout: layout.to_value(dtype, shape)?,
                                }),
                                tile_shape: Some(tile_shape),
                            }),
                        });
                    }
                    Err(err) => out.push(skipped(TensorStrategy::TensorTiled, err.to_string())),
                }
            }
            None => {
                if options.auto_tile_bytes.is_none() {
                    out.push(skipped(
                        TensorStrategy::TensorTiled,
                        "no tile shape supplied".to_owned(),
                    ));
                }
            }
        }

        // Mode 17 float-tiled. The same opt-in as mode 15 (an explicit tile
        // shape or `auto_tile_bytes`); it is the independently decodable form
        // of the mode-9 plane split, so it is offered only for the dtypes that
        // have both a mode-9 layout and a logical-tensor identity.
        match tiled {
            Some((tile_shape, origin)) => match float_tensor_dtype(dtype) {
                Some(float_dtype)
                    if matches!(
                        float_dtype,
                        weftcodec::modes::FloatDtype::Bf16
                            | weftcodec::modes::FloatDtype::F16
                            | weftcodec::modes::FloatDtype::F32
                    ) =>
                {
                    match self.prepare_mode17(dtype, shape, logical, &tile_shape, &limits) {
                        Ok((frame, root)) => {
                            let physical = frame.len() as u64;
                            let estimate = options
                                .estimate_ns(crate::cost_model::CostClass::TensorTiled, size);
                            out.push(PreparedCandidate {
                                candidate: TensorPlanCandidate {
                                    strategy: TensorStrategy::FloatTiled,
                                    applicable: true,
                                    detail: format!(
                                        "one mode-17 float-tiled frame tiled {tile_shape:?} ({origin})"
                                    ),
                                    physical_bytes: Some(physical),
                                    estimated_decode_ns: Some(estimate),
                                    cost: Some(price(physical, estimate, options.lambda_ns)),
                                    dependency: None,
                                },
                                artifact: Some(PreparedArtifact {
                                    kind: RepresentationKind::Tensor,
                                    strategy_name: "float-tiled".to_owned(),
                                    frame: Some(frame),
                                    dependencies: Vec::new(),
                                    tensor: Some(TensorRepresentation {
                                        tensor_mode: 17,
                                        dtype,
                                        shape: shape.to_vec(),
                                        logical_root: root,
                                        layout: layout.to_value(dtype, shape)?,
                                    }),
                                    tile_shape: Some(tile_shape),
                                }),
                            });
                        }
                        Err(err) => out.push(skipped(TensorStrategy::FloatTiled, err.to_string())),
                    }
                }
                Some(float_dtype) => out.push(skipped(
                    TensorStrategy::FloatTiled,
                    format!(
                        "{} has no logical-tensor identity layout for mode 17",
                        float_dtype.as_str()
                    ),
                )),
                None => out.push(skipped(
                    TensorStrategy::FloatTiled,
                    format!("dtype {} has no mode-9 float layout", dtype.as_str()),
                )),
            },
            None => {
                if options.auto_tile_bytes.is_none() {
                    out.push(skipped(
                        TensorStrategy::FloatTiled,
                        "no tile shape supplied".to_owned(),
                    ));
                }
            }
        }

        // Mode 11 weight-delta against the causal parent.
        if independent_only {
            out.push(skipped(
                TensorStrategy::WeightDelta,
                "rebase requires an independent representation".to_owned(),
            ));
        } else {
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
                    let estimate =
                        options.estimate_ns(crate::cost_model::CostClass::WeightDelta, size);
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
                            tile_shape: None,
                        }),
                    });
                }
                Ok(None) => out.push(skipped(
                    TensorStrategy::WeightDelta,
                    "no usable causal parent with the same dtype/shape".to_owned(),
                )),
                Err(err) => out.push(skipped(TensorStrategy::WeightDelta, err.to_string())),
            }
        }

        // Mode 16 sparse-delta against the causal parent. The same exact delta
        // as mode 11, priced like it; the planner keeps mode 11 whenever its
        // Huffman planes are smaller (dense deltas) and mode 16 when the
        // zero-run stream is smaller (sparse deltas).
        if independent_only {
            out.push(skipped(
                TensorStrategy::SparseDelta,
                "rebase requires an independent representation".to_owned(),
            ));
        } else {
            match self.prepare_delta(
                dtype,
                shape,
                logical,
                causal,
                &limits,
                TensorStrategy::SparseDelta,
            ) {
                Ok(Some((frame, dependency))) => {
                    let physical = frame.len() as u64;
                    let estimate =
                        options.estimate_ns(crate::cost_model::CostClass::SparseDelta, size);
                    out.push(PreparedCandidate {
                        candidate: TensorPlanCandidate {
                            strategy: TensorStrategy::SparseDelta,
                            applicable: true,
                            detail: "mode 16 against the causal parent".to_owned(),
                            physical_bytes: Some(physical),
                            estimated_decode_ns: Some(estimate),
                            cost: Some(price(physical, estimate, options.lambda_ns)),
                            dependency: Some(dependency),
                        },
                        artifact: Some(PreparedArtifact {
                            kind: RepresentationKind::Tensor,
                            strategy_name: "sparse-delta".to_owned(),
                            frame: Some(frame),
                            dependencies: vec![dependency],
                            tensor: Some(TensorRepresentation {
                                tensor_mode: 16,
                                dtype,
                                shape: shape.to_vec(),
                                logical_root: self
                                    .logical_root_for_bytes(dtype, shape, logical, None)?,
                                layout: layout.to_value(dtype, shape)?,
                            }),
                            tile_shape: None,
                        }),
                    });
                }
                Ok(None) => out.push(skipped(
                    TensorStrategy::SparseDelta,
                    "no usable causal parent with the same dtype/shape".to_owned(),
                )),
                Err(err) => out.push(skipped(TensorStrategy::SparseDelta, err.to_string())),
            }
        }

        // Mode 14 semantic-delta against a same-semantic-root candidate.
        if independent_only {
            out.push(skipped(
                TensorStrategy::SemanticDelta,
                "rebase requires an independent representation".to_owned(),
            ));
        } else {
            match self.prepare_semantic(dtype, shape, logical, options, &limits) {
                Ok(Some((frame, dependency, policy))) => {
                    let physical = frame.len() as u64;
                    let estimate =
                        options.estimate_ns(crate::cost_model::CostClass::SemanticDelta, size);
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
                            tile_shape: None,
                        }),
                    });
                }
                Ok(None) => out.push(skipped(
                    TensorStrategy::SemanticDelta,
                    "no same-semantic-root candidate with the same dtype/shape".to_owned(),
                )),
                Err(err) => out.push(skipped(TensorStrategy::SemanticDelta, err.to_string())),
            }
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

    fn prepare_mode15(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        tile_shape: &[u64],
        limits: &Limits,
    ) -> Result<(Vec<u8>, Hash32)> {
        let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
            Error::invalid_argument(format!(
                "dtype `{}` has no element layout for mode 15",
                dtype.as_str()
            ))
        })?;
        let frame = weftcodec::encode_tiled_tensor(logical, physical, shape, tile_shape, limits)?;
        let parsed = Frame::parse(&frame, limits)?;
        let meta = weftcodec::tiled_tensor_metadata(&parsed, limits)?;
        if meta.shape != shape {
            return Err(Error::corrupt(
                "mode-15 shape disagrees with the write shape",
            ));
        }
        Ok((frame, meta.logical_root))
    }

    fn prepare_mode17(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        tile_shape: &[u64],
        limits: &Limits,
    ) -> Result<(Vec<u8>, Hash32)> {
        let float_dtype = float_tensor_dtype(dtype).ok_or_else(|| {
            Error::invalid_argument(format!(
                "dtype `{}` has no float layout for mode 17",
                dtype.as_str()
            ))
        })?;
        let frame = weftcodec::encode_float_tiled(logical, float_dtype, shape, tile_shape, limits)?;
        let parsed = Frame::parse(&frame, limits)?;
        let meta = weftcodec::float_tiled_metadata(&parsed, limits)?;
        if meta.shape != shape {
            return Err(Error::corrupt(
                "mode-17 shape disagrees with the write shape",
            ));
        }
        Ok((frame, meta.logical_root))
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
        if !matches!(
            strategy,
            TensorStrategy::WeightDelta | TensorStrategy::SparseDelta
        ) {
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
        let frame = if strategy == TensorStrategy::SparseDelta {
            weftcodec::encode_sparse_delta(logical, weight, shape, &base_frame, limits)?
        } else {
            weftcodec::encode_weight_delta(logical, weight, shape, &base_frame, limits)?
        };
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
        let manifest =
            self.with_backend(|backend| recovery::read_manifest_via(backend, manifest_id))?;
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
                    overlay: None,
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
                        overlay: None,
                    }),
                    identity: crate::format::ContentIdentity::WholeFileV1,
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
                        overlay: None,
                    }),
                    identity: crate::format::ContentIdentity::WholeFileV1,
                }
            }
            RepresentationKind::Overlay => {
                return Err(Error::invalid_argument(
                    "overlay representations are not produced by the tensor writer",
                ))
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
        gate: Option<&AuthorizedRead>,
        cancel: Option<&CancelToken>,
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
        let frame_bytes = self.with_backend(|backend| {
            recovery::read_blob_verified_via(
                backend,
                BlobKind::Manifest,
                frame_id,
                identity::MANIFEST_DOMAIN,
            )
        })?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        let expected_mode = match tensor.tensor_mode {
            9 => Mode::FloatTensor,
            10 => Mode::TensorSharded,
            11 => Mode::WeightDelta,
            12 => Mode::KvCache,
            13 => Mode::ActivationStream,
            14 => Mode::SemanticDelta,
            15 => Mode::TensorTiled,
            16 => Mode::SparseDelta,
            17 => Mode::FloatTiled,
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
            9 => self.decode_mode9(&frame, tensor, manifest, &limits)?,
            10 => self.decode_mode10(&frame, tensor, manifest, &limits)?,
            11 => self.decode_delta(
                &frame,
                tensor,
                rep,
                &limits,
                depth,
                seen,
                DeltaKind::Weight,
                gate,
                cancel,
            )?,
            14 => self.decode_delta(
                &frame,
                tensor,
                rep,
                &limits,
                depth,
                seen,
                DeltaKind::Semantic,
                gate,
                cancel,
            )?,
            16 => self.decode_delta(
                &frame,
                tensor,
                rep,
                &limits,
                depth,
                seen,
                DeltaKind::Sparse,
                gate,
                cancel,
            )?,
            12 => self.decode_kv_page(&frame, tensor, &limits)?,
            13 => self.decode_activation(&frame, tensor, &limits)?,
            15 => self.decode_mode15(&frame, tensor, manifest, rep, &limits)?,
            17 => self.decode_mode17(&frame, tensor, manifest, rep, &limits)?,
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

    fn decode_mode9(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        limits: &Limits,
    ) -> Result<Vec<u8>> {
        let meta = weftcodec::modes::float_tensor_metadata(frame, limits)?;
        let expected = float_tensor_dtype(tensor.dtype).ok_or_else(|| {
            Error::corrupt(format!(
                "mode-9 dtype `{}` has no float layout",
                tensor.dtype.as_str()
            ))
        })?;
        if meta.dtype != expected {
            return Err(Error::corrupt(format!(
                "mode-9 dtype `{}` disagrees with manifest dtype `{}`",
                meta.dtype.as_str(),
                tensor.dtype.as_str()
            )));
        }
        if meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-9 shape disagrees with the representation manifest",
            ));
        }
        if meta.element_count * expected.element_size() != manifest.size {
            return Err(Error::corrupt(
                "mode-9 element count disagrees with the manifest size",
            ));
        }
        let logical = weftcodec::modes::decode_float_tensor(frame, limits)?;
        if logical.len() as u64 != manifest.size {
            return Err(Error::corrupt(
                "mode-9 decoded length disagrees with the manifest",
            ));
        }
        let physical = tensor
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("mode-9 dtype has no logical root"))?;
        let root = weftcodec::tensor::logical_root(physical, &tensor.shape, &logical, limits)?;
        if root != tensor.logical_root {
            return Err(Error::corrupt("mode-9 logical root recomputation mismatch"));
        }
        Ok(logical)
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

    /// Sequential form used by the generic `kind:"tensor"` read dispatch.
    fn decode_mode15(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        rep: &Representation,
        _limits: &Limits,
    ) -> Result<Vec<u8>> {
        self.decode_mode15_with_options(frame, tensor, manifest, rep, &TileDecodeOptions::default())
            .map(|(bytes, _stats)| bytes)
    }

    /// Sequential form used by the generic `kind:"tensor"` read dispatch.
    fn decode_mode17(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        rep: &Representation,
        _limits: &Limits,
    ) -> Result<Vec<u8>> {
        self.decode_mode17_with_options(frame, tensor, manifest, rep, &TileDecodeOptions::default())
            .map(|(bytes, _stats)| bytes)
    }

    /// Decode a mode-15 frame, optionally on a bounded tile worker pool.
    ///
    /// Every tile root is verified, the assembled logical root is recomputed,
    /// and the declared lengths are checked. Order is deterministic because the
    /// tiles are placed in descriptor order regardless of completion order.
    fn decode_mode15_with_options(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        rep: &Representation,
        options: &TileDecodeOptions,
    ) -> Result<(Vec<u8>, TileDecodeStats)> {
        let limits = Limits::default();
        let descriptor = weftcodec::parse_tiled_tensor(frame, &limits)?;
        let meta = &descriptor.meta;
        let physical = tensor
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("mode-15 dtype has no element layout"))?;
        if meta.dtype != physical {
            return Err(Error::corrupt(format!(
                "mode-15 dtype `{}` disagrees with manifest dtype `{}`",
                meta.dtype.as_str(),
                tensor.dtype.as_str()
            )));
        }
        if meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-15 shape disagrees with the representation manifest",
            ));
        }
        if meta.logical_root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-15 logical_root disagrees with the manifest logical_root",
            ));
        }
        let total_u64 = meta
            .element_count
            .checked_mul(meta.dtype.element_size())
            .ok_or_else(|| Error::limit_exceeded("tiled tensor decoded bytes overflow u64"))?;
        if total_u64 != manifest.size || total_u64 != rep.decoded_size {
            return Err(Error::corrupt(
                "mode-15 decoded length disagrees with the manifest",
            ));
        }
        let total = usize::try_from(total_u64)
            .map_err(|_| Error::limit_exceeded("tiled tensor decoded bytes do not fit usize"))?;
        let mut out = vec![0u8; total];
        let sizes: Vec<Result<usize>> = meta
            .tiles
            .iter()
            .map(|entry| {
                usize::try_from(entry.element_len)
                    .map_err(|_| Error::limit_exceeded("tensor tile length does not fit usize"))
            })
            .collect();
        let (tiles, run) = run_bounded(
            &meta.tiles,
            &sizes,
            options.threads,
            options.max_inflight_bytes,
            |_position, entry| {
                let start = usize::try_from(entry.offset)
                    .map_err(|_| Error::limit_exceeded("tensor tile offset does not fit usize"))?;
                let end = start
                    .checked_add(usize::try_from(entry.compressed_len).map_err(|_| {
                        Error::limit_exceeded("tensor tile length does not fit usize")
                    })?)
                    .ok_or_else(|| Error::limit_exceeded("tensor tile range overflows usize"))?;
                let stored = descriptor
                    .payload
                    .get(start..end)
                    .ok_or_else(|| Error::corrupt("tensor tile range is outside the payload"))?;
                weftcodec::decode_tiled_tile(meta.dtype, entry, stored, &limits)
                    .map_err(Error::from)
            },
            |tile| tile.len(),
        )?;
        let mut tiles_bytes = 0usize;
        for (entry, tile) in meta.tiles.iter().zip(tiles.iter()) {
            tiles_bytes = tiles_bytes.checked_add(tile.len()).ok_or_else(|| {
                Error::limit_exceeded("tiled tensor decoded length overflows usize")
            })?;
            weftcodec::place_tiled_tile(&mut out, meta, entry, tile)?;
        }
        if tiles_bytes != total {
            return Err(Error::corrupt(format!(
                "tiled tensor decoded to {tiles_bytes} bytes but the descriptor requires {total}"
            )));
        }
        let logical = weftcodec::tensor::logical_root(meta.dtype, &meta.shape, &out, &limits)?;
        if logical != meta.logical_root {
            return Err(Error::corrupt(format!(
                "tiled tensor logical root {logical} does not match the descriptor's {}",
                meta.logical_root
            )));
        }
        Ok((out, tile_stats(&run)))
    }

    /// Decode a mode-17 frame, optionally on a bounded tile worker pool.
    ///
    /// This is the mode-9 plane split applied per tile: each tile is an exact
    /// stand-alone float frame, so tiles are independent units. Every tile root
    /// is verified, the assembled logical root is recomputed, and descriptor
    /// lengths are checked against the representation manifest.
    fn decode_mode17_with_options(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        rep: &Representation,
        options: &TileDecodeOptions,
    ) -> Result<(Vec<u8>, TileDecodeStats)> {
        let limits = Limits::default();
        let descriptor = weftcodec::parse_float_tiled(frame, &limits)?;
        let meta = &descriptor.meta;
        if !float_dtype_matches_tensor(meta.dtype, tensor.dtype) {
            return Err(Error::corrupt(format!(
                "mode-17 dtype `{}` disagrees with manifest dtype `{}`",
                meta.dtype.as_str(),
                tensor.dtype.as_str()
            )));
        }
        if meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-17 shape disagrees with the representation manifest",
            ));
        }
        if meta.logical_root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-17 logical_root disagrees with the manifest logical_root",
            ));
        }
        let total_u64 = meta
            .element_count
            .checked_mul(meta.dtype.element_size())
            .ok_or_else(|| Error::limit_exceeded("float tiled decoded bytes overflow u64"))?;
        if total_u64 != manifest.size || total_u64 != rep.decoded_size {
            return Err(Error::corrupt(
                "mode-17 decoded length disagrees with the manifest",
            ));
        }
        let total = usize::try_from(total_u64)
            .map_err(|_| Error::limit_exceeded("float tiled decoded bytes do not fit usize"))?;
        let mut out = vec![0u8; total];
        let sizes: Vec<Result<usize>> = meta
            .tiles
            .iter()
            .map(|entry| {
                usize::try_from(entry.element_len)
                    .map_err(|_| Error::limit_exceeded("float tile length does not fit usize"))
            })
            .collect();
        let (tiles, run) = run_bounded(
            &meta.tiles,
            &sizes,
            options.threads,
            options.max_inflight_bytes,
            |_position, entry| {
                let start = usize::try_from(entry.offset)
                    .map_err(|_| Error::limit_exceeded("float tile offset does not fit usize"))?;
                let end = start
                    .checked_add(usize::try_from(entry.compressed_len).map_err(|_| {
                        Error::limit_exceeded("float tile length does not fit usize")
                    })?)
                    .ok_or_else(|| Error::limit_exceeded("float tile range overflows usize"))?;
                let stored = descriptor
                    .payload
                    .get(start..end)
                    .ok_or_else(|| Error::corrupt("float tile range is outside the payload"))?;
                weftcodec::decode_float_tile(meta.dtype, entry, stored, &limits)
                    .map_err(Error::from)
            },
            |tile| tile.len(),
        )?;
        let mut tiles_bytes = 0usize;
        for (entry, tile) in meta.tiles.iter().zip(tiles.iter()) {
            tiles_bytes = tiles_bytes.checked_add(tile.len()).ok_or_else(|| {
                Error::limit_exceeded("float tiled decoded length overflows usize")
            })?;
            weftcodec::place_float_tile(&mut out, meta, entry, tile)?;
        }
        if tiles_bytes != total {
            return Err(Error::corrupt(format!(
                "float tiled decoded to {tiles_bytes} bytes but the descriptor requires {total}"
            )));
        }
        let tensor_dtype = tensor
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("mode-17 dtype has no element layout"))?;
        let logical = weftcodec::tensor::logical_root(tensor_dtype, &meta.shape, &out, &limits)?;
        if logical != meta.logical_root {
            return Err(Error::corrupt(format!(
                "float tiled logical root {logical} does not match the descriptor's {}",
                meta.logical_root
            )));
        }
        Ok((out, tile_stats(&run)))
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
        kind: DeltaKind,
        gate: Option<&AuthorizedRead>,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>> {
        let (descriptor_dtype, descriptor_shape, base_hash) = match kind {
            DeltaKind::Semantic => {
                let meta = weftcodec::semantic_delta_metadata(frame, limits)?;
                (
                    TensorElementDtype::from_weight_name(meta.dtype.as_str())?,
                    meta.shape,
                    meta.base,
                )
            }
            DeltaKind::Sparse => {
                let meta = weftcodec::sparse_delta_metadata(frame, limits)?;
                (
                    TensorElementDtype::from_weight_name(meta.dtype.as_str())?,
                    meta.shape,
                    meta.base,
                )
            }
            DeltaKind::Weight => {
                let meta = weftcodec::weight_delta_metadata(frame, limits)?;
                (
                    TensorElementDtype::from_weight_name(meta.dtype.as_str())?,
                    meta.shape,
                    meta.base,
                )
            }
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
        let dep_manifest =
            self.with_backend(|backend| recovery::read_manifest_via(backend, dep_root))?;
        seen.push(dep_root);
        let base_bytes = self.read_manifest_bytes_inner_gated(
            &dep_manifest,
            0,
            dep_manifest.size,
            true,
            depth + 1,
            seen,
            gate,
            cancel,
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
        let logical = match kind {
            DeltaKind::Semantic => weftcodec::decode_semantic_delta_with_base(
                frame,
                &base_frame,
                &MapResolver::new(),
                limits,
            )?,
            DeltaKind::Sparse => weftcodec::decode_sparse_delta_with_base(
                frame,
                &base_frame,
                &MapResolver::new(),
                limits,
            )?,
            DeltaKind::Weight => {
                weftcodec::decode_with_base_frame(frame, &base_frame, &MapResolver::new(), limits)?
            }
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

fn tile_shape_attr(record: &ObjectRecord) -> Result<Option<Vec<u64>>> {
    match record.attrs.get(ATTR_TENSOR_TILE_SHAPE) {
        None => Ok(None),
        Some(Value::Array(items)) => {
            let mut shape = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::U64(dim) if *dim > 0 => shape.push(*dim),
                    Value::U64(_) => {
                        return Err(Error::corrupt(
                            "tensor tile_shape attribute contains a zero extent",
                        ))
                    }
                    _ => {
                        return Err(Error::corrupt(
                            "tensor tile_shape attribute is not an array of u64",
                        ))
                    }
                }
            }
            Ok(Some(shape))
        }
        Some(_) => Err(Error::corrupt(
            "tensor tile_shape attribute is not an array",
        )),
    }
}

fn apply_tensor_attrs(
    attrs: &mut BTreeMap<String, Value>,
    dtype: TensorElementDtype,
    shape: &[u64],
    root: Hash32,
    layout: &TensorLayoutSpec,
    strategy: &str,
    tile_shape: Option<&[u64]>,
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
    match tile_shape {
        Some(tile_shape) => {
            attrs.insert(
                ATTR_TENSOR_TILE_SHAPE.to_owned(),
                Value::Array(tile_shape.iter().map(|dim| Value::U64(*dim)).collect()),
            );
        }
        None => {
            attrs.remove(ATTR_TENSOR_TILE_SHAPE);
        }
    }
    Ok(())
}

/// Which exact word-wise delta frame a `kind:"tensor"` manifest names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeltaKind {
    /// Mode 11 `WEIGHT_DELTA` (Huffman byte planes).
    Weight,
    /// Mode 14 `SEMANTIC_DELTA` (semantic-root-checked mode 11 planes).
    Semantic,
    /// Mode 16 `SPARSE_DELTA` (zero-run stream).
    Sparse,
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
