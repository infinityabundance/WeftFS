//! F7 AI-corpus cost graph and latency harness.
//!
//! Reads `bench/corpora/ai-v1/MANIFEST.json`, verifies that every file matches
//! its declared length, then measures four storage strategies over the same
//! bytes:
//!
//! 1. `zstd_forced` — WeftCodec mode ZSTD for every object (content-only).
//! 2. `cdc_zstd` — content-defined chunking + global dedup + zstd chunks.
//! 3. `weft_auto` — WeftCodec `auto` with RAW/ZSTD plus typed TENSOR/TOKENS
//!    candidates, no relation-derived bases.
//! 4. `weft_context` — the same candidates plus DELTA/DICTIONARY against bases
//!    named by declared typed relations.
//!
//! Every decode is compared byte-for-byte with the expected plaintext before a
//! latency sample is accepted. The receipt contains raw samples, per-file
//! accounting, losses, and a cost graph (visible bytes, physical bytes, read
//! latency, dependency depth).
//!
//! Run inside the bench court:
//!
//! ```sh
//! scripts/docker-court.sh bench "bash bench/scripts/run_ai_corpus_bench.sh"
//! ```
//!
//! `--only cdc` runs the CDC strategy alone (the default full run is
//! unchanged), and `WEFT_CDC_AVG_KIB` sets its average chunk size (default 8).
//! `--cdc-sweep <kib,kib,...>` measures CDC at several average sizes in one
//! court run and writes a sweep receipt.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Deserialize;
use serde_json::{json, Value as Json};

use weftcodec::dependency::{decode_frame_dag, FrameSource, MapResolver};
use weftcodec::frame::{Frame, Mode};
use weftcodec::hash::{hash_domain, Hash32, DOMAIN_CHUNK};
use weftcodec::limits::Limits;
use weftcodec::modes::{self, TensorDtype};

#[derive(Debug, Deserialize)]
struct Manifest {
    schema: String,
    corpus: String,
    license: String,
    provenance: String,
    generator: String,
    seed: String,
    files: Vec<Entry>,
    totals: Totals,
}

#[derive(Debug, Deserialize)]
struct Totals {
    files: usize,
    source_bytes: u64,
    visible_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct Entry {
    path: String,
    kind: String,
    source_bytes: u64,
    visible_bytes: u64,
    sha256: String,
    #[serde(default)]
    relation: Option<Relation>,
    #[serde(default)]
    tensor: Option<TensorMeta>,
    #[serde(default)]
    tokens: Option<TokenMeta>,
}

#[derive(Debug, Clone, Deserialize)]
struct Relation {
    predicate: String,
    target: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TensorMeta {
    dtype: String,
    shape: Vec<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct TokenMeta {
    vocab_schema: String,
    token_count: u64,
    payload_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct TokenInput {
    vocab_schema: String,
    token_ids: Vec<u64>,
}

enum Kind {
    Plain,
    Tensor(TensorDtype, Vec<u64>),
    Tokens(Vec<u64>, String),
}

struct Args {
    corpus: PathBuf,
    out: PathBuf,
    csv: PathBuf,
    graph: PathBuf,
    iterations: usize,
    only: String,
    cdc_params: CdcParams,
    cdc_sweep: Option<Vec<f64>>,
}

fn main() {
    let args = parse_args();
    if let Err(err) = run(&args) {
        eprintln!("bench_ai_corpus: {err}");
        std::process::exit(1);
    }
}

fn parse_args() -> Args {
    let mut corpus = PathBuf::from("bench/corpora/ai-v1");
    let mut out = PathBuf::from("bench/receipts/ai-corpus-receipt.json");
    let mut csv = PathBuf::from("bench/receipts/ai-cost-graph.csv");
    let mut graph = PathBuf::from("bench/receipts/ai-cost-graph.json");
    let mut iterations = 7usize;
    let mut only = "all".to_owned();
    let mut cdc_sweep: Option<Vec<f64>> = None;
    let argv: Vec<String> = env::args().skip(1).collect();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--corpus" => {
                index += 1;
                corpus = PathBuf::from(&argv[index]);
            }
            "--out" => {
                index += 1;
                out = PathBuf::from(&argv[index]);
            }
            "--csv" => {
                index += 1;
                csv = PathBuf::from(&argv[index]);
            }
            "--graph" => {
                index += 1;
                graph = PathBuf::from(&argv[index]);
            }
            "--iterations" => {
                index += 1;
                iterations = argv[index].parse().unwrap_or(7).max(1);
            }
            "--only" => {
                index += 1;
                if index >= argv.len() {
                    eprintln!("--only needs a value: all|cdc|zstd");
                    std::process::exit(64);
                }
                only = argv[index].clone();
            }
            "--cdc-sweep" => {
                index += 1;
                if index >= argv.len() {
                    eprintln!("--cdc-sweep needs a comma-separated list of average sizes in KiB");
                    std::process::exit(64);
                }
                cdc_sweep = Some(parse_kib_list(&argv[index]));
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(64);
            }
        }
        index += 1;
    }
    if !matches!(only.as_str(), "all" | "cdc" | "zstd") {
        eprintln!("--only expects all|cdc|zstd, got {only:?}");
        std::process::exit(64);
    }
    let avg_kib = match env::var("WEFT_CDC_AVG_KIB") {
        Ok(raw) => match raw.trim().parse::<f64>() {
            Ok(value) => value,
            Err(_) => {
                eprintln!("WEFT_CDC_AVG_KIB={raw:?} is not a number of KiB");
                std::process::exit(64);
            }
        },
        Err(env::VarError::NotPresent) => 8.0,
        Err(err) => {
            eprintln!("WEFT_CDC_AVG_KIB: {err}");
            std::process::exit(64);
        }
    };
    let cdc_params = match CdcParams::from_avg_kib(avg_kib) {
        Ok(params) => params,
        Err(err) => {
            eprintln!("WEFT_CDC_AVG_KIB={avg_kib}: {err}");
            std::process::exit(64);
        }
    };
    Args {
        corpus,
        out,
        csv,
        graph,
        iterations,
        only,
        cdc_params,
        cdc_sweep,
    }
}

fn parse_kib_list(raw: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.parse::<f64>() {
            Ok(value) => out.push(value),
            Err(_) => {
                eprintln!("--cdc-sweep: {part:?} is not a number of KiB");
                std::process::exit(64);
            }
        }
    }
    if out.is_empty() {
        eprintln!("--cdc-sweep needs at least one average chunk size");
        std::process::exit(64);
    }
    out
}

fn run(args: &Args) -> Result<(), String> {
    let limits = Limits::default();
    let manifest_path = args.corpus.join("MANIFEST.json");
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| format!("bad manifest JSON: {e}"))?;

    let mut entries = manifest.files.clone();
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let mut data: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for entry in &entries {
        let path = args.corpus.join(Path::new(&entry.path));
        let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if bytes.len() as u64 != entry.source_bytes {
            return Err(format!(
                "{}: {} bytes but manifest declares {}",
                entry.path,
                bytes.len(),
                entry.source_bytes
            ));
        }
        if let Some(got) = sha256_file(&path) {
            if got != entry.sha256 {
                return Err(format!(
                    "{}: sha256 {got} != manifest {}",
                    entry.path, entry.sha256
                ));
            }
        }
        data.insert(entry.path.clone(), bytes);
    }

    let mut kinds: HashMap<String, Kind> = HashMap::new();
    let mut expected: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in &entries {
        let bytes = &data[&entry.path];
        match (&entry.tensor, &entry.tokens) {
            (Some(tensor), _) => {
                let dtype = TensorDtype::from_name(&tensor.dtype)
                    .map_err(|e| format!("{}: {e}", entry.path))?;
                kinds.insert(
                    entry.path.clone(),
                    Kind::Tensor(dtype, tensor.shape.clone()),
                );
                expected.insert(entry.path.clone(), bytes.clone());
            }
            (_, Some(tokens)) => {
                let input: TokenInput = serde_json::from_slice(bytes)
                    .map_err(|e| format!("{}: bad token JSON: {e}", entry.path))?;
                if input.token_ids.len() as u64 != tokens.token_count {
                    return Err(format!(
                        "{}: token count {} but manifest declares {}",
                        entry.path,
                        input.token_ids.len(),
                        tokens.token_count
                    ));
                }
                if input.vocab_schema != tokens.vocab_schema {
                    return Err(format!(
                        "{}: vocab schema {:?} but manifest declares {:?}",
                        entry.path, input.vocab_schema, tokens.vocab_schema
                    ));
                }
                let payload = modes::encode_token_payload(&input.token_ids, &limits)
                    .map_err(|e| format!("{}: {e}", entry.path))?;
                if payload.len() as u64 != tokens.payload_bytes {
                    return Err(format!(
                        "{}: canonical payload {} bytes but manifest declares {}",
                        entry.path,
                        payload.len(),
                        tokens.payload_bytes
                    ));
                }
                kinds.insert(
                    entry.path.clone(),
                    Kind::Tokens(input.token_ids, input.vocab_schema),
                );
                expected.insert(entry.path.clone(), payload);
            }
            _ => {
                kinds.insert(entry.path.clone(), Kind::Plain);
                expected.insert(entry.path.clone(), bytes.clone());
            }
        }
    }

    let manifest_sha = sha256_file(&manifest_path).unwrap_or_else(|| "unavailable".to_owned());
    let corpus_meta = json!({
        "path": args.corpus.display().to_string(),
        "schema": &manifest.schema,
        "name": &manifest.corpus,
        "license": &manifest.license,
        "provenance": &manifest.provenance,
        "generator": &manifest.generator,
        "seed": &manifest.seed,
        "manifest_sha256": manifest_sha,
        "manifest_bytes": manifest_bytes.len(),
        "totals": {
            "files": manifest.totals.files,
            "source_bytes": manifest.totals.source_bytes,
            "visible_bytes": manifest.totals.visible_bytes,
        },
        "verified_lengths": true,
    });

    // Sweep mode measures CDC at several average chunk sizes and needs no
    // WeftCodec planner work at all.
    if let Some(sweep) = &args.cdc_sweep {
        return run_cdc_sweep(args, &entries, &expected, &corpus_meta, sweep);
    }

    // A resolver containing every plaintext lets DELTA/DICTIONARY frames
    // decode with their dependency already materialized (the warm path).
    let mut plaintext_resolver = MapResolver::new();
    for bytes in data.values() {
        plaintext_resolver.insert_bytes(bytes.clone());
    }

    let run_weft = args.only == "all";
    let run_zstd = args.only == "all" || args.only == "zstd";
    let run_cdc = args.only == "all" || args.only == "cdc";

    let level = 3;
    let mut auto_plans: Vec<Plan> = Vec::new();
    let mut context_plans: Vec<Plan> = Vec::new();
    let mut depths: HashMap<String, usize> = HashMap::new();
    let mut planner_policy = Json::Null;
    let mut planner_rejections = Json::Null;
    if run_weft {
        auto_plans = plan_weft(&entries, &expected, &kinds, false, level, &limits)?;
        let context_planning = plan_weft_context(&entries, &expected, &kinds, level, &limits)?;
        planner_policy = context_planning.policy;
        planner_rejections = Json::Array(context_planning.rejections);
        context_plans = context_planning.plans;
        depths = compute_depths(&entries);
    }

    let zstd = if run_zstd {
        Some(measure_zstd_forced(
            &entries,
            &expected,
            &plaintext_resolver,
            level,
            &limits,
            args.iterations,
        )?)
    } else {
        None
    };
    let auto = if run_weft {
        Some(measure_weft(
            "weft_auto",
            &entries,
            &auto_plans,
            &expected,
            &depths,
            false,
            &plaintext_resolver,
            &limits,
            args.iterations,
        )?)
    } else {
        None
    };
    let context = if run_weft {
        Some(measure_weft(
            "weft_context",
            &entries,
            &context_plans,
            &expected,
            &depths,
            true,
            &plaintext_resolver,
            &limits,
            args.iterations,
        )?)
    } else {
        None
    };
    let cdc = if run_cdc {
        Some(measure_cdc(
            &entries,
            &expected,
            args.iterations,
            &args.cdc_params,
        )?)
    } else {
        None
    };
    let typed_candidates = if run_weft {
        typed_mode_report(&entries, &expected, &kinds, &auto_plans, &limits)?
    } else {
        Json::Null
    };

    let mut cost_rows: Vec<Json> = Vec::new();
    for strategy in [zstd.as_ref(), auto.as_ref(), context.as_ref()]
        .into_iter()
        .flatten()
    {
        for row in &strategy.records {
            cost_rows.push(row.clone());
        }
        cost_rows.push(strategy.aggregate.clone());
    }
    if let Some(cdc) = &cdc {
        for row in &cdc.records {
            cost_rows.push(row.clone());
        }
        cost_rows.push(cdc.aggregate.clone());
    }

    let mut strategies = serde_json::Map::new();
    if let Some(strategy) = &zstd {
        strategies.insert("zstd_forced".to_owned(), strategy.receipt.clone());
    }
    if let Some(strategy) = &cdc {
        strategies.insert("cdc_zstd".to_owned(), strategy.receipt.clone());
    }
    if let Some(strategy) = &auto {
        strategies.insert("weft_auto".to_owned(), strategy.receipt.clone());
    }
    if let Some(strategy) = &context {
        strategies.insert("weft_context".to_owned(), strategy.receipt.clone());
    }

    let receipt = json!({
        "kind": "ai-corpus-cost",
        "schema": "weft.ai-cost.v1",
        "timestamp_utc": utc_stamp(),
        "run_mode": args.only,
        "cdc_parameters": args.cdc_params.to_json(),
        "environment": environment(),
        "revisions": court_revisions(),
        "corpus": corpus_meta,
        "iterations": args.iterations,
        "typed_mode_candidates": typed_candidates,
        "planner_policy": planner_policy,
        "planner_rejections": planner_rejections,
        "strategies": strategies,
        "cost_graph": {
            "axes": ["visible_bytes", "physical_bytes", "read_latency_ns", "dependency_depth"],
            "rows": cost_rows,
        },
        "notes": [
            "All sizes are bytes. `visible_bytes` is the manifest's declared visible representation; for TENSOR it is the element payload and for TOKENS it is the canonical delta-LEB128 payload, while `source_bytes` is the JSON/text file.",
            "`physical_bytes` counts every stored frame/chunk/index/manifest once for the whole corpus; `pair_bytes` sums marginal + base plaintext per dependent object and therefore may double-count shared bases. Pair losses are reported raw and are not hidden.",
            "`weft_context` uses a whole-pair-aware planner: a DELTA/DICTIONARY/REFERENCE candidate is selected only when the base is shared (>=2 direct dependents) or retained (a declared chain root), or when marginal frame + base plaintext is strictly smaller than the best context-free frame. `planner_cost` records the objective value; `planner_rejections` records every contextual candidate that lost for cost reasons.",
            "`read_latency_ns` is the median of verified decodes; `historical_read_ns` uses the cold dependency-DAG path when the object has a relation chain, otherwise the warm path.",
            "CDC is content-only and relation-agnostic. WeftCodec context bases come only from declared typed relations in the manifest. `cdc_parameters` records the average/min/max chunk sizes and the gear mask actually used.",
            "No winner or superiority claim is made. See bench/RESULTS-ai-v1.md for the analysis and every loss.",
        ],
    });

    write_json(&args.out, &receipt)?;
    write_json(
        &args.graph,
        &json!({
            "schema": "weft.ai-cost-graph.v1",
            "axes": ["visible_bytes", "physical_bytes", "read_latency_ns", "dependency_depth"],
            "rows": cost_rows,
        }),
    )?;
    write_csv(&args.csv, &cost_rows)?;
    println!(
        "wrote {} , {} , {}",
        args.out.display(),
        args.graph.display(),
        args.csv.display()
    );
    Ok(())
}

fn court_revisions() -> Json {
    json!({
        "WeftFS": env::var("WEFT_REPO_WEFTFS").ok(),
        "WeftCodec": env::var("WEFT_REPO_WEFTCODEC").ok(),
        "WeftCodec-Go": env::var("WEFT_REPO_WEFTCODEC_GO").ok(),
        "WeftFS-Go": env::var("WEFT_REPO_WEFTFS_GO").ok(),
        "court_image": env::var("WEFT_COURT_IMAGE").ok(),
        "court_image_id": env::var("WEFT_COURT_IMAGE_ID").ok(),
        "court_memory": env::var("WEFT_COURT_MEMORY").ok(),
        "court_cpus": env::var("WEFT_COURT_CPUS").ok(),
        "court_pids": env::var("WEFT_COURT_PIDS").ok(),
        "kernel": env::var("WEFT_COURT_KERNEL").ok(),
    })
}

struct Plan {
    entry: usize,
    mode: &'static str,
    mode_value: u8,
    frame: Vec<u8>,
    base_path: Option<String>,
    base_amortized: bool,
    planner_cost: u64,
    context_free_frame_bytes: u64,
}

struct Planning {
    plans: Vec<Plan>,
    rejections: Vec<Json>,
    policy: Json,
}

type Candidate = (&'static str, u8, Vec<u8>, Option<String>);

fn context_free_candidates(
    entry: &Entry,
    bytes: &[u8],
    kinds: &HashMap<String, Kind>,
    level: i32,
    limits: &Limits,
) -> Result<Vec<Candidate>, String> {
    let mut candidates: Vec<Candidate> = Vec::new();
    candidates.push((
        "raw",
        Mode::Raw.as_u8(),
        modes::encode_raw(bytes, None, limits).map_err(|e| format!("{}: {e}", entry.path))?,
        None,
    ));
    candidates.push((
        "zstd",
        Mode::Zstd.as_u8(),
        modes::encode_zstd(bytes, level, None, limits)
            .map_err(|e| format!("{}: {e}", entry.path))?,
        None,
    ));
    match kinds.get(&entry.path) {
        Some(Kind::Tensor(dtype, shape)) => {
            candidates.push((
                "tensor",
                Mode::Tensor.as_u8(),
                modes::encode_tensor(bytes, *dtype, shape, limits)
                    .map_err(|e| format!("{}: {e}", entry.path))?,
                None,
            ));
        }
        Some(Kind::Tokens(ids, vocab)) => {
            candidates.push((
                "tokens",
                Mode::TokenSequence.as_u8(),
                modes::encode_token_sequence(ids, vocab, limits)
                    .map_err(|e| format!("{}: {e}", entry.path))?,
                None,
            ));
        }
        _ => {}
    }
    Ok(candidates)
}

fn plan_weft(
    entries: &[Entry],
    expected: &HashMap<String, Vec<u8>>,
    kinds: &HashMap<String, Kind>,
    _use_context: bool,
    level: i32,
    limits: &Limits,
) -> Result<Vec<Plan>, String> {
    let mut plans = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let bytes = &expected[&entry.path];
        let candidates = context_free_candidates(entry, bytes, kinds, level, limits)?;
        let chosen = candidates
            .into_iter()
            .min_by(|a, b| a.2.len().cmp(&b.2.len()).then(a.1.cmp(&b.1)))
            .ok_or_else(|| format!("{}: no candidate", entry.path))?;
        let frame_len = chosen.2.len() as u64;
        plans.push(Plan {
            entry: index,
            mode: chosen.0,
            mode_value: chosen.1,
            frame: chosen.2,
            base_path: chosen.3,
            base_amortized: false,
            planner_cost: frame_len,
            context_free_frame_bytes: frame_len,
        });
    }
    Ok(plans)
}

/// Whole-pair-aware context planner.
///
/// A contextual candidate is selected only when its base cost is amortized
/// (the base is shared by >=2 direct dependents, or is a retained chain root)
/// or when `marginal frame + base plaintext` is strictly smaller than the best
/// context-free frame. Raw pair cost is still reported for every selected
/// object, including the remaining losses.
fn plan_weft_context(
    entries: &[Entry],
    expected: &HashMap<String, Vec<u8>>,
    kinds: &HashMap<String, Kind>,
    level: i32,
    limits: &Limits,
) -> Result<Planning, String> {
    let mut dependent_counts: HashMap<&str, usize> = HashMap::new();
    for entry in entries {
        if let Some(relation) = &entry.relation {
            *dependent_counts
                .entry(relation.target.as_str())
                .or_insert(0) += 1;
        }
    }
    let retained_roots: BTreeMap<&str, ()> = entries
        .iter()
        .filter(|entry| entry.relation.is_none())
        .map(|entry| (entry.path.as_str(), ()))
        .collect();
    let shared_bases: BTreeMap<&str, ()> = dependent_counts
        .iter()
        .filter(|(_, count)| **count >= 2)
        .map(|(path, _)| (*path, ()))
        .collect();

    let mut plans = Vec::with_capacity(entries.len());
    let mut rejections: Vec<Json> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let bytes = &expected[&entry.path];
        let free_candidates = context_free_candidates(entry, bytes, kinds, level, limits)?;
        let context_free = free_candidates
            .into_iter()
            .min_by(|a, b| a.2.len().cmp(&b.2.len()).then(a.1.cmp(&b.1)))
            .ok_or_else(|| format!("{}: no context-free candidate", entry.path))?;
        let context_free_len = context_free.2.len() as u64;

        let mut chosen = Plan {
            entry: index,
            mode: context_free.0,
            mode_value: context_free.1,
            frame: context_free.2,
            base_path: None,
            base_amortized: false,
            planner_cost: context_free_len,
            context_free_frame_bytes: context_free_len,
        };

        if let Some(relation) = &entry.relation {
            if let Some(base) = expected.get(&relation.target) {
                let base_bytes = base.len() as u64;
                let base_amortized = retained_roots.contains_key(relation.target.as_str())
                    || shared_bases.contains_key(relation.target.as_str());
                let mut contextual: Vec<Candidate> = Vec::new();
                contextual.push((
                    "delta",
                    Mode::Delta.as_u8(),
                    modes::encode_delta(bytes, base, limits)
                        .map_err(|e| format!("{}: {e}", entry.path))?,
                    Some(relation.target.clone()),
                ));
                if base == bytes {
                    contextual.push((
                        "reference",
                        Mode::Reference.as_u8(),
                        modes::encode_reference(bytes, base, limits)
                            .map_err(|e| format!("{}: {e}", entry.path))?,
                        Some(relation.target.clone()),
                    ));
                }
                contextual.push((
                    "dictionary",
                    Mode::Dictionary.as_u8(),
                    modes::encode_dictionary(bytes, base, level, limits)
                        .map_err(|e| format!("{}: {e}", entry.path))?,
                    Some(relation.target.clone()),
                ));

                let mut best: Option<(u64, Candidate)> = None;
                for candidate in contextual {
                    let frame_bytes = candidate.2.len() as u64;
                    let planner_cost = frame_bytes + if base_amortized { 0 } else { base_bytes };
                    let acceptable = planner_cost < context_free_len;
                    if acceptable {
                        let replace = match &best {
                            None => true,
                            Some((cost, current)) => {
                                planner_cost < *cost
                                    || (planner_cost == *cost
                                        && (frame_bytes, candidate.1)
                                            < (current.2.len() as u64, current.1))
                            }
                        };
                        if replace {
                            best = Some((planner_cost, candidate));
                        }
                    } else {
                        rejections.push(json!({
                            "path": entry.path,
                            "mode": candidate.0,
                            "frame_bytes": frame_bytes,
                            "base_path": relation.target,
                            "base_bytes": base_bytes,
                            "base_amortized": base_amortized,
                            "planner_cost": planner_cost,
                            "context_free_frame_bytes": context_free_len,
                            "reason": if base_amortized {
                                "marginal frame is not smaller than the best context-free frame"
                            } else {
                                "marginal frame + unamortized base is not smaller than the best context-free frame"
                            },
                        }));
                    }
                }
                if let Some((planner_cost, candidate)) = best {
                    chosen = Plan {
                        entry: index,
                        mode: candidate.0,
                        mode_value: candidate.1,
                        frame: candidate.2,
                        base_path: candidate.3,
                        base_amortized,
                        planner_cost,
                        context_free_frame_bytes: context_free_len,
                    };
                }
            }
        }
        plans.push(chosen);
    }

    let policy = json!({
        "objective": "marginal_frame_bytes + (base_bytes unless the base is amortized)",
        "amortized_when": "base is a retained chain root (no declared relation) or has >=2 direct dependents",
        "accept_when": "planner_cost < best context-free frame bytes",
        "retained_chain_roots": retained_roots.keys().collect::<Vec<_>>(),
        "shared_bases": shared_bases.keys().collect::<Vec<_>>(),
        "direct_dependents": dependent_counts,
        "contextual_objects": plans.iter().filter(|plan| plan.base_path.is_some()).count(),
        "context_free_objects": plans.iter().filter(|plan| plan.base_path.is_none()).count(),
    });
    Ok(Planning {
        plans,
        rejections,
        policy,
    })
}

struct StrategyOutput {
    records: Vec<Json>,
    aggregate: Json,
    receipt: Json,
}

fn measure_zstd_forced(
    entries: &[Entry],
    expected: &HashMap<String, Vec<u8>>,
    resolver: &MapResolver,
    level: i32,
    limits: &Limits,
    iterations: usize,
) -> Result<StrategyOutput, String> {
    // zstd_forced must use mode ZSTD even when RAW would be smaller.
    let mut records = Vec::with_capacity(entries.len());
    let mut frames_total = 0u64;
    let mut index_bytes = 0u64;
    let mut losses = Vec::new();
    let mut latencies = Vec::new();
    for entry in entries {
        let source = &expected[&entry.path];
        let frame = modes::encode_zstd(source, level, None, limits)
            .map_err(|e| format!("{}: {e}", entry.path))?;
        let decoded = modes::decode_bytes(&frame, resolver, limits)
            .map_err(|e| format!("{}: zstd decode: {e}", entry.path))?;
        if &decoded != source {
            return Err(format!("{}: zstd decoded bytes differ", entry.path));
        }
        let mut samples = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            let out = modes::decode_bytes(&frame, resolver, limits)
                .map_err(|e| format!("{}: {e}", entry.path))?;
            samples.push(start.elapsed().as_nanos());
            if &out != source {
                return Err(format!("{}: zstd decode bytes differ", entry.path));
            }
        }
        let median = median_ns(&mut samples);
        latencies.push(median);
        frames_total += frame.len() as u64;
        index_bytes += file_index_bytes(entry);
        if frame.len() as u64 > entry.visible_bytes {
            losses.push(json!({
                "path": entry.path,
                "kind": entry.kind,
                "frame_bytes": frame.len(),
                "visible_bytes": entry.visible_bytes,
                "reason": "zstd frame exceeds the visible representation",
            }));
        }
        records.push(json!({
            "strategy": "zstd_forced",
            "path": entry.path,
            "kind": entry.kind,
            "visible_bytes": entry.visible_bytes,
            "source_bytes": entry.source_bytes,
            "mode": "zstd",
            "frame_bytes": frame.len(),
            "base_bytes": 0,
            "pair_bytes": frame.len(),
            "physical_contribution_bytes": frame.len(),
            "manifest_bytes": file_index_bytes(entry),
            "read_latency_ns": median,
            "dependency_depth": 0,
        }));
    }
    let physical = frames_total + index_bytes;
    let aggregate = aggregate_row(
        "zstd_forced",
        entries.len(),
        &records,
        physical,
        frames_total,
        0,
        0,
        &latencies,
    );
    Ok(StrategyOutput {
        aggregate: aggregate.clone(),
        receipt: json!({
            "strategy": "zstd_forced",
            "description": "WeftCodec mode ZSTD for every object; content-only baseline.",
            "frame_bytes": frames_total,
            "index_bytes": index_bytes,
            "physical_bytes": physical,
            "records": &records,
            "aggregate": aggregate,
            "losses": losses,
        }),
        records,
    })
}

fn measure_weft(
    name: &'static str,
    entries: &[Entry],
    plans: &[Plan],
    expected: &HashMap<String, Vec<u8>>,
    depths: &HashMap<String, usize>,
    include_relations: bool,
    resolver: &MapResolver,
    limits: &Limits,
    iterations: usize,
) -> Result<StrategyOutput, String> {
    let frame_depths = plan_depths(entries, plans);
    let mut frames_by_hash: HashMap<Hash32, Vec<u8>> = HashMap::new();
    for plan in plans {
        let frame = Frame::parse(&plan.frame, limits)
            .map_err(|e| format!("{}: parse: {e}", entries[plan.entry].path))?;
        frames_by_hash
            .entry(frame.header.content_hash)
            .or_insert_with(|| plan.frame.clone());
    }
    let source = MapFrames {
        frames: frames_by_hash,
    };

    let mut records = Vec::with_capacity(entries.len());
    let mut frames_total = 0u64;
    let mut index_bytes = 0u64;
    let mut relation_bytes = 0u64;
    let mut base_total = 0u64;
    let mut pair_total = 0u64;
    let mut losses = Vec::new();
    let mut pair_losses = Vec::new();
    let mut planner_violations = Vec::new();
    let mut latencies = Vec::new();

    for plan in plans {
        let entry = &entries[plan.entry];
        let want = &expected[&entry.path];
        // Warm decode (dependency plaintext available).
        let warm = modes::decode_bytes(&plan.frame, resolver, limits)
            .map_err(|e| format!("{}: warm decode: {e}", entry.path))?;
        if &warm != want {
            return Err(format!("{}: warm decode bytes differ", entry.path));
        }
        let mut warm_samples = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            let out = modes::decode_bytes(&plan.frame, resolver, limits)
                .map_err(|e| format!("{}: {e}", entry.path))?;
            warm_samples.push(start.elapsed().as_nanos());
            if &out != want {
                return Err(format!("{}: warm decode bytes differ", entry.path));
            }
        }
        let warm_median = median_ns(&mut warm_samples);

        // Cold dependency-DAG decode: every frame is resolved from frames,
        // recursively, with no plaintext cache.
        let cold = decode_frame_dag(&source, &plan.frame, limits)
            .map_err(|e| format!("{}: cold decode: {e}", entry.path))?;
        if &cold != want {
            return Err(format!("{}: cold decode bytes differ", entry.path));
        }
        let mut cold_samples = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            let out = decode_frame_dag(&source, &plan.frame, limits)
                .map_err(|e| format!("{}: {e}", entry.path))?;
            cold_samples.push(start.elapsed().as_nanos());
            if &out != want {
                return Err(format!("{}: cold decode bytes differ", entry.path));
            }
        }
        let cold_median = median_ns(&mut cold_samples);

        let depth = *frame_depths.get(&entry.path).unwrap_or(&0);
        let relation_depth = *depths.get(&entry.path).unwrap_or(&0);
        let historical = if depth > 0 { cold_median } else { warm_median };
        latencies.push(historical);

        let base_bytes = plan
            .base_path
            .as_ref()
            .and_then(|path| entries.iter().find(|e| &e.path == path))
            .map(|e| e.visible_bytes)
            .unwrap_or(0);
        let pair = plan.frame.len() as u64 + base_bytes;
        frames_total += plan.frame.len() as u64;
        index_bytes += file_index_bytes(entry);
        base_total += base_bytes;
        pair_total += pair;
        if plan.frame.len() as u64 > entry.visible_bytes {
            losses.push(json!({
                "path": entry.path,
                "kind": entry.kind,
                "mode": plan.mode,
                "frame_bytes": plan.frame.len(),
                "visible_bytes": entry.visible_bytes,
                "reason": "frame exceeds the visible representation",
            }));
        }
        if plan.base_path.is_some() && pair > entry.visible_bytes {
            pair_losses.push(json!({
                "path": entry.path,
                "kind": entry.kind,
                "mode": plan.mode,
                "pair_bytes": pair,
                "visible_bytes": entry.visible_bytes,
                "base_amortized": plan.base_amortized,
                "planner_cost": plan.planner_cost,
                "context_free_frame_bytes": plan.context_free_frame_bytes,
                "reason": "marginal + raw base plaintext exceeds the visible representation; the base may still be retained/amortized, which is why the planner accepted it",
            }));
        }
        if plan.planner_cost >= plan.context_free_frame_bytes && plan.base_path.is_some() {
            planner_violations.push(json!({
                "path": entry.path,
                "mode": plan.mode,
                "planner_cost": plan.planner_cost,
                "context_free_frame_bytes": plan.context_free_frame_bytes,
            }));
        }
        records.push(json!({
            "strategy": name,
            "path": entry.path,
            "kind": entry.kind,
            "visible_bytes": entry.visible_bytes,
            "source_bytes": entry.source_bytes,
            "mode": plan.mode,
            "mode_value": plan.mode_value,
            "frame_bytes": plan.frame.len(),
            "base_path": plan.base_path,
            "base_bytes": base_bytes,
            "base_amortized": plan.base_amortized,
            "planner_cost": plan.planner_cost,
            "context_free_frame_bytes": plan.context_free_frame_bytes,
            "pair_bytes": pair,
            "physical_contribution_bytes": plan.frame.len(),
            "manifest_bytes": file_index_bytes(entry),
            "read_latency_ns": historical,
            "warm_decode_ns": warm_median,
            "cold_chain_ns": cold_median,
            "dependency_depth": depth,
            "relation_chain_depth": relation_depth,
        }));
    }
    if include_relations {
        for entry in entries {
            if let Some(relation) = &entry.relation {
                relation_bytes +=
                    64 + relation.predicate.len() as u64 + relation.target.len() as u64 + 4;
            }
        }
    }
    let physical = frames_total + index_bytes + relation_bytes;
    let aggregate = aggregate_row(
        name,
        plans.len(),
        &records,
        physical,
        frames_total,
        base_total,
        pair_total,
        &latencies,
    );
    Ok(StrategyOutput {
        aggregate: aggregate.clone(),
        receipt: json!({
            "strategy": name,
            "frame_bytes": frames_total,
            "index_bytes": index_bytes,
            "relation_manifest_bytes": relation_bytes,
            "physical_bytes": physical,
            "base_plaintext_referenced_bytes": base_total,
            "pair_bytes_sum": pair_total,
            "records": &records,
            "aggregate": aggregate,
            "losses": losses,
            "pair_losses": pair_losses,
            "planner_violations": planner_violations,
        }),
        records,
    })
}

/// Depth of each chosen frame's own dependency DAG (0 for ZSTD/RAW/TENSOR/
/// TOKENS, 1 + base depth for DELTA/DICTIONARY/REFERENCE).
fn plan_depths(entries: &[Entry], plans: &[Plan]) -> HashMap<String, u64> {
    let by_path: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.path.as_str(), index))
        .collect();
    let mut depths: Vec<Option<u64>> = vec![None; entries.len()];
    for plan in plans {
        if plan.base_path.is_none() {
            depths[plan.entry] = Some(0);
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for plan in plans {
            if depths[plan.entry].is_some() {
                continue;
            }
            if let Some(base_path) = &plan.base_path {
                let base_depth = by_path
                    .get(base_path.as_str())
                    .and_then(|index| depths[*index]);
                if let Some(base_depth) = base_depth {
                    depths[plan.entry] = Some(base_depth + 1);
                    changed = true;
                }
            }
        }
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.path.clone(), depths[index].unwrap_or(0)))
        .collect()
}

/// Sizes of the typed TENSOR/TOKEN_SEQUENCE candidates versus the chosen
/// content-only candidate. This records that the typed modes were evaluated
/// even when they lose on space.
fn typed_mode_report(
    entries: &[Entry],
    expected: &HashMap<String, Vec<u8>>,
    kinds: &HashMap<String, Kind>,
    plans: &[Plan],
    limits: &Limits,
) -> Result<Json, String> {
    let mut rows = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let payload = &expected[&entry.path];
        let (typed_mode, typed_frame) = match kinds.get(&entry.path) {
            Some(Kind::Tensor(dtype, shape)) => (
                "tensor",
                modes::encode_tensor(payload, *dtype, shape, limits)
                    .map_err(|e| format!("{}: {e}", entry.path))?,
            ),
            Some(Kind::Tokens(ids, vocab)) => (
                "tokens",
                modes::encode_token_sequence(ids, vocab, limits)
                    .map_err(|e| format!("{}: {e}", entry.path))?,
            ),
            _ => continue,
        };
        let zstd_frame = modes::encode_zstd(payload, 3, None, limits)
            .map_err(|e| format!("{}: {e}", entry.path))?;
        rows.push(json!({
            "path": entry.path,
            "kind": entry.kind,
            "payload_bytes": payload.len(),
            "typed_mode": typed_mode,
            "typed_mode_frame_bytes": typed_frame.len(),
            "zstd_frame_bytes": zstd_frame.len(),
            "chosen_mode": plans[index].mode,
            "chosen_frame_bytes": plans[index].frame.len(),
            "typed_without_descriptor_bytes": payload.len(),
            "typed_descriptor_overhead_bytes": typed_frame.len() as u64 - payload.len() as u64,
        }));
    }
    Ok(json!(rows))
}

struct MapFrames {
    frames: HashMap<Hash32, Vec<u8>>,
}

impl FrameSource for MapFrames {
    fn get_frame(&self, content_hash: &Hash32) -> weftcodec::Result<Option<Vec<u8>>> {
        Ok(self.frames.get(content_hash).cloned())
    }
}

fn aggregate_row(
    strategy: &str,
    files: usize,
    records: &[Json],
    physical: u64,
    frames: u64,
    base: u64,
    pair: u64,
    latencies: &[u64],
) -> Json {
    let visible: u64 = records
        .iter()
        .filter_map(|r| r["visible_bytes"].as_u64())
        .sum();
    let depth_max = records
        .iter()
        .filter_map(|r| r["dependency_depth"].as_u64())
        .max()
        .unwrap_or(0);
    let depth_mean = if records.is_empty() {
        0.0
    } else {
        records
            .iter()
            .filter_map(|r| r["dependency_depth"].as_u64())
            .sum::<u64>() as f64
            / records.len() as f64
    };
    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
    let p95 = sorted
        .get(sorted.len().saturating_mul(95) / 100)
        .copied()
        .unwrap_or(median);
    json!({
        "strategy": strategy,
        "path": "__totals__",
        "kind": "aggregate",
        "files": files,
        "visible_bytes": visible,
        "physical_bytes": physical,
        "frame_bytes": frames,
        "base_plaintext_referenced_bytes": base,
        "pair_bytes": pair,
        "ratio_physical_to_visible": if visible == 0 { 0.0 } else { physical as f64 / visible as f64 },
        "read_latency_ns": median,
        "read_latency_ns_p95": p95,
        "dependency_depth": depth_max,
        "dependency_depth_mean": depth_mean,
    })
}

struct CdcResult {
    records: Vec<Json>,
    aggregate: Json,
    receipt: Json,
}

/// Gear-hash content-defined chunking parameters.
///
/// Defaults reproduce the original harness constants exactly: average 8 KiB
/// (`mask = 2^13 - 1`), min `avg/4` = 2 KiB, max `avg*4` = 32 KiB.
#[derive(Debug, Clone, Copy)]
struct CdcParams {
    avg_kib_requested: f64,
    avg_bytes_requested: u64,
    avg_bytes_effective: u64,
    mask_bits: u32,
    min_bytes: usize,
    max_bytes: usize,
}

impl CdcParams {
    fn from_avg_kib(avg_kib: f64) -> Result<Self, String> {
        if !avg_kib.is_finite() || avg_kib <= 0.0 {
            return Err(format!(
                "average chunk size {avg_kib} KiB is not a positive finite number"
            ));
        }
        let avg_bytes_requested = (avg_kib * 1024.0).round();
        if !(256.0..=16_777_216.0).contains(&avg_bytes_requested) {
            return Err(format!(
                "average chunk size {avg_kib} KiB resolves to {avg_bytes_requested} B; must be 256 B..=16 MiB"
            ));
        }
        let avg_bytes_requested = avg_bytes_requested as u64;
        // The gear mask encodes an average as a power of two. Non-power-of-two
        // requests round to the nearest power of two; `avg_bytes_effective`
        // records what was actually used.
        let mask_bits = (avg_bytes_requested as f64).log2().round().clamp(2.0, 24.0) as u32;
        let avg_bytes_effective = 1u64 << mask_bits;
        let min_bytes = usize::try_from((avg_bytes_requested / 4).max(64))
            .map_err(|_| "min chunk size does not fit usize".to_owned())?;
        let max_bytes = usize::try_from(avg_bytes_requested * 4)
            .map_err(|_| "max chunk size does not fit usize".to_owned())?;
        Ok(Self {
            avg_kib_requested: avg_kib,
            avg_bytes_requested,
            avg_bytes_effective,
            mask_bits,
            min_bytes,
            max_bytes,
        })
    }

    fn mask(&self) -> u64 {
        (1u64 << self.mask_bits) - 1
    }

    fn to_json(&self) -> Json {
        json!({
            "source": "WEFT_CDC_AVG_KIB",
            "avg_kib_requested": self.avg_kib_requested,
            "avg_bytes_requested": self.avg_bytes_requested,
            "avg_bytes_effective": self.avg_bytes_effective,
            "mask_bits": self.mask_bits,
            "mask": self.mask(),
            "min_bytes": self.min_bytes,
            "max_bytes": self.max_bytes,
            "min_rule": "max(avg/4, 64 B)",
            "max_rule": "avg*4",
        })
    }
}

fn measure_cdc(
    entries: &[Entry],
    expected: &HashMap<String, Vec<u8>>,
    iterations: usize,
    params: &CdcParams,
) -> Result<CdcResult, String> {
    let table = gear_table();
    let mut unique: HashMap<Hash32, usize> = HashMap::new();
    let mut compressed: Vec<Vec<u8>> = Vec::new();
    let mut per_file: Vec<(String, Vec<usize>, Vec<u32>)> = Vec::new();
    let mut compressed_total = 0u64;
    let mut unique_raw_total = 0u64;
    let mut total_raw = 0u64;
    let mut marginal_compressed_total = 0u64;

    for entry in entries {
        let bytes = &expected[&entry.path];
        let chunks = cdc_chunks(bytes, &table, params);
        let mut refs = Vec::with_capacity(chunks.len());
        let mut lens = Vec::with_capacity(chunks.len());
        for (start, end) in chunks {
            let raw_len = (end - start) as u64;
            total_raw += raw_len;
            let chunk = &bytes[start..end];
            let hash = hash_domain(DOMAIN_CHUNK, chunk);
            lens.push((end - start) as u32);
            let index = if let Some(index) = unique.get(&hash) {
                *index
            } else {
                let packed = zstd::bulk::compress(chunk, 3)
                    .map_err(|e| format!("{}: zstd chunk: {e}", entry.path))?;
                let index = compressed.len();
                compressed_total += packed.len() as u64;
                unique_raw_total += raw_len;
                compressed.push(packed);
                unique.insert(hash, index);
                index
            };
            refs.push(index);
        }
        per_file.push((entry.path.clone(), refs, lens));
    }
    let unique_count = compressed.len() as u64;
    let total_chunks: u64 = per_file.iter().map(|(_, refs, _)| refs.len() as u64).sum();
    let chunk_index_bytes = unique_count * 48;
    let mut file_table_bytes = 0u64;
    let mut file_manifest_bytes = 0u64;
    let mut records = Vec::with_capacity(entries.len());
    let mut latencies = Vec::new();
    let mut losses = Vec::new();
    let mut seen_unique = vec![false; compressed.len()];

    for (entry, (path, refs, lens)) in entries.iter().zip(per_file.iter()) {
        if &entry.path != path {
            return Err("CDC file order mismatch".to_owned());
        }
        let mut marginal = 0u64;
        let mut unique_contribution = 0u64;
        for (index, raw_len) in refs.iter().zip(lens.iter()) {
            marginal += compressed[*index].len() as u64;
            if !seen_unique[*index] {
                seen_unique[*index] = true;
                unique_contribution += compressed[*index].len() as u64;
            }
            let _ = raw_len;
        }
        let manifest = 32 * refs.len() as u64 + 8 + 2 + entry.path.len() as u64;
        file_table_bytes += 32 + 8 + 2 + entry.path.len() as u64;
        file_manifest_bytes += manifest;
        marginal_compressed_total += marginal;

        // Verified decode: decompress every chunk and concatenate.
        let mut samples = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            let mut out = Vec::new();
            for (index, raw_len) in refs.iter().zip(lens.iter()) {
                let decoded = zstd::bulk::decompress(&compressed[*index], *raw_len as usize)
                    .map_err(|e| format!("{}: zstd decode: {e}", entry.path))?;
                out.extend_from_slice(&decoded);
            }
            samples.push(start.elapsed().as_nanos());
            if out != expected[&entry.path] {
                return Err(format!("{}: CDC decode bytes differ", entry.path));
            }
        }
        let median = median_ns(&mut samples);
        latencies.push(median);
        if marginal > entry.visible_bytes {
            losses.push(json!({
                "path": entry.path,
                "kind": entry.kind,
                "marginal_bytes": marginal,
                "visible_bytes": entry.visible_bytes,
                "reason": "CDC marginal bytes exceed the visible representation",
            }));
        }
        records.push(json!({
            "strategy": "cdc_zstd",
            "path": entry.path,
            "kind": entry.kind,
            "visible_bytes": entry.visible_bytes,
            "source_bytes": entry.source_bytes,
            "mode": "cdc-zstd",
            "chunks": refs.len(),
            "frame_bytes": marginal,
            "marginal_bytes": marginal,
            "unique_contribution_bytes": unique_contribution,
            "base_bytes": 0,
            "pair_bytes": marginal,
            "physical_contribution_bytes": unique_contribution,
            "manifest_bytes": manifest,
            "read_latency_ns": median,
            "dependency_depth": 0,
        }));
    }
    let physical = compressed_total + chunk_index_bytes + file_manifest_bytes + file_table_bytes;
    let aggregate = aggregate_row(
        "cdc_zstd",
        entries.len(),
        &records,
        physical,
        compressed_total,
        0,
        0,
        &latencies,
    );
    let (pairs, pair_aggregate) = cdc_pair_diagnostics(entries, &per_file);
    let chunking = params.to_json();
    let description = format!(
        "Gear-hash content-defined chunking (min {} B, average {} B via WEFT_CDC_AVG_KIB, max {} B), global dedup by H(weft/chunk/v1), zstd level 3 per unique chunk.",
        params.min_bytes, params.avg_bytes_effective, params.max_bytes
    );
    Ok(CdcResult {
        aggregate: aggregate.clone(),
        receipt: json!({
            "strategy": "cdc_zstd",
            "description": description,
            "chunking": chunking,
            "total_chunks": total_chunks,
            "total_raw_bytes": total_raw,
            "unique_raw_bytes": unique_raw_total,
            "dedup_saved_raw_bytes": total_raw - unique_raw_total,
            "marginal_compressed_bytes": marginal_compressed_total,
            "dedup_saved_compressed_bytes": marginal_compressed_total - compressed_total,
            "unique_chunks": unique_count,
            "unique_compressed_bytes": compressed_total,
            "chunk_index_bytes": chunk_index_bytes,
            "file_manifest_bytes": file_manifest_bytes,
            "file_table_bytes": file_table_bytes,
            "index_overhead_bytes": chunk_index_bytes + file_manifest_bytes + file_table_bytes,
            "physical_bytes": physical,
            "pair_diagnostics": {
                "pairs": pairs,
                "aggregate": pair_aggregate,
            },
            "records": &records,
            "aggregate": aggregate,
            "losses": losses,
        }),
        records,
    })
}

/// Per near-duplicate pair (declared `derived_from` / `revised_from`), measure
/// the two competing explanations for CDC's loss:
///
/// - boundary reproducibility: chunk starts shared at the *same absolute
///   offset*;
/// - content reuse: dependent chunks whose bytes (identified by the chunk
///   hash used for global dedup) also occur in the base.
///
/// Content reuse needs no offset agreement, so a chunk that survives an edit
/// shift still counts. `matched_shifted_chunks` isolates exactly that case.
fn cdc_pair_diagnostics(
    entries: &[Entry],
    per_file: &[(String, Vec<usize>, Vec<u32>)],
) -> (Vec<Json>, Json) {
    struct View<'a> {
        starts: Vec<usize>,
        refs: &'a [usize],
        lens: &'a [u32],
        bytes: u64,
    }

    let mut views: HashMap<&str, View> = HashMap::new();
    for (path, refs, lens) in per_file {
        let mut starts = Vec::with_capacity(lens.len());
        let mut cursor = 0usize;
        for len in lens.iter() {
            starts.push(cursor);
            cursor += *len as usize;
        }
        views.insert(
            path.as_str(),
            View {
                starts,
                refs: refs.as_slice(),
                lens: lens.as_slice(),
                bytes: cursor as u64,
            },
        );
    }

    let mut pairs = Vec::new();
    let mut dep_chunks_total = 0u64;
    let mut matched_chunks_total = 0u64;
    let mut dep_bytes_total = 0u64;
    let mut matched_bytes_total = 0u64;
    let mut dep_boundaries_total = 0u64;
    let mut shared_boundaries_total = 0u64;
    let mut union_boundaries_total = 0u64;
    let mut matched_same_offset_total = 0u64;
    let mut matched_shifted_total = 0u64;

    for entry in entries {
        let Some(relation) = &entry.relation else {
            continue;
        };
        if relation.predicate != "derived_from" && relation.predicate != "revised_from" {
            continue;
        }
        let Some(dep) = views.get(entry.path.as_str()) else {
            continue;
        };
        let Some(base) = views.get(relation.target.as_str()) else {
            continue;
        };

        // Interior chunk starts: every chunk start except offset 0. The two
        // files can differ in length, so the final end offset is not a
        // boundary candidate.
        let base_starts: HashSet<usize> = base.starts.iter().skip(1).copied().collect();
        let dep_starts: Vec<usize> = dep.starts.iter().skip(1).copied().collect();
        let shared = dep_starts
            .iter()
            .filter(|offset| base_starts.contains(offset))
            .count() as u64;
        let union = (dep_starts.len() as u64) + (base_starts.len() as u64) - shared;

        let base_refs: HashSet<usize> = base.refs.iter().copied().collect();
        let base_offset_ref: HashMap<usize, usize> = base
            .starts
            .iter()
            .copied()
            .zip(base.refs.iter().copied())
            .collect();
        let mut matched_chunks = 0u64;
        let mut matched_bytes = 0u64;
        let mut matched_same_offset = 0u64;
        let mut dep_bytes = 0u64;
        for (index, (reference, len)) in dep.refs.iter().zip(dep.lens.iter()).enumerate() {
            dep_bytes += *len as u64;
            if base_refs.contains(reference) {
                matched_chunks += 1;
                matched_bytes += *len as u64;
                if base_offset_ref.get(&dep.starts[index]) == Some(reference) {
                    matched_same_offset += 1;
                }
            }
        }
        let dep_chunks = dep.refs.len() as u64;
        let matched_shifted = matched_chunks - matched_same_offset;

        pairs.push(json!({
            "path": entry.path,
            "base_path": relation.target,
            "predicate": relation.predicate,
            "kind": entry.kind,
            "dep_bytes": dep_bytes,
            "base_bytes": base.bytes,
            "dep_chunks": dep_chunks,
            "base_chunks": base.refs.len(),
            "dep_boundaries": dep_starts.len(),
            "base_boundaries": base_starts.len(),
            "shared_boundaries": shared,
            "boundary_shared_fraction": if union == 0 { 1.0 } else { shared as f64 / union as f64 },
            "dep_boundary_match_fraction": if dep_starts.is_empty() {
                1.0
            } else {
                shared as f64 / dep_starts.len() as f64
            },
            "content_matched_chunks": matched_chunks,
            "content_reuse_fraction_by_count": if dep_chunks == 0 {
                0.0
            } else {
                matched_chunks as f64 / dep_chunks as f64
            },
            "content_matched_bytes": matched_bytes,
            "content_reuse_fraction_by_bytes": if dep_bytes == 0 {
                0.0
            } else {
                matched_bytes as f64 / dep_bytes as f64
            },
            "unmatched_bytes": dep_bytes - matched_bytes,
            "content_matched_same_offset_chunks": matched_same_offset,
            "content_matched_shifted_chunks": matched_shifted,
        }));

        dep_chunks_total += dep_chunks;
        matched_chunks_total += matched_chunks;
        dep_bytes_total += dep_bytes;
        matched_bytes_total += matched_bytes;
        dep_boundaries_total += dep_starts.len() as u64;
        shared_boundaries_total += shared;
        union_boundaries_total += union;
        matched_same_offset_total += matched_same_offset;
        matched_shifted_total += matched_shifted;
    }

    let aggregate = json!({
        "pairs": pairs.len(),
        "dep_chunks": dep_chunks_total,
        "content_matched_chunks": matched_chunks_total,
        "content_reuse_fraction_by_count": ratio(matched_chunks_total, dep_chunks_total),
        "dep_bytes": dep_bytes_total,
        "content_matched_bytes": matched_bytes_total,
        "content_reuse_fraction_by_bytes": ratio(matched_bytes_total, dep_bytes_total),
        "unmatched_bytes": dep_bytes_total - matched_bytes_total,
        "dep_boundaries": dep_boundaries_total,
        "shared_boundaries": shared_boundaries_total,
        "union_boundaries": union_boundaries_total,
        "boundary_shared_fraction": ratio(shared_boundaries_total, union_boundaries_total),
        "dep_boundary_match_fraction": ratio(shared_boundaries_total, dep_boundaries_total),
        "content_matched_same_offset_chunks": matched_same_offset_total,
        "content_matched_shifted_chunks": matched_shifted_total,
    });
    (pairs, aggregate)
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn run_cdc_sweep(
    args: &Args,
    entries: &[Entry],
    expected: &HashMap<String, Vec<u8>>,
    corpus_meta: &Json,
    sweep: &[f64],
) -> Result<(), String> {
    let limits = Limits::default();
    let level = 3;
    let mut resolver = MapResolver::new();
    for bytes in expected.values() {
        resolver.insert_bytes(bytes.clone());
    }

    // The forced-zstd comparator is measured in this same run so every delta
    // in the sweep has an identical denominator.
    let zstd = measure_zstd_forced(
        entries,
        expected,
        &resolver,
        level,
        &limits,
        args.iterations,
    )?;
    let zstd_physical = zstd.aggregate["physical_bytes"].as_u64().unwrap_or(0);
    let zstd_frame_bytes = zstd.aggregate["frame_bytes"].clone();

    let mut configurations = Vec::new();
    for avg_kib in sweep {
        let params = CdcParams::from_avg_kib(*avg_kib)?;
        let cdc = measure_cdc(entries, expected, args.iterations, &params)?;
        let physical = cdc.receipt["physical_bytes"].as_u64().unwrap_or(0);
        let index_overhead = [
            "chunk_index_bytes",
            "file_manifest_bytes",
            "file_table_bytes",
        ]
        .iter()
        .filter_map(|key| cdc.receipt[*key].as_u64())
        .sum::<u64>();
        let unique_chunks = cdc.receipt["unique_chunks"].as_u64().unwrap_or(0);
        let unique_compressed_bytes = cdc.receipt["unique_compressed_bytes"].as_u64().unwrap_or(0);
        let unique_raw_bytes = cdc.receipt["unique_raw_bytes"].as_u64().unwrap_or(0);
        let marginal_compressed_bytes = cdc.receipt["marginal_compressed_bytes"]
            .as_u64()
            .unwrap_or(0);
        let pair_aggregate = cdc.receipt["pair_diagnostics"]["aggregate"].clone();
        let delta = if zstd_physical == 0 {
            0.0
        } else {
            (physical as f64 - zstd_physical as f64) / zstd_physical as f64
        };
        configurations.push(json!({
            "avg_chunk_kib_requested": avg_kib,
            "unique_chunks": unique_chunks,
            "unique_compressed_bytes": unique_compressed_bytes,
            "unique_raw_bytes": unique_raw_bytes,
            "marginal_compressed_bytes": marginal_compressed_bytes,
            "index_overhead_bytes": index_overhead,
            "physical_bytes": physical,
            "delta_vs_zstd_forced_fraction": delta,
            "pair_aggregate": pair_aggregate,
            "cdc": cdc.receipt,
        }));
    }

    let receipt = json!({
        "kind": "ai-corpus-cdc-sweep",
        "schema": "weft.ai-cdc-sweep.v1",
        "label": "Synthetic ai-v1 corpus, single host, OOM-bounded bench court. This is a chunk-size sensitivity measurement of the harness CDC construction, not a compression, throughput, or superiority claim for any product path.",
        "timestamp_utc": utc_stamp(),
        "environment": environment(),
        "revisions": court_revisions(),
        "corpus": corpus_meta,
        "iterations": args.iterations,
        "sweep_avg_kib": sweep,
        "simulated_court_limits": {
            "memory": env::var("WEFT_COURT_MEMORY").ok(),
            "memory_swap": env::var("WEFT_COURT_MEMORY").ok(),
            "cpus": env::var("WEFT_COURT_CPUS").ok(),
            "pids": env::var("WEFT_COURT_PIDS").ok(),
            "tmpfs_tmp": "1g",
            "cap_drop": "ALL",
            "no_new_privileges": true,
        },
        "comparator": {
            "strategy": "zstd_forced",
            "physical_bytes": zstd_physical,
            "frame_bytes": zstd_frame_bytes,
            "receipt": zstd.receipt,
        },
        "configurations": configurations,
        "notes": [
            "`boundary_shared_fraction` counts chunk starts shared at identical absolute offsets (Jaccard over interior boundaries of each pair).",
            "`content_reuse_fraction_by_bytes` counts dependent-pair chunk bytes whose chunk hash also occurs in the base; it does not require matching offsets, so it measures content-defined reuse directly.",
            "`content_matched_shifted_chunks` counts matched chunks whose absolute start offset differs from the base's, which isolates offset tolerance.",
            "Pairs are the 20 manifest-declared derived_from/revised_from relations. No pair was selected or discarded after measurement.",
            "The corpus and all other strategies are unchanged from bench/RESULTS-ai-v1.md.",
        ],
    });

    write_json(&args.out, &receipt)?;
    println!("wrote {}", args.out.display());
    Ok(())
}

fn cdc_chunks(data: &[u8], table: &[u64; 256], params: &CdcParams) -> Vec<(usize, usize)> {
    let min = params.min_bytes;
    let max = params.max_bytes;
    let mask = params.mask();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut hash = 0u64;
    for (index, byte) in data.iter().enumerate() {
        hash = (hash << 1).wrapping_add(table[*byte as usize]);
        let len = index + 1 - start;
        if len >= min && ((hash & mask) == 0 || len >= max) {
            out.push((start, index + 1));
            start = index + 1;
            hash = 0;
        }
    }
    if start < data.len() {
        out.push((start, data.len()));
    }
    out
}

fn gear_table() -> [u64; 256] {
    let mut out = [0u64; 256];
    let mut state = 0x243F_6A88_85A3_08D3u64;
    for slot in out.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *slot = state;
    }
    out
}

fn compute_depths(entries: &[Entry]) -> HashMap<String, usize> {
    let by_path: HashMap<&str, &Entry> = entries.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut memo: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        let mut stack = Vec::new();
        let mut current = entry.path.clone();
        let mut depth = 0usize;
        loop {
            if let Some(value) = memo.get(&current) {
                depth += value;
                break;
            }
            if stack.contains(&current) || stack.len() > 64 {
                break;
            }
            stack.push(current.clone());
            let Some(next) = by_path
                .get(current.as_str())
                .and_then(|e| e.relation.as_ref())
            else {
                break;
            };
            current = next.target.clone();
            depth += 1;
        }
        memo.insert(entry.path.clone(), depth);
    }
    memo
}

fn file_index_bytes(entry: &Entry) -> u64 {
    32 + 8 + 1 + 2 + entry.path.len() as u64 + 1
}

fn median_ns(samples: &mut Vec<u128>) -> u64 {
    samples.sort_unstable();
    let value = samples.get(samples.len() / 2).copied().unwrap_or(0);
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| format!("serialize: {e}"))?;
    fs::write(path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn write_csv(path: &Path, rows: &[Json]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut out = String::from(
        "strategy,path,kind,visible_bytes,physical_bytes,frame_bytes,base_bytes,manifest_bytes,read_latency_ns,dependency_depth,mode,chunks\n",
    );
    for row in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_field(row["strategy"].as_str().unwrap_or("")),
            csv_field(row["path"].as_str().unwrap_or("")),
            csv_field(row["kind"].as_str().unwrap_or("")),
            row["visible_bytes"].as_u64().unwrap_or(0),
            row["physical_bytes"]
                .as_u64()
                .or_else(|| row["physical_contribution_bytes"].as_u64())
                .unwrap_or(0),
            row["frame_bytes"].as_u64().unwrap_or(0),
            row["base_bytes"].as_u64().unwrap_or(0),
            row["manifest_bytes"].as_u64().unwrap_or(0),
            row["read_latency_ns"].as_u64().unwrap_or(0),
            row["dependency_depth"].as_u64().unwrap_or(0),
            csv_field(row["mode"].as_str().unwrap_or("")),
            row["chunks"].as_u64().unwrap_or(0),
        ));
    }
    fs::write(path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    let output = Command::new("sha256sum").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    text.split_whitespace().next().map(|s| s.to_owned())
}

fn environment() -> Json {
    json!({
        "uname": command_output("uname", &["-srm"]),
        "cpu_model": cpu_model(),
        "nproc": command_output("nproc", &[]),
        "mem_total_kib": mem_total_kib(),
        "sha256sum": command_output("sha256sum", &["--version"]),
    })
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn cpu_model() -> Option<String> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    text.lines()
        .find(|line| line.starts_with("model name"))
        .and_then(|line| line.split(':').nth(1))
        .map(|value| value.trim().to_owned())
}

fn mem_total_kib() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    text.lines()
        .find(|line| line.starts_with("MemTotal:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
}

fn utc_stamp() -> String {
    // The bench script passes the timestamp; fall back to a deterministic
    // marker rather than opening a time crate.
    env::var("WEFT_BENCH_STAMP").unwrap_or_else(|_| "unset".to_owned())
}
