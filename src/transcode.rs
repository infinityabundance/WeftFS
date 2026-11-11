//! Background transcoding: fast-save first, planned representation later.
//!
//! A training save takes the critical-path write policy ([`WritePolicy::Fast`]):
//! each tensor is stored as raw chunks or one fast ZSTD frame, so the save
//! returns after a linear pass over the bytes. This module is the
//! LSM-compaction-like second half: a **bounded, opt-in, exclusive** pass that
//! walks the live subtree, prices a standalone planned representation for
//! objects whose current representation is a fast one, and republishes the
//! smaller representation.
//!
//! Safety properties:
//!
//! * A rewrite is `Store::rebase_tensor_object`: a **new version** of the same
//!   object id through the normal commit protocol. The old version stays
//!   reachable for every snapshot and for the causal history, so a crash at
//!   any point leaves either the old or the new representation, never a
//!   partial one.
//! * The logical bytes are read through the verifying object path, the planned
//!   candidate is encoded from those exact bytes, and (by default) the new
//!   version is read back and compared byte for byte before the pass reports
//!   success. Any mismatch is a typed `Corrupt` error and the pass stops.
//! * Budgets are hard: scanned objects, logical bytes read, wall milliseconds,
//!   and a cancellation flag. Each is checked before any work starts on the
//!   next object.
//! * A provenance record (`weft.provenance.v1`, `operation: "transcode"`)
//!   names the old content root, the new content root and the policy.
//!
//! The pass is exclusive in the same sense as `gc --apply`: it opens the store
//! writable, which takes the single-writer lock. It is meant to run after the
//! save (or from a separate process) rather than concurrently with the writer.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use weftcodec::wcb::Value;

use crate::cost_model::CostModelKind;
use crate::error::{Error, Result};
use crate::format::{Manifest, RepresentationKind};
use crate::identity::Hash32;
use crate::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use crate::recovery;
use crate::state::ObjectKind;
use crate::store::Store;
use crate::tensor_store::{
    TensorIndependentPlan, TensorPlanOptions, TensorStrategy, TensorWriteSpec, WritePolicy,
};

/// Tool name recorded in a transcode provenance record.
pub const TRANSCODE_TOOL: &str = "weftfs transcode";
/// Default minimum fraction of the current physical bytes a rewrite must save.
pub const DEFAULT_MIN_SAVED_PERCENT: u8 = 1;

/// Hard bounds and knobs for one transcode pass.
#[derive(Clone, Debug)]
pub struct TranscodeOptions {
    /// Maximum live objects examined.
    pub max_objects: usize,
    /// Maximum logical bytes read into the pass.
    pub max_bytes: u64,
    /// Maximum wall time for the pass. `0` disables the wall bound.
    pub max_millis: u64,
    /// Rewrite only when the planned representation saves at least this
    /// percentage of the current physical bytes. `0` accepts any strictly
    /// smaller representation.
    pub min_saved_percent: u8,
    /// Price the plan and report it without publishing.
    pub dry_run: bool,
    /// Force a standalone planned strategy instead of the bytes-only plan.
    pub strategy_hint: Option<TensorStrategy>,
    /// Cooperative cancellation, checked between objects.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Maximum codec dependency depth when pricing a plan.
    pub max_candidates: usize,
    /// Cost model for the preliminary decode estimate (planner pricing).
    pub cost_model: CostModelKind,
    /// Optional host calibration replacing the baked decode table.
    pub calibration: Option<Arc<crate::cost_model::CostTable>>,
    /// Bytes charged per estimated nanosecond of decode time.
    pub lambda_ns: u64,
    /// Read the republished version back and compare the logical bytes.
    pub verify_after: bool,
    /// Test hook: abort the process immediately after this many publications
    /// (integer `usize::MAX` disables it). Used by the crash proxy.
    pub crash_after_publishes: Option<usize>,
    /// Test hook: abort the process just before publishing candidate `N`.
    pub crash_before_publish: Option<usize>,
}

impl Default for TranscodeOptions {
    fn default() -> Self {
        TranscodeOptions {
            max_objects: 100_000,
            max_bytes: u64::MAX,
            max_millis: 0,
            min_saved_percent: DEFAULT_MIN_SAVED_PERCENT,
            dry_run: false,
            strategy_hint: None,
            cancel: None,
            max_candidates: crate::tensor_store::DEFAULT_TENSOR_MAX_CANDIDATES,
            cost_model: CostModelKind::default(),
            calibration: None,
            lambda_ns: 0,
            verify_after: true,
            crash_after_publishes: None,
            crash_before_publish: None,
        }
    }
}

/// One object the pass decided to rewrite.
#[derive(Clone, Debug)]
pub struct TranscodeCandidate {
    pub object: Hash32,
    pub path: String,
    pub from_strategy: String,
    pub to_strategy: TensorStrategy,
    pub logical_bytes: u64,
    pub old_physical_bytes: u64,
    pub new_physical_bytes: u64,
    pub estimated_decode_ns: u64,
    pub candidates: Vec<(String, u64, u64)>,
}

/// One object the pass skipped, with the reason.
#[derive(Clone, Debug)]
pub struct TranscodeSkip {
    pub object: Hash32,
    pub path: String,
    pub reason: String,
}

/// One completed rewrite.
#[derive(Clone, Debug)]
pub struct TranscodeOutcome {
    pub object: Hash32,
    pub path: String,
    pub old_version: Hash32,
    pub new_version: Hash32,
    pub content: Hash32,
    pub from_strategy: String,
    pub to_strategy: TensorStrategy,
    pub old_physical_bytes: u64,
    pub new_physical_bytes: u64,
    pub provenance_id: Hash32,
    pub verified: bool,
}

/// Result of one bounded pass.
#[derive(Clone, Debug)]
pub struct TranscodeReport {
    pub root: Hash32,
    pub scanned: usize,
    pub truncated: bool,
    pub dry_run: bool,
    pub candidates: Vec<TranscodeCandidate>,
    pub skipped: Vec<TranscodeSkip>,
    pub applied: Vec<TranscodeOutcome>,
    pub bytes_read: u64,
    pub elapsed_ms: f64,
    /// Why the pass stopped before exhausting the subtree, when it did.
    pub stopped_reason: Option<String>,
}

impl Store {
    /// Run one bounded transcode pass over the live subtree rooted at `root`.
    pub fn transcode(
        &mut self,
        root: Hash32,
        options: &TranscodeOptions,
    ) -> Result<TranscodeReport> {
        let started = Instant::now();
        let objects = self.collect_transcode_subtree(root, options.max_objects)?;
        let truncated = objects.len() >= options.max_objects;
        if !options.dry_run {
            self.require_writable()?;
        }
        let plan_options = TensorPlanOptions {
            lambda_ns: options.lambda_ns,
            cost_model: options.cost_model,
            calibration: options.calibration.clone(),
            max_candidates: options.max_candidates,
            write_policy: WritePolicy::Planned,
            ..TensorPlanOptions::default()
        };
        let mut report = TranscodeReport {
            root,
            scanned: 0,
            truncated,
            dry_run: options.dry_run,
            candidates: Vec::new(),
            skipped: Vec::new(),
            applied: Vec::new(),
            bytes_read: 0,
            elapsed_ms: 0.0,
            stopped_reason: None,
        };
        let mut fast_objects_seen = 0usize;
        for (object, path) in objects {
            if let Some(reason) = self.transcode_budget_reached(&report, options, started) {
                report.stopped_reason = Some(reason);
                break;
            }
            report.scanned += 1;
            let record = match self.current_state().objects.get(&object) {
                Some(record) => record.clone(),
                None => continue,
            };
            if record.kind != ObjectKind::File {
                continue;
            }
            let _meta = match self.tensor_metadata(object) {
                Ok(meta) => meta,
                Err(err) if err.class() == crate::error::Class::NotFound => continue,
                Err(err) => {
                    report.skipped.push(TranscodeSkip {
                        object,
                        path,
                        reason: format!("tensor metadata unreadable: {err}"),
                    });
                    continue;
                }
            };
            let manifest =
                self.read_manifest_keyed(record.content)?;
            let from_strategy = manifest
                .representation
                .as_ref()
                .map(|rep| rep.strategy.clone())
                .unwrap_or_else(|| "raw".to_owned());
            // Only a linear-pass representation is a transcode source. A
            // planned object is left alone, and a delta cannot be rebased
            // without dropping its dependency.
            let fast_source = matches!(from_strategy.as_str(), "raw" | "zstd");
            if !fast_source {
                continue;
            }
            let old_physical = self.representation_physical_bytes(&manifest)?;
            if report.bytes_read.saturating_add(record.size) > options.max_bytes {
                report.stopped_reason = Some(format!(
                    "byte budget reached: {} + {} > {} logical bytes",
                    report.bytes_read, record.size, options.max_bytes
                ));
                break;
            }
            report.bytes_read = report.bytes_read.saturating_add(record.size);

            if options.dry_run {
                let independent = match self.plan_tensor_independent(
                    object,
                    &plan_options,
                    options.strategy_hint,
                ) {
                    Ok(plan) => plan,
                    Err(err) => {
                        report.skipped.push(TranscodeSkip {
                            object,
                            path,
                            reason: format!("planned representation could not be priced: {err}"),
                        });
                        continue;
                    }
                };
                if independent.chosen == TensorStrategy::Raw && from_strategy == "raw" {
                    continue;
                }
                let decision = transcode_decision(
                    &independent,
                    from_strategy.as_str(),
                    old_physical,
                    record.size,
                    options.min_saved_percent,
                );
                match decision {
                    Ok(()) => report.candidates.push(TranscodeCandidate {
                        object,
                        path,
                        from_strategy,
                        to_strategy: independent.chosen,
                        logical_bytes: record.size,
                        old_physical_bytes: old_physical,
                        new_physical_bytes: independent.physical_bytes,
                        estimated_decode_ns: independent.estimated_decode_ns,
                        candidates: candidate_rows(&independent),
                    }),
                    Err(reason) => report.skipped.push(TranscodeSkip {
                        object,
                        path,
                        reason,
                    }),
                }
                continue;
            }

            if options.crash_before_publish == Some(fast_objects_seen) {
                // Test hook: die between the plan and the publication.
                std::process::abort();
            }
            fast_objects_seen += 1;

            // One encode pass: the decision is made from the priced plan and a
            // rejected plan costs no publication.
            let old_bytes = if options.verify_after {
                Some(self.read_object_uncounted(object)?)
            } else {
                None
            };
            let decision = match self.rebase_tensor_object_decided(
                object,
                &plan_options,
                options.strategy_hint,
                Some(old_physical),
                options.min_saved_percent,
            ) {
                Ok(decision) => decision,
                Err(err) => {
                    report.skipped.push(TranscodeSkip {
                        object,
                        path,
                        reason: format!("planned representation could not be priced: {err}"),
                    });
                    continue;
                }
            };
            let independent = decision.plan;
            if independent.chosen == TensorStrategy::Raw && from_strategy == "raw" {
                continue;
            }
            let Some(rebase) = decision.rebase else {
                let reason = transcode_decision(
                    &independent,
                    from_strategy.as_str(),
                    old_physical,
                    record.size,
                    options.min_saved_percent,
                )
                .err()
                .unwrap_or_else(|| "planned representation was not published".to_owned());
                report.skipped.push(TranscodeSkip {
                    object,
                    path,
                    reason,
                });
                continue;
            };
            let candidate = TranscodeCandidate {
                object,
                path: path.clone(),
                from_strategy: from_strategy.clone(),
                to_strategy: rebase.plan.chosen,
                logical_bytes: record.size,
                old_physical_bytes: old_physical,
                new_physical_bytes: rebase.plan.physical_bytes,
                estimated_decode_ns: rebase.plan.estimated_decode_ns,
                candidates: candidate_rows(&independent),
            };
            report.candidates.push(candidate.clone());
            let mut verified = false;
            if let Some(old_bytes) = old_bytes {
                let new_bytes = self.read_object_uncounted(object)?;
                if new_bytes != old_bytes {
                    return Err(Error::corrupt(format!(
                        "transcode of object {object} produced different logical bytes; \
                         the previous version is still reachable"
                    )));
                }
                verified = true;
            }
            let record = self.transcode_provenance(&candidate, &rebase)?;
            let provenance_id = self.record_provenance(&[object], &record)?;
            report.applied.push(TranscodeOutcome {
                object,
                path,
                old_version: rebase.old_version,
                new_version: rebase.new_version,
                content: rebase.new_content,
                from_strategy: rebase.old_strategy,
                to_strategy: rebase.plan.chosen,
                old_physical_bytes: candidate.old_physical_bytes,
                new_physical_bytes: rebase.plan.physical_bytes,
                provenance_id,
                verified,
            });
            if options.crash_after_publishes == Some(report.applied.len()) {
                // Test hook: die immediately after a durable publication.
                std::process::abort();
            }
        }
        if !options.dry_run && !report.applied.is_empty() {
            self.checkpoint()?;
        }
        if report.stopped_reason.is_none() && report.truncated {
            report.stopped_reason = Some(format!(
                "object budget reached during the subtree walk: {}",
                options.max_objects
            ));
        }
        report.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(report)
    }

    fn transcode_budget_reached(
        &self,
        report: &TranscodeReport,
        options: &TranscodeOptions,
        started: Instant,
    ) -> Option<String> {
        if report.scanned >= options.max_objects {
            return Some(format!("object budget reached: {}", options.max_objects));
        }
        if let Some(cancel) = &options.cancel {
            if cancel.load(Ordering::Relaxed) {
                return Some("cancelled by the caller".to_owned());
            }
        }
        if options.max_millis > 0 && started.elapsed().as_millis() as u64 >= options.max_millis {
            return Some(format!("wall budget reached: {} ms", options.max_millis));
        }
        None
    }

    /// Physical bytes a manifest's current representation occupies: the frame
    /// blob plus every dependency frame, or every chunk blob for a raw object.
    pub fn representation_physical_bytes(&self, manifest: &Manifest) -> Result<u64> {
        match &manifest.representation {
            None => self.chunk_table_physical_bytes(manifest),
            Some(rep) => {
                if rep.kind == RepresentationKind::Raw {
                    return self.chunk_table_physical_bytes(manifest);
                }
                let mut total = 0u64;
                if let Some(frame) = rep.frame {
                    total = total.saturating_add(self.with_backend(|backend| {
                        backend.len(&crate::backend::Key::manifest(frame))
                    })?);
                }
                for dependency in &rep.dependencies {
                    let dependency_manifest = self.read_manifest_keyed(*dependency)?;
                    total = total
                        .saturating_add(self.representation_physical_bytes(&dependency_manifest)?);
                }
                Ok(total)
            }
        }
    }

    fn chunk_table_physical_bytes(&self, manifest: &Manifest) -> Result<u64> {
        let mut total = 0u64;
        for chunk in &manifest.chunks {
            total =
                total.saturating_add(self.with_backend(|backend| {
                    backend.len(&crate::backend::Key::object(chunk.hash))
                })?);
        }
        Ok(total)
    }

    fn transcode_provenance(
        &self,
        candidate: &TranscodeCandidate,
        rebase: &crate::tensor_store::TensorRebase,
    ) -> Result<ProvenanceRecord> {
        let mut params = BTreeMap::new();
        params.insert(
            "from_strategy".to_owned(),
            Value::Str(candidate.from_strategy.clone()),
        );
        params.insert(
            "to_strategy".to_owned(),
            Value::Str(rebase.plan.chosen.as_str().to_owned()),
        );
        params.insert(
            "old_physical_bytes".to_owned(),
            Value::U64(candidate.old_physical_bytes),
        );
        params.insert(
            "new_physical_bytes".to_owned(),
            Value::U64(rebase.plan.physical_bytes),
        );
        params.insert(
            "logical_bytes".to_owned(),
            Value::U64(candidate.logical_bytes),
        );
        params.insert(
            "old_version".to_owned(),
            Value::Bytes(rebase.old_version.as_bytes().to_vec()),
        );
        params.insert(
            "new_version".to_owned(),
            Value::Bytes(rebase.new_version.as_bytes().to_vec()),
        );
        let inputs = vec![ProvenanceInput::new(
            "source",
            rebase.old_content,
            Some(candidate.object),
        )];
        let environment = ProvenanceEnvironment {
            tool: TRANSCODE_TOOL.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            env_hash: None,
            fields: BTreeMap::new(),
        };
        let principal =
            ProvenancePrincipal::new(self.options().owner_uid, self.options().owner_gid);
        ProvenanceRecord::new(
            "transcode",
            params,
            inputs,
            vec![rebase.new_content],
            environment,
            Determinism::Exact,
            principal,
            self.clock_now_ns(),
        )
    }

    /// Depth-first walk of the live subtree rooted at `root`.
    fn collect_transcode_subtree(
        &self,
        root: Hash32,
        limit: usize,
    ) -> Result<Vec<(Hash32, String)>> {
        let mut out = Vec::new();
        let mut stack = vec![(root, "/".to_owned())];
        while let Some((object, path)) = stack.pop() {
            if out.len() >= limit {
                break;
            }
            let record = match self.current_state().objects.get(&object) {
                Some(record) => record,
                None => continue,
            };
            match record.kind {
                ObjectKind::Dir => {
                    let mut children = self.list_dir(object)?;
                    children.sort_by(|a, b| a.name.cmp(&b.name));
                    for child in children.into_iter().rev() {
                        let name = String::from_utf8_lossy(&child.name).into_owned();
                        let child_path = if path == "/" {
                            format!("/{name}")
                        } else {
                            format!("{path}/{name}")
                        };
                        stack.push((child.target, child_path));
                    }
                }
                ObjectKind::File => out.push((object, path)),
                ObjectKind::Symlink => {}
            }
        }
        Ok(out)
    }
}

/// Rows `(strategy, physical_bytes, estimated_decode_ns)` for a plan.
fn candidate_rows(plan: &TensorIndependentPlan) -> Vec<(String, u64, u64)> {
    plan.candidates
        .iter()
        .map(|item| {
            (
                item.strategy.as_str().to_owned(),
                item.physical_bytes.unwrap_or(0),
                item.estimated_decode_ns.unwrap_or(0),
            )
        })
        .collect()
}

/// Apply the transcode acceptance rule to a priced plan. `Err` carries the
/// human-readable reason the plan was skipped.
fn transcode_decision(
    plan: &TensorIndependentPlan,
    from_strategy: &str,
    old_physical: u64,
    logical_bytes: u64,
    min_saved_percent: u8,
) -> std::result::Result<(), String> {
    if plan.chosen == TensorStrategy::Raw && from_strategy == "raw" {
        return Err("already stored raw".to_owned());
    }
    if plan.physical_bytes >= old_physical {
        return Err(format!(
            "planned `{}` is not smaller ({} vs {} physical bytes)",
            plan.chosen.as_str(),
            plan.physical_bytes,
            old_physical
        ));
    }
    let saved = old_physical - plan.physical_bytes;
    if min_saved_percent > 0
        && saved.saturating_mul(100) < old_physical.saturating_mul(u64::from(min_saved_percent))
    {
        return Err(format!(
            "planned `{}` saves {saved} bytes, below the {min_saved_percent}% threshold",
            plan.chosen.as_str()
        ));
    }
    if plan.physical_bytes > logical_bytes {
        return Err(format!(
            "planned representation is larger than the logical bytes ({} > {logical_bytes})",
            plan.physical_bytes
        ));
    }
    Ok(())
}

/// Convenience: no budget except the object cap.
pub fn transcode_options() -> TranscodeOptions {
    TranscodeOptions::default()
}

/// The write spec a fast save uses (exposed for callers that write tensors
/// outside the checkpoint API).
pub fn fast_write_spec(
    policy: WritePolicy,
    codec: crate::tensor_store::FastCodec,
    level: i32,
) -> TensorWriteSpec {
    TensorWriteSpec {
        plan: TensorPlanOptions {
            write_policy: policy,
            fast_codec: codec,
            fast_zstd_level: level,
            ..TensorPlanOptions::default()
        },
        ..TensorWriteSpec::default()
    }
}
