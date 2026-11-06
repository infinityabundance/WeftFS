//! Decode-cost calibration for the tensor planner (R1).
//!
//! Measures end-to-end `Store::read_tensor` for every planner representation on
//! two payload sizes (16 MiB and 1 MiB by default) with 7 repetitions and a
//! warm-up read, then fits
//!
//! ```text
//! estimated_decode_ns = fixed_ns + decoded_bytes * ns_per_mib / 1_048_576
//! ```
//!
//! The measured table is host-calibrated, not a portable constant: it is bound
//! to the court image, kernel, CPU, and allocator recorded in the receipt. The
//! fitted rows are pasted into `src/cost_model.rs` and
//! `WeftFS-Go/store/cost_model.go` (identical integers).
//!
//! Usage (inside the bench court):
//!   cargo run --release --example calibrate_decode -- \
//!       --out /work/.docker-cache/calibration/decode.json
//!
//! Synthetic payloads: BF16 values drawn from a bounded exponent/mantissa
//! distribution (not uniform random bytes), so zstd and the mode-9 plane coder
//! see realistic weight-like data. Every measurement is of the shipped store
//! read path, including frame/chunk verification.

use std::fs;
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftcodec::SemanticPolicy;
use weftfs::cost_model::CostClass;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};

const MIB: u64 = 1_048_576;

fn main() {
    if let Err(err) = run() {
        eprintln!("calibrate_decode: {err}");
        std::process::exit(1);
    }
}

fn arg(args: &[String], flag: &str, default: &str) -> String {
    args.iter()
        .position(|value| value == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
        .unwrap_or_else(|| default.to_owned())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = arg(&args, "--out", "decode-calibration.json");
    let reps: usize = arg(&args, "--reps", "7")
        .parse()
        .map_err(|_| "--reps must be an integer".to_owned())?;
    let large_mib: u64 = arg(&args, "--large-mib", "16")
        .parse()
        .map_err(|_| "--large-mib must be an integer".to_owned())?;
    // The small payload isolates the per-call fixed cost (manifest read, frame
    // verification, allocation); the large payload fixes ns/MiB.
    let small_kib: u64 = arg(&args, "--small-kib", "128")
        .parse()
        .map_err(|_| "--small-kib must be an integer".to_owned())?;
    if reps < 5 {
        return Err("the calibration requires at least 5 repetitions".to_owned());
    }
    if large_mib < 8 || small_kib < 64 || small_kib * 1024 >= large_mib * MIB {
        return Err("sizes must satisfy 8 <= large MiB and 64 KiB <= small < large".to_owned());
    }

    let store_path = std::env::temp_dir().join(format!(
        "weft-decode-calibration-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    if store_path.exists() {
        fs::remove_dir_all(&store_path).map_err(|err| err.to_string())?;
    }
    let mut store = Store::init(
        &store_path,
        Options {
            chunk_size: 64 * 1024,
            checkpoint_interval: 512,
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .map_err(|err| err.to_string())?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    let layout = TensorLayoutSpec::row_major();

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    let classes = [
        ("raw", TensorStrategy::Raw),
        ("zstd", TensorStrategy::Zstd),
        ("float-tensor", TensorStrategy::FloatTensor),
        ("tensor-sharded", TensorStrategy::TensorSharded),
        ("tensor-tiled", TensorStrategy::TensorTiled),
        ("weight-delta", TensorStrategy::WeightDelta),
        ("semantic-delta", TensorStrategy::SemanticDelta),
    ];
    for (name, strategy) in classes {
        let mut measured = Vec::new();
        for target_bytes in [small_kib * 1024, large_mib * MIB] {
            match measure(&mut store, root, &layout, strategy, target_bytes, reps) {
                Ok(row) => measured.push(row),
                Err(err) => {
                    errors.push(format!("{name} @ {target_bytes} bytes: {err}"));
                    break;
                }
            }
        }
        if measured.len() != 2 {
            continue;
        }
        let small = &measured[0];
        let large = &measured[1];
        // Delta rows are fitted on the *incremental* frame cost: the base read
        // is measured with the same repetitions and subtracted, so a delta's
        // base can be priced recursively by the planner.
        let small_ns = (small.median_ns
            .saturating_sub(small.dependency_median_ns.unwrap_or(0))) as f64;
        let large_ns = (large.median_ns
            .saturating_sub(large.dependency_median_ns.unwrap_or(0))) as f64;
        let delta_bytes = (large.logical_bytes - small.logical_bytes) as f64;
        let per_byte = if large_ns > small_ns && delta_bytes > 0.0 {
            (large_ns - small_ns) / delta_bytes
        } else {
            0.0
        };
        let fixed = (small_ns - per_byte * small.logical_bytes as f64).max(0.0);
        let ns_per_mib = (per_byte * MIB as f64).round() as u64;
        let fixed_ns = fixed.round() as u64;
        rows.push(json!({
            "class": name,
            "strategy": strategy.as_str(),
            "small": small.to_json(),
            "large": large.to_json(),
            "fit": {
                "fixed_ns": fixed_ns,
                "ns_per_mib": ns_per_mib,
                "measured_large_ns_per_mib": (large_ns / (large.logical_bytes as f64 / MIB as f64)),
                "implies_ns": weftfs::cost_model::estimate_ns(
                    CostClass::parse(name).unwrap_or(CostClass::Raw),
                    large.logical_bytes,
                    weftfs::cost_model::CostModelKind::Measured,
                ),
            },
        }));
    }
    drop(store);
    let _ = fs::remove_dir_all(&store_path);

    let receipt = json!({
        "schema": "weft.decode-calibration.v1",
        "label": "host-calibrated decode table; single-host court, synthetic weight-like BF16, warm cache",
        "calibration_id": weftfs::cost_model::MEASURED_CALIBRATION,
        "environment": {
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "weftfs_revision": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "weftcodec_revision": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "method": {
            "reps": reps,
            "warmup_reads": 1,
            "small_bytes": small_kib * 1024,
            "large_bytes": large_mib * MIB,
            "fit": "fixed_ns + ns_per_mib * decoded_bytes / 1048576; fit from the small and large medians; delta rows subtract the measured base read",
            "includes": "manifest read, frame/chunk verification, full logical decode",
        },
        "rows": rows,
        "errors": errors,
    });
    if let Some(parent) = std::path::Path::new(&out).parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{out}: {err}"))?;
    println!("{}", out);
    // Also print a copy-paste table for the two cost_model modules.
    println!("// measured table (fixed_ns, ns_per_mib)");
    for row in receipt["rows"].as_array().into_iter().flatten() {
        println!(
            "({}, {}, {}),",
            row["class"].as_str().unwrap_or("?"),
            row["fit"]["fixed_ns"].as_u64().unwrap_or(0),
            row["fit"]["ns_per_mib"].as_u64().unwrap_or(0),
        );
    }
    Ok(())
}

struct Measurement {
    logical_bytes: u64,
    reps_ns: Vec<u128>,
    dependency_median_ns: Option<u128>,
    median_ns: u128,
    min_ns: u128,
    max_ns: u128,
    backend_bytes: u64,
}

impl Measurement {
    fn to_json(&self) -> Json {
        json!({
            "logical_bytes": self.logical_bytes,
            "reps_ns": self.reps_ns.iter().map(|value| *value as u64).collect::<Vec<_>>(),
            "median_ns": self.median_ns as u64,
            "min_ns": self.min_ns as u64,
            "max_ns": self.max_ns as u64,
            "median_ns_per_mib": (self.median_ns as f64) / (self.logical_bytes as f64 / MIB as f64),
            "backend_bytes_read_last": self.backend_bytes,
            "dependency_median_ns": self.dependency_median_ns.map(|value| value as u64),
        })
    }
}

fn measure(
    store: &mut Store,
    root: weftfs::Hash32,
    layout: &TensorLayoutSpec,
    strategy: TensorStrategy,
    target_bytes: u64,
    reps: usize,
) -> Result<Measurement, String> {
    let elements = target_bytes / 2;
    let shape = vec![elements / 1024, 1024];
    let (dtype, shape, bytes, spec, base_id) = match strategy {
        TensorStrategy::TensorSharded => {
            let elements = target_bytes;
            (
                "q8_0".to_owned(),
                vec![elements],
                q8_payload(elements),
                TensorWriteSpec {
                    strategy_hint: Some(strategy),
                    ..TensorWriteSpec::default()
                },
                None,
            )
        }
        TensorStrategy::TensorTiled => (
            "bf16".to_owned(),
            shape,
            bf16_payload(elements),
            TensorWriteSpec {
                strategy_hint: Some(strategy),
                tile_shape: Some(vec![32, 256]),
                ..TensorWriteSpec::default()
            },
            None,
        ),
        TensorStrategy::WeightDelta => {
            let (base_id, base_bytes) = base_case(store, root, layout, target_bytes, false)?;
            let base = store.object_meta(base_id).map_err(|err| err.to_string())?;
            (
                "bf16".to_owned(),
                shape,
                nudge(&base_bytes),
                TensorWriteSpec {
                    strategy_hint: Some(strategy),
                    causal: vec![base.version],
                    ..TensorWriteSpec::default()
                },
                Some(base_id),
            )
        }
        TensorStrategy::SemanticDelta => {
            let (base_id, base_bytes) = base_case(store, root, layout, target_bytes, true)?;
            (
                "bf16".to_owned(),
                shape,
                clear_low_mantissa_bits(&base_bytes, 3),
                TensorWriteSpec {
                    strategy_hint: Some(strategy),
                    plan: TensorPlanOptions {
                        semantic_policy: Some(SemanticPolicy::MantissaTruncate { bits: 3 }),
                        candidates: vec![base_id],
                        ..TensorPlanOptions::default()
                    },
                    ..TensorWriteSpec::default()
                },
                Some(base_id),
            )
        }
        _ => (
            "bf16".to_owned(),
            shape,
            bf16_payload(elements),
            TensorWriteSpec {
                strategy_hint: Some(strategy),
                ..TensorWriteSpec::default()
            },
            None,
        ),
    };
    let dir = store
        .mkdir(root, strategy.as_str().as_bytes(), 0o755, 0, 0)
        .or_else(|err| {
            if err.class() == weftfs::Class::Exists {
                store.resolve_components(&[strategy.as_str().as_bytes()])
            } else {
                Err(err)
            }
        })
        .map_err(|err| err.to_string())?;
    let id = store
        .write_tensor_file_with_spec(
            dir,
            format!("calibration-{target_bytes}").as_bytes(),
            &dtype,
            &shape,
            &bytes,
            layout,
            &spec,
        )
        .map_err(|err| format!("write: {err}"))?;
    let meta = store.tensor_metadata(id).map_err(|err| err.to_string())?;
    if meta.strategy != strategy.as_str() {
        return Err(format!(
            "requested `{}` but the store recorded `{}`",
            strategy.as_str(),
            meta.strategy
        ));
    }
    // Warm-up read (also verifies the object is decodable).
    store.read_tensor(id).map_err(|err| format!("warmup: {err}"))?;
    let dependency_median_ns = match base_id {
        Some(base_id) => {
            store
                .read_tensor(base_id)
                .map_err(|err| format!("base warmup: {err}"))?;
            let mut base_ns = Vec::with_capacity(reps);
            for _ in 0..reps {
                let started = Instant::now();
                store
                    .read_tensor(base_id)
                    .map_err(|err| format!("base read: {err}"))?;
                base_ns.push(started.elapsed().as_nanos());
            }
            base_ns.sort_unstable();
            Some(base_ns[base_ns.len() / 2])
        }
        None => None,
    };
    let mut reps_ns = Vec::with_capacity(reps);
    let mut backend_bytes = 0u64;
    for _ in 0..reps {
        let before = store.backend_stats();
        let started = Instant::now();
        let data = store.read_tensor(id).map_err(|err| format!("read: {err}"))?;
        let elapsed = started.elapsed().as_nanos();
        let after = store.backend_stats();
        backend_bytes = after.bytes_read.saturating_sub(before.bytes_read);
        if data.len() as u64 != target_bytes {
            return Err(format!(
                "decoded {} bytes, expected {target_bytes}",
                data.len()
            ));
        }
        reps_ns.push(elapsed);
    }
    let mut sorted = reps_ns.clone();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    Ok(Measurement {
        logical_bytes: target_bytes,
        reps_ns,
        dependency_median_ns,
        median_ns: median,
        min_ns: *sorted.first().unwrap_or(&0),
        max_ns: *sorted.last().unwrap_or(&0),
        backend_bytes,
    })
}

/// Build a base object used by the delta cases; returns its id and bytes.
fn base_case(
    store: &mut Store,
    root: weftfs::Hash32,
    layout: &TensorLayoutSpec,
    target_bytes: u64,
    semantic: bool,
) -> Result<(weftfs::Hash32, Vec<u8>), String> {
    let elements = target_bytes / 2;
    let shape = vec![elements / 1024, 1024];
    let base_bytes = bf16_payload(elements);
    let dir = store
        .mkdir(root, b"calibration-bases", 0o755, 0, 0)
        .or_else(|err| {
            if err.class() == weftfs::Class::Exists {
                store.resolve_components(&[b"calibration-bases"])
            } else {
                Err(err)
            }
        })
        .map_err(|err| err.to_string())?;
    let name = format!(
        "base-{}-{target_bytes}",
        if semantic { "semantic" } else { "weight" }
    );
    let base_id = match store.resolve_components(&[b"calibration-bases", name.as_bytes()]) {
        Ok(id) => id,
        Err(_) => store
            .write_tensor_file_with_spec(
                dir,
                name.as_bytes(),
                "bf16",
                &shape,
                &base_bytes,
                layout,
                &TensorWriteSpec {
                    strategy_hint: Some(TensorStrategy::Raw),
                    ..TensorWriteSpec::default()
                },
            )
            .map_err(|err| format!("base write: {err}"))?,
    };
    Ok((base_id, base_bytes))
}

/// Deterministic weight-like BF16 payload.
fn bf16_payload(elements: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements as usize * 2);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..elements {
        // f32 in roughly [-1, 1), converted to BF16 by truncation.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let unit = ((state >> 40) as f32) / ((1u64 << 24) as f32); // [0, 1)
        let value = (unit * 2.0 - 1.0) * 0.5;
        let bits = value.to_bits();
        let bf16 = (bits >> 16) as u16;
        out.extend_from_slice(&bf16.to_le_bytes());
    }
    out
}

/// q8_0 logical bytes (one two's-complement byte per element).
fn q8_payload(elements: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(elements as usize);
    let mut state = 0x1234_5678_9abc_def0u64;
    for _ in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state >> 32) as u8);
    }
    out
}

/// Flip the lowest bit of every 4096th 16-bit word (a sparse weight update).
fn nudge(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let words = out.len() / 2;
    for index in (0..words).step_by(4096) {
        out[index * 2] ^= 1;
    }
    out
}

/// Clear (then re-randomize) the low `bits` mantissa bits while preserving
/// sign/exponent, so two tensors share a semantic root under
/// `mantissa_truncate:bits`.
fn clear_low_mantissa_bits(bytes: &[u8], bits: u32) -> Vec<u8> {
    let mask = !(((1u16 << bits) - 1) as u16);
    let mut out = Vec::with_capacity(bytes.len());
    for chunk in bytes.chunks_exact(2) {
        let word = u16::from_le_bytes([chunk[0], chunk[1]]) & mask;
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}
