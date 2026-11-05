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
//!   byte-for-byte, and measures a single-tensor restore.
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
use weftfs::backend::BackendStats;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorWriteSpec};

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
        other => Err(format!(
            "unknown phase `{other}` (expected `ingest` or `restore`)"
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
        let spec = TensorWriteSpec::default();
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
            plan: TensorPlanOptions::default(),
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
                "planner": "TensorPlanOptions::default() (lambda_ns = 0, bytes only)",
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
    let single_runs: usize = value_of(args, "--single-runs")
        .map(|text| {
            text.parse()
                .map_err(|_| "--single-runs must be an integer".to_owned())
        })
        .transpose()?
        .unwrap_or(5);

    let ingest_text = fs::read_to_string(&ingest_json)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;
    let ingest: Json = serde_json::from_str(&ingest_text)
        .map_err(|err| format!("{}: {err}", ingest_json.display()))?;

    let open_start = Instant::now();
    let store = Store::open(&store_path, OpenMode::ReadOnly)
        .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let open_ms = open_start.elapsed().as_secs_f64() * 1000.0;
    let open_stats = store.backend_stats();

    let mut checkpoints = Vec::new();
    for (name, dir_name) in [("base", "base"), ("instruct", "instruct")] {
        let result =
            restore_checkpoint(&store, &ingest, name, dir_name, reassemble_dir.as_deref())?;
        checkpoints.push(result);
    }

    let mut single = Vec::new();
    for (checkpoint, dir_name) in [("base", "base"), ("instruct", "instruct")] {
        let id = store
            .resolve_components(&[dir_name.as_bytes(), single_tensor.as_bytes()])
            .map_err(|err| format!("resolve {dir_name}/{single_tensor}: {err}"))?;
        let mut runs = Vec::new();
        let mut verified = false;
        let mut bytes = 0u64;
        for _ in 0..single_runs {
            let before = store.backend_stats();
            let started = Instant::now();
            let data = store
                .read_tensor(id)
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

fn restore_checkpoint(
    store: &Store,
    ingest: &Json,
    checkpoint: &str,
    dir_name: &str,
    reassemble_dir: Option<&Path>,
) -> Result<Json, String> {
    let records = ingest
        .get("ingest")
        .and_then(|value| value.get(checkpoint))
        .and_then(|value| value.get("tensors"))
        .and_then(Json::as_array)
        .ok_or_else(|| format!("ingest receipt has no {checkpoint} tensors"))?;

    // Timed pass: read and hash every tensor, compare against the source hash
    // recorded during ingest.
    let before = store.backend_stats();
    let started = Instant::now();
    let mut mismatches = Vec::new();
    let mut root_inputs = Vec::new();
    let mut restored_bytes = 0u64;
    for record in records {
        let name = record
            .get("name")
            .and_then(Json::as_str)
            .ok_or_else(|| "ingest tensor record is missing name".to_owned())?;
        let id = store
            .resolve_components(&[dir_name.as_bytes(), name.as_bytes()])
            .map_err(|err| format!("resolve {dir_name}/{name}: {err}"))?;
        let data = store
            .read_tensor(id)
            .map_err(|err| format!("read {dir_name}/{name}: {err}"))?;
        restored_bytes += data.len() as u64;
        let hash = blake3::hash(&data).to_hex().to_string();
        let expected = record
            .get("blake3")
            .and_then(Json::as_str)
            .unwrap_or_default();
        if hash != expected {
            mismatches.push(name.to_owned());
        }
        root_inputs.push((name.to_owned(), id));
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
        let mut file =
            File::create(&target).map_err(|err| format!("{}: {err}", target.display()))?;
        file.write_all(&header)
            .map_err(|err| format!("{}: {err}", target.display()))?;
        for record in records {
            let name = record
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let id = store
                .resolve_components(&[dir_name.as_bytes(), name.as_bytes()])
                .map_err(|err| format!("resolve {dir_name}/{name}: {err}"))?;
            let data = store
                .read_tensor(id)
                .map_err(|err| format!("read {dir_name}/{name}: {err}"))?;
            file.write_all(&data)
                .map_err(|err| format!("{}: {err}", target.display()))?;
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
        });
    }

    Ok(json!({
        "checkpoint": checkpoint,
        "tensors_read": root_inputs.len(),
        "read_ms": round(read_ms),
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
