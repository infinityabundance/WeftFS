//! One explicit artifact API: resolve, plan, materialize, verify, pin, release,
//! plus transaction semantics (subphase T072).
//!
//! Normative description: `docs/ARTIFACT-API.md`. This module is the only
//! surface a consumer needs; DCP, Orbax and the agent-state consumer all call
//! it through `python/weftcodec/artifact_api.py`, which is a thin binding over
//! the `weftfs artifact` CLI verbs.
//!
//! # Snapshot scoping
//!
//! Every call names a snapshot ([`SnapshotRef`]) or accepts the head and marks
//! the result `provisional = true`. There is deliberately no mutable `latest`
//! inside a pinned context: a Context Root pins its own commit snapshot and a
//! request that names a different snapshot is refused rather than retargeted.
//! A named or hashed snapshot that this store does not hold is
//! [`crate::error::Class::SnapshotMissing`], never a silent fall back to the
//! head.
//!
//! # What this is not
//!
//! It is not a scheduler and it is not a transport. It produces ordered
//! requirements, verifies bytes, and mutates retention state. It performs no
//! placement, caching policy, prefetch or network transfer decision.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use weftcodec::wcb::{self, Value};
use weftcodec::Limits;

use crate::auth::Principal;
use crate::checkpoint::{CheckpointTensor, RestoredTensor};
use crate::error::{Class, Error, Result};
use crate::evaluate::{Evidence, ResolvedRef};
use crate::identity::{self, Hash32};
use crate::retention::{RetentionClass, RetentionRequest};
use crate::state::{ObjectKind, State};
use crate::store::{OpenMode, Options, Store};

/// Plan record schema. The plan is derived, never stored as identity-bearing
/// state; `plan_root` is its digest for receipts and logs.
pub const ARTIFACT_PLAN_SCHEMA: &str = "weft.artifact.plan.v1";
/// Planning domain for `plan_root = H(ARTIFACT_PLAN_DOMAIN, WCB(plan body))`.
pub const ARTIFACT_PLAN_DOMAIN: &str = "weft/artifact/plan/v1";
/// Output manifest schema written beside materialized bytes.
pub const ARTIFACT_MANIFEST_SCHEMA: &str = "weft.artifact.manifest.v1";
/// Pin marker schema (ordinary committed state under `/pins/<root-hex>`).
pub const ARTIFACT_PIN_SCHEMA: &str = "weft.artifact.pin.v1";
/// Reserved namespace for pin markers.
pub const PINS_DIR: &str = "pins";
/// Domain for the deterministic directory seed used by plan materialization.
const ARTIFACT_DIR_DOMAIN: &str = "weft/artifact/dir/v1";
/// Default declared lifetime used when `release` rebinds a KV artifact to
/// `ephemeral`. It is a hint for ordering, never a guarantee.
pub const DEFAULT_RELEASE_LIFETIME_MS: u64 = 3_600_000;
/// Upper bound on entries one materialization may publish.
pub const MAX_ARTIFACT_ENTRIES: usize = 1_000_000;

/// How a snapshot is named. `Head` is the only provisional form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotRef {
    /// A pinned commit hash.
    Id(Hash32),
    /// A snapshot name resolved in the current metadata view.
    Name(String),
    /// The current head. Every result is marked `provisional`.
    Head,
}

/// A resolved, replayable snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnedView {
    pub snapshot: Hash32,
    /// True only for [`SnapshotRef::Head`]. A provisional view is usable for
    /// reads but is not an identity: callers must not persist it as a pin.
    pub provisional: bool,
}

/// What the caller wants. One enum for every verb keeps the three consumers on
/// exactly the same operation set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactRef {
    /// A stored Context Root, by name or authoritative `weft://` URI.
    ContextRoot { name_or_uri: String },
    /// A `weft.checkpoint.v1` checkpoint, optionally one tensor inside it.
    Checkpoint {
        name: String,
        tensor: Option<String>,
    },
    /// One exact store object id.
    Object { id: Hash32 },
}

impl ArtifactRef {
    pub fn kind(&self) -> &'static str {
        match self {
            ArtifactRef::ContextRoot { .. } => "context_root",
            ArtifactRef::Checkpoint { .. } => "checkpoint",
            ArtifactRef::Object { .. } => "object",
        }
    }

    /// A stable human label used in pin records and receipts.
    pub fn label(&self) -> String {
        match self {
            ArtifactRef::ContextRoot { name_or_uri } => name_or_uri.clone(),
            ArtifactRef::Checkpoint { name, tensor } => match tensor {
                Some(tensor) => format!("{name}/{tensor}"),
                None => name.clone(),
            },
            ArtifactRef::Object { id } => id.to_hex(),
        }
    }
}

/// One planned artifact entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactEntry {
    /// Stable name: the store path for a Context Root entry, the tensor name
    /// for a checkpoint tensor, or the store path for an exact object.
    pub name: String,
    /// `file`, `dir`, `symlink`, or `tensor`.
    pub kind: String,
    pub object_id: Option<Hash32>,
    pub version_id: Option<Hash32>,
    pub content_id: Option<Hash32>,
    /// Verified content root (`H("weft/chunk/v1", bytes)`) for byte artifacts;
    /// for a checkpoint tensor this is the whole-tensor content root.
    pub content_root: Hash32,
    /// Logical tensor root, when the artifact has one.
    pub logical_root: Option<Hash32>,
    pub bytes: u64,
    pub dtype: Option<String>,
    pub shape: Vec<u64>,
    /// Number of stored shards (checkpoint tensors only).
    pub shards: u64,
    /// Declared dependencies: the causal closure for a Context Root entry, or
    /// the shard content roots for a checkpoint tensor. Sorted unique.
    pub dependencies: Vec<Hash32>,
    /// Store object ids the entry depends on (checkpoint shard objects).
    /// Derived from the manifest, not part of the plan digest.
    pub dependency_objects: Vec<Hash32>,
}

/// A declared dependency that is not usable at the pinned snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingDependency {
    pub root: Hash32,
    pub reason: String,
}

/// The resolve result.
#[derive(Clone, Debug)]
pub struct ArtifactResolution {
    pub kind: &'static str,
    pub snapshot: Hash32,
    pub provisional: bool,
    pub context_root_id: Option<Hash32>,
    pub selector_id: Option<Hash32>,
    pub manifest_root: Option<Hash32>,
    pub matched_count: u64,
    pub complete: bool,
    pub entries: Vec<ArtifactEntry>,
    pub evidence: Vec<Evidence>,
    /// Set for a checkpoint resolution: every tensor in the manifest, even when
    /// `tensor` narrowed `entries`.
    pub tensor_count: u64,
}

/// The plan. A plan is a statement about what *would* be read, never a claim
/// that anything was read.
#[derive(Clone, Debug)]
pub struct ArtifactPlan {
    pub snapshot: Hash32,
    pub provisional: bool,
    pub kind: &'static str,
    pub entries: Vec<ArtifactEntry>,
    /// Sorted unique dependency roots referenced by any entry.
    pub dependencies: Vec<Hash32>,
    pub missing: Vec<MissingDependency>,
    pub complete: bool,
    pub plan_root: Hash32,
    pub tensor_count: u64,
}

/// A materialized directory.
#[derive(Clone, Debug)]
pub struct MaterializeOutcome {
    pub out: PathBuf,
    pub manifest_path: PathBuf,
    pub files: u64,
    pub bytes: u64,
    pub snapshot: Hash32,
    pub provisional: bool,
    pub plan_root: Hash32,
    /// `(relative name, sha256 hex)` for every produced file, sorted by name.
    pub file_digests: Vec<(String, String)>,
}

/// One tensor materialized into memory (the streaming consumer path).
#[derive(Clone, Debug)]
pub struct MaterializedTensor {
    pub checkpoint: String,
    pub tensor: String,
    pub bytes: Vec<u8>,
    pub report: RestoredTensor,
    pub snapshot: Hash32,
    pub provisional: bool,
    pub plan_root: Hash32,
}

/// Which materialization form the caller asked for.
#[derive(Clone, Debug)]
pub enum Materialized {
    Directory(MaterializeOutcome),
    Tensor(MaterializedTensor),
}

/// One verification problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactProblem {
    pub class: String,
    pub entry: String,
    pub detail: String,
}

/// Verification result.
#[derive(Clone, Debug)]
pub struct ArtifactVerification {
    pub ok: bool,
    /// Entries whose bytes were re-read and re-hashed from the store.
    pub store_checked: u64,
    /// Entries whose output file (when `out` was supplied) was re-hashed.
    pub output_checked: u64,
    pub problems: Vec<ArtifactProblem>,
    pub snapshot: Hash32,
    pub provisional: bool,
    pub plan_root: Hash32,
}

/// Pin outcome.
#[derive(Clone, Debug)]
pub struct PinOutcome {
    pub root: Hash32,
    pub marker_created: bool,
    /// Retention binding id when the artifact carried a KV provenance
    /// descriptor root and a `durable` binding was published.
    pub retention_binding: Option<Hash32>,
    pub commit: Hash32,
    pub snapshot: Hash32,
}

/// Release outcome.
#[derive(Clone, Debug)]
pub struct ReleaseOutcome {
    pub root: Hash32,
    pub marker_removed: bool,
    /// Retention binding id when a `durable` binding was replaced by an
    /// `ephemeral` one.
    pub retention_binding: Option<Hash32>,
    /// `true` when a GC sweep may now reclaim the artifact; reclamation itself
    /// is a separate, explicit `gc` call.
    pub reclaimable_after_gc: bool,
    pub commit: Hash32,
    pub snapshot: Hash32,
}

/// The one artifact API.
pub struct ContextPlane {
    store: Store,
    principal: Principal,
    limits: Limits,
}

impl ContextPlane {
    /// Open a store as a context plane. `mode` is the store's open mode: the
    /// read verbs work read-only, `pin`/`release`/`transaction` need
    /// [`OpenMode::ReadWrite`].
    pub fn open(root: impl AsRef<Path>, mode: OpenMode) -> Result<ContextPlane> {
        ContextPlane::open_with(root, mode, Options::default())
    }

    /// [`ContextPlane::open`] with explicit store options.
    pub fn open_with(
        root: impl AsRef<Path>,
        mode: OpenMode,
        options: Options,
    ) -> Result<ContextPlane> {
        let store = Store::open_with_options(root.as_ref(), mode, options)?;
        Ok(ContextPlane {
            store,
            principal: Principal::root(),
            limits: Limits::default(),
        })
    }

    /// Wrap an already-open store (used by the CLI and by courts).
    pub fn from_store(store: Store, principal: Principal) -> ContextPlane {
        ContextPlane {
            store,
            principal,
            limits: Limits::default(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    pub fn set_principal(&mut self, principal: Principal) {
        self.principal = principal;
    }

    /// Resolve a [`SnapshotRef`] once.
    pub fn observe_snapshot(&self, reference: &SnapshotRef) -> Result<PinnedView> {
        observe_snapshot(&self.store, reference)
    }

    /// Resolve the artifact descriptor and return its entries.
    pub fn resolve(&self, view: PinnedView, artifact: &ArtifactRef) -> Result<ArtifactResolution> {
        resolve_artifact(&self.store, &self.principal, &self.limits, view, artifact)
    }

    /// Produce the ordered plan, including every declared dependency that is
    /// missing at the pinned snapshot. A plan with `complete == false` is a
    /// truthful plan; [`ContextPlane::materialize`] refuses it.
    pub fn plan(&self, view: PinnedView, artifact: &ArtifactRef) -> Result<ArtifactPlan> {
        plan_artifact(&self.store, &self.principal, &self.limits, view, artifact)
    }

    /// Materialize a checkpoint tensor into memory.
    pub fn materialize_tensor(
        &self,
        view: PinnedView,
        checkpoint: &str,
        tensor: &str,
    ) -> Result<MaterializedTensor> {
        materialize_tensor_in(
            &self.store,
            &self.principal,
            &self.limits,
            view,
            checkpoint,
            tensor,
        )
    }

    /// Materialize an artifact to `out`.
    pub fn materialize(
        &self,
        view: PinnedView,
        artifact: &ArtifactRef,
        out: &Path,
    ) -> Result<Materialized> {
        let plan = self.plan(view, artifact)?;
        require_complete_plan(&plan)?;
        if let ArtifactRef::Checkpoint {
            name,
            tensor: Some(tensor),
        } = artifact
        {
            return Ok(Materialized::Tensor(materialize_tensor_in(
                &self.store,
                &self.principal,
                &self.limits,
                view,
                name,
                tensor,
            )?));
        }
        let entries = match artifact {
            ArtifactRef::Checkpoint { name, tensor: None } => {
                read_checkpoint_entries(&self.store, view, &plan, name)?
            }
            _ => {
                let state = state_for(&self.store, view)?;
                read_entries(&self.store, &self.principal, &plan, &state)?
            }
        };
        Ok(Materialized::Directory(publish_materialization(
            view,
            &plan,
            &entries,
            out,
            artifact.kind(),
        )?))
    }

    /// Verify the artifact at the pinned snapshot and, when `out` is supplied,
    /// the produced files.
    pub fn verify(
        &self,
        view: PinnedView,
        artifact: &ArtifactRef,
        out: Option<&Path>,
    ) -> Result<ArtifactVerification> {
        verify_artifact(
            &self.store,
            &self.principal,
            &self.limits,
            view,
            artifact,
            out,
        )
    }

    /// Pin an artifact: publish a durable pin marker reachable from the head
    /// (a GC root), and — when the artifact carries a KV provenance descriptor
    /// root — a `durable` retention binding, in the same group commit.
    pub fn pin(
        &mut self,
        view: PinnedView,
        artifact: &ArtifactRef,
        policy_epoch: u64,
        declared_lifetime_ms: u64,
    ) -> Result<PinOutcome> {
        let resolution = self.resolve(view, artifact)?;
        let descriptor = descriptor_root_of(&self.store, &resolution)?;
        pin_in(
            &mut self.store,
            view,
            artifact,
            &resolution,
            descriptor,
            policy_epoch,
            declared_lifetime_ms,
        )
    }

    /// Release a pin: remove the marker and, if a durable retention binding was
    /// published by [`ContextPlane::pin`], replace it with an `ephemeral` one at
    /// the next policy epoch. Reclamation still requires an explicit `gc`.
    pub fn release(&mut self, view: PinnedView, artifact: &ArtifactRef) -> Result<ReleaseOutcome> {
        let resolution = self.resolve(view, artifact)?;
        let descriptor = descriptor_root_of(&self.store, &resolution)?;
        release_in(&mut self.store, view, artifact, &resolution, descriptor)
    }

    /// Run `f` and publish every mutation it makes as **one** durable commit.
    ///
    /// A failure inside `f` publishes nothing and leaves the store exactly as
    /// it was. Nested transactions are refused rather than flattened. The same
    /// rule [`Store::group`] documents applies unchanged.
    pub fn transaction<T>(
        &mut self,
        f: impl FnOnce(&mut ArtifactTransaction<'_>) -> Result<T>,
    ) -> Result<(T, Hash32)> {
        if self.store.mode() != OpenMode::ReadWrite {
            return Err(Error::read_only(
                "an artifact transaction needs a writable store",
            ));
        }
        if self.store.group_open() {
            return Err(Error::invalid_argument(
                "nested artifact transactions are refused, never flattened",
            ));
        }
        let principal = self.principal.clone();
        let limits = self.limits.clone();
        let mut value = None;
        let ((), commit) = self.store.group(|store| {
            let mut tx = ArtifactTransaction {
                store,
                principal,
                limits,
            };
            let result = f(&mut tx);
            match result {
                Ok(result) => {
                    value = Some(result);
                    Ok(())
                }
                Err(err) => Err(err),
            }
        })?;
        Ok((value.expect("transaction body produced no value"), commit))
    }
}

/// The transactional subset of the API. It borrows the store mutably for the
/// duration of [`ContextPlane::transaction`], so every `pin`/`release` it
/// performs lands in the one commit the transaction publishes.
pub struct ArtifactTransaction<'a> {
    store: &'a mut Store,
    principal: Principal,
    limits: Limits,
}

impl ArtifactTransaction<'_> {
    pub fn pin(
        &mut self,
        view: PinnedView,
        artifact: &ArtifactRef,
        policy_epoch: u64,
        declared_lifetime_ms: u64,
    ) -> Result<PinOutcome> {
        require_pin_snapshot(self.store, view)?;
        let resolution =
            resolve_artifact(self.store, &self.principal, &self.limits, view, artifact)?;
        let descriptor = descriptor_root_of(self.store, &resolution)?;
        pin_marker_and_retention(
            self.store,
            view,
            artifact,
            &resolution,
            descriptor,
            policy_epoch,
            declared_lifetime_ms,
        )
    }

    pub fn release(&mut self, view: PinnedView, artifact: &ArtifactRef) -> Result<ReleaseOutcome> {
        let resolution =
            resolve_artifact(self.store, &self.principal, &self.limits, view, artifact)?;
        let descriptor = descriptor_root_of(self.store, &resolution)?;
        remove_marker_and_rebind(self.store, view, artifact, &resolution, descriptor)
    }

    /// Resolve inside the transaction (reads see the transaction's own writes).
    pub fn resolve(&self, view: PinnedView, artifact: &ArtifactRef) -> Result<ArtifactResolution> {
        resolve_artifact(self.store, &self.principal, &self.limits, view, artifact)
    }
}

// ---------------------------------------------------------------------------
// Free functions (shared by the plane and the transaction)
// ---------------------------------------------------------------------------

fn observe_snapshot(store: &Store, reference: &SnapshotRef) -> Result<PinnedView> {
    match reference {
        SnapshotRef::Head => Ok(PinnedView {
            snapshot: store.head(),
            provisional: true,
        }),
        SnapshotRef::Id(id) => {
            require_snapshot(store, *id)?;
            Ok(PinnedView {
                snapshot: *id,
                provisional: false,
            })
        }
        SnapshotRef::Name(name) => {
            let id = store
                .current_state()
                .snapshots
                .get(name)
                .copied()
                .ok_or_else(|| {
                    Error::snapshot_missing(format!(
                        "snapshot `{name}` is not present in this store"
                    ))
                })?;
            require_snapshot(store, id)?;
            Ok(PinnedView {
                snapshot: id,
                provisional: false,
            })
        }
    }
}

fn resolve_artifact(
    store: &Store,
    principal: &Principal,
    limits: &Limits,
    view: PinnedView,
    artifact: &ArtifactRef,
) -> Result<ArtifactResolution> {
    match artifact {
        ArtifactRef::ContextRoot { name_or_uri } => {
            resolve_context_root(store, principal, limits, view, name_or_uri)
        }
        ArtifactRef::Checkpoint { name, tensor } => {
            resolve_checkpoint(store, view, name, tensor.as_deref())
        }
        ArtifactRef::Object { id } => resolve_object(store, principal, view, *id),
    }
}

fn resolve_context_root(
    store: &Store,
    principal: &Principal,
    limits: &Limits,
    view: PinnedView,
    name_or_uri: &str,
) -> Result<ArtifactResolution> {
    let resolved = store.resolve_context_at(
        store.current_state(),
        name_or_uri,
        None,
        principal,
        limits,
        store.clock_now_ns(),
    )?;
    let root_snapshot = resolved.stored.root.snapshot;
    if !view.provisional && view.snapshot != root_snapshot {
        return Err(Error::invalid_argument(format!(
            "context root `{}` pins snapshot {}, which is not the requested {}; a pinned root \
             is never retargeted",
            resolved.stored.name,
            root_snapshot.to_hex(),
            view.snapshot.to_hex()
        )));
    }
    require_snapshot(store, root_snapshot)?;
    let pinned = PinnedView {
        snapshot: root_snapshot,
        provisional: view.provisional,
    };
    let state = state_for(store, pinned)?;
    let mut entries = Vec::with_capacity(resolved.resolution.results.len());
    for result in &resolved.resolution.results {
        entries.push(entry_for_ref(store, &state, result)?);
    }
    entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    Ok(ArtifactResolution {
        kind: "context_root",
        snapshot: root_snapshot,
        provisional: view.provisional,
        context_root_id: Some(resolved.stored.context_root_id),
        selector_id: Some(resolved.resolution.selector_id),
        manifest_root: None,
        matched_count: resolved.resolution.matched_count,
        complete: true,
        entries,
        evidence: resolved.resolution.evidence,
        tensor_count: 0,
    })
}

fn resolve_checkpoint(
    store: &Store,
    view: PinnedView,
    name: &str,
    tensor: Option<&str>,
) -> Result<ArtifactResolution> {
    let loaded = store
        .load_checkpoint_manifest_at(view.snapshot, name)
        .map_err(|err| match err.class() {
            Class::NotFound => Error::not_found(format!(
                "checkpoint `{name}` is not present at snapshot {}",
                view.snapshot.to_hex()
            )),
            _ => err,
        })?;
    let tensor_count = loaded.manifest.tensors.len() as u64;
    let mut entries = Vec::new();
    for candidate in &loaded.manifest.tensors {
        if let Some(only) = tensor {
            if candidate.name != only {
                continue;
            }
        }
        entries.push(checkpoint_entry(candidate));
    }
    if let Some(only) = tensor {
        if entries.is_empty() {
            return Err(Error::not_found(format!(
                "checkpoint `{name}` has no tensor `{only}`"
            )));
        }
    }
    entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    Ok(ArtifactResolution {
        kind: "checkpoint",
        snapshot: view.snapshot,
        provisional: view.provisional,
        context_root_id: None,
        selector_id: None,
        manifest_root: Some(loaded.manifest_root),
        matched_count: entries.len() as u64,
        complete: loaded.manifest.complete,
        entries,
        evidence: Vec::new(),
        tensor_count,
    })
}

fn resolve_object(
    store: &Store,
    principal: &Principal,
    view: PinnedView,
    id: Hash32,
) -> Result<ArtifactResolution> {
    let state = state_for(store, view)?;
    let record = state.objects.get(&id).ok_or_else(|| {
        Error::not_found(format!(
            "object {id} is not present at snapshot {}",
            view.snapshot.to_hex()
        ))
    })?;
    // Authorization precedes the metadata entry so an unauthorized id is
    // indistinguishable from an absent one at this boundary.
    store.authorize_read_in(&state, id, principal, store.clock_now_ns())?;
    let reference = ResolvedRef {
        namespace: crate::state::DEFAULT_NAMESPACE.to_owned(),
        snapshot: view.snapshot,
        object_id: record.id,
        version_id: record.version,
        content_id: record.content,
    };
    let entry = entry_for_ref(store, &state, &reference)?;
    Ok(ArtifactResolution {
        kind: "object",
        snapshot: view.snapshot,
        provisional: view.provisional,
        context_root_id: None,
        selector_id: None,
        manifest_root: None,
        matched_count: 1,
        complete: true,
        entries: vec![entry],
        evidence: Vec::new(),
        tensor_count: 0,
    })
}

fn entry_for_ref(store: &Store, state: &State, result: &ResolvedRef) -> Result<ArtifactEntry> {
    let record = state
        .objects
        .get(&result.object_id)
        .ok_or_else(|| Error::corrupt("selector result has no object record"))?;
    let name = state
        .path_for(result.object_id)
        .map(|path| String::from_utf8_lossy(&path).into_owned())
        .unwrap_or_else(|| format!("/objects/{}", result.object_id.to_hex()));
    let closure = crate::state::object_causal_closure(record)?
        .map(|closure| closure.closure)
        .unwrap_or_default();
    // `record.content` is the manifest id. The verified whole-bytes identity is
    // the manifest's `content_hash`; for a directory or symlink it is the
    // canonical hash of the empty bytes or of the target.
    let content_root = match record.kind {
        ObjectKind::File => store.manifest_of(record.content)?.content_hash,
        ObjectKind::Dir => identity::content_hash(b""),
        ObjectKind::Symlink => identity::content_hash(
            record
                .symlink_target()
                .ok_or_else(|| Error::corrupt("symlink object has no target"))?,
        ),
    };
    Ok(ArtifactEntry {
        name,
        kind: record.kind.as_str().to_owned(),
        object_id: Some(record.id),
        version_id: Some(record.version),
        content_id: Some(record.content),
        content_root,
        logical_root: record
            .attrs
            .get(crate::kv_capability::ATTR_KV_PROV_ROOT)
            .and_then(|value| match value {
                Value::Str(text) => Hash32::from_hex(text).ok(),
                _ => None,
            }),
        bytes: record.size,
        dtype: None,
        shape: Vec::new(),
        shards: 0,
        dependencies: closure,
        dependency_objects: Vec::new(),
    })
}

fn plan_artifact(
    store: &Store,
    principal: &Principal,
    limits: &Limits,
    view: PinnedView,
    artifact: &ArtifactRef,
) -> Result<ArtifactPlan> {
    let resolution = resolve_artifact(store, principal, limits, view, artifact)?;
    let state = state_for(store, view)?;
    let mut missing = Vec::new();
    let mut dependency_set: BTreeSet<Hash32> = BTreeSet::new();
    let entry_roots: BTreeSet<Hash32> = resolution
        .entries
        .iter()
        .map(|entry| entry.content_root)
        .collect();
    for entry in &resolution.entries {
        for root in &entry.dependencies {
            dependency_set.insert(*root);
        }
        if entry.kind == "tensor" {
            // Every shard object named by the manifest must exist at the pinned
            // snapshot; otherwise the restore would stop at the first missing
            // shard after reading its neighbours.
            for object in &entry.dependency_objects {
                if !state.objects.contains_key(object) {
                    missing.push(MissingDependency {
                        root: *object,
                        reason: format!(
                            "shard object {} for `{}` is absent at the pinned snapshot",
                            object.to_hex(),
                            entry.name
                        ),
                    });
                }
            }
            continue;
        }
        for root in &entry.dependencies {
            if !dependency_present(&state, *root, entry.object_id) && !entry_roots.contains(root) {
                missing.push(MissingDependency {
                    root: *root,
                    reason: format!("declared dependency of `{}` is absent", entry.name),
                });
            }
        }
    }
    let dependencies: Vec<Hash32> = dependency_set.into_iter().collect();
    let complete = missing.is_empty();
    let plan_root = plan_root(
        resolution.snapshot,
        resolution.provisional,
        resolution.kind,
        &resolution.entries,
        &dependencies,
        &missing,
        complete,
    )?;
    Ok(ArtifactPlan {
        snapshot: resolution.snapshot,
        provisional: resolution.provisional,
        kind: resolution.kind,
        entries: resolution.entries,
        dependencies,
        missing,
        complete,
        plan_root,
        tensor_count: resolution.tensor_count,
    })
}

fn materialize_tensor_in(
    store: &Store,
    principal: &Principal,
    limits: &Limits,
    view: PinnedView,
    checkpoint: &str,
    tensor: &str,
) -> Result<MaterializedTensor> {
    let plan = plan_artifact(
        store,
        principal,
        limits,
        view,
        &ArtifactRef::Checkpoint {
            name: checkpoint.to_owned(),
            tensor: Some(tensor.to_owned()),
        },
    )?;
    require_complete_plan(&plan)?;
    let restored =
        store.restore_named_tensor_at(Some(view.snapshot), checkpoint, tensor, None, None)?;
    let entry = plan
        .entries
        .iter()
        .find(|entry| entry.name == tensor)
        .ok_or_else(|| {
            Error::corrupt(format!(
                "checkpoint `{checkpoint}` restored `{tensor}` but the plan does not name it"
            ))
        })?;
    if restored.report.restored_content_root != entry.content_root {
        return Err(Error::corrupt(format!(
            "restored tensor `{tensor}` content root does not match the plan"
        )));
    }
    if let Some(logical) = entry.logical_root {
        if restored.report.full_logical_root != logical {
            return Err(Error::corrupt(format!(
                "restored tensor `{tensor}` logical root does not match the plan"
            )));
        }
    }
    Ok(MaterializedTensor {
        checkpoint: checkpoint.to_owned(),
        tensor: tensor.to_owned(),
        bytes: restored.bytes,
        report: restored.report,
        snapshot: view.snapshot,
        provisional: view.provisional,
        plan_root: plan.plan_root,
    })
}

fn verify_artifact(
    store: &Store,
    principal: &Principal,
    limits: &Limits,
    view: PinnedView,
    artifact: &ArtifactRef,
    out: Option<&Path>,
) -> Result<ArtifactVerification> {
    let plan = plan_artifact(store, principal, limits, view, artifact)?;
    let mut problems = Vec::new();
    if !plan.complete {
        for missing in &plan.missing {
            problems.push(ArtifactProblem {
                class: "DependencyMismatch".to_owned(),
                entry: missing.root.to_hex(),
                detail: missing.reason.clone(),
            });
        }
    }
    let state = state_for(store, view)?;
    let mut store_checked = 0u64;
    for entry in &plan.entries {
        if entry.kind == "tensor" {
            let checkpoint = match artifact {
                ArtifactRef::Checkpoint { name, .. } => name.clone(),
                _ => {
                    problems.push(ArtifactProblem {
                        class: "Corrupt".to_owned(),
                        entry: entry.name.clone(),
                        detail: "a tensor entry is only valid for a checkpoint".to_owned(),
                    });
                    continue;
                }
            };
            match store.restore_named_tensor_at(
                Some(view.snapshot),
                &checkpoint,
                &entry.name,
                None,
                None,
            ) {
                Ok(restored) => {
                    store_checked += 1;
                    if restored.report.restored_content_root != entry.content_root {
                        problems.push(ArtifactProblem {
                            class: "Corrupt".to_owned(),
                            entry: entry.name.clone(),
                            detail: "restored content root does not match the plan".to_owned(),
                        });
                    }
                    if let Some(logical) = entry.logical_root {
                        if restored.report.full_logical_root != logical {
                            problems.push(ArtifactProblem {
                                class: "Corrupt".to_owned(),
                                entry: entry.name.clone(),
                                detail: "restored logical root does not match the plan".to_owned(),
                            });
                        }
                    }
                }
                Err(err) => problems.push(ArtifactProblem {
                    class: err.class_name().to_owned(),
                    entry: entry.name.clone(),
                    detail: err.message().to_owned(),
                }),
            }
            continue;
        }
        let Some(id) = entry.object_id else {
            continue;
        };
        match read_authorized(store, principal, &state, id) {
            Ok(bytes) => {
                store_checked += 1;
                if identity::content_hash(&bytes) != entry.content_root {
                    problems.push(ArtifactProblem {
                        class: "Corrupt".to_owned(),
                        entry: entry.name.clone(),
                        detail: "re-read bytes do not hash to the planned root".to_owned(),
                    });
                }
            }
            Err(err) => problems.push(ArtifactProblem {
                class: err.class_name().to_owned(),
                entry: entry.name.clone(),
                detail: err.message().to_owned(),
            }),
        }
    }
    let mut output_checked = 0u64;
    if let Some(dir) = out {
        let manifest_path = dir.join(".weft-artifact.json");
        match fs::read(&manifest_path) {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(manifest) => {
                    if manifest.get("plan_root").and_then(|value| value.as_str())
                        != Some(plan.plan_root.to_hex().as_str())
                    {
                        problems.push(ArtifactProblem {
                            class: "Corrupt".to_owned(),
                            entry: ".weft-artifact.json".to_owned(),
                            detail: "output manifest names a different plan".to_owned(),
                        });
                    }
                    if let Some(files) = manifest.get("files").and_then(|v| v.as_array()) {
                        for file in files {
                            let Some(name) = file.get("name").and_then(|v| v.as_str()) else {
                                continue;
                            };
                            let Some(expected) = file.get("sha256").and_then(|v| v.as_str()) else {
                                continue;
                            };
                            let path = dir.join(name);
                            match fs::read(&path) {
                                Ok(bytes) => {
                                    output_checked += 1;
                                    let actual = hex::encode(crate::safetensors::sha256(&bytes));
                                    if actual != expected {
                                        problems.push(ArtifactProblem {
                                            class: "Corrupt".to_owned(),
                                            entry: name.to_owned(),
                                            detail: "output file sha256 differs from the manifest"
                                                .to_owned(),
                                        });
                                    }
                                }
                                Err(err) => problems.push(ArtifactProblem {
                                    class: "Io".to_owned(),
                                    entry: name.to_owned(),
                                    detail: err.to_string(),
                                }),
                            }
                        }
                    }
                }
                Err(err) => problems.push(ArtifactProblem {
                    class: "Malformed".to_owned(),
                    entry: ".weft-artifact.json".to_owned(),
                    detail: err.to_string(),
                }),
            },
            Err(err) => problems.push(ArtifactProblem {
                class: "Io".to_owned(),
                entry: ".weft-artifact.json".to_owned(),
                detail: err.to_string(),
            }),
        }
    }
    Ok(ArtifactVerification {
        ok: problems.is_empty(),
        store_checked,
        output_checked,
        problems,
        snapshot: view.snapshot,
        provisional: view.provisional,
        plan_root: plan.plan_root,
    })
}

fn pin_in(
    store: &mut Store,
    view: PinnedView,
    artifact: &ArtifactRef,
    resolution: &ArtifactResolution,
    descriptor: Option<Hash32>,
    policy_epoch: u64,
    declared_lifetime_ms: u64,
) -> Result<PinOutcome> {
    require_pin_snapshot(store, view)?;
    let mut value = None;
    let ((), commit) = store.group(|store| {
        value = Some(pin_marker_and_retention(
            store,
            view,
            artifact,
            resolution,
            descriptor,
            policy_epoch,
            declared_lifetime_ms,
        )?);
        Ok(())
    })?;
    let mut outcome = value.expect("pin published no outcome");
    outcome.commit = commit;
    Ok(outcome)
}

fn pin_marker_and_retention(
    store: &mut Store,
    view: PinnedView,
    artifact: &ArtifactRef,
    resolution: &ArtifactResolution,
    descriptor: Option<Hash32>,
    policy_epoch: u64,
    declared_lifetime_ms: u64,
) -> Result<PinOutcome> {
    let root = pin_root(resolution);
    let marker = ArtifactPinRecord {
        root,
        kind: resolution.kind,
        label: artifact.label(),
        policy_epoch,
        declared_lifetime_ms,
        issued_at_ns: store.clock_now_ns(),
    };
    let bytes = marker.canonical_bytes()?;
    let mut created = false;
    let root_dir = store.resolve_path("/")?;
    let pins = ensure_dir(store, root_dir, PINS_DIR)?;
    let name = root.to_hex();
    match store.lookup(pins, name.as_bytes()) {
        Ok(id) => {
            let existing = store.read_object(id)?;
            if existing != bytes {
                return Err(Error::conflict(format!(
                    "artifact {root} is already pinned with a different record"
                )));
            }
        }
        Err(err) if err.class() == Class::NotFound => {
            let uid = store.options().owner_uid;
            let gid = store.options().owner_gid;
            store.create_file(
                pins,
                name.as_bytes(),
                &bytes,
                0o644,
                uid,
                gid,
                BTreeMap::new(),
            )?;
            created = true;
        }
        Err(err) => return Err(err),
    }
    let retention_binding = match descriptor {
        Some(descriptor_root) => {
            let mut request = RetentionRequest::new(
                descriptor_root,
                RetentionClass::Durable,
                declared_lifetime_ms,
                format!("artifact-pin:{}", root.to_hex()),
            );
            request.policy_epoch = policy_epoch;
            request.issued_at_ns = Some(store.clock_now_ns());
            Some(store.set_retention(request)?)
        }
        None => None,
    };
    Ok(PinOutcome {
        root,
        marker_created: created,
        retention_binding,
        commit: store.head(),
        snapshot: view.snapshot,
    })
}

fn release_in(
    store: &mut Store,
    view: PinnedView,
    artifact: &ArtifactRef,
    resolution: &ArtifactResolution,
    descriptor: Option<Hash32>,
) -> Result<ReleaseOutcome> {
    let mut value = None;
    let ((), commit) = store.group(|store| {
        value = Some(remove_marker_and_rebind(
            store, view, artifact, resolution, descriptor,
        )?);
        Ok(())
    })?;
    let mut outcome = value.expect("release published no outcome");
    outcome.commit = commit;
    Ok(outcome)
}

fn remove_marker_and_rebind(
    store: &mut Store,
    view: PinnedView,
    _artifact: &ArtifactRef,
    resolution: &ArtifactResolution,
    descriptor: Option<Hash32>,
) -> Result<ReleaseOutcome> {
    let root = pin_root(resolution);
    let mut removed = false;
    let root_dir = store.resolve_path("/")?;
    match store.lookup(root_dir, PINS_DIR.as_bytes()) {
        Ok(pins) => match store.lookup(pins, root.to_hex().as_bytes()) {
            Ok(_) => {
                store.unlink(pins, root.to_hex().as_bytes())?;
                removed = true;
            }
            Err(err) if err.class() == Class::NotFound => {}
            Err(err) => return Err(err),
        },
        Err(err) if err.class() == Class::NotFound => {}
        Err(err) => return Err(err),
    }
    let retention_binding = match descriptor {
        Some(descriptor_root) => {
            let epoch = store
                .retention_of(descriptor_root)?
                .map(|existing| existing.policy_epoch.saturating_add(1))
                .unwrap_or(1);
            let mut request = RetentionRequest::new(
                descriptor_root,
                RetentionClass::Ephemeral,
                DEFAULT_RELEASE_LIFETIME_MS,
                format!("artifact-release:{}", root.to_hex()),
            );
            request.policy_epoch = epoch;
            request.issued_at_ns = Some(store.clock_now_ns());
            Some(store.set_retention(request)?)
        }
        None => None,
    };
    Ok(ReleaseOutcome {
        root,
        marker_removed: removed,
        retention_binding,
        reclaimable_after_gc: removed,
        commit: store.head(),
        snapshot: view.snapshot,
    })
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn pin_root(resolution: &ArtifactResolution) -> Hash32 {
    if let Some(id) = resolution.context_root_id {
        return id;
    }
    if let Some(root) = resolution.manifest_root {
        return root;
    }
    resolution
        .entries
        .first()
        .map(|entry| entry.content_root)
        .unwrap_or(Hash32::ZERO)
}

fn checkpoint_entry(tensor: &CheckpointTensor) -> ArtifactEntry {
    let mut dependencies: Vec<Hash32> = tensor
        .shards
        .iter()
        .map(|shard| shard.content_root)
        .collect();
    dependencies.sort_unstable();
    dependencies.dedup();
    let mut dependency_objects: Vec<Hash32> =
        tensor.shards.iter().map(|shard| shard.object_id).collect();
    dependency_objects.sort_unstable();
    dependency_objects.dedup();
    ArtifactEntry {
        name: tensor.name.clone(),
        kind: "tensor".to_owned(),
        object_id: None,
        version_id: None,
        content_id: None,
        content_root: tensor.content_root,
        logical_root: Some(tensor.logical_root),
        bytes: tensor.element_bytes,
        dtype: Some(tensor.dtype.clone()),
        shape: tensor.shape.clone(),
        shards: tensor.shards.len() as u64,
        dependencies,
        dependency_objects,
    }
}

fn dependency_present(state: &State, root: Hash32, exclude: Option<Hash32>) -> bool {
    state.objects.values().any(|record| {
        if Some(record.id) == exclude {
            // The entry that declares a dependency cannot satisfy it.
            return false;
        }
        record.id == root
            || record.version == root
            || record.content == root
            || crate::state::object_causal_closure(record)
                .ok()
                .flatten()
                .map(|closure| closure.closure.contains(&root))
                .unwrap_or(false)
    })
}

fn descriptor_root_of(store: &Store, resolution: &ArtifactResolution) -> Result<Option<Hash32>> {
    for entry in &resolution.entries {
        let Some(id) = entry.object_id else {
            continue;
        };
        let record = store.object_meta(id)?;
        if let Some(Value::Str(text)) = record.attrs.get(crate::kv_capability::ATTR_KV_PROV_ROOT) {
            return Ok(Hash32::from_hex(text).ok());
        }
    }
    Ok(None)
}

fn ensure_dir(store: &mut Store, parent: Hash32, name: &str) -> Result<Hash32> {
    match store.lookup(parent, name.as_bytes()) {
        Ok(id) => {
            if store.object_meta(id)?.kind != ObjectKind::Dir {
                return Err(Error::not_dir(format!("`{name}` is not a directory")));
            }
            Ok(id)
        }
        Err(err) if err.class() == Class::NotFound => {
            let uid = store.options().owner_uid;
            let gid = store.options().owner_gid;
            let seed = identity::h(ARTIFACT_DIR_DOMAIN, name.as_bytes());
            store.mkdir_seeded(parent, name.as_bytes(), 0o755, uid, gid, seed.as_bytes())
        }
        Err(err) => Err(err),
    }
}

fn read_entries(
    store: &Store,
    principal: &Principal,
    plan: &ArtifactPlan,
    state: &State,
) -> Result<Vec<PlannedBytes>> {
    let mut out = Vec::with_capacity(plan.entries.len());
    for entry in &plan.entries {
        let (bytes, kind) = match entry.kind.as_str() {
            "tensor" => {
                return Err(Error::invalid_argument(
                    "a tensor entry cannot be materialized as a file entry",
                ))
            }
            "dir" => (Vec::new(), "dir"),
            "symlink" => {
                let id = entry
                    .object_id
                    .ok_or_else(|| Error::corrupt("symlink entry has no object id"))?;
                let record = state
                    .objects
                    .get(&id)
                    .ok_or_else(|| Error::not_found("symlink object is absent"))?;
                let bytes = record
                    .symlink_target()
                    .ok_or_else(|| Error::corrupt("symlink has no target"))?
                    .to_vec();
                (bytes, "symlink")
            }
            _ => {
                let id = entry
                    .object_id
                    .ok_or_else(|| Error::corrupt("file entry has no object id"))?;
                (read_authorized(store, principal, state, id)?, "file")
            }
        };
        if kind != "dir" && identity::content_hash(&bytes) != entry.content_root {
            return Err(Error::corrupt(format!(
                "materialized bytes for `{}` do not hash to the planned root",
                entry.name
            )));
        }
        out.push(PlannedBytes {
            relative: entry.name.trim_start_matches('/').to_owned(),
            display: entry.name.clone(),
            kind,
            bytes,
        });
    }
    Ok(out)
}

fn read_checkpoint_entries(
    store: &Store,
    view: PinnedView,
    plan: &ArtifactPlan,
    checkpoint: &str,
) -> Result<Vec<PlannedBytes>> {
    let mut out = Vec::with_capacity(plan.entries.len());
    for entry in &plan.entries {
        if entry.kind != "tensor" {
            return Err(Error::corrupt(
                "a checkpoint plan produced a non-tensor entry",
            ));
        }
        // A tensor name becomes a path component. Refuse anything that could
        // escape the output directory instead of sanitising it silently.
        if entry.name.is_empty()
            || entry.name == "."
            || entry.name == ".."
            || entry.name.contains('/')
            || entry.name.contains('\\')
            || entry.name.contains('\0')
        {
            return Err(Error::invalid_name(format!(
                "tensor name `{}` is not usable as a file name",
                entry.name
            )));
        }
        let restored = store.restore_named_tensor_at(
            Some(view.snapshot),
            checkpoint,
            &entry.name,
            None,
            None,
        )?;
        if restored.report.restored_content_root != entry.content_root {
            return Err(Error::corrupt(format!(
                "restored tensor `{}` content root does not match the plan",
                entry.name
            )));
        }
        if let Some(logical) = entry.logical_root {
            if restored.report.full_logical_root != logical {
                return Err(Error::corrupt(format!(
                    "restored tensor `{}` logical root does not match the plan",
                    entry.name
                )));
            }
        }
        out.push(PlannedBytes {
            relative: entry.name.clone(),
            display: entry.name.clone(),
            kind: "tensor",
            bytes: restored.bytes,
        });
    }
    Ok(out)
}

fn read_authorized(
    store: &Store,
    principal: &Principal,
    state: &State,
    id: Hash32,
) -> Result<Vec<u8>> {
    let authorized = store.authorize_read_in(state, id, principal, store.clock_now_ns())?;
    let record = state
        .objects
        .get(&id)
        .ok_or_else(|| Error::not_found("object is absent at the pinned snapshot"))?;
    match record.kind {
        ObjectKind::Dir => Err(Error::is_dir("object is a directory")),
        ObjectKind::Symlink => record
            .symlink_target()
            .map(|bytes| bytes.to_vec())
            .ok_or_else(|| Error::corrupt("symlink object has no target attribute")),
        ObjectKind::File => store.read_object_authorized(&authorized),
    }
}

fn require_complete_plan(plan: &ArtifactPlan) -> Result<()> {
    if plan.complete {
        return Ok(());
    }
    // The dependency list is authorized metadata, so naming the count is not a
    // disclosure. The refusal is typed: a planner that cannot satisfy a
    // declared dependency must not produce a partial artifact silently.
    Err(Error::dependency_mismatch(format!(
        "materialization refused: {} declared dependency root(s) are absent at snapshot {} \
         (the plan is complete=false; no output was published)",
        plan.missing.len(),
        plan.snapshot.to_hex()
    )))
}

fn require_snapshot(store: &Store, snapshot: Hash32) -> Result<()> {
    match store.state_at(snapshot) {
        Ok(_) => Ok(()),
        Err(err) => Err(Error::snapshot_missing(format!(
            "snapshot {} is not replayable in this store: {}",
            snapshot.to_hex(),
            err.message()
        ))),
    }
}

fn state_for(store: &Store, view: PinnedView) -> Result<State> {
    match store.state_at(view.snapshot) {
        Ok(state) => Ok(state),
        Err(err) => Err(Error::snapshot_missing(format!(
            "snapshot {} is not replayable in this store: {}",
            view.snapshot.to_hex(),
            err.message()
        ))),
    }
}

fn require_pin_snapshot(store: &Store, view: PinnedView) -> Result<()> {
    if view.provisional {
        return Err(Error::invalid_argument(
            "pinning requires a named or hashed snapshot; a provisional head pin would move",
        ));
    }
    require_snapshot(store, view.snapshot)
}

#[derive(Clone)]
struct PlannedBytes {
    relative: String,
    display: String,
    kind: &'static str,
    bytes: Vec<u8>,
}

/// Canonical pin marker. It is an ordinary store file, so it snapshots,
/// replicates and recovers with everything else, and it is reachable from the
/// head, which makes it a GC root.
#[derive(Clone, Debug)]
struct ArtifactPinRecord {
    root: Hash32,
    kind: &'static str,
    label: String,
    policy_epoch: u64,
    declared_lifetime_ms: u64,
    issued_at_ns: i64,
}

impl ArtifactPinRecord {
    fn to_value(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert(
            "schema".to_owned(),
            Value::Str(ARTIFACT_PIN_SCHEMA.to_owned()),
        );
        map.insert("root".to_owned(), Value::Str(self.root.to_hex()));
        map.insert("artifact_kind".to_owned(), Value::Str(self.kind.to_owned()));
        map.insert("label".to_owned(), Value::Str(self.label.clone()));
        map.insert("policy_epoch".to_owned(), Value::U64(self.policy_epoch));
        map.insert(
            "declared_lifetime_ms".to_owned(),
            Value::U64(self.declared_lifetime_ms),
        );
        map.insert(
            "issued_at_ns".to_owned(),
            Value::U64(self.issued_at_ns.max(0) as u64),
        );
        Value::Map(map)
    }

    fn canonical_bytes(&self) -> Result<Vec<u8>> {
        wcb::encode(&self.to_value(), &Limits::default()).map_err(Error::from)
    }
}

fn plan_root(
    snapshot: Hash32,
    provisional: bool,
    kind: &str,
    entries: &[ArtifactEntry],
    dependencies: &[Hash32],
    missing: &[MissingDependency],
    complete: bool,
) -> Result<Hash32> {
    let mut map = BTreeMap::new();
    map.insert(
        "schema".to_owned(),
        Value::Str(ARTIFACT_PLAN_SCHEMA.to_owned()),
    );
    map.insert("snapshot".to_owned(), Value::Str(snapshot.to_hex()));
    map.insert("provisional".to_owned(), Value::Bool(provisional));
    map.insert("kind".to_owned(), Value::Str(kind.to_owned()));
    map.insert("complete".to_owned(), Value::Bool(complete));
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut row = BTreeMap::new();
        row.insert("name".to_owned(), Value::Str(entry.name.clone()));
        row.insert("entry_kind".to_owned(), Value::Str(entry.kind.clone()));
        row.insert(
            "content_root".to_owned(),
            Value::Str(entry.content_root.to_hex()),
        );
        if let Some(logical) = entry.logical_root {
            row.insert("logical_root".to_owned(), Value::Str(logical.to_hex()));
        }
        row.insert("bytes".to_owned(), Value::U64(entry.bytes));
        if let Some(dtype) = &entry.dtype {
            row.insert("dtype".to_owned(), Value::Str(dtype.clone()));
        }
        if !entry.shape.is_empty() {
            row.insert(
                "shape".to_owned(),
                Value::Array(entry.shape.iter().map(|d| Value::U64(*d)).collect()),
            );
        }
        let mut deps = entry.dependencies.clone();
        deps.sort_unstable();
        deps.dedup();
        if !deps.is_empty() {
            row.insert(
                "dependencies".to_owned(),
                Value::Array(deps.iter().map(|d| Value::Str(d.to_hex())).collect()),
            );
        }
        rows.push(Value::Map(row));
    }
    map.insert("entries".to_owned(), Value::Array(rows));
    if !dependencies.is_empty() {
        map.insert(
            "dependencies".to_owned(),
            Value::Array(
                dependencies
                    .iter()
                    .map(|d| Value::Str(d.to_hex()))
                    .collect(),
            ),
        );
    }
    if !missing.is_empty() {
        map.insert(
            "missing".to_owned(),
            Value::Array(
                missing
                    .iter()
                    .map(|missing| {
                        let mut row = BTreeMap::new();
                        row.insert("root".to_owned(), Value::Str(missing.root.to_hex()));
                        row.insert("reason".to_owned(), Value::Str(missing.reason.clone()));
                        Value::Map(row)
                    })
                    .collect(),
            ),
        );
    }
    let bytes = wcb::encode(&Value::Map(map), &Limits::default()).map_err(Error::from)?;
    Ok(identity::h(ARTIFACT_PLAN_DOMAIN, &bytes))
}

fn publish_materialization(
    view: PinnedView,
    plan: &ArtifactPlan,
    entries: &[PlannedBytes],
    out: &Path,
    kind: &str,
) -> Result<MaterializeOutcome> {
    if entries.len() > MAX_ARTIFACT_ENTRIES {
        return Err(Error::limit_exceeded(format!(
            "materialization enumerates more than {MAX_ARTIFACT_ENTRIES} entries"
        )));
    }
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", parent.display())))?;
    }
    let tmp = sibling_tmp(out)?;
    if tmp.exists() {
        fs::remove_dir_all(&tmp)
            .map_err(|err| Error::io(format!("cannot clear {}: {err}", tmp.display())))?;
    }
    fs::create_dir(&tmp)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", tmp.display())))?;
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in entries {
        if entry.kind == "dir" {
            let path = tmp.join(&entry.relative);
            fs::create_dir_all(&path).map_err(|err| {
                Error::io(format!("cannot create directory {}: {err}", path.display()))
            })?;
            files.push(serde_json::json!({
                "name": entry.relative,
                "display_path": entry.display,
                "kind": "dir",
                "sha256": hex::encode(crate::safetensors::sha256(b"")),
                "bytes": 0,
            }));
            continue;
        }
        let path = tmp.join(&entry.relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| Error::io(format!("cannot create {}: {err}", parent.display())))?;
        }
        let mut file = fs::File::create(&path)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", path.display())))?;
        file.write_all(&entry.bytes)
            .map_err(|err| Error::io(format!("cannot write {}: {err}", path.display())))?;
        file.sync_all()
            .map_err(|err| Error::io(format!("cannot sync {}: {err}", path.display())))?;
        total = total.saturating_add(entry.bytes.len() as u64);
        files.push(serde_json::json!({
            "name": entry.relative,
            "display_path": entry.display,
            "kind": entry.kind,
            "sha256": hex::encode(crate::safetensors::sha256(&entry.bytes)),
            "bytes": entry.bytes.len(),
        }));
    }
    let manifest_path = tmp.join(".weft-artifact.json");
    let manifest = serde_json::json!({
        "schema": ARTIFACT_MANIFEST_SCHEMA,
        "artifact_kind": kind,
        "plan_root": plan.plan_root.to_hex(),
        "snapshot": plan.snapshot.to_hex(),
        "provisional": plan.provisional,
        "complete": plan.complete,
        "files": files,
    });
    let encoded = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| Error::io(format!("cannot encode artifact manifest: {err}")))?;
    let mut manifest_file = fs::File::create(&manifest_path)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", manifest_path.display())))?;
    manifest_file
        .write_all(&encoded)
        .map_err(|err| Error::io(format!("cannot write {}: {err}", manifest_path.display())))?;
    manifest_file
        .sync_all()
        .map_err(|err| Error::io(format!("cannot sync {}: {err}", manifest_path.display())))?;

    if out.exists() {
        if !out.is_dir() {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::exists(format!(
                "materialization output {} exists and is not a directory",
                out.display()
            )));
        }
        let mut items = fs::read_dir(out)
            .map_err(|err| Error::io(format!("cannot read {}: {err}", out.display())))?;
        if items.next().is_some() {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::exists(format!(
                "materialization output {} is not empty",
                out.display()
            )));
        }
        fs::remove_dir(out)
            .map_err(|err| Error::io(format!("cannot replace {}: {err}", out.display())))?;
    }
    fs::rename(&tmp, out).map_err(|err| {
        let _ = fs::remove_dir_all(&tmp);
        Error::io(format!(
            "cannot publish materialization at {}: {err}",
            out.display()
        ))
    })?;
    let file_digests = manifest["files"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    (
                        row["name"].as_str().unwrap_or_default().to_owned(),
                        row["sha256"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(MaterializeOutcome {
        out: out.to_path_buf(),
        manifest_path: out.join(".weft-artifact.json"),
        files: entries.len() as u64,
        bytes: total,
        snapshot: view.snapshot,
        provisional: view.provisional,
        plan_root: plan.plan_root,
        file_digests,
    })
}

fn sibling_tmp(out: &Path) -> Result<PathBuf> {
    let name = out
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::invalid_argument("output path has no final component"))?;
    let parent = out
        .parent()
        .ok_or_else(|| Error::invalid_argument("output path has no parent"))?;
    Ok(parent.join(format!(".{name}.weft-artifact-tmp-{}", std::process::id())))
}
