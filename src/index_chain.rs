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
/// Element-level patch against the base row: added ids/edges then removed
/// ids/edges. Used whenever the base already has the row, so an append to a
/// large set costs the appended element rather than the whole set.
const PATCH: u8 = 2;

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

/// Encode a delta that carries only the rows the incremental maintainer
/// recorded as changed.
///
/// This is the operation-derived counterpart of [`encode_delta`]: the writer
/// already knows which elements moved, so it never scans the whole index to
/// find the difference. The bytes are ordinary WFX3 (same tags, same patch
/// rules); only the way the row set was computed differs.
pub(crate) fn encode_delta_changed(
    delta: &crate::index_incremental::IndexDelta,
    index: &StateIndex,
    base_id: Hash32,
    base_commit: Hash32,
) -> Result<Vec<u8>> {
    let base_generation = index
        .generation()
        .checked_sub(1)
        .ok_or_else(|| Error::invalid_argument("a WFX3 delta needs a previous generation"))?;
    let header = SegmentHeader {
        kind: SegmentKind::Delta,
        generation: index.generation(),
        base_generation,
        base_id,
        base_commit,
        object_count: index.object_count() as u64,
        relation_count: index.relation_count() as u64,
        entry_count: index.entry_count() as u64,
    };
    let mut out = Vec::with_capacity(HEADER_BYTES + 256);
    write_header(&mut out, header);

    // objects_with_attr
    let mut name_rows: Vec<(&String, SetRowWrite)> = Vec::new();
    for (name, change) in &delta.with_attr {
        if let Some(row) = changed_set_row(change, index.objects_with_attr.get(name)) {
            name_rows.push((name, row));
        }
    }
    push_u32(&mut out, name_rows.len() as u64)?;
    for (name, row) in name_rows {
        push_str(&mut out, name)?;
        write_changed_set_row(&mut out, row)?;
    }

    // objects_by_attr_eq
    let mut attr_rows: Vec<((String, Vec<u8>), SetRowWrite)> = Vec::new();
    for (key, change) in &delta.attr_eq {
        if let Some(row) = changed_set_row(change, index.objects_by_attr_eq.get(key)) {
            attr_rows.push((key.clone(), row));
        }
    }
    push_u32(&mut out, attr_rows.len() as u64)?;
    for ((name, key), row) in attr_rows {
        push_str(&mut out, &name)?;
        push_bytes(&mut out, &key)?;
        write_changed_set_row(&mut out, row)?;
    }

    // relations_from
    let mut relation_rows: Vec<((Hash32, String), EdgeRowWrite)> = Vec::new();
    for (key, change) in &delta.relations {
        if let Some(row) = changed_edge_row(change, index.relations_from.get(key))? {
            relation_rows.push((key.clone(), row));
        }
    }
    push_u32(&mut out, relation_rows.len() as u64)?;
    for ((subject, predicate), row) in relation_rows {
        out.extend_from_slice(subject.as_bytes());
        push_str(&mut out, &predicate)?;
        write_changed_edge_row(&mut out, row)?;
    }

    // tags
    let mut tag_rows: Vec<(&String, SetRowWrite)> = Vec::new();
    for (tag, change) in &delta.tags {
        if let Some(row) = changed_set_row(change, index.tags.get(tag)) {
            tag_rows.push((tag, row));
        }
    }
    push_u32(&mut out, tag_rows.len() as u64)?;
    for (tag, row) in tag_rows {
        push_str(&mut out, tag)?;
        write_changed_set_row(&mut out, row)?;
    }

    finish_segment(out)
}

/// What a changed set-valued row becomes in the segment.
#[derive(Debug)]
enum SetRowWrite {
    Present(BTreeSet<Hash32>),
    Absent,
    Patch {
        added: Vec<Hash32>,
        removed: Vec<Hash32>,
    },
}

/// What a changed relation row becomes in the segment.
#[derive(Debug)]
enum EdgeRowWrite {
    Present(Vec<Vec<u8>>),
    Absent,
    Patch {
        added: Vec<Vec<u8>>,
        removed: Vec<Vec<u8>>,
    },
}

fn changed_set_row(
    change: &crate::index_incremental::RowChange,
    new: Option<&BTreeSet<Hash32>>,
) -> Option<SetRowWrite> {
    match (change.base_present, new) {
        (false, None) => None,
        (false, Some(ids)) => Some(SetRowWrite::Present(ids.clone())),
        (true, None) => Some(SetRowWrite::Absent),
        (true, Some(ids)) => {
            let mut added = Vec::new();
            let mut removed = Vec::new();
            for (key, was_present) in &change.elements {
                let bytes: [u8; 32] = key.as_slice().try_into().ok()?;
                let id = Hash32::from_bytes(bytes);
                let present = ids.contains(&id);
                match (*was_present, present) {
                    (false, true) => added.push(id),
                    (true, false) => removed.push(id),
                    _ => {}
                }
            }
            if added.is_empty() && removed.is_empty() {
                return None;
            }
            added.sort();
            removed.sort();
            Some(SetRowWrite::Patch { added, removed })
        }
    }
}

fn changed_edge_row(
    change: &crate::index_incremental::RowChange,
    new: Option<&Vec<IndexedRelation>>,
) -> Result<Option<EdgeRowWrite>> {
    match (change.base_present, new) {
        (false, None) => Ok(None),
        (false, Some(edges)) => Ok(Some(EdgeRowWrite::Present(canonical_edges(Some(edges))?))),
        (true, None) => Ok(Some(EdgeRowWrite::Absent)),
        (true, Some(edges)) => {
            let new_set: BTreeSet<Vec<u8>> = canonical_edges(Some(edges))?.into_iter().collect();
            let mut added = Vec::new();
            let mut removed = Vec::new();
            for (key, was_present) in &change.elements {
                let present = new_set.contains(key);
                match (*was_present, present) {
                    (false, true) => added.push(key.clone()),
                    (true, false) => removed.push(key.clone()),
                    _ => {}
                }
            }
            if added.is_empty() && removed.is_empty() {
                return Ok(None);
            }
            Ok(Some(EdgeRowWrite::Patch { added, removed }))
        }
    }
}

fn write_changed_set_row(out: &mut Vec<u8>, row: SetRowWrite) -> Result<()> {
    match row {
        SetRowWrite::Present(ids) => {
            out.push(PRESENT);
            push_ids(out, &ids)?;
        }
        SetRowWrite::Absent => out.push(ABSENT),
        SetRowWrite::Patch { added, removed } => {
            out.push(PATCH);
            push_u32(out, added.len() as u64)?;
            for id in &added {
                out.extend_from_slice(id.as_bytes());
            }
            push_u32(out, removed.len() as u64)?;
            for id in &removed {
                out.extend_from_slice(id.as_bytes());
            }
        }
    }
    Ok(())
}

fn write_changed_edge_row(out: &mut Vec<u8>, row: EdgeRowWrite) -> Result<()> {
    match row {
        EdgeRowWrite::Present(edges) => {
            out.push(PRESENT);
            push_u32(out, edges.len() as u64)?;
            for edge in &edges {
                out.extend_from_slice(edge);
            }
        }
        EdgeRowWrite::Absent => out.push(ABSENT),
        EdgeRowWrite::Patch { added, removed } => {
            out.push(PATCH);
            push_u32(out, added.len() as u64)?;
            for edge in &added {
                out.extend_from_slice(edge);
            }
            push_u32(out, removed.len() as u64)?;
            for edge in &removed {
                out.extend_from_slice(edge);
            }
        }
    }
    Ok(())
}

fn write_header(out: &mut Vec<u8>, header: SegmentHeader) {
    out.extend_from_slice(MAGIC_V3);
    out.extend_from_slice(&VERSION_V3.to_le_bytes());
    out.push(header.kind.as_u8());
    push_u64(out, header.generation);
    push_u64(out, header.base_generation);
    out.extend_from_slice(header.base_id.as_bytes());
    out.extend_from_slice(header.base_commit.as_bytes());
    push_u64(out, header.object_count);
    push_u64(out, header.relation_count);
    push_u64(out, header.entry_count);
}

fn finish_segment(mut out: Vec<u8>) -> Result<Vec<u8>> {
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

fn encode_segment(
    header: SegmentHeader,
    index: &StateIndex,
    base: Option<&StateIndex>,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(HEADER_BYTES + 1024);
    write_header(&mut out, header);

    // objects_with_attr
    let mut names: BTreeSet<&String> = index.objects_with_attr.keys().collect();
    if let Some(base) = base {
        names.extend(base.objects_with_attr.keys());
    }
    let mut rows: Vec<(
        &String,
        Option<&BTreeSet<Hash32>>,
        Option<&BTreeSet<Hash32>>,
    )> = Vec::new();
    for name in names {
        let new = index.objects_with_attr.get(name);
        let old = base.and_then(|b| b.objects_with_attr.get(name));
        if base.is_some() && new == old {
            continue;
        }
        rows.push((name, new, old));
    }
    push_u32(&mut out, rows.len() as u64)?;
    for (name, ids, old) in rows {
        push_str(&mut out, name)?;
        write_id_row(&mut out, ids, old)?;
    }

    // objects_by_attr_eq
    let mut keys: BTreeSet<(String, Vec<u8>)> = index.objects_by_attr_eq.keys().cloned().collect();
    if let Some(base) = base {
        keys.extend(base.objects_by_attr_eq.keys().cloned());
    }
    let mut attr_rows: Vec<(
        (String, Vec<u8>),
        Option<&BTreeSet<Hash32>>,
        Option<&BTreeSet<Hash32>>,
    )> = Vec::new();
    for key in keys {
        let new = index.objects_by_attr_eq.get(&key);
        let old = base.and_then(|b| b.objects_by_attr_eq.get(&key));
        if base.is_some() && new == old {
            continue;
        }
        attr_rows.push((key, new, old));
    }
    push_u32(&mut out, attr_rows.len() as u64)?;
    for ((name, key), ids, old) in attr_rows {
        push_str(&mut out, &name)?;
        push_bytes(&mut out, &key)?;
        write_id_row(&mut out, ids, old)?;
    }

    // relations_from
    let mut relation_keys: Vec<(Hash32, String)> = index.relations_from.keys().cloned().collect();
    if let Some(base) = base {
        relation_keys.extend(base.relations_from.keys().cloned());
    }
    relation_keys.sort();
    relation_keys.dedup();
    let mut relation_rows: Vec<((Hash32, String), Option<Vec<Vec<u8>>>, Option<Vec<Vec<u8>>>)> =
        Vec::new();
    for key in relation_keys {
        let new = index.relations_from.get(&key);
        let old = base.and_then(|b| b.relations_from.get(&key));
        let new_canon = canonical_edges(new)?;
        let old_canon = canonical_edges(old)?;
        if base.is_some() && new_canon == old_canon {
            continue;
        }
        relation_rows.push((
            key,
            if new.is_some() { Some(new_canon) } else { None },
            if old.is_some() { Some(old_canon) } else { None },
        ));
    }
    push_u32(&mut out, relation_rows.len() as u64)?;
    for ((subject, predicate), edges, old) in relation_rows {
        out.extend_from_slice(subject.as_bytes());
        push_str(&mut out, &predicate)?;
        write_edge_row(&mut out, edges, old)?;
    }

    // tags
    let mut tags: BTreeSet<&String> = index.tags.keys().collect();
    if let Some(base) = base {
        tags.extend(base.tags.keys());
    }
    let mut tag_rows: Vec<(
        &String,
        Option<&BTreeSet<Hash32>>,
        Option<&BTreeSet<Hash32>>,
    )> = Vec::new();
    for tag in tags {
        let new = index.tags.get(tag);
        let old = base.and_then(|b| b.tags.get(tag));
        if base.is_some() && new == old {
            continue;
        }
        tag_rows.push((tag, new, old));
    }
    push_u32(&mut out, tag_rows.len() as u64)?;
    for (tag, ids, old) in tag_rows {
        push_str(&mut out, tag)?;
        write_id_row(&mut out, ids, old)?;
    }

    finish_segment(out)
}

/// Write one set-valued row: present, absent, or an element-level patch.
fn write_id_row(
    out: &mut Vec<u8>,
    new: Option<&BTreeSet<Hash32>>,
    old: Option<&BTreeSet<Hash32>>,
) -> Result<()> {
    match (new, old) {
        (None, _) => out.push(ABSENT),
        (Some(ids), None) => {
            out.push(PRESENT);
            push_ids(out, ids)?;
        }
        (Some(ids), Some(old)) => {
            let added: BTreeSet<Hash32> = ids.difference(old).copied().collect();
            let removed: BTreeSet<Hash32> = old.difference(ids).copied().collect();
            out.push(PATCH);
            push_ids(out, &added)?;
            push_ids(out, &removed)?;
        }
    }
    Ok(())
}

/// Write one relation row: present, absent, or an element-level patch.
fn write_edge_row(
    out: &mut Vec<u8>,
    new: Option<Vec<Vec<u8>>>,
    old: Option<Vec<Vec<u8>>>,
) -> Result<()> {
    match (new, old) {
        (None, _) => out.push(ABSENT),
        (Some(edges), None) => {
            out.push(PRESENT);
            push_u32(out, edges.len() as u64)?;
            for edge in edges {
                out.extend_from_slice(&edge);
            }
        }
        (Some(edges), Some(old)) => {
            let new_set: BTreeSet<Vec<u8>> = edges.into_iter().collect();
            let old_set: BTreeSet<Vec<u8>> = old.into_iter().collect();
            let added: Vec<&Vec<u8>> = new_set.difference(&old_set).collect();
            let removed: Vec<&Vec<u8>> = old_set.difference(&new_set).collect();
            out.push(PATCH);
            push_u32(out, added.len() as u64)?;
            for edge in added {
                out.extend_from_slice(edge);
            }
            push_u32(out, removed.len() as u64)?;
            for edge in removed {
                out.extend_from_slice(edge);
            }
        }
    }
    Ok(())
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

/// Apply an element-level patch to a set-valued row. Validates that every
/// removal names a present id, every addition names an absent id, and the row
/// does not become empty; a malformed patch is a typed error, never tolerated.
fn apply_id_patch(ids: &mut BTreeSet<Hash32>, reader: &mut IndexReader<'_>) -> Result<()> {
    let added = reader.id_set()?;
    let removed = reader.id_set()?;
    for id in &removed {
        if !ids.contains(id) {
            return Err(Error::corrupt(
                "WFX3 patch removes an id that is not in the base row",
            ));
        }
    }
    for id in &added {
        if ids.contains(id) {
            return Err(Error::corrupt(
                "WFX3 patch adds an id already in the base row",
            ));
        }
    }
    for id in &removed {
        ids.remove(id);
    }
    for id in added {
        ids.insert(id);
    }
    if ids.is_empty() {
        return Err(Error::corrupt("WFX3 patch empties a set-valued row"));
    }
    Ok(())
}

/// Apply an element-level patch to a relation row (added/removed target edges).
fn apply_edge_patch(edges: &mut Vec<IndexedRelation>, reader: &mut IndexReader<'_>) -> Result<()> {
    let base: BTreeSet<Vec<u8>> = edges.iter().map(target_bytes).collect::<Result<_>>()?;
    let added_count = reader.bounded_count(1)?;
    let mut added = Vec::with_capacity(added_count);
    let mut added_keys: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut last: Option<Vec<u8>> = None;
    for _ in 0..added_count {
        let (kind, target) = read_target(reader)?;
        let edge = IndexedRelation { kind, target };
        let encoded = target_bytes(&edge)?;
        if let Some(previous) = &last {
            if encoded <= *previous {
                return Err(Error::corrupt("WFX3 patch additions are not sorted"));
            }
        }
        if base.contains(&encoded) {
            return Err(Error::corrupt(
                "WFX3 patch adds a target already in the base row",
            ));
        }
        last = Some(encoded.clone());
        added_keys.insert(encoded);
        added.push(edge);
    }
    let removed_count = reader.bounded_count(1)?;
    let mut removed_keys: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut last: Option<Vec<u8>> = None;
    for _ in 0..removed_count {
        let (kind, target) = read_target(reader)?;
        let encoded = target_bytes(&IndexedRelation { kind, target })?;
        if let Some(previous) = &last {
            if encoded <= *previous {
                return Err(Error::corrupt("WFX3 patch removals are not sorted"));
            }
        }
        if !base.contains(&encoded) {
            return Err(Error::corrupt(
                "WFX3 patch removes a target that is not in the base row",
            ));
        }
        if added_keys.contains(&encoded) {
            return Err(Error::corrupt(
                "WFX3 patch both adds and removes one target",
            ));
        }
        last = Some(encoded.clone());
        removed_keys.insert(encoded);
    }
    if base.len() - removed_keys.len() + added_keys.len() == 0 {
        return Err(Error::corrupt("WFX3 patch empties a relation row"));
    }
    let mut kept = Vec::with_capacity(base.len() - removed_keys.len());
    for edge in edges.drain(..) {
        if !removed_keys.contains(&target_bytes(&edge)?) {
            kept.push(edge);
        }
    }
    kept.extend(added);
    *edges = kept;
    Ok(())
}

/// Resolve a segment file against an optional base index.
///
/// A full segment must not have a base; a delta requires one and its generation
/// must be exactly one greater. The resolved counts must equal the header.
#[cfg(test)]
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
            PATCH => {
                let Some(ids) = objects_with_attr.get_mut(&name) else {
                    return Err(Error::corrupt(
                        "WFX3 objects_with_attr patch has no base row",
                    ));
                };
                apply_id_patch(ids, &mut reader)?;
            }
            ABSENT => {
                if header.kind == SegmentKind::Full || objects_with_attr.remove(&name).is_none() {
                    return Err(Error::corrupt(
                        "WFX3 objects_with_attr tombstone has no base row",
                    ));
                }
            }
            _ => return Err(Error::corrupt("WFX3 present flag is not 0/1/2")),
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
            PATCH => {
                let Some(ids) = objects_by_attr_eq.get_mut(&(name, key)) else {
                    return Err(Error::corrupt("WFX3 attr_eq patch has no base row"));
                };
                apply_id_patch(ids, &mut reader)?;
            }
            ABSENT => {
                if header.kind == SegmentKind::Full
                    || objects_by_attr_eq.remove(&(name, key)).is_none()
                {
                    return Err(Error::corrupt("WFX3 attr_eq tombstone has no base row"));
                }
            }
            _ => return Err(Error::corrupt("WFX3 present flag is not 0/1/2")),
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
            PATCH => {
                let Some(edges) = relations_from.get_mut(&key) else {
                    return Err(Error::corrupt("WFX3 relation patch has no base row"));
                };
                apply_edge_patch(edges, &mut reader)?;
            }
            ABSENT => {
                if header.kind == SegmentKind::Full || relations_from.remove(&key).is_none() {
                    return Err(Error::corrupt("WFX3 relation tombstone has no base row"));
                }
            }
            _ => return Err(Error::corrupt("WFX3 present flag is not 0/1/2")),
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
            PATCH => {
                let Some(ids) = tags.get_mut(&tag) else {
                    return Err(Error::corrupt("WFX3 tag patch has no base row"));
                };
                apply_id_patch(ids, &mut reader)?;
            }
            ABSENT => {
                if header.kind == SegmentKind::Full || tags.remove(&tag).is_none() {
                    return Err(Error::corrupt("WFX3 tag tombstone has no base row"));
                }
            }
            _ => return Err(Error::corrupt("WFX3 present flag is not 0/1/2")),
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
