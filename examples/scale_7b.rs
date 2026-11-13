//! 7B-scale court harness (subphase R12, increment 1; X04).
//!
//! Phases, each run as its own process so peak RSS is honest:
//!
//! * `ingest`     — write every tensor of one safetensors container through the
//!                  per-tensor planner (`write_tensor_file_with_spec[_seeded]`),
//!                  or, with `--stream`, through the bounded source writer
//!                  (`write_tensor_file_from_source`). With a deterministic
//!                  seed (`--seeded`/default) both modes must publish the same
//!                  head, which is the 7B identity proof.
//! * `checkpoint` — the bounded / streaming checkpoint writer
//!                  (`Store::write_checkpoint`, `--max-ingest-bytes`), N-way
//!                  sharded, flat or packed, planned or fast-raw.
//! * `restore`    — `Store::restore_checkpoint_pipelined` at 1/4/8 workers into
//!                  a SHA-256 sink; the container digest must equal the source
//!                  file's SHA-256.
//! * `verify`     — `Store::verify(true)` plus the retained byte count.
//! * `catalog`    — pack-catalog accounting: entries, segment blobs on disk,
//!                  bytes per namespace, and the amplification ratio against the
//!                  logical data.
//!
//! Nothing here is a model framework: the safetensors header is parsed directly
//! and the bytes go through the shipped store API. Every number is printed as
//! JSON; the court wrapper adds the environment and assembles the receipt.
//! With `WEFT_PHASE_PROFILE=1` each phase also reports the per-stage profile
//! (encode, publish, digest, index, commit), so a memory or wall finding can be
//! attributed to a stage.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

use weftfs::backend::BackendKind;
use weftfs::checkpoint::{CheckpointRestoreOptions, CheckpointWriteOptions, ShardStrategy};
use weftfs::restore_pipeline::RestorePipelineOptions;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{
    FastCodec, StreamedWriteOptions, TensorLayoutSpec, TensorPlanOptions, TensorWriteSpec,
    WritePolicy,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("scale_7b: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let phase = args.first().cloned().unwrap_or_default();
    match phase.as_str() {
        "ingest" => ingest(&args[1..]),
        "checkpoint" => checkpoint(&args[1..]),
        "restore" => restore(&args[1..]),
        "verify" => verify(&args[1..]),
        "catalog" => catalog(&args[1..]),
        other => Err(format!(
            "unknown phase `{other}` (expected ingest, checkpoint, restore, verify, catalog)"
        )),
    }
}

/// Begin phase profiling when the court asked for it.
fn profile_begin() {
    if std::env::var_os("WEFT_PHASE_PROFILE").is_some() {
        weftfs::phase_profile::begin();
    }
}

/// Per-stage totals for the report, or `null` when profiling is off.
fn profile_json() -> Json {
    if std::env::var_os("WEFT_PHASE_PROFILE").is_some() {
        weftfs::phase_profile::snapshot().to_json()
    } else {
        Json::Null
    }
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn required(args: &[String], flag: &str) -> Result<String, String> {
    value_of(args, flag).ok_or_else(|| format!("missing {flag}"))
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn parse_u64(args: &[String], flag: &str, default: u64) -> Result<u64, String> {
    match value_of(args, flag) {
        None => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|_| format!("{flag} must be an integer (got `{text}`)")),
    }
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------------------
// Process facts
// ---------------------------------------------------------------------------

fn peak_rss_kib() -> u64 {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(kib) = rest.split_whitespace().next() {
                return kib.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn tree_bytes(path: &Path) -> u64 {
    fn walk(path: &Path, total: &mut u64) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&entry.path(), total);
            } else if meta.is_file() {
                *total = total.saturating_add(meta.len());
            }
        }
    }
    let mut total = 0;
    walk(path, &mut total);
    total
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 8 << 20];
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

/// SHA-256 of one tensor's bytes, read in bounded windows.
///
/// The streamed ingest must not materialise a 1.09 GB tensor to hash it; this
/// walks the same bounded source the writer uses.
fn sha256_tensor_streaming(
    file: &weftfs::safetensors::SafetensorsFile,
    tensor: &weftfs::safetensors::SafetensorsTensor,
) -> Result<String, String> {
    let mut source = file
        .open_tensor_source(tensor)
        .map_err(|err| err.to_string())?;
    let total = source.len();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 8 << 20];
    let mut offset = 0u64;
    while offset < total {
        let want = (total - offset).min(buffer.len() as u64) as usize;
        source
            .read_at(offset, &mut buffer[..want])
            .map_err(|err| err.to_string())?;
        hasher.update(&buffer[..want]);
        offset += want as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}

// ---------------------------------------------------------------------------
// safetensors header parsing (no framework)
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

#[derive(Clone, Debug)]
struct Container {
    path: PathBuf,
    file_len: u64,
    data_start: u64,
    tensors: Vec<TensorHeader>,
}

fn parse_safetensors(path: &Path) -> Result<Container, String> {
    let mut file = File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let file_len = file
        .metadata()
        .map_err(|err| format!("{}: {err}", path.display()))?
        .len();
    let mut len_bytes = [0u8; 8];
    file.read_exact(&mut len_bytes)
        .map_err(|err| format!("{}: header length: {err}", path.display()))?;
    let header_len = u64::from_le_bytes(len_bytes);
    if header_len > 100 * 1024 * 1024 {
        return Err(format!(
            "{}: implausible header length {header_len}",
            path.display()
        ));
    }
    let mut json_bytes = vec![0u8; header_len as usize];
    file.read_exact(&mut json_bytes)
        .map_err(|err| format!("{}: header: {err}", path.display()))?;
    let data_start = 8 + header_len;
    let value: Json = serde_json::from_slice(&json_bytes)
        .map_err(|err| format!("{}: header JSON: {err}", path.display()))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{}: header is not an object", path.display()))?;
    let mut tensors = Vec::new();
    for (name, entry) in object {
        if name == "__metadata__" {
            continue;
        }
        let entry = entry
            .as_object()
            .ok_or_else(|| format!("{}: tensor `{name}` is not an object", path.display()))?;
        let dtype = entry
            .get("dtype")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("{}: tensor `{name}` has no dtype", path.display()))?
            .to_ascii_lowercase();
        let shape = entry
            .get("shape")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("{}: tensor `{name}` has no shape", path.display()))?
            .iter()
            .map(|dim| dim.as_u64().unwrap_or(0))
            .collect::<Vec<u64>>();
        let offsets = entry
            .get("data_offsets")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("{}: tensor `{name}` has no data_offsets", path.display()))?;
        if offsets.len() != 2 {
            return Err(format!(
                "{}: tensor `{name}` has bad data_offsets",
                path.display()
            ));
        }
        tensors.push(TensorHeader {
            name: name.clone(),
            dtype,
            shape,
            begin: offsets[0].as_u64().unwrap_or(0),
            end: offsets[1].as_u64().unwrap_or(0),
        });
    }
    tensors.sort_by_key(|tensor| tensor.begin);
    Ok(Container {
        path: path.to_path_buf(),
        file_len,
        data_start,
        tensors,
    })
}

fn read_tensor_bytes(dataset: &Container, tensor: &TensorHeader) -> Result<Vec<u8>, String> {
    let mut file =
        File::open(&dataset.path).map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    file.seek(SeekFrom::Start(dataset.data_start + tensor.begin))
        .map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    let mut bytes = vec![0u8; tensor.byte_len() as usize];
    file.read_exact(&mut bytes)
        .map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

// ---------------------------------------------------------------------------
// Store opening
// ---------------------------------------------------------------------------

fn store_options(_args: &[String]) -> Options {
    Options {
        chunk_size: 65_536,
        checkpoint_interval: 256,
        // A fixed clock makes every phase's store reproducible, so the slice
        // and streamed ingest runs can be compared by head rather than by
        // per-field inspection. The receipt records the value.
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn store_backend(args: &[String]) -> Result<BackendKind, String> {
    let kind = match value_of(args, "--backend").as_deref() {
        None | Some("local") => BackendKind::Local,
        Some("object") => BackendKind::Object,
        Some("s3") => BackendKind::S3 {
            config: weftfs::s3::S3Config::from_env().map_err(|err| err.to_string())?,
        },
        Some(other) => return Err(format!("unknown --backend `{other}`")),
    };
    Ok(if flag(args, "--pack") {
        let mut options = weftfs::packs::PackOptions::default();
        if let Some(text) = value_of(args, "--pack-catalog-bytes") {
            options.max_catalog_bytes = text
                .parse()
                .map_err(|_| "--pack-catalog-bytes must be an integer".to_owned())?;
        }
        options.validate().map_err(|err| err.to_string())?;
        BackendKind::packed(kind, options).map_err(|err| err.to_string())?
    } else {
        kind
    })
}

// ---------------------------------------------------------------------------
// ingest — per-tensor planner path (whole tensor bytes, or a bounded source)
// ---------------------------------------------------------------------------

fn ingest(args: &[String]) -> Result<(), String> {
    let source = PathBuf::from(required(args, "--source")?);
    let store_path = PathBuf::from(required(args, "--store")?);
    let out = PathBuf::from(required(args, "--out")?);
    let streaming = flag(args, "--stream");
    let stream_options = StreamedWriteOptions {
        window_bytes: parse_u64(args, "--window-bytes", 4 << 20)? as usize,
        max_frame_bytes: parse_u64(args, "--max-stream-bytes", u64::MAX)?,
        ..StreamedWriteOptions::default()
    };
    let limit = match value_of(args, "--limit") {
        Some(text) => Some(
            text.parse::<usize>()
                .map_err(|_| "--limit must be an integer".to_owned())?,
        ),
        None => None,
    };
    let source_sha256 = match value_of(args, "--source-sha256") {
        Some(text) => text,
        None => sha256_file(&source)?,
    };

    let container = parse_safetensors(&source)?;
    // A streamed write needs the real container reader; parse it once.
    let parsed = if streaming {
        Some(weftfs::safetensors::SafetensorsFile::parse(&source).map_err(|err| err.to_string())?)
    } else {
        None
    };
    let selected: Vec<&TensorHeader> = match limit {
        Some(limit) => container.tensors.iter().take(limit).collect(),
        None => container.tensors.iter().collect(),
    };
    let logical_bytes: u64 = selected.iter().map(|tensor| tensor.byte_len()).sum();
    let largest = selected
        .iter()
        .max_by_key(|tensor| tensor.byte_len())
        .map(|tensor| (tensor.name.clone(), tensor.byte_len()))
        .unwrap_or_default();

    if flag(args, "--fresh") && store_path.exists() {
        fs::remove_dir_all(&store_path)
            .map_err(|err| format!("{}: {err}", store_path.display()))?;
    }
    let backend = store_backend(args)?;
    let mut store = Store::init_with_backend(&store_path, store_options(args), backend)
        .map_err(|err| err.to_string())?;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    let dir = store
        .mkdir_seeded(root, b"model", 0o755, 0, 0, &[0x33u8; 32])
        .map_err(|err| err.to_string())?;
    let layout = TensorLayoutSpec::row_major();
    let spec = TensorWriteSpec {
        plan: TensorPlanOptions::default(),
        ..TensorWriteSpec::default()
    };

    profile_begin();
    let started = Instant::now();
    let mut rows = Vec::with_capacity(selected.len());
    let mut strategies: BTreeMap<String, u64> = BTreeMap::new();
    let mut written_bytes = 0u64;
    for (index, tensor) in selected.iter().enumerate() {
        eprintln!(
            "PROGRESS ingest-begin {} {} bytes={} rss_hwm_kib={}",
            index,
            tensor.name,
            tensor.byte_len(),
            peak_rss_kib()
        );
        // A deterministic per-tensor seed: the streamed and slice paths must
        // publish the same ObjectId, manifest and head for the same tensor.
        let seed = weftfs::identity::h("weft/scale7b/ingest-seed/v1", tensor.name.as_bytes());
        let read_started = Instant::now();
        let (id, digest, read_ms, write_ms) = if let Some(parsed) = parsed.as_ref() {
            let tensor_ref = parsed
                .tensors
                .iter()
                .find(|candidate| candidate.name == tensor.name)
                .ok_or_else(|| format!("the container reader has no tensor `{}`", tensor.name))?;
            // The digest is computed with a bounded streaming read, never by
            // materialising the tensor (that is the whole point of this phase).
            let digest = sha256_tensor_streaming(parsed, tensor_ref)?;
            let read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
            let tensor_started = Instant::now();
            let mut bytes_source = parsed
                .open_tensor_source(tensor_ref)
                .map_err(|err| err.to_string())?;
            let id = store
                .write_tensor_file_from_source(
                    dir,
                    tensor.name.as_bytes(),
                    &tensor.dtype,
                    &tensor.shape,
                    &mut bytes_source,
                    &layout,
                    &spec,
                    seed.as_bytes(),
                    &stream_options,
                )
                .map_err(|err| format!("streamed write {}: {err}", tensor.name))?;
            let write_ms = tensor_started.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "PROGRESS ingest {} {} {} bytes={} read_ms={} write_ms={} rss_hwm_kib={}",
                index,
                tensor.name,
                "streamed",
                tensor.byte_len(),
                round(read_ms),
                round(write_ms),
                peak_rss_kib()
            );
            (id, digest, read_ms, write_ms)
        } else {
            let bytes = read_tensor_bytes(&container, tensor)?;
            let read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
            let tensor_started = Instant::now();
            let id = store
                .write_tensor_file_with_spec_seeded(
                    dir,
                    tensor.name.as_bytes(),
                    &tensor.dtype,
                    &tensor.shape,
                    &bytes,
                    &layout,
                    &spec,
                    seed.as_bytes(),
                )
                .map_err(|err| format!("write {}: {err}", tensor.name))?;
            let write_ms = tensor_started.elapsed().as_secs_f64() * 1000.0;
            let meta = store.tensor_metadata(id).map_err(|err| err.to_string())?;
            eprintln!(
                "PROGRESS ingest {} {} {} bytes={} read_ms={} write_ms={} rss_hwm_kib={}",
                index,
                tensor.name,
                meta.strategy,
                tensor.byte_len(),
                round(read_ms),
                round(write_ms),
                peak_rss_kib()
            );
            let digest = sha256_hex(&bytes);
            (id, digest, read_ms, write_ms)
        };
        let meta = store.tensor_metadata(id).map_err(|err| err.to_string())?;
        *strategies.entry(meta.strategy.clone()).or_insert(0) += 1;
        written_bytes = written_bytes.saturating_add(tensor.byte_len());
        rows.push(json!({
            "name": tensor.name,
            "dtype": tensor.dtype,
            "shape": tensor.shape,
            "logical_bytes": tensor.byte_len(),
            "sha256": digest,
            "object_id": id.to_hex(),
            "logical_root": meta.logical_root.to_hex(),
            "strategy": meta.strategy,
            "read_ms": round(read_ms),
            "write_ms": round(write_ms),
            "rss_hwm_kib": peak_rss_kib(),
        }));
        // A partial result file is rewritten every tensor so an OOM still
        // leaves the exact progress that preceded it.
        write_json(
            &out,
            &json!({
                "phase": "ingest",
                "status": "running",
                "source": source.display().to_string(),
                "source_sha256": source_sha256,
                "logical_bytes": logical_bytes,
                "bytes_written": written_bytes,
                "tensors_done": index + 1,
                "tensors_total": selected.len(),
                "peak_rss_kib": peak_rss_kib(),
                "rows": rows,
            }),
        )?;
    }
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let verify = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    let report = json!({
        "phase": "ingest",
        "status": "ok",
        "source": source.display().to_string(),
        "source_bytes": container.file_len,
        "source_sha256": source_sha256,
        "streamed": streaming,
        "window_bytes": stream_options.window_bytes,
        "max_stream_bytes": stream_options.max_frame_bytes,
        "head": store.head().to_hex(),
        "tensors": selected.len(),
        "logical_bytes": logical_bytes,
        "largest_tensor": {"name": largest.0, "bytes": largest.1},
        "wall_ms": round(wall_ms),
        "mb_per_s": round(if wall_ms > 0.0 { logical_bytes as f64 / 1_000_000.0 / (wall_ms / 1000.0) } else { 0.0 }),
        "peak_rss_kib": peak_rss_kib(),
        "strategies": strategies,
        "store_bytes": tree_bytes(&store_path),
        "generation": store.generation(),
        "verify": verify_json(&verify),
        "profile": profile_json(),
        "rows": rows,
    });
    write_json(&out, &report)?;
    println!(
        "{}",
        serde_json::to_string(&report).map_err(|err| err.to_string())?
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// checkpoint — bounded / streaming writer
// ---------------------------------------------------------------------------

fn checkpoint(args: &[String]) -> Result<(), String> {
    let source = PathBuf::from(required(args, "--source")?);
    let store_path = PathBuf::from(required(args, "--store")?);
    let name = required(args, "--name")?;
    let out = PathBuf::from(required(args, "--out")?);
    let shards = parse_u64(args, "--shards", 8)? as u32;
    let axis = parse_u64(args, "--axis", 0)? as u32;
    let source_sha256 = match value_of(args, "--source-sha256") {
        Some(text) => text,
        None => sha256_file(&source)?,
    };
    let fast = value_of(args, "--write-policy").unwrap_or_else(|| "fast".to_owned()) == "fast";
    let fast_codec = match value_of(args, "--fast-codec").as_deref() {
        None | Some("raw") => FastCodec::Raw,
        Some("zstd") => FastCodec::Zstd,
        Some(other) => return Err(format!("unknown --fast-codec `{other}`")),
    };
    let strategy = match value_of(args, "--shard-strategy").as_deref() {
        None | Some("tensor-sharded") => ShardStrategy::TensorSharded,
        Some("planned") => ShardStrategy::Planned,
        Some("zstd") => ShardStrategy::Zstd,
        Some("raw") => ShardStrategy::Raw,
        Some(other) => return Err(format!("unknown --shard-strategy `{other}`")),
    };
    let mut options = CheckpointWriteOptions {
        shards,
        axis,
        strategy,
        write_policy: if fast {
            WritePolicy::Fast
        } else {
            WritePolicy::Planned
        },
        fast_codec,
        fast_zstd_level: parse_u64(args, "--fast-zstd-level", 1)? as i32,
        semantic_search: !flag(args, "--no-semantic"),
        max_ingest_bytes: parse_u64(args, "--max-ingest-bytes", 1024 * 1024 * 1024)?,
        // A fixed seed makes every phase's checkpoint reproducible, so a flat
        // write and a packed write of the same bytes must publish the same
        // head. Recorded in the report.
        deterministic_seed: Some([0x5a; 32]),
        ..CheckpointWriteOptions::default()
    };
    if let Some(filters) = value_of(args, "--tensor-filters") {
        options.filters = filters
            .split(',')
            .filter(|part| !part.trim().is_empty())
            .map(|part| part.trim().to_owned())
            .collect();
    }

    if flag(args, "--fresh") && store_path.exists() {
        fs::remove_dir_all(&store_path)
            .map_err(|err| format!("{}: {err}", store_path.display()))?;
    }
    let backend = store_backend(args)?;
    let mut store = if store_path.join("FORMAT").exists() {
        // A later checkpoint write into an existing store opens it read-write;
        // `init_with_backend` only creates a new store.
        Store::open_with_options_and_backend(
            &store_path,
            OpenMode::ReadWrite,
            store_options(args),
            backend,
        )
        .map_err(|err| err.to_string())?
    } else {
        Store::init_with_backend(&store_path, store_options(args), backend)
            .map_err(|err| err.to_string())?
    };
    profile_begin();
    let started = Instant::now();
    let report = store
        .write_checkpoint(&name, &source, &options)
        .map_err(|err| format!("checkpoint write: {err}"))?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let verify_report = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    let pack_stats = store.pack_stats();
    let out_json = json!({
        "phase": "checkpoint",
        "status": "ok",
        "source": source.display().to_string(),
        "source_bytes": report.source_file_bytes,
        "source_sha256": source_sha256,
        "reported_source_sha256": hex::encode(report.source_sha256),
        "name": report.name,
        "checkpoint_id": report.checkpoint_id.to_hex(),
        "manifest_id": report.manifest_id.to_hex(),
        "manifest_root": report.manifest_root.to_hex(),
        "head": report.head.to_hex(),
        "tensors": report.tensors.len(),
        "total_element_bytes": report.total_element_bytes,
        "shard_objects": report.shard_objects,
        "deterministic_seed": hex::encode([0x5au8; 32]),
        "complete": report.complete,
        "shards": shards,
        "axis": axis,
        "write_policy": if fast { "fast" } else { "planned" },
        "shard_strategy": strategy.as_str(),
        "fast_codec": if fast_codec == FastCodec::Raw { "raw" } else { "zstd" },
        "max_ingest_bytes": options.max_ingest_bytes,
        "packed": flag(args, "--pack"),
        "wall_ms": round(wall_ms),
        "mb_per_s": round(if wall_ms > 0.0 { report.source_file_bytes as f64 / 1_000_000.0 / (wall_ms / 1000.0) } else { 0.0 }),
        "peak_rss_kib": peak_rss_kib(),
        "store_bytes": tree_bytes(&store_path),
        "generation": store.generation(),
        "backend": {
            "reads": store.backend_stats().reads,
            "range_reads": store.backend_stats().range_reads,
            "bytes_read": store.backend_stats().bytes_read,
            "writes": store.backend_stats().writes,
            "bytes_written": store.backend_stats().bytes_written,
        },
        "pack_stats": pack_stats.map(|stats| json!({
            "sealed_packs": stats.sealed_packs,
            "packed_members": stats.packed_members,
            "packed_bytes": stats.packed_bytes,
            "flat_members": stats.flat_members,
            "flat_bytes": stats.flat_bytes,
            "catalog_entries": stats.catalog_entries,
            "catalog_segment_publishes": stats.catalog_segment_publishes,
            "catalog_root_publishes": stats.catalog_root_publishes,
            "packing_disabled": stats.packing_disabled,
        })),
        "verify": verify_json(&verify_report),
        "profile": profile_json(),
    });
    write_json(&out, &out_json)?;
    println!(
        "{}",
        serde_json::to_string(&out_json).map_err(|err| err.to_string())?
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// restore — pipelined workers into a SHA-256 sink
// ---------------------------------------------------------------------------

struct HashingSink {
    hasher: Sha256,
    bytes: u64,
}

impl Write for HashingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hasher.update(buf);
        self.bytes = self.bytes.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn restore(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let name = required(args, "--name")?;
    let out = PathBuf::from(required(args, "--out")?);
    let threads = parse_u64(args, "--threads", 1)? as usize;
    let tile_threads = parse_u64(args, "--tile-threads", 1)? as usize;
    let expect_sha = value_of(args, "--expect-sha256");

    let backend = store_backend(args)?;
    let store = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        store_options(args),
        backend,
    )
    .map_err(|err| err.to_string())?;
    let pipeline = RestorePipelineOptions {
        threads,
        tile_threads,
        ..RestorePipelineOptions::default()
    };
    let before = store.backend_stats();
    let started = Instant::now();
    let mut sink = HashingSink {
        hasher: Sha256::new(),
        bytes: 0,
    };
    let report = store
        .restore_checkpoint_pipelined(
            &name,
            &CheckpointRestoreOptions::default(),
            &pipeline,
            &mut sink,
        )
        .map_err(|err| format!("restore: {err}"))?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = store.backend_stats();
    let restored_sha = hex::encode(sink.hasher.finalize());
    let json_out = json!({
        "phase": "restore",
        "status": "ok",
        "name": report.name,
        "threads": threads,
        "tile_threads": tile_threads,
        "wall_ms": round(wall_ms),
        "bytes": sink.bytes,
        "mb_per_s": round(if wall_ms > 0.0 { sink.bytes as f64 / 1_000_000.0 / (wall_ms / 1000.0) } else { 0.0 }),
        "sha256": restored_sha,
        "expect_sha256": expect_sha,
        "sha256_match": expect_sha.map(|expected| expected == restored_sha),
        "source_sha256": report.source_sha256.map(hex::encode),
        "exact_source_file": report.exact_source_file,
        "tensors": report.tensors.len(),
        "shards_read": report.shards_read,
        "units_touched": report.units_touched,
        "store_bytes_read": report.store_bytes_read,
        "store_reads": report.store_reads,
        "backend": {
            "reads": after.reads.saturating_sub(before.reads),
            "range_reads": after.range_reads.saturating_sub(before.range_reads),
            "bytes_read": after.bytes_read.saturating_sub(before.bytes_read),
        },
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &json_out)?;
    println!(
        "{}",
        serde_json::to_string(&json_out).map_err(|err| err.to_string())?
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

fn verify(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let out = PathBuf::from(required(args, "--out")?);
    let backend = store_backend(args)?;
    let store = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        store_options(args),
        backend,
    )
    .map_err(|err| err.to_string())?;
    let started = Instant::now();
    let report = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let json_out = json!({
        "phase": "verify",
        "status": "ok",
        "wall_ms": round(wall_ms),
        "store_bytes": tree_bytes(&store_path),
        "generation": store.generation(),
        "verify": verify_json(&report),
    });
    write_json(&out, &json_out)?;
    println!(
        "{}",
        serde_json::to_string(&json_out).map_err(|err| err.to_string())?
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// catalog — pack-catalog accounting
// ---------------------------------------------------------------------------

/// Per-namespace bytes and file counts on disk.
fn namespace_bytes(root: &Path) -> BTreeMap<String, (u64, u64)> {
    fn walk(dir: &Path, total: &mut u64, files: &mut u64) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&entry.path(), total, files);
            } else if meta.is_file() {
                *total = total.saturating_add(meta.len());
                *files += 1;
            }
        }
    }
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let mut total = 0u64;
        let mut files = 0u64;
        if meta.is_dir() {
            walk(&entry.path(), &mut total, &mut files);
        } else if meta.is_file() {
            total = meta.len();
            files = 1;
        }
        out.insert(name, (total, files));
    }
    out
}

/// Report what the physical pack plane actually costs.
///
/// `packs/` is the packed data, `pack-catalog/` is catalog metadata. The
/// amplification a packed write pays is `catalog bytes / packed bytes`; the
/// absolute overhead is what the court compares before and after N15.
fn catalog(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let out = PathBuf::from(required(args, "--out")?);
    let backend = store_backend(args)?;
    let store = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        store_options(args),
        backend,
    )
    .map_err(|err| err.to_string())?;
    let namespaces = namespace_bytes(&store_path);
    let bytes_of = |name: &str| namespaces.get(name).map(|entry| entry.0).unwrap_or(0);
    let files_of = |name: &str| namespaces.get(name).map(|entry| entry.1).unwrap_or(0);
    let packed = bytes_of("packs");
    let catalog = bytes_of("pack-catalog");
    let pack_stats = store.pack_stats();
    let out_json = json!({
        "phase": "catalog",
        "status": "ok",
        "generation": store.generation(),
        "head": store.head().to_hex(),
        "namespaces": namespaces
            .iter()
            .map(|(name, (bytes, files))| (name.clone(), json!({"bytes": bytes, "files": files})))
            .collect::<BTreeMap<String, Json>>(),
        "packed_bytes": packed,
        "catalog_bytes": catalog,
        "catalog_files": files_of("pack-catalog"),
        "amplification": if packed > 0 {
            round(catalog as f64 / packed as f64)
        } else {
            0.0
        },
        "store_bytes": tree_bytes(&store_path),
        "objects_bytes": bytes_of("objects"),
        "manifests_bytes": bytes_of("manifests"),
        "commits_bytes": bytes_of("commits"),
        "indexes_bytes": bytes_of("indexes"),
        "pack_stats": pack_stats.map(|stats| json!({
            "sealed_packs": stats.sealed_packs,
            "packed_members": stats.packed_members,
            "packed_bytes": stats.packed_bytes,
            "flat_members": stats.flat_members,
            "flat_bytes": stats.flat_bytes,
            "catalog_entries": stats.catalog_entries,
            "catalog_segment_publishes": stats.catalog_segment_publishes,
            "catalog_root_publishes": stats.catalog_root_publishes,
            "packing_disabled": stats.packing_disabled,
        })),
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &out_json)?;
    println!(
        "{}",
        serde_json::to_string(&out_json).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn verify_json(report: &weftfs::store::VerifyReport) -> Json {
    json!({
        "ok": report.ok,
        "full": report.full,
        "generation": report.generation,
        "checked_objects": report.checked_objects,
        "checked_manifests": report.checked_manifests,
        "checked_chunks": report.checked_chunks,
        "checked_representations": report.checked_representations,
        "problems": report.problems.iter().map(|p| json!({"class": p.class, "detail": p.detail})).collect::<Vec<Json>>(),
    })
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
        }
    }
    let text = serde_json::to_string_pretty(value).map_err(|err| err.to_string())?;
    fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}
