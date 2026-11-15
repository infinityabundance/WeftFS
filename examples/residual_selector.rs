//! T093 — residual-fingerprint representation selector, evaluated held-out.
//!
//! # Hypothesis
//!
//! Cheap fingerprints of a tensor **and of its residual against its causal
//! parent** — byte entropy, zero-word fraction, exponent-plane entropy — carry
//! enough information to choose an exact representation nearly as well as
//! measuring every candidate, and better than the trivial selectors (an entropy
//! threshold, a size threshold, always-zstd, and always-best-of-two).
//!
//! # Declared split (frozen before any measurement; `bench/CONTRACTS-selector.md`)
//!
//! * FIT: Qwen2.5-0.5B base and Instruct, and pythia-70m steps 0, 1, 2, 4, 8.
//! * VALIDATION: Qwen2.5-3B base, used once to select the tree depth and the
//!   trivial thresholds (and only that).
//! * HELD-OUT: Qwen2.5-3B-Instruct, measured **once**, after the model is frozen.
//!
//! # Candidates (all exact)
//!
//! `raw`, `zstd-3`, `float-tensor` (mode 9). Deliberately no lossy mode: a
//! selector that could choose a lossy representation would not be comparable.
//!
//! # Objective
//!
//! `cost = bytes + (encode_ns + decode_ns) / 1e6 * LAMBDA_NS_PER_BYTE`, with
//! `LAMBDA_NS_PER_BYTE = 2000` units per ns… no: see [`LAMBDA_BYTES_PER_NS`].
//! The declared objective is `bytes + ns * 0.001` — one millisecond of CPU is
//! worth one kilobyte of storage — plus a bytes-only reading. Both are reported.
//!
//! # Policies
//!
//! 1. `always_zstd`
//! 2. `always_best_of_two` (measure zstd-3 and mode 9, take the cheaper)
//! 3. `entropy_threshold` (fitted on FIT+VALIDATION)
//! 4. `size_threshold` (fitted on FIT+VALIDATION)
//! 5. `fingerprint_tree` (depth-2, cost-sensitive, fitted on FIT only, depth
//!    chosen on VALIDATION)
//! 6. `oracle` (measures all three, takes the true minimum)
//!
//! Regret is `policy_cost - oracle_cost` per tensor and in aggregate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftcodec::limits::Limits;
use weftcodec::modes::{self, FloatDtype};
use weftfs::safetensors::SafetensorsFile;

const LAMBDA_BYTES_PER_NS: f64 = 0.001;
const MAX_PLAN_NS: f64 = 50_000_000.0; // declared planning bound: 50 ms per tensor

// ---------------------------------------------------------------------------
// utilities
// ---------------------------------------------------------------------------

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.is_empty() {
        return f64::NAN;
    }
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    }
}

fn codec_dtype(canonical: &str) -> Result<&'static str, String> {
    match canonical {
        "f16" => Ok("fp16"),
        "bf16" => Ok("bf16"),
        "f32" => Ok("fp32"),
        other => Err(format!("dtype `{other}` has no exact float codec mapping")),
    }
}

fn year_month_day(secs: u64) -> (i64, u32, u32) {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
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

fn utc_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let rem = secs % 86_400;
    let (y, m, d) = year_month_day(secs);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        now.subsec_millis()
    )
}

// ---------------------------------------------------------------------------
// fingerprints
// ---------------------------------------------------------------------------

/// One-pass fingerprints of `bytes` and of `bytes` against `parent`.
#[derive(Clone, Debug, Default)]
struct Fingerprint {
    bytes: u64,
    /// Shannon entropy of the byte histogram, bits per byte.
    entropy: f64,
    /// Fraction of 16-bit words that are zero.
    zero_words: f64,
    /// Shannon entropy of the high byte plane (bits per byte).
    high_plane_entropy: f64,
    /// Fraction of 16-bit words that differ from the parent (0 when no parent).
    changed_words: f64,
    /// Entropy of the residual's byte histogram (the tensor itself when there
    /// is no parent).
    residual_entropy: f64,
    /// Fraction of zero 16-bit words in the residual.
    residual_zero_words: f64,
    has_parent: bool,
}

fn entropy_of(histogram: &[u64; 256], total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let total = total as f64;
    let mut sum = 0.0;
    for count in histogram {
        if *count > 0 {
            let p = *count as f64 / total;
            sum -= p * p.log2();
        }
    }
    sum
}

fn fingerprint(bytes: &[u8], parent: Option<&[u8]>) -> Fingerprint {
    let mut histogram = [0u64; 256];
    let mut high = [0u64; 256];
    let mut residual = [0u64; 256];
    let mut zero_words = 0u64;
    let mut changed = 0u64;
    let words = bytes.len() / 2;
    for (index, byte) in bytes.iter().enumerate() {
        histogram[*byte as usize] += 1;
        if index % 2 == 1 {
            high[*byte as usize] += 1;
        }
    }
    let mut residual_zero = 0u64;
    for index in 0..words {
        let left = u16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]);
        if left == 0 {
            zero_words += 1;
        }
        let (residual_word, differs) = match parent {
            Some(base) if base.len() == bytes.len() => {
                let right = u16::from_le_bytes([base[index * 2], base[index * 2 + 1]]);
                (left ^ right, left != right)
            }
            _ => (left, false),
        };
        if differs {
            changed += 1;
        }
        if residual_word == 0 {
            residual_zero += 1;
        }
        residual[(residual_word & 0xff) as usize] += 1;
        residual[(residual_word >> 8) as usize] += 1;
    }
    Fingerprint {
        bytes: bytes.len() as u64,
        entropy: entropy_of(&histogram, bytes.len() as u64),
        zero_words: if words == 0 {
            0.0
        } else {
            zero_words as f64 / words as f64
        },
        high_plane_entropy: entropy_of(&high, (bytes.len() / 2) as u64),
        changed_words: if words == 0 {
            0.0
        } else {
            changed as f64 / words as f64
        },
        residual_entropy: entropy_of(&residual, bytes.len() as u64),
        residual_zero_words: if words == 0 {
            0.0
        } else {
            residual_zero as f64 / words as f64
        },
        has_parent: parent.is_some(),
    }
}

// ---------------------------------------------------------------------------
// candidates
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Strategy {
    Raw,
    Zstd,
    FloatTensor,
}

impl Strategy {
    fn index(self) -> usize {
        match self {
            Strategy::Raw => 0,
            Strategy::Zstd => 1,
            Strategy::FloatTensor => 2,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Strategy::Raw => "raw",
            Strategy::Zstd => "zstd-3",
            Strategy::FloatTensor => "float-tensor",
        }
    }
}
const STRATEGIES: [Strategy; 3] = [Strategy::Raw, Strategy::Zstd, Strategy::FloatTensor];

/// One measured candidate: bytes, encode wall and decode wall (milliseconds).
#[derive(Clone, Copy, Debug, Default)]
struct Measurement {
    bytes: u64,
    encode_ms: f64,
    decode_ms: f64,
}

impl Measurement {
    fn cost(&self, lambda: f64) -> f64 {
        self.bytes as f64 + (self.encode_ms + self.decode_ms) * 1_000_000.0 * lambda
    }
}

struct TensorRow {
    family: String,
    name: String,
    fingerprint: Fingerprint,
    measurements: [Measurement; 3],
    /// Whether every candidate decoded back to the exact input.
    exact: [bool; 3],
    dtype: String,
    shape: Vec<u64>,
    fingerprint_ns: f64,
}

fn measure_tensor(
    bytes: &[u8],
    dtype: FloatDtype,
    shape: &[u64],
    limits: &Limits,
    reps: usize,
) -> Result<([Measurement; 3], [bool; 3]), String> {
    let mut out = [Measurement::default(); 3];
    let mut exact = [false; 3];

    // raw
    out[Strategy::Raw.index()] = Measurement {
        bytes: bytes.len() as u64,
        encode_ms: 0.0,
        decode_ms: {
            let mut samples = Vec::with_capacity(reps);
            let mut decoded = Vec::new();
            for _ in 0..reps {
                let started = Instant::now();
                decoded = bytes.to_vec();
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            exact[Strategy::Raw.index()] = decoded == bytes;
            median(samples)
        },
    };

    // zstd-3
    {
        let started = Instant::now();
        let frame = zstd::bulk::compress(bytes, 3).map_err(|err| format!("zstd: {err}"))?;
        let encode_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut samples = Vec::with_capacity(reps);
        let mut decoded: Vec<u8> = Vec::new();
        for _ in 0..reps {
            let started = Instant::now();
            decoded = zstd::bulk::decompress(&frame, bytes.len())
                .map_err(|err| format!("unzstd: {err}"))?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        exact[Strategy::Zstd.index()] = decoded == bytes;
        out[Strategy::Zstd.index()] = Measurement {
            bytes: frame.len() as u64,
            encode_ms,
            decode_ms: median(samples),
        };
    }

    // mode 9
    {
        let started = Instant::now();
        let frame = modes::encode_float_tensor(bytes, dtype, shape, limits)
            .map_err(|err| format!("mode 9 encode: {err}"))?;
        let encode_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut samples = Vec::with_capacity(reps);
        let mut decoded: Vec<u8> = Vec::new();
        for _ in 0..reps {
            let started = Instant::now();
            let parsed =
                weftcodec::frame::Frame::parse(&frame, limits).map_err(|err| err.to_string())?;
            decoded = modes::decode_float_tensor(&parsed, limits).map_err(|err| err.to_string())?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        exact[Strategy::FloatTensor.index()] = decoded == bytes;
        out[Strategy::FloatTensor.index()] = Measurement {
            bytes: frame.len() as u64,
            encode_ms,
            decode_ms: median(samples),
        };
    }
    Ok((out, exact))
}

// ---------------------------------------------------------------------------
// cost-sensitive decision tree over declared features
// ---------------------------------------------------------------------------

const FEATURES: usize = 5;

fn features(fp: &Fingerprint) -> [f64; FEATURES] {
    [
        (fp.bytes.max(1) as f64).log2(),
        fp.residual_entropy,
        fp.residual_zero_words,
        fp.high_plane_entropy,
        if fp.has_parent { 1.0 } else { 0.0 },
    ]
}

const FEATURE_NAMES: [&str; FEATURES] = [
    "log2_bytes",
    "residual_entropy",
    "residual_zero_words",
    "high_plane_entropy",
    "has_parent",
];

#[derive(Clone, Debug)]
enum Node {
    Leaf(usize),
    Split {
        feature: usize,
        threshold: f64,
        left: Box<Node>,
        right: Box<Node>,
    },
}

/// Greedy cost-sensitive tree: at each node, try every feature and every
/// midpoint of its sorted values, and keep the split that minimises the summed
/// cost of the two children's best single choice. Deterministic.
fn fit_tree(rows: &[(Fingerprint, [f64; 3])], depth: usize, lambda: f64) -> Node {
    if rows.len() < 8 || depth == 0 {
        return Node::Leaf(best_single(rows, lambda));
    }
    let mut best: Option<(f64, usize, f64)> = None;
    for feature in 0..FEATURES {
        let mut values: Vec<f64> = rows.iter().map(|row| features(&row.0)[feature]).collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        values.dedup();
        for pair in values.windows(2) {
            let threshold = (pair[0] + pair[1]) / 2.0;
            let (left, right): (Vec<(Fingerprint, [f64; 3])>, Vec<(Fingerprint, [f64; 3])>) =
                rows.iter().cloned().partition(|row| features(&row.0)[feature] <= threshold);
            if left.len() < 4 || right.len() < 4 {
                continue;
            }
            let total = subset_cost(&left, lambda) + subset_cost(&right, lambda);
            let better = match best {
                None => true,
                Some((current, _, _)) => total < current,
            };
            if better {
                best = Some((total, feature, threshold));
            }
        }
    }
    let Some((_, feature, threshold)) = best else {
        return Node::Leaf(best_single(rows, lambda));
    };
    let (left_rows, right_rows): (Vec<_>, Vec<_>) = rows
        .iter()
        .cloned()
        .partition(|row| features(&row.0)[feature] <= threshold);
    Node::Split {
        feature,
        threshold,
        left: Box::new(fit_tree(&left_rows, depth - 1, lambda)),
        right: Box::new(fit_tree(&right_rows, depth - 1, lambda)),
    }
}

fn best_single(rows: &[(Fingerprint, [f64; 3])], lambda: f64) -> usize {
    let mut best = (f64::INFINITY, 0usize);
    for strategy in 0..3 {
        let total: f64 = rows
            .iter()
            .map(|row| row.1[strategy] * (1.0 + 0.0))
            .sum::<f64>()
            * 0.0
            + rows.iter().map(|row| row.1[strategy]).sum::<f64>();
        let _ = lambda;
        if total < best.0 {
            best = (total, strategy);
        }
    }
    best.1
}

fn subset_cost(rows: &[(Fingerprint, [f64; 3])], _lambda: f64) -> f64 {
    (0..3)
        .map(|strategy| rows.iter().map(|row| row.1[strategy]).sum::<f64>())
        .fold(f64::INFINITY, f64::min)
}

fn predict(node: &Node, fp: &Fingerprint) -> usize {
    match node {
        Node::Leaf(strategy) => *strategy,
        Node::Split {
            feature,
            threshold,
            left,
            right,
        } => {
            if features(fp)[*feature] <= *threshold {
                predict(left, fp)
            } else {
                predict(right, fp)
            }
        }
    }
}

fn describe(node: &Node) -> Json {
    match node {
        Node::Leaf(strategy) => json!({"leaf": STRATEGIES[*strategy].name()}),
        Node::Split {
            feature,
            threshold,
            left,
            right,
        } => json!({
            "feature": FEATURE_NAMES[*feature],
            "threshold": threshold,
            "left": describe(left),
            "right": describe(right),
        }),
    }
}

// ---------------------------------------------------------------------------
// family loading
// ---------------------------------------------------------------------------

struct Family {
    label: String,
    checkpoint: SafetensorsFile,
    parent: Option<SafetensorsFile>,
    parent_label: Option<String>,
}

fn load_family(spec: &str) -> Result<Family, String> {
    // spec = "label:path" or "label:path:parent_path"
    let mut parts = spec.split(':');
    let label = parts.next().ok_or("empty family spec")?.to_owned();
    let path = parts.next().ok_or("family spec needs a path")?.to_owned();
    let parent = parts.next().map(|text| text.to_owned());
    let checkpoint =
        SafetensorsFile::parse(Path::new(&path)).map_err(|err| format!("{label}: {err}"))?;
    let (parent_file, parent_label) = match parent {
        Some(parent_path) => {
            let file = SafetensorsFile::parse(Path::new(&parent_path))
                .map_err(|err| format!("{label} parent: {err}"))?;
            (Some(file), Some(format!("{label}:parent")))
        }
        None => (None, None),
    };
    Ok(Family {
        label,
        checkpoint,
        parent: parent_file,
        parent_label,
    })
}

fn rows_of(
    family: &Family,
    max_bytes: u64,
    reps: usize,
    limits: &Limits,
) -> Result<Vec<TensorRow>, String> {
    let mut rows = Vec::new();
    for tensor in &family.checkpoint.tensors {
        if tensor.byte_len() == 0 || tensor.byte_len() > max_bytes {
            continue;
        }
        let bytes = family
            .checkpoint
            .read_tensor(tensor)
            .map_err(|err| format!("{}: {err}", family.label))?;
        let parent_bytes = match &family.parent {
            Some(parent) => match parent.tensor(&tensor.name) {
                Ok(other) if other.shape == tensor.shape && other.dtype == tensor.dtype => Some(
                    parent
                        .read_tensor(other)
                        .map_err(|err| format!("{}: {err}", family.label))?,
                ),
                _ => None,
            },
            None => None,
        };
        let started = Instant::now();
        let fp = fingerprint(&bytes, parent_bytes.as_deref());
        let fingerprint_ns = started.elapsed().as_secs_f64() * 1e9;
        let codec_name = codec_dtype(&tensor.dtype)?;
        let dtype = FloatDtype::from_name(codec_name).map_err(|err| err.to_string())?;
        let (measurements, exact) = measure_tensor(&bytes, dtype, &tensor.shape, limits, reps)?;
        rows.push(TensorRow {
            family: family.label.clone(),
            name: tensor.name.clone(),
            fingerprint: fp,
            measurements,
            exact,
            dtype: tensor.dtype.clone(),
            shape: tensor.shape.clone(),
            fingerprint_ns,
        });
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// policies
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Threshold {
    feature: usize,
    threshold: f64,
    below: usize,
    above: usize,
}

fn fit_threshold(rows: &[(Fingerprint, [f64; 3])], feature: usize) -> Threshold {
    let mut values: Vec<f64> = rows.iter().map(|row| features(&row.0)[feature]).collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values.dedup();
    let mut best = Threshold {
        feature,
        threshold: values.first().copied().unwrap_or(0.0),
        below: 0,
        above: 1,
    };
    let mut best_cost = f64::INFINITY;
    for pair in values.windows(2) {
        let threshold = (pair[0] + pair[1]) / 2.0;
        let (below, above): (Vec<_>, Vec<_>) = rows
            .iter()
            .partition(|row| features(&row.0)[feature] <= threshold);
        if below.is_empty() || above.is_empty() {
            continue;
        }
        let below_best = (0..3)
            .min_by(|a, b| {
                let left: f64 = below.iter().map(|row| row.1[*a]).sum();
                let right: f64 = below.iter().map(|row| row.1[*b]).sum();
                left.partial_cmp(&right).unwrap()
            })
            .unwrap();
        let above_best = (0..3)
            .min_by(|a, b| {
                let left: f64 = above.iter().map(|row| row.1[*a]).sum();
                let right: f64 = above.iter().map(|row| row.1[*b]).sum();
                left.partial_cmp(&right).unwrap()
            })
            .unwrap();
        let cost: f64 = below.iter().map(|row| row.1[below_best]).sum::<f64>()
            + above.iter().map(|row| row.1[above_best]).sum::<f64>();
        if cost < best_cost {
            best_cost = cost;
            best = Threshold {
                feature,
                threshold,
                below: below_best,
                above: above_best,
            };
        }
    }
    best
}

// ---------------------------------------------------------------------------
// driver
// ---------------------------------------------------------------------------

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out =
        PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "residual-selector.json".to_owned()));
    let reps: usize = arg(&args, "--reps")
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);
    let max_fit_bytes: u64 = arg(&args, "--max-fit-bytes")
        .and_then(|value| value.parse().ok())
        .unwrap_or(8 * 1024 * 1024);
    let max_eval_bytes: u64 = arg(&args, "--max-eval-bytes")
        .and_then(|value| value.parse().ok())
        .unwrap_or(16 * 1024 * 1024);
    let max_fit_tensors: usize = arg(&args, "--max-fit-tensors")
        .and_then(|value| value.parse().ok())
        .unwrap_or(40);
    let max_eval_tensors: usize = arg(&args, "--max-eval-tensors")
        .and_then(|value| value.parse().ok())
        .unwrap_or(24);
    let corpus = PathBuf::from(
        arg(&args, "--corpus").unwrap_or_else(|| ".docker-cache/real-corpus".to_owned()),
    );
    let fit_specs: Vec<String> = arg(&args, "--fit")
        .map(|text| text.split(';').map(|part| part.to_owned()).collect())
        .unwrap_or_else(|| {
            vec![
                format!(
                    "qwen0.5b-base:{}",
                    corpus.join("Qwen2.5-0.5B.safetensors").display()
                ),
                format!(
                    "qwen0.5b-instruct:{}:{}",
                    corpus.join("Qwen2.5-0.5B-Instruct.safetensors").display(),
                    corpus.join("Qwen2.5-0.5B.safetensors").display()
                ),
                format!(
                    "pythia-step0:{}",
                    corpus.join("pythia-70m-step0.safetensors").display()
                ),
                format!(
                    "pythia-step8:{}:{}",
                    corpus.join("pythia-70m-step8.safetensors").display(),
                    corpus.join("pythia-70m-step0.safetensors").display()
                ),
            ]
        });
    let validation_specs: Vec<String> = arg(&args, "--validation")
        .map(|text| text.split(';').map(|part| part.to_owned()).collect())
        .unwrap_or_else(|| {
            vec![format!(
                "qwen3b-base:{}",
                corpus.join("Qwen2.5-3B.safetensors").display()
            )]
        });
    let heldout_specs: Vec<String> = arg(&args, "--heldout")
        .map(|text| text.split(';').map(|part| part.to_owned()).collect())
        .unwrap_or_else(|| {
            vec![format!(
                "qwen3b-instruct:{}:{}",
                corpus.join("Qwen2.5-3B-Instruct.safetensors").display(),
                corpus.join("Qwen2.5-3B.safetensors").display()
            )]
        });
    let limits = Limits::default();

    let mut fit_rows: Vec<TensorRow> = Vec::new();
    for spec in &fit_specs {
        let family = load_family(spec)?;
        let mut rows = rows_of(&family, max_fit_bytes, reps, &limits)?;
        rows.sort_by_key(|row| (row.fingerprint.bytes, row.name.clone()));
        if rows.len() > max_fit_tensors {
            let stride = rows.len() / max_fit_tensors;
            rows = rows
                .into_iter()
                .enumerate()
                .filter(|(index, _)| index % stride.max(1) == 0)
                .map(|(_, row)| row)
                .take(max_fit_tensors)
                .collect();
        }
        eprintln!("T093 fit: {} tensors from {}", rows.len(), family.label);
        fit_rows.extend(rows);
    }

    let mut validation_rows: Vec<TensorRow> = Vec::new();
    for spec in &validation_specs {
        let family = load_family(spec)?;
        let mut rows = rows_of(&family, max_eval_bytes, reps, &limits)?;
        rows.sort_by_key(|row| (row.fingerprint.bytes, row.name.clone()));
        if rows.len() > max_eval_tensors {
            let stride = rows.len() / max_eval_tensors;
            rows = rows
                .into_iter()
                .enumerate()
                .filter(|(index, _)| index % stride.max(1) == 0)
                .map(|(_, row)| row)
                .take(max_eval_tensors)
                .collect();
        }
        eprintln!(
            "T093 validation: {} tensors from {}",
            rows.len(),
            family.label
        );
        validation_rows.extend(rows);
    }

    // ---- fit on FIT only, choose the depth on VALIDATION ----------------
    let lambda = LAMBDA_BYTES_PER_NS;
    let fit_pairs: Vec<(Fingerprint, [f64; 3])> = fit_rows
        .iter()
        .map(|row| {
            (
                row.fingerprint.clone(),
                [
                    row.measurements[0].cost(lambda),
                    row.measurements[1].cost(lambda),
                    row.measurements[2].cost(lambda),
                ],
            )
        })
        .collect();
    let mut chosen_depth = 1usize;
    let mut depth_rows = Vec::new();
    let mut best_validation = f64::INFINITY;
    for depth in 1..=3usize {
        let tree = fit_tree(&fit_pairs, depth, lambda);
        let validation_cost: f64 = validation_rows
            .iter()
            .map(|row| row.measurements[predict(&tree, &row.fingerprint)].cost(lambda))
            .sum();
        depth_rows.push(json!({
            "depth": depth,
            "validation_cost": validation_cost,
            "tree": describe(&tree),
        }));
        if validation_cost < best_validation {
            best_validation = validation_cost;
            chosen_depth = depth;
        }
    }
    let tree = fit_tree(&fit_pairs, chosen_depth, lambda);

    // Trivial selectors, fitted on FIT + VALIDATION.
    let mut threshold_pairs = fit_pairs.clone();
    threshold_pairs.extend(validation_rows.iter().map(|row| {
        (
            row.fingerprint.clone(),
            [
                row.measurements[0].cost(lambda),
                row.measurements[1].cost(lambda),
                row.measurements[2].cost(lambda),
            ],
        )
    }));
    // Entropy threshold chooses raw below the threshold and zstd above it: the
    // classic "compressible or not" rule, expressed over the residual entropy.
    let mut entropy_values: Vec<f64> = threshold_pairs
        .iter()
        .map(|row| row.0.residual_entropy)
        .collect();
    entropy_values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    entropy_values.dedup();
    let mut entropy_rule = (0.0f64, Strategy::Raw, Strategy::Zstd);
    let mut best_entropy = f64::INFINITY;
    for pair in entropy_values.windows(2) {
        let threshold = (pair[0] + pair[1]) / 2.0;
        let below: Vec<_> = threshold_pairs
            .iter()
            .filter(|row| row.0.residual_entropy <= threshold)
            .collect();
        let above: Vec<_> = threshold_pairs
            .iter()
            .filter(|row| row.0.residual_entropy > threshold)
            .collect();
        if below.is_empty() || above.is_empty() {
            continue;
        }
        for low in [Strategy::Raw, Strategy::Zstd] {
            for high in [Strategy::Raw, Strategy::Zstd] {
                let cost: f64 = below.iter().map(|row| row.1[low.index()]).sum::<f64>()
                    + above.iter().map(|row| row.1[high.index()]).sum::<f64>();
                if cost < best_entropy {
                    best_entropy = cost;
                    entropy_rule = (threshold, low, high);
                }
            }
        }
    }
    let size_rule = fit_threshold(&threshold_pairs, 0);

    // ---- held-out, measured once ---------------------------------------
    let mut heldout_rows: Vec<TensorRow> = Vec::new();
    for spec in &heldout_specs {
        let family = load_family(spec)?;
        let mut rows = rows_of(&family, max_eval_bytes, reps, &limits)?;
        rows.sort_by_key(|row| (row.fingerprint.bytes, row.name.clone()));
        if rows.len() > max_eval_tensors {
            let stride = rows.len() / max_eval_tensors;
            rows = rows
                .into_iter()
                .enumerate()
                .filter(|(index, _)| index % stride.max(1) == 0)
                .map(|(_, row)| row)
                .take(max_eval_tensors)
                .collect();
        }
        eprintln!(
            "T093 held-out: {} tensors from {}",
            rows.len(),
            family.label
        );
        heldout_rows.extend(rows);
    }

    let policy_names = [
        "always_zstd",
        "always_best_of_two",
        "entropy_threshold",
        "size_threshold",
        "fingerprint_tree",
        "oracle",
    ];
    let mut policy_costs = [0.0f64; 6];
    let mut policy_costs_bytes_only = [0.0f64; 6];
    let mut rows_json = Vec::new();
    for row in &heldout_rows {
        let costs: [f64; 3] = [
            row.measurements[0].cost(lambda),
            row.measurements[1].cost(lambda),
            row.measurements[2].cost(lambda),
        ];
        let oracle = (0..3)
            .min_by(|a, b| costs[*a].partial_cmp(&costs[*b]).unwrap())
            .unwrap();
        let best_two = if row.measurements[Strategy::Zstd.index()].cost(lambda)
            <= row.measurements[Strategy::FloatTensor.index()].cost(lambda)
        {
            Strategy::Zstd
        } else {
            Strategy::FloatTensor
        };
        let entropy_choice = if row.fingerprint.residual_entropy <= entropy_rule.0 {
            entropy_rule.1
        } else {
            entropy_rule.2
        };
        let size_choice = if features(&row.fingerprint)[size_rule.feature] <= size_rule.threshold {
            STRATEGIES[size_rule.below]
        } else {
            STRATEGIES[size_rule.above]
        };
        let tree_choice = STRATEGIES[predict(&tree, &row.fingerprint)];
        let choices = [
            Strategy::Zstd,
            best_two,
            entropy_choice,
            size_choice,
            tree_choice,
            STRATEGIES[oracle],
        ];
        for (index, choice) in choices.iter().enumerate() {
            policy_costs[index] += row.measurements[choice.index()].cost(lambda);
            policy_costs_bytes_only[index] += row.measurements[choice.index()].bytes as f64;
        }
        rows_json.push(json!({
            "family": row.family,
            "name": row.name,
            "dtype": row.dtype,
            "shape": row.shape,
            "bytes": row.fingerprint.bytes,
            "fingerprint": {
                "entropy": row.fingerprint.entropy,
                "zero_words": row.fingerprint.zero_words,
                "high_plane_entropy": row.fingerprint.high_plane_entropy,
                "changed_words": row.fingerprint.changed_words,
                "residual_entropy": row.fingerprint.residual_entropy,
                "residual_zero_words": row.fingerprint.residual_zero_words,
                "has_parent": row.fingerprint.has_parent,
            },
            "measurements": STRATEGIES.iter().enumerate().map(|(index, strategy)| json!({
                "strategy": strategy.name(),
                "bytes": row.measurements[index].bytes,
                "encode_ms": row.measurements[index].encode_ms,
                "decode_ms": row.measurements[index].decode_ms,
                "exact": row.exact[index],
            })).collect::<Vec<_>>(),
            "oracle": STRATEGIES[oracle].name(),
            "choices": policy_names.iter().enumerate().map(|(index, name)| json!({
                "policy": name,
                "strategy": choices[index].name(),
                "cost": row.measurements[choices[index].index()].cost(lambda),
                "regret": row.measurements[choices[index].index()].cost(lambda) - costs[oracle],
            })).collect::<Vec<_>>(),
        }));
    }

    let oracle_cost = policy_costs[5];
    let policies = policy_names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            json!({
                "policy": name,
                "cost": policy_costs[index],
                "cost_bytes_only": policy_costs_bytes_only[index],
                "regret": policy_costs[index] - oracle_cost,
                "regret_fraction": if oracle_cost > 0.0 {
                    (policy_costs[index] - oracle_cost) / oracle_cost
                } else { 0.0 },
            })
        })
        .collect::<Vec<_>>();

    // Planning cost: the fingerprint is one pass; the tree is five comparisons.
    let plan_started = Instant::now();
    let mut sink = 0usize;
    for row in &heldout_rows {
        sink += predict(&tree, &row.fingerprint);
        sink += if row.fingerprint.residual_entropy <= entropy_rule.0 {
            1
        } else {
            0
        };
    }
    let plan_ns = plan_started.elapsed().as_secs_f64() * 1e9 / (heldout_rows.len().max(1) as f64);
    // Measured: the one-pass fingerprint over the held-out bytes.
    let fingerprint_ns: f64 = heldout_rows.iter().map(|row| row.fingerprint_ns).sum();
    let fingerprint_bytes: u64 = heldout_rows.iter().map(|row| row.fingerprint.bytes).sum();

    let document = json!({
        "schema": "weftfs.residual-selector/1",
        "generated_at": utc_stamp(),
        "revision": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
        "environment": {
            "court": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "declared_split": {
            "fit": fit_specs,
            "validation": validation_specs,
            "heldout": heldout_specs,
        },
        "objective": {
            "lambda_bytes_per_ns": lambda,
            "formula": "bytes + (encode_ns + decode_ns) * 1e-6 * 1000 * lambda",
            "bytes_only_reported": true,
        },
        "candidates": STRATEGIES.iter().map(|s| s.name()).collect::<Vec<_>>(),
        "features": FEATURE_NAMES,
        "fit": {
            "tensors": fit_rows.len(),
            "objects": sink,
            "depth_sweep": depth_rows,
            "chosen_depth": chosen_depth,
            "tree": describe(&tree),
            "entropy_rule": {
                "threshold": entropy_rule.0,
                "below": entropy_rule.1.name(),
                "above": entropy_rule.2.name(),
            },
            "size_rule": {
                "feature": FEATURE_NAMES[size_rule.feature],
                "threshold": size_rule.threshold,
                "below": STRATEGIES[size_rule.below].name(),
                "above": STRATEGIES[size_rule.above].name(),
            },
        },
        "validation": {
            "tensors": validation_rows.len(),
            "cost": best_validation,
        },
        "planning": {
            "per_tensor_decision_ns": plan_ns,
            "fingerprint_pass_ns_sum": fingerprint_ns,
            "fingerprint_pass_bytes": fingerprint_bytes,
            "fingerprint_ns_per_mib": fingerprint_ns / (fingerprint_bytes as f64 / 1048576.0),
            "headroom_ns": MAX_PLAN_NS - plan_ns,
        },
        "heldout": {
            "tensors": heldout_rows.len(),
            "oracle_cost": oracle_cost,
            "oracle_cost_bytes_only": policy_costs_bytes_only[5],
            "policies": policies,
            "rows": rows_json,
        },
    });
    let bytes = serde_json::to_vec_pretty(&document).map_err(|err| err.to_string())?;
    fs::write(&out, bytes).map_err(|err| format!("write {}: {err}", out.display()))?;
    println!(
        "{}",
        json!({
            "out": out.display().to_string(),
            "fit_tensors": fit_rows.len(),
            "heldout_tensors": heldout_rows.len(),
            "oracle_cost": oracle_cost,
            "tree_regret_fraction": (policy_costs[4] - oracle_cost) / oracle_cost,
        })
    );
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("residual_selector: {err}");
        std::process::exit(1);
    }
}
