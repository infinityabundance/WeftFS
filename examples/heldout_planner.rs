//! Held-out predictive-planner court (T042 / N03).
//!
//! Two modes, deliberately separated into two process invocations:
//!
//! * `fit` measures the FIT families (Qwen2.5-0.5B base, pythia-70m step 0),
//!   writes `weft.heldout.model.v1`, and records every raw observation.
//! * `predict` loads that model **unchanged**, predicts each tensor's strategy,
//!   bytes, write wall, cold-restore wall and decode CPU from the model alone,
//!   then measures the same tensor and publishes the error distribution.
//!
//! The prediction is computed before the exhaustive plan is consulted and
//! before the store writes anything, so no held-out number can leak into a
//! prediction. The model carries no family name from the validation or
//! held-out split.
//!
//! Split declaration: `bench/CONTRACTS-heldout.md` (committed before any
//! measurement).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value as Json};

use weftfs::safetensors::{sha256_hex, SafetensorsFile};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{PlannerMode, TensorLayoutSpec, TensorPlanOptions, TensorWriteSpec};

const SCHEMA: &str = "weft.heldout.planner.v1";
const MODEL_SCHEMA: &str = "weft.heldout.model.v1";
const SLA_MULTIPLIERS: [f64; 5] = [0.75, 1.00, 1.25, 1.50, 2.00];
const SLA_MARGIN: f64 = 0.90;
const MIB: f64 = 1024.0 * 1024.0;

// ---------------------------------------------------------------------------
// small utilities
// ---------------------------------------------------------------------------

fn utc_stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    // Deterministic UTC formatting without a chrono-style dependency.
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
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

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let clamped = p.clamp(0.0, 1.0);
    let rank = (sorted.len() as f64 - 1.0) * clamped;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let w = rank - lo as f64;
        sorted[lo] * (1.0 - w) + sorted[hi] * w
    }
}

fn median(sorted: &[f64]) -> f64 {
    percentile(sorted, 0.5)
}

/// Absolute and signed-relative error distribution for one target.
fn error_block(pairs: &[(f64, f64)]) -> Json {
    if pairs.is_empty() {
        return json!({"n": 0, "note": "no observations"});
    }
    let mut abs_err: Vec<f64> = pairs.iter().map(|(p, m)| (p - m).abs()).collect();
    let mut rel: Vec<f64> = pairs
        .iter()
        .filter(|(_, m)| m.abs() > 0.0)
        .map(|(p, m)| (p - m) / m)
        .collect();
    let mut abs_rel: Vec<f64> = rel.iter().map(|v| v.abs()).collect();
    abs_err.sort_by(|a, b| a.partial_cmp(b).unwrap());
    rel.sort_by(|a, b| a.partial_cmp(b).unwrap());
    abs_rel.sort_by(|a, b| a.partial_cmp(b).unwrap());
    json!({
        "n": pairs.len(),
        "abs": {
            "median": median(&abs_err),
            "p90": percentile(&abs_err, 0.90),
            "p95": percentile(&abs_err, 0.95),
            "max": abs_err.last().copied().unwrap_or(f64::NAN),
        },
        "abs_rel": {
            "median": median(&abs_rel),
            "p90": percentile(&abs_rel, 0.90),
            "p95": percentile(&abs_rel, 0.95),
            "max": abs_rel.last().copied().unwrap_or(f64::NAN),
        },
        "signed_rel": {
            "median": median(&rel),
            "p10": percentile(&rel, 0.10),
            "p90": percentile(&rel, 0.90),
        },
        "small_n": pairs.len() < 7,
    })
}

fn peak_rss_kib() -> u64 {
    let text = match fs::read_to_string("/proc/self/status") {
        Ok(text) => text,
        Err(_) => return 0,
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse()
                .unwrap_or(0);
        }
    }
    0
}

/// Process CPU time in nanoseconds (`CLOCK_PROCESS_CPUTIME_ID`).
///
/// `/proc/self/stat` counts in 10 ms ticks, which is coarser than a cold read
/// of a small tensor; the clock is used so a decode-CPU observation is not a
/// row of zeros.
fn cpu_ns() -> u64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64).saturating_mul(1_000_000_000) + ts.tv_nsec as u64
}

fn walk_files(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => walk_files(&path, out),
                Ok(ft) if ft.is_file() => out.push(path),
                _ => {}
            }
        }
    }
}

fn fadvise_dontneed(root: &Path) -> usize {
    let mut files = Vec::new();
    walk_files(root, &mut files);
    let mut advised = 0;
    for path in &files {
        if let Ok(file) = OpenOptions::new().read(true).open(path) {
            use std::os::unix::io::AsRawFd;
            let rc =
                unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
            if rc == 0 {
                advised += 1;
            }
        }
    }
    advised
}

// ---------------------------------------------------------------------------
// arguments
// ---------------------------------------------------------------------------

struct Args {
    mode: String,
    family: String,
    container: PathBuf,
    /// FIT families, as `name=path`; repeatable. Empty means `--family`/`--container`.
    fit_specs: Vec<(String, PathBuf)>,
    out: PathBuf,
    model_in: Option<PathBuf>,
    model_out: Option<PathBuf>,
    work: PathBuf,
    max_tensor_bytes: u64,
    max_tensors: usize,
    max_total_bytes: u64,
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mode = value_of(&raw, "--mode").ok_or("--mode fit|predict is required")?;
    if mode != "fit" && mode != "predict" {
        return Err(format!("unknown --mode {mode}"));
    }
    let family = value_of(&raw, "--family").unwrap_or_default();
    let container = PathBuf::from(value_of(&raw, "--container").unwrap_or_default());
    let mut fit_specs = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == "--fit" {
            let spec = raw.get(i + 1).ok_or("--fit needs name=path")?;
            let (name, path) = spec
                .split_once('=')
                .ok_or("--fit needs name=path")?;
            fit_specs.push((name.to_owned(), PathBuf::from(path)));
            i += 2;
            continue;
        }
        i += 1;
    }
    let out = PathBuf::from(value_of(&raw, "--out").ok_or("--out is required")?);
    let work = PathBuf::from(
        value_of(&raw, "--work").ok_or("--work (store directory, must not exist) is required")?,
    );
    let parse_u64 = |flag: &str, default: u64| -> Result<u64, String> {
        match value_of(&raw, flag) {
            None => Ok(default),
            Some(text) => text
                .parse()
                .map_err(|_| format!("{flag} must be an integer")),
        }
    };
    Ok(Args {
        mode,
        family,
        container,
        fit_specs,
        out,
        model_in: value_of(&raw, "--model-in").map(PathBuf::from),
        model_out: value_of(&raw, "--model-out").map(PathBuf::from),
        work,
        max_tensor_bytes: parse_u64("--max-tensor-bytes", 192 * 1024 * 1024)?,
        max_tensors: parse_u64("--max-tensors", 96)? as usize,
        max_total_bytes: parse_u64("--max-total-bytes", 1536 * 1024 * 1024)?,
    })
}

// ---------------------------------------------------------------------------
// subset selection (a rule, declared in the contract, not result-dependent)
// ---------------------------------------------------------------------------

fn select_tensors(
    file: &SafetensorsFile,
    max_tensor_bytes: u64,
    max_tensors: usize,
    max_total_bytes: u64,
) -> Vec<usize> {
    let mut eligible: Vec<usize> = file
        .tensors
        .iter()
        .enumerate()
        .filter(|(_, t)| t.byte_len() > 0 && t.byte_len() <= max_tensor_bytes)
        .map(|(i, _)| i)
        .collect();
    eligible.sort_by(|a, b| file.tensors[*a].name.cmp(&file.tensors[*b].name));
    if eligible.is_empty() {
        return eligible;
    }
    let mut stride = 1usize;
    loop {
        let picked: Vec<usize> = eligible.iter().copied().step_by(stride).collect();
        let total: u64 = picked.iter().map(|i| file.tensors[*i].byte_len()).sum();
        if picked.len() <= max_tensors && total <= max_total_bytes {
            return picked;
        }
        if stride > eligible.len() {
            // Nothing satisfies the bound; the single smallest tensor is the
            // declared last resort (recorded in the receipt).
            return vec![*eligible
                .iter()
                .min_by_key(|i| file.tensors[**i].byte_len())
                .unwrap()];
        }
        stride += 1;
    }
}

// ---------------------------------------------------------------------------
// the model
// ---------------------------------------------------------------------------

fn slope_through_origin(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let sxx: f64 = xs.iter().map(|x| x * x).sum();
    if sxx <= 0.0 {
        return None;
    }
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| x * y).sum();
    Some(sxy / sxx)
}

fn applicability(dtype: &str, logical_bytes: u64) -> Vec<&'static str> {
    let mut out = vec!["raw", "zstd"];
    if logical_bytes >= 1024 * 1024 {
        out.push("zstd-19");
    }
    if matches!(dtype, "bf16" | "f16" | "f32") {
        out.push("float-tensor");
    }
    out
}

fn model_json(body: &Json) -> String {
    serde_json::to_string(body).unwrap_or_default()
}

struct Model {
    raw: Json,
    ratios: BTreeMap<(String, String), f64>,
    encode: BTreeMap<String, f64>,
    decode: BTreeMap<String, f64>,
    publish: f64,
    restore_overhead: f64,
    write_overhead: f64,
    sha256: String,
}

impl Model {
    fn load(path: &Path) -> Result<Model, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("read model: {e}"))?;
        let raw: Json = serde_json::from_str(&text).map_err(|e| format!("parse model: {e}"))?;
        if raw.get("schema").and_then(|v| v.as_str()) != Some(MODEL_SCHEMA) {
            return Err("model schema mismatch".to_owned());
        }
        let body = raw.get("body").cloned().ok_or("model has no body")?;
        let mut ratios = BTreeMap::new();
        if let Some(table) = body.get("byte_ratio").and_then(|v| v.as_object()) {
            for (dtype, per) in table {
                if let Some(per) = per.as_object() {
                    for (strategy, entry) in per {
                        if let Some(value) = entry.get("median").and_then(|v| v.as_f64()) {
                            ratios.insert((dtype.clone(), strategy.clone()), value);
                        }
                    }
                }
            }
        }
        let mut encode = BTreeMap::new();
        if let Some(table) = body.get("encode_ns_per_mib").and_then(|v| v.as_object()) {
            for (strategy, entry) in table {
                if let Some(value) = entry.get("value").and_then(|v| v.as_f64()) {
                    encode.insert(strategy.clone(), value);
                }
            }
        }
        let mut decode = BTreeMap::new();
        if let Some(table) = body.get("decode_ns_per_mib").and_then(|v| v.as_object()) {
            for (strategy, entry) in table {
                if let Some(value) = entry.get("value").and_then(|v| v.as_f64()) {
                    decode.insert(strategy.clone(), value);
                }
            }
        }
        let publish = body
            .get("publish_ns_per_mib")
            .and_then(|v| v.as_f64())
            .ok_or("model has no publish rate")?;
        let restore_overhead = body
            .get("restore_overhead_ns")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let write_overhead = body
            .get("write_overhead_ns")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        // The digest is over the canonical compact body, exactly what the fit
        // wrote; a model file whose declared digest does not match is refused.
        let body_text = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        let sha256 = sha256_hex(body_text.as_bytes());
        if let Some(declared) = raw.get("sha256").and_then(|v| v.as_str()) {
            if declared != sha256 {
                return Err(format!(
                    "model digest mismatch: declared {declared}, computed {sha256}"
                ));
            }
        }
        Ok(Model {
            raw,
            ratios,
            encode,
            decode,
            publish,
            restore_overhead,
            write_overhead,
            sha256,
        })
    }

    fn ratio(&self, dtype: &str, strategy: &str) -> Option<f64> {
        self.ratios
            .get(&(dtype.to_owned(), strategy.to_owned()))
            .copied()
    }

    fn encode_ns(&self, strategy: &str) -> Option<f64> {
        self.encode.get(strategy).copied()
    }

    fn decode_ns(&self, strategy: &str) -> Option<f64> {
        self.decode.get(strategy).copied()
    }
}

struct Prediction {
    strategy: String,
    bytes: f64,
    write_ns: f64,
    restore_ns: f64,
    /// Per-strategy predicted bytes, for diagnostics.
    candidates: BTreeMap<String, f64>,
}

/// The product's own eligibility rule for mode 9: the exact mode-9 frame must
/// be at least 1/64 smaller than the level-3 ZSTD frame, or the planner does not
/// offer it (`src/tensor_store.rs`, the `1/64 rule`). A predictor that ignores
/// this rule predicts decisions the shipped planner can never take; the first
/// held-out pass of this court made exactly that mistake and is retained.
fn mode9_eligible(mode9_bytes: f64, zstd_bytes: Option<f64>) -> bool {
    match zstd_bytes {
        Some(zstd) => mode9_bytes * 64.0 <= zstd * 63.0,
        // No level-3 prediction means the rule cannot exclude it.
        None => true,
    }
}

fn predict(model: &Model, dtype: &str, logical_bytes: u64, lambda_ns: f64) -> Option<Prediction> {
    let mib = logical_bytes as f64 / MIB;
    // Predict every offered size first so eligibility can be evaluated against
    // the level-3 prediction, exactly as the planner does.
    let mut predicted_bytes: BTreeMap<&'static str, f64> = BTreeMap::new();
    for strategy in applicability(dtype, logical_bytes) {
        if let Some(ratio) = model.ratio(dtype, strategy) {
            predicted_bytes.insert(strategy, ratio * logical_bytes as f64);
        }
    }
    let zstd_bytes = predicted_bytes.get("zstd").copied();
    let mut candidates = BTreeMap::new();
    let mut best: Option<(String, f64, f64, f64, f64)> = None;
    for (strategy, bytes) in predicted_bytes {
        if strategy == "float-tensor" && !mode9_eligible(bytes, zstd_bytes) {
            continue;
        }
        let Some(encode) = model.encode_ns(strategy) else {
            continue;
        };
        let Some(decode) = model.decode_ns(strategy) else {
            continue;
        };
        candidates.insert(strategy.to_owned(), bytes);
        let stored_mib = bytes / MIB;
        let write_ns = encode * mib + model.publish * stored_mib + model.write_overhead;
        let restore_ns = decode * mib + model.restore_overhead;
        let price = bytes + lambda_ns * restore_ns + lambda_ns * write_ns;
        let better = match &best {
            None => true,
            Some((_, _, _, _, best_price)) => price < *best_price,
        };
        if better {
            best = Some((strategy.to_owned(), bytes, write_ns, restore_ns, price));
        }
    }
    best.map(|(strategy, bytes, write_ns, restore_ns, _)| Prediction {
        strategy,
        bytes,
        write_ns,
        restore_ns,
        candidates,
    })
}

// ---------------------------------------------------------------------------
// one family run
// ---------------------------------------------------------------------------

struct TensorRow {
    name: String,
    dtype: String,
    shape: Vec<u64>,
    logical_bytes: u64,
    source_sha256: String,
    prediction: Option<Prediction>,
    candidates: Vec<(String, u64)>,
    candidate_detail: Vec<Json>,
    planned_chosen: String,
    planned_chosen_bytes: u64,
    written_strategy: String,
    written_bytes: u64,
    write_wall_ns: u64,
    encode_ns: u64,
    publish_ns: u64,
    restore_ns: u64,
    restore_cpu_ns: u64,
    restored_sha256: String,
    exact: bool,
    two_stage_analysis: Option<Json>,
}

struct FamilyRun {
    rows: Vec<TensorRow>,
    open_ms: f64,
    family_restore_ns: u64,
    fadvised_files: usize,
    measured_write_ns: u64,
}

fn new_store(work: &Path) -> Result<Store, String> {
    if work.exists() {
        return Err(format!(
            "{} already exists (refusing to reuse state)",
            work.display()
        ));
    }
    let mut options = Options::default();
    options.chunk_size = 1024 * 1024;
    Store::init(work, options).map_err(|e| format!("init store: {e}"))
}

fn run_family(
    args: &Args,
    file: &SafetensorsFile,
    model: Option<&Model>,
) -> Result<FamilyRun, String> {
    let selected = select_tensors(
        file,
        args.max_tensor_bytes,
        args.max_tensors,
        args.max_total_bytes,
    );
    if selected.is_empty() {
        return Err("no tensor satisfies the declared bounds".to_owned());
    }
    let mut store = new_store(&args.work)?;
    let root = store.resolve_path("/").map_err(|e| e.to_string())?;
    let dir = store
        .mkdir(root, b"tensors", 0o755, 0, 0)
        .map_err(|e| format!("mkdir: {e}"))?;

    let layout = TensorLayoutSpec::row_major();
    let mut write_spec = TensorWriteSpec::default();
    write_spec.plan.planner = PlannerMode::TwoStage;
    write_spec.plan.max_candidates = 64;

    let mut plan_options = TensorPlanOptions::default();
    plan_options.planner = PlannerMode::Exhaustive;
    plan_options.max_candidates = 64;
    plan_options.semantic_search = false;

    weftfs::phase_profile::begin();

    let mut rows: Vec<TensorRow> = Vec::new();
    let mut ids: Vec<weftfs::Hash32> = Vec::new();
    let mut measured_write_ns = 0u64;
    for (order, index) in selected.iter().enumerate() {
        let tensor = &file.tensors[*index];
        let bytes = file
            .read_tensor(tensor)
            .map_err(|e| format!("read {}: {e}", tensor.name))?;
        let logical = bytes.len() as u64;
        let source_sha256 = sha256_hex(&bytes);

        // Prediction first: model output only, before any candidate is encoded.
        let prediction = model.and_then(|m| predict(m, &tensor.dtype, logical, 0.0));

        // Exhaustive plan: the ground-truth candidate set and the shipped choice.
        let plan = store
            .plan_tensor(
                &tensor.dtype,
                &tensor.shape,
                &bytes,
                &layout,
                &[],
                &plan_options,
            )
            .map_err(|e| format!("plan {}: {e}", tensor.name))?;
        let candidates: Vec<(String, u64)> = plan
            .candidates
            .iter()
            .filter(|c| c.applicable)
            .filter_map(|c| {
                c.physical_bytes
                    .map(|b| (c.strategy.as_str().to_owned(), b))
            })
            .collect();
        let candidate_detail: Vec<Json> = plan
            .candidates
            .iter()
            .map(|c| {
                json!({
                    "strategy": c.strategy.as_str(),
                    "applicable": c.applicable,
                    "physical_bytes": c.physical_bytes,
                    "detail": c.detail,
                })
            })
            .collect();
        let two_stage_analysis = plan.planner.as_ref().map(|trace| {
            json!({
                "mode": trace.mode.as_str(),
                "zstd3": trace.analysis.zstd3.as_ref().map(estimate_json),
                "zstd19": trace.analysis.zstd19.as_ref().map(estimate_json),
                "mode9_exact_bytes": trace.analysis.mode9.as_ref().map(|m| m.exact_bytes),
                "actual_zstd3": trace.actual_zstd3,
                "actual_zstd19": trace.actual_zstd19,
                "actual_mode9": trace.actual_mode9,
                "violations": trace.prediction_violations.len(),
                "fallback_exhaustive": trace.fallback_exhaustive,
                "decision": {
                    "need_zstd3": trace.decision.need_zstd3,
                    "zstd3_skip_bound": trace.decision.zstd3_skip_bound,
                    "mode9": format!("{:?}", trace.decision.mode9),
                },
            })
        });

        // Product write: the shipped planned writer, measured end to end.
        let before_phases = weftfs::phase_profile::snapshot();
        let started = Instant::now();
        let id = store
            .write_tensor_file_with_spec(
                dir,
                format!("{order:04}-{}", tensor.name.replace('/', "_")).as_bytes(),
                &tensor.dtype,
                &tensor.shape,
                &bytes,
                &layout,
                &write_spec,
            )
            .map_err(|e| format!("write {}: {e}", tensor.name))?;
        let write_wall_ns = started.elapsed().as_nanos() as u64;
        measured_write_ns += write_wall_ns;
        let diff = weftfs::phase_profile::snapshot().since(&before_phases);
        let encode_ns = diff
            .phases
            .get("encode")
            .map(|p| p.total_ns as u64)
            .unwrap_or(0);
        let publish_ns = diff
            .phases
            .get("blob_publish")
            .map(|p| p.total_ns as u64)
            .unwrap_or(0);
        let meta = store.tensor_metadata(id).map_err(|e| e.to_string())?;
        // `object_meta(id).size` is the logical size; the frame bytes of the
        // chosen representation are the encoded bytes the planner measured on
        // these exact bytes (same encoders, same input), and the manifest the
        // store published names the same frame.
        let written_bytes = candidates
            .iter()
            .find(|(strategy, _)| *strategy == meta.strategy)
            .map(|(_, bytes)| *bytes)
            .unwrap_or(plan.chosen_physical_bytes);
        ids.push(id);
        rows.push(TensorRow {
            name: tensor.name.clone(),
            dtype: tensor.dtype.clone(),
            shape: tensor.shape.clone(),
            logical_bytes: logical,
            source_sha256,
            prediction,
            candidates,
            candidate_detail,
            planned_chosen: plan.chosen.as_str().to_owned(),
            planned_chosen_bytes: plan.chosen_physical_bytes,
            written_strategy: meta.strategy.clone(),
            written_bytes,
            write_wall_ns,
            encode_ns,
            publish_ns,
            restore_ns: 0,
            restore_cpu_ns: 0,
            restored_sha256: String::new(),
            exact: false,
            two_stage_analysis,
        });
        drop(bytes);
    }

    // Cold restore: close, drop the page cache for exactly these files, reopen
    // in a read-only handle. Store open is reported separately and excluded
    // from the family restore figure (both sides of the prediction).
    drop(store);
    let advised = fadvise_dontneed(&args.work);
    let open_started = Instant::now();
    let store = Store::open(&args.work, OpenMode::ReadOnly).map_err(|e| e.to_string())?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;
    let mut family_restore_ns = 0u64;
    for (row, id) in rows.iter_mut().zip(ids.iter()) {
        let cpu_before = cpu_ns();
        let started = Instant::now();
        let restored = store.read_tensor(*id).map_err(|e| e.to_string())?;
        let elapsed = started.elapsed().as_nanos() as u64;
        row.restore_cpu_ns = cpu_ns().saturating_sub(cpu_before);
        row.restore_ns = elapsed;
        family_restore_ns += elapsed;
        row.restored_sha256 = sha256_hex(&restored);
        row.exact =
            row.restored_sha256 == row.source_sha256 && restored.len() as u64 == row.logical_bytes;
        if !row.exact {
            return Err(format!("restore mismatch for {}", row.name));
        }
    }
    Ok(FamilyRun {
        rows,
        open_ms,
        family_restore_ns,
        fadvised_files: advised,
        measured_write_ns,
    })
}

fn estimate_json(estimate: &weftfs::two_stage::SampleEstimate) -> Json {
    json!({
        "method": estimate.method,
        "level": estimate.level,
        "predicted": estimate.predicted,
        "lower": estimate.lower,
        "upper": estimate.upper,
        "sampled_bytes": estimate.sampled_bytes,
        "sampled_chunks": estimate.sampled_chunks,
        "total_chunks": estimate.total_chunks,
    })
}

// ---------------------------------------------------------------------------
// fit
// ---------------------------------------------------------------------------

fn fit(fit_families: &[String], runs: &[FamilyRun]) -> Result<Json, String> {
    let mut ratio_obs: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    let mut encode_obs: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    let mut decode_obs: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    let mut publish_obs: (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    let mut restore_residuals: Vec<f64> = Vec::new();

    for run in runs {
        for row in &run.rows {
            for (strategy, bytes) in &row.candidates {
                if row.logical_bytes == 0 {
                    continue;
                }
                ratio_obs
                    .entry((row.dtype.clone(), strategy.clone()))
                    .or_default()
                    .push(*bytes as f64 / row.logical_bytes as f64);
            }
            let mib = row.logical_bytes as f64 / MIB;
            encode_obs
                .entry(row.written_strategy.clone())
                .or_default()
                .0
                .push(mib);
            encode_obs
                .entry(row.written_strategy.clone())
                .or_default()
                .1
                .push(row.encode_ns as f64);
            decode_obs
                .entry(row.written_strategy.clone())
                .or_default()
                .0
                .push(mib);
            decode_obs
                .entry(row.written_strategy.clone())
                .or_default()
                .1
                .push(row.restore_ns as f64);
            publish_obs.0.push(row.written_bytes as f64 / MIB);
            publish_obs.1.push(row.publish_ns as f64);
        }
    }

    let mut byte_ratio = Map::new();
    for ((dtype, strategy), mut values) in ratio_obs {
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let entry = json!({
            "n": values.len(),
            "median": median(&values),
            "p05": percentile(&values, 0.05),
            "p95": percentile(&values, 0.95),
        });
        byte_ratio
            .entry(dtype)
            .or_insert_with(|| Json::Object(Map::new()))
            .as_object_mut()
            .unwrap()
            .insert(strategy, entry);
    }

    let mut encode_ns_per_mib = Map::new();
    for (strategy, (xs, ys)) in &encode_obs {
        if let Some(slope) = slope_through_origin(xs, ys) {
            encode_ns_per_mib.insert(strategy.clone(), json!({"value": slope, "n": xs.len()}));
        }
    }
    let mut decode_ns_per_mib = Map::new();
    let mut decode_slopes: BTreeMap<String, f64> = BTreeMap::new();
    for (strategy, (xs, ys)) in &decode_obs {
        if let Some(slope) = slope_through_origin(xs, ys) {
            decode_ns_per_mib.insert(strategy.clone(), json!({"value": slope, "n": xs.len()}));
            decode_slopes.insert(strategy.clone(), slope);
        }
    }
    for row in runs.iter().flat_map(|r| r.rows.iter()) {
        if let Some(slope) = decode_slopes.get(&row.written_strategy) {
            let predicted = slope * row.logical_bytes as f64 / MIB;
            restore_residuals.push(row.restore_ns as f64 - predicted);
        }
    }
    restore_residuals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let restore_overhead = if restore_residuals.is_empty() {
        0.0
    } else {
        median(&restore_residuals).max(0.0)
    };
    let publish_ns_per_mib = slope_through_origin(&publish_obs.0, &publish_obs.1).unwrap_or(0.0);
    let mut write_residuals: Vec<f64> = Vec::new();
    for row in runs.iter().flat_map(|r| r.rows.iter()) {
        let overhead = row.write_wall_ns as f64 - (row.encode_ns + row.publish_ns) as f64;
        write_residuals.push(overhead);
    }
    write_residuals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let write_overhead = if write_residuals.is_empty() {
        0.0
    } else {
        median(&write_residuals).max(0.0)
    };

    let body = json!({
        "fit_families": fit_families,
        "fit_tensors": runs.iter().map(|r| r.rows.len()).sum::<usize>(),
        "byte_ratio": byte_ratio,
        "encode_ns_per_mib": encode_ns_per_mib,
        "decode_ns_per_mib": decode_ns_per_mib,
        "publish_ns_per_mib": publish_ns_per_mib,
        "restore_overhead_ns": restore_overhead,
        "write_overhead_ns": write_overhead,
        "method": {
            "byte_ratio": "median of actual_frame_bytes / logical_bytes over every candidate that was encoded in the FIT split",
            "encode": "least-squares slope through the origin of measured encode phase ns against logical MiB, per chosen strategy",
            "decode": "least-squares slope through the origin of measured cold read ns against logical MiB, per chosen strategy",
            "publish": "least-squares slope through the origin of measured blob-publish ns against stored MiB, pooled",
            "restore_overhead": "median residual (read ns - slope*MiB), clamped at zero",
            "write_overhead": "median residual (per-object write wall - encode - publish), clamped at zero",
        },
    });
    Ok(json!({
        "schema": MODEL_SCHEMA,
        "body": body,
    }))
}

// ---------------------------------------------------------------------------
// predict reporting
// ---------------------------------------------------------------------------

fn family_json(run: &FamilyRun, model: &Model) -> Json {
    let mut bytes_cond: Vec<(f64, f64)> = Vec::new();
    let mut bytes_e2e: Vec<(f64, f64)> = Vec::new();
    let mut write_cond: Vec<(f64, f64)> = Vec::new();
    let mut write_e2e: Vec<(f64, f64)> = Vec::new();
    let mut restore_cond: Vec<(f64, f64)> = Vec::new();
    let mut restore_e2e: Vec<(f64, f64)> = Vec::new();
    let mut cpu_cond: Vec<(f64, f64)> = Vec::new();
    let mut predicted_write_total = 0.0;
    let mut predicted_restore_total = 0.0;
    let mut measured_write_total = 0.0;
    let mut measured_restore_total = 0.0;
    let mut agreement = 0usize;
    let mut predicted_rows = 0usize;
    let mut strategy_missing = 0usize;
    let mut inapplicable_prediction = 0usize;

    for row in &run.rows {
        let mib = row.logical_bytes as f64 / MIB;
        measured_write_total += row.write_wall_ns as f64;
        measured_restore_total += row.restore_ns as f64;
        let measured_strategy = row.written_strategy.clone();
        // Conditional accuracy: the model's rate and size at the strategy that
        // was actually chosen. This isolates size/rate error from choice error.
        if let Some(ratio) = model.ratio(&row.dtype, &measured_strategy) {
            let predicted_bytes = ratio * row.logical_bytes as f64;
            bytes_cond.push((predicted_bytes, row.written_bytes as f64));
        }
        if let (Some(encode), Some(ratio)) = (
            model.encode_ns(&measured_strategy),
            model.ratio(&row.dtype, &measured_strategy),
        ) {
            let predicted = encode * mib + model.publish * ratio * mib + model.write_overhead;
            write_cond.push((predicted, row.write_wall_ns as f64));
        }
        if let Some(decode) = model.decode_ns(&measured_strategy) {
            let predicted = decode * mib + model.restore_overhead;
            restore_cond.push((predicted, row.restore_ns as f64));
            cpu_cond.push((predicted, row.restore_cpu_ns as f64));
        }
        // End-to-end: the model's own choice, formed before any measurement.
        let Some(prediction) = &row.prediction else {
            continue;
        };
        predicted_rows += 1;
        if !row
            .candidates
            .iter()
            .any(|(strategy, _)| *strategy == prediction.strategy)
        {
            inapplicable_prediction += 1;
        }
        if prediction.strategy == measured_strategy {
            agreement += 1;
        } else if model.ratio(&row.dtype, &prediction.strategy).is_none() {
            strategy_missing += 1;
        }
        predicted_write_total += prediction.write_ns;
        predicted_restore_total += prediction.restore_ns;
        bytes_e2e.push((prediction.bytes, row.written_bytes as f64));
        write_e2e.push((prediction.write_ns, row.write_wall_ns as f64));
        restore_e2e.push((prediction.restore_ns, row.restore_ns as f64));
    }

    json!({
        "tensors": run.rows.len(),
        "predicted_rows": predicted_rows,
        "exact_restores": run.rows.iter().filter(|r| r.exact).count(),
        "measured_write_ms": measured_write_total / 1e6,
        "measured_family_restore_ms": run.family_restore_ns as f64 / 1e6,
        "measured_restore_sum_ms": measured_restore_total / 1e6,
        "store_open_ms": run.open_ms,
        "fadvised_files": run.fadvised_files,
        "predicted_write_ms": predicted_write_total / 1e6,
        "predicted_restore_ms": predicted_restore_total / 1e6,
        "strategy_agreement": if predicted_rows > 0 {
            agreement as f64 / predicted_rows as f64
        } else { 0.0 },
        "strategy_missing_from_model": strategy_missing,
        "predicted_strategy_not_applicable": inapplicable_prediction,
        "errors": {
            "bytes_conditional": error_block(&bytes_cond),
            "bytes_end_to_end": error_block(&bytes_e2e),
            "write_wall_conditional": error_block(&write_cond),
            "write_wall_end_to_end": error_block(&write_e2e),
            "restore_wall_conditional": error_block(&restore_cond),
            "restore_wall_end_to_end": error_block(&restore_e2e),
            "decode_cpu_conditional": error_block(&cpu_cond),
        },
    })
}

fn sla_json(predicted_restore_ms: f64, measured_restore_ms: f64) -> Json {
    if predicted_restore_ms <= 0.0 {
        return json!({
            "rule": "accept iff predicted <= 0.90 * threshold",
            "accepted": 0,
            "missed": 0,
            "miss_rate": 0.0,
            "note": "no prediction was formed for any tensor in this family; nothing is admitted",
            "rows": [],
        });
    }
    let mut rows = Vec::new();
    let mut accepted = 0usize;
    let mut missed = 0usize;
    for multiplier in SLA_MULTIPLIERS {
        let threshold = predicted_restore_ms * multiplier;
        let accepted_here = predicted_restore_ms <= SLA_MARGIN * threshold;
        let miss = accepted_here && measured_restore_ms > threshold;
        if accepted_here {
            accepted += 1;
            if miss {
                missed += 1;
            }
        }
        rows.push(json!({
            "sla_ms": threshold,
            "multiplier_of_prediction": multiplier,
            "accepted": accepted_here,
            "measured_ms": measured_restore_ms,
            "miss": miss,
        }));
    }
    json!({
        "rule": "accept iff predicted <= 0.90 * threshold",
        "accepted": accepted,
        "missed": missed,
        "miss_rate": if accepted > 0 { missed as f64 / accepted as f64 } else { 0.0 },
        "rows": rows,
    })
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    if let Err(err) = real_main() {
        eprintln!("heldout_planner: {err}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), String> {
    let args = parse_args()?;
    let started = Instant::now();
    let stamp = utc_stamp();

    if args.mode == "fit" {
        // FIT: one or more whole families, each measured exactly like a
        // prediction family, then pooled into one model.
        let specs: Vec<(String, PathBuf)> = if args.fit_specs.is_empty() {
            if args.family.is_empty() || args.container.as_os_str().is_empty() {
                return Err("fit needs --fit name=path (or --family and --container)".to_owned());
            }
            vec![(args.family.clone(), args.container.clone())]
        } else {
            args.fit_specs.clone()
        };
        let mut families_json = Vec::new();
        let mut all_runs: Vec<FamilyRun> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for (index, (name, path)) in specs.iter().enumerate() {
            let file = SafetensorsFile::parse(path)
                .map_err(|e| format!("parse {}: {e}", path.display()))?;
            let mut family_args = Args {
                mode: args.mode.clone(),
                family: name.clone(),
                container: path.clone(),
                fit_specs: Vec::new(),
                out: args.out.clone(),
                model_in: None,
                model_out: None,
                work: args.work.with_extension(format!("store-{index}")),
                max_tensor_bytes: args.max_tensor_bytes,
                max_tensors: args.max_tensors,
                max_total_bytes: args.max_total_bytes,
            };
            family_args.work = args.work.with_file_name(format!(
                "{}-store-{index}",
                args.work.file_name().unwrap_or_default().to_string_lossy()
            ));
            let run = run_family(&family_args, &file, None)?;
            families_json.push(json!({
                "family": name,
                "container": {
                    "path": path.display().to_string(),
                    "file_bytes": file.file_bytes,
                    "sha256": sha256_hex(&file.sha256),
                    "tensors_in_container": file.tensors.len(),
                },
                "selected": run.rows.len(),
                "logical_bytes": run.rows.iter().map(|r| r.logical_bytes).sum::<u64>(),
                "names": run.rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
                "tensors": run.rows.iter().map(row_json).collect::<Vec<_>>(),
                "store_open_ms": run.open_ms,
                "measured_family_restore_ms": run.family_restore_ns as f64 / 1e6,
            }));
            let _ = fs::remove_dir_all(&family_args.work);
            all_runs.push(run);
            names.push(name.clone());
        }
        let model_value = fit(&names, &all_runs)?;
        let body = model_value.get("body").cloned().unwrap();
        let model_sha = sha256_hex(model_json(&body).as_bytes());
        let mut complete = model_value;
        complete
            .as_object_mut()
            .unwrap()
            .insert("sha256".to_owned(), json!(model_sha));
        if let Some(path) = &args.model_out {
            write_json(path, &complete)?;
        }
        let receipt = json!({
            "schema": SCHEMA,
            "stamp": stamp,
            "mode": "fit",
            "families": families_json,
            "subset": {
                "max_tensor_bytes": args.max_tensor_bytes,
                "max_tensors": args.max_tensors,
                "max_total_bytes": args.max_total_bytes,
                "rule": "every tensor with byte_len <= the declared bound, strided by the smallest k that keeps the subset within the declared tensor and byte bounds",
            },
            "court": {
                "limits": "3g memory / 3g swap / 1024 pids / 4 cpus",
                "host": uname(),
                "kernel": fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_owned(),
                "peak_rss_kib": peak_rss_kib(),
            },
            "model": complete,
            "model_sha256": model_sha,
            "exactness": {
                "tensors": all_runs.iter().map(|r| r.rows.len()).sum::<usize>(),
                "exact": all_runs.iter().flat_map(|r| r.rows.iter()).filter(|r| r.exact).count(),
                "mismatches": all_runs.iter().flat_map(|r| r.rows.iter()).filter(|r| !r.exact).count(),
            },
            "checks": {
                "all_arms_exact": all_runs.iter().flat_map(|r| r.rows.iter()).all(|r| r.exact),
                "model_written": args.model_out.is_some(),
            },
            "elapsed_ms": started.elapsed().as_millis() as u64,
        });
        if let Some(parent) = args.out.parent() {
            let _ = fs::create_dir_all(parent);
        }
        write_json(&args.out, &receipt)?;
        println!(
            "heldout_planner: fit {} families={} tensors={} elapsed_ms={} out={}",
            names.join("+"),
            names.len(),
            all_runs.iter().map(|r| r.rows.len()).sum::<usize>(),
            started.elapsed().as_millis(),
            args.out.display()
        );
        // Each family's store directory was removed by `run_family`; the
        // enclosing work directory holds the model file and is kept for the
        // next phase. The court's trap removes it at the end.
        return Ok(());
    }

    if args.family.is_empty() || args.container.as_os_str().is_empty() {
        return Err("predict needs --family and --container".to_owned());
    }
    let file = SafetensorsFile::parse(&args.container)
        .map_err(|e| format!("parse {}: {e}", args.container.display()))?;
    let container_sha256 = sha256_hex(&file.sha256);
    let model = Model::load(args.model_in.as_ref().ok_or("--model-in is required")?)?;

    let run = run_family(&args, &file, Some(&model))?;

    let family = family_json(&run, &model);
    let predicted_restore_ms = family["predicted_restore_ms"].as_f64().unwrap_or(0.0);
    let measured_restore_ms = family["measured_family_restore_ms"]
        .as_f64()
        .unwrap_or(0.0);
    let sla = sla_json(predicted_restore_ms, measured_restore_ms);
    let bytes_median = family["errors"]["bytes_conditional"]["abs_rel"]["median"]
        .as_f64()
        .unwrap_or(f64::INFINITY);
    let bytes_median_e2e = family["errors"]["bytes_end_to_end"]["abs_rel"]["median"]
        .as_f64()
        .unwrap_or(f64::INFINITY);
    let agreement = family["strategy_agreement"].as_f64().unwrap_or(0.0);
    let checks = json!({
        "model_sha256": model.sha256,
        "model_schema": model.raw.get("schema").cloned(),
        "bytes_prediction_median_abs_rel_le_0.10": bytes_median <= 0.10,
        "bytes_prediction_end_to_end_median_abs_rel_le_0.10": bytes_median_e2e <= 0.10,
        "strategy_agreement_ge_0.90": agreement >= 0.90,
        "restore_prediction_median_abs_rel_le_0.20": family["errors"]["restore_wall_end_to_end"]["abs_rel"]["median"]
            .as_f64()
            .map(|v| v <= 0.20)
            .unwrap_or(false),
        "sla_misses_reported": true,
        "all_arms_exact": run.rows.iter().all(|r| r.exact),
    });
    let receipt = json!({
        "schema": SCHEMA,
        "stamp": stamp,
        "mode": "predict",
        "family": args.family,
        "container": {
            "path": args.container.display().to_string(),
            "file_bytes": file.file_bytes,
            "sha256": container_sha256,
            "tensors_in_container": file.tensors.len(),
        },
        "subset": {
            "rule": "every tensor with byte_len <= the declared bound, strided by the smallest k that keeps the subset within the declared tensor and byte bounds",
            "max_tensor_bytes": args.max_tensor_bytes,
            "max_tensors": args.max_tensors,
            "max_total_bytes": args.max_total_bytes,
            "selected": run.rows.len(),
            "logical_bytes": run.rows.iter().map(|r| r.logical_bytes).sum::<u64>(),
            "names": run.rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
        },
        "court": {
            "limits": "3g memory / 3g swap / 1024 pids / 4 cpus",
            "host": uname(),
            "kernel": fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_owned(),
            "peak_rss_kib": peak_rss_kib(),
            "lambda_ns": 0,
            "prediction_objective": "bytes-only (the shipped default); latency lambda is a secondary row in RESULTS",
        },
        "model_sha256": model.sha256,
        "exactness": {
            "tensors": run.rows.len(),
            "exact": run.rows.iter().filter(|r| r.exact).count(),
            "mismatches": run.rows.iter().filter(|r| !r.exact).count(),
        },
        "family_summary": family,
        "sla": sla,
        "checks": checks,
        "tensors": run.rows.iter().map(row_json).collect::<Vec<_>>(),
        "elapsed_ms": started.elapsed().as_millis() as u64,
    });
    if let Some(parent) = args.out.parent() {
        let _ = fs::create_dir_all(parent);
    }
    write_json(&args.out, &receipt)?;
    println!(
        "heldout_planner: predict {} tensors={} elapsed_ms={} out={}",
        args.family,
        run.rows.len(),
        started.elapsed().as_millis(),
        args.out.display()
    );
    let _ = fs::remove_dir_all(&args.work);
    Ok(())
}

fn row_json(row: &TensorRow) -> Json {
    json!({
        "name": row.name,
        "dtype": row.dtype,
        "shape": row.shape,
        "logical_bytes": row.logical_bytes,
        "source_sha256": row.source_sha256,
        "restored_sha256": row.restored_sha256,
        "exact": row.exact,
        "candidates": row.candidates.iter().map(|(s, b)| json!({"strategy": s, "bytes": b})).collect::<Vec<_>>(),
        "candidate_detail": row.candidate_detail,
        "planned_chosen": row.planned_chosen,
        "planned_chosen_bytes": row.planned_chosen_bytes,
        "written_strategy": row.written_strategy,
        "written_bytes": row.written_bytes,
        "write_wall_ns": row.write_wall_ns,
        "encode_ns": row.encode_ns,
        "publish_ns": row.publish_ns,
        "restore_ns": row.restore_ns,
        "restore_cpu_ns": row.restore_cpu_ns,
        "prediction": row.prediction.as_ref().map(|p| json!({
            "strategy": p.strategy,
            "bytes": p.bytes,
            "write_ns": p.write_ns,
            "restore_ns": p.restore_ns,
            "candidates": p.candidates,
        })),
        "two_stage_analysis": row.two_stage_analysis,
    })
}

fn uname() -> String {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return "unknown".to_owned();
    }
    let field = |bytes: &[libc::c_char]| -> String {
        let end = bytes.iter().position(|c| *c == 0).unwrap_or(bytes.len());
        bytes[..end]
            .iter()
            .map(|c| *c as u8 as char)
            .collect::<String>()
    };
    format!(
        "{} {} {}",
        field(&uts.sysname),
        field(&uts.release),
        field(&uts.machine)
    )
}

// Keep the imports honest on platforms where a helper is unused.
#[allow(dead_code)]
fn _unused(_: &Duration, _: &mut File, _: BTreeSet<String>) {}
