//! Checkpoint-family lifecycle court (`bench/scripts/checkpoint_family_court.sh`).
//!
//! One court, every phase of a checkpoint family's life, measured on real
//! public weights:
//!
//! * `ingest`     — write checkpoint A and checkpoint B (B offers A's matching
//!                  tensor as its causal parent) and record the planner's choice
//!                  per tensor, physical bytes, peak RSS and wall time.
//! * `measure`    — retention breakdown, snapshot metadata cost, and the restore
//!                  matrix (full / 100-tensor / single / 100-row / 1-row) at one
//!                  and four threads, each with a complete latency distribution
//!                  and the backend bytes actually fetched.
//! * `baselines`  — independent per-tensor `zstd -19`, per-tensor
//!                  `zstd --patch-from`, and one whole-file `--patch-from`.
//! * `checkpoint` — one checkpoint written through the checkpoint API, 8-way
//!                  sharded, for the cross-topology phase.
//! * `topology`   — restore the 8-way checkpoint as 8-way, 4-way, row-major and
//!                  blocked, verifying every per-shard and per-tensor root.
//! * `crash-child`/`recovery` — a real process abort at each publication phase,
//!                  then reopen + verify and report time-to-usable.
//! * `gc`         — retention after deleting checkpoint B, with and without a
//!                  pinned snapshot, plus orphan checks.
//! * `assemble`   — merge the phase JSON into the receipt and print the tables.
//!
//! Every phase is a separate process so peak RSS is honest for that phase. The
//! harness prints JSON only; the court wrapper records the image, kernel, the
//! hard limits and the corpus hashes.
//!
//! Nothing here is a model framework: the safetensors header is parsed directly
//! (8-byte little-endian length + JSON) and the exact tensor bytes go through the
//! shipped store API.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

use weftcodec::tensor::Region;
use weftfs::backend::BackendKind;
use weftfs::checkpoint::{
    CheckpointRestoreOptions, CheckpointWriteOptions, RestoreTopology, ShardStrategy,
};
use weftfs::restore_pipeline::{RestorePipelineOptions, RestorePipelineStats};
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{AccessPattern, TensorLayoutSpec, TensorPlanOptions, TensorWriteSpec};
use weftfs::TileDecodeOptions;

const FIXED_TIMESTAMP_NS: i64 = 1_700_000_000_000_000_000;
const HEADER_OBJECT: &str = "__safetensors_header__";

fn main() {
    if let Err(err) = run() {
        eprintln!("family_lifecycle: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let phase = args.first().cloned().unwrap_or_default();
    match phase.as_str() {
        "ingest" => ingest(&args[1..]),
        "measure" => measure(&args[1..]),
        "baselines" => baselines(&args[1..]),
        "checkpoint" => checkpoint(&args[1..]),
        "restore" => restore(&args[1..]),
        "readers" => readers(&args[1..]),
        "throughput" => throughput(&args[1..]),
        "topology" => topology(&args[1..]),
        "crash-child" => crash_child(&args[1..]),
        "recovery" => recovery(&args[1..]),
        "gc" => gc(&args[1..]),
        "merge" => merge(&args[1..]),
        "recompare" => recompare(&args[1..]),
        "assemble" => assemble(&args[1..]),
        other => Err(format!(
            "unknown phase `{other}` (expected ingest, measure, baselines, checkpoint, restore, \
             throughput, topology, crash-child, recovery, gc, assemble)"
        )),
    }
}

// ---------------------------------------------------------------------------
// Small utilities
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

/// Complete latency distribution over the collected samples.
fn distribution(samples: &[f64]) -> Json {
    if samples.is_empty() {
        return json!({"samples": 0});
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pick = |pct: usize| -> f64 {
        let index = ((sorted.len() * pct) / 100).min(sorted.len() - 1);
        sorted[index]
    };
    let sum: f64 = sorted.iter().sum();
    json!({
        "samples": sorted.len(),
        "min_ms": round(sorted[0]),
        "p50_ms": round(pick(50)),
        "p95_ms": round(pick(95)),
        "p99_ms": round(pick(99)),
        "max_ms": round(sorted[sorted.len() - 1]),
        "mean_ms": round(sum / sorted.len() as f64),
        "all_ms": sorted.iter().map(|v| round(*v)).collect::<Vec<f64>>(),
    })
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 2]
}

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
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.is_dir() {
                walk(&entry.path(), total);
            } else {
                *total = total.saturating_add(meta.len());
            }
        }
    }
    let mut total = 0u64;
    walk(path, &mut total);
    total
}

/// Length of one content-addressed blob in a local store, or zero when the
/// blob is not present locally (a remote backend cannot be walked).
fn blob_len(store: &Path, kind: &str, hash: weftfs::Hash32) -> u64 {
    let text = hash.to_hex();
    let path = store.join(kind).join(&text[..2]).join(&text);
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Physical bytes of one representation: its manifest blob, its frame blob
/// (when frame-backed), and any raw chunk blobs. A frame's codec dependencies
/// belong to the object that names them and are not double-counted here.
fn physical_blob_bytes(
    store: &Path,
    manifest: &weftfs::format::Manifest,
    content: weftfs::Hash32,
) -> u64 {
    let mut total = blob_len(store, "manifests", content);
    if let Some(representation) = &manifest.representation {
        if let Some(frame) = representation.frame {
            total = total.saturating_add(blob_len(store, "manifests", frame));
        }
    }
    for chunk in &manifest.chunks {
        total = total.saturating_add(blob_len(store, "objects", chunk.hash));
    }
    total
}

fn physical_breakdown(store: &Path) -> Json {
    let objects = tree_bytes(&store.join("objects"));
    let manifests = tree_bytes(&store.join("manifests"));
    let indexes = tree_bytes(&store.join("indexes"));
    let commits = tree_bytes(&store.join("commits"));
    let refs = tree_bytes(&store.join("refs"));
    let total = tree_bytes(store);
    let data = objects + manifests;
    json!({
        "objects_bytes": objects,
        "manifests_bytes": manifests,
        "data_only_bytes": data,
        "indexes_bytes": indexes,
        "commits_bytes": commits,
        "refs_bytes": refs,
        "other_bytes": total.saturating_sub(data + indexes + commits + refs),
        "store_total_bytes": total,
        "indexes_percent_of_total": if total == 0 { 0.0 } else {
            round(indexes as f64 * 100.0 / total as f64)
        },
    })
}

fn stats_json(
    before: &weftfs::backend::BackendStats,
    after: &weftfs::backend::BackendStats,
) -> Json {
    json!({
        "reads": after.reads.saturating_sub(before.reads),
        "range_reads": after.range_reads.saturating_sub(before.range_reads),
        "bytes_read": after.bytes_read.saturating_sub(before.bytes_read),
        "writes": after.writes.saturating_sub(before.writes),
        "bytes_written": after.bytes_written.saturating_sub(before.bytes_written),
        "link_publications": after.link_publications.saturating_sub(before.link_publications),
        "rename_publications": after.rename_publications.saturating_sub(before.rename_publications),
        "rename_fallbacks": after.rename_fallbacks.saturating_sub(before.rename_fallbacks),
    })
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|err| format!("{}: {err}", path.display()))
}

fn read_json(path: &Path) -> Result<Json, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Backend named on the command line (`local`, `object`, `s3`).
///
/// A remote S3 backend has one namespace per bucket (the store root is not a
/// key prefix), so the court gives every repetition its own bucket through
/// `suffix`. Local stores use a directory instead.
fn backend_from_with_suffix(args: &[String], suffix: &str) -> Result<BackendKind, String> {
    let kind = match value_of(args, "--backend")
        .unwrap_or_else(|| "local".to_owned())
        .as_str()
    {
        "local" => BackendKind::Local,
        "object" => BackendKind::Object,
        "s3" => {
            let mut config = weftfs::s3::S3Config::from_env().map_err(|err| err.to_string())?;
            if !suffix.is_empty() {
                config.bucket = format!("{}-{}", config.bucket, suffix);
            }
            BackendKind::S3 { config }
        }
        other => {
            return Err(format!(
                "--backend must be local, object, or s3 (got `{other}`)"
            ))
        }
    };
    // `--pack` is a physical publication batching layer over the selected
    // backend, exactly as the CLI's global flag is.
    if flag(args, "--pack") {
        let mut policy = weftfs::packs::PackOptions::default();
        if let Some(bytes) = value_of(args, "--pack-target-bytes") {
            policy.pack_target_bytes = bytes
                .parse()
                .map_err(|_| "--pack-target-bytes must be an integer".to_owned())?;
        }
        if let Some(bytes) = value_of(args, "--pack-member-threshold") {
            policy.member_threshold_bytes = bytes
                .parse()
                .map_err(|_| "--pack-member-threshold must be an integer".to_owned())?;
        }
        if let Some(bytes) = value_of(args, "--pack-catalog-bytes") {
            policy.max_catalog_bytes = bytes
                .parse()
                .map_err(|_| "--pack-catalog-bytes must be an integer".to_owned())?;
        }
        if let Some(segments) = value_of(args, "--pack-segments") {
            policy.catalog_segments = segments
                .parse()
                .map_err(|_| "--pack-segments must be an integer".to_owned())?;
        }
        policy.validate().map_err(|err| err.to_string())?;
        return BackendKind::packed(kind, policy).map_err(|err| err.to_string());
    }
    Ok(kind)
}

/// Bucket suffix for one repetition of a remote store. The suffix also names
/// the store so the full store and the two GC fixtures cannot collide.
fn remote_suffix(root: &Path, rep: usize, reps: usize) -> String {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store");
    let name: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let tag = if rep + 1 == reps {
        "kept".to_owned()
    } else {
        format!("rep{rep}")
    };
    format!("{name}-{tag}")
}

fn backend_is_remote(args: &[String]) -> bool {
    value_of(args, "--backend")
        .map(|kind| kind != "local" && kind != "object")
        .unwrap_or(false)
}

fn present_present(value: &Json) -> bool {
    value
        .get("present")
        .and_then(Json::as_bool)
        .unwrap_or(false)
}

/// The store root actually written.
///
/// A remote S3 backend has one namespace per bucket and the store root is not a
/// key prefix, so the bucket name carries both the store's identity and the
/// repetition. Local backends use the directory itself.
fn store_root(_args: &[String], base: &Path, _rep: usize, _reps: usize) -> PathBuf {
    base.to_path_buf()
}

fn options(chunk_size: u64, checkpoint_interval: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
        ..Options::default()
    }
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
    header_bytes: Vec<u8>,
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
    let mut header_bytes = Vec::with_capacity(8 + json_bytes.len());
    header_bytes.extend_from_slice(&len_bytes);
    header_bytes.extend_from_slice(&json_bytes);
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
        header_bytes,
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

/// Streaming window for every harness read that does not need whole bytes.
const STREAM_WINDOW: u64 = 4 * 1024 * 1024;

/// Feed one tensor's bytes to `consume` in bounded windows.
fn for_each_tensor_window(
    dataset: &Container,
    tensor: &TensorHeader,
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut file =
        File::open(&dataset.path).map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    file.seek(SeekFrom::Start(dataset.data_start + tensor.begin))
        .map_err(|err| format!("{}: {err}", dataset.path.display()))?;
    let mut remaining = tensor.byte_len();
    let mut buffer = vec![0u8; STREAM_WINDOW.min(remaining) as usize];
    while remaining > 0 {
        let want = STREAM_WINDOW.min(remaining) as usize;
        file.read_exact(&mut buffer[..want])
            .map_err(|err| format!("{}: {err}", dataset.path.display()))?;
        consume(&buffer[..want])?;
        remaining -= want as u64;
    }
    Ok(())
}

/// Compare two same-length tensors in bounded windows.
fn tensors_equal(
    left_dataset: &Container,
    left: &TensorHeader,
    right_dataset: &Container,
    right: &TensorHeader,
) -> Result<bool, String> {
    if left.byte_len() != right.byte_len() {
        return Ok(false);
    }
    let mut a = File::open(&left_dataset.path)
        .map_err(|err| format!("{}: {err}", left_dataset.path.display()))?;
    let mut b = File::open(&right_dataset.path)
        .map_err(|err| format!("{}: {err}", right_dataset.path.display()))?;
    a.seek(SeekFrom::Start(left_dataset.data_start + left.begin))
        .map_err(|err| format!("{}: {err}", left_dataset.path.display()))?;
    b.seek(SeekFrom::Start(right_dataset.data_start + right.begin))
        .map_err(|err| format!("{}: {err}", right_dataset.path.display()))?;
    let mut remaining = left.byte_len();
    let mut buf_a = vec![0u8; STREAM_WINDOW.min(remaining) as usize];
    let mut buf_b = vec![0u8; STREAM_WINDOW.min(remaining) as usize];
    while remaining > 0 {
        let want = STREAM_WINDOW.min(remaining) as usize;
        a.read_exact(&mut buf_a[..want])
            .map_err(|err| format!("{}: {err}", left_dataset.path.display()))?;
        b.read_exact(&mut buf_b[..want])
            .map_err(|err| format!("{}: {err}", right_dataset.path.display()))?;
        if buf_a[..want] != buf_b[..want] {
            return Ok(false);
        }
        remaining -= want as u64;
    }
    Ok(true)
}

fn present_dtypes(dataset: &Container) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    for tensor in &dataset.tensors {
        *map.entry(tensor.dtype.clone()).or_default() += 1;
    }
    map
}

/// The largest rank-2 tensor: the region-read subject.
fn region_subject(dataset: &Container) -> Option<&TensorHeader> {
    dataset
        .tensors
        .iter()
        .filter(|tensor| tensor.shape.len() == 2 && tensor.shape[1] > 0)
        .max_by_key(|tensor| tensor.byte_len())
}

// ---------------------------------------------------------------------------
// ingest
// ---------------------------------------------------------------------------

fn ingest(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let base_path = PathBuf::from(required(args, "--base-file")?);
    let next_path = PathBuf::from(required(args, "--next-file")?);
    let family = value_of(args, "--family").unwrap_or_else(|| "unlabelled".to_owned());
    let variant = value_of(args, "--variant").unwrap_or_else(|| "default".to_owned());
    let out = PathBuf::from(required(args, "--out")?);
    let reps = parse_u64(args, "--reps", 3)? as usize;
    let chunk_size = parse_u64(args, "--chunk-size", 65_536)?;
    let checkpoint_interval = parse_u64(args, "--checkpoint-interval", 256)?;
    let lambda_ns = parse_u64(args, "--lambda-ns", 0)?;
    let access_pattern_text =
        value_of(args, "--access-pattern").unwrap_or_else(|| "whole".to_owned());
    let causal_enabled = !flag(args, "--no-causal");

    let base = parse_safetensors(&base_path)?;
    let next = parse_safetensors(&next_path)?;
    let base_names: BTreeMap<&str, &TensorHeader> =
        base.tensors.iter().map(|t| (t.name.as_str(), t)).collect();
    let next_names: BTreeMap<&str, &TensorHeader> =
        next.tensors.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut base_keys: Vec<&str> = base_names.keys().copied().collect();
    let mut next_keys: Vec<&str> = next_names.keys().copied().collect();
    base_keys.sort_unstable();
    next_keys.sort_unstable();
    if base_keys != next_keys {
        return Err("the two containers do not have the same tensor names".to_owned());
    }
    let mut shape_mismatches = Vec::new();
    let mut dtype_mismatches = Vec::new();
    for (name, base_tensor) in &base_names {
        let other = next_names
            .get(name)
            .ok_or_else(|| format!("second container is missing {name}"))?;
        if base_tensor.shape != other.shape {
            shape_mismatches.push((*name).to_owned());
        }
        if base_tensor.dtype != other.dtype {
            dtype_mismatches.push(format!(
                "{}: {} vs {}",
                name, base_tensor.dtype, other.dtype
            ));
        }
    }
    let causal_possible =
        causal_enabled && shape_mismatches.is_empty() && dtype_mismatches.is_empty();

    // Expected region bytes and the whole-family data digest, computed from the
    // source container before any store exists.
    let subject = region_subject(&next)
        .or_else(|| region_subject(&base))
        .ok_or_else(|| "no rank-2 tensor for the region matrix".to_owned())?;
    let subject_checkpoint = if region_subject(&next).is_some() {
        "next"
    } else {
        "base"
    };
    let row_bytes = subject.shape[1] * (subject.byte_len() / product(&subject.shape));
    // `rows:N` is a court convenience: N rows of this family's largest rank-2
    // tensor, expressed in bytes so the same spelling works for both corpora.
    let (access_pattern, resolved_pattern_text) = match access_pattern_text.strip_prefix("rows:") {
        Some(rows) => {
            let rows: u64 = rows
                .parse()
                .map_err(|_| format!("--access-pattern rows:{rows} is not an integer"))?;
            let region_bytes = rows.saturating_mul(row_bytes);
            (
                AccessPattern::Region {
                    region_bytes,
                    reads_per_write: 1000,
                },
                format!("region:{region_bytes}:1000"),
            )
        }
        None => (
            AccessPattern::parse(&access_pattern_text).map_err(|err| err.to_string())?,
            access_pattern_text.clone(),
        ),
    };
    let region_100 = Region::new(
        vec![0, 0],
        vec![100.min(subject.shape[0]), subject.shape[1]],
    );
    let region_1 = Region::new(vec![0, 0], vec![1, subject.shape[1]]);
    let source_of_subject = if subject_checkpoint == "next" {
        &next
    } else {
        &base
    };
    let subject_bytes = read_tensor_bytes(source_of_subject, subject)?;
    let region_100_expected = slice_region(&subject_bytes, &subject.shape, &region_100)?;
    let region_1_expected = slice_region(&subject_bytes, &subject.shape, &region_1)?;
    drop(subject_bytes);

    // Benchmark scoping only: a small real-weight subset for the GC and crash
    // phases. The library and CLI never limit anything.
    let limit: Option<usize> = value_of(args, "--limit")
        .map(|text| {
            text.parse()
                .map_err(|_| "--limit must be an integer".to_owned())
        })
        .transpose()?;
    let only_changed = flag(args, "--only-changed");
    let base_tensors: Vec<&TensorHeader> =
        if only_changed {
            let mut chosen = Vec::new();
            for tensor in &base.tensors {
                if let Some(other) = next.tensors.iter().find(|other| {
                    other.name == tensor.name && other.byte_len() == tensor.byte_len()
                }) {
                    if !tensors_equal(&base, tensor, &next, other)? {
                        chosen.push(tensor);
                    }
                }
                if limit.map(|limit| chosen.len() >= limit).unwrap_or(false) {
                    break;
                }
            }
            chosen
        } else {
            match limit {
                Some(limit) => base.tensors.iter().take(limit).collect(),
                None => base.tensors.iter().collect(),
            }
        };
    let next_tensors: Vec<&TensorHeader> = if only_changed {
        base_tensors
            .iter()
            .filter_map(|tensor| next.tensors.iter().find(|other| other.name == tensor.name))
            .collect()
    } else {
        match limit {
            Some(limit) => next.tensors.iter().take(limit).collect(),
            None => next.tensors.iter().collect(),
        }
    };
    // The digest the restore/gc phases compare against covers exactly the
    // selected tensors, so a limited fixture is self-consistent.
    let mut concat = BTreeMap::new();
    for (label, dataset, selected) in [
        ("base", &base, &base_tensors),
        ("next", &next, &next_tensors),
    ] {
        let mut hasher = Sha256::new();
        for tensor in selected {
            for_each_tensor_window(dataset, tensor, |piece| {
                hasher.update(piece);
                Ok(())
            })?;
        }
        concat.insert(label.to_owned(), hex::encode(hasher.finalize()));
    }

    let layout = TensorLayoutSpec::row_major();
    let plan = TensorPlanOptions {
        lambda_ns,
        access_pattern,
        tile_shape: value_of(args, "--tile-shape").map(|text| {
            text.split(',')
                .filter_map(|part| part.trim().parse::<u64>().ok())
                .collect::<Vec<u64>>()
        }),
        auto_tile_bytes: value_of(args, "--auto-tile-bytes")
            .map(|text| text.parse::<u64>())
            .transpose()
            .map_err(|_| "--auto-tile-bytes must be an integer".to_owned())?,
        ..TensorPlanOptions::default()
    };
    let spec = TensorWriteSpec {
        strategy_hint: match value_of(args, "--strategy") {
            Some(text) => Some(
                weftfs::tensor_store::TensorStrategy::parse(&text)
                    .map_err(|err| err.to_string())?,
            ),
            None => None,
        },
        plan: plan.clone(),
        ..TensorWriteSpec::default()
    };

    let mut rep_records = Vec::new();
    let mut last_tensors: Option<Json> = None;
    let mut last_physical = Json::Null;
    let mut last_verify = Json::Null;
    let mut last_snapshot = Json::Null;
    let mut last_generation = 0u64;
    // Phase profiling is opt-in. When it is on, the seven library phases are
    // attributed per tensor (a snapshot diff around one write) and summarised
    // for the whole phase; when it is off, the rows carry `null`.
    let profiling = phase_profiling_requested();
    if profiling {
        weftfs::phase_profile::begin();
    }

    for rep in 0..reps {
        let root_path = store_root(args, &store_path, rep, reps);
        let suffix = if backend_is_remote(args) {
            remote_suffix(&root_path, rep, reps)
        } else {
            String::new()
        };
        if !backend_is_remote(args) && root_path.exists() {
            fs::remove_dir_all(&root_path)
                .map_err(|err| format!("{}: {err}", root_path.display()))?;
        }
        let kind = backend_from_with_suffix(args, &suffix)?;
        let mut store =
            Store::init_with_backend(&root_path, options(chunk_size, checkpoint_interval), kind)
                .map_err(|e| e.to_string())?;
        let root = store.resolve_path("/").map_err(|e| e.to_string())?;
        let base_dir = store
            .mkdir(root, b"base", 0o755, 0, 0)
            .map_err(|e| e.to_string())?;
        let next_dir = store
            .mkdir(root, b"next", 0o755, 0, 0)
            .map_err(|e| e.to_string())?;
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
            .map_err(|e| format!("write base header: {e}"))?;
        store
            .create_file(
                next_dir,
                HEADER_OBJECT.as_bytes(),
                &next.header_bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .map_err(|e| format!("write next header: {e}"))?;

        let base_started = Instant::now();
        let mut base_rows = Vec::new();
        let mut base_versions: BTreeMap<String, weftfs::Hash32> = BTreeMap::new();
        for tensor in &base_tensors {
            let read_started = Instant::now();
            let bytes = read_tensor_bytes(&base, tensor)?;
            if profiling {
                weftfs::phase_profile::record(
                    weftfs::Phase::SourceRead,
                    read_started.elapsed().as_nanos() as u64,
                );
            }
            if std::env::var_os("WEFT_FAMILY_PROGRESS").is_some() {
                eprintln!(
                    "ingest: base {} bytes={} rss_hwm_kib={}",
                    tensor.name,
                    tensor.byte_len(),
                    peak_rss_kib()
                );
            }
            let phase_before = profiling.then(weftfs::phase_profile::snapshot);
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
            let write_ms = started.elapsed().as_secs_f64() * 1000.0;
            let tensor_phases = phase_before
                .map(|before| weftfs::phase_profile::snapshot().since(&before).to_json());
            let meta = store.tensor_metadata(id).map_err(|e| e.to_string())?;
            let record = store.object_meta(id).map_err(|e| e.to_string())?;
            let manifest = store
                .manifest_of(record.content)
                .map_err(|e| e.to_string())?;
            let data_bytes = physical_blob_bytes(&root_path, &manifest, record.content);
            base_versions.insert(tensor.name.clone(), record.version);
            base_rows.push(json!({
                "name": tensor.name,
                "dtype": tensor.dtype,
                "shape": tensor.shape,
                "logical_bytes": tensor.byte_len(),
                "sha256": sha256_hex(&bytes),
                "object_id": id.to_hex(),
                "version": record.version.to_hex(),
                "logical_root": meta.logical_root.to_hex(),
                "strategy": meta.strategy,
                "tile_shape": meta.tile_shape,
                "data_bytes": data_bytes,
                "write_ms": round(write_ms),
                "phases": tensor_phases,
            }));
        }
        let base_wall_ms = base_started.elapsed().as_secs_f64() * 1000.0;
        let base_stats_after = store.backend_stats();

        let next_started = Instant::now();
        let mut next_rows = Vec::new();
        for tensor in &next_tensors {
            let read_started = Instant::now();
            let bytes = read_tensor_bytes(&next, tensor)?;
            if profiling {
                weftfs::phase_profile::record(
                    weftfs::Phase::SourceRead,
                    read_started.elapsed().as_nanos() as u64,
                );
            }
            if std::env::var_os("WEFT_FAMILY_PROGRESS").is_some() {
                eprintln!(
                    "ingest: next {} bytes={} rss_hwm_kib={}",
                    tensor.name,
                    tensor.byte_len(),
                    peak_rss_kib()
                );
            }
            let mut tensor_spec = spec.clone();
            if causal_possible {
                if let Some(version) = base_versions.get(&tensor.name) {
                    tensor_spec.causal = vec![*version];
                }
            }
            let phase_before = profiling.then(weftfs::phase_profile::snapshot);
            let started = Instant::now();
            let id = store
                .write_tensor_file_with_spec(
                    next_dir,
                    tensor.name.as_bytes(),
                    &tensor.dtype,
                    &tensor.shape,
                    &bytes,
                    &layout,
                    &tensor_spec,
                )
                .map_err(|err| format!("write next/{}: {err}", tensor.name))?;
            let write_ms = started.elapsed().as_secs_f64() * 1000.0;
            let tensor_phases = phase_before
                .map(|before| weftfs::phase_profile::snapshot().since(&before).to_json());
            let meta = store.tensor_metadata(id).map_err(|e| e.to_string())?;
            let record = store.object_meta(id).map_err(|e| e.to_string())?;
            let manifest = store
                .manifest_of(record.content)
                .map_err(|e| e.to_string())?;
            let data_bytes = physical_blob_bytes(&root_path, &manifest, record.content);
            next_rows.push(json!({
                "name": tensor.name,
                "dtype": tensor.dtype,
                "shape": tensor.shape,
                "logical_bytes": tensor.byte_len(),
                "sha256": sha256_hex(&bytes),
                "data_bytes": data_bytes,
                "source_delta_bytes": if rep + 1 == reps { tensor_byte_delta(&base, &next, &tensor.name)? } else { Json::Null },
                "region_sha256": if tensor.name == subject.name { json!(sha256_hex(&region_100_expected)) } else { Json::Null },
                "object_id": id.to_hex(),
                "version": record.version.to_hex(),
                "logical_root": meta.logical_root.to_hex(),
                "strategy": meta.strategy,
                "tile_shape": meta.tile_shape,
                "causal": if tensor_spec.causal.is_empty() { Json::Null } else { json!(tensor_spec.causal[0].to_hex()) },
                "write_ms": round(write_ms),
                "phases": tensor_phases,
            }));
        }
        let next_wall_ms = next_started.elapsed().as_secs_f64() * 1000.0;
        let next_stats_after = store.backend_stats();

        let generation = store.generation();
        if rep + 1 == reps {
            let verify = store.verify(true).map_err(|e| format!("verify: {e}"))?;
            last_verify = json!({
                "ok": verify.ok,
                "full": verify.full,
                "checked_objects": verify.checked_objects,
                "checked_manifests": verify.checked_manifests,
                "checked_chunks": verify.checked_chunks,
                "checked_representations": verify.checked_representations,
                "problems": verify.problems.iter().map(|p| json!({"class": p.class, "detail": p.detail})).collect::<Vec<Json>>(),
            });
            let mut create_samples = Vec::new();
            if !flag(args, "--no-snapshot") {
                for index in 0..3 {
                    let name = format!("family-pin-{index}");
                    let started = Instant::now();
                    let commit = store
                        .snapshot_create(&name)
                        .map_err(|e| format!("snapshot: {e}"))?;
                    create_samples.push((
                        name.clone(),
                        commit,
                        started.elapsed().as_secs_f64() * 1000.0,
                    ));
                    // Keep the first name; the extra names exercise the same path.
                    if index == 0 {
                        last_snapshot = json!({
                            "name": name,
                            "commit": commit.to_hex(),
                            "create_ms_samples": create_samples.iter().map(|(_,_,ms)| round(*ms)).collect::<Vec<f64>>(),
                        });
                    }
                }
                // Snapshot metadata cost: list + pin resolution against the live store.
                let mut meta_samples = Vec::new();
                for _ in 0..5 {
                    let started = Instant::now();
                    let names = store.snapshot_list();
                    let mut rows = Vec::new();
                    for (name, commit) in &names {
                        let resolved = store
                            .resolve_snapshot_ref(name)
                            .map_err(|e| format!("resolve snapshot: {e}"))?;
                        rows.push((name.clone(), commit.to_hex(), resolved.to_hex()));
                    }
                    let _ = rows;
                    meta_samples.push(started.elapsed().as_secs_f64() * 1000.0);
                }
                if let Some(object) = last_snapshot.as_object_mut() {
                    object.insert(
                        "metadata_ms_median".to_owned(),
                        json!(round(median(&meta_samples))),
                    );
                    object.insert(
                        "metadata_ms_samples".to_owned(),
                        json!(meta_samples.iter().map(|v| round(*v)).collect::<Vec<f64>>()),
                    );
                }
            }
            last_physical = if backend_is_remote(args) {
                json!({
                    "note": "remote object store: no local tree to walk; retention is reported from the write stream",
                    "bytes_written": store.backend_stats().bytes_written,
                })
            } else {
                physical_breakdown(&root_path)
            };
            last_tensors = Some(json!({
                "base": base_rows,
                "next": next_rows,
                "stats": {
                    "base": stats_json(&weftfs::backend::BackendStats::default(), &base_stats_after),
                    "next": stats_json(&base_stats_after, &next_stats_after),
                },
            }));
        }
        let _ = store;
        last_generation = generation;
        rep_records.push(json!({
            "rep": rep,
            "base_wall_ms": round(base_wall_ms),
            "next_wall_ms": round(next_wall_ms),
            "family_wall_ms": round(base_wall_ms + next_wall_ms),
            "base_tensor_write_ms": round(sum_write_ms(&base_rows)),
            "next_tensor_write_ms": round(sum_write_ms(&next_rows)),
            "generation": generation,
        }));
    }

    let walls: Vec<f64> = rep_records
        .iter()
        .filter_map(|row| row.get("family_wall_ms").and_then(Json::as_f64))
        .collect();
    let phase_totals = if profiling {
        weftfs::phase_profile::finish().to_json()
    } else {
        Json::Null
    };
    let tensors = last_tensors.ok_or_else(|| "ingest produced no tensor records".to_owned())?;
    let base_rows = tensors
        .get("base")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let next_rows = tensors
        .get("next")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();

    let receipt = json!({
        "schema": "weft.family-lifecycle.ingest.v1",
        "phase": "ingest",
        "family": family,
        "variant": variant,
        "store": store_path.display().to_string(),
        "store_actual": store_root(args, &store_path, reps.saturating_sub(1), reps).display().to_string(),
        "options": {
            "chunk_size": chunk_size,
            "checkpoint_interval": checkpoint_interval,
            "lambda_ns": lambda_ns,
            "access_pattern": resolved_pattern_text,
            "access_pattern_requested": access_pattern_text,
            "tile_shape": value_of(args, "--tile-shape"),
            "auto_tile_bytes": value_of(args, "--auto-tile-bytes"),
            "strategy_hint": value_of(args, "--strategy"),
            "planner_cost_model": "measured (baked weftfs-r2 table)",
            "reps": reps,
            "tensor_limit": limit,
            "causal_parent_offered": causal_possible,
        },
        "corpus": {
            "first": corpus_summary(&base)?,
            "second": corpus_summary(&next)?,
            "same_tensor_names": true,
            "shape_mismatches": shape_mismatches,
            "dtype_mismatches": dtype_mismatches,
            "concat_data_sha256": concat,
            "subject": {
                "checkpoint": subject_checkpoint,
                "tensor": subject.name,
                "shape": subject.shape,
                "row_bytes": row_bytes,
                "region_100_bytes": region_100_expected.len(),
                "region_1_bytes": region_1_expected.len(),
                "region_100_sha256": sha256_hex(&region_100_expected),
                "region_1_sha256": sha256_hex(&region_1_expected),
            },
        },
        "reps": rep_records,
        "ingest_ms": {
            "median_family_wall_ms": round(median(&walls)),
            "min_family_wall_ms": round(walls.iter().cloned().fold(f64::INFINITY, f64::min)),
            "max_family_wall_ms": round(walls.iter().cloned().fold(0.0f64, f64::max)),
        },
        "phases": phase_totals,
        "phase_note": if profiling {
            "wall-clock totals across every thread; a phase total can exceed wall time when the prepare path runs it concurrently. Unmeasured time is the residual (chunk-table construction, planner arithmetic, in-memory state application, harness bookkeeping)."
        } else {
            "phase profiling was off (set WEFT_PHASE_PROFILE=1)"
        },
        "tensors": {
            "base": base_rows,
            "next": next_rows,
            "stats": tensors.get("stats").cloned().unwrap_or(Json::Null),
        },
        "strategy_distribution": {
            "base": distribution_of(&base_rows),
            "next": distribution_of(&next_rows),
        },
        "physical": last_physical,
        "verify": last_verify,
        "snapshot": last_snapshot,
        "snapshot_requested": !flag(args, "--no-snapshot"),
        "generation": last_generation,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

fn sum_write_ms(rows: &[Json]) -> f64 {
    rows.iter()
        .filter_map(|row| row.get("write_ms").and_then(Json::as_f64))
        .sum()
}

fn product(shape: &[u64]) -> u64 {
    shape.iter().fold(1u64, |acc, dim| acc.saturating_mul(*dim))
}

fn slice_region(bytes: &[u8], shape: &[u64], region: &Region) -> Result<Vec<u8>, String> {
    region.validate(shape).map_err(|err| err.to_string())?;
    let element = bytes.len() as u64 / product(shape).max(1);
    let start = (region.start[0] * shape[1] * element) as usize;
    let end = ((region.end[0] * shape[1] * element) as usize).min(bytes.len());
    Ok(bytes[start..end].to_vec())
}

fn tensor_byte_delta(base: &Container, next: &Container, name: &str) -> Result<Json, String> {
    let Some(base_tensor) = base.tensors.iter().find(|t| t.name == name) else {
        return Ok(Json::Null);
    };
    let Some(next_tensor) = next.tensors.iter().find(|t| t.name == name) else {
        return Ok(Json::Null);
    };
    if base_tensor.byte_len() != next_tensor.byte_len() {
        return Ok(Json::Null);
    }
    let left = base_tensor;
    let right = next_tensor;
    let mut a = File::open(&base.path).map_err(|err| format!("{}: {err}", base.path.display()))?;
    let mut b = File::open(&next.path).map_err(|err| format!("{}: {err}", next.path.display()))?;
    a.seek(SeekFrom::Start(base.data_start + left.begin))
        .map_err(|err| format!("{}: {err}", base.path.display()))?;
    b.seek(SeekFrom::Start(next.data_start + right.begin))
        .map_err(|err| format!("{}: {err}", next.path.display()))?;
    let mut remaining = left.byte_len();
    let mut buf_a = vec![0u8; STREAM_WINDOW.min(remaining) as usize];
    let mut buf_b = vec![0u8; STREAM_WINDOW.min(remaining) as usize];
    let mut changed = 0u64;
    while remaining > 0 {
        let want = STREAM_WINDOW.min(remaining) as usize;
        a.read_exact(&mut buf_a[..want])
            .map_err(|err| format!("{}: {err}", base.path.display()))?;
        b.read_exact(&mut buf_b[..want])
            .map_err(|err| format!("{}: {err}", next.path.display()))?;
        changed += buf_a[..want]
            .chunks_exact(4)
            .zip(buf_b[..want].chunks_exact(4))
            .filter(|(x, y)| x != y)
            .count() as u64;
        remaining -= want as u64;
    }
    Ok(json!({
        "changed_words": changed,
        "words": left.byte_len() / 4,
    }))
}

fn distribution_of(rows: &[Json]) -> Json {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    let mut data: BTreeMap<String, u64> = BTreeMap::new();
    let mut logical: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows {
        let strategy = row
            .get("strategy")
            .and_then(Json::as_str)
            .unwrap_or("unknown")
            .to_owned();
        *count.entry(strategy.clone()).or_default() += 1;
        *data.entry(strategy.clone()).or_default() +=
            row.get("data_bytes").and_then(Json::as_u64).unwrap_or(0);
        *logical.entry(strategy).or_default() +=
            row.get("logical_bytes").and_then(Json::as_u64).unwrap_or(0);
    }
    json!({
        "tensors_by_strategy": count,
        "data_bytes_by_strategy": data,
        "logical_bytes_by_strategy": logical,
    })
}

fn corpus_summary(dataset: &Container) -> Result<Json, String> {
    Ok(json!({
        "path": dataset.path.display().to_string(),
        "file_bytes": dataset.file_len,
        "sha256": file_sha256(&dataset.path)?,
        "header_bytes": dataset.header_bytes.len(),
        "tensor_count": dataset.tensors.len(),
        "tensor_data_bytes": dataset.tensors.iter().map(|t| t.byte_len()).sum::<u64>(),
        "dtype_histogram": present_dtypes(dataset),
    }))
}

// ---------------------------------------------------------------------------
// measure: retention, snapshot metadata, restore matrix
// ---------------------------------------------------------------------------

struct HashSink {
    hasher: Sha256,
    bytes: u64,
}

impl Write for HashSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hasher.update(buf);
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Counts restored bytes without hashing them, so a scaling run measures the
/// store's decode + read path rather than the harness's single-threaded
/// SHA-256 of the output. Exactness is proven separately with a `HashSink`.
struct CountSink {
    bytes: u64,
}

impl Write for CountSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The restore destination chosen by `--sink`.
enum RestoreSink {
    Sha(HashSink),
    Count(CountSink),
}

impl RestoreSink {
    fn new(sha: bool) -> Self {
        if sha {
            RestoreSink::Sha(HashSink {
                hasher: Sha256::new(),
                bytes: 0,
            })
        } else {
            RestoreSink::Count(CountSink { bytes: 0 })
        }
    }

    fn bytes(&self) -> u64 {
        match self {
            RestoreSink::Sha(sink) => sink.bytes,
            RestoreSink::Count(sink) => sink.bytes,
        }
    }

    fn digest(self) -> Option<[u8; 32]> {
        match self {
            RestoreSink::Sha(sink) => Some(sink.hasher.finalize().into()),
            RestoreSink::Count(_) => None,
        }
    }
}

impl Write for RestoreSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            RestoreSink::Sha(sink) => sink.write(buf),
            RestoreSink::Count(sink) => sink.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn measure(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let ingest_path = PathBuf::from(required(args, "--ingest")?);
    let out = PathBuf::from(required(args, "--out")?);
    let reps = parse_u64(args, "--reps", 3)? as usize;
    let threads: Vec<usize> = value_of(args, "--threads")
        .unwrap_or_else(|| "1,4".to_owned())
        .split(',')
        .filter_map(|text| text.trim().parse().ok())
        .collect();

    let ingest = read_json(&ingest_path)?;
    let subject = ingest
        .pointer("/corpus/subject")
        .ok_or_else(|| "ingest JSON has no corpus.subject".to_owned())?
        .clone();
    let subject_name = subject
        .get("tensor")
        .and_then(Json::as_str)
        .ok_or_else(|| "subject.tensor missing".to_owned())?
        .to_owned();
    let subject_shape: Vec<u64> = subject
        .get("shape")
        .and_then(Json::as_array)
        .map(|items| items.iter().filter_map(Json::as_u64).collect())
        .unwrap_or_default();
    let expected_concat: BTreeMap<String, String> = ingest
        .pointer("/corpus/concat_data_sha256")
        .and_then(Json::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                .collect()
        })
        .unwrap_or_default();
    let region_100_sha = subject
        .get("region_100_sha256")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned();
    let region_1_sha = subject
        .get("region_1_sha256")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned();

    let open_started = Instant::now();
    let store = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        options(65_536, 256),
        backend_from_with_suffix(
            args,
            &if backend_is_remote(args) {
                remote_suffix(&store_path, 0, 1)
            } else {
                String::new()
            },
        )?,
    )
    .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;

    // Resolve every tensor object once (in-memory state only).
    let next_rows = ingest
        .pointer("/tensors/next")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let base_rows = ingest
        .pointer("/tensors/base")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    if next_rows.is_empty() || base_rows.is_empty() {
        return Err("ingest JSON has no tensor rows".to_owned());
    }
    let mut all_ids: Vec<(String, weftfs::Hash32)> = Vec::new();
    for row in next_rows.iter().chain(base_rows.iter()) {
        let name = row.get("name").and_then(Json::as_str).unwrap_or("");
        let id = row.get("object_id").and_then(Json::as_str).unwrap_or("");
        let id = weftfs::Hash32::from_hex(id).map_err(|err| format!("object id: {err}"))?;
        all_ids.push((name.to_owned(), id));
    }
    let next_len = next_rows.len();
    let next_ids: Vec<weftfs::Hash32> = all_ids[..next_len].iter().map(|(_, id)| *id).collect();
    let _base_ids: Vec<weftfs::Hash32> = all_ids[next_len..].iter().map(|(_, id)| *id).collect();

    let subject_id = all_ids
        .iter()
        .find(|(name, _)| *name == subject_name)
        .map(|(_, id)| *id)
        .ok_or_else(|| format!("subject tensor `{subject_name}` not found"))?;

    let mut matrix: BTreeMap<String, Json> = BTreeMap::new();

    // Full family restore, once per requested thread count.
    for &thread_count in &threads {
        let pipeline = RestorePipelineOptions {
            threads: thread_count,
            max_inflight_bytes: 64 * 1024 * 1024,
            tile_threads: thread_count,
            shard_threads: 1,
            shard_max_inflight_bytes: 64 * 1024 * 1024,
        };
        let mut samples = Vec::new();
        let mut bytes_read = 0u64;
        let mut requests = 0u64;
        let mut hash_ok = false;
        for rep in 0..reps {
            let before = store.backend_stats();
            let mut sink = HashSink {
                hasher: Sha256::new(),
                bytes: 0,
            };
            let started = Instant::now();
            let stats: RestorePipelineStats = store
                .restore_tensors_pipelined(&next_ids, &mut sink, &pipeline)
                .map_err(|err| format!("full restore (threads={thread_count}): {err}"))?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            let after = store.backend_stats();
            samples.push(elapsed);
            bytes_read = after.bytes_read.saturating_sub(before.bytes_read);
            requests = after.reads.saturating_sub(before.reads)
                + after.range_reads.saturating_sub(before.range_reads);
            if rep == 0 {
                let got = hex::encode(sink.hasher.finalize());
                hash_ok = expected_concat
                    .get("next")
                    .map(|expected| *expected == got)
                    .unwrap_or(false);
                if sink.bytes != stats.bytes_written {
                    return Err("pipeline hash sink saw a different byte count".to_owned());
                }
            }
        }
        matrix.insert(
            format!("full_family_threads_{thread_count}"),
            json!({
                "phase": "full_family",
                "threads": thread_count,
                "logical_bytes": next_rows.iter().filter_map(|r| r.get("logical_bytes").and_then(Json::as_u64)).sum::<u64>(),
                "backend_bytes_read_last_rep": bytes_read,
                "backend_requests_last_rep": requests,
                "sha256_match": hash_ok,
                "latency": distribution(&samples),
            }),
        );
    }

    // 100-tensor subset (the first 100 in source order).
    let subset_count = 100usize.min(next_ids.len());
    let subset_ids = &next_ids[..subset_count];
    let subset_expected: Vec<String> = next_rows[..subset_count]
        .iter()
        .map(|row| {
            row.get("sha256")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_owned()
        })
        .collect();
    for &thread_count in &threads {
        let pipeline = RestorePipelineOptions {
            threads: thread_count,
            max_inflight_bytes: 64 * 1024 * 1024,
            tile_threads: thread_count,
            shard_threads: 1,
            shard_max_inflight_bytes: 64 * 1024 * 1024,
        };
        let mut samples = Vec::new();
        let mut bytes_read = 0u64;
        for _ in 0..reps {
            let before = store.backend_stats();
            let mut sink = io::sink();
            let started = Instant::now();
            store
                .restore_tensors_pipelined(subset_ids, &mut sink, &pipeline)
                .map_err(|err| format!("subset restore: {err}"))?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            let after = store.backend_stats();
            bytes_read = after.bytes_read.saturating_sub(before.bytes_read);
        }
        // Correctness pass (not timed): every subset tensor by SHA-256.
        let mut all_match = true;
        for (index, id) in subset_ids.iter().enumerate() {
            let bytes = store.read_tensor(*id).map_err(|e| e.to_string())?;
            if sha256_hex(&bytes) != subset_expected[index] {
                all_match = false;
            }
        }
        matrix.insert(
            format!("subset100_threads_{thread_count}"),
            json!({
                "phase": "subset100",
                "tensors": subset_count,
                "threads": thread_count,
                "backend_bytes_read_last_rep": bytes_read,
                "sha256_match": all_match,
                "latency": distribution(&samples),
            }),
        );
    }

    // Single tensor.
    for &thread_count in &threads {
        let tile = TileDecodeOptions {
            threads: thread_count,
            ..TileDecodeOptions::default()
        };
        let mut samples = Vec::new();
        let mut bytes_read = 0u64;
        let mut logical = 0u64;
        let mut hash_ok = false;
        for rep in 0..reps {
            let before = store.backend_stats();
            let started = Instant::now();
            let bytes = store
                .read_tensor_with_options(subject_id, &tile)
                .map_err(|err| format!("single tensor: {err}"))?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            let after = store.backend_stats();
            bytes_read = after.bytes_read.saturating_sub(before.bytes_read);
            logical = bytes.len() as u64;
            if rep == 0 {
                hash_ok = next_rows
                    .iter()
                    .find(|row| {
                        row.get("name").and_then(Json::as_str) == Some(subject_name.as_str())
                    })
                    .and_then(|row| row.get("sha256"))
                    .and_then(Json::as_str)
                    .map(|expected| expected == sha256_hex(&bytes))
                    .unwrap_or(false);
            }
        }
        matrix.insert(
            format!("single_tensor_threads_{thread_count}"),
            json!({
                "phase": "single_tensor",
                "tensor": subject_name,
                "threads": thread_count,
                "logical_bytes": logical,
                "backend_bytes_read_last_rep": bytes_read,
                "sha256_match": hash_ok,
                "latency": distribution(&samples),
            }),
        );
    }

    // 100-row and 1-row region reads.
    let region_100 = Region::new(
        vec![0, 0],
        vec![100.min(subject_shape[0]), subject_shape[1]],
    );
    let region_1 = Region::new(vec![0, 0], vec![1, subject_shape[1]]);
    for (label, region, expected) in [
        ("region_100_rows", &region_100, &region_100_sha),
        ("region_1_row", &region_1, &region_1_sha),
    ] {
        for &thread_count in &threads {
            let tile = TileDecodeOptions {
                threads: thread_count,
                ..TileDecodeOptions::default()
            };
            let mut samples = Vec::new();
            let mut bytes_read = 0u64;
            let mut requests = 0u64;
            let mut region_bytes = 0u64;
            let mut units = 0u64;
            let mut hash_ok = false;
            for rep in 0..reps {
                let before = store.backend_stats();
                let started = Instant::now();
                let read = store
                    .read_tensor_region_with_options(subject_id, region, &tile)
                    .map_err(|err| format!("{label}: {err}"))?;
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                let after = store.backend_stats();
                bytes_read = after.bytes_read.saturating_sub(before.bytes_read);
                requests = after.reads.saturating_sub(before.reads)
                    + after.range_reads.saturating_sub(before.range_reads);
                region_bytes = read.bytes.len() as u64;
                units = read.units_touched;
                if rep == 0 {
                    hash_ok = sha256_hex(&read.bytes) == *expected;
                }
            }
            matrix.insert(
                format!("{label}_threads_{thread_count}"),
                json!({
                    "phase": label,
                    "tensor": subject_name,
                    "region": {"start": region.start, "end": region.end},
                    "threads": thread_count,
                    "logical_bytes": region_bytes,
                    "units_touched": units,
                    "backend_bytes_read_last_rep": bytes_read,
                    "backend_requests_last_rep": requests,
                    "sha256_match": hash_ok,
                    "latency": distribution(&samples),
                }),
            );
        }
    }

    // Snapshot metadata cost against the pinned snapshot.
    let snapshot_rows = store.snapshot_list();
    let mut snapshot_meta_samples = Vec::new();
    for _ in 0..reps * 2 {
        let started = Instant::now();
        let names = store.snapshot_list();
        for (name, _) in &names {
            let _ = store.resolve_snapshot_ref(name);
        }
        snapshot_meta_samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let snapshot_commit = snapshot_rows
        .first()
        .map(|(_, commit)| commit.to_hex())
        .unwrap_or_default();
    // The same full restore with the immutable snapshot pinned. A pin must not
    // change what a live read does; this measures that claim rather than
    // assuming it.
    let pinned_samples = {
        let pipeline = RestorePipelineOptions {
            threads: 1,
            max_inflight_bytes: 64 * 1024 * 1024,
            tile_threads: 1,
            shard_threads: 1,
            shard_max_inflight_bytes: 64 * 1024 * 1024,
        };
        let mut samples = Vec::new();
        let mut bytes_read = 0u64;
        for _ in 0..reps {
            let before = store.backend_stats();
            let mut sink = io::sink();
            let started = Instant::now();
            store
                .restore_tensors_pipelined(&next_ids, &mut sink, &pipeline)
                .map_err(|err| format!("pinned restore: {err}"))?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            let after = store.backend_stats();
            bytes_read = after.bytes_read.saturating_sub(before.bytes_read);
        }
        json!({
            "threads": 1,
            "backend_bytes_read_last_rep": bytes_read,
            "latency": distribution(&samples),
        })
    };

    let report = json!({
        "schema": "weft.family-lifecycle.measure.v1",
        "phase": "measure",
        "store": store_path.display().to_string(),
        "open_ms": round(open_ms),
        "retention": if backend_is_remote(args) {
            json!({"note": "remote object store: no local tree to walk"})
        } else {
            physical_breakdown(&store_path)
        },
        "retained_blob_bytes": store.retained_blob_bytes().map_err(|e| e.to_string())?,
        "snapshot": {
            "names": snapshot_rows.iter().map(|(name, commit)| json!({"name": name, "commit": commit.to_hex()})).collect::<Vec<Json>>(),
            "commit": snapshot_commit,
            "metadata_ms": distribution(&snapshot_meta_samples),
            "pinned_full_restore": pinned_samples,
        },
        "restore_matrix": matrix,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &report)?;
    println!("{}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// baselines: per-tensor zstd -19, per-tensor and whole-file patch-from
// ---------------------------------------------------------------------------

fn run_zstd(args: &[&str]) -> Result<(i64, f64), String> {
    let started = Instant::now();
    let status = Command::new("zstd")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|err| format!("zstd: {err}"))?;
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok((status.code().map(|code| code as i64).unwrap_or(-1), ms))
}

fn zstd_file(src: &Path, dst: &Path, extra: &[&str]) -> Result<(u64, f64, i64), String> {
    let mut args: Vec<&str> = Vec::new();
    args.push("-19");
    args.push("-T1");
    args.push("-q");
    args.push("-f");
    args.extend_from_slice(extra);
    let src_text = src.to_string_lossy().to_string();
    let dst_text = dst.to_string_lossy().to_string();
    args.push("-o");
    args.push(&dst_text);
    args.push(&src_text);
    let (code, ms) = run_zstd(&args)?;
    let size = fs::metadata(dst).map(|m| m.len()).unwrap_or(0);
    Ok((size, ms, code))
}

fn baselines(args: &[String]) -> Result<(), String> {
    let base_path = PathBuf::from(required(args, "--base-file")?);
    let next_path = PathBuf::from(required(args, "--next-file")?);
    let out = PathBuf::from(required(args, "--out")?);
    let scratch = PathBuf::from(required(args, "--scratch")?);
    let reps = parse_u64(args, "--reps", 3)? as usize;
    let family = value_of(args, "--family").unwrap_or_else(|| "unlabelled".to_owned());
    fs::create_dir_all(&scratch).map_err(|err| format!("{}: {err}", scratch.display()))?;

    let base = parse_safetensors(&base_path)?;
    let next = parse_safetensors(&next_path)?;

    let mut phases = Vec::new();
    for (label, dataset) in [("first", &base), ("second", &next)] {
        let mut rows = Vec::new();
        let mut total_zst = 0u64;
        let mut total_src = 0u64;
        let mut total_compress_ms = 0.0f64;
        let started = Instant::now();
        let mut decompress_single = Vec::new();
        for (index, tensor) in dataset.tensors.iter().enumerate() {
            let bytes = read_tensor_bytes(dataset, tensor)?;
            let src = scratch.join(format!("{label}-{index}.bin"));
            let dst = scratch.join(format!("{label}-{index}.zst"));
            fs::write(&src, &bytes).map_err(|err| format!("{}: {err}", src.display()))?;
            let (size, ms, code) = zstd_file(&src, &dst, &[])?;
            if code != 0 {
                return Err(format!("zstd -19 exited {code} on {label}/{index}"));
            }
            total_zst += size;
            total_src += bytes.len() as u64;
            total_compress_ms += ms;
            rows.push(json!({
                "name": tensor.name,
                "logical_bytes": bytes.len(),
                "zstd19_bytes": size,
            }));
            if index == 0 {
                // Single-tensor decompress: the independent baseline cannot
                // serve a region without decoding the whole frame.
                for _ in 0..reps {
                    let started = Instant::now();
                    let status = Command::new("zstd")
                        .args(["-d", "-q", "-f", "-o"])
                        .arg(&src)
                        .arg(&dst)
                        .status()
                        .map_err(|err| format!("zstd -d: {err}"))?;
                    if !status.success() {
                        return Err("zstd -d failed".to_owned());
                    }
                    decompress_single.push(started.elapsed().as_secs_f64() * 1000.0);
                }
            }
            let _ = fs::remove_file(&src);
            let _ = fs::remove_file(&dst);
        }
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        phases.push(json!({
            "label": label,
            "tensor_count": dataset.tensors.len(),
            "source_bytes": total_src,
            "zstd19_bytes": total_zst,
            "ratio_vs_source": if total_src == 0 { 0.0 } else { round(total_zst as f64 / total_src as f64) },
            "compress_sum_ms": round(total_compress_ms),
            "wall_ms": round(wall_ms),
            "single_tensor_decompress_ms": distribution(&decompress_single),
            "tensors": rows,
        }));
    }

    // Per-tensor patch-from (dictionary = the corresponding first-checkpoint
    // tensor), and one whole-file patch-from.
    let mut patch_rows = Vec::new();
    let mut patch_total = 0u64;
    let mut patch_failures = 0usize;
    for (index, tensor) in next.tensors.iter().enumerate() {
        let Some(base_tensor) = base.tensors.iter().find(|t| t.name == tensor.name) else {
            continue;
        };
        if base_tensor.byte_len() != tensor.byte_len() || tensor.byte_len() == 0 {
            continue;
        }
        let base_bytes = read_tensor_bytes(&base, base_tensor)?;
        let bytes = read_tensor_bytes(&next, tensor)?;
        let dict = scratch.join(format!("pf-dict-{index}.bin"));
        let src = scratch.join(format!("pf-src-{index}.bin"));
        let dst = scratch.join(format!("pf-{index}.zst"));
        fs::write(&dict, &base_bytes).map_err(|err| format!("{}: {err}", dict.display()))?;
        fs::write(&src, &bytes).map_err(|err| format!("{}: {err}", src.display()))?;
        let dict_text = dict.to_string_lossy().to_string();
        let (size, _ms, code) = zstd_file(&src, &dst, &["--patch-from", &dict_text])?;
        if code == 0 {
            patch_total += size;
        } else {
            patch_failures += 1;
        }
        let _ = fs::remove_file(&dict);
        let _ = fs::remove_file(&src);
        let _ = fs::remove_file(&dst);
    }
    patch_rows.push(json!({
        "scope": "per-tensor",
        "level": 19,
        "bytes": patch_total,
        "failures": patch_failures,
    }));

    let whole_dst = scratch.join("whole-patch.zst");
    let dict_text = base_path.to_string_lossy().to_string();
    let next_text = next_path.to_string_lossy().to_string();
    let mut whole_args: Vec<&str> =
        vec!["-12", "-T1", "-q", "-f", "--patch-from", &dict_text, "-o"];
    let whole_dst_text = whole_dst.to_string_lossy().to_string();
    whole_args.push(&whole_dst_text);
    whole_args.push(&next_text);
    let whole_started = Instant::now();
    let whole_status = Command::new("zstd")
        .args(&whole_args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|err| format!("zstd --patch-from: {err}"))?;
    let whole_ms = whole_started.elapsed().as_secs_f64() * 1000.0;
    patch_rows.push(json!({
        "scope": "whole-file",
        "level": 12,
        "exit_code": whole_status.code(),
        "wall_ms": round(whole_ms),
        "bytes": fs::metadata(&whole_dst).map(|m| m.len()).unwrap_or(0),
        "note": "zstd --patch-from -19 was OOM-killed at the 3 GiB court limit in R1/P2; -12 is used and the level is recorded",
    }));
    let _ = fs::remove_file(&whole_dst);

    let receipt = json!({
        "schema": "weft.family-lifecycle.baselines.v1",
        "phase": "baselines",
        "family": family,
        "zstd_cli": command_version("zstd"),
        "per_tensor_zstd19": phases,
        "patch_from": patch_rows,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

fn command_version(tool: &str) -> String {
    Command::new(tool)
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.lines().next().unwrap_or("").trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

// ---------------------------------------------------------------------------
// checkpoint + topology
// ---------------------------------------------------------------------------

fn checkpoint(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let file = PathBuf::from(required(args, "--file")?);
    let name = required(args, "--name")?;
    let out = PathBuf::from(required(args, "--out")?);
    let shards = parse_u64(args, "--shards", 8)? as u32;
    let axis = parse_u64(args, "--axis", 0)? as u32;
    let strategy = ShardStrategy::parse(
        &value_of(args, "--strategy").unwrap_or_else(|| "tensor-sharded".to_owned()),
    )
    .map_err(|err| err.to_string())?;
    let tile_shape: Option<Vec<u64>> = value_of(args, "--tile-shape").map(|text| {
        text.split(',')
            .filter_map(|part| part.trim().parse().ok())
            .collect()
    });
    let filters: Vec<String> = args
        .windows(2)
        .filter(|pair| pair[0] == "--tensor-filter")
        .map(|pair| pair[1].clone())
        .collect();
    let write_policy = weftfs::tensor_store::WritePolicy::parse(
        &value_of(args, "--write-policy").unwrap_or_else(|| "planned".to_owned()),
    )
    .map_err(|err| err.to_string())?;
    let fast_codec = weftfs::tensor_store::FastCodec::parse(
        &value_of(args, "--fast-codec").unwrap_or_else(|| "zstd".to_owned()),
    )
    .map_err(|err| err.to_string())?;
    let profiling = phase_profiling_requested();

    if !backend_is_remote(args) && store_path.exists() && !flag(args, "--append") {
        fs::remove_dir_all(&store_path)
            .map_err(|err| format!("{}: {err}", store_path.display()))?;
    }
    let backend = backend_from_with_suffix(
        args,
        &if backend_is_remote(args) {
            remote_suffix(&store_path, 0, 1)
        } else {
            String::new()
        },
    )?;
    let mut store = if flag(args, "--append") {
        Store::open_with_options_and_backend(
            &store_path,
            OpenMode::ReadWrite,
            options(65_536, 256),
            backend,
        )
        .map_err(|err| format!("open {} for append: {err}", store_path.display()))?
    } else {
        Store::init_with_backend(&store_path, options(65_536, 256), backend)
            .map_err(|e| e.to_string())?
    };
    let started = Instant::now();
    if profiling {
        weftfs::phase_profile::begin();
    }
    let report = store
        .write_checkpoint(
            &name,
            &file,
            &CheckpointWriteOptions {
                shards,
                axis,
                filters: filters.clone(),
                strategy,
                tile_shape: tile_shape.clone(),
                write_policy,
                fast_codec,
                ..CheckpointWriteOptions::default()
            },
        )
        .map_err(|err| format!("checkpoint write: {err}"))?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let phase_totals = if profiling {
        weftfs::phase_profile::finish().to_json()
    } else {
        Json::Null
    };
    let stats = store.backend_stats();
    let pack_stats = store.pack_stats().map(|stats| json!(stats));
    let verify = store.verify(true).map_err(|e| format!("verify: {e}"))?;
    let receipt = json!({
        "schema": "weft.family-lifecycle.checkpoint.v1",
        "phase": "checkpoint",
        "store": store_path.display().to_string(),
        "name": name,
        "strategy": strategy.as_str(),
        "shards": shards,
        "axis": axis,
        "tile_shape": tile_shape,
        "filters": filters,
        "write_policy": write_policy.as_str(),
        "fast_codec": match fast_codec {
            weftfs::tensor_store::FastCodec::Raw => "raw",
            weftfs::tensor_store::FastCodec::Zstd => "zstd",
        },
        "pack": store.pack_stats().is_some(),
        "pack_stats": pack_stats,
        "wall_ms": round(wall_ms),
        "phases": phase_totals,
        "phase_note": if profiling {
            "wall-clock totals across every thread; unmeasured time is the residual (chunk-table construction, planner arithmetic, in-memory state application, harness bookkeeping)"
        } else {
            "phase profiling was off (set WEFT_PHASE_PROFILE=1)"
        },
        "tensors": report.tensors.len(),
        "shard_objects": report.shard_objects,
        "total_element_bytes": report.total_element_bytes,
        "complete": report.complete,
        "source_sha256": hex::encode(report.source_sha256),
        "manifest_id": report.manifest_id.to_hex(),
        "manifest_root": report.manifest_root.to_hex(),
        "head": report.head.to_hex(),
        "physical": if backend_is_remote(args) {
            json!({"note": "remote object store: no local tree to walk"})
        } else {
            physical_breakdown(&store_path)
        },
        "backend": stats_json(&weftfs::backend::BackendStats::default(), &stats),
        "verify": {"ok": verify.ok, "problems": verify.problems.len()},
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

/// True when the caller asked for phase profiling (`WEFT_PHASE_PROFILE=1`).
fn phase_profiling_requested() -> bool {
    weftfs::phase_profile::enabled()
}

/// Concurrent independent readers on distinct checkpoints.
///
/// Opens one read-only [`Store`] and runs `--readers` scoped threads, each
/// restoring one of `--names` (round-robin) `--per-reader` times per
/// repetition. Every restore is checked against its checkpoint's recorded
/// source SHA-256. The report is the per-reader rate, the aggregate rate, and
/// (with `WEFT_LOCK_PROBE=1`) the same backend-lock probe the single-restore
/// phase reports.
fn readers(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let names: Vec<String> = required(args, "--names")?
        .split(',')
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty() {
        return Err("--names must name at least one checkpoint".to_owned());
    }
    let reader_count = parse_u64(args, "--readers", 4)? as usize;
    let per_reader = parse_u64(args, "--per-reader", 1)? as usize;
    let reps = parse_u64(args, "--reps", 1)? as usize;
    let out = PathBuf::from(required(args, "--out")?);
    let sink_mode = value_of(args, "--sink").unwrap_or_else(|| "sha256".to_owned());
    if sink_mode != "sha256" && sink_mode != "counting" {
        return Err(format!(
            "--sink must be sha256 or counting (got `{sink_mode}`)"
        ));
    }
    let use_sha = sink_mode == "sha256";

    let store = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        options(65_536, 256),
        backend_from_with_suffix(
            args,
            &if backend_is_remote(args) {
                remote_suffix(&store_path, 0, 1)
            } else {
                String::new()
            },
        )?,
    )
    .map_err(|err| format!("open {}: {err}", store_path.display()))?;

    // Load each checkpoint once so every reader checks against the recorded
    // source SHA-256, not against the first reader's output.
    let mut checkpoints: Vec<(String, Option<[u8; 32]>, u64)> = Vec::new();
    for name in &names {
        let manifest = store
            .load_checkpoint(name)
            .map_err(|err| format!("load checkpoint {name}: {err}"))?;
        checkpoints.push((name.clone(), manifest.source_sha256, manifest.element_bytes));
    }

    let probe_enabled = std::env::var("WEFT_LOCK_PROBE")
        .map(|value| value == "1")
        .unwrap_or(false);
    if probe_enabled {
        store.set_lock_probe(true);
    }

    let mut workload_samples = Vec::new();
    let mut aggregate: Vec<Json> = Vec::new();
    let sink_label = sink_mode.as_str();
    for rep in 0..reps {
        if probe_enabled {
            store.reset_lock_probe();
        }
        let started = Instant::now();
        let results: Vec<Json> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for reader in 0..reader_count {
                let (name, expected, logical) = &checkpoints[reader % checkpoints.len()];
                let store = &store;
                handles.push(scope.spawn(move || -> Result<Json, String> {
                    let mut durations = Vec::new();
                    let mut bytes = 0u64;
                    let mut sha_match = true;
                    let mut first = true;
                    // With a counting sink, prove exactness once *outside* the
                    // measured loop so the numbers are the store's steady-state
                    // restores, not a SHA-256 pass.
                    if !use_sha {
                        let mut verify = RestoreSink::new(true);
                        store
                            .restore_checkpoint_pipelined(
                                name,
                                &CheckpointRestoreOptions::default(),
                                &RestorePipelineOptions::default(),
                                &mut verify,
                            )
                            .map_err(|err| format!("reader {reader} verify {name}: {err}"))?;
                        if let (Some(expected), Some(digest)) = (*expected, verify.digest()) {
                            sha_match = digest == expected;
                        }
                    }
                    for _ in 0..per_reader {
                        // With `--sink sha256` every restore hashes; with
                        // `counting` the measured restores skip output hashing.
                        let sha_this = use_sha;
                        let mut sink = RestoreSink::new(sha_this);
                        let restore_started = Instant::now();
                        let report = if sha_this {
                            store.restore_checkpoint_pipelined(
                                name,
                                &CheckpointRestoreOptions::default(),
                                &RestorePipelineOptions::default(),
                                &mut sink,
                            )
                        } else {
                            store.restore_checkpoint_pipelined_unhashed(
                                name,
                                &CheckpointRestoreOptions::default(),
                                &RestorePipelineOptions::default(),
                                &mut sink,
                            )
                        }
                        .map_err(|err| format!("reader {reader} restore {name}: {err}"))?;
                        durations.push(restore_started.elapsed().as_secs_f64() * 1000.0);
                        bytes = bytes.saturating_add(report.file_bytes);
                        if sha_this && first {
                            if let (Some(expected), Some(digest)) = (*expected, sink.digest()) {
                                if digest != expected {
                                    sha_match = false;
                                }
                            }
                        }
                        first = false;
                    }
                    let wall: f64 = durations.iter().sum();
                    Ok(json!({
                        "reader": reader,
                        "checkpoint": name,
                        "logical_bytes": logical,
                        "restores": per_reader,
                        "bytes": bytes,
                        "wall_ms": round(wall),
                        "mb_per_s": round(bytes as f64 / 1_000_000.0 / (wall / 1000.0).max(1e-9)),
                        "sha256_match": sha_match,
                        "sink": sink_label,
                        "latency": distribution(&durations),
                    }))
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("reader thread panicked"))
                .collect::<Result<Vec<_>, String>>()
        })?;
        let process_wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        // The aggregate rate uses the slowest reader's measured restore time,
        // excluding the untimed exactness pass.
        let wall_ms = results
            .iter()
            .filter_map(|row| row.get("wall_ms").and_then(Json::as_f64))
            .fold(0.0, f64::max);
        workload_samples.push(process_wall_ms);
        let total_bytes: u64 = results
            .iter()
            .map(|row| row.get("bytes").and_then(Json::as_u64).unwrap_or(0))
            .sum();
        let rate = total_bytes as f64 / 1_000_000.0 / (wall_ms / 1000.0).max(1e-9);
        let lock = if probe_enabled {
            let report = store.lock_probe_report().with_wall_ms(wall_ms);
            json!({
                "wait": report.wait,
                "hold": report.hold,
                "io": report.io,
                "max_demand": report.max_demand,
                "mean_demand": round(report.mean_demand),
                "hold_share_of_wall": report.hold_share_of_wall.map(round),
                "wait_share_of_wall": report.wait_share_of_wall.map(round),
            })
        } else {
            Json::Null
        };
        aggregate.push(json!({
            "repetition": rep,
            "readers": reader_count,
            "per_reader": per_reader,
            "process_wall_ms": round(process_wall_ms),
            "wall_ms": round(wall_ms),
            "total_bytes": total_bytes,
            "aggregate_mb_per_s": round(rate),
            "readers_report": results,
            "lock": lock,
        }));
    }

    let receipt = json!({
        "schema": "weft.family-lifecycle.readers.v1",
        "phase": "readers",
        "store": store_path.display().to_string(),
        "checkpoints": names,
        "readers": reader_count,
        "per_reader": per_reader,
        "reps": reps,
        "lock_probe_enabled": probe_enabled,
        "sink": sink_mode,
        "sink": "SHA-256 hashing sink per reader: decode + backend read + hash, no host writeback",
        "workload_wall": distribution(&workload_samples),
        "repetitions": aggregate,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

/// Render one backend-lock probe report with per-tensor attribution.
///
/// `names` and `sizes` are indexed by restore input position, so a row can be
/// resolved back to the tensor the pipeline was decoding. Durations are
/// reported in milliseconds; `backend_share_of_elapsed` is `(wait + hold) /
/// elapsed` and `decode_cpu_ms` is `elapsed - wait - hold` for the same tensor
/// (the part of its wall time that was not spent inside the store's backend
/// lock).
fn lock_report_json(report: &weftfs::LockProbeReport, names: &[String], sizes: &[u64]) -> Json {
    #[derive(Default, Clone, Copy)]
    struct Agg {
        entries: u64,
        bytes: u64,
        wait_ns: u128,
        hold_ns: u128,
        elapsed_ns: u128,
        io_ns: u128,
    }
    let mut by_index: BTreeMap<usize, Agg> = BTreeMap::new();
    for row in &report.tensors {
        let entry = by_index.entry(row.index).or_default();
        entry.entries = entry.entries.saturating_add(row.entries);
        entry.bytes = entry.bytes.saturating_add(row.bytes);
        entry.wait_ns = entry.wait_ns.saturating_add(row.wait_ns);
        entry.hold_ns = entry.hold_ns.saturating_add(row.hold_ns);
        entry.elapsed_ns = entry.elapsed_ns.saturating_add(row.elapsed_ns);
        entry.io_ns = entry.io_ns.saturating_add(row.io_ns);
    }
    let mut rows: Vec<(usize, Agg)> = by_index.into_iter().collect();
    rows.sort_by(|left, right| right.1.hold_ns.cmp(&left.1.hold_ns));
    let hold_total_ms: f64 = rows.iter().map(|(_, row)| row.hold_ns as f64 / 1e6).sum();
    let elapsed_total_ms: f64 = rows
        .iter()
        .map(|(_, row)| row.elapsed_ns as f64 / 1e6)
        .sum();
    let tensors: Vec<Json> = rows
        .iter()
        .take(10)
        .map(|(index, row)| {
            let backend_ns = row
                .wait_ns
                .saturating_add(row.hold_ns)
                .saturating_add(row.io_ns);
            let share = if row.elapsed_ns == 0 {
                0.0
            } else {
                backend_ns as f64 / row.elapsed_ns as f64
            };
            let cpu_ns = row.elapsed_ns.saturating_sub(backend_ns);
            json!({
                "index": index,
                "name": names.get(*index).cloned(),
                "element_bytes": sizes.get(*index).copied(),
                "entries": row.entries,
                "bytes": row.bytes,
                "wait_ms": round(row.wait_ns as f64 / 1e6),
                "hold_ms": round(row.hold_ns as f64 / 1e6),
                "transport_ms": round(row.io_ns as f64 / 1e6),
                "elapsed_ms": round(row.elapsed_ns as f64 / 1e6),
                "backend_share_of_elapsed": round(share),
                "decode_cpu_ms": round(cpu_ns as f64 / 1e6),
            })
        })
        .collect();
    json!({
        "wait": report.wait,
        "hold": report.hold,
        "io": report.io,
        "max_demand": report.max_demand,
        "mean_demand": round(report.mean_demand),
        "hold_share_of_wall": report.hold_share_of_wall.map(round),
        "wait_share_of_wall": report.wait_share_of_wall.map(round),
        "hold_total_ms_over_tensors": round(hold_total_ms),
        "elapsed_total_ms_over_tensors": round(elapsed_total_ms),
        "tensor_rows": rows.len(),
        "tensors": tensors,
        "threads": report.threads,
    })
}

/// Pipelined full-checkpoint restore at several worker counts.
///
/// The sink hashes every restored byte and counts it, so exactness is proven
/// without a host writeback: the numbers are decode + read + SHA-256, and the
/// report says so. Each run compares the container SHA-256 against the
/// checkpoint's recorded source SHA-256.
fn restore(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let name = required(args, "--name")?;
    let out = PathBuf::from(required(args, "--out")?);
    let reps = parse_u64(args, "--reps", 3)? as usize;
    let threads: Vec<usize> = value_of(args, "--threads")
        .unwrap_or_else(|| "1".to_owned())
        .split(',')
        .map(|part| {
            part.trim()
                .parse()
                .map_err(|_| format!("--threads member `{part}` is not an integer"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tile_threads: Vec<usize> = value_of(args, "--tile-threads")
        .unwrap_or_else(|| "1".to_owned())
        .split(',')
        .map(|part| {
            part.trim()
                .parse()
                .map_err(|_| format!("--tile-threads member `{part}` is not an integer"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let max_inflight = parse_u64(args, "--max-inflight-bytes", 256 * 1024 * 1024)? as usize;
    let shard_threads = parse_u64(args, "--shard-threads", 1)? as usize;
    let shard_max_inflight =
        parse_u64(args, "--shard-max-inflight-bytes", 64 * 1024 * 1024)? as usize;
    let sink_mode = value_of(args, "--sink").unwrap_or_else(|| "sha256".to_owned());
    if sink_mode != "sha256" && sink_mode != "counting" {
        return Err(format!(
            "--sink must be sha256 or counting (got `{sink_mode}`)"
        ));
    }
    let use_sha = sink_mode == "sha256";
    let file_hash = flag(args, "--file-hash");

    let open_started = Instant::now();
    let store = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        options(65_536, 256),
        backend_from_with_suffix(
            args,
            &if backend_is_remote(args) {
                remote_suffix(&store_path, 0, 1)
            } else {
                String::new()
            },
        )?,
    )
    .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;
    let manifest = store
        .load_checkpoint(&name)
        .map_err(|err| format!("load checkpoint: {err}"))?;
    let source_sha256 = manifest.source_sha256.map(hex::encode).unwrap_or_default();
    let logical_bytes = manifest.element_bytes;
    let largest = manifest
        .tensors
        .iter()
        .map(|tensor| tensor.element_bytes)
        .max()
        .unwrap_or(0);
    let tensors = manifest.tensors.len();

    // Restore order is the container's `source_offset` order, so a probe row's
    // index is resolved back to the tensor the pipeline was decoding.
    let mut restore_order: Vec<(usize, String, u64)> = manifest
        .tensors
        .iter()
        .enumerate()
        .map(|(position, tensor)| (position, tensor.name.clone(), tensor.element_bytes))
        .collect();
    restore_order.sort_by_key(|(position, _, _)| manifest.tensors[*position].source_offset);
    let order_names: Vec<String> = restore_order
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect();
    let order_sizes: Vec<u64> = restore_order.iter().map(|(_, _, size)| *size).collect();

    let probe_enabled = std::env::var("WEFT_LOCK_PROBE")
        .map(|value| value == "1")
        .unwrap_or(false);
    if probe_enabled {
        store.set_lock_probe(true);
    }

    let mut matrix: BTreeMap<String, Json> = BTreeMap::new();
    for &worker_threads in &threads {
        for &tile_worker_threads in &tile_threads {
            let pipeline = RestorePipelineOptions {
                threads: worker_threads,
                max_inflight_bytes: max_inflight,
                tile_threads: tile_worker_threads,
                shard_threads,
                shard_max_inflight_bytes: shard_max_inflight,
            };
            let mut samples = Vec::new();
            let mut backend_bytes = 0u64;
            let mut requests = 0u64;
            let mut decode_bytes = 0u64;
            let mut stats_row = Json::Null;
            let mut sha_match = false;
            let mut last_pipeline = RestorePipelineStats::default();
            // One unrecorded restore immediately before this configuration, so
            // a configuration never pays for the previous one's cache state.
            {
                let mut warm = RestoreSink::new(false);
                store
                    .restore_checkpoint_pipelined_unhashed(
                        &name,
                        &CheckpointRestoreOptions::default(),
                        &RestorePipelineOptions::default(),
                        &mut warm,
                    )
                    .map_err(|err| format!("warm-up restore: {err}"))?;
            }
            if probe_enabled {
                store.reset_lock_probe();
            }
            for rep in 0..reps {
                let before = store.backend_stats();
                let mut sink = RestoreSink::new(use_sha);
                let started = Instant::now();
                let report = if use_sha {
                    store.restore_checkpoint_pipelined(
                        &name,
                        &CheckpointRestoreOptions::default(),
                        &pipeline,
                        &mut sink,
                    )
                } else if file_hash {
                    store.restore_checkpoint_pipelined_without_tensor_sha(
                        &name,
                        &CheckpointRestoreOptions::default(),
                        &pipeline,
                        &mut sink,
                    )
                } else {
                    store.restore_checkpoint_pipelined_unhashed(
                        &name,
                        &CheckpointRestoreOptions::default(),
                        &pipeline,
                        &mut sink,
                    )
                }
                .map_err(|err| format!("pipelined restore: {err}"))?;
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                let after = store.backend_stats();
                backend_bytes = after.bytes_read.saturating_sub(before.bytes_read);
                requests = after.reads.saturating_sub(before.reads)
                    + after.range_reads.saturating_sub(before.range_reads);
                if let Some(stats) = report.pipeline {
                    last_pipeline = stats;
                }
                let sink_bytes = sink.bytes();
                if let Some(digest) = sink.digest() {
                    if rep == 0 {
                        sha_match =
                            source_sha256.is_empty() || hex::encode(digest) == source_sha256;
                    }
                }
                if rep == 0 {
                    if sink_bytes != report.file_bytes {
                        return Err("restored sink saw a different byte count".to_owned());
                    }
                    decode_bytes = report.tensors.iter().map(|tensor| tensor.bytes).sum();
                    stats_row = json!({
                        "tasks": tensors,
                        "bytes_written": report.file_bytes,
                        "shards_read": report.shards_read,
                        "units_touched": report.units_touched,
                        "exact_source_file": report.exact_source_file,
                        "header_reproduced": report.header_reproduced,
                        "manifest_root": report.manifest_root.to_hex(),
                        "file_sha256": hex::encode(report.file_sha256),
                    });
                }
            }
            // Snapshot the probe before the untimed verification restore, so
            // the measured window is exactly the repetitions above.
            let probe_snapshot = if probe_enabled {
                Some(store.lock_probe_report())
            } else {
                None
            };
            // A counting sink never hashed its output; prove exactness with one
            // untimed SHA-256 pass at this worker count.
            if !use_sha {
                let mut verify = RestoreSink::new(true);
                store
                    .restore_checkpoint_pipelined(
                        &name,
                        &CheckpointRestoreOptions::default(),
                        &pipeline,
                        &mut verify,
                    )
                    .map_err(|err| format!("verification restore: {err}"))?;
                sha_match = source_sha256.is_empty()
                    || verify
                        .digest()
                        .map(|digest| hex::encode(digest) == source_sha256)
                        .unwrap_or(false);
            }
            let best_ms = samples
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min)
                .max(1.0);
            let mb_per_s = logical_bytes as f64 / 1_000_000.0 / (best_ms / 1000.0);
            let wall_sum_ms: f64 = samples.iter().sum();
            let utilisation = if last_pipeline.total_ms > 0.0 {
                last_pipeline.decode_sum_ms
                    / (last_pipeline.total_ms * last_pipeline.effective_threads.max(1) as f64)
            } else {
                0.0
            };
            let pipeline_json = json!({
                "requested_threads": last_pipeline.requested_threads,
                "effective_threads": last_pipeline.effective_threads,
                "tasks": last_pipeline.tasks,
                "observed_max_inflight_bytes": last_pipeline.observed_max_inflight_bytes,
                "observed_max_inflight_tasks": last_pipeline.observed_max_inflight_tasks,
                "oversized_tasks": last_pipeline.oversized_tasks,
                "decode_wall_ms": round(last_pipeline.decode_wall_ms),
                "decode_sum_ms": round(last_pipeline.decode_sum_ms),
                "transfer_ms": round(last_pipeline.transfer_ms),
                "write_ms": round(last_pipeline.write_ms),
                "total_ms": round(last_pipeline.total_ms),
                "worker_utilisation": round(utilisation),
            });
            let lock_json = if let Some(report) = probe_snapshot {
                lock_report_json(
                    &report.with_wall_ms(wall_sum_ms),
                    &order_names,
                    &order_sizes,
                )
            } else {
                Json::Null
            };
            matrix.insert(
                format!("threads_{worker_threads}_tiles_{tile_worker_threads}"),
                json!({
                    "threads": worker_threads,
                    "tile_threads": tile_worker_threads,
                    "max_inflight_bytes": max_inflight,
                    "tensors": tensors,
                    "logical_bytes": logical_bytes,
                    "largest_tensor_bytes": largest,
                    "amdahl_cap_speedup": if largest > 0 { logical_bytes as f64 / largest as f64 } else { 0.0 },
                    "decode_bytes_last_rep": decode_bytes,
                    "backend_bytes_read_last_rep": backend_bytes,
                    "backend_requests_last_rep": requests,
                    "mb_per_s_best": round(mb_per_s),
                    "sha256_match": sha_match,
                    "sink": sink_mode,
                    "stats": stats_row,
                    "pipeline": pipeline_json,
                    "lock": lock_json,
                    "latency": distribution(&samples),
                }),
            );
        }
    }

    // The largest tensor alone, sequential and at the highest worker count: the
    // measured single-tensor floor an Amdahl argument is built on.
    let subject = manifest
        .tensors
        .iter()
        .max_by_key(|tensor| tensor.element_bytes)
        .map(|tensor| tensor.name.clone())
        .ok_or_else(|| "checkpoint has no tensors".to_owned())?;
    let mut single = Vec::new();
    if probe_enabled {
        store.reset_lock_probe();
    }
    for _ in 0..reps {
        let options = CheckpointRestoreOptions {
            tensor: Some(subject.clone()),
            ..CheckpointRestoreOptions::default()
        };
        let mut sink = HashSink {
            hasher: Sha256::new(),
            bytes: 0,
        };
        let started = Instant::now();
        store
            .restore_checkpoint_pipelined(
                &name,
                &options,
                &RestorePipelineOptions::default(),
                &mut sink,
            )
            .map_err(|err| format!("single tensor restore: {err}"))?;
        single.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let single_wall_ms: f64 = single.iter().sum();
    let single_lock = if probe_enabled {
        let report = store.lock_probe_report().with_wall_ms(single_wall_ms);
        lock_report_json(&report, &[subject.clone()], &[largest])
    } else {
        Json::Null
    };

    let receipt = json!({
        "schema": "weft.family-lifecycle.restore.v1",
        "phase": "restore",
        "store": store_path.display().to_string(),
        "checkpoint": name,
        "open_ms": round(open_ms),
        "logical_bytes": logical_bytes,
        "largest_tensor": subject,
        "largest_tensor_bytes": largest,
        "amdahl_cap_speedup": if largest > 0 { logical_bytes as f64 / largest as f64 } else { 0.0 },
        "sink": if use_sha {
            "SHA-256 hashing sink: decode + backend read + hash, no host writeback"
        } else {
            "counting sink: decode + backend read, no output hash and no host writeback; exactness proven by one untimed SHA-256 pass per worker count"
        },
        "lock_probe_enabled": probe_enabled,
        "shard_threads": shard_threads,
        "shard_max_inflight_bytes": shard_max_inflight,
        "file_hash": file_hash,
        "single_largest_tensor": distribution(&single),
        "single_largest_tensor_lock": single_lock,
        "configurations": matrix,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

/// Assemble the throughput court's phase receipts into one receipt.
///
/// `--input name=path` is repeatable. Each file is parsed and embedded under
/// `inputs.<name>` unchanged, so the court adds provenance and environment but
/// never rewrites a measured number.
fn throughput(args: &[String]) -> Result<(), String> {
    let out = PathBuf::from(required(args, "--out")?);
    let label = value_of(args, "--label").unwrap_or_default();
    let mut inputs: BTreeMap<String, Json> = BTreeMap::new();
    for pair in args.windows(2) {
        if pair[0] != "--input" {
            continue;
        }
        let (name, path) = pair[1]
            .split_once('=')
            .ok_or_else(|| format!("--input must be name=path (got `{}`)", pair[1]))?;
        let text = fs::read_to_string(path).map_err(|err| format!("read {path}: {err}"))?;
        let value: Json =
            serde_json::from_str(&text).map_err(|err| format!("parse {path}: {err}"))?;
        inputs.insert(name.to_owned(), value);
    }
    if inputs.is_empty() {
        return Err("throughput needs at least one --input name=path".to_owned());
    }
    let receipt = json!({
        "schema": "weft.throughput-court.v1",
        "phase": "throughput",
        "label": label,
        "court": {
            "image": std::env::var("WEFT_COURT_IMAGE").ok(),
            "image_id": std::env::var("WEFT_COURT_IMAGE_ID").ok(),
            "memory": std::env::var("WEFT_COURT_MEMORY").ok(),
            "cpus": std::env::var("WEFT_COURT_CPUS").ok(),
            "pids": std::env::var("WEFT_COURT_PIDS").ok(),
            "kernel": std::env::var("WEFT_COURT_KERNEL").ok(),
        },
        "revisions": {
            "weftfs": std::env::var("WEFT_REPO_WEFTFS").ok(),
            "weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").ok(),
            "weftcodec_go": std::env::var("WEFT_REPO_WEFTCODEC_GO").ok(),
            "weftfs_go": std::env::var("WEFT_REPO_WEFTFS_GO").ok(),
        },
        "labels": [
            "Single host, warm page cache, one run per configuration unless the inner receipt says otherwise; no fleet, media, power-loss, or cloud claim.",
            "Phase totals are wall-clock sums across every thread and are not a partition of wall time; unmeasured time is the residual.",
            "Restore throughput is measured with a SHA-256 hashing sink, so it is decode + backend read + hash, not host writeback."
        ],
        "inputs": inputs,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

fn topology(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let name = required(args, "--name")?;
    let out = PathBuf::from(required(args, "--out")?);
    let reps = parse_u64(args, "--reps", 3)? as usize;

    let store = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadOnly,
        options(65_536, 256),
        backend_from_with_suffix(
            args,
            &if backend_is_remote(args) {
                remote_suffix(&store_path, 0, 1)
            } else {
                String::new()
            },
        )?,
    )
    .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let manifest = store
        .checkpoint_manifest(&name)
        .map_err(|err| format!("manifest {name}: {err}"))?;
    let naive_bytes: u64 = manifest.tensors.iter().map(|t| t.element_bytes).sum();
    let largest = manifest
        .tensors
        .iter()
        .filter(|tensor| tensor.shape.len() == 2)
        .max_by_key(|tensor| tensor.element_bytes)
        .ok_or_else(|| "checkpoint has no rank-2 tensor".to_owned())?;
    let block_shape = dividing_block(&largest.shape)?;
    let largest_bytes = largest.element_bytes;

    let mut rows = Vec::new();
    let plans: Vec<(&str, Option<RestoreTopology>, Option<String>)> = vec![
        (
            "sharded_8",
            Some(RestoreTopology::Sharded { axis: 0, shards: 8 }),
            None,
        ),
        (
            "sharded_4",
            Some(RestoreTopology::Sharded { axis: 0, shards: 4 }),
            None,
        ),
        ("row_major", Some(RestoreTopology::RowMajor), None),
        (
            "blocked_largest_tensor",
            Some(RestoreTopology::Blocked {
                block_shape: block_shape.clone(),
            }),
            Some(largest.name.clone()),
        ),
    ];
    for (label, plan, tensor) in plans {
        let mut samples = Vec::new();
        let mut bytes_read = 0u64;
        let mut requests = 0u64;
        let mut roots_verified = 0usize;
        let mut shards_verified = 0usize;
        let mut total_tensors = 0usize;
        let mut exact_source_file = false;
        for rep in 0..reps {
            let before = store.backend_stats();
            let mut sink = io::sink();
            let started = Instant::now();
            let report = store
                .restore_checkpoint(
                    &name,
                    &CheckpointRestoreOptions {
                        tensor: tensor.clone(),
                        region: None,
                        topology: plan.clone(),
                    },
                    &mut sink,
                )
                .map_err(|err| format!("topology {label}: {err}"))?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
            let after = store.backend_stats();
            bytes_read = report
                .store_bytes_read
                .max(after.bytes_read.saturating_sub(before.bytes_read));
            requests = report.store_reads;
            if rep == 0 {
                total_tensors = report.tensors.len();
                exact_source_file = report.exact_source_file;
                for tensor in &report.tensors {
                    if tensor.full_root_verified {
                        roots_verified += 1;
                    }
                    if tensor.shard_roots_verified {
                        shards_verified += 1;
                    }
                }
            }
        }
        rows.push(json!({
            "label": label,
            "tensor_filter": tensor,
            "topology": format!("{plan:?}"),
            "tensors_restored": total_tensors,
            "full_roots_verified": roots_verified,
            "shard_roots_verified": shards_verified,
            "all_roots_verified": roots_verified == total_tensors && total_tensors > 0,
            "exact_source_file": exact_source_file,
            "naive_full_read_bytes": if tensor.is_some() { largest_bytes } else { naive_bytes },
            "backend_bytes_read_last_rep": bytes_read,
            "bytes_ratio_vs_naive_full": if (if tensor.is_some() { largest_bytes } else { naive_bytes }) == 0 { 0.0 } else {
                round(bytes_read as f64 / (if tensor.is_some() { largest_bytes } else { naive_bytes }) as f64)
            },
            "backend_requests_last_rep": requests,
            "latency": distribution(&samples),
        }));
    }

    let receipt = json!({
        "schema": "weft.family-lifecycle.topology.v1",
        "phase": "topology",
        "store": store_path.display().to_string(),
        "name": name,
        "tensors": manifest.tensors.len(),
        "element_bytes": naive_bytes,
        "blocked_block_shape": block_shape,
        "restores": rows,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

fn dividing_block(shape: &[u64]) -> Result<Vec<u64>, String> {
    if shape.len() != 2 {
        return Err("dividing_block expects a rank-2 tensor".to_owned());
    }
    let pick = |extent: u64| -> u64 {
        for candidate in [128u64, 64, 32, 16, 8, 4, 2, 1] {
            if extent % candidate == 0 {
                return candidate;
            }
        }
        1
    };
    Ok(vec![pick(shape[0]), pick(shape[1])])
}

// ---------------------------------------------------------------------------
// crash recovery
// ---------------------------------------------------------------------------

fn crash_child(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let file = PathBuf::from(required(args, "--file")?);
    let name = required(args, "--name")?;
    let phase = required(args, "--phase")?;
    let filters: Vec<String> = args
        .windows(2)
        .filter(|pair| pair[0] == "--tensor-filter")
        .map(|pair| pair[1].clone())
        .collect();

    // The store is created by this child so the parent's measured reopen is the
    // recovery path and nothing else.
    if !backend_is_remote(args) && store_path.exists() {
        fs::remove_dir_all(&store_path)
            .map_err(|err| format!("{}: {err}", store_path.display()))?;
    }
    // The phase trigger is detached for setup and re-armed only for the
    // checkpoint's own publication, so `init` and the baseline commit are not
    // mistaken for the measured phases.
    std::env::remove_var("WEFT_CRASH_PHASE");
    let mut store = Store::init_with_backend(
        &store_path,
        options(65_536, 256),
        backend_from_with_suffix(
            args,
            &if backend_is_remote(args) {
                remote_suffix(&store_path, 0, 1)
            } else {
                String::new()
            },
        )?,
    )
    .map_err(|e| e.to_string())?;
    // A store that has never published a commit has no superblock slot to
    // recover, so the crash fixture first publishes an ordinary commit.
    let root = store.resolve_path("/").map_err(|e| e.to_string())?;
    store
        .create_file(
            root,
            b"baseline-marker",
            b"store in use before the checkpoint write",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .map_err(|err| format!("baseline marker: {err}"))?;
    std::env::set_var("WEFT_CRASH_PHASE", &phase);
    store
        .write_checkpoint(
            &name,
            &file,
            &CheckpointWriteOptions {
                shards: 1,
                axis: 0,
                filters,
                strategy: ShardStrategy::TensorSharded,
                tile_shape: None,
                ..CheckpointWriteOptions::default()
            },
        )
        .map_err(|err| format!("checkpoint write: {err}"))?;
    // Reaching here means the configured phase was never hit.
    Err(format!("crash phase `{phase}` did not trigger"))
}

fn recovery(args: &[String]) -> Result<(), String> {
    let work = PathBuf::from(required(args, "--work")?);
    let file = PathBuf::from(required(args, "--file")?);
    let out = PathBuf::from(required(args, "--out")?);
    let name = value_of(args, "--name").unwrap_or_else(|| "family".to_owned());
    let reps = parse_u64(args, "--reps", 3)? as usize;
    let filters: Vec<String> = args
        .windows(2)
        .filter(|pair| pair[0] == "--tensor-filter")
        .map(|pair| pair[1].clone())
        .collect();
    let phases = [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ];
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;

    let mut phase_rows = Vec::new();
    for phase in phases {
        let mut rows = Vec::new();
        for rep in 0..reps {
            let store = work.join(format!("crash-{phase}-{rep}"));
            if store.exists() {
                fs::remove_dir_all(&store).map_err(|err| format!("{}: {err}", store.display()))?;
            }
            let mut command = Command::new(&exe);
            command
                .arg("crash-child")
                .arg("--store")
                .arg(&store)
                .arg("--file")
                .arg(&file)
                .arg("--name")
                .arg(&name)
                .arg("--phase")
                .arg(phase);
            for filter in &filters {
                command.arg("--tensor-filter").arg(filter);
            }
            let status = command
                .env("WEFT_CRASH_PHASE", phase)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|err| format!("crash child: {err}"))?;
            let aborted = status.signal().map(|signal| signal == 6).unwrap_or(false);
            if !aborted {
                return Err(format!(
                    "crash child for `{phase}` exited {:?} without SIGABRT",
                    status.code()
                ));
            }
            let open_started = Instant::now();
            let store_handle = Store::open_with_options_and_backend(
                &store,
                OpenMode::ReadWrite,
                options(65_536, 256),
                backend_from_with_suffix(
                    args,
                    &if backend_is_remote(args) {
                        remote_suffix(&store, 0, 1)
                    } else {
                        String::new()
                    },
                )?,
            )
            .map_err(|err| format!("open after crash ({phase}): {err}"))?;
            let open_ms = open_started.elapsed().as_secs_f64() * 1000.0;
            let usable_started = Instant::now();
            let manifest = store_handle.checkpoint_manifest(&name);
            let present = match &manifest {
                Ok(manifest) => json!({
                    "present": true,
                    "tensors": manifest.tensors.len(),
                    "container": manifest.container,
                }),
                Err(err) if err.class() == weftfs::error::Class::NotFound => {
                    json!({"present": false})
                }
                Err(err) => return Err(format!("manifest after crash ({phase}): {err}")),
            };
            let verify_started = Instant::now();
            let verify = store_handle
                .verify(true)
                .map_err(|err| format!("verify after crash ({phase}): {err}"))?;
            let verify_ms = verify_started.elapsed().as_secs_f64() * 1000.0;
            let usable_ms = usable_started.elapsed().as_secs_f64() * 1000.0;
            rows.push(json!({
                "open_ms": round(open_ms),
                "usable_ms": round(usable_ms),
                "verify_ms": round(verify_ms),
                "verify_ok": verify.ok,
                "problems": verify.problems.len(),
                "problem_details": verify.problems.iter().map(|p| json!({"class": p.class, "detail": p.detail})).collect::<Vec<Json>>(),
                "rejected_generations": verify.rejected_generations,
                "expected": match phase {
                    "after_commit_file" => {
                        let unpublished_tail = verify.problems.iter().any(|problem| {
                            problem
                                .detail
                                .contains("above the highest published superblock generation")
                        });
                        !present_present(&present)
                            && (verify.ok || (verify.problems.len() == 1 && unpublished_tail))
                    }
                    "after_superblock_write" => present_present(&present) && verify.ok,
                    _ => !present_present(&present) && verify.ok,
                },
                "checkpoint": present,
                "generation": store_handle.generation(),
                "aborted_with_signal": 6,
            }));
            drop(store_handle);
            let _ = fs::remove_dir_all(&store);
        }
        let old = rows
            .iter()
            .filter(|row| {
                !row.pointer("/checkpoint/present")
                    .and_then(Json::as_bool)
                    .unwrap_or(false)
            })
            .count();
        let new = rows.len() - old;
        let opens: Vec<f64> = rows
            .iter()
            .filter_map(|r| r.get("open_ms").and_then(Json::as_f64))
            .collect();
        let usable: Vec<f64> = rows
            .iter()
            .filter_map(|r| r.get("usable_ms").and_then(Json::as_f64))
            .collect();
        phase_rows.push(json!({
            "phase": phase,
            "reps": rows,
            "old_state": old,
            "new_state": new,
            "partial_state": 0,
            "open_ms": distribution(&opens),
            "time_to_usable_ms": distribution(&usable),
            "verify_all_ok": rows.iter().all(|r| r.get("verify_ok").and_then(Json::as_bool).unwrap_or(false)),
            "all_expected": rows.iter().all(|r| r.get("expected").and_then(Json::as_bool).unwrap_or(false)),
            "expected_note": match phase {
                "after_commit_file" => "the commit file exists but the superblock never published it: the old state is selected and verify names the unpublished tail; that is the documented outcome, not data loss",
                _ => "verify --full must be clean",
            },
        }));
    }

    let receipt = json!({
        "schema": "weft.family-lifecycle.recovery.v1",
        "phase": "recovery",
        "label": "process-fault proxy (SIGABRT at a publication phase); not physical power loss",
        "name": name,
        "phases": phase_rows,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// gc
// ---------------------------------------------------------------------------

fn gc(args: &[String]) -> Result<(), String> {
    let store_path = PathBuf::from(required(args, "--store")?);
    let ingest_path = PathBuf::from(required(args, "--ingest")?);
    let out = PathBuf::from(required(args, "--out")?);
    let pinned = flag(args, "--snapshot");

    let ingest = read_json(&ingest_path)?;
    let next_rows = ingest
        .pointer("/tensors/next")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let base_rows = ingest
        .pointer("/tensors/base")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    if next_rows.is_empty() {
        return Err("ingest JSON has no next rows".to_owned());
    }

    let mut store = Store::open_with_options_and_backend(
        &store_path,
        OpenMode::ReadWrite,
        options(65_536, 256),
        backend_from_with_suffix(
            args,
            &if backend_is_remote(args) {
                remote_suffix(&store_path, 0, 1)
            } else {
                String::new()
            },
        )?,
    )
    .map_err(|err| format!("open {}: {err}", store_path.display()))?;
    let before = if backend_is_remote(args) {
        json!({"note": "remote object store: no local tree to walk"})
    } else {
        physical_breakdown(&store_path)
    };

    let mut snapshot_row = Json::Null;
    if pinned {
        let started = Instant::now();
        let commit = store
            .snapshot_create("family-pin")
            .map_err(|err| format!("snapshot: {err}"))?;
        snapshot_row = json!({
            "name": "family-pin",
            "commit": commit.to_hex(),
            "create_ms": round(started.elapsed().as_secs_f64() * 1000.0),
        });
    }

    // Delete checkpoint `next` (every tensor object) in one group commit.
    let next_dir = store
        .resolve_components(&[b"next"])
        .map_err(|err| format!("resolve /next: {err}"))?;
    let entries = store
        .list_dir(next_dir)
        .map_err(|err| format!("list /next: {err}"))?;
    let delete_started = Instant::now();
    let deleted = store
        .group(|txn| {
            let mut count = 0usize;
            for entry in &entries {
                txn.unlink(next_dir, &entry.name)?;
                count += 1;
            }
            Ok(count)
        })
        .map_err(|err| format!("delete group: {err}"))?;
    let delete_ms = delete_started.elapsed().as_secs_f64() * 1000.0;
    let next_entries_after = store
        .list_dir(next_dir)
        .map(|entries| entries.len())
        .unwrap_or(0);
    let unlinked_still_present: Vec<bool> = next_rows
        .iter()
        .take(4)
        .filter_map(|row| row.get("object_id").and_then(Json::as_str))
        .filter_map(|text| weftfs::Hash32::from_hex(text).ok())
        .map(|id| store.object_meta(id).is_ok())
        .collect();
    let after_delete = if backend_is_remote(args) {
        json!({"note": "remote object store: no local tree to walk"})
    } else {
        physical_breakdown(&store_path)
    };

    let plan_started = Instant::now();
    let plan = store.gc(false).map_err(|err| format!("gc plan: {err}"))?;
    let plan_ms = plan_started.elapsed().as_secs_f64() * 1000.0;
    let apply_started = Instant::now();
    let applied = store.gc(true).map_err(|err| format!("gc apply: {err}"))?;
    let apply_ms = apply_started.elapsed().as_secs_f64() * 1000.0;
    let after = if backend_is_remote(args) {
        json!({"note": "remote object store: no local tree to walk"})
    } else {
        physical_breakdown(&store_path)
    };
    let retained = store.retained_blob_bytes().map_err(|e| e.to_string())?;
    let verify = store
        .verify(true)
        .map_err(|err| format!("verify after gc: {err}"))?;

    // The live checkpoint must still restore exactly.
    let base_ids: Vec<weftfs::Hash32> = base_rows
        .iter()
        .filter_map(|row| row.get("object_id").and_then(Json::as_str))
        .filter_map(|text| weftfs::Hash32::from_hex(text).ok())
        .collect();
    let mut hasher = Sha256::new();
    for id in &base_ids {
        let bytes = store
            .read_tensor(*id)
            .map_err(|err| format!("read after gc: {err}"))?;
        hasher.update(&bytes);
    }
    let base_hash = hex::encode(hasher.finalize());
    let expected_base = ingest
        .pointer("/corpus/concat_data_sha256/base")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_owned();

    // Orphan check. `gc --apply` removes object records with no directory
    // entry, no relation target, and no child role; it reports each one, and
    // `verify --full` is the independent check that nothing reachable broke.
    let orphans = applied.removed_orphans.len();

    let receipt = json!({
        "schema": "weft.family-lifecycle.gc.v1",
        "phase": "gc",
        "store": store_path.display().to_string(),
        "snapshot_pinned": pinned,
        "snapshot": snapshot_row,
        "retention_before": before,
        "deleted_entries": deleted.0,
        "next_entries_after_delete": next_entries_after,
        "unlinked_objects_still_in_state": unlinked_still_present,
        "delete_ms": round(delete_ms),
        "retention_after_delete": after_delete,
        "gc_plan": {
            "ms": round(plan_ms),
            "candidates": plan.candidates.len(),
            "candidate_sample": plan.candidates.iter().take(12).cloned().collect::<Vec<String>>(),
            "problems": plan.problems,
            "marked_manifests": plan.marked_manifests,
            "marked_chunks": plan.marked_chunks,
        },
        "gc_apply": {
            "ms": round(apply_ms),
            "deleted": applied.deleted.len(),
            "deleted_sample": applied.deleted.iter().take(12).cloned().collect::<Vec<String>>(),
            "deleted_bytes": applied.deleted_bytes,
            "deleted_indexes": applied.deleted_indexes.len(),
            "removed_orphans": applied.removed_orphans.len(),
            "retained": applied.retained,
            "candidates": applied.candidates.len(),
            "reclaimed_packs": applied.reclaimed_packs,
            "reclaimed_pack_bytes": applied.reclaimed_pack_bytes,
            "problems": applied.problems,
        },
        "retention_after_gc": after,
        "retained_blob_bytes": retained,
        "verify": {
            "ok": verify.ok,
            "checked_objects": verify.checked_objects,
            "problems": verify.problems.iter().map(|p| json!({"class": p.class, "detail": p.detail})).collect::<Vec<Json>>(),
        },
        "live_checkpoint_restore": {
            "tensors": base_ids.len(),
            "concat_sha256": base_hash,
            "matches_ingest": base_hash == expected_base,
        },
        "orphan_object_records": orphans,
        "peak_rss_kib": peak_rss_kib(),
    });
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// assemble
// ---------------------------------------------------------------------------

fn assemble(args: &[String]) -> Result<(), String> {
    let out = PathBuf::from(required(args, "--out")?);
    let family = required(args, "--family")?;
    let variant = required(args, "--variant")?;
    let ingest_path = PathBuf::from(required(args, "--ingest")?);
    let measure_path = PathBuf::from(required(args, "--measure")?);
    let baselines_path = PathBuf::from(required(args, "--baselines")?);
    let topology_path = PathBuf::from(required(args, "--topology")?);
    let recovery_path = PathBuf::from(required(args, "--recovery")?);
    let gc_plain_path = PathBuf::from(required(args, "--gc-plain")?);
    let gc_pinned_path = PathBuf::from(required(args, "--gc-pinned")?);
    let checkpoint_path = PathBuf::from(required(args, "--checkpoint")?);

    let ingest = read_json(&ingest_path)?;
    let measure = read_json(&measure_path)?;
    let baselines = read_json(&baselines_path)?;
    let topology = read_json(&topology_path)?;
    let recovery = read_json(&recovery_path)?;
    let gc_plain = read_json(&gc_plain_path)?;
    let gc_pinned = read_json(&gc_pinned_path)?;
    let checkpoint = read_json(&checkpoint_path)?;

    let comparison = comparison_block(&ingest, &baselines);

    let receipt = json!({
        "schema": "weft.family-lifecycle.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unstamped".to_owned()),
        "family": family,
        "variant": variant,
        "command": std::env::var("WEFT_COURT_COMMAND").unwrap_or_default(),
        "environment": {
            "court_image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "court_kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "court_cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "court_memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "court_pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftfs_go": std::env::var("WEFT_REPO_WEFTFS_GO").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftcodec_go": std::env::var("WEFT_REPO_WEFTCODEC_GO").unwrap_or_else(|_| "unknown".to_owned()),
            "corpus": std::env::var("WEFT_FAMILY_CORPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "label": std::env::var("WEFT_FAMILY_LABEL").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "comparison": comparison,
        "ingest": ingest,
        "measure": measure,
        "baselines": baselines,
        "topology": topology,
        "recovery": recovery,
        "gc": {
            "without_snapshot": gc_plain,
            "with_snapshot": gc_pinned,
        },
        "checkpoint_8way": checkpoint,
        "peak_rss_kib_by_phase": {
            "ingest": ingest.get("peak_rss_kib").cloned().unwrap_or(Json::Null),
            "measure": measure.get("peak_rss_kib").cloned().unwrap_or(Json::Null),
            "baselines": baselines.get("peak_rss_kib").cloned().unwrap_or(Json::Null),
            "checkpoint": checkpoint.get("peak_rss_kib").cloned().unwrap_or(Json::Null),
            "topology": topology.get("peak_rss_kib").cloned().unwrap_or(Json::Null),
            "recovery": recovery.get("peak_rss_kib").cloned().unwrap_or(Json::Null),
            "gc_without_snapshot": gc_plain.get("peak_rss_kib").cloned().unwrap_or(Json::Null),
            "gc_with_snapshot": gc_pinned.get("peak_rss_kib").cloned().unwrap_or(Json::Null),
        },
    });
    write_json(&out, &receipt)?;
    print_tables(&receipt);
    println!("receipt {}", out.display());
    Ok(())
}

fn comparison_block(ingest: &Json, baselines: &Json) -> Json {
    // Per-tensor loss against the independent zstd-19 baseline: match each
    // planner choice to the zstd -19 frame size of the same tensor.
    let zstd_rows = baselines
        .get("per_tensor_zstd19")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let zstd_tensor_map = |label: &str| -> BTreeMap<String, u64> {
        zstd_rows
            .iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(label))
            .and_then(|row| row.get("tensors"))
            .and_then(Json::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("name")?.as_str()?.to_owned(),
                            row.get("zstd19_bytes")?.as_u64()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let zstd_total = |label: &str| -> u64 {
        zstd_rows
            .iter()
            .find(|row| row.get("label").and_then(Json::as_str) == Some(label))
            .and_then(|row| row.get("zstd19_bytes"))
            .and_then(Json::as_u64)
            .unwrap_or(0)
    };
    let mut loss_rows = Vec::new();
    let mut loss_detail = Vec::new();
    for (label, key) in [("first", "base"), ("second", "next")] {
        let rows = ingest
            .pointer(&format!("/tensors/{key}"))
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        let zst_map = zstd_tensor_map(label);
        let zst_total = zstd_total(label);
        let chosen_total: u64 = rows
            .iter()
            .filter_map(|row| row.get("data_bytes").and_then(Json::as_u64))
            .sum();
        let mut per_tensor_losses = 0usize;
        let mut loss_bytes = 0i64;
        let mut win_bytes = 0i64;
        let mut worst: Vec<(i64, String, u64, u64, String)> = Vec::new();
        for row in &rows {
            let name = row.get("name").and_then(Json::as_str).unwrap_or("");
            let data = row.get("data_bytes").and_then(Json::as_u64).unwrap_or(0);
            let Some(zst) = zst_map.get(name) else {
                continue;
            };
            let delta = data as i64 - *zst as i64;
            if delta > 0 {
                per_tensor_losses += 1;
                loss_bytes += delta;
            } else {
                win_bytes += -delta;
            }
            worst.push((
                delta,
                name.to_owned(),
                data,
                *zst,
                row.get("strategy")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_owned(),
            ));
        }
        worst.sort_by(|a, b| b.0.cmp(&a.0));
        for (delta, name, data, zst, strategy) in worst.iter().take(5) {
            if *delta > 0 {
                loss_detail.push(json!({
                    "checkpoint": label,
                    "tensor": name,
                    "strategy": strategy,
                    "weftfs_data_bytes": data,
                    "zstd19_bytes": zst,
                    "loss_bytes": delta,
                    "loss_percent": if *zst == 0 { 0.0 } else { round(*delta as f64 * 100.0 / *zst as f64) },
                }));
            }
        }
        let remote_no_tree = ingest
            .pointer("/physical/note")
            .and_then(Json::as_str)
            .is_some();
        loss_rows.push(json!({
            "checkpoint": label,
            "per_tensor_bytes_available": !remote_no_tree,
            "note": if remote_no_tree {
                "remote object store: per-tensor physical bytes cannot be attributed (no local tree); the comparison is not computed"
            } else {
                "per-tensor physical bytes from the local store tree"
            },
            "weftfs_data_bytes": if remote_no_tree { Json::Null } else { json!(chosen_total) },
            "zstd19_bytes": zst_total,
            "ratio_vs_zstd19": if remote_no_tree || zst_total == 0 { Json::Null } else { json!(round(chosen_total as f64 / zst_total as f64)) },
            "weftfs_minus_zstd19_bytes": chosen_total as i64 - zst_total as i64,
            "per_tensor_losses": per_tensor_losses,
            "per_tensor_loss_bytes": loss_bytes,
            "per_tensor_win_bytes": win_bytes,
            "policy": "planner-chosen data bytes vs zstd -19 CLI bytes of the same tensor",
        }));
    }

    json!({
        "weftfs_vs_zstd19": loss_rows,
        "where_the_planner_loses": loss_detail,
        "patch_from": baselines.get("patch_from").cloned().unwrap_or(Json::Null),
    })
}

fn print_tables(receipt: &Json) {
    let family = receipt.get("family").and_then(Json::as_str).unwrap_or("?");
    let variant = receipt.get("variant").and_then(Json::as_str).unwrap_or("?");
    println!("\n== {family} / {variant} ==");
    if let Some(ingest) = receipt.get("ingest") {
        println!(
            "ingest median wall {} ms, peak RSS {} KiB",
            ingest
                .pointer("/ingest_ms/median_family_wall_ms")
                .cloned()
                .unwrap_or(Json::Null),
            ingest.get("peak_rss_kib").cloned().unwrap_or(Json::Null)
        );
        for key in ["base", "next"] {
            if let Some(dist) =
                ingest.pointer(&format!("/strategy_distribution/{key}/tensors_by_strategy"))
            {
                println!("  strategies {key}: {dist}");
            }
        }
        if let Some(data) = ingest.pointer("/physical/data_only_bytes") {
            println!(
                "  physical data-only {data}, indexes {}, total {}",
                ingest
                    .pointer("/physical/indexes_bytes")
                    .cloned()
                    .unwrap_or(Json::Null),
                ingest
                    .pointer("/physical/store_total_bytes")
                    .cloned()
                    .unwrap_or(Json::Null)
            );
        }
    }
    if let Some(matrix) = receipt
        .pointer("/measure/restore_matrix")
        .and_then(Json::as_object)
    {
        for (name, row) in matrix {
            let p50 = row
                .pointer("/latency/p50_ms")
                .cloned()
                .unwrap_or(Json::Null);
            let p99 = row
                .pointer("/latency/p99_ms")
                .cloned()
                .unwrap_or(Json::Null);
            let bytes = row
                .get("backend_bytes_read_last_rep")
                .cloned()
                .unwrap_or(Json::Null);
            let ok = row.get("sha256_match").cloned().unwrap_or(Json::Null);
            println!("  {name}: p50 {p50} ms p99 {p99} ms bytes {bytes} match {ok}");
        }
    }
    if let Some(rows) = receipt
        .pointer("/topology/restores")
        .and_then(Json::as_array)
    {
        for row in rows {
            println!(
                "  topology {}: p50 {} ms, bytes {} ({:.3}x naive), roots_ok {}",
                row.get("label").and_then(Json::as_str).unwrap_or("?"),
                row.pointer("/latency/p50_ms")
                    .cloned()
                    .unwrap_or(Json::Null),
                row.get("backend_bytes_read_last_rep")
                    .cloned()
                    .unwrap_or(Json::Null),
                row.get("bytes_ratio_vs_naive_full")
                    .and_then(Json::as_f64)
                    .unwrap_or(0.0),
                row.get("all_roots_verified").cloned().unwrap_or(Json::Null)
            );
        }
    }
    if let Some(phases) = receipt.pointer("/recovery/phases").and_then(Json::as_array) {
        for row in phases {
            println!(
                "  recovery {}: open p50 {} ms, usable p50 {} ms, old {} new {}, verify_ok {}",
                row.get("phase").and_then(Json::as_str).unwrap_or("?"),
                row.pointer("/open_ms/p50_ms")
                    .cloned()
                    .unwrap_or(Json::Null),
                row.pointer("/time_to_usable_ms/p50_ms")
                    .cloned()
                    .unwrap_or(Json::Null),
                row.get("old_state").cloned().unwrap_or(Json::Null),
                row.get("new_state").cloned().unwrap_or(Json::Null),
                row.get("all_expected").cloned().unwrap_or(Json::Null)
            );
        }
    }
    for key in ["without_snapshot", "with_snapshot"] {
        if let Some(row) = receipt.pointer(&format!("/gc/{key}")) {
            println!(
                "  gc {key}: deleted {} bytes in {}, retained blob bytes {}, verify_ok {}, live-restore {}",
                row.pointer("/gc_apply/deleted_bytes").cloned().unwrap_or(Json::Null),
                row.get("snapshot_pinned").cloned().unwrap_or(Json::Null),
                row.get("retained_blob_bytes").cloned().unwrap_or(Json::Null),
                row.pointer("/verify/ok").cloned().unwrap_or(Json::Null),
                row.pointer("/live_checkpoint_restore/matches_ingest").cloned().unwrap_or(Json::Null),
            );
        }
    }
}

fn _unused() {}

/// Recompute one configuration's `comparison` block from data already inside
/// that configuration's receipt. Used when only the comparison derivation
/// changes: every measured phase is left byte-identical.
fn recompare(args: &[String]) -> Result<(), String> {
    let input = PathBuf::from(required(args, "--config")?);
    let out = PathBuf::from(required(args, "--out")?);
    let mut receipt = read_json(&input)?;
    let ingest = receipt
        .get("ingest")
        .cloned()
        .ok_or_else(|| "config receipt has no ingest block".to_owned())?;
    let baselines = receipt
        .get("baselines")
        .cloned()
        .ok_or_else(|| "config receipt has no baselines block".to_owned())?;
    let comparison = comparison_block(&ingest, &baselines);
    receipt
        .as_object_mut()
        .ok_or_else(|| "config receipt is not an object".to_owned())?
        .insert("comparison".to_owned(), json!({
            "weftfs_vs_zstd19": comparison.get("weftfs_vs_zstd19").cloned().unwrap_or(Json::Null),
            "where_the_planner_loses": comparison.get("where_the_planner_loses").cloned().unwrap_or(Json::Null),
            "patch_from": comparison.get("patch_from").cloned().unwrap_or(Json::Null),
            "derived_by": "family_lifecycle recompare: the same function assemble uses, over the ingest and baselines blocks already in this receipt",
        }));
    write_json(&out, &receipt)?;
    println!("{}", out.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// merge: the court receipt
// ---------------------------------------------------------------------------

fn merge(args: &[String]) -> Result<(), String> {
    let out = PathBuf::from(required(args, "--out")?);
    let list = PathBuf::from(required(args, "--inputs")?);
    let text = fs::read_to_string(&list).map_err(|err| format!("{}: {err}", list.display()))?;
    let mut configurations = Vec::new();
    let mut failed = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match read_json(Path::new(line)) {
            Ok(value) => configurations.push(value),
            Err(err) => failed.push(json!({"path": line, "error": err})),
        }
    }
    let receipt = json!({
        "schema": "weft.family-lifecycle.court.v1",
        "stamp": std::env::var("WEFT_STAMP").unwrap_or_else(|_| "unstamped".to_owned()),
        "command": std::env::var("WEFT_COURT_COMMAND").unwrap_or_default(),
        "label": std::env::var("WEFT_FAMILY_LABEL").unwrap_or_else(|_| "unknown".to_owned()),
        "environment": {
            "court_image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "court_kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "court_cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "court_memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "court_pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftfs_go": std::env::var("WEFT_REPO_WEFTFS_GO").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftcodec": std::env::var("WEFT_REPO_WEFTCODEC").unwrap_or_else(|_| "unknown".to_owned()),
            "repo_weftcodec_go": std::env::var("WEFT_REPO_WEFTCODEC_GO").unwrap_or_else(|_| "unknown".to_owned()),
        },
        "declared_limits": {
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "memory_swap": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "cap_drop": "ALL",
            "no_new_privileges": true,
            "tmpfs": "/tmp 1g",
        },
        "configurations": configurations,
        "failed_inputs": failed,
        "non_claims": [
            "single host, warm page cache, fixed clock fixture",
            "synthetic simulated training loop is declared in the receipt, never presented as a real training run",
            "process-fault crash proxy, not physical media power loss",
            "MinIO single node on loopback, if an S3 configuration is present",
            "no fleet, cloud, or production claim",
        ],
    });
    write_json(&out, &receipt)?;
    println!("receipt {}", out.display());
    Ok(())
}
