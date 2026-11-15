//! T075 — taint tracing and quarantine.
//!
//! Two separate things, deliberately kept separate:
//!
//! * **Taint trace.** Given one or more tainted content roots (a dataset member
//!   or a compromised input), the store walks the *provenance* graph forward to
//!   the precise set of downstream artifacts and decides, for each artifact of
//!   interest, one of three verdicts:
//!   - `tainted` — a provenance path exists from a tainted root to it, and the
//!     witness path is returned;
//!   - `clear` — the artifact's whole backward provenance closure is complete
//!     (every node is either a declared source or has a receipt) and no node in
//!     it is tainted;
//!   - `unproven` — the graph is incomplete somewhere on that closure. An
//!     incomplete graph is **never** reported as `clear`.
//! * **Quarantine.** An authenticated record that revokes visibility and
//!   refuses materialization for a content root. Naming a content root is the
//!   point: identity is representation-independent, so a rewrite cannot escape
//!   by producing a new object id with the same content.
//!
//! # This is not unlearning
//!
//! Quarantining cannot remove information that has already been learned into
//! model weights. It provides traceability and refusal to serve. Nothing in
//! this module inspects, edits, retrains or repairs a model. A quarantined
//! checkpoint is not served; the weights that were trained from the tainted
//! data are, as a matter of fact, still in those weights. Any claim that this
//! constitutes unlearning is false and is not made anywhere in this repository.

use std::collections::{BTreeMap, BTreeSet};

use weftcodec::wcb::Value;

use crate::error::{Class, Error, Result};
use crate::identity::{self, Hash32};
use crate::provenance::{ProvenanceRecord, PROVENANCE_PREDICATE};
use crate::state::{ObjectKind, RelationTarget, State};
use crate::store::Store;
use crate::wcb_util as wu;

/// Trace record schema.
pub const TAINT_TRACE_SCHEMA: &str = "weft.taint-trace.v1";
/// Quarantine record schema.
pub const QUARANTINE_SCHEMA: &str = "weft.quarantine.v1";
/// Trace identity domain.
pub const TAINT_TRACE_DOMAIN: &str = "weft/taint-trace/v1";
/// Quarantine identity domain.
pub const QUARANTINE_DOMAIN: &str = "weft/quarantine/v1";
/// Namespace directory holding quarantine state.
pub const QUARANTINE_DIR: &str = "quarantine";
const QUARANTINE_RECORDS_DIR: &str = "records";
const QUARANTINE_CURRENT_DIR: &str = "current";
const QUARANTINE_POINTER_MAGIC: &[u8; 4] = b"WQP1";
const QUARANTINE_POINTER_BYTES: usize = 72;

/// Bounds. A trace that would exceed them is `LimitExceeded`, never truncated.
pub const MAX_TAINT_DEPTH: u32 = 64;
pub const MAX_TAINT_ROOTS: usize = 1_024;
pub const MAX_TAINT_NODES: usize = 65_536;
pub const MAX_TAINT_ARTIFACTS: usize = 4_096;
pub const MAX_QUARANTINE_ROOTS: usize = 1_024;

/// What a quarantine does when the artifact is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuarantineMode {
    /// Refuse exactly like an absent object: `NotFound: no such object`. The
    /// refusal discloses nothing about existence.
    NonDisclosing,
    /// Refuse with `Quarantined: ...`, disclosing that the artifact exists and
    /// is withheld.
    Explicit,
}

impl QuarantineMode {
    pub fn as_str(self) -> &'static str {
        match self {
            QuarantineMode::NonDisclosing => "non-disclosing",
            QuarantineMode::Explicit => "explicit",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "non-disclosing" => Ok(QuarantineMode::NonDisclosing),
            "explicit" => Ok(QuarantineMode::Explicit),
            other => Err(Error::invalid_argument(format!(
                "unknown quarantine mode `{other}` (non-disclosing|explicit)"
            ))),
        }
    }
}

/// Verdict for one artifact of interest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaintVerdict {
    /// A path from a tainted root exists.
    Tainted,
    /// The whole backward closure is complete and contains no tainted root.
    Clear,
    /// The graph is incomplete on the closure; nothing is claimed.
    Unproven,
}

impl TaintVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            TaintVerdict::Tainted => "tainted",
            TaintVerdict::Clear => "clear",
            TaintVerdict::Unproven => "unproven",
        }
    }
}

/// One tainted content root and how it was reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaintNode {
    pub content: Hash32,
    pub depth: u32,
    /// `root` for a declared tainted root, `provenance` for a produced output.
    pub via: String,
    /// Provenance record that produced this content, when any.
    pub record: Option<Hash32>,
    pub operation: Option<String>,
    /// Role this content played in that record's inputs.
    pub role: Option<String>,
    /// Live object ids carrying this content, sorted.
    pub objects: Vec<Hash32>,
}

/// One artifact's verdict with its evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactVerdict {
    /// The object the caller named.
    pub object: Hash32,
    pub content: Hash32,
    pub verdict: TaintVerdict,
    /// Witness: the tainted roots and records that reach it (tainted only).
    pub witness: Vec<Hash32>,
    /// Gaps that make the verdict `unproven`.
    pub gaps: Vec<String>,
}

/// The complete result of a trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaintTrace {
    pub roots: Vec<Hash32>,
    /// Sorted by `(depth, content)`.
    pub tainted: Vec<TaintNode>,
    pub artifacts: Vec<ArtifactVerdict>,
    /// Global gaps (not tied to one artifact).
    pub gaps: Vec<String>,
    pub iterations: u32,
    pub records_scanned: u64,
}

impl TaintTrace {
    /// `weft.taint-trace.v1` canonical bytes. The record is what a quarantine
    /// commits to, so it contains the verdicts and their evidence, not a count.
    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn root(&self) -> Result<Hash32> {
        Ok(identity::h(TAINT_TRACE_DOMAIN, &self.encode()?))
    }

    /// `closed` when no gap exists, `unproven` otherwise. There is no third
    /// status: an incomplete graph is stated, never inferred around.
    pub fn status(&self) -> &'static str {
        if self.gaps.is_empty()
            && self
                .artifacts
                .iter()
                .all(|artifact| artifact.verdict != TaintVerdict::Unproven)
        {
            "closed"
        } else {
            "unproven"
        }
    }

    /// Decode a canonical trace record. Exact key sets; the decoder never
    /// interprets a byte string that is not exactly the shipped record.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(
            map,
            &[
                "schema",
                "roots",
                "tainted",
                "artifacts",
                "gaps",
                "iterations",
                "records_scanned",
            ],
        )?;
        let schema = wu::get_text(map, "schema")?;
        if schema != TAINT_TRACE_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "taint trace schema `{schema}` is not `{TAINT_TRACE_SCHEMA}`"
            )));
        }
        let mut roots = Vec::new();
        for item in wu::get_array(map, "roots")? {
            match item {
                Value::Bytes(bytes) => roots.push(Hash32::from_slice(bytes)?),
                _ => return Err(Error::malformed("taint trace root must be bytes32")),
            }
        }
        let mut tainted = Vec::new();
        for item in wu::get_array(map, "tainted")? {
            let node = wu::as_map(item)?;
            wu::check_keys(
                node,
                &["content", "depth", "via", "record", "operation", "role", "objects"],
            )?;
            let objects = wu::get_array(node, "objects")?
                .iter()
                .map(|value| match value {
                    Value::Bytes(bytes) => Hash32::from_slice(bytes).map_err(Error::from),
                    _ => Err(Error::malformed("taint node object must be bytes32")),
                })
                .collect::<Result<Vec<_>>>()?;
            tainted.push(TaintNode {
                content: wu::get_hash(node, "content")?,
                depth: u32::try_from(wu::get_u64(node, "depth")?)
                    .map_err(|_| Error::malformed("taint depth does not fit u32"))?,
                via: wu::get_text(node, "via")?,
                record: wu::get_opt_hash(node, "record")?,
                operation: match node.get("operation") {
                    Some(Value::Null) => None,
                    Some(Value::Str(text)) => Some(text.clone()),
                    _ => return Err(Error::malformed("`operation` must be text or null")),
                },
                role: match node.get("role") {
                    Some(Value::Null) => None,
                    Some(Value::Str(text)) => Some(text.clone()),
                    _ => return Err(Error::malformed("`role` must be text or null")),
                },
                objects,
            });
        }
        let mut artifacts = Vec::new();
        for item in wu::get_array(map, "artifacts")? {
            let artifact = wu::as_map(item)?;
            wu::check_keys(
                artifact,
                &["object", "content", "verdict", "witness", "gaps"],
            )?;
            let verdict = match wu::get_text(artifact, "verdict")?.as_str() {
                "tainted" => TaintVerdict::Tainted,
                "clear" => TaintVerdict::Clear,
                "unproven" => TaintVerdict::Unproven,
                other => {
                    return Err(Error::malformed(format!(
                        "unknown taint verdict `{other}`"
                    )))
                }
            };
            let mut gaps = Vec::new();
            for gap in wu::get_array(artifact, "gaps")? {
                match gap {
                    Value::Str(text) => gaps.push(text.clone()),
                    _ => return Err(Error::malformed("taint gap must be text")),
                }
            }
            artifacts.push(ArtifactVerdict {
                object: wu::get_hash(artifact, "object")?,
                content: wu::get_hash(artifact, "content")?,
                verdict,
                witness: wu::get_array(artifact, "witness")?
                    .iter()
                    .map(|value| match value {
                        Value::Bytes(bytes) => Hash32::from_slice(bytes).map_err(Error::from),
                        _ => Err(Error::malformed("witness entry must be bytes32")),
                    })
                    .collect::<Result<Vec<_>>>()?,
                gaps,
            });
        }
        let mut gaps = Vec::new();
        for gap in wu::get_array(map, "gaps")? {
            match gap {
                Value::Str(text) => gaps.push(text.clone()),
                _ => return Err(Error::malformed("taint gap must be text")),
            }
        }
        Ok(TaintTrace {
            roots,
            tainted,
            artifacts,
            gaps,
            iterations: u32::try_from(wu::get_u64(map, "iterations")?)
                .map_err(|_| Error::malformed("iterations does not fit u32"))?,
            records_scanned: wu::get_u64(map, "records_scanned")?,
        })
    }

    pub fn to_value(&self) -> Value {
        wu::map([
            ("schema", Value::Str(TAINT_TRACE_SCHEMA.to_owned())),
            (
                "roots",
                Value::Array(
                    self.roots
                        .iter()
                        .map(|root| wu::hash_value(*root))
                        .collect(),
                ),
            ),
            (
                "tainted",
                Value::Array(
                    self.tainted
                        .iter()
                        .map(|node| {
                            wu::map([
                                ("content", wu::hash_value(node.content)),
                                ("depth", Value::U64(u64::from(node.depth))),
                                ("via", Value::Str(node.via.clone())),
                                ("record", wu::opt_hash_value(node.record)),
                                (
                                    "operation",
                                    match &node.operation {
                                        Some(text) => Value::Str(text.clone()),
                                        None => Value::Null,
                                    },
                                ),
                                (
                                    "role",
                                    match &node.role {
                                        Some(text) => Value::Str(text.clone()),
                                        None => Value::Null,
                                    },
                                ),
                                (
                                    "objects",
                                    Value::Array(
                                        node.objects.iter().map(|id| wu::hash_value(*id)).collect(),
                                    ),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "artifacts",
                Value::Array(
                    self.artifacts
                        .iter()
                        .map(|artifact| {
                            wu::map([
                                ("object", wu::hash_value(artifact.object)),
                                ("content", wu::hash_value(artifact.content)),
                                ("verdict", Value::Str(artifact.verdict.as_str().to_owned())),
                                (
                                    "witness",
                                    Value::Array(
                                        artifact
                                            .witness
                                            .iter()
                                            .map(|id| wu::hash_value(*id))
                                            .collect(),
                                    ),
                                ),
                                (
                                    "gaps",
                                    Value::Array(
                                        artifact
                                            .gaps
                                            .iter()
                                            .map(|gap| Value::Str(gap.clone()))
                                            .collect(),
                                    ),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "gaps",
                Value::Array(
                    self.gaps
                        .iter()
                        .map(|gap| Value::Str(gap.clone()))
                        .collect(),
                ),
            ),
            ("iterations", Value::U64(u64::from(self.iterations))),
            ("records_scanned", Value::U64(self.records_scanned)),
        ])
    }
}

/// One entry of the reverse provenance index: a record that consumed a set of
/// content roots and produced others.
struct Consumer {
    record: Hash32,
    operation: String,
    outputs: Vec<Hash32>,
    input_roles: BTreeMap<Hash32, String>,
    complete: bool,
    gaps: Vec<String>,
}

/// Build the reverse index from the live state's `provenance` relations.
///
/// Returns the consumers, per-record gaps (a record whose own evidence is
/// inconsistent), and global gaps (a relation that cannot be interpreted at
/// all, so nothing downstream of it is trustworthy).
fn consumer_index(
    store: &Store,
    state: &State,
) -> Result<(Vec<Consumer>, BTreeMap<Hash32, Vec<String>>, Vec<String>)> {
    let mut gaps = Vec::new();
    let mut ids = BTreeSet::new();
    for relation in state.relations.values() {
        if relation.predicate != PROVENANCE_PREDICATE {
            continue;
        }
        match &relation.target {
            RelationTarget::Value(Value::Str(text)) => match Hash32::from_hex(text) {
                Ok(id) => {
                    ids.insert(id);
                }
                Err(_) => gaps.push(format!(
                    "relation {} names a provenance id that is not 64 hex",
                    relation.id
                )),
            },
            _ => gaps.push(format!(
                "relation {} carries a provenance predicate without a record id",
                relation.id
            )),
        }
    }
    let mut consumers = Vec::new();
    let mut record_gaps: BTreeMap<Hash32, Vec<String>> = BTreeMap::new();
    for id in ids {
        let record = match store.read_provenance(id) {
            Ok(record) => record,
            Err(err) => {
                gaps.push(format!("provenance record {id} is unreadable: {err}"));
                continue;
            }
        };
        let mut input_roles = BTreeMap::new();
        let mut gaps_for_record: Vec<String> = Vec::new();
        for input in &record.inputs {
            input_roles.insert(input.content, input.role.clone());
            if let Some(object) = input.object {
                match state.objects.get(&object) {
                    Some(entry) if entry.content == input.content => {}
                    Some(_) => gaps_for_record.push(format!(
                        "provenance record {id} input object {object} no longer carries content {}",
                        input.content
                    )),
                    None => gaps_for_record.push(format!(
                        "provenance record {id} input object {object} is gone"
                    )),
                }
            }
        }
        if !gaps_for_record.is_empty() {
            record_gaps.insert(id, gaps_for_record.clone());
        }
        consumers.push(Consumer {
            record: id,
            operation: record.operation.clone(),
            outputs: record.outputs.clone(),
            input_roles,
            complete: gaps_for_record.is_empty(),
            gaps: gaps_for_record,
        });
    }
    consumers.sort_by_key(|consumer| consumer.record);
    Ok((consumers, record_gaps, gaps))
}

fn objects_with_content(state: &State, content: Hash32) -> Vec<Hash32> {
    let mut out: Vec<Hash32> = state
        .objects
        .values()
        .filter(|record| record.kind == ObjectKind::File && record.content == content)
        .map(|record| record.id)
        .collect();
    out.sort_unstable();
    out
}

impl Store {
    /// Trace the downstream artifacts of one or more tainted content roots.
    ///
    /// `artifacts` are object ids the caller cares about (for example two
    /// checkpoint manifests). Each receives an explicit verdict. Anything the
    /// caller did not name is still in `tainted`; nothing is silently dropped.
    pub fn taint_trace(
        &self,
        roots: &[Hash32],
        artifacts: &[Hash32],
        max_depth: u32,
    ) -> Result<TaintTrace> {
        if roots.is_empty() {
            return Err(Error::invalid_argument(
                "taint_trace requires at least one tainted root",
            ));
        }
        if roots.len() > MAX_TAINT_ROOTS || artifacts.len() > MAX_TAINT_ARTIFACTS {
            return Err(Error::limit_exceeded(format!(
                "taint_trace accepts at most {MAX_TAINT_ROOTS} roots and \
                 {MAX_TAINT_ARTIFACTS} artifacts"
            )));
        }
        let depth_limit = if max_depth == 0 {
            MAX_TAINT_DEPTH
        } else {
            max_depth.min(MAX_TAINT_DEPTH)
        };
        let state = self.current_state();
        let (consumers, record_gaps, index_gaps) = consumer_index(self, state)?;
        let mut gaps = index_gaps;

        // Forward fixpoint over provenance records.
        let mut tainted: BTreeMap<Hash32, TaintNode> = BTreeMap::new();
        let mut frontier: Vec<(Hash32, u32)> = Vec::new();
        for root in roots {
            let node = TaintNode {
                content: *root,
                depth: 0,
                via: "root".to_owned(),
                record: None,
                operation: None,
                role: None,
                objects: objects_with_content(state, *root),
            };
            if node.objects.is_empty() {
                gaps.push(format!(
                    "declared tainted root {root} has no live object in this store"
                ));
            }
            tainted.insert(*root, node);
            frontier.push((*root, 0));
        }
        let mut iterations = 0u32;
        while !frontier.is_empty() {
            if iterations >= depth_limit {
                return Err(Error::limit_exceeded(format!(
                    "taint closure exceeds depth {depth_limit}; refusing to truncate a \
                     provenance walk"
                )));
            }
            iterations += 1;
            let mut next = Vec::new();
            for consumer in &consumers {
                if !consumer.complete {
                    // Its own evidence is inconsistent, so it can neither
                    // propagate taint nor be relied on. It is recorded against
                    // the artifacts that reach it (see the per-artifact loop).
                    continue;
                }
                let hit = consumer
                    .input_roles
                    .iter()
                    .find(|(content, _)| tainted.contains_key(*content))
                    .map(|(content, role)| (*content, role.clone()));
                let Some((source, role)) = hit else {
                    continue;
                };
                let depth = tainted[&source].depth.saturating_add(1);
                for output in &consumer.outputs {
                    if tainted.contains_key(output) {
                        continue;
                    }
                    if tainted.len() >= MAX_TAINT_NODES {
                        return Err(Error::limit_exceeded(format!(
                            "taint closure exceeds {MAX_TAINT_NODES} nodes"
                        )));
                    }
                    tainted.insert(
                        *output,
                        TaintNode {
                            content: *output,
                            depth,
                            via: "provenance".to_owned(),
                            record: Some(consumer.record),
                            operation: Some(consumer.operation.clone()),
                            role: Some(role.clone()),
                            objects: objects_with_content(state, *output),
                        },
                    );
                    next.push((*output, depth));
                }
            }
            frontier = next;
        }

        // Per-artifact verdicts.
        let mut verdicts = Vec::new();
        for object in artifacts {
            let record = state.objects.get(object).ok_or_else(|| {
                Error::not_found(format!("artifact object {object} does not exist"))
            })?;
            let content = record.content;
            if let Some(node) = tainted.get(&content) {
                let mut witness = vec![content];
                if let Some(id) = node.record {
                    witness.push(id);
                    // Walk back through producers to a declared root.
                    let mut current = id;
                    let mut guard = 0;
                    loop {
                        guard += 1;
                        if guard > MAX_TAINT_DEPTH * 2 {
                            break;
                        }
                        let Some(producer) = consumers.iter().find(|c| c.record == current) else {
                            break;
                        };
                        let source = producer
                            .input_roles
                            .keys()
                            .find(|candidate| tainted.contains_key(*candidate));
                        let Some(source) = source else { break };
                        witness.push(*source);
                        match tainted[source].record {
                            Some(previous) => {
                                if previous == current {
                                    break;
                                }
                                current = previous;
                            }
                            None => break,
                        }
                    }
                }
                verdicts.push(ArtifactVerdict {
                    object: *object,
                    content,
                    verdict: TaintVerdict::Tainted,
                    witness,
                    gaps: Vec::new(),
                });
                continue;
            }
            // A taint verdict of `clear` needs a complete closure: the
            // artifact must have its own receipt, and every record on the
            // closure must have consistent evidence. A leaf with no receipt is
            // a source, not a gap — that is how provenance works. A record
            // whose input hints no longer resolve is a gap: the chain is
            // broken and nothing downstream of it is provable.
            let closure = self.provenance_closure(*object, depth_limit.max(1))?;
            let mut artifact_gaps: Vec<String> = Vec::new();
            for node in &closure.nodes {
                if tainted.contains_key(&node.content) {
                    artifact_gaps.push(format!(
                        "node {} at depth {} is tainted but was not reached forward",
                        node.content, node.depth
                    ));
                }
                if node.depth == 0 && node.provenance.is_none() {
                    artifact_gaps.push(format!(
                        "artifact {} has no provenance receipt, so its derivation is unknown",
                        node.object.unwrap_or(*object)
                    ));
                }
                if let Some(id) = node.provenance {
                    if let Some(broken) = record_gaps.get(&id) {
                        artifact_gaps.extend(broken.iter().cloned());
                    }
                }
            }
            let verdict = if artifact_gaps.is_empty() {
                TaintVerdict::Clear
            } else {
                TaintVerdict::Unproven
            };
            verdicts.push(ArtifactVerdict {
                object: *object,
                content,
                verdict,
                witness: Vec::new(),
                gaps: artifact_gaps,
            });
        }

        let mut tainted_nodes: Vec<TaintNode> = tainted.into_values().collect();
        tainted_nodes.sort_by(|a, b| (a.depth, a.content).cmp(&(b.depth, b.content)));
        verdicts.sort_by_key(|verdict| (verdict.verdict, verdict.object));
        Ok(TaintTrace {
            roots: {
                let mut sorted = roots.to_vec();
                sorted.sort_unstable();
                sorted.dedup();
                sorted
            },
            tainted: tainted_nodes,
            artifacts: verdicts,
            gaps,
            iterations,
            records_scanned: consumers.len() as u64,
        })
    }
}

// ---------------------------------------------------------------------------
// Quarantine records
// ---------------------------------------------------------------------------

/// Ed25519 signature over the record without its `signature` key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineSignature {
    pub public_key: [u8; 32],
    pub signature: [u8; 64],
}

impl QuarantineSignature {
    fn to_value(&self) -> Value {
        wu::map([
            ("algorithm", Value::Str("ed25519".to_owned())),
            ("public_key", Value::Bytes(self.public_key.to_vec())),
            ("signature", Value::Bytes(self.signature.to_vec())),
        ])
    }

    fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, &["algorithm", "public_key", "signature"])?;
        let algorithm = wu::get_text(map, "algorithm")?;
        if algorithm != "ed25519" {
            return Err(Error::malformed(format!(
                "quarantine signature algorithm `{algorithm}` is not `ed25519`"
            )));
        }
        let public_key: [u8; 32] = wu::get_bytes(map, "public_key")?
            .try_into()
            .map_err(|_| Error::malformed("quarantine public_key must be 32 bytes"))?;
        let signature: [u8; 64] = wu::get_bytes(map, "signature")?
            .try_into()
            .map_err(|_| Error::malformed("quarantine signature must be 64 bytes"))?;
        Ok(QuarantineSignature {
            public_key,
            signature,
        })
    }
}

/// One `weft.quarantine.v1` record: a quarantine or a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineRecord {
    /// `quarantine` or `release`.
    pub kind: String,
    /// The content root this record governs.
    pub content: Hash32,
    pub mode: QuarantineMode,
    pub reason: String,
    /// Root of the `weft.taint-trace.v1` record this decision came from.
    pub trace: Option<Hash32>,
    /// The trace's own status at decision time: `closed` or `unproven`.
    pub trace_status: String,
    pub policy_epoch: u64,
    pub issued_at_ns: i64,
    pub supersedes: Option<Hash32>,
    pub signature: Option<QuarantineSignature>,
}

impl QuarantineRecord {
    pub const KEYS: &'static [&'static str] = &[
        "schema",
        "kind",
        "content",
        "mode",
        "reason",
        "trace",
        "trace_status",
        "policy_epoch",
        "issued_at",
        "supersedes",
        "signature",
    ];

    pub fn is_quarantine(&self) -> bool {
        self.kind == "quarantine"
    }

    pub fn to_value_without_signature(&self) -> Value {
        wu::map([
            ("schema", Value::Str(QUARANTINE_SCHEMA.to_owned())),
            ("kind", Value::Str(self.kind.clone())),
            ("content", wu::hash_value(self.content)),
            ("mode", Value::Str(self.mode.as_str().to_owned())),
            ("reason", Value::Str(self.reason.clone())),
            ("trace", wu::opt_hash_value(self.trace)),
            ("trace_status", Value::Str(self.trace_status.clone())),
            ("policy_epoch", Value::U64(self.policy_epoch)),
            ("issued_at", Value::Timestamp(self.issued_at_ns)),
            ("supersedes", wu::opt_hash_value(self.supersedes)),
        ])
    }

    pub fn to_value(&self) -> Value {
        let mut map = match self.to_value_without_signature() {
            Value::Map(map) => map,
            _ => unreachable!("map constructor returns a map"),
        };
        map.insert(
            "signature".to_owned(),
            match &self.signature {
                Some(signature) => signature.to_value(),
                None => Value::Null,
            },
        );
        Value::Map(map)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value())
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        wu::encode(&self.to_value_without_signature())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value = wu::decode(bytes)?;
        let record = Self::from_value(&value)?;
        record.verify_signature()?;
        Ok(record)
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let map = wu::as_map(value)?;
        wu::check_keys(map, Self::KEYS)?;
        let schema = wu::get_text(map, "schema")?;
        if schema != QUARANTINE_SCHEMA {
            return Err(Error::unsupported_version(format!(
                "quarantine schema `{schema}` is not `{QUARANTINE_SCHEMA}`"
            )));
        }
        let kind = wu::get_text(map, "kind")?;
        if kind != "quarantine" && kind != "release" {
            return Err(Error::malformed(format!(
                "quarantine record kind `{kind}` is not `quarantine` or `release`"
            )));
        }
        let record = QuarantineRecord {
            kind,
            content: wu::get_hash(map, "content")?,
            mode: QuarantineMode::parse(&wu::get_text(map, "mode")?)?,
            reason: wu::get_text(map, "reason")?,
            trace: wu::get_opt_hash(map, "trace")?,
            trace_status: wu::get_text(map, "trace_status")?,
            policy_epoch: wu::get_u64(map, "policy_epoch")?,
            issued_at_ns: wu::get_timestamp(map, "issued_at")?,
            supersedes: wu::get_opt_hash(map, "supersedes")?,
            signature: match map.get("signature") {
                Some(Value::Null) => None,
                Some(value) => Some(QuarantineSignature::from_value(value)?),
                None => return Err(Error::malformed("`signature` is missing")),
            },
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<()> {
        if self.reason.is_empty() {
            return Err(Error::malformed("quarantine reason must not be empty"));
        }
        if self.reason.len() > 8_192 {
            return Err(Error::limit_exceeded(
                "quarantine reason exceeds 8192 bytes",
            ));
        }
        if self.trace_status != "closed" && self.trace_status != "unproven" {
            return Err(Error::malformed(format!(
                "quarantine trace_status `{}` is not `closed` or `unproven`",
                self.trace_status
            )));
        }
        if self.kind == "release" && self.trace.is_some() {
            return Err(Error::malformed(
                "a release record does not carry a trace root",
            ));
        }
        Ok(())
    }

    pub fn id(&self) -> Result<Hash32> {
        Ok(identity::h(QUARANTINE_DOMAIN, &self.encode()?))
    }

    pub fn sign(&mut self, seed: &[u8; 32]) -> Result<()> {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(seed);
        let signature = key.sign(&self.signing_bytes()?);
        self.signature = Some(QuarantineSignature {
            public_key: key.verifying_key().to_bytes(),
            signature: signature.to_bytes(),
        });
        Ok(())
    }

    pub fn verify_signature(&self) -> Result<()> {
        let Some(signature) = &self.signature else {
            return Ok(());
        };
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let key = VerifyingKey::from_bytes(&signature.public_key)
            .map_err(|err| Error::corrupt(format!("quarantine verification key: {err}")))?;
        let parsed = Signature::from_bytes(&signature.signature);
        key.verify(&self.signing_bytes()?, &parsed)
            .map_err(|_| Error::corrupt("quarantine signature does not verify"))
    }

    /// Require a signature from a member of `authority`. An empty authority set
    /// means no one may quarantine or lift: the operation is `Unauthorized`.
    pub fn verify_authority(&self, authority: &[[u8; 32]]) -> Result<()> {
        let Some(signature) = &self.signature else {
            return Err(Error::unauthorized(
                "a quarantine decision must be signed by an authorized principal",
            ));
        };
        if authority.is_empty() {
            return Err(Error::unauthorized(
                "no quarantine authority is configured; quarantine and release are refused",
            ));
        }
        self.verify_signature()?;
        if authority.iter().any(|key| key == &signature.public_key) {
            Ok(())
        } else {
            Err(Error::unauthorized(
                "quarantine signer is not in the configured authority set",
            ))
        }
    }
}

/// A request to quarantine or release one content root.
#[derive(Clone, Debug)]
pub struct QuarantineRequest {
    pub content: Hash32,
    pub mode: QuarantineMode,
    pub reason: String,
    pub trace: Option<Hash32>,
    pub trace_status: String,
    pub policy_epoch: u64,
    pub issued_at_ns: Option<i64>,
    /// The signer's key. Required.
    pub seed: [u8; 32],
    /// Principals allowed to make this decision.
    pub authority: Vec<[u8; 32]>,
    /// Set for a release: the quarantine record being lifted.
    pub release_of: Option<Hash32>,
}

/// The 64-byte pointer stored per content root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantinePointer {
    pub content: Hash32,
    pub record: Hash32,
}

impl QuarantinePointer {
    pub fn encode(&self) -> [u8; QUARANTINE_POINTER_BYTES] {
        let mut out = [0u8; QUARANTINE_POINTER_BYTES];
        out[..4].copy_from_slice(QUARANTINE_POINTER_MAGIC);
        out[8..40].copy_from_slice(self.content.as_bytes());
        out[40..72].copy_from_slice(self.record.as_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != QUARANTINE_POINTER_BYTES {
            return Err(Error::corrupt(format!(
                "quarantine pointer is {} bytes, expected {QUARANTINE_POINTER_BYTES}",
                bytes.len()
            )));
        }
        if &bytes[..4] != QUARANTINE_POINTER_MAGIC {
            return Err(Error::corrupt("quarantine pointer magic is not `WQP1`"));
        }
        if bytes[4..8] != [0, 0, 0, 0] {
            return Err(Error::corrupt("quarantine pointer reserved is not zero"));
        }
        debug_assert_eq!(QUARANTINE_POINTER_BYTES, 72);
        // 4-byte magic + 4 reserved + content root + record id = 72 bytes.
        Ok(QuarantinePointer {
            content: Hash32::from_slice(&bytes[8..40]).map_err(Error::from)?,
            record: Hash32::from_slice(&bytes[40..72]).map_err(Error::from)?,
        })
    }
}

impl Store {
    fn ensure_quarantine_dir(&mut self, parent: Hash32, name: &str) -> Result<Hash32> {
        match self.lookup(parent, name.as_bytes()) {
            Ok(id) => {
                if self.object_meta(id)?.kind != ObjectKind::Dir {
                    return Err(Error::not_dir(format!(
                        "`{name}` exists and is not a directory"
                    )));
                }
                Ok(id)
            }
            Err(err) if err.class() == Class::NotFound => {
                let uid = self.options().owner_uid;
                let gid = self.options().owner_gid;
                self.mkdir(parent, name.as_bytes(), 0o755, uid, gid)
            }
            Err(err) => Err(err),
        }
    }

    fn quarantine_dirs(&mut self) -> Result<(Hash32, Hash32)> {
        let root = self.resolve_path("/")?;
        let quarantine = self.ensure_quarantine_dir(root, QUARANTINE_DIR)?;
        let records = self.ensure_quarantine_dir(quarantine, QUARANTINE_RECORDS_DIR)?;
        let current = self.ensure_quarantine_dir(quarantine, QUARANTINE_CURRENT_DIR)?;
        Ok((records, current))
    }

    fn quarantine_current_dir(&self) -> Result<Option<Hash32>> {
        let root = self.resolve_path("/")?;
        let quarantine = match self.lookup(root, QUARANTINE_DIR.as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        match self.lookup(quarantine, QUARANTINE_CURRENT_DIR.as_bytes()) {
            Ok(id) => Ok(Some(id)),
            Err(err) if err.class() == Class::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Read one quarantine record by id.
    pub fn quarantine_record(&self, id: Hash32) -> Result<QuarantineRecord> {
        let root = self.resolve_path("/")?;
        let quarantine = match self.lookup(root, QUARANTINE_DIR.as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => {
                return Err(Error::not_found(format!(
                    "quarantine record {id} does not exist"
                )))
            }
            Err(err) => return Err(err),
        };
        let records = match self.lookup(quarantine, QUARANTINE_RECORDS_DIR.as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => {
                return Err(Error::not_found(format!(
                    "quarantine record {id} does not exist"
                )))
            }
            Err(err) => return Err(err),
        };
        let name = id.to_hex();
        let object = self.lookup(records, name.as_bytes())?;
        let bytes = self.read_object_exempt(object)?;
        let record = QuarantineRecord::decode(&bytes)?;
        if record.id()? != id {
            return Err(Error::corrupt(format!(
                "quarantine record {id} does not re-encode to its own identity"
            )));
        }
        Ok(record)
    }

    /// The current decision for a content root, if any.
    pub fn quarantine_of(&self, content: Hash32) -> Result<Option<(Hash32, QuarantineRecord)>> {
        let Some(dir) = self.quarantine_current_dir()? else {
            return Ok(None);
        };
        let name = content.to_hex();
        let object = match self.lookup(dir, name.as_bytes()) {
            Ok(id) => id,
            Err(err) if err.class() == Class::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let bytes = self.read_object_exempt(object)?;
        let pointer = QuarantinePointer::decode(&bytes)?;
        if pointer.content != content {
            return Err(Error::corrupt(format!(
                "quarantine pointer `{name}` names content {}",
                pointer.content
            )));
        }
        let record = self.quarantine_record(pointer.record)?;
        if record.content != content {
            return Err(Error::corrupt(
                "quarantine record names a different content root than its pointer",
            ));
        }
        Ok(Some((pointer.record, record)))
    }

    /// True when `content` is under an active quarantine.
    pub fn is_quarantined(&self, content: Hash32) -> Result<bool> {
        Ok(self
            .quarantine_of(content)?
            .map(|(_, record)| record.is_quarantine())
            .unwrap_or(false))
    }

    /// The refusal for a content root, or `None` when it may be served.
    pub(crate) fn quarantine_denial(&self, content: Hash32) -> Result<Option<Error>> {
        let Some((_, record)) = self.quarantine_of(content)? else {
            return Ok(None);
        };
        if !record.is_quarantine() {
            return Ok(None);
        }
        Ok(Some(match record.mode {
            // Byte-identical to an absent object: the refusal discloses nothing.
            QuarantineMode::NonDisclosing => Error::not_found("no such object"),
            QuarantineMode::Explicit => Error::quarantined(format!(
                "artifact content {} is quarantined: {}",
                content, record.reason
            )),
        }))
    }

    /// Quarantine one content root.
    ///
    /// The record is signed; the signer must be a member of `request.authority`.
    /// A quarantine is append-only and epoch-fenced: a second record for the
    /// same content must name the current record in `release_of`/`supersedes`
    /// terms, which is handled by [`Store::release_quarantine`].
    pub fn quarantine(&mut self, request: QuarantineRequest) -> Result<Hash32> {
        self.require_writable()?;
        if request.release_of.is_some() {
            return Err(Error::invalid_argument(
                "use release_quarantine to lift a quarantine",
            ));
        }
        if let Some((current, record)) = self.quarantine_of(request.content)? {
            if record.is_quarantine() {
                let held = record.policy_epoch;
                if request.policy_epoch <= held {
                    return Err(Error::conflict(format!(
                        "content {} is already quarantined at policy epoch {held}; a new \
                         decision must name a higher epoch",
                        request.content
                    )));
                }
                let mut record = QuarantineRecord {
                    kind: "quarantine".to_owned(),
                    content: request.content,
                    mode: request.mode,
                    reason: request.reason,
                    trace: request.trace,
                    trace_status: request.trace_status,
                    policy_epoch: request.policy_epoch,
                    issued_at_ns: 0,
                    supersedes: Some(current),
                    signature: None,
                };
                record.issued_at_ns = request.issued_at_ns.unwrap_or_else(|| self.now_ns());
                record.validate()?;
                record.sign(&request.seed)?;
                record.verify_authority(&request.authority)?;
                return self.publish_quarantine(record);
            }
        }
        let mut record = QuarantineRecord {
            kind: "quarantine".to_owned(),
            content: request.content,
            mode: request.mode,
            reason: request.reason,
            trace: request.trace,
            trace_status: request.trace_status,
            policy_epoch: request.policy_epoch,
            issued_at_ns: request.issued_at_ns.unwrap_or_else(|| self.now_ns()),
            supersedes: None,
            signature: None,
        };
        record.validate()?;
        record.sign(&request.seed)?;
        record.verify_authority(&request.authority)?;
        self.publish_quarantine(record)
    }

    /// Lift a quarantine with a signed, epoch-fenced release record.
    pub fn release_quarantine(&mut self, request: QuarantineRequest) -> Result<Hash32> {
        self.require_writable()?;
        let Some((current, record)) = self.quarantine_of(request.content)? else {
            return Err(Error::not_found(format!(
                "content {} is not quarantined",
                request.content
            )));
        };
        if !record.is_quarantine() {
            return Err(Error::conflict(format!(
                "content {} is already released (record {current})",
                request.content
            )));
        }
        if request.policy_epoch <= record.policy_epoch {
            return Err(Error::conflict(format!(
                "a release must name an epoch above the active quarantine's epoch {}",
                record.policy_epoch
            )));
        }
        if let Some(expected) = request.release_of {
            if expected != current {
                return Err(Error::conflict(format!(
                    "release names record {expected} but content {} is held by {current}",
                    request.content
                )));
            }
        }
        let mut release = QuarantineRecord {
            kind: "release".to_owned(),
            content: request.content,
            mode: record.mode,
            reason: request.reason,
            trace: None,
            trace_status: "closed".to_owned(),
            policy_epoch: request.policy_epoch,
            issued_at_ns: request.issued_at_ns.unwrap_or_else(|| self.now_ns()),
            supersedes: Some(current),
            signature: None,
        };
        release.validate()?;
        release.sign(&request.seed)?;
        release.verify_authority(&request.authority)?;
        self.publish_quarantine(release)
    }

    fn publish_quarantine(&mut self, record: QuarantineRecord) -> Result<Hash32> {
        let bytes = record.encode()?;
        let id = record.id()?;
        let (records_dir, current_dir) = self.quarantine_dirs()?;
        let uid = self.options().owner_uid;
        let gid = self.options().owner_gid;
        let name = id.to_hex();
        match self.create_file(
            records_dir,
            name.as_bytes(),
            &bytes,
            0o644,
            uid,
            gid,
            BTreeMap::new(),
        ) {
            Ok(_) => {}
            Err(err) if err.class() == Class::Exists => {}
            Err(err) => return Err(err),
        }
        let pointer = QuarantinePointer {
            content: record.content,
            record: id,
        }
        .encode();
        let content_name = record.content.to_hex();
        // The pointer is replaced, not appended: one current decision per
        // content root, with the full history reachable through `supersedes`.
        self.put_quarantine_pointer(current_dir, &content_name, &pointer, uid, gid)?;
        Ok(id)
    }

    fn put_quarantine_pointer(
        &mut self,
        dir: Hash32,
        name: &str,
        pointer: &[u8; QUARANTINE_POINTER_BYTES],
        uid: u64,
        gid: u64,
    ) -> Result<()> {
        match self.lookup(dir, name.as_bytes()) {
            Ok(object) => {
                self.write_file(object, 0, pointer)?;
                Ok(())
            }
            Err(err) if err.class() == Class::NotFound => {
                self.create_file(
                    dir,
                    name.as_bytes(),
                    pointer,
                    0o644,
                    uid,
                    gid,
                    BTreeMap::new(),
                )?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Every current quarantine decision for the store, sorted by content root.
    pub fn quarantine_entries(&self) -> Result<Vec<(Hash32, Hash32, QuarantineRecord)>> {
        let Some(dir) = self.quarantine_current_dir()? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in self.list_dir(dir)? {
            let bytes = self.read_object_exempt(entry.target)?;
            let pointer = QuarantinePointer::decode(&bytes)?;
            let record_id = pointer.record;
            let record = self.quarantine_record(record_id)?;
            if record.is_quarantine() {
                out.push((pointer.content, record_id, record));
            }
        }
        out.sort_by_key(|(content, _, _)| *content);
        Ok(out)
    }

}

/// Set of object ids whose content is tainted, for a caller that wants to act on
/// the whole trace rather than one artifact.
pub fn tainted_objects(trace: &TaintTrace) -> BTreeSet<Hash32> {
    let mut out = BTreeSet::new();
    for node in &trace.tainted {
        out.extend(node.objects.iter().copied());
    }
    out
}
