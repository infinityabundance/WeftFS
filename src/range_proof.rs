//! T073 — portable authenticated range proofs.
//!
//! A range proof answers one question for a verifier that holds only a proof
//! bundle (or a store directory) plus a checkpoint root:
//!
//! *are these bytes the declared canonical region of tensor T, and is that
//! region committed to by the checkpoint's authenticated root?*
//!
//! It is built from the structures the store already has. The leaf of the
//! tree is the frozen Merkle leaf over a chunk table entry
//! (`WEFT-MODEL.md` §2.3.2):
//!
//! ```text
//! chunk_hash_i = H("weft/chunk/v1", chunk_bytes_i)
//! leaf_i       = H("weft/merkle/leaf/v1", u64le(len_i) || chunk_hash_i)
//! parent(l, r) = H("weft/merkle/node/v1", l || r)
//! ```
//!
//! with the frozen pairing rule (left-to-right, an odd trailing node promoted
//! unchanged, never duplicated). **No parallel tree is introduced, no new leaf
//! or node domain, and no on-disk structure is added.** The sibling table is
//! derived from the chunk table the manifest already carries; the proof is the
//! authentication path over that table.
//!
//! ## Why an anchor record exists
//!
//! A `weft.checkpoint.v1` shard entry records `content_root` =
//! `H("weft/chunk/v1", whole shard bytes)`, a *whole-file* digest. A range
//! proof cannot be checked against a whole-file digest without reading every
//! byte, which is exactly what a range proof must avoid. The chunk table's
//! Merkle root is therefore not authenticated by the checkpoint alone. The
//! smallest additive change is one canonical record,
//! `weft.range-anchor.v1`, which binds
//!
//! ```text
//! signed( checkpoint_root, tensor, dtype, shape, logical_root,
//!         shard entry {object_id, content_root, logical_root, byte_len},
//!         shard manifest id, chunk_size, chunk_count, bytes,
//!         chunk_table_root, [load attestation root] )
//! ```
//!
//! and is itself content-addressed. The signature is what turns "a chunk table
//! someone produced" into "the chunk table for this checkpoint's shard". The
//! verifier separately recomputes the checkpoint manifest root from the
//! manifest's canonical bytes, so the anchor cannot be replayed against another
//! checkpoint. When a signed load attestation is named, the verifier checks its
//! signature and that it names the same checkpoint root and tensor logical
//! root.
//!
//! This is stated plainly in `spec/WEFT-RANGE-PROOF.md`: a chunk-table proof is
//! only available for a shard whose retained representation *is* a chunk table
//! (`weft.manifest.v2` `kind:"raw"`, or a `weft.manifest.v3` Merkle manifest).
//! A frame-backed shard (modes 9/10/15) has no chunk table, and a range proof
//! over it is refused rather than approximated.

use std::collections::BTreeMap;

use weftcodec::wcb::Value;

use crate::recovery::BlobKind;
use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::store::Store;
use crate::wcb_util as wu;

/// Canonical anchor schema.
pub const RANGE_ANCHOR_SCHEMA: &str = "weft.range-anchor.v1";
/// Canonical proof schema.
pub const RANGE_PROOF_SCHEMA: &str = "weft.range-proof.v1";
/// Domain for `RangeAnchorId`.
pub const RANGE_ANCHOR_DOMAIN: &str = "weft/range-anchor/v1";
/// Domain for `RangeProofId`.
pub const RANGE_PROOF_DOMAIN: &str = "weft/range-proof/v1";
/// Domain for the digest of the requested region bytes.
pub const RANGE_DOMAIN: &str = "weft/range/v1";
/// Namespace directory holding anchors.
pub const RANGE_ANCHOR_DIR: &str = "range-anchors";

/// Hard bounds. Every violation is `LimitExceeded`.
pub const MAX_ANCHOR_SHARDS: usize = 4_096;
pub const MAX_PROOF_LEAVES: usize = 16_777_216;
pub const MAX_PATH_NODES: usize = 64;

fn hash_value(hash: Hash32) -> Value {
    wu::hash_value(hash)
}

fn opt_hash(hash: Option<Hash32>) -> Value {
    wu::opt_hash_value(hash)
}

// ---------------------------------------------------------------------------
// Merkle path over the existing chunk table
// ---------------------------------------------------------------------------

/// Which side of a parent a path node sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathSide {
    /// The sibling is the left child; the running hash is the right child.
    Left,
    /// The sibling is the right child; the running hash is the left child.
    Right,
}

impl PathSide {
    pub fn as_str(self) -> &'static str {
        match self {
            PathSide::Left => "left",
            PathSide::Right => "right",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "left" => Ok(PathSide::Left),
            "right" => Ok(PathSide::Right),
            other => Err(Error::malformed(format!("unknown path side `{other}`"))),
        }
    }
}

/// One sibling node on the authentication path to the Merkle root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathNode {
    pub side: PathSide,
    pub hash: Hash32,
}

/// One leaf of the range: a whole chunk that the region intersects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofLeaf {
    /// Chunk index in the canonical chunk table.
    pub index: u64,
    /// Byte offset of the chunk inside the object.
    pub offset: u64,
    /// Declared chunk length.
    pub len: u64,
    /// `H("weft/chunk/v1", chunk_bytes)`.
    pub hash: Hash32,
}

/// Leaves of the chunk table, in canonical order.
fn level_leaves(chunks: &[(u64, Hash32)]) -> Vec<Hash32> {
    chunks
        .iter()
        .map(|(len, hash)| identity::merkle_leaf(*len, *hash))
        .collect()
}

/// Fold one level with the frozen promotion rule.
fn fold_level(level: &[Hash32]) -> Vec<Hash32> {
    let mut next = Vec::with_capacity(level.len().div_ceil(2));
    let mut index = 0;
    while index + 1 < level.len() {
        next.push(identity::merkle_node(level[index], level[index + 1]));
        index += 2;
    }
    if index < level.len() {
        // Odd trailing node: promoted unchanged, never hashed with itself.
        next.push(level[index]);
    }
    next
}

/// Build the authentication path for `leaf_index` over `chunks`.
///
/// The path only contains levels where a sibling exists. The verifier
/// re-derives each level's width from `chunk_count` alone, so a missing,
/// extra, or wrongly sided node is a rejection rather than an alternative
/// interpretation.
pub fn build_path(chunks: &[(u64, Hash32)], leaf_index: u64) -> Result<Vec<PathNode>> {
    let count = chunks.len() as u64;
    if count == 0 || leaf_index >= count {
        return Err(Error::invalid_argument(
            "range proof leaf index is outside the chunk table",
        ));
    }
    let mut level = level_leaves(chunks);
    let mut index = leaf_index as usize;
    let mut path = Vec::new();
    while level.len() > 1 {
        let width = level.len();
        if index % 2 == 1 {
            path.push(PathNode {
                side: PathSide::Left,
                hash: level[index - 1],
            });
        } else if index + 1 < width {
            path.push(PathNode {
                side: PathSide::Right,
                hash: level[index + 1],
            });
        }
        // index even and index+1 == width: the node is promoted at this level
        // and has no sibling, so the path carries no node for it.
        level = fold_level(&level);
        index /= 2;
        if path.len() > MAX_PATH_NODES {
            return Err(Error::limit_exceeded(format!(
                "authentication path exceeds {MAX_PATH_NODES} nodes"
            )));
        }
    }
    Ok(path)
}

/// Verify one path against a declared root.
///
/// `chunk_count` is the table width at leaf level; every level width is
/// derived from it by the frozen promotion rule, so the expected number of
/// nodes and each node's side are determined by the leaf index and the table
/// width, not by the proof.
pub fn verify_path(
    chunk_count: u64,
    leaf_index: u64,
    leaf_hash: Hash32,
    path: &[PathNode],
    expected_root: Hash32,
) -> Result<()> {
    if chunk_count == 0 {
        return Err(Error::malformed("range proof chunk_count must be positive"));
    }
    if leaf_index >= chunk_count {
        return Err(Error::corrupt(
            "range proof leaf index is outside the declared chunk table",
        ));
    }
    let mut hash = leaf_hash;
    let mut index = leaf_index;
    let mut width = chunk_count;
    let mut consumed = 0usize;
    while width > 1 {
        let has_sibling = index % 2 == 1 || index + 1 < width;
        if has_sibling {
            let node = path
                .get(consumed)
                .ok_or_else(|| Error::corrupt("range proof authentication path is truncated"))?;
            let expected_side = if index % 2 == 1 {
                PathSide::Left
            } else {
                PathSide::Right
            };
            if node.side != expected_side {
                return Err(Error::corrupt(format!(
                    "range proof node {consumed} is on the {} side but the leaf geometry \
                     requires {}",
                    node.side.as_str(),
                    expected_side.as_str()
                )));
            }
            hash = match node.side {
                PathSide::Left => identity::merkle_node(node.hash, hash),
                PathSide::Right => identity::merkle_node(hash, node.hash),
            };
            consumed += 1;
        }
        index /= 2;
        width = width.div_ceil(2);
    }
    if consumed != path.len() {
        return Err(Error::corrupt(
            "range proof authentication path has nodes the tree geometry does not admit",
        ));
    }
    if hash != expected_root {
        return Err(Error::corrupt(
            "range proof authentication path does not reconstruct the anchored Merkle root",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Anchor record
// ---------------------------------------------------------------------------

/// One shard's binding inside an anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorShard {
    pub index: u64,
    pub object_name: String,
    pub object_id: Hash32,
    /// The checkpoint entry's whole-file content root.
    pub content_root: Hash32,
    pub logical_root: Hash32,
    pub byte_len: u64,
    /// Manifest id of the retained chunk table.
    pub manifest: Hash32,
    /// Merkle root over the retained chunk table.
    pub chunk_table_root: Hash32,
    pub chunk_count: u64,
}

/// `weft.range-anchor.v1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeAnchor {
    pub checkpoint_name: String,
    /// `H("weft/checkpoint/manifest/v1", canonical manifest bytes)`.
    pub checkpoint_root: Hash32,
    /// Manifest blob id of the checkpoint manifest object.
    pub checkpoint_manifest_id: Hash32,
    pub tensor: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    pub logical_root: Hash32,
    pub content_root: Hash32,
    pub element_bytes: u64,
    pub chunk_size: u64,
    pub bytes: u64,
    pub chunk_count: u64,
    pub chunk_table_root: Hash32,
    pub shards: Vec<AnchorShard>,
    /// Root of a signed load attestation covering this tensor, when present.
    pub load_attestation: Option<Hash32>,
    pub issued_at_ns: i64,
    pub signature: Option<AnchorSignature>,
}

/// Ed25519 signature over the anchor's canonical bytes without `signature`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorSignature {
    pub public_key: [u8; 32],
    pub signature: [u8; 64],
}

impl AnchorSignature {
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
                "range anchor signature algorithm `{algorithm}` is not `ed25519`"
            )));
        }
        let public_key: [u8; 32] = wu::get_bytes(map, "public_key")?
            .try_into()
            .map_err(|_| Error::malformed("range anchor public_key must be 32 bytes"))?;
        let signature: [u8; 64] = wu::get_bytes(map, "signature")?
            .try_into()
            .map_err(|_| Error::malformed("range anchor signature must be 64 bytes"))?;
        Ok(AnchorSignature {
            public_key,
            signature,
        })
    }
}

impl AnchorShard {
    fn to_value(&self) -> Value {
        wu::map([
            ("index", Value::U64(self.index)),
            ("object_name", Value::Str(self.object_name.clone())),
            ("object_id", hash_value(self.object_id)),
            ("content_root", hash_value(self.content_root)),
            ("logical_root", hash_value(self.logical_root)),
            ("byte_len", Value::U64(self.byte_len)),
            ("manifest", hash_value(self.manifest)),
            ("chunk_table_root", hash_value(self.chunk_table_root)),
            ("chunk_count", Value::U64(self.chunk_count)),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(
            map,
            &[
                "index",
                "object_name",
                "object_id",
                "content_root",
                "logical_root",
                "byte_len",
                "manifest",
                "chunk_table_root",
                "chunk_count",
            ],
        )?;
        let object_name = wu::get_text(map, "object_name")?;
        if object_name.is_empty() {
            return Err(Error::malformed("range anchor shard has an empty name"));
        }
        let chunk_count = wu::get_u64(map, "chunk_count")?;
        if chunk_count == 0 {
            return Err(Error::malformed("range anchor shard declares zero chunks"));
        }
        Ok(AnchorShard {
            index: wu::get_u64(map, "index")?,
            object_name,
            object_id: wu::get_hash(map, "object_id")?,
            content_root: wu::get_hash(map, "content_root")?,
            logical_root: wu::get_hash(map, "logical_root")?,
            byte_len: wu::get_u64(map, "byte_len")?,
            manifest: wu::get_hash(map, "manifest")?,
            chunk_table_root: wu::get_hash(map, "chunk_table_root")?,
            chunk_count,
        })
    }
}

impl RangeAnchor {
    pub const KEYS: &'static [&'static str] = &[
        "schema",
        "checkpoint_name",
        "checkpoint_root",
        "checkpoint_manifest_id",
        "tensor",
        "dtype",
        "shape",
        "logical_root",
        "content_root",
        "element_bytes",
        "chunk_size",
        "bytes",
        "chunk_count",
        "chunk_table_root",
        "shards",
        "load_attestation",
        "issued_at",
        "signature",
    ];

    fn to_value_without_signature(&self) -> Value {
        wu::map([
            ("schema", Value::Str(RANGE_ANCHOR_SCHEMA.to_owned())),
            ("checkpoint_name", Value::Str(self.checkpoint_name.clone())),
            ("checkpoint_root", hash_value(self.checkpoint_root)),
            (
                "checkpoint_manifest_id",
                hash_value(self.checkpoint_manifest_id),
            ),
            ("tensor", Value::Str(self.tensor.clone())),
            ("dtype", Value::Str(self.dtype.clone())),
            (
                "shape",
                Value::Array(self.shape.iter().map(|dim| Value::U64(*dim)).collect()),
            ),
            ("logical_root", hash_value(self.logical_root)),
            ("content_root", hash_value(self.content_root)),
            ("element_bytes", Value::U64(self.element_bytes)),
            ("chunk_size", Value::U64(self.chunk_size)),
            ("bytes", Value::U64(self.bytes)),
            ("chunk_count", Value::U64(self.chunk_count)),
            ("chunk_table_root", hash_value(self.chunk_table_root)),
            (
                "shards",
                Value::Array(self.shards.iter().map(|shard| shard.to_value()).collect()),
            ),
            ("load_attestation", opt_hash(self.load_attestation)),
            ("issued_at", Value::Timestamp(self.issued_at_ns)),
        ])
    }

    fn to_value(&self) -> Value {
        let mut map = match self.to_value_without_signature() {
            Value::Map(map) => map,
            _ => unreachable!("map constructor returns a map"),
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

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    /// The exact bytes a signature covers.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value_without_signature())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        let anchor = Self::from_value(&value)?;
        anchor.verify_signature()?;
        Ok(anchor)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != RANGE_ANCHOR_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "range anchor schema `{schema}` is not `{RANGE_ANCHOR_SCHEMA}`"
            )));
        }
        let checkpoint_name = wu::get_text(map, "checkpoint_name")?;
        let tensor = wu::get_text(map, "tensor")?;
        if checkpoint_name.is_empty() || tensor.is_empty() {
            return Err(Error::malformed(
                "range anchor checkpoint_name and tensor must be non-empty",
            ));
        }
        let mut shape = Vec::new();
        for dim in wu::get_array(map, "shape")? {
            match dim {
                Value::U64(value) => shape.push(*value),
                _ => return Err(Error::malformed("range anchor shape must be u64")),
            }
        }
        let shard_values = wu::get_array(map, "shards")?;
        if shard_values.is_empty() {
            return Err(Error::malformed("range anchor has no shards"));
        }
        if shard_values.len() > MAX_ANCHOR_SHARDS {
            return Err(Error::limit_exceeded(format!(
                "range anchor lists {} shards, above the {MAX_ANCHOR_SHARDS} bound",
                shard_values.len()
            )));
        }
        let mut shards = Vec::with_capacity(shard_values.len());
        for item in shard_values {
            shards.push(AnchorShard::from_value(item)?);
        }
        shards.sort_by_key(|shard| shard.index);
        for pair in shards.windows(2) {
            if pair[0].index >= pair[1].index {
                return Err(Error::non_canonical(
                    "range anchor shard indices are not distinct and ascending",
                ));
            }
        }
        let chunk_count = wu::get_u64(map, "chunk_count")?;
        if chunk_count == 0 {
            return Err(Error::malformed("range anchor declares zero chunks"));
        }
        let anchor = RangeAnchor {
            checkpoint_name,
            checkpoint_root: wu::get_hash(map, "checkpoint_root")?,
            checkpoint_manifest_id: wu::get_hash(map, "checkpoint_manifest_id")?,
            tensor,
            dtype: wu::get_text(map, "dtype")?,
            shape,
            logical_root: wu::get_hash(map, "logical_root")?,
            content_root: wu::get_hash(map, "content_root")?,
            element_bytes: wu::get_u64(map, "element_bytes")?,
            chunk_size: wu::get_u64(map, "chunk_size")?,
            bytes: wu::get_u64(map, "bytes")?,
            chunk_count,
            chunk_table_root: wu::get_hash(map, "chunk_table_root")?,
            shards,
            load_attestation: wu::get_opt_hash(map, "load_attestation")?,
            issued_at_ns: wu::get_timestamp(map, "issued_at")?,
            signature: match map.get("signature") {
                Some(Value::Null) => None,
                Some(value) => Some(AnchorSignature::from_value(value)?),
                None => return Err(Error::malformed("`signature` is missing")),
            },
        };
        anchor.validate()?;
        Ok(anchor)
    }

    pub fn validate(&self) -> Result<()> {
        if self.chunk_size == 0 {
            return Err(Error::malformed("range anchor chunk_size must be positive"));
        }
        if self.bytes == 0 {
            return Err(Error::malformed("range anchor bytes must be positive"));
        }
        for shard in &self.shards {
            if shard.byte_len == 0 {
                return Err(Error::malformed("range anchor shard has zero bytes"));
            }
            if shard.chunk_count != shard.byte_len.div_ceil(self.chunk_size) {
                return Err(Error::non_canonical(
                    "range anchor shard chunk_count disagrees with its byte length",
                ));
            }
        }
        let sum: u64 = self.shards.iter().map(|shard| shard.byte_len).sum();
        if sum != self.bytes {
            return Err(Error::corrupt(
                "range anchor shard byte lengths do not sum to the tensor byte length",
            ));
        }
        Ok(())
    }

    /// `RangeAnchorId = H("weft/range-anchor/v1", WCB(record))`.
    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(RANGE_ANCHOR_DOMAIN, &self.encode()?))
    }

    pub fn sign(&mut self, seed: &[u8; 32]) -> Result<()> {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(seed);
        let signature = key.sign(&self.signing_bytes()?);
        self.signature = Some(AnchorSignature {
            public_key: key.verifying_key().to_bytes(),
            signature: signature.to_bytes(),
        });
        Ok(())
    }

    pub fn verify_signature(&self) -> Result<()> {
        let Some(signature) = &self.signature else {
            return Ok(());
        };
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let key = VerifyingKey::from_bytes(&signature.public_key)
            .map_err(|err| Error::corrupt(format!("range anchor verification key: {err}")))?;
        let parsed = Signature::from_bytes(&signature.signature);
        key.verify(&self.signing_bytes()?, &parsed)
            .map_err(|_| Error::corrupt("range anchor signature does not verify"))
    }

    /// Verify the signature and require its key to be in `trust`. An unsigned
    /// anchor is `Unauthorized` when a trust set is configured.
    pub fn verify_signature_with_trust(&self, trust: &[[u8; 32]]) -> Result<()> {
        if trust.is_empty() {
            return self.verify_signature();
        }
        let Some(signature) = &self.signature else {
            return Err(Error::unauthorized(
                "range anchor is unsigned and this verifier requires a trusted signer",
            ));
        };
        self.verify_signature()?;
        if trust.iter().any(|key| key == &signature.public_key) {
            Ok(())
        } else {
            Err(Error::unauthorized(
                "range anchor signer is not in the configured trust set",
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Proof record
// ---------------------------------------------------------------------------

/// `weft.range-proof.v1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeProof {
    pub anchor: Hash32,
    /// Manifest id of the shard's chunk table.
    pub manifest: Hash32,
    /// The checkpoint shard entry's whole-file content root.
    pub content_root: Hash32,
    pub tensor: String,
    pub shard_index: u64,
    pub chunk_size: u64,
    pub chunk_count: u64,
    pub size: u64,
    pub offset: u64,
    pub length: u64,
    /// `H("weft/range/v1", region_bytes)`.
    pub region_root: Hash32,
    pub leaves: Vec<ProofLeaf>,
    pub paths: Vec<Vec<PathNode>>,
}

impl RangeProof {
    pub const KEYS: &'static [&'static str] = &[
        "schema",
        "anchor",
        "manifest",
        "content_root",
        "tensor",
        "shard_index",
        "chunk_size",
        "chunk_count",
        "size",
        "offset",
        "length",
        "region_root",
        "leaves",
        "paths",
    ];

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(RANGE_PROOF_SCHEMA.to_owned())),
            ("anchor", hash_value(self.anchor)),
            ("manifest", hash_value(self.manifest)),
            ("content_root", hash_value(self.content_root)),
            ("tensor", Value::Str(self.tensor.clone())),
            ("shard_index", Value::U64(self.shard_index)),
            ("chunk_size", Value::U64(self.chunk_size)),
            ("chunk_count", Value::U64(self.chunk_count)),
            ("size", Value::U64(self.size)),
            ("offset", Value::U64(self.offset)),
            ("length", Value::U64(self.length)),
            ("region_root", hash_value(self.region_root)),
            (
                "leaves",
                Value::Array(
                    self.leaves
                        .iter()
                        .map(|leaf| {
                            wu::map([
                                ("index", Value::U64(leaf.index)),
                                ("offset", Value::U64(leaf.offset)),
                                ("len", Value::U64(leaf.len)),
                                ("hash", hash_value(leaf.hash)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "paths",
                Value::Array(
                    self.paths
                        .iter()
                        .map(|path| {
                            Value::Array(
                                path.iter()
                                    .map(|node| {
                                        wu::map([
                                            ("side", Value::Str(node.side.as_str().to_owned())),
                                            ("hash", hash_value(node.hash)),
                                        ])
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                ),
            ),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != RANGE_PROOF_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "range proof schema `{schema}` is not `{RANGE_PROOF_SCHEMA}`"
            )));
        }
        let leaf_values = wu::get_array(map, "leaves")?;
        if leaf_values.len() > MAX_PROOF_LEAVES {
            return Err(Error::limit_exceeded(format!(
                "range proof has {} leaves, above the {MAX_PROOF_LEAVES} bound",
                leaf_values.len()
            )));
        }
        let mut leaves = Vec::with_capacity(leaf_values.len());
        for item in leaf_values {
            let leaf = wu::as_map(item)?;
            wu::check_keys(leaf, &["index", "offset", "len", "hash"])?;
            let index = wu::get_u64(leaf, "index")?;
            let offset = wu::get_u64(leaf, "offset")?;
            let len = wu::get_u64(leaf, "len")?;
            if len == 0 {
                return Err(Error::malformed("range proof leaf has zero length"));
            }
            leaves.push(ProofLeaf {
                index,
                offset,
                len,
                hash: wu::get_hash(leaf, "hash")?,
            });
        }
        let path_values = wu::get_array(map, "paths")?;
        if path_values.len() != leaves.len() {
            return Err(Error::malformed(
                "range proof must carry exactly one authentication path per leaf",
            ));
        }
        let mut paths = Vec::with_capacity(path_values.len());
        for item in path_values {
            let nodes = match item {
                Value::Array(nodes) => nodes,
                _ => {
                    return Err(Error::malformed(
                        "range proof authentication path must be an array",
                    ))
                }
            };
            let mut path = Vec::with_capacity(nodes.len());
            for node in nodes {
                let node = wu::as_map(node)?;
                wu::check_keys(node, &["side", "hash"])?;
                path.push(PathNode {
                    side: PathSide::parse(&wu::get_text(node, "side")?)?,
                    hash: wu::get_hash(node, "hash")?,
                });
            }
            paths.push(path);
        }
        let proof = RangeProof {
            anchor: wu::get_hash(map, "anchor")?,
            manifest: wu::get_hash(map, "manifest")?,
            content_root: wu::get_hash(map, "content_root")?,
            tensor: wu::get_text(map, "tensor")?,
            shard_index: wu::get_u64(map, "shard_index")?,
            chunk_size: wu::get_u64(map, "chunk_size")?,
            chunk_count: wu::get_u64(map, "chunk_count")?,
            size: wu::get_u64(map, "size")?,
            offset: wu::get_u64(map, "offset")?,
            length: wu::get_u64(map, "length")?,
            region_root: wu::get_hash(map, "region_root")?,
            leaves,
            paths,
        };
        proof.validate_shape()?;
        Ok(proof)
    }

    /// Cache-free structural validation. Never touches bytes.
    pub fn validate_shape(&self) -> Result<()> {
        if self.chunk_size == 0 {
            return Err(Error::malformed("range proof chunk_size must be positive"));
        }
        if self.chunk_count == 0 {
            return Err(Error::malformed("range proof chunk_count must be positive"));
        }
        if self.size == 0 {
            return Err(Error::malformed("range proof size must be positive"));
        }
        if self.length == 0 {
            return Err(Error::malformed("range proof length must be positive"));
        }
        if self.leaves.is_empty() {
            return Err(Error::malformed("range proof has no leaves"));
        }
        if self.leaves.len() != self.paths.len() {
            return Err(Error::malformed(
                "range proof leaf and path counts disagree",
            ));
        }
        self.expected_span()?;
        Ok(())
    }

    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(RANGE_PROOF_DOMAIN, &self.encode()?))
    }

    /// The leaf indices and byte spans the declared region requires, derived
    /// from `offset`, `length`, `chunk_size`, `size` and `chunk_count` alone.
    pub fn expected_span(&self) -> Result<(u64, u64, Vec<(u64, u64, u64)>)> {
        if self.offset >= self.size {
            return Err(Error::invalid_argument(
                "range proof offset is at or past the object size",
            ));
        }
        let end = self
            .offset
            .checked_add(self.length)
            .ok_or_else(|| Error::invalid_argument("range proof region overflows"))?;
        if end > self.size {
            return Err(Error::invalid_argument(
                "range proof region extends past the object size",
            ));
        }
        if self.size.div_ceil(self.chunk_size) != self.chunk_count {
            return Err(Error::corrupt(
                "range proof chunk_count disagrees with size and chunk_size",
            ));
        }
        let first = self.offset / self.chunk_size;
        let last = (end - 1) / self.chunk_size;
        let mut span = Vec::new();
        for index in first..=last {
            let offset = index
                .checked_mul(self.chunk_size)
                .ok_or_else(|| Error::corrupt("chunk offset overflow"))?;
            let len = if index == self.chunk_count - 1 {
                self.size - offset
            } else {
                self.chunk_size
            };
            span.push((index, offset, len));
        }
        Ok((first, last, span))
    }
}

/// Counters from a successful verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeVerification {
    pub anchor: Hash32,
    pub chunk_table_root: Hash32,
    pub leaves_verified: u64,
    pub path_nodes_verified: u64,
    /// Chunk bytes supplied to (or read from) the verifier.
    pub chunk_bytes: u64,
    /// Bytes the requested region actually covers.
    pub region_bytes: u64,
    /// True when the shard manifest was re-read and re-derived from the store.
    pub manifest_rechecked: bool,
    /// True when a signed load attestation was present and verified.
    pub load_attestation_verified: bool,
    /// True when the anchor carried a signature that verified.
    pub anchor_signature_verified: bool,
}

// ---------------------------------------------------------------------------
// Store API
// ---------------------------------------------------------------------------

fn chunk_table_of(store: &Store, manifest_id: Hash32) -> Result<(Vec<(u64, Hash32)>, u64, u64)> {
    let manifest = store.read_manifest_keyed_public(manifest_id)?;
    if let Some(representation) = &manifest.representation {
        if representation.kind != crate::format::RepresentationKind::Raw {
            return Err(Error::unsupported_representation(format!(
                "object manifest {manifest_id} carries a {}-backed representation; a range \
                 proof requires a chunk table (raw or Merkle manifest)",
                representation.kind.as_str()
            )));
        }
    }
    if manifest.chunks.is_empty() {
        return Err(Error::unsupported_representation(
            "object manifest has no chunk table to prove a range over",
        ));
    }
    let chunks: Vec<(u64, Hash32)> = manifest
        .chunks
        .iter()
        .map(|chunk| (chunk.len, chunk.hash))
        .collect();
    Ok((chunks, manifest.chunk_size, manifest.size))
}

/// One anchor written by [`Store::write_range_anchors`].
#[derive(Clone, Debug)]
pub struct WrittenRangeAnchor {
    pub anchor_id: Hash32,
    pub object_id: Hash32,
    pub tensor: String,
    pub shard_index: u64,
    pub chunk_table_root: Hash32,
    pub chunk_count: u64,
}

/// A refused (tensor, shard) pair and the exact reason.
#[derive(Clone, Debug)]
pub struct RefusedRangeAnchor {
    pub tensor: String,
    pub shard_index: u64,
    pub reason: String,
}

/// A portable proof bundle: everything a verifier needs that is not a blob.
#[derive(Clone, Debug)]
pub struct RangeProofBundle {
    pub proof: RangeProof,
    pub anchor_bytes: Vec<u8>,
    pub checkpoint_bytes: Vec<u8>,
    /// Chunk bytes in leaf order.
    pub chunks: Vec<Vec<u8>>,
}

impl Store {
    /// Read a manifest by id through the public API surface.
    pub(crate) fn read_manifest_keyed_public(&self, id: Hash32) -> Result<crate::format::Manifest> {
        self.read_manifest_keyed(id)
    }

    fn range_anchor_base(&self) -> Result<Option<Hash32>> {
        match self.lookup(self.resolve_path("/")?, RANGE_ANCHOR_DIR.as_bytes()) {
            Ok(id) => Ok(Some(id)),
            Err(err) if err.class() == crate::error::Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn range_anchor_dir(&self, checkpoint: &str) -> Result<Option<Hash32>> {
        let Some(base) = self.range_anchor_base()? else {
            return Ok(None);
        };
        match self.lookup(base, checkpoint.as_bytes()) {
            Ok(id) => Ok(Some(id)),
            Err(err) if err.class() == crate::error::Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn ensure_anchor_dir(&mut self, parent: Hash32, name: &str) -> Result<Hash32> {
        match self.lookup(parent, name.as_bytes()) {
            Ok(id) => {
                if self.object_meta(id)?.kind != crate::state::ObjectKind::Dir {
                    return Err(Error::not_dir(format!(
                        "`{name}` exists and is not a directory"
                    )));
                }
                Ok(id)
            }
            Err(err) if err.class() == crate::error::Class::NotFound => {
                let uid = self.options().owner_uid;
                let gid = self.options().owner_gid;
                self.mkdir(parent, name.as_bytes(), 0o755, uid, gid)
            }
            Err(err) => Err(err),
        }
    }

    fn ensure_range_anchor_dir(&mut self, checkpoint: &str) -> Result<Hash32> {
        let root = self.resolve_path("/")?;
        let base = self.ensure_anchor_dir(root, RANGE_ANCHOR_DIR)?;
        self.ensure_anchor_dir(base, checkpoint)
    }

    /// Write one anchor per chunk-table-backed shard of `checkpoint`.
    ///
    /// Frame-backed shards have no chunk table and are **refused** (reported,
    /// not skipped silently). The checkpoint's own identity is unchanged: an
    /// anchor is an additive record in its own namespace.
    pub fn write_range_anchors(
        &mut self,
        checkpoint: &str,
        tensor_filter: Option<&str>,
        signer: Option<&[u8; 32]>,
        load_attestation: Option<Hash32>,
    ) -> Result<(Vec<WrittenRangeAnchor>, Vec<RefusedRangeAnchor>)> {
        self.require_writable()?;
        let loaded = self.load_checkpoint_manifest(checkpoint)?;
        let checkpoint_root = loaded.manifest_root;
        let dir = self.ensure_range_anchor_dir(checkpoint)?;
        let uid = self.options().owner_uid;
        let gid = self.options().owner_gid;
        let mut written = Vec::new();
        let mut refused = Vec::new();
        for tensor in &loaded.manifest.tensors {
            if let Some(filter) = tensor_filter {
                if tensor.name != filter {
                    continue;
                }
            }
            let _shard_total = tensor.shards.len();
            for shard in &tensor.shards {
                let object = match self.object_meta(shard.object_id) {
                    Ok(object) => object,
                    Err(err) => {
                        refused.push(RefusedRangeAnchor {
                            tensor: tensor.name.clone(),
                            shard_index: u64::from(shard.index),
                            reason: format!("shard object is not readable: {err}"),
                        });
                        continue;
                    }
                };
                let (chunks, chunk_size, size) = match chunk_table_of(self, object.content) {
                    Ok(value) => value,
                    Err(err) => {
                        refused.push(RefusedRangeAnchor {
                            tensor: tensor.name.clone(),
                            shard_index: u64::from(shard.index),
                            reason: err.to_string(),
                        });
                        continue;
                    }
                };
                if size != shard.byte_len {
                    refused.push(RefusedRangeAnchor {
                        tensor: tensor.name.clone(),
                        shard_index: u64::from(shard.index),
                        reason: format!(
                            "chunk table declares {size} bytes but the checkpoint entry declares {}",
                            shard.byte_len
                        ),
                    });
                    continue;
                }
                let chunk_table_root = identity::merkle_root_from_chunks(chunks.iter().copied());
                let mut anchor = RangeAnchor {
                    checkpoint_name: checkpoint.to_owned(),
                    checkpoint_root,
                    checkpoint_manifest_id: loaded.manifest_id,
                    tensor: tensor.name.clone(),
                    dtype: tensor.dtype.clone(),
                    shape: tensor.shape.clone(),
                    logical_root: tensor.logical_root,
                    content_root: tensor.content_root,
                    element_bytes: tensor.element_bytes,
                    chunk_size,
                    bytes: size,
                    chunk_count: chunks.len() as u64,
                    chunk_table_root,
                    shards: vec![AnchorShard {
                        index: u64::from(shard.index),
                        object_name: shard.object_name.clone(),
                        object_id: shard.object_id,
                        content_root: shard.content_root,
                        logical_root: shard.logical_root,
                        byte_len: shard.byte_len,
                        manifest: object.content,
                        chunk_table_root,
                        chunk_count: chunks.len() as u64,
                    }],
                    load_attestation,
                    issued_at_ns: self.now_ns(),
                    signature: None,
                };
                if let Some(seed) = signer {
                    anchor.sign(seed)?;
                }
                let bytes = anchor.encode()?;
                let anchor_id = anchor.id()?;
                let name = format!("{}.s{:03}.anchor", tensor.name, shard.index);
                let object_id = self.create_file(
                    dir,
                    name.as_bytes(),
                    &bytes,
                    0o644,
                    uid,
                    gid,
                    BTreeMap::new(),
                )?;
                written.push(WrittenRangeAnchor {
                    anchor_id,
                    object_id,
                    tensor: tensor.name.clone(),
                    shard_index: u64::from(shard.index),
                    chunk_table_root,
                    chunk_count: chunks.len() as u64,
                });
            }
        }
        // Anchors live in the namespace, so the commit that created them is the
        // reachability root. `create_file` commits each anchor; the loop is
        // deterministic and idempotent (same name, same canonical bytes).
        Ok((written, refused))
    }

    /// Every anchor written for `checkpoint`, ordered by tensor then shard.
    pub fn range_anchors(&self, checkpoint: &str) -> Result<Vec<(Hash32, Hash32, RangeAnchor)>> {
        let Some(dir) = self.range_anchor_dir(checkpoint)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in self.list_dir(dir)? {
            let id = entry.target;
            let record = self.object_meta(id)?;
            if record.kind != crate::state::ObjectKind::File {
                continue;
            }
            let bytes = self.read_object(id)?;
            let anchor = RangeAnchor::decode(&bytes)?;
            let anchor_id = anchor.id()?;
            out.push((anchor_id, id, anchor));
        }
        out.sort_by(|a, b| {
            (&a.2.tensor, a.2.shards[0].index, a.0).cmp(&(&b.2.tensor, b.2.shards[0].index, b.0))
        });
        Ok(out)
    }

    /// Read one anchor by its identity.
    pub fn range_anchor(
        &self,
        checkpoint: &str,
        anchor_id: Hash32,
    ) -> Result<(Hash32, RangeAnchor)> {
        let anchors = self.range_anchors(checkpoint)?;
        for (id, object, anchor) in anchors {
            if id == anchor_id {
                return Ok((object, anchor));
            }
        }
        Err(Error::not_found(format!(
            "checkpoint `{checkpoint}` has no range anchor {anchor_id}"
        )))
    }

    /// Prove the declared canonical region `[offset, offset+length)` of a
    /// checkpoint tensor shard.
    ///
    /// The region MUST lie inside one shard and the shard MUST be stored as a
    /// chunk table. A region that spans a shard boundary, or a frame-backed
    /// shard, is refused with a typed error — never approximated.
    pub fn prove_range(
        &self,
        checkpoint: &str,
        tensor: &str,
        shard_index: u64,
        offset: u64,
        length: u64,
        anchor_id: Hash32,
    ) -> Result<RangeProofBundle> {
        let loaded = self.load_checkpoint_manifest(checkpoint)?;
        let checkpoint_bytes = loaded.manifest.canonical_bytes()?;
        let entry = loaded.manifest.tensor(tensor)?;
        let shard = entry
            .shards
            .iter()
            .find(|shard| u64::from(shard.index) == shard_index)
            .ok_or_else(|| {
                Error::not_found(format!(
                    "checkpoint `{checkpoint}` tensor `{tensor}` has no shard {shard_index}"
                ))
            })?;
        let (_, anchor) = self.range_anchor(checkpoint, anchor_id)?;
        if anchor.checkpoint_root != loaded.manifest_root {
            return Err(Error::corrupt(
                "range anchor names a different checkpoint root",
            ));
        }
        if anchor.tensor != tensor {
            return Err(Error::invalid_argument(
                "range anchor names a different tensor",
            ));
        }
        let anchor_shard = anchor
            .shards
            .iter()
            .find(|candidate| candidate.index == shard_index)
            .ok_or_else(|| Error::not_found("range anchor does not cover this shard"))?;
        if anchor_shard.content_root != shard.content_root {
            return Err(Error::corrupt(
                "range anchor shard content root disagrees with the checkpoint entry",
            ));
        }
        // Region -> shard. Shards split along an axis, so only an axis-0 split
        // yields a contiguous byte interval per shard. Refuse anything else
        // rather than reinterpreting the coordinates.
        let shard_count = u64::from(shard.shards);
        let shard_base = shard_index
            .checked_mul(shard.byte_len)
            .ok_or_else(|| Error::invalid_argument("shard base overflows"))?;
        let shard_end = shard_base + shard.byte_len;
        if length == 0 {
            return Err(Error::invalid_argument(
                "range proof length must be positive",
            ));
        }
        if offset >= shard_end || offset + length > shard_end {
            return Err(Error::invalid_argument(format!(
                "region [{offset}, {offset}+{length}) is not contained in shard {shard_index} \
                 ([{shard_base}, {shard_end})); a proof spanning shards is not expressible"
            )));
        }
        if shard_count > 1 && shard.axis != 0 {
            return Err(Error::unsupported_representation(
                "a region proof for an interleaved (axis != 0) shard layout is not \
                 expressible; write the checkpoint with axis 0 or one shard",
            ));
        }
        let local = offset - shard_base;
        let object = self.object_meta(shard.object_id)?;
        let (chunks, chunk_size, size) = chunk_table_of(self, object.content)?;
        if size != shard.byte_len {
            return Err(Error::corrupt(
                "shard chunk table length disagrees with the checkpoint entry",
            ));
        }
        let chunk_table_root = identity::merkle_root_from_chunks(chunks.iter().copied());
        if chunk_table_root != anchor.chunk_table_root {
            return Err(Error::corrupt(
                "shard chunk table does not fold to the anchored Merkle root",
            ));
        }
        let mut proof = RangeProof {
            anchor: anchor_id,
            manifest: anchor_shard.manifest,
            content_root: shard.content_root,
            tensor: tensor.to_owned(),
            shard_index,
            chunk_size,
            chunk_count: chunks.len() as u64,
            size,
            offset: local,
            length,
            region_root: Hash32::ZERO,
            leaves: Vec::new(),
            paths: Vec::new(),
        };
        let (_, _, span) = proof.expected_span()?;
        let mut chunks_out = Vec::new();
        for (index, leaf_offset, leaf_len) in span {
            let hash = chunks[index as usize].1;
            proof.leaves.push(ProofLeaf {
                index,
                offset: leaf_offset,
                len: leaf_len,
                hash,
            });
            proof.paths.push(build_path(&chunks, index)?);
            let bytes =
                self.read_blob_verified_keyed(BlobKind::Object, hash, identity::CHUNK_DOMAIN)?;
            if bytes.len() as u64 != leaf_len {
                return Err(Error::corrupt(format!(
                    "chunk {index} is {} bytes, manifest declares {leaf_len}",
                    bytes.len()
                )));
            }
            chunks_out.push(bytes);
        }
        // Assemble exactly the requested bytes and hash them.
        let covered = (proof.leaves[proof.leaves.len() - 1].offset
            + proof.leaves[proof.leaves.len() - 1].len)
            - proof.leaves[0].offset;
        let mut buffer = Vec::with_capacity(covered as usize);
        for bytes in &chunks_out {
            buffer.extend_from_slice(bytes);
        }
        let start = (local - proof.leaves[0].offset) as usize;
        let end = start + length as usize;
        proof.region_root = identity::h(RANGE_DOMAIN, &buffer[start..end]);
        proof.validate_shape()?;
        Ok(RangeProofBundle {
            proof,
            anchor_bytes: anchor.encode()?,
            checkpoint_bytes,
            chunks: chunks_out,
        })
    }

    /// Verify a proof against an anchor and this store.
    ///
    /// `trust` is the set of anchor signer keys that are accepted. An empty set
    /// accepts an unsigned anchor but records that fact in the counters; a
    /// non-empty set requires a valid signature from a trusted key.
    pub fn verify_range_proof(
        &self,
        proof: &RangeProof,
        anchor: &RangeAnchor,
        chunks: Option<&[Vec<u8>]>,
        trust: &[[u8; 32]],
    ) -> Result<RangeVerification> {
        let mut report = RangeVerification {
            anchor: proof.anchor,
            chunk_table_root: anchor.chunk_table_root,
            leaves_verified: 0,
            path_nodes_verified: 0,
            chunk_bytes: 0,
            region_bytes: 0,
            manifest_rechecked: false,
            load_attestation_verified: false,
            anchor_signature_verified: false,
        };
        // 1. The anchor must be the one the proof names and must be structurally
        //    sound and, when a trust set exists, signed by it.
        if anchor.id()? != proof.anchor {
            return Err(Error::corrupt(
                "range proof anchor id does not match the supplied anchor",
            ));
        }
        anchor.validate()?;
        anchor.verify_signature_with_trust(trust)?;
        report.anchor_signature_verified = anchor.signature.is_some();
        // 2. The anchor must bind the checkpoint the store actually holds.
        let loaded = self.load_checkpoint_manifest(&anchor.checkpoint_name)?;
        if loaded.manifest_root != anchor.checkpoint_root {
            return Err(Error::corrupt(
                "range anchor checkpoint root does not match the checkpoint in this store",
            ));
        }
        if loaded.manifest_id != anchor.checkpoint_manifest_id {
            return Err(Error::corrupt(
                "range anchor names a different checkpoint manifest blob",
            ));
        }
        let entry = loaded.manifest.tensor(&anchor.tensor)?;
        if entry.logical_root != anchor.logical_root
            || entry.content_root != anchor.content_root
            || entry.element_bytes != anchor.element_bytes
            || entry.dtype != anchor.dtype
            || entry.shape != anchor.shape
        {
            return Err(Error::corrupt(
                "range anchor does not match the checkpoint tensor entry",
            ));
        }
        let shard = entry
            .shards
            .iter()
            .find(|shard| u64::from(shard.index) == proof.shard_index)
            .ok_or_else(|| Error::corrupt("range proof names a shard the tensor does not have"))?;
        let anchor_shard = anchor
            .shards
            .iter()
            .find(|candidate| candidate.index == proof.shard_index)
            .ok_or_else(|| Error::corrupt("range anchor does not cover the proof's shard"))?;
        if anchor_shard.content_root != shard.content_root
            || anchor_shard.logical_root != shard.logical_root
            || anchor_shard.byte_len != shard.byte_len
            || anchor_shard.object_id != shard.object_id
        {
            return Err(Error::corrupt(
                "range anchor shard binding disagrees with the checkpoint entry",
            ));
        }
        // 3. Dependency re-read: the chunk table really is the shard's.
        let object = self.object_meta(shard.object_id)?;
        if object.content != anchor_shard.manifest {
            return Err(Error::corrupt(
                "range anchor manifest is not the shard object's manifest",
            ));
        }
        let (table, chunk_size, size) = chunk_table_of(self, anchor_shard.manifest)?;
        if size != shard.byte_len || chunk_size != anchor.chunk_size {
            return Err(Error::corrupt(
                "shard chunk table geometry disagrees with the anchor",
            ));
        }
        if table.len() as u64 != anchor.chunk_count {
            return Err(Error::corrupt(
                "shard chunk table width disagrees with the anchor",
            ));
        }
        if identity::merkle_root_from_chunks(table.iter().copied()) != anchor.chunk_table_root {
            return Err(Error::corrupt(
                "shard chunk table does not fold to the anchored Merkle root",
            ));
        }
        let manifest = self.read_manifest_keyed_public(anchor_shard.manifest)?;
        if manifest.content_hash != shard.content_root {
            return Err(Error::corrupt(
                "shard manifest content hash disagrees with the checkpoint entry",
            ));
        }
        report.manifest_rechecked = true;
        // 4. Proof geometry and every path.
        if proof.manifest != anchor_shard.manifest
            || proof.content_root != anchor_shard.content_root
            || proof.tensor != anchor.tensor
            || proof.chunk_size != anchor.chunk_size
            || proof.chunk_count != anchor.chunk_count
            || proof.size != anchor.bytes
        {
            return Err(Error::corrupt(
                "range proof declares fields that disagree with its anchor",
            ));
        }
        let (_, _, span) = proof.expected_span()?;
        if span.len() != proof.leaves.len() {
            return Err(Error::corrupt(format!(
                "range proof carries {} leaves but the declared region requires {}",
                proof.leaves.len(),
                span.len()
            )));
        }
        for (leaf, (index, offset, len)) in proof.leaves.iter().zip(span.iter()) {
            if leaf.index != *index || leaf.offset != *offset || leaf.len != *len {
                return Err(Error::corrupt(format!(
                    "range proof leaf {} does not cover chunk {index} at {offset}+{len}",
                    leaf.index
                )));
            }
            if table[*index as usize].1 != leaf.hash || table[*index as usize].0 != leaf.len {
                return Err(Error::corrupt(format!(
                    "range proof leaf {index} is not the chunk the shard table declares"
                )));
            }
        }
        for (position, (leaf, path)) in proof.leaves.iter().zip(proof.paths.iter()).enumerate() {
            verify_path(
                proof.chunk_count,
                leaf.index,
                identity::merkle_leaf(leaf.len, leaf.hash),
                path,
                anchor.chunk_table_root,
            )
            .map_err(|err| {
                Error::corrupt(format!(
                    "range proof path for leaf {position} (chunk {}) is invalid: {}",
                    leaf.index,
                    err.message()
                ))
            })?;
            report.leaves_verified += 1;
            report.path_nodes_verified += path.len() as u64;
        }
        // 5. Chunk bytes and the region digest.
        let resolved: Vec<Vec<u8>> = match chunks {
            Some(chunks) => {
                if chunks.len() != proof.leaves.len() {
                    return Err(Error::invalid_argument(
                        "supplied chunk bytes do not match the proof leaf count",
                    ));
                }
                chunks.to_vec()
            }
            None => {
                let mut out = Vec::with_capacity(proof.leaves.len());
                for leaf in &proof.leaves {
                    out.push(self.read_blob_verified_keyed(
                        BlobKind::Object,
                        leaf.hash,
                        identity::CHUNK_DOMAIN,
                    )?);
                }
                out
            }
        };
        let mut buffer = Vec::new();
        for (leaf, bytes) in proof.leaves.iter().zip(resolved.iter()) {
            if bytes.len() as u64 != leaf.len {
                return Err(Error::corrupt(format!(
                    "chunk {} is {} bytes, the proof declares {}",
                    leaf.index,
                    bytes.len(),
                    leaf.len
                )));
            }
            let actual = identity::h(identity::CHUNK_DOMAIN, bytes);
            if actual != leaf.hash {
                return Err(Error::corrupt(format!(
                    "chunk {} bytes do not hash to the proof leaf",
                    leaf.index
                )));
            }
            report.chunk_bytes += bytes.len() as u64;
            buffer.extend_from_slice(bytes);
        }
        let start = (proof.offset - proof.leaves[0].offset) as usize;
        let end = start + proof.length as usize;
        let region = &buffer[start..end];
        if identity::h(RANGE_DOMAIN, region) != proof.region_root {
            return Err(Error::corrupt(
                "range proof region digest does not match the supplied chunk bytes",
            ));
        }
        report.region_bytes = proof.length;
        // 6. Optional signed load attestation binding.
        if let Some(expected) = anchor.load_attestation {
            let attestation = crate::load_attestation::LoadAttestation::decode(
                &self
                    .read_blob_verified_keyed(
                        BlobKind::Manifest,
                        expected,
                        crate::load_attestation::LOAD_ATTESTATION_DOMAIN,
                    )
                    .map_err(|_| {
                        Error::not_found(format!(
                            "range anchor names load attestation {expected}, which this store \
                             does not hold"
                        ))
                    })?,
            )?;
            if attestation.checkpoint_root != anchor.checkpoint_root {
                return Err(Error::corrupt(
                    "load attestation names a different checkpoint root",
                ));
            }
            let attested = attestation
                .tensors
                .iter()
                .find(|tensor| tensor.name == anchor.tensor)
                .ok_or_else(|| {
                    Error::corrupt("load attestation does not cover the anchored tensor")
                })?;
            if attested.logical_root != anchor.logical_root {
                return Err(Error::corrupt(
                    "load attestation names a different tensor logical root",
                ));
            }
            attestation.verify_signature()?;
            report.load_attestation_verified = true;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(count: usize) -> Vec<(u64, Hash32)> {
        (0..count)
            .map(|index| (4, Hash32::from_bytes([index as u8 + 1; 32])))
            .collect()
    }

    #[test]
    fn path_round_trips_for_every_table_width_and_leaf() {
        for width in 1..=65usize {
            let table = chunks(width);
            let root = identity::merkle_root_from_chunks(table.iter().copied());
            for index in 0..width {
                let leaf = identity::merkle_leaf(table[index].0, table[index].1);
                let path = build_path(&table, index as u64).unwrap();
                verify_path(width as u64, index as u64, leaf, &path, root).unwrap();
            }
        }
    }

    #[test]
    fn a_truncated_or_reordered_path_is_rejected() {
        let table = chunks(10);
        let root = identity::merkle_root_from_chunks(table.iter().copied());
        let leaf = identity::merkle_leaf(table[3].0, table[3].1);
        let path = build_path(&table, 3).unwrap();
        let mut truncated = path.clone();
        truncated.pop();
        assert!(verify_path(10, 3, leaf, &truncated, root).is_err());
        let mut flipped = path.clone();
        flipped[0].side = match flipped[0].side {
            PathSide::Left => PathSide::Right,
            PathSide::Right => PathSide::Left,
        };
        assert!(verify_path(10, 3, leaf, &flipped, root).is_err());
    }

    #[test]
    fn a_promoted_leaf_has_no_sibling_at_its_own_level() {
        // Seven leaves: index 6 is promoted at level 0.
        let table = chunks(7);
        let path = build_path(&table, 6).unwrap();
        // Level 0 contributes no node (promotion); level 1 (width 4) has a
        // left sibling; level 2 (width 2) has a left sibling.
        assert_eq!(path[0].side, PathSide::Left);
        assert_eq!(path.len(), 2);
    }
}
