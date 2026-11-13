//! N08: where a compressed checkpoint restore actually spends its time.
//!
//! The measured fact under investigation (R22/R25): a cold restore of the
//! compressed Qwen 0.5B checkpoint is ≈1.4 s against ≈0.35 s for the native
//! per-tensor files. This harness does not change the writer. It profiles the
//! **restore** path end to end and reports the split, then prices one
//! principled change: a whole-object tensor read used to copy the decoded
//! buffer before verifying it (`WEFT_TENSOR_READ_COPY=1` reproduces that copy
//! exactly), and now returns the decoded buffer directly.
//!
//! Phases (`WEFT_PHASE_PROFILE` is enabled by this harness):
//!
//! * `blob_io_manifest` / `blob_hash_manifest` / `manifest_decode` — reading
//!   and checking the representation manifest and the stored frame;
//! * `tensor_decode` — the mode-9/10/15 payload decode (new in N08);
//! * `logical_root` — recomputing the tensor's logical root (new in N08);
//! * `content_verify` — the whole-content BLAKE3 over the decoded bytes;
//! * `digest` — the per-tensor SHA-256 the restore report carries;
//! * `assemble` — a byte copy between decode and the caller (zero after N08 for
//!   a whole-tensor read; the control arm restores it).
//!
//! Phases:
//!
//! ```text
//!   restore-cost-court build    # write one store with c1/c10/c100
//!   restore-cost-court measure  # per-size restore, region, resume, phases
//! ```
//!
//! Environment: `WEFT_COST_WORK` (default `/tmp/weft-restore-cost`),
//! `WEFT_COST_TENSOR_BYTES` (default 1048576), `WEFT_COST_REPS` (default 7),
//! `WEFT_COST_COPY` (`1` = pre-N08 copy control), `WEFT_COST_SIZES` (default
//! `1,10,100`). Numbers are printed as one JSON object; the court script wraps
//! them into a receipt with the environment and revisions.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use weftcodec::tensor::Region;
use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::{Error, Result};

const STAMP: i64 = 1_700_000_000_000_000_000;

struct Args {
    work: PathBuf,
    tensor_bytes: usize,
    reps: usize,
    copy_control: bool,
    sizes: Vec<usize>,
}

fn parse_args() -> Result<(String, Args)> {
    let phase = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "measure".to_owned());
    let work = std::env::var("WEFT_COST_WORK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/weft-restore-cost"));
    let tensor_bytes = std::env::var("WEFT_COST_TENSOR_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1024 * 1024);
    let reps = std::env::var("WEFT_COST_REPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(7);
    let copy_control = matches!(
        std::env::var("WEFT_COST_COPY").as_deref(),
        Ok("1") | Ok("true")
    );
    let sizes = std::env::var("WEFT_COST_SIZES")
        .unwrap_or_else(|_| "1,10,100".to_owned())
        .split(',')
        .filter_map(|value| value.trim().parse::<usize>().ok())
        .collect::<Vec<_>>();
    Ok((
        phase,
        Args {
            work,
            tensor_bytes,
            reps,
            copy_control,
            sizes: if sizes.is_empty() {
                vec![1, 10, 100]
            } else {
                sizes
            },
        },
    ))
}

/// Column extent of the synthetic tensor (tensor_bytes / 2 elements, 512 rows).
fn region_cols(tensor_bytes: usize) -> u64 {
    ((tensor_bytes / 2).max(2) as u64 / 512).max(1).min(1024)
}

fn store_opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 4096,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(STAMP),
        ..Options::default()
    }
}

fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Shape chosen so 512 * 1024 * 2 bytes = 1 MiB and axis 0 divides by two.
fn tensor_shape(tensor_bytes: usize) -> Vec<u64> {
    let elements = (tensor_bytes / 2).max(2);
    let rows = 512u64.min(elements as u64);
    let cols = (elements as u64 / rows).max(1);
    vec![rows, cols]
}

fn container_path(work: &Path, count: usize) -> PathBuf {
    work.join("src").join(format!("c{count}.safetensors"))
}

fn build_one(work: &Path, count: usize, tensor_bytes: usize) -> Result<PathBuf> {
    let shape = tensor_shape(tensor_bytes);
    let path = container_path(work, count);
    if path.exists() {
        return Ok(path);
    }
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|err| Error::io(format!("create source dir: {err}")))?;
    let owned: Vec<(String, Vec<u8>)> = (0..count)
        .map(|index| {
            (
                format!("t{index:03}"),
                bytes(tensor_bytes, 0x51ee_0000 + index as u64),
            )
        })
        .collect();
    let entries: Vec<SafetensorsEntry> = owned
        .iter()
        .map(|(name, data)| SafetensorsEntry {
            name,
            dtype: "bf16",
            shape: &shape,
            data,
        })
        .collect();
    let file = safetensors::encode_file(&entries, None)?;
    fs::write(&path, &file).map_err(|err| Error::io(format!("write source: {err}")))?;
    Ok(path)
}

fn file_sha256(path: &Path) -> Result<String> {
    let data = fs::read(path).map_err(|err| Error::io(format!("read source: {err}")))?;
    let digest = Sha256::digest(&data);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn store_path(work: &Path, count: usize) -> PathBuf {
    work.join(format!("store-c{count}"))
}

fn build(args: &Args) -> Result<Value> {
    let mut checkpoints = BTreeMap::new();
    for count in &args.sizes {
        let store_path = store_path(&args.work, *count);
        if store_path.exists() {
            fs::remove_dir_all(&store_path)
                .map_err(|err| Error::io(format!("reset store: {err}")))?;
        }
        let mut store = Store::init(&store_path, store_opts())?;
        let source = build_one(&args.work, *count, args.tensor_bytes)?;
        let name = "ckpt".to_owned();
        let started = Instant::now();
        let report = store.write_checkpoint(
            &name,
            &source,
            &CheckpointWriteOptions {
                shards: 2,
                axis: 0,
                strategy: ShardStrategy::TensorSharded,
                deterministic_seed: Some([0x2a; 32]),
                ..CheckpointWriteOptions::default()
            },
        )?;
        let verify = store.verify(true)?;
        if !verify.problems.is_empty() {
            return Err(Error::corrupt(format!(
                "checkpoint {name} store verify: {:?}",
                verify.problems
            )));
        }
        checkpoints.insert(
            format!("c{count}"),
            json!({
                "tensors": count,
                "checkpoint": name,
                "store": store_path.display().to_string(),
                "source": source.display().to_string(),
                "source_sha256": file_sha256(&source)?,
                "logical_bytes": report.tensors.iter().map(|item| item.element_bytes).sum::<u64>(),
                "written_objects": report.tensors.len(),
                "write_ms": started.elapsed().as_secs_f64() * 1000.0,
            }),
        );
    }
    Ok(json!({
        "phase": "build",
        "schema": "weft.restore_cost.build.v1",
        "tensor_bytes": args.tensor_bytes,
        "checkpoints": checkpoints,
    }))
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn summary(samples: &[f64]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean = if sorted.is_empty() {
        0.0
    } else {
        sorted.iter().sum::<f64>() / sorted.len() as f64
    };
    json!({
        "reps": sorted.len(),
        "min_ms": sorted.first().copied().unwrap_or(0.0),
        "p50_ms": quantile(&sorted, 0.5),
        "p95_ms": quantile(&sorted, 0.95),
        "p99_ms": quantile(&sorted, 0.99),
        "max_ms": sorted.last().copied().unwrap_or(0.0),
        "mean_ms": mean,
        "samples_ms": samples,
    })
}

/// A sink that hashes and counts the restored container without storing it.
struct HashingSink {
    hasher: Sha256,
    bytes: u64,
}

impl Write for HashingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hasher.update(buf);
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn restore_full(store: &Store, name: &str, reps: usize) -> Result<Value> {
    let mut samples = Vec::with_capacity(reps);
    let mut bytes = 0u64;
    let mut per_tensor_hash: Vec<BTreeMap<String, Value>> = Vec::new();
    let mut pipelined = Vec::new();
    let mut exact = true;
    for _ in 0..reps {
        weftfs::phase_profile::begin();
        let started = Instant::now();
        let mut sink = HashingSink {
            hasher: Sha256::new(),
            bytes: 0,
        };
        let report = store.restore_checkpoint_pipelined(
            name,
            &CheckpointRestoreOptions::default(),
            &weftfs::restore_pipeline::RestorePipelineOptions {
                threads: 4,
                ..weftfs::restore_pipeline::RestorePipelineOptions::default()
            },
            &mut sink,
        )?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        let snapshot = weftfs::phase_profile::finish();
        bytes = sink.bytes;
        exact &= report.exact_source_file;
        if let Some(stats) = report.pipeline {
            pipelined.push(json!({
                "effective_threads": stats.effective_threads,
                "decode_wall_ms": stats.decode_wall_ms,
                "decode_sum_ms": stats.decode_sum_ms,
                "write_ms": stats.write_ms,
                "transfer_ms": stats.transfer_ms,
                "total_ms": stats.total_ms,
                "observed_max_inflight_bytes": stats.observed_max_inflight_bytes,
            }));
        }
        let mut phases = BTreeMap::new();
        for (name, totals) in snapshot.phases {
            phases.insert(
                name.to_owned(),
                json!({
                    "count": totals.count,
                    "total_ms": totals.total_ns as f64 / 1.0e6,
                    "max_ms": totals.max_ns as f64 / 1.0e6,
                    "mean_ms": totals.mean_ns() / 1.0e6,
                }),
            );
        }
        per_tensor_hash.push(phases);
    }
    Ok(json!({
        "wall": summary(&samples),
        "restored_bytes": bytes,
        "exact_source_file": exact,
        "pipeline": pipelined,
        "phases": per_tensor_hash.last().cloned().unwrap_or_default(),
    }))
}

fn restore_one(store: &Store, name: &str, tensor: &str, reps: usize) -> Result<Value> {
    let mut samples = Vec::with_capacity(reps);
    let mut bytes = 0u64;
    for _ in 0..reps {
        let started = Instant::now();
        let mut sink = HashingSink {
            hasher: Sha256::new(),
            bytes: 0,
        };
        let report = store.restore_checkpoint(
            name,
            &CheckpointRestoreOptions {
                tensor: Some(tensor.to_owned()),
                region: None,
                topology: None,
            },
            &mut sink,
        )?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        bytes = sink.bytes;
        debug_assert!(report.tensors.len() <= 1);
    }
    Ok(json!({
        "wall": summary(&samples),
        "restored_bytes_per_rep": bytes,
    }))
}

fn read_region(
    store: &Store,
    name: &str,
    tensor: &str,
    reps: usize,
    tensor_bytes: usize,
) -> Result<Value> {
    let mut samples = Vec::with_capacity(reps);
    let mut bytes = 0u64;
    for _ in 0..reps {
        let started = Instant::now();
        let mut sink = HashingSink {
            hasher: Sha256::new(),
            bytes: 0,
        };
        let report = store.restore_checkpoint(
            name,
            &CheckpointRestoreOptions {
                tensor: Some(tensor.to_owned()),
                region: Some(Region::new(
                    vec![0, 0],
                    vec![2, region_cols(tensor_bytes)],
                )),
                topology: None,
            },
            &mut sink,
        )?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        bytes = sink.bytes;
        if report.tensors.len() != 1 {
            return Err(Error::corrupt("region restore did not report one tensor"));
        }
    }
    Ok(json!({
        "wall": summary(&samples),
        "restored_bytes_per_rep": bytes,
    }))
}

/// The T058 question, measured as far as this subphase reaches: after a
/// simulated failure (the first tensor restored, the rest not), how long does
/// it take to have a usable set of `k` tensors?
fn resume(store: &Store, name: &str, count: usize, reps: usize) -> Result<Value> {
    let k = count.min(10).max(1);
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        let started = Instant::now();
        for index in 0..k {
            let mut sink = HashingSink {
                hasher: Sha256::new(),
                bytes: 0,
            };
            store.restore_checkpoint(
                name,
                &CheckpointRestoreOptions {
                    tensor: Some(format!("t{index:03}")),
                    region: None,
                    topology: None,
                },
                &mut sink,
            )?;
        }
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(json!({
        "tensors": k,
        "wall": summary(&samples),
        "basis": "per-tensor restore of the first k tensors, one at a time (the resumed usable set)",
    }))
}

fn measure(args: &Args) -> Result<Value> {
    if args.copy_control {
        std::env::set_var("WEFT_TENSOR_READ_COPY", "1");
    }
    let mut out = BTreeMap::new();
    let mut backend_bytes = 0u64;
    let mut backend_reads = 0u64;
    for count in &args.sizes {
        let store_path = store_path(&args.work, *count);
        let store = Store::open_with_options(&store_path, OpenMode::ReadOnly, store_opts())?;
        let name = "ckpt";
        let full = restore_full(&store, &name, args.reps)?;
        let mut rows = BTreeMap::new();
        rows.insert("full".to_owned(), full);
        rows.insert(
            "first_tensor".to_owned(),
            restore_one(&store, &name, "t000", args.reps)?,
        );
        rows.insert(
            "region_2_rows".to_owned(),
            read_region(&store, &name, "t000", args.reps, args.tensor_bytes)?,
        );
        rows.insert(
            "resume".to_owned(),
            resume(&store, &name, *count, args.reps)?,
        );
        rows.insert(
            "source_sha256".to_owned(),
            json!(file_sha256(&container_path(&args.work, *count))?),
        );
        out.insert(format!("c{count}"), json!(rows));
        let stats = store.backend_stats();
        backend_bytes += stats.bytes_read;
        backend_reads += stats.reads;
    }
    Ok(json!({
        "phase": "measure",
        "schema": "weft.restore_cost.measure.v1",
        "tensor_bytes": args.tensor_bytes,
        "reps": args.reps,
        "copy_control": args.copy_control,
        "writer_sha_control": matches!(
            std::env::var("WEFT_RESTORE_WRITER_SHA").as_deref(),
            Ok("1") | Ok("true")
        ),
        "sizes": args.sizes,
        "checkpoints": out,
        "backend": {
            "bytes_read": backend_bytes,
            "reads": backend_reads,
        },
        "restore_pipeline": {
            "threads": 4,
            "tile_threads": 1,
            "shard_threads": 1,
        },
        "note": "cold label means a fresh process; the page cache is not dropped inside the court",
    }))
}

fn main() {
    let result = (|| -> Result<Value> {
        let (phase, args) = parse_args()?;
        match phase.as_str() {
            "build" => build(&args),
            "measure" => measure(&args),
            other => Err(Error::invalid_argument(format!(
                "unknown phase `{other}` (expected build|measure)"
            ))),
        }
    })();
    match result {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
        }
        Err(err) => {
            eprintln!("restore-cost-court: {err}");
            std::process::exit(1);
        }
    }
}

// `Duration` is used by the summary type above in debug builds only.
#[allow(dead_code)]
fn _unused(_: Duration) {}
