//! Causal closure resolution over artifact identities (P02).
//!
//! Normative description: `spec/WEFT-CAUSAL.md` §4 and
//! `spec/WEFT-TRAINING-ARTIFACT.md` §5.
//!
//! A **causal graph** is a directed graph whose nodes are artifact identities
//! (a `VersionId`, a `CausalClosureId`, a `ContentId` or a
//! `TrainingArtifactRoot`) and whose edges are the `weft.causal.parents` an
//! object declares. The record's `weft.causal.closure` array is a *declared*
//! transitive set, not an edge list. This module keeps those two things
//! distinct:
//!
//! * [`ClosureMode::Complete`] follows both `parents` and declared `closure`
//!   entries; it is the union of every declared ancestor.
//! * [`ClosureMode::Minimal`] follows `parents` edges only, from the roots the
//!   caller actually needs. It is a subset of `Complete` for the same roots
//!   when `closure` is a superset of `parents` (which the format requires).
//!
//! Both modes are bounded by an edge-hop depth and a node count, both produce
//! nodes in ascending identity order (depth is reported, never used to order),
//! and both **refuse** on an ancestor they cannot resolve. There is no
//! "truncate to what we could see" path and no silent omission: a closure that
//! drops a declared parent is a correctness failure, not an optimization.
//!
//! # Non-disclosure
//!
//! An identity that is absent and an identity the principal may not see produce
//! the *same* class and the *same* text. The caller supplies a `visible`
//! predicate; this module never learns why an identity is invisible.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::state::{ObjectRecord, State};
use crate::wcb_util as wu;

/// Causal identity domain (`WEFT-CAUSAL.md` §1.2).
pub const CAUSAL_DOMAIN: &str = "weft/causal/v1";
/// Schema string of the causal closure record.
pub const CAUSAL_SCHEMA: &str = "weft.causal.v1";
/// Attribute keys the causal record is stored under.
pub const ATTR_PARENTS: &str = "weft.causal.parents";
pub const ATTR_CLOSURE: &str = "weft.causal.closure";
pub const ATTR_HASH: &str = "weft.causal.hash";
pub const ATTR_IDENTITY_MODE: &str = "weft.identity.mode";

/// Default depth bound: edge hops from a root.
pub const DEFAULT_MAX_DEPTH: u32 = 64;
/// Default node bound.
pub const DEFAULT_MAX_NODES: usize = 65_536;

/// Which closure definition to compute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClosureMode {
    /// Every declared ancestor, following `parents` and declared `closure`.
    Complete,
    /// Only the parent edges reachable from the requested roots.
    Minimal,
}

impl ClosureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ClosureMode::Complete => "complete",
            ClosureMode::Minimal => "minimal",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "complete" => Ok(ClosureMode::Complete),
            "minimal" => Ok(ClosureMode::Minimal),
            other => Err(Error::invalid_argument(format!(
                "closure mode `{other}` is not `complete` or `minimal`"
            ))),
        }
    }
}

/// Bounds on one closure query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClosureBounds {
    pub max_depth: u32,
    pub max_nodes: usize,
}

impl Default for ClosureBounds {
    fn default() -> Self {
        ClosureBounds {
            max_depth: DEFAULT_MAX_DEPTH,
            max_nodes: DEFAULT_MAX_NODES,
        }
    }
}

impl ClosureBounds {
    pub fn validate(&self) -> Result<()> {
        if self.max_depth == 0 {
            return Err(Error::limit_exceeded("closure max_depth must be positive"));
        }
        if self.max_nodes == 0 {
            return Err(Error::limit_exceeded("closure max_nodes must be positive"));
        }
        Ok(())
    }
}

/// One node of the causal graph as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNode {
    /// Object id when the identity is a version present in this state.
    pub object: Option<Hash32>,
    /// Direct outgoing edges (`weft.causal.parents`).
    pub parents: Vec<Hash32>,
    /// The declared transitive set (`weft.causal.closure`).
    pub closure: Vec<Hash32>,
}

/// One resolved closure node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosureNode {
    pub identity: Hash32,
    pub object: Option<Hash32>,
    /// Shortest edge-hop distance from any root. A root itself is depth 0.
    pub depth: u32,
    /// Direct declared parents, canonical order.
    pub parents: Vec<Hash32>,
    /// Declared closure entries, canonical order.
    pub declared_closure: Vec<Hash32>,
}

/// The result of one closure query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosureResult {
    pub mode: ClosureMode,
    pub roots: Vec<Hash32>,
    /// Ascending by identity bytes.
    pub nodes: Vec<ClosureNode>,
    /// Authorized closure size before any budget filter.
    pub total_closure_size: usize,
    /// Entries actually returned.
    pub returned_closure_size: usize,
    /// True if a budget prevented returning the full authorized set. v1
    /// refuses instead of truncating, so this is always false when `Ok`.
    pub truncated: bool,
    pub max_depth_seen: u32,
    /// Edges inspected, for cost evidence.
    pub edges_considered: u64,
}

impl ClosureResult {
    pub fn identities(&self) -> Vec<Hash32> {
        self.nodes.iter().map(|node| node.identity).collect()
    }
}

/// An indexed causal graph for one state.
#[derive(Clone, Debug, Default)]
pub struct CausalGraph {
    nodes: BTreeMap<Hash32, GraphNode>,
}

/// Render the canonical `weft.causal.v1` record for two arrays.
pub fn causal_record_value(parents: &[Hash32], closure: &[Hash32]) -> Value {
    wu::map([
        ("schema", Value::Str(CAUSAL_SCHEMA.to_owned())),
        (
            "parents",
            Value::Array(parents.iter().map(|hash| wu::hash_value(*hash)).collect()),
        ),
        (
            "closure",
            Value::Array(closure.iter().map(|hash| wu::hash_value(*hash)).collect()),
        ),
    ])
}

/// `CausalClosureId = H("weft/causal/v1", WCB({schema, parents, closure}))`.
pub fn causal_id(parents: &[Hash32], closure: &[Hash32]) -> Result<Hash32> {
    Ok(identity::h(
        CAUSAL_DOMAIN,
        &wu::encode(&causal_record_value(parents, closure))?,
    ))
}

fn hash_array(value: &Value, what: &str) -> Result<Vec<Hash32>> {
    let items = match value {
        Value::Array(items) => items,
        _ => return Err(Error::malformed(format!("{what} must be an array"))),
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::Bytes(bytes) => out.push(Hash32::from_slice(bytes).map_err(Error::from)?),
            _ => return Err(Error::malformed(format!("{what} entries must be bytes32"))),
        }
    }
    Ok(out)
}

/// Validate a canonical closure array pair (`WEFT-CAUSAL.md` §1.1).
pub fn validate_causal_arrays(parents: &[Hash32], closure: &[Hash32], max: usize) -> Result<()> {
    for (label, array) in [("parents", parents), ("closure", closure)] {
        if array.len() > max {
            return Err(Error::limit_exceeded(format!(
                "causal {label} exceeds max_causal_entries ({max})"
            )));
        }
        let mut previous: Option<Hash32> = None;
        for entry in array {
            if *entry == Hash32::ZERO {
                return Err(Error::non_canonical(format!(
                    "causal {label} contains the zero hash"
                )));
            }
            if let Some(previous) = previous {
                if *entry <= previous {
                    return Err(Error::non_canonical(format!(
                        "causal {label} is not strictly ascending"
                    )));
                }
            }
            previous = Some(*entry);
        }
    }
    let set: BTreeSet<Hash32> = closure.iter().copied().collect();
    for parent in parents {
        if !set.contains(parent) {
            return Err(Error::non_canonical(
                "causal closure does not contain every parent",
            ));
        }
    }
    Ok(())
}

fn causal_arrays(
    record: &ObjectRecord,
    limits: &Limits,
) -> Result<Option<(Vec<Hash32>, Vec<Hash32>)>> {
    let parents = match record.attrs.get(ATTR_PARENTS) {
        Some(value) => hash_array(value, "weft.causal.parents")?,
        None => return Ok(None),
    };
    let closure = match record.attrs.get(ATTR_CLOSURE) {
        Some(value) => hash_array(value, "weft.causal.closure")?,
        None => {
            return Err(Error::corrupt(
                "weft.causal.parents is present without weft.causal.closure",
            ))
        }
    };
    validate_causal_arrays(
        &parents,
        &closure,
        usize::try_from(limits.max_causal_entries).unwrap_or(usize::MAX),
    )?;
    let expected = causal_id(&parents, &closure)?;
    match record.attrs.get(ATTR_HASH) {
        Some(Value::Bytes(bytes)) => {
            let recorded = Hash32::from_slice(bytes).map_err(Error::from)?;
            if recorded != expected {
                return Err(Error::corrupt(format!(
                    "object {} records a causal hash that is not its closure record",
                    record.id.to_hex()
                )));
            }
        }
        Some(_) => {
            return Err(Error::corrupt(
                "weft.causal.hash must be bytes32 when present",
            ))
        }
        None => {
            if matches!(
                record.attrs.get(ATTR_IDENTITY_MODE),
                Some(Value::Str(mode)) if mode == "causal_exact"
            ) {
                return Err(Error::corrupt(
                    "causal_exact identity is missing weft.causal.hash",
                ));
            }
        }
    }
    Ok(Some((parents, closure)))
}

impl CausalGraph {
    /// Index every object's causal record in one pass. Every object identity
    /// is a node, even when it declares no ancestry (its edge set is empty).
    pub fn build(state: &State, limits: &Limits) -> Result<CausalGraph> {
        let mut nodes = BTreeMap::new();
        for record in state.objects.values() {
            let (parents, closure) = match causal_arrays(record, limits)? {
                Some((parents, closure)) => (parents, closure),
                None => (Vec::new(), Vec::new()),
            };
            nodes.entry(record.version).or_insert(GraphNode {
                object: Some(record.id),
                parents,
                closure,
            });
        }
        Ok(CausalGraph { nodes })
    }

    /// Build from explicit records (used by tests and the court harness).
    pub fn from_nodes(nodes: impl IntoIterator<Item = (Hash32, GraphNode)>) -> CausalGraph {
        CausalGraph {
            nodes: nodes.into_iter().collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, identity: Hash32) -> Option<&GraphNode> {
        self.nodes.get(&identity)
    }

    fn require(&self, identity: Hash32, named_by: Option<Hash32>) -> Result<&GraphNode> {
        match self.nodes.get(&identity) {
            Some(node) => Ok(node),
            None => Err(missing_ancestor(identity, named_by)),
        }
    }

    /// Compute a bounded closure.
    ///
    /// `visible` decides whether an identity may be disclosed. An invisible
    /// identity and an absent one produce the same refusal.
    pub fn compute(
        &self,
        mode: ClosureMode,
        roots: &[Hash32],
        bounds: ClosureBounds,
        visible: &dyn Fn(Hash32) -> bool,
    ) -> Result<ClosureResult> {
        bounds.validate()?;
        let mut sorted_roots: Vec<Hash32> = roots.to_vec();
        sorted_roots.sort_unstable();
        sorted_roots.dedup();
        for root in &sorted_roots {
            if *root == Hash32::ZERO {
                return Err(Error::non_canonical("closure root must not be zero"));
            }
        }

        let mut depth_of: BTreeMap<Hash32, u32> = BTreeMap::new();
        let mut queue: VecDeque<(Hash32, u32, Option<Hash32>)> = VecDeque::new();
        for root in &sorted_roots {
            queue.push_back((*root, 0, None));
        }
        let mut edges_considered: u64 = 0;

        while let Some((identity, depth, named_by)) = queue.pop_front() {
            if !visible(identity) {
                return Err(missing_ancestor(identity, named_by));
            }
            if let Some(seen) = depth_of.get(&identity) {
                if *seen <= depth {
                    continue;
                }
            }
            let node = self.require(identity, named_by)?;
            if depth_of.len() >= bounds.max_nodes && !depth_of.contains_key(&identity) {
                return Err(Error::limit_exceeded(format!(
                    "causal closure exceeds max_nodes ({})",
                    bounds.max_nodes
                )));
            }
            depth_of.insert(identity, depth);
            let edges: Vec<Hash32> = match mode {
                ClosureMode::Complete => {
                    let mut edges = node.parents.clone();
                    edges.extend(node.closure.iter().copied());
                    edges.sort_unstable();
                    edges.dedup();
                    edges
                }
                ClosureMode::Minimal => node.parents.clone(),
            };
            edges_considered += edges.len() as u64;
            if !edges.is_empty() && depth >= bounds.max_depth {
                return Err(Error::limit_exceeded(format!(
                    "causal closure depth exceeds max_depth ({}) at ancestor {}",
                    bounds.max_depth,
                    identity.to_hex()
                )));
            }
            for edge in edges {
                queue.push_back((edge, depth + 1, Some(identity)));
            }
        }

        let mut nodes = Vec::with_capacity(depth_of.len());
        let mut max_depth_seen = 0;
        for (identity, depth) in &depth_of {
            let node = self.require(*identity, None)?;
            max_depth_seen = max_depth_seen.max(*depth);
            nodes.push(ClosureNode {
                identity: *identity,
                object: node.object,
                depth: *depth,
                parents: node.parents.clone(),
                declared_closure: node.closure.clone(),
            });
        }
        let total = nodes.len();
        Ok(ClosureResult {
            mode,
            roots: sorted_roots,
            nodes,
            total_closure_size: total,
            returned_closure_size: total,
            truncated: false,
            max_depth_seen,
            edges_considered,
        })
    }

    /// Diagnostic oracle: rebuild a causal index from a full scan of `state`
    /// for every query, then walk it. Present only so a benchmark can price
    /// the index; it is never used by a serving path.
    pub fn naive_scan(
        state: &State,
        limits: &Limits,
        mode: ClosureMode,
        roots: &[Hash32],
        bounds: ClosureBounds,
    ) -> Result<ClosureResult> {
        let mut by_version: BTreeMap<Hash32, GraphNode> = BTreeMap::new();
        for record in state.objects.values() {
            let (parents, closure) = match causal_arrays(record, limits)? {
                Some((parents, closure)) => (parents, closure),
                None => (Vec::new(), Vec::new()),
            };
            by_version.entry(record.version).or_insert(GraphNode {
                object: Some(record.id),
                parents,
                closure,
            });
        }
        let graph = CausalGraph::from_nodes(by_version);
        graph.compute(mode, roots, bounds, &|_| true)
    }
}

/// The one refusal shape for an ancestor that cannot be resolved.
///
/// It names the identity the caller already supplied and never says whether
/// the identity exists and was denied, or does not exist at all.
pub fn missing_ancestor(identity: Hash32, named_by: Option<Hash32>) -> Error {
    let mut message = format!(
        "causal ancestor {} is not resolvable at this snapshot",
        identity.to_hex()
    );
    if let Some(subject) = named_by {
        message.push_str(&format!(" (declared by {})", subject.to_hex()));
    }
    Error::dependency_mismatch(message)
}

/// True when `identity` names an object present in `state`.
pub fn identity_present(state: &State, identity: Hash32) -> bool {
    state
        .objects
        .values()
        .any(|record| record.version == identity)
}

/// Build a `(version -> object id)` index once for repeated checks.
pub fn identity_index(state: &State) -> BTreeMap<Hash32, Hash32> {
    state
        .objects
        .values()
        .map(|record| (record.version, record.id))
        .collect()
}

/// Classify a closure refusal for CLI reporting without changing its message.
pub fn refusal_class(err: &Error) -> Class {
    err.class()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> Hash32 {
        Hash32::from_bytes([byte; 32])
    }

    fn graph(edges: &[(u8, &[u8], &[u8])]) -> CausalGraph {
        let mut nodes = BTreeMap::new();
        for (id, parents, closure) in edges {
            nodes.insert(
                h(*id),
                GraphNode {
                    object: None,
                    parents: parents.iter().map(|value| h(*value)).collect(),
                    closure: closure.iter().map(|value| h(*value)).collect(),
                },
            );
        }
        CausalGraph::from_nodes(nodes)
    }

    #[test]
    fn complete_follows_declared_closure_minimal_follows_edges() {
        // 1 -> 2, 1 declares {2,3}; 2 -> 3. Minimal from 1 is {1,2,3} through
        // parent edges; complete from 1 also includes the declared 3 directly.
        let g = graph(&[(1, &[2], &[2]), (2, &[3], &[3]), (3, &[], &[])]);
        let all = |_| true;
        let complete = g
            .compute(
                ClosureMode::Complete,
                &[h(1)],
                ClosureBounds::default(),
                &all,
            )
            .unwrap();
        let minimal = g
            .compute(
                ClosureMode::Minimal,
                &[h(1)],
                ClosureBounds::default(),
                &all,
            )
            .unwrap();
        assert_eq!(complete.identities(), vec![h(1), h(2), h(3)]);
        assert_eq!(minimal.identities(), vec![h(1), h(2), h(3)]);
        assert_eq!(complete.max_depth_seen, 2);
    }

    #[test]
    fn declared_closure_entry_without_an_edge_is_only_in_complete() {
        // 1 declares 3 in closure but has no parent edge to it, and 2 is absent.
        let g = graph(&[(1, &[2], &[2, 3]), (3, &[], &[])]);
        let all = |_| true;
        // Minimal refuses because 2 is absent: never silently omit it.
        let err = g
            .compute(
                ClosureMode::Minimal,
                &[h(1)],
                ClosureBounds::default(),
                &all,
            )
            .unwrap_err();
        assert_eq!(err.class(), Class::DependencyMismatch);
        assert!(err.to_string().contains(&h(2).to_hex()));
        // Complete also refuses, for the same identity, with the same class.
        let err = g
            .compute(
                ClosureMode::Complete,
                &[h(1)],
                ClosureBounds::default(),
                &all,
            )
            .unwrap_err();
        assert_eq!(err.class(), Class::DependencyMismatch);
    }

    #[test]
    fn invisible_and_absent_ancestors_are_indistinguishable() {
        let g = graph(&[(1, &[2], &[2]), (2, &[], &[])]);
        let visible = |identity: Hash32| identity != h(2);
        let hidden = g
            .compute(
                ClosureMode::Minimal,
                &[h(1)],
                ClosureBounds::default(),
                &visible,
            )
            .unwrap_err();
        let absent_graph = graph(&[(1, &[2], &[2])]);
        let absent = absent_graph
            .compute(
                ClosureMode::Minimal,
                &[h(1)],
                ClosureBounds::default(),
                &|_| true,
            )
            .unwrap_err();
        assert_eq!(hidden.class(), absent.class());
        assert_eq!(hidden.to_string(), absent.to_string());
    }

    #[test]
    fn depth_and_node_bounds_refuse() {
        let g = graph(&[(1, &[2], &[2]), (2, &[3], &[3]), (3, &[], &[])]);
        let bounds = ClosureBounds {
            max_depth: 1,
            max_nodes: 64,
        };
        let err = g
            .compute(ClosureMode::Minimal, &[h(1)], bounds, &|_| true)
            .unwrap_err();
        assert_eq!(err.class(), Class::LimitExceeded);
        let bounds = ClosureBounds {
            max_depth: 8,
            max_nodes: 2,
        };
        let err = g
            .compute(ClosureMode::Minimal, &[h(1)], bounds, &|_| true)
            .unwrap_err();
        assert_eq!(err.class(), Class::LimitExceeded);
    }

    #[test]
    fn ordering_is_ascending_and_depth_is_reported() {
        let g = graph(&[(1, &[9, 2], &[2, 9]), (2, &[], &[]), (9, &[], &[])]);
        let result = g
            .compute(
                ClosureMode::Minimal,
                &[h(1)],
                ClosureBounds::default(),
                &|_| true,
            )
            .unwrap();
        let ids = result.identities();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
        assert_eq!(result.nodes[0].depth, 0);
        assert_eq!(result.nodes[1].depth, 1);
        assert_eq!(result.nodes[2].depth, 1);
    }
}
