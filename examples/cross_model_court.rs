//! T051 cross-model lossless structural delta court.
//!
//! For each pair it measures:
//!   * `delta`  — a whole-byte mode-3 DELTA frame (the module under test) and
//!                whether the policy accepts or declines it;
//!   * `patch`  — `zstd --patch-from` at level 19 (level 12 if 19 is killed);
//!   * `store`  — the shipped planner writing the target checkpoint with the
//!                base as a causal parent, all manifests and indexes included;
//!   * `zstd`   — independent `zstd -19` of the target (no base).
//!
//! Pairs include two accepts (consecutive and distant training checkpoints, and
//! an independent fine-tune slice) and three declines (different dtype AND
//! size, different shape with the same dtype, and a synthetic pair with nothing
//! in common). The synthetic pair is labelled synthetic everywhere.
//!
//! Whole-file Qwen payloads do not fit the 3 GiB court, so the fine-tune delta
//! is measured on a 256 MiB contiguous slice and the store arm covers the whole
//! model. That is stated in every cell.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value as Json};

use weftfs::checkpoint::{CheckpointWriteOptions, ShardStrategy};
use weftfs::cross_model::{
    plan_cross_model_delta, tensor_mode_eligibility, CrossModelDecision, DeclineReason,
};
use weftfs::safetensors::SafetensorsFile;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::TensorStrategy;

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

fn read_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>, String> {
    let mut file = fs::File::open(path).map_err(|err| format!("open {}: {err}", path.display()))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| format!("seek: {err}"))?;
    let mut out = vec![0u8; len as usize];
    file.read_exact(&mut out)
        .map_err(|err| format!("read: {err}"))?;
    Ok(out)
}

fn data_start(path: &Path) -> Result<u64, String> {
    SafetensorsFile::parse(path)
        .map(|file| file.data_start)
        .map_err(|err| err.message().to_owned())
}

fn tensor_bytes(path: &Path, name: &str) -> Result<(Vec<u8>, String, Vec<u64>), String> {
    let file = SafetensorsFile::parse(path).map_err(|err| err.message().to_owned())?;
    let tensor = file.tensor(name).map_err(|err| err.message().to_owned())?;
    let bytes = file
        .read_tensor(tensor)
        .map_err(|err| err.message().to_owned())?;
    Ok((bytes, tensor.dtype.clone(), tensor.shape.clone()))
}

struct ZstdResult {
    bytes: Option<u64>,
    level: i32,
    failed_level_19: bool,
    error: Option<String>,
}

fn run_zstd(zstd: &str, level: i32, patch_base: Option<&Path>, target: &Path, out: &Path) -> Result<u64, String> {
    let mut command = Command::new(zstd);
    command.arg(format!("-{level}")).arg("-T1").arg("-q").arg("-f");
    if let Some(base) = patch_base {
        command.arg(format!("--patch-from={}", base.display()));
    }
    command.arg(target).arg("-o").arg(out);
    let status = command.status().map_err(|err| format!("zstd: {err}"))?;
    if !status.success() {
        return Err(format!("zstd exited {status}"));
    }
    fs::metadata(out)
        .map(|meta| meta.len())
        .map_err(|err| format!("stat: {err}"))
}

fn patch_from(zstd: &str, base: &Path, target: &Path, out_dir: &Path, tag: &str) -> ZstdResult {
    let out = out_dir.join(format!("{tag}.patch.zst"));
    match run_zstd(zstd, 19, Some(base), target, &out) {
        Ok(bytes) => {
            let _ = fs::remove_file(&out);
            ZstdResult {
                bytes: Some(bytes),
                level: 19,
                failed_level_19: false,
                error: None,
            }
        }
        Err(error) => {
            let out12 = out_dir.join(format!("{tag}.patch12.zst"));
            let bytes = run_zstd(zstd, 12, Some(base), target, &out12).ok();
            let _ = fs::remove_file(&out12);
            ZstdResult {
                bytes,
                level: 12,
                failed_level_19: true,
                error: Some(error),
            }
        }
    }
}

fn plain_zstd(zstd: &str, target: &Path, out_dir: &Path, tag: &str) -> Option<u64> {
    let out = out_dir.join(format!("{tag}.zst"));
    let bytes = run_zstd(zstd, 19, None, target, &out).ok();
    let _ = fs::remove_file(&out);
    bytes
}

fn strategy_counts(store: &Store, checkpoint: &str) -> Json {
    let manifest = match store.checkpoint_manifest(checkpoint) {
        Ok(manifest) => manifest,
        Err(_) => return Json::Null,
    };
    let mut counts = std::collections::BTreeMap::new();
    for tensor in &manifest.tensors {
        for shard in &tensor.shards {
            let name = store
                .tensor_metadata(shard.object_id)
                .map(|meta| meta.strategy)
                .unwrap_or_else(|_| "unknown".to_owned());
            *counts.entry(name).or_insert(0u64) += 1;
        }
    }
    Json::Object(
        counts
            .into_iter()
            .map(|(key, value)| (key, Json::from(value)))
            .collect(),
    )
}

/// Write `base` then `target` into a fresh store and return the marginal bytes.
fn store_pair(
    root: &Path,
    base: &Path,
    target: &Path,
    forced: Option<TensorStrategy>,
    seed: u8,
) -> Result<(u64, u64, Json, String), String> {
    let _ = fs::remove_dir_all(root);
    let mut store = Store::init(root, opts()).map_err(|err| err.message().to_owned())?;
    let base_report = store
        .write_checkpoint(
            "base",
            base,
            &CheckpointWriteOptions {
                strategy: ShardStrategy::Planned,
                deterministic_seed: Some([seed; 32]),
                ..CheckpointWriteOptions::default()
            },
        )
        .map_err(|err| format!("base checkpoint: {err}"))?;
    let base_physical = store
        .retained_blob_bytes()
        .map_err(|err| err.message().to_owned())?;
    let before = base_physical;
    let target_report = store
        .write_checkpoint(
            "target",
            target,
            &CheckpointWriteOptions {
                strategy: ShardStrategy::Planned,
                causal_checkpoint: Some("base".to_owned()),
                forced_strategy: forced,
                deterministic_seed: Some([seed.wrapping_add(1); 32]),
                ..CheckpointWriteOptions::default()
            },
        )
        .map_err(|err| format!("target checkpoint: {err}"))?;
    let after = store
        .retained_blob_bytes()
        .map_err(|err| err.message().to_owned())?;
    let verification = store.verify(true).map_err(|err| err.message().to_owned())?;
    if !verification.ok {
        return Err(format!(
            "store pair failed verification: {} problems",
            verification.problems.len()
        ));
    }
    Ok((
        base_physical,
        after - before,
        strategy_counts(&store, "target"),
        format!(
            "{} -> {} ({} tensors)",
            base_report.manifest_root.to_hex(),
            target_report.manifest_root.to_hex(),
            target_report.tensors.len()
        ),
    ))
}

fn delta_cell(name: &str, base: &[u8], target: &[u8], note: &str) -> Json {
    let decision = plan_cross_model_delta(target, base, Default::default());
    match decision {
        CrossModelDecision::Accept(delta) => {
            // Replay the accepted frame and compare it with the target byte for
            // byte; a delta that is small but wrong is not evidence.
            let limits = weftcodec::Limits::default();
            let frame = weftcodec::modes::encode_delta(target, base, &limits)
                .unwrap_or_else(|err| panic!("encode accepted delta: {err}"));
            let replay = weftcodec::frame::Frame::parse(&frame, &limits)
                .and_then(|parsed| {
                    weftcodec::modes::decode_delta_ops(
                        parsed.body,
                        base,
                        parsed.header.decoded_len,
                        &limits,
                    )
                })
                .unwrap_or_else(|err| panic!("decode accepted delta: {err}"));
            json!({
            "pair": name,
            "note": note,
            "decision": "accept",
            "base_bytes": base.len(),
            "target_bytes": target.len(),
            "frame_bytes": delta.frame_bytes,
            "frame_ratio": delta.frame_ratio,
            "copy_bytes": delta.ops.copy_bytes,
            "insert_bytes": delta.ops.insert_bytes,
            "copy_ops": delta.ops.copy_ops,
            "insert_ops": delta.ops.insert_ops,
            "matched_fraction": delta.ops.matched_fraction(),
            "replay_exact": replay == target,
        })
        }
        CrossModelDecision::Decline(reason) => {
            let matched = match &reason {
                DeclineReason::NothingInCommon {
                    matched_fraction, ..
                } => Json::from(*matched_fraction),
                _ => Json::Null,
            };
            json!({
                "pair": name,
                "note": note,
                "decision": "decline",
                "reason": reason.as_str(),
                "reason_detail": format!("{reason:?}"),
                "matched_fraction": matched,
                "base_bytes": base.len(),
                "target_bytes": target.len(),
            })
        }
    }
}

fn synthetic(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
    }
    out.truncate(len);
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let fail = |message: &str| -> ! {
        eprintln!("cross_model_court: {message}");
        std::process::exit(2);
    };
    let corpus = PathBuf::from(arg(&args, "--corpus").unwrap_or_else(|| fail("--corpus")));
    let out_path = arg(&args, "--out").unwrap_or_else(|| fail("--out"));
    let scratch = PathBuf::from(arg(&args, "--scratch").unwrap_or_else(|| fail("--scratch")));
    let zstd = arg(&args, "--zstd").unwrap_or_else(|| "/usr/bin/zstd".to_owned());
    let slice_mib: u64 = arg(&args, "--slice-mib")
        .and_then(|text| text.parse().ok())
        .unwrap_or(256);
    fs::create_dir_all(&scratch).unwrap_or_else(|err| fail(&format!("scratch: {err}")));

    let pythia0 = corpus.join(".docker-cache/real-corpus/pythia-70m-step0.safetensors");
    let pythia1 = corpus.join(".docker-cache/real-corpus/pythia-70m-step1.safetensors");
    let pythia8 = corpus.join(".docker-cache/real-corpus/pythia-70m-step8.safetensors");
    let qwen_base = corpus.join(".docker-cache/real-corpus/Qwen2.5-0.5B.safetensors");
    let qwen_instruct = corpus.join(".docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors");
    for path in [&pythia0, &pythia1, &pythia8, &qwen_base, &qwen_instruct] {
        if !path.exists() {
            fail(&format!("missing corpus {}", path.display()));
        }
    }

    let mut cells = Vec::new();

    // ------------------------------------------------------------------ accept 1
    {
        let base = fs::read(&pythia0).unwrap_or_else(|err| fail(&format!("pythia0: {err}")));
        let target = fs::read(&pythia1).unwrap_or_else(|err| fail(&format!("pythia1: {err}")));
        let patch = patch_from(&zstd, &pythia0, &pythia1, &scratch, "pythia01");
        let z = plain_zstd(&zstd, &pythia1, &scratch, "pythia1");
        let mut cell = delta_cell(
            "pythia-step0-step1",
            &base,
            &target,
            "consecutive training checkpoints of one model, F32, identical shapes",
        );
        cell["zstd19_target_bytes"] = Json::from(z);
        cell["patch_from"] = json!({
            "bytes": patch.bytes,
            "level": patch.level,
            "level_19_failed": patch.failed_level_19,
            "error": patch.error,
        });
        let store_root = scratch.join("store-pythia01");
        let (base_physical, marginal, strategies, roots) =
            store_pair(&store_root, &pythia0, &pythia1, None, 0x31)
                .unwrap_or_else(|err| fail(&err));
        cell["store"] = json!({
            "base_physical_bytes": base_physical,
            "target_marginal_bytes": marginal,
            "strategy_counts": strategies,
            "roots": roots,
            "note": "marginal retained bytes include every manifest, frame, index segment and commit",
        });
        let _ = fs::remove_dir_all(&store_root);
        cells.push(cell);
    }

    // ------------------------------------------------------------------ accept 2
    {
        let base = fs::read(&pythia0).unwrap_or_else(|err| fail(&format!("pythia0: {err}")));
        let target = fs::read(&pythia8).unwrap_or_else(|err| fail(&format!("pythia8: {err}")));
        let patch = patch_from(&zstd, &pythia0, &pythia8, &scratch, "pythia08");
        let z = plain_zstd(&zstd, &pythia8, &scratch, "pythia8");
        let mut cell = delta_cell(
            "pythia-step0-step8",
            &base,
            &target,
            "distant training checkpoints of one model, F32, identical shapes",
        );
        cell["zstd19_target_bytes"] = Json::from(z);
        cell["patch_from"] = json!({
            "bytes": patch.bytes,
            "level": patch.level,
            "level_19_failed": patch.failed_level_19,
            "error": patch.error,
        });
        let mut forced = Vec::new();
        for (label, strategy, seed) in [
            ("weight_delta", TensorStrategy::WeightDelta, 0x41u8),
            ("sparse_delta", TensorStrategy::SparseDelta, 0x51u8),
        ] {
            let store_root = scratch.join(format!("store-pythia08-{label}"));
            match store_pair(&store_root, &pythia0, &pythia8, Some(strategy), seed) {
                Ok((base_physical, marginal, strategies, roots)) => forced.push(json!({
                    "arm": label,
                    "base_physical_bytes": base_physical,
                    "target_marginal_bytes": marginal,
                    "strategy_counts": strategies,
                    "roots": roots,
                })),
                Err(error) => forced.push(json!({"arm": label, "error": error})),
            }
            let _ = fs::remove_dir_all(&store_root);
        }
        cell["forced_arms"] = Json::Array(forced);
        cells.push(cell);
    }

    // ------------------------------------------------- accept 3: fine-tune slice
    {
        let base_start = data_start(&qwen_base).unwrap_or_else(|err| fail(&err));
        let target_start = data_start(&qwen_instruct).unwrap_or_else(|err| fail(&err));
        let len = slice_mib * 1024 * 1024;
        let base = read_range(&qwen_base, base_start, len).unwrap_or_else(|err| fail(&err));
        let target = read_range(&qwen_instruct, target_start, len).unwrap_or_else(|err| fail(&err));
        let base_path = scratch.join("qwen-base-slice.bin");
        let target_path = scratch.join("qwen-instruct-slice.bin");
        fs::write(&base_path, &base).unwrap();
        fs::write(&target_path, &target).unwrap();
        let patch = patch_from(&zstd, &base_path, &target_path, &scratch, "qwen-slice");
        let z = plain_zstd(&zstd, &target_path, &scratch, "qwen-slice");
        let mut cell = delta_cell(
            "qwen-finetune-slice",
            &base,
            &target,
            "independent fine-tune (base vs Instruct), BF16, identical shapes; the first 256 MiB of each container after its header",
        );
        cell["zstd19_target_bytes"] = Json::from(z);
        cell["patch_from"] = json!({
            "bytes": patch.bytes,
            "level": patch.level,
            "level_19_failed": patch.failed_level_19,
            "error": patch.error,
        });
        cell["slice_bytes"] = Json::from(len);
        cells.push(cell);
    }

    // --------------------------------------- store arm over the whole fine-tune
    {
        let store_root = scratch.join("store-qwen-finetune");
        let (base_physical, marginal, strategies, roots) =
            store_pair(&store_root, &qwen_base, &qwen_instruct, None, 0x61)
                .unwrap_or_else(|err| fail(&err));
        let z = plain_zstd(&zstd, &qwen_instruct, &scratch, "qwen-instruct");
        cells.push(json!({
            "pair": "qwen-finetune-store",
            "note": "whole Qwen 0.5B base vs Instruct through the shipped planner; the whole-file byte delta cannot be built in 3 GiB, so only the store arm is measured",
            "decision": "store-arm-only",
            "zstd19_target_bytes": z,
            "store": {
                "base_physical_bytes": base_physical,
                "target_marginal_bytes": marginal,
                "strategy_counts": strategies,
                "roots": roots,
            },
        }));
        let _ = fs::remove_dir_all(&store_root);
    }

    // ----------------------------------------------------------------- decline 1
    {
        // Different model size, different dtype and different shapes: the 0.5B
        // BF16 embedding against the 70M F32 embedding.
        let (qwen, qwen_dtype, qwen_shape) =
            tensor_bytes(&qwen_base, "model.embed_tokens.weight").unwrap_or_else(|err| fail(&err));
        let (pythia, pythia_dtype, pythia_shape) =
            tensor_bytes(&pythia0, "gpt_neox.embed_in.weight").unwrap_or_else(|err| fail(&err));
        let eligibility =
            tensor_mode_eligibility(&pythia_dtype, &pythia_shape, &qwen_dtype, &qwen_shape)
                .err()
                .map(|err| json!({"class": err.class().as_str(), "message": err.message()}));
        let mut cell = delta_cell(
            "cross-size-dtype-embedding",
            &pythia,
            &qwen,
            "Qwen2.5-0.5B BF16 embedding (151936x896) against pythia-70m F32 embedding (50304x512); different size, dtype and shape",
        );
        cell["base_tensor"] = json!({"dtype": pythia_dtype, "shape": pythia_shape});
        cell["target_tensor"] = json!({"dtype": qwen_dtype, "shape": qwen_shape});
        cell["tensor_mode_eligibility"] = eligibility.unwrap_or(Json::Null);
        cells.push(cell);
    }

    // ----------------------------------------------------------------- decline 2
    {
        let (qwen_embed, embed_dtype, embed_shape) =
            tensor_bytes(&qwen_base, "model.embed_tokens.weight").unwrap_or_else(|err| fail(&err));
        let (qwen_mlp, mlp_dtype, mlp_shape) =
            tensor_bytes(&qwen_base, "model.layers.0.mlp.down_proj.weight")
                .unwrap_or_else(|err| fail(&err));
        let eligibility =
            tensor_mode_eligibility(&embed_dtype, &embed_shape, &mlp_dtype, &mlp_shape)
                .err()
                .map(|err| json!({"class": err.class().as_str(), "message": err.message()}));
        let mut cell = delta_cell(
            "same-dtype-different-shape",
            &qwen_mlp,
            &qwen_embed,
            "two Qwen BF16 tensors of different shapes; the shape-exact modes must refuse and the byte delta must decline",
        );
        cell["tensor_mode_eligibility"] = eligibility.unwrap_or(Json::Null);
        cells.push(cell);
    }

    // ----------------------------------------------------------------- decline 3
    {
        let base = synthetic(4 * 1024 * 1024, 0x1111);
        let target = synthetic(4 * 1024 * 1024, 0x2222);
        let mut cell = delta_cell(
            "synthetic-nothing-in-common",
            &base,
            &target,
            "SYNTHETIC: two deterministic random 4 MiB buffers with nothing in common; not model data",
        );
        cell["synthetic"] = Json::from(true);
        cells.push(cell);
    }

    let accepted = cells
        .iter()
        .filter(|cell| cell["decision"] == "accept")
        .count();
    let declined = cells
        .iter()
        .filter(|cell| cell["decision"] == "decline")
        .count();
    let all_declines_typed = cells
        .iter()
        .filter(|cell| cell["decision"] == "decline")
        .all(|cell| cell["reason"].as_str().is_some());

    let receipt = json!({
        "schema": "weft.cross-model-court.v1",
        "generated_utc": chrono_like(),
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_default(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_default(),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_default(),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_default(),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_default(),
            "single_host": true,
            "warm_cache": true,
            "note": "one host, warm page cache; whole-file Qwen payloads do not fit 3 GiB, so that pair is a slice plus a whole-model store arm",
        },
        "revision": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_default(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_default(),
        },
        "policy": {
            "min_matched_fraction": weftfs::cross_model::DEFAULT_MIN_MATCHED_FRACTION,
            "max_frame_ratio": weftfs::cross_model::DEFAULT_MAX_FRAME_RATIO,
        },
        "cells": cells,
        "summary": {
            "accepted": accepted,
            "declined": declined,
            "all_declines_have_a_typed_reason": all_declines_typed,
        },
        "labels": {
            "synthetic_cells": ["synthetic-nothing-in-common"],
            "real_cells": ["pythia-step0-step1", "pythia-step0-step8", "qwen-finetune-slice", "qwen-finetune-store", "cross-size-dtype-embedding", "same-dtype-different-shape"],
        },
    });
    fs::write(&out_path, serde_json::to_vec_pretty(&receipt).unwrap())
        .unwrap_or_else(|err| fail(&format!("write receipt: {err}")));
    println!("cross_model_court: wrote {out_path}");
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
