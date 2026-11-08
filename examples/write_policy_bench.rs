//! Stage 1 court harness: critical-path write policy and bounded background
//! transcode, on real safetensors containers.
//!
//! Each phase is one process so pause/wall/RSS/CPU are honest for that phase.
//! The harness prints one JSON object per invocation; `assemble` merges the
//! phase files into the receipt.
//!
//! This is deliberately not a model framework: the container is handed to the
//! shipped `Store::write_checkpoint` / `Store::transcode` /
//! `Store::restore_checkpoint` APIs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};

use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{FastCodec, WritePolicy};
use weftfs::transcode::TranscodeOptions;
use weftfs::load_receipt::{LoadReceiptOptions, LoadVerification};
use weftfs::recovery_first::RecoveryFirstOptions;

fn main() {
    if let Err(err) = run() {
        eprintln!("write_policy_bench: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(phase) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    match phase {
        "info" => info(&args[1..]),
        "save" => save(&args[1..]),
        "transcode" => transcode(&args[1..]),
        "restore" => restore(&args[1..]),
        "verify" => verify(&args[1..]),
        "loadreceipt" => loadreceipt(&args[1..]),
        "recovery" => recovery(&args[1..]),
        "assemble" => assemble(&args[1..]),
        other => Err(format!("unknown phase `{other}`\n{}", usage())),
    }
}

fn usage() -> String {
    "usage: write_policy_bench <info|save|transcode|restore|verify|loadreceipt|recovery|assemble> ...".to_owned()
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

fn peak_rss_kib() -> u64 {
    let Ok(text) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(value) = rest.split_whitespace().next() {
                return value.parse().unwrap_or(0);
            }
        }
    }
    0
}

/// `(user_ms, sys_ms)` from /proc/self/stat (ticks are 100 Hz in this court).
fn cpu_ms() -> (f64, f64) {
    let Ok(text) = fs::read_to_string("/proc/self/stat") else {
        return (0.0, 0.0);
    };
    // Fields 14 and 15 (1-based) are utime and stime; the command name in
    // parentheses may contain spaces, so split after the last ')'.
    let Some(rest) = text.rsplit(')').next() else {
        return (0.0, 0.0);
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0);
    let stime: u64 = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0);
    (utime as f64 * 10.0, stime as f64 * 10.0)
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn open_store(path: &str) -> Result<Store, String> {
    Store::open_with_options(
        Path::new(path),
        OpenMode::ReadWrite,
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )
    .map_err(|err| err.to_string())
}

fn parse_policy(text: &str) -> Result<WritePolicy, String> {
    WritePolicy::parse(text).map_err(|err| err.to_string())
}

fn strategy_histogram(store: &Store) -> Result<BTreeMap<String, u64>, String> {
    let mut hist: BTreeMap<String, u64> = BTreeMap::new();
    let mut ids: Vec<_> = store.current_state().objects.keys().copied().collect();
    ids.sort();
    for id in ids {
        let Some(record) = store.current_state().objects.get(&id) else {
            continue;
        };
        if record.kind != weftfs::state::ObjectKind::File {
            continue;
        }
        let Ok(meta) = store.tensor_metadata(id) else {
            *hist.entry("non-tensor".to_owned()).or_insert(0) += 1;
            continue;
        };
        *hist.entry(meta.strategy).or_insert(0) += 1;
    }
    Ok(hist)
}

fn info(args: &[String]) -> Result<(), String> {
    let store_path = arg(args, "--store").ok_or("info needs --store")?;
    let store =
        Store::open(Path::new(store_path), OpenMode::ReadOnly).map_err(|err| err.to_string())?;
    let hist = strategy_histogram(&store)?;
    let objects = dir_bytes(&Path::new(store_path).join("objects"));
    let manifests = dir_bytes(&Path::new(store_path).join("manifests"));
    let store_bytes = dir_bytes(Path::new(store_path));
    println!(
        "{}",
        json!({
            "phase": "info",
            "objects_bytes": objects,
            "manifests_bytes": manifests,
            "store_bytes": store_bytes,
            "strategies": hist,
            "generation": store.generation(),
            "head": store.head().to_hex(),
            "logical_retained": store.retained_blob_bytes().map_err(|e| e.to_string())?,
        })
    );
    Ok(())
}

fn save(args: &[String]) -> Result<(), String> {
    let source = arg(args, "--source").ok_or("save needs --source")?;
    let store_path = arg(args, "--store").ok_or("save needs --store")?;
    let name = arg(args, "--name").unwrap_or("model");
    let policy = parse_policy(arg(args, "--policy").unwrap_or("planned"))?;
    let codec =
        FastCodec::parse(arg(args, "--codec").unwrap_or("raw")).map_err(|err| err.to_string())?;
    let level: i32 = arg(args, "--level")
        .unwrap_or("1")
        .parse()
        .map_err(|_| "bad --level")?;
    let shards: u32 = arg(args, "--shards")
        .unwrap_or("1")
        .parse()
        .map_err(|_| "bad --shards")?;
    let strategy = ShardStrategy::parse(arg(args, "--shard-strategy").unwrap_or("tensor-sharded"))
        .map_err(|err| err.to_string())?;
    let on_critical_path = flag(args, "--critical-path");
    let source_bytes = fs::metadata(source).map_err(|e| e.to_string())?.len();

    let started = Instant::now();
    let (cpu0u, cpu0s) = cpu_ms();
    let store_dir = PathBuf::from(store_path);
    if store_dir.exists() {
        fs::remove_dir_all(&store_dir).map_err(|e| e.to_string())?;
    }
    let mut store = Store::init(
        &store_dir,
        Options {
            gc_grace_secs: 0,
            fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
            ..Options::default()
        },
    )
    .map_err(|err| err.to_string())?;
    let open_ms = started.elapsed().as_secs_f64() * 1000.0;

    let pause_started = Instant::now();
    let report = store
        .write_checkpoint(
            name,
            Path::new(source),
            &CheckpointWriteOptions {
                shards,
                axis: 0,
                filters: Vec::new(),
                strategy,
                tile_shape: None,
                write_policy: policy,
                fast_codec: codec,
                fast_zstd_level: level,
                on_critical_path,
                deadline_ms: None,
                causal_checkpoint: arg(args, "--causal-checkpoint").map(str::to_owned),
                semantic_search: !flag(args, "--no-semantic"),
            },
        )
        .map_err(|err| err.to_string())?;
    // This is the caller's pause: the save call has returned durably.
    let pause_ms = pause_started.elapsed().as_secs_f64() * 1000.0;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (cpu1u, cpu1s) = cpu_ms();
    let hist = strategy_histogram(&store)?;
    let objects = dir_bytes(&store_dir.join("objects"));
    let manifests = dir_bytes(&store_dir.join("manifests"));
    let store_bytes = dir_bytes(&store_dir);
    println!(
        "{}",
        json!({
            "phase": "save",
            "policy": policy.as_str(),
            "policy_resolved": match policy.resolved() {
                weftfs::tensor_store::ResolvedWritePolicy::Fast => "fast",
                weftfs::tensor_store::ResolvedWritePolicy::Planned => "planned",
            },
            "codec": codec.as_str(),
            "level": level,
            "shards": shards,
            "shard_strategy": strategy.as_str(),
            "source": source,
            "source_bytes": source_bytes,
            "open_ms": open_ms,
            "pause_ms": pause_ms,
            "wall_ms": wall_ms,
            "cpu_user_ms": cpu1u - cpu0u,
            "cpu_sys_ms": cpu1s - cpu0s,
            "peak_rss_kib": peak_rss_kib(),
            "objects_bytes": objects,
            "manifests_bytes": manifests,
            "store_bytes": store_bytes,
            "strategies": hist,
            "tensors": report.tensors.len(),
            "shard_objects": report.shard_objects,
            "element_bytes": report.total_element_bytes,
            "source_sha256": hex::encode(report.source_sha256),
            "checkpoint_id": report.checkpoint_id.to_hex(),
            "head": report.head.to_hex(),
        })
    );
    Ok(())
}

fn transcode(args: &[String]) -> Result<(), String> {
    let store_path = arg(args, "--store").ok_or("transcode needs --store")?;
    let root_path = arg(args, "--root").unwrap_or("/");
    let max_objects: usize = arg(args, "--max-objects")
        .unwrap_or("100000")
        .parse()
        .map_err(|_| "bad --max-objects")?;
    let max_bytes: u64 = arg(args, "--max-bytes")
        .unwrap_or("18446744073709551615")
        .parse()
        .map_err(|_| "bad --max-bytes")?;
    let max_millis: u64 = arg(args, "--max-millis")
        .unwrap_or("0")
        .parse()
        .map_err(|_| "bad --max-millis")?;
    let min_saved: u8 = arg(args, "--min-saved-percent")
        .unwrap_or("1")
        .parse()
        .map_err(|_| "bad --min-saved-percent")?;
    let crash_after: Option<usize> = arg(args, "--crash-after").map(|v| v.parse().unwrap());
    let crash_before: Option<usize> = arg(args, "--crash-before").map(|v| v.parse().unwrap());
    let store_dir = PathBuf::from(store_path);
    let before = dir_bytes(&store_dir.join("objects")) + dir_bytes(&store_dir.join("manifests"));
    let (cpu0u, cpu0s) = cpu_ms();
    let started = Instant::now();
    let mut store = open_store(store_path)?;
    let root = store.resolve_path(root_path).map_err(|e| e.to_string())?;
    let report = store
        .transcode(
            root,
            &TranscodeOptions {
                max_objects,
                max_bytes,
                max_millis,
                min_saved_percent: min_saved,
                dry_run: flag(args, "--dry-run"),
                verify_after: !flag(args, "--no-verify-after"),
                crash_after_publishes: crash_after,
                crash_before_publish: crash_before,
                ..TranscodeOptions::default()
            },
        )
        .map_err(|e| e.to_string())?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (cpu1u, cpu1s) = cpu_ms();
    let after = dir_bytes(&store_dir.join("objects")) + dir_bytes(&store_dir.join("manifests"));
    let hist = strategy_histogram(&store)?;
    let old_total: u64 = report.candidates.iter().map(|c| c.old_physical_bytes).sum();
    let new_total: u64 = report.applied.iter().map(|c| c.new_physical_bytes).sum();
    println!(
        "{}",
        json!({
            "phase": "transcode",
            "root": root_path,
            "scanned": report.scanned,
            "candidates": report.candidates.len(),
            "applied": report.applied.len(),
            "dry_run": report.dry_run,
            "stopped_reason": report.stopped_reason,
            "bytes_read": report.bytes_read,
            "wall_ms": wall_ms,
            "cpu_user_ms": cpu1u - cpu0u,
            "cpu_sys_ms": cpu1s - cpu0s,
            "peak_rss_kib": peak_rss_kib(),
            "old_physical_bytes": old_total,
            "new_physical_bytes": new_total,
            "saved_bytes": old_total.saturating_sub(new_total),
            "store_bytes_before": before,
            "store_bytes_after": after,
            "strategies": hist,
            "items": report.candidates.iter().map(|c| json!({
                "path": c.path,
                "from": c.from_strategy,
                "to": c.to_strategy.as_str(),
                "old_physical_bytes": c.old_physical_bytes,
                "new_physical_bytes": c.new_physical_bytes,
            })).collect::<Vec<_>>(),
            "outcomes": report.applied.iter().map(|c| json!({
                "path": c.path,
                "verified": c.verified,
                "provenance": c.provenance_id.to_hex(),
                "content": c.content.to_hex(),
            })).collect::<Vec<_>>(),
        })
    );
    Ok(())
}

fn parse_region(text: &str) -> Result<weftcodec::Region, String> {
    let mut start = Vec::new();
    let mut end = Vec::new();
    for pair in text.split(',') {
        let (a, b) = pair.split_once(':').ok_or("region needs s:e pairs")?;
        start.push(a.parse::<u64>().map_err(|_| "bad region start")?);
        end.push(b.parse::<u64>().map_err(|_| "bad region end")?);
    }
    Ok(weftcodec::Region::new(start, end))
}

fn restore(args: &[String]) -> Result<(), String> {
    let store_path = arg(args, "--store").ok_or("restore needs --store")?;
    let name = arg(args, "--name").unwrap_or("model");
    let store =
        Store::open(Path::new(store_path), OpenMode::ReadOnly).map_err(|e| e.to_string())?;
    let options = CheckpointRestoreOptions {
        tensor: arg(args, "--tensor").map(str::to_owned),
        region: match arg(args, "--range") {
            Some(text) => Some(parse_region(text)?),
            None => None,
        },
        topology: None,
        ..CheckpointRestoreOptions::default()
    };
    let mut bytes = Vec::with_capacity(1 << 20);
    let started = Instant::now();
    let (cpu0u, cpu0s) = cpu_ms();
    let report = store
        .restore_checkpoint(name, &options, &mut bytes)
        .map_err(|e| e.to_string())?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (cpu1u, cpu1s) = cpu_ms();
    println!(
        "{}",
        json!({
            "phase": "restore",
            "tensor": options.tensor,
            "range": arg(args, "--range"),
            "wall_ms": wall_ms,
            "cpu_user_ms": cpu1u - cpu0u,
            "cpu_sys_ms": cpu1s - cpu0s,
            "peak_rss_kib": peak_rss_kib(),
            "exact_source_file": report.exact_source_file,
            "file_sha256": hex::encode(report.file_sha256),
            "source_sha256": report.source_sha256.map(hex::encode),
            "file_bytes": report.file_bytes,
            "tensors": report.tensors.len(),
            "shards_read": report.shards_read,
            "store_bytes_read": report.store_bytes_read,
            "store_reads": report.store_reads,
            "per_tensor": report.tensors.iter().map(|t| json!({
                "name": t.name,
                "bytes": t.bytes,
                "full_root_verified": t.full_root_verified,
                "shard_roots_verified": t.shard_roots_verified,
                "sha256": hex::encode(t.sha256),
            })).collect::<Vec<_>>(),
        })
    );
    Ok(())
}

fn verify(args: &[String]) -> Result<(), String> {
    let store_path = arg(args, "--store").ok_or("verify needs --store")?;
    let name = arg(args, "--name").unwrap_or("model");
    let store = open_store(store_path)?;
    let started = Instant::now();
    let tensors = weftfs::checkpoint::verify_checkpoint(&store, name).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let report = store
        .restore_checkpoint(name, &CheckpointRestoreOptions::default(), &mut bytes)
        .map_err(|e| e.to_string())?;
    let full = store.verify(true).map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({
            "phase": "verify",
            "tensors_verified": tensors,
            "exact_source_file": report.exact_source_file,
            "file_sha256": hex::encode(report.file_sha256),
            "source_sha256": report.source_sha256.map(hex::encode),
            "verify_full_problems": full.problems.len(),
            "wall_ms": started.elapsed().as_secs_f64() * 1000.0,
            "strategies": strategy_histogram(&store)?,
            "stopped": false,
        })
    );
    Ok(())
}

fn loadreceipt(args: &[String]) -> Result<(), String> {
    let store_path = arg(args, "--store").ok_or("loadreceipt needs --store")?;
    let name = arg(args, "--name").unwrap_or("model");
    let verification = LoadVerification::parse(arg(args, "--verification").unwrap_or("lazy"))
        .map_err(|e| e.to_string())?;
    let reps: usize = arg(args, "--reps").unwrap_or("1").parse().map_err(|_| "bad --reps")?;
    let receipt_out = arg(args, "--receipt-out").map(PathBuf::from);
    let host_out = PathBuf::from(arg(args, "--host-out").unwrap_or("/tmp/loadreceipt-out.safetensors"));
    let store = Store::open(Path::new(store_path), OpenMode::ReadOnly)
        .map_err(|e| e.to_string())?;
    let mut samples = Vec::new();
    let mut last = None;
    for _ in 0..reps {
        let mut writer = fs::File::create(&host_out).map_err(|e| e.to_string())?;
        let started = Instant::now();
        let (report, receipt) = store
            .restore_checkpoint_with_load_receipt(
                name,
                &CheckpointRestoreOptions::default(),
                &LoadReceiptOptions {
                    replica: arg(args, "--replica").unwrap_or("court").to_owned(),
                    verification,
                },
                &mut writer,
            )
            .map_err(|e| e.to_string())?;
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        samples.push(wall_ms);
        last = Some((report, receipt));
    }
    let (report, receipt) = last.ok_or("no restore ran")?;
    if let Some(path) = &receipt_out {
        fs::write(path, receipt.encode().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    println!(
        "{}",
        json!({
            "phase": "loadreceipt",
            "verification": verification.as_str(),
            "reps": reps,
            "samples_ms": samples,
            "median_ms": median(&samples),
            "bytes_produced": receipt.bytes_produced,
            "units_verified": receipt.units_verified,
            "tensors": receipt.tensors.len(),
            "exact_source_file": report.exact_source_file,
            "store_bytes_read": report.store_bytes_read,
            "store_reads": report.store_reads,
            "loaded_set_root": receipt.loaded_set_root.to_hex(),
            "attested_set_root": receipt.attested_set_root.to_hex(),
            "receipt_root": receipt.root().map(|r| r.to_hex()).map_err(|e| e.to_string())?,
            "peak_rss_kib": peak_rss_kib(),
        })
    );
    Ok(())
}

fn recovery(args: &[String]) -> Result<(), String> {
    let store_path = arg(args, "--store").ok_or("recovery needs --store")?;
    let name = arg(args, "--name").unwrap_or("model");
    let required: Vec<String> = arg(args, "--required")
        .ok_or("recovery needs --required a,b,c")?
        .split(',')
        .map(str::to_owned)
        .collect();
    let cache = arg(args, "--cache").map(PathBuf::from);
    let host_out = PathBuf::from(arg(args, "--host-out").unwrap_or("/tmp/recovery-out.safetensors"));
    let store = Store::open(Path::new(store_path), OpenMode::ReadOnly)
        .map_err(|e| e.to_string())?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&host_out)
        .map_err(|e| e.to_string())?;
    let report = store
        .restore_checkpoint_recovery_first(
            name,
            &CheckpointRestoreOptions::default(),
            &RecoveryFirstOptions {
                required,
                cache_dir: cache,
                abort_after_required: flag(args, "--abort-after-required"),
            },
            &mut file,
        )
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({
            "phase": "recovery",
            "required_order": report.required_order,
            "cached_required": report.cached_required.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            "time_to_first_usable_ms": report.time_to_first_usable_ms,
            "time_to_complete_ms": report.time_to_complete_ms,
            "bytes_fetched_before_first_usable": report.bytes_fetched_before_first_usable,
            "bytes_fetched_total": report.bytes_fetched_total,
            "store_reads_before_first_usable": report.store_reads_before_first_usable,
            "exact_source_file": report.exact_source_file,
            "file_bytes": report.file_bytes,
            "tensors": report.tensors.len(),
            "peak_rss_kib": peak_rss_kib(),
        })
    );
    Ok(())
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sorted[sorted.len() / 2]
}

fn assemble(args: &[String]) -> Result<(), String> {
    let work = PathBuf::from(arg(args, "--work").ok_or("assemble needs --work")?);
    let out = PathBuf::from(arg(args, "--out").ok_or("assemble needs --out")?);
    let mut phases: BTreeMap<String, Json> = BTreeMap::new();
    let mut entries: Vec<_> = fs::read_dir(&work)
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            // An aborted crash-proxy child flushed no report; its exit status
            // is in the sibling .txt file.
            continue;
        }
        let value: Json = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("assemble: skipping {} ({err})", path.display());
                continue;
            }
        };
        let key = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("phase")
            .to_owned();
        phases.insert(key, value);
    }
    let receipt = json!({
        "schema": "weft.write-policy.court.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_default(),
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_default(),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_default(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_default(),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_default(),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_default(),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_default(),
        },
        "revisions": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_default(),
            "weftfs_go": std::env::var("WEFT_REPO_WEFTFS_GO").unwrap_or_default(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_default(),
        },
        "phases": phases,
    });
    fs::write(
        &out,
        serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("assembled {}", out.display());
    Ok(())
}
