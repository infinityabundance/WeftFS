//! Blob/commit plane court harness (subphase R10).
//!
//! Runs one deterministic workload through four physical layouts:
//!
//! | config | blobs | commit / control state |
//! |---|---|---|
//! | `local-local` | local | local (one backend, the historical default) |
//! | `s3-s3` | S3 | S3 (one backend) |
//! | `s3-blobs-local-commits` | S3 | local directory (split) |
//! | `local-blobs-s3-commits` | local directory | S3 (split) |
//!
//! Every configuration runs in its own child process so peak RSS is measured
//! per configuration instead of per process-lifetime high-water mark. The
//! parent asserts that all completed children report the *same* content root
//! and the same commit generation, then writes one receipt.
//!
//! `scripts/s3_court.sh --planes` starts MinIO and calls
//! `bench/scripts/planes_court.sh`, which calls this harness. Without an S3
//! endpoint only `local-local` runs and the S3 layouts are recorded as skipped
//! with the exact reason.
//!
//! This is a single-host benchmark with a synthetic fixture. It is not a
//! cloud, fleet, media, power-loss, or production claim.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use weftfs::backend::BackendKind;
use weftfs::s3::S3Config;
use weftfs::store::{OpenMode, Options, Store};

const SCHEMA: &str = "weft-planes-court-v1";
/// 48 files x 128 KiB = 6 MiB of visible content.
const FILES: usize = 48;
const FILE_BYTES: usize = 128 * 1024;
const CHUNK_SIZE: u64 = 64 * 1024;
/// 96 four-KiB range writes after creation.
const RANGE_WRITES: usize = 96;
const RANGE_BYTES: usize = 4096;

fn usage() -> &'static str {
    "usage: planes_bench --out <receipt.json> [--config <name>] [--root <dir>]\n\
     Run without --config to execute every configuration in child processes."
}

fn main() {
    if let Err(err) = run() {
        eprintln!("planes_bench: {err}");
        std::process::exit(1);
    }
}

enum Args {
    Parent {
        out: PathBuf,
        root: PathBuf,
        tests_summary: Option<PathBuf>,
    },
    Child {
        config: String,
        out: PathBuf,
        root: PathBuf,
    },
}

fn parse_args() -> Result<Args, String> {
    let mut out: Option<PathBuf> = None;
    let mut config: Option<String> = None;
    let mut root: Option<PathBuf> = None;
    let mut tests_summary: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a path")?)),
            "--config" => config = Some(args.next().ok_or("--config needs a name")?),
            "--root" => root = Some(PathBuf::from(args.next().ok_or("--root needs a dir")?)),
            "--tests-summary" => {
                tests_summary = Some(PathBuf::from(
                    args.next().ok_or("--tests-summary needs a path")?,
                ))
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    let out = out.ok_or("--out <receipt.json> is required")?;
    let root = root.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("weft-planes-{}", std::process::id()))
    });
    match config {
        Some(config) => Ok(Args::Child { config, out, root }),
        None => Ok(Args::Parent {
            out,
            root,
            tests_summary,
        }),
    }
}

fn run() -> Result<(), String> {
    match parse_args()? {
        Args::Parent {
            out,
            root,
            tests_summary,
        } => run_parent(&out, &root, tests_summary.as_deref()),
        Args::Child { config, out, root } => run_child(&config, &out, &root),
    }
}

// ---------------------------------------------------------------------------
// Parent
// ---------------------------------------------------------------------------

const CONFIGS: [&str; 4] = [
    "local-local",
    "s3-s3",
    "s3-blobs-local-commits",
    "local-blobs-s3-commits",
];

fn run_parent(out: &Path, root: &Path, tests_summary: Option<&Path>) -> Result<(), String> {
    std::fs::create_dir_all(root)
        .map_err(|err| format!("cannot create {}: {err}", root.display()))?;
    let exe = std::env::current_exe().map_err(|err| format!("cannot locate harness: {err}"))?;
    let s3_available = std::env::var("WEFT_S3_TEST_ENDPOINT")
        .map(|value| !value.is_empty())
        .unwrap_or(false);

    let mut configs: Vec<Value> = Vec::new();
    // Each S3 layout gets its own bucket: the control records are not
    // content-addressed, so two layouts sharing a bucket would overwrite each
    // other's head. The parent never talks to S3 itself.
    let bucket_base = std::env::var("WEFT_S3_BUCKET").unwrap_or_else(|_| "weft-planes".to_owned());
    for config in CONFIGS {
        if config != "local-local" && !s3_available {
            configs.push(json!({
                "config": config,
                "status": "skipped",
                "reason": "WEFT_S3_TEST_ENDPOINT is not set; run scripts/s3_court.sh --planes to start MinIO and include every layout",
            }));
            continue;
        }
        let child_out = root.join(format!("{config}.json"));
        let mut command = Command::new(&exe);
        command
            .arg("--config")
            .arg(config)
            .arg("--out")
            .arg(&child_out)
            .arg("--root")
            .arg(root);
        if config != "local-local" {
            command.env("WEFT_S3_BUCKET", format!("{bucket_base}-{config}"));
        }
        let status = command
            .status()
            .map_err(|err| format!("cannot spawn child for {config}: {err}"))?;
        if !status.success() {
            return Err(format!("child configuration {config} failed with {status}"));
        }
        let text = std::fs::read_to_string(&child_out)
            .map_err(|err| format!("cannot read {config} result: {err}"))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|err| format!("cannot parse {config} result: {err}"))?;
        configs.push(value);
    }

    // Exactness: every configuration that ran must agree on the content root,
    // the commit generation, and the verified byte total.
    let completed: Vec<&Value> = configs
        .iter()
        .filter(|value| value.get("status").and_then(Value::as_str) == Some("ok"))
        .collect();
    let first = completed.first().copied();
    let mut exact = json!({ "configs_compared": completed.len() });
    if let Some(first) = first {
        let root_hex = first["snapshot_root"].clone();
        let generation = first["generation"].clone();
        let bytes = first["verified_bytes"].clone();
        let mut roots: Vec<Value> = Vec::new();
        let mut generations: Vec<Value> = Vec::new();
        let mut matches = true;
        for value in &completed {
            matches &= value["snapshot_root"] == root_hex;
            matches &= value["generation"] == generation;
            matches &= value["verified_bytes"] == bytes;
            roots.push(value["snapshot_root"].clone());
            generations.push(value["generation"].clone());
        }
        exact = json!({
            "configs_compared": completed.len(),
            "match": matches,
            "snapshot_root": root_hex,
            "generation": generation,
            "verified_bytes": bytes,
            "snapshot_roots": roots,
            "generations": generations,
        });
    }

    let receipt = json!({
        "schema": SCHEMA,
        "generated_utc": utc_now(),
        "workload": {
            "files": FILES,
            "file_bytes": FILE_BYTES,
            "chunk_size": CHUNK_SIZE,
            "range_writes": RANGE_WRITES,
            "range_write_bytes": RANGE_BYTES,
            "visible_bytes": (FILES * FILE_BYTES) as u64,
            "fixed_timestamp_ns": 1_700_000_000_000_000_000i64,
            "note": "deterministic bytes, seeded ObjectIds, fixed clock; every layout must reproduce the same root",
        },
        "environment": {
            "court_image": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "court_image_id": std::env::var("WEFT_COURT_IMAGE_ID").unwrap_or_else(|_| "unknown".to_owned()),
            "court_kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "unknown".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "unknown".to_owned()),
            "pids": std::env::var("WEFT_COURT_PIDS").unwrap_or_else(|_| "unknown".to_owned()),
            "revision_weftfs": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
            "s3_endpoint": std::env::var("WEFT_S3_ENDPOINT").unwrap_or_else(|_| "unset".to_owned()),
            "s3_server": std::env::var("WEFT_S3_SERVER_LABEL").unwrap_or_else(|_| "unset".to_owned()),
            "s3_server_release": std::env::var("WEFT_S3_SERVER_VERSION").unwrap_or_else(|_| "unset".to_owned()),
            "s3_server_sha256": std::env::var("WEFT_S3_SERVER_SHA256").unwrap_or_else(|_| "unset".to_owned()),
        },
        "configs": configs,
        "exactness": exact,
        "tests": match tests_summary {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
                let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
                json!({ "summary_file": path.display().to_string(), "summary": lines })
            }
            None => json!(null),
        },
        "label": "Single-host benchmark with a synthetic deterministic fixture in the OOM-protected Docker court. Not a fleet, cloud, media, power-loss, or production claim.",
    });

    let text = serde_json::to_string_pretty(&receipt)
        .map_err(|err| format!("cannot encode receipt: {err}"))?;
    std::fs::write(out, text).map_err(|err| format!("cannot write {}: {err}", out.display()))?;
    println!("planes_bench: receipt {}", out.display());
    if first.is_some() {
        let exact = &receipt["exactness"]["match"];
        println!("planes_bench: exactness match={exact}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Child: one configuration
// ---------------------------------------------------------------------------

fn s3_config() -> S3Config {
    S3Config::from_env().unwrap_or_else(|err| {
        eprintln!("planes_bench: S3 configuration is incomplete: {err}");
        std::process::exit(2);
    })
}

fn backend_for(
    config: &str,
    s3: Option<&S3Config>,
    blob_root: &Path,
) -> Result<(BackendKind, String), String> {
    let s3_kind = || -> Result<BackendKind, String> {
        s3.map(|config| BackendKind::S3 {
            config: config.clone(),
        })
        .ok_or_else(|| format!("{config} needs an S3 configuration"))
    };
    match config {
        "local-local" => Ok((
            BackendKind::Local,
            "local/local (single backend)".to_owned(),
        )),
        "s3-s3" => Ok((s3_kind()?, "s3/s3 (single backend)".to_owned())),
        "s3-blobs-local-commits" => Ok((
            BackendKind::split(s3_kind()?, BackendKind::Local).map_err(|err| err.to_string())?,
            "s3 blobs / local commits".to_owned(),
        )),
        "local-blobs-s3-commits" => Ok((
            BackendKind::split_with_blob_root(
                BackendKind::Local,
                s3_kind()?,
                Some(blob_root.to_path_buf()),
            )
            .map_err(|err| err.to_string())?,
            "local blobs / s3 commits".to_owned(),
        )),
        other => Err(format!("unknown --config `{other}`")),
    }
}

fn deterministic_bytes(len: usize, seed: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = u64::from(seed)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(1);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

fn peak_rss_kib() -> u64 {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(value) = rest.split_whitespace().next() {
                return value.parse().unwrap_or(0);
            }
        }
    }
    0
}

fn utc_now() -> String {
    // `date` is not guaranteed in the court; the parent receives the stamp from
    // the shell wrapper when it is available, otherwise this coarse clock.
    match std::process::Command::new("date")
        .args(["-u", "+%Y%m%dT%H%M%SZ"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        _ => format!("epoch-{}", now_secs()),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

fn run_child(config: &str, out: &Path, root: &Path) -> Result<(), String> {
    let child_started = Instant::now();
    let s3 = if config == "local-local" {
        None
    } else {
        Some(s3_config())
    };

    let base = root.join(config);
    let _ = std::fs::remove_dir_all(&base);
    let store_path = base.join("store");
    let blob_root = base.join("blobs");
    let (kind, description) = backend_for(config, s3.as_ref(), &blob_root)?;

    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK_SIZE,
        checkpoint_interval: 8,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    };

    let mut phases: Vec<Value> = Vec::new();
    let mut store = Store::init_with_backend(&store_path, options, kind.clone())
        .map_err(|err| format!("init: {err}"))?;
    let plane_probes = store.plane_probes().cloned();
    let capabilities = store.plane_capabilities();
    let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;

    // Create: every file carries one 128 KiB chunk pair.
    let bodies: Vec<Vec<u8>> = (0..FILES)
        .map(|index| deterministic_bytes(FILE_BYTES, index as u8))
        .collect();
    let mut ids = Vec::with_capacity(FILES);
    let started = Instant::now();
    for (index, body) in bodies.iter().enumerate() {
        let id = store
            .create_file_seeded(
                root_id,
                format!("f{index:03}").as_bytes(),
                body,
                0o644,
                0,
                0,
                Default::default(),
                format!("seed-{index}").as_bytes(),
            )
            .map_err(|err| format!("create: {err}"))?;
        ids.push(id);
    }
    phases.push(json!({ "phase": "create", "ms": elapsed_ms(started) }));

    // Range writes: 96 four-KiB overwrites spread across the file set.
    let patch = deterministic_bytes(RANGE_BYTES, 0xAA);
    let started = Instant::now();
    let mut samples: Vec<f64> = Vec::with_capacity(RANGE_WRITES);
    for step in 0..RANGE_WRITES {
        let file = &mut ids[step % FILES];
        let offset = (step * 4096) % (FILE_BYTES - RANGE_BYTES);
        let started_write = Instant::now();
        store
            .write_file(*file, offset as u64, &patch)
            .map_err(|err| format!("range write: {err}"))?;
        samples.push(started_write.elapsed().as_secs_f64() * 1000.0);
    }
    phases.push(json!({
        "phase": "range_write",
        "ms": elapsed_ms(started),
        "count": RANGE_WRITES,
        "p50_ms": percentile(&mut samples, 50.0),
        "p99_ms": percentile(&mut samples, 99.0),
    }));

    // Read: re-read every file and hash the bytes so the receipt carries a
    // byte-exactness check, not just a success flag.
    let started = Instant::now();
    let mut verified = 0u64;
    let mut digest = blake3::Hasher::new();
    for (index, id) in ids.iter().enumerate() {
        let bytes = store
            .read_object(*id)
            .map_err(|err| format!("read: {err}"))?;
        if bytes.len() != bodies[index].len() {
            return Err(format!("file {index} length changed"));
        }
        verified += bytes.len() as u64;
        digest.update(&bytes);
    }
    phases.push(json!({ "phase": "read", "ms": elapsed_ms(started) }));

    let generation = store.generation();
    let snapshot_root = store.snapshot_root().to_hex();
    let verify_started = Instant::now();
    let report = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    let verify_ms = elapsed_ms(verify_started);
    let problems = report.problems.len();
    let plane_stats = store.plane_stats();
    let backend_stats = store.backend_stats();
    let diagnostics = store.backend_diagnostics();
    let reopened_generation;
    let reopened_root;
    drop(store);

    // Reopen read-only and prove the recovered state is the same whole state.
    let reopen_started = Instant::now();
    let reopened = Store::open_with_backend(&store_path, OpenMode::ReadOnly, kind)
        .map_err(|err| format!("reopen: {err}"))?;
    let reopen_ms = elapsed_ms(reopen_started);
    reopened_generation = reopened.generation();
    reopened_root = reopened.snapshot_root().to_hex();
    let reopen_report = reopened
        .verify(true)
        .map_err(|err| format!("reopen verify: {err}"))?;
    let reopen_problems = reopen_report.problems.len();
    drop(reopened);

    let result = json!({
        "config": config,
        "description": description,
        "status": "ok",
        "store": store_path.display().to_string(),
        "blob_root": blob_root.display().to_string(),
        "s3_bucket": std::env::var("WEFT_S3_BUCKET").ok(),
        "generation": generation,
        "snapshot_root": snapshot_root,
        "verified_bytes": verified,
        "content_digest": hex::encode(digest.finalize().as_bytes()),
        "phases": phases,
        "verify": { "ms": verify_ms, "problems": problems },
        "reopen": {
            "ms": reopen_ms,
            "generation": reopened_generation,
            "snapshot_root": reopened_root,
            "problems": reopen_problems,
            "match": reopened_generation == generation && reopened_root == snapshot_root,
        },
        "planes": {
            "capabilities": capabilities,
            "probes": plane_probes,
            "stats": plane_stats,
            "merged_stats": backend_stats,
        },
        "diagnostics": diagnostics,
        "peak_rss_kib": peak_rss_kib(),
        "wall_ms": elapsed_ms(child_started),
    });

    let text = serde_json::to_string_pretty(&result).map_err(|err| err.to_string())?;
    std::fs::write(out, text).map_err(|err| format!("cannot write {}: {err}", out.display()))?;
    Ok(())
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn percentile(samples: &mut [f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((p / 100.0) * (samples.len() as f64 - 1.0)).round() as usize;
    samples[rank.min(samples.len() - 1)]
}

#[allow(dead_code)]
fn _unused_duration(_: Duration) {}
