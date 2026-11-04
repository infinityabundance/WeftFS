//! Context Root persistence, capability enforcement, causal closure, and
//! exact materialization (F2).
//!
//! A Context Root is stored as a normal root-subject relation with predicate
//! `context_root`, target = text name, and attrs holding the canonical record
//! hex plus its authoritative `weft://` URI. Resolution verifies the recomputed
//! `ContextRootId`, pins the recorded snapshot, evaluates the selector under the
//! recorded resolver, and applies capability and budget ceilings *before*
//! returning counts or bytes.
//!
//! Capability records are stored as root-subject `capability` relations keyed
//! by their hash. A capability supplied by the caller is an explicit additional
//! narrowing; v1 makes **no cryptographic unforgeability claim**.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use weftcodec::context::{
    Budget, CapabilityScope, CausalClosure, ContextRoot, ContextUri, IdentityMode,
};
use weftcodec::selector::{Expr, Selector};
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::auth::{can_read, Principal};
use crate::error::{Error, Result};
use crate::evaluate::Resolution;
use crate::identity::{self, Hash32};
use crate::recovery::{self, BlobKind};
use crate::state::{
    apply_causal_attrs, object_causal_closure, ObjectKind, ObjectRecord, RelationKind,
    RelationRecord, RelationTarget, State, CAPABILITY_PREDICATE, CAPABILITY_RECORD_ATTR,
    CONTEXT_ROOT_PREDICATE, CONTEXT_ROOT_RECORD_ATTR, CONTEXT_ROOT_URI_ATTR,
};
use crate::store::Store;

static MATERIALIZE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A capability scope plus its content address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityContext {
    pub scope: CapabilityScope,
    pub hash: Hash32,
}

impl CapabilityContext {
    pub fn new(scope: CapabilityScope) -> Result<Self> {
        let hash = scope.id()?;
        if hash == Hash32::ZERO {
            return Err(Error::non_canonical("capability hash must not be zero"));
        }
        Ok(CapabilityContext { scope, hash })
    }

    pub fn allows_namespace(&self, namespace: &str) -> bool {
        self.scope.namespaces.iter().any(|item| item == namespace)
    }

    pub fn allows_root(&self, root: Hash32) -> bool {
        self.scope.roots.contains(&root)
    }

    pub fn allows_predicate(&self, predicate: &str) -> bool {
        self.scope.predicates.iter().any(|item| item == predicate)
    }

    pub fn check_valid_at(&self, now_ns: i64) -> Result<()> {
        if let Some(expires_at) = self.scope.expires_at {
            if now_ns > expires_at {
                // Non-disclosing: expiry is folded into a generic denial.
                return Err(Error::unauthorized("capability is not valid"));
            }
        }
        Ok(())
    }
}

/// The effective budget is the component-wise minimum of the root and
/// capability budgets. `None` means unbounded within store policy.
pub fn effective_budget(
    root: Option<&Budget>,
    capability: Option<&CapabilityContext>,
) -> Option<Budget> {
    let cap_budget = capability.and_then(|cap| cap.scope.budget.as_ref());
    match (root, cap_budget) {
        (None, None) => None,
        (Some(budget), None) => Some(budget.clone()),
        (None, Some(budget)) => Some(budget.clone()),
        (Some(left), Some(right)) => Some(Budget {
            max_results: min_opt(left.max_results, right.max_results),
            max_bytes: min_opt(left.max_bytes, right.max_bytes),
            max_depth: min_opt(left.max_depth, right.max_depth),
            max_work: min_opt(left.max_work, right.max_work),
        }),
    }
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

/// A stored Context Root relation.
#[derive(Clone, Debug)]
pub struct StoredContextRoot {
    pub name: String,
    pub root: ContextRoot,
    pub record_hex: String,
    pub uri: String,
    pub relation_id: Hash32,
    pub context_root_id: Hash32,
}

/// A resolved Context Root.
#[derive(Clone, Debug)]
pub struct ContextResolution {
    pub stored: StoredContextRoot,
    pub selector: Selector,
    pub resolution: Resolution,
    pub effective_budget: Option<Budget>,
    pub capability_hash: Option<Hash32>,
    /// No truncation policy exists in v1: a budget overrun is an error, so a
    /// successful resolution is always complete for its pinned selector.
    pub truncated: bool,
    pub work_units: u64,
}

/// The result of a materialization.
#[derive(Clone, Debug)]
pub struct MaterializationReport {
    pub out: PathBuf,
    pub manifest_path: PathBuf,
    pub files: u64,
    pub bytes: u64,
    pub context_root_id: Hash32,
    pub snapshot_id: Hash32,
    pub selector_id: Hash32,
    pub closure: Vec<Hash32>,
}

#[derive(Clone, Debug)]
struct MaterializeEntry {
    relative: String,
    display_path: String,
    object_id: Hash32,
    version_id: Hash32,
    content_id: Hash32,
    content_hash: Hash32,
    size: u64,
    kind: &'static str,
    bytes: Vec<u8>,
    causal_closure: Vec<Hash32>,
}

// ---------------------------------------------------------------------------
// Causal closure construction
// ---------------------------------------------------------------------------

/// Resolve explicit `--causal` entries to artifact identities and compute the
/// transitive closure available in this state.
///
/// Entries may be ObjectIds (resolved to that object's current VersionId) or
/// literal identity hashes. Stored `weft.causal.closure` attrs are traversed
/// with a visited set, so causal cycles terminate and each identity is counted
/// once. The result is sorted/unique by construction.
pub fn compute_causal_closure(
    state: &State,
    entries: &[Hash32],
    limits: &Limits,
) -> Result<CausalClosure> {
    let max_entries = usize::try_from(limits.max_causal_entries)
        .unwrap_or(usize::MAX)
        .max(1);
    let mut version_to_object: BTreeMap<Hash32, Hash32> = BTreeMap::new();
    for record in state.objects.values() {
        version_to_object.insert(record.version, record.id);
    }

    let mut parents: BTreeSet<Hash32> = BTreeSet::new();
    let mut closure: BTreeSet<Hash32> = BTreeSet::new();
    let mut queue: VecDeque<Hash32> = VecDeque::new();
    let mut visited: BTreeSet<Hash32> = BTreeSet::new();

    for entry in entries {
        if *entry == Hash32::ZERO {
            return Err(Error::non_canonical("causal entry must not be all zero"));
        }
        let identity = match state.objects.get(entry) {
            Some(record) => record.version,
            None => *entry,
        };
        parents.insert(identity);
        closure.insert(identity);
        queue.push_back(identity);
    }

    while let Some(identity) = queue.pop_front() {
        if !visited.insert(identity) {
            continue;
        }
        let Some(object_id) = version_to_object.get(&identity).copied() else {
            continue;
        };
        let Some(record) = state.objects.get(&object_id) else {
            continue;
        };
        let Some(stored) = object_causal_closure(record)? else {
            continue;
        };
        for ancestor in stored.closure {
            if ancestor == Hash32::ZERO {
                return Err(Error::corrupt(
                    "stored causal closure contains the zero hash",
                ));
            }
            closure.insert(ancestor);
            queue.push_back(ancestor);
            if closure.len() > max_entries {
                return Err(Error::limit_exceeded(format!(
                    "causal closure exceeds max_causal_entries ({max_entries})"
                )));
            }
        }
    }

    if closure.len() > max_entries {
        return Err(Error::limit_exceeded(format!(
            "causal closure exceeds max_causal_entries ({max_entries})"
        )));
    }
    Ok(CausalClosure::new(
        parents.into_iter().collect(),
        closure.into_iter().collect(),
    )?)
}

/// Build the causal attrs/version for a new or updated object.
///
/// Returns `(attrs, identity_mode, causal_hash, closure)` where the mode is
/// `causal_exact` iff `entries` is non-empty. The caller stores the closure
/// record content-addressed before publishing the object.
pub fn prepare_causal(
    state: &State,
    mut attrs: BTreeMap<String, Value>,
    entries: &[Hash32],
    limits: &Limits,
) -> Result<(
    BTreeMap<String, Value>,
    IdentityMode,
    Option<Hash32>,
    Option<CausalClosure>,
)> {
    if entries.is_empty() {
        return Ok((attrs, IdentityMode::BytesOnly, None, None));
    }
    let closure = compute_causal_closure(state, entries, limits)?;
    let causal_hash = closure.id()?;
    apply_causal_attrs(&mut attrs, &closure, IdentityMode::CausalExact, causal_hash)?;
    Ok((
        attrs,
        IdentityMode::CausalExact,
        Some(causal_hash),
        Some(closure),
    ))
}

// ---------------------------------------------------------------------------
// Capability parsing and storage
// ---------------------------------------------------------------------------

/// Parse a capability record from a JSON file or canonical WCB hex.
pub fn parse_capability_text(text: &str) -> Result<CapabilityScope> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        let json: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|err| Error::parse(format!("bad capability JSON: {err}")))?;
        let object = json
            .as_object()
            .ok_or_else(|| Error::parse("capability JSON must be an object"))?;
        for key in object.keys() {
            match key.as_str() {
                "schema" | "principal" | "namespaces" | "roots" | "predicates" | "budget"
                | "expires_at" => {}
                other => {
                    return Err(Error::non_canonical(format!(
                        "unknown capability key `{other}`"
                    )))
                }
            }
        }
        if let Some(schema) = object.get("schema") {
            if schema.as_str() != Some("weft.capability.v1") {
                return Err(Error::unknown_schema(
                    "capability schema must be weft.capability.v1",
                ));
            }
        }
        let principal = object
            .get("principal")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::parse("capability `principal` must be text"))?;
        let namespaces = string_array(object.get("namespaces"), "namespaces")?;
        let roots = match object.get("roots") {
            None => Vec::new(),
            Some(value) => {
                let items = value
                    .as_array()
                    .ok_or_else(|| Error::parse("capability `roots` must be an array"))?;
                let mut roots = Vec::with_capacity(items.len());
                for item in items {
                    let text = item
                        .as_str()
                        .ok_or_else(|| Error::parse("capability roots must be hex text"))?;
                    roots.push(Hash32::from_hex(text)?);
                }
                roots
            }
        };
        let mut predicates = string_array(object.get("predicates"), "predicates")?;
        predicates.sort();
        predicates.dedup();
        let budget = match object.get("budget") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => {
                let map = value
                    .as_object()
                    .ok_or_else(|| Error::parse("capability budget must be an object"))?;
                let mut budget = Budget::new();
                for (key, value) in map {
                    let number = value
                        .as_u64()
                        .ok_or_else(|| Error::parse(format!("budget `{key}` must be u64")))?;
                    match key.as_str() {
                        "max_results" => budget.max_results = Some(number),
                        "max_bytes" => budget.max_bytes = Some(number),
                        "max_depth" => budget.max_depth = Some(number),
                        "max_work" => budget.max_work = Some(number),
                        other => {
                            return Err(Error::non_canonical(format!(
                                "unknown budget key `{other}`"
                            )))
                        }
                    }
                }
                budget.validate()?;
                Some(budget)
            }
        };
        let expires_at = match object.get("expires_at") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(
                value
                    .as_i64()
                    .ok_or_else(|| Error::parse("capability expires_at must be an integer"))?,
            ),
        };
        return CapabilityScope::new(principal, namespaces, roots, predicates, budget, expires_at)
            .map_err(Error::from);
    }
    let bytes = hex::decode(trimmed)
        .map_err(|_| Error::parse("capability file is neither JSON nor canonical hex"))?;
    CapabilityScope::from_canonical_bytes(&bytes, &Limits::default()).map_err(Error::from)
}

fn string_array(value: Option<&serde_json::Value>, what: &str) -> Result<Vec<String>> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(value) => {
            let items = value
                .as_array()
                .ok_or_else(|| Error::parse(format!("capability `{what}` must be an array")))?;
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(
                    item.as_str()
                        .ok_or_else(|| Error::parse(format!("capability `{what}` must be text")))?
                        .to_owned(),
                );
            }
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------
// Store integration
// ---------------------------------------------------------------------------

impl Store {
    /// Persist a Context Root relation. Duplicate names are `Exists` unless
    /// `replace` is true, in which case delete+add is one commit.
    pub fn save_context_root(
        &mut self,
        name: &str,
        root: &ContextRoot,
        capability: Option<&CapabilityScope>,
        replace: bool,
    ) -> Result<Hash32> {
        self.require_writable_context()?;
        crate::state::validate_snapshot_name(name)?;
        let record_bytes = root.canonical_bytes()?;
        let stored_id = root.id()?;
        let uri = root.authoritative_uri()?;
        let mut attrs = BTreeMap::new();
        attrs.insert(
            CONTEXT_ROOT_RECORD_ATTR.to_owned(),
            Value::Str(hex::encode(record_bytes)),
        );
        attrs.insert(CONTEXT_ROOT_URI_ATTR.to_owned(), Value::Str(uri));
        let relation = RelationRecord::new(
            State::ROOT_ID,
            CONTEXT_ROOT_PREDICATE,
            RelationTarget::Value(Value::Str(name.to_owned())),
            None,
            attrs,
        )?;

        let existing_ids: Vec<Hash32> =
            Self::context_root_relations_at(self.current_state(), name)?
                .iter()
                .map(|relation| relation.id)
                .collect();
        if !existing_ids.is_empty() && !replace {
            return Err(Error::exists(
                "a context root with this name already exists",
            ));
        }

        let mut ops = Vec::new();
        if let Some(scope) = capability {
            ops.extend(self.capability_ops(scope)?);
        }
        match existing_ids.as_slice() {
            [] => ops.push(crate::state::Op::PutRelation(relation.clone())),
            [current] => {
                if *current == relation.id {
                    if !ops.is_empty() {
                        self.commit_ops_context(ops, false)?;
                    }
                    return Ok(relation.id);
                }
                ops.push(crate::state::Op::DelRelation(*current));
                ops.push(crate::state::Op::PutRelation(relation.clone()));
            }
            _ => {
                return Err(Error::corrupt(
                    "more than one context_root relation has the same name",
                ))
            }
        }
        if ops.is_empty() {
            return Ok(relation.id);
        }
        self.commit_ops_context(ops, false)?;
        // Keep the id check honest even when the encoder changed.
        debug_assert_eq!(stored_id, root.id()?);
        Ok(relation.id)
    }

    /// Delete a named Context Root.
    pub fn delete_context_root(&mut self, name: &str) -> Result<()> {
        self.require_writable_context()?;
        let existing_ids: Vec<Hash32> =
            Self::context_root_relations_at(self.current_state(), name)?
                .iter()
                .map(|relation| relation.id)
                .collect();
        match existing_ids.as_slice() {
            [] => Err(Error::not_found("no such context root")),
            [current] => {
                self.commit_ops_context(vec![crate::state::Op::DelRelation(*current)], false)?;
                Ok(())
            }
            _ => Err(Error::corrupt(
                "more than one context_root relation has the same name",
            )),
        }
    }

    /// All stored Context Roots, sorted by name.
    pub fn list_context_roots(&self) -> Result<Vec<StoredContextRoot>> {
        self.list_context_roots_at(self.current_state())
    }

    /// Stored Context Roots in an explicit state.
    pub fn list_context_roots_at(&self, state: &State) -> Result<Vec<StoredContextRoot>> {
        let mut out = Vec::new();
        for relation in state.relations.values() {
            if !is_context_root_relation(relation) {
                continue;
            }
            let RelationTarget::Value(Value::Str(name)) = &relation.target else {
                return Err(Error::corrupt("context_root target is not text"));
            };
            out.push(Self::context_root_from_relation(name, relation)?);
        }
        out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        Ok(out)
    }

    /// Load a Context Root by name or `weft://` URI.
    pub fn load_context_root(&self, name_or_uri: &str) -> Result<StoredContextRoot> {
        self.load_context_root_at(self.current_state(), name_or_uri)
    }

    /// Load a Context Root from an explicit state.
    pub fn load_context_root_at(
        &self,
        state: &State,
        name_or_uri: &str,
    ) -> Result<StoredContextRoot> {
        if name_or_uri.starts_with(weftcodec::context::URI_PREFIX) {
            let limits = Limits::default();
            let parsed = weftcodec::context::parse_context_uri(name_or_uri, &limits)?;
            let (namespace, id, expected_root) = match &parsed {
                ContextUri::Authoritative { namespace, id } => (namespace.clone(), *id, None),
                ContextUri::Descriptor { .. } => {
                    let root = parsed
                        .to_context_root(&limits)?
                        .ok_or_else(|| Error::non_canonical("descriptor produced no root"))?;
                    (root.namespace.clone(), root.id()?, Some(root))
                }
            };
            let mut matches = Vec::new();
            for relation in state.relations.values() {
                if !is_context_root_relation(relation) {
                    continue;
                }
                let RelationTarget::Value(Value::Str(name)) = &relation.target else {
                    return Err(Error::corrupt("context_root target is not text"));
                };
                let stored = Self::context_root_from_relation(name, relation)?;
                if stored.context_root_id == id {
                    matches.push(stored);
                }
            }
            matches.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
            let stored = matches
                .into_iter()
                .next()
                .ok_or_else(|| Error::not_found("no stored context root has this address"))?;
            if stored.root.namespace != namespace {
                return Err(Error::corrupt(
                    "context-root URI namespace does not match the stored record",
                ));
            }
            if let Some(expected) = expected_root {
                if expected.canonical_bytes()? != stored.root.canonical_bytes()? {
                    return Err(Error::non_canonical(
                        "descriptor URI does not canonicalize to the stored context root",
                    ));
                }
            }
            return Ok(stored);
        }
        let existing = Self::context_root_relations_at(state, name_or_uri)?;
        match existing.as_slice() {
            [relation] => Self::context_root_from_relation(name_or_uri, relation),
            [] => Err(Error::not_found("no such context root")),
            _ => Err(Error::corrupt(
                "more than one context_root relation has the same name",
            )),
        }
    }

    /// Persist a capability record (idempotent by hash).
    pub fn save_capability(&mut self, scope: &CapabilityScope) -> Result<Hash32> {
        self.require_writable_context()?;
        let ops = self.capability_ops(scope)?;
        if !ops.is_empty() {
            self.commit_ops_context(ops, false)?;
        }
        scope.id().map_err(Error::from)
    }

    /// Load a stored capability by hash.
    pub fn load_capability(&self, hash: Hash32) -> Result<CapabilityScope> {
        self.load_capability_at(self.current_state(), hash)
    }

    /// Load a stored capability from an explicit state.
    pub fn load_capability_at(&self, state: &State, hash: Hash32) -> Result<CapabilityScope> {
        let wanted = hash.to_hex();
        for relation in state.relations.values() {
            if !is_capability_relation(relation) {
                continue;
            }
            let RelationTarget::Value(Value::Str(target)) = &relation.target else {
                return Err(Error::corrupt("capability target is not text"));
            };
            if target != &wanted {
                continue;
            }
            return Self::capability_from_relation(relation);
        }
        Err(Error::not_found("no stored capability has this hash"))
    }

    /// Resolve a stored Context Root under an explicit capability context.
    ///
    /// Order of operations: verify identity, verify snapshot, verify resolver,
    /// authorize/filter, evaluate, then enforce budgets. Counts and bytes are
    /// only produced after authorization.
    pub fn resolve_context(
        &self,
        name_or_uri: &str,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        limits: &Limits,
        now_ns: i64,
    ) -> Result<ContextResolution> {
        self.resolve_context_at(
            self.current_state(),
            name_or_uri,
            capability,
            principal,
            limits,
            now_ns,
        )
    }

    pub(crate) fn resolve_context_at(
        &self,
        metadata_state: &State,
        name_or_uri: &str,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        limits: &Limits,
        now_ns: i64,
    ) -> Result<ContextResolution> {
        let stored = self.load_context_root_at(metadata_state, name_or_uri)?;
        let root = stored.root.clone();
        // 1. Identity is recomputed from canonical bytes by the decoder; verify
        //    it against the relation's stored address again here.
        let recomputed = root.id()?;
        if recomputed != stored.context_root_id {
            return Err(Error::corrupt(
                "stored context-root record does not match its address",
            ));
        }
        if root.resolver != weftcodec::context::DEFAULT_RESOLVER {
            return Err(Error::unsupported_version(format!(
                "unknown resolver `{}`",
                root.resolver
            )));
        }
        // 2. Snapshot pinning: the named commit must exist and replay.
        let snapshot_state = self.state_at(root.snapshot)?;

        // 3. Capability binding.
        let bound_capability;
        let active_capability = match root.capability_hash {
            Some(hash) => {
                let supplied = match capability {
                    Some(cap) => {
                        if cap.hash != hash {
                            return Err(Error::corrupt(
                                "supplied capability hash does not match the context root binding",
                            ));
                        }
                        Some(cap)
                    }
                    None => None,
                };
                match supplied {
                    Some(cap) => Some(cap),
                    None => {
                        // A stored capability is an acceptable source for the
                        // bound grant; v1 otherwise requires the caller to
                        // supply the record by file.
                        let scope = self.load_capability_at(metadata_state, hash)?;
                        bound_capability = CapabilityContext::new(scope)?;
                        Some(&bound_capability)
                    }
                }
            }
            None => capability,
        };
        if let Some(cap) = active_capability {
            cap.check_valid_at(now_ns)?;
            if !cap.scope.roots.is_empty() && !cap.allows_root(recomputed) {
                // Non-disclosing denial: the requested root is simply not
                // authorized under this capability.
                return Err(Error::unauthorized(
                    "capability does not grant this context root",
                ));
            }
        }

        let selector = root.selector()?;
        let budget = effective_budget(root.budget.as_ref(), active_capability);
        let resolution = crate::evaluate::resolve_state_with_budget(
            &selector,
            root.snapshot,
            &snapshot_state,
            principal,
            limits,
            active_capability,
            Some(recomputed),
            budget.as_ref(),
        )?;
        if resolution.snapshot_id != root.snapshot {
            return Err(Error::corrupt("resolution changed the pinned snapshot"));
        }
        if resolution.selector_id != root.selector_id {
            return Err(Error::corrupt("resolution changed the selector identity"));
        }
        let work_units = crate::evaluate::work_units(&selector, &snapshot_state, principal);
        Ok(ContextResolution {
            stored,
            selector,
            resolution,
            effective_budget: budget,
            capability_hash: root.capability_hash,
            truncated: false,
            work_units,
        })
    }

    /// Exact-id access under a capability context.
    ///
    /// Without a capability this returns the object if the principal can read
    /// it. With a capability the object must be readable by the principal and
    /// either (a) the `default` namespace is granted, or (b) it appears in the
    /// authorized result set of a granted Context Root. Possession of an id is
    /// never authority. Denials are reported as `NotFound`.
    pub fn exact_id_object(
        &self,
        id: Hash32,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<ObjectRecord> {
        self.exact_id_object_at(self.current_state(), id, capability, principal, now_ns)
    }

    /// Exact-id access against an explicit view state (snapshot mount).
    pub fn exact_id_object_at(
        &self,
        state: &State,
        id: Hash32,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<ObjectRecord> {
        // Match ObjectId, VersionId, or ContentId (ContentId is the manifest id
        // for files and the whole-bytes digest for symlinks/dirs).
        let candidates: Vec<&ObjectRecord> = state
            .objects
            .values()
            .filter(|record| record.id == id || record.version == id || record.content == id)
            .collect();
        if candidates.is_empty() {
            return Err(Error::not_found("no such object"));
        }
        if let Some(cap) = capability {
            cap.check_valid_at(now_ns)?;
            let namespace_grant = cap.allows_namespace(crate::state::DEFAULT_NAMESPACE);
            let mut allowed_roots: Vec<Hash32> = Vec::new();
            if !namespace_grant {
                allowed_roots = cap.scope.roots.clone();
            }
            for record in candidates {
                if !can_read(record, principal) {
                    continue;
                }
                if namespace_grant {
                    return Ok(record.clone());
                }
                // Root grant: the object (or one of its identities) must be
                // selected by at least one granted root.
                for root_id in &allowed_roots {
                    let context = self.load_context_root(&root_id.to_hex())?;
                    let resolution = self.resolve_context_at(
                        self.current_state(),
                        &context.name,
                        Some(cap),
                        principal,
                        &Limits::default(),
                        now_ns,
                    )?;
                    if resolution.resolution.results.iter().any(|result| {
                        result.object_id == record.id
                            || result.version_id == id
                            || result.content_id == id
                    }) {
                        return Ok(record.clone());
                    }
                }
            }
            return Err(Error::not_found("no such object"));
        }
        for record in candidates {
            if can_read(record, principal) {
                return Ok(record.clone());
            }
        }
        Err(Error::not_found("no such object"))
    }

    /// Saved-context views under a capability.
    ///
    /// A saved context is visible only when the principal can read the
    /// namespace root and the capability grants the selector's namespace and
    /// every relation predicate it references.
    pub fn visible_saved_contexts(
        &self,
        state: &State,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        now_ns: i64,
    ) -> Result<Vec<crate::store::SavedContext>> {
        if let Some(cap) = capability {
            cap.check_valid_at(now_ns)?;
        }
        let root = state
            .objects
            .get(&State::ROOT_ID)
            .ok_or_else(|| Error::corrupt("namespace root is missing"))?;
        if !can_read(root, principal) {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for saved in self.list_contexts_at(state)? {
            let selector = self.load_context_at(state, &saved.name)?;
            if let Some(cap) = capability {
                if !cap.allows_namespace(&selector.namespace) {
                    continue;
                }
                if !selector_predicates_allowed(&selector, cap) {
                    continue;
                }
            }
            out.push(saved);
        }
        Ok(out)
    }

    /// Materialize a resolved Context Root atomically.
    ///
    /// Every content hash is verified before the output directory is published;
    /// files are written into a sibling temporary directory and renamed into
    /// place. A failure leaves no partial output at `out`.
    pub fn materialize_context(
        &self,
        name_or_uri: &str,
        out: &Path,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        limits: &Limits,
        now_ns: i64,
    ) -> Result<MaterializationReport> {
        let resolved = self.resolve_context(name_or_uri, capability, principal, limits, now_ns)?;
        let state = self.state_at(resolved.stored.root.snapshot)?;
        let mut entries = Vec::new();
        let mut total_bytes: u64 = 0;
        let mut seen_paths: BTreeSet<String> = BTreeSet::new();
        for result in &resolved.resolution.results {
            let record = state
                .objects
                .get(&result.object_id)
                .ok_or_else(|| Error::corrupt("selector result has no object record"))?
                .clone();
            let display_path = state
                .path_for(result.object_id)
                .map(|path| String::from_utf8_lossy(&path).into_owned())
                .unwrap_or_else(|| format!("/objects/{}", result.object_id.to_hex()));
            let relative = display_path.trim_start_matches('/').to_owned();
            if !seen_paths.insert(display_path.clone()) {
                return Err(Error::corrupt(format!(
                    "materialization path `{display_path}` is not unique"
                )));
            }
            let (bytes, content_hash) = match record.kind {
                ObjectKind::Dir => (Vec::new(), identity::content_hash(b"")),
                ObjectKind::Symlink => {
                    let bytes = record
                        .symlink_target()
                        .ok_or_else(|| Error::corrupt("symlink has no target"))?
                        .to_vec();
                    let hash = identity::content_hash(&bytes);
                    if hash != record.content {
                        return Err(Error::corrupt("symlink content hash mismatch"));
                    }
                    (bytes, hash)
                }
                ObjectKind::File => {
                    // `read_object_at` verifies every chunk and the whole-file
                    // content hash against the pinned manifest.
                    let bytes =
                        self.read_object_at(Some(resolved.stored.root.snapshot), record.id)?;
                    let hash = identity::content_hash(&bytes);
                    (bytes, hash)
                }
            };
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            let closure = object_causal_closure(&record)?
                .map(|closure| closure.closure)
                .unwrap_or_default();
            entries.push(MaterializeEntry {
                relative,
                display_path,
                object_id: record.id,
                version_id: record.version,
                content_id: record.content,
                content_hash,
                size: bytes.len() as u64,
                kind: record.kind.as_str(),
                bytes,
                causal_closure: closure,
            });
        }
        if let Some(budget) = &resolved.effective_budget {
            if let Some(max) = budget.max_bytes {
                if total_bytes > max {
                    return Err(Error::limit_exceeded(format!(
                        "materialization needs {total_bytes} bytes, budget is {max}"
                    )));
                }
            }
        }
        entries.sort_by(|a, b| a.display_path.as_bytes().cmp(b.display_path.as_bytes()));

        let (tmp, manifest) = self.write_materialization(&resolved, out, &entries, total_bytes)?;
        // Publish atomically.
        if out.exists() {
            if !out.is_dir() {
                cleanup_dir(&tmp);
                return Err(Error::exists(
                    "materialization output exists and is not a directory",
                ));
            }
            let mut items = fs::read_dir(out)
                .map_err(|err| Error::io(format!("cannot read {}: {err}", out.display())))?;
            if items.next().is_some() {
                cleanup_dir(&tmp);
                return Err(Error::exists(
                    "materialization output directory is not empty",
                ));
            }
            fs::remove_dir(out)
                .map_err(|err| Error::io(format!("cannot replace {}: {err}", out.display())))?;
        }
        fs::rename(&tmp, out).map_err(|err| {
            cleanup_dir(&tmp);
            Error::io(format!(
                "cannot publish materialization at {}: {err}",
                out.display()
            ))
        })?;
        let manifest_path = out.join(".weft-context.json");
        write_manifest(&manifest_path, &manifest)?;
        Ok(MaterializationReport {
            out: out.to_path_buf(),
            manifest_path,
            files: entries.len() as u64,
            bytes: total_bytes,
            context_root_id: resolved.stored.context_root_id,
            snapshot_id: resolved.resolution.snapshot_id,
            selector_id: resolved.resolution.selector_id,
            closure: resolved.stored.root.causal.clone(),
        })
    }

    // ---- internals -------------------------------------------------------

    fn require_writable_context(&self) -> Result<()> {
        if self.mode() != crate::store::OpenMode::ReadWrite {
            return Err(Error::read_only(
                "store is opened read-only; mutation refused",
            ));
        }
        Ok(())
    }

    fn commit_ops_context(
        &mut self,
        ops: Vec<crate::state::Op>,
        force_checkpoint: bool,
    ) -> Result<Hash32> {
        self.commit_ops_pub(ops, force_checkpoint)
    }

    fn capability_ops(&self, scope: &CapabilityScope) -> Result<Vec<crate::state::Op>> {
        let hash = scope.id()?;
        let target = hash.to_hex();
        if self.current_state().relations.values().any(|relation| {
            is_capability_relation(relation) && relation_target_text(relation) == Some(&target)
        }) {
            return Ok(Vec::new());
        }
        let mut attrs = BTreeMap::new();
        attrs.insert(
            CAPABILITY_RECORD_ATTR.to_owned(),
            Value::Str(hex::encode(scope.canonical_bytes()?)),
        );
        let relation = RelationRecord::new(
            State::ROOT_ID,
            CAPABILITY_PREDICATE,
            RelationTarget::Value(Value::Str(target)),
            None,
            attrs,
        )?;
        Ok(vec![crate::state::Op::PutRelation(relation)])
    }

    fn context_root_relations_at<'a>(
        state: &'a State,
        name: &str,
    ) -> Result<Vec<&'a RelationRecord>> {
        let mut out = Vec::new();
        for relation in state.relations.values() {
            if !is_context_root_relation(relation) {
                continue;
            }
            if matches!(&relation.target, RelationTarget::Value(Value::Str(text)) if text == name) {
                out.push(relation);
            }
        }
        if out.len() > 1 {
            return Err(Error::corrupt(
                "more than one context_root relation has the same name",
            ));
        }
        Ok(out)
    }

    fn context_root_from_relation(
        name: &str,
        relation: &RelationRecord,
    ) -> Result<StoredContextRoot> {
        let record_hex = match relation.attrs.get(CONTEXT_ROOT_RECORD_ATTR) {
            Some(Value::Str(text)) => text.clone(),
            _ => {
                return Err(Error::corrupt(
                    "context_root relation has no context.record attribute",
                ))
            }
        };
        let uri = match relation.attrs.get(CONTEXT_ROOT_URI_ATTR) {
            Some(Value::Str(text)) => text.clone(),
            _ => return Err(Error::corrupt("context_root relation has no uri attribute")),
        };
        let bytes = hex::decode(&record_hex)
            .map_err(|_| Error::corrupt("context_root record is not hex"))?;
        let root = ContextRoot::from_canonical_bytes(&bytes, &Limits::default())?;
        let id = root.id()?;
        let expected_uri = root.authoritative_uri()?;
        if uri != expected_uri {
            return Err(Error::corrupt(
                "context_root uri does not match the canonical record",
            ));
        }
        let parsed = weftcodec::context::parse_context_uri(&uri, &Limits::default())?;
        match parsed {
            ContextUri::Authoritative {
                namespace,
                id: parsed_id,
            } if namespace == root.namespace && parsed_id == id => {}
            _ => return Err(Error::corrupt("context_root uri is not authoritative")),
        }
        Ok(StoredContextRoot {
            name: name.to_owned(),
            root,
            record_hex,
            uri,
            relation_id: relation.id,
            context_root_id: id,
        })
    }

    fn capability_from_relation(relation: &RelationRecord) -> Result<CapabilityScope> {
        let record_hex = match relation.attrs.get(CAPABILITY_RECORD_ATTR) {
            Some(Value::Str(text)) => text.clone(),
            _ => {
                return Err(Error::corrupt(
                    "capability relation has no capability.record",
                ))
            }
        };
        let expected = relation_target_text(relation)
            .ok_or_else(|| Error::corrupt("capability target is not text"))?;
        let bytes =
            hex::decode(&record_hex).map_err(|_| Error::corrupt("capability record is not hex"))?;
        let scope = CapabilityScope::from_canonical_bytes(&bytes, &Limits::default())?;
        if scope.id()?.to_hex() != expected {
            return Err(Error::corrupt(
                "stored capability record does not match its target hash",
            ));
        }
        Ok(scope)
    }

    fn write_materialization(
        &self,
        resolved: &ContextResolution,
        out: &Path,
        entries: &[MaterializeEntry],
        total_bytes: u64,
    ) -> Result<(PathBuf, serde_json::Value)> {
        let parent = out.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", parent.display())))?;
        let base = out
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "context".to_owned());
        let tmp = parent.join(format!(
            ".{base}.weft-tmp-{}-{}",
            std::process::id(),
            MATERIALIZE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if tmp.exists() {
            cleanup_dir(&tmp);
        }
        fs::create_dir(&tmp)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", tmp.display())))?;

        let result = (|| -> Result<serde_json::Value> {
            let mut file_records = Vec::new();
            for entry in entries {
                let relative = if entry.relative.is_empty() {
                    ".".to_owned()
                } else {
                    entry.relative.clone()
                };
                let target = tmp.join(&relative);
                match entry.kind {
                    "dir" => {
                        fs::create_dir_all(&target).map_err(|err| {
                            Error::io(format!("cannot create {}: {err}", target.display()))
                        })?;
                    }
                    "symlink" => {
                        if let Some(parent) = target.parent() {
                            fs::create_dir_all(parent).map_err(|err| {
                                Error::io(format!("cannot create {}: {err}", parent.display()))
                            })?;
                        }
                        symlink(
                            std::str::from_utf8(&entry.bytes).map_err(|_| {
                                Error::invalid_argument("symlink target is not valid UTF-8")
                            })?,
                            &target,
                        )
                        .map_err(|err| {
                            Error::io(format!("cannot create symlink {}: {err}", target.display()))
                        })?;
                    }
                    _ => {
                        if let Some(parent) = target.parent() {
                            fs::create_dir_all(parent).map_err(|err| {
                                Error::io(format!("cannot create {}: {err}", parent.display()))
                            })?;
                        }
                        let mut file = fs::File::create(&target).map_err(|err| {
                            Error::io(format!("cannot create {}: {err}", target.display()))
                        })?;
                        file.write_all(&entry.bytes)?;
                        if identity::content_hash(&entry.bytes) != entry.content_hash {
                            return Err(Error::corrupt("materialization content hash changed"));
                        }
                        file.sync_all()?;
                    }
                }
                file_records.push(json!({
                    "path": entry.display_path,
                    "object_id": entry.object_id.to_hex(),
                    "version_id": entry.version_id.to_hex(),
                    "content_id": entry.content_id.to_hex(),
                    "content_hash": entry.content_hash.to_hex(),
                    "size": entry.size,
                    "kind": entry.kind,
                    "causal_closure": entry.causal_closure.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
                }));
            }
            let manifest = json!({
                "format": "weft-context-materialization.v1",
                "context_root_id": resolved.stored.context_root_id.to_hex(),
                "uri": resolved.stored.uri,
                "name": resolved.stored.name,
                "selector_id": resolved.resolution.selector_id.to_hex(),
                "snapshot_id": resolved.resolution.snapshot_id.to_hex(),
                "closure": resolved.stored.root.causal.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
                "capability_hash": resolved.capability_hash.map(|hash| hash.to_hex()),
                "budget": resolved.effective_budget.as_ref().map(|budget| budget.to_json()),
                "matched_count": resolved.resolution.matched_count,
                "returned": resolved.resolution.results.len(),
                "truncated": resolved.truncated,
                "bytes": total_bytes,
                "files": file_records,
            });
            Ok(manifest)
        })();
        match result {
            Ok(manifest) => Ok((tmp, manifest)),
            Err(err) => {
                cleanup_dir(&tmp);
                Err(err)
            }
        }
    }
}

fn cleanup_dir(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn write_manifest(path: &Path, manifest: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|err| Error::io(format!("cannot encode materialization manifest: {err}")))?;
    let mut file = fs::File::create(path)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", path.display())))?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

pub(crate) fn is_context_root_relation(relation: &RelationRecord) -> bool {
    relation.subject == State::ROOT_ID
        && relation.predicate == CONTEXT_ROOT_PREDICATE
        && relation.kind == RelationKind::Value
        && relation.provenance.is_none()
}

pub(crate) fn is_capability_relation(relation: &RelationRecord) -> bool {
    relation.subject == State::ROOT_ID
        && relation.predicate == CAPABILITY_PREDICATE
        && relation.kind == RelationKind::Value
        && relation.provenance.is_none()
}

pub(crate) fn relation_target_text(relation: &RelationRecord) -> Option<&str> {
    match &relation.target {
        RelationTarget::Value(Value::Str(text)) => Some(text),
        _ => None,
    }
}

fn selector_predicates_allowed(selector: &Selector, cap: &CapabilityContext) -> bool {
    fn walk(expr: &Expr, cap: &CapabilityContext) -> bool {
        match expr {
            Expr::Tag(_) => cap.allows_predicate("tagged_as"),
            Expr::Relation { predicate, .. } => cap.allows_predicate(predicate),
            Expr::And(items) | Expr::Or(items) => items.iter().all(|item| walk(item, cap)),
            Expr::Not(inner) => walk(inner, cap),
            _ => true,
        }
    }
    walk(&selector.predicate, cap)
}

/// Store a causal closure record content-addressed and return its id.
///
/// The record is a normal manifest blob under the causal domain, so GC can keep
/// it reachable through the object that references it.
pub(crate) fn store_causal_closure(store: &Store, closure: &CausalClosure) -> Result<Hash32> {
    let bytes = closure.canonical_bytes()?;
    let id = closure.id()?;
    let stored = recovery::read_blob_verified(
        store.root(),
        BlobKind::Manifest,
        id,
        weftcodec::hash::DOMAIN_CAUSAL,
    );
    if stored.is_err() {
        store.store_causal_blob(&bytes)?;
    }
    Ok(id)
}
