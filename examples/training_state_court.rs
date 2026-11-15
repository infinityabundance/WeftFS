//! T049 training-state court: real optimizer moments, gradients, attention mask.
//!
//! The Python side (`bench/scripts/training_state_court.py`) builds a real
//! transformer, runs a real backward pass and one AdamW step, and dumps
//! safetensors containers plus a JSON meta file. This harness ingests those
//! bytes through the shipped API, restores them, and measures the storage and
//! time trade against raw bytes and `zstd -19` on the same bytes.
//!
//! Nothing here is a synthetic byte string: the payloads come from autograd.
//! The fixture model is small (0.414 M parameters) and that is stated.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::safetensors::{self, SafetensorsFile};
use weftfs::store::{Options, Store};
use weftfs::training_state::{AdamwHyper, GradientEntry, OptimizerEntry, TrainingWriteOptions};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn zstd19(zstd: &str, input: &Path, out_dir: &Path) -> Result<u64, String> {
    let out = out_dir.join(format!(
        "{}.zst",
        input
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("input")
    ));
    let status = Command::new(zstd)
        .args(["-19", "-T1", "-q", "-f"])
        .arg(input)
        .arg("-o")
        .arg(&out)
        .status()
        .map_err(|err| format!("zstd: {err}"))?;
    if !status.success() {
        return Err(format!("zstd -19 failed: {status}"));
    }
    let bytes = fs::metadata(&out)
        .map(|meta| meta.len())
        .map_err(|err| format!("stat: {err}"))?;
    let _ = fs::remove_file(&out);
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1).map(str::to_owned))
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn strategy_counts(store: &Store, objects: &[weftfs::identity::Hash32]) -> Json {
    let mut counts = BTreeMap::new();
    for object in objects {
        let name = store
            .tensor_metadata(*object)
            .map(|meta| meta.strategy)
            .unwrap_or_else(|_| "unknown".to_owned());
        *counts.entry(name).or_insert(0u64) += 1;
    }
    Json::Object(
        counts
            .into_iter()
            .map(|(key, value)| (key, Json::from(value)))
            .collect(),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let fail = |message: &str| -> ! {
        eprintln!("training_state_court: {message}");
        std::process::exit(2);
    };
    let model_path = PathBuf::from(arg(&args, "--model").unwrap_or_else(|| fail("--model")));
    let moments_path = PathBuf::from(arg(&args, "--moments").unwrap_or_else(|| fail("--moments")));
    let gradients_path =
        PathBuf::from(arg(&args, "--gradients").unwrap_or_else(|| fail("--gradients")));
    let mask_path = PathBuf::from(arg(&args, "--mask").unwrap_or_else(|| fail("--mask")));
    let meta_path = PathBuf::from(arg(&args, "--meta").unwrap_or_else(|| fail("--meta")));
    let store_dir = PathBuf::from(arg(&args, "--store").unwrap_or_else(|| fail("--store")));
    let scratch = PathBuf::from(arg(&args, "--scratch").unwrap_or_else(|| fail("--scratch")));
    let restore_dir =
        PathBuf::from(arg(&args, "--restore-dir").unwrap_or_else(|| fail("--restore-dir")));
    let out_path = arg(&args, "--out").unwrap_or_else(|| fail("--out"));
    // The zstd baseline is optional: the Python court has libzstd through the
    // `zstandard` module but no `zstd` CLI, so it may fill these in itself.
    let zstd = arg(&args, "--zstd");

    let meta: Json = serde_json::from_slice(
        &fs::read(&meta_path).unwrap_or_else(|err| fail(&format!("meta: {err}"))),
    )
    .unwrap_or_else(|err| fail(&format!("meta json: {err}")));
    fs::create_dir_all(&scratch).unwrap();
    fs::create_dir_all(&restore_dir).unwrap();
    let _ = fs::remove_dir_all(&store_dir);
    let mut store =
        Store::init(&store_dir, opts()).unwrap_or_else(|err| fail(&format!("store: {err}")));

    // ---- model checkpoint (the real parameters) ------------------------------
    let before = store.retained_blob_bytes().unwrap();
    let started = Instant::now();
    let model_report = store
        .write_checkpoint(
            "tiny-transformer",
            &model_path,
            &CheckpointWriteOptions {
                strategy: ShardStrategy::Planned,
                deterministic_seed: Some([0x51u8; 32]),
                ..CheckpointWriteOptions::default()
            },
        )
        .unwrap_or_else(|err| fail(&format!("model checkpoint: {err}")));
    let model_write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let model_physical = store.retained_blob_bytes().unwrap() - before;
    let model_bytes = fs::metadata(&model_path).map(|m| m.len()).unwrap_or(0);

    let hyper = &meta["hyper"];
    let adamw = AdamwHyper::new(
        hyper["lr"].as_f64().unwrap_or(0.0),
        hyper["beta1"].as_f64().unwrap_or(0.0),
        hyper["beta2"].as_f64().unwrap_or(0.0),
        hyper["eps"].as_f64().unwrap_or(0.0),
        hyper["weight_decay"].as_f64().unwrap_or(0.0),
    );
    let step = meta["step"].as_u64().unwrap_or(0);

    // ---- optimizer moments (f32) ---------------------------------------------
    let moments = SafetensorsFile::parse(&moments_path)
        .unwrap_or_else(|err| fail(&format!("moments: {err}")));
    let mut grouped: BTreeMap<
        String,
        (
            Option<&weftfs::safetensors::SafetensorsTensor>,
            Option<&weftfs::safetensors::SafetensorsTensor>,
        ),
    > = BTreeMap::new();
    for tensor in &moments.tensors {
        let (name, which) = if let Some(name) = tensor.name.strip_suffix(".exp_avg_sq") {
            (name, 1)
        } else if let Some(name) = tensor.name.strip_suffix(".exp_avg") {
            (name, 0)
        } else {
            fail(&format!("unexpected moment key {}", tensor.name));
        };
        let slot = grouped.entry(name.to_owned()).or_default();
        let target = if which == 0 { &mut slot.0 } else { &mut slot.1 };
        *target = Some(tensor);
    }
    let mut moment_bytes: Vec<(String, Vec<u8>, Vec<u8>)> = Vec::new();
    let mut moment_raw = fs::File::create(scratch.join("moments.raw")).unwrap();
    for (name, (avg, sq)) in &grouped {
        let avg = avg.unwrap_or_else(|| fail("missing exp_avg"));
        let sq = sq.unwrap_or_else(|| fail("missing exp_avg_sq"));
        let avg_bytes = moments.read_tensor(avg).unwrap();
        let sq_bytes = moments.read_tensor(sq).unwrap();
        moment_raw.write_all(&avg_bytes).unwrap();
        moment_raw.write_all(&sq_bytes).unwrap();
        moment_bytes.push((name.clone(), avg_bytes, sq_bytes));
    }
    drop(moment_raw);
    // The entries borrow the buffers above, which stay alive through the write
    // and the restore comparison.
    let entries: Vec<OptimizerEntry<'_>> = moment_bytes
        .iter()
        .map(|(name, avg, sq)| OptimizerEntry {
            name,
            exp_avg: avg,
            exp_avg_sq: sq,
            step,
            moment_dtype: Some("f32"),
        })
        .collect();
    let moments_logical = moment_bytes
        .iter()
        .map(|(_, avg, sq)| (avg.len() + sq.len()) as u64)
        .sum::<u64>();
    let before = store.retained_blob_bytes().unwrap();
    let started = Instant::now();
    let optimizer_report = store
        .write_optimizer_state(
            "step",
            "tiny-transformer",
            adamw,
            &entries,
            &TrainingWriteOptions {
                record_provenance: true,
                ..TrainingWriteOptions::default()
            },
        )
        .unwrap_or_else(|err| fail(&format!("optimizer state: {err}")));
    let optimizer_write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let optimizer_physical = store.retained_blob_bytes().unwrap() - before;

    // ---- gradients (bf16) -----------------------------------------------------
    let gradients = SafetensorsFile::parse(&gradients_path)
        .unwrap_or_else(|err| fail(&format!("gradients: {err}")));
    let mut gradient_bytes = Vec::new();
    let mut gradient_raw = fs::File::create(scratch.join("gradients.raw")).unwrap();
    for tensor in &gradients.tensors {
        let bytes = gradients.read_tensor(tensor).unwrap();
        gradient_raw.write_all(&bytes).unwrap();
        gradient_bytes.push((tensor.name.clone(), bytes));
    }
    drop(gradient_raw);
    let gradient_entries: Vec<GradientEntry<'_>> = gradient_bytes
        .iter()
        .map(|(name, bytes)| GradientEntry {
            name,
            grad: bytes,
            grad_dtype: Some("bf16"),
        })
        .collect();
    let gradients_logical = gradient_bytes
        .iter()
        .map(|(_, bytes)| bytes.len() as u64)
        .sum::<u64>();
    let before = store.retained_blob_bytes().unwrap();
    let started = Instant::now();
    let gradient_report = store
        .write_gradients(
            "step",
            "tiny-transformer",
            step,
            meta["loss_kind"].as_str().unwrap_or("cross_entropy"),
            meta["loss"].as_f64().unwrap_or(0.0),
            meta["tokens"].as_u64().unwrap_or(0),
            meta["seq_len"].as_u64().unwrap_or(0),
            meta["global_norm"].as_f64(),
            &gradient_entries,
            &TrainingWriteOptions {
                record_provenance: true,
                ..TrainingWriteOptions::default()
            },
        )
        .unwrap_or_else(|err| fail(&format!("gradients: {err}")));
    let gradient_write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let gradient_physical = store.retained_blob_bytes().unwrap() - before;

    // ---- attention mask (u8) --------------------------------------------------
    let mask =
        SafetensorsFile::parse(&mask_path).unwrap_or_else(|err| fail(&format!("mask: {err}")));
    let mask_tensor = mask
        .tensors
        .first()
        .unwrap_or_else(|| fail("empty mask container"));
    let mask_bytes = mask.read_tensor(mask_tensor).unwrap();
    let before = store.retained_blob_bytes().unwrap();
    let started = Instant::now();
    let mask_report = store
        .write_attention_mask(
            meta["mask"]["name"].as_str().unwrap_or("causal-padded"),
            meta["mask"]["kind"].as_str().unwrap_or("hybrid"),
            &mask_tensor.shape,
            meta["mask"]["seq_len"].as_u64().unwrap_or(0),
            meta["mask"]["pad_positions"].as_u64().unwrap_or(0),
            &mask_bytes,
            meta["mask"]["interpretation"].as_str().unwrap_or(""),
            &TrainingWriteOptions::default(),
        )
        .unwrap_or_else(|err| fail(&format!("mask: {err}")));
    let mask_write_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mask_physical = store.retained_blob_bytes().unwrap() - before;
    let mask_logical = mask_bytes.len() as u64;

    // ---- restore and dump for the Python oracle ------------------------------
    let started = Instant::now();
    let restored_optimizer = store
        .restore_optimizer_state("step")
        .unwrap_or_else(|err| fail(&format!("restore optimizer: {err}")));
    let optimizer_restore_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let restored_gradients = store
        .restore_gradients("step")
        .unwrap_or_else(|err| fail(&format!("restore gradients: {err}")));
    let gradient_restore_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let restored_mask = store
        .read_attention_mask(&meta["mask"]["name"].as_str().unwrap_or("causal-padded"))
        .unwrap_or_else(|err| fail(&format!("restore mask: {err}")));
    let mask_restore_ms = started.elapsed().as_secs_f64() * 1000.0;

    let original_moments: BTreeMap<&str, (&Vec<u8>, &Vec<u8>)> = moment_bytes
        .iter()
        .map(|(name, avg, sq)| (name.as_str(), (avg, sq)))
        .collect();
    let mut optimizer_exact = restored_optimizer.params.len() == moment_bytes.len();
    for (name, avg, sq, restored_step) in &restored_optimizer.params {
        let (original_avg, original_sq) = original_moments
            .get(name.as_str())
            .unwrap_or_else(|| fail(&format!("restored unknown parameter {name}")));
        optimizer_exact &= avg == *original_avg && sq == *original_sq && *restored_step == step;
        fs::write(restore_dir.join(format!("{name}.exp_avg.bin")), avg).unwrap();
        fs::write(restore_dir.join(format!("{name}.exp_avg_sq.bin")), sq).unwrap();
    }
    let mut gradient_exact = restored_gradients.params.len() == gradient_bytes.len();
    for (name, bytes) in &restored_gradients.params {
        let original = gradients
            .tensor(name)
            .and_then(|tensor| gradients.read_tensor(tensor))
            .unwrap();
        gradient_exact &= *bytes == original;
        fs::write(restore_dir.join(format!("{name}.grad.bin")), bytes).unwrap();
    }
    let mask_exact = restored_mask.bytes == mask_bytes && restored_mask.shape == mask_tensor.shape;
    fs::write(restore_dir.join("mask.bin"), &restored_mask.bytes).unwrap();

    for (label, name) in [
        ("optimizer", "step"),
        ("gradients", "step"),
        (
            "mask",
            meta["mask"]["name"].as_str().unwrap_or("causal-padded"),
        ),
    ] {
        match label {
            "optimizer" => store
                .verify_optimizer_state(name)
                .unwrap_or_else(|err| fail(&format!("verify {label}: {err}"))),
            "gradients" => store
                .verify_gradients(name)
                .unwrap_or_else(|err| fail(&format!("verify {label}: {err}"))),
            _ => store
                .verify_attention_mask(name)
                .unwrap_or_else(|err| fail(&format!("verify {label}: {err}"))),
        };
    }
    let verification = store
        .verify(true)
        .unwrap_or_else(|err| fail(&format!("verify: {err}")));

    // ---- raw and zstd baselines ----------------------------------------------
    let mask_raw = scratch.join("mask.raw");
    fs::write(&mask_raw, &mask_bytes).unwrap();
    let (moments_zstd, gradients_zstd, mask_zstd, zstd_present) = match zstd.as_deref() {
        Some(zstd) => (
            zstd19(zstd, &scratch.join("moments.raw"), &scratch).unwrap_or_else(|err| fail(&err)),
            zstd19(zstd, &scratch.join("gradients.raw"), &scratch).unwrap_or_else(|err| fail(&err)),
            zstd19(zstd, &mask_raw, &scratch).unwrap_or_else(|err| fail(&err)),
            true,
        ),
        None => (0, 0, 0, false),
    };

    // Strategy mix for each artifact (objects named by the manifests).
    let opt_manifest = store.load_optimizer_state("step").unwrap();
    let mut opt_objects = Vec::new();
    for param in &opt_manifest.params {
        opt_objects.push(param.exp_avg.object);
        opt_objects.push(param.exp_avg_sq.object);
    }
    let grad_manifest = store.load_gradients("step").unwrap();
    let grad_objects: Vec<_> = grad_manifest
        .params
        .iter()
        .map(|param| param.grad.object)
        .collect();

    let mask_manifest = store
        .load_attention_mask(meta["mask"]["name"].as_str().unwrap_or("causal-padded"))
        .unwrap();

    let receipt = json!({
        "schema": "weft.training-state-court.v1",
        "generated_utc": chrono_like(),
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_default(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_default(),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_default(),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_default(),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_default(),
            "single_host": true,
            "warm_cache": true,
            "peak_rss_kib": peak_rss_kib(),
            "note": "CPU only, one host, warm page cache; the fixture model is 0.414 M parameters and the payloads come from a real torch backward pass",
        },
        "revision": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_default(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_default(),
        },
        "model": {
            "checkpoint": "tiny-transformer",
            "container_bytes": model_bytes,
            "tensors": model_report.tensors.len(),
            "manifest_root": model_report.manifest_root.to_hex(),
            "physical_bytes": model_physical,
            "write_ms": model_write_ms,
        },
        "optimizer_state": {
            "artifact": optimizer_report.artifact.to_hex(),
            "params": optimizer_report.params,
            "logical_bytes": moments_logical,
            "manifest_bytes": optimizer_report.manifest_bytes,
            "physical_minus_manifest_bytes": optimizer_physical - optimizer_report.manifest_bytes,
            "stored_physical_bytes": optimizer_physical,
            "zstd19_raw_bytes": if zstd_present { Json::from(moments_zstd) } else { Json::Null },
            "physical_vs_raw": optimizer_physical as f64 / moments_logical as f64,
            "physical_vs_zstd19": if zstd_present { Json::from(optimizer_physical as f64 / moments_zstd as f64) } else { Json::Null },
            "write_ms": optimizer_write_ms,
            "restore_ms": optimizer_restore_ms,
            "restore_exact": optimizer_exact,
            "strategy_counts": strategy_counts(&store, &opt_objects),
            "moment_dtype": "f32 (explicit; the model parameters are bf16)",
            "adamw": {"lr": hyper["lr"], "beta1": hyper["beta1"], "beta2": hyper["beta2"], "eps": hyper["eps"], "weight_decay": hyper["weight_decay"], "step": step},
        },
        "gradients": {
            "artifact": gradient_report.artifact.to_hex(),
            "params": gradient_report.params,
            "logical_bytes": gradients_logical,
            "manifest_bytes": gradient_report.manifest_bytes,
            "physical_minus_manifest_bytes": gradient_physical - gradient_report.manifest_bytes,
            "stored_physical_bytes": gradient_physical,
            "zstd19_raw_bytes": if zstd_present { Json::from(gradients_zstd) } else { Json::Null },
            "physical_vs_raw": gradient_physical as f64 / gradients_logical as f64,
            "physical_vs_zstd19": if zstd_present { Json::from(gradient_physical as f64 / gradients_zstd as f64) } else { Json::Null },
            "write_ms": gradient_write_ms,
            "restore_ms": gradient_restore_ms,
            "restore_exact": gradient_exact,
            "strategy_counts": strategy_counts(&store, &grad_objects),
            "grad_dtype": "bf16 (explicit)",
            "loss_bits": meta["loss_bits"],
            "loss": meta["loss"],
            "tokens": meta["tokens"],
            "seq_len": meta["seq_len"],
            "global_norm": meta["global_norm"],
        },
        "attention_mask": {
            "artifact": mask_report.artifact.to_hex(),
            "kind": mask_manifest.kind,
            "shape": mask_manifest.shape,
            "seq_len": mask_manifest.seq_len,
            "pad_positions": mask_manifest.pad_positions,
            "logical_bytes": mask_logical,
            "manifest_bytes": mask_report.manifest_bytes,
            "physical_minus_manifest_bytes": mask_physical - mask_report.manifest_bytes,
            "stored_physical_bytes": mask_physical,
            "zstd19_raw_bytes": if zstd_present { Json::from(mask_zstd) } else { Json::Null },
            "physical_vs_raw": mask_physical as f64 / mask_logical as f64,
            "physical_vs_zstd19": if zstd_present { Json::from(mask_physical as f64 / mask_zstd as f64) } else { Json::Null },
            "write_ms": mask_write_ms,
            "restore_ms": mask_restore_ms,
            "restore_exact": mask_exact,
            "strategy_counts": strategy_counts(&store, &[mask_manifest.mask.object]),
        },
        "training_state_total": {
            "logical_bytes": moments_logical + gradients_logical + mask_logical,
            "stored_physical_bytes": optimizer_physical + gradient_physical + mask_physical,
            "zstd19_total_bytes": if zstd_present { Json::from(moments_zstd + gradients_zstd + mask_zstd) } else { Json::Null },
        },
        "verify": {
            "full": true,
            "checked_objects": verification.checked_objects,
            "checked_manifests": verification.checked_manifests,
            "checked_chunks": verification.checked_chunks,
            "checked_representations": verification.checked_representations,
            "problems": verification.problems.len(),
        },
        "store_dir": store_dir.display().to_string(),
        "restore_dir": restore_dir.display().to_string(),
        "labels": {"synthetic_fixture_model": true, "real_backward_pass": true},
    });
    fs::write(&out_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    println!("training_state_court: wrote {out_path}");
}

fn chrono_like() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, seconds) = (now / 86_400, now % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

// Howard Hinnant's civil_from_days.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
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
