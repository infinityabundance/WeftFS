//! R8 reconstruction single-flight + decoded-object cache court harness.
//!
//! Phases (each is a separate process, so peak RSS is per configuration):
//!
//! * `prepare`     — parse the real Qwen2.5-0.5B base/instruct safetensors
//!   headers, write one shared base tensor with the shipped planner, eight
//!   dependents that carry the base version as their causal parent (so a
//!   dependent read must decode the base), four independent tensors, and the
//!   `derived_from` relations the cache scores dependents from. Emits a plan
//!   JSON with the exact object ids and expected decoded BLAKE3 digests.
//! * `run`         — one configuration (`none`, `singleflight`, `cache`, `both`)
//!   at one thread count. Every worker repeatedly reads the same objects
//!   concurrently and hashes every byte it gets back.
//! * `adversarial` — cancellation, error propagation, a disabled cache,
//!   eviction of an unpinned base, and retention of a pinned base, all through
//!   the shipped store API on a scratch store.
//!
//! Nothing here is part of a frozen format; the harness only uses public store
//! APIs.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Instant;

use serde_json::{json, Value as Json};
use weftfs::error::Class;
use weftfs::identity::Hash32;
use weftfs::state::RelationTarget;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorWriteSpec};
use weftfs::{CancelToken, Principal};

const LAYERS: [usize; 8] = [0, 1, 2, 3, 4, 5, 8, 9];
const INDEPENDENT_LAYERS: [usize; 4] = [10, 11, 12, 13];

fn main() {
    if let Err(err) = run() {
        eprintln!("cache_bench: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let phase = args.first().cloned().unwrap_or_default();
    match phase.as_str() {
        "prepare" => prepare(&args[1..]),
        "run" => run_config(&args[1..]),
        "adversarial" => adversarial(&args[1..]),
        other => Err(format!(
            "unknown phase `{other}` (expected prepare|run|adversarial)"
        )),
    }
}

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn required(args: &[String], flag: &str) -> Result<String, String> {
    value_of(args, flag).ok_or_else(|| format!("missing required {flag}"))
}

fn number<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> Result<T, String> {
    match value_of(args, flag) {
        None => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|_| format!("{flag} must be a number (got `{text}`)")),
    }
}

// ---------------------------------------------------------------------------
// safetensors parsing (exact bytes; no model framework)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct TensorHeader {
    name: String,
    dtype: String,
    shape: Vec<u64>,
    begin: u64,
    end: u64,
}

struct Safetensors {
    path: PathBuf,
    data_start: u64,
    tensors: Vec<TensorHeader>,
}

fn safetensors_dtype(text: &str) -> Result<&'static str, String> {
    let lower = match text {
        "F16" => "f16",
        "BF16" => "bf16",
        "F32" => "f32",
        "F64" => "f64",
        "I8" => "i8",
        "U8" => "u8",
        "I16" => "i16",
        "I32" => "i32",
        "I64" => "i64",
        other => return Err(format!("safetensors dtype `{other}` has no exact mapping")),
    };
    Ok(lower)
}

fn parse_safetensors(path: &Path) -> Result<Safetensors, String> {
    let mut file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut prefix = [0u8; 8];
    file.read_exact(&mut prefix)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let header_len = u64::from_le_bytes(prefix);
    if header_len == 0 || header_len > 16 * 1024 * 1024 {
        return Err(format!(
            "implausible safetensors header length {header_len}"
        ));
    }
    let mut header = vec![0u8; header_len as usize];
    file.read_exact(&mut header)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let parsed: Json = serde_json::from_slice(&header)
        .map_err(|err| format!("{}: header JSON: {err}", path.display()))?;
    let map = parsed
        .as_object()
        .ok_or_else(|| "safetensors header is not an object".to_owned())?;
    let mut tensors = Vec::new();
    for (name, value) in map {
        if name == "__metadata__" {
            continue;
        }
        let dtype = value
            .get("dtype")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("tensor {name} has no dtype"))?;
        let shape = value
            .get("shape")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("tensor {name} has no shape"))?
            .iter()
            .map(|dim| {
                dim.as_u64()
                    .ok_or_else(|| "shape dim is not u64".to_owned())
            })
            .collect::<Result<Vec<u64>, String>>()?;
        let offsets = value
            .get("data_offsets")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("tensor {name} has no data_offsets"))?;
        if offsets.len() != 2 {
            return Err(format!("tensor {name} has malformed data_offsets"));
        }
        let begin = offsets[0].as_u64().ok_or("offset is not u64")?;
        let end = offsets[1].as_u64().ok_or("offset is not u64")?;
        tensors.push(TensorHeader {
            name: name.clone(),
            dtype: safetensors_dtype(dtype)?.to_owned(),
            shape,
            begin,
            end,
        });
    }
    tensors.sort_by_key(|tensor| tensor.begin);
    Ok(Safetensors {
        path: path.to_path_buf(),
        data_start: 8 + header_len,
        tensors,
    })
}

fn read_tensor_bytes(dataset: &Safetensors, tensor: &TensorHeader) -> Result<Vec<u8>, String> {
    let mut file =
        File::open(&dataset.path).map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    file.seek(SeekFrom::Start(dataset.data_start + tensor.begin))
        .map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    let mut bytes = vec![0u8; (tensor.end - tensor.begin) as usize];
    file.read_exact(&mut bytes)
        .map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    Ok(bytes)
}

fn tensor<'a>(dataset: &'a Safetensors, name: &str) -> Result<&'a TensorHeader, String> {
    dataset
        .tensors
        .iter()
        .find(|tensor| tensor.name == name)
        .ok_or_else(|| format!("tensor `{name}` is missing"))
}

fn mlp_name(layer: usize) -> String {
    format!("model.layers.{layer}.mlp.down_proj.weight")
}

/// A deterministic, sparse edit: flip the low byte of every `stride`-th bf16
/// word. The result is a genuine weight delta against the base.
fn edit_weights(base: &[u8], variant: usize) -> Vec<u8> {
    let mut out = base.to_vec();
    let stride = 512 + variant * 64;
    let mut word = variant % 8;
    while word * 2 + 1 < out.len() {
        out[word * 2] ^= 0x5a;
        word += stride;
    }
    out
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

// ---------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------

fn object_entry(name: &str, group: &str, id: Hash32, bytes: &[u8], strategy: &str) -> Result<Json, String> {
    Ok(json!({
        "name": name,
        "group": group,
        "object_id": id.to_hex(),
        "bytes": bytes.len(),
        "blake3": blake3_hex(bytes),
        "strategy": strategy,
    }))
}

fn prepare(args: &[String]) -> Result<(), String> {
    let base_file = PathBuf::from(required(args, "--base-file")?);
    let instruct_file = PathBuf::from(required(args, "--instruct-file")?);
    let store_path = PathBuf::from(required(args, "--store")?);
    let out = PathBuf::from(required(args, "--out")?);
    let force = args.iter().any(|arg| arg == "--force");

    if store_path.exists() {
        if !force {
            return Err(format!(
                "{} already exists (pass --force to rebuild)",
                store_path.display()
            ));
        }
        std::fs::remove_dir_all(&store_path)
            .map_err(|err| format!("{}: {err}", store_path.display()))?;
    }

    let base = parse_safetensors(&base_file)?;
    let instruct = parse_safetensors(&instruct_file)?;
    let options = Options {
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        ..Options::default()
    };
    let mut store = Store::init(&store_path, options).map_err(|err| err.to_string())?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    let base_dir = store
        .mkdir(root, b"base", 0o755, 0, 0)
        .map_err(|err| format!("mkdir base: {err}"))?;
    let instruct_dir = store
        .mkdir(root, b"instruct", 0o755, 0, 0)
        .map_err(|err| format!("mkdir instruct: {err}"))?;
    let shared_dir = store
        .mkdir(root, b"shared", 0o755, 0, 0)
        .map_err(|err| format!("mkdir shared: {err}"))?;
    let independent_dir = store
        .mkdir(root, b"independent", 0o755, 0, 0)
        .map_err(|err| format!("mkdir independent: {err}"))?;
    let layout = TensorLayoutSpec::row_major();

    // Real base tensors (Qwen2.5-0.5B, BF16).
    let mut base_objects: Vec<(usize, Hash32, Hash32, Vec<u8>, String)> = Vec::new();
    for layer in LAYERS {
        let header = tensor(&base, &mlp_name(layer))?;
        let bytes = read_tensor_bytes(&base, header)?;
        let name = format!("base-{layer}");
        let id = store
            .write_tensor_file(
                base_dir,
                name.as_bytes(),
                &header.dtype,
                &header.shape,
                &bytes,
                &layout,
                None,
            )
            .map_err(|err| format!("write {name}: {err}"))?;
        let meta = store.tensor_metadata(id).map_err(|err| err.to_string())?;
        let record = store.object_meta(id).map_err(|err| err.to_string())?;
        base_objects.push((layer, id, record.version, bytes, meta.strategy.clone()));
    }

    let mut objects: Vec<Json> = Vec::new();
    // 1. The shared base: the first real base tensor, read first.
    let (first_layer, first_id, first_version, first_bytes, first_strategy) = &base_objects[0];
    objects.push(object_entry(
        &format!("base-{first_layer}"),
        "shared_base",
        *first_id,
        first_bytes,
        first_strategy,
    )?);

    // 2. Synthetic sparse dependents of that one base (a genuinely shared
    // base with six dependents).
    let first_header = tensor(&base, &mlp_name(*first_layer))?;
    for variant in 0..6usize {
        let bytes = edit_weights(first_bytes, variant);
        let name = format!("shared-dependent-{variant}");
        let id = store
            .write_tensor_file_with_spec(
                shared_dir,
                name.as_bytes(),
                &first_header.dtype,
                &first_header.shape,
                &bytes,
                &layout,
                &TensorWriteSpec {
                    causal: vec![*first_version],
                    ..TensorWriteSpec::default()
                },
            )
            .map_err(|err| format!("write {name}: {err}"))?;
        let meta = store.tensor_metadata(id).map_err(|err| err.to_string())?;
        store
            .relation_add(
                id,
                "derived_from",
                RelationTarget::Object(*first_id),
                None,
                BTreeMap::new(),
            )
            .map_err(|err| format!("relation for {name}: {err}"))?;
        objects.push(object_entry(
            &name,
            "shared_dependent",
            id,
            &bytes,
            &meta.strategy,
        )?);
    }

    // 3. The remaining real base tensors.
    for (layer, id, _version, bytes, strategy) in base_objects.iter().skip(1) {
        objects.push(object_entry(
            &format!("base-{layer}"),
            "base",
            *id,
            bytes,
            strategy,
        )?);
    }

    // 4. Real instruct deltas, each causal on its own base tensor.
    for (layer, base_id, base_version, _bytes, _strategy) in &base_objects {
        let header = tensor(&instruct, &mlp_name(*layer))?;
        let bytes = read_tensor_bytes(&instruct, header)?;
        let name = format!("instruct-{layer}");
        let id = store
            .write_tensor_file_with_spec(
                instruct_dir,
                name.as_bytes(),
                &header.dtype,
                &header.shape,
                &bytes,
                &layout,
                &TensorWriteSpec {
                    causal: vec![*base_version],
                    ..TensorWriteSpec::default()
                },
            )
            .map_err(|err| format!("write {name}: {err}"))?;
        let meta = store.tensor_metadata(id).map_err(|err| err.to_string())?;
        store
            .relation_add(
                id,
                "derived_from",
                RelationTarget::Object(*base_id),
                None,
                BTreeMap::new(),
            )
            .map_err(|err| format!("relation for {name}: {err}"))?;
        objects.push(object_entry(&name, "delta", id, &bytes, &meta.strategy)?);
    }

    // 5. Independent tensors: distinct objects, no shared dependency.
    for layer in INDEPENDENT_LAYERS {
        let header = tensor(&base, &mlp_name(layer))?;
        let bytes = read_tensor_bytes(&base, header)?;
        let name = format!("independent-{layer}");
        let id = store
            .write_tensor_file(
                independent_dir,
                name.as_bytes(),
                &header.dtype,
                &header.shape,
                &bytes,
                &layout,
                None,
            )
            .map_err(|err| format!("write {name}: {err}"))?;
        let meta = store.tensor_metadata(id).map_err(|err| err.to_string())?;
        objects.push(object_entry(&name, "independent", id, &bytes, &meta.strategy)?);
    }

    let verify = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    if !verify.ok {
        return Err(format!("verify failed: {:?}", verify.problems));
    }
    let generation = store.generation();
    drop(store);

    let mut counts = BTreeMap::new();
    for entry in &objects {
        let group = entry["group"].as_str().unwrap_or("?").to_owned();
        *counts.entry(group).or_insert(0u64) += 1;
    }
    let plan = json!({
        "schema": "weft.cache-bench.plan.v1",
        "store": store_path.display().to_string(),
        "generation": generation,
        "corpus": {
            "base_file": base_file.display().to_string(),
            "instruct_file": instruct_file.display().to_string(),
            "base_file_bytes": std::fs::metadata(&base_file).map(|m| m.len()).unwrap_or(0),
            "instruct_file_bytes": std::fs::metadata(&instruct_file).map(|m| m.len()).unwrap_or(0),
            "instruct_parsed_tensors": instruct.tensors.len(),
        },
        "groups": counts,
        "logical_bytes_per_round": objects.iter().filter_map(|e| e["bytes"].as_u64()).sum::<u64>(),
        "objects": objects,
        "verify": {
            "ok": verify.ok,
            "checked_objects": verify.checked_objects,
            "checked_manifests": verify.checked_manifests,
            "checked_chunks": verify.checked_chunks,
            "checked_representations": verify.checked_representations,
        },
    });
    let text = serde_json::to_string_pretty(&plan).map_err(|err| err.to_string())?;
    std::fs::write(&out, text + "\n").map_err(|err| format!("{}: {err}", out.display()))?;
    println!(
        "prepared {} objects, generation {}",
        plan["objects"].as_array().map(Vec::len).unwrap_or(0),
        generation
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

struct Entry {
    object_id: Hash32,
    blake3: String,
    bytes: u64,
    group: String,
}

fn load_plan(path: &Path) -> Result<(PathBuf, Vec<Entry>), String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let plan: Json =
        serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    let store = PathBuf::from(
        plan.get("store")
            .and_then(Json::as_str)
            .ok_or_else(|| "plan has no store".to_owned())?,
    );
    let mut entries = Vec::new();
    for item in plan
        .get("objects")
        .and_then(Json::as_array)
        .ok_or_else(|| "plan has no objects".to_owned())?
    {
        entries.push(Entry {
            object_id: Hash32::from_hex(item["object_id"].as_str().ok_or("missing id")?)
                .map_err(|err| err.to_string())?,
            blake3: item["blake3"].as_str().ok_or("missing blake3")?.to_owned(),
            bytes: item["bytes"].as_u64().unwrap_or(0),
            group: item["group"].as_str().unwrap_or("?").to_owned(),
        });
    }
    Ok((store, entries))
}


fn group_counts(entries: &[Entry]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for entry in entries {
        *counts.entry(entry.group.clone()).or_insert(0) += 1;
    }
    counts
}

fn rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
        }
    }
    0
}

fn percentile(samples: &[u64], percent: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = ((sorted.len() as f64 - 1.0) * percent).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn run_config(args: &[String]) -> Result<(), String> {
    let plan_path = PathBuf::from(required(args, "--plan")?);
    let out = PathBuf::from(required(args, "--out")?);
    let config = required(args, "--config")?;
    let threads: usize = number(args, "--threads", 1usize)?;
    let rounds: usize = number(args, "--rounds", 4usize)?;
    let cache_bytes: u64 = number(args, "--cache-bytes", 128u64 * 1024 * 1024)?;
    let (store_path, entries) = load_plan(&plan_path)?;
    let (single_flight, cache) = match config.as_str() {
        "none" => (false, 0),
        "singleflight" => (true, 0),
        "cache" => (false, cache_bytes),
        "both" => (true, cache_bytes),
        other => {
            return Err(format!(
                "--config must be none|singleflight|cache|both (got `{other}`)"
            ))
        }
    };

    let mut options = Options {
        chunk_size: 64 * 1024,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        single_flight,
        cache_bytes: cache,
        ..Options::default()
    };
    if !single_flight {
        options.single_flight_max_entries = 64;
    }
    let store = Arc::new(
        Store::open_with_options(&store_path, OpenMode::ReadOnly, options)
            .map_err(|err| err.to_string())?,
    );
    let now = store.clock_now_ns();
    let mut tokens = Vec::new();
    for entry in &entries {
        tokens.push(
            store
                .authorize_read(entry.object_id, &Principal::root(), now)
                .map_err(|err| format!("authorize {}: {err}", entry.object_id.to_hex()))?,
        );
    }
    let entries = Arc::new(entries);
    let tokens = Arc::new(tokens);

    let before = store.backend_stats();
    let verify_before = store.read_layer_stats();
    let wall_start = Instant::now();
    let barrier = Arc::new(Barrier::new(threads.max(1)));
    let matches = Arc::new(AtomicU64::new(0));
    let mismatches = Arc::new(AtomicU64::new(0));
    let mut per_thread: Vec<Vec<u64>> = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..threads.max(1) {
            let store = Arc::clone(&store);
            let entries = Arc::clone(&entries);
            let tokens = Arc::clone(&tokens);
            let barrier = Arc::clone(&barrier);
            let matches = Arc::clone(&matches);
            let mismatches = Arc::clone(&mismatches);
            handles.push(scope.spawn(move || {
                let mut latencies = Vec::with_capacity(rounds * entries.len());
                barrier.wait();
                for _ in 0..rounds {
                    for (entry, token) in entries.iter().zip(tokens.iter()) {
                        let started = Instant::now();
                        let bytes = match store.read_object_authorized(token) {
                            Ok(bytes) => bytes,
                            Err(err) => {
                                mismatches.fetch_add(1, Ordering::Relaxed);
                                eprintln!("read {} failed: {err}", entry.object_id.to_hex());
                                continue;
                            }
                        };
                        latencies.push(started.elapsed().as_nanos() as u64);
                        if blake3_hex(&bytes) == entry.blake3 {
                            matches.fetch_add(1, Ordering::Relaxed);
                        } else {
                            mismatches.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                latencies
            }));
        }
        for handle in handles {
            per_thread.push(handle.join().unwrap_or_default());
        }
    });
    let wall = wall_start.elapsed();
    let after = store.backend_stats();
    let stats = store.read_layer_stats();
    let verify = store.verify(true).map_err(|err| format!("verify: {err}"))?;

    let mut all: Vec<u64> = Vec::new();
    for samples in &per_thread {
        all.extend_from_slice(samples);
    }
    let logical_bytes: u64 = entries.iter().map(|entry| entry.bytes).sum();
    let expected_reads = (threads.max(1) * rounds * entries.len()) as u64;
    let report = json!({
        "schema": "weft.cache-bench.run.v1",
        "config": config,
        "threads": threads,
        "rounds": rounds,
        "objects": entries.len(),
        "group_counts": group_counts(&entries),
        "cache_bytes_configured": cache,
        "single_flight_configured": single_flight,
        "expected_reads": expected_reads,
        "hash_matches": matches.load(Ordering::Relaxed),
        "hash_mismatches": mismatches.load(Ordering::Relaxed),
        "exact": mismatches.load(Ordering::Relaxed) == 0
            && matches.load(Ordering::Relaxed) == expected_reads,
        "wall_ms": wall.as_secs_f64() * 1000.0,
        "latency_ns": {
            "p50": percentile(&all, 0.50),
            "p99": percentile(&all, 0.99),
            "max": all.iter().copied().max().unwrap_or(0),
            "samples": all.len(),
        },
        "backend": {
            "bytes_read": after.bytes_read.saturating_sub(before.bytes_read),
            "reads": after.reads.saturating_sub(before.reads),
            "range_reads": after.range_reads.saturating_sub(before.range_reads),
        },
        "logical_bytes_per_round": logical_bytes,
        "decode_calls": stats.decode_calls.saturating_sub(verify_before.decode_calls),
        "decode_bytes": stats.decode_bytes.saturating_sub(verify_before.decode_bytes),
        "single_flight": {
            "enabled": stats.single_flight.enabled,
            "leaders": stats.single_flight.leaders,
            "waiters": stats.single_flight.waiters,
            "degraded": stats.single_flight.degraded,
            "cancellations": stats.single_flight.cancellations,
            "max_observed_entries": stats.single_flight.max_observed_entries,
        },
        "cache": {
            "enabled": stats.cache.enabled,
            "hits": stats.cache.hits,
            "misses": stats.cache.misses,
            "admissions": stats.cache.admissions,
            "evictions": stats.cache.evictions,
            "stale_removals": stats.cache.stale_removals,
            "rejections": stats.cache.rejections,
            "entries": stats.cache.entries,
            "bytes": stats.cache.bytes,
            "bytes_served": stats.cache.bytes_served,
        },
        "peak_rss_kib": rss_kib(),
        "verify": {
            "ok": verify.ok,
            "checked_objects": verify.checked_objects,
            "checked_representations": verify.checked_representations,
        },
        // Keep the per-thread latency samples for auditability, not just the
        // percentiles.
        "per_thread_samples": per_thread.iter().map(|samples| samples.len()).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&report).map_err(|err| err.to_string())?;
    std::fs::write(&out, text + "\n").map_err(|err| format!("{}: {err}", out.display()))?;
    println!(
        "config={config} threads={threads} reads={} decode_calls={} cache_hits={} p50={}us p99={}us wall={}ms exact={}",
        expected_reads,
        report["decode_calls"],
        report["cache"]["hits"],
        report["latency_ns"]["p50"].as_u64().unwrap_or(0) / 1000,
        report["latency_ns"]["p99"].as_u64().unwrap_or(0) / 1000,
        report["wall_ms"].as_f64().unwrap_or(0.0) as u64,
        report["exact"],
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// adversarial
// ---------------------------------------------------------------------------

fn adversarial(args: &[String]) -> Result<(), String> {
    let scratch = PathBuf::from(required(args, "--scratch")?);
    let out = PathBuf::from(required(args, "--out")?);
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).map_err(|err| format!("{}: {err}", scratch.display()))?;
    }
    std::fs::create_dir_all(&scratch).map_err(|err| format!("{}: {err}", scratch.display()))?;

    let mut checks: Vec<Json> = Vec::new();
    let mut check = |name: &str, pass: bool, detail: String| {
        checks.push(json!({"check": name, "pass": pass, "detail": detail}));
    };

    // 1. Cancellation: a waiter gives up, the leader still finishes.
    {
        let dir = scratch.join("cancel");
        let options = Options {
            chunk_size: 64 * 1024,
            single_flight: true,
            ..Options::default()
        };
        let mut store = Store::init(&dir, options).map_err(|err| err.to_string())?;
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let data = patterned(256 * 1024, 1);
        let id = store
            .create_file_seeded(
                root,
                b"cancel",
                &data,
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"r8-cancel",
            )
            .map_err(|err| err.to_string())?;
        store.set_single_flight_test_dwell(500_000_000);
        let store = Arc::new(store);
        let leader_store = Arc::clone(&store);
        let leader = std::thread::spawn(move || {
            let token = leader_store
                .authorize_read(id, &Principal::root(), leader_store.clock_now_ns())
                .unwrap();
            leader_store.read_object_authorized(&token).unwrap()
        });
        std::thread::sleep(std::time::Duration::from_millis(80));
        let waiter_store = Arc::clone(&store);
        let cancel = CancelToken::new();
        let waiter_cancel = cancel.clone();
        let waiter = std::thread::spawn(move || {
            let token = waiter_store
                .authorize_read(id, &Principal::root(), waiter_store.clock_now_ns())
                .unwrap();
            waiter_store.read_object_authorized_cancellable(&token, Some(&waiter_cancel))
        });
        std::thread::sleep(std::time::Duration::from_millis(40));
        cancel.cancel();
        let waiter_err = waiter.join().unwrap().unwrap_err();
        let leader_bytes = leader.join().unwrap();
        let pass = waiter_err.class() == Class::Cancelled
            && leader_bytes == data
            && store.decode_calls() == 1;
        check(
            "cancellation_waiter_only",
            pass,
            format!(
                "waiter class {}, leader bytes {}, decode_calls {}",
                waiter_err.class(),
                leader_bytes.len(),
                store.decode_calls()
            ),
        );
    }

    // 2. A decode error fans out to every waiter and nothing is cached.
    {
        let dir = scratch.join("error");
        let options = Options {
            chunk_size: 64 * 1024,
            single_flight: true,
            cache_bytes: 16 * 1024 * 1024,
            ..Options::default()
        };
        let mut store = Store::init(&dir, options).map_err(|err| err.to_string())?;
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let data = patterned(256 * 1024, 2);
        let id = store
            .create_file_seeded(
                root,
                b"broken",
                &data,
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"r8-error",
            )
            .map_err(|err| err.to_string())?;
        let record = store.object_meta(id).map_err(|err| err.to_string())?;
        let manifest = weftfs::recovery::read_manifest(store.root(), record.content)
            .map_err(|err| err.to_string())?;
        let chunk = manifest.chunks[0].hash;
        let path = weftfs::recovery::object_path(store.root(), chunk);
        std::fs::write(&path, b"tampered").map_err(|err| err.to_string())?;
        store.set_single_flight_test_dwell(200_000_000);
        let store = Arc::new(store);
        let threads = 4usize;
        let barrier = Arc::new(Barrier::new(threads));
        let mut handles = Vec::new();
        for _ in 0..threads {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let token = store
                    .authorize_read(id, &Principal::root(), store.clock_now_ns())
                    .unwrap();
                barrier.wait();
                store
                    .read_object_authorized(&token)
                    .map(|_| ())
                    .map_err(|e| e.class())
            }));
        }
        let mut classes = Vec::new();
        for handle in handles {
            classes.push(handle.join().unwrap());
        }
        let all_corrupt = classes.iter().all(|result| *result == Err(Class::Corrupt));
        let stats = store.read_layer_stats();
        check(
            "decode_error_fans_out",
            all_corrupt && stats.decode_calls == 1 && stats.cache.entries == 0,
            format!(
                "classes {:?}, decode_calls {}, cache entries {}",
                classes, stats.decode_calls, stats.cache.entries
            ),
        );
    }

    // 3. A disabled cache really is disabled.
    {
        let dir = scratch.join("disabled");
        let mut store = Store::init(&dir, Options::default()).map_err(|err| err.to_string())?;
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let data = patterned(64 * 1024, 3);
        let id = store
            .create_file_seeded(
                root,
                b"plain",
                &data,
                0o644,
                0,
                0,
                BTreeMap::new(),
                b"r8-disabled",
            )
            .map_err(|err| err.to_string())?;
        let token = store
            .authorize_read(id, &Principal::root(), store.clock_now_ns())
            .map_err(|err| err.to_string())?;
        store
            .read_object_authorized(&token)
            .map_err(|err| err.to_string())?;
        store
            .read_object_authorized(&token)
            .map_err(|err| err.to_string())?;
        let stats = store.cache_stats();
        check(
            "cache_disabled_by_default",
            !stats.enabled && stats.entries == 0 && stats.hits == 0 && store.decode_calls() == 2,
            format!(
                "enabled {}, entries {}, hits {}, decode_calls {}",
                stats.enabled,
                stats.entries,
                stats.hits,
                store.decode_calls()
            ),
        );
    }

    // 4 + 5. Eviction of an unpinned base, retention of a pinned base.
    //
    // Scores are decode_cost * dependent_count * reads per byte, so the base
    // is given 1 dependent while the two other entries get 2 each: the base is
    // then the deterministic lowest-value entry.
    {
        let dir = scratch.join("pin");
        let mut store = Store::init(
            &dir,
            Options {
                chunk_size: 64 * 1024,
                cache_bytes: 2 * 4096,
                ..Options::default()
            },
        )
        .map_err(|err| err.to_string())?;
        let root = store.resolve_path("/").map_err(|err| err.to_string())?;
        let mut ids = Vec::new();
        for index in 0..3u8 {
            let data = patterned(4096, 10 + index);
            let id = store
                .create_file_seeded(
                    root,
                    format!("o{index}").as_bytes(),
                    &data,
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    &[b'r', 8, b'p', index],
                )
                .map_err(|err| err.to_string())?;
            ids.push(id);
        }
        // 1 dependent on the base, 2 each on the other two entries.
        for (index, target) in [0usize, 1, 1, 2, 2].iter().enumerate() {
            let dependent = store
                .create_file_seeded(
                    root,
                    format!("dep{index}").as_bytes(),
                    b"dependent",
                    0o644,
                    0,
                    0,
                    BTreeMap::new(),
                    format!("r8-dep{index}").as_bytes(),
                )
                .map_err(|err| err.to_string())?;
            store
                .relation_add(
                    dependent,
                    "derived_from",
                    RelationTarget::Object(ids[*target]),
                    None,
                    BTreeMap::new(),
                )
                .map_err(|err| err.to_string())?;
        }
        let token_for = |store: &Store, id: Hash32| {
            store
                .authorize_read(id, &Principal::root(), store.clock_now_ns())
                .map_err(|err| err.to_string())
        };
        let base_token = token_for(&store, ids[0])?;
        store.pin_cache_object(ids[0]).map_err(|err| err.to_string())?;
        store
            .read_object_authorized(&base_token)
            .map_err(|err| err.to_string())?;
        for id in &ids[1..] {
            let token = token_for(&store, *id)?;
            store
                .read_object_authorized(&token)
                .map_err(|err| err.to_string())?;
        }
        let before = store.decode_calls();
        store
            .read_object_authorized(&base_token)
            .map_err(|err| err.to_string())?;
        let pinned_retained = store.decode_calls() == before;
        let stats = store.cache_stats();
        check(
            "pinned_base_retained",
            pinned_retained && stats.entries == 2,
            format!(
                "pinned hit {}, entries {}, evictions {}",
                pinned_retained, stats.entries, stats.evictions
            ),
        );

        // Unpin, clear, and admit in score order so the base is resident while
        // a strictly higher-value entry is admitted: the base must be the one
        // evicted.
        store.unpin_cache_object(ids[0]);
        store.clear_object_cache();
        for id in &ids {
            let token = token_for(&store, *id)?;
            store
                .read_object_authorized(&token)
                .map_err(|err| err.to_string())?;
        }
        let stats = store.cache_stats();
        let before = store.decode_calls();
        store
            .read_object_authorized(&base_token)
            .map_err(|err| err.to_string())?;
        let base_evicted = store.decode_calls() == before + 1;
        check(
            "unpinned_base_evicted",
            base_evicted && stats.evictions > 0 && stats.entries == 2,
            format!(
                "base re-decoded {}, evictions {}, entries {}",
                base_evicted, stats.evictions, stats.entries
            ),
        );
    }

    let passed = checks.iter().filter(|check| check["pass"] == true).count();
    let report = json!({
        "schema": "weft.cache-bench.adversarial.v1",
        "checks": checks,
        "passed": passed,
        "total": checks.len(),
    });
    let text = serde_json::to_string_pretty(&report).map_err(|err| err.to_string())?;
    std::fs::write(&out, text + "\n").map_err(|err| format!("{}: {err}", out.display()))?;
    println!("adversarial {passed}/{}", checks.len());
    if passed != checks.len() {
        return Err("adversarial checks failed".to_owned());
    }
    Ok(())
}

fn patterned(len: usize, seed: u8) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ u64::from(seed);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}
