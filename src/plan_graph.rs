//! Write-time dependency-graph planning for a checkpoint trajectory.
//!
//! A trajectory is an ordered sequence of versions (`v0, v1, ...`). Each version
//! may be stored independently or as a delta against an **earlier** version.
//! The parent choice trades stored bytes against restore time:
//!
//! * a delta is usually smaller than an independent representation, but
//! * a cold restore of version `i` has to decode every frame on its chain, so
//!   version `i`'s restore cost is `own(i) + restore(parent(i))`.
//!
//! This module is a **pure optimizer**: it never touches a store. The store
//! layer ([`crate::tensor_store::Store`]) builds a [`VersionGraph`] of measured
//! edge costs and calls one of the solvers here. The objective is
//!
//! ```text
//! F(plan) = sum_i edge_bytes(i, parent(i))
//!         + lambda_read_ns * reads_per_version * sum_i chain_decode_ns(i)
//! ```
//!
//! where `chain_decode_ns(i)` is the sum of the measured incremental decode
//! costs of every frame on `i`'s chain (the same recursion
//! [`crate::rebase`] uses for the current representation). Both terms are
//! integer-only, so a plan is reproducible bit-for-bit.
//!
//! # Solvers
//!
//! * [`Solver::Dp`] is a dynamic program over the **chain-or-restart** state
//!   space: `parent(i) = none` or `parent(i) = i - 1`. Its state is
//!   `(version, depth)`; it is exact over that space and runs in
//!   `O(versions * max_depth)`. It is the practical planner for long
//!   trajectories (a restart is the only decision that matters once the graph
//!   is a chain; non-adjacent deltas are strictly dominated by the adjacent
//!   delta in every measured trajectory).
//! * [`Solver::BruteForce`] enumerates **every** parent assignment
//!   (`none` or any earlier compatible version) and is the exact oracle. It is
//!   limited to small graphs (the search is `O(versions!)`); the limit is a
//!   typed [`crate::Class::LimitExceeded`], never a silent fallback.
//! * [`Solver::Greedy`] walks the sequence once and, for each version, picks the
//!   parent among all earlier versions that minimises the objective of *that
//!   version* under the plan chosen so far. It is deterministic and bounded,
//!   and it is the only solver that considers arbitrary earlier parents at
//!   scale; it is not claimed to be optimal.
//!
//! The brute-force oracle and the chain/restart DP are compared in
//! `tests/parent_graph.rs` and in the S13 court. When the measured graph
//! prefers a non-adjacent parent the court reports the DP's loss; it is not
//! hidden.
//!
//! # Constraints
//!
//! * `max_depth` (default 8, the frozen reader bound) bounds every chain.
//! * `sla_ns` bounds the worst-case cold restore of any version.
//! * `storage_budget` bounds the sum of edge bytes.
//!
//! If no plan satisfies the SLA (or the storage budget) the solver fails with a
//! typed error naming the best achievable value. It never degrades silently.
//!
//! # Determinism and tie-breaks
//!
//! Every solver is deterministic: candidate order is the version index, there
//! is no randomness, and a complete plan is compared by
//! `(objective, fewer dependencies, lower parent index)` where the parent index
//! comparison is lexicographic in version order and "no parent" sorts before
//! every parent index.

use crate::cost_model::{RestoreOverhead, RESTORE_OVERHEAD_CALIBRATION, SLA_MARGIN_CALIBRATION};
use crate::error::{Class, Error, Result};

/// Schema string for a serialized version graph.
pub const VERSION_GRAPH_SCHEMA: &str = "weft.s13.version-graph.v1";
/// Schema string for a serialized plan.
pub const PARENT_GRAPH_PLAN_SCHEMA: &str = "weft.s13.parent-graph.plan.v1";
/// The frozen reader bound (`src/representation.rs`).
pub const MAX_SUPPORTED_DEPTH: u32 = 8;
/// Default exhaustive-oracle limit. The search is `O(n!)`; above this the
/// caller must ask for `--solver dp` or `--solver greedy` explicitly.
pub const DEFAULT_BRUTEFORCE_LIMIT: usize = 12;

/// Measured cost of one representation edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdgeCost {
    /// Representation bytes charged to this version (objects and manifests).
    pub bytes: u64,
    /// Incremental decode nanoseconds for this version's own frames.
    pub decode_ns: u64,
    /// Decode nanoseconds attributable to tensors that actually take the delta
    /// edge. Recorded for diagnostics; the objective uses `decode_ns`.
    pub coupled_ns: u64,
    /// Tensors that take the delta edge.
    pub coupled_tensors: u32,
    /// Tensors in the version.
    pub total_tensors: u32,
}

/// One version in the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionCost {
    pub name: String,
    /// Logical bytes of the version (used only for reporting).
    pub logical_bytes: u64,
    /// Cost when the version is written independently (no causal parent).
    pub independent: EdgeCost,
    /// `edges[j]` is the cost when version `i` names version `j` as its causal
    /// parent. `None` means the two versions are not compatible (different
    /// tensor sets, dtypes, or shapes).
    pub edges: Vec<Option<EdgeCost>>,
}

/// A sequence of measured version costs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionGraph {
    pub versions: Vec<VersionCost>,
}

impl VersionGraph {
    /// Validate the graph's shape. A graph of `n` versions has `n` entries and
    /// `edges[j]` for every `j < i`; any other shape is malformed.
    pub fn validate(&self) -> Result<()> {
        for (i, version) in self.versions.iter().enumerate() {
            if version.edges.len() != i {
                return Err(Error::malformed(format!(
                    "version {i} (`{}`) has {} parent candidates, expected {i}",
                    version.name,
                    version.edges.len()
                )));
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.versions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// The edge cost for `parent(i) = parent`, or the independent cost for
    /// `None`. A missing candidate is a typed `InvalidArgument`.
    pub fn edge(&self, i: usize, parent: Option<usize>) -> Result<EdgeCost> {
        let version = self
            .versions
            .get(i)
            .ok_or_else(|| Error::invalid_argument(format!("version {i} is outside the graph")))?;
        match parent {
            None => Ok(version.independent),
            Some(j) => {
                if j >= i {
                    return Err(Error::invalid_argument(format!(
                        "version {i} cannot name `{}` as a parent",
                        self.versions
                            .get(j)
                            .map(|v| v.name.as_str())
                            .unwrap_or("<out of range>")
                    )));
                }
                version.edges[j].ok_or_else(|| {
                    Error::invalid_argument(format!(
                        "version {i} (`{}`) is not compatible with parent {j} (`{}`)",
                        version.name, self.versions[j].name
                    ))
                })
            }
        }
    }
}

/// Which optimizer to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Solver {
    /// Chain-or-restart dynamic program (`parent = none | previous`).
    Dp,
    /// Exhaustive oracle over every earlier parent.
    BruteForce,
    /// One-pass greedy over every earlier parent.
    Greedy,
}

impl Solver {
    pub fn as_str(self) -> &'static str {
        match self {
            Solver::Dp => "dp",
            Solver::BruteForce => "brute-force",
            Solver::Greedy => "greedy",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "dp" => Ok(Solver::Dp),
            "brute-force" | "bruteforce" | "exact" => Ok(Solver::BruteForce),
            "greedy" => Ok(Solver::Greedy),
            other => Err(Error::invalid_argument(format!(
                "--solver must be `dp`, `brute-force`, or `greedy` (got `{other}`)"
            ))),
        }
    }
}

/// Objective coefficients and hard constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanOptions {
    /// Bytes charged per nanosecond of decode time (`0` = bytes-only).
    pub lambda_read_ns: u64,
    /// Expected reads per version over its lifetime. Each read pays the whole
    /// chain (`chain_decode_ns`), so this multiplies the decode term.
    pub reads_per_version: u64,
    /// Maximum chain length (`0`..=`MAX_SUPPORTED_DEPTH`).
    pub max_depth: u32,
    /// Worst-case cold restore bound in nanoseconds for any version.
    pub sla_ns: Option<u64>,
    /// Maximum sum of edge bytes.
    pub storage_budget: Option<u64>,
    /// Solver to run.
    pub solver: Solver,
    /// Exhaustive-oracle limit (`Default` is [`DEFAULT_BRUTEFORCE_LIMIT`]).
    pub brute_force_limit: usize,
    /// Measured whole-checkpoint restore overhead added once per version.
    ///
    /// `Default` is `Some(RestoreOverhead::MEASURED)`. `None` means the caller
    /// has no measured term; a plan that then claims an SLA is refused with a
    /// typed `InvalidArgument` instead of treating the missing work as free.
    ///
    /// When an SLA is claimed, the plan's measured **SLA margin**
    /// (`RestoreOverhead::margin_ns_per_mib`) is added to the prediction before
    /// the comparison. The margin is not optional: a model whose margin is zero
    /// is a diagnostic, not an SLA model, and is refused.
    pub restore_overhead: Option<RestoreOverhead>,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions {
            lambda_read_ns: 0,
            reads_per_version: 1,
            max_depth: MAX_SUPPORTED_DEPTH,
            sla_ns: None,
            storage_budget: None,
            solver: Solver::Dp,
            brute_force_limit: DEFAULT_BRUTEFORCE_LIMIT,
            restore_overhead: Some(RestoreOverhead::MEASURED),
        }
    }
}

impl PlanOptions {
    pub fn validate(&self) -> Result<()> {
        if self.max_depth > MAX_SUPPORTED_DEPTH {
            return Err(Error::invalid_argument(format!(
                "max_depth {} exceeds the frozen reader bound {MAX_SUPPORTED_DEPTH}",
                self.max_depth
            )));
        }
        if self.reads_per_version == 0 {
            return Err(Error::invalid_argument(
                "reads_per_version must be at least 1",
            ));
        }
        if self.sla_ns.is_some() {
            if let Some(term) = self.restore_overhead {
                if term.margin_ns_per_mib == 0 {
                    return Err(Error::invalid_argument(
                        "an SLA claim requires a measured SLA margin; this restore-overhead \
                         model has margin_ns_per_mib = 0 (see SLA_MARGIN_CALIBRATION)",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A complete plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParentGraphPlan {
    pub schema: &'static str,
    pub solver: Solver,
    /// `parents[i]` is the chosen parent index or `None` (independent).
    pub parents: Vec<Option<usize>>,
    /// Objective `F`, including the decode term.
    pub objective: u128,
    /// Sum of edge bytes.
    pub predicted_bytes: u64,
    /// Chain decode nanoseconds per version (`index i` = decode cost of `i`
    /// including every ancestor frame). This is the pure decode sum; the
    /// restore prediction below adds the measured restore overhead.
    pub predicted_decode_chain_ns: Vec<u64>,
    /// Whole-restore nanoseconds per version: `predicted_decode_chain_ns[i]`
    /// plus the version's measured restore overhead, added **once** (never per
    /// dependency hop).
    pub predicted_chain_ns: Vec<u64>,
    /// The measured restore overhead charged to each version.
    pub predicted_overhead_ns: Vec<u64>,
    /// The measured SLA margin charged to each version (see
    /// [`SLA_MARGIN_CALIBRATION`]). Charged only to the SLA bound below, never
    /// to the objective or the point prediction.
    pub predicted_margin_ns: Vec<u64>,
    /// `predicted_chain_ns[i] + predicted_margin_ns[i]`: the bound the SLA is
    /// compared against.
    pub predicted_sla_bound_ns: Vec<u64>,
    /// The overhead model the prediction used, or `None` when the caller
    /// supplied no model (then no SLA may be claimed).
    pub restore_overhead: Option<RestoreOverhead>,
    /// The measured-overhead calibration the baked term came from.
    pub restore_overhead_calibration: &'static str,
    /// The measured SLA-margin calibration the baked term came from.
    pub sla_margin_calibration: &'static str,
    /// `reads_per_version * sum(chain_ns)`.
    pub predicted_total_decode_ns: u128,
    /// Chain depth per version (`0` = independent).
    pub depths: Vec<u32>,
    /// Number of delta edges (versions with a parent).
    pub dependencies: u32,
    /// Deepest chain in the plan.
    pub max_depth: u32,
    /// Worst-case cold restore (`max(predicted_chain_ns)`). This is the point
    /// prediction; the SLA is compared against [`Self::worst_sla_bound_ns`].
    pub worst_restore_ns: u64,
    /// Worst-case restore **bound** (`max(predicted_sla_bound_ns)`), i.e. the
    /// point prediction plus the measured margin.
    pub worst_sla_bound_ns: u64,
    /// True when the plan's worst restore bound satisfies the SLA (also true
    /// when none was given).
    pub sla_satisfied: bool,
    /// True when the plan satisfies the storage budget.
    pub budget_satisfied: bool,
}

impl ParentGraphPlan {
    /// Number of versions.
    pub fn len(&self) -> usize {
        self.parents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }

    /// The chosen parent index as the planner reports it (`-1` for none).
    pub fn parent_indices(&self) -> Vec<i64> {
        self.parents
            .iter()
            .map(|parent| parent.map(|p| p as i64).unwrap_or(-1))
            .collect()
    }
}

/// Evaluate a complete parent assignment. Used by every solver and by the
/// store layer to price an externally supplied plan. Fails closed on an
/// incompatible edge, a depth violation, or a number that overflows `u128`.
pub fn evaluate(
    graph: &VersionGraph,
    parents: &[Option<usize>],
    options: &PlanOptions,
) -> Result<ParentGraphPlan> {
    options.validate()?;
    graph.validate()?;
    if parents.len() != graph.len() {
        return Err(Error::invalid_argument(format!(
            "plan has {} parents but the graph has {} versions",
            parents.len(),
            graph.len()
        )));
    }
    // An SLA is a claim about wall time. Without a measured restore-overhead
    // model the prediction is known to omit the container/manifest/hash work,
    // so the claim would be optimistic. Refuse instead of assuming it is zero.
    if options.sla_ns.is_some() && options.restore_overhead.is_none() {
        return Err(Error::invalid_argument(
            "an SLA claim requires a measured restore-overhead model; set \
             PlanOptions::restore_overhead (or drop the SLA). Treating the \
             whole-checkpoint restore work as zero is the S13 defect",
        ));
    }
    let mut decode_chain_ns = vec![0u64; parents.len()];
    let mut chain_ns = vec![0u64; parents.len()];
    let mut overhead_ns = vec![0u64; parents.len()];
    let mut margin_ns = vec![0u64; parents.len()];
    let mut sla_bound_ns = vec![0u64; parents.len()];
    let mut depths = vec![0u32; parents.len()];
    let mut bytes: u128 = 0;
    let mut decode_weighted: u128 = 0;
    let mut dependencies = 0u32;
    for (i, parent) in parents.iter().copied().enumerate() {
        let edge = graph.edge(i, parent)?;
        let (decode_chain, depth) = match parent {
            None => (edge.decode_ns, 0u32),
            Some(j) => {
                if depths[j] >= options.max_depth {
                    return Err(Error::invalid_argument(format!(
                        "version {i} naming parent {j} would exceed max_depth {}",
                        options.max_depth
                    )));
                }
                let chain = decode_chain_ns[j]
                    .checked_add(edge.decode_ns)
                    .ok_or_else(|| {
                        Error::limit_exceeded("chain decode nanoseconds overflow u64")
                    })?;
                (chain, depths[j] + 1)
            }
        };
        decode_chain_ns[i] = decode_chain;
        // The restore overhead is charged once per version restore, never once
        // per dependency hop: a deeper chain decodes one more frame, which the
        // decode rows already price.
        let overhead = options
            .restore_overhead
            .map(|term| {
                term.estimate_ns(
                    u64::from(graph.versions[i].independent.total_tensors),
                    graph.versions[i].logical_bytes,
                )
            })
            .unwrap_or(0);
        overhead_ns[i] = overhead;
        chain_ns[i] = decode_chain
            .checked_add(overhead)
            .ok_or_else(|| Error::limit_exceeded("chain restore nanoseconds overflow u64"))?;
        // The SLA margin is charged once per version restore, alongside the
        // overhead and never to a hop. It bounds the measured under-prediction
        // of the *whole* restore in the environment the plan is made for, so it
        // is applied only to the SLA comparison below.
        let margin = options
            .restore_overhead
            .map(|term| term.estimate_margin_ns(graph.versions[i].logical_bytes))
            .unwrap_or(0);
        margin_ns[i] = margin;
        sla_bound_ns[i] = chain_ns[i]
            .checked_add(margin)
            .ok_or_else(|| Error::limit_exceeded("SLA restore bound overflow u64"))?;
        depths[i] = depth;
        dependencies += u32::from(parent.is_some());
        bytes = bytes
            .checked_add(u128::from(edge.bytes))
            .ok_or_else(|| Error::limit_exceeded("edge byte total overflow u128"))?;
        let weighted = u128::from(options.lambda_read_ns)
            .checked_mul(u128::from(options.reads_per_version))
            .and_then(|factor| factor.checked_mul(u128::from(chain_ns[i])))
            .ok_or_else(|| Error::limit_exceeded("objective overflow u128"))?;
        decode_weighted = decode_weighted
            .checked_add(weighted)
            .ok_or_else(|| Error::limit_exceeded("objective overflow u128"))?;
    }
    let predicted_bytes =
        u64::try_from(bytes).map_err(|_| Error::limit_exceeded("edge byte total exceeds u64"))?;
    let objective = bytes
        .checked_add(decode_weighted)
        .ok_or_else(|| Error::limit_exceeded("objective overflow u128"))?;
    let worst_restore_ns = chain_ns.iter().copied().max().unwrap_or(0);
    let worst_sla_bound_ns = sla_bound_ns.iter().copied().max().unwrap_or(0);
    let max_depth = depths.iter().copied().max().unwrap_or(0);
    let sla_satisfied = options
        .sla_ns
        .map(|sla| worst_sla_bound_ns <= sla)
        .unwrap_or(true);
    let budget_satisfied = options
        .storage_budget
        .map(|budget| predicted_bytes <= budget)
        .unwrap_or(true);
    let predicted_total_decode_ns = u128::from(options.reads_per_version)
        * chain_ns.iter().map(|n| u128::from(*n)).sum::<u128>();
    Ok(ParentGraphPlan {
        schema: PARENT_GRAPH_PLAN_SCHEMA,
        solver: options.solver,
        parents: parents.to_vec(),
        objective,
        predicted_bytes,
        predicted_decode_chain_ns: decode_chain_ns,
        predicted_chain_ns: chain_ns,
        predicted_overhead_ns: overhead_ns,
        predicted_margin_ns: margin_ns,
        predicted_sla_bound_ns: sla_bound_ns,
        restore_overhead: options.restore_overhead,
        restore_overhead_calibration: RESTORE_OVERHEAD_CALIBRATION,
        sla_margin_calibration: SLA_MARGIN_CALIBRATION,
        predicted_total_decode_ns,
        depths,
        dependencies,
        max_depth,
        worst_restore_ns,
        worst_sla_bound_ns,
        sla_satisfied,
        budget_satisfied,
    })
}

/// The measured restore overhead charged to version `i`.
///
/// It depends only on the version (tensor count and logical bytes), never on
/// the chosen parent, so every planner state for version `i` pays it once.
fn version_overhead_ns(graph: &VersionGraph, options: &PlanOptions, i: usize) -> u64 {
    let version = &graph.versions[i];
    options
        .restore_overhead
        .map(|term| {
            term.estimate_ns(
                u64::from(version.independent.total_tensors),
                version.logical_bytes,
            )
        })
        .unwrap_or(0)
}

/// Decode chain plus the version's own restore overhead: the point prediction
/// an SLA is stated against. The solvers must prune on [`sla_bound_ns`], not on
/// this, so a plan that only fits without the measured margin is never chosen.
fn restore_chain_ns(
    graph: &VersionGraph,
    options: &PlanOptions,
    i: usize,
    decode_chain_ns: u64,
) -> u64 {
    decode_chain_ns.saturating_add(version_overhead_ns(graph, options, i))
}

/// The measured SLA margin charged to version `i` (see
/// [`SLA_MARGIN_CALIBRATION`]).
fn version_margin_ns(graph: &VersionGraph, options: &PlanOptions, i: usize) -> u64 {
    options
        .restore_overhead
        .map(|term| term.estimate_margin_ns(graph.versions[i].logical_bytes))
        .unwrap_or(0)
}

/// The quantity an SLA actually bounds: the point prediction plus the measured
/// margin. Every solver prunes and `check_constraints` compares on this value.
fn sla_bound_ns(
    graph: &VersionGraph,
    options: &PlanOptions,
    i: usize,
    decode_chain_ns: u64,
) -> u64 {
    restore_chain_ns(graph, options, i, decode_chain_ns)
        .saturating_add(version_margin_ns(graph, options, i))
}

/// Result of a solver before constraint finalisation.
#[derive(Clone)]
struct Candidate {
    parents: Vec<Option<usize>>,
    objective: u128,
    dependencies: u32,
    /// Chain decode nanoseconds of the last version.
    chain_ns: u64,
    /// Chain depth of the last version.
    depth: u32,
}

/// The deterministic total order: `(objective, fewer dependencies, lower parent
/// index)`. Returns true when `a` is strictly better than `b`.
fn better(a: &Candidate, b: &Candidate) -> bool {
    if a.objective != b.objective {
        return a.objective < b.objective;
    }
    if a.dependencies != b.dependencies {
        return a.dependencies < b.dependencies;
    }
    // Lexicographic in version order; `None` (-1) sorts before any index.
    a.parents.cmp(&b.parents).is_lt()
}

fn check_constraints(plan: &ParentGraphPlan, options: &PlanOptions) -> Result<()> {
    if let Some(sla) = options.sla_ns {
        if plan.worst_sla_bound_ns > sla {
            return Err(Error::new(
                Class::LimitExceeded,
                format!(
                    "no plan satisfies the {} ns restore SLA; the best predicted worst-case restore is {} ns plus a {} ns measured SLA margin (point prediction {} ns; calibration {}), bound {} ns",
                    sla,
                    plan.worst_restore_ns,
                    plan.worst_sla_bound_ns - plan.worst_restore_ns,
                    plan.worst_restore_ns,
                    SLA_MARGIN_CALIBRATION,
                    plan.worst_sla_bound_ns
                ),
            ));
        }
    }
    if let Some(budget) = options.storage_budget {
        if plan.predicted_bytes > budget {
            return Err(Error::new(
                Class::LimitExceeded,
                format!(
                    "no plan satisfies the {budget}-byte storage budget; the best predicted total is {} bytes",
                    plan.predicted_bytes
                ),
            ));
        }
    }
    Ok(())
}

/// Run the requested solver.
pub fn optimize(graph: &VersionGraph, options: &PlanOptions) -> Result<ParentGraphPlan> {
    options.validate()?;
    graph.validate()?;
    if graph.is_empty() {
        return evaluate(graph, &[], options);
    }
    let parents = match options.solver {
        Solver::Dp => solve_dp(graph, options)?,
        Solver::Greedy => solve_greedy(graph, options)?,
        Solver::BruteForce => {
            if graph.len() > options.brute_force_limit {
                return Err(Error::new(
                    Class::LimitExceeded,
                    format!(
                        "brute-force solver is limited to {} versions; the graph has {} (use --solver dp or greedy)",
                        options.brute_force_limit,
                        graph.len()
                    ),
                ));
            }
            solve_brute_force(graph, options)?
        }
    };
    let mut plan = evaluate(graph, &parents, options)?;
    // The chain/restart DP optimises the objective, not the optional storage
    // budget. When the unconstrained optimum exceeds the budget, fall back to
    // the exhaustive oracle, which prunes on partial bytes. Never silently
    // return an over-budget plan.
    if options.solver == Solver::Dp {
        if let Some(budget) = options.storage_budget {
            if plan.predicted_bytes > budget {
                if graph.len() > options.brute_force_limit {
                    return Err(Error::new(
                        Class::LimitExceeded,
                        format!(
                            "the dp plan is {} bytes, over the {budget}-byte budget, and an exact budget-aware search for {} versions is above the {} oracle limit; use --solver brute-force with fewer versions",
                            plan.predicted_bytes,
                            graph.len(),
                            options.brute_force_limit
                        ),
                    ));
                }
                let parents = solve_brute_force(graph, options)?;
                plan = evaluate(graph, &parents, options)?;
            }
        }
    }
    check_constraints(&plan, options)?;
    Ok(plan)
}

/// Chain-or-restart dynamic program.
///
/// State space: `(version index, chain depth)`. For each state we keep the
/// cycle-free best plan prefix. The two transitions are
///
/// * restart: `parent(i) = none`, depth `0`; and
/// * continue: `parent(i) = i - 1`, depth `depth(i-1) + 1`.
///
/// A non-adjacent parent can only change the *byte* term (every measured
/// trajectory's furthest delta is larger than the adjacent one), so this is
/// the exact optimum of the family the store can realise with a single
/// `causal_checkpoint` per version.
fn solve_dp(graph: &VersionGraph, options: &PlanOptions) -> Result<Vec<Option<usize>>> {
    let n = graph.len();
    // `states[i][d]` = the best prefix ending at version `i` with chain depth
    // `d`. In the chain-or-restart family the chain of `(i, d)` is the last
    // `d + 1` versions, so its decode cost is fixed by `(i, d)`; the state does
    // not need a Pareto frontier.
    let mut states: Vec<Vec<Option<Candidate>>> = vec![Vec::new(); n];
    // Version 0 can only be independent.
    let v0 = &graph.versions[0];
    if let Some(sla) = options.sla_ns {
        if sla_bound_ns(graph, options, 0, v0.independent.decode_ns) > sla {
            return Err(Error::new(
                Class::LimitExceeded,
                format!(
                    "no plan satisfies the {} ns restore SLA; version 0 alone needs {} ns plus a {} ns measured SLA margin",
                    sla,
                    restore_chain_ns(graph, options, 0, v0.independent.decode_ns),
                    version_margin_ns(graph, options, 0)
                ),
            ));
        }
    }
    states[0] = vec![Some(Candidate {
        parents: vec![None],
        objective: u128::from(v0.independent.bytes)
            .checked_add(decode_term(options, v0.independent.decode_ns))
            .ok_or_else(|| Error::limit_exceeded("objective overflow u128"))?,
        dependencies: 0,
        chain_ns: v0.independent.decode_ns,
        depth: 0,
    })];
    for i in 1..n {
        let version = &graph.versions[i];
        let mut next: Vec<Option<Candidate>> = vec![None; options.max_depth as usize + 1];

        // Restart: the previous prefix may end at any depth; only the smallest
        // objective for the new depth-0 state matters.
        let mut best_restart: Option<Candidate> = None;
        let restart_term = u128::from(version.independent.bytes)
            .checked_add(decode_term(options, version.independent.decode_ns))
            .ok_or_else(|| Error::limit_exceeded("objective overflow u128"))?;
        // A restart of version `i` is itself a restore; if its own overhead
        // already breaks the SLA no prefix can use it.
        let restart_fits = options
            .sla_ns
            .map(|sla| sla_bound_ns(graph, options, i, version.independent.decode_ns) <= sla)
            .unwrap_or(true);
        for state in states[i - 1].iter().flatten() {
            if !restart_fits {
                break;
            }
            let mut parents = state.parents.clone();
            parents.push(None);
            let candidate = Candidate {
                parents,
                objective: state
                    .objective
                    .checked_add(restart_term)
                    .ok_or_else(|| Error::limit_exceeded("objective overflow u128"))?,
                dependencies: state.dependencies,
                chain_ns: version.independent.decode_ns,
                depth: 0,
            };
            if best_restart
                .as_ref()
                .map(|best| better(&candidate, best))
                .unwrap_or(true)
            {
                best_restart = Some(candidate);
            }
        }
        next[0] = best_restart;

        // Continue: parent is the previous version.
        if options.max_depth >= 1 {
            if let Some(edge) = version.edges[i - 1] {
                for d in 0..=(options.max_depth as usize - 1) {
                    let Some(state) = states[i - 1].get(d).and_then(|s| s.as_ref()) else {
                        continue;
                    };
                    let chain = state.chain_ns.checked_add(edge.decode_ns).ok_or_else(|| {
                        Error::limit_exceeded("chain decode nanoseconds overflow u64")
                    })?;
                    if options
                        .sla_ns
                        .map(|sla| sla_bound_ns(graph, options, i, chain) > sla)
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let mut parents = state.parents.clone();
                    parents.push(Some(i - 1));
                    let candidate = Candidate {
                        parents,
                        objective: state
                            .objective
                            .checked_add(
                                u128::from(edge.bytes)
                                    .checked_add(decode_term(options, chain))
                                    .ok_or_else(|| {
                                        Error::limit_exceeded("objective overflow u128")
                                    })?,
                            )
                            .ok_or_else(|| Error::limit_exceeded("objective overflow u128"))?,
                        dependencies: state.dependencies + 1,
                        chain_ns: chain,
                        depth: state.depth + 1,
                    };
                    let slot = next[d + 1].clone();
                    if slot
                        .as_ref()
                        .map(|best| better(&candidate, best))
                        .unwrap_or(true)
                    {
                        next[d + 1] = Some(candidate);
                    }
                }
            }
        }
        states[i] = next;
    }
    let best = states[n - 1]
        .iter()
        .flatten()
        .min_by(|a, b| {
            if better(a, b) {
                std::cmp::Ordering::Less
            } else if better(b, a) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .ok_or_else(|| {
            Error::new(
                Class::LimitExceeded,
                "no chain-or-restart plan satisfies the constraints",
            )
        })?;
    Ok(best.parents.clone())
}

fn decode_term(options: &PlanOptions, chain_ns: u64) -> u128 {
    u128::from(options.lambda_read_ns)
        .saturating_mul(u128::from(options.reads_per_version))
        .saturating_mul(u128::from(chain_ns))
}

/// Exhaustive oracle over every earlier compatible parent.
fn solve_brute_force(graph: &VersionGraph, options: &PlanOptions) -> Result<Vec<Option<usize>>> {
    let n = graph.len();
    let mut parents: Vec<Option<usize>> = vec![None; n];
    let mut chain_ns = vec![0u64; n];
    let mut best: Option<Candidate> = None;
    let mut bytes: u128 = 0;

    fn recurse(
        i: usize,
        graph: &VersionGraph,
        options: &PlanOptions,
        parents: &mut Vec<Option<usize>>,
        chain_ns: &mut Vec<u64>,
        depths: &mut Vec<u32>,
        bytes: &mut u128,
        best: &mut Option<Candidate>,
    ) -> Result<()> {
        if i == graph.len() {
            let plan = evaluate(graph, parents.as_slice(), options)?;
            let candidate = Candidate {
                parents: parents.clone(),
                objective: plan.objective,
                dependencies: plan.dependencies,
                chain_ns: *chain_ns.last().unwrap_or(&0),
                depth: *depths.last().unwrap_or(&0),
            };
            if best
                .as_ref()
                .map(|current| better(&candidate, current))
                .unwrap_or(true)
            {
                *best = Some(candidate);
            }
            return Ok(());
        }
        // Try `none` first (a deterministic order), then every earlier version.
        for choice in std::iter::once(None).chain((0..i).map(Some)) {
            let edge = match graph.edge(i, choice) {
                Ok(edge) => edge,
                Err(_) => continue,
            };
            let (chain, depth) = match choice {
                None => (edge.decode_ns, 0u32),
                Some(j) => (chain_ns[j].saturating_add(edge.decode_ns), depths[j] + 1),
            };
            if depth > options.max_depth {
                continue;
            }
            if let Some(sla) = options.sla_ns {
                if sla_bound_ns(graph, options, i, chain) > sla {
                    continue;
                }
            }
            let edge_bytes = u128::from(edge.bytes);
            *bytes = bytes
                .checked_add(edge_bytes)
                .ok_or_else(|| Error::limit_exceeded("objective overflow u128"))?;
            if let Some(budget) = options.storage_budget {
                if *bytes > u128::from(budget) {
                    *bytes -= edge_bytes;
                    continue;
                }
            }
            parents[i] = choice;
            chain_ns[i] = chain;
            depths[i] = depth;
            recurse(
                i + 1,
                graph,
                options,
                parents,
                chain_ns,
                depths,
                bytes,
                best,
            )?;
            *bytes -= edge_bytes;
        }
        parents[i] = None;
        depths[i] = 0;
        chain_ns[i] = 0;
        Ok(())
    }

    let mut depths = vec![0u32; n];
    recurse(
        0,
        graph,
        options,
        &mut parents,
        &mut chain_ns,
        &mut depths,
        &mut bytes,
        &mut best,
    )?;
    let best = best.ok_or_else(|| {
        Error::new(
            Class::LimitExceeded,
            "no parent assignment satisfies the constraints",
        )
    })?;
    Ok(best.parents)
}

/// Deterministic one-pass greedy over every earlier compatible parent.
fn solve_greedy(graph: &VersionGraph, options: &PlanOptions) -> Result<Vec<Option<usize>>> {
    let n = graph.len();
    let mut parents: Vec<Option<usize>> = vec![None; n];
    let mut chain_ns = vec![0u64; n];
    let mut depths = vec![0u32; n];
    let mut bytes_total: u128 = 0;
    for i in 0..n {
        let version = &graph.versions[i];
        let mut best: Option<(u128, u32, Option<usize>, u64, u64, u32)> = None;
        let mut consider = |parent: Option<usize>, edge: EdgeCost, chain: u64, depth: u32| {
            // The version's own contribution is its edge bytes plus the whole
            // chain decode cost priced at the read weight.
            let edge_cost = u128::from(edge.bytes)
                .checked_add(decode_term(options, chain))
                .unwrap_or(u128::MAX);
            let deps = u32::from(parent.is_some());
            let better_candidate = match &best {
                None => true,
                Some((cost, best_deps, best_parent, _, _, _)) => {
                    edge_cost < *cost
                        || (edge_cost == *cost
                            && (deps < *best_deps || (deps == *best_deps && parent < *best_parent)))
                }
            };
            if better_candidate {
                best = Some((edge_cost, deps, parent, chain, edge.bytes, depth));
            }
        };
        // Restart.
        if options
            .sla_ns
            .map(|sla| sla_bound_ns(graph, options, i, version.independent.decode_ns) <= sla)
            .unwrap_or(true)
        {
            consider(None, version.independent, version.independent.decode_ns, 0);
        }
        if options.max_depth >= 1 {
            for j in 0..i {
                let Some(edge) = version.edges[j] else {
                    continue;
                };
                if depths[j] + 1 > options.max_depth {
                    continue;
                }
                let chain = chain_ns[j].saturating_add(edge.decode_ns);
                if options
                    .sla_ns
                    .map(|sla| sla_bound_ns(graph, options, i, chain) > sla)
                    .unwrap_or(false)
                {
                    continue;
                }
                consider(Some(j), edge, chain, depths[j] + 1);
            }
        }
        drop(consider);
        let Some((_cost, _, parent, chain, edge_bytes, depth)) = best else {
            return Err(Error::new(
                Class::LimitExceeded,
                format!("no feasible parent for version {i}"),
            ));
        };
        let new_bytes = bytes_total + u128::from(edge_bytes);
        if let Some(budget) = options.storage_budget {
            if new_bytes > u128::from(budget) {
                return Err(Error::new(
                    Class::LimitExceeded,
                    format!("greedy plan exceeds the {budget}-byte storage budget at version {i}"),
                ));
            }
        }
        parents[i] = parent;
        chain_ns[i] = chain;
        depths[i] = depth;
        bytes_total = new_bytes;
    }
    Ok(parents)
}

/// Options for pricing a version graph from stored checkpoints.
#[derive(Clone, Debug)]
pub struct PriceOptions {
    /// Per-tensor planner options. The checkpoint writer's `Planned` strategy
    /// uses the same candidate set with no strategy hint.
    ///
    /// The planner options choose **which** representation each tensor uses; the
    /// per-hop decode price for that chosen representation comes from
    /// [`hop_decode_ns`], not from `TensorPlanOptions`'s decode table. A host
    /// calibration in `plan.calibration` therefore still prices write-time
    /// candidate choice, but does not re-price graph hops.
    pub plan: crate::tensor_store::TensorPlanOptions,
    /// Hard bound on the number of tensors per version (fail closed).
    pub max_tensors: usize,
    /// Shards per tensor. The parent-graph planner only supports `1`; a
    /// multi-shard checkpoint would need a per-shard parent graph and is
    /// refused rather than approximated.
    pub shards: u32,
}

impl Default for PriceOptions {
    fn default() -> Self {
        PriceOptions {
            plan: crate::tensor_store::TensorPlanOptions::default(),
            max_tensors: 200_000,
            shards: 1,
        }
    }
}

/// Decode price of one graph edge's chosen representation.
///
/// The store knows the chosen representation at plan time, so the graph prices
/// each edge with **that** representation's measured hop row
/// ([`crate::cost_model::hop_estimate_ns`]) instead of the write-time decode
/// row. The two tables are deliberately different: the write table drives
/// candidate selection and correcting it would flip stored representations,
/// while the hop table exists only to price chains. Mode 18
/// (`FLOAT_TILED_STREAMS`) keeps its own measured decode row because the
/// persisted calibration class set has no mode-18 class.
pub fn hop_decode_ns(strategy: crate::tensor_store::TensorStrategy, decoded_bytes: u64) -> u64 {
    if strategy == crate::tensor_store::TensorStrategy::FloatTiledStreams {
        return crate::cost_model::estimate_streams_ns(decoded_bytes);
    }
    crate::cost_model::hop_estimate_ns(strategy.cost_class(), decoded_bytes)
}

/// Append the store-facing planner. Kept in this module so the pure optimizer
/// and the measurement layer live together and cannot drift.
impl crate::store::Store {
    /// Price every edge of a trajectory from checkpoints already written
    /// **independently** in this store.
    ///
    /// Each checkpoint must be complete, use `--shard-strategy planned`, and
    /// have exactly `options.shards` shards (only `1` is supported). The
    /// function calls the shipped [`crate::store::Store::plan_tensor`] with and
    /// without a causal parent version, so the byte and decode numbers are the
    /// same ones the writer would use; nothing is published.
    pub fn price_parent_graph(
        &self,
        checkpoints: &[String],
        options: &PriceOptions,
    ) -> Result<VersionGraph> {
        if options.shards != 1 {
            return Err(Error::invalid_argument(
                "plan_parents supports only --shards 1; a multi-shard graph would need a per-shard parent decision",
            ));
        }
        if options.max_tensors == 0 {
            return Err(Error::invalid_argument(
                "plan_parents max_tensors must be at least 1",
            ));
        }
        // Load and validate every manifest once.
        let mut manifests = Vec::with_capacity(checkpoints.len());
        let mut manifest_bytes = Vec::with_capacity(checkpoints.len());
        for name in checkpoints {
            let manifest = self.load_checkpoint(name)?;
            if !manifest.complete {
                return Err(Error::invalid_argument(format!(
                    "checkpoint `{name}` is partial; the parent graph needs complete versions"
                )));
            }
            if manifest.tensors.len() > options.max_tensors {
                return Err(Error::new(
                    Class::LimitExceeded,
                    format!(
                        "checkpoint `{name}` has {} tensors, bound is {}",
                        manifest.tensors.len(),
                        options.max_tensors
                    ),
                ));
            }
            for tensor in &manifest.tensors {
                if tensor.shards.len() != 1 || tensor.shards[0].index != 0 {
                    return Err(Error::invalid_argument(format!(
                        "tensor `{}` in `{name}` is not single-shard; plan_parents supports only --shards 1",
                        tensor.name
                    )));
                }
            }
            let manifest_id = self.resolve_path(&format!(
                "{}/{}/manifest",
                crate::checkpoint::CHECKPOINT_ROOT,
                name
            ))?;
            let size = self.object_meta(manifest_id)?.size;
            manifests.push(manifest);
            manifest_bytes.push(size);
        }

        // Parent shard versions per tensor, so `plan_tensor` can find the
        // causal base through the store state.
        let mut versions_by_manifest: Vec<
            std::collections::BTreeMap<String, crate::identity::Hash32>,
        > = Vec::with_capacity(manifests.len());
        for manifest in &manifests {
            let mut map = std::collections::BTreeMap::new();
            for tensor in &manifest.tensors {
                let record = self.object_meta(tensor.shards[0].object_id)?;
                map.insert(tensor.name.clone(), record.version);
            }
            versions_by_manifest.push(map);
        }

        // Union of tensor names in version order, so the matrix is symmetric
        // about names that appear later.
        let mut names: Vec<String> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for manifest in &manifests {
            for tensor in &manifest.tensors {
                if seen.insert(tensor.name.clone()) {
                    names.push(tensor.name.clone());
                }
            }
        }

        let mut versions: Vec<VersionCost> = Vec::with_capacity(manifests.len());
        for (i, manifest) in manifests.iter().enumerate() {
            let mut independent = EdgeCost::default();
            let mut edge_sums: Vec<EdgeCost> = vec![EdgeCost::default(); i];
            let mut edge_compatible: Vec<bool> = vec![false; i];
            let mut logical_bytes = 0u64;
            for tensor_name in &names {
                let Some(tensor) = manifest.tensors.iter().find(|t| &t.name == tensor_name) else {
                    continue;
                };
                let restored =
                    self.restore_named_tensor_at(None, &checkpoints[i], tensor_name, None, None)?;
                logical_bytes = logical_bytes.saturating_add(tensor.element_bytes);
                let layout = crate::tensor_store::TensorLayoutSpec::row_major();
                let price = self.plan_tensor(
                    &tensor.dtype,
                    &tensor.shape,
                    &restored.bytes,
                    &layout,
                    &[],
                    &options.plan,
                )?;
                independent.bytes = independent
                    .bytes
                    .saturating_add(price.chosen_physical_bytes);
                independent.decode_ns = independent
                    .decode_ns
                    .saturating_add(hop_decode_ns(price.chosen, tensor.element_bytes));
                independent.total_tensors += 1;

                for j in 0..i {
                    // Only offer a causal version when the parent has the same
                    // tensor with the same dtype and shape; a mismatch would
                    // be ignored by the codec anyway, but stating it keeps the
                    // graph honest.
                    let parent_tensor =
                        manifests[j].tensors.iter().find(|t| t.name == *tensor_name);
                    let causal: Vec<crate::identity::Hash32> = match parent_tensor {
                        Some(parent)
                            if parent.dtype == tensor.dtype && parent.shape == tensor.shape =>
                        {
                            versions_by_manifest[j]
                                .get(tensor_name)
                                .copied()
                                .into_iter()
                                .collect()
                        }
                        _ => Vec::new(),
                    };
                    let price = self.plan_tensor(
                        &tensor.dtype,
                        &tensor.shape,
                        &restored.bytes,
                        &layout,
                        &causal,
                        &options.plan,
                    )?;
                    let sum = &mut edge_sums[j];
                    sum.bytes = sum.bytes.saturating_add(price.chosen_physical_bytes);
                    sum.decode_ns = sum
                        .decode_ns
                        .saturating_add(hop_decode_ns(price.chosen, tensor.element_bytes));
                    sum.total_tensors += 1;
                    if is_delta_strategy(price.chosen) && !causal.is_empty() {
                        sum.coupled_ns = sum
                            .coupled_ns
                            .saturating_add(hop_decode_ns(price.chosen, tensor.element_bytes));
                        sum.coupled_tensors += 1;
                        edge_compatible[j] = true;
                    }
                }
            }
            independent.bytes = independent.bytes.saturating_add(manifest_bytes[i]);
            let edges = edge_sums
                .into_iter()
                .zip(edge_compatible)
                .map(|(mut edge, compatible)| {
                    if !compatible {
                        return None;
                    }
                    edge.bytes = edge.bytes.saturating_add(manifest_bytes[i]);
                    Some(edge)
                })
                .collect();
            versions.push(VersionCost {
                name: checkpoints[i].clone(),
                logical_bytes,
                independent,
                edges,
            });
        }
        let graph = VersionGraph { versions };
        graph.validate()?;
        Ok(graph)
    }

    /// Write a chosen parent graph into this store, in version order.
    ///
    /// `sources` are `(checkpoint name, safetensors path)`; `plan.parents[i]`
    /// names an earlier source index or `None`. Every version is written with
    /// [`crate::checkpoint::CheckpointWriteOptions::causal_checkpoint`] set to
    /// the chosen earlier name, so the decision surface is the existing causal
    /// checkpoint path; no format changes.
    pub fn write_parent_graph(
        &mut self,
        sources: &[(String, std::path::PathBuf)],
        plan: &ParentGraphPlan,
        write: &crate::checkpoint::CheckpointWriteOptions,
    ) -> Result<Vec<crate::checkpoint::CheckpointWriteReport>> {
        if plan.parents.len() != sources.len() {
            return Err(Error::invalid_argument(format!(
                "plan has {} versions but {} sources were supplied",
                plan.parents.len(),
                sources.len()
            )));
        }
        let mut reports = Vec::with_capacity(sources.len());
        for (i, (name, path)) in sources.iter().enumerate() {
            let mut options = write.clone();
            options.causal_checkpoint = plan.parents[i].map(|j| {
                debug_assert!(j < i);
                sources[j].0.clone()
            });
            reports.push(self.write_checkpoint(name, path, &options)?);
        }
        Ok(reports)
    }
}

fn is_delta_strategy(strategy: crate::tensor_store::TensorStrategy) -> bool {
    use crate::tensor_store::TensorStrategy::*;
    matches!(
        strategy,
        WeightDelta | SparseDelta | ZstdPatch | SemanticDelta
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_from(rows: &[(&[Option<EdgeCost>], EdgeCost)]) -> VersionGraph {
        let mut versions = Vec::new();
        for (i, (edges, independent)) in rows.iter().enumerate() {
            assert_eq!(edges.len(), i);
            versions.push(VersionCost {
                name: format!("v{i}"),
                logical_bytes: 1_000,
                independent: *independent,
                edges: edges.to_vec(),
            });
        }
        VersionGraph { versions }
    }

    fn edge(bytes: u64, ns: u64) -> EdgeCost {
        EdgeCost {
            bytes,
            decode_ns: ns,
            coupled_ns: ns,
            coupled_tensors: 1,
            total_tensors: 1,
        }
    }

    #[test]
    fn supported_depth_matches_the_frozen_reader_bound() {
        assert_eq!(
            MAX_SUPPORTED_DEPTH as usize,
            crate::representation::MAX_CODEC_DEPENDENCY_DEPTH
        );
    }

    #[test]
    fn graph_shape_is_validated() {
        let graph = VersionGraph {
            versions: vec![VersionCost {
                name: "v0".into(),
                logical_bytes: 1,
                independent: edge(1, 1),
                edges: vec![Some(edge(1, 1))],
            }],
        };
        assert!(graph.validate().is_err());
    }

    #[test]
    fn objective_is_bytes_with_zero_lambda() {
        let graph = graph_from(&[(&[], edge(100, 10)), (&[Some(edge(20, 5))], edge(90, 9))]);
        let options = PlanOptions {
            lambda_read_ns: 0,
            ..PlanOptions::default()
        };
        let plan = optimize(&graph, &options).unwrap();
        assert_eq!(plan.parents, vec![None, Some(0)]);
        assert_eq!(plan.predicted_bytes, 120);
        assert_eq!(plan.objective, 120);
    }

    #[test]
    fn lambda_prefers_restart() {
        let graph = graph_from(&[(&[], edge(100, 10)), (&[Some(edge(20, 100))], edge(90, 9))]);
        let options = PlanOptions {
            lambda_read_ns: 1,
            reads_per_version: 1,
            ..PlanOptions::default()
        };
        let plan = optimize(&graph, &options).unwrap();
        // Delta: 20 + (10+100) = 130. Restart: 90 + 9 = 99, plus the prefix 110.
        assert_eq!(plan.parents, vec![None, None]);
    }

    #[test]
    fn depth_bound_is_enforced() {
        let graph = graph_from(&[
            (&[], edge(100, 1)),
            (&[Some(edge(10, 1))], edge(100, 1)),
            (&[Some(edge(10, 1)), Some(edge(10, 1))], edge(100, 1)),
        ]);
        let options = PlanOptions {
            max_depth: 1,
            ..PlanOptions::default()
        };
        let plan = optimize(&graph, &options).unwrap();
        assert_eq!(plan.max_depth, 1);
        assert!(plan.depths.iter().all(|d| *d <= 1));
    }

    #[test]
    fn dp_matches_bruteforce_on_a_chain_or_restart_optimum() {
        // Every non-adjacent edge is deliberately expensive, so the optimum is
        // a chain/restart plan and the restricted DP must find it.
        let graph = graph_from(&[
            (&[], edge(100, 10)),
            (&[Some(edge(20, 8))], edge(90, 9)),
            (&[Some(edge(150, 8)), Some(edge(21, 8))], edge(91, 9)),
            (
                &[Some(edge(150, 8)), Some(edge(150, 8)), Some(edge(22, 8))],
                edge(92, 9),
            ),
        ]);
        let mut options = PlanOptions {
            lambda_read_ns: 1,
            ..PlanOptions::default()
        };
        options.solver = Solver::Dp;
        let dp = optimize(&graph, &options).unwrap();
        options.solver = Solver::BruteForce;
        let brute = optimize(&graph, &options).unwrap();
        assert_eq!(dp.parents, brute.parents);
        assert_eq!(dp.objective, brute.objective);
    }

    #[test]
    fn brute_force_can_prefer_a_non_adjacent_parent() {
        // Version 3 is much smaller against version 0 than against version 2.
        let graph = graph_from(&[
            (&[], edge(100, 10)),
            (&[Some(edge(90, 5))], edge(100, 10)),
            (&[Some(edge(90, 5)), Some(edge(100, 5))], edge(100, 10)),
            (
                &[Some(edge(10, 5)), Some(edge(100, 5)), Some(edge(100, 5))],
                edge(100, 10),
            ),
        ]);
        let mut options = PlanOptions {
            lambda_read_ns: 0,
            max_depth: 8,
            ..PlanOptions::default()
        };
        options.solver = Solver::BruteForce;
        let brute = optimize(&graph, &options).unwrap();
        assert_eq!(brute.parents[3], Some(0));
        options.solver = Solver::Dp;
        let dp = optimize(&graph, &options).unwrap();
        assert_ne!(dp.parents, brute.parents);
        assert!(dp.objective >= brute.objective);
    }

    #[test]
    fn determinism_is_total() {
        let graph = graph_from(&[
            (&[], edge(100, 10)),
            (&[Some(edge(10, 5))], edge(100, 10)),
            (&[Some(edge(20, 5)), Some(edge(10, 5))], edge(100, 10)),
        ]);
        let options = PlanOptions {
            lambda_read_ns: 0,
            ..PlanOptions::default()
        };
        let a = optimize(&graph, &options).unwrap();
        let b = optimize(&graph, &options).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn sla_refusal_is_typed_and_does_not_degrade() {
        let graph = graph_from(&[
            (&[], edge(100, 100)),
            (&[Some(edge(10, 100))], edge(100, 100)),
        ]);
        let options = PlanOptions {
            sla_ns: Some(50),
            lambda_read_ns: 0,
            ..PlanOptions::default()
        };
        let err = optimize(&graph, &options).unwrap_err();
        assert_eq!(err.class(), Class::LimitExceeded);
        assert!(err.message().contains("SLA"));
    }

    #[test]
    fn restore_overhead_is_charged_once_per_version() {
        let graph = graph_from(&[
            (&[], edge(100, 10)),
            (&[Some(edge(20, 8))], edge(90, 9)),
            (&[Some(edge(20, 8)), Some(edge(20, 8))], edge(90, 9)),
        ]);
        let options = PlanOptions::default();
        let term = options
            .restore_overhead
            .expect("the default must be measured");
        let plan = evaluate(&graph, &[None, Some(0), Some(1)], &options).unwrap();
        for (i, version) in graph.versions.iter().enumerate() {
            let expected = term.estimate_ns(
                u64::from(version.independent.total_tensors),
                version.logical_bytes,
            );
            assert_eq!(plan.predicted_overhead_ns[i], expected);
            assert_eq!(
                plan.predicted_chain_ns[i],
                plan.predicted_decode_chain_ns[i] + expected
            );
        }
        // The overhead does not grow with chain depth: version 2's overhead is
        // its own version term, not two hops' worth.
        assert_eq!(plan.predicted_overhead_ns[2], plan.predicted_overhead_ns[0]);
        assert_eq!(plan.restore_overhead, Some(term));
        assert_eq!(
            plan.restore_overhead_calibration,
            RESTORE_OVERHEAD_CALIBRATION
        );
    }

    #[test]
    fn an_sla_claim_without_a_measured_overhead_is_refused() {
        let graph = graph_from(&[(&[], edge(100, 10)), (&[Some(edge(20, 5))], edge(90, 9))]);
        let options = PlanOptions {
            sla_ns: Some(1_000_000_000),
            restore_overhead: None,
            ..PlanOptions::default()
        };
        let err = optimize(&graph, &options).unwrap_err();
        assert_eq!(err.class(), Class::InvalidArgument);
        assert!(err.message().contains("restore-overhead"));
    }

    #[test]
    fn the_sla_uses_the_corrected_restore_not_the_raw_decode_rows() {
        // The raw decode rows fit comfortably under the SLA; the measured
        // restore overhead does not. The corrected model must refuse.
        let graph = graph_from(&[(&[], edge(100, 10)), (&[Some(edge(20, 8))], edge(90, 9))]);
        let measured = RestoreOverhead {
            per_version_ns: 0,
            per_tensor_ns: 0,
            per_mib_ns: 100_000_000,
            margin_ns_per_mib: 1,
        };
        let options = PlanOptions {
            sla_ns: Some(50_000),
            restore_overhead: Some(measured),
            ..PlanOptions::default()
        };
        let err = optimize(&graph, &options).unwrap_err();
        assert_eq!(err.class(), Class::LimitExceeded);
        assert!(err.message().contains("SLA"));
        // The same graph with the overhead switched off (and no SLA) would
        // have claimed 18 ns.
        let raw = PlanOptions {
            sla_ns: None,
            restore_overhead: None,
            ..PlanOptions::default()
        };
        let plan = evaluate(&graph, &[None, Some(0)], &raw).unwrap();
        assert!(plan.worst_restore_ns < 50_000);
    }

    #[test]
    fn storage_budget_refusal() {
        let graph = graph_from(&[(&[], edge(100, 1)), (&[Some(edge(10, 1))], edge(100, 1))]);
        let options = PlanOptions {
            storage_budget: Some(105),
            ..PlanOptions::default()
        };
        // Delta total 110 > 105; independent total 200 > 105.
        let err = optimize(&graph, &options).unwrap_err();
        assert_eq!(err.class(), Class::LimitExceeded);
    }

    #[test]
    fn brute_force_limit_is_typed() {
        let mut versions = Vec::new();
        for i in 0..3 {
            versions.push(VersionCost {
                name: format!("v{i}"),
                logical_bytes: 1,
                independent: edge(100, 1),
                edges: (0..i).map(|_| Some(edge(10, 1))).collect(),
            });
        }
        let graph = VersionGraph { versions };
        let options = PlanOptions {
            solver: Solver::BruteForce,
            brute_force_limit: 2,
            ..PlanOptions::default()
        };
        let err = optimize(&graph, &options).unwrap_err();
        assert_eq!(err.class(), Class::LimitExceeded);
    }

    #[test]
    fn incompatible_parent_is_rejected() {
        let graph = graph_from(&[(&[], edge(100, 1)), (&[None], edge(100, 1))]);
        let err = evaluate(&graph, &[None, Some(0)], &PlanOptions::default()).unwrap_err();
        assert_eq!(err.class(), Class::InvalidArgument);
    }
}
