//! Packed blob court harness (subphase R11).
//!
//! Runs one deterministic workload through four configurations:
//!
//! | config | blobs |
//! |---|---|
//! | `local-flat` | local backend, one physical object per blob |
//! | `local-packed` | local backend, small blobs grouped into packs |
//! | `s3-flat` | MinIO/S3, one physical object per blob |
//! | `s3-packed` | MinIO/S3, small blobs grouped into packs |
//!
//! Each configuration runs in its own child process so peak RSS is measured per
//! configuration. Phases:
//!
//! 1. Publish 10,000 one-kilobyte files through the store in one group commit
//!    (2 objects + 1 manifest each): request counts, bytes, wall time.
//! 2. Read 1,000 of the chunk blobs sequentially and 1,000 randomly, cold and
//!    warm (a fresh read plane sees a cold catalog), with p50/p99 latency.
//! 3. Publish an isolated 16 MiB pack of 4 KiB members and read one 4 KiB range,
//!    proving with the backend byte counter that only that range is fetched.
//! 4. `verify --full` on the writer and again after a read-only reopen.
//!
//! `scripts/s3_court.sh --packs` starts MinIO and calls
//! `bench/scripts/packs_court.sh`, which calls this harness. Without an S3
//! endpoint only the local configurations run and the S3 ones are recorded as
//! skipped with the exact reason.
//!
//! Every loss is published: the catalog is extra bytes written, and a cold read
//! of a tiny member pays catalog read amplification.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value};

use weftfs::backend::{BackendKind, Key, KeyKind};
use weftfs::identity;
use weftfs::packs::{PackOptions, PackedBlobPlane};
use weftfs::planes::BlobPlane;
use weftfs::s3::S3Config;
use weftfs::store::{OpenMode, Options, Store};

const SCHEMA: &str = "weft-packs-court-v1";
/// 10,000 one-kilobyte files: one 1 KiB chunk plus one manifest each.
const SMALL_FILES: usize = 10_000;
const SMALL_BYTES: usize = 1024;
/// 1,000 members are read per pass.
const READS: usize = 1000;
/// 2,048 x 8 KiB members make exactly one 16 MiB pack.
const RANGE_PACK_MEMBERS: usize = 2048;
const RANGE_PACK_MEMBER_BYTES: usize = 8192;
const RANGE_READ_BYTES: u64 = 4096;

fn usage() -> &'static str {
    "usage: packs_bench --out <receipt.json> [--config <name>] [--root <dir>]\n\
     Run without --config to execute every configuration in child processes."
}

fn main() {
    if let Err(err) = run() {
        eprintln!("packs_bench: {err}");
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
    let root = root
        .unwrap_or_else(|| std::env::temp_dir().join(format!("weft-packs-{}", std::process::id())));
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

const CONFIGS: [&str; 4] = ["local-flat", "local-packed", "s3-flat", "s3-packed"];

fn run_parent(out: &Path, root: &Path, tests_summary: Option<&Path>) -> Result<(), String> {
    std::fs::create_dir_all(root)
        .map_err(|err| format!("cannot create {}: {err}", root.display()))?;
    let exe = std::env::current_exe().map_err(|err| format!("cannot locate harness: {err}"))?;
    let s3_available = std::env::var("WEFT_S3_TEST_ENDPOINT")
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let bucket_base = std::env::var("WEFT_S3_BUCKET").unwrap_or_else(|_| "weft-packs".to_owned());

    let mut configs: Vec<Value> = Vec::new();
    for config in CONFIGS {
        if config.starts_with("s3-") && !s3_available {
            configs.push(json!({
                "config": config,
                "status": "skipped",
                "reason": "WEFT_S3_TEST_ENDPOINT is not set; run scripts/s3_court.sh --packs to start MinIO and include the S3 configurations",
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
        if config.starts_with("s3-") {
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

    // Exactness: every configuration that ran must agree on the snapshot root,
    // the commit generation, the verified byte total, and the digest of the
    // 1,000 read payloads. Packing changes physical layout only.
    let completed: Vec<&Value> = configs
        .iter()
        .filter(|value| value.get("status").and_then(Value::as_str) == Some("ok"))
        .collect();
    let first = completed.first().copied();
    let mut exact = json!({ "configs_compared": completed.len() });
    if let Some(first) = first {
        let root_hex = first["snapshot_root"].clone();
        let generation = first["generation"].clone();
        let verified = first["read_digest"].clone();
        let mut matches = true;
        for value in &completed {
            matches &= value["snapshot_root"] == root_hex;
            matches &= value["generation"] == generation;
            matches &= value["read_digest"] == verified;
            matches &= value["verify"]["problems"] == 0;
        }
        exact = json!({
            "configs_compared": completed.len(),
            "match": matches,
            "snapshot_root": root_hex,
            "generation": generation,
            "read_digest": verified,
            "verify_problems_total": completed.iter().map(|value| value["verify"]["problems"].as_u64().unwrap_or(0)).sum::<u64>(),
        });
    }

    let receipt = json!({
        "schema": SCHEMA,
        "generated_utc": utc_now(),
        "workload": {
            "small_files": SMALL_FILES,
            "small_file_bytes": SMALL_BYTES,
            "chunk_size": SMALL_BYTES,
            "logical_small_bytes": SMALL_FILES * SMALL_BYTES,
            "reads_per_pass": READS,
            "range_pack_members": RANGE_PACK_MEMBERS,
            "range_pack_member_bytes": RANGE_PACK_MEMBER_BYTES,
            "range_pack_bytes": RANGE_PACK_MEMBERS * RANGE_PACK_MEMBER_BYTES,
            "range_read_bytes": RANGE_READ_BYTES,
            "fixed_timestamp_ns": 1_700_000_000_000_000_000i64,
            "note": "deterministic payloads and seeds; pack options are fixed; every configuration must reproduce the same snapshot root and the same read digest",
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
        "label": "Single-host benchmark with a deterministic synthetic fixture in the OOM-protected Docker court. MinIO is a single node with a tmpfs data directory when the S3 configurations run. Not a cloud, fleet, device-cache, media, power-loss, or production claim.",
    });

    let text = serde_json::to_string_pretty(&receipt)
        .map_err(|err| format!("cannot encode receipt: {err}"))?;
    std::fs::write(out, text).map_err(|err| format!("cannot write {}: {err}", out.display()))?;
    println!("packs_bench: receipt {}", out.display());
    println!(
        "packs_bench: exactness match={}",
        receipt["exactness"]["match"]
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Child: one configuration
// ---------------------------------------------------------------------------

fn pack_options() -> PackOptions {
    PackOptions {
        pack_target_bytes: 16 * 1024 * 1024,
        member_threshold_bytes: 64 * 1024,
        max_members_per_pack: 65_536,
        max_catalog_bytes: 8 * 1024 * 1024,
        catalog_segments: 256,
    }
}

fn s3_kind() -> BackendKind {
    let config = S3Config::from_env().unwrap_or_else(|err| {
        eprintln!("packs_bench: S3 configuration is incomplete: {err}");
        std::process::exit(2);
    });
    BackendKind::S3 { config }
}

/// An S3 kind whose bucket is `{configured bucket}-{suffix}`, so an isolated
/// phase never shares a catalog root with the store.
fn s3_kind_in(bucket_suffix: &str) -> BackendKind {
    let mut config = S3Config::from_env().unwrap_or_else(|err| {
        eprintln!("packs_bench: S3 configuration is incomplete: {err}");
        std::process::exit(2);
    });
    config.bucket = format!("{}-{bucket_suffix}", config.bucket);
    BackendKind::S3 { config }
}

/// The single backend under a configuration (packing wraps it).
fn inner_kind(config: &str) -> BackendKind {
    match config {
        "local-flat" | "local-packed" => BackendKind::Local,
        "s3-flat" | "s3-packed" => s3_kind(),
        other => {
            eprintln!("packs_bench: unknown --config `{other}`");
            std::process::exit(2);
        }
    }
}

fn backend_for(config: &str) -> Result<(BackendKind, String), String> {
    match config {
        "local-flat" => Ok((BackendKind::Local, "local, flat blobs".to_owned())),
        "local-packed" => Ok((
            BackendKind::packed(BackendKind::Local, pack_options())
                .map_err(|err| err.to_string())?,
            "local, packed blobs".to_owned(),
        )),
        "s3-flat" => Ok((s3_kind(), "s3, flat blobs".to_owned())),
        "s3-packed" => Ok((
            BackendKind::packed(s3_kind(), pack_options()).map_err(|err| err.to_string())?,
            "s3, packed blobs".to_owned(),
        )),
        other => Err(format!("unknown --config `{other}`")),
    }
}

fn payload(index: usize) -> Vec<u8> {
    let mut state = (index as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x1234_5678_9ABC_DEF0);
    let mut out = Vec::with_capacity(SMALL_BYTES);
    while out.len() < SMALL_BYTES {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

fn chunk_key(index: usize) -> Key {
    Key::object(identity::content_hash(&payload(index)))
}

fn stats_delta(before: &weftfs::BackendStats, after: &weftfs::BackendStats) -> Value {
    json!({
        "reads": after.reads.saturating_sub(before.reads),
        "range_reads": after.range_reads.saturating_sub(before.range_reads),
        "writes": after.writes.saturating_sub(before.writes),
        "bytes_read": after.bytes_read.saturating_sub(before.bytes_read),
        "bytes_written": after.bytes_written.saturating_sub(before.bytes_written),
        "object_requests": (after.reads + after.writes)
            .saturating_sub(before.reads + before.writes),
    })
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
    match std::process::Command::new("date")
        .args(["-u", "+%Y%m%dT%H%M%SZ"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        _ => format!(
            "epoch-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_secs())
                .unwrap_or(0)
        ),
    }
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn percentile(samples: &mut [f64], pct: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((pct / 100.0) * (samples.len() as f64 - 1.0)).ceil() as usize;
    samples[rank.min(samples.len() - 1)]
}

/// Deterministic xorshift64* permutation of `0..count`, first `take`.
fn shuffled(count: usize, take: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    let mut state = seed | 1;
    for index in (1..order.len()).rev() {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let swap = (value % (index as u64 + 1)) as usize;
        order.swap(index, swap);
    }
    order.truncate(take);
    order
}

struct ReadPass {
    config: Value,
    digest: String,
}

fn read_pass(
    plane: &dyn BlobPlane,
    label: &'static str,
    order: &[usize],
) -> Result<ReadPass, String> {
    let mut samples: Vec<f64> = Vec::with_capacity(order.len());
    let mut collected: Vec<(usize, Vec<u8>)> = Vec::with_capacity(order.len());
    let before = plane.stats();
    let started = Instant::now();
    for index in order {
        let key = chunk_key(*index);
        let read_started = Instant::now();
        let bytes = plane
            .get(&key)
            .map_err(|err| format!("{label} read {index}: {err}"))?;
        samples.push(read_started.elapsed().as_secs_f64() * 1000.0);
        let expected = payload(*index);
        if bytes != expected {
            return Err(format!("{label} read {index}: payload mismatch"));
        }
        collected.push((*index, bytes));
    }
    let wall_ms = elapsed_ms(started);
    let delta = stats_delta(&before, &plane.stats());
    let p50 = percentile(&mut samples, 50.0);
    let p99 = percentile(&mut samples, 99.0);
    // Digest in index order so a pass order cannot change it; every pass must
    // reproduce the same digest and the same expected bytes.
    collected.sort_by_key(|(index, _)| *index);
    let mut digest = blake3::Hasher::new();
    for (index, bytes) in &collected {
        digest.update(&(*index as u64).to_le_bytes());
        digest.update(bytes);
    }
    Ok(ReadPass {
        digest: hex::encode(digest.finalize().as_bytes()),
        config: json!({
            "pass": label,
            "reads": order.len(),
            "wall_ms": wall_ms,
            "p50_ms": p50,
            "p99_ms": p99,
            "requests": delta,
        }),
    })
}

fn footprint(config: &str, path: &Path) -> Result<Value, String> {
    let inner = inner_kind(config);
    let plane = inner
        .open_blob_plane(path)
        .map_err(|err| format!("open inner blob plane for footprint: {err}"))?;
    let mut namespaces = serde_json::Map::new();
    let mut total_physical = 0u64;
    for namespace in [
        KeyKind::Object,
        KeyKind::Manifest,
        KeyKind::Pack,
        KeyKind::PackCatalog,
    ] {
        let keys = plane
            .list(&Key::prefix(namespace))
            .map_err(|err| format!("list {namespace:?}: {err}"))?;
        let mut bytes = 0u64;
        for key in &keys {
            bytes += plane
                .len(key)
                .map_err(|err| format!("len {}: {err}", key.display_path()))?;
        }
        total_physical += bytes;
        namespaces.insert(
            namespace.as_str().to_owned(),
            json!({ "objects": keys.len(), "bytes": bytes }),
        );
    }
    let logical_bytes = if config.ends_with("-packed") {
        let mut typed = PackedBlobPlane::new(
            inner.open_blob_plane(path).map_err(|err| err.to_string())?,
            inner
                .open_commit_plane(path)
                .map_err(|err| err.to_string())?,
            pack_options(),
        )
        .map_err(|err| err.to_string())?;
        let stats = typed.catalog_stats().map_err(|err| err.to_string())?;
        Some(json!({
            "packed_members": stats.packed_members,
            "packed_bytes": stats.packed_bytes,
            "flat_members": stats.flat_members,
            "flat_bytes": stats.flat_bytes,
            "catalog_entries": stats.catalog_entries,
            "sealed_packs": stats.sealed_packs,
        }))
    } else {
        None
    };
    Ok(json!({
        "namespaces": namespaces,
        "total_physical_bytes": total_physical,
        "pack_stats": logical_bytes,
    }))
}

fn run_child(config: &str, out: &Path, root: &Path) -> Result<(), String> {
    let child_started = Instant::now();
    let base = root.join(config);
    let _ = std::fs::remove_dir_all(&base);
    let store_path = base.join("store");
    let pack_path = base.join("pack16m");
    if !config.starts_with("s3-") {
        std::fs::create_dir_all(&store_path).map_err(|err| format!("mkdir: {err}"))?;
    }
    let (kind, description) = backend_for(config)?;

    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: SMALL_BYTES as u64,
        checkpoint_interval: 1_000_000,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    };

    let mut store = Store::init_with_backend(&store_path, options, kind.clone())
        .map_err(|err| format!("init: {err}"))?;
    let plane_capabilities = store.plane_capabilities();
    let root_id = store.resolve_path("/").map_err(|err| err.to_string())?;

    // Phase 1: publish 10,000 files in one group commit.
    let write_before = store.backend_stats();
    let write_started = Instant::now();
    store
        .group(|store| {
            for index in 0..SMALL_FILES {
                let body = payload(index);
                store
                    .create_file_seeded(
                        root_id,
                        format!("f{index:05}").as_bytes(),
                        &body,
                        0o644,
                        0,
                        0,
                        Default::default(),
                        format!("packs-seed-{index}").as_bytes(),
                    )
                    .map(|_| ())?;
            }
            Ok(())
        })
        .map_err(|err| format!("group create: {err}"))?;
    let write_ms = elapsed_ms(write_started);
    let write_delta = stats_delta(&write_before, &store.backend_stats());
    let pack_stats_after_write = if config.ends_with("-packed") {
        Some(
            packed_plane(config, &store_path)?
                .catalog_stats()
                .map_err(|err| err.to_string())?,
        )
    } else {
        None
    };

    // Phase 2: read 1,000 chunk blobs sequentially and 1,000 randomly, cold
    // and warm. Each cold pass uses a fresh plane, so the catalog is cold.
    let sequential: Vec<usize> = (0..READS).collect();
    // A permutation of the same 1,000 members, so the random pass exercises
    // random access order over the identical key set and must reproduce the
    // same digest.
    let random = shuffled(READS, READS, 0xC0FF_EE12_3456_789A);
    let mut passes: Vec<Value> = Vec::new();

    let sequential_plane = kind
        .open_blob_plane(&store_path)
        .map_err(|err| format!("open read plane: {err}"))?;
    let cold = read_pass(&*sequential_plane, "sequential_cold", &sequential)?;
    let read_digest = cold.digest.clone();
    passes.push(cold.config);
    let warm = read_pass(&*sequential_plane, "sequential_warm", &sequential)?;
    if warm.digest != read_digest {
        return Err("warm sequential digest differs".to_owned());
    }
    passes.push(warm.config);
    drop(sequential_plane);

    let random_plane = kind
        .open_blob_plane(&store_path)
        .map_err(|err| format!("open read plane: {err}"))?;
    let cold = read_pass(&*random_plane, "random_cold", &random)?;
    if cold.digest != read_digest {
        return Err("random cold digest differs".to_owned());
    }
    passes.push(cold.config);
    let warm = read_pass(&*random_plane, "random_warm", &random)?;
    if warm.digest != read_digest {
        return Err("random warm digest differs".to_owned());
    }
    passes.push(warm.config);
    drop(random_plane);

    // Phase 3: an isolated 16 MiB pack of 4 KiB members.
    let range_phase = range_pack_phase(config, &pack_path)?;

    // Phase 4: verify --full on the writer, then after a read-only reopen.
    let verify_started = Instant::now();
    let report = store.verify(true).map_err(|err| format!("verify: {err}"))?;
    let verify_ms = elapsed_ms(verify_started);
    let generation = store.generation();
    let snapshot_root = store.snapshot_root().to_hex();
    let diagnostics = store.backend_diagnostics();
    let footprint_value = footprint(config, &store_path)?;
    let pack_stats = if config.ends_with("-packed") {
        Some(
            packed_plane(config, &store_path)?
                .catalog_stats()
                .map_err(|err| err.to_string())?,
        )
    } else {
        None
    };
    drop(store);

    let reopen_started = Instant::now();
    let reopened = Store::open_with_backend(&store_path, OpenMode::ReadOnly, kind.clone())
        .map_err(|err| format!("reopen: {err}"))?;
    let reopen_ms = elapsed_ms(reopen_started);
    let reopen_report = reopened
        .verify(true)
        .map_err(|err| format!("reopen verify: {err}"))?;
    let reopen_generation = reopened.generation();
    let reopen_root = reopened.snapshot_root().to_hex();
    drop(reopened);

    let result = json!({
        "config": config,
        "description": description,
        "status": "ok",
        "generation": generation,
        "snapshot_root": snapshot_root,
        "read_digest": read_digest,
        "write": {
            "ms": write_ms,
            "requests": write_delta,
            "pack_stats": pack_stats_after_write,
        },
        "reads": passes,
        "range_pack": range_phase,
        "verify": {
            "ms": verify_ms,
            "problems": report.problems.len(),
            "checked_commits": report.checked_commits,
            "checked_objects": report.checked_objects,
            "checked_manifests": report.checked_manifests,
            "checked_chunks": report.checked_chunks,
        },
        "reopen": {
            "ms": reopen_ms,
            "generation": reopen_generation,
            "snapshot_root": reopen_root,
            "problems": reopen_report.problems.len(),
            "match": reopen_generation == generation && reopen_root == snapshot_root,
        },
        "footprint": footprint_value,
        "planes": {
            "capabilities": plane_capabilities,
            "pack_stats": pack_stats,
        },
        "diagnostics": diagnostics,
        "peak_rss_kib": peak_rss_kib(),
        "wall_ms": elapsed_ms(child_started),
    });

    let text = serde_json::to_string_pretty(&result).map_err(|err| err.to_string())?;
    std::fs::write(out, text).map_err(|err| format!("cannot write {}: {err}", out.display()))?;
    Ok(())
}

/// A typed packed plane over the same physical store and policy.
fn packed_plane(config: &str, path: &Path) -> Result<PackedBlobPlane, String> {
    let inner = inner_kind(config);
    let blobs = inner
        .open_blob_plane(path)
        .map_err(|err| format!("open inner blob plane: {err}"))?;
    let catalog = inner
        .open_commit_plane(path)
        .map_err(|err| format!("open inner commit plane: {err}"))?;
    PackedBlobPlane::new(blobs, catalog, pack_options()).map_err(|err| err.to_string())
}

/// A distinct 8 KiB member for the isolated 16 MiB pack. The index is part of
/// the bytes, so no two members collide (a purely periodic pattern would
/// collide every 256 indices and deduplicate).
fn range_member(index: usize) -> Vec<u8> {
    let mut body = vec![0u8; RANGE_PACK_MEMBER_BYTES];
    body[..8].copy_from_slice(&(index as u64).to_le_bytes());
    for (offset, byte) in body.iter_mut().enumerate().skip(8) {
        *byte = ((index * 31 + offset) & 0xff) as u8;
    }
    body
}

fn range_pack_phase(config: &str, path: &Path) -> Result<Value, String> {
    let options = PackOptions {
        pack_target_bytes: (RANGE_PACK_MEMBERS * RANGE_PACK_MEMBER_BYTES) as u64,
        member_threshold_bytes: RANGE_PACK_MEMBER_BYTES as u64,
        max_members_per_pack: RANGE_PACK_MEMBERS as u32,
        max_catalog_bytes: 4 * 1024 * 1024,
        catalog_segments: 64,
    };
    let inner = if config.starts_with("s3-") {
        s3_kind_in("range16m")
    } else {
        std::fs::create_dir_all(path).map_err(|err| format!("mkdir pack node: {err}"))?;
        BackendKind::Local
    };
    let kind = BackendKind::packed(inner, options).map_err(|err| err.to_string())?;
    let mut plane = kind
        .open_blob_plane(path)
        .map_err(|err| format!("open 16 MiB pack plane: {err}"))?;
    // Physical skeleton for a local directory.
    plane
        .init_layout()
        .map_err(|err| format!("init pack layout: {err}"))?;
    let publish_started = Instant::now();
    for index in 0..RANGE_PACK_MEMBERS {
        let body = range_member(index);
        let key = Key::object(identity::h("weft/packs-bench/range/v1", &body));
        plane
            .put_if_absent(&key, &body)
            .map_err(|err| format!("publish range member {index}: {err}"))?;
    }
    plane
        .flush()
        .map_err(|err| format!("flush 16 MiB pack: {err}"))?;
    let publish_ms = elapsed_ms(publish_started);

    let packs = plane
        .list(&Key::prefix(KeyKind::Pack))
        .map_err(|err| format!("list packs: {err}"))?;
    let mut pack_bytes = 0u64;
    for key in &packs {
        pack_bytes += plane.len(key).map_err(|err| format!("pack len: {err}"))?;
    }

    // Warm the catalog, then read 4 KiB from inside member 1,000.
    let member_index = 1000usize;
    let body = range_member(member_index);
    let key = Key::object(identity::h("weft/packs-bench/range/v1", &body));
    let _ = plane
        .get(&key)
        .map_err(|err| format!("warm range member: {err}"))?;
    let offset = 2048u64;
    let before = plane.stats();
    let read_started = Instant::now();
    let got = plane
        .get_range(&key, offset, RANGE_READ_BYTES)
        .map_err(|err| format!("range read: {err}"))?;
    let read_ms = elapsed_ms(read_started);
    let delta = stats_delta(&before, &plane.stats());
    if got != body[offset as usize..(offset + RANGE_READ_BYTES) as usize] {
        return Err("range read payload mismatch".to_owned());
    }
    drop(plane);

    Ok(json!({
        "config": config,
        "pack_target_bytes": options.pack_target_bytes,
        "members": RANGE_PACK_MEMBERS,
        "member_bytes": RANGE_PACK_MEMBER_BYTES,
        "publish_ms": publish_ms,
        "packs": packs.len(),
        "pack_bytes": pack_bytes,
        "range_offset": offset,
        "range_bytes": RANGE_READ_BYTES,
        "read_ms": read_ms,
        "requests": delta,
        "only_the_range": delta["range_reads"] == 1
            && delta["bytes_read"] == RANGE_READ_BYTES,
    }))
}
