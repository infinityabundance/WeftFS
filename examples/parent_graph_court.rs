//! S13 court: choose a parent graph for a real checkpoint trajectory.
//!
//! One invocation does the whole experiment, because the measured edge matrix
//! is expensive to build and would be lossy to serialize between phases:
//!
//! 1. verify the corpus (SHA-256 of every source container);
//! 2. write every version independently into a private probe store;
//! 3. price every earlier-version parent with the shipped tensor planner
//!    ([`weftfs::store::Store::price_parent_graph`]);
//! 4. run the chain/restart DP, the exhaustive oracle (N <= 12), and the
//!    greedy solver over the same measured matrix;
//! 5. realise the current chain, restart intervals 2/3/4, and the optimizer's
//!    plan in separate stores; measure bytes and restore medians; verify every
//!    version byte-for-byte;
//! 6. record SLA acceptance and refusal.
//!
//! The harness never builds a hand-written cost formula: the byte price comes
//! from `Store::plan_tensor` and the decode price from
//! `weftfs::cost_model`, exactly as the store's own planner does.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::plan_graph::{self, PlanOptions, PriceOptions, Solver, VersionGraph};
use weftfs::safetensors::SafetensorsFile;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::TensorPlanOptions;

fn main() {
    if let Err(err) = run() {
        eprintln!("parent_graph_court: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = PathBuf::from(arg(&args, "--corpus").ok_or("--corpus DIR is required")?);
    let work = PathBuf::from(arg(&args, "--work").ok_or("--work DIR is required")?);
    let out = PathBuf::from(arg(&args, "--out").ok_or("--out FILE is required")?);
    let max_depth: u32 = arg(&args, "--max-depth")
        .unwrap_or("8")
        .parse()
        .map_err(|_| "bad --max-depth")?;
    let lambda_ns: Option<u64> = match arg(&args, "--lambda-ns") {
        Some(text) => Some(text.parse().map_err(|_| "bad --lambda-ns")?),
        None => None,
    };
    let reads_per_version: u64 = arg(&args, "--reads-per-version")
        .unwrap_or("1")
        .parse()
        .map_err(|_| "bad --reads-per-version")?;
    let sla_ms: Option<u64> = match arg(&args, "--sla-ms") {
        Some(text) => Some(text.parse().map_err(|_| "bad --sla-ms")?),
        None => None,
    };
    let restore_reps: usize = arg(&args, "--restore-reps")
        .unwrap_or("7")
        .parse()
        .map_err(|_| "bad --restore-reps")?;
    if restore_reps < 5 {
        return Err("--restore-reps must be at least 5 for the S13b prediction-error court".into());
    }
    let sla_sweep_ms: Vec<u64> = arg(&args, "--sla-sweep-ms")
        .unwrap_or("500,850,900,950,1000,2000,2500,3000,4000,6000,8000,10000,11000,14000")
        .split(',')
        .map(|item| item.trim().parse::<u64>().map_err(|_| "bad --sla-sweep-ms"))
        .collect::<Result<_, _>>()?;
    let steps: Vec<u32> = arg(&args, "--steps")
        .unwrap_or("0,1,2,4,8,16,64,512,1000")
        .split(',')
        .map(|s| s.trim().parse::<u32>().map_err(|_| "bad --steps"))
        .collect::<Result<_, _>>()?;
    let skip_realize = flag(&args, "--skip-realize");
    let keep = flag(&args, "--keep");
    // S13c inputs. The margin is derived from the two committed S13b receipts;
    // the hop rows are re-measured by `examples/calibrate_restore.rs --chains-only`
    // in the same court invocation and compared with the baked constants.
    let overhead_receipt = PathBuf::from(
        arg(&args, "--overhead-receipt")
            .unwrap_or("bench/receipts/s13b-restore-floor-20261108T230417Z.json"),
    );
    let floor_court_receipt = PathBuf::from(
        arg(&args, "--floor-court-receipt")
            .unwrap_or("bench/receipts/parent-graph-20261108T232016Z.json"),
    );
    let hop_receipt = arg(&args, "--hop-receipt").map(PathBuf::from);

    if work.exists() {
        fs::remove_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    }
    fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;

    let mut checks: BTreeMap<String, bool> = BTreeMap::new();

    // ---- corpus -----------------------------------------------------------
    let mut corpus_rows = Vec::new();
    let mut sources: Vec<(String, PathBuf)> = Vec::new();
    for step in &steps {
        let name = format!("step{step}");
        let path = corpus.join(format!("pythia-70m-step{step}.safetensors"));
        if !path.exists() {
            return Err(format!("missing corpus file {}", path.display()));
        }
        let file = SafetensorsFile::parse(&path).map_err(|e| e.to_string())?;
        let bytes = fs::metadata(&path).map_err(|e| e.to_string())?.len();
        corpus_rows.push(json!({
            "step": step,
            "name": name,
            "path": path.display().to_string(),
            "bytes": bytes,
            "sha256": hex::encode(file.sha256),
            "tensors": file.tensors.len(),
            "element_bytes": file.tensors.iter().map(|t| t.byte_len()).sum::<u64>(),
        }));
        sources.push((name, path));
    }
    let labels: Vec<String> = sources.iter().map(|(name, _)| name.clone()).collect();
    eprintln!("parent_graph: verified {} corpus files", sources.len());

    // ---- probe store: every version independent ---------------------------
    let probe_path = work.join("probe");
    let mut probe = Store::init(
        &probe_path,
        Options {
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            ..Options::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let write_options = CheckpointWriteOptions {
        shards: 1,
        axis: 0,
        strategy: ShardStrategy::Planned,
        semantic_search: false,
        ..CheckpointWriteOptions::default()
    };
    let mut independent = Vec::new();
    for (index, (name, path)) in sources.iter().enumerate() {
        let before = data_bytes(&probe_path);
        let started = Instant::now();
        let report = probe
            .write_checkpoint(name, path, &write_options)
            .map_err(|e| e.to_string())?;
        let write_ms = started.elapsed().as_secs_f64() * 1000.0;
        let after = data_bytes(&probe_path);
        eprintln!("parent_graph: independent {name}: write {write_ms:.1} ms, data {after} B");
        independent.push(json!({
            "index": index,
            "name": name,
            "write_ms": write_ms,
            "marginal_data_bytes": after.saturating_sub(before),
            "cumulative_data_bytes": after,
            "manifest_id": report.manifest_id.to_hex(),
            "source_sha256": hex::encode(report.source_sha256),
        }));
    }
    checks.insert(
        "probe_writes".to_owned(),
        independent.len() == sources.len(),
    );

    // ---- measured version graph ------------------------------------------
    let price_options = PriceOptions {
        plan: TensorPlanOptions {
            semantic_search: false,
            ..TensorPlanOptions::default()
        },
        max_tensors: 200_000,
        shards: 1,
    };
    let price_started = Instant::now();
    let graph = probe
        .price_parent_graph(&labels, &price_options)
        .map_err(|e| e.to_string())?;
    let price_ms = price_started.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "parent_graph: priced {} versions in {price_ms:.1} ms",
        graph.len()
    );
    drop(probe);

    let graph_row = graph_json(&graph);
    checks.insert("graph_valid".to_owned(), graph.validate().is_ok());

    // ---- S13c: measured SLA margin and graph-hop rows ---------------------
    //
    // The margin is *derived* here from the two committed S13b receipts and
    // compared with the baked constant. The hop rows are re-measured by a
    // separate `calibrate_restore --chains-only` invocation in the same court
    // and compared with the baked table.
    let baked_margin = weftfs::cost_model::RestoreOverhead::MEASURED.margin_ns_per_mib;
    let margin = derive_sla_margin(&overhead_receipt, &floor_court_receipt)?;
    checks.insert(
        "baked_margin_matches_derivation".to_owned(),
        margin.ns_per_mib == baked_margin,
    );
    let hop_calibration = match hop_receipt.as_ref() {
        Some(path) => Some(read_hop_calibration(path)?),
        None => None,
    };
    checks.insert(
        "baked_hop_rows_match_remeasurement".to_owned(),
        hop_calibration
            .as_ref()
            .map(|row| row.matches_baked)
            .unwrap_or(false),
    );

    // ---- plan classes -----------------------------------------------------
    let n = graph.len();
    let chain = chain_plan(n);
    let k2 = restart_plan(n, 2);
    let k3 = restart_plan(n, 3);
    let k4 = restart_plan(n, 4);

    let base_options = PlanOptions {
        lambda_read_ns: 0,
        reads_per_version,
        max_depth,
        sla_ns: sla_ms.map(|ms| ms.saturating_mul(1_000_000)),
        storage_budget: None,
        solver: Solver::Dp,
        ..PlanOptions::default()
    };

    let chain_metrics = plan_graph::evaluate(&graph, &chain, &base_options)
        .map_err(|e| format!("chain plan: {e}"))?;
    let k2_metrics = plan_graph::evaluate(&graph, &k2, &base_options)
        .map_err(|e| format!("restart-2 plan: {e}"))?;
    let k3_metrics = plan_graph::evaluate(&graph, &k3, &base_options)
        .map_err(|e| format!("restart-3 plan: {e}"))?;
    let k4_metrics = plan_graph::evaluate(&graph, &k4, &base_options)
        .map_err(|e| format!("restart-4 plan: {e}"))?;

    // Break-even lambda between the chain and each restart interval: the plan
    // with more bytes wins only when a saved decode nanosecond is worth at
    // least `extra_bytes / saved_ns` bytes.
    let break_even = |other: &plan_graph::ParentGraphPlan| -> Option<f64> {
        let extra = i128::from(other.predicted_bytes) - i128::from(chain_metrics.predicted_bytes);
        let saved = chain_metrics.predicted_total_decode_ns as i128
            - other.predicted_total_decode_ns as i128;
        if saved <= 0 || extra <= 0 {
            return None;
        }
        Some(extra as f64 / saved as f64)
    };
    let be2 = break_even(&k2_metrics);
    let be3 = break_even(&k3_metrics);
    let be4 = break_even(&k4_metrics);
    let chosen_lambda = lambda_ns.unwrap_or_else(|| {
        // The smallest integer lambda at which any restart interval beats the
        // chain on the measured matrix; 0 when none does.
        let mut best: Option<u64> = None;
        for value in [be2, be3, be4].into_iter().flatten() {
            let lambda = value.ceil().max(0.0) as u64;
            best = Some(best.map_or(lambda, |current| current.min(lambda)));
        }
        best.unwrap_or(0)
    });

    let options = PlanOptions {
        lambda_read_ns: chosen_lambda,
        ..base_options.clone()
    };

    let dp_plan = plan_graph::optimize(&graph, &options).map_err(|e| format!("dp: {e}"))?;
    let mut dp0_options = options.clone();
    dp0_options.lambda_read_ns = 0;
    let dp0_plan = plan_graph::optimize(&graph, &dp0_options).map_err(|e| format!("dp0: {e}"))?;
    let mut greedy_options = options.clone();
    greedy_options.solver = Solver::Greedy;
    let greedy_plan =
        plan_graph::optimize(&graph, &greedy_options).map_err(|e| format!("greedy: {e}"))?;

    let (brute_plan, brute_error) = if n <= plan_graph::DEFAULT_BRUTEFORCE_LIMIT {
        let mut brute_options = options.clone();
        brute_options.solver = Solver::BruteForce;
        match plan_graph::optimize(&graph, &brute_options) {
            Ok(plan) => (Some(plan), None),
            Err(err) => (None, Some(format!("{}: {}", err.class(), err.message()))),
        }
    } else {
        (
            None,
            Some(format!(
                "graph has {n} versions; oracle limit is {}",
                plan_graph::DEFAULT_BRUTEFORCE_LIMIT
            )),
        )
    };
    let dp_matches_brute = match &brute_plan {
        Some(brute) => brute.objective == dp_plan.objective && brute.parents == dp_plan.parents,
        None => false,
    };
    checks.insert("dp_matches_brute_force".to_owned(), dp_matches_brute);

    // ---- lambda x SLA sweep -------------------------------------------------
    //
    // Pure model first: collect every distinct accepted plan so the court can
    // realise each one once and attach its measured worst restore.
    let mut sweep_options_rows: Vec<(u64, u64, Json, Option<Vec<Option<usize>>>)> = Vec::new();
    let mut accepted_parents: Vec<Vec<Option<usize>>> = Vec::new();
    for sla_ms in &sla_sweep_ms {
        for lambda in [0u64, 1u64] {
            let mut sweep_options = options.clone();
            sweep_options.lambda_read_ns = lambda;
            sweep_options.sla_ns = Some(sla_ms.saturating_mul(1_000_000));
            match plan_graph::optimize(&graph, &sweep_options) {
                Ok(plan) => {
                    if !accepted_parents.iter().any(|p| p == &plan.parents) {
                        accepted_parents.push(plan.parents.clone());
                    }
                    sweep_options_rows.push((
                        lambda,
                        *sla_ms,
                        json!({
                            "kind": "accepted",
                            "predicted_worst_restore_ns": plan.worst_restore_ns,
                            "predicted_decode_worst_ns": plan.predicted_decode_chain_ns.iter().copied().max().unwrap_or(0),
                            "max_depth": plan.max_depth,
                            "parents": plan.parent_indices(),
                        }),
                        Some(plan.parents.clone()),
                    ));
                }
                Err(err) => sweep_options_rows.push((
                    lambda,
                    *sla_ms,
                    json!({
                        "kind": "refused",
                        "class": err.class().as_str(),
                        "message": err.message(),
                    }),
                    None,
                )),
            }
        }
    }

    // ---- SLA acceptance and typed refusal --------------------------------
    let mut sla_rows = Vec::new();
    if let Some(ms) = sla_ms {
        let sla_ns = ms.saturating_mul(1_000_000);
        let accepted = plan_graph::optimize(&graph, &options);
        sla_rows.push(json!({
            "sla_ms": ms,
            "sla_ns": sla_ns,
            "outcome": match &accepted {
                Ok(plan) => json!({
                    "kind": "accepted",
                    "worst_restore_ns": plan.worst_restore_ns,
                    "predicted_decode_worst_ns": plan.predicted_decode_chain_ns.iter().copied().max().unwrap_or(0),
                    "satisfied": plan.sla_satisfied,
                    "parents": plan.parent_indices(),
                }),
                Err(err) => json!({
                    "kind": "refused",
                    "class": err.class().as_str(),
                    "message": err.message(),
                }),
            },
        }));
        // A deliberately impossible SLA must be refused with a typed error.
        let mut impossible = options.clone();
        impossible.sla_ns = Some(1);
        match plan_graph::optimize(&graph, &impossible) {
            Ok(_) => sla_rows.push(json!({
                "sla_ms": 0.000001,
                "outcome": {"kind": "accepted", "bug": "an impossible SLA was accepted"},
            })),
            Err(err) => sla_rows.push(json!({
                "sla_ms": 0.000001,
                "outcome": {
                    "kind": "refused",
                    "class": err.class().as_str(),
                    "message": err.message(),
                },
            })),
        }
        // Without a measured overhead model an SLA claim is refused.
        let mut unmeasured = options.clone();
        unmeasured.restore_overhead = None;
        match plan_graph::optimize(&graph, &unmeasured) {
            Ok(_) => sla_rows.push(json!({
                "sla_ms": ms,
                "restore_overhead": "none",
                "outcome": {"kind": "accepted", "bug": "an SLA was accepted without a restore-overhead model"},
            })),
            Err(err) => sla_rows.push(json!({
                "sla_ms": ms,
                "restore_overhead": "none",
                "outcome": {"kind": "refused", "class": err.class().as_str(), "message": err.message()},
            })),
        }
    }

    // ---- realise the plan classes ---------------------------------------
    let mut realizations = Vec::new();
    if !skip_realize {
        let mut plan_list: Vec<(String, Vec<Option<usize>>)> = vec![
            ("chain".to_owned(), chain.clone()),
            ("restart-2".to_owned(), k2.clone()),
            ("restart-3".to_owned(), k3.clone()),
            ("restart-4".to_owned(), k4.clone()),
            ("optimizer".to_owned(), dp_plan.parents.clone()),
            ("optimizer-bytes-only".to_owned(), dp0_plan.parents.clone()),
        ];
        // Realise every plan the SLA sweep accepted, so an accepted plan can be
        // checked against its measured restore. Bound the extra work.
        let mut extra = 0usize;
        for parents in &accepted_parents {
            if plan_list.iter().any(|(_, existing)| existing == parents) {
                continue;
            }
            if extra >= 3 {
                eprintln!("parent_graph: more than 3 sweep-only plans; extra ones stay unmeasured");
                break;
            }
            extra += 1;
            plan_list.push((format!("sweep-plan-{extra}"), parents.clone()));
        }
        let mut seen_plans = std::collections::BTreeSet::new();
        for (label, parents) in plan_list {
            let key: Vec<i64> = parents
                .iter()
                .map(|parent| parent.map(|p| p as i64).unwrap_or(-1))
                .collect();
            if !seen_plans.insert(key) {
                eprintln!("parent_graph: skipping {label}; its parent list was already realised");
                continue;
            }
            let row = realize(
                &work,
                &label,
                &sources,
                &parents,
                &graph,
                &options,
                restore_reps,
            )?;
            checks.insert(
                format!("exact_{label}"),
                row.get("all_versions_exact")
                    .and_then(Json::as_bool)
                    .unwrap_or(false)
                    && row
                        .get("verify_full_ok")
                        .and_then(Json::as_bool)
                        .unwrap_or(false),
            );
            realizations.push(row);
        }
    }

    // ---- attach measured restores to the sweep ---------------------------
    let measurement_key = |parents: &[Option<usize>]| -> Vec<i64> {
        parents
            .iter()
            .map(|parent| parent.map(|p| p as i64).unwrap_or(-1))
            .collect()
    };
    let mut measured_by_plan: BTreeMap<Vec<i64>, (String, f64, bool)> = BTreeMap::new();
    for row in &realizations {
        let label = row["label"].as_str().unwrap_or("?").to_owned();
        let parents = row["predicted"]["parents"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|value| value.as_i64().unwrap_or(-1))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let worst = row["restore_worst_ms"].as_f64().unwrap_or(0.0);
        let exact = row["all_versions_exact"].as_bool().unwrap_or(false);
        measured_by_plan.insert(parents, (label, worst, exact));
    }
    let mut accepted_measured = 0usize;
    let mut accepted_unmeasured = 0usize;
    let mut accepted_misses = 0usize;
    let mut sla_sweep = Vec::new();
    for (lambda, sla_ms, outcome, parents) in &sweep_options_rows {
        let mut row = json!({
            "lambda_ns": lambda,
            "sla_ms": sla_ms,
            "outcome": outcome,
        });
        if let Some(parents) = parents {
            let key = measurement_key(parents);
            match measured_by_plan.get(&key) {
                Some((label, worst_ms, exact)) => {
                    accepted_measured += 1;
                    let holds = *worst_ms <= *sla_ms as f64;
                    if !holds {
                        accepted_misses += 1;
                    }
                    let predicted_ns = outcome["predicted_worst_restore_ns"].as_u64().unwrap_or(0);
                    let measured_ns = (*worst_ms * 1_000_000.0).round() as u64;
                    let error_ppm = if measured_ns == 0 {
                        0
                    } else {
                        (((predicted_ns as f64 - measured_ns as f64) / measured_ns as f64)
                            * 1_000_000.0)
                            .round() as i64
                    };
                    row["measured"] = json!({
                        "label": label,
                        "worst_restore_ms": worst_ms,
                        "sla_holds": holds,
                        "all_versions_exact": exact,
                        "prediction_error_ppm": error_ppm,
                    });
                }
                None => {
                    accepted_unmeasured += 1;
                    row["measured"] = Json::Null;
                }
            }
        }
        sla_sweep.push(row);
    }
    checks.insert(
        "no_accepted_plan_misses_its_sla".to_owned(),
        accepted_misses == 0 && accepted_unmeasured == 0,
    );
    // S13c acceptance target: the point prediction is within 15 % of the
    // measured worst restore for every realised plan class, including the floor.
    let floor_error = realizations
        .iter()
        .find(|row| {
            row["predicted"]["parents"]
                .as_array()
                .map(|values| values.iter().all(|value| value.as_i64() == Some(-1)))
                .unwrap_or(false)
        })
        .map(|row| {
            let predicted = row["predicted_restore_worst_ns"].as_f64().unwrap_or(0.0);
            let measured = row["restore_worst_ms"].as_f64().unwrap_or(0.0) * 1_000_000.0;
            if measured == 0.0 {
                0.0
            } else {
                (predicted - measured).abs() / measured
            }
        });
    checks.insert(
        "floor_prediction_within_15_percent".to_owned(),
        floor_error.map(|error| error <= 0.15).unwrap_or(false),
    );
    let worst_plan_error_ppm = realizations
        .iter()
        .filter_map(|row| row["worst_prediction_error_ppm"].as_i64())
        .max()
        .unwrap_or(i64::MAX);
    checks.insert(
        "every_plan_prediction_within_15_percent".to_owned(),
        !realizations.is_empty() && worst_plan_error_ppm <= 150_000,
    );
    // The smallest SLA at which each realised plan class is accepted, i.e. its
    // margin-adjusted bound. This is the accept/refuse boundary the docs quote.
    let sla_boundary = json!({
        "chain_ms": chain_metrics.worst_sla_bound_ns as f64 / 1_000_000.0,
        "restart-2_ms": k2_metrics.worst_sla_bound_ns as f64 / 1_000_000.0,
        "restart-3_ms": k3_metrics.worst_sla_bound_ns as f64 / 1_000_000.0,
        "restart-4_ms": k4_metrics.worst_sla_bound_ns as f64 / 1_000_000.0,
        "all-independent_ms": dp_plan.worst_sla_bound_ns as f64 / 1_000_000.0,
        "note": "an SLA below these values is refused; the measured restore of the accepted plan is compared against the SLA in the sweep",
    });

    // ---- lambda sweep -----------------------------------------------------
    let mut lambda_sweep = Vec::new();
    for lambda in [0u64, 1, 2, 4, 8, 16, 64] {
        let mut sweep_options = options.clone();
        sweep_options.lambda_read_ns = lambda;
        let dp = match plan_graph::optimize(&graph, &sweep_options) {
            Ok(plan) => json!({"kind": "plan", "plan": plan_json(&plan)}),
            Err(err) => {
                json!({"kind": "refused", "class": err.class().as_str(), "message": err.message()})
            }
        };
        let brute = if n <= plan_graph::DEFAULT_BRUTEFORCE_LIMIT {
            let mut brute_options = sweep_options.clone();
            brute_options.solver = Solver::BruteForce;
            match plan_graph::optimize(&graph, &brute_options) {
                Ok(plan) => json!({"kind": "plan", "plan": plan_json(&plan)}),
                Err(err) => {
                    json!({"kind": "refused", "class": err.class().as_str(), "message": err.message()})
                }
            }
        } else {
            json!({"kind": "skipped", "reason": "above the oracle limit"})
        };
        lambda_sweep.push(json!({
            "lambda_ns": lambda,
            "dp": dp,
            "brute_force": brute,
        }));
    }

    // ---- receipt ---------------------------------------------------------
    let receipt = json!({
        "schema": "weft.s13c.sla-honesty.v1",
        "court": {
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".into()),
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".into()),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".into()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".into()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".into()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".into()),
        },
        "revisions": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".into()),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".into()),
        },
        "corpus": corpus_rows,
        "configuration": {
            "steps": steps,
            "max_depth": max_depth,
            "reads_per_version": reads_per_version,
            "sla_ms": sla_ms,
            "lambda_ns": chosen_lambda,
            "lambda_ns_source": if lambda_ns.is_some() { "explicit" } else { "break-even" },
            "restore_reps": restore_reps,
            "restore_reps_minimum": 5,
            "sla_sweep_ms": sla_sweep_ms,
            "restore_overhead": {
                "calibration": options
                    .restore_overhead
                    .map(|_| weftfs::cost_model::RESTORE_OVERHEAD_CALIBRATION),
                "per_version_ns": options.restore_overhead.map(|term| term.per_version_ns),
                "per_tensor_ns": options.restore_overhead.map(|term| term.per_tensor_ns),
                "per_mib_ns": options.restore_overhead.map(|term| term.per_mib_ns),
                "note": "charged once per version restore, never per dependency hop",
            },
            "hop_calibration": weftfs::cost_model::CHAIN_HOP_CALIBRATION,
            "hop_rows": weftfs::cost_model::hop_table()
                .into_iter()
                .map(|(class, row)| json!({
                    "class": class.as_str(),
                    "fixed_ns": row.fixed_ns,
                    "ns_per_mib": row.ns_per_mib,
                }))
                .collect::<Vec<_>>(),
            "restore_note": "cold store reopen per repetition; OS page cache is warm, so this is decode-dominated and labelled",
            "pricing_ms": price_ms,
        },
        "independent": independent,
        "graph": graph_row,
        "plans": {
            "chain": plan_json(&chain_metrics),
            "restart-2": plan_json(&k2_metrics),
            "restart-3": plan_json(&k3_metrics),
            "restart-4": plan_json(&k4_metrics),
            "dp": plan_json(&dp_plan),
            "dp_bytes_only": plan_json(&dp0_plan),
            "greedy": plan_json(&greedy_plan),
            "brute_force": brute_plan.as_ref().map(plan_json),
            "brute_force_error": brute_error,
            "break_even_lambda": {
                "restart_2": be2,
                "restart_3": be3,
                "restart_4": be4,
            },
        },
        "sla": sla_rows,
        "sla_margin": margin_json(&margin, baked_margin, &overhead_receipt, &floor_court_receipt),
        "hop_calibration": hop_calibration.as_ref().map(|row| row.json.clone()),
        "sla_boundary": sla_boundary,
        "sla_sweep": sla_sweep,
        "sla_sweep_summary": {
            "accepted_with_measurement": accepted_measured,
            "accepted_without_measurement": accepted_unmeasured,
            "accepted_misses": accepted_misses,
        },
        "floor_prediction_error": floor_error,
        "lambda_sweep": lambda_sweep,
        "realizations": realizations,
        "checks": checks,
    });
    fs::write(
        &out,
        serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", out.display()))?;
    eprintln!("parent_graph: receipt written to {}", out.display());
    if !keep {
        // The work tree is large; the court script keeps it on failure only.
    }
    Ok(())
}

/// Write one plan into a fresh store and measure it.
#[allow(clippy::too_many_arguments)]
fn realize(
    work: &Path,
    label: &str,
    sources: &[(String, PathBuf)],
    parents: &[Option<usize>],
    graph: &VersionGraph,
    options: &PlanOptions,
    restore_reps: usize,
) -> Result<Json, String> {
    let predicted =
        plan_graph::evaluate(graph, parents, options).map_err(|e| format!("{label}: {e}"))?;
    let path = work.join(format!("realize-{label}"));
    let mut store = Store::init(
        &path,
        Options {
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            ..Options::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let write_options = CheckpointWriteOptions {
        shards: 1,
        axis: 0,
        strategy: ShardStrategy::Planned,
        semantic_search: false,
        ..CheckpointWriteOptions::default()
    };
    let mut writes = Vec::new();
    for (i, (name, source)) in sources.iter().enumerate() {
        let before = data_bytes(&path);
        let mut write = write_options.clone();
        write.causal_checkpoint = parents[i].map(|j| sources[j].0.clone());
        let started = Instant::now();
        let report = store
            .write_checkpoint(name, source, &write)
            .map_err(|e| e.to_string())?;
        let write_ms = started.elapsed().as_secs_f64() * 1000.0;
        let after = data_bytes(&path);
        let mut strategy_mix: BTreeMap<String, u64> = BTreeMap::new();
        let mut max_depth = 0u32;
        for tensor in &report.tensors {
            for id in &tensor.object_ids {
                let meta = store.tensor_metadata(*id).map_err(|e| e.to_string())?;
                *strategy_mix.entry(meta.strategy).or_insert(0) += 1;
                max_depth = max_depth.max(
                    store
                        .shard_dependency_depth(*id)
                        .map_err(|e| e.to_string())?,
                );
            }
        }
        eprintln!(
            "parent_graph: [{label}] {name} parent={:?}: write {write_ms:.1} ms, data {after} B, depth {max_depth}",
            parents[i]
        );
        writes.push(json!({
            "index": i,
            "name": name,
            "parent": parents[i].map(|p| p as i64).unwrap_or(-1),
            "parent_name": parents[i].map(|p| sources[p].0.clone()),
            "write_ms": write_ms,
            "marginal_data_bytes": after.saturating_sub(before),
            "cumulative_data_bytes": after,
            "max_dependency_depth": max_depth,
            "strategy_mix": strategy_mix,
            "manifest_id": report.manifest_id.to_hex(),
        }));
    }

    // Exactness and restore time per version.
    let source_files: Vec<SafetensorsFile> = sources
        .iter()
        .map(|(_, path)| SafetensorsFile::parse(path).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let mut restores = Vec::new();
    for (i, (name, _)) in sources.iter().enumerate() {
        let mut times = Vec::with_capacity(restore_reps);
        let mut last_report = None;
        for _ in 0..restore_reps.max(1) {
            // Reopen read-only so no in-process state survives between reps.
            let reader = Store::open(&path, OpenMode::ReadOnly).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            let started = Instant::now();
            let report = reader
                .restore_checkpoint(name, &CheckpointRestoreOptions::default(), &mut bytes)
                .map_err(|e| e.to_string())?;
            times.push(started.elapsed().as_secs_f64() * 1000.0);
            drop(bytes);
            last_report = Some((report, reader));
        }
        let (report, reader) = last_report.ok_or("no restore report")?;
        let source_sha = hex::encode(source_files[i].sha256);
        let mut all_tensors_exact = true;
        for tensor in &report.tensors {
            let expected = source_files[i]
                .tensors
                .iter()
                .find(|item| item.name == tensor.name)
                .ok_or_else(|| format!("{} is not in the source container", tensor.name))?;
            let payload = source_files[i]
                .read_tensor(expected)
                .map_err(|e| e.to_string())?;
            all_tensors_exact &= weftfs::safetensors::sha256(&payload) == tensor.sha256;
        }
        drop(reader);
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = times[times.len() / 2];
        restores.push(json!({
            "index": i,
            "name": name,
            "median_ms": median,
            "reps_ms": times,
            "predicted_restore_ms": predicted.predicted_chain_ns[i] as f64 / 1_000_000.0,
            "predicted_decode_ms": predicted.predicted_decode_chain_ns[i] as f64 / 1_000_000.0,
            "predicted_overhead_ms": predicted.predicted_overhead_ns[i] as f64 / 1_000_000.0,
            "predicted_margin_ms": predicted.predicted_margin_ns[i] as f64 / 1_000_000.0,
            "predicted_sla_bound_ms": predicted.predicted_sla_bound_ns[i] as f64 / 1_000_000.0,
            "prediction_error_ppm": if median > 0.0 {
                (((predicted.predicted_chain_ns[i] as f64 / 1_000_000.0 - median) / median) * 1_000_000.0).round() as i64
            } else {
                0
            },
            "exact_source_file": report.exact_source_file,
            "source_sha256_match": hex::encode(report.file_sha256) == source_sha,
            "all_tensors_exact": all_tensors_exact,
            "store_bytes_read": report.store_bytes_read,
            "tensors": report.tensors.len(),
        }));
    }

    let verify = store.verify(true).map_err(|e| e.to_string())?;
    let total = data_bytes(&path);
    let restore_sum: f64 = restores
        .iter()
        .filter_map(|row| row.get("median_ms").and_then(Json::as_f64))
        .sum();
    let worst = restores
        .iter()
        .filter_map(|row| row.get("median_ms").and_then(Json::as_f64))
        .fold(0.0f64, f64::max);
    let all_exact = restores.iter().all(|row| {
        row.get("exact_source_file")
            .and_then(Json::as_bool)
            .unwrap_or(false)
            && row
                .get("all_tensors_exact")
                .and_then(Json::as_bool)
                .unwrap_or(false)
    });
    let worst_prediction_error_ppm = restores
        .iter()
        .filter_map(|row| row.get("prediction_error_ppm").and_then(Json::as_i64))
        .map(i64::abs)
        .max()
        .unwrap_or(0);
    // For a pure chain, the per-hop measured increment is checked against the
    // measured delta row the model uses (hypothesis 3).
    let chain_hops: Option<Json> = if parents
        .iter()
        .enumerate()
        .all(|(i, parent)| i == 0 || *parent == Some(i - 1))
        && parents.len() > 1
    {
        let mut hops = Vec::new();
        for i in 1..parents.len() {
            let previous = restores[i - 1]["median_ms"].as_f64().unwrap_or(0.0);
            let current = restores[i]["median_ms"].as_f64().unwrap_or(0.0);
            let predicted_decode = predicted.predicted_decode_chain_ns[i] as f64
                - predicted.predicted_decode_chain_ns[i - 1] as f64;
            hops.push(json!({
                "index": i,
                "measured_increment_ms": current - previous,
                "predicted_decode_increment_ms": predicted_decode / 1_000_000.0,
                "increment_error_ppm": if current - previous > 0.0 {
                    ((((predicted_decode / 1_000_000.0) - (current - previous)) / (current - previous)) * 1_000_000.0).round() as i64
                } else { 0 },
            }));
        }
        Some(json!(hops))
    } else {
        None
    };
    Ok(json!({
        "label": label,
        "store": path.display().to_string(),
        "predicted": plan_json(&predicted),
        "writes": writes,
        "total_data_bytes": total,
        "predicted_bytes": predicted.predicted_bytes,
        "bytes_error_bytes": i128::from(total) - i128::from(predicted.predicted_bytes),
        "restores": restores,
        "restore_sum_ms": restore_sum,
        "restore_worst_ms": worst,
        "predicted_restore_worst_ns": predicted.worst_restore_ns,
        "predicted_sla_bound_worst_ns": predicted.worst_sla_bound_ns,
        "worst_prediction_error_ppm": worst_prediction_error_ppm,
        "chain_hops": chain_hops,
        "all_versions_exact": all_exact,
        "verify_full_ok": verify.ok,
        "verify_problems": verify.problems.len(),
        "verify_checked_representations": verify.checked_representations,
    }))
}

fn plan_json(plan: &plan_graph::ParentGraphPlan) -> Json {
    json!({
        "schema": plan.schema,
        "solver": plan.solver.as_str(),
        "parents": plan.parent_indices(),
        "objective": plan.objective.to_string(),
        "predicted_bytes": plan.predicted_bytes,
        "predicted_decode_chain_ns": plan.predicted_decode_chain_ns,
        "predicted_overhead_ns": plan.predicted_overhead_ns,
        "predicted_margin_ns": plan.predicted_margin_ns,
        "predicted_chain_ns": plan.predicted_chain_ns,
        "predicted_sla_bound_ns": plan.predicted_sla_bound_ns,
        "predicted_total_decode_ns": plan.predicted_total_decode_ns.to_string(),
        "restore_overhead_calibration": plan.restore_overhead_calibration,
        "sla_margin_calibration": plan.sla_margin_calibration,
        "depths": plan.depths,
        "dependencies": plan.dependencies,
        "max_depth": plan.max_depth,
        "worst_restore_ns": plan.worst_restore_ns,
        "worst_restore_ms": plan.worst_restore_ns as f64 / 1_000_000.0,
        "worst_sla_bound_ns": plan.worst_sla_bound_ns,
        "worst_sla_bound_ms": plan.worst_sla_bound_ns as f64 / 1_000_000.0,
        "sla_satisfied": plan.sla_satisfied,
        "budget_satisfied": plan.budget_satisfied,
    })
}

fn graph_json(graph: &VersionGraph) -> Json {
    json!({
        "schema": plan_graph::VERSION_GRAPH_SCHEMA,
        "versions": graph.versions.iter().enumerate().map(|(i, version)| json!({
            "index": i,
            "name": version.name,
            "logical_bytes": version.logical_bytes,
            "independent": {
                "bytes": version.independent.bytes,
                "decode_ns": version.independent.decode_ns,
                "tensors": version.independent.total_tensors,
            },
            "parents": version.edges.iter().enumerate().map(|(j, edge)| json!({
                "parent": j,
                "compatible": edge.is_some(),
                "bytes": edge.as_ref().map(|e| e.bytes),
                "decode_ns": edge.as_ref().map(|e| e.decode_ns),
                "coupled_ns": edge.as_ref().map(|e| e.coupled_ns),
                "coupled_tensors": edge.as_ref().map(|e| e.coupled_tensors),
                "total_tensors": edge.as_ref().map(|e| e.total_tensors),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// The S13c SLA margin derived from the two committed S13b receipts.
struct SlaMargin {
    ns_per_mib: u64,
    fit_allowance_ns_per_mib: u64,
    cold_allowance_ns_per_mib: u64,
    worst_in_regime_residual_ppm: i64,
    in_regime_points: u64,
    excluded_below_bytes: u64,
    cold_deficit_ns: u64,
    logical_bytes: u64,
    predicted_restore_ns: u64,
    measured_restore_ns: u64,
}

/// Derive the SLA margin: the largest in-regime fit residual applied to the
/// measured per-MiB overhead, plus the measured cold-versus-warm
/// under-prediction of the all-independent floor per logical MiB.
fn derive_sla_margin(overhead: &Path, floor: &Path) -> Result<SlaMargin, String> {
    let calibration: Json = read_receipt(overhead)?;
    let court: Json = read_receipt(floor)?;
    let per_mib_ns = calibration["fit"]["per_mib_ns"]
        .as_u64()
        .ok_or("the overhead receipt has no fit.per_mib_ns")?;
    let mut point_bytes: BTreeMap<String, u64> = BTreeMap::new();
    for point in calibration["points"]
        .as_array()
        .ok_or("the overhead receipt has no points")?
    {
        point_bytes.insert(
            point["id"].as_str().ok_or("point id")?.to_owned(),
            point["per_tensor_bytes"]
                .as_u64()
                .ok_or("per_tensor_bytes")?,
        );
    }
    let mut worst_ppm = 0i64;
    let mut in_regime = 0u64;
    for row in calibration["fit"]["residuals"]
        .as_array()
        .ok_or("the overhead receipt has no residuals")?
    {
        let id = row["id"].as_str().ok_or("residual id")?;
        let bytes = *point_bytes.get(id).ok_or("residual without a point")?;
        if bytes < 1_048_576 {
            continue;
        }
        in_regime += 1;
        worst_ppm = worst_ppm.max(row["error_ppm"].as_i64().ok_or("error_ppm")?.abs());
    }
    if in_regime < 5 {
        return Err(format!(
            "only {in_regime} in-regime fit points; the margin is not identifiable"
        ));
    }
    let fit_allowance =
        (u128::from(worst_ppm as u64) * u128::from(per_mib_ns) + 999_999) / 1_000_000;

    let optimizer = court["realizations"]
        .as_array()
        .ok_or("the floor court receipt has no realizations")?
        .iter()
        .find(|row| row["label"] == "optimizer")
        .ok_or("the floor court receipt has no all-independent realization")?;
    let predicted = optimizer["predicted_restore_worst_ns"]
        .as_u64()
        .ok_or("predicted_restore_worst_ns")?;
    let measured = (optimizer["restore_worst_ms"]
        .as_f64()
        .ok_or("restore_worst_ms")?
        * 1_000_000.0)
        .round() as u64;
    if measured <= predicted {
        return Err("the floor court measured the floor at or below its prediction; no cold allowance is identifiable".into());
    }
    let logical_bytes = court["graph"]["versions"][0]["logical_bytes"]
        .as_u64()
        .ok_or("logical_bytes")?;
    let cold_deficit = measured - predicted;
    let cold_allowance = (u128::from(cold_deficit) * 1_048_576 + u128::from(logical_bytes) - 1)
        / u128::from(logical_bytes);

    let ns_per_mib = u64::try_from(fit_allowance + cold_allowance)
        .map_err(|_| "the derived margin does not fit u64")?;
    Ok(SlaMargin {
        ns_per_mib,
        fit_allowance_ns_per_mib: u64::try_from(fit_allowance).unwrap_or(u64::MAX),
        cold_allowance_ns_per_mib: u64::try_from(cold_allowance).unwrap_or(u64::MAX),
        worst_in_regime_residual_ppm: worst_ppm,
        in_regime_points: in_regime,
        excluded_below_bytes: 1_048_576,
        cold_deficit_ns: cold_deficit,
        logical_bytes,
        predicted_restore_ns: predicted,
        measured_restore_ns: measured,
    })
}

fn margin_json(margin: &SlaMargin, baked: u64, overhead: &Path, floor: &Path) -> Json {
    json!({
        "calibration": weftfs::cost_model::SLA_MARGIN_CALIBRATION,
        "baked_ns_per_mib": baked,
        "derived_ns_per_mib": margin.ns_per_mib,
        "matches_baked": margin.ns_per_mib == baked,
        "derivation": {
            "fit_allowance_ns_per_mib": margin.fit_allowance_ns_per_mib,
            "cold_allowance_ns_per_mib": margin.cold_allowance_ns_per_mib,
            "worst_in_regime_fit_residual_ppm": margin.worst_in_regime_residual_ppm,
            "in_regime_points": margin.in_regime_points,
            "excluded_below_bytes": margin.excluded_below_bytes,
            "fit_formula": "ceil(worst_in_regime_fit_residual_ppm * fit.per_mib_ns / 1e6)",
            "cold_formula": "ceil((measured_worst - predicted_worst) * 1 MiB / logical_bytes)",
            "cold_deficit_ns": margin.cold_deficit_ns,
            "logical_bytes": margin.logical_bytes,
            "predicted_restore_ns": margin.predicted_restore_ns,
            "measured_restore_ns": margin.measured_restore_ns,
        },
        "inputs": {
            "overhead_receipt": overhead.display().to_string(),
            "floor_court_receipt": floor.display().to_string(),
        },
        "note": "bounds a measured single-host, warm-page-cache environment; it is not a cold-media, different-backend, or fleet promise",
    })
}

struct HopCalibration {
    json: Json,
    matches_baked: bool,
}

/// Recompute each delta hop row from a fresh controlled-chain run and compare
/// with the baked table. The gate is repeatability (10 %), not exact equality:
/// the baked row is the union of the two cited runs, and a third run shifts the
/// sample set. A real drift fails; measurement noise does not.
fn read_hop_calibration(path: &Path) -> Result<HopCalibration, String> {
    let receipt: Json = read_receipt(path)?;
    let mut rows = Vec::new();
    let mut matches_baked = true;
    for row in receipt["hop_fit"]
        .as_array()
        .ok_or("the hop receipt has no hop_fit")?
    {
        let strategy = row["strategy"].as_str().ok_or("strategy")?;
        let Some(class) = weftfs::cost_model::CostClass::parse(strategy) else {
            continue;
        };
        let baked_row = weftfs::cost_model::hop_coefficients(class);
        let samples = row["samples"].as_u64().unwrap_or(0);
        let mut numerator: u128 = 0;
        let mut denominator: u128 = 0;
        for hop in row["measured_hops"].as_array().ok_or("measured_hops")? {
            let bytes = hop["element_bytes"].as_u64().ok_or("element_bytes")?;
            let ns = hop["hop_ns"].as_u64().ok_or("hop_ns")?;
            numerator += u128::from(bytes) * u128::from(ns);
            denominator += u128::from(bytes) * u128::from(bytes);
        }
        if denominator == 0 {
            return Err("hop samples have no bytes".into());
        }
        let scaled = numerator * 1_048_576;
        let (quotient, remainder) = (scaled / denominator, scaled % denominator);
        let derived = u64::try_from(quotient + u128::from(2 * remainder >= denominator))
            .map_err(|_| "hop row does not fit u64")?;
        let error_ppm = if baked_row.ns_per_mib == 0 {
            i64::MAX
        } else {
            (((derived as f64 - baked_row.ns_per_mib as f64) / baked_row.ns_per_mib as f64)
                * 1_000_000.0)
                .round() as i64
        };
        // Only the delta classes have a hop override to compare against.
        if class == weftfs::cost_model::CostClass::SparseDelta
            || class == weftfs::cost_model::CostClass::WeightDelta
        {
            if error_ppm.abs() > 100_000 || samples < 5 {
                matches_baked = false;
            }
            rows.push(json!({
                "class": class.as_str(),
                "samples": samples,
                "sizes_bytes": row["sizes_bytes"],
                "baked_ns_per_mib": baked_row.ns_per_mib,
                "remeasured_ns_per_mib": derived,
                "remeasured_error_ppm": error_ppm,
                "repeatability_gate_ppm": 100_000,
            }));
        }
    }
    Ok(HopCalibration {
        json: json!({
            "calibration": weftfs::cost_model::CHAIN_HOP_CALIBRATION,
            "receipt": path.display().to_string(),
            "rows": rows,
            "matches_baked_within_repeatability_gate": matches_baked,
        }),
        matches_baked,
    })
}

fn read_receipt(path: &Path) -> Result<Json, String> {
    let text = fs::read_to_string(path).map_err(|err| {
        format!(
            "{}: {err}; the measurement receipt must exist",
            path.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
}

fn chain_plan(n: usize) -> Vec<Option<usize>> {
    (0..n)
        .map(|i| if i == 0 { None } else { Some(i - 1) })
        .collect()
}

fn restart_plan(n: usize, k: usize) -> Vec<Option<usize>> {
    (0..n)
        .map(|i| if i % k == 0 { None } else { Some(i - 1) })
        .collect()
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn data_bytes(store: &Path) -> u64 {
    dir_bytes(&store.join("objects")) + dir_bytes(&store.join("manifests"))
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|item| item == name)
}
