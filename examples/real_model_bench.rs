//! Real public checkpoint family benchmark for the tensor representation planner.
//!
//! This harness drives the shipped store API, not a model framework. It parses
//! the safetensors header itself (8-byte little-endian length + JSON header)
//! and feeds each tensor's exact bytes to `write_tensor_file_with_spec` with
//! `strategy_hint = None`, so the planner chooses every representation.
//!
//! Two phases run in separate processes so peak RSS is honest for each:
//!
//! * `ingest`  — writes both checkpoints (base first, then the instruct
//!   checkpoint with the corresponding base tensor version as its causal
//!   parent), records the chosen strategy per tensor, and walks the store
//!   directory for physical accounting.
//! * `restore` — reopens the store read-only, reads and hashes every tensor,
//!   recomputes every logical root, reassembles the original safetensors file
//!   byte-for-byte, and measures a single-tensor restore. With
//!   `--materialize-pipeline` the reassembly streams decoded tensors to the
//!   destination in source order while workers keep decoding
//!   (`RestorePipelineOptions`); without it the shipped batched materialize is
//!   measured.
//!
//! The safetensors header (including its 8-byte length prefix) is stored as a
//! raw object so the complete original file can be reconstructed exactly.
//!
//! Usage (inside the bench court):
//!   cargo run --release --example real_model_bench -- ingest \
//!       --base-file ... --instruct-file ... --store ... --out ingest.json
//!   cargo run --release --example real_model_bench -- restore \
//!       --store ... --ingest-json ingest.json --out restore.json \
//!       --reassemble-dir /work/.docker-cache/real-corpus/restored

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use weftfs::backend::BackendStats;
use weftfs::parallel::{ParallelReadOptions, ParallelReadStats};
use weftfs::restore_pipeline::{RestorePipelineOptions, RestorePipelineStats};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec};
use weftfs::TileDecodeOptions;

const HEADER_OBJECT: &str = "__safetensors_header__";

fn main() {
    if let Err(err) = run() {
        eprintln!("real_model_bench: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let phase = args.first().cloned().unwrap_or_default();
    match phase.as_str() {
        "ingest" => ingest(&args[1..]),
        "restore" => restore(&args[1..]),
        "tiled" => tiled(&args[1..]),
        "hot" => hot(&args[1..]),
        "stats" => stats(&args[1..]),
        "timing" => timing(&args[1..]),
        other => Err(format!(
            "unknown phase `{other}` (expected `ingest`, `restore`, `tiled`, `hot`, `stats`, or `timing`)"
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

/// Optional R8 read layers for the read-side phases.
///
/// `--cache-bytes N` enables the verified decoded-object cache and
/// `--single-flight` enables coalescing. Both default to off, so a harness run
/// without the flags is the shipped read path. `tiled` deliberately does not
/// use these (mode 15 returns per-request tile statistics and bypasses the
/// layers by design).
fn read_layer_options(args: &[String]) -> Result<Options, String> {
    let cache_bytes: u64 = value_of(args, "--cache-bytes")
        .map(|text| {
            text.parse()
                .map_err(|_| "--cache-bytes must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(0);
    let single_flight = args.iter().any(|arg| arg == "--single-flight");
    let max_entries: usize = value_of(args, "--single-flight-max-entries")
        .map(|text| {
            text.parse()
                .map_err(|_| "--single-flight-max-entries must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(64);
    let max_bytes: u64 = value_of(args, "--single-flight-max-bytes")
        .map(|text| {
            text.parse()
                .map_err(|_| "--single-flight-max-bytes must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(256 * 1024 * 1024);
    Ok(Options {
        cache_bytes,
        single_flight,
        single_flight_max_entries: max_entries,
        single_flight_max_bytes: max_bytes,
        ..Options::default()
    })
}

// ---------------------------------------------------------------------------
// safetensors parsing (no transformers/model dependency)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct TensorHeader {
    name: String,
    dtype: String,
    shape: Vec<u64>,
    begin: u64,
    end: u64,
}

impl TensorHeader {
    fn byte_len(&self) -> u64 {
        self.end - self.begin
    }
}

struct Safetensors {
    path: PathBuf,
    file_len: u64,
    /// Exact bytes of the length prefix plus the JSON header.
    header_bytes: Vec<u8>,
    data_start: u64,
    /// Tensors in file data order (ascending `data_offsets[0]`).
    tensors: Vec<TensorHeader>,
}

fn safetensors_dtype(text: &str) -> Result<&'static str, String> {
    // Safetensors names are uppercase; WEFT-TENSOR names are lowercase.
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
        other => {
            return Err(format!(
                "safetensors dtype `{other}` has no exact WEFT-TENSOR v1 mapping"
            ))
        }
    };
    Ok(lower)
}

fn element_bytes(dtype: &str) -> u64 {
    match dtype {
        "f16" | "bf16" | "i16" => 2,
        "f32" | "i32" => 4,
        "f64" | "i64" => 8,
        _ => 1,
    }
}

fn parse_safetensors(path: &Path) -> Result<Safetensors, String> {
    let file_len = fs::metadata(path)
        .map_err(|err| format!("{}: {err}", path.display()))?
        .len();
    let mut file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut length_bytes = [0u8; 8];
    file.read_exact(&mut length_bytes)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let header_len = u64::from_le_bytes(length_bytes);
    if header_len > 64 * 1024 * 1024 || 8 + header_len > file_len {
        return Err(format!(
            "{}: implausible safetensors header length {header_len} (file {file_len})",
            path.display()
        ));
    }
    let mut header_json = vec![0u8; header_len as usize];
    file.read_exact(&mut header_json)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let mut header_bytes = Vec::with_capacity(8 + header_len as usize);
    header_bytes.extend_from_slice(&length_bytes);
    header_bytes.extend_from_slice(&header_json);
    let parsed: Json = serde_json::from_slice(&header_json)
        .map_err(|err| format!("{}: header JSON: {err}", path.display()))?;
    let map = parsed
        .as_object()
        .ok_or_else(|| format!("{}: header is not a JSON object", path.display()))?;
    let data_start = 8 + header_len;
    let mut tensors = Vec::new();
    for (name, entry) in map {
        if name == "__metadata__" {
            continue;
        }
        let dtype = entry
            .get("dtype")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("{}/{name}: missing dtype", path.display()))?;
        let shape: Vec<u64> = entry
            .get("shape")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("{}/{name}: missing shape", path.display()))?
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| format!("{}/{name}: non-integer shape", path.display()))
            })
            .collect::<Result<_, _>>()?;
        let offsets = entry
            .get("data_offsets")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("{}/{name}: missing data_offsets", path.display()))?;
        if offsets.len() != 2 {
            return Err(format!(
                "{}/{name}: data_offsets must have 2 entries",
                path.display()
            ));
        }
        let begin = offsets[0]
            .as_u64()
            .ok_or_else(|| format!("{}/{name}: bad data_offsets[0]", path.display()))?;
        let end = offsets[1]
            .as_u64()
            .ok_or_else(|| format!("{}/{name}: bad data_offsets[1]", path.display()))?;
        let canonical = safetensors_dtype(dtype)?;
        let elements: u64 = shape.iter().product();
        let expected = elements
            .checked_mul(element_bytes(canonical))
            .ok_or_else(|| format!("{}/{name}: shape overflows byte length", path.display()))?;
        if end < begin || end - begin != expected {
            return Err(format!(
                "{}/{name}: data_offsets [{begin},{end}] do not match {elements} x {canonical}",
                path.display()
            ));
        }
        if data_start + end > file_len {
            return Err(format!(
                "{}/{name}: tensor extends past the file ({data_start}+{end} > {file_len})",
                path.display()
            ));
        }
        tensors.push(TensorHeader {
            name: name.clone(),
            dtype: canonical.to_owned(),
            shape,
            begin,
            end,
        });
    }
    tensors.sort_by_key(|tensor| tensor.begin);
    // A safetensors data section is a contiguous packing of the tensors.
    let mut cursor = 0u64;
    for tensor in &tensors {
        if tensor.begin != cursor {
            return Err(format!(
                "{}: data section is not contiguous at {} (begin {}, expected {})",
                path.display(),
                tensor.name,
                tensor.begin,
                cursor
            ));
        }
        cursor = tensor.end;
    }
    if data_start + cursor != file_len {
        return Err(format!(
            "{}: data section ends at {} but file is {file_len} bytes",
            path.display(),
            data_start + cursor
        ));
    }
    Ok(Safetensors {
        path: path.to_path_buf(),
        file_len,
        header_bytes,
        data_start,
        tensors,
    })
}

fn read_tensor_bytes(dataset: &Safetensors, tensor: &TensorHeader) -> Result<Vec<u8>, String> {
    let mut file =
        File::open(&dataset.path).map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    file.seek(SeekFrom::Start(dataset.data_start + tensor.begin))
        .map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    let mut bytes = vec![0u8; tensor.byte_len() as usize];
    file.read_exact(&mut bytes)
        .map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    Ok(bytes)
}

fn file_blake3(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

// ---------------------------------------------------------------------------
// ingest
// ---------------------------------------------------------------------------

fn ingest(args: &[String]) -> Result<(), String> {
    let base_file = PathBuf::from(required(args, "--base-file")?);
    let instruct_file = PathBuf::from(required(args, "--instruct-file")?);
    let skip_instruct = args.iter().any(|arg| arg == "--skip-instruct");
    let store_path = PathBuf::from(required(args, "--store")?);
    let out = PathBuf::from(required(args, "--out")?);
    let label = value_of(args, "--label").unwrap_or_else(|| "real-model".to_owned());
    let chunk_size: u64 = value_of(args, "--chunk-size")
        .map(|text| {
            text.parse()
                .map_err(|_| "--chunk-size must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(64 * 1024);
    let checkpoint_interval: u64 = value_of(args, "--checkpoint-interval")
        .map(|text| {
            text.parse()
                .map_err(|_| "--checkpoint-interval must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(256);
    let auto_tile_bytes: Option<u64> = value_of(args, "--auto-tile-bytes")
        .map(|text| {
            text.parse()
                .map_err(|_| "--auto-tile-bytes must be an integer".to_owned())
        })
        .transpose()?;
    // Control configuration for the tiled-parallel court: force mode 15 for
    // rank-2 tensors with an auto-derived tile shape. Rank > 2 keeps the
    // planner, because v1 auto-tile derivation is defined for rank 2 only.
    let force_tiled = args.iter().any(|arg| arg == "--force-tiled");

    let base = parse_safetensors(&base_file)?;
    let instruct = parse_safetensors(&instruct_file)?;
    // The pair must be structurally comparable for a causal delta.
    let base_names: BTreeMap<&str, &TensorHeader> =
        base.tensors.iter().map(|t| (t.name.as_str(), t)).collect();
    let instruct_names: BTreeMap<&str, &TensorHeader> = instruct
        .tensors
        .iter()
        .map(|t| (t.name.as_str(), t))
        .collect();
    let mut base_keys: Vec<&str> = base_names.keys().copied().collect();
    let mut instruct_keys: Vec<&str> = instruct_names.keys().copied().collect();
    base_keys.sort_unstable();
    instruct_keys.sort_unstable();
    if base_keys != instruct_keys {
        return Err("checkpoints do not have the same tensor names".to_owned());
    }
    let mut shape_mismatches = Vec::new();
    let mut dtype_mismatches = Vec::new();
    for (name, base_tensor) in &base_names {
        let other = instruct_names
            .get(name)
            .ok_or_else(|| format!("instruct checkpoint is missing {name}"))?;
        if base_tensor.shape != other.shape {
            shape_mismatches.push((*name).to_owned());
        }
        if base_tensor.dtype != other.dtype {
            dtype_mismatches.push(format!("{name}: {} vs {}", base_tensor.dtype, other.dtype));
        }
    }
    let causal_possible = shape_mismatches.is_empty() && dtype_mismatches.is_empty();

    if store_path.exists() {
        fs::remove_dir_all(&store_path)
            .map_err(|err| format!("{}: {err}", store_path.display()))?;
    }
    let options = Options {
        chunk_size,
        checkpoint_interval,
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

    let mut notes = Vec::new();
    if !causal_possible {
        notes.push(
            "checkpoints differ in dtype or shape; the planner was offered no causal parent"
                .to_owned(),
        );
    }

    let layout = TensorLayoutSpec::row_major();
    let base_start = Instant::now();
    store
        .create_file(
            base_dir,
            HEADER_OBJECT.as_bytes(),
            &base.header_bytes,
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .map_err(|err| format!("write base header: {err}"))?;
    let mut base_records = Vec::new();
    let mut base_versions: BTreeMap<String, weftfs::Hash32> = BTreeMap::new();
    let mut base_write_ms = 0.0f64;
    for tensor in &base.tensors {
        let bytes = read_tensor_bytes(&base, tensor)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let spec = TensorWriteSpec {
            strategy_hint: if force_tiled && tensor.shape.len() == 2 {
                Some(TensorStrategy::TensorTiled)
            } else {
                None
            },
            plan: TensorPlanOptions {
                auto_tile_bytes,
                ..TensorPlanOptions::default()
            },
            ..TensorWriteSpec::default()
        };
        let started = Instant::now();
        let id = store
            .write_tensor_file_with_spec(
                base_dir,
                tensor.name.as_bytes(),
                &tensor.dtype,
                &tensor.shape,
                &bytes,
                &layout,
                &spec,
            )
            .map_err(|err| format!("write base/{}: {err}", tensor.name))?;
        base_write_ms += started.elapsed().as_secs_f64() * 1000.0;
        let meta = store
            .tensor_metadata(id)
            .map_err(|err| format!("metadata base/{}: {err}", tensor.name))?;
        let record = store
            .object_meta(id)
            .map_err(|err| format!("record base/{}: {err}", tensor.name))?;
        base_versions.insert(tensor.name.clone(), record.version);
        base_records.push(json!({
            "name": tensor.name,
            "dtype": tensor.dtype,
            "shape": tensor.shape,
            "bytes": tensor.byte_len(),
            "blake3": hash,
            "object_id": id.to_hex(),
            "version": record.version.to_hex(),
            "logical_root": meta.logical_root.to_hex(),
            "strategy": meta.strategy,
            "tile_shape": meta.tile_shape,
        }));
    }
    let base_ingest_ms = base_start.elapsed().as_secs_f64() * 1000.0;
    let base_stats = store.backend_stats();

    let instruct_start = Instant::now();
    if !skip_instruct {
        store
            .create_file(
                instruct_dir,
                HEADER_OBJECT.as_bytes(),
                &instruct.header_bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .map_err(|err| format!("write instruct header: {err}"))?;
    }
    let mut instruct_records = Vec::new();
    let mut instruct_write_ms = 0.0f64;
    for (index, tensor) in instruct.tensors.iter().enumerate() {
        if skip_instruct {
            break;
        }
        let bytes = read_tensor_bytes(&instruct, tensor)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let mut spec = TensorWriteSpec {
            strategy_hint: if force_tiled && tensor.shape.len() == 2 {
                Some(TensorStrategy::TensorTiled)
            } else {
                None
            },
            plan: TensorPlanOptions {
                auto_tile_bytes,
                ..TensorPlanOptions::default()
            },
            ..TensorWriteSpec::default()
        };
        if causal_possible {
            if let Some(version) = base_versions.get(&tensor.name) {
                spec.causal = vec![*version];
            }
        }
        let started = Instant::now();
        let id = store
            .write_tensor_file_with_spec(
                instruct_dir,
                tensor.name.as_bytes(),
                &tensor.dtype,
                &tensor.shape,
                &bytes,
                &layout,
                &spec,
            )
            .map_err(|err| format!("write instruct/{}: {err}", tensor.name))?;
        instruct_write_ms += started.elapsed().as_secs_f64() * 1000.0;
        let meta = store
            .tensor_metadata(id)
            .map_err(|err| format!("metadata instruct/{}: {err}", tensor.name))?;
        let record = store
            .object_meta(id)
            .map_err(|err| format!("record instruct/{}: {err}", tensor.name))?;
        instruct_records.push(json!({
            "index": index,
            "name": tensor.name,
            "dtype": tensor.dtype,
            "shape": tensor.shape,
            "bytes": tensor.byte_len(),
            "blake3": hash,
            "object_id": id.to_hex(),
            "version": record.version.to_hex(),
            "logical_root": meta.logical_root.to_hex(),
            "strategy": meta.strategy,
            "tile_shape": meta.tile_shape,
            "causal": if spec.causal.is_empty() { Json::Null } else { json!(spec.causal[0].to_hex()) },
        }));
    }
    let instruct_ingest_ms = instruct_start.elapsed().as_secs_f64() * 1000.0;
    let instruct_stats = store.backend_stats();

    let generation = store.generation();
    let verify = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    drop(store);

    let physical = physical_breakdown(&store_path)?;
    let base_distribution = distribution(&base_records);
    let instruct_distribution = distribution(&instruct_records);

    let receipt = json!({
        "schema": "weft.real-model.ingest.v1",
        "label": label,
        "phase": "ingest",
        "store": {
            "path": store_path.display().to_string(),
            "options": {
                "chunk_size": chunk_size,
                "checkpoint_interval": checkpoint_interval,
                "representation": "Raw (frozen v1 manifest)",
                "planner": "TensorPlanOptions (lambda_ns = 0, measured decode table, mode-9 candidate)",
                "auto_tile_bytes": auto_tile_bytes,
                "force_tiled_rank2": force_tiled,
                "max_semantic_candidates": weftfs::tensor_store::DEFAULT_TENSOR_MAX_CANDIDATES,
            },
            "generation": generation,
        },
        "corpus": {
            "base": corpus_summary(&base)?,
            "instruct": corpus_summary(&instruct)?,
            "same_tensor_names": true,
            "shape_mismatches": shape_mismatches,
            "dtype_mismatches": dtype_mismatches,
            "causal_parent_offered": causal_possible,
            "order": "safetensors data_offsets order",
        },
        "ingest": {
            "base": {
                "wall_ms": round(base_ingest_ms),
                "tensor_write_ms": round(base_write_ms),
                "header_object_bytes": base.header_bytes.len(),
                "backend": stats_json(&base_stats),
                "tensors": base_records,
            },
            "instruct": {
                "wall_ms": round(instruct_ingest_ms),
                "tensor_write_ms": round(instruct_write_ms),
                "header_object_bytes": instruct.header_bytes.len(),
                "backend": stats_json(&instruct_stats),
                "tensors": instruct_records,
            },
        },
        "strategy_distribution": {
            "base": base_distribution,
            "instruct": instruct_distribution,
        },
        "physical": physical,
        "peak_rss_kib": peak_rss_kib(),
        "verify": {"ok": verify.ok, "full": true},
        "notes": notes,
    });
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}

fn corpus_summary(dataset: &Safetensors) -> Result<Json, String> {
    let mut dtypes: BTreeMap<String, usize> = BTreeMap::new();
    for tensor in &dataset.tensors {
        *dtypes.entry(tensor.dtype.clone()).or_default() += 1;
    }
    Ok(json!({
        "path": dataset.path.display().to_string(),
        "file_bytes": dataset.file_len,
        "blake3": file_blake3(&dataset.path)?,
        "header_bytes": dataset.header_bytes.len(),
        "tensor_count": dataset.tensors.len(),
        "tensor_data_bytes": dataset.tensors.iter().map(|t| t.byte_len()).sum::<u64>(),
        "dtype_histogram": dtypes,
    }))
}

fn distribution(records: &[Json]) -> Json {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    let mut logical: BTreeMap<String, u64> = BTreeMap::new();
    for record in records {
        let strategy = record
            .get("strategy")
            .and_then(Json::as_str)
            .unwrap_or("unknown")
            .to_owned();
        *count.entry(strategy.clone()).or_default() += 1;
        *logical.entry(strategy).or_default() +=
            record.get("bytes").and_then(Json::as_u64).unwrap_or(0);
    }
    json!({"tensors_by_strategy": count, "logical_bytes_by_strategy": logical})
}

fn stats_json(stats: &BackendStats) -> Json {
    json!({
        "reads": stats.reads,
        "range_reads": stats.range_reads,
        "bytes_read": stats.bytes_read,
        "writes": stats.writes,
        "bytes_written": stats.bytes_written,
        "link_publications": stats.link_publications,
        "rename_publications": stats.rename_publications,
        "rename_fallbacks": stats.rename_fallbacks,
    })
}

fn physical_breakdown(store: &Path) -> Result<Json, String> {
    let objects = tree_bytes(&store.join("objects"))?;
    let manifests = tree_bytes(&store.join("manifests"))?;
    let indexes = tree_bytes(&store.join("indexes"))?;
    let commits = tree_bytes(&store.join("commits"))?;
    let total = tree_bytes(store)?;
    Ok(json!({
        "objects_bytes": objects,
        "manifests_bytes": manifests,
        "tensor_data_bytes": objects + manifests,
        "indexes_bytes": indexes,
        "commits_bytes": commits,
        "other_bytes": total.saturating_sub(objects + manifests + indexes + commits),
        "store_total_bytes": total,
    }))
}

// ---------------------------------------------------------------------------
// restore
// ---------------------------------------------------------------------------

fn restore(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let ingest_json = PathBuf::from(required(args, "--ingest-json")?);
    let out = PathBuf::from(required(args, "--out")?);
    let reassemble_dir = value_of(args, "--reassemble-dir").map(PathBuf::from);
    let single_tensor = value_of(args, "--single-tensor")
        .unwrap_or_else(|| "model.layers.10.mlp.down_proj.weight".to_owned());
    // Benchmark scoping only: `base`/`instruct` restore one checkpoint instead
    // of both. The library and CLI always restore exactly what is asked for.
    let only_checkpoint = value_of(args, "--checkpoint");
    if let Some(only) = &only_checkpoint {
        if only != "base" && only != "instruct" {
            return Err(format!(
                "--checkpoint must be `base` or `instruct` (got `{only}`)"
            ));
        }
    }
    let requested: Vec<(&str, &str)> = [("base", "base"), ("instruct", "instruct")]
        .into_iter()
        .filter(|(name, _)| {
            only_checkpoint
                .as_deref()
                .map(|only| only == *name)
                .unwrap_or(true)
        })
        .collect();
    let single_runs: usize = value_of(args, "--single-runs")
        .map(|text| {
            text.parse()
                .map_err(|_| "--single-runs must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(5);
    let threads: usize = value_of(args, "--threads")
        .map(|text| {
            text.parse()
                .map_err(|_| "--threads must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(1);
    // Staging bound for bounded parallel reads. Default 64 MiB matches
    // `ParallelReadOptions::default()`.
    let max_inflight_mib: usize = value_of(args, "--max-inflight-mib")
        .map(|text| {
            text.parse()
                .map_err(|_| "--max-inflight-mib must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(64);
    let parallel = ParallelReadOptions {
        threads,
        max_inflight_bytes: max_inflight_mib.saturating_mul(1024 * 1024),
    };
    let materialize_pipeline = args.iter().any(|arg| arg == "--materialize-pipeline");

    let ingest_text = fs::read_to_string(&ingest_json)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let ingest: Json = serde_json::from_str(&ingest_text)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;

    let open_start = Instant::now();
    let store =
        Store::open_with_options(&store_path, OpenMode::ReadOnly, read_layer_options(args)?)
            .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let open_ms = open_start.elapsed().as_secs_f64() * 1000.0;
    let open_stats = store.backend_stats();

    let mut checkpoints = Vec::new();
    for (name, dir_name) in requested.iter().copied() {
        let result = restore_checkpoint(
            &store,
            &ingest,
            name,
            dir_name,
            reassemble_dir.as_deref(),
            &parallel,
            materialize_pipeline,
        )?;
        checkpoints.push(result);
    }

    let mut single = Vec::new();
    for (checkpoint, dir_name) in requested.iter().copied() {
        let id = store
            .resolve_components(&[dir_name.as_bytes(), single_tensor.as_bytes()])
            .map_err(|err| format!("resolve {dir_name}/{single_tensor}: {err}"))?;
        let mut runs = Vec::new();
        let mut verified = false;
        let mut bytes = 0u64;
        for _ in 0..single_runs {
            let before = store.backend_stats();
            let started = Instant::now();
            let token = store
                .authorize_read(id, &weftfs::Principal::root(), store.clock_now_ns())
                .map_err(|err| format!("authorize {dir_name}/{single_tensor}: {err}"))?;
            let data = store
                .read_tensor_authorized(&token, &weftfs::TileDecodeOptions::default())
                .map_err(|err| format!("read {dir_name}/{single_tensor}: {err}"))?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            let after = store.backend_stats();
            bytes = data.len() as u64;
            verified =
                data.len() as u64 == expected_tensor_bytes(&ingest, checkpoint, &single_tensor);
            runs.push(json!({
                "ms": round(elapsed),
                "backend_bytes_read": after.bytes_read.saturating_sub(before.bytes_read),
                "backend_reads": after.reads.saturating_sub(before.reads),
            }));
        }
        let mut timings: Vec<f64> = runs
            .iter()
            .filter_map(|run| run.get("ms").and_then(Json::as_f64))
            .collect();
        timings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        single.push(json!({
            "checkpoint": checkpoint,
            "tensor": single_tensor,
            "logical_bytes": bytes,
            "size_matches_ingest": verified,
            "runs": runs,
            "median_ms": round(timings.get(timings.len() / 2).copied().unwrap_or(0.0)),
            "min_ms": round(timings.first().copied().unwrap_or(0.0)),
        }));
    }

    let receipt = json!({
        "schema": "weft.real-model.restore.v1",
        "phase": "restore",
        "store": store_path.display().to_string(),
        "threads": threads,
        "effective_threads": weftfs::parallel::effective_threads(threads),
        "max_inflight_bytes": parallel.max_inflight_bytes,
        "materialize_mode": if materialize_pipeline { "pipeline" } else { "batch" },
        "checkpoints_requested": requested.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        "open_ms": round(open_ms),
        "open_backend": stats_json(&open_stats),
        "checkpoints": checkpoints,
        "single_tensor_restore": single,
        "peak_rss_kib": peak_rss_kib(),
    });
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}

/// Aggregate of mode-15 tile counters over a pass.
#[derive(Default)]
struct TileTotals {
    tensors: usize,
    tiles: usize,
    effective_threads: usize,
    max_inflight_bytes: usize,
    observed_max_inflight_bytes: usize,
    observed_max_inflight_tasks: usize,
    oversized_tiles: usize,
    bytes: u64,
}

impl TileTotals {
    fn merge(&mut self, stats: &weftfs::TileDecodeStats) {
        self.tensors += 1;
        self.tiles += stats.tiles;
        self.effective_threads = stats.effective_threads;
        self.max_inflight_bytes = stats.max_inflight_bytes;
        self.observed_max_inflight_bytes = self
            .observed_max_inflight_bytes
            .max(stats.observed_max_inflight_bytes);
        self.observed_max_inflight_tasks = self
            .observed_max_inflight_tasks
            .max(stats.observed_max_inflight_tasks);
        self.oversized_tiles += stats.oversized_tiles;
    }

    fn to_json(&self, options: &TileDecodeOptions) -> Json {
        json!({
            "threads_requested": options.threads,
            "threads_effective": weftfs::effective_threads(options.threads),
            "max_inflight_bytes": options.max_inflight_bytes,
            "tensors": self.tensors,
            "tiles": self.tiles,
            "bytes": self.bytes,
            "observed_max_inflight_bytes": self.observed_max_inflight_bytes,
            "observed_max_inflight_tasks": self.observed_max_inflight_tasks,
            "oversized_tiles": self.oversized_tiles,
        })
    }
}

fn tile_stats_json(stats: &weftfs::TileDecodeStats) -> Json {
    json!({
        "threads_requested": stats.requested_threads,
        "threads_effective": stats.effective_threads,
        "tiles": stats.tiles,
        "max_inflight_bytes": stats.max_inflight_bytes,
        "observed_max_inflight_bytes": stats.observed_max_inflight_bytes,
        "observed_max_inflight_tasks": stats.observed_max_inflight_tasks,
        "oversized_tiles": stats.oversized_tiles,
    })
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn region_bytes(full: &[u8], shape: &[u64], region: &weftcodec::Region, element: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for row in region.start[0]..region.end[0] {
        for col in region.start[1]..region.end[1] {
            let at = ((row * shape[1] + col) as usize) * element;
            out.extend_from_slice(&full[at..at + element]);
        }
    }
    out
}

/// Full-checkpoint restore with intra-tensor mode-15 tile workers.
///
/// The read and materialize passes both call
/// `Store::read_tensor_with_options`, so the requested worker count applies to
/// every mode-15 tensor. Non-tiled representations keep the shipped sequential
/// read. Region reads verify the returned bytes against the source container.
fn tiled(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let ingest_json = PathBuf::from(required(args, "--ingest-json")?);
    let out = PathBuf::from(required(args, "--out")?);
    let source_path = PathBuf::from(required(args, "--file")?);
    let checkpoint = value_of(args, "--checkpoint").unwrap_or_else(|| "base".to_owned());
    let dir_name = value_of(args, "--dir").unwrap_or_else(|| checkpoint.clone());
    let config = value_of(args, "--config").unwrap_or_else(|| "default".to_owned());
    let embedding =
        value_of(args, "--embedding").unwrap_or_else(|| "model.embed_tokens.weight".to_owned());
    let threads: usize = value_of(args, "--threads")
        .map(|text| {
            text.parse()
                .map_err(|_| "--threads must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(1);
    let max_inflight_mib: usize = value_of(args, "--max-inflight-mib")
        .map(|text| {
            text.parse()
                .map_err(|_| "--max-inflight-mib must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(64);
    let remove_materialized = args.iter().any(|arg| arg == "--remove-materialized");
    let out_file = value_of(args, "--out-file")
        .map(PathBuf::from)
        .unwrap_or_else(|| out.with_extension("safetensors"));

    let options = TileDecodeOptions {
        threads,
        max_inflight_bytes: max_inflight_mib.saturating_mul(1024 * 1024),
    };

    let ingest_text = fs::read_to_string(&ingest_json)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let ingest: Json = serde_json::from_str(&ingest_text)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let records = ingest
        .get("ingest")
        .and_then(|value| value.get(checkpoint.as_str()))
        .and_then(|value| value.get("tensors"))
        .and_then(Json::as_array)
        .ok_or_else(|| format!("ingest receipt has no {checkpoint} tensors"))?;
    let storage = json!({
        "physical": ingest.get("physical").cloned().unwrap_or(Json::Null),
        "strategy_distribution": ingest
            .get("strategy_distribution")
            .and_then(|value| value.get(checkpoint.as_str()))
            .cloned()
            .unwrap_or(Json::Null),
    });

    let source = parse_safetensors(&source_path)?;
    let source_sha256 = file_sha256(&source_path)?;
    let source_by_name: BTreeMap<&str, &TensorHeader> = source
        .tensors
        .iter()
        .map(|tensor| (tensor.name.as_str(), tensor))
        .collect();

    let open_start = Instant::now();
    let store = Store::open(&store_path, OpenMode::ReadOnly)
        .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let open_ms = open_start.elapsed().as_secs_f64() * 1000.0;

    let mut names = Vec::with_capacity(records.len());
    let mut ids = Vec::with_capacity(records.len());
    for record in records {
        let name = record
            .get("name")
            .and_then(Json::as_str)
            .ok_or_else(|| "ingest tensor record is missing name".to_owned())?;
        let id = store
            .resolve_components(&[dir_name.as_bytes(), name.as_bytes()])
            .map_err(|err| format!("resolve {dir_name}/{name}: {err}"))?;
        names.push(name.to_owned());
        ids.push(id);
    }

    // Pass 1: decode every tensor with the requested tile worker count.
    let before = store.backend_stats();
    let started = Instant::now();
    let mut hash_mismatches = Vec::new();
    let mut restored_bytes = 0u64;
    let mut read_stats = TileTotals::default();
    let mut strategy_count: BTreeMap<String, usize> = BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
        let (data, stats) = store
            .read_tensor_with_options_and_stats(ids[index], &options)
            .map_err(|err| format!("read {dir_name}/{}: {err}", names[index]))?;
        restored_bytes += data.len() as u64;
        read_stats.bytes += data.len() as u64;
        let hash = blake3::hash(&data).to_hex().to_string();
        if hash
            != record
                .get("blake3")
                .and_then(Json::as_str)
                .unwrap_or_default()
        {
            hash_mismatches.push(names[index].clone());
        }
        read_stats.merge(&stats);
        let strategy = record
            .get("strategy")
            .and_then(Json::as_str)
            .unwrap_or("unknown")
            .to_owned();
        *strategy_count.entry(strategy).or_default() += 1;
    }
    let read_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = store.backend_stats();

    // Pass 2: every stored logical root must recompute to the recorded value.
    let roots_started = Instant::now();
    let mut root_mismatches = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let root = store
            .tensor_logical_root(ids[index])
            .map_err(|err| format!("logical root {dir_name}/{}: {err}", names[index]))?;
        if root.to_hex()
            != record
                .get("logical_root")
                .and_then(Json::as_str)
                .unwrap_or_default()
        {
            root_mismatches.push(names[index].clone());
        }
    }
    let root_ms = roots_started.elapsed().as_secs_f64() * 1000.0;

    // Pass 3: reassemble the exact source container byte-for-byte.
    let header_id = store
        .resolve_components(&[dir_name.as_bytes(), HEADER_OBJECT.as_bytes()])
        .map_err(|err| format!("resolve {dir_name}/{HEADER_OBJECT}: {err}"))?;
    let header = store
        .read_object(header_id)
        .map_err(|err| format!("read {dir_name}/{HEADER_OBJECT}: {err}"))?;
    let materialize_started = Instant::now();
    let mut materialize_stats = TileTotals::default();
    {
        let mut file =
            File::create(&out_file).map_err(|err| format!("{}: {err}", out_file.display()))?;
        file.write_all(&header)
            .map_err(|err| format!("{}: {err}", out_file.display()))?;
        for id in &ids {
            let (data, stats) = store
                .read_tensor_with_options_and_stats(*id, &options)
                .map_err(|err| format!("materialize read: {err}"))?;
            materialize_stats.merge(&stats);
            materialize_stats.bytes += data.len() as u64;
            file.write_all(&data)
                .map_err(|err| format!("{}: {err}", out_file.display()))?;
        }
        file.flush()
            .map_err(|err| format!("{}: {err}", out_file.display()))?;
    }
    let materialize_ms = materialize_started.elapsed().as_secs_f64() * 1000.0;
    let reassembled_sha256 = file_sha256(&out_file)?;
    let materialized_bytes = fs::metadata(&out_file).map(|m| m.len()).unwrap_or(0);
    let materialize_exact = reassembled_sha256 == source_sha256;
    if remove_materialized {
        let _ = fs::remove_file(&out_file);
    }

    // Region reads of the embedding at this worker count.
    let embedding_header = source_by_name
        .get(embedding.as_str())
        .ok_or_else(|| format!("source container has no tensor {embedding}"))?;
    if embedding_header.shape.len() != 2 {
        return Err(format!(
            "{embedding} is rank {}, region reads need rank 2",
            embedding_header.shape.len()
        ));
    }
    let embedding_id = store
        .resolve_components(&[dir_name.as_bytes(), embedding.as_bytes()])
        .map_err(|err| format!("resolve {dir_name}/{embedding}: {err}"))?;
    let embedding_bytes = read_tensor_bytes(&source, embedding_header)?;
    let rows = embedding_header.shape[0];
    let cols = embedding_header.shape[1];
    let element = element_bytes(&embedding_header.dtype) as usize;
    let regions = [
        ("whole", vec![0, 0], vec![rows, cols]),
        ("rows_100", vec![0, 0], vec![rows.min(100), cols]),
        ("row_1", vec![0, 0], vec![rows.min(1), cols]),
    ];
    let mut region_rows = Vec::new();
    for (label, start, end) in regions {
        let region = weftcodec::Region::new(start, end);
        let expected = region_bytes(&embedding_bytes, &embedding_header.shape, &region, element);
        let region_before = store.backend_stats();
        let region_started = Instant::now();
        let (read, stats) = store
            .read_tensor_region_with_options_and_stats(embedding_id, &region, &options)
            .map_err(|err| format!("region {label}: {err}"))?;
        let ms = region_started.elapsed().as_secs_f64() * 1000.0;
        let region_after = store.backend_stats();
        region_rows.push(json!({
            "label": label,
            "shape": read.shape,
            "bytes": read.bytes.len(),
            "ms": round(ms),
            "exact": read.bytes == expected,
            "tiles_touched": read.units_touched,
            "tile_stats": tile_stats_json(&stats),
            "backend_bytes_read": region_after.bytes_read.saturating_sub(region_before.bytes_read),
            "backend_range_reads": region_after.range_reads.saturating_sub(region_before.range_reads),
        }));
    }

    let receipt = json!({
        "schema": "weft.tiled-parallel.v1",
        "phase": "tiled",
        "config": config,
        "checkpoint": checkpoint,
        "threads": threads,
        "effective_threads": weftfs::effective_threads(threads),
        "max_inflight_bytes": options.max_inflight_bytes,
        "open_ms": round(open_ms),
        "store": store_path.display().to_string(),
        "source": {
            "path": source_path.display().to_string(),
            "sha256": source_sha256.clone(),
            "tensor_count": source.tensors.len(),
        },
        "storage": storage,
        "restore": {
            "tensors": records.len(),
            "read_ms": round(read_ms),
            "restored_bytes": restored_bytes,
            "hash_exact": hash_mismatches.is_empty(),
            "hash_mismatches": hash_mismatches,
            "logical_root_check_ms": round(root_ms),
            "logical_roots_exact": root_mismatches.is_empty(),
            "logical_root_mismatches": root_mismatches,
            "tile_stats": read_stats.to_json(&options),
            "observed_strategy_count": strategy_count,
            "backend": {
                "bytes_read": after.bytes_read.saturating_sub(before.bytes_read),
                "reads": after.reads.saturating_sub(before.reads),
                "range_reads": after.range_reads.saturating_sub(before.range_reads),
            },
        },
        "materialize": {
            "path": out_file.display().to_string(),
            "ms": round(materialize_ms),
            "bytes": materialized_bytes,
            "sha256": reassembled_sha256,
            "source_sha256": source_sha256,
            "exact": materialize_exact,
            "tile_stats": materialize_stats.to_json(&options),
            "removed": remove_materialized,
        },
        "region_reads": {
            "tensor": embedding,
            "rows": region_rows,
        },
        "peak_rss_kib": peak_rss_kib(),
    });
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}

fn expected_tensor_bytes(ingest: &Json, checkpoint: &str, name: &str) -> u64 {
    ingest
        .get("ingest")
        .and_then(|value| value.get(checkpoint))
        .and_then(|value| value.get("tensors"))
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .find(|record| record.get("name").and_then(Json::as_str) == Some(name))
        .and_then(|record| record.get("bytes"))
        .and_then(Json::as_u64)
        .unwrap_or(0)
}

/// Aggregate of `ParallelReadStats` over the batches of one pass.
#[derive(Default)]
struct ReadStatsTotal {
    batches: usize,
    tasks: usize,
    observed_max_inflight_bytes: usize,
    observed_max_inflight_tasks: usize,
    oversized_tasks: usize,
    effective_threads: usize,
}

impl ReadStatsTotal {
    fn merge(&mut self, stats: &ParallelReadStats) {
        self.batches += 1;
        self.tasks += stats.tasks;
        self.effective_threads = stats.effective_threads;
        self.observed_max_inflight_bytes = self
            .observed_max_inflight_bytes
            .max(stats.observed_max_inflight_bytes);
        self.observed_max_inflight_tasks = self
            .observed_max_inflight_tasks
            .max(stats.observed_max_inflight_tasks);
        self.oversized_tasks += stats.oversized_tasks;
    }

    fn to_json(&self, options: &ParallelReadOptions, batch_bytes: u64) -> Json {
        json!({
            "threads_requested": options.threads,
            "threads_effective": weftfs::parallel::effective_threads(options.threads),
            "max_inflight_bytes": options.max_inflight_bytes,
            "batch_bytes": batch_bytes,
            "batches": self.batches,
            "tasks": self.tasks,
            "observed_max_inflight_bytes": self.observed_max_inflight_bytes,
            "observed_max_inflight_tasks": self.observed_max_inflight_tasks,
            "oversized_tasks": self.oversized_tasks,
        })
    }
}

/// JSON view of one ordered restore pipeline.
fn pipeline_stats_json(stats: &RestorePipelineStats) -> Json {
    json!({
        "threads_requested": stats.requested_threads,
        "threads_effective": stats.effective_threads,
        "tasks": stats.tasks,
        "tensors_written": stats.tensors_written,
        "bytes_written": stats.bytes_written,
        "tensors_dropped": stats.tensors_dropped,
        "bytes_dropped": stats.bytes_dropped,
        "max_inflight_bytes": stats.max_inflight_bytes,
        "observed_max_inflight_bytes": stats.observed_max_inflight_bytes,
        "observed_max_inflight_tasks": stats.observed_max_inflight_tasks,
        "oversized_tasks": stats.oversized_tasks,
        "decode_wall_ms": round(stats.decode_wall_ms),
        "decode_sum_ms": round(stats.decode_sum_ms),
        "transfer_ms": round(stats.transfer_ms),
        "write_ms": round(stats.write_ms),
        "total_ms": round(stats.total_ms),
    })
}

/// Split `sizes` into contiguous batches whose declared bytes fit `budget`.
/// A single tensor larger than the budget gets a batch of its own, so the
/// reassembly buffer never holds more than one oversized tensor plus the bound.
fn plan_batches(sizes: &[u64], budget: u64) -> Vec<(usize, usize)> {
    let mut batches = Vec::new();
    let mut start = 0usize;
    while start < sizes.len() {
        let mut end = start;
        let mut accumulated = 0u64;
        while end < sizes.len() {
            let next = sizes[end];
            if end > start && accumulated.saturating_add(next) > budget {
                break;
            }
            accumulated = accumulated.saturating_add(next);
            end += 1;
            if accumulated >= budget {
                break;
            }
        }
        batches.push((start, end));
        start = end;
    }
    batches
}

fn restore_checkpoint(
    store: &Store,
    ingest: &Json,
    checkpoint: &str,
    dir_name: &str,
    reassemble_dir: Option<&Path>,
    options: &ParallelReadOptions,
    materialize_pipeline: bool,
) -> Result<Json, String> {
    let records = ingest
        .get("ingest")
        .and_then(|value| value.get(checkpoint))
        .and_then(|value| value.get("tensors"))
        .and_then(Json::as_array)
        .ok_or_else(|| format!("ingest receipt has no {checkpoint} tensors"))?;

    // Resolve every tensor once. Sizes come from the ingest receipt (the same
    // logical sizes the store recorded) so batch planning needs no I/O.
    let mut names: Vec<String> = Vec::with_capacity(records.len());
    let mut ids: Vec<weftfs::identity::Hash32> = Vec::with_capacity(records.len());
    let mut sizes: Vec<u64> = Vec::with_capacity(records.len());
    for record in records {
        let name = record
            .get("name")
            .and_then(Json::as_str)
            .ok_or_else(|| "ingest tensor record is missing name".to_owned())?;
        let id = store
            .resolve_components(&[dir_name.as_bytes(), name.as_bytes()])
            .map_err(|err| format!("resolve {dir_name}/{name}: {err}"))?;
        names.push(name.to_owned());
        ids.push(id);
        sizes.push(record.get("bytes").and_then(Json::as_u64).unwrap_or(0));
    }
    let largest = sizes.iter().copied().max().unwrap_or(0);
    let batch_bytes = (options.max_inflight_bytes as u64).max(largest).max(1);
    let batches = plan_batches(&sizes, batch_bytes);

    // Timed pass: read and hash every tensor, compare against the source hash
    // recorded during ingest. Reads are bounded-parallel; hashing and ordering
    // stay in the caller's thread and in input order.
    let before = store.backend_stats();
    let started = Instant::now();
    let mut mismatches = Vec::new();
    let mut root_inputs = Vec::new();
    let mut restored_bytes = 0u64;
    let mut read_stats = ReadStatsTotal::default();
    let tokens = ids
        .iter()
        .map(|id| {
            store
                .authorize_read(*id, &weftfs::Principal::root(), store.clock_now_ns())
                .map_err(|err| format!("authorize tensor: {err}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (start, end) in &batches {
        let batch_ids = &ids[*start..*end];
        let (datas, stats) = store
            .read_tensors_bounded_authorized(&tokens[*start..*end], options)
            .map_err(|err| format!("read {dir_name} batch {start}..{end}: {err}"))?;
        read_stats.merge(&stats);
        if datas.len() != batch_ids.len() {
            return Err(format!(
                "read {dir_name} batch {start}..{end}: {} results for {} tensors",
                datas.len(),
                batch_ids.len()
            ));
        }
        for (offset, data) in datas.into_iter().enumerate() {
            let index = start + offset;
            let name = &names[index];
            restored_bytes += data.len() as u64;
            let hash = blake3::hash(&data).to_hex().to_string();
            let expected = records
                .get(index)
                .and_then(|record| record.get("blake3"))
                .and_then(Json::as_str)
                .unwrap_or_default();
            if hash != expected {
                mismatches.push(name.clone());
            }
            root_inputs.push((name.clone(), ids[index]));
        }
    }
    let read_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = store.backend_stats();

    // Logical roots: every stored tensor must recompute to the root recorded at
    // write time.
    let root_started = Instant::now();
    let mut root_mismatches = Vec::new();
    for (name, id) in &root_inputs {
        let root = store
            .tensor_logical_root(*id)
            .map_err(|err| format!("logical root {dir_name}/{name}: {err}"))?;
        let expected = records
            .iter()
            .find(|record| record.get("name").and_then(Json::as_str) == Some(name.as_str()))
            .and_then(|record| record.get("logical_root"))
            .and_then(Json::as_str)
            .unwrap_or_default();
        if root.to_hex() != expected {
            root_mismatches.push(name.clone());
        }
    }
    let root_ms = root_started.elapsed().as_secs_f64() * 1000.0;

    // Materialise the exact original safetensors file: exact header bytes then
    // each tensor in data_offsets order.
    let mut materialize = Json::Null;
    if let Some(dir) = reassemble_dir {
        fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        let header_id = store
            .resolve_components(&[dir_name.as_bytes(), HEADER_OBJECT.as_bytes()])
            .map_err(|err| format!("resolve {dir_name}/{HEADER_OBJECT}: {err}"))?;
        let header = store
            .read_object(header_id)
            .map_err(|err| format!("read {dir_name}/{HEADER_OBJECT}: {err}"))?;
        let target = dir.join(format!("{checkpoint}.safetensors"));
        let started = Instant::now();
        let mut materialize_stats = ReadStatsTotal::default();
        let mut file =
            File::create(&target).map_err(|err| format!("{}: {err}", target.display()))?;
        file.write_all(&header)
            .map_err(|err| format!("{}: {err}", target.display()))?;
        let pipeline_stats = if materialize_pipeline {
            let pipeline_options = RestorePipelineOptions {
                threads: options.threads,
                max_inflight_bytes: options.max_inflight_bytes,
            };
            Some(
                store
                    .restore_tensors_pipelined_authorized(&tokens, &mut file, &pipeline_options)
                    .map_err(|err| format!("pipeline restore {dir_name}: {err}"))?,
            )
        } else {
            None
        };
        if pipeline_stats.is_none() {
            // Same batching, same bytes, same order: the reassembly buffer never
            // holds more than the staging bound plus one oversized tensor.
            for (start, end) in &batches {
                let (datas, stats) = store
                    .read_tensors_bounded_authorized(&tokens[*start..*end], options)
                    .map_err(|err| format!("read {dir_name} batch {start}..{end}: {err}"))?;
                materialize_stats.merge(&stats);
                if datas.len() != end - start {
                    return Err(format!(
                        "read {dir_name} batch {start}..{end}: {} results for {} tensors",
                        datas.len(),
                        end - start
                    ));
                }
                for data in datas {
                    file.write_all(&data)
                        .map_err(|err| format!("{}: {err}", target.display()))?;
                }
            }
        }
        file.flush()
            .map_err(|err| format!("{}: {err}", target.display()))?;
        drop(file);
        let materialize_ms = started.elapsed().as_secs_f64() * 1000.0;
        let reassembled_blake3 = file_blake3(&target)?;
        let source_blake3 = ingest
            .get("corpus")
            .and_then(|value| value.get(checkpoint))
            .and_then(|value| value.get("blake3"))
            .and_then(Json::as_str)
            .unwrap_or_default();
        materialize = json!({
            "path": target.display().to_string(),
            "materialize_ms": round(materialize_ms),
            "bytes": fs::metadata(&target).map(|m| m.len()).unwrap_or(0),
            "reassembled_blake3": reassembled_blake3,
            "source_blake3": source_blake3,
            "exact": reassembled_blake3 == source_blake3,
            "mode": if pipeline_stats.is_some() { "pipeline" } else { "batch" },
            "pipeline": pipeline_stats
                .as_ref()
                .map(pipeline_stats_json)
                .unwrap_or(Json::Null),
            "parallel": if pipeline_stats.is_some() {
                Json::Null
            } else {
                materialize_stats.to_json(options, batch_bytes)
            },
        });
    }

    Ok(json!({
        "checkpoint": checkpoint,
        "tensors_read": root_inputs.len(),
        "read_ms": round(read_ms),
        "parallel_read": read_stats.to_json(options, batch_bytes),
        "restored_bytes": restored_bytes,
        "backend": {
            "bytes_read": after.bytes_read.saturating_sub(before.bytes_read),
            "reads": after.reads.saturating_sub(before.reads),
            "range_reads": after.range_reads.saturating_sub(before.range_reads),
        },
        "hash_mismatches": mismatches,
        "hash_exact": mismatches.is_empty(),
        "logical_root_check_ms": round(root_ms),
        "logical_root_mismatches": root_mismatches,
        "logical_roots_exact": root_mismatches.is_empty(),
        "materialize": materialize,
    }))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// hot: read every tensor of one checkpoint N times so the advisory access
// counters make the rebase dry run meaningful.
// ---------------------------------------------------------------------------

fn hot(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let ingest_json = PathBuf::from(required(args, "--ingest-json")?);
    let out = PathBuf::from(required(args, "--out")?);
    let checkpoint = value_of(args, "--checkpoint").unwrap_or_else(|| "instruct".to_owned());
    let reads: u64 = value_of(args, "--reads")
        .map(|text| {
            text.parse()
                .map_err(|_| "--reads must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(10);
    // Optional contiguous-stride subset, so one checkpoint can be split into
    // "hot" and "warm" tensor sets without changing the store.
    let start: usize = value_of(args, "--start")
        .map(|text| {
            text.parse()
                .map_err(|_| "--start must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(0);
    let stride: usize = value_of(args, "--stride")
        .map(|text| {
            text.parse()
                .map_err(|_| "--stride must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(1);
    if stride == 0 {
        return Err("--stride must be at least 1".to_owned());
    }
    let limit: usize = value_of(args, "--limit")
        .map(|text| {
            text.parse()
                .map_err(|_| "--limit must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(usize::MAX);
    let mut named: Vec<String> = Vec::new();
    if let Some(text) = value_of(args, "--tensors") {
        named.extend(
            text.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        );
    }
    if let Some(path) = value_of(args, "--tensors-file") {
        let text = fs::read_to_string(&path).map_err(|err| format!("{path}: {err}"))?;
        named.extend(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        );
    }
    let ingest_text = fs::read_to_string(&ingest_json)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let ingest: Json = serde_json::from_str(&ingest_text)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let records = ingest
        .get("ingest")
        .and_then(|value| value.get(checkpoint.as_str()))
        .and_then(|value| value.get("tensors"))
        .and_then(Json::as_array)
        .ok_or_else(|| format!("ingest receipt has no {checkpoint} tensors"))?;
    let store =
        Store::open_with_options(&store_path, OpenMode::ReadWrite, read_layer_options(args)?)
            .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut total_bytes = 0u64;
    let selected: Vec<(usize, &Json)> = if named.is_empty() {
        records
            .iter()
            .enumerate()
            .filter(|(index, _)| *index >= start && (*index - start) % stride == 0)
            .take(limit)
            .collect()
    } else {
        let by_name: BTreeMap<&str, (usize, &Json)> = records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                (
                    record.get("name").and_then(Json::as_str).unwrap_or(""),
                    (index, record),
                )
            })
            .collect();
        named
            .iter()
            .filter_map(|name| by_name.get(name.as_str()).copied())
            .take(limit)
            .collect()
    };
    for (index, record) in &selected {
        let name = record
            .get("name")
            .and_then(Json::as_str)
            .ok_or_else(|| "ingest tensor record is missing name".to_owned())?;
        let id = store
            .resolve_components(&[checkpoint.as_bytes(), name.as_bytes()])
            .map_err(|err| format!("resolve {checkpoint}/{name}: {err}"))?;
        let token = store
            .authorize_read(id, &weftfs::Principal::root(), store.clock_now_ns())
            .map_err(|err| format!("authorize {checkpoint}/{name}: {err}"))?;
        for _ in 0..reads {
            let data = store
                .read_tensor_authorized(&token, &weftfs::TileDecodeOptions::default())
                .map_err(|err| format!("read {checkpoint}/{name}: {err}"))?;
            total_bytes += data.len() as u64;
        }
        rows.push(json!({
            "index": index,
            "name": name,
            "object_id": id.to_hex(),
            "access_reads": store.access_reads(id),
            "strategy": store.tensor_metadata(id).map(|m| m.strategy).unwrap_or_else(|_| "unknown".to_owned()),
        }));
    }
    store
        .flush_access_counts()
        .map_err(|err| format!("flush access counters: {err}"))?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    drop(store);
    let receipt = json!({
        "schema": "weft.real-model.hot.v1",
        "phase": "hot",
        "checkpoint": checkpoint,
        "reads_per_tensor": reads,
        "selection": {"start": start, "stride": stride, "limit": if limit == usize::MAX { Json::Null } else { json!(limit) }, "named": named},
        "tensors": rows,
        "logical_bytes_read": total_bytes,
        "wall_ms": round(wall_ms),
        "peak_rss_kib": peak_rss_kib(),
    });
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// stats: per-tensor identity, strategy, physical bytes, and snapshot view.
// ---------------------------------------------------------------------------

fn stats(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let ingest_json = PathBuf::from(required(args, "--ingest-json")?);
    let out = PathBuf::from(required(args, "--out")?);
    let checkpoint = value_of(args, "--checkpoint").unwrap_or_else(|| "instruct".to_owned());
    let snapshot = value_of(args, "--snapshot");
    let hash = args.iter().any(|arg| arg == "--sha256");
    let ingest_text = fs::read_to_string(&ingest_json)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let ingest: Json = serde_json::from_str(&ingest_text)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let records = ingest
        .get("ingest")
        .and_then(|value| value.get(checkpoint.as_str()))
        .and_then(|value| value.get("tensors"))
        .and_then(Json::as_array)
        .ok_or_else(|| format!("ingest receipt has no {checkpoint} tensors"))?;
    let store = Store::open(&store_path, OpenMode::ReadOnly)
        .map_err(|err| format!("open {}: {err}", store_path.display()))?;

    let snapshot_view = match &snapshot {
        Some(name) => {
            let commit = store
                .resolve_snapshot_ref(name)
                .map_err(|err| format!("snapshot {name}: {err}"))?;
            let state = store
                .state_at(commit)
                .map_err(|err| format!("state at {name}: {err}"))?;
            Some((name.clone(), commit, state))
        }
        None => None,
    };

    let mut rows = Vec::new();
    for record in records {
        let name = record
            .get("name")
            .and_then(Json::as_str)
            .ok_or_else(|| "ingest tensor record is missing name".to_owned())?;
        let id = store
            .resolve_components(&[checkpoint.as_bytes(), name.as_bytes()])
            .map_err(|err| format!("resolve {checkpoint}/{name}: {err}"))?;
        let content = match &snapshot_view {
            Some((_, _, state)) => state
                .objects
                .get(&id)
                .map(|record| record.content)
                .ok_or_else(|| format!("{checkpoint}/{name} is absent from the snapshot"))?,
            None => {
                store
                    .object_meta(id)
                    .map_err(|err| format!("record {checkpoint}/{name}: {err}"))?
                    .content
            }
        };
        let manifest = store
            .manifest_of(content)
            .map_err(|err| format!("manifest {checkpoint}/{name}: {err}"))?;
        let physical_bytes = representation_bytes(&store_path, &manifest)?;
        let meta = match &snapshot_view {
            Some((_, _, state)) => store.tensor_metadata_at(state, id),
            None => store.tensor_metadata(id),
        }
        .map_err(|err| format!("metadata {checkpoint}/{name}: {err}"))?;
        let mut row = json!({
            "name": name,
            "object_id": id.to_hex(),
            "strategy": meta.strategy,
            "dtype": meta.dtype.as_str(),
            "logical_bytes": manifest.size,
            "physical_bytes": physical_bytes,
            "chunk_count": manifest.chunks.len(),
            "logical_root": meta.logical_root.to_hex(),
            "access_reads": if snapshot_view.is_some() { 0 } else { store.access_reads(id) },
        });
        if hash {
            let data = store
                .read_object_at(snapshot_view.as_ref().map(|(_, commit, _)| *commit), id)
                .map_err(|err| format!("read {checkpoint}/{name}: {err}"))?;
            row["sha256"] = json!(hex::encode(Sha256::digest(&data)));
        }
        rows.push(row);
    }

    let snapshots: BTreeMap<String, String> = store
        .snapshot_list()
        .into_iter()
        .map(|(name, commit)| (name, commit.to_hex()))
        .collect();
    let retained = store
        .retained_blob_bytes()
        .map_err(|err| format!("retained bytes: {err}"))?;
    let breakdown = json!({
        "objects": tree_bytes(&store_path.join("objects"))?,
        "manifests": tree_bytes(&store_path.join("manifests"))?,
        "indexes": tree_bytes(&store_path.join("indexes"))?,
        "commits": tree_bytes(&store_path.join("commits"))?,
        "access": tree_bytes(&store_path.join("access"))?,
        "refs": tree_bytes(&store_path.join("refs"))?,
    });
    let receipt = json!({
        "schema": "weft.real-model.stats.v1",
        "phase": "stats",
        "checkpoint": checkpoint,
        "view": snapshot_view.as_ref().map(|(name, _, _)| format!("snapshot:{name}")).unwrap_or_else(|| "live".to_owned()),
        "snapshot": snapshot_view.as_ref().map(|(name, commit, _)| json!({"name": name, "commit": commit.to_hex()})),
        "generation": store.generation(),
        "head": store.head().to_hex(),
        "snapshots": snapshots,
        "tree_bytes": tree_bytes(&store_path)?,
        "retained_blob_bytes": retained,
        "breakdown": breakdown,
        "tensors": rows,
    });
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// timing: median decoded read time for a named tensor list.
// ---------------------------------------------------------------------------

fn timing(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let ingest_json = PathBuf::from(required(args, "--ingest-json")?);
    let out = PathBuf::from(required(args, "--out")?);
    let checkpoint = value_of(args, "--checkpoint").unwrap_or_else(|| "instruct".to_owned());
    let reads: usize = value_of(args, "--reads")
        .map(|text| {
            text.parse()
                .map_err(|_| "--reads must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(3);
    if reads == 0 {
        return Err("--reads must be at least 1".to_owned());
    }
    let mut names: Vec<String> = Vec::new();
    if let Some(text) = value_of(args, "--tensors") {
        names.extend(
            text.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        );
    }
    if let Some(path) = value_of(args, "--tensors-file") {
        let text = fs::read_to_string(&path).map_err(|err| format!("{path}: {err}"))?;
        names.extend(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        );
    }
    let ingest_text = fs::read_to_string(&ingest_json)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let ingest: Json = serde_json::from_str(&ingest_text)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    if names.is_empty() {
        let records = ingest
            .get("ingest")
            .and_then(|value| value.get(checkpoint.as_str()))
            .and_then(|value| value.get("tensors"))
            .and_then(Json::as_array)
            .ok_or_else(|| format!("ingest receipt has no {checkpoint} tensors"))?;
        for record in records {
            if let Some(name) = record.get("name").and_then(Json::as_str) {
                names.push(name.to_owned());
            }
        }
    }

    let store = Store::open(&store_path, OpenMode::ReadOnly)
        .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let mut rows = Vec::new();
    for name in names {
        let id = store
            .resolve_components(&[checkpoint.as_bytes(), name.as_bytes()])
            .map_err(|err| format!("resolve {checkpoint}/{name}: {err}"))?;
        let expected = expected_tensor_bytes(&ingest, &checkpoint, &name);
        let token = store
            .authorize_read(id, &weftfs::Principal::root(), store.clock_now_ns())
            .map_err(|err| format!("authorize {checkpoint}/{name}: {err}"))?;
        // Warm-up read outside the timed runs.
        store
            .read_tensor_authorized(&token, &weftfs::TileDecodeOptions::default())
            .map_err(|err| format!("warmup {checkpoint}/{name}: {err}"))?;
        let mut runs = Vec::new();
        let mut backend_bytes = 0u64;
        let mut exact = true;
        for _ in 0..reads {
            let before = store.backend_stats();
            let started = Instant::now();
            let data = store
                .read_tensor_authorized(&token, &weftfs::TileDecodeOptions::default())
                .map_err(|err| format!("read {checkpoint}/{name}: {err}"))?;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let after = store.backend_stats();
            backend_bytes = after.bytes_read.saturating_sub(before.bytes_read);
            exact &= data.len() as u64 == expected;
            runs.push(json!({
                "ms": round(elapsed_ms),
                "backend_bytes_read": after.bytes_read.saturating_sub(before.bytes_read),
                "backend_reads": after.reads.saturating_sub(before.reads),
            }));
        }
        let mut timings: Vec<f64> = runs
            .iter()
            .filter_map(|run| run.get("ms").and_then(Json::as_f64))
            .collect();
        timings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let record = store
            .object_meta(id)
            .map_err(|err| format!("meta {name}: {err}"))?;
        let meta = store
            .tensor_metadata(id)
            .map_err(|err| format!("tensor meta {name}: {err}"))?;
        rows.push(json!({
            "name": name,
            "object_id": id.to_hex(),
            "strategy": meta.strategy,
            "logical_bytes": record.size,
            "size_matches_ingest": exact,
            "access_reads": store.access_reads(id),
            "runs": runs,
            "median_ms": round(timings.get(timings.len() / 2).copied().unwrap_or(0.0)),
            "min_ms": round(timings.first().copied().unwrap_or(0.0)),
            "max_ms": round(timings.last().copied().unwrap_or(0.0)),
            "backend_bytes_read": backend_bytes,
        }));
    }
    let receipt = json!({
        "schema": "weft.real-model.timing.v1",
        "phase": "timing",
        "checkpoint": checkpoint,
        "reads_per_tensor": reads,
        "tensors": rows,
        "peak_rss_kib": peak_rss_kib(),
    });
    fs::write(
        &out,
        serde_json::to_vec_pretty(&receipt).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}

fn representation_bytes(
    store_path: &Path,
    manifest: &weftfs::format::Manifest,
) -> Result<u64, String> {
    if let Some(rep) = &manifest.representation {
        if let Some(frame) = rep.frame {
            // The frame blob lives in the same `manifests/` namespace as the
            // manifest. A local store can stat it directly; any other backend
            // falls back to a verified read (correct, but it materialises the
            // frame).
            let path = weftfs::recovery::manifest_path(store_path, frame);
            if let Ok(metadata) = fs::metadata(&path) {
                return Ok(metadata.len());
            }
            let bytes = weftfs::recovery::read_blob_verified(
                store_path,
                weftfs::recovery::BlobKind::Manifest,
                frame,
                weftfs::identity::MANIFEST_DOMAIN,
            )
            .map_err(|err| format!("frame blob {frame}: {err}"))?;
            return Ok(bytes.len() as u64);
        }
    }
    Ok(manifest.chunks.iter().map(|chunk| chunk.len).sum())
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
