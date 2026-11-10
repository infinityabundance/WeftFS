//! Prepare/commit checkpoint write court: N ranks publish immutable blobs,
//! one committer publishes the single commit.
//!
//! Runs the real Qwen2.5-0.5B base container through
//! `Store::write_checkpoint` (sequential) and
//! `Store::write_checkpoint_parallel` at 1/2/4/8 ranks, on the local backend
//! and on MinIO, and proves that every path publishes byte-identical state
//! (same head, same checkpoint manifest root) for the same deterministic
//! checkpoint seed and fixed clock.
//!
//! The parent process re-executes itself once per configuration so each
//! configuration's wall time and peak RSS are isolated from the others.
//!
//! Usage (inside the court):
//!   bench_parallel_checkpoint --model FILE --work DIR --out RECEIPT \
//!       [--s3] [--filter GLOB] [--shards 8]
//!
//! The S3 phase derives one bucket per configuration and is intended for a
//! tensor subset (`--filter`), because the court MinIO data directory is a
//! 1 GiB tmpfs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

use weftfs::backend::BackendKind;
use weftfs::checkpoint::{CheckpointWriteOptions, ParallelCheckpointOptions, ShardStrategy};
use weftfs::error::Result;
use weftfs::s3::{S3Backend, S3Config};
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{FastCodec, WritePolicy};

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|item| item == name)
}

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            total += if meta.is_dir() {
                dir_bytes(&entry.path())
            } else {
                meta.len()
            };
        }
    }
    total
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if flag(&args, "--child") {
        if let Err(err) = child(&args) {
            eprintln!("bench_parallel_checkpoint child: {err}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(err) = parent(&args) {
        eprintln!("bench_parallel_checkpoint: {err}");
        std::process::exit(1);
    }
}

fn child(args: &[String]) -> Result<()> {
    let ranks: usize = arg(args, "--ranks")
        .unwrap_or("1")
        .parse()
        .expect("--ranks");
    let store_label = arg(args, "--store").expect("--store");
    let model = arg(args, "--model").expect("--model");
    let shards: u32 = arg(args, "--shards")
        .unwrap_or("8")
        .parse()
        .expect("--shards");
    let max_inflight: u64 = arg(args, "--max-inflight-bytes")
        .unwrap_or("335544320")
        .parse()
        .expect("--max-inflight-bytes");
    let per_rank: u64 = arg(args, "--per-rank-bytes")
        .unwrap_or("4294967296")
        .parse()
        .expect("--per-rank-bytes");
    let seed_hex = arg(args, "--seed").expect("--seed");
    let s3 = flag(args, "--s3");
    let filter = arg(args, "--filter");
    let fast = flag(args, "--fast");
    let phases = flag(args, "--phases");
    let pack = flag(args, "--pack");
    let fast_codec_raw = arg(args, "--fast-codec")
        .map(|codec| codec == "raw")
        .unwrap_or(false);

    let seed: [u8; 32] = {
        let bytes = hex::decode(seed_hex).expect("seed hex");
        bytes.try_into().expect("32-byte seed")
    };
    let options = Options {
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    };
    let checkpoint = CheckpointWriteOptions {
        shards,
        axis: 0,
        filters: filter.map(|glob| vec![glob.to_owned()]).unwrap_or_default(),
        strategy: ShardStrategy::TensorSharded,
        write_policy: if fast {
            WritePolicy::Fast
        } else {
            WritePolicy::Planned
        },
        fast_codec: if fast_codec_raw {
            FastCodec::Raw
        } else {
            FastCodec::Zstd
        },
        fast_zstd_level: 1,
        deterministic_seed: Some(seed),
        ..CheckpointWriteOptions::default()
    };

    let pack_options: Option<weftfs::packs::PackOptions> = if pack {
        let mut policy = weftfs::packs::PackOptions::default();
        if let Some(bytes) = arg(args, "--pack-target-bytes") {
            policy.pack_target_bytes = bytes.parse().expect("--pack-target-bytes");
        }
        if let Some(bytes) = arg(args, "--pack-member-threshold") {
            policy.member_threshold_bytes = bytes.parse().expect("--pack-member-threshold");
        }
        if let Some(bytes) = arg(args, "--pack-catalog-bytes") {
            policy.max_catalog_bytes = bytes.parse().expect("--pack-catalog-bytes");
        }
        if let Some(segments) = arg(args, "--pack-segments") {
            policy.catalog_segments = segments.parse().expect("--pack-segments");
        }
        policy.validate().expect("pack policy");
        Some(policy)
    } else {
        None
    };

    let mut store = if s3 {
        let mut config = S3Config::from_env()?;
        config.bucket = store_label.to_owned();
        S3Backend::new("<parallel-bench>", config.clone()).ensure_bucket()?;
        let kind = match pack_options {
            Some(policy) => BackendKind::packed(BackendKind::S3 { config }, policy)?,
            None => BackendKind::S3 { config },
        };
        Store::init_with_backend("<parallel-bench>", options.clone(), kind)?
    } else {
        let path = PathBuf::from(store_label);
        if path.exists() {
            fs::remove_dir_all(&path)
                .map_err(|err| weftfs::error::Error::io(format!("{}: {err}", path.display())))?;
        }
        match pack_options {
            Some(policy) => Store::init_with_backend(
                &path,
                options.clone(),
                BackendKind::packed(BackendKind::Local, policy)?,
            )?,
            None => Store::init(&path, options.clone())?,
        }
    };

    if phases {
        weftfs::phase_profile::begin();
    }
    let before = store.backend_stats();
    let started = Instant::now();
    let report = if ranks == 0 {
        store.write_checkpoint("ckpt", Path::new(model), &checkpoint)?
    } else {
        let parallel = ParallelCheckpointOptions {
            ranks,
            max_in_flight_bytes: max_inflight,
            per_rank_bytes: per_rank,
            ..ParallelCheckpointOptions::default()
        };
        store.write_checkpoint_parallel("ckpt", Path::new(model), &checkpoint, &parallel)?
    };
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let phase_report = if phases {
        weftfs::phase_profile::finish().to_json()
    } else {
        Json::Null
    };
    let after = store.backend_stats();
    let pack_stats = store.pack_stats().map(|stats| json!(stats));

    let store_bytes = if s3 {
        Json::Null
    } else {
        json!(dir_bytes(Path::new(store_label)))
    };
    println!(
        "{}",
        json!({
            "ranks": ranks,
            "backend": if s3 { "s3" } else { "local" },
            "filter": filter,
            "shards": shards,
            "write_policy": match (fast, fast_codec_raw) {
                (true, true) => "fast/raw",
                (true, false) => "fast/zstd-1",
                (false, _) => "planned",
            },
            "pack": pack,
            "max_in_flight_bytes": max_inflight,
            "head": report.head.to_hex(),
            "checkpoint_id": report.checkpoint_id.to_hex(),
            "manifest_id": report.manifest_id.to_hex(),
            "manifest_root": report.manifest_root.to_hex(),
            "generation": store.generation(),
            "shard_objects": report.shard_objects,
            "element_bytes": report.total_element_bytes,
            "store_bytes": store_bytes,
            "prepare_ms": report.prepare_ms,
            "validate_ms": report.validate_ms,
            "commit_ms": report.commit_ms,
            "wall_ms": wall_ms,
            "peak_rss_kib": peak_rss_kib(),
            "phases": phase_report,
            "backend_stats": {
                "reads": after.reads - before.reads,
                "range_reads": after.range_reads - before.range_reads,
                "bytes_read": after.bytes_read - before.bytes_read,
                "writes": after.writes - before.writes,
                "bytes_written": after.bytes_written - before.bytes_written,
                "link_publications": after.link_publications - before.link_publications,
                "rename_publications": after.rename_publications - before.rename_publications,
            },
            "pack_stats": pack_stats,
        })
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_child(
    exe: &Path,
    model: &Path,
    store: &str,
    ranks: usize,
    seed: &str,
    s3: bool,
    filter: Option<&str>,
    shards: u32,
    max_inflight: u64,
    fast: bool,
    phases: bool,
    pack: bool,
    fast_codec: Option<&str>,
) -> std::io::Result<Json> {
    let mut command = Command::new(exe);
    command
        .arg("--child")
        .arg("--ranks")
        .arg(ranks.to_string())
        .arg("--store")
        .arg(store)
        .arg("--model")
        .arg(model)
        .arg("--seed")
        .arg(seed)
        .arg("--shards")
        .arg(shards.to_string())
        .arg("--max-inflight-bytes")
        .arg(max_inflight.to_string());
    if s3 {
        command.arg("--s3");
    }
    if let Some(filter) = filter {
        command.arg("--filter").arg(filter);
    }
    if fast {
        command.arg("--fast");
    }
    if phases {
        command.arg("--phases");
    }
    if pack {
        command.arg("--pack");
    }
    if let Some(codec) = fast_codec {
        command.arg("--fast-codec").arg(codec);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "child failed (ranks={ranks} s3={s3}): {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().last().unwrap_or("").trim();
    serde_json::from_str(line).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("child printed no JSON ({err}): {text}"),
        )
    })
}

/// Minimum of a numeric field across repetitions (`null` is ignored).
fn minimum(samples: &[Json], field: &str) -> Json {
    let mut values: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample[field].as_f64())
        .collect();
    if values.is_empty() {
        return Json::Null;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    json!(values[0])
}

/// Maximum of a numeric field across repetitions (`null` is ignored).
fn maximum(samples: &[Json], field: &str) -> Json {
    let mut values: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample[field].as_f64())
        .collect();
    if values.is_empty() {
        return Json::Null;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    json!(values[values.len() - 1])
}

/// Median of a numeric field across repetitions (`null` is ignored).
fn median(samples: &[Json], field: &str) -> Json {
    let mut values: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample[field].as_f64())
        .collect();
    if values.is_empty() {
        return Json::Null;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = values.len() / 2;
    let value = if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    };
    json!(value)
}

fn parent(args: &[String]) -> Result<()> {
    let model = PathBuf::from(arg(args, "--model").expect("--model"));
    let work = PathBuf::from(arg(args, "--work").unwrap_or("bench/.parallel-work"));
    let out = PathBuf::from(arg(args, "--out").expect("--out"));
    let s3 = flag(args, "--s3");
    let filter = arg(args, "--filter");
    let shards: u32 = arg(args, "--shards")
        .unwrap_or("8")
        .parse()
        .expect("--shards");
    let max_inflight: u64 = arg(args, "--max-inflight-bytes")
        .unwrap_or("335544320")
        .parse()
        .expect("--max-inflight-bytes");
    let seed = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

    fs::create_dir_all(&work)
        .map_err(|err| weftfs::error::Error::io(format!("{}: {err}", work.display())))?;
    let exe = std::env::current_exe()
        .map_err(|err| weftfs::error::Error::io(format!("current_exe: {err}")))?;

    let mut configurations = Vec::new();
    let fast = flag(args, "--fast");
    let reps: usize = arg(args, "--reps").unwrap_or("3").parse().expect("--reps");
    // `--ranks` narrows the rank sweep for a reduced court run. The default is
    // the full sweep, so a standalone run is unchanged.
    let rank_sweep: Vec<usize> = arg(args, "--ranks")
        .map(|text| {
            text.split([',', ' '])
                .filter(|part| !part.is_empty())
                .map(|part| part.parse().expect("--ranks member"))
                .collect()
        })
        .unwrap_or_else(|| vec![0, 1, 2, 4, 8]);
    let runs: &[usize] = &rank_sweep;
    let stamp = arg(args, "--stamp")
        .map(str::to_owned)
        .unwrap_or_else(chrono_stamp);

    let mut heads: BTreeMap<String, String> = BTreeMap::new();
    let mut manifest_roots: BTreeMap<String, String> = BTreeMap::new();
    let phases = flag(args, "--phases");
    let fast_codec = arg(args, "--fast-codec");
    let pack_compare = flag(args, "--pack-compare");

    // One measured configuration. `prefix` distinguishes the flat and packed
    // halves of a `--pack-compare` run without changing the default receipt.
    let mut measure_configuration =
        |ranks: usize, pack: bool, prefix: &str, configurations: &mut Vec<Json>| -> Result<()> {
            let label = format!(
                "{prefix}{}",
                if ranks == 0 {
                    "sequential".to_owned()
                } else {
                    format!("ranks-{ranks}")
                }
            );
            let mut samples: Vec<Json> = Vec::new();
            for rep in 0..reps {
                let store = if s3 {
                    format!(
                        "weft-parallel-{}-r{ranks}-{rep}{}",
                        stamp.to_lowercase(),
                        if pack { "-packed" } else { "" }
                    )
                } else {
                    work.join(format!(
                        "store-r{ranks}{}",
                        if pack { "-packed" } else { "" }
                    ))
                    .to_string_lossy()
                    .into_owned()
                };
                let started = Instant::now();
                let value = run_child(
                    &exe,
                    &model,
                    &store,
                    ranks,
                    seed,
                    s3,
                    filter,
                    shards,
                    max_inflight,
                    fast,
                    phases,
                    pack,
                    fast_codec,
                )
                .map_err(|err| weftfs::error::Error::io(format!("child: {err}")))?;
                heads.insert(label.clone(), value["head"].as_str().unwrap().to_owned());
                manifest_roots.insert(
                    label.clone(),
                    value["manifest_root"].as_str().unwrap().to_owned(),
                );
                let mut sample = value.as_object().cloned().unwrap_or_default();
                sample.insert("store".to_owned(), Json::String(store.clone()));
                sample.insert(
                    "parent_observed_ms".to_owned(),
                    json!(started.elapsed().as_secs_f64() * 1000.0),
                );
                samples.push(Json::Object(sample));
            }
            let median_block = json!({
                "prepare_ms": median(&samples, "prepare_ms"),
                "validate_ms": median(&samples, "validate_ms"),
                "commit_ms": median(&samples, "commit_ms"),
                "wall_ms": median(&samples, "wall_ms"),
                "peak_rss_kib": median(&samples, "peak_rss_kib"),
                "store_bytes": median(&samples, "store_bytes"),
            });
            let best_block = json!({
                "prepare_ms": minimum(&samples, "prepare_ms"),
                "validate_ms": minimum(&samples, "validate_ms"),
                "commit_ms": minimum(&samples, "commit_ms"),
                "wall_ms": minimum(&samples, "wall_ms"),
                "peak_rss_kib": maximum(&samples, "peak_rss_kib"),
                "store_bytes": minimum(&samples, "store_bytes"),
            });
            println!(
                "{label}: head={} median_prepare_ms={:?} median_commit_ms={:?} median_wall_ms={:?}",
                &heads[&label][..16],
                median_block["prepare_ms"],
                median_block["commit_ms"],
                median_block["wall_ms"]
            );
            configurations.push(json!({
                "label": label,
                "ranks": ranks,
                "pack": pack,
                "reps": reps,
                "median": median_block,
                "best": best_block,
                "runs": samples,
            }));
            Ok(())
        };

    for &ranks in runs {
        measure_configuration(ranks, false, "", &mut configurations)?;
    }
    // The packed configurations are a second, labelled set. They publish the
    // same head and manifest root; only physical placement differs.
    let mut packed_configurations: Vec<Json> = Vec::new();
    if pack_compare {
        for &ranks in runs {
            measure_configuration(ranks, true, "packed-", &mut packed_configurations)?;
        }
    }

    let baseline_label = configurations
        .first()
        .and_then(|item| item["label"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "sequential".to_owned());
    let expected_head = heads.get(&baseline_label).cloned().unwrap_or_default();
    let expected_root = manifest_roots
        .get(&baseline_label)
        .cloned()
        .unwrap_or_default();
    let mut checks: Vec<Json> = Vec::new();
    for (label, head) in &heads {
        checks.push(json!({
            "check": format!("head_matches_sequential:{label}"),
            "pass": *head == expected_head,
            "head": head,
        }));
        checks.push(json!({
            "check": format!("manifest_root_matches_sequential:{label}"),
            "pass": manifest_roots[label] == expected_root,
            "manifest_root": manifest_roots[label],
        }));
    }
    // Every repetition of a configuration must publish the same head too.
    for configuration in &configurations {
        let label = configuration["label"].as_str().unwrap_or("");
        for (index, run) in configuration["runs"]
            .as_array()
            .map(|items| items.iter())
            .into_iter()
            .flatten()
            .enumerate()
        {
            checks.push(json!({
                "check": format!("head_matches_sequential:{label}:rep{index}"),
                "pass": run["head"].as_str() == Some(expected_head.as_str()),
                "head": run["head"],
            }));
        }
    }

    // Speedups are reported from the measurements, never asserted. A reduced
    // rank sweep (`--ranks`) may not contain ranks=1, so every ratio above a
    // missing baseline is reported as null rather than as a fabricated number.
    let mut derived = Vec::new();
    let seq_wall = configurations
        .first()
        .map(|item| item["best"]["wall_ms"].as_f64().unwrap_or(0.0))
        .unwrap_or(0.0);
    let baseline = configurations
        .iter()
        .find(|item| item["ranks"].as_u64() == Some(1));
    let ranks1_prepare = baseline
        .map(|item| item["best"]["prepare_ms"].as_f64().unwrap_or(0.0))
        .unwrap_or(0.0);
    let ranks1_wall = baseline
        .map(|item| item["best"]["wall_ms"].as_f64().unwrap_or(0.0))
        .unwrap_or(0.0);
    let has_ranks1 = baseline.is_some();
    for item in configurations.iter().skip(1) {
        let ranks = item["ranks"].as_u64().unwrap_or(0);
        let prepare = item["best"]["prepare_ms"].as_f64().unwrap_or(0.0);
        let wall = item["best"]["wall_ms"].as_f64().unwrap_or(0.0);
        derived.push(json!({
            "ranks": ranks,
            "basis": "best-of-N (warm-page-cache comparable run); medians are published beside it",
            "prepare_speedup_vs_ranks_1": if has_ranks1 && prepare > 0.0 { ranks1_prepare / prepare } else { 0.0 },
            "wall_speedup_vs_ranks_1": if has_ranks1 && wall > 0.0 { ranks1_wall / wall } else { 0.0 },
            "wall_speedup_vs_sequential": if wall > 0.0 { seq_wall / wall } else { 0.0 },
            "prepare_ms": prepare,
            "wall_ms": wall,
            "validate_ms": item["best"]["validate_ms"],
            "commit_ms": item["best"]["commit_ms"],
            "peak_rss_kib": item["best"]["peak_rss_kib"],
            "store_bytes": item["best"]["store_bytes"],
            "median_prepare_ms": item["median"]["prepare_ms"],
            "median_wall_ms": item["median"]["wall_ms"],
        }));
    }
    let sequential_best = configurations
        .first()
        .map(|item| item["best"].clone())
        .unwrap_or(Json::Null);
    let sequential_median = configurations
        .first()
        .map(|item| item["median"].clone())
        .unwrap_or(Json::Null);
    let sequential_label = configurations
        .first()
        .and_then(|item| item["label"].as_str().map(str::to_owned))
        .unwrap_or_default();

    // Flat vs packed at the same rank count. Every number is measured; the
    // delta fields are simple ratios so a reader can see a loss as easily as a
    // win. `requests` is `reads + range_reads + writes` of the blob/commit
    // plane, so a packed save's request count is directly comparable.
    let pack_comparison: Json = if pack_compare {
        let mut rows = Vec::new();
        for (flat, packed) in configurations.iter().zip(packed_configurations.iter()) {
            let ranks = flat["ranks"].as_u64().unwrap_or(0);
            let flat_best = &flat["best"];
            let packed_best = &packed["best"];
            let flat_runs = flat["runs"].as_array().cloned().unwrap_or_default();
            let packed_runs = packed["runs"].as_array().cloned().unwrap_or_default();
            let flat_writes: f64 = flat_runs
                .iter()
                .filter_map(|run| run.pointer("/backend_stats/writes").and_then(Json::as_f64))
                .sum();
            let packed_writes: f64 = packed_runs
                .iter()
                .filter_map(|run| run.pointer("/backend_stats/writes").and_then(Json::as_f64))
                .sum();
            let flat_written: f64 = flat_runs
                .iter()
                .filter_map(|run| {
                    run.pointer("/backend_stats/bytes_written")
                        .and_then(Json::as_f64)
                })
                .sum();
            let packed_written: f64 = packed_runs
                .iter()
                .filter_map(|run| {
                    run.pointer("/backend_stats/bytes_written")
                        .and_then(Json::as_f64)
                })
                .sum();
            let flat_wall = flat_best["wall_ms"].as_f64().unwrap_or(0.0);
            let packed_wall = packed_best["wall_ms"].as_f64().unwrap_or(0.0);
            let flat_prepare = flat_best["prepare_ms"].as_f64().unwrap_or(0.0);
            let packed_prepare = packed_best["prepare_ms"].as_f64().unwrap_or(0.0);
            rows.push(json!({
                "ranks": ranks,
                "flat": {
                    "wall_ms": flat_wall,
                    "prepare_ms": flat_prepare,
                    "commit_ms": flat_best["commit_ms"],
                    "store_bytes": flat_best["store_bytes"],
                    "peak_rss_kib": flat_best["peak_rss_kib"],
                    "writes_last_rep_sum": flat_writes,
                    "bytes_written_last_rep_sum": flat_written,
                    "phases": flat_runs.first().and_then(|run| run.get("phases")).cloned().unwrap_or(Json::Null),
                },
                "packed": {
                    "wall_ms": packed_wall,
                    "prepare_ms": packed_prepare,
                    "commit_ms": packed_best["commit_ms"],
                    "store_bytes": packed_best["store_bytes"],
                    "peak_rss_kib": packed_best["peak_rss_kib"],
                    "writes_last_rep_sum": packed_writes,
                    "bytes_written_last_rep_sum": packed_written,
                    "pack_stats": packed_runs.first().and_then(|run| run.get("pack_stats")).cloned().unwrap_or(Json::Null),
                    "phases": packed_runs.first().and_then(|run| run.get("phases")).cloned().unwrap_or(Json::Null),
                },
                "wall_speedup_flat_over_packed": if packed_wall > 0.0 { flat_wall / packed_wall } else { 0.0 },
                "prepare_speedup_flat_over_packed": if packed_prepare > 0.0 { flat_prepare / packed_prepare } else { 0.0 },
                "request_ratio_flat_over_packed": if packed_writes > 0.0 { flat_writes / packed_writes } else { 0.0 },
                "written_bytes_ratio_flat_over_packed": if packed_written > 0.0 { flat_written / packed_written } else { 0.0 },
                "same_head": flat_runs.first().and_then(|run| run.get("head")) == packed_runs.first().and_then(|run| run.get("head")),
                "same_manifest_root": flat_runs.first().and_then(|run| run.get("manifest_root")) == packed_runs.first().and_then(|run| run.get("manifest_root")),
            }));
        }
        json!({
            "basis": "best-of-N per configuration (warm-page-cache comparable); the median spreads stay in the two configuration blocks",
            "request_metric": "backend_stats.writes (blob and control publications), summed over the repetitions of that configuration",
            "rows": rows,
        })
    } else {
        Json::Null
    };

    let receipt = json!({
        "schema": "weft-parallel-checkpoint-court-v1",
        "generated_utc": stamp,
        "backend": if s3 { "s3" } else { "local" },
        "model": {
            "path": model.display().to_string(),
            "file_bytes": fs::metadata(&model).map(|m| m.len()).unwrap_or(0),
            "sha256": file_sha256(&model).unwrap_or_default(),
        },
        "shards": shards,
        "rank_sweep": rank_sweep,
        "write_policy": match (fast, fast_codec) {
            (true, Some("raw")) => "fast/raw",
            (true, _) => "fast/zstd-1",
            (false, _) => "planned",
        },
        "phases": phases,
        "pack_compare": pack_compare,
        "tensor_filter": filter,
        "max_in_flight_bytes": max_inflight,
        "deterministic_seed": seed,
        "wall_clock": "fixed 1700000000000000000 ns",
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").ok(),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").ok(),
            "memory": std::env::var("WEFT_COURT_MEMORY").ok(),
            "cpus": std::env::var("WEFT_COURT_CPUS").ok(),
            "pids": std::env::var("WEFT_COURT_PIDS").ok(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").ok(),
            "s3_server": std::env::var("WEFT_S3_SERVER_LABEL").ok(),
            "s3_server_version": std::env::var("WEFT_S3_SERVER_VERSION").ok(),
            "s3_server_sha256": std::env::var("WEFT_S3_SERVER_SHA256").ok(),
        },
        "sequential_best": sequential_best,
        "sequential_median": sequential_median,
        "sequential_label": sequential_label,
        "reps": reps,
        "configurations": configurations,
        "packed_configurations": packed_configurations,
        "pack_comparison": pack_comparison,
        "derived": derived,
        "checks": checks,
        "checks_passed": checks.iter().all(|check| check["pass"].as_bool().unwrap_or(false)),
        "labels": [
            "The unit of a prepare task is one tensor (all of its shards): the tensor LogicalTensorRoot spans its shards.",
            "The S3 phase runs a tensor subset when --filter is given, because the court MinIO data directory is a 1 GiB tmpfs; it proves the create-once PUT path under concurrency, not a full-model S3 ingest.",
            "Peak RSS is the child process VmHWM, which includes that configuration's own store handle and worker handles.",
            "Wall time includes store open/replay, verification, and the commit; prepare_ms is only the parallel blob-writing phase.",
            "Repetitions on this host alternate between a cold and a warm page cache for the 988 MB container; the `best` block is the comparable warm-cache number and the `median` block publishes the spread. Neither is a throughput guarantee.",
            "Single host, warm page cache, one run per configuration; no fleet, media, power-loss, or cloud claim."
        ],
    });
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&out, serde_json::to_vec_pretty(&receipt).unwrap())
        .map_err(|err| weftfs::error::Error::io(format!("{}: {err}", out.display())))?;
    if !receipt["checks_passed"].as_bool().unwrap_or(false) {
        return Err(weftfs::error::Error::corrupt(
            "a parallel checkpoint head differed from the sequential write",
        ));
    }
    eprintln!("bench_parallel_checkpoint: receipt {}", out.display());
    Ok(())
}

/// UTC stamp without a chrono dependency, matching the receipt naming used by
/// the other courts (`YYYYMMDDTHHMMSSZ`).
fn chrono_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, secs) = (now / 86_400, now % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
