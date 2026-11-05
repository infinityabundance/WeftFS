//! Domain-separated identities (`WEFT-MODEL.md` §2.3.1).
//!
//! Every identity in the store is `H(domain, canonical_bytes)` with the single
//! v1 construction implemented by `weftcodec::hash::hash_domain`.

use std::fs::File;
use std::io::Read;

use weftcodec::hash::{
    hash_domain, DOMAIN_CHUNK, DOMAIN_COMMIT, DOMAIN_MANIFEST, DOMAIN_OBJECT, DOMAIN_RELATION,
    DOMAIN_SELECTOR, DOMAIN_SNAPSHOT, DOMAIN_STRUCTURED, DOMAIN_VERSION,
};

use crate::error::{Error, Result};

pub use weftcodec::hash::Hash32;

/// Re-export of the object-identity domain.
pub const OBJECT_DOMAIN: &str = DOMAIN_OBJECT;
pub const VERSION_DOMAIN: &str = DOMAIN_VERSION;
pub const ENTRY_DOMAIN: &str = "weft/entry/v1";
pub const RELATION_DOMAIN: &str = DOMAIN_RELATION;
pub const COMMIT_DOMAIN: &str = DOMAIN_COMMIT;
pub const MANIFEST_DOMAIN: &str = DOMAIN_MANIFEST;
pub const CHUNK_DOMAIN: &str = DOMAIN_CHUNK;
pub const SELECTOR_DOMAIN: &str = DOMAIN_SELECTOR;
pub const SNAPSHOT_DOMAIN: &str = DOMAIN_SNAPSHOT;
pub const STRUCTURED_DOMAIN: &str = DOMAIN_STRUCTURED;

/// Merkle leaf domain (`WEFT-MODEL.md` §2.3.2).
pub const MERKLE_LEAF_DOMAIN: &str = "weft/merkle/leaf/v1";
/// Merkle internal-node domain.
pub const MERKLE_NODE_DOMAIN: &str = "weft/merkle/node/v1";
/// Merkle empty-tree domain.
pub const MERKLE_EMPTY_DOMAIN: &str = "weft/merkle/empty/v1";

/// Alias for callers that name the digest type `Id`.
pub type Id = Hash32;

/// `H(domain, data)`.
pub fn h(domain: &str, data: &[u8]) -> Hash32 {
    hash_domain(domain, data)
}

/// `ObjectId = H("weft/object/v1", seed)`.
pub fn object_id_from_seed(seed: &[u8]) -> Hash32 {
    h(OBJECT_DOMAIN, seed)
}

/// `ContentId`-style whole-bytes digest `H("weft/chunk/v1", bytes)`.
pub fn content_hash(bytes: &[u8]) -> Hash32 {
    h(CHUNK_DOMAIN, bytes)
}

/// `leaf_i = H("weft/merkle/leaf/v1", u64le(chunk_len) || chunk_hash)`.
pub fn merkle_leaf(chunk_len: u64, chunk_hash: Hash32) -> Hash32 {
    let mut preimage = [0u8; 40];
    preimage[..8].copy_from_slice(&chunk_len.to_le_bytes());
    preimage[8..].copy_from_slice(chunk_hash.as_bytes());
    h(MERKLE_LEAF_DOMAIN, &preimage)
}

/// `parent(left, right) = H("weft/merkle/node/v1", left || right)`.
pub fn merkle_node(left: Hash32, right: Hash32) -> Hash32 {
    let mut preimage = [0u8; 64];
    preimage[..32].copy_from_slice(left.as_bytes());
    preimage[32..].copy_from_slice(right.as_bytes());
    h(MERKLE_NODE_DOMAIN, &preimage)
}

/// Root of the empty chunk sequence: `H("weft/merkle/empty/v1", empty)`.
pub fn merkle_empty_root() -> Hash32 {
    h(MERKLE_EMPTY_DOMAIN, b"")
}

/// Merkle root over chunk order.
///
/// Leaves are built as specified in `WEFT-MODEL.md` §2.3.2. At every level an
/// odd trailing node is promoted unchanged (never duplicated). The chunk size
/// and the chunk-length of the final chunk are part of the leaf preimage, so
/// the root is deliberately chunk-size dependent; the chunk size is recorded
/// in the manifest.
pub fn merkle_root_from_chunks(chunks: impl IntoIterator<Item = (u64, Hash32)>) -> Hash32 {
    let mut level: Vec<Hash32> = chunks
        .into_iter()
        .map(|(len, hash)| merkle_leaf(len, hash))
        .collect();
    if level.is_empty() {
        return merkle_empty_root();
    }
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut index = 0;
        while index + 1 < level.len() {
            next.push(merkle_node(level[index], level[index + 1]));
            index += 2;
        }
        if index < level.len() {
            // Odd node promoted unchanged.
            next.push(level[index]);
        }
        level = next;
    }
    level[0]
}

/// Start a streaming computation of the same digest as [`content_hash`].
///
/// The v1 identity is frozen: `ContentId = BLAKE3(u32le(|domain|) || domain
/// || whole_file_bytes)`. Feeding the file in bounded pieces produces the
/// identical digest without materialising the file in memory. The hash pass
/// itself remains O(file size) by construction.
pub(crate) fn content_hash_hasher() -> blake3::Hasher {
    let len = u32::try_from(CHUNK_DOMAIN.len()).unwrap_or(u32::MAX);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&len.to_le_bytes());
    hasher.update(CHUNK_DOMAIN.as_bytes());
    hasher
}

/// Finish a [`content_hash_hasher`] computation.
pub(crate) fn finish_content_hash(hasher: blake3::Hasher) -> Hash32 {
    Hash32::from_bytes(*hasher.finalize().as_bytes())
}

/// `ManifestId = H("weft/manifest/v1", WCB(manifest))`, used for file
/// manifests and for checkpoint state manifests.
pub fn manifest_id(wcb_bytes: &[u8]) -> Hash32 {
    h(MANIFEST_DOMAIN, wcb_bytes)
}

/// `VersionId = H("weft/version/v1", WCB(version_record))`.
pub fn version_id(wcb_bytes: &[u8]) -> Hash32 {
    h(VERSION_DOMAIN, wcb_bytes)
}

/// `Commit hash = H("weft/store/commit/v1", WCB(commit_record))`.
pub fn commit_id(wcb_bytes: &[u8]) -> Hash32 {
    h(COMMIT_DOMAIN, wcb_bytes)
}

/// 32 bytes from the operating system CSPRNG. Fails closed if unavailable;
/// there is no time/pid-based fallback.
pub fn os_random_32() -> Result<[u8; 32]> {
    let mut buf = [0u8; 32];
    let mut file = File::open("/dev/urandom")
        .map_err(|e| Error::io(format!("cannot open /dev/urandom: {e}")))?;
    file.read_exact(&mut buf)
        .map_err(|e| Error::io(format!("cannot read /dev/urandom: {e}")))?;
    Ok(buf)
}
