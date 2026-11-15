//! Incremental, generation-scoped ready-state cache (P03).
//!
//! A cached `TrainingReadyReport` is returned only when **every** input that
//! can change the answer is unchanged:
//!
//! * the store generation and head (any commit);
//! * every object's `(id, version, content, size, kind)` and attribute digest
//!   (so a replaced manifest, a changed causal edge, or a new dependency
//!   invalidates);
//! * every relation, entry and snapshot name;
//! * the Context Root name, recorded id and selector id, and the pinned
//!   snapshot;
//! * the declared policy in [`TrainingReadyOptions`];
//! * the mutable authority subtree: every training grant record and epoch,
//!   and every quarantine pointer/record;
//! * the capability hash and its expiry.
//!
//! The key is recomputed from the current state on every call. A stale or
//! forged entry is rejected with a recorded reason and the full verification
//! runs again; a cache miss never returns a pass. The cache is derived local
//! state, never part of a commit, and `--no-cache` / `weft verify --full`
//! remain the independent checks.
//!
//! The state and authority digests are **whole-state** digests, so an
//! unrelated commit also invalidates: that is over-invalidation, not a
//! correctness risk, and it is measured as a loss in
//! `bench/RESULTS-training-ready-cost.md`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use weftcodec::hash::DomainHasher;
use weftcodec::wcb::Value;
use weftcodec::Limits;

use crate::auth::Principal;
use crate::context::CapabilityContext;
use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::state::{ObjectKind, State};
use crate::store::Store;
use crate::training_ready::{TrainingReadyOptions, TrainingReadyReport};
use crate::wcb_util as wu;

/// Cache entry schema.
pub const READY_CACHE_SCHEMA: &str = "weft.ready-cache.v1";
/// Key domain.
pub const READY_CACHE_KEY_DOMAIN: &str = "weft/ready-cache/key/v1";
/// State digest domain.
pub const READY_CACHE_STATE_DOMAIN: &str = "weft/ready-cache/state/v1";
/// Authority digest domain.
pub const READY_CACHE_AUTHORITY_DOMAIN: &str = "weft/ready-cache/authority/v1";
/// Evidence domain.
pub const READY_CACHE_EVIDENCE_DOMAIN: &str = "weft/ready-cache/evidence/v1";
/// Options digest domain.
pub const READY_CACHE_OPTIONS_DOMAIN: &str = "weft/ready-cache/options/v1";
/// Attributes digest domain.
pub const READY_CACHE_ATTRS_DOMAIN: &str = "weft/ready-cache/attrs/v1";
/// Default cache directory name inside a caller-provided parent.
pub const READY_CACHE_DIR: &str = "ready-cache";

/// Caller-supplied cache settings.
#[derive(Clone, Debug)]
pub struct ReadyCacheOptions {
    pub dir: PathBuf,
    /// Ignore a hit and re-verify, then overwrite the entry.
    pub refresh: bool,
}

impl ReadyCacheOptions {
    pub fn new(dir: impl Into<PathBuf>) -> ReadyCacheOptions {
        ReadyCacheOptions {
            dir: dir.into(),
            refresh: false,
        }
    }
}

/// What happened on one cached check.
#[derive(Clone, Debug)]
pub struct ReadyCacheOutcome {
    pub hit: bool,
    pub key: Hash32,
    pub head: Hash32,
    pub generation: u64,
    pub state_digest: Hash32,
    pub authority_digest: Hash32,
    pub options_digest: Hash32,
    /// Why a found entry was not used. `None` on a hit or a clean miss.
    pub rejected: Option<String>,
    pub entry_written: bool,
    pub write_note: Option<String>,
    pub plan_ms: f64,
    pub verify_ms: f64,
}

impl ReadyCacheOutcome {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "hit": self.hit,
            "key": self.key.to_hex(),
            "head": self.head.to_hex(),
            "generation": self.generation,
            "state_digest": self.state_digest.to_hex(),
            "authority_digest": self.authority_digest.to_hex(),
            "options_digest": self.options_digest.to_hex(),
            "rejected": self.rejected,
            "entry_written": self.entry_written,
            "write_note": self.write_note,
            "plan_ms": self.plan_ms,
            "verify_ms": self.verify_ms,
        })
    }
}

fn attrs_digest(attrs: &BTreeMap<String, Value>) -> Result<Hash32> {
    Ok(identity::h(
        READY_CACHE_ATTRS_DOMAIN,
        &wu::encode(&Value::Map(attrs.clone()))?,
    ))
}

fn feed_u64(hasher: &mut DomainHasher, value: u64) {
    hasher.update(&value.to_le_bytes());
}

fn feed_str(hasher: &mut DomainHasher, text: &str) {
    feed_u64(hasher, text.len() as u64);
    hasher.update(text.as_bytes());
}

/// Digest of every immutable input in `state`.
pub fn state_digest(state: &State) -> Result<Hash32> {
    let mut hasher = DomainHasher::new(READY_CACHE_STATE_DOMAIN);
    feed_u64(&mut hasher, state.generation);
    feed_u64(&mut hasher, state.objects.len() as u64);
    for record in state.objects.values() {
        hasher.update(record.id.as_bytes());
        hasher.update(record.version.as_bytes());
        hasher.update(record.content.as_bytes());
        feed_u64(&mut hasher, record.size);
        feed_str(&mut hasher, record.kind.as_str());
        feed_u64(&mut hasher, record.mode);
        feed_u64(&mut hasher, record.uid);
        feed_u64(&mut hasher, record.gid);
        hasher.update(attrs_digest(&record.attrs)?.as_bytes());
    }
    feed_u64(&mut hasher, state.entries.len() as u64);
    for entry in state.entries.values() {
        hasher.update(entry.id.as_bytes());
        hasher.update(entry.parent.as_bytes());
        feed_str(&mut hasher, &String::from_utf8_lossy(&entry.name));
        hasher.update(entry.target.as_bytes());
    }
    feed_u64(&mut hasher, state.relations.len() as u64);
    for relation in state.relations.values() {
        hasher.update(relation.id.as_bytes());
        hasher.update(relation.subject.as_bytes());
        feed_str(&mut hasher, &relation.predicate);
        feed_str(&mut hasher, relation.kind.as_str());
        match &relation.target {
            crate::state::RelationTarget::Object(id) => {
                hasher.update(b"object");
                hasher.update(id.as_bytes());
            }
            crate::state::RelationTarget::Value(value) => {
                hasher.update(b"value");
                hasher.update(&wu::encode(value)?);
            }
        }
    }
    feed_u64(&mut hasher, state.snapshots.len() as u64);
    for (name, commit) in &state.snapshots {
        feed_str(&mut hasher, name);
        hasher.update(commit.as_bytes());
    }
    feed_u64(&mut hasher, state.tips.len() as u64);
    for (object, version) in &state.tips {
        hasher.update(object.as_bytes());
        hasher.update(version.as_bytes());
    }
    Ok(hasher.finalize())
}

fn subtree_digest(state: &State, hasher: &mut DomainHasher, path: &str) -> Result<()> {
    feed_str(hasher, path);
    let mut current = Hash32::ZERO;
    let parts: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    for (index, part) in parts.iter().enumerate() {
        match state.lookup(current, part.as_bytes()) {
            Ok(next) => current = next,
            Err(err) if err.class() == Class::NotFound => {
                hasher.update(b"absent");
                return Ok(());
            }
            Err(err) => return Err(err),
        }
        if index + 1 == parts.len() {
            break;
        }
    }
    let Some(record) = state.objects.get(&current) else {
        hasher.update(b"absent");
        return Ok(());
    };
    if record.kind != ObjectKind::Dir {
        hasher.update(b"notdir");
        return Ok(());
    }
    hasher.update(b"dir");
    let mut entries = state.list_dir(current)?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    feed_u64(hasher, entries.len() as u64);
    for entry in entries {
        feed_str(hasher, &String::from_utf8_lossy(&entry.name));
        if let Some(target) = state.objects.get(&entry.target) {
            hasher.update(target.version.as_bytes());
            hasher.update(target.content.as_bytes());
            feed_u64(hasher, target.size);
            feed_str(hasher, target.kind.as_str());
            hasher.update(attrs_digest(&target.attrs)?.as_bytes());
        } else {
            hasher.update(b"dangling");
        }
    }
    Ok(())
}

/// Digest of the mutable authority subtrees: grants and quarantine.
pub fn authority_digest(state: &State) -> Result<Hash32> {
    let mut hasher = DomainHasher::new(READY_CACHE_AUTHORITY_DOMAIN);
    subtree_digest(
        state,
        &mut hasher,
        crate::training_artifact::TRAINING_GRANT_DIR,
    )?;
    subtree_digest(state, &mut hasher, crate::taint::QUARANTINE_DIR)?;
    Ok(hasher.finalize())
}

fn options_digest(
    options: &TrainingReadyOptions,
    capability: Option<&CapabilityContext>,
) -> Result<Hash32> {
    let value = wu::map([
        (
            "max_bytes",
            match options.max_bytes {
                Some(v) => Value::U64(v),
                None => Value::Null,
            },
        ),
        (
            "max_objects",
            match options.max_objects {
                Some(v) => Value::U64(v),
                None => Value::Null,
            },
        ),
        (
            "max_decode_ms",
            match options.max_decode_ms {
                Some(v) => Value::U64(v),
                None => Value::Null,
            },
        ),
        ("require_tensors", Value::Bool(options.require_tensors)),
        (
            "capability_hash",
            match capability {
                Some(capability) => wu::hash_value(capability.hash),
                None => Value::Null,
            },
        ),
        (
            "capability_expires_at",
            match capability.and_then(|capability| capability.scope.expires_at) {
                Some(value) => Value::Timestamp(value),
                None => Value::Null,
            },
        ),
    ]);
    Ok(identity::h(
        READY_CACHE_OPTIONS_DOMAIN,
        &wu::encode(&value)?,
    ))
}

fn cache_key(
    head: Hash32,
    state_digest: Hash32,
    authority_digest: Hash32,
    options_digest: Hash32,
    context_root_name: &str,
    context_root_id: Hash32,
    selector_id: Hash32,
    snapshot: Hash32,
) -> Result<Hash32> {
    let value = wu::map([
        ("schema", Value::Str(READY_CACHE_SCHEMA.to_owned())),
        ("head", wu::hash_value(head)),
        ("state_digest", wu::hash_value(state_digest)),
        ("authority_digest", wu::hash_value(authority_digest)),
        ("options_digest", wu::hash_value(options_digest)),
        (
            "context_root_name",
            Value::Str(context_root_name.to_owned()),
        ),
        ("context_root_id", wu::hash_value(context_root_id)),
        ("selector_id", wu::hash_value(selector_id)),
        ("snapshot", wu::hash_value(snapshot)),
    ]);
    Ok(identity::h(READY_CACHE_KEY_DOMAIN, &wu::encode(&value)?))
}

fn evidence_digest(
    key: Hash32,
    head: Hash32,
    generation: u64,
    state_digest: Hash32,
    authority_digest: Hash32,
    options_digest: Hash32,
    files: &serde_json::Value,
) -> Result<Hash32> {
    let value = wu::map([
        ("key", wu::hash_value(key)),
        ("head", wu::hash_value(head)),
        ("generation", Value::U64(generation)),
        ("state_digest", wu::hash_value(state_digest)),
        ("authority_digest", wu::hash_value(authority_digest)),
        ("options_digest", wu::hash_value(options_digest)),
        ("files", Value::Bytes(serde_json::to_vec(files).unwrap_or_default())),
    ]);
    Ok(identity::h(
        READY_CACHE_EVIDENCE_DOMAIN,
        &wu::encode(&value)?,
    ))
}

fn json_hash(value: &serde_json::Value, what: &str) -> Result<Hash32> {
    let text = value
        .as_str()
        .ok_or_else(|| Error::malformed(format!("cache {what} must be hex")))?;
    Hash32::from_hex(text)
        .map_err(|_| Error::malformed(format!("cache {what} is not a 32-byte hash")))
}

fn json_u64(value: &serde_json::Value, what: &str) -> Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| Error::malformed(format!("cache {what} must be a u64")))
}

/// Serialize the report for the cache (cache-only JSON; never a format).
pub fn report_to_cache(report: &TrainingReadyReport) -> serde_json::Value {
    let files: Vec<serde_json::Value> = report
        .files
        .iter()
        .map(|file| {
            serde_json::json!({
                "path": file.path,
                "kind": file.kind,
                "object_id": file.object_id.to_hex(),
                "version_id": file.version_id.to_hex(),
                "content_root": file.content_root.to_hex(),
                "content_hash": file.content_hash.to_hex(),
                "content_identity": file.content_identity,
                "size": file.size,
                "causal_closure": file.causal_closure.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
                "origin": file.origin,
                "tensor": file.tensor.as_ref().map(|tensor| serde_json::json!({
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
                "checkpoint": file.checkpoint.as_ref().map(|checkpoint| serde_json::json!({
                    "name": checkpoint.name,
                    "manifest_root": checkpoint.manifest_root.to_hex(),
                    "complete": checkpoint.complete,
                    "source_sha256": checkpoint.source_sha256.map(|digest| {
                        digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
                    }),
                    "tensors": checkpoint.tensors,
                })),
            })
        })
        .collect();
    serde_json::json!({
        "schema": READY_CACHE_SCHEMA,
        "context_root_id": report.context_root_id.to_hex(),
        "context_root_name": report.context_root_name,
        "uri": report.uri,
        "snapshot_id": report.snapshot_id.to_hex(),
        "selector_id": report.selector_id.to_hex(),
        "capability_hash": report.capability_hash.map(|id| id.to_hex()),
        "declared_closure": report.declared_closure.iter().map(|id| id.to_hex()).collect::<Vec<_>>(),
        "preconditions": {
            "resolution_exact": report.preconditions.resolution_exact,
            "content_verified": report.preconditions.content_verified,
            "causal_complete": report.preconditions.causal_complete,
            "authorized": report.preconditions.authorized,
            "tensors_verified": report.preconditions.tensors_verified,
            "budget_enforced": report.preconditions.budget_enforced,
        },
        "cost": {
            "max_bytes": report.cost.max_bytes,
            "max_objects": report.cost.max_objects,
            "max_decode_ms": report.cost.max_decode_ms,
            "estimated_bytes": report.cost.estimated_bytes,
            "estimated_objects": report.cost.estimated_objects,
            "estimated_decode_ms": report.cost.estimated_decode_ms,
            "actual_bytes": report.cost.actual_bytes,
            "actual_objects": report.cost.actual_objects,
            "actual_decode_ms": report.cost.actual_decode_ms,
        },
        "tensor_objects_verified": report.tensor_objects_verified,
        "checkpoint_tensors_verified": report.checkpoint_tensors_verified,
        "checkpoints": report.checkpoints.iter().map(|checkpoint| serde_json::json!({
            "name": checkpoint.name,
            "manifest_root": checkpoint.manifest_root.to_hex(),
            "complete": checkpoint.complete,
            "source_sha256": checkpoint.source_sha256.map(|digest| {
                digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
            }),
            "tensors": checkpoint.tensors,
        })).collect::<Vec<_>>(),
        "files": files,
    })
}

fn parse_hex_digest(text: &str) -> Result<[u8; 32]> {
    let hash = Hash32::from_hex(text).map_err(|_| Error::malformed("cache sha256 is not hex"))?;
    Ok(hash.into_bytes())
}

/// Reconstruct a `TrainingReadyReport` from a cache entry.
pub fn report_from_cache(value: &serde_json::Value) -> Result<TrainingReadyReport> {
    use crate::training_ready::{
        PreconditionReport, TrainingReadyCheckpoint, TrainingReadyCost, TrainingReadyFile,
        TrainingReadyTensor,
    };
    let object = value
        .as_object()
        .ok_or_else(|| Error::malformed("cache report must be an object"))?;
    let mut files = Vec::new();
    for file in object
        .get("files")
        .and_then(|value| value.as_array())
        .ok_or_else(|| Error::malformed("cache report files must be an array"))?
    {
        let map = file
            .as_object()
            .ok_or_else(|| Error::malformed("cache file must be an object"))?;
        let tensor = match map.get("tensor").and_then(|value| value.as_object()) {
            Some(tensor) => Some(TrainingReadyTensor {
                dtype: tensor
                    .get("dtype")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                shape: tensor
                    .get("shape")
                    .and_then(|v| v.as_array())
                    .map(|items| items.iter().filter_map(|item| item.as_u64()).collect())
                    .unwrap_or_default(),
                logical_root: json_hash(
                    tensor
                        .get("logical_root")
                        .unwrap_or(&serde_json::Value::Null),
                    "tensor logical_root",
                )?,
                source: tensor
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                layout: tensor
                    .get("layout")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                strategy: tensor
                    .get("strategy")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                checkpoint: tensor
                    .get("checkpoint")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                tensor_name: tensor
                    .get("tensor_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                offline_verifiable: tensor
                    .get("offline_verifiable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            }),
            None => None,
        };
        let checkpoint = match map.get("checkpoint").and_then(|value| value.as_object()) {
            Some(checkpoint) => Some(TrainingReadyCheckpoint {
                name: checkpoint
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                manifest_root: json_hash(
                    checkpoint
                        .get("manifest_root")
                        .unwrap_or(&serde_json::Value::Null),
                    "checkpoint manifest_root",
                )?,
                complete: checkpoint
                    .get("complete")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                source_sha256: checkpoint
                    .get("source_sha256")
                    .and_then(|v| v.as_str())
                    .map(parse_hex_digest)
                    .transpose()?,
                tensors: checkpoint
                    .get("tensors")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            }),
            None => None,
        };
        files.push(TrainingReadyFile {
            path: map
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            kind: map
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            object_id: json_hash(
                map.get("object_id").unwrap_or(&serde_json::Value::Null),
                "object_id",
            )?,
            version_id: json_hash(
                map.get("version_id").unwrap_or(&serde_json::Value::Null),
                "version_id",
            )?,
            content_root: json_hash(
                map.get("content_root").unwrap_or(&serde_json::Value::Null),
                "content_root",
            )?,
            content_hash: json_hash(
                map.get("content_hash").unwrap_or(&serde_json::Value::Null),
                "content_hash",
            )?,
            content_identity: map
                .get("content_identity")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            size: json_u64(map.get("size").unwrap_or(&serde_json::Value::Null), "size")?,
            causal_closure: map
                .get("causal_closure")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .map(|item| json_hash(item, "causal_closure"))
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            tensor,
            checkpoint,
            origin: map
                .get("origin")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
        });
    }
    let preconditions = object
        .get("preconditions")
        .and_then(|value| value.as_object())
        .ok_or_else(|| Error::malformed("cache preconditions missing"))?;
    let cost = object
        .get("cost")
        .and_then(|value| value.as_object())
        .ok_or_else(|| Error::malformed("cache cost missing"))?;
    Ok(TrainingReadyReport {
        out: None,
        manifest_path: None,
        record_path: None,
        context_root_id: json_hash(
            object
                .get("context_root_id")
                .unwrap_or(&serde_json::Value::Null),
            "context_root_id",
        )?,
        context_root_name: object
            .get("context_root_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned(),
        uri: object
            .get("uri")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned(),
        snapshot_id: json_hash(
            object
                .get("snapshot_id")
                .unwrap_or(&serde_json::Value::Null),
            "snapshot_id",
        )?,
        selector_id: json_hash(
            object
                .get("selector_id")
                .unwrap_or(&serde_json::Value::Null),
            "selector_id",
        )?,
        capability_hash: object
            .get("capability_hash")
            .and_then(|v| v.as_str())
            .map(|text| {
                Hash32::from_hex(text).map_err(|_| Error::malformed("cache capability_hash"))
            })
            .transpose()?,
        declared_closure: object
            .get("declared_closure")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .map(|item| json_hash(item, "declared_closure"))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default(),
        preconditions: PreconditionReport {
            resolution_exact: preconditions
                .get("resolution_exact")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            content_verified: preconditions
                .get("content_verified")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            causal_complete: preconditions
                .get("causal_complete")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            authorized: preconditions
                .get("authorized")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            tensors_verified: preconditions
                .get("tensors_verified")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            budget_enforced: preconditions
                .get("budget_enforced")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        },
        files,
        cost: TrainingReadyCost {
            max_bytes: cost.get("max_bytes").and_then(|v| v.as_u64()),
            max_objects: cost.get("max_objects").and_then(|v| v.as_u64()),
            max_decode_ms: cost.get("max_decode_ms").and_then(|v| v.as_u64()),
            estimated_bytes: cost
                .get("estimated_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            estimated_objects: cost
                .get("estimated_objects")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            estimated_decode_ms: cost
                .get("estimated_decode_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            actual_bytes: cost
                .get("actual_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            actual_objects: cost
                .get("actual_objects")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            actual_decode_ms: cost
                .get("actual_decode_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        },
        tensor_objects_verified: object
            .get("tensor_objects_verified")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        checkpoint_tensors_verified: object
            .get("checkpoint_tensors_verified")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        checkpoints: object
            .get("checkpoints")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .map(|item| -> Result<TrainingReadyCheckpoint> {
                        let map = item
                            .as_object()
                            .ok_or_else(|| Error::malformed("cache checkpoint"))?;
                        Ok(TrainingReadyCheckpoint {
                            name: map
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_owned(),
                            manifest_root: json_hash(
                                map.get("manifest_root").unwrap_or(&serde_json::Value::Null),
                                "checkpoint manifest_root",
                            )?,
                            complete: map
                                .get("complete")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            source_sha256: map
                                .get("source_sha256")
                                .and_then(|v| v.as_str())
                                .map(parse_hex_digest)
                                .transpose()?,
                            tensors: map.get("tensors").and_then(|v| v.as_u64()).unwrap_or(0),
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default(),
    })
}

impl Store {
    /// Check training-ready with an optional generation-scoped cache.
    pub fn check_training_ready_cached(
        &self,
        root: &str,
        capability: Option<&CapabilityContext>,
        principal: &Principal,
        limits: &Limits,
        now_ns: i64,
        options: &TrainingReadyOptions,
        cache: Option<&ReadyCacheOptions>,
    ) -> Result<(TrainingReadyReport, Option<ReadyCacheOutcome>)> {
        let Some(cache) = cache else {
            let report =
                self.check_training_ready(root, capability, principal, limits, now_ns, options)?;
            return Ok((report, None));
        };
        let plan_start = std::time::Instant::now();
        let state = self.current_state();
        if let Some(valid_until) = capability.and_then(|capability| capability.scope.expires_at) {
            if now_ns > valid_until {
                // Let the full verification produce the typed refusal.
                let report = self
                    .check_training_ready(root, capability, principal, limits, now_ns, options)?;
                return Ok((report, None));
            }
        }
        let state_digest = state_digest(state)?;
        let authority_digest = authority_digest(state)?;
        let options_digest = options_digest(options, capability)?;
        let stored = self.load_context_root_at(state, root)?;
        let key = cache_key(
            self.head(),
            state_digest,
            authority_digest,
            options_digest,
            &stored.name,
            stored.context_root_id,
            stored.root.selector_id,
            stored.root.snapshot,
        )?;
        let plan_ms = plan_start.elapsed().as_secs_f64() * 1000.0;

        let mut rejected = None;
        let path = cache.dir.join(format!("{}.json", key.to_hex()));
        if !cache.refresh {
            match fs::read(&path) {
                Ok(bytes) => match self.cache_entry_matches(
                    &bytes,
                    key,
                    state,
                    state_digest,
                    authority_digest,
                    options_digest,
                ) {
                    Ok(report) => {
                        return Ok((
                            report,
                            Some(ReadyCacheOutcome {
                                hit: true,
                                key,
                                head: self.head(),
                                generation: self.generation(),
                                state_digest,
                                authority_digest,
                                options_digest,
                                rejected: None,
                                entry_written: false,
                                write_note: None,
                                plan_ms,
                                verify_ms: 0.0,
                            }),
                        ))
                    }
                    Err(err) => rejected = Some(format!("{}: {}", err.class(), err.message())),
                },
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => rejected = Some(format!("Io: {err}")),
            }
        }

        let verify_start = std::time::Instant::now();
        let report =
            self.check_training_ready(root, capability, principal, limits, now_ns, options)?;
        let verify_ms = verify_start.elapsed().as_secs_f64() * 1000.0;
        let body = report_to_cache(&report);
        let files = body
            .get("files")
            .cloned()
            .unwrap_or(serde_json::Value::Array(Vec::new()));
        let evidence = evidence_digest(
            key,
            self.head(),
            self.generation(),
            state_digest,
            authority_digest,
            options_digest,
            &files,
        )?;
        let entry = serde_json::json!({
            "schema": READY_CACHE_SCHEMA,
            "key": key.to_hex(),
            "head": self.head().to_hex(),
            "generation": self.generation(),
            "state_digest": state_digest.to_hex(),
            "authority_digest": authority_digest.to_hex(),
            "options_digest": options_digest.to_hex(),
            "evidence": evidence.to_hex(),
            "report": body,
        });
        let (entry_written, write_note) = match write_entry(&cache.dir, &path, &entry) {
            Ok(()) => (true, None),
            Err(err) => (false, Some(format!("{}: {}", err.class(), err.message()))),
        };
        Ok((
            report,
            Some(ReadyCacheOutcome {
                hit: false,
                key,
                head: self.head(),
                generation: self.generation(),
                state_digest,
                authority_digest,
                options_digest,
                rejected,
                entry_written,
                write_note,
                plan_ms,
                verify_ms,
            }),
        ))
    }

    fn cache_entry_matches(
        &self,
        bytes: &[u8],
        key: Hash32,
        state: &State,
        state_digest: Hash32,
        authority_digest: Hash32,
        options_digest: Hash32,
    ) -> Result<TrainingReadyReport> {
        let entry: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|err| Error::malformed(format!("cache JSON: {err}")))?;
        let object = entry
            .as_object()
            .ok_or_else(|| Error::malformed("cache entry must be an object"))?;
        if object.get("schema").and_then(|v| v.as_str()) != Some(READY_CACHE_SCHEMA) {
            return Err(Error::unknown_schema("cache entry schema"));
        }
        if json_hash(object.get("key").unwrap_or(&serde_json::Value::Null), "key")? != key {
            return Err(Error::corrupt("cache entry key does not match"));
        }
        if json_hash(
            object.get("head").unwrap_or(&serde_json::Value::Null),
            "head",
        )? != self.head()
        {
            return Err(Error::corrupt("cache entry head does not match"));
        }
        if json_u64(
            object.get("generation").unwrap_or(&serde_json::Value::Null),
            "generation",
        )? != self.generation()
        {
            return Err(Error::corrupt("cache entry generation does not match"));
        }
        if json_hash(
            object
                .get("state_digest")
                .unwrap_or(&serde_json::Value::Null),
            "state_digest",
        )? != state_digest
        {
            return Err(Error::corrupt("cache entry state digest does not match"));
        }
        if json_hash(
            object
                .get("authority_digest")
                .unwrap_or(&serde_json::Value::Null),
            "authority_digest",
        )? != authority_digest
        {
            return Err(Error::corrupt(
                "cache entry authority digest does not match",
            ));
        }
        if json_hash(
            object
                .get("options_digest")
                .unwrap_or(&serde_json::Value::Null),
            "options_digest",
        )? != options_digest
        {
            return Err(Error::corrupt("cache entry options digest does not match"));
        }
        let report_value = object
            .get("report")
            .ok_or_else(|| Error::malformed("cache entry report missing"))?;
        let files = report_value
            .get("files")
            .cloned()
            .ok_or_else(|| Error::malformed("cache entry files missing"))?;
        let expected = evidence_digest(
            key,
            self.head(),
            self.generation(),
            state_digest,
            authority_digest,
            options_digest,
            &files,
        )?;
        if json_hash(
            object.get("evidence").unwrap_or(&serde_json::Value::Null),
            "evidence",
        )? != expected
        {
            return Err(Error::corrupt("cache entry evidence digest does not match"));
        }
        let report = report_from_cache(report_value)?;
        // The cached file set must still name the exact current identities.
        for file in &report.files {
            let record = state
                .objects
                .get(&file.object_id)
                .ok_or_else(|| Error::corrupt("cached file object is absent from the state"))?;
            if record.version != file.version_id
                || record.content != file.content_root
                || record.size != file.size
            {
                return Err(Error::corrupt(format!(
                    "cached file {} identities changed",
                    file.path
                )));
            }
        }
        Ok(report)
    }
}

fn write_entry(dir: &Path, path: &Path, entry: &serde_json::Value) -> Result<()> {
    fs::create_dir_all(dir).map_err(|err| Error::io(format!("create cache directory: {err}")))?;
    let bytes = serde_json::to_vec(entry)
        .map_err(|err| Error::invalid_argument(format!("cache JSON: {err}")))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &bytes).map_err(|err| Error::io(format!("write cache entry: {err}")))?;
    fs::rename(&tmp, path).map_err(|err| Error::io(format!("publish cache entry: {err}")))?;
    Ok(())
}
