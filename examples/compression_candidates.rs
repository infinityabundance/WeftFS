//! Plan-only compression-candidate court for one checkpoint pair.
//!
//! Measures the per-checkpoint physical bytes of three planner configurations
//! on real safetensors weights, without writing a checkpoint:
//!
//! 1. `current`   — the pre-Stage-3 planner (no level-19 ZSTD, no patch);
//! 2. `zstd19`    — plus the level-19 ZSTD candidate;
//! 3. `zstd19+patch` — plus the whole-base ZSTD patch candidate.
//!
//! The base checkpoint is written once per tensor with the fast raw policy so
//! the patch candidate has a causal base to name; those writes are not part of
//! any measured total.
//!
//! Usage (inside the bench court):
//!
//! ```text
//! compression_candidates --base BASE.safetensors --next NEXT.safetensors \
//!   --label qwen --out receipt.json [--zstd /usr/bin/zstd]
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::json;

use weftfs::identity::Hash32;
use weftfs::safetensors::SafetensorsFile;
use weftfs::state::State;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{
    AccessPattern, FastCodec, TensorPlanOptions, TensorWriteSpec, WritePolicy,
};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

/// Compressed size of one file at a level, via `zstd -<level>`.
fn zstd_file(zstd: &str, level: i32, input: &Path, out: &Path) -> Result<u64, String> {
    let level = format!("-{level}");
    let status = Command::new(zstd)
        .args([level.as_str(), "-T1", "-q", "-f"])
        .arg(input)
        .arg("-o")
        .arg(out)
        .status()
        .map_err(|err| format!("zstd: {err}"))?;
    if !status.success() {
        return Err(format!("zstd {level} failed: {status}"));
    }
    fs::metadata(out)
        .map(|meta| meta.len())
        .map_err(|err| format!("stat {}: {err}", out.display()))
}

/// Compressed size of `input` patched against `base`, via
/// `zstd -<level> --patch-from base`.
fn zstd_patch_file(
    zstd: &str,
    level: i32,
    base: &Path,
    input: &Path,
    out: &Path,
) -> Result<u64, String> {
    let level = format!("-{level}");
    let status = Command::new(zstd)
        .args([level.as_str(), "-T1", "-q", "-f", "--patch-from"])
        .arg(base)
        .arg("-o")
        .arg(out)
        .arg(input)
        .status()
        .map_err(|err| format!("zstd: {err}"))?;
    if !status.success() {
        return Err(format!("zstd {level} --patch-from failed: {status}"));
    }
    fs::metadata(out)
        .map(|meta| meta.len())
        .map_err(|err| format!("stat {}: {err}", out.display()))
}

fn plan_options(zstd19: bool, patch: bool) -> TensorPlanOptions {
    TensorPlanOptions {
        lambda_ns: 0,
        zstd19,
        zstd19_cpu_budget_ns: u64::MAX,
        zstd_patch: patch,
        zstd_patch_cpu_budget_ns: u64::MAX,
        // The pair is measured for storage economics; a latency objective
        // would price encodes and decodes and is a different question.
        access_pattern: AccessPattern::Whole,
        ..TensorPlanOptions::default()
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let base = PathBuf::from(arg(&args, "--base").ok_or("missing --base")?);
    let next = PathBuf::from(arg(&args, "--next").ok_or("missing --next")?);
    let label = arg(&args, "--label").unwrap_or_else(|| "pair".to_owned());
    let out = PathBuf::from(arg(&args, "--out").ok_or("missing --out")?);
    let zstd = arg(&args, "--zstd").unwrap_or_else(|| "zstd".to_owned());
    let single = args.iter().any(|item| item == "--single");
    let work = PathBuf::from(arg(&args, "--work").unwrap_or_else(|| {
        out.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{label}-work"))
            .display()
            .to_string()
    }));
    fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    let store_dir = work.join("store");
    if store_dir.exists() {
        fs::remove_dir_all(&store_dir).map_err(|err| format!("{err}"))?;
    }

    let base_file = SafetensorsFile::parse(&base).map_err(|e| e.to_string())?;
    let next_file = SafetensorsFile::parse(&next).map_err(|e| e.to_string())?;
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for (index, tensor) in base_file.tensors.iter().enumerate() {
        by_name.insert(tensor.name.clone(), index);
    }

    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 64 * 1024,
        checkpoint_interval: 1_000_000,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    };
    let mut store = Store::init(&store_dir, options).map_err(|err| err.to_string())?;
    let root = State::ROOT_ID;

    let mut independent = 0u64;
    let mut current = 0u64;
    let mut with19 = 0u64;
    let mut with_patch = 0u64;
    let mut rows = Vec::new();
    let mut strategy_counts: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut base_versions: Vec<(String, Hash32)> = Vec::new();
    let started = Instant::now();

    // Publish each matching base tensor with the fast raw policy so the patch
    // candidate has a causal base. These writes are outside the measurement.
    // In `--single` mode there is no base, so no deltas are offered.
    for tensor in &next_file.tensors {
        if single {
            break;
        }
        let Some(&index) = by_name.get(&tensor.name) else {
            continue;
        };
        let base_tensor = &base_file.tensors[index];
        if base_tensor.dtype != tensor.dtype || base_tensor.shape != tensor.shape {
            continue;
        }
        let bytes = base_file.read_tensor(base_tensor).map_err(|e| e.to_string())?;
        let shape: Vec<u64> = tensor.shape.clone();
        let specification = TensorWriteSpec {
            plan: TensorPlanOptions {
                write_policy: WritePolicy::Fast,
                fast_codec: FastCodec::Raw,
                zstd19: false,
                zstd_patch: false,
                ..TensorPlanOptions::default()
            },
            ..TensorWriteSpec::default()
        };
        let name = format!("base-{}", tensor.name.replace('.', "-"));
        let id = store
            .write_tensor_file_with_spec(
                root,
                name.as_bytes(),
                &tensor.dtype,
                &shape,
                &bytes,
                &weftfs::tensor_store::TensorLayoutSpec::row_major(),
                &specification,
            )
            .map_err(|err| format!("base write {}: {err}", tensor.name))?;
        let version = store
            .object_meta(id)
            .map_err(|err| err.to_string())?
            .version;
        base_versions.push((tensor.name.clone(), version));
    }

    let mut version_of: BTreeMap<String, Hash32> = BTreeMap::new();
    for (name, version) in base_versions {
        version_of.insert(name, version);
    }

    for tensor in &next_file.tensors {
        let bytes = next_file.read_tensor(tensor).map_err(|e| e.to_string())?;
        let shape: Vec<u64> = tensor.shape.clone();
        let layout = weftfs::tensor_store::TensorLayoutSpec::row_major();

        let mut measure = |options: &TensorPlanOptions, causal: &[Hash32]| {
            store.plan_tensor(&tensor.dtype, &shape, &bytes, &layout, causal, options)
        };

        let causal: Vec<Hash32> = if single {
            Vec::new()
        } else {
            version_of.get(&tensor.name).copied().into_iter().collect()
        };
        // No causal base: the standalone plan.
        let standalone =
            measure(&plan_options(false, false), &[]).map_err(|e| e.to_string())?;
        // The shipped planner with a causal base (pre-Stage-3 behaviour).
        let a = measure(&plan_options(false, false), &causal).map_err(|e| e.to_string())?;
        let b = measure(&plan_options(true, false), &causal).map_err(|e| e.to_string())?;
        let c = measure(&plan_options(true, true), &causal).map_err(|e| e.to_string())?;
        independent += standalone.chosen_physical_bytes;
        current += a.chosen_physical_bytes;
        with19 += b.chosen_physical_bytes;
        with_patch += c.chosen_physical_bytes;
        for (tag, plan) in [
            ("standalone", &standalone),
            ("current", &a),
            ("zstd19", &b),
            ("zstd19+patch", &c),
        ] {
            *strategy_counts
                .entry(tag.to_owned())
                .or_default()
                .entry(plan.chosen.as_str().to_owned())
                .or_default() += 1;
        }
        rows.push(json!({
            "tensor": tensor.name,
            "dtype": tensor.dtype,
            "bytes": bytes.len(),
            "standalone": {
                "chosen": standalone.chosen.as_str(),
                "physical_bytes": standalone.chosen_physical_bytes,
            },
            "current": {
                "chosen": a.chosen.as_str(),
                "physical_bytes": a.chosen_physical_bytes,
            },
            "zstd19": {
                "chosen": b.chosen.as_str(),
                "physical_bytes": b.chosen_physical_bytes,
            },
            "zstd19+patch": {
                "chosen": c.chosen.as_str(),
                "physical_bytes": c.chosen_physical_bytes,
            },
        }));
    }

    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    // Whole-file independent baselines.
    let zstd19_path = work.join("base.zst19");
    let patch_path = work.join("next.patch.zst");
    let whole_zstd19 = zstd_file(&zstd, 19, &base, &zstd19_path)?;
    let whole_patch = if single || base == next {
        // `zstd --patch-from` refuses to use one file as both input and
        // dictionary; there is no meaningful patch baseline here.
        0
    } else {
        zstd_patch_file(&zstd, 12, &base, &next, &patch_path)?
    };
    // `zstd -19` of the target container is the other independent reference.
    let target_zstd19_path = work.join("next.zst19");
    let target_zstd19 = zstd_file(&zstd, 19, &next, &target_zstd19_path)?;

    let receipt = json!({
        "schema": "weft.receipt.compression-candidates.v1",
        "label": label,
        "environment": {
            "court_image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_default(),
            "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_default(),
            "court_cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_default(),
            "court_memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_default(),
            "weftfs_revision": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_default(),
            "weftcodec_revision": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_default(),
            "planned_ms": elapsed_ms,
        },
        "corpus": {
            "base": base.display().to_string(),
            "next": next.display().to_string(),
            "base_bytes": fs::metadata(&base).map(|m| m.len()).unwrap_or(0),
            "next_bytes": fs::metadata(&next).map(|m| m.len()).unwrap_or(0),
        },
        "totals": {
            "tensors": rows.len(),
            "standalone": independent,
            "current": current,
            "zstd19": with19,
            "zstd19+patch": with_patch,
            "delta_causal": current as i128 - independent as i128,
            "zstd19_delta": with19 as i128 - current as i128,
            "patch_delta": with_patch as i128 - with19 as i128,
        },
        "baselines": {
            "zstd19_base_whole": whole_zstd19,
            "zstd19_next_whole": target_zstd19,
            "patch_from_12_whole": whole_patch,
            "zstd_level": 19,
            "patch_level": 12,
            "note": "whole-file zstd CLI baselines; the patch level is 12 because level 19 with a ~988 MB base was OOM-killed at the 3 GiB court in R1/P2",
        },
        "strategy_counts": strategy_counts,
        "rows": rows,
        "non_claims": [
            "single host, warm page cache",
            "plan-only totals: no checkpoint was written, so store publication and index bytes are excluded",
            "the patch candidate's base is a fast-raw store object, not the original checkpoint container",
            "zstd CLI baselines are independent processes; the in-process candidates use the same zstd library at a different call site",
        ],
    });
    fs::write(
        &out,
        serde_json::to_string_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!(
        "{}: standalone={} current={} zstd19={} zstd19+patch={} whole_zstd19={} patch_from={}",
        label, independent, current, with19, with_patch, whole_zstd19, whole_patch
    );
    Ok(())
}
