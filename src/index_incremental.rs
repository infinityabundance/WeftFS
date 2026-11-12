//! Operation-derived incremental maintenance of the advisory selector index.
//!
//! `StateIndex::build` is the oracle: it is a pure function of a replayed
//! [`State`]. This module produces the *next* index from the previous index and
//! the committed operation stream, mutating only the affected rows. The result
//! must be identical to a full rebuild; any inconsistency (a row the ops say
//! should exist but the base does not have, a missing pre-image) returns
//! `None`, and the caller rebuilds from state. Nothing partial is ever used.
//!
//! What "affected" means:
//! * `put_object` / `delete_object` — the object's attribute rows
//!   (`objects_with_attr`, `objects_by_attr_eq`);
//! * `put_relation` / `del_relation` — the `(subject, predicate)` row and, for
//!   `tagged_as` value relations, the tag set;
//! * `put_entry` / `del_entry` / `set_snapshot` / `set_tip` /
//!   `set_quota_usage` — no index rows (they change counts only).
//!
//! The changed rows are recorded as *element-level* membership before the
//! first mutation, so the published WFX3 delta is proportional to the change,
//! not to the size of the row it touches.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use weftcodec::wcb::Value;

use crate::error::Result;
use crate::identity::Hash32;
use crate::index::{eq_key, target_bytes, IndexedRelation, StateIndex};
use crate::state::{ObjectRecord, Op, RelationKind, RelationRecord, RelationTarget, State};

/// Membership of every element this commit touched, recorded before the first
/// mutation, plus whether the row itself existed before the commit.
#[derive(Debug, Default)]
pub(crate) struct RowChange {
    /// `true` when the row key existed in the base index.
    pub base_present: bool,
    /// Element key -> membership in the base row.
    pub elements: BTreeMap<Vec<u8>, bool>,
}

/// The row-level change set of one commit, in the four index sections.
#[derive(Debug, Default)]
pub(crate) struct IndexDelta {
    pub with_attr: BTreeMap<String, RowChange>,
    pub attr_eq: BTreeMap<(String, Vec<u8>), RowChange>,
    pub relations: BTreeMap<(Hash32, String), RowChange>,
    pub tags: BTreeMap<String, RowChange>,
}

impl IndexDelta {
    pub(crate) fn is_empty(&self) -> bool {
        self.with_attr.is_empty()
            && self.attr_eq.is_empty()
            && self.relations.is_empty()
            && self.tags.is_empty()
    }
}

/// The records that existed *before* this commit for every object and relation
/// id the commit touches. `None` means the id did not exist.
#[derive(Debug, Default)]
pub(crate) struct PreImages {
    pub objects: BTreeMap<Hash32, Option<ObjectRecord>>,
    pub relations: BTreeMap<Hash32, Option<RelationRecord>>,
}

impl PreImages {
    pub(crate) fn empty() -> PreImages {
        PreImages::default()
    }

    /// Capture the pre-op image of every index-relevant id named by `ops` from
    /// the state *before* the ops applied.
    pub(crate) fn capture(state: &State, ops: &[Op]) -> PreImages {
        let mut pre = PreImages::default();
        for op in ops {
            match op {
                Op::PutObject(record) => {
                    pre.objects
                        .entry(record.id)
                        .or_insert_with(|| state.objects.get(&record.id).cloned());
                }
                Op::DeleteObject(id) => {
                    pre.objects
                        .entry(*id)
                        .or_insert_with(|| state.objects.get(id).cloned());
                }
                Op::PutRelation(record) => {
                    pre.relations
                        .entry(record.id)
                        .or_insert_with(|| state.relations.get(&record.id).cloned());
                }
                Op::DelRelation(id) => {
                    pre.relations
                        .entry(*id)
                        .or_insert_with(|| state.relations.get(id).cloned());
                }
                _ => {}
            }
        }
        pre
    }
}

/// Derive the index for `state` by applying `ops` to `index`.
///
/// `index` must be the index of the state *before* `ops` (its generation must
/// be one less than `state.generation`). `None` means the base could not be
/// maintained exactly; the caller must rebuild with [`StateIndex::build`].
pub(crate) fn apply(
    mut index: StateIndex,
    state: &State,
    ops: &[Op],
    pre: &PreImages,
) -> Option<(StateIndex, IndexDelta)> {
    if index.generation().checked_add(1) != Some(state.generation) {
        return None;
    }
    for op in ops {
        match op {
            Op::PutObject(record) if !pre.objects.contains_key(&record.id) => return None,
            Op::DeleteObject(id) if !pre.objects.contains_key(id) => return None,
            Op::PutRelation(record) if !pre.relations.contains_key(&record.id) => return None,
            Op::DelRelation(id) if !pre.relations.contains_key(id) => return None,
            _ => {}
        }
    }

    let mut delta = IndexDelta::default();

    // Group per id so several ops on one object in one commit cost one
    // remove/add pair, and the *post* state is the single authority for what
    // the object looks like now.
    let mut objects: BTreeSet<Hash32> = BTreeSet::new();
    let mut relations: BTreeSet<Hash32> = BTreeSet::new();
    for op in ops {
        match op {
            Op::PutObject(record) => {
                objects.insert(record.id);
            }
            Op::DeleteObject(id) => {
                objects.insert(*id);
            }
            Op::PutRelation(record) => {
                relations.insert(record.id);
            }
            Op::DelRelation(id) => {
                relations.insert(*id);
            }
            _ => {}
        }
    }

    for id in objects {
        let old = pre.objects.get(&id).and_then(|record| record.as_ref());
        let new = state.objects.get(&id);
        if old.map(|record| &record.attrs) == new.map(|record| &record.attrs) {
            continue;
        }
        if let Some(old) = old {
            remove_object(&mut index, &mut delta, old)?;
        }
        if let Some(new) = new {
            add_object(&mut index, &mut delta, new);
        }
    }

    for id in relations {
        let old = pre.relations.get(&id).and_then(|record| record.as_ref());
        let new = state.relations.get(&id);
        if old == new {
            continue;
        }
        if let Some(old) = old {
            remove_relation(&mut index, &mut delta, old)?;
        }
        if let Some(new) = new {
            add_relation(&mut index, &mut delta, new)?;
        }
    }

    index.set_shape(
        state.generation,
        state.objects.len(),
        state.relations.len(),
        state.entries.len(),
    );
    Some((index, delta))
}

fn remove_object(
    index: &mut StateIndex,
    delta: &mut IndexDelta,
    record: &ObjectRecord,
) -> Option<()> {
    for (name, value) in &record.attrs {
        if !matches!(value, Value::Null) {
            remove_id(
                &mut index.objects_with_attr,
                &mut delta.with_attr,
                name.clone(),
                record.id,
            )?;
        }
        match eq_key(value) {
            Some(key) => remove_id(
                &mut index.objects_by_attr_eq,
                &mut delta.attr_eq,
                (name.clone(), key),
                record.id,
            )?,
            // Mirrors `StateIndex::build`: arrays/maps never equal a scalar
            // literal, any other failure marks the index incomplete.
            None if !matches!(value, Value::Array(_) | Value::Map(_)) => {
                index.set_incomplete();
            }
            None => {}
        }
    }
    Some(())
}

fn add_object(index: &mut StateIndex, delta: &mut IndexDelta, record: &ObjectRecord) {
    for (name, value) in &record.attrs {
        if !matches!(value, Value::Null) {
            add_id(
                &mut index.objects_with_attr,
                &mut delta.with_attr,
                name.clone(),
                record.id,
            );
        }
        match eq_key(value) {
            Some(key) => add_id(
                &mut index.objects_by_attr_eq,
                &mut delta.attr_eq,
                (name.clone(), key),
                record.id,
            ),
            None if !matches!(value, Value::Array(_) | Value::Map(_)) => {
                index.set_incomplete();
            }
            None => {}
        }
    }
}

fn remove_relation(
    index: &mut StateIndex,
    delta: &mut IndexDelta,
    record: &RelationRecord,
) -> Option<()> {
    let encoded = relation_bytes(record).ok()?;
    let key = (record.subject, record.predicate.clone());
    let row = index.relations_from.get_mut(&key)?;
    let position = row
        .iter()
        .position(|edge| target_bytes(edge).ok().as_deref() == Some(encoded.as_slice()))?;
    let change = delta
        .relations
        .entry(key.clone())
        .or_insert_with(|| RowChange {
            base_present: true,
            elements: BTreeMap::new(),
        });
    change.elements.entry(encoded.clone()).or_insert(true);
    row.remove(position);
    if row.is_empty() {
        index.relations_from.remove(&key);
    }
    if let Some(text) = tag_text(record) {
        // The tag set is a set of subjects: the subject leaves it only when no
        // relation on this (subject, `tagged_as`) row still names this target.
        let remaining = index
            .relations_from
            .get(&key)
            .map(|edges| {
                edges
                    .iter()
                    .any(|edge| target_bytes(edge).ok().as_deref() == Some(encoded.as_slice()))
            })
            .unwrap_or(false);
        if !remaining {
            remove_id(
                &mut index.tags,
                &mut delta.tags,
                text.to_owned(),
                record.subject,
            )?;
        }
    }
    Some(())
}

fn add_relation(
    index: &mut StateIndex,
    delta: &mut IndexDelta,
    record: &RelationRecord,
) -> Option<()> {
    let encoded = relation_bytes(record).ok()?;
    let key = (record.subject, record.predicate.clone());
    let existed = index.relations_from.contains_key(&key);
    let change = delta
        .relations
        .entry(key.clone())
        .or_insert_with(|| RowChange {
            base_present: existed,
            elements: BTreeMap::new(),
        });
    let row = index.relations_from.entry(key).or_default();
    let present = row
        .iter()
        .any(|edge| target_bytes(edge).ok().as_deref() == Some(encoded.as_slice()));
    change.elements.entry(encoded).or_insert(present);
    row.push(IndexedRelation {
        kind: record.kind,
        target: record.target.clone(),
    });
    if let Some(text) = tag_text(record) {
        add_id(
            &mut index.tags,
            &mut delta.tags,
            text.to_owned(),
            record.subject,
        );
    }
    Some(())
}

/// The canonical bytes of one relation edge (`kind` + target), the key the
/// index uses for relation-row membership.
fn relation_bytes(record: &RelationRecord) -> Result<Vec<u8>> {
    target_bytes(&IndexedRelation {
        kind: record.kind,
        target: record.target.clone(),
    })
}

/// The tag text a `tagged_as` value relation carries, exactly as
/// `StateIndex::build` recognises it.
fn tag_text(record: &RelationRecord) -> Option<&str> {
    if record.kind != RelationKind::Value || record.predicate != "tagged_as" {
        return None;
    }
    match &record.target {
        RelationTarget::Value(Value::Str(text)) => Some(text),
        _ => None,
    }
}

fn id_key(id: Hash32) -> Vec<u8> {
    id.as_bytes().to_vec()
}

fn remove_id<K: Ord + Clone + Eq + std::hash::Hash>(
    map: &mut HashMap<K, BTreeSet<Hash32>>,
    delta: &mut BTreeMap<K, RowChange>,
    key: K,
    id: Hash32,
) -> Option<()> {
    let row = map.get_mut(&key)?;
    let change = delta.entry(key.clone()).or_insert_with(|| RowChange {
        base_present: true,
        elements: BTreeMap::new(),
    });
    change.elements.entry(id_key(id)).or_insert(true);
    if !row.remove(&id) {
        return None;
    }
    if row.is_empty() {
        map.remove(&key);
    }
    Some(())
}

fn add_id<K: Ord + Clone + Eq + std::hash::Hash>(
    map: &mut HashMap<K, BTreeSet<Hash32>>,
    delta: &mut BTreeMap<K, RowChange>,
    key: K,
    id: Hash32,
) {
    let row_existed = map.contains_key(&key);
    let change = delta.entry(key.clone()).or_insert_with(|| RowChange {
        base_present: row_existed,
        elements: BTreeMap::new(),
    });
    let row = map.entry(key).or_default();
    change
        .elements
        .entry(id_key(id))
        .or_insert(row.contains(&id));
    row.insert(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{EntryRecord, ObjectKind};

    fn state_with(records: &[ObjectRecord]) -> State {
        let mut state = State::default();
        state.generation = 2;
        for record in records {
            state.objects.insert(record.id, record.clone());
        }
        state
    }

    fn object(seed: u8, attrs: &[(&str, Value)]) -> ObjectRecord {
        let mut id = [0u8; 32];
        id[0] = seed;
        ObjectRecord {
            id: Hash32::from_bytes(id),
            kind: ObjectKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime_ns: 0,
            version: Hash32::ZERO,
            content: Hash32::ZERO,
            size: 0,
            attrs: attrs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        }
    }

    /// The incremental result and the oracle must encode identically, including
    /// the row order and element membership.
    fn assert_matches_oracle(start: State, ops: Vec<Op>, end: State) {
        let base = StateIndex::build(&start);
        let pre = PreImages::capture(&start, &ops);
        let (incremental, _) = apply(base, &end, &ops, &pre).expect("maintenance");
        let oracle = StateIndex::build(&end);
        assert_eq!(
            incremental.encode_sections().unwrap(),
            oracle.encode_sections().unwrap(),
            "incremental index differs from the rebuild oracle"
        );
    }

    #[test]
    fn a_pure_attribute_edit_moves_exactly_one_row() {
        let a = object(1, &[("team", Value::Str("red".into()))]);
        let mut start = state_with(std::slice::from_ref(&a));
        start.generation = 1;
        let mut end = start.clone();
        let mut edited = a.clone();
        edited
            .attrs
            .insert("team".to_owned(), Value::Str("blue".into()));
        end.objects.insert(edited.id, edited.clone());
        end.generation = 2;
        assert_matches_oracle(start, vec![Op::PutObject(edited)], end);
    }

    #[test]
    fn a_deleted_object_leaves_no_rows() {
        let a = object(1, &[("team", Value::Str("red".into()))]);
        let mut start = state_with(std::slice::from_ref(&a));
        start.generation = 1;
        let mut end = start.clone();
        end.objects.clear();
        end.generation = 2;
        assert_matches_oracle(start, vec![Op::DeleteObject(a.id)], end);
    }

    #[test]
    fn entries_do_not_touch_rows() {
        let a = object(1, &[]);
        let mut start = state_with(std::slice::from_ref(&a));
        start.generation = 1;
        let mut end = start.clone();
        let entry = EntryRecord {
            id: Hash32::from_bytes([9u8; 32]),
            parent: a.id,
            name: b"x".to_vec(),
            target: a.id,
        };
        end.entries.insert(entry.id, entry.clone());
        end.generation = 2;
        assert_matches_oracle(start, vec![Op::PutEntry(entry)], end);
    }

    fn relation(seed: u8, subject: Hash32, predicate: &str, text: &str) -> RelationRecord {
        let target = RelationTarget::Value(Value::Str(text.to_owned()));
        let provenance: Option<BTreeMap<String, Value>> = None;
        // `seed` distinguishes otherwise-identical assertions through attrs, so
        // two tag edges on one subject resolve to two relation ids.
        let mut attrs = BTreeMap::new();
        if seed != 0 {
            attrs.insert("mark".to_owned(), Value::U64(seed as u64));
        }
        let id = crate::state::derive_relation_id(
            subject,
            predicate,
            RelationKind::Value,
            &target,
            &provenance,
            &attrs,
        )
        .unwrap();
        RelationRecord {
            id,
            subject,
            predicate: predicate.to_owned(),
            kind: RelationKind::Value,
            target,
            provenance,
            attrs,
        }
    }

    /// A duplicate tag edge: deleting one of two `tagged_as` assertions on the
    /// same subject must leave the subject in the tag set (the oracle does).
    #[test]
    fn removing_one_of_two_identical_tag_edges_keeps_the_tag() {
        let a = object(1, &[]);
        let first = relation(0, a.id, "tagged_as", "alpha");
        let second = relation(1, a.id, "tagged_as", "alpha");
        let mut start = state_with(std::slice::from_ref(&a));
        start.generation = 1;
        start.relations.insert(first.id, first.clone());
        start.relations.insert(second.id, second.clone());
        let mut end = start.clone();
        end.generation = 2;
        end.relations.remove(&first.id);
        assert_matches_oracle(start, vec![Op::DelRelation(first.id)], end);
    }

    /// The operation-derived delta must be byte-identical to the whole-state
    /// diff for the same index: the optimisation may not change the wire bytes.
    #[test]
    fn the_derived_delta_equals_the_full_diff_delta() {
        let a = object(
            1,
            &[("team", Value::Str("red".into())), ("score", Value::U64(3))],
        );
        let b = object(2, &[("team", Value::Str("blue".into()))]);
        let mut start = state_with(&[a.clone(), b.clone()]);
        start.generation = 1;
        let mut edited = a.clone();
        edited
            .attrs
            .insert("team".to_owned(), Value::Str("green".into()));
        edited.attrs.remove("score");
        let tag = relation(0, b.id, "tagged_as", "alpha");
        let mut end = start.clone();
        end.objects.insert(edited.id, edited.clone());
        end.relations.insert(tag.id, tag.clone());
        end.generation = 2;
        let ops = vec![Op::PutObject(edited), Op::PutRelation(tag)];

        let base = StateIndex::build(&start);
        let pre = PreImages::capture(&start, &ops);
        let (incremental, delta) = apply(base.clone(), &end, &ops, &pre).expect("maintenance");
        let base_id = Hash32::from_bytes([7u8; 32]);
        let base_commit = Hash32::from_bytes([8u8; 32]);
        let derived =
            crate::index_chain::encode_delta_changed(&delta, &incremental, base_id, base_commit)
                .unwrap();
        let full_diff =
            crate::index_chain::encode_delta(&base, &incremental, base_id, base_commit).unwrap();
        assert_eq!(derived, full_diff, "delta bytes must not change");
    }
}
