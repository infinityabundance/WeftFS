//! Local-vs-S3 storage backend benchmark (subphase Q4).
//!
//! Workload: 1,000 seeded 256-byte creates, 100 x 1 MiB seeded writes
//! (100 MiB total), and 1,000 reads of the small files. The same seeded
//! operations run over `LocalBackend` and the S3-compatible endpoint.
//!
//! Scope label: a single-node MinIO (or other S3-compatible) endpoint on
//! localhost, in the OOM-protected court. This is not a cloud, multi-node,
//! fleet, device-cache, or power-loss claim.
//!
//! Run: `scripts/s3_court.sh --bench` (or inside the rust court with the S3
//! environment variables set).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use weftfs::backend::BackendKind;
use weftfs::s3::S3Config;
use weftfs::store::{OpenMode, Options, Store};

const CREATES: usize = 1_000;
const WRITE_FILES: usize = 100;
const MIB: usize = 1024 * 1024;

fn opts() -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: 65_536,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        ..Options::default()
    }
}

fn seed(index: usize) -> Vec<u8> {
    let mut out = vec![0u8; 32];
    out[..8].copy_from_slice(&(index as u64).to_le_bytes());
    out[8..16].copy_from_slice(&(index as u64).wrapping_mul(2_654_435_761).to_le_bytes());
    out
}

/// Deterministic non-repeating bytes so no chunk deduplicates.
fn pattern(index: usize, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut x = (index as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(1);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push((x & 0xff) as u8);
    }
    out
}

fn duration_us(durations: &[Duration]) -> serde_json::Value {
    if durations.is_empty() {
        return json!(null);
    }
    let mut sorted: Vec<u128> = durations.iter().map(|value| value.as_micros()).collect();
    sorted.sort_unstable();
    let at = |percent: f64| -> f64 {
        let index = ((sorted.len() as f64 - 1.0) * percent).round() as usize;
        sorted[index.min(sorted.len() - 1)] as f64
    };
    json!({
        "samples": sorted.len(),
        "p50_us": at(0.50),
        "p95_us": at(0.95),
        "max_us": *sorted.last().unwrap() as f64,
    })
}

fn walk_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total = total.saturating_add(walk_bytes(&entry.path()));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

struct Phase {
    wall: Duration,
    per_op: Vec<Duration>,
    bytes: u64,
}

fn run_backend(label: &str, kind: BackendKind, store_path: &Path) -> serde_json::Value {
    if store_path.exists() && kind.label() != "s3" {
        fs::remove_dir_all(store_path).expect("clear bench store");
        fs::create_dir_all(store_path).expect("create bench store");
    }
    let init_started = Instant::now();
    let mut store =
        Store::init_with_backend(store_path, opts(), kind.clone()).expect("init bench store");
    let init = init_started.elapsed();
    let root = store.resolve_path("/").expect("resolve root");

    // 1,000 small creates.
    let started = Instant::now();
    let mut per_op = Vec::with_capacity(CREATES);
    let mut ids = Vec::with_capacity(CREATES);
    for index in 0..CREATES {
        let name = format!("small-{index:04}");
        let payload = format!("weft-s3-bench-{index:04}");
        let op = Instant::now();
        let id = store
            .create_file_seeded(
                root,
                name.as_bytes(),
                payload.as_bytes(),
                0o644,
                0,
                0,
                BTreeMap::new(),
                &seed(index),
            )
            .expect("create small file");
        per_op.push(op.elapsed());
        ids.push(id);
    }
    let creates = Phase {
        wall: started.elapsed(),
        per_op,
        bytes: (CREATES * 25) as u64,
    };

    // 100 MiB as 100 x 1 MiB files.
    let started = Instant::now();
    let mut per_op = Vec::with_capacity(WRITE_FILES);
    for index in 0..WRITE_FILES {
        let name = format!("bulk-{index:03}");
        let payload = pattern(index + CREATES, MIB);
        let op = Instant::now();
        store
            .create_file_seeded(
                root,
                name.as_bytes(),
                &payload,
                0o644,
                0,
                0,
                BTreeMap::new(),
                &seed(CREATES + index),
            )
            .expect("create bulk file");
        per_op.push(op.elapsed());
    }
    let writes = Phase {
        wall: started.elapsed(),
        per_op,
        bytes: (WRITE_FILES * MIB) as u64,
    };

    // 1,000 reads of the small files.
    let started = Instant::now();
    let mut per_op = Vec::with_capacity(CREATES);
    let mut read_bytes = 0u64;
    for id in &ids {
        let op = Instant::now();
        let bytes = store.read_object(*id).expect("read small file");
        per_op.push(op.elapsed());
        read_bytes += bytes.len() as u64;
    }
    let reads = Phase {
        wall: started.elapsed(),
        per_op,
        bytes: read_bytes,
    };

    let stats = store.backend_stats();
    let generation = store.generation();
    let head = store.snapshot_root().to_hex();
    let verify_started = Instant::now();
    let verify = store.verify(true).expect("verify");
    let verify_wall = verify_started.elapsed();
    let diagnostics = store.backend_diagnostics();
    drop(store);

    let reopened =
        Store::open_with_backend(store_path, OpenMode::ReadOnly, kind).expect("reopen read-only");
    assert_eq!(reopened.snapshot_root().to_hex(), head, "reopen head");
    drop(reopened);

    let physical = if label == "s3" {
        0
    } else {
        walk_bytes(store_path)
    };
    let secs = |phase: &Phase| phase.wall.as_secs_f64();
    json!({
        "backend": label,
        "init_us": init.as_micros() as u64,
        "generation": generation,
        "head": head,
        "verify_ok": verify.ok,
        "verify_wall_us": verify_wall.as_micros() as u64,
        "creates": {
            "ops": CREATES,
            "payload_bytes": creates.bytes,
            "wall_us": creates.wall.as_micros() as u64,
            "ops_per_sec": CREATES as f64 / secs(&creates).max(f64::MIN_POSITIVE),
            "latency": duration_us(&creates.per_op),
        },
        "writes": {
            "ops": WRITE_FILES,
            "mib": WRITE_FILES,
            "bytes": writes.bytes,
            "wall_us": writes.wall.as_micros() as u64,
            "mib_per_sec": WRITE_FILES as f64 / secs(&writes).max(f64::MIN_POSITIVE),
            "latency": duration_us(&writes.per_op),
        },
        "reads": {
            "ops": CREATES,
            "bytes": reads.bytes,
            "wall_us": reads.wall.as_micros() as u64,
            "ops_per_sec": CREATES as f64 / secs(&reads).max(f64::MIN_POSITIVE),
            "latency": duration_us(&reads.per_op),
        },
        "physical_bytes": physical,
        "backend_stats": {
            "reads": stats.reads,
            "range_reads": stats.range_reads,
            "bytes_read": stats.bytes_read,
            "writes": stats.writes,
            "bytes_written": stats.bytes_written,
        },
        "diagnostics": diagnostics,
    })
}

fn utc_stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = now / 86_400;
    let secs = now % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut out: Option<PathBuf> = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                out = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let out = out.unwrap_or_else(|| {
        PathBuf::from(format!("bench/receipts/s3-backend-{}.json", utc_stamp()))
    });

    let config = S3Config::from_env().expect("S3 configuration for the bench");
    let scratch = std::env::temp_dir().join(format!("weft-s3-bench-{}", std::process::id()));
    let local_dir = scratch.join("local");
    let s3_label = scratch.join("s3-label");

    let local = run_backend("local", BackendKind::Local, &local_dir);
    let s3 = run_backend(
        "s3",
        BackendKind::S3 {
            config: config.clone(),
        },
        &s3_label,
    );

    let same_head = local["head"] == s3["head"];
    let same_generation = local["generation"] == s3["generation"];
    assert!(
        same_head && same_generation,
        "local and S3 stores diverged: local {} / s3 {}",
        local["head"],
        s3["head"]
    );

    let report = json!({
        "schema": "weft-s3-backend-bench-v1",
        "kind": "local-vs-s3-storage-backend",
        "generated_utc": utc_stamp(),
        "label": "Single-node S3-compatible endpoint on localhost inside the OOM-protected Docker court. Not a cloud, multi-node, fleet, device-cache, power-loss, RPO/RTO, or production claim.",
        "environment": {
            "kernel": env("WEFT_COURT_KERNEL"),
            "court_image": env("WEFT_COURT_IMAGE"),
            "court_image_id": env("WEFT_COURT_IMAGE_ID"),
            "court_memory": env("WEFT_COURT_MEMORY"),
            "court_cpus": env("WEFT_COURT_CPUS"),
            "court_pids": env("WEFT_COURT_PIDS"),
            "docker_client": env("WEFT_DOCKER_CLIENT"),
            "docker_server": env("WEFT_DOCKER_SERVER"),
            "revision_weftfs": env("WEFT_REPO_WEFTFS"),
            "s3_endpoint": config.endpoint,
            "s3_bucket": config.bucket,
            "s3_region": config.region,
            "s3_server": env("WEFT_S3_SERVER_LABEL"),
            "s3_server_version": env("WEFT_S3_SERVER_VERSION"),
            "s3_server_sha256": env("WEFT_S3_SERVER_SHA256"),
            "s3_container_image": env("WEFT_S3_CONTAINER_IMAGE"),
            "s3_container_image_id": env("WEFT_S3_CONTAINER_IMAGE_ID"),
        },
        "workload": {
            "creates": CREATES,
            "create_payload_bytes": 25,
            "write_files": WRITE_FILES,
            "write_mib": WRITE_FILES,
            "reads": CREATES,
            "chunk_size": 65_536,
            "checkpoint_interval": 256,
            "fixed_timestamp_ns": 1_700_000_000_000_000_000_u64,
        },
        "local": local,
        "s3": s3,
        "same_head": same_head,
        "same_generation": same_generation,
        "notes": [
            "Both backends received the same seeded operations and fixed clock, so an equal head demonstrates identical logical content.",
            "`s3.backend_stats` counts HTTP-backed backend reads and writes; S3 provides no physical byte counter, so `physical_bytes` is 0 there and the requested payload bytes are reported per phase.",
            "MinIO runs as one container on a localhost bridge network with a tmpfs data directory. Latency includes loopback HTTP and SigV4 signing.",
            "No fsync, media, power-loss, cloud, multi-node, or fleet claim is made."
        ],
    });

    let text = serde_json::to_string_pretty(&report).expect("serialize receipt");
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("create receipt dir");
    }
    fs::write(&out, text).expect("write receipt");
    let _ = fs::remove_dir_all(&scratch);
    eprintln!("s3_backend_bench: wrote {}", out.display());
    println!(
        "local creates {:.1} ms writes {:.1} ms reads {:.1} ms; s3 creates {:.1} ms writes {:.1} ms reads {:.1} ms; same_head={same_head}",
        local["creates"]["wall_us"].as_u64().unwrap_or(0) as f64 / 1e3,
        local["writes"]["wall_us"].as_u64().unwrap_or(0) as f64 / 1e3,
        local["reads"]["wall_us"].as_u64().unwrap_or(0) as f64 / 1e3,
        s3["creates"]["wall_us"].as_u64().unwrap_or(0) as f64 / 1e3,
        s3["writes"]["wall_us"].as_u64().unwrap_or(0) as f64 / 1e3,
        s3["reads"]["wall_us"].as_u64().unwrap_or(0) as f64 / 1e3,
    );
}
