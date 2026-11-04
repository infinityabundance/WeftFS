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

use std::collections::{BTreeMap, HashMap};
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
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(64);
            }
        }
        index += 1;
    }
    Args {
        corpus,
        out,
        csv,
        graph,
        iterations,
    }
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

    // A resolver containing every plaintext lets DELTA/DICTIONARY frames
    // decode with their dependency already materialized (the warm path).
    let mut plaintext_resolver = MapResolver::new();
    for bytes in data.values() {
        plaintext_resolver.insert_bytes(bytes.clone());
    }

    let level = 3;
    let auto_plans = plan_weft(&entries, &expected, &kinds, false, level, &limits)?;
    let context_planning = plan_weft_context(&entries, &expected, &kinds, level, &limits)?;
    let context_plans = &context_planning.plans;
    let depths = compute_depths(&entries);

    let zstd = measure_zstd_forced(
        &entries,
        &expected,
        &plaintext_resolver,
        level,
        &limits,
        args.iterations,
    )?;
    let auto = measure_weft(
        "weft_auto",
        &entries,
        &auto_plans,
        &expected,
        &depths,
        false,
        &plaintext_resolver,
        &limits,
        args.iterations,
    )?;
    let context = measure_weft(
        "weft_context",
        &entries,
        context_plans,
        &expected,
        &depths,
        true,
        &plaintext_resolver,
        &limits,
        args.iterations,
    )?;
    let cdc = measure_cdc(&entries, &expected, args.iterations)?;
    let typed_candidates = typed_mode_report(&entries, &expected, &kinds, &auto_plans, &limits)?;

    let mut cost_rows: Vec<Json> = Vec::new();
    for strategy in [&zstd, &auto, &context] {
        for row in &strategy.records {
            cost_rows.push(row.clone());
        }
        cost_rows.push(strategy.aggregate.clone());
    }
    for row in &cdc.records {
        cost_rows.push(row.clone());
    }
    cost_rows.push(cdc.aggregate.clone());

    let manifest_sha = sha256_file(&manifest_path).unwrap_or_else(|| "unavailable".to_owned());
    let receipt = json!({
        "kind": "ai-corpus-cost",
        "schema": "weft.ai-cost.v1",
        "timestamp_utc": utc_stamp(),
        "environment": environment(),
        "revisions": {
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
        },
        "corpus": {
            "path": args.corpus.display().to_string(),
            "schema": manifest.schema,
            "name": manifest.corpus,
            "license": manifest.license,
            "provenance": manifest.provenance,
            "generator": manifest.generator,
            "seed": manifest.seed,
            "manifest_sha256": manifest_sha,
            "manifest_bytes": manifest_bytes.len(),
            "totals": {
                "files": manifest.totals.files,
                "source_bytes": manifest.totals.source_bytes,
                "visible_bytes": manifest.totals.visible_bytes,
            },
            "verified_lengths": true,
        },
        "iterations": args.iterations,
        "typed_mode_candidates": typed_candidates,
        "planner_policy": context_planning.policy,
        "planner_rejections": context_planning.rejections,
        "strategies": {
            "zstd_forced": zstd.receipt,
            "cdc_zstd": cdc.receipt,
            "weft_auto": auto.receipt,
            "weft_context": context.receipt,
        },
        "cost_graph": {
            "axes": ["visible_bytes", "physical_bytes", "read_latency_ns", "dependency_depth"],
            "rows": cost_rows,
        },
        "notes": [
            "All sizes are bytes. `visible_bytes` is the manifest's declared visible representation; for TENSOR it is the element payload and for TOKENS it is the canonical delta-LEB128 payload, while `source_bytes` is the JSON/text file.",
            "`physical_bytes` counts every stored frame/chunk/index/manifest once for the whole corpus; `pair_bytes` sums marginal + base plaintext per dependent object and therefore may double-count shared bases. Pair losses are reported raw and are not hidden.",
            "`weft_context` uses a whole-pair-aware planner: a DELTA/DICTIONARY/REFERENCE candidate is selected only when the base is shared (>=2 direct dependents) or retained (a declared chain root), or when marginal frame + base plaintext is strictly smaller than the best context-free frame. `planner_cost` records the objective value; `planner_rejections` records every contextual candidate that lost for cost reasons.",
            "`read_latency_ns` is the median of verified decodes; `historical_read_ns` uses the cold dependency-DAG path when the object has a relation chain, otherwise the warm path.",
            "CDC is content-only and relation-agnostic. WeftCodec context bases come only from declared typed relations in the manifest.",
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

fn measure_cdc(
    entries: &[Entry],
    expected: &HashMap<String, Vec<u8>>,
    iterations: usize,
) -> Result<CdcResult, String> {
    let table = gear_table();
    let mut unique: HashMap<Hash32, usize> = HashMap::new();
    let mut compressed: Vec<Vec<u8>> = Vec::new();
    let mut per_file: Vec<(String, Vec<usize>, Vec<u32>)> = Vec::new();
    let mut compressed_total = 0u64;

    for entry in entries {
        let bytes = &expected[&entry.path];
        let chunks = cdc_chunks(bytes, &table);
        let mut refs = Vec::with_capacity(chunks.len());
        let mut lens = Vec::with_capacity(chunks.len());
        for (start, end) in chunks {
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
                compressed.push(packed);
                unique.insert(hash, index);
                index
            };
            refs.push(index);
        }
        per_file.push((entry.path.clone(), refs, lens));
    }
    let unique_count = compressed.len() as u64;
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
    Ok(CdcResult {
        aggregate: aggregate.clone(),
        receipt: json!({
            "strategy": "cdc_zstd",
            "description": "Gear-hash content-defined chunking (min 2 KiB, avg 8 KiB, max 32 KiB), global dedup by H(weft/chunk/v1), zstd level 3 per unique chunk.",
            "unique_chunks": unique_count,
            "unique_compressed_bytes": compressed_total,
            "chunk_index_bytes": chunk_index_bytes,
            "file_manifest_bytes": file_manifest_bytes,
            "file_table_bytes": file_table_bytes,
            "physical_bytes": physical,
            "records": &records,
            "aggregate": aggregate,
            "losses": losses,
        }),
        records,
    })
}

fn cdc_chunks(data: &[u8], table: &[u64; 256]) -> Vec<(usize, usize)> {
    const MIN: usize = 2048;
    const MAX: usize = 32768;
    const MASK: u64 = (1 << 13) - 1; // average 8 KiB
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut hash = 0u64;
    for (index, byte) in data.iter().enumerate() {
        hash = (hash << 1).wrapping_add(table[*byte as usize]);
        let len = index + 1 - start;
        if len >= MIN && ((hash & MASK) == 0 || len >= MAX) {
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
