//! On-disk byte formats: superblock, commit records, file manifests, and
//! checkpoint state manifests.
//!
//! Normative sources: `WEFT-MODEL.md` §2.1–§2.4 and `WEFTCODEC-FORMAT.md` §1.
//! All integers are little-endian; all structured bytes are canonical WCB.

use std::collections::BTreeMap;

use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::representation::TensorElementDtype;
use crate::state::{validate_object_record, EntryRecord, ObjectRecord, Op, RelationRecord, State};
use crate::wcb_util as wu;

/// `FORMAT` file contents.
pub const FORMAT_CONTENTS: &str = "weftfs-store 1\n";
/// Superblock file size: two 4096-byte slots.
pub const SUPERBLOCK_BYTES: usize = 8192;
/// One superblock slot.
pub const SLOT_BYTES: usize = 4096;
/// Superblock magic.
pub const SUPERBLOCK_MAGIC: &[u8; 4] = b"WFSB";
/// Commit file magic.
pub const COMMIT_MAGIC: &[u8; 4] = b"WFC1";
/// Store format major.
pub const FORMAT_MAJOR: u16 = 1;
/// Store format minor.
pub const FORMAT_MINOR: u16 = 0;
/// Commit schema string.
pub const COMMIT_SCHEMA: &str = "weftfs.commit.v1";
/// File manifest schema string (see `WEFT-MODEL.md` §2.3).
pub const MANIFEST_SCHEMA: &str = "weft.manifest.v1";
/// Legacy v1 spelling accepted on read (see `docs/store-status.md` §3.1).
pub const MANIFEST_SCHEMA_V1_LEGACY: &str = "weftfs.manifest.v1";

/// Experimental v2 representation manifest. The schema string follows the
/// normative `weft.` spelling used by the frozen v1 writer; there is exactly
/// one v2 spelling, and no alias.
pub const MANIFEST_SCHEMA_V2: &str = "weft.manifest.v2";
/// Checkpoint state manifest schema string.
pub const STATE_SCHEMA: &str = "weftfs.state.v1";

/// One superblock slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuperblockSlot {
    pub generation: u64,
    pub commit: Hash32,
    pub commit_len: u64,
    pub timestamp_ns: i64,
}

impl SuperblockSlot {
    /// Encode one 4096-byte slot.
    pub fn encode(&self) -> [u8; SLOT_BYTES] {
        let mut out = [0u8; SLOT_BYTES];
        out[0..4].copy_from_slice(SUPERBLOCK_MAGIC);
        out[4..6].copy_from_slice(&FORMAT_MAJOR.to_le_bytes());
        out[6..8].copy_from_slice(&FORMAT_MINOR.to_le_bytes());
        out[8..16].copy_from_slice(&self.generation.to_le_bytes());
        out[16..48].copy_from_slice(self.commit.as_bytes());
        out[48..56].copy_from_slice(&self.commit_len.to_le_bytes());
        out[56..64].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        let crc = crc32c::crc32c(&out[0..64]);
        out[64..68].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Decode and validate one 4096-byte slot.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != SLOT_BYTES {
            return Err(Error::malformed("superblock slot must be 4096 bytes"));
        }
        if &bytes[0..4] != SUPERBLOCK_MAGIC {
            return Err(Error::malformed("bad superblock magic"));
        }
        let major = u16::from_le_bytes([bytes[4], bytes[5]]);
        let minor = u16::from_le_bytes([bytes[6], bytes[7]]);
        if major != FORMAT_MAJOR || minor != FORMAT_MINOR {
            return Err(Error::unsupported_version(format!(
                "superblock version {major}.{minor}"
            )));
        }
        let stored_crc = u32::from_le_bytes([bytes[64], bytes[65], bytes[66], bytes[67]]);
        let actual_crc = crc32c::crc32c(&bytes[0..64]);
        if stored_crc != actual_crc {
            return Err(Error::corrupt("superblock slot CRC mismatch"));
        }
        if bytes[68..].iter().any(|byte| *byte != 0) {
            return Err(Error::malformed("superblock slot padding is not zero"));
        }
        let generation = u64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes"));
        let mut commit_bytes = [0u8; 32];
        commit_bytes.copy_from_slice(&bytes[16..48]);
        let commit = Hash32::from_bytes(commit_bytes);
        let commit_len = u64::from_le_bytes(bytes[48..56].try_into().expect("8 bytes"));
        let timestamp_ns = i64::from_le_bytes(bytes[56..64].try_into().expect("8 bytes"));
        if generation == 0 {
            return Err(Error::malformed("superblock generation must be positive"));
        }
        if commit == Hash32::ZERO {
            return Err(Error::malformed("superblock commit hash is zero"));
        }
        if commit_len == 0 {
            return Err(Error::malformed("superblock commit length is zero"));
        }
        Ok(SuperblockSlot {
            generation,
            commit,
            commit_len,
            timestamp_ns,
        })
    }
}

/// Decode a full superblock into its two slot outcomes.
pub fn decode_superblock(bytes: &[u8]) -> Result<[Result<SuperblockSlot>; 2]> {
    if bytes.len() != SUPERBLOCK_BYTES {
        return Err(Error::malformed(format!(
            "superblock must be {SUPERBLOCK_BYTES} bytes, got {}",
            bytes.len()
        )));
    }
    let first = SuperblockSlot::decode(&bytes[0..SLOT_BYTES]);
    let second = SuperblockSlot::decode(&bytes[SLOT_BYTES..]);
    Ok([first, second])
}

/// A commit record (`WEFT-MODEL.md` §2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub generation: u64,
    pub parent: Option<Hash32>,
    pub timestamp_ns: i64,
    pub ops: Vec<Op>,
    pub checkpoint: Option<Hash32>,
}

impl Commit {
    const KEYS: &'static [&'static str] = &[
        "schema",
        "generation",
        "parent",
        "timestamp_ns",
        "ops",
        "checkpoint",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(COMMIT_SCHEMA.to_owned())),
            ("generation", Value::U64(self.generation)),
            ("parent", wu::opt_hash_value(self.parent)),
            ("timestamp_ns", Value::Timestamp(self.timestamp_ns)),
            (
                "ops",
                Value::Array(self.ops.iter().map(Op::to_value).collect()),
            ),
            ("checkpoint", wu::opt_hash_value(self.checkpoint)),
        ])
    }

    /// Canonical WCB bytes of the commit record.
    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    /// `H("weft/store/commit/v1", WCB(commit))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::commit_id(&self.encode()?))
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != COMMIT_SCHEMA {
            return Err(Error::unknown_schema(format!(
                "commit schema `{schema}` is not `{COMMIT_SCHEMA}`"
            )));
        }
        let ops = wu::get_array(map, "ops")?
            .iter()
            .map(Op::from_wcb)
            .collect::<Result<Vec<_>>>()?;
        Ok(Commit {
            generation: wu::get_u64(map, "generation")?,
            parent: wu::get_opt_hash(map, "parent")?,
            timestamp_ns: wu::get_timestamp(map, "timestamp_ns")?,
            ops,
            checkpoint: wu::get_opt_hash(map, "checkpoint")?,
        })
    }
}

/// Encode a commit file: `WFC1 || u64le len || WCB || u32le CRC32C(WCB)`.
pub fn encode_commit_file(wcb_bytes: &[u8]) -> Result<Vec<u8>> {
    let len = u64::try_from(wcb_bytes.len())
        .map_err(|_| Error::limit_exceeded("commit record exceeds u64 length"))?;
    let crc = crc32c::crc32c(wcb_bytes);
    let mut out = Vec::with_capacity(4 + 8 + wcb_bytes.len() + 4);
    out.extend_from_slice(COMMIT_MAGIC);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(wcb_bytes);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// Decode a commit file, returning `(wcb_bytes, commit)`. Validates magic,
/// exact length, and the CRC over the WCB bytes. The store then computes and
/// compares the commit hash.
pub fn decode_commit_file(bytes: &[u8]) -> Result<(Vec<u8>, Commit)> {
    if bytes.len() < 16 {
        return Err(Error::corrupt("commit file is truncated"));
    }
    if &bytes[0..4] != COMMIT_MAGIC {
        return Err(Error::malformed("bad commit file magic"));
    }
    let len = u64::from_le_bytes(bytes[4..12].try_into().expect("8 bytes"));
    let len_usize =
        usize::try_from(len).map_err(|_| Error::limit_exceeded("commit length exceeds usize"))?;
    let expected = 12usize
        .checked_add(len_usize)
        .and_then(|n| n.checked_add(4))
        .ok_or_else(|| Error::malformed("commit length overflows"))?;
    if expected != bytes.len() {
        return Err(Error::corrupt(format!(
            "commit file length {} does not match declared {}",
            bytes.len(),
            expected
        )));
    }
    let wcb = &bytes[12..12 + len_usize];
    let stored_crc =
        u32::from_le_bytes(bytes[12 + len_usize..].try_into().expect("exactly 4 bytes"));
    if stored_crc != crc32c::crc32c(wcb) {
        return Err(Error::corrupt("commit record CRC mismatch"));
    }
    let value = wu::decode(wcb)?;
    let commit = Commit::from_wcb(&value)?;
    Ok((wcb.to_vec(), commit))
}

/// One chunk reference in a file manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkRef {
    pub hash: Hash32,
    pub len: u64,
}

/// How the manifest's logical content is physically represented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepresentationKind {
    /// The logical bytes are the concatenated chunk blobs (`chunks`).
    Raw,
    /// The logical bytes are decoded from a WeftCodec WFT1 frame blob.
    Codec,
    /// The logical bytes are decoded from a WeftTensor frame (modes 10-14).
    Tensor,
}

impl RepresentationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RepresentationKind::Raw => "raw",
            RepresentationKind::Codec => "codec",
            RepresentationKind::Tensor => "tensor",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "raw" => Ok(RepresentationKind::Raw),
            "codec" => Ok(RepresentationKind::Codec),
            "tensor" => Ok(RepresentationKind::Tensor),
            other => Err(Error::malformed(format!(
                "unknown representation kind `{other}`"
            ))),
        }
    }
}

/// Tensor-specific fields of a `kind:"tensor"` representation manifest.
///
/// `layout` is the canonical WCB map `weft.tensor.layout.v1` from
/// `spec/WEFT-TENSOR.md` §4. It is kept as a `Value` so the store can validate
/// the exact canonical record (including the additive quantized dtype names)
/// without re-defining the descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorRepresentation {
    /// 10 `TENSOR_SHARDED`, 11 `WEIGHT_DELTA`, 12 `KV_CACHE`,
    /// 13 `ACTIVATION_STREAM`, 14 `SEMANTIC_DELTA`.
    pub tensor_mode: u32,
    pub dtype: TensorElementDtype,
    pub shape: Vec<u64>,
    pub logical_root: Hash32,
    pub layout: Value,
}

impl TensorRepresentation {
    /// Canonical number of the frame mode a strategy requires.
    pub fn expected_mode(strategy: &str) -> Option<u32> {
        match strategy {
            "tensor-sharded" => Some(10),
            "weight-delta" => Some(11),
            "kv-cache" => Some(12),
            "activation" => Some(13),
            "semantic-delta" => Some(14),
            _ => None,
        }
    }

    /// Canonical strategy name for a frame mode.
    pub fn strategy_for_mode(mode: u32) -> Option<&'static str> {
        match mode {
            10 => Some("tensor-sharded"),
            11 => Some("weight-delta"),
            12 => Some("kv-cache"),
            13 => Some("activation"),
            14 => Some("semantic-delta"),
            _ => None,
        }
    }

    /// True when the mode carries a base-frame dependency (11/14).
    pub fn mode_has_dependency(mode: u32) -> bool {
        matches!(mode, 11 | 14)
    }
}

/// Physical representation descriptor carried by a v2 manifest.
///
/// `frame` is a manifest-domain blob id: the blob under `manifests/<ab>/<hex>`
/// whose bytes are the complete WFT1 frame. `dependencies` are the **content
/// roots** (manifest ids) of the base files a decoder needs, in the frame's
/// dependency-table order. `strategy` is informational and is cross-checked
/// against the frame header's payload mode on every decode. `tensor` is present
/// exactly when `kind` is [`RepresentationKind::Tensor`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Representation {
    pub kind: RepresentationKind,
    pub frame: Option<Hash32>,
    pub dependencies: Vec<Hash32>,
    pub strategy: String,
    pub decoded_size: u64,
    pub tensor: Option<TensorRepresentation>,
}

impl Representation {
    const KEYS: &'static [&'static str] =
        &["kind", "frame", "dependencies", "strategy", "decoded_size"];
    const KEYS_TENSOR: &'static [&'static str] = &[
        "kind",
        "frame",
        "tensor_mode",
        "dtype",
        "shape",
        "logical_root",
        "layout",
        "dependencies",
        "decoded_size",
        "strategy",
    ];

    pub fn to_value(&self) -> Value {
        let dependencies = self
            .dependencies
            .iter()
            .map(|hash| wu::hash_value(*hash))
            .collect();
        if self.kind == RepresentationKind::Tensor {
            let tensor = self.tensor.clone().unwrap_or(TensorRepresentation {
                tensor_mode: 0,
                dtype: TensorElementDtype::U8,
                shape: Vec::new(),
                logical_root: Hash32::ZERO,
                layout: Value::Null,
            });
            let shape = tensor.shape.iter().map(|dim| Value::U64(*dim)).collect();
            return wu::map([
                ("kind", Value::Str(self.kind.as_str().to_owned())),
                ("frame", wu::opt_hash_value(self.frame)),
                ("tensor_mode", Value::U64(u64::from(tensor.tensor_mode))),
                ("dtype", Value::Str(tensor.dtype.as_str().to_owned())),
                ("shape", Value::Array(shape)),
                ("logical_root", wu::hash_value(tensor.logical_root)),
                ("layout", tensor.layout),
                ("dependencies", Value::Array(dependencies)),
                ("decoded_size", Value::U64(self.decoded_size)),
                ("strategy", Value::Str(self.strategy.clone())),
            ]);
        }
        wu::map([
            ("kind", Value::Str(self.kind.as_str().to_owned())),
            ("frame", wu::opt_hash_value(self.frame)),
            ("dependencies", Value::Array(dependencies)),
            ("strategy", Value::Str(self.strategy.clone())),
            ("decoded_size", Value::U64(self.decoded_size)),
        ])
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        let kind_text = wu::get_text(map, "kind")?;
        let kind = RepresentationKind::parse(&kind_text)?;
        if kind == RepresentationKind::Tensor {
            return Self::tensor_from_wcb(map);
        }
        wu::check_keys(map, Self::KEYS)?;
        let frame = wu::get_opt_hash(map, "frame")?;
        let dependencies = parse_dependencies(map)?;
        let strategy = wu::get_text(map, "strategy")?;
        let decoded_size = wu::get_u64(map, "decoded_size")?;
        match kind {
            RepresentationKind::Raw => {
                if frame.is_some() {
                    return Err(Error::malformed("raw representation must not name a frame"));
                }
                if !dependencies.is_empty() {
                    return Err(Error::malformed(
                        "raw representation must not have dependencies",
                    ));
                }
                if strategy != "raw" {
                    return Err(Error::malformed(
                        "raw representation strategy must be `raw`",
                    ));
                }
            }
            RepresentationKind::Codec => {
                match frame {
                    Some(hash) if hash != Hash32::ZERO => {}
                    _ => {
                        return Err(Error::malformed(
                            "codec representation requires a non-zero frame id",
                        ))
                    }
                }
                match strategy.as_str() {
                    "zstd" => {
                        if !dependencies.is_empty() {
                            return Err(Error::malformed(
                                "zstd representation must not have dependencies",
                            ));
                        }
                    }
                    "delta" | "reference" => {
                        if dependencies.len() != 1 {
                            return Err(Error::malformed(
                                "delta/reference representation requires exactly one dependency",
                            ));
                        }
                    }
                    other => {
                        return Err(Error::malformed(format!(
                            "unknown codec strategy `{other}`"
                        )))
                    }
                }
            }
            RepresentationKind::Tensor => unreachable!("handled above"),
        }
        Ok(Representation {
            kind,
            frame,
            dependencies,
            strategy,
            decoded_size,
            tensor: None,
        })
    }

    fn tensor_from_wcb(map: &std::collections::BTreeMap<String, Value>) -> Result<Self> {
        wu::check_keys(map, Self::KEYS_TENSOR)?;
        let frame = match wu::get_opt_hash(map, "frame")? {
            Some(hash) if hash != Hash32::ZERO => hash,
            _ => {
                return Err(Error::malformed(
                    "tensor representation requires a non-zero frame id",
                ))
            }
        };
        let tensor_mode_u64 = wu::get_u64(map, "tensor_mode")?;
        let tensor_mode = u32::try_from(tensor_mode_u64)
            .map_err(|_| Error::malformed("tensor_mode does not fit u32"))?;
        let strategy = wu::get_text(map, "strategy")?;
        let expected = TensorRepresentation::expected_mode(&strategy)
            .ok_or_else(|| Error::malformed(format!("unknown tensor strategy `{strategy}`")))?;
        if expected != tensor_mode {
            return Err(Error::malformed(format!(
                "tensor strategy `{strategy}` requires tensor_mode {expected}, got {tensor_mode}"
            )));
        }
        let dtype = TensorElementDtype::parse(&wu::get_text(map, "dtype")?)?;
        let mut shape = Vec::new();
        for item in wu::get_array(map, "shape")? {
            match item {
                Value::U64(dim) => shape.push(*dim),
                _ => return Err(Error::malformed("tensor shape must be an array of u64")),
            }
        }
        weftcodec::tensor::validate_shape(&shape, &weftcodec::Limits::default())?;
        let logical_root = wu::get_hash(map, "logical_root")?;
        let layout = wu::field(map, "layout")?.clone();
        if !matches!(layout, Value::Map(_)) {
            return Err(Error::malformed(
                "tensor layout must be a canonical WCB map",
            ));
        }
        let dependencies = parse_dependencies(map)?;
        let decoded_size = wu::get_u64(map, "decoded_size")?;
        if TensorRepresentation::mode_has_dependency(tensor_mode) {
            if dependencies.len() != 1 {
                return Err(Error::malformed(
                    "weight-delta/semantic-delta representation requires exactly one dependency",
                ));
            }
        } else if !dependencies.is_empty() {
            return Err(Error::malformed(
                "this tensor representation must not have dependencies",
            ));
        }
        Ok(Representation {
            kind: RepresentationKind::Tensor,
            frame: Some(frame),
            dependencies,
            strategy,
            decoded_size,
            tensor: Some(TensorRepresentation {
                tensor_mode,
                dtype,
                shape,
                logical_root,
                layout,
            }),
        })
    }
}

fn parse_dependencies(map: &std::collections::BTreeMap<String, Value>) -> Result<Vec<Hash32>> {
    let mut dependencies = Vec::new();
    for item in wu::get_array(map, "dependencies")? {
        let bytes = match item {
            Value::Bytes(bytes) => bytes,
            _ => {
                return Err(Error::malformed(
                    "representation dependency must be bytes32",
                ))
            }
        };
        dependencies.push(Hash32::from_slice(bytes).map_err(Error::from)?);
    }
    Ok(dependencies)
}

/// A file content manifest (`WEFT-MODEL.md` §2.3).
///
/// `representation` is `None` for the frozen v1 schema (written whenever the
/// store is in `RepresentationMode::Raw`) and `Some` for the experimental v2
/// representation schema. A v1 manifest must never contain a representation
/// key; a v2 manifest must always contain one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub size: u64,
    pub chunk_size: u64,
    pub chunks: Vec<ChunkRef>,
    pub content_hash: Hash32,
    pub representation: Option<Representation>,
}

/// Largest accepted chunk size in v1 (defensive bound; v1 writes 64 KiB).
pub const MAX_CHUNK_SIZE: u64 = 16 * 1024 * 1024;

impl Manifest {
    const KEYS_V1: &'static [&'static str] =
        &["schema", "size", "chunk_size", "chunks", "content_hash"];
    const KEYS_V2: &'static [&'static str] = &[
        "schema",
        "size",
        "chunk_size",
        "chunks",
        "content_hash",
        "representation",
    ];

    pub fn to_value(&self) -> Value {
        let chunks: Vec<Value> = self
            .chunks
            .iter()
            .map(|chunk| {
                wu::map([
                    ("hash", wu::hash_value(chunk.hash)),
                    ("len", Value::U64(chunk.len)),
                ])
            })
            .collect();
        match &self.representation {
            // The frozen v1 map: byte-for-byte the pre-representation writer.
            None => wu::map([
                ("schema", Value::Str(MANIFEST_SCHEMA.to_owned())),
                ("size", Value::U64(self.size)),
                ("chunk_size", Value::U64(self.chunk_size)),
                ("chunks", Value::Array(chunks)),
                ("content_hash", wu::hash_value(self.content_hash)),
            ]),
            Some(representation) => wu::map([
                ("schema", Value::Str(MANIFEST_SCHEMA_V2.to_owned())),
                ("size", Value::U64(self.size)),
                ("chunk_size", Value::U64(self.chunk_size)),
                ("chunks", Value::Array(chunks)),
                ("content_hash", wu::hash_value(self.content_hash)),
                ("representation", representation.to_value()),
            ]),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::manifest_id(&self.encode()?))
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        let schema = wu::get_text(map, "schema")?;
        // The normative v1 writer emits `weft.manifest.v1`; the older
        // `weftfs.manifest.v1` spelling is accepted on read. The experimental
        // v2 representation schema is `weft.manifest.v2`, with no alias. Any
        // other schema fails with `UnsupportedVersion`. See
        // docs/store-status.md §3.1.
        let (v2, keys): (bool, &[&str]) = match schema.as_str() {
            MANIFEST_SCHEMA | MANIFEST_SCHEMA_V1_LEGACY => (false, Self::KEYS_V1),
            MANIFEST_SCHEMA_V2 => (true, Self::KEYS_V2),
            other => {
                return Err(Error::unsupported_version(format!(
                    "manifest schema `{other}` is not a supported v1/v2 schema"
                )))
            }
        };
        wu::check_keys(map, keys)?;
        let size = wu::get_u64(map, "size")?;
        let chunk_size = wu::get_u64(map, "chunk_size")?;
        if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
            return Err(Error::malformed("manifest chunk_size is out of range"));
        }
        let representation = if v2 {
            Some(Representation::from_wcb(wu::field(map, "representation")?)?)
        } else {
            None
        };
        let frame_backed = representation
            .as_ref()
            .map(|rep| {
                rep.kind == RepresentationKind::Codec || rep.kind == RepresentationKind::Tensor
            })
            .unwrap_or(false);
        let mut chunks = Vec::new();
        let mut total: u64 = 0;
        for item in wu::get_array(map, "chunks")? {
            let chunk_map = wu::as_map(item)?;
            wu::check_keys(chunk_map, &["hash", "len"])?;
            let len = wu::get_u64(chunk_map, "len")?;
            if len == 0 {
                return Err(Error::corrupt("manifest chunk has zero length"));
            }
            if len > chunk_size {
                return Err(Error::corrupt("manifest chunk exceeds chunk_size"));
            }
            total = total
                .checked_add(len)
                .ok_or_else(|| Error::corrupt("manifest size overflows"))?;
            chunks.push(ChunkRef {
                hash: wu::get_hash(chunk_map, "hash")?,
                len,
            });
        }
        if frame_backed {
            // A codec or tensor representation carries its bytes inside the
            // frame; any chunk entries here would be a second, conflicting
            // physical layout.
            if !chunks.is_empty() {
                return Err(Error::corrupt(
                    "frame-backed manifest must not list chunk references",
                ));
            }
        } else {
            if total != size {
                return Err(Error::corrupt(format!(
                    "manifest chunk lengths sum to {total}, declared size is {size}"
                )));
            }
            if size == 0 && !chunks.is_empty() {
                return Err(Error::corrupt("empty file manifest must have no chunks"));
            }
            if size > 0 && chunks.is_empty() {
                return Err(Error::corrupt("non-empty file manifest has no chunks"));
            }
            for (index, chunk) in chunks.iter().enumerate() {
                let is_last = index + 1 == chunks.len();
                if !is_last && chunk.len != chunk_size {
                    return Err(Error::corrupt("non-final chunk is not full"));
                }
            }
        }
        let content_hash = wu::get_hash(map, "content_hash")?;
        if size == 0 && content_hash != identity::content_hash(b"") {
            return Err(Error::corrupt("empty manifest content hash mismatch"));
        }
        if let Some(representation) = &representation {
            if representation.decoded_size != size {
                return Err(Error::corrupt(format!(
                    "representation decoded_size {} differs from manifest size {size}",
                    representation.decoded_size
                )));
            }
        }
        Ok(Manifest {
            size,
            chunk_size,
            chunks,
            content_hash,
            representation,
        })
    }
}

/// Serialize state to the checkpoint map (`WEFT-MODEL.md` §2.4).
pub fn state_to_value(state: &State, generation: u64) -> Value {
    let objects: Vec<Value> = state.objects.values().map(ObjectRecord::to_value).collect();

    let mut entries: Vec<&EntryRecord> = state.entries.values().collect();
    entries.sort_by(|a, b| a.parent.cmp(&b.parent).then_with(|| a.name.cmp(&b.name)));
    let entries: Vec<Value> = entries.iter().map(|record| record.to_value()).collect();

    let relations: Vec<Value> = state
        .relations
        .values()
        .map(RelationRecord::to_value)
        .collect();

    let snapshots: BTreeMap<String, Value> = state
        .snapshots
        .iter()
        .map(|(name, hash)| (name.clone(), wu::hash_value(*hash)))
        .collect();

    let tips: BTreeMap<String, Value> = state
        .tips
        .iter()
        .map(|(object, version)| (object.to_hex(), wu::hash_value(*version)))
        .collect();

    wu::map([
        ("schema", Value::Str(STATE_SCHEMA.to_owned())),
        ("generation", Value::U64(generation)),
        ("objects", Value::Array(objects)),
        ("entries", Value::Array(entries)),
        ("relations", Value::Array(relations)),
        ("snapshots", Value::Map(snapshots)),
        ("tips", Value::Map(tips)),
    ])
}

/// Encode a checkpoint state manifest.
pub fn encode_state(state: &State, generation: u64) -> Result<Vec<u8>> {
    wu::encode(&state_to_value(state, generation))
}

/// Decode a checkpoint state manifest. Structural errors are `Malformed`;
/// identity mismatches are `Corrupt`.
pub fn decode_state(value: &Value) -> Result<State> {
    let map = wu::as_map(value)?;
    wu::check_keys(
        map,
        &[
            "schema",
            "generation",
            "objects",
            "entries",
            "relations",
            "snapshots",
            "tips",
        ],
    )?;
    let schema = wu::get_text(map, "schema")?;
    if schema != STATE_SCHEMA {
        return Err(Error::unknown_schema(format!(
            "state manifest schema `{schema}` is not `{STATE_SCHEMA}`"
        )));
    }
    let generation = wu::get_u64(map, "generation")?;
    let mut state = State {
        generation,
        ..State::default()
    };

    for item in wu::get_array(map, "objects")? {
        let record = ObjectRecord::from_wcb(item)?;
        validate_object_record(&record)?;
        if state.objects.insert(record.id, record).is_some() {
            return Err(Error::malformed("duplicate object id in state manifest"));
        }
    }
    for item in wu::get_array(map, "entries")? {
        let record = EntryRecord::from_wcb(item)?;
        if state.entries.insert(record.id, record).is_some() {
            return Err(Error::malformed("duplicate entry id in state manifest"));
        }
    }
    for item in wu::get_array(map, "relations")? {
        let record = RelationRecord::from_wcb(item)?;
        if state.relations.insert(record.id, record).is_some() {
            return Err(Error::malformed("duplicate relation id in state manifest"));
        }
    }

    for (name, value) in wu::get_map(map, "snapshots")? {
        let hash = match value {
            Value::Bytes(bytes) => Hash32::from_slice(&bytes)
                .map_err(|_| Error::corrupt("snapshot hash must be 32 bytes"))?,
            _ => return Err(Error::malformed("snapshot value must be bytes32")),
        };
        if hash == Hash32::ZERO {
            return Err(Error::corrupt("snapshot points at the zero hash"));
        }
        state.snapshots.insert(name, hash);
    }

    for (key, value) in wu::get_map(map, "tips")? {
        let object = Hash32::from_hex(&key)
            .map_err(|_| Error::corrupt("tip key must be a 32-byte hex object id"))?;
        if object.to_hex() != key {
            return Err(Error::corrupt("tip key must be lowercase hex"));
        }
        let version = match value {
            Value::Bytes(bytes) => Hash32::from_slice(&bytes)
                .map_err(|_| Error::corrupt("tip version must be 32 bytes"))?,
            _ => return Err(Error::malformed("tip value must be bytes32")),
        };
        state.tips.insert(object, version);
    }

    Ok(state)
}
