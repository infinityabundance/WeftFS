//! Checkpoint object model and topology-independent restore.
//!
//! A checkpoint is a store directory holding one `weft.checkpoint.v1` manifest
//! and one shard object per named tensor slice. Each shard is written through
//! the tensor store as a mode-10 `TENSOR_SHARDED` object whose payload is the
//! shard's exact element bytes (a `q8_0` word transport: the quantized words
//! are the payload bytes, so the round trip is bit-exact). The checkpoint
//! manifest records the *logical* dtype, shape and `LogicalTensorRoot` of each
//! tensor plus the storage topology `{axis, shards, index, object_id,
//! content_root}` of every shard.
//!
//! Restore treats the storage topology as an implementation detail: it reads
//! the shard objects, verifies every shard's own logical root and the
//! reassembled tensor's logical root, and can re-express the same bytes under a
//! different topology or layout before writing the container. The logical root
//! is therefore identical for every topology, by construction.
//!
//! Shard objects are mode-10 frames, which are not independently seekable: a
//! region restore reads only the shard objects the region intersects, but
//! decodes each intersecting shard's whole frame. The touched-shard count is
//! reported and asserted by the tests; it is not a byte-range claim inside a
//! shard.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use weftcodec::tensor::{self as tc, PhysicalTensor, Region};
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::representation::TensorElementDtype;
use crate::safetensors::{self, SafetensorsFile, SafetensorsTensor};
use crate::state::{ObjectKind, State};
use crate::store::Store;
use crate::tensor_store::{
    FastCodec, TensorLayoutSpec, TensorStrategy, TensorWriteSpec, WritePolicy,
};
use crate::wcb_util as wu;

/// Canonical checkpoint manifest schema.
pub const CHECKPOINT_SCHEMA: &str = "weft.checkpoint.v1";
/// Store directory that holds every checkpoint.
pub const CHECKPOINT_ROOT: &str = "checkpoints";
/// Name of the manifest file inside a checkpoint directory.
pub const CHECKPOINT_MANIFEST_NAME: &str = "manifest";
/// Domain for the checkpoint manifest identity.
pub const CHECKPOINT_MANIFEST_DOMAIN: &str = "weft/checkpoint/manifest/v1";
/// Container name recorded in the manifest.
pub const CHECKPOINT_CONTAINER: &str = "safetensors";

/// SHA-256 digest (container digests are SHA-256, store identities are BLAKE3).
pub type Sha256Digest = [u8; 32];

/// Storage strategy for one shard object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShardStrategy {
    /// Mode 10 `TENSOR_SHARDED` with a `q8_0` byte-word transport.
    TensorSharded,
    /// Mode 15 `TENSOR_TILED` over the shard's logical dtype.
    TensorTiled,
    /// Mode 17 `FLOAT_TILED`: per-tile mode-9 plane split.
    FloatTiled,
    /// Let the tensor planner choose per shard (raw, zstd, mode 9/15/17/18,
    /// or a weight/sparse delta when a causal checkpoint is supplied).
    Planned,
    /// One ZSTD frame per shard (exact fallback).
    Zstd,
    /// Raw chunks per shard (exact fallback).
    Raw,
}

impl ShardStrategy {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "tensor-sharded" => Ok(ShardStrategy::TensorSharded),
            "tensor-tiled" => Ok(ShardStrategy::TensorTiled),
            "float-tiled" => Ok(ShardStrategy::FloatTiled),
            "planned" => Ok(ShardStrategy::Planned),
            "zstd" => Ok(ShardStrategy::Zstd),
            "raw" => Ok(ShardStrategy::Raw),
            other => Err(Error::invalid_argument(format!(
                "--shard-strategy must be `tensor-sharded`, `tensor-tiled`, `float-tiled`, `planned`, `zstd`, or `raw` (got `{other}`)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ShardStrategy::TensorSharded => "tensor-sharded",
            ShardStrategy::TensorTiled => "tensor-tiled",
            ShardStrategy::FloatTiled => "float-tiled",
            ShardStrategy::Planned => "planned",
            ShardStrategy::Zstd => "zstd",
            ShardStrategy::Raw => "raw",
        }
    }
}

/// Options for one checkpoint write.
#[derive(Clone, Debug)]
pub struct CheckpointWriteOptions {
    pub shards: u32,
    pub axis: u32,
    /// Glob patterns matched against tensor names; empty selects every tensor.
    pub filters: Vec<String>,
    pub strategy: ShardStrategy,
    /// Tile extents for `ShardStrategy::TensorTiled` (mode 15).
    pub tile_shape: Option<Vec<u64>>,
    /// Critical-path write policy. `Fast` stores every shard with the fast
    /// codec below and never runs a planned candidate encode; `Planned` is the
    /// historical shard-strategy behaviour; `Auto` resolves against
    /// `on_critical_path` / `deadline_ms`.
    pub write_policy: WritePolicy,
    /// Representation a `Fast` write builds for each shard.
    pub fast_codec: FastCodec,
    /// ZSTD level used by `FastCodec::Zstd` (clamped to 1..=3).
    pub fast_zstd_level: i32,
    /// Caller declaration for `WritePolicy::Auto`: this save is on the training
    /// critical path.
    pub on_critical_path: bool,
    /// Caller declaration for `WritePolicy::Auto`: the caller has this many
    /// milliseconds before it needs the save to return.
    pub deadline_ms: Option<u64>,
    /// Optional previous checkpoint in the same store. Tensors with the same
    /// name, dtype and shape offer that checkpoint's matching shard version as
    /// a causal parent, so the planner can price a weight-delta against it.
    pub causal_checkpoint: Option<String>,
    /// Offer the mode-14 semantic-delta candidate (see
    /// `TensorPlanOptions::semantic_search`).
    pub semantic_search: bool,
    /// Deterministic ObjectId seed for the checkpoint directory, the manifest
    /// file, and every shard object. `None` (the default) uses OS randomness,
    /// exactly as before. Setting it makes two writes of the same input publish
    /// byte-identical state, which is how the sequential and parallel
    /// (prepare/commit) paths are proven equivalent.
    pub deterministic_seed: Option<[u8; 32]>,
    /// Allow experimental representations (mode 18) to be auto-selected by the
    /// per-shard planner. Default false.
    pub allow_experimental_modes: bool,
    /// Per-tensor streaming ingest bound. The writer streams the source tensor
    /// in fixed windows and reads one shard at a time, so no whole-tensor
    /// buffer is allocated; a tensor (or one of its shards) larger than this is
    /// refused before anything is published. Default 1 GiB.
    pub max_ingest_bytes: u64,
}

impl Default for CheckpointWriteOptions {
    fn default() -> Self {
        CheckpointWriteOptions {
            shards: 1,
            axis: 0,
            filters: Vec::new(),
            strategy: ShardStrategy::TensorSharded,
            tile_shape: None,
            write_policy: WritePolicy::Planned,
            fast_codec: FastCodec::Zstd,
            fast_zstd_level: 1,
            on_critical_path: false,
            deadline_ms: None,
            causal_checkpoint: None,
            semantic_search: true,
            deterministic_seed: None,
            allow_experimental_modes: false,
            max_ingest_bytes: 1024 * 1024 * 1024,
        }
    }
}

impl CheckpointWriteOptions {
    /// The policy after resolving `Auto`.
    pub fn resolved_policy(&self) -> WritePolicy {
        match self.write_policy {
            WritePolicy::Auto { .. } => WritePolicy::Auto {
                on_critical_path: self.on_critical_path,
                deadline_ms: self.deadline_ms,
            },
            other => other,
        }
    }

    /// True when this save takes the fast path.
    pub fn is_fast(&self) -> bool {
        self.resolved_policy().is_fast()
    }

    /// Strategy actually used for a shard under the resolved policy.
    pub fn effective_strategy(&self) -> ShardStrategy {
        if self.is_fast() {
            match self.fast_codec {
                FastCodec::Raw => ShardStrategy::Raw,
                FastCodec::Zstd => ShardStrategy::Zstd,
            }
        } else {
            self.strategy
        }
    }
}

/// Hard bound on worker ranks for one prepare/commit checkpoint write.
pub const MAX_PARALLEL_RANKS: usize = 64;
/// Default bound on bytes reserved by in-flight prepare tasks.
pub const DEFAULT_MAX_IN_FLIGHT_BYTES: u64 = 1 << 30;
/// Default cumulative byte budget for one rank.
pub const DEFAULT_PER_RANK_BYTES: u64 = 1 << 29;

/// Bounds for [`Store::write_checkpoint_parallel`].
///
/// The split is deliberately small: N worker handles publish immutable blobs,
/// one committer publishes the single commit. There is no consensus protocol,
/// no second writer, and no change to the writer lock, the lease, the fence,
/// the superblock, or the commit file format.
#[derive(Clone, Debug)]
pub struct ParallelCheckpointOptions {
    /// Worker ranks. `1` still exercises the prepare/commit split.
    pub ranks: usize,
    /// Total bytes that may be held in flight by preparing workers.
    pub max_in_flight_bytes: u64,
    /// Cumulative decoded-bytes budget for one rank.
    pub per_rank_bytes: u64,
    /// Wall-clock deadline for the prepare phase. A rank that would start work
    /// past it fails with `Cancelled`; nothing is published.
    pub deadline_ms: Option<u64>,
    /// Re-read and re-hash every prepared shard on the committer (default on).
    /// Turning it off is a deliberate performance choice, never a correctness
    /// claim about untrusted workers.
    pub verify_fragments: bool,
}

impl Default for ParallelCheckpointOptions {
    fn default() -> Self {
        ParallelCheckpointOptions {
            ranks: 1,
            max_in_flight_bytes: DEFAULT_MAX_IN_FLIGHT_BYTES,
            per_rank_bytes: DEFAULT_PER_RANK_BYTES,
            deadline_ms: None,
            verify_fragments: true,
        }
    }
}

impl ParallelCheckpointOptions {
    fn validate(&self) -> Result<()> {
        if self.ranks == 0 {
            return Err(Error::invalid_argument("--ranks must be at least 1"));
        }
        if self.ranks > MAX_PARALLEL_RANKS {
            return Err(Error::limit_exceeded(format!(
                "--ranks {} exceeds the {MAX_PARALLEL_RANKS}-rank bound",
                self.ranks
            )));
        }
        if self.max_in_flight_bytes == 0 || self.per_rank_bytes == 0 {
            return Err(Error::invalid_argument(
                "the in-flight and per-rank byte bounds must be nonzero",
            ));
        }
        Ok(())
    }
}

/// Bounded in-flight byte accounting between prepare tasks.
struct ParallelBudget {
    limit: u64,
    state: Mutex<u64>,
    condvar: Condvar,
}

impl ParallelBudget {
    fn new(limit: u64) -> ParallelBudget {
        ParallelBudget {
            limit,
            state: Mutex::new(0),
            condvar: Condvar::new(),
        }
    }

    /// Reserve `need` bytes, or less when one task alone already exceeds the
    /// bound (an oversized task always runs, alone, exactly like
    /// `parallel::read_tensors_bounded`).
    fn reserve(&self, need: u64, deadline: Option<Instant>) -> Result<u64> {
        let want = need.min(self.limit);
        let mut held = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *held != 0 && *held + want > self.limit {
            match deadline {
                Some(limit) => {
                    let now = Instant::now();
                    if now >= limit {
                        return Err(Error::cancelled(
                            "checkpoint prepare deadline reached while waiting for an in-flight byte slot",
                        ));
                    }
                    let (guard, _) = self
                        .condvar
                        .wait_timeout(held, limit - now)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    held = guard;
                }
                None => {
                    held = self
                        .condvar
                        .wait(held)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
        }
        *held += want;
        Ok(want)
    }

    fn release(&self, reserved: u64) {
        let mut held = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *held = held.saturating_sub(reserved);
        drop(held);
        self.condvar.notify_all();
    }
}

/// One tensor prepared by a worker and awaiting the committer.
#[derive(Clone, Debug)]
struct PreparedCheckpointTensor {
    tensor_index: usize,
    tensor: CheckpointTensor,
    object_names: Vec<String>,
    prepared: Vec<crate::tensor_store::PreparedTensorObject>,
}

/// One stored shard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointShard {
    pub axis: u32,
    pub shards: u32,
    pub index: u32,
    /// Directory entry name inside the checkpoint directory.
    pub object_name: String,
    pub object_id: Hash32,
    /// `H("weft/chunk/v1", shard_bytes)`, the store's content identity.
    pub content_root: Hash32,
    /// `LogicalTensorRoot` of the shard sub-tensor under the tensor's own dtype.
    pub logical_root: Hash32,
    pub byte_len: u64,
}

/// One logical tensor in a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    /// `LogicalTensorRoot` of the whole tensor under its logical dtype.
    pub logical_root: Hash32,
    /// `H("weft/chunk/v1", whole canonical row-major bytes)`.
    pub content_root: Hash32,
    pub element_bytes: u64,
    /// `data_offsets[0]` in the source container.
    pub source_offset: u64,
    pub shards: Vec<CheckpointShard>,
}

/// A `weft.checkpoint.v1` manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointManifest {
    pub name: String,
    pub container: String,
    /// True when every tensor of the source container is present.
    pub complete: bool,
    pub element_bytes: u64,
    /// Exact `u64le(len) || JSON` header of the source container.
    pub source_header: Option<Vec<u8>>,
    pub source_file_bytes: Option<u64>,
    pub source_sha256: Option<Sha256Digest>,
    /// Sorted by name (canonical form).
    pub tensors: Vec<CheckpointTensor>,
}

/// One tensor's write report.
#[derive(Clone, Debug)]
pub struct WrittenTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    pub logical_root: Hash32,
    pub element_bytes: u64,
    pub axis: u32,
    pub shards: u32,
    pub object_names: Vec<String>,
    pub object_ids: Vec<Hash32>,
}

/// Result of a checkpoint write.
#[derive(Clone, Debug)]
pub struct CheckpointWriteReport {
    pub name: String,
    pub checkpoint_id: Hash32,
    pub head: Hash32,
    pub manifest_id: Hash32,
    pub manifest_root: Hash32,
    pub tensors: Vec<WrittenTensor>,
    pub total_element_bytes: u64,
    pub shard_objects: usize,
    pub complete: bool,
    pub source_sha256: Sha256Digest,
    pub source_file_bytes: u64,
    /// Wall time of the prepare phase (parallel path only).
    pub prepare_ms: Option<f64>,
    /// Wall time the committer spent re-reading and re-hashing fragments.
    pub validate_ms: Option<f64>,
    /// Wall time of the single commit (group open through publication).
    pub commit_ms: Option<f64>,
}

/// Physical arrangement a restore verifies the bytes against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreTopology {
    RowMajor,
    ColumnMajor,
    Blocked { block_shape: Vec<u64> },
    Sharded { axis: u32, shards: u32 },
}

/// Options for one restore.
#[derive(Clone, Debug, Default)]
pub struct CheckpointRestoreOptions {
    /// Restore exactly one tensor by name.
    pub tensor: Option<String>,
    /// Restore only this region (full-tensor coordinates). Requires `tensor`.
    pub region: Option<Region>,
    /// Re-express the assembled bytes in this layout and verify the root.
    /// Rejected together with `region` (a region has no full-tensor identity).
    pub topology: Option<RestoreTopology>,
}

/// One tensor's restore report.
#[derive(Clone, Debug)]
pub struct RestoredTensor {
    pub name: String,
    pub dtype: String,
    /// Shape of the restored bytes (`region.extent()` for a slice).
    pub shape: Vec<u64>,
    /// Full logical shape of the tensor.
    pub full_shape: Vec<u64>,
    pub full_logical_root: Hash32,
    pub restored_content_root: Hash32,
    pub bytes: u64,
    pub shards_read: usize,
    pub units_touched: u64,
    /// The whole-tensor root was recomputed and matched the manifest.
    pub full_root_verified: bool,
    /// Every read shard's own logical root was recomputed and matched.
    pub shard_roots_verified: bool,
    /// Root recomputed through the requested physical topology.
    pub topology_root: Option<Hash32>,
    pub sha256: Sha256Digest,
}

/// One tensor restored to memory (no container header).
///
/// This is the primitive the persistent checkpoint service serves: the bytes
/// are the exact decoded tensor payload and the report carries the same
/// verification fields the whole-container restore reports per tensor.
#[derive(Clone, Debug)]
pub struct RestoredTensorBytes {
    pub bytes: Vec<u8>,
    pub report: RestoredTensor,
}

/// Result of a restore.
#[derive(Clone, Debug)]
pub struct CheckpointRestoreReport {
    pub name: String,
    pub manifest_root: Hash32,
    pub file_sha256: Sha256Digest,
    pub file_bytes: u64,
    pub source_sha256: Option<Sha256Digest>,
    /// True only when the whole source file (header included) was reproduced.
    pub exact_source_file: bool,
    pub header_reproduced: bool,
    pub tensors: Vec<RestoredTensor>,
    pub shards_read: usize,
    pub units_touched: u64,
    /// Backend bytes read (manifest + frame blobs), measured by the backend.
    pub store_bytes_read: u64,
    pub store_reads: u64,
}

// ---------------------------------------------------------------------------
// Manifest encoding
// ---------------------------------------------------------------------------

fn u64_array(values: &[u64]) -> Value {
    Value::Array(values.iter().map(|dim| Value::U64(*dim)).collect())
}

fn shard_value(shard: &CheckpointShard) -> Value {
    wu::map([
        ("axis", Value::U64(u64::from(shard.axis))),
        ("shards", Value::U64(u64::from(shard.shards))),
        ("index", Value::U64(u64::from(shard.index))),
        ("object_name", Value::Str(shard.object_name.clone())),
        ("object_id", wu::hash_value(shard.object_id)),
        ("content_root", wu::hash_value(shard.content_root)),
        ("logical_root", wu::hash_value(shard.logical_root)),
        ("byte_len", Value::U64(shard.byte_len)),
    ])
}

fn tensor_value(tensor: &CheckpointTensor) -> Value {
    wu::map([
        ("name", Value::Str(tensor.name.clone())),
        ("dtype", Value::Str(tensor.dtype.clone())),
        ("shape", u64_array(&tensor.shape)),
        ("logical_root", wu::hash_value(tensor.logical_root)),
        ("content_root", wu::hash_value(tensor.content_root)),
        ("element_bytes", Value::U64(tensor.element_bytes)),
        ("source_offset", Value::U64(tensor.source_offset)),
        (
            "shards",
            Value::Array(tensor.shards.iter().map(shard_value).collect()),
        ),
    ])
}

impl CheckpointManifest {
    /// Canonical WCB value.
    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(CHECKPOINT_SCHEMA.to_owned())),
            ("name", Value::Str(self.name.clone())),
            ("container", Value::Str(self.container.clone())),
            ("complete", Value::Bool(self.complete)),
            ("element_bytes", Value::U64(self.element_bytes)),
            (
                "source_header",
                match &self.source_header {
                    Some(bytes) => Value::Bytes(bytes.clone()),
                    None => Value::Null,
                },
            ),
            (
                "source_file_bytes",
                match self.source_file_bytes {
                    Some(bytes) => Value::U64(bytes),
                    None => Value::Null,
                },
            ),
            (
                "source_sha256",
                match self.source_sha256 {
                    Some(digest) => Value::Bytes(digest.to_vec()),
                    None => Value::Null,
                },
            ),
            (
                "tensors",
                Value::Array(self.tensors.iter().map(tensor_value).collect()),
            ),
        ])
    }

    /// Canonical WCB bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    /// `H("weft/checkpoint/manifest/v1", canonical_bytes)`.
    pub fn root(&self) -> Result<Hash32> {
        Ok(identity::h(
            CHECKPOINT_MANIFEST_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }

    /// Strict decode: structure, every field, and canonical re-encoding.
    pub fn decode(bytes: &[u8]) -> Result<CheckpointManifest> {
        let value = wu::decode(bytes)?;
        let manifest = CheckpointManifest::from_value(&value)?;
        if manifest.canonical_bytes()? != bytes {
            return Err(Error::non_canonical(
                "checkpoint manifest bytes are not the canonical encoding of its fields",
            ));
        }
        Ok(manifest)
    }

    /// Parse and validate a manifest value.
    pub fn from_value(value: &Value) -> Result<CheckpointManifest> {
        let map = wu::as_map(value)?;
        wu::check_keys(
            map,
            &[
                "schema",
                "name",
                "container",
                "complete",
                "element_bytes",
                "source_header",
                "source_file_bytes",
                "source_sha256",
                "tensors",
            ],
        )?;
        let schema = wu::get_text(map, "schema")?;
        if schema != CHECKPOINT_SCHEMA {
            return Err(Error::unknown_schema(format!(
                "checkpoint schema `{schema}` is not `{CHECKPOINT_SCHEMA}`"
            )));
        }
        let name = wu::get_text(map, "name")?;
        if name.is_empty() {
            return Err(Error::malformed("checkpoint name must not be empty"));
        }
        let container = wu::get_text(map, "container")?;
        if container != CHECKPOINT_CONTAINER {
            return Err(Error::unsupported_version(format!(
                "checkpoint container `{container}` is not `{CHECKPOINT_CONTAINER}`"
            )));
        }
        let complete = match wu::field(map, "complete")? {
            Value::Bool(value) => *value,
            _ => return Err(Error::malformed("checkpoint `complete` must be a bool")),
        };
        let element_bytes = wu::get_u64(map, "element_bytes")?;
        let source_header = match wu::field(map, "source_header")? {
            Value::Null => None,
            Value::Bytes(bytes) => Some(bytes.clone()),
            _ => {
                return Err(Error::malformed(
                    "checkpoint `source_header` must be bytes or null",
                ))
            }
        };
        let source_file_bytes = match wu::field(map, "source_file_bytes")? {
            Value::Null => None,
            Value::U64(value) => Some(*value),
            _ => {
                return Err(Error::malformed(
                    "checkpoint `source_file_bytes` must be u64 or null",
                ))
            }
        };
        let source_sha256 = match wu::field(map, "source_sha256")? {
            Value::Null => None,
            Value::Bytes(bytes) => Some(digest32(bytes, "source_sha256")?),
            _ => {
                return Err(Error::malformed(
                    "checkpoint `source_sha256` must be bytes32 or null",
                ))
            }
        };
        if complete
            && (source_header.is_none() || source_file_bytes.is_none() || source_sha256.is_none())
        {
            return Err(Error::malformed(
                "a complete checkpoint must carry the source header, byte length, and SHA-256",
            ));
        }

        let tensors_value = wu::field(map, "tensors")?;
        let tensor_values = match tensors_value {
            Value::Array(items) => items,
            _ => return Err(Error::malformed("checkpoint `tensors` must be an array")),
        };
        let limits = Limits::default();
        if tensor_values.len() as u64 > limits.max_structured_items {
            return Err(Error::limit_exceeded(
                "checkpoint tensor count exceeds the structured-item bound",
            ));
        }
        let mut tensors = Vec::with_capacity(tensor_values.len());
        let mut names = BTreeSet::new();
        let mut total = 0u64;
        for item in tensor_values {
            let mut tensor = tensor_from_value(item, &limits)?;
            tensor.shards.sort_by_key(|shard| shard.index);
            if !names.insert(tensor.name.clone()) {
                return Err(Error::malformed(format!(
                    "checkpoint has duplicate tensor `{}`",
                    tensor.name
                )));
            }
            total = total
                .checked_add(tensor.element_bytes)
                .ok_or_else(|| Error::limit_exceeded("checkpoint element bytes overflow"))?;
            tensors.push(tensor);
        }
        if tensors.is_empty() {
            return Err(Error::malformed(
                "a checkpoint must name at least one tensor",
            ));
        }
        if total != element_bytes {
            return Err(Error::corrupt(format!(
                "checkpoint element_bytes {element_bytes} does not match the tensor sum {total}"
            )));
        }
        // Canonical array order: tensors sorted by name.
        tensors.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(CheckpointManifest {
            name,
            container,
            complete,
            element_bytes,
            source_header,
            source_file_bytes,
            source_sha256,
            tensors,
        })
    }

    /// Tensor by exact name.
    pub fn tensor(&self, name: &str) -> Result<&CheckpointTensor> {
        self.tensors
            .iter()
            .find(|tensor| tensor.name == name)
            .ok_or_else(|| {
                Error::not_found(format!(
                    "checkpoint `{}` has no tensor named `{name}`",
                    self.name
                ))
            })
    }
}

fn digest32(bytes: &[u8], field: &str) -> Result<[u8; 32]> {
    bytes.try_into().map_err(|_| {
        Error::malformed(format!(
            "`{field}` must be exactly 32 bytes (found {})",
            bytes.len()
        ))
    })
}

fn tensor_from_value(value: &Value, limits: &Limits) -> Result<CheckpointTensor> {
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
    let name = wu::get_text(map, "name")?;
    if name.is_empty() {
        return Err(Error::malformed("checkpoint tensor name must not be empty"));
    }
    let dtype = TensorElementDtype::parse(&wu::get_text(map, "dtype")?)?;
    let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
        Error::unsupported_version(format!(
            "checkpoint tensor dtype `{}` has no physical layout",
            dtype.as_str()
        ))
    })?;
    let mut shape = Vec::new();
    for item in wu::get_array(map, "shape")? {
        match item {
            Value::U64(dim) => shape.push(*dim),
            _ => return Err(Error::malformed("checkpoint tensor shape must be u64")),
        }
    }
    let count = tc::validate_shape(&shape, limits)?;
    let expected = tc::payload_len(count, physical)?;
    let element_bytes = wu::get_u64(map, "element_bytes")?;
    if expected != element_bytes {
        return Err(Error::corrupt(format!(
            "checkpoint tensor `{name}` declares {element_bytes} element bytes but \
             shape {:?} x {} requires {expected}",
            shape,
            dtype.as_str()
        )));
    }
    let logical_root = wu::get_hash(map, "logical_root")?;
    let content_root = wu::get_hash(map, "content_root")?;
    let source_offset = wu::get_u64(map, "source_offset")?;
    let shard_values = wu::get_array(map, "shards")?;
    if shard_values.is_empty() {
        return Err(Error::malformed(format!(
            "checkpoint tensor `{name}` has no shards"
        )));
    }
    let mut shards = Vec::with_capacity(shard_values.len());
    for item in shard_values {
        shards.push(shard_from_value(item)?);
    }
    validate_shard_set(name.as_str(), &shape, element_bytes, &shards, limits)?;
    Ok(CheckpointTensor {
        name,
        dtype: dtype.as_str().to_owned(),
        shape,
        logical_root,
        content_root,
        element_bytes,
        source_offset,
        shards,
    })
}

fn shard_from_value(value: &Value) -> Result<CheckpointShard> {
    let map = wu::as_map(value)?;
    wu::check_keys(
        map,
        &[
            "axis",
            "shards",
            "index",
            "object_name",
            "object_id",
            "content_root",
            "logical_root",
            "byte_len",
        ],
    )?;
    let axis = u32::try_from(wu::get_u64(map, "axis")?)
        .map_err(|_| Error::malformed("shard axis does not fit u32"))?;
    let shards = u32::try_from(wu::get_u64(map, "shards")?)
        .map_err(|_| Error::malformed("shard count does not fit u32"))?;
    let index = u32::try_from(wu::get_u64(map, "index")?)
        .map_err(|_| Error::malformed("shard index does not fit u32"))?;
    let object_name = wu::get_text(map, "object_name")?;
    crate::state::validate_name_bytes(object_name.as_bytes())?;
    Ok(CheckpointShard {
        axis,
        shards,
        index,
        object_name,
        object_id: wu::get_hash(map, "object_id")?,
        content_root: wu::get_hash(map, "content_root")?,
        logical_root: wu::get_hash(map, "logical_root")?,
        byte_len: wu::get_u64(map, "byte_len")?,
    })
}

fn validate_shard_set(
    name: &str,
    shape: &[u64],
    element_bytes: u64,
    shards: &[CheckpointShard],
    limits: &Limits,
) -> Result<()> {
    let first = &shards[0];
    if first.shards == 0 {
        return Err(Error::malformed(format!(
            "checkpoint tensor `{name}` declares zero shards"
        )));
    }
    if shards.len() != first.shards as usize {
        return Err(Error::malformed(format!(
            "checkpoint tensor `{name}` declares {} shard entries but shard count {}",
            shards.len(),
            first.shards
        )));
    }
    if first.shards as u64 > limits.max_structured_items {
        return Err(Error::limit_exceeded(format!(
            "checkpoint tensor `{name}` shard count exceeds the bound"
        )));
    }
    // The shard geometry itself is codec-validated.
    tc::shard_shape(shape, first.axis, first.shards, 0)?;
    let shard_bytes = element_bytes / u64::from(first.shards);
    let mut seen = BTreeSet::new();
    for shard in shards {
        if shard.axis != first.axis || shard.shards != first.shards {
            return Err(Error::non_canonical(format!(
                "checkpoint tensor `{name}` shard set disagrees on axis or shard count"
            )));
        }
        if shard.index >= first.shards {
            return Err(Error::non_canonical(format!(
                "checkpoint tensor `{name}` shard index {} is not below {}",
                shard.index, first.shards
            )));
        }
        if !seen.insert(shard.index) {
            return Err(Error::non_canonical(format!(
                "checkpoint tensor `{name}` shard index {} is duplicated",
                shard.index
            )));
        }
        if shard.byte_len != shard_bytes {
            return Err(Error::corrupt(format!(
                "checkpoint tensor `{name}` shard {} is {} bytes but {} shards of {} \
                 element bytes require {shard_bytes}",
                shard.index, shard.byte_len, first.shards, element_bytes
            )));
        }
    }
    if seen.len() != shards.len() {
        return Err(Error::non_canonical(format!(
            "checkpoint tensor `{name}` shard indices are not a permutation of 0..{}",
            first.shards
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn shard_object_name(tensor_index: usize, shard: u32) -> String {
    format!("t{tensor_index:05}.s{shard:03}")
}

/// Row-major byte shape of a tensor payload.
///
/// A fixed-width dtype is contiguous on the last axis, so the payload bytes of
/// a `shape` tensor are exactly a row-major `u8` tensor of `shape` with the
/// last dimension multiplied by the element size. That lets a mode-10 `q8_0`
/// object carry the exact bytes while the store's region API still addresses
/// them with logical coordinates.
fn byte_shape(shape: &[u64], element_bytes: u64) -> Result<Vec<u64>> {
    let mut out = shape.to_vec();
    let last = out
        .len()
        .checked_sub(1)
        .ok_or_else(|| Error::malformed("a tensor shape must have rank >= 1"))?;
    out[last] = out[last]
        .checked_mul(element_bytes)
        .ok_or_else(|| Error::limit_exceeded("byte shape overflows u64"))?;
    Ok(out)
}

fn validate_tile_shape(shape: &[u64], tile_shape: &[u64]) -> Result<()> {
    if tile_shape.len() != shape.len() {
        return Err(Error::invalid_argument(format!(
            "--tile-shape rank {} does not equal the shard rank {} (no silent padding)",
            tile_shape.len(),
            shape.len()
        )));
    }
    for (axis, (extent, tile)) in shape.iter().zip(tile_shape.iter()).enumerate() {
        if *tile == 0 {
            return Err(Error::invalid_argument(format!(
                "--tile-shape axis {axis} must be greater than zero"
            )));
        }
        if *extent % *tile != 0 {
            return Err(Error::invalid_argument(format!(
                "--tile-shape axis {axis} extent {tile} does not divide shard extent {extent} \
                 (no silent padding)"
            )));
        }
    }
    Ok(())
}

/// [`ensure_dir`] with a deterministic ObjectId seed for the new directory.
fn ensure_dir_seeded(
    store: &mut Store,
    parent: Hash32,
    name: &[u8],
    seed: Option<&[u8; 32]>,
) -> Result<Hash32> {
    match store.lookup(parent, name) {
        Ok(id) => {
            store.require_dir(id)?;
            Ok(id)
        }
        Err(err) if err.class() == Class::NotFound => {
            let (uid, gid) = (store.options().owner_uid, store.options().owner_gid);
            match seed {
                Some(seed) => store.mkdir_seeded(parent, name, 0o755, uid, gid, seed),
                None => store.mkdir(parent, name, 0o755, uid, gid),
            }
        }
        Err(err) => Err(err),
    }
}

/// Glob matching with `*` (any run of characters) and `?` (one character).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn selected_by_filters(name: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    filters.iter().any(|pattern| glob_match(pattern, name))
}

/// Domain for every deterministic checkpoint ObjectId seed.
const CHECKPOINT_SEED_DOMAIN: &str = "weft/checkpoint/seed/v1";

/// Deterministic ObjectId seeds derived from one checkpoint seed.
///
/// The derivation is a pure function of the base seed and the shard's
/// (tensor name, index), so the sequential writer and the prepare/commit
/// writer choose exactly the same ObjectIds (and therefore publish the same
/// commit bytes) for the same input.
struct CheckpointSeeds {
    base: [u8; 32],
    checkpoints: [u8; 32],
    dir: [u8; 32],
    manifest: [u8; 32],
}

impl CheckpointSeeds {
    fn new(base: &[u8; 32]) -> CheckpointSeeds {
        CheckpointSeeds {
            base: *base,
            checkpoints: derive_seed(base, b"checkpoints"),
            dir: derive_seed(base, b"dir"),
            manifest: derive_seed(base, b"manifest"),
        }
    }

    fn shard(&self, tensor: &str, shard_index: u32) -> [u8; 32] {
        let mut what = Vec::with_capacity(tensor.len() + 5);
        what.extend_from_slice(b"shard");
        what.extend_from_slice(&(tensor.len() as u32).to_le_bytes());
        what.extend_from_slice(tensor.as_bytes());
        what.extend_from_slice(&shard_index.to_le_bytes());
        derive_seed(&self.base, &what)
    }
}

fn derive_seed(base: &[u8; 32], what: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(base.len() + what.len());
    input.extend_from_slice(base);
    input.extend_from_slice(what);
    *identity::h(CHECKPOINT_SEED_DOMAIN, &input).as_bytes()
}

/// Validate every tensor's split and tile shape before anything is written, so
/// a non-divisible shape never leaves half a checkpoint on disk.
fn validate_checkpoint_layout(
    selected: &[&SafetensorsTensor],
    options: &CheckpointWriteOptions,
    effective_strategy: ShardStrategy,
) -> Result<()> {
    let limits = Limits::default();
    for tensor in selected {
        let dtype = TensorElementDtype::parse(&tensor.dtype)?;
        let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
            Error::unsupported_version(format!(
                "tensor `{}` dtype `{}` has no physical layout",
                tensor.name, tensor.dtype
            ))
        })?;
        tc::validate_shape(&tensor.shape, &limits)?;
        tc::payload_len(
            tensor.shape.iter().try_fold(1u64, |acc, dim| {
                acc.checked_mul(*dim)
                    .ok_or_else(|| Error::limit_exceeded("tensor shape overflow"))
            })?,
            physical,
        )?;
        if let Err(err) = tc::shard_shape(&tensor.shape, options.axis, options.shards, 0) {
            let err = Error::from(err);
            return Err(Error::new(
                err.class(),
                format!(
                    "tensor `{}` shape {:?} cannot be split {}-way on axis {} \
                     (no silent padding): {}",
                    tensor.name, tensor.shape, options.shards, options.axis, err
                ),
            ));
        }
        if matches!(
            effective_strategy,
            ShardStrategy::TensorTiled | ShardStrategy::FloatTiled
        ) {
            let tile = options.tile_shape.as_ref().ok_or_else(|| {
                Error::invalid_argument(format!(
                    "--shard-strategy {} requires --tile-shape A,B,...",
                    effective_strategy.as_str()
                ))
            })?;
            for shard_index in 0..options.shards {
                let shard_shape =
                    tc::shard_shape(&tensor.shape, options.axis, options.shards, shard_index)?;
                validate_tile_shape(&shard_shape, tile).map_err(|err| {
                    Error::new(
                        err.class(),
                        format!(
                            "tensor `{}` shard shape {:?} cannot be tiled {:?}: {}",
                            tensor.name,
                            shard_shape,
                            tile,
                            err.message()
                        ),
                    )
                })?;
            }
        }
    }
    Ok(())
}

/// The stored dtype/shape/strategy hint for one shard under a strategy.
fn checkpoint_shard_transport(
    dtype: TensorElementDtype,
    shard_shape: &[u64],
    physical: weftcodec::TensorDtype,
    options: &CheckpointWriteOptions,
    effective_strategy: ShardStrategy,
) -> Result<(String, Vec<u64>, Option<TensorStrategy>, Option<Vec<u64>>)> {
    Ok(match effective_strategy {
        // A q8_0 object treats every payload byte as a quantized word, so the
        // mode-10 round trip is exact. The transport shape doubles the last
        // logical dimension by the element size, keeping the payload's
        // row-major byte order addressable.
        ShardStrategy::TensorSharded => (
            "q8_0".to_owned(),
            byte_shape(shard_shape, physical.element_size())?,
            Some(TensorStrategy::TensorSharded),
            None,
        ),
        // Mode 15/17 carry the shard under its real dtype and shape; tile
        // coordinates are logical coordinates.
        ShardStrategy::TensorTiled | ShardStrategy::FloatTiled => {
            let tile = options.tile_shape.clone().ok_or_else(|| {
                Error::invalid_argument(format!(
                    "{} checkpoint shards require --tile-shape",
                    effective_strategy.as_str()
                ))
            })?;
            validate_tile_shape(shard_shape, &tile)?;
            let hint = if effective_strategy == ShardStrategy::TensorTiled {
                TensorStrategy::TensorTiled
            } else {
                TensorStrategy::FloatTiled
            };
            (
                dtype.as_str().to_owned(),
                shard_shape.to_vec(),
                Some(hint),
                Some(tile),
            )
        }
        ShardStrategy::Planned => (dtype.as_str().to_owned(), shard_shape.to_vec(), None, None),
        ShardStrategy::Zstd => (
            dtype.as_str().to_owned(),
            shard_shape.to_vec(),
            Some(TensorStrategy::Zstd),
            None,
        ),
        ShardStrategy::Raw => (
            dtype.as_str().to_owned(),
            shard_shape.to_vec(),
            Some(TensorStrategy::Raw),
            None,
        ),
    })
}

fn checkpoint_shard_spec(
    hint: Option<TensorStrategy>,
    tile_shape: Option<Vec<u64>>,
    causal: Option<Hash32>,
    fast: bool,
    options: &CheckpointWriteOptions,
) -> TensorWriteSpec {
    TensorWriteSpec {
        strategy_hint: hint,
        tile_shape,
        causal: causal
            .filter(|_| !fast)
            .map(|version| vec![version])
            .unwrap_or_default(),
        plan: crate::tensor_store::TensorPlanOptions {
            write_policy: if fast {
                WritePolicy::Fast
            } else {
                WritePolicy::Planned
            },
            fast_codec: options.fast_codec,
            fast_zstd_level: options.fast_zstd_level,
            semantic_search: options.semantic_search,
            allow_experimental_modes: options.allow_experimental_modes,
            ..crate::tensor_store::TensorPlanOptions::default()
        },
        ..TensorWriteSpec::default()
    }
}

fn checkpoint_strategy_hint(strategy: ShardStrategy) -> &'static str {
    match strategy {
        ShardStrategy::TensorSharded => {
            " (mode 10 needs a shard byte length divisible by 16; \
             choose --shard-strategy zstd otherwise)"
        }
        ShardStrategy::TensorTiled => {
            " (mode 15 needs a tile shape that divides every shard extent)"
        }
        ShardStrategy::FloatTiled => {
            " (mode 17 needs a tile shape that divides every shard extent)"
        }
        _ => "",
    }
}

fn context(tensor: &str, shard: &CheckpointShard, err: Error) -> Error {
    Error::new(
        err.class(),
        format!(
            "checkpoint tensor `{tensor}` shard {}/{} (`{}`, object {}): {}",
            shard.index,
            shard.shards,
            shard.object_name,
            shard.object_id,
            err.message()
        ),
    )
}

impl Store {
    /// Write a checkpoint from one safetensors container.
    ///
    /// Every shard object and the manifest are published in a single group
    /// commit: either the whole checkpoint is durable or none of it is.
    pub fn write_checkpoint(
        &mut self,
        name: &str,
        source: &Path,
        options: &CheckpointWriteOptions,
    ) -> Result<CheckpointWriteReport> {
        crate::state::validate_name_bytes(name.as_bytes())?;
        if options.shards == 0 {
            return Err(Error::invalid_argument("--shards must be at least 1"));
        }
        let file = SafetensorsFile::parse(source)?;
        if file.tensors.is_empty() {
            return Err(Error::invalid_argument(format!(
                "{}: the container has no tensors",
                source.display()
            )));
        }
        let selected: Vec<&SafetensorsTensor> = file
            .tensors
            .iter()
            .filter(|tensor| selected_by_filters(&tensor.name, &options.filters))
            .collect();
        if selected.is_empty() {
            return Err(Error::not_found(format!(
                "no tensor in {} matches the requested filters {:?}",
                source.display(),
                options.filters
            )));
        }
        let complete = selected.len() == file.tensors.len();
        let effective_strategy = options.effective_strategy();
        let fast = options.is_fast();

        // Validate every split before writing anything, so a non-divisible
        // shape never leaves half a checkpoint on disk.
        validate_checkpoint_layout(&selected, options, effective_strategy)?;
        let limits = Limits::default();

        let (uid, gid) = (self.options().owner_uid, self.options().owner_gid);
        let name_owned = name.to_owned();
        let options = options.clone();
        // Resolve the optional causal checkpoint's matching shard versions
        // before the group. A tensor is only linked when the dtype, shape and
        // shard layout agree; otherwise it is written independently.
        let causal_versions = self.resolve_causal_versions(&options)?;
        let seeds = options
            .deterministic_seed
            .map(|base| CheckpointSeeds::new(&base));
        let (report, head) = self.group(|store| {
            let checkpoints = ensure_dir_seeded(
                store,
                State::ROOT_ID,
                CHECKPOINT_ROOT.as_bytes(),
                seeds.as_ref().map(|seeds| &seeds.checkpoints),
            )?;
            let dir = match seeds.as_ref() {
                Some(seeds) => store.mkdir_seeded(
                    checkpoints,
                    name_owned.as_bytes(),
                    0o755,
                    uid,
                    gid,
                    &seeds.dir,
                )?,
                None => store.mkdir(checkpoints, name_owned.as_bytes(), 0o755, uid, gid)?,
            };
            let mut tensors = Vec::with_capacity(selected.len());
            let mut shard_objects = 0usize;
            for (tensor_index, tensor) in selected.iter().enumerate() {
                let dtype = TensorElementDtype::parse(&tensor.dtype)?;
                let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
                    Error::unsupported_version(format!(
                        "tensor `{}` dtype `{}` has no physical layout",
                        tensor.name, tensor.dtype
                    ))
                })?;
                // The source container must describe exactly the declared
                // payload; the streaming shard reads rely on it. Same class and
                // message shape as `weftcodec::tensor::check_payload`.
                let count = tc::validate_shape(&tensor.shape, &limits)?;
                let expected_payload = tc::payload_len(count, physical)?;
                if expected_payload != tensor.byte_len() {
                    return Err(Error::malformed(format!(
                        "tensor payload is {} bytes but shape {:?} x {} requires {expected_payload}",
                        tensor.byte_len(),
                        tensor.shape,
                        physical.as_str()
                    )));
                }
                // Whole-tensor identity is streamed in bounded windows: the
                // source is never materialised. `content_root` and
                // `logical_root` are byte-identical to the one-shot digests.
                let (content_root, values_hash) = file.hash_tensor_digests(
                    tensor,
                    safetensors::DEFAULT_INGEST_WINDOW_BYTES,
                    options.max_ingest_bytes,
                )?;
                let logical_root = tc::logical_root_from_values_hash(
                    physical,
                    &tensor.shape,
                    values_hash,
                    &limits,
                )?;
                let mut shards = Vec::with_capacity(options.shards as usize);
                let mut object_names = Vec::with_capacity(options.shards as usize);
                let mut object_ids = Vec::with_capacity(options.shards as usize);
                for shard_index in 0..options.shards {
                    let shard_shape =
                        tc::shard_shape(&tensor.shape, options.axis, options.shards, shard_index)?;
                    let shard_bytes = file.read_tensor_shard(
                        tensor,
                        physical,
                        options.axis,
                        options.shards,
                        shard_index,
                        options.max_ingest_bytes,
                    )?;
                    tc::check_payload(physical, &shard_shape, &shard_bytes, &limits)?;
                    let object_name = shard_object_name(tensor_index, shard_index);
                    let (store_dtype, store_shape, hint, tile_shape) = checkpoint_shard_transport(
                        dtype,
                        &shard_shape,
                        physical,
                        &options,
                        effective_strategy,
                    )?;
                    let spec = checkpoint_shard_spec(
                        hint,
                        tile_shape,
                        causal_versions
                            .get(&tensor.name)
                            .and_then(|versions| versions.get(shard_index as usize))
                            .copied(),
                        fast,
                        &options,
                    );
                    let id = match seeds.as_ref() {
                        Some(seeds) => store.write_tensor_file_with_spec_seeded(
                            dir,
                            object_name.as_bytes(),
                            &store_dtype,
                            &store_shape,
                            &shard_bytes,
                            &TensorLayoutSpec::row_major(),
                            &spec,
                            &seeds.shard(&tensor.name, shard_index),
                        ),
                        None => store.write_tensor_file_with_spec(
                            dir,
                            object_name.as_bytes(),
                            &store_dtype,
                            &store_shape,
                            &shard_bytes,
                            &TensorLayoutSpec::row_major(),
                            &spec,
                        ),
                    }
                    .map_err(|err| {
                        let hint = checkpoint_strategy_hint(effective_strategy);
                        Error::new(
                            err.class(),
                            format!(
                                "checkpoint tensor `{}` shard {}/{}: {}{hint}",
                                tensor.name,
                                shard_index,
                                options.shards,
                                err.message()
                            ),
                        )
                    })?;
                    shard_objects += 1;
                    shards.push(CheckpointShard {
                        axis: options.axis,
                        shards: options.shards,
                        index: shard_index,
                        object_name: object_name.clone(),
                        object_id: id,
                        content_root: identity::content_hash(&shard_bytes),
                        logical_root: tc::logical_root(
                            physical,
                            &shard_shape,
                            &shard_bytes,
                            &limits,
                        )?,
                        byte_len: shard_bytes.len() as u64,
                    });
                    object_names.push(object_name);
                    object_ids.push(id);
                }
                tensors.push((
                    CheckpointTensor {
                        name: tensor.name.clone(),
                        dtype: dtype.as_str().to_owned(),
                        shape: tensor.shape.clone(),
                        logical_root,
                        content_root,
                        element_bytes: tensor.byte_len(),
                        source_offset: tensor.begin,
                        shards,
                    },
                    object_names,
                    object_ids,
                ));
            }
            let total_element_bytes = tensors
                .iter()
                .map(|(tensor, _, _)| tensor.element_bytes)
                .sum::<u64>();
            let manifest = CheckpointManifest {
                name: name_owned.clone(),
                container: CHECKPOINT_CONTAINER.to_owned(),
                complete,
                element_bytes: total_element_bytes,
                source_header: Some(file.header_bytes.clone()),
                source_file_bytes: Some(file.file_bytes),
                source_sha256: Some(file.sha256),
                tensors: {
                    let mut sorted: Vec<CheckpointTensor> = tensors
                        .iter()
                        .map(|(tensor, _, _)| tensor.clone())
                        .collect();
                    sorted.sort_by(|a, b| a.name.cmp(&b.name));
                    sorted
                },
            };
            let manifest_bytes = manifest.canonical_bytes()?;
            let manifest_root = manifest.root()?;
            let mut attrs = BTreeMap::new();
            attrs.insert(
                "weft.checkpoint.schema".to_owned(),
                Value::Str(CHECKPOINT_SCHEMA.to_owned()),
            );
            attrs.insert(
                "weft.checkpoint.id".to_owned(),
                wu::hash_value(manifest_root),
            );
            let manifest_id = match seeds.as_ref() {
                Some(seeds) => store.create_file_seeded(
                    dir,
                    CHECKPOINT_MANIFEST_NAME.as_bytes(),
                    &manifest_bytes,
                    0o644,
                    uid,
                    gid,
                    attrs,
                    &seeds.manifest,
                )?,
                None => store.create_file(
                    dir,
                    CHECKPOINT_MANIFEST_NAME.as_bytes(),
                    &manifest_bytes,
                    0o644,
                    uid,
                    gid,
                    attrs,
                )?,
            };
            let written = tensors
                .into_iter()
                .map(|(tensor, object_names, object_ids)| WrittenTensor {
                    name: tensor.name,
                    dtype: tensor.dtype,
                    shape: tensor.shape,
                    logical_root: tensor.logical_root,
                    element_bytes: tensor.element_bytes,
                    axis: options.axis,
                    shards: options.shards,
                    object_names,
                    object_ids,
                })
                .collect::<Vec<_>>();
            Ok(CheckpointWriteReport {
                name: name_owned.clone(),
                checkpoint_id: dir,
                head: Hash32::ZERO,
                manifest_id,
                manifest_root,
                tensors: written,
                total_element_bytes,
                shard_objects,
                complete,
                source_sha256: file.sha256,
                source_file_bytes: file.file_bytes,
                prepare_ms: None,
                validate_ms: None,
                commit_ms: None,
            })
        })?;
        let mut report = report;
        report.head = head;
        Ok(report)
    }

    /// Resolve the matching shard versions of an optional causal checkpoint.
    fn resolve_causal_versions(
        &self,
        options: &CheckpointWriteOptions,
    ) -> Result<BTreeMap<String, Vec<Hash32>>> {
        let mut causal_versions: BTreeMap<String, Vec<Hash32>> = BTreeMap::new();
        let Some(previous) = options.causal_checkpoint.as_deref() else {
            return Ok(causal_versions);
        };
        let previous_manifest = self.load_checkpoint(previous)?;
        for tensor in &previous_manifest.tensors {
            let mut versions = Vec::with_capacity(tensor.shards.len());
            let mut usable = tensor.shards.len() == options.shards as usize;
            for shard in &tensor.shards {
                if shard.axis != options.axis || shard.shards != options.shards {
                    usable = false;
                    break;
                }
                match self.object_meta(shard.object_id) {
                    Ok(record) => versions.push(record.version),
                    Err(_) => {
                        usable = false;
                        break;
                    }
                }
            }
            if usable {
                causal_versions.insert(tensor.name.clone(), versions);
            }
        }
        Ok(causal_versions)
    }

    /// Write a checkpoint with a **prepare / commit split**: `ranks` workers
    /// publish immutable, content-addressed blob sets in parallel and return
    /// manifest fragments; exactly one committer validates every fragment and
    /// publishes all state ops in a single commit.
    ///
    /// The single-writer proof is untouched. Workers hold read-only handles and
    /// only ever call create-once blob publication (`put_if_absent` / an
    /// invariant-preserving `put_atomic` on an immutable key); no second
    /// process touches the superblock, a commit record, the writer lock, or the
    /// lease. The committer is the same handle that already owns them.
    ///
    /// Failure semantics: a worker that dies (error or panic) leaves only
    /// unreferenced blobs, which GC reclaims; the committer never publishes a
    /// partial set and the error names the tensor and shard. A shard name that
    /// already exists with different bytes fails closed (`Exists`/`Corrupt`),
    /// exactly as the sequential path does.
    pub fn write_checkpoint_parallel(
        &mut self,
        name: &str,
        source: &Path,
        options: &CheckpointWriteOptions,
        parallel: &ParallelCheckpointOptions,
    ) -> Result<CheckpointWriteReport> {
        crate::state::validate_name_bytes(name.as_bytes())?;
        if options.shards == 0 {
            return Err(Error::invalid_argument("--shards must be at least 1"));
        }
        parallel.validate()?;
        let file = SafetensorsFile::parse(source)?;
        if file.tensors.is_empty() {
            return Err(Error::invalid_argument(format!(
                "{}: the container has no tensors",
                source.display()
            )));
        }
        let selected: Vec<&SafetensorsTensor> = file
            .tensors
            .iter()
            .filter(|tensor| selected_by_filters(&tensor.name, &options.filters))
            .collect();
        if selected.is_empty() {
            return Err(Error::not_found(format!(
                "no tensor in {} matches the requested filters {:?}",
                source.display(),
                options.filters
            )));
        }
        let complete = selected.len() == file.tensors.len();
        let effective_strategy = options.effective_strategy();
        let fast = options.is_fast();
        validate_checkpoint_layout(&selected, options, effective_strategy)?;
        let causal_versions = self.resolve_causal_versions(options)?;
        let seeds = options
            .deterministic_seed
            .map(|base| CheckpointSeeds::new(&base));
        let deadline = parallel
            .deadline_ms
            .map(|ms| Instant::now() + Duration::from_millis(ms));

        // ---- prepare (N workers, one task = one tensor) --------------------
        // A task is one tensor rather than one shard because the tensor-level
        // LogicalTensorRoot spans all of its shards; the worker that holds the
        // tensor bytes is the only one that can compute it. Every blob a worker
        // writes is content-addressed, so task order and rank assignment are
        // irrelevant.
        let in_flight = ParallelBudget::new(parallel.max_in_flight_bytes);
        let prepare_started = Instant::now();
        let prepared: Vec<PreparedCheckpointTensor> = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(parallel.ranks);
            for rank in 0..parallel.ranks {
                let file = &file;
                let selected = &selected;
                let options = options.clone();
                let causal_versions = &causal_versions;
                let seeds = seeds.as_ref();
                let in_flight = &in_flight;
                let root = self.root().to_path_buf();
                let store_options = self.options().clone();
                let backend_kind = self.backend_kind().clone();
                let per_rank_bytes = parallel.per_rank_bytes;
                let ranks = parallel.ranks;
                let deadline = deadline;
                handles.push(scope.spawn(move || -> Result<Vec<PreparedCheckpointTensor>> {
                    // One isolated handle per rank: its own backend instance and
                    // its own replayed read-only state. Control-state
                    // publication is refused by `require_writable`; immutable
                    // blob publication is the only write this handle performs.
                    let worker =
                        Store::open_blob_worker(&root, store_options, backend_kind)?;
                    let mut out = Vec::new();
                    let mut rank_bytes: u64 = 0;
                    for (tensor_index, tensor) in selected.iter().enumerate() {
                        if tensor_index % ranks != rank {
                            continue;
                        }
                        if let Some(limit) = deadline {
                            if Instant::now() >= limit {
                                return Err(Error::cancelled(format!(
                                    "checkpoint prepare deadline ({:?}) reached before tensor `{}`",
                                    parallel.deadline_ms.unwrap_or(0),
                                    tensor.name
                                )));
                            }
                        }
                        let need = tensor.byte_len();
                        rank_bytes = rank_bytes.checked_add(need).ok_or_else(|| {
                            Error::limit_exceeded("per-rank byte accounting overflow")
                        })?;
                        if rank_bytes > per_rank_bytes {
                            return Err(Error::limit_exceeded(format!(
                                "rank {rank} would prepare {rank_bytes} bytes, above its {per_rank_bytes}-byte budget"
                            )));
                        }
                        let reserved = in_flight.reserve(need, deadline)?;
                        let fragment = worker.prepare_checkpoint_tensor(
                            file,
                            tensor,
                            tensor_index,
                            &options,
                            effective_strategy,
                            fast,
                            causal_versions,
                            seeds,
                            deadline,
                        );
                        in_flight.release(reserved);
                        out.push(fragment?);
                    }
                    Ok(out)
                }));
            }
            let mut all = Vec::with_capacity(selected.len());
            for handle in handles {
                match handle.join() {
                    Ok(Ok(items)) => all.extend(items),
                    Ok(Err(err)) => return Err(err),
                    Err(_) => {
                        return Err(Error::cancelled(
                            "a checkpoint prepare worker panicked; nothing was published",
                        ))
                    }
                }
            }
            all.sort_by_key(|item| item.tensor_index);
            Ok(all)
        })?;

        let prepare_ms = prepare_started.elapsed().as_secs_f64() * 1000.0;

        // ---- validate every fragment (the single committer) ----------------
        let validate_started = Instant::now();
        if parallel.verify_fragments {
            for fragment in &prepared {
                self.verify_prepared_checkpoint_tensor(fragment)?;
            }
        }
        let validate_ms = validate_started.elapsed().as_secs_f64() * 1000.0;

        // ---- commit (one publication) --------------------------------------
        let (uid, gid) = (self.options().owner_uid, self.options().owner_gid);
        let name_owned = name.to_owned();
        let options = options.clone();
        let commit_started = Instant::now();
        let (report, head) = self.group(|store| {
            let checkpoints = ensure_dir_seeded(
                store,
                State::ROOT_ID,
                CHECKPOINT_ROOT.as_bytes(),
                seeds.as_ref().map(|seeds| &seeds.checkpoints),
            )?;
            let dir = match seeds.as_ref() {
                Some(seeds) => store.mkdir_seeded(
                    checkpoints,
                    name_owned.as_bytes(),
                    0o755,
                    uid,
                    gid,
                    &seeds.dir,
                )?,
                None => store.mkdir(checkpoints, name_owned.as_bytes(), 0o755, uid, gid)?,
            };
            let mut tensors = Vec::with_capacity(prepared.len());
            let mut shard_objects = 0usize;
            for fragment in &prepared {
                let mut object_ids = Vec::with_capacity(fragment.prepared.len());
                for (object_name, shard) in
                    fragment.object_names.iter().zip(fragment.prepared.iter())
                {
                    let id = store.commit_prepared_tensor_object(
                        dir,
                        object_name.as_bytes(),
                        shard,
                        0o644,
                    )?;
                    object_ids.push(id);
                    shard_objects += 1;
                }
                tensors.push((
                    fragment.tensor.clone(),
                    fragment.object_names.clone(),
                    object_ids,
                ));
            }
            let total_element_bytes = tensors
                .iter()
                .map(|(tensor, _, _)| tensor.element_bytes)
                .sum::<u64>();
            let manifest = CheckpointManifest {
                name: name_owned.clone(),
                container: CHECKPOINT_CONTAINER.to_owned(),
                complete,
                element_bytes: total_element_bytes,
                source_header: Some(file.header_bytes.clone()),
                source_file_bytes: Some(file.file_bytes),
                source_sha256: Some(file.sha256),
                tensors: {
                    let mut sorted: Vec<CheckpointTensor> = tensors
                        .iter()
                        .map(|(tensor, _, _)| tensor.clone())
                        .collect();
                    sorted.sort_by(|a, b| a.name.cmp(&b.name));
                    sorted
                },
            };
            let manifest_bytes = manifest.canonical_bytes()?;
            let manifest_root = manifest.root()?;
            let mut attrs = BTreeMap::new();
            attrs.insert(
                "weft.checkpoint.schema".to_owned(),
                Value::Str(CHECKPOINT_SCHEMA.to_owned()),
            );
            attrs.insert(
                "weft.checkpoint.id".to_owned(),
                wu::hash_value(manifest_root),
            );
            let manifest_id = match seeds.as_ref() {
                Some(seeds) => store.create_file_seeded(
                    dir,
                    CHECKPOINT_MANIFEST_NAME.as_bytes(),
                    &manifest_bytes,
                    0o644,
                    uid,
                    gid,
                    attrs,
                    &seeds.manifest,
                )?,
                None => store.create_file(
                    dir,
                    CHECKPOINT_MANIFEST_NAME.as_bytes(),
                    &manifest_bytes,
                    0o644,
                    uid,
                    gid,
                    attrs,
                )?,
            };
            let written = tensors
                .into_iter()
                .map(|(tensor, object_names, object_ids)| WrittenTensor {
                    name: tensor.name,
                    dtype: tensor.dtype,
                    shape: tensor.shape,
                    logical_root: tensor.logical_root,
                    element_bytes: tensor.element_bytes,
                    axis: options.axis,
                    shards: options.shards,
                    object_names,
                    object_ids,
                })
                .collect::<Vec<_>>();
            Ok(CheckpointWriteReport {
                name: name_owned.clone(),
                checkpoint_id: dir,
                head: Hash32::ZERO,
                manifest_id,
                manifest_root,
                tensors: written,
                total_element_bytes,
                shard_objects,
                complete,
                source_sha256: file.sha256,
                source_file_bytes: file.file_bytes,
                prepare_ms: Some(prepare_ms),
                validate_ms: Some(validate_ms),
                commit_ms: Some(0.0),
            })
        })?;
        let commit_ms = commit_started.elapsed().as_secs_f64() * 1000.0;
        let mut report = report;
        report.head = head;
        report.commit_ms = Some(commit_ms);
        Ok(report)
    }

    /// Prepare one tensor (all of its shards) from a worker handle: publish
    /// immutable blobs and return the manifest fragment. No state op is
    /// published; the caller owns the single commit.
    #[allow(clippy::too_many_arguments)]
    fn prepare_checkpoint_tensor(
        &self,
        file: &SafetensorsFile,
        tensor: &SafetensorsTensor,
        tensor_index: usize,
        options: &CheckpointWriteOptions,
        effective_strategy: ShardStrategy,
        fast: bool,
        causal_versions: &BTreeMap<String, Vec<Hash32>>,
        seeds: Option<&CheckpointSeeds>,
        deadline: Option<Instant>,
    ) -> Result<PreparedCheckpointTensor> {
        let limits = Limits::default();
        let dtype = TensorElementDtype::parse(&tensor.dtype)?;
        let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
            Error::unsupported_version(format!(
                "tensor `{}` dtype `{}` has no physical layout",
                tensor.name, tensor.dtype
            ))
        })?;
        // The prepare/commit split re-verifies the whole reassembled tensor in
        // the committer, so it still materialises one tensor; the bound is
        // enforced here so the option is never silently ignored on this path.
        if tensor.byte_len() > options.max_ingest_bytes {
            return Err(Error::limit_exceeded(format!(
                "tensor `{}` is {} bytes, above the {}-byte ingest bound (raise --max-ingest-bytes)",
                tensor.name,
                tensor.byte_len(),
                options.max_ingest_bytes
            )));
        }
        let bytes = file.read_tensor(tensor)?;
        tc::check_payload(physical, &tensor.shape, &bytes, &limits)?;
        let mut shards = Vec::with_capacity(options.shards as usize);
        let mut object_names = Vec::with_capacity(options.shards as usize);
        let mut prepared = Vec::with_capacity(options.shards as usize);
        for shard_index in 0..options.shards {
            if let Some(limit) = deadline {
                if Instant::now() >= limit {
                    return Err(Error::cancelled(format!(
                        "checkpoint prepare deadline reached at tensor `{}` shard {}/{} (nothing was published)",
                        tensor.name, shard_index, options.shards
                    )));
                }
            }
            let shard_shape =
                tc::shard_shape(&tensor.shape, options.axis, options.shards, shard_index)?;
            let shard_bytes = tc::shard_bytes(
                physical,
                &tensor.shape,
                options.axis,
                options.shards,
                shard_index,
                &bytes,
                &limits,
            )?;
            let object_name = shard_object_name(tensor_index, shard_index);
            let (store_dtype, store_shape, hint, tile_shape) = checkpoint_shard_transport(
                dtype,
                &shard_shape,
                physical,
                options,
                effective_strategy,
            )?;
            let spec = checkpoint_shard_spec(
                hint,
                tile_shape,
                causal_versions
                    .get(&tensor.name)
                    .and_then(|versions| versions.get(shard_index as usize))
                    .copied(),
                fast,
                options,
            );
            let object = self
                .prepare_tensor_object_with_spec_seeded(
                    &store_dtype,
                    &store_shape,
                    &shard_bytes,
                    &TensorLayoutSpec::row_major(),
                    &spec,
                    &match seeds {
                        Some(seeds) => seeds.shard(&tensor.name, shard_index),
                        None => identity::os_random_32()?,
                    },
                )
                .map_err(|err| {
                    let hint = checkpoint_strategy_hint(effective_strategy);
                    Error::new(
                        err.class(),
                        format!(
                            "checkpoint tensor `{}` shard {}/{}: {}{hint}",
                            tensor.name,
                            shard_index,
                            options.shards,
                            err.message()
                        ),
                    )
                })?;
            shards.push(CheckpointShard {
                axis: options.axis,
                shards: options.shards,
                index: shard_index,
                object_name: object_name.clone(),
                object_id: identity::object_id_from_seed(&object.seed),
                content_root: object.content_root,
                // The checkpoint manifest records the *logical* shard root under
                // the tensor's own dtype, exactly as the sequential path does;
                // a q8_0 byte transport is a storage detail.
                logical_root: tc::logical_root(physical, &shard_shape, &shard_bytes, &limits)?,
                byte_len: object.size,
            });
            object_names.push(object_name);
            prepared.push(object);
            // Court hook: pause after a shard's blobs are durable and before the
            // next one, so an external kill can prove "worker died before any
            // publication -> nothing visible, GC reclaims the orphans". Inert
            // unless `WEFT_CHECKPOINT_PREPARE_PAUSE_MS` is set.
            if let Ok(text) = std::env::var("WEFT_CHECKPOINT_PREPARE_PAUSE_MS") {
                if let Ok(ms) = text.parse::<u64>() {
                    std::thread::sleep(Duration::from_millis(ms));
                }
            }
        }
        let logical_root = tc::logical_root(physical, &tensor.shape, &bytes, &limits)?;
        let tensor = CheckpointTensor {
            name: tensor.name.clone(),
            dtype: dtype.as_str().to_owned(),
            shape: tensor.shape.clone(),
            logical_root,
            content_root: identity::content_hash(&bytes),
            element_bytes: bytes.len() as u64,
            source_offset: tensor.begin,
            shards,
        };
        Ok(PreparedCheckpointTensor {
            tensor_index,
            tensor,
            object_names,
            prepared,
        })
    }

    /// Re-read every prepared shard through the *committer's* backend and
    /// prove it decodes to the exact bytes, content root, and logical root the
    /// fragment claims. A worker's claims are never trusted.
    fn verify_prepared_checkpoint_tensor(&self, fragment: &PreparedCheckpointTensor) -> Result<()> {
        let limits = Limits::default();
        let dtype = TensorElementDtype::parse(&fragment.tensor.dtype)?;
        let mut tensor_bytes = Vec::with_capacity(
            usize::try_from(fragment.tensor.element_bytes)
                .map_err(|_| Error::limit_exceeded("tensor does not fit in memory"))?,
        );
        for (shard, prepared) in fragment.tensor.shards.iter().zip(fragment.prepared.iter()) {
            let bytes = self
                .read_manifest_verified(prepared.manifest_id, prepared.content_root, prepared.size)
                .map_err(|err| {
                    Error::new(
                        err.class(),
                        format!(
                            "prepared shard `{}` (tensor `{}`): {}",
                            shard.object_name, fragment.tensor.name, err
                        ),
                    )
                })?;
            if identity::content_hash(&bytes) != prepared.content_root {
                return Err(Error::corrupt(format!(
                    "prepared shard `{}` bytes do not match their content root",
                    shard.object_name
                )));
            }
            let physical = dtype
                .physical_tensor_dtype()
                .ok_or_else(|| Error::unsupported_version("tensor dtype has no logical root"))?;
            let shard_shape = tc::shard_shape(
                &fragment.tensor.shape,
                shard.axis,
                shard.shards,
                shard.index,
            )?;
            let logical = tc::logical_root(physical, &shard_shape, &bytes, &limits)?;
            if logical != shard.logical_root {
                return Err(Error::corrupt(format!(
                    "prepared shard `{}` logical root disagrees with its bytes",
                    shard.object_name
                )));
            }
            tensor_bytes.extend_from_slice(&bytes);
        }
        if tensor_bytes.len() as u64 != fragment.tensor.element_bytes {
            return Err(Error::corrupt(format!(
                "prepared tensor `{}` reassembles to {} bytes, not {}",
                fragment.tensor.name,
                tensor_bytes.len(),
                fragment.tensor.element_bytes
            )));
        }
        if identity::content_hash(&tensor_bytes) != fragment.tensor.content_root {
            return Err(Error::corrupt(format!(
                "prepared tensor `{}` reassembled content root mismatch",
                fragment.tensor.name
            )));
        }
        let physical = dtype
            .physical_tensor_dtype()
            .ok_or_else(|| Error::unsupported_version("tensor dtype has no logical root"))?;
        let logical = tc::logical_root(physical, &fragment.tensor.shape, &tensor_bytes, &limits)?;
        if logical != fragment.tensor.logical_root {
            return Err(Error::corrupt(format!(
                "prepared tensor `{}` logical root disagrees with its bytes",
                fragment.tensor.name
            )));
        }
        Ok(())
    }

    /// Load and validate one checkpoint manifest.
    pub fn load_checkpoint(&self, name: &str) -> Result<CheckpointManifest> {
        let (_, manifest) = self.load_checkpoint_with_id(name)?;
        Ok(manifest)
    }

    fn load_checkpoint_with_id(&self, name: &str) -> Result<(Hash32, CheckpointManifest)> {
        let id = self.resolve_components(&[
            CHECKPOINT_ROOT.as_bytes(),
            name.as_bytes(),
            CHECKPOINT_MANIFEST_NAME.as_bytes(),
        ])?;
        let bytes = self.read_object(id)?;
        Ok((id, CheckpointManifest::decode(&bytes)?))
    }

    /// Load a checkpoint manifest against a pinned state and snapshot.
    fn load_checkpoint_with_id_at(
        &self,
        state: &State,
        snapshot: Option<Hash32>,
        name: &str,
    ) -> Result<(Hash32, CheckpointManifest)> {
        let id = resolve_in_state(
            state,
            &[
                CHECKPOINT_ROOT.as_bytes(),
                name.as_bytes(),
                CHECKPOINT_MANIFEST_NAME.as_bytes(),
            ],
        )?;
        let bytes = self.read_object_at(snapshot, id)?;
        Ok((id, CheckpointManifest::decode(&bytes)?))
    }

    /// Names of every checkpoint directory.
    pub fn list_checkpoints(&self) -> Result<Vec<String>> {
        let dir = match self.resolve_components(&[CHECKPOINT_ROOT.as_bytes()]) {
            Ok(dir) => dir,
            Err(err) if err.class() == Class::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut names = Vec::new();
        for entry in self.list_dir(dir)? {
            match String::from_utf8(entry.name) {
                Ok(name) => names.push(name),
                Err(_) => continue,
            }
        }
        names.sort();
        Ok(names)
    }
}

// ---------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------

fn checked_product(values: &[u64]) -> Result<u64> {
    values.iter().try_fold(1u64, |acc, dim| {
        acc.checked_mul(*dim)
            .ok_or_else(|| Error::limit_exceeded("tensor dimension product overflows u64"))
    })
}

fn row_major_strides(shape: &[u64]) -> Vec<u64> {
    let mut strides = vec![1u64; shape.len()];
    for axis in (0..shape.len().saturating_sub(1)).rev() {
        strides[axis] = strides[axis + 1].saturating_mul(shape[axis + 1]);
    }
    strides
}

#[allow(clippy::too_many_arguments)]
fn copy_box(
    dst: &mut [u8],
    dst_shape: &[u64],
    dst_start: &[u64],
    src: &[u8],
    src_shape: &[u64],
    src_start: &[u64],
    extent: &[u64],
    element_bytes: usize,
) -> Result<()> {
    let dst_strides = row_major_strides(dst_shape);
    let src_strides = row_major_strides(src_shape);
    fn recurse(
        axis: usize,
        dst: &mut [u8],
        dst_strides: &[u64],
        dst_start: &[u64],
        src: &[u8],
        src_strides: &[u64],
        src_start: &[u64],
        extent: &[u64],
        element_bytes: usize,
        dst_base: u64,
        src_base: u64,
    ) -> Result<()> {
        if axis + 1 == extent.len() {
            let dst_off = dst_base
                .checked_add(dst_start[axis].saturating_mul(dst_strides[axis]))
                .and_then(|offset| offset.checked_mul(element_bytes as u64))
                .ok_or_else(|| Error::limit_exceeded("destination offset overflows"))?;
            let src_off = src_base
                .checked_add(src_start[axis].saturating_mul(src_strides[axis]))
                .and_then(|offset| offset.checked_mul(element_bytes as u64))
                .ok_or_else(|| Error::limit_exceeded("source offset overflows"))?;
            let len = usize::try_from(extent[axis])
                .ok()
                .and_then(|count| count.checked_mul(element_bytes))
                .ok_or_else(|| Error::limit_exceeded("copy length overflows"))?;
            let dst_off = usize::try_from(dst_off)
                .map_err(|_| Error::limit_exceeded("destination offset does not fit usize"))?;
            let src_off = usize::try_from(src_off)
                .map_err(|_| Error::limit_exceeded("source offset does not fit usize"))?;
            let dst_end = dst_off
                .checked_add(len)
                .ok_or_else(|| Error::limit_exceeded("destination range overflows"))?;
            let src_end = src_off
                .checked_add(len)
                .ok_or_else(|| Error::limit_exceeded("source range overflows"))?;
            if dst_end > dst.len() || src_end > src.len() {
                return Err(Error::corrupt("region copy extends past a tensor buffer"));
            }
            dst[dst_off..dst_end].copy_from_slice(&src[src_off..src_end]);
            return Ok(());
        }
        for index in 0..extent[axis] {
            let dst_next = dst_base
                .checked_add((dst_start[axis] + index).saturating_mul(dst_strides[axis]))
                .ok_or_else(|| Error::limit_exceeded("destination base overflows"))?;
            let src_next = src_base
                .checked_add((src_start[axis] + index).saturating_mul(src_strides[axis]))
                .ok_or_else(|| Error::limit_exceeded("source base overflows"))?;
            recurse(
                axis + 1,
                dst,
                dst_strides,
                dst_start,
                src,
                src_strides,
                src_start,
                extent,
                element_bytes,
                dst_next,
                src_next,
            )?;
        }
        Ok(())
    }
    recurse(
        0,
        dst,
        &dst_strides,
        dst_start,
        src,
        &src_strides,
        src_start,
        extent,
        element_bytes,
        0,
        0,
    )
}

struct RestoredBytes {
    bytes: Vec<u8>,
    shape: Vec<u64>,
    shards_read: usize,
    units_touched: u64,
    full_root_verified: bool,
    shard_roots_verified: bool,
    restored_content_root: Hash32,
    topology_root: Option<Hash32>,
}

impl Store {
    /// Load a checkpoint manifest by name.
    pub fn checkpoint_manifest(&self, name: &str) -> Result<CheckpointManifest> {
        self.load_checkpoint(name)
    }

    /// Restore a checkpoint into `writer`, returning the measured report.
    ///
    /// The output is a complete safetensors container. When every tensor of a
    /// complete checkpoint is restored, the original header is reproduced byte
    /// for byte and the result's SHA-256 equals the source file's. A partial
    /// restore (one tensor or a region) writes a synthesized header and reports
    /// `exact_source_file = false` and `header_reproduced = false`.
    pub fn restore_checkpoint<W: Write>(
        &self,
        name: &str,
        options: &CheckpointRestoreOptions,
        writer: &mut W,
    ) -> Result<CheckpointRestoreReport> {
        self.restore_checkpoint_inner(None, None, name, options, false, writer)
    }

    /// Restore a checkpoint as it exists at a pinned snapshot.
    ///
    /// Checkpoint entry names, shard object ids, and every shard read are all
    /// resolved against the pinned state. The live head is never consulted, so a
    /// later rename, delete, or rewrite cannot change what is verified.
    pub fn restore_checkpoint_at<W: Write>(
        &self,
        snapshot: Hash32,
        name: &str,
        options: &CheckpointRestoreOptions,
        writer: &mut W,
    ) -> Result<CheckpointRestoreReport> {
        let state = self.state_at(snapshot)?;
        self.restore_checkpoint_inner(Some(&state), Some(snapshot), name, options, false, writer)
    }

    fn restore_checkpoint_inner<W: Write>(
        &self,
        state: Option<&State>,
        snapshot: Option<Hash32>,
        name: &str,
        options: &CheckpointRestoreOptions,
        lazy_tiles: bool,
        writer: &mut W,
    ) -> Result<CheckpointRestoreReport> {
        if options.region.is_some() && options.tensor.is_none() {
            return Err(Error::invalid_argument(
                "a region restore requires --tensor (one named tensor)",
            ));
        }
        if options.region.is_some() && options.topology.is_some() {
            return Err(Error::invalid_argument(
                "a region restore cannot be verified against a full-tensor topology",
            ));
        }
        let (_, manifest) = match state {
            Some(state) => self.load_checkpoint_with_id_at(state, snapshot, name)?,
            None => self.load_checkpoint_with_id(name)?,
        };
        let manifest_root = manifest.root()?;
        let selected: Vec<&CheckpointTensor> = match &options.tensor {
            Some(tensor) => vec![manifest.tensor(tensor)?],
            None => manifest.tensors.iter().collect(),
        };
        let mut order: Vec<&CheckpointTensor> = selected.clone();
        order.sort_by_key(|tensor| tensor.source_offset);

        let limits = Limits::default();
        let before = self.backend_stats();

        let exact_all = options.tensor.is_none()
            && options.region.is_none()
            && manifest.complete
            && manifest.source_header.is_some()
            && manifest.source_sha256.is_some();
        let header = match (&manifest.source_header, exact_all) {
            (Some(header), true) => header.clone(),
            _ => {
                let mut entries = Vec::with_capacity(order.len());
                for tensor in &order {
                    let shape = match &options.region {
                        Some(region) => region.extent(),
                        None => tensor.shape.clone(),
                    };
                    let dtype = TensorElementDtype::parse(&tensor.dtype)?;
                    let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
                        Error::unsupported_version("tensor dtype has no physical layout")
                    })?;
                    let len = tc::payload_len(checked_product(&shape)?, physical)?;
                    entries.push((tensor.name.as_str(), tensor.dtype.as_str(), shape, len));
                }
                let metadata = manifest
                    .source_header
                    .as_ref()
                    .and_then(|header| source_metadata(header));
                let borrowed: Vec<(&str, &str, &[u64], u64)> = entries
                    .iter()
                    .map(|(name, dtype, shape, len)| (*name, *dtype, shape.as_slice(), *len))
                    .collect();
                safetensors::encode_header(&borrowed, metadata.as_ref())?
            }
        };

        let mut hasher = Sha256::new();
        hasher.update(&header);
        writer
            .write_all(&header)
            .map_err(|err| Error::io(format!("restore output: {err}")))?;
        let mut file_bytes = header.len() as u64;

        let mut tensors = Vec::with_capacity(order.len());
        let mut shards_read = 0usize;
        let mut units_touched = 0u64;
        for tensor in &order {
            let restored = self.restore_tensor(
                name,
                tensor,
                options.region.as_ref(),
                options.topology.as_ref(),
                &limits,
                state,
                snapshot,
                lazy_tiles,
            )?;
            writer
                .write_all(&restored.bytes)
                .map_err(|err| Error::io(format!("restore output: {err}")))?;
            hasher.update(&restored.bytes);
            file_bytes += restored.bytes.len() as u64;
            shards_read += restored.shards_read;
            units_touched += restored.units_touched;
            tensors.push(RestoredTensor {
                name: tensor.name.clone(),
                dtype: tensor.dtype.clone(),
                shape: restored.shape,
                full_shape: tensor.shape.clone(),
                full_logical_root: tensor.logical_root,
                restored_content_root: restored.restored_content_root,
                bytes: restored.bytes.len() as u64,
                shards_read: restored.shards_read,
                units_touched: restored.units_touched,
                full_root_verified: restored.full_root_verified,
                shard_roots_verified: restored.shard_roots_verified,
                topology_root: restored.topology_root,
                sha256: safetensors::sha256(&restored.bytes),
            });
        }
        let file_sha256: [u8; 32] = hasher.finalize().into();
        let exact_source_file = manifest
            .source_sha256
            .map(|expected| expected == file_sha256)
            .unwrap_or(false);
        let header_reproduced = manifest
            .source_header
            .as_ref()
            .map(|expected| expected.as_slice() == header.as_slice())
            .unwrap_or(false);
        if exact_all {
            if let Some(expected_len) = manifest.source_file_bytes {
                if file_bytes != expected_len {
                    return Err(Error::corrupt(format!(
                        "restored file is {file_bytes} bytes but the source checkpoint records \
                         {expected_len}"
                    )));
                }
            }
            if let Some(expected) = manifest.source_sha256 {
                if file_sha256 != expected {
                    return Err(Error::corrupt(format!(
                        "restored file SHA-256 {} does not match the checkpoint's source SHA-256 {}",
                        hex::encode(file_sha256),
                        hex::encode(expected)
                    )));
                }
            }
        }
        let after = self.backend_stats();
        Ok(CheckpointRestoreReport {
            name: name.to_owned(),
            manifest_root,
            file_sha256,
            file_bytes,
            source_sha256: manifest.source_sha256,
            exact_source_file,
            header_reproduced,
            tensors,
            shards_read,
            units_touched,
            store_bytes_read: after.bytes_read.saturating_sub(before.bytes_read),
            store_reads: after.reads.saturating_sub(before.reads)
                + after.range_reads.saturating_sub(before.range_reads),
        })
    }

    /// Restore exactly one named tensor's payload, without a container header.
    ///
    /// `snapshot` pins the manifest and every shard read to one commit. The
    /// returned bytes are the tensor's canonical row-major payload (or the
    /// region's payload when `region` is set). Every shard root, the whole
    /// tensor root, the content root, and (when requested) the re-layout
    /// topology root are verified before bytes are returned.
    pub fn restore_named_tensor_at(
        &self,
        snapshot: Option<Hash32>,
        checkpoint: &str,
        tensor_name: &str,
        region: Option<&Region>,
        topology: Option<&RestoreTopology>,
    ) -> Result<RestoredTensorBytes> {
        if region.is_some() && topology.is_some() {
            return Err(Error::invalid_argument(
                "a region restore cannot be verified against a full-tensor topology",
            ));
        }
        let state = match snapshot {
            Some(commit) => Some(self.state_at(commit)?),
            None => None,
        };
        let manifest = match state.as_ref() {
            Some(state) => {
                self.load_checkpoint_with_id_at(state, snapshot, checkpoint)?
                    .1
            }
            None => self.load_checkpoint(checkpoint)?,
        };
        let tensor = manifest.tensor(tensor_name)?;
        let limits = Limits::default();
        let restored = self.restore_tensor(
            checkpoint,
            tensor,
            region,
            topology,
            &limits,
            state.as_ref(),
            snapshot,
            false,
        )?;
        let report = RestoredTensor {
            name: tensor.name.clone(),
            dtype: tensor.dtype.clone(),
            shape: restored.shape.clone(),
            full_shape: tensor.shape.clone(),
            full_logical_root: tensor.logical_root,
            restored_content_root: restored.restored_content_root,
            bytes: restored.bytes.len() as u64,
            shards_read: restored.shards_read,
            units_touched: restored.units_touched,
            full_root_verified: restored.full_root_verified,
            shard_roots_verified: restored.shard_roots_verified,
            topology_root: restored.topology_root,
            sha256: safetensors::sha256(&restored.bytes),
        };
        Ok(RestoredTensorBytes {
            bytes: restored.bytes,
            report,
        })
    }

    /// Restore a single tensor to memory (used by `checkpoint verify`).
    pub fn restore_checkpoint_bytes(
        &self,
        name: &str,
        options: &CheckpointRestoreOptions,
    ) -> Result<(Vec<u8>, CheckpointRestoreReport)> {
        let mut out = Vec::new();
        let report = self.restore_checkpoint(name, options, &mut out)?;
        Ok((out, report))
    }

    /// Restore one tensor to memory from a pinned snapshot.
    pub fn restore_checkpoint_bytes_at(
        &self,
        snapshot: Hash32,
        name: &str,
        options: &CheckpointRestoreOptions,
    ) -> Result<(Vec<u8>, CheckpointRestoreReport)> {
        let mut out = Vec::new();
        let report = self.restore_checkpoint_at(snapshot, name, options, &mut out)?;
        Ok((out, report))
    }

    /// Restore a complete checkpoint and emit a `weft.load.receipt.v1` record.
    ///
    /// The bytes are produced into `writer` only after the loader has decided
    /// the tensor is verified: a corrupt tile makes the restore fail before
    /// that tensor's bytes reach the writer. The receipt's set roots are
    /// computed from the produced bytes and from the manifest; a mismatch is a
    /// typed `Corrupt` error naming the tensor.
    ///
    /// `verification = Lazy` verifies the per-unit roots the format provides
    /// (tile roots for modes 15/17/18, shard roots for the checkpoint
    /// topology). `Full` additionally re-reads every stored shard through the
    /// verifying object path and re-checks its whole-content BLAKE3.
    pub fn restore_checkpoint_with_load_receipt<W: Write>(
        &self,
        name: &str,
        options: &CheckpointRestoreOptions,
        load: &crate::load_receipt::LoadReceiptOptions,
        writer: &mut W,
    ) -> Result<(CheckpointRestoreReport, crate::load_receipt::LoadReceipt)> {
        use crate::load_receipt::{LoadReceipt, LoadReceiptTensor, LoadVerification};

        if options.tensor.is_some() || options.region.is_some() {
            return Err(Error::invalid_argument(
                "a load receipt requires a complete checkpoint restore (no --tensor/--range)",
            ));
        }
        let started = std::time::Instant::now();
        let manifest = self.load_checkpoint(name)?;
        let manifest_root = manifest.root()?;
        let limits = Limits::default();

        // Restore into a private buffer so no unverified byte reaches the
        // caller's writer.
        let mut container = Vec::new();
        let lazy_tiles = load.verification == LoadVerification::Lazy;
        let report =
            self.restore_checkpoint_inner(None, None, name, options, lazy_tiles, &mut container)?;
        writer
            .write_all(&container)
            .map_err(|err| Error::io(format!("load receipt output: {err}")))?;

        // The restore wrote the header followed by the tensors in
        // `source_offset` order.
        let payload_bytes: u64 = report.tensors.iter().map(|tensor| tensor.bytes).sum();
        if payload_bytes > container.len() as u64 {
            return Err(Error::corrupt(
                "restored container is shorter than its tensor payloads",
            ));
        }
        let payload_start = container.len() as u64 - payload_bytes;
        let mut cursor = payload_start as usize;
        let mut rows = Vec::with_capacity(report.tensors.len());
        for tensor in &report.tensors {
            let end = cursor
                .checked_add(tensor.bytes as usize)
                .ok_or_else(|| Error::limit_exceeded("load receipt cursor overflows usize"))?;
            let slice = container.get(cursor..end).ok_or_else(|| {
                Error::corrupt("load receipt payload range is outside the container")
            })?;
            cursor = end;
            let dtype = TensorElementDtype::parse(&tensor.dtype)?;
            let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
                Error::unsupported_version(format!(
                    "load receipt tensor `{}` has no physical dtype",
                    tensor.name
                ))
            })?;
            let loaded_root = tc::logical_root(physical, &tensor.full_shape, slice, &limits)
                .map_err(|err| {
                    let err = Error::from(err);
                    Error::new(
                        err.class(),
                        format!(
                            "load receipt tensor `{}`: loaded bytes have no logical root: {}",
                            tensor.name,
                            err.message()
                        ),
                    )
                })?;
            if loaded_root != tensor.full_logical_root {
                return Err(Error::corrupt(format!(
                    "load receipt tensor `{}`: loaded root {loaded_root} does not match the \
                     checkpoint's attested root {}",
                    tensor.name, tensor.full_logical_root
                )));
            }
            rows.push(LoadReceiptTensor {
                name: tensor.name.clone(),
                dtype: tensor.dtype.clone(),
                shape: tensor.full_shape.clone(),
                attested_root: tensor.full_logical_root,
                loaded_root,
                content_root: tensor.restored_content_root,
                sha256: tensor.sha256,
                bytes: tensor.bytes,
                units_verified: tensor.units_touched,
            });
        }

        if load.verification == LoadVerification::Full {
            for tensor in &manifest.tensors {
                for shard in &tensor.shards {
                    let bytes = self
                        .read_object_uncounted(shard.object_id)
                        .map_err(|err| context(&tensor.name, shard, err))?;
                    if identity::content_hash(&bytes) != shard.content_root {
                        return Err(context(
                            &tensor.name,
                            shard,
                            Error::corrupt(
                                "full load verification: whole-shard content hash mismatch",
                            ),
                        ));
                    }
                }
            }
        }

        let attested = LoadReceipt::roster_root(
            &rows
                .iter()
                .map(|row| (row.name.clone(), row.attested_root))
                .collect::<Vec<_>>(),
        )?;
        let loaded = LoadReceipt::roster_root(
            &rows
                .iter()
                .map(|row| (row.name.clone(), row.loaded_root))
                .collect::<Vec<_>>(),
        )?;
        if attested != loaded {
            return Err(Error::corrupt(
                "load receipt set root does not match the attested set root",
            ));
        }
        let receipt = LoadReceipt {
            checkpoint: name.to_owned(),
            manifest_root,
            replica: load.replica.clone(),
            verification: load.verification,
            tensors: rows,
            loaded_set_root: loaded,
            attested_set_root: attested,
            bytes_produced: report.file_bytes,
            units_verified: report.units_touched,
            wall_ms: started.elapsed().as_millis() as u64,
            exact_source_file: report.exact_source_file,
            file_sha256: report.file_sha256,
        };
        Ok((report, receipt))
    }

    fn restore_tensor(
        &self,
        checkpoint: &str,
        tensor: &CheckpointTensor,
        region: Option<&Region>,
        topology: Option<&RestoreTopology>,
        limits: &Limits,
        state: Option<&State>,
        snapshot: Option<Hash32>,
        lazy_tiles: bool,
    ) -> Result<RestoredBytes> {
        let dtype = TensorElementDtype::parse(&tensor.dtype)?;
        let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
            Error::unsupported_version(format!(
                "tensor `{}` dtype `{}` has no physical layout",
                tensor.name, tensor.dtype
            ))
        })?;
        let element_bytes = physical.element_size() as usize;
        match region {
            None => {
                let mut shards = tensor.shards.clone();
                shards.sort_by_key(|shard| shard.index);
                let mut payloads = Vec::with_capacity(shards.len());
                let mut verified = 0usize;
                let mut units_touched = 0u64;
                for shard in &shards {
                    let (bytes, units) =
                        self.read_shard(checkpoint, tensor, shard, state, snapshot, lazy_tiles)?;
                    units_touched += units;
                    if bytes.len() as u64 != shard.byte_len {
                        return Err(context(
                            &tensor.name,
                            shard,
                            Error::corrupt(format!(
                                "shard payload is {} bytes but the manifest records {}",
                                bytes.len(),
                                shard.byte_len
                            )),
                        ));
                    }
                    if identity::content_hash(&bytes) != shard.content_root {
                        return Err(context(
                            &tensor.name,
                            shard,
                            Error::corrupt("shard content root mismatch"),
                        ));
                    }
                    let shard_shape =
                        tc::shard_shape(&tensor.shape, shard.axis, shard.shards, shard.index)?;
                    let shard_root = tc::logical_root(physical, &shard_shape, &bytes, limits)
                        .map_err(|err| context(&tensor.name, shard, Error::from(err)))?;
                    if shard_root != shard.logical_root {
                        return Err(context(
                            &tensor.name,
                            shard,
                            Error::corrupt(format!(
                                "shard logical root {shard_root} does not match the checkpoint's \
                                 {}",
                                shard.logical_root
                            )),
                        ));
                    }
                    verified += 1;
                    payloads.push(bytes);
                }
                let bytes = PhysicalTensor::sharded(
                    physical,
                    &tensor.shape,
                    tensor.shards[0].axis,
                    payloads,
                    limits,
                )?
                .assemble_with_limits(limits)?;
                if bytes.len() as u64 != tensor.element_bytes {
                    return Err(Error::corrupt(format!(
                        "checkpoint tensor `{}` reassembled to {} bytes, manifest records {}",
                        tensor.name,
                        bytes.len(),
                        tensor.element_bytes
                    )));
                }
                let content_root = identity::content_hash(&bytes);
                if content_root != tensor.content_root {
                    return Err(Error::corrupt(format!(
                        "checkpoint tensor `{}` content root mismatch after reassembly",
                        tensor.name
                    )));
                }
                let root = tc::logical_root(physical, &tensor.shape, &bytes, limits)?;
                if root != tensor.logical_root {
                    return Err(Error::corrupt(format!(
                        "checkpoint tensor `{}` logical root {root} does not match the \
                         manifest's {}",
                        tensor.name, tensor.logical_root
                    )));
                }
                let topology_root = match topology {
                    Some(topology) => {
                        let root = verify_layout_roundtrip(
                            physical,
                            &tensor.shape,
                            &bytes,
                            topology,
                            limits,
                        )
                        .map_err(|err| {
                            Error::new(
                                err.class(),
                                format!("checkpoint tensor `{}`: {}", tensor.name, err.message()),
                            )
                        })?;
                        if root != tensor.logical_root {
                            return Err(Error::corrupt(format!(
                                "checkpoint tensor `{}` logical root {root} under the requested \
                                 topology does not match the manifest's {}",
                                tensor.name, tensor.logical_root
                            )));
                        }
                        Some(root)
                    }
                    None => None,
                };
                Ok(RestoredBytes {
                    bytes,
                    shape: tensor.shape.clone(),
                    shards_read: verified,
                    units_touched: units_touched.max(verified as u64),
                    full_root_verified: true,
                    shard_roots_verified: true,
                    restored_content_root: content_root,
                    topology_root,
                })
            }
            Some(region) => {
                region.validate(&tensor.shape)?;
                let extent = region.extent();
                let byte_len = tc::payload_len(checked_product(&extent)?, physical)?;
                let byte_len = usize::try_from(byte_len)
                    .map_err(|_| Error::limit_exceeded("region payload does not fit in memory"))?;
                let mut out = vec![0u8; byte_len];
                let axis = tensor.shards[0].axis as usize;
                let plane = tensor.shape[axis] / u64::from(tensor.shards[0].shards);
                let mut shards_read = 0usize;
                let mut units_touched = 0u64;
                for shard in &tensor.shards {
                    let slab_start = u64::from(shard.index) * plane;
                    let slab_end = slab_start + plane;
                    let lo = region.start[axis].max(slab_start);
                    let hi = region.end[axis].min(slab_end);
                    if lo >= hi {
                        continue;
                    }
                    let mut local_start = region.start.clone();
                    let mut local_end = region.end.clone();
                    local_start[axis] = lo - slab_start;
                    local_end[axis] = hi - slab_start;
                    let local = Region::new(local_start, local_end);
                    // Confirm the shard entry still resolves to the recorded
                    // object, then take the region through the stored object.
                    // Mode 10 stores a q8_0 byte transport addressed in byte
                    // coordinates; mode 15 stores the shard's real dtype and
                    // shape and is addressed in logical coordinates.
                    self.verify_shard_entry(checkpoint, tensor, shard, state, snapshot)?;
                    let store_meta = match state {
                        Some(state) => self.tensor_metadata_at(state, shard.object_id)?,
                        None => self.tensor_metadata(shard.object_id)?,
                    };
                    let local_extent = local.extent();
                    let expected_len = tc::payload_len(checked_product(&local_extent)?, physical)?;
                    let rank = local_extent.len();
                    // Only the mode-10 `q8_0` byte transport addresses the
                    // stored object in byte coordinates (its last extent is the
                    // logical last extent times the element size). Every other
                    // shard strategy is stored under the logical dtype/shape and
                    // is addressed in logical coordinates.
                    let byte_addressed =
                        store_meta.dtype == crate::representation::TensorElementDtype::Q8_0;
                    let read = if !byte_addressed {
                        self.read_tensor_region(shard.object_id, &local)
                            .map_err(|err| context(&tensor.name, shard, err))?
                    } else {
                        let mut byte_start = local.start.clone();
                        let mut byte_end = local.end.clone();
                        let last = rank - 1;
                        let esize = element_bytes as u64;
                        byte_start[last] = byte_start[last]
                            .checked_mul(esize)
                            .ok_or_else(|| Error::limit_exceeded("region byte offset overflows"))?;
                        byte_end[last] = byte_end[last]
                            .checked_mul(esize)
                            .ok_or_else(|| Error::limit_exceeded("region byte offset overflows"))?;
                        let byte_region = Region::new(byte_start, byte_end);
                        self.read_tensor_region(shard.object_id, &byte_region)
                            .map_err(|err| context(&tensor.name, shard, err))?
                    };
                    if read.bytes.len() as u64 != expected_len {
                        return Err(context(
                            &tensor.name,
                            shard,
                            Error::corrupt(format!(
                                "region read returned {} bytes, expected {expected_len}",
                                read.bytes.len()
                            )),
                        ));
                    }
                    // Destination coordinates are relative to the region.
                    let mut dst_start = vec![0u64; extent.len()];
                    dst_start[axis] = lo - region.start[axis];
                    let src_start = vec![0u64; rank];
                    copy_box(
                        &mut out,
                        &extent,
                        &dst_start,
                        &read.bytes,
                        &local_extent,
                        &src_start,
                        &local_extent,
                        element_bytes,
                    )?;
                    shards_read += 1;
                    units_touched += read.units_touched as u64;
                }
                let content_root = identity::content_hash(&out);
                Ok(RestoredBytes {
                    bytes: out,
                    shape: extent,
                    shards_read,
                    units_touched,
                    full_root_verified: false,
                    shard_roots_verified: false,
                    restored_content_root: content_root,
                    topology_root: None,
                })
            }
        }
    }

    fn read_shard(
        &self,
        checkpoint: &str,
        tensor: &CheckpointTensor,
        shard: &CheckpointShard,
        state: Option<&State>,
        snapshot: Option<Hash32>,
        lazy_tiles: bool,
    ) -> Result<(Vec<u8>, u64)> {
        let id = self.verify_shard_entry(checkpoint, tensor, shard, state, snapshot)?;
        // Lazy verification: a mode-15/17/18 object is read through the range
        // path, which verifies every tile root but deliberately does not
        // re-hash the whole frame. Every other representation has no
        // independent unit, so it takes the whole-object verifying read.
        if lazy_tiles {
            let meta = match state {
                Some(state) => self.tensor_metadata_at(state, id),
                None => self.tensor_metadata(id),
            }
            .map_err(|err| context(&tensor.name, shard, err))?;
            if matches!(
                meta.strategy.as_str(),
                "tensor-tiled" | "float-tiled" | "float-tiled-streams"
            ) {
                let shape = meta.shape.clone();
                let region = Region::new(vec![0u64; shape.len()], shape);
                let read = self
                    .read_tensor_region(id, &region)
                    .map_err(|err| context(&tensor.name, shard, err))?;
                return Ok((read.bytes, read.units_touched));
            }
        }
        let bytes = match state {
            Some(state) => {
                self.tensor_metadata_at(state, id)
                    .map_err(|err| context(&tensor.name, shard, err))?;
                self.read_object_at(snapshot, id)
                    .map_err(|err| context(&tensor.name, shard, err))?
            }
            None => self
                .read_tensor(id)
                .map_err(|err| context(&tensor.name, shard, err))?,
        };
        Ok((bytes, 1))
    }

    fn verify_shard_entry(
        &self,
        checkpoint: &str,
        tensor: &CheckpointTensor,
        shard: &CheckpointShard,
        state: Option<&State>,
        _snapshot: Option<Hash32>,
    ) -> Result<Hash32> {
        let components = [
            CHECKPOINT_ROOT.as_bytes(),
            checkpoint.as_bytes(),
            shard.object_name.as_bytes(),
        ];
        let id = match state {
            Some(state) => resolve_in_state(state, &components),
            None => self.resolve_components(&components),
        }
        .map_err(|err| context(&tensor.name, shard, err))?;
        if id != shard.object_id {
            return Err(context(
                &tensor.name,
                shard,
                Error::corrupt(format!(
                    "checkpoint shard entry resolves to object {id}, manifest records {}",
                    shard.object_id
                )),
            ));
        }
        Ok(id)
    }
}

/// Resolve pre-split byte components against an explicit state.
fn resolve_in_state(state: &State, components: &[&[u8]]) -> Result<Hash32> {
    let mut current = State::ROOT_ID;
    for component in components {
        crate::state::validate_name_bytes(component)?;
        let object = state
            .objects
            .get(&current)
            .ok_or_else(|| Error::not_found("no such parent object"))?;
        if object.kind != ObjectKind::Dir {
            return Err(Error::not_dir("path component is not a directory"));
        }
        current = state.lookup(current, component)?;
    }
    Ok(current)
}

fn verify_layout_roundtrip(
    dtype: weftcodec::TensorDtype,
    shape: &[u64],
    row_major: &[u8],
    topology: &RestoreTopology,
    limits: &Limits,
) -> Result<Hash32> {
    let (reassembled, layout) = match topology {
        RestoreTopology::RowMajor => (row_major.to_vec(), "row-major".to_owned()),
        RestoreTopology::ColumnMajor => {
            let physical = tc::column_major_bytes(dtype, shape, row_major, limits)?;
            let assembled = PhysicalTensor::column_major(dtype, shape, physical, limits)?
                .assemble_with_limits(limits)?;
            (assembled, "column-major".to_owned())
        }
        RestoreTopology::Blocked { block_shape } => {
            let blocks = tc::blocked_bytes(dtype, shape, block_shape, row_major, limits)?;
            let assembled = PhysicalTensor::blocked(dtype, shape, block_shape, blocks, limits)?
                .assemble_with_limits(limits)?;
            (assembled, format!("blocked{block_shape:?}"))
        }
        RestoreTopology::Sharded { axis, shards } => {
            let payloads = tc::split_shards(dtype, shape, *axis, *shards, row_major, limits)?;
            let assembled = PhysicalTensor::sharded(dtype, shape, *axis, payloads, limits)?
                .assemble_with_limits(limits)?;
            (assembled, format!("sharded({axis},{shards})"))
        }
    };
    if reassembled != row_major {
        return Err(Error::corrupt(format!(
            "re-expressing the tensor as {layout} did not reproduce its canonical row-major bytes"
        )));
    }
    tc::logical_root(dtype, shape, &reassembled, limits).map_err(Error::from)
}

/// Extract the source container's `__metadata__` object from exact header bytes.
fn source_metadata(header_bytes: &[u8]) -> Option<serde_json::Value> {
    if header_bytes.len() < 9 {
        return None;
    }
    let header_len = u64::from_le_bytes(header_bytes[0..8].try_into().ok()?) as usize;
    let end = 8usize.checked_add(header_len)?;
    if end > header_bytes.len() {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(&header_bytes[8..end]).ok()?;
    parsed
        .as_object()
        .and_then(|map| map.get("__metadata__"))
        .cloned()
}

// ---------------------------------------------------------------------------
// Verification helper
// ---------------------------------------------------------------------------

/// Full verification of a checkpoint: every shard decoded, every per-shard and
/// per-tensor logical root recomputed. Returns the number of tensors verified.
pub fn verify_checkpoint(store: &Store, name: &str) -> Result<usize> {
    verify_checkpoint_inner(store, None, name)
}

/// Full verification of a checkpoint as it exists at a pinned snapshot.
pub fn verify_checkpoint_at(store: &Store, snapshot: Hash32, name: &str) -> Result<usize> {
    verify_checkpoint_inner(store, Some(snapshot), name)
}

fn verify_checkpoint_inner(store: &Store, snapshot: Option<Hash32>, name: &str) -> Result<usize> {
    let manifest = match snapshot {
        Some(snapshot) => {
            let state = store.state_at(snapshot)?;
            store
                .load_checkpoint_with_id_at(&state, Some(snapshot), name)?
                .1
        }
        None => store.checkpoint_manifest(name)?,
    };
    for tensor in &manifest.tensors {
        let options = CheckpointRestoreOptions {
            tensor: Some(tensor.name.clone()),
            region: None,
            topology: None,
        };
        let (_, report) = match snapshot {
            Some(snapshot) => store.restore_checkpoint_bytes_at(snapshot, name, &options)?,
            None => store.restore_checkpoint_bytes(name, &options)?,
        };
        let restored = report
            .tensors
            .first()
            .ok_or_else(|| Error::corrupt("restore report has no tensor"))?;
        if !restored.full_root_verified || !restored.shard_roots_verified {
            return Err(Error::corrupt(format!(
                "checkpoint tensor `{}` was not fully verified",
                tensor.name
            )));
        }
        if restored.bytes != tensor.element_bytes {
            return Err(Error::corrupt(format!(
                "checkpoint tensor `{}` restored to the wrong length",
                tensor.name
            )));
        }
    }
    Ok(manifest.tensors.len())
}
