//! S13b: measure the whole-checkpoint restore cost the affine decode table omits.
//!
//! The decode rows in [`weftfs::cost_model`] are fitted on single-object
//! `Store::read_tensor` reads (`examples/calibrate_decode.rs`). A checkpoint
//! restore does more work per version: it loads the checkpoint manifest, then
//! for every tensor re-verifies the shard content root and logical root,
//! re-verifies after reassembly, hashes the tensor for the report, and hashes
//! the assembled container. Those passes are proportional to the version's
//! logical bytes and may also carry a per-tensor and per-version fixed cost.
//! S13 compared the graph model with measured restores and found that term
//! missing (the all-independent floor was under-predicted by 73.7 %).
//!
//! This harness times real `restore_checkpoint` calls, at a range of object
//! sizes (count fixed) and object counts (total bytes fixed), and fits
//!
//! ```text
//! restore_ns = decode_rows_ns + per_version_ns + per_tensor_ns * tensors
//!            + per_mib_ns * logical_mib
//! ```
//!
//! It also times a controlled dependency chain so the per-hop delta cost can be
//! checked against the measured delta row (S13b hypothesis 3).
//!
//! Usage (inside the bench court):
//!
//! ```sh
//! cargo run --release --example calibrate_restore -- \
//!   --out /work/.docker-cache/s13b/restore-floor.json \
//!   --work /work/.docker-cache/s13b/work
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::cost_model::{self, CostModelKind};
use weftfs::safetensors::{self, SafetensorsEntry};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::TensorStrategy;

const MIB: u64 = 1_048_576;
const MIB_F: f64 = 1_048_576.0;

/// Payload dtype for the synthetic sweep. `F32` matches the real pythia-70m
/// corpus (fp16-precision values widened to f32, low two bytes zero), so the
/// planner chooses the same zstd/zstd-19 candidates the S13 court realises.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PayloadDtype {
    Bf16,
    F32,
}

impl PayloadDtype {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "bf16" => Ok(PayloadDtype::Bf16),
            "f32" => Ok(PayloadDtype::F32),
            other => Err(format!(
                "--sweep-dtype must be `bf16` or `f32` (got `{other}`)"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            PayloadDtype::Bf16 => "bf16",
            PayloadDtype::F32 => "f32",
        }
    }

    fn element_bytes(self) -> u64 {
        match self {
            PayloadDtype::Bf16 => 2,
            PayloadDtype::F32 => 4,
        }
    }

    /// Deterministic weight-like payload. Values follow a normal-ish weight
    /// distribution (sum of four uniforms, std ~0.02) rather than a uniform
    /// interval, so the zstd frame's literal density is comparable to a real
    /// checkpoint. `F32` widens the value with a zero low mantissa half,
    /// exactly like pythia's stored checkpoints (fp16-precision values).
    fn payload(self, elements: u64, seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(elements as usize * self.element_bytes() as usize);
        let mut state = seed | 1;
        let mut uniform = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 40) as f32) / ((1u64 << 24) as f32)
        };
        for _ in 0..elements {
            // Irwin-Hall(4) has mean 2 and standard deviation 1/sqrt(3).
            let gaussian = (uniform() + uniform() + uniform() + uniform() - 2.0) * 0.0346;
            let bits = gaussian.to_bits();
            match self {
                PayloadDtype::Bf16 => out.extend_from_slice(&((bits >> 16) as u16).to_le_bytes()),
                PayloadDtype::F32 => {
                    // fp16 precision widened to f32: the low 13 mantissa bits are
                    // zero, exactly like the pythia-70m containers (plane 0
                    // entropy 0, plane 1 has a handful of symbols).
                    out.extend_from_slice(&(bits & !0x1fffu32).to_le_bytes());
                }
            }
        }
        out
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("calibrate_restore: {err}");
        std::process::exit(1);
    }
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

fn u64_list(text: &str) -> Result<Vec<u64>, String> {
    text.split(',')
        .map(|item| {
            item.trim()
                .parse::<u64>()
                .map_err(|_| format!("bad number `{item}` in list"))
        })
        .collect()
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = PathBuf::from(arg(&args, "--out").unwrap_or("restore-floor.json"));
    let work = PathBuf::from(arg(&args, "--work").unwrap_or("/tmp/weft-restore-floor"));
    let reps: usize = arg(&args, "--reps")
        .unwrap_or("7")
        .parse()
        .map_err(|_| "--reps must be an integer")?;
    if reps < 5 {
        return Err("--reps must be at least 5 (the calibration requires MIN_SAMPLES=5)".into());
    }
    let counts = u64_list(arg(&args, "--object-counts").unwrap_or("1,2,4,8,16,32"))?;
    let count_total_mib: u64 = arg(&args, "--count-total-mib")
        .unwrap_or("32")
        .parse()
        .map_err(|_| "--count-total-mib must be an integer")?;
    let sizes = u64_list(arg(&args, "--object-mib").unwrap_or("1,2,4,8,16,32"))?;
    let size_count: u64 = arg(&args, "--size-count")
        .unwrap_or("8")
        .parse()
        .map_err(|_| "--size-count must be an integer")?;
    let chain_tensor_mib: u64 = arg(&args, "--chain-tensor-mib")
        .unwrap_or("8")
        .parse()
        .map_err(|_| "--chain-tensor-mib must be an integer")?;
    // S13c: measure the controlled chain at more than one tensor size so the
    // per-hop row has an identifiable affine shape (fixed term and per-MiB
    // slope) instead of one point with a borrowed intercept.
    let chain_sizes_mib: Vec<u64> = match arg(&args, "--chain-sizes-mib") {
        Some(text) => u64_list(text)?,
        None => vec![chain_tensor_mib],
    };
    if chain_sizes_mib.is_empty() {
        return Err("--chain-sizes-mib must name at least one size".into());
    }
    let chain_depth: u64 = arg(&args, "--chain-depth")
        .unwrap_or("8")
        .parse()
        .map_err(|_| "--chain-depth must be an integer")?;
    // A chains-only invocation measures the hop sweep and nothing else. It is
    // the cheap way to (re)derive the graph hop rows without repeating the
    // whole restore-floor sweep.
    let chains_only = flag(&args, "--chains-only");
    let mut real_containers: Vec<PathBuf> = Vec::new();
    for (index, item) in args.iter().enumerate() {
        if item == "--real-container" {
            if let Some(path) = args.get(index + 1) {
                real_containers.push(PathBuf::from(path));
            }
        }
    }
    // A whole trajectory in one store: the shape the S13 court realises for the
    // all-independent plan, measured across every checkpoint (not just step 0).
    let real_corpus = arg(&args, "--real-corpus").map(PathBuf::from);
    let real_steps: Vec<u64> = arg(&args, "--real-steps")
        .map(u64_list)
        .transpose()?
        .unwrap_or_else(|| vec![0, 1, 2, 4, 8, 16, 64, 512, 1000]);
    let real_strategy = match arg(&args, "--real-strategy").unwrap_or("zstd") {
        "zstd" => ShardStrategy::Zstd,
        "planned" => ShardStrategy::Planned,
        other => {
            return Err(format!(
                "--real-strategy must be `zstd` or `planned` (got `{other}`)"
            ))
        }
    };
    // The sweeps must be measured on the representation the graph prices. The
    // checkpoint writer's `Planned` path is that representation; `zstd` is the
    // single-candidate hint path used by the decode-row calibration.
    let sweep_strategy = match arg(&args, "--sweep-strategy").unwrap_or("zstd") {
        "zstd" => ShardStrategy::Zstd,
        "planned" => ShardStrategy::Planned,
        other => {
            return Err(format!(
                "--sweep-strategy must be `zstd` or `planned` (got `{other}`)"
            ))
        }
    };
    let sweep_dtype = PayloadDtype::parse(arg(&args, "--sweep-dtype").unwrap_or("bf16"))?;

    // A chains-only run keeps the hop sweep and skips the expensive restore-floor
    // sweep, the real container and the trajectory. Nothing measured by the
    // skipped phases is claimed by the resulting receipt.
    let (counts, sizes, real_containers, real_corpus) = if chains_only {
        (Vec::new(), Vec::new(), Vec::new(), None)
    } else {
        (counts, sizes, real_containers, real_corpus)
    };

    if work.exists() {
        fs::remove_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    }
    fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;

    let mut points: Vec<Json> = Vec::new();
    let mut checks: std::collections::BTreeMap<String, bool> = Default::default();

    // ---- object-count sweep at fixed total bytes ---------------------------
    for count in &counts {
        let total_bytes = count_total_mib * MIB;
        if total_bytes % count != 0 {
            return Err(format!(
                "--count-total-mib {count_total_mib} ({total_bytes} bytes) is not divisible by object count {count}"
            ));
        }
        let per_tensor_bytes = total_bytes / count;
        let point = measure_point(
            &work,
            &format!("count-{count}"),
            "count",
            *count,
            per_tensor_bytes,
            sweep_strategy,
            sweep_dtype,
            reps,
        )?;
        points.push(point);
    }

    // ---- object-size sweep at fixed object count ---------------------------
    for size_mib in &sizes {
        let point = measure_point(
            &work,
            &format!("size-{size_mib}mib"),
            "size",
            size_count,
            *size_mib * MIB,
            sweep_strategy,
            sweep_dtype,
            reps,
        )?;
        points.push(point);
    }

    // ---- fit ---------------------------------------------------------------
    let fit = fit_points(&points, reps)?;
    let fit_json = fit.clone().unwrap_or(Json::Null);

    // ---- real-container validation points ---------------------------------
    let mut real_points: Vec<Json> = Vec::new();
    for (index, container) in real_containers.iter().enumerate() {
        let name = container
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(|stem| stem.to_owned())
            .unwrap_or_else(|| format!("real-{index}"));
        real_points.push(measure_real(
            &work,
            &format!("real-{name}"),
            container,
            real_strategy,
            reps,
        )?);
    }
    // ---- whole-trajectory validation --------------------------------------
    let mut trajectory_rows: Vec<Json> = Vec::new();
    if let Some(dir) = real_corpus.as_ref() {
        trajectory_rows = measure_trajectory(&work, dir, &real_steps, real_strategy, reps)?;
    }
    let mut trajectory_validation: Vec<Json> = Vec::new();
    if let Some(fit) = fit.as_ref() {
        let per_version = fit["per_version_ns"].as_u64().unwrap_or(0) as f64;
        let per_tensor = fit["per_tensor_ns"].as_u64().unwrap_or(0) as f64;
        let per_mib = fit["per_mib_ns"].as_u64().unwrap_or(0) as f64;
        for (index, point) in trajectory_rows.iter().enumerate() {
            let tensors = point["tensors"].as_u64().unwrap_or(0) as f64;
            let bytes = point["logical_bytes"].as_u64().unwrap_or(0) as f64;
            let residue = point["residue_ns"].as_i64().unwrap_or(0) as f64;
            let fitted = per_version + per_tensor * tensors + (per_mib / MIB_F) * bytes;
            let predicted = point["model_decode_ns"].as_u64().unwrap_or(0) as f64 + fitted;
            let measured = point["restore_median_ns"].as_u64().unwrap_or(0) as f64;
            trajectory_validation.push(json!({
                "index": index,
                "name": point["name"],
                "predicted_restore_ns": predicted.round() as u64,
                "measured_restore_ns": measured.round() as u64,
                "restore_error_ppm": if measured < 1.0 { 0 } else { (((predicted - measured) / measured) * 1_000_000.0).round() as i64 },
                "measured_residue_ns": residue as i64,
                "fitted_residue_ns": fitted.round() as i64,
                "residue_error_ppm": if fitted.abs() < 1.0 { 0 } else { (((residue - fitted) / fitted) * 1_000_000.0).round() as i64 },
            }));
        }
    }
    let mut real_validation: Vec<Json> = Vec::new();
    // The baked constants are the fit's per-tensor/per-MiB terms and the larger
    // of the fitted intercept and the measured one-tensor manifest read. That
    // rule keeps the whole-checkpoint term non-zero even when the intercept is
    // not identifiable from the sweep.
    let baked = fit.as_ref().map(|fit| {
        let per_version_fit = fit["per_version_ns"].as_u64().unwrap_or(0);
        let manifest_floor = points
            .iter()
            .find(|point| point["tensors"].as_u64() == Some(1))
            .and_then(|point| point["manifest_read_median_ns"].as_u64())
            .unwrap_or(0);
        json!({
            "per_version_ns": per_version_fit.max(manifest_floor).max(1),
            "per_tensor_ns": fit["per_tensor_ns"].as_u64().unwrap_or(0),
            "per_mib_ns": fit["per_mib_ns"].as_u64().unwrap_or(0),
            "per_version_source": if per_version_fit >= manifest_floor { "fit_intercept" } else { "measured_one_tensor_manifest_read" },
        })
    });
    if let Some(fit) = fit.as_ref() {
        let per_version = fit["per_version_ns"].as_u64().unwrap_or(0) as f64;
        let per_tensor = fit["per_tensor_ns"].as_u64().unwrap_or(0) as f64;
        let per_mib = fit["per_mib_ns"].as_u64().unwrap_or(0) as f64;
        for point in &real_points {
            let tensors = point["tensors"].as_u64().unwrap_or(0) as f64;
            let bytes = point["logical_bytes"].as_u64().unwrap_or(0) as f64;
            let residue = point["residue_ns"].as_i64().unwrap_or(0) as f64;
            let fitted = per_version + per_tensor * tensors + (per_mib / MIB_F) * bytes;
            let predicted_restore = point["model_decode_ns"].as_u64().unwrap_or(0) as f64 + fitted;
            let measured_restore = point["restore_median_ns"].as_u64().unwrap_or(0) as f64;
            real_validation.push(json!({
                "id": point["id"],
                "tensors": tensors as u64,
                "logical_bytes": bytes as u64,
                "measured_residue_ns": residue as i64,
                "fitted_residue_ns": fitted.round() as i64,
                "residue_error_ppm": if fitted.abs() < 1.0 { 0 } else { (((residue - fitted) / fitted) * 1_000_000.0).round() as i64 },
                "predicted_restore_ns": predicted_restore.round() as u64,
                "measured_restore_ns": measured_restore.round() as u64,
                "restore_error_ppm": if measured_restore.abs() < 1.0 { 0 } else { (((predicted_restore - measured_restore) / measured_restore) * 1_000_000.0).round() as i64 },
            }));
        }
    }

    // ---- controlled dependency chain (S13b hypothesis 3, S13c hop rows) ----
    // One chain per (change kind, tensor size). Two change kinds force the
    // planner's sparse-delta and weight-delta representations; several sizes
    // make the per-hop fixed term and per-MiB slope identifiable.
    let mut chain_sweep: Vec<Json> = Vec::new();
    for size_mib in &chain_sizes_mib {
        let sparse = measure_chain(
            &work,
            &format!("chain-sparse-{size_mib}mib"),
            *size_mib,
            chain_depth,
            ChangeKind::Sparse,
            sweep_dtype,
            reps,
        )?;
        let dense = measure_chain(
            &work,
            &format!("chain-dense-{size_mib}mib"),
            *size_mib,
            chain_depth,
            ChangeKind::Dense,
            sweep_dtype,
            reps,
        )?;
        chain_sweep.push(json!({"tensor_mib": size_mib, "sparse": sparse, "dense": dense}));
    }
    let hop_fit = fit_hops(&chain_sweep);
    let first = chain_sweep.first().cloned().unwrap_or(Json::Null);
    let chain_sparse = first["sparse"].clone();
    let chain_dense = first["dense"].clone();

    // ---- checks ------------------------------------------------------------
    checks.insert(
        "hop_samples_at_least_five_per_class".into(),
        hop_fit
            .as_array()
            .map(|rows| {
                rows.iter()
                    .all(|row| row.get("samples").and_then(Json::as_u64).unwrap_or(0) >= 5)
            })
            .unwrap_or(false),
    );
    checks.insert(
        "hop_sizes_at_least_two_per_class".into(),
        hop_fit
            .as_array()
            .map(|rows| {
                rows.iter().all(|row| {
                    row.get("sizes_bytes")
                        .and_then(Json::as_array)
                        .map(|sizes| sizes.len() >= 2)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false),
    );
    if !chains_only {
        checks.insert(
            "points_complete".into(),
            points.iter().all(|point| {
                point
                    .get("restore_median_ns")
                    .and_then(Json::as_u64)
                    .unwrap_or(0)
                    > 0
            }),
        );
        checks.insert(
            "every_point_exact".into(),
            points
                .iter()
                .chain(real_points.iter())
                .all(|point| point.get("exact_source_file").and_then(Json::as_bool) == Some(true)),
        );
        checks.insert(
            "fit_samples_at_least_five".into(),
            fit.as_ref()
                .and_then(|fit| fit.get("samples"))
                .and_then(Json::as_u64)
                .unwrap_or(0)
                >= 5,
        );
        checks.insert(
            "fit_terms_non_negative".into(),
            fit.as_ref().is_some_and(|fit| {
                fit.get("per_version_ns")
                    .and_then(Json::as_u64)
                    .unwrap_or(1)
                    > 0
                    && fit.get("per_tensor_ns").and_then(Json::as_u64).is_some()
                    && fit.get("per_mib_ns").and_then(Json::as_u64).unwrap_or(0) > 0
            }),
        );
        checks.insert(
            "trajectory_exact".into(),
            !trajectory_rows.is_empty()
                && trajectory_rows
                    .iter()
                    .all(|row| row.get("exact_source_file").and_then(Json::as_bool) == Some(true)),
        );
        checks.insert(
            "trajectory_floor_error_within_10_percent".into(),
            !trajectory_validation.is_empty()
                && trajectory_validation.iter().all(|row| {
                    row.get("restore_error_ppm")
                        .and_then(Json::as_i64)
                        .map(|ppm| ppm.abs() <= 100_000)
                        .unwrap_or(false)
                }),
        );
    }

    let receipt = json!({
        "schema": if chains_only { "weft.s13c.chain-hop.v1" } else { "weft.s13b.restore-floor.v1" },
        "label": if chains_only {
            "controlled dependency-chain hop cost at several tensor sizes; single-host bench court, synthetic weight-like payload, warm OS page cache"
        } else {
            "whole-checkpoint restore overhead; single-host bench court, synthetic weight-like BF16, warm OS page cache"
        },
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
        "commands": {
            "court": "scripts/docker-court.sh bench \"cd /work && cargo run --release --example calibrate_restore -- --out <out> --work <work>\"",
            "note": "every measurement is a real Store::restore_checkpoint of a complete checkpoint written with --shard-strategy zstd",
        },
        "configuration": {
            "reps": reps,
            "warmup_restores": 1,
            "object_counts": counts,
            "count_total_mib": count_total_mib,
            "object_mib": sizes,
            "size_count": size_count,
            "chain_tensor_mib": chain_tensor_mib,
            "chain_sizes_mib": chain_sizes_mib,
            "chain_depth": chain_depth,
            "chains_only": chains_only,
            "real_strategy": real_strategy.as_str(),
            "sweep_strategy": sweep_strategy.as_str(),
            "real_corpus": real_corpus.as_ref().map(|dir| dir.display().to_string()),
            "real_steps": real_steps,
            "sweep_dtype": sweep_dtype.as_str(),
            "representation": "zstd (one frame per shard; the all-independent floor representation)",
            "restore_note": "fresh read-only Store handle per repetition, opened before the timer starts; OS page cache warm; output written to a fresh Vec",
        },
        "method": {
            "fit": "least squares on residue = restore_median_ns - sum(measured decode rows); features [1, tensors, logical_bytes]",
            "decode_rows": cost_model::MEASURED_CALIBRATION,
            "fixed_terms": "per_version_ns is the intercept at zero tensors and zero bytes; per_tensor_ns is the count slope at fixed bytes; per_mib_ns is the byte slope at fixed count",
            "integer_output": "the fitted f64 coefficients are rounded to u64 nanoseconds; the store uses integer arithmetic only",
        },
        "points": points,
        "real": real_points,
        "real_validation": real_validation,
        "trajectory": trajectory_rows,
        "trajectory_validation": trajectory_validation,
        "fit": fit_json,
        "baked": baked,
        "chain": {
            "sparse": chain_sparse,
            "dense": chain_dense,
        },
        "chain_sweep": chain_sweep,
        "hop_fit": hop_fit,
        "checks": checks,
    });
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("{}", out.display());
    println!(
        "fit: per_version_ns={} per_tensor_ns={} per_mib_ns={} samples={}",
        fit_json["per_version_ns"],
        fit_json["per_tensor_ns"],
        fit_json["per_mib_ns"],
        fit_json["samples"]
    );
    Ok(())
}

/// One measured checkpoint: `count` tensors of `per_tensor_bytes` each.
fn measure_point(
    work: &Path,
    id: &str,
    kind: &str,
    count: u64,
    per_tensor_bytes: u64,
    strategy: ShardStrategy,
    dtype: PayloadDtype,
    reps: usize,
) -> Result<Json, String> {
    let dir = work.join(id);
    let container = dir.join("source.safetensors");
    fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;

    let elements = per_tensor_bytes / dtype.element_bytes();
    // Rank-1 keeps the safetensors header small and works for every tensor size
    // in the sweep (down to a few KiB).
    let shape = vec![elements];
    let payload = dtype.payload(elements, 0x5eed_0000 ^ per_tensor_bytes);
    let names: Vec<String> = (0..count)
        .map(|index| format!("tensor.{index:04}"))
        .collect();
    let mut payloads: Vec<Vec<u8>> = Vec::with_capacity(count as usize);
    for index in 0..count {
        // Distinct bytes per tensor, so no checkpoint-level dedup can hide a
        // per-tensor read.
        let mut data = payload.clone();
        data[0] ^= index as u8;
        data[1] = (index >> 8) as u8;
        payloads.push(data);
    }
    let mut entries = Vec::with_capacity(count as usize);
    for (index, data) in payloads.iter().enumerate() {
        entries.push(SafetensorsEntry {
            name: names[index].as_str(),
            dtype: dtype.as_str(),
            shape: shape.as_slice(),
            data: data.as_slice(),
        });
    }
    let file = safetensors::encode_file(&entries, None).map_err(|err| err.to_string())?;
    fs::write(&container, &file).map_err(|err| format!("{}: {err}", container.display()))?;
    let point = measure_source(work, id, kind, &container, strategy, reps)?;
    Ok(point)
}

/// Measure one real container (for example a public model checkpoint) with the
/// same zstd representation as the all-independent floor.
fn measure_real(
    work: &Path,
    id: &str,
    container: &Path,
    strategy: ShardStrategy,
    reps: usize,
) -> Result<Json, String> {
    if !container.is_file() {
        return Err(format!("missing container {}", container.display()));
    }
    measure_source(work, id, "real", container, strategy, reps)
}

fn measure_source(
    work: &Path,
    id: &str,
    kind: &str,
    container: &Path,
    strategy: ShardStrategy,
    reps: usize,
) -> Result<Json, String> {
    let dir = work.join(id);
    let store_path = dir.join("store");
    fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let container_bytes = fs::metadata(container)
        .map_err(|err| format!("{}: {err}", container.display()))?
        .len();
    let container_sha256 = if kind == "real" {
        let bytes = fs::read(container).map_err(|err| format!("{}: {err}", container.display()))?;
        Some(hex::encode(safetensors::sha256(&bytes)))
    } else {
        None
    };

    let mut store = Store::init(
        &store_path,
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .map_err(|err| err.to_string())?;
    let write = CheckpointWriteOptions {
        shards: 1,
        axis: 0,
        strategy,
        semantic_search: false,
        ..CheckpointWriteOptions::default()
    };
    let started = Instant::now();
    let report = store
        .write_checkpoint("point", container, &write)
        .map_err(|err| err.to_string())?;
    let write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let count = report.tensors.len() as u64;
    let logical_bytes = report
        .tensors
        .iter()
        .map(|tensor| tensor.element_bytes)
        .sum::<u64>();
    let per_tensor_bytes = logical_bytes / count.max(1);

    // The model's decode term, exactly as the graph planner sums it.
    let mut model_decode_ns = 0u64;
    let mut strategies: std::collections::BTreeMap<String, u64> = Default::default();
    for tensor in &report.tensors {
        let element_bytes = tensor.element_bytes;
        for object_id in &tensor.object_ids {
            let meta = store
                .tensor_metadata(*object_id)
                .map_err(|err| err.to_string())?;
            let strategy = TensorStrategy::parse(&meta.strategy).map_err(|err| err.to_string())?;
            *strategies.entry(meta.strategy.clone()).or_insert(0) += 1;
            model_decode_ns = model_decode_ns.saturating_add(cost_model::estimate_ns(
                strategy.cost_class(),
                element_bytes,
                CostModelKind::Measured,
            ));
        }
    }
    drop(store);

    // Manifest load cost only (the per-version component candidate).
    let mut manifest_ns = Vec::with_capacity(reps);
    for _ in 0..reps {
        let reader = Store::open(&store_path, OpenMode::ReadOnly).map_err(|err| err.to_string())?;
        let started = Instant::now();
        let loaded = reader
            .load_checkpoint("point")
            .map_err(|err| err.to_string())?;
        manifest_ns.push(started.elapsed().as_nanos());
        std::hint::black_box(loaded.tensors.len());
    }
    let manifest_median_ns = median_u128(&mut manifest_ns);

    // Warm-up restore (also proves the checkpoint decodes).
    let reader = Store::open(&store_path, OpenMode::ReadOnly).map_err(|err| err.to_string())?;
    let mut warm = Vec::new();
    let warm_report = reader
        .restore_checkpoint("point", &CheckpointRestoreOptions::default(), &mut warm)
        .map_err(|err| err.to_string())?;
    drop(reader);
    let exact = warm_report.exact_source_file;
    let store_bytes_read = warm_report.store_bytes_read;
    drop(warm);

    let mut times = Vec::with_capacity(reps);
    let mut last_report = None;
    for _ in 0..reps {
        let reader = Store::open(&store_path, OpenMode::ReadOnly).map_err(|err| err.to_string())?;
        let mut bytes = Vec::new();
        let started = Instant::now();
        let report = reader
            .restore_checkpoint("point", &CheckpointRestoreOptions::default(), &mut bytes)
            .map_err(|err| err.to_string())?;
        times.push(started.elapsed().as_nanos());
        std::hint::black_box(bytes.len());
        drop(bytes);
        last_report = Some(report);
    }
    let report = last_report.ok_or("no restore report")?;
    let restore_median_ns = median_u128(&mut times);
    let residue = restore_median_ns as i128 - model_decode_ns as i128;

    let point = json!({
        "id": id,
        "kind": kind,
        "container": container.display().to_string(),
        "container_sha256": container_sha256,
        "tensors": count,
        "per_tensor_bytes": per_tensor_bytes,
        "logical_bytes": logical_bytes,
        "container_bytes": container_bytes,
        "write_ms": write_ms,
        "model_decode_ns": model_decode_ns,
        "restore_median_ns": u64::try_from(restore_median_ns).unwrap_or(u64::MAX),
        "restore_reps_ns": times,
        "residue_ns": i64::try_from(residue).unwrap_or(if residue < 0 { i64::MIN } else { i64::MAX }),
        "manifest_read_median_ns": u64::try_from(manifest_median_ns).unwrap_or(u64::MAX),
        "store_bytes_read": store_bytes_read,
        "tensors_restored": report.tensors.len(),
        "strategies": strategies,
        "exact_source_file": exact,
    });
    fs::remove_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    Ok(point)
}

/// Measure the whole trajectory in one store: every checkpoint independent, the
/// shape the S13 court realises for the all-independent plan.
fn measure_trajectory(
    work: &Path,
    corpus: &Path,
    steps: &[u64],
    strategy: ShardStrategy,
    reps: usize,
) -> Result<Vec<Json>, String> {
    let dir = work.join("trajectory");
    let store_path = dir.join("store");
    fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut containers = Vec::new();
    for step in steps {
        let path = corpus.join(format!("pythia-70m-step{step}.safetensors"));
        if !path.is_file() {
            return Err(format!("missing corpus file {}", path.display()));
        }
        containers.push((*step, path));
    }
    let mut store = Store::init(
        &store_path,
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .map_err(|err| err.to_string())?;
    let write = CheckpointWriteOptions {
        shards: 1,
        axis: 0,
        strategy,
        semantic_search: false,
        ..CheckpointWriteOptions::default()
    };
    let mut rows = Vec::new();
    for (step, path) in &containers {
        let name = format!("step{step}");
        let report = store
            .write_checkpoint(&name, path, &write)
            .map_err(|err| err.to_string())?;
        let mut model_decode_ns = 0u64;
        let mut strategies: std::collections::BTreeMap<String, u64> = Default::default();
        for tensor in &report.tensors {
            for object_id in &tensor.object_ids {
                let meta = store
                    .tensor_metadata(*object_id)
                    .map_err(|err| err.to_string())?;
                let strategy =
                    TensorStrategy::parse(&meta.strategy).map_err(|err| err.to_string())?;
                *strategies.entry(meta.strategy.clone()).or_insert(0) += 1;
                model_decode_ns = model_decode_ns.saturating_add(cost_model::estimate_ns(
                    strategy.cost_class(),
                    tensor.element_bytes,
                    CostModelKind::Measured,
                ));
            }
        }
        let logical_bytes = report.tensors.iter().map(|t| t.element_bytes).sum::<u64>();
        let tensors = report.tensors.len() as u64;
        drop(store);

        let reader = Store::open(&store_path, OpenMode::ReadOnly).map_err(|e| e.to_string())?;
        let mut warm = Vec::new();
        let warm_report = reader
            .restore_checkpoint(&name, &CheckpointRestoreOptions::default(), &mut warm)
            .map_err(|e| e.to_string())?;
        drop(reader);
        drop(warm);
        let mut times = Vec::with_capacity(reps);
        let mut last = None;
        for _ in 0..reps {
            let reader = Store::open(&store_path, OpenMode::ReadOnly).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            let started = Instant::now();
            let report = reader
                .restore_checkpoint(&name, &CheckpointRestoreOptions::default(), &mut bytes)
                .map_err(|e| e.to_string())?;
            times.push(started.elapsed().as_nanos());
            std::hint::black_box(bytes.len());
            drop(bytes);
            last = Some(report);
        }
        let report = last.ok_or("no restore report")?;
        let restore_median_ns = median_u128(&mut times);
        let residue = restore_median_ns as i128 - model_decode_ns as i128;
        rows.push(json!({
            "name": name,
            "step": step,
            "tensors": tensors,
            "logical_bytes": logical_bytes,
            "model_decode_ns": model_decode_ns,
            "restore_median_ns": u64::try_from(restore_median_ns).unwrap_or(u64::MAX),
            "restore_reps_ns": times,
            "residue_ns": i64::try_from(residue).unwrap_or(if residue < 0 { i64::MIN } else { i64::MAX }),
            "store_bytes_read": report.store_bytes_read,
            "strategies": strategies,
            "exact_source_file": report.exact_source_file && warm_report.exact_source_file,
        }));
        store = Store::open(&store_path, OpenMode::ReadWrite).map_err(|e| e.to_string())?;
    }
    drop(store);
    fs::remove_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    Ok(rows)
}

/// Fit `residue = a + b * tensors + c * logical_bytes` by least squares.
///
/// Returns `Ok(None)` when the sweep cannot identify all three terms (for
/// example a one-point exploratory run); the raw points are still recorded.
fn fit_points(points: &[Json], reps: usize) -> Result<Option<Json>, String> {
    let rows: Vec<(f64, f64, f64, f64)> = points
        .iter()
        .map(|point| {
            let tensors = point["tensors"].as_u64().unwrap_or(0) as f64;
            let bytes = point["logical_bytes"].as_u64().unwrap_or(0) as f64;
            let residue = point["residue_ns"].as_i64().unwrap_or(0) as f64;
            (1.0, tensors, bytes, residue)
        })
        .collect();
    // Normal equations for a 3-parameter linear model.
    let mut ata = [[0.0f64; 3]; 3];
    let mut atb = [0.0f64; 3];
    for (c0, c1, c2, target) in &rows {
        let features = [*c0, *c1, *c2];
        for i in 0..3 {
            for j in 0..3 {
                ata[i][j] += features[i] * features[j];
            }
            atb[i] += features[i] * target;
        }
    }
    let Some(solved) = solve3(ata, atb) else {
        return Ok(None);
    };
    let per_version = solved[0].max(0.0).round();
    // A fitted fixed term is never allowed to be zero: zero would mean "free",
    // which is exactly the defect S13b fixes.
    let per_version = if per_version < 1.0 { 1.0 } else { per_version };
    let per_tensor = solved[1].max(0.0).round();
    let per_mib = solved[2].max(0.0) * MIB_F;
    let per_mib = per_mib.round().max(1.0);

    let mut residuals = Vec::with_capacity(rows.len());
    let mut worst_ppm = 0i64;
    for (index, (point, (_c0, c1, c2, target))) in points.iter().zip(rows.iter()).enumerate() {
        let predicted = per_version + per_tensor * *c1 + (per_mib / MIB_F) * *c2;
        let residue = *target;
        let ppm = if predicted.abs() < 1.0 {
            0
        } else {
            (((residue - predicted) / predicted) * 1_000_000.0).round() as i64
        };
        worst_ppm = worst_ppm.max(ppm.abs());
        residuals.push(json!({
            "point": index,
            "id": point["id"],
            "measured_residue_ns": residue as i64,
            "fitted_residue_ns": predicted.round() as i64,
            "error_ppm": ppm,
        }));
    }
    let samples = (points.len() * reps) as u64;
    Ok(Some(json!({
        "per_version_ns": per_version as u64,
        "per_tensor_ns": per_tensor as u64,
        "per_mib_ns": per_mib as u64,
        "samples": samples,
        "points": points.len(),
        "worst_residual_ppm": worst_ppm,
        "residuals": residuals,
    })))
}

fn solve3(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let mut pivot = col;
        for row in col + 1..3 {
            if a[row][col].abs() > a[pivot][col].abs() {
                pivot = row;
            }
        }
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let scale = a[col][col];
        for j in col..3 {
            a[col][j] /= scale;
        }
        b[col] /= scale;
        for row in 0..3 {
            if row == col {
                continue;
            }
            let factor = a[row][col];
            if factor == 0.0 {
                continue;
            }
            for j in col..3 {
                a[row][j] -= factor * a[col][j];
            }
            b[row] -= factor * b[col];
        }
    }
    Some(b)
}

/// Fit the per-hop decode rows from the controlled dependency chains.
///
/// One sample is one hop increment: `restore(version i) - restore(version i-1)`,
/// i.e. the cost of decoding version `i`'s frame against an already-decoded
/// base. The increment cancels both the per-version restore overhead and every
/// per-tensor cost that both versions pay, so the row has **no fixed term**:
/// its only identifiable parameter is the per-MiB rate. The baked row is the
/// least-squares line through the origin over every sample of that class.
///
/// The coefficient of determination is not the point here — reproducibility is,
/// so the raw `(bytes, ns)` pairs are recorded and the through-origin rate over
/// two independent runs is compared in the court.
fn fit_hops(chains: &[Json]) -> Json {
    let mut samples: std::collections::BTreeMap<String, Vec<(u64, u64)>> = Default::default();
    for chain in chains {
        for kind in ["sparse", "dense"] {
            let hops = chain[kind]["hops"].as_array().cloned().unwrap_or_default();
            for hop in hops {
                let Some(ns) = hop["measured_hop_delta_ns"].as_u64() else {
                    continue;
                };
                let Some(bytes) = hop["element_bytes"].as_u64() else {
                    continue;
                };
                if bytes == 0 {
                    continue;
                }
                let strategy = hop["strategy"].as_str().unwrap_or("unknown").to_owned();
                samples.entry(strategy).or_default().push((bytes, ns));
            }
        }
    }
    let rows: Vec<Json> = samples
        .into_iter()
        .map(|(strategy, samples)| {
            // Least squares through the origin: ns_per_mib = sum(b*ns) / sum(b^2).
            let numerator: f64 = samples
                .iter()
                .map(|(bytes, ns)| *bytes as f64 * *ns as f64)
                .sum();
            let denominator: f64 = samples
                .iter()
                .map(|(bytes, _)| *bytes as f64 * *bytes as f64)
                .sum();
            let per_mib = if denominator > 0.0 {
                ((numerator / denominator) * MIB_F).round().max(0.0) as u64
            } else {
                0
            };
            // Least squares with a free intercept, recorded for comparison only.
            let n = samples.len() as f64;
            let mean_x = samples.iter().map(|(b, _)| *b as f64).sum::<f64>() / n;
            let mean_y = samples.iter().map(|(_, ns)| *ns as f64).sum::<f64>() / n;
            let mut sxx = 0.0;
            let mut sxy = 0.0;
            for (bytes, ns) in &samples {
                let dx = *bytes as f64 - mean_x;
                sxx += dx * dx;
                sxy += dx * (*ns as f64 - mean_y);
            }
            let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
            let fit_fixed = (mean_y - slope * mean_x).max(0.0).round() as u64;
            let fit_per_mib = (slope * MIB_F).round().max(0.0) as u64;
            let errors: Vec<i64> = samples
                .iter()
                .map(|(bytes, ns)| {
                    let predicted = hop_ns(0, per_mib, *bytes) as f64;
                    if predicted <= 0.0 {
                        0
                    } else {
                        (((predicted - *ns as f64) / predicted) * 1_000_000.0).round() as i64
                    }
                })
                .collect();
            let mut sizes: Vec<u64> = samples.iter().map(|(b, _)| *b).collect();
            sizes.sort_unstable();
            sizes.dedup();
            let measured_hops: Vec<Json> = samples
                .iter()
                .map(|(bytes, ns)| json!({"element_bytes": bytes, "hop_ns": ns}))
                .collect();
            json!({
                "strategy": strategy,
                "samples": samples.len() as u64,
                "sizes_bytes": sizes,
                "measured_hops": measured_hops,
                "fixed_ns": 0,
                "ns_per_mib": per_mib,
                "max_under_prediction_ppm": errors.iter().copied().min().unwrap_or(0),
                "max_over_prediction_ppm": errors.iter().copied().max().unwrap_or(0),
                "least_squares_with_intercept": {
                    "fixed_ns": fit_fixed,
                    "ns_per_mib": fit_per_mib,
                },
            })
        })
        .collect();
    Json::Array(rows)
}

fn hop_ns(fixed_ns: u64, ns_per_mib: u64, bytes: u64) -> u64 {
    let scaled = (u128::from(bytes) * u128::from(ns_per_mib)) / 1_048_576u128;
    u64::try_from(scaled)
        .unwrap_or(u64::MAX)
        .saturating_add(fixed_ns)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Sparse,
    Dense,
}

/// A controlled dependency chain: version 0 stored with zstd, each later
/// version a delta on the previous one. Measures the realised per-hop cost.
fn measure_chain(
    work: &Path,
    id: &str,
    tensor_mib: u64,
    depth: u64,
    kind: ChangeKind,
    dtype: PayloadDtype,
    reps: usize,
) -> Result<Json, String> {
    let dir = work.join(id);
    let store_path = dir.join("store");
    fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let bytes = tensor_mib * MIB;
    let elements = bytes / dtype.element_bytes();
    let shape = vec![elements];
    let base = dtype.payload(elements, 0x1234_5678_0000 ^ bytes);

    let mut store = Store::init(
        &store_path,
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .map_err(|err| err.to_string())?;

    let mut payload = base.clone();
    let mut hops = Vec::new();
    let mut previous_name: Option<String> = None;
    let mut previous_restore_ns: Option<u128> = None;
    for step in 0..=depth {
        if step > 0 {
            match kind {
                ChangeKind::Sparse => {
                    // A handful of changed words: the case a real fine-tune
                    // trajectory produces (pythia step0 -> step1 is 19 words).
                    let words = payload.len() / 2;
                    let stride = (words / 8).max(1);
                    for index in (0..words).step_by(stride as usize) {
                        payload[index * 2] ^= 1;
                    }
                }
                ChangeKind::Dense => {
                    // Every third byte differs: forces a dense word delta.
                    for index in (0..payload.len()).step_by(3) {
                        payload[index] ^= 0x5a;
                    }
                }
            }
        }
        let container = dir.join(format!("step{step}.safetensors"));
        let file = safetensors::encode_file(
            &[SafetensorsEntry {
                name: "tensor",
                dtype: dtype.as_str(),
                shape: shape.as_slice(),
                data: payload.as_slice(),
            }],
            None,
        )
        .map_err(|err| err.to_string())?;
        fs::write(&container, &file).map_err(|err| format!("{}: {err}", container.display()))?;
        let name = format!("chain{step}");
        let write = CheckpointWriteOptions {
            shards: 1,
            axis: 0,
            strategy: ShardStrategy::Planned,
            semantic_search: false,
            causal_checkpoint: previous_name.clone(),
            ..CheckpointWriteOptions::default()
        };
        let report = store
            .write_checkpoint(&name, &container, &write)
            .map_err(|err| err.to_string())?;
        let strategy = report
            .tensors
            .first()
            .and_then(|tensor| tensor.object_ids.first().copied())
            .map(|object_id| store.tensor_metadata(object_id))
            .transpose()
            .map_err(|err| err.to_string())?
            .map(|meta| meta.strategy)
            .unwrap_or_else(|| "unknown".to_owned());
        let element_bytes = report
            .tensors
            .first()
            .map(|tensor| tensor.element_bytes)
            .unwrap_or(bytes);
        let strategy_parsed = TensorStrategy::parse(&strategy).map_err(|err| err.to_string())?;
        let model_hop_ns = cost_model::estimate_ns(
            strategy_parsed.cost_class(),
            element_bytes,
            CostModelKind::Measured,
        );
        drop(store);

        let reader = Store::open(&store_path, OpenMode::ReadOnly).map_err(|err| err.to_string())?;
        let mut warm = Vec::new();
        reader
            .restore_checkpoint(&name, &CheckpointRestoreOptions::default(), &mut warm)
            .map_err(|err| err.to_string())?;
        drop(warm);
        let mut times = Vec::with_capacity(reps);
        for _ in 0..reps {
            let reader =
                Store::open(&store_path, OpenMode::ReadOnly).map_err(|err| err.to_string())?;
            let mut out = Vec::new();
            let started = Instant::now();
            reader
                .restore_checkpoint(&name, &CheckpointRestoreOptions::default(), &mut out)
                .map_err(|err| err.to_string())?;
            times.push(started.elapsed().as_nanos());
            std::hint::black_box(out.len());
            drop(out);
        }
        let median = median_u128(&mut times);
        let measured_hop_ns = previous_restore_ns.map(|previous| median.saturating_sub(previous));
        hops.push(json!({
            "step": step,
            "name": name,
            "parent": if step == 0 { None } else { Some(step - 1) },
            "strategy": strategy,
            "element_bytes": element_bytes,
            "restore_median_ns": u64::try_from(median).unwrap_or(u64::MAX),
            "measured_hop_delta_ns": measured_hop_ns.and_then(|value| u64::try_from(value).ok()),
            "model_hop_ns": if step == 0 { 0 } else { model_hop_ns },
            "hop_error_ppm": match measured_hop_ns {
                Some(measured) if step > 0 => Some(
                    (((measured as f64 - model_hop_ns as f64) / model_hop_ns.max(1) as f64)
                        * 1_000_000.0)
                        .round() as i64,
                ),
                _ => None,
            },
        }));
        previous_restore_ns = Some(median);
        previous_name = Some(name);
        store = Store::open(&store_path, OpenMode::ReadWrite).map_err(|err| err.to_string())?;
        fs::remove_file(&container).map_err(|err| format!("{}: {err}", container.display()))?;
    }
    drop(store);
    fs::remove_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    Ok(json!({
        "tensor_mib": tensor_mib,
        "depth": depth,
        "kind": match kind { ChangeKind::Sparse => "sparse", ChangeKind::Dense => "dense" },
        "hops": hops,
    }))
}

fn median_u128(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    if values.is_empty() {
        0
    } else {
        values[values.len() / 2]
    }
}
