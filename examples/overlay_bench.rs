//! Overlay court harness: the same workload through the chunk-COW writer and
//! the immutable-overlay writer, on one backend, with compaction.
//!
//! Workload (deterministic, seeded): one 128 MiB object (64 KiB chunks) then
//! 500 overwrites of 4 KiB at 4 KiB-aligned pseudo-random offsets. Both
//! configurations use `content_identity = merkle`, so their content roots are
//! directly comparable and must be equal.
//!
//! Each configuration runs in its own child process so `VmHWM` is attributable
//! and no page cache or in-process state leaks between them. Per configuration:
//! write-latency p50/p99, backend request and byte counters, a 4 KiB range read,
//! a full read, `verify --full`, retained blob bytes and (local only) on-disk
//! bytes. The overlay configuration additionally creates a snapshot before
//! compaction, compacts, and proves the root, the object bytes and the snapshot
//! bytes are unchanged. The COW configuration calls `compact` too and records
//! the documented `UnsupportedRepresentation` refusal.
//!
//! Run: `bench/scripts/overlay_court.sh` (local) or
//! `scripts/s3_court.sh --overlay` (MinIO).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use weftfs::backend::BackendKind;
use weftfs::format::{ContentIdentity, RepresentationKind};
use weftfs::identity::{self, Hash32};
use weftfs::s3::{S3Backend, S3Config};
use weftfs::store::{CompactReport, OpenMode, Options, RepresentationMode, Store};

const FILE_SIZE: u64 = 128 * 1024 * 1024;
const CHUNK_SIZE: u64 = 64 * 1024;
const WRITES: usize = 500;
const WRITE_LEN: u64 = 4096;
const WRITE_ALIGN: u64 = 4096;
const RANGE_REPS: usize = 20;
const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn options(mode: RepresentationMode) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK_SIZE,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        representation: mode,
        content_identity: ContentIdentity::MerkleV3,
        ..Options::default()
    }
}

fn initial_content() -> Vec<u8> {
    let mut rng = Rng(SEED);
    let mut content = Vec::with_capacity(FILE_SIZE as usize);
    while content.len() < FILE_SIZE as usize {
        let take = (FILE_SIZE as usize - content.len()).min(1024 * 1024);
        for _ in 0..take {
            content.push((rng.next() >> 24) as u8);
        }
    }
    content
}

/// The deterministic write plan: offsets and payloads derived from the seed
/// only, so every configuration and every rerun applies the same bytes.
fn plan() -> (Vec<u64>, Vec<Vec<u8>>) {
    let mut rng = Rng(SEED ^ 0x5DEE_CE66_D1CE_4E5B);
    let chunks = FILE_SIZE / WRITE_ALIGN;
    let mut offsets = Vec::with_capacity(WRITES);
    let mut payloads = Vec::with_capacity(WRITES);
    for _ in 0..WRITES {
        offsets.push((rng.next() % chunks) * WRITE_ALIGN);
        let mut payload = Vec::with_capacity(WRITE_LEN as usize);
        for _ in 0..WRITE_LEN {
            payload.push((rng.next() >> 24) as u8);
        }
        payloads.push(payload);
    }
    (offsets, payloads)
}

fn duration_stats(durations: &[Duration]) -> Value {
    if durations.is_empty() {
        return json!(null);
    }
    let mut sorted: Vec<f64> = durations.iter().map(|d| d.as_secs_f64() * 1e3).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |percent: f64| -> f64 {
        let index = ((sorted.len() as f64 - 1.0) * percent).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    };
    json!({
        "samples": sorted.len(),
        "p50_ms": at(0.50),
        "p99_ms": at(0.99),
        "max_ms": sorted[sorted.len() - 1],
        "total_ms": sorted.iter().sum::<f64>(),
    })
}

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
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

fn backend_stats(store: &Store) -> Value {
    let stats = store.backend_stats();
    json!({
        "reads": stats.reads,
        "range_reads": stats.range_reads,
        "writes": stats.writes,
        "bytes_read": stats.bytes_read,
        "bytes_written": stats.bytes_written,
        "link_publications": stats.link_publications,
        "rename_publications": stats.rename_publications,
    })
}

fn delta(before: &Value, after: &Value, field: &str) -> u64 {
    after[field].as_u64().unwrap_or(0) - before[field].as_u64().unwrap_or(0)
}

fn manifest_of(store: &Store, id: Hash32) -> weftfs::format::Manifest {
    let content = store.object_meta(id).expect("object metadata").content;
    store.manifest_of(content).expect("manifest")
}

fn overlay_shape(store: &Store, id: Hash32) -> Value {
    let manifest = manifest_of(store, id);
    match manifest.representation.as_ref() {
        Some(rep) if rep.kind == RepresentationKind::Overlay => {
            let overlay = rep.overlay.as_ref().expect("overlay map");
            json!({
                "kind": "overlay",
                "depth": overlay.depth,
                "extents": overlay.extents.len(),
                "overlay_bytes": overlay.overlay_bytes,
                "chunks": overlay.extents.iter().map(|e| e.chunks.len() as u64).sum::<u64>(),
            })
        }
        Some(rep) => json!({"kind": rep.kind.as_str()}),
        None => json!({
            "kind": "raw",
            "chunks": manifest.chunks.len(),
            "representation": "none (canonical chunk table)",
        }),
    }
}

struct ReadResult {
    wall: Duration,
    bytes: u64,
    digest: Hash32,
    backend_bytes: u64,
    backend_reads: u64,
}

fn measure_full_read(store: &Store, id: Hash32) -> ReadResult {
    let before = backend_stats(store);
    let started = Instant::now();
    let bytes = store.read_object(id).expect("full read");
    let wall = started.elapsed();
    let after = backend_stats(store);
    ReadResult {
        wall,
        bytes: bytes.len() as u64,
        digest: identity::content_hash(&bytes),
        backend_bytes: delta(&before, &after, "bytes_read"),
        backend_reads: delta(&before, &after, "reads") + delta(&before, &after, "range_reads"),
    }
}

fn read_result_json(label: &str, result: &ReadResult) -> Value {
    json!({
        "label": label,
        "bytes": result.bytes,
        "digest": result.digest.to_hex(),
        "wall_ms": result.wall.as_secs_f64() * 1e3,
        "backend_requests": result.backend_reads,
        "backend_bytes_read": result.backend_bytes,
    })
}

fn compact_result_json(
    report: &CompactReport,
    wall: Duration,
    backend_read: u64,
    backend_requests: u64,
) -> Value {
    json!({
        "previous_content": report.previous_content.to_hex(),
        "content": report.content.to_hex(),
        "size": report.size,
        "content_root": report.content_root.to_hex(),
        "chunks": report.chunks,
        "overlay_depth": report.overlay_depth,
        "overlay_bytes": report.overlay_bytes,
        "verified_bytes": report.verified_bytes,
        "wall_ms": wall.as_secs_f64() * 1e3,
        "backend_bytes_read": backend_read,
        "backend_requests": backend_requests,
    })
}

/// One measured configuration, in its own process.
fn run_config(child: &str, store_path: &Path, out: &Path) {
    let overlay = child == "overlay";
    let mode = if overlay {
        RepresentationMode::Overlay
    } else {
        RepresentationMode::Raw
    };
    let backend = backend_from_env();
    if matches!(backend, BackendKind::Local) {
        if store_path.exists() {
            fs::remove_dir_all(store_path).expect("clear store");
        }
    }
    let mut store =
        Store::init_with_backend(store_path, options(mode), backend).expect("init store");
    let root = store.resolve_path("/").expect("resolve root");
    let content = initial_content();
    let (offsets, payloads) = plan();

    let init_started = Instant::now();
    store
        .create_file_seeded(
            root,
            b"f",
            &content,
            0o644,
            0,
            0,
            BTreeMap::new(),
            &[0x42u8; 32],
        )
        .expect("create 128 MiB object");
    let init = init_started.elapsed();
    drop(content);
    let id = store.resolve_path("/f").expect("resolve f");
    let root_before_writes = manifest_of(&store, id).content_hash;

    let before_writes = backend_stats(&store);
    let mut walls = Vec::with_capacity(WRITES);
    let writes_started = Instant::now();
    for (offset, payload) in offsets.iter().zip(payloads.iter()) {
        let started = Instant::now();
        store.write_file(id, *offset, payload).expect("range write");
        walls.push(started.elapsed());
    }
    let writes_wall = writes_started.elapsed();
    let after_writes = backend_stats(&store);

    let root_after_writes = manifest_of(&store, id).content_hash;
    let shape = overlay_shape(&store, id);

    // A 4 KiB range read: only intersecting chunks are fetched.
    let range_offset = offsets[0];
    let range_before = backend_stats(&store);
    let mut range_walls = Vec::with_capacity(RANGE_REPS);
    let mut range_bytes = 0u64;
    for _ in 0..RANGE_REPS {
        let started = Instant::now();
        let bytes = store
            .read_range(id, range_offset, WRITE_LEN)
            .expect("range read");
        range_walls.push(started.elapsed());
        range_bytes += bytes.len() as u64;
    }
    let range_after = backend_stats(&store);
    let range_reads = delta(&range_before, &range_after, "reads")
        + delta(&range_before, &range_after, "range_reads");
    let range_backend_bytes = delta(&range_before, &range_after, "bytes_read");

    // The same range read through a freshly opened read-only handle: an honest
    // proxy for a new process, where the overlay chain must be resolved from
    // blobs instead of an in-process validated table.
    let cold_range = {
        let cold = Store::open_with_options_and_backend(
            store_path,
            OpenMode::ReadOnly,
            options(mode),
            backend_from_env(),
        )
        .expect("reopen read-only");
        let cold_id = cold.resolve_path("/f").expect("resolve f (cold)");
        let before = backend_stats(&cold);
        let first_started = Instant::now();
        let first_bytes = cold
            .read_range(cold_id, range_offset, WRITE_LEN)
            .expect("cold range read")
            .len() as u64;
        let first_wall = first_started.elapsed();
        let first_stats = backend_stats(&cold);
        let mut walls = Vec::with_capacity(RANGE_REPS);
        let mut bytes = first_bytes;
        for _ in 0..RANGE_REPS {
            let started = Instant::now();
            let read = cold
                .read_range(cold_id, range_offset, WRITE_LEN)
                .expect("cold range read");
            walls.push(started.elapsed());
            bytes += read.len() as u64;
        }
        let after = backend_stats(&cold);
        json!({
            "offset": range_offset,
            "length": WRITE_LEN,
            "reps": RANGE_REPS + 1,
            "bytes_per_read": bytes / (RANGE_REPS + 1) as u64,
            "latency": duration_stats(&walls),
            "first_read_ms": first_wall.as_secs_f64() * 1e3,
            "first_read_backend_requests": delta(&before, &first_stats, "reads")
                + delta(&before, &first_stats, "range_reads"),
            "first_read_backend_bytes": delta(&before, &first_stats, "bytes_read"),
            "backend_requests_per_read": (delta(&before, &after, "reads")
                + delta(&before, &after, "range_reads")) / (RANGE_REPS + 1) as u64,
            "backend_bytes_per_read": delta(&before, &after, "bytes_read") / (RANGE_REPS + 1) as u64,
        })
    };

    let full_before_compaction = measure_full_read(&store, id);
    let snapshot = store.snapshot_create("overlay-court").expect("snapshot");
    let snapshot_record = {
        let state = store.state_at(snapshot).expect("snapshot state");
        state.objects.get(&id).expect("snapshot record").clone()
    };
    let snapshot_bytes_before = store
        .read_content(snapshot_record.content, 0, snapshot_record.size)
        .expect("snapshot read before compaction");
    let snapshot_digest_before = identity::content_hash(&snapshot_bytes_before);

    let verify_started = Instant::now();
    let verify_before = store.verify(true).expect("verify");
    let verify_before_wall = verify_started.elapsed();
    let retained_before = store.retained_blob_bytes().expect("retained bytes");

    // Compaction: the overlay configuration must succeed with an unchanged
    // root; the COW configuration must refuse.
    let compact_before = backend_stats(&store);
    let compact_started = Instant::now();
    let compaction = if overlay {
        match store.compact(id) {
            Ok(report) => {
                let wall = compact_started.elapsed();
                let after = backend_stats(&store);
                json!({
                    "applicable": true,
                    "ok": true,
                    "report": compact_result_json(
                        &report,
                        wall,
                        delta(&compact_before, &after, "bytes_read"),
                        delta(&compact_before, &after, "reads")
                            + delta(&compact_before, &after, "range_reads"),
                    ),
                })
            }
            Err(err) => json!({
                "applicable": true,
                "ok": false,
                "class": err.class_name(),
                "message": err.message(),
            }),
        }
    } else {
        match store.compact(id) {
            Ok(_) => json!({
                "applicable": false,
                "ok": false,
                "detail": "COW configuration unexpectedly compacted",
            }),
            Err(err) => json!({
                "applicable": false,
                "ok": false,
                "refused": true,
                "class": err.class_name(),
                "message": err.message(),
                "wall_ms": compact_started.elapsed().as_secs_f64() * 1e3,
            }),
        }
    };
    let compact_after = backend_stats(&store);

    // Post-compaction measurements. Both configurations run them so the table
    // can be compared column by column.
    let root_after_compaction = manifest_of(&store, id).content_hash;
    let shape_after = overlay_shape(&store, id);
    let full_after_compaction = measure_full_read(&store, id);
    let snapshot_after_compaction = store
        .read_content(snapshot_record.content, 0, snapshot_record.size)
        .expect("snapshot read after compaction");
    let snapshot_digest_after_compaction = identity::content_hash(&snapshot_after_compaction);
    let verify_after_started = Instant::now();
    let verify_after = store.verify(true).expect("verify after compaction");
    let verify_after_wall = verify_after_started.elapsed();
    let retained_after = store.retained_blob_bytes().expect("retained bytes after");

    // Reclaim what compaction and the writes left unreachable. This is not part
    // of compaction; it is measured separately so the table can show where the
    // physical bytes went.
    let gc_started = Instant::now();
    let gc = store.gc(true).expect("gc after compaction");
    let gc_wall = gc_started.elapsed();
    let retained_after_gc = store
        .retained_blob_bytes()
        .expect("retained bytes after gc");

    // The strongest snapshot-retention evidence: the pinned version must still
    // read, byte for byte, after compaction and after GC.
    let snapshot_after_gc = store
        .read_content(snapshot_record.content, 0, snapshot_record.size)
        .expect("snapshot read after gc");
    let snapshot_digest_after_gc = identity::content_hash(&snapshot_after_gc);
    let retained_after_gc = store
        .retained_blob_bytes()
        .expect("retained bytes after gc");

    let disk = if backend_kind_label() == "local" {
        json!(walk_bytes(store_path))
    } else {
        json!(null)
    };

    let report = json!({
        "configuration": child,
        "representation": mode.as_str(),
        "content_identity": "merkle",
        "create": {
            "wall_ms": init.as_secs_f64() * 1e3,
            "bytes": FILE_SIZE,
        },
        "writes": {
            "ops": WRITES,
            "bytes_per_op": WRITE_LEN,
            "wall_ms": writes_wall.as_secs_f64() * 1e3,
            "latency": duration_stats(&walls),
            "backend_requests": delta(&before_writes, &after_writes, "reads")
                + delta(&before_writes, &after_writes, "range_reads")
                + delta(&before_writes, &after_writes, "writes"),
            "backend_bytes_written": delta(&before_writes, &after_writes, "bytes_written"),
            "backend_bytes_read": delta(&before_writes, &after_writes, "bytes_read"),
        },
        "content_root_before_writes": root_before_writes.to_hex(),
        "content_root_after_writes": root_after_writes.to_hex(),
        "shape_after_writes": shape,
        "range_read": {
            "offset": range_offset,
            "length": WRITE_LEN,
            "reps": RANGE_REPS,
            "bytes_per_read": range_bytes / RANGE_REPS as u64,
            "latency": duration_stats(&range_walls),
            "backend_requests_per_read": range_reads / RANGE_REPS as u64,
            "backend_bytes_per_read": range_backend_bytes / RANGE_REPS as u64,
        },
        "range_read_cold_handle": cold_range,
        "full_read_before_compaction": read_result_json("before", &full_before_compaction),
        "verify_before_compaction": {
            "ok": verify_before.ok,
            "wall_ms": verify_before_wall.as_secs_f64() * 1e3,
            "checked_chunks": verify_before.checked_chunks,
            "checked_representations": verify_before.checked_representations,
            "problems": verify_before.problems.iter().map(|p| format!("{}: {}", p.class, p.detail)).collect::<Vec<_>>(),
        },
        "snapshot": {
            "commit": snapshot.to_hex(),
            "content": snapshot_record.content.to_hex(),
            "size": snapshot_record.size,
            "digest_before_compaction": snapshot_digest_before.to_hex(),
            "digest_after_compaction": snapshot_digest_after_compaction.to_hex(),
            "digest_after_gc": snapshot_digest_after_gc.to_hex(),
            "unchanged": snapshot_digest_before == snapshot_digest_after_compaction
                && snapshot_digest_before == snapshot_digest_after_gc,
        },
        "compaction": compaction,
        "compaction_backend_bytes_read": delta(&compact_before, &compact_after, "bytes_read"),
        "compaction_backend_requests": delta(&compact_before, &compact_after, "reads")
            + delta(&compact_before, &compact_after, "range_reads"),
        "content_root_after_compaction": root_after_compaction.to_hex(),
        "root_unchanged_by_compaction": root_after_writes == root_after_compaction,
        "shape_after_compaction": shape_after,
        "full_read_after_compaction": read_result_json("after", &full_after_compaction),
        "verify_after_compaction": {
            "ok": verify_after.ok,
            "wall_ms": verify_after_wall.as_secs_f64() * 1e3,
            "checked_chunks": verify_after.checked_chunks,
            "checked_representations": verify_after.checked_representations,
            "problems": verify_after.problems.iter().map(|p| format!("{}: {}", p.class, p.detail)).collect::<Vec<_>>(),
        },
        "retained_blob_bytes_before_compaction": retained_before,
        "retained_blob_bytes_after_compaction": retained_after,
        "retained_blob_bytes_after_gc": retained_after_gc,
        "gc_after_compaction": {
            "apply": true,
            "wall_ms": gc_wall.as_secs_f64() * 1e3,
            "marked_manifests": gc.marked_manifests,
            "marked_chunks": gc.marked_chunks,
            "deleted": gc.deleted.len(),
            "deleted_bytes": gc.deleted_bytes,
            "removed_orphans": gc.removed_orphans.len(),
            "problems": gc.problems,
        },
        "disk_bytes": disk,
        "peak_rss_kib": peak_rss_kib(),
    });

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("create output dir");
    }
    fs::write(
        out,
        serde_json::to_string_pretty(&report).expect("serialize"),
    )
    .expect("write configuration result");
    eprintln!("overlay_bench: {child} -> {}", out.display());
}

fn backend_kind_label() -> String {
    std::env::var("WEFT_OVERLAY_COURT_BACKEND").unwrap_or_else(|_| "local".to_owned())
}

fn backend_from_env() -> BackendKind {
    if backend_kind_label() == "s3" {
        BackendKind::S3 {
            config: S3Config::from_env().expect("S3 configuration"),
        }
    } else {
        BackendKind::Local
    }
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
    if args.get(1).map(String::as_str) == Some("child") {
        let config = args[2].clone();
        let store_path = PathBuf::from(&args[3]);
        let out = PathBuf::from(&args[4]);
        run_config(&config, &store_path, &out);
        return;
    }

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
        .unwrap_or_else(|| PathBuf::from(format!("bench/receipts/overlay-{}.json", utc_stamp())));

    let backend_label = backend_kind_label();
    let base_bucket = env("WEFT_S3_BUCKET").unwrap_or_else(|| "weft-overlay-court".to_owned());
    let scratch = std::env::temp_dir().join(format!("weft-overlay-court-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create scratch");

    let mut results: BTreeMap<String, Value> = BTreeMap::new();
    for config in ["cow", "overlay"] {
        let child_out = scratch.join(format!("{config}.json"));
        let store_path = scratch.join(config);
        let mut command = Command::new(std::env::current_exe().expect("current exe"));
        command
            .arg("child")
            .arg(config)
            .arg(&store_path)
            .arg(&child_out)
            .env("WEFT_OVERLAY_COURT_BACKEND", &backend_label);
        if backend_label == "s3" {
            let mut config_s3 = S3Config::from_env().expect("S3 configuration");
            config_s3.bucket = format!("{base_bucket}-{config}");
            let backend = S3Backend::new("<overlay-court>", config_s3.clone());
            backend.ensure_bucket().expect("ensure bucket");
            command.env("WEFT_S3_BUCKET", &config_s3.bucket);
        }
        let status = command.status().expect("spawn child");
        assert!(status.success(), "child {config} failed: {status:?}");
        let text = fs::read_to_string(&child_out).expect("child result");
        results.insert(
            config.to_owned(),
            serde_json::from_str(&text).expect("child json"),
        );
    }

    let cow = &results["cow"];
    let overlay = &results["overlay"];
    let cow_root = cow["content_root_after_writes"]
        .as_str()
        .unwrap_or_default();
    let overlay_root = overlay["content_root_after_writes"]
        .as_str()
        .unwrap_or_default();
    let cow_digest = cow["full_read_before_compaction"]["digest"]
        .as_str()
        .unwrap_or_default();
    let overlay_digest = overlay["full_read_before_compaction"]["digest"]
        .as_str()
        .unwrap_or_default();
    let snapshot_unchanged = overlay["snapshot"]["unchanged"].as_bool().unwrap_or(false);
    let root_unchanged = overlay["root_unchanged_by_compaction"]
        .as_bool()
        .unwrap_or(false);
    let cow_refused = cow["compaction"]["refused"].as_bool().unwrap_or(false);
    let verify_ok = cow["verify_before_compaction"]["ok"]
        .as_bool()
        .unwrap_or(false)
        && overlay["verify_before_compaction"]["ok"]
            .as_bool()
            .unwrap_or(false)
        && cow["verify_after_compaction"]["ok"]
            .as_bool()
            .unwrap_or(false)
        && overlay["verify_after_compaction"]["ok"]
            .as_bool()
            .unwrap_or(false);
    let same_bytes = cow_digest == overlay_digest && !cow_digest.is_empty();

    let checks = json!([
        {"name": "content_root_matches_between_representations", "ok": cow_root == overlay_root && !cow_root.is_empty()},
        {"name": "full_read_bytes_identical", "ok": same_bytes},
        {"name": "overlay_compaction_preserves_content_root", "ok": root_unchanged},
        {"name": "snapshot_read_unchanged_by_compaction", "ok": snapshot_unchanged},
        {"name": "cow_compaction_is_refused", "ok": cow_refused},
        {"name": "verify_full_ok_everywhere", "ok": verify_ok},
    ]);
    let passed = checks
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["ok"] == json!(true))
        .count();

    let report = json!({
        "schema": "weft-overlay-court-v1",
        "kind": "chunk-cow-vs-immutable-write-overlay",
        "generated_utc": utc_stamp(),
        "backend": backend_label,
        "label": "Single-host synthetic court inside the OOM-protected bench container. Local means the normative POSIX layout on the container filesystem; s3 means single-node MinIO on a localhost bridge network with a tmpfs data directory. Not a cloud, multi-node, fleet, media, device-cache, power-loss, RPO/RTO, or production claim.",
        "workload": {
            "file_size": FILE_SIZE,
            "chunk_size": CHUNK_SIZE,
            "writes": WRITES,
            "write_bytes": WRITE_LEN,
            "write_alignment": WRITE_ALIGN,
            "seed": format!("{SEED:#x}"),
            "range_read_reps": RANGE_REPS,
            "content_identity": "merkle",
            "max_overlay_depth": weftfs::representation::DEFAULT_MAX_OVERLAY_DEPTH,
            "max_overlay_bytes": weftfs::representation::DEFAULT_MAX_OVERLAY_BYTES,
        },
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
            "revision_weftfs_go": env("WEFT_REPO_WEFTFS_GO"),
            "revision_weftcodec": env("WEFT_REPO_WEFTCODEC"),
            "s3_endpoint": env("WEFT_S3_ENDPOINT"),
            "s3_bucket_base": env("WEFT_S3_BUCKET"),
            "s3_server": env("WEFT_S3_SERVER_LABEL"),
            "s3_server_version": env("WEFT_S3_SERVER_VERSION"),
            "s3_server_sha256": env("WEFT_S3_SERVER_SHA256"),
            "s3_container_image": env("WEFT_S3_CONTAINER_IMAGE"),
        },
        "configurations": results,
        "checks": checks,
        "summary": {"checks_passed": passed, "checks_total": 6},
    });

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("create receipt dir");
    }
    fs::write(
        &out,
        serde_json::to_string_pretty(&report).expect("serialize receipt"),
    )
    .expect("write receipt");
    let _ = fs::remove_dir_all(&scratch);
    eprintln!("overlay_bench: wrote {}", out.display());
    println!(
        "overlay court [{backend_label}]: {passed}/6 checks; cow writes p50 {:.2} ms p99 {:.2} ms; overlay writes p50 {:.2} ms p99 {:.2} ms; cow bytes {:.1} MiB overlay bytes {:.1} MiB",
        cow["writes"]["latency"]["p50_ms"].as_f64().unwrap_or(0.0),
        cow["writes"]["latency"]["p99_ms"].as_f64().unwrap_or(0.0),
        overlay["writes"]["latency"]["p50_ms"].as_f64().unwrap_or(0.0),
        overlay["writes"]["latency"]["p99_ms"].as_f64().unwrap_or(0.0),
        cow["retained_blob_bytes_after_compaction"].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0),
        overlay["retained_blob_bytes_after_compaction"].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0),
    );
    if passed != 6 {
        eprintln!("overlay_bench: {passed}/6 checks passed; failing the court");
        std::process::exit(1);
    }
}
