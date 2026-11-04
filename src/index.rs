//! Per-state contextual index for selector candidate narrowing.
//!
//! The index is **not** a semantic authority. `evaluate::eval_expr` remains the
//! only definition of a match; every candidate produced here is re-evaluated.
//! The index may over-approximate (a superset of true matches) but must never
//! under-approximate: a missing index entry would silently drop a result.
//!
//! Keying:
//! * `objects_by_attr_eq` — `(attribute name, canonical WCB bytes of a
//!   normalized comparison key)`. Numeric families (`U64`, `I64`,
//!   `Timestamp`) normalize to one decimal `i128` key because
//!   `evaluate::equal_values` compares them by value; all other scalar types
//!   stay type-tagged. Arrays and maps can never equal a selector literal
//!   (`Literal` is scalar-only), so they are deliberately absent.
//! * `objects_with_attr` — present, non-null attribute names (`Has`).
//! * `relations_from` — `(subject, predicate) -> edges`, used to derive
//!   object-target relation candidates.
//! * `tags` — `tagged_as` value-relation text to subjects.
//!
//! All keys and payloads are owned, so a `StateIndex` can outlive the `State`
//! it was built from and be cached against the snapshot that produced it.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use weftcodec::selector::Field;
use weftcodec::wcb::{self, Value};

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::state::{RelationKind, RelationTarget, State};

/// Number of replayed states whose derived index is retained by a store.
pub(crate) const DEFAULT_INDEX_CAPACITY: usize = 8;

/// The superseded canonical-WCB index magic. WFX1 files are advisory and are
/// discarded (rebuilt from state), never interpreted by this version.
pub(crate) const INDEX_MAGIC_V1: &[u8; 4] = b"WFX1";
/// Compact binary index magic.
pub(crate) const INDEX_MAGIC_V2: &[u8; 4] = b"WFX2";
/// WFX2 layout version.
pub(crate) const INDEX_VERSION_V2: u32 = 1;
/// Domain for the integrity digest over every preceding WFX2 byte.
pub(crate) const INDEX_V2_DOMAIN: &str = "weft/index/v2";
/// `magic(4) + version(4) + commit(32) + generation/object/relation/entry(32)`.
const INDEX_V2_HEADER_BYTES: usize = 72;
/// Trailing BLAKE3 digest length.
const INDEX_DIGEST_BYTES: usize = 32;
/// Relation-target tags used by the sorted `relations_from` section.
const TARGET_OBJECT: u8 = 0;
const TARGET_NULL: u8 = 1;
const TARGET_BOOL: u8 = 2;
const TARGET_U64: u8 = 3;
const TARGET_I64: u8 = 4;
const TARGET_TIMESTAMP: u8 = 5;
const TARGET_STR: u8 = 6;
const TARGET_BYTES: u8 = 7;
/// Hard cap on one index file. Larger files are never written or loaded; the
/// index is advisory, so the caller falls back to rebuilding from state.
pub(crate) const MAX_INDEX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// One relation edge kept by the index.
#[derive(Clone, Debug)]
pub(crate) struct IndexedRelation {
    pub kind: RelationKind,
    pub target: RelationTarget,
}

/// Owned, data-only index derived from one replayed `State`.
#[derive(Debug)]
pub(crate) struct StateIndex {
    /// Commit whose replay produced this index (zero for an in-memory build
    /// that has not been attached to a commit yet).
    commit: Hash32,
    generation: u64,
    object_count: usize,
    relation_count: usize,
    entry_count: usize,
    /// False when at least one scalar attribute value could not be encoded.
    /// Callers must then treat every candidate set as a full authorized scan.
    complete: bool,
    pub objects_by_attr_eq: HashMap<(String, Vec<u8>), BTreeSet<Hash32>>,
    pub objects_with_attr: HashMap<String, BTreeSet<Hash32>>,
    pub relations_from: HashMap<(Hash32, String), Vec<IndexedRelation>>,
    pub tags: HashMap<String, BTreeSet<Hash32>>,
}

impl StateIndex {
    pub(crate) fn build(state: &State) -> StateIndex {
        let mut objects_by_attr_eq: HashMap<(String, Vec<u8>), BTreeSet<Hash32>> = HashMap::new();
        let mut objects_with_attr: HashMap<String, BTreeSet<Hash32>> = HashMap::new();
        let mut relations_from: HashMap<(Hash32, String), Vec<IndexedRelation>> = HashMap::new();
        let mut tags: HashMap<String, BTreeSet<Hash32>> = HashMap::new();
        let mut complete = true;

        for record in state.objects.values() {
            for (name, value) in &record.attrs {
                if !matches!(value, Value::Null) {
                    objects_with_attr
                        .entry(name.clone())
                        .or_default()
                        .insert(record.id);
                }
                match eq_key(value) {
                    Some(key) => {
                        objects_by_attr_eq
                            .entry((name.clone(), key))
                            .or_default()
                            .insert(record.id);
                    }
                    // Arrays/maps never equal a scalar literal. Any other
                    // failure is an encoding limit, which would lose entries;
                    // mark the index incomplete so callers scan instead.
                    None if !matches!(value, Value::Array(_) | Value::Map(_)) => {
                        complete = false;
                    }
                    None => {}
                }
            }
        }

        for relation in state.relations.values() {
            if relation.kind == RelationKind::Value && relation.predicate == "tagged_as" {
                if let RelationTarget::Value(Value::Str(text)) = &relation.target {
                    tags.entry(text.clone())
                        .or_default()
                        .insert(relation.subject);
                }
            }
            relations_from
                .entry((relation.subject, relation.predicate.clone()))
                .or_default()
                .push(IndexedRelation {
                    kind: relation.kind,
                    target: relation.target.clone(),
                });
        }

        StateIndex {
            commit: Hash32::ZERO,
            generation: state.generation,
            object_count: state.objects.len(),
            relation_count: state.relations.len(),
            entry_count: state.entries.len(),
            complete,
            objects_by_attr_eq,
            objects_with_attr,
            relations_from,
            tags,
        }
    }

    pub(crate) fn complete(&self) -> bool {
        self.complete
    }

    /// Number of indexed objects (benchmark/diagnostic only).
    pub(crate) fn object_count(&self) -> usize {
        self.object_count
    }

    /// The commit this index describes.
    pub(crate) fn commit(&self) -> Hash32 {
        self.commit
    }

    /// Attach the commit key before persisting a freshly built index.
    pub(crate) fn with_commit(mut self, commit: Hash32) -> StateIndex {
        self.commit = commit;
        self
    }

    /// Encode the compact advisory file (WFX2).
    ///
    /// Sections are written once, sorted, so a load is a sequential read plus
    /// map insertion. Cross-language index parity is not claimed; the payload
    /// is internal and rebuildable, not a frozen store format.
    pub(crate) fn encode_file(&self) -> Result<Vec<u8>> {
        // A WFX2 segment is only written when every scalar attribute value was
        // encodable; the loaded index is therefore always `complete`. An
        // incomplete index must never be persisted and later treated as exact.
        if !self.complete {
            return Err(Error::invalid_argument(
                "an incomplete selector index cannot be persisted",
            ));
        }
        let mut out = Vec::with_capacity(INDEX_V2_HEADER_BYTES + 1024);
        out.extend_from_slice(INDEX_MAGIC_V2);
        out.extend_from_slice(&INDEX_VERSION_V2.to_le_bytes());
        out.extend_from_slice(self.commit.as_bytes());
        push_u64(&mut out, self.generation);
        push_u64(&mut out, self.object_count as u64);
        push_u64(&mut out, self.relation_count as u64);
        push_u64(&mut out, self.entry_count as u64);

        let mut names: Vec<&String> = self.objects_with_attr.keys().collect();
        names.sort();
        push_u32(&mut out, names.len() as u64)?;
        for name in names {
            push_str(&mut out, name)?;
            push_ids(&mut out, &self.objects_with_attr[name])?;
        }

        let mut attr_eq: Vec<(&(String, Vec<u8>), &BTreeSet<Hash32>)> =
            self.objects_by_attr_eq.iter().collect();
        attr_eq.sort_by(|a, b| a.0 .0.cmp(&b.0 .0).then_with(|| a.0 .1.cmp(&b.0 .1)));
        push_u32(&mut out, attr_eq.len() as u64)?;
        for ((name, key), ids) in attr_eq {
            push_str(&mut out, name)?;
            push_bytes(&mut out, key)?;
            push_ids(&mut out, ids)?;
        }

        // Flatten every edge and sort by (subject, predicate, target key).
        let mut edges: Vec<(Hash32, &String, Vec<u8>)> = Vec::new();
        for ((subject, predicate), list) in &self.relations_from {
            for edge in list {
                edges.push((*subject, predicate, target_bytes(edge)?));
            }
        }
        edges.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        push_u32(&mut out, edges.len() as u64)?;
        for (subject, predicate, target) in edges {
            out.extend_from_slice(subject.as_bytes());
            push_str(&mut out, predicate)?;
            out.extend_from_slice(&target);
        }

        let mut tags: Vec<&String> = self.tags.keys().collect();
        tags.sort();
        push_u32(&mut out, tags.len() as u64)?;
        for tag in tags {
            push_str(&mut out, tag)?;
            push_ids(&mut out, &self.tags[tag])?;
        }

        let total = out
            .len()
            .checked_add(INDEX_DIGEST_BYTES)
            .ok_or_else(|| Error::limit_exceeded("index file size overflows usize"))?;
        if total as u64 > MAX_INDEX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "index file is {total} bytes, above the {MAX_INDEX_FILE_BYTES}-byte cap"
            )));
        }
        let digest = identity::h(INDEX_V2_DOMAIN, &out);
        out.extend_from_slice(digest.as_bytes());
        Ok(out)
    }

    /// Decode a WFX2 advisory file. Any failure is `Err`; the caller ignores
    /// the index and rebuilds from state. WFX1 files are deliberately
    /// discarded (their canonical-WCB decode was slower than rebuilding).
    pub(crate) fn decode_file(bytes: &[u8]) -> Result<StateIndex> {
        if bytes.len() < INDEX_V2_HEADER_BYTES + INDEX_DIGEST_BYTES {
            return Err(Error::corrupt("index file is truncated"));
        }
        if &bytes[0..4] == INDEX_MAGIC_V1 {
            return Err(Error::unsupported_version(
                "WFX1 index segment is advisory and superseded; rebuilding",
            ));
        }
        if &bytes[0..4] != INDEX_MAGIC_V2 {
            return Err(Error::malformed("bad index file magic"));
        }
        if bytes.len() as u64 > MAX_INDEX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "index file is {} bytes, above the {MAX_INDEX_FILE_BYTES}-byte cap",
                bytes.len()
            )));
        }
        let (body, trailer) = bytes.split_at(bytes.len() - INDEX_DIGEST_BYTES);
        let digest = identity::h(INDEX_V2_DOMAIN, body);
        if trailer != digest.as_bytes() {
            return Err(Error::corrupt("index integrity digest mismatch"));
        }
        let mut reader = IndexReader::new(body);
        let magic = reader.take(4)?;
        if magic != INDEX_MAGIC_V2 {
            return Err(Error::malformed("bad index file magic"));
        }
        let version = reader.u32()?;
        if version != INDEX_VERSION_V2 {
            return Err(Error::unsupported_version(format!(
                "index version {version} is not {INDEX_VERSION_V2}"
            )));
        }
        let commit = reader.hash32()?;
        let generation = reader.u64()?;
        let object_count = usize_of(reader.u64()?, "object count")?;
        let relation_count = usize_of(reader.u64()?, "relation count")?;
        let entry_count = usize_of(reader.u64()?, "entry count")?;

        let objects_with_attr = read_named_sets(&mut reader, 8)?;

        let attr_eq_count = reader.bounded_count(12)?;
        let mut objects_by_attr_eq = HashMap::with_capacity(attr_eq_count);
        for _ in 0..attr_eq_count {
            let name = reader.string()?;
            let key = reader.bytes()?.to_vec();
            let ids = reader.id_set()?;
            if objects_by_attr_eq.insert((name, key), ids).is_some() {
                return Err(Error::corrupt("duplicate attr_eq entry in index"));
            }
        }

        let edge_count = reader.bounded_count(37)?;
        let mut relations_from: HashMap<(Hash32, String), Vec<IndexedRelation>> =
            HashMap::with_capacity(edge_count);
        let mut current_key: Option<(Hash32, String)> = None;
        let mut current_edges: Vec<IndexedRelation> = Vec::new();
        for _ in 0..edge_count {
            let subject = reader.hash32()?;
            let predicate = reader.string()?;
            let (kind, target) = read_target(&mut reader)?;
            let edge = IndexedRelation { kind, target };
            let same = match &current_key {
                Some((current_subject, current_predicate)) => {
                    *current_subject == subject && current_predicate == &predicate
                }
                None => false,
            };
            if same {
                current_edges.push(edge);
            } else {
                if let Some(key) = current_key.take() {
                    if relations_from
                        .insert(key, std::mem::take(&mut current_edges))
                        .is_some()
                    {
                        return Err(Error::corrupt("duplicate relations_from entry in index"));
                    }
                }
                current_key = Some((subject, predicate));
                current_edges.push(edge);
            }
        }
        if let Some(key) = current_key.take() {
            if relations_from.insert(key, current_edges).is_some() {
                return Err(Error::corrupt("duplicate relations_from entry in index"));
            }
        }

        let tags = read_named_sets(&mut reader, 8)?;
        if reader.remaining() != 0 {
            return Err(Error::corrupt("trailing bytes after index sections"));
        }

        Ok(StateIndex {
            commit,
            generation,
            object_count,
            relation_count,
            entry_count,
            complete: true,
            objects_by_attr_eq,
            objects_with_attr,
            relations_from,
            tags,
        })
    }

    /// Cheap structural check that this index still describes `state`. The
    /// cache key is the snapshot `State` was replayed at; this guard rejects an
    /// index whose state shape has changed underneath that key.
    pub(crate) fn matches_state(&self, state: &State) -> bool {
        self.generation == state.generation
            && self.object_count == state.objects.len()
            && self.relation_count == state.relations.len()
            && self.entry_count == state.entries.len()
    }
}

/// Attribute name addressed by an `attrs.<path>` field. Longer paths are
/// joined with `.`, matching `evaluate::resolve_field`.
pub(crate) fn attr_field_key(field: &Field) -> Option<String> {
    let first = field.segments.first()?.as_str();
    if first == "attrs" && field.segments.len() > 1 {
        Some(field.segments[1..].join("."))
    } else {
        None
    }
}

/// Root fields that `resolve_field` always returns non-null for. `Has` on
/// these is true for every authorized object, so no index is required.
pub(crate) fn field_always_present(field: &Field) -> bool {
    field.segments.len() == 1
        && matches!(
            field.segments[0].as_str(),
            "object_id" | "content_id" | "namespace" | "size" | "mtime" | "kind"
        )
}

/// Canonical comparison key for attribute equality.
///
/// Returns `None` for arrays/maps (never equal to a scalar literal) and for
/// scalar values the canonical encoder rejects under default limits.
pub(crate) fn eq_key(value: &Value) -> Option<Vec<u8>> {
    let normalized = match value {
        Value::Null => tagged("null", Value::Null),
        Value::Bool(flag) => tagged("bool", Value::Bool(*flag)),
        // `equal_values` compares the numeric families by value.
        Value::U64(number) => numeric(*number as i128),
        Value::I64(number) => numeric(*number as i128),
        Value::Timestamp(number) => numeric(*number as i128),
        Value::Str(text) => tagged("str", Value::Str(text.clone())),
        Value::Bytes(bytes) => tagged("bytes", Value::Bytes(bytes.clone())),
        Value::Array(_) | Value::Map(_) => return None,
    };
    wcb::encode_default(&normalized).ok()
}

fn push_u32(out: &mut Vec<u8>, value: u64) -> Result<()> {
    let value =
        u32::try_from(value).map_err(|_| Error::limit_exceeded("index count exceeds u32"))?;
    out.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    push_u32(out, bytes.len() as u64)?;
    out.extend_from_slice(bytes);
    Ok(())
}

fn push_str(out: &mut Vec<u8>, text: &str) -> Result<()> {
    push_bytes(out, text.as_bytes())
}

fn push_ids(out: &mut Vec<u8>, ids: &BTreeSet<Hash32>) -> Result<()> {
    push_u32(out, ids.len() as u64)?;
    for id in ids {
        out.extend_from_slice(id.as_bytes());
    }
    Ok(())
}

/// `target kind || target payload`, used as the sort key and written verbatim.
fn target_bytes(edge: &IndexedRelation) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match &edge.target {
        RelationTarget::Object(hash) => {
            out.push(TARGET_OBJECT);
            out.extend_from_slice(hash.as_bytes());
        }
        RelationTarget::Value(value) => match value {
            Value::Null => out.push(TARGET_NULL),
            Value::Bool(flag) => {
                out.push(TARGET_BOOL);
                out.push(u8::from(*flag));
            }
            Value::U64(number) => {
                out.push(TARGET_U64);
                push_u64(&mut out, *number);
            }
            Value::I64(number) => {
                out.push(TARGET_I64);
                push_u64(&mut out, *number as u64);
            }
            Value::Timestamp(number) => {
                out.push(TARGET_TIMESTAMP);
                push_u64(&mut out, *number as u64);
            }
            Value::Str(text) => {
                out.push(TARGET_STR);
                push_str(&mut out, text)?;
            }
            Value::Bytes(bytes) => {
                out.push(TARGET_BYTES);
                push_bytes(&mut out, bytes)?;
            }
            Value::Array(_) | Value::Map(_) => {
                return Err(Error::malformed(
                    "index relation value target must be a scalar",
                ))
            }
        },
    }
    Ok(out)
}

fn read_named_sets(
    reader: &mut IndexReader<'_>,
    min_element_bytes: usize,
) -> Result<HashMap<String, BTreeSet<Hash32>>> {
    let count = reader.bounded_count(min_element_bytes)?;
    let mut map = HashMap::with_capacity(count);
    for _ in 0..count {
        let name = reader.string()?;
        let ids = reader.id_set()?;
        if map.insert(name, ids).is_some() {
            return Err(Error::corrupt("duplicate named set in index"));
        }
    }
    Ok(map)
}

fn read_target(reader: &mut IndexReader<'_>) -> Result<(RelationKind, RelationTarget)> {
    let tag = reader.u8()?;
    match tag {
        TARGET_OBJECT => Ok((
            RelationKind::Object,
            RelationTarget::Object(reader.hash32()?),
        )),
        TARGET_NULL => Ok((RelationKind::Value, RelationTarget::Value(Value::Null))),
        TARGET_BOOL => {
            let flag = reader.u8()?;
            if flag > 1 {
                return Err(Error::malformed("index boolean target is not 0/1"));
            }
            Ok((
                RelationKind::Value,
                RelationTarget::Value(Value::Bool(flag == 1)),
            ))
        }
        TARGET_U64 => Ok((
            RelationKind::Value,
            RelationTarget::Value(Value::U64(reader.u64()?)),
        )),
        TARGET_I64 => Ok((
            RelationKind::Value,
            RelationTarget::Value(Value::I64(reader.u64()? as i64)),
        )),
        TARGET_TIMESTAMP => Ok((
            RelationKind::Value,
            RelationTarget::Value(Value::Timestamp(reader.u64()? as i64)),
        )),
        TARGET_STR => Ok((
            RelationKind::Value,
            RelationTarget::Value(Value::Str(reader.string()?)),
        )),
        TARGET_BYTES => Ok((
            RelationKind::Value,
            RelationTarget::Value(Value::Bytes(reader.bytes()?.to_vec())),
        )),
        other => Err(Error::malformed(format!(
            "unknown index relation target tag {other}"
        ))),
    }
}

/// Bounds-checked little-endian reader over the WFX2 body (digest excluded).
struct IndexReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> IndexReader<'a> {
    fn new(bytes: &'a [u8]) -> IndexReader<'a> {
        IndexReader { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| Error::corrupt("index section length overflows"))?;
        if end > self.bytes.len() {
            return Err(Error::corrupt("index section is truncated"));
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn hash32(&mut self) -> Result<Hash32> {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(self.take(32)?);
        Ok(Hash32::from_bytes(bytes))
    }

    fn bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn string(&mut self) -> Result<String> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::malformed("index text is not valid UTF-8"))
    }

    /// Read a section count after proving the remaining bytes can hold that
    /// many minimum-size elements. Prevents a corrupt count from allocating.
    fn bounded_count(&mut self, min_element_bytes: usize) -> Result<usize> {
        let count = self.u32()? as usize;
        if min_element_bytes != 0 && count > self.remaining() / min_element_bytes {
            return Err(Error::corrupt(
                "index section count exceeds the remaining bytes",
            ));
        }
        Ok(count)
    }

    fn id_set(&mut self) -> Result<BTreeSet<Hash32>> {
        let count = self.u32()? as usize;
        if count > self.remaining() / 32 {
            return Err(Error::corrupt(
                "index object-id set count exceeds the remaining bytes",
            ));
        }
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            ids.push(self.hash32()?);
        }
        Ok(ids.into_iter().collect())
    }
}

fn usize_of(value: u64, what: &str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| Error::limit_exceeded(format!("{what} ({value}) does not fit usize")))
}

fn tagged(tag: &str, value: Value) -> Value {
    Value::Array(vec![Value::Str(format!("weft.eq.{tag}")), value])
}

fn numeric(value: i128) -> Value {
    Value::Array(vec![
        Value::Str("weft.eq.num".to_owned()),
        Value::Str(value.to_string()),
    ])
}

/// Bounded LRU of `StateIndex` keyed by the snapshot commit hash that produced
/// the state. Eviction is explicit (oldest first) and no unbounded growth is
/// possible.
pub(crate) struct StateIndexCache {
    capacity: usize,
    entries: VecDeque<(Hash32, Arc<StateIndex>)>,
}

impl StateIndexCache {
    pub(crate) fn new(capacity: usize) -> StateIndexCache {
        StateIndexCache {
            capacity: capacity.max(1),
            entries: VecDeque::new(),
        }
    }

    pub(crate) fn get(&mut self, key: Hash32) -> Option<Arc<StateIndex>> {
        let position = self
            .entries
            .iter()
            .position(|(candidate, _)| *candidate == key)?;
        let entry = self.entries.remove(position)?;
        let index = entry.1.clone();
        self.entries.push_back(entry);
        Some(index)
    }

    pub(crate) fn insert(&mut self, key: Hash32, index: Arc<StateIndex>) {
        if let Some(position) = self
            .entries
            .iter()
            .position(|(candidate, _)| *candidate == key)
        {
            self.entries.remove(position);
        }
        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((key, index));
    }
}
