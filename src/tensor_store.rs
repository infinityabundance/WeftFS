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
//! cost = physical_bytes
//!      + lambda_ns       * estimated_decode_ns   (x reads_per_write)
//!      + lambda_write_ns * estimated_encode_ns   (one write)
//! ```
//!
//! and picks the minimum (ties keep the fixed candidate order). `lambda_ns`
//! defaults to `0`, i.e. bytes-only, and `lambda_write_ns` defaults to
//! `lambda_ns`, so a bytes-only plan is bit-for-bit the historical plan and a
//! latency-priced plan pays for its one-time encode. Estimated times are an
//! explicit integer model, not a measurement; the bench harness measures real
//! restore time. Only the chosen representation is written; the full cost
//! table is exposed by `Store::plan_tensor` / `weft plan --json` because the
//! manifest schema has no field for planner bookkeeping.

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

/// Default byte bound on the semantic-delta candidate search (512 MiB).
pub const DEFAULT_MAX_SEMANTIC_BYTES: u64 = 512 * 1024 * 1024;
/// Bound on state objects an opted-in implicit semantic scan examines.
pub const DEFAULT_SEMANTIC_SCAN_MAX_OBJECTS: usize = 256;

/// Default byte bound on the whole-tensor delta and patch candidates (512 MiB).
///
/// Those encoders hold the causal base, a synthetic base frame and the delta
/// planes at once, so a 593 MiB tensor (Qwen2.5-3B's embedding) needs several
/// GiB at the current encoder shape. The bound keeps the planner inside the
/// 3 GiB court; the skipped candidates are named in the plan.
pub const DEFAULT_MAX_DELTA_BYTES: u64 = 512 * 1024 * 1024;

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
    /// One mode-4 `DICTIONARY` frame whose raw-content dictionary is the whole
    /// causal base and whose window covers it: the `zstd --patch-from`
    /// analogue. `kind:"codec"` with one dependency.
    ZstdPatch,
    /// One mode-9 `FLOAT_TENSOR` frame (v2 `kind:"tensor"`).
    FloatTensor,
    /// One mode-10 `TENSOR_SHARDED` frame.
    TensorSharded,
    /// One mode-15 `TENSOR_TILED` frame.
    TensorTiled,
    /// One mode-17 `FLOAT_TILED` frame: per-tile mode-9 plane split.
    FloatTiled,
    /// One mode-18 `FLOAT_TILED_STREAMS` frame: per-tile interleaved
    /// Huffman streams.
    FloatTiledStreams,
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
    /// One mode-20 `ENGRAM_SHARD` frame: region-addressable row groups with an
    /// explicit supersession chain. Offered only when the caller supplies
    /// engram metadata; never auto-selected.
    EngramShard,
}

impl TensorStrategy {
    /// Encode-cost class for the write term of the objective.
    pub fn encode_class(self) -> crate::cost_model::EncodeClass {
        use crate::cost_model::EncodeClass;
        match self {
            TensorStrategy::Raw => EncodeClass::Raw,
            TensorStrategy::Zstd => EncodeClass::Zstd,
            // A patch frame is one base read plus one high-level zstd encode;
            // price it with the measured level-19 rate (conservative).
            TensorStrategy::ZstdPatch => EncodeClass::Zstd19,
            TensorStrategy::FloatTensor => EncodeClass::FloatTensor,
            TensorStrategy::TensorSharded => EncodeClass::TensorSharded,
            TensorStrategy::TensorTiled => EncodeClass::TensorTiled,
            TensorStrategy::FloatTiled => EncodeClass::FloatTiled,
            TensorStrategy::FloatTiledStreams => EncodeClass::FloatTiledStreams,
            TensorStrategy::WeightDelta => EncodeClass::WeightDelta,
            TensorStrategy::SparseDelta => EncodeClass::SparseDelta,
            TensorStrategy::SemanticDelta => EncodeClass::SemanticDelta,
            // The read-only modes are never produced by this planner; price
            // them as the closest encode class so the objective stays defined.
            TensorStrategy::KvCache | TensorStrategy::Activation => EncodeClass::TensorSharded,
            TensorStrategy::EngramShard => EncodeClass::TensorTiled,
        }
    }

    /// True for representations that are experimental and therefore never
    /// auto-selected without an explicit opt-in.
    ///
    /// Mode 18 (`FLOAT_TILED_STREAMS`) is frozen and readable, but it lost
    /// every measured axis (1.5-3.0x slower on small regions, 1.5-26.6 %
    /// slower at four threads, 0.03-2.41 % larger). It stays selectable by an
    /// explicit strategy hint and by `allow_experimental_modes`; it is never
    /// chosen by an automatic plan.
    pub fn is_experimental(self) -> bool {
        matches!(self, TensorStrategy::FloatTiledStreams)
    }

    /// Canonical `representation.strategy` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            TensorStrategy::Raw => "raw",
            TensorStrategy::Zstd => "zstd",
            TensorStrategy::ZstdPatch => "zstd-patch",
            TensorStrategy::FloatTensor => "float-tensor",
            TensorStrategy::TensorSharded => "tensor-sharded",
            TensorStrategy::TensorTiled => "tensor-tiled",
            TensorStrategy::FloatTiled => "float-tiled",
            TensorStrategy::FloatTiledStreams => "float-tiled-streams",
            TensorStrategy::WeightDelta => "weight-delta",
            TensorStrategy::SparseDelta => "sparse-delta",
            TensorStrategy::SemanticDelta => "semantic-delta",
            TensorStrategy::KvCache => "kv-cache",
            TensorStrategy::Activation => "activation",
            TensorStrategy::EngramShard => "engram-shard",
        }
    }

    /// Parse a canonical strategy spelling.
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "raw" => Ok(TensorStrategy::Raw),
            "zstd" => Ok(TensorStrategy::Zstd),
            "zstd-patch" => Ok(TensorStrategy::ZstdPatch),
            "float-tensor" => Ok(TensorStrategy::FloatTensor),
            "tensor-sharded" => Ok(TensorStrategy::TensorSharded),
            "tensor-tiled" => Ok(TensorStrategy::TensorTiled),
            "float-tiled" => Ok(TensorStrategy::FloatTiled),
            "float-tiled-streams" => Ok(TensorStrategy::FloatTiledStreams),
            "weight-delta" => Ok(TensorStrategy::WeightDelta),
            "sparse-delta" => Ok(TensorStrategy::SparseDelta),
            "semantic-delta" => Ok(TensorStrategy::SemanticDelta),
            "kv-cache" => Ok(TensorStrategy::KvCache),
            "activation" => Ok(TensorStrategy::Activation),
            "engram-shard" => Ok(TensorStrategy::EngramShard),
            other => Err(Error::malformed(format!(
                "unknown tensor strategy `{other}`"
            ))),
        }
    }

    /// Frame mode this strategy requires, when it is frame-backed.
    pub fn frame_mode(self) -> Option<u32> {
        match self {
            TensorStrategy::Raw | TensorStrategy::Zstd => None,
            TensorStrategy::ZstdPatch => Some(4),
            TensorStrategy::FloatTensor => Some(9),
            TensorStrategy::TensorSharded => Some(10),
            TensorStrategy::TensorTiled => Some(15),
            TensorStrategy::FloatTiled => Some(17),
            TensorStrategy::FloatTiledStreams => Some(18),
            TensorStrategy::WeightDelta => Some(11),
            TensorStrategy::SparseDelta => Some(16),
            TensorStrategy::KvCache => Some(12),
            TensorStrategy::Activation => Some(13),
            TensorStrategy::SemanticDelta => Some(14),
            TensorStrategy::EngramShard => Some(20),
        }
    }

    /// True for representations that carry a base content root.
    pub fn uses_dependency(self) -> bool {
        matches!(
            self,
            TensorStrategy::ZstdPatch
                | TensorStrategy::WeightDelta
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
            // A patch frame reads its base and then inflates: price the base
            // read plus a zstd-class decode.
            TensorStrategy::ZstdPatch => CostClass::WeightDelta,
            TensorStrategy::FloatTensor => CostClass::FloatTensor,
            TensorStrategy::TensorSharded => CostClass::TensorSharded,
            TensorStrategy::TensorTiled => CostClass::TensorTiled,
            // Mode 17 is priced with the mode-15 tile coefficients: it runs
            // the same per-tile geometry and a Huffman exponent decode, plus
            // the BF16 join. The calibration schema's class set is frozen, so
            // no new cost class was added.
            TensorStrategy::FloatTiled => CostClass::TensorTiled,
            // Mode 18 is priced from its own measured row (see
            // `cost_model::MODE18_MEASURED`) by `estimate_ns_at_pattern`; the
            // frozen calibration class set has no mode-18 row, so this class
            // is used only when a host calibration replaces the baked table.
            TensorStrategy::FloatTiledStreams => CostClass::TensorTiled,
            TensorStrategy::WeightDelta => CostClass::WeightDelta,
            TensorStrategy::SparseDelta => CostClass::SparseDelta,
            TensorStrategy::SemanticDelta => CostClass::SemanticDelta,
            TensorStrategy::KvCache => CostClass::KvCache,
            TensorStrategy::Activation => CostClass::Activation,
            // Mode 20 uses the same per-plane Huffman coder and group geometry
            // as mode 15; it is priced with the mode-15 row.
            TensorStrategy::EngramShard => CostClass::TensorTiled,
        }
    }
}

/// Codec used on the training critical path when the write policy is fast.
///
/// Only these two representations are ever built by a `Fast` write: both are
/// linear passes over the tensor bytes. The expensive planned candidates
/// (modes 9/10/15/16/17/18 and the delta family) are never encoded during the
/// save; a background transcode pass converts them afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FastCodec {
    /// Canonical raw chunks (no entropy coding at all).
    Raw,
    /// One ZSTD frame at [`TensorPlanOptions::fast_zstd_level`].
    Zstd,
}

impl FastCodec {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "raw" => Ok(FastCodec::Raw),
            "zstd" => Ok(FastCodec::Zstd),
            other => Err(Error::invalid_argument(format!(
                "--fast-codec must be `raw` or `zstd` (got `{other}`)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FastCodec::Raw => "raw",
            FastCodec::Zstd => "zstd",
        }
    }
}

/// Where a write sits relative to the training critical path.
///
/// `Auto` is the only policy that inspects the caller's declaration: a write
/// marked `on_critical_path`, or given a deadline, takes the fast path; every
/// other write keeps the full planned behaviour. `Fast` and `Planned` are
/// unconditional.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritePolicy {
    /// Never run an expensive candidate during the write.
    Fast,
    /// Today's behaviour: encode and price every applicable candidate.
    Planned,
    /// Fast when the caller declares a critical path or a deadline.
    Auto {
        on_critical_path: bool,
        deadline_ms: Option<u64>,
    },
}

impl WritePolicy {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "fast" => Ok(WritePolicy::Fast),
            "planned" => Ok(WritePolicy::Planned),
            "auto" => Ok(WritePolicy::Auto {
                on_critical_path: false,
                deadline_ms: None,
            }),
            other => Err(Error::invalid_argument(format!(
                "--write-policy must be `fast`, `planned`, or `auto` (got `{other}`)"
            ))),
        }
    }

    /// Resolve `Auto` against the caller's declaration.
    pub fn resolved(self) -> ResolvedWritePolicy {
        match self {
            WritePolicy::Fast => ResolvedWritePolicy::Fast,
            WritePolicy::Planned => ResolvedWritePolicy::Planned,
            WritePolicy::Auto {
                on_critical_path,
                deadline_ms,
            } => {
                if on_critical_path || deadline_ms.is_some() {
                    ResolvedWritePolicy::Fast
                } else {
                    ResolvedWritePolicy::Planned
                }
            }
        }
    }

    pub fn is_fast(self) -> bool {
        self.resolved() == ResolvedWritePolicy::Fast
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WritePolicy::Fast => "fast",
            WritePolicy::Planned => "planned",
            WritePolicy::Auto { .. } => "auto",
        }
    }
}

/// The policy after `Auto` has been resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedWritePolicy {
    Fast,
    Planned,
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

    /// Canonicalise a caller payload, borrowing it when the layout is already
    /// row-major.
    ///
    /// [`Self::canonicalize`] always returns an owned copy. A row-major caller
    /// already holds the canonical bytes, so copying them doubles the live
    /// tensor footprint for no benefit; this returns the caller's slice and
    /// performs the same shape/payload validation. Only non-row-major layouts
    /// allocate. The produced bytes are identical to [`Self::canonicalize`].
    pub fn canonicalize_cow<'a>(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        bytes: &'a [u8],
    ) -> Result<std::borrow::Cow<'a, [u8]>> {
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
                Ok(std::borrow::Cow::Borrowed(bytes))
            }
            _ => Ok(std::borrow::Cow::Owned(
                self.canonicalize(dtype, shape, bytes)?,
            )),
        }
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

/// The access pattern a plan is priced for.
///
/// `Whole` is the default and reproduces the historical bytes-only plan exactly
/// (the decode term is the full-tensor estimate, and `lambda_ns` still defaults
/// to 0). `Region` and `Mixed` price repeated region reads: an independently
/// tiled representation pays only for the fetched region per read, while a
/// single-frame representation pays a whole-tensor decode per read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccessPattern {
    /// The whole tensor is read once (or the caller did not say).
    #[default]
    Whole,
    /// `reads_per_write` reads of `region_bytes` each.
    Region {
        region_bytes: u64,
        reads_per_write: u64,
    },
    /// Both whole-tensor reads and region reads.
    Mixed {
        whole_reads: u64,
        region_bytes: u64,
        region_reads: u64,
    },
}

impl AccessPattern {
    /// Parse `whole`, `region:BYTES:READS`, or `mixed:WHOLE:REGION_BYTES:REGION_READS`.
    pub fn parse(text: &str) -> Result<Self> {
        let parts: Vec<&str> = text.split(':').collect();
        match parts.as_slice() {
            ["whole"] => Ok(AccessPattern::Whole),
            ["region", bytes, reads] => Ok(AccessPattern::Region {
                region_bytes: bytes.parse().map_err(|_| {
                    Error::invalid_argument(format!("--access-pattern region bytes `{bytes}` is not u64"))
                })?,
                reads_per_write: reads.parse().map_err(|_| {
                    Error::invalid_argument(format!(
                        "--access-pattern reads_per_write `{reads}` is not u64"
                    ))
                })?,
            }),
            ["mixed", whole, bytes, reads] => Ok(AccessPattern::Mixed {
                whole_reads: whole.parse().map_err(|_| {
                    Error::invalid_argument(format!("--access-pattern whole_reads `{whole}` is not u64"))
                })?,
                region_bytes: bytes.parse().map_err(|_| {
                    Error::invalid_argument(format!("--access-pattern region bytes `{bytes}` is not u64"))
                })?,
                region_reads: reads.parse().map_err(|_| {
                    Error::invalid_argument(format!(
                        "--access-pattern region_reads `{reads}` is not u64"
                    ))
                })?,
            }),
            _ => Err(Error::invalid_argument(format!(
                "--access-pattern must be whole, region:BYTES:READS, or mixed:WHOLE:BYTES:READS (got `{text}`)"
            ))),
        }
    }

    /// Canonical spelling for JSON output.
    pub fn as_str(self) -> String {
        match self {
            AccessPattern::Whole => "whole".to_owned(),
            AccessPattern::Region {
                region_bytes,
                reads_per_write,
            } => format!("region:{region_bytes}:{reads_per_write}"),
            AccessPattern::Mixed {
                whole_reads,
                region_bytes,
                region_reads,
            } => format!("mixed:{whole_reads}:{region_bytes}:{region_reads}"),
        }
    }
}

/// Tile geometry used to count how many whole tiles a read actually touches.
///
/// A tile-independent strategy decodes **whole tiles**, not the requested
/// region bytes: a 1-row read of a `[64, 896]`-tiled `[151936, 896]` tensor
/// decodes a 114,688-byte tile, and a 1-row read of a `[128, 224]` tile grid
/// touches four tiles. Pricing the logical region bytes (1,792 B) undercounts
/// both the decoded bytes and the per-tile fixed cost by orders of magnitude.
///
/// The planner only knows a region *size*, not an offset. It therefore prices
/// the leading row-major prefix of the tensor: `region_bytes` is interpreted as
/// the first `ceil(region_bytes / row_bytes)` rows (a partial row counts as one
/// row), which is exact for the row-slice access patterns the court measures
/// and conservative for every other shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileGeometry {
    shape: Vec<u64>,
    tile_shape: Vec<u64>,
    element_bytes: u64,
}

impl TileGeometry {
    /// Build geometry for one candidate tile shape. `None` when the tile shape
    /// does not have the tensor's rank or has a zero extent.
    pub fn new(
        shape: &[u64],
        tile_shape: &[u64],
        dtype: TensorElementDtype,
    ) -> Option<TileGeometry> {
        if shape.len() != tile_shape.len() || shape.is_empty() {
            return None;
        }
        if shape.contains(&0) || tile_shape.contains(&0) {
            return None;
        }
        Some(TileGeometry {
            shape: shape.to_vec(),
            tile_shape: tile_shape.to_vec(),
            element_bytes: dtype.element_bytes(),
        })
    }

    /// Canonical bytes in one full tile.
    pub fn tile_bytes(&self) -> u64 {
        self.tile_shape
            .iter()
            .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
            .and_then(|elements| elements.checked_mul(self.element_bytes))
            .unwrap_or(u64::MAX)
    }

    /// Total tiles in the tensor.
    pub fn total_tiles(&self) -> u64 {
        self.shape
            .iter()
            .zip(self.tile_shape.iter())
            .try_fold(1u64, |acc, (extent, tile)| {
                let count = ceil_div(*extent, *tile);
                acc.checked_mul(count)
            })
            .unwrap_or(u64::MAX)
    }

    /// How many whole tiles a read of `region_bytes` (the leading row-major
    /// prefix of the tensor) intersects.
    ///
    /// The prefix is `ceil(region_bytes / element_bytes)` elements. It covers
    /// `elements / row_elements` complete rows plus, when there is a remainder,
    /// a partial final row. The intersected tile box is therefore
    /// `[0, rows) x [0, columns_or_remainder) x full` and the count is exact
    /// for that box:
    ///
    /// ```text
    /// tiles = ceil(rows / tile[0]) * ceil(cols / tile[1]) * prod(ceil(shape[i] / tile[i]))
    /// ```
    /// A 64-byte read of a `[512, 512]` bf16 tensor tiled `[16, 32]` is 32
    /// elements: one partial row of 32 columns, i.e. exactly one tile.
    pub fn tiles_intersected(&self, region_bytes: u64) -> u64 {
        let rank = self.shape.len();
        let row_elements = self.shape[1..]
            .iter()
            .try_fold(1u64, |acc, dim| acc.checked_mul(*dim));
        let row_elements = match row_elements {
            Some(value) if value > 0 => value,
            _ => return self.total_tiles(),
        };
        let elements = ceil_div(region_bytes.max(1), self.element_bytes.max(1));
        let full_rows = elements / row_elements;
        let remainder = elements % row_elements;

        let tiles_after_axis = |from: usize| -> u64 {
            self.shape[from..]
                .iter()
                .zip(self.tile_shape[from..].iter())
                .try_fold(1u64, |acc, (extent, tile)| {
                    acc.checked_mul(ceil_div(*extent, *tile))
                })
                .unwrap_or(u64::MAX)
        };

        let tiles = if remainder == 0 {
            // Complete rows only.
            let rows = full_rows.max(1).min(self.shape[0]);
            ceil_div(rows, self.tile_shape[0]).saturating_mul(tiles_after_axis(1))
        } else {
            // Complete rows plus a partial row whose touched columns are the
            // remainder; the partial row lives in the next tile row.
            let rows = full_rows.saturating_add(1).min(self.shape[0]);
            let columns = remainder.min(self.shape[1]);
            let per_tile_row = ceil_div(columns, self.tile_shape[1]);
            let rest = if rank > 2 {
                self.shape[2..]
                    .iter()
                    .zip(self.tile_shape[2..].iter())
                    .try_fold(1u64, |acc, (extent, tile)| {
                        acc.checked_mul(ceil_div(*extent, *tile))
                    })
                    .unwrap_or(u64::MAX)
            } else {
                1
            };
            ceil_div(rows, self.tile_shape[0]).saturating_mul(per_tile_row.saturating_mul(rest))
        };

        if rank == 1 {
            return ceil_div(elements.max(1).min(self.shape[0]), self.tile_shape[0])
                .min(self.total_tiles());
        }
        tiles.min(self.total_tiles()).max(1)
    }
}

/// Ceiling division that is safe at the integer bounds.
fn ceil_div(value: u64, divisor: u64) -> u64 {
    debug_assert!(divisor > 0);
    value / divisor + u64::from(value % divisor != 0)
}

/// Planner options. The default is bytes-only (`lambda_ns = 0`) with the
/// host-calibrated decode cost table.
#[derive(Clone, Debug)]
pub struct TensorPlanOptions {
    /// Bytes charged per estimated nanosecond of decode time. `0` is
    /// bytes-only.
    pub lambda_ns: u64,
    /// Bytes charged per estimated nanosecond of **encode** time. Defaults to
    /// `lambda_ns` ([`TensorPlanOptions::effective_write_lambda`]), so a
    /// bytes-only plan (both zero) is unchanged and a latency-priced plan pays
    /// for its one-time encode. Set explicitly with `--lambda-write-ns`.
    pub lambda_write_ns: u64,
    /// Whether `lambda_write_ns` was set explicitly. When false the write
    /// lambda tracks `lambda_ns`.
    pub lambda_write_ns_set: bool,
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
    /// Access pattern the decode estimate is priced for. `Whole` is the default
    /// and reproduces the historical plan exactly.
    pub access_pattern: AccessPattern,
    /// Interleaved stream count for the mode-18 candidate. `None` uses
    /// `FLOAT_TILED_STREAMS_DEFAULT` (4). Mode 18 is offered only when a tile
    /// shape (or `auto_tile_bytes`) is supplied, like modes 15 and 17.
    pub n_streams: Option<u64>,
    /// Critical-path policy for this write. `Fast` restricts the candidate set
    /// to a linear-pass representation (raw or a fast ZSTD frame) and never
    /// runs a planned candidate encode; `Planned` is the historical behaviour.
    pub write_policy: WritePolicy,
    /// Representation a `Fast` write builds.
    pub fast_codec: FastCodec,
    /// ZSTD level used by `FastCodec::Zstd` (clamped to 1..=3).
    pub fast_zstd_level: i32,
    /// Offer the mode-14 semantic-delta candidate. The candidate search may
    /// decode several live tensors; disable it when a caller only cares about
    /// the causal weight/sparse deltas.
    pub semantic_search: bool,
    /// Search the **whole store** for implicit mode-14 candidates when
    /// `candidates` is empty.
    ///
    /// Off by default, and this matters: the scan walks every file object in
    /// the state and, for each one whose dtype and shape match, reads and
    /// decodes the whole tensor before it can price a mode-14 delta. That is
    /// `O(objects)` per tensor, i.e. quadratic in a store that gains objects
    /// per checkpoint. Measured on the R12b training fixture it was 130 ms of
    /// a 211 ms first save and 344 ms of a 472 ms later save. Callers that
    /// want the candidate should pass an explicit `candidates` list (the R7
    /// semantic-index shape); enabling this flag restores the historical
    /// implicit search, bounded by `semantic_scan_max_objects`.
    pub semantic_scan: bool,
    /// Upper bound on the state objects the implicit scan examines. The scan
    /// is deterministic (ascending object id), so a truncated scan is a
    /// well-defined subset, never a race.
    pub semantic_scan_max_objects: usize,
    /// Hard bound on `candidate_tensors * tensor_bytes` for the semantic
    /// candidate search. A larger tensor falls back to the other candidates
    /// instead of risking the court's memory limit.
    pub max_semantic_bytes: u64,
    /// Hard bound on the whole-tensor delta and patch candidates. Those
    /// encoders need several full-size copies (the causal base, a base frame
    /// and the delta planes), so a tensor above this bound falls back to the
    /// other candidates instead of risking the court's memory limit. `0`
    /// disables the bound. Default 512 MiB.
    pub max_delta_bytes: u64,
    /// Allow experimental representations to be **automatically** selected.
    /// Default false: mode 18 stays preparable only through an explicit
    /// strategy hint. Mode 18 is frozen and readable; the gate is about
    /// selection, not the wire format.
    pub allow_experimental_modes: bool,
    /// Offer a level-19 ZSTD candidate in addition to the level-3 one.
    /// Level 19 encoding is ~28.5 MiB/s measured; it is only offered by the
    /// `Planned` policy (never `Fast`/`Auto`-resolved-fast) and only when the
    /// size threshold and the CPU budget below both hold.
    pub zstd19: bool,
    pub zstd19_min_bytes: u64,
    /// Refuse the level-19 candidate when its estimated encode time exceeds
    /// this bound. Default 20 s.
    pub zstd19_cpu_budget_ns: u64,
    /// Explicit mode-20 `ENGRAM_SHARD` metadata. `Some` offers the candidate;
    /// `None` never offers it.
    pub engram: Option<EngramWriteSpec>,
    /// Offer the whole-base ZSTD patch candidate (`zstd --patch-from`
    /// analogue) when a usable causal base exists.
    pub zstd_patch: bool,
    /// ZSTD level for the patch candidate. 12 is the level the measured
    /// baseline uses: level 19 with a ~988 MB base was OOM-killed at the 3 GiB
    /// court limit.
    pub zstd_patch_level: i32,
    pub zstd_patch_min_bytes: u64,
    pub zstd_patch_cpu_budget_ns: u64,
}

impl Default for TensorPlanOptions {
    fn default() -> Self {
        TensorPlanOptions {
            lambda_ns: 0,
            lambda_write_ns: 0,
            lambda_write_ns_set: false,
            cost_model: crate::cost_model::CostModelKind::default(),
            calibration: None,
            semantic_policy: None,
            max_candidates: DEFAULT_TENSOR_MAX_CANDIDATES,
            candidates: Vec::new(),
            tile_shape: None,
            auto_tile_bytes: None,
            access_pattern: AccessPattern::Whole,
            n_streams: None,
            write_policy: WritePolicy::Planned,
            // Measured on the 0.5B court: one level-1 ZSTD frame is the
            // cheapest critical-path codec (raw chunking is ~7x slower because
            // it publishes 15k chunk blobs).
            fast_codec: FastCodec::Zstd,
            fast_zstd_level: 1,
            semantic_search: true,
            semantic_scan: false,
            semantic_scan_max_objects: DEFAULT_SEMANTIC_SCAN_MAX_OBJECTS,
            max_semantic_bytes: DEFAULT_MAX_SEMANTIC_BYTES,
            max_delta_bytes: DEFAULT_MAX_DELTA_BYTES,
            allow_experimental_modes: false,
            zstd19: true,
            zstd19_min_bytes: 1024 * 1024,
            zstd19_cpu_budget_ns: 20_000_000_000,
            engram: None,
            zstd_patch: true,
            zstd_patch_level: 12,
            zstd_patch_min_bytes: 1024 * 1024,
            zstd_patch_cpu_budget_ns: 20_000_000_000,
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

    /// The write lambda actually applied: the explicit value when set,
    /// otherwise the decode lambda (so `--lambda-ns 1` prices both).
    pub fn effective_write_lambda(&self) -> u64 {
        if self.lambda_write_ns_set {
            self.lambda_write_ns
        } else {
            self.lambda_ns
        }
    }

    /// Estimated encode nanoseconds for one whole-tensor encode.
    pub fn estimate_encode_ns(&self, strategy: TensorStrategy, size: u64) -> u64 {
        crate::cost_model::estimate_encode_ns(strategy.encode_class(), size)
    }

    /// Price a candidate: storage once, decode per expected read, encode once.
    pub fn price_candidate(&self, physical_bytes: u64, decode_ns: u64, encode_ns: u64) -> u128 {
        u128::from(physical_bytes)
            + u128::from(self.lambda_ns) * u128::from(decode_ns)
            + u128::from(self.effective_write_lambda()) * u128::from(encode_ns)
    }

    /// Decode cost at the configured access pattern.
    ///
    /// A tile-independent strategy (mode 15/17) pays only for the fetched
    /// region bytes per region read; every other strategy decodes the whole
    /// tensor per read. `Whole` is exactly [`Self::estimate_ns`]. Prefer
    /// [`Self::estimate_ns_at_pattern_for`] for tile candidates so the estimate
    /// counts whole tiles rather than logical region bytes.
    pub fn estimate_ns_at_pattern(
        &self,
        class: crate::cost_model::CostClass,
        strategy: TensorStrategy,
        size: u64,
    ) -> u64 {
        self.pattern_cost(class, strategy, size, None)
    }

    /// Decode cost at the configured pattern, counting the tiles a region read
    /// actually intersects. `geometry` is required for a tile-independent
    /// strategy to be priced per tile; without it the historical logical-bytes
    /// estimate is used (which undercounts tile decodes).
    pub fn estimate_ns_at_pattern_for(
        &self,
        class: crate::cost_model::CostClass,
        strategy: TensorStrategy,
        size: u64,
        geometry: Option<&TileGeometry>,
    ) -> u64 {
        self.pattern_cost(class, strategy, size, geometry)
    }

    fn pattern_cost(
        &self,
        class: crate::cost_model::CostClass,
        strategy: TensorStrategy,
        size: u64,
        geometry: Option<&TileGeometry>,
    ) -> u64 {
        let tile_independent = matches!(
            strategy,
            TensorStrategy::TensorTiled
                | TensorStrategy::FloatTiled
                | TensorStrategy::FloatTiledStreams
        );
        let full = if strategy == TensorStrategy::FloatTiledStreams && self.calibration.is_none() {
            crate::cost_model::estimate_streams_ns(size)
        } else {
            self.estimate_ns(class, size)
        };
        // One tile decode. Per-tile fixed cost is charged per tile, which is
        // what makes a many-tile read more expensive than a tiny logical
        // region read on a single-frame strategy.
        let per_tile_cost = |tile_bytes: u64| {
            if strategy == TensorStrategy::FloatTiledStreams && self.calibration.is_none() {
                crate::cost_model::estimate_streams_ns(tile_bytes)
            } else {
                self.estimate_ns(class, tile_bytes)
            }
        };
        let region_cost = |region_bytes: u64| {
            if !tile_independent {
                return full;
            }
            match geometry {
                Some(geometry) => {
                    let tiles = geometry.tiles_intersected(region_bytes);
                    per_tile_cost(geometry.tile_bytes()).saturating_mul(tiles)
                }
                // No geometry: the historical (undercounting) estimate.
                None => per_tile_cost(region_bytes.min(size)),
            }
        };
        match self.access_pattern {
            AccessPattern::Whole => full,
            AccessPattern::Region {
                region_bytes,
                reads_per_write,
            } => region_cost(region_bytes).saturating_mul(reads_per_write.max(1)),
            AccessPattern::Mixed {
                whole_reads,
                region_bytes,
                region_reads,
            } => full
                .saturating_mul(whole_reads)
                .saturating_add(region_cost(region_bytes).saturating_mul(region_reads)),
        }
    }
}

/// One changed row group in an incremental engram update.
#[derive(Clone, Copy, Debug)]
pub struct EngramGroupUpdate<'a> {
    /// Group index (canonical order).
    pub group: u32,
    /// New canonical row-major bytes for exactly that group's rows.
    pub bytes: &'a [u8],
}

/// Explicit engram metadata. Its presence is the opt-in for the mode-20
/// `EngramShard` candidate; without it the planner never offers mode 20.
#[derive(Clone, Debug, Default)]
pub struct EngramWriteSpec {
    /// Strictly increasing row edges: first 0, last = rows.
    pub row_group_edges: Vec<u64>,
    /// `ngram-sha256-v1` or `ngram-blake3-v1`.
    pub hash_function: String,
    /// Vocabulary size (1..=rows). Defaults to `rows` when zero.
    pub vocab_size: u64,
    /// Exact storage dtype; defaults to the tensor dtype.
    pub precision: Option<String>,
    /// Optional lossless quantization scheme; only `none` is implemented.
    pub quantization: Option<String>,
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

/// One tensor object whose immutable blobs are already durable but which no
/// commit has referenced yet.
///
/// This is the *prepare* half of the prepare/commit split used by
/// `Store::write_checkpoint_parallel`: a worker (its own handle, its own
/// backend, read-only control state) writes the content-addressed blobs and
/// returns this fragment; exactly one committer validates the fragments and
/// publishes every state op in a single commit. Blob publication is
/// create-once, so it needs no ordering and no coordination: two workers can
/// never disagree about the bytes behind a hash, and a worker that dies leaves
/// only unreferenced blobs that GC reclaims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedTensorObject {
    /// ObjectId seed chosen by the preparer (deterministic when the caller
    /// supplied a checkpoint seed).
    pub seed: [u8; 32],
    pub manifest_id: Hash32,
    /// `H("weft/chunk/v1", logical_bytes)`.
    pub content_root: Hash32,
    pub size: u64,
    /// `LogicalTensorRoot` of the stored (canonicalised) tensor bytes.
    pub logical_root: Hash32,
    /// Reserved `weft.tensor.*` attributes for the object record.
    pub attrs: BTreeMap<String, Value>,
    /// Stored manifest dtype spelling (for diagnostics and re-validation).
    pub dtype: String,
    /// Stored shape (for diagnostics and re-validation).
    pub shape: Vec<u64>,
    /// Chosen representation strategy name.
    pub strategy: String,
    /// Causal parent version ids recorded with the object record (checkpoint
    /// causal parents). Copied into the commit op by the committer.
    pub causal: Vec<Hash32>,
}

/// One priced planner candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorPlanCandidate {
    pub strategy: TensorStrategy,
    pub applicable: bool,
    pub detail: String,
    pub physical_bytes: Option<u64>,
    pub estimated_decode_ns: Option<u64>,
    /// Estimated one-time encode cost (whole tensor), from the write-cost
    /// table. Charged once for the plan's single write.
    pub estimated_encode_ns: Option<u64>,
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
    pub chosen_estimated_encode_ns: u64,
    pub chosen_cost: u128,
    pub runner_up: Option<TensorStrategy>,
    pub runner_up_physical_bytes: Option<u64>,
    pub runner_up_estimated_decode_ns: Option<u64>,
    pub runner_up_estimated_encode_ns: Option<u64>,
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

/// Result of the one-pass rebase decision used by the transcode worker.
#[derive(Clone, Debug)]
pub struct TensorRebaseDecision {
    /// The priced standalone plan (always present).
    pub plan: TensorIndependentPlan,
    /// The publication, or `None` when the plan did not clear the savings bar.
    pub rebase: Option<TensorRebase>,
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
        let seed = identity::os_random_32()?;
        self.write_tensor_file_with_spec_seeded(
            parent, name, dtype, shape, bytes, layout, spec, &seed,
        )
    }

    /// [`Store::write_tensor_file_with_spec`] with an explicit ObjectId seed.
    /// A checkpoint writer derives the seed from its checkpoint seed so the
    /// sequential and the parallel (prepare/commit) paths can be proven to
    /// publish byte-identical state.
    #[allow(clippy::too_many_arguments)]
    pub fn write_tensor_file_with_spec_seeded(
        &mut self,
        parent: Hash32,
        name: &[u8],
        dtype: &str,
        shape: &[u64],
        bytes: &[u8],
        layout: &TensorLayoutSpec,
        spec: &TensorWriteSpec,
        seed: &[u8; 32],
    ) -> Result<Hash32> {
        self.require_writable()?;
        self.require_dir(parent)?;
        crate::state::validate_name_bytes(name)?;
        self.require_free_name(parent, name)?;
        let prepared =
            self.prepare_tensor_object_with_spec_seeded(dtype, shape, bytes, layout, spec, seed)?;
        self.commit_prepared_tensor_object(parent, name, &prepared, 0o644)
    }

    /// Prepare one tensor object: run the planner and publish every immutable
    /// blob, but publish **no** state ops.
    ///
    /// This is the `&self` half of the write path. It never consults or mutates
    /// control state beyond reads (a causal base is read through the caller's
    /// handle), and every blob it writes is content-addressed and create-once,
    /// so N handles may prepare the same store concurrently. The caller must
    /// use [`Store::commit_prepared_tensor_object`] (or install the fragment
    /// itself) to make the object visible; until then GC may reclaim the blobs.
    pub fn prepare_tensor_object_with_spec(
        &self,
        dtype: &str,
        shape: &[u64],
        bytes: &[u8],
        layout: &TensorLayoutSpec,
        spec: &TensorWriteSpec,
    ) -> Result<PreparedTensorObject> {
        let seed = identity::os_random_32()?;
        self.prepare_tensor_object_with_spec_seeded(dtype, shape, bytes, layout, spec, &seed)
    }

    /// [`Store::prepare_tensor_object_with_spec`] with an explicit ObjectId
    /// seed.
    pub fn prepare_tensor_object_with_spec_seeded(
        &self,
        dtype: &str,
        shape: &[u64],
        bytes: &[u8],
        layout: &TensorLayoutSpec,
        spec: &TensorWriteSpec,
        seed: &[u8; 32],
    ) -> Result<PreparedTensorObject> {
        let dtype = TensorElementDtype::parse(dtype)?;
        layout.validate(dtype, shape)?;
        // Row-major callers already hold the canonical bytes; borrowing avoids a
        // second full-tensor copy in the middle of the planner.
        let logical = layout.canonicalize_cow(dtype, shape, bytes)?;
        let prepared = self.prepare_candidates(
            dtype,
            shape,
            logical.as_ref(),
            layout,
            &spec.causal,
            &spec.plan,
            spec.tile_shape
                .as_deref()
                .or(spec.plan.tile_shape.as_deref()),
            false,
            spec.plan.allow_experimental_modes
                || spec
                    .strategy_hint
                    .is_some_and(TensorStrategy::is_experimental),
            spec.strategy_hint,
            true,
        )?;
        let chosen = self.choose_candidate(&prepared, spec.strategy_hint)?;
        let artifact = chosen
            .artifact
            .clone()
            .ok_or_else(|| Error::invalid_argument("chosen tensor strategy has no artifact"))?;
        let root = match &artifact.tensor {
            Some(tensor) => tensor.logical_root,
            None => self.logical_root_for_bytes(dtype, shape, logical.as_ref(), None)?,
        };
        let (manifest_id, content_hash, size) =
            self.store_tensor_artifact(dtype, shape, logical.as_ref(), layout, &artifact)?;
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
        Ok(PreparedTensorObject {
            seed: *seed,
            manifest_id,
            content_root: content_hash,
            size,
            logical_root: root,
            attrs,
            dtype: dtype.as_str().to_owned(),
            shape: shape.to_vec(),
            strategy: artifact.strategy_name.clone(),
            causal: spec.causal.clone(),
        })
    }

    /// Publish the state ops for one prepared tensor object.
    pub fn commit_prepared_tensor_object(
        &mut self,
        parent: Hash32,
        name: &[u8],
        prepared: &PreparedTensorObject,
        mode: u64,
    ) -> Result<Hash32> {
        let (owner_uid, owner_gid) = (self.options().owner_uid, self.options().owner_gid);
        self.commit_created_file(
            parent,
            name,
            &prepared.seed,
            prepared.manifest_id,
            prepared.content_root,
            prepared.size,
            mode,
            owner_uid,
            owner_gid,
            prepared.attrs.clone(),
            &prepared.causal,
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
        let logical = layout.canonicalize_cow(dtype, shape, bytes)?;
        let prepared = self.prepare_candidates(
            dtype,
            shape,
            logical.as_ref(),
            layout,
            causal,
            options,
            options.tile_shape.as_deref(),
            false,
            options.allow_experimental_modes,
            None,
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
            chosen_estimated_encode_ns: chosen.candidate.estimated_encode_ns.unwrap_or(0),
            chosen_cost: chosen.candidate.cost.unwrap_or(0),
            runner_up: runner_up.map(|item| item.candidate.strategy),
            runner_up_physical_bytes: runner_up.and_then(|item| item.candidate.physical_bytes),
            runner_up_estimated_decode_ns: runner_up
                .and_then(|item| item.candidate.estimated_decode_ns),
            runner_up_estimated_encode_ns: runner_up
                .and_then(|item| item.candidate.estimated_encode_ns),
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
            options.allow_experimental_modes || hint.is_some_and(TensorStrategy::is_experimental),
            hint,
            false,
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
        self.rebase_tensor_object_decided(id, options, hint, None, 0)
            .map(|decision| {
                decision
                    .rebase
                    .expect("an unconditional rebase always publishes")
            })
    }

    /// Republish a tensor object with its planned independent representation,
    /// but only when the plan saves at least `min_saved_percent` of
    /// `old_physical_bytes`.
    ///
    /// This is the one-pass form the background transcode worker uses: the
    /// candidate set is encoded **once**, the decision is made from the priced
    /// plan, and a rejected plan costs no publication. `None` for
    /// `old_physical_bytes` means "always publish" (the historical rebase).
    pub fn rebase_tensor_object_decided(
        &mut self,
        id: Hash32,
        options: &TensorPlanOptions,
        hint: Option<TensorStrategy>,
        old_physical_bytes: Option<u64>,
        min_saved_percent: u8,
    ) -> Result<TensorRebaseDecision> {
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
            options.allow_experimental_modes || hint.is_some_and(TensorStrategy::is_experimental),
            hint,
            true,
        )?;
        let chosen = self.choose_candidate(&prepared, hint)?;
        let plan = TensorIndependentPlan {
            chosen: chosen.candidate.strategy,
            physical_bytes: chosen.candidate.physical_bytes.unwrap_or(0),
            estimated_decode_ns: chosen.candidate.estimated_decode_ns.unwrap_or(0),
            cost: chosen.candidate.cost.unwrap_or(0),
            candidates: prepared.iter().map(|item| item.candidate.clone()).collect(),
        };
        if let Some(old_physical) = old_physical_bytes {
            let new_physical = plan.physical_bytes;
            if new_physical >= old_physical {
                return Ok(TensorRebaseDecision { plan, rebase: None });
            }
            let saved = old_physical - new_physical;
            if min_saved_percent > 0
                && saved.saturating_mul(100)
                    < old_physical.saturating_mul(u64::from(min_saved_percent))
            {
                return Ok(TensorRebaseDecision { plan, rebase: None });
            }
        }
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
        Ok(TensorRebaseDecision {
            plan,
            rebase: Some(TensorRebase {
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
                    candidates: Vec::new(),
                },
            }),
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
            self.read_manifest_keyed(record.content)?;
        if manifest.size != record.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        if let Some(rep) = &manifest.representation {
            if rep.kind == RepresentationKind::Tensor {
                if let Some(tensor) = &rep.tensor {
                    if tensor.tensor_mode == 20 {
                        let frame_id = rep.frame.ok_or_else(|| {
                            Error::corrupt("tensor representation has no frame id")
                        })?;
                        return self.read_engram_object(frame_id, tensor, &manifest, rep);
                    }
                    if tensor.tensor_mode == 15
                        || tensor.tensor_mode == 17
                        || tensor.tensor_mode == 18
                    {
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
            self.read_manifest_keyed(record.content)?;
        if manifest.size != record.size {
            return Err(Error::corrupt("object size disagrees with manifest"));
        }
        if let Some(rep) = &manifest.representation {
            if rep.kind == RepresentationKind::Tensor {
                if let Some(tensor) = &rep.tensor {
                    if tensor.tensor_mode == 20 {
                        let frame_id = rep.frame.ok_or_else(|| {
                            Error::corrupt("tensor representation has no frame id")
                        })?;
                        let bytes = self.read_engram_object(frame_id, tensor, &manifest, rep)?;
                        self.note_object_read(id);
                        return Ok((bytes, TileDecodeStats::none(options)));
                    }
                    if tensor.tensor_mode == 15
                        || tensor.tensor_mode == 17
                        || tensor.tensor_mode == 18
                    {
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

    /// Publish an incremental engram update.
    ///
    /// The listed groups are replaced with the supplied canonical bytes; every
    /// other group is emitted as a reference to its existing group root, so
    /// only the changed groups' bytes are written. The previous object version
    /// is never rewritten. The new version's descriptor records the replaced
    /// group root in `supersedes` and the earlier artifact as its dependency.
    ///
    /// Returns the new object version id.
    pub fn update_engram_groups(
        &mut self,
        id: Hash32,
        updates: &[EngramGroupUpdate<'_>],
    ) -> Result<Hash32> {
        self.require_writable()?;
        if updates.is_empty() {
            return Err(Error::invalid_argument(
                "an engram update must change at least one group",
            ));
        }
        let record = self.object_meta(id)?;
        if record.kind != ObjectKind::File {
            return Err(Error::invalid_argument(
                "engram updates require a file object",
            ));
        }
        let manifest =
            self.read_manifest_keyed(record.content)?;
        let rep = manifest
            .representation
            .as_ref()
            .ok_or_else(|| Error::invalid_argument("object has no representation"))?;
        let tensor = rep
            .tensor
            .as_ref()
            .ok_or_else(|| Error::invalid_argument("object is not a tensor"))?;
        if tensor.tensor_mode != 20 || rep.strategy != "engram-shard" {
            return Err(Error::invalid_argument(
                "engram updates require an engram-shard object",
            ));
        }
        let frame_id = rep
            .frame
            .ok_or_else(|| Error::corrupt("engram representation has no frame id"))?;
        let frame_bytes = self.read_blob_verified_keyed(BlobKind::Manifest, frame_id, identity::MANIFEST_DOMAIN)?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        let meta = weftcodec::engram::engram_metadata(&frame, &limits)?;
        let old_table = self.read_tensor(id)?;
        let word = meta.dtype.element_size();
        let dim = meta.dim();
        let row_bytes = dim
            .checked_mul(word)
            .ok_or_else(|| Error::limit_exceeded("engram row length overflows"))?;
        let mut new_table = old_table;
        let mut changed = std::collections::BTreeSet::new();
        for update in updates {
            let index = update.group as usize;
            if index >= meta.groups.len() {
                return Err(Error::invalid_argument(format!(
                    "engram group {} is outside the table",
                    update.group
                )));
            }
            if !changed.insert(update.group) {
                return Err(Error::invalid_argument(format!(
                    "engram group {} appears more than once in one update",
                    update.group
                )));
            }
            let group = &meta.groups[index];
            let expected = (group.row_end - group.row_start)
                .checked_mul(row_bytes)
                .ok_or_else(|| Error::limit_exceeded("engram group length overflows"))?;
            if update.bytes.len() as u64 != expected {
                return Err(Error::invalid_argument(format!(
                    "engram group {} update is {} bytes but the group holds {expected}",
                    update.group,
                    update.bytes.len()
                )));
            }
            let start = usize::try_from(group.row_start * row_bytes)
                .map_err(|_| Error::limit_exceeded("engram group start does not fit usize"))?;
            let end = start
                .checked_add(update.bytes.len())
                .ok_or_else(|| Error::limit_exceeded("engram group end overflows usize"))?;
            let slot = new_table
                .get_mut(start..end)
                .ok_or_else(|| Error::corrupt("engram group range is outside the table"))?;
            slot.copy_from_slice(update.bytes);
        }
        let mut references = BTreeMap::new();
        let mut supersedes = vec![None; meta.groups.len()];
        for (index, group) in meta.groups.iter().enumerate() {
            if changed.contains(&(index as u32)) {
                supersedes[index] = Some(group.group_root);
            } else {
                references.insert(
                    index as u64,
                    weftcodec::engram::EngramReference {
                        group_root: group.group_root,
                        dependency: 0,
                    },
                );
                supersedes[index] = group.supersedes;
            }
        }
        let physical = meta.dtype;
        let input = weftcodec::engram::EngramInput {
            hash_function: meta.hash_function.clone(),
            vocab_size: meta.vocab_size,
            table_shape: meta.table_shape.clone(),
            dtype: physical,
            precision: meta.precision.clone(),
            row_group_edges: meta.row_group_edges.clone(),
            references,
            supersedes,
        };
        let new_frame = weftcodec::engram::encode_engram_with_dependencies(
            &new_table,
            &input,
            &[manifest.content_hash],
            &limits,
        )?;
        let root =
            weftcodec::tensor::logical_root(physical, &meta.table_shape, &new_table, &limits)?;
        let layout = TensorLayoutSpec::row_major();
        let artifact = PreparedArtifact {
            kind: RepresentationKind::Tensor,
            strategy_name: "engram-shard".to_owned(),
            frame: Some(new_frame),
            dependencies: vec![record.content],
            tensor: Some(TensorRepresentation {
                tensor_mode: 20,
                dtype: tensor.dtype,
                shape: meta.table_shape.clone(),
                logical_root: root,
                layout: layout.to_value(tensor.dtype, &meta.table_shape)?,
            }),
            tile_shape: None,
        };
        let (manifest_id, content_hash, size) = self.store_tensor_artifact(
            tensor.dtype,
            &meta.table_shape,
            &new_table,
            &layout,
            &artifact,
        )?;
        let mut attrs = BTreeMap::new();
        apply_tensor_attrs(
            &mut attrs,
            tensor.dtype,
            &meta.table_shape,
            root,
            &layout,
            "engram-shard",
            None,
        )?;
        self.publish_file_version_with_attrs(
            id,
            record,
            manifest_id,
            content_hash,
            size,
            &[],
            attrs,
        )
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
            self.read_manifest_keyed(record.content)?;
        if let Some(rep) = &manifest.representation {
            if rep.kind == RepresentationKind::Tensor {
                if let Some(tensor) = &rep.tensor {
                    if tensor.tensor_mode == 20 {
                        let frame_id = rep.frame.ok_or_else(|| {
                            Error::corrupt("tensor representation has no frame id")
                        })?;
                        return self.read_engram_region(frame_id, tensor, rep, region, options);
                    }
                    if tensor.tensor_mode == 15
                        || tensor.tensor_mode == 17
                        || tensor.tensor_mode == 18
                    {
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
        let frame_bytes = self.read_blob_verified_keyed(BlobKind::Manifest, frame_id, identity::MANIFEST_DOMAIN)?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        let expected = match tensor.tensor_mode {
            15 => Mode::TensorTiled,
            17 => Mode::FloatTiled,
            18 => Mode::FloatTiledStreams,
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
        let (logical, stats) = if tensor.tensor_mode == 18 {
            self.decode_mode18_with_options(&frame, tensor, manifest, rep, options)?
        } else if tensor.tensor_mode == 17 {
            self.decode_mode17_with_options(&frame, tensor, manifest, rep, options)?
        } else {
            self.decode_mode15_with_options(&frame, tensor, manifest, rep, options)?
        };
        if identity::content_hash(&logical) != manifest.content_hash {
            return Err(Error::corrupt("whole-content hash mismatch"));
        }
        Ok((logical, stats))
    }

    /// Build a resolver for a mode-20 frame's dependency artifacts. Each
    /// representation dependency is the earlier tensor object's **manifest
    /// id**; its stored frame supplies the referenced group table. The map key
    /// is the frame's own content hash, which is the hash the engram frame's
    /// dependency table names.
    fn engram_resolver(&self, rep: &Representation) -> Result<MapResolver> {
        let limits = Limits::default();
        let mut resolver = MapResolver::new();
        for dependency in &rep.dependencies {
            let manifest =
                self.read_manifest_keyed(*dependency)?;
            let dep_rep = manifest
                .representation
                .ok_or_else(|| Error::corrupt("engram dependency has no representation"))?;
            let frame_id = dep_rep
                .frame
                .ok_or_else(|| Error::corrupt("engram dependency has no frame id"))?;
            let frame_bytes = self.read_blob_verified_keyed(BlobKind::Manifest, frame_id, identity::MANIFEST_DOMAIN)?;
            let frame = Frame::parse(&frame_bytes, &limits)?;
            resolver.insert(frame.header.content_hash, frame_bytes);
        }
        Ok(resolver)
    }

    /// Decode a complete mode-20 object and verify its representation
    /// identity. References resolve through the earlier artifacts named by the
    /// representation's dependency list.
    fn read_engram_object(
        &self,
        frame_id: Hash32,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        rep: &Representation,
    ) -> Result<Vec<u8>> {
        let (layout_dtype, layout_shape, _) = TensorLayoutSpec::from_value(&tensor.layout)?;
        if layout_dtype != tensor.dtype || layout_shape != tensor.shape {
            return Err(Error::corrupt(
                "tensor layout descriptor disagrees with the manifest dtype/shape",
            ));
        }
        let frame_bytes = self.read_blob_verified_keyed(BlobKind::Manifest, frame_id, identity::MANIFEST_DOMAIN)?;
        let limits = Limits::default();
        let frame = Frame::parse(&frame_bytes, &limits)?;
        if frame.header.mode != Mode::EngramShard {
            return Err(Error::corrupt(format!(
                "tensor representation mode 20 has frame mode `{}`",
                frame.header.mode.as_str()
            )));
        }
        let meta = weftcodec::engram::engram_metadata(&frame, &limits)?;
        let physical = tensor
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("engram dtype has no element layout"))?;
        if meta.dtype != physical || meta.table_shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-20 dtype/shape disagrees with the representation manifest",
            ));
        }
        let resolver = self.engram_resolver(rep)?;
        let logical = weftcodec::engram::decode_engram(&frame, &resolver, &limits)?;
        let root = weftcodec::tensor::logical_root(physical, &tensor.shape, &logical, &limits)?;
        if root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-20 logical root disagrees with the manifest logical_root",
            ));
        }
        if identity::content_hash(&logical) != manifest.content_hash {
            return Err(Error::corrupt("whole-content hash mismatch"));
        }
        Ok(logical)
    }

    /// Region read for a mode-20 object: only the groups intersecting the row
    /// range are fetched and decoded. Like the mode-15 path this does not read
    /// the whole-frame trailer or re-check the whole-content hash.
    fn read_engram_region(
        &self,
        frame_id: Hash32,
        tensor: &TensorRepresentation,
        rep: &Representation,
        region: &Region,
        options: &TileDecodeOptions,
    ) -> Result<(RegionRead, TileDecodeStats)> {
        let limits = Limits::default();
        let key = crate::backend::Key::manifest(frame_id);
        let probe_len = 96u64
            .checked_add(
                limits
                    .max_dependency_count
                    .saturating_mul(weftcodec::frame::DEP_ENTRY_LEN as u64),
            )
            .ok_or_else(|| Error::limit_exceeded("mode-20 probe length overflows"))?;
        let probe = self.with_backend(|backend| backend.read(&key, 0, probe_len))?;
        if probe.len() < 96 {
            return Err(Error::corrupt("mode-20 frame is shorter than its header"));
        }
        let (header, _) = weftcodec::frame::parse_prefix(&probe, &limits)?;
        if header.mode != Mode::EngramShard {
            return Err(Error::corrupt(format!(
                "tensor representation mode 20 has frame mode `{}`",
                header.mode.as_str()
            )));
        }
        if header.chunk_count != 0 {
            return Err(Error::corrupt("mode-20 frame must not carry a chunk table"));
        }
        let table_len = header.table_len;
        let tables_end = 96u64
            .checked_add(table_len)
            .ok_or_else(|| Error::limit_exceeded("mode-20 table length overflows"))?;
        let dep_bytes = if table_len == 0 {
            Vec::new()
        } else {
            self.with_backend(|backend| backend.read(&key, 96, table_len))?
        };
        if dep_bytes.len() as u64 != table_len {
            return Err(Error::corrupt("mode-20 dependency table is truncated"));
        }
        let mut dependencies = Vec::with_capacity(header.dep_count as usize);
        for index in 0..header.dep_count as usize {
            let off = index * weftcodec::frame::DEP_ENTRY_LEN;
            let entry = dep_bytes
                .get(off..off + weftcodec::frame::DEP_ENTRY_LEN)
                .ok_or_else(|| Error::corrupt("mode-20 dependency entry is truncated"))?;
            let hash = identity::Hash32::from_slice(&entry[0..32])?;
            let decoded_len = u64::from_le_bytes(entry[32..40].try_into().unwrap_or([0; 8]));
            let dep_domain = entry[40];
            let dep_flags = entry[41];
            let codec_hint = u32::from_le_bytes(entry[44..48].try_into().unwrap_or([0; 4]));
            dependencies.push(weftcodec::Dependency {
                hash,
                decoded_len,
                dep_domain,
                dep_flags,
                codec_hint,
            });
        }
        let prefix_read = self.with_backend(|backend| backend.read(&key, tables_end, 4))?;
        if prefix_read.len() != 4 {
            return Err(Error::corrupt("mode-20 descriptor length is truncated"));
        }
        let meta_len = u64::from(u32::from_le_bytes(
            prefix_read.as_slice().try_into().unwrap_or([0; 4]),
        ));
        let payload_len = header
            .body_len
            .checked_sub(4)
            .and_then(|value| value.checked_sub(meta_len))
            .ok_or_else(|| Error::corrupt("mode-20 descriptor extends past the body"))?;
        weftcodec::limits::check_len(meta_len, limits.max_string_bytes, "mode-20 descriptor")?;
        let descriptor_start = tables_end + 4;
        let descriptor =
            self.with_backend(|backend| backend.read(&key, descriptor_start, meta_len))?;
        if descriptor.len() as u64 != meta_len {
            return Err(Error::corrupt("mode-20 descriptor is truncated"));
        }
        let meta = weftcodec::engram::parse_engram_descriptor(
            &descriptor,
            header.decoded_len,
            payload_len,
            &[],
            &limits,
        )?;
        let physical = tensor
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("engram dtype has no element layout"))?;
        if meta.dtype != physical || meta.table_shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-20 dtype/shape disagrees with the representation manifest",
            ));
        }
        region.validate(&meta.table_shape)?;
        if region.is_empty() {
            return Ok((
                RegionRead {
                    shape: region.extent(),
                    bytes: Vec::new(),
                    units_touched: 0,
                },
                TileDecodeStats::none(options),
            ));
        }
        let header_prefix =
            self.with_backend(|backend| backend.read(&key, 0, descriptor_start + meta_len))?;
        let synthetic = Frame {
            header,
            dependencies,
            chunks: Vec::new(),
            body: &[],
            bytes: &header_prefix,
        };
        let resolver = self.engram_resolver(rep)?;
        let descriptor_view = weftcodec::engram::EngramDescriptor {
            meta: meta.clone(),
            payload: &[],
        };
        let payload_base = descriptor_start + meta_len;
        let (row_bytes, touched) = weftcodec::engram::read_engram_region_with(
            &synthetic,
            &descriptor_view,
            region.start[0],
            region.end[0],
            &resolver,
            &limits,
            |group| match group.storage {
                weftcodec::engram::EngramStorage::Inline {
                    offset,
                    compressed_len,
                } => {
                    let absolute = payload_base.checked_add(offset).ok_or_else(|| {
                        weftcodec::Error::limit_exceeded("engram group offset overflows")
                    })?;
                    let stored = self
                        .with_backend(|backend| backend.read(&key, absolute, compressed_len))
                        .map_err(|err| weftcodec::Error::io(err.to_string()))?;
                    Ok(Some(stored))
                }
                weftcodec::engram::EngramStorage::Reference { .. } => Ok(None),
            },
        )?;
        let dim = meta.dim();
        let word = meta.dtype.element_size();
        let start_col = region.start[1];
        let end_col = region.end[1];
        let bytes = if start_col == 0 && end_col == dim {
            row_bytes
        } else {
            let rows = region.end[0] - region.start[0];
            let row_len = usize::try_from(
                dim.checked_mul(word)
                    .ok_or_else(|| Error::limit_exceeded("engram row length overflows"))?,
            )
            .map_err(|_| Error::limit_exceeded("engram row length does not fit usize"))?;
            let col_start = usize::try_from(start_col * word)
                .map_err(|_| Error::limit_exceeded("engram column start does not fit usize"))?;
            let col_end = usize::try_from(end_col * word)
                .map_err(|_| Error::limit_exceeded("engram column end does not fit usize"))?;
            let mut out = Vec::with_capacity(
                usize::try_from(rows)
                    .unwrap_or(0)
                    .saturating_mul(col_end.saturating_sub(col_start)),
            );
            for row in 0..usize::try_from(rows).unwrap_or(0) {
                let base = row * row_len;
                let slice = row_bytes
                    .get(base + col_start..base + col_end)
                    .ok_or_else(|| Error::corrupt("engram region slice is outside the rows"))?;
                out.extend_from_slice(slice);
            }
            out
        };
        Ok((
            RegionRead {
                shape: region.extent(),
                bytes,
                units_touched: touched.len() as u64,
            },
            TileDecodeStats::none(options),
        ))
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
        if tensor.tensor_mode == 18 {
            return self.read_float_tiled_streams_region(frame_id, tensor, region, options);
        }
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
                weftcodec::decode_tiled_tile(meta.dtype, entry, &stored, &limits).map_err(|err| {
                    let err = Error::from(err);
                    Error::new(
                        err.class(),
                        format!(
                            "tile {} (offset {}): {}",
                            index,
                            entry.offset,
                            err.message()
                        ),
                    )
                })
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
                weftcodec::decode_float_tile(meta.dtype, entry, &stored, &limits).map_err(|err| {
                    let err = Error::from(err);
                    Error::new(
                        err.class(),
                        format!(
                            "tile {} (offset {}): {}",
                            index,
                            entry.offset,
                            err.message()
                        ),
                    )
                })
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

    /// Region read for a mode-18 multi-stream float-tiled object.
    ///
    /// Only the frame prefix and the stored ranges of intersecting tiles are
    /// read. Each tile decodes its exponent streams with the wide-word kernel
    /// and verifies its stream-aware tile root. The descriptor's dtype, shape
    /// and logical root are checked against the representation manifest before
    /// any tile is read.
    fn read_float_tiled_streams_region(
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
                "mode-18 frame is shorter than its header and descriptor length",
            ));
        }
        let (header, _) = weftcodec::frame::parse_prefix(&probe, &limits)?;
        if header.mode != Mode::FloatTiledStreams {
            return Err(Error::corrupt(format!(
                "tensor representation mode 18 has frame mode `{}`",
                header.mode.as_str()
            )));
        }
        if header.table_len != 0 || header.dep_count != 0 || header.chunk_count != 0 {
            return Err(Error::corrupt(
                "mode-18 frame must not carry dependency or chunk tables",
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
                "mode-18 descriptor length extends past the frame body",
            ));
        }
        weftcodec::limits::check_len(meta_len, limits.max_string_bytes, "mode-18 descriptor")?;
        let prefix_len = 96u64
            .checked_add(4)
            .and_then(|value| value.checked_add(meta_len))
            .ok_or_else(|| Error::limit_exceeded("mode-18 prefix length overflows"))?;
        let prefix = if probe.len() as u64 >= prefix_len {
            probe
        } else {
            self.with_backend(|backend| backend.read(&key, 0, prefix_len))?
        };
        if prefix.len() as u64 != prefix_len {
            return Err(Error::corrupt("mode-18 frame prefix is truncated"));
        }
        let descriptor_bytes = &prefix[100..];
        let stored_payload_len = header.body_len - 4 - meta_len;
        let meta = weftcodec::modes::parse_float_tiled_streams_descriptor(
            descriptor_bytes,
            header.decoded_len,
            stored_payload_len,
            &limits,
        )?;
        if !float_dtype_matches_tensor(meta.dtype, tensor.dtype) {
            return Err(Error::corrupt(format!(
                "mode-18 dtype `{}` disagrees with manifest dtype `{}`",
                meta.dtype.as_str(),
                tensor.dtype.as_str()
            )));
        }
        if meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-18 shape disagrees with the representation manifest",
            ));
        }
        if meta.logical_root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-18 logical_root disagrees with the manifest logical_root",
            ));
        }
        let indices = weftcodec::float_tiled_streams_region_indices(&meta, region)?;
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
                weftcodec::decode_float_tile_streams(
                    meta.dtype,
                    entry,
                    meta.n_streams,
                    &stored,
                    &limits,
                    1,
                )
                .map_err(|err| {
                    let err = Error::from(err);
                    Error::new(
                        err.class(),
                        format!(
                            "tile {} (offset {}): {}",
                            index,
                            entry.offset,
                            err.message()
                        ),
                    )
                })
            },
            |tile| tile.len(),
        )?;
        for (index, tile) in indices.iter().zip(tiles.iter()) {
            weftcodec::place_float_tiled_streams_tile_region(
                &mut out,
                region,
                &meta,
                &meta.tiles[*index],
                tile,
            )?;
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
            self.read_manifest_keyed(record.content)?;
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
        let frame_bytes = self.read_blob_verified_keyed(BlobKind::Manifest, frame_id, identity::MANIFEST_DOMAIN)?;
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
            self.read_manifest_keyed(record.content)?;
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
            spec.plan.allow_experimental_modes
                || spec
                    .strategy_hint
                    .is_some_and(TensorStrategy::is_experimental),
            spec.strategy_hint,
            true,
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
        allow_experimental: bool,
        hint: Option<TensorStrategy>,
        keep_artifacts: bool,
    ) -> Result<Vec<PreparedCandidate>> {
        let _phase = crate::phase_profile::scope(crate::phase_profile::Phase::Encode);
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

        // Critical-path fast path: build at most one linear-pass representation
        // and return without encoding a single planned candidate. Nothing here
        // calls a mode encoder, so a save pays only the chunker or one ZSTD
        // frame. The logical identity is unchanged, so a later background
        // transcode can republish the object with a smaller representation
        // without touching any manifest or checkpoint record.
        if options.write_policy.is_fast() && !quantized {
            retain_candidate(
                &mut out,
                hint,
                keep_artifacts,
                PreparedCandidate {
                    candidate: TensorPlanCandidate {
                        strategy: TensorStrategy::Raw,
                        applicable: true,
                        detail: "canonical row-major chunks (fast policy)".to_owned(),
                        physical_bytes: Some(size),
                        estimated_decode_ns: Some(options.estimate_ns_at_pattern(
                            crate::cost_model::CostClass::Raw,
                            TensorStrategy::Raw,
                            size,
                        )),
                        estimated_encode_ns: Some(
                            options.estimate_encode_ns(TensorStrategy::Raw, size),
                        ),
                        cost: Some(options.price_candidate(
                            size,
                            options.estimate_ns_at_pattern(
                                crate::cost_model::CostClass::Raw,
                                TensorStrategy::Raw,
                                size,
                            ),
                            options.estimate_encode_ns(TensorStrategy::Raw, size),
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
                },
            );
            if options.fast_codec == FastCodec::Zstd {
                let level = options.fast_zstd_level.clamp(1, 3);
                let codec_chunk =
                    crate::representation::codec_chunk_size(self.options().chunk_size.max(1))
                        as usize;
                match crate::phase_profile::timed(crate::phase_profile::Phase::EncodeZstd, || {
                    weftcodec::modes::encode_zstd(logical, level, Some(codec_chunk), &limits)
                }) {
                    Ok(frame) => {
                        let physical = frame.len() as u64;
                        let estimate = options.estimate_ns_at_pattern(
                            crate::cost_model::CostClass::Zstd,
                            TensorStrategy::Zstd,
                            size,
                        );
                        retain_candidate(
                            &mut out,
                            hint,
                            keep_artifacts,
                            PreparedCandidate {
                                candidate: TensorPlanCandidate {
                                    strategy: TensorStrategy::Zstd,
                                    applicable: true,
                                    detail: format!("one ZSTD level-{level} frame (fast policy)"),
                                    physical_bytes: Some(physical),
                                    estimated_decode_ns: Some(estimate),
                                    estimated_encode_ns: Some(
                                        options.estimate_encode_ns(TensorStrategy::Zstd, size),
                                    ),
                                    cost: Some(options.price_candidate(
                                        physical,
                                        estimate,
                                        options.estimate_encode_ns(TensorStrategy::Zstd, size),
                                    )),
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
                            },
                        );
                    }
                    Err(err) => out.push(skipped(TensorStrategy::Zstd, err.to_string())),
                }
            }
            return Ok(out);
        }

        // Raw chunks (v2 kind:"raw").
        //
        // A caller that pinned the strategy to RAW or ZSTD cannot choose
        // anything else: `choose_candidate` returns the first applicable
        // candidate with that strategy or a typed error. Building the rest
        // would be work thrown away — measured at 22.7 ms of mode-9 encodes
        // per checkpoint on the R12b training fixture, whose DCP adapter pins
        // every shard to ZSTD. The two blocks below are the same candidate
        // objects the full path builds first for those hints.
        if hint == Some(TensorStrategy::Raw) && !quantized {
            let raw_candidate = PreparedCandidate {
                candidate: TensorPlanCandidate {
                    strategy: TensorStrategy::Raw,
                    applicable: true,
                    detail: "canonical row-major chunks".to_owned(),
                    physical_bytes: Some(size),
                    estimated_decode_ns: Some(options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::Raw,
                        TensorStrategy::Raw,
                        size,
                    )),
                    estimated_encode_ns: Some(
                        options.estimate_encode_ns(TensorStrategy::Raw, size),
                    ),
                    cost: Some(options.price_candidate(
                        size,
                        options.estimate_ns_at_pattern(
                            crate::cost_model::CostClass::Raw,
                            TensorStrategy::Raw,
                            size,
                        ),
                        options.estimate_encode_ns(TensorStrategy::Raw, size),
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
            };
            retain_candidate(&mut out, hint, keep_artifacts, raw_candidate);
            return Ok(out);
        }
        if hint == Some(TensorStrategy::Zstd) && !quantized {
            let codec_chunk =
                crate::representation::codec_chunk_size(self.options().chunk_size.max(1)) as usize;
            match crate::phase_profile::timed(crate::phase_profile::Phase::EncodeZstd, || {
                weftcodec::modes::encode_zstd(
                    logical,
                    crate::representation::CODEC_ZSTD_LEVEL,
                    Some(codec_chunk),
                    &limits,
                )
            }) {
                Ok(frame) => {
                    let physical = frame.len() as u64;
                    let estimate = options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::Zstd,
                        TensorStrategy::Zstd,
                        size,
                    );
                    retain_candidate(
                        &mut out,
                        hint,
                        keep_artifacts,
                        PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::Zstd,
                                applicable: true,
                                detail: "one ZSTD frame".to_owned(),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                estimated_encode_ns: Some(
                                    options.estimate_encode_ns(TensorStrategy::Zstd, size),
                                ),
                                cost: Some(options.price_candidate(
                                    physical,
                                    estimate,
                                    options.estimate_encode_ns(TensorStrategy::Zstd, size),
                                )),
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
                        },
                    );
                }
                Err(err) => out.push(skipped(TensorStrategy::Zstd, err.to_string())),
            }
            return Ok(out);
        }

        let raw_candidate = if quantized {
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
                    estimated_decode_ns: Some(options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::Raw,
                        TensorStrategy::Raw,
                        size,
                    )),
                    estimated_encode_ns: Some(
                        options.estimate_encode_ns(TensorStrategy::Raw, size),
                    ),
                    cost: Some(options.price_candidate(
                        size,
                        options.estimate_ns_at_pattern(
                            crate::cost_model::CostClass::Raw,
                            TensorStrategy::Raw,
                            size,
                        ),
                        options.estimate_encode_ns(TensorStrategy::Raw, size),
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
        };
        retain_candidate(&mut out, hint, keep_artifacts, raw_candidate);

        // Zstandard frame.
        let codec_chunk =
            crate::representation::codec_chunk_size(self.options().chunk_size.max(1)) as usize;
        if quantized {
            out.push(skipped(
                TensorStrategy::Zstd,
                "q8_0 identity requires the mode-10 scale plane".to_owned(),
            ));
        } else {
            match crate::phase_profile::timed(crate::phase_profile::Phase::EncodeZstd, || {
                weftcodec::modes::encode_zstd(
                    logical,
                    crate::representation::CODEC_ZSTD_LEVEL,
                    Some(codec_chunk),
                    &limits,
                )
            }) {
                Ok(frame) => {
                    let physical = frame.len() as u64;
                    let estimate = options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::Zstd,
                        TensorStrategy::Zstd,
                        size,
                    );
                    retain_candidate(
                        &mut out,
                        hint,
                        keep_artifacts,
                        PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::Zstd,
                                applicable: true,
                                detail: "one ZSTD frame".to_owned(),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                estimated_encode_ns: Some(
                                    options.estimate_encode_ns(TensorStrategy::Zstd, size),
                                ),
                                cost: Some(options.price_candidate(
                                    physical,
                                    estimate,
                                    options.estimate_encode_ns(TensorStrategy::Zstd, size),
                                )),
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
                        },
                    );
                }
                Err(err) => out.push(skipped(TensorStrategy::Zstd, err.to_string())),
            }
        }

        // ZSTD level 19: the same frozen ZSTD mode (level is not part of the
        // wire identity), encoded once more at a much slower setting. It is
        // gated three ways so it can never land on a training critical path:
        // only `Planned` offers it, only at or above the size threshold, and
        // only when the estimated encode time fits the CPU budget.
        if quantized {
            out.push(skipped(
                TensorStrategy::Zstd,
                "q8_0 identity requires the mode-10 scale plane".to_owned(),
            ));
        } else if options.write_policy.is_fast() {
            out.push(skipped(
                TensorStrategy::Zstd,
                "zstd-19 is never offered by the fast write policy".to_owned(),
            ));
        } else if !options.zstd19 {
            out.push(skipped(
                TensorStrategy::Zstd,
                "zstd-19 disabled (--no-zstd19)".to_owned(),
            ));
        } else if size < options.zstd19_min_bytes {
            out.push(skipped(
                TensorStrategy::Zstd,
                format!(
                    "zstd-19 skipped: {size} bytes is below the {} byte threshold",
                    options.zstd19_min_bytes
                ),
            ));
        } else {
            let encode_ns =
                crate::cost_model::estimate_encode_ns(crate::cost_model::EncodeClass::Zstd19, size);
            if encode_ns > options.zstd19_cpu_budget_ns {
                out.push(skipped(
                    TensorStrategy::Zstd,
                    format!(
                        "zstd-19 skipped: estimated encode {encode_ns} ns exceeds the {} ns CPU budget",
                        options.zstd19_cpu_budget_ns
                    ),
                ));
            } else {
                match crate::phase_profile::timed(crate::phase_profile::Phase::EncodeZstd19, || {
                    weftcodec::modes::encode_zstd(logical, 19, Some(codec_chunk), &limits)
                }) {
                    Ok(frame) => {
                        let physical = frame.len() as u64;
                        let estimate = options.estimate_ns_at_pattern(
                            crate::cost_model::CostClass::Zstd,
                            TensorStrategy::Zstd,
                            size,
                        );
                        retain_candidate(
                            &mut out,
                            hint,
                            keep_artifacts,
                            PreparedCandidate {
                                candidate: TensorPlanCandidate {
                                    strategy: TensorStrategy::Zstd,
                                    applicable: true,
                                    detail: "one ZSTD level-19 frame".to_owned(),
                                    physical_bytes: Some(physical),
                                    estimated_decode_ns: Some(estimate),
                                    estimated_encode_ns: Some(encode_ns),
                                    cost: Some(
                                        options.price_candidate(physical, estimate, encode_ns),
                                    ),
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
                            },
                        );
                    }
                    Err(err) => out.push(skipped(TensorStrategy::Zstd, err.to_string())),
                }
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
                match crate::phase_profile::timed(
                    crate::phase_profile::Phase::EncodeFloatTensor,
                    || weftcodec::modes::encode_float_tensor(logical, float_dtype, shape, &limits),
                ) {
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
                            let estimate = options.estimate_ns_at_pattern(
                                crate::cost_model::CostClass::FloatTensor,
                                TensorStrategy::FloatTensor,
                                size,
                            );
                            let root = self.logical_root_for_bytes(dtype, shape, logical, None)?;
                            retain_candidate(
                                &mut out,
                                hint,
                                keep_artifacts,
                                PreparedCandidate {
                                    candidate: TensorPlanCandidate {
                                        strategy: TensorStrategy::FloatTensor,
                                        applicable: true,
                                        detail: format!(
                                            "one mode-9 FLOAT_TENSOR frame ({})",
                                            float_dtype.as_str()
                                        ),
                                        physical_bytes: Some(physical),
                                        estimated_decode_ns: Some(estimate),
                                        estimated_encode_ns: Some(
                                            options.estimate_encode_ns(
                                                TensorStrategy::FloatTensor,
                                                size,
                                            ),
                                        ),
                                        cost: Some(options.price_candidate(
                                            physical,
                                            estimate,
                                            options.estimate_encode_ns(
                                                TensorStrategy::FloatTensor,
                                                size,
                                            ),
                                        )),
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
                                },
                            );
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
                    let estimate = options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::TensorSharded,
                        TensorStrategy::TensorSharded,
                        size,
                    );
                    retain_candidate(
                        &mut out,
                        hint,
                        keep_artifacts,
                        PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::TensorSharded,
                                applicable: true,
                                detail: "one mode-10 shard (shard_count = 1)".to_owned(),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                estimated_encode_ns: Some(
                                    options.estimate_encode_ns(TensorStrategy::TensorSharded, size),
                                ),
                                cost: Some(options.price_candidate(
                                    physical,
                                    estimate,
                                    options.estimate_encode_ns(TensorStrategy::TensorSharded, size),
                                )),
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
                        },
                    );
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
                        let geometry = TileGeometry::new(shape, &tile_shape, dtype);
                        let estimate = options.estimate_ns_at_pattern_for(
                            crate::cost_model::CostClass::TensorTiled,
                            TensorStrategy::TensorTiled,
                            size,
                            geometry.as_ref(),
                        );
                        retain_candidate(
                            &mut out,
                            hint,
                            keep_artifacts,
                            PreparedCandidate {
                                candidate: TensorPlanCandidate {
                                    strategy: TensorStrategy::TensorTiled,
                                    applicable: true,
                                    detail: format!(
                                        "one mode-15 frame tiled {tile_shape:?} ({origin})"
                                    ),
                                    physical_bytes: Some(physical),
                                    estimated_decode_ns: Some(estimate),
                                    estimated_encode_ns: Some(
                                        options
                                            .estimate_encode_ns(TensorStrategy::TensorTiled, size),
                                    ),
                                    cost: Some(
                                        options.price_candidate(
                                            physical,
                                            estimate,
                                            options.estimate_encode_ns(
                                                TensorStrategy::TensorTiled,
                                                size,
                                            ),
                                        ),
                                    ),
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
                            },
                        );
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

        // Mode 20 engram-shard. The caller must supply the row-group metadata;
        // without it the candidate is skipped and never auto-selected.
        match &options.engram {
            Some(spec) => match self.prepare_mode20(dtype, shape, logical, spec, &limits) {
                Ok((frame, root)) => {
                    let physical = frame.len() as u64;
                    let estimate = options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::TensorTiled,
                        TensorStrategy::EngramShard,
                        size,
                    );
                    retain_candidate(
                        &mut out,
                        hint,
                        keep_artifacts,
                        PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::EngramShard,
                                applicable: true,
                                detail: format!(
                                    "one mode-20 frame with {} row groups",
                                    spec.row_group_edges.len().saturating_sub(1)
                                ),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                estimated_encode_ns: Some(
                                    options.estimate_encode_ns(TensorStrategy::EngramShard, size),
                                ),
                                cost: Some(options.price_candidate(
                                    physical,
                                    estimate,
                                    options.estimate_encode_ns(TensorStrategy::EngramShard, size),
                                )),
                                dependency: None,
                            },
                            artifact: Some(PreparedArtifact {
                                kind: RepresentationKind::Tensor,
                                strategy_name: "engram-shard".to_owned(),
                                frame: Some(frame),
                                dependencies: Vec::new(),
                                tensor: Some(TensorRepresentation {
                                    tensor_mode: 20,
                                    dtype,
                                    shape: shape.to_vec(),
                                    logical_root: root,
                                    layout: layout.to_value(dtype, shape)?,
                                }),
                                tile_shape: None,
                            }),
                        },
                    );
                }
                Err(err) => out.push(skipped(TensorStrategy::EngramShard, err.to_string())),
            },
            None => out.push(skipped(
                TensorStrategy::EngramShard,
                "no engram metadata supplied".to_owned(),
            )),
        }

        // Mode 17 float-tiled. The same opt-in as mode 15 (an explicit tile
        // shape or `auto_tile_bytes`); it is the independently decodable form
        // of the mode-9 plane split, so it is offered only for the dtypes that
        // have both a mode-9 layout and a logical-tensor identity.
        match tiled.clone() {
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
                            let geometry = TileGeometry::new(shape, &tile_shape, dtype);
                            let estimate = options.estimate_ns_at_pattern_for(
                                crate::cost_model::CostClass::TensorTiled,
                                TensorStrategy::FloatTiled,
                                size,
                                geometry.as_ref(),
                            );
                            retain_candidate(
                                &mut out,
                                hint,
                                keep_artifacts,
                                PreparedCandidate {
                                candidate: TensorPlanCandidate {
                                    strategy: TensorStrategy::FloatTiled,
                                    applicable: true,
                                    detail: format!(
                                        "one mode-17 float-tiled frame tiled {tile_shape:?} ({origin})"
                                    ),
                                    physical_bytes: Some(physical),
                                    estimated_decode_ns: Some(estimate),
                                    estimated_encode_ns: Some(options.estimate_encode_ns(TensorStrategy::FloatTiled, size)),
                                    cost: Some(options.price_candidate(physical, estimate, options.estimate_encode_ns(TensorStrategy::FloatTiled, size))),
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

        // Mode 18 multi-stream float-tiled. Same tile opt-in as 15/17; the
        // stream count defaults to 4. It is offered alongside mode 17 so the
        // bytes-only default keeps choosing the strictly smaller
        // representation (mode 18 pays one 256-byte table per exponent
        // stream), while a latency-priced plan can choose it.
        //
        // EXPERIMENTAL GATE: mode 18 lost every measured axis, so it is never
        // auto-selected. It is prepared (and therefore selectable) only when
        // the caller opted in with `allow_experimental_modes` or asked for it
        // by name. The wire format is frozen and unaffected.
        let streams_tiled = if allow_experimental {
            tiled
        } else {
            out.push(skipped(
                TensorStrategy::FloatTiledStreams,
                "experimental: mode 18 is not auto-selected; pass --allow-experimental-modes or name the strategy".to_owned(),
            ));
            None
        };
        match streams_tiled {
            Some((tile_shape, origin)) => match float_tensor_dtype(dtype) {
                Some(float_dtype)
                    if matches!(
                        float_dtype,
                        weftcodec::modes::FloatDtype::Bf16
                            | weftcodec::modes::FloatDtype::F16
                            | weftcodec::modes::FloatDtype::F32
                    ) =>
                {
                    let n_streams = options
                        .n_streams
                        .unwrap_or(weftcodec::FLOAT_TILED_STREAMS_DEFAULT);
                    match self.prepare_mode18(
                        dtype,
                        shape,
                        logical,
                        &tile_shape,
                        n_streams,
                        &limits,
                    ) {
                        Ok((frame, root)) => {
                            let physical = frame.len() as u64;
                            let geometry = TileGeometry::new(shape, &tile_shape, dtype);
                            let estimate = options.estimate_ns_at_pattern_for(
                                crate::cost_model::CostClass::TensorTiled,
                                TensorStrategy::FloatTiledStreams,
                                size,
                                geometry.as_ref(),
                            );
                            retain_candidate(
                                &mut out,
                                hint,
                                keep_artifacts,
                                PreparedCandidate {
                                candidate: TensorPlanCandidate {
                                    strategy: TensorStrategy::FloatTiledStreams,
                                    applicable: true,
                                    detail: format!(
                                        "one mode-18 float-tiled frame tiled {tile_shape:?} ({origin}) with {n_streams} streams"
                                    ),
                                    physical_bytes: Some(physical),
                                    estimated_decode_ns: Some(estimate),
                                    estimated_encode_ns: Some(options.estimate_encode_ns(TensorStrategy::FloatTiledStreams, size)),
                                    cost: Some(options.price_candidate(physical, estimate, options.estimate_encode_ns(TensorStrategy::FloatTiledStreams, size))),
                                    dependency: None,
                                },
                                artifact: Some(PreparedArtifact {
                                    kind: RepresentationKind::Tensor,
                                    strategy_name: "float-tiled-streams".to_owned(),
                                    frame: Some(frame),
                                    dependencies: Vec::new(),
                                    tensor: Some(TensorRepresentation {
                                        tensor_mode: 18,
                                        dtype,
                                        shape: shape.to_vec(),
                                        logical_root: root,
                                        layout: layout.to_value(dtype, shape)?,
                                    }),
                                    tile_shape: Some(tile_shape),
                                }),
                            });
                        }
                        Err(err) => {
                            out.push(skipped(TensorStrategy::FloatTiledStreams, err.to_string()))
                        }
                    }
                }
                Some(float_dtype) => out.push(skipped(
                    TensorStrategy::FloatTiledStreams,
                    format!(
                        "{} has no logical-tensor identity layout for mode 18",
                        float_dtype.as_str()
                    ),
                )),
                None => out.push(skipped(
                    TensorStrategy::FloatTiledStreams,
                    format!("dtype {} has no mode-9 float layout", dtype.as_str()),
                )),
            },
            None => {
                if allow_experimental && options.auto_tile_bytes.is_none() {
                    out.push(skipped(
                        TensorStrategy::FloatTiledStreams,
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
        } else if options.max_delta_bytes != 0 && size > options.max_delta_bytes {
            out.push(skipped(
                TensorStrategy::WeightDelta,
                format!(
                    "delta candidates are bounded to {} bytes; tensor is {size} bytes",
                    options.max_delta_bytes
                ),
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
                    let estimate = options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::WeightDelta,
                        TensorStrategy::WeightDelta,
                        size,
                    );
                    retain_candidate(
                        &mut out,
                        hint,
                        keep_artifacts,
                        PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::WeightDelta,
                                applicable: true,
                                detail: "mode 11 against the causal parent".to_owned(),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                estimated_encode_ns: Some(
                                    options.estimate_encode_ns(TensorStrategy::WeightDelta, size),
                                ),
                                cost: Some(options.price_candidate(
                                    physical,
                                    estimate,
                                    options.estimate_encode_ns(TensorStrategy::WeightDelta, size),
                                )),
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
                        },
                    );
                }
                Ok(None) => out.push(skipped(
                    TensorStrategy::WeightDelta,
                    "no usable causal parent with the same dtype/shape".to_owned(),
                )),
                Err(err) => out.push(skipped(TensorStrategy::WeightDelta, err.to_string())),
            }
        }

        // ZSTD patch against the causal parent: a mode-4 DICTIONARY frame
        // whose raw-content dictionary is the whole base and whose window
        // covers it (`zstd --patch-from` semantics). This is the candidate for
        // a checkpoint that changes in one region but is dense elsewhere,
        // where the COPY/INSERT delta (mode 11/16) loses.
        if independent_only {
            out.push(skipped(
                TensorStrategy::ZstdPatch,
                "rebase requires an independent representation".to_owned(),
            ));
        } else if options.write_policy.is_fast() {
            out.push(skipped(
                TensorStrategy::ZstdPatch,
                "the fast write policy never offers a patch candidate".to_owned(),
            ));
        } else if !options.zstd_patch {
            out.push(skipped(
                TensorStrategy::ZstdPatch,
                "zstd patch disabled (--no-zstd-patch)".to_owned(),
            ));
        } else if options.max_delta_bytes != 0 && size > options.max_delta_bytes {
            out.push(skipped(
                TensorStrategy::ZstdPatch,
                format!(
                    "delta candidates are bounded to {} bytes; tensor is {size} bytes",
                    options.max_delta_bytes
                ),
            ));
        } else if size < options.zstd_patch_min_bytes {
            out.push(skipped(
                TensorStrategy::ZstdPatch,
                format!(
                    "zstd patch skipped: {size} bytes is below the {} byte threshold",
                    options.zstd_patch_min_bytes
                ),
            ));
        } else {
            let encode_ns = options.estimate_encode_ns(TensorStrategy::ZstdPatch, size);
            if encode_ns > options.zstd_patch_cpu_budget_ns {
                out.push(skipped(
                    TensorStrategy::ZstdPatch,
                    format!(
                        "zstd patch skipped: estimated encode {encode_ns} ns exceeds the {} ns CPU budget",
                        options.zstd_patch_cpu_budget_ns
                    ),
                ));
            } else {
                match self.prepare_patch(dtype, shape, logical, causal, &limits, options) {
                    Ok(Some((frame, dependency))) => {
                        let physical = frame.len() as u64;
                        let estimate = options.estimate_ns_at_pattern(
                            crate::cost_model::CostClass::WeightDelta,
                            TensorStrategy::ZstdPatch,
                            size,
                        );
                        retain_candidate(
                            &mut out,
                            hint,
                            keep_artifacts,
                            PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::ZstdPatch,
                                applicable: true,
                                detail: format!(
                                    "one zstd level-{} patch frame against the causal parent (window covers the base)",
                                    options.zstd_patch_level
                                ),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                estimated_encode_ns: Some(encode_ns),
                                cost: Some(options.price_candidate(physical, estimate, encode_ns)),
                                dependency: Some(dependency),
                            },
                            artifact: Some(PreparedArtifact {
                                kind: RepresentationKind::Codec,
                                strategy_name: "zstd-patch".to_owned(),
                                frame: Some(frame),
                                dependencies: vec![dependency],
                                tensor: None,
                                tile_shape: None,
                            }),
                        });
                    }
                    Ok(None) => out.push(skipped(
                        TensorStrategy::ZstdPatch,
                        "no usable causal base with the same dtype/shape".to_owned(),
                    )),
                    Err(err) => out.push(skipped(TensorStrategy::ZstdPatch, err.to_string())),
                }
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
        } else if options.max_delta_bytes != 0 && size > options.max_delta_bytes {
            out.push(skipped(
                TensorStrategy::SparseDelta,
                format!(
                    "delta candidates are bounded to {} bytes; tensor is {size} bytes",
                    options.max_delta_bytes
                ),
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
                    let estimate = options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::SparseDelta,
                        TensorStrategy::SparseDelta,
                        size,
                    );
                    retain_candidate(
                        &mut out,
                        hint,
                        keep_artifacts,
                        PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::SparseDelta,
                                applicable: true,
                                detail: "mode 16 against the causal parent".to_owned(),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                estimated_encode_ns: Some(
                                    options.estimate_encode_ns(TensorStrategy::SparseDelta, size),
                                ),
                                cost: Some(options.price_candidate(
                                    physical,
                                    estimate,
                                    options.estimate_encode_ns(TensorStrategy::SparseDelta, size),
                                )),
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
                        },
                    );
                }
                Ok(None) => out.push(skipped(
                    TensorStrategy::SparseDelta,
                    "no usable causal parent with the same dtype/shape".to_owned(),
                )),
                Err(err) => out.push(skipped(TensorStrategy::SparseDelta, err.to_string())),
            }
        }

        // Mode 14 semantic-delta against a same-semantic-root candidate. The
        // search may decode `max_candidates` live tensors, so it is bounded:
        // a tensor large enough to exceed the byte bound falls back to the
        // other candidates rather than risking an unbounded allocation.
        if independent_only {
            out.push(skipped(
                TensorStrategy::SemanticDelta,
                "rebase requires an independent representation".to_owned(),
            ));
        } else if !options.semantic_search {
            out.push(skipped(
                TensorStrategy::SemanticDelta,
                "semantic search disabled by the write policy".to_owned(),
            ));
        } else if options.candidates.is_empty() && !options.semantic_scan {
            out.push(skipped(
                TensorStrategy::SemanticDelta,
                "no candidate list and the implicit store scan is off".to_owned(),
            ));
        } else if size.saturating_mul(options.max_candidates as u64) > options.max_semantic_bytes {
            out.push(skipped(
                TensorStrategy::SemanticDelta,
                format!(
                    "semantic search would touch at most {} bytes (bound {}); skipping",
                    size.saturating_mul(options.max_candidates as u64),
                    options.max_semantic_bytes
                ),
            ));
        } else {
            match crate::phase_profile::timed(crate::phase_profile::Phase::EncodeSemantic, || {
                self.prepare_semantic(dtype, shape, logical, options, &limits)
            }) {
                Ok(Some((frame, dependency, policy))) => {
                    let physical = frame.len() as u64;
                    let estimate = options.estimate_ns_at_pattern(
                        crate::cost_model::CostClass::SemanticDelta,
                        TensorStrategy::SemanticDelta,
                        size,
                    );
                    retain_candidate(
                        &mut out,
                        hint,
                        keep_artifacts,
                        PreparedCandidate {
                            candidate: TensorPlanCandidate {
                                strategy: TensorStrategy::SemanticDelta,
                                applicable: true,
                                detail: format!(
                                    "mode 14 against a same-semantic-root base (policy {policy})"
                                ),
                                physical_bytes: Some(physical),
                                estimated_decode_ns: Some(estimate),
                                estimated_encode_ns: Some(
                                    options.estimate_encode_ns(TensorStrategy::SemanticDelta, size),
                                ),
                                cost: Some(options.price_candidate(
                                    physical,
                                    estimate,
                                    options.estimate_encode_ns(TensorStrategy::SemanticDelta, size),
                                )),
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
                        },
                    );
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

    fn prepare_mode18(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        tile_shape: &[u64],
        n_streams: u64,
        limits: &Limits,
    ) -> Result<(Vec<u8>, Hash32)> {
        let float_dtype = float_tensor_dtype(dtype).ok_or_else(|| {
            Error::invalid_argument(format!(
                "dtype `{}` has no float layout for mode 18",
                dtype.as_str()
            ))
        })?;
        let frame = weftcodec::encode_float_tiled_streams(
            logical,
            float_dtype,
            shape,
            tile_shape,
            n_streams,
            limits,
        )?;
        let parsed = Frame::parse(&frame, limits)?;
        let meta = weftcodec::float_tiled_streams_metadata(&parsed, limits)?;
        if meta.shape != shape || meta.n_streams != n_streams {
            return Err(Error::corrupt(
                "mode-18 shape or stream count disagrees with the write",
            ));
        }
        Ok((frame, meta.logical_root))
    }

    fn prepare_mode20(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        spec: &EngramWriteSpec,
        limits: &Limits,
    ) -> Result<(Vec<u8>, Hash32)> {
        let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
            Error::invalid_argument(format!(
                "dtype `{}` has no element layout for mode 20",
                dtype.as_str()
            ))
        })?;
        let precision_name = spec.precision.as_deref().unwrap_or(physical.as_str());
        let precision_dtype = weftcodec::TensorDtype::from_name(precision_name).map_err(|_| {
            Error::invalid_argument(format!(
                "engram precision `{precision_name}` is not a tensor dtype"
            ))
        })?;
        let quantization =
            spec.quantization
                .as_ref()
                .map(|scheme| weftcodec::engram::EngramQuantization {
                    scheme: scheme.clone(),
                    lossless: true,
                });
        let vocab_size = if spec.vocab_size == 0 {
            shape[0]
        } else {
            spec.vocab_size
        };
        let input = weftcodec::engram::EngramInput {
            hash_function: spec.hash_function.clone(),
            vocab_size,
            table_shape: shape.to_vec(),
            dtype: physical,
            precision: weftcodec::engram::EngramPrecision {
                storage_dtype: precision_dtype,
                quantization,
            },
            row_group_edges: spec.row_group_edges.clone(),
            references: std::collections::BTreeMap::new(),
            supersedes: Vec::new(),
        };
        let frame = weftcodec::engram::encode_engram(logical, &input, limits)?;
        let parsed = Frame::parse(&frame, limits)?;
        let meta = weftcodec::engram::engram_metadata(&parsed, limits)?;
        if meta.table_shape != shape || meta.dtype != physical {
            return Err(Error::corrupt(
                "mode-20 shape or dtype disagrees with the write",
            ));
        }
        let root = weftcodec::tensor::logical_root(physical, shape, logical, limits)?;
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

    /// Encode the whole-tensor patch candidate against the causal base. The
    /// base must have the same dtype and shape (checked by
    /// `tensor_causal_base`), and the resulting chain must respect the frozen
    /// dependency depth bound.
    fn prepare_patch(
        &self,
        dtype: TensorElementDtype,
        shape: &[u64],
        logical: &[u8],
        causal: &[Hash32],
        limits: &Limits,
        options: &TensorPlanOptions,
    ) -> Result<Option<(Vec<u8>, Hash32)>> {
        let Some((base_root, base_bytes)) = self.tensor_causal_base(dtype, shape, causal)? else {
            return Ok(None);
        };
        let base_depth = self.representation_dep_depth(base_root, &mut Vec::new())?;
        if base_depth + 1 > MAX_CODEC_DEPENDENCY_DEPTH {
            return Ok(None);
        }
        let frame =
            weftcodec::modes::encode_patch(logical, &base_bytes, options.zstd_patch_level, limits)?;
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
            self.read_manifest_keyed(manifest_id)?;
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
        if options.candidates.is_empty() {
            // No explicit candidate list. The implicit whole-store scan is
            // opt-in: it is O(objects) per tensor and reads whole tensors.
            if !options.semantic_scan {
                return Ok(out);
            }
        }
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
        // A bounded, deterministic prefix: a truncated scan is a subset, never
        // a race, and the caller opted into an implicit search.
        ids.truncate(options.semantic_scan_max_objects.max(1));
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
        let content_hash = crate::phase_profile::timed(crate::phase_profile::Phase::Digest, || {
            identity::content_hash(logical)
        });
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
                        dependencies: artifact.dependencies.clone(),
                        strategy: artifact.strategy_name.clone(),
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
        let frame_bytes = self.read_blob_verified_keyed(BlobKind::Manifest, frame_id, identity::MANIFEST_DOMAIN)?;
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
            18 => Mode::FloatTiledStreams,
            20 => Mode::EngramShard,
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
            18 => weftcodec::decode_float_tiled_streams(&frame, &limits, 1)?,
            20 => {
                let resolver = self.engram_resolver(rep)?;
                weftcodec::engram::decode_engram(&frame, &resolver, &limits)?
            }
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

    /// Whole-object decode for a mode-18 multi-stream float-tiled object.
    ///
    /// Tiles decode on the bounded pool; each tile's exponent streams use the
    /// codec's wide-word multi-stream kernel inside that worker. Every tile
    /// root, the assembled logical root and the whole-content hash are checked.
    fn decode_mode18_with_options(
        &self,
        frame: &Frame<'_>,
        tensor: &TensorRepresentation,
        manifest: &Manifest,
        rep: &Representation,
        options: &TileDecodeOptions,
    ) -> Result<(Vec<u8>, TileDecodeStats)> {
        let limits = Limits::default();
        let descriptor = weftcodec::parse_float_tiled_streams(frame, &limits)?;
        let meta = &descriptor.meta;
        if !float_dtype_matches_tensor(meta.dtype, tensor.dtype) {
            return Err(Error::corrupt(format!(
                "mode-18 dtype `{}` disagrees with manifest dtype `{}`",
                meta.dtype.as_str(),
                tensor.dtype.as_str()
            )));
        }
        if meta.shape != tensor.shape {
            return Err(Error::corrupt(
                "mode-18 shape disagrees with the representation manifest",
            ));
        }
        if meta.logical_root != tensor.logical_root {
            return Err(Error::corrupt(
                "mode-18 logical_root disagrees with the manifest logical_root",
            ));
        }
        let total_u64 = meta
            .element_count
            .checked_mul(meta.dtype.element_size())
            .ok_or_else(|| Error::limit_exceeded("float tiled decoded bytes overflow u64"))?;
        if total_u64 != manifest.size || total_u64 != rep.decoded_size {
            return Err(Error::corrupt(
                "mode-18 decoded length disagrees with the manifest",
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
                weftcodec::decode_float_tile_streams(
                    meta.dtype,
                    entry,
                    meta.n_streams,
                    stored,
                    &limits,
                    1,
                )
                .map_err(Error::from)
            },
            |tile| tile.len(),
        )?;
        let mut tiles_bytes = 0usize;
        for (entry, tile) in meta.tiles.iter().zip(tiles.iter()) {
            tiles_bytes = tiles_bytes.checked_add(tile.len()).ok_or_else(|| {
                Error::limit_exceeded("float tiled decoded length overflows usize")
            })?;
            weftcodec::place_float_tiled_streams_tile(&mut out, meta, entry, tile)?;
        }
        if tiles_bytes != total {
            return Err(Error::corrupt(format!(
                "float tiled decoded to {tiles_bytes} bytes but the descriptor requires {total}"
            )));
        }
        let tensor_dtype = tensor
            .dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("mode-18 dtype has no element layout"))?;
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
            self.read_manifest_keyed(dep_root)?;
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
            estimated_encode_ns: None,
            cost: None,
            dependency: None,
        },
        artifact: None,
    }
}

/// Append one planner candidate, retaining at most the encoded frames that can
/// still be chosen.
///
/// The planner has to build (and price) every candidate, but a caller only ever
/// uses the chosen one. Holding every frame at once peaked at about six
/// tensor-sized allocations per tensor; this keeps at most two:
///
/// * `hint = Some(strategy)` — `choose_candidate` returns the first applicable
///   candidate with that strategy, so only that candidate keeps its artifact.
///   A later candidate with the same strategy (zstd-3 and zstd-19 both map to
///   `TensorStrategy::Zstd`) is dropped.
/// * `hint = None` — only the running minimum-`cost` candidate keeps its
///   artifact. A strictly cheaper arrival clears every earlier artifact before
///   being retained (peak: the previous best plus the new frame); ties keep the
///   earlier candidate, exactly like `choose_candidate`.
///
/// Non-applicable candidates and `keep_artifacts == false` (pricing-only) keep
/// no frame. Prices, candidate order, tie-breaks and the selected strategy are
/// unchanged: this only decides which frames stay allocated.
fn retain_candidate(
    out: &mut Vec<PreparedCandidate>,
    hint: Option<TensorStrategy>,
    keep_artifacts: bool,
    mut item: PreparedCandidate,
) {
    if std::env::var_os("WEFT_TRACE_CANDIDATES").is_some() {
        let rss = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("VmHWM:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .unwrap_or(0);
        eprintln!(
            "tc: strategy={} applicable={} physical={:?} keep={keep_artifacts} rss_hwm_kib={rss}",
            item.candidate.strategy.as_str(),
            item.candidate.applicable,
            item.candidate.physical_bytes,
        );
    }
    if !keep_artifacts || !item.candidate.applicable {
        item.artifact = None;
        out.push(item);
        return;
    }
    match hint {
        Some(strategy) => {
            let already_kept = out.iter().any(|existing| {
                existing.candidate.applicable && existing.candidate.strategy == strategy
            });
            if item.candidate.strategy != strategy || already_kept {
                item.artifact = None;
            }
            out.push(item);
        }
        None => {
            let retained_best = out
                .iter()
                .filter(|existing| existing.candidate.applicable)
                .filter_map(|existing| existing.candidate.cost)
                .min();
            let cheaper = match (item.candidate.cost, retained_best) {
                (Some(cost), Some(best)) => cost < best,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if cheaper {
                for existing in out.iter_mut() {
                    existing.artifact = None;
                }
            } else {
                item.artifact = None;
            }
            out.push(item);
        }
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
