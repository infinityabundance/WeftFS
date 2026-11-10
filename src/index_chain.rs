//! WFX3 incremental advisory index segments.
//!
//! A WFX2 segment is a full-state snapshot. On a trajectory that commits once
//! per tensor that is a large, repeated write of state that did not change.
//! WFX3 adds a **delta** segment: an immediate base segment id plus the index
//! rows that changed. A reader resolves the full ancestor and applies the
//! deltas in order.
//!
//! The format is internal and rebuildable. Correctness still comes from
//! `evaluate::eval_expr`; the index only narrows candidates and may
//! over-approximate. Any chain inconsistency is a typed error and the store
//! rebuilds from replayed state.
//!
//! See `docs/INDEX-AMPLIFICATION.md` for the design, the commit attestation,
//! and the read/GC tradeoff.

use std::collections::{BTreeSet, HashMap};

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::index::{
    push_bytes, push_ids, push_str, push_u32, push_u64, read_target, target_bytes, IndexReader,
    IndexedRelation, StateIndex,
};
use crate::state::{RelationKind, RelationTarget};
use crate::wcb_util as wu;

/// WFX3 magic.
pub(crate) const MAGIC_V3: &[u8; 4] = b"WFX3";
/// WFX3 layout version.
pub(crate) const VERSION_V3: u32 = 1;
/// Domain for the segment's own integrity digest over every preceding byte.
pub(crate) const SEGMENT_DOMAIN: &str = "weft/index/segment/v3";
/// Domain for the attested segment chain.
pub(crate) const CHAIN_DOMAIN: &str = "weft/index/chain/v1";
/// Writer default: force a full segment after this many deltas.
pub(crate) const DEFAULT_MAX_CHAIN_DELTAS: u32 = 32;
/// Hard reader bound: a chain longer than this is refused and rebuilt.
pub(crate) const HARD_MAX_CHAIN_DELTAS: u32 = 256;
/// Hard cap on one WFX3 segment (same cap as WFX2).
pub(crate) const MAX_SEGMENT_BYTES: u64 = crate::index::MAX_INDEX_FILE_BYTES;
/// Trailing digest length.
const DIGEST_BYTES: usize = 32;
/// Fixed header length:
/// magic(4) + version(4) + kind(1) + generation(8) + base_generation(8) +
/// base_id(32) + base_commit(32) + object/relation/entry counts(24).
const HEADER_BYTES: usize = 4 + 4 + 1 + 8 + 8 + 32 + 32 + 8 + 8 + 8;
const PRESENT: u8 = 1;
const ABSENT: u8 = 0;

/// Segment kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SegmentKind {
    Full,
    Delta,
}

impl SegmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SegmentKind::Full => "full",
            SegmentKind::Delta => "delta",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "full" => Ok(SegmentKind::Full),
            "delta" => Ok(SegmentKind::Delta),
            other => Err(Error::malformed(format!(
                "index segment kind `{other}` is neither `full` nor `delta`"
            ))),
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            SegmentKind::Full => 0,
            SegmentKind::Delta => 1,
        }
    }

    fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(SegmentKind::Full),
            1 => Ok(SegmentKind::Delta),
            other => Err(Error::malformed(format!(
                "index segment kind {other} is reserved"
            ))),
        }
    }
}

/// The attested chain: head segment first, full ancestor last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexChain {
    pub kind: SegmentKind,
    pub chain: Vec<Hash32>,
}

impl IndexChain {
    /// `chain.len() - 1`; a full segment has depth 0.
    pub fn depth(&self) -> u64 {
        self.chain.len().saturating_sub(1) as u64
    }

    /// `H("weft/index/chain/v1", WCB({generation, chain}))`.
    pub fn root(&self, generation: u64) -> Result<Hash32> {
        let chain = self
            .chain
            .iter()
            .map(|id| wu::hash_value(*id))
            .collect::<Vec<_>>();
        let value = wu::map([
            ("generation", weftcodec::wcb::Value::U64(generation)),
            ("chain", weftcodec::wcb::Value::Array(chain)),
        ]);
        Ok(identity::h(CHAIN_DOMAIN, &wu::encode(&value)?))
    }

    /// The base segment id named by a delta head (`None` for full).
    pub fn base(&self) -> Option<Hash32> {
        match self.kind {
            SegmentKind::Full => None,
            SegmentKind::Delta => self.chain.get(1).copied(),
        }
    }
}

/// Parsed WFX3 header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SegmentHeader {
    pub kind: SegmentKind,
    pub generation: u64,
    pub base_generation: u64,
    pub base_id: Hash32,
    pub base_commit: Hash32,
    pub object_count: u64,
    pub relation_count: u64,
    pub entry_count: u64,
}

/// The segment id: `H("weft/index/segment/v3", body-without-digest)`.
pub(crate) fn segment_id(body: &[u8]) -> Hash32 {
    identity::h(SEGMENT_DOMAIN, body)
}

/// The id of an encoded segment file (strips the trailing digest).
pub(crate) fn file_segment_id(bytes: &[u8]) -> Result<Hash32> {
    if bytes.len() < DIGEST_BYTES {
        return Err(Error::corrupt("WFX3 segment is truncated"));
    }
    Ok(segment_id(&bytes[..bytes.len() - DIGEST_BYTES]))
}

/// A parsed segment file.
#[derive(Clone, Debug)]
pub(crate) struct Segment {
    pub header: SegmentHeader,
    pub id: Hash32,
    body: Vec<u8>,
}

/// Parse and integrity-check a WFX3 file without resolving its rows.
pub(crate) fn decode_segment(bytes: &[u8]) -> Result<Segment> {
    if bytes.len() < HEADER_BYTES + DIGEST_BYTES {
        return Err(Error::corrupt("WFX3 segment is truncated"));
    }
    if bytes.len() as u64 > MAX_SEGMENT_BYTES {
        return Err(Error::limit_exceeded(format!(
            "WFX3 segment is {} bytes, above the {MAX_SEGMENT_BYTES}-byte cap",
            bytes.len()
        )));
    }
    let (body, trailer) = bytes.split_at(bytes.len() - DIGEST_BYTES);
    let digest = identity::h(SEGMENT_DOMAIN, body);
    if trailer != digest.as_bytes() {
        return Err(Error::corrupt("WFX3 segment integrity digest mismatch"));
    }
    let header = decode_header(body)?;
    Ok(Segment {
        header,
        id: digest,
        body: body.to_vec(),
    })
}

fn decode_header(body: &[u8]) -> Result<SegmentHeader> {
    if body.len() < HEADER_BYTES {
        return Err(Error::corrupt("WFX3 header is truncated"));
    }
    if &body[0..4] != MAGIC_V3 {
        return Err(Error::malformed("bad WFX3 magic"));
    }
    let version = u32::from_le_bytes(body[4..8].try_into().expect("4 bytes"));
    if version != VERSION_V3 {
        return Err(Error::unsupported_version(format!(
            "WFX3 version {version} is not {VERSION_V3}"
        )));
    }
    let kind = SegmentKind::from_u8(body[8])?;
    let generation = u64::from_le_bytes(body[9..17].try_into().expect("8 bytes"));
    let base_generation = u64::from_le_bytes(body[17..25].try_into().expect("8 bytes"));
    let base_id = hash_at(body, 25);
    let base_commit = hash_at(body, 57);
    let object_count = u64::from_le_bytes(body[89..97].try_into().expect("8 bytes"));
    let relation_count = u64::from_le_bytes(body[97..105].try_into().expect("8 bytes"));
    let entry_count = u64::from_le_bytes(body[105..113].try_into().expect("8 bytes"));
    if generation == 0 {
        return Err(Error::corrupt("WFX3 generation must be positive"));
    }
    match kind {
        SegmentKind::Full => {
            if base_generation != 0 || base_id != Hash32::ZERO || base_commit != Hash32::ZERO {
                return Err(Error::corrupt("a full WFX3 segment must not name a base"));
            }
        }
        SegmentKind::Delta => {
            if base_generation == 0 || base_generation + 1 != generation {
                return Err(Error::corrupt(
                    "a WFX3 delta must name the immediately previous generation",
                ));
            }
            if base_id == Hash32::ZERO || base_commit == Hash32::ZERO {
                return Err(Error::corrupt(
                    "a WFX3 delta must name its base segment and commit",
                ));
            }
        }
    }
    Ok(SegmentHeader {
        kind,
        generation,
        base_generation,
        base_id,
        base_commit,
        object_count,
        relation_count,
        entry_count,
    })
}

fn hash_at(body: &[u8], offset: usize) -> Hash32 {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&body[offset..offset + 32]);
    Hash32::from_bytes(bytes)
}

/// Encode a full segment for `index`.
pub(crate) fn encode_full(index: &StateIndex) -> Result<Vec<u8>> {
    let generation = index.generation();
    let header = SegmentHeader {
        kind: SegmentKind::Full,
        generation,
        base_generation: 0,
        base_id: Hash32::ZERO,
        base_commit: Hash32::ZERO,
        object_count: index.object_count() as u64,
        relation_count: index.relation_count() as u64,
        entry_count: index.entry_count() as u64,
    };
    encode_segment(header, index, None)
}

/// Encode a delta from `base` to `index`.
pub(crate) fn encode_delta(
    base: &StateIndex,
    index: &StateIndex,
    base_id: Hash32,
    base_commit: Hash32,
) -> Result<Vec<u8>> {
    if base.generation().checked_add(1) != Some(index.generation()) {
        return Err(Error::invalid_argument(
            "a WFX3 delta base must be the immediately previous generation",
        ));
    }
    let header = SegmentHeader {
        kind: SegmentKind::Delta,
        generation: index.generation(),
        base_generation: base.generation(),
        base_id,
        base_commit,
        object_count: index.object_count() as u64,
        relation_count: index.relation_count() as u64,
        entry_count: index.entry_count() as u64,
    };
    encode_segment(header, index, Some(base))
}

fn encode_segment(
    header: SegmentHeader,
    index: &StateIndex,
    base: Option<&StateIndex>,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(HEADER_BYTES + 1024);
    out.extend_from_slice(MAGIC_V3);
    out.extend_from_slice(&VERSION_V3.to_le_bytes());
    out.push(header.kind.as_u8());
    push_u64(&mut out, header.generation);
    push_u64(&mut out, header.base_generation);
    out.extend_from_slice(header.base_id.as_bytes());
    out.extend_from_slice(header.base_commit.as_bytes());
    push_u64(&mut out, header.object_count);
    push_u64(&mut out, header.relation_count);
    push_u64(&mut out, header.entry_count);

    // objects_with_attr
    let mut names: BTreeSet<&String> = index.objects_with_attr.keys().collect();
    if let Some(base) = base {
        names.extend(base.objects_with_attr.keys());
    }
    let mut rows: Vec<(&String, Option<&BTreeSet<Hash32>>)> = Vec::new();
    for name in names {
        let new = index.objects_with_attr.get(name);
        let old = base.and_then(|b| b.objects_with_attr.get(name));
        if base.is_some() && new == old {
            continue;
        }
        rows.push((name, new));
    }
    push_u32(&mut out, rows.len() as u64)?;
    for (name, ids) in rows {
        push_str(&mut out, name)?;
        match ids {
            Some(ids) => {
                out.push(PRESENT);
                push_ids(&mut out, ids)?;
            }
            None => out.push(ABSENT),
        }
    }

    // objects_by_attr_eq
    let mut keys: BTreeSet<(String, Vec<u8>)> = index.objects_by_attr_eq.keys().cloned().collect();
    if let Some(base) = base {
        keys.extend(base.objects_by_attr_eq.keys().cloned());
    }
    let mut attr_rows: Vec<((String, Vec<u8>), Option<&BTreeSet<Hash32>>)> = Vec::new();
    for key in keys {
        let new = index.objects_by_attr_eq.get(&key);
        let old = base.and_then(|b| b.objects_by_attr_eq.get(&key));
        if base.is_some() && new == old {
            continue;
        }
        attr_rows.push((key, new));
    }
    push_u32(&mut out, attr_rows.len() as u64)?;
    for ((name, key), ids) in attr_rows {
        push_str(&mut out, &name)?;
        push_bytes(&mut out, &key)?;
        match ids {
            Some(ids) => {
                out.push(PRESENT);
                push_ids(&mut out, ids)?;
            }
            None => out.push(ABSENT),
        }
    }

    // relations_from
    let mut relation_keys: Vec<(Hash32, String)> = index.relations_from.keys().cloned().collect();
    if let Some(base) = base {
        relation_keys.extend(base.relations_from.keys().cloned());
    }
    relation_keys.sort();
    relation_keys.dedup();
    let mut relation_rows: Vec<((Hash32, String), Option<Vec<Vec<u8>>>)> = Vec::new();
    for key in relation_keys {
        let new = index.relations_from.get(&key);
        let old = base.and_then(|b| b.relations_from.get(&key));
        let new_canon = canonical_edges(new)?;
        let old_canon = canonical_edges(old)?;
        if base.is_some() && new_canon == old_canon {
            continue;
        }
        relation_rows.push((key, if new.is_some() { Some(new_canon) } else { None }));
    }
    push_u32(&mut out, relation_rows.len() as u64)?;
    for ((subject, predicate), edges) in relation_rows {
        out.extend_from_slice(subject.as_bytes());
        push_str(&mut out, &predicate)?;
        match edges {
            Some(edges) => {
                out.push(PRESENT);
                push_u32(&mut out, edges.len() as u64)?;
                for edge in edges {
                    out.extend_from_slice(&edge);
                }
            }
            None => out.push(ABSENT),
        }
    }

    // tags
    let mut tags: BTreeSet<&String> = index.tags.keys().collect();
    if let Some(base) = base {
        tags.extend(base.tags.keys());
    }
    let mut tag_rows: Vec<(&String, Option<&BTreeSet<Hash32>>)> = Vec::new();
    for tag in tags {
        let new = index.tags.get(tag);
        let old = base.and_then(|b| b.tags.get(tag));
        if base.is_some() && new == old {
            continue;
        }
        tag_rows.push((tag, new));
    }
    push_u32(&mut out, tag_rows.len() as u64)?;
    for (tag, ids) in tag_rows {
        push_str(&mut out, tag)?;
        match ids {
            Some(ids) => {
                out.push(PRESENT);
                push_ids(&mut out, ids)?;
            }
            None => out.push(ABSENT),
        }
    }

    if out.len() as u64 + DIGEST_BYTES as u64 > MAX_SEGMENT_BYTES {
        return Err(Error::limit_exceeded(format!(
            "WFX3 segment is {} bytes, above the {MAX_SEGMENT_BYTES}-byte cap",
            out.len() + DIGEST_BYTES
        )));
    }
    let digest = identity::h(SEGMENT_DOMAIN, &out);
    out.extend_from_slice(digest.as_bytes());
    Ok(out)
}

/// Canonical edge bytes, sorted by target, for equality and delta encoding.
fn canonical_edges(edges: Option<&Vec<IndexedRelation>>) -> Result<Vec<Vec<u8>>> {
    let Some(edges) = edges else {
        return Ok(Vec::new());
    };
    let mut encoded = edges.iter().map(target_bytes).collect::<Result<Vec<_>>>()?;
    encoded.sort();
    Ok(encoded)
}

/// Resolve a segment file against an optional base index.
///
/// A full segment must not have a base; a delta requires one and its generation
/// must be exactly one greater. The resolved counts must equal the header.
pub(crate) fn resolve_segment(bytes: &[u8], base: Option<&StateIndex>) -> Result<StateIndex> {
    let segment = decode_segment(bytes)?;
    resolve_parsed_segment(&segment, base)
}

fn resolve_parsed_segment(segment: &Segment, base: Option<&StateIndex>) -> Result<StateIndex> {
    let header = segment.header;
    let mut reader = IndexReader::new(&segment.body);
    let _ = reader.take(HEADER_BYTES)?;

    let mut objects_with_attr: HashMap<String, BTreeSet<Hash32>> = match (header.kind, base) {
        (SegmentKind::Full, None) => HashMap::new(),
        (SegmentKind::Delta, Some(base)) => base.objects_with_attr.clone(),
        (SegmentKind::Full, Some(_)) => {
            return Err(Error::corrupt("a full WFX3 segment cannot have a base"))
        }
        (SegmentKind::Delta, None) => {
            return Err(Error::corrupt("a WFX3 delta segment has no base"))
        }
    };
    let mut objects_by_attr_eq: HashMap<(String, Vec<u8>), BTreeSet<Hash32>> =
        match (header.kind, base) {
            (SegmentKind::Full, None) => HashMap::new(),
            (SegmentKind::Delta, Some(base)) => base.objects_by_attr_eq.clone(),
            _ => unreachable!(),
        };
    let mut relations_from: HashMap<(Hash32, String), Vec<IndexedRelation>> =
        match (header.kind, base) {
            (SegmentKind::Full, None) => HashMap::new(),
            (SegmentKind::Delta, Some(base)) => base.relations_from.clone(),
            _ => unreachable!(),
        };
    let mut tags: HashMap<String, BTreeSet<Hash32>> = match (header.kind, base) {
        (SegmentKind::Full, None) => HashMap::new(),
        (SegmentKind::Delta, Some(base)) => base.tags.clone(),
        _ => unreachable!(),
    };

    let mut last_name: Option<String> = None;
    let count = reader.bounded_count(4)?;
    for _ in 0..count {
        let name = reader.string()?;
        if let Some(previous) = &last_name {
            if &name <= previous {
                return Err(Error::corrupt("WFX3 objects_with_attr rows are not sorted"));
            }
        }
        last_name = Some(name.clone());
        let present = reader.u8()?;
        match present {
            PRESENT => {
                objects_with_attr.insert(name, reader.id_set()?);
            }
            ABSENT => {
                if header.kind == SegmentKind::Full || objects_with_attr.remove(&name).is_none() {
                    return Err(Error::corrupt(
                        "WFX3 objects_with_attr tombstone has no base row",
                    ));
                }
            }
            _ => return Err(Error::corrupt("WFX3 present flag is not 0/1")),
        }
    }

    let mut last_attr: Option<(String, Vec<u8>)> = None;
    let count = reader.bounded_count(4)?;
    for _ in 0..count {
        let name = reader.string()?;
        let key = reader.bytes()?.to_vec();
        if let Some((previous_name, previous_key)) = &last_attr {
            if (&name, &key) <= (previous_name, previous_key) {
                return Err(Error::corrupt("WFX3 attr_eq rows are not sorted"));
            }
        }
        last_attr = Some((name.clone(), key.clone()));
        let present = reader.u8()?;
        match present {
            PRESENT => {
                objects_by_attr_eq.insert((name, key), reader.id_set()?);
            }
            ABSENT => {
                if header.kind == SegmentKind::Full
                    || objects_by_attr_eq.remove(&(name, key)).is_none()
                {
                    return Err(Error::corrupt("WFX3 attr_eq tombstone has no base row"));
                }
            }
            _ => return Err(Error::corrupt("WFX3 present flag is not 0/1")),
        }
    }

    let mut last_relation: Option<(Hash32, String)> = None;
    let count = reader.bounded_count(4)?;
    for _ in 0..count {
        let subject = reader.hash32()?;
        let predicate = reader.string()?;
        let key = (subject, predicate.clone());
        if let Some(previous) = &last_relation {
            if &key <= previous {
                return Err(Error::corrupt("WFX3 relation rows are not sorted"));
            }
        }
        last_relation = Some(key.clone());
        let present = reader.u8()?;
        match present {
            PRESENT => {
                let edge_count = reader.bounded_count(32)?;
                let mut edges = Vec::with_capacity(edge_count);
                let mut last_target: Option<Vec<u8>> = None;
                for _ in 0..edge_count {
                    let (kind, target) = read_target(&mut reader)?;
                    let edge = IndexedRelation { kind, target };
                    let encoded = target_bytes(&edge)?;
                    if let Some(previous) = &last_target {
                        if encoded <= *previous {
                            return Err(Error::corrupt("WFX3 relation targets are not sorted"));
                        }
                    }
                    last_target = Some(encoded);
                    edges.push(edge);
                }
                relations_from.insert(key, edges);
            }
            ABSENT => {
                if header.kind == SegmentKind::Full || relations_from.remove(&key).is_none() {
                    return Err(Error::corrupt("WFX3 relation tombstone has no base row"));
                }
            }
            _ => return Err(Error::corrupt("WFX3 present flag is not 0/1")),
        }
    }

    let mut last_tag: Option<String> = None;
    let count = reader.bounded_count(4)?;
    for _ in 0..count {
        let tag = reader.string()?;
        if let Some(previous) = &last_tag {
            if &tag <= previous {
                return Err(Error::corrupt("WFX3 tag rows are not sorted"));
            }
        }
        last_tag = Some(tag.clone());
        let present = reader.u8()?;
        match present {
            PRESENT => {
                tags.insert(tag, reader.id_set()?);
            }
            ABSENT => {
                if header.kind == SegmentKind::Full || tags.remove(&tag).is_none() {
                    return Err(Error::corrupt("WFX3 tag tombstone has no base row"));
                }
            }
            _ => return Err(Error::corrupt("WFX3 present flag is not 0/1")),
        }
    }
    if reader.remaining() != 0 {
        return Err(Error::corrupt("trailing bytes after WFX3 sections"));
    }

    let resolved = StateIndex::from_parts(
        Hash32::ZERO,
        header.generation,
        header.object_count as usize,
        header.relation_count as usize,
        header.entry_count as usize,
        objects_by_attr_eq,
        objects_with_attr,
        relations_from,
        tags,
    );
    if resolved.object_count() as u64 != header.object_count
        || resolved.relation_count() as u64 != header.relation_count
        || resolved.entry_count() as u64 != header.entry_count
    {
        return Err(Error::corrupt(
            "resolved WFX3 index counts disagree with its header",
        ));
    }
    Ok(resolved)
}

/// The resolved view of a chain: the full index plus the attestation fields.
#[derive(Debug)]
pub(crate) struct ResolvedChain {
    pub index: StateIndex,
    pub chain: IndexChain,
}

/// Resolve a head segment and its ancestors. `load` returns the bytes stored
/// for a segment, keyed by the base commit named in the delta header.
pub(crate) fn resolve_chain<F>(head: &[u8], load: F) -> Result<ResolvedChain>
where
    F: FnMut(Hash32, Hash32) -> Result<Vec<u8>>,
{
    let head_segment = decode_segment(head)?;
    resolve_chain_segment(head_segment, load)
}

/// Resolve an already-decoded head segment and its ancestors.
pub(crate) fn resolve_chain_segment<F>(head_segment: Segment, mut load: F) -> Result<ResolvedChain>
where
    F: FnMut(Hash32, Hash32) -> Result<Vec<u8>>,
{
    if head_segment.header.kind == SegmentKind::Full {
        let index = resolve_parsed_segment(&head_segment, None)?;
        return Ok(ResolvedChain {
            index,
            chain: IndexChain {
                kind: SegmentKind::Full,
                chain: vec![head_segment.id],
            },
        });
    }
    let mut segments: Vec<Segment> = vec![head_segment];
    loop {
        let last = segments.last().expect("the head is present");
        if last.header.kind == SegmentKind::Full {
            break;
        }
        if segments.len() > HARD_MAX_CHAIN_DELTAS as usize + 1 {
            return Err(Error::limit_exceeded(format!(
                "WFX3 index chain is deeper than {HARD_MAX_CHAIN_DELTAS} deltas"
            )));
        }
        let header = last.header;
        let base_bytes = load(header.base_commit, header.base_id).map_err(|err| {
            Error::corrupt(format!(
                "cannot read base segment {} for commit {}: {err}",
                header.base_id, header.base_commit
            ))
        })?;
        let base_segment = decode_segment(&base_bytes).map_err(|err| {
            Error::corrupt(format!(
                "base segment {} for commit {}: {err}",
                header.base_id, header.base_commit
            ))
        })?;
        if base_segment.id != header.base_id {
            return Err(Error::corrupt(
                "WFX3 base segment id does not match the bytes it names",
            ));
        }
        if base_segment.header.generation != header.base_generation {
            return Err(Error::corrupt(
                "WFX3 base segment generation does not match the delta header",
            ));
        }
        segments.push(base_segment);
    }
    let ids: Vec<Hash32> = segments.iter().map(|segment| segment.id).collect();
    // `segments` is head..full; resolve from full..head.
    let mut resolved: Option<StateIndex> = None;
    for segment in segments.iter().rev() {
        let next = resolve_parsed_segment(segment, resolved.as_ref())?;
        if next.generation() != segment.header.generation {
            return Err(Error::corrupt("WFX3 resolved generation mismatch"));
        }
        resolved = Some(next);
    }
    let index = resolved.ok_or_else(|| Error::corrupt("WFX3 chain resolved to nothing"))?;
    let chain = IndexChain {
        kind: SegmentKind::Delta,
        chain: ids,
    };
    Ok(ResolvedChain { index, chain })
}

/// Test hook: decode a full WFX3 segment, remove one tag row, and re-encode it
/// with a valid digest but a different `candidates_root`. Used to prove the
/// commit attestation, not the digest, refutes a self-consistent subset.
pub(crate) fn forge_remove_tag_for_test(bytes: &[u8], tag: &str) -> Result<Vec<u8>> {
    let segment = decode_segment(bytes)?;
    if segment.header.kind != SegmentKind::Full {
        return Err(Error::invalid_argument(
            "the forge hook needs a full segment; set index_chain_max_deltas = 0",
        ));
    }
    let mut index = resolve_parsed_segment(&segment, None)?;
    index.tags.remove(tag);
    encode_full(&index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_segment_round_trips() {
        let mut state = crate::state::State::default();
        state.generation = 1;
        let index = StateIndex::build(&state);
        let bytes = encode_full(&index).expect("encode");
        let segment = decode_segment(&bytes).expect("decode header");
        assert_eq!(segment.header.kind, SegmentKind::Full);
        let resolved = resolve_segment(&bytes, None).expect("resolve");
        assert_eq!(resolved.object_count(), index.object_count());
        assert_eq!(resolved.relation_count(), index.relation_count());
        assert_eq!(resolved.entry_count(), index.entry_count());
        assert_eq!(
            resolved.candidates_root().expect("candidates"),
            index.candidates_root().expect("candidates")
        );
    }
}
