//! Sampled-vs-exhaustive planner oracle (N11).
//!
//! For every tensor it runs the shipped `TwoStage` planner and a forced
//! `Exhaustive` planner **on the same bytes in the same process**, then reports
//! the paired difference:
//!
//! * estimate-violation rate: an actual encoded size outside the interval the
//!   sampled planner priced (level 3 and level 19), and mode-9 exactness;
//! * skip-regret: extra published bytes and extra measured planner time when
//!   the sampled planner chose a worse representation than the oracle;
//! * fallback behaviour: whether the sampled planner recorded a violation and
//!   fell back to exhaustive, and whether a regret case happened **without**
//!   any recorded violation (the dangerous, undetected case).
//!
//! Two catalogues, both declared in `bench/CONTRACTS-heldout.md` §5: real
//! tensors from the validation and held-out families, and eight stratified
//! synthetic tensors constructed to stress the 4-chunk sampler.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value as Json};

use weftfs::safetensors::SafetensorsFile;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{
    PlannerMode, TensorLayoutSpec, TensorPlan, TensorPlanOptions, TensorWriteSpec,
};

const SCHEMA: &str = "weft.two-stage-oracle.v1";

fn utc_stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        now.subsec_millis()
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (sorted.len() as f64 - 1.0) * p.clamp(0.0, 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] * (1.0 - (rank - lo as f64)) + sorted[hi] * (rank - lo as f64)
    }
}

fn median(sorted: &[f64]) -> f64 {
    percentile(sorted, 0.5)
}

struct Args {
    container: Option<PathBuf>,
    family: String,
    out: PathBuf,
    work: PathBuf,
    max_tensor_bytes: u64,
    max_tensors: usize,
    max_total_bytes: u64,
    seed: u64,
    per_stratum: usize,
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let parse_u64 = |flag: &str, default: u64| -> Result<u64, String> {
        match value_of(&raw, flag) {
            None => Ok(default),
            Some(text) => text
                .parse()
                .map_err(|_| format!("{flag} must be an integer")),
        }
    };
    Ok(Args {
        container: value_of(&raw, "--container").map(PathBuf::from),
        family: value_of(&raw, "--family").unwrap_or_else(|| "synthetic".to_owned()),
        out: PathBuf::from(value_of(&raw, "--out").ok_or("--out is required")?),
        work: PathBuf::from(value_of(&raw, "--work").ok_or("--work is required")?),
        max_tensor_bytes: parse_u64("--max-tensor-bytes", 192 * 1024 * 1024)?,
        max_tensors: parse_u64("--max-tensors", 96)? as usize,
        max_total_bytes: parse_u64("--max-total-bytes", 1536 * 1024 * 1024)?,
        seed: parse_u64("--seed", 0x9e3779b97f4a7c15)?,
        per_stratum: parse_u64("--per-stratum", 8)? as usize,
    })
}

fn plan_options(mode: PlannerMode) -> TensorPlanOptions {
    let mut options = TensorPlanOptions::default();
    options.planner = mode;
    options.max_candidates = 64;
    options.semantic_search = false;
    options
}

struct Observation {
    label: String,
    dtype: String,
    logical_bytes: u64,
    two_stage: PlanSummary,
    exhaustive: PlanSummary,
    two_stage_wall_ns: u64,
    exhaustive_wall_ns: u64,
    violations: usize,
    violation_details: Vec<String>,
    fallback: bool,
    zstd3_contains: Option<bool>,
    zstd19_contains: Option<bool>,
    mode9_exact: Option<bool>,
}

struct PlanSummary {
    chosen: String,
    bytes: u64,
    candidates: BTreeMap<String, u64>,
}

fn summarise(plan: &TensorPlan) -> PlanSummary {
    let mut candidates = BTreeMap::new();
    for candidate in &plan.candidates {
        if let Some(bytes) = candidate.physical_bytes {
            // Two candidates are both `TensorStrategy::Zstd` (levels 3 and 19);
            // keep the smallest for the map and rely on `plan.chosen` for the
            // decision, so the map is a set of offered strategies.
            candidates
                .entry(candidate.strategy.as_str().to_owned())
                .and_modify(|existing: &mut u64| *existing = (*existing).min(bytes))
                .or_insert(bytes);
        }
    }
    PlanSummary {
        chosen: plan.chosen.as_str().to_owned(),
        bytes: plan.chosen_physical_bytes,
        candidates,
    }
}

fn observe(
    store: &Store,
    label: &str,
    dtype: &str,
    shape: &[u64],
    bytes: &[u8],
    layout: &TensorLayoutSpec,
) -> Result<Observation, String> {
    let exhaustive_options = plan_options(PlannerMode::Exhaustive);
    // The sampled planner is exercised through the shipped prepare half
    // (`encode_tensor_object`), which is the only entry point that records the
    // R18 decision stream. `plan_tensor` returns no trace, so calling it here
    // would compare the exhaustive planner with itself.
    let mut spec = TensorWriteSpec::default();
    spec.plan = plan_options(PlannerMode::TwoStage);
    let seed = [0x5au8; 32];
    let started = Instant::now();
    let encoded = store
        .encode_tensor_object(dtype, shape, bytes, layout, &spec, &seed)
        .map_err(|e| format!("two-stage encode {label}: {e}"))?;
    let two_stage = encoded.plan.clone();
    let two_stage_wall_ns = started.elapsed().as_nanos() as u64;
    let started = Instant::now();
    let exhaustive = store
        .plan_tensor(dtype, shape, bytes, layout, &[], &exhaustive_options)
        .map_err(|e| format!("exhaustive plan {label}: {e}"))?;
    let exhaustive_wall_ns = started.elapsed().as_nanos() as u64;

    let trace = two_stage.planner.as_ref();
    let analysis = trace.map(|t| &t.analysis);
    let zstd3_contains = analysis
        .and_then(|a| a.zstd3.as_ref().map(|e| (e, trace)))
        .and_then(|(estimate, trace)| {
            trace.and_then(|t| t.actual_zstd3.map(|actual| estimate.contains(actual)))
        });
    let zstd19_contains = analysis
        .and_then(|a| a.zstd19.as_ref().map(|e| (e, trace)))
        .and_then(|(estimate, trace)| {
            trace.and_then(|t| t.actual_zstd19.map(|actual| estimate.contains(actual)))
        });
    let mode9_exact = analysis
        .and_then(|a| a.mode9.as_ref().map(|p| (p, trace)))
        .and_then(|(prediction, trace)| {
            trace.and_then(|t| {
                t.actual_mode9
                    .map(|actual| prediction.exact_bytes == actual)
            })
        });

    Ok(Observation {
        label: label.to_owned(),
        dtype: dtype.to_owned(),
        logical_bytes: bytes.len() as u64,
        two_stage: summarise(&two_stage),
        exhaustive: summarise(&exhaustive),
        two_stage_wall_ns,
        exhaustive_wall_ns,
        violations: trace.map(|t| t.prediction_violations.len()).unwrap_or(0),
        violation_details: trace
            .map(|t| t.prediction_violations.clone())
            .unwrap_or_default(),
        fallback: trace.map(|t| t.fallback_exhaustive).unwrap_or(false),
        zstd3_contains,
        zstd19_contains,
        mode9_exact,
    })
}

fn obs_json(obs: &Observation) -> Json {
    json!({
        "label": obs.label,
        "dtype": obs.dtype,
        "logical_bytes": obs.logical_bytes,
        "two_stage": {
            "chosen": obs.two_stage.chosen,
            "bytes": obs.two_stage.bytes,
            "wall_ns": obs.two_stage_wall_ns,
            "candidates": obs.two_stage.candidates,
        },
        "exhaustive": {
            "chosen": obs.exhaustive.chosen,
            "bytes": obs.exhaustive.bytes,
            "wall_ns": obs.exhaustive_wall_ns,
            "candidates": obs.exhaustive.candidates,
        },
        "regret_bytes": obs.two_stage.bytes as i64 - obs.exhaustive.bytes as i64,
        "regret_ns": obs.two_stage_wall_ns as i64 - obs.exhaustive_wall_ns as i64,
        "violations": obs.violations,
        "violation_details": obs.violation_details,
        "fallback_exhaustive": obs.fallback,
        "zstd3_interval_contains_actual": obs.zstd3_contains,
        "zstd19_interval_contains_actual": obs.zstd19_contains,
        "mode9_exact_prediction_holds": obs.mode9_exact,
    })
}

/// Aggregate one catalogue.
fn aggregate(observations: &[Observation]) -> Json {
    let n = observations.len();
    let disagree: Vec<&Observation> = observations
        .iter()
        .filter(|o| {
            o.two_stage.chosen != o.exhaustive.chosen || o.two_stage.bytes != o.exhaustive.bytes
        })
        .collect();
    let mut regrets: Vec<f64> = disagree
        .iter()
        .map(|o| (o.two_stage.bytes as i64 - o.exhaustive.bytes as i64) as f64)
        .collect();
    regrets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let loss_regrets: Vec<f64> = regrets.iter().copied().filter(|v| *v > 0.0).collect();
    let total_regret: f64 = loss_regrets.iter().sum();
    let violations = observations.iter().filter(|o| o.violations > 0).count();
    let fallbacks = observations.iter().filter(|o| o.fallback).count();
    let undetected = disagree
        .iter()
        .filter(|o| {
            (o.two_stage.bytes as i64 - o.exhaustive.bytes as i64) > 0
                && o.violations == 0
                && !o.fallback
        })
        .count();
    let checked3 = observations
        .iter()
        .filter(|o| o.zstd3_contains.is_some())
        .count();
    let bad3 = observations
        .iter()
        .filter(|o| o.zstd3_contains == Some(false))
        .count();
    let checked19 = observations
        .iter()
        .filter(|o| o.zstd19_contains.is_some())
        .count();
    let bad19 = observations
        .iter()
        .filter(|o| o.zstd19_contains == Some(false))
        .count();
    let checked9 = observations
        .iter()
        .filter(|o| o.mode9_exact.is_some())
        .count();
    let bad9 = observations
        .iter()
        .filter(|o| o.mode9_exact == Some(false))
        .count();
    json!({
        "tensors": n,
        "disagreements": disagree.len(),
        "skip_regret_tensors": loss_regrets.len(),
        "skip_regret_bytes_total": total_regret,
        "skip_regret_bytes_median": if loss_regrets.is_empty() { 0.0 } else { median(&loss_regrets) },
        "skip_regret_bytes_p90": if loss_regrets.is_empty() { 0.0 } else { percentile(&loss_regrets, 0.90) },
        "skip_regret_bytes_max": loss_regrets.last().copied().unwrap_or(0.0),
        "undetected_regret_tensors": undetected,
        "violating_tensors": violations,
        "violation_rate": if n > 0 { violations as f64 / n as f64 } else { 0.0 },
        "fallback_tensors": fallbacks,
        "zstd3_interval_checked": checked3,
        "zstd3_interval_violations": bad3,
        "zstd19_interval_checked": checked19,
        "zstd19_interval_violations": bad19,
        "mode9_exact_checked": checked9,
        "mode9_exact_violations": bad9,
    })
}

// ---------------------------------------------------------------------------
// synthetic strata
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Avoid the zero state.
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn fill(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(8) {
            let value = self.next().to_le_bytes();
            let len = chunk.len();
            chunk.copy_from_slice(&value[..len]);
        }
    }
}

fn stratum(name: &str, chunks: usize, chunk: usize, seed: u64) -> Result<Vec<u8>, String> {
    let size = chunks * chunk;
    let sampled: Vec<usize> = weftfs::two_stage::sample_indices(chunks as u64, 4)
        .into_iter()
        .map(|v| v as usize)
        .collect();
    let mut rng = Rng::new(
        seed ^ name
            .bytes()
            .fold(0u64, |acc, b| acc.rotate_left(5) ^ b as u64),
    );
    let mut out = vec![0u8; size];
    match name {
        "uniform-random" => rng.fill(&mut out),
        "all-zero" => {}
        "alternating" => {
            for index in 0..chunks {
                let start = index * chunk;
                if index % 2 == 0 {
                    rng.fill(&mut out[start..start + chunk]);
                }
            }
        }
        "rare-heavy" => {
            let heavy = 8 + (rng.next() as usize % (chunks - 8).max(1));
            let start = heavy * chunk;
            rng.fill(&mut out[start..start + chunk]);
        }
        "sampled-lucky" => {
            for index in 0..chunks {
                if sampled.contains(&index) {
                    continue;
                }
                let start = index * chunk;
                rng.fill(&mut out[start..start + chunk]);
            }
        }
        "sampled-unlucky" => {
            for index in &sampled {
                let start = index * chunk;
                rng.fill(&mut out[start..start + chunk]);
            }
        }
        "two-level" => {
            for index in 0..chunks {
                let start = index * chunk;
                if index % 2 == 1 {
                    let value = (index & 0xff) as u8;
                    out[start..start + chunk].fill(value);
                } else {
                    rng.fill(&mut out[start..start + chunk]);
                }
            }
        }
        "per-chunk-random" => {
            for index in 0..chunks {
                let start = index * chunk;
                if rng.next() & 1 == 0 {
                    rng.fill(&mut out[start..start + chunk]);
                }
            }
        }
        other => return Err(format!("unknown stratum {other}")),
    }
    Ok(out)
}

const STRATA: [&str; 8] = [
    "uniform-random",
    "all-zero",
    "alternating",
    "rare-heavy",
    "sampled-lucky",
    "sampled-unlucky",
    "two-level",
    "per-chunk-random",
];

fn select_real(file: &SafetensorsFile, args: &Args) -> Vec<usize> {
    let mut eligible: Vec<usize> = file
        .tensors
        .iter()
        .enumerate()
        .filter(|(_, t)| t.byte_len() > 0 && t.byte_len() <= args.max_tensor_bytes)
        .map(|(i, _)| i)
        .collect();
    eligible.sort_by(|a, b| file.tensors[*a].name.cmp(&file.tensors[*b].name));
    let mut stride = 1usize;
    loop {
        let picked: Vec<usize> = eligible.iter().copied().step_by(stride).collect();
        let total: u64 = picked.iter().map(|i| file.tensors[*i].byte_len()).sum();
        if picked.len() <= args.max_tensors && total <= args.max_total_bytes {
            return picked;
        }
        if stride > eligible.len() {
            return eligible.iter().copied().take(1).collect();
        }
        stride += 1;
    }
}

fn main() {
    if let Err(err) = real_main() {
        eprintln!("two_stage_oracle: {err}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), String> {
    let args = parse_args()?;
    let started = Instant::now();
    if args.work.exists() {
        return Err(format!("{} already exists", args.work.display()));
    }
    let store = Store::init(&args.work, Options::default()).map_err(|e| e.to_string())?;
    let layout = TensorLayoutSpec::row_major();

    // The sampler operates on the codec chunk grid; generate on the same grid.
    let chunk =
        weftfs::representation::codec_chunk_size(store.options().chunk_size.max(1)) as usize;

    let mut strata_out = Vec::new();
    let mut all_synthetic: Vec<Observation> = Vec::new();
    for (index, name) in STRATA.iter().enumerate() {
        let mut observations: Vec<Observation> = Vec::new();
        let mut rows = Vec::new();
        for rep in 0..args.per_stratum.max(1) {
            let seed = args
                .seed
                .wrapping_add(index as u64 * 7919)
                .wrapping_add(rep as u64 * 104_729);
            let bytes = stratum(name, 32, chunk, seed)?;
            let shape = vec![(bytes.len() / 2 / 1024) as u64, 1024u64];
            let observation = observe(
                &store,
                &format!("synthetic/{name}#{rep}"),
                "bf16",
                &shape,
                &bytes,
                &layout,
            )?;
            rows.push(obs_json(&observation));
            observations.push(observation);
        }
        strata_out.push(json!({
            "stratum": name,
            "repetitions": rows.len(),
            "observations": rows,
            "aggregate": aggregate(&observations),
        }));
        all_synthetic.extend(observations);
    }

    let mut real_out = Vec::new();
    let mut all_real: Vec<Observation> = Vec::new();
    if let Some(path) = &args.container {
        let file = SafetensorsFile::parse(path).map_err(|e| e.to_string())?;
        for index in select_real(&file, &args) {
            let tensor = &file.tensors[index];
            let bytes = file.read_tensor(tensor).map_err(|e| e.to_string())?;
            let observation = observe(
                &store,
                &tensor.name,
                &tensor.dtype,
                &tensor.shape,
                &bytes,
                &layout,
            )?;
            real_out.push(obs_json(&observation));
            all_real.push(observation);
            drop(bytes);
        }
    }

    let stamp = utc_stamp();
    let mut all_observations: Vec<Observation> = Vec::new();
    for observation in all_synthetic.iter().chain(all_real.iter()) {
        all_observations.push(Observation {
            label: observation.label.clone(),
            dtype: observation.dtype.clone(),
            logical_bytes: observation.logical_bytes,
            two_stage: PlanSummary {
                chosen: observation.two_stage.chosen.clone(),
                bytes: observation.two_stage.bytes,
                candidates: observation.two_stage.candidates.clone(),
            },
            exhaustive: PlanSummary {
                chosen: observation.exhaustive.chosen.clone(),
                bytes: observation.exhaustive.bytes,
                candidates: observation.exhaustive.candidates.clone(),
            },
            two_stage_wall_ns: observation.two_stage_wall_ns,
            exhaustive_wall_ns: observation.exhaustive_wall_ns,
            violations: observation.violations,
            violation_details: observation.violation_details.clone(),
            fallback: observation.fallback,
            zstd3_contains: observation.zstd3_contains,
            zstd19_contains: observation.zstd19_contains,
            mode9_exact: observation.mode9_exact,
        });
    }
    let overall = aggregate(&all_observations);
    let receipt = json!({
        "schema": SCHEMA,
        "stamp": stamp,
        "family": args.family,
        "chunk_bytes": chunk,
        "chunks_per_synthetic": 32,
        "seed": args.seed,
        "court": {
            "limits": "3g memory / 3g swap / 1024 pids / 4 cpus",
            "objective": "bytes-only (lambda 0): the shipped default",
        },
        "synthetic": strata_out,
        "synthetic_aggregate": aggregate(&all_synthetic),
        "real": real_out,
        "real_aggregate": aggregate(&all_real),
        "overall_aggregate": overall,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });
    if let Some(parent) = args.out.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(
        &args.out,
        serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "two_stage_oracle: family={} synthetic={} real={} out={}",
        args.family,
        all_synthetic.len(),
        all_real.len(),
        args.out.display()
    );
    let _ = fs::remove_dir_all(Path::new(&args.work));
    Ok(())
}
