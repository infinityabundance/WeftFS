//! Lineage-aware rebasing of hot tensor objects.
//!
//! The planner may store a fine-tune checkpoint as a weight delta: physically
//! small, but every read has to decode its base too. When an object is read
//! often enough that `reads * estimated_decode_ns` exceeds a threshold, the
//! stored representation is the wrong trade. Rebase publishes a **new version**
//! with an independent representation (raw, zstd, or mode-9 `FLOAT_TENSOR`) and
//! records provenance (`operation: "rebase"`, input = the old content root).
//!
//! Invariants:
//!
//! * Existing versions and every snapshot are untouched. Rebase is a normal
//!   new version whose causal parent is the previous version.
//! * Logical bytes and identity are unchanged: the content root, tensor dtype,
//!   shape, layout, and `LogicalTensorRoot` are the same before and after.
//! * Idempotent. The rebase consumes the object's read counter; a second run
//!   sees zero reads and writes nothing.
//! * The decision is explicit and inspectable: `--dry-run` prices every
//!   candidate and reports the reason, without writing or counting reads.

use std::collections::BTreeMap;

use weftcodec::wcb::Value;

use crate::cost_model::{self, CostClass, CostModelKind};
use crate::error::{Error, Result};
use crate::format::RepresentationKind;
use crate::identity::Hash32;
use crate::provenance::{
    Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal, ProvenanceRecord,
};
use crate::recovery;
use crate::state::ObjectKind;
use crate::store::Store;
use crate::tensor_store::{TensorPlanOptions, TensorStrategy};

/// Environment tool name recorded in rebase provenance.
pub const REBASE_TOOL: &str = "weftfs";

/// Rebase request.
#[derive(Clone, Debug)]
pub struct RebaseOptions {
    /// Rebase when `reads * decode_cost_ns` strictly exceeds this value.
    pub threshold_ns: u64,
    /// Price candidates with this byte/ns exchange rate (0 = bytes only).
    pub lambda_ns: u64,
    /// Decode cost table.
    pub cost_model: CostModelKind,
    /// Force a standalone strategy (`raw`, `zstd`, `float-tensor`, ...).
    pub strategy_hint: Option<TensorStrategy>,
    /// Bound on the number of objects examined.
    pub max_objects: usize,
    /// Do not write; only produce the plan.
    pub dry_run: bool,
}

impl Default for RebaseOptions {
    fn default() -> Self {
        RebaseOptions {
            threshold_ns: 1_000_000_000,
            lambda_ns: 0,
            cost_model: CostModelKind::default(),
            strategy_hint: None,
            max_objects: 100_000,
            dry_run: true,
        }
    }
}

/// Why one object was (or was not) selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseCandidate {
    pub object: Hash32,
    pub path: String,
    pub reads: u64,
    pub from_strategy: String,
    pub from_cost_class: String,
    pub logical_bytes: u64,
    pub estimated_decode_ns: u64,
    pub read_cost_ns: u64,
    pub to_strategy: TensorStrategy,
    pub new_physical_bytes: u64,
    pub new_estimated_decode_ns: u64,
    /// Full candidate table, so a dry run shows why the choice won.
    pub candidates: Vec<(String, Option<u64>, Option<u64>)>,
}

/// One examined object that was not selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseSkip {
    pub object: Hash32,
    pub path: String,
    pub reason: String,
}

/// The complete rebase decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebasePlan {
    pub root: Hash32,
    pub threshold_ns: u64,
    pub cost_model: CostModelKind,
    pub scanned: usize,
    pub truncated: bool,
    pub candidates: Vec<RebaseCandidate>,
    pub skipped: Vec<RebaseSkip>,
}

/// One applied rebase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseOutcome {
    pub object: Hash32,
    pub path: String,
    pub old_version: Hash32,
    pub new_version: Hash32,
    pub content: Hash32,
    pub from_strategy: String,
    pub to_strategy: TensorStrategy,
    pub reads_consumed: u64,
    pub provenance_id: Hash32,
}

/// A plan plus the outcome of applying it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseReport {
    pub plan: RebasePlan,
    pub applied: Vec<RebaseOutcome>,
}

impl Store {
    /// Enumerate live objects under `root` (inclusive) and price a rebase.
    ///
    /// A candidate must satisfy all three conditions:
    /// 1. it has been read at least once (`reads > 0`);
    /// 2. `reads * estimated_decode_ns(current representation)` exceeds the
    ///    threshold;
    /// 3. a standalone representation decodes strictly faster, and is not
    ///    larger than the logical bytes.
    pub fn plan_rebase(&self, root: Hash32, options: &RebaseOptions) -> Result<RebasePlan> {
        if options.threshold_ns == 0 {
            return Err(Error::invalid_argument(
                "rebase threshold must be greater than zero",
            ));
        }
        let mut objects = self.collect_subtree(root, options.max_objects)?;
        let truncated = objects.len() >= options.max_objects;
        objects.sort_by_key(|(object, _)| *object);
        let mut plan = RebasePlan {
            root,
            threshold_ns: options.threshold_ns,
            cost_model: options.cost_model,
            scanned: 0,
            truncated,
            candidates: Vec::new(),
            skipped: Vec::new(),
        };
        let plan_options = TensorPlanOptions {
            lambda_ns: options.lambda_ns,
            cost_model: options.cost_model,
            ..TensorPlanOptions::default()
        };
        for (object, path) in objects {
            plan.scanned += 1;
            let record = match self.current_state().objects.get(&object) {
                Some(record) => record.clone(),
                None => continue,
            };
            if record.kind != ObjectKind::File {
                continue;
            }
            let reads = self.access_reads(object);
            if reads == 0 {
                continue;
            }
            let meta = match self.tensor_metadata(object) {
                Ok(meta) => meta,
                Err(err) if err.class() == crate::error::Class::NotFound => continue,
                Err(err) => {
                    plan.skipped.push(RebaseSkip {
                        object,
                        path,
                        reason: format!("tensor metadata unreadable: {err}"),
                    });
                    continue;
                }
            };
            let manifest =
                self.with_backend(|backend| recovery::read_manifest_via(backend, record.content))?;
            let from_strategy = manifest
                .representation
                .as_ref()
                .map(|rep| rep.strategy.clone())
                .unwrap_or_else(|| "raw".to_owned());
            if let Some(rep) = &manifest.representation {
                if rep.kind != RepresentationKind::Tensor
                    && from_strategy != "raw"
                    && from_strategy != "zstd"
                {
                    plan.skipped.push(RebaseSkip {
                        object,
                        path,
                        reason: format!(
                            "representation `{from_strategy}` is not a tensor representation"
                        ),
                    });
                    continue;
                }
            }
            let class = CostClass::parse(&from_strategy).unwrap_or(CostClass::Raw);
            let decode_cost_ns = match self.current_decode_cost(&record, options.cost_model) {
                Ok(cost) => cost,
                Err(err) => {
                    plan.skipped.push(RebaseSkip {
                        object,
                        path,
                        reason: format!("decode cost could not be priced: {err}"),
                    });
                    continue;
                }
            };
            let read_cost_ns = reads.saturating_mul(decode_cost_ns);
            if read_cost_ns <= options.threshold_ns {
                continue;
            }
            let independent =
                match self.plan_tensor_independent(object, &plan_options, options.strategy_hint) {
                    Ok(plan) => plan,
                    Err(err) => {
                        plan.skipped.push(RebaseSkip {
                            object,
                            path,
                            reason: format!(
                                "independent representation could not be priced: {err}"
                            ),
                        });
                        continue;
                    }
                };
            if independent.estimated_decode_ns >= decode_cost_ns {
                plan.skipped.push(RebaseSkip {
                    object,
                    path,
                    reason: format!(
                        "no standalone representation decodes faster than `{from_strategy}` \
                         ({} vs {} ns)",
                        independent.estimated_decode_ns, decode_cost_ns
                    ),
                });
                continue;
            }
            if independent.physical_bytes > record.size {
                plan.skipped.push(RebaseSkip {
                    object,
                    path,
                    reason: format!(
                        "standalone representation is larger than the logical bytes \
                         ({} > {})",
                        independent.physical_bytes, record.size
                    ),
                });
                continue;
            }
            let _ = meta;
            plan.candidates.push(RebaseCandidate {
                object,
                path,
                reads,
                from_strategy,
                from_cost_class: class.as_str().to_owned(),
                logical_bytes: record.size,
                estimated_decode_ns: decode_cost_ns,
                read_cost_ns,
                to_strategy: independent.chosen,
                new_physical_bytes: independent.physical_bytes,
                new_estimated_decode_ns: independent.estimated_decode_ns,
                candidates: independent
                    .candidates
                    .iter()
                    .map(|item| {
                        (
                            item.strategy.as_str().to_owned(),
                            item.physical_bytes,
                            item.estimated_decode_ns,
                        )
                    })
                    .collect(),
            });
        }
        Ok(plan)
    }

    /// Cost one object's *current* representation, including the frames it
    /// depends on. Delta rows are incremental, so a chain's cost is the sum of
    /// each frame's own decode cost.
    fn current_decode_cost(
        &self,
        record: &crate::state::ObjectRecord,
        model: CostModelKind,
    ) -> Result<u64> {
        let mut seen = std::collections::BTreeSet::new();
        self.decode_cost_of_content(record.content, record.size, model, 0, &mut seen)
    }

    fn decode_cost_of_content(
        &self,
        content: Hash32,
        size: u64,
        model: CostModelKind,
        depth: usize,
        seen: &mut std::collections::BTreeSet<Hash32>,
    ) -> Result<u64> {
        if depth > crate::representation::MAX_CODEC_DEPENDENCY_DEPTH {
            return Err(Error::corrupt(format!(
                "rebase decode-cost depth exceeds {}",
                crate::representation::MAX_CODEC_DEPENDENCY_DEPTH
            )));
        }
        if !seen.insert(content) {
            return Err(Error::corrupt("rebase decode-cost cycle"));
        }
        let manifest =
            self.with_backend(|backend| recovery::read_manifest_via(backend, content))?;
        let strategy = manifest
            .representation
            .as_ref()
            .map(|rep| rep.strategy.as_str())
            .unwrap_or("raw");
        let class = CostClass::parse(strategy).unwrap_or(CostClass::Raw);
        let mut total = cost_model::estimate_ns(class, size, model);
        if let Some(rep) = &manifest.representation {
            for dependency in &rep.dependencies {
                let dependency_manifest =
                    self.with_backend(|backend| recovery::read_manifest_via(backend, *dependency))?;
                total = total.saturating_add(self.decode_cost_of_content(
                    *dependency,
                    dependency_manifest.size,
                    model,
                    depth + 1,
                    seen,
                )?);
            }
        }
        seen.remove(&content);
        Ok(total)
    }

    /// Apply a planned rebase. A dry run returns the plan with no writes.
    pub fn rebase(&mut self, root: Hash32, options: &RebaseOptions) -> Result<RebaseReport> {
        let plan = self.plan_rebase(root, options)?;
        if options.dry_run {
            return Ok(RebaseReport {
                plan,
                applied: Vec::new(),
            });
        }
        self.require_writable()?;
        let plan_options = TensorPlanOptions {
            lambda_ns: options.lambda_ns,
            cost_model: options.cost_model,
            ..TensorPlanOptions::default()
        };
        let mut applied = Vec::with_capacity(plan.candidates.len());
        for candidate in &plan.candidates {
            let rebase =
                self.rebase_tensor_object(candidate.object, &plan_options, options.strategy_hint)?;
            let reads_consumed = self.consume_access_reads(candidate.object);
            let record = self.provenance_record(candidate, &rebase, options)?;
            let provenance_id = self.record_provenance(&[candidate.object], &record)?;
            applied.push(RebaseOutcome {
                object: candidate.object,
                path: candidate.path.clone(),
                old_version: rebase.old_version,
                new_version: rebase.new_version,
                content: rebase.new_content,
                from_strategy: rebase.old_strategy,
                to_strategy: rebase.plan.chosen,
                reads_consumed,
                provenance_id,
            });
        }
        // Consumed counters require a fresh snapshot; do it once, after every
        // object has been rewritten.
        self.flush_access_counts()?;
        Ok(RebaseReport { plan, applied })
    }

    fn provenance_record(
        &self,
        candidate: &RebaseCandidate,
        rebase: &crate::tensor_store::TensorRebase,
        options: &RebaseOptions,
    ) -> Result<ProvenanceRecord> {
        let mut params = BTreeMap::new();
        params.insert(
            "read_cost_ns".to_owned(),
            Value::U64(candidate.read_cost_ns),
        );
        params.insert("threshold_ns".to_owned(), Value::U64(options.threshold_ns));
        params.insert("reads".to_owned(), Value::U64(candidate.reads));
        params.insert(
            "from_strategy".to_owned(),
            Value::Str(candidate.from_strategy.clone()),
        );
        params.insert(
            "to_strategy".to_owned(),
            Value::Str(rebase.plan.chosen.as_str().to_owned()),
        );
        params.insert(
            "estimated_decode_ns".to_owned(),
            Value::U64(candidate.estimated_decode_ns),
        );
        params.insert(
            "old_version".to_owned(),
            Value::Bytes(rebase.old_version.as_bytes().to_vec()),
        );
        params.insert(
            "new_version".to_owned(),
            Value::Bytes(rebase.new_version.as_bytes().to_vec()),
        );
        params.insert(
            "cost_model".to_owned(),
            Value::Str(options.cost_model.as_str().to_owned()),
        );
        let inputs = vec![ProvenanceInput::new(
            "source",
            rebase.old_content,
            Some(candidate.object),
        )];
        let environment = ProvenanceEnvironment {
            tool: REBASE_TOOL.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            env_hash: None,
            fields: BTreeMap::new(),
        };
        let principal =
            ProvenancePrincipal::new(self.options().owner_uid, self.options().owner_gid);
        ProvenanceRecord::new(
            "rebase",
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
    fn collect_subtree(&self, root: Hash32, limit: usize) -> Result<Vec<(Hash32, String)>> {
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
