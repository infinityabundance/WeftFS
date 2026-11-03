//! Selector evaluation (`WEFT-SELECTORS.md` §3).
//!
//! Evaluation order is normative:
//! 1. compute the authorized universe first;
//! 2. evaluate predicates (absent/mistyped comparisons are false; `attr_ne` is
//!    defined only when both sides exist; `not` complements inside the
//!    authorized universe);
//! 3. order by the order list with an unconditional `object_id` ascending
//!    tie-break;
//! 4. apply offset, then limit.
//!
//! `selector_id` is always the query hash; `snapshot_id` is the pinned commit
//! hash; neither is derived from the result set.

use std::cmp::Ordering;

use weftcodec::selector::{CmpOp, Expr, Field, Literal, OrderItem, Selector};
use weftcodec::wcb::Value;

use crate::auth::{can_read, Principal};
use crate::error::{Error, Result};
use crate::identity::Hash32;
use crate::state::{ObjectKind, ObjectRecord, RelationKind, RelationTarget, State};
use crate::store::Store;

/// A pinned reference returned by resolution and `pin`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRef {
    pub namespace: String,
    pub snapshot: Hash32,
    pub object_id: Hash32,
    pub version_id: Hash32,
    pub content_id: Hash32,
}

/// One piece of positive evidence for a match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceEntry {
    /// `attr`, `relation`, or `tag`.
    pub kind: String,
    /// Field path or predicate name.
    pub field: String,
    /// Actual value (attribute value, target object bytes, or tag text).
    pub value: Option<Value>,
}

/// Evidence attached to one matched object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub object_id: Hash32,
    pub entries: Vec<EvidenceEntry>,
}

/// The full resolution result.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub selector_id: Hash32,
    pub snapshot_id: Hash32,
    pub results: Vec<ResolvedRef>,
    pub matched_count: u64,
    pub evidence: Vec<Evidence>,
}

/// Resolve `name-or-hex` to a pinned commit hash. Names are looked up in the
/// current state's snapshot map.
pub fn lookup_snapshot(store: &Store, name_or_hash: &str) -> Result<Hash32> {
    if let Ok(hash) = Hash32::from_hex(name_or_hash) {
        return Ok(hash);
    }
    store
        .current_state()
        .snapshots
        .get(name_or_hash)
        .copied()
        .ok_or_else(|| Error::not_found(format!("no snapshot named `{name_or_hash}`")))
}

pub(crate) fn resolve(
    store: &Store,
    selector: &Selector,
    snapshot: Option<Hash32>,
    principal: &Principal,
    limits: &weftcodec::Limits,
) -> Result<Resolution> {
    if selector.namespace != crate::state::DEFAULT_NAMESPACE {
        return Err(Error::namespace_mismatch(format!(
            "selector namespace `{}` is not `{}`",
            selector.namespace,
            crate::state::DEFAULT_NAMESPACE
        )));
    }
    let snapshot_id = snapshot.unwrap_or_else(|| store.snapshot_root());
    let owned_state;
    let state: &State = if snapshot_id == store.snapshot_root() {
        store.current_state()
    } else {
        owned_state = store.state_at(snapshot_id)?;
        &owned_state
    };
    resolve_state(selector, snapshot_id, state, principal, limits)
}

/// Evaluate a selector against an explicit replayed state (used by the FUSE
/// frontend for a mounted snapshot, avoiding a replay per lookup).
pub(crate) fn resolve_state(
    selector: &Selector,
    snapshot_id: Hash32,
    state: &State,
    principal: &Principal,
    limits: &weftcodec::Limits,
) -> Result<Resolution> {
    if selector.namespace != crate::state::DEFAULT_NAMESPACE {
        return Err(Error::namespace_mismatch(format!(
            "selector namespace `{}` is not `{}`",
            selector.namespace,
            crate::state::DEFAULT_NAMESPACE
        )));
    }
    let root = state
        .objects
        .get(&State::ROOT_ID)
        .ok_or_else(|| Error::corrupt("namespace root is missing"))?;
    if !can_read(root, principal) {
        return Err(Error::unauthorized(
            "principal cannot read the namespace root",
        ));
    }
    let selector_id = selector.selector_id_with_limits(limits)?;

    let mut matches: Vec<(ObjectRecord, Vec<EvidenceEntry>)> = Vec::new();
    for record in state.objects.values() {
        if !can_read(record, principal) {
            continue;
        }
        if let Some(evidence) = eval_expr(&selector.predicate, record, state, principal) {
            matches.push((record.clone(), evidence));
        }
    }
    let matched_count = matches.len() as u64;

    matches.sort_by(|a, b| compare_records(&a.0, &b.0, &selector.order, state));

    let offset = selector.offset.unwrap_or(0);
    let limit = selector.limit.unwrap_or(u64::MAX);
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(matches.len());
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    let end = start.saturating_add(take).min(matches.len());

    let mut results = Vec::with_capacity(end.saturating_sub(start));
    let mut evidence = Vec::with_capacity(end.saturating_sub(start));
    for (record, entries) in &matches[start..end] {
        results.push(ResolvedRef {
            namespace: selector.namespace.clone(),
            snapshot: snapshot_id,
            object_id: record.id,
            version_id: record.version,
            content_id: record.content,
        });
        evidence.push(Evidence {
            object_id: record.id,
            entries: entries.clone(),
        });
    }

    Ok(Resolution {
        selector_id,
        snapshot_id,
        results,
        matched_count,
        evidence,
    })
}

pub(crate) fn require_one(
    store: &Store,
    selector: &Selector,
    snapshot: Option<Hash32>,
    principal: &Principal,
    limits: &weftcodec::Limits,
) -> Result<ResolvedRef> {
    let resolution = resolve(store, selector, snapshot, principal, limits)?;
    match resolution.matched_count {
        0 => Err(Error::not_found("selector matched no authorized objects")),
        1 => {
            if let Some(result) = resolution.results.first() {
                return Ok(result.clone());
            }
            // The only match was hidden by offset/limit; re-evaluate without
            // pagination to return it. Identity is unaffected for the caller.
            let mut unpaged = selector.clone();
            unpaged.offset = None;
            unpaged.limit = None;
            let again = resolve(store, &unpaged, snapshot, principal, limits)?;
            again
                .results
                .into_iter()
                .next()
                .ok_or_else(|| Error::not_found("selector matched no authorized objects"))
        }
        count => Err(Error::ambiguous(format!(
            "selector matched {count} authorized objects; require_one needs exactly 1"
        ))),
    }
}

fn eval_expr(
    expr: &Expr,
    object: &ObjectRecord,
    state: &State,
    principal: &Principal,
) -> Option<Vec<EvidenceEntry>> {
    match expr {
        Expr::True => Some(Vec::new()),
        Expr::False => None,
        Expr::Has(field) => match resolve_field(object, field) {
            Some(Value::Null) | None => None,
            Some(value) => Some(vec![EvidenceEntry {
                kind: "attr".to_owned(),
                field: field.as_string(),
                value: Some(value),
            }]),
        },
        Expr::Tag(name) => {
            let mut evidence = Vec::new();
            for relation in state.relations.values() {
                if relation.subject != object.id || relation.predicate != "tagged_as" {
                    continue;
                }
                let RelationTarget::Value(value) = &relation.target else {
                    continue;
                };
                if relation.kind != RelationKind::Value {
                    continue;
                }
                if let Value::Str(text) = value {
                    if text.as_str() == name.as_str() {
                        evidence.push(EvidenceEntry {
                            kind: "tag".to_owned(),
                            field: "tagged_as".to_owned(),
                            value: Some(value.clone()),
                        });
                    }
                }
            }
            if evidence.is_empty() {
                None
            } else {
                Some(evidence)
            }
        }
        Expr::Relation { predicate, target } => {
            // v1 is one-hop. An object-target edge is visible only when the
            // target object exists and the principal may read it.
            let target_visible = state
                .objects
                .get(target)
                .map(|record| can_read(record, principal))
                .unwrap_or(false);
            if !target_visible {
                return None;
            }
            let matched = state.relations.values().any(|relation| {
                relation.subject == object.id
                    && relation.predicate == *predicate
                    && relation.kind == RelationKind::Object
                    && matches!(
                        &relation.target,
                        RelationTarget::Object(hash) if hash == target
                    )
            });
            if matched {
                Some(vec![EvidenceEntry {
                    kind: "relation".to_owned(),
                    field: predicate.clone(),
                    value: Some(Value::Bytes(target.as_bytes().to_vec())),
                }])
            } else {
                None
            }
        }
        Expr::Compare { field, op, value } => {
            let left = resolve_field(object, field);
            if compare(&left, *op, &literal_value(value)) {
                Some(vec![EvidenceEntry {
                    kind: "attr".to_owned(),
                    field: field.as_string(),
                    value: left,
                }])
            } else {
                None
            }
        }
        Expr::And(operands) => {
            let mut evidence = Vec::new();
            for operand in operands {
                let part = eval_expr(operand, object, state, principal)?;
                evidence.extend(part);
            }
            Some(evidence)
        }
        Expr::Or(operands) => {
            let mut evidence = Vec::new();
            let mut any = false;
            for operand in operands {
                if let Some(part) = eval_expr(operand, object, state, principal) {
                    any = true;
                    evidence.extend(part);
                }
            }
            if any {
                Some(evidence)
            } else {
                None
            }
        }
        Expr::Not(inner) => {
            if eval_expr(inner, object, state, principal).is_some() {
                None
            } else {
                Some(Vec::new())
            }
        }
    }
}

fn literal_value(literal: &Literal) -> Value {
    match literal {
        Literal::Null => Value::Null,
        Literal::Bool(value) => Value::Bool(*value),
        Literal::U64(value) => Value::U64(*value),
        Literal::I64(value) => Value::I64(*value),
        Literal::Str(value) => Value::Str(value.clone()),
    }
}

/// Resolve a v1 root field on an object record. Unknown paths are absent
/// (closed world), never errors.
fn resolve_field(object: &ObjectRecord, field: &Field) -> Option<Value> {
    let segments = &field.segments;
    let first = segments.first()?.as_str();
    match first {
        "object_id" if segments.len() == 1 => Some(Value::Str(object.id.to_hex())),
        "content_id" if segments.len() == 1 => Some(Value::Str(object.content.to_hex())),
        "namespace" if segments.len() == 1 => {
            Some(Value::Str(crate::state::DEFAULT_NAMESPACE.to_owned()))
        }
        "media_type" if segments.len() == 1 => object
            .attrs
            .get("media_type")
            .or_else(|| object.attrs.get("weft.media_type"))
            .cloned(),
        "size" if segments.len() == 1 => Some(Value::U64(object.size)),
        "mtime" if segments.len() == 1 => Some(Value::Timestamp(object.mtime_ns)),
        "kind" if segments.len() == 1 => Some(Value::Str(object.kind.as_str().to_owned())),
        "attrs" if segments.len() > 1 => {
            let key = segments[1..].join(".");
            object.attrs.get(&key).cloned()
        }
        _ => None,
    }
}

fn numeric(value: &Value) -> Option<i128> {
    match value {
        Value::U64(number) => Some(*number as i128),
        Value::I64(number) => Some(*number as i128),
        Value::Timestamp(number) => Some(*number as i128),
        _ => None,
    }
}

/// Equality semantics: numeric families compare by value; other types compare
/// only with the same type. Mistyped comparisons are false.
fn equal_values(left: &Value, right: &Value) -> bool {
    if let (Some(a), Some(b)) = (numeric(left), numeric(right)) {
        return a == b;
    }
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Bytes(a), Value::Bytes(b)) => a == b,
        _ => false,
    }
}

fn comparable_order(left: &Value, right: &Value) -> Option<Ordering> {
    if let (Some(a), Some(b)) = (numeric(left), numeric(right)) {
        return Some(a.cmp(&b));
    }
    match (left, right) {
        (Value::Str(a), Value::Str(b)) => Some(a.as_bytes().cmp(b.as_bytes())),
        (Value::Bytes(a), Value::Bytes(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        _ => None,
    }
}

fn compare(left: &Option<Value>, op: CmpOp, right: &Value) -> bool {
    let Some(left) = left else {
        // Absent fields make every comparison false, including `!=`.
        return false;
    };
    match op {
        CmpOp::Eq => equal_values(left, right),
        CmpOp::Ne => comparable_order(left, right)
            .map(|ordering| ordering != Ordering::Equal)
            .unwrap_or(false),
        CmpOp::Lt => comparable_order(left, right) == Some(Ordering::Less),
        CmpOp::Le => matches!(
            comparable_order(left, right),
            Some(Ordering::Less | Ordering::Equal)
        ),
        CmpOp::Gt => comparable_order(left, right) == Some(Ordering::Greater),
        CmpOp::Ge => matches!(
            comparable_order(left, right),
            Some(Ordering::Greater | Ordering::Equal)
        ),
    }
}

fn value_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::U64(_) | Value::I64(_) | Value::Timestamp(_) => 2,
        Value::Str(_) => 3,
        Value::Bytes(_) => 4,
        Value::Array(_) => 5,
        Value::Map(_) => 6,
    }
}

/// Total, deterministic value order for result ordering. Missing values sort
/// before present values in ascending order (and after them when the order item
/// is descending, because the whole comparison is reversed).
fn total_value_order(left: &Value, right: &Value) -> Ordering {
    if let Some(ordering) = comparable_order(left, right) {
        return ordering;
    }
    value_rank(left).cmp(&value_rank(right))
}

fn compare_optional(left: &Option<Value>, right: &Option<Value>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => total_value_order(a, b),
    }
}

fn compare_records(
    left: &ObjectRecord,
    right: &ObjectRecord,
    order: &[OrderItem],
    state: &State,
) -> Ordering {
    let _ = state;
    for item in order {
        let a = resolve_field(left, &item.field);
        let b = resolve_field(right, &item.field);
        let mut ordering = compare_optional(&a, &b);
        if item.desc {
            ordering = ordering.reverse();
        }
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    // Unconditional deterministic tie-break: object_id ascending.
    left.id.cmp(&right.id)
}

/// True when the object kind is a regular file (used by tests/CLI helpers).
pub fn is_file(record: &ObjectRecord) -> bool {
    record.kind == ObjectKind::File
}
