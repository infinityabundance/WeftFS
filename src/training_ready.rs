//! "Training-ready" is a storage refusal, not a promise.
//!
//! A Context Root may be materialized only when every precondition in
//! `spec/WEFT-TRAINING-READY.md` holds. The checks run against the pinned
//! snapshot and read every byte through the store's ordinary verified read
//! path, so the proof is that hashing and decoding happened, not that a field
//! was trusted. No output directory or partial file is created on failure.
//!
//! Output format (canonical WCB only; no JSON files are written into the
//! output directory):
//!
//! ```text
//! <out>/<store path>                 verified decoded bytes
//! <out>/manifest.wcb                 weft.training_ready.manifest.v1
//! <out>/training-ready.wcb           weft.training_ready.v1
//! ```

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::json;
use weftcodec::tensor::{self as tc, PhysicalTensor};
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::auth::{can_read, Principal};
use crate::checkpoint::{
    CheckpointManifest, CHECKPOINT_MANIFEST_NAME, CHECKPOINT_ROOT, CHECKPOINT_SCHEMA,
};
use crate::context::{self, CapabilityContext, ContextResolution};
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::recovery;
use crate::representation::TensorElementDtype;
use crate::state::{object_causal_closure, ObjectKind, ObjectRecord, State, DEFAULT_NAMESPACE};
use crate::store::Store;
use crate::wcb_util as wu;

/// Schema of the per-file manifest written as `manifest.wcb`.
pub const TRAINING_READY_MANIFEST_SCHEMA: &str = "weft.training_ready.manifest.v1";
/// Schema of the summary record written as `training-ready.wcb`.
pub const TRAINING_READY_SCHEMA: &str = "weft.training_ready.v1";
/// Per-file manifest file name.
pub const MANIFEST_FILE: &str = "manifest.wcb";
/// Summary record file name.
pub const RECORD_FILE: &str = "training-ready.wcb";
/// Assumed verification throughput for the pre-write decode estimate.
pub const ESTIMATE_BYTES_PER_SECOND: u64 = 32 * 1024 * 1024;
/// Per-tensor-object estimate overhead.
pub const ESTIMATE_TENSOR_MS: u64 = 1;

static MATERIALIZE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Optional cost envelope and tensor requirement for a training-ready run.
#[derive(Clone, Debug, Default)]
pub struct TrainingReadyOptions {
    pub max_bytes: Option<u64>,
    pub max_objects: Option<u64>,
    pub max_decode_ms: Option<u64>,
    pub require_tensors: bool,
}

/// One checked precondition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreconditionReport {
    pub resolution_exact: bool,
    pub content_verified: bool,
    pub causal_complete: bool,
    pub authorized: bool,
    pub tensors_verified: bool,
    pub budget_enforced: bool,
}

/// Estimated and actual cost of the verification pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrainingReadyCost {
    pub max_bytes: Option<u64>,
    pub max_objects: Option<u64>,
    pub max_decode_ms: Option<u64>,
    pub estimated_bytes: u64,
    pub estimated_objects: u64,
    pub estimated_decode_ms: u64,
    pub actual_bytes: u64,
    pub actual_objects: u64,
    pub actual_decode_ms: u64,
}

/// Tensor identity of one materialized file, when the file is a tensor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingReadyTensor {
    pub dtype: String,
    pub shape: Vec<u64>,
    pub logical_root: Hash32,
    /// `object` (the object's own identity) or `checkpoint` (the logical tensor
    /// identity recorded by a `weft.checkpoint.v1` manifest).
    pub source: String,
    pub layout: String,
    pub strategy: String,
    pub checkpoint: Option<String>,
    pub tensor_name: Option<String>,
    /// False for a q8_0 byte-transport object, whose own root cannot be
    /// recomputed from decoded bytes alone (the checkpoint's logical shard root
    /// is recorded instead).
    pub offline_verifiable: bool,
}

/// Checkpoint metadata attached to a manifest file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingReadyCheckpoint {
    pub name: String,
    pub manifest_root: Hash32,
    pub complete: bool,
    pub source_sha256: Option<[u8; 32]>,
    pub tensors: u64,
}

/// One materialized file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainingReadyFile {
    /// Store path, beginning with `/`.
    pub path: String,
    pub kind: String,
    pub object_id: Hash32,
    pub version_id: Hash32,
    /// `ObjectRecord::content` (manifest / content address).
    pub content_root: Hash32,
    /// `H("weft/chunk/v1", decoded bytes)`.
    pub content_hash: Hash32,
    pub content_identity: String,
    pub size: u64,
    pub causal_closure: Vec<Hash32>,
    pub tensor: Option<TrainingReadyTensor>,
    pub checkpoint: Option<TrainingReadyCheckpoint>,
    /// Why the object is in the set: `result`, `causal`, or `checkpoint`.
    pub origin: String,
}

/// Result of a check or a materialization.
#[derive(Clone, Debug)]
pub struct TrainingReadyReport {
    pub out: Option<PathBuf>,
    pub manifest_path: Option<PathBuf>,
    pub record_path: Option<PathBuf>,
    pub context_root_id: Hash32,
    pub context_root_name: String,
    pub uri: String,
    pub snapshot_id: Hash32,
    pub selector_id: Hash32,
    pub capability_hash: Option<Hash32>,
    pub declared_closure: Vec<Hash32>,
    pub preconditions: PreconditionReport,
    pub files: Vec<TrainingReadyFile>,
    pub cost: TrainingReadyCost,
    pub tensor_objects_verified: u64,
    pub checkpoint_tensors_verified: u64,
    pub checkpoints: Vec<TrainingReadyCheckpoint>,
}

/// Result of verifying an already materialized directory.
#[derive(Clone, Debug)]
pub struct TrainingReadyVerification {
    pub files: u64,
    pub bytes: u64,
    pub tensor_roots_verified: u64,
    pub checkpoints_verified: u64,
    pub checkpoint_tensors_verified: u64,
    pub context_root_id: Option<Hash32>,
    pub snapshot_id: Option<Hash32>,
}

#[derive(Clone, Debug)]
struct Candidate {
    origin: &'static str,
}

#[derive(Clone, Debug)]
struct FileFacts {
    path: String,
    kind: ObjectKind,
    content_root: Hash32,
    content_hash: Hash32,
    content_identity: String,
    size: u64,
    causal_closure: Vec<Hash32>,
    tensor: Option<TrainingReadyTensor>,
    checkpoint: Option<TrainingReadyCheckpoint>,
    object_id: Hash32,
    version_id: Hash32,
    origin: &'static str,
}

impl Store {
    /// Run every training-ready precondition and report without writing.
    pub fn check_training_ready(
        &self,
        root: &str,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        limits: &Limits,
        now_ns: i64,
        options: &TrainingReadyOptions,
    ) -> Result<TrainingReadyReport> {
        self.training_ready(root, None, capability, principal, limits, now_ns, options)
    }

    /// Verify every precondition, then atomically publish an output directory.
    ///
    /// The output directory must not exist. Every check happens before the
    /// temporary directory is created; any failure removes it, so a caller never
    /// observes a partial materialization.
    pub fn materialize_training_ready(
        &self,
        root: &str,
        out: &Path,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        limits: &Limits,
        now_ns: i64,
        options: &TrainingReadyOptions,
    ) -> Result<TrainingReadyReport> {
        if out.exists() {
            return Err(Error::exists(format!(
                "training-ready output already exists: {}",
                out.display()
            )));
        }
        self.training_ready(
            root,
            Some(out),
            capability,
            principal,
            limits,
            now_ns,
            options,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn training_ready(
        &self,
        root: &str,
        out: Option<&Path>,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        limits: &Limits,
        now_ns: i64,
        options: &TrainingReadyOptions,
    ) -> Result<TrainingReadyReport> {
        // (a) Resolve at the pinned snapshot. This recomputes the ContextRootId,
        // verifies the snapshot, applies every capability, and evaluates the
        // selector over the authorized universe.
        let resolved = self.resolve_context(root, capability, principal, limits, now_ns)?;
        let snapshot = resolved.resolution.snapshot_id;
        if resolved.truncated
            || resolved.resolution.matched_count != resolved.resolution.results.len() as u64
        {
            return Err(Error::corrupt(format!(
                "context resolution is not an exact result set (matched {}, returned {})",
                resolved.resolution.matched_count,
                resolved.resolution.results.len()
            )));
        }
        let state = self.state_at(snapshot)?;
        if state.objects.get(&State::ROOT_ID).is_none() {
            return Err(Error::corrupt("pinned snapshot has no namespace root"));
        }

        // Candidate set: the exact result set, the declared causal closure, and
        // checkpoint shard objects named by any checkpoint manifest in the set.
        let mut candidates: BTreeMap<Hash32, Candidate> = BTreeMap::new();
        for result in &resolved.resolution.results {
            if result.object_id == State::ROOT_ID {
                // The namespace root has no materializable path.
                continue;
            }
            if !state.objects.contains_key(&result.object_id) {
                return Err(Error::corrupt(
                    "selector result has no object record at the pinned snapshot",
                ));
            }
            candidates
                .entry(result.object_id)
                .or_insert(Candidate { origin: "result" });
        }
        let closure_walk = self.walk_causal_closure(&state, &resolved, limits)?;
        for id in closure_walk {
            candidates
                .entry(id)
                .or_insert(Candidate { origin: "causal" });
        }

        // Expand checkpoint manifests before estimating: a manifest names shard
        // objects that must be part of the materialization.
        let mut checkpoint_manifests: Vec<(String, CheckpointManifest, Hash32)> = Vec::new();
        let mut shard_tensors: BTreeMap<Hash32, TrainingReadyTensor> = BTreeMap::new();
        let mut pending: Vec<Hash32> = candidates.keys().copied().collect();
        while let Some(id) = pending.pop() {
            let record = match state.objects.get(&id) {
                Some(record) => record,
                None => continue,
            };
            if record.kind != ObjectKind::File || !is_checkpoint_manifest(record) {
                continue;
            }
            let path = object_path(&state, id);
            let name = checkpoint_name_from_path(&path).ok_or_else(|| {
                Error::corrupt(format!(
                    "checkpoint manifest {id} is not at /{CHECKPOINT_ROOT}/<name>/{CHECKPOINT_MANIFEST_NAME}"
                ))
            })?;
            let bytes = self.read_object_at(Some(snapshot), id)?;
            let manifest = CheckpointManifest::decode(&bytes)?;
            let manifest_root = manifest.root()?;
            let recorded = record.attrs.get("weft.checkpoint.id").and_then(|value| {
                if let Value::Bytes(bytes) = value {
                    Hash32::from_slice(bytes).ok()
                } else {
                    None
                }
            });
            if recorded != Some(manifest_root) {
                return Err(Error::corrupt(format!(
                    "checkpoint manifest {id} root does not match its recorded identity"
                )));
            }
            for tensor in &manifest.tensors {
                for shard in &tensor.shards {
                    let shard_id = resolve_in_state(
                        &state,
                        &[
                            CHECKPOINT_ROOT.as_bytes(),
                            name.as_bytes(),
                            shard.object_name.as_bytes(),
                        ],
                    )
                    .map_err(|err| {
                        Error::new(
                            err.class(),
                            format!(
                                "checkpoint `{name}` shard `{}` is not resolvable at the pinned \
                                 snapshot: {}",
                                shard.object_name,
                                err.message()
                            ),
                        )
                    })?;
                    if shard_id != shard.object_id {
                        return Err(Error::corrupt(format!(
                            "checkpoint `{name}` shard `{}` resolves to object {shard_id}, \
                             manifest records {}",
                            shard.object_name, shard.object_id
                        )));
                    }
                    let shard_shape =
                        tc::shard_shape(&tensor.shape, shard.axis, shard.shards, shard.index)?;
                    shard_tensors.insert(
                        shard_id,
                        TrainingReadyTensor {
                            dtype: tensor.dtype.clone(),
                            shape: shard_shape,
                            logical_root: shard.logical_root,
                            source: "checkpoint".to_owned(),
                            layout: "checkpoint-shard".to_owned(),
                            strategy: "checkpoint-shard".to_owned(),
                            checkpoint: Some(name.clone()),
                            tensor_name: Some(tensor.name.clone()),
                            offline_verifiable: true,
                        },
                    );
                    if candidates
                        .insert(
                            shard_id,
                            Candidate {
                                origin: "checkpoint",
                            },
                        )
                        .is_none()
                    {
                        pending.push(shard_id);
                    }
                }
            }
            checkpoint_manifests.push((name, manifest, manifest_root));
        }

        // (d) Capability authorization for every object in the closure.
        self.authorize_training_ready(
            &state,
            &resolved,
            &candidates,
            capability,
            principal,
            limits,
            now_ns,
        )?;

        // (f) Estimate before reading any file bytes for the materialization and
        // before writing anything at all.
        let mut estimated_bytes: u64 = 0;
        let mut estimated_objects: u64 = 0;
        let mut tensor_estimate: u64 = 0;
        for id in candidates.keys() {
            let record = state
                .objects
                .get(id)
                .ok_or_else(|| Error::corrupt("candidate has no object record"))?;
            estimated_objects += 1;
            estimated_bytes = estimated_bytes.saturating_add(record.size);
            if shard_tensors.contains_key(id) || self.tensor_metadata_at(&state, *id).is_ok() {
                tensor_estimate += 1;
            }
        }
        let estimated_decode_ms = estimated_bytes
            .saturating_mul(1000)
            .div_ceil(ESTIMATE_BYTES_PER_SECOND)
            .saturating_add(tensor_estimate.saturating_mul(ESTIMATE_TENSOR_MS));
        let mut cost = TrainingReadyCost {
            max_bytes: options.max_bytes,
            max_objects: options.max_objects,
            max_decode_ms: options.max_decode_ms,
            estimated_bytes,
            estimated_objects,
            estimated_decode_ms,
            ..TrainingReadyCost::default()
        };
        enforce_budget(&cost, options)?;

        // (b)(c)(e) Read and verify every candidate. This is the proof pass.
        let started = Instant::now();
        let mut facts: Vec<FileFacts> = Vec::with_capacity(candidates.len());
        let mut actual_bytes: u64 = 0;
        let mut tensor_objects_verified: u64 = 0;
        for id in candidates.keys() {
            let record = state
                .objects
                .get(id)
                .ok_or_else(|| Error::corrupt("candidate has no object record"))?;
            let path = object_path(&state, *id);
            let causal_closure = object_causal_closure(record)?
                .map(|closure| closure.closure)
                .unwrap_or_default();
            let (content_hash, actual_size, content_identity) = match record.kind {
                ObjectKind::Dir => {
                    let empty = identity::content_hash(b"");
                    if record.content != empty && record.size != 0 {
                        return Err(Error::corrupt(format!(
                            "directory {id} has a non-empty content identity"
                        )));
                    }
                    (empty, 0u64, "directory".to_owned())
                }
                ObjectKind::Symlink => {
                    let bytes = record
                        .symlink_target()
                        .ok_or_else(|| Error::corrupt("symlink has no target"))?;
                    let digest = identity::content_hash(bytes);
                    if digest != record.content {
                        return Err(Error::corrupt(format!(
                            "symlink {id} content hash does not match its record"
                        )));
                    }
                    (digest, bytes.len() as u64, "symlink".to_owned())
                }
                ObjectKind::File => {
                    // The verified read path checks every chunk, a v3 Merkle
                    // root, or a v2 representation decode, plus the whole-file
                    // digest. Nothing here trusts a stored hash.
                    let bytes = self.read_object_at(Some(snapshot), *id)?;
                    if bytes.len() as u64 != record.size {
                        return Err(Error::corrupt(format!(
                            "object {id} read {} bytes but its record declares {}",
                            bytes.len(),
                            record.size
                        )));
                    }
                    let manifest = self.with_backend(|backend| {
                        recovery::read_manifest_via(backend, record.content)
                    })?;
                    let identity_name = manifest.identity.as_str().to_owned();
                    (
                        identity::content_hash(&bytes),
                        bytes.len() as u64,
                        identity_name,
                    )
                }
            };

            let mut tensor = None;
            if record.kind == ObjectKind::File {
                match self.tensor_metadata_at(&state, *id) {
                    Ok(meta) => {
                        tensor = Some(TrainingReadyTensor {
                            dtype: meta.dtype.as_str().to_owned(),
                            shape: meta.shape.clone(),
                            logical_root: meta.logical_root,
                            source: "object".to_owned(),
                            layout: meta.layout.name().to_owned(),
                            strategy: meta.strategy,
                            checkpoint: None,
                            tensor_name: None,
                            offline_verifiable: meta.dtype != TensorElementDtype::Q8_0,
                        });
                    }
                    Err(err) if err.class() == Class::NotFound => {}
                    Err(err) => return Err(err),
                }
                if let Some(from_checkpoint) = shard_tensors.get(id) {
                    tensor = Some(from_checkpoint.clone());
                }
                if options.require_tensors && tensor.is_some() {
                    self.tensor_logical_root_at(&state, snapshot, *id)?;
                    tensor_objects_verified += 1;
                }
            }

            actual_bytes = actual_bytes.saturating_add(actual_size);
            facts.push(FileFacts {
                path,
                kind: record.kind,
                content_root: record.content,
                content_hash,
                content_identity,
                size: actual_size,
                causal_closure,
                tensor,
                checkpoint: None,
                object_id: record.id,
                version_id: record.version,
                origin: candidates[id].origin,
            });
        }

        // (e) Full checkpoint verification: every shard decoded and every
        // per-shard and per-tensor logical root recomputed, at the snapshot.
        let mut checkpoint_tensors_verified = 0u64;
        let mut checkpoints = Vec::new();
        for (name, manifest, manifest_root) in &checkpoint_manifests {
            let verified = crate::checkpoint::verify_checkpoint_at(self, snapshot, name)?;
            if verified != manifest.tensors.len() {
                return Err(Error::corrupt(format!(
                    "checkpoint `{name}` verified {verified} tensors, manifest declares {}",
                    manifest.tensors.len()
                )));
            }
            checkpoint_tensors_verified += verified as u64;
            let info = TrainingReadyCheckpoint {
                name: name.clone(),
                manifest_root: *manifest_root,
                complete: manifest.complete,
                source_sha256: manifest.source_sha256,
                tensors: manifest.tensors.len() as u64,
            };
            checkpoints.push(info.clone());
            for fact in &mut facts {
                if fact.path == format!("/{CHECKPOINT_ROOT}/{name}/{CHECKPOINT_MANIFEST_NAME}") {
                    fact.checkpoint = Some(info.clone());
                }
            }
        }

        cost.actual_bytes = actual_bytes;
        cost.actual_objects = facts.len() as u64;
        cost.actual_decode_ms = started.elapsed().as_millis() as u64;
        enforce_budget(&cost, options)?;

        facts.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        for pair in facts.windows(2) {
            if pair[0].path == pair[1].path {
                return Err(Error::corrupt(format!(
                    "training-ready path `{}` is not unique",
                    pair[0].path
                )));
            }
        }

        let preconditions = PreconditionReport {
            resolution_exact: true,
            content_verified: true,
            causal_complete: true,
            authorized: true,
            tensors_verified: true,
            budget_enforced: true,
        };

        let mut report = TrainingReadyReport {
            out: None,
            manifest_path: None,
            record_path: None,
            context_root_id: resolved.stored.context_root_id,
            context_root_name: resolved.stored.name.clone(),
            uri: resolved.stored.uri.clone(),
            snapshot_id: snapshot,
            selector_id: resolved.resolution.selector_id,
            capability_hash: resolved.capability_hash,
            declared_closure: resolved.stored.root.causal.clone(),
            preconditions,
            files: facts
                .into_iter()
                .map(|fact| TrainingReadyFile {
                    path: fact.path,
                    kind: fact.kind.as_str().to_owned(),
                    object_id: fact.object_id,
                    version_id: fact.version_id,
                    content_root: fact.content_root,
                    content_hash: fact.content_hash,
                    content_identity: fact.content_identity,
                    size: fact.size,
                    causal_closure: fact.causal_closure,
                    tensor: fact.tensor,
                    checkpoint: fact.checkpoint,
                    origin: fact.origin.to_owned(),
                })
                .collect(),
            cost,
            tensor_objects_verified,
            checkpoint_tensors_verified,
            checkpoints,
        };

        if let Some(out) = out {
            let (manifest_path, record_path) =
                self.write_training_ready(&state, snapshot, out, &report)?;
            report.out = Some(out.to_path_buf());
            report.manifest_path = Some(manifest_path);
            report.record_path = Some(record_path);
        }
        Ok(report)
    }

    fn walk_causal_closure(
        &self,
        state: &State,
        resolved: &ContextResolution,
        limits: &Limits,
    ) -> Result<Vec<Hash32>> {
        let mut version_to_objects: BTreeMap<Hash32, Vec<Hash32>> = BTreeMap::new();
        let mut content_to_objects: BTreeMap<Hash32, Vec<Hash32>> = BTreeMap::new();
        for record in state.objects.values() {
            version_to_objects
                .entry(record.version)
                .or_default()
                .push(record.id);
            content_to_objects
                .entry(record.content)
                .or_default()
                .push(record.id);
        }
        let max_entries = usize::try_from(limits.max_causal_entries)
            .unwrap_or(usize::MAX)
            .max(1);
        let mut queue: VecDeque<Hash32> = resolved.stored.root.causal.iter().copied().collect();
        let mut visited: BTreeSet<Hash32> = BTreeSet::new();
        let mut out = Vec::new();
        while let Some(identity) = queue.pop_front() {
            if identity == Hash32::ZERO {
                return Err(Error::corrupt(
                    "declared causal closure contains the zero hash",
                ));
            }
            if !visited.insert(identity) {
                continue;
            }
            let id = resolve_identity(state, &version_to_objects, &content_to_objects, identity)?
                .ok_or_else(|| {
                Error::dependency_mismatch(format!(
                    "causal parent {identity} is missing from the pinned snapshot"
                ))
            })?;
            if id == State::ROOT_ID {
                continue;
            }
            if !out.contains(&id) {
                out.push(id);
            }
            let record = state
                .objects
                .get(&id)
                .ok_or_else(|| Error::corrupt("causal parent has no object record"))?;
            if let Some(closure) = object_causal_closure(record)? {
                for parent in closure.parents.into_iter().chain(closure.closure) {
                    queue.push_back(parent);
                }
            }
            if visited.len() > max_entries {
                return Err(Error::limit_exceeded(format!(
                    "causal closure exceeds max_causal_entries ({max_entries})"
                )));
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn authorize_training_ready(
        &self,
        state: &State,
        resolved: &ContextResolution,
        candidates: &BTreeMap<Hash32, Candidate>,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        limits: &Limits,
        now_ns: i64,
    ) -> Result<()> {
        for id in candidates.keys() {
            let record = state
                .objects
                .get(id)
                .ok_or_else(|| Error::corrupt("candidate has no object record"))?;
            if !can_read(record, principal) {
                return Err(Error::unauthorized(format!(
                    "principal cannot read object {id} in the training-ready closure"
                )));
            }
        }

        let envelope_capability = context::principal_capability(principal)?;
        let mut capabilities: Vec<&CapabilityContext> = Vec::new();
        if let Some(cap) = capability {
            capabilities.push(cap);
        }
        if let Some(cap) = envelope_capability.as_ref() {
            capabilities.push(cap);
        }
        if capabilities.is_empty() {
            return Ok(());
        }
        for cap in &capabilities {
            cap.check_valid_at(now_ns)?;
            if !context::selector_predicates_allowed(&resolved.selector, cap) {
                return Err(Error::unauthorized(
                    "capability does not grant every relation predicate in the selector",
                ));
            }
        }

        let contexts = self.list_context_roots_at(state)?;
        let mut namespace_wide = false;
        let mut authorized: BTreeSet<Hash32> = BTreeSet::new();
        for cap in &capabilities {
            if cap.allows_namespace(DEFAULT_NAMESPACE) {
                namespace_wide = true;
                continue;
            }
            for root_id in &cap.scope.roots {
                let Some(stored) = contexts
                    .iter()
                    .find(|stored| stored.context_root_id == *root_id)
                else {
                    continue;
                };
                if let Some(bound) = stored.root.capability_hash {
                    if bound != cap.hash {
                        continue;
                    }
                }
                match self.resolve_context_at(
                    state,
                    &stored.name,
                    Some(cap),
                    principal,
                    limits,
                    now_ns,
                ) {
                    Ok(resolution) => {
                        authorized.extend(
                            resolution
                                .resolution
                                .results
                                .iter()
                                .map(|result| result.object_id),
                        );
                    }
                    Err(err) if err.class() == Class::Unauthorized => {}
                    Err(err) => return Err(err),
                }
            }
        }
        if namespace_wide {
            return Ok(());
        }
        for id in candidates.keys() {
            if !authorized.contains(id) {
                return Err(Error::unauthorized(format!(
                    "capability does not authorize object {id} in the training-ready closure"
                )));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn write_training_ready(
        &self,
        state: &State,
        snapshot: Hash32,
        out: &Path,
        report: &TrainingReadyReport,
    ) -> Result<(PathBuf, PathBuf)> {
        if out.exists() {
            return Err(Error::exists(format!(
                "training-ready output already exists: {}",
                out.display()
            )));
        }
        let parent = out.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", parent.display())))?;
        let base = out
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "training-ready".to_owned());
        let tmp = parent.join(format!(
            ".{base}.weft-training-tmp-{}-{}",
            std::process::id(),
            MATERIALIZE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if tmp.exists() {
            let _ = fs::remove_dir_all(&tmp);
        }
        fs::create_dir(&tmp)
            .map_err(|err| Error::io(format!("cannot create {}: {err}", tmp.display())))?;

        let result = (|| -> Result<(PathBuf, PathBuf)> {
            for file in &report.files {
                let relative = relative_output_path(&file.path)?;
                let target = tmp.join(&relative);
                match file.kind.as_str() {
                    "dir" => {
                        fs::create_dir_all(&target).map_err(|err| {
                            Error::io(format!("cannot create {}: {err}", target.display()))
                        })?;
                    }
                    "symlink" => {
                        let record = state
                            .objects
                            .get(&file.object_id)
                            .ok_or_else(|| Error::corrupt("symlink candidate vanished"))?;
                        let target_bytes = record
                            .symlink_target()
                            .ok_or_else(|| Error::corrupt("symlink has no target"))?;
                        if let Some(parent) = target.parent() {
                            fs::create_dir_all(parent).map_err(|err| {
                                Error::io(format!("cannot create {}: {err}", parent.display()))
                            })?;
                        }
                        symlink(
                            std::str::from_utf8(target_bytes).map_err(|_| {
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
                        let bytes = self.read_object_at(Some(snapshot), file.object_id)?;
                        if bytes.len() as u64 != file.size {
                            return Err(Error::corrupt(format!(
                                "object {} changed size between verification and write",
                                file.object_id
                            )));
                        }
                        if identity::content_hash(&bytes) != file.content_hash {
                            return Err(Error::corrupt(format!(
                                "object {} changed content between verification and write",
                                file.object_id
                            )));
                        }
                        let mut handle = fs::File::create(&target).map_err(|err| {
                            Error::io(format!("cannot create {}: {err}", target.display()))
                        })?;
                        handle.write_all(&bytes)?;
                        handle.sync_all()?;
                    }
                }
            }

            let manifest_path = tmp.join(MANIFEST_FILE);
            write_wcb(&manifest_path, &training_ready_manifest_value(report))?;
            let record_path = tmp.join(RECORD_FILE);
            write_wcb(&record_path, &training_ready_record_value(report))?;
            if let Ok(dir) = fs::File::open(&tmp) {
                let _ = dir.sync_all();
            }
            Ok((manifest_path, record_path))
        })();

        let (_manifest_path, _record_path) = match result {
            Ok(paths) => paths,
            Err(err) => {
                let _ = fs::remove_dir_all(&tmp);
                return Err(err);
            }
        };
        if out.exists() {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::exists(format!(
                "training-ready output appeared during materialization: {}",
                out.display()
            )));
        }
        fs::rename(&tmp, out).map_err(|err| {
            let _ = fs::remove_dir_all(&tmp);
            Error::io(format!(
                "cannot publish training-ready output at {}: {err}",
                out.display()
            ))
        })?;
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        // Verify the published paths exist inside the final directory.
        let final_manifest = out.join(MANIFEST_FILE);
        let final_record = out.join(RECORD_FILE);
        Ok((final_manifest, final_record))
    }
}

/// Verify an existing training-ready directory against its canonical manifest.
pub fn verify_training_ready_output(out: &Path) -> Result<TrainingReadyVerification> {
    let manifest_path = out.join(MANIFEST_FILE);
    let bytes = fs::read(&manifest_path)
        .map_err(|err| Error::io(format!("cannot read {}: {err}", manifest_path.display())))?;
    let value = wu::decode(&bytes)?;
    let map = wu::as_map(&value)?;
    let schema = wu::get_text(map, "schema")?;
    if schema != TRAINING_READY_MANIFEST_SCHEMA {
        return Err(Error::unknown_schema(format!(
            "training-ready manifest schema is `{schema}`, expected `{TRAINING_READY_MANIFEST_SCHEMA}`"
        )));
    }
    let context_root_id = wu::get_opt_hash(map, "context_root_id")?;
    let snapshot_id = wu::get_opt_hash(map, "snapshot_id")?;
    let files = match wu::field(map, "files")? {
        Value::Array(items) => items,
        _ => return Err(Error::malformed("training-ready `files` must be an array")),
    };

    let mut file_count = 0u64;
    let mut total_bytes = 0u64;
    let mut tensor_roots_verified = 0u64;
    let mut checkpoints: Vec<(String, TrainingReadyCheckpoint)> = Vec::new();

    for item in files {
        let file_map = wu::as_map(item)?;
        let path = wu::get_text(file_map, "path")?;
        let relative = relative_output_path(&path)?;
        let target = out.join(&relative);
        let kind = wu::get_text(file_map, "kind")?;
        let size = wu::get_u64(file_map, "size")?;
        let content_hash = wu::get_hash(file_map, "content_hash")?;
        match kind.as_str() {
            "dir" => {
                if !target.is_dir() {
                    return Err(Error::corrupt(format!(
                        "materialized directory `{path}` is missing"
                    )));
                }
            }
            "symlink" => {
                let resolved = fs::read_link(&target)
                    .map_err(|err| Error::io(format!("cannot read {}: {err}", target.display())))?;
                let bytes = resolved.to_string_lossy().into_owned().into_bytes();
                if identity::content_hash(&bytes) != content_hash {
                    return Err(Error::corrupt(format!(
                        "materialized symlink `{path}` does not match its recorded digest"
                    )));
                }
            }
            _ => {
                let bytes = fs::read(&target)
                    .map_err(|err| Error::io(format!("cannot read {}: {err}", target.display())))?;
                if bytes.len() as u64 != size {
                    return Err(Error::corrupt(format!(
                        "materialized file `{path}` is {} bytes, manifest records {size}",
                        bytes.len()
                    )));
                }
                if identity::content_hash(&bytes) != content_hash {
                    return Err(Error::corrupt(format!(
                        "materialized file `{path}` content hash mismatch"
                    )));
                }
                total_bytes = total_bytes.saturating_add(size);
                if let Some(tensor) = parse_tensor_value(file_map)? {
                    if tensor.offline_verifiable {
                        let dtype = TensorElementDtype::parse(&tensor.dtype)?;
                        let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
                            Error::unsupported_version("tensor dtype has no physical layout")
                        })?;
                        let root =
                            tc::logical_root(physical, &tensor.shape, &bytes, &Limits::default())?;
                        if root != tensor.logical_root {
                            return Err(Error::corrupt(format!(
                                "materialized tensor `{path}` logical root does not match"
                            )));
                        }
                        tensor_roots_verified += 1;
                    }
                }
                if let Some(checkpoint) = parse_checkpoint_value(file_map)? {
                    checkpoints.push((path.clone(), checkpoint));
                }
            }
        }
        file_count += 1;
    }

    let mut checkpoints_verified = 0u64;
    let mut checkpoint_tensors_verified = 0u64;
    for (path, info) in &checkpoints {
        let manifest_bytes = fs::read(out.join(relative_output_path(path)?))
            .map_err(|err| Error::io(format!("cannot read checkpoint manifest {path}: {err}")))?;
        let manifest = CheckpointManifest::decode(&manifest_bytes)?;
        if manifest.root()? != info.manifest_root {
            return Err(Error::corrupt(format!(
                "materialized checkpoint `{path}` manifest root does not match"
            )));
        }
        let parent = Path::new(path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let parent = if parent == Path::new("/") {
            PathBuf::new()
        } else {
            relative_output_path(&parent.to_string_lossy())?
        };
        let limits = Limits::default();
        for tensor in &manifest.tensors {
            let dtype = TensorElementDtype::parse(&tensor.dtype)?;
            let physical = dtype.physical_tensor_dtype().ok_or_else(|| {
                Error::unsupported_version(format!(
                    "checkpoint tensor `{}` dtype `{}` has no physical layout",
                    tensor.name, tensor.dtype
                ))
            })?;
            let mut payloads = Vec::with_capacity(tensor.shards.len());
            for shard in &tensor.shards {
                let shard_target = out.join(&parent).join(&shard.object_name);
                let shard_bytes = fs::read(&shard_target).map_err(|err| {
                    Error::io(format!(
                        "cannot read checkpoint shard `{}` for tensor `{}`: {err}",
                        shard.object_name, tensor.name
                    ))
                })?;
                if shard_bytes.len() as u64 != shard.byte_len {
                    return Err(Error::corrupt(format!(
                        "checkpoint shard `{}` is {} bytes, manifest records {}",
                        shard.object_name,
                        shard_bytes.len(),
                        shard.byte_len
                    )));
                }
                if identity::content_hash(&shard_bytes) != shard.content_root {
                    return Err(Error::corrupt(format!(
                        "checkpoint shard `{}` content root mismatch",
                        shard.object_name
                    )));
                }
                let shard_shape =
                    tc::shard_shape(&tensor.shape, shard.axis, shard.shards, shard.index)?;
                let shard_root = tc::logical_root(physical, &shard_shape, &shard_bytes, &limits)?;
                if shard_root != shard.logical_root {
                    return Err(Error::corrupt(format!(
                        "checkpoint shard `{}` logical root mismatch",
                        shard.object_name
                    )));
                }
                payloads.push(shard_bytes);
            }
            let assembled = PhysicalTensor::sharded(
                physical,
                &tensor.shape,
                tensor.shards[0].axis,
                payloads,
                &limits,
            )?
            .assemble_with_limits(&limits)?;
            if assembled.len() as u64 != tensor.element_bytes {
                return Err(Error::corrupt(format!(
                    "checkpoint tensor `{}` reassembled to the wrong length",
                    tensor.name
                )));
            }
            if identity::content_hash(&assembled) != tensor.content_root {
                return Err(Error::corrupt(format!(
                    "checkpoint tensor `{}` content root mismatch",
                    tensor.name
                )));
            }
            if tc::logical_root(physical, &tensor.shape, &assembled, &limits)?
                != tensor.logical_root
            {
                return Err(Error::corrupt(format!(
                    "checkpoint tensor `{}` logical root mismatch",
                    tensor.name
                )));
            }
            checkpoint_tensors_verified += 1;
        }
        checkpoints_verified += 1;
    }

    Ok(TrainingReadyVerification {
        files: file_count,
        bytes: total_bytes,
        tensor_roots_verified,
        checkpoints_verified,
        checkpoint_tensors_verified,
        context_root_id,
        snapshot_id,
    })
}

impl TrainingReadyReport {
    /// CLI JSON view. On-disk artifacts never use JSON.
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "out": self.out.as_ref().map(|path| path.display().to_string()),
            "manifest": self.manifest_path.as_ref().map(|path| path.display().to_string()),
            "record": self.record_path.as_ref().map(|path| path.display().to_string()),
            "context_root_id": self.context_root_id.to_hex(),
            "context_root_name": self.context_root_name,
            "uri": self.uri,
            "snapshot_id": self.snapshot_id.to_hex(),
            "selector_id": self.selector_id.to_hex(),
            "capability_hash": self.capability_hash.map(|hash| hash.to_hex()),
            "declared_closure": self.declared_closure.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
            "preconditions": {
                "resolution_exact": self.preconditions.resolution_exact,
                "content_verified": self.preconditions.content_verified,
                "causal_complete": self.preconditions.causal_complete,
                "authorized": self.preconditions.authorized,
                "tensors_verified": self.preconditions.tensors_verified,
                "budget_enforced": self.preconditions.budget_enforced,
            },
            "cost": {
                "max_bytes": self.cost.max_bytes,
                "max_objects": self.cost.max_objects,
                "max_decode_ms": self.cost.max_decode_ms,
                "estimated_bytes": self.cost.estimated_bytes,
                "estimated_objects": self.cost.estimated_objects,
                "estimated_decode_ms": self.cost.estimated_decode_ms,
                "actual_bytes": self.cost.actual_bytes,
                "actual_objects": self.cost.actual_objects,
                "actual_decode_ms": self.cost.actual_decode_ms,
            },
            "tensor_objects_verified": self.tensor_objects_verified,
            "checkpoint_tensors_verified": self.checkpoint_tensors_verified,
            "files": self.files.iter().map(file_json).collect::<Vec<_>>(),
        })
    }
}

impl TrainingReadyVerification {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "files": self.files,
            "bytes": self.bytes,
            "tensor_roots_verified": self.tensor_roots_verified,
            "checkpoints_verified": self.checkpoints_verified,
            "checkpoint_tensors_verified": self.checkpoint_tensors_verified,
            "context_root_id": self.context_root_id.map(|hash| hash.to_hex()),
            "snapshot_id": self.snapshot_id.map(|hash| hash.to_hex()),
        })
    }
}

fn enforce_budget(cost: &TrainingReadyCost, options: &TrainingReadyOptions) -> Result<()> {
    if let Some(max) = options.max_bytes {
        if cost.estimated_bytes > max || cost.actual_bytes > max {
            return Err(Error::budget_exceeded(format!(
                "training-ready needs {} bytes (estimate {}), budget is {max}",
                cost.actual_bytes.max(cost.estimated_bytes),
                cost.estimated_bytes
            )));
        }
    }
    if let Some(max) = options.max_objects {
        if cost.estimated_objects > max || cost.actual_objects > max {
            return Err(Error::budget_exceeded(format!(
                "training-ready needs {} objects (estimate {}), budget is {max}",
                cost.actual_objects.max(cost.estimated_objects),
                cost.estimated_objects
            )));
        }
    }
    if let Some(max) = options.max_decode_ms {
        if cost.estimated_decode_ms > max || cost.actual_decode_ms > max {
            return Err(Error::budget_exceeded(format!(
                "training-ready decode needs {} ms (estimate {}), budget is {max}",
                cost.actual_decode_ms.max(cost.estimated_decode_ms),
                cost.estimated_decode_ms
            )));
        }
    }
    Ok(())
}

fn resolve_identity(
    state: &State,
    version_to_objects: &BTreeMap<Hash32, Vec<Hash32>>,
    content_to_objects: &BTreeMap<Hash32, Vec<Hash32>>,
    identity: Hash32,
) -> Result<Option<Hash32>> {
    if state.objects.contains_key(&identity) {
        return Ok(Some(identity));
    }
    for index in [version_to_objects, content_to_objects] {
        if let Some(ids) = index.get(&identity) {
            let contents: BTreeSet<Hash32> = ids
                .iter()
                .filter_map(|id| state.objects.get(id).map(|record| record.content))
                .collect();
            if contents.len() > 1 {
                return Err(Error::ambiguous(format!(
                    "causal identity {identity} matches objects with different content roots"
                )));
            }
            if let Some(id) = ids.first() {
                return Ok(Some(*id));
            }
        }
    }
    Ok(None)
}

fn resolve_in_state(state: &State, components: &[&[u8]]) -> Result<Hash32> {
    let mut current = State::ROOT_ID;
    for component in components {
        crate::state::validate_name_bytes(component)?;
        let object = state
            .objects
            .get(&current)
            .ok_or_else(|| Error::not_found("no such parent object"))?;
        if object.kind != ObjectKind::Dir {
            return Err(Error::not_dir("path component is not a directory"));
        }
        current = state.lookup(current, component)?;
    }
    Ok(current)
}

fn is_checkpoint_manifest(record: &ObjectRecord) -> bool {
    matches!(
        record.attrs.get("weft.checkpoint.schema"),
        Some(Value::Str(text)) if text == CHECKPOINT_SCHEMA
    )
}

fn checkpoint_name_from_path(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() == 3 && parts[0] == CHECKPOINT_ROOT && parts[2] == CHECKPOINT_MANIFEST_NAME {
        Some(parts[1].to_owned())
    } else {
        None
    }
}

fn object_path(state: &State, id: Hash32) -> String {
    state
        .path_for(id)
        .map(|path| String::from_utf8_lossy(&path).into_owned())
        .unwrap_or_else(|| format!("/_weft/objects/{id}"))
}

fn relative_output_path(path: &str) -> Result<PathBuf> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(Error::invalid_name("training-ready path is empty"));
    }
    let mut out = PathBuf::new();
    for component in trimmed.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(Error::invalid_name(format!(
                "training-ready path `{path}` is not a safe relative path"
            )));
        }
        out.push(component);
    }
    Ok(out)
}

fn write_wcb(path: &Path, value: &Value) -> Result<()> {
    let bytes = wu::encode(value)?;
    let mut file = fs::File::create(path)
        .map_err(|err| Error::io(format!("cannot create {}: {err}", path.display())))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn tensor_value(tensor: &TrainingReadyTensor) -> Value {
    wu::map([
        ("dtype", Value::Str(tensor.dtype.clone())),
        (
            "shape",
            Value::Array(tensor.shape.iter().map(|dim| Value::U64(*dim)).collect()),
        ),
        ("logical_root", wu::hash_value(tensor.logical_root)),
        ("source", Value::Str(tensor.source.clone())),
        ("layout", Value::Str(tensor.layout.clone())),
        ("strategy", Value::Str(tensor.strategy.clone())),
        (
            "checkpoint",
            tensor
                .checkpoint
                .as_ref()
                .map(|name| Value::Str(name.clone()))
                .unwrap_or(Value::Null),
        ),
        (
            "tensor_name",
            tensor
                .tensor_name
                .as_ref()
                .map(|name| Value::Str(name.clone()))
                .unwrap_or(Value::Null),
        ),
        ("offline_verifiable", Value::Bool(tensor.offline_verifiable)),
    ])
}

fn checkpoint_value(checkpoint: &TrainingReadyCheckpoint) -> Value {
    wu::map([
        ("name", Value::Str(checkpoint.name.clone())),
        ("manifest_root", wu::hash_value(checkpoint.manifest_root)),
        ("complete", Value::Bool(checkpoint.complete)),
        (
            "source_sha256",
            checkpoint
                .source_sha256
                .map(|bytes| Value::Bytes(bytes.to_vec()))
                .unwrap_or(Value::Null),
        ),
        ("tensors", Value::U64(checkpoint.tensors)),
    ])
}

fn file_value(file: &TrainingReadyFile) -> Value {
    wu::map([
        ("path", Value::Str(file.path.clone())),
        ("kind", Value::Str(file.kind.clone())),
        ("object_id", wu::hash_value(file.object_id)),
        ("version_id", wu::hash_value(file.version_id)),
        ("content_root", wu::hash_value(file.content_root)),
        ("content_hash", wu::hash_value(file.content_hash)),
        (
            "content_identity",
            Value::Str(file.content_identity.clone()),
        ),
        ("size", Value::U64(file.size)),
        (
            "causal_closure",
            Value::Array(
                file.causal_closure
                    .iter()
                    .map(|hash| wu::hash_value(*hash))
                    .collect(),
            ),
        ),
        (
            "tensor",
            file.tensor
                .as_ref()
                .map(tensor_value)
                .unwrap_or(Value::Null),
        ),
        (
            "checkpoint",
            file.checkpoint
                .as_ref()
                .map(checkpoint_value)
                .unwrap_or(Value::Null),
        ),
        ("origin", Value::Str(file.origin.clone())),
    ])
}

fn training_ready_manifest_value(report: &TrainingReadyReport) -> Value {
    wu::map([
        (
            "schema",
            Value::Str(TRAINING_READY_MANIFEST_SCHEMA.to_owned()),
        ),
        ("context_root_id", wu::hash_value(report.context_root_id)),
        (
            "context_root_name",
            Value::Str(report.context_root_name.clone()),
        ),
        ("uri", Value::Str(report.uri.clone())),
        ("snapshot_id", wu::hash_value(report.snapshot_id)),
        ("selector_id", wu::hash_value(report.selector_id)),
        (
            "capability_hash",
            wu::opt_hash_value(report.capability_hash),
        ),
        (
            "declared_closure",
            Value::Array(
                report
                    .declared_closure
                    .iter()
                    .map(|hash| wu::hash_value(*hash))
                    .collect(),
            ),
        ),
        ("preconditions", precondition_value(&report.preconditions)),
        ("cost", cost_value(&report.cost)),
        (
            "files",
            Value::Array(report.files.iter().map(file_value).collect()),
        ),
    ])
}

fn training_ready_record_value(report: &TrainingReadyReport) -> Value {
    wu::map([
        ("schema", Value::Str(TRAINING_READY_SCHEMA.to_owned())),
        ("context_root_id", wu::hash_value(report.context_root_id)),
        (
            "context_root_name",
            Value::Str(report.context_root_name.clone()),
        ),
        ("uri", Value::Str(report.uri.clone())),
        ("snapshot_id", wu::hash_value(report.snapshot_id)),
        ("selector_id", wu::hash_value(report.selector_id)),
        (
            "capability_hash",
            wu::opt_hash_value(report.capability_hash),
        ),
        ("preconditions", precondition_value(&report.preconditions)),
        ("cost", cost_value(&report.cost)),
        ("files", Value::U64(report.files.len() as u64)),
        (
            "checkpoint_tensors_verified",
            Value::U64(report.checkpoint_tensors_verified),
        ),
        (
            "tensor_objects_verified",
            Value::U64(report.tensor_objects_verified),
        ),
    ])
}

fn precondition_value(preconditions: &PreconditionReport) -> Value {
    wu::map([
        (
            "resolution_exact",
            Value::Bool(preconditions.resolution_exact),
        ),
        (
            "content_verified",
            Value::Bool(preconditions.content_verified),
        ),
        (
            "causal_complete",
            Value::Bool(preconditions.causal_complete),
        ),
        ("authorized", Value::Bool(preconditions.authorized)),
        (
            "tensors_verified",
            Value::Bool(preconditions.tensors_verified),
        ),
        (
            "budget_enforced",
            Value::Bool(preconditions.budget_enforced),
        ),
    ])
}

fn cost_value(cost: &TrainingReadyCost) -> Value {
    wu::map([
        (
            "max_bytes",
            cost.max_bytes.map(Value::U64).unwrap_or(Value::Null),
        ),
        (
            "max_objects",
            cost.max_objects.map(Value::U64).unwrap_or(Value::Null),
        ),
        (
            "max_decode_ms",
            cost.max_decode_ms.map(Value::U64).unwrap_or(Value::Null),
        ),
        ("estimated_bytes", Value::U64(cost.estimated_bytes)),
        ("estimated_objects", Value::U64(cost.estimated_objects)),
        ("estimated_decode_ms", Value::U64(cost.estimated_decode_ms)),
        ("actual_bytes", Value::U64(cost.actual_bytes)),
        ("actual_objects", Value::U64(cost.actual_objects)),
        ("actual_decode_ms", Value::U64(cost.actual_decode_ms)),
    ])
}

fn parse_tensor_value(map: &BTreeMap<String, Value>) -> Result<Option<TrainingReadyTensor>> {
    match wu::field(map, "tensor")? {
        Value::Null => Ok(None),
        Value::Map(tensor) => Ok(Some(TrainingReadyTensor {
            dtype: wu::get_text(tensor, "dtype")?,
            shape: match wu::field(tensor, "shape")? {
                Value::Array(items) => items
                    .iter()
                    .map(|value| match value {
                        Value::U64(dim) => Ok(*dim),
                        _ => Err(Error::malformed("tensor shape must be u64")),
                    })
                    .collect::<Result<Vec<u64>>>()?,
                _ => return Err(Error::malformed("tensor shape must be an array")),
            },
            logical_root: wu::get_hash(tensor, "logical_root")?,
            source: wu::get_text(tensor, "source")?,
            layout: wu::get_text(tensor, "layout")?,
            strategy: wu::get_text(tensor, "strategy")?,
            checkpoint: match wu::field(tensor, "checkpoint")? {
                Value::Null => None,
                Value::Str(text) => Some(text.clone()),
                _ => return Err(Error::malformed("tensor checkpoint must be text or null")),
            },
            tensor_name: match wu::field(tensor, "tensor_name")? {
                Value::Null => None,
                Value::Str(text) => Some(text.clone()),
                _ => return Err(Error::malformed("tensor name must be text or null")),
            },
            offline_verifiable: match wu::field(tensor, "offline_verifiable")? {
                Value::Bool(value) => *value,
                _ => return Err(Error::malformed("tensor offline_verifiable must be bool")),
            },
        })),
        _ => Err(Error::malformed("tensor must be a map or null")),
    }
}

fn parse_checkpoint_value(
    map: &BTreeMap<String, Value>,
) -> Result<Option<TrainingReadyCheckpoint>> {
    match wu::field(map, "checkpoint")? {
        Value::Null => Ok(None),
        Value::Map(checkpoint) => Ok(Some(TrainingReadyCheckpoint {
            name: wu::get_text(checkpoint, "name")?,
            manifest_root: wu::get_hash(checkpoint, "manifest_root")?,
            complete: match wu::field(checkpoint, "complete")? {
                Value::Bool(value) => *value,
                _ => return Err(Error::malformed("checkpoint complete must be bool")),
            },
            source_sha256: match wu::field(checkpoint, "source_sha256")? {
                Value::Null => None,
                Value::Bytes(bytes) => {
                    let array: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                        Error::malformed("checkpoint source_sha256 must be 32 bytes")
                    })?;
                    Some(array)
                }
                _ => {
                    return Err(Error::malformed(
                        "checkpoint source_sha256 must be bytes or null",
                    ))
                }
            },
            tensors: wu::get_u64(checkpoint, "tensors")?,
        })),
        _ => Err(Error::malformed("checkpoint must be a map or null")),
    }
}

fn file_json(file: &TrainingReadyFile) -> serde_json::Value {
    json!({
        "path": file.path,
        "kind": file.kind,
        "object_id": file.object_id.to_hex(),
        "version_id": file.version_id.to_hex(),
        "content_root": file.content_root.to_hex(),
        "content_hash": file.content_hash.to_hex(),
        "content_identity": file.content_identity,
        "size": file.size,
        "origin": file.origin,
        "causal_closure": file.causal_closure.iter().map(|hash| hash.to_hex()).collect::<Vec<_>>(),
        "tensor": file.tensor.as_ref().map(|tensor| json!({
            "dtype": tensor.dtype,
            "shape": tensor.shape,
            "logical_root": tensor.logical_root.to_hex(),
            "source": tensor.source,
            "layout": tensor.layout,
            "strategy": tensor.strategy,
            "checkpoint": tensor.checkpoint,
            "tensor_name": tensor.tensor_name,
            "offline_verifiable": tensor.offline_verifiable,
        })),
        "checkpoint": file.checkpoint.as_ref().map(|checkpoint| json!({
            "name": checkpoint.name,
            "manifest_root": checkpoint.manifest_root.to_hex(),
            "complete": checkpoint.complete,
            "source_sha256": checkpoint.source_sha256.map(hex::encode),
            "tensors": checkpoint.tensors,
        })),
    })
}

// The digest is computed during the verified read and carried in `FileFacts`.
