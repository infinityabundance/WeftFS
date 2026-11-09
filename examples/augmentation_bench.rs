//! Same-backend augmentation benchmark (subphase R3).
//!
//! One command runs configurations A/B/C/D/E (and the supplementary native +
//! application-level baseline N) over **one** backend, then writes one
//! `weft.augmentation.v1` receipt and a raw log. The frozen methodology is
//! `bench/CONTRACTS-augmentation.md`; this binary implements it and refuses
//! inputs that would violate it.
//!
//! Usage (inside the OOM-protected bench court):
//!
//! ```text
//! cargo run --release --quiet --example augmentation_bench -- run \
//!   --fixture /work/.docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors \
//!   --work /work/.docker-cache/augmentation \
//!   --out bench/receipts/augmentation-local.json \
//!   --log bench/receipts/augmentation-local.log \
//!   --backend local --max-others 100 --include-embedding 1 \
//!   --with-native-app
//! ```
//!
//! Phases (`prepare`, `write`, `restore`, `cleanup`) are available as
//! subcommands for diagnosis, but `run` is the supported entry point: it spawns
//! every write and restore in a **fresh process**, which is what makes the
//! `VmHWM` and cold-cache numbers honest.
//!
//! Scope label: one host, one local filesystem or one MinIO endpoint on
//! loopback, in the court's hard limits. Not a cloud, fleet, media, or
//! power-loss claim.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

use weftcodec::context::{ContextRoot, DEFAULT_RESOLVER};
use weftcodec::selector;
use weftcodec::wcb::Value as WcbValue;
use weftcodec::Limits;
use weftfs::auth::Principal;
use weftfs::backend::{BackendKind, BackendStats, Key, KeyKind, StorageBackend};
use weftfs::identity::{self, Hash32};
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::s3::{S3Backend, S3Config};
use weftfs::safetensors::SafetensorsFile;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{
    FastCodec, TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec, WritePolicy,
};

const FIXTURE_SHA256: &str = "fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe";
const EMBEDDING: &str = "model.embed_tokens.weight";
const FIXED_TIMESTAMP_NS: i64 = 1_700_000_000_000_000_000;
const DEFAULT_CHUNK_SIZE: u64 = 1 << 20;
const PROJECT_ATTR: &str = "project";
const PROJECT_VALUE: &str = "augmentation";
const CONFIGS: [&str; 5] = ["A", "B", "C", "D", "E"];
const NATIVE_APP: &str = "N";
const SEED_DOMAIN: &str = "weft/augmentation/object-seed/v1";
const NATIVE_KEY_DOMAIN: &str = "weft/augmentation/native-key/v1";
const NATIVE_APP_KEY_DOMAIN: &str = "weft/augmentation/native-app-key/v1";

// ---------------------------------------------------------------------------
// Subset
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TensorSel {
    name: String,
    /// Native local file name (deterministic).
    file: String,
    /// Native S3 object key (hex of a domain-separated digest).
    key: String,
    dtype: String,
    shape: Vec<u64>,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Subset {
    schema: String,
    source: String,
    source_sha256: String,
    source_bytes: u64,
    total_source_tensors: usize,
    selection: String,
    include_embedding: bool,
    other_tensors: usize,
    tensors: Vec<TensorSel>,
    logical_bytes: u64,
    /// SHA-256 over the canonical `tensors` array (used for native-app keys).
    subset_digest: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn tensor_file_name(index: usize, name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{index:04}-{safe}.bin")
}

fn select_subset(
    file: &SafetensorsFile,
    include_embedding: bool,
    other_tensors: usize,
) -> Result<Subset, String> {
    let mut selected_names: Vec<String> = Vec::new();
    if include_embedding {
        if !file.tensors.iter().any(|tensor| tensor.name == EMBEDDING) {
            return Err(format!(
                "fixture has no `{EMBEDDING}` tensor; the contract requires it when include_embedding=1"
            ));
        }
        selected_names.push(EMBEDDING.to_owned());
    }
    let mut rest: Vec<&str> = file
        .tensors
        .iter()
        .map(|tensor| tensor.name.as_str())
        .filter(|name| *name != EMBEDDING)
        .collect();
    rest.sort_unstable();
    if other_tensors > rest.len() {
        return Err(format!(
            "requested {other_tensors} non-embedding tensors but the container has {}",
            rest.len()
        ));
    }
    if other_tensors > 0 {
        // Deterministic even spacing over name-sorted non-embedding tensors.
        let stride = (rest.len() / other_tensors).max(1);
        for name in rest.iter().step_by(stride).take(other_tensors) {
            selected_names.push((*name).to_owned());
        }
    }

    let mut tensors = Vec::with_capacity(selected_names.len());
    let mut logical_bytes = 0u64;
    for (index, name) in selected_names.iter().enumerate() {
        let tensor = file
            .tensors
            .iter()
            .find(|tensor| &tensor.name == name)
            .ok_or_else(|| format!("selected tensor `{name}` disappeared from the parse"))?;
        let byte_len = tensor.byte_len();
        let len = usize::try_from(byte_len)
            .map_err(|_| format!("tensor `{name}` is too large for this host"))?;
        let mut bytes = vec![0u8; len];
        file.read_tensor_range(tensor, 0, &mut bytes)
            .map_err(|err| format!("read `{name}`: {err}"))?;
        let sha = sha256_hex(&bytes);
        drop(bytes);
        logical_bytes = logical_bytes
            .checked_add(byte_len)
            .ok_or_else(|| "logical byte total overflow".to_owned())?;
        tensors.push(TensorSel {
            name: name.clone(),
            file: tensor_file_name(index, name),
            key: identity::h(NATIVE_KEY_DOMAIN, name.as_bytes()).to_hex(),
            dtype: tensor.dtype.clone(),
            shape: tensor.shape.clone(),
            bytes: byte_len,
            sha256: sha,
        });
    }

    let canonical = serde_json::to_vec(
        &tensors
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "dtype": t.dtype,
                    "shape": t.shape,
                    "bytes": t.bytes,
                    "sha256": t.sha256,
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|err| format!("encode subset digest: {err}"))?;

    let selection = format!(
        "{}embedding + evenly spaced {} of {} name-sorted non-embedding tensors",
        if include_embedding { "" } else { "no " },
        other_tensors,
        file.tensors.len().saturating_sub(1)
    );

    Ok(Subset {
        schema: "weft.augmentation.subset.v1".to_owned(),
        source: file.path.display().to_string(),
        source_sha256: hex::encode(file.sha256),
        source_bytes: file.file_bytes,
        total_source_tensors: file.tensors.len(),
        selection,
        include_embedding,
        other_tensors,
        tensors,
        logical_bytes,
        subset_digest: sha256_hex(&canonical),
    })
}

fn load_subset(path: &Path) -> Result<Subset, String> {
    let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|err| format!("{}: {err}", path.display()))
}

fn write_json_atomic(path: &Path, value: &Json) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|err| format!("encode JSON: {err}"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = File::create(&tmp).map_err(|err| format!("{}: {err}", tmp.display()))?;
        file.write_all(&bytes)
            .map_err(|err| format!("{}: {err}", tmp.display()))?;
        file.sync_all()
            .map_err(|err| format!("{}: {err}", tmp.display()))?;
    }
    fs::rename(&tmp, path).map_err(|err| format!("{}: {err}", path.display()))?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Process measurements
// ---------------------------------------------------------------------------

fn peak_rss_kib() -> u64 {
    let Ok(text) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let value = rest.split_whitespace().next().unwrap_or("0");
            return value.parse().unwrap_or(0);
        }
    }
    0
}

fn cpu_ms() -> f64 {
    let Ok(text) = fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    let Some(close) = text.rfind(") ") else {
        return 0.0;
    };
    let fields: Vec<&str> = text[close + 2..].split_whitespace().collect();
    // After the comm field, index 0 is field 3 (`state`). utime is field 14,
    // stime is field 15.
    let utime: u64 = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0);
    let stime: u64 = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0);
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = if ticks > 0 { ticks as f64 } else { 100.0 };
    1000.0 * (utime + stime) as f64 / hz
}

fn percentile_us(samples: &[Duration], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut values: Vec<u64> = samples.iter().map(|d| d.as_micros() as u64).collect();
    values.sort_unstable();
    let index = (((values.len() - 1) as f64) * p).round() as usize;
    values[index.min(values.len() - 1)] as f64
}

fn latency_summary(samples: &[Duration]) -> Json {
    if samples.is_empty() {
        return json!({
            "samples": 0,
            "p50_us": Json::Null,
            "p95_us": Json::Null,
            "p99_us": Json::Null,
        });
    }
    let total: u64 = samples.iter().map(|d| d.as_micros() as u64).sum();
    json!({
        "samples": samples.len(),
        "p50_us": percentile_us(samples, 0.50),
        "p95_us": percentile_us(samples, 0.95),
        "p99_us": percentile_us(samples, 0.99),
        "mean_us": total as f64 / samples.len() as f64,
    })
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                out.push(path);
            }
        }
    }
    out
}

fn footprint_local(root: &Path) -> (u64, u64) {
    let files = walk_files(root);
    let mut bytes = 0u64;
    for path in &files {
        if let Ok(meta) = fs::metadata(path) {
            bytes = bytes.saturating_add(meta.len());
        }
    }
    (bytes, files.len() as u64)
}

fn fadvise_dontneed(root: &Path) {
    use std::os::unix::io::AsRawFd;
    for path in walk_files(root) {
        if let Ok(file) = File::open(&path) {
            unsafe {
                libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Args {
    subcommand: String,
    fixture: Option<PathBuf>,
    work: Option<PathBuf>,
    out: Option<PathBuf>,
    log: Option<PathBuf>,
    backend: String,
    config: Option<String>,
    subset: Option<PathBuf>,
    phase_out: Option<PathBuf>,
    s3_bucket: Option<String>,
    chunk_size: u64,
    seed: String,
    max_others: usize,
    include_embedding: bool,
    with_native_app: bool,
    reduced: bool,
    passes: usize,
    force: bool,
    label: String,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            subcommand: String::new(),
            fixture: None,
            work: None,
            out: None,
            log: None,
            backend: "local".to_owned(),
            config: None,
            subset: None,
            phase_out: None,
            s3_bucket: None,
            chunk_size: DEFAULT_CHUNK_SIZE,
            seed: "weft-augmentation-r3".to_owned(),
            max_others: 100,
            include_embedding: true,
            with_native_app: false,
            reduced: false,
            passes: 2,
            force: false,
            label: "full".to_owned(),
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    args.subcommand = it.next().ok_or_else(|| "missing subcommand".to_owned())?;
    while let Some(flag) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--fixture" => args.fixture = Some(PathBuf::from(value("--fixture")?)),
            "--work" => args.work = Some(PathBuf::from(value("--work")?)),
            "--out" => args.out = Some(PathBuf::from(value("--out")?)),
            "--log" => args.log = Some(PathBuf::from(value("--log")?)),
            "--backend" => args.backend = value("--backend")?,
            "--config" => args.config = Some(value("--config")?),
            "--subset" => args.subset = Some(PathBuf::from(value("--subset")?)),
            "--phase-out" => args.phase_out = Some(PathBuf::from(value("--phase-out")?)),
            "--s3-bucket" => args.s3_bucket = Some(value("--s3-bucket")?),
            "--chunk-size" => {
                args.chunk_size = value("--chunk-size")?
                    .parse()
                    .map_err(|_| "--chunk-size must be an integer".to_owned())?
            }
            "--seed" => args.seed = value("--seed")?,
            "--max-others" => {
                args.max_others = value("--max-others")?
                    .parse()
                    .map_err(|_| "--max-others must be an integer".to_owned())?
            }
            "--include-embedding" => {
                args.include_embedding = parse_bool(&value("--include-embedding")?)?
            }
            "--with-native-app" => args.with_native_app = true,
            "--reduced" => args.reduced = true,
            "--passes" => {
                args.passes = value("--passes")?
                    .parse()
                    .map_err(|_| "--passes must be an integer".to_owned())?
            }
            "--force" => args.force = true,
            "--label" => args.label = value("--label")?,
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    Ok(args)
}

fn parse_bool(text: &str) -> Result<bool, String> {
    match text {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        other => Err(format!("expected 0|1, got `{other}`")),
    }
}

fn base_seed(seed: &str) -> [u8; 32] {
    Sha256::digest(seed.as_bytes()).into()
}

fn object_seed(base: &[u8; 32], config: &str, name: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(SEED_DOMAIN.as_bytes());
    hasher.update(base);
    hasher.update(config.as_bytes());
    hasher.update([0u8]);
    hasher.update(name.as_bytes());
    hasher.finalize().into()
}

fn options(chunk_size: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(FIXED_TIMESTAMP_NS),
        ..Options::default()
    }
}

fn s3_config(args: &Args) -> Result<S3Config, String> {
    let mut config = S3Config::from_env().map_err(|err| err.to_string())?;
    if let Some(bucket) = &args.s3_bucket {
        config.bucket = bucket.clone();
    }
    Ok(config)
}

fn backend_kind(args: &Args) -> Result<BackendKind, String> {
    match args.backend.as_str() {
        "local" => Ok(BackendKind::Local),
        "s3" => Ok(BackendKind::S3 {
            config: s3_config(args)?,
        }),
        other => Err(format!("unknown backend `{other}` (expected local|s3)")),
    }
}

fn config_store_path(work: &Path, config: &str) -> PathBuf {
    work.join(config).join("store")
}

fn config_native_path(work: &Path, config: &str) -> PathBuf {
    work.join(config).join("native")
}

fn limit_label() -> Json {
    json!({
        "court_image": std::env::var("WEFT_COURT_IMAGE").ok(),
        "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").ok(),
        "kernel": std::env::var("WEFT_COURT_KERNEL").ok(),
        "memory": std::env::var("WEFT_COURT_MEMORY").ok(),
        "cpus": std::env::var("WEFT_COURT_CPUS").ok(),
        "pids": std::env::var("WEFT_COURT_PIDS").ok(),
        "nproc": std::env::var("WEFT_COURT_NPROC").ok(),
        "s3_server": {
            "label": std::env::var("WEFT_S3_SERVER_LABEL").ok(),
            "release": std::env::var("WEFT_S3_SERVER_RELEASE").ok(),
            "binary_sha256": std::env::var("WEFT_S3_SERVER_SHA256").ok(),
            "container_image": std::env::var("WEFT_S3_CONTAINER_IMAGE").ok(),
        },
        "revisions": {
            "WeftFS": std::env::var("WEFT_REPO_WEFTFS").ok(),
            "WeftCodec": std::env::var("WEFT_REPO_WEFTCODEC").ok(),
            "WeftCodec-Go": std::env::var("WEFT_REPO_WEFTCODEC_GO").ok(),
            "WeftFS-Go": std::env::var("WEFT_REPO_WEFTFS_GO").ok(),
        },
        "uname": uname(),
    })
}

fn uname() -> String {
    let output = Command::new("uname").arg("-srm").output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        _ => "unknown".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Native (A) and native + application scheme (N)
// ---------------------------------------------------------------------------

fn write_native_local(dir: &Path, file: &SafetensorsFile, subset: &Subset) -> Result<Json, String> {
    fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let dir_file = File::open(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut writes = 0u64;
    let mut bytes_written = 0u64;
    let mut source_read_ms = 0f64;
    let mut write_ms = 0f64;
    for sel in &subset.tensors {
        let tensor = file
            .tensors
            .iter()
            .find(|t| t.name == sel.name)
            .ok_or_else(|| format!("tensor `{}` missing", sel.name))?;
        let read_started = Instant::now();
        let mut bytes = vec![0u8; sel.bytes as usize];
        file.read_tensor_range(tensor, 0, &mut bytes)
            .map_err(|err| format!("read `{}`: {err}", sel.name))?;
        source_read_ms += read_started.elapsed().as_secs_f64() * 1000.0;
        let write_started = Instant::now();
        let path = dir.join(&sel.file);
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        out.write_all(&bytes)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        out.sync_all()
            .map_err(|err| format!("{}: {err}", path.display()))?;
        write_ms += write_started.elapsed().as_secs_f64() * 1000.0;
        writes += 1;
        bytes_written = bytes_written.saturating_add(bytes.len() as u64);
    }
    dir_file
        .sync_all()
        .map_err(|err| format!("{}: {err}", dir.display()))?;
    Ok(json!({
        "native_writes": writes,
        "native_bytes_written": bytes_written,
        "native_sync_calls": writes + 1,
        "source_read_ms": source_read_ms,
        "write_ms": write_ms,
        "physical_footprint_bytes": footprint_local(dir).0,
        "footprint_source": "recursive file walk (du source)",
    }))
}

fn write_native_s3(args: &Args, subset: &Subset, file: &SafetensorsFile) -> Result<Json, String> {
    let config = s3_config(args)?;
    let mut backend = S3Backend::new(config_store_path(&args.work.clone().unwrap(), "A"), config);
    backend
        .ensure_bucket()
        .map_err(|err| format!("ensure bucket: {err}"))?;
    let mut writes = 0u64;
    let mut bytes_written = 0u64;
    let mut source_read_ms = 0f64;
    let mut write_ms = 0f64;
    for sel in &subset.tensors {
        let tensor = file
            .tensors
            .iter()
            .find(|t| t.name == sel.name)
            .ok_or_else(|| format!("tensor `{}` missing", sel.name))?;
        let read_started = Instant::now();
        let mut bytes = vec![0u8; sel.bytes as usize];
        file.read_tensor_range(tensor, 0, &mut bytes)
            .map_err(|err| format!("read `{}`: {err}", sel.name))?;
        source_read_ms += read_started.elapsed().as_secs_f64() * 1000.0;
        let hash = Hash32::from_hex(&sel.key).map_err(|err| err.to_string())?;
        let write_started = Instant::now();
        backend
            .put_atomic(&Key::Object { hash }, &bytes)
            .map_err(|err| format!("PUT `{}`: {err}", sel.name))?;
        write_ms += write_started.elapsed().as_secs_f64() * 1000.0;
        writes += 1;
        bytes_written = bytes_written.saturating_add(bytes.len() as u64);
    }
    let config = s3_config(args)?;
    let (footprint, objects) = s3_footprint(&config, &[KeyKind::Object], false)?;
    Ok(json!({
        "native_writes": writes,
        "native_bytes_written": bytes_written,
        "source_read_ms": source_read_ms,
        "write_ms": write_ms,
        "backend_stats": stats_json(&backend.stats()),
        "physical_footprint_bytes": footprint,
        "physical_footprint_objects": objects,
        "footprint_source": "S3 object listing of the objects/ prefix",
    }))
}

fn native_app_manifest(subset: &Subset) -> Json {
    json!({
        "schema": "weft.augmentation.native_app_manifest.v1",
        "source_sha256": subset.source_sha256,
        "source_bytes": subset.source_bytes,
        "subset_digest": subset.subset_digest,
        "tensors": subset.tensors.iter().map(|t| json!({
            "name": t.name,
            "file": t.file,
            "bytes": t.bytes,
            "sha256": t.sha256,
        })).collect::<Vec<_>>(),
        "logical_bytes": subset.logical_bytes,
    })
}

fn native_app_provenance(subset: &Subset) -> Json {
    let mut outputs: Vec<&str> = subset.tensors.iter().map(|t| t.sha256.as_str()).collect();
    outputs.sort_unstable();
    outputs.dedup();
    json!({
        "schema": "weft.augmentation.native_app_provenance.v1",
        "operation": "augmentation.write",
        "tool": "weftfs-augmentation",
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp_ns": FIXED_TIMESTAMP_NS,
        "outputs": outputs,
        "output_count": outputs.len(),
    })
}

fn native_app_context(subset: &Subset) -> Json {
    json!({
        "schema": "weft.augmentation.native_app_context.v1",
        "selection": {"attribute": PROJECT_ATTR, "value": PROJECT_VALUE},
        "keys": subset.tensors.iter().map(|t| t.file.clone()).collect::<Vec<_>>(),
        "tensor_count": subset.tensors.len(),
    })
}

fn check_native_manifest(manifest: &Json, subset: &Subset) -> Result<(), String> {
    let entries = manifest
        .get("tensors")
        .and_then(|t| t.as_array())
        .ok_or_else(|| "native-app manifest has no tensors".to_owned())?;
    if entries.len() != subset.tensors.len() {
        return Err(format!(
            "native-app manifest lists {} tensors, expected {}",
            entries.len(),
            subset.tensors.len()
        ));
    }
    let mut seen: BTreeMap<&str, (&str, u64)> = BTreeMap::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "native-app manifest entry has no name".to_owned())?;
        let sha = entry
            .get("sha256")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("native-app manifest entry {name} has no sha256"))?;
        let bytes = entry
            .get("bytes")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("native-app manifest entry {name} has no byte length"))?;
        if seen.insert(name, (sha, bytes)).is_some() {
            return Err(format!("native-app manifest repeats tensor `{name}`"));
        }
    }
    for sel in &subset.tensors {
        match seen.get(sel.name.as_str()) {
            Some((sha, bytes)) if *sha == sel.sha256 && *bytes == sel.bytes => {}
            Some(_) => {
                return Err(format!(
                    "native-app manifest disagrees with the subset for `{}`",
                    sel.name
                ))
            }
            None => return Err(format!("native-app manifest is missing `{}`", sel.name)),
        }
    }
    Ok(())
}

fn check_native_context(context: &Json, subset: &Subset) -> Result<(), String> {
    let keys: BTreeSet<&str> = context
        .get("keys")
        .and_then(|k| k.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    let expected: BTreeSet<&str> = subset.tensors.iter().map(|t| t.file.as_str()).collect();
    if keys != expected {
        return Err(format!(
            "native-app context key set has {} entries, expected {}",
            keys.len(),
            expected.len()
        ));
    }
    Ok(())
}

fn check_native_provenance(provenance: &Json, subset: &Subset) -> Result<(), String> {
    let outputs: BTreeSet<&str> = provenance
        .get("outputs")
        .and_then(|o| o.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    let expected: BTreeSet<&str> = subset.tensors.iter().map(|t| t.sha256.as_str()).collect();
    if outputs != expected {
        return Err("native-app provenance outputs do not equal the tensor digests".to_owned());
    }
    Ok(())
}

fn native_app_s3_key(name: &str, subset: &Subset) -> Hash32 {
    identity::h(
        NATIVE_APP_KEY_DOMAIN,
        format!("{name}:{}", subset.subset_digest).as_bytes(),
    )
}

fn publish_json_atomic_local(path: &Path, value: &Json) -> Result<u64, String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|err| format!("encode: {err}"))?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = File::create(&tmp).map_err(|err| format!("{}: {err}", tmp.display()))?;
        file.write_all(&bytes)
            .map_err(|err| format!("{}: {err}", tmp.display()))?;
        file.sync_all()
            .map_err(|err| format!("{}: {err}", tmp.display()))?;
    }
    fs::rename(&tmp, path).map_err(|err| format!("{}: {err}", path.display()))?;
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    Ok(bytes.len() as u64)
}

fn write_native_app_local(
    dir: &Path,
    file: &SafetensorsFile,
    subset: &Subset,
) -> Result<Json, String> {
    let mut native = write_native_local(dir, file, subset)?;
    let manifest_bytes =
        publish_json_atomic_local(&dir.join("manifest.json"), &native_app_manifest(subset))?;
    let provenance_bytes =
        publish_json_atomic_local(&dir.join("provenance.json"), &native_app_provenance(subset))?;
    let context_bytes =
        publish_json_atomic_local(&dir.join("context.json"), &native_app_context(subset))?;
    let meta = manifest_bytes + provenance_bytes + context_bytes;
    native["native_bytes_written"] =
        json!(native["native_bytes_written"].as_u64().unwrap_or(0) + meta);
    native["metadata_bytes_written"] = json!(meta);
    native["metadata_objects"] = json!(3);
    native["physical_footprint_bytes"] = json!(footprint_local(dir).0);
    native["footprint_source"] = json!("recursive file walk (du source)");
    Ok(native)
}

fn write_native_app_s3(
    args: &Args,
    subset: &Subset,
    file: &SafetensorsFile,
) -> Result<Json, String> {
    let mut native = write_native_s3(args, subset, file)?;
    let config = s3_config(args)?;
    let mut backend = S3Backend::new(config_store_path(&args.work.clone().unwrap(), "N"), config);
    backend
        .ensure_bucket()
        .map_err(|err| format!("ensure bucket: {err}"))?;
    let objects = [
        (
            "manifest",
            serde_json::to_vec_pretty(&native_app_manifest(subset))
                .map_err(|err| err.to_string())?,
        ),
        (
            "provenance",
            serde_json::to_vec_pretty(&native_app_provenance(subset))
                .map_err(|err| err.to_string())?,
        ),
        (
            "context",
            serde_json::to_vec_pretty(&native_app_context(subset))
                .map_err(|err| err.to_string())?,
        ),
    ];
    let mut meta_bytes = 0u64;
    for (name, bytes) in &objects {
        backend
            .put_atomic(
                &Key::Manifest {
                    hash: native_app_s3_key(name, subset),
                },
                bytes,
            )
            .map_err(|err| format!("PUT {name}: {err}"))?;
        meta_bytes = meta_bytes.saturating_add(bytes.len() as u64);
    }
    let config = s3_config(args)?;
    let (footprint, objects) = s3_footprint(&config, &[KeyKind::Object, KeyKind::Manifest], false)?;
    native["metadata_bytes_written"] = json!(meta_bytes);
    native["metadata_objects"] = json!(3);
    native["physical_footprint_bytes"] = json!(footprint);
    native["physical_footprint_objects"] = json!(objects);
    native["footprint_source"] = json!("S3 object listing of objects/ and manifests/");
    Ok(native)
}

// ---------------------------------------------------------------------------
// WeftFS write (B/C/D/E)
// ---------------------------------------------------------------------------

fn representation_spec(config: &str) -> TensorWriteSpec {
    let planned = matches!(config, "C" | "E");
    if planned {
        TensorWriteSpec {
            strategy_hint: None,
            causal: Vec::new(),
            plan: TensorPlanOptions {
                write_policy: WritePolicy::Planned,
                semantic_search: false,
                ..TensorPlanOptions::default()
            },
            tile_shape: None,
        }
    } else {
        // B/D: the shipped Fast path with the raw codec builds exactly one
        // representation (canonical chunks) and runs no planned encoder.
        TensorWriteSpec {
            strategy_hint: Some(TensorStrategy::Raw),
            causal: Vec::new(),
            plan: TensorPlanOptions {
                write_policy: WritePolicy::Fast,
                fast_codec: FastCodec::Raw,
                fast_zstd_level: 1,
                semantic_search: false,
                ..TensorPlanOptions::default()
            },
            tile_shape: None,
        }
    }
}

fn uses_protocol(config: &str) -> bool {
    matches!(config, "D" | "E")
}

fn write_weftfs(
    args: &Args,
    config: &str,
    subset: &Subset,
    file: &SafetensorsFile,
) -> Result<Json, String> {
    let work = args.work.clone().unwrap();
    let store_path = config_store_path(&work, config);
    if store_path.exists() {
        return Err(format!(
            "{} already exists (refusing to reuse state)",
            store_path.display()
        ));
    }
    let kind = backend_kind(args)?;
    let init_started = Instant::now();
    let mut store = Store::init_with_backend(&store_path, options(args.chunk_size), kind)
        .map_err(|err| format!("init store: {err}"))?;
    let init_ms = init_started.elapsed().as_secs_f64() * 1000.0;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    let tensors_dir = store
        .mkdir(root, b"tensors", 0o755, 0, 0)
        .map_err(|err| format!("mkdir /tensors: {err}"))?;

    let seed_base = base_seed(&args.seed);
    let layout = TensorLayoutSpec::row_major();
    let mut ids: Vec<Hash32> = Vec::with_capacity(subset.tensors.len());
    let mut output_contents: BTreeSet<Hash32> = BTreeSet::new();
    let mut representations: BTreeMap<String, u64> = BTreeMap::new();
    let mut source_read_ms = 0f64;
    let mut write_ms = 0f64;
    let mut samples: Vec<Duration> = Vec::with_capacity(subset.tensors.len());
    let mut id_by_name: BTreeMap<String, String> = BTreeMap::new();
    let mut content_by_name: BTreeMap<String, String> = BTreeMap::new();

    for sel in &subset.tensors {
        let tensor = file
            .tensors
            .iter()
            .find(|t| t.name == sel.name)
            .ok_or_else(|| format!("tensor `{}` missing", sel.name))?;
        let read_started = Instant::now();
        let mut bytes = vec![0u8; sel.bytes as usize];
        file.read_tensor_range(tensor, 0, &mut bytes)
            .map_err(|err| format!("read `{}`: {err}", sel.name))?;
        source_read_ms += read_started.elapsed().as_secs_f64() * 1000.0;

        let started = Instant::now();
        let stats_before = store.backend_stats();
        let spec = representation_spec(config);
        let seed = object_seed(&seed_base, config, &sel.name);
        let mut prepared = store
            .prepare_tensor_object_with_spec_seeded(
                &sel.dtype, &sel.shape, &bytes, &layout, &spec, &seed,
            )
            .map_err(|err| format!("prepare `{}`: {err}", sel.name))?;
        if uses_protocol(config) {
            prepared.attrs.insert(
                PROJECT_ATTR.to_owned(),
                WcbValue::Str(PROJECT_VALUE.to_owned()),
            );
        }
        let id = store
            .commit_prepared_tensor_object(tensors_dir, sel.name.as_bytes(), &prepared, 0o644)
            .map_err(|err| format!("commit `{}`: {err}", sel.name))?;
        write_ms += started.elapsed().as_secs_f64() * 1000.0;
        samples.push(started.elapsed());
        if std::env::var("WEFT_AUG_TRACE").is_ok() {
            let stats_after = store.backend_stats();
            eprintln!(
                "trace {config} {} logical={} strategy={} written_delta={} written_total={}",
                sel.name,
                bytes.len(),
                prepared.strategy,
                stats_after.bytes_written - stats_before.bytes_written,
                stats_after.bytes_written
            );
        }

        ids.push(id);
        // The store's `ObjectRecord.content` for a file is the manifest id
        // (see `commit_created_file`); the provenance API checks exactly that
        // field. `prepared.content_root` is the logical-byte hash and is not
        // the same value.
        output_contents.insert(prepared.manifest_id);
        *representations
            .entry(prepared.strategy.clone())
            .or_insert(0) += 1;
        id_by_name.insert(sel.name.clone(), id.to_hex());
        content_by_name.insert(sel.name.clone(), prepared.manifest_id.to_hex());
    }

    let stats_after_write = store.backend_stats();
    let head = store.head().to_hex();
    let generation = store.generation();

    let mut protocol = Json::Null;
    let mut protocol_ms = 0f64;
    if uses_protocol(config) {
        let started = Instant::now();
        let snapshot = store
            .snapshot_create("augmentation")
            .map_err(|err| format!("snapshot_create: {err}"))?;
        let parsed = selector::parse(&format!(
            "select attrs.{PROJECT_ATTR} == \"{PROJECT_VALUE}\""
        ))
        .map_err(|err| format!("selector parse: {err}"))?;
        let context = ContextRoot::new(&parsed, snapshot, DEFAULT_RESOLVER, Vec::new(), None, None)
            .map_err(|err| format!("ContextRoot::new: {err}"))?;
        let context_id = context.id().map_err(|err| err.to_string())?;
        store
            .save_context_root("augmentation", &context, None, false)
            .map_err(|err| format!("save_context_root: {err}"))?;
        let resolution = store
            .resolve_context(
                "augmentation",
                None,
                &Principal::root(),
                &Limits::default(),
                FIXED_TIMESTAMP_NS,
            )
            .map_err(|err| format!("resolve_context: {err}"))?;
        if resolution.resolution.matched_count as usize != subset.tensors.len() {
            return Err(format!(
                "context resolved {} objects, expected {}",
                resolution.resolution.matched_count,
                subset.tensors.len()
            ));
        }
        let mut params = BTreeMap::new();
        params.insert("config".to_owned(), WcbValue::Str(config.to_owned()));
        params.insert(
            "tensors".to_owned(),
            WcbValue::U64(subset.tensors.len() as u64),
        );
        params.insert(
            "logical_bytes".to_owned(),
            WcbValue::U64(subset.logical_bytes),
        );
        let record = ProvenanceRecord::new(
            "augmentation.write",
            params,
            Vec::new(),
            output_contents.iter().copied().collect(),
            ProvenanceEnvironment::new("weftfs-augmentation", env!("CARGO_PKG_VERSION")),
            Determinism::Exact,
            ProvenancePrincipal::new(0, 0),
            FIXED_TIMESTAMP_NS,
        )
        .map_err(|err| format!("provenance record: {err}"))?;
        let provenance_id = store
            .record_provenance(&ids, &record)
            .map_err(|err| format!("record_provenance: {err}"))?;
        protocol_ms = started.elapsed().as_secs_f64() * 1000.0;
        protocol = json!({
            "snapshot": snapshot.to_hex(),
            "context_root_id": context_id.to_hex(),
            "selector_id": resolution.resolution.selector_id.to_hex(),
            "resolved_objects": resolution.resolution.matched_count,
            "work_units": resolution.work_units,
            "provenance_id": provenance_id.to_hex(),
            "provenance_outputs": output_contents.len(),
        });
    }

    let footprint = if args.backend == "s3" {
        let config = s3_config(args)?;
        let (bytes, objects) = s3_footprint(&config, &store_kinds(), true)?;
        json!({
            "physical_footprint_bytes": bytes,
            "physical_footprint_objects": objects,
            "footprint_source": "S3 object listing of every store key prefix plus root control records",
        })
    } else {
        let (bytes, files) = footprint_local(&store_path);
        json!({
            "physical_footprint_bytes": bytes,
            "physical_footprint_objects": files,
            "footprint_source": "recursive file walk of the store directory (du source)",
        })
    };

    Ok(json!({
        "schema": "weft.augmentation.phase.v1",
        "phase": "write",
        "config": config,
        "backend": args.backend,
        "logical_bytes": subset.logical_bytes,
        "tensor_count": subset.tensors.len(),
        "init_ms": init_ms,
        "source_read_ms": source_read_ms,
        "write_ms": write_ms,
        "protocol_ms": protocol_ms,
        "per_tensor_write": latency_summary(&samples),
        "backend_stats": stats_json(&stats_after_write),
        "physical_bytes_written": stats_after_write.bytes_written,
        "physical_bytes_written_source": "Store::backend_stats().bytes_written (probe, LOCK, FORMAT, superblock, commits, manifests, chunk/frame blobs, index segments)",
        "representations": representations,
        "store_head": head,
        "generation": generation,
        "protocol": protocol,
        "storage": footprint,
        "object_ids": id_by_name,
        "output_contents": content_by_name,
    }))
}

// ---------------------------------------------------------------------------
// WeftFS / native restore
// ---------------------------------------------------------------------------

fn verified_read(
    label: &str,
    bytes: &[u8],
    expected_sha256: &str,
    mismatches: &mut u64,
) -> Result<(), String> {
    let actual = sha256_hex(bytes);
    if actual != expected_sha256 {
        *mismatches += 1;
        return Err(format!(
            "{label}: SHA-256 mismatch (expected {expected_sha256}, got {actual})"
        ));
    }
    Ok(())
}

fn restore_weftfs(
    args: &Args,
    config: &str,
    subset: &Subset,
    write_phase: &Json,
) -> Result<Json, String> {
    let work = args.work.clone().unwrap();
    let store_path = config_store_path(&work, config);
    if args.backend == "local" {
        fadvise_dontneed(&store_path);
    }

    let object_ids_json = write_phase
        .get("object_ids")
        .and_then(|v| v.as_object())
        .ok_or_else(|| format!("write phase for {config} has no object_ids"))?;
    let content_roots_json = write_phase
        .get("output_contents")
        .and_then(|v| v.as_object())
        .ok_or_else(|| format!("write phase for {config} has no output_contents"))?;
    let mut tensor_ids: BTreeMap<String, Hash32> = BTreeMap::new();
    for sel in &subset.tensors {
        let text = object_ids_json
            .get(&sel.name)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("write phase has no id for `{}`", sel.name))?;
        tensor_ids.insert(
            sel.name.clone(),
            Hash32::from_hex(text).map_err(|e| e.to_string())?,
        );
    }

    let kind = backend_kind(args)?;
    let started = Instant::now();
    let store = Store::open_with_backend(&store_path, OpenMode::ReadOnly, kind)
        .map_err(|err| format!("open store: {err}"))?;
    let open_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stats_after_open = store.backend_stats();

    let mut protocol = Json::Null;
    let mut protocol_ms = 0f64;
    let selected_ids: BTreeSet<Hash32> = tensor_ids.values().copied().collect();
    if uses_protocol(config) {
        let started = Instant::now();
        let resolution = store
            .resolve_context(
                "augmentation",
                None,
                &Principal::root(),
                &Limits::default(),
                FIXED_TIMESTAMP_NS,
            )
            .map_err(|err| format!("resolve_context: {err}"))?;
        let resolved: BTreeSet<Hash32> = resolution
            .resolution
            .results
            .iter()
            .map(|item| item.object_id)
            .collect();
        if resolved != selected_ids {
            return Err(format!(
                "context resolved {} objects, but the write phase published {}; sets differ",
                resolved.len(),
                selected_ids.len()
            ));
        }
        // One receipt read, then in-memory relation scans for every output.
        let mut provenance_ids: BTreeSet<Hash32> = BTreeSet::new();
        for id in &selected_ids {
            for provenance in store
                .provenance_ids_of(*id)
                .map_err(|err| err.to_string())?
            {
                provenance_ids.insert(provenance);
            }
        }
        if provenance_ids.len() != 1 {
            return Err(format!(
                "expected exactly one provenance record across the outputs, found {}",
                provenance_ids.len()
            ));
        }
        let record = store
            .read_provenance(*provenance_ids.iter().next().unwrap())
            .map_err(|err| format!("read_provenance: {err}"))?;
        let expected: BTreeSet<Hash32> = content_roots_json
            .values()
            .filter_map(|v| v.as_str())
            .filter_map(|text| Hash32::from_hex(text).ok())
            .collect();
        let actual: BTreeSet<Hash32> = record.outputs.iter().copied().collect();
        if actual != expected {
            return Err(format!(
                "provenance outputs ({} roots) do not equal the written content roots ({} roots)",
                actual.len(),
                expected.len()
            ));
        }
        protocol_ms = started.elapsed().as_secs_f64() * 1000.0;
        protocol = json!({
            "context_resolved": true,
            "resolved_objects": resolved.len(),
            "selector_id": resolution.resolution.selector_id.to_hex(),
            "work_units": resolution.work_units,
            "provenance_verified": true,
            "provenance_id": provenance_ids.iter().next().unwrap().to_hex(),
            "provenance_outputs": actual.len(),
        });
    }

    let pass = |label: &str| -> Result<(Json, u64, Vec<Duration>), String> {
        let mut mismatches = 0u64;
        let mut samples = Vec::with_capacity(subset.tensors.len());
        let mut restored_bytes = 0u64;
        let cpu_start = cpu_ms();
        let wall_started = Instant::now();
        for sel in &subset.tensors {
            let id = *tensor_ids
                .get(&sel.name)
                .ok_or_else(|| format!("no id for `{}`", sel.name))?;
            let op = Instant::now();
            let bytes = store
                .read_tensor(id)
                .map_err(|err| format!("read `{}`: {err}", sel.name))?;
            verified_read(&sel.name, &bytes, &sel.sha256, &mut mismatches)?;
            samples.push(op.elapsed());
            restored_bytes = restored_bytes.saturating_add(bytes.len() as u64);
        }
        let wall_ms = wall_started.elapsed().as_secs_f64() * 1000.0;
        let cpu = cpu_ms() - cpu_start;
        let peak = peak_rss_kib();
        Ok((
            json!({
                "basis": label,
                "wall_ms": wall_ms,
                "cpu_user_sys_ms": cpu,
                "peak_rss_kib": peak,
                "per_tensor": latency_summary(&samples),
                "restored_bytes": restored_bytes,
                "mismatches": mismatches,
            }),
            restored_bytes,
            samples,
        ))
    };

    let (mut cold, restored_bytes, _cold_samples) =
        pass("cold (fresh process; local store files fadvise(DONTNEED) before open)")?;
    let stats_after_cold = store.backend_stats();
    let cold_read_ms = cold["wall_ms"].as_f64().unwrap_or(0.0);
    // The cold restore wall is everything a fresh caller pays: open (commit
    // replay), protocol verification, and the read pass. The pass-only number
    // is kept as `read_wall_ms` so the decomposition is visible.
    cold["read_wall_ms"] = json!(cold_read_ms);
    cold["wall_ms"] = json!(open_ms + protocol_ms + cold_read_ms);
    let cold_bytes_read = stats_after_cold.bytes_read;
    let cold_requests = stats_after_cold.reads + stats_after_cold.range_reads;

    let warm = if args.passes >= 2 {
        let cpu_start = cpu_ms();
        let wall_started = Instant::now();
        let mut mismatches = 0u64;
        let mut samples = Vec::with_capacity(subset.tensors.len());
        for sel in &subset.tensors {
            let id = *tensor_ids
                .get(&sel.name)
                .ok_or_else(|| format!("no id for `{}`", sel.name))?;
            let op = Instant::now();
            let bytes = store
                .read_tensor(id)
                .map_err(|err| format!("read `{}`: {err}", sel.name))?;
            verified_read(&sel.name, &bytes, &sel.sha256, &mut mismatches)?;
            samples.push(op.elapsed());
        }
        let wall_ms = wall_started.elapsed().as_secs_f64() * 1000.0;
        let cpu = cpu_ms() - cpu_start;
        let stats_after_warm = store.backend_stats();
        json!({
            "basis": "warm (same process, store already open, OS/backend cache warm)",
            "wall_ms": wall_ms,
            "cpu_user_sys_ms": cpu,
            "peak_rss_kib": peak_rss_kib(),
            "per_tensor": latency_summary(&samples),
            "backend_bytes_read": stats_after_warm.bytes_read - stats_after_cold.bytes_read,
            "backend_read_requests": (stats_after_warm.reads + stats_after_warm.range_reads)
                - (stats_after_cold.reads + stats_after_cold.range_reads),
            "mismatches": mismatches,
        })
    } else {
        Json::Null
    };

    Ok(json!({
        "schema": "weft.augmentation.phase.v1",
        "phase": "restore",
        "config": config,
        "backend": args.backend,
        "logical_bytes": restored_bytes,
        "tensor_count": subset.tensors.len(),
        "open_ms": open_ms,
        "protocol_ms": protocol_ms,
        "read_ms": cold_read_ms,
        "cold": merge_cold(cold, cold_bytes_read, cold_requests, stats_after_open),
        "warm": warm,
        "protocol": protocol,
        "exactness": {
            "tensors_verified": subset.tensors.len(),
            "mismatches": 0,
            "match": true,
            "restored_logical_bytes": restored_bytes,
            "expected_logical_bytes": subset.logical_bytes,
        },
        "backend_stats_total": stats_json(&store.backend_stats()),
    }))
}

fn merge_cold(cold: Json, bytes_read: u64, requests: u64, stats_after_open: BackendStats) -> Json {
    let mut object = cold.as_object().cloned().unwrap_or_default();
    object.insert("backend_bytes_read".to_owned(), json!(bytes_read));
    object.insert("backend_read_requests".to_owned(), json!(requests));
    object.insert(
        "backend_bytes_read_at_open".to_owned(),
        json!(stats_after_open.bytes_read),
    );
    object.insert("includes_open".to_owned(), json!(true));
    Json::Object(object)
}

fn restore_native_local(args: &Args, config: &str, subset: &Subset) -> Result<Json, String> {
    let dir = config_native_path(&args.work.clone().unwrap(), config);
    if args.backend == "local" {
        fadvise_dontneed(&dir);
    }
    if config == NATIVE_APP {
        let mut metadata_reads = 0u64;
        let mut metadata_bytes = 0u64;
        {
            let mut read_json = |name: &str| -> Result<Json, String> {
                let path = dir.join(name);
                let bytes = fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
                metadata_reads += 1;
                metadata_bytes = metadata_bytes.saturating_add(bytes.len() as u64);
                serde_json::from_slice(&bytes).map_err(|err| format!("{}: {err}", path.display()))
            };
            let manifest = read_json("manifest.json")?;
            let provenance = read_json("provenance.json")?;
            let context = read_json("context.json")?;
            check_native_manifest(&manifest, subset)?;
            check_native_context(&context, subset)?;
            check_native_provenance(&provenance, subset)?;
        }
        return restore_native_pass_local(
            args,
            config,
            subset,
            &dir,
            metadata_reads,
            metadata_bytes,
        );
    }
    restore_native_pass_local(args, config, subset, &dir, 0, 0)
}

fn restore_native_pass_local(
    args: &Args,
    config: &str,
    subset: &Subset,
    dir: &Path,
    metadata_reads: u64,
    metadata_bytes: u64,
) -> Result<Json, String> {
    let pass = |label: &str| -> Result<Json, String> {
        let mut mismatches = 0u64;
        let mut samples = Vec::with_capacity(subset.tensors.len());
        let mut bytes_read = 0u64;
        let cpu_start = cpu_ms();
        let wall_started = Instant::now();
        for sel in &subset.tensors {
            let path = dir.join(&sel.file);
            let op = Instant::now();
            let bytes = fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            verified_read(&sel.name, &bytes, &sel.sha256, &mut mismatches)?;
            bytes_read = bytes_read.saturating_add(bytes.len() as u64);
            samples.push(op.elapsed());
        }
        let wall_ms = wall_started.elapsed().as_secs_f64() * 1000.0;
        let cpu = cpu_ms() - cpu_start;
        Ok(json!({
            "basis": label,
            "wall_ms": wall_ms,
            "cpu_user_sys_ms": cpu,
            "peak_rss_kib": peak_rss_kib(),
            "per_tensor": latency_summary(&samples),
            "restored_bytes": bytes_read,
            "mismatches": mismatches,
            "requests": subset.tensors.len() as u64 + metadata_reads,
            "bytes_read": bytes_read + metadata_bytes,
        }))
    };

    let cold = pass("cold (fresh process; local files fadvise(DONTNEED) before read)")?;
    let warm = if args.passes >= 2 {
        pass("warm (same process, OS page cache warm)")?
    } else {
        Json::Null
    };
    let restored_bytes = cold["restored_bytes"].as_u64().unwrap_or(0);
    Ok(json!({
        "schema": "weft.augmentation.phase.v1",
        "phase": "restore",
        "config": config,
        "backend": args.backend,
        "logical_bytes": restored_bytes,
        "tensor_count": subset.tensors.len(),
        "open_ms": 0.0,
        "protocol_ms": 0.0,
        "read_ms": cold["wall_ms"],
        "cold": cold,
        "warm": warm,
        "protocol": if config == NATIVE_APP {
            json!({
                "manifest_verified": true,
                "context_key_set_verified": true,
                "provenance_verified": true,
                "metadata_reads": metadata_reads,
            })
        } else { Json::Null },
        "exactness": {
            "tensors_verified": subset.tensors.len(),
            "mismatches": 0,
            "match": true,
            "restored_logical_bytes": restored_bytes,
            "expected_logical_bytes": subset.logical_bytes,
        },
    }))
}

fn restore_native_s3(args: &Args, config: &str, subset: &Subset) -> Result<Json, String> {
    let config_s3 = s3_config(args)?;
    let backend = S3Backend::new(
        config_store_path(&args.work.clone().unwrap(), config),
        config_s3,
    );
    let mut metadata_reads = 0u64;
    let mut metadata_bytes = 0u64;
    if config == NATIVE_APP {
        {
            let mut read_key = |name: &str| -> Result<Json, String> {
                let bytes = backend
                    .read_all(&Key::Manifest {
                        hash: native_app_s3_key(name, subset),
                    })
                    .map_err(|err| format!("GET {name}: {err}"))?;
                metadata_reads += 1;
                metadata_bytes = metadata_bytes.saturating_add(bytes.len() as u64);
                serde_json::from_slice(&bytes).map_err(|err| format!("{name}: {err}"))
            };
            let manifest = read_key("manifest")?;
            let provenance = read_key("provenance")?;
            let context = read_key("context")?;
            check_native_manifest(&manifest, subset)?;
            check_native_context(&context, subset)?;
            check_native_provenance(&provenance, subset)?;
        }
    }

    let pass = |label: &str| -> Result<Json, String> {
        let mut mismatches = 0u64;
        let mut samples = Vec::with_capacity(subset.tensors.len());
        let mut bytes_read = 0u64;
        let cpu_start = cpu_ms();
        let wall_started = Instant::now();
        for sel in &subset.tensors {
            let hash = Hash32::from_hex(&sel.key).map_err(|err| err.to_string())?;
            let op = Instant::now();
            let bytes = backend
                .read_all(&Key::Object { hash })
                .map_err(|err| format!("GET `{}`: {err}", sel.name))?;
            verified_read(&sel.name, &bytes, &sel.sha256, &mut mismatches)?;
            bytes_read = bytes_read.saturating_add(bytes.len() as u64);
            samples.push(op.elapsed());
        }
        let wall_ms = wall_started.elapsed().as_secs_f64() * 1000.0;
        let cpu = cpu_ms() - cpu_start;
        Ok(json!({
            "basis": label,
            "wall_ms": wall_ms,
            "cpu_user_sys_ms": cpu,
            "peak_rss_kib": peak_rss_kib(),
            "per_tensor": latency_summary(&samples),
            "restored_bytes": bytes_read,
            "mismatches": mismatches,
            "requests": subset.tensors.len() as u64 + metadata_reads,
            "bytes_read": bytes_read + metadata_bytes,
        }))
    };

    let cold = pass("cold (fresh process; no cache control on a remote endpoint)")?;
    let warm = if args.passes >= 2 {
        pass("warm (same process, repeated GET; server and kernel caches warm)")?
    } else {
        Json::Null
    };
    let restored_bytes = cold["restored_bytes"].as_u64().unwrap_or(0);
    Ok(json!({
        "schema": "weft.augmentation.phase.v1",
        "phase": "restore",
        "config": config,
        "backend": args.backend,
        "logical_bytes": restored_bytes,
        "tensor_count": subset.tensors.len(),
        "open_ms": 0.0,
        "protocol_ms": 0.0,
        "read_ms": cold["wall_ms"],
        "cold": cold,
        "warm": warm,
        "protocol": if config == NATIVE_APP {
            json!({
                "manifest_verified": true,
                "context_key_set_verified": true,
                "provenance_verified": true,
                "metadata_reads": metadata_reads,
            })
        } else { Json::Null },
        "exactness": {
            "tensors_verified": subset.tensors.len(),
            "mismatches": 0,
            "match": true,
            "restored_logical_bytes": restored_bytes,
            "expected_logical_bytes": subset.logical_bytes,
        },
        "backend_stats_total": stats_json(&backend.stats()),
    }))
}

// ---------------------------------------------------------------------------
// S3 helpers
// ---------------------------------------------------------------------------

fn store_kinds() -> Vec<KeyKind> {
    vec![
        KeyKind::Commit,
        KeyKind::Object,
        KeyKind::Manifest,
        KeyKind::Index,
        KeyKind::SnapshotRef,
        KeyKind::Tag,
        KeyKind::Tmp,
        KeyKind::Access,
        KeyKind::Pack,
        KeyKind::PackCatalog,
    ]
}

fn s3_footprint(
    config: &S3Config,
    kinds: &[KeyKind],
    include_controls: bool,
) -> Result<(u64, u64), String> {
    let backend = S3Backend::new("/weft-augmentation-footprint", config.clone());
    let mut bytes = 0u64;
    let mut objects = 0u64;
    for kind in kinds {
        match backend.list_prefix(&Key::prefix(*kind)) {
            Ok(keys) => {
                for key in keys {
                    if let Ok(len) = backend.len(&key) {
                        bytes = bytes.saturating_add(len);
                        objects += 1;
                    }
                }
            }
            Err(err) if err.class() == weftfs::error::Class::NotFound => {}
            Err(err) => return Err(format!("list {:?}: {err}", kind)),
        }
    }
    if include_controls {
        for key in [
            Key::format(),
            Key::superblock(),
            Key::lock(),
            Key::quota(),
            Key::quota_migrated(),
            Key::access_counts(),
            Key::access_journal(),
            Key::pack_catalog_root(),
        ] {
            if let Ok(len) = backend.len(&key) {
                bytes = bytes.saturating_add(len);
                objects += 1;
            }
        }
    }
    Ok((bytes, objects))
}

fn s3_cleanup(args: &Args) -> Result<(), String> {
    let config = s3_config(args)?;
    let mut backend = S3Backend::new(
        config_store_path(&args.work.clone().unwrap(), "cleanup"),
        config.clone(),
    );
    let mut kinds = store_kinds();
    kinds.push(KeyKind::Format);
    kinds.push(KeyKind::Lock);
    kinds.push(KeyKind::Superblock);
    let mut deleted = 0u64;
    let mut failed = 0u64;
    for kind in kinds {
        match backend.list_prefix(&Key::prefix(kind)) {
            Ok(keys) => {
                for key in keys {
                    match backend.delete(&key) {
                        Ok(()) => deleted += 1,
                        Err(err) => {
                            failed += 1;
                            eprintln!("cleanup: delete {} failed: {err}", key.display_path());
                        }
                    }
                }
            }
            Err(err) if err.class() == weftfs::error::Class::NotFound => {}
            Err(err) => {
                failed += 1;
                eprintln!("cleanup: list {:?} failed: {err}", kind);
            }
        }
    }
    for key in [
        Key::format(),
        Key::superblock(),
        Key::lock(),
        Key::quota(),
        Key::quota_migrated(),
        Key::access_counts(),
        Key::access_journal(),
        Key::pack_catalog_root(),
    ] {
        match backend.delete(&key) {
            Ok(()) => deleted += 1,
            Err(err) if err.class() == weftfs::error::Class::NotFound => {}
            Err(err) => {
                failed += 1;
                eprintln!("cleanup: delete {} failed: {err}", key.display_path());
            }
        }
    }
    let (remaining, objects) = s3_footprint(&config, &store_kinds(), true)?;
    eprintln!(
        "cleanup: deleted {deleted} objects, {failed} failures; remaining {objects} objects, {remaining} bytes"
    );
    if failed > 0 {
        return Err(format!(
            "cleanup left {failed} failed deletions in bucket {}",
            config.bucket
        ));
    }
    Ok(())
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

// ---------------------------------------------------------------------------
// Parent: one command, fresh child per phase
// ---------------------------------------------------------------------------

fn append_log(log: &mut File, text: &str) {
    let _ = writeln!(log, "{text}");
    let _ = log.flush();
}

fn run_child(args: &Args, phase: &str, config: &str, log: &mut File) -> Result<Json, String> {
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
    let work = args.work.clone().unwrap();
    let phase_dir = work.join("phases");
    fs::create_dir_all(&phase_dir).map_err(|err| format!("{}: {err}", phase_dir.display()))?;
    let phase_out = phase_dir.join(format!("{config}-{phase}.json"));
    let child_log = phase_dir.join(format!("{config}-{phase}.log"));
    let subset = work.join("subset.json");

    let mut command = Command::new(exe);
    command
        .arg(phase)
        .arg("--work")
        .arg(&work)
        .arg("--subset")
        .arg(&subset)
        .arg("--config")
        .arg(config)
        .arg("--backend")
        .arg(&args.backend)
        .arg("--phase-out")
        .arg(&phase_out)
        .arg("--chunk-size")
        .arg(args.chunk_size.to_string())
        .arg("--seed")
        .arg(&args.seed)
        .arg("--passes")
        .arg(args.passes.to_string());
    // One bucket per configuration so no configuration can observe or reclaim
    // another's objects, and so the MinIO tmpfs is bounded one run at a time.
    if let Some(bucket) = &args.s3_bucket {
        command
            .arg("--s3-bucket")
            .arg(format!("{}-{}", bucket, config.to_ascii_lowercase()));
    }
    let log_file =
        File::create(&child_log).map_err(|err| format!("{}: {err}", child_log.display()))?;
    let err_file = log_file.try_clone().map_err(|err| err.to_string())?;
    command.stdout(Stdio::from(log_file));
    command.stderr(Stdio::from(err_file));
    let status = command
        .status()
        .map_err(|err| format!("spawn {phase} {config}: {err}"))?;
    let text = fs::read(&child_log).unwrap_or_default();
    append_log(
        log,
        &format!(
            "--- {phase} {config} (exit {:?}) ---\n{}",
            status.code(),
            String::from_utf8_lossy(&text)
        ),
    );
    if !status.success() {
        return Err(format!(
            "{phase} {config} exited with {:?} (see {})",
            status.code(),
            child_log.display()
        ));
    }
    if phase == "cleanup" {
        return Ok(json!(null));
    }
    let bytes = fs::read(&phase_out).map_err(|err| format!("{}: {err}", phase_out.display()))?;
    serde_json::from_slice(&bytes).map_err(|err| format!("{}: {err}", phase_out.display()))
}

fn config_dir(args: &Args, config: &str) -> PathBuf {
    args.work.clone().unwrap().join(config)
}

fn remove_config_dir(args: &Args, config: &str) -> Result<(), String> {
    let path = config_dir(args, config);
    if path.exists() {
        fs::remove_dir_all(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(())
}

fn summarise(entry: &Json) -> Json {
    let write = &entry["write"];
    let restore = &entry["restore"];
    let logical = write["logical_bytes"].as_u64().unwrap_or(0);
    let physical_written = write["physical_bytes_written"].as_u64().unwrap_or(0);
    let footprint = write["storage"]["physical_footprint_bytes"]
        .as_u64()
        .or_else(|| write["physical_footprint_bytes"].as_u64())
        .unwrap_or(0);
    let write_wall = write["wall_ms"].as_f64().unwrap_or_else(|| {
        write["init_ms"].as_f64().unwrap_or(0.0)
            + write["source_read_ms"].as_f64().unwrap_or(0.0)
            + write["write_ms"].as_f64().unwrap_or(0.0)
            + write["protocol_ms"].as_f64().unwrap_or(0.0)
    });
    let cold_wall = restore["cold"]["wall_ms"].as_f64().unwrap_or(0.0);
    let cold_bytes = restore["cold"]["backend_bytes_read"]
        .as_u64()
        .or_else(|| restore["cold"]["bytes_read"].as_u64())
        .unwrap_or(0);
    let cold_requests = restore["cold"]["backend_read_requests"]
        .as_u64()
        .or_else(|| restore["cold"]["requests"].as_u64())
        .unwrap_or(0);
    let write_requests = write["backend_stats"]["writes"]
        .as_u64()
        .or_else(|| write["native_writes"].as_u64())
        .map(|count| count + write["metadata_objects"].as_u64().unwrap_or(0))
        .unwrap_or(0);
    json!({
        "logical_bytes": logical,
        "physical_bytes_written": physical_written,
        "physical_footprint_bytes": footprint,
        "write_wall_ms": write_wall,
        "write_requests": write_requests,
        "write_amplification": if logical > 0 { json!(physical_written as f64 / logical as f64) } else { Json::Null },
        "cold_restore_wall_ms": cold_wall,
        "cold_physical_bytes_read": cold_bytes,
        "cold_read_requests": cold_requests,
        "read_amplification": if logical > 0 { json!(cold_bytes as f64 / logical as f64) } else { Json::Null },
        "logical_restore_bytes_per_s": if cold_wall > 0.0 { json!(logical as f64 / (cold_wall / 1000.0)) } else { Json::Null },
        "physical_restore_bytes_per_s": if cold_wall > 0.0 { json!(cold_bytes as f64 / (cold_wall / 1000.0)) } else { Json::Null },
        "write_peak_rss_kib": write["peak_rss_kib"].as_u64(),
        "restore_peak_rss_kib": restore["cold"]["peak_rss_kib"].as_u64(),
        "write_cpu_ms": write["cpu_user_sys_ms"].as_f64(),
        "restore_cpu_ms": restore["cold"]["cpu_user_sys_ms"].as_f64(),
        "restore_per_tensor": restore["cold"]["per_tensor"].clone(),
        "warm_restore_wall_ms": restore["warm"]["wall_ms"].as_f64(),
        "warm_physical_bytes_read": restore["warm"]["backend_bytes_read"]
            .as_u64()
            .or_else(|| restore["warm"]["bytes_read"].as_u64()),
        "chosen_representations": write["representations"].clone(),
        "exactness": restore["exactness"].clone(),
        "protocol": restore["protocol"].clone(),
    })
}

fn deltas(base: &Json, entry: &Json) -> Json {
    let base_latency = base["cold_restore_wall_ms"].as_f64().unwrap_or(0.0);
    let latency = entry["cold_restore_wall_ms"].as_f64().unwrap_or(0.0);
    let base_logical = base["logical_restore_bytes_per_s"].as_f64().unwrap_or(0.0);
    let logical = entry["logical_restore_bytes_per_s"].as_f64().unwrap_or(0.0);
    let base_physical = base["physical_restore_bytes_per_s"].as_f64().unwrap_or(0.0);
    let physical = entry["physical_restore_bytes_per_s"]
        .as_f64()
        .unwrap_or(0.0);
    let base_footprint = base["physical_footprint_bytes"].as_u64().unwrap_or(0) as f64;
    let footprint = entry["physical_footprint_bytes"].as_u64().unwrap_or(0) as f64;
    let base_total = base["write_wall_ms"].as_f64().unwrap_or(0.0) + base_latency;
    let total = entry["write_wall_ms"].as_f64().unwrap_or(0.0) + latency;
    json!({
        "latency_reduction_pct": if base_latency > 0.0 {
            json!(100.0 * (base_latency - latency) / base_latency)
        } else { Json::Null },
        "logical_throughput_improvement_pct": if base_logical > 0.0 {
            json!(100.0 * (logical - base_logical) / base_logical)
        } else { Json::Null },
        "physical_throughput_improvement_pct": if base_physical > 0.0 {
            json!(100.0 * (physical - base_physical) / base_physical)
        } else { Json::Null },
        "storage_reduction_pct": if base_footprint > 0.0 {
            json!(100.0 * (base_footprint - footprint) / base_footprint)
        } else { Json::Null },
        "augmentation_overhead_pct": if base_total > 0.0 {
            json!(100.0 * (total - base_total) / base_total)
        } else { Json::Null },
        "formula_note": "positive reduction/improvement = X better; positive overhead = X costs more end-to-end than A",
    })
}

fn not_measured() -> Json {
    json!([
        {"backend": "3FS", "reason": "No deployed 3FS cluster. The only executed 3FS evidence is an ABI-compatible dlopen shim (docs/THREEFS.md); no cluster, client, or RDMA transport was available in this court."},
        {"backend": "CephFS", "reason": "No CephFS mount or MDS was available. The Ceph evidence in this repository is Ceph RGW, an object-store S3 server, not a POSIX filesystem."},
        {"backend": "Lustre", "reason": "No Lustre server, client, or mount exists in the court."},
        {"backend": "BeeGFS", "reason": "No BeeGFS server, client, or mount exists in the court."},
        {"backend": "DAOS", "reason": "No DAOS server, client library, or pool exists in the court."},
        {"backend": "JuiceFS", "reason": "No JuiceFS binary, metadata engine, or object store was configured."},
        {"backend": "IBM Storage Scale", "reason": "Not installed and no licensed client or cluster is available."},
        {"backend": "WEKA", "reason": "Not installed and no licensed client or cluster is available."},
        {"backend": "VAST", "reason": "Not installed and no appliance or client is available."},
        {"backend": "AIStore", "reason": "Not installed and no cluster is available."},
        {"backend": "live AWS S3", "reason": "No credentials or bucket. This receipt is MinIO on loopback, not AWS."},
        {"backend": "live Google Cloud Storage", "reason": "No credentials or bucket. The GCS evidence in this repository is fake-gcs-server and a service-account unit court."},
        {"backend": "live Azure Blob", "reason": "No credentials or account. The Azure evidence in this repository is Azurite."},
        {"backend": "physical power loss / host cache loss", "reason": "The court is a process-level measurement; crash and power-loss courts are separate and no media claim is made here."},
        {"backend": "multi-node / fleet", "reason": "One host, one server process, one writer. No distributed publication claim."},
    ])
}

fn assemble(args: &Args, entries: &BTreeMap<String, Json>, log_path: &Path) -> Json {
    let mut configurations = serde_json::Map::new();
    for (config, entry) in entries {
        configurations.insert(
            config.clone(),
            json!({
                "write": entry["write"],
                "restore": entry["restore"],
                "summary": summarise(entry),
            }),
        );
    }
    let mut delta_map = serde_json::Map::new();
    let base = entries
        .get("A")
        .map(|entry| summarise(entry))
        .ok_or_else(|| "missing configuration A".to_owned());
    let base = match base {
        Ok(value) => value,
        Err(_) => json!(null),
    };
    if let Some(a) = entries.get("A") {
        let base_summary = summarise(a);
        for (config, entry) in entries {
            if config == "A" {
                continue;
            }
            delta_map.insert(config.clone(), deltas(&base_summary, &summarise(entry)));
        }
    }

    let logicals: BTreeSet<u64> = entries
        .values()
        .map(|entry| entry["write"]["logical_bytes"].as_u64().unwrap_or(0))
        .collect();
    let all_exact = entries.values().all(|entry| {
        entry["restore"]["exactness"]["match"]
            .as_bool()
            .unwrap_or(false)
            && entry["restore"]["exactness"]["mismatches"]
                .as_u64()
                .unwrap_or(1)
                == 0
    });
    let required = CONFIGS.iter().all(|config| entries.contains_key(*config));
    let protocol_ok = ["D", "E"].iter().all(|config| {
        entries
            .get(*config)
            .map(|entry| {
                entry["restore"]["protocol"]["context_resolved"]
                    .as_bool()
                    .unwrap_or(false)
                    && entry["restore"]["protocol"]["provenance_verified"]
                        .as_bool()
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    let n_exact = entries
        .get(NATIVE_APP)
        .map(|entry| {
            entry["restore"]["exactness"]["match"]
                .as_bool()
                .unwrap_or(false)
        })
        .unwrap_or(true);
    let metrics_complete = entries.values().all(|entry| {
        let write = &entry["write"];
        let restore = &entry["restore"];
        let footprint_present = write["storage"]["physical_footprint_bytes"].is_u64()
            || write["physical_footprint_bytes"].is_u64();
        let reads_present = restore["cold"]["backend_bytes_read"].is_u64()
            || restore["cold"]["bytes_read"].is_u64();
        write["physical_bytes_written"].is_u64()
            && footprint_present
            && restore["cold"]["wall_ms"].is_f64()
            && restore["cold"]["peak_rss_kib"].is_u64()
            && reads_present
    });
    let mut checks = serde_json::Map::new();
    checks.insert(
        "all_five_configurations_present".to_owned(),
        json!(required),
    );
    checks.insert(
        "identical_logical_bytes".to_owned(),
        json!(logicals.len() == 1),
    );
    checks.insert("all_restored_tensors_exact".to_owned(), json!(all_exact));
    checks.insert(
        "protocol_configurations_verified".to_owned(),
        json!(protocol_ok),
    );
    checks.insert("supplementary_native_app_exact".to_owned(), json!(n_exact));
    checks.insert(
        "mandatory_metrics_present".to_owned(),
        json!(metrics_complete),
    );
    checks.insert("not_measured_declared".to_owned(), json!(true));
    checks.insert(
        "base_configuration_present".to_owned(),
        json!(base != json!(null)),
    );
    let pass = checks
        .values()
        .all(|value| value.as_bool().unwrap_or(false));

    json!({
        "schema": "weft.augmentation.v1",
        "generated_utc": utc_stamp(),
        "result": if pass { "pass" } else { "fail" },
        "scope": "single host, one backend, OOM-protected Docker court. Not a cloud, fleet, media, or power-loss claim.",
        "contract": "bench/CONTRACTS-augmentation.md",
        "configuration": {
            "label": args.label,
            "reduced": args.reduced,
            "backend": args.backend,
            "chunk_size": args.chunk_size,
            "seed": args.seed,
            "fixed_timestamp_ns": FIXED_TIMESTAMP_NS,
            "restore_passes": args.passes,
            "minio_restart_between_configurations": std::env::var("WEFT_AUG_MINIO_RESTART").is_ok(),
            "configs": if args.with_native_app {
                json!(["A", "B", "C", "D", "E", "N"])
            } else {
                json!(["A", "B", "C", "D", "E"])
            },
            "supplementary_native_app": args.with_native_app,
            "subset_rule": "embedding + evenly spaced N name-sorted non-embedding tensors",
            "max_others": args.max_others,
            "include_embedding": args.include_embedding,
        },
        "environment": limit_label(),
        "subset": {
            "schema": "weft.augmentation.subset.v1",
            "selection": entries.values().next().and_then(|e| e.get("subset_selection")).cloned(),
            "tensor_count": entries.values().next().and_then(|e| e.get("subset_tensor_count")).cloned(),
        },
        "mandatory_metrics": [
            "durable_write_wall_ms", "logical_bytes", "physical_bytes_written",
            "physical_bytes_written_source", "physical_footprint_bytes",
            "footprint_source", "cold_restore_wall_ms", "cold_physical_bytes_read",
            "cold_read_requests", "warm_restore_wall_ms", "warm_physical_bytes_read",
            "p50_us", "p95_us", "p99_us", "peak_rss_kib", "cpu_user_sys_ms",
            "write_requests", "write_amplification", "read_amplification",
            "logical_restore_bytes_per_s", "physical_restore_bytes_per_s",
            "exactness"
        ],
        "formulas": {
            "latency_reduction_pct": "100*(A.cold_restore_wall_ms - X.cold_restore_wall_ms)/A.cold_restore_wall_ms",
            "throughput_improvement_pct": "100*(X.logical_restore_bytes_per_s - A.logical_restore_bytes_per_s)/A.logical_restore_bytes_per_s; physical-byte throughput is reported separately",
            "storage_reduction_pct": "100*(A.physical_footprint_bytes - X.physical_footprint_bytes)/A.physical_footprint_bytes",
            "augmentation_overhead_pct": "100*((X.write_wall_ms + X.cold_restore_wall_ms) - (A.write_wall_ms + A.cold_restore_wall_ms))/(A.write_wall_ms + A.cold_restore_wall_ms)"
        },
        "configurations": configurations,
        "deltas_vs_A": delta_map,
        "checks": checks,
        "not_measured": not_measured(),
        "raw_log": log_path.display().to_string(),
        "methodology": {
            "write_wall": "source read + store init + encode + index build + every commit + protocol + final durability barrier",
            "cold_restore": "fresh process; local store files fadvise(DONTNEED) before open; includes open, protocol verification, every tensor read and SHA-256 check",
            "warm_restore": "second pass in the same process; OS/backend cache warm; no store cache is enabled",
            "physical_bytes_written": "backend counters for WeftFS (probe, LOCK, FORMAT, superblock, commits, manifests, chunks/frames, index segments) or harness PUT/file counters for A/N",
            "physical_footprint": "recursive file walk for local, object listing for S3",
            "logical_bytes": "exact source tensor payload bytes in the subset",
        },
    })
}

fn utc_stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Minimal UTC formatter: days since epoch -> civil date (Howard Hinnant).
    let days = (now / 86_400) as i64;
    let secs = now % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn assemble_cmd(args: &Args) -> Result<(), String> {
    let work = args
        .work
        .clone()
        .ok_or_else(|| "--work is required".to_owned())?;
    let out = args
        .out
        .clone()
        .ok_or_else(|| "--out is required".to_owned())?;
    let log_path = args
        .log
        .clone()
        .unwrap_or_else(|| out.with_extension("log"));
    let subset = load_subset(&work.join("subset.json"))?;
    let mut configs: Vec<String> = CONFIGS.iter().map(|c| (*c).to_owned()).collect();
    if args.with_native_app {
        configs.push(NATIVE_APP.to_owned());
    }
    let mut entries: BTreeMap<String, Json> = BTreeMap::new();
    let mut log =
        File::create(&log_path).map_err(|err| format!("{}: {err}", log_path.display()))?;
    append_log(
        &mut log,
        &format!(
            "augmentation: assembled from {} phases in {}",
            configs.len(),
            work.join("phases").display()
        ),
    );
    for config in &configs {
        let write_path = work.join("phases").join(format!("{config}-write.json"));
        let restore_path = work.join("phases").join(format!("{config}-restore.json"));
        let write_bytes =
            fs::read(&write_path).map_err(|err| format!("{}: {err}", write_path.display()))?;
        let restore_bytes =
            fs::read(&restore_path).map_err(|err| format!("{}: {err}", restore_path.display()))?;
        let write: Json = serde_json::from_slice(&write_bytes)
            .map_err(|err| format!("{}: {err}", write_path.display()))?;
        let restore: Json = serde_json::from_slice(&restore_bytes)
            .map_err(|err| format!("{}: {err}", restore_path.display()))?;
        append_log(
            &mut log,
            &format!(
                "augmentation: {config} write {:.1} ms / restore {:.1} ms / written {} / footprint {} / exact={}",
                write["wall_ms"].as_f64().unwrap_or(0.0),
                restore["cold"]["wall_ms"].as_f64().unwrap_or(0.0),
                write["physical_bytes_written"].as_u64().unwrap_or(0),
                write["storage"]["physical_footprint_bytes"]
                    .as_u64()
                    .or_else(|| write["physical_footprint_bytes"].as_u64())
                    .unwrap_or(0),
                restore["exactness"]["match"].as_bool().unwrap_or(false)
            ),
        );
        let phase_log = work.join("phases").join(format!("{config}-write.log"));
        if let Ok(text) = fs::read_to_string(phase_log) {
            append_log(&mut log, &text);
        }
        let phase_log = work.join("phases").join(format!("{config}-restore.log"));
        if let Ok(text) = fs::read_to_string(phase_log) {
            append_log(&mut log, &text);
        }
        entries.insert(config.clone(), json!({"write": write, "restore": restore}));
    }
    let mut receipt = assemble(args, &entries, &log_path);
    attach_subset(&mut receipt, &subset, &configs);
    write_json_atomic(&out, &receipt)?;
    let result = receipt["result"].as_str().unwrap_or("fail");
    append_log(
        &mut log,
        &format!("augmentation: result={result} receipt={}", out.display()),
    );
    if result != "pass" {
        return Err(format!(
            "assembled receipt result is `{result}` ({})",
            out.display()
        ));
    }
    Ok(())
}

fn run(args: Args) -> Result<(), String> {
    let fixture = args
        .fixture
        .clone()
        .ok_or_else(|| "--fixture is required".to_owned())?;
    let work = args
        .work
        .clone()
        .ok_or_else(|| "--work is required".to_owned())?;
    let out = args
        .out
        .clone()
        .ok_or_else(|| "--out is required".to_owned())?;
    let log_path = args
        .log
        .clone()
        .unwrap_or_else(|| out.with_extension("log"));
    if work.exists() {
        if !args.force {
            return Err(format!(
                "{} already exists; pass --force to replace it",
                work.display()
            ));
        }
        fs::remove_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    }
    fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    let mut log =
        File::create(&log_path).map_err(|err| format!("{}: {err}", log_path.display()))?;
    append_log(
        &mut log,
        &format!(
            "augmentation: backend={} configs={} subset(max_others={}, include_embedding={}) passes={} label={}",
            args.backend,
            if args.with_native_app { "A,B,C,D,E,N" } else { "A,B,C,D,E" },
            args.max_others,
            args.include_embedding,
            args.passes,
            args.label
        ),
    );
    append_log(&mut log, &format!("augmentation: work={}", work.display()));

    // 1. Verify the whole fixture and freeze the subset.
    let parse_started = Instant::now();
    let file =
        SafetensorsFile::parse(&fixture).map_err(|err| format!("{}: {err}", fixture.display()))?;
    let actual = hex::encode(file.sha256);
    append_log(
        &mut log,
        &format!(
            "augmentation: fixture {} bytes={} sha256={} (parse {:.1} ms)",
            fixture.display(),
            file.file_bytes,
            actual,
            parse_started.elapsed().as_secs_f64() * 1000.0
        ),
    );
    if actual != FIXTURE_SHA256 {
        return Err(format!(
            "fixture SHA-256 mismatch: expected {FIXTURE_SHA256}, got {actual}"
        ));
    }
    let subset = select_subset(&file, args.include_embedding, args.max_others)?;
    if subset.logical_bytes == 0 || subset.tensors.is_empty() {
        return Err("empty subset".to_owned());
    }
    let subset_path = work.join("subset.json");
    write_json_atomic(
        &subset_path,
        &serde_json::to_value(&subset).map_err(|err| err.to_string())?,
    )?;
    append_log(
        &mut log,
        &format!(
            "augmentation: subset {} tensors, {} logical bytes, digest {}",
            subset.tensors.len(),
            subset.logical_bytes,
            subset.subset_digest
        ),
    );

    let mut configs: Vec<String> = CONFIGS.iter().map(|c| (*c).to_owned()).collect();
    if args.with_native_app {
        configs.push(NATIVE_APP.to_owned());
    }

    let mut entries: BTreeMap<String, Json> = BTreeMap::new();
    let mut failure: Option<String> = None;
    for config in &configs {
        append_log(
            &mut log,
            &format!("augmentation: === configuration {config} ==="),
        );
        let write_result = run_child(&args, "write", config, &mut log);
        let write = match write_result {
            Ok(value) => value,
            Err(err) => {
                failure = Some(err);
                break;
            }
        };
        let restore_result = run_child(&args, "restore", config, &mut log);
        let restore = match restore_result {
            Ok(value) => value,
            Err(err) => {
                failure = Some(err);
                break;
            }
        };
        entries.insert(
            config.clone(),
            json!({
                "write": write,
                "restore": restore,
            }),
        );
        if args.backend == "s3" {
            if let Err(err) = run_child(&args, "cleanup", config, &mut log) {
                append_log(
                    &mut log,
                    &format!("augmentation: cleanup {config} failed: {err}"),
                );
            }
        }
        if let Err(err) = remove_config_dir(&args, config) {
            append_log(
                &mut log,
                &format!("augmentation: cleanup local {config}: {err}"),
            );
        }
    }

    let mut receipt = if let Some(err) = &failure {
        json!({
            "schema": "weft.augmentation.v1",
            "generated_utc": utc_stamp(),
            "result": "fail",
            "error": err,
            "contract": "bench/CONTRACTS-augmentation.md",
            "configuration": {
                "label": args.label,
                "backend": args.backend,
                "configs": configs,
            },
            "environment": limit_label(),
            "raw_log": log_path.display().to_string(),
        })
    } else {
        assemble(&args, &entries, &log_path)
    };
    attach_subset(&mut receipt, &subset, &configs);
    write_json_atomic(&out, &receipt)?;
    let result = receipt["result"].as_str().unwrap_or("fail");
    append_log(
        &mut log,
        &format!("augmentation: result={result} receipt={}", out.display()),
    );
    if let Some(err) = failure {
        return Err(format!("{err}; partial receipt at {}", out.display()));
    }
    Ok(())
}

/// Attach the subset metadata that the assembler cannot see from phase files.
fn attach_subset(receipt: &mut Json, subset: &Subset, configs: &[String]) {
    if let Some(object) = receipt.as_object_mut() {
        object.insert(
            "subset".to_owned(),
            json!({
                "schema": subset.schema,
                "source": subset.source,
                "source_sha256": subset.source_sha256,
                "source_bytes": subset.source_bytes,
                "total_source_tensors": subset.total_source_tensors,
                "selection": subset.selection,
                "include_embedding": subset.include_embedding,
                "other_tensors": subset.other_tensors,
                "tensor_count": subset.tensors.len(),
                "logical_bytes": subset.logical_bytes,
                "subset_digest": subset.subset_digest,
                "tensors": subset.tensors,
            }),
        );
        object.insert("configs_requested".to_owned(), json!(configs));
    }
}

fn main() {
    // The court runs as uid 0 with cap-drop=ALL. New directories and files
    // must be removable by the host user that owns the bind-mounted work tree,
    // so the court sets a permissive umask. Outside the court the process
    // leaves the caller's umask alone.
    if std::env::var("WEFT_IN_COURT").is_ok() {
        unsafe {
            libc::umask(0o000);
        }
    }
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("augmentation_bench: {err}");
            std::process::exit(64);
        }
    };
    let result = match args.subcommand.as_str() {
        "run" => run(args.clone()),
        "prepare" => (|| {
            let fixture = args
                .fixture
                .clone()
                .ok_or_else(|| "--fixture is required".to_owned())?;
            let work = args
                .work
                .clone()
                .ok_or_else(|| "--work is required".to_owned())?;
            let file = SafetensorsFile::parse(&fixture).map_err(|err| err.to_string())?;
            let actual = hex::encode(file.sha256);
            if actual != FIXTURE_SHA256 {
                return Err(format!(
                    "fixture SHA-256 mismatch: expected {FIXTURE_SHA256}, got {actual}"
                ));
            }
            let subset = select_subset(&file, args.include_embedding, args.max_others)?;
            fs::create_dir_all(&work).map_err(|err| err.to_string())?;
            write_json_atomic(
                &work.join("subset.json"),
                &serde_json::to_value(&subset).map_err(|err| err.to_string())?,
            )?;
            println!(
                "subset: {} tensors, {} logical bytes, digest {}",
                subset.tensors.len(),
                subset.logical_bytes,
                subset.subset_digest
            );
            Ok(())
        })(),
        "write" => (|| {
            let config = args
                .config
                .clone()
                .ok_or_else(|| "--config is required".to_owned())?;
            let subset = load_subset(&args.subset.clone().ok_or("--subset is required")?)?;
            // The one-time whole-fixture SHA-256 gate is a court integrity
            // check, not an augmentation cost. It is measured and reported
            // separately, and is excluded from every configuration equally.
            let gate_started = Instant::now();
            let fixture = args
                .fixture
                .clone()
                .unwrap_or_else(|| PathBuf::from(&subset.source));
            let file = SafetensorsFile::parse(&fixture).map_err(|err| err.to_string())?;
            let fixture_verify_ms = gate_started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let cpu_start = cpu_ms();
            let phase = match config.as_str() {
                "A" => {
                    if args.backend == "s3" {
                        write_native_s3(&args, &subset, &file)?
                    } else {
                        let dir = config_native_path(&args.work.clone().unwrap(), "A");
                        write_native_local(&dir, &file, &subset)?
                    }
                }
                NATIVE_APP => {
                    if args.backend == "s3" {
                        write_native_app_s3(&args, &subset, &file)?
                    } else {
                        let dir = config_native_path(&args.work.clone().unwrap(), NATIVE_APP);
                        write_native_app_local(&dir, &file, &subset)?
                    }
                }
                other if CONFIGS.contains(&other) => write_weftfs(&args, other, &subset, &file)?,
                other => return Err(format!("unknown configuration `{other}`")),
            };
            let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
            let cpu = cpu_ms() - cpu_start;
            let mut phase = phase;
            phase["schema"] = json!("weft.augmentation.phase.v1");
            phase["phase"] = json!("write");
            phase["config"] = json!(config);
            phase["backend"] = json!(args.backend);
            phase["logical_bytes"] = json!(subset.logical_bytes);
            phase["tensor_count"] = json!(subset.tensors.len());
            phase["wall_ms"] = json!(wall_ms);
            phase["fixture_verify_ms"] = json!(fixture_verify_ms);
            phase["cpu_user_sys_ms"] = json!(cpu);
            phase["peak_rss_kib"] = json!(peak_rss_kib());
            phase["subset_selection"] = json!(subset.selection);
            phase["subset_tensor_count"] = json!(subset.tensors.len());
            if phase.get("physical_bytes_written").is_none() {
                phase["physical_bytes_written"] = phase["native_bytes_written"].clone();
                phase["physical_bytes_written_source"] = json!(
                    "harness-counted file bytes (local) or PUT body bytes (S3), plus metadata objects for N"
                );
            }
            write_json_atomic(&args.phase_out.ok_or("--phase-out is required")?, &phase)?;
            Ok(())
        })(),
        "restore" => (|| {
            let config = args
                .config
                .clone()
                .ok_or_else(|| "--config is required".to_owned())?;
            let subset = load_subset(&args.subset.clone().ok_or("--subset is required")?)?;
            let phase = match config.as_str() {
                "A" | NATIVE_APP => {
                    if args.backend == "s3" {
                        restore_native_s3(&args, &config, &subset)?
                    } else {
                        restore_native_local(&args, &config, &subset)?
                    }
                }
                other if CONFIGS.contains(&other) => {
                    let write_path = args
                        .work
                        .clone()
                        .unwrap()
                        .join("phases")
                        .join(format!("{other}-write.json"));
                    let bytes = fs::read(&write_path)
                        .map_err(|err| format!("{}: {err}", write_path.display()))?;
                    let write_phase: Json = serde_json::from_slice(&bytes)
                        .map_err(|err| format!("{}: {err}", write_path.display()))?;
                    restore_weftfs(&args, other, &subset, &write_phase)?
                }
                other => return Err(format!("unknown configuration `{other}`")),
            };
            write_json_atomic(&args.phase_out.ok_or("--phase-out is required")?, &phase)?;
            Ok(())
        })(),
        "cleanup" => {
            if args.backend == "s3" {
                s3_cleanup(&args)
            } else {
                Ok(())
            }
        }
        "assemble" => assemble_cmd(&args),
        other => Err(format!(
            "unknown subcommand `{other}` (expected run|prepare|write|restore|cleanup|assemble)"
        )),
    };
    if let Err(err) = result {
        eprintln!("augmentation_bench: {err}");
        std::process::exit(1);
    }
}
