//! On-disk byte formats: superblock, commit records, file manifests, and
//! checkpoint state manifests.
//!
//! Normative sources: `WEFT-MODEL.md` §2.1–§2.4 and `WEFTCODEC-FORMAT.md` §1.
//! All integers are little-endian; all structured bytes are canonical WCB.

use std::collections::BTreeMap;

use weftcodec::wcb::Value;

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::state::{EntryRecord, ObjectRecord, Op, RelationRecord, State};
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

/// A file content manifest (`WEFT-MODEL.md` §2.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub size: u64,
    pub chunk_size: u64,
    pub chunks: Vec<ChunkRef>,
    pub content_hash: Hash32,
}

/// Largest accepted chunk size in v1 (defensive bound; v1 writes 64 KiB).
pub const MAX_CHUNK_SIZE: u64 = 16 * 1024 * 1024;

impl Manifest {
    const KEYS: &'static [&'static str] =
        &["schema", "size", "chunk_size", "chunks", "content_hash"];

    pub fn to_value(&self) -> Value {
        let chunks = self
            .chunks
            .iter()
            .map(|chunk| {
                wu::map([
                    ("hash", wu::hash_value(chunk.hash)),
                    ("len", Value::U64(chunk.len)),
                ])
            })
            .collect();
        wu::map([
            ("schema", Value::Str(MANIFEST_SCHEMA.to_owned())),
            ("size", Value::U64(self.size)),
            ("chunk_size", Value::U64(self.chunk_size)),
            ("chunks", Value::Array(chunks)),
            ("content_hash", wu::hash_value(self.content_hash)),
        ])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::manifest_id(&self.encode()?))
    }

    pub fn from_wcb(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        // The normative spec writes `weft.manifest.v1`. The older
        // `weftfs.manifest.v1` spelling is accepted on read for compatibility
        // with early notes; the writer emits the spec spelling. See
        // docs/store-status.md.
        if schema != MANIFEST_SCHEMA && schema != "weftfs.manifest.v1" {
            return Err(Error::unknown_schema(format!(
                "manifest schema `{schema}` is not `{MANIFEST_SCHEMA}`"
            )));
        }
        let size = wu::get_u64(map, "size")?;
        let chunk_size = wu::get_u64(map, "chunk_size")?;
        if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
            return Err(Error::malformed("manifest chunk_size is out of range"));
        }
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
        let content_hash = wu::get_hash(map, "content_hash")?;
        if size == 0 && content_hash != identity::content_hash(b"") {
            return Err(Error::corrupt("empty manifest content hash mismatch"));
        }
        Ok(Manifest {
            size,
            chunk_size,
            chunks,
            content_hash,
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
