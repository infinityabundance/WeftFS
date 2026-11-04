//! Tensor representation planner benchmark (synthetic model family).
//!
//! Workload: 10 checkpoints x 8 tensors x 1 MiB bf16, 1% of elements changed
//! per checkpoint. Three planners are measured in separate processes so peak
//! RSS is honest for each:
//!
//! * `zstd`    — every tensor is an independent ZSTD frame;
//! * `bytes`   — `lambda_ns = 0` (bytes-only planner);
//! * `latency` — a latency weight that charges bytes per estimated ns.
//!
//! Full restore is timed for 1, 2, and 4 concurrent read-only workers. Every
//! number is produced by this harness inside the OOM-limited bench court; the
//! label is synthetic, single-host, and makes no fleet claim.
//!
//! Usage (inside the court):
//!   cargo run --release --example tensor_planner_bench -- \
//!       --planner zstd --out bench/receipts/part-zstd.json
//!   cargo run --release --example tensor_planner_bench -- \
//!       --merge a.json b.json c.json --out bench/receipts/tensor-planner-....json

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};

const CHECKPOINTS: usize = 10;
const TENSORS: usize = 8;
const TENSOR_BYTES: usize = 1024 * 1024;
const CHANGE_PERCENT: usize = 1;
const SHAPE: [u64; 2] = [512, 1024];

fn court(label: &str) -> Option<String> {
    std::env::var(label).ok()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(index) = args.iter().position(|arg| arg == "--merge") {
        let rest = &args[index + 1..];
        if let Err(err) = merge(rest) {
            eprintln!("tensor_planner_bench: {err}");
            std::process::exit(1);
        }
        return;
    }
    let planner = value_of(&args, "--planner").unwrap_or_else(|| "bytes".to_owned());
    let out = value_of(&args, "--out").unwrap_or_else(|| format!("tensor-planner-{planner}.json"));
    if let Err(err) = run(&planner, Path::new(&out)) {
        eprintln!("tensor_planner_bench: {err}");
        std::process::exit(1);
    }
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn merge(args: &[String]) -> Result<(), String> {
    let out = value_of(args, "--out").ok_or("--merge requires --out")?;
    let mut runs = Vec::new();
    for arg in args {
        if arg == "--out" || arg == &out || arg == "--merge" {
            continue;
        }
        let text = fs::read_to_string(arg).map_err(|err| format!("{arg}: {err}"))?;
        let value: Json = serde_json::from_str(&text).map_err(|err| format!("{arg}: {err}"))?;
        runs.push(value);
    }
    let receipt = json!({
        "schema": "weft.tensor-planner.bench.v1",
        "label": "synthetic model family, single host court, no fleet claim",
        "workload": {
            "checkpoints": CHECKPOINTS,
            "tensors_per_checkpoint": TENSORS,
            "tensor_bytes": TENSOR_BYTES,
            "dtype": "bf16",
            "shape": SHAPE,
            "change_percent_per_step": CHANGE_PERCENT,
        },
        "environment": {
            "court_image": court("WEFT_COURT_IMAGE"),
            "court_image_id": court("WEFT_COURT_IMAGE_ID"),
            "kernel": court("WEFT_COURT_KERNEL"),
            "memory_limit": court("WEFT_COURT_MEMORY"),
            "cpu_limit": court("WEFT_COURT_CPUS"),
            "pids_limit": court("WEFT_COURT_PIDS"),
            "docker_server": court("WEFT_DOCKER_SERVER"),
            "revision_weftfs": court("WEFT_REPO_WEFTFS"),
            "revision_weftcodec": court("WEFT_REPO_WEFTCODEC"),
        },
        "runs": runs,
    });
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{out}: {err}"))?;
    println!("{out}");
    Ok(())
}

fn run(planner: &str, out: &Path) -> Result<(), String> {
    let dir = PathBuf::from(format!(
        "/tmp/weft-tensor-planner-{planner}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|err| format!("{dir:?}: {err}"))?;
    let options = Options {
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        ..Options::default()
    };
    let mut store = Store::init(&dir, options).map_err(|err| err.to_string())?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    let layout = TensorLayoutSpec::row_major();

    let mut previous: Vec<Option<weftfs::Hash32>> = vec![None; TENSORS];
    let mut ids: Vec<weftfs::Hash32> = Vec::with_capacity(CHECKPOINTS * TENSORS);
    let mut tensors: Vec<Vec<u8>> = (0..TENSORS).map(|tensor| model_tensor(tensor)).collect();
    let ingest_start = Instant::now();
    for checkpoint in 0..CHECKPOINTS {
        if checkpoint > 0 {
            for bytes in tensors.iter_mut() {
                advance(bytes, checkpoint as u64, CHANGE_PERCENT);
            }
        }
        for tensor in 0..TENSORS {
            let name = format!("c{checkpoint:02}t{tensor:02}").into_bytes();
            let mut spec = TensorWriteSpec::default();
            match planner {
                "zstd" => {
                    spec.strategy_hint = Some(TensorStrategy::Zstd);
                }
                "bytes" => {
                    spec.plan = TensorPlanOptions::default();
                    if let Some(version) = previous[tensor] {
                        spec.causal = vec![version];
                    }
                }
                "latency" => {
                    spec.plan = TensorPlanOptions {
                        lambda_ns: 100_000,
                        ..TensorPlanOptions::default()
                    };
                    if let Some(version) = previous[tensor] {
                        spec.causal = vec![version];
                    }
                }
                other => return Err(format!("unknown planner `{other}`")),
            }
            let id = store
                .write_tensor_file_with_spec(
                    root,
                    &name,
                    "bf16",
                    &SHAPE,
                    &tensors[tensor],
                    &layout,
                    &spec,
                )
                .map_err(|err| format!("write c{checkpoint:02}t{tensor:02}: {err}"))?;
            let record = store
                .object_meta(id)
                .map_err(|err| format!("meta c{checkpoint:02}t{tensor:02}: {err}"))?;
            previous[tensor] = Some(record.version);
            ids.push(id);
        }
    }
    let ingest_ms = ingest_start.elapsed().as_secs_f64() * 1000.0;
    let data_bytes = tree_bytes(&dir.join("manifests"))? + tree_bytes(&dir.join("objects"))?;
    let total_bytes = tree_bytes(&dir)?;
    let index_bytes = tree_bytes(&dir.join("indexes")).unwrap_or(0);
    let peak_rss_kib = peak_rss_kib();
    let verify_ok = store
        .verify(true)
        .map(|report| report.ok)
        .map_err(|err| format!("verify: {err}"))?;
    drop(store);

    let mut restores = Vec::new();
    for workers in [1usize, 2, 4] {
        let start = Instant::now();
        let outcome: std::result::Result<usize, String> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for worker in 0..workers {
                let ids = &ids;
                let dir = dir.clone();
                handles.push(
                    scope.spawn(move || -> std::result::Result<(usize, u64), String> {
                        let store = Store::open(&dir, OpenMode::ReadOnly)
                            .map_err(|err| format!("restore open: {err}"))?;
                        let mut count = 0usize;
                        let mut bytes = 0u64;
                        let mut index = worker;
                        while index < ids.len() {
                            let data = store
                                .read_tensor(ids[index])
                                .map_err(|err| format!("restore read {}: {err}", ids[index]))?;
                            bytes += data.len() as u64;
                            count += 1;
                            index += workers;
                        }
                        Ok((count, bytes))
                    }),
                );
            }
            let mut total = 0usize;
            for handle in handles {
                let (count, _bytes) = handle
                    .join()
                    .map_err(|_| "restore worker panicked".to_owned())??;
                total += count;
            }
            Ok(total)
        });
        let restored = outcome?;
        let restore_ms = start.elapsed().as_secs_f64() * 1000.0;
        restores.push(json!({
            "workers": workers,
            "objects": restored,
            "wall_ms": round(restore_ms),
            "objects_per_second": round(restored as f64 / (restore_ms / 1000.0).max(f64::MIN_POSITIVE)),
        }));
    }

    let receipt = json!({
        "planner": planner,
        "lambda_ns": if planner == "latency" { 100_000 } else { 0 },
        "physical": {
            "data_bytes": data_bytes,
            "index_bytes": index_bytes,
            "store_total_bytes": total_bytes,
        },
        "ingest_ms": round(ingest_ms),
        "ingest_objects_per_second": round(ids.len() as f64 / (ingest_ms / 1000.0).max(f64::MIN_POSITIVE)),
        "restore": restores,
        "peak_rss_kib": peak_rss_kib,
        "verify_ok": verify_ok,
    });
    fs::write(
        out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{out:?}: {err}"))?;
    println!("{}", out.display());
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn peak_rss_kib() -> u64 {
    let status = match fs::read_to_string("/proc/self/status") {
        Ok(text) => text,
        Err(_) => return 0,
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(value) = rest.split_whitespace().next() {
                return value.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn tree_bytes(path: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(format!("{}: {err}", path.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let metadata = entry.metadata().map_err(|err| err.to_string())?;
        if metadata.is_dir() {
            total += tree_bytes(&entry.path())?;
        } else if metadata.is_file() {
            total += metadata.len();
        }
    }
    Ok(total)
}

/// Deterministic pseudorandom bf16 tensor with a slowly varying, compressible
/// structure plus noise, so independent ZSTD has a realistic baseline.
fn model_tensor(tensor: usize) -> Vec<u8> {
    let elements = TENSOR_BYTES / 2;
    let mut out = Vec::with_capacity(TENSOR_BYTES);
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ (tensor as u64 + 1);
    for index in 0..elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let trend = (index / 64) as u16 & 0x1f;
        let noise = (state as u16) & 0x03;
        let word = 0x3f00u16 | trend | noise;
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

/// Change `percent` of elements by flipping the least-significant mantissa bit.
fn advance(bytes: &mut [u8], step: u64, percent: usize) {
    let elements = bytes.len() / 2;
    let count = (elements * percent / 100).max(1);
    let stride = (elements / count).max(1);
    let mut state = 0x1234_5678_9abc_def0u64 ^ (step << 17);
    let mut index = 0usize;
    let mut changed = 0usize;
    while changed < count && index < elements {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let flip = (state as usize) % 3;
        bytes[index * 2] ^= 1u8 << flip;
        changed += 1;
        index += stride + ((state as usize) & 3);
    }
}
