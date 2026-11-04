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

use crate::identity::Hash32;
use crate::state::{RelationKind, RelationTarget, State};

/// Number of replayed states whose derived index is retained by a store.
pub(crate) const DEFAULT_INDEX_CAPACITY: usize = 8;

/// One relation edge kept by the index.
#[derive(Clone, Debug)]
pub(crate) struct IndexedRelation {
    pub kind: RelationKind,
    pub target: RelationTarget,
}

/// Owned, data-only index derived from one replayed `State`.
#[derive(Debug)]
pub(crate) struct StateIndex {
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
