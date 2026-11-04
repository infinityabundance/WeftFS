//! Backend comparison: the same seeded workload over `LocalBackend` and
//! `ObjectBackend`.
//!
//! Workload: 1,000 seeded file creations plus one 100 MiB streaming write.
//! Reported: wall time, backend bytes written, publication primitive counts,
//! on-disk bytes, and whether both backends produced the same commits.
//!
//! Run only inside the bench court:
//! `scripts/docker-court.sh bench "bash bench/scripts/run_backend_bench.sh"`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use weftfs::backend::{BackendKind, BackendStats};
use weftfs::store::{OpenMode, Options, Store};

const CREATES: usize = 1_000;
const MIB: usize = 1024 * 1024;
const WRITE_MIB: usize = 100;

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
    out
}

/// Deterministic non-repeating bytes so no chunk is deduplicated.
struct PatternReader {
    remaining: usize,
    block: u64,
}

impl Read for PatternReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let n = out.len().min(self.remaining);
        let mut x = self
            .block
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(1);
        for byte in &mut out[..n] {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *byte = (x & 0xff) as u8;
        }
        self.block += 1;
        self.remaining -= n;
        Ok(n)
    }
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

fn run_one(label: &str, kind: BackendKind, work: &Path) -> serde_json::Value {
    if work.exists() {
        fs::remove_dir_all(work).expect("clear backend bench dir");
    }
    fs::create_dir_all(work).expect("create backend bench dir");

    let init_started = Instant::now();
    let mut store = Store::init_with_backend(work, opts(), kind.clone()).expect("init store");
    let init_ns = init_started.elapsed().as_nanos() as u64;
    let root = store.resolve_path("/").expect("resolve root");

    let create_started = Instant::now();
    for index in 0..CREATES {
        let name = format!("f{index:04}");
        let payload = [b'x'; 256];
        store
            .create_file_seeded(
                root,
                name.as_bytes(),
                &payload,
                0o644,
                0,
                0,
                BTreeMap::new(),
                &seed(index),
            )
            .expect("create file");
    }
    let create_ns = create_started.elapsed().as_nanos() as u64;

    let write_started = Instant::now();
    let reader = PatternReader {
        remaining: WRITE_MIB * MIB,
        block: 0,
    };
    store
        .create_file_streaming_seeded(
            root,
            b"bulk.bin",
            reader,
            0o644,
            0,
            0,
            BTreeMap::new(),
            &seed(CREATES + 1),
        )
        .expect("stream 100 MiB");
    let write_ns = write_started.elapsed().as_nanos() as u64;

    let stats: BackendStats = store.backend_stats();
    let capabilities = store.backend_capabilities();
    let generation = store.generation();
    let head = store.snapshot_root().to_hex();
    let physical = walk_bytes(work);
    let verify_started = Instant::now();
    let report = store.verify(true).expect("verify");
    let verify_ns = verify_started.elapsed().as_nanos() as u64;
    let diagnostics = store.backend_diagnostics();
    drop(store);

    // Reopen read-only and require the same head.
    let reopened =
        Store::open_with_backend(work, OpenMode::ReadOnly, kind).expect("reopen read-only");
    assert_eq!(reopened.snapshot_root().to_hex(), head, "reopen head");
    drop(reopened);

    json!({
        "backend": label,
        "init_ns": init_ns,
        "creates": CREATES,
        "create_wall_ns": create_ns,
        "create_per_sec": CREATES as f64 / (create_ns as f64 / 1e9),
        "write_mib": WRITE_MIB,
        "write_wall_ns": write_ns,
        "write_mib_per_sec": WRITE_MIB as f64 / (write_ns as f64 / 1e9),
        "verify_wall_ns": verify_ns,
        "verify_ok": report.ok,
        "generation": generation,
        "head": head,
        "physical_bytes": physical,
        "backend_stats": {
            "reads": stats.reads,
            "range_reads": stats.range_reads,
            "bytes_read": stats.bytes_read,
            "writes": stats.writes,
            "bytes_written": stats.bytes_written,
            "link_publications": stats.link_publications,
            "rename_publications": stats.rename_publications,
            "rename_fallbacks": stats.rename_fallbacks,
        },
        "capabilities": {
            "atomic_publish": capabilities.atomic_publish,
            "durable_fsync": capabilities.durable_fsync,
            "true_range_reads": capabilities.true_range_reads,
            "max_object_bytes": capabilities.max_object_bytes,
            "consistency": capabilities.consistency,
            "publication": capabilities.publication,
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

/// Howard Hinnant's `civil_from_days`.
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
    let out = out
        .unwrap_or_else(|| PathBuf::from(format!("bench/receipts/backend-{}.json", utc_stamp())));

    let scratch = std::env::temp_dir().join(format!("weft-backend-bench-{}", std::process::id()));
    let local_dir = scratch.join("local");
    let object_dir = scratch.join("object");

    let local = run_one("local", BackendKind::Local, &local_dir);
    let object = run_one("object", BackendKind::Object, &object_dir);

    let same_head = local["head"] == object["head"];
    let same_generation = local["generation"] == object["generation"];
    assert!(
        same_head && same_generation,
        "backends diverged: local {} / object {}",
        local["head"],
        object["head"]
    );
    let local_bytes = local["physical_bytes"].as_u64().unwrap_or(0);
    let object_bytes = object["physical_bytes"].as_u64().unwrap_or(0);

    let report = json!({
        "schema": "weft-backend-bench-v1",
        "kind": "storage-backend-comparison",
        "label": "Single host, synthetic workload, Docker bench court with hard memory/CPU/pids limits and bounded /tmp. Not a device, fleet, power-loss, RPO/RTO, or production claim.",
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
            "revision_weftcodec": env("WEFT_REPO_WEFTCODEC"),
        },
        "workload": {
            "creates": CREATES,
            "create_payload_bytes": 256,
            "stream_write_mib": WRITE_MIB,
            "chunk_size": 65_536,
            "checkpoint_interval": 256,
            "fixed_timestamp_ns": 1_700_000_000_000_000_000_u64,
        },
        "local": local,
        "object": object,
        "same_generation": same_generation,
        "same_head": same_head,
        "physical_bytes_ratio_object_over_local":
            (object_bytes as f64) / (local_bytes.max(1) as f64),
        "syscalls": null,
        "notes": [
            "No syscall counter is present in the bench court. Wall time and backend byte counters are reported instead.",
            "`bytes_written` counts bytes accepted by the backend publication path; physical bytes are the recursive on-disk total after the run.",
            "Both backends receive the same seeded operations and fixed clock, so equal heads demonstrate identical commit bytes.",
            "The object backend's link/rename publication counts show which primitive was actually exercised; rename_fallbacks is non-zero only if link(2) failed.",
            "No fsync, device-cache, power-loss, or distributed-filesystem claim is made."
        ],
    });

    let text = serde_json::to_string_pretty(&report).expect("serialize receipt");
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("create receipt dir");
    }
    fs::write(&out, text).expect("write receipt");
    let _ = fs::remove_dir_all(&scratch);
    eprintln!("backend_bench: wrote {}", out.display());
    println!(
        "local head {} creates {:.1} ms write {:.1} ms; object head {} creates {:.1} ms write {:.1} ms; same_head={same_head}",
        local["head"].as_str().unwrap_or("?"),
        local["create_wall_ns"].as_u64().unwrap_or(0) as f64 / 1e6,
        local["write_wall_ns"].as_u64().unwrap_or(0) as f64 / 1e6,
        object["head"].as_str().unwrap_or("?"),
        object["create_wall_ns"].as_u64().unwrap_or(0) as f64 / 1e6,
        object["write_wall_ns"].as_u64().unwrap_or(0) as f64 / 1e6,
    );
}
