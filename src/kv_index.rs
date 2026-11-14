//! Persisted, generation-coupled KV prefix index (subphase T063).
//!
//! # What this is
//!
//! [`Store::kv_prefix_lookup`](crate::store::Store::kv_prefix_lookup) is the
//! oracle: it reads and parses every `weft.kv.prefix.v1` record. That is
//! `O(prefixes)` record reads per request, which is the dominant cost of a
//! reuse decision once a store holds more than a few hundred prefixes.
//!
//! This module adds a **derived, persisted index** that turns the lookup into
//! one index read plus `O(blocks walked × log N)` probes:
//!
//! * A canonical **identity** is
//!   `H("weft/kv/prefix-identity/v1", WCB(tenant, model, tokenizer_hash,
//!   attention_hash, dtype, head_count, head_dim, layer_count, page_rows))`.
//!   Every field the frozen compatibility rule compares is in it, in one
//!   fixed order.
//! * A **block boundary** is a chain-root value: the frozen token-hash chain
//!   root of the request's first `k * page_rows` tokens. A prefix record whose
//!   `position_end` lands on a boundary is indexed under
//!   `(identity_root, token_chain_root)`.
//! * A lookup hashes the request **once**, walking page-sized blocks forward,
//!   and probes the index at each boundary. The longest verified match wins.
//!   No stored record is read unless its index entry names it, so the scan is
//!   gone.
//!
//! Every entry stores the content id of the record it names, the record's
//! current `ObjectId`, its token length and its page count. On use the record
//! is read once, re-hashed, and compared; a mismatch is a rejected entry (the
//! answer falls back to the oracle scan, never to the bad entry).
//!
//! # Authentication and freshness
//!
//! * Segments are content-addressed immutable blobs
//!   (`kv-index/<ab>/<hash>`), each carrying a
//!   `H("weft/kv/index-segment/v1", bytes)` trailer.
//! * The root record (`kv-index.root`) names the exact segment digests, the
//!   commit generation and the commit head it describes, and a digest over the
//!   live entries. A load requires **all** of: root digest, generation equal to
//!   the store's generation, head equal to the store's head, every segment
//!   digest, a contiguous generation chain, and an entry count that matches.
//!   Anything else is refused; the index is rebuilt from the oracle.
//! * `kv_prefix_put` / `kv_prefix_delete` / `kv_prefix_revoke` append a
//!   one-generation **delta segment** after the commit. Invalidation is
//!   therefore bounded: one small segment per mutation, and at most
//!   [`MAX_KV_INDEX_DELTAS`] segments before a full rebuild (`kv_index_build`,
//!   also exposed as `weftfs kv index-build`).
//!
//! # What this is not
//!
//! Not an authorization mechanism: the index never returns page bytes and
//! [`Store::kv_prefix_get`](crate::store::Store::kv_prefix_get) still enforces
//! the capability envelope per page. Not a replacement for the oracle: the
//! oracle scan remains the differential reference in the courts. Partial-tail
//! prefixes (a last page shorter than `page_rows` and not page-aligned) are not
//! indexed; a request that needs them falls back to the oracle.

use std::collections::BTreeMap;
use std::time::Instant;

use weftcodec::wcb::{self, Value};
use weftcodec::Limits;

use crate::backend::{Key, KeyKind};
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::kv_prefix::{
    kv_revocation_ids, KvLookup, KvPrefixRecord, KvPrefixRequest, MAX_KV_PREFIX_TOKENS,
};
use crate::store::{OpenMode, Store};

/// Canonical root record schema.
pub const KV_INDEX_ROOT_SCHEMA: &str = "weft.kv.index.v1";
/// Segment digest domain.
pub const DOMAIN_KV_INDEX_SEGMENT: &str = "weft/kv/index-segment/v1";
/// Root record identity domain (diagnostics only).
pub const DOMAIN_KV_INDEX_ROOT: &str = "weft/kv/index-root/v1";
/// Prefix identity domain.
pub const DOMAIN_KV_PREFIX_IDENTITY: &str = "weft/kv/prefix-identity/v1";
/// DOMAIN for one entry's contribution to the accumulator.
pub const DOMAIN_KV_INDEX_ENTRY: &str = "weft/kv/index-entry/v1";

/// Segment magic.
pub const KV_INDEX_MAGIC: &[u8; 4] = b"WKI1";
/// Segment layout version.
pub const KV_INDEX_VERSION: u16 = 1;
/// Fixed entry width on the wire.
pub const KV_INDEX_ENTRY_BYTES: usize = 152;
/// Segment header width (magic, version, kind, generation, count, reserved).
pub const KV_INDEX_HEADER_BYTES: usize = 32;
/// Trailer digest width.
pub const KV_INDEX_TRAILER_BYTES: usize = 32;

/// Hard bound on live entries in one index. A build that exceeds it is refused
/// rather than silently truncated; the oracle scan still answers.
pub const MAX_KV_INDEX_ENTRIES: usize = 4_000_000;
/// Hard bound on the encoded size of the whole index.
pub const MAX_KV_INDEX_BYTES: u64 = 768 * 1024 * 1024;
/// Maximum delta segments before the next mutation forces a full rebuild.
pub const MAX_KV_INDEX_DELTAS: u32 = 32;
/// Maximum page-block probes for one request. A very long prompt with tiny
/// pages stops here; the bound is reported, never silent.
pub const MAX_KV_INDEX_PROBES: usize = 65_536;

/// One entry's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KvIndexEntryKind {
    /// The record is live at this boundary.
    Live = 0,
    /// The record at this boundary was deleted or revoked.
    Tombstone = 1,
}

impl KvIndexEntryKind {
    fn from_u8(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(KvIndexEntryKind::Live),
            1 => Ok(KvIndexEntryKind::Tombstone),
            other => Err(Error::corrupt(format!(
                "kv index entry kind {other} is not recognised"
            ))),
        }
    }
}

/// One index entry: a block boundary mapped to the prefix record that ends
/// there, plus the record reference used to verify it before use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvIndexEntry {
    /// Identity the entry belongs to.
    pub identity_root: Hash32,
    /// Token-chain root at `position_end` (the block-boundary key).
    pub chain_root: Hash32,
    /// `H("weft/kv/prefix/v1", canonical record bytes)` — verified on use.
    pub record_prefix_id: Hash32,
    /// The record file's current `ObjectId` — the reference read on use.
    pub record_object_id: Hash32,
    /// Token length of the record.
    pub position_end: u64,
    /// Pages per layer of the record.
    pub page_count: u64,
    /// Live or tombstone.
    pub kind: KvIndexEntryKind,
}

impl KvIndexEntry {
    fn key(&self) -> (Hash32, Hash32) {
        (self.identity_root, self.chain_root)
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.identity_root.as_bytes());
        out.extend_from_slice(self.chain_root.as_bytes());
        out.extend_from_slice(self.record_prefix_id.as_bytes());
        out.extend_from_slice(self.record_object_id.as_bytes());
        out.extend_from_slice(&self.position_end.to_le_bytes());
        out.extend_from_slice(&self.page_count.to_le_bytes());
        out.push(self.kind as u8);
        out.extend_from_slice(&[0u8; 7]);
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != KV_INDEX_ENTRY_BYTES {
            return Err(Error::corrupt("kv index entry is truncated"));
        }
        let hash = |at: usize| -> Hash32 {
            let mut raw = [0u8; 32];
            raw.copy_from_slice(&bytes[at..at + 32]);
            Hash32::from_bytes(raw)
        };
        let number = |at: usize| -> u64 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[at..at + 8]);
            u64::from_le_bytes(raw)
        };
        let kind = KvIndexEntryKind::from_u8(bytes[144])?;
        if bytes[145..152].iter().any(|byte| *byte != 0) {
            return Err(Error::corrupt("kv index entry padding is not zero"));
        }
        Ok(KvIndexEntry {
            identity_root: hash(0),
            chain_root: hash(32),
            record_prefix_id: hash(64),
            record_object_id: hash(96),
            position_end: number(128),
            page_count: number(136),
            kind,
        })
    }
}

/// Which segment produced this entry in a merged view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SegmentKind {
    Base,
    Delta,
}

impl SegmentKind {
    /// Stable wire value.
    pub fn as_u16(self) -> u16 {
        match self {
            SegmentKind::Base => 0,
            SegmentKind::Delta => 1,
        }
    }

    fn from_u16(raw: u16) -> Result<Self> {
        match raw {
            0 => Ok(SegmentKind::Base),
            1 => Ok(SegmentKind::Delta),
            other => Err(Error::corrupt(format!(
                "kv index segment kind {other} is not recognised"
            ))),
        }
    }
}

/// One decoded segment.
#[derive(Clone, Debug)]
pub struct KvIndexSegment {
    pub kind: SegmentKind,
    pub generation: u64,
    pub entries: Vec<KvIndexEntry>,
    /// `H("weft/kv/index-segment/v1", encoded bytes)`.
    pub digest: Hash32,
}

impl KvIndexSegment {
    /// Encode a segment. Entries are sorted and de-duplicated by boundary key
    /// (a later entry for the same key wins), so the bytes are deterministic.
    pub fn encode(kind: SegmentKind, generation: u64, entries: &[KvIndexEntry]) -> Result<Vec<u8>> {
        let mut sorted: Vec<&KvIndexEntry> = entries.iter().collect();
        sorted.sort_by_key(|entry| entry.key());
        sorted.dedup_by_key(|entry| entry.key());
        let mut body = Vec::with_capacity(
            KV_INDEX_HEADER_BYTES + sorted.len() * KV_INDEX_ENTRY_BYTES + KV_INDEX_TRAILER_BYTES,
        );
        body.extend_from_slice(KV_INDEX_MAGIC);
        body.extend_from_slice(&KV_INDEX_VERSION.to_le_bytes());
        body.extend_from_slice(&kind.as_u16().to_le_bytes());
        body.extend_from_slice(&generation.to_le_bytes());
        body.extend_from_slice(&(sorted.len() as u64).to_le_bytes());
        body.extend_from_slice(&[0u8; 8]);
        for entry in &sorted {
            entry.encode_into(&mut body);
        }
        let digest = identity::h(DOMAIN_KV_INDEX_SEGMENT, &body);
        body.extend_from_slice(digest.as_bytes());
        Ok(body)
    }

    /// Decode and verify a segment. Any structural or digest failure is
    /// `Corrupt`; the caller drops the whole index.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < KV_INDEX_HEADER_BYTES + KV_INDEX_TRAILER_BYTES {
            return Err(Error::corrupt("kv index segment is truncated"));
        }
        if &bytes[0..4] != KV_INDEX_MAGIC {
            return Err(Error::corrupt("kv index segment magic is wrong"));
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != KV_INDEX_VERSION {
            return Err(Error::corrupt(format!(
                "kv index segment version {version} is not supported"
            )));
        }
        let kind = SegmentKind::from_u16(u16::from_le_bytes([bytes[6], bytes[7]]))?;
        let generation = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let count = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        let body_len = KV_INDEX_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(KV_INDEX_ENTRY_BYTES)
                    .ok_or_else(|| Error::corrupt("kv index segment count overflows"))?,
            )
            .ok_or_else(|| Error::corrupt("kv index segment length overflows"))?;
        if bytes.len() != body_len + KV_INDEX_TRAILER_BYTES {
            return Err(Error::corrupt(
                "kv index segment length does not match its entry count",
            ));
        }
        let declared = Hash32::from_bytes(bytes[body_len..].try_into().unwrap());
        let computed = identity::h(DOMAIN_KV_INDEX_SEGMENT, &bytes[..body_len]);
        if declared != computed {
            return Err(Error::corrupt("kv index segment digest does not match"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut previous: Option<(Hash32, Hash32)> = None;
        for index in 0..count {
            let at = KV_INDEX_HEADER_BYTES + index * KV_INDEX_ENTRY_BYTES;
            let entry = KvIndexEntry::decode(&bytes[at..at + KV_INDEX_ENTRY_BYTES])?;
            let key = entry.key();
            if previous.is_some_and(|prev| prev > key) {
                return Err(Error::corrupt("kv index segment entries are not sorted"));
            }
            if previous == Some(key) {
                return Err(Error::corrupt("kv index segment has a duplicate entry"));
            }
            previous = Some(key);
            entries.push(entry);
        }
        Ok(KvIndexSegment {
            kind,
            generation,
            entries,
            digest: computed,
        })
    }
}

/// One segment reference in the root record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvIndexSegmentRef {
    pub kind: SegmentKind,
    pub generation: u64,
    pub entries: u64,
    pub digest: Hash32,
}

/// The mutable index root record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvIndexRoot {
    /// Store generation this index describes.
    pub generation: u64,
    /// Store head commit this index describes.
    pub head: Hash32,
    /// Generation the base segment was built from.
    pub built_for_generation: u64,
    /// Head the base segment was built from.
    pub built_for_head: Hash32,
    /// Live entries after applying every delta.
    pub entry_count: u64,
    /// XOR accumulator over the live entries (`H` of each entry's bytes).
    pub entries_accumulator: Hash32,
    /// Segments in application order (base first).
    pub segments: Vec<KvIndexSegmentRef>,
    /// Total encoded bytes of every named segment.
    pub bytes: u64,
    /// Revocation epoch observed at build time (diagnostics and freshness).
    pub revocation_epoch: u64,
    /// Prefixes skipped because their last page is a partial tail. When this is
    /// non-zero the index cannot answer a *miss* on its own: the oracle decides.
    pub skipped_partial: u64,
}

fn hash_text(value: Hash32) -> Value {
    Value::Str(value.to_hex())
}

fn parse_hash(text: &str, what: &str) -> Result<Hash32> {
    Hash32::from_hex(text).map_err(|_| Error::corrupt(format!("{what} is not a 64-hex digest")))
}

impl KvIndexRoot {
    fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert(
            "schema".to_owned(),
            Value::Str(KV_INDEX_ROOT_SCHEMA.to_owned()),
        );
        map.insert("generation".to_owned(), Value::U64(self.generation));
        map.insert("head".to_owned(), hash_text(self.head));
        map.insert(
            "built_for_generation".to_owned(),
            Value::U64(self.built_for_generation),
        );
        map.insert("built_for_head".to_owned(), hash_text(self.built_for_head));
        map.insert("entry_count".to_owned(), Value::U64(self.entry_count));
        map.insert(
            "entries_accumulator".to_owned(),
            hash_text(self.entries_accumulator),
        );
        map.insert("bytes".to_owned(), Value::U64(self.bytes));
        map.insert(
            "revocation_epoch".to_owned(),
            Value::U64(self.revocation_epoch),
        );
        map.insert(
            "skipped_partial".to_owned(),
            Value::U64(self.skipped_partial),
        );
        map.insert(
            "segments".to_owned(),
            Value::Array(
                self.segments
                    .iter()
                    .map(|segment| {
                        let mut inner = BTreeMap::new();
                        inner.insert("kind".to_owned(), Value::U64(segment.kind.as_u16() as u64));
                        inner.insert("generation".to_owned(), Value::U64(segment.generation));
                        inner.insert("entries".to_owned(), Value::U64(segment.entries));
                        inner.insert("digest".to_owned(), hash_text(segment.digest));
                        Value::Map(inner)
                    })
                    .collect(),
            ),
        );
        Value::Map(map)
    }

    /// Canonical WCB bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wcb::encode(&self.to_value(), &Limits::default()).map_err(Error::from)
    }

    /// Strict decode.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        const KEYS: usize = 11;
        let value = wcb::decode(bytes, &Limits::default()).map_err(Error::from)?;
        let Value::Map(map) = value else {
            return Err(Error::corrupt("kv index root is not a map"));
        };
        if map.len() != KEYS {
            return Err(Error::corrupt("kv index root has an unknown key set"));
        }
        let text = |key: &str| -> Result<String> {
            match map.get(key) {
                Some(Value::Str(value)) => Ok(value.clone()),
                _ => Err(Error::corrupt(format!("kv index root `{key}` is not text"))),
            }
        };
        let number = |key: &str| -> Result<u64> {
            match map.get(key) {
                Some(Value::U64(value)) => Ok(*value),
                _ => Err(Error::corrupt(format!("kv index root `{key}` is not u64"))),
            }
        };
        if text("schema")? != KV_INDEX_ROOT_SCHEMA {
            return Err(Error::corrupt("unknown kv index root schema"));
        }
        let segments = match map.get("segments") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    let Value::Map(inner) = item else {
                        return Err(Error::corrupt("kv index segment ref is not a map"));
                    };
                    if inner.len() != 4 {
                        return Err(Error::corrupt(
                            "kv index segment ref has an unknown key set",
                        ));
                    }
                    let num = |key: &str| match inner.get(key) {
                        Some(Value::U64(value)) => Ok(*value),
                        _ => Err(Error::corrupt("kv index segment ref field is not u64")),
                    };
                    let dig = |key: &str| match inner.get(key) {
                        Some(Value::Str(value)) => parse_hash(value, "kv index segment digest"),
                        _ => Err(Error::corrupt("kv index segment ref field is not text")),
                    };
                    Ok(KvIndexSegmentRef {
                        kind: SegmentKind::from_u16(u16::try_from(num("kind")?).map_err(
                            |_| Error::corrupt("kv index segment kind does not fit u16"),
                        )?)?,
                        generation: num("generation")?,
                        entries: num("entries")?,
                        digest: dig("digest")?,
                    })
                })
                .collect::<Result<Vec<KvIndexSegmentRef>>>()?,
            _ => return Err(Error::corrupt("kv index root `segments` is not an array")),
        };
        Ok(KvIndexRoot {
            generation: number("generation")?,
            head: parse_hash(&text("head")?, "kv index head")?,
            built_for_generation: number("built_for_generation")?,
            built_for_head: parse_hash(&text("built_for_head")?, "kv index built_for_head")?,
            entry_count: number("entry_count")?,
            entries_accumulator: parse_hash(
                &text("entries_accumulator")?,
                "kv index entries accumulator",
            )?,
            segments,
            bytes: number("bytes")?,
            revocation_epoch: number("revocation_epoch")?,
            skipped_partial: number("skipped_partial")?,
        })
    }
}

/// The canonical identity of a prefix across every compatibility field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvPrefixIdentity {
    pub tenant: String,
    pub model: String,
    pub tokenizer_hash: Hash32,
    pub attention_hash: Hash32,
    pub dtype: String,
    pub head_count: u32,
    pub head_dim: u32,
    pub layer_count: u32,
    pub page_rows: u64,
}

impl KvPrefixIdentity {
    /// From a stored record.
    pub fn of_record(record: &KvPrefixRecord) -> Self {
        KvPrefixIdentity {
            tenant: record.tenant.clone(),
            model: record.model.clone(),
            tokenizer_hash: record.tokenizer_hash,
            attention_hash: record.attention_hash,
            dtype: record.dtype.clone(),
            head_count: record.head_count,
            head_dim: record.head_dim,
            layer_count: record.layer_count,
            page_rows: record.page_rows,
        }
    }

    /// From a request.
    pub fn of_request(request: &KvPrefixRequest) -> Self {
        KvPrefixIdentity {
            tenant: request.tenant.clone(),
            model: request.model.clone(),
            tokenizer_hash: request.tokenizer_hash,
            attention_hash: request.attention_hash,
            dtype: request.dtype.clone(),
            head_count: request.head_count,
            head_dim: request.head_dim,
            layer_count: request.layer_count,
            page_rows: request.page_rows,
        }
    }

    fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert(
            "schema".to_owned(),
            Value::Str("weft.kv.prefix_identity.v1".to_owned()),
        );
        map.insert("tenant".to_owned(), Value::Str(self.tenant.clone()));
        map.insert("model".to_owned(), Value::Str(self.model.clone()));
        map.insert("tokenizer_hash".to_owned(), hash_text(self.tokenizer_hash));
        map.insert("attention_hash".to_owned(), hash_text(self.attention_hash));
        map.insert("dtype".to_owned(), Value::Str(self.dtype.clone()));
        map.insert(
            "head_count".to_owned(),
            Value::U64(u64::from(self.head_count)),
        );
        map.insert("head_dim".to_owned(), Value::U64(u64::from(self.head_dim)));
        map.insert(
            "layer_count".to_owned(),
            Value::U64(u64::from(self.layer_count)),
        );
        map.insert("page_rows".to_owned(), Value::U64(self.page_rows));
        Value::Map(map)
    }

    /// `H("weft/kv/prefix-identity/v1", WCB(identity))`.
    pub fn root(&self) -> Hash32 {
        // The value is built from owned strings; encoding cannot fail for
        // these bounded fields, and a failure would be a programming error.
        let bytes = wcb::encode(&self.to_value(), &Limits::default())
            .expect("canonical prefix identity encoding cannot fail");
        identity::h(DOMAIN_KV_PREFIX_IDENTITY, &bytes)
    }
}

/// One entry's contribution to the root accumulator.
pub fn entry_digest(entry: &KvIndexEntry) -> Hash32 {
    let mut bytes = Vec::with_capacity(KV_INDEX_ENTRY_BYTES);
    entry.encode_into(&mut bytes);
    identity::h(DOMAIN_KV_INDEX_ENTRY, &bytes)
}

/// XOR accumulator over the live entries the root names.
///
/// It is **layout independent**: a rebuilt single base and a base-plus-deltas
/// index over the same live set produce the same value, because XOR is
/// commutative. That is what lets a delta update the attestation in O(delta)
/// instead of re-hashing every entry. It is a multiset accumulator, not a
/// Merkle root: two different live sets with the same XOR would require a
/// second-preimage on BLAKE3 for every differing entry.
pub fn entries_accumulator(entries: &[KvIndexEntry]) -> Hash32 {
    let mut bytes = [0u8; 32];
    for entry in entries {
        let contribution = entry_digest(entry);
        for (out, part) in bytes.iter_mut().zip(contribution.as_bytes()) {
            *out ^= part;
        }
    }
    Hash32::from_bytes(bytes)
}

/// Fold one entry's contribution into an accumulator.
pub fn accumulate(accumulator: Hash32, entry: &KvIndexEntry) -> Hash32 {
    let contribution = entry_digest(entry);
    let mut bytes = [0u8; 32];
    for index in 0..32 {
        bytes[index] = accumulator.as_bytes()[index] ^ contribution.as_bytes()[index];
    }
    Hash32::from_bytes(bytes)
}

/// A merged, validated in-memory index.
#[derive(Clone, Debug)]
pub struct LoadedKvIndex {
    /// Generation this view is valid for.
    pub generation: u64,
    /// Head this view is valid for.
    pub head: Hash32,
    /// Live entries sorted by `(identity_root, chain_root)`.
    pub entries: Vec<KvIndexEntry>,
    /// Distinct identities covered.
    pub identities: u64,
    /// Segment count applied.
    pub segments: u64,
    /// Encoded segment bytes.
    pub bytes: u64,
    /// Revocation epoch at load time.
    pub revocation_epoch: u64,
    /// Prefixes the index does not cover (partial tails).
    pub skipped_partial: u64,
}

impl LoadedKvIndex {
    fn with_identity<'a>(&'a self, identity: Hash32) -> &'a [KvIndexEntry] {
        let start = self
            .entries
            .partition_point(|entry| entry.identity_root < identity);
        let end = self
            .entries
            .partition_point(|entry| entry.identity_root <= identity);
        &self.entries[start..end]
    }

    fn get(&self, identity: Hash32, chain_root: Hash32) -> Option<&KvIndexEntry> {
        let slice = self.with_identity(identity);
        slice
            .binary_search_by_key(&chain_root, |entry| entry.chain_root)
            .ok()
            .map(|index| &slice[index])
    }
}

/// Index diagnostics for one handle.
#[derive(Clone, Debug, Default)]
pub struct KvIndexStats {
    /// Loads that returned a valid index.
    pub loads: u64,
    /// Loads refused by a digest, generation, head or count check.
    pub rejects: u64,
    /// Full rebuilds performed (explicit or after a stale/missing index).
    pub rebuilds: u64,
    /// Indexed lookups answered.
    pub lookups: u64,
    /// Boundary probes issued by indexed lookups.
    pub probes: u64,
    /// Candidate entries rejected because their record did not verify.
    pub rejected_candidates: u64,
    /// Indexed lookups that fell back to the oracle scan.
    pub fallbacks: u64,
    /// Delta segments appended after a commit.
    pub deltas_appended: u64,
    /// Notes (bounded) that explain the counters above.
    pub notes: Vec<String>,
}

impl KvIndexStats {
    fn note(&mut self, text: String) {
        if self.notes.len() < 64 {
            self.notes.push(text);
        }
    }
}

/// One build report.
#[derive(Clone, Debug)]
pub struct KvIndexBuildReport {
    pub generation: u64,
    pub head: Hash32,
    pub built_for_generation: u64,
    pub built_for_head: Hash32,
    pub entries: u64,
    pub identities: u64,
    pub segments: u64,
    pub bytes: u64,
    pub skipped_partial: u64,
    /// Prefixes skipped because they are revoked and can never be returned.
    pub skipped_revoked: u64,
    pub removed_segments: u64,
    pub build_ms: f64,
    pub revocation_epoch: u64,
}

/// A present/absent index status.
#[derive(Clone, Debug)]
pub struct KvIndexStatus {
    pub present: bool,
    pub valid: bool,
    pub generation: u64,
    pub index_generation: Option<u64>,
    pub index_head: Option<Hash32>,
    pub entries: u64,
    pub identities: u64,
    pub segments: u64,
    pub bytes: u64,
    pub revocation_epoch: Option<u64>,
    /// Prefixes the index does not cover (partial tails).
    pub skipped_partial: u64,
    /// `H("weft/kv/index-root/v1", root canonical bytes)` when a root exists.
    pub index_digest: Option<Hash32>,
    pub notes: Vec<String>,
}

impl Store {
    /// Where index segments and the root live: the backend's `kv-index/`
    /// namespace, outside committed state so an index write is not a commit.
    fn kv_index_note(&self, text: String) {
        let mut stats = self
            .kv_index_stats()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stats.note(text);
    }

    /// Read the stored root record, if any.
    fn kv_index_read_root(&self) -> Result<Option<KvIndexRoot>> {
        let bytes = match self.with_backend(|backend| backend.read_all(&Key::kv_index_root())) {
            Ok(bytes) => bytes,
            Err(err) if err.class() == Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(Some(KvIndexRoot::from_canonical_bytes(&bytes)?))
    }

    fn kv_index_read_segment(&self, digest: Hash32) -> Result<KvIndexSegment> {
        let bytes = self
            .with_backend(|backend| backend.read_all(&Key::kv_index(digest)))
            .map_err(|err| {
                Error::corrupt(format!("kv index segment {digest} is unreadable: {err}"))
            })?;
        if bytes.len() as u64 > MAX_KV_INDEX_BYTES {
            return Err(Error::limit_exceeded(
                "kv index segment exceeds the configured byte bound",
            ));
        }
        let segment = KvIndexSegment::decode(&bytes)?;
        if segment.digest != digest {
            return Err(Error::corrupt(
                "kv index segment is stored under a different digest",
            ));
        }
        Ok(segment)
    }

    /// Load and validate the persisted index for the current generation.
    ///
    /// Every failure returns `None` and is counted as a reject; nothing partial
    /// is returned.
    fn kv_index_load_inner(&self, note_reject: bool) -> Option<LoadedKvIndex> {
        let root = match self.kv_index_read_root() {
            Ok(Some(root)) => root,
            Ok(None) => return None,
            Err(err) => {
                if note_reject {
                    self.kv_index_note(format!("kv index root refused: {err}"));
                }
                self.bump_kv_index(|stats| stats.rejects += 1);
                return None;
            }
        };
        let generation = self.generation();
        if root.generation != generation || root.head != self.head() {
            if note_reject {
                self.kv_index_note(format!(
                    "kv index is stale: it describes generation {} / head {}, store is at {} / {}",
                    root.generation,
                    root.head.to_hex(),
                    generation,
                    self.head().to_hex()
                ));
            }
            self.bump_kv_index(|stats| stats.rejects += 1);
            return None;
        }
        if root.segments.is_empty() || root.segments.len() > 1 + MAX_KV_INDEX_DELTAS as usize {
            self.bump_kv_index(|stats| stats.rejects += 1);
            return None;
        }
        if root.segments[0].kind != SegmentKind::Base
            || root.segments[1..]
                .iter()
                .any(|segment| segment.kind != SegmentKind::Delta)
            || root.segments[1..]
                .windows(2)
                .any(|pair| pair[0].generation >= pair[1].generation)
        {
            self.kv_index_note("kv index segment chain is malformed".to_owned());
            self.bump_kv_index(|stats| stats.rejects += 1);
            return None;
        }
        let mut segments = Vec::with_capacity(root.segments.len());
        let mut bytes = 0u64;
        for reference in &root.segments {
            let segment = match self.kv_index_read_segment(reference.digest) {
                Ok(segment) => segment,
                Err(err) => {
                    self.kv_index_note(err.message().to_owned());
                    self.bump_kv_index(|stats| stats.rejects += 1);
                    return None;
                }
            };
            if segment.kind != reference.kind
                || segment.generation != reference.generation
                || segment.entries.len() as u64 != reference.entries
            {
                self.kv_index_note("kv index segment does not match its root reference".to_owned());
                self.bump_kv_index(|stats| stats.rejects += 1);
                return None;
            }
            bytes = bytes.saturating_add(
                (KV_INDEX_HEADER_BYTES
                    + segment.entries.len() * KV_INDEX_ENTRY_BYTES
                    + KV_INDEX_TRAILER_BYTES) as u64,
            );
            segments.push(segment);
        }
        if bytes != root.bytes {
            self.kv_index_note("kv index byte accounting does not match".to_owned());
            self.bump_kv_index(|stats| stats.rejects += 1);
            return None;
        }
        let entries = match merge_segments(&segments) {
            Ok(entries) => entries,
            Err(err) => {
                self.kv_index_note(err.message().to_owned());
                self.bump_kv_index(|stats| stats.rejects += 1);
                return None;
            }
        };
        if entries.len() as u64 != root.entry_count
            || entries_accumulator(&entries) != root.entries_accumulator
        {
            self.kv_index_note("kv index entries do not match the root attestation".to_owned());
            self.bump_kv_index(|stats| stats.rejects += 1);
            return None;
        }
        let identities = entries
            .iter()
            .map(|entry| entry.identity_root)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u64;
        self.bump_kv_index(|stats| stats.loads += 1);
        Some(LoadedKvIndex {
            generation,
            head: self.head(),
            entries,
            identities,
            segments: root.segments.len() as u64,
            bytes,
            revocation_epoch: root.revocation_epoch,
            skipped_partial: root.skipped_partial,
        })
    }

    /// The loaded index for this generation (cached), or `None` when absent,
    /// stale or refused by any attestation check.
    ///
    /// An `Arc` is returned so a lookup never copies the entry vector.
    fn kv_index_loaded(&self) -> Option<std::sync::Arc<LoadedKvIndex>> {
        {
            let cache = self
                .kv_index_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(loaded) = cache.as_ref() {
                if loaded.generation == self.generation() && loaded.head == self.head() {
                    return Some(std::sync::Arc::clone(loaded));
                }
            }
        }
        let loaded = std::sync::Arc::new(self.kv_index_load_inner(true)?);
        let mut cache = self
            .kv_index_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *cache = Some(std::sync::Arc::clone(&loaded));
        Some(loaded)
    }

    fn kv_index_clear_cache(&self) {
        let mut cache = self
            .kv_index_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *cache = None;
    }

    /// Build (or rebuild) the index from the oracle prefix list in one pass.
    ///
    /// Writes the base segment, then the root record. A crash before the root
    /// leaves unreferenced segments and the previous (now stale) root, so the
    /// next reader rebuilds rather than serving a partial index.
    pub fn kv_index_build(&mut self) -> Result<KvIndexBuildReport> {
        let started = Instant::now();
        self.require_writable()?;
        let prefixes = self.kv_prefix_records_with_ids()?;
        let generation = self.generation();
        let head = self.head();
        let revocation_epoch = self.kv_revocation_epoch()?;
        let revoked = kv_revocation_ids(self)?;
        let mut entries = Vec::with_capacity(prefixes.len());
        let mut identities = std::collections::BTreeSet::new();
        let mut skipped_partial = 0u64;
        let mut skipped_revoked = 0u64;
        for (_, object_id, record) in &prefixes {
            let prefix_id = record.id()?;
            if revoked.contains(&prefix_id) {
                // A revoked prefix can never be returned, so it is not
                // indexed: revocation shrinks the index instead of filtering it
                // on every lookup.
                skipped_revoked += 1;
                continue;
            }
            if record.position_end % record.page_rows != 0 {
                skipped_partial += 1;
                continue;
            }
            let identity = KvPrefixIdentity::of_record(record);
            let identity_root = identity.root();
            identities.insert(identity_root);
            entries.push(KvIndexEntry {
                identity_root,
                chain_root: record.token_chain_root,
                record_prefix_id: prefix_id,
                record_object_id: *object_id,
                position_end: record.position_end,
                page_count: record.page_count,
                kind: KvIndexEntryKind::Live,
            });
        }
        if entries.len() > MAX_KV_INDEX_ENTRIES {
            return Err(Error::limit_exceeded(format!(
                "kv index would hold {} entries, above the {MAX_KV_INDEX_ENTRIES} bound",
                entries.len()
            )));
        }
        let base = KvIndexSegment::encode(SegmentKind::Base, generation, &entries)?;
        if base.len() as u64 > MAX_KV_INDEX_BYTES {
            return Err(Error::limit_exceeded(format!(
                "kv index base segment is {} bytes, above the {MAX_KV_INDEX_BYTES} bound",
                base.len()
            )));
        }
        let base_digest = identity::h(DOMAIN_KV_INDEX_SEGMENT, &base[..base.len() - 32]);
        let live: Vec<KvIndexEntry> = {
            let mut sorted = entries.clone();
            sorted.sort_by_key(|entry| entry.key());
            sorted
        };
        let root = KvIndexRoot {
            generation,
            head,
            built_for_generation: generation,
            built_for_head: head,
            entry_count: live.len() as u64,
            entries_accumulator: entries_accumulator(&live),
            segments: vec![KvIndexSegmentRef {
                kind: SegmentKind::Base,
                generation,
                entries: live.len() as u64,
                digest: base_digest,
            }],
            bytes: base.len() as u64,
            revocation_epoch,
            skipped_partial,
        };
        // Segments first, root second: a crash between them leaves an
        // unreferenced segment, never a root that names a missing segment.
        self.with_backend_mut(|backend| backend.put_atomic(&Key::kv_index(base_digest), &base))?;
        kv_index_fault("after-base-segment");
        let removed = self.kv_index_write_root(&root)?;
        kv_index_fault("after-root");
        self.bump_kv_index(|stats| stats.rebuilds += 1);
        self.kv_index_clear_cache();
        Ok(KvIndexBuildReport {
            generation,
            head,
            built_for_generation: generation,
            built_for_head: head,
            entries: live.len() as u64,
            identities: identities.len() as u64,
            segments: 1,
            bytes: base.len() as u64,
            skipped_partial,
            skipped_revoked,
            removed_segments: removed,
            build_ms: started.elapsed().as_secs_f64() * 1000.0,
            revocation_epoch,
        })
    }

    /// Publish the root and delete segments it no longer names.
    fn kv_index_write_root(&self, root: &KvIndexRoot) -> Result<u64> {
        let bytes = root.canonical_bytes()?;
        self.with_backend_mut(|backend| backend.put_replace(&Key::kv_index_root(), &bytes))?;
        let named: std::collections::BTreeSet<Hash32> =
            root.segments.iter().map(|segment| segment.digest).collect();
        let existing =
            self.with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::KvIndex)))?;
        let mut removed = 0u64;
        for key in existing {
            let digest = match key {
                Key::KvIndex { hash } => hash,
                _ => continue,
            };
            if named.contains(&digest) {
                continue;
            }
            match self.with_backend_mut(|backend| backend.delete(&Key::kv_index(digest))) {
                Ok(()) => removed += 1,
                Err(err) if err.class() == Class::NotFound => {}
                Err(_) => {
                    // A leaked segment is harmless: the root never names it and
                    // the next rebuild retries. Never fail the build for it.
                }
            }
        }
        Ok(removed)
    }

    /// Append a one-generation delta after a committed mutation.
    ///
    /// Called after the commit that produced `generation`; if the stored index
    /// is not the immediately preceding generation (absent, stale, already
    /// compacted, or above the delta bound) nothing is written and the index is
    /// simply left stale. A write failure is a note, never a failed commit.
    ///
    /// The new attestation is computed in `O(delta)`: `entry_count` moves by the
    /// number of live slots added/removed and the accumulator XORs the removed
    /// entry out and the added entry in. No stored record is re-read.
    fn kv_index_append_delta(
        &self,
        generation: u64,
        previous_head: Hash32,
        entries: Vec<KvIndexEntry>,
        slots_added: u64,
        slots_removed: u64,
    ) {
        if self.mode() != OpenMode::ReadWrite || entries.is_empty() {
            return;
        }
        let root = match self.kv_index_read_root() {
            Ok(Some(root)) => root,
            _ => return,
        };
        if root.generation + 1 != generation || root.head != previous_head {
            return;
        }
        if root.segments.len() >= 1 + MAX_KV_INDEX_DELTAS as usize {
            self.kv_index_note(format!(
                "kv index has {} segments; the next mutation leaves it stale until `kv index-build`",
                root.segments.len()
            ));
            return;
        }
        let encoded = match KvIndexSegment::encode(SegmentKind::Delta, generation, &entries) {
            Ok(encoded) => encoded,
            Err(err) => {
                self.kv_index_note(format!("kv index delta not encoded: {err}"));
                return;
            }
        };
        if encoded.len() as u64 > MAX_KV_INDEX_BYTES {
            self.kv_index_note("kv index delta exceeds the byte bound".to_owned());
            return;
        }
        let digest = match encoded
            .get(encoded.len().saturating_sub(32)..)
            .and_then(|trailer| trailer.try_into().ok())
            .map(Hash32::from_bytes)
        {
            Some(digest) => digest,
            None => return,
        };
        if let Err(err) = self
            .with_backend_mut(|backend| backend.put_atomic(&Key::kv_index(digest), &encoded))
        {
            self.kv_index_note(format!("kv index delta not published: {err}"));
            return;
        }
        let mut accumulator = root.entries_accumulator;
        for entry in &entries {
            // XOR is its own inverse, so the same fold removes a tombstone's
            // pre-image and adds a live entry.
            accumulator = accumulate(accumulator, entry);
        }
        let entry_count = root
            .entry_count
            .saturating_add(slots_added)
            .saturating_sub(slots_removed);
        let mut segments = root.segments.clone();
        segments.push(KvIndexSegmentRef {
            kind: SegmentKind::Delta,
            generation,
            entries: entries.len() as u64,
            digest,
        });
        let next = KvIndexRoot {
            generation,
            head: self.head(),
            built_for_generation: root.built_for_generation,
            built_for_head: root.built_for_head,
            entry_count,
            entries_accumulator: accumulator,
            segments,
            bytes: root.bytes.saturating_add(encoded.len() as u64),
            revocation_epoch: self.kv_revocation_epoch().unwrap_or(root.revocation_epoch),
            skipped_partial: root.skipped_partial,
        };
        if let Err(err) = self.kv_index_write_root(&next) {
            self.kv_index_note(format!("kv index root not updated: {err}"));
            return;
        }
        self.bump_kv_index(|stats| stats.deltas_appended += 1);
        self.kv_index_clear_cache();
    }

    /// Record a `kv_prefix` mutation in the index (bounded: one delta per
    /// commit, appended after the commit has published).
    ///
    /// `previous` is the record the name held before the commit (with its
    /// object id); when it was indexed its exact entry is folded out so the
    /// accumulator stays exact. A partial-tail record is not indexed, so it
    /// contributes no entry in either direction.
    pub(crate) fn kv_index_note_put(
        &self,
        generation: u64,
        previous_head: Hash32,
        previous: Option<(&KvPrefixRecord, Hash32)>,
        record: &KvPrefixRecord,
        record_object_id: Hash32,
    ) {
        let mut entries = Vec::new();
        let mut removed = 0u64;
        let mut added = 0u64;
        if let Some((previous, previous_object)) = previous {
            if previous.position_end % previous.page_rows == 0 {
                entries.push(KvIndexEntry {
                    identity_root: KvPrefixIdentity::of_record(previous).root(),
                    chain_root: previous.token_chain_root,
                    record_prefix_id: match previous.id() {
                        Ok(id) => id,
                        Err(_) => return,
                    },
                    record_object_id: previous_object,
                    position_end: previous.position_end,
                    page_count: previous.page_count,
                    kind: KvIndexEntryKind::Live,
                });
                removed = 1;
            }
        }
        if record.position_end % record.page_rows == 0 {
            entries.push(KvIndexEntry {
                identity_root: KvPrefixIdentity::of_record(record).root(),
                chain_root: record.token_chain_root,
                record_prefix_id: match record.id() {
                    Ok(id) => id,
                    Err(_) => return,
                },
                record_object_id,
                position_end: record.position_end,
                page_count: record.page_count,
                kind: KvIndexEntryKind::Live,
            });
            added = 1;
        }
        // The entry vector is sorted before encoding, so one boundary key can
        // only appear once; a replace is (removed=1, added=1).
        self.kv_index_append_delta(generation, previous_head, entries, added, removed);
    }

    /// Record a whole batch write (many prefixes, one commit) as one delta.
    pub(crate) fn kv_index_note_batch(
        &self,
        generation: u64,
        previous_head: Hash32,
        puts: &[(Option<(&KvPrefixRecord, Hash32)>, &KvPrefixRecord, Hash32)],
    ) {
        type Slot = (Option<KvIndexEntry>, Option<KvIndexEntry>);
        let mut slots: BTreeMap<(Hash32, Hash32), Slot> = BTreeMap::new();
        let indexed = |record: &KvPrefixRecord, object: Hash32| -> Option<KvIndexEntry> {
            if record.position_end % record.page_rows != 0 {
                return None;
            }
            Some(KvIndexEntry {
                identity_root: KvPrefixIdentity::of_record(record).root(),
                chain_root: record.token_chain_root,
                record_prefix_id: record.id().ok()?,
                record_object_id: object,
                position_end: record.position_end,
                page_count: record.page_count,
                kind: KvIndexEntryKind::Live,
            })
        };
        for (previous, record, object_id) in puts {
            if let Some((previous, previous_object)) = previous {
                if let Some(entry) = indexed(previous, *previous_object) {
                    let key = entry.key();
                    slots.entry(key).or_default().0 = Some(entry);
                }
            }
            if let Some(entry) = indexed(record, *object_id) {
                let key = entry.key();
                slots.entry(key).or_default().1 = Some(entry);
            }
        }
        let mut entries = Vec::new();
        let mut added = 0u64;
        let mut removed = 0u64;
        for (_, (old, new)) in slots {
            match (old, new) {
                (Some(old), Some(new)) if old == new => {}
                (Some(old), Some(new)) => {
                    entries.push(old);
                    entries.push(new);
                    removed += 1;
                    added += 1;
                }
                (Some(old), None) => {
                    entries.push(old);
                    removed += 1;
                }
                (None, Some(new)) => {
                    entries.push(new);
                    added += 1;
                }
                (None, None) => {}
            }
        }
        self.kv_index_append_delta(generation, previous_head, entries, added, removed);
    }

    /// Record a deletion/revocation tombstone.
    pub(crate) fn kv_index_note_tombstone(
        &self,
        generation: u64,
        previous_head: Hash32,
        record: &KvPrefixRecord,
        record_object_id: Hash32,
    ) {
        if record.position_end % record.page_rows != 0 {
            return;
        }
        let entry = KvIndexEntry {
            identity_root: KvPrefixIdentity::of_record(record).root(),
            chain_root: record.token_chain_root,
            record_prefix_id: match record.id() {
                Ok(id) => id,
                Err(_) => return,
            },
            record_object_id,
            position_end: record.position_end,
            page_count: record.page_count,
            kind: KvIndexEntryKind::Live,
        };
        // Fold the exact live entry out of the accumulator.
        self.kv_index_append_delta(generation, previous_head, vec![entry], 0, 1);
    }

    /// Status of the persisted index without rebuilding.
    pub fn kv_index_status(&self) -> Result<KvIndexStatus> {
        let generation = self.generation();
        let root = self.kv_index_read_root()?;
        let notes = self
            .kv_index_stats()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .notes
            .clone();
        let Some(root) = root else {
            return Ok(KvIndexStatus {
                present: false,
                valid: false,
                generation,
                index_generation: None,
                index_head: None,
                entries: 0,
                identities: 0,
                segments: 0,
                bytes: 0,
                revocation_epoch: None,
                skipped_partial: 0,
                index_digest: None,
                notes,
            });
        };
        let loaded = self.kv_index_load_inner(false);
        Ok(KvIndexStatus {
            present: true,
            valid: loaded.is_some(),
            generation,
            index_generation: Some(root.generation),
            index_head: Some(root.head),
            entries: root.entry_count,
            identities: loaded.as_ref().map(|index| index.identities).unwrap_or(0),
            segments: root.segments.len() as u64,
            bytes: root.bytes,
            revocation_epoch: Some(root.revocation_epoch),
            skipped_partial: root.skipped_partial,
            index_digest: Some(identity::h(DOMAIN_KV_INDEX_ROOT, &root.canonical_bytes()?)),
            notes,
        })
    }

    /// Drop the index (root and segments). The store is unaffected.
    pub fn kv_index_drop(&mut self) -> Result<u64> {
        self.require_writable()?;
        let existing =
            self.with_backend(|backend| backend.list_prefix(&Key::prefix(KeyKind::KvIndex)))?;
        let mut removed = 0u64;
        for key in existing {
            if self
                .with_backend_mut(|backend| backend.delete(&key))
                .is_ok()
            {
                removed += 1;
            }
        }
        let _ = self.with_backend_mut(|backend| backend.delete(&Key::kv_index_root()));
        self.kv_index_clear_cache();
        Ok(removed)
    }

    /// Indexed lookup: the same answer as [`Store::kv_prefix_lookup`], without
    /// the prefix-record scan.
    ///
    /// Falls back to the oracle scan when the index is absent/stale or when
    /// every candidate entry fails verification. The fallback is counted and
    /// noted; it is never silent, but it is also never wrong.
    pub fn kv_prefix_lookup_indexed(&self, request: &KvPrefixRequest) -> Result<KvLookup> {
        if request.tokens.is_empty() {
            return Err(Error::invalid_argument(
                "kv lookup needs at least one token",
            ));
        }
        if request.tokens.len() as u64 > MAX_KV_PREFIX_TOKENS {
            return Err(Error::limit_exceeded(format!(
                "kv lookup token length exceeds {MAX_KV_PREFIX_TOKENS}"
            )));
        }
        let Some(index) = self.kv_index_loaded() else {
            self.bump_kv_index(|stats| stats.fallbacks += 1);
            return self.kv_prefix_lookup_scan(request);
        };
        let identity = KvPrefixIdentity::of_request(request);
        let identity_root = identity.root();
        let page_rows = request.page_rows;
        if page_rows == 0 {
            return Err(Error::invalid_argument(
                "kv lookup page_rows must be positive",
            ));
        }
        let revoked = kv_revocation_ids(self)?;
        let mut probes = 0u64;
        let mut rejected = Vec::new();
        let mut best: Option<KvPrefixRecord> = None;
        let mut best_object = Hash32::ZERO;
        let mut blocks = 0usize;
        // Walk page boundaries once, carrying the frozen token chain.
        let mut chain_root = crate::kv_prefix::kv_token_chain_root(&[]);
        let mut position = 0u64;
        loop {
            let remaining = request.tokens.len() as u64 - position;
            let step = remaining.min(page_rows);
            chain_root = kv_prefix_chain_root_from(chain_root, &request.tokens, position, step);
            position += step;
            blocks += 1;
            if blocks > MAX_KV_INDEX_PROBES {
                self.kv_index_note(format!(
                    "kv index probe bound {MAX_KV_INDEX_PROBES} reached; later boundaries were \
                     not probed"
                ));
                break;
            }
            probes += 1;
            let Some(entry) = index.get(identity_root, chain_root) else {
                if position >= request.tokens.len() as u64 {
                    break;
                }
                continue;
            };
            // Verify the candidate reference before use.
            match self.kv_index_verify_entry(entry, request, &revoked) {
                Ok(Some(record)) => {
                    if best
                        .as_ref()
                        .map(|candidate| record.position_end > candidate.position_end)
                        .unwrap_or(true)
                    {
                        best_object = entry.record_object_id;
                        best = Some(record);
                    }
                }
                Ok(None) => {
                    rejected.push(format!(
                        "entry at {} tokens refers to a revoked prefix",
                        entry.position_end
                    ));
                }
                Err(err) => {
                    rejected.push(format!(
                        "entry at {} tokens failed verification: {}",
                        entry.position_end,
                        err.message()
                    ));
                }
            }
            if position >= request.tokens.len() as u64 {
                break;
            }
        }
        let _ = best_object;
        self.bump_kv_index(|stats| {
            stats.lookups += 1;
            stats.probes += probes;
            stats.rejected_candidates += rejected.len() as u64;
            for note in rejected.iter().take(4) {
                stats.note(note.clone());
            }
        });
        match best {
            Some(record) => Ok(KvLookup::Reusable {
                record: Box::new(record),
                scanned: probes,
            }),
            None => {
                // A verified index found nothing. That is a real "no compatible
                // prefix" answer only when no entry was rejected; a rejection
                // means the index disagreed with the store, so the oracle scan
                // decides. Either way the answer is correct.
                if rejected.is_empty() && index.skipped_partial == 0 {
                    Ok(KvLookup::Recompute {
                        reasons: vec!["no compatible kv prefix is indexed".to_owned()],
                        scanned: probes,
                    })
                } else {
                    self.bump_kv_index(|stats| stats.fallbacks += 1);
                    self.kv_prefix_lookup_scan(request)
                }
            }
        }
    }

    /// Batch form of [`Store::kv_prefix_lookup_indexed`]: one index load, one
    /// revocation read, one walk per request.
    pub fn kv_prefix_lookup_batch_indexed(
        &self,
        requests: &[KvPrefixRequest],
    ) -> Result<Vec<KvLookup>> {
        let mut out = Vec::with_capacity(requests.len());
        for request in requests {
            out.push(self.kv_prefix_lookup_indexed(request)?);
        }
        Ok(out)
    }

    /// Verify one index entry: read the record it names, re-derive its id, and
    /// re-run the frozen compatibility rules.
    fn kv_index_verify_entry(
        &self,
        entry: &KvIndexEntry,
        request: &KvPrefixRequest,
        revoked: &std::collections::BTreeSet<Hash32>,
    ) -> Result<Option<KvPrefixRecord>> {
        if revoked.contains(&entry.record_prefix_id) {
            return Ok(None);
        }
        let bytes = self.read_object(entry.record_object_id)?;
        let record = KvPrefixRecord::from_canonical_bytes(&bytes)?;
        if record.id()? != entry.record_prefix_id {
            return Err(Error::corrupt(
                "kv index entry names a record with a different content id",
            ));
        }
        if record.position_end != entry.position_end
            || record.page_count != entry.page_count
            || record.token_chain_root != entry.chain_root
            || KvPrefixIdentity::of_record(&record).root() != entry.identity_root
        {
            return Err(Error::corrupt(
                "kv index entry disagrees with the record it names",
            ));
        }
        match record.compatibility(request) {
            Ok(()) => Ok(Some(record)),
            Err(reason) => Err(Error::corrupt(format!(
                "kv index candidate is not compatible: {reason}"
            ))),
        }
    }
}

/// `root' = chain(root, tokens[..])` step by step (the frozen chain).
fn kv_prefix_chain_root_from(root: Hash32, tokens: &[u64], start: u64, count: u64) -> Hash32 {
    let mut value = root;
    for offset in 0..count {
        value = kv_prefix_chain_step(value, tokens[(start + offset) as usize]);
    }
    value
}

fn kv_prefix_chain_step(root: Hash32, token: u64) -> Hash32 {
    let mut data = Vec::with_capacity(40);
    data.extend_from_slice(root.as_bytes());
    data.extend_from_slice(&token.to_le_bytes());
    identity::h(crate::kv_prefix::DOMAIN_KV_TOKEN_CHAIN, &data)
}

/// Merge a base segment and its deltas into one sorted live entry list.
///
/// A later segment's entry for the same boundary key replaces an earlier one;
/// a `Tombstone` removes it (no shipped writer emits one: a removal is encoded
/// as the exact live entry, which the XOR accumulator folds out). The input
/// segments must be sorted by generation.
fn merge_segments(segments: &[KvIndexSegment]) -> Result<Vec<KvIndexEntry>> {
    if segments.len() == 1 {
        return Ok(segments[0]
            .entries
            .iter()
            .filter(|entry| entry.kind == KvIndexEntryKind::Live)
            .cloned()
            .collect());
    }
    Ok(merge_live(&segments[0].entries, &segments[1..]))
}

fn merge_live(base: &[KvIndexEntry], deltas: &[KvIndexSegment]) -> Vec<KvIndexEntry> {
    // Later segments win. Sorting is stable, so keep the *last* entry for a
    // key by walking the sorted list backwards.
    let mut updates: Vec<KvIndexEntry> = deltas
        .iter()
        .flat_map(|delta| delta.entries.iter().cloned())
        .collect();
    updates.sort_by_key(|entry| entry.key());
    updates.reverse();
    updates.dedup_by(|a, b| a.key() == b.key());
    updates.reverse();
    let mut out = Vec::with_capacity(base.len() + updates.len());
    let (mut b, mut d) = (0usize, 0usize);
    while b < base.len() || d < updates.len() {
        let take_base = match (base.get(b), updates.get(d)) {
            (Some(base_entry), Some(update)) => base_entry.key() < update.key(),
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_base {
            let entry = &base[b];
            if entry.kind == KvIndexEntryKind::Live {
                out.push(entry.clone());
            }
            b += 1;
        } else {
            let entry = &updates[d];
            if entry.kind == KvIndexEntryKind::Live {
                out.push(entry.clone());
            }
            d += 1;
        }
    }
    out
}

/// Encoded width of one index entry (tests and receipts).
pub fn index_entry_bytes() -> usize {
    KV_INDEX_ENTRY_BYTES
}

/// Test-only crash hook: `WEFT_KV_INDEX_FAULT=<point>` aborts the process at
/// the named point. It exists so the crash court can kill a real build between
/// its two durable publications; it is never read by a normal run.
fn kv_index_fault(point: &str) {
    if std::env::var("WEFT_KV_INDEX_FAULT").ok().as_deref() == Some(point) {
        std::process::abort();
    }
}

impl Store {
    /// Test-only: publish an arbitrary root and its segments.
    ///
    /// Used by the adversarial court to install a forged or torn index and
    /// prove that the reader refuses it. Not part of the public API contract.
    #[doc(hidden)]
    pub fn kv_index_install_for_test(
        &mut self,
        root: &KvIndexRoot,
        segments: &[Vec<u8>],
    ) -> Result<()> {
        self.require_writable()?;
        for bytes in segments {
            if bytes.len() < KV_INDEX_TRAILER_BYTES {
                return Err(Error::invalid_argument("segment too short"));
            }
            let digest = Hash32::from_bytes(
                bytes[bytes.len() - 32..]
                    .try_into()
                    .map_err(|_| Error::invalid_argument("segment trailer"))?,
            );
            self.with_backend_mut(|backend| backend.put_atomic(&Key::kv_index(digest), bytes))?;
        }
        let bytes = root.canonical_bytes()?;
        self.with_backend_mut(|backend| backend.put_replace(&Key::kv_index_root(), &bytes))?;
        self.kv_index_clear_cache();
        Ok(())
    }

    /// Snapshot of the index counters and bounded notes.
    pub fn kv_index_stats_snapshot(&self) -> KvIndexStats {
        self.kv_index_stats()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Test-only: the stored root record, if any.
    #[doc(hidden)]
    pub fn kv_index_root_for_test(&self) -> Result<Option<KvIndexRoot>> {
        self.kv_index_read_root()
    }
}

