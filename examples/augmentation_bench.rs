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
use weftfs::phase_profile::{self, Phase, PhaseSnapshot};
use weftfs::provenance::{
    Determinism, ProvenanceEnvironment, ProvenancePrincipal, ProvenanceRecord,
};
use weftfs::s3::{S3Backend, S3Config};
use weftfs::safetensors::SafetensorsFile;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::tensor_store::{
    FastCodec, PlannerMode, TensorLayoutSpec, TensorPlanOptions, TensorStrategy, TensorWriteSpec,
    WritePolicy,
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
    /// Instrument the shipped phase collector for this child. Off by default
    /// so the contract court's wall numbers are uninstrumented.
    phase_profile: bool,
    /// Emit one row per tensor (plan candidates, per-phase encode wall,
    /// publication latency and durable operations).
    per_tensor_detail: bool,
    /// Repetitions for the `ablate` orchestrator.
    reps: usize,
    /// Comma-separated configuration keys for `ablate` (e.g.
    /// `A,B-group,C,C-noz19`).
    configs: Option<String>,
    /// Official contract-court receipt used to prove the default C row is
    /// unchanged at this revision.
    official_receipt: Option<PathBuf>,
    /// Expected SHA-256 of the fixture. Defaults to the frozen Qwen2.5-0.5B
    /// Instruct container; the cross-family decision-diff court passes the
    /// pinned digest of the family it is running so the gate never silently
    /// accepts a different file.
    fixture_sha256: Option<String>,
    /// Decision-diff only: one repetition, no timing claim. The timing checks
    /// (`at_least_seven_reps`, the same-revision identity gate) are omitted from
    /// the receipt rather than asserted, and the receipt records the mode.
    diff_only: bool,
    /// R21: encode threads for the pipelined group writer. `None` runs the
    /// sequential writer (the exact pre-R21 path) unless the configuration key
    /// carries a `pipe<N>` suffix.
    pipeline_workers: Option<usize>,
    pipeline_staging_bytes: Option<u64>,
    pipeline_encode_bytes: Option<u64>,
    pipeline_publish_bytes: Option<u64>,
    pipeline_worker_bytes: Option<u64>,
    pipeline_sync_bytes: Option<u64>,
    pipeline_candidate_min_bytes: Option<u64>,
    pipeline_cpu_tokens: Option<usize>,
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
            phase_profile: false,
            per_tensor_detail: false,
            reps: 7,
            configs: None,
            official_receipt: None,
            fixture_sha256: None,
            diff_only: false,
            pipeline_workers: None,
            pipeline_staging_bytes: None,
            pipeline_encode_bytes: None,
            pipeline_publish_bytes: None,
            pipeline_worker_bytes: None,
            pipeline_sync_bytes: None,
            pipeline_candidate_min_bytes: None,
            pipeline_cpu_tokens: None,
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
            "--phase-profile" => args.phase_profile = true,
            "--per-tensor-detail" => args.per_tensor_detail = true,
            "--reps" => {
                args.reps = value("--reps")?
                    .parse()
                    .map_err(|_| "--reps must be an integer".to_owned())?
            }
            "--configs" => args.configs = Some(value("--configs")?),
            "--official-receipt" => {
                args.official_receipt = Some(PathBuf::from(value("--official-receipt")?))
            }
            "--fixture-sha256" => args.fixture_sha256 = Some(value("--fixture-sha256")?),
            "--diff-only" => args.diff_only = true,
            "--pipeline-workers" => {
                args.pipeline_workers = Some(
                    value("--pipeline-workers")?
                        .parse()
                        .map_err(|_| "--pipeline-workers must be an integer".to_owned())?,
                )
            }
            "--pipeline-staging-bytes" => {
                args.pipeline_staging_bytes = Some(parse_u64_flag(
                    "--pipeline-staging-bytes",
                    &value("--pipeline-staging-bytes")?,
                )?)
            }
            "--pipeline-encode-bytes" => {
                args.pipeline_encode_bytes = Some(parse_u64_flag(
                    "--pipeline-encode-bytes",
                    &value("--pipeline-encode-bytes")?,
                )?)
            }
            "--pipeline-publish-bytes" => {
                args.pipeline_publish_bytes = Some(parse_u64_flag(
                    "--pipeline-publish-bytes",
                    &value("--pipeline-publish-bytes")?,
                )?)
            }
            "--pipeline-worker-bytes" => {
                args.pipeline_worker_bytes = Some(parse_u64_flag(
                    "--pipeline-worker-bytes",
                    &value("--pipeline-worker-bytes")?,
                )?)
            }
            "--pipeline-sync-bytes" => {
                args.pipeline_sync_bytes = Some(parse_u64_flag(
                    "--pipeline-sync-bytes",
                    &value("--pipeline-sync-bytes")?,
                )?)
            }
            "--pipeline-candidate-min-bytes" => {
                args.pipeline_candidate_min_bytes = Some(parse_u64_flag(
                    "--pipeline-candidate-min-bytes",
                    &value("--pipeline-candidate-min-bytes")?,
                )?)
            }
            "--pipeline-cpu-tokens" => {
                args.pipeline_cpu_tokens = Some(
                    value("--pipeline-cpu-tokens")?
                        .parse()
                        .map_err(|_| "--pipeline-cpu-tokens must be an integer".to_owned())?,
                )
            }
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

/// Parse a byte count, accepting a `k`/`m`/`g` suffix.
fn parse_u64_flag(name: &str, text: &str) -> Result<u64, String> {
    let (digits, scale) = match text.chars().last() {
        Some('k') | Some('K') => (&text[..text.len() - 1], 1024u64),
        Some('m') | Some('M') => (&text[..text.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&text[..text.len() - 1], 1024 * 1024 * 1024),
        _ => (text, 1),
    };
    digits
        .parse::<u64>()
        .map(|value| value.saturating_mul(scale))
        .map_err(|_| format!("{name} must be an integer (optionally k/m/g)"))
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
    backend_kind_publication(args, publication_default())
}

/// The backend for a row, including its packed blob plane when the row packs.
fn backend_kind_publication(args: &Args, publication: Publication) -> Result<BackendKind, String> {
    let base = match args.backend.as_str() {
        "local" => BackendKind::Local,
        "s3" => BackendKind::S3 {
            config: s3_config(args)?,
        },
        other => return Err(format!("unknown backend `{other}` (expected local|s3)")),
    };
    let Some(threshold) = publication.pack_threshold() else {
        return Ok(base);
    };
    // One pack target must be at least the member threshold; 16 MiB is the
    // shipped target, raised only when a swept threshold is larger.
    let pack_target_bytes = (16 * 1024 * 1024).max(threshold);
    BackendKind::packed(
        base,
        weftfs::packs::PackOptions {
            pack_target_bytes,
            member_threshold_bytes: threshold,
            ..weftfs::packs::PackOptions::default()
        },
    )
    .map_err(|err| format!("packed blob plane: {err}"))
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
        "mode9_encode_threads": std::env::var("WEFT_AUG_ENCODE_THREADS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_else(weftfs::tensor_store::default_tensor_encode_threads),
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

fn representation_spec(config: &str, flags_state: &ConfigFlags) -> TensorWriteSpec {
    let ConfigFlags {
        ablate_zstd19,
        ablate_float,
        planner,
        zstd_encode_threads,
        zstd_encode_batch_bytes,
        zstd_frame_chunk,
        zstd19_gate_margin_bytes,
        two_stage_off_zstd19,
        two_stage_off_mode9,
        ..
    } = *flags_state;
    let base = config_base(config);
    let planned = matches!(base, "C" | "E");
    // R19: the intra-tensor mode-9 emission thread bound. The shipped default
    // is `min(available_parallelism, 4)`; `WEFT_AUG_ENCODE_THREADS=1` forces the
    // sequential encoder so the two can be compared in the same session. It
    // never changes an emitted byte or a chosen representation.
    let encode_threads = std::env::var("WEFT_AUG_ENCODE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 1)
        .unwrap_or_else(weftfs::tensor_store::default_tensor_encode_threads);
    if planned {
        let mut plan = TensorPlanOptions {
            write_policy: WritePolicy::Planned,
            semantic_search: false,
            // An explicit two-stage ablation removes one candidate from the
            // shipped two-stage candidate set; the R17 `-nom9`/`-noz19` rows
            // above instead pin the exhaustive planner.
            zstd19: !ablate_zstd19 && !two_stage_off_zstd19,
            float_tensor: !ablate_float && !two_stage_off_mode9,
            planner,
            tensor_encode_threads: encode_threads,
            ..TensorPlanOptions::default()
        };
        if let Some(threads) = zstd_encode_threads {
            plan.zstd_encode_threads = threads;
        }
        if let Some(bytes) = zstd_encode_batch_bytes {
            plan.zstd_encode_batch_bytes = bytes;
        }
        if let Some(chunk) = zstd_frame_chunk {
            plan.zstd_frame_chunk = chunk;
        }
        if let Some(margin) = zstd19_gate_margin_bytes {
            plan.zstd19_gate_margin_bytes = margin;
        }
        TensorWriteSpec {
            strategy_hint: None,
            causal: Vec::new(),
            plan,
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
                planner,
                ..TensorPlanOptions::default()
            },
            tile_shape: None,
        }
    }
}

/// The base configuration of an ablation key: `C-noz19` -> `C`.
fn config_base(key: &str) -> &str {
    key.split('-').next().unwrap_or(key)
}

/// The ablation suffix of a configuration key (`""` for the contract rows).
fn config_suffix(key: &str) -> &str {
    key.split_once('-').map(|(_, suffix)| suffix).unwrap_or("")
}

/// A configuration key that this harness can write or restore.
fn is_weftfs_config(key: &str) -> bool {
    CONFIGS.contains(&config_base(key))
}

/// A configuration that uses the WeftFS store (B/C/D/E and their variants).
/// A and N are native and have no store to profile or crash.
fn is_weft_store_config(key: &str) -> bool {
    matches!(config_base(key), "B" | "C" | "D" | "E")
}

/// Parse an ablation key into (ablate zstd-19, ablate mode 9, one group commit,
/// planner mode).
///
/// `-legacy` is the R18 decision-diff control: it pins the pre-R18 exhaustive
/// planner on the unmodified candidate set. The R17 ablations (`-noz19`,
/// `-nom9`, `-both`) are defined on that same exhaustive planner, because
/// "encode this candidate and throw it away" only has a meaning there; the
/// default rows (`C`, `E`) run the shipped two-stage planner.
fn ablation_flags(key: &str) -> Result<ConfigFlags, String> {
    let base = config_base(key);
    let mut flags = ConfigFlags {
        ablate_zstd19: false,
        ablate_float: false,
        publication: publication_default(),
        planner: PlannerMode::TwoStage,
        pipeline_workers: 0,
        pipeline_sync_bytes: None,
        pipeline_cpu_tokens_flag: None,
        zstd_encode_threads: None,
        zstd_encode_batch_bytes: None,
        zstd_frame_chunk: None,
        zstd19_gate_margin_bytes: None,
        two_stage_off_zstd19: false,
        two_stage_off_mode9: false,
    };
    // Suffixes compose with `-`: `noz19-percommit` is the R17 attribution row
    // with the historical per-commit publication, `pack1m-group` is a packed
    // blob plane without the deferred barrier.
    for part in config_suffix(key)
        .split('-')
        .filter(|part| !part.is_empty())
    {
        match part {
            // R18 decision-diff control: the exhaustive planner, same
            // publication as the default row, so the only difference is the
            // planner and the storage comparison stays exact.
            "legacy" if matches!(base, "C" | "E") => flags.planner = PlannerMode::Exhaustive,
            "noz19" if matches!(base, "C" | "E") => {
                flags.planner = PlannerMode::Exhaustive;
                flags.ablate_zstd19 = true;
            }
            "nom9" if matches!(base, "C" | "E") => {
                flags.planner = PlannerMode::Exhaustive;
                flags.ablate_float = true;
            }
            "both" if matches!(base, "C" | "E") => {
                flags.planner = PlannerMode::Exhaustive;
                flags.ablate_zstd19 = true;
                flags.ablate_float = true;
            }
            // R20 publication rows.
            "percommit" => flags.publication = Publication::PerCommit,
            "group" => flags.publication = Publication::Group,
            "atomic" => flags.publication = Publication::Atomic,
            part if part.starts_with("pack") => {
                flags.publication = Publication::Packed(pack_threshold(part)?)
            }
            // R21: keep one durability batch for the whole group, so the
            // barrier is not overlapped with encoding (the ablation control).
            "nosync" | "sync0" => flags.pipeline_sync_bytes = Some(0),
            // R21: early-barrier granularity in MiB (`sync8`, `sync16`, ...).
            part if part.starts_with("sync") => {
                let mib: u64 = part[4..]
                    .trim_end_matches('m')
                    .parse()
                    .map_err(|_| format!("configuration `{key}` has no numeric sync size"))?;
                flags.pipeline_sync_bytes = Some(mib.saturating_mul(1024 * 1024));
            }
            // R21: CPU tokens (`tokens2`, `tokens3`, ...): busy encoder threads.
            part if part.starts_with("tokens") => {
                let tokens: usize = part[6..]
                    .parse()
                    .map_err(|_| format!("configuration `{key}` has no numeric token count"))?;
                if tokens == 0 {
                    return Err(format!("configuration `{key}` asks for zero CPU tokens"));
                }
                flags.pipeline_cpu_tokens_flag = Some(tokens);
            }
            // R21 pipelined group writer: `pipe1` / `pipe2` / `pipe4` / `pipe8`.
            part if part.starts_with("pipe") => {
                let workers: usize = part[4..]
                    .parse()
                    .map_err(|_| format!("configuration `{key}` has no numeric pipe count"))?;
                if workers == 0 || workers > 64 {
                    return Err(format!(
                        "configuration `{key}` asks for {workers} pipeline workers (bound 1..=64)"
                    ));
                }
                flags.pipeline_workers = workers;
            }
            // ---- R22 sweep dimensions -------------------------------------
            // Chunk-parallel ZSTD threads inside one tensor: `zpar1`..`zpar8`.
            part if part.starts_with("zpar") => {
                let threads: usize = part[4..]
                    .parse()
                    .map_err(|_| format!("configuration `{key}` has no numeric zpar count"))?;
                if threads == 0 || threads > 64 {
                    return Err(format!(
                        "configuration `{key}` asks for {threads} zstd encode threads (bound 1..=64)"
                    ));
                }
                flags.zstd_encode_threads = Some(threads);
            }
            // Ordered-batch byte bound, in KiB: `zcb512`, `zcb32768`.
            part if part.starts_with("zcb") => {
                let kib: u64 = part[3..]
                    .parse()
                    .map_err(|_| format!("configuration `{key}` has no numeric zcb size"))?;
                if kib == 0 {
                    return Err(format!("configuration `{key}` asks for a zero batch bound"));
                }
                flags.zstd_encode_batch_bytes = Some(kib.saturating_mul(1024));
            }
            // Explicit codec frame chunk size in bytes: `zchunk16384`.
            "znochunk" => {
                flags.zstd_frame_chunk = Some(weftfs::tensor_store::ZstdFrameChunk::Unchunked)
            }
            part if part.starts_with("zchunk") => {
                let bytes: u64 = part[6..]
                    .parse()
                    .map_err(|_| format!("configuration `{key}` has no numeric zchunk size"))?;
                if bytes == 0 {
                    return Err(format!("configuration `{key}` asks for a zero chunk size"));
                }
                flags.zstd_frame_chunk = Some(weftfs::tensor_store::ZstdFrameChunk::Bytes(bytes));
            }
            // Level-19 gate margin: `gate<N>` requires a predicted win of N
            // MiB, `leeway<N>` permits a predicted loss of N MiB.
            part if part.starts_with("gate") => {
                let mib: i64 = part[4..]
                    .parse()
                    .map_err(|_| format!("configuration `{key}` has no numeric gate margin"))?;
                flags.zstd19_gate_margin_bytes = Some(mib.saturating_mul(1024 * 1024));
            }
            part if part.starts_with("leeway") => {
                let mib: i64 = part[6..]
                    .parse()
                    .map_err(|_| format!("configuration `{key}` has no numeric leeway"))?;
                flags.zstd19_gate_margin_bytes = Some(-mib.saturating_mul(1024 * 1024));
            }
            // Two-stage candidate ablations: `t19off` / `tm9off`.
            "t19off" if matches!(base, "C" | "E") => flags.two_stage_off_zstd19 = true,
            "tm9off" if matches!(base, "C" | "E") => flags.two_stage_off_mode9 = true,
            part => {
                return Err(format!(
                    "configuration `{key}` has unknown ablation suffix `{part}`"
                ))
            }
        }
    }
    Ok(flags)
}

/// How a WeftFS row publishes its state ops and its blobs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Publication {
    /// One commit and one durability barrier per tensor: the R17/R19 form.
    PerCommit,
    /// One group commit; every blob keeps its own eager `fsync` pair.
    Group,
    /// One group commit plus one deferred durability barrier for its blobs.
    Atomic,
    /// [`Publication::Atomic`] with the blob plane packed at this member
    /// threshold.
    Packed(u64),
}

impl Publication {
    fn group_commit(self) -> bool {
        !matches!(self, Publication::PerCommit)
    }

    fn defer_blob_sync(self) -> bool {
        !matches!(self, Publication::PerCommit | Publication::Group)
    }

    fn pack_threshold(self) -> Option<u64> {
        match self {
            Publication::Packed(threshold) => Some(threshold),
            _ => None,
        }
    }

    fn label(self) -> String {
        match self {
            Publication::PerCommit => "per-commit".to_owned(),
            Publication::Group => "one-group-commit".to_owned(),
            Publication::Atomic => "one-atomic-publication".to_owned(),
            Publication::Packed(threshold) => {
                format!("one-atomic-publication+pack({threshold})")
            }
        }
    }
}

/// The shipped default a contract row runs: one atomic publication.
fn publication_default() -> Publication {
    Publication::Atomic
}

/// Parse `64k`/`1m`/`8m` after the `pack` prefix into a member threshold.
fn pack_threshold(suffix: &str) -> Result<u64, String> {
    let digits = suffix.trim_start_matches("pack");
    let (value, multiplier) = match digits.strip_suffix('k') {
        Some(value) => (value, 1024u64),
        None => match digits.strip_suffix('m') {
            Some(value) => (value, 1024 * 1024),
            None => return Err(format!("pack suffix `{suffix}` needs k or m, e.g. pack64k")),
        },
    };
    let value: u64 = value
        .parse()
        .map_err(|_| format!("pack suffix `{suffix}` has no numeric threshold"))?;
    if value == 0 {
        return Err(format!("pack suffix `{suffix}` must be positive"));
    }
    Ok(value * multiplier)
}

/// The parsed publication and planner flags of one configuration key.
#[derive(Clone, Copy, Debug)]
struct ConfigFlags {
    ablate_zstd19: bool,
    ablate_float: bool,
    publication: Publication,
    planner: PlannerMode,
    /// R21: encode threads for the pipelined group writer; `0` is the
    /// sequential (pre-R21) writer.
    pipeline_workers: usize,
    /// R21: `Some(0)` keeps one deferred durability batch for the whole group
    /// (the R20 behaviour) instead of an early barrier every
    /// `publish_sync_bytes`.
    pipeline_sync_bytes: Option<u64>,
    /// R21: CPU tokens (busy encoder threads) for the pipeline.
    pipeline_cpu_tokens_flag: Option<usize>,
    /// R22: threads inside one chunked ZSTD frame (`zpar<N>`).
    zstd_encode_threads: Option<usize>,
    /// R22: ordered-batch byte bound for chunk-parallel ZSTD (`zcb<KiB>`).
    zstd_encode_batch_bytes: Option<u64>,
    /// R22: ZSTD frame chunking (`zchunk<bytes>`, `znochunk`).
    zstd_frame_chunk: Option<weftfs::tensor_store::ZstdFrameChunk>,
    /// R22: level-19 gate margin in bytes (`gate<MiB>` / `leeway<MiB>`).
    zstd19_gate_margin_bytes: Option<i64>,
    /// R22: disable a candidate under the **two-stage** planner (the R17
    /// `-nom9` / `-noz19` controls pin the exhaustive planner instead, so they
    /// change two things at once).
    two_stage_off_zstd19: bool,
    two_stage_off_mode9: bool,
}

fn uses_protocol(config: &str) -> bool {
    matches!(config_base(config), "D" | "E")
}

/// Accumulate one per-tensor phase delta into a named `(count, total_ns)` map.
fn add_phase_totals(acc: &mut BTreeMap<&'static str, (u64, u128)>, delta: &PhaseSnapshot) {
    for (name, totals) in &delta.phases {
        let entry = acc.entry(*name).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(totals.count);
        entry.1 = entry.1.saturating_add(totals.total_ns);
    }
}

fn phase_totals_json(acc: &BTreeMap<&'static str, (u64, u128)>) -> Json {
    let mut out = serde_json::Map::new();
    for (name, (count, total_ns)) in acc {
        if *count == 0 {
            continue;
        }
        out.insert(
            (*name).to_owned(),
            json!({ "count": count, "total_ms": *total_ns as f64 / 1_000_000.0 }),
        );
    }
    Json::Object(out)
}

fn phase_ms(delta: &PhaseSnapshot, phase: Phase) -> f64 {
    delta
        .phases
        .get(phase.as_str())
        .map(|totals| totals.total_ns as f64 / 1_000_000.0)
        .unwrap_or(0.0)
}

fn phase_count(delta: &PhaseSnapshot, phase: Phase) -> u64 {
    delta
        .phases
        .get(phase.as_str())
        .map(|totals| totals.count)
        .unwrap_or(0)
}

/// One planner decision as a per-tensor receipt row.
fn plan_json(plan: &weftfs::tensor_store::TensorPlan) -> Json {
    let chosen = plan.chosen;
    let candidates: Vec<Json> = plan
        .candidates
        .iter()
        .map(|candidate| {
            let selected = candidate.applicable
                && candidate.strategy == chosen
                && candidate.physical_bytes == Some(plan.chosen_physical_bytes)
                && candidate.cost == Some(plan.chosen_cost);
            let zstd_level = if candidate.strategy == TensorStrategy::Zstd {
                if candidate.detail.contains("level-19") || candidate.detail.contains("zstd-19") {
                    Some(19)
                } else if candidate.detail.contains("level-1 ") {
                    Some(1)
                } else {
                    Some(3)
                }
            } else {
                None
            };
            json!({
                "strategy": candidate.strategy.as_str(),
                "zstd_level": zstd_level,
                "applicable": candidate.applicable,
                "selected": selected,
                "physical_bytes": candidate.physical_bytes,
                "estimated_decode_ns": candidate.estimated_decode_ns,
                "estimated_encode_ns": candidate.estimated_encode_ns,
                "detail": candidate.detail,
            })
        })
        .collect();
    json!({
        "chosen": plan.chosen.as_str(),
        "chosen_physical_bytes": plan.chosen_physical_bytes,
        "runner_up": plan.runner_up.map(|strategy| strategy.as_str()),
        "runner_up_physical_bytes": plan.runner_up_physical_bytes,
        "candidates": candidates,
        "planner": planner_json(plan),
    })
}

fn estimate_json(estimate: &weftfs::two_stage::SampleEstimate) -> Json {
    json!({
        "method": estimate.method,
        "level": estimate.level,
        "predicted": estimate.predicted,
        "lower": estimate.lower,
        "upper": estimate.upper,
        "sampled_chunks": estimate.sampled_chunks,
        "sampled_bytes": estimate.sampled_bytes,
        "total_chunks": estimate.total_chunks,
    })
}

/// The R18 decision stream for one tensor, in machine-readable form.
fn planner_json(plan: &weftfs::tensor_store::TensorPlan) -> Json {
    let Some(trace) = &plan.planner else {
        return Json::Null;
    };
    let analysis = &trace.analysis;
    json!({
        "mode": trace.mode.as_str(),
        "analysis": {
            "logical_bytes": analysis.logical_bytes,
            "raw_bytes": analysis.raw_bytes,
            "chunk_bytes": analysis.chunk_bytes,
            "zstd3": analysis.zstd3.as_ref().map(estimate_json),
            "zstd19": analysis.zstd19.as_ref().map(estimate_json),
            "mode9_exact": analysis.mode9.as_ref().map(|p| p.exact_bytes),
            "mode9_exponent_symbols": analysis.mode9.as_ref().map(|p| p.exponent_symbols),
            "sampled_chunks": analysis.samples.len(),
        },
        "decision": {
            "encode_zstd3": trace.decision.need_zstd3,
            "mode9_gate": match trace.decision.mode9 {
                weftfs::two_stage::Mode9Gate::Pass => "pass",
                weftfs::two_stage::Mode9Gate::Fail => "fail",
                weftfs::two_stage::Mode9Gate::Unresolved => "unresolved",
            },
            "zstd3_skip_bound": trace.decision.zstd3_skip_bound,
        },
        "level19": trace.level19.as_ref().map(|decision| json!({
            "encode": decision.encode,
            "skip_bound": decision.skip_bound,
            "estimate": decision.estimate.as_ref().map(estimate_json),
        })),
        "actual": {
            "zstd3": trace.actual_zstd3,
            "zstd19": trace.actual_zstd19,
            "mode9": trace.actual_mode9,
        },
        "prediction_violations": trace.prediction_violations,
        "fallback_exhaustive": trace.fallback_exhaustive,
    })
}

/// The R18 decision-stream counters for one write, accumulated from the
/// per-tensor planner traces. One implementation for the sequential and the
/// pipelined writer, so the two cannot drift.
#[derive(Clone, Copy, Debug, Default)]
struct PlannerTally {
    tensors: u64,
    skipped_zstd3: u64,
    skipped_zstd19: u64,
    encoded_zstd19: u64,
    mode9_pass: u64,
    mode9_fail: u64,
    mode9_unresolved: u64,
    violation_tensors: u64,
    fallback_tensors: u64,
}

impl PlannerTally {
    fn add(&mut self, plan: &weftfs::tensor_store::TensorPlan) {
        let Some(trace) = plan.planner.as_ref() else {
            return;
        };
        self.tensors += 1;
        if !trace.decision.need_zstd3 {
            self.skipped_zstd3 += 1;
        }
        if trace.level19.as_ref().map(|decision| decision.encode) == Some(true) {
            self.encoded_zstd19 += 1;
        } else {
            self.skipped_zstd19 += 1;
        }
        match trace.decision.mode9 {
            weftfs::two_stage::Mode9Gate::Pass => self.mode9_pass += 1,
            weftfs::two_stage::Mode9Gate::Fail => self.mode9_fail += 1,
            weftfs::two_stage::Mode9Gate::Unresolved => self.mode9_unresolved += 1,
        }
        if !trace.prediction_violations.is_empty() {
            self.violation_tensors += 1;
        }
        if trace.fallback_exhaustive {
            self.fallback_tensors += 1;
        }
    }
}

/// The measured pipeline behaviour, in the receipt's field names.
fn pipeline_stats_json(stats: &weftfs::tensor_pipeline::TensorPipelineStats) -> Json {
    json!({
        "tensors": stats.tensors,
        "encode_threads": stats.encode_threads,
        "cpu_tokens": stats.cpu_tokens,
        "budgets": {
            "staging_bytes": stats.staging_bytes,
            "encode_bytes": stats.encode_bytes,
            "publish_bytes": stats.publish_bytes,
            "worker_bytes": stats.worker_bytes,
            "publish_sync_bytes": stats.publish_sync_bytes,
            "candidate_parallel_min_bytes": stats.candidate_parallel_min_bytes,
            "reservation_bound_bytes": stats.reservation_bound_bytes,
        },
        "observed": {
            "max_staging_bytes": stats.observed_max_staging_bytes,
            "max_encode_bytes": stats.observed_max_encode_bytes,
            "max_publish_bytes": stats.observed_max_publish_bytes,
            "max_worker_bytes": stats.observed_max_worker_bytes,
            "max_resident_tensors": stats.observed_max_resident_tensors,
            "oversize_staging": stats.oversize_staging,
            "oversize_publish": stats.oversize_publish,
            "candidate_parallel_tensors": stats.candidate_parallel_tensors,
        },
        "stages_ms": {
            "reader": stats.reader_ms,
            "encode_busy": stats.encode_busy_ms,
            "publish_busy": stats.publish_busy_ms,
            "encode_wall": stats.encode_wall_ms,
            "publish_wall": stats.publish_wall_ms,
            "pipeline_wall": stats.pipeline_wall_ms,
        },
        "overlap": {
            "encode_overlapped_ms": stats.encode_overlapped_ms,
            "encode_overlap_fraction": stats.encode_overlap_fraction(),
            "publish_overlapped_ms": stats.publish_overlapped_ms,
            "publish_hidden_fraction": stats.publish_hidden_fraction(),
        },
        "waits_ms": {
            "staging": stats.staging_wait_ms,
            "worker_bytes": stats.worker_bytes_wait_ms,
            "encode_queue": stats.encode_queue_wait_ms,
            "publish_idle": stats.publish_idle_ms,
            "cpu_tokens": stats.cpu_token_wait_ms,
        },
        "early_barrier": {
            "passes": stats.writeback_passes,
            "files": stats.writeback_files,
            "busy_ms": stats.writeback_ms,
            "note": "file fsyncs performed on the writeback thread while encoding continued; the commit's own barrier still closes the batch and fsyncs every remaining file and directory before the commit record",
        },
        "source": "Store::prepare_tensor_group_pipelined (R21); every field is measured, never estimated",
    })
}

fn write_weftfs(
    args: &Args,
    config: &str,
    subset: &Subset,
    file: &SafetensorsFile,
) -> Result<Json, String> {
    let flags = ablation_flags(config)?;
    let ConfigFlags {
        ablate_zstd19,
        ablate_float,
        publication,
        planner,
        pipeline_workers,
        pipeline_sync_bytes,
        pipeline_cpu_tokens_flag,
        ..
    } = flags;
    let group_commit = publication.group_commit();
    let work = args.work.clone().unwrap();
    let store_path = config_store_path(&work, config);
    if store_path.exists() {
        return Err(format!(
            "{} already exists (refusing to reuse state)",
            store_path.display()
        ));
    }
    let mut kind = backend_kind_publication(args, publication)?;
    let profiling = args.phase_profile;
    if profiling {
        phase_profile::begin();
    }
    let init_started = Instant::now();
    let mut store_options = options(args.chunk_size);
    store_options.defer_group_blob_sync = publication.defer_blob_sync();
    let mut store = Store::init_with_backend(&store_path, store_options, kind)
        .map_err(|err| format!("init store: {err}"))?;
    let init_ms = init_started.elapsed().as_secs_f64() * 1000.0;
    let root = store.resolve_path("/").map_err(|err| err.to_string())?;
    let tensors_dir = store
        .mkdir(root, b"tensors", 0o755, 0, 0)
        .map_err(|err| format!("mkdir /tensors: {err}"))?;

    let seed_base = base_seed(&args.seed);
    let layout = TensorLayoutSpec::row_major();
    let mut output_contents: BTreeSet<Hash32> = BTreeSet::new();
    let mut ids: Vec<Hash32> = Vec::with_capacity(subset.tensors.len());
    let mut representations: BTreeMap<String, u64> = BTreeMap::new();
    let mut source_read_ms = 0f64;
    let mut prepare_ms = 0f64;
    let mut commit_ms = 0f64;
    let mut samples: Vec<Duration> = Vec::with_capacity(subset.tensors.len());
    let mut id_by_name: BTreeMap<String, String> = BTreeMap::new();
    let mut content_by_name: BTreeMap<String, String> = BTreeMap::new();
    let mut prepare_phases: BTreeMap<&'static str, (u64, u128)> = BTreeMap::new();
    let mut commit_phases: BTreeMap<&'static str, (u64, u128)> = BTreeMap::new();
    let mut details: Vec<Json> = Vec::new();
    // R18 decision-stream counters for this write.
    let mut tally = PlannerTally::default();
    // Group mode stages every prepared object and publishes one commit. The
    // group is opened *before* preparation so a deferred durability barrier
    // covers the blobs those preparations publish.
    let mut staged: Vec<(String, weftfs::tensor_store::PreparedTensorObject, usize)> = Vec::new();
    let mut pipeline_stats = Json::Null;
    if group_commit {
        store
            .begin_group()
            .map_err(|err| format!("begin group: {err}"))?;
    }

    if pipeline_workers > 0 {
        // R21: bounded parallel encode-and-publish pipeline. Byte-identical to
        // the sequential loop (same planner, same seeds, same blobs), with the
        // encoding overlapped by publication.
        let mut popts = weftfs::tensor_pipeline::TensorPipelineOptions::default();
        popts.encode_threads = pipeline_workers;
        popts.cpu_tokens = args
            .pipeline_cpu_tokens
            .or(pipeline_cpu_tokens_flag)
            .unwrap_or(pipeline_workers);
        if let Some(value) = args.pipeline_staging_bytes {
            popts.staging_bytes = value;
        }
        if let Some(value) = args.pipeline_encode_bytes {
            popts.encode_bytes = value;
        }
        if let Some(value) = args.pipeline_publish_bytes {
            popts.publish_bytes = value;
        }
        if let Some(value) = args.pipeline_worker_bytes {
            popts.worker_bytes = value;
        }
        if let Some(value) = args.pipeline_sync_bytes.or(pipeline_sync_bytes) {
            popts.publish_sync_bytes = value;
        }
        if let Some(value) = args.pipeline_candidate_min_bytes {
            popts.candidate_parallel_min_bytes = value;
        }
        let spec = representation_spec(config, &flags);
        let pipeline_tensors: Vec<weftfs::tensor_pipeline::TensorPipelineTensor<'_>> = subset
            .tensors
            .iter()
            .map(|sel| weftfs::tensor_pipeline::TensorPipelineTensor {
                name: &sel.name,
                dtype: &sel.dtype,
                shape: &sel.shape,
                seed: object_seed(&seed_base, config, &sel.name),
                byte_len: sel.bytes,
            })
            .collect();
        let pipeline_snap_before = profiling.then(phase_profile::snapshot);
        let read = |index: usize, buf: &mut [u8]| -> weftfs::Result<()> {
            let sel = &subset.tensors[index];
            let tensor = file
                .tensors
                .iter()
                .find(|t| t.name == sel.name)
                .ok_or_else(|| {
                    weftfs::Error::not_found(format!("tensor `{}` is missing", sel.name))
                })?;
            file.read_tensor_range(tensor, 0, buf)
        };
        let outcome = store
            .prepare_tensor_group_pipelined(&pipeline_tensors, &read, &spec, &layout, &popts)
            .map_err(|err| format!("pipeline: {err}"))?;
        source_read_ms = outcome.stats.reader_ms;
        prepare_ms = outcome.stats.pipeline_wall_ms;
        // The phase profile is process-wide and thread-summed, so the
        // pipeline's whole-stage delta is the same accounting the sequential
        // loop accumulates per tensor.
        if profiling {
            if let Some(before) = &pipeline_snap_before {
                add_phase_totals(
                    &mut prepare_phases,
                    &phase_profile::snapshot().since(before),
                );
            }
        }
        pipeline_stats = pipeline_stats_json(&outcome.stats);
        for (index, result) in outcome.results.into_iter().enumerate() {
            let sel = &subset.tensors[index];
            tally.add(&result.plan);
            let mut prepared = result.prepared;
            if uses_protocol(config) {
                prepared.attrs.insert(
                    PROJECT_ATTR.to_owned(),
                    WcbValue::Str(PROJECT_VALUE.to_owned()),
                );
            }
            let detail = json!({
                "name": sel.name,
                "dtype": sel.dtype,
                "shape": sel.shape,
                "logical_bytes": sel.bytes,
                "prepare_ms": result.encode_ms + result.publish_ms,
                "pipeline": {
                    "encode_ms": result.encode_ms,
                    "publish_ms": result.publish_ms,
                },
                "plan": plan_json(&result.plan),
            });
            let detail_index = details.len();
            details.push(detail);
            staged.push((sel.name.clone(), prepared, detail_index));
        }
    } else {
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

            let stats_before = store.backend_stats();
            let snap_before = profiling.then(phase_profile::snapshot);
            let started = Instant::now();
            let spec = representation_spec(config, &flags);
            let seed = object_seed(&seed_base, config, &sel.name);
            let (mut prepared, plan) = store
                .prepare_tensor_object_with_spec_seeded_planned(
                    &sel.dtype, &sel.shape, &bytes, &layout, &spec, &seed,
                )
                .map_err(|err| format!("prepare `{}`: {err}", sel.name))?;
            let prepare_elapsed = started.elapsed();
            prepare_ms += prepare_elapsed.as_secs_f64() * 1000.0;
            tally.add(&plan);
            let snap_after_prepare = profiling.then(phase_profile::snapshot);
            let prepare_delta = match (&snap_before, &snap_after_prepare) {
                (Some(before), Some(after)) => {
                    let delta = after.since(before);
                    add_phase_totals(&mut prepare_phases, &delta);
                    Some(delta)
                }
                _ => None,
            };
            if uses_protocol(config) {
                prepared.attrs.insert(
                    PROJECT_ATTR.to_owned(),
                    WcbValue::Str(PROJECT_VALUE.to_owned()),
                );
            }
            let detail_index = details.len();
            let mut detail = json!({
                "name": sel.name,
                "dtype": sel.dtype,
                "shape": sel.shape,
                "logical_bytes": sel.bytes,
                "prepare_ms": prepare_elapsed.as_secs_f64() * 1000.0,
                "plan": plan_json(&plan),
            });
            if let Some(delta) = &prepare_delta {
                let encode_total = phase_ms(delta, Phase::Encode);
                let zstd = phase_ms(delta, Phase::EncodeZstd);
                let zstd19 = phase_ms(delta, Phase::EncodeZstd19);
                let float = phase_ms(delta, Phase::EncodeFloatTensor);
                detail["prepare_phase_ms"] = json!({
                    "encode": encode_total,
                    "encode_zstd_level3": zstd,
                    "encode_zstd_level19": zstd19,
                    "encode_float_tensor": float,
                    "encode_other": (encode_total - zstd - zstd19 - float).max(0.0),
                    "digest": phase_ms(delta, Phase::Digest),
                    "blob_publish": phase_ms(delta, Phase::BlobPublish),
                });
                detail["prepare_phase_counts"] = json!({
                    "encode_zstd_level3": phase_count(delta, Phase::EncodeZstd),
                    "encode_zstd_level19": phase_count(delta, Phase::EncodeZstd19),
                    "encode_float_tensor": phase_count(delta, Phase::EncodeFloatTensor),
                });
                detail["prepare_phase_ns"] = delta.to_json();
            }

            if group_commit {
                staged.push((sel.name.clone(), prepared, detail_index));
                details.push(detail);
            } else {
                let stats_before_commit = store.backend_stats();
                let snap_before_commit = profiling.then(phase_profile::snapshot);
                let commit_started = Instant::now();
                let id = store
                    .commit_prepared_tensor_object(
                        tensors_dir,
                        sel.name.as_bytes(),
                        &prepared,
                        0o644,
                    )
                    .map_err(|err| format!("commit `{}`: {err}", sel.name))?;
                let commit_elapsed = commit_started.elapsed();
                commit_ms += commit_elapsed.as_secs_f64() * 1000.0;
                let stats_after = store.backend_stats();
                if profiling {
                    if let Some(before) = &snap_before_commit {
                        let delta = phase_profile::snapshot().since(before);
                        add_phase_totals(&mut commit_phases, &delta);
                        detail["commit_phase_ms"] = delta.to_json();
                    }
                }
                detail["commit_ms"] = json!(commit_elapsed.as_secs_f64() * 1000.0);
                detail["publication_writes"] = json!(stats_after
                    .writes
                    .saturating_sub(stats_before_commit.writes));
                detail["publication_bytes"] = json!(stats_after
                    .bytes_written
                    .saturating_sub(stats_before_commit.bytes_written));
                detail["durable_operations_total"] = json!(stats_after.writes);
                samples.push(prepare_elapsed + commit_elapsed);
                details.push(detail);
                ids.push(id);
                id_by_name.insert(sel.name.clone(), id.to_hex());
                output_contents.insert(prepared.manifest_id);
                *representations
                    .entry(prepared.strategy.clone())
                    .or_insert(0) += 1;
                content_by_name.insert(sel.name.clone(), prepared.manifest_id.to_hex());
                if std::env::var("WEFT_AUG_TRACE").is_ok() {
                    eprintln!(
                    "trace {config} {} logical={} strategy={} written_delta={} written_total={}",
                    sel.name,
                    bytes.len(),
                    prepared.strategy,
                    stats_after
                        .bytes_written
                        .saturating_sub(stats_before.bytes_written),
                    stats_after.bytes_written
                );
                }
            }
        }
    }

    if group_commit {
        let group_started = Instant::now();
        let mut group_ids: Vec<Hash32> = Vec::with_capacity(staged.len());
        let mut per_commit: Vec<(Duration, u64, u64)> = Vec::with_capacity(staged.len());
        for (name, prepared, _) in &staged {
            let before = store.backend_stats();
            let started = Instant::now();
            let id = store
                .commit_prepared_tensor_object(tensors_dir, name.as_bytes(), prepared, 0o644)
                .map_err(|err| format!("commit `{name}`: {err}"))?;
            let elapsed = started.elapsed();
            let after = store.backend_stats();
            group_ids.push(id);
            per_commit.push((
                elapsed,
                after.writes.saturating_sub(before.writes),
                after.bytes_written.saturating_sub(before.bytes_written),
            ));
        }
        // The publication itself (commit record, head, index segment and the
        // deferred durability barrier) is attributed here, once, so the ledger
        // keeps a per-category breakdown for a grouped row.
        let snap_before_publish = profiling.then(phase_profile::snapshot);
        store
            .commit_group()
            .map_err(|err| format!("group commit: {err}"))?;
        if profiling {
            if let Some(before) = &snap_before_publish {
                add_phase_totals(&mut commit_phases, &phase_profile::snapshot().since(before));
            }
        }
        commit_ms = group_started.elapsed().as_secs_f64() * 1000.0;
        ids = group_ids;
        for (index, ((name, prepared, detail_index), id)) in
            staged.iter().zip(ids.iter()).enumerate()
        {
            let (elapsed, writes, bytes) = per_commit[index];
            details[*detail_index]["commit_ms"] = json!(elapsed.as_secs_f64() * 1000.0);
            details[*detail_index]["publication_writes"] = json!(writes);
            details[*detail_index]["publication_bytes"] = json!(bytes);
            samples.push(Duration::from_secs_f64(
                details[*detail_index]["prepare_ms"].as_f64().unwrap_or(0.0) / 1000.0
                    + elapsed.as_secs_f64(),
            ));
            id_by_name.insert(name.clone(), id.to_hex());
            output_contents.insert(prepared.manifest_id);
            *representations
                .entry(prepared.strategy.clone())
                .or_insert(0) += 1;
            content_by_name.insert(name.clone(), prepared.manifest_id.to_hex());
        }
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

    let footprint_started = Instant::now();
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
    let footprint_ms = footprint_started.elapsed().as_secs_f64() * 1000.0;

    let ledger = store.publication_ledger();
    let pack_stats = store.pack_stats();

    let whole_phases = if profiling {
        phase_profile::finish().to_json()
    } else {
        Json::Null
    };

    Ok(json!({
        "schema": "weft.augmentation.phase.v1",
        "phase": "write",
        "config": config,
        "ablation": {
            "zstd19_disabled": ablate_zstd19,
            "float_tensor_disabled": ablate_float,
            "group_commit": group_commit,
            "publication": publication.label(),
            "deferred_blob_sync": publication.defer_blob_sync(),
            "pack_member_threshold_bytes": publication.pack_threshold(),
        },
        "publication_ledger": publication_ledger_json(&ledger, &stats_after_write, pack_stats),
        "backend": args.backend,
        "logical_bytes": subset.logical_bytes,
        "tensor_count": subset.tensors.len(),
        "init_ms": init_ms,
        "source_read_ms": source_read_ms,
        "prepare_ms": prepare_ms,
        "commit_ms": commit_ms,
        "write_ms": prepare_ms + commit_ms,
        "protocol_ms": protocol_ms,
        "footprint_ms": footprint_ms,
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
        "prepare_phases": phase_totals_json(&prepare_phases),
        "commit_phases": phase_totals_json(&commit_phases),
        "planner": json!({
            "mode": planner.as_str(),
            "tensors_with_trace": tally.tensors,
            "skipped_zstd3": tally.skipped_zstd3,
            "skipped_zstd19": tally.skipped_zstd19,
            "encoded_zstd19": tally.encoded_zstd19,
            "mode9_gate_pass": tally.mode9_pass,
            "mode9_gate_fail": tally.mode9_fail,
            "mode9_gate_unresolved": tally.mode9_unresolved,
            "prediction_violation_tensors": tally.violation_tensors,
            "exhaustive_fallback_tensors": tally.fallback_tensors,
            "note": "the trace is per tensor in `per_tensor[].plan.planner`; a skipped candidate carries the bound that made the skip safe",
        }),
        "pipeline": pipeline_stats,
        "phases": whole_phases,
        "per_tensor": if args.per_tensor_detail { Json::Array(details) } else { Json::Null },
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

    let kind = backend_kind_publication(args, ablation_flags(config)?.publication)?;
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
        "fsyncs": stats.fsyncs,
    })
}

/// The durability ledger of one WeftFS write: where every durable operation and
/// byte went, by category.
fn publication_ledger_json(
    ledger: &weftfs::PublicationLedger,
    stats: &BackendStats,
    pack: Option<weftfs::packs::PackStats>,
) -> Json {
    let mut categories = serde_json::Map::new();
    categories.insert(
        "object_blobs".to_owned(),
        json!({ "count": ledger.object_blobs, "bytes": ledger.object_blob_bytes }),
    );
    categories.insert(
        "manifest_blobs".to_owned(),
        json!({ "count": ledger.manifest_blobs, "bytes": ledger.manifest_blob_bytes }),
    );
    categories.insert(
        "commit_records".to_owned(),
        json!({ "count": ledger.commit_records, "bytes": ledger.commit_record_bytes }),
    );
    categories.insert(
        "superblock".to_owned(),
        json!({ "count": ledger.superblock_writes }),
    );
    categories.insert(
        "index_segments".to_owned(),
        json!({ "count": ledger.index_segments, "bytes": ledger.index_bytes }),
    );
    let accounted_bytes = ledger.object_blob_bytes
        + ledger.manifest_blob_bytes
        + ledger.commit_record_bytes
        + ledger.index_bytes;
    json!({
        "categories": Json::Object(categories),
        "duplicate_blobs": ledger.duplicate_blobs,
        "backend_writes": stats.writes,
        "backend_bytes_written": stats.bytes_written,
        "fsyncs": {
            "total": stats.fsyncs,
            "file": ledger.file_barriers,
            "dir": ledger.dir_barriers,
            "source": "total from BackendStats, split from StorageBackend::durability_barriers",
        },
        "accounted_blob_and_control_bytes": accounted_bytes,
        "unaccounted_bytes": stats.bytes_written.saturating_sub(accounted_bytes),
        "pack": pack.map(|pack| json!({
            "sealed_packs": pack.sealed_packs,
            "packed_members": pack.packed_members,
            "packed_bytes": pack.packed_bytes,
            "flat_members": pack.flat_members,
            "flat_bytes": pack.flat_bytes,
            "catalog_entries": pack.catalog_entries,
            "catalog_segment_publishes": pack.catalog_segment_publishes,
            "catalog_root_publishes": pack.catalog_root_publishes,
            "open_pack_bytes": pack.open_pack_bytes,
            "open_pack_members": pack.open_pack_members,
            "packing_disabled": pack.packing_disabled,
        })),
        "note": "counted at the point each artifact is handed to the backend; `unaccounted_bytes` is FORMAT/LOCK/probe/control records",
    })
}

// ---------------------------------------------------------------------------
// Parent: one command, fresh child per phase
// ---------------------------------------------------------------------------

fn append_log(log: &mut File, text: &str) {
    let _ = writeln!(log, "{text}");
    let _ = log.flush();
}

fn run_child(
    args: &Args,
    work: &Path,
    phase: &str,
    config: &str,
    extra: &[String],
    envs: &[(&str, &str)],
    log: &mut File,
) -> Result<Json, String> {
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
    let phase_dir = work.join("phases");
    fs::create_dir_all(&phase_dir).map_err(|err| format!("{}: {err}", phase_dir.display()))?;
    let phase_out = phase_dir.join(format!("{config}-{phase}.json"));
    let child_log = phase_dir.join(format!("{config}-{phase}.log"));
    let subset = work.join("subset.json");

    let mut command = Command::new(exe);
    command
        .arg(phase)
        .arg("--work")
        .arg(work)
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
    for value in extra {
        command.arg(value);
    }
    for (name, value) in envs {
        command.env(name, value);
    }
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

/** The R17/R18 configuration matrix. */
const DEFAULT_ABLATION_CONFIGS: &str =
    "A,B,C,C-percommit,C-legacy,D,E,N,B-group,C-noz19,C-nom9,C-both";

/// Pinned C row from the R3 receipt (`augmentation-local-20261109T134132Z`).
/// Strategy counts must stay identical across revisions; the physical footprint
/// drifted by ~1.86 MB between R3 and this revision (index/commit evolution),
/// so it is recorded as a delta rather than used as a pass/fail gate.
const R3_C_PHYSICAL_FOOTPRINT: u64 = 284_139_539;
const R3_C_STRATEGIES: [(&str, u64); 3] = [("float-tensor", 35), ("zstd", 50), ("raw", 16)];

fn stat_block(values: &[f64]) -> Json {
    if values.is_empty() {
        return Json::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = |q: f64| -> f64 {
        let index = ((sorted.len() as f64 - 1.0) * q).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    };
    json!({
        "median": rank(0.5),
        "p95": rank(0.95),
        "min": sorted[0],
        "max": sorted[sorted.len() - 1],
        "samples": values,
    })
}

fn extra_args(phase_profile: bool, detail: bool) -> Vec<String> {
    let mut extra: Vec<String> = Vec::new();
    if phase_profile {
        extra.push("--phase-profile".to_owned());
    }
    if detail {
        extra.push("--per-tensor-detail".to_owned());
    }
    extra
}

/// One crash-proxy write. Returns the child status; a successful crash write
/// is a harness failure, because the store was supposed to abort.
fn crash_write(
    args: &Args,
    work: &Path,
    config: &str,
    phase: &str,
    log: &mut File,
) -> Result<i32, String> {
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
    let child_log = work
        .join("phases")
        .join(format!("{config}-crash-{phase}.log"));
    fs::create_dir_all(child_log.parent().unwrap()).map_err(|err| err.to_string())?;
    let out = File::create(&child_log).map_err(|err| err.to_string())?;
    let err_out = out.try_clone().map_err(|err| err.to_string())?;
    let status = Command::new(exe)
        .arg("crash-write")
        .arg("--work")
        .arg(work)
        .arg("--config")
        .arg(config)
        .arg("--backend")
        .arg(&args.backend)
        .env("WEFT_CRASH_PHASE", phase)
        // A group row crashes inside a group commit, so the crash points there
        // are the ones exercised. A per-commit row crashes on the single-
        // mutation path. Both must leave old-or-new-whole.
        .env(
            "WEFT_CRASH_GROUP",
            if ablation_flags(config)
                .map(|flags| flags.publication.group_commit())
                .unwrap_or(false)
            {
                "1"
            } else {
                "0"
            },
        )
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err_out))
        .status()
        .map_err(|err| format!("spawn crash-write {phase}: {err}"))?;
    let code = status.code().unwrap_or(-1);
    append_log(log, &format!("crash-write {config} {phase}: exit {code}"));
    Ok(code)
}

/// Three process-fault crash phases on one written store. Every phase must
/// leave either the whole old generation or the whole new one.
fn crash_proxy(
    args: &Args,
    work: &Path,
    config: &str,
    _baseline_generation: u64,
    log: &mut File,
) -> Result<Json, String> {
    if args.backend != "local" {
        return Ok(json!({
            "status": "skipped",
            "reason": "the crash proxy is local-only",
        }));
    }
    let phases = [
        "after_objects",
        "after_commit_file",
        "after_superblock_write",
    ];
    // Read the visible head from the store itself: the write phase's recorded
    // generation predates the protocol commits (D/E add snapshot, context and
    // provenance commits after the tensors), so it is not the crash baseline.
    let initial = run_child(args, work, "crash-check", config, &[], &[], log)?;
    let baseline_generation = initial["generation"].as_u64().unwrap_or(0);
    let mut rows: Vec<Json> = Vec::new();
    let mut old = 0u64;
    let mut new = 0u64;
    let mut partial = 0u64;
    let mut visible_before = baseline_generation;
    for phase in phases {
        let code = crash_write(args, work, config, phase, log)?;
        if code == 0 {
            return Err(format!(
                "crash-write {config} {phase} exited 0; the store was expected to abort"
            ));
        }
        let check = run_child(args, work, "crash-check", config, &[], &[], log)?;
        let generation = check["generation"].as_u64().unwrap_or(u64::MAX);
        let expected_new = phase == "after_superblock_write";
        let state = if generation == visible_before {
            "old"
        } else if generation > visible_before {
            "new"
        } else {
            "partial"
        };
        match state {
            "old" => old += 1,
            "new" => new += 1,
            _ => partial += 1,
        }
        // An abort after the commit record but before the superblock leaves an
        // unreferenced commit tail. `verify` reports exactly that as a problem
        // by design; it is still the whole old state, and recovery ignores the
        // tail. Every other problem is a failure.
        let problems: Vec<String> = check["problems"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let orphan_tail_only = !problems.is_empty()
            && problems.iter().all(|problem| {
                problem.starts_with("commit file for generation ")
                    && problem.contains("exists above the highest published superblock")
            });
        let verify_consistent = problems.is_empty() || (state == "old" && orphan_tail_only);
        let exact = check["tensor_mismatches"].as_u64() == Some(0)
            && check["tensor_missing"].as_u64() == Some(0);
        let expected_ok = if expected_new {
            state == "new"
        } else {
            state == "old"
        };
        rows.push(json!({
            "phase": phase,
            "exit_code": code,
            "generation_before": visible_before,
            "generation_after": generation,
            "state": state,
            "expected": if expected_new { "new" } else { "old" },
            "expected_ok": expected_ok,
            "verify_ok": check["verify_ok"],
            "verify_problems": check["verify_problems"],
            "problems": problems,
            "verify_consistent": verify_consistent,
            "tensor_mismatches": check["tensor_mismatches"],
            "tensor_missing": check["tensor_missing"],
            "exact": exact,
        }));
        visible_before = generation;
    }
    Ok(json!({
        "status": "measured",
        "baseline_generation": baseline_generation,
        "old": old,
        "new": new,
        "partial": partial,
        "ok": partial == 0
            && rows.iter().all(|row| {
                row["expected_ok"].as_bool() == Some(true)
                    && row["verify_consistent"].as_bool() == Some(true)
                    && row["exact"].as_bool() == Some(true)
            }),
        "phases": rows,
    }))
}

/// Account for the entire measured write wall of one profiled run.
fn build_ledger(write: &Json) -> Json {
    let number = |value: &Json| value.as_f64().unwrap_or(0.0);
    let wall = number(&write["wall_ms"]);
    let init = number(&write["init_ms"]);
    let source = number(&write["source_read_ms"]);
    let prepare = number(&write["prepare_ms"]);
    let commit = number(&write["commit_ms"]);
    let protocol = number(&write["protocol_ms"]);
    let footprint = number(&write["footprint_ms"]);
    let prepare_phase = |name: &str| number(&write["prepare_phases"][name]["total_ms"]);
    let commit_phase = |name: &str| number(&write["commit_phases"][name]["total_ms"]);
    let encode = prepare_phase("encode");
    let zstd3 = prepare_phase("encode_zstd");
    let zstd19 = prepare_phase("encode_zstd19");
    let mode9 = prepare_phase("encode_float_tensor");
    let analyze = prepare_phase("analyze");
    let encode_other = (encode - zstd3 - zstd19 - mode9 - analyze).max(0.0);
    let prepare_digest = prepare_phase("digest");
    let prepare_publish = prepare_phase("blob_publish");
    let prepare_residual = (prepare - encode - prepare_digest - prepare_publish).max(0.0);
    let index = commit_phase("index_segment");
    let manifest_commit = commit_phase("manifest_commit");
    // The deferred durability barrier runs *inside* the commit publication and
    // before the commit record, so it is nested in `manifest_commit` and is
    // reported separately rather than subtracted again.
    let backend_sync = commit_phase("backend_sync");
    let commit_digest = commit_phase("digest");
    let commit_publish = commit_phase("blob_publish");
    let metadata = commit_phase("metadata_commit");
    let commit_residual =
        (commit - index - manifest_commit - commit_digest - commit_publish - metadata).max(0.0);
    let accounted = init + source + prepare + commit + protocol + footprint;
    json!({
        "wall_ms": wall,
        "init_ms": init,
        "source_read_ms": source,
        "prepare_ms": prepare,
        "prepare_breakdown": {
            "encode_total_ms": encode,
            "encode_zstd_level3_ms": zstd3,
            "encode_zstd_level19_ms": zstd19,
            "encode_mode9_float_tensor_ms": mode9,
            "encode_analyze_ms": analyze,
            "encode_other_planning_and_raw_ms": encode_other,
            "digest_ms": prepare_digest,
            "blob_publish_ms": prepare_publish,
            "prepare_unattributed_ms": prepare_residual,
        },
        "commit_ms": commit,
        "commit_breakdown": {
            "index_segment_ms": index,
            "manifest_commit_ms": manifest_commit,
            "durability_barrier_nested_in_manifest_commit_ms": backend_sync,
            "digest_ms": commit_digest,
            "blob_publish_ms": commit_publish,
            "metadata_commit_ms": metadata,
            "commit_unattributed_ms": commit_residual,
        },
        "protocol_ms": protocol,
        "footprint_walk_ms": footprint,
        "accounted_ms": accounted,
        "unattributed_residual_ms": (wall - accounted).max(0.0),
        "unattributed_residual_share_of_wall": if wall > 0.0 {
            (wall - accounted).max(0.0) / wall
        } else {
            0.0
        },
        "encode_counts": {
            "zstd_level3": write["prepare_phases"]["encode_zstd"]["count"],
            "zstd_level19": write["prepare_phases"]["encode_zstd19"]["count"],
            "mode9_float_tensor": write["prepare_phases"]["encode_float_tensor"]["count"],
        },
        "planner": write["planner"],
        "publication": write["ablation"],
        "publication_ledger": write["publication_ledger"],
        "next_instrumentation": "fsync is inside BlobPublish/ManifestCommit on the local backend; the publication ledger now splits those into blob kinds and file/dir barriers, so a change can be judged on measured counts rather than a residual.",
    })
}

/// Per-tensor decision diff between two planned-write configurations.
///
/// `before` is normally the exhaustive planner (`C-legacy`) and `after` the
/// shipped two-stage planner (`C`). The diff answers, tensor by tensor: did the
/// representation change, did its published size change, which candidates were
/// skipped and with what bound, and did any prediction interval get violated.
fn decision_diff(
    before_key: &str,
    after_key: &str,
    before: &Json,
    after: &Json,
) -> Result<Json, String> {
    let rows = |write: &Json| -> Result<BTreeMap<String, Json>, String> {
        let per_tensor = write["per_tensor"].as_array().ok_or_else(|| {
            "per-tensor detail missing (--per-tensor-detail was not set)".to_owned()
        })?;
        let mut out = BTreeMap::new();
        for row in per_tensor {
            let name = row["name"].as_str().unwrap_or_default().to_owned();
            out.insert(name, row.clone());
        }
        Ok(out)
    };
    let before_rows = rows(before)?;
    let after_rows = rows(after)?;
    let mut changed: Vec<Json> = Vec::new();
    let mut compared = 0u64;
    let mut mode9_checked = 0u64;
    let mut mode9_mismatches = 0u64;
    let mut violation_tensors = 0u64;
    let mut skipped_zstd3_with_bound = 0u64;
    let mut skipped_zstd19_with_bound = 0u64;
    for (name, before_row) in &before_rows {
        let Some(after_row) = after_rows.get(name) else {
            return Err(format!("tensor `{name}` missing from the {after_key} row"));
        };
        compared += 1;
        let before_strategy = before_row["plan"]["chosen"].as_str().unwrap_or_default();
        let after_strategy = after_row["plan"]["chosen"].as_str().unwrap_or_default();
        let before_bytes = before_row["plan"]["chosen_physical_bytes"].as_u64();
        let after_bytes = after_row["plan"]["chosen_physical_bytes"].as_u64();
        if before_strategy != after_strategy || before_bytes != after_bytes {
            changed.push(json!({
                "name": name,
                "before_strategy": before_strategy,
                "after_strategy": after_strategy,
                "before_bytes": before_bytes,
                "after_bytes": after_bytes,
            }));
        }
        // The exact mode-9 prediction: wherever the encode ran, the measured
        // frame must equal it; wherever it did not, the prediction is still
        // recorded and the skip carries a bound.
        let planner = &after_row["plan"]["planner"];
        if let (Some(predicted), Some(actual)) = (
            planner["analysis"]["mode9_exact"].as_u64(),
            planner["actual"]["mode9"].as_u64(),
        ) {
            mode9_checked += 1;
            if predicted != actual {
                mode9_mismatches += 1;
            }
        }
        if planner["prediction_violations"]
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false)
        {
            violation_tensors += 1;
        }
        if planner["decision"]["zstd3_skip_bound"].is_string() {
            skipped_zstd3_with_bound += 1;
        }
        if planner["level19"]["skip_bound"].is_string() {
            skipped_zstd19_with_bound += 1;
        }
    }
    let sum = |write: &Json| -> u64 {
        write["per_tensor"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["plan"]["chosen_physical_bytes"].as_u64())
                    .sum()
            })
            .unwrap_or(0)
    };
    let before_bytes_total = sum(before);
    let after_bytes_total = sum(after);
    let planner_counts = |write: &Json| write["planner"].clone();
    Ok(json!({
        "before": before_key,
        "after": after_key,
        "tensors_compared": compared,
        "strategy_or_size_changes": changed.len(),
        "changed": changed,
        "decisions_match": changed.is_empty(),
        "chosen_bytes_before": before_bytes_total,
        "chosen_bytes_after": after_bytes_total,
        "chosen_bytes_delta": after_bytes_total as i64 - before_bytes_total as i64,
        "planner_before": planner_counts(before),
        "planner_after": planner_counts(after),
        "mode9_predictions_checked_against_actual": mode9_checked,
        "mode9_prediction_mismatches": mode9_mismatches,
        "interval_violation_tensors": violation_tensors,
        "skipped_zstd3_with_bound": skipped_zstd3_with_bound,
        "skipped_zstd19_with_bound": skipped_zstd19_with_bound,
    }))
}

fn ablate(args: &Args) -> Result<(), String> {
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

    let file =
        SafetensorsFile::parse(&fixture).map_err(|err| format!("{}: {err}", fixture.display()))?;
    let actual = hex::encode(file.sha256);
    let expected_sha256 = args
        .fixture_sha256
        .clone()
        .unwrap_or_else(|| FIXTURE_SHA256.to_owned());
    if actual != expected_sha256 {
        return Err(format!(
            "fixture SHA-256 mismatch: expected {expected_sha256}, got {actual}"
        ));
    }
    let subset = select_subset(&file, args.include_embedding, args.max_others)?;
    let keys: Vec<String> = args
        .configs
        .as_deref()
        .unwrap_or(DEFAULT_ABLATION_CONFIGS)
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .collect();
    if keys.is_empty() {
        return Err("--configs produced an empty list".to_owned());
    }
    for key in &keys {
        if key != NATIVE_APP && !is_weftfs_config(key) {
            return Err(format!("unknown configuration `{key}`"));
        }
        if key != NATIVE_APP {
            ablation_flags(key)?;
        }
    }
    let reps = args.reps.max(1);
    append_log(
        &mut log,
        &format!(
            "ablate: fixture {} bytes={} sha256={} subset={} tensors/{} bytes digest={}",
            fixture.display(),
            file.file_bytes,
            actual,
            subset.tensors.len(),
            subset.logical_bytes,
            subset.subset_digest
        ),
    );
    append_log(
        &mut log,
        &format!(
            "ablate: configs={keys:?} reps={reps} backend={}",
            args.backend
        ),
    );

    let mut rows: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    let mut write_samples: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    let mut orders: Vec<Json> = Vec::new();
    let mut detail_rep: BTreeMap<String, Json> = BTreeMap::new();
    for rep in 1..=reps {
        let rep_dir = work.join(format!("rep-{rep:02}"));
        fs::create_dir_all(&rep_dir).map_err(|err| format!("{}: {err}", rep_dir.display()))?;
        write_json_atomic(
            &rep_dir.join("subset.json"),
            &serde_json::to_value(&subset).map_err(|err| err.to_string())?,
        )?;
        let mut order = keys.clone();
        let rotation = (rep - 1) % order.len();
        order.rotate_left(rotation);
        orders.push(json!({ "rep": rep, "order": order }));
        append_log(
            &mut log,
            &format!("ablate: === rep {rep}/{reps} order={order:?} ==="),
        );
        for key in &order {
            let is_weft = is_weft_store_config(key);
            append_log(&mut log, &format!("ablate: rep {rep} config {key} write"));
            let extra = if is_weft {
                extra_args(true, rep == 1)
            } else {
                Vec::new()
            };
            let write = run_child(args, &rep_dir, "write", key, &extra, &[], &mut log)?;
            append_log(&mut log, &format!("ablate: rep {rep} config {key} restore"));
            let restore = run_child(args, &rep_dir, "restore", key, &[], &[], &mut log)?;
            let crash = if rep == 1 && is_weft {
                Some(crash_proxy(
                    args,
                    &rep_dir,
                    key,
                    write["generation"].as_u64().unwrap_or(0),
                    &mut log,
                )?)
            } else if rep == 1 {
                Some(json!({
                    "status": "skipped",
                    "reason": "the native baseline has no WeftFS store to crash",
                }))
            } else {
                None
            };
            if rep == 1 && is_weft {
                detail_rep.insert(
                    key.clone(),
                    json!({
                        "write": write.clone(),
                        "restore": restore.clone(),
                        "crash": crash.clone(),
                    }),
                );
            }
            let exact = restore["exactness"]["mismatches"].as_u64() == Some(0)
                && restore["exactness"]["match"].as_bool() == Some(true);
            let row = json!({
                "rep": rep,
                "wall_ms": write["wall_ms"],
                "cpu_ms": write["cpu_user_sys_ms"],
                "peak_rss_kib": write["peak_rss_kib"],
                "init_ms": write["init_ms"],
                "source_read_ms": write["source_read_ms"],
                "prepare_ms": write["prepare_ms"],
                "commit_ms": write["commit_ms"],
                "protocol_ms": write["protocol_ms"],
                "physical_footprint_bytes": write["storage"]["physical_footprint_bytes"]
                    .as_u64()
                    .or_else(|| write["physical_footprint_bytes"].as_u64()),
                "physical_footprint_objects": write["storage"]["physical_footprint_objects"]
                    .as_u64()
                    .or_else(|| write["physical_footprint_objects"].as_u64()),
                "physical_bytes_written": write["physical_bytes_written"],
                "write_requests": write["backend_stats"]["writes"],
                "representations": write["representations"],
                "store_head": write["store_head"],
                "generation": write["generation"],
                "ablation": write["ablation"],
                "pipeline": write["pipeline"],
                "encode_counts": if is_weft {
                    build_ledger(&write)["encode_counts"].clone()
                } else {
                    Json::Null
                },
                "restore": {
                    "wall_ms": restore["cold"]["wall_ms"],
                    "bytes_read": restore["cold"]["backend_bytes_read"],
                    "requests": restore["cold"]["backend_read_requests"],
                    "cpu_ms": restore["cold"]["cpu_user_sys_ms"],
                    "peak_rss_kib": restore["cold"]["peak_rss_kib"],
                    "tensors_verified": restore["exactness"]["tensors_verified"],
                    "mismatches": restore["exactness"]["mismatches"],
                    "exact": exact,
                },
                "crash": crash,
            });
            rows.entry(key.clone()).or_default().push(row);
            write_samples
                .entry(key.clone())
                .or_default()
                .push(write.clone());
            // Reclaim the ~0.3-0.4 GB store before the next configuration.
            let store_dir = config_store_path(&rep_dir, key);
            if let Err(err) = fs::remove_dir_all(&store_dir) {
                append_log(&mut log, &format!("ablate: cleanup {key}: {err}"));
            }
        }
        if let Err(err) = fs::remove_dir_all(&rep_dir) {
            append_log(&mut log, &format!("ablate: cleanup {rep_dir:?}: {err}"));
        }
    }

    let mut configs_receipt: BTreeMap<String, Json> = BTreeMap::new();
    for (key, samples) in &rows {
        let walls: Vec<f64> = samples
            .iter()
            .filter_map(|row| row["wall_ms"].as_f64())
            .collect();
        let cpus: Vec<f64> = samples
            .iter()
            .filter_map(|row| row["cpu_ms"].as_f64())
            .collect();
        let rss: Vec<f64> = samples
            .iter()
            .filter_map(|row| row["peak_rss_kib"].as_f64())
            .collect();
        let footprints: Vec<f64> = samples
            .iter()
            .filter_map(|row| row["physical_footprint_bytes"].as_f64())
            .collect();
        let restores: Vec<f64> = samples
            .iter()
            .filter_map(|row| row["restore"]["wall_ms"].as_f64())
            .collect();
        let strategies: Vec<Json> = samples
            .iter()
            .map(|row| row["representations"].clone())
            .collect();
        let stable = strategies.windows(2).all(|pair| pair[0] == pair[1]);
        let all_exact = samples
            .iter()
            .all(|row| row["restore"]["exact"].as_bool() == Some(true));
        let mut receipt = json!({
            "base": config_base(key),
            "suffix": config_suffix(key),
            "reps": samples.len(),
            "wall_ms": stat_block(&walls),
            "cpu_ms": stat_block(&cpus),
            "peak_rss_kib": stat_block(&rss),
            "physical_footprint_bytes": stat_block(&footprints),
            "cold_restore_wall_ms": stat_block(&restores),
            "representations": strategies.first().cloned().unwrap_or(Json::Null),
            "strategies_stable_across_reps": stable,
            "all_restores_exact": all_exact,
            "samples": samples,
        });
        if is_weft_store_config(key) {
            let writes = write_samples.get(key).cloned().unwrap_or_default();
            // Use the repetition whose wall is the median, so the ledger is
            // representative of a timed repetition rather than of rep 1 under
            // arbitrary host load.
            let mut order: Vec<usize> = (0..writes.len()).collect();
            order.sort_by(|a, b| {
                writes[*a]["wall_ms"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .partial_cmp(&writes[*b]["wall_ms"].as_f64().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let median_rep = order.get(order.len() / 2).copied().unwrap_or(0);
            // The full distribution can be bimodal when the host has an
            // external I/O-heavy process. The ledger describes the
            // least-contended repetition (the minimum wall), which is the
            // closest observation of this configuration's own cost; the
            // median and p95 are published unchanged in `wall_ms`.
            let least_contended = order.first().copied().unwrap_or(0);
            let write = writes.get(least_contended).cloned().unwrap_or(Json::Null);
            let detail = detail_rep.get(key);
            receipt["attribution_rep"] = json!({
                "rep": least_contended + 1,
                "least_contended_rep": true,
                "median_wall_rep": median_rep + 1,
                "median_wall_ms": writes.get(median_rep).and_then(|write| write["wall_ms"].as_f64()),
                "least_contended_wall_ms": writes.get(least_contended).and_then(|write| write["wall_ms"].as_f64()),
                "phases": write["phases"],
                "pipeline": write["pipeline"],
                "prepare_phases": write["prepare_phases"],
                "commit_phases": write["commit_phases"],
                "ledger": build_ledger(&write),
                "crash": detail.map(|detail| detail["crash"].clone()).unwrap_or(Json::Null),
                "per_tensor": detail.map(|detail| detail["write"]["per_tensor"].clone()).unwrap_or(Json::Null),
            });
        }
        configs_receipt.insert(key.clone(), receipt);
    }

    let c_row = configs_receipt.get("C").cloned().unwrap_or(Json::Null);
    let c_strategies_ok = R3_C_STRATEGIES
        .iter()
        .all(|(name, expected)| c_row["representations"][name].as_u64() == Some(*expected));
    let c_footprint_delta = c_row["physical_footprint_bytes"]["median"]
        .as_f64()
        .map(|value| value as i64 - R3_C_PHYSICAL_FOOTPRINT as i64);
    // Same-revision identity: the official contract court's C write and the
    // ablation runner's C write are separate processes with the same fixed
    // seed and timestamp, so strategies, head, footprint and written bytes
    // must be identical. This is the proof that the default path did not move.
    let official = match &args.official_receipt {
        Some(path) => {
            let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
            Some((
                path.clone(),
                serde_json::from_slice::<Json>(&bytes).map_err(|err| err.to_string())?,
            ))
        }
        None => None,
    };
    let official_c = official.as_ref().and_then(|(path, receipt)| {
        receipt["configurations"]["C"]["write"]
            .as_object()
            .map(|_| {
                (
                    path.clone(),
                    receipt["configurations"]["C"]["write"].clone(),
                )
            })
    });
    let official_court_comparison = match (
        &official_c,
        write_samples.get("C").and_then(|writes| writes.first()),
    ) {
        (Some((path, official)), Some(sample)) => {
            let strategies_match = official["representations"] == sample["representations"];
            let head_match = official["store_head"] == sample["store_head"];
            let footprint_match = official["storage"]["physical_footprint_bytes"]
                == sample["storage"]["physical_footprint_bytes"];
            let bytes_match =
                official["physical_bytes_written"] == sample["physical_bytes_written"];
            // `/tensors` is created with a random ObjectId seed by the harness
            // (only tensor objects are seeded), so the store head is not
            // reproducible across processes even though every tensor manifest
            // and every physical byte is. The identity proof is therefore the
            // per-tensor content set, not the head.
            let outputs_match = official["output_contents"] == sample["output_contents"];
            json!({
                "official_receipt": path.display().to_string(),
                "official_store_head": official["store_head"],
                "ablate_store_head": sample["store_head"],
                "official_footprint_bytes": official["storage"]["physical_footprint_bytes"],
                "ablate_footprint_bytes": sample["storage"]["physical_footprint_bytes"],
                "strategies_match": strategies_match,
                "head_match": head_match,
                "footprint_match": footprint_match,
                "bytes_match": bytes_match,
                "output_contents_match": outputs_match,
                "match": strategies_match && footprint_match && bytes_match && outputs_match,
                "head_note": "the store head is not reproducible across processes because /tensors is created with a random ObjectId seed; tensor manifests are reproducible",
            })
        }
        _ => json!({ "match": false, "reason": "no --official-receipt or no C sample" }),
    };
    let default_identity_ok = official_court_comparison["match"].as_bool() == Some(true);
    let zstd19_eligible = subset.tensors.iter().any(|sel| sel.bytes >= 1 << 20);
    let tensors = subset.tensors.len() as u64;
    // R18 decision diff: the exhaustive planner (`-legacy`) against the shipped
    // two-stage planner. Both rows must be present for the diff to exist.
    let diff_pair = if detail_rep.contains_key("C") && detail_rep.contains_key("C-legacy") {
        Some(("C-legacy", "C"))
    } else if detail_rep.contains_key("E") && detail_rep.contains_key("E-legacy") {
        Some(("E-legacy", "E"))
    } else {
        None
    };
    let decision_diff_json = match diff_pair {
        Some((before_key, after_key)) => Some(decision_diff(
            before_key,
            after_key,
            &detail_rep[before_key]["write"],
            &detail_rep[after_key]["write"],
        )?),
        None => None,
    };
    let decision_diff_ok = match (&decision_diff_json, diff_pair) {
        (Some(diff), Some(_)) => {
            diff["decisions_match"].as_bool() == Some(true)
                && diff["mode9_prediction_mismatches"].as_u64() == Some(0)
                && diff["interval_violation_tensors"].as_u64() == Some(0)
        }
        _ => false,
    };
    let storage_unchanged_ok = match diff_pair {
        Some((before_key, after_key)) => {
            let before = configs_receipt[before_key]["physical_footprint_bytes"]["median"].as_f64();
            let after = configs_receipt[after_key]["physical_footprint_bytes"]["median"].as_f64();
            before.is_some() && before == after
        }
        None => false,
    };
    // Encode counts: the exhaustive rows keep their R17 meaning (the ablation
    // suffix removes exactly one candidate); a two-stage row must instead agree
    // with its own decision stream.
    let ablation_counts_ok = configs_receipt.keys().all(|key| {
        if !is_weft_store_config(key) {
            return true;
        }
        let counts = &configs_receipt[key]["attribution_rep"]["ledger"]["encode_counts"];
        let planner = &configs_receipt[key]["attribution_rep"]["ledger"]["planner"];
        let zstd3 = counts["zstd_level3"].as_u64().unwrap_or(0);
        let zstd19 = counts["zstd_level19"].as_u64().unwrap_or(0);
        let mode9 = counts["mode9_float_tensor"].as_u64().unwrap_or(0);
        match config_base(key) {
            // The Fast pass-through rows run no planned encoder at all.
            "B" | "D" => zstd3 == 0 && zstd19 == 0 && mode9 == 0,
            _ => {
                let flags = match ablation_flags(key) {
                    Ok(flags) => flags,
                    Err(_) => return false,
                };
                match flags.planner {
                    PlannerMode::TwoStage => {
                        // Two-stage: encode counts follow the recorded decisions,
                        // independently of how the row publishes.
                        let pass = planner["mode9_gate_pass"].as_u64().unwrap_or(0);
                        let unresolved = planner["mode9_gate_unresolved"].as_u64().unwrap_or(0);
                        let encoded19 = planner["encoded_zstd19"].as_u64().unwrap_or(0);
                        zstd3 <= tensors && zstd19 == encoded19 && mode9 == pass + unresolved
                    }
                    PlannerMode::Exhaustive => {
                        // Exhaustive planner: the R17 ablation contract.
                        let zstd19_ok = if flags.ablate_zstd19 {
                            zstd19 == 0
                        } else {
                            !zstd19_eligible || zstd19 > 0
                        };
                        let mode9_ok = if flags.ablate_float {
                            mode9 == 0
                        } else {
                            mode9 == tensors
                        };
                        zstd19_ok && mode9_ok
                    }
                }
            }
        }
    });
    let crash_ok = configs_receipt.iter().all(|(_, entry)| {
        let crash = &entry["attribution_rep"]["crash"];
        if crash.is_null() || crash["status"].as_str() == Some("skipped") {
            true
        } else {
            crash["ok"].as_bool() == Some(true)
        }
    });
    let all_exact = configs_receipt
        .iter()
        .all(|(_, entry)| entry["all_restores_exact"].as_bool() == Some(true));
    let reps_ok = configs_receipt
        .iter()
        .all(|(_, entry)| entry["reps"].as_u64().unwrap_or(0) >= 7);
    let mut checks: serde_json::Map<String, Json> = serde_json::Map::new();
    if !args.diff_only {
        checks.insert(
            "all_configs_present".to_owned(),
            json!(configs_receipt.len() == rows.len()),
        );
        checks.insert("at_least_seven_reps".to_owned(), json!(reps_ok));
        checks.insert(
            "default_c_matches_official_court".to_owned(),
            json!(default_identity_ok),
        );
        checks.insert("crash_proxy_old_or_new".to_owned(), json!(crash_ok));
    }
    checks.insert("all_restores_exact".to_owned(), json!(all_exact));
    checks.insert(
        "strategies_stable_across_reps".to_owned(),
        json!(configs_receipt
            .iter()
            .all(|(_, entry)| entry["strategies_stable_across_reps"].as_bool() == Some(true))),
    );
    if !args.diff_only {
        // The R3 strategy counts are the frozen Qwen fixture's. A cross-family
        // decision diff runs on a different container, so the check is omitted
        // there rather than asserted or faked.
        checks.insert(
            "default_c_row_matches_r3_strategies".to_owned(),
            json!(c_strategies_ok),
        );
    }
    checks.insert(
        "ablation_encode_counts_exact".to_owned(),
        json!(ablation_counts_ok),
    );
    checks.insert(
        "two_stage_decision_diff_present".to_owned(),
        json!(decision_diff_json.is_some()),
    );
    checks.insert(
        "two_stage_matches_exhaustive".to_owned(),
        json!(decision_diff_ok),
    );
    checks.insert(
        "two_stage_storage_unchanged".to_owned(),
        json!(storage_unchanged_ok),
    );
    let checks = Json::Object(checks);
    let result = if checks
        .as_object()
        .map(|map| map.values().all(|value| value.as_bool() == Some(true)))
        .unwrap_or(false)
    {
        "pass"
    } else {
        "fail"
    };
    let receipt = json!({
        "schema": "weft.write_cost_ablation.v1",
        "generated_utc": utc_stamp(),
        "result": result,
        "mode": if args.diff_only { "decision-diff" } else { "matrix" },
        "contract": "bench/CONTRACTS-augmentation.md",
        "methodology": {
            "reps": reps,
            "interleaving": "the configuration order is rotated by one position per repetition so no configuration always occupies the same slot",
            "repetition_profiles": "every timed repetition enables the shipped phase collector; the attribution rep is repetition 1 and additionally emits per-tensor rows",
            "wall_source": "write child VmHWM/wall around the whole write phase, excluding the whole-fixture SHA-256 gate",
            "baseline_note": "the comparator is the same backend without the WeftFS layer (A); the reference FUSE filesystem is not a comparator",
            "host_contention": "every repetition is published raw; if the host is shared with an external CPU/IO-heavy process, the median can include that process. The ledger uses the least-contended repetition and records both walls so the two are never conflated.",
        },
        "environment": limit_label(),
        "fixture": {
            "path": fixture.display().to_string(),
            "bytes": file.file_bytes,
            "sha256": actual,
        },
        "subset": {
            "tensor_count": subset.tensors.len(),
            "logical_bytes": subset.logical_bytes,
            "subset_digest": subset.subset_digest,
            "include_embedding": subset.include_embedding,
            "other_tensors": subset.other_tensors,
            "selection": subset.selection,
        },
        "orders": orders,
        "configs": configs_receipt,
        "decision_diff": decision_diff_json,
        "default_identity": {
            "c_footprint_delta_vs_r3_bytes": c_footprint_delta,
            "r3_footprint_bytes": R3_C_PHYSICAL_FOOTPRINT,
            "r3_note": "R3 predates the advisory index and commit-record evolution; strategies are compared, the footprint is reported as a delta, and the same-revision identity proof is default_c_matches_official_court",
            "official_court_comparison": official_court_comparison,
        },
        "checks": checks,
        "not_measured": not_measured(),
    });
    write_json_atomic(&out, &receipt)?;
    append_log(
        &mut log,
        &format!("ablate: result={result} receipt={}", out.display()),
    );
    if result != "pass" {
        return Err(format!(
            "ablation checks failed; receipt at {}",
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
    if actual
        != args
            .fixture_sha256
            .clone()
            .unwrap_or_else(|| FIXTURE_SHA256.to_owned())
    {
        return Err(format!(
            "fixture SHA-256 mismatch: expected {}, got {actual}",
            args.fixture_sha256
                .clone()
                .unwrap_or_else(|| FIXTURE_SHA256.to_owned())
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
        let write_result = run_child(&args, &work, "write", config, &[], &[], &mut log);
        let write = match write_result {
            Ok(value) => value,
            Err(err) => {
                failure = Some(err);
                break;
            }
        };
        let restore_result = run_child(&args, &work, "restore", config, &[], &[], &mut log);
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
            if let Err(err) = run_child(&args, &work, "cleanup", config, &[], &[], &mut log) {
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
            let expected_sha256 = args
                .fixture_sha256
                .clone()
                .unwrap_or_else(|| FIXTURE_SHA256.to_owned());
            if actual != expected_sha256 {
                return Err(format!(
                    "fixture SHA-256 mismatch: expected {expected_sha256}, got {actual}"
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
                other if is_weftfs_config(other) => write_weftfs(&args, other, &subset, &file)?,
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
                other if is_weftfs_config(other) => {
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
        "ablate" => ablate(&args),
        "crash-write" => (|| {
            let config = args
                .config
                .clone()
                .ok_or_else(|| "--config is required".to_owned())?;
            let work = args
                .work
                .clone()
                .ok_or_else(|| "--work is required".to_owned())?;
            let path = config_store_path(&work, &config);
            let kind = backend_kind_publication(&args, ablation_flags(&config)?.publication)?;
            let mut store = Store::open_with_backend(&path, OpenMode::ReadWrite, kind)
                .map_err(|err| format!("open {}: {err}", path.display()))?;
            let root = store.resolve_path("/").map_err(|err| err.to_string())?;
            let phase = std::env::var("WEFT_CRASH_PHASE").unwrap_or_default();
            let group = std::env::var("WEFT_CRASH_GROUP").as_deref() == Ok("1");
            let probe = |store: &mut Store| -> Result<(), String> {
                store
                    .create_file(
                        root,
                        format!("crash-{phase}").as_bytes(),
                        b"crash-probe\n",
                        0o644,
                        0,
                        0,
                        BTreeMap::new(),
                    )
                    .map_err(|err| format!("crash write: {err}"))?;
                Ok(())
            };
            if group {
                // Exercise the group publication path: the crash points inside
                // the commit only fire while this group is being published.
                store
                    .begin_group()
                    .map_err(|err| format!("begin group: {err}"))?;
                probe(&mut store)?;
                store
                    .commit_group()
                    .map_err(|err| format!("group commit: {err}"))?;
            } else {
                probe(&mut store)?;
            }
            // Reaching here means WEFT_CRASH_PHASE did not match a crash point.
            Ok(())
        })(),
        "crash-check" => (|| {
            let config = args
                .config
                .clone()
                .ok_or_else(|| "--config is required".to_owned())?;
            let work = args
                .work
                .clone()
                .ok_or_else(|| "--work is required".to_owned())?;
            let subset = load_subset(&args.subset.clone().ok_or("--subset is required")?)?;
            let path = config_store_path(&work, &config);
            // The reader must use the same blob plane the row wrote with: a
            // packed store opened as a flat local store cannot resolve any
            // packed member, so recovery would reject the newest generation and
            // the crash verdict would be about the harness, not the store.
            let kind = backend_kind_publication(&args, ablation_flags(&config)?.publication)?;
            let store = Store::open_with_backend(&path, OpenMode::ReadOnly, kind)
                .map_err(|err| format!("open {}: {err}", path.display()))?;
            let report = store.verify(true).map_err(|err| err.to_string())?;
            let mut mismatches = 0u64;
            let mut missing = 0u64;
            for sel in &subset.tensors {
                let object_path = format!("/tensors/{}", sel.name);
                match store.resolve_path(&object_path) {
                    Ok(id) => match store.read_object(id) {
                        Ok(bytes) => {
                            if sha256_hex(&bytes) != sel.sha256 {
                                mismatches += 1;
                            }
                        }
                        Err(_) => mismatches += 1,
                    },
                    Err(_) => missing += 1,
                }
            }
            let value = json!({
                "generation": store.generation(),
                "verify_ok": report.ok,
                "verify_problems": report.problems.len(),
                "problems": report
                    .problems
                    .iter()
                    .map(|problem| problem.detail.clone())
                    .collect::<Vec<String>>(),
                "tensor_mismatches": mismatches,
                "tensor_missing": missing,
            });
            write_json_atomic(&args.phase_out.ok_or("--phase-out is required")?, &value)?;
            Ok(())
        })(),
        other => Err(format!(
            "unknown subcommand `{other}` (expected run|prepare|write|restore|cleanup|assemble|ablate|crash-write|crash-check)"
        )),
    };
    if let Err(err) = result {
        eprintln!("augmentation_bench: {err}");
        std::process::exit(1);
    }
}
