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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use weftcodec::selector::Field;
use weftcodec::wcb::{self, Value};

use crate::error::{Error, Result};
use crate::identity::{self, Hash32};
use crate::state::{RelationKind, RelationTarget, State};
use crate::wcb_util as wu;

/// Number of replayed states whose derived index is retained by a store.
pub(crate) const DEFAULT_INDEX_CAPACITY: usize = 8;

/// Advisory on-disk index file magic: `WFX1`.
pub(crate) const INDEX_FILE_MAGIC: &[u8; 4] = b"WFX1";
/// Domain for the BLAKE3 integrity digest over the canonical index WCB.
pub(crate) const INDEX_DOMAIN: &str = "weft/index/v1";
/// Canonical schema string for a persisted per-commit index.
pub(crate) const INDEX_SCHEMA: &str = "weftfs.index.v1";
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

    /// Canonical WCB value for the persisted form (`weftfs.index.v1`).
    ///
    /// Array elements are sorted so the same state always produces identical
    /// bytes: attribute names, `(name, comparison key)` pairs, relation
    /// `(subject, predicate)` pairs, object-id sets, and relation edges.
    pub(crate) fn to_value(&self) -> Value {
        let mut attrs: Vec<(&String, &BTreeSet<Hash32>)> = self.objects_with_attr.iter().collect();
        attrs.sort_by(|a, b| a.0.cmp(b.0));
        let attrs = Value::Map(
            attrs
                .into_iter()
                .map(|(name, ids)| (name.clone(), hash_set_value(ids)))
                .collect::<BTreeMap<String, Value>>(),
        );

        let mut attr_eq: Vec<(&(String, Vec<u8>), &BTreeSet<Hash32>)> =
            self.objects_by_attr_eq.iter().collect();
        attr_eq.sort_by(|a, b| a.0 .0.cmp(&b.0 .0).then_with(|| a.0 .1.cmp(&b.0 .1)));
        let attr_eq = Value::Array(
            attr_eq
                .into_iter()
                .map(|((name, key), ids)| {
                    wu::map([
                        ("name", Value::Str(name.clone())),
                        ("key", Value::Bytes(key.clone())),
                        ("objects", hash_set_value(ids)),
                    ])
                })
                .collect(),
        );

        let mut relations: Vec<(&(Hash32, String), &Vec<IndexedRelation>)> =
            self.relations_from.iter().collect();
        relations.sort_by(|a, b| a.0 .0.cmp(&b.0 .0).then_with(|| a.0 .1.cmp(&b.0 .1)));
        let relations_from = Value::Array(
            relations
                .into_iter()
                .map(|((subject, predicate), edges)| {
                    let mut edges: Vec<&IndexedRelation> = edges.iter().collect();
                    edges.sort_by_cached_key(|edge| edge_sort_key(edge));
                    wu::map([
                        ("subject", wu::hash_value(*subject)),
                        ("predicate", Value::Str(predicate.clone())),
                        (
                            "edges",
                            Value::Array(edges.iter().map(|e| relation_to_value(e)).collect()),
                        ),
                    ])
                })
                .collect(),
        );

        let mut tags: Vec<(&String, &BTreeSet<Hash32>)> = self.tags.iter().collect();
        tags.sort_by(|a, b| a.0.cmp(b.0));
        let tags = Value::Map(
            tags.into_iter()
                .map(|(tag, ids)| (tag.clone(), hash_set_value(ids)))
                .collect::<BTreeMap<String, Value>>(),
        );

        wu::map([
            ("schema", Value::Str(INDEX_SCHEMA.to_owned())),
            ("commit", wu::hash_value(self.commit)),
            ("generation", Value::U64(self.generation)),
            ("object_count", Value::U64(self.object_count as u64)),
            ("relation_count", Value::U64(self.relation_count as u64)),
            ("entry_count", Value::U64(self.entry_count as u64)),
            ("complete", Value::Bool(self.complete)),
            ("attrs", attrs),
            ("attr_eq", attr_eq),
            ("relations_from", relations_from),
            ("tags", tags),
        ])
    }

    /// Encode the advisory file:
    /// `WFX1 || u64le len || canonical WCB || BLAKE3(weft/index/v1, WCB)`.
    pub(crate) fn encode_file(&self) -> Result<Vec<u8>> {
        let wcb_bytes = wu::encode(&self.to_value())?;
        let total = 4usize
            .checked_add(8)
            .and_then(|n| n.checked_add(wcb_bytes.len()))
            .and_then(|n| n.checked_add(32))
            .ok_or_else(|| Error::limit_exceeded("index file size overflows usize"))?;
        if total as u64 > MAX_INDEX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "index file is {total} bytes, above the {MAX_INDEX_FILE_BYTES}-byte cap"
            )));
        }
        let len = u64::try_from(wcb_bytes.len())
            .map_err(|_| Error::limit_exceeded("index encoding exceeds u64 length"))?;
        let digest = identity::h(INDEX_DOMAIN, &wcb_bytes);
        let mut out = Vec::with_capacity(4 + 8 + wcb_bytes.len() + 32);
        out.extend_from_slice(INDEX_FILE_MAGIC);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&wcb_bytes);
        out.extend_from_slice(digest.as_bytes());
        Ok(out)
    }

    /// Decode an advisory file, checking magic, exact length, integrity
    /// digest, schema, and canonical re-encoding. Any failure is `Err`; the
    /// caller ignores the index and rebuilds from state.
    pub(crate) fn decode_file(bytes: &[u8]) -> Result<StateIndex> {
        if bytes.len() < 4 + 8 + 32 {
            return Err(Error::corrupt("index file is truncated"));
        }
        if &bytes[0..4] != INDEX_FILE_MAGIC {
            return Err(Error::malformed("bad index file magic"));
        }
        let len = u64::from_le_bytes(bytes[4..12].try_into().expect("8 bytes"));
        if len > MAX_INDEX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "index declares {len} bytes, above the {MAX_INDEX_FILE_BYTES}-byte cap"
            )));
        }
        let len = usize::try_from(len)
            .map_err(|_| Error::limit_exceeded("index length exceeds usize"))?;
        let expected = 12usize
            .checked_add(len)
            .and_then(|n| n.checked_add(32))
            .ok_or_else(|| Error::malformed("index length overflows"))?;
        if expected as u64 > MAX_INDEX_FILE_BYTES {
            return Err(Error::limit_exceeded(format!(
                "index file is {expected} bytes, above the {MAX_INDEX_FILE_BYTES}-byte cap"
            )));
        }
        if expected != bytes.len() {
            return Err(Error::corrupt(format!(
                "index file length {} does not match declared {expected}",
                bytes.len()
            )));
        }
        let wcb_bytes = &bytes[12..12 + len];
        let stored = &bytes[12 + len..];
        let digest = identity::h(INDEX_DOMAIN, wcb_bytes);
        if stored != digest.as_bytes() {
            return Err(Error::corrupt("index integrity digest mismatch"));
        }
        let value = wu::decode(wcb_bytes)?;
        // `from_value` enforces the canonical array order for every
        // order-sensitive field; the WCB decoder already rejects non-minimal
        // varints, unsorted/duplicate map keys, and trailing bytes.
        StateIndex::from_value(value)
    }

    /// Consume the decoded WCB tree, moving strings/byte vectors into the
    /// index maps instead of cloning them.
    fn from_value(value: Value) -> Result<StateIndex> {
        let mut map = match value {
            Value::Map(map) => map,
            _ => return Err(Error::malformed("index root must be a WCB map")),
        };
        wu::check_keys(
            &map,
            &[
                "schema",
                "commit",
                "generation",
                "object_count",
                "relation_count",
                "entry_count",
                "complete",
                "attrs",
                "attr_eq",
                "relations_from",
                "tags",
            ],
        )?;
        let schema = take_text(&mut map, "schema")?;
        if schema != INDEX_SCHEMA {
            return Err(Error::unknown_schema(format!(
                "index schema `{schema}` is not `{INDEX_SCHEMA}`"
            )));
        }
        let commit = take_hash(&mut map, "commit")?;
        let generation = take_u64(&mut map, "generation")?;
        let object_count = usize_of(take_u64(&mut map, "object_count")?, "object count")?;
        let relation_count = usize_of(take_u64(&mut map, "relation_count")?, "relation count")?;
        let entry_count = usize_of(take_u64(&mut map, "entry_count")?, "entry count")?;
        let complete = match map.remove("complete") {
            Some(Value::Bool(flag)) => flag,
            Some(_) => return Err(Error::malformed("key `complete` must be bool")),
            None => return Err(Error::malformed("missing map key `complete`")),
        };

        let attrs = match map.remove("attrs") {
            Some(Value::Map(attrs)) => attrs,
            Some(_) => return Err(Error::malformed("key `attrs` must be a map")),
            None => return Err(Error::malformed("missing map key `attrs`")),
        };
        let mut objects_with_attr = HashMap::with_capacity(attrs.len());
        for (name, ids) in attrs {
            objects_with_attr.insert(name, hash_set_from_value(ids)?);
        }

        let attr_eq = match map.remove("attr_eq") {
            Some(Value::Array(items)) => items,
            Some(_) => return Err(Error::malformed("key `attr_eq` must be an array")),
            None => return Err(Error::malformed("missing map key `attr_eq`")),
        };
        let mut objects_by_attr_eq = HashMap::with_capacity(attr_eq.len());
        let mut previous_attr: Option<(String, Vec<u8>)> = None;
        for item in attr_eq {
            let mut entry = match item {
                Value::Map(entry) => entry,
                _ => return Err(Error::malformed("attr_eq entry must be a WCB map")),
            };
            wu::check_keys(&entry, &["name", "key", "objects"])?;
            let name = take_text(&mut entry, "name")?;
            let key = take_bytes(&mut entry, "key")?;
            if let Some((previous_name, previous_key)) = &previous_attr {
                if (previous_name.as_str(), previous_key.as_slice())
                    >= (name.as_str(), key.as_slice())
                {
                    return Err(Error::non_canonical(
                        "index attr_eq entries are not strictly sorted",
                    ));
                }
            }
            previous_attr = Some((name.clone(), key.clone()));
            let ids = match entry.remove("objects") {
                Some(value) => hash_set_from_value(value)?,
                None => return Err(Error::malformed("missing map key `objects`")),
            };
            if objects_by_attr_eq
                .insert((name.clone(), key), ids)
                .is_some()
            {
                return Err(Error::malformed(format!(
                    "duplicate attr_eq entry for `{name}`"
                )));
            }
        }

        let relations = match map.remove("relations_from") {
            Some(Value::Array(items)) => items,
            Some(_) => return Err(Error::malformed("key `relations_from` must be an array")),
            None => return Err(Error::malformed("missing map key `relations_from`")),
        };
        let mut relations_from: HashMap<(Hash32, String), Vec<IndexedRelation>> =
            HashMap::with_capacity(relations.len());
        let mut previous_relation: Option<(Hash32, String)> = None;
        for item in relations {
            let mut entry = match item {
                Value::Map(entry) => entry,
                _ => return Err(Error::malformed("relations_from entry must be a WCB map")),
            };
            wu::check_keys(&entry, &["subject", "predicate", "edges"])?;
            let subject = take_hash(&mut entry, "subject")?;
            let predicate = take_text(&mut entry, "predicate")?;
            if let Some((previous_subject, previous_predicate)) = &previous_relation {
                if (*previous_subject, previous_predicate.as_str()) >= (subject, predicate.as_str())
                {
                    return Err(Error::non_canonical(
                        "index relations_from entries are not strictly sorted",
                    ));
                }
            }
            previous_relation = Some((subject, predicate.clone()));
            let edges = match entry.remove("edges") {
                Some(Value::Array(edges)) => edges,
                Some(_) => return Err(Error::malformed("key `edges` must be an array")),
                None => return Err(Error::malformed("missing map key `edges`")),
            };
            let mut out = Vec::with_capacity(edges.len());
            let mut previous_edge: Option<Vec<u8>> = None;
            for edge in edges {
                let indexed = relation_from_value(edge)?;
                let key = edge_sort_key(&indexed);
                if let Some(previous) = &previous_edge {
                    if *previous > key {
                        return Err(Error::non_canonical("index relation edges are not sorted"));
                    }
                }
                previous_edge = Some(key);
                out.push(indexed);
            }
            if relations_from
                .insert((subject, predicate.clone()), out)
                .is_some()
            {
                return Err(Error::malformed(format!(
                    "duplicate relations_from entry for `{predicate}`"
                )));
            }
        }

        let tags = match map.remove("tags") {
            Some(Value::Map(tags)) => tags,
            Some(_) => return Err(Error::malformed("key `tags` must be a map")),
            None => return Err(Error::malformed("missing map key `tags`")),
        };
        let mut tags_out = HashMap::with_capacity(tags.len());
        for (tag, ids) in tags {
            tags_out.insert(tag, hash_set_from_value(ids)?);
        }

        Ok(StateIndex {
            commit,
            generation,
            object_count,
            relation_count,
            entry_count,
            complete,
            objects_by_attr_eq,
            objects_with_attr,
            relations_from,
            tags: tags_out,
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

fn hash_set_value(ids: &BTreeSet<Hash32>) -> Value {
    Value::Array(ids.iter().map(|id| wu::hash_value(*id)).collect())
}

fn take_text(map: &mut BTreeMap<String, Value>, key: &str) -> Result<String> {
    match map.remove(key) {
        Some(Value::Str(text)) => Ok(text),
        Some(_) => Err(Error::malformed(format!("key `{key}` must be text"))),
        None => Err(Error::malformed(format!("missing map key `{key}`"))),
    }
}

fn take_u64(map: &mut BTreeMap<String, Value>, key: &str) -> Result<u64> {
    match map.remove(key) {
        Some(Value::U64(value)) => Ok(value),
        Some(_) => Err(Error::malformed(format!("key `{key}` must be u64"))),
        None => Err(Error::malformed(format!("missing map key `{key}`"))),
    }
}

fn take_bytes(map: &mut BTreeMap<String, Value>, key: &str) -> Result<Vec<u8>> {
    match map.remove(key) {
        Some(Value::Bytes(bytes)) => Ok(bytes),
        Some(_) => Err(Error::malformed(format!("key `{key}` must be bytes"))),
        None => Err(Error::malformed(format!("missing map key `{key}`"))),
    }
}

fn take_hash(map: &mut BTreeMap<String, Value>, key: &str) -> Result<Hash32> {
    Hash32::from_slice(&take_bytes(map, key)?).map_err(Error::from)
}

fn hash_set_from_value(value: Value) -> Result<BTreeSet<Hash32>> {
    let items = match value {
        Value::Array(items) => items,
        _ => return Err(Error::malformed("index object-id set must be an array")),
    };
    let mut ids = BTreeSet::new();
    let mut previous: Option<Hash32> = None;
    for item in items {
        let hash = match item {
            Value::Bytes(bytes) => Hash32::from_slice(&bytes).map_err(Error::from)?,
            _ => return Err(Error::malformed("index object id must be bytes32")),
        };
        if let Some(previous) = previous {
            if hash <= previous {
                return Err(Error::non_canonical(
                    "index object-id set is not strictly sorted",
                ));
            }
        }
        previous = Some(hash);
        ids.insert(hash);
    }
    Ok(ids)
}

/// Deterministic edge order used by both the encoder and the decoder. Object
/// targets sort before value targets.
fn edge_sort_key(edge: &IndexedRelation) -> Vec<u8> {
    match &edge.target {
        RelationTarget::Object(hash) => {
            let mut key = Vec::with_capacity(1 + 32);
            key.push(0);
            key.extend_from_slice(hash.as_bytes());
            key
        }
        RelationTarget::Value(value) => {
            let mut key = Vec::with_capacity(1 + 16);
            key.push(1);
            key.extend_from_slice(&wu::encode(value).unwrap_or_default());
            key
        }
    }
}

fn relation_to_value(edge: &IndexedRelation) -> Value {
    match &edge.target {
        RelationTarget::Object(hash) => wu::map([
            ("kind", Value::Str(RelationKind::Object.as_str().to_owned())),
            ("object", wu::hash_value(*hash)),
        ]),
        RelationTarget::Value(value) => wu::map([
            ("kind", Value::Str(RelationKind::Value.as_str().to_owned())),
            ("value", value.clone()),
        ]),
    }
}

fn relation_from_value(value: Value) -> Result<IndexedRelation> {
    let mut map = match value {
        Value::Map(map) => map,
        _ => return Err(Error::malformed("index relation edge must be a WCB map")),
    };
    let kind = RelationKind::parse(&take_text(&mut map, "kind")?)?;
    match kind {
        RelationKind::Object => {
            let target = take_hash(&mut map, "object")?;
            if !map.is_empty() {
                return Err(Error::malformed("unexpected key in object relation edge"));
            }
            Ok(IndexedRelation {
                kind,
                target: RelationTarget::Object(target),
            })
        }
        RelationKind::Value => {
            let target = match map.remove("value") {
                Some(value) => value,
                None => return Err(Error::malformed("missing map key `value`")),
            };
            if !map.is_empty() {
                return Err(Error::malformed("unexpected key in value relation edge"));
            }
            wu::validate_scalar(&target)?;
            Ok(IndexedRelation {
                kind,
                target: RelationTarget::Value(target),
            })
        }
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
